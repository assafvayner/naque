use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::{LlmError, LlmProvider, LlmRequest, LlmResponse, ToolCall, ToolExecutor};

// ---------------------------------------------------------------------------
// MockProvider
// ---------------------------------------------------------------------------

/// A provider backed by a queue of pre-scripted responses.
///
/// Responses are returned in FIFO order. If the queue is exhausted and the
/// provider is called again, it returns an error.
///
/// Use `last_request_message_count()` to inspect what the agent sent on the
/// most recent call — useful for testing conversation-memory behaviour.
pub struct MockProvider {
    responses: Mutex<VecDeque<LlmResponse>>,
    last_message_count: Mutex<usize>,
}

impl MockProvider {
    pub fn new(responses: impl IntoIterator<Item = LlmResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            last_message_count: Mutex::new(0),
        }
    }

    /// Number of messages in the last `complete` call's request.
    pub fn last_request_message_count(&self) -> usize {
        *self.last_message_count.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        *self.last_message_count.lock().unwrap() = req.messages.len();
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| LlmError::Provider("MockProvider: response queue exhausted".to_string()))
    }

    fn name(&self) -> &str {
        "mock"
    }
}

// ---------------------------------------------------------------------------
// MockExecutor
// ---------------------------------------------------------------------------

/// A tool executor backed by a name → result mapping.
///
/// If a tool name is not in the map the executor returns a generic string.
/// All calls (name + input) are recorded in `calls` for post-hoc assertions.
pub struct MockExecutor {
    results: HashMap<String, Result<String, String>>,
    pub calls: Vec<ToolCall>,
}

impl MockExecutor {
    pub fn new() -> Self {
        Self {
            results: HashMap::new(),
            calls: Vec::new(),
        }
    }

    /// Register a successful result for `tool_name`.
    pub fn on_success(mut self, tool_name: impl Into<String>, result: impl Into<String>) -> Self {
        self.results.insert(tool_name.into(), Ok(result.into()));
        self
    }

    /// Register an error result for `tool_name`.
    pub fn on_error(mut self, tool_name: impl Into<String>, err: impl Into<String>) -> Self {
        self.results.insert(tool_name.into(), Err(err.into()));
        self
    }
}

impl Default for MockExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ToolExecutor for MockExecutor {
    async fn execute(&mut self, call: &ToolCall) -> Result<String, LlmError> {
        self.calls.push(call.clone());
        match self.results.get(&call.name) {
            Some(Ok(s)) => Ok(s.clone()),
            Some(Err(e)) => Err(LlmError::Tool(e.clone())),
            None => Ok(format!("(mock result for {})", call.name)),
        }
    }
}
