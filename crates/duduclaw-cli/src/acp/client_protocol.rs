//! Agent Client Protocol (ACP) v1 server — the editor-facing protocol used by
//! Zed / JetBrains / nvim agent panels (agentclientprotocol.com).
//!
//! NOT to be confused with the A2A protocol served by `duduclaw acp-server`
//! (`super::server`) — the two share the "ACP" acronym by historical accident.
//! This module implements the real client protocol surface:
//!
//! - `initialize` — version negotiation + capability/auth-method advertisement
//! - `authenticate` — re-checks home readiness (actual setup happens in a
//!   terminal via `duduclaw onboard`; there is no secret exchange here)
//! - `session/new` — creates a session bound to the Main-role agent
//!   (`AUTH_REQUIRED` (-32000) when the DuDuClaw home isn't configured)
//! - `session/prompt` — runs the turn through the gateway reply pipeline
//!   (`build_reply_for_agent`: session memory, contract enforcement, the works)
//!   and streams `session/update` notifications (tool_call / plan /
//!   agent_message_chunk) before answering with a `stopReason`
//! - `session/cancel` (notification) — cancels the in-flight turn; the pending
//!   `session/prompt` responds `{"stopReason": "cancelled"}` as the spec
//!   requires
//!
//! Wire format: newline-delimited JSON-RPC 2.0 over stdio (one message per
//! line, no embedded newlines) — verified against the official schema repo
//! (`zed-industries/agent-client-protocol`, schema/v1). Protocol version: 1.
//!
//! Deliberate v1 scope cuts (all spec-legal): `loadSession: false` (no
//! `session/load`), prompt capabilities `image/audio/embeddedContext: false`
//! (text-only prompts), no MCP server pass-through (`mcpServers` accepted and
//! ignored — DuDuClaw agents already carry their own MCP tool surface), no
//! client fs/terminal calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::channel_reply::{
    ChannelStatusMap, ProgressEvent, ReplyContext, StepPhase, build_reply_for_agent,
};

use super::server::{jsonrpc_error, jsonrpc_response};

/// The ACP protocol version this server speaks (spec `ProtocolVersion::V1`).
const PROTOCOL_VERSION: u64 = 1;

/// ACP-reserved error code: authentication required (spec `ErrorCode::AuthRequired`).
const AUTH_REQUIRED: i64 = -32000;

/// The single auth method we advertise. Setup happens out-of-band in a
/// terminal; `authenticate` with this id simply re-checks readiness.
const AUTH_METHOD_ID: &str = "duduclaw-onboard";

/// Stable per-client user id fed to the prediction engine (ACP clients don't
/// carry a per-user identity the way messaging channels do).
const ACP_USER_ID: &str = "acp-client";

/// Guidance shown whenever the home isn't ready to serve sessions.
const AUTH_GUIDANCE: &str = "DuDuClaw home is not configured yet. Run `duduclaw onboard` in a \
     terminal (or start `duduclaw gateway` and finish setup in the dashboard), then retry.";

// ── Server state ────────────────────────────────────────────

struct SessionEntry {
    /// Resolved target agent (the Main-role agent at `session/new` time).
    agent: String,
    /// Cancels the in-flight prompt turn, if any. Replaced at each turn start;
    /// `session/cancel` takes and fires it.
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    /// True while a `session/prompt` turn is running (one turn at a time).
    in_flight: bool,
}

struct ServerState {
    home_dir: PathBuf,
    initialized: std::sync::atomic::AtomicBool,
    sessions: Mutex<HashMap<String, SessionEntry>>,
    /// Reply pipeline context, built lazily on the first configured session.
    ctx: tokio::sync::OnceCell<Arc<ReplyContext>>,
    /// Monotonic tool-call id counter (unique across the whole connection).
    tool_call_seq: AtomicU64,
}

impl ServerState {
    fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::SeqCst)
    }
}

/// Home readiness: can we resolve a Main-role agent to serve sessions?
///
/// Maps every "not set up" shape (missing home / missing agents dir / no
/// Main-role agent) to `AUTH_REQUIRED`; genuine scan failures on an existing
/// agents dir stay internal errors (-32603) so misconfiguration isn't
/// misreported as "please onboard".
async fn resolve_ready_agent(home_dir: &Path) -> std::result::Result<String, (i64, String)> {
    if !home_dir.join("agents").is_dir() {
        return Err((AUTH_REQUIRED, AUTH_GUIDANCE.to_string()));
    }
    match super::handlers::resolve_send_target(home_dir, "default").await {
        Ok(agent) => Ok(agent),
        // -32602 here means "no Main-role agent" → onboarding incomplete.
        Err((-32602, _)) => Err((AUTH_REQUIRED, AUTH_GUIDANCE.to_string())),
        Err((code, msg)) => Err((code, msg)),
    }
}

