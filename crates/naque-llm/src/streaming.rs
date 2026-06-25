//! SSE stream parsing for provider streaming responses.
//!
//! Two layers: [`SseBuffer`] splits a raw byte stream into per-event `data:`
//! payloads; provider-specific accumulators turn those payloads into text
//! deltas, tool calls, and usage. All of it is pure and unit-tested; the
//! network wiring lives in the provider impls.

use serde_json::Value;

use crate::{LlmResponse, ToolCall, Usage};

/// Accumulates raw response bytes and yields complete SSE event payloads.
///
/// Carriage returns are stripped on input so events are always delimited by a
/// blank line (`\n\n`). Each yielded `String` is the concatenation of the
/// event's `data:` line contents (newline-joined for multi-line data).
#[derive(Default)]
pub(crate) struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend(chunk.iter().copied().filter(|&b| b != b'\r'));
    }

    /// Pop the next complete event's `data:` payload, or `None` if no complete
    /// event is buffered yet.
    pub fn next_event(&mut self) -> Option<String> {
        let pos = self.buf.windows(2).position(|w| w == b"\n\n")?;
        let frame: Vec<u8> = self.buf.drain(..pos + 2).collect();
        let text = String::from_utf8_lossy(&frame);
        let mut data = String::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest);
            }
        }
        Some(data)
    }
}

/// Assembles an Anthropic streaming response from SSE event payloads.
#[derive(Default)]
pub(crate) struct AnthropicStreamAcc {
    text: String,
    stop_reason: String,
    usage: Usage,
    /// content block index -> partial tool block
    blocks: std::collections::BTreeMap<u64, ToolBlock>,
}

#[derive(Default)]
struct ToolBlock {
    id: String,
    name: String,
    json: String,
}

impl AnthropicStreamAcc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle one SSE `data:` payload (a JSON object). Unknown/`[DONE]`/`ping`
    /// payloads are ignored. Text deltas are forwarded to `on_text`.
    pub fn handle(&mut self, data: &str, on_text: &mut dyn FnMut(&str)) {
        if data.trim().is_empty() || data.trim() == "[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                    self.usage.input_tokens = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    self.usage.output_tokens = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                }
            },
            Some("content_block_start") => {
                let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = v.get("content_block");
                if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("tool_use") {
                    self.blocks.insert(
                        idx,
                        ToolBlock {
                            id: block
                                .and_then(|b| b.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            name: block
                                .and_then(|b| b.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            json: String::new(),
                        },
                    );
                }
            },
            Some("content_block_delta") => {
                let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = v.get("delta");
                match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(Value::as_str) {
                            self.text.push_str(t);
                            on_text(t);
                        }
                    },
                    Some("input_json_delta") => {
                        if let Some(p) = delta.and_then(|d| d.get("partial_json")).and_then(Value::as_str)
                            && let Some(b) = self.blocks.get_mut(&idx)
                        {
                            b.json.push_str(p);
                        }
                    },
                    _ => {},
                }
            },
            Some("message_delta") => {
                if let Some(sr) = v.get("delta").and_then(|d| d.get("stop_reason")).and_then(Value::as_str) {
                    self.stop_reason = sr.to_string();
                }
                if let Some(ot) = v.get("usage").and_then(|u| u.get("output_tokens")).and_then(Value::as_u64) {
                    self.usage.output_tokens = ot;
                }
            },
            _ => {},
        }
    }

    /// Finalize into an [`LlmResponse`].
    pub fn finish(self) -> LlmResponse {
        let tool_calls = self
            .blocks
            .into_values()
            .map(|b| ToolCall {
                id: b.id,
                name: b.name,
                input: serde_json::from_str(&b.json).unwrap_or(Value::Null),
            })
            .collect();
        LlmResponse {
            text: if self.text.is_empty() { None } else { Some(self.text) },
            tool_calls,
            usage: self.usage,
            stop_reason: self.stop_reason,
        }
    }
}

/// Assembles an OpenAI-compatible streaming response from SSE `data:` payloads.
#[derive(Default)]
pub(crate) struct OpenAiStreamAcc {
    text: String,
    stop_reason: String,
    usage: Usage,
    /// tool_call index -> (id, name, arguments-so-far)
    tools: std::collections::BTreeMap<u64, OpenAiToolFrag>,
}

#[derive(Default)]
struct OpenAiToolFrag {
    id: String,
    name: String,
    args: String,
}

