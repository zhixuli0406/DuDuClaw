//! Privacy/security regression suite (P3-5) — property (a):
//!
//!   **os_native=false ⇒ every `os_*` MCP tool is denied fail-closed.**
//!
//! This is the "external yardstick" the P3-5 work package asks for: a
//! standalone integration test that survives independently of
//! `crates/duduclaw-cli/src/mcp_dispatch.rs`'s own inline `#[cfg(test)]`
//! module (which is where the *implementation* keeps its own unit tests —
//! see `os_tool_denied_when_os_native_absent` / `os_tool_denied_when_os_native_false`
//! / `os_p2_4_sensing_tools_denied_when_os_native_absent` in that file). A
//! future refactor could delete/weaken the source file's own tests together
//! with the gate; this file is deliberately a *different* file, driving the
//! exact same public `McpDispatcher::dispatch_tool_call` entry point real
//! transports use, so a regression here fails a build that never touched
//! `mcp_dispatch.rs` at all.
//!
//! Design doc: `commercial/docs/TODO-os-native-agent.md` P3-5;
//! `commercial/docs/research-os-native-agent-methodology.md` §5.1
//! ("Release Governance" control point — regression tests must catch a
//! privacy property regressing across an update).
//!
//! Run: `cargo test -p duduclaw-cli --test privacy_regression_os_native_gate`
//! (fully offline/deterministic — no network, no credentials, no live agent).

use std::sync::Arc;

use duduclaw_cli::mcp_auth::{Principal, Scope};
use duduclaw_cli::mcp_dispatch::McpDispatcher;
use duduclaw_cli::mcp_memory_quota::DailyQuota;
use duduclaw_cli::mcp_namespace::NamespaceContext;
use duduclaw_cli::mcp_rate_limit::RateLimiter;
use duduclaw_cli::odoo_pool::OdooConnectorPool;
use serde_json::Value;

/// The `os_*` MCP tool surface gated by `[capabilities] os_native` (mirrors
/// `OS_NATIVE_TOOLS` in `mcp_dispatch.rs`, kept as an independent literal list
/// on purpose — this suite is the human-authored expectation, not a re-export
/// of the implementation's own constant. If a new `os_*` tool is added to
/// production and NOT added here, this suite stays green but silently loses
/// coverage; that's an acceptable trade-off for an external, decoupled yardstick
/// and is called out in `evals/_privacy/README.md`).
const OS_NATIVE_TOOL_SURFACE: &[&str] = &[
    "os_notify",
    "os_watch_status",
    "os_open",
    "os_frontmost",
    "os_spotlight_search",
    "os_calendar_today",
];

/// JSON-RPC error code the dispatcher's security pipeline uses for every
/// deny-by-default rejection (scope / policy / namespace / capability gates).
const DENY_CODE: i64 = -32003;

fn make_principal(scopes: Vec<Scope>) -> Principal {
    Principal {
        client_id: "test-client".to_string(),
        scopes: scopes.into_iter().collect(),
        is_external: false,
        created_at: chrono::Utc::now(),
    }
}

fn make_ns_ctx() -> NamespaceContext {
    NamespaceContext {
        write_namespace: "internal/test-client".to_string(),
        read_namespaces: vec![
            "internal/test-client".to_string(),
            "shared/public".to_string(),
        ],
    }
}

fn make_params(tool: &str, args: Value) -> Value {
    serde_json::json!({ "name": tool, "arguments": args })
}

async fn make_dispatcher(tmp: &tempfile::TempDir) -> McpDispatcher {
    let home_dir = tmp.path().to_path_buf();
    let http = reqwest::Client::new();
    let memory_path = home_dir.join("memory.db");
    let memory =
        Arc::new(duduclaw_memory::SqliteMemoryEngine::new(&memory_path).expect("test memory db"));
    let odoo = Arc::new(OdooConnectorPool::default());
    McpDispatcher::new(
        home_dir,
        http,
        memory,
        "dudu".to_string(),
        odoo,
        RateLimiter::new(),
        DailyQuota::new(),
    )
}

/// Minimal args each `os_*` tool needs to get past its own required-field
/// validation and reach the `os_native` gate (the gate runs before any tool
/// handler logic, so these values are never actually acted on).
fn minimal_args_for(tool: &str) -> Value {
    match tool {
        "os_notify" => serde_json::json!({ "title": "t", "body": "b" }),
        "os_open" => serde_json::json!({ "target": "https://example.com" }),
        "os_spotlight_search" => serde_json::json!({ "query": "x" }),
        _ => serde_json::json!({}),
    }
}

