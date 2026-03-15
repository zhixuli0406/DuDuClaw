//! Shared AI reply builder for all channel bots.
//!
//! Calls the Claude Code SDK (Python) via subprocess for AI responses,
//! using the multi-account rotator for key management and budget tracking.
//! Falls back to direct Anthropic API if Python is unavailable.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use tokio::sync::RwLock;
use tracing::{info, warn};


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
    // Determine which agent to use: config.toml default_agent → main_agent() → fallback
    let default_agent_name = get_default_agent(&ctx.home_dir).await;

    let reg = ctx.registry.read().await;
    let agent = if let Some(name) = &default_agent_name {
        reg.get(name).or_else(|| reg.main_agent())
    } else {
        reg.main_agent()
    };

    if let Some(a) = agent {
        info!("Using agent: {} ({})", a.config.agent.display_name, a.config.agent.name);
    }

    let model = agent
        .map(|a| a.config.model.preferred.clone())
        .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());

    let system_prompt = build_system_prompt(agent);
    drop(reg);

    // 1. Try `claude` CLI directly (Claude Code SDK — has built-in tools)
    match call_claude_cli(text, &model, &system_prompt, &ctx.home_dir).await {
        Ok(reply) => {
            info!("🤖 Claude replied via Claude Code SDK ({} chars)", reply.len());
            return reply;
        }
        Err(e) => {
            warn!("claude CLI unavailable: {e}");
        }
    }

    // 2. Fallback: Python wrapper (with account rotator)
    match call_python_sdk_v2(text, &model, &system_prompt, &ctx.home_dir).await {
        Ok(reply) => {
            info!("🤖 Claude replied via Python SDK ({} chars)", reply.len());
            return reply;
        }
        Err(e) => {
            warn!("Python SDK unavailable: {e}");
        }
    }

    // 3. Fallback: static error
    let reg = ctx.registry.read().await;
    let name = reg
        .main_agent()
        .map(|a| a.config.agent.display_name.as_str())
        .unwrap_or("DuDuClaw");
    format!(
        "{name} 收到你的訊息，但目前無法回覆。\n\
        請安裝 Claude Code SDK：\n\
        $ npm install -g @anthropic-ai/claude-code\n\
        並設定 API Key：\n\
        $ export ANTHROPIC_API_KEY=sk-ant-..."
    )
}

// ── Python SDK subprocess ───────────────────────────────────

// ── Claude Code SDK (claude CLI) ────────────────────────────

/// Call the `claude` CLI (Claude Code SDK) directly.
///
/// The claude CLI has built-in tools: bash, web search, file operations, etc.
/// This is the primary method for AI conversation.
async fn call_claude_cli(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    // Find claude binary
    let claude_path = which_claude().ok_or_else(|| "claude CLI not found in PATH".to_string())?;

    // Get API key
    let api_key = get_api_key(home_dir)
        .await
        .ok_or_else(|| "No API key configured".to_string())?;

    let mut cmd = tokio::process::Command::new(&claude_path);
    cmd.args(["-p", user_message, "--model", model, "--output-format", "text"]);

    // Pass system prompt
    if !system_prompt.is_empty() {
        cmd.args(["--system-prompt", system_prompt]);
    }

    cmd.env("ANTHROPIC_API_KEY", &api_key);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        cmd.output(),
    )
    .await
    .map_err(|_| "claude CLI timeout (120s)".to_string())?
    .map_err(|e| format!("claude CLI spawn error: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "claude CLI exit {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(200).collect::<String>()
        ));
    }

    if stdout.is_empty() {
        return Err("Empty response from claude CLI".to_string());
    }

    Ok(stdout)
}

/// Find the `claude` binary in PATH or common locations.
fn which_claude() -> Option<String> {
    // Check PATH
    if let Ok(output) = std::process::Command::new("which")
        .arg("claude")
        .output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(path);
        }
    }

    // Common locations
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.npm-global/bin/claude"),
        "/usr/local/bin/claude".to_string(),
        format!("{home}/.claude/bin/claude"),
        format!("{home}/.local/bin/claude"),
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    None
}

// ── Python SDK subprocess (fallback) ────────────────────────

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

/// Read the default_agent from config.toml [general] section.
async fn get_default_agent(home_dir: &Path) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let general = table.get("general")?.as_table()?;
    let name = general.get("default_agent")?.as_str()?;
    if name.is_empty() { None } else { Some(name.to_string()) }
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
