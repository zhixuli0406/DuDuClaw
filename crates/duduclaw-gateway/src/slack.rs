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

    // Collect per-agent tokens FIRST so the global Socket Mode connection can
    // defer to them. A Slack bot token bound to a specific agent is more
    // specific than the generic global connection, which routes via
    // `default_agent` and can answer as the wrong agent ("identity mixing").
    // When the same token is configured both globally and per-agent we keep
    // only the agent-bound connection.
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
    // 1. Global bot from config.toml — skipped when an agent already owns the
    //    same bot token (the per-agent connection below is authoritative).
    if let (Some(app_token), Some(bot_token)) = (
        read_slack_token(home_dir, "slack_app_token").await,
        read_slack_token(home_dir, "slack_bot_token").await,
    ) {
        if !app_token.is_empty() && !bot_token.is_empty() {
            if let Some(owner) = crate::channel_reply::find_global_token_owner(
                &bot_token,
                agent_tokens.iter().map(|(n, _, bot)| (n.as_str(), bot.as_str())),
            ) {
                warn!(
                    "Slack global bot token is also bound to agent '{owner}' — \
                     skipping the global connection to avoid identity mixing; \
                     the per-agent bot is authoritative"
                );
            } else {
                seen_tokens.insert(bot_token.clone());
                if let Some(handle) = spawn_slack_bot(app_token, bot_token, "slack".into(), None, ctx.clone()).await {
                    results.push(("slack".to_string(), handle));
                }
            }
        }
    }

    // 2. Per-agent bots (dedup among agents themselves — first claim wins).
    for (agent_name, app_token, bot_token) in agent_tokens {
        if seen_tokens.contains(&bot_token) {
            info!("Slack bot for agent '{agent_name}' shares an already-claimed token — skipping duplicate");
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

    // Stall watchdog: Slack Socket Mode normally produces a ping/disconnect
    // every ~30s. If the stream goes silent for >120s the TCP is half-closed
    // and we'll never see a Close frame — break out so the outer reconnect
    // loop can re-issue `apps.connections.open` and grab a fresh URL.
    const STALL_TIMEOUT_SECS: u64 = 120;

    loop {
        let next = tokio::time::timeout(
            std::time::Duration::from_secs(STALL_TIMEOUT_SECS),
            stream.next(),
        )
        .await;

        let msg_result = match next {
            Ok(Some(r)) => r,
            Ok(None) => break, // stream closed cleanly
            Err(_) => {
                warn!(
                    "Slack [{label}] Socket Mode stalled ({STALL_TIMEOUT_SECS}s no traffic), reconnecting"
                );
                break;
            }
        };

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

            // Handle envelope types. slash_commands / interactive are spawned
            // detached: an AI reply can take minutes and the response_url stays
            // valid for 30 min, while blocking here would delay acks for
            // subsequent envelopes (Slack then re-delivers them).
            match envelope.envelope_type.as_str() {
                "events_api" => {
                    if let Some(payload) = &envelope.payload {
                        handle_event(payload, bot_token, &bot_user_id, ctx, &http, agent_name).await;
                    }
                }
                "slash_commands" => {
                    if let Some(payload) = envelope.payload {
                        let ctx = ctx.clone();
                        let http = http.clone();
                        let agent = agent_name.map(str::to_string);
                        tokio::spawn(async move {
                            handle_slash_command_envelope(payload, &ctx, &http, agent.as_deref()).await;
                        });
                    }
                }
                "interactive" => {
                    if let Some(payload) = envelope.payload {
                        let ctx = ctx.clone();
                        let http = http.clone();
                        tokio::spawn(async move {
                            handle_interactive_envelope(payload, &ctx, &http).await;
                        });
                    }
                }
                _ => {}
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

    // AI-app "is thinking…" status (auto-clears when the app replies;
    // fails soft on workspaces without the assistant feature).
    let status_guard = crate::channel_typing::slack_status(
        http.clone(),
        bot_token.to_string(),
        channel.to_string(),
        thread_ts.clone().unwrap_or_else(|| ts.to_string()),
    );

    // Progress callback — post interim status into the thread, edit-in-place
    // via chat.update. TodoUpdate bypasses the 30s throttle.
    let progress_http = http.clone();
    let progress_token = bot_token.to_string();
    let progress_channel = channel.to_string();
    let progress_thread = thread_ts.clone().unwrap_or_else(|| ts.to_string());
    let progress_msg_ts: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let progress_msg_cleanup = progress_msg_ts.clone();
    let last_progress = Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap_or_else(std::time::Instant::now),
    ));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        // Step events are a dashboard-only agentic-tree signal — never rendered
        // as channel text (would be an empty message).
        if matches!(event, crate::channel_reply::ProgressEvent::Step { .. }) {
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
        let c = progress_http.clone();
        let t = progress_token.clone();
        let ch = progress_channel.clone();
        let th = progress_thread.clone();
        let msg_ts = progress_msg_ts.clone();
        tokio::spawn(async move {
            let mut guard = msg_ts.lock().await;
            match guard.as_deref() {
                Some(existing_ts) => {
                    let _ = c
                        .post(format!("{SLACK_API}/chat.update"))
                        .header("Authorization", format!("Bearer {t}"))
                        .json(&json!({ "channel": ch, "ts": existing_ts, "text": msg_text }))
                        .send()
                        .await;
                }
                None => {
                    *guard = post_message_returning_ts(&c, &t, &ch, &msg_text, Some(&th)).await;
                }
            }
        });
    });

    let reply = if let Some(agent) = agent_name {
        build_reply_for_agent(text, ctx, agent, &session_id, user, Some(on_progress)).await
    } else {
        build_reply_with_session(text, ctx, &session_id, user, Some(on_progress)).await
    };
    drop(status_guard);

    // Remove the interim progress message — the final reply supersedes it.
    if let Some(pts) = progress_msg_cleanup.lock().await.take() {
        let _ = http
            .post(format!("{SLACK_API}/chat.delete"))
            .header("Authorization", format!("Bearer {bot_token}"))
            .json(&json!({ "channel": channel, "ts": pts }))
            .send()
            .await;
    }

    // Guard: don't send empty replies
    if reply.trim().is_empty() {
        warn!(channel, "Slack: reply is empty — skipping send");
        return;
    }

    // Mention the sender in group channels so they get notified
    let mention = if !is_dm { Some(user) } else { None };

    // Split long messages (Slack limit: 4000 chars per section; the native
    // markdown block takes 12000)
    let reply_thread = thread_ts.as_deref().or(Some(ts));
    send_markdown_message(http, bot_token, channel, &reply, reply_thread, mention, Some(&session_id)).await;

    remove_reaction_add_done(http, bot_token, channel, ts).await;
}

