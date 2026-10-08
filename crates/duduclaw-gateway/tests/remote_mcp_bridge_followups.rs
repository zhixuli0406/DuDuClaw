//! Bridge follow-ups (2026-10-08) against a loopback fake Streamable HTTP
//! server: operator-supplied headers on every request, the opt-in
//! server-initiated GET stream with `Last-Event-ID` resumption, and the
//! third-party tool gate (`action_rules`) filtering `tools/list` and
//! refusing a blocked call before it leaves.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::extract::State;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use duduclaw_gateway::remote_mcp::{bridge, connect, store};

#[derive(Default)]
struct Fake {
    workspace_headers: Vec<String>,
    gets: Vec<Option<String>>,
    calls: Vec<String>,
}
type S = Arc<Mutex<Fake>>;

fn ws(h: &HeaderMap) -> String {
    h.get("x-workspace").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

async fn mcp_post(State(s): State<S>, headers: HeaderMap, body: String) -> Response {
    s.lock().unwrap().workspace_headers.push(ws(&headers));
    if ws(&headers) != "w-1" {
        return (StatusCode::FORBIDDEN, "missing workspace header").into_response();
    }
    let frame: Value = serde_json::from_str(&body).unwrap();
    let method = frame["method"].as_str().unwrap_or("").to_string();
    if frame.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    s.lock().unwrap().calls.push(method.clone());
    let result = match method.as_str() {
        "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"f","version":"1"}}),
        "tools/list" => json!({"tools":[
            {"name":"lookup","annotations":{"readOnlyHint":true}},
            {"name":"wipe","annotations":{"destructiveHint":true}}
        ]}),
        "ping" => {
            // Give the GET stream time to deliver and resume before the
            // client's stdin ends.
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            json!({})
        }
        _ => json!({"content":[{"type":"text","text":"ran"}]}),
    };
    (
        [("content-type", "application/json"), ("mcp-session-id", "sess-9")],
        json!({"jsonrpc":"2.0","id":frame["id"],"result":result}).to_string(),
    )
        .into_response()
}

async fn mcp_get(State(s): State<S>, headers: HeaderMap) -> Response {
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok()).map(str::to_string);
    let n = {
        let mut f = s.lock().unwrap();
        f.gets.push(last.clone());
        f.gets.len()
    };
    if ws(&headers) != "w-1" || headers.get("mcp-session-id").is_none() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if n == 1 {
        let note = json!({"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":"x://1"}});
        (
            [("content-type", "text/event-stream")],
            format!("id: ev-1\ndata: {note}\n\n"),
        )
            .into_response()
    } else {
        // Resumed with Last-Event-ID; then no more stream.
        StatusCode::METHOD_NOT_ALLOWED.into_response()
    }
}

async fn start() -> (S, String) {
    let s: S = Arc::new(Mutex::new(Fake::default()));
    let app = Router::new()
        .route("/mcp", post(mcp_post).get(mcp_get).delete(|| async { StatusCode::NO_CONTENT }))
        .with_state(s.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (s, base)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headers_server_stream_and_tool_gate() {
    let (fake, base) = start().await;
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let agent = home.join("agents").join("a1");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("agent.toml"),
        "[capabilities]\naction_rules = [{ tool = \"crm.wipe\", verdict = \"block\" }]\n",
    )
    .unwrap();

    let req = |headers: Option<Vec<(String, String)>>| connect::ConnectRequest {
        agent_id: "a1".into(),
        server: "crm".into(),
        url: Some(format!("{base}/mcp")),
        auth: store::AuthKind::None,
        bearer: None,
        redirect_origin: None,
        client_id: None,
        client_secret: None,
        allowed_origins: vec![],
        headers,
        server_stream: Some(true),
    };
    // Reserved and CR/LF headers are refused before anything is sent.
    assert!(connect::start_connect(home, req(Some(vec![("Authorization".into(), "x".into())]))).await.is_err());
    assert!(connect::start_connect(home, req(Some(vec![("X-A".into(), "a\r\nb".into())]))).await.is_err());
    // Without the header the probe is refused by the server.
    assert!(connect::start_connect(home, req(None)).await.is_err());
    connect::start_connect(home, req(Some(vec![("X-Workspace".into(), "w-1".into())])))
        .await
        .expect("connect with the header");
    let raw = std::fs::read_to_string(store::store_path(home)).unwrap();
    assert!(!raw.contains("w-1"), "header values are stored encrypted");
    assert!(raw.contains("X-Workspace"), "header names are listed in plaintext");

    let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
                 {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"wipe\",\"arguments\":{}}}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"lookup\",\"arguments\":{}}}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"ping\"}\n";
    let (client, server) = tokio::io::duplex(1 << 20);
    let code = bridge::run_bridge(home, "a1", "crm", input.as_bytes(), server).await;
    assert_eq!(code, 0);
    let mut out = String::new();
    tokio::io::BufReader::new(client).read_to_string(&mut out).await.unwrap();
    let msgs: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by_id = |id: i64| msgs.iter().find(|m| m["id"] == id).cloned().unwrap_or(Value::Null);

    // Gate: `wipe` is hidden and refused without reaching the server.
    let listing = by_id(2);
    let names: Vec<&str> = listing["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["lookup"], "{out}");
    assert_eq!(by_id(3)["error"]["code"], -32003, "{out}");
    assert_eq!(by_id(4)["result"]["content"][0]["text"], "ran");
    let f = fake.lock().unwrap();
    assert_eq!(f.calls.iter().filter(|m| *m == "tools/call").count(), 1, "{:?}", f.calls);
    // Headers on every request.
    assert!(f.workspace_headers.iter().skip(1).all(|h| h == "w-1"), "{:?}", f.workspace_headers);
    // Server stream: delivered, then resumed with Last-Event-ID, then 405 stops it.
    assert!(msgs.iter().any(|m| m["method"] == "notifications/resources/updated"), "{out}");
    assert_eq!(f.gets.first().cloned().flatten(), None);
    assert_eq!(f.gets.get(1).cloned().flatten().as_deref(), Some("ev-1"), "{:?}", f.gets);
    assert_eq!(f.gets.len(), 2, "405 stops reconnecting: {:?}", f.gets);
    // The snapshot the dashboard reads was recorded.
    let views = duduclaw_gateway::third_party_tools::load_snapshots(home, "a1");
    assert_eq!(views[0].server, "crm");
    assert_eq!(views[0].tools.len(), 2);
}
