//! Feishu (Lark) Bot integration via Event Subscription webhook.
//!
//! Uses the Feishu Open Platform API v2 for message sending and
//! webhook events for message receiving.

use std::path::Path;
use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::post,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::channel_reply::{ReplyContext, build_reply_with_session, set_channel_connected};

const FEISHU_API: &str = "https://open.feishu.cn/open-apis";

// ── Feishu API types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FeishuEvent {
    #[serde(default)]
    challenge: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    header: Option<EventHeader>,
    #[serde(default)]
    event: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct EventHeader {
    event_type: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    code: i32,
    msg: String,
    tenant_access_token: Option<String>,
    expire: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SendMessageBody {
    receive_id: String,
    msg_type: String,
    content: String,
}

// ── Shared state ────────────────────────────────────────────────

struct FeishuState {
    ctx: Arc<ReplyContext>,
    app_id: String,
    app_secret: String,
    verification_token: String,
    /// Cached tenant access token (refreshed every 2 hours)
    token: RwLock<(String, std::time::Instant)>,
    http: reqwest::Client,
}

impl FeishuState {
    async fn get_token(&self) -> Result<String, String> {
        {
            let cached = self.token.read().await;
            // Token valid for 2 hours, refresh 5 minutes early
            if !cached.0.is_empty() && cached.1.elapsed().as_secs() < 6900 {
                return Ok(cached.0.clone());
            }
        }

        // Refresh token
        let resp: TokenResponse = self
            .http
            .post(format!("{FEISHU_API}/auth/v3/tenant_access_token/internal"))
            .json(&serde_json::json!({
                "app_id": self.app_id,
                "app_secret": self.app_secret,
            }))
            .send()
            .await
            .map_err(|e| format!("Token refresh failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Token parse failed: {e}"))?;

        if resp.code != 0 {
            return Err(format!("Feishu token error: {}", resp.msg));
        }

        let token = resp.tenant_access_token.ok_or("No token returned")?;
        *self.token.write().await = (token.clone(), std::time::Instant::now());
        info!("Feishu tenant_access_token refreshed (expires in {}s)", resp.expire.unwrap_or(7200));
        Ok(token)
    }
}

// ── Public API ──────────────────────────────────────────────────

/// Create the Feishu webhook router.
///
/// Returns `None` if Feishu is not configured.
pub async fn start_feishu_webhook(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<Router> {
    let app_id = read_feishu_config(home_dir, "feishu_app_id").await?;
    let app_secret = read_feishu_config(home_dir, "feishu_app_secret").await?;
    let verification_token = read_feishu_config(home_dir, "feishu_verification_token").await.unwrap_or_default();

    if app_id.is_empty() || app_secret.is_empty() {
        return None;
    }

    info!("📲 Feishu webhook starting (app: {app_id})");

    let state = Arc::new(FeishuState {
        ctx,
        app_id,
        app_secret,
        verification_token,
        token: RwLock::new((String::new(), std::time::Instant::now())),
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default(),
    });

    // Pre-fetch token
    match state.get_token().await {
        Ok(_) => {
            set_channel_connected(&state.ctx.channel_status, "feishu", true, None, Some(&state.ctx.event_tx)).await;
        }
        Err(e) => {
            warn!("Feishu token error: {e}");
            set_channel_connected(&state.ctx.channel_status, "feishu", false, Some(e), Some(&state.ctx.event_tx)).await;
        }
    }

    Some(
        Router::new()
            .route("/webhook/feishu", post(handle_webhook))
            .with_state(state),
    )
}

// ── Webhook handler ─────────────────────────────────────────────

async fn handle_webhook(
    State(state): State<Arc<FeishuState>>,
    body: Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let event: FeishuEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("Feishu webhook parse error: {e}");
            return (StatusCode::BAD_REQUEST, "Parse error").into_response();
        }
    };

    // 1. First verify token (if configured)
    if !state.verification_token.is_empty() {
        if let Some(token) = event.token.as_deref() {
            if !constant_time_eq(token.as_bytes(), state.verification_token.as_bytes()) {
                warn!("Feishu webhook token mismatch — possible spoofed request");
                return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
            }
        } else {
            return (StatusCode::UNAUTHORIZED, "missing token").into_response();
        }
    }

    // 2. Then handle challenge
    if let Some(challenge) = event.challenge {
        return (
            StatusCode::OK,
            serde_json::json!({ "challenge": challenge }).to_string(),
        )
            .into_response();
    }

    // Handle message event
    if let Some(header) = &event.header {
        if header.event_type == "im.message.receive_v1" {
            if let Some(event_data) = &event.event {
                handle_message(event_data, &state).await;
            }
        }
    }

    StatusCode::OK.into_response()
}

