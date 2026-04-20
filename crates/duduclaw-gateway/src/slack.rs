//! Slack Bot integration via Socket Mode (WebSocket).
//!
//! Socket Mode avoids needing a public URL — ideal for local deployment.
//! Connects to Slack's WebSocket gateway and receives events in real-time.

use std::path::Path;
use std::sync::Arc;

use duduclaw_core::truncate_bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::channel_format;
use crate::channel_reply::{ReplyContext, build_reply_for_agent, build_reply_with_session, set_channel_connected};
use crate::channel_settings::keys;

const SLACK_API: &str = "https://slack.com/api";

// ── Slack API types ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SlackApiResponse {
    ok: bool,
    url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackEnvelope {
    #[serde(rename = "type")]
    envelope_type: String,
    envelope_id: Option<String>,
    payload: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct SlackAck {
    envelope_id: String,
}

#[derive(Debug, Serialize)]
struct PostMessage {
    channel: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<String>,
}

// ── Public API ──────────────────────────────────────────────────

/// Start the Slack bot via Socket Mode as a background task.
///
/// Kept for backward compatibility — delegates to `start_slack_bots()`.
pub async fn start_slack_bot(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut bots = start_slack_bots(home_dir, ctx).await;
    bots.pop().map(|(_, h)| h)
}

/// Start multiple Slack bots: one global (from config.toml) plus per-agent bots.
pub async fn start_slack_bots(
    home_dir: &Path,
    ctx: Arc<ReplyContext>,
) -> Vec<(String, tokio::task::JoinHandle<()>)> {
    let mut results = Vec::new();
    let mut seen_tokens: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Global bot from config.toml
    if let (Some(app_token), Some(bot_token)) = (
        read_slack_token(home_dir, "slack_app_token").await,
        read_slack_token(home_dir, "slack_bot_token").await,
    ) {
        if !app_token.is_empty() && !bot_token.is_empty() {
            seen_tokens.insert(bot_token.clone());
            if let Some(handle) = spawn_slack_bot(app_token, bot_token, "slack".into(), None, ctx.clone()).await {
                results.push(("slack".to_string(), handle));
            }
        }
    }

    // 2. Per-agent bots from agent configs
    let agent_tokens: Vec<(String, String, String)> = {
        let reg = ctx.registry.read().await;
        let mut tokens = Vec::new();
        for agent in reg.list() {
            if let Some(channels) = &agent.config.channels {
                if let Some(slack) = &channels.slack {
                    let app = crate::config_crypto::resolve_agent_token(&slack.app_token_enc, &slack.app_token, home_dir);
                    let bot = crate::config_crypto::resolve_agent_token(&slack.bot_token_enc, &slack.bot_token, home_dir);
                    if !app.is_empty() && !bot.is_empty() {
                        tokens.push((agent.config.agent.name.clone(), app, bot));
                    }
                }
            }
        }
        tokens
    };

    for (agent_name, app_token, bot_token) in agent_tokens {
        if seen_tokens.contains(&bot_token) {
            info!("Slack bot for agent '{agent_name}' shares global token — skipping duplicate");
            continue;
        }
        seen_tokens.insert(bot_token.clone());
        let label = format!("slack:{agent_name}");
        if let Some(handle) = spawn_slack_bot(app_token, bot_token, label.clone(), Some(agent_name), ctx.clone()).await {
            results.push((label, handle));
        }
    }

    results
}

async fn spawn_slack_bot(
    app_token: String,
    bot_token: String,
    label: String,
    agent_name: Option<String>,
    ctx: Arc<ReplyContext>,
) -> Option<tokio::task::JoinHandle<()>> {
    info!("Slack Socket Mode starting (label: {label})...");

    let handle = tokio::spawn(async move {
        loop {
            match run_socket_mode(&app_token, &bot_token, &ctx, &label, agent_name.as_deref()).await {
                Ok(()) => info!("Slack Socket Mode disconnected ({label})"),
                Err(e) => warn!("Slack Socket Mode error ({label}): {e}"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            info!("Slack Socket Mode reconnecting ({label})...");
        }
    });

    Some(handle)
}

// ── Socket Mode loop ────────────────────────────────────────────

async fn run_socket_mode(
    app_token: &str,
    bot_token: &str,
    ctx: &Arc<ReplyContext>,
    label: &str,
    agent_name: Option<&str>,
) -> Result<(), String> {
    // Use shared HTTP client (Fix CR-G9)
    let http = crate::shared_http_client().clone();

    // Get WebSocket URL via apps.connections.open
    let resp: SlackApiResponse = http
        .post(format!("{SLACK_API}/apps.connections.open"))
        .header("Authorization", format!("Bearer {app_token}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .map_err(|e| format!("apps.connections.open failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    if !resp.ok {
        return Err(format!("Slack API error: {}", resp.error.unwrap_or_default()));
    }

    let ws_url = resp.url.ok_or("No WebSocket URL returned")?;

    // Validate Slack WebSocket URL
    if let Ok(url) = url::Url::parse(&ws_url) {
        let host = url.host_str().unwrap_or("");
        if !host.ends_with(".slack.com") && !host.ends_with(".slack-msgs.com") {
            tracing::warn!(ws_url = %ws_url, "Suspicious Slack WebSocket URL, rejecting");
            return Err("Invalid Slack WebSocket URL domain".into());
        }
    }

    // Get bot user ID via auth.test for precise mention detection
    let bot_user_id = match http
        .post(format!("{SLACK_API}/auth.test"))
        .header("Authorization", format!("Bearer {bot_token}"))
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                data["user_id"].as_str().unwrap_or("").to_string()
            } else {
                String::new()
            }
        }
        Err(_) => String::new(),
    };
    if !bot_user_id.is_empty() {
        info!("Slack bot user ID: {bot_user_id}");
    }

    info!("Slack [{label}] Socket Mode connected");
    set_channel_connected(&ctx.channel_status, label, true, None, Some(&ctx.event_tx)).await;

    // Connect WebSocket
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| format!("WebSocket connect failed: {e}"))?;

    let (mut sink, mut stream) = ws_stream.split();

    while let Some(msg_result) = stream.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                warn!("Slack WS error: {e}");
                break;
            }
        };

        if let Message::Text(text) = msg {
            let envelope: SlackEnvelope = match serde_json::from_str(&text) {
                Ok(e) => e,
                Err(_) => continue,
            };

            // Always acknowledge the envelope first
            if let Some(ref eid) = envelope.envelope_id {
                let ack = serde_json::to_string(&SlackAck {
                    envelope_id: eid.clone(),
                })
                .unwrap_or_default();
                let _ = sink.send(Message::Text(ack.into())).await;
            }

            // Handle events_api type
            if envelope.envelope_type == "events_api" {
                if let Some(payload) = &envelope.payload {
                    handle_event(payload, bot_token, &bot_user_id, ctx, &http, agent_name).await;
                }
            }
        }
    }

    set_channel_connected(&ctx.channel_status, label, false, None, Some(&ctx.event_tx)).await;
    Ok(())
}

