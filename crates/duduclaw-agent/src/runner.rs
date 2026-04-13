//! CLI interactive session runner for DuDuClaw agents.
//!
//! Provides `duduclaw chat` / `duduclaw agent run <name>` with:
//! - AccountRotator for multi-account OAuth + API Key rotation
//! - Streaming JSON output with real-time terminal progress
//! - Capabilities enforcement (deny-by-default high-risk tools)
//! - System prompt via tempfile (avoids OS arg-length limits)
//! - Deterministic skill ordering (cache-friendly)

use std::path::PathBuf;

use chrono::Utc;
use duduclaw_core::error::{DuDuClawError, Result};
use tracing::{info, warn};

use crate::account_rotator::{AccountRotator, AuthMethod};
use crate::prompt_snapshot::{PromptModule, SystemPromptSnapshot, SHARED_BASE};
use crate::registry::{AgentRegistry, LoadedAgent};

/// Hard max timeout — absolute safety net to kill hung processes.
const HARD_MAX_TIMEOUT_SECS: u64 = 30 * 60; // 30 minutes

/// Response from a Claude CLI call, including estimated cost.
struct CliResponse {
    text: String,
    /// Rough cost estimate in cents (0 for OAuth).
    cost_cents: u64,
}

/// Runs an agent in CLI interactive mode.
pub struct AgentRunner {
    home_dir: PathBuf,
    agents_dir: PathBuf,
    registry: AgentRegistry,
}

impl AgentRunner {
    /// Create a new runner, scanning the agents directory under `home_dir`.
    pub async fn new(home_dir: PathBuf) -> Result<Self> {
        let agents_dir = home_dir.join("agents");
        let mut registry = AgentRegistry::new(agents_dir.clone());
        registry.scan().await?;
        Ok(Self {
            home_dir,
            agents_dir,
            registry,
        })
    }

    /// Run an interactive CLI session with the specified agent (or the main agent).
    pub async fn run_interactive(&self, agent_name: Option<&str>) -> Result<()> {
        let agent = match agent_name {
            Some(name) => self
                .registry
                .get(name)
                .ok_or_else(|| DuDuClawError::Agent(format!("Agent '{}' not found", name)))?,
            None => self
                .registry
                .main_agent()
                .ok_or_else(|| DuDuClawError::Agent("No main agent configured".into()))?,
        };

        let display_name = agent.config.agent.display_name.clone();
        let model_id = agent.config.model.preferred.clone();
        let capabilities = agent.config.capabilities.clone();

        info!(
            agent = %agent.config.agent.name,
            display_name = %display_name,
            model = %model_id,
            "Starting interactive session"
        );

        // Build system prompt snapshot — frozen for the entire session to
        // maximize prompt cache hits (Anthropic API + local inference).
        let snapshot = build_system_prompt(agent);
        info!(
            content_hash = %snapshot.content_hash,
            modules = snapshot.module_order.len(),
            bytes = snapshot.frozen_prompt.len(),
            "System prompt snapshot frozen"
        );

        // Initialize AccountRotator for multi-account support
        let rotator = self.init_rotator().await;

        // Display session banner
        print_banner(&display_name, &model_id, &rotator).await;

        // Interactive loop
        let stdin = tokio::io::stdin();
        let reader = tokio::io::BufReader::new(stdin);
        use tokio::io::AsyncBufReadExt;
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let input = line.trim();
            if input.is_empty() {
                continue;
            }

            // Special commands
            if input == "/quit" || input == "/exit" {
                break;
            }
            if input == "/accounts" {
                print_account_status(&rotator).await;
                continue;
            }

            eprintln!();

            let response = call_with_streaming(
                snapshot.prompt(),
                input,
                &model_id,
                &rotator,
                &capabilities,
            )
            .await;

            match response {
                Ok(text) => {
                    // Clear the progress line and print response
                    eprint!("\r\x1b[K");
                    println!("[{}]: {}", display_name, text);
                }
                Err(e) => {
                    eprint!("\r\x1b[K");
                    eprintln!("[error]: {e}");
                }
            }
            println!("---");
        }

