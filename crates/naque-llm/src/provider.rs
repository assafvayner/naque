use crate::{LlmError, LlmRequest, LlmResponse};

/// Sink for streamed text fragments. `Send` so it can cross the provider's
/// async boundary inside a spawned turn.
pub type TextSink<'a> = dyn FnMut(&str) + Send + 'a;

/// A provider that can complete an LLM request.
///
/// Implementations are expected to be cheaply clonable (e.g. `Arc`-wrapped
/// HTTP clients) so they can be shared across threads, but the trait itself
/// only requires `Send + Sync`.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError>;
    fn name(&self) -> &str;

    /// Stream a completion, calling `on_text` for each text fragment as it
    /// arrives, and returning the fully assembled response (incl. tool calls).
    ///
    /// Default: no real streaming — emit the whole text once. Providers that
    /// support SSE override this.
    async fn complete_streaming(&self, req: &LlmRequest, on_text: &mut TextSink<'_>) -> Result<LlmResponse, LlmError> {
        let resp = self.complete(req).await?;
        if let Some(t) = &resp.text
            && !t.is_empty()
        {
            on_text(t);
        }
        Ok(resp)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LlmRequest, LlmResponse, MockProvider, Usage};

    fn req() -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system: "s".into(),
            messages: vec![],
            tools: vec![],
            max_tokens: 16,
        }
    }

    #[tokio::test]
    async fn default_streaming_emits_full_text_once() {
        let p = MockProvider::new(vec![LlmResponse {
            text: Some("hello world".into()),
            tool_calls: vec![],
            usage: Usage::default(),
            stop_reason: "end_turn".into(),
        }]);
        let mut chunks: Vec<String> = Vec::new();
        let resp = p
            .complete_streaming(&req(), &mut |s: &str| chunks.push(s.to_string()))
            .await
            .unwrap();
        assert_eq!(chunks, vec!["hello world".to_string()]);
        assert_eq!(resp.text.as_deref(), Some("hello world"));
    }
}
