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
        /// Client conversation nonce (dashboard "conversation" id). Two roles:
        /// ① scopes this turn to its own server-side session bucket
        /// (`…#conv:<nonce>`), so an in-flight reply from conversation A persists
        /// to A's history and never bleeds into a freshly-opened conversation B;
        /// ② echoed back verbatim on every reply frame so the browser renders the
        /// reply into the conversation that started it — not whichever
        /// conversation happens to be open when a long task finishes. Absent →
        /// byte-compatible single-bucket behaviour (pre-fix clients).
        #[serde(default)]
        conv: Option<String>,
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
        /// Conversation nonce this progress belongs to (echoed from the
        /// originating `user_message`). The browser drops frames whose `conv`
        /// differs from the conversation currently open — see `UserMessage.conv`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conv: Option<String>,
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
        /// Conversation nonce this step belongs to (echoed from the originating
        /// `user_message`) — see `UserMessage.conv`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conv: Option<String>,
    },
    /// Server → Client: assistant finished responding.
    #[serde(rename = "assistant_done")]
    AssistantDone {
        content: String,
        tokens_used: u32,
        /// Conversation nonce this reply belongs to (echoed from the originating
        /// `user_message`). The browser renders it into that conversation only —
        /// see `UserMessage.conv`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conv: Option<String>,
        /// The model that ACTUALLY produced this reply (parsed from the CLI
        /// stream-json `message.model`) — may differ from `session_info.model`
        /// (the configured intent) after CLI-side substitution. Absent when
        /// the backend didn't report one (chat commands, local inference).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// O-4→O-3: a structured chat-artifact (see
        /// `web/src/components/console/artifact-types.ts`'s `ChatArtifact`)
        /// derived from an O-4 `<system_operator_pending>` marker on this
        /// reply — e.g. a `confirm_action` card for a pending restart/
        /// shutdown/factory-reset. `None` for the overwhelming majority of
        /// replies (anything that never went through O-4's pending-op path).
        /// The raw marker tag is ALWAYS stripped from `content` before this
        /// frame is built, regardless of whether this field is populated —
        /// see `channel_reply::build_reply_with_session_with_artifact`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<serde_json::Value>,
    },
    /// Server → Client: error occurred.
    #[serde(rename = "error")]
    Error {
        message: String,
        /// C-ACL (P0-S1): machine-readable error class for clients that need
        /// to branch without parsing zh-TW prose (e.g. an agent picker
        /// greying out an agent whose binding was just revoked). Absent on
        /// every error frame that predates this field — byte-compatible with
        /// clients that only ever read `message`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
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
        /// `true` when this frame is a server-initiated refresh after an
        /// agent-config change (not a connect/resume announcement). The client
        /// updates only the agent metadata (name / icon / vision / model) and
        /// leaves its session state untouched.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refresh: bool,
    },
}

impl ChatMessage {
    /// Build a plain-text error frame (`code: None`) — the wire shape every
    /// error predating C-ACL used, so existing string-matching clients (e.g.
    /// the dashboard's `isResumeNotFound`) keep working unchanged.
    fn error(message: impl Into<String>) -> Self {
        ChatMessage::Error { message: message.into(), code: None }
    }

