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
    id: String,
    data: Option<String>,
    message: Option<TgMessage>,
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
/// Deduplicates by token value — if an agent token matches the global token, it
/// is skipped (the global bot already covers it).
pub async fn start_telegram_bots(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Vec<(String, tokio::task::JoinHandle<()>)> {
    let mut results = Vec::new();
    let mut seen_tokens: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Global bot from config.toml
    if let Some(token) = read_telegram_token(home_dir).await {
        if !token.is_empty() {
            seen_tokens.insert(token.clone());
            if let Some(handle) = spawn_telegram_bot(token, "telegram".into(), None, ctx.clone(), home_dir).await {
                results.push(("telegram".to_string(), handle));
            }
        }
    }

    // 2. Per-agent bots from agent configs
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

    for (agent_name, token) in agent_tokens {
        if seen_tokens.contains(&token) {
            info!("Telegram bot for agent '{agent_name}' shares global token — skipping duplicate");
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
    let commands = json!({
        "commands": [
            { "command": "ask", "description": "Ask DuDuClaw AI a question" },
            { "command": "status", "description": "Show bot status" },
            { "command": "voice", "description": "Toggle voice reply mode" },
            { "command": "reset", "description": "Clear conversation session" },
            { "command": "help", "description": "Show available commands" }
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
        let url = format!("{api_base}/getUpdates?offset={offset}&timeout=25&allowed_updates=[\"message\"]");

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

                let Some(msg) = update.message else { continue };
                let chat_id = msg.chat.id;
                let msg_id = msg.message_id;
                let thread_id = msg.message_thread_id;
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
                        handle_command(text, &client, &api_base, chat_id, thread_id, &ctx, &scope_id, agent_name.as_deref()).await;
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
                            send_reply(&client, &api_base, chat_id, "⚠️ Voice transcription failed — please try again", thread_id, msg_id, None).await;
                            continue;
                        }
                    }
                } else if let Some(audio) = &msg.audio {
                    info!("🎵 Telegram [{sender}]: audio message");
                    match transcribe_voice(&client, &api_base, &audio.file_id).await {
                        Ok(text) => text,
                        Err(e) => {
                            warn!("Audio transcription failed: {e}");
                            send_reply(&client, &api_base, chat_id, "⚠️ Audio transcription failed — please try again", thread_id, msg_id, None).await;
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

                // Progress callback
                let progress_client = client.clone();
                let progress_api = api_base.clone();
                let progress_chat_id = chat_id;
                let progress_thread_id = thread_id;
                let last_progress = Arc::new(std::sync::Mutex::new(
                    std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(60))
                        .unwrap_or_else(std::time::Instant::now),
                ));
                let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
                    let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
                    if last.elapsed().as_secs() < 30 {
                        return;
                    }
                    *last = std::time::Instant::now();
                    drop(last);

                    let msg_text = event.to_display();
                    let c = progress_client.clone();
                    let api = progress_api.clone();
                    tokio::spawn(async move {
                        send_reply(&c, &api, progress_chat_id, &msg_text, progress_thread_id, None, None).await;
                    });
                });

                let user_id = msg.from.as_ref().map(|u| u.id.to_string()).unwrap_or_default();
                let reply = if let Some(ref agent) = agent_name {
                    build_reply_for_agent(&input_text, &ctx, agent, &session_id, &user_id, Some(on_progress)).await
                } else {
                    build_reply_with_session(&input_text, &ctx, &session_id, &user_id, Some(on_progress)).await
                };

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
                            send_reply(&client, &api_base, chat_id, &reply, thread_id, msg_id, Some(channel_format::telegram_conversation_buttons())).await;
                        }
                    }
                } else {
                    // Send with inline keyboard buttons
                    send_reply(&client, &api_base, chat_id, &reply, thread_id, msg_id, Some(channel_format::telegram_conversation_buttons())).await;
                }
            }
        }
    }
}

