//! Credentials arrive only from the RPC actor's existing refresh lifecycle.
use super::{ChatState, Frame, Submission};
use crate::{
    Error, Result,
    client::{ClientOptions, MAX_FRAME_BYTES},
    transport::GatewayUrl,
};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use zeroize::Zeroizing;

// Deliberately neither Debug nor Serialize.
pub(crate) struct Credentials {
    pub token: Zeroizing<String>,
    pub generation: u64,
}
enum Command {
    Send(Submission),
    Cancel,
}
struct Inner {
    commands: mpsc::Sender<Command>,
    state: watch::Receiver<ChatState>,
    shutdown: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}
#[derive(Clone)]
pub struct ChatClient {
    inner: Arc<Inner>,
}
impl ChatClient {
    pub(crate) fn start(
        target: GatewayUrl,
        credentials: watch::Receiver<Option<Arc<Credentials>>>,
        refresh: mpsc::Sender<bool>,
        options: ClientOptions,
    ) -> Self {
        let (commands, rx) = mpsc::channel(1);
        let (state, receiver) = watch::channel(ChatState::default());
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (finished_tx, finished) = watch::channel(false);
        tokio::spawn(crate::without_tracing(async move {
            Actor {
                shutdown: shutdown_rx,
                target,
                credentials,
                refresh,
                options,
                commands: rx,
                state,
                model: ChatState::default(),
                refresh_attempted: false,
            }
            .run().await;
            finished_tx.send_replace(true);
        }));
        Self {
            inner: Arc::new(Inner {
                commands,
                state: receiver,
                shutdown, finished,
            }),
        }
    }
    pub async fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
        let mut finished = self.inner.finished.clone();
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while !*finished.borrow_and_update() { if finished.changed().await.is_err() { break; } }
        }).await;
    }
    pub fn state(&self) -> watch::Receiver<ChatState> {
        self.inner.state.clone()
    }
    pub fn send(&self, input: Submission) -> Result<()> {
        let state = self.inner.state.borrow();
        if !state.connected {
            return Err(Error::Disconnected);
        }
        if state.running {
            return Err(Error::Busy);
        }
        self.inner
            .commands
            .try_send(Command::Send(input))
            .map_err(|_| Error::Busy)
    }
    pub fn cancel(&self) -> Result<()> {
        if !self.inner.state.borrow().can_cancel() {
            return Err(Error::RpcRejected);
        }
        self.inner
            .commands
            .try_send(Command::Cancel)
            .map_err(|_| Error::Busy)
    }
}
struct Actor {
    shutdown: watch::Receiver<bool>,
    target: GatewayUrl,
    credentials: watch::Receiver<Option<Arc<Credentials>>>,
    refresh: mpsc::Sender<bool>,
    options: ClientOptions,
    commands: mpsc::Receiver<Command>,
    state: watch::Sender<ChatState>,
    model: ChatState,
    refresh_attempted: bool,
}
impl Actor {
    fn publish(&self) {
        self.state.send_replace(self.model.clone());
    }
    async fn run(mut self) {
        let mut attempt = 0;
        loop {
            if *self.shutdown.borrow() { return; }
            if self.credentials.has_changed().is_err() {
                return;
            }
            let credential = self.credentials.borrow_and_update().clone();
            let Some(credential) = credential else {
                tokio::select! {
                    _ = self.shutdown.changed() => return,
                    changed = self.credentials.changed() => if changed.is_err() { return; },
                }
                continue;
            };
            let mut url = self.target.websocket();
            url.set_path("/ws/chat");
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_FRAME_BYTES))
                .max_frame_size(Some(MAX_FRAME_BYTES));
            let opened = tokio::select! {
                _ = self.shutdown.changed() => return,
                changed = self.credentials.changed() => { if changed.is_err() { return; } continue; }
                result = tokio::time::timeout(self.options.request_timeout,
                    tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false)) => result,
            };
            if let Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) = &opened
                && response.status().as_u16() == 401
            {
                let _ = self.refresh.try_send(!self.refresh_attempted);
                self.refresh_attempted = true;
            }
            if let Ok(Ok((mut socket, _))) = opened {
                let started = tokio::time::Instant::now();
                let mut shutdown = self.shutdown.clone();
                tokio::select! {
                    _ = shutdown.changed() => {},
                    _ = self.serve(&mut socket, &credential) => {},
                }
                crate::client::close::socket(&mut socket).await;
                if started.elapsed() > Duration::from_secs(30) {
                    attempt = 0;
                }
            }
            self.model.disconnected();
            self.publish();
            if *self.shutdown.borrow() { return; }
            // Never replay an uncertain user send or cancel after reconnect.
            while self.commands.try_recv().is_ok() {}
            match self.credentials.has_changed() {
                Err(_) => return,
                Ok(true) => continue,
                Ok(false) => {}
            }
            let delay = crate::client::backoff(&self.options, attempt);
            attempt = attempt.saturating_add(1);
            tokio::select! {
                _ = self.shutdown.changed() => return,
                _ = tokio::time::sleep(delay) => {},
                changed = self.credentials.changed() => if changed.is_err() { return; },
            }
        }
    }
    async fn serve<S>(&mut self, socket: &mut WebSocketStream<S>, credentials: &Credentials)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        #[derive(serde::Serialize)]
        struct Auth<'a> {
            r#type: &'static str,
            token: &'a str,
        }
        let Ok(wire) = serde_json::to_string(&Auth {
            r#type: "auth",
            token: &credentials.token,
        }) else {
            return;
        };
        let mut wire = Zeroizing::new(wire);
        if !send(
            socket,
            Message::Text(std::mem::take(&mut *wire).into()),
            self.options.request_timeout,
        )
        .await
        {
            return;
        }
        let mut authenticated = false;
        let handshake = tokio::time::sleep(self.options.request_timeout);
        tokio::pin!(handshake);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        heartbeat.tick().await;
        let mut last_received = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = self.shutdown.changed() => return,
                _ = &mut handshake, if !authenticated => return,
                changed = self.credentials.changed() => {
                    if changed.is_err() { return; }
                    let current = self.credentials.borrow();
                    if current.as_ref().map(|c| c.generation) != Some(credentials.generation) { return; }
                }
                _ = heartbeat.tick() => {
                    if last_received.elapsed() > Duration::from_secs(90) { return; }
                    if !send(socket, Message::Ping(Vec::new().into()), self.options.request_timeout).await { return; }
                }
                command = self.commands.recv(), if authenticated => {
                    let value = match command {
                        Some(Command::Send(input)) => match self.model.prepare(input) {
                            Ok(frame) => serde_json::to_value(frame).ok(),
                            Err(error) => { self.model.status = crate::sanitize::TerminalText::new(error.to_string()); None }
                        },
                        Some(Command::Cancel) => self.model.cancel_frame(),
                        None => return,
                    };
                    self.publish();
                    if let Some(value) = value
                        && !send(socket, Message::Text(value.to_string().into()), self.options.request_timeout).await { return; }
                }
                message = socket.next() => {
                    last_received = tokio::time::Instant::now();
                    match message {
                        Some(Ok(Message::Text(wire))) => {
                            let Ok(frame) = Frame::parse(&wire) else { return; };
                            if !authenticated && frame.kind == "error" {
                                let _ = self.refresh.try_send(!self.refresh_attempted);
                                self.refresh_attempted = true;
                                return;
                            }
                            if frame.kind == "session_info" { authenticated = true; self.refresh_attempted = false; }
                            self.model.apply(frame); self.publish();
                        }
                        Some(Ok(Message::Ping(bytes))) => {
                            if !send(socket, Message::Pong(bytes), self.options.request_timeout).await { return; }
                        }
                        Some(Ok(Message::Close(close))) => {
                            if close.is_some_and(|c| u16::from(c.code) == 4001) {
                                let _ = self.refresh.try_send(!self.refresh_attempted);
                                self.refresh_attempted = true;
                            }
                            return;
                        }
                        Some(Ok(Message::Pong(_))) => {},
                        Some(Ok(_)) => {},
                        _ => return,
                    }
                }
            }
        }
    }
}
async fn send<S>(socket: &mut WebSocketStream<S>, message: Message, budget: Duration) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    matches!(
        tokio::time::timeout(budget, socket.send(message)).await,
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_tungstenite::tungstenite::protocol::Role;

    async fn pair() -> (
        WebSocketStream<tokio::io::DuplexStream>,
        WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (a, b) = tokio::io::duplex(4096);
        tokio::join!(
            WebSocketStream::from_raw_socket(a, Role::Client, None),
            WebSocketStream::from_raw_socket(b, Role::Server, None)
        )
    }
    #[tokio::test]
    async fn only_authentication_closes_request_refresh() {
        use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
        for authenticated in [false, true] {
            for code in [None, Some(1000), Some(1011), Some(4001), Some(1008)] {
                let credentials = Arc::new(Credentials {
                    token: Zeroizing::new("close-test-token".into()), generation: 1,
                });
                let (_credential_tx, credential_rx) = watch::channel(Some(credentials.clone()));
                let (refresh_tx, mut refresh_rx) = mpsc::channel(1);
                let (_command_tx, command_rx) = mpsc::channel(1);
                let (state_tx, _) = watch::channel(ChatState::default());
                let (_shutdown_tx, shutdown) = watch::channel(false);
                let mut actor = Actor {
                    shutdown, target: GatewayUrl::parse("http://127.0.0.1").unwrap(),
                    credentials: credential_rx, refresh: refresh_tx,
                    options: ClientOptions::default(), commands: command_rx,
                    state: state_tx, model: ChatState::default(), refresh_attempted: false,
                };
                let (mut client, mut server) = pair().await;
                let fake = async {
                    let _ = server.next().await.unwrap().unwrap();
                    if authenticated {
                        server.send(Message::Text(json!({"type":"session_info"}).to_string().into())).await.unwrap();
                    }
                    server.send(Message::Close(code.map(|code| CloseFrame {
                        code: CloseCode::from(code), reason: "fixture".into(),
                    }))).await.unwrap();
                };
                tokio::time::timeout(Duration::from_secs(2), async {
                    tokio::join!(actor.serve(&mut client, &credentials), fake);
                }).await.unwrap();
                assert_eq!(refresh_rx.try_recv().ok(),
                    matches!(code, Some(4001)).then_some(true),
                    "authenticated={authenticated}, close={code:?}");

                // An abrupt transport loss is also a reconnect, even before auth.
                let (mut client, mut server) = pair().await;
                let fake = async move {
                    let _ = server.next().await.unwrap().unwrap();
                    drop(server);
                };
                tokio::time::timeout(Duration::from_secs(2), async {
                    tokio::join!(actor.serve(&mut client, &credentials), fake);
                }).await.unwrap();
                assert!(refresh_rx.try_recv().is_err());
            }
        }
    }
    #[tokio::test]
    async fn in_memory_gateway_auth_refresh_and_credential_rotation() {
        let credentials = Arc::new(Credentials {
            token: Zeroizing::new("canary-token".into()),
            generation: 1,
        });
        let (credential_tx, credential_rx) = watch::channel(Some(credentials.clone()));
        let (refresh_tx, mut refresh_rx) = mpsc::channel(1);
        let (_command_tx, command_rx) = mpsc::channel(1);
        let (state_tx, mut state_rx) = watch::channel(ChatState::default());
        let (_shutdown_tx, shutdown) = watch::channel(false);
        let mut actor = Actor {
            shutdown,
            target: GatewayUrl::parse("http://127.0.0.1").unwrap(),
            credentials: credential_rx,
            refresh: refresh_tx,
            options: ClientOptions::default(),
            commands: command_rx,
            state: state_tx,
            model: ChatState::default(),
            refresh_attempted: false,
        };
        // Failed authentication asks the existing owner to refresh once, then
        // requests account login. The chat actor never invokes REST refresh.
        for expected in [true, false] {
            let (mut client, mut server) = pair().await;
            let fake = async {
                let Some(Ok(Message::Text(wire))) = server.next().await else {
                    panic!("missing auth");
                };
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&wire).unwrap(),
                    json!({"type":"auth","token":"canary-token"})
                );
                server
                    .send(Message::Text(
                        json!({"type":"error","message":"authentication failed"})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(actor.serve(&mut client, &credentials), fake);
            })
            .await
            .unwrap();
            assert_eq!(refresh_rx.try_recv().unwrap(), expected);
        }
        let (mut client, mut server) = pair().await;
        let fake = async {
            let _ = server.next().await.unwrap().unwrap();
            server
                .send(Message::Text(
                    json!({"type":"session_info","session_id":"s1","agent_name":"adu",
                "agent_icon":"","supports_vision":false,"model":"fixture"})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            loop {
                if state_rx.borrow_and_update().connected {
                    break;
                }
                state_rx.changed().await.unwrap();
            }
            credential_tx.send_replace(None);
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(actor.serve(&mut client, &credentials), fake);
        })
        .await
        .unwrap();
        assert!(!actor.refresh_attempted);
        // Dropping the credential owner is shutdown, never permission to dial
        // again with the last token retained in the watch channel.
        drop(credential_tx);
        tokio::time::timeout(Duration::from_secs(1), actor.run())
            .await
            .unwrap();
    }
}
