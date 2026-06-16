//! Per-agent runtime/model config readers from `agent.toml` (RFC-25).
//!
//! Foundation for multi-runtime unlock:
//! - Phase 0: `[model] utility` — the cheap model for internal tasks (replaces
//!   scattered hardcoded `"claude-haiku-4-5"` literals).
//! - Phase 1+: `[runtime] provider` — which AgentRuntime backend to use.
//!
//! These are deliberately lightweight (parse `agent.toml` as a generic
//! `toml::Value`) so callers that only have an `agent_dir` can resolve config
//! without loading the full `AgentConfig`.

use std::path::Path;

use duduclaw_core::types::RuntimeType;

/// Default lightweight model when `[model] utility` is unset.
pub const DEFAULT_UTILITY_MODEL: &str = "claude-haiku-4-5";

fn read_agent_toml(agent_dir: &Path) -> Option<toml::Value> {
    let text = std::fs::read_to_string(agent_dir.join("agent.toml")).ok()?;
    text.parse::<toml::Value>().ok()
}

/// Resolve the agent's "utility" model (cheap internal tasks: compression,
/// key-fact extraction, GVU evolution, summarization, skill synthesis).
///
/// Reads `[model] utility` from the agent's `agent.toml`; falls back to
/// [`DEFAULT_UTILITY_MODEL`] when the file/key is missing or malformed.
pub fn agent_utility_model(agent_dir: &Path) -> String {
    read_agent_toml(agent_dir)
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(|m| m.get("utility"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_UTILITY_MODEL.to_string())
}

/// Resolve the agent's runtime provider (RFC-25 Phase 1).
///
/// Reads `[runtime] provider` from the agent's `agent.toml`; falls back to
/// [`RuntimeType::Claude`] when the file/key is missing or unrecognised.
pub fn agent_runtime_provider(agent_dir: &Path) -> RuntimeType {
    read_agent_toml(agent_dir)
        .as_ref()
        .and_then(|v| v.get("runtime"))
        .and_then(|r| r.get("provider"))
        .and_then(|s| s.as_str())
        .map(RuntimeType::parse)
        .unwrap_or_default()
}

/// Resolve the agent's runtime fallback provider (`[runtime] fallback`).
/// `None` when unset.
pub fn agent_runtime_fallback(agent_dir: &Path) -> Option<RuntimeType> {
    read_agent_toml(agent_dir)
        .as_ref()
        .and_then(|v| v.get("runtime"))
        .and_then(|r| r.get("fallback"))
        .and_then(|s| s.as_str())
        .map(RuntimeType::parse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_agent_toml(dir: &Path, body: &str) {
        let mut f = std::fs::File::create(dir.join("agent.toml")).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn utility_defaults_when_absent() {
        let dir = TempDir::new().unwrap();
        assert_eq!(agent_utility_model(dir.path()), DEFAULT_UTILITY_MODEL);
    }

    #[test]
    fn utility_reads_config() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[model]\nutility = \"claude-sonnet-4-6\"\n");
        assert_eq!(agent_utility_model(dir.path()), "claude-sonnet-4-6");
    }

    #[test]
    fn utility_defaults_on_malformed() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "this is not valid toml ===");
        assert_eq!(agent_utility_model(dir.path()), DEFAULT_UTILITY_MODEL);
    }

    #[test]
    fn provider_defaults_to_claude() {
        let dir = TempDir::new().unwrap();
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
        assert_eq!(agent_runtime_fallback(dir.path()), None);
    }

    #[test]
    fn provider_reads_config() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(
            dir.path(),
            "[runtime]\nprovider = \"gemini\"\nfallback = \"claude\"\n",
        );
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Gemini);
        assert_eq!(agent_runtime_fallback(dir.path()), Some(RuntimeType::Claude));
    }

    #[test]
    fn provider_unknown_falls_back_to_claude() {
        let dir = TempDir::new().unwrap();
        write_agent_toml(dir.path(), "[runtime]\nprovider = \"nonsense\"\n");
        assert_eq!(agent_runtime_provider(dir.path()), RuntimeType::Claude);
    }
}
