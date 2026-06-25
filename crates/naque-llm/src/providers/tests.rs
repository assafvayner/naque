use serde_json::{json, Value};

use super::openai::{openai_build_body, openai_parse_response};
use crate::{
    ClaudeProvider, GeminiProvider, HfProvider, LlmProvider, LlmRequest, Message, OllamaProvider, OpenAIProvider,
    ToolCall, ToolDef,
};

fn make_request() -> LlmRequest {
    LlmRequest {
        model: "test-model".to_string(),
        system: "You are a test assistant.".to_string(),
        messages: vec![
            Message::User("Hello".to_string()),
            Message::Assistant {
                text: Some("I'll call the tool.".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_abc".to_string(),
                    name: "run_query".to_string(),
                    input: json!({ "sql": "SELECT 1" }),
                }],
            },
            Message::ToolResult {
                tool_use_id: "call_abc".to_string(),
                content: "1 row".to_string(),
                is_error: false,
            },
        ],
        tools: vec![ToolDef {
            name: "run_query".to_string(),
            description: "Run a SQL query".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": { "type": "string" }
                },
                "required": ["sql"]
            }),
        }],
        max_tokens: 1024,
    }
}

// ---------------------------------------------------------------------------
// ClaudeProvider tests
// ---------------------------------------------------------------------------

#[test]
fn claude_build_body_maps_messages_and_tools() {
    let provider = ClaudeProvider::new("key".to_string(), None);
    let req = make_request();
    let body = provider.build_body(&req);

    // Top-level fields
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["max_tokens"], 1024);
    assert_eq!(body["system"], "You are a test assistant.");
    assert!(body.get("thinking").is_none(), "must not send thinking field");

    // Tools
    let tool = &body["tools"][0];
    assert_eq!(tool["name"], "run_query");
    assert_eq!(tool["description"], "Run a SQL query");
    assert!(tool["input_schema"].is_object());

    // Messages
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);

    // User message
    let user_msg = &messages[0];
    assert_eq!(user_msg["role"], "user");
    assert_eq!(user_msg["content"], "Hello");

    // Assistant message — content is an array with text + tool_use blocks
    let asst_msg = &messages[1];
    assert_eq!(asst_msg["role"], "assistant");
    let content = asst_msg["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "I'll call the tool.");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "call_abc");
    assert_eq!(content[1]["name"], "run_query");
    assert_eq!(content[1]["input"]["sql"], "SELECT 1");

    // ToolResult — role user, content is array with tool_result block
    let tr_msg = &messages[2];
    assert_eq!(tr_msg["role"], "user");
    let tr_content = tr_msg["content"].as_array().unwrap();
    assert_eq!(tr_content.len(), 1);
    assert_eq!(tr_content[0]["type"], "tool_result");
    assert_eq!(tr_content[0]["tool_use_id"], "call_abc");
    assert_eq!(tr_content[0]["content"], "1 row");
    assert_eq!(tr_content[0]["is_error"], false);
}

#[test]
fn claude_parse_response_text_and_tools() {
    let sample = json!({
        "content": [
            { "type": "text", "text": "Here is the result: " },
            {
                "type": "tool_use",
                "id": "toolu_01",
                "name": "run_query",
                "input": { "sql": "SELECT COUNT(*) FROM users" }
            }
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 10, "output_tokens": 5 }
    });

    let resp = ClaudeProvider::parse_response(&sample).unwrap();

    assert_eq!(resp.text.as_deref(), Some("Here is the result: "));
    assert_eq!(resp.stop_reason, "tool_use");
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "toolu_01");
    assert_eq!(resp.tool_calls[0].name, "run_query");
    assert_eq!(resp.tool_calls[0].input["sql"], "SELECT COUNT(*) FROM users");
}

#[test]
fn claude_parse_response_text_only() {
    let sample = json!({
        "content": [
            { "type": "text", "text": "Just a text answer." }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 20, "output_tokens": 7 }
    });

    let resp = ClaudeProvider::parse_response(&sample).unwrap();

    assert_eq!(resp.text.as_deref(), Some("Just a text answer."));
    assert!(resp.tool_calls.is_empty());
    assert_eq!(resp.stop_reason, "end_turn");
}

