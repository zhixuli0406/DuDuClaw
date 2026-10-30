//! `duduclaw mcp-proxy` — a redacting stdio JSON-RPC pass-through for
//! **external** MCP servers (design
//! `commercial/docs/specs/DESIGN-redaction-field-rules-2026-09.md` §13.3 plan A,
//! contract §13.6).
//!
//! ## Why
//!
//! RFC-23 redaction only ever covered DuDuClaw's *own* MCP tools, because the
//! single choke point is `mcp_dispatch.rs` inside `duduclaw mcp-server`. A
//! customer's own MCP servers declared in the agent's `.mcp.json` (a Postgres
//! / MySQL MCP, a 鼎新 ERP bridge, …) are spawned by the Claude CLI directly,
//! so their tool results reached the model completely unredacted — §13.1's
//! "就算把 Odoo 寫死的部分抽掉，自訂資料庫的資料今天也到不了去識別化".
//!
//! This subcommand closes that by sitting between the CLI and the upstream
//! server: the gateway rewrites every non-`duduclaw` stdio entry in the
//! per-spawn `.mcp.json` to launch `duduclaw mcp-proxy --server <name> --
//! <original cmd> [args…]` instead (see
//! `duduclaw-gateway::redaction_proxy`), and this process forwards the
//! newline-delimited JSON-RPC stream in both directions while applying the
//! exact same two redaction operations the in-process choke point applies:
//!
//! - **`tools/call` request** → [`decide_tool_args_with`] on `params.arguments`
//!   (only when the arguments actually carry `<REDACT:…>` tokens). `Deny` is
//!   answered here with a JSON-RPC error and never reaches upstream; `Allow`
//!   forwards the restored arguments; `Passthrough` forwards verbatim.
//! - **matching response** → [`redact_tool_result_with`] on its `result`,
//!   with the call's own `(tool, arguments)` as [`ToolContext`], so structured
//!   field rules (`json_path` / `db_field`) see the same context they would
//!   inside `mcp-server`.
//!
//! Tool names are namespaced `<server>.<tool>` (§13.6) so an operator writes
//! `match_tool = "crm_pg.pg_select"` and cannot accidentally catch a
//! same-named DuDuClaw tool.
//!
//! ## Everything else is opaque
//!
//! `initialize`, `tools/list`, notifications, upstream→client requests
//! (sampling / roots) and **any line that is not parseable JSON** are
//! forwarded byte-for-byte. The proxy is a pass-through first and a redactor
//! second: a protocol feature it does not understand must never be lost.
//!
//! ## Fail-closed
//!
//! [`McpRedactionLayer::try_init`] has exactly the same three outcomes as in
//! `mcp-server`: `Ok(None)` ⇒ redaction disabled, pure pass-through (zero
//! overhead); `Ok(Some(_))` ⇒ redact; `Err(_)` ⇒ the operator asked for
//! redaction and it could not be built, so the proxy refuses to start
//! (non-zero exit) rather than quietly forwarding PII.
//!
//! ## Upstream environment
//!
//! The original `.mcp.json` `env` map for the wrapped server is handed over in
//! the **environment variable** [`PROXY_UPSTREAM_ENV_VAR`] as a JSON object
//! (`{"PGPASSWORD":"…"}`), not as `--env K=V` argv flags: on Linux
//! `/proc/<pid>/cmdline` is world-readable while `/proc/<pid>/environ` is
//! owner-only, and this repo already avoids argv for secrets (BE-C1, the
//! `--system-prompt-file` temp-file pattern). The upstream child inherits this
//! process's environment minus [`PROXY_PRIVATE_ENV`] (the DuDuClaw-internal
//! identity/credential names the rewritten `.mcp.json` entry injects for the
//! proxy itself) plus that map — i.e. exactly the environment the upstream
//! would have received had the Claude CLI launched it directly.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_redaction::EgressDecision;

use crate::mcp_redaction::{
    McpRedactionLayer, decide_tool_args_with, egress_deny_response, redact_tool_result_with,
};
use duduclaw_gateway::third_party_tools::{DispatchPermit, ThirdPartyGate};

/// Env var carrying the upstream server's `.mcp.json` `env` map, JSON-encoded.
/// Written by the gateway's config rewrite, consumed (and removed) here.
pub const PROXY_UPSTREAM_ENV_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV";

/// Env var naming an owner-only file holding the same JSON map, read only
/// when [`PROXY_UPSTREAM_ENV_VAR`] is absent. The Codex runtime uses it
/// because its server definitions travel as `-c` overrides in `argv`
/// (2026-10-19). Must match
/// `duduclaw_core::mcp_proxy_rewrite::PROXY_UPSTREAM_ENV_FILE_VAR`.
pub const PROXY_UPSTREAM_ENV_FILE_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV_FILE";

/// Env var naming the per-spawn directory a `--spawn-scoped` proxy reads its
/// upstream env from (Grok; 2026-10-20). See
/// [`crate::mcp_proxy_spawn_env`].
pub use crate::mcp_proxy_spawn_env::PROXY_UPSTREAM_ENV_DIR_VAR;

/// DuDuClaw-internal names the rewritten `.mcp.json` entry sets **for the
/// proxy process** and which must not be handed down to the third-party
/// upstream server. Everything else is inherited, so the upstream sees the
/// same environment the Claude CLI would have given it.
pub const PROXY_PRIVATE_ENV: &[&str] = &[
    PROXY_UPSTREAM_ENV_VAR,
    PROXY_UPSTREAM_ENV_FILE_VAR,
    PROXY_UPSTREAM_ENV_DIR_VAR,
    "DUDUCLAW_HOME",
    "DUDUCLAW_PORT",
    "DUDUCLAW_INSTANCE",
    "DUDUCLAW_AGENT_ID",
    "DUDUCLAW_AGENT_TOKEN",
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
    "DUDUCLAW_REDACTION_SCOPES",
    "DUDUCLAW_TURN_ID",
    "DUDUCLAW_EGRESS_TURN_ID",
    "DUDUCLAW_SESSION_ID",
    "DUDUCLAW_TURN_USER_MESSAGE_SEQ",
    "DUDUCLAW_TURN_USER_MESSAGE_AT",
    "DUDUCLAW_DISPATCH_SESSION",
    "DUDUCLAW_DISPATCH_RUN_ID",
    "DUDUCLAW_UPSTREAM_UNKNOWN",
    "DUDUCLAW_ON_BEHALF_OF",
    "DUDUCLAW_TASK_ID",

];

// ── Pure frame helpers (unit-tested without any process) ────────────────────

/// Parse one newline-delimited frame. `None` ⇒ the line is **not** JSON and
/// must be forwarded verbatim (§13.6: 非 JSON 行原樣轉發).
pub fn parse_frame(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line).ok()
}

/// Extract `(id, tool_name, arguments)` from a `tools/call` request.
///
/// Returns `None` for every other method, for notifications (no `id`), and
/// for malformed `params`. The proxy loop refuses malformed tool calls;
/// other protocol traffic retains its passthrough behavior.
pub fn tool_call_of(frame: &Value) -> Option<(Value, String, Value)> {
    if frame.get("method").and_then(|m| m.as_str()) != Some("tools/call") {
        return None;
    }
    let id = frame.get("id")?;
    if id.is_null() {
        return None;
    }
    let params = frame.get("params")?;
    let name = params.get("name").and_then(|n| n.as_str())?.to_string();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    Some((id.clone(), name, args))
}

/// Canonical bookkeeping key for a JSON-RPC id. JSON-RPC allows numbers and
/// strings; both are kept distinct (`7` vs `"7"`) by prefixing the kind, so a
/// server that answers string ids cannot collide with a client using numbers.
pub fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::Number(n) => Some(format!("n:{n}")),
        Value::String(s) => Some(format!("s:{s}")),
        _ => None,
    }
}

/// `true` when the frame is a *response* (carries `result` or `error`) rather
/// than a request or notification.
pub fn is_response(frame: &Value) -> bool {
    frame.get("result").is_some() || frame.get("error").is_some()
}

/// `<server>.<tool>` — the namespaced name rules match on (§13.6).
pub fn namespaced_tool(server: &str, tool: &str) -> String {
    format!("{server}.{tool}")
}

/// Return a **new** request frame with `params.arguments` replaced by
/// `args` (immutability convention 1 of `CLAUDE.md`'s coding style: build a
/// copy, never mutate the caller's value in place).
///
/// A frame whose `params` is not an object is returned unchanged — there is
/// nothing to restore into, and dropping the frame would be worse.
pub fn with_restored_args(frame: &Value, args: Value) -> Value {
    let mut out = frame.clone();
    match out.get_mut("params").and_then(|p| p.as_object_mut()) {
        Some(params) => {
            params.insert("arguments".to_string(), args);
            out
        }
        None => out,
    }
}

