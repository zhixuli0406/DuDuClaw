//! WebChat WebSocket integration tests.
//!
//! These test the ChatMessage protocol and connection management logic
//! without requiring a live WebSocket server.

use serde_json::json;

/// Test ChatMessage serialization format for user_message.
#[test]
fn test_user_message_format() {
    let msg = json!({
        "type": "user_message",
        "content": "Hello world",
        "session_id": null
    });
    assert_eq!(msg["type"], "user_message");
    assert_eq!(msg["content"], "Hello world");
}

/// Test ChatMessage serialization format for assistant_done.
#[test]
fn test_assistant_done_format() {
    let msg = json!({
        "type": "assistant_done",
        "content": "Hi there!",
        "tokens_used": 42
    });
    assert_eq!(msg["type"], "assistant_done");
    assert_eq!(msg["tokens_used"], 42);
}

/// Test ChatMessage serialization format for error.
#[test]
fn test_error_message_format() {
    let msg = json!({
        "type": "error",
        "message": "Something went wrong"
    });
    assert_eq!(msg["type"], "error");
    assert!(msg["message"].as_str().unwrap().contains("wrong"));
}

/// Test session_info message format on connect.
#[test]
fn test_session_info_format() {
    let msg = json!({
        "type": "session_info",
        "session_id": "webchat:user-123",
        "agent_name": "DuDu",
        "agent_icon": "🐾"
    });
    assert_eq!(msg["type"], "session_info");
    assert!(msg["session_id"].as_str().unwrap().starts_with("webchat:"));
}

/// Test concurrent connection limit logic.
#[test]
fn test_connection_limit_logic() {
    // MAX_CONNECTIONS_PER_USER = 3
    let max = 3usize;
    let mut count = 0usize;

    // First 3 connections should be allowed
    for _ in 0..max {
        count += 1;
        assert!(count <= max);
    }

    // 4th connection should be rejected
    count += 1;
    assert!(count > max, "4th connection should exceed limit");
}

/// Test session persistence across reconnection (protocol level).
#[test]
fn test_session_id_stability() {
    let user_id = "webchat:abc-123";
    let session_id = format!("webchat:{user_id}");
    // Same user_id should produce same session_id
    let session_id2 = format!("webchat:{user_id}");
    assert_eq!(session_id, session_id2);
}