// ── Text helpers ───────────────────────────────────────────────

/// Remove Slack-style `<@USERID>` bot mentions from message text.
fn strip_bot_mention(text: &str) -> String {
    let mut result = text.to_string();
    while let Some(start) = result.find("<@") {
        if let Some(end) = result[start..].find('>') {
            result = format!("{}{}", &result[..start], &result[start + end + 1..]);
        } else {
            break;
        }
    }
    result.trim().to_string()
}

/// Convert standard markdown to Slack mrkdwn format.
/// Slack uses *bold*, _italic_, `code`, ```code block``` — mostly compatible.
fn to_slack_mrkdwn(text: &str) -> String {
    // Slack mrkdwn is mostly compatible with standard markdown
    // Main difference: **bold** → *bold*
    text.replace("**", "*")
}

// ── Event handling ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_event(
    payload: &serde_json::Value,
    bot_token: &str,
    bot_user_id: &str,
    ctx: &Arc<ReplyContext>,
    http: &reqwest::Client,
    agent_name: Option<&str>,
) {
    let event = match payload.get("event") {
        Some(e) => e,
        None => return,
    };

    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if event_type != "message" {
        return;
    }

    // Ignore bot messages (including our own)
    if event.get("bot_id").is_some() || event.get("subtype").is_some() {
        return;
    }

    let raw_text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let text = strip_bot_mention(raw_text);
    let text = text.as_str();
    if text.is_empty() {
        return;
    }

    let channel = event.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let user = event.get("user").and_then(|v| v.as_str()).unwrap_or("unknown");
    let thread_ts = event.get("thread_ts").and_then(|v| v.as_str()).map(|s| s.to_string());
    let ts = event.get("ts").and_then(|v| v.as_str()).unwrap_or("");
    let channel_type = event.get("channel_type").and_then(|v| v.as_str()).unwrap_or("channel");
    let is_dm = channel_type == "im";

    // ── Channel whitelist ──
    if !is_dm && !ctx.channel_settings.is_channel_allowed("slack", "global", channel).await {
        return;
    }

    // ── Mention-only filter ──
    // Per-agent bots default to mention-only to prevent all bots responding
    let default_mention_only = agent_name.is_some();
    let mention_only = ctx.channel_settings.get_bool("slack", "global", keys::MENTION_ONLY, default_mention_only).await;
    // Precise mention detection: check for <@BOT_USER_ID> rather than any <@
    let was_mentioned = if bot_user_id.is_empty() {
        raw_text.contains("<@") // Fallback if bot_user_id unknown
    } else {
        raw_text.contains(&format!("<@{bot_user_id}>"))
    };
    if !is_dm && mention_only && !was_mentioned {
        return;
    }

    info!("📩 Slack [{user}]: {}", truncate_bytes(&text, 80));

    // Add thinking emoji reaction
    let _ = http
        .post(format!("{SLACK_API}/reactions.add"))
        .header("Authorization", format!("Bearer {bot_token}"))
        .json(&json!({ "channel": channel, "name": "hourglass_flowing_sand", "timestamp": ts }))
        .send()
        .await;

    // Chat commands
    if crate::chat_commands::is_command(text) {
        if let Some(cmd) = crate::chat_commands::parse_command(text, None) {
            let session_id = format!("slack:{channel}");
            let agent_id = {
                let reg = ctx.registry.read().await;
                reg.main_agent()
                    .map(|a| a.config.agent.name.clone())
                    .unwrap_or_default()
            };
            let reply = crate::chat_commands::handle_command(&cmd, ctx, &session_id, &agent_id, true).await;
            send_message(http, bot_token, channel, &reply, thread_ts.as_deref().or(Some(ts))).await;
            remove_reaction_add_done(http, bot_token, channel, ts).await;
            return;
        }
    }

    // Build AI reply
    let session_id = if is_dm {
        format!("slack:{user}")
    } else {
        format!("slack:group:{channel}")
    };
    let reply = if let Some(agent) = agent_name {
        build_reply_for_agent(text, ctx, agent, &session_id, user, None).await
    } else {
        build_reply_with_session(text, ctx, &session_id, user, None).await
    };

    // Guard: don't send empty replies
    if reply.trim().is_empty() {
        warn!(channel, "Slack: reply is empty — skipping send");
        return;
    }

    // Mention the sender in group channels so they get notified
    let reply = if !is_dm {
        format!("<@{user}> {}", to_slack_mrkdwn(&reply))
    } else {
        to_slack_mrkdwn(&reply)
    };

    // Split long messages (Slack limit: 4000 chars)
    let reply_thread = thread_ts.as_deref().or(Some(ts));
    if reply.len() > 3900 {
        for chunk in split_message(&reply, 3900) {
            send_message(http, bot_token, channel, chunk, reply_thread).await;
        }
    } else {
        send_message(http, bot_token, channel, &reply, reply_thread).await;
    }

    remove_reaction_add_done(http, bot_token, channel, ts).await;
}

