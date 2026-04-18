//! Channel-native feedback abstraction for Computer Use.
//!
//! Provides a unified `ChannelSender` trait so the `ComputerUseOrchestrator`
//! can send screenshots, text updates, and confirmation requests back to the
//! user's messaging channel without knowing which channel is in use.
//!
//! Supported channels (all 7):
//! - Telegram  — Bot API sendMessage / sendPhoto
//! - LINE      — Messaging API push message
//! - Discord   — REST API Create Message + attachment
//! - Slack     — Web API chat.postMessage + files.upload
//! - WhatsApp  — Cloud API messages (text / image via media upload)
//! - Feishu    — Open API send message (text / image)
//! - WebChat   — WebSocket JSON envelope
//!
//! The user can be on their phone — all interaction happens in-channel,
//! not via a Dashboard.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use tokio::sync::{Mutex, oneshot};
use tracing::warn;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error type for channel send operations.
#[derive(Debug)]
pub struct ChannelSendError(pub String);

impl std::fmt::Display for ChannelSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "channel send error: {}", self.0)
    }
}

impl std::error::Error for ChannelSendError {}

// ---------------------------------------------------------------------------
// Confirmation reply system
// ---------------------------------------------------------------------------

/// Global registry of pending confirmations.
///
/// When a sender calls `request_confirmation()`, it registers a oneshot channel
/// here keyed by the user/chat ID. When the channel handler receives the user's
/// reply (「確認」「好」「yes」or 「取消」「no」), it calls `resolve_confirmation()`
/// which sends the result through the oneshot.
static CONFIRMATION_REGISTRY: std::sync::OnceLock<
    Mutex<HashMap<String, oneshot::Sender<bool>>>,
> = std::sync::OnceLock::new();

fn confirmation_registry() -> &'static Mutex<HashMap<String, oneshot::Sender<bool>>> {
    CONFIRMATION_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Wait for a user's confirmation reply with timeout.
///
/// Called by `ChannelSender::request_confirmation()`. Registers a oneshot
/// and waits for the channel handler to call `resolve_confirmation()`.
pub async fn wait_for_confirmation(
    user_id: &str,
    timeout_secs: u64,
) -> Result<bool, ChannelSendError> {
    let (tx, rx) = oneshot::channel();
    confirmation_registry().lock().await.insert(user_id.to_string(), tx);

    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        rx,
    )
    .await
    {
        Ok(Ok(confirmed)) => Ok(confirmed),
        Ok(Err(_)) => {
            // Sender dropped — treat as declined
            Ok(false)
        }
        Err(_) => {
            // Timeout — remove from registry and treat as declined
            confirmation_registry().lock().await.remove(user_id);
            Ok(false)
        }
    }
}

/// Resolve a pending confirmation from a user's reply.
///
/// Called by channel message handlers (Telegram, LINE, etc.) when the user
/// replies to a confirmation prompt. The reply text is matched against
/// known confirmation/denial words.
///
/// Returns `true` if there was a pending confirmation for this user.
pub async fn resolve_confirmation(user_id: &str, reply_text: &str) -> bool {
    let sender = confirmation_registry().lock().await.remove(user_id);
    if let Some(tx) = sender {
        let confirmed = is_confirmation_reply(reply_text);
        let _ = tx.send(confirmed);
        true
    } else {
        false
    }
}

/// Check if a reply text is a positive confirmation.
fn is_confirmation_reply(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    matches!(
        t.as_str(),
        "yes" | "y" | "ok" | "sure" | "confirm"
            | "好" | "確認" | "繼續" | "可以" | "對"
            | "はい" | "うん"
    )
}

