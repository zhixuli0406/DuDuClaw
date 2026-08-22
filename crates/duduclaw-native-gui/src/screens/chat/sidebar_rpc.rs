// S4b — dedicated read-only RPC connection for the chat screen's own needs
// (agent picker + conversation list/history), reusing `crate::rpc`'s pure
// req/res codec — the SAME wire protocol `ws_status.rs` already speaks on
// the main authenticated `/ws` — but over its OWN connection/thread.
//
// ── Why not reuse `ws_status.rs`'s already-open connection ────────────────
// `ws_status.rs` already carries exactly this infrastructure
// (`Command::Call` / the `call()` free function), explicitly built as
// "new infrastructure for future RPC-backed pages" with no consumer yet.
// The natural wiring would be handing `ChatState` a clone of `main.rs`'s
// `session_tx` at construction time. This pass's task brief keeps
// `main.rs`'s wiring off-limits on purpose (a concurrent pass owns
// `main.rs`/`nav.rs` routing this round — see `screens/chat.rs`'s module doc
// comment), so that one-line wiring change is not available here. This
// module is the self-contained alternative: `screens/chat.rs`'s `ChatState`
// already receives the JWT via `ChatState::connect` (called by `main.rs` at
// the exact two call sites that also feed `chat_ws.rs`, byte-unchanged), and
// hands it here too — zero `main.rs` edits. Once `main.rs` is free to
// change, wiring this screen onto `ws_status::call` instead and deleting
// this module is the natural follow-up.
//
// ── Why simpler than `ws_status.rs`/`chat_ws.rs` ───────────────────────────
// Both of those maintain a PERSISTENT, auto-reconnecting connection because
// they carry continuous traffic (chat streaming, server-pushed events).
// Every call this module makes is infrequent and read-only — opening the
// chat page, switching the agent filter, picking a conversation — so
// `call_once` dials a FRESH connection per call, authenticates, sends
// exactly one request, and closes. No reconnect backoff state machine, no
// multiplexed `PendingCalls` registry (one request per connection is its own
// only pending call). The cost — one extra TCP+WS handshake per call, a few
// ms on loopback — is a good trade for the code this avoids carrying.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::api;
use crate::rpc::{self, CallError, WsFrame};

/// Matches `ws_status.rs`'s own handshake timeout.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Matches `ws_status.rs`'s own call timeout.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// This module only ever sends ONE request per connection, so a fixed id is
/// fine — there is never a second in-flight request to collide with on the
/// same socket.
const HANDSHAKE_ID: &str = "1";
const CALL_ID: &str = "2";

pub enum Command {
    /// Remember this JWT for future `Call`s — sent from `ChatState::connect`
    /// at the same moment `chat_ws::Command::Connect` is (see module doc
    /// comment). Does not itself open a connection; calls are lazy/one-shot
    /// (see `call_once`).
    Connect { jwt: String },
    Call { method: String, params: Value, respond_to: oneshot::Sender<Result<Value, CallError>> },
    Disconnect,
}

/// Spawn the background thread + its tiny tokio runtime (same
/// background-OS-thread + `new_current_thread` runtime shape as
/// `chat_ws.rs`/`ws_status.rs` — gpui's own executor is not a tokio context,
/// see `main.rs`'s gotcha list). Returns a command sender; this module has
/// no event channel to poll (see module doc comment — every result comes
/// back through its own call's `oneshot`, not a shared event stream), so
/// there is nothing for `main.rs`'s poll loop to drain, and none is added.
pub fn spawn() -> tokio_mpsc::UnboundedSender<Command> {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[chat-sidebar-rpc] tokio runtime build failed: {e}");
                return;
            }
        };
        rt.block_on(run(cmd_rx));
    });
    cmd_tx
}

async fn run(mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>) {
    let mut jwt: Option<String> = None;
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Connect { jwt: j } => jwt = Some(j),
            Command::Disconnect => jwt = None,
            Command::Call { method, params, respond_to } => {
                let Some(j) = jwt.clone() else {
                    let _ = respond_to.send(Err(CallError::NotConnected));
                    continue;
                };
                // Detached so one slow call can't stall the next command
                // reaching this loop (mirrors `ws_status.rs::dispatch_call`'s
                // timeout-watchdog reasoning, inlined here since there's no
                // shared `PendingCalls` registry to delegate the timeout to).
                tokio::spawn(async move {
                    // WP-C-M2: resolved fresh per call (not a hardcoded
                    // `WS_URL` const) — this module dials a brand-new
                    // connection per call anyway (see module doc comment),
                    // so there's no reconnect-loop lifetime concern here,
                    // unlike `ws_status.rs`/`chat_ws.rs`'s long-lived
                    // sessions.
                    let ws_url = api::gateway_ws_url("/ws");
                    let result = tokio::time::timeout(CALL_TIMEOUT, call_once(&ws_url, &j, &method, params))
                        .await
                        .unwrap_or(Err(CallError::Timeout));
                    let _ = respond_to.send(result);
                });
            }
        }
    }
}

