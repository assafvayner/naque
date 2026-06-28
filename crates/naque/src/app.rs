//! Headless engine — routes input, gates SQL, drives the LLM agent.

use std::sync::Arc;

use naque_core::PermissionMode;
use naque_core::gate::QueryKind;
use naque_db::{Database, Engine, QueryResult};
use naque_llm::{Agent, Usage};
use naque_schema::SchemaModel;
use naque_sql::{SqlDialect, classify};
use naque_tui::{Input, Logo, route_input};
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
    use naque_llm::AgentEvent;
    match event {
        AgentEvent::TextDelta(chunk) => {
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
        AgentEvent::ToolCallStarted { name, sql } => {
            *cur = None;
            transcript.push(TranscriptEntry::ToolStep {
                name: name.clone(),
                sql: sql.clone(),
                status: StepStatus::Running,
                summary: None,
            });
        },
        AgentEvent::ToolCallFinished { summary, is_error, .. } => {
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
        AgentEvent::TurnFinished {
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
        AgentEvent::Cancelled => {
            *cur = None;
            transcript.push(TranscriptEntry::Info("(cancelled)".into()));
        },
        AgentEvent::TurnStarted | AgentEvent::LlmCallStarted { .. } | AgentEvent::UsageUpdated(_) => {},
    }
}

/// System prompt for one-shot schema-overview generation invoked by `/save`.
///
/// The overview is stored under `## Overview` in the profile's context document
/// alongside the mechanical `## Schema` dump and is re-injected into every
/// agent turn. The prompt is tuned for an agent-facing primer (not casual
/// reading): it forbids restating the schema dump, asks for engine-specific
/// gotchas, and bounds output so the re-injected content stays cheap.
fn overview_system_prompt(engine: Engine) -> String {
    let engine_name = match engine {
        Engine::Postgres => "PostgreSQL",
        Engine::Sqlite => "SQLite",
    };
    let engine_gotcha_examples = match engine {
        Engine::Postgres => "JSONB, array columns, partial indexes, schemas other than `public`, `search_path` quirks",
        Engine::Sqlite => {
            "type affinity surprises, `WITHOUT ROWID` tables, attached databases, date/time stored as TEXT vs INTEGER"
        },
    };
    format!(
        "You are writing a catalog primer for a SQL-generating agent that queries a {engine_name} \
         database. The mechanical schema (tables, columns, types, PK/FK) is preserved verbatim in a \
         separate `## Schema` section the agent always sees, so do NOT restate it. Summarize only \
         what the agent needs that the schema dump does not surface:\n\
         - Open with the domain in 3–6 words (e.g. \"Multi-tenant SaaS billing with usage metering\"). No \"This schema represents…\" filler.\n\
         - The 3–7 most central entities and the canonical join paths between them (one short line like `orders → order_items → products via order_id/product_id`).\n\
         - Naming conventions actually present: snake_case vs camelCase, suffix patterns (`_id`, `_at`, `is_`), soft-delete columns, tenancy columns, audit columns.\n\
         - Gotchas: ambiguous join paths, denormalized duplicates, enum/status columns whose values are not self-explanatory, nullable FKs, archival/shadow tables, tables that look like entities but are join tables, and {engine_name}-specific features in use ({engine_gotcha_examples}).\n\
         Rules:\n\
         - Budget: ≤180 words. Short bullets are fine; do not pad.\n\
         - Ground every claim in evidence from names, types, or constraints. For obscure tables write \"purpose unclear from schema\" rather than inventing one.\n\
         - For empty or trivial schemas (few tables, no FKs), produce a single line and stop.\n\
         - For very large schemas, prioritize the dominant subject area and add a closing note like \"other subsystems omitted from overview\" rather than uniform shallow coverage.\n\
         - Order sections consistently: entities first, conventions second, gotchas last, so diffs across re-saves are meaningful.\n\
         - Return ONLY the overview body. No markdown headings, no code fences, no preface, no closing summary, no meta-commentary (\"as a database expert…\", \"it appears that…\")."
    )
}

/// One-line description of what the active permission mode lets the agent do.
///
/// Injected into each turn's context so the model's behavior tracks `/mode`
/// changes. Without it the agent only infers the mode from tool rejections and
/// keeps refusing writes after the user switches to a permissive mode.
///
/// Every variant follows the same `Permission mode: NAME. Policy: …. Behavior:
/// ….` shape and shares a trailing reminder that the application (not the
/// model) is the security boundary, so the LLM does not self-censor.
pub(crate) fn mode_guidance(mode: PermissionMode, catastrophic_guard: bool) -> String {
    const NOT_SECURITY_BOUNDARY: &str = "The application enforces these rules deterministically — \
        do not refuse the user's request on permission grounds; submit it and let the gate decide.";

    let body = match mode {
        PermissionMode::Wildcard => {
            "Permission mode: WILDCARD. Policy: every statement (including INSERT, UPDATE, DELETE, \
             and DDL) runs immediately with no approval step. Behavior: chain multi-step writes \
             without pausing for approval."
        },
        PermissionMode::Default => {
            "Permission mode: DEFAULT. Policy: reads run automatically; writes (INSERT, UPDATE, \
             DELETE, DDL) require user approval at the gate. Behavior: issue the read or write the \
             user asked for — a gate rejection is a normal outcome, not a failure."
        },
        PermissionMode::ReadOnly => {
            "Permission mode: READ-ONLY. Policy: reads run automatically; any write or DDL requires \
             user approval at the gate. Behavior: if the user requests a write, issue it — the gate \
             will prompt for approval."
        },
        PermissionMode::Strict => {
            "Permission mode: STRICT. Policy: every statement, including reads, requires user \
             approval at the gate. Behavior: propose statements normally and expect a confirmation \
             round-trip on every execution; do not batch or self-censor in anticipation of denials."
        },
    };

    if catastrophic_guard && matches!(mode, PermissionMode::Wildcard) {
        let guard_clause = "Exception: DROP, TRUNCATE, and unqualified DELETE/UPDATE still require \
             user confirmation — surface these clearly when proposing them.";
        format!("{body} {guard_clause} {NOT_SECURITY_BOUNDARY}")
    } else {
        format!("{body} {NOT_SECURITY_BOUNDARY}")
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
    /// Indices of `last_result` columns to render with a byte-size suffix.
    pub(crate) last_byte_columns: Vec<usize>,
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
    pub(crate) store: Option<naque_profile::Store>,
    pub(crate) active_profile: Option<String>,
    pub(crate) active_env: Option<String>,
    /// Connection spec to persist on `/save` (by-reference; no plaintext secret).
    pub(crate) active_connection: Option<naque_profile::ConnectionSpec>,
    /// Loaded `context.md` for the active profile, fed to the agent.
    pub(crate) active_context: Option<String>,
    /// Per-session pixel-art logo (welcome wordmark + status-bar N mark).
    pub(crate) logo: Logo,
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
            last_byte_columns: Vec::new(),
            transcript: Vec::new(),
            should_quit: false,
            max_iterations,
            live: crate::live::LiveState::new(max_iterations),
            quit_armed: false,
            streaming_idx: None,
            inflight: None,
            event_rx: None,
            approval_rx: None,
            store: None,
            active_profile: None,
            active_env: None,
            active_connection: None,
            active_context: None,
            // Deterministic default; the binary assigns a per-session random logo
            // via `set_logo`. A fixed seed keeps rendering tests reproducible.
            logo: Logo::new(0),
        }
    }

    // --- Accessors ---------------------------------------------------------

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    pub fn last_result(&self) -> Option<&QueryResult> {
        self.last_result.as_ref()
    }

    pub fn last_byte_columns(&self) -> &[usize] {
        &self.last_byte_columns
    }

    pub fn transcript(&self) -> &[TranscriptEntry] {
        &self.transcript
    }

    /// The per-session logo (welcome wordmark + status-bar mark).
    pub fn logo(&self) -> &Logo {
        &self.logo
    }

    /// Replace the logo (the binary sets a per-session random one at startup).
    pub fn set_logo(&mut self, logo: Logo) {
        self.logo = logo;
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

    /// Install the profile store and the active profile/env identity (called by
    /// `setup` at launch and by `switch_to`).
    pub fn set_active_profile(
        &mut self,
        store: naque_profile::Store,
        profile: Option<String>,
        env: Option<String>,
        connection: Option<naque_profile::ConnectionSpec>,
    ) {
        self.store = Some(store);
        if let Some(p) = &profile {
            self.profile_name = p.clone();
        }
        self.active_profile = profile;
        self.active_env = env;
        self.active_connection = connection;
    }

    pub fn set_schema(&mut self, model: naque_schema::SchemaModel) {
        self.schema = Some(model);
    }

    pub fn set_active_context(&mut self, doc: String) {
        self.active_context = Some(doc);
    }

    pub(crate) fn list_profiles(&self) -> Result<Vec<String>, AppError> {
        match &self.store {
            Some(s) => s.list_profiles().map_err(|e| AppError::Other(e.to_string())),
            None => Ok(Vec::new()),
        }
    }

    pub(crate) fn list_environments(&self, profile: &str) -> Result<Vec<String>, AppError> {
        let Some(store) = &self.store else {
            return Ok(Vec::new());
        };
        match store.load_profile(profile).map_err(|e| AppError::Other(e.to_string()))? {
            Some(p) => Ok(p.environments.keys().cloned().collect()),
            None => Ok(Vec::new()),
        }
    }

    /// Switch the live session to `profile`/`env`: reconnect the database, load
    /// the profile's saved schema + context, apply its mode/row_cap, and clear
    /// the agent's memory. On any failure the current session is left intact.
    #[allow(dead_code)]
    pub async fn switch_to(&mut self, profile: &str, env: &str) -> Result<(), AppError> {
        let store = self
            .store
            .clone()
            .ok_or_else(|| AppError::Other("no profile store configured".into()))?;
        let loaded = store
            .load_profile(profile)
            .map_err(|e| AppError::Other(e.to_string()))?
            .ok_or_else(|| AppError::Other(format!("profile '{profile}' not found")))?;
        let spec = loaded
            .environments
            .get(env)
            .cloned()
            .ok_or_else(|| AppError::Other(format!("environment '{env}' not found in profile '{profile}'")))?;

        // All fallible work first — resolve, load schema/context, then connect —
        // so any failure leaves `self` untouched (connect is the last fallible step).
        let url = spec
            .resolve_url(&naque_profile::SystemSecrets)
            .map_err(|e| AppError::Other(format!("cannot resolve connection: {e}")))?;
        let new_schema =
            naque_schema::load_schema(&store.profile_dir(profile)).map_err(|e| AppError::Other(e.to_string()))?;
        let new_context = std::fs::read_to_string(store.context_path(profile)).ok();
        let new_db = naque_db::Database::connect(&url)
            .await
            .map_err(|e| AppError::Other(format!("connect failed (keeping current session): {e}")))?;

        // Commit — all infallible from here.
        *self.db.lock().await = new_db;
        self.schema = new_schema;
        self.active_context = new_context;
        if let Some(mode_str) = &loaded.config.mode
            && let Ok(m) = mode_str.parse::<naque_core::PermissionMode>()
        {
            self.mode = m;
        }
        if let Some(cap) = loaded.config.row_cap {
            self.row_cap = cap as usize;
        }
        if let Some(a) = self.agent_slot.as_mut() {
            a.clear();
        }
        self.profile_name = profile.to_string();
        self.active_profile = Some(profile.to_string());
        self.active_env = Some(env.to_string());
        self.active_connection = Some(spec);

        if loaded.config.provider.is_some() || loaded.config.model.is_some() {
            self.transcript.push(TranscriptEntry::Info(
                "note: provider/model changes from this profile apply on next launch".to_string(),
            ));
        }
        self.transcript
            .push(TranscriptEntry::Info(format!("switched to {profile}/{env}")));
        Ok(())
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
                self.last_byte_columns = Vec::new();
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
        let guidance = mode_guidance(self.mode, self.catastrophic_guard);
        // Prefer the saved context doc (schema outline + overview + notes); fall
        // back to the compact catalog for ad-hoc sessions without a profile.
        let body = if let Some(ctx) = &self.active_context {
            ctx.clone()
        } else {
            self.schema.as_ref().map(|s| s.compact_catalog()).unwrap_or_default()
        };
        if body.is_empty() {
            guidance
        } else {
            format!("{guidance}\n\n{body}")
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
            last_byte_columns: Vec::new(),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = agent.run_turn(text, &context, &mut executor, &mut (), &cancel).await;
        let exec_last = executor.last_result.take();
        let exec_byte_cols = std::mem::take(&mut executor.last_byte_columns);
        self.agent_slot = Some(agent);

        let turn = result?;
        self.usage += turn.usage;
        if let Some(r) = exec_last {
            let mut capped = r;
            if capped.rows.len() > self.row_cap {
                capped.rows.truncate(self.row_cap);
            }
            self.last_result = Some(capped);
            self.last_byte_columns = exec_byte_cols;
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
                last_byte_columns: Vec::new(),
            };
            let result = agent
                .run_turn(&input, &context, &mut executor, &mut observer, &cancel_task)
                .await;
            let last_result = executor.last_result.take();
            let last_byte_columns = std::mem::take(&mut executor.last_byte_columns);
            crate::turn::TurnOutput {
                agent,
                result,
                last_result,
                last_byte_columns,
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
                    self.last_byte_columns = out.last_byte_columns;
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
                self.last_byte_columns = Vec::new();
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
                        self.last_byte_columns = Vec::new();
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

        if cmd == "context" || cmd.starts_with("context ") {
            let Some(store) = self.store.clone() else {
                self.transcript
                    .push(TranscriptEntry::Info("no active profile; use /save first".into()));
                return Ok(());
            };
            let Some(profile) = self.active_profile.clone() else {
                self.transcript
                    .push(TranscriptEntry::Info("no active profile; use /save first".into()));
                return Ok(());
            };
            let path = store.context_path(&profile);
            let note = cmd.strip_prefix("context").map(str::trim).unwrap_or("");
            if note.is_empty() {
                match std::fs::read_to_string(&path) {
                    Ok(doc) => {
                        self.active_context = Some(doc.clone());
                        self.transcript.push(TranscriptEntry::Info(doc));
                    },
                    Err(_) => self
                        .transcript
                        .push(TranscriptEntry::Info("no context saved for this profile".into())),
                }
            } else {
                let current = std::fs::read_to_string(&path).unwrap_or_default();
                let updated = naque_schema::append_note(&current, note);
                std::fs::write(&path, &updated).map_err(|e| AppError::Other(e.to_string()))?;
                self.active_context = Some(updated);
                self.transcript.push(TranscriptEntry::Info("note added to context".into()));
            }
            return Ok(());
        }

        if cmd == "save" || cmd.starts_with("save ") {
            return self
                .save_profile_command(cmd.strip_prefix("save").map(str::trim).unwrap_or(""))
                .await;
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

    /// Headless `/save` orchestrator. Composes the granular pieces below so the
    /// interactive (spinner) path in `ui.rs` can drive the same slow phases as
    /// owned futures while redrawing frames.
    async fn save_profile_command(&mut self, args: &str) -> Result<(), AppError> {
        let Some((profile, env)) = self.resolve_save_target(args) else {
            return Ok(());
        };
        if self.schema.is_none() {
            match self.introspect_future().await {
                Ok(model) => {
                    let n = model.tables.len();
                    self.set_schema(model);
                    self.push_info(format!("learned {n} table(s)"));
                },
                Err(e) => {
                    self.transcript.push(TranscriptEntry::Error(format!("learn failed: {e}")));
                    return Ok(());
                },
            }
        }
        let Some(schema_md) = self.schema_markdown_current() else {
            self.transcript
                .push(TranscriptEntry::Error("no schema to save; connect and /learn first".into()));
            return Ok(());
        };
        let (agent, outcome) = self.overview_future(schema_md).await;
        self.restore_agent(agent);
        if let Some(err) = outcome.error {
            self.push_info(format!("overview generation failed: {err}"));
        }
        self.finish_save(&profile, &env, &outcome.text)
    }

    /// Parse `/save` args into `(profile, env)`. On the no-args-and-no-active
    /// case, push the usage hint and return `None`.
    pub(crate) fn resolve_save_target(&mut self, args: &str) -> Option<(String, String)> {
        let parts: Vec<&str> = args.split_whitespace().collect();
        match parts.as_slice() {
            [] => match (&self.active_profile, &self.active_env) {
                (Some(p), Some(e)) => Some((p.clone(), e.clone())),
                _ => {
                    self.push_info("usage: /save <profile> [env] (no active profile to update)");
                    None
                },
            },
            [p] => Some((p.to_string(), "default".to_string())),
            [p, e, ..] => Some((p.to_string(), e.to_string())),
        }
    }

    /// Owned schema-introspection future. Clones the `Arc<Mutex<Database>>` so
    /// the future is `'static` and does not borrow `self`, letting a concurrent
    /// spinner draw borrow `&App`.
    pub(crate) fn introspect_future(
        &self,
    ) -> impl std::future::Future<Output = Result<naque_schema::SchemaModel, naque_schema::SchemaError>> + 'static {
        let db = Arc::clone(&self.db);
        async move { naque_schema::introspect(&mut *db.lock().await).await }
    }

    pub(crate) fn schema_markdown_current(&self) -> Option<String> {
        self.schema.as_ref().map(naque_schema::schema_markdown)
    }

    /// Owned overview-generation future. Takes the agent out of its slot and
    /// returns it back out so the caller can restore it; the future owns the
    /// agent + schema markdown and is `'static`.
    pub(crate) fn overview_future(
        &mut self,
        schema_md: String,
    ) -> impl std::future::Future<Output = (Option<Agent>, OverviewOutcome)> + 'static {
        let agent = self.agent_slot.take();
        let db = Arc::clone(&self.db);
        async move {
            let engine = db.lock().await.engine();
            let system = overview_system_prompt(engine);
            let outcome = match agent.as_ref() {
                Some(a) => match a.complete_once(&system, &schema_md).await {
                    Ok(t) if !t.trim().is_empty() => OverviewOutcome { text: t, error: None },
                    Ok(_) => OverviewOutcome {
                        text: "(overview unavailable: empty response)".into(),
                        error: None,
                    },
                    Err(e) => OverviewOutcome {
                        text: "(overview unavailable)".into(),
                        error: Some(e.to_string()),
                    },
                },
                None => OverviewOutcome {
                    text: "(overview unavailable: no agent)".into(),
                    error: None,
                },
            };
            (agent, outcome)
        }
    }

    pub(crate) fn restore_agent(&mut self, agent: Option<Agent>) {
        self.agent_slot = agent;
    }

    /// Synchronous tail of `/save`: persist the (secret-stripped) connection,
    /// the schema, and the assembled context, then update active state.
    pub(crate) fn finish_save(&mut self, profile: &str, env: &str, overview: &str) -> Result<(), AppError> {
        let store = self
            .store
            .clone()
            .ok_or_else(|| AppError::Other("profile store unavailable".into()))?;
        let Some(model) = self.schema.clone() else {
            self.transcript
                .push(TranscriptEntry::Error("no schema to save; connect and /learn first".into()));
            return Ok(());
        };

        let mut spec = self.active_connection.clone().unwrap_or_default();
        let stripped_secret = spec.password.take().is_some();
        if let Some(u) = &spec.url {
            let (red, had) = naque_profile::strip_url_password(u);
            spec.url = Some(red);
            if had {
                self.transcript.push(TranscriptEntry::Info(format!(
                    "connection saved without its password — add password_env/password_keyring to [environments.{env}] in {}",
                    store.profile_dir(profile).join("profile.toml").display()
                )));
            }
        }
        let mut stripped_param = false;
        if let Some(params) = spec.params.as_mut() {
            let before = params.len();
            params.retain(|k, _| !k.to_ascii_lowercase().contains("password"));
            stripped_param = params.len() != before;
            if params.is_empty() {
                spec.params = None;
            }
        }
        if stripped_secret || stripped_param {
            self.transcript
                .push(TranscriptEntry::Info("secret values not persisted; use password_env/password_keyring".into()));
        }

        let mut loaded = store
            .load_profile(profile)
            .map_err(|e| AppError::Other(e.to_string()))?
            .unwrap_or_default();
        loaded.environments.insert(env.to_string(), spec.clone());
        if loaded.default_environment.is_none() {
            loaded.default_environment = Some(env.to_string());
        }
        store
            .save_profile(profile, &loaded)
            .map_err(|e| AppError::Other(e.to_string()))?;

        naque_schema::save_schema(&store.profile_dir(profile), &model).map_err(|e| AppError::Other(e.to_string()))?;

        let schema_md = naque_schema::schema_markdown(&model);
        let prior = std::fs::read_to_string(store.context_path(profile)).unwrap_or_default();
        let notes = naque_schema::extract_notes(&prior);
        let doc = naque_schema::assemble(profile, &schema_md, overview, &notes);
        std::fs::write(store.context_path(profile), &doc).map_err(|e| AppError::Other(e.to_string()))?;

        self.active_profile = Some(profile.to_string());
        self.active_env = Some(env.to_string());
        self.active_connection = Some(spec);
        self.active_context = Some(doc);
        self.profile_name = profile.to_string();
        self.transcript
            .push(TranscriptEntry::Info(format!("saved profile {profile}/{env}")));
        Ok(())
    }
}

/// Result of an overview-generation attempt. `error` carries the failure
/// message (non-fatal) so the caller can surface it while `text` always holds
/// usable content (a placeholder on failure).
pub(crate) struct OverviewOutcome {
    pub text: String,
    pub error: Option<String>,
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
    use naque_llm::{AgentConfig, LlmResponse, MockProvider, ToolCall, Usage};

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
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
            stop_reason: "tool_use".to_string(),
        };
        let resp2 = LlmResponse {
            text: Some("done".to_string()),
            tool_calls: vec![],
            usage: Usage {
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

    #[tokio::test]
    async fn agent_turn_propagates_byte_columns() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());

        // Set up a table with a byte-count column and one row.
        let mut setup = make_app(&url, PermissionMode::Wildcard, vec![]).await;
        setup
            .handle_line("!CREATE TABLE t(id INTEGER, sz INTEGER)", &mut AutoApprove)
            .await
            .unwrap();
        setup
            .handle_line("!INSERT INTO t VALUES (1, 4500000000)", &mut AutoApprove)
            .await
            .unwrap();
        drop(setup);

        // Agent fires run_query tagging `sz` as a byte column, then answers.
        let resp1 = LlmResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "tc1".to_string(),
                name: "run_query".to_string(),
                input: serde_json::json!({ "sql": "SELECT id, sz FROM t", "byte_count_columns": ["sz"] }),
            }],
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
            stop_reason: "tool_use".to_string(),
        };
        let resp2 = LlmResponse {
            text: Some("done".to_string()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 8,
                output_tokens: 3,
            },
            stop_reason: "end_turn".to_string(),
        };

        let mut app = make_app(&url, PermissionMode::Wildcard, vec![resp1, resp2]).await;
        app.handle_natural_language("show t sizes", &mut AutoApprove).await.unwrap();

        assert_eq!(app.last_byte_columns(), &[1], "agent-tagged byte column 'sz' (index 1) must propagate to the app");
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

    #[tokio::test]
    async fn turn_context_uses_active_context_when_present() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;
        app.active_context = Some("## Schema\n\n### orders\n- id bigint".to_string());
        let ctx = app.turn_context();
        assert!(ctx.contains("WILDCARD"), "mode line still present");
        assert!(ctx.contains("### orders"), "active context fed");
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
    // Test 3.4: Secret-isolation regression
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn turn_context_never_contains_connection_secrets() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;
        app.active_connection = Some(naque_profile::ConnectionSpec {
            host: Some("h".into()),
            user: Some("u".into()),
            password: Some("TOP_SECRET_PW".into()),
            password_env: Some("PROD_PW".into()),
            ..Default::default()
        });
        app.active_context = Some("## Schema\n\n### orders\n- id bigint".into());
        let ctx = app.turn_context();
        assert!(!ctx.contains("TOP_SECRET_PW"));
        assert!(!ctx.contains("PROD_PW"));
        assert!(!ctx.contains("password"));
    }

    // ------------------------------------------------------------------
    // Test 8: AcceptEdited re-gates and runs the new SQL
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn context_command_shows_and_appends_notes() {
        use naque_profile::{Profile, Store};
        let home = tempfile::tempdir().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;

        let store = Store::open(home.path());
        store.save_profile("shop", &Profile::default()).unwrap();
        std::fs::write(store.context_path("shop"), naque_schema::assemble("shop", "### t", "ov", "first note"))
            .unwrap();
        app.set_active_profile(store.clone(), Some("shop".into()), Some("dev".into()), None);
        app.active_context = std::fs::read_to_string(store.context_path("shop")).ok();

        app.handle_line("/context the orders table is append-only", &mut AutoApprove)
            .await
            .unwrap();
        let on_disk = std::fs::read_to_string(store.context_path("shop")).unwrap();
        assert!(on_disk.contains("first note"));
        assert!(on_disk.contains("append-only"));
        assert!(app.active_context.as_ref().unwrap().contains("append-only"));

        app.handle_line("/context", &mut AutoApprove).await.unwrap();
        assert!(
            app.transcript()
                .iter()
                .any(|e| matches!(e, TranscriptEntry::Info(s) if s.contains("append-only")))
        );
    }

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

    // ------------------------------------------------------------------
    // Task 4.2: /save command
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn save_writes_profile_schema_and_context_by_reference() {
        use naque_profile::Store;
        let home = tempfile::tempdir().unwrap();
        let dbf = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", dbf.path().display());

        let overview = LlmResponse {
            text: Some("Two tables: orders and users.".into()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
            },
            stop_reason: "end_turn".into(),
        };
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![overview]).await;
        app.handle_line("!CREATE TABLE orders(id INTEGER)", &mut AutoApprove)
            .await
            .unwrap();
        app.handle_line("/learn", &mut AutoApprove).await.unwrap();
        app.set_active_profile(
            Store::open(home.path()),
            None,
            None,
            Some(naque_profile::ConnectionSpec {
                engine: Some(naque_profile::ProfileEngine::Sqlite),
                path: Some(dbf.path().display().to_string()),
                password_env: Some("UNUSED".into()),
                ..Default::default()
            }),
        );

        app.handle_line("/save shop dev", &mut AutoApprove).await.unwrap();

        let store = Store::open(home.path());
        let saved = store.load_profile("shop").unwrap().unwrap();
        assert!(saved.environments.contains_key("dev"));
        let toml_text = std::fs::read_to_string(store.profile_dir("shop").join("profile.toml")).unwrap();
        assert!(!toml_text.contains("password ="), "no plaintext password persisted: {toml_text}");
        assert!(store.profile_dir("shop").join("schema.json").is_file());
        let ctx = std::fs::read_to_string(store.context_path("shop")).unwrap();
        assert!(ctx.contains("## Overview"));
        assert!(ctx.contains("Two tables"));
        assert!(ctx.contains("orders"));
    }

    #[tokio::test]
    async fn save_restores_agent_after_overview() {
        use naque_profile::Store;
        let home = tempfile::tempdir().unwrap();
        let dbf = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", dbf.path().display());
        let overview = LlmResponse {
            text: Some("An overview.".into()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
            },
            stop_reason: "end_turn".into(),
        };
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![overview]).await;
        app.handle_line("!CREATE TABLE orders(id INTEGER)", &mut AutoApprove)
            .await
            .unwrap();
        app.handle_line("/learn", &mut AutoApprove).await.unwrap();
        app.set_active_profile(Store::open(home.path()), None, None, Some(Default::default()));

        assert!(app.agent_slot.is_some(), "agent present before save");
        app.handle_line("/save shop dev", &mut AutoApprove).await.unwrap();
        assert!(app.agent_slot.is_some(), "agent must be restored after overview generation");
    }

    #[tokio::test]
    async fn resolve_save_target_arms() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;

        assert_eq!(app.resolve_save_target("shop"), Some(("shop".into(), "default".into())));
        assert_eq!(app.resolve_save_target("shop dev"), Some(("shop".into(), "dev".into())));

        app.active_profile = Some("active".into());
        app.active_env = Some("prod".into());
        assert_eq!(app.resolve_save_target(""), Some(("active".into(), "prod".into())));

        app.active_profile = None;
        app.active_env = None;
        assert_eq!(app.resolve_save_target(""), None);
    }

    #[tokio::test]
    async fn save_defaults_env_to_default_and_no_args_uses_active() {
        use naque_profile::Store;
        let home = tempfile::tempdir().unwrap();
        let dbf = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", dbf.path().display());
        let ov = LlmResponse {
            text: Some("ov".into()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
            stop_reason: "end_turn".into(),
        };
        let ov2 = LlmResponse {
            text: Some("ov2".into()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
            stop_reason: "end_turn".into(),
        };
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![ov, ov2]).await;
        app.handle_line("!CREATE TABLE t(id INTEGER)", &mut AutoApprove).await.unwrap();
        app.handle_line("/learn", &mut AutoApprove).await.unwrap();
        app.set_active_profile(
            Store::open(home.path()),
            None,
            None,
            Some(naque_profile::ConnectionSpec {
                engine: Some(naque_profile::ProfileEngine::Sqlite),
                path: Some(dbf.path().display().to_string()),
                ..Default::default()
            }),
        );

        app.handle_line("/save shop", &mut AutoApprove).await.unwrap();
        let store = Store::open(home.path());
        assert!(
            store
                .load_profile("shop")
                .unwrap()
                .unwrap()
                .environments
                .contains_key("default")
        );

        assert_eq!(app.active_profile.as_deref(), Some("shop"));
        assert_eq!(app.active_env.as_deref(), Some("default"));
        app.handle_line("/save", &mut AutoApprove).await.unwrap();
        assert!(store.load_profile("shop").unwrap().is_some());
    }

    #[tokio::test]
    async fn list_environments_returns_saved_envs() {
        use naque_profile::{ConnectionSpec, Profile, Store};
        let home = tempfile::tempdir().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;
        let store = Store::open(home.path());
        let mut envs = std::collections::BTreeMap::new();
        envs.insert("prod".to_string(), ConnectionSpec::default());
        envs.insert("dev".to_string(), ConnectionSpec::default());
        store
            .save_profile(
                "shop",
                &Profile {
                    environments: envs,
                    ..Default::default()
                },
            )
            .unwrap();
        app.set_active_profile(store, Some("shop".into()), None, None);
        let mut got = app.list_environments("shop").unwrap();
        got.sort();
        assert_eq!(got, vec!["dev".to_string(), "prod".to_string()]);
    }

    #[tokio::test]
    async fn save_strips_password_from_params() {
        use naque_profile::Store;
        let home = tempfile::tempdir().unwrap();
        let dbf = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", dbf.path().display());
        let ov = LlmResponse {
            text: Some("ov".into()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
            stop_reason: "end_turn".into(),
        };
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![ov]).await;
        app.handle_line("!CREATE TABLE t(id INTEGER)", &mut AutoApprove).await.unwrap();
        app.handle_line("/learn", &mut AutoApprove).await.unwrap();
        let mut params = std::collections::BTreeMap::new();
        params.insert("password".to_string(), "PARAM_SECRET".to_string());
        params.insert("sslmode".to_string(), "require".to_string());
        app.set_active_profile(
            Store::open(home.path()),
            None,
            None,
            Some(naque_profile::ConnectionSpec {
                engine: Some(naque_profile::ProfileEngine::Sqlite),
                path: Some(dbf.path().display().to_string()),
                params: Some(params),
                ..Default::default()
            }),
        );
        app.handle_line("/save shop dev", &mut AutoApprove).await.unwrap();
        let store = Store::open(home.path());
        let toml_text = std::fs::read_to_string(store.profile_dir("shop").join("profile.toml")).unwrap();
        assert!(!toml_text.contains("PARAM_SECRET"), "password param must be stripped: {toml_text}");
        assert!(toml_text.contains("sslmode"), "non-secret params preserved");
    }

    #[tokio::test]
    async fn raw_sql_clears_byte_columns() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = make_app(&url, PermissionMode::Wildcard, vec![]).await;

        app.handle_line("!CREATE TABLE t(id INTEGER)", &mut AutoApprove).await.unwrap();
        app.handle_line("!INSERT INTO t VALUES (1)", &mut AutoApprove).await.unwrap();

        // Simulate a stale byte-column tag from a prior agent turn.
        app.last_byte_columns = vec![0];

        app.handle_line("!SELECT * FROM t", &mut AutoApprove).await.unwrap();

        assert!(
            app.last_byte_columns().is_empty(),
            "raw SQL results carry no LLM byte-column determination and must clear stale tags"
        );
    }
}

