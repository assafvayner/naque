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

/// Render a Postgres INTERVAL roughly as psql does, e.g.
/// `1 year 2 mons 3 days 04:05:06`. Months/days/microseconds are stored
/// independently, so the time part can exceed 24h and carries its own sign.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn format_pg_interval(iv: &sqlx::postgres::types::PgInterval) -> String {
    let mut parts: Vec<String> = Vec::new();
    let (years, mons) = (iv.months / 12, iv.months % 12);
    if years != 0 {
        parts.push(format!("{years} year{}", if years.abs() == 1 { "" } else { "s" }));
    }
    if mons != 0 {
        parts.push(format!("{mons} mon{}", if mons.abs() == 1 { "" } else { "s" }));
    }
    if iv.days != 0 {
        parts.push(format!("{} day{}", iv.days, if iv.days.abs() == 1 { "" } else { "s" }));
    }
    if iv.microseconds != 0 || parts.is_empty() {
        let neg = iv.microseconds < 0;
        let total = iv.microseconds.unsigned_abs();
        let (micros, secs) = (total % 1_000_000, total / 1_000_000);
        let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
        let sign = if neg { "-" } else { "" };
        if micros != 0 {
            parts.push(format!("{sign}{h:02}:{m:02}:{s:02}.{micros:06}"));
        } else {
            parts.push(format!("{sign}{h:02}:{m:02}:{s:02}"));
        }
    }
    parts.join(" ")
}

/// Render a Postgres MONEY value. Assumes the common locale `frac_digits = 2`
/// (whole cents); no currency symbol since the client does not know the locale.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn format_pg_money(m: sqlx::postgres::types::PgMoney) -> String {
    m.to_bigdecimal(2).to_string()
}

/// Render a Postgres HSTORE as `"k1"=>"v1", "k2"=>NULL`, keys in map order.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn format_pg_hstore(h: &sqlx::postgres::types::PgHstore) -> String {
    h.0.iter()
        .map(|(k, v)| match v {
            Some(val) => format!("\"{k}\"=>\"{val}\""),
            None => format!("\"{k}\"=>NULL"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render a Postgres BIT / VARBIT as a string of `0`/`1`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn format_bit_vec(b: &sqlx::types::BitVec) -> String {
    b.iter().map(|bit| if bit { '1' } else { '0' }).collect()
}

/// Render a decoded Postgres array as `{a,b,c}` (array-literal style), with
/// `NULL` for null elements.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn format_array<T: std::fmt::Display>(items: &[Option<T>]) -> String {
    let inner = items
        .iter()
        .map(|o| match o {
            Some(v) => v.to_string(),
            None => "NULL".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{inner}}}")
}

#[cfg(test)]
mod format_tests {
    use std::collections::BTreeMap;

    use sqlx::postgres::types::{PgHstore, PgInterval, PgMoney};
    use sqlx::types::BitVec;

    use super::{format_array, format_bit_vec, format_pg_hstore, format_pg_interval, format_pg_money};

    #[test]
    fn interval_full_components() {
        let iv = PgInterval {
            months: 14,
            days: 3,
            microseconds: 4 * 3_600_000_000 + 5 * 60_000_000 + 6_000_000,
        };
        assert_eq!(format_pg_interval(&iv), "1 year 2 mons 3 days 04:05:06");
    }

    #[test]
    fn interval_fractional_seconds() {
        let iv = PgInterval {
            months: 0,
            days: 0,
            microseconds: 1_500_000,
        };
        assert_eq!(format_pg_interval(&iv), "00:00:01.500000");
    }

    #[test]
    fn interval_negative_time() {
        let iv = PgInterval {
            months: 0,
            days: 0,
            microseconds: -1_000_000,
        };
        assert_eq!(format_pg_interval(&iv), "-00:00:01");
    }

    #[test]
    fn interval_zero() {
        let iv = PgInterval {
            months: 0,
            days: 0,
            microseconds: 0,
        };
        assert_eq!(format_pg_interval(&iv), "00:00:00");
    }

    #[test]
    fn interval_singular_units() {
        let iv = PgInterval {
            months: 13,
            days: 1,
            microseconds: 0,
        };
        assert_eq!(format_pg_interval(&iv), "1 year 1 mon 1 day");
    }

    #[test]
    #[allow(clippy::inconsistent_digit_grouping)] // 123_45 intentionally groups as dollars_cents
    fn money_values() {
        assert_eq!(format_pg_money(PgMoney(123_45)), "123.45");
        assert_eq!(format_pg_money(PgMoney(-99)), "-0.99");
    }

    #[test]
    fn hstore_keys_and_null() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), Some("1".to_string()));
        m.insert("b".to_string(), None);
        assert_eq!(format_pg_hstore(&PgHstore(m)), "\"a\"=>\"1\", \"b\"=>NULL");
    }

    #[test]
    fn hstore_empty() {
        assert_eq!(format_pg_hstore(&PgHstore(BTreeMap::new())), "");
    }

    #[test]
    fn bitvec_pattern() {
        let mut b = BitVec::from_elem(3, false);
        b.set(0, true);
        b.set(2, true);
        assert_eq!(format_bit_vec(&b), "101");
        assert_eq!(format_bit_vec(&BitVec::new()), "");
    }

    #[test]
    fn array_join_with_null() {
        assert_eq!(format_array(&[Some(1), None, Some(3)]), "{1,NULL,3}");
        assert_eq!(format_array::<i32>(&[]), "{}");
    }
}
