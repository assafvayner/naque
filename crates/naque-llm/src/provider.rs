use crate::{LlmError, LlmRequest, LlmResponse};

/// A provider that can complete an LLM request.
///
/// Implementations are expected to be cheaply clonable (e.g. `Arc`-wrapped
/// HTTP clients) so they can be shared across threads, but the trait itself
/// only requires `Send + Sync`.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError>;
    fn name(&self) -> &str;
}

/// Something that can execute a tool call on behalf of the agent loop.
///
/// `&mut self` is intentional — executors often carry mutable state such as
/// a database connection, a running query cache, or call history.
#[async_trait::async_trait]
pub trait ToolExecutor: Send {
    /// Execute one tool call, returning the string result fed back to the model.
    async fn execute(&mut self, call: &crate::ToolCall) -> Result<String, LlmError>;
}
