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

fn verify_live_io() -> std::result::Result<(), String> {
    crate::isolation_io::verify().map_err(|error| error.to_string())
}

struct AttachedDestination<'a>(&'a Connection);
impl Drop for AttachedDestination<'_> {
    fn drop(&mut self) {
        // Detach is cleanup, including after revocation or rollback.
        let _ = self.0.execute_batch("DETACH DATABASE hdst");
    }
}

/// How many rows moved, per physical table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReassignSummary {
    /// Rows moved out of `memories`.
    pub memories: u64,
    /// Rows moved out of `key_facts`.
    pub key_facts: u64,
    /// Rows moved out of `memories_archive` (0 when the table doesn't exist).
    pub archived: u64,
    /// Rows left in the source namespace because their lineage matches a
    /// source the target namespace has forgotten (P2-B), or because their id
    /// was forgotten in the destination database.
    pub fenced: u64,
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

/// Fill `temp.reassign_fenced` with the rows of `from` (in `main`) that must
/// stay behind: their lineage matches a tombstone of `to` in `fs`, or (for a
/// cross-database move) their id is forgotten in `forgotten`.
fn collect_fenced(
    conn: &Connection,
    from: &str,
    to: &str,
    fs: &str,
    forgotten: Option<&str>,
    has_archive: bool,
) -> std::result::Result<u64, String> {
    verify_live_io()?;
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS reassign_fenced (store TEXT NOT NULL, id TEXT NOT NULL,
                                                         PRIMARY KEY (store, id));
         DELETE FROM temp.reassign_fenced;",
    )
    .map_err(|e| format!("fence table: {e}"))?;
    let hit = crate::lineage::db::tombstone_match_sql(
        fs,
        "?2",
        "o.source_session",
        "o.source_message",
        "o.source_seq",
        "o.source_observed_at",
    );
    let mut sources: Vec<(&str, &str, &str)> = vec![
        ("memories", "main.memories", "memories"),
        ("key_facts", "main.key_facts", "key_facts"),
    ];
    if has_archive {
        sources.push(("archive", "main.memories_archive", "memories"));
    }
    for (label, table, store) in sources {
        let by_id = forgotten
            .map(|f| {
                format!(
                    " OR EXISTS (SELECT 1 FROM {f} g WHERE g.memory_store = '{store}' AND g.memory_id = t.id)"
                )
            })
            .unwrap_or_default();
        verify_live_io()?;
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO temp.reassign_fenced (store, id)
                 SELECT '{label}', t.id FROM {table} t
                 WHERE t.agent_id = ?1 AND (
                       EXISTS (SELECT 1 FROM main.memory_origins o
                               WHERE o.memory_store = '{store}' AND o.memory_id = t.id
                                 AND o.agent_id = ?1 AND {hit}){by_id})"
            ),
            params![from, to],
        )
        .map_err(|e| format!("fence check failed: {e}"))?;
    }
    conn.query_row("SELECT COUNT(*) FROM temp.reassign_fenced", [], |r| r.get::<_, i64>(0))
        .map(|n| n.max(0) as u64)
        .map_err(|e| format!("fence count failed: {e}"))
}

/// `AND <col> NOT IN (fenced ids of label)`.
fn not_fenced(col: &str, label: &str) -> String {
    format!(" AND {col} NOT IN (SELECT id FROM temp.reassign_fenced WHERE store = '{label}')")
}