/// Remove Feishu-style `@_user_N` mentions from message text.
fn strip_feishu_mentions(text: &str) -> String {
    let mut result = text.to_string();
    while let Some(start) = result.find("@_user_") {
        if let Some(end) = result[start..].find(char::is_whitespace) {
            // Remove the mention and the trailing whitespace
            result = format!("{}{}", &result[..start], &result[start + end + 1..]);
        } else {
            result = result[..start].to_string();
        }
    }
    result.trim().to_string()
}

async fn handle_message(event: &serde_json::Value, state: &FeishuState) {
    let message = match event.get("message") {
        Some(m) => m,
        None => return,
    };

    let msg_type = message.get("message_type").and_then(|v| v.as_str()).unwrap_or("");
    if msg_type != "text" {
        return;
    }

    // Parse content JSON: {"text":"hello"}
    let content_str = message.get("content").and_then(|v| v.as_str()).unwrap_or("{}");
    let raw_text = serde_json::from_str::<serde_json::Value>(content_str)
        .ok()
        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let text = strip_feishu_mentions(&raw_text);

    if text.is_empty() {
        return;
    }

    let chat_id = message.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let msg_id = message.get("message_id").and_then(|v| v.as_str()).unwrap_or("");
    let sender = event
        .get("sender")
        .and_then(|s| s.get("sender_id"))
        .and_then(|s| s.get("open_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    info!("📩 Feishu [{sender}]: {}", &text[..text.len().min(80)]);

    // Chat commands
    if crate::chat_commands::is_command(&text) {
        if let Some(cmd) = crate::chat_commands::parse_command(&text, None) {
            let session_id = format!("feishu:{chat_id}");
            let agent_id = {
                let reg = state.ctx.registry.read().await;
                reg.main_agent()
                    .map(|a| a.config.agent.name.clone())
                    .unwrap_or_default()
            };
            let reply = crate::chat_commands::handle_command(&cmd, &state.ctx, &session_id, &agent_id, true).await;
            if !reply.trim().is_empty() {
                send_message(state, chat_id, &reply).await;
            }
            return;
        }
    }

    let session_id = format!("feishu:{chat_id}");
    let reply = build_reply_with_session(&text, &state.ctx, &session_id, sender, None).await;

    // Guard: don't send empty replies
    if reply.trim().is_empty() {
        warn!(chat_id, "Feishu: reply is empty — skipping send");
        return;
    }

    // Reply to the original message so the sender gets a threaded notification
    if !msg_id.is_empty() {
        reply_message(state, msg_id, &reply).await;
    } else {
        send_message(state, chat_id, &reply).await;
    }
}

async fn send_message(state: &FeishuState, chat_id: &str, text: &str) {
    let token = match state.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Feishu token error: {e}");
            return;
        }
    };

    let content = serde_json::json!({ "text": text }).to_string();
    let body = SendMessageBody {
        receive_id: chat_id.to_string(),
        msg_type: "text".to_string(),
        content,
    };

    match state
        .http
        .post(format!("{FEISHU_API}/im/v1/messages?receive_id_type=chat_id"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            error!("Feishu send failed ({status}): {}", &text[..text.len().min(200)]);
        }
        Err(e) => error!("Feishu send error: {e}"),
        _ => {}
    }
}

/// Reply to a specific message (threaded reply in Feishu).
async fn reply_message(state: &FeishuState, message_id: &str, text: &str) {
    let token = match state.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Feishu token error: {e}");
            return;
        }
    };

    let content = serde_json::json!({ "text": text }).to_string();
    let body = serde_json::json!({
        "msg_type": "text",
        "content": content,
    });

    match state
        .http
        .post(format!("{FEISHU_API}/im/v1/messages/{message_id}/reply"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            error!("Feishu reply failed ({status}): {}", &text[..text.len().min(200)]);
        }
        Err(e) => error!("Feishu reply error: {e}"),
        _ => {}
    }
}

async fn read_feishu_config(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_feishu_mentions() {
        assert_eq!(strip_feishu_mentions("@_user_1 hello"), "hello");
        assert_eq!(strip_feishu_mentions("hi @_user_23 there"), "hi there");
        assert_eq!(strip_feishu_mentions("no mention"), "no mention");
        assert_eq!(strip_feishu_mentions("@_user_1"), "");
    }

    #[test]
    fn test_parse_challenge() {
        let json = r#"{"challenge":"test_challenge_123"}"#;
        let event: FeishuEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.challenge.as_deref(), Some("test_challenge_123"));
    }

    #[test]
    fn test_parse_message_event() {
        let json = r#"{"header":{"event_type":"im.message.receive_v1"},"event":{"message":{"message_type":"text","content":"{\"text\":\"hello\"}","chat_id":"oc_abc123"},"sender":{"sender_id":{"open_id":"ou_xyz"}}}}"#;
        let event: FeishuEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.header.as_ref().unwrap().event_type, "im.message.receive_v1");
        let msg = event.event.as_ref().unwrap().get("message").unwrap();
        assert_eq!(msg.get("chat_id").unwrap().as_str().unwrap(), "oc_abc123");
    }
}
