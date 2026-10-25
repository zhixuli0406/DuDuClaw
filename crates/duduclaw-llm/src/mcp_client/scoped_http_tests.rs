//! Synthetic loopback transport tests. No provider or OS credential store is used.
//! Initial tests preceded the scoped API; source-only review later added wire
//! discriminators. Neither compile-RED nor any test runtime was observed.

use super::*;
use crate::tool_loop::{ClientOperation, PreparedClientCall, SecretBearer, ToolCallLease};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

const OK: usize = 0;
const HTTP_401: usize = 1;
const HTTP_500: usize = 2;
const RPC_ERROR: usize = 3;
const BAD_JSON: usize = 4;
const WRONG_ID: usize = 5;
const BAD_SSE: usize = 6;
const TOOL_ERROR: usize = 7;
const HANG_TOOL: usize = 8;
const HANG_INITIALIZE: usize = 9;
const REDIRECT: usize = 10;
const CLOSE_TOOL: usize = 11;
const REVOKE_BEFORE_REPLY: usize = 12;
const ECHO_BEARER: usize = 13;
const HANG_SSE: usize = 14;

#[derive(Clone)]
struct ObservedRequest {
    method: String,
    authorization: String,
    session: Option<String>,
    path: String,
    lease_alive: bool,
    arguments: Value,
}

#[derive(Default)]
struct State {
    requests: std::sync::Mutex<Vec<ObservedRequest>>,
    // Count every wire POST before parsing JSON or deciding how to respond.
    received_posts: AtomicUsize,
    pause_prepare: AtomicBool,
    prepare_ready: Notify,
    prepare_resume: Notify,
    pause_validation_after_method: std::sync::Mutex<Option<String>>,
    validation_ready: Notify,
    validation_resume: Notify,
    revision: AtomicUsize,
    prepares: AtomicUsize,
    tool_prepares: AtomicUsize,
    before_calls: AtomicUsize,
    after_calls: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    dropped: AtomicUsize,
    validations: AtomicUsize,
    initialize_count: AtomicUsize,
    calls: AtomicUsize,
    mode: AtomicUsize,
    revoke_on_initialize: AtomicUsize,
    missing_lease: AtomicBool,
    missing_bearer: AtomicBool,
    deny_tool: AtomicBool,
    prepare_delay_ms: AtomicUsize,
    release: Notify,
}

struct Fixture {
    url: String,
    state: Arc<State>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The server task owns a JoinSet; dropping it aborts all fixture sockets.
        self.server.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        state.revision.store(1, Ordering::SeqCst);
        let observed = state.clone();
        let server = tokio::spawn(async move {
            let mut sockets = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { return };
                        let state = observed.clone();
                        sockets.spawn(serve(socket, state));
                    }
                    Some(_) = sockets.join_next(), if !sockets.is_empty() => {}
                }
            }
        });
        Self {
            url: format!("http://{address}/mcp"),
            state,
            server,
        }
    }

    async fn registry(&self, timeout: Duration) -> ToolRegistry {
        ToolRegistry::from_client_slots_named(
            vec![(
                "scoped".into(),
                RegistryClient::scoped_http(
                    ScopedHttpRecipe::new(self.url.clone(), vec![], timeout).unwrap(),
                    Arc::new(HostGuard(self.state.clone())),
                ),
            )],
            vec![],
        )
        .await
        .unwrap()
    }

    async fn wait_for_calls(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.state.calls.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("fixture tools/call was not observed");
    }
}

