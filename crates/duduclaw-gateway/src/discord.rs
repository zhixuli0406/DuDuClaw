//! Lightweight Discord Bot integration.
//!
//! Verifies bot token on startup. Message receiving requires the Gateway
//! WebSocket (not implemented yet), so for now the bot can only send messages
//! and verify connectivity.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

use duduclaw_agent::registry::AgentRegistry;

const DISCORD_API: &str = "https://discord.com/api/v10";

#[derive(Debug, Deserialize)]
struct DiscordUser {
    username: Option<String>,
    id: Option<String>,
}

/// Start Discord bot verification on gateway startup.
///
/// Discord message receiving requires the Gateway WebSocket protocol
/// (wss://gateway.discord.gg) which is more complex. For now we only verify
/// the token and log the bot identity.
pub async fn start_discord_bot(
    home_dir: &Path,
    _registry: Arc<RwLock<AgentRegistry>>,
) -> Option<()> {
    let token = read_discord_token(home_dir).await?;
    if token.is_empty() {
        return None;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    match client
        .get(format!("{DISCORD_API}/users/@me"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().is_success() {
                if let Ok(user) = resp.json::<DiscordUser>().await {
                    let name = user.username.as_deref().unwrap_or("unknown");
                    let id = user.id.as_deref().unwrap_or("?");
                    info!("🎮 Discord bot connected: {name} ({id})");
                    info!("   Discord 訊息接收需要 Gateway WebSocket（開發中）");
                }
            } else {
                warn!("Discord bot token invalid (HTTP {})", resp.status());
                return None;
            }
        }
        Err(e) => {
            warn!("Discord connection failed: {e}");
            return None;
        }
    }

    Some(())
}

async fn read_discord_token(home_dir: &Path) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let channels = table.get("channels")?.as_table()?;
    let token = channels.get("discord_bot_token")?.as_str()?;
    if token.is_empty() { None } else { Some(token.to_string()) }
}
