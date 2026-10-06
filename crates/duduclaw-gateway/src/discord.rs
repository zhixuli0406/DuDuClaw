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

use duduclaw_core::truncate_bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::channel_format::{self, split_text};
use crate::channel_reply::{
    ReplyContext, build_guarded_reply_for_agent, build_guarded_reply_with_session,
    set_channel_connected,
};
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

#[derive(Debug, Serialize)]
struct GatewayResume {
    op: u8,
    d: ResumeData,
}

#[derive(Debug, Serialize)]
struct ResumeData {
    token: String,
    session_id: String,
    seq: u64,
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
    // §10.6: user-visible product name honours white-label branding.
    let product =
        crate::branding::effective_product_name(&duduclaw_core::platform::duduclaw_home());
    vec![
        json!({
            "name": "ask",
            "description": format!("Ask {product} AI a question"),
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
            "description": format!("Show {product} bot status"),
            "type": 1
        }),
        json!({
            "name": "config",
            "description": format!("Configure {product} settings for this server"),
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
            set_channel_connected(
                &channel_status,
                "discord",
                false,
                Some(msg),
                Some(&event_tx),
            )
            .await;
            return None;
        }
        Err(e) => {
            warn!("Discord connection failed: {e}");
            set_channel_connected(
                &channel_status,
                "discord",
                false,
                Some(e.to_string()),
                Some(&event_tx),
            )
            .await;
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
                info.url
                    .unwrap_or_else(|| "wss://gateway.discord.gg".to_string())
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
        gateway_loop(
            token,
            bot_id,
            app_id,
            gateway_url,
            http,
            ctx,
            "discord".to_string(),
            None,
        )
        .await;
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

    // Loaded once for the whole bot-start pass (WP-6C) — every per-agent
    // resolve below shares it rather than re-reading config.toml per agent.
    let sm_cfg =
        duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(home_dir).await;

    // Collect per-agent tokens first so we know whether the global token is
    // the only path or a legacy fallback — this lets us demote a 401 on the
    // global token to info-level when per-agent bots will cover Discord anyway.
    let agent_tokens: Vec<(String, String)> = {
        let reg = ctx.registry.read().await;
        let mut tokens = Vec::new();
        for agent in reg.list() {
            if let Some(channels) = &agent.config.channels {
                if let Some(discord) = &channels.discord {
                    // WP-H1: this used to be a hand-inlined 4th copy of
                    // `resolve_agent_token`'s enc-then-plaintext logic — with
                    // its own empty-string dance and no `secret://` support, so
                    // a reference here was passed to Discord as the bot token.
                    if let Some(token) = crate::config_crypto::resolve_agent_token(
                        &discord.bot_token_enc,
                        &discord.bot_token,
                        home_dir,
                        &sm_cfg,
                    )
                    .await
                    {
                        tokens.push((agent.config.agent.name.clone(), token.expose_owned()));
                    }
                }
            }
        }
        tokens
    };
    let has_agent_tokens = !agent_tokens.is_empty();

    // 1. Global bot from config.toml (legacy when per-agent tokens exist).
    //    The Discord Gateway only allows one active session per token, and the
    //    generic global bot routes via `default_agent` — so when an agent
    //    already binds this token we skip the global bot to avoid a second
    //    session fighting for it and to keep replies attributed to the right
    //    agent (no identity mixing). The per-agent bot is authoritative.
    if let Some(token) = read_discord_token(home_dir).await {
        if !token.is_empty() {
            if let Some(owner) = crate::channel_reply::find_global_token_owner(
                &token,
                agent_tokens.iter().map(|(n, t)| (n.as_str(), t.as_str())),
            ) {
                warn!(
                    "Discord global token is also bound to agent '{owner}' — \
                     skipping the global bot to avoid a duplicate Gateway \
                     session and identity mixing; the per-agent bot is authoritative"
                );
            } else {
                seen_tokens.insert(token.clone());
                if let Some(handle) = spawn_discord_bot(
                    token,
                    "discord".to_string(),
                    None,
                    ctx.clone(),
                    home_dir,
                    has_agent_tokens,
                )
                .await
                {
                    results.push(("discord".to_string(), handle));
                }
            }
        }
    }

    // 2. Per-agent bots from agent configs
    for (agent_name, token) in agent_tokens {
        if seen_tokens.contains(&token) {
            info!(
                "Discord bot for agent '{agent_name}' shares an already-claimed token — skipping duplicate"
            );
            continue;
        }
        seen_tokens.insert(token.clone());
        let label = format!("discord:{agent_name}");
        if let Some(handle) = spawn_discord_bot(
            token,
            label.clone(),
            Some(agent_name),
            ctx.clone(),
            home_dir,
            false, // per-agent path is authoritative; any 401 here is a real problem
        )
        .await
        {
            results.push((label, handle));
        }
    }

    results
}

/// Spawn a single Discord bot connection (shared by global and per-agent paths).
///
/// `quiet_on_auth_failure`: when true, a 401/403 on token validation is
/// logged at info level (e.g. the global `config.toml` token is stale but
/// per-agent tokens will cover Discord connectivity). When false, the same
/// failure is escalated to warn.
async fn spawn_discord_bot(
    token: String,
    label: String,
    agent_name: Option<String>,
    ctx: Arc<ReplyContext>,
    home_dir: &Path,
    quiet_on_auth_failure: bool,
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
            let status = resp.status();
            if quiet_on_auth_failure
                && (status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN)
            {
                info!(
                    "Discord bot [{label}] token stale (HTTP {status}) — skipping; per-agent tokens will handle connectivity"
                );
            } else {
                warn!("Discord bot [{label}] token invalid (HTTP {status})");
            }
            set_channel_connected(
                &channel_status,
                &label,
                false,
                Some("token invalid".into()),
                Some(&event_tx),
            )
            .await;
            return None;
        }
        Err(e) => {
            warn!("Discord [{label}] connection failed: {e}");
            set_channel_connected(
                &channel_status,
                &label,
                false,
                Some(e.to_string()),
                Some(&event_tx),
            )
            .await;
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
                info.url
                    .unwrap_or_else(|| "wss://gateway.discord.gg".to_string())
            } else {
                "wss://gateway.discord.gg".to_string()
            }
        }
        Err(_) => "wss://gateway.discord.gg".to_string(),
    };

    let gateway_url = format!("{gateway_url}/?v=10&encoding=json");
    info!("   Discord [{label}] Gateway: {gateway_url}");

    let handle = tokio::spawn(async move {
        gateway_loop(
            token,
            bot_id,
            app_id,
            gateway_url,
            http,
            ctx,
            label,
            agent_name,
        )
        .await;
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
            warn!(
                "Discord: slash command registration failed ({status}): {}",
                truncate_bytes(&body, 200)
            );
        }
        Err(e) => {
            warn!("Discord: slash command registration error: {e}");
        }
    }
}

// ── Gateway loop ────────────────────────────────────────────

// Handler concurrency: `channel_decision_route::DISCORD_PERMITS` — 10 permits
// for ordinary work as before, plus a separate small pool for decision
// messages and decision buttons so busy replies cannot starve a confirmation
// (F4, review M3).

/// Compute the wait time for a token-check 429 fallback (when Discord doesn't
/// send a `Retry-After` header). Streak `0` is treated as `1` so the first
/// backoff is 60s rather than 0s. Sequence is 60 → 120 → 240 → 480 → 900,
/// capped at 15 min to avoid pathological waits.
///
/// Extracted as a pure function so the table-driven test below can lock the
/// progression in place — the previous flat 60s loop went unnoticed for so
/// long because there was no test pinning the backoff schedule.
fn token_check_backoff_secs(streak: u32) -> u64 {
    const CAP: u64 = 900;
    let n = streak.max(1) - 1; // streak 1 → 60·2^0 = 60
    let base = 60u64.checked_shl(n).unwrap_or(CAP);
    base.min(CAP)
}

/// 24h sliding-window threshold for invalid-session storms (#4.2).
///
/// Five op-9 invalid sessions inside one day usually indicates a
/// non-transient problem — Discord-side outage, repeated session resets
/// from a guild-level issue, or a token quietly invalidated. Once we hit
/// this many, we emit a `discord_invalid_session_storm` security audit
/// event so it surfaces in the dashboard's reliability page.
pub(crate) const INVALID_SESSION_WINDOW_HOURS: i64 = 24;
pub(crate) const INVALID_SESSION_ALERT_THRESHOLD: usize = 5;

