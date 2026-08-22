// S2 — session/connection manager.
//
// Phase 1a shipped this file as a one-shot WS "smoke test" (see git history
// for that version). S2 turns it into the real thing: dispatches the
// login/local-session HTTP calls (`api.rs`) from a background thread and
// then owns a persistent, auto-reconnecting authenticated WS connection.
// Still the exact same background-OS-thread + tiny current-thread tokio
// runtime + channel-bridge architecture the original smoke test
// established (see `main.rs`'s module doc comment for why: gpui's own async
// executor is not a tokio context, so anything using tokio/reqwest/
// tokio-tungstenite has to run on a dedicated thread that owns its own
// runtime) — `main.rs` polls the event half via the same `try_recv` timer
// loop as before, just now driving a real state machine instead of a
// one-shot enum.
//
// Wire protocol source of truth: `web/src/lib/ws-client.ts` (`WsFrame`, the
// `connect { jwt }` handshake, the 1s→…→30s reconnect backoff shape),
// cross-checked against the actual server implementation
// (`duduclaw-gateway/src/server.rs::handle_socket` lines ~4623-4705,
// `protocol.rs::WsFrame`) — not guessed. The task brief's own backoff
// numbers (1s floor, 5s ceiling) differ from `ws-client.ts`'s (1s floor,
// 30s ceiling, 10 attempt cap); S2 follows the task brief's numbers and has
// no attempt cap (an authenticated desktop app session should keep trying
// to come back, not eventually give up silently — the cap in ws-client.ts
// is a browser-tab-specific concern that doesn't obviously apply here).

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use gpui::SharedString;
use tokio::sync::{mpsc as tokio_mpsc, oneshot, Mutex as TokioMutex};
use tokio_tungstenite::tungstenite::Message;

use crate::api::{self, AuthError, LoginResponse};
use crate::i18n::{self, Locale};
use crate::rpc::{self, CallError, PendingCalls};

const RECONNECT_FLOOR: Duration = Duration::from_secs(1);
const RECONNECT_CEILING: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// S4 task brief's own number for a `call()` round trip — deliberately
/// independent of `HANDSHAKE_TIMEOUT` (a stalled single RPC on an otherwise
/// live connection is a different failure mode than a stalled handshake).
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// Sender half of the per-connection outbound frame channel — `dispatch_call`
/// writes into this; `connect_and_run`'s select loop is the only reader,
/// forwarding straight to the socket's write half. `Option` because it only
/// exists while a connection is actually authenticated (see `active_writer`'s
/// own doc comment on `run`).
type OutboundTx = tokio_mpsc::UnboundedSender<Message>;

