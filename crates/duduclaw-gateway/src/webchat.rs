//! WebChat — embedded chat endpoint in the Dashboard.
//!
//! Provides a WebSocket endpoint `/ws/chat` for real-time conversation
//! directly from the web browser, without requiring any external messaging app.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, State},
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::channel_reply::ReplyContext;

/// Maximum concurrent WebSocket connections per user.
const MAX_CONNECTIONS_PER_USER: usize = 3;

/// Global limit on concurrent WebChat connections (prevents API quota exhaustion).
const MAX_TOTAL_WEBCHAT_CONNECTIONS: usize = 10;

static WEBCHAT_SEMAPHORE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(MAX_TOTAL_WEBCHAT_CONNECTIONS));

/// WebSocket message envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChatMessage {
    /// Client → Server: user sends a message.
    #[serde(rename = "user_message")]
    UserMessage {
        content: String,
        session_id: Option<String>,
    },
    /// Server → Client: assistant response chunk (streaming).
    #[serde(rename = "assistant_chunk")]
    AssistantChunk { content: String },
    /// Server → Client: assistant finished responding.
    #[serde(rename = "assistant_done")]
    AssistantDone {
        content: String,
        tokens_used: u32,
    },
    /// Server → Client: error occurred.
    #[serde(rename = "error")]
    Error { message: String },
    /// Server → Client: session info (sent on connect).
    #[serde(rename = "session_info")]
    SessionInfo {
        session_id: String,
        agent_name: String,
        agent_icon: String,
    },
}

/// Shared state for WebChat connections.
pub struct WebChatState {
    pub ctx: Arc<ReplyContext>,
    /// Track active connections per user_id.
    connections: tokio::sync::Mutex<std::collections::HashMap<String, usize>>,
}

impl WebChatState {
    pub fn new(ctx: Arc<ReplyContext>) -> Self {
        Self {
            ctx,
            connections: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    async fn acquire_connection(&self, user_id: &str) -> bool {
        let mut map = self.connections.lock().await;
        let count = map.entry(user_id.to_string()).or_insert(0);
        if *count >= MAX_CONNECTIONS_PER_USER {
            return false;
        }
        *count += 1;
        true
    }

    async fn release_connection(&self, user_id: &str) {
        let mut map = self.connections.lock().await;
        if let Some(count) = map.get_mut(user_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(user_id);
            }
        }
    }
}

/// Axum handler: upgrade HTTP to WebSocket for WebChat.
///
/// SEC2-M2: Derives `user_id` from the peer IP address so that the
/// per-user connection limit is effective rather than trivially bypassed
/// by reconnecting (each reconnect previously generated a fresh UUID).
pub async fn ws_chat_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<Arc<WebChatState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_chat_socket(socket, state, peer.ip()))
}

/// Public entry point for WebChat socket handling (used by server.rs).
/// Caller must supply the peer IP so the per-user limit is correctly enforced.
pub async fn handle_chat_socket_public(socket: WebSocket, state: Arc<WebChatState>, peer_ip: IpAddr) {
    handle_chat_socket(socket, state, peer_ip).await;
}

