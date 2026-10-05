//! LINE Messaging API integration with webhook receiver.
//!
//! Mounts a `/webhook/line` POST endpoint on the Axum router to receive
//! messages from LINE, validates signatures, and sends replies.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use duduclaw_core::truncate_bytes;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{error, info, warn};

use crate::channel_format;
use crate::channel_reply::{
    ChannelStatusMap, ReplyContext, build_guarded_reply_for_agent, set_channel_connected,
};
use crate::channel_settings::keys;

mod ingress;
pub(crate) use ingress::durable_line_enabled;
use ingress::{drain_line_ingress, line_conversation, record_reply_failure};

const LINE_API: &str = "https://api.line.me/v2/bot";
fn line_provider_url(_token: &str, path: &str) -> String {
    let url = format!("{LINE_API}{path}");
    #[cfg(test)]
    return crate::test_channel_provider::url(_token, &url);
    #[cfg(not(test))]
    url
}

type HmacSha256 = Hmac<Sha256>;

// ── LINE API types ──────────────────────────────────────────

#[derive(Deserialize)]
struct LineWebhookBody {
    destination: Option<String>,
    events: Vec<LineEvent>,
}

#[derive(Deserialize)]
struct LineEvent {
    #[serde(rename = "webhookEventId")]
    webhook_event_id: Option<String>,
    #[serde(rename = "type")]
    event_type: String,
    #[serde(rename = "replyToken")]
    reply_token: Option<String>,
    source: Option<LineSource>,
    message: Option<LineMessage>,
    /// Present on `postback` events (quick-reply button presses).
    postback: Option<LinePostback>,
    /// When the event occurred, milliseconds since the epoch.
    #[serde(default)]
    timestamp: Option<i64>,
    /// `isRedelivery` is true on a webhook LINE delivered again.
    #[serde(rename = "deliveryContext", default)]
    delivery_context: Option<LineDeliveryContext>,
}

#[derive(Debug, Deserialize)]
struct LineDeliveryContext {
    #[serde(rename = "isRedelivery", default)]
    is_redelivery: bool,
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
    /// Present when the user replied-with-quote to an earlier message —
    /// WP1.6 matches it against recorded decision-card ids so a quoted
    /// 「同意」counts as pressing the card's button.
    #[serde(rename = "quotedMessageId")]
    quoted_message_id: Option<String>,
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
    /// Bot basic ID (e.g. `@abc1234`) — the stable handle behind the
    /// add-friend deep link. LINE may serialize with or without the leading
    /// `@`; callers normalize.
    #[serde(rename = "basicId")]
    basic_id: Option<String>,
}

