// One-shot WS RPC round trip — Shell-S4 (2026-08-22). The gateway's
// `approvals.list`/`approvals.decide` RPCs exist ONLY on `/ws`
// (`duduclaw-gateway/src/server.rs`'s route table has no REST equivalent for
// either — grepped before writing this module, not assumed; see
// `Cargo.toml`'s own comment on this dependency block for the same finding
// spelled out for the dependency decision it drove).
//
// ── Why `tokio`/`tokio-tungstenite` are used here, unlike `oobe/claim.rs`'s
// hand-rolled HTTP client ──────────────────────────────────────────────
// WebSocket framing (RFC 6455: client-to-server masking, the
// Sec-WebSocket-Accept SHA-1 handshake, fragmented frames, ping/pong/close)
// is a materially bigger surface to hand-roll correctly than plain HTTP/1.1
// request/response framing — the risk of a subtly wrong hand-rolled
// implementation is real, not hypothetical, unlike `claim.rs`'s few dozen
// lines. Before reaching for the dependency, this crate's resolved graph was
// checked (`cargo tree -p duduclaw-shell -i tokio-tungstenite` etc.): all
// three crates below are ALREADY compiled, via `duduclaw-native-gui` (a
// `path` dependency of this crate — see `Cargo.toml`) using them for its own
// persistent `/ws` session (`ws_status.rs`). See `Cargo.toml`'s own comment
// on the `[dependencies.tokio]` block for the full finding.
//
// ── Why ONE-SHOT, not `ws_status.rs`'s persistent-connection pattern ────
// `ws_status.rs` owns a long-lived reconnecting connection with a full state
// machine (backoff, a live connection-state enum, a background read loop
// dispatching into a pending-calls registry) because ITS caller (chat/
// session UI) needs a connection that is ALREADY authenticated and live the
// instant a page wants to send something, and receives server-pushed
// events. This module's only callers (`approvals::list_approvals`/
// `::decide_approval`) are poll-triggered, not event-driven — panel-open
// plus a periodic timer, see `overlay::notifications_feed`'s own header
// comment — so there is no "always-on" connection worth keeping warm
// between calls, and nothing here ever needs to receive an unsolicited
// server push. Each call dials fresh, authenticates, sends exactly one
// request, awaits exactly the matching response, and closes — a
// dramatically smaller surface than a persistent-connection manager, at the
// cost of one extra WS handshake per call (single-digit ms on loopback,
// irrelevant at this module's actual call cadence).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_WS_URL: &str = "ws://127.0.0.1:18789/ws";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// A stray `event` frame is possible in principle even on a connection this
/// short-lived (nothing here subscribes to anything, but the wire protocol
/// doesn't forbid the server from pushing one). Bounded, not unbounded, so a
/// misbehaving peer can't wedge this call forever inside the loop below —
/// same "circuit breaker on every loop" discipline this codebase's own
/// operating rules ask for.
const MAX_FRAMES_BEFORE_GIVING_UP: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    Unreachable(String),
    /// The `connect{jwt}` handshake itself was rejected — the JWT is
    /// invalid or expired. The caller's cue to bootstrap a fresh session
    /// (`gateway_client::bootstrap_local_session`) rather than retry the
    /// same token.
    AuthRejected,
    /// The request-specific `res` frame carried `ok:false` — a server-side
    /// business-logic rejection (e.g. "id is required"), NOT an auth
    /// problem. Carries the `error` payload's JSON text for a diagnostic.
    Rejected(String),
    Timeout,
    Malformed(String),
}