        println!("\nSession ended. Goodbye!");
        Ok(())
    }

    /// Initialize AccountRotator from config.toml.
    async fn init_rotator(&self) -> AccountRotator {
        let config_content = tokio::fs::read_to_string(self.home_dir.join("config.toml"))
            .await
            .unwrap_or_default();
        let config_table: toml::Table = config_content.parse().unwrap_or_default();
        let rotator = crate::account_rotator::create_from_config(&config_table);
        if let Err(e) = rotator.load_from_config(&self.home_dir).await {
            warn!(error = %e, "Failed to load accounts — CLI may not have auth");
        }
        rotator
    }

    /// Return a list of all loaded agents.
    pub fn list_agents(&self) -> Vec<&LoadedAgent> {
        self.registry.list()
    }

    /// Return the agents directory path.
    pub fn agents_dir(&self) -> &PathBuf {
        &self.agents_dir
    }
}

// ── System prompt builder ───────────────────────────────────

/// Build a system prompt snapshot from an agent's loaded markdown files.
///
/// The prompt is assembled with `<!-- MODULE: name -->` delimiters in a strict
/// order to ensure byte-identical prefixes across turns (prompt cache friendly).
/// Skills are sorted alphabetically by name for deterministic ordering.
///
/// Module order:
///   SHARED_BASE → IDENTITY (SOUL.md + IDENTITY.md) → SKILLS → MEMORY → DYNAMIC
fn build_system_prompt(agent: &LoadedAgent) -> SystemPromptSnapshot {
    let mut buf = String::new();
    let mut modules = Vec::new();

    // Helper: append a module with delimiter tracking.
    macro_rules! append_module {
        ($name:expr, $content:expr) => {{
            let offset = buf.len();
            buf.push_str(&format!("<!-- MODULE: {} -->\n", $name));
            buf.push_str($content);
            buf.push_str("\n\n");
            modules.push(PromptModule {
                name: $name.to_string(),
                byte_offset: offset,
                byte_length: buf.len() - offset,
            });
        }};
    }

    // 1. Shared base — identical across all agents for prefix cache sharing.
    append_module!("SHARED_BASE", SHARED_BASE);

    // 2. Identity — SOUL.md + IDENTITY.md
    {
        let mut identity_parts = String::new();
        if let Some(soul) = &agent.soul {
            identity_parts.push_str("# Soul\n");
            identity_parts.push_str(soul.trim_end());
        }
        if let Some(identity) = &agent.identity {
            if !identity_parts.is_empty() {
                identity_parts.push_str("\n\n---\n\n");
            }
            identity_parts.push_str("# Identity\n");
            identity_parts.push_str(identity.trim_end());
        }
        if !identity_parts.is_empty() {
            append_module!("IDENTITY", &identity_parts);
        }
    }

    // 3. Skills — alphabetically sorted for deterministic byte sequences.
    {
        let mut skills: Vec<_> = agent.skills.iter().collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        if !skills.is_empty() {
            let mut skills_text = String::new();
            for (i, skill) in skills.iter().enumerate() {
                if i > 0 {
                    skills_text.push_str("\n\n---\n\n");
                }
                skills_text.push_str(&format!("# Skill: {}\n{}", skill.name, skill.content.trim_end()));
            }
            append_module!("SKILLS", &skills_text);
        }
    }

    // 4. Memory context
    if let Some(memory) = &agent.memory {
        append_module!("MEMORY", memory.trim_end());
    }

    // 5. Dynamic — timestamp and session-specific metadata.
    //    This section is intentionally last so the frozen prefix above
    //    stays byte-identical even if dynamic content varies.
    {
        let dynamic = format!("Session started: {}", Utc::now().to_rfc3339());
        append_module!("DYNAMIC", &dynamic);
    }

    SystemPromptSnapshot::new(buf, modules)
}

// ── CLI banner ──────────────────────────────────────────────