/// Check if there are any pending confirmations for a user.
pub async fn has_pending_confirmation(user_id: &str) -> bool {
    confirmation_registry().lock().await.contains_key(user_id)
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction for sending messages/photos back to a messaging channel.
///
/// Implementations exist for each of the 7 supported channels.
/// The orchestrator holds a `&dyn ChannelSender` and uses it to report
/// screenshots, progress, and confirmations.
#[async_trait]
pub trait ChannelSender: Send + Sync {
    /// Send a text message to the channel.
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError>;

    /// Send a photo (PNG bytes) with an optional caption.
    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError>;

    /// Request confirmation from the user and wait for their reply.
    ///
    /// Returns `true` if the user confirmed, `false` otherwise.
    /// Times out after `timeout_secs` (default: 60s).
    async fn request_confirmation(
        &self,
        prompt: &str,
        screenshot: Option<&[u8]>,
        timeout_secs: u64,
    ) -> Result<bool, ChannelSendError>;

    /// Channel type identifier (e.g., "telegram", "line").
    fn channel_type(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Channel identifier for sender construction.
#[derive(Debug, Clone)]
pub struct ChannelTarget {
    /// Channel type: "telegram", "line", "discord", "slack", "whatsapp", "feishu", "webchat"
    pub(crate) channel_type: String,
    /// Chat/channel/room ID in that platform.
    pub(crate) chat_id: String,
    /// Bot token or access token for the platform.
    pub(crate) token: String,
    /// Additional platform-specific identifier (e.g., WhatsApp phone_number_id, Discord user_id).
    pub(crate) extra_id: Option<String>,
}

/// Create a `Box<dyn ChannelSender>` for the given channel target.
///
/// This is the primary entry point for the orchestrator to obtain a sender
/// without knowing the specific channel implementation.
pub fn create_sender(target: &ChannelTarget, http: reqwest::Client) -> Box<dyn ChannelSender> {
    match target.channel_type.as_str() {
        "telegram" => Box::new(TelegramSender {
            bot_token: target.token.clone(),
            chat_id: target.chat_id.clone(),
            http,
        }),
        "line" => Box::new(LineSender {
            access_token: target.token.clone(),
            user_id: target.chat_id.clone(),
            http,
        }),
        "discord" => Box::new(DiscordSender {
            bot_token: target.token.clone(),
            channel_id: target.chat_id.clone(),
            user_id: target.extra_id.clone().unwrap_or_default(),
            http,
        }),
        "slack" => Box::new(SlackSender {
            bot_token: target.token.clone(),
            channel_id: target.chat_id.clone(),
            user_id: target.extra_id.clone().unwrap_or_default(),
            http,
        }),
        "whatsapp" => Box::new(WhatsAppSender {
            access_token: target.token.clone(),
            phone_number_id: target.extra_id.clone().unwrap_or_default(),
            to: target.chat_id.clone(),
            http,
        }),
        "feishu" => Box::new(FeishuSender {
            access_token: target.token.clone(),
            chat_id: target.chat_id.clone(),
            http,
        }),
        "webchat" => {
            warn!("WebChat sender created via generic factory — use create_webchat_sender() with event_tx for full functionality");
            Box::new(WebChatSender {
                session_id: target.chat_id.clone(),
                event_tx: None,
            })
        }
        _ => {
            warn!(channel = %target.channel_type, "Unknown channel type, using NullSender");
            Box::new(NullSender)
        }
    }
}

// ===========================================================================
// 1. Telegram
// ===========================================================================

/// Telegram channel sender — Bot API `sendMessage` / `sendPhoto`.
pub struct TelegramSender {
    pub(crate) bot_token: String,
    pub(crate) chat_id: String,
    pub(crate) http: reqwest::Client,
}

#[async_trait]
impl ChannelSender for TelegramSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        self.http
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
                "parse_mode": "Markdown"
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Telegram sendMessage: {e}")))?;
        Ok(())
    }

    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        let url = format!("https://api.telegram.org/bot{}/sendPhoto", self.bot_token);
        let part = reqwest::multipart::Part::bytes(png_data.to_vec())
            .file_name("screenshot.png")
            .mime_str("image/png")
            .map_err(|e| ChannelSendError(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat_id.clone())
            .text("caption", caption.to_string())
            .part("photo", part);
        self.http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Telegram sendPhoto: {e}")))?;
        Ok(())
    }

    async fn request_confirmation(
        &self, prompt: &str, screenshot: Option<&[u8]>, _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        if let Some(png) = screenshot { self.send_photo(png, prompt).await?; }
        else { self.send_text(prompt).await?; }
        wait_for_confirmation(&self.chat_id, _timeout_secs).await
    }

    fn channel_type(&self) -> &'static str { "telegram" }
}

// ===========================================================================
// 2. LINE
// ===========================================================================

/// LINE channel sender — Messaging API `push message`.
pub struct LineSender {
    pub(crate) access_token: String,
    pub(crate) user_id: String,
    pub(crate) http: reqwest::Client,
}

#[async_trait]
impl ChannelSender for LineSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        self.http
            .post("https://api.line.me/v2/bot/message/push")
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({
                "to": self.user_id,
                "messages": [{"type": "text", "text": text}]
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("LINE push: {e}")))?;
        Ok(())
    }

    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        // LINE Blob Upload API: upload image content → get message content for sending
        // Step 1: Request upload endpoint
        let req_resp = self.http
            .post("https://api-data.line.me/v2/bot/message/content/upload")
            .bearer_auth(&self.access_token)
            .header("Content-Type", "image/png")
            .body(png_data.to_vec())
            .send()
            .await;

        match req_resp {
            Ok(resp) if resp.status().is_success() => {
                // Upload succeeded — send image via the response content token
                // LINE's audienceMatch upload returns a content provider URL
                // For simplicity, use the originalContentUrl pattern
                let resp_json: serde_json::Value = resp.json().await.unwrap_or_default();
                let content_url = resp_json["contentUrl"].as_str().unwrap_or("");

                if !content_url.is_empty() {
                    // Send as image message with the uploaded URL
                    self.http
                        .post("https://api.line.me/v2/bot/message/push")
                        .bearer_auth(&self.access_token)
                        .json(&serde_json::json!({
                            "to": self.user_id,
                            "messages": [{
                                "type": "image",
                                "originalContentUrl": content_url,
                                "previewImageUrl": content_url,
                            }]
                        }))
                        .send()
                        .await
                        .map_err(|e| ChannelSendError(format!("LINE sendImage: {e}")))?;

                    // Send caption as follow-up text
                    if !caption.is_empty() {
                        self.send_text(caption).await?;
                    }
                    return Ok(());
                }
            }
            _ => {
                // Blob upload not available — fall back to base64 in Flex Message
            }
        }

        // Fallback: Blob upload not available — send text notification
        let msg = format!("{caption}\n(📸 截圖已擷取，共 {} KB — 需設定 LINE Blob Upload API 才能顯示圖片)", png_data.len() / 1024);
        self.send_text(&msg).await
    }

    async fn request_confirmation(
        &self, prompt: &str, screenshot: Option<&[u8]>, timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        // Send a Confirm Template message via LINE
        let confirm_msg = serde_json::json!({
            "type": "template",
            "altText": prompt,
            "template": {
                "type": "confirm",
                "text": prompt,
                "actions": [
                    {"type": "message", "label": "確認", "text": "確認"},
                    {"type": "message", "label": "取消", "text": "取消"},
                ]
            }
        });

        self.http
            .post("https://api.line.me/v2/bot/message/push")
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({
                "to": self.user_id,
                "messages": [confirm_msg]
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("LINE confirm: {e}")))?;

        if let Some(png) = screenshot {
            self.send_photo(png, "").await?;
        }

        // Wait for the user's reply via the global confirmation channel
        wait_for_confirmation(&self.user_id, timeout_secs).await
    }

    fn channel_type(&self) -> &'static str { "line" }
}

