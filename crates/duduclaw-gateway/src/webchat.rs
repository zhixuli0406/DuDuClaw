//! WebChat — embedded chat endpoint in the Dashboard.
//!
//! Provides a WebSocket endpoint `/ws/chat` for real-time conversation
//! directly from the web browser, without requiring any external messaging app.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, State},
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::HeaderMap,
    response::IntoResponse,
};
use duduclaw_auth::{JwtConfig, UserDb};
use duduclaw_core::truncate_bytes;
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
    /// Client → Server: first frame, authenticates the connection (C5).
    #[serde(rename = "auth")]
    Auth { token: String },
    /// Client → Server: user sends a message, optionally with file attachments.
    #[serde(rename = "user_message")]
    UserMessage {
        content: String,
        session_id: Option<String>,
        /// Uploaded files (base64). Saved to disk and referenced by path in the
        /// prompt, mirroring the channel attachment pipeline (Telegram/LINE).
        #[serde(default)]
        attachments: Vec<ChatAttachment>,
    },
    /// Server → Client: assistant response chunk (streaming).
    #[serde(rename = "assistant_chunk")]
    AssistantChunk { content: String },
    /// Server → Client: interim progress while a long task runs (tool
    /// activity / TODO task board). Purely informational; superseded by
    /// `assistant_done`.
    #[serde(rename = "progress")]
    Progress { content: String },
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
        /// Whether the agent's model can interpret uploaded images. The dashboard
        /// uses this to label the upload control and warn before sending an image
        /// to a text-only model. Documents are readable regardless.
        supports_vision: bool,
        /// The agent's preferred model id (for display next to the upload control).
        model: String,
    },
}

/// A file attachment uploaded through the WebChat socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatAttachment {
    /// Original file name (sanitized server-side before writing to disk).
    pub filename: String,
    /// MIME type hinted by the browser; falls back to magic-byte detection.
    #[serde(default)]
    pub mime: Option<String>,
    /// Base64-encoded file bytes (standard alphabet).
    pub data_base64: String,
}

/// Shared state for WebChat connections.
pub struct WebChatState {
    pub ctx: Arc<ReplyContext>,
    /// C5: JWT verifier — the WebChat socket now authenticates like /ws.
    jwt_config: Arc<JwtConfig>,
    /// C5: user store — confirms the authenticated user is still active.
    user_db: Arc<UserDb>,
    /// Track active connections per user_id.
    connections: tokio::sync::Mutex<std::collections::HashMap<String, usize>>,
}

