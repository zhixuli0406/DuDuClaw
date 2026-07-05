//! Multi-runtime agent execution — Claude CLI, Codex CLI, Gemini CLI.
//!
//! The `AgentRuntime` trait abstracts over different CLI-based AI agents.
//! Each runtime translates its JSONL output format into a unified `RuntimeResponse`.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod openai_compat;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;
use tracing::{info, warn};

// ── Core trait ──────────────────────────────────────────────────

/// Unified response from any agent runtime.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeResponse {
    pub content: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub model_used: String,
    pub runtime_name: String,
}

/// A single turn in conversation history.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationTurn {
    pub role: String,    // "user" | "assistant"
    pub content: String,
}

/// Context passed to a runtime execution.
#[derive(Debug, Clone)]
pub struct RuntimeContext {
    /// Agent working directory (contains SOUL.md, CLAUDE.md, etc.)
    pub agent_dir: Option<PathBuf>,
    /// System prompt built from agent's loaded files.
    pub system_prompt: String,
    /// Model name to use (e.g., "claude-sonnet-4-6", "gpt-5", "gemini-2.5-flash").
    pub model: String,
    /// Maximum output tokens.
    pub max_tokens: u32,
    /// Home directory for config/key lookup.
    pub home_dir: PathBuf,
    /// Agent ID for telemetry.
    pub agent_id: String,
    /// Preferred OpenAI-compatible provider name (e.g. "minimax", "deepseek").
    /// Used by OpenAiCompatRuntime to resolve the correct API key first.
    pub preferred_provider: Option<String>,
    /// Conversation history for multi-turn context (chronological, newest last).
    /// Excludes the current user message. Empty on first turn.
    pub conversation_history: Vec<ConversationTurn>,
    /// Agent capability restrictions (`agent.toml [capabilities]`).
    ///
    /// `Some` when the caller resolved an agent directory — runtimes MUST
    /// translate these into their CLI's enforcement flags (Claude
    /// `--allowedTools`/`--disallowedTools`, Codex `--sandbox`, Gemini
    /// `--approval-mode`/`--sandbox`) and emit a structured `warn!` when the
    /// CLI cannot fully honor them. `None` (agent-less utility calls) keeps
    /// each runtime's legacy behavior.
    pub capabilities: Option<duduclaw_core::types::CapabilitiesConfig>,
}

/// Streaming chunk from a runtime execution.
#[derive(Debug, Clone)]
pub enum RuntimeChunk {
    Text(String),
    ToolUse { name: String, input: serde_json::Value },
    ToolResult { output: String },
    Done(RuntimeResponse),
    Error(String),
}

/// Abstract runtime for executing agent tasks.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Human-readable name of this runtime.
    fn name(&self) -> &str;

    /// Execute a prompt and return the response.
    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String>;

    /// Check if this runtime is available (CLI installed, API key configured, etc.)
    async fn is_available(&self) -> bool;
}

// ── Runtime type enum ───────────────────────────────────────────

// Re-export RuntimeType from core
pub use duduclaw_core::types::RuntimeType;

// ── Registry ────────────────────────────────────────────────────

/// Registry of available runtimes, auto-detected at startup.
pub struct RuntimeRegistry {
    runtimes: HashMap<RuntimeType, Box<dyn AgentRuntime>>,
}

impl RuntimeRegistry {
    /// Create a new registry and auto-detect available runtimes.
    pub async fn new(home_dir: &Path) -> Self {
        let mut runtimes: HashMap<RuntimeType, Box<dyn AgentRuntime>> = HashMap::new();

        // Claude is always available (it's the core)
        runtimes.insert(
            RuntimeType::Claude,
            Box::new(claude::ClaudeRuntime::new(home_dir.to_path_buf())),
        );

        // Codex: check if `codex` CLI is installed
        let codex = codex::CodexRuntime::new();
        if codex.is_available().await {
            info!("Codex CLI detected — registering CodexRuntime");
            runtimes.insert(RuntimeType::Codex, Box::new(codex));
        }

        // Gemini: check if `gemini` CLI is installed
        let gemini = gemini::GeminiRuntime::new();
        if gemini.is_available().await {
            info!("Gemini CLI detected — registering GeminiRuntime");
            runtimes.insert(RuntimeType::Gemini, Box::new(gemini));
        }

        // Antigravity (`agy`): the 2026-06-18 successor to the personal Gemini CLI.
        let antigravity = antigravity::AntigravityRuntime::new();
        if antigravity.is_available().await {
            info!("Antigravity CLI (agy) detected — registering AntigravityRuntime");
            runtimes.insert(RuntimeType::Antigravity, Box::new(antigravity));
        }

        // OpenAI-compatible: always available if API key is configured
        runtimes.insert(
            RuntimeType::OpenAiCompat,
            Box::new(openai_compat::OpenAiCompatRuntime::new()),
        );

        Self { runtimes }
    }

