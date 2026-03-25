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

use crate::channel_reply::{ChannelStatusMap, ReplyContext, build_reply, set_channel_connected};

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
    #[serde(rename = "userId")]
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineMessage {
    #[serde(rename = "type")]
    msg_type: String,
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct LineReplyBody {
    #[serde(rename = "replyToken")]
    reply_token: String,
    messages: Vec<LineReplyMessage>,
}

#[derive(Debug, Serialize)]
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
            set_channel_connected(&channel_status, "line", true, None).await;
        }
        Ok(resp) => {
            let msg = format!("token invalid (HTTP {})", resp.status());
            warn!("LINE bot {msg}");
            set_channel_connected(&channel_status, "line", false, Some(msg)).await;
            return None;
        }
        Err(e) => {
            warn!("LINE connection failed: {e}");
            set_channel_connected(&channel_status, "line", false, Some(e.to_string())).await;
            return None;
        }
    }

    let state = LineState { token, secret, ctx, http, channel_status };

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
    set_channel_connected(&state.channel_status, "line", true, None).await;

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
            let sender = event
                .source
                .as_ref()
                .and_then(|s| s.user_id.as_deref())
                .unwrap_or("unknown");

            info!("📩 LINE [{sender}]: {}", &text[..text.len().min(80)]);

            let reply = build_reply(text, &state.ctx).await;
            send_reply(&state.http, &state.token, reply_token, &reply).await;
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

async fn send_reply(http: &reqwest::Client, token: &str, reply_token: &str, text: &str) {
    let body = LineReplyBody {
        reply_token: reply_token.to_string(),
        messages: vec![LineReplyMessage {
            msg_type: "text".to_string(),
            text: text.to_string(),
        }],
    };

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

async fn read_line_config(home_dir: &Path) -> Option<(String, String)> {
    let token = crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "line_channel_token").await?;
    let secret = crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "line_channel_secret")
        .await
        .unwrap_or_default();
    Some((token, secret))
}
