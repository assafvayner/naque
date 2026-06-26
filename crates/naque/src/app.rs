//! Headless engine — routes input, gates SQL, drives the LLM agent.

use std::sync::Arc;

use naque_core::PermissionMode;
use naque_core::gate::QueryKind;
use naque_db::{Database, Engine, QueryResult};
use naque_llm::{Agent, Usage};
use naque_schema::SchemaModel;
use naque_sql::{SqlDialect, classify};
use naque_tui::{Input, route_input};
use tokio::sync::Mutex;

use crate::approval::Approver;
use crate::executor::{QueryToolExecutor, format_result_text};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Running,
    Ok,
    Err,
}

#[derive(Debug, Clone)]
pub enum TranscriptEntry {
    User(String),
    Agent(String),
    Sql {
        sql: String,
        label: String,
    },
    Info(String),
    Error(String),
    Rejected(String),
    /// Streamed model narration that precedes/accompanies tool calls.
    Reasoning(String),
    /// A tool step; renders multi-line while `Running`, collapses when done.
    ToolStep {
        name: String,
        sql: Option<String>,
        status: StepStatus,
        summary: Option<String>,
    },
}

/// Fold one [`AgentEvent`] into the transcript. `cur` is the index of the
/// currently-streaming `Reasoning` entry, if any.
///
/// Rules:
/// - `TextDelta` appends to the current `Reasoning` entry (creating one if needed).
/// - `ToolCallStarted` finalizes any streaming reasoning, then pushes a `Running` `ToolStep`.
/// - `ToolCallFinished` collapses the matching trailing `ToolStep`.
/// - `TurnFinished` relabels a trailing streaming `Reasoning` as the `Agent` answer (the final block of text is the
///   answer, not narration).
/// - `Cancelled` finalizes streaming reasoning and appends an `Info` note.
pub fn apply_event_to_transcript(
    transcript: &mut Vec<TranscriptEntry>,
    cur: &mut Option<usize>,
    event: &naque_llm::AgentEvent,
) {
    use naque_llm::AgentEvent as E;
    match event {
        E::TextDelta(chunk) => {
            let idx = match *cur {
                Some(i) => i,
                None => {
                    transcript.push(TranscriptEntry::Reasoning(String::new()));
                    let i = transcript.len() - 1;
                    *cur = Some(i);
                    i
                },
            };
            if let Some(TranscriptEntry::Reasoning(s)) = transcript.get_mut(idx) {
                s.push_str(chunk);
            }
        },
        E::ToolCallStarted { name, sql } => {
            *cur = None;
            transcript.push(TranscriptEntry::ToolStep {
                name: name.clone(),
                sql: sql.clone(),
                status: StepStatus::Running,
                summary: None,
            });
        },
        E::ToolCallFinished { summary, is_error, .. } => {
            if let Some(TranscriptEntry::ToolStep {
                status, summary: slot, ..
            }) = transcript.iter_mut().rev().find(|e| {
                matches!(
                    e,
                    TranscriptEntry::ToolStep {
                        status: StepStatus::Running,
                        ..
                    }
                )
            }) {
                *status = if *is_error { StepStatus::Err } else { StepStatus::Ok };
                *slot = Some(summary.clone());
            }
        },
        E::TurnFinished {
            iterations,
            hit_iteration_cap,
        } => {
            if let Some(i) = cur.take()
                && let Some(TranscriptEntry::Reasoning(s)) = transcript.get(i)
            {
                let answer = s.clone();
                transcript[i] = TranscriptEntry::Agent(answer);
            }
            // On an iteration-cap finish the loop's last events were tool calls,
            // so `cur` is already None and nothing was relabeled into an answer.
            // Surface an explicit notice so the turn doesn't appear to stop silently.
            if *hit_iteration_cap {
                transcript.push(TranscriptEntry::Info(format!(
                    "(stopped after {iterations} rounds: reached max iterations)"
                )));
            }
        },
        E::Cancelled => {
            *cur = None;
            transcript.push(TranscriptEntry::Info("(cancelled)".into()));
        },
        E::TurnStarted | E::LlmCallStarted { .. } | E::UsageUpdated(_) => {},
    }
}

