//! LINE Messaging API integration with webhook receiver.
//!
//! Mounts a `/webhook/line` POST endpoint on the Axum router to receive
//! messages from LINE, validates signatures, and sends replies.

use std::path::Path;
use std::sync::Arc;

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
use crate::channel_reply::{ChannelStatusMap, ReplyContext, build_reply_with_progress, build_reply_with_session, set_channel_connected};
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
    #[serde(rename = "type")]
    msg_type: String,
    text: Option<String>,
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
    token: String,
    secret: String,
    ctx: Arc<ReplyContext>,
    http: reqwest::Client,
    channel_status: ChannelStatusMap,
    event_tx: tokio::sync::broadcast::Sender<String>,
}

// ── Public API ──────────────────────────────────────────────

/// Initialize LINE bot and return an Axum Router with the webhook endpoint.
///
/// Returns `None` if LINE is not configured.
pub async fn start_line_bot(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<Router> {
    let (token, secret) = read_line_config(home_dir).await?;
    if token.is_empty() {
        return None;
    }

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()?;

    let channel_status = ctx.channel_status.clone();
    let event_tx = ctx.event_tx.clone();

    // Verify token
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
            set_channel_connected(&channel_status, "line", true, None, Some(&event_tx)).await;
        }
        Ok(resp) => {
            let msg = format!("token invalid (HTTP {})", resp.status());
            warn!("LINE bot {msg}");
            set_channel_connected(&channel_status, "line", false, Some(msg), Some(&event_tx)).await;
            return None;
        }
        Err(e) => {
            warn!("LINE connection failed: {e}");
            set_channel_connected(&channel_status, "line", false, Some(e.to_string()), Some(&event_tx)).await;
            return None;
        }
    }

    let state = LineState { token, secret, ctx, http, channel_status, event_tx };

    let router = Router::new()
        .route("/webhook/line", post(line_webhook_handler))
        .with_state(state);

    info!("   LINE webhook endpoint: /webhook/line");
    info!("   ⚠ 請在 LINE Developer Console 設定 Webhook URL: https://your-domain:18789/webhook/line");
    Some(router)
}

// ── Webhook handler ─────────────────────────────────────────

async fn line_webhook_handler(
    State(state): State<LineState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    // Validate signature
    let signature = match headers.get("x-line-signature").and_then(|v| v.to_str().ok()) {
        Some(sig) => sig.to_string(),
        None => {
            warn!("LINE webhook: missing X-Line-Signature");
            return StatusCode::BAD_REQUEST;
        }
    };

    if !verify_signature(&state.secret, &body, &signature) {
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

    // Process events
    for event in webhook.events {
        if event.event_type != "message" {
            continue;
        }

        if let Some(msg) = &event.message
            && msg.msg_type == "text"
            && let Some(text) = &msg.text
            && let Some(reply_token) = &event.reply_token
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
                // LINE has no structured mention system; skip unless text starts with bot name
                // In practice, mention_only for LINE groups means the bot won't respond at all
                // unless explicitly mentioned by name in the text (future: configurable trigger word)
                continue;
            }

            let sender = source.as_ref()
                .and_then(|s| s.user_id.as_deref())
                .unwrap_or("unknown");

            info!("📩 LINE [{sender}]: {}", &text[..text.len().min(80)]);

            // Progress callback via Push API (requires userId).
            // LINE Push API has monthly message quotas — debounce at 60s
            // (more conservative than Telegram's 30s).
            let user_id_for_push = event.source
                .as_ref()
                .and_then(|s| s.user_id.clone());
            let on_progress: Option<crate::channel_reply::ProgressCallback> = if let Some(uid) = user_id_for_push {
                let push_http = state.http.clone();
                let push_token = state.token.clone();
                let last_progress = Arc::new(std::sync::Mutex::new(std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(120))
                    .unwrap_or_else(std::time::Instant::now)));
                Some(Box::new(move |event: crate::channel_reply::ProgressEvent| {
                    let mut last = last_progress.lock().unwrap();
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

            let reply = build_reply_with_session(text, &state.ctx, &session_id, sender, on_progress).await;

            // Use Flex Message for long replies, plain text for short ones
            let agent_name = {
                let reg = state.ctx.registry.read().await;
                reg.main_agent().map(|a| a.config.agent.display_name.clone())
            };
            let flex_msg = channel_format::to_line_flex_message(&reply, agent_name.as_deref());
            send_reply_rich(&state.http, &state.token, reply_token, flex_msg).await;
        }
    }

    StatusCode::OK
}

// ── Helpers ─────────────────────────────────────────────────

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

/// Send a rich reply (Flex Message, etc.) via the LINE Reply API.
async fn send_reply_rich(
    http: &reqwest::Client,
    token: &str,
    reply_token: &str,
    message: serde_json::Value,
) {
    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [message]
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
            error!("LINE reply failed ({status}): {}", &text[..text.len().min(200)]);
        }
        Err(e) => error!("LINE reply error: {e}"),
        _ => {}
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
            warn!("LINE push failed ({status}): {}", &body[..body.len().min(200)]);
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