async fn serve(mut socket: tokio::net::TcpStream, state: Arc<State>) {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    let body_start = loop {
        let size = socket.read(&mut chunk).await.unwrap_or(0);
        if size == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..size]);
        assert!(bytes.len() < 64 * 1024, "oversized fixture request");
        if let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..body_start]).into_owned();
    if headers
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("POST "))
    {
        state.received_posts.fetch_add(1, Ordering::SeqCst);
    }
    let header = |name: &str| {
        headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    let length = header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let authorization = header("authorization").unwrap_or_default();
    let session = header("mcp-session-id");
    let path = headers
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_owned();
    assert!(length < 32 * 1024, "oversized fixture body");
    while bytes.len() < body_start + length {
        let size = socket.read(&mut chunk).await.unwrap_or(0);
        if size == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..size]);
    }
    let frame: Value = serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
    let method = frame["method"].as_str().unwrap_or("").to_owned();
    let id = frame.get("id").cloned().unwrap_or(Value::Null);
    state.requests.lock().unwrap().push(ObservedRequest {
        method: method.clone(),
        authorization: authorization.clone(),
        session,
        path,
        lease_alive: state.active.load(Ordering::SeqCst) > 0,
        arguments: frame
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or(Value::Null),
    });
    let mode = state.mode.load(Ordering::SeqCst);
    let mut extra = String::new();
    let mut content_type = "application/json";
    let mut status = "200 OK";
    let body = match method.as_str() {
        "initialize" => {
            let number = state.initialize_count.fetch_add(1, Ordering::SeqCst) + 1;
            if mode == HANG_INITIALIZE {
                state.release.notified().await;
            }
            if state.revoke_on_initialize.load(Ordering::SeqCst) == number {
                state.revision.fetch_add(1, Ordering::SeqCst);
            }
            extra = format!("mcp-session-id: fixture-{number}\r\n");
            json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}}).to_string()
        }
        "notifications/initialized" => {
            status = "202 Accepted";
            String::new()
        }
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
            {"name":"echo","description":"synthetic echo","inputSchema":{"type":"object"}},
            {"name":"danger","description":"filtered fixture","inputSchema":{"type":"object"}}
        ]}})
        .to_string(),
        "tools/call" => {
            state.calls.fetch_add(1, Ordering::SeqCst);
            if mode == CLOSE_TOOL {
                return;
            }
            if mode == HANG_TOOL {
                state.release.notified().await;
            }
            if mode == HANG_SSE {
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 4096\r\n\r\ndata: ").await;
                state.release.notified().await;
                return;
            }
            if mode == REVOKE_BEFORE_REPLY {
                state.revision.fetch_add(1, Ordering::SeqCst);
            }
            match mode {
                HTTP_401 | HTTP_500 => {
                    status = if mode == HTTP_401 { "401 Unauthorized" } else { "500 Internal Server Error" };
                    authorization.clone()
                }
                RPC_ERROR => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":authorization}}).to_string(),
                BAD_JSON => "invalid-json".into(),
                WRONG_ID => json!({"jsonrpc":"2.0","id":999,"result":{}}).to_string(),
                BAD_SSE => { content_type = "text/event-stream"; "data: {}\n\n".into() }
                REDIRECT => { status = "302 Found"; extra = "Location: /other\r\n".into(); String::new() }
                _ => json!({"jsonrpc":"2.0","id":id,"result":{"isError":mode == TOOL_ERROR,"content":[{"type":"text","text":if mode == ECHO_BEARER { authorization.as_str() } else { "private_result" }}]}}).to_string(),
            }
        }
        _ => {
            status = "400 Bad Request";
            String::new()
        }
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

