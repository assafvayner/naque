use crate::{standard_tools, LlmError, LlmProvider, LlmRequest, Message, ToolExecutor, Usage};

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
    ) -> Result<TurnResult, LlmError> {
        self.conversation.push(Message::User(user_input.to_string()));

        let system = format!("{}\n\n{}", self.config.system_preamble, catalog);
        let tools = standard_tools();

        let mut iterations: u32 = 0;
        let mut usage = Usage::default();
        let mut tool_invocations: Vec<String> = Vec::new();
        let mut last_text: Option<String> = None;

        loop {
            if iterations >= self.config.max_iterations {
                return Ok(TurnResult {
                    answer: last_text.unwrap_or_else(|| "(stopped: reached max iterations)".to_string()),
                    iterations,
                    tool_invocations,
                    usage,
                    hit_iteration_cap: true,
                });
            }

            let req = LlmRequest {
                model: self.config.model.clone(),
                system: system.clone(),
                messages: self.conversation.clone(),
                tools: tools.clone(),
                max_tokens: self.config.max_tokens,
            };

            let resp = self.provider.complete(&req).await?;
            iterations += 1;
            usage += resp.usage.clone();
            last_text = resp.text.clone();

            // Record the assistant turn in memory.
            self.conversation.push(Message::Assistant {
                text: resp.text.clone(),
                tool_calls: resp.tool_calls.clone(),
            });

            if resp.tool_calls.is_empty() {
                // Final answer.
                return Ok(TurnResult {
                    answer: resp.text.unwrap_or_default(),
                    iterations,
                    tool_invocations,
                    usage,
                    hit_iteration_cap: false,
                });
            }

            // Execute each tool call and record results.
            for call in &resp.tool_calls {
                tool_invocations.push(call.name.clone());
                let result_msg = match executor.execute(call).await {
                    Ok(content) => Message::ToolResult {
                        tool_use_id: call.id.clone(),
                        content,
                        is_error: false,
                    },
                    Err(e) => Message::ToolResult {
                        tool_use_id: call.id.clone(),
                        content: e.to_string(),
                        is_error: true,
                    },
                };
                self.conversation.push(result_msg);
            }
        }
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
