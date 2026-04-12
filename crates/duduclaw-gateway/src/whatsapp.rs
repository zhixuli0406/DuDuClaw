//! WhatsApp Cloud API integration (Meta Business Platform).
//!
//! Uses webhook for receiving messages and REST API for sending.
//! Webhook URL: `POST /webhook/whatsapp`
//! Verification: `GET /webhook/whatsapp?hub.mode=subscribe&hub.verify_token=...&hub.challenge=...`

use std::path::Path;
use std::sync::Arc;

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

struct WhatsAppState {
    ctx: Arc<ReplyContext>,
    access_token: String,
    verify_token: String,
    app_secret: String,
    phone_number_id: String,
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

    info!("📱 WhatsApp webhook starting (phone: {phone_number_id})");
    set_channel_connected(&ctx.channel_status, "whatsapp", true, None, Some(&ctx.event_tx)).await;

    let state = Arc::new(WhatsAppState {
        ctx,
        access_token,
        verify_token,
        app_secret,
        phone_number_id,
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

    if query.mode.as_deref() == Some("subscribe")
        && query
            .verify_token
            .as_deref()
            .map(|t| constant_time_eq(t.as_bytes(), state.verify_token.as_bytes()))
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
    // Verify signature if app_secret is configured — header is mandatory when secret exists
    if !state.app_secret.is_empty() {
        let sig_str = match headers.get("x-hub-signature-256") {
            Some(h) => h.to_str().unwrap_or(""),
            None => {
                warn!("WhatsApp webhook: missing required x-hub-signature-256 header");
                return StatusCode::UNAUTHORIZED;
            }
        };
        if !verify_signature(&body, &state.app_secret, sig_str) {
            warn!("WhatsApp webhook: signature verification failed");
            return StatusCode::UNAUTHORIZED;
        }
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
                .unwrap_or_else(|| state.phone_number_id.clone());

            if let Some(messages) = &change.value.messages {
                for msg in messages {
                    let supported_types = ["text", "image", "audio", "video", "document"];
                    if !supported_types.contains(&msg.msg_type.as_str()) {
                        continue;
                    }

                    let sender = &msg.from;
                    let base_text = msg.text.as_ref().map(|t| t.body.clone()).unwrap_or_default();
                    let mut attachment_lines: Vec<String> = Vec::new();

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
                        match download_media(&state.http, &state.access_token, media_id).await {
                            Ok(data) => {
                                let mt = crate::media::media_type_from_mime(mime);
                                let ext = crate::media::extension_from_mime(mime);
                                let fname = format!("{type_label}.{ext}");
                                match crate::media::save_attachment_to_disk(&state.ctx.home_dir, &data, &fname).await {
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
                        match download_media(&state.http, &state.access_token, &doc.id).await {
                            Ok(data) => {
                                let mt = crate::media::media_type_from_mime(mime);
                                match crate::media::save_attachment_to_disk(&state.ctx.home_dir, &data, fname).await {
                                    Ok(path) => {
                                        attachment_lines.push(crate::media::format_attachment_ref(&mt, fname, &path));
                                    }
                                    Err(e) => warn!("Failed to save WhatsApp document: {e}"),
                                }
                            }
                            Err(e) => warn!("Failed to download WhatsApp document: {e}"),
                        }
                    }

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

                    info!("📩 WhatsApp [{sender}]: {}", &input_text[..input_text.len().min(80)]);

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
                                &cmd, &state.ctx, &session_id, &agent_id, true,
                            ).await;
                            send_text(&state.http, &state.access_token, &phone_id, sender, &reply).await;
                            continue;
                        }
                    }

                    let session_id = format!("whatsapp:{sender}");
                    let reply = build_reply_with_session(
                        &input_text, &state.ctx, &session_id, sender, None,
                    ).await;

                    // Guard: don't send empty replies
                    if reply.trim().is_empty() {
                        warn!("WhatsApp: reply is empty for {sender} — skipping send");
                        continue;
                    }

                    send_text(&state.http, &state.access_token, &phone_id, sender, &reply).await;
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
            error!("WhatsApp send failed ({status}): {}", &text[..text.len().min(200)]);
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
}
