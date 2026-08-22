// S4 — background session manager for the dedicated chat socket
// (`/ws/chat`), separate from `ws_status.rs`'s main authenticated `/ws`.
// Same background-OS-thread + tiny current-thread tokio runtime +
// channel-bridge architecture as `ws_status.rs` (see that file's module doc
// comment for why); deliberately its OWN thread/runtime rather than reusing
// `ws_status.rs`'s, so the two protocols stay fully independent — a chat
// reconnect storm can never starve the main `/ws` RPC layer or vice versa.
//
// Protocol source of truth: `crates/duduclaw-gateway/src/webchat.rs`
// (`handle_chat_socket`) — read directly, not guessed:
//   1. First client frame MUST be `{"type":"auth","token":"<jwt>"}` within
//      a 10s server-side timeout, or the server sends an `error` frame and
//      closes.
//   2. On success the server immediately sends one `session_info` frame
//      (no separate "auth ok" ack — `session_info` IS the ack).
//   3. Per turn: client sends `user_message`; server streams
//      `assistant_chunk` (repeated) / `step` / `progress`, then exactly one
//      `assistant_done` (or `error`).
//   4. Server pings every 30s, closes after 60s without a `Pong` — this
//      client replies to every `Ping` explicitly (same reasoning as
//      `ws_status.rs`'s post-S4 change: the split read/write halves don't
//      get an implicit auto-Pong).
//
// Honest stubs (S4 scope cuts, not oversights):
//   - No file attachments (`chat_protocol::OutFrame::UserMessage.attachments`
//     is always empty).
//   - No session resume (`session_id` is always either `None` or whatever
//     the connection's own `session_info` announced — never a client-chosen
//     past session id; WP3 resume is a Phase 1b "past conversations" list
//     feature this crate doesn't have yet).
//   - No `step`/`progress` history — only the MOST RECENT one is surfaced,
//     as a transient "thinking" status line, not a persistent tool-call
//     tree (see `chat_protocol.rs`'s doc comment).

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::api;
use crate::chat_protocol::{self, InFrame};

/// Matches `ws_status.rs`'s own numbers (see that file's module doc comment
/// for why the task brief's 1s/5s pair, not `ws-client.ts`'s 1s/30s/10-cap
/// pair, is what this crate follows).
const RECONNECT_FLOOR: Duration = Duration::from_secs(1);
const RECONNECT_CEILING: Duration = Duration::from_secs(5);
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);

pub enum Command {
    /// (Re)connect with this JWT — aborts any prior connection/backoff loop,
    /// same replace-not-stack semantics as `ws_status::Command::ConnectWs`.
    Connect { jwt: String },
    /// Send one user turn on the current connection. Silently dropped if
    /// nothing is connected (mirrors `ws_status.rs`'s `dispatch_call`
    /// fail-fast-not-queue policy) — the caller (the chat screen) already
    /// disables the send control while `ConnState != Authenticated`, so
    /// this is a defence-in-depth no-op, not the primary guard.
    ///
    /// `agent`: S4b's agent picker (`screens/chat/agents_picker.rs`) — `None`
    /// is byte-compatible with S4's pre-picker behavior (the server treats
    /// an absent `agent` as "the main/default agent"); `Some(id)` routes
    /// this turn to that specific employee, per `chat_protocol.rs`'s
    /// `OutFrame::UserMessage.agent` doc comment.
    Send { content: String, session_id: Option<String>, agent: Option<String>, conv: Option<String> },
    Disconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Authenticated,
}

#[derive(Debug, Clone)]
pub enum Event {
    ConnState(ConnState),
    /// Terminal per attempt, same contract as `ws_status::Event::WsAuthFailed`
    /// — the reconnect loop has already stopped; the caller must send a
    /// fresh `Connect` with a new/refreshed JWT.
    AuthFailed,
    SessionInfo { session_id: String, agent_name: String, agent_icon: String, model: String },
    Chunk { content: String },
    /// Folded `step`/`progress` into one "still working" status line — see
    /// this file's module doc comment on why no tree/history is kept.
    Status { text: String },
    Done { content: String, tokens_used: u32, model: Option<String> },
    Error {
        message: String,
        /// Machine-readable error class (e.g. `agent_forbidden`,
        /// `must_change_password_required`) — carried through for a future
        /// pass that branches on it (mirrors `ws-client.ts`'s
        /// `MUST_CHANGE_PASSWORD_ERROR_CODE` handling). `screens/chat.rs`
        /// currently just shows `message` to the user regardless of code.
        #[allow(dead_code)]
        code: Option<String>,
    },
}

