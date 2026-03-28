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

/// Look up an agent from the registry and route to the best model.
///
/// Routing logic per agent:
/// 1. If agent has `model.local` with `prefer_local = true` and local engine is available
///    → try local inference first
/// 2. If local fails or is not configured → fall back to Claude Code SDK via AccountRotator
///
/// Local inference and account rotation are completely separate paths.
pub async fn call_claude_for_agent(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
) -> Result<String, String> {
    let reg = registry.read().await;

    let agent = if agent_id == "default" {
        reg.main_agent()
    } else {
        reg.get(agent_id)
    };

    let agent = agent.ok_or_else(|| format!("Agent '{agent_id}' not found in registry"))?;

    let system_prompt = build_system_prompt(agent);
    let agent_name = agent.config.agent.name.clone();
    let claude_model = agent.config.model.preferred.clone();
    let local_config = agent.config.model.local.clone();
    drop(reg);

    // Step 1: Try local inference if agent prefers it
    if let Some(ref local) = local_config {
        if local.prefer_local {
            info!(agent = %agent_name, local_model = %local.model, "Trying local inference for agent");
            match call_local_inference(home_dir, prompt, &system_prompt, Some(&local.model)).await {
                Ok(response) => {
                    info!(agent = %agent_name, "Agent served by local model");
                    return Ok(response);
                }
                Err(e) if e == "ROUTER_ESCALATE_TO_CLOUD" => {
                    info!(agent = %agent_name, "Router escalated to Claude API");
                }
                Err(e) => {
                    warn!(agent = %agent_name, error = %e, "Local inference failed, falling back to Claude API");
                }
            }
        }
    }

    // Step 2: Fall back to Claude Code SDK via account rotation
    info!(agent = %agent_name, model = %claude_model, prompt_len = prompt.len(), "Calling Claude CLI");
    call_with_rotation(home_dir, prompt, &claude_model, &system_prompt).await
}

/// Cached AccountRotator — avoids rebuilding on every call (BE-H4).
static ROTATOR_CACHE: std::sync::OnceLock<tokio::sync::RwLock<Option<(std::time::Instant, std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>)>>> = std::sync::OnceLock::new();

/// Cached InferenceEngine — singleton for local LLM inference.
static INFERENCE_ENGINE: std::sync::OnceLock<tokio::sync::RwLock<Option<std::sync::Arc<duduclaw_inference::InferenceEngine>>>> = std::sync::OnceLock::new();

/// Get or create the inference engine singleton.
async fn get_inference_engine(home_dir: &std::path::Path) -> Option<std::sync::Arc<duduclaw_inference::InferenceEngine>> {
    let cache = INFERENCE_ENGINE.get_or_init(|| tokio::sync::RwLock::new(None));

    {
        let guard = cache.read().await;
        if let Some(engine) = guard.as_ref() {
            return Some(engine.clone());
        }
    }

    // Initialize engine
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        warn!("Failed to initialize inference engine: {e}");
        return None;
    }
    if !engine.is_available().await {
        return None;
    }
    let arc = std::sync::Arc::new(engine);
    *cache.write().await = Some(arc.clone());
    Some(arc)
}

/// Call local inference engine instead of Claude CLI.
///
/// If the confidence router is enabled, it may decide to escalate to Cloud API
/// (returns `Err` with a special marker so the caller knows to try Cloud).
async fn call_local_inference(
    home_dir: &std::path::Path,
    prompt: &str,
    system_prompt: &str,
    model_id: Option<&str>,
) -> Result<String, String> {
    let engine = get_inference_engine(home_dir)
        .await
        .ok_or_else(|| "Local inference engine not available".to_string())?;

    let request = duduclaw_inference::InferenceRequest {
        system_prompt: system_prompt.to_string(),
        user_prompt: prompt.to_string(),
        params: engine.config().generation.clone(),
        model_id: model_id.map(|s| s.to_string()),
    };

    // Use router if enabled — may escalate to Cloud API
    if engine.router_enabled() {
        match engine.route_and_generate(&request).await {
            Ok(Some(response)) => {
                info!(
                    model = %response.model_id,
                    tokens = response.tokens_generated,
                    tps = format!("{:.1}", response.tokens_per_second),
                    ms = response.generation_time_ms,
                    "Local inference completed (routed)"
                );
                return Ok(response.text);
            }
            Ok(None) => {
                // Router decided Cloud API is needed
                return Err("ROUTER_ESCALATE_TO_CLOUD".to_string());
            }
            Err(e) => {
                warn!(error = %e, "Routed local inference failed");
                return Err(format!("Local inference error: {e}"));
            }
        }
    }

    // No router — direct generation
    let response = engine
        .generate(&request)
        .await
        .map_err(|e| format!("Local inference error: {e}"))?;

    info!(
        model = %response.model_id,
        tokens = response.tokens_generated,
        tps = format!("{:.1}", response.tokens_per_second),
        ms = response.generation_time_ms,
        "Local inference completed"
    );

    Ok(response.text)
}

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

