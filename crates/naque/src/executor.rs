//! The agent's `ToolExecutor` implementation.
//!
//! Routes the four standard tool calls (`inspect_table`, `sample_table`,
//! `explain`, `run_query`) to the database, going through the permission gate
//! for anything that modifies state.

use std::sync::Arc;

use naque_core::gate::QueryKind;
use naque_db::{Database, QueryResult};
use naque_llm::{LlmError, ToolCall, ToolExecutor};
use naque_schema::SchemaModel;
use tokio::sync::Mutex;

use crate::approval::Approver;
use crate::run_gated;

/// Upper bound on rows returned by `sample_table`, matching the tool schema
/// advertised to the LLM.
const SAMPLE_TABLE_LIMIT_CAP: u32 = 50;
const SAMPLE_TABLE_LIMIT_DEFAULT: u32 = 10;

pub struct QueryToolExecutor<'a> {
    pub db: Arc<Mutex<Database>>,
    pub mode: naque_core::PermissionMode,
    pub catastrophic_guard: bool,
    pub schema: Option<SchemaModel>,
    pub approver: &'a mut dyn Approver,
    pub last_result: Option<QueryResult>,
    /// Indices of `last_result` columns the agent tagged as byte counts.
    pub last_byte_columns: Vec<usize>,
}

#[async_trait::async_trait]
impl ToolExecutor for QueryToolExecutor<'_> {
    async fn execute(&mut self, call: &ToolCall) -> Result<String, LlmError> {
        match call.name.as_str() {
            "inspect_table" => self.inspect_table(call).await,
            "sample_table" => self.sample_table(call).await,
            "explain" => self.explain(call).await,
            "run_query" => self.run_query(call).await,
            other => Ok(format!("unknown tool: {other}")),
        }
    }
}

impl QueryToolExecutor<'_> {
    async fn inspect_table(&mut self, call: &ToolCall) -> Result<String, LlmError> {
        let name = call
            .input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LlmError::Tool("inspect_table: missing 'name'".to_string()))?;

        // Basic validation: reject names that look dangerous before interpolation.
        if !is_safe_identifier(name) {
            return Ok(format!("error: invalid table name {name:?}"));
        }

        if let Some(schema) = &self.schema
            && let Some(description) = schema.describe_table(name)
        {
            return Ok(description);
        }

        let mut db = self.db.lock().await;

        // Fall back to a live introspection query.
        let sql = match db.engine() {
            naque_db::Engine::Sqlite => {
                format!("PRAGMA table_info('{name}')")
            },
            naque_db::Engine::Postgres => {
                format!(
                    "SELECT column_name, data_type, is_nullable \
                     FROM information_schema.columns \
                     WHERE table_name = '{name}' \
                     ORDER BY ordinal_position"
                )
            },
        };

        match db.fetch_readonly(&sql).await {
            Ok(result) => Ok(format_result_text(&result)),
            Err(e) => Ok(format!("error: {e}")),
        }
    }

    async fn sample_table(&mut self, call: &ToolCall) -> Result<String, LlmError> {
        let name = call
            .input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LlmError::Tool("sample_table: missing 'name'".to_string()))?;

        // Basic validation: reject names that look dangerous.
        if !is_safe_identifier(name) {
            return Ok(format!("error: invalid table name {name:?}"));
        }

        let requested = call
            .input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::from(SAMPLE_TABLE_LIMIT_DEFAULT));
        let limit = if requested == 0 {
            u64::from(SAMPLE_TABLE_LIMIT_DEFAULT)
        } else {
            requested.min(u64::from(SAMPLE_TABLE_LIMIT_CAP))
        };

        let sql = format!("SELECT * FROM {name} LIMIT {limit}");

        let mut db = self.db.lock().await;
        match db.fetch_readonly(&sql).await {
            Ok(result) => {
                let text = format_result_text(&result);
                self.last_byte_columns = Vec::new();
                self.last_result = Some(result);
                Ok(text)
            },
            Err(e) => Ok(format!("error: {e}")),
        }
    }

    async fn explain(&mut self, call: &ToolCall) -> Result<String, LlmError> {
        let sql = call
            .input
            .get("sql")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LlmError::Tool("explain: missing 'sql'".to_string()))?;

        let explain_sql = format!("EXPLAIN {sql}");

        let mut db = self.db.lock().await;
        match db.fetch_readonly(&explain_sql).await {
            Ok(result) => Ok(format_result_text(&result)),
            Err(e) => Ok(format!("error: {e}")),
        }
    }

    async fn run_query(&mut self, call: &ToolCall) -> Result<String, LlmError> {
        let sql = call
            .input
            .get("sql")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LlmError::Tool("run_query: missing 'sql'".to_string()))?;

        let byte_column_names: Vec<String> = call
            .input
            .get("byte_count_columns")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        let mut db = self.db.lock().await;
        match run_gated(&mut db, self.mode, self.catastrophic_guard, sql, QueryKind::Primary, self.approver).await {
            Ok(result) => {
                let text = format_result_text(&result);
                self.last_byte_columns = resolve_byte_columns(&result, &byte_column_names);
                self.last_result = Some(result);
                Ok(format!("auto_executed\n{text}"))
            },
            // `run_gated` returns the exact string `"rejected"` in its Err arm only when
            // the user declined at the approval prompt; every other Err carries a DB
            // error message. Splitting on that literal keeps the LLM out of the security
            // path while still letting the agent distinguish "user rejected" from
            // "query failed" in its own follow-up reasoning.
            Err(reason) if reason == "rejected" => {
                Ok("rejected\nreason: user rejected the statement at the approval prompt".to_string())
            },
            Err(message) => Ok(format!("error\nmessage: {message}")),
        }
    }
}