/// Handle bot commands (/ask, /status, /voice, /reset, /help).
async fn handle_command(
    text: &str,
    client: &reqwest::Client,
    api_base: &str,
    chat_id: i64,
    thread_id: Option<i64>,
    ctx: &Arc<ReplyContext>,
    scope_id: &str,
    agent_name: Option<&str>,
) {
    // Parse command (strip @bot_username suffix)
    let raw_cmd = text.split_whitespace().next().unwrap_or("");
    let cmd = raw_cmd.split('@').next().unwrap_or(raw_cmd);
    // Use raw_cmd.len() to skip the full "/ask@BotName" token, not just "/ask"
    let args = text[raw_cmd.len()..].trim();

    match cmd {
        "/ask" => {
            if args.is_empty() {
                send_reply(client, api_base, chat_id, "Usage: /ask <your question>", thread_id, None, None).await;
                return;
            }
            let session_id = if let Some(tid) = thread_id {
                format!("telegram:{chat_id}:{tid}")
            } else {
                format!("telegram:{chat_id}")
            };
            let reply = if let Some(agent) = agent_name {
                build_reply_for_agent(args, ctx, agent, &session_id, scope_id, None).await
            } else {
                build_reply_with_session(args, ctx, &session_id, scope_id, None).await
            };
            send_reply(client, api_base, chat_id, &reply, thread_id, None, Some(channel_format::telegram_conversation_buttons())).await;
        }
        "/status" => {
            let agent_info = {
                let reg = ctx.registry.read().await;
                reg.main_agent().map(|a| {
                    format!("*Agent*: {} ({})\n*Model*: {}",
                        a.config.agent.display_name,
                        a.config.agent.name,
                        a.config.model.preferred)
                }).unwrap_or_else(|| "No agent configured".to_string())
            };

            let mention_only = ctx.channel_settings.get_bool("telegram", scope_id, keys::MENTION_ONLY, false).await;
            let status = format!("{agent_info}\n\nMention Only: {}", if mention_only { "✅" } else { "❌" });
            send_reply(client, api_base, chat_id, &status, thread_id, None, None).await;
        }
        "/voice" => {
            let session_key = format!("telegram:{chat_id}");
            let mut sessions = ctx.voice_sessions.lock().await;
            let msg = if sessions.contains(&session_key) {
                sessions.remove(&session_key);
                "🔇 Voice reply mode disabled"
            } else {
                sessions.insert(session_key);
                "🎤 Voice reply mode enabled"
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
                Ok(()) => format!("✅ Session `{session_id}` cleared."),
                Err(e) => format!("⚠️ Failed to clear session: {e}"),
            };
            send_reply(client, api_base, chat_id, &msg, thread_id, None, None).await;
        }
        "/help" => {
            let help = "\
/ask <prompt> — Ask AI a question\n\
/status — Show bot status\n\
/voice — Toggle voice reply mode\n\
/reset — Clear conversation session\n\
/help — Show this help";
            send_reply(client, api_base, chat_id, help, thread_id, None, None).await;
        }
        _ => {} // Unknown command — ignore
    }
}

/// Handle callback queries from inline keyboard buttons.
async fn handle_callback_query(
    client: &reqwest::Client,
    api_base: &str,
    cb: &TgCallbackQuery,
    ctx: &Arc<ReplyContext>,
) {
    let cb_data = cb.data.as_deref().unwrap_or("");
    let chat_id = match cb.message.as_ref().map(|m| m.chat.id) {
        Some(id) => id,
        None => {
            // No message context (e.g. inline mode) — just answer the callback
            let _ = client
                .post(format!("{api_base}/answerCallbackQuery"))
                .json(&json!({ "callback_query_id": cb.id }))
                .send()
                .await;
            return;
        }
    };
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);

    // Parse callback data: "duduclaw:{action}"
    if let Some(action) = cb_data.strip_prefix("duduclaw:") {
        match action {
            "new_session" => {
                let session_id = if let Some(tid) = thread_id {
                    format!("telegram:{chat_id}:{tid}")
                } else {
                    format!("telegram:{chat_id}")
                };
                let msg = format!("Started new session. Previous: `{session_id}`");
                send_reply(client, api_base, chat_id, &msg, thread_id, None, None).await;
            }
            "voice_toggle" => {
                let session_key = format!("telegram:{chat_id}");
                let mut sessions = ctx.voice_sessions.lock().await;
                let msg = if sessions.contains(&session_key) {
                    sessions.remove(&session_key);
                    "🔇 Voice mode disabled"
                } else {
                    sessions.insert(session_key);
                    "🎤 Voice mode enabled"
                };
                send_reply(client, api_base, chat_id, msg, thread_id, None, None).await;
            }
            _ => {}
        }
    }

    // Answer the callback query to dismiss the loading state
    let _ = client
        .post(format!("{api_base}/answerCallbackQuery"))
        .json(&json!({ "callback_query_id": cb.id }))
        .send()
        .await;
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
                        warn!("Telegram: retrying without reply_parameters");
                        let fallback = SendMessage {
                            chat_id,
                            text: chunk.to_string(),
                            parse_mode: Some("Markdown".to_string()),
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
