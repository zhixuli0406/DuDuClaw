//! MCP protocol eras, end to end: legacy `initialize` clients and modern
//! (2026-07-28, per-request `_meta`) clients against the Streamable HTTP
//! router and the real `duduclaw mcp-server` stdio binary.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use axum::body::Body;
use duduclaw_cli::mcp_dispatch::McpDispatcher;
use duduclaw_cli::mcp_http_server::{HttpServerConfig, build_router};
use duduclaw_cli::mcp_memory_quota::DailyQuota;
use duduclaw_cli::mcp_rate_limit::RateLimiter;
use duduclaw_cli::odoo_pool::OdooConnectorPool;
use duduclaw_memory::SqliteMemoryEngine;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const MODERN: &str = "2026-07-28";
const KEY: &str = "ddc_dev_5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "era-test", "version": "0"}
    })
}

// ── Streamable HTTP ──────────────────────────────────────────────────────────

fn http_home() -> TempDir {
    let dir = TempDir::new().unwrap();
    let created_at = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "[mcp_keys.\"{KEY}\"]\nclient_id = \"era-client\"\nscopes = [\"memory:read\", \"memory:write\"]\ncreated_at = \"{created_at}\"\nis_external = true\n"
        ),
    )
    .unwrap();
    dir
}

fn dispatcher(home: &Path) -> McpDispatcher {
    McpDispatcher::new(
        home.to_path_buf(),
        reqwest::Client::new(),
        Arc::new(SqliteMemoryEngine::new(&home.join("memory.db")).unwrap()),
        "test-agent".to_string(),
        Arc::new(OdooConnectorPool::default()),
        RateLimiter::new(),
        DailyQuota::new(),
    )
}

async fn post(
    home: &Path,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, http::HeaderMap, String) {
    let app = build_router(
        &HttpServerConfig::new("127.0.0.1:0".parse().unwrap(), home.to_path_buf()),
        dispatcher(home),
    );
    let mut b = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {KEY}"));
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let resp = app
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or(Value::Null)
}

