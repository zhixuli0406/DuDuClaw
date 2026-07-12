//! Agent-lifecycle memory operations — re-key an agent's memories to a
//! successor (WP4 hand-off).
//!
//! Two flavours, both built on the live [`SqliteMemoryEngine`] through its
//! public `conn_for_maintenance()` guard; **no schema change**:
//!
//! * [`reassign_agent`] — in-place re-key **within one database** (`UPDATE
//!   agent_id`). This is the primitive the WP4 spec names: it rewrites the
//!   `agent_id` column across every physical table that carries it (`memories`,
//!   `memories_fts`, `key_facts`, and `memories_archive` when present) inside a
//!   single `BEGIN IMMEDIATE` transaction so FTS and base rows never diverge.
//!   The temporal columns (`valid_from`/`valid_until`/`superseded_by`/…) and the
//!   supersession chains store **memory ids**, not agent ids — moving every row
//!   wholesale keeps each chain internally consistent without touching them.
//!
//! * [`reassign_agent_cross_db`] — move rows between **two separate database
//!   files** (the per-agent `agents/<id>/memory.db` layout the gateway uses).
//!   `ATTACH`es the destination, copies `from`'s rows in as `to`, rebuilds the
//!   destination FTS for exactly those rows, then deletes them from the source —
//!   all in one transaction. Re-running after success is a no-op (the source is
//!   already empty), so hand-off is idempotent.
//!
//! Both return a [`ReassignSummary`] with per-table move counts. A genuine
//! primary-key collision (astronomically unlikely with UUID ids) aborts the
//! transaction and surfaces as an error rather than a silent partial move.

use crate::engine::SqliteMemoryEngine;
use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{params, Connection};
use std::path::Path;

/// How many rows moved, per physical table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReassignSummary {
    /// Rows moved out of `memories`.
    pub memories: u64,
    /// Rows moved out of `key_facts`.
    pub key_facts: u64,
    /// Rows moved out of `memories_archive` (0 when the table doesn't exist).
    pub archived: u64,
}

impl ReassignSummary {
    /// Total rows touched across every table.
    pub fn total(&self) -> u64 {
        self.memories + self.key_facts + self.archived
    }
}

/// Whether `memories_archive` exists in the connection's main schema.
fn archive_table_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories_archive'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// In-place re-key: rewrite `agent_id` from `from_agent` to `to_agent` across
/// every table in this one database. Use when both agents share a memory DB.
///
/// No-op-safe: re-running after the first move updates zero rows (nothing is
/// left tagged `from_agent`).
pub async fn reassign_agent(
    engine: &SqliteMemoryEngine,
    from_agent: &str,
    to_agent: &str,
) -> Result<ReassignSummary> {
    if from_agent == to_agent {
        return Err(DuDuClawError::Memory(
            "reassign_agent: from and to are identical".to_string(),
        ));
    }
    let conn = engine.conn_for_maintenance().await;
    let has_archive = archive_table_exists(&conn);

    let txn: std::result::Result<ReassignSummary, String> = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("BEGIN failed: {e}"))?;

        let memories = conn
            .execute(
                "UPDATE memories SET agent_id = ?1 WHERE agent_id = ?2",
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories re-key failed: {e}"))? as u64;

        // memories_fts is a standalone fts5 table with an UNINDEXED agent_id
        // column that search filters on — it MUST be re-keyed too or the moved
        // rows become unfindable under the new agent.
        conn.execute(
            "UPDATE memories_fts SET agent_id = ?1 WHERE agent_id = ?2",
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("memories_fts re-key failed: {e}"))?;

        let key_facts = conn
            .execute(
                "UPDATE key_facts SET agent_id = ?1 WHERE agent_id = ?2",
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("key_facts re-key failed: {e}"))? as u64;
        // key_facts_fts carries no agent_id and is keyed by an unchanged rowid,
        // so re-keying the base table is sufficient.

        let archived = if has_archive {
            conn.execute(
                "UPDATE memories_archive SET agent_id = ?1 WHERE agent_id = ?2",
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories_archive re-key failed: {e}"))? as u64
        } else {
            0
        };

        conn.execute_batch("COMMIT")
            .map_err(|e| format!("COMMIT failed: {e}"))?;
        Ok(ReassignSummary {
            memories,
            key_facts,
            archived,
        })
    })();

    match txn {
        Ok(summary) => Ok(summary),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(DuDuClawError::Memory(format!("reassign_agent failed: {e}")))
        }
    }
}

