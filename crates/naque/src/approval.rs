//! Gate-prompt abstraction — decouples the engine from the TUI.

use naque_core::gate::GateDecision;

/// The outcome of an approval prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run the query as-is.
    Accept,
    /// Run a rewritten version of the query.
    AcceptEdited(String),
    /// Do not run the query.
    Reject,
}

/// Anything that can prompt a human (or a test script) about a gated query.
pub trait Approver: Send {
    /// Ask whether to run `sql`.
    ///
    /// `label` is a human-readable summary of what the statement does.
    /// `decision` is the gate's verdict (`Prompt` or `PromptCatastrophic`).
    fn approve(&mut self, sql: &str, label: &str, decision: GateDecision) -> ApprovalDecision;
}

// ---------------------------------------------------------------------------
// Test / scripted implementations
// ---------------------------------------------------------------------------

/// Always accepts without modification.
pub struct AutoApprove;

impl Approver for AutoApprove {
    fn approve(&mut self, _sql: &str, _label: &str, _decision: GateDecision) -> ApprovalDecision {
        ApprovalDecision::Accept
    }
}

/// Always rejects.
pub struct AutoReject;

impl Approver for AutoReject {
    fn approve(&mut self, _sql: &str, _label: &str, _decision: GateDecision) -> ApprovalDecision {
        ApprovalDecision::Reject
    }
}

/// Returns decisions from a pre-scripted queue (FIFO).
///
/// If the queue is exhausted, panics — this is a test helper and an empty
/// queue means the test is missing expected interactions.
pub struct ScriptedApprover {
    queue: std::collections::VecDeque<ApprovalDecision>,
}

impl ScriptedApprover {
    pub fn new(decisions: impl IntoIterator<Item = ApprovalDecision>) -> Self {
        Self {
            queue: decisions.into_iter().collect(),
        }
    }
}

impl Approver for ScriptedApprover {
    fn approve(&mut self, sql: &str, _label: &str, _decision: GateDecision) -> ApprovalDecision {
        self.queue.pop_front().unwrap_or_else(|| {
            panic!("ScriptedApprover: queue exhausted (approve called for: {sql:?})")
        })
    }
}