/// Compute the reconnect delay for an invalid-session event with jitter.
///
/// Discord docs require 1–5 s, with jitter encouraged so a million bots
/// reconnecting at once don't synchronise. Extracted as a pure function
/// so we can unit-test the bounds without booting a websocket; the
/// `nanos_seed` argument lets the test pin the deterministic output.
pub(crate) fn invalid_session_jitter_ms(nanos_seed: u32) -> u64 {
    // 1000-5000 ms range — Discord's spec.
    1000 + ((nanos_seed % 4000) as u64)
}

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

    // Token-check 429 retry tracking — separate from `consecutive_failures`,
    // which counts gateway connection attempts. Without an independent counter,
    // the rate-limit branch did `continue` without incrementing anything, so
    // 22 consecutive 60s waits could accumulate (observed 2026-05-08 20:27).
    // Cap retries before giving up; back off exponentially with a sane ceiling.
    let mut token_check_rate_limited_streak: u32 = 0;
    const MAX_TOKEN_CHECK_RETRIES: u32 = 5;

    // 24-hour sliding window of op-9 "invalid session" timestamps (#4.2).
    // 4 invalid sessions in 24h was observed on 2026-05-09 — borderline
    // tolerable but a 5+ count means something upstream is broken. We
    // keep the timestamps locally (no DB write per event) and emit a
    // single security audit alert when the threshold trips, then reset
    // the window so we don't spam alerts every subsequent event.
    let mut invalid_session_history: std::collections::VecDeque<chrono::DateTime<chrono::Utc>> =
        std::collections::VecDeque::new();

    // Persistent session state across reconnects — required for Gateway RESUME (op 6).
    // Without these, every reconnect IDENTIFYs anew and Discord drops every event
    // buffered during the disconnect window. See Discord Gateway docs §"Resuming".
    let session_seq = Arc::new(AtomicU64::new(u64::MAX));
    let mut session_id: Option<String> = None;
    let mut resume_gateway_url: Option<String> = None;

    loop {
        // Exponential backoff: 5s, 10s, 20s, ... capped at 60s.
        // Cap was 300s — too long for an interactive bot; if we genuinely
        // can't reconnect after 60s the underlying issue won't fix itself
        // by waiting longer.
        if consecutive_failures > 0 {
            let backoff = std::cmp::min(5u64 << consecutive_failures.min(4), 60);
            warn!(
                "Discord [{label}] reconnecting in {backoff}s (attempt {consecutive_failures}/{MAX_FAILURES})"
            );
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        }

        // Re-verify token before reconnecting to avoid hammering Discord
        if consecutive_failures >= 2 {
            match http
                .get(format!("{DISCORD_API}/users/@me"))
                .header("Authorization", format!("Bot {token}"))
                .send()
                .await
            {
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    error!("Discord [{label}] token is invalid (401), stopping bot");
                    set_channel_connected(
                        &channel_status,
                        &label,
                        false,
                        Some("token invalid — update via Dashboard".into()),
                        Some(&event_tx),
                    )
                    .await;
                    return;
                }
                Ok(resp) if resp.status().as_u16() == 429 => {
                    // Honor Retry-After (header value is seconds, optionally float
                    // per Discord; round up to whole seconds). Fall back to
                    // exponential 60→120→240→480→900 keyed on the streak length.
                    let header_secs = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<f64>().ok())
                        .map(|f| f.ceil() as u64);

                    token_check_rate_limited_streak += 1;
                    if token_check_rate_limited_streak >= MAX_TOKEN_CHECK_RETRIES {
                        error!(
                            "Discord [{label}] token-check rate-limited {token_check_rate_limited_streak}x — \
                             abandoning bot to avoid hammering Discord",
                        );
                        set_channel_connected(
                            &channel_status,
                            &label,
                            false,
                            Some("rate-limited; bot stopped — try again later".into()),
                            Some(&event_tx),
                        )
                        .await;
                        return;
                    }

                    let backoff = header_secs.unwrap_or_else(|| {
                        token_check_backoff_secs(token_check_rate_limited_streak)
                    });
                    warn!(
                        "Discord [{label}] rate limited during token check (streak \
                         {token_check_rate_limited_streak}/{MAX_TOKEN_CHECK_RETRIES}), \
                         waiting {backoff}s (retry-after header: {header_secs:?})",
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }
                Err(_) => {
                    // network error, proceed to try gateway anyway
                    token_check_rate_limited_streak = 0;
                }
                _ => {
                    // token ok
                    token_check_rate_limited_streak = 0;
                }
            }
        }

        if consecutive_failures >= MAX_FAILURES {
            error!("Discord [{label}] {MAX_FAILURES} consecutive failures, stopping bot");
            set_channel_connected(
                &channel_status,
                &label,
                false,
                Some(format!(
                    "stopped after {MAX_FAILURES} failures — check token"
                )),
                Some(&event_tx),
            )
            .await;
            return;
        }

        // Use resume_gateway_url if available — Discord requires reconnects to
        // hit the URL provided in the original READY event for RESUME to work.
        let connect_url = match (&session_id, &resume_gateway_url) {
            (Some(_), Some(rurl)) => rurl.clone(),
            _ => gateway_url.clone(),
        };
        let attempting_resume = session_id.is_some();
        info!(
            "Discord [{label}] Gateway connecting (mode={})...",
            if attempting_resume {
                "RESUME"
            } else {
                "IDENTIFY"
            }
        );
        set_channel_connected(
            &channel_status,
            &label,
            false,
            Some("connecting".into()),
            Some(&event_tx),
        )
        .await;

        let ws = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(&connect_url),
        )
        .await
        {
            Ok(Ok((ws, resp))) => {
                info!(
                    "Discord Gateway WebSocket connected (HTTP {})",
                    resp.status()
                );
                ws
            }
            Ok(Err(e)) => {
                warn!("Discord [{label}] Gateway connection failed: {e}");
                set_channel_connected(
                    &channel_status,
                    &label,
                    false,
                    Some(e.to_string()),
                    Some(&event_tx),
                )
                .await;
                consecutive_failures += 1;
                continue;
            }
            Err(_) => {
                warn!("Discord [{label}] Gateway connection timeout (15s)");
                set_channel_connected(
                    &channel_status,
                    &label,
                    false,
                    Some("Connection timeout".into()),
                    Some(&event_tx),
                )
                .await;
                consecutive_failures += 1;
                continue;
            }
        };

        let (mut write, mut read) = ws.split();
        let mut heartbeat_interval_ms: u64 = 41250;
        // Track last heartbeat ACK to detect zombied connections.
        // Discord requires: if no ACK received within heartbeat_interval,
        // the client MUST close and reconnect.
        let mut last_heartbeat_ack = std::time::Instant::now();
        let mut awaiting_heartbeat_ack = false;
        let mut last_message_at = std::time::Instant::now();

        // Capacity 16 + try_send so heartbeat task can never block on a busy
        // receiver. Capacity 1 + send().await caused permanent deadlock when
        // select! consumed slowly: heartbeat task stuck awaiting send → no
        // future ticks → zombie detection never fires → silent stall.
        let (heartbeat_tx, mut heartbeat_rx) = tokio::sync::mpsc::channel::<()>(16);
        let heartbeat_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        // Tracks whether the current connection has been promoted to a usable
        // session (READY for new sessions, RESUMED for resumes). Used by the
        // outer `consecutive_failures += 1` to distinguish "broke before
        // session was ever live" from "broke after working for a while".
        let mut session_live = false;

        // Inner loop break signals.
        enum BreakReason {
            Recoverable,
            SessionInvalid, // clear session_id and reconnect
            Fatal,          // exit gateway_loop entirely
        }
        let break_reason: BreakReason = loop {
            tokio::select! {
                msg_opt = read.next() => {
                    last_message_at = std::time::Instant::now();
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
                                break BreakReason::Recoverable;
                            }
                            continue;
                        }
                        Some(Ok(Message::Close(frame))) => {
                            let raw_code = frame.as_ref().map(|f| u16::from(f.code));
                            let reason = frame.as_ref().map(|f| f.reason.to_string()).unwrap_or_default();
                            warn!("Discord [{label}] Gateway closed (code: {raw_code:?}, reason: {reason})");
                            match raw_code {
                                // Fatal — do not retry
                                Some(4004) => {
                                    error!("Discord [{label}] authentication failed (4004), stopping");
                                    set_channel_connected(&channel_status, &label, false, Some("authentication failed — update token via Dashboard".into()), Some(&event_tx)).await;
                                    break BreakReason::Fatal;
                                }
                                Some(4014) => {
                                    error!("Discord [{label}] disallowed intents (4014), stopping");
                                    set_channel_connected(&channel_status, &label, false, Some("disallowed intents — enable MESSAGE CONTENT INTENT in Discord Developer Portal".into()), Some(&event_tx)).await;
                                    break BreakReason::Fatal;
                                }
                                Some(4013) => {
                                    error!("Discord [{label}] invalid intents (4013), stopping");
                                    set_channel_connected(&channel_status, &label, false, Some("invalid intents".into()), Some(&event_tx)).await;
                                    break BreakReason::Fatal;
                                }
                                // Session is no longer resumable — must IDENTIFY fresh.
                                // 4007 invalid seq, 4009 session timed out, 4003 not authenticated.
                                Some(4007) | Some(4009) | Some(4003) => {
                                    break BreakReason::SessionInvalid;
                                }
                                _ => break BreakReason::Recoverable,
                            }
                        }
                        Some(Err(e)) => { warn!("Discord Gateway error: {e}"); break BreakReason::Recoverable; }
                        None => break BreakReason::Recoverable,
                        _ => continue,
                    };

                    let payload: GatewayPayload = match serde_json::from_str(&msg) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Discord Gateway: failed to parse payload: {e} (first 200 chars: {})", truncate_bytes(&msg, 200));
                            continue;
                        }
                    };

                    if let Some(s) = payload.s {
                        session_seq.store(s, Ordering::Relaxed);
                    }

                    match payload.op {
                        // Hello — start heartbeating, then either RESUME or IDENTIFY
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
                                    // try_send: drop ticks rather than block when receiver is slow.
                                    if tx.try_send(()).is_err() && tx.is_closed() {
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
                            drop(guard);

                            // Send RESUME if we have a valid session, otherwise IDENTIFY.
                            let send_res = match (&session_id, session_seq.load(Ordering::Relaxed)) {
                                (Some(sid), seq) if seq != u64::MAX => {
                                    let resume = GatewayResume {
                                        op: 6,
                                        d: ResumeData {
                                            token: token.clone(),
                                            session_id: sid.clone(),
                                            seq,
                                        },
                                    };
                                    info!("Discord [{label}] sending RESUME (seq={seq})");
                                    let json_str = serde_json::to_string(&resume).unwrap_or_default();
                                    write.send(Message::Text(json_str.into())).await
                                }
                                _ => {
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
                                    write.send(Message::Text(json_str.into())).await
                                }
                            };
                            if send_res.is_err() {
                                break BreakReason::Recoverable;
                            }
                            info!("Discord Gateway handshake sent (heartbeat: {heartbeat_interval_ms}ms)");
                        }

                        // Heartbeat ACK — record timestamp for zombie detection
                        11 => {
                            last_heartbeat_ack = std::time::Instant::now();
                            awaiting_heartbeat_ack = false;
                        }

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
                                            let message_app_id=app_id.clone();
                                            let ctx = ctx.clone();
                                            let agent = agent_name.clone();
                                            // The handler takes its own permit: a
                                            // decision-shaped message only after the
                                            // sender passed the access check (F5-C).
                                            tokio::spawn(async move {
                                                handle_message_create(
                                                    &d, &bot_id, &http, &token, &ctx,
                                                    agent.as_deref(), &message_app_id
                                                )
                                                .await;
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
                                                // Decision lane only for a decision
                                                // button whose presser passed the access
                                                // check (F5-C, review F4-M1).
                                                let lane = discord_interaction_lane(&ctx, &app_id, &d).await;
                                                let _permit = crate::channel_decision_route::current_discord_permits()
                                                    .acquire(lane)
                                                    .await;
                                                handle_interaction(&d, &bot_id, &app_id, &http, &token, &ctx).await;
                                            });
                                        }
                                    }
                                    "READY" => {
                                        // Capture session_id + resume_gateway_url for future RESUMEs.
                                        if let Some(d) = &payload.d {
                                            session_id = d.get("session_id")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string());
                                            resume_gateway_url = d.get("resume_gateway_url")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string());
                                        }
                                        info!(
                                            "Discord [{label}] Gateway READY (session_id={}, resume_url={})",
                                            session_id.as_deref().unwrap_or("?"),
                                            resume_gateway_url.is_some()
                                        );
                                        consecutive_failures = 0;
                                        session_live = true;
                                        set_channel_connected(&channel_status, &label, true, None, Some(&event_tx)).await;
                                    }
                                    "RESUMED" => {
                                        info!("Discord [{label}] Gateway RESUMED");
                                        consecutive_failures = 0;
                                        session_live = true;
                                        set_channel_connected(&channel_status, &label, true, None, Some(&event_tx)).await;
                                    }
                                    "GUILD_CREATE" => {
                                        if let Some(d) = &payload.d {
                                            let guild_name = d["name"].as_str().unwrap_or("unknown").to_string();
                                            let guild_id = d["id"].as_str().unwrap_or("").to_string();
                                            info!("Discord: joined guild '{guild_name}' ({guild_id})");
                                            if !guild_id.is_empty() {
                                                let ctx = ctx.clone();
                                                tokio::spawn(async move {
                                                    let settings = &ctx.channel_settings;
                                                    // Record the guild name for status reporting
                                                    // (upsert keeps renames fresh).
                                                    let _ = settings.set("discord", &guild_id, keys::GUILD_NAME, &guild_name).await;
                                                    // Seed defaults only when unset. mention_only is
                                                    // intentionally NOT seeded: its default differs
                                                    // between the global bot (false) and per-agent
                                                    // bots (true); a stored value would override both.
                                                    let existing = settings.get_all("discord", &guild_id).await;
                                                    if !existing.iter().any(|(k, _)| k == keys::AUTO_THREAD) {
                                                        let _ = settings.set("discord", &guild_id, keys::AUTO_THREAD, "true").await;
                                                    }
                                                });
                                            }
                                        }
                                    }
                                    // Thread lifecycle: close the mapped session when a
                                    // thread archives or is deleted, so a revived thread
                                    // starts a fresh conversation instead of dragging in
                                    // stale context.
                                    "THREAD_UPDATE" | "THREAD_DELETE" => {
                                        if let Some(d) = &payload.d {
                                            let thread_id = d["id"].as_str().unwrap_or("").to_string();
                                            let archived = event == "THREAD_DELETE"
                                                || d["thread_metadata"]["archived"].as_bool().unwrap_or(false);
                                            if archived && !thread_id.is_empty() {
                                                let ctx = ctx.clone();
                                                let reason = if event == "THREAD_DELETE" { "deleted" } else { "archived" };
                                                tokio::spawn(async move {
                                                    let session_id = format!("discord:thread:{thread_id}");
                                                    match ctx.session_manager.delete_session(&session_id).await {
                                                        Ok(()) => info!("Discord: thread {thread_id} {reason} — session closed"),
                                                        // Most threads never had a session; not an error.
                                                        Err(e) => debug!("Discord: thread {thread_id} {reason}, no session to close ({e})"),
                                                    }
                                                });
                                            }
                                        }
                                    }
                                    "THREAD_CREATE" => {
                                        debug!("Discord: thread created (session is created lazily on first message)");
                                    }
                                    _ => {
                                        debug!("Discord event: {event}");
                                    }
                                }
                            }
                        }

                        // Reconnect — Discord asks us to disconnect & RESUME on a new socket.
                        7 => {
                            info!("Discord [{label}] Gateway requested reconnect (op 7)");
                            break BreakReason::Recoverable;
                        }

                        // Invalid Session — d is a boolean: true = resumable, false = must re-IDENTIFY.
                        // Per Discord docs, wait 1-5s before reconnecting.
                        9 => {
                            let resumable = payload.d
                                .as_ref()
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            // #4.2: track invalid sessions in a 24h sliding window;
                            // emit one consolidated security_audit event when the
                            // storm threshold trips so the dashboard surfaces it.
                            let now = chrono::Utc::now();
                            invalid_session_history.push_back(now);
                            // Trim entries outside the observation window so the
                            // VecDeque stays bounded under steady-state churn.
                            let cutoff = now - chrono::Duration::hours(INVALID_SESSION_WINDOW_HOURS);
                            while invalid_session_history
                                .front()
                                .is_some_and(|t| *t < cutoff)
                            {
                                invalid_session_history.pop_front();
                            }
                            let count_24h = invalid_session_history.len();

                            warn!(
                                count_24h,
                                "Discord [{label}] Gateway invalid session (resumable={resumable})"
                            );
                            set_channel_connected(&channel_status, &label, false, Some("invalid session".to_string()), Some(&event_tx)).await;

                            if count_24h >= INVALID_SESSION_ALERT_THRESHOLD {
                                let home = ctx.home_dir.clone();
                                let label_for_audit = label.clone();
                                tokio::task::spawn_blocking(move || {
                                    crate::security_autopilot::audit_and_emit(
                                        &home,
                                        &duduclaw_security::audit::AuditEvent::new(
                                            "discord_invalid_session_storm",
                                            label_for_audit,
                                            duduclaw_security::audit::Severity::Warning,
                                            serde_json::json!({
                                                "count_24h": count_24h,
                                                "threshold": INVALID_SESSION_ALERT_THRESHOLD,
                                                "window_hours": INVALID_SESSION_WINDOW_HOURS,
                                                "guidance": "Repeated op-9 within 24h usually means a non-transient Discord-side or token-side problem. Check `gh status discord` and re-validate the bot token in the dashboard."
                                            }),
                                        ),
                                    );
                                });
                                // Reset so we don't re-fire on every subsequent
                                // event. New storms must accumulate THRESHOLD
                                // events again before alerting.
                                invalid_session_history.clear();
                            }

                            // Discord requires a 1-5s random delay before reconnecting.
                            let jitter_ms = invalid_session_jitter_ms(
                                std::time::Instant::now().elapsed().subsec_nanos(),
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(jitter_ms)).await;
                            if resumable {
                                break BreakReason::Recoverable;
                            } else {
                                break BreakReason::SessionInvalid;
                            }
                        }

                        _ => {}
                    }
                }

                Some(()) = heartbeat_rx.recv() => {
                    // Check for zombied connection: if we sent a heartbeat
                    // but never got an ACK before the next heartbeat fires,
                    // the connection is dead.
                    if awaiting_heartbeat_ack {
                        let elapsed = last_heartbeat_ack.elapsed();
                        warn!(
                            "Discord [{label}] no heartbeat ACK received in {:.1}s — zombied connection, reconnecting",
                            elapsed.as_secs_f64()
                        );
                        break BreakReason::Recoverable;
                    }

                    let seq_val = session_seq.load(Ordering::Relaxed);
                    let seq_json: Value = if seq_val == u64::MAX {
                        Value::Null
                    } else {
                        Value::Number(seq_val.into())
                    };
                    let hb = json!({ "op": 1, "d": seq_json });
                    if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                        break BreakReason::Recoverable;
                    }
                    awaiting_heartbeat_ack = true;
                }

                // Stall watchdog: if neither read nor heartbeat fires for
                // 2× heartbeat_interval, the select! is stalled. Without this,
                // a half-closed TCP (FIN never seen) + a frozen heartbeat task
                // would hang forever silently — the exact symptom seen in
                // production at 11:17Z 2026-04-28 where 18 minutes passed
                // with no log output.
                _ = tokio::time::sleep(std::time::Duration::from_secs(
                    (heartbeat_interval_ms / 1000).saturating_mul(2).max(60)
                )) => {
                    let idle = last_message_at.elapsed();
                    warn!(
                        "Discord [{label}] gateway stall watchdog fired (no traffic for {:.0}s), reconnecting",
                        idle.as_secs_f64()
                    );
                    break BreakReason::Recoverable;
                }
            }
        };

        // Cleanup heartbeat
        let mut guard = heartbeat_handle.lock().await;
        if let Some(h) = guard.take() {
            h.abort();
        }
        drop(guard);

        match break_reason {
            BreakReason::Fatal => return,
            BreakReason::SessionInvalid => {
                session_id = None;
                resume_gateway_url = None;
                session_seq.store(u64::MAX, Ordering::Relaxed);
            }
            BreakReason::Recoverable => {}
        }

        // Only escalate failure count if the session never went live this round.
        // A session that worked for hours and then closed cleanly should reconnect
        // at backoff=0, not get penalised.
        if session_live {
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
        }
        set_channel_connected(
            &channel_status,
            &label,
            false,
            Some("reconnecting".to_string()),
            Some(&event_tx),
        )
        .await;
    }
}

// ── Message handling ────────────────────────────────────────

/// Quoted-reply context from a MESSAGE_CREATE payload. Discord embeds the
/// full `referenced_message` object (including content) when the message is
/// a reply — no extra API call needed. `None` when the message is not a
/// reply, or the referenced message was deleted (Discord sends `null`).
fn discord_reply_context(data: &Value, bot_id: &str) -> Option<String> {
    let referenced = data.get("referenced_message")?;
    if referenced.is_null() {
        return None;
    }
    let ref_author = referenced.get("author");
    let who = if ref_author.and_then(|a| a["id"].as_str()) == Some(bot_id) {
        channel_format::QUOTED_SELF_LABEL.to_string()
    } else {
        ref_author
            .and_then(|a| a["username"].as_str())
            .unwrap_or("對方")
            .to_string()
    };
    let ref_content = referenced["content"].as_str().unwrap_or("").trim();
    let excerpt = if !ref_content.is_empty() {
        ref_content.to_string()
    } else if referenced["attachments"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        "（附件訊息，無文字）".to_string()
    } else if referenced["embeds"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        "（嵌入內容訊息，無文字）".to_string()
    } else {
        return None;
    };
    Some(channel_format::format_quoted_context(&who, &excerpt))
}