/// Derives the WS URL from the SAME `DUDUCLAW_SHELL_GATEWAY_URL` env var
/// `gateway_client::session::bootstrap_local_session`'s HTTP client reads
/// (`http://host:port` -> `ws://host:port/ws`) — the gateway's WS and REST
/// surfaces are always the same host:port (one axum router, see
/// `duduclaw-gateway/src/server.rs`'s route table), so a caller overriding
/// the port for `/api/session/local` (e.g. this file's own live test, or an
/// operator running the gateway on a non-default port) must not leave `/ws`
/// silently pointed at the OLD default port. Falls back to
/// `DEFAULT_WS_URL` when the env var is unset/empty, matching `session::
/// gateway_base_url`'s own fallback behavior exactly.
fn ws_url() -> String {
    let base = std::env::var("DUDUCLAW_SHELL_GATEWAY_URL").ok().filter(|v| !v.trim().is_empty());
    match base {
        None => DEFAULT_WS_URL.to_string(),
        Some(base) => match base.strip_prefix("http://") {
            Some(rest) => format!("ws://{rest}/ws"),
            // Not `http://` at all — `session.rs`'s own loopback gate would
            // already have refused this before ever reaching here in
            // practice; falling back to the default rather than producing a
            // malformed `ws://` URL is the fail-safe choice.
            None => DEFAULT_WS_URL.to_string(),
        },
    }
}

/// Synchronous entry point — spins up a throwaway current-thread tokio
/// runtime for exactly this one round trip and tears it down on return. See
/// this file's header comment for why a fresh runtime per call, not a
/// shared background one, is the right tradeoff at this call frequency.
/// Callers (this crate's own convention, see `oobe/steps/account.rs`) run
/// this from a `std::thread::spawn`, never from gpui's own executor.
pub(crate) fn call_once(jwt: &str, method: &str, params: Value) -> Result<Value, RpcError> {
    call_once_at(&ws_url(), jwt, method, params)
}

pub(crate) fn call_once_at(url: &str, jwt: &str, method: &str, params: Value) -> Result<Value, RpcError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| RpcError::Unreachable(format!("failed to start local async runtime: {e}")))?;
    rt.block_on(call_once_async(url, jwt, method, params))
}

async fn call_once_async(url: &str, jwt: &str, method: &str, params: Value) -> Result<Value, RpcError> {
    let connect_fut = tokio_tungstenite::connect_async(url);
    let (ws_stream, _response) = match tokio::time::timeout(HANDSHAKE_TIMEOUT, connect_fut).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => return Err(RpcError::Unreachable(e.to_string())),
        Err(_) => return Err(RpcError::Timeout),
    };
    let (mut write, mut read) = ws_stream.split();

    // Handshake — wire shape copied field-for-field from
    // `duduclaw-gateway/src/server.rs`'s `handle_socket` (the
    // `method == "connect"`, `params.get("jwt")` branch) and cross-checked
    // against `duduclaw-native-gui/src/ws_status.rs`'s own identical
    // `connect{jwt}` frame — not guessed.
    let connect_req = serde_json::json!({
        "type": "req", "id": "connect", "method": "connect", "params": { "jwt": jwt },
    });
    write.send(Message::Text(connect_req.to_string())).await.map_err(|e| RpcError::Unreachable(e.to_string()))?;

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            let v: Value = serde_json::from_str(&text).map_err(|e| RpcError::Malformed(format!("handshake response was not valid JSON: {e}")))?;
            if v.get("ok").and_then(Value::as_bool) != Some(true) {
                return Err(RpcError::AuthRejected);
            }
        }
        Ok(Some(Ok(_))) => return Err(RpcError::Malformed("handshake response was not a text frame".to_string())),
        Ok(Some(Err(e))) => return Err(RpcError::Unreachable(e.to_string())),
        Ok(None) => return Err(RpcError::Unreachable("connection closed during handshake".to_string())),
        Err(_) => return Err(RpcError::Timeout),
    }

    // The real request — fixed id "call", same reasoning as "connect"
    // above: this connection only ever sends these two frames in its whole
    // lifetime, so there is no need for `duduclaw-native-gui/src/rpc.rs`'s
    // full `PendingCalls` id-minting machinery.
    let req = serde_json::json!({ "type": "req", "id": "call", "method": method, "params": params });
    write.send(Message::Text(req.to_string())).await.map_err(|e| RpcError::Unreachable(e.to_string()))?;

    let outcome = 'frames: {
        for _ in 0..MAX_FRAMES_BEFORE_GIVING_UP {
            match tokio::time::timeout(CALL_TIMEOUT, read.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let v: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => break 'frames Err(RpcError::Malformed(format!("response was not valid JSON: {e}"))),
                    };
                    let is_our_response = v.get("type").and_then(Value::as_str) == Some("res") && v.get("id").and_then(Value::as_str) == Some("call");
                    if !is_our_response {
                        // Not this call's frame (e.g. an unsolicited
                        // `event`) — keep waiting, still inside the bounded
                        // frame count above.
                        continue;
                    }
                    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    break 'frames if ok {
                        Ok(v.get("payload").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(RpcError::Rejected(v.get("error").map(ToString::to_string).unwrap_or_default()))
                    };
                }
                Ok(Some(Ok(_))) => continue, // non-text frame (ping/pong/binary) — not our answer, keep waiting.
                Ok(Some(Err(e))) => break 'frames Err(RpcError::Unreachable(e.to_string())),
                Ok(None) => break 'frames Err(RpcError::Unreachable("connection closed while awaiting response".to_string())),
                Err(_) => break 'frames Err(RpcError::Timeout),
            }
        }
        Err(RpcError::Malformed("too many unrelated frames before this call's response arrived".to_string()))
    };

    // Best-effort close — the outcome above is already decided either way;
    // a failure to close cleanly must never override it.
    let _ = write.close().await;
    outcome
}