pub fn spawn() -> (tokio_mpsc::UnboundedSender<Command>, std_mpsc::Receiver<Event>) {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();
    let (evt_tx, evt_rx) = std_mpsc::channel::<Event>();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[chat] tokio runtime build failed: {e}");
                return;
            }
        };
        rt.block_on(run(cmd_rx, evt_tx));
    });

    (cmd_tx, evt_rx)
}

async fn run(mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>, evt_tx: std_mpsc::Sender<Event>) {
    let mut task: Option<tokio::task::JoinHandle<()>> = None;
    // The outbound-frame channel for whichever connection attempt is
    // currently live — `Command::Send` writes into it; `None` while
    // disconnected (see `session_loop`'s per-attempt registration).
    let out_tx: std::sync::Arc<tokio::sync::Mutex<Option<tokio_mpsc::UnboundedSender<Message>>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(None));

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Connect { jwt } => {
                if let Some(handle) = task.take() {
                    handle.abort();
                }
                *out_tx.lock().await = None;
                let evt_tx = evt_tx.clone();
                let out_tx = out_tx.clone();
                // WP-C-M2: resolved fresh (not a hardcoded `CHAT_WS_URL`
                // const) — see `ws_status.rs`'s matching `ConnectWs` arm for
                // why this must happen at connect time.
                let chat_ws_url = api::gateway_ws_url("/ws/chat");
                task = Some(tokio::spawn(session_loop(chat_ws_url, jwt, evt_tx, out_tx)));
            }
            Command::Send { content, session_id, agent, conv } => {
                let frame = chat_protocol::build_user_message(&content, session_id, agent, conv);
                let sender = out_tx.lock().await.clone();
                if let Some(sender) = sender {
                    let _ = sender.send(Message::Text(frame));
                } else {
                    eprintln!("[chat] Send dropped — not connected");
                }
            }
            Command::Disconnect => {
                if let Some(handle) = task.take() {
                    handle.abort();
                }
                *out_tx.lock().await = None;
                let _ = evt_tx.send(Event::ConnState(ConnState::Disconnected));
            }
        }
    }
}

enum Outcome {
    AuthFailed,
    DroppedBeforeAuth,
    DroppedAfterAuth,
}

async fn session_loop(
    url: String,
    jwt: String,
    evt_tx: std_mpsc::Sender<Event>,
    out_tx: std::sync::Arc<tokio::sync::Mutex<Option<tokio_mpsc::UnboundedSender<Message>>>>,
) {
    let mut backoff = RECONNECT_FLOOR;
    loop {
        let _ = evt_tx.send(Event::ConnState(ConnState::Connecting));
        match connect_and_run(&url, &jwt, &evt_tx, &out_tx).await {
            Outcome::AuthFailed => {
                let _ = evt_tx.send(Event::AuthFailed);
                let _ = evt_tx.send(Event::ConnState(ConnState::Disconnected));
                return;
            }
            Outcome::DroppedAfterAuth => {
                backoff = RECONNECT_FLOOR;
                let _ = evt_tx.send(Event::ConnState(ConnState::Disconnected));
                tokio::time::sleep(backoff).await;
            }
            Outcome::DroppedBeforeAuth => {
                let _ = evt_tx.send(Event::ConnState(ConnState::Disconnected));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_CEILING);
            }
        }
    }
}