async fn handle_message_create(
    data: &Value,
    bot_id: &str,
    http: &reqwest::Client,
    token: &str,
    ctx: &Arc<ReplyContext>,
    agent_name: Option<&str>,
    app_id: &str,
) {
    // Ignore messages from the bot itself or other bots
    let author = data.get("author");
    let author_id = author.and_then(|a| a["id"].as_str()).unwrap_or("");
    let is_bot = author.and_then(|a| a["bot"].as_bool()).unwrap_or(false);

    if author_id == bot_id || is_bot {
        return;
    }

    // F5-C (review F4-M1): a decision-shaped message (verb + full request id
    // after removing this bot's mention) takes no permit until its sender has
    // passed the channel lookup and access check below, then takes one from
    // the decision pool and is answered before any attachment is touched.
    // Everything else takes an ordinary permit first, as before.
    let content = data["content"].as_str().unwrap_or("");
    let decision_text = strip_bot_mention(content, bot_id);
    let decision_shaped = crate::channel_decision_route::DiscordLane::for_message(&decision_text)
        == crate::channel_decision_route::DiscordLane::Decision;
    let permits = crate::channel_decision_route::current_discord_permits();
    let mut _permit = if decision_shaped {
        None
    } else {
        permits
            .acquire(crate::channel_decision_route::DiscordLane::General)
            .await
    };

    let channel_id = data["channel_id"].as_str().unwrap_or("");
    let guild_id = match data.get("guild_id") {
        None | Some(Value::Null) => "",
        Some(Value::String(guild)) if !guild.is_empty() => guild.as_str(),
        _ => return,
    };
    let Some(current_channel) =
        discord_verified_message_identity(http, token, channel_id, guild_id).await
    else {
        return;
    };
    let current_session_id = &current_channel.session_id;
    let is_thread = current_channel.is_thread;
    let decision_context = crate::approval::DecisionContext {
        channel: "discord".into(),
        account_id: app_id.into(),
        conversation_id: channel_id.into(),
        principal_id: author_id.into(),
    };
    // The authoritative lookup precedes the fresh SQL read and all session,
    // command, attachment and auto-thread mutations for this inbound turn.
    let Ok(user_access_policy) = crate::decision_notify::read_trusted_channel_access(
        ctx,
        &decision_context,
        crate::decision_notify::DecisionAccessScope {
            channel_id: (!guild_id.is_empty()).then_some(channel_id),
            guild_id: (!guild_id.is_empty()).then_some(guild_id),
            session_id: Some(current_session_id),
        },
    )
    .await
    else {
        return;
    };
    if user_access_policy
        .blocked
        .iter()
        .any(|id| id == author_id || id == current_session_id)
    {
        return;
    }

    if decision_shaped {
        let scope = crate::decision_notify::DecisionAccessScope {
            channel_id: (!guild_id.is_empty()).then_some(channel_id),
            guild_id: (!guild_id.is_empty()).then_some(guild_id),
            session_id: Some(current_session_id),
        };
        // Full access check (allowlist, pairing, …) before a decision permit:
        // a refused sender gets the uniform refusal without ever holding one.
        let outcome = if crate::decision_notify::check_trusted_decision_access(
            ctx,
            &decision_context,
            scope,
            &decision_text,
        )
        .await
        .is_err()
        {
            Err(crate::channel_decision_route::DECISION_REFUSED.to_string())
        } else {
            _permit = permits
                .acquire(crate::channel_decision_route::DiscordLane::Decision)
                .await;
            crate::decision_notify::route_trusted_decision_fastlane_with_scope(
                ctx,
                &decision_context,
                &decision_text,
                scope,
            )
            .await
            .unwrap_or_else(|| Err(crate::channel_decision_route::DECISION_REFUSED.into()))
        };
        if let Ok(target) = crate::approval::TrustedReplyTarget::new(
            decision_context,
            token.into(),
            channel_id.into(),
            None,
        ) {
            let ack = outcome.unwrap_or_else(|error| format!("⚠️ {error}"));
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                target.sender(http.clone()).send_text(&ack),
            )
            .await;
        }
        return;
    }

    // WP1.3: download attachments to disk (agent-Readable absolute paths) so
    // office documents can be parsed by skills, not just linked. Files land in
    // the shared `{home}/attachments/` (the DELIVER validator trusts this root
    // too); download failures degrade to a plain URL reference.
    let mut attachment_lines: Vec<String> = Vec::new();
    if let Some(arr) = data["attachments"].as_array() {
        let attach_base =
            crate::channel_reply::resolve_attachment_base(ctx.as_ref(), agent_name).await;
        for att in arr {
            let Some(url) = att["url"].as_str() else {
                continue;
            };
            let content_type = att["content_type"]
                .as_str()
                .unwrap_or("application/octet-stream");
            let filename = att["filename"].as_str().unwrap_or("file");
            let mt = crate::media::media_type_from_mime(content_type);
            #[cfg(test)]
            attachment_download_probe::record(token);
            // F5-C (review F4-M1): bounded, so one slow attachment cannot
            // hold an ordinary permit indefinitely.
            let download = tokio::time::timeout(
                DISCORD_ATTACHMENT_TIMEOUT,
                crate::media::download_url(
                    &ctx.http,
                    url,
                    None,
                    crate::media::MAX_FILE_SIZE as usize,
                ),
            )
            .await
            .unwrap_or_else(|_| Err("attachment download timed out".into()));
            match download
            {
                Ok(bytes) => {
                    match crate::media::save_attachment_in_base(&attach_base, &bytes, filename)
                        .await
                    {
                        Ok(path) => {
                            attachment_lines
                                .push(crate::media::format_attachment_ref(&mt, filename, &path));
                        }
                        Err(e) => {
                            warn!("Discord: failed to save attachment {filename}: {e}");
                            attachment_lines.push(format!("[Attached file: {filename}]({url})"));
                        }
                    }
                }
                Err(e) => {
                    warn!("Discord: failed to download attachment {filename}: {e}");
                    attachment_lines.push(format!("[Attached file: {filename}]({url})"));
                }
            }
        }
    }

    if content.is_empty() && attachment_lines.is_empty() {
        return;
    }

    let message_id = data["id"].as_str().unwrap_or("");

    // W2-7: cache channel->guild before any filtering below returns early —
    // maximizes the chance the mapping is already known by the time a
    // same-channel `/goal` command or approval fan-out needs it.
    record_channel_guild(&ctx.home_dir, channel_id, guild_id);
    let author_name = author
        .and_then(|a| a["username"].as_str())
        .unwrap_or("someone");
    let user_id = author_id;

    // Check if bot is mentioned
    let mentions = data["mentions"].as_array();
    let bot_mentioned = mentions
        .map(|arr| arr.iter().any(|m| m["id"].as_str() == Some(bot_id)))
        .unwrap_or(false);

    // Replying to the bot's own message addresses the bot like an @mention —
    // Discord omits the author from `mentions` when the replier turns the
    // ping off, so the reference itself is the reliable signal.
    let replied_to_bot = data
        .get("referenced_message")
        .and_then(|m| m.get("author"))
        .and_then(|a| a["id"].as_str())
        == Some(bot_id);

    let settings = &ctx.channel_settings;
    let scope_id = if guild_id.is_empty() { "dm" } else { guild_id };
    if settings
        .refresh_channel_snapshot("discord", scope_id)
        .await
        .is_err()
    {
        return;
    }

    // ── Mention-only filter ──
    // Per-agent bots default to mention-only in guilds to prevent all bots
    // in the same server from responding to every message.
    let default_mention_only = agent_name.is_some();
    let mention_only = settings
        .get_bool(
            "discord",
            scope_id,
            keys::MENTION_ONLY,
            default_mention_only,
        )
        .await;
    if mention_only && !guild_id.is_empty() && !bot_mentioned && !replied_to_bot {
        return; // In guild, mention_only enabled, but bot not mentioned → skip
    }

    // Normal interaction keeps `/pair` verification and hints. The strict
    // bound routers above/below remain must-authorized and reread SQL policy.
    let gate_text = strip_bot_mention(content, bot_id);
    if let Some(reply) = crate::channel_reply::check_user_access_gate_with_policy(
        ctx,
        current_session_id,
        user_id,
        &gate_text,
        &user_access_policy,
    )
    .await
    {
        if !reply.is_empty() {
            let _ =
                send_discord_message(http, token, channel_id, json!({ "content": reply })).await;
        }
        return;
    }

    // WP1.6 (ecosystem): replying to a decision card with a bare verb
    // (「同意」/「拒絕」…) counts as pressing its button — clients without
    // component rendering (watch/embeds-off) still get to decide. Same
    // dispatch (auth + accounting) as a physical press; anything that isn't
    // a whole-message verb on a live card falls through to normal chat.
    if replied_to_bot && !content.is_empty() {
        if let Some(ref_mid) = data
            .get("referenced_message")
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
        {
            if let Some(outcome) = crate::decision_text::route_text_reply(
                &ctx.home_dir,
                "discord",
                author_id,
                channel_id,
                ref_mid,
                content,
            )
            .await
            {
                let ack = match outcome {
                    Ok(m) => m,
                    Err(e) => format!("⚠ {e}"),
                };
                let _ =
                    send_discord_message(http, token, channel_id, json!({ "content": ack })).await;
                return;
            }
        }
    }

    // Strip bot mention from content and append attachment info
    let stripped = strip_bot_mention(content, bot_id);
    let stripped = stripped.trim();

    // ── Quoted-reply context ──
    let combined = match discord_reply_context(data, bot_id) {
        Some(quote_block) if stripped.is_empty() => quote_block,
        Some(quote_block) => format!("{quote_block}\n{stripped}"),
        None => stripped.to_string(),
    };

    // Combine text content with attachment references
    let combined = if attachment_lines.is_empty() {
        combined
    } else if combined.is_empty() {
        attachment_lines.join("\n")
    } else {
        format!("{combined}\n\n{}", attachment_lines.join("\n"))
    };
    let clean_content = combined.trim();

    if clean_content.is_empty() {
        return;
    }

    info!(
        "📩 Discord [{author_name}] (guild:{guild_id}): {}",
        truncate_bytes(&clean_content, 80)
    );

    // ── Auto-thread ──
    // Default to true in guilds so conversations are organized into threads
    let auto_thread_default = !guild_id.is_empty();
    let auto_thread = settings
        .get_bool("discord", scope_id, keys::AUTO_THREAD, auto_thread_default)
        .await;
    if discord_bound_text(clean_content) {
        if let Some(outcome) = crate::decision_notify::route_trusted_decision_fastlane_with_scope(
            ctx,
            &decision_context,
            clean_content,
            crate::decision_notify::DecisionAccessScope {
                channel_id: (!guild_id.is_empty()).then_some(channel_id),
                guild_id: (!guild_id.is_empty()).then_some(guild_id),
                session_id: Some(current_session_id),
            },
        )
        .await
        {
            if let Ok(target) = crate::approval::TrustedReplyTarget::new(
                decision_context,
                token.into(),
                channel_id.into(),
                None,
            ) {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    target
                        .sender(http.clone())
                        .send_text(&outcome.unwrap_or_else(|e| format!("⚠️ {e}"))),
                )
                .await;
            }
            return;
        }
    }

    // ── Chat commands (/status, /new, /handoff, /undo, /rollback, …) ──
    // Intercepted before the AI pipeline and before auto-thread creation
    // (a command should never spawn a new thread). Mirrors slack.rs.
    if crate::chat_commands::is_command(clean_content) {
        if let Some(cmd) = crate::chat_commands::parse_command(clean_content, None) {
            let session_id = current_session_id;
            // Auto-thread mode: session-scoped commands issued in a MAIN
            // channel target `discord:{channel_id}` while the conversations
            // live in `discord:thread:{tid}` — an empty result there is just
            // confusing, so guide the user into the thread instead.
            // (In-thread behavior unchanged; stateless commands still work.)
            if !is_thread
                && auto_thread
                && !guild_id.is_empty()
                && matches!(
                    cmd,
                    crate::chat_commands::ChatCommand::Undo(_)
                        | crate::chat_commands::ChatCommand::Rollback
                        | crate::chat_commands::ChatCommand::Handoff(_)
                        | crate::chat_commands::ChatCommand::New
                        | crate::chat_commands::ChatCommand::Compact
                )
                && ctx
                    .session_manager
                    .get_messages(&session_id)
                    .await
                    .map(|m| m.is_empty())
                    .unwrap_or(true)
            {
                let _ = send_discord_message(
                    http,
                    token,
                    channel_id,
                    json!({ "content": "此頻道的對話都在各自的對話串內進行，請在對話串內使用此指令。" }),
                )
                .await;
                return;
            }
            let agent_id = match agent_name {
                Some(a) => a.to_string(),
                None => {
                    let reg = ctx.registry.read().await;
                    reg.main_agent()
                        .map(|a| a.config.agent.name.clone())
                        .unwrap_or_default()
                }
            };
            // Real per-channel admin status (fail-closed) — never hardcoded.
            let is_admin =
                crate::channel_reply::is_channel_admin(ctx, "discord", &[user_id, &session_id])
                    .await;
            let reply = crate::chat_commands::handle_command(
                &cmd,
                ctx,
                &session_id,
                &agent_id,
                is_admin,
                user_id,
            )
            .await;
            let _ =
                send_discord_message(http, token, channel_id, json!({ "content": reply })).await;
            return;
        }
    }

    // Track whether we created a new thread (for guide message later)
    let mut created_thread = false;
    let reply_channel_id = if auto_thread && !is_thread && !guild_id.is_empty() {
        // Create a thread from this message
        // 通道→行為「討論串自動封存時間」 (per guild, else global; v1.68.0).
        let archive_minutes = thread_archive_minutes(
            &settings
                .get_with_fallback("discord", scope_id, keys::THREAD_ARCHIVE_MINUTES, "1440")
                .await,
        );
        match create_thread(
            http,
            token,
            channel_id,
            message_id,
            clean_content,
            archive_minutes,
        )
        .await
        {
            Some(thread_id) => {
                created_thread = true;
                thread_id
            }
            None => channel_id.to_string(), // Fallback to main channel
        }
    } else {
        channel_id.to_string()
    };

    // A successful server-created thread has a new identity; otherwise the
    // inbound identity is reused byte-for-byte by the normal dispatch.
    let session_id = if created_thread {
        format!("discord:thread:{reply_channel_id}")
    } else {
        current_session_id.clone()
    };

    #[cfg(test)]
    if ordinary_dispatch_probe::record_if_installed(token, &session_id) {
        return;
    }

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
                    _ => {
                        consecutive_failures = 0;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            }
        });
        TypingGuard { flag, handle }
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
    let progress_msg_id: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let progress_msg_id_cb = progress_msg_id.clone();
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        let mut last = match last_progress.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let throttle = crate::channel_capabilities::progress_throttle_secs("discord").unwrap_or(30);
        if last.elapsed().as_secs() < throttle {
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
            let existing_id = match mid.lock() {
                Ok(g) => g.clone(),
                Err(e) => e.into_inner().clone(),
            };
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
                            match mid.lock() {
                                Ok(mut g) => *g = Some(id.to_string()),
                                Err(e) => *e.into_inner() = Some(id.to_string()),
                            };
                        }
                    }
                }
            }
        });
    });
    let cleanup_http = http.clone();
    let cleanup_token = token.to_string();
    let cleanup_channel = reply_channel_id.clone();

    // ── Resolve effective agent ──
    // Per-agent bot binding wins; otherwise a guild-level `/agent` override
    // (AGENT_OVERRIDE, written by the slash command / select menu) applies.
    // v1.68.0: the dashboard's 「指定 AI 員工」 writes the `global` scope; it
    // now applies (to guilds and DMs) wherever no per-guild value is set.
    let guild_agent_override = if agent_name.is_none() {
        match settings
            .get_scoped_or_global("discord", scope_id, keys::AGENT_OVERRIDE)
            .await
        {
            Some(name) if !name.is_empty() => {
                let reg = ctx.registry.read().await;
                if reg.get(&name).is_some() {
                    Some(name)
                } else {
                    warn!(
                        "Discord guild {guild_id}: agent_override '{name}' is not a loaded agent — ignoring"
                    );
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    };
    let effective_agent: Option<String> =
        agent_name.map(|s| s.to_string()).or(guild_agent_override);

    // ── Get agent display name for embed footer ──
    let display_name = {
        let reg = ctx.registry.read().await;
        match &effective_agent {
            Some(name) => reg.get(name).map(|a| a.config.agent.display_name.clone()),
            None => reg
                .main_agent()
                .map(|a| a.config.agent.display_name.clone()),
        }
    };

    let context = crate::approval::DecisionContext {
        channel: "discord".into(),
        account_id: app_id.into(),
        conversation_id: reply_channel_id.clone(),
        principal_id: user_id.into(),
    };
    let target = crate::approval::TrustedReplyTarget::new(
        context,
        token.to_string(),
        reply_channel_id.clone(),
        None,
    )
    .map(|target| {
        target
            .with_user_access_policy(user_access_policy)
            .with_decision_access_scope(crate::decision_notify::DecisionAccessScope {
                channel_id: (!guild_id.is_empty()).then_some(channel_id),
                guild_id: (!guild_id.is_empty()).then_some(guild_id),
                session_id: Some(&session_id),
            })
    });
    let guarded = crate::approval::scope_trusted_reply(target, async {
        if let Some(agent) = &effective_agent {
            build_guarded_reply_for_agent(
                clean_content,
                ctx,
                agent,
                &session_id,
                user_id,
                Some(on_progress),
            )
            .await
        } else {
            build_guarded_reply_with_session(
                clean_content,
                ctx,
                &session_id,
                user_id,
                Some(on_progress),
            )
            .await
        }
    })
    .await;

    // Stop typing (explicit drop; also runs automatically on panic via Drop)
    drop(typing_guard);

    // WP1.3: 📎DELIVER: outbound — upload any generated files to Discord and
    // strip the marker. Byte-identical no-op when no marker is present.
    let reply = {
        let sender = crate::channel_sender::DiscordSender {
            bot_token: token.to_string(),
            channel_id: reply_channel_id.clone(),
            user_id: user_id.to_string(),
            http: http.clone(),
        };
        if !guarded.still_valid().await {
            return;
        }
        crate::channel_reply::deliver_documents_for_reply_guarded(
            ctx.as_ref(),
            effective_agent.as_deref(),
            guarded.text.clone(),
            &sender,
            Some(&guarded),
        )
        .await
    };

    // ── Guard: don't send empty replies (Discord rejects empty content) ──
    if reply.trim().is_empty() {
        warn!(channel_id, "Discord: reply is empty — skipping send");
        return;
    }

    // ── Send reply with embed + buttons (respecting per-guild response_mode) ──
    let response_mode = channel_format::ResponseMode::parse(
        &settings
            .get_with_fallback("discord", scope_id, keys::RESPONSE_MODE, "auto")
            .await,
    );
    let mut payloads = channel_format::to_discord_messages_mode(
        &reply,
        display_name.as_deref(),
        false,
        response_mode,
    );

    // Reply to the original message so the sender gets a notification
    // (on the FIRST message only). Skip message_reference when:
    // 1. We just created a new thread (original message is in the parent channel)
    // 2. The reply target channel differs from the original message's channel
    //    (can happen when thread detection missed or gateway state is stale)
    if !created_thread && reply_channel_id == channel_id {
        if let Some(obj) = payloads.first_mut().and_then(|p| p.as_object_mut()) {
            obj.insert(
                "message_reference".to_string(),
                json!({
                    "message_id": message_id,
                    "channel_id": channel_id,
                    "fail_if_not_exists": false,
                }),
            );
        }
    }

    // Add conversation buttons to the LAST message of the reply — the P1
    // goal-intent confirmation buttons when this is the specific reply that
    // just appended the confirmation menu, otherwise the ordinary
    // conversation-control buttons (unchanged from before P1). A raced/
    // consumed nonce degrades to the ordinary buttons; the text menu
    // embedded in `reply` still works fine on its own.
    let buttons = if crate::goal_intent::reply_has_confirmation_menu(&reply) {
        crate::goal_intent::pending_button_nonce(&session_id)
            .map(|nonce| channel_format::discord_gintent_buttons(&nonce))
            .unwrap_or_else(|| channel_format::discord_conversation_buttons(&session_id))
    } else {
        channel_format::discord_conversation_buttons(&session_id)
    };
    if let Some(obj) = payloads.last_mut().and_then(|p| p.as_object_mut()) {
        obj.insert("components".to_string(), json!([buttons]));
    }

    // ── Delete progress message now that we have the real reply ──
    let pmid_val = match progress_msg_id.lock() {
        Ok(mut g) => g.take(),
        Err(e) => e.into_inner().take(),
    };
    if let Some(pmid) = pmid_val {
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

    // ── Send every message (long replies span several; nothing is dropped) ──
    for payload in payloads {
        if !guarded.still_valid().await {
            return;
        }
        if let Err(e) = send_discord_message(http, token, &reply_channel_id, payload).await {
            warn!(
                channel_id = %e.channel_id,
                status = ?e.status,
                "Discord reply delivery failed: {e}"
            );
            let event = crate::protocol::WsFrame::event(
                "channels.send_failed",
                json!({
                    "channel": "discord",
                    "target_channel_id": e.channel_id,
                    "http_status": e.status,
                    "error": e.detail,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            );
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = ctx.event_tx.send(json);
            }
            break; // don't spam retries for the remaining chunks of one reply
        }
    }

    // ── Guide message in original channel when a new thread was created ──
    // Users may not notice the thread indicator on their message, so send a
    // brief pointer in the channel (as a reply) that auto-deletes after 30s.
    if created_thread {
        let guide_http = http.clone();
        let guide_token = token.to_string();
        let guide_channel = channel_id.to_string();
        let guide_msg_id = message_id.to_string();
        tokio::spawn(async move {
            let guide = json!({
                "content": "💬 已在對話串中回覆 ↓",
                "message_reference": {
                    "message_id": guide_msg_id,
                    "channel_id": guide_channel,
                    "fail_if_not_exists": false,
                },
            });
            let resp = guide_http
                .post(format!("{DISCORD_API}/channels/{guide_channel}/messages"))
                .header("Authorization", format!("Bot {guide_token}"))
                .json(&guide)
                .send()
                .await;
            // Auto-delete the guide message after 30 seconds to keep channel clean
            if let Ok(r) = resp {
                if let Ok(body) = r.json::<serde_json::Value>().await {
                    if let Some(mid) = body["id"].as_str() {
                        let mid = mid.to_string();
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        let _ = guide_http
                            .delete(format!(
                                "{DISCORD_API}/channels/{guide_channel}/messages/{mid}"
                            ))
                            .header("Authorization", format!("Bot {guide_token}"))
                            .send()
                            .await;
                    }
                }
            }
        });
    }
}

/// Strip `<@BOT_ID>` mentions from message content.
fn strip_bot_mention(text: &str, bot_id: &str) -> String {
    text.replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "") // Nickname mention variant
        .trim()
        .to_string()
}

/// Create a thread from a message. Returns the thread channel_id.
/// Discord's allowed `auto_archive_duration` values (minutes). Anything
/// else (unset, malformed) is the 24 h default.
fn thread_archive_minutes(raw: &str) -> u32 {
    match raw.trim().parse::<u32>() {
        Ok(m @ (60 | 1440 | 4320 | 10080)) => m,
        _ => 1440,
    }
}

async fn create_thread(
    http: &reqwest::Client,
    token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
    archive_minutes: u32,
) -> Option<String> {
    // Thread name: first 97 chars, filter control characters (safe for CJK multi-byte)
    let name: String = content
        .chars()
        .filter(|c| !c.is_control())
        .take(97)
        .collect();
    let name = if content.chars().filter(|c| !c.is_control()).count() > 97 {
        format!("{name}...")
    } else {
        name
    };

    let url = format!("{DISCORD_API}/channels/{channel_id}/messages/{message_id}/threads");
    #[cfg(test)]
    let url = crate::test_channel_provider::url(token, &url);
    let resp = http
        .post(url)
        .header("Authorization", format!("Bot {token}"))
        .json(&json!({
            "name": name,
            "auto_archive_duration": archive_minutes
        }))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(
            "Discord: failed to create thread ({status}): {}",
            truncate_bytes(&body, 200)
        );
        return None;
    }

    let data: Value = resp.json().await.ok()?;
    let thread_id = data["id"].as_str()?.to_string();
    info!("Discord: created thread {thread_id}");
    Some(thread_id)
}

/// Error type for Discord message send failures, carrying enough context
/// for the caller to emit a structured dashboard event.
#[derive(Debug)]
struct DiscordSendError {
    status: Option<u16>,
    detail: String,
    channel_id: String,
}

impl std::fmt::Display for DiscordSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(code) = self.status {
            write!(f, "HTTP {code}: {}", self.detail)
        } else {
            write!(f, "{}", self.detail)
        }
    }
}

