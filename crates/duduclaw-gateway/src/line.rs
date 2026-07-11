//! LINE Messaging API integration with webhook receiver.
//!
//! Mounts a `/webhook/line` POST endpoint on the Axum router to receive
//! messages from LINE, validates signatures, and sends replies.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_core::truncate_bytes;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{error, info, warn};

use crate::channel_format;
use crate::channel_reply::{ChannelStatusMap, ReplyContext, build_reply_with_session, set_channel_connected};
use crate::channel_settings::keys;

const LINE_API: &str = "https://api.line.me/v2/bot";

type HmacSha256 = Hmac<Sha256>;

// ── LINE API types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LineWebhookBody {
    events: Vec<LineEvent>,
}

#[derive(Debug, Deserialize)]
struct LineEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(rename = "replyToken")]
    reply_token: Option<String>,
    source: Option<LineSource>,
    message: Option<LineMessage>,
    /// Present on `postback` events (quick-reply button presses).
    postback: Option<LinePostback>,
}

#[derive(Debug, Deserialize)]
struct LinePostback {
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineSource {
    #[serde(rename = "type")]
    source_type: Option<String>,
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
    #[serde(rename = "roomId")]
    room_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineMessage {
    /// Message ID, used to download content for image/video/audio/file.
    id: Option<String>,
    #[serde(rename = "type")]
    msg_type: String,
    text: Option<String>,
    /// Original filename (for file type messages).
    #[serde(rename = "fileName")]
    file_name: Option<String>,
    /// File size in bytes (for file type messages).
    #[serde(rename = "fileSize")]
    #[allow(dead_code)]
    file_size: Option<u64>,
    /// Content provider info (for image/video/audio).
    #[serde(rename = "contentProvider")]
    content_provider: Option<LineContentProvider>,
}

#[derive(Debug, Deserialize)]
struct LineContentProvider {
    /// "line" for LINE-hosted content, "external" for external URLs.
    #[serde(rename = "type")]
    provider_type: String,
    /// URL when provider_type is "external".
    #[serde(rename = "originalContentUrl")]
    original_content_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct LineReplyBody {
    #[serde(rename = "replyToken")]
    reply_token: String,
    messages: Vec<LineReplyMessage>,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct LineReplyMessage {
    #[serde(rename = "type")]
    msg_type: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LineBotInfo {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

// ── Shared state ────────────────────────────────────────────

#[derive(Clone)]
pub struct LineState {
    // The token/secret are NOT baked in — they're read from config on every
    // request so a dashboard config change takes effect without a gateway
    // restart (the `/webhook/line` route is always mounted).
    home_dir: PathBuf,
    ctx: Arc<ReplyContext>,
    http: reqwest::Client,
    channel_status: ChannelStatusMap,
    event_tx: tokio::sync::broadcast::Sender<String>,
}

// ── Public API ──────────────────────────────────────────────

/// Mount the LINE webhook endpoint. The route is ALWAYS mounted (even when LINE
/// is not yet configured) and the handler reads the token/secret from config on
/// every request — so configuring or changing LINE in the dashboard takes effect
/// immediately, with no gateway restart. A best-effort token check runs now to
/// set the initial channel status.
pub async fn start_line_bot(home_dir: &Path, ctx: Arc<ReplyContext>) -> Router {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    // Initial status (best-effort) so the dashboard reflects an already-configured
    // LINE channel without waiting for the first webhook.
    verify_line_status(home_dir, &http, &ctx.channel_status, &ctx.event_tx).await;

    let state = LineState {
        home_dir: home_dir.to_path_buf(),
        channel_status: ctx.channel_status.clone(),
        event_tx: ctx.event_tx.clone(),
        http,
        ctx,
    };

    info!("   LINE webhook endpoint mounted: /webhook/line (token read per request — no restart needed on config change)");
    Router::new()
        .route("/webhook/line", post(line_webhook_handler))
        .with_state(state)
}

/// Re-check the configured LINE token and update the channel status. Called on
/// startup and whenever the dashboard saves LINE config (hot reload), so the
/// "connected" indicator updates live instead of staying on "連線中".
pub async fn refresh_line_status(home_dir: &Path, ctx: Arc<ReplyContext>) {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    verify_line_status(home_dir, &http, &ctx.channel_status, &ctx.event_tx).await;
}

/// Read the current LINE config and ping the LINE API to set the channel status.
/// Marks disconnected (with a reason) when not configured / token invalid.
async fn verify_line_status(
    home_dir: &Path,
    http: &reqwest::Client,
    channel_status: &ChannelStatusMap,
    event_tx: &tokio::sync::broadcast::Sender<String>,
) {
    let (token, secret) = match read_line_config(home_dir).await {
        Some(pair) => pair,
        None => {
            set_channel_connected(channel_status, "line", false, Some("not configured".into()), Some(event_tx)).await;
            return;
        }
    };
    if token.is_empty() {
        set_channel_connected(channel_status, "line", false, Some("not configured".into()), Some(event_tx)).await;
        return;
    }
    // HS2: an empty secret would make signature verification accept forged
    // requests; surface it as not-connected so the operator fixes it.
    if secret.is_empty() {
        set_channel_connected(channel_status, "line", false, Some("channel secret missing".into()), Some(event_tx)).await;
        return;
    }
    match http
        .get(format!("{LINE_API}/info"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(info) = resp.json::<LineBotInfo>().await {
                let name = info.display_name.as_deref().unwrap_or("unknown");
                info!("💬 LINE bot connected: {name}");
            } else {
                info!("💬 LINE bot token verified");
            }
            set_channel_connected(channel_status, "line", true, None, Some(event_tx)).await;
        }
        Ok(resp) => {
            let msg = format!("token invalid (HTTP {})", resp.status());
            warn!("LINE bot {msg}");
            set_channel_connected(channel_status, "line", false, Some(msg), Some(event_tx)).await;
        }
        Err(e) => {
            warn!("LINE connection check failed: {e}");
            set_channel_connected(channel_status, "line", false, Some(e.to_string()), Some(event_tx)).await;
        }
    }
}

// ── Webhook handler ─────────────────────────────────────────

async fn line_webhook_handler(
    State(state): State<LineState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    // Load the current LINE credentials per request — config changes apply live.
    let (token, secret) = match read_line_config(&state.home_dir).await {
        Some((t, s)) if !t.is_empty() && !s.is_empty() => (t, s),
        _ => {
            // Not configured (or missing secret). Accept so LINE's "Verify" still
            // gets a 200, but process nothing — fail closed (no secret ⇒ can't and
            // won't validate/handle events).
            return StatusCode::OK;
        }
    };

    // Validate signature
    let signature = match headers.get("x-line-signature").and_then(|v| v.to_str().ok()) {
        Some(sig) => sig.to_string(),
        None => {
            warn!("LINE webhook: missing X-Line-Signature");
            return StatusCode::BAD_REQUEST;
        }
    };

    if !verify_signature(&secret, &body, &signature) {
        warn!("LINE webhook: invalid signature");
        return StatusCode::UNAUTHORIZED;
    }

    // Parse body
    let webhook: LineWebhookBody = match serde_json::from_slice(&body) {
        Ok(w) => w,
        Err(e) => {
            warn!("LINE webhook: parse error: {e}");
            return StatusCode::BAD_REQUEST;
        }
    };

    // Update last_event timestamp on each webhook call
    set_channel_connected(&state.channel_status, "line", true, None, Some(&state.event_tx)).await;

    // Process events in a DETACHED task so the webhook returns 200 immediately.
    // LINE times out a slow webhook response and the reply_token is short-lived;
    // blocking the 200 on a multi-second model reply gets the handler future (and
    // the in-flight reply) cancelled when LINE disconnects → "已讀沒回應".
    tokio::spawn(async move {
    for event in webhook.events {
        // ── Quick-reply button presses (postback events) ──
        if event.event_type == "postback" {
            handle_postback(&event, &state, &token).await;
            continue;
        }

        if event.event_type != "message" {
            continue;
        }

        let Some(msg) = &event.message else { continue };
        let Some(reply_token) = &event.reply_token else { continue };

        // Skip unsupported message types (e.g., location, sticker)
        let supported_types = ["text", "image", "video", "audio", "file"];
        if !supported_types.contains(&msg.msg_type.as_str()) {
            continue;
        }

        {
            let source = &event.source;
            let source_type = source.as_ref().and_then(|s| s.source_type.as_deref()).unwrap_or("user");
            let is_group = source_type == "group" || source_type == "room";
            let scope_id = source.as_ref()
                .and_then(|s| s.group_id.as_deref().or(s.room_id.as_deref()))
                .unwrap_or("global");

            // ── Channel whitelist (group chats only) ──
            if is_group && !state.ctx.channel_settings.is_channel_allowed("line", "global", scope_id).await {
                continue;
            }

            // ── Mention-only mode (group chats only, LINE has no native @mention) ──
            let mention_only = state.ctx.channel_settings.get_bool("line", scope_id, keys::MENTION_ONLY, false).await;
            if is_group && mention_only {
                continue;
            }

            let sender = source.as_ref()
                .and_then(|s| s.user_id.as_deref())
                .unwrap_or("unknown");

            // ── Build input text + attachment references ──
            let mut attachment_lines: Vec<String> = Vec::new();
            let mut base_text = msg.text.as_deref().unwrap_or("").to_string();
            // Voice-to-text: an `audio` message is transcribed and folded into the
            // input text (mirrors Telegram). Additive — the saved attachment
            // reference is still emitted, so a failed/keyless transcription
            // degrades gracefully rather than dropping the message.
            let mut voice_text = String::new();

            // Handle non-text message types: download content and save to disk
            if msg.msg_type != "text" {
                if let Some(msg_id) = &msg.id {
                    let type_label = &msg.msg_type;
                    info!("📩 LINE [{sender}]: {type_label} message");

                    // Determine content URL: LINE-hosted or external
                    let content_data = if let Some(cp) = &msg.content_provider
                        && cp.provider_type == "external"
                        && let Some(url) = &cp.original_content_url
                    {
                        // External URL — download directly
                        crate::media::download_url(&state.http, url, None, crate::media::MAX_FILE_SIZE as usize).await.ok()
                    } else {
                        // LINE-hosted — download via Content API
                        download_line_content(&state.http, &token, msg_id).await.ok()
                    };

                    if let Some(data) = content_data {
                        // Transcribe voice/audio messages to text.
                        if msg.msg_type == "audio" {
                            match duduclaw_inference::whisper::transcribe(
                                &data,
                                Some("zh"),
                                &duduclaw_inference::whisper::WhisperMode::Api,
                            )
                            .await
                            {
                                Ok(t) if !t.trim().is_empty() => {
                                    info!(
                                        "🎙 LINE [{sender}] transcribed: {}",
                                        duduclaw_core::truncate_bytes(&t, 80)
                                    );
                                    voice_text = t;
                                }
                                Ok(_) => {}
                                Err(e) => warn!("LINE voice transcription failed: {e}"),
                            }
                        }
                        let mime = crate::media::detect_mime(&data);
                        let mt = crate::media::media_type_from_mime(&mime);
                        let fname = if let Some(name) = &msg.file_name {
                            name.clone()
                        } else {
                            let ext = crate::media::extension_from_mime(&mime);
                            format!("{type_label}.{ext}")
                        };
                        match crate::media::save_attachment_to_disk(&state.ctx.home_dir, &data, &fname).await {
                            Ok(path) => {
                                attachment_lines.push(crate::media::format_attachment_ref(&mt, &fname, &path));
                            }
                            Err(e) => warn!("Failed to save LINE {type_label}: {e}"),
                        }
                    }
                }
            }

            // Fold any transcription into the base text.
            if !voice_text.is_empty() {
                base_text = if base_text.trim().is_empty() {
                    voice_text
                } else {
                    format!("{base_text}\n{voice_text}")
                };
            }

            // Combine text + attachment references
            let input_text = if attachment_lines.is_empty() {
                base_text.clone()
            } else if base_text.trim().is_empty() {
                attachment_lines.join("\n")
            } else {
                format!("{base_text}\n\n{}", attachment_lines.join("\n"))
            };

            if input_text.trim().is_empty() {
                continue;
            }

            info!("📩 LINE [{sender}]: {}", truncate_bytes(&input_text, 80));

            // Progress callback via Push API (requires userId).
            // LINE Push API has monthly message quotas — debounce at 60s
            // (more conservative than Telegram's 30s).
            let user_id_for_push = event.source
                .as_ref()
                .and_then(|s| s.user_id.clone());
            let on_progress: Option<crate::channel_reply::ProgressCallback> = if let Some(uid) = user_id_for_push {
                let push_http = state.http.clone();
                let push_token = token.clone();
                let last_progress = Arc::new(std::sync::Mutex::new(std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(120))
                    .unwrap_or_else(std::time::Instant::now)));
                Some(Box::new(move |event: crate::channel_reply::ProgressEvent| {
                    let mut last = match last_progress.lock() {
                        Ok(g) => g,
                        Err(e) => e.into_inner(),
                    };
                    if last.elapsed().as_secs() < 60 {
                        return;
                    }
                    *last = std::time::Instant::now();
                    drop(last);

                    let msg_text = event.to_display();
                    let c = push_http.clone();
                    let t = push_token.clone();
                    let u = uid.clone();
                    tokio::spawn(async move {
                        push_message(&c, &t, &u, &msg_text).await;
                    });
                }))
            } else {
                None
            };

            // Build session ID scoped to group/room or user DM
            let session_id = if let Some(gid) = source.as_ref().and_then(|s| s.group_id.as_deref()) {
                format!("line:{gid}")
            } else if let Some(rid) = source.as_ref().and_then(|s| s.room_id.as_deref()) {
                format!("line:{rid}")
            } else {
                format!("line:{sender}")
            };

            // Loading animation (LINE shows it in 1:1 chats only; the API
            // silently no-ops elsewhere). RAII guard stops the refresh loop.
            let loading_guard = event
                .source
                .as_ref()
                .and_then(|s| s.user_id.clone())
                .map(|uid| crate::channel_typing::line_loading(state.http.clone(), token.clone(), uid));

            let reply = build_reply_with_session(&input_text, &state.ctx, &session_id, sender, on_progress).await;
            drop(loading_guard);

            // Guard: don't send empty replies
            if reply.trim().is_empty() {
                warn!("LINE: reply is empty — skipping send for {sender}");
                continue;
            }

            // Use Flex Message for long replies, plain text for short ones
            let agent_name = {
                let reg = state.ctx.registry.read().await;
                reg.main_agent().map(|a| a.config.agent.display_name.clone())
            };
            // M25: segment long replies. A single Flex bubble has tight text
            // limits, so an over-limit reply would be rejected and silently
            // dropped. `segment_line_reply` returns one Flex bubble for short
            // replies, or several plain-text messages (each within LINE's
            // 5000-char text limit, capped at 5 messages/request) for long ones.
            let mut messages = segment_line_reply(&reply, agent_name.as_deref());

            // Attach quick-reply buttons to the LAST message (LINE only shows
            // quickReply on the most recent message). Presses arrive as
            // `postback` events handled above.
            if let Some(last) = messages.last_mut() {
                last["quickReply"] = channel_format::line_quick_reply();
            }

            // Try Reply API first; if it fails (e.g. reply token expired after
            // long AI processing), fall back to Push API which doesn't require
            // a reply token but counts against the monthly message quota.
            if !send_reply_rich(&state.http, &token, reply_token, messages.clone()).await {
                warn!("LINE: reply API failed — falling back to push API for {sender}");
                push_message_rich(&state.http, &token, sender, messages).await;
            }
        }
    }
    });

    StatusCode::OK
}

// ── Helpers ─────────────────────────────────────────────────

/// Handle a `postback` event (quick-reply button press).
/// `data` format mirrors the Discord custom_id convention: `duduclaw:{action}`.
async fn handle_postback(event: &LineEvent, state: &LineState, token: &str) {
    let data = event
        .postback
        .as_ref()
        .and_then(|p| p.data.as_deref())
        .unwrap_or("");
    let Some(reply_token) = &event.reply_token else { return };
    let source = &event.source;
    let sender = source
        .as_ref()
        .and_then(|s| s.user_id.as_deref())
        .unwrap_or("unknown");

    info!("🔘 LINE [{sender}] postback: {data}");

    let answer = match data {
        "duduclaw:new_session" => {
            // Session id scoped the same way as the message path.
            let session_id = if let Some(gid) = source.as_ref().and_then(|s| s.group_id.as_deref()) {
                format!("line:{gid}")
            } else if let Some(rid) = source.as_ref().and_then(|s| s.room_id.as_deref()) {
                format!("line:{rid}")
            } else {
                format!("line:{sender}")
            };
            match state.ctx.session_manager.delete_session(&session_id).await {
                Ok(()) => "✅ 已開啟新的對話".to_string(),
                Err(e) => format!("⚠️ 清除工作階段失敗：{e}"),
            }
        }
        _ => "未知的按鈕動作".to_string(),
    };

    let messages = vec![serde_json::json!({ "type": "text", "text": answer })];
    if !send_reply_rich(&state.http, token, reply_token, messages.clone()).await {
        push_message_rich(&state.http, token, sender, messages).await;
    }
}

/// LINE limits for outbound message segmentation.
mod line_limits {
    /// Max messages per reply/push request.
    pub const MAX_MESSAGES: usize = 5;
    /// LINE text-message hard limit is 5000 chars; stay under it for safety.
    pub const TEXT_CHUNK: usize = 4500;
    /// Replies at or under this fit comfortably in a single Flex bubble.
    pub const FLEX_SAFE: usize = 1800;
}

/// Build the LINE message array for a reply, segmenting long content.
///
/// M25: short/medium replies render as a single Flex bubble (existing
/// behaviour). Long replies are split into multiple plain-text messages, each
/// within LINE's 5000-char text limit and the 5-messages-per-request cap, so an
/// over-limit reply is delivered across messages instead of being rejected.
fn segment_line_reply(reply: &str, agent_name: Option<&str>) -> Vec<serde_json::Value> {
    // Small enough for one bubble → keep the rich Flex format
    // (to_line_flex_message does the markdown → plain conversion itself).
    if reply.chars().count() <= line_limits::FLEX_SAFE {
        return vec![channel_format::to_line_flex_message(reply, agent_name)];
    }

    // Long reply → markdown to LINE-friendly plain text, then split into
    // text messages on char-safe boundaries.
    let reply = &crate::markdown_render::to_line_plain(reply);
    let chunks = channel_format::split_text(reply, line_limits::TEXT_CHUNK);

    let mut messages: Vec<serde_json::Value> = Vec::new();
    // Reserve the last slot for a truncation notice if we overflow the cap.
    let limit = line_limits::MAX_MESSAGES;
    for chunk in chunks.iter() {
        if messages.len() >= limit {
            break;
        }
        messages.push(serde_json::json!({ "type": "text", "text": chunk }));
    }

    // If content didn't fit in the message cap, replace the last message with a
    // notice so the user knows the reply was truncated rather than silently cut.
    if chunks.len() > limit {
        if let Some(last) = messages.last_mut() {
            *last = serde_json::json!({
                "type": "text",
                "text": "⚠️ 回覆過長，已截斷。請縮小問題範圍或分次提問。"
            });
        }
    }

    if messages.is_empty() {
        messages.push(channel_format::to_line_flex_message(reply, agent_name));
    }
    messages
}

fn verify_signature(secret: &str, body: &[u8], signature: &str) -> bool {
    use base64::Engine;

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let expected = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    // Use constant-time comparison to prevent timing attacks (BE-M8)
    constant_time_eq(expected.as_bytes(), signature.as_bytes())
}

/// Constant-time byte-slice equality check.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Download message content (image/video/audio/file) from LINE Content API.
async fn download_line_content(
    http: &reqwest::Client,
    token: &str,
    message_id: &str,
) -> Result<Vec<u8>, String> {
    let url = format!("https://api-data.line.me/v2/bot/message/{message_id}/content");
    crate::media::download_url(
        http,
        &url,
        Some(("Authorization", &format!("Bearer {token}"))),
        crate::media::MAX_FILE_SIZE as usize,
    ).await
}

/// Send a rich reply (Flex Message, etc.) via the LINE Reply API.
///
/// Returns `true` on success, `false` on failure (e.g. reply token expired).
async fn send_reply_rich(
    http: &reqwest::Client,
    token: &str,
    reply_token: &str,
    messages: Vec<serde_json::Value>,
) -> bool {
    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": messages
    });

    match http
        .post(format!("{LINE_API}/message/reply"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            error!("LINE reply failed ({status}): {}", truncate_bytes(&text, 200));
            false
        }
        Err(e) => {
            error!("LINE reply error: {e}");
            false
        }
        _ => true,
    }
}

/// Send a rich push message (Flex Message) to a specific LINE user.
///
/// Used as fallback when the Reply API fails (e.g. reply token expired after
/// long AI processing). Counts against the monthly message quota.
async fn push_message_rich(http: &reqwest::Client, token: &str, user_id: &str, messages: Vec<serde_json::Value>) {
    let body = serde_json::json!({
        "to": user_id,
        "messages": messages
    });

    match http
        .post(format!("{LINE_API}/message/push"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("LINE push (rich) failed ({status}): {}", truncate_bytes(&body, 200));
        }
        Err(e) => error!("LINE push (rich) error: {e}"),
        _ => info!("LINE: push fallback succeeded for {user_id}"),
    }
}

/// Send a push message to a specific LINE user (for progress updates).
///
/// Uses the LINE Push API which counts against the monthly message quota.
async fn push_message(http: &reqwest::Client, token: &str, user_id: &str, text: &str) {
    let body = serde_json::json!({
        "to": user_id,
        "messages": [{ "type": "text", "text": text }]
    });

    match http
        .post(format!("{LINE_API}/message/push"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("LINE push failed ({status}): {}", truncate_bytes(&body, 200));
        }
        Err(e) => warn!("LINE push error: {e}"),
        _ => {}
    }
}

async fn read_line_config(home_dir: &Path) -> Option<(String, String)> {
    let token = crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "line_channel_token").await?;
    let secret = crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "line_channel_secret")
        .await
        .unwrap_or_default();
    Some((token, secret))
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_line_reply_short_is_single_message() {
        // M25: short replies stay as one (Flex or text) message.
        let msgs = segment_line_reply("你好", Some("agent"));
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn test_segment_line_reply_long_splits_into_text() {
        // A reply well over the Flex-safe size must become multiple text msgs.
        let long = "字".repeat(line_limits::FLEX_SAFE + 6000);
        let msgs = segment_line_reply(&long, None);
        assert!(msgs.len() > 1, "long reply should be segmented");
        assert!(msgs.len() <= line_limits::MAX_MESSAGES, "must respect 5-message cap");
        for m in &msgs {
            assert_eq!(m["type"], "text");
            // Each text message stays within LINE's 5000-char limit.
            let chars = m["text"].as_str().unwrap().chars().count();
            assert!(chars <= 5000, "text message exceeds LINE limit: {chars}");
        }
    }

    #[test]
    fn test_segment_line_reply_cjk_no_panic() {
        // M25 robustness: pure-CJK long input must not panic during splitting.
        let cjk = "繁體中文測試訊息".repeat(2000);
        let msgs = segment_line_reply(&cjk, None);
        assert!(!msgs.is_empty());
    }

    #[test]
    fn test_parse_postback_event() {
        let json = r#"{
            "events": [{
                "type": "postback",
                "replyToken": "rt-1",
                "source": { "type": "user", "userId": "U123" },
                "postback": { "data": "duduclaw:new_session" }
            }]
        }"#;
        let body: LineWebhookBody = serde_json::from_str(json).unwrap();
        let event = &body.events[0];
        assert_eq!(event.event_type, "postback");
        assert_eq!(
            event.postback.as_ref().and_then(|p| p.data.as_deref()),
            Some("duduclaw:new_session")
        );
    }

    #[test]
    fn test_message_event_without_postback_still_parses() {
        let json = r#"{
            "events": [{
                "type": "message",
                "replyToken": "rt-2",
                "source": { "type": "user", "userId": "U123" },
                "message": { "id": "m1", "type": "text", "text": "hi" }
            }]
        }"#;
        let body: LineWebhookBody = serde_json::from_str(json).unwrap();
        assert!(body.events[0].postback.is_none());
    }
}
