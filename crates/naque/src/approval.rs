//! Gate-prompt abstraction — decouples the engine from the TUI.

use naque_core::gate::GateDecision;
use tokio::sync::{mpsc, oneshot};

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

/// The outcome of a filesystem read-grant prompt. This is the FS permission
/// dimension, distinct from [`ApprovalDecision`] (which gates SQL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathGrant {
    /// Allow this one access without persisting anything.
    Once,
    /// Allow and remember the path for the rest of the session.
    Session,
    /// Refuse the access.
    Deny,
}

/// Anything that can prompt a human (or a test script) about a gated query.
#[async_trait::async_trait]
pub trait Approver: Send {
    /// Ask whether to run `sql`. `label` summarizes the statement; `decision`
    /// is the gate verdict (`Prompt` or `PromptCatastrophic`).
    async fn approve(&mut self, sql: &str, label: &str, decision: GateDecision) -> ApprovalDecision;

    /// Ask whether to allow filesystem `action` (`"read"` / `"list"`) on
    /// `path`, which is outside the configured allow-list. The default denies,
    /// which is the safe choice for non-interactive approvers; the interactive
    /// (channel-backed) approver overrides this to prompt the user.
    async fn approve_path(&mut self, _path: &str, _action: &str) -> PathGrant {
        PathGrant::Deny
    }
}

// ---------------------------------------------------------------------------
// Channel-based implementation (bridges background turn tasks to the UI loop)
// ---------------------------------------------------------------------------

/// A request from a running turn for the UI to approve a gated query.
pub struct ApprovalRequest {
    pub sql: String,
    pub label: String,
    pub decision: GateDecision,
    /// The UI sends the user's decision back through this channel.
    pub reply: oneshot::Sender<ApprovalDecision>,
}

/// A request from a running turn for the UI to grant filesystem access to a
/// path outside the allow-list. The UI replies with a [`PathGrant`].
pub struct PathApprovalRequest {
    pub path: String,
    pub action: String,
    /// The UI sends the user's grant decision back through this channel.
    pub reply: oneshot::Sender<PathGrant>,
}

/// An [`Approver`] that bridges a background turn task to the UI loop: it sends
/// an [`ApprovalRequest`] (SQL) or [`PathApprovalRequest`] (filesystem) and
/// awaits the reply. If the UI side is gone, it rejects/denies (safe default).
pub struct ChannelApprover {
    tx: mpsc::UnboundedSender<ApprovalRequest>,
    path_tx: mpsc::UnboundedSender<PathApprovalRequest>,
}

impl ChannelApprover {
    pub fn new(
        tx: mpsc::UnboundedSender<ApprovalRequest>,
        path_tx: mpsc::UnboundedSender<PathApprovalRequest>,
    ) -> Self {
        Self { tx, path_tx }
    }
}

#[async_trait::async_trait]
impl Approver for ChannelApprover {
    async fn approve(&mut self, sql: &str, label: &str, decision: GateDecision) -> ApprovalDecision {
        let (reply, rx) = oneshot::channel();
        let req = ApprovalRequest {
            sql: sql.to_string(),
            label: label.to_string(),
            decision,
            reply,
        };
        if self.tx.send(req).is_err() {
            return ApprovalDecision::Reject;
        }
        rx.await.unwrap_or(ApprovalDecision::Reject)
    }

    async fn approve_path(&mut self, path: &str, action: &str) -> PathGrant {
        let (reply, rx) = oneshot::channel();
        let req = PathApprovalRequest {
            path: path.to_string(),
            action: action.to_string(),
            reply,
        };
        if self.path_tx.send(req).is_err() {
            return PathGrant::Deny;
        }
        rx.await.unwrap_or(PathGrant::Deny)
    }
}

// ---------------------------------------------------------------------------
// Test / scripted implementations
// ---------------------------------------------------------------------------

/// Always accepts without modification.
pub struct AutoApprove;