/// Send a message to a Discord channel, handling 2000 char limit.
/// Returns `Ok(())` on success, or `Err(DiscordSendError)` on the first failure.
async fn send_discord_message(
    http: &reqwest::Client,
    token: &str,
    channel_id: &str,
    payload: Value,
) -> Result<(), DiscordSendError> {
    // Check if the payload has plain content that needs splitting
    if let Some(content) = payload["content"].as_str() {
        if content.len() > channel_format::limits::DISCORD_MESSAGE {
            let chunks = split_text(content, channel_format::limits::DISCORD_MESSAGE - 100);
            for chunk in chunks.iter() {
                let msg = json!({ "content": chunk });
                send_raw(http, token, channel_id, &msg).await?;
            }
            return Ok(());
        }
    }

    send_raw(http, token, channel_id, &payload).await
}

async fn send_raw(
    http: &reqwest::Client,
    token: &str,
    channel_id: &str,
    payload: &Value,
) -> Result<(), DiscordSendError> {
    // Components (buttons/selects) are sent as-is — `handle_component_interaction`
    // handles the resulting INTERACTION_CREATE (type 3) callbacks.
    let cleaned = payload.clone();

    let url = format!("{DISCORD_API}/channels/{channel_id}/messages");
    #[cfg(test)]
    let url = crate::test_channel_provider::url(token, &url);
    match http
        .post(url)
        .header("Authorization", format!("Bot {token}"))
        .json(&cleaned)
        .send()
        .await
    {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let detail = truncate_bytes(&body, 200).to_string();

            // Retry without message_reference when Discord reports a cross-channel
            // reference error.  This can happen when gateway channel metadata is
            // stale (e.g. thread detection missed), or during the first message
            // after a reconnect when channel state hasn't fully propagated.
            if status.as_u16() == 400 && detail.contains("REPLIES_CANNOT_REFERENCE_OTHER_CHANNEL") {
                warn!(
                    "Discord: cross-channel message_reference detected — retrying without reference"
                );
                let mut fallback = cleaned.clone();
                if let Some(obj) = fallback.as_object_mut() {
                    obj.remove("message_reference");
                }
                match http
                    .post(format!("{DISCORD_API}/channels/{channel_id}/messages"))
                    .header("Authorization", format!("Bot {token}"))
                    .json(&fallback)
                    .send()
                    .await
                {
                    Ok(r2) if r2.status().is_success() => {
                        info!("Discord: retry without message_reference succeeded");
                        return Ok(());
                    }
                    Ok(r2) => {
                        let s2 = r2.status();
                        let b2 = r2.text().await.unwrap_or_default();
                        let d2 = truncate_bytes(&b2, 200).to_string();
                        error!("Discord send failed on retry ({s2}): {d2}");
                        return Err(DiscordSendError {
                            status: Some(s2.as_u16()),
                            detail: d2,
                            channel_id: channel_id.to_string(),
                        });
                    }
                    Err(e2) => {
                        error!("Discord send error on retry: {e2}");
                        return Err(DiscordSendError {
                            status: None,
                            detail: e2.to_string(),
                            channel_id: channel_id.to_string(),
                        });
                    }
                }
            }

            error!("Discord send failed ({status}): {detail}");
            Err(DiscordSendError {
                status: Some(status.as_u16()),
                detail,
                channel_id: channel_id.to_string(),
            })
        }
        Err(e) => {
            error!("Discord send error: {e}");
            Err(DiscordSendError {
                status: None,
                detail: e.to_string(),
                channel_id: channel_id.to_string(),
            })
        }
        _ => Ok(()),
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
            handle_slash_command(
                data,
                interaction_id,
                interaction_token,
                bot_id,
                app_id,
                http,
                token,
                ctx,
            )
            .await;
        }
        // Message Component (button = component type 2, select menu = type 3)
        3 => {
            handle_component_interaction(
                data,
                interaction_id,
                interaction_token,
                app_id,
                http,
                ctx,
            )
            .await;
        }
        _ => {
            debug!("Discord: unhandled interaction type {interaction_type}");
        }
    }
}

/// The official channel types share one classification across decision lanes.
fn discord_thread_kind(channel_type: u64) -> Option<bool> {
    match channel_type {
        10 | 11 | 12 => Some(true),
        0 | 1 | 2 | 3 | 4 | 5 | 13 | 14 | 15 | 16 => Some(false),
        _ => None,
    }
}
/// Upper bound on one attachment download for an ordinary message (F5-C).
const DISCORD_ATTACHMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Lane for an INTERACTION_CREATE: the decision pool only for a decision
/// button whose presser passes the channel access check, with the same
/// identity and scope the decision router uses (F5-C, review F4-M1). Any
/// other interaction, or a refused presser, waits for an ordinary permit and
/// is then refused by the router itself.
async fn discord_interaction_lane(
    ctx: &Arc<ReplyContext>,
    app_id: &str,
    data: &Value,
) -> crate::channel_decision_route::DiscordLane {
    use crate::channel_decision_route::DiscordLane;
    if DiscordLane::for_interaction(data) != DiscordLane::Decision {
        return DiscordLane::General;
    }
    let presser = data["user"]["id"]
        .as_str()
        .or_else(|| data["member"]["user"]["id"].as_str())
        .unwrap_or("");
    let context = crate::approval::DecisionContext {
        channel: "discord".into(),
        account_id: app_id.into(),
        conversation_id: data["channel_id"].as_str().unwrap_or("").into(),
        principal_id: presser.into(),
    };
    let session = discord_component_session(data);
    let scope = crate::decision_notify::DecisionAccessScope {
        channel_id: data["guild_id"]
            .as_str()
            .and_then(|_| data["channel_id"].as_str()),
        guild_id: data["guild_id"].as_str(),
        session_id: Some(&session),
    };
    match crate::decision_notify::check_trusted_decision_access(ctx, &context, scope, "").await {
        Ok(()) => DiscordLane::Decision,
        Err(_) => DiscordLane::General,
    }
}

