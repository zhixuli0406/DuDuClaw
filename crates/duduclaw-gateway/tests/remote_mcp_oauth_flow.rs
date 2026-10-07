//! End to end: native remote MCP with OAuth against a fake authorization
//! server and a fake Streamable HTTP MCP server, both on loopback.
//!
//! connect (discovery via `WWW-Authenticate` → protected resource metadata →
//! path-inserted AS metadata, dynamic registration, PKCE) → browser redirect
//! → callback → bridge `initialize` / `tools/list` (SSE answer) with a
//! proactive refresh of a nearly expired token → server revokes the token →
//! bridge gets 401, refreshes once (rotation) and retries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use serde_json::{Value, json};
use sha2::Digest;
use tokio::io::AsyncReadExt;

use duduclaw_gateway::remote_mcp::{bridge, connect, store};

#[derive(Default)]
struct Fake {
    base: String,
    registered_names: Vec<String>,
    challenge: Option<String>,
    redirect_uri: Option<String>,
    resources_seen: Vec<String>,
    valid_access: Vec<String>,
    valid_refresh: Option<String>,
    issued: u32,
    grants: Vec<String>,
    mcp_calls: Vec<(String, String)>,
}

type S = Arc<Mutex<Fake>>;

fn bearer(h: &HeaderMap) -> String {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string()
}

async fn mcp(State(s): State<S>, headers: HeaderMap, body: String) -> Response {
    let token = bearer(&headers);
    let (valid, base) = {
        let f = s.lock().unwrap();
        (f.valid_access.contains(&token), f.base.clone())
    };
    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                format!(r#"Bearer resource_metadata="{base}/.well-known/oauth-protected-resource/mcp""#),
            )],
        )
            .into_response();
    }
    let frame: Value = serde_json::from_str(&body).unwrap();
    let method = frame["method"].as_str().unwrap_or("").to_string();
    s.lock().unwrap().mcp_calls.push((method.clone(), token));
    if method == "initialize" {
        return (
            [("mcp-session-id", "sess-1"), ("content-type", "application/json")],
            json!({"jsonrpc":"2.0","id":frame["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}).to_string(),
        )
            .into_response();
    }
    // Everything after initialize must carry the session and version.
    let sid = headers.get("mcp-session-id").and_then(|v| v.to_str().ok());
    let ver = headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok());
    if sid != Some("sess-1") || ver != Some("2025-06-18") {
        return (StatusCode::BAD_REQUEST, "missing session or protocol version").into_response();
    }
    if frame.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    match method.as_str() {
        "tools/list" => {
            let note = json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"listing"}});
            let resp = json!({"jsonrpc":"2.0","id":frame["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}});
            (
                [("content-type", "text/event-stream")],
                format!("event: message\ndata: {note}\n\nevent: message\ndata: {resp}\n\n"),
            )
                .into_response()
        }
        _ => (
            [("content-type", "application/json")],
            json!({"jsonrpc":"2.0","id":frame["id"],"result":{"content":[{"type":"text","text":"ok"}]}}).to_string(),
        )
            .into_response(),
    }
}

async fn prm(State(s): State<S>) -> Response {
    let base = s.lock().unwrap().base.clone();
    axum::Json(json!({"resource": format!("{base}/mcp"), "authorization_servers": [format!("{base}/as")]})).into_response()
}

async fn as_meta(State(s): State<S>) -> Response {
    let base = s.lock().unwrap().base.clone();
    axum::Json(json!({
        "issuer": format!("{base}/as"),
        "authorization_endpoint": format!("{base}/as/authorize"),
        "token_endpoint": format!("{base}/as/token"),
        "registration_endpoint": format!("{base}/as/register"),
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
    .into_response()
}

async fn register(State(s): State<S>, axum::Json(body): axum::Json<Value>) -> Response {
    assert_eq!(body["token_endpoint_auth_method"], "none");
    s.lock().unwrap().registered_names.push(body["client_name"].as_str().unwrap_or("").to_string());
    (StatusCode::CREATED, axum::Json(json!({"client_id": "cid-1"}))).into_response()
}

async fn authorize(State(s): State<S>, Query(q): Query<HashMap<String, String>>) -> Response {
    assert_eq!(q["client_id"], "cid-1");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["code_challenge_method"], "S256");
    let mut f = s.lock().unwrap();
    f.challenge = Some(q["code_challenge"].clone());
    f.redirect_uri = Some(q["redirect_uri"].clone());
    f.resources_seen.push(q["resource"].clone());
    let loc = format!("{}?code=code-1&state={}", q["redirect_uri"], q["state"]);
    (StatusCode::FOUND, [(header::LOCATION, loc)]).into_response()
}

fn issue(f: &mut Fake) -> Value {
    f.issued += 1;
    let at = format!("at-{}", f.issued);
    let rt = format!("rt-{}", f.issued);
    f.valid_access = vec![at.clone()];
    f.valid_refresh = Some(rt.clone());
    // 30 s: inside the bridge's 60 s refresh margin, so the first use refreshes.
    let expires = if f.issued == 1 { 30 } else { 3600 };
    json!({"access_token": at, "token_type": "Bearer", "expires_in": expires, "refresh_token": rt})
}

async fn token(State(s): State<S>, Form(form): Form<HashMap<String, String>>) -> Response {
    let mut f = s.lock().unwrap();
    f.grants.push(form.get("grant_type").cloned().unwrap_or_default());
    f.resources_seen.push(form.get("resource").cloned().unwrap_or_default());
    assert_eq!(form.get("client_id").map(String::as_str), Some("cid-1"));
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            assert_eq!(form["code"], "code-1");
            assert_eq!(Some(&form["redirect_uri"]), f.redirect_uri.as_ref());
            let verifier = &form["code_verifier"];
            let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(verifier.as_bytes()));
            if Some(&challenge) != f.challenge.as_ref() {
                return (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant", "error_description": "pkce"}))).into_response();
            }
            axum::Json(issue(&mut f)).into_response()
        }
        Some("refresh_token") => {
            if form.get("refresh_token") != f.valid_refresh.as_ref() {
                return (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant"}))).into_response();
            }
            axum::Json(issue(&mut f)).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "unsupported_grant_type"}))).into_response(),
    }
}

