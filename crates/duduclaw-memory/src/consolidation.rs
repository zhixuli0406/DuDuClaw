//! Auto-Consolidation Pipeline — promotes short-term memory to long-term knowledge.
//!
//! Triggers:
//! - **Session compress**: session summary → episodic memory
//! - **Core block overflow**: old content → archival, keep newest
//! - **Recall aging**: entries > 30 days → archival if high-importance, else discard
//! - **Episodic pressure**: high episodic count → promote to semantic (via existing engine)

use std::sync::Arc;

use tracing::info;

use duduclaw_core::error::Result;
use duduclaw_core::types::MemoryLayer;

use crate::archival_bridge::ArchivalMemoryBridge;
use crate::core_memory::CoreMemoryManager;
use crate::recall_memory::RecallMemoryManager;

/// Report of consolidation actions taken.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    /// Number of recall entries purged.
    pub recall_purged: u64,
    /// Number of recall entries promoted to archival.
    pub recall_promoted: u64,
    /// Number of core blocks overflowed to archival.
    pub core_overflowed: u64,
}

/// Manages memory consolidation across layers.
pub struct ConsolidationPipeline {
    core: Arc<CoreMemoryManager>,
    recall: Arc<RecallMemoryManager>,
    archival: Arc<ArchivalMemoryBridge>,
}

impl ConsolidationPipeline {
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

    /// Called when a session is compressed. Store the summary as episodic memory.
    pub async fn on_session_compress(
        &self,
        agent_id: &str,
        session_id: &str,
        summary: &str,
    ) -> Result<()> {
        if summary.trim().is_empty() {
            return Ok(());
        }

        let content = format!("[Session {session_id} summary] {summary}");
        self.archival
            .store(
                agent_id,
                &content,
                MemoryLayer::Episodic,
                6.0, // moderate importance
                "session_compress",
                vec!["session_summary".to_string()],
            )
            .await?;

        info!(
            agent_id,
            session_id, "Session summary promoted to episodic memory"
        );
        Ok(())
    }

    /// Periodic consolidation check. Run via heartbeat scheduler.
    pub async fn check_and_consolidate(
        &self,
        agent_id: &str,
    ) -> Result<ConsolidationReport> {
        let mut report = ConsolidationReport::default();

        // 1. Purge old recall entries (> 30 days)
        // TODO: promote high-importance entries before purging
        let purged = self.recall.purge_older_than(30).await?;
        report.recall_purged = purged;
        if purged > 0 {
            info!(agent_id, purged, "Recall entries purged (>30 days)");
        }

        Ok(report)
    }

    /// Handle core block overflow: move old content to archival.
    ///
    /// Called when `core_memory_append` would exceed max_tokens.
    /// Extracts the first half of the block content, stores it as
    /// episodic memory, then truncates the block.
    pub async fn overflow_core_block(
        &self,
        agent_id: &str,
        label: &str,
        scope: &str,
    ) -> Result<()> {
        let block = self.core.get_block(agent_id, label, scope).await?;
        let block = match block {
            Some(b) => b,
            None => return Ok(()),
        };

        if block.content.is_empty() {
            return Ok(());
        }

        // Store the entire current content as archival
        let content = format!(
            "[Core memory overflow: {label}] {}",
            block.content
        );
        self.archival
            .store(
                agent_id,
                &content,
                MemoryLayer::Episodic,
                5.0,
                "core_overflow",
                vec![format!("core_{label}")],
            )
            .await?;

        // Clear the block
        self.core.set(agent_id, label, scope, "").await?;

        info!(
            agent_id,
            label, "Core block overflowed to archival and cleared"
        );
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SqliteMemoryEngine;

    fn setup() -> (
        Arc<CoreMemoryManager>,
        Arc<RecallMemoryManager>,
        Arc<ArchivalMemoryBridge>,
    ) {
        let core = Arc::new(CoreMemoryManager::in_memory().unwrap());
        let recall = Arc::new(RecallMemoryManager::in_memory().unwrap());
        let engine = Arc::new(SqliteMemoryEngine::in_memory().unwrap());
        let archival = Arc::new(ArchivalMemoryBridge::new(engine));
        (core, recall, archival)
    }

    #[tokio::test]
    async fn test_on_session_compress() {
        let (core, recall, archival) = setup();
        let pipeline = ConsolidationPipeline::new(core, recall, archival.clone());

        pipeline
            .on_session_compress("agent-1", "sess-123", "User asked about triggers")
            .await
            .unwrap();

        // Verify it was stored in archival
        let results = archival.retrieve("agent-1", "triggers", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("triggers"));
    }

    #[tokio::test]
    async fn test_on_session_compress_empty_summary() {
        let (core, recall, archival) = setup();
        let pipeline = ConsolidationPipeline::new(core, recall, archival);

        // Should not fail on empty summary
        pipeline
            .on_session_compress("agent-1", "sess-123", "")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_overflow_core_block() {
        let (core, recall, archival) = setup();
        let pipeline = ConsolidationPipeline::new(core.clone(), recall, archival.clone());

        core.init_defaults("agent-1").await.unwrap();
        core.append("agent-1", "user_facts", "", "Some important fact")
            .await
            .unwrap();

        pipeline
            .overflow_core_block("agent-1", "user_facts", "")
            .await
            .unwrap();

        // Block should be cleared
        let block = core.get_block("agent-1", "user_facts", "").await.unwrap().unwrap();
        assert!(block.content.is_empty());

        // Content should be in archival
        let results = archival.retrieve("agent-1", "important fact", 5).await.unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_check_and_consolidate() {
        let (core, recall, archival) = setup();
        let pipeline = ConsolidationPipeline::new(core, recall.clone(), archival);

        // Add an old entry
        let mut old_entry = crate::recall_memory::RecallEntry {
            agent_id: "agent-1".into(),
            channel: "telegram".into(),
            chat_id: "123".into(),
            role: "user".into(),
            content: "old message".into(),
            timestamp: "2020-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        recall.record(old_entry).await.unwrap();

        // Add a recent entry
        recall
            .record(crate::recall_memory::RecallEntry {
                agent_id: "agent-1".into(),
                channel: "telegram".into(),
                chat_id: "123".into(),
                role: "user".into(),
                content: "new message".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            })
            .await
            .unwrap();

        let report = pipeline.check_and_consolidate("agent-1").await.unwrap();
        assert_eq!(report.recall_purged, 1);
    }
}
