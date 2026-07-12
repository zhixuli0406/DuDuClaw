//! DingTalk (釘釘) channel — enterprise internal robot (企業內部機器人).
//!
//! Inbound: DingTalk POSTs JSON messages to `POST /webhook/dingtalk` with
//! `timestamp` + `sign` headers. The signature is
//! `Base64(HmacSHA256(key = appSecret, msg = timestamp + "\n" + appSecret))`
//! and requests older than 1 hour are rejected — both checks fail-closed.
//! Source: open.dingtalk.com 企业内部开发机器人 (接收消息 / 安全验签).
//!
//! Outbound: replies go to the per-conversation `sessionWebhook` carried in
//! the inbound payload (valid until `sessionWebhookExpiredTime`, ~90 min).
//! Every inbound message persists `conversationId → sessionWebhook` to
//! `dingtalk_sessions.json` (same pattern as the Teams conversation-
//! reference store) so delegation forwarding can reach a conversation
//! later; sends past the webhook expiry return an error rather than
//! silently dropping (proactive sends beyond the session window are a
//! documented limitation — the robot oapi batchSend path is not wired).
//!
//! msgtype `markdown` (DingTalk renders a markdown subset: headers, bold,
//! links, images, quotes, lists — no tables) with `text` fallback.
//!
//! Config (`config.toml [channels]`, secrets encrypted at rest):
//! - `dingtalk_app_secret` (`_enc`) — robot AppSecret (signature key)
//! - `dingtalk_app_key` — robot AppKey / Client ID (stored for reference)

use std::path::Path;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use base64::Engine;
use duduclaw_core::truncate_bytes;
use tracing::{info, warn};

use crate::channel_reply::{build_reply_with_session, set_channel_connected, ReplyContext};

/// DingTalk message content cap is generous (~20000 chars for markdown);
/// chunk well below for display comfort.
const DINGTALK_TEXT_CHUNK: usize = 3500;

/// Reject callbacks whose `timestamp` header deviates from local time by
/// more than 1 hour (official replay-protection rule).
const MAX_CLOCK_SKEW_MS: i64 = 3600 * 1000;

// ── Signature verification ──────────────────────────────────────

