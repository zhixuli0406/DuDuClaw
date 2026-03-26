//! Shared helper for calling the Claude CLI (Claude Code SDK) on behalf of an agent.
//!
//! Used by both the cron scheduler and the agent dispatcher.

use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use tokio::sync::RwLock;
use tracing::{info, warn};

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

    // Use account rotator for key selection (with retry on failure)
    call_with_rotation(home_dir, prompt, &model, &system_prompt).await
}

/// Cached AccountRotator — avoids rebuilding on every call (BE-H4).
static ROTATOR_CACHE: std::sync::OnceLock<tokio::sync::RwLock<Option<(std::time::Instant, std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>)>>> = std::sync::OnceLock::new();

/// Get or create a cached AccountRotator (refreshes every 5 minutes).
/// Public accessor for the cached rotator — used by handlers.rs too.
pub async fn get_rotator_cached(home_dir: &Path) -> Result<std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>, String> {
    get_rotator(home_dir).await
}

async fn get_rotator(home_dir: &Path) -> Result<std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>, String> {
    let cache = ROTATOR_CACHE.get_or_init(|| tokio::sync::RwLock::new(None));
    let ttl = std::time::Duration::from_secs(300); // 5 min cache

    // Check if cached version is still valid
    {
        let guard = cache.read().await;
        if let Some((created, rotator)) = guard.as_ref() {
            if created.elapsed() < ttl {
                return Ok(rotator.clone());
            }
        }
    }

    // Rebuild
    let config_content = tokio::fs::read_to_string(home_dir.join("config.toml"))
        .await
        .unwrap_or_default();
    let config_table: toml::Table = config_content.parse().unwrap_or_default();
    let rotator = duduclaw_agent::account_rotator::create_from_config(&config_table);
    rotator.load_from_config(home_dir).await?;
    let arc = std::sync::Arc::new(rotator);
    *cache.write().await = Some((std::time::Instant::now(), arc.clone()));
    Ok(arc)
}

/// Call Claude CLI with account rotation — tries next account on failure.
async fn call_with_rotation(
    home_dir: &Path,
    prompt: &str,
    model: &str,
    system_prompt: &str,
) -> Result<String, String> {
    let rotator = get_rotator(home_dir).await?;

    let max_attempts = rotator.count().await.max(1);
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let selected = match rotator.select().await {
            Some(s) => s,
            None => break,
        };

        info!(account = %selected.id, method = ?selected.auth_method, attempt, "Trying account");

        match call_claude_with_env(prompt, model, system_prompt, &selected.env_vars).await {
            Ok(response) => {
                // OAuth accounts: no per-token cost. API key: rough estimate
                let cost = if selected.auth_method == duduclaw_agent::account_rotator::AuthMethod::OAuth {
                    0
                } else {
                    ((prompt.len() + response.len()) / 1000).max(1) as u64
                };
                rotator.on_success(&selected.id, cost).await;
                return Ok(response);
            }
            Err(e) => {
                last_error = e.clone();
                if e.contains("rate") || e.contains("429") {
                    rotator.on_rate_limited(&selected.id).await;
                } else {
                    rotator.on_error(&selected.id).await;
                }
                warn!(account = %selected.id, error = %e, "Account failed, trying next");
            }
        }
    }

    // All accounts failed — fall back to direct key
    let api_key = get_api_key(home_dir).await;
    if !api_key.is_empty() {
        warn!("All rotated accounts failed, using fallback key");
        return call_claude(prompt, model, system_prompt, &api_key).await;
    }

    Err(format!("All accounts exhausted. Last error: {last_error}"))
}

/// Public API key getter for use by other modules (e.g., sandbox dispatcher).
pub async fn get_api_key_from_home(home_dir: &Path) -> String {
    get_api_key(home_dir).await
}

/// Get the API key from env var or config.toml.
async fn get_api_key(home_dir: &Path) -> String {
    // Environment variable takes precedence
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    // Use shared encrypted config reader (tries _enc first, falls back to plaintext)
    crate::config_crypto::read_encrypted_config_field(home_dir, "api", "anthropic_api_key")
        .await
        .unwrap_or_default()
}

/// Call claude CLI with custom env vars (supports both OAuth and API key).
async fn call_claude_with_env(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    env_vars: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let claude = which_claude().ok_or("Claude CLI not found")?;
    let mut cmd = tokio::process::Command::new(&claude);
    cmd.args(["-p", prompt, "--model", model, "--output-format", "text"]);

    if !system_prompt.is_empty() {
        match tempfile::NamedTempFile::new() {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(system_prompt.as_bytes());
                let path = f.into_temp_path();
                cmd.args(["--system-prompt-file", &path.to_string_lossy()]);
                // path kept alive until cmd completes
            }
            Err(_) => {
                cmd.args(["--system-prompt", system_prompt]);
            }
        }
    }

    // Prevent "nested session" error when gateway was launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");

    // Set env vars from the selected account
    for (key, value) in env_vars {
        if value.is_empty() {
            cmd.env_remove(key);
        } else {
            cmd.env(key, value);
        }
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    match tokio::time::timeout(std::time::Duration::from_secs(120), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() { Ok("(empty response)".to_string()) } else { Ok(text) }
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(format!("claude error: {}", stderr.chars().take(200).collect::<String>()))
        }
        Ok(Err(e)) => Err(format!("spawn error: {e}")),
        Err(_) => Err("timeout after 120s".to_string()),
    }
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

    // Pass system prompt via temp file to avoid /proc exposure (BE-C1)
    let _prompt_guard: Option<tempfile::TempPath> = if !system_prompt.is_empty() {
        match tempfile::NamedTempFile::new() {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(system_prompt.as_bytes());
                let path = f.into_temp_path();
                cmd.args(["--system-prompt-file", &path.to_string_lossy()]);
                Some(path)
            }
            Err(_) => {
                cmd.args(["--system-prompt", system_prompt]);
                None
            }
        }
    } else {
        None
    };

    cmd.env("ANTHROPIC_API_KEY", api_key);
    // Prevent "nested session" error when gateway was launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");
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

/// Find the `claude` CLI binary — delegates to shared impl in duduclaw-core (BE-L1).
fn which_claude() -> Option<String> {
    duduclaw_core::which_claude()
}
