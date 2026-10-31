//! Exercise the production CLI entry point: trace must never persist session JWTs.
use axum::{
    Json, Router,
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use duduclaw_gateway::protocol::WsFrame;
use serde_json::{Value, json};
use std::{path::Path, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, SystemTime, UNIX_EPOCH}};

struct Fixture {
    jwt: String,
    authenticated: AtomicBool,
}
fn user() -> Value {
    json!({"id":"canary-user","email":"canary@example.invalid","role":"employee"})
}
async fn local(State(f): State<Arc<Fixture>>) -> Json<Value> {
    Json(json!({"access_token":f.jwt,"refresh_token":f.jwt,"user":user()}))
}
async fn upgrade(ws: WebSocketUpgrade, State(f): State<Arc<Fixture>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| serve(socket, f))
}
async fn serve(mut socket: WebSocket, f: Arc<Fixture>) {
    while let Some(Ok(message)) = socket.recv().await {
        let Message::Text(text) = message else { continue; };
        let WsFrame::Request { id, method, params } = serde_json::from_str(&text).unwrap() else {
            panic!("expected RPC request");
        };
        let payload = match method.as_str() {
            "connect" => {
                assert!(params["jwt"].as_str() == Some(f.jwt.as_str()), "canary session was not used");
                f.authenticated.store(true, Ordering::SeqCst);
                json!({"status":"authenticated","user":user(),"must_change_password":false})
            }
            "system.version" => json!({"version":"canary-fixture"}),
            "agents.list" => json!({"agents":[]}),
            "approvals.list" => json!({"approvals":[],"count":0}),
            _ => panic!("unexpected method: {method}"),
        };
        let response = WsFrame::ok_response(&id, payload);
        if socket.send(Message::Text(serde_json::to_string(&response).unwrap().into())).await.is_err() {
            break;
        }
    }
}
fn assert_no_canary(bytes: &[u8], jwt: &str, location: &str) {
    assert!(!bytes.windows(jwt.len()).any(|w| w == jwt.as_bytes()), "JWT leaked into {location}");
}
fn inspect_logs(path: &Path, jwt: &str) {
    if !path.exists() { return; }
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            inspect_logs(&entry.path(), jwt);
        } else {
            assert_no_canary(&std::fs::read(entry.path()).unwrap(), jwt, "home log file");
        }
    }
}

#[tokio::test]
async fn tui_inbox_real_binary_does_not_log_jwt_at_trace() {
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("NOT RUN: CLI canary socket bind failed: {error}");
            assert!(std::env::var("DUDUCLAW_TUI_ALLOW_NO_BIND").as_deref() == Ok("1"),
                "NOT RUN: bind refused; set DUDUCLAW_TUI_ALLOW_NO_BIND=1 only to acknowledge this unexecuted socket test");
            return;
        }
    };
    let port = listener.local_addr().unwrap().port();
    let home = tempfile::tempdir().unwrap();
    let exp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 3600;
    let jwt = format!("e30.{}.TUI_CANARY_SIGNATURE_20261030", URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({"sub":"canary-user","exp":exp})).unwrap()));
    let fixture = Arc::new(Fixture { jwt: jwt.clone(), authenticated: AtomicBool::new(false) });
    let app = Router::new().route("/api/session/local", post(local)).route("/ws", get(upgrade)).with_state(fixture.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_duduclaw"));
    // Start clean so employee-session markers and ambient credentials cannot alter this path.
    command.env_clear();
    for name in ["PATH", "HOME", "USERPROFILE", "SYSTEMROOT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) { command.env(name, value); }
    }
    command.args(["tui", "inbox", "--json"])
        .env("DUDUCLAW_HOME", home.path())
        .env("DUDUCLAW_PORT", port.to_string())
        .env("RUST_LOG", "trace")
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(30), command.output()).await;
    server.abort();
    let output = result.expect("CLI subprocess deadline").expect("run real duduclaw binary");
    assert_no_canary(&output.stdout, &jwt, "stdout");
    assert_no_canary(&output.stderr, &jwt, "stderr");
    inspect_logs(&home.path().join("logs"), &jwt);
    assert!(output.status.success(), "CLI failed: {}", output.status);
    assert!(fixture.authenticated.load(Ordering::SeqCst), "CLI did not connect with canary");
    let snapshot: Value = serde_json::from_slice(&output.stdout).expect("one JSON snapshot");
    assert_eq!(snapshot["type"], "inbox");
    assert_eq!(snapshot["stale"], false);
    assert_eq!(snapshot["count"], 0);
}