    /// Get a runtime by type.
    pub fn get(&self, runtime_type: &RuntimeType) -> Option<&dyn AgentRuntime> {
        self.runtimes.get(runtime_type).map(|r| r.as_ref())
    }

    /// Get the runtime for an agent config, with fallback.
    pub fn select(
        &self,
        primary: &RuntimeType,
        fallback: Option<&RuntimeType>,
    ) -> Option<&dyn AgentRuntime> {
        self.get(primary).or_else(|| {
            if let Some(fb) = fallback {
                let rt = self.get(fb);
                if rt.is_some() {
                    warn!(
                        primary = ?primary,
                        fallback = ?fb,
                        "Primary runtime unavailable, using fallback"
                    );
                }
                rt
            } else {
                None
            }
        })
    }

    /// List all available runtime types.
    pub fn available(&self) -> Vec<(&RuntimeType, &str)> {
        self.runtimes
            .iter()
            .map(|(t, r)| (t, r.name()))
            .collect()
    }
}

// ── Helpers ─────────────────────────────────────────────────────

/// Load `agent.toml [capabilities]` for an agent directory.
///
/// Returns `None` only when `agent.toml` itself is missing (synthetic /
/// test agent ids) — callers then keep their legacy capability-less
/// behavior. When the file exists but `[capabilities]` is absent OR fails
/// to deserialize, this returns `Some(CapabilitiesConfig::default())`
/// (deny-by-default: `computer_use = false`, `browser_via_bash = false`)
/// with a warn on the malformed case — security gates fail closed.
pub fn load_agent_capabilities(
    agent_dir: &Path,
) -> Option<duduclaw_core::types::CapabilitiesConfig> {
    let path = agent_dir.join("agent.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let parsed = match text.parse::<toml::Value>() {
        Ok(v) => v,
        Err(e) => {
            warn!(
                agent_dir = %agent_dir.display(),
                error = %e,
                "agent.toml parse failed — applying default (deny-by-default) capabilities"
            );
            return Some(duduclaw_core::types::CapabilitiesConfig::default());
        }
    };
    let caps = match parsed.get("capabilities") {
        None => duduclaw_core::types::CapabilitiesConfig::default(),
        Some(section) => match section.clone().try_into() {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    agent_dir = %agent_dir.display(),
                    error = %e,
                    "[capabilities] section malformed — applying default (deny-by-default) capabilities"
                );
                duduclaw_core::types::CapabilitiesConfig::default()
            }
        },
    };
    Some(caps)
}

/// Build the `duduclaw` MCP server definition for a non-Claude CLI's native
/// MCP config (Codex `config.toml [mcp_servers]`, Gemini/Antigravity
/// `settings.json mcpServers`). Mirrors
/// `duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path` for Claude:
/// absolute `duduclaw` binary + `mcp-server` arg + `DUDUCLAW_AGENT_ID` env
/// (the MCP subprocess self-identifies through it — without it every call
/// falls back to `default_agent` and supervisor authorization breaks), plus
/// this instance's `DUDUCLAW_HOME` / `DUDUCLAW_PORT` / `DUDUCLAW_INSTANCE`
/// overrides when set (multi-instance isolation, Plan A).
///
/// Returns `None` when the duduclaw binary cannot be resolved to an absolute
/// path — registering a PATH-relative command would break for CLI subprocesses
/// launched without PATH inheritance.
pub fn duduclaw_mcp_server_json(agent_id: &str) -> Option<serde_json::Value> {
    let bin = duduclaw_core::resolve_duduclaw_bin();
    if !bin.is_absolute() {
        return None;
    }
    let mut env = serde_json::Map::new();
    env.insert(
        duduclaw_core::ENV_AGENT_ID.to_string(),
        serde_json::Value::String(agent_id.to_string()),
    );
    for var in ["DUDUCLAW_HOME", "DUDUCLAW_PORT", "DUDUCLAW_INSTANCE"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                env.insert(var.to_string(), serde_json::Value::String(v));
            }
        }
    }
    Some(serde_json::json!({
        "command": bin.to_string_lossy(),
        "args": ["mcp-server"],
        "env": serde_json::Value::Object(env),
    }))
}