/// Process a WebChat WebSocket connection.
async fn handle_chat_socket(socket: WebSocket, state: Arc<WebChatState>, peer_ip: IpAddr) {
    let _permit = match WEBCHAT_SEMAPHORE.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            warn!("WebChat global connection limit reached");
            return;
        }
    };

    // Use IP for rate limiting (per-IP connection count), but add random suffix for
    // session isolation so users behind NAT get independent sessions (R3-H6).
    let rate_limit_id = format!("webchat:{peer_ip}");
    let session_suffix = &uuid::Uuid::new_v4().to_string()[..8];
    let user_id = format!("webchat:{peer_ip}:{session_suffix}");

    if !state.acquire_connection(&rate_limit_id).await {
        warn!("WebChat connection limit reached for {rate_limit_id}");
        return;
    }

    info!("WebChat connection established: {user_id}");

    let (mut sink, mut stream) = socket.split();

    // Determine the default agent
    let (agent_name, agent_icon, agent_id) = {
        let reg = state.ctx.registry.read().await;
        match reg.main_agent() {
            Some(a) => (
                a.config.agent.display_name.clone(),
                a.config.agent.icon.clone(),
                a.config.agent.name.clone(),
            ),
            None => ("DuDuClaw".to_string(), "🐾".to_string(), String::new()),
        }
    };

    // Send session info on connect
    let session_id = format!("webchat:{user_id}");
    let info_msg = ChatMessage::SessionInfo {
        session_id: session_id.clone(),
        agent_name,
        agent_icon,
    };
    if let Ok(json) = serde_json::to_string(&info_msg) {
        let _ = sink.send(Message::Text(json.into())).await;
    }

    // Heartbeat: send ping every 30s, close if no pong in 60s
    let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut last_pong = std::time::Instant::now();

    // Message processing loop
    loop {
        tokio::select! {
            // Heartbeat tick
            _ = heartbeat_interval.tick() => {
                if last_pong.elapsed().as_secs() > 60 {
                    warn!("WebChat heartbeat timeout: {user_id}");
                    break;
                }
                if sink.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            msg_opt = stream.next() => {
                let msg = match msg_opt {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        warn!("WebChat receive error: {e}");
                        break;
                    }
                    None => break,
                };

                match msg {
                    Message::Text(text) => {
                        let chat_msg: ChatMessage = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                let err = ChatMessage::Error {
                                    message: format!("Invalid message format: {e}"),
                                };
                                if let Ok(json) = serde_json::to_string(&err) {
                                    let _ = sink.send(Message::Text(json.into())).await;
                                }
                                continue;
                            }
                        };

                        match chat_msg {
                            ChatMessage::UserMessage { content, session_id: custom_session } => {
                                let sid = custom_session.as_deref().unwrap_or(&session_id);

                                // Check for chat commands first
                                if crate::chat_commands::is_command(&content) {
                                    if let Some(cmd) = crate::chat_commands::parse_command(&content, None) {
                                        let reply = crate::chat_commands::handle_command(
                                            &cmd, &state.ctx, sid, &agent_id, true,
                                        ).await;
                                        let done = ChatMessage::AssistantDone {
                                            content: reply,
                                            tokens_used: 0,
                                        };
                                        if let Ok(json) = serde_json::to_string(&done) {
                                            let _ = sink.send(Message::Text(json.into())).await;
                                        }
                                        continue;
                                    }
                                }

                                // Build AI reply
                                let reply = crate::channel_reply::build_reply_with_session(
                                    &content, &state.ctx, sid, &user_id, None,
                                ).await;

                                // Guard: don't send empty replies
                                if reply.trim().is_empty() {
                                    warn!("WebChat: reply is empty — skipping send");
                                    continue;
                                }

                                let tokens = crate::cost_telemetry::estimate_tokens(&reply) as u32;
                                let done = ChatMessage::AssistantDone {
                                    content: reply,
                                    tokens_used: tokens,
                                };
                                if let Ok(json) = serde_json::to_string(&done) {
                                    if sink.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            _ => {
                                // Client sent an unexpected message type
                                let err = ChatMessage::Error {
                                    message: "Only user_message is accepted from client".to_string(),
                                };
                                if let Ok(json) = serde_json::to_string(&err) {
                                    let _ = sink.send(Message::Text(json.into())).await;
                                }
                            }
                        }
                    }
                    Message::Ping(data) => {
                        if sink.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Message::Pong(_) => {
                        last_pong = std::time::Instant::now();
                    }
                    Message::Close(_) => {
                        info!("WebChat connection closed by client: {user_id}");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    state.release_connection(&rate_limit_id).await;
    info!("WebChat connection terminated: {user_id}");
}
