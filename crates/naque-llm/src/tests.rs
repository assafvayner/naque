use tokio_util::sync::CancellationToken;

use crate::{Agent, AgentConfig, LlmResponse, MockExecutor, MockProvider, ToolCall, Usage, standard_tools};

fn config(max_iterations: u32) -> AgentConfig {
    AgentConfig {
        model: "mock-model".to_string(),
        max_iterations,
        max_tokens: 1024,
        system_preamble: "You are a helpful assistant.".to_string(),
    }
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        input: serde_json::json!({}),
    }
}

fn text_response(text: &str, input_tokens: u64, output_tokens: u64) -> LlmResponse {
    LlmResponse {
        text: Some(text.to_string()),
        tool_calls: vec![],
        usage: Usage {
            input_tokens,
            output_tokens,
        },
        stop_reason: "end_turn".to_string(),
    }
}

fn tool_response(call: ToolCall) -> LlmResponse {
    LlmResponse {
        text: None,
        tool_calls: vec![call],
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
        },
        stop_reason: "tool_use".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Test 1: immediate answer — no tool calls
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_immediate_answer() {
    let provider = MockProvider::new([text_response("hi", 5, 3)]);
    let mut agent = Agent::new(Box::new(provider), config(5));
    let mut executor = MockExecutor::new();

    let cancel = CancellationToken::new();
    let result = agent.run_turn("hello", "", &mut executor, &mut (), &cancel).await.unwrap();

    assert_eq!(result.answer, "hi");
    assert_eq!(result.iterations, 1);
    assert!(result.tool_invocations.is_empty());
    assert!(!result.hit_iteration_cap);
    assert_eq!(executor.calls.len(), 0);
}

// ---------------------------------------------------------------------------
// Test 2: one tool call then final answer
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_one_tool_then_answer() {
    let tc = tool_call("id1", "run_query");
    let provider = MockProvider::new([tool_response(tc.clone()), text_response("done", 20, 8)]);
    let mut agent = Agent::new(Box::new(provider), config(5));
    let mut executor = MockExecutor::new().on_success("run_query", "42 rows");

    let cancel = CancellationToken::new();
    let result = agent
        .run_turn("query something", "", &mut executor, &mut (), &cancel)
        .await
        .unwrap();

    assert_eq!(result.answer, "done");
    assert_eq!(result.iterations, 2);
    assert_eq!(result.tool_invocations, vec!["run_query"]);
    assert!(!result.hit_iteration_cap);

    // executor was called once
    assert_eq!(executor.calls.len(), 1);
    assert_eq!(executor.calls[0].name, "run_query");

    // usage summed: (10+20) input, (5+8) output
    assert_eq!(result.usage.input_tokens, 30);
    assert_eq!(result.usage.output_tokens, 13);
}

// ---------------------------------------------------------------------------
// Test 3: tool error feeds back; loop continues and returns next answer
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_tool_error_feeds_back() {
    let tc = tool_call("id2", "run_query");
    let provider = MockProvider::new([tool_response(tc.clone()), text_response("recovered", 15, 6)]);
    let mut agent = Agent::new(Box::new(provider), config(5));
    // executor will return an error for run_query
    let mut executor = MockExecutor::new().on_error("run_query", "permission denied");

    let cancel = CancellationToken::new();
    let result = agent
        .run_turn("query again", "", &mut executor, &mut (), &cancel)
        .await
        .unwrap();

    assert_eq!(result.answer, "recovered");
    assert_eq!(result.iterations, 2);
    // the executor was still called
    assert_eq!(executor.calls.len(), 1);

    // verify the ToolResult with is_error=true is in the conversation
    let history = agent.history_len();
    // messages: User, Assistant(tool_call), ToolResult(error), Assistant(text)
    assert_eq!(history, 4);
}

// ---------------------------------------------------------------------------
// Test 4: iteration cap
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_iteration_cap() {
    // Provider always returns a tool call — should not loop forever.
    let tc = tool_call("id3", "inspect_table");
    let responses: Vec<LlmResponse> = (0..10).map(|_| tool_response(tc.clone())).collect();
    let provider = MockProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), config(2));
    let mut executor = MockExecutor::new().on_success("inspect_table", "some schema");

    let cancel = CancellationToken::new();
    let result = agent
        .run_turn("keep calling tools", "", &mut executor, &mut (), &cancel)
        .await
        .unwrap();

    assert!(result.hit_iteration_cap);
    assert_eq!(result.iterations, 2);
}