/// Resolve the LINE OA add-friend link from the configured channel token
/// (WP1.1 LINE QR onboarding). Returns `(add_friend_url, basic_id, display_name)`
/// with `basic_id` normalized to the `@xxx` form. Error strings are zh-TW and
/// user-facing (dashboard RPC surface).
pub async fn fetch_line_add_friend_info(
    home_dir: &Path,
) -> Result<(String, String, Option<String>), String> {
    let (token, _secret) = read_line_config(home_dir)
        .await
        .ok_or_else(|| "尚未設定 LINE 通道（請先在通道頁新增 LINE）".to_string())?;
    if token.is_empty() {
        return Err("尚未設定 LINE 通道（請先在通道頁新增 LINE）".to_string());
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client 建立失敗：{e}"))?;
    let resp = http
        .get(format!("{LINE_API}/info"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("無法連線 LINE API：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("LINE token 無效（HTTP {}）", resp.status()));
    }
    let info: LineBotInfo = resp
        .json()
        .await
        .map_err(|e| format!("LINE API 回應無法解析：{e}"))?;
    let bare = info
        .basic_id
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_start_matches('@')
        .to_string();
    if bare.is_empty() {
        return Err("LINE API 未回傳 basic ID".to_string());
    }
    let basic = format!("@{bare}");
    Ok((
        format!("https://line.me/R/ti/p/{basic}"),
        basic,
        info.display_name,
    ))
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
    ingress: Option<Arc<crate::channel_ingress::IngressStore>>,
}

// ── Public API ──────────────────────────────────────────────

impl LineState {
    pub(crate) fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Build a `LineState` sharing the same `channel_status`/`event_tx` as
    /// the rest of the gateway (both are cloned off `ctx`, which is itself
    /// an `Arc` — cloning the fields shares the underlying state, it does
    /// not fork it). Used by both `start_line_bot` (the direct HTTP
    /// `/webhook/line` route) and the box-side relay client
    /// (`relay_client.rs`, WP-E2), so a webhook delivered via either
    /// transport goes through byte-identical verification + dispatch.
    pub(crate) fn new(home_dir: &Path, ctx: Arc<ReplyContext>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let ingress = crate::channel_ingress::IngressStore::shared(home_dir)
            .map_err(|_| error!("LINE ingress store unavailable"))
            .ok();
        let state = Self {
            ingress,
            home_dir: home_dir.to_path_buf(),
            channel_status: ctx.channel_status.clone(),
            event_tx: ctx.event_tx.clone(),
            http,
            ctx,
        };
        if tokio::runtime::Handle::try_current().is_ok() && ingress::claim_worker_home(&state) {
            let worker_state = state.clone();
            tokio::spawn(async move {
                drain_line_ingress(worker_state).await;
            });
        }
        state
    }
}

/// Mount the LINE webhook endpoint. The route is ALWAYS mounted (even when LINE
/// is not yet configured) and the handler reads the token/secret from config on
/// every request — so configuring or changing LINE in the dashboard takes effect
/// immediately, with no gateway restart. A best-effort token check runs now to
/// set the initial channel status.
pub async fn start_line_bot(home_dir: &Path, ctx: Arc<ReplyContext>) -> Router {
    let state = LineState::new(home_dir, ctx.clone());

    // Initial status (best-effort) so the dashboard reflects an already-configured
    // LINE channel without waiting for the first webhook.
    verify_line_status(home_dir, &state.http, &ctx.channel_status, &ctx.event_tx).await;

    info!(
        "   LINE webhook endpoint mounted: /webhook/line (token read per request — no restart needed on config change)"
    );
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
            set_channel_connected(
                channel_status,
                "line",
                false,
                Some("not configured".into()),
                Some(event_tx),
            )
            .await;
            return;
        }
    };
    if token.is_empty() {
        set_channel_connected(
            channel_status,
            "line",
            false,
            Some("not configured".into()),
            Some(event_tx),
        )
        .await;
        return;
    }
    // HS2: an empty secret would make signature verification accept forged
    // requests; surface it as not-connected so the operator fixes it.
    if secret.is_empty() {
        set_channel_connected(
            channel_status,
            "line",
            false,
            Some("channel secret missing".into()),
            Some(event_tx),
        )
        .await;
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
            set_channel_connected(
                channel_status,
                "line",
                false,
                Some(e.to_string()),
                Some(event_tx),
            )
            .await;
        }
    }
}

// ── Webhook handler ─────────────────────────────────────────

async fn line_webhook_handler(
    State(state): State<LineState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    handle_line_webhook(state, &headers, body).await
}