/// One full round trip: connect → `connect{jwt}` handshake → the actual
/// request → matching response → close. See module doc comment for why this
/// dials fresh every time instead of keeping a connection alive.
///
/// `url` is a parameter (not resolved internally via `api::gateway_ws_url`)
/// so the wire parsing is testable against a local mock server without a
/// real gateway/credentials — same test-seam pattern `ws_status.rs`/
/// `chat_ws.rs` already use for their own `connect_and_run`/`session_loop`.
async fn call_once(url: &str, jwt: &str, method: &str, params: Value) -> Result<Value, CallError> {
    let connect_fut = tokio_tungstenite::connect_async(url);
    let (ws_stream, _resp) = match tokio::time::timeout(HANDSHAKE_TIMEOUT, connect_fut).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            eprintln!("[chat-sidebar-rpc] connect failed: {e}");
            return Err(CallError::NotConnected);
        }
        Err(_) => {
            eprintln!("[chat-sidebar-rpc] connect timed out");
            return Err(CallError::NotConnected);
        }
    };
    let (mut write, mut read) = ws_stream.split();

    // Wire format per `ws_status.rs::connect_and_run` — cross-checked there,
    // not re-derived independently.
    let handshake = serde_json::json!({
        "type": "req",
        "id": HANDSHAKE_ID,
        "method": "connect",
        "params": { "jwt": jwt },
    });
    if let Err(e) = write.send(Message::Text(handshake.to_string())).await {
        eprintln!("[chat-sidebar-rpc] handshake send failed: {e}");
        return Err(CallError::NotConnected);
    }

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<Value>(&text) {
            Ok(v) if v.get("ok").and_then(|b| b.as_bool()) == Some(true) => {}
            Ok(_) => {
                eprintln!("[chat-sidebar-rpc] handshake rejected: {text}");
                return Err(CallError::NotConnected);
            }
            Err(e) => {
                eprintln!("[chat-sidebar-rpc] handshake response unparseable: {e}");
                return Err(CallError::NotConnected);
            }
        },
        Ok(Some(Ok(_other))) => return Err(CallError::NotConnected),
        Ok(Some(Err(e))) => {
            eprintln!("[chat-sidebar-rpc] handshake read failed: {e}");
            return Err(CallError::NotConnected);
        }
        Ok(None) => return Err(CallError::NotConnected),
        Err(_) => return Err(CallError::NotConnected),
    }

    let frame_text = rpc::build_request(CALL_ID, method, params);
    if let Err(e) = write.send(Message::Text(frame_text)).await {
        eprintln!("[chat-sidebar-rpc] request send failed: {e}");
        return Err(CallError::NotConnected);
    }

    // Read frames until the matching `res` arrives. A short-lived connection
    // like this one should never see an interleaved `event` push in
    // practice, but the loop tolerates one anyway (ignored, not fatal)
    // rather than assuming the very next frame is always the answer.
    loop {
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => match rpc::parse_frame(&text) {
                Ok(WsFrame::Response { id, ok, payload, error }) if id == CALL_ID => {
                    let _ = write.send(Message::Close(None)).await;
                    return if ok {
                        Ok(payload.unwrap_or(Value::Null))
                    } else {
                        Err(CallError::Rejected(error.unwrap_or(Value::Null)))
                    };
                }
                _ => continue,
            },
            Ok(Some(Ok(Message::Ping(data)))) => {
                if write.send(Message::Pong(data)).await.is_err() {
                    return Err(CallError::Disconnected);
                }
            }
            Ok(Some(Ok(_other))) => continue,
            Ok(Some(Err(e))) => {
                eprintln!("[chat-sidebar-rpc] read error: {e}");
                return Err(CallError::Disconnected);
            }
            Ok(None) => return Err(CallError::Disconnected),
            Err(_) => return Err(CallError::Timeout),
        }
    }
}

