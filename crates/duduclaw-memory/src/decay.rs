//! Memory decay and archival — automatic cleanup of old, low-value memories.
//!
//! Archival is driven by Ebbinghaus retrievability (MemoryBank, arXiv:2305.10250):
//! a memory is archived once it is old enough AND its retrievability
//! `R = exp(-t / S)` has dropped below a threshold, where stability `S` grows
//! with access reinforcement and importance (see
//! [`crate::engine::ebbinghaus_retrievability`]). Frequently-recalled memories
//! survive; never-recalled ones fade naturally.
//!
//! Designed to be called periodically (e.g., daily via heartbeat scheduler).

use chrono::{DateTime, Utc};
use rusqlite::params;
use tracing::{info, warn};

use crate::engine::{ebbinghaus_retrievability, SqliteMemoryEngine};

/// Policy controlling when memories are archived and deleted.
#[derive(Debug, Clone)]
pub struct MemoryDecayPolicy {
    /// Minimum age in days before an entry is even considered for archival
    /// (default 90). A hard floor — nothing younger is touched regardless of
    /// retrievability.
    pub archive_after_days: u32,
    /// Days after which archived entries are permanently deleted (default 365).
    pub delete_after_days: u32,
    /// Entries at or above this importance are never archived (default 3.0).
    pub min_importance_to_keep: f64,
    /// Archive candidates whose Ebbinghaus retrievability has fallen below
    /// this threshold (default 0.05). Reinforced (recently/frequently accessed)
    /// memories keep a high retrievability and are preserved.
    pub min_retrievability: f64,
}

impl Default for MemoryDecayPolicy {
    fn default() -> Self {
        Self {
            archive_after_days: 90,
            delete_after_days: 365,
            min_importance_to_keep: 3.0,
            min_retrievability: 0.05,
        }
    }
}

/// Result of a decay run.
#[derive(Debug, Default)]
pub struct DecayReport {
    pub archived: u64,
    pub deleted: u64,
}

/// Max ids per SQL `IN (...)` list — stays well below SQLite's 999 bind limit.
const ID_CHUNK: usize = 500;

