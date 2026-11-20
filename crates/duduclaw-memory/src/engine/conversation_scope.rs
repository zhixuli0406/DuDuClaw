//! v1.83.0: which memory rows a reader serving ONE conversation may see.
//!
//! The host writes some rows on its own from a conversation: distilled facts,
//! user-profile traits and their summary, OS footprint round-ups
//! ([`HOST_GENERATED_SOURCE_EVENTS`]). An employee serves many people, so a
//! read made for one conversation (the employee's MCP tools on a channel
//! turn) must not return such a row distilled from another conversation.
//!
//! The source conversation of a row is what the P2-B lineage recorded for it:
//! `memory_origins.source_session` of its `direct` and `inherited` rows
//! (`memory_store = 'memories'`, keyed by `memory_id` + `agent_id`). A
//! `reaffirm` row only says another write repeated the fact; it is not where
//! the content came from, and lineage inheritance ignores it too
//! (`lineage::db::resolve`). No schema change: the table already exists.

use super::*;

/// `source_event` of the rows the host generates on its own from a
/// conversation (same family as `footprint_distill::FOOTPRINT_SOURCE_EVENT`
/// names): conversation distillation (`wiki_ingest::DISTILL_SOURCE_EVENT`,
/// also its wiki pointer rows), user-profile traits, the profile summary and
/// the OS footprint round-up. Rows the employee stored explicitly
/// (`mcp_internal` / `mcp_external`) and every other source event are not in
/// it.
pub const HOST_GENERATED_SOURCE_EVENTS: &[&str] = &[
    "conversation_distill",
    "user_profile",
    "user_profile_consolidation",
    "footprint_distill",
];

/// Whether `source_event` names a host-generated row
/// ([`HOST_GENERATED_SOURCE_EVENTS`]).
pub fn is_host_generated(source_event: &str) -> bool {
    HOST_GENERATED_SOURCE_EVENTS.contains(&source_event.trim())
}

