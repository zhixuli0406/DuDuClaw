//! v1.84.0: channel-qualified user-profile subjects (RM-200 item 4).
//!
//! Before v1.84 a person's profile traits were stored under `user:<id>`, the
//! raw platform user id alone, so two people with the same id on different
//! platforms shared one profile. New writes use `user:<channel>:<id>`
//! ([`crate::user_profile::profile_subject`]). This one-time migration
//! rewrites the existing profile rows whose source conversations prove the
//! platform.
//!
//! Mechanism: the same as the crate's other one-time data steps
//! (`lineage::links::backfill_turn_links`, `lineage::connectors::migrate`) —
//! a `memory_meta` flag, checked and set inside the `BEGIN IMMEDIATE`
//! transaction `SqliteMemoryEngine::init_tables` opens around
//! `create_schema`. The whole rewrite is therefore one transaction with the
//! schema step: it commits completely or not at all, and concurrent openers
//! take turns on the write lock.
//!
//! Rule, per `memories` row of a profile class (`source_event` =
//! `user_profile` / `user_profile_consolidation`) whose subject is
//! `user:<bare>`:
//!
//! * its `direct` / `inherited` origins (`memory_origins`) must exist, all be
//!   conversation turns (`channel_message` / `mcp_turn`), and all come from
//!   sessions of exactly one channel (the exact segment of `source_session`
//!   before its first `:`);
//! * `<bare>` must not already start with `<channel>:` (an already
//!   qualified subject such as WebChat's `user:webchat:<owner>` — this also
//!   makes a second run a no-op);
//! * then `subject` becomes `user:<channel>:<bare>`. Nothing else of the row
//!   changes (`valid_from`, `valid_until`, timestamps, content, id), so its
//!   full-text row (`memories_fts`, keyed by id and content) and its
//!   embedding (a column of the row) stay valid. The entity-embedding cache
//!   row of the old subject (`entity_embedding`, keyed by the normalised
//!   entity text) is dropped; it is rebuilt lazily for whichever entities
//!   still exist.
//!
//! Left untouched: rows without origins, with any non-conversation origin
//! (import, system, dispatch run, untracked) or with origins of several
//! channels. They keep the bare subject; a conversation reads them only as
//! the legacy fallback of `ProfileKey::legacy_subject`, i.e. only for a
//! platform whose ids are globally unique (LINE, Discord) and only when it
//! has no qualified row. Pending held claims carry their triple in
//! `metadata.held_claim` (no `subject` column) and are not rewritten either:
//! their review cards are digests over that triple.
//!
//! Other rows whose subject merely starts with `user:` (facts the
//! conversation distiller extracted with a model-chosen subject) are not
//! profile rows and are left alone: new distilled facts keep using the
//! model's subject, and rewriting the old ones would split their
//! supersession chains.
//!
//! The memory-privacy forgotten-fact list (`<home>/memory_privacy_forgotten.jsonl`)
//! is a file outside this database, keyed by a digest of each of the
//! person's subjects; `memory_privacy::self_subjects` already records the
//! `user:<channel>:<id>` form of every verified channel identity, so it needs
//! no migration.

use super::*;

/// `memory_meta` key marking that the rewrite ran on this database.
pub(crate) const MIGRATION_KEY: &str = "profile_subject_channel_v1";

/// `source_event` of the rows the profile module writes.
const PROFILE_EVENTS_SQL: &str = "'user_profile', 'user_profile_consolidation'";

fn mem_err(e: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(format!("profile subject migration: {e}"))
}

/// Run the rewrite once per database (see the module docs). Must run on the
/// connection inside the schema transaction.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let done = conn
        .query_row(
            "SELECT 1 FROM memory_meta WHERE key = ?1",
            params![MIGRATION_KEY],
            |_| Ok(()),
        )
        .optional()
        .map_err(mem_err)?
        .is_some();
    if done {
        return Ok(());
    }
    let rewritten = rewrite_channel_subjects(conn)?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_meta (key, value) VALUES (?1, '1')",
        params![MIGRATION_KEY],
    )
    .map_err(mem_err)?;
    if rewritten > 0 {
        info!(rewritten, "profile subjects qualified with their channel");
    }
    Ok(())
}

