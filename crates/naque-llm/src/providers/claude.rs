use serde_json::{Value, json};

use crate::{LlmError, LlmRequest, LlmResponse, Message, ToolCall, Usage};

pub struct ClaudeProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl ClaudeProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string()),
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Result<Self, LlmError> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| LlmError::Provider("ANTHROPIC_API_KEY not set".to_string()))?;
        Ok(Self::new(key, None))
    }

    pub fn build_body(&self, req: &LlmRequest) -> Value {
        let messages: Vec<Value> = req.messages.iter().map(map_message).collect();

        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();

        json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "system": req.system,
            "tools": tools,
            "messages": messages,
        })
    }

    pub fn parse_response(json: &Value) -> Result<LlmResponse, LlmError> {
        let content = json
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| LlmError::Provider("missing content array".to_string()))?;

        let mut text_parts: Vec<&str> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        text_parts.push(t);
                    }
                },
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    tool_calls.push(ToolCall { id, name, input });
                },
                // Ignore "thinking" and any other block types.
                _ => {},
            }
        }

        let text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let stop_reason = json.get("stop_reason").and_then(Value::as_str).unwrap_or("").to_string();

        let usage = {
            let u = json.get("usage");
            Usage {
                input_tokens: u.and_then(|v| v.get("input_tokens")).and_then(Value::as_u64).unwrap_or(0),
                output_tokens: u.and_then(|v| v.get("output_tokens")).and_then(Value::as_u64).unwrap_or(0),
            }
        };

        Ok(LlmResponse {
            text,
            tool_calls,
            usage,
            stop_reason,
        })
    }
}

fn map_message(msg: &Message) -> Value {
    match msg {
        Message::User(s) => json!({ "role": "user", "content": s }),
        Message::Assistant { text, tool_calls } => {
            let mut content: Vec<Value> = Vec::new();
            if let Some(t) = text
                && !t.is_empty()
            {
                content.push(json!({ "type": "text", "text": t }));
            }
            for call in tool_calls {
                content.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": call.input,
                }));
            }
            json!({ "role": "assistant", "content": content })
        },
        Message::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                    "is_error": is_error,
                }]
            })
        },
    }
}

#[async_trait::async_trait]
impl crate::LlmProvider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/v1/messages", self.base_url);
        let body = self.build_body(req);

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Provider(e.to_string()))?;

        let status = resp.status();
        let json: Value = resp.json().await.map_err(|e| LlmError::Provider(e.to_string()))?;

        if !status.is_success() {
            let msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(LlmError::Provider(format!("HTTP {status}: {msg}")));
        }

        Self::parse_response(&json)
    }
}
