//! Standard MCP **Streamable HTTP** transport endpoint (`/mcp`) — WP3.1-T1.
//!
//! The existing `/mcp/v1/*` routes are a DuDuClaw-specific REST wrapper
//! (single tool call + SSE push). Remote MCP clients — claude.ai custom
//! connectors, Claude mobile, MCP Inspector, any spec client — speak the MCP
//! *protocol* over the Streamable HTTP transport instead: one endpoint that
//! accepts one JSON-RPC request or notification per POST. The server is
//! stateless (never issues an `Mcp-Session-Id`) and answers requests with a
//! plain `application/json` body; notifications get `202 Accepted`.
//!
//! Dual-era (see `crate::mcp_protocol`):
//!
//! - **legacy** (2024-11-05 … 2025-11-25): `initialize` negotiates a revision;
//!   an unsupported `MCP-Protocol-Version` header ⇒ `400`, an absent one ⇒
//!   an older revision, proceed; `ping` answered;
//! - **modern** (2026-07-28): chosen when the header or the body `_meta`
//!   names a modern revision. Every request must carry `MCP-Protocol-Version`
//!   (equal to `_meta`), `Mcp-Method` (equal to `method`) and, for
//!   `tools/call`, `Mcp-Name` (equal to `params.name`); a missing or
//!   mismatched header ⇒ `400` + `-32020`, a malformed `_meta` ⇒ `400` +
//!   `-32602`, an unknown method ⇒ `404` + `-32601`. Results carry
//!   `resultType` and `serverInfo`; `tools/list` and `server/discover` carry
//!   cache hints. `subscriptions/listen` is acknowledged with no notification
//!   types and completed at once: this stateless endpoint has no way to learn
//!   that a caller's tool set changed, and says so instead of holding an
//!   idle stream open.
//!
//! Both eras: `GET` / `DELETE` ⇒ `405`; `Origin` MUST be validated when
//! present (DNS-rebinding defence): loopback hosts and `config.toml [gateway]
//! allowed_origins` entries are accepted, everything else is `403` (fail
//! closed; absent Origin = a non-browser client, allowed).
//!
//! Auth is the same Bearer surface as `/mcp/v1/call` (mcp_keys +
//! scope/namespace enforcement, plus OAuth-issued tokens). Method surface is
//! deliberately tools-only, matching the stdio server.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::mcp_dispatch::{jsonrpc_error, jsonrpc_response};
use crate::mcp_protocol::RequestEra;
use crate::mcp_http_server::HttpState;
use crate::mcp_rate_limit::OpType;

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
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
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
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "server-initiated streams are not supported",
    )
        .into_response()
}

/// `DELETE /mcp` — stateless server, no sessions to terminate; spec allows
/// answering 405.
pub(crate) async fn mcp_delete_handler() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "sessions are not used by this server",
    )
        .into_response()
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

    // 2. Protocol-version header gate: a revision we do not support in either
    //    era ⇒ 400 with the modern `UnsupportedProtocolVersionError` body
    //    (legacy clients only look at the status). Absent ⇒ decided below.
    let header_version = headers
        .get("MCP-Protocol-Version")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(ver) = header_version.as_deref()
        && !crate::mcp_protocol::is_supported(ver)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(crate::mcp_protocol::unsupported_version_error(&Value::Null, ver)),
        )
            .into_response();
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
                    if let Ok(v) = crate::mcp_oauth_server::www_authenticate_value(&headers).parse()
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
            Json(jsonrpc_error(
                &Value::Null,
                -32600,
                "JSON-RPC batching is not supported",
            )),
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

    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    // 5. Era. `initialize` is always legacy; otherwise a modern revision in
    //    the header or in the body `_meta` selects the modern rules.
    let body_era = if method == "initialize" {
        RequestEra::Legacy
    } else {
        crate::mcp_protocol::request_era(&params)
    };
    let header_is_modern = header_version
        .as_deref()
        .is_some_and(|v| crate::mcp_protocol::MODERN_PROTOCOL_VERSIONS.contains(&v));
    let modern = method != "initialize"
        && (header_is_modern || !matches!(body_era, RequestEra::Legacy));

    // 6. Notifications: accept and answer 202 with no body (spec MUST; the
    //    modern header rules do not cover notification POSTs).
    if !has_id {
        return StatusCode::ACCEPTED.into_response();
    }

    if modern {
        if let Err(resp) = validate_modern(&headers, &id, method, &params, &body_era) {
            return resp;
        }
        return modern_request(&state, &principal, &ns_ctx, &id, method, &params).await;
    }

    // 7. Legacy requests — tools-only surface, mirroring the stdio server.
    let jsonrpc = match method {
        "initialize" => jsonrpc_response(
            &id,
            crate::mcp_protocol::initialize_result(&params, json!({ "tools": {} })),
        ),
        "ping" => jsonrpc_response(&id, json!({})),
        "tools/list" => {
            crate::mcp::handle_tools_list_for_agent(
                &id,
                &principal,
                &state.home_dir,
                &state.dispatcher.default_agent,
            )
            .await
        }
        "tools/call" => match call_tool(&state, &principal, &ns_ctx, &params, &id).await {
            Ok(v) => v,
            Err(secs) => return rate_limited(&id, secs, false),
        },
        other => jsonrpc_error(&id, -32601, &format!("Method not found: {other}")),
    };

    // Single application/json response — the spec's non-streaming server mode
    // (clients MUST accept it).
    (StatusCode::OK, Json(jsonrpc)).into_response()
}