#[async_trait::async_trait]
impl Approver for AutoApprove {
    #[allow(clippy::unused_async)]
    async fn approve(&mut self, _sql: &str, _label: &str, _decision: GateDecision) -> ApprovalDecision {
        ApprovalDecision::Accept
    }

    #[allow(clippy::unused_async)]
    async fn approve_path(&mut self, _path: &str, _action: &str) -> PathGrant {
        PathGrant::Once
    }
}

/// Always rejects.
pub struct AutoReject;

#[async_trait::async_trait]
impl Approver for AutoReject {
    #[allow(clippy::unused_async)]
    async fn approve(&mut self, _sql: &str, _label: &str, _decision: GateDecision) -> ApprovalDecision {
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

#[async_trait::async_trait]
impl Approver for ScriptedApprover {
    #[allow(clippy::unused_async)]
    async fn approve(&mut self, sql: &str, _label: &str, _decision: GateDecision) -> ApprovalDecision {
        self.queue
            .pop_front()
            .unwrap_or_else(|| panic!("ScriptedApprover: queue exhausted (approve called for: {sql:?})"))
    }
}

#[cfg(test)]
mod channel_tests {
    use naque_core::gate::GateDecision;

    use super::*;

    #[tokio::test]
    async fn channel_approver_round_trips_decision() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let (path_tx, _path_rx) = tokio::sync::mpsc::unbounded_channel::<PathApprovalRequest>();
        let mut approver = ChannelApprover::new(tx, path_tx);

        let ui = tokio::spawn(async move {
            let req = rx.recv().await.expect("a request");
            assert_eq!(req.sql, "DELETE FROM t");
            req.reply
                .send(ApprovalDecision::AcceptEdited("DELETE FROM t WHERE id=1".into()))
                .unwrap();
        });

        let decision = approver.approve("DELETE FROM t", "WRITE: DELETE", GateDecision::Prompt).await;
        assert_eq!(decision, ApprovalDecision::AcceptEdited("DELETE FROM t WHERE id=1".into()));
        ui.await.unwrap();
    }

    #[tokio::test]
    async fn channel_approver_rejects_if_ui_drops_reply() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let (path_tx, _path_rx) = tokio::sync::mpsc::unbounded_channel::<PathApprovalRequest>();
        let mut approver = ChannelApprover::new(tx, path_tx);
        let ui = tokio::spawn(async move {
            let _req = rx.recv().await.unwrap(); // drop reply sender without responding
        });
        let decision = approver.approve("X", "L", GateDecision::Prompt).await;
        assert_eq!(decision, ApprovalDecision::Reject); // closed channel => safe default
        ui.await.unwrap();
    }

    #[tokio::test]
    async fn channel_approver_round_trips_path_grant() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let (path_tx, mut path_rx) = tokio::sync::mpsc::unbounded_channel::<PathApprovalRequest>();
        let mut approver = ChannelApprover::new(tx, path_tx);

        let ui = tokio::spawn(async move {
            let req = path_rx.recv().await.expect("a path request");
            assert_eq!(req.path, "~/sql/q.sql");
            assert_eq!(req.action, "read");
            req.reply.send(PathGrant::Session).unwrap();
        });

        let grant = approver.approve_path("~/sql/q.sql", "read").await;
        assert_eq!(grant, PathGrant::Session);
        ui.await.unwrap();
    }

    #[tokio::test]
    async fn channel_approver_denies_path_if_ui_drops_reply() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let (path_tx, mut path_rx) = tokio::sync::mpsc::unbounded_channel::<PathApprovalRequest>();
        let mut approver = ChannelApprover::new(tx, path_tx);
        let ui = tokio::spawn(async move {
            let _req = path_rx.recv().await.unwrap(); // drop reply without responding
        });
        assert_eq!(approver.approve_path("/x", "read").await, PathGrant::Deny);
        ui.await.unwrap();
    }
}
