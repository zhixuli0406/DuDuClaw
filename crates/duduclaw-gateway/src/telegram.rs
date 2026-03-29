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
struct TgMessage {
    text: Option<String>,
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

                if let Some(msg) = update.message
                    && let Some(text) = &msg.text
                {
                    let sender = msg
                        .from
                        .as_ref()
                        .and_then(|u| u.first_name.as_deref())
                        .unwrap_or("someone");

                    info!("📩 Telegram [{sender}]: {}", &text[..text.len().min(80)]);

                    // Progress callback: sends keepalive/tool-use messages to the chat.
                    // Uses debounce (min 30s between sends) to respect Telegram rate limits.
                    let progress_client = client.clone();
                    let progress_api = api_base.clone();
                    let progress_chat_id = msg.chat.id;
                    let last_progress = Arc::new(std::sync::Mutex::new(std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(60))
                        .unwrap_or_else(std::time::Instant::now)));
                    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
                        // Debounce: skip if last progress was < 30s ago
                        let mut last = last_progress.lock().unwrap();
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

                    let reply = build_reply_with_progress(text, &ctx, Some(on_progress)).await;
                    send_reply(&client, &api_base, msg.chat.id, &reply).await;
                }
            }
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