/// Commands the gpui foreground pushes into the background manager.
pub enum Command {
    /// Probe the Personal-edition passwordless local session once at
    /// startup (`POST /api/session/local`).
    TryLocalSession,
    /// Submit email/password from the login screen (`POST /api/login`).
    Login { email: String, password: String },
    /// Establish (or re-establish, aborting any prior connection/backoff
    /// loop) the authenticated WS session with this JWT.
    ConnectWs { jwt: String },
    /// S4: issue one RPC call (`{"type":"req",...}`) on the currently
    /// authenticated `/ws` connection. See [`call`] for the ergonomic
    /// wrapper callers should actually use instead of building this
    /// variant by hand.
    #[allow(dead_code)] // no S4 page calls this yet — new infrastructure for future RPC-backed pages, exercised by this file's own tests
    Call {
        method: String,
        params: serde_json::Value,
        respond_to: oneshot::Sender<Result<serde_json::Value, CallError>>,
    },
    /// WP-S5b2-F: one authenticated `GET /api/*` round trip, dispatched on
    /// this manager's own background tokio runtime (see [`api::get_json`]'s
    /// doc comment for why — `screens::files` is the first page needing a
    /// JSON REST read beyond login, and gpui's own executor is not a tokio
    /// context). `jwt` is threaded through explicitly by the caller rather
    /// than read from any state this manager tracks — this command is a
    /// transport-only bridge, not an auth-aware one.
    RestGet {
        path_and_query: String,
        jwt: Option<String>,
        respond_to: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// WP-C-M2: probe `GET <base_url>/healthz`, dispatched on this
    /// manager's own background tokio runtime for the same reason
    /// `RestGet` is (gpui's executor is not a tokio context). Used by
    /// `screens::gateway_picker` to validate a candidate URL before
    /// persisting/switching to it — deliberately independent of whatever
    /// gateway is CURRENTLY connected (`base_url` is caller-supplied, not
    /// read from `api::gateway_base_url()` here), so probing a candidate
    /// can never be confused with the live connection's own state.
    HealthCheck {
        base_url: String,
        respond_to: oneshot::Sender<(bool, Option<String>)>,
    },
}

/// Issue one RPC call and return a receiver for its eventual result.
/// Ergonomic wrapper around [`Command::Call`] — mints the oneshot channel,
/// sends the command, and hands back the receiving half for the caller to
/// `.await` (typically from inside a `cx.spawn` async block, the same
/// pattern `main.rs`'s poll loop already uses for background work).
///
/// Never blocks and never fails synchronously: if the manager thread is
/// gone the send is a silent no-op and the returned receiver simply never
/// resolves — callers that need a bounded wait should race this against
/// their own timer, or rely on the fact that a live manager always resolves
/// within `CALL_TIMEOUT` regardless of connection state (`NotConnected` is
/// returned immediately when there's no active connection; the 15s ceiling
/// only applies to a call that was actually sent).
#[allow(dead_code)] // no S4 page calls this yet — see `Command::Call`'s doc comment
pub fn call(
    session_tx: &tokio_mpsc::UnboundedSender<Command>,
    method: impl Into<String>,
    params: serde_json::Value,
) -> oneshot::Receiver<Result<serde_json::Value, CallError>> {
    let (respond_to, rx) = oneshot::channel();
    let _ = session_tx.send(Command::Call { method: method.into(), params, respond_to });
    rx
}

/// Issue one authenticated `GET /api/*` and return a receiver for its
/// eventual parsed-JSON result — the REST twin of [`call`]. Same
/// never-blocks-never-fails-synchronously contract: if the manager thread is
/// gone the send is a silent no-op and the receiver simply never resolves.
pub fn rest_get(
    session_tx: &tokio_mpsc::UnboundedSender<Command>,
    path_and_query: impl Into<String>,
    jwt: Option<String>,
) -> oneshot::Receiver<Result<serde_json::Value, String>> {
    let (respond_to, rx) = oneshot::channel();
    let _ = session_tx.send(Command::RestGet { path_and_query: path_and_query.into(), jwt, respond_to });
    rx
}

/// Probe a candidate gateway URL's `/healthz` and return a receiver for the
/// `(ok, error_detail)` result — the health-check twin of [`rest_get`]. Same
/// never-blocks-never-fails-synchronously contract: a dead manager thread
/// makes this a silent no-op and the receiver never resolves.
pub fn health_check(
    session_tx: &tokio_mpsc::UnboundedSender<Command>,
    base_url: impl Into<String>,
) -> oneshot::Receiver<(bool, Option<String>)> {
    let (respond_to, rx) = oneshot::channel();
    let _ = session_tx.send(Command::HealthCheck { base_url: base_url.into(), respond_to });
    rx
}

/// The connection state machine driving the shell sidebar's status dot.
/// Four states, matching the task brief's own vocabulary
/// ("disconnected/connecting/connected/authenticated").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsConnState {
    /// No socket open — either never connected yet, or dropped and waiting
    /// out the reconnect backoff.
    Disconnected,
    /// Dialing / awaiting the TCP+WS upgrade.
    Connecting,
    /// TCP+WS open; the `connect{jwt}` handshake request is in flight.
    Connected,
    /// The gateway accepted the JWT — RPCs are usable.
    Authenticated,
}

impl WsConnState {
    /// Dot color for the shell sidebar's connection indicator, using MDS's
    /// semantic tokens (same mapping the Phase 1a smoke test used: amber
    /// while anything short of a full authenticated handshake, green once
    /// authenticated).
    pub fn dot_color(self) -> u32 {
        match self {
            WsConnState::Disconnected | WsConnState::Connecting | WsConnState::Connected => {
                crate::theme::WARNING
            }
            WsConnState::Authenticated => crate::theme::SUCCESS,
        }
    }