/// In-flight `tools/call` requests, keyed by [`id_key`], so the response can
/// be redacted with the call's own `(tool, arguments)` context.
const MAX_PENDING_CALLS: usize = 512;
const PENDING_CALL_TTL: Duration = Duration::from_secs(300);
const MAX_ACTIVE_CALLS: usize = 512;
const CORRELATION_PREFIX: &str = "duduclaw-proxy:";

#[derive(Debug)]
struct PendingContext {
    tool: String,
    args: Value,
    generation: String,
    deadline: Instant,
}

#[derive(Debug, Default)]
pub struct PendingCalls {
    inner: HashMap<String, PendingContext>,
}

impl PendingCalls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Never replace a live call's redaction context with a duplicate ID.
    pub fn insert(&mut self, id: &Value, tool: String, args: Value) -> std::result::Result<String, &'static str> {
        let key = id_key(id).filter(|k| k.len() <= 258)
            .ok_or("工具呼叫的識別碼無效，已拒絕。")?;
        let now = Instant::now();
        self.inner.retain(|_, entry| entry.deadline > now);
        if self.inner.contains_key(&key) {
            return Err("工具呼叫的識別碼重複，請等候原呼叫結束後再試。");
        }
        if self.inner.len() >= MAX_PENDING_CALLS {
            return Err("等候中的工具呼叫已達上限，請稍後再試。");
        }
        let generation = uuid::Uuid::new_v4().to_string();
        self.inner.insert(key, PendingContext {
            tool, args, generation: generation.clone(), deadline: now + PENDING_CALL_TTL,
        });
        Ok(generation)
    }

    pub fn take(&mut self, id: &Value) -> Option<(String, Value)> {
        let entry = self.inner.remove(&id_key(id)?)?;
        (entry.deadline > Instant::now()).then_some((entry.tool, entry.args))
    }

    fn remove_generation(&mut self, id: &Value, generation: &str) -> Option<PendingContext> {
        let key = id_key(id)?;
        if self.inner.get(&key).is_some_and(|entry| entry.generation == generation) {
            self.inner.remove(&key)
        } else { None }
    }

    pub fn len(&self) -> usize { self.inner.len() }
    pub fn is_empty(&self) -> bool { self.inner.is_empty() }
}

/// Owns the slot until the upstream write completes. Every local refusal, cancellation,
/// parse error or failed write drops it without touching a newer call.
struct PendingSlot {
    pending: Arc<Mutex<PendingCalls>>,
    id: Value,
    generation: String,
    armed: bool,
}

impl Drop for PendingSlot {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove_generation(&self.id, &self.generation);
            }
        }
    }
}

struct PreparedFrame {
    line: String,
    slot: Option<PendingSlot>,
}

impl PreparedFrame {
    fn queued(self, permit: Option<DispatchPermit>) -> QueuedFrame {
        QueuedFrame { line: self.line, permit, slot: self.slot }
    }
}

struct QueuedFrame {
    line: String,
    permit: Option<DispatchPermit>,
    slot: Option<PendingSlot>,
}

impl QueuedFrame {
    fn dispatch_allowed(&self) -> bool {
        self.permit.as_ref().is_none_or(DispatchPermit::is_live)
    }

    fn forwarded(&mut self) {
        if let Some(slot) = &mut self.slot { slot.armed = false; }
    }
}

// A fresh wire ID makes expired responses distinguishable from unrelated
// protocol traffic and prevents an old response from consuming a reused ID.
fn wire_id(id: &Value, generation: &str) -> Value {
    Value::String(format!("{CORRELATION_PREFIX}{generation}:{id}"))
}

fn correlation(id: &Value) -> Option<(Value, String)> {
    let encoded = id.as_str()?.strip_prefix(CORRELATION_PREFIX)?;
    let (generation, original) = encoded.split_once(':')?;
    uuid::Uuid::parse_str(generation).ok()?;
    let original: Value = serde_json::from_str(original).ok()?;
    id_key(&original)?;
    Some((original, generation.into()))
}

/// Reap without waiting; returning false never queues another held call.
fn call_capacity(held: &mut tokio::task::JoinSet<()>) -> bool {
    while held.try_join_next().is_some() {}
    held.len() < MAX_ACTIVE_CALLS
}

/// Split the upstream environment: this process's env minus
/// [`PROXY_PRIVATE_ENV`], with the JSON map from [`PROXY_UPSTREAM_ENV_VAR`]
/// layered on top.
///
/// A malformed / absent blob simply contributes nothing — it can never abort
/// the spawn, because the upstream's own `.mcp.json` env is a convenience,
/// not a security control (the security control is what the *proxy* does to
/// the payloads).
pub fn upstream_env(
    inherited: impl IntoIterator<Item = (String, String)>,
    encoded: Option<&str>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = inherited
        .into_iter()
        .filter(|(k, _)| !PROXY_PRIVATE_ENV.contains(&k.as_str()))
        .collect();

    let declared: Vec<(String, String)> = encoded
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    for (k, v) in declared {
        match out.iter_mut().find(|(ek, _)| *ek == k) {
            Some(slot) => slot.1 = v,
            None => out.push((k, v)),
        }
    }
    out
}

/// The upstream env blob: the inline [`PROXY_UPSTREAM_ENV_VAR`] value when
/// set, otherwise the contents of the file [`PROXY_UPSTREAM_ENV_FILE_VAR`]
/// names. An unreadable file contributes nothing (logged), like a malformed
/// blob in [`upstream_env`].
pub fn upstream_env_blob(
    inline: Option<String>,
    file: Option<String>,
    server: &str,
) -> Option<String> {
    if inline.is_some() {
        return inline;
    }
    let path = file.filter(|p| !p.trim().is_empty())?;
    match std::fs::read_to_string(&path) {
        Ok(raw) => Some(raw),
        Err(e) => {
            tracing::warn!(
                server = %server,
                error = %e,
                "mcp-proxy: upstream env file unreadable — the upstream starts without its declared env"
            );
            None
        }
    }
}

// ── The proxy loop ──────────────────────────────────────────────────────────

/// The four streams the proxy sits between. Boxed (rather than generic) so
/// the loop can be driven by in-memory duplex pipes in unit tests and by real
/// stdio + a child process in production, from one code path.
pub struct ProxyIo {
    pub client_in: Box<dyn AsyncRead + Unpin + Send>,
    pub client_out: Box<dyn AsyncWrite + Unpin + Send>,
    pub upstream_in: Box<dyn AsyncWrite + Unpin + Send>,
    pub upstream_out: Box<dyn AsyncRead + Unpin + Send>,
}

/// Drive the bidirectional pass-through until both directions are done.
///
/// `layer` `None` ⇒ pure pass-through (redaction disabled).
pub async fn run_proxy_io(
    server: &str,
    layer: Option<Arc<McpRedactionLayer>>,
    io: ProxyIo,
) -> std::io::Result<()> {
    run_proxy_io_gated(server, layer, None, io).await
}