/// Counts attachment downloads per bot token, so tests can prove a decision
/// message never downloads one.
#[cfg(test)]
pub(crate) mod attachment_download_probe {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    fn counts() -> &'static Mutex<HashMap<String, usize>> {
        static COUNTS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
        COUNTS.get_or_init(Default::default)
    }
    pub(crate) fn record(token: &str) {
        *counts().lock().unwrap().entry(token.into()).or_default() += 1;
    }
    pub(crate) fn count(token: &str) -> usize {
        counts().lock().unwrap().get(token).copied().unwrap_or(0)
    }
}

fn discord_bound_text(text: &str) -> bool {
    crate::channel_decision_route::is_strict_decision(text)
}
struct DiscordCurrentChannel {
    session_id: String,
    is_thread: bool,
}
async fn discord_verified_message_identity(
    http: &reqwest::Client,
    token: &str,
    channel_id: &str,
    guild_id: &str,
) -> Option<DiscordCurrentChannel> {
    if channel_id.is_empty() {
        return None;
    }
    // Official absence of guild_id identifies a DM in the authenticated
    // gateway envelope; it is not a client-supplied bypass flag.
    if guild_id.is_empty() {
        return Some(DiscordCurrentChannel {
            session_id: format!("discord:{channel_id}"),
            is_thread: false,
        });
    }
    let url = format!("{DISCORD_API}/channels/{channel_id}");
    #[cfg(test)]
    let url = crate::test_channel_provider::url(token, &url);
    let response = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let response = http
            .get(url)
            .header("Authorization", format!("Bot {token}"))
            .send()
            .await?;
        response.error_for_status()?.json::<Value>().await
    })
    .await;
    let Ok(Ok(channel)) = response else {
        return None;
    };
    if channel["id"].as_str() != Some(channel_id) || channel["guild_id"].as_str() != Some(guild_id)
    {
        return None;
    }
    let is_thread = discord_thread_kind(channel["type"].as_u64()?)?;
    Some(DiscordCurrentChannel {
        session_id: if is_thread {
            format!("discord:thread:{channel_id}")
        } else {
            format!("discord:{channel_id}")
        },
        is_thread,
    })
}

/// Resolve only platform-provided channel metadata; missing type must never
/// turn a thread into a base-channel allowlist subject.
fn discord_component_session(data: &Value) -> String {
    let channel_id = data["channel_id"].as_str().unwrap_or("");
    let channel = &data["channel"];
    if channel_id.is_empty()
        || channel["id"].as_str() != Some(channel_id)
        || channel["guild_id"]
            .as_str()
            .is_some_and(|guild| Some(guild) != data["guild_id"].as_str())
    {
        return String::new();
    }
    match channel["type"].as_u64().and_then(discord_thread_kind) {
        Some(true) => format!("discord:thread:{channel_id}"),
        Some(false) => format!("discord:{channel_id}"),
        _ => String::new(),
    }
}

/// Handle a message-component interaction (buttons / select menus).
///
/// `custom_id` format: `duduclaw:{action}[:{payload}]` where the payload may
/// itself contain `:` (session ids like `discord:thread:{id}`), so we split
/// at most 3 times.
async fn handle_component_interaction(
    data: &Value,
    interaction_id: &str,
    interaction_token: &str,
    app_id: &str,
    http: &reqwest::Client,
    ctx: &Arc<ReplyContext>,
) {
    let cdata = match data.get("data") {
        Some(d) => d,
        None => return,
    };
    let custom_id = cdata["custom_id"].as_str().unwrap_or("");
    let guild_id = data["guild_id"].as_str().unwrap_or("");

    // Goal-intent confirmation buttons (P1) — a separate, deliberately
    // UN-authorized codec from `decision_action` below (see
    // `channel_format`'s "Goal-intent confirmation" module doc): wire format
    // `gintent:<choice>:<nonce>`, NOT `duduclaw:…`, so this must be checked
    // BEFORE the `ns != "duduclaw"` gate right below — that gate would
    // otherwise silently swallow every gintent press before it's ever
    // decoded (custom_id.splitn(3, ':').next() would be "gintent", not
    // "duduclaw"). Consuming it needs no pressing-user identity, only the
    // single-use nonce.
    //
    // Deferred response (type 5, "thinking…"): `handle_gintent_button`'s
    // plan-first branch calls a live LLM (`goal_plan::generate_plan_first`),
    // which can easily exceed Discord's 3-second interaction-ack window —
    // same reason the `/ask` slash command defers (see `handle_slash_
    // command`). The follow-up is a real (non-ephemeral) message so its
    // content is visible to the whole channel, matching what typing `1`/`2`/
    // `3` as plain text would have produced.
    if let Some((choice, nonce)) = crate::goal_intent::parse_gintent_action(custom_id) {
        send_interaction_response(http, interaction_id, interaction_token, 5, None).await;
        let outcome = crate::goal_intent::handle_gintent_button(ctx, choice, &nonce).await;
        let payload = channel_format::to_discord_message(&outcome, None, false);
        edit_interaction_response(http, app_id, interaction_token, &payload).await;
        return;
    }

    let mut parts = custom_id.splitn(3, ':');
    let ns = parts.next().unwrap_or("");
    let action = parts.next().unwrap_or("");
    let payload = parts.next().unwrap_or("");
    if ns != "duduclaw" {
        debug!("Discord: ignoring non-duduclaw component: {custom_id}");
        return;
    }

    // Ephemeral confirmation helper (flags 64 = only the presser sees it).
    let ephemeral = |msg: String| json!({ "content": msg, "flags": 64 });

    // Decision buttons — every "a human must decide this" card, whichever
    // store backs it. Routed by whether the id decodes as a decision action,
    // which covers both the unified `decide` marker and every card pushed
    // before that marker existed. DM interactions carry the user at `user`,
    // guild interactions at `member.user`.
    if crate::decision_action::parse(custom_id).is_some() {
        let discord_uid = data["user"]["id"]
            .as_str()
            .or_else(|| data["member"]["user"]["id"].as_str())
            .unwrap_or("");
        let outcome = if discord_uid.is_empty() {
            Some(Err("無法識別點擊者身分".to_string()))
        } else {
            crate::decision_notify::route_verified_bound_press(
                ctx,
                &crate::approval::DecisionContext {
                    channel: "discord".into(),
                    account_id: app_id.into(),
                    conversation_id: data["channel_id"].as_str().unwrap_or("").into(),
                    principal_id: discord_uid.into(),
                },
                custom_id,
                crate::decision_notify::DecisionAccessScope {
                    channel_id: data["guild_id"]
                        .as_str()
                        .and_then(|_| data["channel_id"].as_str()),
                    guild_id: data["guild_id"].as_str(),
                    session_id: Some(&discord_component_session(data)),
                },
            )
            .await
        };
        match outcome {
            // Decision landed → light ephemeral ack. The persistent card
            // (this same message) is retired in place by the decide path
            // itself — a detached best-effort `PATCH` that clears the buttons
            // and rewrites the card to a one-line result, decoupled from this
            // interaction's 3-second/15-minute response window (see
            // `decision_card::collapse_all`).
            Some(Ok(m)) => {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(ephemeral(m)),
                )
                .await;
            }
            // Unauthorized / already settled → ephemeral note; the message
            // (and its buttons) stays for whoever IS allowed to act.
            Some(Err(m)) => {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(ephemeral(format!("⚠️ {m}"))),
                )
                .await;
            }
            // `parse` said yes and `route_press` said no — impossible unless
            // the two disagree; refuse rather than silently ignoring.
            None => {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(ephemeral("⚠️ 無效的決定動作".to_string())),
                )
                .await;
            }
        }
        return;
    }

    match action {
        "new_session" => {
            let session_id = if payload.is_empty() {
                let channel_id = data["channel_id"].as_str().unwrap_or("");
                format!("discord:{channel_id}")
            } else {
                payload.to_string()
            };
            let msg = match ctx.session_manager.delete_session(&session_id).await {
                Ok(()) => "✅ 已開啟新的對話".to_string(),
                Err(e) => format!("⚠️ 清除工作階段失敗：{e}"),
            };
            send_interaction_response(
                http,
                interaction_id,
                interaction_token,
                4,
                Some(ephemeral(msg)),
            )
            .await;
        }
        "agent_menu" => {
            let agents: Vec<String> = {
                let reg = ctx.registry.read().await;
                reg.list()
                    .iter()
                    .map(|a| a.config.agent.name.clone())
                    .collect()
            };
            if agents.is_empty() {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(ephemeral("沒有可切換的 Agent".to_string())),
                )
                .await;
                return;
            }
            let menu = channel_format::discord_agent_select_menu(&agents);
            send_interaction_response(
                http,
                interaction_id,
                interaction_token,
                4,
                Some(json!({
                    "content": "選擇此伺服器要使用的 Agent（需要「管理伺服器」權限）：",
                    "components": [menu],
                    "flags": 64
                })),
            )
            .await;
        }
        "agent_select" => {
            let selected = cdata["values"]
                .as_array()
                .and_then(|v| v.first())
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if guild_id.is_empty() {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(ephemeral("❌ 切換 Agent 只能在伺服器中使用".to_string())),
                )
                .await;
                return;
            }
            if !has_manage_guild_permission(data) {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(ephemeral(
                        "❌ 需要「管理伺服器」權限才能切換 Agent".to_string(),
                    )),
                )
                .await;
                return;
            }
            let known = {
                let reg = ctx.registry.read().await;
                reg.get(selected).is_some()
            };
            if !known {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(ephemeral(format!("❌ 找不到 Agent `{selected}`"))),
                )
                .await;
                return;
            }
            let _ = ctx
                .channel_settings
                .set("discord", guild_id, keys::AGENT_OVERRIDE, selected)
                .await;
            send_interaction_response(
                http,
                interaction_id,
                interaction_token,
                4,
                Some(ephemeral(format!(
                    "✅ 此伺服器已切換至 Agent：**{selected}**"
                ))),
            )
            .await;
        }
        // Legacy button on messages sent before v1.36 (Discord replies have
        // no voice mode) — acknowledge honestly instead of silently dropping.
        "voice_toggle" => {
            send_interaction_response(
                http,
                interaction_id,
                interaction_token,
                4,
                Some(ephemeral("ℹ️ Discord 尚不支援語音回覆模式".to_string())),
            )
            .await;
        }
        _ => {
            send_interaction_response(
                http,
                interaction_id,
                interaction_token,
                4,
                Some(ephemeral("未知的按鈕動作".to_string())),
            )
            .await;
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
    let user = data
        .get("member")
        .and_then(|m| m.get("user"))
        .or_else(|| data.get("user"));
    let user_id = user.and_then(|u| u["id"].as_str()).unwrap_or("unknown");
    let username = user
        .and_then(|u| u["username"].as_str())
        .unwrap_or("someone");

    info!("Discord /{cmd_name} from [{username}] guild:{guild_id}");

    match cmd_name {
        "ask" => {
            // Guild whitelist applies to slash commands too.
            if !guild_id.is_empty()
                && !ctx
                    .channel_settings
                    .is_guild_allowed("discord", guild_id)
                    .await
            {
                let product = crate::branding::effective_product_name(
                    &duduclaw_core::platform::duduclaw_home(),
                );
                send_interaction_response(http, interaction_id, interaction_token, 4,
                    Some(json!({"content": format!("❌ 此伺服器未被授權使用 {product}"), "flags": 64}))).await;
                return;
            }

            // Deferred response (type 5) — we'll edit it later
            send_interaction_response(http, interaction_id, interaction_token, 5, None).await;

            let prompt = cmd_data["options"]
                .as_array()
                .and_then(|opts| opts.first())
                .and_then(|o| o["value"].as_str())
                .unwrap_or("");

            // Honour the guild-level agent override (written by /agent or the
            // Switch Agent select menu).
            let scope = if guild_id.is_empty() { "dm" } else { guild_id };
            let agent_override = match ctx
                .channel_settings
                .get("discord", scope, keys::AGENT_OVERRIDE)
                .await
            {
                Some(name) if !name.is_empty() => {
                    let reg = ctx.registry.read().await;
                    if reg.get(&name).is_some() {
                        Some(name)
                    } else {
                        None
                    }
                }
                _ => None,
            };

            let session_id = format!("discord:{channel_id}");
            let guarded = if let Some(agent) = &agent_override {
                build_guarded_reply_for_agent(prompt, ctx, agent, &session_id, user_id, None).await
            } else {
                build_guarded_reply_with_session(prompt, ctx, &session_id, user_id, None).await
            };

            let agent_name = {
                let reg = ctx.registry.read().await;
                match &agent_override {
                    Some(n) => reg.get(n).map(|a| a.config.agent.display_name.clone()),
                    None => reg
                        .main_agent()
                        .map(|a| a.config.agent.display_name.clone()),
                }
            };

            let payload =
                channel_format::to_discord_message(&guarded.text, agent_name.as_deref(), false);
            if guarded.still_valid().await {
                edit_interaction_response(http, app_id, interaction_token, &payload).await;
            }
        }

        "status" => {
            let agent_info = {
                let reg = ctx.registry.read().await;
                reg.main_agent()
                    .map(|a| {
                        format!(
                            "**Agent**: {} ({})\n**Model**: {}",
                            a.config.agent.display_name,
                            a.config.agent.name,
                            a.config.model.preferred
                        )
                    })
                    .unwrap_or_else(|| "No agent configured".to_string())
            };

            let settings = &ctx.channel_settings;
            let scope = if guild_id.is_empty() { "dm" } else { guild_id };
            let mention_only = settings
                .get_bool("discord", scope, keys::MENTION_ONLY, false)
                .await;
            let auto_thread = settings
                .get_bool("discord", scope, keys::AUTO_THREAD, false)
                .await;

            let status_text = format!(
                "{agent_info}\n\n**Guild Settings**:\n\
                 Mention Only: {}\n\
                 Auto Thread: {}",
                if mention_only { "✅" } else { "❌" },
                if auto_thread { "✅" } else { "❌" },
            );

            let product =
                crate::branding::effective_product_name(&duduclaw_core::platform::duduclaw_home());
            let embed = json!({
                "embeds": [{
                    "title": format!("{product} Status"),
                    "description": status_text,
                    "color": 0xF59E0B,
                    "footer": { "text": product }
                }]
            });
            send_interaction_response(http, interaction_id, interaction_token, 4, Some(embed))
                .await;
        }

        "config" => {
            // DMs cannot modify config (would affect global scope)
            if guild_id.is_empty() {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(json!({"content": "❌ /config 只能在伺服器中使用", "flags": 64})),
                )
                .await;
                return;
            }
            // Server-side permission check: require MANAGE_GUILD
            if !has_manage_guild_permission(data) {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(json!({"content": "❌ 需要「管理伺服器」權限才能修改設定", "flags": 64})),
                )
                .await;
                return;
            }

            let sub = cmd_data["options"].as_array().and_then(|opts| opts.first());

            let sub_name = sub.and_then(|s| s["name"].as_str()).unwrap_or("");
            let scope = if guild_id.is_empty() {
                "global"
            } else {
                guild_id
            };

            match sub_name {
                "mention_only" => {
                    let enabled = sub
                        .and_then(|s| s["options"].as_array())
                        .and_then(|opts| opts.first())
                        .and_then(|o| o["value"].as_bool())
                        .unwrap_or(false);

                    let _ = ctx
                        .channel_settings
                        .set(
                            "discord",
                            scope,
                            keys::MENTION_ONLY,
                            if enabled { "true" } else { "false" },
                        )
                        .await;

                    let msg = format!(
                        "Mention-only mode: **{}**",
                        if enabled {
                            "Enabled ✅"
                        } else {
                            "Disabled ❌"
                        }
                    );
                    send_interaction_response(
                        http,
                        interaction_id,
                        interaction_token,
                        4,
                        Some(json!({"content": msg, "flags": 64})),
                    )
                    .await;
                }
                "auto_thread" => {
                    let enabled = sub
                        .and_then(|s| s["options"].as_array())
                        .and_then(|opts| opts.first())
                        .and_then(|o| o["value"].as_bool())
                        .unwrap_or(false);

                    let _ = ctx
                        .channel_settings
                        .set(
                            "discord",
                            scope,
                            keys::AUTO_THREAD,
                            if enabled { "true" } else { "false" },
                        )
                        .await;

                    let msg = format!(
                        "Auto-thread mode: **{}**",
                        if enabled {
                            "Enabled ✅"
                        } else {
                            "Disabled ❌"
                        }
                    );
                    send_interaction_response(
                        http,
                        interaction_id,
                        interaction_token,
                        4,
                        Some(json!({"content": msg, "flags": 64})),
                    )
                    .await;
                }
                "show" => {
                    let all = ctx.channel_settings.get_all("discord", scope).await;
                    let text = if all.is_empty() {
                        "No custom settings configured. Using defaults.".to_string()
                    } else {
                        all.iter()
                            .map(|(k, v)| format!("`{k}`: {v}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    send_interaction_response(
                        http,
                        interaction_id,
                        interaction_token,
                        4,
                        Some(json!({"content": text, "flags": 64})),
                    )
                    .await;
                }
                _ => {
                    send_interaction_response(
                        http,
                        interaction_id,
                        interaction_token,
                        4,
                        Some(json!({"content": "Unknown subcommand", "flags": 64})),
                    )
                    .await;
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
                    send_interaction_response(
                        http,
                        interaction_id,
                        interaction_token,
                        4,
                        Some(json!({"content": info, "flags": 64})),
                    )
                    .await;
                }
                "reset" => {
                    let msg = match ctx.session_manager.delete_session(&session_id).await {
                        Ok(()) => format!("✅ Session `{session_id}` cleared."),
                        Err(e) => format!("⚠️ Failed to clear session: {e}"),
                    };
                    send_interaction_response(
                        http,
                        interaction_id,
                        interaction_token,
                        4,
                        Some(json!({"content": msg, "flags": 64})),
                    )
                    .await;
                }
                _ => {
                    send_interaction_response(
                        http,
                        interaction_id,
                        interaction_token,
                        4,
                        Some(json!({"content": "Unknown subcommand", "flags": 64})),
                    )
                    .await;
                }
            }
        }

        "agent" => {
            // DMs cannot switch agent (would affect global scope)
            if guild_id.is_empty() {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(json!({"content": "❌ /agent 只能在伺服器中使用", "flags": 64})),
                )
                .await;
                return;
            }
            // Require MANAGE_GUILD to switch agent
            if !has_manage_guild_permission(data) {
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(
                        json!({"content": "❌ 需要「管理伺服器」權限才能切換 Agent", "flags": 64}),
                    ),
                )
                .await;
                return;
            }

            let agent_name = cmd_data["options"]
                .as_array()
                .and_then(|opts| opts.first())
                .and_then(|o| o["value"].as_str())
                .unwrap_or("");

            let scope = if guild_id.is_empty() {
                "global"
            } else {
                guild_id
            };
            let reg = ctx.registry.read().await;
            if reg.get(agent_name).is_some() {
                drop(reg);
                let _ = ctx
                    .channel_settings
                    .set("discord", scope, keys::AGENT_OVERRIDE, agent_name)
                    .await;
                let msg = format!("Switched to agent: **{agent_name}**");
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(json!({"content": msg})),
                )
                .await;
            } else {
                let agents: Vec<String> = reg
                    .list()
                    .iter()
                    .map(|a| a.config.agent.name.clone())
                    .collect();
                let msg = format!(
                    "Agent `{agent_name}` not found.\nAvailable: {}",
                    agents.join(", ")
                );
                send_interaction_response(
                    http,
                    interaction_id,
                    interaction_token,
                    4,
                    Some(json!({"content": msg, "flags": 64})),
                )
                .await;
            }
        }

        _ => {
            send_interaction_response(
                http,
                interaction_id,
                interaction_token,
                4,
                Some(json!({"content": "Unknown command", "flags": 64})),
            )
            .await;
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
    #[cfg(test)]
    let url = crate::test_channel_provider::url(interaction_token, &url);
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
    let cleaned = data.clone();

    let url = format!("{DISCORD_API}/webhooks/{app_id}/{interaction_token}/messages/@original");
    match http.patch(&url).json(&cleaned).send().await {
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(
                "Discord edit interaction failed ({status}): {}",
                truncate_bytes(&body, 200)
            );
        }
        Err(e) => error!("Discord edit interaction error: {e}"),
        _ => {}
    }
}

