//! Discord Bot integration with Gateway WebSocket.
//!
//! Full-featured Discord experience:
//! - Gateway WebSocket for MESSAGE_CREATE, INTERACTION_CREATE events
//! - Slash Commands (/ask, /status, /config, /session, /agent)
//! - Embed replies with DuDuClaw branding
//! - Auto-thread creation for conversations
//! - Per-guild settings (mention_only, channel whitelist, auto_thread)
//! - Message splitting for 2000 char Discord limit
//! - Typing indicator during AI processing

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::channel_format::{self, split_text};
use crate::channel_reply::{ReplyContext, build_reply_for_agent, build_reply_with_session, set_channel_connected};
use crate::channel_settings::keys;

const DISCORD_API: &str = "https://discord.com/api/v10";

// ── Discord API types ───────────────────────────────────────

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
const INTENT_GUILDS: u64 = 1 << 0;
const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
const INTENT_GUILD_MESSAGE_TYPING: u64 = 1 << 11;
const INTENT_DIRECT_MESSAGES: u64 = 1 << 12;
const INTENT_MESSAGE_CONTENT: u64 = 1 << 15;

/// RAII guard that stops the typing indicator on drop (including panic paths).
struct TypingGuard {
    flag: Arc<std::sync::atomic::AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TypingGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
        self.handle.abort();
    }
}

/// Combined intents for full Discord experience.
const BOT_INTENTS: u64 = INTENT_GUILDS
    | INTENT_GUILD_MESSAGES
    | INTENT_GUILD_MESSAGE_TYPING
    | INTENT_DIRECT_MESSAGES
    | INTENT_MESSAGE_CONTENT;

// ── Slash command definitions ───────────────────────────────

fn slash_command_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ask",
            "description": "Ask DuDuClaw AI a question",
            "type": 1,
            "options": [{
                "name": "prompt",
                "description": "Your question or prompt",
                "type": 3,
                "required": true
            }]
        }),
        json!({
            "name": "status",
            "description": "Show DuDuClaw bot status",
            "type": 1
        }),
        json!({
            "name": "config",
            "description": "Configure DuDuClaw settings for this server",
            "type": 1,
            "default_member_permissions": "32", // MANAGE_GUILD
            "options": [
                {
                    "name": "mention_only",
                    "description": "Only respond when @mentioned",
                    "type": 1, // SUB_COMMAND
                    "options": [{
                        "name": "enabled",
                        "description": "Enable or disable mention-only mode",
                        "type": 5, // BOOLEAN
                        "required": true
                    }]
                },
                {
                    "name": "auto_thread",
                    "description": "Auto-create threads for conversations",
                    "type": 1,
                    "options": [{
                        "name": "enabled",
                        "description": "Enable or disable auto-thread",
                        "type": 5,
                        "required": true
                    }]
                },
                {
                    "name": "show",
                    "description": "Show current settings",
                    "type": 1
                }
            ]
        }),
        json!({
            "name": "session",
            "description": "Manage conversation session",
            "type": 1,
            "options": [
                {
                    "name": "info",
                    "description": "Show current session info",
                    "type": 1
                },
                {
                    "name": "reset",
                    "description": "Clear current session",
                    "type": 1
                }
            ]
        }),
        json!({
            "name": "agent",
            "description": "Switch active agent",
            "type": 1,
            "options": [{
                "name": "name",
                "description": "Agent name to switch to",
                "type": 3,
                "required": true
            }]
        }),
    ]
}

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
    let event_tx = ctx.event_tx.clone();

    // Verify token + get bot user info
    let bot_id = match http
        .get(format!("{DISCORD_API}/users/@me"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<Value>().await {
                let name = data["username"].as_str().unwrap_or("unknown");
                let id = data["id"].as_str().unwrap_or("").to_string();
                info!("🎮 Discord bot connected: {name} ({id})");
                id
            } else {
                String::new()
            }
        }
        Ok(resp) => {
            let msg = format!("token invalid (HTTP {})", resp.status());
            warn!("Discord bot {msg}");
            set_channel_connected(&channel_status, "discord", false, Some(msg), Some(&event_tx)).await;
            return None;
        }
        Err(e) => {
            warn!("Discord connection failed: {e}");
            set_channel_connected(&channel_status, "discord", false, Some(e.to_string()), Some(&event_tx)).await;
            return None;
        }
    };

    // Get application ID via /applications/@me (authoritative source)
    let app_id = match http
        .get(format!("{DISCORD_API}/applications/@me"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<Value>().await {
                data["id"].as_str().unwrap_or("").to_string()
            } else {
                bot_id.clone() // Fallback
            }
        }
        _ => {
            info!("Discord: /applications/@me unavailable, using bot_id as app_id fallback");
            bot_id.clone()
        }
    };

    // Register global slash commands
    register_slash_commands(&http, &token, &app_id).await;

    // Get Gateway URL
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
        gateway_loop(token, bot_id, app_id, gateway_url, http, ctx, "discord".to_string(), None).await;
    });

    Some(handle)
}