async fn print_banner(display_name: &str, model: &str, rotator: &AccountRotator) {
    let statuses = rotator.status().await;
    let oauth_count = statuses.iter().filter(|s| s.auth_method == "oauth").count();
    let apikey_count = statuses
        .iter()
        .filter(|s| s.auth_method == "apikey")
        .count();
    let available = statuses.iter().filter(|s| s.is_available).count();

    println!("DuDuClaw: {} is ready!", display_name);
    println!("  Model: {model}");
    println!(
        "  Accounts: {available} available ({oauth_count} OAuth + {apikey_count} API Key)"
    );

    if available == 0 {
        println!(
            "  \x1b[33m[warn]\x1b[0m No accounts available — run `duduclaw onboard` or set ANTHROPIC_API_KEY"
        );
    }

    // Show expiry warnings
    for s in &statuses {
        if let Some(days) = s.days_until_expiry {
            if days <= 0 {
                println!(
                    "  \x1b[31m[error]\x1b[0m Account '{}' token EXPIRED — run `claude setup-token` to renew",
                    if s.label.is_empty() { &s.id } else { &s.label }
                );
            } else if days <= 7 {
                println!(
                    "  \x1b[33m[warn]\x1b[0m Account '{}' token expires in {} days",
                    if s.label.is_empty() { &s.id } else { &s.label },
                    days
                );
            }
        }
    }

    println!("  Commands: /quit /accounts");
    println!("---");
}

async fn print_account_status(rotator: &AccountRotator) {
    let statuses = rotator.status().await;
    if statuses.is_empty() {
        println!("  No accounts configured.");
        return;
    }
    for s in &statuses {
        let health = if s.is_available {
            "\x1b[32m●\x1b[0m"
        } else {
            "\x1b[31m●\x1b[0m"
        };
        let label = if s.label.is_empty() {
            &s.id
        } else {
            &s.label
        };
        let method = if s.auth_method == "oauth" {
            "OAuth"
        } else {
            "API Key"
        };
        let email_part = if s.email.is_empty() {
            String::new()
        } else {
            format!(" ({})", s.email)
        };
        println!(
            "  {health} {label} [{method}]{email_part} — {} requests",
            s.total_requests
        );
    }
    println!("---");
}

// ── Claude CLI streaming caller ─────────────────────────────

/// Call Claude CLI with AccountRotator, streaming progress to terminal.
async fn call_with_streaming(
    system_prompt: &str,
    prompt: &str,
    model: &str,
    rotator: &AccountRotator,
    capabilities: &duduclaw_core::types::CapabilitiesConfig,
) -> std::result::Result<String, String> {
    let max_attempts = rotator.count().await.max(1);
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let selected = match rotator.select().await {
            Some(s) => s,
            None => {
                if attempt == 0 {
                    last_error = "No accounts available. Run `duduclaw onboard` or set ANTHROPIC_API_KEY".to_string();
                }
                break;
            }
        };

        let method_label = match selected.auth_method {
            AuthMethod::OAuth => "OAuth",
            AuthMethod::ApiKey => "API Key",
        };
        if attempt > 0 {
            eprint!("\r\x1b[K");
            eprintln!(
                "  \x1b[33m[retry]\x1b[0m Trying account '{}' ({})…",
                selected.id, method_label
            );
        }

        let is_oauth = selected.auth_method == AuthMethod::OAuth;

        match call_claude_streaming(system_prompt, prompt, model, &selected.env_vars, capabilities)
            .await
        {
            Ok(resp) => {
                // OAuth is subscription-based (no per-token cost)
                let cost = if is_oauth { 0 } else { resp.cost_cents };
                rotator.on_success(&selected.id, cost).await;
                return Ok(resp.text);
            }
            Err(e) => {
                last_error = e.clone();
                if is_billing_error(&e) {
                    rotator.on_billing_exhausted(&selected.id).await;
                } else if is_rate_limit_error(&e) {
                    rotator.on_rate_limited(&selected.id).await;
                } else {
                    rotator.on_error(&selected.id).await;
                }
                warn!(account = %selected.id, error = %e, attempt, "Account failed");
            }
        }
    }

    Err(format!(
        "All accounts exhausted. Last error: {last_error}"
    ))
}

