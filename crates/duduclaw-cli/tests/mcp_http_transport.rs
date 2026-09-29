//! MCP HTTP/SSE Transport 整合測試 (W20-P1 Phase 2B/2C)
//!
//! 審查員：QA1-DuDuClaw
//! 日期：2026-05-01
//!
//! 測試矩陣：
//! - TC-HTTP-01: GET /healthz → 200 OK
//! - TC-HTTP-02: POST /mcp/v1/call 無 Authorization → 401
//! - TC-HTTP-03: POST /mcp/v1/call Authorization Bearer invalid → 401
//! - TC-HTTP-04: POST /mcp/v1/call jsonrpc 欄位非 "2.0" → 400 + JSON-RPC error
//! - TC-HTTP-05: POST /mcp/v1/call method != "tools/call" → JSON-RPC -32601
//! - TC-HTTP-06: GET /mcp/v1/stream 無認證 → 401
//! - TC-HTTP-07: SSE 停用時 /mcp/v1/stream → 404
//! - TC-HTTP-08: HTTP Rate limit 超過 60 req/min → 429 + -32029
//! - TC-HTTP-09: GET /mcp/v1/stream 接受 ?api_key= query param
//! - TC-HTTP-10: stream/call 無 conn_id → 422 (missing query param)

use std::io::Write;
use std::sync::Arc;

use axum::body::Body;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;
use duduclaw_cli::mcp_dispatch::McpDispatcher;
use duduclaw_cli::mcp_http_server::{build_router, HttpServerConfig};
use duduclaw_cli::mcp_memory_quota::DailyQuota;
use duduclaw_cli::mcp_rate_limit::RateLimiter;
use duduclaw_cli::odoo_pool::OdooConnectorPool;
use duduclaw_memory::SqliteMemoryEngine;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a temp home dir with a single API key
fn make_home_with_key(key: &str, client_id: &str, scopes: &[&str]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let scopes_toml = scopes
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    // Use a fresh timestamp: keys expire after 30 days (mcp_auth.rs), so a
    // hardcoded date becomes a time-bomb that fails these tests once it ages
    // past the rotation window. Generate "now" so the key is always valid.
    let created_at = chrono::Utc::now().to_rfc3339();
    let content = format!(
        r#"
[mcp_keys."{key}"]
client_id = "{client_id}"
scopes = [{scopes_toml}]
created_at = "{created_at}"
is_external = true
"#
    );
    let mut f = std::fs::File::create(dir.path().join("config.toml")).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    dir
}

/// Build a minimal McpDispatcher rooted at `home_dir`.
fn make_dispatcher(home_dir: &std::path::Path) -> McpDispatcher {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let memory_db = home_dir.join("memory.db");
    let memory = SqliteMemoryEngine::new(&memory_db).unwrap();
    McpDispatcher::new(
        home_dir.to_path_buf(),
        http,
        Arc::new(memory),
        "test-agent".to_string(),
        Arc::new(OdooConnectorPool::default()),
        RateLimiter::new(),
        DailyQuota::new(),
    )
}

/// Build a config for an HTTP server with SSE enabled.
fn make_cfg(home_dir: &std::path::Path) -> HttpServerConfig {
    HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home_dir.to_path_buf())
}

// ── TC-HTTP-01: GET /healthz → 200 ───────────────────────────────────────────

#[tokio::test]
async fn tc_http_01_healthz_returns_200() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok", "healthz 應回傳 {{status: ok}}");
}

// ── TC-HTTP-02: POST /mcp/v1/call 無 Authorization → 401 ─────────────────────

#[tokio::test]
async fn tc_http_02_call_without_auth_returns_401() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp/v1/call")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "無 Authorization header 應回傳 401"
    );
}

// ── TC-HTTP-03: Invalid Bearer key → 401 ─────────────────────────────────────

#[tokio::test]
async fn tc_http_03_invalid_bearer_key_returns_401() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp/v1/call")
        .header("Authorization", "Bearer ddc_prod_00000000000000000000000000000000")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "不存在的 API key 應回傳 401"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // JSON-RPC error code -32003 = Unauthorized
    assert_eq!(
        json["error"]["code"],
        -32003,
        "應回傳 JSON-RPC error code -32003"
    );
}

// ── TC-HTTP-04: jsonrpc != "2.0" → 400 + -32600 ──────────────────────────────

