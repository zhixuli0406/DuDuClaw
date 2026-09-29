//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! WP21 欠帳④ — `agents.create` (the dashboard "create agent" path) must
//! reject [`duduclaw_core::is_reserved_agent_id`] the same way MCP
//! `create_agent` already does (`crates/duduclaw-cli/src/mcp.rs`). An
//! agent claiming a system-sender id (`cron`, `dashboard`, …) would clear
//! every WP21 delegation choke point unconditionally.
use super::*;

#[tokio::test]
async fn rejects_every_reserved_system_sender_id_and_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Every id here is otherwise a *valid* agent id shape (lowercase +
    // hyphens, passes `is_valid_agent_id`) — the only thing that can
    // reject it is the new `is_reserved_agent_id` check.
    for reserved in [
        "cron",
        "dashboard",
        "webhook",
        "heartbeat",
        "autopilot",
        "goal-loop-driver",
        "a2a-client",
        "default",
    ] {
        let frame = handler
            .handle_agents_create(json!({ "name": reserved, "display_name": "X" }))
            .await;
        assert!(
            !matches!(frame, WsFrame::Response { ok: true, .. }),
            "{reserved} must be rejected: {frame:?}"
        );
        assert!(
            !home.path().join("agents").join(reserved).exists(),
            "{reserved} must not create a directory"
        );
    }
}

/// `is_reserved_agent_id` is case-insensitive and also reserves the whole
/// `__…` namespace — both properties are exercised directly against the
/// core predicate here, since `is_valid_agent_id` (lowercase-only, no
/// underscore) would otherwise block these shapes before the reserved
/// check is ever reached, making them untestable through the dashboard
/// RPC alone.
#[test]
fn reserved_check_itself_is_case_insensitive_and_covers_dunder_prefix() {
    assert!(duduclaw_core::is_reserved_agent_id("CRON"));
    assert!(duduclaw_core::is_reserved_agent_id("Dashboard"));
    assert!(duduclaw_core::is_reserved_agent_id("__deferred_gvu__"));
    assert!(duduclaw_core::is_reserved_agent_id("__anything"));
}

/// A non-reserved, otherwise-valid name still succeeds — the new check
/// must not be over-broad.
#[tokio::test]
async fn non_reserved_name_still_succeeds() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_agents_create(json!({ "name": "sales-lead", "display_name": "Sales Lead" }))
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );
    assert!(home.path().join("agents").join("sales-lead").exists());
}

/// The operator's `model_preferred` choice is written verbatim — a created
/// employee no longer silently inherits `claude-sonnet-4-6`. Absent/blank
/// falls back to the default (kept for programmatic MCP/API callers).
#[tokio::test]
async fn create_honors_model_preferred_and_falls_back() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let _ = handler
        .handle_agents_create(json!({
            "name": "picky", "display_name": "Picky", "model_preferred": "opus"
        }))
        .await;
    let picked = std::fs::read_to_string(home.path().join("agents/picky/agent.toml")).unwrap();
    assert!(picked.contains("preferred = \"opus\""), "{picked}");
    assert!(!picked.contains("claude-sonnet-4-6"), "{picked}");

    // No model_preferred → the programmatic fallback still applies.
    let _ = handler
        .handle_agents_create(json!({ "name": "plain", "display_name": "Plain" }))
        .await;
    let fallback =
        std::fs::read_to_string(home.path().join("agents/plain/agent.toml")).unwrap();
    assert!(
        fallback.contains("preferred = \"claude-sonnet-4-6\""),
        "{fallback}"
    );
}