/// Validate a Slack response_url before POSTing to it (external data —
/// never follow an arbitrary URL from a payload).
fn is_valid_slack_response_url(response_url: &str) -> bool {
    match url::Url::parse(response_url) {
        Ok(u) => {
            u.scheme() == "https"
                && u.host_str()
                    .map(|h| h == "hooks.slack.com" || h.ends_with(".slack.com"))
                    .unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// POST a response to a slash-command / interactive `response_url`.
/// `response_type`: "ephemeral" (only the invoker sees it) or "in_channel".
async fn respond_via_response_url(
    http: &reqwest::Client,
    response_url: &str,
    response_type: &str,
    text: &str,
) {
    if !is_valid_slack_response_url(response_url) {
        warn!("Slack: rejecting suspicious response_url");
        return;
    }
    let body = json!({
        "response_type": response_type,
        "replace_original": false,
        "text": text,
    });
    if let Err(e) = http.post(response_url).json(&body).send().await {
        error!("Slack response_url post error: {e}");
    }
}

/// Handle a `slash_commands` Socket-Mode envelope (native slash commands).
///
/// Note: Slack slash commands are declared in the app manifest (there is no
/// runtime registration API) — add `/ask` and `/duduclaw` to the app config
/// with Socket Mode enabled and this handler serves them.
/// Management subcommands respond ephemerally; AI queries post in-channel.
async fn handle_slash_command_envelope(
    payload: serde_json::Value,
    ctx: &Arc<ReplyContext>,
    http: &reqwest::Client,
    agent_name: Option<&str>,
) {
    let command = payload["command"].as_str().unwrap_or("");
    let text = payload["text"].as_str().unwrap_or("").trim().to_string();
    let channel_id = payload["channel_id"].as_str().unwrap_or("");
    let user_id = payload["user_id"].as_str().unwrap_or("unknown");
    let response_url = payload["response_url"].as_str().unwrap_or("");
    if response_url.is_empty() || channel_id.is_empty() {
        return;
    }

    info!("📩 Slack slash {command} from [{user_id}]: {}", truncate_bytes(&text, 80));

    // ── Channel whitelist applies to slash commands too ──
    if !ctx.channel_settings.is_channel_allowed("slack", "global", channel_id).await {
        respond_via_response_url(http, response_url, "ephemeral", "❌ 此頻道未被授權使用 DuDuClaw").await;
        return;
    }

    let session_id = format!("slack:group:{channel_id}");

    match command {
        "/ask" => {
            if text.is_empty() {
                respond_via_response_url(http, response_url, "ephemeral", "用法：/ask <你的問題>").await;
                return;
            }
            let reply = if let Some(agent) = agent_name {
                build_reply_for_agent(&text, ctx, agent, &session_id, user_id, None).await
            } else {
                build_reply_with_session(&text, ctx, &session_id, user_id, None).await
            };
            if reply.trim().is_empty() {
                respond_via_response_url(http, response_url, "ephemeral", "⚠️ 未取得回覆，請再試一次").await;
                return;
            }
            // Queries are visible to the channel (slash invocations are
            // otherwise only shown to the invoker).
            let visible = format!("*<@{user_id}>*: {text}\n\n{}", to_slack_mrkdwn(&reply));
            respond_via_response_url(http, response_url, "in_channel", &visible).await;
        }
        "/duduclaw" => {
            // Management subcommands (status/new/usage/help/...) route through
            // chat_commands and stay ephemeral.
            let cmd_text = if text.is_empty() { "/help".to_string() } else { format!("/{text}") };
            if let Some(cmd) = crate::chat_commands::parse_command(&cmd_text, None) {
                let agent_id = {
                    let reg = ctx.registry.read().await;
                    agent_name
                        .map(|s| s.to_string())
                        .or_else(|| reg.main_agent().map(|a| a.config.agent.name.clone()))
                        .unwrap_or_default()
                };
                let reply = crate::chat_commands::handle_command(&cmd, ctx, &session_id, &agent_id, true).await;
                respond_via_response_url(http, response_url, "ephemeral", &reply).await;
            } else {
                respond_via_response_url(
                    http,
                    response_url,
                    "ephemeral",
                    "未知的子指令。可用：status / new / usage / help（或用 /ask 提問）",
                ).await;
            }
        }
        _ => {
            respond_via_response_url(http, response_url, "ephemeral", &format!("未支援的指令：{command}")).await;
        }
    }
}

/// Handle an `interactive` Socket-Mode envelope (`block_actions` button presses).
/// `action_id` mirrors the Discord custom_id convention (`duduclaw:{action}`);
/// the session id travels in the button `value`.
async fn handle_interactive_envelope(
    payload: serde_json::Value,
    ctx: &Arc<ReplyContext>,
    http: &reqwest::Client,
) {
    if payload["type"].as_str() != Some("block_actions") {
        return;
    }
    let action = match payload["actions"].as_array().and_then(|a| a.first()) {
        Some(a) => a,
        None => return,
    };
    let action_id = action["action_id"].as_str().unwrap_or("");
    let value = action["value"].as_str().unwrap_or("");
    let response_url = payload["response_url"].as_str().unwrap_or("");
    if response_url.is_empty() {
        return;
    }

    match action_id {
        "duduclaw:new_session" => {
            let session_id = if value.is_empty() {
                let channel = payload["channel"]["id"].as_str().unwrap_or("");
                format!("slack:group:{channel}")
            } else {
                value.to_string()
            };
            let msg = match ctx.session_manager.delete_session(&session_id).await {
                Ok(()) => "✅ 已開啟新的對話".to_string(),
                Err(e) => format!("⚠️ 清除工作階段失敗：{e}"),
            };
            respond_via_response_url(http, response_url, "ephemeral", &msg).await;
        }
        other => {
            warn!("Slack: unknown block action: {other}");
        }
    }
}

/// chat.postMessage returning the created message `ts` (for later edits).
async fn post_message_returning_ts(
    http: &reqwest::Client,
    token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Option<String> {
    let mut body = json!({ "channel": channel, "text": text });
    if let Some(th) = thread_ts {
        body["thread_ts"] = json!(th);
    }
    let resp = http
        .post(format!("{SLACK_API}/chat.postMessage"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .ok()?;
    let data: serde_json::Value = resp.json().await.ok()?;
    if data.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    data.get("ts").and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Send an AI reply using Slack's native `markdown` block (standard
/// markdown incl. tables, headers, fenced code — released 2025-02). Falls
/// back to classic mrkdwn text when the workspace rejects the block.
///
/// When `session_id` is given, the LAST chunk carries conversation-control
/// action buttons (handled by the `interactive` Socket-Mode envelope).
#[allow(clippy::too_many_arguments)]
async fn send_markdown_message(
    http: &reqwest::Client,
    token: &str,
    channel: &str,
    markdown: &str,
    thread_ts: Option<&str>,
    mention_user: Option<&str>,
    session_id: Option<&str>,
) {
    // Cumulative cap across markdown blocks is 12000 chars — chunk into
    // separate messages under that.
    const MARKDOWN_BLOCK_CAP: usize = 11500;
    let chunks = channel_format::split_text(markdown, MARKDOWN_BLOCK_CAP);
    let last_idx = chunks.len().saturating_sub(1);

    for (i, chunk) in chunks.iter().enumerate() {
        let mut blocks = vec![];
        if i == 0 {
            if let Some(uid) = mention_user {
                blocks.push(json!({
                    "type": "section",
                    "text": { "type": "mrkdwn", "text": format!("<@{uid}>") }
                }));
            }
        }
        blocks.push(json!({ "type": "markdown", "text": chunk }));
        if i == last_idx {
            if let Some(sid) = session_id {
                blocks.push(channel_format::slack_action_buttons(sid));
            }
        }

        // Fallback text keeps notifications readable if blocks fail to render.
        let fallback = channel_format::truncate_chars(&to_slack_mrkdwn(chunk), 3000);
        let mut body = json!({ "channel": channel, "blocks": blocks, "text": fallback });
        if let Some(th) = thread_ts {
            body["thread_ts"] = json!(th);
        }

        let ok = match http
            .post(format!("{SLACK_API}/chat.postMessage"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|d| d.get("ok").and_then(|v| v.as_bool()))
                .unwrap_or(false),
            Err(e) => {
                error!("Slack send error: {e}");
                false
            }
        };

        if !ok {
            // Workspace/API rejected the markdown block — degrade to the
            // classic mrkdwn text path so nothing is dropped.
            warn!("Slack: markdown block rejected — falling back to mrkdwn text");
            let plain = if i == 0 && mention_user.is_some() {
                format!("<@{}> {}", mention_user.unwrap(), to_slack_mrkdwn(chunk))
            } else {
                to_slack_mrkdwn(chunk)
            };
            for piece in split_message(&plain, 3900) {
                send_message(http, token, channel, piece, thread_ts).await;
            }
        }
    }
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

/// Split a message into chunks of at most `max_len` bytes, respecting line
/// boundaries. Byte offsets are snapped to UTF-8 char boundaries.
///
/// L9: a long CJK run with no newline would previously slice `text[start..end]`
/// mid-character and panic. `truncate_bytes` walks back to the nearest char
/// boundary, so the split is always safe.
fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let remaining = &text[start..];
        // Char-boundary-safe end within the byte budget.
        let safe = truncate_bytes(remaining, max_len);
        let safe_len = safe.len();
        let reached_end = start + safe_len >= text.len();

        let chunk_end = if !reached_end {
            // Prefer to break at the last newline within the safe window.
            match safe.rfind('\n') {
                Some(i) => start + i + 1,
                None => start + safe_len,
            }
        } else {
            text.len()
        };

        // Guard forward progress: a single char wider than max_len, or a
        // pathological input, must still advance by at least one char.
        let chunk_end = if chunk_end <= start {
            match remaining.char_indices().nth(1) {
                Some((i, _)) => start + i,
                None => text.len(),
            }
        } else {
            chunk_end
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
    fn test_split_message_cjk_no_newline_no_panic() {
        // L9: a long CJK run with no newline must not panic on a char boundary.
        // Each CJK char is 3 bytes; max_len=10 lands mid-char repeatedly.
        let text = "你好世界這是一段很長的中文訊息沒有換行符號".repeat(20);
        let chunks = split_message(&text, 10);
        // Reassembling the chunks must reproduce the original exactly (no loss).
        let joined: String = chunks.concat();
        assert_eq!(joined, text);
        // Every chunk is valid UTF-8 (slicing succeeded) and within budget-ish.
        for c in &chunks {
            assert!(!c.is_empty());
        }
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

    #[test]
    fn test_response_url_validation() {
        assert!(is_valid_slack_response_url("https://hooks.slack.com/actions/T123/456/abc"));
        // Unanchored-substring attack must fail (coding convention #2).
        assert!(!is_valid_slack_response_url("https://hooks.slack.com.evil.com/x"));
        assert!(!is_valid_slack_response_url("http://hooks.slack.com/actions/x")); // not https
        assert!(!is_valid_slack_response_url("not a url"));
    }

    #[test]
    fn test_parse_slash_command_envelope() {
        let json = r#"{"type":"slash_commands","envelope_id":"e1","payload":{"command":"/ask","text":"hello","channel_id":"C1","user_id":"U1","response_url":"https://hooks.slack.com/commands/x"}}"#;
        let env: SlackEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.envelope_type, "slash_commands");
        let p = env.payload.unwrap();
        assert_eq!(p["command"], "/ask");
        assert_eq!(p["text"], "hello");
    }

    #[test]
    fn test_parse_interactive_envelope() {
        let json = r#"{"type":"interactive","envelope_id":"e2","payload":{"type":"block_actions","actions":[{"action_id":"duduclaw:new_session","value":"slack:group:C1"}],"response_url":"https://hooks.slack.com/actions/x"}}"#;
        let env: SlackEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.envelope_type, "interactive");
        let p = env.payload.unwrap();
        assert_eq!(p["actions"][0]["action_id"], "duduclaw:new_session");
    }
}
