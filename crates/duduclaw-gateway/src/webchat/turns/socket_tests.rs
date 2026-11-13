//! Real WebSocket reader/writer coverage; only model execution is replaced.
use super::*;
use crate::agent_channels::e2e_seam::{WaitingTurn, WaitingTurns, arm_waiting};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message as WireMessage};

trait TestIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> TestIo for T {}
type Socket = WebSocketStream<Box<dyn TestIo>>;
// Bounds a hang; it is not a latency assertion because loaded Windows runners run these tests in separate processes.
const DEADLINE: Duration = Duration::from_secs(30);
static SOCKET_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

enum Transport {
    Tcp(SocketAddr),
    Memory(mpsc::UnboundedSender<tokio::io::DuplexStream>),
}

/// Axum's regular HTTP upgrade server, fed only in-memory byte streams.
/// The synthetic address is metadata for ConnectInfo; no socket is bound.
struct MemoryListener(mpsc::UnboundedReceiver<tokio::io::DuplexStream>);

impl axum::serve::Listener for MemoryListener {
    type Io = tokio::io::DuplexStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(stream) => (stream, SocketAddr::from(([127, 0, 0, 1], 12345))),
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(SocketAddr::from(([127, 0, 0, 1], 12345)))
    }
}

struct Fixture {
    state: Arc<WebChatState>,
    url: String,
    transport: Transport,
    server: tokio::task::JoinHandle<()>,
    gate: WaitingTurns,
    _home: tempfile::TempDir,
}

impl Fixture {
    async fn start(in_memory: bool) -> Option<Self> {
        // A sandbox-refused bind is explicitly reported as not run.
        let listener = if in_memory {
            None
        } else {
            match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                Ok(listener) => Some(listener),
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("NOT RUN: WebChat TCP integration: loopback bind denied by sandbox: {error}");
                    return None;
                }
                Err(error) => panic!("WebChat TCP integration: loopback bind failed: {error}"),
            }
        };
        let home = tempfile::tempdir().unwrap();
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
        crate::trigger_route::tests::write_agent(
            &home.path().join("agents"),
            "dudu",
            "Main",
            "@dudu",
            "main",
            "",
        );
        let ctx = crate::agent_channels::e2e_tests::reply_ctx(home.path()).await;
        let state = Arc::new(WebChatState::new(
            ctx,
            Arc::new(JwtConfig::new(b"webchat-socket-integration-test-key")),
            Arc::new(UserDb::new(&home.path().join("users.db")).unwrap()),
        ));
        // Match gateway boot before exercising authenticated socket traffic.
        std::fs::write(home.path().join("config.toml"), "").unwrap();
        crate::mfa::reload_policy(home.path());
        assert!(!crate::mfa::enforced(home.path(), false).unwrap());
        let app = axum::Router::new()
            .route("/ws/chat", axum::routing::get(ws_chat_handler))
            .with_state(state.clone());
        let (url, transport, server) = match listener {
            Some(listener) => {
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                        .await.unwrap();
                });
                (format!("ws://{address}/ws/chat"), Transport::Tcp(address), server)
            }
            None => {
                let (tx, rx) = mpsc::unbounded_channel();
                let app = app.layer(axum::Extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345)))));
                let server = tokio::spawn(async move {
                    axum::serve(MemoryListener(rx), app).await.unwrap();
                });
                ("ws://memory.invalid/ws/chat".into(), Transport::Memory(tx), server)
            }
        };
        let gate = arm_waiting(home.path());
        Some(Self {
            state,
            url,
            transport,
            server,
            gate,
            _home: home,
        })
    }

    fn user(&self, name: &str) -> String {
        let user = self
            .state
            .user_db
            .create_user(
                &format!("{name}@socket.test"),
                name,
                "Socket-Test-Password-12345",
                duduclaw_auth::UserRole::Admin,
            )
            .unwrap();
        self.state
            .jwt_config
            .issue_access_token(&user, &[])
            .unwrap()
    }

    async fn connect(&self, token: &str) -> (Socket, Value) {
        let mut socket =
            tokio::time::timeout(DEADLINE, self.open())
                .await
                .expect("WebChat upgrade timed out")
                .expect("WebChat upgrade failed");
        send(&mut socket, json!({"type":"auth", "token":token})).await;
        let first = frame(&mut socket).await;
        (socket, first)
    }

    async fn open(&self) -> Result<Socket, Box<tokio_tungstenite::tungstenite::Error>> {
        let stream: Box<dyn TestIo> = match &self.transport {
            Transport::Tcp(address) => Box::new(tokio::net::TcpStream::connect(address).await
                .map_err(|error| Box::new(tokio_tungstenite::tungstenite::Error::Io(error)))?),
            Transport::Memory(accepted) => {
                let (client, server) = tokio::io::duplex(64 * 1024);
                accepted.send(server).expect("in-memory HTTP server stopped");
                Box::new(client)
            }
        };
        tokio_tungstenite::client_async(&self.url, stream).await.map(|(socket, _)| socket).map_err(Box::new)
    }

    async fn entered(&mut self) -> WaitingTurn {
        tokio::time::timeout(DEADLINE, self.gate.entered.recv())
            .await
            .expect("expected model turn never started")
            .expect("model seam closed")
    }

    async fn turn_count(&self, expected: usize) {
        tokio::time::timeout(DEADLINE, async {
            while self.state.turns.entries.lock().unwrap().len() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("turn admission/completion did not settle");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(WireMessage::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn frame(socket: &mut Socket) -> Value {
    tokio::time::timeout(DEADLINE, async {
        loop {
            match socket
                .next()
                .await
                .expect("WebChat closed before its reply")
                .unwrap()
            {
                WireMessage::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["type"] == "turn_accepted" { continue; }
                    return value;
                },
                WireMessage::Ping(data) => socket.send(WireMessage::Pong(data)).await.unwrap(),
                WireMessage::Pong(_) => {}
                other => panic!("unexpected WebChat frame: {other:?}"),
            }
        }
    })
    .await
    .expect("WebChat reply timed out")
}

async fn message(socket: &mut Socket, conv: &str, msg: &str) {
    send(
        socket,
        json!({"type":"user_message", "content":"Hello", "conv":conv, "msg_id":msg}),
    )
    .await;
}

async fn done(socket: &mut Socket, conv: &str, msg: &str, sid: &str) {
    let result = frame(socket).await;
    assert_eq!(result["type"], "assistant_done", "{result}");
    assert_eq!(result["conv"], conv);
    assert_eq!(result["msg_id"], msg);
    assert_eq!(result["session_id"], sid);
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("OWN_BOT_REPLY")
    );
}

#[tokio::test]
async fn webchat_real_socket_concurrency_fifo_user_limits_and_foreign_cancel() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let Some(fixture) = Fixture::start(false).await else { return; };
    exercise(fixture).await;
}

#[tokio::test]
async fn webchat_memory_socket_concurrency_fifo_user_limits_and_global_connection_limit_stays_ten() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    exercise(Fixture::start(true).await.unwrap()).await;
}

