//! Internal single-connection wrapper with dynamic cell stringification.

use sqlx::postgres::PgConnection;
use sqlx::sqlite::SqliteConnection;
use sqlx::{Column as _, Executor, Row, TypeInfo, ValueRef};

use crate::error::DbError;
use crate::result::{Column, QueryResult};

/// Internal connection that holds either a live Postgres or SQLite connection.
pub(crate) enum Conn {
    Pg(PgConnection),
    Sqlite(SqliteConnection),
}

impl Conn {
    /// Run a row-returning query and collect results with dynamic stringification.
    pub(crate) async fn fetch(&mut self, sql: &str) -> Result<QueryResult, DbError> {
        match self {
            Conn::Pg(c) => fetch_pg(c, sql).await,
            Conn::Sqlite(c) => fetch_sqlite(c, sql).await,
        }
    }

    /// Execute a statement and return the number of rows affected.
    pub(crate) async fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        match self {
            Conn::Pg(c) => {
                let result = c.execute(sql).await.map_err(|e| DbError::Query(e.to_string()))?;
                Ok(result.rows_affected())
            },
            Conn::Sqlite(c) => {
                let result = c.execute(sql).await.map_err(|e| DbError::Query(e.to_string()))?;
                Ok(result.rows_affected())
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Postgres row decoding
// ---------------------------------------------------------------------------

async fn fetch_pg(conn: &mut PgConnection, sql: &str) -> Result<QueryResult, DbError> {
    use sqlx::Row as _;
    use sqlx::postgres::PgRow;

    let rows: Vec<PgRow> = sqlx::query(sql)
        .fetch_all(conn)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;

    if rows.is_empty() {
        return Ok(QueryResult::default());
    }

    let columns: Vec<Column> = rows[0]
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.type_info().name().to_string(),
        })
        .collect();

    let mut result_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let cells = columns
            .iter()
            .enumerate()
            .map(|(i, col)| decode_pg_cell(row, i, &col.type_name))
            .collect();
        result_rows.push(cells);
    }

    Ok(QueryResult {
        columns,
        rows: result_rows,
        rows_affected: None,
    })
}

fn decode_pg_cell(row: &sqlx::postgres::PgRow, i: usize, type_name: &str) -> Option<String> {
    use sqlx::Row as _;

    // Check for NULL via raw value.
    let raw = match row.try_get_raw(i) {
        Ok(r) => r,
        Err(_) => return Some("<unrenderable:RAW_ERROR>".to_string()),
    };
    if raw.is_null() {
        return None;
    }

    let tn = type_name.to_ascii_lowercase();

    // Attempt typed decodes in priority order based on the column's declared type.
    // All branches fall through to the string fallback on mismatch.

    if matches!(
        tn.as_str(),
        "int8" | "bigint" | "int" | "integer" | "int4" | "int2" | "smallint" | "serial" | "bigserial"
    ) {
        if let Ok(v) = row.try_get::<i64, _>(i) {
            return Some(v.to_string());
        }
        if let Ok(v) = row.try_get::<i32, _>(i) {
            return Some(v.to_string());
        }
        if let Ok(v) = row.try_get::<i16, _>(i) {
            return Some(v.to_string());
        }
    }

    if matches!(tn.as_str(), "float8" | "double precision" | "float4" | "real" | "numeric" | "decimal") {
        if let Ok(v) = row.try_get::<f64, _>(i) {
            return Some(v.to_string());
        }
        if let Ok(v) = row.try_get::<f32, _>(i) {
            return Some(v.to_string());
        }
    }

    if (tn == "bool" || tn == "boolean")
        && let Ok(v) = row.try_get::<bool, _>(i)
    {
        return Some(v.to_string());
    }

    if tn == "uuid"
        && let Ok(v) = row.try_get::<sqlx::types::Uuid, _>(i)
    {
        return Some(v.to_string());
    }

    if (tn == "json" || tn == "jsonb")
        && let Ok(v) = row.try_get::<sqlx::types::JsonValue, _>(i)
    {
        return Some(v.to_string());
    }

    if (tn == "numeric" || tn == "decimal")
        && let Ok(v) = row.try_get::<sqlx::types::BigDecimal, _>(i)
    {
        return Some(v.to_string());
    }

    if tn.contains("timestamp") {
        if let Ok(v) = row.try_get::<sqlx::types::chrono::NaiveDateTime, _>(i) {
            return Some(v.to_string());
        }
        if let Ok(v) = row.try_get::<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>, _>(i) {
            return Some(v.to_string());
        }
    }
    if tn == "date"
        && let Ok(v) = row.try_get::<sqlx::types::chrono::NaiveDate, _>(i)
    {
        return Some(v.to_string());
    }
    if (tn == "time" || tn == "timetz")
        && let Ok(v) = row.try_get::<sqlx::types::chrono::NaiveTime, _>(i)
    {
        return Some(v.to_string());
    }

    // Bytea → hex string
    if tn == "bytea"
        && let Ok(v) = row.try_get::<Vec<u8>, _>(i)
    {
        return Some(bytes_to_hex(&v));
    }

    // Generic string (text, varchar, char, citext, etc.)
    if let Ok(v) = row.try_get::<String, _>(i) {
        return Some(v);
    }

    // Integer fallback (handles any int not caught above)
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return Some(v.to_string());
    }
    if let Ok(v) = row.try_get::<i32, _>(i) {
        return Some(v.to_string());
    }

    // Float fallback
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return Some(v.to_string());
    }

    // Non-panicking placeholder
    Some(format!("<unrenderable:{}>", type_name))
}

// ---------------------------------------------------------------------------
// SQLite row decoding
// ---------------------------------------------------------------------------

async fn fetch_sqlite(conn: &mut SqliteConnection, sql: &str) -> Result<QueryResult, DbError> {
    use sqlx::sqlite::SqliteRow;

    let rows: Vec<SqliteRow> = sqlx::query(sql)
        .fetch_all(conn)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;

    if rows.is_empty() {
        // Attempt to distinguish "no rows" from "non-row-returning statement".
        // For SELECT with no rows we return an empty result with columns if we
        // can. In practice, when rows is empty we have no column info; return
        // the typed default.
        return Ok(QueryResult::default());
    }

    let columns: Vec<Column> = rows[0]
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.type_info().name().to_string(),
        })
        .collect();

    let mut result_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut cells = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            cells.push(decode_sqlite_cell(row, i));
        }
        result_rows.push(cells);
    }

    Ok(QueryResult {
        columns,
        rows: result_rows,
        rows_affected: None,
    })
}

/// SQLite is dynamically typed; try the most common decode orders.
fn decode_sqlite_cell(row: &sqlx::sqlite::SqliteRow, i: usize) -> Option<String> {
    use sqlx::Row as _;

    let raw = match row.try_get_raw(i) {
        Ok(r) => r,
        Err(_) => return Some("<unrenderable:RAW_ERROR>".to_string()),
    };
    if raw.is_null() {
        return None;
    }

    // SQLite: try String first (covers TEXT and numbers stored as text)
    if let Ok(v) = row.try_get::<String, _>(i) {
        return Some(v);
    }

    if let Ok(v) = row.try_get::<i64, _>(i) {
        return Some(v.to_string());
    }

    if let Ok(v) = row.try_get::<f64, _>(i) {
        return Some(v.to_string());
    }

    // Blob → hex
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return Some(bytes_to_hex(&v));
    }

    Some("<unrenderable:SQLITE_VALUE>".to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("\\x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