/// Compute the DingTalk robot callback signature:
/// `Base64(HmacSHA256(key = app_secret, msg = "{timestamp}\n{app_secret}"))`.
pub(crate) fn dingtalk_sign(timestamp: &str, app_secret: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(app_secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(format!("{timestamp}\n{app_secret}").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Verify a DingTalk callback: signature match + clock-skew window.
/// `now_ms` is injected for testability. Fail-closed on any parse failure.
pub(crate) fn verify_dingtalk_callback(
    timestamp: &str,
    sign: &str,
    app_secret: &str,
    now_ms: i64,
) -> bool {
    let ts: i64 = match timestamp.parse() {
        Ok(t) => t,
        Err(_) => return false,
    };
    if (now_ms - ts).abs() > MAX_CLOCK_SKEW_MS {
        return false;
    }
    let expected = dingtalk_sign(timestamp, app_secret);
    constant_time_eq(expected.as_bytes(), sign.as_bytes())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── Session-webhook store ───────────────────────────────────────
//
// `sessionWebhook` is per-conversation and only arrives on inbound
// messages. Delegation forwarding needs to reach a conversation later, so
// every inbound message persists `conversationId → webhook + expiry`
// (advisory-locked read-modify-write, mirroring teams_conversations.json).

const SESSION_STORE_FILE: &str = "dingtalk_sessions.json";
const SESSION_STORE_CAP: usize = 500;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionRef {
    pub session_webhook: String,
    /// Epoch millis after which the webhook is dead.
    pub expired_at_ms: i64,
    pub updated_at: u64,
}

fn session_store_path(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join(SESSION_STORE_FILE)
}

fn load_session_store(home_dir: &Path) -> std::collections::HashMap<String, SessionRef> {
    std::fs::read_to_string(session_store_path(home_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write the session store owner-only (`0600`). The file contains tokened
/// sessionWebhook URLs — same pattern as `a2a_signing::write_key_owner_only`:
/// the mode is applied at `open` time (no `write` → `chmod` window), then
/// re-asserted in case the file pre-existed with looser perms.
fn write_store_owner_only(path: &Path, json: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(json.as_bytes())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, json)
    }
}

fn save_session_ref(home_dir: &Path, conversation_id: &str, sref: SessionRef) {
    let path = session_store_path(home_dir);
    let cid = conversation_id.to_string();
    let result = duduclaw_core::with_file_lock(&path, || {
        let mut store = load_session_store(home_dir);
        store.insert(cid.clone(), sref.clone());
        if store.len() > SESSION_STORE_CAP {
            let mut by_age: Vec<(String, u64)> = store
                .iter()
                .map(|(k, v)| (k.clone(), v.updated_at))
                .collect();
            by_age.sort_by_key(|(_, t)| *t);
            for (k, _) in by_age.into_iter().take(store.len() - SESSION_STORE_CAP) {
                store.remove(&k);
            }
        }
        let json = serde_json::to_string(&store).map_err(std::io::Error::other)?;
        write_store_owner_only(&path, &json)
    });
    if let Err(e) = result {
        warn!("DingTalk: failed to persist session webhook: {e}");
    }
}

pub fn lookup_session_ref(home_dir: &Path, conversation_id: &str) -> Option<SessionRef> {
    load_session_store(home_dir).get(conversation_id).cloned()
}

// ── Shared state ────────────────────────────────────────────────

struct DingTalkState {
    ctx: Arc<ReplyContext>,
    app_secret: String,
    http: reqwest::Client,
}

// ── Public API ──────────────────────────────────────────────────

/// Create the DingTalk webhook router. Returns `None` if not configured.
pub async fn start_dingtalk_webhook(home_dir: &Path, ctx: Arc<ReplyContext>) -> Option<Router> {
    let app_secret = read_dingtalk_config(home_dir, "dingtalk_app_secret").await?;
    if app_secret.is_empty() {
        return None;
    }

    info!("📌 DingTalk webhook starting");

    let state = Arc::new(DingTalkState {
        ctx: ctx.clone(),
        app_secret,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default(),
    });

    // The robot has no cheap liveness probe (replies ride sessionWebhooks),
    // so a configured secret marks the channel as ready.
    set_channel_connected(
        &ctx.channel_status,
        "dingtalk",
        true,
        None,
        Some(&ctx.event_tx),
    )
    .await;

    Some(
        Router::new()
            .route("/webhook/dingtalk", post(handle_webhook))
            .with_state(state),
    )
}

// ── Webhook handler ─────────────────────────────────────────────

async fn handle_webhook(
    State(state): State<Arc<DingTalkState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    // Fail-closed: both headers are mandatory.
    let timestamp = headers
        .get("timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let sign = headers
        .get("sign")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if timestamp.is_empty() || sign.is_empty() {
        warn!("DingTalk webhook missing timestamp/sign headers — rejecting");
        return (StatusCode::UNAUTHORIZED, "missing signature".into());
    }
    if !verify_dingtalk_callback(timestamp, sign, &state.app_secret, now_ms()) {
        warn!("DingTalk webhook signature mismatch or stale timestamp — rejecting");
        return (StatusCode::UNAUTHORIZED, "invalid signature".into());
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("DingTalk webhook parse error: {e}");
            return (StatusCode::BAD_REQUEST, "parse error".into());
        }
    };

    // ACK immediately; the LLM reply rides the sessionWebhook asynchronously.
    let st = state.clone();
    tokio::spawn(async move {
        handle_message(&payload, &st).await;
    });
    (StatusCode::OK, "{}".into())
}

async fn handle_message(payload: &serde_json::Value, state: &Arc<DingTalkState>) {
    let msg_type = payload
        .get("msgtype")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if msg_type != "text" {
        return;
    }
    let text = payload
        .get("text")
        .and_then(|t| t.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return;
    }
    let conversation_id = payload
        .get("conversationId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sender = payload
        .get("senderStaffId")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("senderId").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let session_webhook = payload
        .get("sessionWebhook")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let webhook_expired_ms = payload
        .get("sessionWebhookExpiredTime")
        .and_then(|v| v.as_i64())
        .unwrap_or(now_ms() + 85 * 60 * 1000);

    if session_webhook.is_empty() {
        warn!("DingTalk message without sessionWebhook — cannot reply");
        return;
    }

    // Persist the session webhook for later delegation forwarding.
    if !conversation_id.is_empty() {
        save_session_ref(
            &state.ctx.home_dir,
            &conversation_id,
            SessionRef {
                session_webhook: session_webhook.clone(),
                expired_at_ms: webhook_expired_ms,
                updated_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            },
        );
    }

    info!("📩 DingTalk [{sender}]: {}", truncate_bytes(&text, 80));

    let session_id = format!(
        "dingtalk:{}",
        if conversation_id.is_empty() {
            &sender
        } else {
            &conversation_id
        }
    );

    // Chat commands
    if crate::chat_commands::is_command(&text) {
        if let Some(cmd) = crate::chat_commands::parse_command(&text, None) {
            let agent_id = {
                let reg = state.ctx.registry.read().await;
                reg.main_agent()
                    .map(|a| a.config.agent.name.clone())
                    .unwrap_or_default()
            };
            let reply = crate::chat_commands::handle_command(
                &cmd,
                &state.ctx,
                &session_id,
                &agent_id,
                true,
            )
            .await;
            if !reply.trim().is_empty() {
                send_via_webhook(&state.http, &session_webhook, &reply, false).await;
            }
            return;
        }
    }

    // Progress callback — DingTalk has no typing API; forward tool progress
    // and the TodoUpdate task board as text messages (throttled 45 s;
    // TodoUpdate bypasses the throttle). Same pattern as Feishu.
    let progress_webhook = session_webhook.clone();
    let progress_http = state.http.clone();
    let last_progress = std::sync::Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .unwrap_or_else(std::time::Instant::now),
    ));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        if matches!(event, crate::channel_reply::ProgressEvent::Step { .. }) {
            return;
        }
        let is_todo = matches!(
            event,
            crate::channel_reply::ProgressEvent::TodoUpdate { .. }
        );
        {
            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
            if !is_todo && last.elapsed().as_secs() < 45 {
                return;
            }
            *last = std::time::Instant::now();
        }
        let msg_text = event.to_display();
        let http = progress_http.clone();
        let wh = progress_webhook.clone();
        tokio::spawn(async move {
            send_via_webhook(&http, &wh, &msg_text, false).await;
        });
    });

    let reply =
        build_reply_with_session(&text, &state.ctx, &session_id, &sender, Some(on_progress)).await;
    if reply.trim().is_empty() {
        warn!(conversation_id, "DingTalk: reply is empty — skipping send");
        return;
    }

    if now_ms() > webhook_expired_ms {
        warn!(
            conversation_id,
            "DingTalk sessionWebhook expired before the reply finished"
        );
        return;
    }
    send_via_webhook(&state.http, &session_webhook, &reply, true).await;
}

/// POST to a sessionWebhook. `try_markdown = true` sends msgtype `markdown`
/// first and falls back to `text` per chunk on rejection.
async fn send_via_webhook(http: &reqwest::Client, webhook: &str, text: &str, try_markdown: bool) {
    for chunk in crate::channel_format::split_text(text, DINGTALK_TEXT_CHUNK) {
        let mut delivered = false;
        if try_markdown {
            let body = serde_json::json!({
                "msgtype": "markdown",
                "markdown": { "title": markdown_title(&chunk), "text": chunk },
            });
            delivered = post_webhook(http, webhook, &body).await;
        }
        if !delivered {
            let body = serde_json::json!({
                "msgtype": "text",
                "text": { "content": chunk },
            });
            if !post_webhook(http, webhook, &body).await {
                warn!("DingTalk webhook send failed — dropping remaining chunks");
                return;
            }
        }
    }
}

/// DingTalk markdown requires a `title` (shown in the conversation list);
/// derive it from the first non-empty line.
fn markdown_title(text: &str) -> String {
    let first = text
        .lines()
        .map(|l| l.trim_start_matches(['#', ' ', '>', '-', '*']).trim())
        .find(|l| !l.is_empty())
        .unwrap_or("DuDuClaw");
    duduclaw_core::truncate_chars(first, 32)
}

/// SSRF guard for sessionWebhook URLs. The robot signature covers only
/// `timestamp + appSecret` — NOT the body — so a replayed (timestamp, sign)
/// pair with a swapped body could point `sessionWebhook` at an arbitrary
/// host and turn the gateway into a request proxy. Fail-closed: only HTTPS
/// URLs whose host is `dingtalk.com` or an anchored `*.dingtalk.com`
/// subdomain are ever POSTed to (never a `contains` check — that would
/// admit `dingtalk.com.evil.net`).
pub(crate) fn is_allowed_session_webhook(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == "dingtalk.com" || host.ends_with(".dingtalk.com")
}

/// Single webhook POST. DingTalk wraps failures in 200 bodies with a
/// non-zero `errcode`.
async fn post_webhook(http: &reqwest::Client, webhook: &str, body: &serde_json::Value) -> bool {
    if !is_allowed_session_webhook(webhook) {
        warn!("DingTalk sessionWebhook host not on the dingtalk.com allowlist — refusing send");
        return false;
    }
    match http.post(webhook).json(body).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                let code = v.get("errcode").and_then(|c| c.as_i64()).unwrap_or(0);
                if code != 0 {
                    let errmsg = v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("");
                    warn!(
                        "DingTalk webhook errcode {code}: {}",
                        truncate_bytes(errmsg, 200)
                    );
                }
                code == 0
            }
            Err(_) => true,
        },
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            warn!(
                "DingTalk webhook send failed ({status}): {}",
                truncate_bytes(&text, 200)
            );
            false
        }
        Err(e) => {
            // Scrub the URL: sessionWebhook URLs carry a session token in the
            // query string and reqwest's Display embeds the full URL.
            warn!(
                "DingTalk webhook send error: {}",
                crate::wecom::scrub_reqwest_err(e)
            );
            false
        }
    }
}

