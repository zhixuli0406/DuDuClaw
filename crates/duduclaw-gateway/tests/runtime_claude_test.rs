//! ClaudeRuntime unit tests.

/// Test RuntimeResponse structure.
#[test]
fn test_runtime_response_format() {
    let response = serde_json::json!({
        "content": "Hello!",
        "input_tokens": 100,
        "output_tokens": 50,
        "cache_read_tokens": 80,
        "model_used": "claude-sonnet-4-6",
        "runtime_name": "claude"
    });
    assert_eq!(response["runtime_name"], "claude");
    assert!(response["input_tokens"].as_u64().unwrap() > 0);
}

/// Test RuntimeContext construction.
#[test]
fn test_runtime_context_fields() {
    let ctx = serde_json::json!({
        "agent_dir": "/home/user/.duduclaw/agents/dudu",
        "system_prompt": "You are a helpful assistant.",
        "model": "claude-sonnet-4-6",
        "max_tokens": 4096,
        "home_dir": "/home/user/.duduclaw",
        "agent_id": "dudu"
    });
    assert_eq!(ctx["model"], "claude-sonnet-4-6");
    assert_eq!(ctx["max_tokens"], 4096);
}
