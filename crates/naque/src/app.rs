//! Headless engine — routes input, gates SQL, drives the LLM agent.

use naque_core::gate::QueryKind;
use naque_core::PermissionMode;
use naque_db::{Database, Engine, QueryResult};
use naque_llm::{Agent, Usage};
use naque_schema::SchemaModel;
use naque_sql::{classify, SqlDialect};
use naque_tui::{route_input, Input};

use crate::approval::Approver;
use crate::executor::{format_result_text, QueryToolExecutor};
use crate::run_gated;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Db(#[from] naque_db::DbError),
    #[error("query rejected by user")]
    Rejected,
    #[error("LLM error: {0}")]
    Llm(String),
    #[error("{0}")]
    Other(String),
}

impl From<naque_llm::LlmError> for AppError {
    fn from(e: naque_llm::LlmError) -> Self {
        AppError::Llm(e.to_string())
    }
}

impl From<naque_schema::SchemaError> for AppError {
    fn from(e: naque_schema::SchemaError) -> Self {
        AppError::Other(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TranscriptEntry {
    User(String),
    Agent(String),
    Sql { sql: String, label: String },
    Info(String),
    Error(String),
    Rejected(String),
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct App {
    pub(crate) db: Database,
    pub(crate) agent: Agent,
    pub(crate) mode: PermissionMode,
    pub(crate) profile_name: String,
    pub(crate) catastrophic_guard: bool,
    pub(crate) schema: Option<SchemaModel>,
    pub(crate) usage: Usage,
    pub(crate) row_cap: usize,
    pub(crate) last_result: Option<QueryResult>,
    pub(crate) transcript: Vec<TranscriptEntry>,
    pub(crate) should_quit: bool,
}

impl App {
    pub fn new(
        db: Database,
        agent: Agent,
        mode: PermissionMode,
        profile_name: impl Into<String>,
        catastrophic_guard: bool,
        row_cap: usize,
    ) -> Self {
        Self {
            db,
            agent,
            mode,
            profile_name: profile_name.into(),
            catastrophic_guard,
            schema: None,
            usage: Usage::default(),
            row_cap,
            last_result: None,
            transcript: Vec::new(),
            should_quit: false,
        }
    }

    // --- Accessors ---------------------------------------------------------

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    pub fn last_result(&self) -> Option<&QueryResult> {
        self.last_result.as_ref()
    }

    pub fn transcript(&self) -> &[TranscriptEntry] {
        &self.transcript
    }

    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn schema(&self) -> Option<&SchemaModel> {
        self.schema.as_ref()
    }

    // --- SQL execution -----------------------------------------------------

    /// Classify `sql`, run the permission gate, possibly prompt via `approver`,
    /// and execute with correct connection routing.
    pub async fn execute_sql(
        &mut self,
        sql: &str,
        kind: QueryKind,
        approver: &mut dyn Approver,
    ) -> Result<QueryResult, AppError> {
        match run_gated(&mut self.db, self.mode, self.catastrophic_guard, sql, kind, approver).await {
            Ok(result) => {
                // Build a label from the classification for the transcript.
                let dialect = engine_dialect(self.db.engine());
                let class = classify(sql, dialect);
                let label = class
                    .statements
                    .first()
                    .map(|s| s.label.clone())
                    .unwrap_or_else(|| "SQL".to_string());

                let mut capped = result.clone();
                if capped.rows.len() > self.row_cap {
                    capped.rows.truncate(self.row_cap);
                }

                self.last_result = Some(capped);
                self.transcript.push(TranscriptEntry::Sql {
                    sql: sql.to_string(),
                    label,
                });
                Ok(result)
            },
            Err(msg) => {
                if msg == "rejected" {
                    self.transcript.push(TranscriptEntry::Rejected(sql.to_string()));
                    Err(AppError::Rejected)
                } else {
                    self.transcript.push(TranscriptEntry::Error(msg.clone()));
                    Err(AppError::Other(msg))
                }
            },
        }
    }

    // --- Natural language --------------------------------------------------

    pub async fn handle_natural_language(&mut self, text: &str, approver: &mut dyn Approver) -> Result<(), AppError> {
        self.transcript.push(TranscriptEntry::User(text.to_string()));

        // Compute the catalog before splitting borrows.
        let catalog = self.schema.as_ref().map(|s| s.compact_catalog()).unwrap_or_default();

        // Destructure to satisfy the borrow checker: we need both
        // `self.agent` (exclusive borrow for run_turn) and `self.db` (passed
        // into the executor). Rust allows splitting borrows through field
        // access when they are truly disjoint.
        let App {
            agent,
            db,
            mode,
            catastrophic_guard,
            schema,
            ..
        } = self;

        let mut executor = QueryToolExecutor {
            db,
            mode: *mode,
            catastrophic_guard: *catastrophic_guard,
            schema,
            approver,
            last_result: None,
        };

        let turn = agent.run_turn(text, &catalog, &mut executor).await?;

        // Pull results out of the executor before giving control back.
        let exec_last_result = executor.last_result.take();

        // Reassign the fields that were not covered by the destructure.
        self.usage += turn.usage;
        if let Some(r) = exec_last_result {
            let mut capped = r;
            if capped.rows.len() > self.row_cap {
                capped.rows.truncate(self.row_cap);
            }
            self.last_result = Some(capped);
        }
        self.transcript.push(TranscriptEntry::Agent(turn.answer));

        Ok(())
    }

    // --- Command handlers --------------------------------------------------

    pub async fn handle_db_command(&mut self, cmd: &str) -> Result<(), AppError> {
        let cmd = cmd.trim();

        if cmd == "reset" {
            self.db.reconnect().await?;
            self.transcript.push(TranscriptEntry::Info("reconnected".to_string()));
            return Ok(());
        }

        if cmd == "dt" {
            if let Some(schema) = &self.schema {
                let names: Vec<String> = schema.tables.iter().map(|t| t.name.clone()).collect();
                self.transcript
                    .push(TranscriptEntry::Info(format!("tables: {}", names.join(", "))));
                // Also set last_result so callers can inspect it.
                let rows: Vec<Vec<Option<String>>> = names.iter().map(|n| vec![Some(n.clone())]).collect();
                self.last_result = Some(QueryResult {
                    columns: vec![naque_db::Column {
                        name: "table_name".to_string(),
                        type_name: "text".to_string(),
                    }],
                    rows,
                    rows_affected: None,
                });
            } else {
                let sql = match self.db.engine() {
                    Engine::Sqlite => "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
                    Engine::Postgres => {
                        "SELECT table_name FROM information_schema.tables \
                         WHERE table_schema = 'public' ORDER BY table_name"
                    },
                };
                match self.db.fetch_readonly(sql).await {
                    Ok(result) => {
                        let text = format_result_text(&result);
                        self.transcript.push(TranscriptEntry::Info(text));
                        self.last_result = Some(result);
                    },
                    Err(e) => {
                        self.transcript.push(TranscriptEntry::Error(e.to_string()));
                    },
                }
            }
            return Ok(());
        }

        if let Some(table) = cmd.strip_prefix("d ") {
            let table = table.trim();
            if let Some(schema) = &self.schema {
                if let Some(description) = schema.describe_table(table) {
                    self.transcript.push(TranscriptEntry::Info(description));
                } else {
                    self.transcript.push(TranscriptEntry::Info(format!("table not found: {table}")));
                }
            } else {
                self.transcript
                    .push(TranscriptEntry::Info("schema not learned yet; run /learn".to_string()));
            }
            return Ok(());
        }

        if let Some(rest) = cmd.strip_prefix("set ") {
            self.transcript.push(TranscriptEntry::Info(format!("set {rest}")));
            return Ok(());
        }

        self.transcript
            .push(TranscriptEntry::Info(format!("unknown db command: {cmd}")));
        Ok(())
    }

    pub async fn handle_tool_command(&mut self, cmd: &str, approver: &mut dyn Approver) -> Result<(), AppError> {
        let cmd = cmd.trim();

        if let Some(rest) = cmd.strip_prefix("mode ") {
            match rest.trim().parse::<PermissionMode>() {
                Ok(m) => {
                    self.mode = m;
                    self.transcript.push(TranscriptEntry::Info(format!("mode set to {m}")));
                },
                Err(e) => {
                    self.transcript.push(TranscriptEntry::Info(format!("unknown mode: {e}")));
                },
            }
            return Ok(());
        }

        if cmd == "clear" {
            self.agent.clear();
            self.transcript.push(TranscriptEntry::Info("agent memory cleared".to_string()));
            return Ok(());
        }

        if cmd == "learn" {
            match naque_schema::introspect(&mut self.db).await {
                Ok(model) => {
                    let count = model.tables.len();
                    self.schema = Some(model);
                    self.transcript.push(TranscriptEntry::Info(format!("learned {count} table(s)")));
                },
                Err(e) => {
                    self.transcript.push(TranscriptEntry::Error(format!("learn failed: {e}")));
                },
            }
            return Ok(());
        }

        if cmd == "cost" {
            let total = self.usage.input_tokens + self.usage.output_tokens;
            self.transcript.push(TranscriptEntry::Info(format!(
                "tokens used: {} in + {} out = {} total",
                self.usage.input_tokens, self.usage.output_tokens, total
            )));
            return Ok(());
        }

        if let Some(rest) = cmd.strip_prefix("export ") {
            match rest.trim() {
                "csv" => {
                    if let Some(result) = &self.last_result {
                        let table = naque_tui::ResultTable::new(
                            result.columns.iter().map(|c| c.name.clone()).collect(),
                            result.rows.clone(),
                        );
                        let csv = table.to_csv();
                        self.transcript.push(TranscriptEntry::Info(format!("--- CSV ---\n{csv}")));
                    } else {
                        self.transcript.push(TranscriptEntry::Info("no result to export".to_string()));
                    }
                },
                "json" => {
                    if let Some(result) = &self.last_result {
                        let table = naque_tui::ResultTable::new(
                            result.columns.iter().map(|c| c.name.clone()).collect(),
                            result.rows.clone(),
                        );
                        let json = table.to_json();
                        self.transcript.push(TranscriptEntry::Info(format!("--- JSON ---\n{json}")));
                    } else {
                        self.transcript.push(TranscriptEntry::Info("no result to export".to_string()));
                    }
                },
                fmt => {
                    self.transcript
                        .push(TranscriptEntry::Info(format!("unknown export format: {fmt}")));
                },
            }
            return Ok(());
        }

        if cmd == "quit" || cmd == "exit" {
            self.should_quit = true;
            return Ok(());
        }

        // Silence the unused parameter warning in the non-NL paths.
        let _ = approver;

        self.transcript
            .push(TranscriptEntry::Info(format!("unknown tool command: {cmd}")));
        Ok(())
    }

    /// Route a raw input line and dispatch to the appropriate handler.
    pub async fn handle_line(&mut self, line: &str, approver: &mut dyn Approver) -> Result<(), AppError> {
        match route_input(line) {
            Input::NaturalLanguage(text) => {
                self.handle_natural_language(&text, approver).await?;
            },
            Input::RawSql(sql) => {
                // `execute_sql` already records the outcome (Sql / Rejected / Error)
                // in the transcript; don't double-record it here.
                let _ = self.execute_sql(&sql, QueryKind::Primary, approver).await;
            },
            Input::DbCommand(cmd) => {
                self.handle_db_command(&cmd).await?;
            },
            Input::ToolCommand(cmd) => {
                self.handle_tool_command(&cmd, approver).await?;
            },
            Input::Empty => {},
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn engine_dialect(engine: Engine) -> SqlDialect {
    match engine {
        Engine::Postgres => SqlDialect::Postgres,
        Engine::Sqlite => SqlDialect::Sqlite,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use naque_llm::{AgentConfig, LlmResponse, MockProvider, ToolCall, Usage as LlmUsage};

    use super::*;
    use crate::approval::{AutoApprove, AutoReject, ScriptedApprover};
    use crate::ApprovalDecision;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn mock_agent(responses: Vec<LlmResponse>) -> Agent {
        Agent::new(
            Box::new(MockProvider::new(responses)),
            AgentConfig {
                model: "mock".to_string(),
                max_iterations: 10,
                max_tokens: 1024,
                system_preamble: "You are a SQL assistant.".to_string(),
            },
        )
    }

    pub async fn make_app(db_url: &str, mode: PermissionMode, responses: Vec<LlmResponse>) -> App {
        let db = Database::connect(db_url).await.expect("connect");
        let agent = mock_agent(responses);
        App::new(db, agent, mode, "test", false, 1000)
    }

    pub async fn make_app_guard(
        db_url: &str,
        mode: PermissionMode,
        catastrophic_guard: bool,
        responses: Vec<LlmResponse>,
    ) -> App {
        let db = Database::connect(db_url).await.expect("connect");
        let agent = mock_agent(responses);
        App::new(db, agent, mode, "test", catastrophic_guard, 1000)
    }

    // ------------------------------------------------------------------
    // Test 1: wildcard mode — DDL + insert + select
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn wildcard_crud_round_trip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;

        app.handle_line("!CREATE TABLE t(id INTEGER, name TEXT)", &mut AutoApprove)
            .await
            .unwrap();

        app.handle_line("!INSERT INTO t VALUES (1,'a')", &mut AutoApprove)
            .await
            .unwrap();

        app.handle_line("!SELECT * FROM t", &mut AutoApprove).await.unwrap();

        let result = app.last_result().expect("last_result should be Some");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Some("1".to_string()));
        assert_eq!(result.rows[0][1], Some("a".to_string()));
    }

    // Regression: a failed raw SQL must record exactly ONE Error transcript
    // entry (execute_sql records it; handle_line must not double-record).
    #[tokio::test]
    async fn raw_sql_error_recorded_once() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;

        app.handle_line("!SELECT * FROM does_not_exist", &mut AutoApprove)
            .await
            .unwrap();

        let errors = app
            .transcript()
            .iter()
            .filter(|e| matches!(e, TranscriptEntry::Error(_)))
            .count();
        assert_eq!(errors, 1, "a failed raw SQL must record exactly one Error entry");
    }

    // ------------------------------------------------------------------
    // Test 2: default mode + AutoReject — rejected insert leaves row count
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn default_mode_reject_leaves_data_unchanged() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        // Start in wildcard to freely set up.
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;

        // Insert one row in wildcard.
        app.handle_line("!CREATE TABLE t(id INTEGER, name TEXT)", &mut AutoApprove)
            .await
            .unwrap();
        app.handle_line("!INSERT INTO t VALUES (1,'a')", &mut AutoApprove)
            .await
            .unwrap();

        // Switch to default mode and try to insert — reject it.
        app.mode = PermissionMode::Default;
        app.handle_line("!INSERT INTO t VALUES (2,'b')", &mut AutoReject).await.unwrap();

        // Switch back to wildcard to freely query.
        app.mode = PermissionMode::Wildcard;
        app.handle_line("!SELECT count(*) FROM t", &mut AutoApprove).await.unwrap();

        let result = app.last_result().expect("last_result");
        // COUNT(*) returns a single row with value "1".
        assert_eq!(result.rows[0][0], Some("1".to_string()));
    }

    // ------------------------------------------------------------------
    // Test 3: catastrophic guard — DROP TABLE rejected
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn catastrophic_guard_blocks_drop() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app_guard(&url, PermissionMode::Wildcard, true, vec![]).await;

        app.handle_line("!CREATE TABLE t(id INTEGER)", &mut AutoApprove).await.unwrap();
        app.handle_line("!INSERT INTO t VALUES (42)", &mut AutoApprove).await.unwrap();

        // DROP TABLE is catastrophic → PromptCatastrophic → AutoReject.
        app.handle_line("!DROP TABLE t", &mut AutoReject).await.unwrap();

        // Table should still exist.
        app.handle_line("!SELECT * FROM t", &mut AutoApprove).await.unwrap();

        let result = app.last_result().expect("last_result");
        assert_eq!(result.rows.len(), 1);
    }

    // ------------------------------------------------------------------
    // Test 4: readonly routing — SELECT goes through fetch_readonly
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn readonly_mode_select_returns_rows() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());

        // Set up data in wildcard.
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;
        app.handle_line("!CREATE TABLE t(id INTEGER)", &mut AutoApprove).await.unwrap();
        app.handle_line("!INSERT INTO t VALUES (7)", &mut AutoApprove).await.unwrap();

        // Switch to readonly.
        app.mode = PermissionMode::ReadOnly;

        app.handle_line("!SELECT * FROM t", &mut AutoApprove).await.unwrap();

        let result = app.last_result().expect("last_result");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Some("7".to_string()));
    }

