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
    let content = format!(
        r#"
[mcp_keys."{key}"]
client_id = "{client_id}"
scopes = [{scopes_toml}]
created_at = "2026-05-01T00:00:00Z"
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
        Arc::new(tokio::sync::RwLock::new(None)),
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