struct Lease {
    state: Arc<State>,
    revision: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ToolCallLease for Lease {
    async fn validate_before_send(&mut self) -> Result<(), String> {
        self.state.validations.fetch_add(1, Ordering::SeqCst);
        // Snapshot only: the external test task is the sole authority writer.
        // Pausing after this read lets it revoke before the next actual POST;
        // post-response validation alone must not hide a missing send gate.
        let valid = self.state.revision.load(Ordering::SeqCst) == self.revision;
        let last_method = self
            .state
            .requests
            .lock()
            .unwrap()
            .last()
            .map(|request| request.method.clone());
        let pause = last_method.as_ref().is_some_and(|method| {
            let mut requested = self.state.pause_validation_after_method.lock().unwrap();
            if requested.as_ref() == Some(method) {
                requested.take();
                true
            } else {
                false
            }
        });
        if pause {
            self.state.validation_ready.notify_one();
            self.state.validation_resume.notified().await;
        }
        if valid {
            Ok(())
        } else {
            Err("synthetic authority changed".into())
        }
    }
}
struct HostGuard(Arc<State>);
#[async_trait]
impl ToolInterceptor for HostGuard {
    fn before_call(&self, _: &str, _: &str, args: Value) -> InterceptDecision {
        self.0.before_calls.fetch_add(1, Ordering::SeqCst);
        InterceptDecision::Allow(args)
    }
    async fn prepare_client_call(
        &self,
        _: &str,
        operation: ClientOperation<'_>,
        mut args: Value,
    ) -> Result<PreparedClientCall, String> {
        self.0.prepares.fetch_add(1, Ordering::SeqCst);
        if let ClientOperation::Tool { .. } = operation {
            self.0.tool_prepares.fetch_add(1, Ordering::SeqCst);
            if self.0.deny_tool.load(Ordering::SeqCst) {
                return Err("synthetic refusal".into());
            }
            args["host_rewritten"] = json!(true);
        }
        let revision = self.0.revision.load(Ordering::SeqCst);
        let lease: Option<Box<dyn ToolCallLease>> = if self.0.missing_lease.load(Ordering::SeqCst) {
            None
        } else {
            let active = self.0.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.0.max_active.fetch_max(active, Ordering::SeqCst);
            Some(Box::new(Lease {
                state: self.0.clone(),
                revision,
            }))
        };
        let delay = self.0.prepare_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        }
        let fresh_http_bearer = (!self.0.missing_bearer.load(Ordering::SeqCst))
            .then(|| SecretBearer::new(format!("scope-token-{revision}")));
        if self.0.pause_prepare.load(Ordering::SeqCst) {
            // Both proofs already belong to this future and use revision1.
            self.0.prepare_ready.notify_one();
            self.0.prepare_resume.notified().await;
        }
        Ok(PreparedClientCall {
            arguments: args,
            lease,
            fresh_http_bearer,
        })
    }
    fn after_call(&self, _: &str, _: &str, _: &Value, _: &mut Value) {
        self.0.after_calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn discovery_is_guarded_and_does_not_retain_a_lease() {
    let fixture = Fixture::new().await;
    let registry = fixture.registry(Duration::from_secs(2)).await;
    assert_eq!(fixture.state.prepares.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.lease_alive)
    );
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
    assert_eq!(registry.defs().len(), 2);
    assert_eq!(fixture.state.before_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn every_call_uses_its_own_bearer_session_and_rewritten_arguments() {
    let fixture = Fixture::new().await;
    let mut registry = fixture.registry(Duration::from_secs(2)).await;
    registry.set_client_notice(0, "scoped-account").unwrap();
    assert!(!registry.call("echo", json!({})).await.unwrap().is_error);
    fixture.state.revision.store(2, Ordering::SeqCst);
    let result = registry.call("echo", json!({})).await.unwrap();
    assert_eq!(result.host_notice.as_deref(), Some("scoped-account"));
    let requests = fixture.state.requests.lock().unwrap();
    let calls: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "tools/call")
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].authorization, "Bearer scope-token-1");
    assert_eq!(calls[1].authorization, "Bearer scope-token-2");
    assert_ne!(calls[0].session, calls[1].session);
    assert!(
        calls
            .iter()
            .all(|request| request.arguments["host_rewritten"] == true)
    );
    assert!(requests.iter().all(|request| request.lease_alive));
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.after_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.state.before_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_authority_never_opens_a_connection() {
    for missing_token in [false, true] {
        let fixture = Fixture::new().await;
        fixture
            .state
            .missing_lease
            .store(!missing_token, Ordering::SeqCst);
        fixture
            .state
            .missing_bearer
            .store(missing_token, Ordering::SeqCst);
        let result = ToolRegistry::from_client_slots_named(
            vec![(
                "scoped".into(),
                RegistryClient::scoped_http(
                    ScopedHttpRecipe::new(fixture.url.clone(), vec![], Duration::from_secs(1))
                        .unwrap(),
                    Arc::new(HostGuard(fixture.state.clone())),
                ),
            )],
            vec![],
        )
        .await;
        assert!(result.is_err());
        assert!(fixture.state.requests.lock().unwrap().is_empty());
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn initialization_revocation_prevents_the_next_post() {
    let fixture = Fixture::new().await;
    let registry = fixture.registry(Duration::from_secs(2)).await;
    fixture
        .state
        .revoke_on_initialize
        .store(2, Ordering::SeqCst);
    let result = registry.call("echo", json!({})).await;
    assert!(result.is_err() || result.is_ok_and(|value| value.is_error));
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 4);
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn revocation_while_receiving_a_result_withholds_that_result() {
    let fixture = Fixture::new().await;
    let registry = fixture.registry(Duration::from_secs(2)).await;
    fixture
        .state
        .mode
        .store(REVOKE_BEFORE_REPLY, Ordering::SeqCst);
    let result = registry.call("echo", json!({})).await;
    let text = match result {
        Ok(value) => {
            assert!(value.is_error);
            value.content
        }
        Err(error) => error,
    };
    assert!(!text.contains("private_result"));
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn errors_are_single_attempt_and_release_the_lease() {
    for mode in [
        HTTP_401, HTTP_500, RPC_ERROR, BAD_JSON, WRONG_ID, BAD_SSE, TOOL_ERROR, CLOSE_TOOL,
        REDIRECT,
    ] {
        let fixture = Fixture::new().await;
        let registry = fixture.registry(Duration::from_secs(1)).await;
        fixture.state.mode.store(mode, Ordering::SeqCst);
        let result = registry.call("echo", json!({})).await;
        let text = match result {
            Ok(value) => {
                assert!(value.is_error);
                value.content
            }
            Err(error) => error,
        };
        assert!(!text.contains("scope-token-1"));
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        assert!(
            fixture
                .state
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.path == "/mcp")
        );
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn success_redacts_the_current_bearer_from_provider_content() {
    let fixture = Fixture::new().await;
    let registry = fixture.registry(Duration::from_secs(1)).await;
    fixture.state.mode.store(ECHO_BEARER, Ordering::SeqCst);
    let result = registry.call("echo", json!({})).await.unwrap();
    assert!(!result.content.contains("scope-token-1"));
    assert!(result.content.contains("[REDACTED]"));
}

#[tokio::test]
async fn canceled_tool_and_sse_waits_release_local_authority_without_replay() {
    for mode in [HANG_TOOL, HANG_SSE] {
        let fixture = Fixture::new().await;
        let registry = Arc::new(fixture.registry(Duration::from_secs(2)).await);
        fixture.state.mode.store(mode, Ordering::SeqCst);
        let running = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.call("echo", json!({})).await })
        };
        fixture.wait_for_calls(1).await;
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 1);
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        // The remote fixture may still be handling the canceled POST.
        fixture.state.release.notify_one();
        fixture.state.mode.store(OK, Ordering::SeqCst);
        assert!(!registry.call("echo", json!({})).await.unwrap().is_error);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn deadline_covers_prepare_and_handshake_and_releases_owned_values() {
    for delayed_prepare in [false, true] {
        let fixture = Fixture::new().await;
        if delayed_prepare {
            fixture.state.prepare_delay_ms.store(100, Ordering::SeqCst);
        } else {
            fixture.state.mode.store(HANG_INITIALIZE, Ordering::SeqCst);
        }
        let result = ToolRegistry::from_client_slots_named(
            vec![(
                "scoped".into(),
                RegistryClient::scoped_http(
                    ScopedHttpRecipe::new(fixture.url.clone(), vec![], Duration::from_millis(20))
                        .unwrap(),
                    Arc::new(HostGuard(fixture.state.clone())),
                ),
            )],
            vec![],
        )
        .await;
        assert!(result.is_err());
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
        fixture.state.release.notify_one();
    }
}

#[tokio::test]
async fn deadline_also_bounds_waiting_for_the_exact_client_lock() {
    let fixture = Fixture::new().await;
    let registry = fixture.registry(Duration::from_millis(500)).await;
    let held = registry.clients[0].lock().await;
    let result = tokio::time::timeout(Duration::from_secs(1), registry.call("echo", json!({})))
        .await
        .unwrap();
    assert!(result.unwrap().is_error);
    assert_eq!(fixture.state.tool_prepares.load(Ordering::SeqCst), 0);
    drop(held);
}

#[tokio::test]
async fn queued_calls_prepare_only_after_the_prior_call_releases_its_lease() {
    let fixture = Fixture::new().await;
    let registry = Arc::new(fixture.registry(Duration::from_secs(2)).await);
    fixture.state.mode.store(HANG_TOOL, Ordering::SeqCst);
    let first = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.call("echo", json!({})).await })
    };
    fixture.wait_for_calls(1).await;
    let second = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.call("echo", json!({})).await })
    };
    tokio::task::yield_now().await;
    assert_eq!(fixture.state.tool_prepares.load(Ordering::SeqCst), 1);
    fixture.state.deny_tool.store(true, Ordering::SeqCst);
    fixture.state.release.notify_one();
    assert!(!first.await.unwrap().unwrap().is_error);
    let second = second.await.unwrap().unwrap();
    assert!(second.is_error);
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.max_active.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn colliding_names_keep_the_first_route_guard_and_notice() {
    let first = Fixture::new().await;
    let second = Fixture::new().await;
    let mut registry = ToolRegistry::from_client_slots_named(
        vec![
            (
                "same".into(),
                RegistryClient::scoped_http(
                    ScopedHttpRecipe::new(first.url.clone(), vec![], Duration::from_secs(1))
                        .unwrap(),
                    Arc::new(HostGuard(first.state.clone())),
                ),
            ),
            (
                "same".into(),
                RegistryClient::scoped_http(
                    ScopedHttpRecipe::new(second.url.clone(), vec![], Duration::from_secs(1))
                        .unwrap(),
                    Arc::new(HostGuard(second.state.clone())),
                ),
            ),
        ],
        vec![
            ToolFilter {
                allowed: vec!["echo".into()],
                denied: vec![],
            },
            ToolFilter::default(),
        ],
    )
    .await
    .unwrap();
    registry.set_client_notice(1, "wrong-account").unwrap();
    second.state.deny_tool.store(true, Ordering::SeqCst);
    assert!(
        registry
            .register_client_interceptor(0, Arc::new(HostGuard(second.state.clone())))
            .is_err()
    );
    let result = registry.call("echo", json!({})).await.unwrap();
    assert!(!result.is_error);
    assert!(result.host_notice.is_none());
    assert_eq!(first.state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.state.calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.server_for_tool("echo"), Some("same"));
}

