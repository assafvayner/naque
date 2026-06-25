mod agent;
mod error;
mod mock;
mod observer;
mod provider;
mod providers;
mod streaming;
mod tools;
mod types;

#[cfg(test)]
mod tests;

pub use agent::{Agent, AgentConfig, TurnResult};
pub use error::LlmError;
pub use mock::{MockExecutor, MockProvider, PendingProvider, ScriptedStreamProvider};
pub use observer::{AgentEvent, AgentObserver, RecordingObserver, summarize_tool_result};
pub use provider::{LlmProvider, TextSink, ToolExecutor};
pub use providers::{ClaudeProvider, GeminiProvider, HfProvider, OllamaProvider, OpenAIProvider};
pub use tools::standard_tools;
pub use types::{LlmRequest, LlmResponse, Message, ToolCall, ToolDef, Usage};
