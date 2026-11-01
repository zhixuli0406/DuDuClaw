//! Real WebSocket reader/writer coverage; only model execution is replaced.
use super::*;
use crate::agent_channels::e2e_seam::{WaitingTurn, WaitingTurns, arm_waiting};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message as WireMessage};

trait TestIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> TestIo for T {}
type Socket = WebSocketStream<Box<dyn TestIo>>;
const DEADLINE: Duration = Duration::from_secs(5);
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
                WireMessage::Text(text) => return serde_json::from_str(&text).unwrap(),
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
    assert!(limited.get("msg_id").is_none());
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