#[tokio::test]
async fn http_modern_discover_list_and_call() {
    let home = http_home();
    let h = |m: &'static str| vec![("MCP-Protocol-Version", MODERN), ("Mcp-Method", m)];

    let (status, _, body) = post(
        home.path(),
        &h("server/discover"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": modern_meta()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let d = parse(&body);
    assert_eq!(d["result"]["resultType"], "complete");
    assert_eq!(d["result"]["supportedVersions"][0], MODERN);
    assert!(
        d["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2025-11-25"))
    );
    assert!(d["result"]["capabilities"]["tools"].is_object());
    assert_eq!(d["result"]["cacheScope"], "private");
    assert_eq!(
        d["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "duduclaw"
    );

    let (status, _, body) = post(
        home.path(),
        &h("tools/list"),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": modern_meta()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let l = parse(&body);
    assert_eq!(l["result"]["resultType"], "complete");
    assert!(l["result"]["ttlMs"].as_u64().is_some());
    assert!(!l["result"]["tools"].as_array().unwrap().is_empty());

    let mut call_headers = h("tools/call");
    call_headers.push(("Mcp-Name", "memory_search"));
    let (status, _, body) = post(
        home.path(),
        &call_headers,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"_meta": modern_meta(), "name": "memory_search", "arguments": {"query": "x"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let c = parse(&body);
    assert_eq!(c["result"]["resultType"], "complete", "{c}");
    assert!(
        c["result"].get("ttlMs").is_none(),
        "tools/call is not cacheable"
    );
}

#[tokio::test]
async fn http_modern_header_and_meta_validation() {
    let home = http_home();
    let list = |id: i64, meta: Value| json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {"_meta": meta}});

    // Missing Mcp-Method ⇒ 400 + HeaderMismatch.
    let (s, _, b) = post(
        home.path(),
        &[("MCP-Protocol-Version", MODERN)],
        list(1, modern_meta()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&b)["error"]["code"], -32020, "{b}");

    // Mcp-Method disagrees with the body.
    let (s, _, b) = post(
        home.path(),
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
        ],
        list(2, modern_meta()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&b)["error"]["code"], -32020, "{b}");

    // Body _meta modern but no version header.
    let (s, _, b) = post(
        home.path(),
        &[("Mcp-Method", "tools/list")],
        list(3, modern_meta()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&b)["error"]["code"], -32020, "{b}");

    // Missing clientCapabilities ⇒ -32602.
    let (s, _, b) = post(
        home.path(),
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/list"),
        ],
        list(
            4,
            json!({"io.modelcontextprotocol/protocolVersion": MODERN}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&b)["error"]["code"], -32602, "{b}");

    // Unsupported revision ⇒ -32022 listing what is supported.
    let (s, _, b) = post(
        home.path(),
        &[
            ("MCP-Protocol-Version", "1900-01-01"),
            ("Mcp-Method", "tools/list"),
        ],
        list(5, modern_meta()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let e = parse(&b);
    assert_eq!(e["error"]["code"], -32022, "{b}");
    assert_eq!(e["error"]["data"]["requested"], "1900-01-01");

    // Mcp-Name must match params.name (base64 form accepted).
    let call = json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {"_meta": modern_meta(), "name": "memory_search", "arguments": {"query": "x"}}});
    let (s, _, b) = post(
        home.path(),
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "memory_store"),
        ],
        call.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&b)["error"]["code"], -32020, "{b}");
    let (s, _, b) = post(
        home.path(),
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "=?base64?bWVtb3J5X3NlYXJjaA==?="),
        ],
        call,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");

    // Unknown method (ping was removed) ⇒ 404 + -32601.
    let (s, _, b) = post(
        home.path(),
        &[("MCP-Protocol-Version", MODERN), ("Mcp-Method", "ping")],
        json!({"jsonrpc": "2.0", "id": 7, "method": "ping", "params": {"_meta": modern_meta()}}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(parse(&b)["error"]["code"], -32601, "{b}");
}

#[tokio::test]
async fn http_modern_listen_is_acknowledged_then_completed() {
    let home = http_home();
    let (s, headers, body) = post(
        home.path(),
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "subscriptions/listen"),
        ],
        json!({"jsonrpc": "2.0", "id": "listen-1", "method": "subscriptions/listen",
               "params": {"_meta": modern_meta(), "notifications": {"toolsListChanged": true}}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/event-stream");
    let events: Vec<Value> = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(parse)
        .collect();
    assert_eq!(events.len(), 2, "{body}");
    assert_eq!(
        events[0]["method"],
        "notifications/subscriptions/acknowledged"
    );
    // This stateless endpoint cannot detect changes, so it honours nothing.
    assert_eq!(events[0]["params"]["notifications"], json!({}));
    assert_eq!(events[1]["id"], "listen-1");
    assert_eq!(events[1]["result"]["resultType"], "complete");
}

#[tokio::test]
async fn http_legacy_initialize_negotiates_up_to_2025_11_25() {
    let home = http_home();
    for (asked, expected) in [
        ("2025-11-25", "2025-11-25"),
        ("2024-11-05", "2024-11-05"),
        ("2099-01-01", "2025-11-25"),
    ] {
        let (s, _, b) = post(
            home.path(),
            &[],
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": asked}}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let r = parse(&b);
        assert_eq!(r["result"]["protocolVersion"], expected, "{b}");
        assert!(
            r["result"].get("resultType").is_none(),
            "legacy results stay legacy"
        );
    }
    // Legacy ping still works without any modern header.
    let (s, _, b) = post(
        home.path(),
        &[],
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(parse(&b)["result"].is_object(), "{b}");
}

// ── stdio (real binary) ──────────────────────────────────────────────────────

const CLI: &str = env!("CARGO_BIN_EXE_duduclaw");

fn base_command(home: &Path, data: &Path) -> Command {
    let mut cmd = Command::new(CLI);
    cmd.env_clear();
    for key in ["PATH", "TMPDIR", "SystemRoot"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("HOME", home).env("DUDUCLAW_HOME", data);
    cmd
}

fn init_token(home: &Path, data: &Path) -> String {
    let out = base_command(home, data)
        .args(["mcp", "init", "--client", "print"])
        .output()
        .expect("run mcp init");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let marker = "DUDUCLAW_MCP_API_KEY=";
    let start = stdout.find(marker).expect("key in output") + marker.len();
    stdout[start..]
        .split_whitespace()
        .next()
        .unwrap()
        .trim_matches('\'')
        .to_string()
}

struct Stdio_ {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
}

impl Stdio_ {
    fn start(home: &Path, data: &Path) -> Self {
        let token = init_token(home, data);
        let mut child = base_command(home, data)
            .env("DUDUCLAW_MCP_API_KEY", token)
            .arg("mcp-server")
            .current_dir(data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mcp-server");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        Stdio_ { child, stdin, rx }
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn next(&self) -> Value {
        self.rx
            .recv_timeout(Duration::from_secs(60))
            .expect("server message")
    }

    /// Whatever arrives within `ms`, if anything.
    fn maybe_next(&self, ms: u64) -> Option<Value> {
        self.rx.recv_timeout(Duration::from_millis(ms)).ok()
    }
}

impl Drop for Stdio_ {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_serves_modern_requests_without_initialize() {
    let tmp = TempDir::new().unwrap();
    let (home, data) = (tmp.path().join("home"), tmp.path().join("data"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let mut s = Stdio_::start(&home, &data);

    // The dual-era probe: server/discover with modern _meta.
    s.send(json!({"jsonrpc": "2.0", "id": "d", "method": "server/discover", "params": {"_meta": modern_meta()}}));
    let d = s.next();
    assert_eq!(d["id"], "d");
    assert_eq!(d["result"]["resultType"], "complete", "{d}");
    assert_eq!(d["result"]["capabilities"]["tools"]["listChanged"], true);
    assert_eq!(d["result"]["supportedVersions"][0], MODERN);

    // A notification is never answered, even an unknown one.
    s.send(json!({"jsonrpc": "2.0", "method": "notifications/whatever"}));
    s.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": modern_meta()}}));
    let l = s.next();
    assert_eq!(
        l["id"], 1,
        "the notification must not have produced a reply: {l}"
    );
    assert_eq!(l["result"]["resultType"], "complete");
    assert_eq!(l["result"]["cacheScope"], "private");
    assert!(l["result"]["ttlMs"].as_u64().is_some());

    s.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"_meta": modern_meta(), "name": "memory_store", "arguments": {"content": "era note"}}}));
    let c = s.next();
    assert_eq!(c["result"]["resultType"], "complete", "{c}");
    assert_ne!(c["result"]["isError"], true, "{c}");

    // Unsupported revision, missing capabilities, removed ping.
    s.send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "1900-01-01"}}}));
    assert_eq!(s.next()["error"]["code"], -32022);
    s.send(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": MODERN}}}));
    assert_eq!(s.next()["error"]["code"], -32602);
    s.send(
        json!({"jsonrpc": "2.0", "id": 5, "method": "ping", "params": {"_meta": modern_meta()}}),
    );
    assert_eq!(s.next()["error"]["code"], -32601);

    // A subscription is acknowledged first and gets no response until it ends;
    // cancelling it produces nothing.
    s.send(
        json!({"jsonrpc": "2.0", "id": "sub", "method": "subscriptions/listen",
        "params": {"_meta": modern_meta(), "notifications": {"toolsListChanged": true}}}),
    );
    let ack = s.next();
    assert_eq!(
        ack["method"], "notifications/subscriptions/acknowledged",
        "{ack}"
    );
    assert_eq!(
        ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "sub"
    );
    assert_eq!(ack["params"]["notifications"]["toolsListChanged"], true);
    s.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "sub"}}));
    assert!(
        s.maybe_next(500).is_none(),
        "nothing may follow a cancelled subscription"
    );
}