/// Build (once) the reply-pipeline context. Mirrors the gateway's own
/// construction: same sessions.db (SQLite WAL — safe alongside a running
/// gateway), same registry shape.
async fn reply_ctx(state: &ServerState) -> std::result::Result<Arc<ReplyContext>, String> {
    state
        .ctx
        .get_or_try_init(|| async {
            let mut registry =
                duduclaw_agent::registry::AgentRegistry::new(state.home_dir.join("agents"));
            registry
                .scan()
                .await
                .map_err(|e| format!("agent registry scan failed: {e}"))?;
            let registry = Arc::new(tokio::sync::RwLock::new(registry));
            let sessions = Arc::new(
                duduclaw_gateway::session::SessionManager::new(
                    &state.home_dir.join("sessions.db"),
                )
                .map_err(|e| format!("session store init failed: {e}"))?,
            );
            let status: ChannelStatusMap =
                Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
            let (tx, _rx) = tokio::sync::broadcast::channel(16);
            Ok::<_, String>(Arc::new(ReplyContext::new(
                registry,
                state.home_dir.clone(),
                sessions,
                status,
                tx,
            )))
        })
        .await
        .cloned()
}

// ── Wire helpers ────────────────────────────────────────────

fn session_update(session_id: &str, update: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": { "sessionId": session_id, "update": update }
    })
}

/// Map a DuDuClaw/Claude tool name onto the ACP `ToolKind` vocabulary.
fn tool_kind(tool: &str) -> &'static str {
    match tool {
        "Read" | "Grep" | "Glob" | "NotebookRead" => "read",
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "edit",
        "Bash" => "execute",
        "WebSearch" | "WebFetch" => "fetch",
        "TodoWrite" | "Task" => "think",
        t if t.starts_with("memory_") || t.ends_with("_list") || t.ends_with("_read") => "read",
        _ => "other",
    }
}

/// Normalize a DuDuClaw todo status onto the ACP plan-entry vocabulary
/// (unknown values render as pending, matching the dashboard's behaviour).
fn plan_status(status: &str) -> &'static str {
    match status {
        "in_progress" => "in_progress",
        "completed" => "completed",
        _ => "pending",
    }
}

/// Concatenate the text content blocks of an ACP prompt array. Non-text
/// blocks are skipped (we advertise text-only prompt capabilities).
fn extract_prompt_text(prompt: &[Value]) -> String {
    let mut out = String::new();
    for block in prompt {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

// ── Method handlers ─────────────────────────────────────────

fn handle_initialize(state: &ServerState, id: &Value, params: &Value) -> Value {
    let Some(client_version) = params.get("protocolVersion").and_then(|v| v.as_u64()) else {
        return jsonrpc_error(id, -32602, "Missing required parameter: protocolVersion");
    };
    state.initialized.store(true, Ordering::SeqCst);
    // Version negotiation per spec: echo the client's version when we support
    // it, otherwise answer with our latest and let the client decide.
    let negotiated = if client_version == PROTOCOL_VERSION {
        client_version
    } else {
        PROTOCOL_VERSION
    };
    jsonrpc_response(
        id,
        json!({
            "protocolVersion": negotiated,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false
                }
            },
            "authMethods": [{
                "id": AUTH_METHOD_ID,
                "name": "Set up DuDuClaw in a terminal",
                "description": "Run `duduclaw onboard` in a terminal (or `duduclaw gateway` + \
                                the dashboard) to configure your DuDuClaw home, then retry."
            }],
            "agentInfo": {
                "name": "duduclaw",
                "title": "DuDuClaw",
                "version": duduclaw_gateway::updater::current_version()
            }
        }),
    )
}

async fn handle_authenticate(state: &ServerState, id: &Value, params: &Value) -> Value {
    let method_id = params.get("methodId").and_then(|v| v.as_str()).unwrap_or("");
    if method_id != AUTH_METHOD_ID {
        return jsonrpc_error(id, -32602, &format!("Unknown auth method: {method_id}"));
    }
    match resolve_ready_agent(&state.home_dir).await {
        Ok(_) => jsonrpc_response(id, json!({})),
        Err((code, msg)) => jsonrpc_error(id, code, &msg),
    }
}

