//! Google Chat channel — HTTP-endpoint Chat app.
//!
//! Inbound: Google POSTs interaction events to `POST /webhook/googlechat`,
//! authenticated with a JWT signed by `chat@system.gserviceaccount.com`
//! whose audience is the Cloud **project number** (fail-closed verification
//! in `webhook_jwt`). We ACK the POST immediately (Google retries on
//! timeouts and the synchronous window is only 30s — too short for LLM
//! replies) and deliver the reply asynchronously via
//! `spaces.messages.create` using a service-account token (scope
//! `chat.bot`).
//!
//! UX: Google Chat has no typing indicator API, so the channel posts a
//! placeholder message ("思考中…") right away and PATCHes it in place with
//! progress events (tool activity / TODO board) and finally the reply —
//! the closest native equivalent to typing + edit-in-place progress.
//!
//! Formatting: Chat text messages use Google's own markup (not markdown):
//! `*bold*`, `~strike~`, `<url|text>` links, no headers/tables — the
//! conversion lives in `markdown_render::to_googlechat_text`.
//!
//! Config (`config.toml [channels]`):
//! - `googlechat_project_number` — Cloud project number (JWT audience)
//! - `googlechat_service_account_json` (`_enc`) — service-account JSON key

use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use duduclaw_core::truncate_bytes;

use crate::channel_reply::{build_reply_with_session, set_channel_connected, ReplyContext};

const CHAT_API: &str = "https://chat.googleapis.com/v1";
const CHAT_ISSUER: &str = "chat@system.gserviceaccount.com";
const CHAT_JWKS_URL: &str =
    "https://www.googleapis.com/service_accounts/v1/jwk/chat@system.gserviceaccount.com";
const CHAT_SCOPE: &str = "https://www.googleapis.com/auth/chat.bot";

/// Google Chat text messages accept up to 32,000 bytes; chunk well below
/// for display comfort.
const GCHAT_TEXT_CHUNK: usize = 4000;

pub struct GoogleChatState {
    pub(crate) ctx: Arc<ReplyContext>,
    project_number: String,
    creds: GoogleChatCreds,
}

/// Service-account credentials + token cache — separable from the webhook
/// state so delegation forwarding / Computer Use can send without a
/// `ReplyContext`.
pub struct GoogleChatCreds {
    /// From the service-account JSON key.
    client_email: String,
    private_key: String,
    token_uri: String,
    /// Cached OAuth token (access_token, fetched_at).
    token: RwLock<(String, std::time::Instant)>,
    http: reqwest::Client,
}

impl GoogleChatCreds {
    /// Parse a service-account JSON key into a creds handle.
    pub(crate) fn from_service_account_json(sa_json: &str) -> Option<GoogleChatCreds> {
        let sa: serde_json::Value = serde_json::from_str(sa_json).ok()?;
        Some(GoogleChatCreds {
            client_email: sa.get("client_email")?.as_str()?.to_string(),
            private_key: sa.get("private_key")?.as_str()?.to_string(),
            token_uri: sa
                .get("token_uri")
                .and_then(|v| v.as_str())
                .unwrap_or("https://oauth2.googleapis.com/token")
                .to_string(),
            token: RwLock::new((String::new(), std::time::Instant::now())),
            http: reqwest::Client::new(),
        })
    }

    /// Build from global config; `None` when the channel isn't configured.
    pub(crate) async fn from_config(home_dir: &std::path::Path) -> Option<GoogleChatCreds> {
        let sa_json = read_config(home_dir, "googlechat_service_account_json").await?;
        if sa_json.trim().is_empty() {
            return None;
        }
        Self::from_service_account_json(&sa_json)
    }

