use super::*;

pub(crate) fn jsonrpc_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

pub(crate) fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

// ── Tool schema builder ──────────────────────────────────────

pub(crate) fn mcp_error(msg: &str) -> Value {
    serde_json::json!({ "content": [{"type": "text", "text": format!("Error: {msg}")}], "isError": true })
}

pub(crate) fn mcp_text(msg: &str) -> Value {
    serde_json::json!({ "content": [{"type": "text", "text": msg}] })
}

// ── Skill management handlers ───────────────────────────────

pub(crate) async fn write_response(stdout: &mut tokio::io::Stdout, response: &Value) -> Result<()> {
    let serialized = serde_json::to_string(response)
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to serialize response: {e}")))?;
    // Redact any API keys that may have leaked into the response payload
    // (e.g. via error messages that echo back tool arguments).
    let redacted = crate::mcp_redact::redact(&serialized);
    let mut output = redacted.into_owned();
    output.push('\n');
    stdout
        .write_all(output.as_bytes())
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to write to stdout: {e}")))?;
    stdout
        .flush()
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to flush stdout: {e}")))?;
    Ok(())
}

/// The `notifications/tools/list_changed` server→client notification (O7).
///
/// A JSON-RPC *notification*: no `id`, no response expected. Emitted by the
/// stdio loop when the caller's visible tool set changes after a `tools/list`
/// has already been answered — before that there is nothing to invalidate.
pub(crate) fn tools_list_changed_notification() -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed"
    })
}

/// How often the stdio loop re-derives the caller's visible tool set to decide
/// whether a `list_changed` is owed.
///
/// A capability change is a human-scale event (an approval clicked, a config
/// saved), so a few seconds of latency is invisible; the check itself is one
/// `agent.toml` read plus — only for agents that actually declare
/// `scoped_tools` — a small grant query.
pub(crate) const TOOLS_LIST_WATCH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(5);

// ── Method handlers ──────────────────────────────────────────

pub(crate) fn handle_initialize(id: &Value, _request: &Value) -> Value {
    jsonrpc_response(
        id,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                // O7: `tools/list` is filtered by the caller's live
                // capabilities, and those change mid-session (a PORTICO grant
                // is minted, an operator edits `agent.toml`, the dashboard
                // calls `agent_update`). Declaring `listChanged` is what makes
                // hiding a tool honest rather than permanent — the stdio loop
                // emits `notifications/tools/list_changed` whenever the
                // caller's visible set actually differs.
                "tools": { "listChanged": true }
            },
            "serverInfo": {
                "name": "duduclaw",
                "version": duduclaw_gateway::updater::current_version()
            }
        }),
    )
}

pub(crate) fn tool_text(text: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    })
}

pub(crate) fn tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}

// ─────────────────────────────────────────────────────────────────
// OS-native Phase 1 tool handlers (os_notify / os_watch_status / os_open).
// The os_native capability, scope, and (for os_open) ActionGuard gates are all
// enforced upstream in mcp_dispatch; these functions are pure mechanism.
// ─────────────────────────────────────────────────────────────────