async fn start_fake() -> (S, String) {
    let state: S = Arc::new(Mutex::new(Fake::default()));
    let app = Router::new()
        .route("/mcp", post(mcp).delete(|| async { StatusCode::NO_CONTENT }))
        .route("/.well-known/oauth-protected-resource/mcp", get(prm))
        .route("/.well-known/oauth-authorization-server/as", get(as_meta))
        .route("/as/register", post(register))
        .route("/as/authorize", get(authorize))
        .route("/as/token", post(token))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    state.lock().unwrap().base = base.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, base)
}

async fn run(home: &std::path::Path, input: &str) -> Vec<Value> {
    let (client, server) = tokio::io::duplex(1 << 20);
    let code = bridge::run_bridge(home, "a1", "fake", input.as_bytes(), server).await;
    assert_eq!(code, 0);
    let mut out = String::new();
    tokio::io::BufReader::new(client).read_to_string(&mut out).await.unwrap();
    out.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oauth_connect_callback_bridge_and_refresh() {
    let (fake, base) = start_fake().await;
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    std::fs::create_dir_all(home.join("agents").join("a1")).unwrap();

    // 1. Connect: discovery + registration + authorize URL.
    let outcome = connect::start_connect(
        home,
        connect::ConnectRequest {
            agent_id: "a1".into(),
            server: "fake".into(),
            url: Some(format!("{base}/mcp")),
            auth: store::AuthKind::Oauth,
            bearer: None,
            redirect_origin: Some("http://localhost:18789".into()),
            client_id: None,
            client_secret: None,
            allowed_origins: vec![],
        },
    )
    .await
    .expect("start_connect");
    let connect::ConnectOutcome::Authorize { authorize_url, .. } = outcome else {
        panic!("expected an authorize URL")
    };
    assert!(authorize_url.starts_with(&format!("{base}/as/authorize?")));
    assert_eq!(fake.lock().unwrap().registered_names, vec!["DuDuClaw".to_string()]);

    // 2. The browser: the fake AS auto-approves and redirects to the gateway.
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let resp = http.get(&authorize_url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 302);
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    assert!(location.starts_with("http://localhost:18789/oauth/mcp/callback?"), "{location}");
    let loc = url::Url::parse(&location).unwrap();
    let q: HashMap<String, String> = loc.query_pairs().into_owned().collect();

    // 3. Callback: state is single use.
    let done = connect::complete_callback(home, &q["state"], &q["code"]).await.expect("callback");
    assert_eq!((done.agent_id.as_str(), done.server.as_str()), ("a1", "fake"));
    assert_eq!(done.redirect_origin, "http://localhost:18789");
    assert!(connect::complete_callback(home, &q["state"], &q["code"]).await.is_err());
    {
        let f = fake.lock().unwrap();
        assert!(f.resources_seen.iter().all(|r| r == &format!("{base}/mcp")), "{:?}", f.resources_seen);
        assert_eq!(f.grants, vec!["authorization_code"]);
    }
    let rec = store::get(home, "a1", "fake").unwrap().unwrap();
    assert_eq!(rec.status, store::ConnStatus::Connected);
    assert!(rec.has_refresh_token);
    let raw = std::fs::read_to_string(store::store_path(home)).unwrap();
    assert!(!raw.contains("at-1") && !raw.contains("rt-1"), "tokens must not be stored in plaintext");

    // 4. Bridge: the 30 s token is refreshed before use; tools/list comes
    //    back over SSE with a notification first.
    let out = run(
        home,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"t\",\"version\":\"1\"}}}\n\
         {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
         {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
    )
    .await;
    let by_id = |id: i64| out.iter().find(|v| v["id"] == id).cloned().unwrap_or(Value::Null);
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18", "{out:?}");
    assert_eq!(by_id(2)["result"]["tools"][0]["name"], "echo", "{out:?}");
    assert!(out.iter().any(|v| v["method"] == "notifications/message"));
    {
        let f = fake.lock().unwrap();
        assert_eq!(f.grants, vec!["authorization_code", "refresh_token"]);
        assert!(f.mcp_calls.iter().all(|(_, t)| t == "at-2"), "{:?}", f.mcp_calls);
        assert!(f.mcp_calls.iter().any(|(m, _)| m == "notifications/initialized"));
    }

    // 5. The server revokes the token: 401 → one refresh (rotation) → retry.
    fake.lock().unwrap().valid_access.clear();
    let out = run(
        home,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
         {\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"echo\",\"arguments\":{}}}\n",
    )
    .await;
    assert_eq!(out.iter().find(|v| v["id"] == 3).unwrap()["result"]["content"][0]["text"], "ok", "{out:?}");
    {
        let f = fake.lock().unwrap();
        assert_eq!(f.grants, vec!["authorization_code", "refresh_token", "refresh_token"]);
        assert_eq!(f.valid_refresh.as_deref(), Some("rt-3"));
    }
    let rec = store::get(home, "a1", "fake").unwrap().unwrap();
    let secrets = store::open(home, &rec).unwrap();
    assert_eq!(secrets.oauth.unwrap().refresh_token.as_deref(), Some("rt-3"), "rotated refresh token stored");

    // 6. A refresh token the server no longer accepts marks the record
    //    needs_reauth and the bridge answers with an operator-facing error.
    {
        let mut f = fake.lock().unwrap();
        f.valid_access.clear();
        f.valid_refresh = Some("something-else".into());
    }
    let out = run(home, "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"initialize\",\"params\":{}}\n").await;
    assert!(out[0]["error"]["message"].as_str().unwrap().contains("dashboard"), "{out:?}");
    assert_eq!(store::get(home, "a1", "fake").unwrap().unwrap().status, store::ConnStatus::NeedsReauth);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bearer_connect_probes_and_a_refused_token_is_reported() {
    let (fake, base) = start_fake().await;
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    fake.lock().unwrap().valid_access = vec!["static-1".into()];
    let req = |b: &str| connect::ConnectRequest {
        agent_id: "a1".into(),
        server: "fake".into(),
        url: Some(format!("{base}/mcp")),
        auth: store::AuthKind::Bearer,
        bearer: Some(b.into()),
        redirect_origin: None,
        client_id: None,
        client_secret: None,
        allowed_origins: vec![],
    };
    assert!(connect::start_connect(home, req("wrong")).await.is_err());
    assert!(matches!(connect::start_connect(home, req("Bearer static-1")).await, Ok(connect::ConnectOutcome::Connected)));
    let raw = std::fs::read_to_string(store::store_path(home)).unwrap();
    assert!(!raw.contains("static-1"));
    let out = run(home, "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n").await;
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18", "{out:?}");
}

/// A dashboard opened on a LAN address over plain http cannot receive the
/// OAuth redirect. The provider is sent to the browser machine's loopback
/// address on the dashboard port instead, and the operator pastes the
/// address-bar URL back (`mcp.remote_complete`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lan_http_dashboard_signs_in_through_a_pasted_loopback_callback() {
    let (fake, base) = start_fake().await;
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    std::fs::create_dir_all(home.join("agents").join("a1")).unwrap();

    let outcome = connect::start_connect(
        home,
        connect::ConnectRequest {
            agent_id: "a1".into(),
            server: "fake".into(),
            url: Some(format!("{base}/mcp")),
            auth: store::AuthKind::Oauth,
            bearer: None,
            redirect_origin: Some("http://192.168.1.20:18789".into()),
            client_id: None,
            client_secret: None,
            allowed_origins: vec![],
        },
    )
    .await
    .expect("start_connect from a LAN http dashboard");
    let connect::ConnectOutcome::Authorize { authorize_url, completion, redirect_uri } = outcome else {
        panic!("expected an authorize URL")
    };
    assert_eq!(completion, connect::Completion::Paste);
    assert_eq!(redirect_uri, "http://127.0.0.1:18789/oauth/mcp/callback");

    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let resp = http.get(&authorize_url).send().await.unwrap();
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    assert!(location.starts_with("http://127.0.0.1:18789/oauth/mcp/callback?"), "{location}");

    // What the operator copies from the address bar of the failed page.
    let done = connect::complete_pasted(home, &location).await.expect("pasted callback");
    assert_eq!((done.agent_id.as_str(), done.server.as_str()), ("a1", "fake"));
    assert!(connect::complete_pasted(home, &location).await.is_err(), "single use");
    assert_eq!(fake.lock().unwrap().grants, vec!["authorization_code"]);
    let rec = store::get(home, "a1", "fake").unwrap().unwrap();
    assert_eq!(rec.status, store::ConnStatus::Connected);
}