impl WebChatState {
    pub fn new(ctx: Arc<ReplyContext>, jwt_config: Arc<JwtConfig>, user_db: Arc<UserDb>) -> Self {
        Self {
            ctx,
            jwt_config,
            user_db,
            connections: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Verify a JWT and confirm the user is active. Returns the user id.
    fn authenticate(&self, token: &str) -> Result<String, String> {
        authenticate_with(&self.jwt_config, &self.user_db, token)
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

/// C5: core WebChat authentication logic, decoupled from `WebChatState` so it
/// is unit-testable with just a `JwtConfig` + `UserDb` (no `ReplyContext`).
///
/// Verifies the JWT and confirms the named user is still a live, usable account:
/// the token must be valid, the user must exist, be `Active`, and not be flagged
/// `must_change_password`. Production behavior is identical — `authenticate`
/// delegates here.
fn authenticate_with(
    jwt_config: &JwtConfig,
    user_db: &UserDb,
    token: &str,
) -> Result<String, String> {
    let claims = jwt_config
        .verify_access_token(token)
        .map_err(|e| format!("invalid token: {e}"))?;
    match user_db.get_user(&claims.sub) {
        Ok(Some(u)) if u.status == duduclaw_auth::UserStatus::Active => {
            if u.must_change_password {
                return Err("password change required".to_string());
            }
            Ok(claims.sub)
        }
        Ok(Some(_)) => Err("account suspended".to_string()),
        Ok(None) => Err("user not found".to_string()),
        Err(_) => Err("auth service unavailable".to_string()),
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
    headers: HeaderMap,
    State(state): State<Arc<WebChatState>>,
) -> impl IntoResponse {
    // C5 fix: reject cross-site WebSocket hijacking (CSWSH). Previously this
    // endpoint had no Origin check at all.
    if !crate::server::origin_is_allowed(&headers) {
        let origin = headers.get("origin").and_then(|v| v.to_str().ok()).unwrap_or("");
        warn!(origin, "WebChat connection rejected: invalid origin");
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| handle_chat_socket(socket, state, peer.ip()))
        .into_response()
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
    let session_uuid = uuid::Uuid::new_v4().to_string();
    let session_suffix = truncate_bytes(&session_uuid, 8);
    let user_id = format!("webchat:{peer_ip}:{session_suffix}");

    if !state.acquire_connection(&rate_limit_id).await {
        warn!("WebChat connection limit reached for {rate_limit_id}");
        return;
    }

    info!("WebChat connection established: {user_id}");

    let (mut sink, mut stream) = socket.split();

    // ── C5: in-band authentication gate ──────────────────────────────────
    // The first frame MUST be `{"type":"auth","token":"<jwt>"}`. Without a
    // valid token the connection is closed before any agent/LLM interaction.
    // Timeout-guarded to prevent Slowloris-style resource exhaustion.
    {
        let auth_timeout = std::time::Duration::from_secs(10);
        let authed = match tokio::time::timeout(auth_timeout, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                match serde_json::from_str::<ChatMessage>(&text) {
                    Ok(ChatMessage::Auth { token }) => state.authenticate(&token),
                    _ => Err("first frame must be an auth message".to_string()),
                }
            }
            _ => Err("authentication handshake timed out or closed".to_string()),
        };
        if let Err(e) = authed {
            warn!("WebChat auth failed for {user_id}: {e}");
            let err = ChatMessage::Error { message: format!("authentication failed: {e}") };
            if let Ok(json) = serde_json::to_string(&err) {
                let _ = sink.send(Message::Text(json.into())).await;
            }
            let _ = sink.send(Message::Close(None)).await;
            state.release_connection(&rate_limit_id).await;
            return;
        }
    }

    // Determine the default agent
    let (agent_name, agent_icon, agent_id, model_id) = {
        let reg = state.ctx.registry.read().await;
        match reg.main_agent() {
            Some(a) => (
                a.config.agent.display_name.clone(),
                a.config.agent.icon.clone(),
                a.config.agent.name.clone(),
                a.config.model.preferred.clone(),
            ),
            None => ("DuDuClaw".to_string(), "🐾".to_string(), String::new(), String::new()),
        }
    };

    // Resolve whether this agent's model supports image understanding so the
    // dashboard can label the upload control and warn appropriately.
    let supports_vision = if agent_id.is_empty() {
        false
    } else {
        let agent_dir = state.ctx.home_dir.join("agents").join(&agent_id);
        let provider = crate::runtime_config::agent_runtime_provider(&agent_dir);
        crate::model_capabilities::supports_vision(provider, &model_id)
    };

    // Send session info on connect
    let session_id = format!("webchat:{user_id}");
    let info_msg = ChatMessage::SessionInfo {
        session_id: session_id.clone(),
        agent_name,
        agent_icon,
        supports_vision,
        model: model_id,
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

                #[allow(clippy::collapsible_match)]
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
                            ChatMessage::UserMessage { content, session_id: custom_session, attachments } => {
                                let sid = custom_session.as_deref().unwrap_or(&session_id);

                                // Persist any uploaded files and append path references
                                // to the prompt — the agent (Claude CLI) reads them from
                                // disk, exactly as channel attachments are handled.
                                let mut full_content = content.clone();
                                if !attachments.is_empty() {
                                    let refs = save_webchat_attachments(&state.ctx.home_dir, &attachments).await;
                                    if !refs.is_empty() {
                                        if !full_content.trim().is_empty() {
                                            full_content.push_str("\n\n");
                                        }
                                        full_content.push_str(&refs.join("\n"));
                                    }
                                }

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

                                // Build AI reply, interleaving progress events
                                // (tool activity / TODO board) onto the socket
                                // while the task runs.
                                let (ptx, mut prx) =
                                    tokio::sync::mpsc::unbounded_channel::<String>();
                                let on_progress: crate::channel_reply::ProgressCallback =
                                    Box::new(move |event| {
                                        let _ = ptx.send(event.to_display());
                                    });
                                let work = crate::channel_reply::build_reply_with_session(
                                    &full_content, &state.ctx, sid, &user_id, Some(on_progress),
                                );
                                tokio::pin!(work);
                                let reply = loop {
                                    tokio::select! {
                                        r = &mut work => break r,
                                        Some(p) = prx.recv() => {
                                            let msg = ChatMessage::Progress { content: p };
                                            if let Ok(json) = serde_json::to_string(&msg) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                        }
                                    }
                                };

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

/// Decode, size-check, and persist WebChat attachments to `<home>/attachments/`,
/// returning markdown file-reference lines to append to the prompt. Invalid or
/// oversized attachments are logged and skipped (never abort the whole message).
async fn save_webchat_attachments(
    home_dir: &std::path::Path,
    attachments: &[ChatAttachment],
) -> Vec<String> {
    use base64::Engine;

    let mut refs = Vec::new();
    for att in attachments {
        let data = match base64::engine::general_purpose::STANDARD.decode(att.data_base64.as_bytes()) {
            Ok(d) => d,
            Err(e) => {
                warn!(file = %att.filename, "WebChat attachment base64 decode failed: {e}");
                continue;
            }
        };
        if data.len() as u64 > crate::media::MAX_FILE_SIZE {
            warn!(
                file = %att.filename,
                bytes = data.len(),
                "WebChat attachment exceeds max size — skipping"
            );
            continue;
        }
        let filename = if att.filename.trim().is_empty() {
            "upload.bin".to_string()
        } else {
            att.filename.clone()
        };
        match crate::media::save_attachment_to_disk(home_dir, &data, &filename).await {
            Ok(path) => {
                let mime = att
                    .mime
                    .clone()
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| crate::media::detect_mime(&data));
                let mt = crate::media::media_type_from_mime(&mime);
                refs.push(crate::media::format_attachment_ref(&mt, &filename, &path));
            }
            Err(e) => warn!(file = %filename, "WebChat attachment save failed: {e}"),
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::{AccessLevel, JwtConfig, UserDb, UserRole, UserStatus};

    /// Build an isolated in-temp-dir `UserDb` + a test `JwtConfig`.
    fn fixtures() -> (JwtConfig, UserDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = UserDb::new(&dir.path().join("users.db")).expect("user db");
        let jwt = JwtConfig::new(b"test-secret-key-for-webchat-auth-unit-tests");
        (jwt, db, dir)
    }

    /// Issue an access token for `user` with no agent bindings.
    fn token_for(jwt: &JwtConfig, user: &duduclaw_auth::User) -> String {
        let access: Vec<(String, AccessLevel)> = Vec::new();
        jwt.issue_access_token(user, &access).expect("issue token")
    }

    #[test]
    fn authenticate_accepts_valid_active_user() {
        let (jwt, db, _dir) = fixtures();
        let user = db
            .create_user("alice@example.com", "Alice", "pw-strong-123", UserRole::Employee)
            .expect("create user");
        let token = token_for(&jwt, &user);

        let result = authenticate_with(&jwt, &db, &token);
        assert_eq!(result.as_deref(), Ok(user.id.as_str()));
    }

    #[test]
    fn authenticate_rejects_garbage_token() {
        let (jwt, db, _dir) = fixtures();
        let result = authenticate_with(&jwt, &db, "not.a.jwt");
        assert!(result.is_err(), "garbage token must be rejected");
    }

    #[test]
    fn authenticate_rejects_must_change_password_user() {
        let (jwt, db, _dir) = fixtures();
        // The default admin is created with `must_change_password = true`.
        db.ensure_default_admin().expect("bootstrap admin");
        let admin = db
            .list_users()
            .expect("list users")
            .into_iter()
            .find(|u| u.email == "admin@local")
            .expect("admin row");
        assert!(admin.must_change_password, "precondition: flag set");

        let token = token_for(&jwt, &admin);
        let result = authenticate_with(&jwt, &db, &token);
        assert!(
            result.is_err(),
            "a user pending a forced password change must be rejected"
        );
    }

    #[test]
    fn authenticate_rejects_suspended_user() {
        let (jwt, db, _dir) = fixtures();
        let user = db
            .create_user("bob@example.com", "Bob", "pw-strong-123", UserRole::Employee)
            .expect("create user");
        // Token is issued while active, then the account is suspended — mirrors
        // an operator disabling a user whose JWT is still unexpired.
        let token = token_for(&jwt, &user);
        db.set_user_status(&user.id, UserStatus::Suspended)
            .expect("suspend user");

        let result = authenticate_with(&jwt, &db, &token);
        assert!(result.is_err(), "suspended user must be rejected");
    }
}
