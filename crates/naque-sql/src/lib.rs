//! Deterministic SQL statement classification for the permission gate.
//!
//! Parsing failures and unrecognized statements are classified as a
//! non-read-only `Unknown` (fail safe) so the gate treats them as writes.

use naque_core::{CatastrophicReason, ClassifyResult, StatementClass, StatementKind};
use sqlparser::ast::Statement;
use sqlparser::dialect::{Dialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

/// Which SQL dialect to parse with (selected by the connected engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlDialect {
    Postgres,
    Sqlite,
}

impl SqlDialect {
    fn parser_dialect(self) -> Box<dyn Dialect> {
        match self {
            SqlDialect::Postgres => Box::new(PostgreSqlDialect {}),
            SqlDialect::Sqlite => Box::new(SQLiteDialect {}),
        }
    }
}

/// Parse and classify every statement in `sql`.
///
/// On parse error the entire input is treated as a single non-read-only
/// `Unknown` statement.
pub fn classify(sql: &str, dialect: SqlDialect) -> ClassifyResult {
    let parsed = Parser::parse_sql(dialect.parser_dialect().as_ref(), sql);
    match parsed {
        Ok(statements) if !statements.is_empty() => ClassifyResult {
            statements: statements.iter().map(classify_statement).collect(),
        },
        // Parse error OR empty input -> fail safe.
        _ => ClassifyResult {
            statements: vec![StatementClass::unknown("unparseable (treated as write)")],
        },
    }
}

// Reads never mutate data, so they are never catastrophic; unlike `write()`/`ddl()`
// this helper takes no catastrophic param.
fn read(label: &str) -> StatementClass {
    StatementClass {
        kind: StatementKind::Read,
        is_read_only: true,
        catastrophic: None,
        label: label.to_string(),
    }
}

fn write(label: &str, catastrophic: Option<CatastrophicReason>) -> StatementClass {
    StatementClass {
        kind: StatementKind::Write,
        is_read_only: false,
        catastrophic,
        label: label.to_string(),
    }
}

fn ddl(label: &str, catastrophic: Option<CatastrophicReason>) -> StatementClass {
    StatementClass {
        kind: StatementKind::Ddl,
        is_read_only: false,
        catastrophic,
        label: label.to_string(),
    }
}

// `catastrophic` is always `None`: only Set/Transaction kinds use this helper;
// writes/DDL go through `write()`/`ddl()`, which carry a catastrophic reason.
fn classed(kind: StatementKind, is_read_only: bool, label: &str) -> StatementClass {
    StatementClass {
        kind,
        is_read_only,
        catastrophic: None,
        label: label.to_string(),
    }
}