#[test]
fn stdio_legacy_initialize_negotiates_and_keeps_ping() {
    let tmp = TempDir::new().unwrap();
    let (home, data) = (tmp.path().join("home"), tmp.path().join("data"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let mut s = Stdio_::start(&home, &data);

    s.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}));
    let init = s.next();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);
    assert!(init["result"].get("resultType").is_none());
    s.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    s.send(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}));
    let pong = s.next();
    assert_eq!(pong["id"], 2, "{pong}");
    assert!(pong["result"].is_object(), "{pong}");

    // An old client still gets the revision it asked for.
    s.send(json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}));
    assert_eq!(s.next()["result"]["protocolVersion"], "2024-11-05");
}

fn write_key(data: &Path, scopes: &str) {
    let created_at = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        data.join("config.toml"),
        format!(
            "[mcp_keys.\"{KEY}\"]\nclient_id = \"era-client\"\nscopes = [{scopes}]\ncreated_at = \"{created_at}\"\nis_external = true\n"
        ),
    )
    .unwrap();
}

/// A modern subscriber learns about a changed tool set only through its
/// subscription: one tagged `list_changed`, and never the untagged legacy one.
#[test]
fn stdio_modern_subscription_receives_tagged_list_changed() {
    let tmp = TempDir::new().unwrap();
    let (home, data) = (tmp.path().join("home"), tmp.path().join("data"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    write_key(&data, "\"memory:read\", \"memory:write\"");
    let mut child = base_command(&home, &data)
        .env("DUDUCLAW_MCP_API_KEY", KEY)
        .arg("mcp-server")
        .current_dir(&data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp-server");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    let mut s = Stdio_ { child, stdin, rx };

    s.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": modern_meta()}}));
    let before = s.next();
    let names = |r: &Value| -> Vec<String> {
        r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(
        names(&before).contains(&"memory_store".to_string()),
        "{before}"
    );

    s.send(
        json!({"jsonrpc": "2.0", "id": 7, "method": "subscriptions/listen",
        "params": {"_meta": modern_meta(), "notifications": {"toolsListChanged": true}}}),
    );
    assert_eq!(
        s.next()["method"],
        "notifications/subscriptions/acknowledged"
    );

    // Narrow the key: memory_store leaves the listing.
    std::thread::sleep(Duration::from_millis(1100)); // distinct mtime
    write_key(&data, "\"memory:read\"");
    let changed =
        s.rx.recv_timeout(Duration::from_secs(20))
            .expect("a list_changed within 20s");
    assert_eq!(
        changed["method"], "notifications/tools/list_changed",
        "{changed}"
    );
    assert_eq!(
        changed["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        7
    );
    assert!(
        s.maybe_next(1500).is_none(),
        "no untagged legacy notification may follow"
    );

    s.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": modern_meta()}}));
    assert!(!names(&s.next()).contains(&"memory_store".to_string()));
}

/// A subscription asking for nothing this server offers is acknowledged and
/// completed at once; open subscriptions are capped and deduplicated.
#[test]
fn stdio_subscriptions_complete_when_empty_and_are_capped() {
    let tmp = TempDir::new().unwrap();
    let (home, data) = (tmp.path().join("home"), tmp.path().join("data"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let mut s = Stdio_::start(&home, &data);

    s.send(
        json!({"jsonrpc": "2.0", "id": "empty", "method": "subscriptions/listen",
        "params": {"_meta": modern_meta(), "notifications": {"promptsListChanged": true}}}),
    );
    let ack = s.next();
    assert_eq!(ack["params"]["notifications"], json!({}), "{ack}");
    let done = s.next();
    assert_eq!(done["id"], "empty", "{done}");
    assert_eq!(done["result"]["resultType"], "complete");

    let listen = |id: i64| {
        json!({"jsonrpc": "2.0", "id": id, "method": "subscriptions/listen",
            "params": {"_meta": modern_meta(), "notifications": {"toolsListChanged": true}}})
    };
    for id in 0..16 {
        s.send(listen(id));
        assert_eq!(
            s.next()["method"],
            "notifications/subscriptions/acknowledged"
        );
    }
    // Re-sending an open id is not a new subscription.
    s.send(listen(3));
    assert_eq!(
        s.next()["method"],
        "notifications/subscriptions/acknowledged"
    );
    s.send(listen(16));
    let refused = s.next();
    assert_eq!(refused["id"], 16, "{refused}");
    assert_eq!(refused["error"]["code"], -32000);
}

/// A subscription the server cannot authenticate is refused, not acknowledged.
#[test]
fn stdio_listen_refuses_when_authentication_fails() {
    let tmp = TempDir::new().unwrap();
    let (home, data) = (tmp.path().join("home"), tmp.path().join("data"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    write_key(&data, "\"memory:read\"");
    let mut child = base_command(&home, &data)
        .env("DUDUCLAW_MCP_API_KEY", KEY)
        .arg("mcp-server")
        .current_dir(&data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp-server");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    let mut s = Stdio_ { child, stdin, rx };
    // Wait until the server has booted (boot authenticates too), then revoke.
    s.send(
        json!({"jsonrpc": "2.0", "id": "boot", "method": "server/discover",
        "params": {"_meta": modern_meta()}}),
    );
    assert_eq!(s.next()["id"], "boot");
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(data.join("config.toml"), "").unwrap();
    s.send(
        json!({"jsonrpc": "2.0", "id": "sub", "method": "subscriptions/listen",
        "params": {"_meta": modern_meta(), "notifications": {"toolsListChanged": true}}}),
    );
    let r = s.next();
    assert_eq!(r["id"], "sub", "{r}");
    assert_eq!(r["error"]["code"], -32000, "{r}");
    assert_eq!(r["error"]["data"]["duduclawCode"], -32003);
}
