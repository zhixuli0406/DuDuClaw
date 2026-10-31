//! One socket actor owns credentials, correlation, refresh and reconnect.
use crate::{
    Error, Result,
    auth::{Account, AuthClient, Role, Session},
    sanitize::TerminalText,
};
use duduclaw_gateway::protocol::WsFrame;
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc, oneshot, watch},
    task::AbortHandle,
    time::{Instant, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use zeroize::Zeroizing;

// Bound wire allocations and queued parsed events independently of list budgets.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const EVENT_QUEUE_CAPACITY: usize = 16;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub request_timeout: Duration,
    pub refresh_margin: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}
impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(15),
            refresh_margin: Duration::from_secs(60),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
        }
    }
}
#[derive(Clone)]
pub struct ConnectionInfo {
    pub generation: u64,
    pub user_id: String,
    pub role: Role,
    pub boot_id: Option<String>,
    pub version: Option<TerminalText>,
    pub autonomy: Option<TerminalText>,
}
#[derive(Clone)]
pub enum ConnectionState {
    Connecting,
    Connected(ConnectionInfo),
    Disconnected,
    LoginRequired(Error),
}
#[derive(Clone)]
pub struct ServerEvent {
    pub name: String,
    pub payload: Value,
    pub seq: Option<u64>,
    pub boot_id: Option<String>,
}
struct Command {
    method: String,
    params: Value,
    deadline: Instant,
    reply: oneshot::Sender<Result<Value>>,
}
struct Inner {
    commands: mpsc::Sender<Command>,
    status: watch::Receiver<ConnectionState>,
    events: broadcast::Sender<ServerEvent>,
    abort: AbortHandle,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.abort.abort();
    }
}
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}
impl Client {
    /// Dropping the last handle cancels the actor and destroys its tokens.
    pub fn start(auth: AuthClient, session: Session, options: ClientOptions) -> Result<Self> {
        if options.request_timeout.is_zero()
            || options.backoff_base.is_zero()
            || options.backoff_max < options.backoff_base
        {
            return Err(Error::Protocol);
        }
        let (tx, rx) = mpsc::channel(64);
        let (status_tx, status_rx) = watch::channel(ConnectionState::Connecting);
        let (events, _) = broadcast::channel(EVENT_QUEUE_CAPACITY);
        let actor = Actor {
            auth,
            session,
            options,
            rx,
            status: status_tx,
            events: events.clone(),
            next_id: 0,
            generation: 0,
        };
        let task = tokio::spawn(crate::without_tracing(actor.run()));
        Ok(Self {
            inner: Arc::new(Inner {
                commands: tx,
                status: status_rx,
                events,
                abort: task.abort_handle(),
            }),
        })
    }
    pub fn connection(&self) -> watch::Receiver<ConnectionState> {
        self.inner.status.clone()
    }
    /// A Lagged error means local event loss: consumers must invalidate all state.
    pub fn events(&self) -> broadcast::Receiver<ServerEvent> {
        self.inner.events.subscribe()
    }
    pub async fn call(&self, method: &str, params: Value, duration: Duration) -> Result<Value> {
        // Credentials can only travel through the actor's private handshake.
        if matches!(method, "connect" | "connect.challenge") {
            return Err(Error::RpcRejected);
        }
        if !matches!(*self.inner.status.borrow(), ConnectionState::Connected(_)) {
            return Err(Error::Disconnected);
        }
        let deadline = Instant::now() + duration;
        let (tx, rx) = oneshot::channel();
        self.inner
            .commands
            .try_send(Command {
                method: method.to_owned(),
                params,
                deadline,
                reply: tx,
            })
            .map_err(|_| Error::Busy)?;
        tokio::time::timeout_at(deadline, rx)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Disconnected)?
    }
}
struct Actor {
    auth: AuthClient,
    session: Session,
    options: ClientOptions,
    rx: mpsc::Receiver<Command>,
    status: watch::Sender<ConnectionState>,
    events: broadcast::Sender<ServerEvent>,
    next_id: u64,
    generation: u64,
}
#[derive(Clone, Copy)]
enum End {
    Reconnect,
    Refresh,
    Login(Error),
    Shutdown,
}
impl Actor {
    async fn run(mut self) {
        let mut attempt = 0;
        let mut refresh_attempted = false;
        loop {
            if self.session.refresh_in(Duration::ZERO).is_zero()
                && self.auth.refresh(&mut self.session).await.is_err()
            {
                self.login_required(Error::LoginRequired);
                return;
            }
            self.status.send_replace(ConnectionState::Connecting);
            let opened = timeout(self.options.request_timeout, self.open()).await;
            let end = match opened {
                Ok(Ok((mut socket, info))) => {
                    self.generation = info.generation;
                    self.status.send_replace(ConnectionState::Connected(info));
                    refresh_attempted = false;
                    let connected_at = Instant::now();
                    let end = self.serve(&mut socket).await;
                    // Close before the next socket is opened. Bound the close so a
                    // stalled peer cannot stop refresh or application shutdown.
                    let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                    if connected_at.elapsed() >= Duration::from_secs(30) {
                        attempt = 0;
                    }
                    end
                }
                Ok(Err(Error::LoginRequired)) if !refresh_attempted => {
                    refresh_attempted = true;
                    End::Refresh
                }
                Ok(Err(
                    e @ (Error::LoginRequired
                    | Error::LegacyIdentity
                    | Error::IdentityChanged
                    | Error::PasswordChangeRequired),
                )) => End::Login(e),
                _ => End::Reconnect,
            };
            self.status.send_replace(ConnectionState::Disconnected);
            while let Ok(command) = self.rx.try_recv() {
                let _ = command.reply.send(Err(Error::Disconnected));
            }
            match end {
                End::Shutdown => return,
                End::Login(error) => {
                    self.login_required(error);
                    return;
                }
                End::Refresh => {
                    if self.auth.refresh(&mut self.session).await.is_err() {
                        self.login_required(Error::LoginRequired);
                        return;
                    }
                }
                End::Reconnect => {
                    let delay = backoff(&self.options, attempt);
                    attempt = attempt.saturating_add(1);
                    let sleep = tokio::time::sleep(delay);
                    tokio::pin!(sleep);
                    loop {
                        tokio::select! {
                            _ = &mut sleep => break,
                            cmd = self.rx.recv() => match cmd {
                                Some(cmd) => { let _ = cmd.reply.send(Err(Error::Disconnected)); }
                                None => return,
                            }
                        }
                    }
                }
            }
        }
    }
    fn login_required(&self, error: Error) {
        self.status
            .send_replace(ConnectionState::LoginRequired(error));
    }
    async fn open(&self) -> Result<(Socket, ConnectionInfo)> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME_BYTES))
            .max_frame_size(Some(MAX_FRAME_BYTES));
        let (mut socket, _) = tokio_tungstenite::connect_async_with_config(
            self.auth.target().websocket().as_str(),
            Some(config),
            false,
        )
        .await
        .map_err(|_| Error::Network)?;
        let info = authenticate(&mut socket, &self.session, self.generation + 1).await?;
        Ok((socket, info))
    }
    async fn serve(&mut self, socket: &mut Socket) -> End {
        let mut pending: HashMap<String, Command> = HashMap::new();
        // Optional metadata uses its own correlation ID and never gates other RPCs.
        let version_request = WsFrame::Request {
            id: "system-version".into(),
            method: "system.version".into(),
            params: Value::Object(Default::default()),
        };
        if let Ok(wire) = serde_json::to_string(&version_request) {
            let _ = send(socket, Message::Text(wire.into()), self.options.request_timeout).await;
        }
        // Short-lived tokens refresh halfway through their remaining lifetime,
        // avoiding an immediate-refresh loop when TTL is smaller than the margin.
        let remaining = self.session.refresh_in(Duration::ZERO);
        let refresh_delay =
            remaining.saturating_sub(self.options.refresh_margin.min(remaining / 2));
        let refresh = tokio::time::sleep(refresh_delay);
        tokio::pin!(refresh);
        let mut reap = tokio::time::interval(Duration::from_millis(50));
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        heartbeat.tick().await;
        let mut last_received = Instant::now();
        let end = loop {
            tokio::select! {
                _ = &mut refresh => break End::Refresh,
                _ = heartbeat.tick() => {
                    if last_received.elapsed() > Duration::from_secs(60) { break End::Reconnect; }
                    if !send(socket, Message::Ping(Vec::new().into()), self.options.request_timeout).await { break End::Reconnect; }
                }
                _ = reap.tick() => {
                    pending.retain(|_, cmd| !cmd.reply.is_closed());
                    let expired: Vec<_> = pending.iter().filter(|(_, c)| c.deadline <= Instant::now()).map(|(id, _)| id.clone()).collect();
                    for id in expired { if let Some(cmd) = pending.remove(&id) { let _ = cmd.reply.send(Err(Error::Timeout)); } }
                }
                command = self.rx.recv() => {
                    let Some(command) = command else { break End::Shutdown; };
                    if command.reply.is_closed() { continue; }
                    if pending.len() >= 64 { let _ = command.reply.send(Err(Error::Busy)); continue; }
                    if command.deadline <= Instant::now() { let _ = command.reply.send(Err(Error::Timeout)); continue; }
                    self.next_id = self.next_id.wrapping_add(1);
                    let id = self.next_id.to_string();
                    let frame = WsFrame::Request { id: id.clone(), method: command.method.clone(), params: command.params.clone() };
                    let wire = match serde_json::to_string(&frame) { Ok(w) => w, Err(_) => { let _ = command.reply.send(Err(Error::Protocol)); continue; } };
                    let budget = command.deadline.saturating_duration_since(Instant::now()).min(self.options.request_timeout);
                    pending.insert(id, command);
                    if !send(socket, Message::Text(wire.into()), budget).await { break End::Reconnect; }
                }
                message = socket.next() => {
                    last_received = Instant::now();
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let frame: WsFrame = match serde_json::from_str(&text) { Ok(f) => f, Err(_) => break End::Reconnect };
                            match frame {
                                WsFrame::Response { id, ok, payload, error } => {
                                    if id == "system-version" {
                                        if ok && let Some(version) = payload.as_ref().and_then(|p| p.get("version")).and_then(Value::as_str) {
                                            self.status.send_modify(|state| {
                                                if let ConnectionState::Connected(info) = state {
                                                    info.version = Some(TerminalText::new(version.to_owned()));
                                                }
                                            });
                                        }
                                        continue;
                                    }
                                    let code = error.as_ref().and_then(|e| e.get("code")).and_then(Value::as_str);
                                    if code == Some("must_change_password_required") { break End::Login(Error::PasswordChangeRequired); }
                                    if matches!(code, Some("token_expired" | "jwt_expired" | "unauthorized")) { break End::Refresh; }
                                    if let Some(cmd) = pending.remove(&id) {
                                        let result = if cmd.deadline <= Instant::now() { Err(Error::Timeout) }
                                            else if ok { payload.ok_or(Error::Protocol) } else { Err(Error::RpcRejected) };
                                        let _ = cmd.reply.send(result);
                                    }
                                }
                                WsFrame::Event { event, payload, seq, .. } => {
                                    let outer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                                    let boot_id = outer.get("boot_id").or_else(|| payload.get("boot_id")).and_then(Value::as_str).map(str::to_owned);
                                    let _ = self.events.send(ServerEvent { name: event, payload, seq, boot_id });
                                }
                                _ => break End::Reconnect,
                            }
                        }
                        Some(Ok(Message::Ping(bytes))) => {
                            if !send(socket, Message::Pong(bytes), self.options.request_timeout).await { break End::Reconnect; }
                        }
                        Some(Ok(Message::Pong(_))) => {},
                        Some(Ok(Message::Close(close))) => {
                            break if close.is_some_and(|f| u16::from(f.code) == 4001 || u16::from(f.code) == 1008) { End::Refresh } else { End::Reconnect };
                        }
                        Some(Ok(_)) => {},
                        _ => break End::Reconnect,
                    }
                }
            }
        };
        for (_, command) in pending {
            let _ = command.reply.send(Err(Error::Disconnected));
        }
        end
    }
}
pub(crate) async fn authenticate<S>(
    socket: &mut WebSocketStream<S>,
    session: &Session,
    generation: u64,
) -> Result<ConnectionInfo>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Borrow the token while serializing: no secret-bearing serde_json::Value.
    #[derive(serde::Serialize)]
    struct Connect<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        id: &'static str,
        method: &'static str,
        params: ConnectParams<'a>,
    }
    #[derive(serde::Serialize)]
    struct ConnectParams<'a> {
        jwt: &'a str,
        client: &'static str,
    }
    let mut wire = Zeroizing::new(
        serde_json::to_string(&Connect {
            kind: "req",
            id: "connect",
            method: "connect",
            params: ConnectParams {
                jwt: session.access_token(),
                client: "tui",
            },
        })
        .map_err(|_| Error::Protocol)?,
    );
    // Ownership moves into tungstenite; it and TLS may retain unzeroized wire
    // buffers. Avoid an additional explicit to_string() copy in our code.
    socket
        .send(Message::Text(std::mem::take(&mut *wire).into()))
        .await
        .map_err(|_| Error::Network)?;
    while let Some(message) = socket.next().await {
        match message.map_err(|_| Error::Network)? {
            Message::Text(text) => {
                let frame: WsFrame = serde_json::from_str(&text).map_err(|_| Error::Protocol)?;
                match frame {
                    WsFrame::Response {
                        id, ok, payload, ..
                    } if id == "connect" => {
                        if !ok {
                            return Err(Error::LoginRequired);
                        }
                        return handshake(
                            payload.ok_or(Error::Protocol)?,
                            &session.account.id,
                            generation,
                        );
                    }
                    // Current server can send a challenge before connect.
                    WsFrame::Event { .. } => {}
                    _ => return Err(Error::Protocol),
                }
            }
            Message::Ping(bytes) => socket
                .send(Message::Pong(bytes))
                .await
                .map_err(|_| Error::Network)?,
            Message::Close(_) => return Err(Error::Disconnected),
            _ => {}
        }
    }
    Err(Error::Disconnected)
}
async fn send(socket: &mut Socket, message: Message, budget: Duration) -> bool {
    matches!(timeout(budget, socket.send(message)).await, Ok(Ok(())))
}
fn handshake(payload: Value, expected_user: &str, generation: u64) -> Result<ConnectionInfo> {
    // Legacy server.rs handshake returns only {status: "authenticated"};
    // it does not expose the internal admin_fallback identity.
    if payload.get("admin_fallback").and_then(Value::as_bool) == Some(true)
        || (matches!(
            payload.get("status").and_then(Value::as_str),
            Some("authenticated" | "connected")
        ) && payload.get("user").is_none_or(Value::is_null))
    {
        return Err(Error::LegacyIdentity);
    }

    #[derive(Deserialize)]
    struct Hello {
        status: String,
        user: Account,
        #[serde(default)]
        must_change_password: bool,
        #[serde(default)]
        admin_fallback: bool,
        boot_id: Option<String>,
        autonomy: Option<Value>,
    }
    let hello: Hello = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
    if hello.admin_fallback {
        return Err(Error::LegacyIdentity);
    }
    hello.user.validate()?;
    if hello.must_change_password {
        return Err(Error::PasswordChangeRequired);
    }
    if hello.status != "authenticated" {
        return Err(Error::Protocol);
    }
    if hello.user.id != expected_user {
        return Err(Error::IdentityChanged);
    }
    Ok(ConnectionInfo {
        generation,
        user_id: hello.user.id,
        role: hello.user.role,
        boot_id: hello.boot_id,
        version: None,
        autonomy: hello.autonomy.map(|v| TerminalText::new(match v {
            Value::String(s) => s, value => value.to_string(),
        })),
    })
}
fn backoff(options: &ClientOptions, attempt: u32) -> Duration {
    let ceiling = options
        .backoff_base
        .saturating_mul(2u32.saturating_pow(attempt.min(20)))
        .min(options.backoff_max);
    ceiling.mul_f64(rand::thread_rng().gen_range(0.5..=1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn handshake_rejects_legacy_and_password_change() {
        let mut hello =
            json!({"status":"authenticated", "user":{"id":"u1","email":"a@b","role":"employee"}});
        assert_eq!(
            handshake(hello.clone(), "u1", 1).unwrap().role,
            Role::Employee
        );
        hello["must_change_password"] = json!(true);
        assert!(matches!(
            handshake(hello.clone(), "u1", 1),
            Err(Error::PasswordChangeRequired)
        ));
        hello["must_change_password"] = json!(false);
        hello["user"]["id"] = json!("system");
        assert!(matches!(
            handshake(hello, "u1", 1),
            Err(Error::LegacyIdentity)
        ));
        assert!(matches!(
            handshake(json!({"status":"authenticated"}), "u1", 1),
            Err(Error::LegacyIdentity)
        ));
        assert!(matches!(
            handshake(json!({"status":"connected"}), "u1", 1),
            Err(Error::LegacyIdentity)
        ));
    }
    #[test]
    fn backoff_is_bounded_and_jittered() {
        let options = ClientOptions::default();
        let values: Vec<_> = (0..20).map(|_| backoff(&options, 100)).collect();
        assert!(
            values
                .iter()
                .all(|v| *v >= Duration::from_secs(15) && *v <= Duration::from_secs(30))
        );
        assert!(values.windows(2).any(|w| w[0] != w[1]));
    }
}