// ===========================================================================
// 3. Discord
// ===========================================================================

/// Discord channel sender — REST API `Create Message` with file attachment.
pub struct DiscordSender {
    pub(crate) bot_token: String,
    pub(crate) channel_id: String,
    /// The requesting user's Discord ID (for confirmation scoping).
    pub(crate) user_id: String,
    pub(crate) http: reqwest::Client,
}

#[async_trait]
impl ChannelSender for DiscordSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        let url = format!("https://discord.com/api/v10/channels/{}/messages", self.channel_id);
        self.http
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&serde_json::json!({"content": text}))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Discord send: {e}")))?;
        Ok(())
    }

    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        let url = format!("https://discord.com/api/v10/channels/{}/messages", self.channel_id);
        let file_part = reqwest::multipart::Part::bytes(png_data.to_vec())
            .file_name("screenshot.png")
            .mime_str("image/png")
            .map_err(|e| ChannelSendError(e.to_string()))?;
        let payload = serde_json::json!({"content": caption}).to_string();
        let payload_part = reqwest::multipart::Part::text(payload)
            .mime_str("application/json")
            .map_err(|e| ChannelSendError(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .part("payload_json", payload_part)
            .part("files[0]", file_part);
        self.http
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Discord sendPhoto: {e}")))?;
        Ok(())
    }

    async fn request_confirmation(
        &self, prompt: &str, screenshot: Option<&[u8]>, _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        if let Some(png) = screenshot { self.send_photo(png, prompt).await?; }
        else { self.send_text(prompt).await?; }
        // SEC: Use user_id (not channel_id) to prevent other channel members from approving
        let confirm_key = if self.user_id.is_empty() { &self.channel_id } else { &self.user_id };
        wait_for_confirmation(confirm_key, _timeout_secs).await
    }

    fn channel_type(&self) -> &'static str { "discord" }
}