async fn exercise(mut fixture: Fixture) {
    let alice = fixture.user("alice");
    let bob = fixture.user("bob");
    let (mut first, info) = fixture.connect(&alice).await;
    assert_eq!(info["type"], "session_info");
    let base = info["session_id"].as_str().unwrap();
    let sid_a = format!("{base}#conv:a");
    let sid_b = format!("{base}#conv:b");
    let (mut second, info) = fixture.connect(&alice).await;
    assert_eq!(info["type"], "session_info");
    let sid_c = format!("{}#conv:c", info["session_id"].as_str().unwrap());
    let (mut foreign, info) = fixture.connect(&bob).await;
    assert_eq!(info["type"], "session_info");

    message(&mut first, "a", "a1").await;
    let a1 = fixture.entered().await;
    assert_eq!(a1.session_id, sid_a);
    message(&mut first, "a", "a2").await;
    message(&mut first, "b", "b1").await;
    // B must start while A1 is still held; A2 must remain behind A1.
    let b1 = fixture.entered().await;
    assert_eq!(b1.session_id, sid_b);
    fixture.turn_count(3).await;
    assert!(fixture.gate.entered.try_recv().is_err());

    // The second socket shares Alice's three accepted-turn reservations.
    message(&mut second, "c", "c1").await;
    let limited = frame(&mut second).await;
    assert_eq!(limited["type"], "error");
    assert_eq!(limited["code"], "turn_limit");
    assert_eq!(limited["request_id"], "c1");
    assert_eq!(limited["msg_id"], "c1");
    assert_eq!(limited["conv"], "c");
    assert_eq!(limited["session_id"], sid_c);
    send(
        &mut foreign,
        json!({"type":"cancel", "session_id":sid_a, "msg_id":"a1"}),
    )
    .await;
    let denied = frame(&mut foreign).await;
    assert_eq!(denied["code"], "turn_not_found");
    assert!(
        fixture
            .state
            .turns
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|entry| !entry.cancel.is_cancelled())
    );

    a1.release.send(()).unwrap();
    done(&mut first, "a", "a1", &sid_a).await;
    let a2 = fixture.entered().await;
    assert_eq!(a2.session_id, sid_a);
    a2.release.send(()).unwrap();
    done(&mut first, "a", "a2", &sid_a).await;
    b1.release.send(()).unwrap();
    done(&mut first, "b", "b1", &sid_b).await;
    fixture.turn_count(0).await;

    // The real reader accepts cancel while its model future is still held.
    message(&mut first, "cancelme", "cancelme1").await;
    let cancelled = fixture.entered().await;
    send(&mut first, json!({"type":"cancel", "conv":"cancelme", "request_id":"self-cancel"})).await;
    let frames = [frame(&mut first).await, frame(&mut first).await];
    let accepted = frames.iter().find(|frame| frame["type"] == "cancel_accepted").unwrap();
    let reply = frames.iter().find(|frame| frame["code"] == "cancelled").unwrap();
    assert_eq!(accepted["request_id"], "self-cancel");
    assert_eq!(reply["request_id"], "self-cancel");
    assert_eq!(reply["msg_id"], "cancelme1");
    assert!(
        cancelled.release.send(()).is_err(),
        "cancel must drop the model future"
    );
    fixture.turn_count(0).await;
    let history = fixture
        .state
        .ctx
        .session_manager
        .get_messages(&cancelled.session_id)
        .await
        .unwrap();
    assert!(
        history
            .iter()
            .all(|message| !message.content.contains("cancelled"))
    );
    assert!(fixture.state.ctx.session_manager.session_agent(&cancelled.session_id).await.unwrap().is_none(),
        "cancelling a turn before session creation must not create a marker-only session");

    // A different socket receives acknowledgement; the original receives the outcome.
    message(&mut first, "a", "cross-cancel").await;
    let cross_cancelled = fixture.entered().await;
    send(&mut second, json!({"type":"cancel", "session_id":sid_a,
        "msg_id":"cross-cancel", "request_id":"cross-request"})).await;
    let accepted = frame(&mut second).await;
    assert_eq!(accepted["type"], "cancel_accepted");
    assert_eq!(accepted["request_id"], "cross-request");
    let outcome = frame(&mut first).await;
    assert_eq!(outcome["code"], "cancelled");
    assert_eq!(outcome["msg_id"], "cross-cancel");
    assert_eq!(outcome["request_id"], "cross-request");
    assert!(cross_cancelled.release.send(()).is_err());
    fixture.turn_count(0).await;
    let history = fixture.state.ctx.session_manager.get_messages(&sid_a).await.unwrap();
    assert!(history.iter().all(|message| !message.content.contains("cancelled")));
    let marker_count: i64 = rusqlite::Connection::open(fixture._home.path().join("sessions.db")).unwrap()
        .query_row("SELECT count(*) FROM webchat_turn_cancellations WHERE session_id=?1 AND msg_id='cross-cancel'",
            [&sid_a], |row| row.get(0)).unwrap();
    assert_eq!(marker_count, 1);

    // A socket closing never cancels an already accepted turn.
    message(&mut second, "detached", "detached1").await;
    let detached = fixture.entered().await;
    second.close(None).await.unwrap();
    drop(second);
    detached.release.send(()).unwrap();
    fixture.turn_count(0).await;
    assert!(
        fixture
            .state
            .ctx
            .session_manager
            .session_agent(&detached.session_id)
            .await
            .unwrap()
            .is_some()
    );
    let (second, info) = fixture.connect(&alice).await;
    assert_eq!(info["type"], "session_info");

    // All clients use one peer IP. Authentication permits independent users,
    // but Alice's fourth connection is refused at the per-user cap of three.
    let (third, info) = fixture.connect(&alice).await;
    assert_eq!(info["type"], "session_info");
    let (rejected, error) = fixture.connect(&alice).await;
    assert_eq!(error["code"], "connection_limit");
    drop(rejected);
    let mut sockets = vec![first, second, foreign, third];
    for token in [&bob, &bob] {
        let (socket, info) = fixture.connect(token).await;
        assert_eq!(info["type"], "session_info");
        sockets.push(socket);
    }
    let charlie = fixture.user("charlie");
    for _ in 0..3 {
        let (socket, info) = fixture.connect(&charlie).await;
        assert_eq!(info["type"], "session_info");
        sockets.push(socket);
    }
    let dana = fixture.user("dana");
    let (socket, info) = fixture.connect(&dana).await;
    assert_eq!(info["type"], "session_info");
    sockets.push(socket);
    assert_eq!(sockets.len(), MAX_TOTAL_WEBCHAT_CONNECTIONS);
    assert_eq!(WEBCHAT_SEMAPHORE.available_permits(), 0);
    // The eleventh upgrade cannot produce an authenticated session.
    let rejected = tokio::time::timeout(DEADLINE, async {
        match fixture.open().await {
            Err(_) => true,
            Ok(mut socket) => matches!(
                socket.next().await,
                None | Some(Err(_)) | Some(Ok(WireMessage::Close(_)))
            ),
        }
    })
    .await
    .expect("global connection refusal timed out");
    assert!(rejected, "eleventh connection must be refused");
    for mut socket in sockets {
        let _ = socket.close(None).await;
    }
    tokio::time::timeout(DEADLINE, async {
        while !fixture.state.connections.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("socket connection slots were not released");
}

#[tokio::test]
async fn webchat_widget_ip_limit_and_reconnect_global_turn_cap() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    std::fs::write(fixture._home.path().join("config.toml"),
        "[webchat]\npublic_widget = true\nwidget_key = \"0123456789abcdef\"\nmax_global_turns = 2\n").unwrap();
    let token = "widget:0123456789abcdef";
    let mut sockets = Vec::new();
    for _ in 0..3 {
        let (socket, info) = fixture.connect(token).await;
        assert_eq!(info["type"], "session_info", "{info}");
        sockets.push(socket);
    }
    assert_eq!(fixture.state.connections.lock().await.get("webchat:127.0.0.1"), Some(&3));
    // The fourth widget socket is rejected before auth, despite a fresh identity.
    let refused = tokio::time::timeout(DEADLINE, async {
        match fixture.open().await {
            Err(_) => true,
            Ok(mut socket) => matches!(socket.next().await,
                None | Some(Err(_)) | Some(Ok(WireMessage::Close(_)))),
        }
    }).await.expect("widget IP refusal timed out");
    assert!(refused, "widget connections must keep the IP reservation after auth");
    for mut socket in sockets {
        let _ = socket.close(None).await;
    }
    wait_connections_closed(&fixture).await;

    let mut held_turns = Vec::new();
    for index in 0..2 {
        let (mut socket, info) = fixture.connect(token).await;
        assert_eq!(info["type"], "session_info");
        message(&mut socket, &format!("visitor-{index}"), &format!("message-{index}")).await;
        held_turns.push(fixture.entered().await);
        socket.close(None).await.unwrap();
        drop(socket);
        wait_connections_closed(&fixture).await;
        fixture.turn_count(index + 1).await;
    }
    assert_ne!(held_turns[0].session_id, held_turns[1].session_id);
    let (mut socket, info) = fixture.connect(token).await;
    assert_eq!(info["type"], "session_info");
    message(&mut socket, "visitor-overflow", "overflow").await;
    let refused = frame(&mut socket).await;
    assert_eq!(refused["code"], "turn_limit");
    assert_eq!(refused["request_id"], "overflow");
    fixture.turn_count(2).await;
    assert!(fixture.gate.entered.try_recv().is_err(), "overflow must not start model work");
    // Each earlier visitor still owns its turn after losing its connection.
    for held in held_turns {
        held.release.send(()).unwrap();
    }
    fixture.turn_count(0).await;
    socket.close(None).await.unwrap();
    drop(socket);
    wait_connections_closed(&fixture).await;
}