    /// Short localized label shown next to the dot.
    pub fn short_label(self, locale: Locale) -> SharedString {
        match self {
            WsConnState::Disconnected => i18n::t(locale, "native.ws.reconnecting"),
            WsConnState::Connecting | WsConnState::Connected => {
                i18n::t(locale, "native.ws.connecting")
            }
            WsConnState::Authenticated => i18n::t(locale, "native.ws.connected"),
        }
    }
}

/// Events the background manager sends back to the gpui foreground.
// `ServerEvent` naming the enum twice (`Event::ServerEvent`) trips clippy's
// `enum_variant_names` — kept anyway: it's the clearest name for "an actual
// `{"type":"event",...}` wire frame" among this enum's siblings
// (`WsState`/`WsAuthFailed` name the CONNECTION's state, not a payload), and
// losing the "Event" word (e.g. `Push`) would read as importantly vaguer.
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub enum Event {
    LocalSessionOk(LoginResponse),
    LocalSessionUnavailable,
    LoginOk(LoginResponse),
    LoginErr(AuthError),
    WsState(WsConnState),
    /// The WS handshake rejected the JWT (`handle_socket`'s
    /// `"JWT authentication failed: {e}"` branch — the only rejection path
    /// a well-formed `connect{jwt}` request can hit). The reconnect loop
    /// has already stopped itself when this fires: retrying with a token
    /// the server just rejected would violate the task brief's "auth 失敗
    /// … 不無限重試" requirement. The UI must send a fresh `Login` (a new
    /// `ConnectWs` with a new JWT) to recover.
    WsAuthFailed,
    /// S4: a server-initiated `event` frame arrived on the authenticated
    /// `/ws` connection — the "另一條 event 通道往 UI" the task brief asks
    /// for, distinct from [`call`]'s request/response pairing (which
    /// resolves directly through its own oneshot, never through this
    /// channel). Nothing in S4 subscribes to any particular event name yet;
    /// `main.rs` logs it rather than silently dropping it — see that call
    /// site's comment for the explicit "no consumer yet" callout.
    ServerEvent { event: String, payload: serde_json::Value },
}

/// Spawn the background thread + its tokio runtime. Returns a command
/// sender for the gpui foreground to push work into, and an event receiver
/// to poll (same `try_recv` poll-loop pattern `main.rs` already used for
/// Phase 1a's smoke test).
pub fn spawn() -> (tokio_mpsc::UnboundedSender<Command>, std_mpsc::Receiver<Event>) {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();
    let (evt_tx, evt_rx) = std_mpsc::channel::<Event>();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[session] tokio runtime build failed: {e}");
                return;
            }
        };
        rt.block_on(run(cmd_rx, evt_tx));
    });

    (cmd_tx, evt_rx)
}

