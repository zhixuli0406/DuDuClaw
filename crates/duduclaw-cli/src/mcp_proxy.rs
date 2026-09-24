//! `duduclaw mcp-proxy` — a redacting stdio JSON-RPC pass-through for
//! **external** MCP servers (design
//! `commercial/docs/DESIGN-redaction-field-rules-2026-09.md` §13.3 plan A,
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
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_redaction::EgressDecision;

use crate::mcp_redaction::{
    decide_tool_args_with, egress_deny_response, redact_tool_result_with, McpRedactionLayer,
};

/// Env var carrying the upstream server's `.mcp.json` `env` map, JSON-encoded.
/// Written by the gateway's config rewrite, consumed (and removed) here.
pub const PROXY_UPSTREAM_ENV_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV";

/// DuDuClaw-internal names the rewritten `.mcp.json` entry sets **for the
/// proxy process** and which must not be handed down to the third-party
/// upstream server. Everything else is inherited, so the upstream sees the
/// same environment the Claude CLI would have given it.
pub const PROXY_PRIVATE_ENV: &[&str] = &[
    PROXY_UPSTREAM_ENV_VAR,
    "DUDUCLAW_HOME",
    "DUDUCLAW_PORT",
    "DUDUCLAW_INSTANCE",
    "DUDUCLAW_AGENT_ID",
    "DUDUCLAW_AGENT_TOKEN",
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
    "DUDUCLAW_REDACTION_SCOPES",
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
/// for a malformed `params` — all of which are forwarded untouched.
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
#[derive(Debug, Default)]
pub struct PendingCalls {
    inner: HashMap<String, (String, Value)>,
}

impl PendingCalls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember a dispatched call. `tool` is already namespaced.
    pub fn insert(&mut self, id: &Value, tool: String, args: Value) {
        if let Some(k) = id_key(id) {
            self.inner.insert(k, (tool, args));
        }
    }

    /// Take the context for an answered call. `None` ⇒ not one of ours
    /// (a `tools/list` response, a server-initiated request, a duplicate id).
    pub fn take(&mut self, id: &Value) -> Option<(String, Value)> {
        let k = id_key(id)?;
        self.inner.remove(&k)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
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
    let ProxyIo {
        client_in,
        mut client_out,
        mut upstream_in,
        upstream_out,
    } = io;

    // One writer owns `client_out`, because two producers write to it: the
    // upstream→client pump, and the deny responses the client→upstream pump
    // answers itself.
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

    let pending = Arc::new(tokio::sync::Mutex::new(PendingCalls::new()));

    // ── upstream → client ────────────────────────────────────────────
    let up_task = {
        let pending = pending.clone();
        let layer = layer.clone();
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
                    Some(frame) => {
                        rewrite_upstream_frame(layer.as_deref(), &pending, frame, line).await
                    }
                };
                if tx.send(out).is_err() {
                    break;
                }
            }
        })
    };

    // ── client → upstream ────────────────────────────────────────────
    let mut lines = BufReader::new(client_in).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Some(frame) = parse_frame(&line) else {
            // Opaque line — forward verbatim.
            if write_line(&mut upstream_in, &line).await.is_err() {
                break;
            }
            continue;
        };

        let forward = match (layer.as_deref(), tool_call_of(&frame)) {
            // Redaction off, or not a tools/call → verbatim.
            (None, _) | (_, None) => Some(line.clone()),
            (Some(layer), Some((id, tool, args))) => {
                let ns = namespaced_tool(server, &tool);
                let decided = decide_args(layer, &ns, &args);
                match decided {
                    ArgDecision::Deny { reason, tokens_seen } => {
                        // Answer here; the call NEVER reaches upstream.
                        let resp = egress_deny_response(&id, &ns, &reason, tokens_seen);
                        let _ = tx.send(resp.to_string());
                        None
                    }
                    ArgDecision::Forward(restored) => {
                        let effective_args = restored.clone().unwrap_or_else(|| args.clone());
                        pending.lock().await.insert(&id, ns, effective_args);
                        match restored {
                            Some(new_args) => {
                                Some(with_restored_args(&frame, new_args).to_string())
                            }
                            None => Some(line.clone()),
                        }
                    }
                }
            }
        };

        let Some(payload) = forward else { continue };
        if write_line(&mut upstream_in, &payload).await.is_err() {
            break;
        }
    }

    // Client hung up (or the pipe broke): close the upstream's stdin so it
    // shuts down, then drain whatever it still has to say.
    drop(upstream_in);
    drop(tx);
    let _ = up_task.await;
    let _ = writer.await;
    Ok(())
}

