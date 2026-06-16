//! RFC-25 Phase 1 — provider-agnostic choke-point for agent prompting.
//!
//! `run_agent_prompt` is the single entry every internal caller (channel reply,
//! GVU, skill synthesis, delegation, A2A) should use instead of hardcoding the
//! Claude CLI. It resolves the agent's `[runtime] provider`, selects the matching
//! `AgentRuntime` from a process-wide `RuntimeRegistry` (auto-detected once at
//! first use), and executes — falling back to the configured fallback provider,
//! then to Claude, when the primary runtime is unavailable.

use std::path::{Path, PathBuf};

use tokio::sync::OnceCell;

use duduclaw_core::types::RuntimeType;

use crate::runtime::{RuntimeContext, RuntimeRegistry, RuntimeResponse};

/// Process-wide registry, auto-detected once (codex/gemini CLI probing is async).
static REGISTRY: OnceCell<RuntimeRegistry> = OnceCell::const_new();

/// Get (or lazily build) the shared runtime registry.
pub async fn registry(home_dir: &Path) -> &'static RuntimeRegistry {
    REGISTRY
        .get_or_init(|| async { RuntimeRegistry::new(home_dir).await })
        .await
}

/// Parameters for a provider-agnostic agent prompt.
pub struct AgentPrompt<'a> {
    pub agent_dir: Option<&'a Path>,
    pub home_dir: &'a Path,
    pub agent_id: &'a str,
    pub prompt: &'a str,
    pub system_prompt: &'a str,
    /// Model id within the chosen provider (e.g. claude-sonnet-4-6, gemini-2.5-flash).
    pub model: &'a str,
    pub max_tokens: u32,
}

/// Execute a prompt through the agent's configured runtime provider.
///
/// Selection order: `[runtime] provider` → `[runtime] fallback` → Claude.
pub async fn run_agent_prompt(req: AgentPrompt<'_>) -> Result<RuntimeResponse, String> {
    let provider = req
        .agent_dir
        .map(crate::runtime_config::agent_runtime_provider)
        .unwrap_or_default();
    let fallback = req
        .agent_dir
        .and_then(crate::runtime_config::agent_runtime_fallback)
        // Always fall back to Claude (the always-available core) if nothing set.
        .or(Some(RuntimeType::Claude));

    let reg = registry(req.home_dir).await;
    let runtime = reg
        .select(&provider, fallback.as_ref())
        .or_else(|| reg.get(&RuntimeType::Claude))
        .ok_or_else(|| format!("no runtime available for provider {provider:?}"))?;

    let ctx = RuntimeContext {
        agent_dir: req.agent_dir.map(PathBuf::from),
        system_prompt: req.system_prompt.to_string(),
        model: req.model.to_string(),
        max_tokens: req.max_tokens,
        home_dir: req.home_dir.to_path_buf(),
        agent_id: req.agent_id.to_string(),
        preferred_provider: None,
        conversation_history: Vec::new(),
    };

    runtime.execute(req.prompt, &ctx).await
}

/// Convenience wrapper returning just the text content (most internal callers).
pub async fn run_agent_prompt_text(req: AgentPrompt<'_>) -> Result<String, String> {
    run_agent_prompt(req).await.map(|r| r.content)
}