// ── Channel → guild id persistence (W2-7 deep-link coords) ──
//
// `channel_link.rs`'s Discord deep link (`discord.com/channels/<guild>/...`)
// needs the guild id, but the gateway only sees it transiently on inbound
// Gateway events (`MESSAGE_CREATE`'s `guild_id` field) — nothing durable
// remembered a channel's guild anywhere a later `/goal` task creation or
// decision-card push could read it back from. Every inbound guild message
// (not DMs — `guild_id` is empty there) upserts `channel_id -> guild_id`
// here; callers snapshot it onto their own record (`TaskRow.
// source_discord_guild_id`, `decision_message_store::CardEntry`) at write
// time rather than re-reading this cache live, so a link keeps resolving
// even after the bot leaves the guild or this cache is pruned/reset.

const CHANNEL_GUILD_STORE_FILE: &str = "discord_channel_guilds.json";
/// Hard cap on distinct channels remembered. No per-entry timestamp is
/// tracked (this is a small best-effort cache, not an audit log), so once
/// full, unseen channels are simply not recorded until the file is pruned by
/// an operator — degrading to "no link" for the newest channels only, never
/// panicking or growing the file unbounded.
const CHANNEL_GUILD_STORE_CAP: usize = 1_000;

fn channel_guild_store_path(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join(CHANNEL_GUILD_STORE_FILE)
}

fn load_channel_guild_store(home_dir: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(channel_guild_store_path(home_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record `channel_id -> guild_id`. Best-effort and non-fatal: a write
/// failure only means a later deep link degrades to `None`, never blocks
/// message handling. No-ops for DMs (`guild_id` empty) and for a mapping
/// that's already up to date (skips a disk write on every single message in
/// a hot channel).
pub(crate) fn record_channel_guild(home_dir: &Path, channel_id: &str, guild_id: &str) {
    if channel_id.is_empty() || guild_id.is_empty() {
        return;
    }
    let path = channel_guild_store_path(home_dir);
    let channel_id = channel_id.to_string();
    let guild_id = guild_id.to_string();
    let result = duduclaw_core::with_file_lock(&path, || {
        let mut store = load_channel_guild_store(home_dir);
        if store.get(&channel_id).map(String::as_str) == Some(guild_id.as_str()) {
            return Ok(()); // already current — skip the write
        }
        if !store.contains_key(&channel_id) && store.len() >= CHANNEL_GUILD_STORE_CAP {
            return Ok(()); // full; degrade rather than grow unbounded
        }
        store.insert(channel_id.clone(), guild_id.clone());
        let bytes = serde_json::to_vec(&store).map_err(std::io::Error::other)?;
        // Atomic replace (temp + rename), matching decision_message_store's
        // pattern for the same class of small durable JSON state.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)
    });
    if let Err(e) = result {
        tracing::debug!(%channel_id, error = %e, "discord: failed to persist channel->guild mapping (non-fatal)");
    }
}

/// Look up a previously-recorded guild id for a Discord channel. `None` when
/// never seen (DM, or no message from that channel has reached this gateway
/// yet) — the caller (task/decision-card creation) must treat this as an
/// honest gap, not fabricate a value.
pub fn guild_id_for_channel(home_dir: &Path, channel_id: &str) -> Option<String> {
    if channel_id.is_empty() {
        return None;
    }
    load_channel_guild_store(home_dir).get(channel_id).cloned()
}

#[cfg(test)]
mod channel_guild_tests {
    use super::{CHANNEL_GUILD_STORE_CAP, guild_id_for_channel, record_channel_guild};

    #[test]
    fn round_trip_write_then_read() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(guild_id_for_channel(dir.path(), "chan-1"), None);
        record_channel_guild(dir.path(), "chan-1", "guild-1");
        assert_eq!(
            guild_id_for_channel(dir.path(), "chan-1"),
            Some("guild-1".to_string())
        );
    }

    #[test]
    fn dm_messages_are_never_recorded() {
        // MESSAGE_CREATE's guild_id is empty for DMs — must not pollute the
        // store with a channel->"" mapping.
        let dir = tempfile::tempdir().unwrap();
        record_channel_guild(dir.path(), "dm-chan", "");
        assert_eq!(guild_id_for_channel(dir.path(), "dm-chan"), None);
    }

    #[test]
    fn blank_channel_id_is_never_recorded() {
        let dir = tempfile::tempdir().unwrap();
        record_channel_guild(dir.path(), "", "guild-1");
        assert_eq!(guild_id_for_channel(dir.path(), ""), None);
    }

    #[test]
    fn lookup_unknown_channel_is_none_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(guild_id_for_channel(dir.path(), "never-seen"), None);
    }

    #[test]
    fn guild_can_change_for_the_same_channel_id() {
        // Exceedingly unlikely on real Discord (snowflakes don't get
        // reused across guilds) but the store must not get stuck on a
        // stale value if it ever did.
        let dir = tempfile::tempdir().unwrap();
        record_channel_guild(dir.path(), "chan-1", "guild-1");
        record_channel_guild(dir.path(), "chan-1", "guild-2");
        assert_eq!(
            guild_id_for_channel(dir.path(), "chan-1"),
            Some("guild-2".to_string())
        );
    }

    #[test]
    fn store_does_not_grow_past_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(CHANNEL_GUILD_STORE_CAP + 5) {
            record_channel_guild(dir.path(), &format!("chan-{i}"), "guild-x");
        }
        let store = super::load_channel_guild_store(dir.path());
        assert!(store.len() <= CHANNEL_GUILD_STORE_CAP);
        // The first CAP channels seen must still be present (fail-safe:
        // degrade for the newest unseen channels, not evict known-good data).
        assert_eq!(store.get("chan-0"), Some(&"guild-x".to_string()));
    }
}

// ── Config ──────────────────────────────────────────────────

async fn read_discord_token(home_dir: &Path) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "discord_bot_token")
        .await
}

#[cfg(test)]
mod backoff_tests {
    use super::token_check_backoff_secs;

    #[test]
    fn schedule_grows_exponentially_and_caps_at_15min() {
        // Lock down the published progression. If anyone touches the
        // formula, this test should force them to update the schedule
        // intentionally.
        assert_eq!(token_check_backoff_secs(1), 60);
        assert_eq!(token_check_backoff_secs(2), 120);
        assert_eq!(token_check_backoff_secs(3), 240);
        assert_eq!(token_check_backoff_secs(4), 480);
        assert_eq!(token_check_backoff_secs(5), 900); // capped
        assert_eq!(token_check_backoff_secs(6), 900); // still capped
    }

    #[test]
    fn streak_zero_does_not_underflow_or_return_zero() {
        // streak=0 shouldn't ever happen in the call site (we increment
        // before computing), but guard against the regression where
        // `n.saturating_sub(1)` of an underflow returned 0s sleeps.
        assert_eq!(token_check_backoff_secs(0), 60);
    }

    #[test]
    fn extreme_streak_is_clamped_not_overflowed() {
        // Catch the bug where `60u64 << streak` would overflow at streak=58.
        // checked_shl returning None should fall through to CAP.
        assert_eq!(token_check_backoff_secs(100), 900);
        assert_eq!(token_check_backoff_secs(u32::MAX), 900);
    }
}

#[cfg(test)]
mod invalid_session_tests {
    use super::invalid_session_jitter_ms;

    #[test]
    fn jitter_stays_within_discord_spec_1_to_5s() {
        // Discord spec is 1000-5000ms. Sample a few values across the
        // nanos space and prove they all fall in the band.
        for nanos in [0u32, 1, 999, 1000, 3999, 4000, 7777, u32::MAX] {
            let ms = invalid_session_jitter_ms(nanos);
            assert!(
                (1000..=5000).contains(&ms),
                "jitter for nanos={nanos} = {ms} ms, expected [1000, 5000]"
            );
        }
    }

    #[test]
    fn jitter_distributes_across_band_not_clipped_to_one_value() {
        // Sweep input, ensure the output set has more than one value.
        // Catches the "always 1000" regression that would silently break
        // jitter without breaking the bounds test above.
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for nanos in 0..50_000u32 {
            seen.insert(invalid_session_jitter_ms(nanos));
        }
        assert!(
            seen.len() > 100,
            "jitter looks clipped: only {} values",
            seen.len()
        );
    }
}

#[cfg(test)]
mod reply_context_tests {
    use super::*;

    #[test]
    fn referenced_message_builds_quote_block() {
        let data = serde_json::json!({
            "content": "這是你發的嗎",
            "referenced_message": {
                "author": { "id": "BOT1", "username": "trader" },
                "content": "已送出委託 2317 8 股 @264"
            }
        });
        let block = discord_reply_context(&data, "BOT1").expect("quote block");
        assert!(block.contains("2317"));
        assert!(block.contains(channel_format::QUOTED_SELF_LABEL));
    }

    #[test]
    fn non_reply_and_deleted_reference_yield_none() {
        let plain = serde_json::json!({ "content": "hello" });
        assert!(discord_reply_context(&plain, "BOT1").is_none());
        // Discord sends an explicit null when the referenced message was deleted.
        let deleted = serde_json::json!({ "content": "hi", "referenced_message": null });
        assert!(discord_reply_context(&deleted, "BOT1").is_none());
    }

    #[test]
    fn attachment_only_reference_gets_placeholder() {
        let data = serde_json::json!({
            "content": "這個檔案是什麼",
            "referenced_message": {
                "author": { "id": "U2", "username": "amy" },
                "content": "",
                "attachments": [ { "id": "1" } ]
            }
        });
        let block = discord_reply_context(&data, "BOT1").expect("quote block");
        assert!(block.contains("附件訊息"));
        assert!(block.contains("amy"));
    }
}

