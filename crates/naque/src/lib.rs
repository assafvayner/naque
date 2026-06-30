//! The headless `naque` engine: approval gate, connection routing, LLM agent.

pub mod app;
pub mod approval;
pub mod executor;
pub mod fs_access;
pub mod live;
pub mod turn;
pub mod ui;
pub mod web;

pub use app::{App, AppError, TranscriptEntry};
pub use approval::{
    ApprovalDecision, ApprovalRequest, Approver, AutoApprove, AutoReject, ChannelApprover, PathApprovalRequest,
    PathGrant, ScriptedApprover,
};
pub use fs_access::{FsAccess, PathAuth};
pub use live::LiveState;
use naque_core::PermissionMode;
use naque_core::gate::{GateDecision, QueryKind, gate_decision};
use naque_db::{Database, Engine, QueryResult};
use naque_sql::{SqlDialect, classify};

/// Classify `sql`, gate it, prompt if needed, and route to the correct
/// connection (`fetch`/`fetch_readonly`/`execute`).
///
/// Returns `Ok(QueryResult)` on success.
/// Returns `Err("rejected")` when the user rejected the query.
/// Returns `Err(<db error message>)` on execution failure.
///
/// This function is shared between `App::execute_sql` and
/// `QueryToolExecutor::run_query` to avoid duplicating classify→gate→approve→route logic.
pub async fn run_gated(
    db: &mut Database,
    mode: PermissionMode,
    catastrophic_guard: bool,
    sql: &str,
    kind: QueryKind,
    approver: &mut dyn Approver,
) -> Result<QueryResult, String> {
    let dialect = match db.engine() {
        Engine::Postgres => SqlDialect::Postgres,
        Engine::Sqlite => SqlDialect::Sqlite,
    };

    let class = classify(sql, dialect);
    let decision = gate_decision(mode, &class, kind, catastrophic_guard);

    // Resolve the actual SQL to run (may be replaced by AcceptEdited).
    let final_sql = match decision {
        GateDecision::AutoApprove => sql.to_string(),
        GateDecision::Prompt | GateDecision::PromptCatastrophic => {
            let label = class
                .statements
                .first()
                .map(|s| s.label.clone())
                .unwrap_or_else(|| "SQL".to_string());

            match approver.approve(sql, &label, decision).await {
                ApprovalDecision::Accept => sql.to_string(),
                ApprovalDecision::AcceptEdited(new_sql) => {
                    // Recurse: re-classify and re-gate the edited SQL.
                    // This is a tail recursion via Box::pin to avoid stack overflow on deep edits.
                    return Box::pin(run_gated(db, mode, catastrophic_guard, &new_sql, kind, approver)).await;
                },
                ApprovalDecision::Reject => return Err("rejected".to_string()),
            }
        },
    };

    // Re-classify the final SQL for routing (needed after AcceptEdited too, but
    // for the non-edited path we already have `class`; re-classify unconditionally
    // to be safe after the string may have changed).
    let route_class = classify(&final_sql, dialect);
    let read_only = route_class.is_read_only();

    if kind == QueryKind::Introspection || (mode == PermissionMode::ReadOnly && read_only) {
        // DB-level read-only connection.
        db.fetch_readonly(&final_sql).await.map_err(|e| e.to_string())
    } else if read_only {
        // Read on primary connection.
        db.fetch(&final_sql).await.map_err(|e| e.to_string())
    } else {
        // Non-row-returning write — use execute.
        let n = db.execute(&final_sql).await.map_err(|e| e.to_string())?;
        Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            rows_affected: Some(n),
        })
    }
}
