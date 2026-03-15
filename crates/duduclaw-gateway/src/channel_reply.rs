//! Shared AI reply builder for all channel bots.
//!
//! Calls the Anthropic Claude API to generate responses, using the
//! main agent's SOUL.md as the system prompt.

use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info};

const ANTHROPIC_API: &str = "https://api.anthropic.com/v1/messages";

// ── Claude API types ────────────────────────────────────────

#[derive(Serialize)]
struct ClaudeRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<ClaudeMessage>,
}

#[derive(Serialize, Deserialize)]
struct ClaudeMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ClaudeResponse {
    content: Option<Vec<ClaudeContent>>,
    error: Option<ClaudeError>,
}

#[derive(Deserialize)]
struct ClaudeContent {
    text: Option<String>,
}

#[derive(Deserialize)]
struct ClaudeError {
    message: Option<String>,
}

// ── Shared state ────────────────────────────────────────────

/// Shared context for building replies, initialized once at gateway start.
pub struct ReplyContext {
    pub registry: Arc<RwLock<AgentRegistry>>,
    pub home_dir: std::path::PathBuf,
    pub http: reqwest::Client,
}

impl ReplyContext {
    pub fn new(registry: Arc<RwLock<AgentRegistry>>, home_dir: std::path::PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self { registry, home_dir, http }
    }
}

// ── Public API ──────────────────────────────────────────────

/// Build a reply for an incoming user message.
///
/// Tries Claude API first. Falls back to a static response if the API
/// key is missing or the request fails.
pub async fn build_reply(text: &str, ctx: &ReplyContext) -> String {
    let reg = ctx.registry.read().await;
    let main_agent = reg.main_agent();

    // Get agent config
    let model = main_agent
        .map(|a| a.config.model.preferred.clone())
        .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());

    let system_prompt = build_system_prompt(main_agent);

    // Try to get API key
    let api_key = get_api_key(&ctx.home_dir).await;
    drop(reg); // release lock before HTTP call

    if let Some(key) = api_key {
        match call_claude(&ctx.http, &key, &model, &system_prompt, text).await {
            Ok(reply) => {
                info!("🤖 Claude replied ({} chars)", reply.len());
                return reply;
            }
            Err(e) => {
                error!("Claude API error: {e}");
                // Fall through to static reply
            }
        }
    }

    // Fallback: static reply
    build_fallback_reply(text, &ctx.registry).await
}

/// Simpler version that takes just a registry (for backwards compat).
/// Always returns the static fallback.
pub async fn build_reply_simple(text: &str, registry: &Arc<RwLock<AgentRegistry>>) -> String {
    build_fallback_reply(text, registry).await
}

// ── Internal ────────────────────────────────────────────────

fn build_system_prompt(agent: Option<&duduclaw_agent::registry::LoadedAgent>) -> String {
    let mut parts = Vec::new();

    if let Some(a) = agent {
        if let Some(soul) = &a.soul {
            parts.push(soul.clone());
        }
        if let Some(identity) = &a.identity {
            parts.push(identity.clone());
        }
        for skill in &a.skills {
            parts.push(format!("## Skill: {}\n{}", skill.name, skill.content));
        }
    }

    if parts.is_empty() {
        "You are DuDuClaw, a helpful AI assistant. Reply concisely in the user's language.".to_string()
    } else {
        parts.join("\n\n---\n\n")
    }
}

async fn call_claude(
    http: &reqwest::Client,
    api_key: &str,
    model: &str,
    system: &str,
    user_message: &str,
) -> Result<String, String> {
    let body = ClaudeRequest {
        model: model.to_string(),
        max_tokens: 1024,
        system: system.to_string(),
        messages: vec![ClaudeMessage {
            role: "user".to_string(),
            content: user_message.to_string(),
        }],
    };

    let resp = http
        .post(ANTHROPIC_API)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("API error ({status}): {}", &text[..text.len().min(200)]));
    }

    let data: ClaudeResponse = resp
        .json()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    if let Some(err) = data.error {
        return Err(format!("Claude error: {}", err.message.unwrap_or_default()));
    }

    let reply = data
        .content
        .and_then(|blocks| {
            blocks
                .into_iter()
                .filter_map(|b| b.text)
                .collect::<Vec<_>>()
                .into_iter()
                .next()
        })
        .unwrap_or_else(|| "（AI 沒有產生回覆）".to_string());

    Ok(reply)
}

async fn get_api_key(home_dir: &Path) -> Option<String> {
    // 1. Environment variable
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
        && !key.is_empty()
    {
        return Some(key);
    }
    // 2. config.toml [api] section
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let api = table.get("api")?.as_table()?;
    let key = api.get("anthropic_api_key")?.as_str()?;
    if key.is_empty() { None } else { Some(key.to_string()) }
}

async fn build_fallback_reply(text: &str, registry: &Arc<RwLock<AgentRegistry>>) -> String {
    let reg = registry.read().await;
    let main_agent = reg.main_agent();

    let agent_name = main_agent
        .map(|a| a.config.agent.display_name.as_str())
        .unwrap_or("DuDuClaw");

    let truncated: String = text.chars().take(200).collect();

    format!(
        "{agent_name} 收到你的訊息：\n\n\
        > {truncated}\n\n\
        ⚠ AI 回覆暫時不可用（API Key 未設定或請求失敗）\n\
        請在設定中加入 ANTHROPIC_API_KEY。"
    )
}