async fn handle_session_new(state: &ServerState, id: &Value, params: &Value) -> Value {
    // Spec-required params. We don't chdir (agents run in their own agent
    // dirs) and we don't spawn client-supplied MCP servers, but both fields
    // must be present for the request to be well-formed.
    if params.get("cwd").and_then(|v| v.as_str()).is_none() {
        return jsonrpc_error(id, -32602, "Missing required parameter: cwd");
    }
    if params.get("mcpServers").and_then(|v| v.as_array()).is_none() {
        return jsonrpc_error(id, -32602, "Missing required parameter: mcpServers");
    }
    let agent = match resolve_ready_agent(&state.home_dir).await {
        Ok(a) => a,
        Err((code, msg)) => return jsonrpc_error(id, code, &msg),
    };
    // Warm the reply context now so the first prompt doesn't pay the scan.
    if let Err(e) = reply_ctx(state).await {
        return jsonrpc_error(id, -32603, &e);
    }
    let session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
    state.sessions.lock().await.insert(
        session_id.clone(),
        SessionEntry { agent, cancel: None, in_flight: false },
    );
    jsonrpc_response(id, json!({ "sessionId": session_id }))
}

/// Run one `session/prompt` turn. Spawned as a task so the reader loop keeps
/// consuming stdin (a `session/cancel` must be able to interrupt us).
async fn run_prompt_turn(
    state: Arc<ServerState>,
    out: mpsc::UnboundedSender<Value>,
    id: Value,
    session_id: String,
    agent: String,
    text: String,
    cancel: tokio::sync::oneshot::Receiver<()>,
) {
    let ctx = match reply_ctx(&state).await {
        Ok(c) => c,
        Err(e) => {
            let _ = out.send(jsonrpc_error(&id, -32603, &e));
            finish_turn(&state, &session_id).await;
            return;
        }
    };

    // Progress → session/update mapping. Step start/end pairs are matched
    // LIFO (same discipline as the dashboard's StepTracker); TodoWrite
    // boards become ACP plans. The callback is sync — send into the writer
    // channel, never block.
    let step_stack: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(vec![]));
    let progress_out = out.clone();
    let progress_session = session_id.clone();
    let seq_state = state.clone();
    let stack = step_stack.clone();
    let on_progress: duduclaw_gateway::channel_reply::ProgressCallback =
        Box::new(move |event: ProgressEvent| match event {
            ProgressEvent::Step(step) => match step.phase {
                StepPhase::Start => {
                    let call_id =
                        format!("call_{}", seq_state.tool_call_seq.fetch_add(1, Ordering::SeqCst));
                    if let Ok(mut s) = stack.lock() {
                        s.push(call_id.clone());
                    }
                    let title = match &step.summary {
                        Some(summary) => format!("{} — {}", step.tool, summary),
                        None => step.tool.clone(),
                    };
                    let _ = progress_out.send(session_update(
                        &progress_session,
                        json!({
                            "sessionUpdate": "tool_call",
                            "toolCallId": call_id,
                            "title": title,
                            "kind": tool_kind(&step.tool),
                            "status": "in_progress"
                        }),
                    ));
                }
                StepPhase::End => {
                    let call_id = stack.lock().ok().and_then(|mut s| s.pop());
                    if let Some(call_id) = call_id {
                        let _ = progress_out.send(session_update(
                            &progress_session,
                            json!({
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": call_id,
                                "status": "completed"
                            }),
                        ));
                    }
                }
            },
            ProgressEvent::TodoUpdate { todos } => {
                let entries: Vec<Value> = todos
                    .iter()
                    .map(|t| {
                        json!({
                            "content": t.content,
                            "priority": "medium",
                            "status": plan_status(&t.status)
                        })
                    })
                    .collect();
                let _ = progress_out.send(session_update(
                    &progress_session,
                    json!({ "sessionUpdate": "plan", "entries": entries }),
                ));
            }
            // Keepalive/ToolUse/ModelInfo: ToolUse duplicates Step for text
            // channels; the rest carry nothing an ACP client renders.
            _ => {}
        });

    // Gateway-side session id: stable per ACP session → multi-turn memory via
    // the same sessions.db the gateway uses. The `acp:` prefix keeps this out
    // of the external-channel branding footer and marks provenance in the
    // dashboard's session views.
    let gateway_session = format!("acp:{session_id}#agent:{agent}");

    let reply_fut = build_reply_for_agent(
        &text,
        &ctx,
        &agent,
        &gateway_session,
        ACP_USER_ID,
        Some(on_progress),
    );
    tokio::pin!(reply_fut);

    let response = tokio::select! {
        reply = &mut reply_fut => {
            if !reply.is_empty() {
                let _ = out.send(session_update(
                    &session_id,
                    json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": reply }
                    }),
                ));
            }
            jsonrpc_response(&id, json!({ "stopReason": "end_turn" }))
        }
        _ = cancel => {
            // Spec: MUST answer the pending prompt with `cancelled` — even
            // though the dropped future may leave the underlying model call
            // to wind down on its own.
            jsonrpc_response(&id, json!({ "stopReason": "cancelled" }))
        }
    };
    let _ = out.send(response);
    finish_turn(&state, &session_id).await;
}

