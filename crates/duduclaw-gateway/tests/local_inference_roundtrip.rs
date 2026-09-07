//! WP-D: local inference must actually answer, not just be configured.
//!
//! The goal-mode dispatch path (`claude_runner.rs`) routes to the local
//! model when `[general] inference_mode = "local"` or an agent's
//! `[model.local] prefer_local = true`. Both branches funnel into
//! `call_local_inference`, reached from outside the crate through
//! [`duduclaw_gateway::claude_runner::try_local_inference`] — and until now
//! nothing proved that path carries the agent's system prompt to an
//! OpenAI-compatible endpoint and brings text back. `local_llm.rs`'s unit
//! tests cover the *decision* (which strategy, tools or not) but never make
//! a request.
//!
//! This test closes that gap end to end against a real HTTP server: an
//! axum app speaking the OpenAI chat/completions shape, bound to an
//! ephemeral loopback port, that **echoes back the system prompt it
//! received**. If the system prompt were dropped, flattened away, or the
//! response never parsed, the assertion fails.
//!
//! ## Why this lives in its own integration-test binary
//!
//! `get_inference_engine` caches the engine in a process-global
//! `OnceLock` (and latches a process-lifetime "unavailable" flag on a
//! failed init) so the gateway never loads two copies of a GGUF. That
//! makes the engine effectively single-configuration per process: a second
//! test in this binary configuring a *different* home directory would
//! silently reuse the first one's engine and prove nothing. One test, one
//! process — the honest arrangement rather than a mutex that hides the
//! constraint.

use std::net::SocketAddr;

use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

/// Marker only the system prompt carries. If it comes back, the system
/// prompt made it through the whole chain.
const SYSTEM_MARKER: &str = "DUDUCLAW-SYSTEM-PROMPT-4f7a2b";
/// Marker only the user prompt carries.
const USER_MARKER: &str = "DUDUCLAW-USER-PROMPT-91c3de";

/// Minimal OpenAI-compatible `/v1/chat/completions` that echoes what it was
/// asked, so the assertion is about the round trip and not about a model.
async fn chat_completions(Json(body): Json<Value>) -> Json<Value> {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let joined = messages
        .iter()
        .map(|m| {
            format!(
                "{}:{}",
                m["role"].as_str().unwrap_or("?"),
                m["content"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");

    Json(json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": body["model"].as_str().unwrap_or("mock"),
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": format!("ECHO[{joined}]") },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 }
    }))
}

/// `/v1/models` — the endpoint's liveness probe. `OpenAiCompatBackend`
/// health-checks with it before the engine reports itself available, and
/// `inference.local.status` reads the loaded model name from it, so a mock
/// that omitted it would not be OpenAI-compatible in the way this code path
/// actually depends on.
async fn models() -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": [{ "id": "mock-local", "object": "model", "owned_by": "test" }]
    }))
}

/// Bind the mock on an ephemeral port and return its base URL.
async fn spawn_mock_server() -> String {
    let app = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions));
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

#[tokio::test]
async fn local_inference_round_trips_the_agent_system_prompt() {
    let base_url = spawn_mock_server().await;

    // A home dir whose inference.toml points the engine at the mock.
    // `local_tools = false` keeps this on the bare-completion path: the
    // tool loop would try to spawn a real MCP server subprocess, which is
    // a different feature with its own tests and no business in this one.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join("inference.toml"),
        format!(
            r#"
enabled = true

[openai_compat]
base_url = "{base_url}"
model = "mock-local"

[router]
local_tools = false
"#
        ),
    )
    .expect("write inference.toml");

    let out = duduclaw_gateway::claude_runner::try_local_inference(
        home.path(),
        USER_MARKER,
        SYSTEM_MARKER,
        None,
        None,
        None,
    )
    .await
    .expect("local inference should answer through the OpenAI-compat endpoint");

    // The system prompt reached the endpoint as a system message...
    assert!(
        out.contains(SYSTEM_MARKER),
        "system prompt did not reach the local endpoint: {out}"
    );
    assert!(
        out.contains("system:"),
        "system prompt was not sent with the system role: {out}"
    );
    // ...alongside the user prompt, and the reply text came back parsed.
    assert!(
        out.contains(USER_MARKER),
        "user prompt did not reach the local endpoint: {out}"
    );
    assert!(out.starts_with("ECHO["), "response text was not returned verbatim: {out}");
}