/// In-place re-key: rewrite `agent_id` from `from_agent` to `to_agent` across
/// every table in this one database. Use when both agents share a memory DB.
///
/// P2-B: the rows' lineage moves with them, and `from_agent`'s source
/// tombstones are copied to `to_agent` (so the successor cannot relearn what
/// was forgotten). A row whose lineage matches a source `to_agent` has
/// forgotten stays under `from_agent` and is counted as `fenced`.
///
/// No-op-safe: re-running after the first move updates zero rows (nothing is
/// left tagged `from_agent` except fenced rows).
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
    crate::isolation_io::verify()?;
    let has_archive = archive_table_exists(&conn);

    let txn: std::result::Result<ReassignSummary, String> = (|| {
        verify_live_io()?;
        let transaction = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("BEGIN failed: {e}"))?;
        verify_live_io()?;
        let conn = &transaction;
        crate::lineage::db::copy_tombstones(
            &conn,
            "main.forgotten_sources",
            "main.forgotten_sources",
            from_agent,
            to_agent,
        )
        .map_err(|e| format!("tombstone copy failed: {e}"))?;
        let fenced = collect_fenced(
            &conn,
            from_agent,
            to_agent,
            "main.forgotten_sources",
            None,
            has_archive,
        )?;

        verify_live_io()?;
        let memories = conn
            .execute(
                &format!(
                    "UPDATE memories SET agent_id = ?1 WHERE agent_id = ?2{}",
                    not_fenced("id", "memories")
                ),
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories re-key failed: {e}"))? as u64;

        // memories_fts is a standalone fts5 table with an UNINDEXED agent_id
        // column that search filters on — it MUST be re-keyed too or the moved
        // rows become unfindable under the new agent.
        //
        // F6fb2 item 4: the FTS table carries no writer-gate trigger, so it
        // follows the base rows that actually moved — a row the gate skipped
        // (tombstoned / quarantined label, `RAISE(IGNORE)`) keeps its FTS row
        // under `from_agent`, and search under either label never sees the
        // other's content.
        verify_live_io()?;
        conn.execute(
            &format!(
                "UPDATE memories_fts SET agent_id = ?1 WHERE agent_id = ?2{}
                   AND memory_id IN (SELECT id FROM main.memories WHERE agent_id = ?1)",
                not_fenced("memory_id", "memories")
            ),
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("memories_fts re-key failed: {e}"))?;

        verify_live_io()?;
        let key_facts = conn
            .execute(
                &format!(
                    "UPDATE key_facts SET agent_id = ?1 WHERE agent_id = ?2{}",
                    not_fenced("id", "key_facts")
                ),
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("key_facts re-key failed: {e}"))? as u64;
        // key_facts_fts carries no agent_id and is keyed by an unchanged rowid,
        // so re-keying the base table is sufficient.

        let archived = if has_archive {
            verify_live_io()?;
        conn.execute(
                &format!(
                    "UPDATE memories_archive SET agent_id = ?1 WHERE agent_id = ?2{}",
                    not_fenced("id", "archive")
                ),
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories_archive re-key failed: {e}"))? as u64
        } else {
            0
        };

        // Lineage of every moved row (rows fenced above keep theirs).
        verify_live_io()?;
        conn.execute(
            "UPDATE memory_origins SET agent_id = ?1
             WHERE agent_id = ?2
               AND memory_id NOT IN (SELECT id FROM temp.reassign_fenced)",
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("lineage re-key failed: {e}"))?;

        verify_live_io()?;
        transaction.commit().map_err(|e| format!("COMMIT failed: {e}"))?;
        Ok(ReassignSummary {
            memories,
            key_facts,
            archived,
            fenced,
        })
    })();

    match txn {
        Ok(summary) => {
            // D3.1: re-keying moves triples between agents — invalidate both
            // agents' cached SPO graphs (this path bypasses per-agent bumps).
            drop(conn);
            engine.bump_graph_generation(from_agent);
            engine.bump_graph_generation(to_agent);
            Ok(summary)
        }
        Err(e) => {
            Err(DuDuClawError::Memory(format!("reassign_agent failed: {e}")))
        }
    }
}