/// (a) property, absence case: no `agent.toml` at all ⇒ every `os_*` tool is
/// denied with a message that names `os_native` (fail-closed on missing
/// config — I5 in the project's coding conventions).
#[tokio::test]
async fn all_os_native_tools_denied_when_config_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dispatcher = make_dispatcher(&tmp).await;

    // Admin bypasses the scope check so every call reaches the os_native gate
    // (isolates this suite from Scope-table drift, which is a different
    // property covered by mcp_dispatch's own scope tests).
    let principal = make_principal(vec![Scope::Admin]);
    let ns_ctx = make_ns_ctx();

    for tool in OS_NATIVE_TOOL_SURFACE {
        let params = make_params(tool, minimal_args_for(tool));
        let id = serde_json::json!(1);
        let result = dispatcher
            .dispatch_tool_call(&principal, &ns_ctx, &params, &id)
            .await;

        assert_eq!(
            result["error"]["code"], DENY_CODE,
            "{tool}: must be denied fail-closed when agent.toml is absent, got: {result}"
        );
        let msg = result["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("os_native"),
            "{tool}: denial must name os_native so an operator can self-serve the fix, got: {msg}"
        );
    }
}

/// (a) property, explicit-false case: `[capabilities] os_native = false` must
/// deny exactly like the absent-config case — an operator who once enabled
/// then disabled OS integration must not be silently re-granted it.
#[tokio::test]
async fn all_os_native_tools_denied_when_config_explicitly_false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dispatcher = make_dispatcher(&tmp).await;
    let agent_dir = tmp.path().join("agents").join("test-client");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        "[capabilities]\nos_native = false\n",
    )
    .unwrap();

    let principal = make_principal(vec![Scope::Admin]);
    let ns_ctx = make_ns_ctx();

    for tool in OS_NATIVE_TOOL_SURFACE {
        let params = make_params(tool, minimal_args_for(tool));
        let id = serde_json::json!(2);
        let result = dispatcher
            .dispatch_tool_call(&principal, &ns_ctx, &params, &id)
            .await;

        assert_eq!(
            result["error"]["code"], DENY_CODE,
            "{tool}: must be denied when os_native=false, got: {result}"
        );
        let msg = result["error"]["message"].as_str().unwrap_or("");
        assert!(msg.contains("os_native"), "{tool}: got: {msg}");
    }
}

/// (a) property, malformed-config case: a broken `[capabilities]` TOML table
/// must NOT silently grant OS integration (I5 "missing config / detection
/// error / unenumerated tool must DENY, never fall through to allow").
#[tokio::test]
async fn all_os_native_tools_denied_when_config_malformed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dispatcher = make_dispatcher(&tmp).await;
    let agent_dir = tmp.path().join("agents").join("test-client");
    std::fs::create_dir_all(&agent_dir).unwrap();
    // Malformed TOML: os_native given a string instead of a bool.
    std::fs::write(
        agent_dir.join("agent.toml"),
        "[capabilities]\nos_native = \"yes-please\"\n",
    )
    .unwrap();

    let principal = make_principal(vec![Scope::Admin]);
    let ns_ctx = make_ns_ctx();

    for tool in OS_NATIVE_TOOL_SURFACE {
        let params = make_params(tool, minimal_args_for(tool));
        let id = serde_json::json!(3);
        let result = dispatcher
            .dispatch_tool_call(&principal, &ns_ctx, &params, &id)
            .await;

        assert_eq!(
            result["error"]["code"], DENY_CODE,
            "{tool}: malformed config must fail closed (deny), got: {result}"
        );
    }
}

/// Positive control (guards this suite against a false-green from an
/// unrelated global lockout bug): `os_native = true` lets a read-only sensing
/// tool clear the gate — it may still fail downstream (no display/network in
/// CI), but never with the os_native denial.
#[tokio::test]
async fn os_native_true_clears_the_gate() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dispatcher = make_dispatcher(&tmp).await;
    let agent_dir = tmp.path().join("agents").join("test-client");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        "[capabilities]\nos_native = true\n",
    )
    .unwrap();

    let principal = make_principal(vec![Scope::OsNative, Scope::Admin]);
    let ns_ctx = make_ns_ctx();
    let params = make_params("os_watch_status", serde_json::json!({}));
    let id = serde_json::json!(4);

    let result = dispatcher
        .dispatch_tool_call(&principal, &ns_ctx, &params, &id)
        .await;

    let msg = result["error"]["message"].as_str().unwrap_or("");
    assert!(
        !msg.contains("os_native"),
        "os_native=true must clear the gate, got: {result}"
    );
}