    // ------------------------------------------------------------------
    // Test 5: NL turn via MockProvider + run_query tool
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn nl_turn_with_run_query_tool() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());

        // Set up data.
        let mut setup = make_app(&url, PermissionMode::Wildcard, vec![]).await;
        setup
            .handle_line("!CREATE TABLE t(id INTEGER)", &mut AutoApprove)
            .await
            .unwrap();
        setup.handle_line("!INSERT INTO t VALUES (99)", &mut AutoApprove).await.unwrap();
        drop(setup);

        // Build scripted provider:
        // - Response 1: tool call to run_query with SELECT * FROM t
        // - Response 2: final text answer "done"
        let resp1 = LlmResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "tc1".to_string(),
                name: "run_query".to_string(),
                input: serde_json::json!({ "sql": "SELECT * FROM t" }),
            }],
            usage: LlmUsage {
                input_tokens: 10,
                output_tokens: 5,
            },
            stop_reason: "tool_use".to_string(),
        };
        let resp2 = LlmResponse {
            text: Some("done".to_string()),
            tool_calls: vec![],
            usage: LlmUsage {
                input_tokens: 8,
                output_tokens: 3,
            },
            stop_reason: "end_turn".to_string(),
        };

        let mut app = make_app(&url, PermissionMode::Wildcard, vec![resp1, resp2]).await;

        app.handle_natural_language("show t", &mut AutoApprove).await.unwrap();

        // Agent answer recorded.
        let has_agent = app
            .transcript()
            .iter()
            .any(|e| matches!(e, TranscriptEntry::Agent(s) if s == "done"));
        assert!(has_agent, "transcript should contain Agent(\"done\")");

        // last_result populated from run_query.
        let result = app.last_result().expect("last_result should be Some");
        assert_eq!(result.rows.len(), 1);

        // Usage accumulated.
        assert!(app.usage().input_tokens > 0, "cumulative usage should be > 0");
    }

    // ------------------------------------------------------------------
    // Test 6: tool commands and mode switching
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn tool_commands() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Default, vec![]).await;

        // /mode readonly
        app.handle_line("/mode readonly", &mut AutoApprove).await.unwrap();
        assert_eq!(app.mode(), PermissionMode::ReadOnly);

        // /cost — should add an Info entry.
        app.handle_line("/cost", &mut AutoApprove).await.unwrap();
        let has_cost = app
            .transcript()
            .iter()
            .any(|e| matches!(e, TranscriptEntry::Info(s) if s.contains("tokens")));
        assert!(has_cost, "transcript should contain token info");

        // /clear
        app.handle_line("/clear", &mut AutoApprove).await.unwrap();
        let has_clear = app
            .transcript()
            .iter()
            .any(|e| matches!(e, TranscriptEntry::Info(s) if s.contains("cleared")));
        assert!(has_clear, "transcript should contain cleared info");

        // /quit
        assert!(!app.should_quit());
        app.handle_line("/quit", &mut AutoApprove).await.unwrap();
        assert!(app.should_quit());
    }

    // ------------------------------------------------------------------
    // Test 7: /learn populates schema with table name
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn learn_populates_schema() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;

        app.handle_line("!CREATE TABLE t(id INTEGER, name TEXT)", &mut AutoApprove)
            .await
            .unwrap();

        assert!(app.schema().is_none());

        app.handle_line("/learn", &mut AutoApprove).await.unwrap();

        let schema = app.schema().expect("schema should be Some after /learn");
        let catalog = schema.compact_catalog();
        assert!(catalog.contains('t'), "compact_catalog should mention table 't', got: {catalog}");
    }

    // ------------------------------------------------------------------
    // Test 8: AcceptEdited re-gates and runs the new SQL
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn accept_edited_reruns_new_sql() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Default, vec![]).await;

        // Setup in wildcard.
        app.mode = PermissionMode::Wildcard;
        app.handle_line("!CREATE TABLE t(id INTEGER)", &mut AutoApprove).await.unwrap();
        app.handle_line("!INSERT INTO t VALUES (1)", &mut AutoApprove).await.unwrap();
        app.handle_line("!INSERT INTO t VALUES (2)", &mut AutoApprove).await.unwrap();

        // Now in default mode: user edits the SQL before approving.
        // The original SQL is "SELECT * FROM t" (read → Prompt in Default Primary).
        // The edited SQL is also a read in Default Primary → Prompt again → Accept.
        app.mode = PermissionMode::Default;
        let mut scripted = ScriptedApprover::new([
            ApprovalDecision::AcceptEdited("SELECT * FROM t WHERE id = 1".to_string()),
            ApprovalDecision::Accept,
        ]);

        app.execute_sql("SELECT * FROM t", QueryKind::Primary, &mut scripted)
            .await
            .unwrap();

        let result = app.last_result().expect("last_result");
        // The edited query returns only the row with id=1.
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Some("1".to_string()));
    }
}
