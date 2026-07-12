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
        /// L1 (per-agent routing): which AI staff member should answer. When
        /// present and the id resolves to a loaded agent, the reply runs against
        /// that agent's directory / SOUL / session (a per-agent session suffix
        /// keeps each employee's context isolated). When absent, behaviour is
        /// byte-compatible with the pre-L1 default-agent path. An id that does
        /// not resolve produces an `error` frame (fail-closed, no silent
        /// fall-through to the wrong agent).
        #[serde(default)]
        agent: Option<String>,
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
    Progress {
        content: String,
        /// "tool" | "todo" | "keepalive" — lets the dashboard build a live
        /// "agentic task insights" timeline instead of just a status string.
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Tool name for `kind == "tool"` (e.g. "Read", "Bash", "Grep").
        #[serde(skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        /// File path / search pattern extracted from the tool input, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Server → Client: a tool-step boundary in the agent's live task tree
    /// (openhuman-parity project C-P1). Emitted per `tool_use` block (start)
    /// and its matching `tool_result` (end), parsed from the Claude CLI
    /// stream-json. Purely additive to the text stream — older clients that
    /// don't recognise `type: "step"` MUST ignore it (they already ignore
    /// unknown frame types). The frontend (C-P2) folds these into a
    /// collapsible "Agentic task insights" tree.
    ///
    /// Wire shape (stable):
    /// ```json
    /// {"type":"step","phase":"start","tool":"Read","summary":"/etc/hosts","depth":0,"ts":1720598400123}
    /// {"type":"step","phase":"start","tool":"Bash","summary":"cargo test","depth":1,"ts":1720598400456}
    /// {"type":"step","phase":"end","tool":"Bash","depth":0,"ts":1720598400900}
    /// ```
    /// - `phase`: `"start"` | `"end"`.
    /// - `tool`: tool name (e.g. `Read` / `Bash` / `Grep` / `Task`).
    /// - `summary`: CJK-safe args summary, ≤120 chars; omitted on `end` and
    ///   when the tool has no summarisable input.
    /// - `depth`: nesting level — count of still-open tool calls when this step
    ///   started (a `Task` sub-agent's inner tools surface at `depth ≥ 1`).
    /// - `ts`: unix epoch milliseconds.
    #[serde(rename = "step")]
    Step {
        phase: String,
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        depth: usize,
        ts: u64,
    },
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

    // The default (main) agent id — still needed later for resume-ownership
    // checks; the rest of the session_info fields are built by the shared helper.
    let agent_id = {
        let reg = state.ctx.registry.read().await;
        reg.main_agent()
            .map(|a| a.config.agent.name.clone())
            .unwrap_or_default()
    };

    // Send session info on connect — the same frame the WP3 resume path echoes
    // (`agent = None` ⇒ the main/default agent), so the two stay byte-identical.
    let session_id = format!("webchat:{user_id}");
    let info_msg = build_session_info_frame(&state, &session_id, None).await;
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
                            ChatMessage::UserMessage { content, session_id: custom_session, agent: requested_agent, attachments } => {
                                // ── L1: resolve the requested conversation partner ──
                                // A non-empty `agent` must resolve to a loaded
                                // agent; an unknown id fails closed with an error
                                // frame rather than silently answering as the
                                // main agent (identity-mixing guard). Absent →
                                // default-agent path (byte-compatible).
                                let requested_agent = requested_agent
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty());
                                let requested_agent: Option<String> = match requested_agent {
                                    Some(name) => {
                                        let exists = {
                                            let reg = state.ctx.registry.read().await;
                                            reg.get(name).is_some()
                                        };
                                        if exists {
                                            Some(name.to_string())
                                        } else {
                                            let err = ChatMessage::Error {
                                                message: format!("unknown agent: {name}"),
                                            };
                                            if let Ok(json) = serde_json::to_string(&err) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                            continue;
                                        }
                                    }
                                    None => None,
                                };

                                // ── WP3: resume-vs-new session resolution ──
                                // The connection announces its own auto session id
                                // (`session_id`) in the `session_info` frame on
                                // connect; the client echoes it on every turn of
                                // the *current* conversation. A `session_id` that
                                // DIFFERS from the connection's own id is a RESUME
                                // request for a stored past session: it must
                                // already exist (fail closed — never silently open
                                // a new one), and a named `agent` must match the
                                // session's stored owner. Absent / own-id → the
                                // new-or-continue path, byte-compatible with the
                                // pre-WP3 behaviour (including the per-agent
                                // `#agent:` suffix).
                                let requested_sid = custom_session
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty());
                                let is_resume = matches!(requested_sid, Some(s) if s != session_id);

                                let sid_owned;
                                let sid: &str;
                                // The agent the reply actually routes to. On resume
                                // it is adopted from the stored session when the
                                // client didn't name one; otherwise it is the
                                // client's requested agent (None = main/default).
                                let mut effective_agent: Option<String> = requested_agent.clone();

                                if is_resume {
                                    let want = requested_sid.unwrap().to_string();

                                    // ── F3: cross-session / cross-channel resume guard ──
                                    // Without this, any WS client could resume an
                                    // arbitrary `webchat:…` (or `telegram:…`) session
                                    // id and read/write another user's conversation:
                                    // the resume path only checked that a named agent
                                    // matched the session's stored owner, not that the
                                    // *connection* owns the session.
                                    //
                                    // A webchat connection announces its own session id
                                    // `webchat:webchat:{peer_ip}:{random-suffix}` and
                                    // every session it creates shares that id as a
                                    // prefix (optionally a `#agent:<name>` suffix). We
                                    // therefore only allow resuming a session that
                                    // belongs to THIS connection's ownership scope:
                                    //   * exactly the connection's own session id, or
                                    //   * one derived from it (`{session_id}#…`).
                                    // This rejects cross-channel ids (telegram:/discord:
                                    // never share the prefix) and any session minted by
                                    // a different connection (different random suffix).
                                    //
                                    // Residual limitation (honest DEGRADED): because a
                                    // webchat identity is only ephemeral IP + random
                                    // suffix (no durable user), a session created on a
                                    // *previous* connection cannot be securely resumed
                                    // on a fresh one — it fails closed here. That is the
                                    // cost of closing the cross-user read/write hole
                                    // until webchat gains a real authenticated user id.
                                    let owns_session = want == session_id
                                        || want.starts_with(&format!("{session_id}#"));
                                    if !owns_session {
                                        warn!(
                                            "WebChat resume denied: session {want} not owned by connection {session_id}"
                                        );
                                        let err = ChatMessage::Error {
                                            message: "conversation not found".to_string(),
                                        };
                                        if let Ok(json) = serde_json::to_string(&err) {
                                            let _ = sink.send(Message::Text(json.into())).await;
                                        }
                                        continue;
                                    }

                                    match state.ctx.session_manager.session_agent(&want).await {
                                        Ok(Some(stored_agent)) => {
                                            // Identity guard: a named agent must own
                                            // the resumed session.
                                            if let Some(a) = &requested_agent {
                                                if a != &stored_agent {
                                                    let err = ChatMessage::Error {
                                                        message: "this conversation belongs to a different AI staff member".to_string(),
                                                    };
                                                    if let Ok(json) = serde_json::to_string(&err) {
                                                        let _ = sink.send(Message::Text(json.into())).await;
                                                    }
                                                    continue;
                                                }
                                            }
                                            // Route to the session's true owner. The
                                            // main/default agent keeps the
                                            // byte-compatible default path (None);
                                            // any other agent must still be loaded.
                                            if stored_agent == agent_id {
                                                effective_agent = None;
                                            } else {
                                                let loaded = {
                                                    let reg = state.ctx.registry.read().await;
                                                    reg.get(&stored_agent).is_some()
                                                };
                                                if !loaded {
                                                    let err = ChatMessage::Error {
                                                        message: "the AI staff member for this conversation is no longer available".to_string(),
                                                    };
                                                    if let Ok(json) = serde_json::to_string(&err) {
                                                        let _ = sink.send(Message::Text(json.into())).await;
                                                    }
                                                    continue;
                                                }
                                                effective_agent = Some(stored_agent.clone());
                                            }
                                            // Resume writes into the stored session
                                            // verbatim — no re-suffixing.
                                            sid_owned = want;
                                            sid = &sid_owned;

                                            // Echo the effective session so the
                                            // client can confirm the resume took and
                                            // refresh the header to the owning agent.
                                            let info = build_session_info_frame(
                                                &state, sid, effective_agent.as_deref(),
                                            )
                                            .await;
                                            if let Ok(json) = serde_json::to_string(&info) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                        }
                                        Ok(None) => {
                                            let err = ChatMessage::Error {
                                                message: "conversation not found".to_string(),
                                            };
                                            if let Ok(json) = serde_json::to_string(&err) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("WebChat resume lookup failed: {e}");
                                            let err = ChatMessage::Error {
                                                message: "failed to load conversation".to_string(),
                                            };
                                            if let Ok(json) = serde_json::to_string(&err) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                            continue;
                                        }
                                    }
                                } else {
                                    // New / continue the current connection's
                                    // session. Per-agent session isolation: append
                                    // an `#agent:<id>` suffix so employee A's
                                    // context never leaks into employee B's session.
                                    // The channel prefix (split at ':') stays
                                    // "webchat" so the access gate still classifies
                                    // it. Default path keeps the bare id.
                                    let base_sid: &str = session_id.as_str();
                                    sid_owned = match &effective_agent {
                                        Some(a) => format!("{base_sid}#agent:{a}"),
                                        None => base_sid.to_string(),
                                    };
                                    sid = &sid_owned;
                                }

                                // The agent id used for chat-command handling /
                                // contract enforcement — the routed agent when one
                                // was selected/adopted, else the default main agent.
                                let effective_agent_id: &str = effective_agent.as_deref().unwrap_or(&agent_id);

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
                                            &cmd, &state.ctx, sid, effective_agent_id, true,
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
                                let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<
                                    crate::channel_reply::ProgressEvent,
                                >();
                                let on_progress: crate::channel_reply::ProgressCallback =
                                    Box::new(move |event| {
                                        // Forward the STRUCTURED event so the dashboard can
                                        // build a task-insights timeline (not just a string).
                                        let _ = ptx.send(event);
                                    });
                                // Route to the selected agent (per-agent
                                // directory / SOUL / session) when one was
                                // resolved, else the default-agent path. Both
                                // branches consume `on_progress`; they are
                                // mutually exclusive so the single move is valid.
                                let work = async {
                                    match &effective_agent {
                                        Some(a) => {
                                            crate::channel_reply::build_reply_for_agent(
                                                &full_content, &state.ctx, a, sid, &user_id, Some(on_progress),
                                            )
                                            .await
                                        }
                                        None => {
                                            crate::channel_reply::build_reply_with_session(
                                                &full_content, &state.ctx, sid, &user_id, Some(on_progress),
                                            )
                                            .await
                                        }
                                    }
                                };
                                tokio::pin!(work);
                                let reply = loop {
                                    tokio::select! {
                                        r = &mut work => break r,
                                        Some(ev) = prx.recv() => {
                                            use crate::channel_reply::ProgressEvent;
                                            let content = ev.to_display();
                                            let msg = match ev {
                                                ProgressEvent::ToolUse { tool, detail } => ChatMessage::Progress {
                                                    content, kind: Some("tool".into()), tool: Some(tool), detail,
                                                },
                                                ProgressEvent::TodoUpdate { .. } => ChatMessage::Progress {
                                                    content, kind: Some("todo".into()), tool: None, detail: None,
                                                },
                                                ProgressEvent::Keepalive => ChatMessage::Progress {
                                                    content, kind: Some("keepalive".into()), tool: None, detail: None,
                                                },
                                                // C-P1: structured step boundary → dedicated `step` frame.
                                                ProgressEvent::Step(step) => ChatMessage::Step {
                                                    phase: step.phase.as_str().to_string(),
                                                    tool: step.tool,
                                                    summary: step.summary,
                                                    depth: step.depth,
                                                    ts: step.ts_ms,
                                                },
                                            };
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

/// Build a `session_info` frame for `session_id` describing the agent that will
/// answer (`agent = None` → the main/default agent). WP3 uses this to echo the
/// effective session on resume so the client can confirm the switch and refresh
/// its header. Falls back to the white-label product name when no agent is
/// loaded — mirrors the connect-time computation.
async fn build_session_info_frame(
    state: &Arc<WebChatState>,
    session_id: &str,
    agent: Option<&str>,
) -> ChatMessage {
    let (agent_name, agent_icon, resolved_id, model_id) = {
        let reg = state.ctx.registry.read().await;
        let resolved = match agent {
            Some(a) => reg.get(a),
            None => reg.main_agent(),
        };
        match resolved {
            Some(a) => (
                a.config.agent.display_name.clone(),
                a.config.agent.icon.clone(),
                a.config.agent.name.clone(),
                a.config.model.preferred.clone(),
            ),
            None => (
                crate::branding::effective_product_name(&duduclaw_core::platform::duduclaw_home()),
                "🐾".to_string(),
                String::new(),
                String::new(),
            ),
        }
    };
    let supports_vision = if resolved_id.is_empty() {
        false
    } else {
        let agent_dir = state.ctx.home_dir.join("agents").join(&resolved_id);
        let provider = crate::runtime_config::agent_runtime_provider(&agent_dir);
        crate::model_capabilities::supports_vision(provider, &model_id)
    };
    ChatMessage::SessionInfo {
        session_id: session_id.to_string(),
        agent_name,
        agent_icon,
        supports_vision,
        model: model_id,
    }
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

    // ── L1: per-agent routing protocol parsing ──────────────

    /// Extract `(content, session_id, agent)` from a parsed `UserMessage`,
    /// mirroring how the socket loop reads the frame. Panics on any other
    /// variant so a regression in the tag mapping is loud.
    fn parse_user_message(json: &str) -> (String, Option<String>, Option<String>) {
        match serde_json::from_str::<ChatMessage>(json).expect("parse user_message") {
            ChatMessage::UserMessage { content, session_id, agent, .. } => (content, session_id, agent),
            other => panic!("expected UserMessage, got {other:?}"),
        }
    }

    #[test]
    fn user_message_without_agent_parses_to_none() {
        // Byte-compatible legacy frame: no `agent` key at all.
        let (content, sid, agent) =
            parse_user_message(r#"{"type":"user_message","content":"hi","session_id":"webchat:x"}"#);
        assert_eq!(content, "hi");
        assert_eq!(sid.as_deref(), Some("webchat:x"));
        assert_eq!(agent, None, "absent agent must deserialize to None");
    }

    #[test]
    fn user_message_with_agent_carries_id() {
        let (_content, _sid, agent) = parse_user_message(
            r#"{"type":"user_message","content":"hi","session_id":"webchat:x","agent":"sales-bot"}"#,
        );
        assert_eq!(agent.as_deref(), Some("sales-bot"));
    }

    #[test]
    fn user_message_with_null_agent_is_none() {
        // An explicit null (some clients send it) must behave like absence.
        let (_content, _sid, agent) = parse_user_message(
            r#"{"type":"user_message","content":"hi","session_id":"webchat:x","agent":null}"#,
        );
        assert_eq!(agent, None);
    }

    #[test]
    fn per_agent_session_suffix_preserves_channel_prefix() {
        // The suffix used for session isolation must keep "webchat" as the
        // first `:`-delimited segment so the access gate still classifies it.
        let base = "webchat:peer:abcd";
        let suffixed = format!("{base}#agent:sales-bot");
        assert_eq!(suffixed.split(':').next(), Some("webchat"));
        assert_ne!(suffixed, base, "distinct agents get distinct session ids");
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