async fn finish_turn(state: &ServerState, session_id: &str) {
    if let Some(entry) = state.sessions.lock().await.get_mut(session_id) {
        entry.in_flight = false;
        entry.cancel = None;
    }
}

async fn handle_session_prompt(
    state: &Arc<ServerState>,
    out: &mpsc::UnboundedSender<Value>,
    id: &Value,
    params: &Value,
) -> Option<Value> {
    let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
        return Some(jsonrpc_error(id, -32602, "Missing required parameter: sessionId"));
    };
    let Some(prompt) = params.get("prompt").and_then(|v| v.as_array()) else {
        return Some(jsonrpc_error(id, -32602, "Missing required parameter: prompt"));
    };
    let text = extract_prompt_text(prompt);
    if text.is_empty() {
        return Some(jsonrpc_error(
            id,
            -32602,
            "Prompt contains no text content (this agent advertises text-only prompts)",
        ));
    }

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let agent = {
        let mut sessions = state.sessions.lock().await;
        let Some(entry) = sessions.get_mut(session_id) else {
            return Some(jsonrpc_error(id, -32602, &format!("Unknown sessionId: {session_id}")));
        };
        if entry.in_flight {
            return Some(jsonrpc_error(
                id,
                -32603,
                "A prompt turn is already in flight for this session",
            ));
        }
        entry.in_flight = true;
        entry.cancel = Some(cancel_tx);
        entry.agent.clone()
    };

    tokio::spawn(run_prompt_turn(
        state.clone(),
        out.clone(),
        id.clone(),
        session_id.to_string(),
        agent,
        text,
        cancel_rx,
    ));
    None // response is produced by the spawned turn
}

async fn handle_session_cancel(state: &ServerState, params: &Value) {
    let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(entry) = state.sessions.lock().await.get_mut(session_id) {
        if let Some(tx) = entry.cancel.take() {
            let _ = tx.send(());
        }
    }
}

// ── Main server loop ────────────────────────────────────────

