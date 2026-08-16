//! WhatsApp Cloud API integration (Meta Business Platform).
//!
//! Uses webhook for receiving messages and REST API for sending.
//! Webhook URL: `POST /webhook/whatsapp`
//! Verification: `GET /webhook/whatsapp?hub.mode=subscribe&hub.verify_token=...&hub.challenge=...`

use std::path::Path;
use std::sync::Arc;

use duduclaw_core::truncate_bytes;
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{error, info, warn};

use crate::channel_reply::{ReplyContext, build_reply_with_session, set_channel_connected};

const GRAPH_API: &str = "https://graph.facebook.com/v20.0";

type HmacSha256 = Hmac<Sha256>;

// ── WhatsApp API types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WebhookBody {
    entry: Vec<WebhookEntry>,
}

#[derive(Debug, Deserialize)]
struct WebhookEntry {
    changes: Vec<WebhookChange>,
}

#[derive(Debug, Deserialize)]
struct WebhookChange {
    value: ChangeValue,
}

#[derive(Debug, Deserialize)]
struct ChangeValue {
    messages: Option<Vec<WaMessage>>,
    metadata: Option<WaMetadata>,
}

#[derive(Debug, Deserialize)]
struct WaMessage {
    /// WhatsApp message id (wamid) — used for the typing indicator /
    /// read-receipt API.
    #[serde(default)]
    id: String,
    from: String,
    #[serde(rename = "type")]
    msg_type: String,
    text: Option<WaText>,
    image: Option<WaMedia>,
    audio: Option<WaMedia>,
    video: Option<WaMedia>,
    document: Option<WaDocument>,
    #[allow(dead_code)]
    timestamp: String,
    /// Reply/forward context. Present when the user replied to (quoted) an
    /// earlier message or forwarded one. Cloud API sends only the quoted
    /// message's wamid — never its body — so this can annotate but not
    /// reconstruct the quoted text.
    context: Option<WaContext>,
}

#[derive(Debug, Deserialize)]
struct WaContext {
    /// wamid of the message the user replied to.
    id: Option<String>,
    #[allow(dead_code)]
    from: Option<String>,
    #[serde(default)]
    forwarded: bool,
    #[serde(default)]
    frequently_forwarded: bool,
}