/// The manager's command loop. Each `TryLocalSession`/`Login` is dispatched
/// as an independent one-shot task (fire-and-forget — the HTTP call is
/// idempotent-ish and short-lived, no need to serialize them). `ConnectWs`
/// aborts any previously-running WS session task before starting a new
/// one, so a fresh login after a `WsAuthFailed` kick-back cleanly replaces
/// the old (dead) connection loop rather than running two in parallel.
async fn run(mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>, evt_tx: std_mpsc::Sender<Event>) {
    let mut ws_task: Option<tokio::task::JoinHandle<()>> = None;
    // S4: RPC state, shared with whichever `ws_session_loop`/`connect_and_run`
    // attempt is currently live. Created once here (not per-`ConnectWs`) so
    // it survives across the internal reconnect-with-backoff loop inside a
    // single `ws_session_loop` run — `connect_and_run` clears/rejects it
    // itself on every disconnect (see that function), not `run`.
    let pending: Arc<TokioMutex<PendingCalls>> = Arc::new(TokioMutex::new(PendingCalls::new()));
    let active_writer: Arc<TokioMutex<Option<OutboundTx>>> = Arc::new(TokioMutex::new(None));

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::TryLocalSession => {
                let evt_tx = evt_tx.clone();
                tokio::spawn(async move {
                    let event = match api::try_local_session().await {
                        Some(resp) => {
                            eprintln!("[session] local session probe: ok (user={})", resp.user.email);
                            Event::LocalSessionOk(resp)
                        }
                        None => {
                            eprintln!("[session] local session probe: unavailable — showing login form");
                            Event::LocalSessionUnavailable
                        }
                    };
                    let _ = evt_tx.send(event);
                });
            }
            Command::Login { email, password } => {
                let evt_tx = evt_tx.clone();
                tokio::spawn(async move {
                    let event = match api::login(&email, &password).await {
                        Ok(resp) => Event::LoginOk(resp),
                        Err(e) => Event::LoginErr(e),
                    };
                    let _ = evt_tx.send(event);
                });
            }
            Command::ConnectWs { jwt } => {
                if let Some(handle) = ws_task.take() {
                    handle.abort();
                }
                // A newly-superseded connection's own cleanup (inside
                // `connect_and_run`) already rejects its pending calls on a
                // normal disconnect, but `handle.abort()` above is a HARD
                // cancel — it can cut the old task off mid-flight without
                // ever reaching that cleanup code. Doing it here too makes
                // the reject-on-replace guarantee unconditional.
                pending.lock().await.reject_all_disconnected();
                *active_writer.lock().await = None;
                let evt_tx = evt_tx.clone();
                let pending = pending.clone();
                let active_writer = active_writer.clone();
                // WP-C-M2: resolved fresh at connect time (not a hardcoded
                // `WS_URL` const) — picks up whatever `api::set_gateway_
                // base_url` most recently set, so a gateway-picker switch
                // followed by a fresh login reconnects to the NEW target
                // without needing a second, separate "retarget" command.
                let ws_url = api::gateway_ws_url("/ws");
                ws_task = Some(tokio::spawn(ws_session_loop(ws_url, jwt, evt_tx, pending, active_writer)));
            }
            Command::Call { method, params, respond_to } => {
                dispatch_call(&pending, &active_writer, method, params, respond_to).await;
            }
            Command::RestGet { path_and_query, jwt, respond_to } => {
                tokio::spawn(async move {
                    let result = api::get_json(&api::gateway_base_url(), &path_and_query, jwt.as_deref())
                        .await
                        .map_err(|e| describe_rest_auth_error(&e));
                    let _ = respond_to.send(result);
                });
            }
            Command::HealthCheck { base_url, respond_to } => {
                tokio::spawn(async move {
                    let result = api::health_probe(&base_url).await;
                    let _ = respond_to.send(result);
                });
            }
        }
    }
}

/// User-facing-ish (still diagnostic, not localized — callers own the
/// `files.error`-style i18n key) description of a REST-layer [`AuthError`],
/// mirroring `screens::mcp::describe_call_error`'s role for the WS-RPC side.
fn describe_rest_auth_error(e: &AuthError) -> String {
    match e {
        AuthError::Unreachable => "無法連線到伺服器".to_string(),
        AuthError::InvalidCredentials => "沒有存取權限或憑證已過期".to_string(),
        AuthError::Other { status, detail } => match status {
            Some(s) => format!("HTTP {s}: {detail}"),
            None => detail.clone(),
        },
    }
}

/// Send one RPC request on the currently active connection, if any.
///
/// Deliberately does NOT queue a call made while disconnected — `call()`'s
/// doc comment already sets that expectation (`NotConnected` fires
/// immediately rather than waiting for a future reconnect the caller has no
/// visibility into). `CALL_TIMEOUT` is enforced by a detached watchdog task
/// rather than inline, so `dispatch_call` itself never blocks the command
/// loop that's driving every other `Command` too.
async fn dispatch_call(
    pending: &Arc<TokioMutex<PendingCalls>>,
    active_writer: &Arc<TokioMutex<Option<OutboundTx>>>,
    method: String,
    params: serde_json::Value,
    respond_to: oneshot::Sender<Result<serde_json::Value, CallError>>,
) {
    let Some(writer) = active_writer.lock().await.clone() else {
        let _ = respond_to.send(Err(CallError::NotConnected));
        return;
    };

    let id = {
        let mut p = pending.lock().await;
        let id = p.next_id();
        p.insert(id.clone(), respond_to);
        id
    };

    let frame_text = rpc::build_request(&id, &method, params);
    if writer.send(Message::Text(frame_text)).is_err() {
        // The writer channel is closed — the connection is already gone
        // (its own cleanup just hasn't run yet). Fail immediately rather
        // than making the caller wait out the full timeout for a socket
        // that's provably dead.
        if let Some(tx) = pending.lock().await.take(&id) {
            let _ = tx.send(Err(CallError::NotConnected));
        }
        return;
    }

    let pending = pending.clone();
    tokio::spawn(async move {
        tokio::time::sleep(CALL_TIMEOUT).await;
        pending.lock().await.timeout_one(&id);
    });
}