#[tokio::test]
async fn tc_http_04_invalid_jsonrpc_version_returns_error() {
    let key = "ddc_dev_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";
    let dir = make_home_with_key(key, "test-client", &["memory:read", "memory:write"]);
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let body = serde_json::json!({
        "jsonrpc": "1.0",   // 錯誤版本
        "id": 1,
        "method": "tools/call",
        "params": {}
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp/v1/call")
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    // 可接受 400 或 200（JSON-RPC over HTTP 兩種均合規）
    // 重點是 body 含 error.code = -32600
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        json["error"]["code"],
        -32600,
        "jsonrpc != '2.0' 應回傳 -32600 Invalid Request"
    );
}

// ── TC-HTTP-05: method != "tools/call" → -32601 ───────────────────────────────

#[tokio::test]
async fn tc_http_05_unknown_method_returns_method_not_found() {
    let key = "ddc_dev_b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5";
    let dir = make_home_with_key(key, "test-client2", &["memory:read", "memory:write"]);
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "unknown/method",
        "params": {}
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp/v1/call")
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        json["error"]["code"],
        -32601,
        "未知 method 應回傳 -32601 Method Not Found"
    );
    let msg = json["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("tools/call"), "錯誤訊息應提示正確 method");
}

// ── TC-HTTP-06: GET /mcp/v1/stream 無認證 → 401 ──────────────────────────────

#[tokio::test]
async fn tc_http_06_stream_without_auth_returns_401() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/mcp/v1/stream")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "SSE stream 無認證應回傳 401"
    );
}

// ── TC-HTTP-07: SSE 停用時 /mcp/v1/stream → 404 ──────────────────────────────

#[tokio::test]
async fn tc_http_07_stream_disabled_returns_404() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let mut cfg = make_cfg(dir.path());
    cfg.enable_sse = false; // 停用 SSE
    let app = build_router(&cfg, dispatcher);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/mcp/v1/stream")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "SSE 停用時 /mcp/v1/stream 應回傳 404"
    );
}

// ── TC-HTTP-08: HTTP Rate limit 61 次 → 429 + -32029 ─────────────────────────

#[tokio::test]
async fn tc_http_08_http_rate_limit_triggers_after_60_requests() {
    let key = "ddc_dev_c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6";
    let dir = make_home_with_key(key, "rate-limit-client", &["memory:read", "memory:write"]);
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let valid_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "memory_read", "arguments": { "agent_id": "test" } }
    })
    .to_string();

    // 注意：HTTP rate limit 為 60 req/min；
    // 61 次後應看到 -32029。我們用 Arc<Router> 共享狀態。
    // tower::ServiceExt::oneshot 會消耗 service，因此需要用 into_make_service

    // Build a real listener to get shared state across multiple requests
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let key_clone = key.to_string();
    let valid_body_clone = valid_body.clone();

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let http_client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp/v1/call");

    let mut last_status = 200u16;
    let mut got_rate_limit = false;

    for _i in 0..62u32 {
        let resp = http_client
            .post(&url)
            .header("Authorization", format!("Bearer {key_clone}"))
            .header("Content-Type", "application/json")
            .body(valid_body_clone.clone())
            .send()
            .await
            .unwrap();

        last_status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap();

        if let Some(code) = body["error"]["code"].as_i64() {
            if code == -32029 {
                got_rate_limit = true;
                break;
            }
        }
    }

    server.abort();

    assert!(
        got_rate_limit,
        "61 次 HTTP 請求後應觸發 -32029 rate limit（最後 status={}）",
        last_status
    );
}

// ── TC-HTTP-09: /mcp/v1/stream 接受 ?api_key= ────────────────────────────────

#[tokio::test]
async fn tc_http_09_stream_accepts_api_key_query_param() {
    let key = "ddc_dev_d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1";
    let dir = make_home_with_key(key, "sse-client", &["memory:read"]);
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    // ?api_key= 應被接受（SSE 客戶端不支援自定義 header）
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/mcp/v1/stream?api_key={key}"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    // SSE 連線成功，應回傳 200 + text/event-stream
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "?api_key= 應被 SSE endpoint 接受"
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("text/event-stream"),
        "SSE 端點應回傳 text/event-stream content-type，實際：{ct}"
    );
}