/// Start multiple Discord bots: one global (from config.toml) plus per-agent bots.
///
/// Returns a Vec of (label, JoinHandle) where label is "discord" for the global
/// bot and "discord:{agent_name}" for per-agent bots.
/// Deduplicates by token value — if an agent token matches the global token, it
/// is skipped (the global bot already covers it).
pub async fn start_discord_bots(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Vec<(String, tokio::task::JoinHandle<()>)> {
    let mut results: Vec<(String, tokio::task::JoinHandle<()>)> = Vec::new();
    let mut seen_tokens: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Global bot from config.toml
    if let Some(token) = read_discord_token(home_dir).await {
        if !token.is_empty() {
            seen_tokens.insert(token.clone());
            if let Some(handle) = spawn_discord_bot(token, "discord".to_string(), None, ctx.clone(), home_dir).await {
                results.push(("discord".to_string(), handle));
            }
        }
    }

    // 2. Per-agent bots from agent configs
    let agent_tokens: Vec<(String, String)> = {
        let reg = ctx.registry.read().await;
        let mut tokens = Vec::new();
        for agent in reg.list() {
            if let Some(channels) = &agent.config.channels {
                if let Some(discord) = &channels.discord {
                    let token = if let Some(enc) = &discord.bot_token_enc {
                        if !enc.is_empty() {
                            crate::config_crypto::decrypt_value(enc, home_dir)
                                .unwrap_or_default()
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };
                    let token = if token.is_empty() {
                        discord.bot_token.clone()
                    } else {
                        token
                    };
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
            info!("Discord bot for agent '{agent_name}' shares global token — skipping duplicate");
            continue;
        }
        seen_tokens.insert(token.clone());
        let label = format!("discord:{agent_name}");
        if let Some(handle) = spawn_discord_bot(token, label.clone(), Some(agent_name), ctx.clone(), home_dir).await {
            results.push((label, handle));
        }
    }

    results
}

/// Spawn a single Discord bot connection (shared by global and per-agent paths).
async fn spawn_discord_bot(
    token: String,
    label: String,
    agent_name: Option<String>,
    ctx: Arc<ReplyContext>,
    home_dir: &Path,
) -> Option<tokio::task::JoinHandle<()>> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()?;

    let channel_status = ctx.channel_status.clone();
    let event_tx = ctx.event_tx.clone();

    // Verify token + get bot user info
    let bot_id = match http
        .get(format!("{DISCORD_API}/users/@me"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<Value>().await {
                let name = data["username"].as_str().unwrap_or("unknown");
                let id = data["id"].as_str().unwrap_or("").to_string();
                info!("🎮 Discord bot [{label}] connected: {name} ({id})");
                id
            } else {
                return None;
            }
        }
        Ok(resp) => {
            warn!("Discord bot [{label}] token invalid (HTTP {})", resp.status());
            set_channel_connected(&channel_status, &label, false, Some("token invalid".into()), Some(&event_tx)).await;
            return None;
        }
        Err(e) => {
            warn!("Discord [{label}] connection failed: {e}");
            set_channel_connected(&channel_status, &label, false, Some(e.to_string()), Some(&event_tx)).await;
            return None;
        }
    };

    // Get application ID
    let app_id = match http
        .get(format!("{DISCORD_API}/applications/@me"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<Value>().await {
                data["id"].as_str().unwrap_or("").to_string()
            } else {
                bot_id.clone()
            }
        }
        _ => bot_id.clone(),
    };

    // Only register slash commands for the global bot
    if agent_name.is_none() {
        register_slash_commands(&http, &token, &app_id).await;
    }

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
    info!("   Discord [{label}] Gateway: {gateway_url}");

    let handle = tokio::spawn(async move {
        gateway_loop(token, bot_id, app_id, gateway_url, http, ctx, label, agent_name).await;
    });

    Some(handle)
}

/// Register global slash commands with Discord.
async fn register_slash_commands(http: &reqwest::Client, token: &str, app_id: &str) {
    if app_id.is_empty() {
        warn!("Discord: cannot register slash commands — app_id unknown");
        return;
    }

    let commands = slash_command_definitions();
    let url = format!("{DISCORD_API}/applications/{app_id}/commands");

    match http
        .put(&url)
        .header("Authorization", format!("Bot {token}"))
        .json(&commands)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            info!("Discord: registered {} slash commands", commands.len());
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("Discord: slash command registration failed ({status}): {}", &body[..body.len().min(200)]);
        }
        Err(e) => {
            warn!("Discord: slash command registration error: {e}");
        }
    }
}

// ── Gateway loop ────────────────────────────────────────────

/// Concurrency limit for message/interaction handlers.
static HANDLER_SEMAPHORE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(10));

