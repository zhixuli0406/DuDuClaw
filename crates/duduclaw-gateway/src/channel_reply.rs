//! Shared AI reply builder for all channel bots.
//!
//! Calls the Claude Code SDK (Python) via subprocess for AI responses,
//! using the multi-account rotator for key management and budget tracking.
//! Falls back to direct Anthropic API if Python is unavailable.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

const ANTHROPIC_API: &str = "https://api.anthropic.com/v1/messages";

// ── Claude API types (for direct fallback) ──────────────────

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
    pub home_dir: PathBuf,
    pub http: reqwest::Client,
}

impl ReplyContext {
    pub fn new(registry: Arc<RwLock<AgentRegistry>>, home_dir: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            registry,
            home_dir,
            http,
        }
    }
}

// ── Public API ──────────────────────────────────────────────

/// Build a reply for an incoming user message.
///
/// Strategy:
/// 1. Try Python Claude Code SDK (subprocess) — uses rotator + budget tracking
/// 2. Fallback to direct Anthropic API (Rust reqwest) — single key only
/// 3. Fallback to static error message
pub async fn build_reply(text: &str, ctx: &ReplyContext) -> String {
    let reg = ctx.registry.read().await;
    let main_agent = reg.main_agent();

    let model = main_agent
        .map(|a| a.config.model.preferred.clone())
        .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());

    let system_prompt = build_system_prompt(main_agent);
    drop(reg);

    // 1. Try Python Claude Code SDK (with rotator + budget tracking)
    match call_python_sdk_v2(text, &model, &system_prompt, &ctx.home_dir).await {
        Ok(reply) => {
            info!("🤖 Claude replied via SDK ({} chars)", reply.len());
            return reply;
        }
        Err(e) => {
            warn!("Python SDK unavailable, falling back to direct API: {e}");
        }
    }

    // 2. Fallback: direct Anthropic API
    let api_key = get_api_key(&ctx.home_dir).await;
    if let Some(key) = api_key {
        match call_claude_direct(&ctx.http, &key, &model, &system_prompt, text).await {
            Ok(reply) => {
                info!("🤖 Claude replied via direct API ({} chars)", reply.len());
                return reply;
            }
            Err(e) => {
                error!("Claude API error: {e}");
            }
        }
    }

    // 3. Fallback: static
    let reg = ctx.registry.read().await;
    let name = reg
        .main_agent()
        .map(|a| a.config.agent.display_name.as_str())
        .unwrap_or("DuDuClaw");
    format!(
        "{name} 收到你的訊息，但目前無法回覆。\n\
        請確認 API Key 已設定：\n\
        $ export ANTHROPIC_API_KEY=sk-ant-..."
    )
}

// ── Python SDK subprocess ───────────────────────────────────

/// Find the Python source path for `duduclaw.sdk.chat`.
fn find_python_path(home_dir: &Path) -> String {
    // Try common locations
    let candidates = [
        // Installed via pip
        String::new(), // use system PYTHONPATH
        // Development: project root python/
        home_dir
            .parent()
            .unwrap_or(home_dir)
            .join("python")
            .to_string_lossy()
            .to_string(),
        // Homebrew / source install
        "/opt/duduclaw".to_string(),
    ];

    for path in &candidates {
        if !path.is_empty() && Path::new(path).join("duduclaw").exists() {
            return path.clone();
        }
    }

    // Fallback: return existing PYTHONPATH
    std::env::var("PYTHONPATH").unwrap_or_default()
}

/// Call the Python Claude Code SDK via subprocess.
///
/// The Python SDK uses the `anthropic` package with the `AccountRotator`
/// for multi-account rotation, budget tracking, and error recovery.
async fn call_python_sdk_v2(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let prompt_file = home_dir.join(".tmp_system_prompt.md");
    tokio::fs::write(&prompt_file, system_prompt)
        .await
        .map_err(|e| format!("Write prompt: {e}"))?;

    let config_path = home_dir.join("config.toml");
    let python_path = find_python_path(home_dir);

    let mut child = Command::new("python3")
        .args([
            "-m",
            "duduclaw.sdk.chat",
            "--model",
            model,
            "--system-prompt-file",
            &prompt_file.to_string_lossy(),
            "--config",
            &config_path.to_string_lossy(),
        ])
        .env("PYTHONPATH", &python_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Spawn python3: {e}"))?;

    // Write user message to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(user_message.as_bytes())
            .await
            .map_err(|e| format!("Write stdin: {e}"))?;
        drop(stdin); // close stdin to signal EOF
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("Wait: {e}"))?;

    let _ = tokio::fs::remove_file(&prompt_file).await;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(format!(
            "exit {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(200).collect::<String>()
        ));
    }

    if stdout.is_empty() {
        return Err("Empty response".to_string());
    }

    Ok(stdout)
}

// ── Direct Anthropic API (fallback) ─────────────────────────

async fn call_claude_direct(
    http: &reqwest::Client,
    api_key: &str,
    model: &str,
    system: &str,
    user_message: &str,
) -> Result<String, String> {
    let body = ClaudeRequest {
        model: model.to_string(),
        max_tokens: 2048,
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
        .map_err(|e| format!("HTTP: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("({status}): {}", &text[..text.len().min(200)]));
    }

    let data: ClaudeResponse = resp.json().await.map_err(|e| format!("Parse: {e}"))?;

    if let Some(err) = data.error {
        return Err(err.message.unwrap_or_default());
    }

    Ok(data
        .content
        .and_then(|blocks| blocks.into_iter().filter_map(|b| b.text).next())
        .unwrap_or_else(|| "（AI 沒有產生回覆）".to_string()))
}

// ── Helpers ─────────────────────────────────────────────────

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
        "You are DuDuClaw, a helpful AI assistant. Reply concisely in the user's language."
            .to_string()
    } else {
        parts.join("\n\n---\n\n")
    }
}

async fn get_api_key(home_dir: &Path) -> Option<String> {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
        && !key.is_empty()
    {
        return Some(key);
    }
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let api = table.get("api")?.as_table()?;
    let key = api.get("anthropic_api_key")?.as_str()?;
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}