// ── TC-HTTP-11: ADR-002 — healthz 回應攜帶 x-duduclaw-* 標頭 ────────────────────
//
// 驗證 inject_capability_headers middleware 已正確掛載到 build_router。
// 即使是無需認證的 /healthz 端點，每個回應都必須攜帶 ADR-002 標準標頭。

#[tokio::test]
async fn tc_http_11_adr002_headers_present_on_healthz() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // ADR-002 §3.1: 每個回應（成功 or 錯誤）都必須帶 x-duduclaw-version
    let version = resp
        .headers()
        .get("x-duduclaw-version")
        .expect("TC-HTTP-11: x-duduclaw-version 必須出現在 healthz 回應中");
    assert_eq!(
        version.to_str().unwrap(),
        "1.2",
        "TC-HTTP-11: x-duduclaw-version 必須是 1.2"
    );

    // ADR-002 §3.1: 每個回應都必須帶 x-duduclaw-capabilities
    let caps = resp
        .headers()
        .get("x-duduclaw-capabilities")
        .expect("TC-HTTP-11: x-duduclaw-capabilities 必須出現在 healthz 回應中");
    let caps_str = caps.to_str().unwrap();
    assert!(
        caps_str.starts_with("memory/"),
        "TC-HTTP-11: capabilities 必須以 memory 開頭，實際：{caps_str}"
    );
    assert!(
        caps_str.contains("mcp/2"),
        "TC-HTTP-11: capabilities 必須包含 mcp/2，實際：{caps_str}"
    );
    assert!(
        !caps_str.contains("a2a/"),
        "TC-HTTP-11: 停用的 a2a 不應出現在 capabilities，實際：{caps_str}"
    );
}

// ── TC-HTTP-12: ADR-002 — 401 錯誤回應也攜帶 x-duduclaw-* 標頭 ──────────────────
//
// ADR-002 §3.3.3 規定：即使是認證失敗的 4xx 回應，也必須攜帶標準 DuDuClaw 標頭。
// inject_capability_headers 作為最外層 middleware，確保所有回應路徑都有標頭。

#[tokio::test]
async fn tc_http_12_adr002_headers_present_on_401_error() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    // 不帶 Authorization header → 401
    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp/v1/call")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "TC-HTTP-12: 應回傳 401");

    // 即使是 401，也必須攜帶 ADR-002 標頭
    assert!(
        resp.headers().contains_key("x-duduclaw-version"),
        "TC-HTTP-12: 401 回應必須攜帶 x-duduclaw-version (ADR-002 §3.1)"
    );
    assert!(
        resp.headers().contains_key("x-duduclaw-capabilities"),
        "TC-HTTP-12: 401 回應必須攜帶 x-duduclaw-capabilities (ADR-002 §3.1)"
    );
    let version = resp.headers()["x-duduclaw-version"].to_str().unwrap();
    assert_eq!(version, "1.2", "TC-HTTP-12: x-duduclaw-version 必須是 1.2");
}

// ── TC-HTTP-13: ADR-002 — 能力協商 422 via build_router ─────────────────────────
//
// 端到端驗證：請求攜帶 x-duduclaw-capabilities: a2a/1（停用能力），
// negotiate_capabilities middleware 應回傳 422，
// inject_capability_headers middleware 應為 422 回應加上標準標頭，
// 422 body 應符合 ADR-002 §3.3.3 JSON 格式。