/// `tools/call` with the per-key rate gate and the call timeout. `Err` is
/// the number of seconds the caller must wait (rate limited).
async fn call_tool(
    state: &HttpState,
    principal: &crate::mcp_auth::Principal,
    ns_ctx: &crate::mcp_namespace::NamespaceContext,
    params: &Value,
    id: &Value,
) -> Result<Value, u64> {
    // Same per-key rate gate as the legacy call route.
    if let Err(e) = state
        .dispatcher
        .rate_limiter
        .check(&principal.client_id, OpType::HttpRequest)
    {
        return Err(e.retry_after_secs);
    }
    Ok(match tokio::time::timeout(
        state.call_timeout,
        state
            .dispatcher
            .dispatch_tool_call(principal, ns_ctx, params, id),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => jsonrpc_error(id, -32603, "Request timed out (30s limit exceeded)"),
    })
}

/// The rate-limited answer: JSON-RPC `-32029` (moved out of the MCP-reserved
/// range on a modern response) with `Retry-After`.
fn rate_limited(id: &Value, retry_after_secs: u64, modern: bool) -> Response {
    let err = jsonrpc_error(
        id,
        -32029,
        &format!("HTTP rate limit exceeded, retry after {retry_after_secs} seconds"),
    );
    let body = if modern {
        crate::mcp_protocol::modernize_response(err)
    } else {
        err
    };
    let mut resp = (StatusCode::OK, Json(body)).into_response();
    resp.headers_mut().insert(
        "Retry-After",
        retry_after_secs
            .to_string()
            .parse()
            .unwrap_or_else(|_| "1".parse().unwrap()),
    );
    resp
}

/// A `400` carrying a JSON-RPC error body (modern clients read the body to
/// tell a modern server from a legacy one, so it is never bare).
fn bad_request(id: &Value, code: i64, message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(jsonrpc_error(id, code, message))).into_response()
}