/// Run the Agent Client Protocol server: newline-delimited JSON-RPC 2.0 over
/// stdio. Blocks until stdin closes (the editor disconnects).
pub async fn run_acp_client_protocol(home_dir: &Path) -> Result<()> {
    info!("Starting DuDuClaw ACP server (Agent Client Protocol v1 over stdio)");

    let state = Arc::new(ServerState {
        home_dir: home_dir.to_path_buf(),
        initialized: std::sync::atomic::AtomicBool::new(false),
        sessions: Mutex::new(HashMap::new()),
        ctx: tokio::sync::OnceCell::new(),
        tool_call_seq: AtomicU64::new(0),
    });

    // Single writer task: everything (responses + notifications, from the
    // reader loop and spawned prompt turns alike) funnels through one channel
    // so lines never interleave.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = out_rx.recv().await {
            let Ok(mut line) = serde_json::to_string(&msg) else { continue };
            line.push('\n');
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader
            .read_line(&mut line)
            .await
            .map_err(|e| DuDuClawError::Gateway(format!("Failed to read from stdin: {e}")))?;
        if bytes_read == 0 {
            info!("ACP server: stdin closed, shutting down");
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                warn!("ACP server: invalid JSON: {e}");
                let _ = out_tx.send(jsonrpc_error(&Value::Null, -32700, "Parse error"));
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let is_notification = id.is_none() || matches!(id, Some(Value::Null));

        // Notifications (no id) never get a response.
        if is_notification {
            match method {
                "session/cancel" => handle_session_cancel(&state, &params).await,
                // Protocol-level `$/…` notifications may be ignored by spec.
                _ => {}
            }
            continue;
        }
        let id = id.unwrap_or(Value::Null);

        // Everything except initialize requires the handshake first.
        if method != "initialize" && !state.is_initialized() {
            let _ = out_tx.send(jsonrpc_error(
                &id,
                -32600,
                "initialize must be called before any other method",
            ));
            continue;
        }

        let response = match method {
            "initialize" => Some(handle_initialize(&state, &id, &params)),
            "authenticate" => Some(handle_authenticate(&state, &id, &params).await),
            "session/new" => Some(handle_session_new(&state, &id, &params).await),
            "session/prompt" => handle_session_prompt(&state, &out_tx, &id, &params).await,
            _ => Some(jsonrpc_error(&id, -32601, &format!("Method not found: {method}"))),
        };
        if let Some(response) = response {
            let _ = out_tx.send(response);
        }
    }

    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_extraction_joins_text_blocks_and_skips_others() {
        let prompt = vec![
            json!({"type": "text", "text": "第一段"}),
            json!({"type": "image", "data": "…", "mimeType": "image/png"}),
            json!({"type": "text", "text": "second"}),
        ];
        assert_eq!(extract_prompt_text(&prompt), "第一段\nsecond");
        assert_eq!(extract_prompt_text(&[]), "");
        // Resource blocks (embeddedContext) are skipped — we advertise
        // `embeddedContext: false`.
        let resource_only = vec![json!({"type": "resource", "resource": {"uri": "file:///x"}})];
        assert_eq!(extract_prompt_text(&resource_only), "");
    }

    #[test]
    fn tool_kind_maps_common_tools_and_defaults_to_other() {
        assert_eq!(tool_kind("Read"), "read");
        assert_eq!(tool_kind("Edit"), "edit");
        assert_eq!(tool_kind("Bash"), "execute");
        assert_eq!(tool_kind("WebFetch"), "fetch");
        assert_eq!(tool_kind("memory_search"), "read");
        assert_eq!(tool_kind("shared_wiki_write"), "other");
    }

    #[test]
    fn plan_status_normalizes_unknown_to_pending() {
        assert_eq!(plan_status("in_progress"), "in_progress");
        assert_eq!(plan_status("completed"), "completed");
        assert_eq!(plan_status("weird"), "pending");
    }

    #[tokio::test]
    async fn unconfigured_home_maps_to_auth_required() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_ready_agent(dir.path()).await.unwrap_err();
        assert_eq!(err.0, AUTH_REQUIRED);
        assert!(err.1.contains("duduclaw onboard"));
    }

    #[tokio::test]
    async fn agents_dir_without_main_agent_maps_to_auth_required() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        let err = resolve_ready_agent(dir.path()).await.unwrap_err();
        assert_eq!(err.0, AUTH_REQUIRED);
    }

    #[test]
    fn initialize_negotiates_version_and_advertises_auth_method() {
        let state = ServerState {
            home_dir: PathBuf::from("/nonexistent"),
            initialized: std::sync::atomic::AtomicBool::new(false),
            sessions: Mutex::new(HashMap::new()),
            ctx: tokio::sync::OnceCell::new(),
            tool_call_seq: AtomicU64::new(0),
        };
        // Matching version is echoed.
        let resp = handle_initialize(&state, &json!(1), &json!({"protocolVersion": 1}));
        assert_eq!(resp["result"]["protocolVersion"], 1);
        assert!(state.is_initialized());
        // Newer client version → we answer with our latest (1).
        let resp = handle_initialize(&state, &json!(2), &json!({"protocolVersion": 99}));
        assert_eq!(resp["result"]["protocolVersion"], 1);
        // Auth method advertised with the documented id.
        assert_eq!(resp["result"]["authMethods"][0]["id"], AUTH_METHOD_ID);
        // Capabilities are honest: no session loading, text-only prompts.
        assert_eq!(resp["result"]["agentCapabilities"]["loadSession"], false);
        assert_eq!(
            resp["result"]["agentCapabilities"]["promptCapabilities"]["image"],
            false
        );
        // Missing protocolVersion is a hard param error.
        let resp = handle_initialize(&state, &json!(3), &json!({}));
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn session_update_notification_shape_matches_spec() {
        let n = session_update(
            "sess_x",
            json!({"sessionUpdate": "agent_message_chunk",
                   "content": {"type": "text", "text": "hi"}}),
        );
        assert_eq!(n["method"], "session/update");
        assert_eq!(n["params"]["sessionId"], "sess_x");
        assert_eq!(n["params"]["update"]["sessionUpdate"], "agent_message_chunk");
        assert!(n.get("id").is_none(), "notifications carry no id");
    }
}
