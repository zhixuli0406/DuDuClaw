// mcp_http_server.rs — Axum-based HTTP/SSE MCP server (W20-P1 Phase 2B/C)
//
// Routes:
//   POST /mcp/v1/call               — single JSON-RPC 2.0 tool call (Bearer auth only)
//   GET  /mcp/v1/stream             — SSE long-lived event stream (Bearer or ?api_key=)
//   POST /mcp/v1/stream/call        — inject tool call into a named SSE stream
//   GET  /healthz                   — health check (no auth)
//
// W22-P0 ADR-002: All responses carry x-duduclaw-version + x-duduclaw-capabilities headers
// via the inject_capability_headers + negotiate_capabilities middleware stack.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tracing::info;

use crate::mcp_auth::{authenticate_with_key, Principal};
use crate::mcp_capability::{inject_capability_headers, negotiate_capabilities};
use crate::mcp_dispatch::McpDispatcher;
use crate::mcp_http_errors::into_axum_response;
use crate::mcp_namespace::{resolve, NamespaceContext};
use crate::mcp_rate_limit::OpType;
use crate::mcp_sse_store::SseEventStore;

// ── Server config ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HttpServerConfig {
    pub bind: SocketAddr,
    pub home_dir: std::path::PathBuf,
    pub enable_sse: bool,
    /// Request timeout for tool calls (default 30s).
    pub call_timeout: Duration,
}

impl HttpServerConfig {
    pub fn new(bind: SocketAddr, home_dir: std::path::PathBuf) -> Self {
        Self {
            bind,
            home_dir,
            enable_sse: true,
            call_timeout: Duration::from_secs(30),
        }
    }
}

// ── Shared Axum state ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HttpState {
    pub dispatcher: Arc<McpDispatcher>,
    pub sse_store: Arc<SseEventStore>,
    pub call_timeout: Duration,
    pub home_dir: std::path::PathBuf,
}

// ── Auth helper ───────────────────────────────────────────────────────────────

/// Extract and validate a Bearer API key from headers.
/// Returns (Principal, NamespaceContext) or an error Response.
fn authenticate_bearer(
    headers: &HeaderMap,
    home_dir: &std::path::Path,
) -> Result<(Principal, NamespaceContext), Response> {
    let auth_header = headers
        .get("Authorization")
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null,
                    -32003,
                    "Missing Authorization header. Use: Authorization: Bearer <key>",
                )),
            )
                .into_response()
        })?
        .to_str()
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null,
                    -32003,
                    "Invalid Authorization header encoding",
                )),
            )
                .into_response()
        })?;

    let raw_key = auth_header
        .strip_prefix("Bearer ")
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null,
                    -32003,
                    "Authorization header must use: Bearer <key>",
                )),
            )
                .into_response()
        })?;

    let principal = authenticate_with_key(raw_key, home_dir).map_err(|e| {
        (
            StatusCode::UNAUTHORIZED,
            Json(crate::mcp_dispatch::jsonrpc_error(
                &Value::Null,
                -32003,
                &format!("Authentication failed: {e}"),
            )),
        )
            .into_response()
    })?;

    let ns_ctx = resolve(&principal).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::mcp_dispatch::jsonrpc_error(
                &Value::Null,
                -32603,
                &format!("Namespace resolution failed: {e}"),
            )),
        )
            .into_response()
    })?;

    Ok((principal, ns_ctx))
}