async fn connect_and_run(
    url: &str,
    jwt: &str,
    evt_tx: &std_mpsc::Sender<Event>,
    out_tx: &std::sync::Arc<tokio::sync::Mutex<Option<tokio_mpsc::UnboundedSender<Message>>>>,
) -> Outcome {
    let connect_fut = tokio_tungstenite::connect_async(url);
    let (ws_stream, _resp) = match tokio::time::timeout(AUTH_TIMEOUT, connect_fut).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            eprintln!("[chat] connect failed: {e}");
            return Outcome::DroppedBeforeAuth;
        }
        Err(_) => {
            eprintln!("[chat] connect timed out");
            return Outcome::DroppedBeforeAuth;
        }
    };
    let _ = evt_tx.send(Event::ConnState(ConnState::Connected));

    let (mut write, mut read) = ws_stream.split();

    // First frame MUST be `auth` (webchat.rs's C5 gate) — sent before
    // reading anything.
    let auth_frame = chat_protocol::build_auth(jwt);
    if let Err(e) = write.send(Message::Text(auth_frame)).await {
        eprintln!("[chat] auth send failed: {e}");
        return Outcome::DroppedBeforeAuth;
    }

    // Server's very next frame is either `session_info` (success — IS the
    // ack, no separate ok/fail envelope) or `error` (rejected, then closes).
    match tokio::time::timeout(AUTH_TIMEOUT, read.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match chat_protocol::parse_in_frame(&text) {
            Ok(InFrame::SessionInfo { session_id, agent_name, agent_icon, model, .. }) => {
                let _ = evt_tx.send(Event::ConnState(ConnState::Authenticated));
                let _ = evt_tx.send(Event::SessionInfo { session_id, agent_name, agent_icon, model });
            }
            Ok(InFrame::Error { message, .. }) => {
                eprintln!("[chat] auth rejected: {message}");
                return Outcome::AuthFailed;
            }
            Ok(_) => {
                eprintln!("[chat] unexpected first frame (not session_info/error)");
                return Outcome::DroppedBeforeAuth;
            }
            Err(e) => {
                eprintln!("[chat] first frame unparseable: {e}");
                return Outcome::DroppedBeforeAuth;
            }
        },
        Ok(Some(Ok(_other))) => return Outcome::DroppedBeforeAuth,
        Ok(Some(Err(e))) => {
            eprintln!("[chat] handshake read failed: {e}");
            return Outcome::DroppedBeforeAuth;
        }
        Ok(None) => return Outcome::DroppedBeforeAuth,
        Err(_) => {
            eprintln!("[chat] handshake response timed out");
            return Outcome::DroppedBeforeAuth;
        }
    }

    let (tx, mut out_rx) = tokio_mpsc::unbounded_channel::<Message>();
    *out_tx.lock().await = Some(tx);

    let outcome = loop {
        tokio::select! {
            incoming = read.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => dispatch_in_frame(&text, evt_tx),
                    Some(Ok(Message::Ping(data))) => {
                        if write.send(Message::Pong(data)).await.is_err() {
                            break Outcome::DroppedAfterAuth;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => break Outcome::DroppedAfterAuth,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        eprintln!("[chat] read error: {e}");
                        break Outcome::DroppedAfterAuth;
                    }
                }
            }
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(msg) => {
                        if write.send(msg).await.is_err() {
                            break Outcome::DroppedAfterAuth;
                        }
                    }
                    None => break Outcome::DroppedAfterAuth,
                }
            }
        }
    };

    *out_tx.lock().await = None;
    outcome
}

