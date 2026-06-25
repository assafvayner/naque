use serde_json::{json, Value};

use crate::{LlmError, LlmRequest, LlmResponse, Message, ToolCall, Usage};

pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: Option<String>) -> Self {
        Self {
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434".to_string()),
            client: reqwest::Client::new(),
        }
    }

    pub fn build_body(&self, req: &LlmRequest) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        messages.push(json!({ "role": "system", "content": req.system }));
        for msg in &req.messages {
            messages.push(map_message(msg));
        }

        let mut body = json!({
            "model": req.model,
            "stream": false,
            "messages": messages,
        });

        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = Value::Array(tools);
        }

        body
    }

    pub fn parse_response(json: &Value) -> Result<LlmResponse, LlmError> {
        let message = json
            .get("message")
            .ok_or_else(|| LlmError::Provider("missing message".to_string()))?;

        let text = message
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .filter_map(|(i, tc)| {
                        let func = tc.get("function")?;
                        let name = func.get("name")?.as_str()?.to_string();
                        let input = func.get("arguments")?.clone();
                        let id = format!("call_{i}");
                        Some(ToolCall { id, name, input })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let stop_reason = json.get("done_reason").and_then(Value::as_str).unwrap_or("stop").to_string();

        let usage = Usage {
            input_tokens: json.get("prompt_eval_count").and_then(Value::as_u64).unwrap_or(0),
            output_tokens: json.get("eval_count").and_then(Value::as_u64).unwrap_or(0),
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
            let content = text.as_deref().unwrap_or("");
            if tool_calls.is_empty() {
                json!({ "role": "assistant", "content": content })
            } else {
                let tc: Vec<Value> = tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "function": {
                                "name": call.name,
                                "arguments": call.input,
                            }
                        })
                    })
                    .collect();
                json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tc,
                })
            }
        },
        Message::ToolResult { content, .. } => {
            json!({ "role": "tool", "content": content })
        },
    }
}

#[async_trait::async_trait]
impl crate::LlmProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/api/chat", self.base_url);
        let body = self.build_body(req);

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Provider(e.to_string()))?;

        let status = resp.status();
        let json: Value = resp.json().await.map_err(|e| LlmError::Provider(e.to_string()))?;

        if !status.is_success() {
            let msg = json.get("error").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(LlmError::Provider(format!("HTTP {status}: {msg}")));
        }

        Self::parse_response(&json)
    }
}