/// Cross-database move: copy every row `from_agent` owns in *this* engine's DB
/// into the database at `to_db_path`, re-tagged as `to_agent`, then delete them
/// from the source. Used by the gateway's per-agent `memory.db` layout.
///
/// The destination database must already exist with the current schema — open
/// it once via [`SqliteMemoryEngine::new`] (which runs the migrations) before
/// calling. Idempotent: after a successful move the source holds no `from_agent`
/// rows, so a re-run copies and deletes nothing.
pub async fn reassign_agent_cross_db(
    from_engine: &SqliteMemoryEngine,
    to_db_path: &Path,
    from_agent: &str,
    to_agent: &str,
) -> Result<ReassignSummary> {
    let conn = from_engine.conn_for_maintenance().await;
    let has_archive = archive_table_exists(&conn);

    // ATTACH cannot run inside a transaction — do it first.
    conn.execute(
        "ATTACH DATABASE ?1 AS hdst",
        params![to_db_path.to_string_lossy()],
    )
    .map_err(|e| DuDuClawError::Memory(format!("attach destination db failed: {e}")))?;
    // Destination needs the archive table before we can copy into it.
    if has_archive {
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS hdst.memories_archive (
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
        );
    }

    let txn: std::result::Result<ReassignSummary, String> = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("BEGIN failed: {e}"))?;

        // Copy memories into the destination re-tagged. A genuine id collision
        // (UUID clash) aborts here rather than silently skipping — honest fail.
        let memories = conn
            .execute(
                "INSERT INTO hdst.memories
                    (id, agent_id, content, timestamp, tags, created_at, layer,
                     importance, access_count, last_accessed, source_event,
                     valid_from, valid_until, superseded_by, supersedes, subject,
                     predicate, object, confidence, metadata, origin, origin_trust,
                     derived_from, embedding, embedding_model)
                 SELECT id, ?1, content, timestamp, tags, created_at, layer,
                        importance, access_count, last_accessed, source_event,
                        valid_from, valid_until, superseded_by, supersedes, subject,
                        predicate, object, confidence, metadata, origin, origin_trust,
                        derived_from, embedding, embedding_model
                 FROM main.memories WHERE agent_id = ?2",
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories copy failed: {e}"))? as u64;

        // Destination FTS for exactly the copied rows.
        conn.execute(
            "INSERT INTO hdst.memories_fts (content, agent_id, memory_id)
             SELECT content, ?1, id FROM main.memories WHERE agent_id = ?2",
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("memories_fts copy failed: {e}"))?;

        let key_facts = conn
            .execute(
                "INSERT INTO hdst.key_facts
                    (id, agent_id, fact, channel, chat_id, source_session,
                     timestamp, access_count)
                 SELECT id, ?1, fact, channel, chat_id, source_session,
                        timestamp, access_count
                 FROM main.key_facts WHERE agent_id = ?2",
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("key_facts copy failed: {e}"))? as u64;

        // key_facts_fts is rowid-linked; use the destination rowids of the rows
        // we just inserted (matched by their globally-unique ids).
        conn.execute(
            "INSERT INTO hdst.key_facts_fts (rowid, fact)
             SELECT k.rowid, k.fact FROM hdst.key_facts AS k
             WHERE k.agent_id = ?1
               AND k.id IN (SELECT id FROM main.key_facts WHERE agent_id = ?2)",
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("key_facts_fts copy failed: {e}"))?;

        let archived = if has_archive {
            conn.execute(
                "INSERT INTO hdst.memories_archive
                    (id, agent_id, content, timestamp, tags, layer, importance,
                     access_count, last_accessed, source_event, archived_at)
                 SELECT id, ?1, content, timestamp, tags, layer, importance,
                        access_count, last_accessed, source_event, archived_at
                 FROM main.memories_archive WHERE agent_id = ?2",
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories_archive copy failed: {e}"))? as u64
        } else {
            0
        };

        // Delete the moved rows from the source (FTS first to avoid orphans).
        conn.execute(
            "DELETE FROM main.memories_fts WHERE agent_id = ?1",
            params![from_agent],
        )
        .map_err(|e| format!("source memories_fts delete failed: {e}"))?;
        conn.execute(
            "DELETE FROM main.memories WHERE agent_id = ?1",
            params![from_agent],
        )
        .map_err(|e| format!("source memories delete failed: {e}"))?;
        conn.execute(
            "DELETE FROM main.key_facts_fts
             WHERE rowid IN (SELECT rowid FROM main.key_facts WHERE agent_id = ?1)",
            params![from_agent],
        )
        .map_err(|e| format!("source key_facts_fts delete failed: {e}"))?;
        conn.execute(
            "DELETE FROM main.key_facts WHERE agent_id = ?1",
            params![from_agent],
        )
        .map_err(|e| format!("source key_facts delete failed: {e}"))?;
        if has_archive {
            conn.execute(
                "DELETE FROM main.memories_archive WHERE agent_id = ?1",
                params![from_agent],
            )
            .map_err(|e| format!("source memories_archive delete failed: {e}"))?;
        }

        conn.execute_batch("COMMIT")
            .map_err(|e| format!("COMMIT failed: {e}"))?;
        Ok(ReassignSummary {
            memories,
            key_facts,
            archived,
        })
    })();

    let result = match txn {
        Ok(summary) => Ok(summary),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(DuDuClawError::Memory(format!(
                "reassign_agent_cross_db failed: {e}"
            )))
        }
    };
    // Always detach, even on failure, so the guard drops clean.
    let _ = conn.execute_batch("DETACH DATABASE hdst");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TemporalMeta;
    use chrono::Utc;
    use duduclaw_core::traits::MemoryEngine;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};

    fn entry(id: &str, agent: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: String::new(),
        }
    }

    async fn seed(engine: &SqliteMemoryEngine, agent: &str) {
        engine
            .store_temporal(
                agent,
                entry(&format!("{agent}-m1"), agent, "handoff subject alpha"),
                TemporalMeta {
                    subject: Some("user:alice".into()),
                    predicate: Some("prefers".into()),
                    object: Some("tea".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        engine
            .store_temporal(
                agent,
                entry(&format!("{agent}-m2"), agent, "handoff subject beta note"),
                TemporalMeta::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reassign_rekeys_memories_and_fts() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "alice").await;

        // Precondition: alice can find her memory, bob cannot.
        assert_eq!(
            engine.search("alice", "handoff", 10).await.unwrap().len(),
            2,
            "alice sees her rows"
        );
        assert_eq!(
            engine.search("bob", "handoff", 10).await.unwrap().len(),
            0,
            "bob has none yet"
        );

        let summary = reassign_agent(&engine, "alice", "bob").await.unwrap();
        assert_eq!(summary.memories, 2);

        // FTS must follow: bob now finds them, alice finds nothing.
        assert_eq!(
            engine.search("bob", "handoff", 10).await.unwrap().len(),
            2,
            "bob inherits via FTS"
        );
        assert_eq!(
            engine.search("alice", "handoff", 10).await.unwrap().len(),
            0,
            "alice no longer sees re-keyed rows"
        );
        // list_recent (base-table path) agrees.
        assert_eq!(engine.list_recent("bob", 10).await.unwrap().len(), 2);
        assert_eq!(engine.list_recent("alice", 10).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn reassign_is_idempotent() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "alice").await;

        let first = reassign_agent(&engine, "alice", "bob").await.unwrap();
        assert_eq!(first.memories, 2);
        let second = reassign_agent(&engine, "alice", "bob").await.unwrap();
        assert_eq!(second.memories, 0, "re-run moves nothing");
        assert_eq!(engine.search("bob", "handoff", 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn reassign_rejects_same_agent() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "alice").await;
        assert!(reassign_agent(&engine, "alice", "alice").await.is_err());
    }

    #[tokio::test]
    async fn reassign_preserves_temporal_chain() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        // Two facts on the same (subject,predicate) → supersession chain.
        engine
            .store_temporal(
                "alice",
                entry("f1", "alice", "alice lives in Taipei"),
                TemporalMeta {
                    subject: Some("user:alice".into()),
                    predicate: Some("lives_in".into()),
                    object: Some("Taipei".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        engine
            .store_temporal(
                "alice",
                entry("f2", "alice", "alice lives in Tainan"),
                TemporalMeta {
                    subject: Some("user:alice".into()),
                    predicate: Some("lives_in".into()),
                    object: Some("Tainan".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        reassign_agent(&engine, "alice", "bob").await.unwrap();
        // Only the current fact is valid for bob; the chain survived the move.
        let hist = engine.get_history("bob", "user:alice", "lives_in").await.unwrap();
        assert_eq!(hist.len(), 2, "both chain links carried over to bob");
    }

    #[tokio::test]
    async fn cross_db_moves_between_files() {
        let dir = tempfile::tempdir().unwrap();
        let from_db = dir.path().join("from.db");
        let to_db = dir.path().join("to.db");

        let from_engine = SqliteMemoryEngine::new(&from_db).unwrap();
        seed(&from_engine, "alice").await;
        // Ensure destination schema exists (as the gateway does).
        drop(SqliteMemoryEngine::new(&to_db).unwrap());

        let summary = reassign_agent_cross_db(&from_engine, &to_db, "alice", "bob")
            .await
            .unwrap();
        assert_eq!(summary.memories, 2);

        // Source no longer has the rows.
        assert_eq!(
            from_engine.search("alice", "handoff", 10).await.unwrap().len(),
            0
        );
        // Destination (re-opened) finds them under bob, via FTS and base table.
        let to_engine = SqliteMemoryEngine::new(&to_db).unwrap();
        assert_eq!(to_engine.search("bob", "handoff", 10).await.unwrap().len(), 2);
        assert_eq!(to_engine.list_recent("bob", 10).await.unwrap().len(), 2);

        // Idempotent re-run.
        let again = reassign_agent_cross_db(&from_engine, &to_db, "alice", "bob")
            .await
            .unwrap();
        assert_eq!(again.memories, 0);
    }
}