impl SqliteMemoryEngine {
    /// Of `ids` (rows of `agent_id`), the ones a reader serving conversation
    /// `session` may NOT see: a host-generated row
    /// ([`HOST_GENERATED_SOURCE_EVENTS`]) unless it has at least one
    /// `direct` / `inherited` lineage row and every such row has
    /// `source_session = session`. Rows with any other `source_event` are
    /// never hidden.
    ///
    /// Fail closed:
    /// * `session = None` or empty (the conversation is unknown) hides every
    ///   host-generated row;
    /// * a host-generated row with no lineage (written before P2-B), or with
    ///   a source outside this conversation (another session, a dispatch run,
    ///   `system:` / `untracked`), is hidden;
    /// * an id that is not a row of `agent_id` is hidden (callers only pass
    ///   ids they just read, so this only matters for a concurrent delete).
    pub async fn ids_hidden_from_conversation(
        &self,
        agent_id: &str,
        ids: &[String],
        session: Option<&str>,
    ) -> Result<std::collections::HashSet<String>> {
        let session = session.map(str::trim).filter(|s| !s.is_empty());
        let conn = self.conn.lock().await;
        crate::isolation_io::verify()?;
        let mut event_stmt = conn
            .prepare_cached("SELECT source_event FROM memories WHERE agent_id = ?1 AND id = ?2")
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut origin_stmt = conn
            .prepare_cached(
                "SELECT COUNT(*), COALESCE(SUM(source_session != ?3), 0)
                 FROM memory_origins
                 WHERE memory_store = 'memories' AND memory_id = ?2 AND agent_id = ?1
                   AND role != 'reaffirm'",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut hidden = std::collections::HashSet::new();
        for id in ids {
            let event: Option<Option<String>> = event_stmt
                .query_row(params![agent_id, id], |r| r.get(0))
                .optional()
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            let Some(event) = event else {
                hidden.insert(id.clone());
                continue;
            };
            if !is_host_generated(event.as_deref().unwrap_or("")) {
                continue;
            }
            let Some(session) = session else {
                hidden.insert(id.clone());
                continue;
            };
            let (total, foreign): (i64, i64) = origin_stmt
                .query_row(params![agent_id, id, session], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
            if total == 0 || foreign > 0 {
                hidden.insert(id.clone());
            }
        }
        Ok(hidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lineage::{Provenance, SourceRef};
    use duduclaw_core::types::MemoryLayer;

    fn entry(content: &str, source_event: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: "a1".into(),
            content: content.into(),
            timestamp: Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: source_event.into(),
        }
    }

    fn from_session(session: &str, seq: i64) -> Provenance {
        Provenance::source(SourceRef::channel_message(session, seq, Utc::now(), None))
    }

    async fn put(engine: &SqliteMemoryEngine, content: &str, event: &str, prov: Provenance) -> String {
        engine
            .store_temporal("a1", entry(content, event), TemporalMeta::default(), prov)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn host_generated_rows_are_scoped_to_their_conversation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let a = put(&engine, "alpha fact", "conversation_distill", from_session("telegram:A", 1)).await;
        let b = put(&engine, "beta fact", "conversation_distill", from_session("telegram:B", 2)).await;
        let stored = put(&engine, "explicit", "mcp_internal", from_session("telegram:A", 3)).await;
        let system = put(&engine, "system", "footprint_distill", Provenance::test_only()).await;
        let ids = vec![a.clone(), b.clone(), stored.clone(), system.clone(), "missing".to_string()];

        let in_a = engine.ids_hidden_from_conversation("a1", &ids, Some("telegram:A")).await.unwrap();
        assert!(!in_a.contains(&a));
        assert!(in_a.contains(&b));
        assert!(!in_a.contains(&stored), "explicit rows are never hidden");
        assert!(in_a.contains(&system), "a non-conversation source is not this conversation");
        assert!(in_a.contains("missing"));

        // Unknown conversation: every host-generated row is hidden.
        for unknown in [None, Some(""), Some("  ")] {
            let hidden = engine.ids_hidden_from_conversation("a1", &ids, unknown).await.unwrap();
            assert!(hidden.contains(&a) && hidden.contains(&b), "{unknown:?}");
            assert!(!hidden.contains(&stored), "{unknown:?}");
        }
    }

    /// A host-generated row without lineage (written before P2-B) is hidden;
    /// a row derived from two conversations is visible in neither; a
    /// reaffirmation from another conversation does not change the answer.
    #[tokio::test]
    async fn untracked_mixed_and_reaffirmed_rows() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let old = put(&engine, "old fact", "conversation_distill", from_session("telegram:A", 1)).await;
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("DELETE FROM memory_origins WHERE memory_id = ?1", params![old])
                .unwrap();
        }
        let p1 = put(&engine, "trait one", "user_profile", from_session("telegram:A", 2)).await;
        let p2 = put(&engine, "trait two", "user_profile", from_session("telegram:B", 3)).await;
        let summary = put(
            &engine,
            "summary",
            "user_profile_consolidation",
            Provenance::derived(vec![p1.clone(), p2.clone()]),
        )
        .await;
        let ids = vec![old.clone(), p1.clone(), summary.clone()];
        let in_a = engine.ids_hidden_from_conversation("a1", &ids, Some("telegram:A")).await.unwrap();
        assert!(in_a.contains(&old));
        assert!(!in_a.contains(&p1));
        assert!(in_a.contains(&summary), "it also carries telegram:B's trait");

        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute(
                "INSERT INTO memory_origins (memory_store, memory_id, agent_id, source_kind,
                    source_session, source_message, source_seq, source_observed_at, role, created_at)
                 VALUES ('memories', ?1, 'a1', 'channel_message', 'telegram:B', 'm:9', 9, ?2, 'reaffirm', ?2)",
                params![p1, crate::lineage::format_ts(Utc::now())],
            )
            .unwrap();
        }
        let in_a = engine.ids_hidden_from_conversation("a1", &[p1.clone()], Some("telegram:A")).await.unwrap();
        assert!(in_a.is_empty());
        let in_b = engine.ids_hidden_from_conversation("a1", &[p1.clone()], Some("telegram:B")).await.unwrap();
        assert!(in_b.contains(&p1));
    }
}
