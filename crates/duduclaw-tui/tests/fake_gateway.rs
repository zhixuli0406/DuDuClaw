//! Socket tests deliberately fail on bind refusal unless explicitly opted out.
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use duduclaw_gateway::protocol::WsFrame;
use duduclaw_tui::{
    Error, Result,
    auth::{AuthClient, LoginMode, LoginPrompt, PromptField},
    client::{Client, ClientOptions, ConnectionState},
    state::{Model, StateFeed},
    transport::GatewayUrl,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch},
    task::JoinHandle,
    time::timeout,
};
use zeroize::Zeroizing;

#[derive(Clone, Debug)]
enum Control {
    Push(Value),
    Close,
}
struct Fixture {
    events: broadcast::Sender<Control>,
    connections: AtomicUsize,
    refreshed_connections: AtomicUsize,
    refreshes: AtomicUsize,
    fail_refresh: AtomicBool,
    list_calls: AtomicUsize,
    list_delay_ms: AtomicU64,
    short_token: bool,
}
struct Gateway {
    target: GatewayUrl,
    fixture: Arc<Fixture>,
    task: JoinHandle<()>,
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Gateway {
    async fn start(short_token: bool) -> Option<Self> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("NOT RUN: fake gateway socket bind failed: {error}");
                assert!(
                    std::env::var("DUDUCLAW_TUI_ALLOW_NO_BIND").as_deref() == Ok("1"),
                    "NOT RUN: bind refused; set DUDUCLAW_TUI_ALLOW_NO_BIND=1 only to acknowledge this unexecuted socket test"
                );
                return None;
            }
        };
        let addr = listener.local_addr().unwrap();
        let (events, _) = broadcast::channel(64);
        let fixture = Arc::new(Fixture {
            events,
            connections: AtomicUsize::new(0),
            refreshed_connections: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            fail_refresh: AtomicBool::new(false),
            list_calls: AtomicUsize::new(0),
            list_delay_ms: AtomicU64::new(0),
            short_token,
        });
        let app = Router::new()
            .route("/api/session/local", post(local_session))
            .route("/api/refresh", post(refresh))
            .route("/ws", get(upgrade))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Some(Self {
            target: GatewayUrl::parse(&format!("http://{addr}")).unwrap(),
            fixture,
            task,
        })
    }
    async fn client(&self) -> Client {
        struct NoPrompt;
        #[async_trait]
        impl LoginPrompt for NoPrompt {
            async fn read(&mut self, _: PromptField) -> Result<Zeroizing<String>> {
                panic!("local login must not prompt");
            }
        }
        let auth = AuthClient::new(self.target.clone()).unwrap();
        let session = auth
            .login(LoginMode::Personal, &mut NoPrompt)
            .await
            .unwrap();
        Client::start(
            auth,
            session,
            ClientOptions {
                request_timeout: Duration::from_secs(2),
                refresh_margin: Duration::from_secs(1),
                backoff_base: Duration::from_millis(10),
                backoff_max: Duration::from_millis(50),
            },
        )
        .unwrap()
    }
    fn push(&self, name: &str, seq: u64, boot: &str) {
        self.fixture
            .events
            .send(Control::Push(
                json!({"type":"event","event":name,"payload":{},"seq":seq,"boot_id":boot}),
            ))
            .unwrap();
    }
}
fn token(ttl: u64, refreshed: bool) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!(
        "e30.{}.sig",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"sub":"u1","exp":now+ttl,"refreshed":refreshed})).unwrap()
        )
    )
}
async fn local_session(State(f): State<Arc<Fixture>>, headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers
            .get("x-duduclaw-local")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
    Json(
        json!({"access_token":token(if f.short_token { 3 } else { 3600 },false),"refresh_token":token(7200,false),
        "user":{"id":"u1","email":"fixture@example.com","role":"employee","must_change_password":false}}),
    )
}
async fn refresh(
    State(f): State<Arc<Fixture>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    assert!(
        body.get("refresh_token").and_then(Value::as_str).is_some(),
        "refresh shape mismatch"
    );
    assert_eq!(body.as_object().unwrap().len(), 1);
    f.refreshes.fetch_add(1, Ordering::SeqCst);
    if f.fail_refresh.load(Ordering::SeqCst) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"expired"}))).into_response();
    }
    Json(json!({"access_token":token(3600,true)})).into_response()
}
async fn upgrade(ws: WebSocketUpgrade, State(f): State<Arc<Fixture>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| serve(socket, f))
}
async fn send(socket: &mut WebSocket, frame: WsFrame) -> bool {
    socket
        .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
        .await
        .is_ok()
}
async fn serve(mut socket: WebSocket, f: Arc<Fixture>) {
    let mut controls = f.events.subscribe();
    let Some(Ok(Message::Text(text))) = socket.recv().await else {
        return;
    };
    let WsFrame::Request { id, method, params } = serde_json::from_str(&text).unwrap() else {
        panic!("handshake must be a request");
    };
    assert_eq!(method, "connect");
    assert!(params.get("jwt").and_then(Value::as_str).is_some());
    assert!(params.get("token").is_none());
    assert_eq!(params.get("client").and_then(Value::as_str), Some("tui"));
    let payload = params["jwt"].as_str().unwrap().split('.').nth(1).unwrap();
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    if claims["refreshed"] == true {
        f.refreshed_connections.fetch_add(1, Ordering::SeqCst);
    }
    f.connections.fetch_add(1, Ordering::SeqCst);
    if !send(&mut socket,WsFrame::ok_response(&id,json!({"status":"authenticated","user":{"id":"u1","email":"fixture@example.com","role":"employee"},"must_change_password":false,"boot_id":"boot-a","version":"fixture"}))).await { return; }
    let mut first = None;
    loop {
        tokio::select! {
            control = controls.recv() => match control {
                Ok(Control::Push(value)) => { if socket.send(Message::Text(value.to_string().into())).await.is_err() { return; } }
                Ok(Control::Close) => { let _ = socket.send(Message::Close(None)).await; return; }
                _ => return,
            },
            message = socket.recv() => {
                let Some(Ok(message)) = message else { return; };
                match message {
                    Message::Text(text) => {
                        let WsFrame::Request { id,method,params } = serde_json::from_str(&text).unwrap() else { panic!("RPC must be a request"); };
                        let response = match method.as_str() {
                            "agents.list" => json!({"agents":[{"name":"adu","display_name":"阿嘟\u{202e}\u{1b}","status":"active","archived":false}]}),
                            "approvals.list" => {
                                f.list_calls.fetch_add(1,Ordering::SeqCst);
                                tokio::time::sleep(Duration::from_millis(f.list_delay_ms.load(Ordering::SeqCst))).await;
                                json!({"approvals":[{"id":"a1","agent_id":"adu","kind":"tool","summary":"需要審核","status":"pending","request_kind":"question","expires_at":null,"decided_in_dashboard_only":true}],"count":1})
                            }
                            "tasks.list_page" => {
                                assert_eq!(params["agent_id"],"adu","non-admin requires agent scope");
                                assert_eq!(params["status"],"needs_human");
                                json!({"tasks":[{"id":"t1","title":"請確認","assigned_to":"adu","status":"needs_human","pause_reason":"unknown","goal_mode":true}],"total":1,"limit":200,"offset":0})
                            }
                            "pair.first" => { first = Some(id); continue; }
                            "pair.second" => {
                                if !send(&mut socket,WsFrame::ok_response(&id,json!({"value":2}))).await { return; }
                                let first_id = first.take().expect("first request before second");
                                if !send(&mut socket,WsFrame::ok_response(&first_id,json!({"value":1}))).await { return; }
                                continue;
                            }
                            "expire" => {
                                let response = WsFrame::Response { id, ok:false, payload:None, error:Some(json!({"code":"jwt_expired"})) };
                                if !send(&mut socket,response).await { return; }
                                continue;
                            }
                            "never" => continue,
                            "reject" => { if !send(&mut socket,WsFrame::error_response(&id,"untrusted-secret\u{1b}")).await { return; } continue; }
                            _ => panic!("unexpected method"),
                        };
                        if !send(&mut socket,WsFrame::ok_response(&id,response)).await { return; }
                    }
                    Message::Ping(bytes) => { if socket.send(Message::Pong(bytes)).await.is_err() { return; } }
                    Message::Close(_) => return,
                    _ => {},
                }
            }
        }
    }
}
async fn connected(client: &Client, minimum: u64) {
    let mut state = client.connection();
    timeout(Duration::from_secs(6),async {
        loop {
            if matches!(&*state.borrow_and_update(),ConnectionState::Connected(info) if info.generation >= minimum) { return; }
            state.changed().await.unwrap();
        }
    }).await.expect("connection deadline");
}
async fn model_until(receiver: &mut watch::Receiver<Model>, predicate: impl Fn(&Model) -> bool) {
    timeout(Duration::from_secs(6), async {
        loop {
            if predicate(&receiver.borrow_and_update()) {
                return;
            }
            receiver.changed().await.unwrap();
        }
    })
    .await
    .expect("model deadline");
}