/// Outcome of one connect-and-run attempt, driving the reconnect loop's
/// next move.
enum ConnectOutcome {
    /// The server rejected the JWT — terminal, do not retry.
    AuthFailed,
    /// The socket never got past the handshake (connect failure, send
    /// failure, handshake timeout, ...). Retry with the current backoff,
    /// then grow it.
    DroppedBeforeAuth,
    /// The socket WAS fully authenticated at some point during this
    /// attempt, then dropped (network blip, server restart, ...). Retry
    /// immediately-ish, but reset backoff to the floor first — a
    /// previously-good connection means the far end is reachable, so
    /// there's no reason to make the next attempt wait as long as a
    /// never-connected one would.
    DroppedAfterAuth,
}

/// Persistent authenticated WS session with auto-reconnect. Runs until the
/// server rejects the JWT (terminal) or this task is aborted by a newer
/// `ConnectWs` command superseding it (see `run`'s `handle.abort()`).
///
/// `url` is a parameter (owned, not the hardcoded `WS_URL` constant this
/// used to be — see `run`'s `Command::ConnectWs` arm for why it must now be
/// resolved fresh per connect and therefore owned, not borrowed from a
/// `'static` const) so the reconnect/backoff behavior and the
/// auth-failure-is-terminal behavior are both directly testable — see the
/// tests at the bottom of this file, which call this function against the
/// real local gateway (auth-failure path) and against an unreachable port
/// (backoff-growth/ceiling path).
async fn ws_session_loop(
    url: String,
    jwt: String,
    evt_tx: std_mpsc::Sender<Event>,
    pending: Arc<TokioMutex<PendingCalls>>,
    active_writer: Arc<TokioMutex<Option<OutboundTx>>>,
) {
    let mut backoff = RECONNECT_FLOOR;

    loop {
        let _ = evt_tx.send(Event::WsState(WsConnState::Connecting));

        match connect_and_run(&url, &jwt, &evt_tx, &pending, &active_writer).await {
            ConnectOutcome::AuthFailed => {
                let _ = evt_tx.send(Event::WsAuthFailed);
                let _ = evt_tx.send(Event::WsState(WsConnState::Disconnected));
                return;
            }
            ConnectOutcome::DroppedAfterAuth => {
                backoff = RECONNECT_FLOOR;
                let _ = evt_tx.send(Event::WsState(WsConnState::Disconnected));
                tokio::time::sleep(backoff).await;
            }
            ConnectOutcome::DroppedBeforeAuth => {
                let _ = evt_tx.send(Event::WsState(WsConnState::Disconnected));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_CEILING);
            }
        }
    }
}

