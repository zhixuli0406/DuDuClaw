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
    http::{HeaderMap, StatusCode},
    routing::post,
};
use duduclaw_core::truncate_bytes;
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

    // M3: fail-closed. Without a verification token the webhook would accept
    // unsigned, unauthenticated events from anyone who knows the URL. Refuse
    // to start rather than run an open relay.
    if verification_token.is_empty() {
        error!(
            "Feishu webhook NOT started: feishu_verification_token is unset. \
             Set it in the channel config to authenticate incoming events."
        );
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
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // M3: when Feishu signs the request (X-Lark-Signature present), verify the
    // signature against the raw body BEFORE parsing. This authenticates the
    // request cryptographically rather than relying only on the in-body token.
    if let Some(sig) = headers.get("X-Lark-Signature").and_then(|v| v.to_str().ok()) {
        let timestamp = headers
            .get("X-Lark-Request-Timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let nonce = headers
            .get("X-Lark-Request-Nonce")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !verify_feishu_signature(timestamp, nonce, &state.verification_token, &body, sig) {
            warn!("Feishu webhook signature mismatch — rejecting request");
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }
    }

    let event: FeishuEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("Feishu webhook parse error: {e}");
            return (StatusCode::BAD_REQUEST, "Parse error").into_response();
        }
    };

    // Verify the in-body verification token (always configured — startup
    // refuses to run without it). Constant-time compare to avoid timing leaks.
    if let Some(token) = event.token.as_deref() {
        if !constant_time_eq(token.as_bytes(), state.verification_token.as_bytes()) {
            warn!("Feishu webhook token mismatch — possible spoofed request");
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
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
        let tail = &result[start..];
        // M26: locate the trailing whitespace by char so we can advance past it
        // using its real UTF-8 byte length. The previous `end + 1` assumed a
        // 1-byte space and panicked on multi-byte whitespace like U+3000.
        if let Some((ws_off, ws_ch)) = tail.char_indices().find(|(_, c)| c.is_whitespace()) {
            // Resume after the whitespace char (char-boundary safe).
            let resume = start + ws_off + ws_ch.len_utf8();
            result = format!("{}{}", &result[..start], &result[resume..]);
        } else {
            result = result[..start].to_string();
        }
    }
    result.trim().to_string()
}

async fn handle_message(event: &serde_json::Value, state: &Arc<FeishuState>) {
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

    info!("📩 Feishu [{sender}]: {}", truncate_bytes(&text, 80));

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

    // Progress callback — Feishu has no typing API; forward tool-progress
    // and the TodoUpdate task board as text messages (throttled 45s;
    // TodoUpdate bypasses the throttle).
    let progress_chat = chat_id.to_string();
    let progress_state = state.clone();
    let last_progress = std::sync::Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .unwrap_or_else(std::time::Instant::now),
    ));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        let is_todo = matches!(event, crate::channel_reply::ProgressEvent::TodoUpdate { .. });
        {
            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
            if !is_todo && last.elapsed().as_secs() < 45 {
                return;
            }
            *last = std::time::Instant::now();
        }
        let msg_text = event.to_display();
        let st = progress_state.clone();
        let ch = progress_chat.clone();
        tokio::spawn(async move {
            send_message(&st, &ch, &msg_text).await;
        });
    });

    let session_id = format!("feishu:{chat_id}");
    let reply = build_reply_with_session(&text, &state.ctx, &session_id, sender, Some(on_progress)).await;

    // Guard: don't send empty replies
    if reply.trim().is_empty() {
        warn!(chat_id, "Feishu: reply is empty — skipping send");
        return;
    }

    // Rich reply: interactive Card 2.0 markdown (tables/code render
    // natively). Oversized or rejected cards fall back to plain text.
    // Reply to the original message so the sender gets a threaded
    // notification when possible.
    let sent_as_card = if reply.len() <= FEISHU_CARD_BYTE_CAP {
        let card = build_feishu_card(&reply).to_string();
        if !msg_id.is_empty() {
            send_feishu_payload(state, FeishuTarget::Reply(msg_id), "interactive", &card).await
        } else {
            send_feishu_payload(state, FeishuTarget::Chat(chat_id), "interactive", &card).await
        }
    } else {
        false
    };

    if !sent_as_card {
        if !msg_id.is_empty() {
            reply_message(state, msg_id, &reply).await;
        } else {
            send_message(state, chat_id, &reply).await;
        }
    }
}

