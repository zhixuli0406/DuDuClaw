//! Lightweight Telegram Bot long-polling integration.
//!
//! Runs as a background tokio task alongside the WebSocket gateway.
//! Receives messages from Telegram, routes them to the configured main agent,
//! and sends responses back.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::channel_reply::{ReplyContext, build_reply_with_progress, set_channel_connected};
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
struct TgMessage {
    text: Option<String>,
    voice: Option<TgVoice>,
    audio: Option<TgAudio>,
    chat: TgChat,
    from: Option<TgUser>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
}

#[derive(Debug, Serialize)]
struct SendMessage {
    chat_id: i64,
    text: String,
    parse_mode: Option<String>,
}

// ── Public API ──────────────────────────────────────────────

/// Start the Telegram bot polling loop as a background task.
///
/// Returns `None` if no Telegram token is configured.
pub async fn start_telegram_bot(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<tokio::task::JoinHandle<()>> {
    let token = read_telegram_token(home_dir).await?;

    if token.is_empty() {
        return None;
    }

    // Verify token
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(35))
        .build()
        .ok()?;

    let api_base = format!("{}/bot{}", TELEGRAM_API, token);

    match client
        .get(format!("{api_base}/getMe"))
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(data) = resp.json::<TgResponse<TgUser>>().await {
                if data.ok {
                    if let Some(user) = &data.result {
                        let name = user.username.as_deref().unwrap_or("unknown");
                        info!("🤖 Telegram bot connected: @{name}");
                    }
                    set_channel_connected(&ctx.channel_status, "telegram", true, None).await;
                } else {
                    let desc = data.description.unwrap_or_default();
                    warn!("Telegram getMe failed: {desc}");
                    set_channel_connected(&ctx.channel_status, "telegram", false, Some(desc)).await;
                    return None;
                }
            }
        }
        Err(e) => {
            warn!("Telegram connection failed: {e}");
            set_channel_connected(&ctx.channel_status, "telegram", false, Some(e.to_string())).await;
            return None;
        }
    }

    let handle = tokio::spawn(async move {
        poll_loop(client, api_base, ctx).await;
    });

    Some(handle)
}

// ── Internal ────────────────────────────────────────────────

async fn read_telegram_token(home_dir: &Path) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "telegram_bot_token").await
}

