//! Background turn execution: spawn the agent on a task, stream events back.

use naque_db::QueryResult;
use naque_llm::{Agent, AgentEvent, AgentObserver, LlmError, TurnResult};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Forwards agent events to the UI loop over an unbounded channel.
pub struct ChannelObserver {
    tx: mpsc::UnboundedSender<AgentEvent>,
}

impl ChannelObserver {
    pub fn new(tx: mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self { tx }
    }
}

impl AgentObserver for ChannelObserver {
    fn on_event(&mut self, event: AgentEvent) {
        let _ = self.tx.send(event); // ignore if the UI has gone away
    }
}

/// Output handed back when a spawned turn completes.
pub struct TurnOutput {
    pub agent: Agent,
    pub result: Result<TurnResult, LlmError>,
    pub last_result: Option<QueryResult>,
    /// Indices of `last_result` columns the agent tagged as byte counts.
    pub last_byte_columns: Vec<usize>,
}

/// Handle to an in-flight spawned turn.
pub struct RunningTurn {
    pub handle: JoinHandle<TurnOutput>,
    pub cancel: CancellationToken,
}