#[cfg(test)]
mod thread_archive_tests {
    use super::thread_archive_minutes;

    #[test]
    fn archive_minutes_accepts_only_discord_values() {
        assert_eq!(thread_archive_minutes("60"), 60);
        assert_eq!(thread_archive_minutes("4320"), 4320);
        assert_eq!(thread_archive_minutes(" 10080 "), 10080);
        assert_eq!(thread_archive_minutes("1440"), 1440);
        assert_eq!(thread_archive_minutes("30"), 1440);
        assert_eq!(thread_archive_minutes(""), 1440);
        assert_eq!(thread_archive_minutes("abc"), 1440);
    }
}

#[cfg(test)]
mod native_callback_acl_tests {
    use super::*;
    use crate::decision_notify::native_loop_fixture::*;
    use crate::test_channel_provider::TestChannelProvider;
    #[tokio::test]
    async fn actual_native_callback_rechecks_sql_revocation_and_fails_closed() {
        for mode in [
            "allow",
            "session_allow",
            "thread_allow",
            "missing_type",
            "user",
            "blocked",
            "channel",
            "guild",
            "pairing",
            "corrupt",
            "db",
            "binding",
        ] {
            let provider = TestChannelProvider::start().await;
            let fixture = NativeCuFixture::new(&provider.token).await;
            let context = crate::approval::DecisionContext {
                channel: "discord".into(),
                account_id: "A1".into(),
                principal_id: "H1".into(),
                conversation_id: "C1".into(),
            };
            let id = pending_native_request(&fixture, context).await;
            let allowed = matches!(mode, "allow" | "session_allow" | "thread_allow");
            if mode == "session_allow" || mode == "thread_allow" || mode == "missing_type" {
                fixture
                    .ctx
                    .channel_settings
                    .set(
                        "discord",
                        "global",
                        "allowed_users",
                        // F4: both the person and the conversation are listed.
                        if mode == "thread_allow" {
                            r#"["discord:thread:C1","H1"]"#
                        } else {
                            r#"["discord:C1","H1"]"#
                        },
                    )
                    .await
                    .unwrap();
            } else {
                revoke_native_access(&fixture, "discord", "G1", mode).await;
            }
            let mut data = json!({
                "guild_id": "G1",
                "channel_id": "C1",
                "channel": {"id":"C1","type":0,"guild_id":"G1"},
                "member": {"user":{"id":"H1"}},
                "data": {"custom_id": crate::decision_action::encode(
                    crate::decision_action::DecisionSource::Approval,
                    crate::decision_action::DecisionAct::Approve,
                    id.as_str()
                )}
            });
            if mode == "thread_allow" {
                data["channel"]["type"] = json!(11);
            }
            if mode == "missing_type" {
                data["channel"].as_object_mut().unwrap().remove("type");
            }
            if !allowed && mode != "missing_type" {
                provider
                    .enqueue_response("/channels/C1", json!({"id":"C1","type":0,"guild_id":"G1"}));
                handle_message_create(&json!({
                    "id": "text-revoke",
                    "author": {"id":"H1","username":"Human","bot":false},
                    "channel_id": "C1",
                    "guild_id": "G1",
                    "content": format!("確認 {}",id.as_str())
                }),"BOT",&reqwest::Client::new(),&provider.token,&fixture.ctx,None,"A1").await;
                assert_native_pending(&fixture, &id).await;
            }
            handle_component_interaction(
                &data,
                "native-test",
                &provider.token,
                "A1",
                &reqwest::Client::new(),
                &fixture.ctx,
            )
            .await;
            if allowed {
                assert_native_approved(&fixture, &id).await;
            } else {
                assert_native_pending(&fixture, &id).await;
            }
            assert!(
                provider
                    .requests()
                    .iter()
                    .any(|r| r.path.ends_with("/callback")
                        && r.body["data"]["content"].as_str().is_some_and(|text| {
                            if allowed {
                                text.contains("同意")
                            } else {
                                text.contains("⚠")
                            }
                        })),
                "native rejection did not ACK {mode}"
            );
        }
    }
}

#[cfg(test)]
mod thread_type_decision_acl_tests {
    use super::*;
    use crate::decision_notify::native_loop_fixture::*;

    #[tokio::test]
    async fn official_thread_types_use_same_session_in_native_and_text_decisions() {
        for channel_type in [10, 11, 12] {
            for mode in ["allow", "blocked", "base_only"] {
                for native in [false, true] {
                    let provider = crate::test_channel_provider::TestChannelProvider::start().await;
                    let fixture = NativeCuFixture::new(&provider.token).await;
                    let context = crate::approval::DecisionContext {
                        channel: "discord".into(),
                        account_id: "A1".into(),
                        conversation_id: "C1".into(),
                        principal_id: "H1".into(),
                    };
                    let id = pending_native_request(&fixture, context).await;
                    fixture
                        .ctx
                        .channel_settings
                        .set(
                            "discord",
                            "global",
                            "allowed_users",
                            // F4: both the person and the conversation are listed.
                            if mode == "base_only" {
                                r#"["discord:C1","H1"]"#
                            } else {
                                r#"["discord:thread:C1","H1"]"#
                            },
                        )
                        .await
                        .unwrap();
                    fixture
                        .ctx
                        .channel_settings
                        .set(
                            "discord",
                            "global",
                            "blocked_users",
                            if mode == "blocked" {
                                r#"["discord:thread:C1"]"#
                            } else {
                                "[]"
                            },
                        )
                        .await
                        .unwrap();
                    if native {
                        let data = json!({
                            "guild_id": "G1",
                            "channel_id": "C1",
                            "channel": {"id":"C1","type":channel_type,"guild_id":"G1"},
                            "member": {"user":{"id":"H1"}},
                            "data": {"custom_id": crate::decision_action::encode(
                                crate::decision_action::DecisionSource::Approval,
                                crate::decision_action::DecisionAct::Approve,
                                id.as_str()
                            )}
                        });
                        handle_component_interaction(
                            &data,
                            "thread-types",
                            &provider.token,
                            "A1",
                            &reqwest::Client::new(),
                            &fixture.ctx,
                        )
                        .await;
                    } else {
                        provider.enqueue_response(
                            "/channels/C1",
                            json!({"id":"C1","type":channel_type,"guild_id":"G1"}),
                        );
                        let data = json!({
                            "id": "thread-types",
                            "channel_id": "C1",
                            "guild_id": "G1",
                            "author": {"id":"H1","bot":false},
                            "content": format!("approve {}",id.as_str())
                        });
                        handle_message_create(
                            &data,
                            "BOT",
                            &reqwest::Client::new(),
                            &provider.token,
                            &fixture.ctx,
                            None,
                            "A1",
                        )
                        .await;
                    }
                    if mode == "allow" {
                        assert_native_approved(&fixture, &id).await;
                    } else {
                        assert_native_pending(&fixture, &id).await;
                    }
                }
            }
        }
    }

    #[test]
    fn official_partial_channel_thread_types_and_unknown_types_refuse_classification() {
        for channel_type in [10, 11, 12] {
            assert_eq!(discord_thread_kind(channel_type), Some(true));
            assert_eq!(
                discord_component_session(
                    &json!({"channel_id":"C1","channel":{"id":"C1","type":channel_type}})
                ),
                "discord:thread:C1"
            );
        }
        assert_eq!(discord_thread_kind(999), None);
        assert_eq!(
            discord_component_session(&json!({"channel_id":"C1","channel":{"id":"C1","type":999}})),
            ""
        );
    }
}

#[cfg(test)]
mod current_channel_lookup_acl_tests {
    use super::*;
    use crate::decision_notify::native_loop_fixture::*;

    #[tokio::test]
    async fn message_create_resolves_current_channel_not_child_thread_or_absent_type() {
        for mode in [
            "parent_blocked",
            "type10_allow",
            "type11_allow",
            "type12_allow",
            "type10_base",
            "type11_base",
            "type12_base",
            "error",
            "id_mismatch",
            "guild_mismatch",
            "unknown",
        ] {
            for transformed in [false, true] {
                let provider = crate::test_channel_provider::TestChannelProvider::start().await;
                let fixture = NativeCuFixture::new(&provider.token).await;
                let context = crate::approval::DecisionContext {
                    channel: "discord".into(),
                    account_id: "A1".into(),
                    conversation_id: "C1".into(),
                    principal_id: "H1".into(),
                };
                let id = pending_native_request(&fixture, context).await;
                let kind = if mode.contains("11") {
                    11
                } else if mode.contains("12") {
                    12
                } else if mode == "parent_blocked" {
                    0
                } else {
                    10
                };
                let allow = mode.ends_with("_allow");
                fixture
                    .ctx
                    .channel_settings
                    .set(
                        "discord",
                        "global",
                        "allowed_users",
                        // F4: both the person and the conversation are listed.
                        if allow {
                            r#"["discord:thread:C1","H1"]"#
                        } else if mode.ends_with("_base") {
                            r#"["discord:C1","H1"]"#
                        } else {
                            r#"["H1"]"#
                        },
                    )
                    .await
                    .unwrap();
                fixture
                    .ctx
                    .channel_settings
                    .set(
                        "discord",
                        "global",
                        "blocked_users",
                        if mode == "parent_blocked" {
                            r#"["discord:C1"]"#
                        } else {
                            "[]"
                        },
                    )
                    .await
                    .unwrap();
                let mut channel = json!({"id":"C1","type":kind,"guild_id":"G1"});
                if mode == "id_mismatch" {
                    channel["id"] = json!("OTHER");
                }
                if mode == "guild_mismatch" {
                    channel["guild_id"] = json!("OTHER");
                }
                if mode == "unknown" {
                    channel["type"] = json!(999);
                }
                provider.enqueue_response("/channels/C1", channel);
                if mode == "error" {
                    provider.refuse();
                }
                let message = json!({
                    "id": "current-channel",
                    "channel_id": "C1",
                    "guild_id": "G1",
                    "thread": {"id":"CHILD"},
                    "position": 4,
                    "author": {"id":"H1","bot":false},
                    "mentions": [{"id":"BOT"}],
                    "content": format!("{}approve {}",if transformed { "<@BOT> " } else { "" },id.as_str())
                });
                handle_message_create(
                    &message,
                    "BOT",
                    &reqwest::Client::new(),
                    &provider.token,
                    &fixture.ctx,
                    None,
                    "A1",
                )
                .await;
                if allow {
                    assert_native_approved(&fixture, &id).await;
                } else {
                    assert_native_pending(&fixture, &id).await;
                }
                let lookups: Vec<_> = provider
                    .requests()
                    .into_iter()
                    .filter(|r| r.path.ends_with("/channels/C1"))
                    .collect();
                assert_eq!(lookups.len(), 1, "{mode}, transformed={transformed}");
                assert_eq!(
                    lookups[0].authorization.as_deref(),
                    Some(format!("Bot {}", provider.token).as_str())
                );
            }
        }
    }
}

#[cfg(test)]
mod ordinary_dispatch_probe {
    use std::sync::{Arc, Mutex, OnceLock};
    type Records = Arc<Mutex<Vec<String>>>;
    fn probes() -> &'static Mutex<std::collections::HashMap<String, Records>> {
        static PROBES: OnceLock<Mutex<std::collections::HashMap<String, Records>>> =
            OnceLock::new();
        PROBES.get_or_init(Default::default)
    }
    pub(super) fn install(token: &str) -> Records {
        let records = Arc::new(Mutex::new(Vec::new()));
        probes()
            .lock()
            .unwrap()
            .insert(token.into(), records.clone());
        records
    }
    pub(super) fn record_if_installed(token: &str, session: &str) -> bool {
        let Some(records) = probes().lock().unwrap().remove(token) else {
            return false;
        };
        records.lock().unwrap().push(session.into());
        true
    }
    pub(super) fn remove(token: &str) {
        probes().lock().unwrap().remove(token);
    }
}