/// One connection attempt: dial, send the `connect{jwt}` handshake, wait
/// for the response, then — if authenticated — keep reading frames until
/// the socket drops.
async fn connect_and_run(
    url: &str,
    jwt: &str,
    evt_tx: &std_mpsc::Sender<Event>,
    pending: &Arc<TokioMutex<PendingCalls>>,
    active_writer: &Arc<TokioMutex<Option<OutboundTx>>>,
) -> ConnectOutcome {
    use futures_util::{SinkExt, StreamExt};

    let connect_fut = tokio_tungstenite::connect_async(url);
    let (ws_stream, _response) =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, connect_fut).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                eprintln!("[session] ws connect failed: {e}");
                return ConnectOutcome::DroppedBeforeAuth;
            }
            Err(_) => {
                eprintln!("[session] ws connect timed out ({HANDSHAKE_TIMEOUT:?})");
                return ConnectOutcome::DroppedBeforeAuth;
            }
        };

    let _ = evt_tx.send(Event::WsState(WsConnState::Connected));

    let (mut write, mut read) = ws_stream.split();

    // Wire format per `ws-client.ts`'s `doConnect()`: a JWT carries dots,
    // so the web client picks `{jwt: token}` over the legacy `{token}`
    // form — this client only ever holds JWTs, so it always sends `jwt`.
    // Cross-checked against the server's actual branch
    // (`server.rs::handle_socket`, `params.get("jwt")`).
    let req = serde_json::json!({
        "type": "req",
        "id": "1",
        "method": "connect",
        "params": { "jwt": jwt },
    });
    if let Err(e) = write.send(Message::Text(req.to_string())).await {
        eprintln!("[session] handshake send failed: {e}");
        return ConnectOutcome::DroppedBeforeAuth;
    }

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) if v.get("ok").and_then(|b| b.as_bool()) == Some(true) => {
                let _ = evt_tx.send(Event::WsState(WsConnState::Authenticated));
            }
            Ok(_) => {
                // `ok: false` on this specific handshake response has
                // exactly one cause server-side — see `handle_socket`'s
                // `Err(e) => WsFrame::error_response(&id, "JWT
                // authentication failed: {e}")` branch, the only failure
                // path for a well-formed `connect{jwt}` request.
                eprintln!("[session] ws handshake rejected: {text}");
                return ConnectOutcome::AuthFailed;
            }
            Err(e) => {
                eprintln!("[session] ws handshake response unparseable: {e}");
                return ConnectOutcome::DroppedBeforeAuth;
            }
        },
        Ok(Some(Ok(_other))) => {
            eprintln!("[session] ws handshake: unexpected non-text frame");
            return ConnectOutcome::DroppedBeforeAuth;
        }
        Ok(Some(Err(e))) => {
            eprintln!("[session] ws handshake read failed: {e}");
            return ConnectOutcome::DroppedBeforeAuth;
        }
        Ok(None) => {
            eprintln!("[session] ws closed before handshake response");
            return ConnectOutcome::DroppedBeforeAuth;
        }
        Err(_) => {
            eprintln!("[session] ws handshake response timed out ({HANDSHAKE_TIMEOUT:?})");
            return ConnectOutcome::DroppedBeforeAuth;
        }
    }

    // Authenticated — S4: drive both directions. `out_rx` carries outbound
    // `req` frames `dispatch_call` builds (registered into `active_writer`
    // below so `run`'s command loop can reach THIS connection specifically,
    // even though it's nested inside `ws_session_loop`'s own internal
    // reconnect loop); `read` carries everything the server sends —
    // `res` frames resolve pending calls, `event` frames forward to the UI
    // event channel (see `Event::ServerEvent`'s doc comment), and a `Ping`
    // gets an explicit `Pong` (S2 relied on tokio-tungstenite's implicit
    // auto-reply, which only applies to the *unsplit* socket — once
    // `.split()` separates the read/write halves, as this function already
    // did in S2, nothing answers a Ping without this arm).
    let (out_tx, mut out_rx) = tokio_mpsc::unbounded_channel::<Message>();
    *active_writer.lock().await = Some(out_tx);

    let outcome = loop {
        tokio::select! {
            incoming = read.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match rpc::parse_frame(&text) {
                            Ok(rpc::WsFrame::Response { id, ok, payload, error }) => {
                                pending.lock().await.resolve(&id, ok, payload, error);
                            }
                            Ok(rpc::WsFrame::Event { event, payload, .. }) => {
                                let _ = evt_tx.send(Event::ServerEvent { event, payload });
                            }
                            Ok(rpc::WsFrame::Request { .. }) => {
                                // The server never sends `req` frames to a
                                // client — ignore rather than panic on a
                                // wire shape this side should never see.
                            }
                            Err(e) => {
                                eprintln!("[session] ws frame unparseable: {e} (text: {text})");
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if write.send(Message::Pong(data)).await.is_err() {
                            break ConnectOutcome::DroppedAfterAuth;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => break ConnectOutcome::DroppedAfterAuth,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        eprintln!("[session] ws read error: {e}");
                        break ConnectOutcome::DroppedAfterAuth;
                    }
                }
            }
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(msg) => {
                        if write.send(msg).await.is_err() {
                            break ConnectOutcome::DroppedAfterAuth;
                        }
                    }
                    // `active_writer` always holds a live clone of `out_tx`
                    // while this loop runs, so `recv()` cannot observe
                    // "all senders dropped" here — unreachable in practice,
                    // handled anyway rather than `unreachable!()`ing on a
                    // wire-adjacent code path.
                    None => break ConnectOutcome::DroppedAfterAuth,
                }
            }
        }
    };

    // This connection is done (whichever way) — it owns `active_writer`
    // only while it's the live attempt, and any call still pending against
    // it can never get a real answer now. Both `run`'s `ConnectWs` handler
    // and this cleanup end up idempotent-safe if they ever race (the second
    // one just finds nothing to clear).
    *active_writer.lock().await = None;
    pending.lock().await.reject_all_disconnected();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port nothing is listening on (mirrors `api.rs`'s test convention).
    const UNREACHABLE_WS_URL: &str = "ws://127.0.0.1:1/ws";

    /// Live path: send an obviously-invalid JWT to the real local gateway's
    /// `/ws` endpoint and confirm the FULL round trip the task brief asks
    /// for — connecting → connected → the server's real "JWT authentication
    /// failed" rejection → terminal (no further reconnect attempts, per
    /// `WsAuthFailed`'s doc comment / the "auth 失敗 … 不無限重試"
    /// requirement). Requires a local gateway on 127.0.0.1:18789 — same
    /// caveat as `api.rs`'s live-gateway tests.
    #[test]
    fn ws_auth_failure_against_live_gateway_is_terminal() {
        let (cmd_tx, evt_rx) = spawn();
        cmd_tx
            .send(Command::ConnectWs {
                jwt: "not-a-real-jwt".to_string(),
            })
            .expect("manager thread should still be alive");

        let mut saw_connecting = false;
        let mut saw_connected = false;
        let mut saw_auth_failed = false;
        let mut saw_disconnected_after = false;

        // Drain events for up to 8s (well over the 5s handshake timeout) —
        // enough to observe the whole sequence once.
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            match evt_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::WsState(WsConnState::Connecting)) => saw_connecting = true,
                Ok(Event::WsState(WsConnState::Connected)) => saw_connected = true,
                Ok(Event::WsAuthFailed) => saw_auth_failed = true,
                Ok(Event::WsState(WsConnState::Disconnected)) if saw_auth_failed => {
                    saw_disconnected_after = true;
                    break; // the terminal sequence is complete
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
            "expected WsAuthFailed — is the live gateway really rejecting this JWT? \
             (curl :18789/healthz to confirm a gateway is up)"
        );
        assert!(saw_disconnected_after, "expected a final Disconnected state after WsAuthFailed");

        // Terminal: no further events (specifically no second `Connecting`,
        // which would mean it kept retrying a token it already knows is bad).
        match evt_rx.recv_timeout(Duration::from_secs(2)) {
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            other => panic!("expected silence after the terminal AuthFailed, got {other:?}"),
        }
    }

    /// Backoff shape against an unreachable port: the gap between
    /// consecutive `Disconnected` events must grow (never shrink) and must
    /// never exceed `RECONNECT_CEILING` + a scheduling-jitter tolerance —
    /// i.e. the task brief's "斷線自動重連（穩態 ≤5s）" acceptance criterion.
    /// Bounded to ~11s so the suite stays reasonably fast.
    ///
    /// `multi_thread`/2 workers is load-bearing here, not decoration: the
    /// test body's `evt_rx.recv_timeout` is a BLOCKING call, and
    /// `ws_session_loop` is spawned as a real concurrent task rather than
    /// polled inline specifically so its `tokio::time::sleep`/connect
    /// attempts keep making progress on the other worker while this task
    /// blocks. A single-threaded runtime would deadlock here (the one OS
    /// thread stuck in `recv_timeout` could never also drive the spawned
    /// task's timers).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconnect_backoff_grows_and_caps_against_unreachable_host() {
        let (evt_tx, evt_rx) = std_mpsc::channel::<Event>();
        let pending = Arc::new(TokioMutex::new(PendingCalls::new()));
        let active_writer = Arc::new(TokioMutex::new(None));
        let handle = tokio::spawn(ws_session_loop(
            UNREACHABLE_WS_URL.to_string(),
            "x".to_string(),
            evt_tx,
            pending,
            active_writer,
        ));

        let start = std::time::Instant::now();
        let deadline = start + Duration::from_secs(11);
        let mut disconnected_at = Vec::new();
        while std::time::Instant::now() < deadline && disconnected_at.len() < 4 {
            match evt_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::WsState(WsConnState::Disconnected)) => {
                    disconnected_at.push(start.elapsed());
                }
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

        // Gaps must be non-decreasing (backoff never shrinks) and never
        // exceed the ceiling by more than a small jitter allowance.
        let tolerance = Duration::from_millis(600);
        let mut prev_gap = Duration::from_millis(0);
        for pair in disconnected_at.windows(2) {
            let gap = pair[1].saturating_sub(pair[0]);
            assert!(
                gap + tolerance >= prev_gap,
                "backoff shrank: previous gap {prev_gap:?}, this gap {gap:?} \
                 (full sequence: {disconnected_at:?})"
            );
            assert!(
                gap <= RECONNECT_CEILING + tolerance,
                "steady-state reconnect gap exceeded the {RECONNECT_CEILING:?} ceiling: {gap:?} \
                 (full sequence: {disconnected_at:?})"
            );
            prev_gap = gap;
        }
    }

    /// S4: a `call()` issued before any `ConnectWs` has ever succeeded must
    /// fail immediately with `NotConnected` — never hang waiting for a
    /// connection that was never asked for, never silently queue.
    #[test]
    fn call_before_any_connection_is_not_connected() {
        let (cmd_tx, _evt_rx) = spawn();
        let rx = call(&cmd_tx, "ping", serde_json::json!({}));

        // A tiny current-thread runtime just to `.await` the oneshot —
        // mirrors how a real caller (a `cx.spawn` block in `main.rs`) would
        // consume it, without dragging gpui into this test.
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(2), rx)
                    .await
                    .expect("dispatch_call must resolve NotConnected immediately, not hang")
                    .expect("the manager thread must not drop the respond_to sender")
            });

        assert!(
            matches!(result, Err(CallError::NotConnected)),
            "expected NotConnected, got {result:?}"
        );
    }

    /// Live path: authenticate against the real local gateway with a bogus
    /// JWT (same setup as `ws_auth_failure_against_live_gateway_is_terminal`)
    /// and confirm a `call()` issued right after the terminal `WsAuthFailed`
    /// also resolves `NotConnected` — the RPC layer must not be left with a
    /// stale "connected" writer once the connection that owned it is gone.
    /// A genuine authenticated round trip (e.g. a real `ping` → `pong`)
    /// needs a valid JWT this test suite has no way to mint — see this
    /// crate's S4 report for the explicit "unverified, needs a human with
    /// real credentials" callout.
    #[test]
    fn call_after_terminal_auth_failure_is_not_connected() {
        let (cmd_tx, evt_rx) = spawn();
        cmd_tx
            .send(Command::ConnectWs { jwt: "not-a-real-jwt".to_string() })
            .expect("manager thread should still be alive");

        // Drain until WsAuthFailed (terminal) — same wait shape as the
        // sibling test above, just without re-asserting every intermediate
        // state.
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut saw_auth_failed = false;
        while std::time::Instant::now() < deadline && !saw_auth_failed {
            match evt_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::WsAuthFailed) => saw_auth_failed = true,
                Ok(_) => {}
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_auth_failed, "expected the live gateway to reject this JWT (see sibling test)");

        let rx = call(&cmd_tx, "ping", serde_json::json!({}));
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(2), rx).await.unwrap().unwrap()
            });
        assert!(matches!(result, Err(CallError::NotConnected)), "expected NotConnected, got {result:?}");
    }
}
