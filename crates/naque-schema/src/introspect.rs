//! Live-catalog introspection: query the database and build a [`SchemaModel`].

use std::collections::BTreeMap;

use naque_db::{Database, Engine};

use crate::error::SchemaError;
use crate::model::{ColumnInfo, ForeignKey, IndexInfo, SchemaModel, TableInfo};

// (id/constraint_key) -> (ref_table, Vec<(seq, from_col, to_col)>)
type FkGroupMap = BTreeMap<String, (String, Vec<(u64, String, String)>)>;
// (schema, table, constraint) -> (ref_table, Vec<(ordinal, fk_col, ref_col)>)
type PgFkGroupMap = BTreeMap<(String, String, String), (String, Vec<(u64, String, String)>)>;

/// Introspect the live database catalog into a [`SchemaModel`].
pub async fn introspect(db: &mut Database) -> Result<SchemaModel, SchemaError> {
    match db.engine() {
        Engine::Sqlite => introspect_sqlite(db).await,
        Engine::Postgres => introspect_postgres(db).await,
    }
}

/// Introspect and return the structural fingerprint (for drift detection).
pub async fn current_fingerprint(db: &mut Database) -> Result<String, SchemaError> {
    let model = introspect(db).await?;
    Ok(model.fingerprint())
}

// ---------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------