/// The rewrite itself (idempotent by its rule). Returns the number of rows
/// whose subject changed.
pub(crate) fn rewrite_channel_subjects(conn: &Connection) -> Result<usize> {
    let candidates: Vec<(String, String, String)> = {
        let sql = format!(
            "SELECT id, agent_id, subject FROM memories
             WHERE substr(subject, 1, 5) = 'user:' AND source_event IN ({PROFILE_EVENTS_SQL})"
        );
        let mut stmt = conn.prepare(&sql).map_err(mem_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(mem_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(mem_err)?
    };
    let mut origins = conn
        .prepare_cached(
            "SELECT source_kind, source_session FROM memory_origins
             WHERE memory_store = 'memories' AND memory_id = ?1 AND agent_id = ?2
               AND role IN ('direct', 'inherited')",
        )
        .map_err(mem_err)?;
    let conversation_kinds = [
        crate::lineage::SourceKind::ChannelMessage.as_str(),
        crate::lineage::SourceKind::McpTurn.as_str(),
    ];
    let mut rewritten = 0usize;
    let mut old_subjects: std::collections::BTreeSet<(String, String)> = Default::default();
    for (id, agent_id, subject) in candidates {
        let Some(bare) = subject.strip_prefix("user:").filter(|b| !b.trim().is_empty()) else {
            continue;
        };
        let rows: Vec<(String, String)> = origins
            .query_map(params![id, agent_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(mem_err)?
            .collect::<std::result::Result<_, _>>()
            .map_err(mem_err)?;
        let Some(channel) = single_channel(&rows, &conversation_kinds) else {
            continue;
        };
        if bare.starts_with(&format!("{channel}:")) {
            continue;
        }
        let Some(qualified) = crate::user_profile::profile_subject(&channel, bare) else {
            continue;
        };
        if qualified == subject {
            continue;
        }
        conn.execute(
            "UPDATE memories SET subject = ?1 WHERE id = ?2 AND agent_id = ?3",
            params![qualified, id, agent_id],
        )
        .map_err(mem_err)?;
        rewritten += 1;
        old_subjects.insert((agent_id, subject));
    }
    for (agent_id, subject) in old_subjects {
        conn.execute(
            "DELETE FROM entity_embedding WHERE agent_id = ?1 AND entity = ?2",
            params![agent_id, subject.trim().to_lowercase()],
        )
        .map_err(mem_err)?;
    }
    Ok(rewritten)
}

/// The one channel all `(kind, session)` origins share, when every origin
/// is a conversation turn; `None` for no origins, a non-conversation origin,
/// a session without a channel, or several channels.
fn single_channel(rows: &[(String, String)], conversation_kinds: &[&str]) -> Option<String> {
    let mut channel: Option<String> = None;
    for (kind, session) in rows {
        if !conversation_kinds.contains(&kind.as_str()) {
            return None;
        }
        let this = crate::user_profile::channel_of_session(session)?;
        match &channel {
            None => channel = Some(this),
            Some(c) if *c == this => {}
            Some(_) => return None,
        }
    }
    channel
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lineage::{Provenance, SourceRef};
    use duduclaw_core::types::MemoryLayer;

    fn trait_entry(content: &str, event: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: "a1".into(),
            content: content.into(),
            timestamp: Utc::now(),
            tags: vec!["user-profile".into()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 6.0,
            access_count: 0,
            last_accessed: None,
            source_event: event.into(),
        }
    }

    fn msg(session: &str, seq: i64) -> SourceRef {
        SourceRef::channel_message(session, seq, Utc::now(), None)
    }

    async fn put(
        engine: &SqliteMemoryEngine,
        subject: &str,
        predicate: &str,
        event: &str,
        prov: Provenance,
    ) -> String {
        // A fresh value per write, so a second write of the same triple
        // supersedes instead of reaffirming.
        let value = uuid::Uuid::new_v4().to_string();
        let meta = TemporalMeta {
            subject: Some(subject.into()),
            predicate: Some(predicate.into()),
            object: Some(value.clone()),
            origin: Some("user_profile".into()),
            ..Default::default()
        };
        engine
            .store_temporal("a1", trait_entry(&format!("{predicate}: {value}"), event), meta, prov)
            .await
            .unwrap()
    }

    type Snapshot = (String, Option<String>, Option<String>, String, Option<String>, String);

    async fn row(engine: &SqliteMemoryEngine, id: &str) -> Snapshot {
        let conn = engine.conn_for_maintenance().await;
        conn.query_row(
            "SELECT subject, valid_from, valid_until, timestamp, ingested_at, content
             FROM memories WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap()
    }

    /// A v1.83-shaped `memory.db` (bare `user:<id>` profile subjects, no
    /// migration flag) opened by this version: single-channel rows are
    /// rewritten in place, every other row is left alone, timestamps and
    /// validity untouched, and a second open (and a second explicit run)
    /// changes nothing.
    #[tokio::test]
    async fn v183_profile_subjects_are_qualified_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let (single, turn, expired, mixed, no_origins, imported, webchat, distilled, summary);
        let (pre_single, pre_expired): (Snapshot, Snapshot);
        {
            let engine = SqliteMemoryEngine::new(&db).unwrap();
            single = put(&engine, "user:998", "prefers", "user_profile",
                Provenance::Sources(vec![msg("telegram:998", 1), msg("telegram:998", 2)])).await;
            turn = put(&engine, "user:998", "timezone", "user_profile",
                Provenance::source(SourceRef::other(
                    crate::lineage::SourceKind::McpTurn, "telegram:998#agent:a1", "turn:t1", Utc::now(),
                ))).await;
            expired = put(&engine, "user:998", "prefers", "user_profile",
                Provenance::source(msg("telegram:998", 3))).await;
            mixed = put(&engine, "user:777", "prefers", "user_profile",
                Provenance::Sources(vec![msg("telegram:777", 4), msg("line:777", 5)])).await;
            no_origins = put(&engine, "user:555", "prefers", "user_profile",
                Provenance::source(msg("telegram:555", 6))).await;
            imported = put(&engine, "user:444", "prefers", "user_profile",
                Provenance::source(SourceRef::import_item("archive", b"p", Utc::now()))).await;
            webchat = put(&engine, "user:webchat:owner-1", "prefers", "user_profile",
                Provenance::source(msg("webchat:owner-1:conn", 7))).await;
            distilled = put(&engine, "user:998", "likes", "conversation_distill",
                Provenance::source(msg("telegram:998", 8))).await;
            summary = put(&engine, "user:998", "profile_summary", "user_profile_consolidation",
                Provenance::derived(vec![turn.clone()])).await;
            // Shape the file like v1.83.0: no flag, one row without lineage.
            {
                let conn = engine.conn_for_maintenance().await;
                conn.execute("DELETE FROM memory_meta WHERE key = ?1", params![MIGRATION_KEY]).unwrap();
                conn.execute("DELETE FROM memory_origins WHERE memory_id = ?1", params![no_origins]).unwrap();
                conn.execute(
                    "INSERT INTO entity_embedding (agent_id, entity, model, vec) VALUES ('a1', 'user:998', 'm', x'00')",
                    [],
                )
                .unwrap();
            }
            pre_single = row(&engine, &single).await;
            pre_expired = row(&engine, &expired).await;
        }
        assert_eq!(pre_single.0, "user:998");
        assert!(pre_single.2.is_some(), "`expired` superseded `single` (same conversation)");
        // First open of the v1.83-shaped file: rewritten, nothing else changed.
        let after_first = {
            let engine = SqliteMemoryEngine::new(&db).unwrap();
            let single_now = row(&engine, &single).await;
            assert_eq!(single_now.0, "user:telegram:998", "rewritten at open");
            assert_eq!(
                (&single_now.1, &single_now.2, &single_now.3, &single_now.4, &single_now.5),
                (&pre_single.1, &pre_single.2, &pre_single.3, &pre_single.4, &pre_single.5),
                "valid_from / valid_until / timestamps / content untouched"
            );
            let expired_now = row(&engine, &expired).await;
            assert_eq!(
                (&expired_now.1, &expired_now.2, &expired_now.3, &expired_now.4, &expired_now.5),
                (&pre_expired.1, &pre_expired.2, &pre_expired.3, &pre_expired.4, &pre_expired.5),
                "valid_from / valid_until / timestamps / content untouched"
            );
            expired_now
        };
        let engine = SqliteMemoryEngine::new(&db).unwrap();
        for id in [&single, &turn, &expired, &summary] {
            assert_eq!(row(&engine, id).await.0, "user:telegram:998", "{id}");
        }
        assert!(row(&engine, &single).await.2.is_some(), "valid_until kept (superseded by `expired`)");
        assert_eq!(row(&engine, &expired).await, after_first, "second open changes nothing");
        assert_eq!(row(&engine, &mixed).await.0, "user:777", "two channels: untouched");
        assert_eq!(row(&engine, &no_origins).await.0, "user:555", "no origins: untouched");
        assert_eq!(row(&engine, &imported).await.0, "user:444", "import origin: untouched");
        assert_eq!(row(&engine, &webchat).await.0, "user:webchat:owner-1", "already qualified");
        assert_eq!(row(&engine, &distilled).await.0, "user:998", "not a profile row");
        {
            let conn = engine.conn_for_maintenance().await;
            let cached: i64 = conn
                .query_row("SELECT COUNT(*) FROM entity_embedding WHERE entity = 'user:998'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(cached, 0, "stale entity cache of the old subject dropped");
            assert_eq!(rewrite_channel_subjects(&conn).unwrap(), 0, "idempotent by rule");
        }
        let traits = crate::user_profile::profile_traits_for(
            &engine,
            "a1",
            &crate::user_profile::ProfileKey::for_channel("telegram", "998", false).unwrap(),
            RowScope::All,
        )
        .await
        .unwrap();
        assert!(traits.iter().any(|t| t.predicate == "timezone"), "{traits:?}");
    }
}
