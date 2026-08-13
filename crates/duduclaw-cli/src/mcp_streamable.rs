//! Standard MCP **Streamable HTTP** transport endpoint (`/mcp`) — WP3.1-T1.
//!
//! The existing `/mcp/v1/*` routes are a DuDuClaw-specific REST wrapper
//! (single tool call + SSE push). Remote MCP clients — claude.ai custom
//! connectors, Claude mobile, MCP Inspector, any spec client — speak the MCP
//! *protocol* over the Streamable HTTP transport instead: one endpoint that
//! accepts JSON-RPC requests (`initialize` / `tools/list` / `tools/call` /
//! `ping`) per POST. This module adds that endpoint alongside the legacy
//! routes; all requirements were verified against the official spec repo
//! (`modelcontextprotocol/modelcontextprotocol`, revision 2025-06-18 — the
//! revision remote connectors are validated against):
//!
//! - single endpoint, client POSTs one JSON-RPC request or notification per
//!   HTTP request; notifications answer `202 Accepted` with no body
//! - the server may answer requests with a plain `application/json` response
//!   (SSE per-request streams are a server option, not an obligation)
//! - `GET /mcp` without server-push support ⇒ `405 Method Not Allowed`;
//!   `DELETE /mcp` without session support ⇒ `405`
//! - stateless mode is spec-legal: a server that doesn't need sessions simply
//!   never issues an `Mcp-Session-Id`
//! - `MCP-Protocol-Version` header: unsupported value ⇒ `400`; absent ⇒
//!   assume an older revision and proceed (spec backwards-compat rule)
//! - `Origin` MUST be validated when present (DNS-rebinding defence):
//!   loopback hosts and `config.toml [gateway] allowed_origins` entries are
//!   accepted, everything else is `403` (fail closed; absent Origin = a
//!   non-browser client, allowed)
//!
//! Auth is the same Bearer surface as `/mcp/v1/call` (mcp_keys +
//! scope/namespace enforcement); the OAuth 2.1 issuance flow (WP3.1-T2)
//! plugs into that same layer, so this endpoint doesn't change when it lands.
//! Method surface is deliberately tools-only (matching the stdio server):
//! `initialize`, `ping`, `tools/list`, `tools/call`; everything else answers
//! JSON-RPC `-32601`.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::mcp_dispatch::{jsonrpc_error, jsonrpc_response};
use crate::mcp_http_server::HttpState;
use crate::mcp_rate_limit::OpType;

/// Protocol revisions this endpoint accepts in the `MCP-Protocol-Version`
/// header and negotiates in `initialize`. Newest first.
pub(crate) const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The newest revision we implement — offered whenever the client asks for a
/// version we don't know (spec: server answers with its latest supported).
pub(crate) const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";

// ── Origin validation ─────────────────────────────────────────────────────────

/// Hosts that are always acceptable Origins (the server binds loopback by
/// default; a browser-based client on the same machine is legitimate).
const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// Read `[gateway] allowed_origins` from `config.toml`, normalized to the
/// `host[:port]` form `origin_host_matches` expects (scheme + trailing-slash
/// stripped). Read per request — the file is tiny and OS-cached, and it keeps
/// dashboard edits effective without an http-server restart (same operator
/// knob the WebChat/extension flows already document).
fn configured_origins(home_dir: &std::path::Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return Vec::new();
    };
    let Ok(v) = raw.parse::<toml::Value>() else {
        return Vec::new();
    };
    v.get("gateway")
        .and_then(|g| g.get("allowed_origins"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str())
                .filter_map(normalize_origin_entry)
                .collect()
        })
        .unwrap_or_default()
}

/// Normalize an allowlist entry: trim, strip scheme prefix, strip trailing
/// slash. Mirrors the gateway's `normalize_origin_entry` semantics (that fn is
/// private to the gateway's server module; duplicating four lines beats
/// exporting a gateway internal for a sibling process).
fn normalize_origin_entry(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    let mut start = 0;
    for scheme in ["http://", "https://", "ws://", "wss://"] {
        if lower.starts_with(scheme) {
            start = scheme.len();
            break;
        }
    }
    let cleaned = trimmed[start..].trim_end_matches('/').trim();
    if cleaned.is_empty() { None } else { Some(cleaned.to_string()) }
}