async fn gateway_loop(
    token: String,
    bot_id: String,
    app_id: String,
    gateway_url: String,
    http: reqwest::Client,
    ctx: Arc<ReplyContext>,
    label: String,
    agent_name: Option<String>,
) {
    let channel_status = ctx.channel_status.clone();
    let event_tx = ctx.event_tx.clone();
    let mut consecutive_failures: u32 = 0;
    const MAX_FAILURES: u32 = 10;

    loop {
        // Exponential backoff: 5s, 10s, 20s, 40s, ... capped at 300s (5min)
        if consecutive_failures > 0 {
            let backoff = std::cmp::min(5u64 << consecutive_failures.min(6), 300);
            warn!("Discord [{label}] reconnecting in {backoff}s (attempt {consecutive_failures}/{MAX_FAILURES})");
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        }

        // Re-verify token before reconnecting to avoid hammering Discord
        if consecutive_failures >= 2 {
            match http.get(format!("{DISCORD_API}/users/@me"))
                .header("Authorization", format!("Bot {token}"))
                .send().await
            {
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    error!("Discord [{label}] token is invalid (401), stopping bot");
                    set_channel_connected(&channel_status, &label, false, Some("token invalid — update via Dashboard".into()), Some(&event_tx)).await;
                    return;
                }
                Ok(resp) if resp.status().as_u16() == 429 => {
                    warn!("Discord [{label}] rate limited during token check, waiting 60s");
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    continue;
                }
                Err(_) => {} // network error, proceed to try gateway anyway
                _ => {} // token ok
            }
        }

        if consecutive_failures >= MAX_FAILURES {
            error!("Discord [{label}] {MAX_FAILURES} consecutive failures, stopping bot");
            set_channel_connected(&channel_status, &label, false, Some(format!("stopped after {MAX_FAILURES} failures — check token")), Some(&event_tx)).await;
            return;
        }

        info!("Discord [{label}] Gateway connecting...");
        set_channel_connected(&channel_status, &label, false, Some("connecting".into()), Some(&event_tx)).await;

        let ws = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(&gateway_url),
        ).await {
            Ok(Ok((ws, resp))) => {
                info!("Discord Gateway WebSocket connected (HTTP {})", resp.status());
                ws
            }
            Ok(Err(e)) => {
                warn!("Discord [{label}] Gateway connection failed: {e}");
                set_channel_connected(&channel_status, &label, false, Some(e.to_string()), Some(&event_tx)).await;
                consecutive_failures += 1;
                continue;
            }
            Err(_) => {
                warn!("Discord [{label}] Gateway connection timeout (15s)");
                set_channel_connected(&channel_status, &label, false, Some("Connection timeout".into()), Some(&event_tx)).await;
                consecutive_failures += 1;
                continue;
            }
        };

        let (mut write, mut read) = ws.split();
        let sequence = Arc::new(AtomicU64::new(u64::MAX));
        let mut heartbeat_interval_ms: u64 = 41250;
        let mut _identified = false;

        let (heartbeat_tx, mut heartbeat_rx) = tokio::sync::mpsc::channel::<()>(1);
        let heartbeat_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        loop {
            tokio::select! {
                msg_opt = read.next() => {
                    let msg = match msg_opt {
                        Some(Ok(Message::Text(text))) => text.to_string(),
                        Some(Ok(Message::Binary(bin))) => {
                            match String::from_utf8(bin.to_vec()) {
                                Ok(text) => text,
                                Err(_) => {
                                    warn!("Discord Gateway: received non-UTF8 binary frame ({} bytes)", bin.len());
                                    continue;
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            if let Err(e) = write.send(Message::Pong(data)).await {
                                warn!("Discord Gateway: failed to send pong: {e}");
                                break;
                            }
                            continue;
                        }
                        Some(Ok(Message::Close(frame))) => {
                            let raw_code = frame.as_ref().map(|f| u16::from(f.code));
                            let reason = frame.as_ref().map(|f| f.reason.to_string()).unwrap_or_default();
                            warn!("Discord [{label}] Gateway closed (code: {raw_code:?}, reason: {reason})");
                            // Fatal Discord close codes — do not retry
                            match raw_code {
                                Some(4004) => {
                                    error!("Discord [{label}] authentication failed (4004), stopping");
                                    set_channel_connected(&channel_status, &label, false, Some("authentication failed — update token via Dashboard".into()), Some(&event_tx)).await;
                                    return;
                                }
                                Some(4014) => {
                                    error!("Discord [{label}] disallowed intents (4014), stopping");
                                    set_channel_connected(&channel_status, &label, false, Some("disallowed intents — enable MESSAGE CONTENT INTENT in Discord Developer Portal".into()), Some(&event_tx)).await;
                                    return;
                                }
                                Some(4013) => {
                                    error!("Discord [{label}] invalid intents (4013), stopping");
                                    set_channel_connected(&channel_status, &label, false, Some("invalid intents".into()), Some(&event_tx)).await;
                                    return;
                                }
                                _ => break, // recoverable, will retry with backoff
                            }
                        }
                        Some(Err(e)) => { warn!("Discord Gateway error: {e}"); break; }
                        None => break,
                        _ => continue,
                    };

                    let payload: GatewayPayload = match serde_json::from_str(&msg) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Discord Gateway: failed to parse payload: {e} (first 200 chars: {})", &msg[..msg.len().min(200)]);
                            continue;
                        }
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
                            // Abort previous heartbeat task to prevent leaking on duplicate op 10
                            if let Some(old) = guard.take() {
                                old.abort();
                            }
                            *guard = Some(hb_handle);

                            let identify = GatewayIdentify {
                                op: 2,
                                d: IdentifyData {
                                    token: token.clone(),
                                    intents: BOT_INTENTS,
                                    properties: IdentifyProperties {
                                        os: "linux".to_string(),
                                        browser: "duduclaw".to_string(),
                                        device: "duduclaw".to_string(),
                                    },
                                },
                            };
                            let json_str = serde_json::to_string(&identify).unwrap_or_default();
                            if write.send(Message::Text(json_str.into())).await.is_err() {
                                break;
                            }
                            _identified = true;
                            info!("Discord Gateway identified (heartbeat: {heartbeat_interval_ms}ms)");
                        }

                        // Heartbeat ACK
                        11 => {}

                        // Dispatch (events)
                        0 => {
                            if let Some(event_name) = &payload.t {
                                let event = event_name.as_str();
                                match event {
                                    "MESSAGE_CREATE" => {
                                        if let Some(d) = payload.d {
                                            let http = http.clone();
                                            let token = token.clone();
                                            let bot_id = bot_id.clone();
                                            let ctx = ctx.clone();
                                            let agent = agent_name.clone();
                                            tokio::spawn(async move {
                                                let _permit = HANDLER_SEMAPHORE.acquire().await;
                                                handle_message_create(&d, &bot_id, &http, &token, &ctx, agent.as_deref()).await;
                                            });
                                        }
                                    }
                                    "INTERACTION_CREATE" => {
                                        if let Some(d) = payload.d {
                                            let http = http.clone();
                                            let token = token.clone();
                                            let bot_id = bot_id.clone();
                                            let app_id = app_id.clone();
                                            let ctx = ctx.clone();
                                            tokio::spawn(async move {
                                                let _permit = HANDLER_SEMAPHORE.acquire().await;
                                                handle_interaction(&d, &bot_id, &app_id, &http, &token, &ctx).await;
                                            });
                                        }
                                    }
                                    "READY" => {
                                        info!("Discord [{label}] Gateway READY");
                                        consecutive_failures = 0;
                                        set_channel_connected(&channel_status, &label, true, None, Some(&event_tx)).await;
                                    }
                                    "GUILD_CREATE" => {
                                        if let Some(d) = &payload.d {
                                            let guild_name = d["name"].as_str().unwrap_or("unknown");
                                            let guild_id = d["id"].as_str().unwrap_or("");
                                            info!("Discord: joined guild '{guild_name}' ({guild_id})");
                                        }
                                    }
                                    _ => {
                                        debug!("Discord event: {event}");
                                    }
                                }
                            }
                        }

                        // Reconnect
                        7 => { info!("Discord Gateway requested reconnect"); break; }

                        // Invalid Session
                        9 => {
                            warn!("Discord [{label}] Gateway invalid session");
                            set_channel_connected(&channel_status, &label, false, Some("invalid session".to_string()), Some(&event_tx)).await;
                            _identified = false;
                            consecutive_failures += 1;
                            break;
                        }

                        _ => {}
                    }
                }

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

        let _ = _identified;
        consecutive_failures += 1;
        set_channel_connected(&channel_status, &label, false, Some("reconnecting".to_string()), Some(&event_tx)).await;
    }
}