/// Spawn `claude` CLI with `--output-format stream-json` and display progress.
async fn call_claude_streaming(
    system_prompt: &str,
    prompt: &str,
    model: &str,
    env_vars: &std::collections::HashMap<String, String>,
    capabilities: &duduclaw_core::types::CapabilitiesConfig,
) -> std::result::Result<CliResponse, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let claude = duduclaw_core::which_claude()
        .ok_or("Claude CLI not found. Install: npm install -g @anthropic-ai/claude-code")?;

    let mut cmd = duduclaw_core::platform::async_command_for(&claude);
    cmd.args([
        "-p",
        prompt,
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "auto",
        "--max-turns",
        "50",
    ]);

    // Apply tool restrictions (deny-by-default)
    let denied = capabilities.disallowed_tools();
    if !denied.is_empty() {
        cmd.args(["--disallowedTools", &denied.join(",")]);
    }

    // Signal bash-gate.sh to allow browser automation commands
    if capabilities.browser_via_bash {
        cmd.env("DUDUCLAW_BROWSER_VIA_BASH", "1");
    }

    // System prompt via tempfile (avoids OS arg-length limits)
    let _prompt_guard = if !system_prompt.is_empty() {
        match tempfile::NamedTempFile::new() {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = f.write_all(system_prompt.as_bytes()) {
                    warn!(error = %e, "Failed to write system prompt tempfile, using arg fallback");
                    cmd.args(["--system-prompt", system_prompt]);
                    None
                } else {
                    let path = f.into_temp_path();
                    cmd.args(["--system-prompt-file", &path.to_string_lossy()]);
                    Some(path)
                }
            }
            Err(_) => {
                cmd.args(["--system-prompt", system_prompt]);
                None
            }
        }
    } else {
        None
    };

    // Apply account env vars
    for (key, value) in env_vars {
        if value.is_empty() {
            cmd.env_remove(key);
        } else {
            cmd.env(key, value);
        }
    }

    // Prevent "nested session" error when launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("claude CLI spawn error: {e}"))?;
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    // Drain stderr asynchronously to prevent pipe buffer deadlock.
    // Without this, if claude CLI writes >64KB to stderr (common in verbose
    // mode), the pipe fills up and the child blocks forever.
    // Uses line-by-line reading to avoid unbounded memory growth.
    let stderr = child.stderr.take();
    tokio::spawn(async move {
        if let Some(e) = stderr {
            let mut lines = BufReader::new(e).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        }
    });

    let mut result_text = String::new();
    let mut last_tool: Option<String> = None;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;

    let hard_deadline =
        tokio::time::sleep(std::time::Duration::from_secs(HARD_MAX_TIMEOUT_SECS));
    tokio::pin!(hard_deadline);

    loop {
        tokio::select! {
            line_result = reader.next_line() => {
                match line_result {
                    Ok(None) => break,
                    Err(e) => {
                        let _ = child.kill().await;
                        return Err(format!("claude CLI read error: {e}"));
                    }
                    Ok(Some(line)) => {
                        if line.trim().is_empty() { continue; }
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                            handle_stream_event(
                                &event, &mut result_text, &mut last_tool,
                                &mut input_tokens, &mut output_tokens,
                            );
                        }
                    }
                }
            }
            _ = &mut hard_deadline => {
                warn!("claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s) — killing process");
                let _ = child.kill().await;
                if result_text.is_empty() {
                    return Err(format!("claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s, no output)"));
                }
                break;
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("wait error: {e}"))?;
    if !status.success() && result_text.is_empty() {
        return Err(format!(
            "claude CLI exit {}",
            status.code().unwrap_or(-1)
        ));
    }

    let result_text = result_text.trim().to_string();
    if result_text.is_empty() {
        return Err("Empty response from claude CLI".to_string());
    }

    // Clear progress line
    eprint!("\r\x1b[K");

    // Rough cost estimate: ~$3/M input, ~$15/M output for Sonnet.
    // Formula matches estimated_cost_millicents() in cost_telemetry.rs:
    //   (tokens * rate) / 1_000_000, where rate is cents-per-M-tokens.
    // Result is in the same unit as Account::monthly_budget_cents.
    let cost_cents = input_tokens.saturating_mul(300).saturating_add(
        output_tokens.saturating_mul(1500)
    ) / 1_000_000;

    Ok(CliResponse {
        text: result_text,
        cost_cents,
    })
}