async fn poll_loop(
    client: reqwest::Client,
    api_base: String,
    ctx: Arc<ReplyContext>,
) {
    let mut offset: i64 = 0;
    let mut consecutive_errors: u32 = 0;
    info!("Telegram polling started");

    loop {
        let url = format!("{api_base}/getUpdates?offset={offset}&timeout=25");

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                consecutive_errors += 1;
                warn!("Telegram poll error: {e}");
                set_channel_connected(&ctx.channel_status, "telegram", false, Some(e.to_string())).await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        let data: TgResponse<Vec<TgUpdate>> = match resp.json().await {
            Ok(d) => d,
            Err(e) => {
                consecutive_errors += 1;
                warn!("Telegram parse error: {e}");
                set_channel_connected(&ctx.channel_status, "telegram", false, Some(e.to_string())).await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        if !data.ok {
            consecutive_errors += 1;
            let desc = data.description.unwrap_or_default();
            warn!("Telegram API error: {desc}");
            set_channel_connected(&ctx.channel_status, "telegram", false, Some(desc)).await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        // Poll succeeded — mark connected (only log recovery once)
        if consecutive_errors > 0 {
            info!("Telegram polling recovered after {consecutive_errors} errors");
        }
        consecutive_errors = 0;
        set_channel_connected(&ctx.channel_status, "telegram", true, None).await;

        if let Some(updates) = data.result {
            for update in updates {
                offset = update.update_id + 1;

                let Some(msg) = update.message else { continue };
                let chat_id = msg.chat.id;
                let sender = msg
                    .from
                    .as_ref()
                    .and_then(|u| u.first_name.as_deref())
                    .unwrap_or("someone");

                // Extract text — either from text field or transcribed from voice
                let input_text = if let Some(text) = &msg.text {
                    text.clone()
                } else if let Some(voice) = &msg.voice {
                    // Voice message → download OGG → ASR → text
                    info!("🎙 Telegram [{sender}]: voice message (file_id: {})", &voice.file_id);
                    match transcribe_voice(&client, &api_base, &voice.file_id).await {
                        Ok(text) => {
                            info!("🎙 Telegram [{sender}] transcribed: {}", &text[..text.len().min(80)]);
                            text
                        }
                        Err(e) => {
                            warn!("Voice transcription failed: {e}");
                            send_reply(&client, &api_base, chat_id, "⚠️ Voice transcription failed — please try again").await;
                            continue;
                        }
                    }
                } else if let Some(audio) = &msg.audio {
                    // Audio file → download → ASR → text
                    info!("🎵 Telegram [{sender}]: audio message (file_id: {})", &audio.file_id);
                    match transcribe_voice(&client, &api_base, &audio.file_id).await {
                        Ok(text) => text,
                        Err(e) => {
                            warn!("Audio transcription failed: {e}");
                            send_reply(&client, &api_base, chat_id, "⚠️ Audio transcription failed — please try again").await;
                            continue;
                        }
                    }
                } else {
                    continue; // Unsupported message type
                };

                {
                    info!("📩 Telegram [{sender}]: {}", &input_text[..input_text.len().min(80)]);

                    // Progress callback: sends keepalive/tool-use messages to the chat.
                    let progress_client = client.clone();
                    let progress_api = api_base.clone();
                    let progress_chat_id = chat_id;
                    let last_progress = Arc::new(std::sync::Mutex::new(std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(60))
                        .unwrap_or_else(std::time::Instant::now)));
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
                            send_reply(&c, &api, progress_chat_id, &msg_text).await;
                        });
                    });

                    let reply = build_reply_with_progress(&input_text, &ctx, Some(on_progress)).await;

                    // Check if voice mode is enabled for this session
                    let session_key = format!("telegram:{chat_id}");
                    let voice_enabled = ctx.voice_sessions.lock().await.contains(&session_key);

                    if voice_enabled && !reply.is_empty() {
                        // Synthesize reply as audio via edge-tts (free) or MiniMax (paid)
                        let tts_provider = crate::tts::EdgeTtsProvider::new();
                        match tts_provider.synthesize(&reply, "").await {
                            Ok(audio_bytes) => {
                                send_voice(&client, &api_base, chat_id, audio_bytes).await;
                                // Also send text as caption for accessibility
                                if reply.len() > 200 {
                                    send_reply(&client, &api_base, chat_id, &format!("📝 {}", &reply[..200])).await;
                                }
                            }
                            Err(e) => {
                                warn!("TTS synthesis failed, falling back to text: {e}");
                                send_reply(&client, &api_base, chat_id, &reply).await;
                            }
                        }
                    } else {
                        send_reply(&client, &api_base, chat_id, &reply).await;
                    }
                }
            }
        }
    }
}

/// Download a Telegram file by file_id → transcribe via ASR → return text.
/// Maximum audio download size (20MB, Telegram voice limit).
const MAX_TELEGRAM_AUDIO_BYTES: usize = 20 * 1024 * 1024;

/// Simple percent-decode for path validation (no external crate needed).
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

async fn transcribe_voice(
    client: &reqwest::Client,
    api_base: &str,
    file_id: &str,
) -> Result<String, String> {
    // Step 1: Get file path from Telegram
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

    // Validate file_path to prevent path traversal / SSRF.
    // Check both raw and percent-decoded forms to catch %2e%2e / %2F etc.
    let is_safe = |p: &str| -> bool {
        !p.contains("..") && !p.starts_with('/') && !p.contains('\0')
            && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    };
    // Percent-decode: replace %XX with actual chars for validation
    let decoded = percent_decode(&file_path);
    if !is_safe(&file_path) || !is_safe(&decoded) {
        return Err("Invalid file_path from Telegram".to_string());
    }

    // Step 2: Download audio bytes with size limit
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

    // Step 3: Transcribe via Whisper API (sends raw audio, format auto-detected)
    // Language defaults to "zh" — TODO: make configurable via agent.toml [voice].asr_language
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

/// Send an audio file as a Telegram audio message (MP3 format from TTS).
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

    match client
        .post(format!("{api_base}/sendAudio"))
        .multipart(form)
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(data) = resp.json::<TgResponse<serde_json::Value>>().await
                && !data.ok
            {
                error!("Telegram sendAudio failed: {}", data.description.unwrap_or_default());
            }
        }
        Err(e) => {
            error!("Telegram sendAudio error: {e}");
        }
    }
}

async fn send_reply(client: &reqwest::Client, api_base: &str, chat_id: i64, text: &str) {
    let body = SendMessage {
        chat_id,
        text: text.to_string(),
        parse_mode: Some("Markdown".to_string()),
    };

    match client
        .post(format!("{api_base}/sendMessage"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(data) = resp.json::<TgResponse<serde_json::Value>>().await
                && !data.ok
            {
                error!("Telegram send failed: {}", data.description.unwrap_or_default());
            }
        }
        Err(e) => {
            error!("Telegram send error: {e}");
        }
    }
}