// ── Message handling ────────────────────────────────────────

async fn handle_message_create(
    data: &Value,
    bot_id: &str,
    http: &reqwest::Client,
    token: &str,
    ctx: &Arc<ReplyContext>,
    agent_name: Option<&str>,
) {
    // Ignore messages from the bot itself or other bots
    let author = data.get("author");
    let author_id = author.and_then(|a| a["id"].as_str()).unwrap_or("");
    let is_bot = author.and_then(|a| a["bot"].as_bool()).unwrap_or(false);

    if author_id == bot_id || is_bot {
        return;
    }

    let content = data["content"].as_str().unwrap_or("");
    if content.is_empty() {
        return;
    }

    let channel_id = data["channel_id"].as_str().unwrap_or("");
    let guild_id = data["guild_id"].as_str().unwrap_or(""); // empty for DMs
    let message_id = data["id"].as_str().unwrap_or("");
    let author_name = author.and_then(|a| a["username"].as_str()).unwrap_or("someone");
    let user_id = author_id;

    // Check if bot is mentioned
    let mentions = data["mentions"].as_array();
    let bot_mentioned = mentions
        .map(|arr| arr.iter().any(|m| m["id"].as_str() == Some(bot_id)))
        .unwrap_or(false);

    let settings = &ctx.channel_settings;
    let scope_id = if guild_id.is_empty() { "dm" } else { guild_id };

    // ── Mention-only filter ──
    // Per-agent bots default to mention-only in guilds to prevent all bots
    // in the same server from responding to every message.
    let default_mention_only = agent_name.is_some();
    let mention_only = settings.get_bool("discord", scope_id, keys::MENTION_ONLY, default_mention_only).await;
    if mention_only && !guild_id.is_empty() && !bot_mentioned {
        return; // In guild, mention_only enabled, but bot not mentioned → skip
    }

    // ── Channel whitelist ──
    if !guild_id.is_empty() && !settings.is_channel_allowed("discord", scope_id, channel_id).await {
        return;
    }

    // Strip bot mention from content
    let clean_content = strip_bot_mention(content, bot_id);
    let clean_content = clean_content.trim();
    if clean_content.is_empty() {
        return;
    }

    info!("📩 Discord [{author_name}] (guild:{guild_id}): {}", &clean_content[..clean_content.len().min(80)]);

    // ── Auto-thread ──
    // Default to true in guilds so conversations are organized into threads
    let auto_thread_default = !guild_id.is_empty();
    let auto_thread = settings.get_bool("discord", scope_id, keys::AUTO_THREAD, auto_thread_default).await;
    // Detect if message is in a thread: Discord threads have channel_type 11 (PUBLIC_THREAD) or 12 (PRIVATE_THREAD)
    // Note: channel_type is not always present in MESSAGE_CREATE, but the gateway sends it for threads.
    // Fallback: check if thread metadata exists in the payload.
    let channel_type = data["channel_type"].as_u64().unwrap_or(0);
    let is_thread = channel_type == 11 || channel_type == 12
        || data.get("thread").is_some();

    let reply_channel_id = if auto_thread && !is_thread && !guild_id.is_empty() {
        // Create a thread from this message
        match create_thread(http, token, channel_id, message_id, clean_content).await {
            Some(thread_id) => thread_id,
            None => channel_id.to_string(), // Fallback to main channel
        }
    } else {
        channel_id.to_string()
    };

    // ── Typing indicator (RAII guard ensures cleanup on panic/early return) ──
    let typing_guard = {
        let typing_http = http.clone();
        let typing_token = token.to_string();
        let typing_channel = reply_channel_id.clone();
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag_clone = flag.clone();
        let handle = tokio::spawn(async move {
            let mut consecutive_failures = 0u32;
            while flag_clone.load(Ordering::Relaxed) {
                match typing_http
                    .post(format!("{DISCORD_API}/channels/{typing_channel}/typing"))
                    .header("Authorization", format!("Bot {typing_token}"))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().as_u16() == 429 => {
                        // Rate limited — back off and stop
                        warn!("Discord typing rate limited, stopping indicator");
                        break;
                    }
                    Err(_) => {
                        consecutive_failures += 1;
                        if consecutive_failures >= 3 {
                            break; // Stop after 3 consecutive failures
                        }
                    }
                    _ => { consecutive_failures = 0; }
                }
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            }
        });
        TypingGuard { flag, handle }
    };

    // ── Build session ID ──
    let session_id = if auto_thread && !is_thread {
        format!("discord:thread:{reply_channel_id}")
    } else {
        format!("discord:{reply_channel_id}")
    };

    // ── Progress callback (edit-in-place to avoid flooding) ──
    let progress_http = http.clone();
    let progress_token = token.to_string();
    let progress_channel = reply_channel_id.clone();
    let last_progress = Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap_or_else(std::time::Instant::now),
    ));
    // Shared message ID so we can EDIT the same progress message instead of creating new ones
    let progress_msg_id: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let progress_msg_id_cb = progress_msg_id.clone();
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
        let mid = progress_msg_id_cb.clone();
        tokio::spawn(async move {
            let existing_id = mid.lock().unwrap().clone();
            if let Some(msg_id) = existing_id {
                // Edit the existing progress message
                let _ = c
                    .patch(format!("{DISCORD_API}/channels/{ch}/messages/{msg_id}"))
                    .header("Authorization", format!("Bot {t}"))
                    .json(&json!({ "content": msg_text }))
                    .send()
                    .await;
            } else {
                // Send the first progress message and save its ID
                let resp = c
                    .post(format!("{DISCORD_API}/channels/{ch}/messages"))
                    .header("Authorization", format!("Bot {t}"))
                    .json(&json!({ "content": msg_text }))
                    .send()
                    .await;
                if let Ok(r) = resp {
                    if let Ok(body) = r.json::<serde_json::Value>().await {
                        if let Some(id) = body["id"].as_str() {
                            *mid.lock().unwrap() = Some(id.to_string());
                        }
                    }
                }
            }
        });
    });
    let cleanup_http = http.clone();
    let cleanup_token = token.to_string();
    let cleanup_channel = reply_channel_id.clone();

    // ── Get agent display name for embed footer ──
    let display_name = {
        let reg = ctx.registry.read().await;
        reg.main_agent().map(|a| a.config.agent.display_name.clone())
    };

    let reply = if let Some(agent) = agent_name {
        build_reply_for_agent(clean_content, ctx, agent, &session_id, user_id, Some(on_progress)).await
    } else {
        build_reply_with_session(clean_content, ctx, &session_id, user_id, Some(on_progress)).await
    };

    // Stop typing (explicit drop; also runs automatically on panic via Drop)
    drop(typing_guard);

    // ── Send reply with embed + buttons ──
    let mut payload = channel_format::to_discord_message(&reply, display_name.as_deref(), false);

    // Reply to the original message so the sender gets a notification
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("message_reference".to_string(), json!({
            "message_id": message_id,
            "channel_id": reply_channel_id,
        }));
    }

    // Add conversation buttons
    let buttons = channel_format::discord_conversation_buttons(&session_id);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("components".to_string(), json!([buttons]));
    }

    // ── Delete progress message now that we have the real reply ──
    if let Some(pmid) = progress_msg_id.lock().unwrap().take() {
        let c = cleanup_http;
        let t = cleanup_token;
        let ch = cleanup_channel;
        tokio::spawn(async move {
            let _ = c
                .delete(format!("{DISCORD_API}/channels/{ch}/messages/{pmid}"))
                .header("Authorization", format!("Bot {t}"))
                .send()
                .await;
        });
    }

    // ── Split if needed (embed description > 4096 or plain text > 2000) ──
    send_discord_message(http, token, &reply_channel_id, payload).await;
}