/// Delegation-forwarding / proactive send path: looks up the stored
/// sessionWebhook for `conversation_id`. Errors when the conversation has
/// never been seen or the webhook has expired (~90 min) — proactive sends
/// beyond the session window are not supported.
pub async fn send_text_to_conversation(
    home_dir: &Path,
    conversation_id: &str,
    text: &str,
) -> Result<(), String> {
    let sref = lookup_session_ref(home_dir, conversation_id).ok_or_else(|| {
        format!(
            "no stored sessionWebhook for {conversation_id} (robot must receive a message there first)"
        )
    })?;
    if now_ms() > sref.expired_at_ms {
        return Err(format!(
            "sessionWebhook for {conversation_id} expired — DingTalk robots can only reply \
             within ~90 min of the last inbound message"
        ));
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    for chunk in crate::channel_format::split_text(text, DINGTALK_TEXT_CHUNK) {
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": { "title": markdown_title(&chunk), "text": chunk },
        });
        if !post_webhook(&http, &sref.session_webhook, &body).await {
            let fallback = serde_json::json!({
                "msgtype": "text",
                "text": { "content": chunk },
            });
            if !post_webhook(&http, &sref.session_webhook, &fallback).await {
                return Err("DingTalk send failed".into());
            }
        }
    }
    Ok(())
}

