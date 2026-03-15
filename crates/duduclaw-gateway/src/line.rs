//! Lightweight LINE Messaging API integration.
//!
//! LINE uses webhooks (not polling) to receive messages, so full message
//! receiving requires an HTTPS endpoint. For now we verify the token and
//! expose a webhook handler that can be mounted on the Axum router.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

use duduclaw_agent::registry::AgentRegistry;

const LINE_API: &str = "https://api.line.me/v2/bot";

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LineBotInfo {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "userId")]
    user_id: Option<String>,
}

/// Start LINE bot verification on gateway startup.
///
/// LINE message receiving requires a webhook endpoint (HTTPS).
/// The webhook route can be added to the Axum router in a future version.
pub async fn start_line_bot(
    home_dir: &Path,
    _registry: Arc<RwLock<AgentRegistry>>,
) -> Option<()> {
    let token = read_line_token(home_dir).await?;
    if token.is_empty() {
        return None;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    // Verify token with bot info endpoint
    match client
        .get(format!("{LINE_API}/info"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().is_success() {
                if let Ok(info) = resp.json::<LineBotInfo>().await {
                    let name = info.display_name.as_deref().unwrap_or("unknown");
                    info!("💬 LINE bot connected: {name}");
                    info!("   LINE 訊息接收需要 Webhook 端點（開發中）");
                } else {
                    info!("💬 LINE bot token verified");
                    info!("   LINE 訊息接收需要 Webhook 端點（開發中）");
                }
            } else {
                warn!("LINE bot token invalid (HTTP {})", resp.status());
                return None;
            }
        }
        Err(e) => {
            warn!("LINE connection failed: {e}");
            return None;
        }
    }

    Some(())
}

async fn read_line_token(home_dir: &Path) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let channels = table.get("channels")?.as_table()?;
    let token = channels.get("line_channel_token")?.as_str()?;
    if token.is_empty() { None } else { Some(token.to_string()) }
}