/// What to do with a `tools/call`'s arguments.
enum ArgDecision {
    /// Forward; `Some(args)` when the tokens were restored to real values.
    Forward(Option<Value>),
    Deny { reason: String, tokens_seen: usize },
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

/// Redact a `tools/call` response's `result` when it answers a call we
/// remembered. Returns the line to write out.
async fn rewrite_upstream_frame(
    layer: Option<&McpRedactionLayer>,
    pending: &Arc<tokio::sync::Mutex<PendingCalls>>,
    frame: Value,
    original: String,
) -> String {
    let Some(layer) = layer else {
        return original;
    };
    if !is_response(&frame) {
        // A server-initiated request (sampling/createMessage, roots/list) —
        // forwarded untouched in both directions.
        return original;
    }
    let Some(id) = frame.get("id") else {
        return original;
    };
    let Some((tool, args)) = pending.lock().await.take(id) else {
        return original;
    };
    let mut frame = frame;
    let Some(result) = frame.get_mut("result") else {
        // An error response: nothing to redact (and the tool never ran).
        return original;
    };
    redact_tool_result_with(
        &layer.manager,
        &tool,
        result,
        &layer.agent_id,
        &layer.session_id,
        Some(&args),
    );
    frame.to_string()
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

    let env_blob = std::env::var(PROXY_UPSTREAM_ENV_VAR).ok();
    let env_pairs = upstream_env(std::env::vars(), env_blob.as_deref());

    let mut cmd = duduclaw_core::platform::async_command_for(command);
    cmd.args(args);
    cmd.env_clear();
    cmd.envs(env_pairs);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    // stderr is inherited so the upstream's diagnostics still reach the
    // Claude CLI's MCP log exactly as before.
    cmd.stderr(std::process::Stdio::inherit());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        DuDuClawError::Gateway(format!("mcp-proxy: failed to spawn upstream '{command}': {e}"))
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

    if let Err(e) = run_proxy_io(server, layer, io).await {
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
        assert!(tool_call_of(
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none());
        // A tools/call with no id is a notification — nothing to answer.
        assert!(tool_call_of(
            &json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"x"}})
        )
        .is_none());
    }

    #[test]
    fn tools_call_without_arguments_yields_an_empty_object() {
        let call =
            json!({"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"ping"}});
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
        p.insert(&json!(1), "fakepg.pg_select".into(), json!({"table": "t"}));
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
    }

    // ── upstream env ────────────────────────────────────────────────

    #[test]
    fn upstream_env_strips_duduclaw_internals_and_applies_the_declared_map() {
        let inherited = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("DUDUCLAW_MCP_API_KEY".to_string(), "ddc_dev_secret".to_string()),
            ("DUDUCLAW_AGENT_TOKEN".to_string(), "mac".to_string()),
            (PROXY_UPSTREAM_ENV_VAR.to_string(), "{}".to_string()),
            ("PGPASSWORD".to_string(), "inherited".to_string()),
        ];
        let out = upstream_env(inherited, Some(r#"{"PGPASSWORD":"declared","PGHOST":"db"}"#));
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
        assert!(!text.contains("王小明"), "structured rule must fire: {text}");
        assert!(text.contains("<REDACT:DB_FIELD:"), "{text}");
        assert!(!text.contains("wang@example.com"), "general profile: {text}");
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
