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
//! Inbound attachments: files uploaded to Chat are downloaded through the
//! media API (`GET /v1/media/{resourceName}?alt=media`, service-account
//! token) and saved under the agent's `attachments/`; linked Drive files are
//! skipped explicitly (the `chat.bot` scope cannot read Drive content) with a
//! text note. Attachment failures degrade — they never block the message.
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
            let product = crate::branding::effective_product_name(&duduclaw_core::platform::duduclaw_home());
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({
                    "text": format!("🐾 {product} 已加入！直接傳訊息即可對話。")
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
    // Attachment-only messages carry no text — parse attachments before the
    // empty-text bail so a bare file upload still reaches the agent.
    let attachments = parse_gchat_attachments(&message);
    if text.is_empty() && attachments.is_empty() {
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

    // ── Inbound attachments (WP1.3 parity with the other 8 channels) ──
    // Uploaded-to-Chat files are fetched through the media API with the
    // service-account token and saved under the agent's `attachments/`; Drive
    // links need a Drive scope the `chat.bot` token does not carry, so they
    // degrade to an explicit skip note. Every failure degrades to a text
    // reference — attachment problems never block the message itself.
    let mut attachment_lines: Vec<String> = Vec::new();
    if !attachments.is_empty() {
        let attach_base =
            crate::channel_reply::resolve_attachment_base(state.ctx.as_ref(), None).await;
        for att in &attachments {
            let filename = att.effective_filename();
            match &att.reference {
                GchatAttachmentRef::Uploaded { resource_name } => {
                    match download_media(&state.creds, resource_name).await {
                        Ok(bytes) => {
                            let mt = crate::media::media_type_from_mime(&att.content_type);
                            match crate::media::save_attachment_in_base(
                                &attach_base,
                                &bytes,
                                &filename,
                            )
                            .await
                            {
                                Ok(path) => attachment_lines.push(
                                    crate::media::format_attachment_ref(&mt, &filename, &path),
                                ),
                                Err(e) => {
                                    warn!("Google Chat: failed to save attachment {filename}: {e}");
                                    attachment_lines
                                        .push(format!("[Attached file: {filename} (save failed)]"));
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Google Chat: failed to download attachment {filename}: {e}");
                            attachment_lines
                                .push(format!("[Attached file: {filename} (download failed)]"));
                        }
                    }
                }
                GchatAttachmentRef::Drive { file_id } => {
                    // Explicit degrade: downloading Drive content requires a
                    // Drive API scope the Chat service account is not granted.
                    warn!(
                        "Google Chat: Drive attachment {filename} ({file_id}) skipped — \
                         service account lacks Drive scope"
                    );
                    attachment_lines.push(format!(
                        "[Attached Google Drive file: {filename} — not downloaded (Drive scope not granted)]"
                    ));
                }
            }
        }
    }

    // Combine text + attachment references (same convention as telegram/discord).
    let input_text = if attachment_lines.is_empty() {
        text.clone()
    } else if text.is_empty() {
        attachment_lines.join("\n")
    } else {
        format!("{text}\n\n{}", attachment_lines.join("\n"))
    };
    if input_text.trim().is_empty() {
        return;
    }

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
        // Step / ModelInfo events are dashboard-only signals — never rendered
        // as channel text (would be an empty message).
        if matches!(
            event,
            crate::channel_reply::ProgressEvent::Step { .. }
                | crate::channel_reply::ProgressEvent::ModelInfo { .. }
        ) {
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

    let reply =
        build_reply_with_session(&input_text, &state.ctx, &session_id, &sender_id, Some(on_progress))
            .await;

    // WP1.3: 📎DELIVER: — Google Chat attachment upload is not wired, so the
    // sender's default `send_document` degrades to a text notice (→ dashboard
    // Files panel); the marker is stripped from the reply.
    let reply = {
        let doc_sender = crate::channel_sender::create_googlechat_sender(
            state.ctx.home_dir.clone(),
            space.clone(),
            sender_id.clone(),
        );
        crate::channel_reply::deliver_documents_for_reply(
            state.ctx.as_ref(), None, reply, doc_sender.as_ref(),
        ).await
    };

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

// ── Inbound attachments ──────────────────────────────────────────────────────

/// How an inbound Chat attachment's bytes can be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GchatAttachmentRef {
    /// Uploaded directly to Chat — downloadable via
    /// `GET /v1/media/{resourceName}?alt=media` with the bot's own `chat.bot`
    /// scope token.
    Uploaded { resource_name: String },
    /// A linked Google Drive file — downloading needs a Drive API scope the
    /// Chat service account is not granted, so callers degrade explicitly.
    Drive { file_id: String },
}

/// One parsed attachment from a Chat `MESSAGE` event.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GchatAttachment {
    /// `contentName` as sent by Chat; empty when absent.
    filename: String,
    content_type: String,
    reference: GchatAttachmentRef,
}

impl GchatAttachment {
    /// Filename to save under: `contentName`, or a MIME-derived fallback for
    /// nameless uploads so the extension-based skill router still keys on it.
    fn effective_filename(&self) -> String {
        let name = self.filename.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        format!("file.{}", crate::media::extension_from_mime(&self.content_type))
    }
}

/// Extract attachments from a Chat message JSON. The REST field is
/// `attachment[]` (singular key, array value); `attachments` is accepted
/// defensively. Entries with neither an `attachmentDataRef` nor a
/// `driveDataRef` are unactionable and skipped.
fn parse_gchat_attachments(message: &serde_json::Value) -> Vec<GchatAttachment> {
    let Some(arr) = message
        .get("attachment")
        .or_else(|| message.get("attachments"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|att| {
            let filename = att
                .get("contentName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content_type = att
                .get("contentType")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("application/octet-stream")
                .to_string();
            let uploaded = att
                .pointer("/attachmentDataRef/resourceName")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let drive = att
                .pointer("/driveDataRef/driveFileId")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let reference = if let Some(rn) = uploaded {
                GchatAttachmentRef::Uploaded { resource_name: rn.to_string() }
            } else if let Some(id) = drive {
                GchatAttachmentRef::Drive { file_id: id.to_string() }
            } else {
                return None;
            };
            Some(GchatAttachment { filename, content_type, reference })
        })
        .collect()
}

/// Build the media-download URL for an uploaded attachment's `resourceName`.
/// The resource name is a slash-separated path segment
/// (`spaces/…/messages/…/attachments/…`) used verbatim in the URL path.
fn media_download_url(resource_name: &str) -> String {
    format!(
        "{CHAT_API}/media/{}?alt=media",
        resource_name.trim_start_matches('/')
    )
}

/// Download an uploaded attachment's bytes via the Chat media API using the
/// service-account token. Size-capped at `media::MAX_FILE_SIZE`; the URL goes
/// through the shared SSRF-validated downloader.
async fn download_media(
    creds: &GoogleChatCreds,
    resource_name: &str,
) -> Result<Vec<u8>, String> {
    let token = creds.get_token().await?;
    let url = media_download_url(resource_name);
    let bearer = format!("Bearer {token}");
    crate::media::download_url(
        &creds.http,
        &url,
        Some(("Authorization", &bearer)),
        crate::media::MAX_FILE_SIZE as usize,
    )
    .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_uploaded_attachment_ref() {
        let message = json!({
            "text": "here you go",
            "attachment": [{
                "name": "spaces/AAA/messages/BBB/attachments/CCC",
                "contentName": "report.pdf",
                "contentType": "application/pdf",
                "source": "UPLOADED_CONTENT",
                "attachmentDataRef": { "resourceName": "spaces/AAA/messages/BBB/attachments/CCC" }
            }]
        });
        let atts = parse_gchat_attachments(&message);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].filename, "report.pdf");
        assert_eq!(atts[0].content_type, "application/pdf");
        assert_eq!(
            atts[0].reference,
            GchatAttachmentRef::Uploaded {
                resource_name: "spaces/AAA/messages/BBB/attachments/CCC".into()
            }
        );
    }

    #[test]
    fn parses_drive_attachment_ref() {
        let message = json!({
            "attachment": [{
                "contentName": "budget.xlsx",
                "source": "DRIVE_FILE",
                "driveDataRef": { "driveFileId": "drive-id-123" }
            }]
        });
        let atts = parse_gchat_attachments(&message);
        assert_eq!(atts.len(), 1);
        // No contentType in the event → octet-stream default.
        assert_eq!(atts[0].content_type, "application/octet-stream");
        assert_eq!(
            atts[0].reference,
            GchatAttachmentRef::Drive { file_id: "drive-id-123".into() }
        );
    }

    #[test]
    fn skips_unactionable_and_accepts_plural_key() {
        // Neither attachmentDataRef nor driveDataRef → skipped; the plural
        // "attachments" key is accepted defensively.
        let message = json!({
            "attachments": [
                { "contentName": "ghost.bin" },
                {
                    "contentName": "ok.png",
                    "contentType": "image/png",
                    "attachmentDataRef": { "resourceName": "spaces/S/messages/M/attachments/A" }
                }
            ]
        });
        let atts = parse_gchat_attachments(&message);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].filename, "ok.png");
        // No attachment field at all → empty, no panic.
        assert!(parse_gchat_attachments(&json!({ "text": "hi" })).is_empty());
        // Empty resourceName → unactionable.
        let empty_ref = json!({
            "attachment": [{
                "contentName": "x.pdf",
                "attachmentDataRef": { "resourceName": "  " }
            }]
        });
        assert!(parse_gchat_attachments(&empty_ref).is_empty());
    }

    #[test]
    fn media_download_url_is_wellformed() {
        assert_eq!(
            media_download_url("spaces/A/messages/B/attachments/C"),
            "https://chat.googleapis.com/v1/media/spaces/A/messages/B/attachments/C?alt=media"
        );
        // A leading slash must not produce a double slash.
        assert_eq!(
            media_download_url("/spaces/A/messages/B/attachments/C"),
            "https://chat.googleapis.com/v1/media/spaces/A/messages/B/attachments/C?alt=media"
        );
    }

    #[test]
    fn effective_filename_falls_back_to_mime_extension() {
        let named = GchatAttachment {
            filename: "photo.jpg".into(),
            content_type: "image/jpeg".into(),
            reference: GchatAttachmentRef::Uploaded { resource_name: "r".into() },
        };
        assert_eq!(named.effective_filename(), "photo.jpg");
        let nameless = GchatAttachment {
            filename: "".into(),
            content_type: "image/png".into(),
            reference: GchatAttachmentRef::Uploaded { resource_name: "r".into() },
        };
        assert_eq!(nameless.effective_filename(), "file.png");
        let unknown = GchatAttachment {
            filename: "  ".into(),
            content_type: "application/x-mystery".into(),
            reference: GchatAttachmentRef::Drive { file_id: "d".into() },
        };
        assert_eq!(unknown.effective_filename(), "file.bin");
    }
}