/// [`run_proxy_io`] with the third-party tool gate (2026-10-08,
/// `duduclaw_gateway::third_party_tools`): `tools/list` answers are filtered
/// and annotated, `tools/call` requests are refused or held for approval
/// before they reach the upstream. `gate` `None` ⇒ no gate (the upstream is
/// a remote bridge, which gates itself).
pub async fn run_proxy_io_gated(
    server: &str,
    layer: Option<Arc<McpRedactionLayer>>,
    gate: Option<Arc<ThirdPartyGate>>,
    io: ProxyIo,
) -> std::io::Result<()> {
    let ProxyIo {
        client_in,
        mut client_out,
        mut upstream_in,
        upstream_out,
    } = io;

    // One writer owns `client_out`, because two producers write to it: the
    // upstream→client pump, and the answers the client→upstream pump gives
    // itself (redaction denials, refused tools).
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(mut line) = rx.recv().await {
            line.push('\n');
            if client_out.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if client_out.flush().await.is_err() {
                break;
            }
        }
    });

    // One writer owns `upstream_in` too: a call held for approval is
    // forwarded later, from its own task.
    let pending = Arc::new(Mutex::new(PendingCalls::new()));
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<QueuedFrame>();
    let writer_gate = gate.clone();
    let writer_pending = pending.clone();
    let up_writer = tokio::spawn(async move {
        while let Some(mut frame) = up_rx.recv().await {
            if write_queued_frame(&mut upstream_in, &mut frame).await.is_err() {
                if let Ok(mut pending) = writer_pending.lock() { pending.inner.clear(); }
                if let Some(gate) = &writer_gate { gate.close_pending(); }
                break;
            }
        }
        // Dropping `upstream_in` closes the upstream's stdin.
    });

    let list_ids = Arc::new(std::sync::Mutex::new(std::collections::HashSet::<String>::new()));

    // ── upstream → client ────────────────────────────────────────────
    let up_task = {
        let pending = pending.clone();
        let layer = layer.clone();
        let gate = gate.clone();
        let list_ids = list_ids.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(upstream_out).lines();
            // `lines()` grows its buffer as needed — no fixed cap, so a large
            // tool result (a 4 MiB row dump) is never truncated mid-frame.
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let out = match parse_frame(&line) {
                    // Non-JSON: opaque, forward verbatim.
                    None => line,
                    Some(mut frame) => {
                        let mut line = line;
                        let upstream_id = frame.get("id").cloned();
                        let context = if layer.is_some() {
                            match response_context(&pending, &mut frame) {
                                Ok(context) => context,
                                Err(refusal) => {
                                    if tx.send(refusal).is_err() { break; }
                                    continue;
                                }
                            }
                        } else { None };
                        if let Some(gate) = gate.as_ref() {
                            let client_id = frame.get("id").cloned();
                            // The gate correlates the unique upstream generation,
                            // including when a client ID has since been reused.
                            if context.is_some() {
                                if let Some(id) = upstream_id { frame["id"] = id; }
                            }
                            gate.observe_response(&mut frame).await;
                            if context.is_some() {
                                if let Some(id) = client_id { frame["id"] = id; }
                            }
                            line = frame.to_string();
                        }
                        let (frame, line) = filter_listing(gate.as_deref(), &list_ids, frame, line);
                        rewrite_upstream_frame(layer.as_deref(), frame, line, context)
                    }
                };
                if tx.send(out).is_err() {
                    break;
                }
            }
            if let Ok(mut pending) = pending.lock() { pending.inner.clear(); }
            if let Some(gate) = &gate { gate.close_pending(); }
        })
    };

    // ── client → upstream ────────────────────────────────────────────
    let mut held = tokio::task::JoinSet::new();
    let mut lines = BufReader::new(client_in).lines();
    loop {
        let line = tokio::select! {
            completed = held.join_next(), if !held.is_empty() => {
                let _ = completed;
                continue;
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => line,
                _ => break,
            },
        };
        call_capacity(&mut held);
        if line.trim().is_empty() {
            continue;
        }
        let Some(frame) = parse_frame(&line) else {
            // Opaque line — forward verbatim.
            if up_tx.send(QueuedFrame { line, permit: None, slot: None }).is_err() {
                break;
            }
            continue;
        };

        if !frame.is_object()
            || (frame.get("method").and_then(Value::as_str) == Some("tools/call")
                && tool_call_of(&frame).is_none())
        {
            let _ = tx.send(duduclaw_gateway::third_party_tools::refusal_frame(
                &frame.get("id").cloned().unwrap_or(Value::Null),
                "Malformed tools/call cannot pass the egress gate.",
            ));
            continue;
        }
        if let Some(gate) = gate.as_ref() {
            if frame.get("method").and_then(|m| m.as_str()) == Some("tools/list")
                && let Some(k) = frame.get("id").and_then(id_key)
                && let Ok(mut ids) = list_ids.lock()
            {
                ids.insert(k);
            }
            if let Some((id, tool, _)) = tool_call_of(&frame) {
                if !call_capacity(&mut held) {
                    let _ = tx.send(duduclaw_gateway::third_party_tools::refusal_frame(
                        &id, "同時執行的工具呼叫已達上限，請稍後再試。",
                    ));
                    continue;
                }
                if matches!(gate.decide(&tool).1, duduclaw_core::ThirdPartyDecision::Block(_)) {
                    let msg = gate.check_call(&tool).await.err().unwrap_or_default();
                    let _ = tx.send(duduclaw_gateway::third_party_tools::refusal_frame(&id, &msg));
                    continue;
                }
                // Restore before approval so the card and digest describe the
                // exact upstream payload. Waiting must not stall other replies.
                let Some(payload) = prepare_upstream(server, layer.as_deref(), &pending, &tx, &frame, &line).await else { continue; };
                let effective: Value = match serde_json::from_str(&payload.line) { Ok(v) => v, Err(_) => continue };
                let args = effective["params"]["arguments"].clone();
                let gate_id = effective["id"].clone();
                let (gate, tx, up_tx) = (gate.clone(), tx.clone(), up_tx.clone());
                held.spawn(async move {
                    match gate.begin_call(&gate_id, &tool, &args).await {
                        Ok(call) => {
                            if up_tx.send(payload.queued(Some(call.dispatch_permit()))).is_ok() {
                                call.wait().await;
                            } else {
                                call.finish_empty().await;
                            }
                        }
                        Err(msg) => {
                            let _ = tx.send(duduclaw_gateway::third_party_tools::refusal_frame(&id, &msg));
                        }
                    }
                });
                continue;
            }
        }

        let Some(payload) = prepare_upstream(server, layer.as_deref(), &pending, &tx, &frame, &line).await
        else {
            continue;
        };
        if up_tx.send(payload.queued(None)).is_err() {
            break;
        }
    }

    // Client hung up (or the pipe broke): let held calls finish (an
    // approval resolves or expires), close the upstream's stdin so it shuts
    // down, then drain whatever it still has to say.
    while held.join_next().await.is_some() {}
    drop(up_tx);
    let _ = up_writer.await;
    drop(tx);
    let _ = up_task.await;
    let _ = writer.await;
    Ok(())
}

/// Apply the redaction egress decision to one client frame and return the
/// line to send upstream, or `None` when the proxy answered it itself.
async fn prepare_upstream(
    server: &str,
    layer: Option<&McpRedactionLayer>,
    pending: &Arc<Mutex<PendingCalls>>,
    tx: &mpsc::UnboundedSender<String>,
    frame: &Value,
    line: &str,
) -> Option<PreparedFrame> {
    let (Some(layer), Some((id, tool, args))) = (layer, tool_call_of(frame)) else {
        // Propagate cancellation using the same wire ID as the tool request.
        if frame.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
            if let Some(id) = frame.pointer("/params/requestId") {
                if let Some(generation) = pending.lock().ok().and_then(|p| {
                    p.inner.get(&id_key(id)?).map(|entry| entry.generation.clone())
                }) {
                    let mut frame = frame.clone();
                    frame["params"]["requestId"] = wire_id(id, &generation);
                    return Some(PreparedFrame { line: frame.to_string(), slot: None });
                }
            }
        }
        return Some(PreparedFrame { line: line.into(), slot: None });
    };
    let ns = namespaced_tool(server, &tool);
    let restored = match decide_args(layer, &ns, &args) {
        ArgDecision::Deny { reason, tokens_seen } => {
            let _ = tx.send(egress_deny_response(&id, &ns, &reason, tokens_seen).to_string());
            return None;
        }
        ArgDecision::Forward(restored) => restored,
    };
    let effective_args = restored.clone().unwrap_or_else(|| args.clone());
    let generation = match pending.lock().map_err(|_| "工具呼叫狀態無法讀取，已拒絕。")
        .and_then(|mut p| p.insert(&id, ns, effective_args))
    {
        Ok(generation) => generation,
        Err(reason) => {
            let _ = tx.send(duduclaw_gateway::third_party_tools::refusal_frame(&id, reason));
            return None;
        }
    };
    let mut frame = match restored {
        Some(args) => with_restored_args(frame, args),
        None => frame.clone(),
    };
    frame["id"] = wire_id(&id, &generation);
    Some(PreparedFrame {
        line: frame.to_string(),
        slot: Some(PendingSlot { pending: pending.clone(), id, generation, armed: true }),
    })
}

/// When `frame` answers a `tools/list` this proxy saw, let the gate hide the
/// tools the employee may not call and record annotations. Returns the frame
/// and the line to pass on.
fn filter_listing(
    gate: Option<&ThirdPartyGate>,
    list_ids: &std::sync::Mutex<std::collections::HashSet<String>>,
    mut frame: Value,
    line: String,
) -> (Value, String) {
    let Some(gate) = gate else { return (frame, line) };
    if !is_response(&frame) {
        return (frame, line);
    }
    let Some(k) = frame.get("id").and_then(id_key) else {
        return (frame, line);
    };
    let ours = list_ids.lock().map(|mut ids| ids.remove(&k)).unwrap_or(false);
    if !ours {
        return (frame, line);
    }
    match frame.get_mut("result") {
        Some(result) => {
            gate.filter_list_result(result);
            let line = frame.to_string();
            (frame, line)
        }
        None => (frame, line),
    }
}

/// What to do with a `tools/call`'s arguments.
enum ArgDecision {
    /// Forward; `Some(args)` when the tokens were restored to real values.
    Forward(Option<Value>),
    Deny {
        reason: String,
        tokens_seen: usize,
    },
}

