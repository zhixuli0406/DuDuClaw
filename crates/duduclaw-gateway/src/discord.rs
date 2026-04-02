//! Discord Bot integration with Gateway WebSocket for receiving messages.
//!
//! Connects to Discord Gateway (wss://gateway.discord.gg) to receive
//! MESSAGE_CREATE events and replies via REST API.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::channel_reply::{ReplyContext, build_reply_with_progress, set_channel_connected};

const DISCORD_API: &str = "https://discord.com/api/v10";

// ── Discord API types ───────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DiscordUser {
    username: Option<String>,
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GatewayInfo {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u8,
    d: Option<Value>,
    s: Option<u64>,
    t: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewayIdentify {
    op: u8,
    d: IdentifyData,
}

#[derive(Debug, Serialize)]
struct IdentifyData {
    token: String,
    intents: u64,
    properties: IdentifyProperties,
}

#[derive(Debug, Serialize)]
struct IdentifyProperties {
    os: String,
    browser: String,
    device: String,
}

// Discord Gateway intents
const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
const INTENT_MESSAGE_CONTENT: u64 = 1 << 15;
const INTENT_DIRECT_MESSAGES: u64 = 1 << 12;

// ── Public API ──────────────────────────────────────────────

/// Start the Discord bot with Gateway WebSocket for receiving messages.
///
/// Returns a JoinHandle for the background task, or None if not configured.
pub async fn start_discord_bot(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<tokio::task::JoinHandle<()>> {
    let token = read_discord_token(home_dir).await?;
    if token.is_empty() {
        return None;
    }

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()?;

    let channel_status = ctx.channel_status.clone();

    // Verify token + get bot info
    let bot_id = match http
        .get(format!("{DISCORD_API}/users/@me"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(user) = resp.json::<DiscordUser>().await {
                let name = user.username.as_deref().unwrap_or("unknown");
                let id = user.id.clone().unwrap_or_default();
                info!("🎮 Discord bot connected: {name} ({id})");
                id
            } else {
                String::new()
            }
        }
        Ok(resp) => {
            let msg = format!("token invalid (HTTP {})", resp.status());
            warn!("Discord bot {msg}");
            set_channel_connected(&channel_status, "discord", false, Some(msg)).await;
            return None;
        }
        Err(e) => {
            warn!("Discord connection failed: {e}");
            set_channel_connected(&channel_status, "discord", false, Some(e.to_string())).await;
            return None;
        }
    };

    // Get Gateway URL (use /gateway/bot for proper bot auth)
    let gateway_url = match http
        .get(format!("{DISCORD_API}/gateway/bot"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(info) = resp.json::<GatewayInfo>().await {
                info.url.unwrap_or_else(|| "wss://gateway.discord.gg".to_string())
            } else {
                "wss://gateway.discord.gg".to_string()
            }
        }
        Err(_) => "wss://gateway.discord.gg".to_string(),
    };

    let gateway_url = format!("{gateway_url}/?v=10&encoding=json");
    info!("   Discord Gateway: {gateway_url}");
    info!("   ⚠ 請確認 Discord Developer Portal 已啟用 MESSAGE CONTENT Intent");

    let handle = tokio::spawn(async move {
        gateway_loop(token, bot_id, gateway_url, http, ctx).await;
    });

    Some(handle)
}

// ── Gateway loop ────────────────────────────────────────────

async fn gateway_loop(
    token: String,
    bot_id: String,
    gateway_url: String,
    http: reqwest::Client,
    ctx: Arc<ReplyContext>,
) {
    let channel_status = ctx.channel_status.clone();

    loop {
        info!("Discord Gateway connecting...");
        set_channel_connected(&channel_status, "discord", false, Some("connecting".into())).await;

        let ws = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(&gateway_url),
        ).await {
            Ok(Ok((ws, resp))) => {
                info!("Discord Gateway WebSocket connected (HTTP {})", resp.status());
                ws
            }
            Ok(Err(e)) => {
                warn!("Discord Gateway connection failed: {e}");
                set_channel_connected(&channel_status, "discord", false, Some(e.to_string())).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
            Err(_) => {
                warn!("Discord Gateway connection timeout (15s)");
                set_channel_connected(&channel_status, "discord", false, Some("Connection timeout".into())).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let (mut write, mut read) = ws.split();
        // Use AtomicU64 so the heartbeat task always reads the latest sequence number.
        // Encode None as u64::MAX since Discord sequence numbers start at 0.
        let sequence = Arc::new(AtomicU64::new(u64::MAX));
        let mut heartbeat_interval_ms: u64 = 41250;
        let mut identified = false;

        // Channel for heartbeat timer to signal "time to send heartbeat"
        let (heartbeat_tx, mut heartbeat_rx) = tokio::sync::mpsc::channel::<()>(1);
        let heartbeat_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        loop {
            tokio::select! {
                // ── Incoming Gateway events ─────────────────
                msg_opt = read.next() => {
                    let msg = match msg_opt {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Close(_))) => { warn!("Discord Gateway closed"); break; }
                        Some(Err(e)) => { warn!("Discord Gateway error: {e}"); break; }
                        None => break,
                        _ => continue,
                    };

                    let payload: GatewayPayload = match serde_json::from_str(&msg) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };

                    if let Some(s) = payload.s {
                        sequence.store(s, Ordering::Relaxed);
                    }

                    match payload.op {
                        // Hello — start heartbeating
                        10 => {
                            if let Some(d) = &payload.d {
                                heartbeat_interval_ms = d
                                    .get("heartbeat_interval")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(41250);
                            }

                            // Start heartbeat timer task (only signals when to send)
                            let interval = std::time::Duration::from_millis(heartbeat_interval_ms);
                            let tx = heartbeat_tx.clone();
                            let hb_handle = tokio::spawn(async move {
                                loop {
                                    tokio::time::sleep(interval).await;
                                    if tx.send(()).await.is_err() {
                                        break;
                                    }
                                }
                            });

                            let mut guard = heartbeat_handle.lock().await;
                            *guard = Some(hb_handle);

                            // Send Identify
                            let identify = GatewayIdentify {
                                op: 2,
                                d: IdentifyData {
                                    token: token.clone(),
                                    intents: INTENT_GUILD_MESSAGES | INTENT_MESSAGE_CONTENT | INTENT_DIRECT_MESSAGES,
                                    properties: IdentifyProperties {
                                        os: "linux".to_string(),
                                        browser: "duduclaw".to_string(),
                                        device: "duduclaw".to_string(),
                                    },
                                },
                            };
                            let json = serde_json::to_string(&identify).unwrap_or_default();
                            if write.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                            identified = true;
                            info!("Discord Gateway identified (heartbeat: {heartbeat_interval_ms}ms)");
                        }

                        // Heartbeat ACK
                        11 => {}

                        // Dispatch (events)
                        0 => {
                            if let Some(event_name) = &payload.t {
                                if event_name == "MESSAGE_CREATE" {
                                    if let Some(d) = &payload.d {
                                        handle_message_create(d, &bot_id, &http, &token, &ctx).await;
                                    }
                                } else if event_name == "READY" {
                                    info!("Discord Gateway READY");
                                    set_channel_connected(&channel_status, "discord", true, None).await;
                                }
                            }
                        }

                        // Reconnect
                        7 => { info!("Discord Gateway requested reconnect"); break; }

                        // Invalid Session
                        9 => {
                            warn!("Discord Gateway invalid session");
                            set_channel_connected(&channel_status, "discord", false, Some("invalid session".to_string())).await;
                            identified = false;
                            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            break;
                        }

                        _ => {}
                    }
                }

                // ── Heartbeat — read the latest sequence from the shared atomic ──
                Some(()) = heartbeat_rx.recv() => {
                    let seq_val = sequence.load(Ordering::Relaxed);
                    let seq_json: Value = if seq_val == u64::MAX {
                        Value::Null
                    } else {
                        Value::Number(seq_val.into())
                    };
                    let hb = json!({ "op": 1, "d": seq_json });
                    if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                        break;
                    }
                }
            }
        }

        // Cleanup heartbeat
        let mut guard = heartbeat_handle.lock().await;
        if let Some(h) = guard.take() {
            h.abort();
        }
        drop(guard);

        let _ = identified;
        set_channel_connected(&channel_status, "discord", false, Some("reconnecting".to_string())).await;
        warn!("Discord Gateway disconnected, reconnecting in 5s...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn handle_message_create(
    data: &Value,
    bot_id: &str,
    http: &reqwest::Client,
    token: &str,
    ctx: &Arc<ReplyContext>,
) {
    // Ignore messages from the bot itself
    let author_id = data
        .get("author")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if author_id == bot_id {
        return;
    }

    let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.is_empty() {
        return;
    }

    let channel_id = data
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let author_name = data
        .get("author")
        .and_then(|a| a.get("username"))
        .and_then(|v| v.as_str())
        .unwrap_or("someone");

    info!("📩 Discord [{author_name}]: {}", &content[..content.len().min(80)]);

    // Progress callback: send keepalive/tool-use messages to the same channel.
    // Debounce at 30s to avoid flooding.
    let progress_http = http.clone();
    let progress_token = token.to_string();
    let progress_channel = channel_id.to_string();
    let last_progress = Arc::new(std::sync::Mutex::new(std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(60))
        .unwrap_or_else(std::time::Instant::now)));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        let mut last = last_progress.lock().unwrap();
        if last.elapsed().as_secs() < 30 {
            return;
        }
        *last = std::time::Instant::now();
        drop(last);

        let msg_text = event.to_display();
        let c = progress_http.clone();
        let t = progress_token.clone();
        let ch = progress_channel.clone();
        tokio::spawn(async move {
            let _ = c
                .post(format!("{DISCORD_API}/channels/{ch}/messages"))
                .header("Authorization", format!("Bot {t}"))
                .json(&json!({ "content": msg_text }))
                .send()
                .await;
        });
    });

    let reply = build_reply_with_progress(content, ctx, Some(on_progress)).await;

    // Send final reply via REST API
    match http
        .post(format!("{DISCORD_API}/channels/{channel_id}/messages"))
        .header("Authorization", format!("Bot {token}"))
        .json(&json!({ "content": reply }))
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            error!("Discord send failed ({})", resp.status());
        }
        Err(e) => error!("Discord send error: {e}"),
        _ => {}
    }
}

// ── Config ──────────────────────────────────────────────────

async fn read_discord_token(home_dir: &Path) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "discord_bot_token").await
}
