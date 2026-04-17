//! Archival Memory (L3) — Bridge to existing cognitive memory engine.
//!
//! Wraps `SqliteMemoryEngine` (episodic + semantic) with a simplified
//! interface for the 3-layer memory system. The existing engine already
//! provides Generative Agents 3D-weighted retrieval, FTS5 search, and
//! decay/archival policies — this bridge adds:
//!
//! - Layer-filtered retrieval (semantic-preferred for prompt injection)
//! - Prompt section rendering with token budget
//! - Simplified store/retrieve API for MCP tools

use std::sync::Arc;

use duduclaw_core::error::Result;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};

use crate::engine::SqliteMemoryEngine;

// ── Token estimation ────────────────────────────────────────────

fn estimate_tokens(text: &str) -> u32 {
    let mut cjk: u32 = 0;
    let mut total: u32 = 0;
    for c in text.chars() {
        total += 1;
        if matches!(c,
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' |
            '\u{3000}'..='\u{303F}' | '\u{3040}'..='\u{309F}' |
            '\u{30A0}'..='\u{30FF}' | '\u{FF00}'..='\u{FFEF}'
        ) {
            cjk += 1;
        }
    }
    let non_cjk = total - cjk;
    let cjk_tokens = ((cjk as f64) / 1.5).ceil() as u32;
    let ascii_tokens = non_cjk / 4;
    (cjk_tokens + ascii_tokens).max(1)
}

// ── Bridge ──────────────────────────────────────────────────────

/// Bridge between existing `SqliteMemoryEngine` and the 3-layer system.
pub struct ArchivalMemoryBridge {
    engine: Arc<SqliteMemoryEngine>,
}

impl ArchivalMemoryBridge {
    pub fn new(engine: Arc<SqliteMemoryEngine>) -> Self {
        Self { engine }
    }

    /// Retrieve top-k relevant archival memories for a query.
    ///
    /// Prefers semantic memories (generalised knowledge) but also returns
    /// high-importance episodic memories (specific experiences).
    pub async fn retrieve(
        &self,
        agent_id: &str,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<MemoryEntry>> {
        if query.trim().is_empty() {
            // Fall back to recent high-importance memories
            return self.engine.list_recent(agent_id, top_k).await;
        }

        // Search semantic layer first
        let mut results = self
            .engine
            .search_layer(agent_id, query, &MemoryLayer::Semantic, top_k)
            .await?;

        // If not enough, supplement with episodic
        if results.len() < top_k {
            let remaining = top_k - results.len();
            let episodic = self
                .engine
                .search_layer(agent_id, query, &MemoryLayer::Episodic, remaining)
                .await?;
            results.extend(episodic);
        }

        Ok(results)
    }

    /// Store a new archival memory entry.
    pub async fn store(
        &self,
        agent_id: &str,
        content: &str,
        layer: MemoryLayer,
        importance: f64,
        source_event: &str,
        tags: Vec<String>,
    ) -> Result<String> {
        use chrono::Utc;
        use duduclaw_core::traits::MemoryEngine;

        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags,
            embedding: None,
            layer,
            importance,
            access_count: 0,
            last_accessed: None,
            source_event: source_event.to_string(),
        };

        let id = entry.id.clone();
        self.engine.store(agent_id, entry).await?;
        Ok(id)
    }

    /// Render retrieved memories as a system prompt section.
    pub fn render_prompt_section(entries: &[MemoryEntry], budget_tokens: u32) -> String {
        if entries.is_empty() {
            return String::new();
        }

        let header = "## Long-term Knowledge (archival memory)";
        let mut parts = Vec::with_capacity(entries.len() + 1);
        parts.push(header.to_string());
        let mut used_tokens = estimate_tokens(header);

        for entry in entries {
            let layer_tag = entry.layer.as_str();
            let tags_str = if entry.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", entry.tags.join(", "))
            };
            let line = format!(
                "- ({}, imp:{:.1}) {}{}",
                layer_tag, entry.importance, entry.content, tags_str
            );
            let line_tokens = estimate_tokens(&line);
            if used_tokens + line_tokens > budget_tokens {
                break;
            }
            used_tokens += line_tokens;
            parts.push(line);
        }

        if parts.len() <= 1 {
            return String::new();
        }

        parts.join("\n")
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_render_prompt_section() {
        let entries = vec![
            MemoryEntry {
                id: "1".into(),
                agent_id: "a".into(),
                content: "User prefers Traditional Chinese".into(),
                timestamp: Utc::now(),
                tags: vec!["preference".into()],
                embedding: None,
                layer: MemoryLayer::Semantic,
                importance: 8.5,
                access_count: 3,
                last_accessed: None,
                source_event: "user_feedback".into(),
            },
            MemoryEntry {
                id: "2".into(),
                agent_id: "a".into(),
                content: "Discussed trigger scheduling on 2026-04-12".into(),
                timestamp: Utc::now(),
                tags: vec![],
                embedding: None,
                layer: MemoryLayer::Episodic,
                importance: 6.0,
                access_count: 1,
                last_accessed: None,
                source_event: "prediction_episodic".into(),
            },
        ];

        let prompt = ArchivalMemoryBridge::render_prompt_section(&entries, 1500);
        assert!(prompt.contains("## Long-term Knowledge"));
        assert!(prompt.contains("semantic"));
        assert!(prompt.contains("imp:8.5"));
        assert!(prompt.contains("User prefers Traditional Chinese"));
        assert!(prompt.contains("[preference]"));
    }

    #[test]
    fn test_render_empty() {
        let prompt = ArchivalMemoryBridge::render_prompt_section(&[], 1500);
        assert!(prompt.is_empty());
    }
}
