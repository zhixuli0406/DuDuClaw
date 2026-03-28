//! Shared helper for calling the Claude CLI (Claude Code SDK) on behalf of an agent.
//!
//! Used by both the cron scheduler and the agent dispatcher.

use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Build a system prompt from an agent's loaded markdown files.
///
/// Skills are sorted alphabetically by name to ensure deterministic byte
/// sequences across calls — this maximizes prompt cache hit rates.
fn build_system_prompt(agent: &duduclaw_agent::LoadedAgent) -> String {
    let mut parts = Vec::new();

    if let Some(soul) = &agent.soul {
        parts.push(format!("# Soul\n{}", soul.trim_end()));
    }
    if let Some(identity) = &agent.identity {
        parts.push(format!("# Identity\n{}", identity.trim_end()));
    }

    // Sort skills by name for deterministic ordering (cache-friendly)
    let mut skills: Vec<_> = agent.skills.iter().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    for skill in skills {
        parts.push(format!("# Skill: {}\n{}", skill.name, skill.content.trim_end()));
    }

    if let Some(memory) = &agent.memory {
        parts.push(format!("# Memory\n{}", memory.trim_end()));
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
    call_claude_for_agent_with_type(
        home_dir, registry, agent_id, prompt,
        crate::cost_telemetry::RequestType::Chat,
    ).await
}

/// Like [`call_claude_for_agent`] but allows specifying the request type for telemetry.
pub async fn call_claude_for_agent_with_type(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
    request_type: crate::cost_telemetry::RequestType,
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
    let api_mode = agent.config.model.api_mode.clone();
    drop(reg);

    // P0 fix: global mode gate BEFORE per-agent routing
    let inference_mode = get_inference_mode(home_dir).await;
    match inference_mode.as_str() {
        "local" => {
            // Force local inference regardless of per-agent prefer_local
            let model_id = local_config.as_ref().map(|c| c.model.as_str());
            return call_local_inference(home_dir, prompt, &system_prompt, model_id)
                .await
                .map_err(|e| format!(
                    "Agent '{agent_name}' is in local-only mode but inference failed: {e}. \
                     Fix local model setup or switch to 'hybrid' mode in config.toml."
                ));
        }
        "claude" => {
            // Skip local entirely, go straight to Claude API
            info!(agent = %agent_name, model = %claude_model, "Claude-only mode");
            return call_with_rotation(
                home_dir, agent_id, prompt, &claude_model, &system_prompt,
                request_type,
            ).await;
        }
        _ => {
            // "hybrid" — SDK-first design (see routing logic below)
        }
    }

    // ══════════════════════════════════════════════════════════════
    // Hybrid mode routing — SDK is the brain, local is cost-saving offload
    //
    // Design principle: "Claude Code SDK = brain, DuDuClaw = plumbing"
    // OAuth subscription is the primary fuel, API Key is the reserve tank.
    //
    //  ① Local offload: Router-confirmed simple queries → zero cost
    //  ② CLI (claude -p): primary brain, uses OAuth subscription
    //     - Multiple OAuth accounts rotated via CLAUDE_CODE_OAUTH_TOKEN
    //  ③ Direct API (API Key): fallback when all OAuth accounts rate-limited
    //     - cache_control for 95%+ cache hit rate
    // ══════════════════════════════════════════════════════════════

    // Validate api_mode
    if !matches!(api_mode.as_str(), "cli" | "direct" | "auto") {
        warn!(
            agent = %agent_name,
            api_mode = %api_mode,
            "Unrecognized api_mode in agent.toml — expected cli/direct/auto, defaulting to cli"
        );
    }

    // ── ① Local offload: only for clearly simple queries ─────────
    let adaptive_prefer = crate::cost_telemetry::should_prefer_local(agent_id).await;
    if let Some(ref local) = local_config {
        let should_try_local = adaptive_prefer || local.use_router || local.prefer_local;
        if should_try_local {
            let reason = if adaptive_prefer { "adaptive-override" }
                else if local.use_router { "router-driven" }
                else { "prefer-local" };
            info!(agent = %agent_name, local_model = %local.model, reason, "Trying local offload");
            match call_local_inference(home_dir, prompt, &system_prompt, Some(&local.model)).await {
                Ok(response) => {
                    info!(agent = %agent_name, "Query served by local model (cost saved)");
                    return Ok(response);
                }
                Err(e) if e == "ROUTER_ESCALATE_TO_CLOUD" => {
                    info!(agent = %agent_name, "Router: query too complex → escalating to SDK");
                }
                Err(e) => {
                    warn!(agent = %agent_name, error = %e, "Local offload failed → escalating to SDK");
                }
            }
        }
    }

    // ── ② CLI: primary brain (OAuth subscription) ────────────────
    // In "auto" mode: try CLI first. Only fall through to Direct API
    // if CLI fails with rate limit (all OAuth accounts exhausted).
    // In "cli" mode: CLI is the only cloud path.
    // In "direct" mode: skip CLI, go straight to Direct API.
    if api_mode != "direct" {
        info!(agent = %agent_name, model = %claude_model, "Calling Claude CLI (SDK primary)");
        match call_with_rotation(
            home_dir, agent_id, prompt, &claude_model, &system_prompt, request_type,
        ).await {
            Ok(text) => return Ok(text),
            Err(e) => {
                let is_rate = is_rate_limit_error(&e);
                if api_mode == "auto" && is_rate {
                    // All OAuth accounts rate-limited → fall through to Direct API
                    warn!(agent = %agent_name, "All CLI accounts rate-limited → trying Direct API fallback");
                } else {
                    // "cli" mode or non-rate error → report error
                    return Err(e);
                }
            }
        }
    }

    // ── ③ Direct API: fallback with API Key (cache-optimized) ────
    // Only reached when: api_mode="direct", or api_mode="auto" + all OAuth rate-limited
    info!(agent = %agent_name, model = %claude_model, "Trying Direct API (API Key fallback)");
    match try_direct_api(home_dir, agent_id, prompt, &claude_model, &system_prompt, request_type).await {
        Ok(text) => Ok(text),
        Err(e) => Err(e),
    }
}

/// Check whether an error string indicates a billing/credit exhaustion issue.
///
/// These errors should NOT be retried with the same account — the account
/// needs a long cooldown (topped up manually).
fn is_billing_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("credit")
        || lower.contains("balance")
        || lower.contains("billing")
        || lower.contains("payment")
        || lower.contains("402")
        || lower.contains("insufficient_quota")
}