/// WP-E2: the shared verify + dispatch entry point used by both the direct
/// `/webhook/line` HTTP route (above) and the box-side relay client
/// (`relay_client.rs`) when it receives a `channel = "line"` `HookFrame`
/// from `duduclaw-relay`. LINE signature verification and event handling
/// exist in exactly one place regardless of which transport delivered the
/// webhook body — the relay path never re-implements or bypasses it.
///
/// The returned `StatusCode` matters to the direct HTTP route (LINE reads
/// it); the relay path only needs to know success (`is_success()`) vs
/// failure to classify the frame for its own `relay_frames_total` metric —
/// see `relay_client::inject_line_hook`.
pub(crate) async fn handle_line_webhook(
    state: LineState,
    headers: &HeaderMap,
    body: Bytes,
) -> StatusCode {
    // One read of config.toml for the credentials and the stop switch; the
    // credentials are re-read per request so config changes apply live.
    // Before the 200 only what verification and the durable write need is
    // checked (review I-MEDIUM-7); the route/authority snapshot is stored
    // right after the commit (`ingress::spawn_snapshots`).
    let config = match tokio::fs::read_to_string(state.home_dir.join("config.toml")).await {
        Ok(text) => match text.parse::<toml::Table>() {
            Ok(table) => table,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
        },
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
    };
    let (verified_token, secret) =
        match line_credentials_for_table(&state.home_dir, &config).await {
            Some((t, s)) if !t.is_empty() && !s.is_empty() => (t, s),
            // No credentials means no authenticated durable acceptance.
            _ => return StatusCode::SERVICE_UNAVAILABLE,
        };

    // Validate signature
    let signature = match headers
        .get("x-line-signature")
        .and_then(|v| v.to_str().ok())
    {
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
        Err(_) => {
            warn!("LINE webhook: invalid event envelope");
            return StatusCode::BAD_REQUEST;
        }
    };

    // Update last_event timestamp on each webhook call
    set_channel_connected(
        &state.channel_status,
        "line",
        true,
        None,
        Some(&state.event_tx),
    )
    .await;

    if !crate::channel_ingress::config::IngressConfig::from_table(&config).line_enabled {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    let Some(store) = &state.ingress else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let raw: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let destination = match webhook.destination.as_deref() {
        Some(id) if !id.is_empty() => id,
        _ if webhook.events.is_empty() => "verification",
        _ => return StatusCode::BAD_REQUEST,
    };
    // Computed once per envelope.
    let account = crate::channel_ingress::digest(&["line", destination]);
    let pending = crate::channel_ingress::pending_authorization(
        &crate::channel_ingress::digest(&[&verified_token, &secret]),
    );
    let mut accepted = Vec::new();
    for (event, raw_event) in webhook
        .events
        .iter()
        .zip(raw["events"].as_array().into_iter().flatten())
    {
        let Some(event_id) = event
            .webhook_event_id
            .as_deref()
            .filter(|id| !id.is_empty())
        else {
            // LINE guarantees webhookEventId. An unidentifiable event cannot be ACKed.
            return StatusCode::BAD_REQUEST;
        };
        let conversation = line_conversation(event);
        accepted.push(crate::channel_ingress::AcceptedEvent {
            decision_fastlane: line_decision_fastlane(event),
            decision_binding: line_decision_request_id(event).map(|request_id| serde_json::json!({
                "request_id": request_id,
                "context_hash": crate::channel_ingress::digest(&[
                    "line",
                    destination,
                    &conversation,
                    event.source.as_ref().and_then(|s| s.user_id.as_deref()).unwrap_or("")
                ])
            }).to_string()),
            event_id: event_id.to_string(),
            account: account.clone(),
            revision: crate::channel_ingress::PENDING_REVISION.to_string(),
            authorization_revision: pending.clone(),
            conversation,
            payload: serde_json::json!({"destination":destination,"event":raw_event}).to_string(),
        });
    }
    if store
        .append(&accepted, chrono::Utc::now().timestamp())
        .await
        .is_err()
    {
        error!("LINE durable append failed; webhook not acknowledged");
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    let ids = accepted
        .iter()
        .map(|e| crate::channel_ingress::digest(&["line", &e.account, &e.event_id]))
        .collect();
    ingress::spawn_snapshots(state.clone(), ids);
    StatusCode::OK
}

fn line_decision_request_id(event: &LineEvent) -> Option<String> {
    if event.event_type == "postback" {
        let action = event
            .postback
            .as_ref()
            .and_then(|p| p.data.as_deref())
            .and_then(crate::decision_action::parse)?;
        return (action.source == crate::decision_action::DecisionSource::Approval
            && uuid::Uuid::parse_str(&action.id).is_ok())
        .then_some(action.id);
    }
    let message = event.message.as_ref().filter(|m| m.msg_type == "text")?;
    // Same definition every adapter uses (F4): verb + complete request id.
    crate::channel_decision_route::parse_strict_decision(message.text.as_deref().unwrap_or(""))
        .map(|command| command.id.to_owned())
}
fn line_decision_fastlane(event: &LineEvent) -> bool {
    line_decision_request_id(event).is_some()
}

async fn process_line_decision(event: LineEvent, state: &LineState, token: &str) {
    let command = if event.event_type == "postback" {
        let Some(action) = event
            .postback
            .as_ref()
            .and_then(|p| p.data.as_deref())
            .and_then(crate::decision_action::parse)
        else {
            return;
        };
        format!(
            "{} {}",
            if action.approve() { "approve" } else { "deny" },
            action.id
        )
    } else {
        event
            .message
            .as_ref()
            .and_then(|m| m.text.clone())
            .unwrap_or_default()
    };
    let source = event.source.as_ref();
    let scope = crate::decision_notify::DecisionAccessScope {
        channel_id: source.and_then(|s| s.group_id.as_deref().or(s.room_id.as_deref())),
        guild_id: None,
        session_id: None,
    };
    let result = match crate::approval::CURRENT_DECISION_CONTEXT.try_with(Clone::clone) {
        Ok(Some(context)) => {
            if event.event_type == "postback" {
                crate::decision_notify::route_verified_bound_press(
                    &state.ctx,
                    &context,
                    event
                        .postback
                        .as_ref()
                        .and_then(|p| p.data.as_deref())
                        .unwrap_or(""),
                    scope,
                )
                .await
            } else {
                crate::decision_notify::route_trusted_decision_fastlane_with_scope(
                    &state.ctx, &context, &command, scope,
                )
                .await
            }
        }
        _ => Some(Err(crate::channel_decision_route::DECISION_REFUSED.into())),
    };
    #[cfg(test)]
    ingress::decision_commit_test_pause(state, event.webhook_event_id.as_deref().unwrap_or(""))
        .await;
    let message = match result {
        Some(Ok(answer)) => answer,
        Some(Err(error)) => format!("⚠️ {error}"),
        None => {
            record_reply_failure("decision_command_invalid");
            return;
        }
    };
    if let Some(reply_token) = event.reply_token.as_deref() {
        let _ = send_reply_rich(
            &state.http,
            token,
            reply_token,
            vec![serde_json::json!({"type":"text","text":message})],
        )
        .await;
    }
}

async fn process_line_events(
    events: Vec<LineEvent>,
    state: &LineState,
    token: &str,
    agent_id: &str,
    _account_id: &str,
) {
    async {
        for event in events {
            // ── Quick-reply button presses (postback events) ──
            if event.event_type == "postback" {
                handle_postback(&event, &state, &token).await;
                continue;
            }

            if event.event_type != "message" {
                continue;
            }

            let Some(msg) = &event.message else { continue };
            let Some(reply_token) = &event.reply_token else {
                continue;
            };

            // Skip unsupported message types (e.g., location, sticker)
            let supported_types = ["text", "image", "video", "audio", "file"];
            if !supported_types.contains(&msg.msg_type.as_str()) {
                continue;
            }

            {
                let source = &event.source;
                let source_type = source
                    .as_ref()
                    .and_then(|s| s.source_type.as_deref())
                    .unwrap_or("user");
                let is_group = source_type == "group" || source_type == "room";
                let scope_id = source
                    .as_ref()
                    .and_then(|s| s.group_id.as_deref().or(s.room_id.as_deref()))
                    .unwrap_or("global");

                // ── Channel whitelist (group chats only) ──
                if is_group
                    && !state
                        .ctx
                        .channel_settings
                        .is_channel_allowed("line", "global", scope_id)
                        .await
                {
                    continue;
                }

                // WP1.6 (ecosystem): quoting a decision card with a bare verb
                // (「同意」/「拒絕」…) counts as pressing its button — LINE watch
                // and quick-reply surfaces carry `quotedMessageId` but no
                // postback. Quoting the card IS addressing the bot, so this runs
                // before the mention-only gate. Same dispatch (auth +
                // accounting) as a physical press; everything else falls
                // through to normal chat.
                if msg.msg_type == "text" {
                    if let (Some(qid), Some(text)) =
                        (msg.quoted_message_id.as_deref(), msg.text.as_deref())
                    {
                        let sender_for_decision = event
                            .source
                            .as_ref()
                            .and_then(|s| s.user_id.as_deref())
                            .unwrap_or("unknown");
                        let card_chat = if is_group {
                            scope_id
                        } else {
                            sender_for_decision
                        };
                        if let Some(outcome) = crate::decision_text::route_text_reply(
                            &state.home_dir,
                            "line",
                            sender_for_decision,
                            card_chat,
                            qid,
                            text,
                        )
                        .await
                        {
                            let ack = match outcome {
                                Ok(m) => m,
                                Err(e) => format!("⚠ {e}"),
                            };
                            let _ = send_reply_rich(
                                &state.http,
                                &token,
                                reply_token,
                                vec![serde_json::json!({ "type": "text", "text": ack })],
                            )
                            .await;
                            continue;
                        }
                    }
                }

                // ── Mention-only mode (group chats only, LINE has no native @mention) ──
                let mention_only = state
                    .ctx
                    .channel_settings
                    .get_bool("line", scope_id, keys::MENTION_ONLY, false)
                    .await;
                if is_group && mention_only {
                    continue;
                }

                let sender = source
                    .as_ref()
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
                            crate::media::download_url(
                                &state.http,
                                url,
                                None,
                                crate::media::MAX_FILE_SIZE as usize,
                            )
                            .await
                            .ok()
                        } else {
                            // LINE-hosted — download via Content API
                            download_line_content(&state.http, &token, msg_id)
                                .await
                                .ok()
                        };

                        if let Some(data) = content_data {
                            // Transcribe voice/audio messages to text.
                            if msg.msg_type == "audio" {
                                match crate::stt::transcribe_channel_audio(
                                    &state.ctx.home_dir,
                                    &data,
                                    Some("zh"),
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
                            // WP1.3: land under the resolved agent's dir.
                            let attach_base = crate::channel_reply::resolve_attachment_base(
                                state.ctx.as_ref(),
                                Some(agent_id),
                            )
                            .await;
                            match crate::media::save_attachment_in_base(&attach_base, &data, &fname)
                                .await
                            {
                                Ok(path) => {
                                    attachment_lines.push(crate::media::format_attachment_ref(
                                        &mt, &fname, &path,
                                    ));
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

                // ── Chat commands (/status, /new, /handoff, /undo, /rollback, …) ──
                // Intercepted before the AI pipeline — zero LLM cost. Mirrors slack.rs.
                if crate::chat_commands::is_command(&input_text) {
                    if let Some(cmd) = crate::chat_commands::parse_command(&input_text, None) {
                        let session_id = if let Some(gid) =
                            source.as_ref().and_then(|s| s.group_id.as_deref())
                        {
                            format!("line:{gid}")
                        } else if let Some(rid) = source.as_ref().and_then(|s| s.room_id.as_deref())
                        {
                            format!("line:{rid}")
                        } else {
                            format!("line:{sender}")
                        };
                        // Central access gate (pairing / allowlist / blocklist) —
                        // same enforcement the AI path applies; commands must not
                        // bypass it.
                        if let Some(gate_reply) = crate::channel_reply::check_user_access_gate(
                            &state.ctx,
                            &session_id,
                            sender,
                            &input_text,
                        )
                        .await
                        {
                            if !gate_reply.is_empty() {
                                let messages =
                                    vec![serde_json::json!({ "type": "text", "text": gate_reply })];
                                let _ = send_reply_rich(
                                    &state.http,
                                    &token,
                                    reply_token,
                                    messages.clone(),
                                )
                                .await;
                            }
                            continue; // blocked users are silently ignored
                        }
                        // Real per-channel admin status (fail-closed) — never hardcoded.
                        let is_admin = crate::channel_reply::is_channel_admin(
                            &state.ctx,
                            "line",
                            &[sender, &session_id],
                        )
                        .await;
                        let reply = crate::chat_commands::handle_command(
                            &cmd,
                            &state.ctx,
                            &session_id,
                            agent_id,
                            is_admin,
                            sender,
                        )
                        .await;
                        let messages = vec![serde_json::json!({ "type": "text", "text": reply })];
                        let _ = send_reply_rich(&state.http, &token, reply_token, messages.clone())
                            .await;
                        continue;
                    }
                }

                let on_progress = ingress::line_progress_callback(&state, &event, token);

                // Build session ID scoped to group/room or user DM
                let session_id =
                    if let Some(gid) = source.as_ref().and_then(|s| s.group_id.as_deref()) {
                        format!("line:{gid}")
                    } else if let Some(rid) = source.as_ref().and_then(|s| s.room_id.as_deref()) {
                        format!("line:{rid}")
                    } else {
                        format!("line:{sender}")
                    };

                #[cfg(test)]
                if let Some(job) =
                    crate::decision_notify::native_loop_fixture::take_job_for(token, &input_text)
                {
                    // Replace only the model turn, retaining the authenticated
                    // adapter scope, access gate and real CU approval/action.
                    if crate::channel_reply::check_user_access_gate(
                        &state.ctx,
                        &session_id,
                        sender,
                        &input_text,
                    )
                    .await
                    .is_none()
                    {
                        job.await;
                        let _ = send_reply_rich(
                            &state.http,
                            token,
                            reply_token,
                            vec![
                                serde_json::json!({"type":"text","text":"fixture turn completed"}),
                            ],
                        )
                        .await;
                    }
                    continue;
                }

                // Loading animation (LINE shows it in 1:1 chats only; the API
                // silently no-ops elsewhere). RAII guard stops the refresh loop.
                let loading_guard =
                    event
                        .source
                        .as_ref()
                        .and_then(|s| s.user_id.clone())
                        .map(|uid| {
                            crate::channel_typing::line_loading(
                                state.http.clone(),
                                token.to_string(),
                                uid,
                            )
                        });

                // `sender` falls back to a literal placeholder when the event
                // has no `source.userId`; it keeps that value everywhere it is
                // an addressing or logging key (the DM session id, the push
                // target, the revoked-reply notice), but it must never become
                // the CCR principal — every unidentified sender would hash to
                // one shared retrieval scope.
                // `reply_principal_for_sender` yields "" there, which turns
                // CCR off for the turn (fail-closed).
                let guarded = build_guarded_reply_for_agent(
                    &input_text,
                    &state.ctx,
                    agent_id,
                    &session_id,
                    crate::ccr_runtime::reply_principal_for_sender(sender),
                    on_progress,
                )
                .await;
                drop(loading_guard);

                if !guarded.still_valid().await {
                    send_ccr_revoked_reply(&state.http, &token, reply_token, sender).await;
                    continue;
                }

                // WP1.3: 📎DELIVER: — LINE has no bot file API, so the default
                // `send_document` degrades to a text notice (→ dashboard Files
                // panel) and the marker is stripped from the reply.
                // The notice joins the answer, so it goes out through the same
                // revalidated reply / late-reply path and receipt (I-MEDIUM-3).
                let reply = {
                    let notices = ingress::LineNoticeCollector::default();
                    let text = crate::channel_reply::deliver_documents_for_reply_guarded(
                        state.ctx.as_ref(),
                        None,
                        guarded.text.clone(),
                        &notices,
                        Some(&guarded),
                    )
                    .await;
                    notices.append_to(text)
                };

                if !guarded.still_valid().await {
                    send_ccr_revoked_reply(&state.http, &token, reply_token, sender).await;
                    continue;
                }

                // Guard: don't send empty replies
                if reply.trim().is_empty() {
                    warn!("LINE: reply is empty — skipping send for {sender}");
                    continue;
                }

                // Use Flex Message for long replies, plain text for short ones
                let agent_name = {
                    let reg = state.ctx.registry.read().await;
                    reg.main_agent()
                        .map(|a| a.config.agent.display_name.clone())
                };
                // M25: segment long replies. A single Flex bubble has tight text
                // limits, so an over-limit reply would be rejected and silently
                // dropped. `segment_line_reply` returns one Flex bubble for short
                // replies, or several plain-text messages (each within LINE's
                // 5000-char text limit, capped at 5 messages/request) for long ones.
                let mut messages = segment_line_reply(&reply, agent_name.as_deref());

                // Attach quick-reply buttons to the LAST message (LINE only shows
                // quickReply on the most recent message). Presses arrive as
                // `postback` events handled above. P1: the goal-intent
                // confirmation buttons when `reply` is the specific turn that
                // just appended the confirmation menu, otherwise the ordinary
                // conversation-control quick reply (unchanged from before P1).
                if let Some(last) = messages.last_mut() {
                    last["quickReply"] = if crate::goal_intent::reply_has_confirmation_menu(&reply)
                    {
                        crate::goal_intent::pending_button_nonce(&session_id)
                            .map(|nonce| channel_format::line_gintent_quick_reply(&nonce))
                            .unwrap_or_else(channel_format::line_quick_reply)
                    } else {
                        channel_format::line_quick_reply()
                    };
                }

                // Delivery (reply, or the late-reply Push per
                // `line_late_reply`) and its receipt: `ingress::deliver`.
                if !guarded.still_valid().await {
                    send_ccr_revoked_reply(&state.http, &token, reply_token, sender).await;
                    continue;
                }
                let _ = send_reply_rich(&state.http, &token, reply_token, messages).await;
            }
        }
    }
    .await
}

async fn send_ccr_revoked_reply(
    http: &reqwest::Client,
    token: &str,
    reply_token: &str,
    _sender: &str,
) {
    let messages = vec![serde_json::json!({
        "type": "text",
        "text": crate::channel_reply::CCR_DELIVERY_REFUSED_TEXT
    })];
    let _ = send_reply_rich(http, token, reply_token, messages.clone()).await;
}

// ── Helpers ─────────────────────────────────────────────────

/// Handle a `postback` event (quick-reply button press).
/// `data` format mirrors the Discord custom_id convention: `duduclaw:{action}`.
///
/// LINE has no message-edit API a postback can target (confirmed against the
/// Messaging API — no editable message id is ever exposed to a bot), so it
/// never had a persistent card to collapse in the first place: this reply IS
/// already the "append a one-line result" fallback the other channels only
/// fall back to on an edit miss/failure. `decision_card::channel_editable`
/// excludes "line" for the same reason — see its module doc.
async fn handle_postback(event: &LineEvent, state: &LineState, token: &str) {
    let data = event
        .postback
        .as_ref()
        .and_then(|p| p.data.as_deref())
        .unwrap_or("");
    let Some(reply_token) = &event.reply_token else {
        return;
    };
    let source = &event.source;
    let sender = source
        .as_ref()
        .and_then(|s| s.user_id.as_deref())
        .unwrap_or("unknown");

    info!("LINE postback received");

    // Decision buttons — every "a human must decide this" card, whichever
    // store backs it. `None` ⇒ not a decision button, fall through to the
    // other postback actions below.
    if sender != "unknown" {
        if let Some(result) = match crate::approval::CURRENT_DECISION_CONTEXT.try_with(Clone::clone)
        {
            Ok(Some(context)) => {
                crate::decision_notify::route_bound_press(&state.ctx.home_dir, &context, data).await
            }
            _ => {
                crate::decision_notify::route_press(&state.ctx.home_dir, "line", sender, data).await
            }
        } {
            let answer = match result {
                Ok(msg) => msg,
                Err(msg) => format!("⚠️ {msg}"),
            };
            let messages = vec![serde_json::json!({ "type": "text", "text": answer })];
            let _ = send_reply_rich(&state.http, token, reply_token, messages.clone()).await;
            return;
        }
    }

    // Goal-intent confirmation buttons (P1) — a separate, deliberately
    // UN-authorized codec from `decision_action` above (see
    // `channel_format`'s "Goal-intent confirmation" module doc): consuming
    // it needs no pressing-user identity, only the single-use nonce. This
    // whole webhook handler already runs in a `tokio::spawn`ed detached task
    // (see the caller: "LINE times out a slow webhook response… blocking the
    // 200 on a multi-second model reply gets the handler future… cancelled"),
    // so awaiting `handle_gintent_button`'s possible plan-first LLM call here
    // is safe; an expired reply token is recorded as a delivery failure.
    if sender != "unknown" {
        if let Some((choice, nonce)) = crate::goal_intent::parse_gintent_action(data) {
            let outcome =
                crate::goal_intent::handle_gintent_button(&state.ctx, choice, &nonce).await;
            let messages = vec![serde_json::json!({ "type": "text", "text": outcome })];
            let _ = send_reply_rich(&state.http, token, reply_token, messages.clone()).await;
            return;
        }
    }

    let answer = match data {
        "duduclaw:new_session" => {
            // Session id scoped the same way as the message path.
            let session_id = if let Some(gid) = source.as_ref().and_then(|s| s.group_id.as_deref())
            {
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
    let _ = send_reply_rich(&state.http, token, reply_token, messages.clone()).await;
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
    )
    .await
}

/// Send the answer of an ingress run: the Reply API while the reply token
/// is fresh, otherwise per `[channel_ingress] line_late_reply` (Push to the
/// same conversation, or a recorded expiry). Revalidates first; the result
/// goes into the run's receipt. See `ingress::delivery`.
///
/// Returns `true` when LINE accepted the message.
async fn send_reply_rich(
    http: &reqwest::Client,
    token: &str,
    reply_token: &str,
    messages: Vec<serde_json::Value>,
) -> bool {
    ingress::deliver(http, token, reply_token, messages).await
}

/// Whether a `ProgressEvent` should be pushed to a LINE user as a message.
///
/// WP-10C: LINE was the one channel (of the eleven) whose progress callback
/// had no event-type filter — `Step` / `ModelInfo` are dashboard-only
/// signals that render as an empty string via [`crate::channel_reply::ProgressEvent::to_display`],
/// and pushing an empty `text` message would either be a wasted LINE Push
/// API quota hit or an API rejection. Mirrors the explicit variant match
/// already used by the telegram/slack progress callbacks, plus a
/// defense-in-depth empty-string check so a future dashboard-only variant
/// added without updating this match still can't reach the API.
fn should_forward_line_progress_event(event: &crate::channel_reply::ProgressEvent) -> bool {
    if matches!(
        event,
        crate::channel_reply::ProgressEvent::Step { .. }
            | crate::channel_reply::ProgressEvent::ModelInfo { .. }
    ) {
        return false;
    }
    !event.to_display().is_empty()
}

async fn line_credentials_for_table(
    home_dir: &Path,
    table: &toml::Table,
) -> Option<(String, String)> {
    let token = crate::config_crypto::decrypt_config_field_async(
        table,
        "channels",
        "line_channel_token",
        home_dir,
    )
    .await?
    .expose_owned();
    let secret = crate::config_crypto::decrypt_config_field_async(
        table,
        "channels",
        "line_channel_secret",
        home_dir,
    )
    .await
    .map(|s| s.expose_owned())
    .unwrap_or_default();
    Some((token, secret))
}
async fn read_line_config(home_dir: &Path) -> Option<(String, String)> {
    let text = tokio::fs::read_to_string(home_dir.join("config.toml"))
        .await
        .ok()?;
    let table: toml::Table = text.parse().ok()?;
    line_credentials_for_table(home_dir, &table).await
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
        assert!(
            msgs.len() <= line_limits::MAX_MESSAGES,
            "must respect 5-message cap"
        );
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

    // ── WP-10C: LINE progress-event filter ──────────────────────

    use crate::channel_reply::{ProgressEvent, StepEvent, StepPhase, TodoItem};

    #[test]
    fn step_event_is_not_forwarded_to_line() {
        let ev = ProgressEvent::Step(StepEvent {
            phase: StepPhase::Start,
            tool: "Read".into(),
            summary: Some("x".into()),
            depth: 0,
            ts_ms: 1,
        });
        assert!(
            ev.to_display().is_empty(),
            "precondition: Step must render empty"
        );
        assert!(
            !should_forward_line_progress_event(&ev),
            "Step is dashboard-only and must never be pushed to LINE"
        );
    }

    #[test]
    fn model_info_event_is_not_forwarded_to_line() {
        let ev = ProgressEvent::ModelInfo {
            model: "claude-x".into(),
        };
        assert!(
            ev.to_display().is_empty(),
            "precondition: ModelInfo must render empty"
        );
        assert!(
            !should_forward_line_progress_event(&ev),
            "ModelInfo is dashboard-only and must never be pushed to LINE"
        );
    }

    #[test]
    fn keepalive_and_tool_use_and_todo_update_are_forwarded_to_line() {
        // These variants all render non-empty text and must keep reaching
        // the LINE Push API (this fix must not over-filter).
        assert!(should_forward_line_progress_event(
            &ProgressEvent::Keepalive
        ));
        assert!(should_forward_line_progress_event(
            &ProgressEvent::ToolUse {
                tool: "Read".into(),
                detail: Some("foo.rs".into()),
            }
        ));
        assert!(should_forward_line_progress_event(
            &ProgressEvent::TodoUpdate {
                todos: vec![TodoItem {
                    content: "do the thing".into(),
                    status: "pending".into(),
                    active_form: None,
                }],
            }
        ));
    }
}

/// Regression guard for the anonymous-sender CCR leak: this adapter used to
/// pass its `"unknown"` placeholder straight into the reply pipeline's
/// `user_id`, so every sender the webhook could not identify hashed to the
/// same `source_acl` and could retrieve the others' saved tool originals.
#[cfg(test)]
mod ccr_principal_tests {
    use crate::ccr_runtime::source_scan::call_args_at;
    use crate::ccr_runtime::{reply_principal_for_sender, source_acl_for_principal};

    const SRC: &str = include_str!("line.rs");
    const AGENT: &str = "agent-a";
    const SESSION: &str = "line:Cgroup123";

    /// Structural: every `build_guarded_reply_for_agent` call in this file
    /// must launder its principal. Checked over the real source because the
    /// call site lives inside a long async webhook handler that cannot be
    /// driven from a unit test.
    #[test]
    fn every_guarded_reply_call_launders_the_ccr_principal() {
        let args = call_args_at(SRC, "build_guarded_reply_for_agent(", 4);
        assert!(
            !args.is_empty(),
            "no guarded-reply call found — did the call site move?"
        );
        for arg in args {
            assert!(
                arg.starts_with("crate::ccr_runtime::reply_principal_for_sender("),
                "the CCR principal argument must be laundered, found `{arg}`"
            );
        }
    }

    #[test]
    fn an_unidentified_sender_disables_ccr_instead_of_sharing_one_scope() {
        let anonymous = reply_principal_for_sender("unknown");
        assert!(anonymous.is_empty());
        assert!(
            source_acl_for_principal(AGENT, SESSION, anonymous).is_none(),
            "an unidentified sender must disable CCR, never pool into one scope"
        );

        let alice = reply_principal_for_sender("Ualice");
        let bob = reply_principal_for_sender("Ubob");
        assert_ne!(
            source_acl_for_principal(AGENT, SESSION, alice).unwrap(),
            source_acl_for_principal(AGENT, SESSION, bob).unwrap()
        );
    }
}

#[cfg(test)]
#[path = "channel_decision_route/adapter_tests/line.rs"]
mod f4_decision_route_tests;