/// Extract and validate a Bearer key OR `?api_key=` query param.
fn authenticate_bearer_or_query(
    headers: &HeaderMap,
    query_api_key: Option<&str>,
    home_dir: &std::path::Path,
) -> Result<(Principal, NamespaceContext), Response> {
    // Prefer Authorization header; fall back to query param (SSE only)
    if headers.contains_key("Authorization") {
        return authenticate_bearer(headers, home_dir);
    }

    if let Some(raw_key) = query_api_key.filter(|k| !k.is_empty()) {
        let principal = authenticate_with_key(raw_key, home_dir).map_err(|e| {
            (
                StatusCode::UNAUTHORIZED,
                Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null,
                    -32003,
                    &format!("Authentication failed: {e}"),
                )),
            )
                .into_response()
        })?;
        let ns_ctx = resolve(&principal).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::mcp_dispatch::jsonrpc_error(
                    &Value::Null,
                    -32603,
                    &format!("Namespace resolution failed: {e}"),
                )),
            )
                .into_response()
        })?;
        return Ok((principal, ns_ctx));
    }

    Err((
        StatusCode::UNAUTHORIZED,
        Json(crate::mcp_dispatch::jsonrpc_error(
            &Value::Null,
            -32003,
            "Missing API key. Provide Authorization: Bearer <key> or ?api_key=<key>",
        )),
    )
        .into_response())
}

// ── Router builder (extracted for testability) ────────────────────────────────

/// Build the Axum router with all MCP HTTP routes attached to `state`.
/// Extracted from `run` so integration tests can call routes without binding a port.
pub fn build_router(cfg: &HttpServerConfig, dispatcher: McpDispatcher) -> Router {
    let state = HttpState {
        dispatcher: Arc::new(dispatcher),
        sse_store: Arc::new(SseEventStore::new()),
        call_timeout: cfg.call_timeout,
        home_dir: cfg.home_dir.clone(),
    };

    let mut router = Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/mcp/v1/call", post(call_handler));

    if cfg.enable_sse {
        router = router
            .route("/mcp/v1/stream", get(stream_handler))
            .route("/mcp/v1/stream/call", post(stream_call_handler));
    }

    // W22-P0 ADR-002: inject x-duduclaw-version + x-duduclaw-capabilities into all responses.
    // Layer order (axum: last .layer() = outermost = runs first on request, last on response):
    //   outer: inject_capability_headers — adds standard headers to ALL responses (incl. 422)
    //   inner: negotiate_capabilities   — validates client's x-duduclaw-capabilities header
    router
        .with_state(state)
        .layer(middleware::from_fn(negotiate_capabilities))   // inner
        .layer(middleware::from_fn(inject_capability_headers)) // outer
}

// ── Server entry point ────────────────────────────────────────────────────────

pub async fn run(cfg: HttpServerConfig, dispatcher: McpDispatcher) -> Result<(), String> {
    let router = build_router(&cfg, dispatcher);

    info!(bind = %cfg.bind, "MCP HTTP server listening");

    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .map_err(|e| format!("Failed to bind {}: {e}", cfg.bind))?;

    axum::serve(listener, router)
        .await
        .map_err(|e| format!("HTTP server error: {e}"))
}

// ── GET /healthz ──────────────────────────────────────────────────────────────

async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

// ── POST /mcp/v1/call ─────────────────────────────────────────────────────────

/// Single JSON-RPC 2.0 tool call.
/// Authentication: `Authorization: Bearer <key>` (query params NOT accepted).
async fn call_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // Authenticate (header only — no query param to prevent access log leakage)
    let (principal, ns_ctx) = match authenticate_bearer(&headers, &state.home_dir) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let id = body.get("id").cloned().unwrap_or(Value::Null);

    // Validate JSON-RPC
    if body.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        return into_axum_response(crate::mcp_dispatch::jsonrpc_error(
            &id, -32600, "jsonrpc field must be '2.0'",
        ));
    }
    let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");
    if method != "tools/call" {
        return into_axum_response(crate::mcp_dispatch::jsonrpc_error(
            &id,
            -32601,
            &format!("Method not found: '{method}'. Use 'tools/call'"),
        ));
    }

    // ── HTTP rate gate: 60 req/min per API key ────────────────────────────────
    if let Err(e) = state.dispatcher.rate_limiter.check(&principal.client_id, OpType::HttpRequest) {
        let mut resp = into_axum_response(crate::mcp_dispatch::jsonrpc_error(
            &id,
            -32029,
            &format!("HTTP rate limit exceeded, retry after {} seconds", e.retry_after_secs),
        ));
        resp.headers_mut().insert(
            "Retry-After",
            e.retry_after_secs.to_string().parse().unwrap_or_else(|_| "1".parse().unwrap()),
        );
        return resp;
    }

    let params = body.get("params").cloned().unwrap_or(Value::Null);

    // Dispatch with timeout
    let jsonrpc = match tokio::time::timeout(
        state.call_timeout,
        state.dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => crate::mcp_dispatch::jsonrpc_error(
            &id, -32603, "Request timed out (30s limit exceeded)",
        ),
    };

    into_axum_response(jsonrpc)
}