/// Issue one RPC call and return a receiver for its eventual result — the
/// ergonomic wrapper this module's callers (`conversations.rs`,
/// `agents_picker.rs`) actually use. Mirrors `ws_status::call`'s shape
/// exactly (same `oneshot` + fire-and-forget-if-the-thread-is-gone
/// contract), so a future migration onto that shared infrastructure is a
/// pure call-site rename, not a redesign.
pub fn call(
    cmd_tx: &tokio_mpsc::UnboundedSender<Command>,
    method: impl Into<String>,
    params: Value,
) -> oneshot::Receiver<Result<Value, CallError>> {
    let (respond_to, rx) = oneshot::channel();
    let _ = cmd_tx.send(Command::Call { method: method.into(), params, respond_to });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A call before any `Connect` has ever been sent must fail immediately
    /// with `NotConnected` — never hang waiting for a JWT that was never
    /// given.
    #[test]
    fn call_before_connect_is_not_connected() {
        let cmd_tx = spawn();
        let rx = call(&cmd_tx, "ping", serde_json::json!({}));
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), rx).await.unwrap().unwrap() });
        assert!(matches!(result, Err(CallError::NotConnected)), "expected NotConnected, got {result:?}");
    }

    /// Live path: a bogus JWT against the real local gateway must fail the
    /// `connect{jwt}` handshake the same way `ws_status.rs`'s sibling test
    /// proves for the main `/ws` — this module folds that into
    /// `CallError::NotConnected` (see `call_once`'s doc comment: no separate
    /// "auth failed" variant exists on this module's smaller `Command`
    /// surface). Requires a local gateway on 127.0.0.1:18789.
    #[test]
    fn call_with_bogus_jwt_against_live_gateway_is_not_connected() {
        let cmd_tx = spawn();
        cmd_tx.send(Command::Connect { jwt: "not-a-real-jwt".to_string() }).expect("manager thread alive");
        let rx = call(&cmd_tx, "agents.list", serde_json::json!({}));
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(8), rx)
                    .await
                    .expect("call_once must resolve, not hang, on a rejected handshake")
                    .expect("the manager thread must not drop the respond_to sender")
            });
        assert!(
            matches!(result, Err(CallError::NotConnected)),
            "expected NotConnected (rejected handshake) — is a gateway actually running on \
             127.0.0.1:18789? (curl :18789/healthz) got {result:?}"
        );
    }

    /// Spins up a minimal mock `/ws` server: accepts the `connect{jwt}`
    /// handshake unconditionally (replies `ok:true`), then replies to the
    /// next `req` with a real `res` payload. Proves the full round trip
    /// (handshake → request → matching response by id → close) parses a
    /// real network exchange correctly, independent of whether a live
    /// gateway with real credentials is available (same rationale as
    /// `chat_ws.rs`'s own `spawn_mock_chat_server`).
    async fn spawn_mock_rpc_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind mock server");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else { return };
            let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    assert!(text.contains(r#""method":"connect""#), "expected the connect handshake, got: {text}");
                }
                other => panic!("expected the client's connect handshake, got: {other:?}"),
            }
            let ok = r#"{"type":"res","id":"1","ok":true,"payload":{}}"#;
            if ws.send(Message::Text(ok.into())).await.is_err() {
                return;
            }
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    assert!(text.contains(r#""method":"agents.list""#), "expected the real request, got: {text}");
                }
                other => panic!("expected the client's actual request, got: {other:?}"),
            }
            let res = r#"{"type":"res","id":"2","ok":true,"payload":{"agents":[{"name":"mocky"}]}}"#;
            let _ = ws.send(Message::Text(res.into())).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        });
        format!("ws://{addr}")
    }

    /// Exercises `call_once` directly against the mock server (bypassing the
    /// hardcoded `WS_URL` constant via its `url` parameter — same test-seam
    /// pattern `ws_status.rs`/`chat_ws.rs` use for their own
    /// `connect_and_run`/`session_loop`) rather than through the full
    /// `spawn()`+background-thread path, since this is the narrowest seam
    /// that actually exercises the wire parsing end to end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_round_trip_against_mock_server() {
        let url = spawn_mock_rpc_server().await;
        let result = call_once(&url, "test-jwt", "agents.list", serde_json::json!({})).await;
        match result {
            Ok(payload) => assert_eq!(payload, serde_json::json!({"agents": [{"name": "mocky"}]})),
            other => panic!("expected Ok payload, got {other:?}"),
        }
    }
}
