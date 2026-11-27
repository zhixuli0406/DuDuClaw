use super::*;
use std::sync::{Arc, Mutex, atomic::{AtomicU16, AtomicU64, Ordering}};
use axum::{Json, Router, extract::State, http::{HeaderMap, StatusCode}, routing::post};

pub(crate) const RAW: &str = "ddc_member_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub(crate) struct Stub {
    pub(crate) status: Arc<AtomicU16>,
    pub(crate) scopes: Arc<Mutex<Vec<String>>>,
    pub(crate) seen: Arc<Mutex<Vec<(String, Value)>>>,
    pub(crate) delay_ms: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct StubState {
    status: Arc<AtomicU16>, scopes: Arc<Mutex<Vec<String>>>, seen: Arc<Mutex<Vec<(String, Value)>>>,
    delay_ms: Arc<AtomicU64>,
}

impl Stub {
    pub(crate) async fn start(home: &Path) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(home.join("config.toml"), format!("[gateway]\nport={port}\n")).unwrap();
        let status = Arc::new(AtomicU16::new(200));
        let scopes = Arc::new(Mutex::new(vec!["tasks:read".into(), "agents:status".into()]));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let delay_ms = Arc::new(AtomicU64::new(0));
        let state = StubState { status: status.clone(), scopes: scopes.clone(), seen: seen.clone(), delay_ms: delay_ms.clone() };
        let app = Router::new().route("/api/internal/person-key", post(reply)).with_state(state);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        Self { status, scopes, seen, delay_ms, task }
    }
}
impl Drop for Stub { fn drop(&mut self) { self.task.abort(); } }

async fn reply(State(state): State<StubState>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let bearer = headers.get("authorization").unwrap().to_str().unwrap().to_owned();
    state.seen.lock().unwrap().push((bearer.clone(), body.clone()));
    assert_eq!(bearer, format!("Bearer {RAW}"));
    tokio::time::sleep(Duration::from_millis(state.delay_ms.load(Ordering::SeqCst))).await;
    let status = StatusCode::from_u16(state.status.load(Ordering::SeqCst)).unwrap();
    if status != StatusCode::OK {
        return (status, Json(json!({"ok":false, "code":"refused", "message":"scope removed"})));
    }
    if body["op"] == "authenticate" {
        (status, Json(json!({"ok":true,"key_id":"key-1","user_id":"person-1","scopes":*state.scopes.lock().unwrap()})))
    } else {
        (status, Json(json!({"ok":true,"result":{"tasks":[{"id":"visible"}]}})))
    }
}

#[tokio::test]
async fn unreachable_gateway_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::fs::write(home.path().join("config.toml"), format!("[gateway]\nport={port}\n")).unwrap();
    drop(listener);
    assert!(matches!(authenticate(home.path(), RAW).await, Err(AuthError::UnknownKey)));
}

#[tokio::test]
async fn raw_bearer_call_body_and_live_refusal() {
    let home = tempfile::tempdir().unwrap();
    let stub = Stub::start(home.path()).await;
    let call = authenticate(home.path(), RAW).await.unwrap();
    assert!(!format!("{call:?}").contains(RAW));
    assert_eq!(call.principal().client_id, "person:person-1");
    assert!(call.namespace().unwrap().read_namespaces.is_empty());
    let principal = call.principal().clone();
    crate::mcp_auth::with_authenticated_call(call, async {
        let args = json!({"user_id":"someone-else", "status":"open"});
        let result = crate::mcp::person_tools::call(home.path(), &Arc::new(crate::odoo_pool::OdooConnectorPool::default()), &principal, "my_tasks_list", &args).await;
        assert_eq!(result["structuredContent"], json!({"tasks":[{"id":"visible"}]}));
        let seen = stub.seen.lock().unwrap().clone();
        assert_eq!(seen[0].1, json!({"op":"authenticate"}));
        assert_eq!(seen[1], (format!("Bearer {RAW}"), json!({"op":"call","tool":"my_tasks_list","args":args})));
        stub.status.store(403, Ordering::SeqCst);
        let error = crate::mcp::person_tools::call(home.path(), &Arc::new(crate::odoo_pool::OdooConnectorPool::default()), &principal, "my_tasks_list", &json!({})).await;
        assert_eq!(error["isError"], true);
        assert!(error.to_string().contains("scope removed"));
    }).await;
    stub.status.store(401, Ordering::SeqCst);
    assert!(authenticate(home.path(), RAW).await.is_err());
    stub.status.store(200, Ordering::SeqCst);
    *stub.scopes.lock().unwrap() = vec!["admin".into()];
    assert!(authenticate(home.path(), RAW).await.is_err());
    assert!(!home.path().join("agents").exists());
    assert!(!home.path().join("memory.db").exists());
}

#[tokio::test]
async fn http_bearer_authenticates_person_without_namespace() {
    let home = tempfile::tempdir().unwrap();
    let _stub = Stub::start(home.path()).await;
    let path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {RAW}").parse().unwrap());
        let (call, ns) = crate::mcp_http_server::authenticate_bearer_call(&headers, &path, "http://localhost/mcp").unwrap();
        assert!(call.kind().is_person_key());
        assert_eq!(call.raw_person_key(), Some(RAW));
        assert!(ns.read_namespaces.is_empty() && ns.write_namespace.is_empty());
    }).await.unwrap();
}

#[test]
fn malformed_identity_is_not_a_principal() {
    for value in [json!({}), json!({"key_id":"k","user_id":"u","scopes":["unknown"]}),
        json!({"key_id":"","user_id":"u","scopes":[]}), json!({"key_id":"k","user_id":"u","scopes":"tasks:read"})] {
        assert!(identity(value).is_err());
    }
}