#[test]
fn claude_parse_response_ignores_thinking_blocks() {
    let sample = json!({
        "content": [
            { "type": "thinking", "thinking": "Let me reason..." },
            { "type": "text", "text": "Answer after thinking." }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 30, "output_tokens": 10 }
    });

    let resp = ClaudeProvider::parse_response(&sample).unwrap();

    assert_eq!(resp.text.as_deref(), Some("Answer after thinking."));
    assert!(resp.tool_calls.is_empty());
}

#[test]
fn claude_name() {
    let provider = ClaudeProvider::new("k".to_string(), None);
    assert_eq!(provider.name(), "claude");
}

// ---------------------------------------------------------------------------
// OpenAIProvider tests
// ---------------------------------------------------------------------------

#[test]
fn openai_build_body_and_parse() {
    let provider = OpenAIProvider::new("key".to_string(), None);
    let req = make_request();
    let body = provider.build_body(&req);

    // System message is first
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "You are a test assistant.");

    // Tools use the function wrapper
    let tool = &body["tools"][0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["function"]["name"], "run_query");

    // ToolResult → role:tool
    let tool_result_msg = &body["messages"][3]; // sys + user + asst + tool_result
    assert_eq!(tool_result_msg["role"], "tool");
    assert_eq!(tool_result_msg["tool_call_id"], "call_abc");

    // Parse a sample response
    let sample = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_xyz",
                    "type": "function",
                    "function": {
                        "name": "run_query",
                        "arguments": "{\"sql\":\"SELECT 2\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 15,
            "completion_tokens": 8
        }
    });

    let resp = OpenAIProvider::parse_response(&sample).unwrap();

    assert!(resp.text.is_none());
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_xyz");
    assert_eq!(resp.tool_calls[0].name, "run_query");
    assert_eq!(resp.tool_calls[0].input["sql"], "SELECT 2");
    assert_eq!(resp.stop_reason, "tool_calls");
    assert_eq!(resp.usage.input_tokens, 15);
    assert_eq!(resp.usage.output_tokens, 8);
}

#[test]
fn openai_name() {
    let provider = OpenAIProvider::new("k".to_string(), None);
    assert_eq!(provider.name(), "openai");
}

// ---------------------------------------------------------------------------
// OllamaProvider tests
// ---------------------------------------------------------------------------

#[test]
fn ollama_build_body_and_parse() {
    let provider = OllamaProvider::new(None);
    let req = make_request();
    let body = provider.build_body(&req);

    // Must have stream:false
    assert_eq!(body["stream"], Value::Bool(false));

    // System message is first
    assert_eq!(body["messages"][0]["role"], "system");

    // Tools use function wrapper
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "run_query");

    // Parse a sample response — arguments as an object, no id
    let sample = json!({
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": {
                    "name": "run_query",
                    "arguments": { "sql": "SELECT 3" }
                }
            }]
        },
        "done_reason": "stop",
        "prompt_eval_count": 11,
        "eval_count": 4
    });

    let resp = OllamaProvider::parse_response(&sample).unwrap();

    assert!(resp.text.is_none()); // empty string → None
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_0"); // synthesized
    assert_eq!(resp.tool_calls[0].name, "run_query");
    assert_eq!(resp.tool_calls[0].input["sql"], "SELECT 3");
    assert_eq!(resp.stop_reason, "stop");
    assert_eq!(resp.usage.input_tokens, 11);
    assert_eq!(resp.usage.output_tokens, 4);
}

#[test]
fn ollama_name() {
    let provider = OllamaProvider::new(None);
    assert_eq!(provider.name(), "ollama");
}

// ---------------------------------------------------------------------------
// HfProvider tests
// ---------------------------------------------------------------------------

#[test]
fn hf_name() {
    let provider = HfProvider::new("k".to_string(), None);
    assert_eq!(provider.name(), "hf");
}