/// Spec MUST: validate `Origin` when present. Absent header = non-browser
/// client (claude.ai's backend, CLI tools) → allowed. Present → must match
/// loopback or the operator allowlist via the anchored `origin_host_matches`
/// (never substring — project convention 2).
fn origin_ok(headers: &HeaderMap, home_dir: &std::path::Path) -> bool {
    let Some(origin) = headers.get("Origin").and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let mut allowed: Vec<String> = LOOPBACK_HOSTS.iter().map(|s| s.to_string()).collect();
    allowed.extend(configured_origins(home_dir));
    let allowed_refs: Vec<&str> = allowed.iter().map(String::as_str).collect();
    duduclaw_core::origin_host_matches(origin, &allowed_refs)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /mcp` — this server has no server-initiated stream; the spec's
/// prescribed answer is 405.
pub(crate) async fn mcp_get_handler() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "server-initiated streams are not supported").into_response()
}

/// `DELETE /mcp` — stateless server, no sessions to terminate; spec allows
/// answering 405.
pub(crate) async fn mcp_delete_handler() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "sessions are not used by this server").into_response()
}

/// `POST /mcp` — the Streamable HTTP message endpoint.
pub(crate) async fn mcp_post_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // 1. Origin gate (DNS-rebinding defence) — before anything else.
    if !origin_ok(&headers, &state.home_dir) {
        return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
    }

    // 2. Protocol-version header gate: unknown value ⇒ 400 (spec); absent ⇒
    //    treat as an older revision and proceed.
    if let Some(ver) = headers.get("MCP-Protocol-Version").and_then(|v| v.to_str().ok()) {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&ver) {
            return (
                StatusCode::BAD_REQUEST,
                Json(jsonrpc_error(
                    &Value::Null,
                    -32600,
                    &format!(
                        "Unsupported MCP-Protocol-Version '{ver}' (supported: {})",
                        SUPPORTED_PROTOCOL_VERSIONS.join(", ")
                    ),
                )),
            )
                .into_response();
        }
    }

    // 3. Bearer auth — same key surface and scope enforcement as /mcp/v1/call
    //    (static mcp_keys AND OAuth-issued tokens). Spec MUST: a 401 from the
    //    MCP endpoint carries `WWW-Authenticate` pointing at the RFC 9728
    //    resource-metadata document so clients can discover the OAuth flow.
    let (principal, ns_ctx) =
        match crate::mcp_http_server::authenticate_bearer(&headers, &state.home_dir) {
            Ok(p) => p,
            Err(mut r) => {
                if r.status() == StatusCode::UNAUTHORIZED {
                    if let Ok(v) =
                        crate::mcp_oauth_server::www_authenticate_value(&headers).parse()
                    {
                        r.headers_mut().insert("WWW-Authenticate", v);
                    }
                }
                return r;
            }
        };

    // 4. Body: exactly one JSON-RPC request or notification per POST.
    let msg: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(jsonrpc_error(&Value::Null, -32700, "Parse error")),
            )
                .into_response();
        }
    };
    if msg.is_array() {
        // 2025-06-18 removed JSON-RPC batching; a stateless server rejects it.
        return (
            StatusCode::BAD_REQUEST,
            Json(jsonrpc_error(&Value::Null, -32600, "JSON-RPC batching is not supported")),
        )
            .into_response();
    }
    let method = msg.get("method").and_then(|v| v.as_str());
    let has_id = msg.get("id").map(|i| !i.is_null()).unwrap_or(false);

    // Client-to-server JSON-RPC *responses* are only legal replies to server
    // requests — a stateless tools-only server never issues any, so any
    // response body is a protocol violation (spec: return an HTTP error).
    if method.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(jsonrpc_error(
                &Value::Null,
                -32600,
                "Body must be a single JSON-RPC request or notification",
            )),
        )
            .into_response();
    }
    let method = method.unwrap_or_default();

    // 5. Notifications: accept and answer 202 with no body (spec MUST).
    if !has_id {
        return StatusCode::ACCEPTED.into_response();
    }

    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    // 6. Requests — tools-only surface, mirroring the stdio server.
    let jsonrpc = match method {
        "initialize" => handle_initialize(&id, &params),
        "ping" => jsonrpc_response(&id, json!({})),
        "tools/list" => crate::mcp::handle_tools_list(&id, &principal, &state.home_dir),
        "tools/call" => {
            // Same per-key rate gate as the legacy call route.
            if let Err(e) =
                state.dispatcher.rate_limiter.check(&principal.client_id, OpType::HttpRequest)
            {
                let mut resp = (
                    StatusCode::OK,
                    Json(jsonrpc_error(
                        &id,
                        -32029,
                        &format!(
                            "HTTP rate limit exceeded, retry after {} seconds",
                            e.retry_after_secs
                        ),
                    )),
                )
                    .into_response();
                resp.headers_mut().insert(
                    "Retry-After",
                    e.retry_after_secs
                        .to_string()
                        .parse()
                        .unwrap_or_else(|_| "1".parse().unwrap()),
                );
                return resp;
            }
            match tokio::time::timeout(
                state.call_timeout,
                state.dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => jsonrpc_error(&id, -32603, "Request timed out (30s limit exceeded)"),
            }
        }
        other => jsonrpc_error(&id, -32601, &format!("Method not found: {other}")),
    };

    // Single application/json response — the spec's non-streaming server mode
    // (clients MUST accept it).
    (StatusCode::OK, Json(jsonrpc)).into_response()
}

/// MCP `initialize` with real version negotiation: echo the client's revision
/// when we support it, otherwise answer with our latest (the client then
/// decides whether to continue). Capabilities are honest — tools only, no
/// listChanged notifications (stateless transport has no server push).
fn handle_initialize(id: &Value, params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
    let negotiated = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        LATEST_PROTOCOL_VERSION
    };
    jsonrpc_response(
        id,
        json!({
            "protocolVersion": negotiated,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "duduclaw",
                "title": "DuDuClaw",
                "version": duduclaw_gateway::updater::current_version()
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_negotiates_supported_and_falls_back_to_latest() {
        let r = handle_initialize(&json!(1), &json!({"protocolVersion": "2025-03-26"}));
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        let r = handle_initialize(&json!(2), &json!({"protocolVersion": "1999-01-01"}));
        assert_eq!(r["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
        let r = handle_initialize(&json!(3), &json!({}));
        assert_eq!(r["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
        assert_eq!(r["result"]["serverInfo"]["name"], "duduclaw");
    }

    #[test]
    fn origin_entries_normalize_and_loopback_always_allowed() {
        assert_eq!(normalize_origin_entry(" https://Example.com/ "), Some("Example.com".into()));
        assert_eq!(normalize_origin_entry("chrome-ext-id"), Some("chrome-ext-id".into()));
        assert_eq!(normalize_origin_entry("  "), None);

        let dir = tempfile::tempdir().unwrap();
        let mut h = HeaderMap::new();
        // Absent Origin (server-side client) is allowed.
        assert!(origin_ok(&h, dir.path()));
        // Loopback allowed out of the box.
        h.insert("Origin", "http://localhost:5173".parse().unwrap());
        assert!(origin_ok(&h, dir.path()));
        // Non-loopback origin with no allowlist ⇒ denied (fail closed).
        h.insert("Origin", "https://evil.example".parse().unwrap());
        assert!(!origin_ok(&h, dir.path()));
        // `localhost.evil.com` must NOT pass a `localhost` allowlist entry
        // (anchored matching — project convention 2).
        h.insert("Origin", "http://localhost.evil.com".parse().unwrap());
        assert!(!origin_ok(&h, dir.path()));
        // Config allowlist admits an exact host.
        std::fs::write(
            dir.path().join("config.toml"),
            "[gateway]\nallowed_origins = [\"https://app.example.com\"]\n",
        )
        .unwrap();
        h.insert("Origin", "https://app.example.com".parse().unwrap());
        assert!(origin_ok(&h, dir.path()));
    }
}