#[tokio::test]
async fn tc_http_13_adr002_capability_negotiation_422_via_build_router() {
    let dir = TempDir::new().unwrap();
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    // a2a/1 是停用能力 → negotiate_capabilities 應拒絕並回傳 422
    let req = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .header("x-duduclaw-capabilities", "a2a/1")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "TC-HTTP-13: 停用能力請求應回傳 422"
    );

    // 422 回應也必須攜帶標準 ADR-002 標頭（由外層 inject_capability_headers 注入）
    assert!(
        resp.headers().contains_key("x-duduclaw-version"),
        "TC-HTTP-13: 422 回應必須攜帶 x-duduclaw-version"
    );
    assert!(
        resp.headers().contains_key("x-duduclaw-capabilities"),
        "TC-HTTP-13: 422 回應必須攜帶 x-duduclaw-capabilities"
    );

    // 422 回應必須攜帶 x-duduclaw-missing-capabilities
    let missing_hdr = resp
        .headers()
        .get("x-duduclaw-missing-capabilities")
        .expect("TC-HTTP-13: 422 必須攜帶 x-duduclaw-missing-capabilities");
    let missing_str = missing_hdr.to_str().unwrap();
    assert!(
        missing_str.contains("a2a/1"),
        "TC-HTTP-13: missing-capabilities 必須包含 a2a/1，實際：{missing_str}"
    );

    // 驗證 422 body 格式 (ADR-002 §3.3.3)
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        json["error"],
        "capability_mismatch",
        "TC-HTTP-13: body.error 必須是 capability_mismatch"
    );
    assert!(
        json["missing"].is_array(),
        "TC-HTTP-13: body.missing 必須是陣列"
    );
    let missing_arr = json["missing"].as_array().unwrap();
    assert_eq!(missing_arr.len(), 1, "TC-HTTP-13: 應有 1 個缺失能力");
    assert_eq!(
        missing_arr[0]["capability"],
        "a2a",
        "TC-HTTP-13: 缺失能力名稱應為 a2a"
    );
    assert_eq!(
        missing_arr[0]["required_version"],
        1,
        "TC-HTTP-13: 要求版本應為 1"
    );
    assert!(
        missing_arr[0]["server_version"].is_null(),
        "TC-HTTP-13: 停用能力的 server_version 應為 null"
    );
}

// ── TC-HTTP-10: stream/call 無 conn_id → 422 ─────────────────────────────────

#[tokio::test]
async fn tc_http_10_stream_call_missing_conn_id_returns_422() {
    let key = "ddc_dev_e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let dir = make_home_with_key(key, "stream-call-client", &["memory:read", "memory:write"]);
    let dispatcher = make_dispatcher(dir.path());
    let cfg = make_cfg(dir.path());
    let app = build_router(&cfg, dispatcher);

    // conn_id 是必填 query param — 缺少時 Axum 應回傳 422
    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp/v1/stream/call")  // 缺 ?conn_id=
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_read","arguments":{}}}"#,
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    // Axum 0.7 的 Query extractor 在 required field 缺失時回傳 400 Bad Request
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "缺 conn_id 應回傳 400 Bad Request（Axum Query extractor 行為）"
    );
}

// ── H13: Streamable-HTTP (`/mcp`) + Remote-OAuth coverage ────────────────────
//
// Both surfaces are remote-reachable and were shipping with 2 and 5 unit tests
// respectively, none of them exercising the HTTP layer. These tests drive the
// real router.

/// Build a temp home carrying one INTERNAL key (operator proof for the OAuth
/// consent step) alongside the external-key helper above.
fn make_home_with_internal_key(key: &str, client_id: &str, scopes: &[&str]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let scopes_toml = scopes
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let created_at = chrono::Utc::now().to_rfc3339();
    let content = format!(
        r#"
[mcp_keys."{key}"]
client_id = "{client_id}"
scopes = [{scopes_toml}]
created_at = "{created_at}"
is_external = false
"#
    );
    let mut f = std::fs::File::create(dir.path().join("config.toml")).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    dir
}