/// Run the decay policy against a memory engine.
///
/// Phase 1: Select archive candidates (old + low-importance + non-semantic),
/// score each with Ebbinghaus retrievability in Rust, and archive the ones
/// below the threshold atomically (INSERT archive + FTS cleanup + DELETE).
/// Phase 2: Delete entries from archive past the delete threshold.
///
/// High-importance entries and entries with high retrievability (recently or
/// frequently accessed) are preserved regardless of age. Semantic-layer
/// memories are never archived.
pub async fn run_decay(engine: &SqliteMemoryEngine, policy: &MemoryDecayPolicy) -> DecayReport {
    let weights = engine.retrieval_weights.clone();
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
    let now = Utc::now();
    let archive_cutoff =
        (now - chrono::Duration::days(policy.archive_after_days as i64)).to_rfc3339();
    let delete_cutoff =
        (now - chrono::Duration::days(policy.delete_after_days as i64)).to_rfc3339();

    // Phase 1a: Fetch candidate rows (age + importance + layer filters in SQL),
    // then score retrievability in Rust — SQLite math functions are not
    // guaranteed, and this keeps the formula in one place.
    let candidates: Vec<String> = {
        let stmt = conn.prepare(
            "SELECT id, timestamp, last_accessed, access_count, importance
             FROM memories
             WHERE timestamp < ?1
               AND importance < ?2
               AND layer != 'semantic'",
        );
        let mut stmt = match stmt {
            Ok(s) => s,
            Err(e) => {
                warn!("Decay candidate query failed: {e}");
                return report;
            }
        };
        let rows = stmt.query_map(
            params![archive_cutoff, policy.min_importance_to_keep],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            },
        );
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                warn!("Decay candidate scan failed: {e}");
                return report;
            }
        };
        rows.filter_map(|r| r.ok())
            .filter_map(|(id, ts, last_accessed, access_count, importance)| {
                let anchor = last_accessed
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .or_else(|| DateTime::parse_from_rfc3339(&ts).ok())?
                    .with_timezone(&Utc);
                let days = (now - anchor).num_seconds().max(0) as f64 / 86_400.0;
                let r = ebbinghaus_retrievability(days, access_count, importance, &weights);
                (r < policy.min_retrievability).then_some(id)
            })
            .collect()
    };

    if candidates.is_empty() {
        // Nothing to archive — still run the delete phase below.
        report.archived = 0;
    } else {
        // Phase 1b: Archive by explicit id list, in chunks, atomically.
        // Using ids (not a repeated WHERE) guarantees INSERT/FTS/DELETE
        // operate on exactly the same rows.
        let phase1_result: std::result::Result<u64, String> = (|| {
            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| format!("BEGIN failed: {e}"))?;

            let mut archived_total: u64 = 0;
            for chunk in candidates.chunks(ID_CHUNK) {
                let placeholders: Vec<String> =
                    (1..=chunk.len()).map(|i| format!("?{i}")).collect();
                let in_list = placeholders.join(", ");
                let bind: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

                // INSERT OR IGNORE: tolerate re-runs if a previous attempt
                // archived but failed to delete (PRIMARY KEY conflict prevention).
                let archived = conn
                    .execute(
                        &format!(
                            "INSERT OR IGNORE INTO memories_archive
                                 (id, agent_id, content, timestamp, tags, layer,
                                  importance, access_count, last_accessed, source_event)
                             SELECT id, agent_id, content, timestamp, tags, layer,
                                    importance, access_count, last_accessed, source_event
                             FROM memories WHERE id IN ({in_list})"
                        ),
                        bind.as_slice(),
                    )
                    .map_err(|e| format!("Archive INSERT failed: {e}"))?;

                conn.execute(
                    &format!("DELETE FROM memories_fts WHERE memory_id IN ({in_list})"),
                    bind.as_slice(),
                )
                .map_err(|e| format!("FTS cleanup failed: {e}"))?;

                conn.execute(
                    &format!("DELETE FROM memories WHERE id IN ({in_list})"),
                    bind.as_slice(),
                )
                .map_err(|e| format!("Archive DELETE failed: {e}"))?;

                archived_total += archived as u64;
            }

            conn.execute_batch("COMMIT")
                .map_err(|e| format!("COMMIT failed: {e}"))?;

            Ok(archived_total)
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
    }

    // Phase 2: Delete very old archived entries.
    // Note: delete_cutoff applies to `archived_at` (when the record was archived),
    // not the original `timestamp` (when the memory was created).
    // A record survives: archive_after_days (in memories) + delete_after_days (in archive).
    // Wrapped in a transaction so a partial delete is rolled back on error.
    let phase2_result: std::result::Result<u64, String> = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("BEGIN phase2 failed: {e}"))?;
        let count = conn
            .execute(
                "DELETE FROM memories_archive WHERE archived_at < ?1",
                params![delete_cutoff],
            )
            .map_err(|e| {
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

        let policy = MemoryDecayPolicy::default();

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

    #[tokio::test]
    async fn decay_preserves_reinforced_memory() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "test";

        // Old + low-importance, but recalled 50 times, last time yesterday:
        // retrievability stays high → must survive.
        let mut reinforced = old_entry(agent, 100, 2.0);
        reinforced.access_count = 50;
        reinforced.last_accessed = Some(Utc::now() - Duration::days(1));
        engine.store(agent, reinforced).await.unwrap();

        // Old + low-importance, recalled twice but 80 days ago:
        // retrievability has decayed away → must be archived.
        // (The pre-Ebbinghaus policy kept ANY accessed memory forever.)
        let mut stale = old_entry(agent, 100, 2.0);
        stale.access_count = 2;
        stale.last_accessed = Some(Utc::now() - Duration::days(80));
        engine.store(agent, stale).await.unwrap();

        let policy = MemoryDecayPolicy::default();
        let report = run_decay(&engine, &policy).await;
        assert_eq!(report.archived, 1);

        let remaining = engine.list_recent(agent, 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].access_count, 50);
    }
}