#[test]
fn scoped_recipe_refuses_auth_session_overrides_and_unbounded_deadlines() {
    for name in [
        "Authorization",
        "proxy-authorization",
        "Cookie",
        "Mcp-Session-Id",
    ] {
        assert!(
            ScopedHttpRecipe::new(
                "https://example.invalid/mcp".into(),
                vec![(name.into(), "value".into())],
                Duration::from_secs(1)
            )
            .is_err()
        );
    }
    for timeout in [Duration::ZERO, Duration::from_secs(301)] {
        assert!(
            ScopedHttpRecipe::new("https://example.invalid/mcp".into(), vec![], timeout).is_err()
        );
    }
    assert!(
        ScopedHttpRecipe::new(
            "https://user:pass@example.invalid/mcp".into(),
            vec![],
            Duration::from_secs(1)
        )
        .is_err()
    );
}

struct LegacyGuard(Arc<State>);
#[async_trait]
impl ToolInterceptor for LegacyGuard {
    fn before_call(&self, _: &str, _: &str, mut args: Value) -> InterceptDecision {
        self.0.before_calls.fetch_add(1, Ordering::SeqCst);
        args["legacy_rewritten"] = json!(true);
        InterceptDecision::Allow(args)
    }
    fn after_call(&self, _: &str, _: &str, _: &Value, result: &mut Value) {
        self.0.after_calls.fetch_add(1, Ordering::SeqCst);
        *result = json!("legacy_transformed");
    }
}

