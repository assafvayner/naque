//! The agent's `ToolExecutor` implementation.
//!
//! Routes the four standard tool calls (`inspect_table`, `sample_table`,
//! `explain`, `run_query`) to the database, going through the permission gate
//! for anything that modifies state.

use naque_core::gate::QueryKind;
use naque_db::{Database, QueryResult};
use naque_llm::{LlmError, ToolCall, ToolExecutor};
use naque_schema::SchemaModel;

use crate::approval::Approver;
use crate::run_gated;

pub struct QueryToolExecutor<'a> {
    pub db: &'a mut Database,
    pub mode: naque_core::PermissionMode,
    pub catastrophic_guard: bool,
    pub schema: &'a Option<SchemaModel>,
    pub approver: &'a mut dyn Approver,
    pub last_result: Option<QueryResult>,
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

        if let Some(schema) = self.schema
            && let Some(description) = schema.describe_table(name)
        {
            return Ok(description);
        }

        // Fall back to a live introspection query.
        let sql = match self.db.engine() {
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

        match self.db.fetch_readonly(&sql).await {
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

        let limit = call.input.get("limit").and_then(|v| v.as_u64()).unwrap_or(10).min(1000);

        let sql = format!("SELECT * FROM {name} LIMIT {limit}");

        match self.db.fetch_readonly(&sql).await {
            Ok(result) => {
                let text = format_result_text(&result);
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

        match self.db.fetch_readonly(&explain_sql).await {
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

        match run_gated(self.db, self.mode, self.catastrophic_guard, sql, QueryKind::Primary, self.approver).await {
            Ok(result) => {
                let text = format_result_text(&result);
                self.last_result = Some(result);
                Ok(text)
            },
            Err(e) => Ok(e), // surface error to the agent as a string so it can self-correct
        }
    }
}

/// Returns `true` if `name` is safe to interpolate into a SQL identifier
/// position. Allows letters, digits, underscores, dots, and double-quotes.
fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '"'))
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

    #[tokio::test]
    async fn inspect_table_rejects_malicious_name() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut db = Database::connect(&url).await.unwrap();
        let schema: Option<SchemaModel> = None;
        let mut approver = AutoApprove;
        let mut exec = QueryToolExecutor {
            db: &mut db,
            mode: naque_core::PermissionMode::ReadOnly,
            catastrophic_guard: true,
            schema: &schema,
            approver: &mut approver,
            last_result: None,
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