/// One-line description of what the active permission mode lets the agent do.
///
/// Injected into each turn's context so the model's behavior tracks `/mode`
/// changes. Without it the agent only infers the mode from tool rejections and
/// keeps refusing writes after the user switches to a permissive mode.
pub(crate) fn mode_guidance(mode: PermissionMode, catastrophic_guard: bool) -> String {
    let base = match mode {
        PermissionMode::Wildcard => {
            "Permission mode: WILDCARD — every statement runs automatically without approval. \
             Use run_query to execute INSERT, UPDATE, DELETE, and DDL directly when the user asks."
        },
        PermissionMode::Default => {
            "Permission mode: DEFAULT — read-only queries run automatically; INSERT, UPDATE, \
             DELETE, and DDL run only after the user approves a prompt. Go ahead and issue the \
             write the user asked for — they will approve or reject it."
        },
        PermissionMode::ReadOnly => {
            "Permission mode: READ-ONLY — read-only queries run automatically; any write or DDL \
             prompts the user for approval before running. Prefer reads, but you may still issue \
             a write the user explicitly requests."
        },
        PermissionMode::Strict => {
            "Permission mode: STRICT — every statement, including reads, requires the user to \
             approve it before it runs."
        },
    };
    if catastrophic_guard && matches!(mode, PermissionMode::Wildcard) {
        format!("{base} (Dropping or truncating objects still asks the user to confirm.)")
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod apply_tests {
    use naque_llm::AgentEvent;

    use super::*;

    fn names(t: &[TranscriptEntry]) -> Vec<&'static str> {
        t.iter()
            .map(|e| match e {
                TranscriptEntry::Reasoning(_) => "reasoning",
                TranscriptEntry::ToolStep { .. } => "step",
                TranscriptEntry::Agent(_) => "agent",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn deltas_accumulate_into_one_reasoning_entry() {
        let mut t: Vec<TranscriptEntry> = vec![];
        let mut cur: Option<usize> = None;
        apply_event_to_transcript(&mut t, &mut cur, &AgentEvent::TextDelta("Hel".into()));
        apply_event_to_transcript(&mut t, &mut cur, &AgentEvent::TextDelta("lo".into()));
        assert_eq!(t.len(), 1);
        assert!(matches!(&t[0], TranscriptEntry::Reasoning(s) if s == "Hello"));
    }

    #[test]
    fn tool_started_then_finished_collapses() {
        let mut t: Vec<TranscriptEntry> = vec![];
        let mut cur: Option<usize> = None;
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::ToolCallStarted {
                name: "run_query".into(),
                sql: Some("SELECT 1".into()),
            },
        );
        assert!(matches!(
            &t[0],
            TranscriptEntry::ToolStep {
                status: StepStatus::Running,
                ..
            }
        ));
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::ToolCallFinished {
                name: "run_query".into(),
                summary: "1 rows".into(),
                is_error: false,
            },
        );
        match &t[0] {
            TranscriptEntry::ToolStep { status, summary, .. } => {
                assert_eq!(*status, StepStatus::Ok);
                assert_eq!(summary.as_deref(), Some("1 rows"));
            },
            _ => panic!("expected ToolStep"),
        }
    }

    #[test]
    fn finish_relabels_trailing_reasoning_as_agent() {
        let mut t: Vec<TranscriptEntry> = vec![];
        let mut cur: Option<usize> = None;
        apply_event_to_transcript(&mut t, &mut cur, &AgentEvent::TextDelta("final".into()));
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::TurnFinished {
                iterations: 1,
                hit_iteration_cap: false,
            },
        );
        assert_eq!(names(&t), vec!["agent"]);
        assert!(matches!(&t[0], TranscriptEntry::Agent(s) if s == "final"));
    }

    #[test]
    fn finish_with_iteration_cap_pushes_notice() {
        // A capped turn ends right after tool calls, so `cur` is None and there
        // is no trailing reasoning to relabel — the answer would otherwise be
        // dropped. The finish must leave an explicit notice instead.
        let mut t: Vec<TranscriptEntry> = vec![];
        let mut cur: Option<usize> = None;
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::ToolCallStarted {
                name: "run_query".into(),
                sql: Some("SELECT 1".into()),
            },
        );
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::ToolCallFinished {
                name: "run_query".into(),
                summary: "1 rows".into(),
                is_error: false,
            },
        );
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::TurnFinished {
                iterations: 12,
                hit_iteration_cap: true,
            },
        );
        assert!(
            matches!(t.last(), Some(TranscriptEntry::Info(s)) if s.contains("max iterations")),
            "capped finish must push an explicit notice, got {t:?}"
        );
    }

    #[test]
    fn tool_call_after_reasoning_finalizes_it() {
        let mut t: Vec<TranscriptEntry> = vec![];
        let mut cur: Option<usize> = None;
        apply_event_to_transcript(&mut t, &mut cur, &AgentEvent::TextDelta("let me check".into()));
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::ToolCallStarted {
                name: "run_query".into(),
                sql: None,
            },
        );
        apply_event_to_transcript(&mut t, &mut cur, &AgentEvent::TextDelta("the answer is 5".into()));
        apply_event_to_transcript(
            &mut t,
            &mut cur,
            &AgentEvent::TurnFinished {
                iterations: 2,
                hit_iteration_cap: false,
            },
        );
        assert_eq!(names(&t), vec!["reasoning", "step", "agent"]);
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct App {
    pub(crate) db: Arc<Mutex<Database>>,
    pub(crate) agent_slot: Option<Agent>,
    pub(crate) mode: PermissionMode,
    pub(crate) profile_name: String,
    pub(crate) catastrophic_guard: bool,
    pub(crate) schema: Option<SchemaModel>,
    pub(crate) usage: Usage,
    pub(crate) row_cap: usize,
    pub(crate) last_result: Option<QueryResult>,
    pub(crate) transcript: Vec<TranscriptEntry>,
    pub(crate) should_quit: bool,
    pub(crate) max_iterations: u32,
    pub(crate) live: crate::live::LiveState,
    pub(crate) quit_armed: bool,
    /// Index of the in-progress streamed Reasoning entry, if any.
    pub(crate) streaming_idx: Option<usize>,
    pub(crate) inflight: Option<crate::turn::RunningTurn>,
    pub(crate) event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<naque_llm::AgentEvent>>,
    pub(crate) approval_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::approval::ApprovalRequest>>,
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
        let max_iterations = agent.max_iterations();
        Self {
            db: Arc::new(Mutex::new(db)),
            agent_slot: Some(agent),
            mode,
            profile_name: profile_name.into(),
            catastrophic_guard,
            schema: None,
            usage: Usage::default(),
            row_cap,
            last_result: None,
            transcript: Vec::new(),
            should_quit: false,
            max_iterations,
            live: crate::live::LiveState::new(max_iterations),
            quit_armed: false,
            streaming_idx: None,
            inflight: None,
            event_rx: None,
            approval_rx: None,
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

    #[cfg(test)]
    pub(crate) fn transcript_mut(&mut self) -> &mut Vec<TranscriptEntry> {
        &mut self.transcript
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

    /// Whether a raw SQL statement would be auto-approved by the gate in the
    /// current mode (so it can run inline without a prompt).
    pub async fn raw_sql_auto_approves(&self, sql: &str) -> bool {
        use naque_core::gate::{GateDecision, gate_decision};
        let dialect = engine_dialect(self.db.lock().await.engine());
        let class = classify(sql, dialect);
        matches!(
            gate_decision(self.mode, &class, QueryKind::Primary, self.catastrophic_guard),
            GateDecision::AutoApprove
        )
    }

    /// Push an informational transcript entry.
    pub fn push_info(&mut self, msg: impl Into<String>) {
        self.transcript.push(TranscriptEntry::Info(msg.into()));
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
        let gate_result = {
            let mut db = self.db.lock().await;
            run_gated(&mut db, self.mode, self.catastrophic_guard, sql, kind, approver).await
        };
        match gate_result {
            Ok(result) => {
                // Build a label from the classification for the transcript.
                let dialect = engine_dialect(self.db.lock().await.engine());
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

    /// Per-turn context appended to the agent's system prompt: the active
    /// permission mode (so behavior tracks `/mode`) then the schema catalog.
    fn turn_context(&self) -> String {
        let catalog = self.schema.as_ref().map(|s| s.compact_catalog()).unwrap_or_default();
        let guidance = mode_guidance(self.mode, self.catastrophic_guard);
        if catalog.is_empty() {
            guidance
        } else {
            format!("{guidance}\n\n{catalog}")
        }
    }

    pub async fn handle_natural_language(&mut self, text: &str, approver: &mut dyn Approver) -> Result<(), AppError> {
        self.transcript.push(TranscriptEntry::User(text.to_string()));
        let context = self.turn_context();

        let mut agent = self.agent_slot.take().expect("agent available");
        let mut executor = QueryToolExecutor {
            db: Arc::clone(&self.db),
            mode: self.mode,
            catastrophic_guard: self.catastrophic_guard,
            schema: self.schema.clone(),
            approver,
            last_result: None,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = agent.run_turn(text, &context, &mut executor, &mut (), &cancel).await;
        let exec_last = executor.last_result.take();
        self.agent_slot = Some(agent);

        let turn = result?;
        self.usage += turn.usage;
        if let Some(r) = exec_last {
            let mut capped = r;
            if capped.rows.len() > self.row_cap {
                capped.rows.truncate(self.row_cap);
            }
            self.last_result = Some(capped);
        }
        self.transcript.push(TranscriptEntry::Agent(turn.answer));
        Ok(())
    }

    // --- Spawned turn (UI loop) --------------------------------------------

    /// True when a new turn can be started: an agent is available and no turn
    /// is currently in flight. The UI must gate `start_turn` on THIS, not just
    /// `!is_turn_running()` (which diverges after a task panic loses the agent).
    pub fn can_start_turn(&self) -> bool {
        self.agent_slot.is_some() && self.inflight.is_none()
    }

    /// Spawn a natural-language turn on a background task. Takes the agent out
    /// of the slot; events arrive via `next_event`, completion via `poll_finished`.
    pub fn start_turn(&mut self, text: &str) {
        if !self.can_start_turn() {
            self.transcript.push(TranscriptEntry::Error(
                "cannot start a new turn (agent unavailable or a turn is already running)".to_string(),
            ));
            return;
        }
        self.transcript.push(TranscriptEntry::User(text.to_string()));
        self.streaming_idx = None;
        self.live = crate::live::LiveState::new(self.max_iterations);
        self.live.running = true;

        let context = self.turn_context();
        // Safe: `can_start_turn` guaranteed the slot is full above.
        let mut agent = self.agent_slot.take().expect("agent available");

        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = tokio_util::sync::CancellationToken::new();

        let db = std::sync::Arc::clone(&self.db);
        let schema = self.schema.clone();
        let mode = self.mode;
        let guard = self.catastrophic_guard;
        let input = text.to_string();
        let cancel_task = cancel.clone();

        let handle = tokio::spawn(async move {
            let mut observer = crate::turn::ChannelObserver::new(event_tx);
            let mut approver = crate::approval::ChannelApprover::new(approval_tx);
            let mut executor = crate::executor::QueryToolExecutor {
                db,
                mode,
                catastrophic_guard: guard,
                schema,
                approver: &mut approver,
                last_result: None,
            };
            let result = agent
                .run_turn(&input, &context, &mut executor, &mut observer, &cancel_task)
                .await;
            let last_result = executor.last_result.take();
            crate::turn::TurnOutput {
                agent,
                result,
                last_result,
            }
        });

        self.inflight = Some(crate::turn::RunningTurn { handle, cancel });
        self.event_rx = Some(event_rx);
        self.approval_rx = Some(approval_rx);
    }

    pub fn is_turn_running(&self) -> bool {
        self.inflight.is_some()
    }

    /// Await the next streamed event, or `None` if the event channel is closed.
    pub async fn next_event(&mut self) -> Option<naque_llm::AgentEvent> {
        match self.event_rx.as_mut() {
            Some(rx) => rx.recv().await,
            None => None,
        }
    }

    /// Non-blocking variant of `next_event` for draining remaining buffered events.
    pub fn try_recv_event(&mut self) -> Option<naque_llm::AgentEvent> {
        self.event_rx.as_mut().and_then(|rx| rx.try_recv().ok())
    }

    /// True once the spawned task has finished.
    pub fn poll_finished(&mut self) -> bool {
        self.inflight.as_mut().map(|t| t.handle.is_finished()).unwrap_or(false)
    }

    /// Apply one event to both the transcript and the live state.
    pub fn apply_event(&mut self, event: &naque_llm::AgentEvent) {
        apply_event_to_transcript(&mut self.transcript, &mut self.streaming_idx, event);
        self.live.apply(event);
    }

    /// Cancel the in-flight turn, if any.
    pub fn cancel_turn(&mut self) {
        if let Some(t) = &self.inflight {
            t.cancel.cancel();
        }
    }

    /// Take the next pending approval request from a running turn, if any.
    pub fn try_recv_approval(&mut self) -> Option<crate::approval::ApprovalRequest> {
        self.approval_rx.as_mut().and_then(|rx| rx.try_recv().ok())
    }

    /// Reclaim the agent + results after the task has finished.
    pub async fn finalize_turn(&mut self) {
        let Some(t) = self.inflight.take() else { return };
        let out = t.handle.await;

        // The task has finished, so all events it produced are now buffered.
        // Drain them so the final TurnFinished (which relabels the streamed
        // reasoning into the Agent answer) is applied before we tear down.
        while let Some(ev) = self.try_recv_event() {
            self.apply_event(&ev);
        }
        self.event_rx = None;
        self.approval_rx = None;

        match out {
            Ok(out) => {
                self.agent_slot = Some(out.agent);
                if let Some(r) = out.last_result {
                    let mut capped = r;
                    if capped.rows.len() > self.row_cap {
                        capped.rows.truncate(self.row_cap);
                    }
                    self.last_result = Some(capped);
                }
                match out.result {
                    Ok(turn) => {
                        self.usage += turn.usage;
                    },
                    Err(e) => {
                        self.transcript.push(TranscriptEntry::Error(e.to_string()));
                    },
                }
            },
            Err(join_err) => {
                // Task panicked: the moved agent is lost. Surface the error;
                // agent_slot stays None. The UI must refuse new turns while
                // agent_slot is None (it gates on that) so this can't wedge.
                self.transcript
                    .push(TranscriptEntry::Error(format!("turn task failed: {join_err}")));
            },
        }
        self.live.running = false;
        self.streaming_idx = None;
    }

    // --- Command handlers --------------------------------------------------

    pub async fn handle_db_command(&mut self, cmd: &str) -> Result<(), AppError> {
        let cmd = cmd.trim();

        if cmd == "reset" {
            self.db.lock().await.reconnect().await?;
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
                // Live introspect.
                let mut db = self.db.lock().await;
                let sql = match db.engine() {
                    Engine::Sqlite => "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
                    Engine::Postgres => {
                        "SELECT table_name FROM information_schema.tables \
                         WHERE table_schema = 'public' ORDER BY table_name"
                    },
                };
                match db.fetch_readonly(sql).await {
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

        // Bare `/` or `/help` shows the command reference.
        if cmd.is_empty() || cmd == "help" {
            self.transcript.push(TranscriptEntry::Info(naque_tui::help_text()));
            return Ok(());
        }

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
            if let Some(a) = self.agent_slot.as_mut() {
                a.clear();
            }
            self.transcript.push(TranscriptEntry::Info("agent memory cleared".to_string()));
            return Ok(());
        }

        if cmd == "learn" {
            match naque_schema::introspect(&mut *self.db.lock().await).await {
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
    use crate::ApprovalDecision;
    use crate::approval::{AutoApprove, AutoReject, ScriptedApprover};

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

        // /help — lists the slash commands.
        app.handle_line("/help", &mut AutoApprove).await.unwrap();
        let has_help = app
            .transcript()
            .iter()
            .any(|e| matches!(e, TranscriptEntry::Info(s) if s.contains("Slash commands:") && s.contains("/mode")));
        assert!(has_help, "transcript should contain the help listing");

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

    #[test]
    fn mode_guidance_describes_each_mode() {
        assert!(mode_guidance(PermissionMode::Wildcard, false).contains("WILDCARD"));
        assert!(
            mode_guidance(PermissionMode::Wildcard, false)
                .to_ascii_lowercase()
                .contains("insert")
        );
        assert!(mode_guidance(PermissionMode::ReadOnly, false).contains("READ-ONLY"));
        assert!(mode_guidance(PermissionMode::Default, false).contains("DEFAULT"));
        assert!(mode_guidance(PermissionMode::Strict, false).contains("STRICT"));
    }

    #[test]
    fn mode_guidance_notes_guard_only_in_wildcard() {
        assert!(
            mode_guidance(PermissionMode::Wildcard, true)
                .to_ascii_lowercase()
                .contains("confirm")
        );
        assert!(
            !mode_guidance(PermissionMode::Wildcard, false)
                .to_ascii_lowercase()
                .contains("confirm")
        );
    }

    /// Regression: switching mode via `/mode` must change what the agent is
    /// told each turn, so it stops refusing writes after going to wildcard.
    #[tokio::test]
    async fn turn_context_tracks_mode_changes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::ReadOnly, vec![]).await;
        assert!(app.turn_context().contains("READ-ONLY"));

        app.handle_line("/mode wildcard", &mut AutoApprove).await.unwrap();
        assert!(
            app.turn_context().contains("WILDCARD"),
            "after /mode wildcard the agent context must say WILDCARD, got: {}",
            app.turn_context()
        );
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

#[cfg(test)]
mod spawn_tests {
    use naque_llm::{AgentEvent, LlmResponse, Usage as LlmUsage};

    use super::*;

    #[tokio::test]
    async fn spawned_turn_streams_events_and_finalizes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let resp = LlmResponse {
            text: Some("hello".into()),
            tool_calls: vec![],
            usage: LlmUsage {
                input_tokens: 4,
                output_tokens: 2,
            },
            stop_reason: "end_turn".into(),
        };
        let mut app = tests::make_app(&url, PermissionMode::Wildcard, vec![resp]).await;

        app.start_turn("hi");
        assert!(app.is_turn_running());

        let mut saw_finished = false;
        loop {
            if let Some(ev) = app.next_event().await {
                if matches!(ev, AgentEvent::TurnFinished { .. }) {
                    saw_finished = true;
                }
                app.apply_event(&ev);
            } else if app.poll_finished() {
                break;
            }
        }
        assert!(saw_finished);
        app.finalize_turn().await;

        assert!(!app.is_turn_running());
        assert!(
            app.transcript()
                .iter()
                .any(|e| matches!(e, TranscriptEntry::Agent(s) if s == "hello"))
        );
        assert!(app.usage().input_tokens >= 4);
        assert!(app.agent_slot.is_some());
    }

    #[tokio::test]
    async fn start_turn_is_noop_without_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = tests::make_app(&url, PermissionMode::Wildcard, vec![]).await;
        app.agent_slot = None; // simulate an agent lost to a panicked task
        assert!(!app.can_start_turn());
        app.start_turn("hi"); // must NOT panic
        assert!(!app.is_turn_running());
        assert!(app.transcript().iter().any(|e| matches!(e, TranscriptEntry::Error(_))));
    }
}