#[tokio::test]
async fn a_legacy_guard_cannot_authorize_scoped_discovery() {
    let fixture = Fixture::new().await;
    let result = ToolRegistry::from_client_slots_named(
        vec![(
            "scoped".into(),
            RegistryClient::scoped_http(
                ScopedHttpRecipe::new(fixture.url.clone(), vec![], Duration::from_secs(1)).unwrap(),
                Arc::new(LegacyGuard(fixture.state.clone())),
            ),
        )],
        vec![],
    )
    .await;
    assert!(result.is_err());
    assert!(fixture.state.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.state.before_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mixed_slots_preserve_persistent_hooks_filter_and_first_wins() {
    let persistent = Fixture::new().await;
    let scoped = Fixture::new().await;
    let client = McpClient::connect_http(&persistent.url, &[], Duration::from_secs(2))
        .await
        .unwrap();
    let mut registry = ToolRegistry::from_client_slots_named(
        vec![
            ("legacy".into(), RegistryClient::Persistent(client)),
            (
                "scoped".into(),
                RegistryClient::scoped_http(
                    ScopedHttpRecipe::new(scoped.url.clone(), vec![], Duration::from_secs(2))
                        .unwrap(),
                    Arc::new(HostGuard(scoped.state.clone())),
                ),
            ),
        ],
        vec![
            ToolFilter {
                allowed: vec!["echo".into()],
                denied: vec![],
            },
            ToolFilter::default(),
        ],
    )
    .await
    .unwrap();
    registry
        .register_client_interceptor(0, Arc::new(LegacyGuard(persistent.state.clone())))
        .unwrap();
    registry.set_client_notice(0, "legacy-account").unwrap();
    let result = registry.call("echo", json!({})).await.unwrap();
    assert_eq!(result.content, "legacy_transformed");
    assert_eq!(result.host_notice.as_deref(), Some("legacy-account"));
    assert_eq!(registry.server_for_tool("echo"), Some("legacy"));
    assert_eq!(registry.server_for_tool("danger"), Some("scoped"));
    assert_eq!(persistent.state.before_calls.load(Ordering::SeqCst), 1);
    assert_eq!(persistent.state.after_calls.load(Ordering::SeqCst), 1);
    assert_eq!(scoped.state.tool_prepares.load(Ordering::SeqCst), 0);
    let requests = persistent.state.requests.lock().unwrap();
    let call = requests
        .iter()
        .find(|request| request.method == "tools/call")
        .unwrap();
    assert_eq!(call.arguments["legacy_rewritten"], true);
    assert_eq!(call.authorization, "");
    drop(requests);
    assert!(!registry.call("danger", json!({})).await.unwrap().is_error);
    assert_eq!(scoped.state.tool_prepares.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_missing_proof_after_discovery_does_not_send_another_request() {
    for missing_bearer in [false, true] {
        let fixture = Fixture::new().await;
        let registry = fixture.registry(Duration::from_secs(2)).await;
        let before = fixture.state.requests.lock().unwrap().len();
        fixture
            .state
            .missing_bearer
            .store(missing_bearer, Ordering::SeqCst);
        fixture
            .state
            .missing_lease
            .store(!missing_bearer, Ordering::SeqCst);
        assert!(registry.call("echo", json!({})).await.unwrap().is_error);
        assert_eq!(fixture.state.requests.lock().unwrap().len(), before);
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn a_tools_response_or_sse_timeout_releases_authority_without_replay() {
    for mode in [HANG_TOOL, HANG_SSE] {
        let fixture = Fixture::new().await;
        let registry = fixture.registry(Duration::from_millis(500)).await;
        fixture.state.mode.store(mode, Ordering::SeqCst);
        let result = tokio::time::timeout(Duration::from_secs(2), registry.call("echo", json!({})))
            .await
            .unwrap();
        let text = match result {
            Ok(result) => {
                assert!(result.is_error);
                result.content
            }
            Err(error) => error,
        };
        assert!(text.contains("unconfirmed"));
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        fixture.state.release.notify_one();
    }
}

#[tokio::test]
async fn cancel_during_preparation_or_handshake_drops_the_owned_lease() {
    for during_prepare in [false, true] {
        let fixture = Fixture::new().await;
        if during_prepare {
            fixture
                .state
                .prepare_delay_ms
                .store(10_000, Ordering::SeqCst);
        } else {
            fixture.state.mode.store(HANG_INITIALIZE, Ordering::SeqCst);
        }
        let slot = RegistryClient::scoped_http(
            ScopedHttpRecipe::new(fixture.url.clone(), vec![], Duration::from_secs(2)).unwrap(),
            Arc::new(HostGuard(fixture.state.clone())),
        );
        let running = tokio::spawn(async move {
            ToolRegistry::from_client_slots_named(vec![("scoped".into(), slot)], vec![]).await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while fixture.state.active.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        running.abort();
        assert!(matches!(running.await, Err(error) if error.is_cancelled()));
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
        fixture.state.release.notify_one();
    }
}

// The three tests below discriminate before-send validation from merely
// withholding data after a response. No mutation or runtime was executed in
// the source-only phase; the removal analysis is a static counterfactual.
fn assert_static_scoped_failure(result: Result<ToolRegistry, McpError>) {
    match result {
        Err(error) => assert_eq!(
            error,
            McpError::Io(
                "Scoped MCP result unconfirmed; remote execution may have occurred".into(),
            )
        ),
        Ok(_) => panic!("revoked scoped discovery unexpectedly succeeded"),
    }
}

#[tokio::test]
async fn revocation_after_owned_prepare_prevents_even_the_first_initialize_post() {
    let fixture = Fixture::new().await;
    fixture.state.pause_prepare.store(true, Ordering::SeqCst);
    let slot = RegistryClient::scoped_http(
        ScopedHttpRecipe::new(fixture.url.clone(), vec![], Duration::from_secs(2)).unwrap(),
        Arc::new(HostGuard(fixture.state.clone())),
    );
    let running = tokio::spawn(async move {
        ToolRegistry::from_client_slots_named(vec![("scoped".into(), slot)], vec![]).await
    });
    tokio::time::timeout(
        Duration::from_secs(1),
        fixture.state.prepare_ready.notified(),
    )
    .await
    .unwrap();
    assert_eq!(fixture.state.revision.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.received_posts.load(Ordering::SeqCst), 0);
    // This task revokes after the owned rev1 proof exists, before the
    // transport can perform its first validation or network request.
    fixture.state.revision.store(2, Ordering::SeqCst);
    fixture.state.prepare_resume.notify_one();
    assert_static_scoped_failure(running.await.unwrap());
    assert_eq!(fixture.state.received_posts.load(Ordering::SeqCst), 0);
    assert!(fixture.state.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.state.initialize_count.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn revocation_after_a_successful_validation_blocks_notification_and_list_posts() {
    for (checkpoint, expected_methods) in [
        ("initialize", vec!["initialize"]),
        (
            "notifications/initialized",
            vec!["initialize", "notifications/initialized"],
        ),
    ] {
        let fixture = Fixture::new().await;
        *fixture.state.pause_validation_after_method.lock().unwrap() = Some(checkpoint.into());
        let slot = RegistryClient::scoped_http(
            ScopedHttpRecipe::new(fixture.url.clone(), vec![], Duration::from_secs(2)).unwrap(),
            Arc::new(HostGuard(fixture.state.clone())),
        );
        let running = tokio::spawn(async move {
            ToolRegistry::from_client_slots_named(vec![("scoped".into(), slot)], vec![]).await
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            fixture.state.validation_ready.notified(),
        )
        .await
        .unwrap();
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.state.received_posts.load(Ordering::SeqCst),
            expected_methods.len()
        );
        // validate has already read rev1 as allowed and is about to return
        // that read-only decision. Only the NEXT POST's validation can deny.
        fixture.state.revision.store(2, Ordering::SeqCst);
        fixture.state.validation_resume.notify_one();
        assert_static_scoped_failure(running.await.unwrap());
        assert_eq!(
            fixture.state.received_posts.load(Ordering::SeqCst),
            expected_methods.len()
        );
        let methods: Vec<_> = fixture
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.method.clone())
            .collect();
        assert_eq!(methods, expected_methods);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.dropped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn revocation_after_notification_validation_prevents_the_tools_call_post() {
    let fixture = Fixture::new().await;
    let registry = Arc::new(fixture.registry(Duration::from_secs(2)).await);
    let before_posts = fixture.state.received_posts.load(Ordering::SeqCst);
    let before_drops = fixture.state.dropped.load(Ordering::SeqCst);
    let before_requests = fixture.state.requests.lock().unwrap().len();
    *fixture.state.pause_validation_after_method.lock().unwrap() =
        Some("notifications/initialized".into());
    let running = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.call("echo", json!({})).await })
    };
    tokio::time::timeout(
        Duration::from_secs(1),
        fixture.state.validation_ready.notified(),
    )
    .await
    .unwrap();
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 1);
    fixture.state.revision.store(2, Ordering::SeqCst);
    fixture.state.validation_resume.notify_one();
    match running.await.unwrap() {
        Err(error) => assert_eq!(
            error,
            "mcp io error: Scoped MCP result unconfirmed; remote execution may have occurred"
        ),
        Ok(_) => panic!("revoked scoped tools/call unexpectedly succeeded"),
    }
    assert_eq!(
        fixture.state.received_posts.load(Ordering::SeqCst),
        before_posts + 2
    );
    let methods: Vec<_> = fixture.state.requests.lock().unwrap()[before_requests..]
        .iter()
        .map(|request| request.method.clone())
        .collect();
    assert_eq!(methods, ["initialize", "notifications/initialized"]);
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.state.dropped.load(Ordering::SeqCst),
        before_drops + 1
    );
}