#[cfg(test)]
mod tests {
    // `SinkExt`/`StreamExt` (for `.send()`/`.next()` in the mock server
    // handlers below) already reach this scope via `use super::*` — the
    // parent module's own `use futures_util::{SinkExt, StreamExt};` at the
    // top of this file.
    use super::*;

    /// Spins up a minimal real WS server for exactly ONE connection, on its
    /// OWN OS thread with its OWN tiny tokio runtime — deliberately NOT
    /// `#[tokio::test]` on the caller side: `call_once_at` is a
    /// SYNCHRONOUS function that builds and `block_on`s its own runtime
    /// internally (see this file's header comment on why — a fresh
    /// current-thread runtime per call), and calling a `block_on`-ing
    /// function from a thread that is already inside a tokio runtime
    /// panics ("Cannot start a runtime from within a runtime"). Every test
    /// below is therefore a plain `#[test]` calling `call_once_at` directly
    /// on the MAIN test thread — exactly the shape production code uses
    /// (`call_once`/`call_once_at` is always invoked from a bare
    /// `std::thread::spawn`, per `oobe/steps/account.rs`'s established
    /// pattern, never from inside an async context) — while the mock
    /// server lives on a SEPARATE thread with its own independent runtime,
    /// the two talking over a real loopback socket the same way the real
    /// client and the real gateway do. Multiple independent tokio runtimes
    /// on different OS threads is fine; nesting one inside another on the
    /// SAME thread is what panics.
    fn start_mock_ws_server<F, Fut>(handler: F) -> String
    where
        F: FnOnce(tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build mock server runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind ephemeral port");
                let addr = listener.local_addr().expect("local_addr");
                let _ = addr_tx.send(addr);
                if let Ok((stream, _)) = listener.accept().await {
                    if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                        handler(ws).await;
                    }
                }
            });
        });
        let addr = addr_rx.recv_timeout(Duration::from_secs(3)).expect("mock server never bound a port");
        format!("ws://{addr}/ws")
    }

    #[test]
    fn happy_path_returns_the_payload() {
        let url = start_mock_ws_server(|mut ws| async move {
            // connect{jwt} -> ok
            let _ = ws.next().await;
            let _ = ws.send(Message::Text(r#"{"type":"res","id":"connect","ok":true,"payload":{"status":"authenticated"}}"#.into())).await;
            // the real call -> ok with a payload
            let _ = ws.next().await;
            let _ = ws.send(Message::Text(r#"{"type":"res","id":"call","ok":true,"payload":{"approvals":[]}}"#.into())).await;
        });

        let result = call_once_at(&url, "jwt-abc", "approvals.list", serde_json::json!({}));
        assert_eq!(result, Ok(serde_json::json!({"approvals": []})));
    }

    #[test]
    fn auth_rejected_short_circuits_before_the_real_call() {
        let url = start_mock_ws_server(|mut ws| async move {
            let _ = ws.next().await;
            let _ = ws.send(Message::Text(r#"{"type":"res","id":"connect","ok":false,"error":"bad jwt"}"#.into())).await;
            // No second frame expected — if this client mistakenly sent the
            // real call anyway, this handler simply never answers it and
            // the client's own timeout (not a hang) would surface that.
        });

        let result = call_once_at(&url, "stale-jwt", "approvals.list", serde_json::json!({}));
        assert_eq!(result, Err(RpcError::AuthRejected));
    }

    #[test]
    fn server_rejection_maps_to_rejected_with_the_error_payload() {
        let url = start_mock_ws_server(|mut ws| async move {
            let _ = ws.next().await;
            let _ = ws.send(Message::Text(r#"{"type":"res","id":"connect","ok":true}"#.into())).await;
            let _ = ws.next().await;
            let _ = ws.send(Message::Text(r#"{"type":"res","id":"call","ok":false,"error":"id is required"}"#.into())).await;
        });

        let result = call_once_at(&url, "jwt-abc", "approvals.decide", serde_json::json!({}));
        assert!(matches!(result, Err(RpcError::Rejected(_))), "{result:?}");
    }

    #[test]
    fn a_stray_event_frame_before_the_real_response_is_skipped_not_treated_as_the_answer() {
        let url = start_mock_ws_server(|mut ws| async move {
            let _ = ws.next().await;
            let _ = ws.send(Message::Text(r#"{"type":"res","id":"connect","ok":true}"#.into())).await;
            let _ = ws.next().await;
            // Unsolicited event first, THEN the real response.
            let _ = ws.send(Message::Text(r#"{"type":"event","event":"agent.started","payload":{}}"#.into())).await;
            let _ = ws.send(Message::Text(r#"{"type":"res","id":"call","ok":true,"payload":{"ok":true}}"#.into())).await;
        });

        let result = call_once_at(&url, "jwt-abc", "approvals.decide", serde_json::json!({"id":"a1","approve":true}));
        assert_eq!(result, Ok(serde_json::json!({"ok": true})));
    }

    #[test]
    fn connection_refused_maps_to_unreachable() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build runtime");
        let addr = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
            listener.local_addr().expect("local_addr")
            // `listener` dropped here — port very likely still refusing by the time `call_once_at` dials.
        });
        drop(rt);
        let result = call_once_at(&format!("ws://{addr}/ws"), "jwt-abc", "approvals.list", serde_json::json!({}));
        assert!(matches!(result, Err(RpcError::Unreachable(_))), "{result:?}");
    }

    #[test]
    fn malformed_handshake_response_is_reported_not_panicked_on() {
        let url = start_mock_ws_server(|mut ws| async move {
            let _ = ws.next().await;
            let _ = ws.send(Message::Text("this is not json".into())).await;
        });

        let result = call_once_at(&url, "jwt-abc", "approvals.list", serde_json::json!({}));
        assert!(matches!(result, Err(RpcError::Malformed(_))), "{result:?}");
    }

    // `set_var`/`remove_var` are process-global and `unsafe` on this
    // toolchain — same `ENV_LOCK`-guarded discipline `oobe/claim.rs`'s own
    // env-mutating tests already establish (serialize via a local mutex,
    // always restore the prior value on the way out).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn ws_url_derives_from_the_same_env_var_session_rs_uses() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_SHELL_GATEWAY_URL").ok();
        unsafe { std::env::set_var("DUDUCLAW_SHELL_GATEWAY_URL", "http://127.0.0.1:28793") };

        let url = ws_url();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_SHELL_GATEWAY_URL", v),
                None => std::env::remove_var("DUDUCLAW_SHELL_GATEWAY_URL"),
            }
        }

        assert_eq!(url, "ws://127.0.0.1:28793/ws");
    }

    #[test]
    fn ws_url_falls_back_to_the_default_when_the_env_var_is_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_SHELL_GATEWAY_URL").ok();
        unsafe { std::env::remove_var("DUDUCLAW_SHELL_GATEWAY_URL") };

        let url = ws_url();

        unsafe {
            if let Some(v) = prev {
                std::env::set_var("DUDUCLAW_SHELL_GATEWAY_URL", v);
            }
        }

        assert_eq!(url, DEFAULT_WS_URL);
    }
}
