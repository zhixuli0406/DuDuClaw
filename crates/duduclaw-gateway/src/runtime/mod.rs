//! Multi-runtime agent execution — Claude CLI, Codex CLI, Gemini CLI.
//!
//! The `AgentRuntime` trait abstracts over different CLI-based AI agents.
//! Each runtime translates its JSONL output format into a unified `RuntimeResponse`.

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
}