/// Check whether an error indicates rate limiting (usage limit exhausted).
fn is_rate_limit_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("rate") || lower.contains("429") || lower.contains("usage limit")
        || lower.contains("overloaded") || lower.contains("capacity")
}

/// Try calling the Anthropic Messages API directly (bypassing Claude CLI).
///
/// Only works with API key accounts (not OAuth). If no API key is available,
/// returns an error so the caller can fall back to CLI.
async fn try_direct_api(
    home_dir: &Path,
    agent_id: &str,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    request_type: crate::cost_telemetry::RequestType,
) -> Result<String, String> {
    let api_key = get_api_key(home_dir).await;
    if api_key.is_empty() {
        return Err("No API key available for Direct API (OAuth accounts require CLI path)".to_string());
    }

    let response = crate::direct_api::call_direct_api(&api_key, model, system_prompt, prompt).await?;

    // Record telemetry
    if let Some(ref usage) = response.usage {
        if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
            telemetry.record(agent_id, request_type, model, usage).await;
        }
    }

    Ok(response.text)
}

/// Cached inference_mode — avoids reading config.toml on every call (P1-3).
static INFERENCE_MODE_CACHE: std::sync::OnceLock<tokio::sync::RwLock<Option<(std::time::Instant, String)>>> = std::sync::OnceLock::new();