async fn wait_connections_closed(fixture: &Fixture) {
    tokio::time::timeout(DEADLINE, async {
        while !fixture.state.connections.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
    }).await.expect("socket connection slots were not released");
}

#[tokio::test]
async fn webchat_queued_assign_arms_at_arrival_without_rearming_after_fifo_wait() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("assigner");
    let (mut socket, info) = fixture.connect(&token).await;
    assert_eq!(info["type"], "session_info");
    let sid = format!("{}#conv:assign", info["session_id"].as_str().unwrap());
    message(&mut socket, "assign", "first").await;
    let first = fixture.entered().await;
    assert_eq!(first.session_id, sid);

    // No client msg_id: admission and deferred command execution must still
    // share the same host event id, so the consumed assignment is not rearmed.
    send(&mut socket, json!({"type":"user_message", "content":"/交辦 @dudu", "conv":"assign"})).await;
    fixture.turn_count(2).await;
    let arm_msg = fixture.state.turns.entries.lock().unwrap().values()
        .find(|entry| entry.sid == sid && entry.msg_id != "first")
        .expect("queued assignment command").msg_id.clone();
    send(&mut socket, json!({"type":"cancel", "session_id":sid,
        "msg_id":arm_msg, "request_id":"cancel-armed"})).await;
    let refused = frame(&mut socket).await;
    assert_eq!(refused["code"], "turn_not_found", "{refused}");
    assert_eq!(refused["request_id"], "cancel-armed");

    send(&mut socket, json!({"type":"user_message", "content":"Prepare the inventory report",
        "conv":"assign", "msg_id":"assignment-body"})).await;
    let assignment = frame(&mut socket).await;
    assert_eq!(assignment["type"], "assistant_done", "{assignment}");
    assert_eq!(assignment["task"]["agent"], "dudu", "{assignment}");
    let task_id = assignment["task"]["id"].as_str().expect("created task id");
    let store = crate::task_store::TaskStore::open(fixture._home.path()).unwrap();
    let task = store.get_task(task_id).await.unwrap().expect("persisted assignment");
    assert_eq!(task.assigned_to, "dudu");
    assert_eq!(task.created_via.as_deref(), Some("webchat"));
    assert!(fixture.gate.entered.try_recv().is_err(), "assignment body must not start a model turn");
    fixture.turn_count(2).await;

    first.release.send(()).unwrap();
    done(&mut socket, "assign", "first", &sid).await;
    let arm_reply = frame(&mut socket).await;
    assert_eq!(arm_reply["type"], "assistant_done", "{arm_reply}");
    assert_eq!(arm_reply["msg_id"], arm_msg);
    assert!(arm_reply.get("task").is_none_or(Value::is_null));
    fixture.turn_count(0).await;

    // Executing the deferred command must not arm the next unrelated message.
    message(&mut socket, "assign", "after-assignment").await;
    let next = fixture.entered().await;
    assert_eq!(next.session_id, sid);
    next.release.send(()).unwrap();
    done(&mut socket, "assign", "after-assignment", &sid).await;
    fixture.turn_count(0).await;
    let task_count: i64 = rusqlite::Connection::open(fixture._home.path().join("tasks.db")).unwrap()
        .query_row("SELECT count(*) FROM tasks WHERE created_via='webchat'", [], |row| row.get(0)).unwrap();
    assert_eq!(task_count, 1, "the next message must not create another assignment");
    socket.close(None).await.unwrap();
    drop(socket);
    wait_connections_closed(&fixture).await;
}

#[tokio::test]
async fn webchat_memory_socket_mfa_revocation_blocks_fifo_waiter_and_turn_frames() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("revoked");
    let (mut socket, info) = fixture.connect(&token).await;
    assert_eq!(info["type"], "session_info");
    message(&mut socket, "same", "running").await;
    let running = fixture.entered().await;
    message(&mut socket, "same", "queued").await;
    fixture.turn_count(2).await;
    // turn_accepted acknowledges a persisted user row, not FIFO admission.
    // This fixture's model seam returns before that INSERT and is still held
    // before session creation; the queued turn has not entered the seam at all.
    // Waiting for either receipt here would retain SOCKET_TEST_LOCK forever.
    assert!(fixture.gate.entered.try_recv().is_err(), "FIFO waiter must not start before revocation");
    assert!(fixture.state.ctx.session_manager.session_agent(&running.session_id).await.unwrap().is_none());
    fixture.state.user_db.mfa_operator_reset("revoked@socket.test").unwrap();
    running.release.send(()).unwrap();
    fixture.turn_count(0).await;
    assert!(fixture.gate.entered.try_recv().is_err(), "revoked FIFO waiter must not start");
    // A new frame on the retired connection must close it without admitting work
    // or exposing any of the running turn's reply/progress frames.
    // The writer may already have closed the transport after observing the
    // retired credential. Either rejection here or a Close below is valid.
    let _ = socket.send(WireMessage::Text(json!({"type":"user_message", "content":"Hello",
        "conv":"other", "msg_id":"after-revocation"}).to_string().into())).await;
    tokio::time::timeout(DEADLINE, async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(WireMessage::Close(_))) => break,
                Some(Ok(WireMessage::Ping(data))) => { let _ = socket.send(WireMessage::Pong(data)).await; }
                Some(Ok(WireMessage::Pong(_))) => {}
                other => panic!("revoked socket received turn data: {other:?}"),
            }
        }
    }).await.expect("revoked connection must close");
    fixture.turn_count(0).await;
    assert!(fixture.gate.entered.try_recv().is_err());
}

#[tokio::test]
async fn webchat_memory_socket_human_turn_remains_available_during_autonomy_pause() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    duduclaw_core::autonomy_gate::set_pause(fixture._home.path(), None, "pause", "operator").unwrap();
    duduclaw_core::autonomy_gate::set_pause(fixture._home.path(), Some("dudu"), "pause", "operator").unwrap();
    let token = fixture.user("human");
    let (mut socket, info) = fixture.connect(&token).await;
    assert_eq!(info["type"], "session_info");
    message(&mut socket, "human", "paused-chat").await;
    let turn = fixture.entered().await;
    turn.release.send(()).unwrap();
    done(&mut socket, "human", "paused-chat", &turn.session_id).await;
    fixture.turn_count(0).await;
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn webchat_named_employee_ticket_and_delegation_share_exact_session_and_turn() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    crate::trigger_route::tests::write_agent(
        &fixture._home.path().join("agents"),
        "sales-rep", "Sales", "@sales-rep", "specialist", "",
    );
    fixture.state.ctx.registry.write().await.scan().await.unwrap();
    fixture.gate.capture_delegations();
    let token = fixture.user("lineage-owner");
    let (mut socket, info) = fixture.connect(&token).await;
    assert_eq!(info["type"], "session_info");
    let base = info["session_id"].as_str().unwrap().to_owned();
    let queue = crate::message_queue::MessageQueue::open(fixture._home.path()).unwrap();

    // Exercise both explicit selection (the TUI route) and the gateway's
    // @ route through real authentication, admission and the reply pipeline.
    for (conv, content, selected) in [
        ("selected", "Hello", Some("sales-rep")),
        ("mentioned", "@sales-rep Hello", None),
    ] {
        send(&mut socket, json!({"type":"user_message", "content":content,
            "agent":selected, "conv":conv, "msg_id":conv})).await;
        let running = fixture.entered().await;
        let expected_sid = format!("{base}#agent:sales-rep#conv:{conv}");
        let (ticket_id, ticket_sid, user) = {
            let entries = fixture.state.turns.entries.lock().unwrap();
            let (id, entry) = entries.iter().find(|(_, entry)| entry.msg_id == conv)
                .expect("the real socket admitted this turn");
            (id.clone(), entry.sid.clone(), entry.user.clone())
        };
        let delegation_id = running.delegation_id.as_deref().expect("captured delegation");
        let row = queue.get_by_id(delegation_id).await.unwrap().unwrap();
        assert_eq!(ticket_sid, expected_sid);
        assert_eq!(running.session_id, ticket_sid);
        assert_eq!(row.session_id.as_deref(), Some(ticket_sid.as_str()));
        assert_eq!(row.turn_id.as_deref(), Some(ticket_id.as_str()));
        assert_eq!(row.on_behalf_of, Some(format!("user:{user}")));
        assert_eq!(row.sender_agent.as_deref(), Some("sales-rep"));

        send(&mut socket, json!({"type":"cancel", "session_id":ticket_sid,
            "msg_id":conv, "request_id":format!("cancel-{conv}")})).await;
        let frames = [frame(&mut socket).await, frame(&mut socket).await];
        let cancelled = frames.iter().find(|value| value["code"] == "cancelled")
            .expect("the real cancellation pipeline completed");
        assert_eq!(cancelled["session_id"], expected_sid);
        assert_eq!(cancelled["delegations_cancelled"], 1);
        assert!(cancelled.get("downstream_running").is_none());
        assert!(cancelled.get("downstream_ref").is_none());
        let row = queue.get_by_id(delegation_id).await.unwrap().unwrap();
        assert_eq!(row.status, crate::message_queue::MessageStatus::Failed);
        assert_eq!(row.error.as_deref(), Some("origin turn cancelled"));
        assert!(crate::approval::is_turn_cancelled(
            fixture._home.path(), &expected_sid, &ticket_id,
        ).await.unwrap());
        assert!(running.release.send(()).is_err(), "cancel drops the held model turn");
        fixture.turn_count(0).await;
    }
    socket.close(None).await.unwrap();
    drop(socket);
    wait_connections_closed(&fixture).await;
}