#[derive(Debug, Deserialize)]
struct WaMedia {
    id: String,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WaDocument {
    id: String,
    filename: Option<String>,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WaText {
    body: String,
}

#[derive(Debug, Deserialize)]
struct WaMetadata {
    phone_number_id: String,
}

#[derive(Debug, Serialize)]
struct SendTextMessage {
    messaging_product: String,
    to: String,
    text: SendText,
}

#[derive(Debug, Serialize)]
struct SendText {
    body: String,
}

#[derive(Debug, Deserialize)]
struct VerifyQuery {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
}

// ── Shared state ────────────────────────────────────────────────

/// Credentials doctrine P2 (WP-8A): none of `access_token` / `verify_token` /
/// `app_secret` / `phone_number_id` are stored here — every handler re-reads
/// them from config via `ctx.home_dir` (mirroring `line.rs`'s per-request
/// pattern), so a dashboard credential edit (including rotating a leaked
/// `app_secret`) takes effect on the very next webhook call instead of
/// requiring this task or the gateway to restart.
struct WhatsAppState {
    ctx: Arc<ReplyContext>,
    http: reqwest::Client,
}

// ── Public API ──────────────────────────────────────────────────

/// Create the WhatsApp webhook router.
///
/// Returns `None` if WhatsApp is not configured.
pub async fn start_whatsapp_webhook(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<Router> {
    let access_token = read_wa_config(home_dir, "whatsapp_access_token").await?;
    let verify_token = read_wa_config(home_dir, "whatsapp_verify_token").await?;
    let phone_number_id = read_wa_config(home_dir, "whatsapp_phone_number_id").await?;
    let app_secret = read_wa_config(home_dir, "whatsapp_app_secret").await.unwrap_or_default();

    if access_token.is_empty() || phone_number_id.is_empty() {
        return None;
    }

    // WP-H1: an empty app_secret used to make signature verification skip
    // entirely (fail-open) — any inbound POST to the webhook URL was accepted
    // and processed, regardless of source. Same threat model as LINE's
    // missing-channel-secret check and Feishu's missing-verification-token
    // refusal-to-start. WhatsApp still mounts the route (the access token /
    // phone number id are enough to *send*), but every inbound webhook
    // request is rejected until an app_secret is configured — see
    // `receive_webhook` below.
    if app_secret.is_empty() {
        warn!(
            "⚠️  WhatsApp webhook starting WITHOUT app_secret: signature verification is \
             impossible, so ALL inbound webhook requests will be rejected (401) until an \
             App Secret is set in the dashboard channel settings. Outbound sending is \
             unaffected."
        );
    }

    info!("📱 WhatsApp webhook starting (phone: {phone_number_id})");
    set_channel_connected(&ctx.channel_status, "whatsapp", true, None, Some(&ctx.event_tx)).await;
    // access_token / verify_token / app_secret / phone_number_id above are
    // used only to decide whether to mount the router at all — they are
    // intentionally not stored in `WhatsAppState` (WP-8A: see its doc
    // comment). `let _ =` keeps the emptiness/gating logic above intact
    // without an unused-binding warning now that nothing captures them.
    let _ = (access_token, verify_token, app_secret, phone_number_id);

    let state = Arc::new(WhatsAppState {
        ctx,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default(),
    });

    Some(
        Router::new()
            .route("/webhook/whatsapp", get(verify_webhook))
            .route("/webhook/whatsapp", post(receive_webhook))
            .with_state(state),
    )
}

// ── Webhook handlers ────────────────────────────────────────────

async fn verify_webhook(
    State(state): State<Arc<WhatsAppState>>,
    Query(query): Query<VerifyQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Length limit on challenge to prevent abuse
    if let Some(ref challenge) = query.challenge {
        if challenge.len() > 256 {
            return (StatusCode::BAD_REQUEST, "challenge too long").into_response();
        }
    }

    // WP-8A: re-read fresh rather than trust a value captured at task-spawn
    // time — a rotated verify_token is honoured on the very next handshake.
    let verify_token = read_wa_config(&state.ctx.home_dir, "whatsapp_verify_token")
        .await
        .unwrap_or_default();

    if query.mode.as_deref() == Some("subscribe")
        && query
            .verify_token
            .as_deref()
            .map(|t| constant_time_eq(t.as_bytes(), verify_token.as_bytes()))
            .unwrap_or(false)
    {
        if let Some(challenge) = query.challenge {
            info!("WhatsApp webhook verified");
            return (StatusCode::OK, challenge).into_response();
        }
    }

    (StatusCode::FORBIDDEN, "Verification failed").into_response()
}

async fn receive_webhook(
    State(state): State<Arc<WhatsAppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    // WP-8A / credentials doctrine P2: re-read every credential fresh for
    // this request instead of trusting values captured at task-spawn time.
    // Shadows the rest of this function so every downstream use (signature
    // verification, media download, outbound send) automatically picks up
    // the current config — mirrors `line.rs`'s per-request pattern.
    let access_token = read_wa_config(&state.ctx.home_dir, "whatsapp_access_token")
        .await
        .unwrap_or_default();
    let phone_number_id = read_wa_config(&state.ctx.home_dir, "whatsapp_phone_number_id")
        .await
        .unwrap_or_default();
    let app_secret = read_wa_config(&state.ctx.home_dir, "whatsapp_app_secret")
        .await
        .unwrap_or_default();

    // WP-H1: fail-closed. An empty app_secret means signature verification is
    // impossible — previously this silently skipped verification and accepted
    // every inbound request unauthenticated (webhook wide open to anyone who
    // knew the URL). Reject instead, same posture as LINE (missing channel
    // secret) and Feishu (refuses to even start without a verification
    // token). The gating decision lives in the pure `webhook_signature_ok`
    // helper so it's covered by unit tests independent of axum state; the
    // branches below only decide which log message to emit.
    let sig_str = headers.get("x-hub-signature-256").and_then(|h| h.to_str().ok());
    if !webhook_signature_ok(&app_secret, sig_str, &body) {
        if app_secret.is_empty() {
            // Warn once per process so a misconfigured deployment doesn't
            // spam logs on every dropped request.
            static MISSING_SECRET_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            if MISSING_SECRET_WARNED.set(()).is_ok() {
                warn!(
                    "WhatsApp webhook: app_secret not configured — webhook 已拒收所有 inbound \
                     請求（fail-closed）。請至 dashboard 通道設定補設 WhatsApp App Secret 以恢復收訊。"
                );
            }
        } else if sig_str.is_none() {
            warn!("WhatsApp webhook: missing required x-hub-signature-256 header");
        } else {
            warn!("WhatsApp webhook: signature verification failed");
        }
        return StatusCode::UNAUTHORIZED;
    }

    let webhook: WebhookBody = match serde_json::from_slice(&body) {
        Ok(w) => w,
        Err(e) => {
            warn!("WhatsApp webhook parse error: {e}");
            return StatusCode::BAD_REQUEST;
        }
    };

    for entry in &webhook.entry {
        for change in &entry.changes {
            let phone_id = change
                .value
                .metadata
                .as_ref()
                .map(|m| m.phone_number_id.clone())
                .unwrap_or_else(|| phone_number_id.clone());

            if let Some(messages) = &change.value.messages {
                for msg in messages {
                    let supported_types = ["text", "image", "audio", "video", "document"];
                    if !supported_types.contains(&msg.msg_type.as_str()) {
                        continue;
                    }

                    let sender = &msg.from;
                    let base_text = msg.text.as_ref().map(|t| t.body.clone()).unwrap_or_default();
                    let mut attachment_lines: Vec<String> = Vec::new();
                    // WP1.3: land inbound files under the resolved agent's dir.
                    let attach_base = crate::channel_reply::resolve_attachment_base(
                        state.ctx.as_ref(), None,
                    ).await;

                    // ── Download and save media attachments ──
                    let media_info: Option<(&str, &str, &str)> = match msg.msg_type.as_str() {
                        "image" => msg.image.as_ref().map(|m| {
                            (m.id.as_str(), m.mime_type.as_deref().unwrap_or("image/jpeg"), "image")
                        }),
                        "audio" => msg.audio.as_ref().map(|m| {
                            (m.id.as_str(), m.mime_type.as_deref().unwrap_or("audio/ogg"), "audio")
                        }),
                        "video" => msg.video.as_ref().map(|m| {
                            (m.id.as_str(), m.mime_type.as_deref().unwrap_or("video/mp4"), "video")
                        }),
                        _ => None,
                    };

                    if let Some((media_id, mime, type_label)) = media_info {
                        info!("📩 WhatsApp [{sender}]: {type_label} message");
                        match download_media(&state.http, &access_token, media_id).await {
                            Ok(data) => {
                                let mt = crate::media::media_type_from_mime(mime);
                                let ext = crate::media::extension_from_mime(mime);
                                let fname = format!("{type_label}.{ext}");
                                match crate::media::save_attachment_in_base(&attach_base, &data, &fname).await {
                                    Ok(path) => {
                                        attachment_lines.push(crate::media::format_attachment_ref(&mt, &fname, &path));
                                    }
                                    Err(e) => warn!("Failed to save WhatsApp {type_label}: {e}"),
                                }
                            }
                            Err(e) => warn!("Failed to download WhatsApp {type_label}: {e}"),
                        }
                    }

                    // Handle document (has filename)
                    if let Some(doc) = &msg.document {
                        let mime = doc.mime_type.as_deref().unwrap_or("application/octet-stream");
                        let fname = doc.filename.as_deref().unwrap_or("document");
                        info!("📩 WhatsApp [{sender}]: document ({fname})");
                        match download_media(&state.http, &access_token, &doc.id).await {
                            Ok(data) => {
                                let mt = crate::media::media_type_from_mime(mime);
                                match crate::media::save_attachment_in_base(&attach_base, &data, fname).await {
                                    Ok(path) => {
                                        attachment_lines.push(crate::media::format_attachment_ref(&mt, fname, &path));
                                    }
                                    Err(e) => warn!("Failed to save WhatsApp document: {e}"),
                                }
                            }
                            Err(e) => warn!("Failed to download WhatsApp document: {e}"),
                        }
                    }

                    // ── Reply/forward annotation ──
                    // Cloud API never includes the quoted message body, so the
                    // best available signal is an explicit annotation — without
                    // it the agent has no idea the user was replying at all.
                    let mut context_lines: Vec<String> = Vec::new();
                    // Never prefix a chat command — the command parser matches
                    // on the leading slash of the whole input.
                    if let Some(wa_ctx) = msg.context.as_ref().filter(|_| !base_text.trim_start().starts_with('/')) {
                        if wa_ctx.forwarded || wa_ctx.frequently_forwarded {
                            context_lines.push("〔此訊息為使用者轉發的內容，非使用者本人所寫〕".to_string());
                        }
                        if wa_ctx.id.is_some() && !wa_ctx.forwarded && !wa_ctx.frequently_forwarded {
                            context_lines.push(
                                "〔使用者以「回覆」引用了一則先前訊息；WhatsApp 未附引用原文，請從近期對話推斷所指內容〕"
                                    .to_string(),
                            );
                        }
                    }
                    let base_text = if context_lines.is_empty() {
                        base_text
                    } else {
                        format!("{}\n{base_text}", context_lines.join("\n"))
                    };

                    // Combine text + attachments
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

                    info!("📩 WhatsApp [{sender}]: {}", truncate_bytes(&input_text, 80));

                    // Chat commands
                    if crate::chat_commands::is_command(&input_text) {
                        if let Some(cmd) = crate::chat_commands::parse_command(&input_text, None) {
                            let session_id = format!("whatsapp:{sender}");
                            let agent_id = {
                                let reg = state.ctx.registry.read().await;
                                reg.main_agent()
                                    .map(|a| a.config.agent.name.clone())
                                    .unwrap_or_default()
                            };
                            let reply = crate::chat_commands::handle_command(
                                &cmd, &state.ctx, &session_id, &agent_id, true, sender,
                            ).await;
                            send_text(&state.http, &access_token, &phone_id, sender, &reply).await;
                            continue;
                        }
                    }

                    // Typing indicator + read receipt (shows ≤25s or until
                    // the reply arrives; one-shot — tied to the inbound wamid).
                    if !msg.id.is_empty() {
                        crate::channel_typing::whatsapp_typing_once(
                            &state.http, &access_token, &phone_id, &msg.id,
                        )
                        .await;
                    }

                    // Progress callback — WhatsApp has no message-edit API and
                    // progress texts consume conversation quota, so only the
                    // meaningful TodoUpdate board is forwarded (throttled 60s).
                    let progress_http = state.http.clone();
                    let progress_token = access_token.clone();
                    let progress_phone = phone_id.clone();
                    let progress_to = sender.clone();
                    let last_progress = Arc::new(std::sync::Mutex::new(
                        std::time::Instant::now()
                            .checked_sub(std::time::Duration::from_secs(120))
                            .unwrap_or_else(std::time::Instant::now),
                    ));
                    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
                        if !matches!(event, crate::channel_reply::ProgressEvent::TodoUpdate { .. }) {
                            return;
                        }
                        {
                            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
                            let throttle =
                                crate::channel_capabilities::progress_throttle_secs("whatsapp")
                                    .unwrap_or(60);
                            if last.elapsed().as_secs() < throttle {
                                return;
                            }
                            *last = std::time::Instant::now();
                        }
                        let msg_text = event.to_display();
                        let c = progress_http.clone();
                        let t = progress_token.clone();
                        let p = progress_phone.clone();
                        let to = progress_to.clone();
                        tokio::spawn(async move {
                            send_text(&c, &t, &p, &to, &msg_text).await;
                        });
                    });

                    let session_id = format!("whatsapp:{sender}");
                    let reply = build_reply_with_session(
                        &input_text, &state.ctx, &session_id, sender, Some(on_progress),
                    ).await;

                    // WP1.3: 📎DELIVER: outbound — upload generated files via
                    // the WhatsApp media API, strip the marker.
                    let reply = {
                        let doc_sender = crate::channel_sender::WhatsAppSender {
                            access_token: access_token.clone(),
                            phone_number_id: phone_id.clone(),
                            to: sender.clone(),
                            http: state.http.clone(),
                        };
                        crate::channel_reply::deliver_documents_for_reply(
                            state.ctx.as_ref(), None, reply, &doc_sender,
                        ).await
                    };

                    // Guard: don't send empty replies
                    if reply.trim().is_empty() {
                        warn!("WhatsApp: reply is empty for {sender} — skipping send");
                        continue;
                    }

                    // Markdown → WhatsApp formatting (*bold*, ~strike~,
                    // tables → monospace blocks), chunked under the 4096 cap.
                    let formatted = crate::markdown_render::to_whatsapp_text(&reply);
                    for chunk in crate::channel_format::split_text(&formatted, 4000) {
                        send_text(&state.http, &access_token, &phone_id, sender, &chunk).await;
                    }
                }
            }
        }
    }

    StatusCode::OK
}

// ── Send helpers ────────────────────────────────────────────────

async fn send_text(
    http: &reqwest::Client,
    token: &str,
    phone_number_id: &str,
    to: &str,
    text: &str,
) {
    let body = SendTextMessage {
        messaging_product: "whatsapp".to_string(),
        to: to.to_string(),
        text: SendText {
            body: text.to_string(),
        },
    };

    match http
        .post(format!("{GRAPH_API}/{phone_number_id}/messages"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            error!("WhatsApp send failed ({status}): {}", truncate_bytes(&text, 200));
        }
        Err(e) => error!("WhatsApp send error: {e}"),
        _ => {}
    }
}

/// Download a media file from the WhatsApp Cloud API.
async fn download_media(
    http: &reqwest::Client,
    token: &str,
    media_id: &str,
) -> Result<Vec<u8>, String> {
    let url_resp: serde_json::Value = http
        .get(format!("{GRAPH_API}/{media_id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let download_url = url_resp
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("No URL in media response")?;
    let bytes = http
        .get(download_url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    Ok(bytes.to_vec())
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn verify_signature(body: &[u8], secret: &str, signature: &str) -> bool {
    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());
    constant_time_eq(computed.as_bytes(), expected.as_bytes())
}

/// WP-H1: pure authorization gate for the WhatsApp webhook. Fail-closed —
/// an empty `app_secret` (verification impossible) or a missing/invalid
/// signature header both return `false`. Previously an empty secret made
/// the caller skip verification entirely and return `true` unconditionally.
fn webhook_signature_ok(app_secret: &str, sig_header: Option<&str>, body: &[u8]) -> bool {
    if app_secret.is_empty() {
        return false;
    }
    match sig_header {
        Some(sig) => verify_signature(body, app_secret, sig),
        None => false,
    }
}

async fn read_wa_config(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_webhook_body() {
        let json = r#"{"entry":[{"changes":[{"value":{"messages":[{"from":"886912345678","type":"text","text":{"body":"Hello"},"timestamp":"1234567890"}],"metadata":{"phone_number_id":"123456"}}}]}]}"#;
        let body: WebhookBody = serde_json::from_str(json).unwrap();
        let msg = &body.entry[0].changes[0].value.messages.as_ref().unwrap()[0];
        assert_eq!(msg.from, "886912345678");
        assert_eq!(msg.text.as_ref().unwrap().body, "Hello");
    }

    #[test]
    fn test_verify_query_parse() {
        let json = r#"{"hub.mode":"subscribe","hub.verify_token":"mytoken","hub.challenge":"challenge123"}"#;
        let q: VerifyQuery = serde_json::from_str(json).unwrap();
        assert_eq!(q.mode.as_deref(), Some("subscribe"));
        assert_eq!(q.challenge.as_deref(), Some("challenge123"));
    }

    #[test]
    fn test_send_text_message_body_format() {
        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": "886912345678",
            "text": { "body": "Hello from DuDuClaw!" }
        });
        assert_eq!(body["messaging_product"], "whatsapp");
        assert_eq!(body["to"], "886912345678");
        assert_eq!(body["text"]["body"], "Hello from DuDuClaw!");
    }

    // ── WP-H1: WhatsApp webhook signature fail-closed ──────────────

    /// Empty app_secret must reject the request outright — no fallback to
    /// "accept unauthenticated". This is the regression test for the
    /// original fail-open bug (app_secret empty ⇒ verification silently
    /// skipped ⇒ any inbound POST accepted).
    #[test]
    fn test_webhook_signature_ok_rejects_empty_secret_even_with_valid_looking_header() {
        let body = b"{\"entry\":[]}";
        // Even a syntactically well-formed signature header must not help —
        // there is no secret to verify it against.
        assert!(!webhook_signature_ok("", Some("sha256=deadbeef"), body));
        assert!(!webhook_signature_ok("", None, body));
    }

    /// A configured secret + correctly computed signature passes.
    #[test]
    fn test_webhook_signature_ok_accepts_valid_signature() {
        let secret = "test_app_secret";
        let body = b"{\"entry\":[{\"changes\":[]}]}";
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert!(webhook_signature_ok(secret, Some(&sig), body));
    }

    /// A configured secret + wrong/forged signature is rejected.
    #[test]
    fn test_webhook_signature_ok_rejects_invalid_signature() {
        let secret = "test_app_secret";
        let body = b"{\"entry\":[{\"changes\":[]}]}";
        assert!(!webhook_signature_ok(secret, Some("sha256=0000forged0000"), body));
        // Body tampered after signing (secret is correct, payload is not).
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(b"{\"entry\":[{\"changes\":[{\"tampered\":true}]}]}");
        let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert!(!webhook_signature_ok(secret, Some(&sig), body));
    }

    /// A configured secret but a missing signature header is rejected (the
    /// header is mandatory once a secret exists — no anonymous fallback).
    #[test]
    fn test_webhook_signature_ok_rejects_missing_header_when_secret_configured() {
        let body = b"{\"entry\":[]}";
        assert!(!webhook_signature_ok("test_app_secret", None, body));
    }
}