// ---------------------------------------------------------------------------
// Test 5: conversation memory and clear
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_conversation_memory_and_clear() {
    // First turn: provider returns immediate answer.
    let provider = MockProvider::new([
        text_response("first answer", 5, 3),
        text_response("second answer", 15, 7),
    ]);
    let mut agent = Agent::new(Box::new(provider), config(5));
    let mut executor = MockExecutor::new();

    let cancel = CancellationToken::new();
    // Turn 1
    let _ = agent
        .run_turn("first question", "", &mut executor, &mut (), &cancel)
        .await
        .unwrap();
    // History should have User + Assistant = 2 messages.
    assert!(agent.history_len() > 0);

    // Turn 2 — the provider should receive >1 message (the prior conversation
    // plus the new user message).  We can't inspect the request directly here,
    // but we can verify the history grows correctly.
    let before = agent.history_len();
    let _ = agent
        .run_turn("second question", "", &mut executor, &mut (), &cancel)
        .await
        .unwrap();
    // Two more messages (User + Assistant) added.
    assert_eq!(agent.history_len(), before + 2);

    // Clear wipes everything.
    agent.clear();
    assert_eq!(agent.history_len(), 0);
}

// ---------------------------------------------------------------------------
// Test 5b: provider receives prior messages on second turn
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_provider_sees_prior_messages_on_second_turn() {
    let provider = MockProvider::new([text_response("first", 5, 3), text_response("second", 5, 3)]);
    let provider_ref = std::sync::Arc::new(provider);

    // Wrap in a newtype so we can share the Arc.
    struct SharedProvider(std::sync::Arc<MockProvider>);

    #[async_trait::async_trait]
    impl crate::LlmProvider for SharedProvider {
        async fn complete(&self, req: &crate::LlmRequest) -> Result<crate::LlmResponse, crate::LlmError> {
            self.0.complete(req).await
        }
        fn name(&self) -> &str {
            "shared-mock"
        }
    }

    let mut agent = Agent::new(Box::new(SharedProvider(provider_ref.clone())), config(5));
    let mut executor = MockExecutor::new();

    let cancel = CancellationToken::new();
    let _ = agent.run_turn("q1", "", &mut executor, &mut (), &cancel).await.unwrap();
    let after_first = provider_ref.last_request_message_count();
    // After first turn the request has exactly 1 message (the user message).
    assert_eq!(after_first, 1);

    let _ = agent.run_turn("q2", "", &mut executor, &mut (), &cancel).await.unwrap();
    let after_second = provider_ref.last_request_message_count();
    // After second turn: prior [User, Assistant] + new User = 3 messages.
    assert_eq!(after_second, 3);
}

// ---------------------------------------------------------------------------
// Test: complete_once — single-shot text, no tools
// ---------------------------------------------------------------------------
#[tokio::test]
async fn complete_once_returns_text_without_tools() {
    let provider = MockProvider::new(vec![LlmResponse {
        text: Some("a short overview".into()),
        tool_calls: vec![],
        usage: Usage::default(),
        stop_reason: "end_turn".into(),
    }]);
    let agent = Agent::new(Box::new(provider), config(5));
    let out = agent.complete_once("system", "describe this schema").await.unwrap();
    assert_eq!(out, "a short overview");
}

// ---------------------------------------------------------------------------
// Test 6: standard_tools returns the 4 expected tools
// ---------------------------------------------------------------------------
#[test]
fn test_standard_tools() {
    let tools = standard_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();

    assert_eq!(names, vec!["inspect_table", "sample_table", "explain", "run_query"]);

    for tool in &tools {
        assert!(!tool.name.is_empty());
        assert!(!tool.description.is_empty());
        // input_schema must be a JSON object
        assert!(tool.input_schema.is_object(), "input_schema for '{}' must be an object", tool.name);
    }
}