/// Default idle timeout in seconds — resets every time new data arrives.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 120;

/// Spawn a `claude` CLI process with streaming output and read the result.
///
/// Uses `--output-format stream-json --verbose` and an idle timeout that resets
/// on every received line. This means long-running responses (tool use, multi-turn)
/// will never be killed as long as the CLI keeps producing events.
async fn call_claude_streaming(
    cmd: &mut tokio::process::Command,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("claude CLI spawn error: {e}"))?;
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    let idle_timeout = std::time::Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS);
    let mut result_text = String::new();

    loop {
        match tokio::time::timeout(idle_timeout, reader.next_line()).await {
            Err(_) => {
                let _ = child.kill().await;
                if result_text.is_empty() {
                    return Err(format!("claude CLI idle timeout ({DEFAULT_IDLE_TIMEOUT_SECS}s, no output)"));
                }
                warn!("claude CLI idle timeout — returning partial result ({} chars)", result_text.len());
                break;
            }
            Ok(Ok(None)) => break, // stream ended
            Ok(Err(e)) => {
                let _ = child.kill().await;
                return Err(format!("claude CLI read error: {e}"));
            }
            Ok(Ok(Some(line))) => {
                if line.trim().is_empty() { continue; }
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                    match event.get("type").and_then(|t| t.as_str()) {
                        Some("result") => {
                            if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                                result_text = text.to_string();
                            }
                        }
                        Some("assistant") => {
                            if let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array()) {
                                for block in content {
                                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                            result_text = text.to_string();
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let status = child.wait().await.map_err(|e| format!("wait error: {e}"))?;
    if !status.success() && result_text.is_empty() {
        return Err(format!("claude CLI exit {}", status.code().unwrap_or(-1)));
    }

    let result_text = result_text.trim().to_string();
    if result_text.is_empty() {
        return Err("Empty response from claude CLI".to_string());
    }

    Ok(result_text)
}

/// Prepare a `claude` CLI command with common args and env vars.
fn prepare_claude_cmd(
    claude_path: &str,
    prompt: &str,
    model: &str,
    system_prompt: &str,
) -> (tokio::process::Command, Option<tempfile::TempPath>) {
    let mut cmd = tokio::process::Command::new(claude_path);
    cmd.args([
        "-p", prompt,
        "--model", model,
        "--output-format", "stream-json",
        "--verbose",
    ]);

    let prompt_guard = if !system_prompt.is_empty() {
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

    // Prevent "nested session" error when gateway was launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");

    (cmd, prompt_guard)
}

/// Call claude CLI with custom env vars (supports both OAuth and API key).
async fn call_claude_with_env(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    env_vars: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let claude = which_claude().ok_or("Claude CLI not found")?;
    let (mut cmd, _prompt_guard) = prepare_claude_cmd(&claude, prompt, model, system_prompt);

    for (key, value) in env_vars {
        if value.is_empty() {
            cmd.env_remove(key);
        } else {
            cmd.env(key, value);
        }
    }

    call_claude_streaming(&mut cmd).await
}

/// Call the `claude` CLI binary with a prompt and return the response text.
async fn call_claude(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    api_key: &str,
) -> Result<String, String> {
    let claude = which_claude().ok_or("Claude CLI not found. Install: npm install -g @anthropic-ai/claude-code")?;
    let (mut cmd, _prompt_guard) = prepare_claude_cmd(&claude, prompt, model, system_prompt);
    cmd.env("ANTHROPIC_API_KEY", api_key);

    call_claude_streaming(&mut cmd).await
}

/// Find the `claude` CLI binary — delegates to shared impl in duduclaw-core (BE-L1).
fn which_claude() -> Option<String> {
    duduclaw_core::which_claude()
}