// ===========================================================================
// 4. Slack
// ===========================================================================

/// Slack channel sender — Web API `chat.postMessage` / `files.uploadV2`.
pub struct SlackSender {
    pub(crate) bot_token: String,
    pub(crate) channel_id: String,
    /// The requesting user's Slack ID (for confirmation scoping).
    pub(crate) user_id: String,
    pub(crate) http: reqwest::Client,
}

#[async_trait]
impl ChannelSender for SlackSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        self.http
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&serde_json::json!({
                "channel": self.channel_id,
                "text": text
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Slack postMessage: {e}")))?;
        Ok(())
    }

    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        // Slack files.uploadV2: get upload URL → PUT file → complete upload
        // Step 1: Get upload URL
        let get_url_resp = self.http
            .post("https://slack.com/api/files.getUploadURLExternal")
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&serde_json::json!({
                "filename": "screenshot.png",
                "length": png_data.len(),
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Slack getUploadURL: {e}")))?;

        let resp_json: serde_json::Value = get_url_resp
            .json()
            .await
            .map_err(|e| ChannelSendError(format!("Slack getUploadURL parse: {e}")))?;

        let upload_url = resp_json["upload_url"]
            .as_str()
            .ok_or_else(|| ChannelSendError("Slack: no upload_url in response".into()))?;
        let file_id = resp_json["file_id"]
            .as_str()
            .ok_or_else(|| ChannelSendError("Slack: no file_id in response".into()))?;

        // SEC: Validate upload URL domain to prevent SSRF
        if !upload_url.starts_with("https://files.slack.com/") {
            return Err(ChannelSendError(format!(
                "Slack upload URL domain mismatch (possible SSRF): {upload_url}"
            )));
        }

        // Step 2: Upload file
        self.http
            .put(upload_url)
            .body(png_data.to_vec())
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Slack file upload: {e}")))?;

        // Step 3: Complete upload with channel share
        self.http
            .post("https://slack.com/api/files.completeUploadExternal")
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&serde_json::json!({
                "files": [{"id": file_id, "title": caption}],
                "channel_id": self.channel_id,
                "initial_comment": caption,
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Slack completeUpload: {e}")))?;

        Ok(())
    }

    async fn request_confirmation(
        &self, prompt: &str, screenshot: Option<&[u8]>, _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        if let Some(png) = screenshot { self.send_photo(png, prompt).await?; }
        else { self.send_text(prompt).await?; }
        // SEC: Use user_id when available to prevent other channel members from approving
        let confirm_key = if self.user_id.is_empty() { &self.channel_id } else { &self.user_id };
        wait_for_confirmation(confirm_key, _timeout_secs).await
    }

    fn channel_type(&self) -> &'static str { "slack" }
}

// ===========================================================================
// 5. WhatsApp
// ===========================================================================

/// WhatsApp channel sender — Cloud API (Meta Business Platform).
pub struct WhatsAppSender {
    pub(crate) access_token: String,
    pub(crate) phone_number_id: String,
    pub(crate) to: String,
    pub(crate) http: reqwest::Client,
}

#[async_trait]
impl ChannelSender for WhatsAppSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        let url = format!(
            "https://graph.facebook.com/v20.0/{}/messages",
            self.phone_number_id
        );
        self.http
            .post(&url)
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({
                "messaging_product": "whatsapp",
                "to": self.to,
                "type": "text",
                "text": {"body": text}
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("WhatsApp send: {e}")))?;
        Ok(())
    }

    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        // Step 1: Upload media to WhatsApp
        let upload_url = format!(
            "https://graph.facebook.com/v20.0/{}/media",
            self.phone_number_id
        );
        let file_part = reqwest::multipart::Part::bytes(png_data.to_vec())
            .file_name("screenshot.png")
            .mime_str("image/png")
            .map_err(|e| ChannelSendError(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .text("messaging_product", "whatsapp")
            .text("type", "image/png")
            .part("file", file_part);

        let upload_resp = self.http
            .post(&upload_url)
            .bearer_auth(&self.access_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("WhatsApp media upload: {e}")))?;

        let resp_json: serde_json::Value = upload_resp
            .json()
            .await
            .map_err(|e| ChannelSendError(format!("WhatsApp upload parse: {e}")))?;

        let media_id = resp_json["id"]
            .as_str()
            .ok_or_else(|| ChannelSendError("WhatsApp: no media id".into()))?;

        // Step 2: Send image message with media_id
        let msg_url = format!(
            "https://graph.facebook.com/v20.0/{}/messages",
            self.phone_number_id
        );
        self.http
            .post(&msg_url)
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({
                "messaging_product": "whatsapp",
                "to": self.to,
                "type": "image",
                "image": {
                    "id": media_id,
                    "caption": caption,
                }
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("WhatsApp sendImage: {e}")))?;

        Ok(())
    }

    async fn request_confirmation(
        &self, prompt: &str, screenshot: Option<&[u8]>, _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        if let Some(png) = screenshot { self.send_photo(png, prompt).await?; }
        else { self.send_text(prompt).await?; }
        wait_for_confirmation(&self.to, _timeout_secs).await
    }

    fn channel_type(&self) -> &'static str { "whatsapp" }
}

// ===========================================================================
// 6. Feishu (Lark)
// ===========================================================================

/// Feishu channel sender — Open API `im/v1/messages`.
pub struct FeishuSender {
    pub(crate) access_token: String,
    pub(crate) chat_id: String,
    pub(crate) http: reqwest::Client,
}

#[async_trait]
impl ChannelSender for FeishuSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        self.http
            .post("https://open.feishu.cn/open-apis/im/v1/messages")
            .bearer_auth(&self.access_token)
            .query(&[("receive_id_type", "chat_id")])
            .json(&serde_json::json!({
                "receive_id": self.chat_id,
                "msg_type": "text",
                "content": serde_json::json!({"text": text}).to_string(),
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Feishu send: {e}")))?;
        Ok(())
    }

    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        // Step 1: Upload image to Feishu
        let file_part = reqwest::multipart::Part::bytes(png_data.to_vec())
            .file_name("screenshot.png")
            .mime_str("image/png")
            .map_err(|e| ChannelSendError(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part("image", file_part);

        let upload_resp = self.http
            .post("https://open.feishu.cn/open-apis/im/v1/images")
            .bearer_auth(&self.access_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Feishu image upload: {e}")))?;

        let resp_json: serde_json::Value = upload_resp
            .json()
            .await
            .map_err(|e| ChannelSendError(format!("Feishu upload parse: {e}")))?;

        let image_key = resp_json["data"]["image_key"]
            .as_str()
            .ok_or_else(|| ChannelSendError("Feishu: no image_key".into()))?;

        // Step 2: Send image message
        self.http
            .post("https://open.feishu.cn/open-apis/im/v1/messages")
            .bearer_auth(&self.access_token)
            .query(&[("receive_id_type", "chat_id")])
            .json(&serde_json::json!({
                "receive_id": self.chat_id,
                "msg_type": "image",
                "content": serde_json::json!({"image_key": image_key}).to_string(),
            }))
            .send()
            .await
            .map_err(|e| ChannelSendError(format!("Feishu sendImage: {e}")))?;

        // Send caption as follow-up text
        if !caption.is_empty() {
            self.send_text(caption).await?;
        }

        Ok(())
    }

    async fn request_confirmation(
        &self, prompt: &str, screenshot: Option<&[u8]>, _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        if let Some(png) = screenshot { self.send_photo(png, prompt).await?; }
        else { self.send_text(prompt).await?; }
        wait_for_confirmation(&self.chat_id, _timeout_secs).await
    }

    fn channel_type(&self) -> &'static str { "feishu" }
}

// ===========================================================================
// 7. WebChat (WebSocket)
// ===========================================================================

/// WebChat sender — sends JSON messages over the WebSocket broadcast channel.
///
/// Uses the gateway's `event_tx` broadcast sender to push messages to the
/// connected WebSocket client.
///
/// Use `create_webchat_sender()` (not the generic `create_sender()`) to get
/// a fully functional instance with the broadcast channel attached.
pub struct WebChatSender {
    pub(crate) session_id: String,
    /// Broadcast sender — must be provided for messages to be delivered.
    pub(crate) event_tx: Option<tokio::sync::broadcast::Sender<String>>,
}

/// Create a WebChat sender with the broadcast channel attached.
///
/// This is the preferred way to create a WebChat sender. The generic
/// `create_sender()` factory cannot pass the broadcast tx.
pub fn create_webchat_sender(
    session_id: String,
    event_tx: tokio::sync::broadcast::Sender<String>,
) -> Box<dyn ChannelSender> {
    Box::new(WebChatSender {
        session_id,
        event_tx: Some(event_tx),
    })
}

#[async_trait]
impl ChannelSender for WebChatSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        let msg = serde_json::json!({
            "type": "computer_use_text",
            "session_id": self.session_id,
            "text": text,
        });
        if let Some(ref tx) = self.event_tx {
            tx.send(msg.to_string()).ok();
        }
        Ok(())
    }

    async fn send_photo(&self, png_data: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(png_data);
        let msg = serde_json::json!({
            "type": "computer_use_photo",
            "session_id": self.session_id,
            "image_base64": b64,
            "caption": caption,
        });
        if let Some(ref tx) = self.event_tx {
            tx.send(msg.to_string()).ok();
        }
        Ok(())
    }

    async fn request_confirmation(
        &self, prompt: &str, screenshot: Option<&[u8]>, _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        if let Some(png) = screenshot { self.send_photo(png, prompt).await?; }
        else { self.send_text(prompt).await?; }
        wait_for_confirmation(&self.session_id, _timeout_secs).await
    }

    fn channel_type(&self) -> &'static str { "webchat" }
}

// ===========================================================================
// Null sender (testing / fallback)
// ===========================================================================

/// No-op sender for non-channel contexts.
///
/// SECURITY: `request_confirmation` returns `false` (deny-by-default) to prevent
/// high-risk operations from being silently auto-approved when no real channel
/// is connected.
pub struct NullSender;

#[async_trait]
impl ChannelSender for NullSender {
    async fn send_text(&self, _text: &str) -> Result<(), ChannelSendError> { Ok(()) }
    async fn send_photo(&self, _png_data: &[u8], _caption: &str) -> Result<(), ChannelSendError> { Ok(()) }
    async fn request_confirmation(
        &self, _prompt: &str, _screenshot: Option<&[u8]>, _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        // Deny-by-default: no real channel means no one to confirm
        Ok(false)
    }
    fn channel_type(&self) -> &'static str { "null" }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_sender_deny_by_default() {
        let sender = NullSender;
        assert!(sender.send_text("hello").await.is_ok());
        assert!(sender.send_photo(b"png", "cap").await.is_ok());
        // NullSender denies confirmations by default (security: no channel = no approval)
        assert!(!sender.request_confirmation("ok?", None, 60).await.unwrap());
        assert_eq!(sender.channel_type(), "null");
    }

    #[test]
    fn factory_creates_telegram() {
        let target = ChannelTarget {
            channel_type: "telegram".into(),
            chat_id: "123".into(),
            token: "bot-token".into(),
            extra_id: None,
        };
        let sender = create_sender(&target, reqwest::Client::new());
        assert_eq!(sender.channel_type(), "telegram");
    }

    #[test]
    fn factory_creates_slack() {
        let target = ChannelTarget {
            channel_type: "slack".into(),
            chat_id: "C123".into(),
            token: "xoxb-token".into(),
            extra_id: None,
        };
        let sender = create_sender(&target, reqwest::Client::new());
        assert_eq!(sender.channel_type(), "slack");
    }

    #[test]
    fn factory_creates_whatsapp() {
        let target = ChannelTarget {
            channel_type: "whatsapp".into(),
            chat_id: "+886912345678".into(),
            token: "wa-token".into(),
            extra_id: Some("phone_number_id_123".into()),
        };
        let sender = create_sender(&target, reqwest::Client::new());
        assert_eq!(sender.channel_type(), "whatsapp");
    }

    #[test]
    fn factory_creates_feishu() {
        let target = ChannelTarget {
            channel_type: "feishu".into(),
            chat_id: "oc_xxx".into(),
            token: "t-xxx".into(),
            extra_id: None,
        };
        let sender = create_sender(&target, reqwest::Client::new());
        assert_eq!(sender.channel_type(), "feishu");
    }

    #[test]
    fn factory_creates_webchat() {
        let target = ChannelTarget {
            channel_type: "webchat".into(),
            chat_id: "session-123".into(),
            token: String::new(),
            extra_id: None,
        };
        let sender = create_sender(&target, reqwest::Client::new());
        assert_eq!(sender.channel_type(), "webchat");
    }

    #[test]
    fn confirmation_reply_detection() {
        assert!(super::is_confirmation_reply("yes"));
        assert!(super::is_confirmation_reply("Y"));
        assert!(super::is_confirmation_reply("好"));
        assert!(super::is_confirmation_reply("確認"));
        assert!(super::is_confirmation_reply("繼續"));
        assert!(super::is_confirmation_reply("はい"));
        assert!(!super::is_confirmation_reply("no"));
        assert!(!super::is_confirmation_reply("取消"));
        assert!(!super::is_confirmation_reply("hello"));
    }

    #[tokio::test]
    async fn confirmation_resolve_flow() {
        // Register a confirmation
        let (tx, rx) = tokio::sync::oneshot::channel();
        super::confirmation_registry()
            .lock()
            .await
            .insert("test-user".into(), tx);

        // Resolve it
        assert!(super::resolve_confirmation("test-user", "好").await);

        // Should have received true
        assert!(rx.await.unwrap());
    }

    #[tokio::test]
    async fn confirmation_timeout() {
        // Wait with very short timeout, no one resolves
        let result = super::wait_for_confirmation("nonexistent-user", 1).await;
        assert!(!result.unwrap()); // timeout = decline
    }

    #[test]
    fn webchat_sender_with_event_tx() {
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let sender = create_webchat_sender("session-42".into(), tx);
        assert_eq!(sender.channel_type(), "webchat");
    }

    #[test]
    fn factory_unknown_falls_back_to_null() {
        let target = ChannelTarget {
            channel_type: "unknown_channel".into(),
            chat_id: "x".into(),
            token: "t".into(),
            extra_id: None,
        };
        let sender = create_sender(&target, reqwest::Client::new());
        assert_eq!(sender.channel_type(), "null");
    }
}