#[test]
fn hf_build_body_reuses_openai_format() {
    let req = LlmRequest {
        model: "zai-org/GLM-5.2:together".to_string(),
        system: "You are a test assistant.".to_string(),
        messages: vec![Message::User("Hello".to_string())],
        tools: vec![ToolDef {
            name: "get_weather".to_string(),
            description: "Get weather for a city".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"]
            }),
        }],
        max_tokens: 300,
    };

    let body = openai_build_body(&req);

    // Model string passed verbatim (including :provider suffix)
    assert_eq!(body["model"], "zai-org/GLM-5.2:together");

    // System message is first in messages array
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "You are a test assistant.");

    // User message next
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "Hello");

    // Tool is in OpenAI function wrapper format
    let tool = &body["tools"][0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["function"]["name"], "get_weather");
    assert_eq!(tool["function"]["description"], "Get weather for a city");
}

#[test]
fn openai_parse_response_empty_content_with_tool_calls() {
    // Verify parser handles null/empty content + tool_calls (HF/OpenAI pattern)
    let sample = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_hf_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 42,
            "completion_tokens": 12
        }
    });

    let resp = openai_parse_response(&sample).unwrap();

    assert!(resp.text.is_none(), "null content should yield None text");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_hf_1");
    assert_eq!(resp.tool_calls[0].name, "get_weather");
    assert_eq!(resp.tool_calls[0].input["city"], "Paris");
    assert_eq!(resp.stop_reason, "tool_calls");
    assert_eq!(resp.usage.input_tokens, 42);
    assert_eq!(resp.usage.output_tokens, 12);
}

// ---------------------------------------------------------------------------
// GeminiProvider tests
// ---------------------------------------------------------------------------

#[test]
fn gemini_name() {
    let provider = GeminiProvider::new("k".to_string(), None);
    assert_eq!(provider.name(), "gemini");
}

// ---------------------------------------------------------------------------
// HfProvider live test — skipped when HF_TOKEN is unset
// ---------------------------------------------------------------------------
//
// Run with:
//   source ~/hf/prod_token && cargo test -p naque-llm hf_live -- --nocapture

#[tokio::test]
async fn hf_live_tool_call() {
    let key = match std::env::var("HF_TOKEN") {
        Ok(k) => k,
        Err(_) => {
            eprintln!("[skip] HF_TOKEN not set — skipping live HF test");
            return;
        },
    };

    let provider = HfProvider::new(key, None);
    let req = LlmRequest {
        model: "zai-org/GLM-5.2:together".to_string(),
        system: "".to_string(),
        messages: vec![Message::User(
            "Use the get_weather tool for Paris; do not answer in prose.".to_string(),
        )],
        tools: vec![ToolDef {
            name: "get_weather".to_string(),
            description: "Get the current weather for a city.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string", "description": "City name" }
                },
                "required": ["city"]
            }),
        }],
        max_tokens: 300,
    };

    let resp = provider.complete(&req).await.expect("live HF call failed");

    eprintln!("stop_reason: {}", resp.stop_reason);
    eprintln!("text: {:?}", resp.text);
    eprintln!("tool_calls: {:?}", resp.tool_calls);

    // Accept either a tool call OR non-empty text — don't over-assert.
    assert!(
        !resp.tool_calls.is_empty() || resp.text.as_deref().is_some_and(|t| !t.is_empty()),
        "expected at least one tool_call or non-empty text"
    );
}

// ---------------------------------------------------------------------------
// Optional live test — skipped when ANTHROPIC_API_KEY is unset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_claude_say_hi() {
    let key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            eprintln!("[skip] ANTHROPIC_API_KEY not set — skipping live Claude test");
            return;
        },
    };

    let provider = ClaudeProvider::new(key, None);
    let req = LlmRequest {
        model: "claude-opus-4-8".to_string(),
        system: "You are helpful.".to_string(),
        messages: vec![Message::User("Say 'hi' in one word.".to_string())],
        tools: vec![],
        max_tokens: 32,
    };

    let resp = provider.complete(&req).await.expect("live call failed");
    assert!(!resp.stop_reason.is_empty());
    // Don't assert exact text — just confirm we got something.
    eprintln!("live response: {:?}", resp.text);
}