    /// Build an error frame carrying a machine-readable `code` (ASCII
    /// snake_case) so a client can branch on error class without parsing
    /// zh-TW prose.
    fn error_coded(message: impl Into<String>, code: &'static str) -> Self {
        ChatMessage::Error { message: message.into(), code: Some(code.to_string()) }
    }
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

/// C-ACL (P0-S1, 2026-08 audit): TTL for the per-user ACL snapshot cache
/// (`WebChatState::acl_cache`) that gates explicit `agent` selection in a
/// `user_message`. Short enough that revoking a user's binding
/// (`users.unbind_agent`) takes effect for WebChat within this window instead
/// of riding out a JWT's full lifetime (JWT `access_levels` are baked in at
/// issue time and never re-checked) — a bounded staleness window instead of
/// an active-connection disconnect mechanism.
const ACL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Snapshot of a user's role + full agent-binding set — the data
/// `WebChatState::agent_access_allowed` needs to gate explicit `agent`
/// selection in a WebChat `user_message`. Pure data, no DB handle, so the
/// decision logic below (`acl_allows_agent` / `resolve_agent_acl`) is
/// unit-testable without a live `UserDb`.
#[derive(Debug, Clone)]
struct AclSnapshot {
    role: duduclaw_auth::UserRole,
    bound_agents: std::collections::HashSet<String>,
}

/// C-ACL (P0-S1): may `snapshot`'s user address `agent_name` by name in a
/// `user_message`? Admins pass unconditionally; everyone else needs a
/// binding to the named agent at ANY access level — Viewer is enough to
/// converse (this gates *which agent* the turn addresses, not what the user
/// may do to it once selected; that finer-grained check is
/// `duduclaw_auth::acl::require_agent_access` on the dashboard RPC path).
fn acl_allows_agent(snapshot: &AclSnapshot, agent_name: &str) -> bool {
    snapshot.role == duduclaw_auth::UserRole::Admin || snapshot.bound_agents.contains(agent_name)
}

/// Compose a (possibly failed) ACL lookup into a final allow/deny decision.
/// Fails CLOSED: any lookup error (DB unavailable, corrupt row, the user
/// record vanished mid-session) denies rather than falling through to
/// "allow".
fn resolve_agent_acl(snapshot: &Result<AclSnapshot, String>, agent_name: &str) -> bool {
    match snapshot {
        Ok(s) => acl_allows_agent(s, agent_name),
        Err(_) => false,
    }
}

/// C-ACL resume gap (P0-S2, 2026-08 audit follow-up): wave one (`acl_allows_agent`
/// above) only gated an EXPLICIT `agent` field on a `user_message`. Resuming a
/// stored session — a `session_id` naming a past session, with no `agent`
/// field (or one that already matches, per the identity guard) on that turn —
/// adopted the session's stored agent straight from `SessionManager` with no
/// re-check at all: a user unbound from a non-default agent could keep
/// talking to it forever through an old session id, because the 60s
/// `ACL_CACHE_TTL` staleness bound wave one relies on was never consulted on
/// this path.
///
/// This predicate answers "does resuming into `stored_agent` need the same
/// `agent_access_allowed` check wave one already applies to explicit
/// selection?" Session storage has no "the user explicitly chose this agent"
/// flag, so the criterion is the best available proxy: `stored_agent !=
/// default_agent`. The main/default-agent resume path (`stored_agent ==
/// default_agent`) is wave one's byte-compatible scope and MUST stay
/// ungated here — widening the check to the default agent is explicitly out
/// of scope for this fix.
fn resume_needs_acl_check(stored_agent: &str, default_agent: &str) -> bool {
    stored_agent != default_agent
}

/// Load `user_id`'s role + full agent-binding set from the auth DB — the one
/// DB round-trip `WebChatState::agent_access_allowed` performs on a cache
/// miss.
fn load_acl_snapshot(user_db: &UserDb, user_id: &str) -> Result<AclSnapshot, String> {
    let user = user_db
        .get_user(user_id)?
        .ok_or_else(|| "user not found".to_string())?;
    let bindings = user_db.get_user_agents(user_id)?;
    Ok(AclSnapshot {
        role: user.role,
        bound_agents: bindings.into_iter().map(|b| b.agent_name).collect(),
    })
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
    /// C-ACL (P0-S1): per-user role+bindings snapshot cache, keyed by
    /// authenticated user id. Populated lazily on the first explicit-agent
    /// selection and refreshed after `ACL_CACHE_TTL`.
    acl_cache: tokio::sync::Mutex<std::collections::HashMap<String, (AclSnapshot, std::time::Instant)>>,
}

impl WebChatState {
    pub fn new(ctx: Arc<ReplyContext>, jwt_config: Arc<JwtConfig>, user_db: Arc<UserDb>) -> Self {
        Self {
            ctx,
            jwt_config,
            user_db,
            connections: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            acl_cache: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Verify a JWT and confirm the user is active. Returns the user id.
    /// Returns `(auth_user, must_change_password)`. The bool rides along
    /// exactly like `UserContext::must_change_password` on the dashboard WS
    /// path (`server.rs::jwt_account_gate`) — the handshake succeeds for
    /// any Active account, flagged or not; the caller (the socket loop)
    /// restricts a flagged connection to a guidance-only reply instead of
    /// refusing to connect at all. A widget-mode visitor never carries this
    /// flag (there is no account to force a password change on).
    fn authenticate(&self, token: &str) -> Result<(String, bool), String> {
        // WP1.3/1.8 (ecosystem): public-widget visitor mode — site-embedded
        // chat for anonymous visitors (WordPress widget, website demo).
        // Default OFF; every failure path denies. JWT path is untouched.
        if let Some(presented) = token.strip_prefix("widget:") {
            return widget_authenticate(&self.ctx.home_dir, presented).map(|id| (id, false));
        }
        authenticate_with(&self.jwt_config, &self.user_db, token)
    }

    /// C-ACL (P0-S1): may `user_id` address `agent_name` by name in a
    /// `user_message`? Reads the short-TTL `acl_cache` first so a chatty
    /// conversation doesn't hit sqlite on every turn; a miss (or expiry)
    /// re-queries `UserDb` directly and refreshes the cache entry on success.
    /// A failed lookup is logged and denies (fail-closed) without touching
    /// the cache — a transient DB hiccup should not poison a good decision
    /// for `ACL_CACHE_TTL`, and should not cache a bad one either.
    async fn agent_access_allowed(&self, user_id: &str, agent_name: &str) -> bool {
        let now = std::time::Instant::now();
        {
            let cache = self.acl_cache.lock().await;
            if let Some((snapshot, at)) = cache.get(user_id) {
                if now.duration_since(*at) < ACL_CACHE_TTL {
                    return acl_allows_agent(snapshot, agent_name);
                }
            }
        }
        let lookup = load_acl_snapshot(&self.user_db, user_id);
        let allowed = resolve_agent_acl(&lookup, agent_name);
        match lookup {
            Ok(snapshot) => {
                let mut cache = self.acl_cache.lock().await;
                cache.insert(user_id.to_string(), (snapshot, now));
            }
            Err(e) => {
                warn!(user_id, agent = %agent_name, "WebChat ACL lookup failed — denying agent selection: {e}");
            }
        }
        allowed
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

/// WP1.3/1.8 (ecosystem): public-widget visitor auth. Gate requirements —
/// ALL must hold, every miss is a DENY (fail-closed):
///   1. `config.toml [webchat] public_widget = true` (explicit opt-in;
///      default off),
///   2. `widget_key` present with ≥16 chars,
///   3. the presented key matches (constant-time).
///
/// Threat model: the key ships inside the public page's JS, so it is public
/// by design — it grants exactly one thing: anonymous visitor chat, behind
/// the existing per-IP connection cap and the global WebChat semaphore. What
/// it must never grant is access to ANYONE ELSE's conversation: each
/// connection gets a unique random visitor identity, so the resume-ownership
/// guard (keyed on this identity) can never match another visitor's session.
fn widget_authenticate(home_dir: &std::path::Path, presented: &str) -> Result<String, String> {
    let raw = std::fs::read_to_string(home_dir.join("config.toml"))
        .map_err(|_| "widget mode not configured".to_string())?;
    let cfg: toml::Value = raw
        .parse()
        .map_err(|_| "widget mode not configured".to_string())?;
    let section = cfg.get("webchat").ok_or("widget mode not configured")?;
    if section.get("public_widget").and_then(|v| v.as_bool()) != Some(true) {
        return Err("widget mode disabled".to_string());
    }
    let key = section
        .get("widget_key")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if key.len() < 16 {
        return Err("widget key missing or too short (min 16 chars)".to_string());
    }
    if !crate::auth::constant_time_eq(key.as_bytes(), presented.as_bytes()) {
        return Err("invalid widget key".to_string());
    }
    Ok(format!("widget-visitor:{}", uuid::Uuid::new_v4()))
}

/// Stable, id-safe owner tag for an authenticated user.
///
/// A raw user id cannot go into a session id directly: ids are `:`-delimited
/// and the session id is surfaced to the dashboard, so embedding the account
/// identifier would both break parsing and leak who owns a conversation. A
/// truncated SHA-256 is stable across connections (which is the whole point —
/// it survives page reloads), collision-resistant enough at 12 hex chars for
/// per-installation user counts, and reveals nothing.
pub fn webchat_owner_tag(auth_user: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(auth_user.as_bytes());
    hex::encode(digest)[..12].to_string()
}

/// Whether `want` is a webchat session this owner may resume.
///
/// Ownership is by AUTHENTICATED USER, not by connection: a session created in
/// one tab must stay resumable after a reload, from a second tab, or from the
/// sidebar's 對話紀錄. Fail-closed for everything else — a different user's tag,
/// a session from another channel (`telegram:` / `discord:` never carry this
/// prefix), or a crafted id that merely starts with the same characters (the
/// trailing separator makes `…{tag}evil:` fail).
///
/// Pure — exported for tests.
pub fn owns_webchat_session(want: &str, owner_tag: &str) -> bool {
    if owner_tag.is_empty() {
        return false;
    }
    let scope = format!("webchat:webchat:{owner_tag}:");
    want.starts_with(&scope)
}

/// C5: core WebChat authentication logic, decoupled from `WebChatState` so it
/// is unit-testable with just a `JwtConfig` + `UserDb` (no `ReplyContext`).
///
/// Verifies the JWT and confirms the named user is still a live, usable
/// account: the token must be valid, the user must exist, and be `Active`.
/// Production behavior is identical — `authenticate` delegates here.
///
/// Returns `(user_id, must_change_password)`. A caller flagged
/// `must_change_password` (the bootstrap `admin@local`, or any account an
/// operator has reset) is NOT refused here — the flag rides along instead,
/// mirroring `server.rs::jwt_account_gate`'s C1 fix on the dashboard WS
/// path. Before this, an `Err("password change required")` here closed the
/// socket outright; WebChat has no self-service change-password flow to
/// redirect a stuck visitor to (unlike the dashboard, which at least has an
/// account-settings screen behind the same gate), so that was a dead end,
/// not a recoverable failure. The socket loop reads this flag and restricts
/// the connection to a guidance-only reply for every `user_message` instead.
fn authenticate_with(
    jwt_config: &JwtConfig,
    user_db: &UserDb,
    token: &str,
) -> Result<(String, bool), String> {
    let claims = jwt_config
        .verify_access_token(token)
        .map_err(|e| format!("invalid token: {e}"))?;
    match user_db.get_user(&claims.sub) {
        Ok(Some(u)) if u.status == duduclaw_auth::UserStatus::Active => {
            Ok((claims.sub, u.must_change_password))
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

    // Use IP for rate limiting (per-IP connection count). The session id itself
    // is keyed by the AUTHENTICATED user (below), not the IP — see
    // `webchat_owner_tag`.
    let rate_limit_id = format!("webchat:{peer_ip}");
    let session_uuid = uuid::Uuid::new_v4().to_string();
    let session_suffix = truncate_bytes(&session_uuid, 8);

    if !state.acquire_connection(&rate_limit_id).await {
        warn!("WebChat connection limit reached for {rate_limit_id}");
        return;
    }

    let (mut sink, mut stream) = socket.split();

    // ── C5: in-band authentication gate ──────────────────────────────────
    // The first frame MUST be `{"type":"auth","token":"<jwt>"}`. Without a
    // valid token the connection is closed before any agent/LLM interaction.
    // Timeout-guarded to prevent Slowloris-style resource exhaustion.
    // `must_change_password` rides along here rather than being refused —
    // see `authenticate_with`'s doc comment. The connection still opens
    // normally; every `user_message` from a flagged account gets a
    // guidance-only reply instead of reaching the agent (checked below,
    // inside the message loop).
    let (auth_user, must_change_password) = {
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
        match authed {
            Ok(u) => u,
            Err(e) => {
                warn!("WebChat auth failed from {peer_ip}: {e}");
                let err = ChatMessage::error(format!("authentication failed: {e}"));
                if let Ok(json) = serde_json::to_string(&err) {
                    let _ = sink.send(Message::Text(json.into())).await;
                }
                let _ = sink.send(Message::Close(None)).await;
                state.release_connection(&rate_limit_id).await;
                return;
            }
        }
    };

    // Ownership anchor. The authenticated user is the only durable identity this
    // socket has; the per-connection random suffix is NOT one. Keying sessions by
    // the connection (the previous behaviour) meant every page reload minted a
    // new "owner", so the resume guard rejected the user's own conversations —
    // history was readable over the dashboard RPC but never continuable, which
    // read as "the chat keeps resetting itself".
    let owner_tag = webchat_owner_tag(&auth_user);
    let user_id = format!("webchat:{owner_tag}:{session_suffix}");

    info!("WebChat connection established: {user_id}");

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

    // The (session_id, agent) pair the LAST session_info frame announced —
    // when an agent-config change lands, we rebuild + re-send exactly this
    // pair (as a `refresh: true` frame) so an open tab picks up new
    // name/icon/model without touching the client's session state.
    let mut announced_sid = session_id.clone();
    let mut announced_agent: Option<String> = None;
    let mut cfg_rx = crate::channel_reply::agent_config_events().subscribe();

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
            // Agent config changed (dashboard `agents.update` → registry
            // re-scan) — re-announce the current header so the open tab
            // reflects the new name/icon/model immediately. `Lagged` still
            // refreshes (we only need "something changed", not each event);
            // `Closed` is unreachable (static sender) but harmless to ignore.
            cfg_ev = cfg_rx.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                if !matches!(cfg_ev, Err(RecvError::Closed)) {
                    let mut info = build_session_info_frame(
                        &state, &announced_sid, announced_agent.as_deref(),
                    ).await;
                    if let ChatMessage::SessionInfo { ref mut refresh, .. } = info {
                        *refresh = true;
                    }
                    if let Ok(json) = serde_json::to_string(&info) {
                        if sink.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
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
                                let err = ChatMessage::error(format!("Invalid message format: {e}"));
                                if let Ok(json) = serde_json::to_string(&err) {
                                    let _ = sink.send(Message::Text(json.into())).await;
                                }
                                continue;
                            }
                        };

                        match chat_msg {
                            ChatMessage::UserMessage { content, session_id: custom_session, agent: requested_agent, attachments, conv } => {
                                // ── Forced password-change gate ──────────
                                // Mirrors `handlers.rs::dispatch`'s gate on the
                                // dashboard WS path (same error code, so a
                                // frontend can branch on one constant regardless
                                // of transport): the socket stayed open (see
                                // `authenticate_with`'s doc comment) but every
                                // turn gets a guidance-only reply instead of
                                // reaching the agent, until the account's
                                // `must_change_password` flag clears via the
                                // dashboard. Not a silent drop — the visitor
                                // sees exactly why nothing happened.
                                if must_change_password {
                                    let err = ChatMessage::error_coded(
                                        "此帳號尚未設定密碼，請先前往帳號設定完成密碼變更後再繼續使用。",
                                        crate::handlers::MUST_CHANGE_PASSWORD_ERROR_CODE,
                                    );
                                    if let Ok(json) = serde_json::to_string(&err) {
                                        let _ = sink.send(Message::Text(json.into())).await;
                                    }
                                    continue;
                                }
                                // Sanitize the client conversation nonce once —
                                // it scopes the session bucket AND is echoed on
                                // every reply frame so the browser attributes the
                                // reply to the conversation that started it.
                                let conv_nonce = sanitize_conv_nonce(conv.as_deref());
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
                                        if !exists {
                                            let err = ChatMessage::error(format!("unknown agent: {name}"));
                                            if let Ok(json) = serde_json::to_string(&err) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                            continue;
                                        }
                                        // ── C-ACL (P0-S1, 2026-08 audit): explicit
                                        // agent selection was previously gated on
                                        // NOTHING beyond "does this agent exist" —
                                        // any logged-in user (employee, manager)
                                        // could address any agent by name. Admins
                                        // pass unconditionally; everyone else must
                                        // hold a binding (any access level — see
                                        // `acl_allows_agent`). Checked per-message
                                        // (every `user_message` frame re-resolves
                                        // it), which is also what bounds the
                                        // "unbind but JWT keeps stale
                                        // access_levels" staleness window to
                                        // `ACL_CACHE_TTL` instead of the JWT's full
                                        // lifetime. The default (no `agent` field)
                                        // path below is untouched — out of scope.
                                        if !state.agent_access_allowed(&auth_user, name).await {
                                            warn!(
                                                user = %auth_user, agent = %name,
                                                "WebChat agent selection denied: not admin and not bound"
                                            );
                                            let err = ChatMessage::error_coded(
                                                "你沒有與此 AI 員工對話的權限，請聯絡管理員",
                                                "agent_forbidden",
                                            );
                                            if let Ok(json) = serde_json::to_string(&err) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                            continue;
                                        }
                                        Some(name.to_string())
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
                                    let owns_session = owns_webchat_session(&want, &owner_tag);
                                    if !owns_session {
                                        warn!(
                                            "WebChat resume denied: session {want} not owned by this user"
                                        );
                                        let err = ChatMessage::error("conversation not found");
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
                                                    let err = ChatMessage::error(
                                                        "this conversation belongs to a different AI staff member",
                                                    );
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
                                                    let err = ChatMessage::error(
                                                        "the AI staff member for this conversation is no longer available",
                                                    );
                                                    if let Ok(json) = serde_json::to_string(&err) {
                                                        let _ = sink.send(Message::Text(json.into())).await;
                                                    }
                                                    continue;
                                                }
                                                // ── C-ACL resume gap (P0-S2, 2026-08
                                                // audit follow-up): wave one only
                                                // gated an EXPLICIT `agent` field —
                                                // adopting a stored session's
                                                // non-default agent on resume never
                                                // ran `agent_access_allowed` at all,
                                                // so a since-unbound user could keep
                                                // talking to it forever through an
                                                // old session id. Re-run the same
                                                // fail-closed, 60s-cached check here
                                                // (`resume_needs_acl_check` scopes it
                                                // to non-default agents only — the
                                                // default/main agent branch above is
                                                // untouched, matching wave one).
                                                if resume_needs_acl_check(&stored_agent, &agent_id)
                                                    && !state
                                                        .agent_access_allowed(&auth_user, &stored_agent)
                                                        .await
                                                {
                                                    warn!(
                                                        user = %auth_user, agent = %stored_agent,
                                                        "WebChat resume denied: agent binding revoked since session was stored"
                                                    );
                                                    let err = ChatMessage::error_coded(
                                                        "你沒有與此 AI 員工對話的權限，請聯絡管理員",
                                                        "agent_forbidden",
                                                    );
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
                                            announced_sid = sid_owned.clone();
                                            announced_agent = effective_agent.clone();
                                        }
                                        Ok(None) => {
                                            let err = ChatMessage::error("conversation not found");
                                            if let Ok(json) = serde_json::to_string(&err) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("WebChat resume lookup failed: {e}");
                                            let err = ChatMessage::error("failed to load conversation");
                                            if let Ok(json) = serde_json::to_string(&err) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                            continue;
                                        }
                                    }
                                } else {
                                    // New / continue the current connection's
                                    // session. Compose the per-agent, per-conversation
                                    // session id — this id IS the isolation boundary
                                    // (see `compose_session_id`).
                                    sid_owned = compose_session_id(
                                        session_id.as_str(),
                                        effective_agent.as_deref(),
                                        conv_nonce.as_deref(),
                                    );
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
                                    // WP1.3: land inbound files under the resolved agent's dir.
                                    let attach_base = crate::channel_reply::resolve_attachment_base(
                                        state.ctx.as_ref(), Some(effective_agent_id),
                                    ).await;
                                    let refs = save_webchat_attachments(&attach_base, &attachments).await;
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
                                        // WebChat carries no addressable per-sender channel
                                        // identity (no `agent_binding`-style account to bind
                                        // in the dashboard), so `/rules off` can never resolve
                                        // a verified Admin/Manager here — `handle_rules_off`
                                        // correctly refuses with "use the dashboard" rather
                                        // than guessing at an identity.
                                        let reply = crate::chat_commands::handle_command(
                                            &cmd, &state.ctx, sid, effective_agent_id, true, "",
                                        ).await;
                                        let done = ChatMessage::AssistantDone {
                                            content: reply,
                                            tokens_used: 0,
                                            conv: conv_nonce.clone(),
                                            model: None,
                                            // Chat commands (`/xxx`) never go through
                                            // `build_reply_*` / O-4 at all — no marker
                                            // can exist on this path.
                                            artifact: None,
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
                                // O-4→O-3: the `_with_artifact` siblings return the
                                // same stripped text as `build_reply_for_agent` /
                                // `build_reply_with_session`, plus any O-4 pending-op
                                // marker mapped to an O-3 chat-artifact. WebChat is
                                // the only caller that reads the second half — see
                                // `channel_reply::build_reply_with_session_with_artifact`.
                                let work = async {
                                    match &effective_agent {
                                        Some(a) => {
                                            crate::channel_reply::build_reply_for_agent_with_artifact(
                                                &full_content, &state.ctx, a, sid, &user_id, Some(on_progress),
                                            )
                                            .await
                                        }
                                        None => {
                                            crate::channel_reply::build_reply_with_session_with_artifact(
                                                &full_content, &state.ctx, sid, &user_id, Some(on_progress),
                                            )
                                            .await
                                        }
                                    }
                                };
                                tokio::pin!(work);
                                // The model the backend ACTUALLY answered with
                                // (ProgressEvent::ModelInfo) — stamped onto the
                                // assistant_done frame below.
                                let mut actual_model: Option<String> = None;
                                let reply = loop {
                                    tokio::select! {
                                        r = &mut work => break r,
                                        Some(ev) = prx.recv() => {
                                            use crate::channel_reply::ProgressEvent;
                                            let content = ev.to_display();
                                            let msg = match ev {
                                                ProgressEvent::ModelInfo { model } => {
                                                    actual_model = Some(model);
                                                    continue;
                                                }
                                                ProgressEvent::ToolUse { tool, detail } => ChatMessage::Progress {
                                                    content, kind: Some("tool".into()), tool: Some(tool), detail,
                                                    conv: conv_nonce.clone(),
                                                },
                                                ProgressEvent::TodoUpdate { .. } => ChatMessage::Progress {
                                                    content, kind: Some("todo".into()), tool: None, detail: None,
                                                    conv: conv_nonce.clone(),
                                                },
                                                ProgressEvent::Keepalive => ChatMessage::Progress {
                                                    content, kind: Some("keepalive".into()), tool: None, detail: None,
                                                    conv: conv_nonce.clone(),
                                                },
                                                // C-P1: structured step boundary → dedicated `step` frame.
                                                ProgressEvent::Step(step) => ChatMessage::Step {
                                                    phase: step.phase.as_str().to_string(),
                                                    tool: step.tool,
                                                    summary: step.summary,
                                                    depth: step.depth,
                                                    ts: step.ts_ms,
                                                    conv: conv_nonce.clone(),
                                                },
                                            };
                                            if let Ok(json) = serde_json::to_string(&msg) {
                                                let _ = sink.send(Message::Text(json.into())).await;
                                            }
                                        }
                                    }
                                };
                                // O-4→O-3: split off the artifact half early — every
                                // transformation below (📎DELIVER parsing, cli-noise,
                                // branding, etc.) only ever operates on the text; the
                                // artifact rides along untouched to the final frame.
                                let (reply, operator_artifact) = reply;

                                // WP1.3 + live-verification fix (2026-08-15):
                                // 📎DELIVER: handling. WebChat has no in-band
                                // binary frame, so the archival copy IS the
                                // delivery — earlier code only validated the
                                // path and appended a "downloadable from the
                                // Files panel" note without archiving anything
                                // (a false promise) and without running the
                                // WP-4H delivery gate. Now each declared file
                                // goes through the same channel-agnostic
                                // stage_deliverable() as every other channel:
                                // validate → delivery gate → archive + ledger.
                                // Gate rejections surface as actionable text;
                                // the download note is only made when a copy
                                // really landed in attachments/.
                                let reply = {
                                    let (cleaned, paths) =
                                        crate::office_docs::parse_deliverables(&reply);
                                    if paths.is_empty() {
                                        reply
                                    } else {
                                        let agent_dir = state
                                            .ctx
                                            .home_dir
                                            .join("agents")
                                            .join(effective_agent_id);
                                        let mut delivered: Vec<String> = Vec::new();
                                        let mut failed: Vec<String> = Vec::new();
                                        for p in &paths {
                                            match crate::office_docs::stage_deliverable(
                                                p,
                                                &agent_dir,
                                                &state.ctx.home_dir,
                                                crate::artifacts::ArtifactOrigin::Declared,
                                                Some("webchat"),
                                            )
                                            .await
                                            {
                                                Ok(staged) if staged.archived => {
                                                    delivered.push(staged.filename)
                                                }
                                                Ok(staged) => failed.push(format!(
                                                    "{}：封存副本寫入失敗，檔案仍在工作目錄",
                                                    staged.filename
                                                )),
                                                Err(e) => {
                                                    // The gate/validator message is
                                                    // already zh-TW actionable; the
                                                    // raw path never echoes here.
                                                    let name = std::path::Path::new(p)
                                                        .file_name()
                                                        .and_then(|n| n.to_str())
                                                        .unwrap_or("檔案");
                                                    failed.push(format!("{name}：{e}"));
                                                }
                                            }
                                        }
                                        let mut notes: Vec<String> = Vec::new();
                                        if !delivered.is_empty() {
                                            notes.push(format!(
                                                "📎 已生成檔案：{}（可至 Dashboard 檔案面板下載）",
                                                delivered.join("、")
                                            ));
                                        }
                                        for f in &failed {
                                            notes.push(format!("⚠️ 交付被攔下——{f}"));
                                        }
                                        if notes.is_empty() {
                                            cleaned
                                        } else if cleaned.is_empty() {
                                            notes.join("\n")
                                        } else {
                                            format!("{cleaned}\n\n{}", notes.join("\n"))
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
                                    conv: conv_nonce.clone(),
                                    model: actual_model,
                                    artifact: operator_artifact,
                                };
                                if let Ok(json) = serde_json::to_string(&done) {
                                    if sink.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            _ => {
                                // Client sent an unexpected message type
                                let err = ChatMessage::error("Only user_message is accepted from client");
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
                // No agent ⇒ the reply path falls back to this same default
                // (channel_reply), so display it instead of an empty label.
                duduclaw_core::types::DEFAULT_PREFERRED_MODEL.to_string(),
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
        refresh: false,
    }
}

/// Sanitize a client-supplied conversation nonce (dashboard "conversation" id).
///
/// The nonce (a) scopes a webchat turn to its own server-side session bucket and
/// (b) is echoed on every reply frame so the browser attributes a finished reply
/// to the conversation that started it. Because it is interpolated into a session
/// id, it must not carry the `:` / `#` structural separators nor arbitrary text:
/// keep only `[A-Za-z0-9_-]`, cap the length, and return `None` for empty/garbage
/// so the caller falls back to the connection's default single-bucket behaviour.
fn sanitize_conv_nonce(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Compose the per-conversation server-side session id from the connection's
/// base session id, the (optional) routed agent, and the (optional, already
/// sanitized) conversation nonce.
///
/// Isolation invariant (the reason this is a named, tested function rather than
/// inline formatting): two dashboard conversations on the same connection must
/// land in DIFFERENT session buckets so their histories never mix. The channel
/// reply path derives the model's context solely from
/// `SessionManager::get_messages(session_id)` and folds it into the prompt — the
/// fresh-spawn `claude -p` path carries NO CLI-side session state (the legacy
/// `--resume` path was removed). This id is therefore the ONLY isolation
/// boundary: a distinct `#conv:<nonce>` yields a distinct history bucket; an
/// absent nonce keeps the byte-compatible single-bucket id (backward compatible
/// with clients that don't send one). The channel prefix (everything before the
/// first `:`) stays `webchat` so the access gate still classifies it.
fn compose_session_id(base: &str, agent: Option<&str>, conv_nonce: Option<&str>) -> String {
    let mut composed = base.to_string();
    if let Some(a) = agent {
        composed = format!("{composed}#agent:{a}");
    }
    if let Some(c) = conv_nonce {
        composed = format!("{composed}#conv:{c}");
    }
    composed
}

/// Decode, size-check, and persist WebChat attachments to
/// `<base_dir>/attachments/`, returning markdown file-reference lines to append
/// to the prompt. Invalid or oversized attachments are logged and skipped
/// (never abort the whole message). `base_dir` is the per-agent dir (WP1.3),
/// falling back to the shared home dir.
async fn save_webchat_attachments(
    base_dir: &std::path::Path,
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
        match crate::media::save_attachment_in_base(base_dir, &data, &filename).await {
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
mod resume_ownership_tests {
    use super::*;

    /// The tag must be identical across connections — that is the entire fix.
    /// Keying on the per-connection UUID meant a page reload orphaned every
    /// conversation the user had just had.
    #[test]
    fn owner_tag_is_stable_for_the_same_user() {
        assert_eq!(webchat_owner_tag("user-abc"), webchat_owner_tag("user-abc"));
        assert_ne!(webchat_owner_tag("user-abc"), webchat_owner_tag("user-xyz"));
        assert_eq!(webchat_owner_tag("user-abc").len(), 12);
    }

    /// The tag must not carry the account identifier: session ids reach the
    /// dashboard, and "who owns this conversation" is not public information.
    #[test]
    fn owner_tag_does_not_leak_the_user_id() {
        let tag = webchat_owner_tag("boss@customer.com");
        assert!(!tag.contains("boss"), "{tag}");
        assert!(!tag.contains('@'), "{tag}");
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()), "{tag}");
    }

    #[test]
    fn owner_may_resume_their_own_sessions_including_derived_ones() {
        let tag = webchat_owner_tag("user-abc");
        let base = format!("webchat:webchat:{tag}:conn1");
        assert!(owns_webchat_session(&base, &tag));
        // A different connection's suffix is still the same user — this is the
        // page-reload case that used to fail.
        assert!(owns_webchat_session(&format!("webchat:webchat:{tag}:conn2"), &tag));
        // Per-agent / per-conversation derived ids.
        assert!(owns_webchat_session(&format!("{base}#agent:sales#conv:x1"), &tag));
    }

    #[test]
    fn another_users_session_is_refused() {
        let mine = webchat_owner_tag("user-abc");
        let theirs = webchat_owner_tag("user-xyz");
        assert!(!owns_webchat_session(&format!("webchat:webchat:{theirs}:conn1"), &mine));
    }

    #[test]
    fn cross_channel_ids_are_refused() {
        let tag = webchat_owner_tag("user-abc");
        for want in [
            "telegram:12345",
            "discord:thread:999",
            "line:U0001",
            // An internal work session (cron / delegation) is not a webchat one.
            "agent:sales#cron",
        ] {
            assert!(!owns_webchat_session(want, &tag), "accepted {want}");
        }
    }

    #[test]
    fn a_prefix_lookalike_tag_is_refused() {
        // The trailing separator is what stops `…{tag}evil:` from matching —
        // an unanchored prefix check would let a crafted id through.
        let tag = webchat_owner_tag("user-abc");
        assert!(!owns_webchat_session(&format!("webchat:webchat:{tag}evil:conn1"), &tag));
    }

    #[test]
    fn an_empty_owner_tag_never_owns_anything() {
        // Fail closed: an unauthenticated/blank anchor must not become a
        // wildcard that matches every session.
        assert!(!owns_webchat_session("webchat:webchat::conn1", ""));
        assert!(!owns_webchat_session("webchat:webchat:abc:conn1", ""));
    }
}

#[cfg(test)]
mod widget_auth_tests {
    use super::*;

    #[test]
    fn widget_gate_fails_closed_on_every_miss() {
        let dir = tempfile::tempdir().unwrap();
        // No config at all.
        assert!(widget_authenticate(dir.path(), "whatever-key-1234").is_err());
        // Configured but disabled.
        std::fs::write(
            dir.path().join("config.toml"),
            "[webchat]\npublic_widget = false\nwidget_key = \"0123456789abcdef\"\n",
        )
        .unwrap();
        assert!(widget_authenticate(dir.path(), "0123456789abcdef").is_err());
        // Enabled but key too short.
        std::fs::write(
            dir.path().join("config.toml"),
            "[webchat]\npublic_widget = true\nwidget_key = \"short\"\n",
        )
        .unwrap();
        assert!(widget_authenticate(dir.path(), "short").is_err());
        // Enabled, long key, wrong presentation.
        std::fs::write(
            dir.path().join("config.toml"),
            "[webchat]\npublic_widget = true\nwidget_key = \"0123456789abcdef\"\n",
        )
        .unwrap();
        assert!(widget_authenticate(dir.path(), "0123456789abcdeX").is_err());
    }

    #[test]
    fn widget_visitors_get_unique_identities() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[webchat]\npublic_widget = true\nwidget_key = \"0123456789abcdef\"\n",
        )
        .unwrap();
        let a = widget_authenticate(dir.path(), "0123456789abcdef").unwrap();
        let b = widget_authenticate(dir.path(), "0123456789abcdef").unwrap();
        assert!(a.starts_with("widget-visitor:"));
        // Unique per connection ⇒ resume guard can never cross visitors.
        assert_ne!(a, b);
        assert_ne!(webchat_owner_tag(&a), webchat_owner_tag(&b));
    }
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
        assert_eq!(result, Ok((user.id.clone(), false)));
    }

    #[test]
    fn authenticate_rejects_garbage_token() {
        let (jwt, db, _dir) = fixtures();
        let result = authenticate_with(&jwt, &db, "not.a.jwt");
        assert!(result.is_err(), "garbage token must be rejected");
    }

    /// C1 fix (mirrors `server.rs::jwt_account_gate`): a `must_change_password`
    /// account is allowed to CONNECT — the flag rides along on the `Ok`
    /// tuple instead of refusing the handshake. `handle_chat_socket` is the
    /// layer that turns the flag into a guidance-only reply per turn (not
    /// unit-tested here — no live socket in this test module, see the
    /// module doc on `authenticate_with`).
    #[test]
    fn authenticate_allows_must_change_password_user_with_flag_set() {
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
        assert_eq!(
            result,
            Ok((admin.id.clone(), true)),
            "a user pending a forced password change must still be allowed to \
             connect, with the flag surfaced so the caller can degrade to \
             guidance-only replies"
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

    // ── Conversation-nonce attribution (misrouted-reply fix) ────────

    #[test]
    fn user_message_carries_conv_nonce() {
        // The frame parses the `conv` view/bucket token.
        let msg: ChatMessage = serde_json::from_str(
            r#"{"type":"user_message","content":"hi","session_id":"webchat:x","conv":"c-42"}"#,
        )
        .expect("parse");
        match msg {
            ChatMessage::UserMessage { conv, .. } => assert_eq!(conv.as_deref(), Some("c-42")),
            other => panic!("expected UserMessage, got {other:?}"),
        }
    }

    #[test]
    fn user_message_without_conv_is_none() {
        // Legacy frame (no `conv`) → None → single-bucket byte-compatible path.
        let msg: ChatMessage =
            serde_json::from_str(r#"{"type":"user_message","content":"hi","session_id":"webchat:x"}"#)
                .expect("parse");
        match msg {
            ChatMessage::UserMessage { conv, .. } => assert_eq!(conv, None),
            other => panic!("expected UserMessage, got {other:?}"),
        }
    }

    #[test]
    fn assistant_done_echoes_conv_and_skips_when_absent() {
        // With a nonce, the wire carries `conv` so the browser can attribute the
        // reply to the originating conversation.
        let with = ChatMessage::AssistantDone {
            content: "done".into(),
            tokens_used: 3,
            conv: Some("c-42".into()),
            model: Some("claude-sonnet-4-6".into()),
            artifact: None,
        };
        let json = serde_json::to_string(&with).expect("serialize");
        assert!(json.contains(r#""conv":"c-42""#), "conv must be on the wire: {json}");
        assert!(
            json.contains(r#""model":"claude-sonnet-4-6""#),
            "actual model must be on the wire: {json}"
        );

        // Without a nonce, `conv` is omitted entirely (old-client compatible).
        let without = ChatMessage::AssistantDone {
            content: "done".into(),
            tokens_used: 3,
            conv: None,
            model: None,
            artifact: None,
        };
        let json = serde_json::to_string(&without).expect("serialize");
        assert!(!json.contains("conv"), "absent conv must not serialize: {json}");
        assert!(!json.contains("model"), "absent model must not serialize: {json}");
        assert!(!json.contains("artifact"), "absent artifact must not serialize: {json}");
    }

    #[test]
    fn assistant_done_carries_artifact_when_o4_marker_maps_to_one() {
        // O-4→O-3: when `channel_reply::build_reply_*_with_artifact` maps a
        // pending-op marker to an O-3 chat-artifact, it rides the wire as a
        // plain JSON value on this frame — no new envelope, no separate frame.
        let artifact = serde_json::json!({
            "type": "confirm_action",
            "payload": { "action": "restart" },
        });
        let done = ChatMessage::AssistantDone {
            content: "「電源操作（重開機／關機）」會變更這台機器的狀態，請先確認：要執行嗎？".into(),
            tokens_used: 12,
            conv: None,
            model: None,
            artifact: Some(artifact.clone()),
        };
        let json = serde_json::to_string(&done).expect("serialize");
        assert!(
            json.contains(r#""artifact":{"payload":{"action":"restart"},"type":"confirm_action"}"#)
                || json.contains(r#""artifact":{"type":"confirm_action","payload":{"action":"restart"}}"#),
            "artifact must serialize as a plain JSON value on the wire: {json}"
        );
        // The tag itself must never appear on the wire — only WebChat's own
        // `strip_operator_pending_marker` call guarantees that in production,
        // but this test locks the frame's own contract: `content` here is
        // already-stripped human text, exactly what production sends.
        assert!(!json.contains("system_operator_pending"));
    }

    #[test]
    fn sanitize_conv_nonce_keeps_safe_chars() {
        assert_eq!(sanitize_conv_nonce(Some("c-42_AbZ")).as_deref(), Some("c-42_AbZ"));
    }

    #[test]
    fn sanitize_conv_nonce_strips_separators_and_garbage() {
        // `:` / `#` (session-id separators) and other punctuation are removed so
        // a client can never inject extra bucket structure or break the id.
        assert_eq!(sanitize_conv_nonce(Some("a:b#c/d e")).as_deref(), Some("abcde"));
    }

    #[test]
    fn sanitize_conv_nonce_rejects_empty_and_pure_garbage() {
        assert_eq!(sanitize_conv_nonce(None), None);
        assert_eq!(sanitize_conv_nonce(Some("")), None);
        assert_eq!(sanitize_conv_nonce(Some("   ")), None);
        assert_eq!(sanitize_conv_nonce(Some("::##")), None);
    }

    #[test]
    fn sanitize_conv_nonce_caps_length() {
        let long = "a".repeat(200);
        assert_eq!(sanitize_conv_nonce(Some(&long)).map(|s| s.len()), Some(64));
    }

    #[test]
    fn conv_bucket_id_is_distinct_per_conversation_and_owned() {
        // Two conversations on the same connection compose distinct buckets, both
        // still owned by (prefixed with) the connection's session id — so the
        // resume-ownership guard (`starts_with("{session_id}#")`) accepts them.
        let connection = "webchat:webchat:127.0.0.1:00c7766f";
        let a = format!("{connection}#conv:{}", sanitize_conv_nonce(Some("A1")).unwrap());
        let b = format!("{connection}#conv:{}", sanitize_conv_nonce(Some("B2")).unwrap());
        assert_ne!(a, b, "distinct conversations → distinct session buckets");
        assert!(a.starts_with(&format!("{connection}#")));
        assert!(b.starts_with(&format!("{connection}#")));
        assert_eq!(a.split(':').next(), Some("webchat"), "channel prefix preserved");
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
    fn compose_session_id_differs_per_conv_nonce() {
        // The `-p` fresh-spawn isolation guarantee: same connection, same agent,
        // two different conversation nonces MUST produce different session ids so
        // their `get_messages(session_id)` history buckets never mix.
        let base = "webchat:webchat:127.0.0.1:00c7766f";
        let a = compose_session_id(base, Some("sales-bot"), Some("A1"));
        let b = compose_session_id(base, Some("sales-bot"), Some("B2"));
        assert_ne!(a, b, "distinct conv nonce → distinct session bucket");
        assert_eq!(a, "webchat:webchat:127.0.0.1:00c7766f#agent:sales-bot#conv:A1");
    }

    #[test]
    fn compose_session_id_is_stable_for_same_conv() {
        // Same inputs → byte-identical id (deterministic history bucket across
        // turns of one conversation).
        let base = "webchat:x";
        let first = compose_session_id(base, Some("bot"), Some("c-42"));
        let second = compose_session_id(base, Some("bot"), Some("c-42"));
        assert_eq!(first, second);
    }

    #[test]
    fn compose_session_id_absent_nonce_is_single_bucket() {
        // No nonce → byte-compatible with clients that don't send one. No `#conv`
        // segment appended.
        let base = "webchat:x";
        assert_eq!(compose_session_id(base, None, None), "webchat:x");
        assert_eq!(compose_session_id(base, Some("bot"), None), "webchat:x#agent:bot");
        assert!(!compose_session_id(base, Some("bot"), None).contains("#conv:"));
    }

    #[test]
    fn compose_session_id_preserves_channel_prefix() {
        // Channel prefix (before the first `:`) must stay "webchat" so the access
        // gate still classifies the session.
        let composed = compose_session_id("webchat:peer:abcd", Some("bot"), Some("c1"));
        assert_eq!(composed.split(':').next(), Some("webchat"));
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

    // ── C-ACL (P0-S1, 2026-08 audit): explicit-agent selection ACL ─────────
    //
    // These target the pure decision layer (`acl_allows_agent` /
    // `resolve_agent_acl` / `load_acl_snapshot`) rather than the socket loop
    // itself, per the same rationale as `authenticate_with` above: no live
    // WebSocket needed to exercise the actual security decision.

    fn snapshot(role: UserRole, bound: &[&str]) -> AclSnapshot {
        AclSnapshot {
            role,
            bound_agents: bound.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn acl_denies_non_admin_without_a_binding() {
        let snap = snapshot(UserRole::Employee, &["sales-bot"]);
        assert!(!acl_allows_agent(&snap, "other-agent"));
    }

    #[test]
    fn acl_allows_non_admin_with_a_binding() {
        let snap = snapshot(UserRole::Employee, &["sales-bot"]);
        assert!(acl_allows_agent(&snap, "sales-bot"));
    }

    #[test]
    fn acl_allows_admin_regardless_of_bindings() {
        let snap = snapshot(UserRole::Admin, &[]);
        assert!(acl_allows_agent(&snap, "any-agent"));
    }

    #[test]
    fn acl_manager_is_not_admin_and_still_needs_a_binding() {
        // Manager sits below Admin in `UserRole::level()` — same rule as
        // Employee applies; only Admin is unconditional.
        let snap = snapshot(UserRole::Manager, &["ops-bot"]);
        assert!(acl_allows_agent(&snap, "ops-bot"));
        assert!(!acl_allows_agent(&snap, "finance-bot"));
    }

    // ── C-ACL resume gap (P0-S2, 2026-08 audit follow-up) ──────────────────
    //
    // Wave one (`acl_allows_agent`) only gated an EXPLICIT `agent` field on a
    // `user_message`; resuming a stored session adopted its stored agent
    // straight from `SessionManager` with no re-check. These tests target
    // the extracted pure criterion (`resume_needs_acl_check`) and compose it
    // with the already-covered `acl_allows_agent` predicate to document the
    // exact resume-turn decision the socket loop now makes — the socket loop
    // itself is not exercised (same rationale as the P0-S1 section above).

    #[test]
    fn resume_needs_check_denies_when_binding_was_revoked() {
        // Session was stored while the user was bound to "sales-bot"; the
        // binding has since been removed (`bound` no longer contains it).
        let snap = snapshot(UserRole::Employee, &["other-bot"]);
        let default_agent = "main-agent";
        let stored_agent = "sales-bot";

        assert!(resume_needs_acl_check(stored_agent, default_agent));
        assert!(
            !acl_allows_agent(&snap, stored_agent),
            "resume must deny once the binding is gone"
        );
    }

    #[test]
    fn resume_needs_check_allows_when_still_bound() {
        let snap = snapshot(UserRole::Employee, &["sales-bot"]);
        let default_agent = "main-agent";
        let stored_agent = "sales-bot";

        assert!(resume_needs_acl_check(stored_agent, default_agent));
        assert!(
            acl_allows_agent(&snap, stored_agent),
            "resume must pass while the binding still holds"
        );
    }

    #[test]
    fn resume_needs_check_allows_admin_regardless_of_bindings() {
        let snap = snapshot(UserRole::Admin, &[]);
        let default_agent = "main-agent";
        let stored_agent = "sales-bot";

        assert!(resume_needs_acl_check(stored_agent, default_agent));
        assert!(
            acl_allows_agent(&snap, stored_agent),
            "admin must resume into any stored agent unconditionally"
        );
    }

    #[test]
    fn resume_needs_check_is_false_for_the_default_agent_session() {
        // The main/default-agent resume path is wave one's byte-compatible
        // scope and must never be gated here, even for a user with zero
        // bindings — this is the "default agent path unaffected" guarantee.
        let default_agent = "main-agent";
        assert!(!resume_needs_acl_check(default_agent, default_agent));

        // Sanity: a snapshot that would deny every non-default agent must
        // still be irrelevant to the default-agent branch, since the gate
        // short-circuits on `resume_needs_acl_check` before ever consulting
        // the ACL snapshot (mirrors the `if resume_needs_acl_check(..) &&
        // !agent_access_allowed(..)` short-circuit at the real call site).
        let snap = snapshot(UserRole::Employee, &[]);
        assert!(
            !resume_needs_acl_check(default_agent, default_agent) || acl_allows_agent(&snap, default_agent),
            "default agent session must resume regardless of ACL state"
        );
    }

    #[test]
    fn resume_needs_check_is_symmetric_on_agent_identity_not_string_shape() {
        // Different agent ids, even ones that share a default-agent-like
        // name pattern, must still be recognised as non-default (exact
        // string equality — no prefix/substring shortcut).
        assert!(resume_needs_acl_check("main-agent-2", "main-agent"));
        assert!(!resume_needs_acl_check("main-agent", "main-agent"));
    }

    #[test]
    fn resolve_agent_acl_denies_on_lookup_failure() {
        // Fail-closed: a DB error / unreachable auth service must deny, never
        // fall through to "allow".
        let lookup: Result<AclSnapshot, String> = Err("auth service unavailable".to_string());
        assert!(!resolve_agent_acl(&lookup, "any-agent"));
    }

    #[test]
    fn resolve_agent_acl_matches_acl_allows_agent_on_success() {
        let lookup: Result<AclSnapshot, String> = Ok(snapshot(UserRole::Employee, &["sales-bot"]));
        assert!(resolve_agent_acl(&lookup, "sales-bot"));
        assert!(!resolve_agent_acl(&lookup, "other-agent"));
    }

    #[test]
    fn load_acl_snapshot_fails_closed_for_unknown_user() {
        let (_jwt, db, _dir) = fixtures();
        // Mirrors a widget-visitor id or a user deleted mid-session — neither
        // is a row `get_user` can find, and that must deny rather than
        // silently defaulting to some permissive shape.
        let result = load_acl_snapshot(&db, "no-such-user-id");
        assert!(result.is_err(), "an unknown user id must fail closed");
    }

    #[test]
    fn load_acl_snapshot_reflects_role_and_bindings() {
        let (_jwt, db, _dir) = fixtures();
        let user = db
            .create_user("carol@example.com", "Carol", "pw-strong-123", UserRole::Employee)
            .expect("create user");
        db.bind_agent(&user.id, "sales-bot", AccessLevel::Viewer)
            .expect("bind agent");

        let snap = load_acl_snapshot(&db, &user.id).expect("lookup should succeed");
        assert_eq!(snap.role, UserRole::Employee);
        assert!(snap.bound_agents.contains("sales-bot"));
        assert!(!snap.bound_agents.contains("other-bot"));
    }

    #[test]
    fn load_acl_snapshot_admin_role_is_carried_through() {
        let (_jwt, db, _dir) = fixtures();
        let admin = db
            .create_user("dana@example.com", "Dana", "pw-strong-123", UserRole::Admin)
            .expect("create user");

        let snap = load_acl_snapshot(&db, &admin.id).expect("lookup should succeed");
        assert_eq!(snap.role, UserRole::Admin);
        // Admin needs no bindings — `acl_allows_agent` short-circuits on role.
        assert!(acl_allows_agent(&snap, "literally-any-agent"));
    }

    // ── ChatMessage::Error `code` field (P0-S1 wire-format addition) ───────

    #[test]
    fn error_helper_omits_code_on_the_wire() {
        let msg = ChatMessage::error("plain error");
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains(r#""message":"plain error""#));
        assert!(!json.contains("\"code\""), "absent code must not serialize: {json}");
    }

    #[test]
    fn error_coded_helper_carries_code_on_the_wire() {
        let msg = ChatMessage::error_coded("你沒有與此 AI 員工對話的權限，請聯絡管理員", "agent_forbidden");
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains(r#""code":"agent_forbidden""#), "{json}");
    }
}