fn dispatch_in_frame(text: &str, evt_tx: &std_mpsc::Sender<Event>) {
    let frame = match chat_protocol::parse_in_frame(text) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[chat] frame unparseable: {e} (text: {text})");
            return;
        }
    };
    let event = match frame {
        InFrame::AssistantChunk { content } => Event::Chunk { content },
        InFrame::Progress { content, kind, .. } => {
            if kind.as_deref() == Some("keepalive") {
                return;
            }
            Event::Status { text: content }
        }
        InFrame::Step { tool, summary, .. } => {
            Event::Status { text: summary.unwrap_or(tool) }
        }
        InFrame::AssistantDone { content, tokens_used, model, .. } => {
            Event::Done { content, tokens_used, model }
        }
        InFrame::Error { message, code } => Event::Error { message, code },
        InFrame::SessionInfo { session_id, agent_name, agent_icon, model, .. } => {
            Event::SessionInfo { session_id, agent_name, agent_icon, model }
        }
    };
    let _ = evt_tx.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNREACHABLE_URL: &str = "ws://127.0.0.1:1/ws/chat";

    /// Live path: the real gateway's `/ws/chat` must reject a bogus JWT the
    /// same way `ws_status.rs`'s sibling test proves for the main `/ws` —
    /// connecting → connected → `error` frame → terminal `AuthFailed`, no
    /// further reconnect. Requires a local gateway on 127.0.0.1:18789.
    #[test]
    fn chat_auth_failure_against_live_gateway_is_terminal() {
        let (cmd_tx, evt_rx) = spawn();
        cmd_tx
            .send(Command::Connect { jwt: "not-a-real-jwt".to_string() })
            .expect("manager thread should still be alive");

        let mut saw_connecting = false;
        let mut saw_connected = false;
        let mut saw_auth_failed = false;
        let mut saw_disconnected_after = false;

        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            match evt_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::ConnState(ConnState::Connecting)) => saw_connecting = true,
                Ok(Event::ConnState(ConnState::Connected)) => saw_connected = true,
                Ok(Event::AuthFailed) => saw_auth_failed = true,
                Ok(Event::ConnState(ConnState::Disconnected)) if saw_auth_failed => {
                    saw_disconnected_after = true;
                    break;
                }
                Ok(_) => {}
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        assert!(saw_connecting, "expected a Connecting state");
        assert!(saw_connected, "expected a Connected state (TCP+WS upgrade ok)");
        assert!(
            saw_auth_failed,
            "expected AuthFailed — is the live gateway really rejecting this token? \
             (curl :18789/healthz to confirm a gateway is up)"
        );
        assert!(saw_disconnected_after, "expected a final Disconnected state after AuthFailed");
    }

    /// Backoff shape against an unreachable port — same acceptance
    /// criterion as `ws_status.rs`'s sibling test (grows, never shrinks,
    /// caps at `RECONNECT_CEILING` + jitter tolerance).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_reconnect_backoff_grows_and_caps_against_unreachable_host() {
        let (evt_tx, evt_rx) = std_mpsc::channel::<Event>();
        let out_tx = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let handle = tokio::spawn(session_loop(UNREACHABLE_URL.to_string(), "x".to_string(), evt_tx, out_tx));

        let start = std::time::Instant::now();
        let deadline = start + Duration::from_secs(11);
        let mut disconnected_at = Vec::new();
        while std::time::Instant::now() < deadline && disconnected_at.len() < 4 {
            match evt_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::ConnState(ConnState::Disconnected)) => disconnected_at.push(start.elapsed()),
                Ok(_) => {}
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        handle.abort();

        assert!(
            disconnected_at.len() >= 3,
            "expected at least 3 reconnect cycles in 11s, got {}: {disconnected_at:?}",
            disconnected_at.len()
        );

        let tolerance = Duration::from_millis(600);
        let mut prev_gap = Duration::from_millis(0);
        for pair in disconnected_at.windows(2) {
            let gap = pair[1].saturating_sub(pair[0]);
            assert!(gap + tolerance >= prev_gap, "backoff shrank: {prev_gap:?} -> {gap:?}");
            assert!(gap <= RECONNECT_CEILING + tolerance, "gap exceeded ceiling: {gap:?}");
            prev_gap = gap;
        }
    }

    #[test]
    fn dispatch_in_frame_ignores_keepalive_progress() {
        let (tx, rx) = std_mpsc::channel::<Event>();
        dispatch_in_frame(r#"{"type":"progress","content":"x","kind":"keepalive"}"#, &tx);
        assert!(rx.try_recv().is_err(), "keepalive progress must not produce an Event");
    }

    #[test]
    fn dispatch_in_frame_maps_chunk() {
        let (tx, rx) = std_mpsc::channel::<Event>();
        dispatch_in_frame(r#"{"type":"assistant_chunk","content":"hi"}"#, &tx);
        match rx.try_recv().unwrap() {
            Event::Chunk { content } => assert_eq!(content, "hi"),
            other => panic!("expected Chunk, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_in_frame_malformed_is_dropped_not_panicking() {
        let (tx, rx) = std_mpsc::channel::<Event>();
        dispatch_in_frame("not json", &tx);
        assert!(rx.try_recv().is_err());
    }

    /// Spin up a minimal mock `/ws/chat` server on an ephemeral loopback
    /// port: accepts one connection, requires the first frame to be
    /// `{"type":"auth",...}` (any token — this mock doesn't validate it,
    /// it's standing in for a gateway that WOULD accept), replies with a
    /// `session_info` frame, then immediately pushes one `assistant_chunk` +
    /// `assistant_done` pair. Real credentials this test suite has no way to
    /// obtain (see this file's sibling live tests' own caveats) block
    /// verifying the SUCCESS path against the actual gateway — this closes
    /// that gap without needing any: it proves `connect_and_run`'s
    /// handshake-success branch and the streaming read loop both parse a
    /// real network round trip correctly, not just the codec in isolation
    /// (`chat_protocol.rs`'s tests already cover that half).
    async fn spawn_mock_chat_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind mock server");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else { return };
            let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
            // First frame must be `auth` — a malformed/missing one would
            // mean this test's OWN client-side code is broken, not the
            // server under test, so this mock fails loudly (panics) rather
            // than silently returning like the failure branches above.
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    assert!(text.contains(r#""type":"auth""#), "expected an auth frame first, got: {text}");
                }
                other => panic!("expected the client's auth frame, got: {other:?}"),
            }
            let session_info = r#"{"type":"session_info","session_id":"webchat:mock","agent_name":"Mocky",
                "agent_icon":"🤖","supports_vision":false,"model":"mock-model","refresh":false}"#;
            if ws.send(Message::Text(session_info.into())).await.is_err() {
                return;
            }
            let _ = ws.send(Message::Text(r#"{"type":"assistant_chunk","content":"hi "}"#.into())).await;
            let _ = ws
                .send(Message::Text(r#"{"type":"assistant_done","content":"hi there","tokens_used":7}"#.into()))
                .await;
            // Keep the socket open long enough for the client to read
            // everything before this task exits and drops the connection.
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        format!("ws://{addr}/ws/chat")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_full_round_trip_against_mock_server() {
        let url = spawn_mock_chat_server().await;
        let (evt_tx, evt_rx) = std_mpsc::channel::<Event>();
        let out_tx = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let handle = tokio::spawn(session_loop(url, "test-jwt".to_string(), evt_tx, out_tx));

        let mut saw_authenticated = false;
        let mut saw_session_info = false;
        let mut saw_chunk = false;
        let mut saw_done = false;

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline
            && !(saw_authenticated && saw_session_info && saw_chunk && saw_done)
        {
            match evt_rx.recv_timeout(Duration::from_millis(300)) {
                Ok(Event::ConnState(ConnState::Authenticated)) => saw_authenticated = true,
                Ok(Event::SessionInfo { session_id, agent_name, model, .. }) => {
                    assert_eq!(session_id, "webchat:mock");
                    assert_eq!(agent_name, "Mocky");
                    assert_eq!(model, "mock-model");
                    saw_session_info = true;
                }
                Ok(Event::Chunk { content }) => {
                    assert_eq!(content, "hi ");
                    saw_chunk = true;
                }
                Ok(Event::Done { content, tokens_used, .. }) => {
                    assert_eq!(content, "hi there");
                    assert_eq!(tokens_used, 7);
                    saw_done = true;
                }
                Ok(_) => {}
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        handle.abort();

        assert!(saw_authenticated, "expected ConnState::Authenticated");
        assert!(saw_session_info, "expected a SessionInfo event");
        assert!(saw_chunk, "expected a Chunk event");
        assert!(saw_done, "expected a Done event");
    }
}