#[tokio::test]
async fn webchat_downstream_running_offer_stops_real_dispatch_or_can_be_left_alone() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    crate::trigger_route::tests::write_agent(&fixture._home.path().join("agents"),
        "downstream-fixture", "Worker", "@downstream-fixture", "specialist", "");
    let worker_config = fixture._home.path().join("agents/downstream-fixture/agent.toml");
    let config = std::fs::read_to_string(&worker_config).unwrap().replace("reports_to = \"\"", "reports_to = \"dudu\"");
    std::fs::write(worker_config, config).unwrap();
    fixture.state.ctx.registry.write().await.scan().await.unwrap();
    fixture.gate.capture_delegations();
    let token = fixture.user("downstream-owner");
    let (mut socket, info) = fixture.connect(&token).await;
    assert_eq!(info["type"], "session_info");
    let home = fixture._home.path().to_owned();
    let queue = Arc::new(crate::message_queue::MessageQueue::open(&home).unwrap());
    let conn = rusqlite::Connection::open(home.join("message_queue.db")).unwrap();
    crate::model_call_probe::set_dry_run(true);
    for stop in [true, false] {
        let msg = if stop { "stop-child" } else { "leave-child" };
        message(&mut socket, msg, msg).await;
        let running = fixture.entered().await;
        let id = running.delegation_id.as_deref().unwrap().to_owned();
        let row = queue.get_by_id(&id).await.unwrap().unwrap();
        conn.execute("INSERT INTO delegation_callbacks(message_id,agent_id,channel_type,channel_id,created_at)
            VALUES (?1,'dudu','webchat',?2,datetime('now'))", rusqlite::params![id, running.session_id]).unwrap();
        let gate = crate::responsibility::test_hooks::install("downstream_runtime", &row.payload);
        let dispatch = crate::dispatcher::poll_and_dispatch_sqlite(
            &queue, &home, &fixture.state.ctx.registry, None, None);
        let action = async {
            gate.arrived.wait().await;
            assert_eq!(queue.get_by_id(&id).await.unwrap().unwrap().status, crate::message_queue::MessageStatus::Acked);
            send(&mut socket, json!({"type":"cancel", "conv":msg, "msg_id":msg})).await;
            let frames = [frame(&mut socket).await, frame(&mut socket).await];
            let cancelled = frames.iter().find(|f| f["code"] == "cancelled").expect("cancelled frame");
            assert_eq!(cancelled["downstream_running"], 1, "{cancelled}");
            let reference = cancelled["downstream_ref"].as_str().expect("opaque downstream reference");
            assert!(!reference.is_empty());
            if stop {
                send(&mut socket, json!({"type":"cancel_downstream", "downstream_ref":reference, "request_id":"stop-request"})).await;
                let outcome = frame(&mut socket).await;
                assert_eq!(outcome["type"], "downstream_stopped", "{outcome}");
                assert_eq!(outcome["stopped"], 1);
                assert_eq!(outcome["still_stopping"], 0);
                assert_eq!(outcome["request_id"], "stop-request");
                send(&mut socket, json!({"type":"cancel_downstream", "downstream_ref":reference})).await;
                assert_eq!(frame(&mut socket).await["code"], "downstream_ref_unknown");
            } else {
                gate.release.wait().await;
            }
        };
        let (result, ()) = tokio::join!(dispatch, action);
        result.unwrap();
        assert!(running.release.send(()).is_err(), "origin model future was cancelled");
        let row = queue.get_by_id(&id).await.unwrap().unwrap();
        if stop {
            assert_eq!(row.status, crate::message_queue::MessageStatus::Failed);
            assert_eq!(row.error.as_deref(), Some("stopped with origin turn"));
            assert!(row.response.is_none());
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM delegation_callbacks WHERE message_id=?1", [&id],
                |r| r.get::<_, i64>(0)).unwrap(), 1, "stopped callback must remain unconsumed");
        } else {
            assert_eq!(row.status, crate::message_queue::MessageStatus::Done);
            assert_eq!(row.response.as_deref(), Some("model-call-probe dry run"));
        }
        fixture.turn_count(0).await;
    }
    crate::model_call_probe::set_dry_run(false);
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn webchat_downstream_memory_socket_rejects_invalid_and_concurrent_stop_frames() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("stop-admission");
    let user = fixture.state.jwt_config.verify_access_token(&token).unwrap().sub;
    let principal = format!("user:{user}");
    let reference = fixture.state.turns.offer(&user, &principal, "s", "t").unwrap();
    // Holding the registered run keeps the first request in its cleanup wait.
    let guard = crate::downstream_runs::register(fixture._home.path(), Some("s"), Some("t"), Some(&principal)).unwrap().unwrap();
    let (mut socket, _) = fixture.connect(&token).await;
    send(&mut socket, json!({"type":"cancel_downstream","downstream_ref":"invalid","request_id":"invalid"})).await;
    assert_eq!(frame(&mut socket).await["code"], "downstream_ref_unknown");
    send(&mut socket, json!({"type":"cancel_downstream","downstream_ref":reference,"request_id":"first"})).await;
    send(&mut socket, json!({"type":"cancel_downstream","downstream_ref":reference,"request_id":"second"})).await;
    let busy = frame(&mut socket).await;
    assert_eq!(busy, json!({"type":"error","code":"downstream_stop_busy","retryable":true,"request_id":"second"}));
    send(&mut socket, json!({"type":"cancel_downstream","downstream_ref":"invalid","request_id":"invalid-again"})).await;
    assert_eq!(frame(&mut socket).await["code"], "downstream_ref_unknown");
    drop(guard);
    let stopped = frame(&mut socket).await;
    assert_eq!(stopped["type"], "downstream_stopped");
    assert_eq!(stopped["request_id"], "first");
    assert_eq!(stopped["stopped"], 1);
    send(&mut socket, json!({"type":"cancel_downstream","downstream_ref":reference,"request_id":"after"})).await;
    assert_eq!(frame(&mut socket).await["code"], "downstream_ref_unknown");
    socket.close(None).await.unwrap();
}