async fn read_dingtalk_config(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-answer test: sign computed independently with Python
    /// hmac/hashlib/base64 over the same `timestamp + "\n" + secret` input.
    #[test]
    fn test_dingtalk_sign_known_answer() {
        let sign = dingtalk_sign("1577262236757", "this_is_the_app_secret");
        assert_eq!(sign, "xRYb1NisgH2f0Ff6VPblXkWwXk8F4mkuWTjV0kzOH0g=");
    }

    #[test]
    fn test_verify_accepts_fresh_valid_signature() {
        let ts = 1577262236757i64;
        let secret = "this_is_the_app_secret";
        let sign = dingtalk_sign(&ts.to_string(), secret);
        // Within the window (same instant, +30 min, -30 min).
        assert!(verify_dingtalk_callback(&ts.to_string(), &sign, secret, ts));
        assert!(verify_dingtalk_callback(
            &ts.to_string(),
            &sign,
            secret,
            ts + 30 * 60 * 1000
        ));
        assert!(verify_dingtalk_callback(
            &ts.to_string(),
            &sign,
            secret,
            ts - 30 * 60 * 1000
        ));
    }

    #[test]
    fn test_verify_rejects_bad_signature_and_skew() {
        let ts = 1577262236757i64;
        let secret = "this_is_the_app_secret";
        let sign = dingtalk_sign(&ts.to_string(), secret);
        // Wrong secret → reject.
        assert!(!verify_dingtalk_callback(
            &ts.to_string(),
            &sign,
            "other_secret",
            ts
        ));
        // Tampered sign → reject.
        assert!(!verify_dingtalk_callback(
            &ts.to_string(),
            "AAAA",
            secret,
            ts
        ));
        // > 1 h skew in either direction → reject (replay protection).
        assert!(!verify_dingtalk_callback(
            &ts.to_string(),
            &sign,
            secret,
            ts + MAX_CLOCK_SKEW_MS + 1
        ));
        assert!(!verify_dingtalk_callback(
            &ts.to_string(),
            &sign,
            secret,
            ts - MAX_CLOCK_SKEW_MS - 1
        ));
        // Non-numeric timestamp → reject (fail-closed).
        assert!(!verify_dingtalk_callback("not-a-number", &sign, secret, ts));
    }

    #[test]
    fn test_markdown_title() {
        assert_eq!(markdown_title("# 標題\n內文"), "標題");
        assert_eq!(markdown_title("\n\nhello world"), "hello world");
        assert_eq!(markdown_title("   \n  "), "DuDuClaw");
        // CJK-safe truncation at 32 chars.
        let long = "很".repeat(64);
        assert_eq!(markdown_title(&long).chars().count(), 32);
    }

    #[test]
    fn test_payload_parse_shape() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{
                "msgtype": "text",
                "text": { "content": "幫我查一下訂單" },
                "senderStaffId": "manager123",
                "senderNick": "小明",
                "conversationId": "cid6906",
                "conversationType": "2",
                "sessionWebhook": "https://oapi.dingtalk.com/robot/sendBySession?session=abc",
                "sessionWebhookExpiredTime": 1577267236757
            }"#,
        )
        .unwrap();
        assert_eq!(payload["msgtype"], "text");
        assert_eq!(payload["text"]["content"], "幫我查一下訂單");
        assert_eq!(payload["senderStaffId"], "manager123");
        assert!(payload["sessionWebhook"]
            .as_str()
            .unwrap()
            .starts_with("https://"));
    }

    #[test]
    fn test_session_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        save_session_ref(
            home,
            "cid-1",
            SessionRef {
                session_webhook: "https://oapi.dingtalk.com/robot/sendBySession?session=x".into(),
                expired_at_ms: 42,
                updated_at: 1,
            },
        );
        let got = lookup_session_ref(home, "cid-1").expect("stored ref found");
        assert_eq!(got.expired_at_ms, 42);
        assert!(lookup_session_ref(home, "cid-unknown").is_none());
    }

    /// LOW-A: the session store carries tokened webhook URLs — owner-only.
    #[cfg(unix)]
    #[test]
    fn test_session_store_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // Pre-create with loose perms to prove they get re-asserted to 0600.
        let store = session_store_path(home);
        std::fs::write(&store, "{}").unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_session_ref(
            home,
            "cid-perm",
            SessionRef {
                session_webhook: "https://oapi.dingtalk.com/robot/sendBySession?session=t".into(),
                expired_at_ms: 1,
                updated_at: 1,
            },
        );
        let mode = std::fs::metadata(&store).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "session store must be owner-only, got {mode:o}");
    }

    /// MED-A: sessionWebhook SSRF allowlist is anchored and fail-closed.
    #[test]
    fn test_session_webhook_allowlist() {
        // Legit DingTalk endpoints.
        assert!(is_allowed_session_webhook(
            "https://oapi.dingtalk.com/robot/sendBySession?session=abc"
        ));
        assert!(is_allowed_session_webhook("https://api.dingtalk.com/x"));
        assert!(is_allowed_session_webhook("https://dingtalk.com/x"));
        // Anchored: suffix tricks and lookalike hosts are rejected.
        assert!(!is_allowed_session_webhook("https://dingtalk.com.evil.net/x"));
        assert!(!is_allowed_session_webhook("https://evildingtalk.com/x"));
        assert!(!is_allowed_session_webhook("https://oapi.dingtalk.com.attacker.io/r"));
        // Plain HTTP, other hosts, garbage → reject (fail-closed).
        assert!(!is_allowed_session_webhook("http://oapi.dingtalk.com/robot"));
        assert!(!is_allowed_session_webhook("https://internal-service.local/admin"));
        assert!(!is_allowed_session_webhook("https://127.0.0.1:8080/steal"));
        assert!(!is_allowed_session_webhook("not a url"));
        assert!(!is_allowed_session_webhook(""));
    }
}
