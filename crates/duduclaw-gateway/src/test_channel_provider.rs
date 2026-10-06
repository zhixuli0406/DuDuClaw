//! Local transport fixture for exercising real channel handlers and senders.
//! Overrides are keyed by unique synthetic tokens and exist only in test builds.
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU16, Ordering},
    },
};

#[derive(Clone, Debug)]
pub(crate) struct CapturedRequest {
    pub path: String,
    pub authorization: Option<String>,
    pub body: Value,
}

#[derive(Clone)]
struct FixtureState {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    status: Arc<AtomicU16>,
    responses: Arc<Mutex<HashMap<String, VecDeque<Value>>>>,
    queued: Arc<tokio::sync::Notify>,
}

fn routes() -> &'static Mutex<HashMap<String, String>> {
    static ROUTES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ROUTES.get_or_init(Default::default)
}

pub(crate) fn url(token: &str, original: &str) -> String {
    let Some(origin) = routes().lock().unwrap().get(token).cloned() else {
        return original.to_owned();
    };
    let original = reqwest::Url::parse(original).expect("fixture receives a provider URL");
    format!(
        "{origin}{}{}",
        original.path(),
        original
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default()
    )
}

pub(crate) struct LocalProviderRoute(String);
pub(crate) fn register_local_route(token: &str, origin: &str) -> LocalProviderRoute {
    let parsed = reqwest::Url::parse(origin).unwrap();
    assert!(matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    assert!(matches!(parsed.scheme(), "http" | "https"));
    assert!(token.starts_with("p0-test-"));
    assert!(
        routes()
            .lock()
            .unwrap()
            .insert(token.into(), origin.trim_end_matches('/').into())
            .is_none()
    );
    LocalProviderRoute(token.into())
}
impl Drop for LocalProviderRoute {
    fn drop(&mut self) {
        routes().lock().unwrap().remove(&self.0);
    }
}

pub(crate) struct TestChannelProvider {
    pub token: String,
    state: FixtureState,
    server: tokio::task::JoinHandle<()>,
    _route: LocalProviderRoute,
}
impl TestChannelProvider {
    pub async fn start() -> Self {
        let token = format!("p0-test-{}", uuid::Uuid::new_v4());
        let state = FixtureState {
            requests: Default::default(),
            status: Arc::new(AtomicU16::new(200)),
            responses: Default::default(),
            queued: Default::default(),
        };
        let router = Router::new().fallback(capture).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let route = register_local_route(&token, &origin);
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            token,
            state,
            server,
            _route: route,
        }
    }

    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn refuse(&self) {
        self.state.status.store(503, Ordering::SeqCst);
    }
    pub fn enqueue_response(&self, path_suffix: &str, response: Value) {
        self.state
            .responses
            .lock()
            .unwrap()
            .entry(path_suffix.into())
            .or_default()
            .push_back(response);
        self.state.queued.notify_one();
    }
}
impl Drop for TestChannelProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn capture(
    State(state): State<FixtureState>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> impl IntoResponse {
    state.requests.lock().unwrap().push(CapturedRequest {
        path: uri.path().to_owned(),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    });
    let value = loop {
        let queued = {
            let mut responses = state.responses.lock().unwrap();
            responses
                .iter_mut()
                .find(|(suffix, _)| uri.path().ends_with(suffix.as_str()))
                .and_then(|(_, queue)| queue.pop_front())
        };
        if let Some(value) = queued {
            break value;
        }
        if !uri.path().ends_with("getUpdates") {
            break serde_json::json!({"ok":true,"result":{},"ts":"fixture-receipt"});
        }
        if tokio::time::timeout(
            std::time::Duration::from_millis(200),
            state.queued.notified(),
        )
        .await
        .is_err()
        {
            break serde_json::json!({"ok":true,"result":[]});
        }
    };
    // A queued response may carry its own status as `"__status"`.
    let mut value = value;
    let status = value
        .as_object_mut()
        .and_then(|o| o.remove("__status"))
        .and_then(|v| v.as_u64())
        .map(|s| s as u16)
        .unwrap_or_else(|| state.status.load(Ordering::SeqCst));
    (StatusCode::from_u16(status).unwrap(), Json(value))
}