/// Modern request validation (2026-07-28 Streamable HTTP "Server
/// Validation" plus the per-request `_meta` rules).
fn validate_modern(
    headers: &HeaderMap,
    id: &Value,
    method: &str,
    params: &Value,
    body_era: &RequestEra,
) -> Result<(), Response> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let body_version = match body_era {
        RequestEra::Modern(v) => *v,
        RequestEra::Unsupported(v) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(crate::mcp_protocol::unsupported_version_error(id, v)),
            )
                .into_response());
        }
        RequestEra::Legacy => {
            return Err(bad_request(
                id,
                -32602,
                &format!(
                    "missing required _meta[\"{}\"]",
                    crate::mcp_protocol::META_PROTOCOL_VERSION
                ),
            ));
        }
    };
    let mismatch = crate::mcp_protocol::HEADER_MISMATCH;
    match header("MCP-Protocol-Version") {
        None => return Err(bad_request(id, mismatch, "Missing MCP-Protocol-Version header")),
        Some(v) if v != body_version => {
            return Err(bad_request(
                id,
                mismatch,
                &format!(
                    "Header mismatch: MCP-Protocol-Version header value '{v}' does not match body value '{body_version}'"
                ),
            ));
        }
        Some(_) => {}
    }
    if let Some(problem) = crate::mcp_protocol::modern_meta_problem(params) {
        return Err(bad_request(id, -32602, &problem));
    }
    match header("Mcp-Method") {
        None => return Err(bad_request(id, mismatch, "Missing Mcp-Method header")),
        Some(v) if v != method => {
            return Err(bad_request(
                id,
                mismatch,
                &format!(
                    "Header mismatch: Mcp-Method header value '{v}' does not match body value '{method}'"
                ),
            ));
        }
        Some(_) => {}
    }
    if method == "tools/call" {
        let body_name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let decoded = header("Mcp-Name").map(crate::mcp_protocol::decode_header_value);
        match decoded {
            None => return Err(bad_request(id, mismatch, "Missing Mcp-Name header")),
            Some(None) => return Err(bad_request(id, mismatch, "Invalid Mcp-Name header value")),
            Some(Some(v)) if v != body_name => {
                return Err(bad_request(
                    id,
                    mismatch,
                    &format!(
                        "Header mismatch: Mcp-Name header value '{v}' does not match body value '{body_name}'"
                    ),
                ));
            }
            Some(Some(_)) => {}
        }
    }
    Ok(())
}

/// Serve one validated modern request.
async fn modern_request(
    state: &HttpState,
    principal: &crate::mcp_auth::Principal,
    ns_ctx: &crate::mcp_namespace::NamespaceContext,
    id: &Value,
    method: &str,
    params: &Value,
) -> Response {
    use crate::mcp_protocol::{modernize_response, with_cache_hints};
    let jsonrpc = match method {
        "server/discover" => jsonrpc_response(
            id,
            crate::mcp_protocol::discover_result(json!({ "tools": {} })),
        ),
        "tools/list" => with_cache_hints(
            crate::mcp::handle_tools_list_for_agent(
                id,
                principal,
                &state.home_dir,
                &state.dispatcher.default_agent,
            )
            .await,
            crate::mcp_protocol::TOOLS_LIST_TTL_MS,
        ),
        "tools/call" => match call_tool(state, principal, ns_ctx, params, id).await {
            Ok(v) => v,
            Err(secs) => return rate_limited(id, secs, true),
        },
        "subscriptions/listen" => return listen_and_complete(id),
        other => {
            return (
                StatusCode::NOT_FOUND,
                Json(modernize_response(jsonrpc_error(
                    id,
                    -32601,
                    &format!("Method not found: {other}"),
                ))),
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(modernize_response(jsonrpc))).into_response()
}

/// `subscriptions/listen` on this stateless endpoint: an SSE stream holding
/// the acknowledgement (no notification types honoured) and the completion
/// result that ends the subscription from the server side.
fn listen_and_complete(id: &Value) -> Response {
    let ack = crate::mcp_protocol::subscription_ack(id, false);
    let done = crate::mcp_protocol::subscription_complete(id);
    let body = format!("event: message\ndata: {ack}\n\nevent: message\ndata: {done}\n\n");
    let mut resp = (StatusCode::OK, body).into_response();
    let h = resp.headers_mut();
    h.insert("Content-Type", "text/event-stream".parse().unwrap());
    h.insert("Cache-Control", "no-cache".parse().unwrap());
    h.insert("X-Accel-Buffering", "no".parse().unwrap());
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_entries_normalize_and_loopback_always_allowed() {
        assert_eq!(
            normalize_origin_entry(" https://Example.com/ "),
            Some("Example.com".into())
        );
        assert_eq!(
            normalize_origin_entry("chrome-ext-id"),
            Some("chrome-ext-id".into())
        );
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