/// Strip `<@BOT_ID>` mentions from message content.
fn strip_bot_mention(text: &str, bot_id: &str) -> String {
    text.replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "") // Nickname mention variant
        .trim()
        .to_string()
}

/// Create a thread from a message. Returns the thread channel_id.
async fn create_thread(
    http: &reqwest::Client,
    token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
) -> Option<String> {
    // Thread name: first 97 chars, filter control characters (safe for CJK multi-byte)
    let name: String = content.chars()
        .filter(|c| !c.is_control())
        .take(97)
        .collect();
    let name = if content.chars().filter(|c| !c.is_control()).count() > 97 {
        format!("{name}...")
    } else {
        name
    };

    let resp = http
        .post(format!("{DISCORD_API}/channels/{channel_id}/messages/{message_id}/threads"))
        .header("Authorization", format!("Bot {token}"))
        .json(&json!({
            "name": name,
            "auto_archive_duration": 1440 // 24 hours
        }))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!("Discord: failed to create thread ({status}): {}", &body[..body.len().min(200)]);
        return None;
    }

    let data: Value = resp.json().await.ok()?;
    let thread_id = data["id"].as_str()?.to_string();
    info!("Discord: created thread {thread_id}");
    Some(thread_id)
}

/// Send a message to a Discord channel, handling 2000 char limit.
async fn send_discord_message(http: &reqwest::Client, token: &str, channel_id: &str, payload: Value) {
    // Check if the payload has plain content that needs splitting
    if let Some(content) = payload["content"].as_str() {
        if content.len() > channel_format::limits::DISCORD_MESSAGE {
            let chunks = split_text(content, channel_format::limits::DISCORD_MESSAGE - 100);
            for chunk in chunks.iter() {
                let msg = json!({ "content": chunk });
                send_raw(http, token, channel_id, &msg).await;
            }
            return;
        }
    }

    send_raw(http, token, channel_id, &payload).await;
}

