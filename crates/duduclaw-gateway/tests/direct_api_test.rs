//! Direct API and OpenAI-compat SSE parsing tests.

/// Test SSE data line parsing.
#[test]
fn test_sse_data_line_parsing() {
    let line = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
    assert!(line.starts_with("data: "));
    let json_str = &line[6..];
    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
    let content = parsed
        .pointer("/choices/0/delta/content")
        .unwrap()
        .as_str()
        .unwrap();
    assert_eq!(content, "Hello");
}

/// Test SSE DONE marker.
#[test]
fn test_sse_done_marker() {
    let line = "data: [DONE]";
    let json_str = &line[6..];
    assert_eq!(json_str, "[DONE]");
}

/// Test SSE usage extraction from final chunk.
#[test]
fn test_sse_usage_extraction() {
    let line =
        r#"data: {"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":42}}"#;
    let json_str = &line[6..];
    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
    let prompt_tokens = parsed["usage"]["prompt_tokens"].as_u64().unwrap();
    let completion_tokens = parsed["usage"]["completion_tokens"].as_u64().unwrap();
    assert_eq!(prompt_tokens, 150);
    assert_eq!(completion_tokens, 42);
}

/// Test MiniMax response compatibility.
#[test]
fn test_minimax_response_compat() {
    // MiniMax uses OpenAI-compatible format
    let response = r#"{"choices":[{"message":{"role":"assistant","content":"你好！"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
    let parsed: serde_json::Value = serde_json::from_str(response).unwrap();
    let content = parsed
        .pointer("/choices/0/message/content")
        .unwrap()
        .as_str()
        .unwrap();
    assert_eq!(content, "你好！");
}