// ── GET /mcp/v1/stream ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StreamQuery {
    conn_id: Option<String>,
    api_key: Option<String>,
}

async fn stream_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> impl IntoResponse {
    // Auth: Bearer header OR ?api_key= (SSE clients may not support custom headers)
    let (principal, _ns_ctx) =
        match authenticate_bearer_or_query(&headers, query.api_key.as_deref(), &state.home_dir) {
            Ok(p) => p,
            Err(r) => return r,
        };

    let conn_id = query
        .conn_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let (tx, rx) = broadcast::channel::<String>(256);
    state.sse_store.register_connection(&conn_id, tx);

    let conn_id_clone = conn_id.clone();
    let client_id = principal.client_id.clone();

    // Convert broadcast receiver to a Stream of SSE Events
    let bcast_stream =
        tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |msg| {
            let conn_id = conn_id_clone.clone();
            match msg {
                Ok(data) => Some(Ok::<Event, std::convert::Infallible>(
                    Event::default().id(conn_id).data(data),
                )),
                Err(_) => None, // lagged or closed
            }
        });

    // First event: "connected"
    let connected_data = serde_json::json!({
        "type": "connected",
        "conn_id": conn_id,
        "client_id": client_id,
    })
    .to_string();

    let initial = tokio_stream::once(Ok::<Event, std::convert::Infallible>(
        Event::default().event("connected").data(connected_data),
    ));

    let combined = initial.chain(bcast_stream);

    Sse::new(combined)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)).text("heartbeat"))
        .into_response()
}

// ── POST /mcp/v1/stream/call ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StreamCallQuery {
    conn_id: String,
}