fn classify_statement(stmt: &Statement) -> StatementClass {
    match stmt {
        Statement::Query(query) => classify_query(query),
        Statement::ShowVariable { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowTables { .. }
        | Statement::ShowFunctions { .. } => read("read-only"),
        Statement::Explain { analyze, statement, .. } => {
            if *analyze {
                classify_statement(statement) // EXPLAIN ANALYZE executes the inner stmt
            } else {
                read("read-only (explain)")
            }
        },
        Statement::Insert(_) => write("WRITE: INSERT", None),
        Statement::Update { selection, .. } => {
            if selection.is_none() {
                write("WRITE: UPDATE (no WHERE)", Some(CatastrophicReason::UpdateWithoutWhere))
            } else {
                write("WRITE: UPDATE", None)
            }
        },
        Statement::Delete(delete) => {
            if delete.selection.is_none() {
                write("WRITE: DELETE (no WHERE)", Some(CatastrophicReason::DeleteWithoutWhere))
            } else {
                write("WRITE: DELETE", None)
            }
        },
        // Other CREATE* variants (CreateFunction/CreateProcedure/CreateType/...) and uncommon
        // DDL fall through to the Unknown catch-all -- fail-safe (non-read-only, gated), just
        // labeled "Unknown" rather than "DDL".
        Statement::CreateTable(_)
        | Statement::CreateIndex(_)
        | Statement::CreateView { .. }
        | Statement::CreateSchema { .. } => ddl("DDL: CREATE", None),
        Statement::AlterTable { .. } => ddl("DDL: ALTER", None),
        Statement::Drop { .. } => ddl("DDL: DROP", Some(CatastrophicReason::DropObject)),
        Statement::Truncate { .. } => ddl("DDL: TRUNCATE", Some(CatastrophicReason::Truncate)),
        Statement::SetVariable { .. }
        | Statement::SetTimeZone { .. }
        | Statement::SetNames { .. }
        | Statement::SetNamesDefault {} => classed(StatementKind::Set, true, "session SET"),
        Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Savepoint { .. } => classed(StatementKind::Transaction, true, "transaction control"),
        Statement::Copy { to, .. } => {
            if *to {
                read("read-only (COPY TO)")
            } else {
                write("WRITE: COPY FROM", None)
            }
        },
        Statement::Merge { .. } => write("WRITE: MERGE", None),
        _ => StatementClass::unknown("unclassified (treated as write)"),
    }
}

/// A top-level `Query` (SELECT / WITH / VALUES). Read-only unless it carries a
/// row-locking clause (`FOR UPDATE` / `FOR SHARE`), which intends a later write,
/// or a data-modifying body/CTE (e.g. `WITH x AS (INSERT ... RETURNING ...)`),
/// which parses as `Statement::Query` but mutates data.
fn classify_query(query: &sqlparser::ast::Query) -> StatementClass {
    if !query.locks.is_empty() {
        return StatementClass {
            kind: StatementKind::Read,
            is_read_only: false,
            catastrophic: None,
            label: "locking read (FOR UPDATE/SHARE)".to_string(),
        };
    }
    if query_is_data_modifying(query) {
        return write("WRITE: data-modifying query/CTE", None);
    }
    read("read-only")
}

/// True if a `Query` mutates data via its body or any (possibly nested) CTE.
fn query_is_data_modifying(query: &sqlparser::ast::Query) -> bool {
    if let Some(with) = &query.with
        && with.cte_tables.iter().any(|cte| query_is_data_modifying(&cte.query))
    {
        return true;
    }
    set_expr_is_data_modifying(&query.body)
}

fn set_expr_is_data_modifying(body: &sqlparser::ast::SetExpr) -> bool {
    use sqlparser::ast::SetExpr;
    match body {
        SetExpr::Insert(_) | SetExpr::Update(_) => true,
        SetExpr::Query(q) => query_is_data_modifying(q),
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_is_data_modifying(left) || set_expr_is_data_modifying(right)
        },
        // `SELECT ... INTO <table>` creates a table (like CREATE TABLE AS) -- a write.
        SetExpr::Select(s) => s.into.is_some(),
        SetExpr::Values(_) | SetExpr::Table(_) => false,
        // FAIL SAFE: unknown/future SetExpr variants are treated as data-modifying.
        #[allow(unreachable_patterns)]
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use naque_core::{CatastrophicReason, StatementKind};

    use super::*;

    #[test]
    fn parse_error_is_fail_safe() {
        let r = classify("this is not sql ;;", SqlDialect::Postgres);
        assert_eq!(r.statements.len(), 1);
        assert_eq!(r.statements[0].kind, StatementKind::Unknown);
        assert!(!r.statements[0].is_read_only);
        assert!(!r.is_read_only());
    }

    #[test]
    fn empty_input_is_fail_safe() {
        let r = classify("   ", SqlDialect::Sqlite);
        assert!(!r.is_read_only());
        assert_eq!(r.statements[0].kind, StatementKind::Unknown);
    }

    fn one(sql: &str) -> StatementClass {
        let r = classify(sql, SqlDialect::Postgres);
        assert_eq!(r.statements.len(), 1, "expected exactly one statement: {sql}");
        r.statements.into_iter().next().unwrap()
    }

    #[test]
    fn select_is_read_only() {
        let c = one("SELECT id, name FROM users WHERE id = 1");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(c.is_read_only);
        assert!(c.catastrophic.is_none());
    }

    #[test]
    fn cte_select_is_read_only() {
        let c = one("WITH t AS (SELECT 1 AS n) SELECT n FROM t");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(c.is_read_only);
    }

    #[test]
    fn values_is_read_only() {
        let c = one("VALUES (1), (2), (3)");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(c.is_read_only);
    }

    #[test]
    fn show_is_read_only() {
        let c = one("SHOW search_path");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(c.is_read_only);
    }

    #[test]
    fn explain_without_analyze_is_read_only() {
        let c = one("EXPLAIN SELECT * FROM users");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(c.is_read_only);
    }

    #[test]
    fn select_for_update_is_not_read_only() {
        let c = one("SELECT id FROM users WHERE id = 1 FOR UPDATE");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(!c.is_read_only, "locking read must not be auto-allowed");
    }

    #[test]
    fn explain_analyze_recurses_into_inner_read() {
        let c = one("EXPLAIN ANALYZE SELECT * FROM users");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(c.is_read_only);
    }

    #[test]
    fn insert_is_write() {
        let c = one("INSERT INTO users (name) VALUES ('a')");
        assert_eq!(c.kind, StatementKind::Write);
        assert!(!c.is_read_only);
        assert!(c.catastrophic.is_none());
    }

    #[test]
    fn update_with_where_is_write_not_catastrophic() {
        let c = one("UPDATE users SET name = 'a' WHERE id = 1");
        assert_eq!(c.kind, StatementKind::Write);
        assert!(!c.is_read_only);
        assert!(c.catastrophic.is_none());
    }

    #[test]
    fn update_without_where_is_catastrophic() {
        let c = one("UPDATE users SET name = 'a'");
        assert_eq!(c.kind, StatementKind::Write);
        assert_eq!(c.catastrophic, Some(CatastrophicReason::UpdateWithoutWhere));
    }

    #[test]
    fn delete_with_where_is_write_not_catastrophic() {
        let c = one("DELETE FROM users WHERE id = 1");
        assert_eq!(c.kind, StatementKind::Write);
        assert!(!c.is_read_only);
        assert!(c.catastrophic.is_none());
    }

    #[test]
    fn delete_without_where_is_catastrophic() {
        let c = one("DELETE FROM users");
        assert_eq!(c.kind, StatementKind::Write);
        assert_eq!(c.catastrophic, Some(CatastrophicReason::DeleteWithoutWhere));
    }

    #[test]
    fn delete_using_without_where_is_catastrophic() {
        // `DELETE FROM t USING other` with no WHERE cross-joins and deletes ALL of t,
        // so a missing WHERE is catastrophic even when a USING clause is present.
        let c = one("DELETE FROM t USING other WHERE false");
        // sanity: the WHERE-present form is NOT catastrophic
        assert!(c.catastrophic.is_none());
        let c2 = one("DELETE FROM t USING other");
        assert!(!c2.is_read_only);
        assert_eq!(c2.catastrophic, Some(naque_core::CatastrophicReason::DeleteWithoutWhere));
    }

    #[test]
    fn create_table_is_ddl_not_catastrophic() {
        let c = one("CREATE TABLE t (id INT)");
        assert_eq!(c.kind, StatementKind::Ddl);
        assert!(!c.is_read_only);
        assert!(c.catastrophic.is_none());
    }

    #[test]
    fn alter_table_is_ddl_not_catastrophic() {
        let c = one("ALTER TABLE t ADD COLUMN n INT");
        assert_eq!(c.kind, StatementKind::Ddl);
        assert!(c.catastrophic.is_none());
    }

    #[test]
    fn drop_table_is_catastrophic() {
        let c = one("DROP TABLE t");
        assert_eq!(c.kind, StatementKind::Ddl);
        assert_eq!(c.catastrophic, Some(CatastrophicReason::DropObject));
    }

    #[test]
    fn truncate_is_catastrophic() {
        let c = one("TRUNCATE TABLE t");
        assert_eq!(c.kind, StatementKind::Ddl);
        assert_eq!(c.catastrophic, Some(CatastrophicReason::Truncate));
    }

    #[test]
    fn explain_analyze_inherits_write() {
        // EXPLAIN ANALYZE executes the statement, so it inherits write-ness.
        let c = one("EXPLAIN ANALYZE DELETE FROM users");
        assert!(!c.is_read_only);
        assert_eq!(c.catastrophic, Some(CatastrophicReason::DeleteWithoutWhere));
    }

    #[test]
    fn set_is_read_only_no_data_change() {
        let c = one("SET search_path TO public");
        assert_eq!(c.kind, StatementKind::Set);
        assert!(c.is_read_only);
    }

    #[test]
    fn transaction_control_is_read_only() {
        for sql in ["BEGIN", "COMMIT", "ROLLBACK"] {
            let c = one(sql);
            assert_eq!(c.kind, StatementKind::Transaction, "{sql}");
            assert!(c.is_read_only, "{sql}");
        }
    }

    #[test]
    fn copy_from_is_write() {
        let c = one("COPY users FROM '/tmp/u.csv'");
        assert_eq!(c.kind, StatementKind::Write);
        assert!(!c.is_read_only);
    }

    #[test]
    fn multi_statement_batch_aggregates() {
        // A read followed by a write is not a read-only batch.
        let r = classify("SELECT 1; DELETE FROM users;", SqlDialect::Postgres);
        assert_eq!(r.statements.len(), 2);
        assert!(!r.is_read_only());
        assert!(r.any_catastrophic());
        assert_eq!(r.first_catastrophic(), Some(CatastrophicReason::DeleteWithoutWhere));
    }

    #[test]
    fn all_reads_batch_is_read_only() {
        let r = classify("SELECT 1; SELECT 2;", SqlDialect::Sqlite);
        assert_eq!(r.statements.len(), 2);
        assert!(r.is_read_only());
        assert!(!r.any_catastrophic());
    }

    #[test]
    fn data_modifying_cte_is_not_read_only() {
        let c = one("WITH x AS (INSERT INTO t VALUES (1) RETURNING id) SELECT * FROM x");
        assert!(!c.is_read_only, "data-modifying CTE must not be auto-allowed");
    }

    #[test]
    fn copy_to_is_read_only() {
        let c = one("COPY users TO '/tmp/out.csv'");
        assert_eq!(c.kind, StatementKind::Read);
        assert!(c.is_read_only);
    }

    #[test]
    fn select_into_is_not_read_only() {
        let c = one("SELECT * INTO new_table FROM users");
        assert!(!c.is_read_only, "SELECT INTO creates a table and must not be auto-allowed");
    }

    #[test]
    fn merge_is_write() {
        let c = one("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET t.v = s.v");
        assert_eq!(c.kind, StatementKind::Write);
        assert!(!c.is_read_only);
    }

    #[test]
    fn unhandled_parseable_statement_is_fail_safe() {
        // GRANT parses to a Statement variant we don't classify -> fail-safe Unknown, not read-only.
        let c = one("GRANT SELECT ON t TO u");
        assert_eq!(c.kind, StatementKind::Unknown);
        assert!(!c.is_read_only);
    }
}
