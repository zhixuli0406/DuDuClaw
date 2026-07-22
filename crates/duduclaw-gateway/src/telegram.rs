//! Telegram Bot long-polling integration with topic/forum support.
//!
//! Features:
//! - Long-polling via /getUpdates
//! - Text, voice, and audio message handling
//! - Supergroup topic/forum support (message_thread_id)
//! - Mention-only mode for group chats
//! - Bot command registration (/ask, /status, /voice, /reset)
//! - Voice transcription (Whisper) + TTS synthesis
//! - Per-chat settings via ChannelSettingsManager

use std::path::Path;
use std::sync::Arc;

use duduclaw_core::truncate_bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info, warn};

use crate::channel_format;
use crate::channel_reply::{ReplyContext, build_reply_for_agent, build_reply_with_session, set_channel_connected};
use crate::channel_settings::keys;
use crate::tts::TtsProvider;

const TELEGRAM_API: &str = "https://api.telegram.org";

// ── Telegram API types ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TgResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    #[allow(dead_code)]
    id: i64,
    username: Option<String>,
    first_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgVoice {
    file_id: String,
    #[allow(dead_code)]
    duration: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TgAudio {
    file_id: String,
}

#[derive(Debug, Deserialize)]
struct TgFile {
    file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgPhotoSize {
    file_id: String,
    #[allow(dead_code)]
    width: Option<u32>,
    #[allow(dead_code)]
    height: Option<u32>,
    #[allow(dead_code)]
    file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TgDocument {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgVideo {
    file_id: String,
    #[allow(dead_code)]
    duration: Option<u32>,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgSticker {
    file_id: String,
    emoji: Option<String>,
    #[allow(dead_code)]
    is_animated: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: Option<i64>,
    text: Option<String>,
    voice: Option<TgVoice>,
    audio: Option<TgAudio>,
    photo: Option<Vec<TgPhotoSize>>,
    document: Option<TgDocument>,
    video: Option<TgVideo>,
    sticker: Option<TgSticker>,
    caption: Option<String>,
    chat: TgChat,
    from: Option<TgUser>,
    /// Thread ID for supergroup topics/forums.
    message_thread_id: Option<i64>,
    /// Entities (mentions, commands, etc.)
    entities: Option<Vec<TgEntity>>,
    /// Service message: a forum topic was closed (close the mapped session).
    forum_topic_closed: Option<serde_json::Value>,
    /// Service message: a forum topic was created (session is created lazily).
    forum_topic_created: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct TgEntity {
    #[serde(rename = "type")]
    entity_type: String,
    offset: usize,
    length: usize,
}

#[derive(Debug, Deserialize)]
struct TgCallbackQuery {
    /// Unique callback query ID — required by answerCallbackQuery.
    id: String,
    from: Option<TgUser>,
    /// The message the inline keyboard was attached to.
    message: Option<TgMessage>,
    /// The button's `callback_data`.
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
    callback_query: Option<TgCallbackQuery>,
}

#[derive(Debug, Serialize)]
struct SendMessage {
    chat_id: i64,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_parameters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<serde_json::Value>,
}

// ── Public API ──────────────────────────────────────────────

/// Start the Telegram bot polling loop as a background task.
///
/// Kept for backward compatibility — delegates to `start_telegram_bots()`.
pub async fn start_telegram_bot(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut bots = start_telegram_bots(home_dir, ctx).await;
    bots.pop().map(|(_, h)| h)
}

/// Start multiple Telegram bots: one global (from config.toml) plus per-agent bots.
///
/// Returns a Vec of (label, JoinHandle) where label is "telegram" for the global
/// bot and "telegram:{agent_name}" for per-agent bots.
///
/// ## Token exclusivity & precedence (agent binding wins)
///
/// Telegram's `getUpdates` long-poll is **exclusive per token** — only one
/// consumer may poll a given bot at a time; a second poller gets HTTP 409
/// Conflict and updates are split non-deterministically between the two.
///
/// A token may legitimately appear in *both* `config.toml` (global) and a
/// specific agent's `[channels.telegram]`. When that happens we must run exactly
/// **one** poller for it, and it must be the **agent-bound** one: the global
/// poller is generic and routes via `default_agent` (so a CEO bot could answer
/// as COO — "identity mixing"), whereas the per-agent poller routes
/// deterministically to its owner. We therefore collect agent tokens first and
/// skip the global poller for any token an agent already claims.
pub async fn start_telegram_bots(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Vec<(String, tokio::task::JoinHandle<()>)> {
    let mut results = Vec::new();
    let mut seen_tokens: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Collect per-agent tokens FIRST so the global poller can defer to them.
    let agent_tokens: Vec<(String, String)> = {
        let reg = ctx.registry.read().await;
        let mut tokens = Vec::new();
        for agent in reg.list() {
            if let Some(channels) = &agent.config.channels {
                if let Some(tg) = &channels.telegram {
                    let token = crate::config_crypto::resolve_agent_token(
                        &tg.bot_token_enc, &tg.bot_token, home_dir,
                    );
                    if !token.is_empty() {
                        tokens.push((agent.config.agent.name.clone(), token));
                    }
                }
            }
        }
        tokens
    };
    // 1. Global bot from config.toml — skipped when an agent already owns the
    //    same token (the per-agent poller below is authoritative). This is the
    //    fix for the dual-registration 409 + identity-mixing bug.
    if let Some(token) = read_telegram_token(home_dir).await {
        if !token.is_empty() {
            if let Some(owner) = crate::channel_reply::find_global_token_owner(
                &token,
                agent_tokens.iter().map(|(n, t)| (n.as_str(), t.as_str())),
            ) {
                warn!(
                    "Telegram global token is also bound to agent '{owner}' — \
                     skipping the global poller to avoid a 409 Conflict and \
                     identity mixing; the per-agent bot is authoritative"
                );
            } else {
                seen_tokens.insert(token.clone());
                if let Some(handle) = spawn_telegram_bot(token, "telegram".into(), None, ctx.clone(), home_dir).await {
                    results.push(("telegram".to_string(), handle));
                }
            }
        }
    }

    // 2. Per-agent bots (dedup among agents themselves — first claim wins).
    for (agent_name, token) in agent_tokens {
        if seen_tokens.contains(&token) {
            info!("Telegram bot for agent '{agent_name}' shares an already-claimed token — skipping duplicate");
            continue;
        }
        seen_tokens.insert(token.clone());
        let label = format!("telegram:{agent_name}");
        if let Some(handle) = spawn_telegram_bot(token, label.clone(), Some(agent_name), ctx.clone(), home_dir).await {
            results.push((label, handle));
        }
    }

    results
}

/// Spawn a single Telegram bot (shared by global and per-agent paths).
async fn spawn_telegram_bot(
    token: String,
    label: String,
    agent_name: Option<String>,
    ctx: Arc<ReplyContext>,
    _home_dir: &Path,
) -> Option<tokio::task::JoinHandle<()>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(35))
        .build()
        .ok()?;

    let api_base = format!("{}/bot{}", TELEGRAM_API, token);

    // Verify token
    match client.get(format!("{api_base}/getMe")).send().await {
        Ok(resp) => {
            if let Ok(data) = resp.json::<TgResponse<TgUser>>().await {
                if data.ok {
                    if let Some(user) = &data.result {
                        let name = user.username.as_deref().unwrap_or("unknown");
                        info!("Telegram bot connected: @{name} (label: {label})");
                        // Only register commands for the global bot
                        if agent_name.is_none() {
                            register_commands(&client, &api_base).await;
                        }
                    }
                    set_channel_connected(&ctx.channel_status, &label, true, None, Some(&ctx.event_tx)).await;
                } else {
                    let desc = data.description.unwrap_or_default();
                    warn!("Telegram getMe failed for {label}: {desc}");
                    set_channel_connected(&ctx.channel_status, &label, false, Some(desc), Some(&ctx.event_tx)).await;
                    return None;
                }
            }
        }
        Err(e) => {
            warn!("Telegram connection failed for {label}: {e}");
            set_channel_connected(&ctx.channel_status, &label, false, Some(e.to_string()), Some(&ctx.event_tx)).await;
            return None;
        }
    }

    let handle = tokio::spawn(async move {
        poll_loop(client, api_base, ctx, label, agent_name).await;
    });

    Some(handle)
}

/// Register bot commands with Telegram.
async fn register_commands(client: &reqwest::Client, api_base: &str) {
    // §10.6: user-visible product name honours white-label branding.
    let product = crate::branding::effective_product_name(&duduclaw_core::platform::duduclaw_home());
    let commands = json!({
        "commands": [
            { "command": "ask", "description": format!("向 {product} AI 提問") },
            { "command": "status", "description": "顯示機器人狀態" },
            { "command": "voice", "description": "切換語音回覆模式" },
            { "command": "reset", "description": "清除對話工作階段" },
            { "command": "help", "description": "顯示可用指令" }
        ]
    });

    match client
        .post(format!("{api_base}/setMyCommands"))
        .json(&commands)
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(data) = resp.json::<TgResponse<bool>>().await {
                if data.ok {
                    info!("Telegram: registered bot commands");
                }
            }
        }
        Err(e) => warn!("Telegram: failed to register commands: {e}"),
    }
}

// ── Internal ────────────────────────────────────────────────

async fn read_telegram_token(home_dir: &Path) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "telegram_bot_token").await
}

async fn poll_loop(
    client: reqwest::Client,
    api_base: String,
    ctx: Arc<ReplyContext>,
    label: String,
    agent_name: Option<String>,
) {
    let mut offset: i64 = 0;
    let mut consecutive_errors: u32 = 0;
    info!("Telegram polling started");

    // Get bot username for mention detection
    let bot_username = get_bot_username(&client, &api_base).await.unwrap_or_default();

    loop {
        let url = format!("{api_base}/getUpdates?offset={offset}&timeout=25&allowed_updates=[\"message\",\"callback_query\"]");

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                consecutive_errors += 1;
                warn!("Telegram poll error: {e}");
                set_channel_connected(&ctx.channel_status, &label, false, Some(e.to_string()), Some(&ctx.event_tx)).await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        let data: TgResponse<Vec<TgUpdate>> = match resp.json().await {
            Ok(d) => d,
            Err(e) => {
                consecutive_errors += 1;
                warn!("Telegram [{label}] parse error: {e}");
                set_channel_connected(&ctx.channel_status, &label, false, Some(e.to_string()), Some(&ctx.event_tx)).await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        if !data.ok {
            consecutive_errors += 1;
            let desc = data.description.unwrap_or_default();
            warn!("Telegram [{label}] API error: {desc}");
            set_channel_connected(&ctx.channel_status, &label, false, Some(desc), Some(&ctx.event_tx)).await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        if consecutive_errors > 0 {
            info!("Telegram [{label}] polling recovered after {consecutive_errors} errors");
        }
        consecutive_errors = 0;
        set_channel_connected(&ctx.channel_status, &label, true, None, Some(&ctx.event_tx)).await;

        if let Some(updates) = data.result {
            for update in updates {
                offset = update.update_id + 1;

                // ── Inline keyboard button presses ──
                if let Some(cb) = update.callback_query {
                    handle_callback_query(&cb, &client, &api_base, &ctx).await;
                    continue;
                }

                let Some(msg) = update.message else { continue };
                let chat_id = msg.chat.id;
                let msg_id = msg.message_id;
                let thread_id = msg.message_thread_id;

                // ── Forum topic lifecycle (service messages) ──
                // Topic closed → close the mapped session so a reopened topic
                // starts fresh (mirrors Discord thread auto-archive handling).
                if msg.forum_topic_closed.is_some() {
                    if let Some(tid) = thread_id {
                        let session_id = format!("telegram:{chat_id}:{tid}");
                        match ctx.session_manager.delete_session(&session_id).await {
                            Ok(()) => info!("Telegram: forum topic {tid} closed — session cleared"),
                            Err(e) => warn!("Telegram: forum topic {tid} closed, session clear failed: {e}"),
                        }
                    }
                    continue;
                }
                if msg.forum_topic_created.is_some() {
                    // Session is created lazily on the first real message.
                    continue;
                }
                let chat_type = msg.chat.chat_type.as_deref().unwrap_or("private");
                let is_group = chat_type == "group" || chat_type == "supergroup";
                let sender = msg.from.as_ref().and_then(|u| u.first_name.as_deref()).unwrap_or("someone");
                let scope_id = chat_id.to_string();

                // ── Mention-only filter for groups ──
                // Per-agent bots default to mention-only to prevent all bots responding
                let default_mention_only = agent_name.is_some();
                let mention_only = ctx.channel_settings
                    .get_bool("telegram", &scope_id, keys::MENTION_ONLY, default_mention_only).await;

                let text_content = msg.text.as_deref().unwrap_or("");
                let bot_mentioned = is_bot_mentioned(text_content, &msg.entities, &bot_username);

                // Check for bot commands (always process, even in mention-only mode)
                let is_command = text_content.starts_with('/');

                if is_group && mention_only && !bot_mentioned && !is_command {
                    continue;
                }

                // ── Channel whitelist ──
                if is_group && !ctx.channel_settings.is_channel_allowed("telegram", "global", &scope_id).await {
                    continue;
                }

                // Extract text — from text field or transcribed from voice/audio
                // Also collect attachment references for photo/document/video/sticker
                let mut attachment_lines: Vec<String> = Vec::new();
                let caption_text = msg.caption.as_deref().unwrap_or("");

                let input_text = if let Some(text) = &msg.text {
                    // Handle bot commands
                    if text.starts_with('/') {
                        let from_user_id = msg.from.as_ref().map(|u| u.id);
                        handle_command(text, &client, &api_base, chat_id, thread_id, &ctx, &scope_id, agent_name.as_deref(), from_user_id).await;
                        continue;
                    }
                    // Strip bot mention
                    strip_bot_mention(text, &bot_username)
                } else if let Some(voice) = &msg.voice {
                    info!("🎙 Telegram [{sender}]: voice message");
                    match transcribe_voice(&client, &api_base, &voice.file_id).await {
                        Ok(text) => {
                            info!("🎙 Telegram [{sender}] transcribed: {}", truncate_bytes(&text, 80));
                            text
                        }
                        Err(e) => {
                            warn!("Voice transcription failed: {e}");
                            send_reply(&client, &api_base, chat_id, "⚠️ 語音轉文字失敗 — 請再試一次", thread_id, msg_id, None).await;
                            continue;
                        }
                    }
                } else if let Some(audio) = &msg.audio {
                    info!("🎵 Telegram [{sender}]: audio message");
                    match transcribe_voice(&client, &api_base, &audio.file_id).await {
                        Ok(text) => text,
                        Err(e) => {
                            warn!("Audio transcription failed: {e}");
                            send_reply(&client, &api_base, chat_id, "⚠️ 音訊轉文字失敗 — 請再試一次", thread_id, msg_id, None).await;
                            continue;
                        }
                    }
                } else {
                    // No text/voice/audio — use caption as base text (or empty)
                    caption_text.to_string()
                };

                // ── Attachment handling: photo/document/video/sticker ──
                let home_for_attach = ctx.home_dir.clone();
                if let Some(photos) = &msg.photo {
                    // Telegram sends multiple sizes; take the largest (last element)
                    if let Some(largest) = photos.last() {
                        info!("🖼️ Telegram [{sender}]: photo attachment");
                        match download_telegram_file(&client, &api_base, &largest.file_id).await {
                            Ok(data) => {
                                let mime = crate::media::detect_mime(&data);
                                let ext = crate::media::extension_from_mime(&mime);
                                let fname = format!("photo.{ext}");
                                match crate::media::save_attachment_to_disk(&home_for_attach, &data, &fname).await {
                                    Ok(path) => {
                                        attachment_lines.push(crate::media::format_attachment_ref(
                                            &crate::media::MediaType::Image, &fname, &path,
                                        ));
                                    }
                                    Err(e) => warn!("Failed to save photo: {e}"),
                                }
                            }
                            Err(e) => warn!("Failed to download photo: {e}"),
                        }
                    }
                }

                if let Some(doc) = &msg.document {
                    info!("📎 Telegram [{sender}]: document attachment");
                    let fname = doc.file_name.as_deref().unwrap_or("document");
                    match download_telegram_file(&client, &api_base, &doc.file_id).await {
                        Ok(data) => {
                            let mime = doc.mime_type.as_deref().unwrap_or("application/octet-stream");
                            let mt = crate::media::media_type_from_mime(mime);
                            match crate::media::save_attachment_to_disk(&home_for_attach, &data, fname).await {
                                Ok(path) => {
                                    attachment_lines.push(crate::media::format_attachment_ref(&mt, fname, &path));
                                }
                                Err(e) => warn!("Failed to save document: {e}"),
                            }
                        }
                        Err(e) => warn!("Failed to download document: {e}"),
                    }
                }

                if let Some(video) = &msg.video {
                    info!("🎬 Telegram [{sender}]: video attachment");
                    let mime = video.mime_type.as_deref().unwrap_or("video/mp4");
                    let ext = crate::media::extension_from_mime(mime);
                    let fname = format!("video.{ext}");
                    match download_telegram_file(&client, &api_base, &video.file_id).await {
                        Ok(data) => {
                            match crate::media::save_attachment_to_disk(&home_for_attach, &data, &fname).await {
                                Ok(path) => {
                                    attachment_lines.push(crate::media::format_attachment_ref(
                                        &crate::media::MediaType::Video, &fname, &path,
                                    ));
                                }
                                Err(e) => warn!("Failed to save video: {e}"),
                            }
                        }
                        Err(e) => warn!("Failed to download video: {e}"),
                    }
                }

                if let Some(sticker) = &msg.sticker {
                    let emoji_label = sticker.emoji.as_deref().unwrap_or("sticker");
                    info!("🏷️ Telegram [{sender}]: sticker {emoji_label}");
                    match download_telegram_file(&client, &api_base, &sticker.file_id).await {
                        Ok(data) => {
                            let mime = crate::media::detect_mime(&data);
                            let ext = crate::media::extension_from_mime(&mime);
                            let fname = format!("sticker.{ext}");
                            match crate::media::save_attachment_to_disk(&home_for_attach, &data, &fname).await {
                                Ok(path) => {
                                    let label = format!("sticker {emoji_label}");
                                    attachment_lines.push(crate::media::format_attachment_ref(
                                        &crate::media::MediaType::Image, &label, &path,
                                    ));
                                }
                                Err(e) => warn!("Failed to save sticker: {e}"),
                            }
                        }
                        Err(e) => warn!("Failed to download sticker: {e}"),
                    }
                }

                // Combine text + attachment references
                let input_text = if attachment_lines.is_empty() {
                    input_text
                } else if input_text.trim().is_empty() {
                    attachment_lines.join("\n")
                } else {
                    format!("{input_text}\n\n{}", attachment_lines.join("\n"))
                };

                if input_text.trim().is_empty() {
                    continue;
                }

                info!("📩 Telegram [{sender}]: {}", truncate_bytes(&input_text, 80));

                // ── Build session ID (topic-aware) ──
                let session_id = if let Some(tid) = thread_id {
                    format!("telegram:{chat_id}:{tid}")
                } else {
                    format!("telegram:{chat_id}")
                };

                // Progress callback — edit-in-place: the first event posts a
                // status message, later events edit it (no channel spam).
                // TodoUpdate events bypass the 30s throttle so the task board
                // always reflects the latest state.
                let progress_client = client.clone();
                let progress_api = api_base.clone();
                let progress_chat_id = chat_id;
                let progress_thread_id = thread_id;
                let progress_msg_id: Arc<tokio::sync::Mutex<Option<i64>>> =
                    Arc::new(tokio::sync::Mutex::new(None));
                let progress_msg_cleanup = progress_msg_id.clone();
                let last_progress = Arc::new(std::sync::Mutex::new(
                    std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(60))
                        .unwrap_or_else(std::time::Instant::now),
                ));
                let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
                    // Step / ModelInfo events are dashboard-only signals — never
                    // rendered as channel text (would be an empty message).
                    if matches!(
                        event,
                        crate::channel_reply::ProgressEvent::Step { .. }
                            | crate::channel_reply::ProgressEvent::ModelInfo { .. }
                    ) {
                        return;
                    }
                    let is_todo = matches!(event, crate::channel_reply::ProgressEvent::TodoUpdate { .. });
                    {
                        let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
                        if !is_todo && last.elapsed().as_secs() < 30 {
                            return;
                        }
                        *last = std::time::Instant::now();
                    }

                    let msg_text = event.to_display();
                    let c = progress_client.clone();
                    let api = progress_api.clone();
                    let msg_id = progress_msg_id.clone();
                    tokio::spawn(async move {
                        let mut id_guard = msg_id.lock().await;
                        match *id_guard {
                            Some(mid) => edit_progress_message(&c, &api, progress_chat_id, mid, &msg_text).await,
                            None => {
                                *id_guard = send_progress_message(&c, &api, progress_chat_id, &msg_text, progress_thread_id).await;
                            }
                        }
                    });
                });

                // Typing indicator while the reply is generated (RAII —
                // stops on drop, including panic/early-continue paths).
                let typing_guard = crate::channel_typing::telegram_typing(
                    client.clone(),
                    api_base.clone(),
                    chat_id,
                    thread_id,
                );

                let user_id = msg.from.as_ref().map(|u| u.id.to_string()).unwrap_or_default();
                let reply = if let Some(ref agent) = agent_name {
                    // Per-agent bot: unchanged deterministic routing to its owner.
                    build_reply_for_agent(&input_text, &ctx, agent, &session_id, &user_id, Some(on_progress)).await
                } else {
                    // Global/shared bot: route by the user→agent binding (WP9).
                    match resolve_shared_route(&ctx, &user_id).await {
                        SharedRoute::Bound(bound_agent) => {
                            build_reply_for_agent(&input_text, &ctx, &bound_agent, &session_id, &user_id, Some(on_progress)).await
                        }
                        SharedRoute::Unbound => {
                            build_reply_with_session(&input_text, &ctx, &session_id, &user_id, Some(on_progress)).await
                        }
                        SharedRoute::Guide(msg) => msg,
                    }
                };
                drop(typing_guard);

                // Remove the interim progress/task-board message — the final
                // reply supersedes it.
                if let Some(mid) = progress_msg_cleanup.lock().await.take() {
                    let del_body = json!({ "chat_id": chat_id, "message_id": mid });
                    let _ = client.post(format!("{api_base}/deleteMessage")).json(&del_body).send().await;
                }

                // Guard: don't send empty replies (Telegram rejects empty text)
                if reply.trim().is_empty() {
                    warn!(chat_id, "Telegram: reply is empty — skipping send");
                    continue;
                }

                // Check voice mode
                let session_key = format!("telegram:{chat_id}");
                let voice_enabled = ctx.voice_sessions.lock().await.contains(&session_key);

                if voice_enabled {
                    let tts_provider = crate::tts::EdgeTtsProvider::new();
                    match tts_provider.synthesize(&reply, "").await {
                        Ok(audio_bytes) => {
                            send_voice(&client, &api_base, chat_id, audio_bytes).await;
                            if reply.len() > 200 {
                                send_reply(&client, &api_base, chat_id, &format!("📝 {}", truncate_bytes(&reply, 200)), thread_id, msg_id, None).await;
                            }
                        }
                        Err(e) => {
                            warn!("TTS synthesis failed, falling back to text: {e}");
                            send_reply_markdown(&client, &api_base, chat_id, &reply, thread_id, msg_id, Some(channel_format::telegram_conversation_buttons())).await;
                        }
                    }
                } else {
                    // Send with inline keyboard buttons
                    send_reply_markdown(&client, &api_base, chat_id, &reply, thread_id, msg_id, Some(channel_format::telegram_conversation_buttons())).await;
                }
            }
        }
    }
}

/// Handle an inline-keyboard button press (`callback_query` update).
///
/// `callback_data` format mirrors the Discord custom_id convention:
/// `duduclaw:{action}`. Every press is acknowledged via answerCallbackQuery
/// so the client's loading spinner always clears.
async fn handle_callback_query(
    cb: &TgCallbackQuery,
    client: &reqwest::Client,
    api_base: &str,
    ctx: &Arc<ReplyContext>,
) {
    let data = cb.data.as_deref().unwrap_or("");
    let sender = cb.from.as_ref().and_then(|u| u.first_name.as_deref()).unwrap_or("someone");
    let Some(msg) = &cb.message else {
        // No source message (e.g. too old) — just clear the spinner.
        answer_callback_query(client, api_base, &cb.id, "").await;
        return;
    };
    let chat_id = msg.chat.id;
    let thread_id = msg.message_thread_id;

    info!("🔘 Telegram [{sender}] pressed: {data}");

    // Install-approval buttons (Feature D): map the clicking Telegram account
    // to a dashboard user, authorize, and decide. Falls through to the other
    // actions when the data isn't an install-approval action.
    if let Some(tg_uid) = cb.from.as_ref().map(|u| u.id.to_string()) {
        if let Some(result) = crate::install_notify::decide_from_channel(
            &ctx.home_dir, "telegram", &tg_uid, data,
        )
        .await
        {
            let decided = result.is_ok();
            let text = match result {
                Ok(msg) => msg,
                Err(msg) => format!("⚠️ {msg}"),
            };
            answer_callback_query(client, api_base, &cb.id, &text).await;
            // The decision landed → the buttons are spent. Strip the inline
            // keyboard and append the outcome so the message tells the story
            // instead of inviting a second (futile) click.
            if decided && let Some(mid) = msg.message_id {
                let original = msg.text.as_deref().unwrap_or("");
                let body = json!({
                    "chat_id": chat_id,
                    "message_id": mid,
                    "text": format!("{original}\n\n{text}"),
                });
                let _ = client.post(format!("{api_base}/editMessageText")).json(&body).send().await;
            }
            return;
        }
    }

    // Goal-loop buttons (P2a): needs_human retry/done/abort + autonomy kickoff.
    // Same fall-through contract as install-approval — `None` ⇒ not a goal
    // action, continue to the other handlers.
    if let Some(tg_uid) = cb.from.as_ref().map(|u| u.id.to_string()) {
        if let Some(result) = crate::goal_notify::decide_from_channel(
            &ctx.home_dir, "telegram", &tg_uid, data,
        )
        .await
        {
            let decided = result.is_ok();
            let text = match result {
                Ok(msg) => msg,
                Err(msg) => format!("⚠️ {msg}"),
            };
            answer_callback_query(client, api_base, &cb.id, &text).await;
            if decided && let Some(mid) = msg.message_id {
                let original = msg.text.as_deref().unwrap_or("");
                let body = json!({
                    "chat_id": chat_id,
                    "message_id": mid,
                    "text": format!("{original}\n\n{text}"),
                });
                let _ = client.post(format!("{api_base}/editMessageText")).json(&body).send().await;
            }
            return;
        }
    }

    let answer = match data {
        "duduclaw:new_session" => {
            let session_id = if let Some(tid) = thread_id {
                format!("telegram:{chat_id}:{tid}")
            } else {
                format!("telegram:{chat_id}")
            };
            match ctx.session_manager.delete_session(&session_id).await {
                Ok(()) => "✅ 已開啟新的對話".to_string(),
                Err(e) => format!("⚠️ 清除工作階段失敗：{e}"),
            }
        }
        "duduclaw:voice_toggle" => {
            let session_key = format!("telegram:{chat_id}");
            let mut sessions = ctx.voice_sessions.lock().await;
            if sessions.contains(&session_key) {
                sessions.remove(&session_key);
                "🔇 已關閉語音回覆模式".to_string()
            } else {
                sessions.insert(session_key);
                "🎤 已開啟語音回覆模式".to_string()
            }
        }
        _ => "未知的按鈕動作".to_string(),
    };

    answer_callback_query(client, api_base, &cb.id, &answer).await;
}

/// Acknowledge a callback query (clears the client-side loading spinner).
/// An empty `text` acknowledges silently; otherwise a toast is shown.
async fn answer_callback_query(client: &reqwest::Client, api_base: &str, callback_id: &str, text: &str) {
    let mut body = json!({ "callback_query_id": callback_id });
    if !text.is_empty() {
        body["text"] = json!(text);
    }
    match client.post(format!("{api_base}/answerCallbackQuery")).json(&body).send().await {
        Ok(resp) => {
            if let Ok(data) = resp.json::<TgResponse<bool>>().await
                && !data.ok
            {
                warn!("Telegram answerCallbackQuery failed: {}", data.description.unwrap_or_default());
            }
        }
        Err(e) => warn!("Telegram answerCallbackQuery error: {e}"),
    }
}

/// Handle bot commands (/ask, /status, /voice, /reset, /help).
///
/// `from_user_id` is the Telegram sender's personal user id (when present)
/// — used for the per-channel `admin_users` check so a group's admin can be
/// identified by their own id, not just the shared chat id.
#[allow(clippy::too_many_arguments)]
async fn handle_command(
    text: &str,
    client: &reqwest::Client,
    api_base: &str,
    chat_id: i64,
    thread_id: Option<i64>,
    ctx: &Arc<ReplyContext>,
    scope_id: &str,
    agent_name: Option<&str>,
    from_user_id: Option<i64>,
) {
    // ── WP9: `/start <token>` deep-link binding (global/shared bot only) ──
    // Runs BEFORE the access gate so a brand-new employee can onboard even
    // under require_pairing; a successful bind also approves the user. Only
    // the shared bot (agent_name = None) participates — per-agent bots route
    // deterministically to their owner and ignore /start.
    {
        let raw = text.split_whitespace().next().unwrap_or("");
        let base = raw.split('@').next().unwrap_or(raw);
        if base == "/start" && agent_name.is_none() {
            // Split off the command token by whitespace rather than a raw byte
            // slice (coding convention #1: never index strings by byte offset —
            // safe here even with leading whitespace / multi-byte payloads).
            let payload = text
                .split_once(char::is_whitespace)
                .map(|(_, rest)| rest.trim())
                .unwrap_or("");
            let reply = handle_start_binding(ctx, payload, from_user_id).await;
            send_reply(client, api_base, chat_id, &reply, thread_id, None, None).await;
            return;
        }
    }

    // Central access gate (pairing / allowlist / blocklist) — the command
    // intercept runs BEFORE channel_reply's pipeline gate, so apply the SAME
    // gate here: unpaired/blocked users must not run /reset //undo //handoff
    // etc. (/ask is gated again inside build_reply — harmless and idempotent).
    {
        let gate_session_id = if let Some(tid) = thread_id {
            format!("telegram:{chat_id}:{tid}")
        } else {
            format!("telegram:{chat_id}")
        };
        if let Some(gate_reply) = crate::channel_reply::check_user_access_gate(
            ctx,
            &gate_session_id,
            scope_id,
            text,
        )
        .await
        {
            if !gate_reply.is_empty() {
                send_reply(client, api_base, chat_id, &gate_reply, thread_id, None, None).await;
            }
            return; // blocked users are silently ignored (empty reply)
        }
    }
    // Parse command (strip @bot_username suffix)
    let raw_cmd = text.split_whitespace().next().unwrap_or("");
    let cmd = raw_cmd.split('@').next().unwrap_or(raw_cmd);
    // Use raw_cmd.len() to skip the full "/ask@BotName" token, not just "/ask"
    let args = text[raw_cmd.len()..].trim();

    match cmd {
        "/ask" => {
            if args.is_empty() {
                send_reply(client, api_base, chat_id, "用法：/ask <你的問題>", thread_id, None, None).await;
                return;
            }
            let session_id = if let Some(tid) = thread_id {
                format!("telegram:{chat_id}:{tid}")
            } else {
                format!("telegram:{chat_id}")
            };
            let typing_guard = crate::channel_typing::telegram_typing(
                client.clone(),
                api_base.to_string(),
                chat_id,
                thread_id,
            );
            let reply = if let Some(agent) = agent_name {
                build_reply_for_agent(args, ctx, agent, &session_id, scope_id, None).await
            } else {
                // Global/shared bot: honor the user→agent binding (WP9). The
                // per-user key is the sender's id when present, else the chat.
                let bind_key = from_user_id.map(|id| id.to_string()).unwrap_or_else(|| scope_id.to_string());
                match resolve_shared_route(ctx, &bind_key).await {
                    SharedRoute::Bound(bound_agent) => {
                        build_reply_for_agent(args, ctx, &bound_agent, &session_id, scope_id, None).await
                    }
                    SharedRoute::Unbound => {
                        build_reply_with_session(args, ctx, &session_id, scope_id, None).await
                    }
                    SharedRoute::Guide(msg) => msg,
                }
            };
            drop(typing_guard);
            send_reply_markdown(client, api_base, chat_id, &reply, thread_id, None, Some(channel_format::telegram_conversation_buttons())).await;
        }
        "/status" => {
            let agent_info = {
                let reg = ctx.registry.read().await;
                reg.main_agent().map(|a| {
                    format!("*代理*：{} ({})\n*模型*：{}",
                        a.config.agent.display_name,
                        a.config.agent.name,
                        a.config.model.preferred)
                }).unwrap_or_else(|| "尚未設定代理".to_string())
            };

            let mention_only = ctx.channel_settings.get_bool("telegram", scope_id, keys::MENTION_ONLY, false).await;
            let status = format!("{agent_info}\n\n僅在被提及時回覆：{}", if mention_only { "✅" } else { "❌" });
            send_reply(client, api_base, chat_id, &status, thread_id, None, None).await;
        }
        "/voice" => {
            let session_key = format!("telegram:{chat_id}");
            let mut sessions = ctx.voice_sessions.lock().await;
            let msg = if sessions.contains(&session_key) {
                sessions.remove(&session_key);
                "🔇 已關閉語音回覆模式"
            } else {
                sessions.insert(session_key);
                "🎤 已開啟語音回覆模式"
            };
            send_reply(client, api_base, chat_id, msg, thread_id, None, None).await;
        }
        "/reset" => {
            let session_id = if let Some(tid) = thread_id {
                format!("telegram:{chat_id}:{tid}")
            } else {
                format!("telegram:{chat_id}")
            };
            let msg = match ctx.session_manager.delete_session(&session_id).await {
                Ok(()) => format!("✅ 已清除工作階段 `{session_id}`。"),
                Err(e) => format!("⚠️ 清除工作階段失敗：{e}"),
            };
            send_reply(client, api_base, chat_id, &msg, thread_id, None, None).await;
        }
        "/help" => {
            let help = "\
/ask <提問> — 向 AI 提問\n\
/status — 顯示機器人狀態\n\
/voice — 切換語音回覆模式\n\
/reset — 清除對話工作階段\n\
/help — 顯示本說明";
            send_reply(client, api_base, chat_id, help, thread_id, None, None).await;
        }
        _ => {
            // Not a legacy Telegram command — delegate to the shared
            // chat-command dispatcher (/handoff, /undo, /rollback, /new, …)
            // so Telegram gets the same command surface as other channels.
            // Reconstruct "<cmd> <args>" so a "/undo@BotName 2" form parses
            // the same as "/undo 2".
            let normalized = if args.is_empty() {
                cmd.to_string()
            } else {
                format!("{cmd} {args}")
            };
            if let Some(parsed) = crate::chat_commands::parse_command(&normalized, None) {
                let session_id = if let Some(tid) = thread_id {
                    format!("telegram:{chat_id}:{tid}")
                } else {
                    format!("telegram:{chat_id}")
                };
                let agent_id = match agent_name {
                    Some(a) => a.to_string(),
                    None => {
                        let reg = ctx.registry.read().await;
                        reg.main_agent()
                            .map(|a| a.config.agent.name.clone())
                            .unwrap_or_default()
                    }
                };
                // Real per-channel admin status (fail-closed) — never
                // hardcoded. Matches the sender's personal user id, the chat
                // id, or the full session id against `admin_users`.
                let from_id_str = from_user_id.map(|id| id.to_string());
                let mut identities: Vec<&str> = vec![scope_id, &session_id];
                if let Some(fid) = from_id_str.as_deref() {
                    identities.push(fid);
                }
                let is_admin =
                    crate::channel_reply::is_channel_admin(ctx, "telegram", &identities).await;
                let reply = crate::chat_commands::handle_command(
                    &parsed, ctx, &session_id, &agent_id, is_admin,
                )
                .await;
                send_reply(client, api_base, chat_id, &reply, thread_id, None, None).await;
            }
            // Still-unknown slash command — ignore (legacy behavior).
        }
    }
}

// ── WP9: shared-bot user→agent binding ──────────────────────

/// How the shared (global) Telegram bot should route one user's message.
enum SharedRoute {
    /// Route to a specific bound agent (user has scanned a bind link).
    Bound(String),
    /// No binding — fall back to the existing default-agent reply path.
    Unbound,
    /// No binding and shared-bot mode is on — reply with bind-first guidance
    /// (never route to a default agent, to avoid misrouting).
    Guide(String),
}

/// Decide how the global bot routes a message from `user_id` (fail-closed —
/// a message is only routed to a bound agent when a durable binding exists AND
/// that agent is still present in the registry).
async fn resolve_shared_route(ctx: &Arc<ReplyContext>, user_id: &str) -> SharedRoute {
    if let Some(agent) = ctx.agent_binding.resolve_bound_agent("telegram", user_id).await {
        // Honor the binding only if the target agent is still operational — a
        // soft-deleted/archived agent keeps its registry entry (that is what soft
        // delete means), so an existence check is not enough (F2): route only when
        // is_operational(), otherwise fall through instead of misrouting.
        let operational = ctx
            .registry
            .read()
            .await
            .get(&agent)
            .map(|a| a.config.agent.status.is_operational())
            .unwrap_or(false);
        if operational {
            return SharedRoute::Bound(agent);
        }
        warn!(
            agent,
            user_id, "Telegram: bound agent no longer operational — falling back"
        );
    }
    // Unbound: guide the user only when the operator has enabled shared-bot
    // binding for telegram; otherwise keep the legacy default-agent behavior.
    let shared_mode = ctx
        .channel_settings
        .get_bool("telegram", "global", keys::SHARED_BOT_BINDING, false)
        .await;
    if shared_mode {
        SharedRoute::Guide(
            "👋 您好！請使用專屬的綁定連結或掃描 QR code 來連結您的 AI 助理，之後即可直接對話。\n若您沒有連結，請向管理員索取。"
                .to_string(),
        )
    } else {
        SharedRoute::Unbound
    }
}

/// Handle a `/start [payload]` on the global bot. With a payload it is treated
/// as a one-time bind token: on success the Telegram user is bound to the
/// target agent (and approved for the access gate); otherwise a friendly
/// rejection. A bare `/start` greets and points the user at their bind link.
async fn handle_start_binding(
    ctx: &Arc<ReplyContext>,
    payload: &str,
    from_user_id: Option<i64>,
) -> String {
    let Some(uid) = from_user_id else {
        return "⚠️ 目前無法辨識您的帳號，請稍後再試一次。".to_string();
    };
    let user_id = uid.to_string();

    if payload.is_empty() {
        return "👋 歡迎！請使用專屬的綁定連結或掃描 QR code 來連結您的 AI 助理。".to_string();
    }

    match ctx.agent_binding.redeem_bind_token("telegram", payload, &user_id).await {
        Ok(agent_id) => {
            // A successful bind also grants access, so pairing-protected
            // deployments let this now-known employee through immediately.
            ctx.access_control.approve_user(&user_id).await;
            let display = {
                let reg = ctx.registry.read().await;
                reg.get(&agent_id)
                    .map(|a| a.config.agent.display_name.clone())
                    .unwrap_or_else(|| agent_id.clone())
            };
            info!(user_id, agent_id, "Telegram: user bound via /start deep-link");
            format!("✅ 綁定成功！您已連結到「{display}」，直接傳訊息就能開始對話。")
        }
        Err(e) => {
            warn!(user_id, ?e, "Telegram: /start bind token rejected");
            "❌ 這個連結無效或已過期，請向管理員索取新的綁定連結。".to_string()
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────

/// Get the bot username for mention detection.
async fn get_bot_username(client: &reqwest::Client, api_base: &str) -> Option<String> {
    let resp = client.get(format!("{api_base}/getMe")).send().await.ok()?;
    let data: TgResponse<TgUser> = resp.json().await.ok()?;
    data.result?.username
}

/// Extract a substring using UTF-16 offsets (Telegram Bot API convention).
/// Returns `None` if offsets are out of bounds.
fn utf16_slice(text: &str, offset: usize, length: usize) -> Option<String> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let end = offset.checked_add(length)?;
    let slice = utf16.get(offset..end)?;
    String::from_utf16(slice).ok()
}

/// Check if the bot is mentioned in the message.
fn is_bot_mentioned(text: &str, entities: &Option<Vec<TgEntity>>, bot_username: &str) -> bool {
    if bot_username.is_empty() {
        return false;
    }
    // Case-insensitive check for @username in text
    let target = format!("@{bot_username}");
    if text.to_ascii_lowercase().contains(&target.to_ascii_lowercase()) {
        return true;
    }
    // Check entities for mention type (using UTF-16 offsets per Telegram API)
    if let Some(ents) = entities {
        for ent in ents {
            if ent.entity_type == "mention" {
                if let Some(mention) = utf16_slice(text, ent.offset, ent.length) {
                    if mention.eq_ignore_ascii_case(&target) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Strip bot @mention from text.
fn strip_bot_mention(text: &str, bot_username: &str) -> String {
    if bot_username.is_empty() {
        return text.to_string();
    }
    text.replace(&format!("@{bot_username}"), "")
        .trim()
        .to_string()
}

/// Maximum audio download size (20MB, Telegram voice limit).
const MAX_TELEGRAM_AUDIO_BYTES: usize = 20 * 1024 * 1024;

/// Simple percent-decode for path validation.
fn percent_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                out.push(byte as char);
            } else {
                out.push('%');
                out.push_str(&hex);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Download a file from Telegram by file_id. Returns raw bytes.
///
/// Uses the same getFile → download flow as `transcribe_voice`, but returns
/// the raw bytes instead of transcribing. Used for photo/document/video/sticker.
async fn download_telegram_file(
    client: &reqwest::Client,
    api_base: &str,
    file_id: &str,
) -> Result<Vec<u8>, String> {
    let resp = client
        .get(format!("{api_base}/getFile"))
        .query(&[("file_id", file_id)])
        .send()
        .await
        .map_err(|e| format!("getFile: {e}"))?;
    let data: TgResponse<TgFile> = resp.json().await.map_err(|e| format!("getFile parse: {e}"))?;
    let file_path = data
        .result
        .and_then(|f| f.file_path)
        .ok_or_else(|| "getFile returned no file_path".to_string())?;

    let is_safe = |p: &str| -> bool {
        !p.contains("..") && !p.starts_with('/') && !p.contains('\0')
            && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    };
    let decoded = percent_decode(&file_path);
    if !is_safe(&file_path) || !is_safe(&decoded) {
        return Err("Invalid file_path from Telegram".to_string());
    }

    let file_url = api_base.replace("/bot", "/file/bot");
    let resp = client
        .get(format!("{file_url}/{file_path}"))
        .send()
        .await
        .map_err(|e| format!("Download: {e}"))?;

    if let Some(len) = resp.content_length() {
        if len > MAX_TELEGRAM_AUDIO_BYTES as u64 {
            return Err(format!("File too large: {len} bytes (max {MAX_TELEGRAM_AUDIO_BYTES})"));
        }
    }

    let bytes = resp.bytes().await.map_err(|e| format!("Download bytes: {e}"))?;
    if bytes.len() > MAX_TELEGRAM_AUDIO_BYTES {
        return Err(format!("File too large: {} bytes", bytes.len()));
    }

    Ok(bytes.to_vec())
}

async fn transcribe_voice(
    client: &reqwest::Client,
    api_base: &str,
    file_id: &str,
) -> Result<String, String> {
    let resp = client
        .get(format!("{api_base}/getFile"))
        .query(&[("file_id", file_id)])
        .send()
        .await
        .map_err(|e| format!("getFile: {e}"))?;
    let data: TgResponse<TgFile> = resp.json().await.map_err(|e| format!("getFile parse: {e}"))?;
    let file_path = data
        .result
        .and_then(|f| f.file_path)
        .ok_or_else(|| "getFile returned no file_path".to_string())?;

    let is_safe = |p: &str| -> bool {
        !p.contains("..") && !p.starts_with('/') && !p.contains('\0')
            && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    };
    let decoded = percent_decode(&file_path);
    if !is_safe(&file_path) || !is_safe(&decoded) {
        return Err("Invalid file_path from Telegram".to_string());
    }

    let file_url = api_base.replace("/bot", "/file/bot");
    let resp = client
        .get(format!("{file_url}/{file_path}"))
        .send()
        .await
        .map_err(|e| format!("Download: {e}"))?;

    if let Some(len) = resp.content_length() {
        if len > MAX_TELEGRAM_AUDIO_BYTES as u64 {
            return Err(format!("Audio too large: {len} bytes (max {MAX_TELEGRAM_AUDIO_BYTES})"));
        }
    }

    let audio_bytes = resp.bytes().await.map_err(|e| format!("Download bytes: {e}"))?;
    if audio_bytes.len() > MAX_TELEGRAM_AUDIO_BYTES {
        return Err(format!("Audio too large: {} bytes", audio_bytes.len()));
    }

    info!(bytes = audio_bytes.len(), "Voice file downloaded from Telegram");

    let text = duduclaw_inference::whisper::transcribe(
        &audio_bytes,
        Some("zh"),
        &duduclaw_inference::whisper::WhisperMode::Api,
    )
    .await
    .map_err(|_| "Voice transcription failed — please try again".to_string())?;

    if text.trim().is_empty() {
        return Err("Transcription returned empty text".into());
    }
    Ok(text)
}

async fn send_voice(client: &reqwest::Client, api_base: &str, chat_id: i64, audio_data: Vec<u8>) {
    let part = match reqwest::multipart::Part::bytes(audio_data)
        .file_name("reply.mp3")
        .mime_str("audio/mpeg")
    {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to create voice part: {e}");
            return;
        }
    };

    let form = reqwest::multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part("audio", part);

    match client.post(format!("{api_base}/sendAudio")).multipart(form).send().await {
        Ok(resp) => {
            if let Ok(data) = resp.json::<TgResponse<serde_json::Value>>().await
                && !data.ok
            {
                error!("Telegram sendAudio failed: {}", data.description.unwrap_or_default());
            }
        }
        Err(e) => error!("Telegram sendAudio error: {e}"),
    }
}

/// Send an interim progress message; returns its message_id for later edits.
async fn send_progress_message(
    client: &reqwest::Client,
    api_base: &str,
    chat_id: i64,
    text: &str,
    message_thread_id: Option<i64>,
) -> Option<i64> {
    let mut body = json!({ "chat_id": chat_id, "text": text });
    if let Some(tid) = message_thread_id {
        body["message_thread_id"] = json!(tid);
    }
    let resp = client.post(format!("{api_base}/sendMessage")).json(&body).send().await.ok()?;
    let data: TgResponse<serde_json::Value> = resp.json().await.ok()?;
    if !data.ok {
        return None;
    }
    data.result?.get("message_id").and_then(|v| v.as_i64())
}

/// Edit an interim progress message in place (best-effort).
async fn edit_progress_message(
    client: &reqwest::Client,
    api_base: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
) {
    let body = json!({ "chat_id": chat_id, "message_id": message_id, "text": text });
    let _ = client.post(format!("{api_base}/editMessageText")).json(&body).send().await;
}

/// Source-markdown chunk budget for HTML-rendered replies. HTML escaping
/// and tags inflate the text, so chunk well under Telegram's 4096-char
/// post-parse limit.
const TG_MARKDOWN_CHUNK: usize = 3400;

/// Send an AI reply: markdown → Telegram HTML (tables → <pre>, code fences
/// → <pre><code>, headings → bold). Each chunk falls back to plain text if
/// Telegram rejects the HTML entities.
async fn send_reply_markdown(
    client: &reqwest::Client,
    api_base: &str,
    chat_id: i64,
    markdown: &str,
    message_thread_id: Option<i64>,
    reply_to_message_id: Option<i64>,
    reply_markup: Option<serde_json::Value>,
) {
    let chunks = channel_format::split_text(markdown, TG_MARKDOWN_CHUNK);
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let html = crate::markdown_render::to_telegram_html(chunk);
        let reply_params = if i == 0 {
            reply_to_message_id.map(|mid| json!({ "message_id": mid }))
        } else {
            None
        };
        let markup = if i == last { reply_markup.clone() } else { None };

        // Oversized after rendering (pathological escaping) → plain chunk.
        if html.chars().count() > channel_format::limits::TELEGRAM_MESSAGE {
            send_message_once(client, api_base, chat_id, chunk, None, message_thread_id, reply_params, markup).await;
            continue;
        }

        let ok = send_message_once(
            client, api_base, chat_id, &html, Some("HTML"), message_thread_id,
            reply_params.clone(), markup.clone(),
        )
        .await;
        if !ok {
            // HTML parse rejected — resend the raw chunk as plain text so
            // the reply is never silently dropped.
            warn!("Telegram: HTML parse failed — falling back to plain text");
            send_message_once(client, api_base, chat_id, chunk, None, message_thread_id, None, markup).await;
        }
    }
}

/// POST a single sendMessage. Returns `true` on success (`ok: true`).
#[allow(clippy::too_many_arguments)]
async fn send_message_once(
    client: &reqwest::Client,
    api_base: &str,
    chat_id: i64,
    text: &str,
    parse_mode: Option<&str>,
    message_thread_id: Option<i64>,
    reply_parameters: Option<serde_json::Value>,
    reply_markup: Option<serde_json::Value>,
) -> bool {
    let body = SendMessage {
        chat_id,
        text: text.to_string(),
        parse_mode: parse_mode.map(|s| s.to_string()),
        message_thread_id,
        reply_parameters,
        reply_markup,
    };
    match client.post(format!("{api_base}/sendMessage")).json(&body).send().await {
        Ok(resp) => match resp.json::<TgResponse<serde_json::Value>>().await {
            Ok(data) => {
                if !data.ok {
                    error!(
                        "Telegram send failed: {}",
                        data.description.unwrap_or_default()
                    );
                }
                data.ok
            }
            Err(e) => {
                error!("Telegram send parse error: {e}");
                false
            }
        },
        Err(e) => {
            error!("Telegram send error: {e}");
            false
        }
    }
}

#[cfg(test)]
mod callback_tests {
    use super::*;

    #[test]
    fn parses_callback_query_update() {
        let json = r#"{
            "update_id": 42,
            "callback_query": {
                "id": "cbq-1",
                "from": { "id": 7, "username": "amy", "first_name": "Amy" },
                "message": { "message_id": 9, "chat": { "id": -100123, "type": "supergroup" }, "message_thread_id": 55 },
                "data": "duduclaw:new_session"
            }
        }"#;
        let update: TgUpdate = serde_json::from_str(json).unwrap();
        let cb = update.callback_query.expect("callback_query parsed");
        assert_eq!(cb.id, "cbq-1");
        assert_eq!(cb.data.as_deref(), Some("duduclaw:new_session"));
        let msg = cb.message.unwrap();
        assert_eq!(msg.chat.id, -100123);
        assert_eq!(msg.message_thread_id, Some(55));
    }

    #[test]
    fn message_only_update_still_parses() {
        let json = r#"{"update_id": 1, "message": { "chat": { "id": 5, "type": "private" } }}"#;
        let update: TgUpdate = serde_json::from_str(json).unwrap();
        assert!(update.callback_query.is_none());
        assert!(update.message.is_some());
    }
}

async fn send_reply(
    client: &reqwest::Client,
    api_base: &str,
    chat_id: i64,
    text: &str,
    message_thread_id: Option<i64>,
    reply_to_message_id: Option<i64>,
    reply_markup: Option<serde_json::Value>,
) {
    // Split long messages
    let chunks = channel_format::split_text(text, channel_format::limits::TELEGRAM_MESSAGE);

    for (i, chunk) in chunks.iter().enumerate() {
        // Only reply-to the original message on the first chunk
        let reply_params = if i == 0 {
            reply_to_message_id.map(|mid| json!({ "message_id": mid }))
        } else {
            None
        };
        let body = SendMessage {
            chat_id,
            text: chunk.to_string(),
            parse_mode: Some("Markdown".to_string()),
            message_thread_id,
            reply_parameters: reply_params.clone(),
            // Only add buttons to the last chunk
            reply_markup: if i == chunks.len() - 1 { reply_markup.clone() } else { None },
        };

        match client.post(format!("{api_base}/sendMessage")).json(&body).send().await {
            Ok(resp) => {
                if let Ok(data) = resp.json::<TgResponse<serde_json::Value>>().await
                    && !data.ok
                {
                    let desc = data.description.unwrap_or_default();
                    error!("Telegram send failed: {desc}");

                    // Retry without reply_parameters if the referenced message
                    // is invalid (deleted, wrong chat, etc.)
                    if reply_params.is_some() && (desc.contains("message not found")
                        || desc.contains("replied message not found")
                        || desc.contains("Bad Request"))
                    {
                        warn!("Telegram: retrying without reply_parameters / Markdown");
                        // HC1: drop parse_mode on retry. A Markdown parse error
                        // would otherwise re-fail identically and silently drop the
                        // reply. `parse_mode` is `skip_serializing_if=Option::is_none`,
                        // so `None` sends plain text. Mirrors dispatcher.rs.
                        let fallback = SendMessage {
                            chat_id,
                            text: chunk.to_string(),
                            parse_mode: None,
                            message_thread_id,
                            reply_parameters: None,
                            reply_markup: if i == chunks.len() - 1 { reply_markup.clone() } else { None },
                        };
                        match client.post(format!("{api_base}/sendMessage")).json(&fallback).send().await {
                            Ok(r2) => {
                                if let Ok(d2) = r2.json::<TgResponse<serde_json::Value>>().await
                                    && !d2.ok
                                {
                                    error!("Telegram retry also failed: {}", d2.description.unwrap_or_default());
                                }
                            }
                            Err(e2) => error!("Telegram retry error: {e2}"),
                        }
                    }
                }
            }
            Err(e) => error!("Telegram send error: {e}"),
        }
    }
}