    /// Get (or refresh) the service-account access token (JWT-bearer grant).
    async fn get_token(&self) -> Result<String, String> {
        {
            let cached = self.token.read().await;
            // Tokens last 3600s; refresh 5 minutes early.
            if !cached.0.is_empty() && cached.1.elapsed().as_secs() < 3300 {
                return Ok(cached.0.clone());
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let claims = serde_json::json!({
            "iss": self.client_email,
            "scope": CHAT_SCOPE,
            "aud": self.token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(self.private_key.as_bytes())
            .map_err(|e| format!("service-account key: {e}"))?;
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(|e| format!("assertion sign: {e}"))?;

        let resp = self
            .http
            .post(&self.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("token request: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("token status {status}: {}", truncate_bytes(&body, 200)));
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| format!("token parse: {e}"))?;
        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or("no access_token in response")?
            .to_string();
        *self.token.write().await = (token.clone(), std::time::Instant::now());
        Ok(token)
    }
}

/// Read config and build the Google Chat webhook router. Returns `None`
/// when the channel isn't configured.
pub async fn start_googlechat_webhook(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<Router> {
    let project_number = read_config(home_dir, "googlechat_project_number").await?;
    if project_number.trim().is_empty() {
        return None;
    }
    let Some(creds) = GoogleChatCreds::from_config(home_dir).await else {
        error!("Google Chat: googlechat_service_account_json missing or not a valid service-account key");
        return None;
    };

    let state = Arc::new(GoogleChatState {
        ctx: ctx.clone(),
        project_number,
        creds,
    });

    // Verify credentials eagerly so the dashboard shows real status.
    match state.creds.get_token().await {
        Ok(_) => {
            info!("✅ Google Chat webhook ready at /webhook/googlechat");
            set_channel_connected(&ctx.channel_status, "googlechat", true, None, Some(&ctx.event_tx)).await;
        }
        Err(e) => {
            warn!("Google Chat: service-account auth failed (webhook still mounted): {e}");
            set_channel_connected(&ctx.channel_status, "googlechat", false, Some(e), Some(&ctx.event_tx)).await;
        }
    }

    Some(
        Router::new()
            .route("/webhook/googlechat", post(webhook_handler))
            .with_state(state),
    )
}

async fn read_config(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

async fn webhook_handler(
    State(state): State<Arc<GoogleChatState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // ── Verify the Google-signed JWT (fail closed) ──
    let auth = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let Some(token) = crate::webhook_jwt::bearer_token(auth) else {
        warn!("Google Chat webhook: missing bearer token");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    };
    if let Err(e) = crate::webhook_jwt::verify_rs256(
        &state.creds.http,
        token,
        CHAT_JWKS_URL,
        CHAT_ISSUER,
        &state.project_number,
    )
    .await
    {
        warn!("Google Chat webhook: JWT verification failed: {e}");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    let event: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("Google Chat webhook parse error: {e}");
            return (StatusCode::BAD_REQUEST, "bad request").into_response();
        }
    };

    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "MESSAGE" => {
            let st = state.clone();
            tokio::spawn(async move { handle_message(&st, &event).await });
            // ACK synchronously with an empty body — the reply arrives
            // asynchronously via the REST API.
            (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response()
        }
        "ADDED_TO_SPACE" => {
            let space = event.pointer("/space/displayName").and_then(|v| v.as_str()).unwrap_or("(unknown)");
            info!("Google Chat: added to space {space}");
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({
                    "text": "🐾 DuDuClaw 已加入！直接傳訊息即可對話。"
                })),
            )
                .into_response()
        }
        _ => (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response(),
    }
}

async fn handle_message(state: &Arc<GoogleChatState>, event: &serde_json::Value) {
    let message = event.get("message").cloned().unwrap_or_default();
    // argumentText has the app @mention stripped; fall back to raw text.
    let text = message
        .get("argumentText")
        .or_else(|| message.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return;
    }

    let space = message
        .pointer("/space/name")
        .or_else(|| event.pointer("/space/name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if space.is_empty() {
        warn!("Google Chat: MESSAGE event without space name");
        return;
    }
    let thread = message
        .pointer("/thread/name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let sender = message
        .pointer("/sender/displayName")
        .and_then(|v| v.as_str())
        .unwrap_or("someone")
        .to_string();
    let sender_id = message
        .pointer("/sender/name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    info!("📩 Google Chat [{sender}]: {}", truncate_bytes(&text, 80));

    // ── Placeholder message (Chat has no typing API) ──
    let placeholder = create_message(&state.creds, &space, thread.as_deref(), "🤔 思考中…").await;

    // ── Progress: PATCH the placeholder in place ──
    let progress_state = state.clone();
    let progress_name = placeholder.clone();
    let last_progress = Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .unwrap_or_else(std::time::Instant::now),
    ));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        let Some(name) = progress_name.clone() else { return };
        // Step events are a dashboard-only agentic-tree signal — never rendered
        // as channel text (would be an empty message).
        if matches!(event, crate::channel_reply::ProgressEvent::Step { .. }) {
            return;
        }
        let is_todo = matches!(event, crate::channel_reply::ProgressEvent::TodoUpdate { .. });
        {
            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
            if !is_todo && last.elapsed().as_secs() < 30 {
                return;
            }
            *last = std::time::Instant::now();
        }
        let st = progress_state.clone();
        let msg_text = event.to_display();
        tokio::spawn(async move {
            update_message(&st.creds, &name, &msg_text).await;
        });
    });

    // ── Chat commands ──
    let session_id = format!("googlechat:{space}");
    if crate::chat_commands::is_command(&text) {
        if let Some(cmd) = crate::chat_commands::parse_command(&text, None) {
            let agent_id = {
                let reg = state.ctx.registry.read().await;
                reg.main_agent().map(|a| a.config.agent.name.clone()).unwrap_or_default()
            };
            let reply =
                crate::chat_commands::handle_command(&cmd, &state.ctx, &session_id, &agent_id, true).await;
            deliver_reply(&state.creds, &space, thread.as_deref(), placeholder.as_deref(), &reply).await;
            return;
        }
    }

    let reply = build_reply_with_session(&text, &state.ctx, &session_id, &sender_id, Some(on_progress)).await;

    if reply.trim().is_empty() {
        warn!("Google Chat: reply is empty — cleaning up placeholder");
        if let Some(name) = placeholder.as_deref() {
            update_message(&state.creds, name, "⚠️ 沒有產生回覆，請再試一次。").await;
        }
        return;
    }

    deliver_reply(&state.creds, &space, thread.as_deref(), placeholder.as_deref(), &reply).await;
}

/// Deliver the final reply: first chunk replaces the placeholder (PATCH),
/// remaining chunks are new messages in the same thread.
async fn deliver_reply(
    creds: &GoogleChatCreds,
    space: &str,
    thread: Option<&str>,
    placeholder: Option<&str>,
    reply_markdown: &str,
) {
    let formatted = crate::markdown_render::to_googlechat_text(reply_markdown);
    let chunks = crate::channel_format::split_text(&formatted, GCHAT_TEXT_CHUNK);
    let mut chunks = chunks.iter();

    if let (Some(name), Some(first)) = (placeholder, chunks.next()) {
        update_message(creds, name, first).await;
    }
    for chunk in chunks {
        create_message(creds, space, thread, chunk).await;
    }
}

/// Send markdown text to a space (proactive / delegation-forwarding path).
/// The Chat app must already be a member of the space.
pub async fn send_text_to_space(
    home_dir: &Path,
    space: &str,
    markdown: &str,
) -> Result<(), String> {
    let creds = GoogleChatCreds::from_config(home_dir)
        .await
        .ok_or("Google Chat channel not configured")?;
    let formatted = crate::markdown_render::to_googlechat_text(markdown);
    for chunk in crate::channel_format::split_text(&formatted, GCHAT_TEXT_CHUNK) {
        if create_message(&creds, space, None, &chunk).await.is_none() {
            return Err("Google Chat send failed".into());
        }
    }
    Ok(())
}

/// spaces.messages.create — returns the created message `name` for edits.
async fn create_message(
    creds: &GoogleChatCreds,
    space: &str,
    thread: Option<&str>,
    text: &str,
) -> Option<String> {
    let token = match creds.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Google Chat token error: {e}");
            return None;
        }
    };
    let mut body = serde_json::json!({ "text": text });
    let mut url = format!("{CHAT_API}/{space}/messages");
    if let Some(th) = thread {
        body["thread"] = serde_json::json!({ "name": th });
        url.push_str("?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD");
    }
    match creds.http.post(&url).bearer_auth(&token).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())),
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Google Chat send failed ({status}): {}", truncate_bytes(&body, 200));
            None
        }
        Err(e) => {
            error!("Google Chat send error: {e}");
            None
        }
    }
}

/// spaces.messages.patch — edit a message's text in place.
async fn update_message(creds: &GoogleChatCreds, message_name: &str, text: &str) {
    let token = match creds.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Google Chat token error: {e}");
            return;
        }
    };
    let url = format!("{CHAT_API}/{message_name}?updateMask=text");
    let body = serde_json::json!({ "text": text });
    match creds.http.patch(&url).bearer_auth(&token).json(&body).send().await {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("Google Chat update failed ({status}): {}", truncate_bytes(&body, 200));
        }
        Err(e) => warn!("Google Chat update error: {e}"),
        _ => {}
    }
}
