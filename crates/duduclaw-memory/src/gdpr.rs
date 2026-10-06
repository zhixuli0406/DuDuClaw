//! GDPR data-subject request helpers — export / erase by contact.
//!
//! Built on the live [`SqliteMemoryEngine`] through its public
//! `conn_for_maintenance()` guard; **no schema change**. A "contact" is matched
//! three ways: as the structured `subject` or `object` of a temporal triple, or
//! as a free-text mention in `content` / `fact` (LIKE, wildcard-escaped so an id
//! containing `%`/`_` matches literally).
//!
//! `gdpr_export` returns a JSON bundle the caller writes to disk. `gdpr_erase`
//! deletes transactionally across all five physical tables
//! (`memories` + `memories_fts` + `key_facts` + `key_facts_fts` +
//! `memories_archive`) so **no FTS orphan is left behind** (a hazard the
//! pre-existing `purge_stale_facts` path does not guard against) and no
//! decay-archived copy of the person's data survives, then records a tombstone
//! memory documenting the erasure — the retained legal-basis record that the
//! request was fulfilled.
//!
//! `memories_archive` (created lazily by decay and `forget`) keeps only the
//! row's content, not its triple or metadata, so an archived row matches by
//! the free-text arm of the rule (`content` LIKE the contact) or by an id the
//! live match selected (a row decay archived but did not delete).
//!
//! Erase is a hard delete by design (right-to-erasure ⇒ the data is gone, not
//! merely `valid_until`-closed). Callers gate it behind an explicit `--confirm`
//! and should show the `gdpr_export` bundle first.

use crate::engine::SqliteMemoryEngine;
use chrono::Utc;
use duduclaw_core::error::{DuDuClawError, Result};
use serde_json::{json, Value};

/// SQLite IN-list chunk size (stays well under the 999 bound variable limit).
const ID_CHUNK: usize = 400;

/// Shortest contact (in characters, after trimming) export and erase accept.
/// The contact becomes a `%<contact>%` LIKE pattern, so an empty contact would
/// match every row of the employee and a one- or two-character one nearly so.
pub const MIN_CONTACT_CHARS: usize = 3;

/// Validate a data-subject contact and return it trimmed. Refuses a blank
/// contact or one shorter than [`MIN_CONTACT_CHARS`] characters (counted as
/// `chars()`, not bytes), because the free-text arm of the match would then
/// select most or all of the employee's memories.
pub fn validate_contact(contact: &str) -> Result<&str> {
    let trimmed = contact.trim();
    if trimmed.is_empty() {
        return Err(DuDuClawError::Memory(
            "gdpr: the contact is empty; give the person's identifier (at least 3 characters after trimming spaces)".to_string(),
        ));
    }
    let n = trimmed.chars().count();
    if n < MIN_CONTACT_CHARS {
        return Err(DuDuClawError::Memory(format!(
            "gdpr: the contact `{trimmed}` has {n} character(s); at least {MIN_CONTACT_CHARS} are required after trimming spaces, because a shorter text would match unrelated memories"
        )));
    }
    Ok(trimmed)
}

/// Escape LIKE metacharacters so the contact id is matched literally under an
/// `ESCAPE '\'` clause (prevents a `%`/`_` in the id from becoming a wildcard).
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// The memory-row predicate for "references the contact" (`?2` = contact,
/// `?3` = escaped LIKE pattern): the triple's subject/object, a free-text
/// mention, or — for a claim held for trust review, whose triple lives only in
/// `metadata.held_claim` until a reviewer accepts it — the held subject/object
/// (M5: an erased contact's pending claim must not survive to be approved).
const CONTACT_MATCH: &str = "(subject = ?2 OR object = ?2 OR content LIKE ?3 ESCAPE '\\'
      OR (CASE WHEN json_valid(metadata)
               THEN json_extract(metadata, '$.held_claim.subject') END) = ?2
      OR (CASE WHEN json_valid(metadata)
               THEN json_extract(metadata, '$.held_claim.object') END) = ?2)";

/// The archive-row predicate (`?1` = agent, `?2` = escaped LIKE pattern). The
/// archive keeps only `content`, so the free-text arm is all of the live rule
/// that can still be evaluated there.
const ARCHIVE_CONTACT_MATCH: &str = "agent_id = ?1 AND content LIKE ?2 ESCAPE '\\'";