async fn send_raw(http: &reqwest::Client, token: &str, channel_id: &str, payload: &Value) {
    // Strip any `components` (buttons) from the payload — DuDuClaw does not
    // handle Discord button interactions, so sending them only confuses users.
    let cleaned = if payload.get("components").is_some() {
        let mut p = payload.clone();
        p.as_object_mut().map(|m| m.remove("components"));
        p
    } else {
        payload.clone()
    };

    match http
        .post(format!("{DISCORD_API}/channels/{channel_id}/messages"))
        .header("Authorization", format!("Bot {token}"))
        .json(&cleaned)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Discord send failed ({status}): {}", &body[..body.len().min(200)]);
        }
        Err(e) => error!("Discord send error: {e}"),
        _ => {}
    }
}

// ── Interaction handling (Slash Commands + Buttons) ──────────

async fn handle_interaction(
    data: &Value,
    bot_id: &str,
    app_id: &str,
    http: &reqwest::Client,
    token: &str,
    ctx: &Arc<ReplyContext>,
) {
    let interaction_type = data["type"].as_u64().unwrap_or(0);
    let interaction_id = data["id"].as_str().unwrap_or("");
    let interaction_token = data["token"].as_str().unwrap_or("");

    match interaction_type {
        // Application Command (slash command)
        2 => {
            handle_slash_command(data, interaction_id, interaction_token, bot_id, app_id, http, token, ctx).await;
        }
        _ => {
            debug!("Discord: unhandled interaction type {interaction_type}");
        }
    }
}

/// Check if the member has MANAGE_GUILD permission (bit 5).
fn has_manage_guild_permission(data: &Value) -> bool {
    const MANAGE_GUILD: u64 = 1 << 5;
    data["member"]["permissions"]
        .as_str()
        .and_then(|p| p.parse::<u64>().ok())
        .map(|p| p & MANAGE_GUILD != 0)
        .unwrap_or(false)
}

