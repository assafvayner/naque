use crate::{
    AgentEvent, AgentObserver, LlmError, LlmProvider, LlmRequest, Message, ToolExecutor, Usage, standard_tools,
};

/// Configuration for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    /// Maximum provider round-trips per `run_turn` call.
    pub max_iterations: u32,
    pub max_tokens: u32,
    /// Base system instructions; the schema catalog is appended each turn.
    pub system_preamble: String,
}

/// The result of one agent turn.
#[derive(Debug, Clone)]
pub struct TurnResult {
    pub answer: String,
    pub iterations: u32,
    /// Names of tools called during this turn, in order.
    pub tool_invocations: Vec<String>,
    /// Token usage summed across all provider calls in the turn.
    pub usage: Usage,
    pub hit_iteration_cap: bool,
    /// True when the turn ended because it was cancelled (see `run_turn`).
    pub cancelled: bool,
}

/// Stateful agent that owns a provider and maintains conversation memory.
pub struct Agent {
    provider: Box<dyn LlmProvider>,
    config: AgentConfig,
    /// Running conversation history (persists across turns until `clear`).
    conversation: Vec<Message>,
}

impl Agent {
    pub fn new(provider: Box<dyn LlmProvider>, config: AgentConfig) -> Self {
        Self {
            provider,
            config,
            conversation: Vec::new(),
        }
    }

    /// Run one natural-language turn.
    ///
    /// Appends the user message to memory, then loops:
    /// - Call the provider.
    /// - Append the assistant message to memory.
    /// - If the response has tool calls, execute each one, append `ToolResult` messages, and repeat.
    /// - Stop when there are no tool calls (final answer) or `max_iterations` is reached.
    ///
    /// `catalog` is a compact schema description that is appended to the system
    /// preamble for this turn only.
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        catalog: &str,
        executor: &mut dyn ToolExecutor,
        observer: &mut dyn AgentObserver,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<TurnResult, LlmError> {
        let start_len = self.conversation.len();
        self.conversation.push(Message::User(user_input.to_string()));
        observer.on_event(AgentEvent::TurnStarted);

        let system = format!("{}\n\n{}", self.config.system_preamble, catalog);
        let tools = standard_tools();

        let mut iterations: u32 = 0;
        let mut usage = Usage::default();
        let mut tool_invocations: Vec<String> = Vec::new();
        let mut last_text: Option<String> = None;

        loop {
            if cancel.is_cancelled() {
                self.conversation.truncate(start_len);
                observer.on_event(AgentEvent::Cancelled);
                return Ok(cancelled_result(iterations, tool_invocations, usage));
            }
            if iterations >= self.config.max_iterations {
                observer.on_event(AgentEvent::TurnFinished {
                    iterations,
                    hit_iteration_cap: true,
                });
                return Ok(TurnResult {
                    answer: last_text.unwrap_or_else(|| "(stopped: reached max iterations)".to_string()),
                    iterations,
                    tool_invocations,
                    usage,
                    hit_iteration_cap: true,
                    cancelled: false,
                });
            }

            observer.on_event(AgentEvent::LlmCallStarted {
                iteration: iterations + 1,
            });

            let req = LlmRequest {
                model: self.config.model.clone(),
                system: system.clone(),
                messages: self.conversation.clone(),
                tools: tools.clone(),
                max_tokens: self.config.max_tokens,
            };

            let resp = {
                let mut on_text = |chunk: &str| observer.on_event(AgentEvent::TextDelta(chunk.to_string()));
                let fut = self.provider.complete_streaming(&req, &mut on_text);
                tokio::pin!(fut);
                tokio::select! {
                    r = &mut fut => Some(r?),
                    _ = cancel.cancelled() => None,
                }
            };
            let Some(resp) = resp else {
                self.conversation.truncate(start_len);
                observer.on_event(AgentEvent::Cancelled);
                return Ok(cancelled_result(iterations, tool_invocations, usage));
            };
            iterations += 1;
            usage += resp.usage.clone();
            last_text = resp.text.clone();
            observer.on_event(AgentEvent::UsageUpdated(usage.clone()));

            self.conversation.push(Message::Assistant {
                text: resp.text.clone(),
                tool_calls: resp.tool_calls.clone(),
            });

            if resp.tool_calls.is_empty() {
                observer.on_event(AgentEvent::TurnFinished {
                    iterations,
                    hit_iteration_cap: false,
                });
                return Ok(TurnResult {
                    answer: resp.text.unwrap_or_default(),
                    iterations,
                    tool_invocations,
                    usage,
                    hit_iteration_cap: false,
                    cancelled: false,
                });
            }

            for call in &resp.tool_calls {
                tool_invocations.push(call.name.clone());
                observer.on_event(AgentEvent::ToolCallStarted {
                    name: call.name.clone(),
                    sql: tool_call_sql(call),
                });
                let (content, is_error) = match executor.execute(call).await {
                    Ok(content) => (content, false),
                    Err(e) => (e.to_string(), true),
                };
                observer.on_event(AgentEvent::ToolCallFinished {
                    name: call.name.clone(),
                    summary: crate::summarize_tool_result(&content, is_error),
                    is_error,
                });
                self.conversation.push(Message::ToolResult {
                    tool_use_id: call.id.clone(),
                    content,
                    is_error,
                });
            }
        }
    }

