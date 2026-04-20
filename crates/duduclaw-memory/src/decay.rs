//! Memory decay and archival — automatic cleanup of old, low-value memories.
//!
//! Configurable policy that archives old entries and deletes very old ones.
//! Designed to be called periodically (e.g., daily via heartbeat scheduler).

use rusqlite::params;
use tracing::{info, warn};

use crate::engine::SqliteMemoryEngine;

/// Policy controlling when memories are archived and deleted.
#[derive(Debug, Clone)]
pub struct MemoryDecayPolicy {
    /// Days after which low-importance entries are archived (default 90).
    pub archive_after_days: u32,
    /// Days after which archived entries are permanently deleted (default 365).
    pub delete_after_days: u32,
    /// Entries below this importance threshold decay faster (default 3.0).
    pub min_importance_to_keep: f64,
    /// Entries that have never been accessed decay first (default 0).
    pub min_access_count_to_keep: u32,
}

impl Default for MemoryDecayPolicy {
    fn default() -> Self {
        Self {
            archive_after_days: 90,
            delete_after_days: 365,
            min_importance_to_keep: 3.0,
            min_access_count_to_keep: 0,
        }
    }
}

/// Result of a decay run.
#[derive(Debug, Default)]
pub struct DecayReport {
    pub archived: u64,
    pub deleted: u64,
}