async fn stream_call_handler(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): Query<StreamCallQuery>,
    Json(body): Json<Value>,
) -> Response {
    let (principal, ns_ctx) = match authenticate_bearer(&headers, &state.home_dir) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    // HTTP rate gate
    if let Err(e) = state.dispatcher.rate_limiter.check(&principal.client_id, OpType::HttpRequest) {
        return into_axum_response(crate::mcp_dispatch::jsonrpc_error(
            &id,
            -32029,
            &format!("HTTP rate limit exceeded, retry after {} seconds", e.retry_after_secs),
        ));
    }

    let conn_id = query.conn_id.clone();

    // Push progress event
    let progress = serde_json::json!({
        "status": "running",
        "tool": params.get("name").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "conn_id": conn_id,
    });
    state.sse_store.push_event(&conn_id, "tool_progress", &progress.to_string());

    // Dispatch tool call
    let jsonrpc = tokio::time::timeout(
        state.call_timeout,
        state.dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id),
    )
    .await
    .unwrap_or_else(|_| {
        crate::mcp_dispatch::jsonrpc_error(&id, -32603, "Request timed out (30s limit exceeded)")
    });

    // Push result to SSE stream
    state.sse_store.push_event(&conn_id, "tool_result", &jsonrpc.to_string());

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true, "conn_id": conn_id })),
    )
        .into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // provides Router::oneshot

    use super::*;
    use crate::mcp_dispatch::McpDispatcher;
    use crate::mcp_memory_quota::DailyQuota;
    use crate::mcp_rate_limit::RateLimiter;

    // ── Header name constants (mirrors mcp_capability.rs) ─────────────────────

    const HDR_VERSION: &str = "x-duduclaw-version";
    const HDR_CAPABILITIES: &str = "x-duduclaw-capabilities";
    const HDR_MISSING_CAPABILITIES: &str = "x-duduclaw-missing-capabilities";

    // ── Helpers ────────────────────────────────────────────────────────────────

    pub fn ephemeral_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    }

    /// Build a McpDispatcher backed by an in-memory SQLite database.
    /// No real tools are called in these tests; we only need the router to be valid.
    fn make_test_dispatcher() -> McpDispatcher {
        let home_dir = std::env::temp_dir().join("duduclaw_http_server_test");
        let _ = std::fs::create_dir_all(&home_dir);
        let http = reqwest::Client::new();
        let memory = Arc::new(
            duduclaw_memory::SqliteMemoryEngine::in_memory().expect("in-memory db"),
        );
        let odoo = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
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

    /// Build a default HttpServerConfig pointing at a temp dir.
    fn test_config() -> HttpServerConfig {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        HttpServerConfig::new(addr, std::env::temp_dir())
    }

    /// Shorthand: GET /healthz with no extra headers.
    fn healthz_request() -> Request<Body> {
        Request::builder().uri("/healthz").body(Body::empty()).unwrap()
    }

    /// Shorthand: GET /healthz with x-duduclaw-capabilities header.
    fn healthz_with_caps(caps: &str) -> Request<Body> {
        Request::builder()
            .uri("/healthz")
            .header(HDR_CAPABILITIES, caps)
            .body(Body::empty())
            .unwrap()
    }

    // ── HttpServerConfig tests ─────────────────────────────────────────────────

    #[test]
    fn ephemeral_port_binds_successfully() {
        let addr = ephemeral_addr();
        assert!(addr.port() > 0);
    }

    #[test]
    fn config_new_has_correct_defaults() {
        let cfg = test_config();
        assert!(cfg.enable_sse, "SSE should be enabled by default");
        assert_eq!(
            cfg.call_timeout,
            Duration::from_secs(30),
            "default call timeout should be 30s"
        );
    }

    #[test]
    fn config_stores_bind_addr_and_home_dir() {
        let bind: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let home = std::path::PathBuf::from("/tmp/test-home");
        let cfg = HttpServerConfig::new(bind, home.clone());
        assert_eq!(cfg.bind, bind);
        assert_eq!(cfg.home_dir, home);
    }

    // ── build_router smoke tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn healthz_returns_200_ok() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_response_body_contains_ok_status() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["status"], "ok", "healthz body must contain status: ok");
    }

    // ── ADR-002 header injection via build_router ─────────────────────────────

    #[tokio::test]
    async fn healthz_always_has_version_header() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let version = resp
            .headers()
            .get(HDR_VERSION)
            .expect("x-duduclaw-version must be injected by middleware");
        assert_eq!(version.to_str().unwrap(), "1.2");
    }

    #[tokio::test]
    async fn healthz_always_has_capabilities_header() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let caps = resp
            .headers()
            .get(HDR_CAPABILITIES)
            .expect("x-duduclaw-capabilities must be injected by middleware");
        let caps_str = caps.to_str().unwrap();
        assert!(caps_str.starts_with("memory/"), "memory must be first cap: {caps_str}");
    }

    #[tokio::test]
    async fn healthz_capabilities_header_lists_enabled_caps_only() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_request()).await.unwrap();
        let caps_str = resp.headers()[HDR_CAPABILITIES].to_str().unwrap();
        assert!(caps_str.contains("mcp/2"),    "mcp/2 must be listed: {caps_str}");
        assert!(caps_str.contains("audit/2"),  "audit/2 must be listed: {caps_str}");
        assert!(!caps_str.contains("a2a/"),    "disabled a2a must be absent: {caps_str}");
    }

    // ── Capability negotiation through the full router ────────────────────────

    #[tokio::test]
    async fn healthz_with_satisfied_capability_returns_200() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_with_caps("memory/3,mcp/2")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_with_disabled_capability_returns_422() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        // a2a is disabled in the registry → 422
        let resp = router.oneshot(healthz_with_caps("a2a/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn healthz_422_includes_missing_capabilities_header() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_with_caps("a2a/1,secret-manager/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let missing = resp
            .headers()
            .get(HDR_MISSING_CAPABILITIES)
            .expect("422 must include x-duduclaw-missing-capabilities");
        let missing_str = missing.to_str().unwrap();
        assert!(missing_str.contains("a2a/1"), "a2a must be listed as missing: {missing_str}");
    }

    #[tokio::test]
    async fn healthz_422_still_has_standard_duduclaw_headers() {
        // Even a 422 from negotiate_capabilities must carry the standard headers
        // (inject_capability_headers is the outer layer — runs on ALL responses)
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let resp = router.oneshot(healthz_with_caps("a2a/1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            resp.headers().contains_key(HDR_VERSION),
            "422 must carry x-duduclaw-version"
        );
        assert!(
            resp.headers().contains_key(HDR_CAPABILITIES),
            "422 must carry x-duduclaw-capabilities"
        );
    }

    // ── Authentication guard on protected endpoints ───────────────────────────

    #[tokio::test]
    async fn call_endpoint_returns_401_without_auth() {
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sse_stream_endpoint_returns_401_without_auth() {
        let cfg = test_config(); // enable_sse: true by default
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .uri("/mcp/v1/stream")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ── SSE routing configuration ─────────────────────────────────────────────

    #[tokio::test]
    async fn sse_stream_returns_404_when_sse_disabled() {
        let mut cfg = test_config();
        cfg.enable_sse = false;
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .uri("/mcp/v1/stream")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "SSE endpoint must be absent when enable_sse = false"
        );
    }

    // ── Authentication header format error paths ──────────────────────────────

    #[tokio::test]
    async fn call_endpoint_returns_401_with_non_bearer_auth() {
        // Authorization header exists but doesn't start with "Bearer "
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Authorization", "Basic dXNlcjpwYXNz")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Non-Bearer auth scheme must return 401"
        );
    }

    #[tokio::test]
    async fn call_endpoint_returns_401_with_empty_bearer_token() {
        // "Bearer " prefix present but token is empty
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Authorization", "Bearer ")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Empty Bearer token must return 401"
        );
    }

    #[tokio::test]
    async fn call_endpoint_returns_401_with_invalid_bearer_token() {
        // Valid format but unknown key → authenticate_with_key fails
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/call")
            .header("Authorization", "Bearer invalid-key-that-does-not-exist")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Invalid Bearer token must return 401"
        );
    }

    #[tokio::test]
    async fn sse_stream_returns_401_with_invalid_query_api_key() {
        // SSE: no Bearer header, but ?api_key= is invalid
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .uri("/mcp/v1/stream?api_key=invalid-key")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Invalid query api_key must return 401"
        );
    }

    #[tokio::test]
    async fn stream_call_endpoint_returns_401_without_auth() {
        // POST /mcp/v1/stream/call requires Bearer auth
        let cfg = test_config();
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/stream/call?conn_id=test-conn")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "stream/call without auth must return 401"
        );
    }

    #[tokio::test]
    async fn stream_call_returns_404_when_sse_disabled() {
        let mut cfg = test_config();
        cfg.enable_sse = false;
        let router = build_router(&cfg, make_test_dispatcher());
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/v1/stream/call?conn_id=test-conn")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"tools/call","id":1}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "stream/call endpoint must be absent when SSE disabled"
        );
    }
}
