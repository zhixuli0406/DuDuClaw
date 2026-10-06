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
use crate::channel_reply::{
    ReplyContext, build_guarded_reply_for_agent, build_guarded_reply_with_session,
    set_channel_connected,
};
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

    // Loaded once for the whole bot-start pass (WP-6C) — every per-agent
    // resolve below shares it rather than re-reading config.toml per agent.
    let sm_cfg =
        duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(home_dir).await;

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
                    // WP-H1: Slack needs BOTH tokens; `None` from either is
                    // "not configured" — the resolver has no empty-string state.
                    let app = crate::config_crypto::resolve_agent_token(
                        &slack.app_token_enc,
                        &slack.app_token,
                        home_dir,
                        &sm_cfg,
                    )
                    .await;
                    let bot = crate::config_crypto::resolve_agent_token(
                        &slack.bot_token_enc,
                        &slack.bot_token,
                        home_dir,
                        &sm_cfg,
                    )
                    .await;
                    if let (Some(app), Some(bot)) = (app, bot) {
                        tokens.push((
                            agent.config.agent.name.clone(),
                            app.expose_owned(),
                            bot.expose_owned(),
                        ));
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
                agent_tokens
                    .iter()
                    .map(|(n, _, bot)| (n.as_str(), bot.as_str())),
            ) {
                warn!(
                    "Slack global bot token is also bound to agent '{owner}' — \
                     skipping the global connection to avoid identity mixing; \
                     the per-agent bot is authoritative"
                );
            } else {
                seen_tokens.insert(bot_token.clone());
                if let Some(handle) =
                    spawn_slack_bot(app_token, bot_token, "slack".into(), None, ctx.clone()).await
                {
                    results.push(("slack".to_string(), handle));
                }
            }
        }
    }

    // 2. Per-agent bots (dedup among agents themselves — first claim wins).
    for (agent_name, app_token, bot_token) in agent_tokens {
        if seen_tokens.contains(&bot_token) {
            info!(
                "Slack bot for agent '{agent_name}' shares an already-claimed token — skipping duplicate"
            );
            continue;
        }
        seen_tokens.insert(bot_token.clone());
        let label = format!("slack:{agent_name}");
        if let Some(handle) = spawn_slack_bot(
            app_token,
            bot_token,
            label.clone(),
            Some(agent_name),
            ctx.clone(),
        )
        .await
        {
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
            match run_socket_mode(&app_token, &bot_token, &ctx, &label, agent_name.as_deref()).await
            {
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
        .post(slack_inbound_url(app_token, "apps.connections.open"))
        .header("Authorization", format!("Bearer {app_token}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .map_err(|e| format!("apps.connections.open failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    if !resp.ok {
        return Err(format!(
            "Slack API error: {}",
            resp.error.unwrap_or_default()
        ));
    }

    let ws_url = resp.url.ok_or("No WebSocket URL returned")?;

    // Validate Slack WebSocket URL
    if let Ok(url) = url::Url::parse(&ws_url) {
        let host = url.host_str().unwrap_or("");
        let allowed = host.ends_with(".slack.com") || host.ends_with(".slack-msgs.com");
        #[cfg(test)]
        let allowed = allowed
            || crate::test_channel_provider::url(
                app_token,
                "https://slack.com/api/apps.connections.open",
            )
            .starts_with("http://127.0.0.1:")
                && host == "127.0.0.1";
        if !allowed {
            tracing::warn!(ws_url = %ws_url, "Suspicious Slack WebSocket URL, rejecting");
            return Err("Invalid Slack WebSocket URL domain".into());
        }
    }

    let decision_account = match verified_slack_decision_account(&http, bot_token, label).await {
        Ok(account) => {
            crate::channel_decision_route::note_slack_identity_ok(&ctx.home_dir, label);
            Some(account)
        }
        Err(failure) => {
            crate::channel_decision_route::note_slack_identity_failure(ctx, label, failure).await;
            None
        }
    };

    // Get bot user ID via auth.test for precise mention detection
    let bot_user_id = match http
        .post(slack_inbound_url(bot_token, "auth.test"))
        .header("Authorization", format!("Bearer {bot_token}"))
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                // W2-7: `auth.test`'s `url` field is `https://<workspace>.
                // slack.com/` — the coordinate `channel_link.rs`'s "在 Slack
                // 中開啟" permalink needs and that nothing else in the
                // gateway ever captures. Persisted on every (re)connect so
                // it's always fresh for whichever workspace this bot token
                // currently belongs to.
                if let Some(domain) =
                    slack_workspace_domain_from_url(data["url"].as_str().unwrap_or(""))
                {
                    crate::channel_link::record_slack_workspace_domain(&ctx.home_dir, &domain);
                }
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
    let executor = crate::decision_notify::BoundedChannelExecutor::new();
    let busy_acks = crate::decision_notify::BoundedChannelExecutor::new();

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

            // Ordinary work enters the bounded conversation scheduler. Strict
            // decisions stay on the receiver so a waiting action can finish;
            // the envelope ACK confirms transport receipt, not work admission.
            match envelope.envelope_type.as_str() {
                "events_api" => {
                    if let Some(payload) = envelope.payload {
                        if slack_decision_fastlane(
                            &payload,
                            bot_token,
                            ctx,
                            &http,
                            decision_account.as_ref(),
                        )
                        .await
                        {
                            continue;
                        }
                        let key = slack_execution_key(&payload, decision_account.as_ref());
                        let busy_payload = payload.clone();
                        let busy_http = http.clone();
                        let busy_token = bot_token.to_owned();
                        let rejection_events = ctx.event_tx.clone();

                        let ctx = ctx.clone();
                        let http = http.clone();
                        let token = bot_token.to_owned();
                        let bot_user = bot_user_id.clone();
                        let agent = agent_name.map(str::to_owned);
                        let account = decision_account.clone();
                        if executor
                            .try_submit(key, async move {
                                handle_event(
                                    &payload,
                                    &token,
                                    &bot_user,
                                    &ctx,
                                    &http,
                                    agent.as_deref(),
                                    account.as_ref(),
                                )
                                .await;
                            })
                            .is_err()
                        {
                            warn!(
                                "Slack normal work queue full; receiver remains available for decisions"
                            );
                            let _ = rejection_events.send(serde_json::json!({
                                "type": "channel_queue_rejected",
                                "channel": "slack",
                                "retryable": true
                            }).to_string());
                            let _ = busy_acks.try_submit(
                                "busy-ack".into(),
                                slack_busy_ack(busy_payload, busy_token, busy_http),
                            );
                        }
                    }
                }
                "slash_commands" => {
                    if let Some(payload) = envelope.payload {
                        let key = slack_execution_key(&payload, decision_account.as_ref());
                        let busy_payload = payload.clone();
                        let busy_http = http.clone();
                        let busy_token = bot_token.to_owned();
                        let rejection_events = ctx.event_tx.clone();

                        let ctx = ctx.clone();
                        let http = http.clone();
                        let agent = agent_name.map(str::to_owned);
                        if executor
                            .try_submit(key, async move {
                                handle_slash_command_envelope(
                                    payload,
                                    &ctx,
                                    &http,
                                    agent.as_deref(),
                                )
                                .await;
                            })
                            .is_err()
                        {
                            warn!("Slack slash work queue full");
                            let _ = rejection_events.send(serde_json::json!({
                                "type": "channel_queue_rejected",
                                "channel": "slack",
                                "retryable": true
                            }).to_string());
                            let _ = busy_acks.try_submit(
                                "busy-ack".into(),
                                slack_busy_ack(busy_payload, busy_token, busy_http),
                            );
                        }
                    }
                }
                "interactive" => {
                    if let Some(payload) = envelope.payload {
                        // Decision callbacks never queue behind an agent awaiting
                        // the very decision this envelope can provide.
                        handle_interactive_envelope(payload, ctx, &http, decision_account.as_ref())
                            .await;
                    }
                }
                _ => {}
            }
        }
    }

    set_channel_connected(&ctx.channel_status, label, false, None, Some(&ctx.event_tx)).await;
    Ok(())
}

// ── W2-7: workspace domain extraction ────────────────────────────
//
// Pure function so the `<workspace>.slack.com` parsing can be pinned with a
// plain unit test independent of the live `auth.test` call.

/// Extract the `<workspace>` subdomain from Slack `auth.test`'s `url` field
/// (`https://<workspace>.slack.com/`). `None` for a missing/malformed URL or
/// a host that isn't a `*.slack.com` domain — never guesses.
fn slack_workspace_domain_from_url(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_string();
    let domain = host.strip_suffix(".slack.com")?;
    (!domain.is_empty()).then(|| domain.to_string())
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

/// Quoted/shared-message context from a Slack message event. "Share message"
/// (and pasted archive links) deliver the quoted content as legacy
/// `attachments` entries with `text` + `author_name` — separate from `files`
/// and previously dropped, so the agent never saw what was being quoted.
/// Entries without a share/author signal are skipped (plain link unfurls).
fn slack_quoted_context(event: &serde_json::Value, bot_user_id: &str) -> Option<String> {
    let arr = event.get("attachments")?.as_array()?;
    let mut blocks: Vec<String> = Vec::new();
    for a in arr.iter() {
        let is_share = a.get("is_share").and_then(|v| v.as_bool()).unwrap_or(false);
        let author_id = a.get("author_id").and_then(|v| v.as_str());
        let author_name = a.get("author_name").and_then(|v| v.as_str());
        if !is_share && author_id.is_none() && author_name.is_none() {
            continue;
        }
        let quoted_text = a
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                a.get("fallback")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            });
        let Some(quoted_text) = quoted_text else {
            continue;
        };
        let who = if !bot_user_id.is_empty() && author_id == Some(bot_user_id) {
            channel_format::QUOTED_SELF_LABEL
        } else {
            author_name.unwrap_or("對方")
        };
        blocks.push(channel_format::format_quoted_context(who, quoted_text));
        if blocks.len() >= 3 {
            break;
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n"))
    }
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
    decision_account: Option<&SlackDecisionAccount>,
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

    if ctx
        .channel_settings
        .refresh_channel_snapshot("slack", "global")
        .await
        .is_err()
    {
        return;
    }
    let raw_text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let text = strip_bot_mention(raw_text);
    let text = text.as_str();
    // WP1.3: a message may carry only file attachments (no text) — still
    // process it. A shared (quoted) message with no added comment is also
    // non-empty. Genuinely empty messages are ignored.
    let has_files = event
        .get("files")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let has_shared_message = slack_quoted_context(event, bot_user_id).is_some();
    if text.is_empty() && !has_files && !has_shared_message {
        return;
    }

    let channel = event.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let user = event
        .get("user")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let thread_ts = event
        .get("thread_ts")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let ts = event.get("ts").and_then(|v| v.as_str()).unwrap_or("");
    let channel_type = event
        .get("channel_type")
        .and_then(|v| v.as_str())
        .unwrap_or("channel");
    let is_dm = channel_type == "im";

    // ── Channel whitelist ──
    if !is_dm
        && !ctx
            .channel_settings
            .is_channel_allowed("slack", "global", channel)
            .await
    {
        return;
    }

    // WP1.6 (ecosystem): a threaded reply under a decision card carrying a
    // bare verb (「同意」/「拒絕」…) is a button press — the thread parent IS
    // the card, so no @mention is required (this must run before the
    // mention-only filter). Same dispatch (auth + accounting) as a physical
    // press; non-verb replies fall through to normal chat.
    if let Some(parent_ts) = thread_ts.as_deref() {
        if !text.is_empty() {
            if let Some(outcome) = crate::decision_text::route_text_reply(
                &ctx.home_dir,
                "slack",
                user,
                channel,
                parent_ts,
                text,
            )
            .await
            {
                let ack = match outcome {
                    Ok(m) => m,
                    Err(e) => format!("⚠ {e}"),
                };
                // `slack_inbound_url` is `{SLACK_API}/chat.postMessage` in a
                // build; tests route it to the local provider (F4).
                let _ = http
                    .post(slack_inbound_url(bot_token, "chat.postMessage"))
                    .header("Authorization", format!("Bearer {bot_token}"))
                    .json(&json!({ "channel": channel, "thread_ts": parent_ts, "text": ack }))
                    .send()
                    .await;
                return;
            }
        }
    }

    // ── Mention-only filter ──
    // Per-agent bots default to mention-only to prevent all bots responding
    let default_mention_only = agent_name.is_some();
    let mention_only = ctx
        .channel_settings
        .get_bool("slack", "global", keys::MENTION_ONLY, default_mention_only)
        .await;
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
        .post(slack_inbound_url(bot_token, "reactions.add"))
        .header("Authorization", format!("Bearer {bot_token}"))
        .json(&json!({ "channel": channel, "name": "hourglass_flowing_sand", "timestamp": ts }))
        .send()
        .await;

    // Chat commands
    if crate::chat_commands::is_command(text) {
        if let Some(cmd) = crate::chat_commands::parse_command(text, None) {
            // MUST be the same session id the AI reply path below computes —
            // `slack:{channel}` targeted a ghost session for DMs (reply path
            // uses `slack:{user}`) and for groups (`slack:group:{channel}`).
            let session_id = if is_dm {
                format!("slack:{user}")
            } else {
                format!("slack:group:{channel}")
            };
            // Central access gate (pairing / allowlist / blocklist) — same
            // enforcement the AI path applies; commands must not bypass it.
            if let Some(gate_reply) =
                crate::channel_reply::check_user_access_gate(ctx, &session_id, user, text).await
            {
                if !gate_reply.is_empty() {
                    send_message(
                        http,
                        bot_token,
                        channel,
                        &gate_reply,
                        thread_ts.as_deref().or(Some(ts)),
                    )
                    .await;
                }
                remove_reaction_add_done(http, bot_token, channel, ts).await;
                return; // blocked users are silently ignored (empty reply)
            }
            let agent_id = {
                let reg = ctx.registry.read().await;
                reg.main_agent()
                    .map(|a| a.config.agent.name.clone())
                    .unwrap_or_default()
            };
            // Real per-channel admin status (fail-closed) — never hardcoded.
            let is_admin =
                crate::channel_reply::is_channel_admin(ctx, "slack", &[user, &session_id]).await;
            let reply = crate::chat_commands::handle_command(
                &cmd,
                ctx,
                &session_id,
                &agent_id,
                is_admin,
                user,
            )
            .await;
            send_message(
                http,
                bot_token,
                channel,
                &reply,
                thread_ts.as_deref().or(Some(ts)),
            )
            .await;
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
        // Step / ModelInfo events are dashboard-only signals — never rendered
        // as channel text (would be an empty message).
        if matches!(
            event,
            crate::channel_reply::ProgressEvent::Step { .. }
                | crate::channel_reply::ProgressEvent::ModelInfo { .. }
        ) {
            return;
        }
        let is_todo = matches!(
            event,
            crate::channel_reply::ProgressEvent::TodoUpdate { .. }
        );
        {
            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
            let throttle =
                crate::channel_capabilities::progress_throttle_secs("slack").unwrap_or(30);
            if !is_todo && last.elapsed().as_secs() < throttle {
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

    // WP1.3: download Slack file attachments (url_private + Bearer bot token)
    // to the resolved agent's dir, then append markdown refs to the prompt.
    let mut attachment_lines: Vec<String> = Vec::new();
    if let Some(files) = event.get("files").and_then(|v| v.as_array()) {
        let attach_base =
            crate::channel_reply::resolve_attachment_base(ctx.as_ref(), agent_name).await;
        for f in files {
            let url = f
                .get("url_private_download")
                .or_else(|| f.get("url_private"))
                .and_then(|v| v.as_str());
            let Some(url) = url else { continue };
            let mime = f
                .get("mimetype")
                .and_then(|v| v.as_str())
                .unwrap_or("application/octet-stream");
            let filename = f.get("name").and_then(|v| v.as_str()).unwrap_or("file");
            let mt = crate::media::media_type_from_mime(mime);
            match crate::media::download_url(
                http,
                url,
                Some(("Authorization", &format!("Bearer {bot_token}"))),
                crate::media::MAX_FILE_SIZE as usize,
            )
            .await
            {
                Ok(bytes) => {
                    match crate::media::save_attachment_in_base(&attach_base, &bytes, filename)
                        .await
                    {
                        Ok(path) => attachment_lines
                            .push(crate::media::format_attachment_ref(&mt, filename, &path)),
                        Err(e) => warn!("Slack: failed to save attachment {filename}: {e}"),
                    }
                }
                Err(e) => warn!("Slack: failed to download attachment {filename}: {e}"),
            }
        }
    }
    // ── Quoted/shared-message context ──
    let base_text = match slack_quoted_context(event, bot_user_id) {
        Some(quote_block) if text.is_empty() => quote_block,
        Some(quote_block) => format!("{quote_block}\n{text}"),
        None => text.to_string(),
    };
    let input_text = if attachment_lines.is_empty() {
        base_text
    } else if base_text.is_empty() {
        attachment_lines.join("\n")
    } else {
        format!("{base_text}\n\n{}", attachment_lines.join("\n"))
    };

    let decision_context = crate::approval::DecisionContext {
        channel: "slack".into(),
        account_id: decision_account
            .filter(|a| a.matches(payload))
            .map(|a| a.account_id.clone())
            .unwrap_or_default(),
        conversation_id: match thread_ts.as_deref() {
            Some(t) => format!("{channel}:{t}"),
            None => channel.to_string(),
        },
        principal_id: user.into(),
    };
    let target = crate::approval::TrustedReplyTarget::new(
        decision_context,
        bot_token.to_string(),
        channel.to_string(),
        thread_ts.clone(),
    )
    .map(|target| {
        target.with_decision_access_scope(crate::decision_notify::DecisionAccessScope {
            channel_id: (!is_dm).then_some(channel),
            guild_id: None,
            session_id: Some(&session_id),
        })
    });
    #[cfg(test)]
    if let Some(work) =
        crate::decision_notify::native_loop_fixture::take_job_for(bot_token, &input_text)
    {
        crate::approval::scope_trusted_reply(target, work).await;
        drop(status_guard);
        return;
    }
    let guarded = crate::approval::scope_trusted_reply(target, async {
        if let Some(agent) = agent_name {
            build_guarded_reply_for_agent(
                &input_text,
                ctx,
                agent,
                &session_id,
                user,
                Some(on_progress),
            )
            .await
        } else {
            build_guarded_reply_with_session(&input_text, ctx, &session_id, user, Some(on_progress))
                .await
        }
    })
    .await;
    drop(status_guard);

    // WP1.3: 📎DELIVER: outbound — upload generated files via Slack, strip marker.
    let reply = {
        let sender = crate::channel_sender::SlackSender {
            bot_token: bot_token.to_string(),
            channel_id: channel.to_string(),
            user_id: user.to_string(),
            http: http.clone(),
        };
        if !guarded.still_valid().await {
            return;
        }
        crate::channel_reply::deliver_documents_for_reply_guarded(
            ctx.as_ref(),
            agent_name,
            guarded.text.clone(),
            &sender,
            Some(&guarded),
        )
        .await
    };

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
    send_markdown_message(
        http,
        bot_token,
        channel,
        &reply,
        reply_thread,
        mention,
        Some(&session_id),
        Some(&guarded),
    )
    .await;

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
    #[cfg(test)]
    let response_url = crate::test_channel_provider::url(
        response_url.rsplit('/').next().unwrap_or(""),
        response_url,
    );
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

    info!(
        "📩 Slack slash {command} from [{user_id}]: {}",
        truncate_bytes(&text, 80)
    );

    // ── Channel whitelist applies to slash commands too ──
    if !ctx
        .channel_settings
        .is_channel_allowed("slack", "global", channel_id)
        .await
    {
        let product =
            crate::branding::effective_product_name(&duduclaw_core::platform::duduclaw_home());
        respond_via_response_url(
            http,
            response_url,
            "ephemeral",
            &format!("❌ 此頻道未被授權使用 {product}"),
        )
        .await;
        return;
    }

    let session_id = format!("slack:group:{channel_id}");

    match command {
        "/ask" => {
            if text.is_empty() {
                respond_via_response_url(http, response_url, "ephemeral", "用法：/ask <你的問題>")
                    .await;
                return;
            }
            let guarded = if let Some(agent) = agent_name {
                build_guarded_reply_for_agent(&text, ctx, agent, &session_id, user_id, None).await
            } else {
                build_guarded_reply_with_session(&text, ctx, &session_id, user_id, None).await
            };
            let reply = &guarded.text;
            if reply.trim().is_empty() {
                respond_via_response_url(
                    http,
                    response_url,
                    "ephemeral",
                    "⚠️ 未取得回覆，請再試一次",
                )
                .await;
                return;
            }
            // Queries are visible to the channel (slash invocations are
            // otherwise only shown to the invoker).
            let visible = format!("*<@{user_id}>*: {text}\n\n{}", to_slack_mrkdwn(&reply));
            if guarded.still_valid().await {
                respond_via_response_url(http, response_url, "in_channel", &visible).await;
            }
        }
        "/duduclaw" => {
            // Management subcommands (status/new/usage/help/...) route through
            // chat_commands and stay ephemeral.
            let cmd_text = if text.is_empty() {
                "/help".to_string()
            } else {
                format!("/{text}")
            };
            // W3-1: Slack swallows unregistered slash commands client-side, so
            // a bare `/takeover` never reaches us the way it does on Telegram
            // or Discord. `/duduclaw takeover …` is the working form here.
            // Handled before `parse_command` (which deliberately does not know
            // this command) and with the sender's account id, which the shared
            // `handle_command` signature does not carry.
            if let Some(tk) = crate::chat_commands::parse_takeover(&cmd_text) {
                if let Some(gate_reply) = crate::channel_reply::check_user_access_gate(
                    ctx,
                    &session_id,
                    user_id,
                    &cmd_text,
                )
                .await
                {
                    if !gate_reply.is_empty() {
                        respond_via_response_url(http, response_url, "ephemeral", &gate_reply)
                            .await;
                    }
                    return;
                }
                let reply =
                    crate::chat_commands::handle_takeover(ctx, &session_id, user_id, &tk).await;
                respond_via_response_url(http, response_url, "ephemeral", &reply).await;
                return;
            }
            if let Some(cmd) = crate::chat_commands::parse_command(&cmd_text, None) {
                // Central access gate — slash commands must not bypass the
                // pairing/allowlist/blocklist enforcement the AI path applies.
                if let Some(gate_reply) = crate::channel_reply::check_user_access_gate(
                    ctx,
                    &session_id,
                    user_id,
                    &cmd_text,
                )
                .await
                {
                    if !gate_reply.is_empty() {
                        respond_via_response_url(http, response_url, "ephemeral", &gate_reply)
                            .await;
                    }
                    return; // blocked users are silently ignored
                }
                let agent_id = {
                    let reg = ctx.registry.read().await;
                    agent_name
                        .map(|s| s.to_string())
                        .or_else(|| reg.main_agent().map(|a| a.config.agent.name.clone()))
                        .unwrap_or_default()
                };
                // Real per-channel admin status (fail-closed) — never hardcoded.
                let is_admin =
                    crate::channel_reply::is_channel_admin(ctx, "slack", &[user_id, &session_id])
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
                respond_via_response_url(http, response_url, "ephemeral", &reply).await;
            } else {
                respond_via_response_url(
                    http,
                    response_url,
                    "ephemeral",
                    "未知的子指令。可用：status / new / usage / help / takeover（或用 /ask 提問）",
                )
                .await;
            }
        }
        _ => {
            respond_via_response_url(
                http,
                response_url,
                "ephemeral",
                &format!("未支援的指令：{command}"),
            )
            .await;
        }
    }
}

/// Slack overflow menus (W1-5's secondary-actions tier — see
/// `channel_format::slack_goal_buttons`) report the chosen option in
/// `selected_option.value`; the element's own `action_id` stays fixed to the
/// menu itself and is never a decodable decision id. Every other interactive
/// element (a plain button) carries its payload directly in `action_id`.
/// Pulled out as a pure function so the branch is unit-testable without a
/// live Socket-Mode envelope.
fn slack_action_payload(action: &serde_json::Value) -> &str {
    if action["type"].as_str() == Some("overflow") {
        action["selected_option"]["value"].as_str().unwrap_or("")
    } else {
        action["action_id"].as_str().unwrap_or("")
    }
}

/// Handle an `interactive` Socket-Mode envelope (`block_actions` button presses).
/// `action_id` mirrors the Discord custom_id convention (`duduclaw:{action}`);
/// the session id travels in the button `value`.
async fn handle_interactive_envelope(
    payload: serde_json::Value,
    ctx: &Arc<ReplyContext>,
    http: &reqwest::Client,
    decision_account: Option<&SlackDecisionAccount>,
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

    // Decision buttons — every "a human must decide this" card, whichever
    // store backs it. `None` ⇒ not a decision button, fall through below.
    // `slack_action_payload` resolves an overflow menu's selection instead of
    // its fixed `action_id` — everything else is unaffected (payload ==
    // action_id for a plain button).
    let action_data = slack_action_payload(action);
    let slack_uid = payload["user"]["id"].as_str().unwrap_or("");
    if !slack_uid.is_empty() {
        if let Some(result) = crate::decision_notify::route_verified_bound_press(
            ctx,
            &crate::approval::DecisionContext {
                channel: "slack".into(),
                account_id: decision_account
                    .filter(|a| a.matches(&payload))
                    .map(|a| a.account_id.clone())
                    .unwrap_or_default(),
                conversation_id: {
                    let ch = payload["channel"]["id"].as_str().unwrap_or("");
                    match payload["message"]["thread_ts"].as_str() {
                        Some(t) => format!("{ch}:{t}"),
                        None => ch.to_string(),
                    }
                },
                principal_id: slack_uid.into(),
            },
            action_data,
            crate::decision_notify::DecisionAccessScope {
                channel_id: payload["channel"]["id"]
                    .as_str()
                    .filter(|id| !id.starts_with('D')),
                guild_id: None,
                session_id: Some(&if payload["channel"]["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with('D'))
                {
                    format!("slack:{slack_uid}")
                } else {
                    format!(
                        "slack:group:{}",
                        payload["channel"]["id"].as_str().unwrap_or("")
                    )
                }),
            },
        )
        .await
        {
            // Retiring the card (clearing its buttons) happens inside the
            // decide path via `chat.update` — a detached best-effort edit
            // independent of this `response_url`, which is single-use and
            // would conflict with it (see `decision_card::collapse_all`).
            // This only sends the light ephemeral ack; an unauthorized or
            // already-settled press leaves the message for whoever IS
            // allowed to act on it.
            match result {
                Ok(m) => {
                    respond_via_response_url(http, response_url, "ephemeral", &m).await;
                }
                Err(m) => {
                    respond_via_response_url(http, response_url, "ephemeral", &format!("⚠️ {m}"))
                        .await;
                }
            }
            return;
        }
    }

    // Goal-intent confirmation buttons (P1) — a separate, deliberately
    // UN-authorized codec from `decision_action` above (see
    // `channel_format`'s "Goal-intent confirmation" module doc): consuming
    // it needs no pressing-user identity, only the single-use nonce.
    // `handle_interactive_envelope` already runs detached (see the caller's
    // own comment: "an AI reply can take minutes… blocking here would delay
    // acks for subsequent envelopes"), so awaiting `handle_gintent_button`'s
    // possible plan-first LLM call directly is safe. Posted `in_channel` (not
    // ephemeral) so the result — a created goal task, or a generated plan —
    // is visible the same way a plain-text `1`/`2`/`3` reply would have been.
    if let Some((choice, nonce)) = crate::goal_intent::parse_gintent_action(action_data) {
        let outcome = crate::goal_intent::handle_gintent_button(ctx, choice, &nonce).await;
        respond_via_response_url(http, response_url, "in_channel", &outcome).await;
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
    data.get("ts")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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
    guarded: Option<&crate::channel_reply::GuardedReply>,
) {
    // Cumulative cap across markdown blocks is 12000 chars — chunk into
    // separate messages under that.
    const MARKDOWN_BLOCK_CAP: usize = 11500;
    let chunks = channel_format::split_text(markdown, MARKDOWN_BLOCK_CAP);
    let last_idx = chunks.len().saturating_sub(1);

    for (i, chunk) in chunks.iter().enumerate() {
        if crate::channel_reply::guard_lost(guarded).await {
            return;
        }
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
                // P1: the goal-intent confirmation buttons when `markdown`
                // (the full un-chunked reply) is the specific turn that just
                // appended the confirmation menu, otherwise the ordinary
                // conversation-control buttons (unchanged from before P1).
                let buttons = if crate::goal_intent::reply_has_confirmation_menu(markdown) {
                    crate::goal_intent::pending_button_nonce(sid)
                        .map(|nonce| channel_format::slack_gintent_buttons(&nonce))
                        .unwrap_or_else(|| channel_format::slack_action_buttons(sid))
                } else {
                    channel_format::slack_action_buttons(sid)
                };
                blocks.push(buttons);
            }
        }

        // Fallback text keeps notifications readable if blocks fail to render.
        let fallback = channel_format::truncate_chars(&to_slack_mrkdwn(chunk), 3000);
        let mut body = json!({ "channel": channel, "blocks": blocks, "text": fallback });
        if let Some(th) = thread_ts {
            body["thread_ts"] = json!(th);
        }

        if crate::channel_reply::guard_lost(guarded).await {
            return;
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
                if crate::channel_reply::guard_lost(guarded).await {
                    return;
                }
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
        .post(slack_inbound_url(token, "reactions.remove"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({ "channel": channel, "name": "hourglass_flowing_sand", "timestamp": ts }))
        .send()
        .await;
    let _ = http
        .post(slack_inbound_url(token, "reactions.add"))
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

    // ── W2-7: slack_workspace_domain_from_url ────────────────

    #[test]
    fn workspace_domain_extracted_from_auth_test_url() {
        assert_eq!(
            slack_workspace_domain_from_url("https://acme.slack.com/"),
            Some("acme".to_string())
        );
        // auth.test's `url` field is not guaranteed to carry a trailing
        // slash across API versions.
        assert_eq!(
            slack_workspace_domain_from_url("https://acme.slack.com"),
            Some("acme".to_string())
        );
    }

    #[test]
    fn workspace_domain_rejects_non_slack_hosts() {
        assert_eq!(
            slack_workspace_domain_from_url("https://evil.example.com/"),
            None
        );
        assert_eq!(slack_workspace_domain_from_url("not a url"), None);
        assert_eq!(slack_workspace_domain_from_url(""), None);
    }

    #[test]
    fn workspace_domain_rejects_bare_slack_com() {
        // `https://slack.com/` has no workspace subdomain — must not treat
        // the empty prefix as a valid domain.
        assert_eq!(slack_workspace_domain_from_url("https://slack.com/"), None);
    }

    // ── W2-7: record + read round trip (integration-level, real files) ──

    #[test]
    fn slack_workspace_domain_round_trips_through_persisted_file() {
        let dir = tempfile::tempdir().unwrap();
        crate::channel_link::record_slack_workspace_domain(dir.path(), "acme");
        // Re-derive the same "在 Slack 中開啟" URL channel_link.rs would
        // build for a channel, proving the file this function writes is the
        // exact shape channel_link.rs's reader expects.
        let stored = std::fs::read_to_string(
            dir.path()
                .join(crate::channel_link::SLACK_WORKSPACE_STORE_FILE),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(value["domain"], "acme");
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
        assert!(is_valid_slack_response_url(
            "https://hooks.slack.com/actions/T123/456/abc"
        ));
        // Unanchored-substring attack must fail (coding convention #2).
        assert!(!is_valid_slack_response_url(
            "https://hooks.slack.com.evil.com/x"
        ));
        assert!(!is_valid_slack_response_url(
            "http://hooks.slack.com/actions/x"
        )); // not https
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

    // ── W1-5: overflow menu payload resolution ──────────────────────────

    #[test]
    fn slack_action_payload_reads_overflow_selected_value() {
        let action = serde_json::json!({
            "type": "overflow",
            "action_id": "duduclaw:goal_more:t1",
            "selected_option": {
                "text": { "type": "plain_text", "text": "👤 交給我" },
                "value": "duduclaw:decide:goal:take:t1"
            }
        });
        assert_eq!(
            slack_action_payload(&action),
            "duduclaw:decide:goal:take:t1"
        );
    }

    #[test]
    fn slack_action_payload_reads_button_action_id_directly() {
        let action = serde_json::json!({
            "type": "button",
            "action_id": "duduclaw:decide:goal:retry:t1",
            "value": "t1"
        });
        assert_eq!(
            slack_action_payload(&action),
            "duduclaw:decide:goal:retry:t1"
        );
    }

    #[test]
    fn slack_action_payload_degrades_to_empty_on_a_malformed_overflow() {
        // An overflow entry missing `selected_option` (should never happen on
        // a real Slack payload) must not panic — empty string, which
        // `decision_notify::route_press` then fails closed on.
        let action =
            serde_json::json!({ "type": "overflow", "action_id": "duduclaw:goal_more:t1" });
        assert_eq!(slack_action_payload(&action), "");
    }
}

#[cfg(test)]
mod quoted_context_tests {
    use super::*;

    #[test]
    fn shared_message_attachment_builds_quote_block() {
        let event = serde_json::json!({
            "text": "這段是什麼意思",
            "attachments": [{
                "is_share": true,
                "author_id": "U123",
                "author_name": "Amy",
                "text": "季報顯示毛利率提升 2 個百分點"
            }]
        });
        let block = slack_quoted_context(&event, "UBOT").expect("quote block");
        assert!(block.contains("毛利率"));
        assert!(block.contains("Amy"));
    }

    #[test]
    fn bot_authored_share_is_labeled_self() {
        let event = serde_json::json!({
            "attachments": [{ "is_share": true, "author_id": "UBOT", "text": "已完成部署" }]
        });
        let block = slack_quoted_context(&event, "UBOT").expect("quote block");
        assert!(block.contains(channel_format::QUOTED_SELF_LABEL));
    }

    #[test]
    fn plain_link_unfurl_without_share_signal_is_ignored() {
        let event = serde_json::json!({
            "text": "看看這個",
            "attachments": [{ "title": "Some page", "text": "preview text", "service_name": "web" }]
        });
        assert!(slack_quoted_context(&event, "UBOT").is_none());
        assert!(slack_quoted_context(&serde_json::json!({"text": "x"}), "UBOT").is_none());
    }
}

#[derive(Clone)]
struct SlackDecisionAccount {
    account_id: String,
    team_id: String,
    app_id: String,
}
impl SlackDecisionAccount {
    fn matches(&self, p: &serde_json::Value) -> bool {
        p["team_id"].as_str().or_else(|| p["team"]["id"].as_str()) == Some(self.team_id.as_str())
            && p["api_app_id"].as_str() == Some(self.app_id.as_str())
    }
}
/// Why the decision account could not be verified (F4, review L7). Carries
/// Slack's own `error` / `needed` fields, never the token.
fn slack_identity_failure(
    step: &str,
    error: impl Into<String>,
    needed: Option<&str>,
) -> crate::channel_decision_route::SlackIdentityFailure {
    let error: String = error.into();
    crate::channel_decision_route::SlackIdentityFailure {
        step: step.into(),
        error: duduclaw_core::truncate_bytes(&error, 120).to_string(),
        needed: needed.map(|n| duduclaw_core::truncate_bytes(n, 120).to_string()),
        at: chrono::Utc::now().to_rfc3339(),
    }
}

async fn slack_api_json(
    request: reqwest::RequestBuilder,
    step: &str,
) -> Result<serde_json::Value, crate::channel_decision_route::SlackIdentityFailure> {
    let response = request
        .send()
        .await
        .map_err(|_| slack_identity_failure(step, "request_failed", None))?;
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| slack_identity_failure(step, "unreadable_response", None))?;
    if body["ok"] != true {
        return Err(slack_identity_failure(
            step,
            body["error"].as_str().unwrap_or("not_ok"),
            body["needed"].as_str(),
        ));
    }
    Ok(body)
}

async fn verified_slack_decision_account(
    http: &reqwest::Client,
    token: &str,
    label: &str,
) -> Result<SlackDecisionAccount, crate::channel_decision_route::SlackIdentityFailure> {
    let auth = slack_api_json(
        http.post(slack_inbound_url(token, "auth.test"))
            .bearer_auth(token),
        "auth.test",
    )
    .await?;
    let field = |key: &str| {
        auth[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| slack_identity_failure("auth.test", format!("missing_{key}"), None))
    };
    let team = field("team_id")?;
    let user = field("user_id")?;
    let bot = field("bot_id")?;
    // `bots.info` needs the `users:read` scope; without it Slack answers
    // `{"ok":false,"error":"missing_scope","needed":"users:read"}`.
    let info = slack_api_json(
        http.get(slack_inbound_url(token, "bots.info"))
            .bearer_auth(token)
            .query(&[("bot", bot.as_str())]),
        "bots.info",
    )
    .await?;
    let app = info["bot"]["app_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| slack_identity_failure("bots.info", "missing_app_id", None))?
        .to_string();
    Ok(SlackDecisionAccount {
        account_id: format!("{label}|{team}|{user}|{app}"),
        team_id: team,
        app_id: app,
    })
}

#[cfg(test)]
mod bound_account_tests {
    use super::*;
    #[test]
    fn exact_socket_account_requires_verified_team_app_and_does_not_alias_another_bot() {
        let a = SlackDecisionAccount {
            account_id: "slack:alice|T1|U1|A1".into(),
            team_id: "T1".into(),
            app_id: "A1".into(),
        };
        let b = SlackDecisionAccount {
            account_id: "slack:bob|T1|U2|A2".into(),
            team_id: "T1".into(),
            app_id: "A2".into(),
        };
        let envelope = serde_json::json!({
            "team": {"id":"T1"},
            "api_app_id": "A1",
            "user": {"id":"H1"},
            "channel": {"id":"C1"}
        });
        assert!(a.matches(&envelope));
        assert!(!b.matches(&envelope));
        assert_ne!(a.account_id, b.account_id);
        assert!(!a.matches(&serde_json::json!({"team":{"id":"T2"},"api_app_id":"A1"})));
        assert!(!a.matches(&serde_json::json!({"team":{"id":"T1"}})));
    }
}

fn slack_execution_key(
    payload: &serde_json::Value,
    account: Option<&SlackDecisionAccount>,
) -> String {
    let event = payload.get("event").unwrap_or(payload);
    let channel = event["channel"]
        .as_str()
        .or_else(|| payload["channel_id"].as_str())
        .unwrap_or("control");
    let thread = event["thread_ts"].as_str().unwrap_or("");
    format!(
        "{}:{channel}:{thread}",
        account
            .map(|a| a.account_id.as_str())
            .unwrap_or("unverified")
    )
}
async fn slack_decision_fastlane(
    payload: &serde_json::Value,
    token: &str,
    ctx: &Arc<ReplyContext>,
    http: &reqwest::Client,
    account: Option<&SlackDecisionAccount>,
) -> bool {
    let Some(event) = payload.get("event") else {
        return false;
    };
    if event["type"] != "message" || event.get("bot_id").is_some() || event.get("subtype").is_some()
    {
        return false;
    }
    let Some(text) = event["text"].as_str() else {
        return false;
    };
    let text = strip_bot_mention(text);
    // Only a strict decision command (verb + full request id) is consumed
    // here; everything else, including WP1.6 thread replies to old cards,
    // goes to `handle_event` unchanged (F4, review M1/M2).
    if !crate::channel_decision_route::is_strict_decision(&text) {
        return false;
    }
    let channel = event["channel"].as_str().unwrap_or("");
    if event["channel_type"] != "im"
        && !ctx
            .channel_settings
            .is_channel_allowed("slack", "global", channel)
            .await
    {
        return true;
    }
    let context = crate::approval::DecisionContext {
        channel: "slack".into(),
        account_id: account
            .filter(|a| a.matches(payload))
            .map(|a| a.account_id.clone())
            .unwrap_or_default(),
        principal_id: event["user"].as_str().unwrap_or("").into(),
        conversation_id: event["thread_ts"].as_str().map_or_else(
            || channel.to_owned(),
            |thread| format!("{channel}:{thread}"),
        ),
    };
    let session = if event["channel_type"] == "im" {
        format!("slack:{}", context.principal_id)
    } else {
        format!("slack:group:{channel}")
    };
    let scope = crate::decision_notify::DecisionAccessScope {
        channel_id: (event["channel_type"] != "im").then_some(channel),
        guild_id: None,
        session_id: Some(&session),
    };
    if account.is_none() && !channel.is_empty() {
        // This bot's own account never verified (usually a missing
        // `users:read` scope for `bots.info`). Only a sender the channel
        // settings admit is told so (F5-C, review F4-L3); the access check
        // needs a complete context, so it runs with a placeholder account
        // that is never stored or compared with any request. Refused senders
        // get no answer, as for every other unverified-account refusal.
        let mut probe = context.clone();
        probe.account_id = "slack-account-unverified".into();
        if crate::decision_notify::check_trusted_decision_access(ctx, &probe, scope, &text)
            .await
            .is_err()
        {
            return true;
        }
        let mut body = json!({
            "channel": channel,
            "text": crate::channel_decision_route::SLACK_IDENTITY_UNAVAILABLE,
        });
        if let Some(thread) = event["thread_ts"].as_str() {
            body["thread_ts"] = json!(thread);
        }
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            http.post(slack_inbound_url(token, "chat.postMessage"))
                .bearer_auth(token)
                .json(&body)
                .send(),
        )
        .await;
        return true;
    }
    let Some(outcome) =
        crate::decision_notify::route_trusted_decision_fastlane_with_scope(ctx, &context, &text, scope)
            .await
    else {
        return false;
    };
    let target = crate::approval::TrustedReplyTarget::new(
        context,
        token.to_owned(),
        channel.to_owned(),
        event["thread_ts"].as_str().map(str::to_owned),
    );
    if let Ok(target) = target {
        let ack = outcome.unwrap_or_else(|e| format!("⚠ {e}"));
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            target.sender(http.clone()).send_text(&ack),
        )
        .await;
    }
    true
}

fn slack_inbound_url(token: &str, method: &str) -> String {
    let original = format!("{SLACK_API}/{method}");
    #[cfg(test)]
    let original = crate::test_channel_provider::url(token, &original);
    #[cfg(not(test))]
    let _ = token;
    original
}

#[cfg(test)]
mod socket_fastlane_tests {
    use super::*;
    use crate::decision_notify::native_loop_fixture::{NativeCuFixture, install_named_job};
    use crate::test_channel_provider::TestChannelProvider;
    use std::sync::atomic::Ordering;
    fn envelope(id: &str, thread: &str, user: &str, text: &str) -> String {
        serde_json::json!({
            "envelope_id": id,
            "type": "events_api",
            "payload": {
                           "team_id": "T1",
                           "api_app_id": "A1",
                           "event": {
                                        "type": "message",
                                        "channel": "C1",
                                        "channel_type": "im",
                                        "user": user,
                                        "thread_ts": thread,
                                        "ts": id,
                                        "text": text
                                    }
                       }
        }).to_string()
    }
    #[tokio::test]
    async fn actual_socket_reads_same_thread_confirm_while_cu_waits() {
        let provider = TestChannelProvider::start().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        provider.enqueue_response(
            "apps.connections.open",
            serde_json::json!({"ok":true,"url":format!("ws://{addr}")}),
        );
        for _ in 0..2 {
            provider.enqueue_response("auth.test",serde_json::json!({
                "ok": true,
                "team_id": "T1",
                "user_id": "UBOT",
                "bot_id": "B1",
                "url": "https://fixture.slack.com/"
            }));
        }
        provider.enqueue_response(
            "bots.info",
            serde_json::json!({"ok":true,"bot":{"app_id":"A1"}}),
        );
        let fixture = NativeCuFixture::new(&provider.token).await;
        let _cu = install_named_job(&provider.token, "run cu", fixture.work("socket-cu-turn"));
        let token = provider.token.clone();
        let ctx = fixture.ctx.clone();
        let socket =
            tokio::spawn(async move { run_socket_mode(&token, &token, &ctx, "slack", None).await });
        let (stream, _) = listener.accept().await.unwrap();
        let mut server = tokio_tungstenite::accept_async(stream).await.unwrap();
        server
            .send(Message::Text(
                envelope("1", "123.45", "H1", "run cu").into(),
            ))
            .await
            .unwrap();
        let id = fixture.pending_id().await;
        server
            .send(Message::Text(
                envelope("2", "other-thread", "H1", &format!("確認 {id}")).into(),
            ))
            .await
            .unwrap();
        server
            .send(Message::Text(
                envelope("3", "123.45", "H2", &format!("確認 {id}")).into(),
            ))
            .await
            .unwrap();
        // Every socket frame is ACKed before routing; wait for the rejected
        // third frame ACK to prove the original CU has not blocked ingestion.
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(Ok(Message::Text(text))) = server.next().await {
                    if serde_json::from_str::<serde_json::Value>(&text).unwrap()["envelope_id"]
                        == "3"
                    {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
        let mut wrong_app: serde_json::Value = serde_json::from_str(&envelope(
            "wrong-app",
            "123.45",
            "H1",
            &format!("確認 {id}"),
        ))
        .unwrap();
        wrong_app["payload"]["api_app_id"] = serde_json::json!("A2");
        server
            .send(Message::Text(wrong_app.to_string().into()))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(Ok(Message::Text(text))) = server.next().await {
                    if serde_json::from_str::<serde_json::Value>(&text).unwrap()["envelope_id"]
                        == "wrong-app"
                    {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
        server
            .send(Message::Text(
                envelope("4", "123.45", "H1", &format!("確認 {id}")).into(),
            ))
            .await
            .unwrap();
        fixture.wait_executed().await;
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 1);
        assert!(provider.requests().iter().any(|r| {
            r.path.ends_with("chat.postMessage")
                && r.body["thread_ts"] == "123.45"
                && r.body["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&id))
        }));
        let b = crate::approval::ApprovalBroker::open(fixture.home.path()).unwrap();
        assert_eq!(
            b.list_operations().await.unwrap()[0].state,
            crate::approval::OperationState::Succeeded
        );
        socket.abort();
        assert!(socket.await.unwrap_err().is_cancelled());
    }
}

async fn slack_busy_ack(payload: serde_json::Value, token: String, http: reqwest::Client) {
    let event = payload.get("event").unwrap_or(&payload);
    let Some(channel) = event["channel"]
        .as_str()
        .or_else(|| payload["channel_id"].as_str())
    else {
        return;
    };
    let mut body = serde_json::json!({"channel":channel,"text":"目前工作佇列已滿，這則訊息尚未受理；請稍後重新傳送。"});
    if let Some(thread) = event["thread_ts"].as_str() {
        body["thread_ts"] = serde_json::json!(thread);
    }
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        http.post(slack_inbound_url(&token, "chat.postMessage"))
            .bearer_auth(&token)
            .json(&body)
            .send(),
    )
    .await;
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
            "user",
            "blocked",
            "channel",
            "pairing",
            "corrupt",
            "db",
            "binding",
        ] {
            let provider = TestChannelProvider::start().await;
            let fixture = NativeCuFixture::new(&provider.token).await;
            let account = SlackDecisionAccount {
                account_id: "slack|T1|UBOT|A1".into(),
                team_id: "T1".into(),
                app_id: "A1".into(),
            };
            let context = crate::approval::DecisionContext {
                channel: "slack".into(),
                account_id: account.account_id.clone(),
                principal_id: "H1".into(),
                conversation_id: "C1:123.45".into(),
            };
            let id = pending_native_request(&fixture, context).await;
            let allowed = mode == "allow" || mode == "session_allow";
            if mode == "session_allow" {
                fixture
                    .ctx
                    .channel_settings
                    // F4: both the person and the conversation are listed.
                    .set(
                        "slack",
                        "global",
                        "allowed_users",
                        r#"["slack:group:C1","H1"]"#,
                    )
                    .await
                    .unwrap();
            } else {
                revoke_native_access(&fixture, "slack", "global", mode).await;
            }
            let data = json!({
                "type": "block_actions",
                "team": {"id":"T1"},
                "api_app_id": "A1",
                "user": {"id":"H1"},
                "channel": {"id":"C1"},
                "message": {"thread_ts":"123.45"},
                "response_url": format!("https://hooks.slack.com/actions/{}",provider.token),
                "actions": [
                    {"action_id": crate::decision_action::encode(
                        crate::decision_action::DecisionSource::Approval,
                        crate::decision_action::DecisionAct::Approve,
                        id.as_str()
                    )}
                           ]
            });
            if !allowed {
                assert!(slack_decision_fastlane(&json!({
                    "team_id": "T1",
                    "api_app_id": "A1",
                    "event": {
                                 "type": "message",
                                 "channel": "C1",
                                 "channel_type": "channel",
                                 "user": "H1",
                                 "thread_ts": "123.45",
                                 "text": format!("確認 {}",id.as_str())
                             }
                }),&provider.token,&fixture.ctx,&reqwest::Client::new(),Some(&account)).await);
                assert_native_pending(&fixture, &id).await;
            }
            handle_interactive_envelope(
                data,
                &fixture.ctx,
                &reqwest::Client::new(),
                Some(&account),
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
                    .any(|r| r.body["response_type"] == "ephemeral"
                        && r.body["text"].as_str().is_some_and(|text| if allowed {
                            text.contains("同意")
                        } else {
                            text.contains("⚠")
                        })),
                "native rejection did not ACK {mode}"
            );
        }
    }
}

#[cfg(test)]
#[path = "channel_decision_route/adapter_tests/slack.rs"]
mod f4_decision_route_tests;