/// Run the decay policy against a memory engine.
///
/// Phase 1: Create archive table if needed, then move old + low-importance entries.
/// Phase 2: Delete entries from archive past the delete threshold.
///
/// Only affects memories matching the policy criteria — high-importance and
/// recently-accessed entries are preserved regardless of age.
pub async fn run_decay(engine: &SqliteMemoryEngine, policy: &MemoryDecayPolicy) -> DecayReport {
    let conn = engine.conn_for_maintenance().await;
    let mut report = DecayReport::default();

    // Ensure archive table exists
    if let Err(e) = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memories_archive (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            content TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            tags TEXT NOT NULL DEFAULT '[]',
            layer TEXT NOT NULL DEFAULT 'episodic',
            importance REAL NOT NULL DEFAULT 5.0,
            access_count INTEGER NOT NULL DEFAULT 0,
            last_accessed TEXT,
            source_event TEXT DEFAULT '',
            archived_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    ) {
        warn!("Failed to create archive table: {e}");
        return report;
    }

    // Compute cutoff timestamps upfront for parameterized queries (no SQL string interpolation).
    let now = chrono::Utc::now();
    let archive_cutoff = (now - chrono::Duration::days(policy.archive_after_days as i64)).to_rfc3339();
    let delete_cutoff = (now - chrono::Duration::days(policy.delete_after_days as i64)).to_rfc3339();

    // Phase 1: Archive old + low-importance + low-access entries.
    // Wrapped in a transaction for atomicity — INSERT, FTS cleanup, and DELETE
    // must all succeed or all roll back to prevent inconsistency.
    //
    // Safety: BEGIN IMMEDIATE acquires a RESERVED lock, preventing concurrent writes.
    // All three operations (INSERT archive, DELETE FTS, DELETE memories) use the same
    // WHERE condition within this transaction, so they operate on a consistent snapshot.
    // This is safe under WAL mode with IMMEDIATE transactions.
    let phase1_result: std::result::Result<u64, String> = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("BEGIN failed: {e}"))?;

        // INSERT OR IGNORE: tolerate re-runs if a previous attempt archived but
        // failed to delete (PRIMARY KEY conflict prevention).
        let archived = conn.execute(
            "INSERT OR IGNORE INTO memories_archive (id, agent_id, content, timestamp, tags, layer, importance, access_count, last_accessed, source_event)
             SELECT id, agent_id, content, timestamp, tags, layer, importance, access_count, last_accessed, source_event
             FROM memories
             WHERE timestamp < ?1
               AND importance < ?2
               AND access_count <= ?3
               AND layer != 'semantic'",
            params![archive_cutoff, policy.min_importance_to_keep, policy.min_access_count_to_keep],
        ).map_err(|e| format!("Archive INSERT failed: {e}"))?;

        if archived > 0 {
            // Precise FTS cleanup: only delete entries for memories we're about to remove.
            conn.execute(
                "DELETE FROM memories_fts WHERE memory_id IN (
                     SELECT id FROM memories
                     WHERE timestamp < ?1
                       AND importance < ?2
                       AND access_count <= ?3
                       AND layer != 'semantic'
                 )",
                params![archive_cutoff, policy.min_importance_to_keep, policy.min_access_count_to_keep],
            ).map_err(|e| format!("FTS cleanup failed: {e}"))?;

            conn.execute(
                "DELETE FROM memories
                 WHERE timestamp < ?1
                   AND importance < ?2
                   AND access_count <= ?3
                   AND layer != 'semantic'",
                params![archive_cutoff, policy.min_importance_to_keep, policy.min_access_count_to_keep],
            ).map_err(|e| format!("Archive DELETE failed: {e}"))?;
        }

        conn.execute_batch("COMMIT")
            .map_err(|e| format!("COMMIT failed: {e}"))?;

        Ok(archived as u64)
    })();

    match phase1_result {
        Ok(count) => {
            report.archived = count;
        }
        Err(e) => {
            warn!("Archive phase failed: {e}");
            let _ = conn.execute_batch("ROLLBACK");
            return report;
        }
    }

    // Phase 2: Delete very old archived entries.
    // Note: delete_cutoff applies to `archived_at` (when the record was archived),
    // not the original `timestamp` (when the memory was created).
    // A record survives: archive_after_days (in memories) + delete_after_days (in archive).
    // Wrapped in a transaction so a partial delete is rolled back on error.
    let phase2_result: std::result::Result<u64, String> = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("BEGIN phase2 failed: {e}"))?;
        let count = conn.execute(
            "DELETE FROM memories_archive WHERE archived_at < ?1",
            params![delete_cutoff],
        ).map_err(|e| {
            let _ = conn.execute_batch("ROLLBACK");
            format!("Delete phase failed: {e}")
        })?;
        conn.execute_batch("COMMIT")
            .map_err(|e| format!("COMMIT phase2 failed: {e}"))?;
        Ok(count as u64)
    })();

    match phase2_result {
        Ok(count) => {
            report.deleted = count;
        }
        Err(e) => {
            warn!("{e}");
        }
    }

    if report.archived > 0 || report.deleted > 0 {
        info!(
            archived = report.archived,
            deleted = report.deleted,
            "Memory decay completed"
        );
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SqliteMemoryEngine;
    use chrono::{Duration, Utc};
    use duduclaw_core::traits::MemoryEngine;
    use duduclaw_core::types::MemoryEntry;

    fn old_entry(agent_id: &str, days_ago: i64, importance: f64) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: format!("Old memory from {} days ago", days_ago),
            timestamp: Utc::now() - Duration::days(days_ago),
            tags: vec![],
            embedding: None,
            layer: duduclaw_core::types::MemoryLayer::Episodic,
            importance,
            access_count: 0,
            last_accessed: None,
            source_event: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn decay_archives_old_low_importance() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "test";

        // Store a 100-day-old, low-importance entry
        engine.store(agent, old_entry(agent, 100, 2.0)).await.unwrap();
        // Store a recent entry (should be kept)
        engine.store(agent, old_entry(agent, 1, 2.0)).await.unwrap();
        // Store an old but important entry (should be kept)
        engine.store(agent, old_entry(agent, 100, 8.0)).await.unwrap();

        let policy = MemoryDecayPolicy {
            archive_after_days: 90,
            delete_after_days: 365,
            min_importance_to_keep: 3.0,
            min_access_count_to_keep: 0,
        };

        let report = run_decay(&engine, &policy).await;
        assert_eq!(report.archived, 1);

        // Verify only 2 entries remain in main table
        let remaining = engine.list_recent(agent, 10).await.unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[tokio::test]
    async fn decay_preserves_semantic_layer() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "test";

        let mut entry = old_entry(agent, 200, 1.0);
        entry.layer = duduclaw_core::types::MemoryLayer::Semantic;
        engine.store(agent, entry).await.unwrap();

        let policy = MemoryDecayPolicy::default();
        let report = run_decay(&engine, &policy).await;
        assert_eq!(report.archived, 0);

        let remaining = engine.list_recent(agent, 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
    }
}
