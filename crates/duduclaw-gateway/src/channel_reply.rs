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

use crate::handlers::ChannelState;
use crate::session::SessionManager;

/// Shared channel status map, accessible by both channel bots and the RPC handler.
pub type ChannelStatusMap = Arc<RwLock<std::collections::HashMap<String, ChannelState>>>;

// ── Shared state ────────────────────────────────────────────

/// Shared context for building replies, initialized once at gateway start.
pub struct ReplyContext {
    pub registry: Arc<RwLock<AgentRegistry>>,
    pub home_dir: PathBuf,
    pub http: reqwest::Client,
    pub session_manager: Arc<SessionManager>,
    pub channel_status: ChannelStatusMap,
}

impl ReplyContext {
    pub fn new(
        registry: Arc<RwLock<AgentRegistry>>,
        home_dir: PathBuf,
        session_manager: Arc<SessionManager>,
        channel_status: ChannelStatusMap,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            registry,
            home_dir,
            http,
            session_manager,
            channel_status,
        }
    }
}

/// Helper to update a channel's connection state.
pub async fn set_channel_connected(status: &ChannelStatusMap, name: &str, connected: bool, error: Option<String>) {
    let mut map = status.write().await;
    map.insert(name.to_string(), ChannelState {
        connected,
        last_event: Some(chrono::Utc::now()),
        error,
    });
}

// ── Public API ──────────────────────────────────────────────

/// Build a reply for an incoming user message.
///
/// Strategy:
/// 1. Try Python Claude Code SDK (subprocess) — uses rotator + budget tracking
/// 2. Fallback to direct Anthropic API (Rust reqwest) — single key only
/// 3. Fallback to static error message
pub async fn build_reply(text: &str, ctx: &ReplyContext) -> String {
    build_reply_with_session(text, ctx, "default").await
}

/// Build a reply with session tracking.
pub async fn build_reply_with_session(text: &str, ctx: &ReplyContext, session_id: &str) -> String {
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

    let agent_id = agent.map(|a| a.config.agent.name.clone()).unwrap_or_default();
    let agent_dir = agent.map(|a| a.dir.clone());
    let evolution_enabled = agent
        .map(|a| a.config.evolution.micro_reflection)
        .unwrap_or(false);

    let system_prompt = build_system_prompt(agent);
    drop(reg);

    // Load session and prepend history to system prompt
    let session_mgr = &ctx.session_manager;
    let _ = session_mgr.get_or_create(session_id, &agent_id).await;

    // Scan user input for prompt injection before processing
    let scan = duduclaw_security::input_guard::scan_input(
        text,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
    );
    if scan.blocked {
        warn!(
            agent = %agent_id,
            score = scan.risk_score,
            rules = ?scan.matched_rules,
            "Prompt injection detected — blocking message"
        );
        return format!("⚠️ {}", scan.summary);
    }

    // Append user message to session using improved CJK-aware token estimate
    let user_tokens = estimate_tokens(text);
    if let Err(e) = session_mgr
        .append_message(session_id, "user", text, user_tokens)
        .await
    {
        warn!("Failed to save user message to session: {e}");
    }

    // Build conversation history from session
    let history = match session_mgr.get_messages(session_id).await {
        Ok(msgs) => {
            if msgs.len() > 1 {
                let history_text = msgs
                    .iter()
                    .rev()
                    .skip(1) // skip the message we just appended
                    .rev()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("\n\n## Conversation History\n{history_text}")
            } else {
                String::new()
            }
        }
        Err(e) => {
            warn!("Failed to load session messages: {e}");
            String::new()
        }
    };

    let full_system_prompt = if history.is_empty() {
        system_prompt
    } else {
        format!("{system_prompt}{history}")
    };

    // 1. Try `claude` CLI directly (Claude Code SDK — has built-in tools)
    let reply = match call_claude_cli(text, &model, &full_system_prompt, &ctx.home_dir).await {
        Ok(reply) => {
            info!("Claude replied via Claude Code SDK ({} chars)", reply.len());
            Some(reply)
        }
        Err(e) => {
            let log_line = format!("[{}] claude CLI error: {e}\n", chrono::Utc::now());
            let _ = tokio::fs::OpenOptions::new()
                .create(true).append(true)
                .open(ctx.home_dir.join("debug.log")).await
                .map(|mut f| { use tokio::io::AsyncWriteExt; tokio::spawn(async move { let _ = f.write_all(log_line.as_bytes()).await; }); });
            warn!("claude CLI unavailable: {e}");
            None
        }
    };

    // 2. Fallback: Python wrapper (with account rotator)
    let reply = match reply {
        Some(r) => Some(r),
        None => match call_python_sdk_v2(text, &model, &full_system_prompt, &ctx.home_dir).await {
            Ok(reply) => {
                info!("Claude replied via Python SDK ({} chars)", reply.len());
                Some(reply)
            }
            Err(e) => {
                let log_line = format!("[{}] python SDK error: {e}\n", chrono::Utc::now());
                let _ = tokio::fs::OpenOptions::new()
                    .create(true).append(true)
                    .open(ctx.home_dir.join("debug.log")).await
                    .map(|mut f| { use tokio::io::AsyncWriteExt; tokio::spawn(async move { let _ = f.write_all(log_line.as_bytes()).await; }); });
                warn!("Python SDK unavailable: {e}");
                None
            }
        },
    };

    if let Some(reply) = reply {
        // Save assistant reply to session
        let reply_tokens = estimate_tokens(&reply);
        if let Err(e) = session_mgr
            .append_message(session_id, "assistant", &reply, reply_tokens)
            .await
        {
            warn!("Failed to save assistant message to session: {e}");
        }

        // Check if compression needed; generate Claude summary then compress in background
        let sm = ctx.session_manager.clone();
        let sid = session_id.to_string();
        let home_for_compress = ctx.home_dir.clone();
        tokio::spawn(async move {
            if sm.should_compress(&sid).await {
                // Gather last messages to summarise
                let msgs = sm.get_messages(&sid).await.unwrap_or_default();
                let transcript: String = msgs.iter()
                    .map(|m| format!("[{}] {}", m.role, &m.content[..m.content.len().min(300)]))
                    .collect::<Vec<_>>()
                    .join("\n");
                let prompt = format!(
                    "Summarize the following conversation history concisely for use as context \
                     in future turns. Include key facts, decisions, and outcomes. Max 400 words.\n\n{transcript}"
                );
                let summary = match call_claude_cli(&prompt, "claude-haiku-4-5", "", &home_for_compress).await {
                    Ok(s) => s,
                    Err(_) => "[Session compressed — previous conversation summary omitted for brevity]".to_string(),
                };
                if let Err(e) = sm.compress(&sid, &summary).await {
                    warn!("Session compression failed: {e}");
                }
            }
        });

        // Trigger micro reflection in background (non-blocking)
        if evolution_enabled
            && let Some(dir) = agent_dir
        {
                let home = ctx.home_dir.clone();
                let aid = agent_id.clone();
                let summary = format!("User: {}\nAgent: {}", &text[..text.len().min(200)], &reply[..reply.len().min(200)]);
                tokio::spawn(async move {
                    crate::evolution::run_micro(&home, &aid, &dir, &summary).await;
                });
        }
        return reply;
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

    // Pass system prompt via temp file to avoid exposure in /proc/PID/cmdline (BE-C1)
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
                // Fallback to arg if temp file creation fails
                cmd.args(["--system-prompt", system_prompt]);
                None
            }
        }
    } else {
        None
    };

    cmd.env("ANTHROPIC_API_KEY", &api_key);
    // Prevent "nested session" error when gateway was launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let timeout_secs = get_cli_timeout_secs(home_dir).await;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        cmd.output(),
    )
    .await
    .map_err(|_| format!("claude CLI timeout ({timeout_secs}s)"))?
    .map_err(|e| format!("claude CLI spawn error: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = format!(
            "claude CLI exit {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(500).collect::<String>()
        );
        warn!("{detail}");
        return Err(detail);
    }

    if stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Empty stdout from claude CLI, stderr: {}", stderr.chars().take(500).collect::<String>());
        return Err("Empty response from claude CLI".to_string());
    }

    Ok(stdout)
}

