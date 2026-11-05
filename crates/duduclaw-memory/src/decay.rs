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
/// Both phases and archive schema initialization commit in one checked
/// transaction; any refusal rolls back the complete maintenance operation.
///
/// High-importance entries and entries with high retrievability (recently or
/// frequently accessed) are preserved regardless of age. Semantic-layer
/// memories are never archived.
pub async fn run_decay(engine: &SqliteMemoryEngine, policy: &MemoryDecayPolicy) -> DecayReport {
    let weights = engine.retrieval_weights.clone();
    let connection = engine.conn_for_maintenance().await;
    if let Err(error) = crate::isolation_io::verify() {
        warn!("Decay authority refused after connection wait: {error}");
        return DecayReport::default();
    }
    let transaction = match rusqlite::Transaction::new_unchecked(&connection, rusqlite::TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(error) => { warn!("Decay transaction unavailable: {error}"); return DecayReport::default(); }
    };
    if let Err(error) = crate::isolation_io::verify() {
        warn!("Decay authority refused after SQLite wait: {error}");
        return DecayReport::default();
    }
    let conn = &transaction;
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
                return DecayReport::default();
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
                return DecayReport::default();
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
            let mut archived_total: u64 = 0;
            for chunk in candidates.chunks(ID_CHUNK) {
                let placeholders: Vec<String> =
                    (1..=chunk.len()).map(|i| format!("?{i}")).collect();
                let in_list = placeholders.join(", ");
                let bind: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

                // INSERT OR IGNORE: tolerate re-runs if a previous attempt
                // archived but failed to delete (PRIMARY KEY conflict prevention).
                crate::isolation_io::verify().map_err(|e| e.to_string())?;
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

                crate::isolation_io::verify().map_err(|e| e.to_string())?;
                conn.execute(
                    &format!("DELETE FROM memories_fts WHERE memory_id IN ({in_list})"),
                    bind.as_slice(),
                )
                .map_err(|e| format!("FTS cleanup failed: {e}"))?;

                crate::isolation_io::verify().map_err(|e| e.to_string())?;
                conn.execute(
                    &format!("DELETE FROM memories WHERE id IN ({in_list})"),
                    bind.as_slice(),
                )
                .map_err(|e| format!("Archive DELETE failed: {e}"))?;

                archived_total += archived as u64;
            }
            Ok(archived_total)
        })();

        match phase1_result {
            Ok(count) => {
                report.archived = count;
            }
            Err(e) => {
                warn!("Archive phase failed: {e}");
                return DecayReport::default();
            }
        }
    }

    // Phase 2: Delete very old archived entries.
    // Note: delete_cutoff applies to `archived_at` (when the record was archived),
    // not the original `timestamp` (when the memory was created).
    // A record survives: archive_after_days (in memories) + delete_after_days (in archive).
    // Wrapped in a transaction so a partial delete is rolled back on error.
    let phase2_result: std::result::Result<u64, String> = (|| {
        crate::isolation_io::verify().map_err(|e| e.to_string())?;
        let count = conn
            .execute(
                "DELETE FROM memories_archive WHERE archived_at < ?1",
                params![delete_cutoff],
            )
            .map_err(|e| format!("Delete phase failed: {e}"))?;
        Ok(count as u64)
    })();

    match phase2_result {
        Ok(count) => {
            report.deleted = count;
        }
        Err(e) => {
            warn!("{e}");
            return DecayReport::default();
        }
    }

    if let Err(error) = crate::isolation_io::verify() {
        warn!("Decay publication refused: {error}");
        return DecayReport::default();
    }
    if let Err(error) = transaction.commit() {
        warn!("Decay commit failed: {error}");
        return DecayReport::default();
    }
    drop(connection);

    if report.archived > 0 || report.deleted > 0 {
        info!(
            archived = report.archived,
            deleted = report.deleted,
            "Memory decay completed"
        );
    }

    // D3.1: archival DELETEs rows from `memories` (dropping valid triples), so
    // any cached SPO graph is now stale. This maintenance path bypasses the
    // per-agent write bumps, so invalidate every cached agent graph.
    if report.archived > 0 {
        engine.invalidate_all_graph_caches();
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

#[cfg(test)]
mod isolation_tests {
    use super::*;
    use std::{future::Future, sync::{Arc, atomic::{AtomicBool,AtomicUsize,Ordering}}, task::Poll};
    use duduclaw_mcp_boundary::{io_authority,RequestContext,VerificationError};
    use duduclaw_isolation::capability::{CapabilityRecord,LiveRequestVerifier};
    use duduclaw_core::traits::MemoryEngine;
    struct Counted { count:Arc<AtomicUsize>, deny_at:usize }
    impl LiveRequestVerifier for Counted {
        fn verify_live(&self,_:&CapabilityRecord,_:u64)->Result<(),VerificationError> {
            if self.count.fetch_add(1,Ordering::SeqCst)+1 >= self.deny_at { Err(VerificationError::Revoked) } else { Ok(()) }
        }
    }
    fn counted(count:Arc<AtomicUsize>,deny_at:usize)->RequestContext {
        let base=crate::isolation_io::test_context(Arc::new(AtomicBool::new(true)));
        RequestContext::with_live_verifier(base.record().clone(),Arc::new(Counted{count,deny_at}))
    }
    async fn seed(engine:&SqliteMemoryEngine) {
        let memory=duduclaw_core::types::MemoryEntry {
            id:uuid::Uuid::new_v4().to_string(),agent_id:"test".into(),content:"old low value".into(),
            timestamp:Utc::now()-chrono::Duration::days(100),tags:vec![],embedding:None,
            layer:duduclaw_core::types::MemoryLayer::Episodic,importance:1.0,access_count:0,last_accessed:None,source_event:"test".into(),
        };
        engine.store("test",memory).await.unwrap();
    }
    #[tokio::test]
    async fn decay_revoked_at_mutex_wait_never_initializes_archive() {
        let engine=SqliteMemoryEngine::in_memory().unwrap();seed(&engine).await;
        let live=Arc::new(AtomicBool::new(true));
        let held=engine.conn_for_maintenance().await;
        let policy=MemoryDecayPolicy::default();
        let work=io_authority::scope(crate::isolation_io::test_context(live.clone()),run_decay(&engine,&policy));
        tokio::pin!(work);
        std::future::poll_fn(|cx|{assert!(work.as_mut().poll(cx).is_pending());Poll::Ready(())}).await;
        live.store(false,Ordering::SeqCst);drop(held);
        assert_eq!(work.await.archived,0);
        let conn=engine.conn_for_maintenance().await;
        let archive:i64=conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='memories_archive'",[],|r|r.get(0)).unwrap();
        assert_eq!(archive,0);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM memories",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
    #[tokio::test]
    async fn decay_final_authority_refusal_rolls_back_archive_fts_delete_and_schema() {
        let policy=MemoryDecayPolicy::default();
        let probe=SqliteMemoryEngine::in_memory().unwrap();seed(&probe).await;
        let count=Arc::new(AtomicUsize::new(0));
        assert_eq!(io_authority::scope(counted(count.clone(),usize::MAX),run_decay(&probe,&policy)).await.archived,1);
        let last=count.load(Ordering::SeqCst);
        assert!(last>=6,"must check every archive effect and final publication");
        let engine=SqliteMemoryEngine::in_memory().unwrap();seed(&engine).await;
        let report=io_authority::scope(counted(Arc::new(AtomicUsize::new(0)),last),run_decay(&engine,&policy)).await;
        assert_eq!((report.archived,report.deleted),(0,0));
        let conn=engine.conn_for_maintenance().await;
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM memories",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM memories_fts",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='memories_archive'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
}