fn decide_args(layer: &McpRedactionLayer, ns_tool: &str, args: &Value) -> ArgDecision {
    // Hot path: no token-shaped substring anywhere ⇒ nothing to decide.
    if !McpRedactionLayer::args_contain_tokens(args) {
        return ArgDecision::Forward(None);
    }
    match decide_tool_args_with(
        &layer.manager,
        ns_tool,
        args,
        &layer.agent_id,
        &layer.session_id,
    ) {
        EgressDecision::Allow { args, .. } => ArgDecision::Forward(Some(args)),
        EgressDecision::Passthrough(_) => ArgDecision::Forward(None),
        EgressDecision::Deny {
            reason,
            tokens_seen,
        } => ArgDecision::Deny {
            reason,
            tokens_seen,
        },
    }
}

/// Claim the redaction context before the gate's completion notification can
/// wake its waiter. Expired generations never consume a replacement call.
fn response_context(
    pending: &Arc<Mutex<PendingCalls>>, frame: &mut Value,
) -> std::result::Result<Option<PendingContext>, String> {
    if !is_response(frame) { return Ok(None); }
    let Some((id, generation)) = frame.get("id").and_then(correlation) else { return Ok(None); };
    frame["id"] = id.clone();
    let entry = pending.lock().ok().and_then(|mut p| p.remove_generation(&id, &generation));
    entry.filter(|entry| entry.deadline > Instant::now()).map(Some).ok_or_else(|| {
        duduclaw_gateway::third_party_tools::refusal_frame(
            &id, "工具回應已逾時或無法確認原呼叫，內容已扣留，請重新呼叫。",
        )
    })
}

/// Redact using the context claimed before observing terminal completion.
fn rewrite_upstream_frame(
    layer: Option<&McpRedactionLayer>,
    mut frame: Value,
    original: String,
    context: Option<PendingContext>,
) -> String {
    let (Some(layer), Some(entry)) = (layer, context) else { return original; };
    let Some(result) = frame.get_mut("result") else {
        return frame.to_string();
    };
    redact_tool_result_with(
        &layer.manager,
        &entry.tool,
        result,
        &layer.agent_id,
        &layer.session_id,
        Some(&entry.args),
    );
    frame.to_string()
}

async fn write_queued_frame<W: AsyncWrite + Unpin>(
    writer: &mut W, frame: &mut QueuedFrame,
) -> std::io::Result<bool> {
    // A queued call may have timed out or been canceled while an earlier
    // write stalled. The permit holds its lease across this write and flush.
    if !frame.dispatch_allowed() { return Ok(false); }
    if let Some(permit) = &mut frame.permit {
        // Mark before writing: cancellation or an error may leave a partial frame.
        permit.dispatched();
        tokio::select! {
            biased;
            _ = permit.cancelled() => {
                // The frame may be partially written. The caller must close
                // the pipe rather than send another frame after this error.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted, "工具呼叫已取消或逾時，已停止傳送。",
                ));
            }
            result = write_line(writer, &frame.line) => result?,
        }
    } else {
        write_line(writer, &frame.line).await?;
    }
    frame.forwarded();
    Ok(true)
}