/// Find the `claude` binary — delegates to shared impl in duduclaw-core (BE-L1).
fn which_claude() -> Option<String> {
    duduclaw_core::which_claude()
}

// ── Python SDK subprocess (fallback) ────────────────────────

/// Find the Python source path for `duduclaw.sdk.chat`.
fn find_python_path(home_dir: &Path) -> String {
    find_python_path_static(home_dir)
}

/// Public version usable from other modules (e.g. handlers).
pub fn find_python_path_static(home_dir: &Path) -> String {
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

/// Delegate execution — spawn Python subprocess with a given prompt and return the response.
pub async fn call_python_sdk_delegate(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    call_python_sdk_v2(prompt, model, system_prompt, home_dir).await
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

    let prompt_file = home_dir.join(format!(".tmp_system_prompt_{}.md", uuid::Uuid::new_v4()));
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

/// Read the CLI timeout from config.toml [gateway].cli_timeout_secs, default 300.
async fn get_cli_timeout_secs(home_dir: &Path) -> u64 {
    let config_path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(_) => return 300,
    };
    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(_) => return 300,
    };
    table
        .get("gateway")
        .and_then(|g| g.as_table())
        .and_then(|g| g.get("cli_timeout_secs"))
        .and_then(|v| v.as_integer())
        .map(|v| v.max(30) as u64)
        .unwrap_or(300)
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

/// Estimate the token count for a piece of text.
///
/// Uses a CJK-aware heuristic:
/// - CJK characters (U+3000–U+9FFF and supplementary ranges): ~1.5 chars/token
/// - ASCII words: ~4 chars/token
/// - Mixed: weighted average
///
/// This is significantly more accurate than the naive `len / 4` for Chinese,
/// Japanese, and Korean text, which is the primary language of this application.
fn estimate_tokens(text: &str) -> u32 {
    let mut cjk_chars: u32 = 0;
    let mut other_chars: u32 = 0;

    for ch in text.chars() {
        let cp = ch as u32;
        if (0x3000..=0x9FFF).contains(&cp)
            || (0xF900..=0xFAFF).contains(&cp)
            || (0x20000..=0x2A6DF).contains(&cp)
            || (0x2A700..=0x2CEAF).contains(&cp)
        {
            cjk_chars += 1;
        } else {
            other_chars += 1;
        }
    }

    // CJK: ~1.5 chars per token; other: ~4 chars per token
    let cjk_tokens = (cjk_chars as f32 / 1.5).ceil() as u32;
    let other_tokens = (other_chars as f32 / 4.0).ceil() as u32;
    cjk_tokens + other_tokens + 1 // +1 minimum
}

async fn get_api_key(home_dir: &Path) -> Option<String> {
    // Environment variable takes precedence
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some(key);
        }
    }
    // Try encrypted config field, fallback to plaintext
    crate::config_crypto::read_encrypted_config_field(home_dir, "api", "anthropic_api_key").await
}