impl Fixture {
    async fn employee(&self, name: &str, agents: &[&str], primary: Option<&str>) -> (String, String) {
        let user = self.state.user_db.create_user(
            &format!("{name}@socket.test"), name, "Socket-Test-Password-12345",
            duduclaw_auth::UserRole::Employee,
        ).unwrap();
        for agent in agents {
            if *agent != "dudu" {
                crate::trigger_route::tests::write_agent(
                    &self._home.path().join("agents"), agent, agent, &format!("@{agent}"), "specialist", "",
                );
            }
            self.state.user_db.bind_agent(&user.id, agent, duduclaw_auth::AccessLevel::Owner).unwrap();
        }
        self.state.ctx.registry.write().await.scan().await.unwrap();
        self.state.user_db.set_primary_agent(&user.id, primary).unwrap();
        let token = self.state.jwt_config.issue_access_token(&user, &[]).unwrap();
        (user.id, token)
    }

    fn session_count(&self) -> i64 {
        rusqlite::Connection::open(self._home.path().join("sessions.db")).unwrap()
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0)).unwrap()
    }
}

#[tokio::test]
async fn webchat_non_admin_primary_and_sole_binding_route_without_main() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    for primary in [None, Some("finance")] {
        let mut fixture = Fixture::start(true).await.unwrap();
        let bindings: &[&str] = if primary.is_some() { &["finance", "sales"] } else { &["finance"] };
        let (_, token) = fixture.employee("assigned", bindings, primary).await;
        let (mut socket, info) = fixture.connect(&token).await;
        assert_eq!(info["resolved_agent"]["id"], "finance");
        assert_eq!(fixture.session_count(), 0, "hello must not create a session");
        message(&mut socket, "assigned", "first").await;
        let metadata = frame(&mut socket).await;
        assert_eq!(metadata["type"], "session_info");
        assert_eq!(metadata["refresh"], true, "metadata must not turn a new conversation into resume");
        assert_eq!(metadata["conv"], "assigned");
        let first = fixture.entered().await;
        let expected = format!("{}#agent:finance#conv:assigned", info["session_id"].as_str().unwrap());
        assert_eq!(first.session_id, expected);
        // A second message arrives before the first turn creates its session.
        message(&mut socket, "assigned", "second").await;
        assert_eq!(frame(&mut socket).await["resolved_agent"]["id"], "finance");
        fixture.turn_count(2).await;
        assert_eq!(fixture.session_count(), 0);
        first.release.send(()).unwrap();
        done(&mut socket, "assigned", "first", &expected).await;
        let second = fixture.entered().await;
        assert_eq!(second.session_id, expected);
        second.release.send(()).unwrap();
        done(&mut socket, "assigned", "second", &expected).await;
        fixture.turn_count(0).await;
        assert_eq!(fixture.state.ctx.session_manager.session_agent(&expected).await.unwrap().as_deref(), Some("finance"));
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn webchat_no_assignment_and_ambiguous_assignment_create_no_turn_or_session() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    for (bindings, reason) in [(&[][..], "no_agents"), (&["finance", "sales"][..], "multiple_agents")] {
        let mut fixture = Fixture::start(true).await.unwrap();
        let (_, token) = fixture.employee("unassigned", bindings, None).await;
        let (mut socket, info) = fixture.connect(&token).await;
        assert_eq!(info["resolved_agent"], Value::Null);
        assert_eq!(info["assignment_error"]["reason"], reason);
        message(&mut socket, "unassigned", "refused").await;
        assert_eq!(frame(&mut socket).await["type"], "session_info");
        let refusal = frame(&mut socket).await;
        assert_eq!(refusal["code"], "no_assigned_agent");
        assert_eq!(refusal["reason"], reason);
        assert_eq!(fixture.session_count(), 0);
        fixture.turn_count(0).await;
        assert!(fixture.gate.entered.try_recv().is_err());
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn webchat_personal_non_admin_cannot_reach_main_default_explicit_or_resume() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    for mode in ["default", "explicit", "resume"] {
        let mut fixture = Fixture::start(true).await.unwrap();
        crate::team_edition::record(fixture._home.path(), duduclaw_core::EditionProfile::Personal);
        let (_, token) = fixture.employee("personal-main", &["dudu"], None).await;
        let (mut socket, info) = fixture.connect(&token).await;
        assert_eq!(info["resolved_agent"], Value::Null);
        assert_eq!(info["assignment_error"]["reason"], "no_agents");
        let sid = format!("{}#conv:personal", info["session_id"].as_str().unwrap());
        let mut request = json!({"type":"user_message", "content":"Hello", "conv":"personal", "msg_id":mode});
        if mode == "explicit" { request["agent"] = json!("dudu"); }
        if mode == "resume" {
            fixture.state.ctx.session_manager.get_or_create(&sid, "dudu").await.unwrap();
            request["session_id"] = json!(sid);
        }
        send(&mut socket, request).await;
        if mode == "default" {
            assert_eq!(frame(&mut socket).await["type"], "session_info");
            let refusal = frame(&mut socket).await;
            assert_eq!(refusal["code"], "no_assigned_agent");
            assert_eq!(refusal["reason"], "no_agents");
        } else {
            let refusal = frame(&mut socket).await;
            assert_eq!(refusal["code"], "agent_unavailable");
            assert_eq!(refusal["message"], crate::webchat::access::AGENT_UNAVAILABLE_TEXT);
        }
        assert_eq!(fixture.session_count(), if mode == "resume" { 1 } else { 0 });
        assert!(fixture.state.ctx.session_manager.get_messages(&sid).await.unwrap().is_empty());
        fixture.turn_count(0).await;
        assert!(fixture.gate.entered.try_recv().is_err());
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn webchat_bound_main_explicit_and_stored_resume_are_refused_without_rerouting() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let (_, token) = fixture.employee("main-bound", &["dudu", "finance"], Some("finance")).await;
    let (mut socket, info) = fixture.connect(&token).await;
    send(&mut socket, json!({"type":"user_message", "content":"Hello", "agent":"dudu", "msg_id":"explicit"})).await;
    assert_eq!(frame(&mut socket).await["code"], "agent_unavailable");
    assert_eq!(fixture.session_count(), 0);
    let old = format!("{}#conv:old", info["session_id"].as_str().unwrap());
    fixture.state.ctx.session_manager.get_or_create(&old, "dudu").await.unwrap();
    send(&mut socket, json!({"type":"user_message", "content":"Hello", "session_id":old, "msg_id":"resume"})).await;
    let refusal = frame(&mut socket).await;
    assert_eq!(refusal["code"], "agent_unavailable");
    assert_eq!(refusal["message"], crate::webchat::access::AGENT_UNAVAILABLE_TEXT);
    send(&mut socket, json!({"type":"user_message", "content":"Hello", "agent":"dudu",
        "session_id":old, "msg_id":"resume-explicit"})).await;
    let refusal = frame(&mut socket).await;
    assert_eq!(refusal["code"], "agent_unavailable");
    assert_eq!(refusal["message"], crate::webchat::access::AGENT_UNAVAILABLE_TEXT);
    assert_eq!(fixture.session_count(), 1);
    assert!(fixture.state.ctx.session_manager.get_messages(&old).await.unwrap().is_empty());
    fixture.turn_count(0).await;
    assert!(fixture.gate.entered.try_recv().is_err());
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn webchat_fifo_waiter_rechecks_revoked_non_admin_binding() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let (user, token) = fixture.employee("revoked", &["finance"], None).await;
    let (mut socket, _) = fixture.connect(&token).await;
    message(&mut socket, "fifo", "first").await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    let first = fixture.entered().await;
    message(&mut socket, "fifo", "second").await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    fixture.turn_count(2).await;
    fixture.state.user_db.unbind_agent(&user, "finance").unwrap();
    let sid = first.session_id.clone();
    first.release.send(()).unwrap();
    let denied = frame(&mut socket).await;
    assert_eq!(denied["code"], "agent_unavailable");
    assert_eq!(denied["msg_id"], "second");
    fixture.turn_count(0).await;
    assert!(fixture.state.ctx.session_manager.get_messages(&sid).await.unwrap().iter()
        .all(|row| row.role != "assistant"));
    send(&mut socket, json!({"type":"subscribe", "session_id":null})).await;
    assert_eq!(frame(&mut socket).await["type"], "subscribed", "no revoked reply may precede this barrier");
    assert!(fixture.gate.entered.try_recv().is_err());
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn webchat_schedule_command_cannot_resume_a_bound_main_schedule() {
    use crate::assign_schedule::{manage, AssignKind, AssignPayload, Creator, ManageAction, NewSchedule, ScheduleSpec};
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let (user, token) = fixture.employee("schedule-owner", &["dudu", "finance"], Some("finance")).await;
    let store = crate::task_store::TaskStore::open(fixture._home.path()).unwrap();
    let creator = Creator::new(&user, false).unwrap();
    let now = chrono::Utc::now();
    // The schedule was valid when created; an administrator promoted its
    // employee to Main afterwards. Do not bypass the new creation gate.
    crate::trigger_route::tests::write_agent(&fixture._home.path().join("agents"),
        "dudu", "Main", "@dudu", "specialist", "");
    fixture.state.ctx.registry.write().await.scan().await.unwrap();
    let row = manage::create(fixture._home.path(), &store, &creator, NewSchedule {
        agent_id: "dudu".into(), kind: AssignKind::Goal,
        payload: AssignPayload { description: "Old main schedule".into(), ..Default::default() },
        spec: ScheduleSpec::Daily { hour: 9, minute: 0 },
        timezone: Some("Asia/Taipei".into()), created_via: "dashboard".into(), source: None,
    }, now).await.unwrap();
    manage::manage(fixture._home.path(), &store, &creator, &row.id, ManageAction::Pause, now).await.unwrap();
    crate::trigger_route::tests::write_agent(&fixture._home.path().join("agents"),
        "dudu", "Main", "@dudu", "main", "");
    fixture.state.ctx.registry.write().await.scan().await.unwrap();
    let (mut socket, _) = fixture.connect(&token).await;
    send(&mut socket, json!({"type":"user_message",
        "content":format!("/交辦 排程 恢復 {}", duduclaw_core::truncate_chars(&row.id, 8)),
        "conv":"schedule", "msg_id":"resume-main-schedule"})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    assert_eq!(frame(&mut socket).await["code"], "agent_unavailable");
    fixture.turn_count(0).await;
    let rows = store.list_assign_schedules(Some(&user), 200).await.unwrap();
    assert_eq!(rows[0].status, "paused");
    assert_eq!(fixture.session_count(), 0);
    assert!(fixture.gate.entered.try_recv().is_err());
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn non_admin_socket_target_failures_have_identical_frames() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let (_, token) = fixture.employee("uniform-target", &["finance"], None).await;
    crate::trigger_route::tests::write_agent(&fixture._home.path().join("agents"),
        "sales-rep", "Sales", "@sales-rep", "specialist", "");
    fixture.state.ctx.registry.write().await.scan().await.unwrap();
    let (mut socket, _) = fixture.connect(&token).await;
    let mut expected = None;
    for (target, content) in [
        (Some("missing-employee"), "Hello"),
        (Some("dudu"), "Hello"),
        (Some("sales-rep"), "Hello"),
        (None, "@missing-employee Hello"),
        (None, "@dudu Hello"),
        (None, "@sales-rep Hello"),
    ] {
        send(&mut socket, json!({"type":"user_message", "content":content,
            "agent":target, "msg_id":"uniform-target"})).await;
        let mut denied = frame(&mut socket).await;
        if denied["type"] == "session_info" { denied = frame(&mut socket).await; }
        assert_eq!(denied["code"], "agent_unavailable");
        assert_eq!(denied["message"], crate::webchat::access::AGENT_UNAVAILABLE_TEXT);
        if let Some(ref expected) = expected {
            assert_eq!(&denied, expected, "target {target:?}, content {content}");
        } else {
            expected = Some(denied);
        }
    }
    assert_eq!(fixture.session_count(), 0);
    fixture.turn_count(0).await;
    assert!(fixture.gate.entered.try_recv().is_err());
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn admin_socket_unknown_target_retains_legacy_detail() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("admin-unknown-target");
    let (mut socket, _) = fixture.connect(&token).await;
    send(&mut socket, json!({"type":"user_message", "content":"Hello",
        "agent":"missing-employee", "msg_id":"admin-unknown"})).await;
    let denied = frame(&mut socket).await;
    assert_eq!(denied["message"], "unknown agent: missing-employee");
    assert!(denied.get("code").is_none_or(Value::is_null));
    assert_eq!(fixture.session_count(), 0);
    assert!(fixture.gate.entered.try_recv().is_err());
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn non_admin_assignment_socket_failures_have_identical_frames() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let (_, token) = fixture.employee("uniform-assignment", &["finance"], None).await;
    crate::trigger_route::tests::write_agent(&fixture._home.path().join("agents"),
        "sales-rep", "Sales", "@sales-rep", "specialist", "");
    fixture.state.ctx.registry.write().await.scan().await.unwrap();
    let (mut socket, _) = fixture.connect(&token).await;
    let mut expected = None;
    for target in ["missing-employee", "sales-rep", "dudu"] {
        send(&mut socket, json!({"type":"user_message", "content":format!("/交辦 @{target} work"),
            "msg_id":"uniform-assignment"})).await;
        let mut denied = frame(&mut socket).await;
        if denied["type"] == "session_info" { denied = frame(&mut socket).await; }
        assert_eq!(denied["code"], "agent_unavailable");
        assert_eq!(denied["message"], crate::webchat::access::AGENT_UNAVAILABLE_TEXT);
        if let Some(ref expected) = expected { assert_eq!(&denied, expected); }
        else { expected = Some(denied); }
        fixture.turn_count(0).await;
    }
    assert_eq!(fixture.session_count(), 0);
    assert!(!fixture._home.path().join("tasks.db").exists());
    assert!(fixture.gate.entered.try_recv().is_err());
    socket.close(None).await.unwrap();
}


#[tokio::test]
async fn webchat_reconnect_delivers_saved_reply_only_to_owned_subscribed_conversation() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let alice = fixture.user("reconnect-alice");
    let bob = fixture.user("reconnect-bob");
    let (mut original, _) = fixture.connect(&alice).await;
    message(&mut original, "retained", "reply-1").await;
    let pending = fixture.entered().await;
    let sid = pending.session_id.clone();
    // Resume even before the pipeline creates its session row: admission owns
    // the pending conversation and records its already-authorized employee.
    assert!(fixture.state.ctx.session_manager.session_agent(&sid).await.unwrap().is_none());
    original.close(None).await.unwrap();
    drop(original);

    let (mut resumed, _) = fixture.connect(&alice).await;
    send(&mut resumed, json!({"type":"subscribe", "session_id":sid, "conv":"new-tab"})).await;
    assert_eq!(frame(&mut resumed).await["type"], "session_info");
    assert_eq!(frame(&mut resumed).await["type"], "subscribed");
    let (mut other_conversation, _) = fixture.connect(&alice).await;
    let (mut foreign, _) = fixture.connect(&bob).await;
    send(&mut foreign, json!({"type":"subscribe", "session_id":sid, "conv":"new-tab"})).await;
    assert_eq!(frame(&mut foreign).await["type"], "error");
    pending.release.send(()).unwrap();
    done(&mut resumed, "new-tab", "reply-1", &sid).await;
    fixture.turn_count(0).await;
    assert!(fixture.state.ctx.session_manager.get_messages(&sid).await.unwrap().iter()
        .any(|message| message.role == "assistant" && message.content.contains("OWN_BOT_REPLY")));
    // A control acknowledgement is ordered behind any incorrectly leaked reply.
    for socket in [&mut foreign, &mut other_conversation] {
        send(socket, json!({"type":"subscribe", "session_id":null})).await;
        assert_eq!(frame(socket).await["type"], "subscribed");
    }
    send(&mut resumed, json!({"type":"user_message", "content":"offline", "session_id":sid,
        "conv":"new-tab", "msg_id":"reply-2"})).await;
    assert_eq!(frame(&mut resumed).await["type"], "session_info");
    let pending = fixture.entered().await;
    resumed.close(None).await.unwrap();
    drop(resumed);
    pending.release.send(()).unwrap();
    fixture.turn_count(0).await;
    let history = fixture.state.ctx.session_manager.get_messages(&sid).await.unwrap();
    assert_eq!(history.iter().filter(|message| message.role == "assistant"
        && message.content.contains("OWN_BOT_REPLY")).count(), 2);
    assert!(history.iter().all(|message| !message.content.contains("source changed")));
}

#[tokio::test]
async fn webchat_protocol_ping_is_answered_without_application_timers() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("protocol-heartbeat");
    let (mut socket, _) = fixture.connect(&token).await;
    let ping = tokio::time::timeout(DEADLINE, socket.next()).await.unwrap().unwrap().unwrap();
    let WireMessage::Ping(payload) = ping else { panic!("server must originate a protocol ping"); };
    socket.send(WireMessage::Pong(payload)).await.unwrap();
    send(&mut socket, json!({"type":"subscribe", "session_id":null})).await;
    assert_eq!(frame(&mut socket).await["type"], "subscribed");
}

#[tokio::test]
async fn webchat_explicit_logout_cancels_the_origin_turn_without_cancelling_other_tabs() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("explicit-logout");
    let (mut first, _) = fixture.connect(&token).await;
    let (mut second, _) = fixture.connect(&token).await;
    message(&mut first, "first", "m1").await;
    let logged_out = fixture.entered().await;
    message(&mut second, "second", "m2").await;
    let retained = fixture.entered().await;
    send(&mut first, json!({"type":"logout"})).await;
    fixture.turn_count(1).await;
    assert!(logged_out.release.send(()).is_err());
    retained.release.send(()).unwrap();
    done(&mut second, "second", "m2", &retained.session_id).await;
    fixture.turn_count(0).await;
    assert!(fixture.state.ctx.session_manager.session_agent(&logged_out.session_id).await.unwrap().is_none());
}

#[tokio::test]
async fn webchat_queued_reply_rechecks_answerer_acl_and_retracts_history() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    for failure in ["revoked", "unreadable", "allowed"] {
        let mut fixture = Fixture::start(true).await.unwrap();
        let (user, _) = fixture.employee("queued-acl", &["finance", "sales"], Some("finance")).await;
        let sid = format!("webchat:{user}#agent:finance#conv:queued");
        let (tx, mut rx) = outbound_channel();
        let subscription = fixture.state.subscriptions.register(&user, tx.clone());
        subscription.subscribe(&sid, Some("queued".into()));
        let ticket = fixture.state.turns.admit(&user, &sid, Some("queued".into()), "queued", 3, 10).unwrap();
        let input = TurnInput {
            state: fixture.state.clone(), auth_user: user.clone(), credential: None,
            user_id: user.clone(), agent_id: "dudu".into(), requested_agent: None,
            effective_agent: Some("finance".into()), sid: sid.clone(), conv_nonce: Some("queued".into()),
            content: "Hello".into(), attachments: Vec::new(), assign_event: None,
            resume_acl: false, arrival_seq: 1,
        };
        let task = tokio::spawn(execute(input, TurnSink::new(tx, &sid, Some("queued".into()), "queued"), ticket));
        fixture.entered().await.release.send(()).unwrap();
        let queued = tokio::time::timeout(DEADLINE, async {
            loop {
                let frame = rx.recv().await.unwrap();
                if frame.route.is_some() { break frame; }
            }
        }).await.unwrap();
        assert!(!fixture.state.ctx.session_manager.get_messages(&sid).await.unwrap().iter()
            .any(|row| row.role == "assistant"), "queued replies remain unpublished");
        let route = queued.route.as_ref().unwrap();
        assert_eq!(route.agent, "finance");
        assert!(fixture.state.authorize_target(&user, "sales", true).await.is_ok());
        if failure == "revoked" {
            fixture.state.user_db.unbind_agent(&user, "finance").unwrap();
        } else if failure == "unreadable" {
            // The credential is still valid; only the live ACL read fails.
            rusqlite::Connection::open(fixture._home.path().join("users.db")).unwrap()
                .execute_batch("DROP TABLE user_agent_bindings; DELETE FROM auth_meta;").unwrap();
        }
        let allowed = route.authorized(&fixture.state).await;
        assert_eq!(allowed, failure == "allowed");
        if let Some(ack) = queued.ack { let _ = ack.send(allowed); }
        tokio::time::timeout(DEADLINE, task).await.unwrap().unwrap();
        let history = fixture.state.ctx.session_manager.get_messages(&sid).await.unwrap();
        assert_eq!(history.iter().filter(|row| row.role == "assistant").count(),
            if failure == "allowed" { 1 } else { 0 });
    }
}