async fn send_message(
    http: &reqwest::Client,
    token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) {
    let body = PostMessage {
        channel: channel.to_string(),
        text: text.to_string(),
        thread_ts: thread_ts.map(|s| s.to_string()),
    };

    match http
        .post(format!("{SLACK_API}/chat.postMessage"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(data) = resp.json::<SlackApiResponse>().await {
                if !data.ok {
                    error!("Slack send failed: {}", data.error.unwrap_or_default());
                }
            }
        }
        Err(e) => error!("Slack send error: {e}"),
    }
}

async fn remove_reaction_add_done(http: &reqwest::Client, token: &str, channel: &str, ts: &str) {
    let _ = http
        .post(format!("{SLACK_API}/reactions.remove"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({ "channel": channel, "name": "hourglass_flowing_sand", "timestamp": ts }))
        .send()
        .await;
    let _ = http
        .post(format!("{SLACK_API}/reactions.add"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({ "channel": channel, "name": "white_check_mark", "timestamp": ts }))
        .send()
        .await;
}

/// Split a message into chunks of max_len characters, respecting line boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        let chunk_end = if end < text.len() {
            // Find last newline within range
            text[start..end].rfind('\n').map(|i| start + i + 1).unwrap_or(end)
        } else {
            end
        };
        chunks.push(&text[start..chunk_end]);
        start = chunk_end;
    }
    chunks
}