async fn handle_slash_command(
    data: &Value,
    interaction_id: &str,
    interaction_token: &str,
    _bot_id: &str,
    app_id: &str,
    http: &reqwest::Client,
    _bot_token: &str,
    ctx: &Arc<ReplyContext>,
) {
    let cmd_data = match data.get("data") {
        Some(d) => d,
        None => return,
    };
    let cmd_name = cmd_data["name"].as_str().unwrap_or("");
    let guild_id = data["guild_id"].as_str().unwrap_or("");
    let channel_id = data["channel_id"].as_str().unwrap_or("");
    let user = data.get("member")
        .and_then(|m| m.get("user"))
        .or_else(|| data.get("user"));
    let user_id = user.and_then(|u| u["id"].as_str()).unwrap_or("unknown");
    let username = user.and_then(|u| u["username"].as_str()).unwrap_or("someone");

    info!("Discord /{cmd_name} from [{username}] guild:{guild_id}");

    match cmd_name {
        "ask" => {
            // Deferred response (type 5) — we'll edit it later
            send_interaction_response(http, interaction_id, interaction_token, 5, None).await;

            let prompt = cmd_data["options"]
                .as_array()
                .and_then(|opts| opts.first())
                .and_then(|o| o["value"].as_str())
                .unwrap_or("");

            let session_id = format!("discord:{channel_id}");
            let reply = build_reply_with_session(prompt, ctx, &session_id, user_id, None).await;

            let agent_name = {
                let reg = ctx.registry.read().await;
                reg.main_agent().map(|a| a.config.agent.display_name.clone())
            };

            let payload = channel_format::to_discord_message(&reply, agent_name.as_deref(), false);
            edit_interaction_response(http, app_id, interaction_token, &payload).await;
        }

        "status" => {
            let agent_info = {
                let reg = ctx.registry.read().await;
                reg.main_agent().map(|a| {
                    format!("**Agent**: {} ({})\n**Model**: {}",
                        a.config.agent.display_name,
                        a.config.agent.name,
                        a.config.model.preferred)
                }).unwrap_or_else(|| "No agent configured".to_string())
            };

            let settings = &ctx.channel_settings;
            let scope = if guild_id.is_empty() { "dm" } else { guild_id };
            let mention_only = settings.get_bool("discord", scope, keys::MENTION_ONLY, false).await;
            let auto_thread = settings.get_bool("discord", scope, keys::AUTO_THREAD, false).await;

            let status_text = format!(
                "{agent_info}\n\n**Guild Settings**:\n\
                 Mention Only: {}\n\
                 Auto Thread: {}",
                if mention_only { "✅" } else { "❌" },
                if auto_thread { "✅" } else { "❌" },
            );

            let embed = json!({
                "embeds": [{
                    "title": "DuDuClaw Status",
                    "description": status_text,
                    "color": 0xF59E0B,
                    "footer": { "text": "DuDuClaw" }
                }]
            });
            send_interaction_response(http, interaction_id, interaction_token, 4, Some(embed)).await;
        }

        "config" => {
            // DMs cannot modify config (would affect global scope)
            if guild_id.is_empty() {
                send_interaction_response(http, interaction_id, interaction_token, 4,
                    Some(json!({"content": "❌ /config 只能在伺服器中使用", "flags": 64}))).await;
                return;
            }
            // Server-side permission check: require MANAGE_GUILD
            if !has_manage_guild_permission(data) {
                send_interaction_response(http, interaction_id, interaction_token, 4,
                    Some(json!({"content": "❌ 需要「管理伺服器」權限才能修改設定", "flags": 64}))).await;
                return;
            }

            let sub = cmd_data["options"]
                .as_array()
                .and_then(|opts| opts.first());

            let sub_name = sub.and_then(|s| s["name"].as_str()).unwrap_or("");
            let scope = if guild_id.is_empty() { "global" } else { guild_id };

            match sub_name {
                "mention_only" => {
                    let enabled = sub
                        .and_then(|s| s["options"].as_array())
                        .and_then(|opts| opts.first())
                        .and_then(|o| o["value"].as_bool())
                        .unwrap_or(false);

                    let _ = ctx.channel_settings.set("discord", scope, keys::MENTION_ONLY, if enabled { "true" } else { "false" }).await;

                    let msg = format!("Mention-only mode: **{}**", if enabled { "Enabled ✅" } else { "Disabled ❌" });
                    send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": msg, "flags": 64}))).await;
                }
                "auto_thread" => {
                    let enabled = sub
                        .and_then(|s| s["options"].as_array())
                        .and_then(|opts| opts.first())
                        .and_then(|o| o["value"].as_bool())
                        .unwrap_or(false);

                    let _ = ctx.channel_settings.set("discord", scope, keys::AUTO_THREAD, if enabled { "true" } else { "false" }).await;

                    let msg = format!("Auto-thread mode: **{}**", if enabled { "Enabled ✅" } else { "Disabled ❌" });
                    send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": msg, "flags": 64}))).await;
                }
                "show" => {
                    let all = ctx.channel_settings.get_all("discord", scope).await;
                    let text = if all.is_empty() {
                        "No custom settings configured. Using defaults.".to_string()
                    } else {
                        all.iter().map(|(k, v)| format!("`{k}`: {v}")).collect::<Vec<_>>().join("\n")
                    };
                    send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": text, "flags": 64}))).await;
                }
                _ => {
                    send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": "Unknown subcommand", "flags": 64}))).await;
                }
            }
        }

        "session" => {
            let sub_name = cmd_data["options"]
                .as_array()
                .and_then(|opts| opts.first())
                .and_then(|s| s["name"].as_str())
                .unwrap_or("info");

            let session_id = format!("discord:{channel_id}");

            match sub_name {
                "info" => {
                    let info = match ctx.session_manager.get_or_create(&session_id, "main").await {
                        Ok(s) => format!(
                            "**Session**: `{}`\n**Tokens**: {}\n**Last Active**: {}",
                            s.id, s.total_tokens, s.last_active
                        ),
                        Err(_) => "No active session.".to_string(),
                    };
                    send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": info, "flags": 64}))).await;
                }
                "reset" => {
                    let msg = match ctx.session_manager.delete_session(&session_id).await {
                        Ok(()) => format!("✅ Session `{session_id}` cleared."),
                        Err(e) => format!("⚠️ Failed to clear session: {e}"),
                    };
                    send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": msg, "flags": 64}))).await;
                }
                _ => {
                    send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": "Unknown subcommand", "flags": 64}))).await;
                }
            }
        }

        "agent" => {
            // DMs cannot switch agent (would affect global scope)
            if guild_id.is_empty() {
                send_interaction_response(http, interaction_id, interaction_token, 4,
                    Some(json!({"content": "❌ /agent 只能在伺服器中使用", "flags": 64}))).await;
                return;
            }
            // Require MANAGE_GUILD to switch agent
            if !has_manage_guild_permission(data) {
                send_interaction_response(http, interaction_id, interaction_token, 4,
                    Some(json!({"content": "❌ 需要「管理伺服器」權限才能切換 Agent", "flags": 64}))).await;
                return;
            }

            let agent_name = cmd_data["options"]
                .as_array()
                .and_then(|opts| opts.first())
                .and_then(|o| o["value"].as_str())
                .unwrap_or("");

            let scope = if guild_id.is_empty() { "global" } else { guild_id };
            let reg = ctx.registry.read().await;
            if reg.get(agent_name).is_some() {
                drop(reg);
                let _ = ctx.channel_settings.set("discord", scope, keys::AGENT_OVERRIDE, agent_name).await;
                let msg = format!("Switched to agent: **{agent_name}**");
                send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": msg}))).await;
            } else {
                let agents: Vec<String> = reg.list().iter().map(|a| a.config.agent.name.clone()).collect();
                let msg = format!("Agent `{agent_name}` not found.\nAvailable: {}", agents.join(", "));
                send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": msg, "flags": 64}))).await;
            }
        }

        _ => {
            send_interaction_response(http, interaction_id, interaction_token, 4, Some(json!({"content": "Unknown command", "flags": 64}))).await;
        }
    }
}

