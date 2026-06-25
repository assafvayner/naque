use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::{LlmError, LlmRequest, LlmResponse, Message, ToolCall, Usage};

pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAIProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            client: reqwest::Client::builder()
                .user_agent(concat!("naque/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    pub fn from_env() -> Result<Self, LlmError> {
        let key =
            std::env::var("OPENAI_API_KEY").map_err(|_| LlmError::Provider("OPENAI_API_KEY not set".to_string()))?;
        Ok(Self::new(key, None))
    }

    pub fn build_body(&self, req: &LlmRequest) -> Value {
        openai_build_body(req)
    }

    pub fn parse_response(json: &Value) -> Result<LlmResponse, LlmError> {
        openai_parse_response(json)
    }
}

/// Build an OpenAI-compatible chat completions request body.
///
/// This is a free function so other providers (e.g. `HfProvider`) can reuse it
/// without instantiating an `OpenAIProvider`.
pub(crate) fn openai_build_body(req: &LlmRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    messages.push(json!({ "role": "system", "content": req.system }));
    for msg in &req.messages {
        messages.push(map_message(msg));
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
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

/// Parse an OpenAI-compatible chat completions response.
///
/// This is a free function so other providers (e.g. `HfProvider`) can reuse it
/// without instantiating an `OpenAIProvider`.
pub(crate) fn openai_parse_response(json: &Value) -> Result<LlmResponse, LlmError> {
    let choice = json
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| LlmError::Provider("missing choices[0]".to_string()))?;

    let message = choice
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
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let func = tc.get("function")?;
                    let name = func.get("name")?.as_str()?.to_string();
                    let arguments_str = func.get("arguments")?.as_str().unwrap_or("{}");
                    let input: Value = serde_json::from_str(arguments_str)
                        .unwrap_or_else(|_| Value::String(arguments_str.to_string()));
                    Some(ToolCall { id, name, input })
                })
                .collect()
        })
        .unwrap_or_default();

    let stop_reason = choice.get("finish_reason").and_then(Value::as_str).unwrap_or("").to_string();

    let usage = {
        let u = json.get("usage");
        Usage {
            input_tokens: u.and_then(|v| v.get("prompt_tokens")).and_then(Value::as_u64).unwrap_or(0),
            output_tokens: u.and_then(|v| v.get("completion_tokens")).and_then(Value::as_u64).unwrap_or(0),
        }
    };

    Ok(LlmResponse {
        text,
        tool_calls,
        usage,
        stop_reason,
    })
}

/// Perform an OpenAI-compatible chat-completions POST and parse the response.
///
/// Shared by every provider that speaks the OpenAI chat-completions dialect
/// (`OpenAIProvider`, `HfProvider`, `GeminiProvider`). The caller supplies the
/// fully-formed endpoint `url` and a bearer `api_key`.
pub(crate) async fn openai_chat_completion(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    req: &LlmRequest,
) -> Result<LlmResponse, LlmError> {
    let body = openai_build_body(req);

    let resp = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
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

    openai_parse_response(&json)
}

/// Streaming variant of [`openai_chat_completion`]: POSTs with `stream: true`,
/// forwards text deltas to `on_text`, and assembles the full response.
pub(crate) async fn openai_chat_completion_streaming(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    req: &LlmRequest,
    on_text: &mut crate::TextSink<'_>,
) -> Result<LlmResponse, LlmError> {
    let mut body = openai_build_body(req);
    body["stream"] = Value::Bool(true);
    body["stream_options"] = json!({ "include_usage": true });

    let resp = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Provider(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(LlmError::Provider(format!("HTTP {status}: {text}")));
    }

    let mut acc = crate::streaming::OpenAiStreamAcc::new();
    let mut sse = crate::streaming::SseBuffer::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| LlmError::Provider(e.to_string()))?;
        sse.push(&chunk);
        while let Some(data) = sse.next_event() {
            acc.handle(&data, on_text);
        }
    }
    Ok(acc.finish())
}

pub(crate) fn map_message(msg: &Message) -> Value {
    match msg {
        Message::User(s) => json!({ "role": "user", "content": s }),
        Message::Assistant { text, tool_calls } => {
            let mut obj = json!({
                "role": "assistant",
                "content": text.as_deref().unwrap_or(""),
            });
            if !tool_calls.is_empty() {
                let tc: Vec<Value> = tool_calls
                    .iter()
                    .map(|call| {
                        let arguments = serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".to_string());
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": arguments,
                            }
                        })
                    })
                    .collect();
                obj["tool_calls"] = Value::Array(tc);
            }
            obj
        },
        Message::ToolResult {
            tool_use_id, content, ..
        } => {
            json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            })
        },
    }
}

#[async_trait::async_trait]
impl crate::LlmProvider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        openai_chat_completion(&self.client, &url, &self.api_key, req).await
    }

    async fn complete_streaming(
        &self,
        req: &LlmRequest,
        on_text: &mut crate::TextSink<'_>,
    ) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        openai_chat_completion_streaming(&self.client, &url, &self.api_key, req, on_text).await
    }
}
