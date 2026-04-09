//! System prompt snapshot — frozen at session start for prompt cache optimization.
//!
//! By keeping the system prompt byte-identical across all turns in a session,
//! we maximize KV-cache hits (SGLang RadixAttention, Anthropic prompt caching).
//!
//! Reference: Prompt Cache (MLSys 2024, arXiv 2311.04934) — modular prompt segments.

use chrono::{DateTime, Utc};
use ring::digest;

/// Shared base system prompt, identical across all agents for prefix cache sharing.
///
/// This block appears first in every agent's system prompt, ensuring a common
/// prefix that maximizes KV-cache reuse across different agent sessions.
pub const SHARED_BASE: &str = "\
You are a DuDuClaw AI agent running within the Claude Code SDK.\n\
Follow your SOUL.md identity and behavioral directives.\n\
Use MCP tools to interact with channels, memory, and other agents.\n\
Always respond in the language matching the user's input.\n\
";

/// A frozen system prompt, built once at session creation.
///
/// All subsequent turns in the same session reuse this exact string,
/// ensuring prompt cache hits on both Anthropic API and local inference.
#[derive(Debug, Clone)]
pub struct SystemPromptSnapshot {
    /// The complete, frozen system prompt text.
    pub frozen_prompt: String,
    /// When this snapshot was created.
    pub frozen_at: DateTime<Utc>,
    /// SHA-256 of `frozen_prompt`, for debugging and cache key tracking.
    pub content_hash: String,
    /// Module ordering metadata (for diagnostics).
    pub module_order: Vec<PromptModule>,
    /// Force refresh flag — when true, the next turn rebuilds the snapshot.
    /// Used when skills are installed mid-session.
    pub force_refresh: bool,
}

/// A labeled section of the system prompt.
#[derive(Debug, Clone)]
pub struct PromptModule {
    pub name: String,
    pub byte_offset: usize,
    pub byte_length: usize,
}

impl SystemPromptSnapshot {
    /// Build a new snapshot from prompt text.
    ///
    /// Computes SHA-256 hash and records module boundaries.
    pub fn new(prompt: String, modules: Vec<PromptModule>) -> Self {
        let hash = {
            let d = digest::digest(&digest::SHA256, prompt.as_bytes());
            hex_encode(d.as_ref())
        };
        Self {
            frozen_prompt: prompt,
            frozen_at: Utc::now(),
            content_hash: hash,
            module_order: modules,
            force_refresh: false,
        }
    }

    /// Returns true if this snapshot is still valid (not expired).
    /// Snapshots don't expire during a session — this is for future use.
    pub fn is_valid(&self) -> bool {
        !self.force_refresh
    }

    /// Get the frozen prompt text for Claude CLI.
    pub fn prompt(&self) -> &str {
        &self.frozen_prompt
    }

    /// Mark this snapshot for refresh on the next turn.
    pub fn mark_for_refresh(&mut self) {
        self.force_refresh = true;
    }

    /// Check if refresh is needed and reset the flag.
    pub fn needs_refresh(&mut self) -> bool {
        if self.force_refresh {
            self.force_refresh = false;
            true
        } else {
            false
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_hash_is_deterministic() {
        let text = "Hello, world!".to_string();
        let s1 = SystemPromptSnapshot::new(text.clone(), vec![]);
        let s2 = SystemPromptSnapshot::new(text, vec![]);
        assert_eq!(s1.content_hash, s2.content_hash);
        assert!(!s1.content_hash.is_empty());
    }

    #[test]
    fn snapshot_is_always_valid() {
        let s = SystemPromptSnapshot::new("test".to_string(), vec![]);
        assert!(s.is_valid());
    }

    #[test]
    fn module_boundaries_are_recorded() {
        let modules = vec![
            PromptModule {
                name: "SHARED_BASE".to_string(),
                byte_offset: 0,
                byte_length: 100,
            },
            PromptModule {
                name: "IDENTITY".to_string(),
                byte_offset: 100,
                byte_length: 200,
            },
        ];
        let s = SystemPromptSnapshot::new("x".to_string(), modules);
        assert_eq!(s.module_order.len(), 2);
        assert_eq!(s.module_order[0].name, "SHARED_BASE");
        assert_eq!(s.module_order[0].byte_offset, 0);
        assert_eq!(s.module_order[0].byte_length, 100);
        assert_eq!(s.module_order[1].name, "IDENTITY");
        assert_eq!(s.module_order[1].byte_offset, 100);
        assert_eq!(s.module_order[1].byte_length, 200);
    }

    #[test]
    fn shared_base_is_not_empty() {
        assert!(!SHARED_BASE.is_empty());
        assert!(SHARED_BASE.contains("DuDuClaw"));
    }

    #[test]
    fn prompt_returns_frozen_text() {
        let text = "frozen prompt content".to_string();
        let s = SystemPromptSnapshot::new(text.clone(), vec![]);
        assert_eq!(s.prompt(), text);
    }

    #[test]
    fn different_inputs_produce_different_hashes() {
        let s1 = SystemPromptSnapshot::new("aaa".to_string(), vec![]);
        let s2 = SystemPromptSnapshot::new("bbb".to_string(), vec![]);
        assert_ne!(s1.content_hash, s2.content_hash);
    }

    #[test]
    fn force_refresh_lifecycle() {
        let mut snap = SystemPromptSnapshot::new("test".to_string(), vec![]);
        assert!(!snap.needs_refresh());
        assert!(snap.is_valid());
        snap.mark_for_refresh();
        assert!(!snap.is_valid());
        assert!(snap.needs_refresh());
        // After needs_refresh returns true, flag is reset
        assert!(!snap.needs_refresh());
        assert!(snap.is_valid());
    }
}