/// Cross-database move: copy every row `from_agent` owns in *this* engine's DB
/// into the database at `to_db_path`, re-tagged as `to_agent`, then delete them
/// from the source. Used by the gateway's per-agent `memory.db` layout.
///
/// P2-B: inside the one transaction, `from_agent`'s source tombstones are
/// copied into the destination (as `to_agent`), rows whose id the destination
/// has forgotten or whose lineage matches a destination tombstone of
/// `to_agent` are not copied and stay in the source (`fenced`), and the
/// lineage of every moved row moves with it (the destination's insert trigger
/// re-checks each lineage row and each id).
///
/// The destination database must already exist with the current schema — open
/// it once via [`SqliteMemoryEngine::new`] (which runs the migrations) before
/// calling. Idempotent: after a successful move the source holds no `from_agent`
/// rows except fenced ones, so a re-run copies and deletes nothing.
pub async fn reassign_agent_cross_db(
    from_engine: &SqliteMemoryEngine,
    to_db_path: &Path,
    from_agent: &str,
    to_agent: &str,
) -> Result<ReassignSummary> {
    let conn = from_engine.conn_for_maintenance().await;
    crate::isolation_io::verify()?;
    let has_archive = archive_table_exists(&conn);

    // ATTACH cannot run inside a transaction — do it first. Scoped calls
    // require the documented existing-schema destination before this I/O;
    // unscoped host calls retain their previous attach/error behavior.
    crate::isolation_io::verify()?;
    if duduclaw_mcp_boundary::io_authority::is_scoped() && !to_db_path.is_file() {
        return Err(DuDuClawError::Memory("destination database must already exist".into()));
    }
    conn.execute(
        "ATTACH DATABASE ?1 AS hdst",
        params![to_db_path.to_string_lossy()],
    )
    .map_err(|e| DuDuClawError::Memory(format!("attach destination db failed: {e}")))?;
    let attached = AttachedDestination(&conn);

    let txn: std::result::Result<ReassignSummary, String> = (|| {
        verify_live_io()?;
        // Fail closed: a destination without the lineage tables (an older
        // schema) cannot hold tombstones, so nothing is moved.
        if !crate::lineage::db::table_exists(&conn, "hdst", "forgotten_sources")
            .map_err(|e| e.to_string())?
        {
            return Err("destination database has no lineage tables".to_string());
        }
        verify_live_io()?;
        let transaction = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("BEGIN failed: {e}"))?;
        verify_live_io()?;
        let conn = &transaction;
        // Destination needs the archive table before we can copy into it.
        if has_archive {
            verify_live_io()?;
            conn.execute_batch(
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
            ).map_err(|e| format!("destination archive schema: {e}"))?;
        }

        crate::lineage::db::copy_tombstones(
            &conn,
            "main.forgotten_sources",
            "hdst.forgotten_sources",
            from_agent,
            to_agent,
        )
        .map_err(|e| format!("tombstone copy failed: {e}"))?;
        let fenced = collect_fenced(
            &conn,
            from_agent,
            to_agent,
            "hdst.forgotten_sources",
            Some("hdst.forgotten_memories"),
            has_archive,
        )?;
        let nf_mem = not_fenced("id", "memories");
        let nf_facts = not_fenced("id", "key_facts");
        let nf_arch = not_fenced("id", "archive");

        // Copy memories into the destination re-tagged. A genuine id collision
        // (UUID clash) aborts here rather than silently skipping — honest fail.
        verify_live_io()?;
        let memories = conn
            .execute(
                &format!(
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
                 FROM main.memories WHERE agent_id = ?2{nf_mem}"
                ),
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories copy failed: {e}"))? as u64;

        // Destination FTS for exactly the copied rows.
        verify_live_io()?;
        conn.execute(
            &format!(
                "INSERT INTO hdst.memories_fts (content, agent_id, memory_id)
             SELECT content, ?1, id FROM main.memories WHERE agent_id = ?2{nf_mem}"
            ),
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("memories_fts copy failed: {e}"))?;

        verify_live_io()?;
        let key_facts = conn
            .execute(
                &format!(
                    "INSERT INTO hdst.key_facts
                    (id, agent_id, fact, channel, chat_id, source_session,
                     timestamp, access_count)
                 SELECT id, ?1, fact, channel, chat_id, source_session,
                        timestamp, access_count
                 FROM main.key_facts WHERE agent_id = ?2{nf_facts}"
                ),
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("key_facts copy failed: {e}"))? as u64;

        // key_facts_fts is rowid-linked; use the destination rowids of the rows
        // we just inserted (matched by their globally-unique ids).
        verify_live_io()?;
        conn.execute(
            &format!(
                "INSERT INTO hdst.key_facts_fts (rowid, fact)
             SELECT k.rowid, k.fact FROM hdst.key_facts AS k
             WHERE k.agent_id = ?1
               AND k.id IN (SELECT id FROM main.key_facts WHERE agent_id = ?2{nf_facts})"
            ),
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("key_facts_fts copy failed: {e}"))?;

        let archived = if has_archive {
            verify_live_io()?;
        conn.execute(
                &format!(
                    "INSERT INTO hdst.memories_archive
                    (id, agent_id, content, timestamp, tags, layer, importance,
                     access_count, last_accessed, source_event, archived_at)
                 SELECT id, ?1, content, timestamp, tags, layer, importance,
                        access_count, last_accessed, source_event, archived_at
                 FROM main.memories_archive WHERE agent_id = ?2{nf_arch}"
                ),
                params![to_agent, from_agent],
            )
            .map_err(|e| format!("memories_archive copy failed: {e}"))? as u64
        } else {
            0
        };

        // Lineage of the moved rows (the destination trigger re-checks it).
        let archived_lineage = |alias: &str, agent: &str| {
            if has_archive {
                format!(
                    "\n                    OR EXISTS (SELECT 1 FROM main.memories_archive a
                               WHERE a.id = {alias}.memory_id AND a.agent_id = {agent})"
                )
            } else {
                String::new()
            }
        };
        verify_live_io()?;
        conn.execute(
            &format!("INSERT OR IGNORE INTO hdst.memory_origins
                (memory_store, memory_id, agent_id, source_kind, source_session,
                 source_message, source_seq, source_observed_at, source_hash, role,
                 via_memory_id, created_at)
             SELECT memory_store, memory_id, ?1, source_kind, source_session,
                    source_message, source_seq, source_observed_at, source_hash, role,
                    via_memory_id, created_at
             FROM main.memory_origins o
             WHERE o.agent_id = ?2
               AND o.memory_id NOT IN (SELECT id FROM temp.reassign_fenced)
               AND (EXISTS (SELECT 1 FROM main.memories m
                            WHERE m.id = o.memory_id AND m.agent_id = ?2)
                    OR EXISTS (SELECT 1 FROM main.key_facts k
                               WHERE k.id = o.memory_id AND k.agent_id = ?2){}
                    )", archived_lineage("o", "?2")),
            params![to_agent, from_agent],
        )
        .map_err(|e| format!("lineage copy failed: {e}"))?;

        // Delete the moved rows from the source (FTS first to avoid orphans).
        verify_live_io()?;
        conn.execute(
            &format!(
                "DELETE FROM main.memories_fts WHERE agent_id = ?1{}",
                not_fenced("memory_id", "memories")
            ),
            params![from_agent],
        )
        .map_err(|e| format!("source memories_fts delete failed: {e}"))?;
        verify_live_io()?;
        conn.execute(
            &format!(
                "DELETE FROM main.memory_origins
             WHERE agent_id = ?1
               AND memory_id NOT IN (SELECT id FROM temp.reassign_fenced)
               AND (EXISTS (SELECT 1 FROM main.memories m
                            WHERE m.id = memory_origins.memory_id AND m.agent_id = ?1)
                    OR EXISTS (SELECT 1 FROM main.key_facts k
                               WHERE k.id = memory_origins.memory_id AND k.agent_id = ?1){}
                    )",
                archived_lineage("memory_origins", "?1")
            ),
            params![from_agent],
        )
        .map_err(|e| format!("source lineage delete failed: {e}"))?;
        verify_live_io()?;
        conn.execute(
            &format!("DELETE FROM main.memories WHERE agent_id = ?1{nf_mem}"),
            params![from_agent],
        )
        .map_err(|e| format!("source memories delete failed: {e}"))?;
        verify_live_io()?;
        conn.execute(
            &format!(
                "DELETE FROM main.key_facts_fts
             WHERE rowid IN (SELECT rowid FROM main.key_facts WHERE agent_id = ?1{nf_facts})"
            ),
            params![from_agent],
        )
        .map_err(|e| format!("source key_facts_fts delete failed: {e}"))?;
        verify_live_io()?;
        conn.execute(
            &format!("DELETE FROM main.key_facts WHERE agent_id = ?1{nf_facts}"),
            params![from_agent],
        )
        .map_err(|e| format!("source key_facts delete failed: {e}"))?;
        if has_archive {
            verify_live_io()?;
        conn.execute(
                &format!("DELETE FROM main.memories_archive WHERE agent_id = ?1{nf_arch}"),
                params![from_agent],
            )
            .map_err(|e| format!("source memories_archive delete failed: {e}"))?;
        }

        verify_live_io()?;
        transaction.commit().map_err(|e| format!("COMMIT failed: {e}"))?;
        Ok(ReassignSummary {
            memories,
            key_facts,
            archived,
            fenced,
        })
    })();

    let result = match txn {
        Ok(summary) => Ok(summary),
        Err(e) => {
            Err(DuDuClawError::Memory(format!(
                "reassign_agent_cross_db failed: {e}"
            )))
        }
    };
    // Always detach, even on failure, so the guard drops clean.
    drop(attached);
    drop(conn);
    // D3.1: the source engine lost all of `from_agent`'s rows — invalidate its
    // cached SPO graph. (The destination is a freshly-opened engine with no
    // live cache.)
    if result.is_ok() {
        from_engine.bump_graph_generation(from_agent);
    }
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
                }, crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        engine
            .store_temporal(
                agent,
                entry(&format!("{agent}-m2"), agent, "handoff subject beta note"),
                TemporalMeta::default(), crate::lineage::Provenance::test_only(),
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
                }, crate::lineage::Provenance::test_only(),
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
                }, crate::lineage::Provenance::test_only(),
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
    #[tokio::test]
    async fn revoked_lifecycle_calls_waiting_on_engine_mutex_do_not_attach_or_move() {
        use std::{future::Future,sync::{Arc,atomic::{AtomicBool,Ordering}},task::Poll};
        use duduclaw_mcp_boundary::io_authority;
        for cross in [false,true] {
            let dir=tempfile::tempdir().unwrap();
            let destination=dir.path().join("destination.db");
            let engine=SqliteMemoryEngine::in_memory().unwrap();
            seed(&engine,"alice").await;
            let live=Arc::new(AtomicBool::new(true));
            let held=engine.conn_for_maintenance().await;
            let work=io_authority::scope(crate::isolation_io::test_context(live.clone()),async {
                if cross { reassign_agent_cross_db(&engine,&destination,"alice","bob").await }
                else { reassign_agent(&engine,"alice","bob").await }
            });
            tokio::pin!(work);
            std::future::poll_fn(|cx|{assert!(work.as_mut().poll(cx).is_pending());Poll::Ready(())}).await;
            live.store(false,Ordering::SeqCst);drop(held);
            assert!(work.await.unwrap_err().to_string().contains("revoked"));
            assert!(!destination.exists(),"a refused call must not ATTACH/create its destination");
            assert_eq!(engine.list_recent("alice",10).await.unwrap().len(),2);
            assert!(engine.list_recent("bob",10).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn reassign_final_authority_refusal_rolls_back_both_databases_and_detaches() {
        use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}};
        use duduclaw_mcp_boundary::{io_authority,RequestContext,VerificationError};
        use duduclaw_isolation::capability::{CapabilityRecord,LiveRequestVerifier};
        struct Counted{count:Arc<AtomicUsize>,deny_at:usize}
        impl LiveRequestVerifier for Counted {
            fn verify_live(&self,_:&CapabilityRecord,_:u64)->std::result::Result<(),VerificationError>{
                if self.count.fetch_add(1,Ordering::SeqCst)+1>=self.deny_at{Err(VerificationError::Revoked)}else{Ok(())}
            }
        }
        let context=|count:Arc<AtomicUsize>,deny_at:usize| {
            let base=crate::isolation_io::test_context(Arc::new(AtomicBool::new(true)));
            RequestContext::with_live_verifier(base.record().clone(),Arc::new(Counted{count,deny_at}))
        };
        for cross in [false,true] {
            let dir=tempfile::tempdir().unwrap();
            let mut checks=0;
            for pass in 0..2 {
                let engine=SqliteMemoryEngine::in_memory().unwrap();seed(&engine,"alice").await;
                let destination=dir.path().join(format!("destination-{pass}.db"));
                drop(SqliteMemoryEngine::new(&destination).unwrap());
                let count=Arc::new(AtomicUsize::new(0));
                let result=io_authority::scope(context(count.clone(),if pass==0{usize::MAX}else{checks}),async {
                    if cross{reassign_agent_cross_db(&engine,&destination,"alice","bob").await}
                    else{reassign_agent(&engine,"alice","bob").await}
                }).await;
                if pass==0 {
                    assert_eq!(result.unwrap().memories,2);checks=count.load(Ordering::SeqCst);
                    assert!(checks>=8,"must check moved rows and final publication");
                } else {
                    assert!(result.unwrap_err().to_string().contains("revoked"));
                    assert_eq!(engine.list_recent("alice",10).await.unwrap().len(),2);
                    assert!(engine.list_recent("bob",10).await.unwrap().is_empty());
                    let conn=engine.conn_for_maintenance().await;
                    assert_eq!(conn.query_row("SELECT COUNT(*) FROM pragma_database_list WHERE name='hdst'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
                    drop(conn);
                    let dest=SqliteMemoryEngine::new(&destination).unwrap();
                    assert!(dest.list_recent("bob",10).await.unwrap().is_empty());
                }
            }
        }
    }

    #[tokio::test]
    async fn reassign_rechecks_authority_after_sqlite_begin_wait() {
        use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering},mpsc};
        use duduclaw_mcp_boundary::{io_authority,RequestContext,VerificationError};
        use duduclaw_isolation::capability::{CapabilityRecord,LiveRequestVerifier};
        struct AtBegin { live:Arc<AtomicBool>, checks:AtomicUsize, signal:std::sync::Mutex<Option<mpsc::Sender<()>>> }
        impl LiveRequestVerifier for AtBegin {
            fn verify_live(&self,_:&CapabilityRecord,_:u64)->std::result::Result<(),VerificationError>{
                if self.checks.fetch_add(1,Ordering::SeqCst)==1 {
                    if let Some(signal)=self.signal.lock().unwrap().take(){signal.send(()).unwrap();}
                }
                if self.live.load(Ordering::SeqCst){Ok(())}else{Err(VerificationError::Revoked)}
            }
        }
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("source.db");
        let engine=SqliteMemoryEngine::new(&path).unwrap();seed(&engine,"alice").await;
        let (held_tx,held_rx)=mpsc::channel();let (begin_tx,begin_rx)=mpsc::channel();
        let live=Arc::new(AtomicBool::new(true));let thread_live=live.clone();
        let writer=std::thread::spawn(move||{
            let conn=Connection::open(path).unwrap();conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            held_tx.send(()).unwrap();begin_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            thread_live.store(false,Ordering::SeqCst);conn.execute_batch("COMMIT").unwrap();
        });
        held_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let base=crate::isolation_io::test_context(live.clone());
        let context=RequestContext::with_live_verifier(base.record().clone(),Arc::new(AtBegin{live,checks:AtomicUsize::new(0),signal:std::sync::Mutex::new(Some(begin_tx))}));
        assert!(io_authority::scope(context,reassign_agent(&engine,"alice","bob")).await.unwrap_err().to_string().contains("revoked"));
        writer.join().unwrap();
        assert_eq!(engine.list_recent("alice",10).await.unwrap().len(),2);
        assert!(engine.list_recent("bob",10).await.unwrap().is_empty());
    }

}
