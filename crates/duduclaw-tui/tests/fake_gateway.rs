//! Socket tests deliberately fail on bind refusal unless explicitly opted out.
#[path = "decisions/mod.rs"]
mod decisions;
#[path = "m2/mod.rs"]
mod m2;
#[path = "chat/mod.rs"]
mod chat;
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
    chat_connections: AtomicUsize,
    chat_active: AtomicUsize,
    chat_max_active: AtomicUsize,
    chat_refreshed: AtomicUsize,
    chat_sent: std::sync::Mutex<Vec<Value>>,
    refreshed_connections: AtomicUsize,
    refreshes: AtomicUsize,
    fail_refresh: AtomicBool,
    fail_version: AtomicBool,
    list_calls: AtomicUsize,
    list_delay_ms: AtomicU64,
    short_token: bool,
    flood: AtomicUsize,
    task_calls: AtomicUsize,
    decisions: std::sync::Mutex<Vec<(String, Value)>>,
    digest_mismatch: AtomicBool,
    employee_role: AtomicBool,
}
impl Fixture {
    fn role(&self) -> &'static str { if self.employee_role.load(Ordering::SeqCst) { "employee" } else { "manager" } }
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
            chat_connections: AtomicUsize::new(0),
            chat_active: AtomicUsize::new(0),
            chat_max_active: AtomicUsize::new(0),
            chat_refreshed: AtomicUsize::new(0),
            chat_sent: std::sync::Mutex::new(Vec::new()),
            refreshed_connections: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            fail_refresh: AtomicBool::new(false),
            fail_version: AtomicBool::new(false),
            list_calls: AtomicUsize::new(0),
            list_delay_ms: AtomicU64::new(0),
            short_token,
            flood: AtomicUsize::new(0),
            task_calls: AtomicUsize::new(0),
            decisions: std::sync::Mutex::new(Vec::new()),
            digest_mismatch: AtomicBool::new(false),
            employee_role: AtomicBool::new(false),
        });
        let app = Router::new()
            .route("/api/session/local", post(local_session))
            .route("/api/refresh", post(refresh))
            .route("/ws", get(upgrade))
            .route("/ws/chat", get(chat::upgrade))
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
        "user":{"id":"u1","email":"fixture@example.com","role":f.role(),"must_change_password":false}}),
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
    if !send(&mut socket,WsFrame::ok_response(&id,json!({"status":"authenticated","user":{"id":"u1","email":"fixture@example.com","role":f.role()},"must_change_password":false,"boot_id":"boot-a"}))).await { return; }
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
                            "system.version" => {
                                if f.fail_version.load(Ordering::SeqCst) {
                                    let response = WsFrame::Response { id, ok:false, payload:None, error:Some(json!({"code":"unauthorized"})) };
                                    if !send(&mut socket, response).await { return; }
                                    continue;
                                }
                                json!({"version":"fixture","edition":"community","auto_update":false})
                            },
                            "users.me" => json!({"user":{"id":"u1","role":f.role()},"bindings":[{"agent_name":"adu","access_level":"operator"}]}),
                            "approvals.decide" | "tasks.goal_decide" => {
                                f.decisions.lock().unwrap().push((method.clone(), params.clone()));
                                if method == "tasks.goal_decide" && params.get("expected_updated_at").is_some_and(|v| v != "revision") {
                                    let response = WsFrame::Response { id, ok:false, payload:None,
                                        error:Some(json!({"message":"goal_decide_revision_mismatch"})) };
                                    if !send(&mut socket,response).await { return; }
                                    continue;
                                }
                                if f.digest_mismatch.load(Ordering::SeqCst) {
                                    let response = WsFrame::Response { id, ok:false, payload:None,
                                        error:Some(json!({"code":"digest_mismatch","message":"changed\u{1b}]52;bad\u{7}"})) };
                                    if !send(&mut socket,response).await { return; }
                                    continue;
                                }
                                if method == "approvals.decide" {
                                    json!({"id":params["id"],"decided":if params["approve"] == true {"approved"} else {"denied"},"side_effect":null,"capability_handle":null})
                                } else { json!({"ok":true,"message":"done","task":{"id":params["task_id"],"status":"pending"}}) }
                            },
                            "tasks.steer" | "tasks.stop" | "agents.pause" | "agents.resume"
                            | "heartbeat.trigger" | "responsibilities.pause" | "responsibilities.disable" => {
                                f.decisions.lock().unwrap().push((method.clone(), params.clone()));
                                if method == "tasks.stop" && params["expected_authority_revision"] != 7 {
                                    if !send(&mut socket, WsFrame::error_response(&id, "task_changed")).await { return; }
                                    continue;
                                }
                                match method.as_str() {
                                    "tasks.steer" => json!({"steering":{"task_id":params["task_id"],"state":"pending"},"duplicate":false}),
                                    "tasks.stop" => json!({"stop":{"state":"cancel_pending","affected_task_ids":[params["task_id"]]}}),
                                    "responsibilities.pause" | "responsibilities.disable" if params["expected_control_epoch"] != 2 => json!({"conflict":true,"current":{"control_epoch":3}}),
                                    "responsibilities.pause" | "responsibilities.disable" => json!({"responsibility":{"responsibility_id":params["responsibility_id"],"state":"paused","control_epoch":3}}),
                                    _ => json!({"success":true}),
                                }
                            },
                            "heartbeat.status" => json!({"heartbeats":[{"agent_id":"adu","enabled":true,"cron":"0 * * * *","cron_timezone":"UTC","last_run":"09:00","next_run":"10:00","total_runs":1,"active_runs":0,"max_concurrent":1}]}),
                            "agents.list" => {
                                if f.flood.load(Ordering::SeqCst) == 0 {
                                    json!({"agents":[{"name":"adu","display_name":"阿嘟\u{202e}\u{1b}","status":"active","archived":false}]})
                                } else {
                                    json!({"agents":(0..30).map(|i| json!({"name":format!("agent-{i}"),"display_name":"flood","status":"active","archived":false})).collect::<Vec<_>>()})
                                }
                            },
                            "approvals.list" => {
                                f.list_calls.fetch_add(1,Ordering::SeqCst);
                                tokio::time::sleep(Duration::from_millis(f.list_delay_ms.load(Ordering::SeqCst))).await;
                                json!({"approvals":[{"id":"a1","digest":"shown-digest","agent_id":"adu","kind":"tool","summary":"需要審核\u{1b}[2K\u{202e}","status":"pending","request_kind":"question","expires_at":null,"decided_in_dashboard_only":true}],"count":1})
                            }
                            "tasks.list_page" => {
                                f.task_calls.fetch_add(1, Ordering::SeqCst);
                                assert_eq!(params["status"],"needs_human");
                                let flood = f.flood.load(Ordering::SeqCst);
                                if flood == 0 {
                                    assert_eq!(params["agent_id"],"adu","non-admin requires agent scope");
                                    json!({"tasks":[{"id":"t1","updated_at":"revision","title":"請確認","assigned_to":"adu","status":"needs_human","pause_reason":"unknown","goal_mode":true}],"total":1,"limit":200,"offset":0})
                                } else {
                                    let agent = params["agent_id"].as_str().expect("agent scope");
                                    let count = if flood == 1 { 100 } else { 1 };
                                    let title = if flood == 1 { "small".into() } else { "x".repeat(210_000) };
                                    json!({"tasks":(0..count).map(|i| json!({"id":format!("{agent}-{i}"), "title":title, "assigned_to":agent,"status":"needs_human"})).collect::<Vec<_>>(),"total":count})
                                }
                            }
                            "runs.subscribe" => {
                                if !send(&mut socket, WsFrame::error_response(&id, "Unknown method: runs.subscribe")).await { return; }
                                continue;
                            }
                            "runs.get" => json!({"run":{"id":"r1","agent_id":"adu"},"events":[{"kind":"text","label":"assistant","ts":"09:00","preview":"done"}]}),
                            "runs.list" => {
                                assert_eq!(params["agent_id"], "adu");
                                json!({"runs":[{"id":"r1","agent_id":"adu","status":"completed"}]})
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
async fn version_failure_keeps_connection_and_other_rpcs_working() {
    let Some(gateway) = Gateway::start(false).await else { return; };
    gateway.fixture.fail_version.store(true, Ordering::SeqCst);
    let client = gateway.client().await;
    connected(&client, 1).await;
    assert!(client.call("agents.list", json!({}), Duration::from_secs(2)).await.is_ok());
    assert!(matches!(&*client.connection().borrow(), ConnectionState::Connected(info) if info.version.is_none()));
    assert_eq!(gateway.fixture.refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn handshake_correlation_timeout_and_rejected_rpc() {
    let Some(gateway) = Gateway::start(false).await else {
        return;
    };
    let client = gateway.client().await;
    connected(&client, 1).await;
    let mut connection = client.connection();
    timeout(Duration::from_secs(2), async {
        loop {
            if matches!(&*connection.borrow_and_update(), ConnectionState::Connected(info) if info.version.as_ref().is_some_and(|v| v.is("fixture"))) { break; }
            connection.changed().await.unwrap();
        }
    }).await.expect("system.version must populate connection metadata");
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

#[tokio::test]
async fn json_inbox_entry_point_against_gateway() {
    let Some(gateway) = Gateway::start(false).await else { return; };
    let client = gateway.client().await;
    let home = tempfile::tempdir().unwrap();
    let mut output = Vec::new();
    duduclaw_tui::inbox::run_inbox_to(duduclaw_tui::InboxOptions {
        client, home: Some(home.path().into()), follow: false, json: true,
    }, &mut output).await.unwrap();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.lines().count(), 1);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["type"], "inbox");
    assert_eq!(value["stale"], false);
    assert_eq!(value["count"], 2);
    assert_eq!(value["items"][0]["data"]["summary"], "需要審核\u{1b}[2K\u{202e}");
    assert_eq!(value["items"][0]["source"], "員工提問");
    assert_eq!(value["items"][1]["data"]["id"], "t1");
}

#[tokio::test]
async fn json_follow_reports_loss_and_recovery_without_duplicate_poll_output() {
    use std::io::Write;
    struct Lines(tokio::sync::mpsc::UnboundedSender<Vec<u8>>);
    impl Write for Lines {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.0.send(b.to_vec()).unwrap(); Ok(b.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let Some(gateway) = Gateway::start(false).await else { return; };
    let client = gateway.client().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("tui.toml"), "poll_interval_seconds = 5").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let options = duduclaw_tui::InboxOptions { client, home: Some(home.path().into()), follow:true, json:true };
    let task = tokio::spawn(async move { duduclaw_tui::inbox::run_inbox_to(options, &mut Lines(tx)).await });
    let first = timeout(Duration::from_secs(6),rx.recv()).await.unwrap().unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap()["stale"],false);
    gateway.fixture.list_delay_ms.store(120,Ordering::SeqCst);
    gateway.push("events_lost",1,"boot-a");
    let stale = timeout(Duration::from_secs(6),rx.recv()).await.unwrap().unwrap();
    let value:Value=serde_json::from_slice(&stale).unwrap();
    assert_eq!(value["stale"],true);assert!(value["count"].is_null());assert_eq!(value["items"].as_array().unwrap().len(),2);
    let recovered = timeout(Duration::from_secs(6),rx.recv()).await.unwrap().unwrap();assert_eq!(recovered,first);
    assert!(timeout(Duration::from_millis(5200),rx.recv()).await.is_err());
    task.abort(); let _=task.await;
}

#[tokio::test]
async fn cross_agent_item_and_byte_floods_abort_without_publishing_partial_lists() {
    for flood in [1, 2] {
        let Some(gateway) = Gateway::start(false).await else { return; };
        let client = gateway.client().await;
        let mut feed = StateFeed::start(client, Duration::from_secs(60));
        model_until(&mut feed.receiver, |m| m.inbox.count() == Some(2)).await;
        gateway.fixture.flood.store(flood, Ordering::SeqCst);
        let before = gateway.fixture.task_calls.load(Ordering::SeqCst);
        gateway.push("events_lost", 1, "boot-a");
        model_until(&mut feed.receiver, |m| m.inbox.error == Some(Error::SnapshotBudget)).await;
        {
            let model = feed.receiver.borrow();
            assert!(model.inbox.stale && model.employees.stale);
            assert_eq!(model.inbox.count(), None);
            assert_eq!(model.employees.count(), None);
            assert_eq!(model.inbox.items.len(), 2, "keep the last complete list");
            assert_eq!(model.employees.items.len(), 1);
            assert_eq!(model.employees.items[0].id(), "adu");
        }
        let stopped = gateway.fixture.task_calls.load(Ordering::SeqCst);
        assert!(stopped - before < 30, "refetch must stop before fetching every agent");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(gateway.fixture.task_calls.load(Ordering::SeqCst), stopped, "no hot retry");
    }
}

#[tokio::test]
async fn event_flood_is_bounded_and_oversized_frame_reconnects() {
    let Some(gateway) = Gateway::start(false).await else { return; };
    let client = gateway.client().await;
    connected(&client, 1).await;
    let mut events = client.events();
    for seq in 1..=48 { gateway.push("future.event", seq, "boot-a"); }
    timeout(Duration::from_secs(2), async {
        while events.len() <= duduclaw_tui::client::EVENT_QUEUE_CAPACITY {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert!(matches!(events.recv().await, Err(broadcast::error::RecvError::Lagged(_))));
    gateway.fixture.events.send(Control::Push(json!({"type":"event", "event":"future.event", "payload":{"blob":"x".repeat(duduclaw_tui::client::MAX_FRAME_BYTES + 1)}}))).unwrap();
    connected(&client, 2).await;
}