/// Format conversation history as an XML-delimited prompt prefix.
///
/// Used by CLI-based runtimes (Gemini, Codex) that lack native multi-turn
/// support, and as a fallback for Claude CLI when `--resume` is unavailable.
///
/// NOTE: The canonical implementation with turn trimming lives in
/// `channel_reply.rs`. This version is for the AgentRuntime trait path.
pub fn format_history_as_prompt(history: &[ConversationTurn], current_message: &str) -> String {
    if history.is_empty() {
        return current_message.to_string();
    }
    let mut buf = String::with_capacity(history.len() * 200 + current_message.len() + 64);
    buf.push_str("<conversation_history>\n");
    for turn in history {
        // Escape closing tags in content to prevent XML structure corruption
        let safe_content = turn.content
            .replace("</user>", "&lt;/user&gt;")
            .replace("</assistant>", "&lt;/assistant&gt;");
        buf.push('<');
        buf.push_str(&turn.role);
        buf.push('>');
        buf.push_str(&safe_content);
        buf.push_str("</");
        buf.push_str(&turn.role);
        buf.push_str(">\n");
    }
    buf.push_str("</conversation_history>\n\n");
    buf.push_str(current_message);
    buf
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_type_default() {
        assert_eq!(RuntimeType::default(), RuntimeType::Claude);
    }

    #[test]
    fn test_runtime_type_serde() {
        let json = serde_json::to_string(&RuntimeType::Codex).unwrap();
        assert_eq!(json, r#""codex""#);

        let parsed: RuntimeType = serde_json::from_str::<RuntimeType>(r#""gemini""#).unwrap();
        assert_eq!(parsed, RuntimeType::Gemini);
    }

    #[test]
    fn load_capabilities_none_when_agent_toml_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_agent_capabilities(dir.path()).is_none());
    }

    #[test]
    fn load_capabilities_defaults_when_section_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        let caps = load_agent_capabilities(dir.path()).expect("file exists");
        assert!(!caps.computer_use, "deny-by-default");
        assert!(!caps.browser_via_bash);
        assert!(caps.allowed_tools.is_empty());
    }

    #[test]
    fn load_capabilities_parses_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[capabilities]\ncomputer_use = true\nallowed_tools = [\"Read\"]\n",
        )
        .unwrap();
        let caps = load_agent_capabilities(dir.path()).unwrap();
        assert!(caps.computer_use);
        assert_eq!(caps.allowed_tools, vec!["Read".to_string()]);
    }

    #[test]
    fn load_capabilities_fails_closed_on_malformed_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            // computer_use has the wrong type — the section must fail closed
            // to the deny-by-default config, not silently grant anything.
            "[capabilities]\ncomputer_use = \"yes please\"\n",
        )
        .unwrap();
        let caps = load_agent_capabilities(dir.path()).unwrap();
        assert!(!caps.computer_use, "malformed section must fail closed");
    }

    #[test]
    fn duduclaw_mcp_server_json_carries_agent_id() {
        // resolve_duduclaw_bin falls back to current_exe (absolute under
        // cargo test); when unresolvable this correctly yields None.
        let Some(def) = duduclaw_mcp_server_json("agnes") else {
            return;
        };
        assert_eq!(def["args"][0], "mcp-server");
        assert_eq!(def["env"][duduclaw_core::ENV_AGENT_ID], "agnes");
        assert!(std::path::Path::new(def["command"].as_str().unwrap()).is_absolute());
    }
}
