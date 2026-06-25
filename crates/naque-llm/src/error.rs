/// Errors produced by the LLM layer.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider error: {0}")]
    Provider(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("request error: {0}")]
    Request(String),
}