async fn get_inference_mode(home_dir: &Path) -> String {
    let cache = INFERENCE_MODE_CACHE.get_or_init(|| tokio::sync::RwLock::new(None));
    let ttl = std::time::Duration::from_secs(300); // 5 min

    {
        let guard = cache.read().await;
        if let Some((created, mode)) = guard.as_ref() {
            if created.elapsed() < ttl {
                return mode.clone();
            }
        }
    }

    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.unwrap_or_default();
    let table: toml::Table = content.parse().unwrap_or_default();
    let mode = table.get("general")
        .and_then(|g| g.get("inference_mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("claude")
        .to_string();

    *cache.write().await = Some((std::time::Instant::now(), mode.clone()));
    mode
}

/// Cached AccountRotator — avoids rebuilding on every call (BE-H4).
static ROTATOR_CACHE: std::sync::OnceLock<tokio::sync::RwLock<Option<(std::time::Instant, std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>)>>> = std::sync::OnceLock::new();

/// Cached InferenceEngine — singleton for local LLM inference.
static INFERENCE_ENGINE: std::sync::OnceLock<tokio::sync::RwLock<Option<std::sync::Arc<duduclaw_inference::InferenceEngine>>>> = std::sync::OnceLock::new();

/// Mutex protecting the one-time initialization of the inference engine.
/// Prevents concurrent tasks from each loading a full GGUF model (OOM risk).
static INFERENCE_INIT_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// Get or create the inference engine singleton.
async fn get_inference_engine(home_dir: &std::path::Path) -> Option<std::sync::Arc<duduclaw_inference::InferenceEngine>> {
    let cache = INFERENCE_ENGINE.get_or_init(|| tokio::sync::RwLock::new(None));

    // Fast path: engine already initialized
    {
        let guard = cache.read().await;
        if let Some(engine) = guard.as_ref() {
            return Some(engine.clone());
        }
    }

    // Slow path: serialize initialization to prevent concurrent model loading
    let init_lock = INFERENCE_INIT_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _init_guard = init_lock.lock().await;

    // Double-check after acquiring lock (another task may have initialized)
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
///
/// Records token usage telemetry when available.
async fn call_with_rotation(
    home_dir: &Path,
    agent_id: &str,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    request_type: crate::cost_telemetry::RequestType,
) -> Result<String, String> {
    // Pre-flight: check 200K price cliff
    if let Some(estimated) = crate::cost_telemetry::check_price_cliff(system_prompt, prompt) {
        warn!(
            agent_id,
            estimated_tokens = estimated,
            "WARNING: Estimated input tokens near 200K price cliff — pricing will double"
        );
    }

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
                // Use telemetry-based cost if usage available, else rough estimate
                let cost = if let Some(ref usage) = response.usage {
                    if selected.auth_method == duduclaw_agent::account_rotator::AuthMethod::OAuth {
                        0
                    } else {
                        usage.estimated_cost_millicents() / 10 // convert millicents to ~cents
                    }
                } else if selected.auth_method == duduclaw_agent::account_rotator::AuthMethod::OAuth {
                    0
                } else {
                    ((prompt.len() + response.text.len()) / 1000).max(1) as u64
                };
                rotator.on_success(&selected.id, cost).await;

                // Record telemetry
                if let Some(ref usage) = response.usage {
                    if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                        telemetry.record(agent_id, request_type, model, usage).await;
                    }
                }

                return Ok(response.text);
            }
            Err(e) => {
                last_error = e.clone();
                if is_billing_error(&e) {
                    // Billing/credit exhaustion: long cooldown (24h), mark unhealthy immediately
                    warn!(account = %selected.id, error = %e, "Account billing exhausted — 24h cooldown");
                    rotator.on_billing_exhausted(&selected.id).await;
                } else if e.contains("rate") || e.contains("429") {
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
        let response = call_claude(prompt, model, system_prompt, &api_key).await?;

        // Record telemetry for fallback path too
        if let Some(ref usage) = response.usage {
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                telemetry.record(agent_id, request_type, model, usage).await;
            }
        }

        return Ok(response.text);
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

/// Response from a Claude CLI call, including optional token usage telemetry.
struct ClaudeResponse {
    text: String,
    usage: Option<crate::cost_telemetry::TokenUsage>,
}

/// Spawn a `claude` CLI process with streaming output and read the result.
///
/// Uses `--output-format stream-json --verbose` and an idle timeout that resets
/// on every received line. This means long-running responses (tool use, multi-turn)
/// will never be killed as long as the CLI keeps producing events.
///
/// Extracts `TokenUsage` from the `result` event's `usage` field when available.
async fn call_claude_streaming(
    cmd: &mut tokio::process::Command,
) -> Result<ClaudeResponse, String> {
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
    let mut token_usage: Option<crate::cost_telemetry::TokenUsage> = None;

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
                            // Extract token usage from result event
                            if let Some(usage_val) = event.get("usage") {
                                token_usage = crate::cost_telemetry::TokenUsage::from_json(usage_val);
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
                            // Also check usage in assistant message events
                            if token_usage.is_none() {
                                if let Some(usage_val) = event.pointer("/message/usage") {
                                    token_usage = crate::cost_telemetry::TokenUsage::from_json(usage_val);
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

    Ok(ClaudeResponse { text: result_text, usage: token_usage })
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
) -> Result<ClaudeResponse, String> {
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
) -> Result<ClaudeResponse, String> {
    let claude = which_claude().ok_or("Claude CLI not found. Install: npm install -g @anthropic-ai/claude-code")?;
    let (mut cmd, _prompt_guard) = prepare_claude_cmd(&claude, prompt, model, system_prompt);
    cmd.env("ANTHROPIC_API_KEY", api_key);

    call_claude_streaming(&mut cmd).await
}

/// Find the `claude` CLI binary — delegates to shared impl in duduclaw-core (BE-L1).
fn which_claude() -> Option<String> {
    duduclaw_core::which_claude()
}