// ── Discord REST helpers ────────────────────────────────────

/// Send an interaction response.
/// Type 4 = CHANNEL_MESSAGE_WITH_SOURCE, 5 = DEFERRED, 6 = DEFERRED_UPDATE
async fn send_interaction_response(
    http: &reqwest::Client,
    interaction_id: &str,
    interaction_token: &str,
    response_type: u8,
    data: Option<Value>,
) {
    let body = json!({
        "type": response_type,
        "data": data.unwrap_or(json!({}))
    });

    let url = format!("{DISCORD_API}/interactions/{interaction_id}/{interaction_token}/callback");
    if let Err(e) = http.post(&url).json(&body).send().await {
        error!("Discord interaction response error: {e}");
    }
}

/// Edit the original interaction response (for deferred responses).
/// Uses application_id (snowflake), NOT bot token, per Discord API docs.
async fn edit_interaction_response(
    http: &reqwest::Client,
    app_id: &str,
    interaction_token: &str,
    data: &Value,
) {
    // Strip components (buttons) — not handled by DuDuClaw
    let cleaned = if data.get("components").is_some() {
        let mut d = data.clone();
        d.as_object_mut().map(|m| m.remove("components"));
        d
    } else {
        data.clone()
    };

    let url = format!("{DISCORD_API}/webhooks/{app_id}/{interaction_token}/messages/@original");
    match http.patch(&url).json(&cleaned).send().await {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Discord edit interaction failed ({status}): {}", &body[..body.len().min(200)]);
        }
        Err(e) => error!("Discord edit interaction error: {e}"),
        _ => {}
    }
}

// ── Config ──────────────────────────────────────────────────

async fn read_discord_token(home_dir: &Path) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "discord_bot_token").await
}
