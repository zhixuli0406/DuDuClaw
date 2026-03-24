//! Shared helper for calling the Claude CLI (Claude Code SDK) on behalf of an agent.
//!
//! Used by both the cron scheduler and the agent dispatcher.

use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use tokio::sync::RwLock;
use tracing::info;

/// Build a system prompt from an agent's loaded markdown files.
fn build_system_prompt(agent: &duduclaw_agent::LoadedAgent) -> String {
    let mut parts = Vec::new();

    if let Some(soul) = &agent.soul {
        parts.push(format!("# Soul\n{soul}"));
    }
    if let Some(identity) = &agent.identity {
        parts.push(format!("# Identity\n{identity}"));
    }
    for skill in &agent.skills {
        parts.push(format!("# Skill: {}\n{}", skill.name, skill.content));
    }
    if let Some(memory) = &agent.memory {
        parts.push(format!("# Memory\n{memory}"));
    }

    parts.join("\n\n---\n\n")
}

/// Look up an agent from the registry and call the Claude CLI with a prompt.
///
/// Returns the response text on success, or an error message.
pub async fn call_claude_for_agent(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
) -> Result<String, String> {
    let reg = registry.read().await;

    // Look up agent; fall back to main agent if "default"
    let agent = if agent_id == "default" {
        reg.main_agent()
    } else {
        reg.get(agent_id)
    };

    let agent = agent.ok_or_else(|| format!("Agent '{agent_id}' not found in registry"))?;

    let system_prompt = build_system_prompt(agent);
    let model_id = agent.config.agent.name.clone();
    let model = agent.config.model.preferred.clone();
    drop(reg);

    info!(agent = %model_id, prompt_len = prompt.len(), "Calling Claude CLI");

    let api_key = get_api_key(home_dir).await;
    if api_key.is_empty() {
        return Err("No API key configured".to_string());
    }

    call_claude(prompt, &model, &system_prompt, &api_key).await
}

/// Public API key getter for use by other modules (e.g., sandbox dispatcher).
pub async fn get_api_key_from_home(home_dir: &Path) -> String {
    get_api_key(home_dir).await
}

/// Get the API key from env var or config.toml.
async fn get_api_key(home_dir: &Path) -> String {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }

    let config_path = home_dir.join("config.toml");
    if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
        if let Ok(table) = content.parse::<toml::Value>() {
            // Try encrypted key first
            if let Some(enc) = table
                .get("api")
                .and_then(|v| v.get("anthropic_api_key_enc"))
                .and_then(|v| v.as_str())
            {
                if !enc.is_empty() {
                    // Attempt decryption via keyfile
                    let keyfile = home_dir.join(".keyfile");
                    if let Ok(bytes) = std::fs::read(&keyfile) {
                        if bytes.len() == 32 {
                            let mut key = [0u8; 32];
                            key.copy_from_slice(&bytes);
                            if let Ok(engine) = duduclaw_security::crypto::CryptoEngine::new(&key)
                            {
                                if let Ok(plain) = engine.decrypt_string(enc) {
                                    if !plain.is_empty() {
                                        return plain;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Fallback: plaintext key
            if let Some(key) = table
                .get("api")
                .and_then(|v| v.get("anthropic_api_key"))
                .and_then(|v| v.as_str())
            {
                if !key.is_empty() {
                    return key.to_string();
                }
            }
        }
    }

    String::new()
}

/// Call the `claude` CLI binary with a prompt and return the response text.
async fn call_claude(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    api_key: &str,
) -> Result<String, String> {
    let claude = which_claude().ok_or("Claude CLI not found. Install: npm install -g @anthropic-ai/claude-code")?;

    let mut cmd = tokio::process::Command::new(&claude);
    cmd.args(["-p", prompt, "--model", model, "--output-format", "text"]);
    if !system_prompt.is_empty() {
        cmd.args(["--system-prompt", system_prompt]);
    }
    cmd.env("ANTHROPIC_API_KEY", api_key);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    match tokio::time::timeout(std::time::Duration::from_secs(120), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                Ok("(empty response)".to_string())
            } else {
                Ok(text)
            }
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(format!(
                "claude error: {}",
                stderr.chars().take(200).collect::<String>()
            ))
        }
        Ok(Err(e)) => Err(format!("spawn error: {e}")),
        Err(_) => Err("timeout after 120s".to_string()),
    }
}

/// Find the `claude` CLI binary on the system.
fn which_claude() -> Option<String> {
    if let Ok(out) = std::process::Command::new("which").arg("claude").output() {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.npm-global/bin/claude"),
        "/usr/local/bin/claude".to_string(),
        format!("{home}/.claude/bin/claude"),
        format!("{home}/.local/bin/claude"),
    ];
    for p in &candidates {
        if std::path::Path::new(p).exists() {
            return Some(p.clone());
        }
    }
    None
}