#[tokio::test]
async fn handshake_correlation_timeout_and_rejected_rpc() {
    let Some(gateway) = Gateway::start(false).await else {
        return;
    };
    let client = gateway.client().await;
    connected(&client, 1).await;
    let a = client.call("pair.first", json!({}), Duration::from_secs(2));
    let b = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        client
            .call("pair.second", json!({}), Duration::from_secs(2))
            .await
    };
    let (a, b) = tokio::join!(a, b);
    assert_eq!(a.unwrap()["value"], 1);
    assert_eq!(b.unwrap()["value"], 2);
    assert_eq!(
        client
            .call("never", json!({}), Duration::from_millis(40))
            .await,
        Err(Error::Timeout)
    );
    assert_eq!(
        client
            .call("reject", json!({}), Duration::from_secs(1))
            .await,
        Err(Error::RpcRejected)
    );
}
#[tokio::test]
async fn list_poll_push_loss_gap_regression_boot_change_and_reconnect() {
    let Some(gateway) = Gateway::start(false).await else {
        return;
    };
    let client = gateway.client().await;
    let mut feed = StateFeed::start(client.clone(), Duration::from_secs(1));
    model_until(&mut feed.receiver, |m| m.inbox.count() == Some(2)).await;
    assert_eq!(
        feed.receiver.borrow().employees.items[0]
            .display_name
            .plain(),
        "阿嘟"
    );
    let calls = gateway.fixture.list_calls.load(Ordering::SeqCst);
    timeout(Duration::from_secs(3), async {
        while gateway.fixture.list_calls.load(Ordering::SeqCst) == calls {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("polling works without live events");
    gateway.fixture.list_delay_ms.store(120, Ordering::SeqCst);
    for (name, seq, boot) in [
        ("approval.created", 1, "boot-a"),
        ("events_lost", 2, "boot-a"),
        ("future.event", 4, "boot-a"),
        ("future.event", 3, "boot-a"),
        ("future.event", 1, "boot-b"),
    ] {
        gateway.push(name, seq, boot);
        model_until(&mut feed.receiver, |m| m.inbox.count().is_none()).await;
        model_until(&mut feed.receiver, |m| m.inbox.count() == Some(2)).await;
    }
    gateway.fixture.events.send(Control::Close).unwrap();
    model_until(&mut feed.receiver, |m| m.employees.count().is_none()).await;
    connected(&client, 2).await;
    model_until(&mut feed.receiver, |m| m.inbox.count() == Some(2)).await;
    assert!(gateway.fixture.connections.load(Ordering::SeqCst) >= 2);
}
#[tokio::test]
async fn token_expiry_refresh_reconnect_uses_new_access_token() {
    let Some(gateway) = Gateway::start(true).await else {
        return;
    };
    let client = gateway.client().await;
    connected(&client, 1).await;
    connected(&client, 2).await;
    assert!(gateway.fixture.refreshes.load(Ordering::SeqCst) >= 1);
    assert!(gateway.fixture.refreshed_connections.load(Ordering::SeqCst) >= 1);
}
#[tokio::test]
async fn refresh_failure_surfaces_login_required_without_local_fallback() {
    let Some(gateway) = Gateway::start(true).await else {
        return;
    };
    gateway.fixture.fail_refresh.store(true, Ordering::SeqCst);
    let client = gateway.client().await;
    connected(&client, 1).await;
    let mut state = client.connection();
    timeout(Duration::from_secs(6), async {
        loop {
            if matches!(
                *state.borrow_and_update(),
                ConnectionState::LoginRequired(Error::LoginRequired)
            ) {
                return;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("login required after refresh failure");
    assert_eq!(gateway.fixture.connections.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.fixture.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn server_expiry_rejection_refreshes_before_reconnecting() {
    let Some(gateway) = Gateway::start(false).await else {
        return;
    };
    let client = gateway.client().await;
    connected(&client, 1).await;
    assert_eq!(
        client
            .call("expire", json!({}), Duration::from_secs(2))
            .await,
        Err(Error::Disconnected)
    );
    connected(&client, 2).await;
    assert_eq!(gateway.fixture.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        gateway.fixture.refreshed_connections.load(Ordering::SeqCst),
        1
    );
}
