//! GDPR data-subject request helpers — export / erase by contact.
//!
//! Built on the live [`SqliteMemoryEngine`] through its public
//! `conn_for_maintenance()` guard; **no schema change**. A "contact" is matched
//! three ways: as the structured `subject` or `object` of a temporal triple, or
//! as a free-text mention in `content` / `fact` (LIKE, wildcard-escaped so an id
//! containing `%`/`_` matches literally).
//!
//! `gdpr_export` returns a JSON bundle the caller writes to disk. `gdpr_erase`
//! deletes transactionally across all four physical tables
//! (`memories` + `memories_fts` + `key_facts` + `key_facts_fts`) so **no FTS
//! orphan is left behind** (a hazard the pre-existing `purge_stale_facts` path
//! does not guard against), then records a tombstone memory documenting the
//! erasure — the retained legal-basis record that the request was fulfilled.
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

/// Escape LIKE metacharacters so the contact id is matched literally under an
/// `ESCAPE '\'` clause (prevents a `%`/`_` in the id from becoming a wildcard).
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Outcome of an erase: how many rows were removed and the tombstone id (if one
/// was written).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GdprEraseSummary {
    pub contact: String,
    pub memories_deleted: u64,
    pub key_facts_deleted: u64,
    pub tombstone_id: Option<String>,
}

/// Aggregate every stored row referencing `contact` into a JSON bundle
/// (memories with full temporal/provenance columns + key facts). Read-only.
pub async fn gdpr_export(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    contact: &str,
) -> Result<Value> {
    let conn = engine.conn_for_maintenance().await;
    let like = format!("%{}%", like_escape(contact));

    let mut mem_stmt = conn
        .prepare(
            "SELECT id, content, layer, timestamp, tags, subject, predicate, object,
                    valid_from, valid_until, superseded_by, supersedes, confidence,
                    origin, origin_trust
             FROM memories
             WHERE agent_id = ?1
               AND (subject = ?2 OR object = ?2 OR content LIKE ?3 ESCAPE '\\')
             ORDER BY COALESCE(valid_from, timestamp) ASC",
        )
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

    Ok(json!({
        "contact": contact,
        "agent_id": agent_id,
        "exported_at": Utc::now().to_rfc3339(),
        "counts": { "memories": memories.len(), "key_facts": key_facts.len() },
        "memories": memories,
        "key_facts": key_facts,
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
    let conn = engine.conn_for_maintenance().await;
    let like = format!("%{}%", like_escape(contact));

    // Collect the exact ids/rowids first so the FTS delete and the base-table
    // delete operate on identical rows (mirrors the decay janitor's contract).
    let mem_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT id FROM memories
                 WHERE agent_id = ?1
                   AND (subject = ?2 OR object = ?2 OR content LIKE ?3 ESCAPE '\\')",
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

    let now = Utc::now().to_rfc3339();
    let tombstone_id = if tombstone {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        None
    };

    let txn: std::result::Result<(u64, u64), String> = (|| {
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
                "[gdpr-erasure] contact_sha256={contact_hash} removed memories={mem_deleted} key_facts={kf_deleted} at={now}"
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
        Ok((mem_deleted, kf_deleted))
    })();

    match txn {
        Ok((memories_deleted, key_facts_deleted)) => {
            // D3.1: erasure deletes rows (and may insert a tombstone triple) —
            // invalidate this agent's cached SPO graph. This maintenance path
            // bypasses the per-agent write bumps in the engine.
            if memories_deleted > 0 || tombstone_id.is_some() {
                drop(conn);
                engine.bump_graph_generation(agent_id);
            }
            Ok(GdprEraseSummary {
                contact: contact.to_string(),
                memories_deleted,
                key_facts_deleted,
                tombstone_id,
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
                },
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
                TemporalMeta::default(),
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
                },
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
                },
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
                },
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
}