/// Outcome of an erase: how many rows were removed and the tombstone id (if one
/// was written).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GdprEraseSummary {
    pub contact: String,
    pub memories_deleted: u64,
    pub key_facts_deleted: u64,
    /// Decay/forget archive copies removed from `memories_archive`.
    pub archived_memories_deleted: u64,
    pub tombstone_id: Option<String>,
    /// Ids of the memory rows deleted (live and archived) — the caller scrubs
    /// review cards and events that reference them (R-M3).
    #[serde(skip_serializing)]
    pub erased_memory_ids: Vec<String>,
}

/// Whether the lazily created `memories_archive` table exists.
fn archive_exists(conn: &rusqlite::Connection) -> Result<bool> {
    crate::lineage::db::table_exists(conn, "main", "memories_archive")
}

/// Ids of the archived rows of `agent_id` that mention the contact.
fn archived_ids(conn: &rusqlite::Connection, agent_id: &str, like: &str) -> Result<Vec<String>> {
    if !archive_exists(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&format!("SELECT id FROM memories_archive WHERE {ARCHIVE_CONTACT_MATCH}"))
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let rows = stmt
        .query_map(rusqlite::params![agent_id, like], |r| r.get::<_, String>(0))
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let mut v = Vec::new();
    for row in rows {
        v.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
    }
    Ok(v)
}

/// Aggregate every stored row referencing `contact` into a JSON bundle
/// (memories with full temporal/provenance columns + key facts). Read-only.
pub async fn gdpr_export(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    contact: &str,
) -> Result<Value> {
    let contact = validate_contact(contact)?;
    let conn = engine.conn_for_maintenance().await;
    let like = format!("%{}%", like_escape(contact));

    let mut mem_stmt = conn
        .prepare(&format!(
            "SELECT id, content, layer, timestamp, tags, subject, predicate, object,
                    valid_from, valid_until, superseded_by, supersedes, confidence,
                    origin, origin_trust
             FROM memories
             WHERE agent_id = ?1 AND {CONTACT_MATCH}
             ORDER BY COALESCE(valid_from, timestamp) ASC"
        ))
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let mem_rows = mem_stmt
        .query_map(rusqlite::params![agent_id, contact, like], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "content": r.get::<_, String>(1)?,
                "layer": r.get::<_, Option<String>>(2)?,
                "timestamp": r.get::<_, Option<String>>(3)?,
                "tags": r.get::<_, Option<String>>(4)?,
                "subject": r.get::<_, Option<String>>(5)?,
                "predicate": r.get::<_, Option<String>>(6)?,
                "object": r.get::<_, Option<String>>(7)?,
                "valid_from": r.get::<_, Option<String>>(8)?,
                "valid_until": r.get::<_, Option<String>>(9)?,
                "superseded_by": r.get::<_, Option<String>>(10)?,
                "supersedes": r.get::<_, Option<String>>(11)?,
                "confidence": r.get::<_, Option<f64>>(12)?,
                "origin": r.get::<_, Option<String>>(13)?,
                "origin_trust": r.get::<_, Option<f64>>(14)?,
            }))
        })
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let mut memories = Vec::new();
    for row in mem_rows {
        memories.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
    }

    let mut kf_stmt = conn
        .prepare(
            "SELECT id, fact, channel, chat_id, source_session, timestamp
             FROM key_facts
             WHERE agent_id = ?1 AND fact LIKE ?2 ESCAPE '\\'
             ORDER BY timestamp ASC",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let kf_rows = kf_stmt
        .query_map(rusqlite::params![agent_id, like], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "fact": r.get::<_, String>(1)?,
                "channel": r.get::<_, Option<String>>(2)?,
                "chat_id": r.get::<_, Option<String>>(3)?,
                "source_session": r.get::<_, Option<String>>(4)?,
                "timestamp": r.get::<_, Option<String>>(5)?,
            }))
        })
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let mut key_facts = Vec::new();
    for row in kf_rows {
        key_facts.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
    }

    let mut archived_memories = Vec::new();
    if archive_exists(&conn)? {
        let mut arch_stmt = conn
            .prepare(&format!(
                "SELECT id, content, layer, timestamp, tags, importance, source_event, archived_at
                 FROM memories_archive
                 WHERE {ARCHIVE_CONTACT_MATCH}
                 ORDER BY timestamp ASC"
            ))
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let arch_rows = arch_stmt
            .query_map(rusqlite::params![agent_id, like], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "content": r.get::<_, String>(1)?,
                    "layer": r.get::<_, Option<String>>(2)?,
                    "timestamp": r.get::<_, Option<String>>(3)?,
                    "tags": r.get::<_, Option<String>>(4)?,
                    "importance": r.get::<_, Option<f64>>(5)?,
                    "source_event": r.get::<_, Option<String>>(6)?,
                    "archived_at": r.get::<_, Option<String>>(7)?,
                }))
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        for row in arch_rows {
            archived_memories.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
    }

    Ok(json!({
        "contact": contact,
        "agent_id": agent_id,
        "exported_at": Utc::now().to_rfc3339(),
        "counts": {
            "memories": memories.len(),
            "key_facts": key_facts.len(),
            "archived_memories": archived_memories.len(),
        },
        "memories": memories,
        "key_facts": key_facts,
        "archived_memories": archived_memories,
    }))
}

