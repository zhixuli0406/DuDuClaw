//! Context Window Budget Manager — allocates token budgets across 3 memory layers.
//!
//! Assembles the complete memory section for system prompt injection by:
//! 1. Loading Core Memory blocks (L1) — always present
//! 2. Retrieving recent Recall entries (L2) — cross-session conversation
//! 3. Searching Archival memories (L3) — relevant long-term knowledge
//!
//! Each layer has a configurable token budget. If a layer doesn't fill its
//! budget, the surplus is NOT redistributed (to keep prompt structure stable).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use duduclaw_core::error::Result;

use crate::archival_bridge::ArchivalMemoryBridge;
use crate::core_memory::CoreMemoryManager;
use crate::recall_memory::RecallMemoryManager;

// ── Config ──────────────────────────────────────────────────────

/// Token budget configuration for the 3-layer memory system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryBudgetConfig {
    /// Whether the 3-layer memory system is enabled.
    pub enabled: bool,
    /// Max tokens for Core Memory (L1). Default: 2000.
    pub core_tokens: u32,
    /// Max tokens for Recall Memory (L2). Default: 3000.
    pub recall_tokens: u32,
    /// Max tokens for Archival Memory (L3). Default: 1500.
    pub archival_tokens: u32,
    /// How many recent recall entries to auto-inject. Default: 10.
    pub recall_auto_inject: u32,
    /// How many archival entries to auto-retrieve. Default: 5.
    pub archival_auto_retrieve: u32,
}

impl Default for MemoryBudgetConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            core_tokens: 2000,
            recall_tokens: 3000,
            archival_tokens: 1500,
            recall_auto_inject: 10,
            archival_auto_retrieve: 5,
        }
    }
}

impl MemoryBudgetConfig {
    pub fn total_tokens(&self) -> u32 {
        self.core_tokens + self.recall_tokens + self.archival_tokens
    }
}

// ── Manager ─────────────────────────────────────────────────────

/// Builds the complete memory section for system prompt injection.
pub struct MemoryBudgetManager {
    pub core: Arc<CoreMemoryManager>,
    pub recall: Arc<RecallMemoryManager>,
    pub archival: Arc<ArchivalMemoryBridge>,
}

impl MemoryBudgetManager {
    pub fn new(
        core: Arc<CoreMemoryManager>,
        recall: Arc<RecallMemoryManager>,
        archival: Arc<ArchivalMemoryBridge>,
    ) -> Self {
        Self {
            core,
            recall,
            archival,
        }
    }

    /// Build the complete memory prompt section.
    ///
    /// # Arguments
    /// - `config`: Token budget configuration
    /// - `agent_id`: Which agent's memory to load
    /// - `channel`: Channel type (e.g., "telegram")
    /// - `chat_id`: Chat/conversation identifier
    /// - `user_message`: Current user message (for relevance-based archival retrieval)
    pub async fn build_memory_prompt(
        &self,
        config: &MemoryBudgetConfig,
        agent_id: &str,
        channel: &str,
        chat_id: &str,
        user_message: &str,
    ) -> Result<String> {
        if !config.enabled {
            return Ok(String::new());
        }

        let scope = format!("{channel}:{chat_id}");
        let mut sections = Vec::with_capacity(3);

        // L1: Core Memory (always present)
        let blocks = self.core.get_blocks(agent_id, &scope).await?;
        let core_section = CoreMemoryManager::render_prompt_section(&blocks, config.core_tokens);
        if !core_section.is_empty() {
            sections.push(core_section);
        }

        // L2: Recall Memory (recent cross-session conversations)
        let recall_entries = self
            .recall
            .get_recent(agent_id, channel, chat_id, config.recall_auto_inject)
            .await?;
        let recall_section =
            RecallMemoryManager::render_prompt_section(&recall_entries, config.recall_tokens);
        if !recall_section.is_empty() {
            sections.push(recall_section);
        }

        // L3: Archival Memory (relevant long-term knowledge)
        let archival_entries = self
            .archival
            .retrieve(agent_id, user_message, config.archival_auto_retrieve as usize)
            .await?;
        let archival_section = ArchivalMemoryBridge::render_prompt_section(
            &archival_entries,
            config.archival_tokens,
        );
        if !archival_section.is_empty() {
            sections.push(archival_section);
        }

        if sections.is_empty() {
            return Ok(String::new());
        }

        Ok(sections.join("\n\n"))
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = MemoryBudgetConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.core_tokens, 2000);
        assert_eq!(config.recall_tokens, 3000);
        assert_eq!(config.archival_tokens, 1500);
        assert_eq!(config.total_tokens(), 6500);
    }

    #[test]
    fn test_config_serde() {
        let json = r#"{"enabled": true, "core_tokens": 1000}"#;
        let config: MemoryBudgetConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.core_tokens, 1000);
        // Defaults for unset fields
        assert_eq!(config.recall_tokens, 3000);
    }
}