// ── Config ──────────────────────────────────────────────────────

async fn read_slack_token(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_bot_mention() {
        assert_eq!(strip_bot_mention("<@U12345> hello"), "hello");
        assert_eq!(strip_bot_mention("hi <@UABC> there"), "hi  there");
        assert_eq!(strip_bot_mention("no mention"), "no mention");
        assert_eq!(strip_bot_mention("<@U1> <@U2> hi"), "hi");
    }

    #[test]
    fn test_to_slack_mrkdwn() {
        assert_eq!(to_slack_mrkdwn("**bold**"), "*bold*");
        assert_eq!(to_slack_mrkdwn("normal text"), "normal text");
    }

    #[test]
    fn test_dm_vs_channel_detection() {
        // DM: channel_type = "im" → session uses user ID
        let is_dm = "im" == "im";
        assert!(is_dm);
        let session_id = if is_dm {
            format!("slack:{}", "U123")
        } else {
            format!("slack:group:{}", "C456")
        };
        assert_eq!(session_id, "slack:U123");

        // Channel: channel_type = "channel" → session uses channel ID
        let is_dm2 = "channel" == "im";
        assert!(!is_dm2);
        let session_id2 = if is_dm2 {
            format!("slack:{}", "U123")
        } else {
            format!("slack:group:{}", "C456")
        };
        assert_eq!(session_id2, "slack:group:C456");
    }

    #[test]
    fn test_split_message_short() {
        let chunks = split_message("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_message_long() {
        let text = "line1\nline2\nline3\nline4\nline5";
        let chunks = split_message(text, 12);
        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].ends_with('\n'));
    }

    #[test]
    fn test_parse_slack_envelope() {
        let json = r#"{"type":"events_api","envelope_id":"abc123","payload":{"event":{"type":"message","text":"hello","channel":"C123","user":"U456"}}}"#;
        let env: SlackEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.envelope_type, "events_api");
        assert_eq!(env.envelope_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_thread_reply_uses_ts() {
        // Thread replies should use the original message ts as thread_ts
        let ts = "1234567890.123456";
        let thread_ts = Some(ts);
        assert_eq!(thread_ts, Some("1234567890.123456"));
    }
}