    /// Maximum provider round-trips per turn (from the agent config).
    pub fn max_iterations(&self) -> u32 {
        self.config.max_iterations
    }

    /// Clear conversation memory. Does not affect the provider.
    pub fn clear(&mut self) {
        self.conversation.clear();
    }

    /// Number of messages currently in memory.
    pub fn history_len(&self) -> usize {
        self.conversation.len()
    }
}

/// Extract a human-readable SQL/target string from a tool call for display.
fn tool_call_sql(call: &crate::ToolCall) -> Option<String> {
    call.input
        .get("sql")
        .and_then(|v| v.as_str())
        .or_else(|| call.input.get("name").and_then(|v| v.as_str()))
        .map(str::to_string)
}

fn cancelled_result(iterations: u32, tool_invocations: Vec<String>, usage: Usage) -> TurnResult {
    TurnResult {
        answer: "(cancelled)".to_string(),
        iterations,
        tool_invocations,
        usage,
        hit_iteration_cap: false,
        cancelled: true,
    }
}

#[cfg(test)]
mod observer_tests {
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{AgentEvent, LlmResponse, MockExecutor, MockProvider, RecordingObserver, ToolCall, Usage};

    fn cfg() -> AgentConfig {
        AgentConfig {
            model: "mock".into(),
            max_iterations: 5,
            max_tokens: 256,
            system_preamble: "sys".into(),
        }
    }

    #[tokio::test]
    async fn emits_turn_and_tool_events_in_order() {
        let resp1 = LlmResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "t1".into(),
                name: "run_query".into(),
                input: serde_json::json!({ "sql": "SELECT 1" }),
            }],
            usage: Usage {
                input_tokens: 4,
                output_tokens: 2,
            },
            stop_reason: "tool_use".into(),
        };
        let resp2 = LlmResponse {
            text: Some("the answer".into()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 3,
                output_tokens: 1,
            },
            stop_reason: "end_turn".into(),
        };
        let mut agent = Agent::new(Box::new(MockProvider::new(vec![resp1, resp2])), cfg());
        let mut exec = MockExecutor::new().on_success("run_query", "1 rows");
        let mut obs = RecordingObserver::default();
        let cancel = CancellationToken::new();

        let out = agent.run_turn("hi", "catalog", &mut exec, &mut obs, &cancel).await.unwrap();

        assert_eq!(out.answer, "the answer");
        assert!(!out.cancelled);
        assert_eq!(obs.events.first(), Some(&AgentEvent::TurnStarted));
        assert!(matches!(obs.events.last(), Some(AgentEvent::TurnFinished { .. })));
        let started = obs.events.iter().position(|e| {
            matches!(e, AgentEvent::ToolCallStarted { name, sql }
                if name == "run_query" && sql.as_deref() == Some("SELECT 1"))
        });
        let finished = obs
            .events
            .iter()
            .position(|e| matches!(e, AgentEvent::ToolCallFinished { name, .. } if name == "run_query"));
        assert!(started.is_some() && finished.is_some() && started < finished);
        assert!(
            obs.events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "the answer"))
        );
    }

    #[tokio::test]
    async fn streams_text_in_multiple_deltas() {
        use crate::ScriptedStreamProvider;
        let provider = ScriptedStreamProvider::new(vec![vec!["par", "tial", " answer"]]);
        let mut agent = Agent::new(Box::new(provider), cfg());
        let mut exec = MockExecutor::new();
        let mut obs = RecordingObserver::default();
        let cancel = CancellationToken::new();

        let out = agent.run_turn("hi", "cat", &mut exec, &mut obs, &cancel).await.unwrap();
        assert_eq!(out.answer, "partial answer");
        let deltas: Vec<&str> = obs
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["par", "tial", " answer"]);
    }

    #[tokio::test]
    async fn cancel_during_call_interrupts_and_rolls_back() {
        use crate::PendingProvider;
        let mut agent = Agent::new(Box::new(PendingProvider), cfg());
        let mut exec = MockExecutor::new();
        let mut obs = RecordingObserver::default();
        let cancel = CancellationToken::new();
        let before = agent.history_len();

        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            token.cancel();
        });

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            agent.run_turn("hi", "cat", &mut exec, &mut obs, &cancel),
        )
        .await
        .expect("run_turn must return promptly after cancel")
        .unwrap();

        assert!(out.cancelled);
        assert_eq!(agent.history_len(), before);
        assert_eq!(obs.events.last(), Some(&AgentEvent::Cancelled));
    }

    #[tokio::test]
    async fn pre_cancelled_token_rolls_back_and_reports_cancelled() {
        let mut agent = Agent::new(Box::new(MockProvider::new(vec![])), cfg());
        let mut exec = MockExecutor::new();
        let mut obs = RecordingObserver::default();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let before = agent.history_len();
        let out = agent.run_turn("hi", "catalog", &mut exec, &mut obs, &cancel).await.unwrap();

        assert!(out.cancelled);
        assert_eq!(agent.history_len(), before, "conversation rolled back to boundary");
        assert_eq!(obs.events.last(), Some(&AgentEvent::Cancelled));
    }
}
