mod agent;
mod error;
mod mock;
mod provider;
mod providers;
mod tools;
mod types;

#[cfg(test)]
mod tests;

pub use agent::{Agent, AgentConfig, TurnResult};
pub use error::LlmError;
pub use mock::{MockExecutor, MockProvider};
pub use provider::{LlmProvider, ToolExecutor};
pub use providers::{ClaudeProvider, OllamaProvider, OpenAIProvider};
pub use tools::standard_tools;
pub use types::{LlmRequest, LlmResponse, Message, ToolCall, ToolDef, Usage};