impl OpenAiStreamAcc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle(&mut self, data: &str, on_text: &mut dyn FnMut(&str)) {
        if data.trim().is_empty() || data.trim() == "[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(u) = v.get("usage") {
            if let Some(p) = u.get("prompt_tokens").and_then(Value::as_u64) {
                self.usage.input_tokens = p;
            }
            if let Some(c) = u.get("completion_tokens").and_then(Value::as_u64) {
                self.usage.output_tokens = c;
            }
        }
        let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|a| a.first()) else {
            return;
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = fr.to_string();
        }
        let Some(delta) = choice.get("delta") else {
            return;
        };
        if let Some(c) = delta.get("content").and_then(Value::as_str)
            && !c.is_empty()
        {
            self.text.push_str(c);
            on_text(c);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let idx = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let frag = self.tools.entry(idx).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str)
                    && !id.is_empty()
                {
                    frag.id = id.to_string();
                }
                if let Some(func) = call.get("function") {
                    if let Some(n) = func.get("name").and_then(Value::as_str)
                        && !n.is_empty()
                    {
                        frag.name = n.to_string();
                    }
                    if let Some(a) = func.get("arguments").and_then(Value::as_str) {
                        frag.args.push_str(a);
                    }
                }
            }
        }
    }

    pub fn finish(self) -> LlmResponse {
        let tool_calls = self
            .tools
            .into_values()
            .map(|f| ToolCall {
                id: f.id,
                name: f.name,
                input: serde_json::from_str(&f.args).unwrap_or(Value::Null),
            })
            .collect();
        LlmResponse {
            text: if self.text.is_empty() { None } else { Some(self.text) },
            tool_calls,
            usage: self.usage,
            stop_reason: self.stop_reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_two_events() {
        let mut b = SseBuffer::new();
        b.push(b"event: x\ndata: one\n\ndata: two\n\n");
        assert_eq!(b.next_event().as_deref(), Some("one"));
        assert_eq!(b.next_event().as_deref(), Some("two"));
        assert_eq!(b.next_event(), None);
    }

    #[test]
    fn handles_crlf_and_partial_chunks() {
        let mut b = SseBuffer::new();
        b.push(b"data: hel"); // partial — no complete event yet
        assert_eq!(b.next_event(), None);
        b.push(b"lo\r\n\r\n");
        assert_eq!(b.next_event().as_deref(), Some("hello"));
    }

    #[test]
    fn anthropic_accumulates_text_and_tool_call() {
        use serde_json::json;
        let mut acc = AnthropicStreamAcc::new();
        let mut text = String::new();
        let feed = |v: serde_json::Value, acc: &mut AnthropicStreamAcc, t: &mut String| {
            acc.handle(&v.to_string(), &mut |s| t.push_str(s));
        };
        feed(
            json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}),
            &mut acc,
            &mut text,
        );
        feed(json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}), &mut acc, &mut text);
        feed(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}),
            &mut acc,
            &mut text,
        );
        feed(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}),
            &mut acc,
            &mut text,
        );
        feed(
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu1","name":"run_query"}}),
            &mut acc,
            &mut text,
        );
        feed(
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"sql\":\"SEL"}}),
            &mut acc,
            &mut text,
        );
        feed(
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ECT 1\"}"}}),
            &mut acc,
            &mut text,
        );
        feed(
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
            &mut acc,
            &mut text,
        );

        assert_eq!(text, "Hello");
        let resp = acc.finish();
        assert_eq!(resp.text.as_deref(), Some("Hello"));
        assert_eq!(resp.stop_reason, "tool_use");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 7);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "run_query");
        assert_eq!(resp.tool_calls[0].id, "tu1");
        assert_eq!(resp.tool_calls[0].input, serde_json::json!({ "sql": "SELECT 1" }));
    }

    #[test]
    fn openai_accumulates_text_and_tool_call() {
        use serde_json::json;
        let mut acc = OpenAiStreamAcc::new();
        let mut text = String::new();
        let feed = |v: serde_json::Value, acc: &mut OpenAiStreamAcc, t: &mut String| {
            acc.handle(&v.to_string(), &mut |s| t.push_str(s));
        };
        feed(json!({"choices":[{"delta":{"content":"Hi"}}]}), &mut acc, &mut text);
        feed(json!({"choices":[{"delta":{"content":" there"}}]}), &mut acc, &mut text);
        feed(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"run_query","arguments":"{\"sql\":\""}}]}}]}),
            &mut acc,
            &mut text,
        );
        feed(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"SELECT 1\"}"}}]}}]}),
            &mut acc,
            &mut text,
        );
        feed(
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":9}}),
            &mut acc,
            &mut text,
        );
        acc.handle("[DONE]", &mut |s| text.push_str(s));

        assert_eq!(text, "Hi there");
        let resp = acc.finish();
        assert_eq!(resp.text.as_deref(), Some("Hi there"));
        assert_eq!(resp.stop_reason, "tool_calls");
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 9);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "c1");
        assert_eq!(resp.tool_calls[0].name, "run_query");
        assert_eq!(resp.tool_calls[0].input, serde_json::json!({ "sql": "SELECT 1" }));
    }

    #[test]
    fn anthropic_orphan_input_delta_produces_no_tool_call() {
        let mut acc = AnthropicStreamAcc::new();
        // input_json_delta for index 0 with no content_block_start first
        acc.handle(
            &serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"sql\":\"X\"}"}}).to_string(),
            &mut |_| {},
        );
        let resp = acc.finish();
        assert!(resp.tool_calls.is_empty(), "orphan delta must not create a tool call");
    }
}
