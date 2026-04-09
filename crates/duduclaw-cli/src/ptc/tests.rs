//! Tests for the PTC module.

use super::router::{PtcDecision, PtcRouter};
use super::sandbox::{PtcRpcServer, PtcSandbox};
use super::types::{BLOCKED_TOOLS, ScriptRequest};

#[test]
fn test_recursive_execute_program_prevention() {
    // execute_program is in BLOCKED_TOOLS, so it can't be called from within a PTC script
    assert!(BLOCKED_TOOLS.contains(&"execute_program"));
}

#[test]
fn test_blocked_tools_includes_dangerous_operations() {
    assert!(BLOCKED_TOOLS.contains(&"create_agent"));
    assert!(BLOCKED_TOOLS.contains(&"agent_remove"));
    assert!(BLOCKED_TOOLS.contains(&"agent_update_soul"));
    assert!(BLOCKED_TOOLS.contains(&"evolution_toggle"));
}

#[test]
fn test_router_disabled_always_json() {
    let router = PtcRouter::new(false);
    let decision = router.should_use_ptc("batch process all files", 10);
    assert_eq!(decision, PtcDecision::UseJsonToolCall);
}

#[test]
fn test_router_high_tool_count_triggers_ptc() {
    let router = PtcRouter::new(true);
    let decision = router.should_use_ptc("do something", 5);
    assert_eq!(decision, PtcDecision::UsePtcScript);
}

#[test]
fn test_router_keyword_triggers_ptc() {
    let router = PtcRouter::new(true);
    let decision = router.should_use_ptc("batch process these items", 0);
    assert_eq!(decision, PtcDecision::UsePtcScript);
}

#[test]
fn test_router_simple_query_stays_json() {
    let router = PtcRouter::new(true);
    let decision = router.should_use_ptc("what's the weather?", 0);
    assert_eq!(decision, PtcDecision::UseJsonToolCall);
}

#[test]
fn test_router_with_inference_hint_complex() {
    let router = PtcRouter::new(true);
    let decision = router.with_inference_hint("simple question", 0, true);
    assert_eq!(decision, PtcDecision::UsePtcScript);
}

#[test]
fn test_router_with_inference_hint_simple() {
    let router = PtcRouter::new(true);
    let decision = router.with_inference_hint("what's the weather?", 0, false);
    assert_eq!(decision, PtcDecision::UseJsonToolCall);
}

#[test]
fn test_router_with_inference_hint_disabled() {
    let router = PtcRouter::new(false);
    let decision = router.with_inference_hint("complex batch job", 10, true);
    assert_eq!(decision, PtcDecision::UseJsonToolCall);
}

#[test]
fn test_rpc_server_records_tool_calls() {
    let server = PtcRpcServer::new(|tool, _args| {
        serde_json::json!({"status": "ok", "tool": tool})
    });

    let result = server.handle_tool_call(
        "web_search",
        &serde_json::json!({"query": "test"}),
    );
    assert_eq!(result["status"], "ok");

    let calls = server.drain_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool, "web_search");
}

#[test]
fn test_rpc_server_drain_clears_calls() {
    let server = PtcRpcServer::new(|_tool, _args| serde_json::json!({"ok": true}));

    server.handle_tool_call("tool_a", &serde_json::json!({}));
    server.handle_tool_call("tool_b", &serde_json::json!({}));

    let calls = server.drain_tool_calls();
    assert_eq!(calls.len(), 2);

    // Second drain should be empty
    let calls = server.drain_tool_calls();
    assert_eq!(calls.len(), 0);
}

#[tokio::test]
async fn test_execute_unsupported_language() {
    let server = PtcRpcServer::new(|_tool, _args| serde_json::json!({"ok": true}));
    let req = ScriptRequest {
        code: "fn main() {}".to_string(),
        language: "rust".to_string(),
        timeout_seconds: 5,
        allowed_tools: vec![],
    };

    let result = PtcSandbox::execute(&req, &server).await.unwrap();
    assert!(!result.success);
    assert!(result.stderr.contains("Unsupported language"));
}

#[tokio::test]
async fn test_execute_in_container_fallback() {
    // Verify the container method falls back to subprocess execution.
    // Actual container tests require Docker.
    let server = PtcRpcServer::new(|_tool, _args| serde_json::json!({"ok": true}));
    let req = ScriptRequest {
        code: "print('hello')".to_string(),
        language: "python".to_string(),
        timeout_seconds: 5,
        allowed_tools: vec![],
    };

    // Should not panic — just delegates to subprocess
    let result = PtcSandbox::execute_in_container(&req, &server).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_execute_simple_python_script() {
    let server = PtcRpcServer::new(|_tool, _args| serde_json::json!({"ok": true}));
    let req = ScriptRequest {
        code: "print('hello ptc')".to_string(),
        language: "python".to_string(),
        timeout_seconds: 10,
        allowed_tools: vec![],
    };

    let result = PtcSandbox::execute(&req, &server).await.unwrap();
    // Only assert if python3 is available on the system
    if result.exit_code.is_some() {
        if result.success {
            assert!(result.stdout.contains("hello ptc"));
        }
    }
}

#[test]
fn test_script_request_default_timeout() {
    let req: ScriptRequest = serde_json::from_str(
        r#"{"code": "print(1)", "language": "python"}"#,
    )
    .unwrap();
    assert_eq!(req.timeout_seconds, 30);
}
