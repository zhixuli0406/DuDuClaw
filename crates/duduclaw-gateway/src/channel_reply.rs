//! Shared reply builder for all channel bots.

use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use tokio::sync::RwLock;

/// Build a reply message for an incoming user message.
///
/// Reads the main agent's info from the registry and formats a response.
/// When Claude API integration is complete, this will route to the AI.
pub async fn build_reply(text: &str, registry: &Arc<RwLock<AgentRegistry>>) -> String {
    let reg = registry.read().await;
    let main_agent = reg.main_agent();

    let agent_name = main_agent
        .map(|a| a.config.agent.display_name.as_str())
        .unwrap_or("DuDuClaw");

    let agent_icon = main_agent
        .map(|a| a.config.agent.icon.as_str())
        .unwrap_or("🐾");

    let skills: Vec<&str> = main_agent
        .map(|a| a.skills.iter().map(|s| s.name.as_str()).collect())
        .unwrap_or_default();

    let skills_text = if skills.is_empty() {
        String::from("（尚無技能）")
    } else {
        skills.join(", ")
    };

    let truncated: String = text.chars().take(200).collect();

    format!(
        "{agent_icon} {agent_name} 收到你的訊息！\n\n\
        > {truncated}\n\n\
        📋 目前狀態\n\
        • Agent: {agent_name}\n\
        • 技能: {skills_text}\n\
        • AI 回覆: 即將支援（Claude API 整合中）\n\n\
        💡 這是 DuDuClaw 的自動回覆，AI 對話功能開發中。"
    )
}