async fn write_line<W: AsyncWrite + Unpin>(w: &mut W, line: &str) -> std::io::Result<()> {
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

// ── Subcommand entry point ──────────────────────────────────────────────────

/// `duduclaw mcp-proxy --server <name> -- <cmd> [args…]`.
///
/// Returns the upstream's exit code so the caller can propagate it
/// (`std::process::exit`), matching what the Claude CLI would have observed
/// had it spawned the upstream directly.
pub async fn run_mcp_proxy(
    home_dir: &Path,
    server: &str,
    command: &str,
    args: &[String],
) -> Result<i32> {
    run_mcp_proxy_scoped(home_dir, server, command, args, false).await
}

/// The inherited half of a spawn-scoped upstream's environment: the
/// agent-CLI allowlist only.
pub fn scoped_inherited_env() -> Vec<(String, String)> {
    duduclaw_core::spawn_env::agent_cli_spawn_env_pairs()
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// [`run_mcp_proxy`] for an entry that may be `spawn_scoped` (written into a
/// persistent CLI config by the Grok runtime): its upstream
/// env comes only from the per-spawn directory, and without one the proxy
/// refuses before starting anything ([`crate::mcp_proxy_spawn_env`]).
pub async fn run_mcp_proxy_scoped(
    home_dir: &Path,
    server: &str,
    command: &str,
    args: &[String],
    spawn_scoped: bool,
) -> Result<i32> {
    // Checked before anything else starts: a refused spawn-scoped proxy
    // launches no upstream and initialises nothing.
    let scoped_blob = if spawn_scoped {
        match crate::mcp_proxy_spawn_env::scoped_env_blob(
            std::env::var_os(PROXY_UPSTREAM_ENV_DIR_VAR),
            server,
        ) {
            Ok(blob) => Some(blob),
            Err(why) => {
                tracing::warn!(server = %server, reason = %why, "mcp-proxy: spawn-scoped entry not registered for this spawn — not starting the upstream");
                return Err(DuDuClawError::Gateway(format!(
                    "mcp-proxy: MCP server '{server}' is not available here ({why})"
                )));
            }
        }
    } else {
        None
    };
    if server.trim().is_empty() {
        return Err(DuDuClawError::Gateway(
            "mcp-proxy: --server must be a non-empty server name".to_string(),
        ));
    }
    if command.trim().is_empty() {
        return Err(DuDuClawError::Gateway(
            "mcp-proxy: missing upstream command after `--`".to_string(),
        ));
    }

    let default_agent = crate::mcp::get_default_agent(home_dir).await;

    // Same three outcomes as `mcp-server` (§10.2): disabled ⇒ pass-through,
    // enabled ⇒ redact, broken ⇒ refuse to start.
    let layer = match McpRedactionLayer::try_init(home_dir, &default_agent) {
        Ok(opt) => {
            match opt.as_ref() {
                Some(l) => tracing::info!(
                    server = %server,
                    agent = %l.agent_id,
                    session = %l.session_id,
                    rules = l.manager.engine().rule_count(),
                    "mcp-proxy: redaction enabled"
                ),
                None => tracing::debug!(
                    server = %server,
                    "mcp-proxy: redaction disabled — pure pass-through"
                ),
            }
            opt.map(Arc::new)
        }
        Err(e) => {
            tracing::error!(
                server = %server,
                error = %e,
                "mcp-proxy: redaction is enabled but failed to initialise — refusing to proxy"
            );
            return Err(DuDuClawError::Gateway(format!(
                "redaction is enabled but failed to initialise; refusing to proxy MCP server '{server}' without it: {e}"
            )));
        }
    };

    // Third-party tool gate (2026-10-08). Not applied when the upstream is
    // DuDuClaw's own remote bridge, which gates the same tools itself (the
    // redaction rewrite wraps bridges; gating twice would ask twice).
    let upstream_is_bridge = std::env::current_exe().is_ok_and(|exe| {
        duduclaw_core::mcp_proxy_rewrite::is_remote_bridge_invocation(command, args, &exe)
    });
    let gate = (!upstream_is_bridge).then(|| {
        Arc::new(ThirdPartyGate::new(home_dir, &default_agent, server, "proxy"))
    });

    let env_blob = match scoped_blob {
        Some(blob) => Some(blob),
        None => upstream_env_blob(
            std::env::var(PROXY_UPSTREAM_ENV_VAR).ok(),
            std::env::var(PROXY_UPSTREAM_ENV_FILE_VAR).ok(),
            server,
        ),
    };
    // A spawn-scoped proxy runs under a CLI (Grok) that hands
    // its MCP children its own environment, vendor keys included
    // (`XAI_API_KEY`, `GEMINI_API_KEY`). Its upstream starts from the
    // agent-CLI allowlist instead (PATH, HOME, locale, proxy, temp dir; never
    // a secret-shaped name) plus the declared env, the same base a server
    // gets under the Claude CLI or the tool loop.
    let inherited: Vec<(String, String)> = if spawn_scoped {
        scoped_inherited_env()
    } else {
        std::env::vars().collect()
    };
    let env_pairs = upstream_env(inherited, env_blob.as_deref());

    let mut cmd = duduclaw_core::platform::async_command_for(command);
    cmd.args(args);
    cmd.env_clear();
    cmd.envs(env_pairs);
    if upstream_is_bridge {
        // This is this executable's exact remote-bridge subcommand, not an
        // external server. It needs host identity to sign its ledger calls.
        for key in ["DUDUCLAW_HOME", "DUDUCLAW_PORT", "DUDUCLAW_INSTANCE",
            duduclaw_core::ENV_AGENT_ID, duduclaw_core::ENV_AGENT_TOKEN,
            duduclaw_core::ENV_MCP_API_KEY, duduclaw_core::ENV_TRUST_TURN_ID,
            duduclaw_core::egress_guard::ENV_TURN_ALIAS,
            duduclaw_core::ENV_TASK_ID, duduclaw_core::ENV_ON_BEHALF_OF]
        {
            cmd.env_remove(key);
            if let Some(value) = std::env::var_os(key) { cmd.env(key, value); }
        }
    }

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    // stderr is inherited so the upstream's diagnostics still reach the
    // Claude CLI's MCP log exactly as before.
    cmd.stderr(std::process::Stdio::inherit());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        DuDuClawError::Gateway(format!(
            "mcp-proxy: failed to spawn upstream '{command}': {e}"
        ))
    })?;
    let upstream_in = child
        .stdin
        .take()
        .ok_or_else(|| DuDuClawError::Gateway("mcp-proxy: upstream stdin unavailable".into()))?;
    let upstream_out = child
        .stdout
        .take()
        .ok_or_else(|| DuDuClawError::Gateway("mcp-proxy: upstream stdout unavailable".into()))?;

    let io = ProxyIo {
        client_in: Box::new(tokio::io::stdin()),
        client_out: Box::new(tokio::io::stdout()),
        upstream_in: Box::new(upstream_in),
        upstream_out: Box::new(upstream_out),
    };

    if let Err(e) = run_proxy_io_gated(server, layer, gate, io).await {
        tracing::warn!(server = %server, error = %e, "mcp-proxy: stream loop ended with an error");
    }

    let status = child.wait().await.map_err(|e| {
        DuDuClawError::Gateway(format!("mcp-proxy: waiting for upstream failed: {e}"))
    })?;
    Ok(status.code().unwrap_or(1))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_redaction::REDACTION_FAILED_PLACEHOLDER;
    use duduclaw_redaction::{
        ManagerPaths, RedactionConfig, RedactionManager, RestoreScope, RuleKind, RuleSpec,
    };
    use serde_json::json;
    use tokio::io::duplex;

    // ── pure frame classification ────────────────────────────────────

    #[test]
    fn non_json_lines_are_opaque() {
        assert!(parse_frame("not json at all").is_none());
        assert!(parse_frame("{\"unterminated\": ").is_none());
        assert!(parse_frame("{\"jsonrpc\":\"2.0\"}").is_some());
    }

    #[test]
    fn only_tools_call_requests_are_intercepted() {
        let call = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "pg_select", "arguments": {"table": "customers"}}
        });
        let (id, tool, args) = tool_call_of(&call).expect("a tools/call is intercepted");
        assert_eq!(id, json!(3));
        assert_eq!(tool, "pg_select");
        assert_eq!(args["table"], json!("customers"));

        // Everything else forwards untouched.
        assert!(tool_call_of(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).is_none());
        assert!(tool_call_of(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).is_none());
        assert!(
            tool_call_of(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none()
        );
        // A tools/call with no id is a notification — nothing to answer.
        assert!(
            tool_call_of(&json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"x"}}))
                .is_none()
        );
    }

    #[test]
    fn tools_call_without_arguments_yields_an_empty_object() {
        let call = json!({"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"ping"}});
        let (_, _, args) = tool_call_of(&call).unwrap();
        assert_eq!(args, json!({}));
    }

    #[test]
    fn numeric_and_string_ids_never_collide() {
        assert_ne!(id_key(&json!(7)), id_key(&json!("7")));
        assert_eq!(id_key(&json!(7)).unwrap(), "n:7");
        assert_eq!(id_key(&json!("7")).unwrap(), "s:7");
        assert!(id_key(&Value::Null).is_none());
    }

    #[test]
    fn responses_are_distinguished_from_requests() {
        assert!(is_response(&json!({"id": 1, "result": {}})));
        assert!(is_response(&json!({"id": 1, "error": {"code": -1}})));
        assert!(!is_response(&json!({"id": 1, "method": "roots/list"})));
    }

    #[test]
    fn pending_calls_bookkeeping_is_take_once() {
        let mut p = PendingCalls::new();
        p.insert(&json!(1), "fakepg.pg_select".into(), json!({"table": "t"})).unwrap();
        assert_eq!(p.len(), 1);
        let (tool, args) = p.take(&json!(1)).expect("first take wins");
        assert_eq!(tool, "fakepg.pg_select");
        assert_eq!(args["table"], json!("t"));
        assert!(p.take(&json!(1)).is_none(), "an id is only answered once");
        assert!(p.is_empty());
        // An id we never dispatched (tools/list response) is not ours.
        assert!(p.take(&json!(99)).is_none());
    }

    #[test]
    fn pending_calls_refuse_duplicates_without_changing_redaction_context() {
        let mut pending = PendingCalls::new();
        pending.insert(&json!(1), "fakepg.pg_select".into(), json!({"table":"private"})).unwrap();
        assert!(pending.insert(&json!(1), "fakepg.public".into(), json!({})).is_err());
        let (tool, args) = pending.take(&json!(1)).unwrap();
        assert_eq!(tool, "fakepg.pg_select");
        assert_eq!(args["table"], "private");
    }

    #[test]
    fn pending_calls_bound_capacity_expire_and_reject_invalid_ids() {
        let mut pending = PendingCalls::new();
        for id in 0..MAX_PENDING_CALLS {
            pending.insert(&json!(id), "fakepg.get".into(), json!({})).unwrap();
        }
        assert!(pending.insert(&json!(MAX_PENDING_CALLS), "fakepg.get".into(), json!({})).is_err());
        assert!(pending.insert(&json!({}), "fakepg.get".into(), json!({})).is_err());
        assert!(pending.insert(&json!("x".repeat(257)), "fakepg.get".into(), json!({})).is_err());
        let entry = pending.inner.get_mut(&id_key(&json!(0)).unwrap()).unwrap();
        assert!(entry.deadline <= Instant::now() + Duration::from_secs(300));
        entry.deadline = Instant::now();
        pending.insert(&json!(MAX_PENDING_CALLS), "fakepg.get".into(), json!({})).unwrap();
        assert_eq!(pending.len(), MAX_PENDING_CALLS);
        assert!(pending.take(&json!(0)).is_none());
    }

    #[tokio::test]
    async fn local_refusal_or_cancellation_drops_only_its_own_context() {
        let tmp = tempfile::tempdir().unwrap();
        let layer = layer_for(tmp.path());
        let pending = Arc::new(Mutex::new(PendingCalls::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let frame = json!({"id":1, "method":"tools/call", "params":{"name":"pg_select", "arguments":{}}});
        let old = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &frame, &frame.to_string()).await.unwrap();
        assert_eq!(pending.lock().unwrap().len(), 1);
        // Dropping a prepared call covers gate refusal, canceled approval,
        // failed parsing, and failed enqueue before upstream dispatch.
        drop(old);
        assert!(pending.lock().unwrap().is_empty());
        let old = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &frame, &frame.to_string()).await.unwrap();
        pending.lock().unwrap().inner.get_mut("n:1").unwrap().deadline = Instant::now();
        let replacement = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &frame, &frame.to_string()).await.unwrap();
        drop(old);
        assert_eq!(pending.lock().unwrap().len(), 1, "expired guard cannot remove a replacement");
        drop(replacement);
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn duplicate_preparation_preserves_original_response_redaction() {
        let tmp = tempfile::tempdir().unwrap();
        let layer = layer_for(tmp.path());
        let pending = Arc::new(Mutex::new(PendingCalls::new()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let original = json!({"id":1, "method":"tools/call", "params":{"name":"pg_select", "arguments":{}}});
        let first = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &original, &original.to_string()).await.unwrap();
        let duplicate = json!({"id":1, "method":"tools/call", "params":{"name":"public", "arguments":{}}});
        assert!(prepare_upstream("fakepg", Some(&layer), &pending, &tx, &duplicate, &duplicate.to_string()).await.is_none());
        assert!(rx.recv().await.unwrap().contains("重複"));
        let sent: Value = serde_json::from_str(&first.line).unwrap();
        let mut response = json!({"id":sent["id"], "result":{"rows":[{"name":"王小明"}]}});
        let context = response_context(&pending, &mut response).unwrap();
        let out = rewrite_upstream_frame(Some(&layer), response, String::new(), context);
        assert!(!out.contains("王小明"), "{out}");
        assert!(out.contains("<REDACT:DB_FIELD:"), "{out}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["id"], 1);
    }

    #[test]
    fn expired_wire_response_is_withheld_and_cannot_take_reused_id() {
        let pending = Arc::new(Mutex::new(PendingCalls::new()));
        let old = pending.lock().unwrap().insert(&json!("same"), "fakepg.private".into(), json!({})).unwrap();
        pending.lock().unwrap().inner.get_mut("s:same").unwrap().deadline = Instant::now();
        let new = pending.lock().unwrap().insert(&json!("same"), "fakepg.public".into(), json!({})).unwrap();
        let mut response = json!({"id":wire_id(&json!("same"), &old), "result":{"secret":"private"}});
        let refusal = response_context(&pending, &mut response).unwrap_err();
        assert!(!refusal.contains("private"));
        assert_eq!(pending.lock().unwrap().len(), 1);
        let mut response = json!({"id":wire_id(&json!("same"), &new), "result":{}});
        assert_eq!(response_context(&pending, &mut response).unwrap().unwrap().tool, "fakepg.public");
        assert_eq!(response["id"], "same");
    }

    #[tokio::test]
    async fn cancellation_uses_the_same_upstream_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let layer = layer_for(tmp.path());
        let pending = Arc::new(Mutex::new(PendingCalls::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let frame = json!({"id":"call", "method":"tools/call", "params":{"name":"pg_select"}});
        let call = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &frame, &frame.to_string()).await.unwrap();
        let cancel = json!({"method":"notifications/cancelled", "params":{"requestId":"call"}});
        let cancel = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &cancel, &cancel.to_string()).await.unwrap();
        let call: Value = serde_json::from_str(&call.line).unwrap();
        let cancel: Value = serde_json::from_str(&cancel.line).unwrap();
        assert_eq!(call["id"], cancel["params"]["requestId"]);
    }

    #[tokio::test]
    async fn canceled_queued_call_never_reaches_upstream_and_releases_context() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("agents/a1")).unwrap();
        std::fs::write(tmp.path().join("agents/a1/agent.toml"), "[capabilities]\n").unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[security]\negress_guard='shadow'\n").unwrap();
        let gate = ThirdPartyGate::with_lane(
            tmp.path(), "a1", "fakepg", "proxy", duduclaw_core::ProcessLane::Normal,
        );
        let layer = layer_for(tmp.path());
        let pending = Arc::new(Mutex::new(PendingCalls::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let frame = json!({"id":1, "method":"tools/call", "params":{"name":"pg_select", "arguments":{}}});
        let prepared = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &frame, &frame.to_string()).await.unwrap();
        let sent: Value = serde_json::from_str(&prepared.line).unwrap();
        let call = gate.begin_call(&sent["id"], "pg_select", &json!({})).await.unwrap();
        let mut queued = prepared.queued(Some(call.dispatch_permit()));
        assert!(queued.dispatch_allowed());
        drop(call);
        assert!(!queued.dispatch_allowed());
        let (mut writer, mut reader) = duplex(64);
        assert!(!write_queued_frame(&mut writer, &mut queued).await.unwrap());
        drop(queued);
        assert!(pending.lock().unwrap().is_empty());
        drop(writer);
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes).await.unwrap();
        assert!(bytes.is_empty(), "a canceled queued call must never be dispatched");
    }

    #[tokio::test]
    async fn blocked_upstream_write_stops_when_call_owner_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("agents/a1")).unwrap();
        std::fs::write(tmp.path().join("agents/a1/agent.toml"), "[capabilities]\n").unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[security]\negress_guard='shadow'\n").unwrap();
        let gate = ThirdPartyGate::with_lane(
            tmp.path(), "a1", "fakepg", "proxy", duduclaw_core::ProcessLane::Normal,
        );
        let layer = layer_for(tmp.path());
        let pending = Arc::new(Mutex::new(PendingCalls::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let frame = json!({"id":1, "method":"tools/call", "params":{"name":"pg_select", "arguments":{}}});
        let prepared = prepare_upstream("fakepg", Some(&layer), &pending, &tx, &frame, &frame.to_string()).await.unwrap();
        let sent: Value = serde_json::from_str(&prepared.line).unwrap();
        let call = gate.begin_call(&sent["id"], "pg_select", &json!({})).await.unwrap();
        let mut queued = prepared.queued(Some(call.dispatch_permit()));
        let (mut writer, mut reader) = duplex(8);
        writer.write_all(b"occupied").await.unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let writing = tokio::spawn(async move {
            let _ = started.send(());
            // The full pipe prevents any bytes of this frame from landing.
            // Returning drops the writer, just like the production error path.
            write_queued_frame(&mut writer, &mut queued).await
        });
        ready.await.unwrap();
        drop(call);
        let error = tokio::time::timeout(Duration::from_secs(1), writing)
            .await.expect("cancellation must interrupt a blocked write")
            .unwrap().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(pending.lock().unwrap().is_empty());
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes).await.unwrap();
        assert_eq!(bytes, b"occupied", "no delayed tool frame may follow cancellation");
    }

    #[tokio::test]
    async fn active_calls_are_bounded_and_completed_tasks_are_reaped() {
        let mut held = tokio::task::JoinSet::new();
        for _ in 0..MAX_ACTIVE_CALLS {
            held.spawn(std::future::pending::<()>());
        }
        assert!(!call_capacity(&mut held));
        held.abort_all();
        while held.join_next().await.is_some() {}
        let (tx, rx) = tokio::sync::oneshot::channel();
        held.spawn(async move { let _ = tx.send(()); });
        rx.await.unwrap();
        tokio::task::yield_now().await;
        assert!(call_capacity(&mut held));
        assert!(held.is_empty(), "completed task is reaped without waiting for EOF");
    }

    #[test]
    fn restored_args_replace_only_the_arguments_key() {
        let frame = json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"pg_select","arguments":{"who":"<REDACT:E:aa>"},"_meta":{"x":1}}
        });
        let out = with_restored_args(&frame, json!({"who": "wang@example.com"}));
        assert_eq!(out["params"]["arguments"]["who"], json!("wang@example.com"));
        assert_eq!(out["params"]["name"], json!("pg_select"));
        assert_eq!(out["params"]["_meta"]["x"], json!(1));
        assert_eq!(out["id"], json!(5));
        // The input is untouched (immutability convention).
        assert_eq!(frame["params"]["arguments"]["who"], json!("<REDACT:E:aa>"));
    }

    #[test]
    fn frames_without_params_survive_arg_restoration() {
        let frame = json!({"jsonrpc":"2.0","id":5,"method":"tools/call"});
        assert_eq!(with_restored_args(&frame, json!({"a":1})), frame);
    }

    #[test]
    fn namespacing_uses_the_server_name() {
        assert_eq!(namespaced_tool("crm_pg", "pg_select"), "crm_pg.pg_select");
    }

    #[test]
    fn proxy_env_var_name_is_the_documented_contract() {
        // The gateway writes this name into the rewritten `.mcp.json`
        // (`duduclaw_gateway::redaction_proxy::PROXY_UPSTREAM_ENV_VAR`). It
        // cannot import this constant (that crate is a dependency of this
        // one, not the other way round), so the literal is pinned on both
        // sides and this test is the tripwire for a rename.
        assert_eq!(PROXY_UPSTREAM_ENV_VAR, "DUDUCLAW_MCP_PROXY_ENV");
        assert_eq!(
            PROXY_UPSTREAM_ENV_VAR,
            duduclaw_gateway::redaction_proxy::PROXY_UPSTREAM_ENV_VAR
        );
        assert_eq!(
            REDACTION_FAILED_PLACEHOLDER,
            duduclaw_gateway::redaction_proxy::REDACTION_FAILED_PLACEHOLDER
        );
        assert_eq!(
            PROXY_UPSTREAM_ENV_FILE_VAR,
            duduclaw_core::mcp_proxy_rewrite::PROXY_UPSTREAM_ENV_FILE_VAR
        );
    }

    /// Vendor keys the CLI holds never reach a spawn-scoped upstream; only
    /// allowlisted names (and the declared env) do.
    #[test]
    fn a_spawn_scoped_upstream_starts_from_the_allowlist() {
        let inherited = scoped_inherited_env();
        for (k, _) in &inherited {
            assert!(
                duduclaw_core::spawn_env::AGENT_CLI_ENV_ALLOWLIST.contains(&k.as_str())
                    || duduclaw_core::spawn_env::AGENT_CLI_ENV_ALLOWLIST_MACOS.contains(&k.as_str())
                    || duduclaw_core::spawn_env::AGENT_CLI_ENV_ALLOWLIST_WINDOWS.contains(&k.as_str()),
                "{k}"
            );
            assert!(!k.ends_with("_API_KEY"), "{k}");
        }
        let map: HashMap<_, _> = upstream_env(inherited, Some(r#"{"ML_PASSWORD":"pw"}"#))
            .into_iter()
            .collect();
        assert_eq!(map.get("ML_PASSWORD").map(String::as_str), Some("pw"));
        assert!(!map.contains_key("XAI_API_KEY") && !map.contains_key("GEMINI_API_KEY"));
    }

    #[test]
    fn the_env_dir_name_never_reaches_the_upstream() {
        let inherited = vec![(PROXY_UPSTREAM_ENV_DIR_VAR.to_string(), "/tmp/x".to_string())];
        let map: HashMap<_, _> = upstream_env(inherited, None).into_iter().collect();
        assert!(!map.contains_key(PROXY_UPSTREAM_ENV_DIR_VAR));
    }

    /// A spawn-scoped proxy with no per-spawn directory starts nothing: the
    /// upstream command here would leave a marker file if it ran.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_spawn_scoped_proxy_without_its_env_file_starts_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("ran");
        let script = format!("touch {}", marker.display());
        // No DUDUCLAW_MCP_PROXY_ENV_DIR in this test process.
        assert!(std::env::var_os(PROXY_UPSTREAM_ENV_DIR_VAR).is_none());
        let res = run_mcp_proxy_scoped(
            tmp.path(),
            "masterlink",
            "/bin/sh",
            &["-c".to_string(), script],
            true,
        )
        .await;
        assert!(res.is_err());
        assert!(!marker.exists(), "the upstream must not have been started");
    }

    // ── upstream env file (Codex, 2026-10-19) ───────────────────────

    #[test]
    fn upstream_env_blob_prefers_the_inline_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("env.json");
        std::fs::write(&file, r#"{"A":"file"}"#).unwrap();
        let got = upstream_env_blob(
            Some(r#"{"A":"inline"}"#.to_string()),
            Some(file.to_string_lossy().into_owned()),
            "s",
        );
        assert_eq!(got.as_deref(), Some(r#"{"A":"inline"}"#));
    }

    #[test]
    fn upstream_env_blob_reads_the_file_when_no_inline_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("env.json");
        std::fs::write(&file, r#"{"PGPASSWORD":"from-file"}"#).unwrap();
        let blob = upstream_env_blob(None, Some(file.to_string_lossy().into_owned()), "s");
        let map: HashMap<_, _> = upstream_env(Vec::new(), blob.as_deref()).into_iter().collect();
        assert_eq!(map.get("PGPASSWORD").map(String::as_str), Some("from-file"));
        // A missing file contributes nothing rather than aborting.
        assert!(upstream_env_blob(None, Some("/nonexistent/x.json".into()), "s").is_none());
        assert!(upstream_env_blob(None, None, "s").is_none());
    }

    #[test]
    fn the_env_file_name_never_reaches_the_upstream() {
        let inherited = vec![(PROXY_UPSTREAM_ENV_FILE_VAR.to_string(), "/tmp/x".to_string())];
        let map: HashMap<_, _> = upstream_env(inherited, None).into_iter().collect();
        assert!(!map.contains_key(PROXY_UPSTREAM_ENV_FILE_VAR));
    }

    // ── upstream env ────────────────────────────────────────────────

    #[test]
    fn turn_identity_never_reaches_an_external_upstream() {
        let inherited = [
            ("DUDUCLAW_TURN_ID".into(), "turn".into()),
            ("DUDUCLAW_EGRESS_TURN_ID".into(), "turn".into()),
            ("DUDUCLAW_SESSION_ID".into(), "session".into()),
            ("DUDUCLAW_AGENT_TOKEN".into(), "token".into()),
            ("DUDUCLAW_ON_BEHALF_OF".into(), "user".into()),
        ];
        assert!(upstream_env(inherited, None).is_empty());
    }

    #[test]
    fn upstream_env_strips_duduclaw_internals_and_applies_the_declared_map() {
        let inherited = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            (
                "DUDUCLAW_MCP_API_KEY".to_string(),
                "ddc_dev_secret".to_string(),
            ),
            ("DUDUCLAW_AGENT_TOKEN".to_string(), "mac".to_string()),
            (PROXY_UPSTREAM_ENV_VAR.to_string(), "{}".to_string()),
            ("PGPASSWORD".to_string(), "inherited".to_string()),
        ];
        let out = upstream_env(
            inherited,
            Some(r#"{"PGPASSWORD":"declared","PGHOST":"db"}"#),
        );
        let map: HashMap<_, _> = out.into_iter().collect();

        assert_eq!(map.get("PATH").map(String::as_str), Some("/usr/bin"));
        // Declared wins over inherited for the same name.
        assert_eq!(map.get("PGPASSWORD").map(String::as_str), Some("declared"));
        assert_eq!(map.get("PGHOST").map(String::as_str), Some("db"));
        // DuDuClaw-internal identity/credentials never reach a third-party server.
        assert!(!map.contains_key("DUDUCLAW_MCP_API_KEY"));
        assert!(!map.contains_key("DUDUCLAW_AGENT_TOKEN"));
        assert!(!map.contains_key(PROXY_UPSTREAM_ENV_VAR));
    }

    #[test]
    fn a_malformed_env_blob_is_ignored_not_fatal() {
        let inherited = vec![("PATH".to_string(), "/bin".to_string())];
        let out = upstream_env(inherited, Some("not json"));
        assert_eq!(out, vec![("PATH".to_string(), "/bin".to_string())]);
    }

    // ── end-to-end over in-memory pipes ─────────────────────────────

    /// A manager with one `json_path` rule over `fakepg.pg_select`'s
    /// `$.rows[*].name`, plus the `general` profile (so an email in the same
    /// payload is caught by the text pass).
    fn proxy_manager(home: &Path) -> Arc<RedactionManager> {
        let mut cfg = RedactionConfig::default();
        cfg.enabled = true;
        cfg.profiles = vec!["general".to_string()];
        cfg.rules.insert(
            "pg_customers".to_string(),
            RuleSpec {
                id: "pg_customers".into(),
                category: "DB_FIELD".into(),
                restore_scope: RestoreScope::Owner,
                priority: 70,
                cross_session_stable: false,
                apply_to_system_prompt: false,
                enabled: true,
                kind: RuleKind::JsonPath {
                    paths: vec!["$.rows[*].name".into()],
                    match_tool: Some("fakepg.pg_select".into()),
                    match_args: Default::default(),
                    match_result: Default::default(),
                    exclude_keys: Vec::new(),
                },
            },
        );
        Arc::new(RedactionManager::open(cfg, ManagerPaths::under_home(home)).unwrap())
    }

    fn layer_for(home: &Path) -> Arc<McpRedactionLayer> {
        Arc::new(McpRedactionLayer {
            manager: proxy_manager(home),
            agent_id: "agnes".to_string(),
            session_id: "s1".to_string(),
        })
    }

    /// Drive `run_proxy_io` with a scripted in-memory "upstream" that replies
    /// to every request it sees with `canned` (its `id` patched in), and
    /// records every line it received.
    async fn drive(
        server: &str,
        layer: Option<Arc<McpRedactionLayer>>,
        client_lines: Vec<String>,
        canned: Option<Value>,
    ) -> (Vec<String>, Vec<String>) {
        let (client_w, client_in) = duplex(1 << 16); // test writes → proxy reads
        let (client_out, client_r) = duplex(1 << 16); // proxy writes → test reads
        let (up_in, up_side_out) = duplex(1 << 16); // proxy writes → upstream reads
        let (up_side_in, up_out) = duplex(1 << 16); // upstream writes → proxy reads

        let proxy = tokio::spawn({
            let server = server.to_string();
            async move {
                run_proxy_io(
                    &server,
                    layer,
                    ProxyIo {
                        client_in: Box::new(client_in),
                        client_out: Box::new(client_out),
                        upstream_in: Box::new(up_in),
                        upstream_out: Box::new(up_out),
                    },
                )
                .await
            }
        });

        // Fake upstream.
        let upstream = tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut lines = BufReader::new(up_side_out).lines();
            let mut out = up_side_in;
            while let Ok(Some(line)) = lines.next_line().await {
                seen.push(line.clone());
                if let (Some(tpl), Ok(req)) =
                    (canned.as_ref(), serde_json::from_str::<Value>(&line))
                {
                    let mut resp = tpl.clone();
                    if let (Some(obj), Some(id)) = (resp.as_object_mut(), req.get("id")) {
                        obj.insert("id".to_string(), id.clone());
                    }
                    let mut payload = resp.to_string();
                    payload.push('\n');
                    if out.write_all(payload.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = out.flush().await;
                }
            }
            seen
        });

        // Feed the client side, then close it so the proxy drains and exits.
        let mut w = client_w;
        for l in client_lines {
            let mut payload = l;
            payload.push('\n');
            w.write_all(payload.as_bytes()).await.unwrap();
        }
        w.flush().await.unwrap();
        drop(w);

        let _ = proxy.await.unwrap();
        let seen = upstream.await.unwrap();

        let mut replies = Vec::new();
        let mut lines = BufReader::new(client_r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            replies.push(l);
        }
        (seen, replies)
    }

    /// The third-party tool gate in the proxy: a blocked tool is hidden and
    /// refused without reaching the upstream, an `ask` tool waits for an
    /// ApprovalBroker decision, a trusted read passes.
    #[tokio::test]
    async fn gate_hides_refuses_and_holds_calls_for_approval() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[security]\negress_guard=\"shadow\"\n").unwrap();
        let agent = tmp.path().join("agents").join("a1");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("agent.toml"),
            "[capabilities]\naction_rules = [{ effect = \"delete\", verdict = \"block\" }, { effect = \"modify\", verdict = \"ask\" }]\ntrusted_read_hint_servers = [\"crm\"]\n",
        )
        .unwrap();
        let gate = Arc::new(ThirdPartyGate::with_lane(
            tmp.path(),
            "a1",
            "crm",
            "proxy",
            duduclaw_core::ProcessLane::Normal,
        ));

        let (client_w, client_in) = duplex(1 << 16);
        let (client_out, client_r) = duplex(1 << 16);
        let (up_in, up_side_out) = duplex(1 << 16);
        let (up_side_in, up_out) = duplex(1 << 16);
        let proxy = tokio::spawn(async move {
            run_proxy_io_gated(
                "crm",
                None,
                Some(gate),
                ProxyIo {
                    client_in: Box::new(client_in),
                    client_out: Box::new(client_out),
                    upstream_in: Box::new(up_in),
                    upstream_out: Box::new(up_out),
                },
            )
            .await
        });
        let upstream = tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut lines = BufReader::new(up_side_out).lines();
            let mut out = up_side_in;
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                seen.push(line);
                let result = if req["method"] == "tools/list" {
                    json!({ "tools": [
                        { "name": "get", "annotations": { "readOnlyHint": true } },
                        { "name": "put" },
                        { "name": "drop", "annotations": { "destructiveHint": true } },
                    ]})
                } else {
                    json!({ "content": [{ "type": "text", "text": "done" }] })
                };
                let resp = json!({ "jsonrpc": "2.0", "id": req["id"], "result": result });
                out.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                out.flush().await.unwrap();
            }
            seen
        });
        // A person approves whatever is waiting.
        let home = tmp.path().to_path_buf();
        let approver = tokio::spawn(async move {
            let broker = duduclaw_gateway::approval::ApprovalBroker::open(&home).unwrap();
            for _ in 0..200 {
                let pending = broker.list_pending(Some("a1")).await.unwrap_or_default();
                if let Some(rec) = pending.first() {
                    assert_eq!(rec.action_kind, "mcp_call");
                    assert!(rec.summary.contains("crm.put"), "{}", rec.summary);
                    broker.decide(&rec.id, true, "dashboard:admin").await.unwrap();
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            false
        });

        let mut w = client_w;
        let mut lines = BufReader::new(client_r).lines();
        let mut replies = Vec::new();
        // List first (a call before any listing knows no annotations, so the
        // tool counts as `modify`), then call.
        w.write_all(format!("{}\n", json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).as_bytes())
            .await
            .unwrap();
        w.flush().await.unwrap();
        replies.push(serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap());
        for (id, name) in [(2, "get"), (3, "drop"), (4, "put")] {
            let frame = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": {} } });
            w.write_all(format!("{frame}\n").as_bytes()).await.unwrap();
        }
        w.flush().await.unwrap();
        drop(w);

        assert!(approver.await.unwrap(), "an approval was filed");
        proxy.await.unwrap().unwrap();
        let seen = upstream.await.unwrap();
        while let Ok(Some(l)) = lines.next_line().await {
            replies.push(serde_json::from_str::<Value>(&l).unwrap());
        }
        assert!(seen.iter().all(|l| !l.contains("\"drop\"")), "{seen:?}");
        assert_eq!(seen.len(), 3, "{seen:?}");
        let by_id = |id: i64| replies.iter().find(|r| r["id"] == id).cloned().unwrap();
        let listed: Vec<String> = by_id(1)["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(listed, vec!["get", "put"]);
        assert!(by_id(2)["result"].is_object());
        assert_eq!(by_id(3)["error"]["code"], json!(-32003));
        assert!(by_id(4)["result"].is_object(), "approved call ran");
    }

    #[tokio::test]
    async fn tool_result_is_redacted_on_the_way_back() {
        let tmp = tempfile::TempDir::new().unwrap();
        let layer = layer_for(tmp.path());

        let rows = json!({"rows": [{"id": 1, "name": "王小明", "email": "wang@example.com"}]});
        let canned = json!({
            "jsonrpc": "2.0",
            "result": {"content": [{"type": "text", "text": serde_json::to_string_pretty(&rows).unwrap()}]}
        });
        let call = json!({
            "jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":"pg_select","arguments":{"table":"customers"}}
        })
        .to_string();

        let (seen, replies) = drive("fakepg", Some(layer), vec![call], Some(canned)).await;

        assert_eq!(seen.len(), 1, "the call reached upstream verbatim");
        assert_eq!(replies.len(), 1);
        let out: Value = serde_json::from_str(&replies[0]).unwrap();
        let text = out["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains("王小明"),
            "structured rule must fire: {text}"
        );
        assert!(text.contains("<REDACT:DB_FIELD:"), "{text}");
        assert!(
            !text.contains("wang@example.com"),
            "general profile: {text}"
        );
        assert!(text.contains("<REDACT:EMAIL:"), "{text}");
        // The envelope survives (§3.4 root-node protection).
        assert_eq!(out["result"]["content"][0]["type"], json!("text"));
        assert_eq!(out["id"], json!(7));
    }

    #[tokio::test]
    async fn non_tool_traffic_and_opaque_lines_pass_through_untouched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let layer = layer_for(tmp.path());

        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string();
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string();
        let garbage = "this is not JSON".to_string();

        let (seen, _) = drive(
            "fakepg",
            Some(layer),
            vec![init.clone(), note.clone(), garbage.clone()],
            None,
        )
        .await;

        assert_eq!(seen, vec![init, note, garbage]);
    }

    #[tokio::test]
    async fn a_denied_tool_call_never_reaches_upstream() {
        // `tool_egress` is default-deny, so a call carrying a hallucinated or
        // non-whitelisted token is refused — and the refusal is answered by
        // the proxy itself.
        let tmp = tempfile::TempDir::new().unwrap();
        let layer = layer_for(tmp.path());

        let call = json!({
            "jsonrpc":"2.0","id":11,"method":"tools/call",
            "params":{"name":"pg_select","arguments":{"who":"<REDACT:EMAIL:deadbeefdeadbeefdeadbeefdeadbeef>"}}
        })
        .to_string();

        let (seen, replies) = drive("fakepg", Some(layer), vec![call], None).await;

        assert!(
            seen.is_empty(),
            "a Deny must not be forwarded upstream, got {seen:?}"
        );
        assert_eq!(replies.len(), 1);
        let err: Value = serde_json::from_str(&replies[0]).unwrap();
        assert_eq!(err["error"]["code"], json!(-32007));
        assert_eq!(err["error"]["data"]["tool"], json!("fakepg.pg_select"));
        assert_eq!(err["id"], json!(11));
    }

    #[tokio::test]
    async fn batch_tool_call_cannot_bypass_gate() {
        let batch = json!([{"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"send", "arguments":{}}}]);
        let (seen, replies) = drive("crm", None, vec![batch.to_string()], None).await;
        assert!(seen.is_empty());
        let reply: Value = serde_json::from_str(&replies[0]).unwrap();
        assert_eq!(reply["error"]["code"], -32003);
    }

    #[tokio::test]
    async fn notification_tool_call_cannot_bypass_gate() {
        let call = json!({"jsonrpc":"2.0", "method":"tools/call", "params":{"name":"send", "arguments":{}}});
        let (seen, replies) = drive("crm", None, vec![call.to_string()], None).await;
        assert!(seen.is_empty());
        let reply: Value = serde_json::from_str(&replies[0]).unwrap();
        assert_eq!(reply["error"]["code"], -32003);
    }

    #[tokio::test]
    async fn redaction_disabled_is_a_pure_passthrough() {
        let rows = json!({"rows": [{"id": 1, "name": "王小明"}]});
        let canned = json!({
            "jsonrpc": "2.0",
            "result": {"content": [{"type": "text", "text": serde_json::to_string_pretty(&rows).unwrap()}]}
        });
        let call = json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"pg_select","arguments":{"table":"customers"}}
        })
        .to_string();

        let (seen, replies) = drive("fakepg", None, vec![call.clone()], Some(canned)).await;
        assert_eq!(seen, vec![call]);
        assert!(replies[0].contains("王小明"), "{}", replies[0]);
    }
}