/// Parse a stream-json event, update result text, token usage, and show progress.
fn handle_stream_event(
    event: &serde_json::Value,
    result_text: &mut String,
    last_tool: &mut Option<String>,
    input_tokens: &mut u64,
    output_tokens: &mut u64,
) {
    match event.get("type").and_then(|t| t.as_str()) {
        Some("result") => {
            // Check for error result first
            if event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false) {
                let err_msg = event
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Unknown error");
                *result_text = format!("[error] {err_msg}");
            } else if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                *result_text = text.to_string();
            }
            // Extract token usage from result event
            extract_usage(event.get("usage"), input_tokens, output_tokens);
        }
        Some("assistant") => {
            if let Some(content) = event
                .pointer("/message/content")
                .and_then(|c| c.as_array())
            {
                for block in content {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                *result_text = text.to_string();
                            }
                        }
                        Some("tool_use") => {
                            let tool = block
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("unknown");
                            let detail = extract_tool_detail(block);
                            show_terminal_progress(tool, detail.as_deref(), last_tool);
                        }
                        _ => {}
                    }
                }
            }
            // Extract usage from assistant message if not yet captured
            if *input_tokens == 0 {
                extract_usage(event.pointer("/message/usage"), input_tokens, output_tokens);
            }
        }
        _ => {}
    }
}

/// Extract input/output token counts from a usage JSON object.
fn extract_usage(
    usage: Option<&serde_json::Value>,
    input_tokens: &mut u64,
    output_tokens: &mut u64,
) {
    if let Some(u) = usage {
        if let Some(n) = u.get("input_tokens").and_then(|v| v.as_u64()) {
            *input_tokens = n;
        }
        if let Some(n) = u.get("output_tokens").and_then(|v| v.as_u64()) {
            *output_tokens = n;
        }
    }
}

/// Show tool-use progress on the terminal (single overwritten line).
fn show_terminal_progress(tool: &str, detail: Option<&str>, last_tool: &mut Option<String>) {
    // Suppress consecutive duplicate tool names without detail
    if detail.is_none() && last_tool.as_deref() == Some(tool) {
        return;
    }
    *last_tool = Some(tool.to_string());

    let action = match tool {
        "Read" | "read" => "Reading",
        "Write" | "write" => "Writing",
        "Edit" | "edit" => "Editing",
        "Grep" | "grep" | "search" => "Searching",
        "Glob" | "glob" => "Finding files",
        "Bash" | "bash" => "Running command",
        "Agent" | "agent" => "Spawning agent",
        _ => "Using tool",
    };

    match detail {
        Some(d) => eprint!("\r\x1b[K  \x1b[2m{action}: {d}\x1b[0m"),
        None => eprint!("\r\x1b[K  \x1b[2m{action}…\x1b[0m"),
    }
    use std::io::Write;
    let _ = std::io::stderr().flush();
}

/// Extract a human-readable detail from a tool_use block's input.
fn extract_tool_detail(block: &serde_json::Value) -> Option<String> {
    let input = block.get("input")?;
    // Try common field names for tool details
    for key in ["file_path", "path", "command", "pattern", "description"] {
        if let Some(val) = input.get(key).and_then(|v| v.as_str()) {
            let truncated: String = val.chars().take(60).collect();
            return Some(truncated);
        }
    }
    None
}

// ── Error classification ────────────────────────────────────

fn is_billing_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("credit")
        || lower.contains("balance")
        || lower.contains("billing")
        || lower.contains("payment")
        || lower.contains("402")
        || lower.contains("insufficient_quota")
}

fn is_rate_limit_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("rate limit")
        || lower.contains("rate-limit")
        || lower.contains("ratelimit")
        || lower.contains("429")
        || lower.contains("usage limit")
        || lower.contains("overloaded")
        || lower.contains("capacity limit")
}