async fn introspect_sqlite(db: &mut Database) -> Result<SchemaModel, SchemaError> {
    let tables_result = db
        .fetch_readonly(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .await?;

    let table_names: Vec<String> = tables_result
        .rows
        .iter()
        .filter_map(|row| row.first().and_then(|v| v.clone()))
        .collect();

    let mut tables = Vec::with_capacity(table_names.len());

    for table_name in &table_names {
        let columns = sqlite_columns(db, table_name).await?;
        let foreign_keys = sqlite_foreign_keys(db, table_name).await?;
        let indexes = sqlite_indexes(db, table_name).await?;

        tables.push(TableInfo {
            schema: None,
            name: table_name.clone(),
            columns,
            foreign_keys,
            indexes,
            description: None,
        });
    }

    Ok(SchemaModel {
        engine: "sqlite".to_owned(),
        tables,
        docs: vec![],
    })
}

async fn sqlite_columns(db: &mut Database, table: &str) -> Result<Vec<ColumnInfo>, SchemaError> {
    // PRAGMA table_info columns: cid, name, type, notnull, dflt_value, pk
    let result = db
        .fetch_readonly(&format!("PRAGMA table_info(\"{table}\")"))
        .await?;

    let mut cols = Vec::new();
    for row in &result.rows {
        let name = cell(row, 1).unwrap_or_default();
        let data_type = cell(row, 2).unwrap_or_default();
        let notnull = cell(row, 3).unwrap_or_default();
        let dflt_value = row.get(4).and_then(|v| v.clone());
        let pk = cell(row, 5).unwrap_or_default();

        if name.is_empty() {
            continue;
        }

        cols.push(ColumnInfo {
            name,
            data_type,
            nullable: notnull != "1",
            default: dflt_value,
            primary_key: pk != "0",
        });
    }
    Ok(cols)
}

async fn sqlite_foreign_keys(
    db: &mut Database,
    table: &str,
) -> Result<Vec<ForeignKey>, SchemaError> {
    // PRAGMA foreign_key_list columns: id, seq, table, from, to, on_update, on_delete, match
    let result = db
        .fetch_readonly(&format!("PRAGMA foreign_key_list(\"{table}\")"))
        .await?;

    // Group by id (each FK constraint may span multiple columns via seq).
    let mut groups: FkGroupMap = BTreeMap::new();

    for row in &result.rows {
        let id = cell(row, 0).unwrap_or_default();
        let seq_str = cell(row, 1).unwrap_or_default();
        let ref_table = cell(row, 2).unwrap_or_default();
        let from_col = cell(row, 3).unwrap_or_default();
        let to_col = cell(row, 4).unwrap_or_default();

        let seq: u64 = seq_str.parse().unwrap_or(0);

        let entry = groups
            .entry(id)
            .or_insert_with(|| (ref_table.clone(), Vec::new()));
        entry.1.push((seq, from_col, to_col));
    }

    let mut fks = Vec::new();
    for (_id, (ref_table, mut cols)) in groups {
        cols.sort_by_key(|(seq, _, _)| *seq);
        let from_cols: Vec<String> = cols.iter().map(|(_, f, _)| f.clone()).collect();
        let to_cols: Vec<String> = cols.iter().map(|(_, _, t)| t.clone()).collect();
        fks.push(ForeignKey {
            columns: from_cols,
            ref_table,
            ref_columns: to_cols,
        });
    }
    Ok(fks)
}

async fn sqlite_indexes(db: &mut Database, table: &str) -> Result<Vec<IndexInfo>, SchemaError> {
    // PRAGMA index_list columns: seq, name, unique, origin, partial
    let list_result = db
        .fetch_readonly(&format!("PRAGMA index_list(\"{table}\")"))
        .await?;

    let mut indexes = Vec::new();
    for row in &list_result.rows {
        let idx_name = cell(row, 1).unwrap_or_default();
        let unique_str = cell(row, 2).unwrap_or_default();

        if idx_name.is_empty() {
            continue;
        }

        let unique = unique_str == "1";

        // PRAGMA index_info columns: seqno, cid, name
        let info_result = db
            .fetch_readonly(&format!("PRAGMA index_info(\"{idx_name}\")"))
            .await?;

        let mut col_entries: Vec<(i64, String)> = Vec::new();
        for irow in &info_result.rows {
            let seqno: i64 = cell(irow, 0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let col_name = cell(irow, 2).unwrap_or_default();
            col_entries.push((seqno, col_name));
        }
        col_entries.sort_by_key(|(seq, _)| *seq);
        let columns: Vec<String> = col_entries
            .into_iter()
            .map(|(_, c)| c)
            .filter(|c| !c.is_empty())
            .collect();

        indexes.push(IndexInfo {
            name: idx_name,
            columns,
            unique,
        });
    }
    Ok(indexes)
}

// ---------------------------------------------------------------------------
// Postgres
// ---------------------------------------------------------------------------

async fn introspect_postgres(db: &mut Database) -> Result<SchemaModel, SchemaError> {
    // Fetch all columns for all user tables, ordered for deterministic assembly.
    let cols_result = db
        .fetch_readonly(
            "SELECT
                c.table_schema,
                c.table_name,
                c.column_name,
                c.data_type,
                c.is_nullable,
                c.column_default,
                c.ordinal_position
             FROM information_schema.columns c
             JOIN information_schema.tables t
               ON t.table_schema = c.table_schema
              AND t.table_name   = c.table_name
             WHERE t.table_type  = 'BASE TABLE'
               AND t.table_schema NOT IN ('pg_catalog', 'information_schema')
             ORDER BY c.table_schema, c.table_name, c.ordinal_position",
        )
        .await?;

    // Build a map: (schema, table) -> Vec<(ordinal, ColumnInfo)>
    let mut table_cols: BTreeMap<(String, String), Vec<(u64, ColumnInfo)>> = BTreeMap::new();

    for row in &cols_result.rows {
        let schema = cell(row, 0).unwrap_or_default();
        let tname = cell(row, 1).unwrap_or_default();
        let col_name = cell(row, 2).unwrap_or_default();
        let data_type = cell(row, 3).unwrap_or_default();
        let is_nullable = cell(row, 4).unwrap_or_default();
        let col_default = row.get(5).and_then(|v| v.clone());
        let ordinal: u64 = cell(row, 6).and_then(|s| s.parse().ok()).unwrap_or(0);

        if col_name.is_empty() {
            continue;
        }

        let col = ColumnInfo {
            name: col_name,
            data_type,
            nullable: is_nullable.eq_ignore_ascii_case("YES"),
            default: col_default,
            primary_key: false, // filled in below
        };

        table_cols
            .entry((schema, tname))
            .or_default()
            .push((ordinal, col));
    }

    // Fetch primary key columns.
    let pk_result = db
        .fetch_readonly(
            "SELECT
                kcu.table_schema,
                kcu.table_name,
                kcu.column_name
             FROM information_schema.table_constraints tc
             JOIN information_schema.key_column_usage kcu
               ON kcu.constraint_name = tc.constraint_name
              AND kcu.table_schema     = tc.table_schema
              AND kcu.table_name       = tc.table_name
             WHERE tc.constraint_type = 'PRIMARY KEY'
               AND tc.table_schema NOT IN ('pg_catalog', 'information_schema')
             ORDER BY kcu.table_schema, kcu.table_name, kcu.ordinal_position",
        )
        .await?;

    // Set of (schema, table, col) that are PKs.
    let mut pk_set: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    for row in &pk_result.rows {
        let schema = cell(row, 0).unwrap_or_default();
        let tname = cell(row, 1).unwrap_or_default();
        let col = cell(row, 2).unwrap_or_default();
        if !col.is_empty() {
            pk_set.insert((schema, tname, col));
        }
    }

    // Apply PK flags.
    for ((schema, tname), cols) in &mut table_cols {
        for (_, col) in cols.iter_mut() {
            if pk_set.contains(&(schema.clone(), tname.clone(), col.name.clone())) {
                col.primary_key = true;
            }
        }
    }

    // Fetch foreign keys. Pair each source column with its referenced column
    // by position via referential_constraints + position_in_unique_constraint.
    // This avoids the N^2 cross-product that key_column_usage ×
    // constraint_column_usage produces for composite FKs.
    let fk_result = db
        .fetch_readonly(
            "SELECT kcu.table_schema, kcu.table_name, kcu.constraint_name,
                    kcu.column_name AS fk_column, kcu.ordinal_position,
                    ccu.table_name AS ref_table, ccu.column_name AS ref_column
             FROM information_schema.table_constraints tc
             JOIN information_schema.key_column_usage kcu
               ON kcu.constraint_name = tc.constraint_name
              AND kcu.constraint_schema = tc.constraint_schema
             JOIN information_schema.referential_constraints rc
               ON rc.constraint_name = tc.constraint_name
              AND rc.constraint_schema = tc.constraint_schema
             JOIN information_schema.key_column_usage ccu
               ON ccu.constraint_name = rc.unique_constraint_name
              AND ccu.constraint_schema = rc.unique_constraint_schema
              AND ccu.ordinal_position = kcu.position_in_unique_constraint
             WHERE tc.constraint_type = 'FOREIGN KEY'
               AND tc.table_schema NOT IN ('pg_catalog', 'information_schema')
             ORDER BY kcu.table_schema, kcu.table_name, kcu.constraint_name, kcu.ordinal_position",
        )
        .await?;

    // Group FK rows by (schema, table, constraint_name). Each row pairs one
    // source column with the referenced column at the same position.
    let mut fk_groups: PgFkGroupMap = BTreeMap::new();

    for row in &fk_result.rows {
        let schema = cell(row, 0).unwrap_or_default();
        let tname = cell(row, 1).unwrap_or_default();
        let constraint = cell(row, 2).unwrap_or_default();
        let col_name = cell(row, 3).unwrap_or_default();
        let ordinal: u64 = cell(row, 4).and_then(|s| s.parse().ok()).unwrap_or(0);
        let ref_table = cell(row, 5).unwrap_or_default();
        let ref_col = cell(row, 6).unwrap_or_default();

        let entry = fk_groups
            .entry((schema, tname, constraint))
            .or_insert_with(|| (ref_table.clone(), Vec::new()));
        entry.1.push((ordinal, col_name, ref_col));
    }

    // Build (schema, table) -> Vec<ForeignKey>.
    let mut table_fks: BTreeMap<(String, String), Vec<ForeignKey>> = BTreeMap::new();
    for ((schema, tname, _constraint), (ref_table, mut entries)) in fk_groups {
        entries.sort_by_key(|(ord, _, _)| *ord);
        let columns: Vec<String> = entries.iter().map(|(_, c, _)| c.clone()).collect();
        let ref_columns: Vec<String> = entries.iter().map(|(_, _, r)| r.clone()).collect();
        table_fks
            .entry((schema, tname))
            .or_default()
            .push(ForeignKey {
                columns,
                ref_table,
                ref_columns,
            });
    }

    // Fetch indexes from pg_indexes.
    let idx_result = db
        .fetch_readonly(
            "SELECT schemaname, tablename, indexname, indexdef
             FROM pg_indexes
             WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
             ORDER BY schemaname, tablename, indexname",
        )
        .await?;

    let mut table_indexes: BTreeMap<(String, String), Vec<IndexInfo>> = BTreeMap::new();
    for row in &idx_result.rows {
        let schema = cell(row, 0).unwrap_or_default();
        let tname = cell(row, 1).unwrap_or_default();
        let idx_name = cell(row, 2).unwrap_or_default();
        let idx_def = cell(row, 3).unwrap_or_default();

        if idx_name.is_empty() {
            continue;
        }

        let unique = idx_def.contains("CREATE UNIQUE INDEX");
        let columns = parse_index_columns(&idx_def);

        table_indexes
            .entry((schema, tname))
            .or_default()
            .push(IndexInfo {
                name: idx_name,
                columns,
                unique,
            });
    }

    // Assemble TableInfo list.
    let mut tables = Vec::new();
    for ((schema, tname), mut col_entries) in table_cols {
        col_entries.sort_by_key(|(ord, _)| *ord);
        let columns: Vec<ColumnInfo> = col_entries.into_iter().map(|(_, c)| c).collect();

        let key = (schema.clone(), tname.clone());
        let foreign_keys = table_fks.remove(&key).unwrap_or_default();
        let indexes = table_indexes.remove(&key).unwrap_or_default();

        tables.push(TableInfo {
            schema: Some(schema),
            name: tname,
            columns,
            foreign_keys,
            indexes,
            description: None,
        });
    }

    Ok(SchemaModel {
        engine: "postgres".to_owned(),
        tables,
        docs: vec![],
    })
}

/// Best-effort parse of column names from a Postgres index definition.
/// e.g. `CREATE INDEX foo ON public.bar (col1, col2)` → `["col1", "col2"]`.
///
/// This is best-effort only: it does not handle quoted identifiers or
/// expression indexes (e.g. `(lower(name))`). Index column names are
/// informational and do not drive query generation, so imperfect parsing here
/// is acceptable.
fn parse_index_columns(indexdef: &str) -> Vec<String> {
    // Find the last '(' ... ')' in the def.
    if let Some(open) = indexdef.rfind('(') {
        if let Some(close) = indexdef.rfind(')') {
            if close > open {
                let inner = &indexdef[open + 1..close];
                return inner
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
    vec![]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get the cell at `idx` as an owned `String`, or `None` if out of bounds or NULL.
#[inline]
fn cell(row: &[Option<String>], idx: usize) -> Option<String> {
    row.get(idx).and_then(|v| v.clone())
}