/// Returns `true` if `name` is safe to interpolate into a SQL identifier
/// position. Allows letters, digits, underscores, dots, and double-quotes.
fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '"'))
}

/// Map agent-supplied byte-column names to their indices in `result.columns`.
/// Names not present in the result are dropped.
fn resolve_byte_columns(result: &QueryResult, names: &[String]) -> Vec<usize> {
    names
        .iter()
        .filter_map(|name| result.columns.iter().position(|c| &c.name == name))
        .collect()
}

/// Render a `QueryResult` to a compact text table for the agent to read.
pub fn format_result_text(result: &QueryResult) -> String {
    if let Some(n) = result.rows_affected
        && result.rows.is_empty()
    {
        return format!("{n} row(s) affected");
    }

    if result.columns.is_empty() && result.rows.is_empty() {
        return "(no rows)".to_string();
    }

    let col_names: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
    let mut out = col_names.join(" | ");
    out.push('\n');
    out.push_str(&"-".repeat(out.len().saturating_sub(1)));
    out.push('\n');

    for row in &result.rows {
        let cells: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap_or("NULL")).collect();
        out.push_str(&cells.join(" | "));
        out.push('\n');
    }

    if result.rows.is_empty() {
        out.push_str("(0 rows)\n");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::AutoApprove;

    #[test]
    fn safe_identifier_accepts_plain_names() {
        assert!(is_safe_identifier("users"));
        assert!(is_safe_identifier("public.users"));
        assert!(is_safe_identifier("user_accounts"));
        assert!(is_safe_identifier("\"MixedCase\""));
    }

    #[test]
    fn safe_identifier_rejects_injection() {
        assert!(!is_safe_identifier(""));
        assert!(!is_safe_identifier("users; DROP TABLE x"));
        assert!(!is_safe_identifier("users' OR '1'='1"));
        assert!(!is_safe_identifier("users WHERE 1=1"));
        assert!(!is_safe_identifier("a b"));
    }

    #[test]
    fn resolve_byte_columns_maps_names_to_indices() {
        let result = QueryResult {
            columns: vec![
                naque_db::Column {
                    name: "name".into(),
                    type_name: "text".into(),
                },
                naque_db::Column {
                    name: "sz".into(),
                    type_name: "bigint".into(),
                },
            ],
            rows: vec![],
            rows_affected: None,
        };
        assert_eq!(resolve_byte_columns(&result, &["sz".to_string()]), vec![1]);
        assert_eq!(resolve_byte_columns(&result, &["missing".to_string()]), Vec::<usize>::new());
    }

    #[tokio::test]
    async fn run_query_records_byte_count_columns() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut db = Database::connect(&url).await.unwrap();
        db.execute("CREATE TABLE t (name TEXT, sz INTEGER)").await.unwrap();
        db.execute("INSERT INTO t VALUES ('a', 4500000000)").await.unwrap();

        let mut approver = AutoApprove;
        let mut exec = QueryToolExecutor {
            db: Arc::new(Mutex::new(db)),
            mode: naque_core::PermissionMode::Wildcard,
            catastrophic_guard: true,
            schema: None,
            approver: &mut approver,
            last_result: None,
            last_byte_columns: Vec::new(),
        };

        let call = ToolCall {
            id: "tc".into(),
            name: "run_query".into(),
            input: serde_json::json!({ "sql": "SELECT name, sz FROM t", "byte_count_columns": ["sz"] }),
        };
        exec.execute(&call).await.unwrap();
        assert_eq!(exec.last_byte_columns, vec![1]);
    }

    #[tokio::test]
    async fn run_query_clears_byte_columns_when_absent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut db = Database::connect(&url).await.unwrap();
        db.execute("CREATE TABLE t (name TEXT, sz INTEGER)").await.unwrap();
        db.execute("INSERT INTO t VALUES ('a', 4500000000)").await.unwrap();

        let mut approver = AutoApprove;
        let mut exec = QueryToolExecutor {
            db: Arc::new(Mutex::new(db)),
            mode: naque_core::PermissionMode::Wildcard,
            catastrophic_guard: true,
            schema: None,
            approver: &mut approver,
            last_result: None,
            last_byte_columns: vec![1],
        };

        let call = ToolCall {
            id: "tc".into(),
            name: "run_query".into(),
            input: serde_json::json!({ "sql": "SELECT name, sz FROM t" }),
        };
        exec.execute(&call).await.unwrap();
        assert!(exec.last_byte_columns.is_empty());
    }

    #[tokio::test]
    async fn sample_table_clears_byte_columns() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut db = Database::connect(&url).await.unwrap();
        db.execute("CREATE TABLE t (name TEXT, sz INTEGER)").await.unwrap();
        db.execute("INSERT INTO t VALUES ('a', 4500000000)").await.unwrap();

        let mut approver = AutoApprove;
        let mut exec = QueryToolExecutor {
            db: Arc::new(Mutex::new(db)),
            mode: naque_core::PermissionMode::Wildcard,
            catastrophic_guard: true,
            schema: None,
            approver: &mut approver,
            last_result: None,
            last_byte_columns: Vec::new(),
        };

        // run_query tags a byte column...
        let q = ToolCall {
            id: "tc1".into(),
            name: "run_query".into(),
            input: serde_json::json!({ "sql": "SELECT name, sz FROM t", "byte_count_columns": ["sz"] }),
        };
        exec.execute(&q).await.unwrap();
        assert_eq!(exec.last_byte_columns, vec![1]);

        // ...then sample_table on the same/other table must clear the stale tag.
        let s = ToolCall {
            id: "tc2".into(),
            name: "sample_table".into(),
            input: serde_json::json!({ "name": "t" }),
        };
        exec.execute(&s).await.unwrap();
        assert!(exec.last_byte_columns.is_empty(), "sample_table results carry no LLM byte-column determination");
    }

    #[tokio::test]
    async fn sample_table_clamps_limit_to_cap() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut db = Database::connect(&url).await.unwrap();
        db.execute("CREATE TABLE t (id INTEGER)").await.unwrap();
        let row_count = SAMPLE_TABLE_LIMIT_CAP + 25;
        for id in 0..row_count {
            db.execute(&format!("INSERT INTO t VALUES ({id})")).await.unwrap();
        }

        let mut approver = AutoApprove;
        let mut exec = QueryToolExecutor {
            db: Arc::new(Mutex::new(db)),
            mode: naque_core::PermissionMode::Wildcard,
            catastrophic_guard: true,
            schema: None,
            approver: &mut approver,
            last_result: None,
            last_byte_columns: Vec::new(),
        };

        let call = ToolCall {
            id: "tc".into(),
            name: "sample_table".into(),
            input: serde_json::json!({ "name": "t", "limit": 999 }),
        };
        exec.execute(&call).await.unwrap();
        let rows = exec.last_result.as_ref().expect("sample_table stores last_result").rows.len();
        assert!(
            rows <= SAMPLE_TABLE_LIMIT_CAP as usize,
            "sample_table returned {rows} rows, expected at most {SAMPLE_TABLE_LIMIT_CAP}"
        );
        assert_eq!(rows, SAMPLE_TABLE_LIMIT_CAP as usize, "cap should be exactly hit");
    }

    #[tokio::test]
    async fn run_query_returns_labelled_envelope() {
        use crate::approval::AutoReject;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut db = Database::connect(&url).await.unwrap();
        db.execute("CREATE TABLE t (id INTEGER)").await.unwrap();
        db.execute("INSERT INTO t VALUES (1)").await.unwrap();

        // auto_executed: a SELECT under Wildcard auto-approves and prefixes the table.
        {
            let mut approver = AutoApprove;
            let mut exec = QueryToolExecutor {
                db: Arc::new(Mutex::new(Database::connect(&url).await.unwrap())),
                mode: naque_core::PermissionMode::Wildcard,
                catastrophic_guard: true,
                schema: None,
                approver: &mut approver,
                last_result: None,
                last_byte_columns: Vec::new(),
            };
            let call = ToolCall {
                id: "ok".into(),
                name: "run_query".into(),
                input: serde_json::json!({ "sql": "SELECT id FROM t" }),
            };
            let out = exec.execute(&call).await.unwrap();
            assert!(out.starts_with("auto_executed\n"), "expected auto_executed envelope, got: {out}");
            assert!(out.contains("id"), "body should still contain rendered table: {out}");
        }

        // error: invalid SQL surfaces under the `error` envelope with a `message:` body.
        {
            let mut approver = AutoApprove;
            let mut exec = QueryToolExecutor {
                db: Arc::new(Mutex::new(Database::connect(&url).await.unwrap())),
                mode: naque_core::PermissionMode::Wildcard,
                catastrophic_guard: true,
                schema: None,
                approver: &mut approver,
                last_result: None,
                last_byte_columns: Vec::new(),
            };
            let call = ToolCall {
                id: "err".into(),
                name: "run_query".into(),
                input: serde_json::json!({ "sql": "SELECT * FROM no_such_table" }),
            };
            let out = exec.execute(&call).await.unwrap();
            assert!(out.starts_with("error\n"), "expected error envelope, got: {out}");
            assert!(out.contains("message:"), "error body should carry a message: key: {out}");
        }

        // rejected: a write under ReadOnly mode prompts; AutoReject declines.
        {
            let mut approver = AutoReject;
            let mut exec = QueryToolExecutor {
                db: Arc::new(Mutex::new(Database::connect(&url).await.unwrap())),
                mode: naque_core::PermissionMode::ReadOnly,
                catastrophic_guard: true,
                schema: None,
                approver: &mut approver,
                last_result: None,
                last_byte_columns: Vec::new(),
            };
            let call = ToolCall {
                id: "rej".into(),
                name: "run_query".into(),
                input: serde_json::json!({ "sql": "INSERT INTO t VALUES (2)" }),
            };
            let out = exec.execute(&call).await.unwrap();
            assert!(out.starts_with("rejected\n"), "expected rejected envelope, got: {out}");
            assert!(out.contains("reason:"), "rejected body should carry a reason: key: {out}");
        }
    }

    #[tokio::test]
    async fn inspect_table_rejects_malicious_name() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let db = Database::connect(&url).await.unwrap();
        let mut approver = AutoApprove;
        let mut exec = QueryToolExecutor {
            db: Arc::new(Mutex::new(db)),
            mode: naque_core::PermissionMode::ReadOnly,
            catastrophic_guard: true,
            schema: None,
            approver: &mut approver,
            last_result: None,
            last_byte_columns: Vec::new(),
        };

        let call = ToolCall {
            id: "tc".into(),
            name: "inspect_table".into(),
            input: serde_json::json!({ "name": "t'; DROP TABLE t; --" }),
        };
        let out = exec.execute(&call).await.unwrap();
        assert!(out.starts_with("error: invalid table name"), "expected rejection, got: {out}");
    }
}