#[tokio::test]
async fn webchat_subscription_refusals_echo_only_requested_correlation() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("subscription-refusal");
    let (mut socket, info) = fixture.connect(&token).await;
    for sid in [info["session_id"].as_str().unwrap().to_owned(),
        format!("{}#conv:missing", info["session_id"].as_str().unwrap()),
        "webchat:someone-else#conv:foreign".to_string()] {
        send(&mut socket, json!({"type":"subscribe", "session_id":sid, "conv":"requested-view"})).await;
        let refusal = frame(&mut socket).await;
        assert_eq!(refusal["type"], "error");
        assert_eq!(refusal["code"], "subscription_unavailable");
        assert_eq!(refusal["session_id"], sid);
        assert_eq!(refusal["conv"], "requested-view");
    }
    socket.close(None).await.unwrap();
}


/// Exposes the readiness boundary without sleeps or dependence on OS buffers.
struct BackpressuredWriter {
    ready: oneshot::Receiver<()>,
    waiting: Option<oneshot::Sender<()>>,
    written: Arc<Mutex<Vec<Message>>>,
}

impl futures_util::Stream for BackpressuredWriter {
    type Item = Result<Message, ()>;
    fn poll_next(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Pending
    }
}

impl futures_util::Sink<Message> for BackpressuredWriter {
    type Error = ();
    fn poll_ready(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), ()>> {
        if let Some(waiting) = self.waiting.take() { let _ = waiting.send(()); }
        std::future::Future::poll(std::pin::Pin::new(&mut self.ready), cx).map(|ready| ready.map_err(|_| ()))
    }
    fn start_send(self: std::pin::Pin<&mut Self>, item: Message) -> Result<(), ()> {
        self.written.lock().unwrap().push(item);
        Ok(())
    }
    fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), ()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_close(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), ()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn webchat_writer_rechecks_acl_after_backpressure_before_start_send() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    for failure in ["allowed", "revoked", "generation", "cancelled", "source_refused", "unreadable"] {
        let refused = failure != "allowed";
        let fixture = Fixture::start(true).await.unwrap();
        let (user, token) = fixture.employee("backpressure", &["finance"], None).await;
        let credential = fixture.state.jwt_config.verify_access_token(&token).unwrap();
        let subscriptions = subscriptions::Subscriptions::default();
        let (tx, mut rx) = outbound_channel();
        let subscription = subscriptions.register(&user, tx);
        subscription.subscribe("owned", None);
        let cancel = CancellationToken::new();
        let source = crate::channel_reply::ReplyDeliveryFence::test_live().for_turn(&cancel);
        let mut turn = subscriptions.sinks(&user, "owned", "m", "finance", Some(&credential), source.clone())
            .pop().unwrap();
        let sending = tokio::spawn(async move {
            turn.send(Message::Text(r#"{"type":"assistant_done","content":"private"}"#.into())).await
        });
        let queued = rx.recv().await.unwrap();
        let (release, ready) = oneshot::channel();
        let (waiting, blocked) = oneshot::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let captured = written.clone();
        let state = fixture.state.clone();
        let writer = tokio::spawn(async move {
            let gate = SocketGate::default();
            // Match production's adapter order: SplitSink buffers the frame
            // before the underlying writer becomes ready.
            let (writer, _reader) = GuardedSocket::new(
                BackpressuredWriter { ready, waiting: Some(waiting), written: captured }, gate.clone(),
            ).split();
            queued.deliver_live(&state, Some(&credential),
                &mut crate::handlers::live_context::SocketRecovery::default(),
                &mut BoundedSocketSink(writer), &gate).await
        });
        // This notification is emitted only after the first authorization has
        // passed, and while poll_ready is still pending. No frame was written.
        tokio::time::timeout(DEADLINE, blocked).await.unwrap().unwrap();
        assert!(written.lock().unwrap().is_empty());
        match failure {
            "revoked" => fixture.state.user_db.unbind_agent(&user, "finance").unwrap(),
            "generation" => fixture.state.user_db.bind_agent(&user, "unrelated", duduclaw_auth::AccessLevel::Owner).unwrap(),
            "cancelled" => cancel.cancel(),
            "source_refused" => source.refuse(),
            "unreadable" => {
                rusqlite::Connection::open(fixture._home.path().join("users.db")).unwrap()
                    .execute_batch("DELETE FROM auth_meta").unwrap();
            }
            _ => {}
        }
        release.send(()).unwrap();
        assert_eq!(tokio::time::timeout(DEADLINE, writer).await.unwrap().unwrap().is_ok(), !refused);
        assert_eq!(sending.await.unwrap().is_ok(), !refused);
        assert_eq!(written.lock().unwrap().len(), usize::from(!refused));
        assert_eq!(source.valid().await, !refused);
    }
}

#[tokio::test]
async fn webchat_final_policy_change_sends_no_bytes_and_publishes_no_history() {
    let _serial = tokio::time::timeout(DEADLINE, SOCKET_TEST_LOCK.lock()).await.unwrap();
    for change in ["allowed", "role", "default", "status", "public", "missing_config", "config_busy", "registry_busy"] {
        assert_final_policy_window(change).await;
    }
}

#[tokio::test]
async fn webchat_final_edition_downgrade_sends_no_bytes_and_publishes_no_history() {
    let _serial = tokio::time::timeout(DEADLINE, SOCKET_TEST_LOCK.lock()).await.unwrap();
    for change in ["edition_downgrade", "edition_roundtrip", "edition_unchanged"] {
        assert_final_policy_window(change).await;
    }
}

async fn assert_final_policy_window(change: &str) {
    tokio::time::timeout(DEADLINE, async {
        let allowed = matches!(change, "allowed" | "edition_unchanged");
        let fixture = Fixture::start(true).await.unwrap();
        let (user, token) = fixture.employee("final-policy", &["finance"], None).await;
        let (user, token) = if change.starts_with("edition_") {
            let db = &fixture.state.user_db;
            let manager = db.create_user("manager@socket.test", "Manager", "Manager-Test-Password-12345",
                duduclaw_auth::UserRole::Manager).unwrap();
            db.set_manager(&user, Some(&manager.id)).unwrap();
            let access = db.effective_access(&manager.id).unwrap().unwrap();
            assert!(access.direct.is_empty(), "permission must come only from the team grant");
            assert!(access.team.contains_key("finance"));
            let token = fixture.state.jwt_config.issue_access_token(&manager, &[]).unwrap();
            (manager.id, token)
        } else { (user, token) };
        let credential = fixture.state.jwt_config.verify_access_token(&token).unwrap();
        let path = fixture._home.path().join("agents/finance/agent.toml");
        if change == "public" {
            let raw = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{raw}\n[channels]\npublic_trigger = true\n")).unwrap();
            fixture.state.ctx.registry.write().await.scan().await.unwrap();
            fixture.state.user_db.unbind_agent(&user, "finance").unwrap();
        }
        let sessions = &fixture.state.ctx.session_manager;
        sessions.get_or_create("owned", "finance").await.unwrap();
        let row = sessions.append_delivery_message("owned", "assistant", "private", 0, None, true).await.unwrap();
        let generation = fixture.state.user_db.acl_generation().unwrap();
        let policy = fixture.state.target_policy(&user, "finance", true).await.unwrap();
        assert!(fixture.state.authorize_target(&user, "finance", true).await.is_ok());
        assert!(policy.with_current(|| ()).is_ok());
        let (tx, mut rx) = outbound_channel();
        let subscriptions = subscriptions::Subscriptions::default();
        let subscription = subscriptions.register(&user, tx);
        subscription.subscribe("owned", None);
        let source = crate::channel_reply::ReplyDeliveryFence::test_live();
        let mut turn = subscriptions.sinks(&user, "owned", "m", "finance", Some(&credential), source.clone()).pop().unwrap();
        let sending = tokio::spawn(async move {
            turn.send(Message::Text(r#"{"type":"assistant_done","content":"private"}"#.into())).await
        });
        let queued = rx.recv().await.unwrap();
        let (release, ready) = oneshot::channel();
        let (waiting, blocked) = oneshot::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let captured = written.clone();
        let state = fixture.state.clone();
        let writer = tokio::spawn(async move {
            let gate = SocketGate::default();
            let (writer, _reader) = GuardedSocket::new(
                BackpressuredWriter { ready, waiting: Some(waiting), written: captured }, gate.clone(),
            ).split();
            queued.deliver_live(&state, Some(&credential), &mut Default::default(),
                &mut BoundedSocketSink(writer), &gate).await
        });
        // SplitSink::poll_ready only reserves its slot. The underlying
        // readiness notification is AFTER deliver_live's LAST async check
        // and permit creation, immediately before GuardedSocket::start_send.
        blocked.await.unwrap();
        assert!(written.lock().unwrap().is_empty());
        let mut config_lock = None;
        let mut registry_lock = None;
        match change {
            "edition_downgrade" | "edition_roundtrip" | "edition_unchanged" => {
                let home = fixture._home.path();
                if change != "edition_unchanged" {
                    crate::team_edition::record(home, duduclaw_core::EditionProfile::Personal);
                }
                if change != "edition_downgrade" {
                    crate::team_edition::record(home, duduclaw_core::EditionProfile::Enterprise);
                }
            }
            "role" | "status" | "public" => {
                let raw = std::fs::read_to_string(&path).unwrap();
                let raw = match change {
                    "role" => raw.replace("role = \"specialist\"", "role = \"main\""),
                    "status" => raw.replace("status = \"active\"", "status = \"paused\""),
                    _ => raw.replace("public_trigger = true", "public_trigger = false"),
                };
                std::fs::write(&path, raw).unwrap();
                fixture.state.ctx.registry.write().await.scan().await.unwrap();
            }
            "default" => {
                let path = fixture._home.path().join("config.toml");
                duduclaw_core::with_file_lock(&path, || std::fs::write(&path,
                    "[general]\ndefault_agent = \"finance\"\n")).unwrap();
            }
            "missing_config" => std::fs::remove_file(fixture._home.path().join("config.toml")).unwrap(),
            "config_busy" => {
                let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
                    .open(fixture._home.path().join("config.toml.lock")).unwrap();
                fs2::FileExt::try_lock_exclusive(&lock).unwrap();
                config_lock = Some(lock);
            }
            "registry_busy" => registry_lock = Some(fixture.state.ctx.registry.write().await),
            _ => {}
        }
        assert_eq!(fixture.state.user_db.acl_generation().unwrap(), generation,
            "policy-only changes must exercise the gap outside users.db");
        // Independently exercise publication with an unconditional source
        // callback BEFORE the socket refusal can latch anything false.
        let published = sessions.publish_webchat_message("owned", row, &fixture.state.user_db,
            generation, &policy, || true).await;
        assert_eq!(matches!(published, Ok(true)), allowed, "{change}");
        release.send(()).unwrap();
        assert_eq!(writer.await.unwrap().is_ok(), allowed, "{change}");
        assert_eq!(sending.await.unwrap().is_ok(), allowed, "{change}");
        assert_eq!(written.lock().unwrap().len(), usize::from(allowed), "{change}");
        let history = sessions.webchat_history("owned").await.unwrap();
        assert_eq!(history.iter().filter(|row| row.message.role == "assistant").count(),
            usize::from(allowed), "{change}");
        assert_eq!(sessions.get_messages("owned").await.unwrap().len(), usize::from(allowed));
        drop(registry_lock);
        drop(config_lock);
    }).await.unwrap_or_else(|_| panic!("final policy window timed out: {change}"));
}