fn post_mcp(body: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("Content-Type", "application/json");
    if let Some(t) = bearer {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// No Authorization header ⇒ 401, and (spec MUST) a `WWW-Authenticate` header
/// pointing at the RFC 9728 resource-metadata document so a remote client can
/// discover the OAuth flow instead of guessing.
#[tokio::test]
async fn mcp_streamable_rejects_missing_token_with_www_authenticate() {
    let dir = TempDir::new().unwrap();
    let app = build_router(&make_cfg(dir.path()), make_dispatcher(dir.path()));

    let resp = app
        .oneshot(post_mcp(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let www = resp
        .headers()
        .get("WWW-Authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        www.contains("Bearer") && www.contains("resource_metadata"),
        "401 must advertise the OAuth discovery document, got: {www:?}"
    );
}

/// A syntactically valid but unregistered key ⇒ 401, never a tool call.
#[tokio::test]
async fn mcp_streamable_rejects_wrong_token() {
    let key = "ddc_dev_00112233445566778899aabbccddeeff";
    let dir = make_home_with_key(key, "streamable-client", &["memory:read"]);
    let app = build_router(&make_cfg(dir.path()), make_dispatcher(dir.path()));

    let wrong = "ddc_dev_ffeeddccbbaa99887766554433221100";
    let resp = app
        .oneshot(post_mcp(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            Some(wrong),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Malformed JSON ⇒ 400 with JSON-RPC `-32700` (Parse error), not a 500 and
/// not a silent 200.
#[tokio::test]
async fn mcp_streamable_invalid_json_returns_parse_error() {
    let key = "ddc_dev_0a1b2c3d4e5f60718293a4b5c6d7e8f9";
    let dir = make_home_with_key(key, "streamable-client", &["memory:read"]);
    let app = build_router(&make_cfg(dir.path()), make_dispatcher(dir.path()));

    let resp = app
        .oneshot(post_mcp("{not json at all", Some(key)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let j = body_json(resp).await;
    assert_eq!(j["error"]["code"].as_i64(), Some(-32700), "body: {j}");
}

/// Session handling, asserted as it actually is: this endpoint is **stateless**
/// (spec-legal). It never issues an `Mcp-Session-Id`, `GET`/`DELETE` answer 405,
/// and a client that sends a session id anyway is still served — continuation
/// works because there is no session to lose, not because one is resumed.
#[tokio::test]
async fn mcp_streamable_is_stateless_across_requests() {
    let key = "ddc_dev_11223344556677889900aabbccddeeff";
    let dir = make_home_with_key(key, "streamable-client", &["memory:read"]);
    let cfg = make_cfg(dir.path());

    // First request: initialize. No session id is minted.
    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let resp = app
        .oneshot(post_mcp(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            Some(key),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("Mcp-Session-Id").is_none(),
        "a stateless server must not mint a session id"
    );
    let j = body_json(resp).await;
    assert_eq!(j["result"]["protocolVersion"].as_str(), Some("2025-06-18"));

    // Second request carrying a made-up session id: still served.
    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {key}"))
        .header("Mcp-Session-Id", "made-up-session")
        .body(Body::from(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let j = body_json(resp).await;
    assert_eq!(j["id"].as_i64(), Some(2), "body: {j}");

    // GET / DELETE are the spec's 405s (no server push, no sessions).
    for method in [Method::GET, Method::DELETE] {
        let app = build_router(&cfg, make_dispatcher(dir.path()));
        let req = Request::builder()
            .method(method.clone())
            .uri("/mcp")
            .header("Authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} /mcp must answer 405"
        );
    }
}

/// An unsupported `MCP-Protocol-Version` header is refused at the door (400),
/// rather than silently negotiated down.
#[tokio::test]
async fn mcp_streamable_rejects_unknown_protocol_version() {
    let key = "ddc_dev_22334455667788990011aabbccddeeff";
    let dir = make_home_with_key(key, "streamable-client", &["memory:read"]);
    let app = build_router(&make_cfg(dir.path()), make_dispatcher(dir.path()));

    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {key}"))
        .header("MCP-Protocol-Version", "1999-01-01")
        .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── OAuth 2.1 surface ────────────────────────────────────────────────────────

fn pkce_pair(verifier: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn post_form(uri: &str, form: &[(&str, &str)]) -> Request<Body> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

/// Full Dynamic Client Registration round trip: register → operator-approved
/// consent → PKCE token exchange → the minted token actually works on `/mcp`.
#[tokio::test]
async fn mcp_oauth_dcr_round_trip_mints_a_usable_token() {
    let operator_key = "ddc_dev_33445566778899001122aabbccddeeff";
    let dir = make_home_with_internal_key(operator_key, "operator", &["memory:read"]);
    let cfg = make_cfg(dir.path());
    let redirect_uri = "http://127.0.0.1:33418/callback";

    // 1. Register.
    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let req = Request::builder()
        .method(Method::POST)
        .uri("/oauth/register")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "redirect_uris": [redirect_uri],
                "client_name": "Integration Test Client",
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let reg = body_json(resp).await;
    let client_id = reg["client_id"].as_str().expect("client_id").to_string();
    assert!(client_id.starts_with("mcp_"), "reg: {reg}");

    // 2. Consent (operator proof = an internal key).
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = pkce_pair(verifier);
    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let resp = app
        .oneshot(post_form(
            "/oauth/decision",
            &[
                ("client_id", &client_id),
                ("redirect_uri", redirect_uri),
                ("state", "xyz"),
                ("code_challenge", &challenge),
                ("scope", "memory:read"),
                ("operator_key", operator_key),
                ("action", "approve"),
            ],
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "approval must redirect with a code, got {}",
        resp.status()
    );
    let location = resp
        .headers()
        .get("Location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let code = location
        .split(&['?', '&'][..])
        .find_map(|kv| kv.strip_prefix("code="))
        .map(|c| urlencoding::decode(c).unwrap().into_owned())
        .expect("authorization code in redirect");

    // 3. Token exchange with the matching PKCE verifier.
    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let resp = app
        .oneshot(post_form(
            "/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("client_id", &client_id),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tok = body_json(resp).await;
    let access = tok["access_token"].as_str().expect("access_token").to_string();
    assert!(access.starts_with("ddc_oauth_"), "token body: {tok}");

    // 4. The minted token authenticates on the Streamable-HTTP endpoint.
    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let resp = app
        .oneshot(post_mcp(
            r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#,
            Some(&access),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let j = body_json(resp).await;
    assert_eq!(j["id"].as_i64(), Some(9), "body: {j}");

    // 5. The code is single-use: replaying it fails.
    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let resp = app
        .oneshot(post_form(
            "/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("client_id", &client_id),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ],
        ))
        .await
        .unwrap();
    assert!(
        !resp.status().is_success(),
        "an authorization code must not be redeemable twice"
    );
}

/// DCR is fail-closed on the redirect URI: a non-loopback `http://` target is
/// refused at registration, so no consent screen can ever bounce a code there.
#[tokio::test]
async fn mcp_oauth_register_rejects_unacceptable_redirect_uri() {
    let dir = TempDir::new().unwrap();
    let app = build_router(&make_cfg(dir.path()), make_dispatcher(dir.path()));

    let req = Request::builder()
        .method(Method::POST)
        .uri("/oauth/register")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::json!({ "redirect_uris": ["http://localhost.evil.com/cb"] }).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let j = body_json(resp).await;
    assert_eq!(j["error"].as_str(), Some("invalid_redirect_uri"), "body: {j}");
}

/// Consent without a valid INTERNAL operator key never mints a code — an
/// external key cannot self-escalate into an OAuth grant.
#[tokio::test]
async fn mcp_oauth_decision_requires_internal_operator_key() {
    let external_key = "ddc_dev_44556677889900112233aabbccddeeff";
    let dir = make_home_with_key(external_key, "external-client", &["memory:read"]);
    let cfg = make_cfg(dir.path());
    let redirect_uri = "http://127.0.0.1:33418/callback";

    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let req = Request::builder()
        .method(Method::POST)
        .uri("/oauth/register")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::json!({ "redirect_uris": [redirect_uri] }).to_string(),
        ))
        .unwrap();
    let reg = body_json(app.oneshot(req).await.unwrap()).await;
    let client_id = reg["client_id"].as_str().unwrap().to_string();

    for key in [external_key, "ddc_dev_00000000000000000000000000000000"] {
        let app = build_router(&cfg, make_dispatcher(dir.path()));
        let resp = app
            .oneshot(post_form(
                "/oauth/decision",
                &[
                    ("client_id", &client_id),
                    ("redirect_uri", redirect_uri),
                    ("state", ""),
                    ("code_challenge", &pkce_pair("verifier-verifier-verifier-x")),
                    ("scope", "memory:read"),
                    ("operator_key", key),
                    ("action", "approve"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "key {key} must not be able to approve a grant"
        );
        assert!(
            resp.headers().get("Location").is_none(),
            "a rejected approval must never redirect a code anywhere"
        );
    }
}

/// The unauthenticated RFC 9728 / RFC 8414 discovery documents are served and
/// name the endpoints a remote client needs.
#[tokio::test]
async fn mcp_oauth_discovery_documents_are_public_and_complete() {
    let dir = TempDir::new().unwrap();
    let cfg = make_cfg(dir.path());

    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let req = Request::builder()
        .method(Method::GET)
        .uri("/.well-known/oauth-protected-resource")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let j = body_json(resp).await;
    assert!(j.get("resource").is_some(), "body: {j}");

    let app = build_router(&cfg, make_dispatcher(dir.path()));
    let req = Request::builder()
        .method(Method::GET)
        .uri("/.well-known/oauth-authorization-server")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let j = body_json(resp).await;
    for key in [
        "authorization_endpoint",
        "token_endpoint",
        "registration_endpoint",
    ] {
        assert!(j.get(key).is_some(), "{key} missing from metadata: {j}");
    }
}
