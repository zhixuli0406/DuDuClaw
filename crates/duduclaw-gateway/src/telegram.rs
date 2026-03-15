//! Lightweight Telegram Bot long-polling integration.
//!
//! Runs as a background tokio task alongside the WebSocket gateway.
//! Receives messages from Telegram, routes them to the configured main agent,
//! and sends responses back.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use duduclaw_agent::registry::AgentRegistry;

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
    registry: Arc<RwLock<AgentRegistry>>,
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
                } else {
                    warn!("Telegram getMe failed: {}", data.description.unwrap_or_default());
                    return None;
                }
            }
        }
        Err(e) => {
            warn!("Telegram connection failed: {e}");
            return None;
        }
    }

    let handle = tokio::spawn(async move {
        poll_loop(client, api_base, registry).await;
    });

    Some(handle)
}

// ── Internal ────────────────────────────────────────────────

async fn read_telegram_token(home_dir: &Path) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let channels = table.get("channels")?.as_table()?;
    let token = channels.get("telegram_bot_token")?.as_str()?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

async fn poll_loop(
    client: reqwest::Client,
    api_base: String,
    registry: Arc<RwLock<AgentRegistry>>,
) {
    let mut offset: i64 = 0;
    info!("Telegram polling started");

    loop {
        let url = format!("{api_base}/getUpdates?offset={offset}&timeout=25");

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("Telegram poll error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        let data: TgResponse<Vec<TgUpdate>> = match resp.json().await {
            Ok(d) => d,
            Err(e) => {
                warn!("Telegram parse error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        if !data.ok {
            warn!("Telegram API error: {}", data.description.unwrap_or_default());
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

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

                    let reply = build_reply(text, &registry).await;
                    send_reply(&client, &api_base, msg.chat.id, &reply).await;
                }
            }
        }
    }
}

async fn build_reply(
    text: &str,
    registry: &Arc<RwLock<AgentRegistry>>,
) -> String {
    let reg = registry.read().await;

    // Find main agent
    let main_agent = reg.main_agent();

    let agent_name = main_agent
        .map(|a| a.config.agent.display_name.as_str())
        .unwrap_or("DuDuClaw");

    let agent_icon = main_agent
        .map(|a| a.config.agent.icon.as_str())
        .unwrap_or("🐾");

    // List available skills
    let skills: Vec<&str> = main_agent
        .map(|a| a.skills.iter().map(|s| s.name.as_str()).collect())
        .unwrap_or_default();

    let skills_text = if skills.is_empty() {
        String::from("（尚無技能）")
    } else {
        skills.join(", ")
    };

    // Build response
    // TODO: integrate Claude API for actual AI responses
    format!(
        "{agent_icon} *{agent_name}* 收到你的訊息！\n\n\
        > {text}\n\n\
        📋 *目前狀態*\n\
        • Agent: {agent_name}\n\
        • 技能: {skills_text}\n\
        • AI 回覆: _即將支援（Claude API 整合中）_\n\n\
        💡 這是 DuDuClaw 的自動回覆，AI 對話功能開發中。",
        text = text.chars().take(200).collect::<String>(),
    )
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