/// Where to deliver a Feishu message.
enum FeishuTarget<'a> {
    /// Threaded reply to a message id.
    Reply(&'a str),
    /// Direct send to a chat id.
    Chat(&'a str),
}

/// Send a raw Feishu message payload (`msg_type` + pre-serialised
/// `content`). Returns `true` on HTTP+API success.
async fn send_feishu_payload(
    state: &FeishuState,
    target: FeishuTarget<'_>,
    msg_type: &str,
    content: &str,
) -> bool {
    let token = match state.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Feishu token error: {e}");
            return false;
        }
    };
    let (url, body) = match target {
        FeishuTarget::Reply(mid) => (
            format!("{FEISHU_API}/im/v1/messages/{mid}/reply"),
            serde_json::json!({ "msg_type": msg_type, "content": content }),
        ),
        FeishuTarget::Chat(cid) => (
            format!("{FEISHU_API}/im/v1/messages?receive_id_type=chat_id"),
            serde_json::json!({ "receive_id": cid, "msg_type": msg_type, "content": content }),
        ),
    };
    match state
        .http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            // Feishu wraps errors in 200 bodies with a non-zero `code`.
            match resp.json::<serde_json::Value>().await {
                Ok(v) => v.get("code").and_then(|c| c.as_i64()).unwrap_or(0) == 0,
                Err(_) => true,
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            warn!("Feishu card send failed ({status}): {}", truncate_bytes(&text, 200));
            false
        }
        Err(e) => {
            warn!("Feishu card send error: {e}");
            false
        }
    }
}

/// Card 2.0 payload with a `markdown` element — Feishu cards render
/// near-CommonMark natively, including tables and fenced code blocks.
fn build_feishu_card(markdown: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "2.0",
        "config": { "wide_screen_mode": true },
        "body": {
            "elements": [{ "tag": "markdown", "content": markdown }]
        }
    })
}

/// Feishu interactive-card size ceiling is ~30 KB; leave headroom for the
/// card scaffolding.
const FEISHU_CARD_BYTE_CAP: usize = 25_000;

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
            error!("Feishu send failed ({status}): {}", truncate_bytes(&text, 200));
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
            error!("Feishu reply failed ({status}): {}", truncate_bytes(&text, 200));
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

/// Verify a Feishu event-subscription signature.
///
/// Feishu computes `sha256(timestamp + nonce + token + raw_body)` and sends it
/// hex-encoded in the `X-Lark-Signature` header. We recompute and compare in
/// constant time. Returns `false` on any decode failure (fail-closed).
fn verify_feishu_signature(
    timestamp: &str,
    nonce: &str,
    token: &str,
    body: &[u8],
    signature: &str,
) -> bool {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(token.as_bytes());
    hasher.update(body);
    let digest = hasher.finalize();

    // Hex-encode without pulling in an extra crate.
    let mut expected = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        expected.push_str(&format!("{byte:02x}"));
    }

    constant_time_eq(expected.as_bytes(), signature.as_bytes())
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
    fn test_strip_feishu_mentions_multibyte_whitespace_no_panic() {
        // M26: U+3000 (ideographic space, 3 bytes) after the mention must not
        // panic on a mid-char slice. Also exercise CJK content around it.
        assert_eq!(strip_feishu_mentions("@_user_1\u{3000}你好"), "你好");
        assert_eq!(strip_feishu_mentions("早安 @_user_9\u{3000}世界"), "早安 世界");
        // Mention immediately followed by CJK with no whitespace at all.
        let only = strip_feishu_mentions("@_user_5你好嗎");
        assert!(only.is_empty() || only == "你好嗎" || only.starts_with('@') == false);
    }

    #[test]
    fn test_verify_feishu_signature() {
        use sha2::{Digest, Sha256};
        let (ts, nonce, token, body) = ("1700000000", "abc123", "tok", b"{\"a\":1}".as_slice());
        let mut h = Sha256::new();
        h.update(ts.as_bytes());
        h.update(nonce.as_bytes());
        h.update(token.as_bytes());
        h.update(body);
        let expected: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();

        assert!(verify_feishu_signature(ts, nonce, token, body, &expected));
        assert!(!verify_feishu_signature(ts, nonce, token, body, "deadbeef"));
        assert!(!verify_feishu_signature(ts, nonce, "wrong-token", body, &expected));
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