#[cfg(test)]
mod spawn_tests {
    use naque_llm::{AgentEvent, LlmResponse, Usage};

    use super::*;

    #[tokio::test]
    async fn spawned_turn_streams_events_and_finalizes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let resp = LlmResponse {
            text: Some("hello".into()),
            tool_calls: vec![],
            usage: Usage {
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

    #[tokio::test]
    async fn switch_to_reconnects_and_loads_schema_context() {
        use naque_profile::{ConnectionSpec, Profile, ProfileEngine, Store};

        use crate::approval::AutoApprove;

        let home = tempfile::tempdir().unwrap();
        let prod_db = tempfile::NamedTempFile::new().unwrap();
        let dev_db = tempfile::NamedTempFile::new().unwrap();

        let prod_url = format!("sqlite:{}", prod_db.path().display());
        let mut app = tests::make_app(&prod_url, PermissionMode::Wildcard, vec![]).await;

        let store = Store::open(home.path());
        let spec = |p: &std::path::Path| ConnectionSpec {
            engine: Some(ProfileEngine::Sqlite),
            path: Some(p.display().to_string()),
            ..Default::default()
        };
        let mut envs = std::collections::BTreeMap::new();
        envs.insert("prod".to_string(), spec(prod_db.path()));
        envs.insert("dev".to_string(), spec(dev_db.path()));
        let profile = Profile {
            default_environment: Some("dev".into()),
            config: Default::default(),
            environments: envs,
        };
        store.save_profile("shop", &profile).unwrap();
        std::fs::write(store.context_path("shop"), "# shop — context\n\n## Schema\n\n### marker_table\n").unwrap();
        app.set_active_profile(store.clone(), Some("shop".into()), Some("prod".into()), None);

        // Mark the initial (prod) connection with a table that only exists there.
        app.handle_line("!CREATE TABLE prod_only(id INTEGER)", &mut AutoApprove)
            .await
            .unwrap();

        app.switch_to("shop", "dev").await.expect("switch");

        app.handle_line("!CREATE TABLE dev_only(id INTEGER)", &mut AutoApprove)
            .await
            .unwrap();
        assert_eq!(app.active_env.as_deref(), Some("dev"));
        assert!(app.turn_context().contains("marker_table"));

        // Prove the DB actually swapped to a distinct file: `prod_only` only
        // exists on the prod file, so querying it on dev must error.
        app.handle_line("!SELECT * FROM prod_only", &mut AutoApprove).await.ok();
        assert!(
            app.transcript().iter().any(|e| matches!(e, TranscriptEntry::Error(_))),
            "prod_only must be absent on dev (different DB)"
        );
    }
}