/// Hard-delete every row referencing `contact` across all four physical tables
/// in one `BEGIN IMMEDIATE` transaction, then (when `tombstone`) record a
/// content-free-ish erasure marker. Returns per-table deletion counts.
pub async fn gdpr_erase(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    contact: &str,
    tombstone: bool,
) -> Result<GdprEraseSummary> {
    let contact = validate_contact(contact)?;
    let conn = engine.conn_for_maintenance().await;
    let like = format!("%{}%", like_escape(contact));

    // Collect the exact ids/rowids first so the FTS delete and the base-table
    // delete operate on identical rows (mirrors the decay janitor's contract).
    let mem_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                &format!("SELECT id FROM memories WHERE agent_id = ?1 AND {CONTACT_MATCH}"),
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![agent_id, contact, like], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut v = Vec::new();
        for row in rows {
            v.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        v
    };
    let kf_rowids: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT rowid FROM key_facts
                 WHERE agent_id = ?1 AND fact LIKE ?2 ESCAPE '\\'",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![agent_id, like], |r| r.get::<_, i64>(0))
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut v = Vec::new();
        for row in rows {
            v.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        v
    };

    let archive_ids = archived_ids(&conn, agent_id, &like)?;
    let has_archive = archive_exists(&conn)?;

    let now = Utc::now().to_rfc3339();
    let tombstone_id = if tombstone {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        None
    };

    let txn: std::result::Result<(u64, u64, u64), String> = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("BEGIN failed: {e}"))?;

        let mut mem_deleted: u64 = 0;
        for chunk in mem_ids.chunks(ID_CHUNK) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let in_list = placeholders.join(", ");
            let bind: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            conn.execute(
                &format!("DELETE FROM memories_fts WHERE memory_id IN ({in_list})"),
                bind.as_slice(),
            )
            .map_err(|e| format!("memories_fts delete failed: {e}"))?;
            let n = conn
                .execute(
                    &format!("DELETE FROM memories WHERE id IN ({in_list})"),
                    bind.as_slice(),
                )
                .map_err(|e| format!("memories delete failed: {e}"))?;
            mem_deleted += n as u64;
        }

        let mut kf_deleted: u64 = 0;
        for chunk in kf_rowids.chunks(ID_CHUNK) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let in_list = placeholders.join(", ");
            let bind: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            conn.execute(
                &format!("DELETE FROM key_facts_fts WHERE rowid IN ({in_list})"),
                bind.as_slice(),
            )
            .map_err(|e| format!("key_facts_fts delete failed: {e}"))?;
            let n = conn
                .execute(
                    &format!("DELETE FROM key_facts WHERE rowid IN ({in_list})"),
                    bind.as_slice(),
                )
                .map_err(|e| format!("key_facts delete failed: {e}"))?;
            kf_deleted += n as u64;
        }

        // Archived copies: the rows matched in the archive itself, plus any
        // archive row carrying an id the live match selected (decay inserts
        // with OR IGNORE, so a row can sit in both tables after a failed run).
        let mut arch_deleted: u64 = 0;
        if has_archive {
            for ids in [&archive_ids, &mem_ids] {
                for chunk in ids.chunks(ID_CHUNK) {
                    let placeholders: Vec<String> =
                        (2..=chunk.len() + 1).map(|i| format!("?{i}")).collect();
                    let in_list = placeholders.join(", ");
                    let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&agent_id];
                    bind.extend(chunk.iter().map(|id| id as &dyn rusqlite::ToSql));
                    let n = conn
                        .execute(
                            &format!(
                                "DELETE FROM memories_archive WHERE agent_id = ?1 AND id IN ({in_list})"
                            ),
                            bind.as_slice(),
                        )
                        .map_err(|e| format!("memories_archive delete failed: {e}"))?;
                    arch_deleted += n as u64;
                }
            }
        }

        // Erasure record (legal-basis audit): a Semantic memory noting the
        // request was fulfilled, with counts — kept intentionally after the
        // delete so it is not itself removed. The contact is stored as a
        // SHA-256 pseudonym, never the raw identifier: (1) data minimisation —
        // the personal id must not survive an erasure request, and (2) it keeps
        // the record from re-matching a future export/erase of the same contact
        // (raw-id LIKE cannot match the digest).
        if let Some(ref tid) = tombstone_id {
            let contact_hash = {
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(contact.as_bytes());
                digest[..8]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            };
            let content = format!(
                "[gdpr-erasure] contact_sha256={contact_hash} removed memories={mem_deleted} key_facts={kf_deleted} archived={arch_deleted} at={now}"
            );
            conn.execute(
                "INSERT INTO memories
                    (id, agent_id, content, timestamp, tags, layer, importance,
                     source_event, subject, predicate, valid_from)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'semantic', 3.0,
                         'gdpr_erasure', ?6, 'erasure_event', ?4)",
                rusqlite::params![
                    tid,
                    agent_id,
                    content,
                    now,
                    "[\"gdpr-erasure\"]",
                    "gdpr:erasure",
                ],
            )
            .map_err(|e| format!("tombstone insert failed: {e}"))?;
            conn.execute(
                "INSERT INTO memories_fts (content, agent_id, memory_id) VALUES (?1, ?2, ?3)",
                rusqlite::params![content, agent_id, tid],
            )
            .map_err(|e| format!("tombstone fts insert failed: {e}"))?;
        }

        conn.execute_batch("COMMIT")
            .map_err(|e| format!("COMMIT failed: {e}"))?;
        Ok((mem_deleted, kf_deleted, arch_deleted))
    })();

    match txn {
        Ok((memories_deleted, key_facts_deleted, archived_memories_deleted)) => {
            // D3.1: erasure deletes rows (and may insert a tombstone triple) —
            // invalidate this agent's cached SPO graph. This maintenance path
            // bypasses the per-agent write bumps in the engine.
            if memories_deleted > 0 || tombstone_id.is_some() {
                drop(conn);
                engine.bump_graph_generation(agent_id);
            }
            let live: std::collections::HashSet<String> = mem_ids.iter().cloned().collect();
            let mut erased_memory_ids = mem_ids;
            erased_memory_ids.extend(archive_ids.into_iter().filter(|id| !live.contains(id)));
            Ok(GdprEraseSummary {
                contact: contact.to_string(),
                memories_deleted,
                key_facts_deleted,
                archived_memories_deleted,
                tombstone_id,
                erased_memory_ids,
            })
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(DuDuClawError::Memory(format!("gdpr erase failed: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SqliteMemoryEngine;
    use crate::TemporalMeta;
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
        // Ids are agent-scoped so two agents can hold the same logical rows.
        // A triple whose subject is the contact.
        engine
            .store_temporal(
                agent,
                entry(&format!("{agent}-m1"), agent, "Alice prefers tea"),
                TemporalMeta {
                    subject: Some("user:alice".into()),
                    predicate: Some("prefers".into()),
                    object: Some("tea".into()),
                    ..Default::default()
                }, crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        // A free-text mention of the contact.
        engine
            .store_temporal(
                agent,
                entry(
                    &format!("{agent}-m2"),
                    agent,
                    "met user:alice at the conference",
                ),
                TemporalMeta::default(), crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        // An unrelated row that must survive.
        engine
            .store_temporal(
                agent,
                entry(&format!("{agent}-m3"), agent, "Bob likes coffee"),
                TemporalMeta {
                    subject: Some("user:bob".into()),
                    predicate: Some("likes".into()),
                    object: Some("coffee".into()),
                    ..Default::default()
                }, crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn export_then_erase_removes_only_contact_rows() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "agent1").await;

        let bundle = gdpr_export(&engine, "agent1", "user:alice").await.unwrap();
        assert_eq!(bundle["counts"]["memories"], 2, "alice appears in m1+m2");

        let summary = gdpr_erase(&engine, "agent1", "user:alice", true)
            .await
            .unwrap();
        assert_eq!(summary.memories_deleted, 2);
        assert_eq!(summary.archived_memories_deleted, 0, "no archive table yet");
        assert!(summary.tombstone_id.is_some());

        // Bob survived.
        let after = gdpr_export(&engine, "agent1", "user:alice").await.unwrap();
        assert_eq!(after["counts"]["memories"], 0, "alice fully erased");
        let bob = gdpr_export(&engine, "agent1", "user:bob").await.unwrap();
        assert_eq!(bob["counts"]["memories"], 1, "bob untouched");
    }

    #[tokio::test]
    async fn erase_is_agent_scoped() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "agent1").await;
        seed(&engine, "agent2").await;

        gdpr_erase(&engine, "agent1", "user:alice", false)
            .await
            .unwrap();
        // agent2's alice rows are untouched.
        let other = gdpr_export(&engine, "agent2", "user:alice").await.unwrap();
        assert_eq!(other["counts"]["memories"], 2, "cross-agent isolation");
    }

    #[tokio::test]
    async fn wildcard_in_contact_is_literal() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        engine
            .store_temporal(
                "a",
                entry("x1", "a", "literal percent user_100"),
                TemporalMeta {
                    subject: Some("user_100".into()),
                    predicate: Some("p".into()),
                    ..Default::default()
                }, crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        engine
            .store_temporal(
                "a",
                entry("x2", "a", "userX100 should not match"),
                TemporalMeta {
                    subject: Some("userX100".into()),
                    predicate: Some("p".into()),
                    ..Default::default()
                }, crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        // `_` must be literal, so "userX100" (X in the wildcard slot) must NOT match.
        let bundle = gdpr_export(&engine, "a", "user_100").await.unwrap();
        assert_eq!(
            bundle["counts"]["memories"], 1,
            "underscore matched literally"
        );
    }

    /// A blank or very short contact would turn into a `%…%` pattern matching
    /// most or all of the employee's memories; export and erase refuse it
    /// before touching the database, and nothing is deleted.
    #[tokio::test]
    async fn gdpr_refuses_blank_or_short_contact() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "agent1").await;
        for bad in ["", "  ", "ab", " ab ", "李四"] {
            let export_err = gdpr_export(&engine, "agent1", bad).await.unwrap_err();
            assert!(
                export_err.to_string().contains("at least 3"),
                "export of {bad:?} must name the rule: {export_err}"
            );
            let erase_err = gdpr_erase(&engine, "agent1", bad, true).await.unwrap_err();
            assert!(
                erase_err.to_string().contains("at least 3"),
                "erase of {bad:?} must name the rule: {erase_err}"
            );
        }
        // Nothing was deleted and no tombstone was written.
        let conn = engine.conn_for_maintenance().await;
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories WHERE agent_id = 'agent1'", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert_eq!(n, 3, "all seeded rows survive a refused erase");
        // Three characters (CJK counted by chars, not bytes) is accepted, and
        // surrounding spaces are trimmed before matching.
        assert_eq!(validate_contact(" 王小明 ").unwrap(), "王小明");
        let bundle = gdpr_export(&engine, "agent1", "  user:alice  ").await.unwrap();
        assert_eq!(bundle["counts"]["memories"], 2);
        assert_eq!(bundle["contact"], "user:alice");
    }

    async fn archive_count(engine: &SqliteMemoryEngine, agent: &str, like: &str) -> i64 {
        let conn = engine.conn_for_maintenance().await;
        conn.query_row(
            "SELECT COUNT(*) FROM memories_archive WHERE agent_id = ?1 AND content LIKE ?2",
            rusqlite::params![agent, like],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Decay and `forget` move rows into `memories_archive`; export lists the
    /// archived copies and erase removes them in the same transaction, counts
    /// them, and leaves other contacts' and other agents' archive rows alone.
    #[tokio::test]
    async fn erase_removes_archived_copies_and_counts_them() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "agent1").await;
        seed(&engine, "agent2").await;
        // Archive alice's free-text mention and bob's row through the real
        // soft-delete path, and agent2's mention as well.
        assert!(engine.forget("agent1", "agent1-m2").await.unwrap());
        assert!(engine.forget("agent1", "agent1-m3").await.unwrap());
        assert!(engine.forget("agent2", "agent2-m2").await.unwrap());
        // A row decay archived but failed to delete sits in both tables; its
        // archive copy goes with the live row and is counted once.
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute(
                "INSERT INTO memories_archive (id, agent_id, content, timestamp)
                 SELECT id, agent_id, content, timestamp FROM memories WHERE id = 'agent1-m1'",
                [],
            )
            .unwrap();
        }

        let bundle = gdpr_export(&engine, "agent1", "user:alice").await.unwrap();
        assert_eq!(bundle["counts"]["archived_memories"], 1, "m2 archived copy");

        let summary = gdpr_erase(&engine, "agent1", "user:alice", true).await.unwrap();
        assert_eq!(summary.memories_deleted, 1, "only m1 is still live");
        assert_eq!(summary.archived_memories_deleted, 2, "m2 copy + m1 stray copy");
        assert!(summary.erased_memory_ids.contains(&"agent1-m2".to_string()));

        assert_eq!(archive_count(&engine, "agent1", "%alice%").await, 0);
        let conn = engine.conn_for_maintenance().await;
        let stray: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories_archive WHERE id = 'agent1-m1'", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert_eq!(stray, 0, "stray copy of the live row is gone");
        assert_eq!(archive_count(&engine, "agent1", "%Bob%").await, 1, "bob kept");
        assert_eq!(archive_count(&engine, "agent2", "%alice%").await, 1, "agent2 kept");

        let after = gdpr_export(&engine, "agent1", "user:alice").await.unwrap();
        assert_eq!(after["counts"]["archived_memories"], 0);
    }

    /// M5: a claim held for trust review keeps its triple only in
    /// `metadata.held_claim`; export and erase still find it, and an erased
    /// held claim can no longer be promoted (nothing is written back).
    #[tokio::test]
    async fn held_claim_is_exported_erased_and_cannot_be_promoted_after() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        engine
            .store_temporal(
                "a",
                entry("op", "a", "prefers: tea"),
                TemporalMeta {
                    subject: Some("user:alice".into()),
                    predicate: Some("prefers".into()),
                    object: Some("tea".into()),
                    origin: Some("operator".into()),
                    ..Default::default()
                }, crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        let held = engine
            .hold_refused_claim(
                "a",
                entry("held", "a", "prefers: coffee"),
                TemporalMeta {
                    subject: Some("user:alice".into()),
                    predicate: Some("prefers".into()),
                    object: Some("coffee".into()),
                    origin: Some("channel".into()),
                    ..Default::default()
                }, crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        let bundle = gdpr_export(&engine, "a", "user:alice").await.unwrap();
        assert_eq!(bundle["counts"]["memories"], 2, "fact + held claim");
        assert!(bundle["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == serde_json::json!(held)));

        let summary = gdpr_erase(&engine, "a", "user:alice", false).await.unwrap();
        assert_eq!(summary.memories_deleted, 2);
        let report = engine
            .promote_quarantined("a", &[held], "operator")
            .await
            .unwrap();
        assert_eq!((report.promoted, report.stale), (0, 0));
        let after = gdpr_export(&engine, "a", "user:alice").await.unwrap();
        assert_eq!(after["counts"]["memories"], 0);
    }
}