#[cfg(test)]
mod ordinary_current_channel_acl_tests {
    use super::*;
    use crate::decision_notify::native_loop_fixture::NativeCuFixture;
    async fn seed(fixture: &NativeCuFixture) {
        for (session, text) in [
            ("discord:C1", "base-history"),
            ("discord:thread:C1", "thread-history"),
        ] {
            fixture
                .ctx
                .session_manager
                .get_or_create(session, "alice")
                .await
                .unwrap();
            fixture
                .ctx
                .session_manager
                .append_message(session, "user", text, 1)
                .await
                .unwrap();
        }
    }
    fn archived(fixture: &NativeCuFixture, session: &str) -> bool {
        let db = rusqlite::Connection::open(fixture.home.path().join("sessions.db")).unwrap();
        db.query_row(
            "SELECT archived_at IS NOT NULL FROM sessions WHERE id=?1",
            [session],
            |row| row.get(0),
        )
        .unwrap()
    }
    fn message(content: &str) -> Value {
        json!({
            "id": "ordinary-current",
            "channel_id": "C1",
            "guild_id": "G1",
            "thread": {"id":"CHILD"},
            "position": 4,
            "author": {"id":"H1","bot":false},
            "content": content
        })
    }
    async fn invoke(
        fixture: &NativeCuFixture,
        provider: &crate::test_channel_provider::TestChannelProvider,
        content: &str,
    ) {
        handle_message_create(
            &message(content),
            "BOT",
            &reqwest::Client::new(),
            &provider.token,
            &fixture.ctx,
            None,
            "A1",
        )
        .await;
    }
    #[tokio::test]
    async fn ordinary_and_new_use_authoritative_thread_subject_with_auto_thread_on_or_off() {
        for kind in [10, 11, 12] {
            for auto_thread in [false, true] {
                for mode in ["thread_allow", "thread_blocked", "base_only"] {
                    for content in ["ordinary conversation", "/new"] {
                        let provider =
                            crate::test_channel_provider::TestChannelProvider::start().await;
                        let fixture = NativeCuFixture::new(&provider.token).await;
                        seed(&fixture).await;
                        let records = ordinary_dispatch_probe::install(&provider.token);
                        fixture
                            .ctx
                            .channel_settings
                            .set(
                                "discord",
                                "G1",
                                "auto_thread",
                                if auto_thread { "true" } else { "false" },
                            )
                            .await
                            .unwrap();
                        fixture
                            .ctx
                            .channel_settings
                            .set(
                                "discord",
                                "global",
                                "allowed_users",
                                // F4: both the person and the conversation are listed.
                                match mode {
                                    "thread_allow" => r#"["discord:thread:C1","H1"]"#,
                                    "thread_blocked" => r#"["H1"]"#,
                                    _ => r#"["discord:C1","H1"]"#,
                                },
                            )
                            .await
                            .unwrap();
                        fixture
                            .ctx
                            .channel_settings
                            .set(
                                "discord",
                                "global",
                                "blocked_users",
                                if mode == "thread_blocked" {
                                    r#"["discord:thread:C1"]"#
                                } else {
                                    "[]"
                                },
                            )
                            .await
                            .unwrap();
                        provider.enqueue_response(
                            "/channels/C1",
                            json!({"id":"C1","guild_id":"G1","type":kind}),
                        );
                        invoke(&fixture, &provider, content).await;
                        let allowed = mode == "thread_allow";
                        assert_eq!(
                            *records.lock().unwrap(),
                            if allowed && content != "/new" {
                                vec!["discord:thread:C1".to_owned()]
                            } else {
                                vec![]
                            },
                            "kind={kind} auto={auto_thread} mode={mode} content={content}"
                        );
                        let base = fixture
                            .ctx
                            .session_manager
                            .get_messages("discord:C1")
                            .await
                            .unwrap();
                        let thread = fixture
                            .ctx
                            .session_manager
                            .get_messages("discord:thread:C1")
                            .await
                            .unwrap();
                        assert_eq!(base.len(), 1);
                        assert_eq!(base[0].content, "base-history");
                        assert_eq!(thread.len(), 1, "archiving retains replayable history");
                        assert_eq!(thread[0].content, "thread-history");
                        assert!(!archived(&fixture, "discord:C1"));
                        assert_eq!(
                            archived(&fixture, "discord:thread:C1"),
                            allowed && content == "/new"
                        );
                        let requests = provider.requests();
                        assert_eq!(
                            requests
                                .iter()
                                .filter(|r| r.path.ends_with("/channels/C1"))
                                .count(),
                            1
                        );
                        assert!(!requests.iter().any(|r| r.path.ends_with("/threads")));
                        assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
                        ordinary_dispatch_probe::remove(&provider.token);
                    }
                }
            }
        }
    }
    #[tokio::test]
    async fn failed_current_channel_lookup_cannot_dispatch_reset_history_or_create_thread() {
        for mode in ["error", "id", "guild", "unknown"] {
            for content in ["ordinary conversation", "/new"] {
                let provider = crate::test_channel_provider::TestChannelProvider::start().await;
                let fixture = NativeCuFixture::new(&provider.token).await;
                seed(&fixture).await;
                let records = ordinary_dispatch_probe::install(&provider.token);
                fixture
                    .ctx
                    .channel_settings
                    .set("discord", "G1", "auto_thread", "true")
                    .await
                    .unwrap();
                let mut channel = json!({"id":"C1","guild_id":"G1","type":10});
                match mode {
                    "error" => provider.refuse(),
                    "id" => channel["id"] = json!("OTHER"),
                    "guild" => channel["guild_id"] = json!("OTHER"),
                    _ => channel["type"] = json!(999),
                }
                provider.enqueue_response("/channels/C1", channel);
                invoke(&fixture, &provider, content).await;
                assert!(records.lock().unwrap().is_empty());
                for session in ["discord:C1", "discord:thread:C1"] {
                    assert_eq!(
                        fixture
                            .ctx
                            .session_manager
                            .get_messages(session)
                            .await
                            .unwrap()
                            .len(),
                        1
                    );
                }
                assert!(!archived(&fixture, "discord:C1"));
                assert!(!archived(&fixture, "discord:thread:C1"));
                assert_eq!(
                    provider.requests().len(),
                    1,
                    "lookup failure sent other requests"
                );
                assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
                ordinary_dispatch_probe::remove(&provider.token);
            }
        }
    }
    #[tokio::test]
    async fn authoritative_base_channel_can_create_thread_and_dispatch_new_identity() {
        let provider = crate::test_channel_provider::TestChannelProvider::start().await;
        let fixture = NativeCuFixture::new(&provider.token).await;
        seed(&fixture).await;
        let records = ordinary_dispatch_probe::install(&provider.token);
        fixture
            .ctx
            .channel_settings
            .set("discord", "G1", "auto_thread", "true")
            .await
            .unwrap();
        fixture
            .ctx
            .channel_settings
            // F4: both the person and the conversation are listed.
            .set("discord", "global", "allowed_users", r#"["H1","discord:C1"]"#)
            .await
            .unwrap();
        provider.enqueue_response("/channels/C1", json!({"id":"C1","guild_id":"G1","type":0}));
        provider.enqueue_response("/threads", json!({"id":"NEW"}));
        invoke(&fixture, &provider, "ordinary conversation").await;
        assert_eq!(*records.lock().unwrap(), vec!["discord:thread:NEW"]);
        assert_eq!(
            provider
                .requests()
                .iter()
                .filter(|r| r.path.ends_with("/threads"))
                .count(),
            1
        );
        assert_eq!(
            fixture
                .ctx
                .session_manager
                .get_messages("discord:C1")
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn authoritative_base_subject_with_auto_thread_off_preserves_thread_history() {
        for mode in ["base_allow", "base_blocked", "thread_only"] {
            for content in ["ordinary conversation", "/new"] {
                let provider = crate::test_channel_provider::TestChannelProvider::start().await;
                let fixture = NativeCuFixture::new(&provider.token).await;
                seed(&fixture).await;
                let records = ordinary_dispatch_probe::install(&provider.token);
                fixture
                    .ctx
                    .channel_settings
                    .set("discord", "G1", "auto_thread", "false")
                    .await
                    .unwrap();
                fixture
                    .ctx
                    .channel_settings
                    .set(
                        "discord",
                        "global",
                        "allowed_users",
                        // F4: both the person and the conversation are listed.
                        match mode {
                            "base_allow" => r#"["discord:C1","H1"]"#,
                            "base_blocked" => r#"["H1"]"#,
                            _ => r#"["discord:thread:C1","H1"]"#,
                        },
                    )
                    .await
                    .unwrap();
                fixture
                    .ctx
                    .channel_settings
                    .set(
                        "discord",
                        "global",
                        "blocked_users",
                        if mode == "base_blocked" {
                            r#"["discord:C1"]"#
                        } else {
                            "[]"
                        },
                    )
                    .await
                    .unwrap();
                provider
                    .enqueue_response("/channels/C1", json!({"id":"C1","guild_id":"G1","type":0}));
                invoke(&fixture, &provider, content).await;
                assert_eq!(
                    *records.lock().unwrap(),
                    if mode == "base_allow" && content != "/new" {
                        vec!["discord:C1".to_owned()]
                    } else {
                        vec![]
                    }
                );
                assert_eq!(
                    archived(&fixture, "discord:C1"),
                    mode == "base_allow" && content == "/new"
                );
                assert!(!archived(&fixture, "discord:thread:C1"));
                assert_eq!(
                    fixture
                        .ctx
                        .session_manager
                        .get_messages("discord:thread:C1")
                        .await
                        .unwrap()[0]
                        .content,
                    "thread-history"
                );
                assert!(
                    !provider
                        .requests()
                        .iter()
                        .any(|r| r.path.ends_with("/threads"))
                );
                ordinary_dispatch_probe::remove(&provider.token);
            }
        }
    }

    #[tokio::test]
    async fn trusted_dm_absence_uses_base_session_without_guild_lookup() {
        let provider = crate::test_channel_provider::TestChannelProvider::start().await;
        let fixture = NativeCuFixture::new(&provider.token).await;
        let records = ordinary_dispatch_probe::install(&provider.token);
        let mut data = message("ordinary conversation");
        data.as_object_mut().unwrap().remove("guild_id");
        handle_message_create(
            &data,
            "BOT",
            &reqwest::Client::new(),
            &provider.token,
            &fixture.ctx,
            None,
            "A1",
        )
        .await;
        assert_eq!(*records.lock().unwrap(), vec!["discord:C1"]);
        assert!(provider.requests().is_empty());
    }
}

#[cfg(test)]
mod normal_pairing_acl_tests {
    use super::*;
    use crate::decision_notify::native_loop_fixture::*;
    #[tokio::test]
    async fn actual_message_create_preserves_pairing_verdicts_and_strict_bound_isolation() {
        for scope in ["dm", "guild_base", "guild_thread"] {
            for mode in [
                "human",
                "session",
                "invalid",
                "expired",
                "human_blocked",
                "session_blocked",
                "db",
                "corrupt",
                "hint",
                "new_hint",
                "mention",
                "mention_ignored",
                "channel_blocked",
                "guild_blocked",
                "lookup",
            ] {
                if scope == "dm"
                    && matches!(
                        mode,
                        "mention_ignored" | "channel_blocked" | "guild_blocked" | "lookup"
                    )
                {
                    continue;
                }
                let provider = crate::test_channel_provider::TestChannelProvider::start().await;
                let fixture = NativeCuFixture::new(&provider.token).await;
                let session = if scope == "guild_thread" {
                    "discord:thread:C1"
                } else {
                    "discord:C1"
                };
                fixture
                    .ctx
                    .session_manager
                    .get_or_create(session, "alice")
                    .await
                    .unwrap();
                fixture
                    .ctx
                    .session_manager
                    .append_message(session, "user", "history", 1)
                    .await
                    .unwrap();
                let records = ordinary_dispatch_probe::install(&provider.token);
                fixture
                    .ctx
                    .channel_settings
                    .set("discord", "global", "require_pairing", "true")
                    .await
                    .unwrap();
                fixture
                    .ctx
                    .channel_settings
                    .set("discord", "G1", "auto_thread", "false")
                    .await
                    .unwrap();
                let subject = if mode == "session" || mode == "session_blocked" {
                    session
                } else {
                    "H1"
                };
                let code = fixture
                    .ctx
                    .access_control
                    .generate_pairing_code(subject)
                    .await
                    .unwrap();
                if mode == "expired" {
                    let path = fixture.home.path().join("access_control.json");
                    let mut state: Value =
                        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                    state["pending"][subject]["expires_at"] = json!("2000-01-01T00:00:00Z");
                    std::fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
                }
                if mode == "human_blocked" || mode == "session_blocked" {
                    fixture
                        .ctx
                        .channel_settings
                        .set(
                            "discord",
                            "global",
                            "blocked_users",
                            &serde_json::to_string(&[subject]).unwrap(),
                        )
                        .await
                        .unwrap();
                }
                if mode == "channel_blocked" {
                    fixture
                        .ctx
                        .channel_settings
                        .set("discord", "G1", "allowed_channels", r#"["OTHER"]"#)
                        .await
                        .unwrap();
                }
                if mode == "guild_blocked" {
                    fixture
                        .ctx
                        .channel_settings
                        .set("discord", "global", "allowed_guilds", r#"["OTHER"]"#)
                        .await
                        .unwrap();
                }
                if mode == "corrupt" {
                    fixture
                        .ctx
                        .channel_settings
                        .set("discord", "global", "require_pairing", "invalid")
                        .await
                        .unwrap();
                }
                if mode == "db" {
                    rusqlite::Connection::open(fixture.home.path().join("sessions.db"))
                        .unwrap()
                        .execute("DROP TABLE channel_settings", [])
                        .unwrap();
                }
                if mode == "mention_ignored" {
                    fixture
                        .ctx
                        .channel_settings
                        .set("discord", "G1", "mention_only", "true")
                        .await
                        .unwrap();
                }
                let context = crate::approval::DecisionContext {
                    channel: "discord".into(),
                    account_id: "A1".into(),
                    conversation_id: "C1".into(),
                    principal_id: "H1".into(),
                };
                let id = pending_native_request(&fixture, context).await;
                let content = match mode {
                    "invalid" => "/pair invalid".into(),
                    "hint" => "ordinary conversation".into(),
                    "new_hint" => "/new".into(),
                    "mention" => format!("<@BOT> /pair {code}"),
                    _ => format!("/pair {code}"),
                };
                let mut data = json!({
                    "id": "pairing",
                    "channel_id": "C1",
                    "author": {"id":"H1","bot":false},
                    "content": content
                });
                if scope != "dm" {
                    data["guild_id"] = json!("G1");
                    provider.enqueue_response("/channels/C1",json!({
                        "id": "C1",
                        "guild_id": "G1",
                        "type": if scope=="guild_thread" {10} else {0}
                    }));
                }
                if mode == "mention" {
                    data["mentions"] = json!([{"id":"BOT"}]);
                }
                if mode == "lookup" {
                    provider.refuse();
                }
                handle_message_create(
                    &data,
                    "BOT",
                    &reqwest::Client::new(),
                    &provider.token,
                    &fixture.ctx,
                    None,
                    "A1",
                )
                .await;
                let approved = fixture.ctx.access_control.runtime_approved_users().await;
                let success = matches!(mode, "human" | "session" | "mention");
                assert_eq!(
                    approved.iter().any(|id| id == subject),
                    success,
                    "scope={scope} mode={mode}"
                );
                assert!(
                    records.lock().unwrap().is_empty(),
                    "pairing must not dispatch a model"
                );
                assert_native_pending(&fixture, &id).await;
                let requests = provider.requests();
                let replies: Vec<_> = requests
                    .iter()
                    .filter_map(|r| r.body["content"].as_str())
                    .collect();
                if success {
                    assert!(replies.iter().any(|text| text.contains("配對成功")));
                } else if matches!(mode, "invalid" | "expired") {
                    assert!(
                        replies
                            .iter()
                            .any(|text| text.contains("配對碼錯誤或已過期"))
                    );
                } else if matches!(mode, "hint" | "new_hint") {
                    assert!(replies.iter().any(|text| text.contains("尚未配對")));
                } else {
                    assert!(
                        replies.is_empty(),
                        "blocked/unknown authority must not respond"
                    );
                }
                assert_eq!(
                    fixture
                        .ctx
                        .session_manager
                        .get_messages(session)
                        .await
                        .unwrap()
                        .len(),
                    1
                );
                let db =
                    rusqlite::Connection::open(fixture.home.path().join("sessions.db")).unwrap();
                let archived: bool = db
                    .query_row(
                        "SELECT archived_at IS NOT NULL FROM sessions WHERE id=?1",
                        [session],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(!archived, "unpaired /new must not archive history");
                assert!(!requests.iter().any(|r| r.path.ends_with("/threads")));
                if matches!(mode, "human" | "session") {
                    // Pairing is persisted and then normal and strict entry
                    // points may proceed under their respective gate contracts.
                    let reopened = crate::access_control::AccessController::with_persistence(
                        fixture.home.path().join("access_control.json"),
                    );
                    assert!(
                        reopened
                            .runtime_approved_users()
                            .await
                            .iter()
                            .any(|id| id == subject)
                    );
                    data["content"] = json!("ordinary after pairing");
                    if scope != "dm" {
                        provider.enqueue_response("/channels/C1", json!({
                            "id": "C1",
                            "guild_id": "G1",
                            "type": if scope=="guild_thread" {10} else {0}
                        }));
                    }
                    handle_message_create(
                        &data,
                        "BOT",
                        &reqwest::Client::new(),
                        &provider.token,
                        &fixture.ctx,
                        None,
                        "A1",
                    )
                    .await;
                    assert_eq!(*records.lock().unwrap(), vec![session.to_owned()]);
                    data["content"] = json!(format!("approve {}", id.as_str()));
                    if scope != "dm" {
                        provider.enqueue_response("/channels/C1", json!({
                            "id": "C1",
                            "guild_id": "G1",
                            "type": if scope=="guild_thread" {10} else {0}
                        }));
                    }
                    handle_message_create(
                        &data,
                        "BOT",
                        &reqwest::Client::new(),
                        &provider.token,
                        &fixture.ctx,
                        None,
                        "A1",
                    )
                    .await;
                    assert_native_approved(&fixture, &id).await;
                }
                ordinary_dispatch_probe::remove(&provider.token);
            }
        }
    }
}

#[cfg(test)]
#[path = "channel_decision_route/adapter_tests/discord.rs"]
mod f4_decision_route_tests;
