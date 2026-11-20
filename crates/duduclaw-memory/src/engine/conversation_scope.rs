//! v1.83.0: which memory rows a reader serving ONE conversation may see.
//!
//! The host writes many rows on its own, most of them from conversations:
//! distilled facts, user-profile traits and their summary, conversation
//! summaries, consolidated rules, recorded decisions, and any source event
//! added later. An employee serves many people, so a read made for one
//! conversation (the employee's MCP tools on a channel turn) must not return
//! such a row from another conversation. This is an allow-list: only the
//! explicitly stored rows of [`EXPLICIT_SOURCE_EVENTS`] skip the check; every
//! other `source_event` (empty and unknown ones included) must come from this
//! conversation (fail closed).
//!
//! The source conversation of a row is what the P2-B lineage recorded for it:
//! `memory_origins.source_session` of its `direct` and `inherited` rows
//! (`memory_store = 'memories'`, keyed by `memory_id` + `agent_id`). A
//! `reaffirm` row only says another write repeated the fact; it is not where
//! the content came from, and lineage inheritance ignores it too
//! (`lineage::db::resolve`). No schema change: the table already exists.

use super::*;

/// `source_event` of the rows whose content was stored by an explicit act of
/// the employee or an operator, not derived by the host from conversation
/// content. Only these skip the same-conversation check:
///
/// | event          | writer                                              | why exempt |
/// |----------------|-----------------------------------------------------|------------|
/// | `mcp_internal` | `duduclaw-cli/src/mcp_memory_handlers.rs` (`handle_memory_store`) | the employee's own `memory_store` call |
/// | `import`       | `duduclaw-memory/src/import.rs` (`build_memory_entry`, run by `duduclaw-cli/src/wizard.rs`) | operator file import |
///
/// Every other `source_event` is checked. The ones the code writes today, and
/// why each is not exempt:
///
/// | event                           | writer | why not exempt |
/// |---------------------------------|--------|----------------|
/// | `conversation_distill`          | `duduclaw-gateway/src/wiki_ingest.rs` | facts the host distils from a conversation |
/// | `user_profile`                  | `duduclaw-memory/src/user_profile.rs` (`trait_write`) | traits about a person, distilled or recorded during their conversation |
/// | `user_profile_consolidation`    | `duduclaw-memory/src/user_profile.rs` (`consolidate_profile`) | summary of those traits |
/// | `conversation_summary`          | `duduclaw-gateway/src/skill_synthesis_pipeline/pipeline.rs` (tests only; older rows) | a conversation summary |
/// | `decision_capture`              | `duduclaw-gateway/src/decision_capture.rs` | another person's question and options (the decision tools read through `list_open_decisions_in_session`, not this filter) |
/// | `decision_resolve`              | `duduclaw-memory/src/engine.rs` (`decision_artifact_entry`) | decision rows and their outcome |
/// | `reflexion_consolidation`       | `duduclaw-gateway/src/reflexion.rs` | rules consolidated from conversation mistakes |
/// | `night_consolidation`           | `duduclaw-memory/src/night.rs` | consolidation of episodic (conversation) rows |
/// | `night_schema_induction`        | `duduclaw-memory/src/night.rs` | schemas induced from episodic rows |
/// | `prediction_episodic`           | `duduclaw-gateway/src/channel_reply/inner.rs` | per-turn record of a channel reply |
/// | `playbook_entry`                | `duduclaw-gateway/src/playbook/store.rs` | host-written playbook from conversation signals |
/// | `persona_suppression_induction` | `duduclaw-gateway/src/persona_induction.rs` | host inference from operational history |
/// | `task_transition`               | `duduclaw-gateway/src/prediction/transition.rs` | host-derived task pointers |
/// | `task_forward_induction`        | `duduclaw-gateway/src/prediction/task_rule_induce.rs` | host-induced task rules |
/// | `footprint_distill`             | `duduclaw-gateway/src/footprint_distill.rs` | host round-up, no conversation of this turn |
/// | `mcp_external`                  | `duduclaw-cli/src/mcp_memory_handlers.rs` (`handle_memory_store`) | external client rows live in `external/` namespaces, which never read with a conversation scope |
/// | `migrate-from-<platform>`       | `duduclaw-cli/src/migrate_from/apply.rs` | dynamic name; migrated content may hold other people's conversations |
/// | `agent-import-<format>`         | `duduclaw-cli/src/expert/agent_config/slots.rs` | dynamic name; imported agent memory may hold other people's conversations |
/// | `<migration id>`                | `duduclaw-gateway/src/migration_import.rs` | dynamic name; legacy import archive |
/// | `agent_mood`, `curated`, `fact` | no production writer (tests / older rows only) | not enumerated by a writer, so fail closed |
/// | empty / any other value         | — | unknown, so fail closed |
pub const EXPLICIT_SOURCE_EVENTS: &[&str] = &["mcp_internal", "import"];

/// Whether `source_event` names an explicitly stored row
/// ([`EXPLICIT_SOURCE_EVENTS`]), exempt from the same-conversation check.
pub fn is_explicitly_stored(source_event: &str) -> bool {
    EXPLICIT_SOURCE_EVENTS.contains(&source_event.trim())
}

impl SqliteMemoryEngine {
    /// Of `ids` (rows of `agent_id`), the ones a reader serving conversation
    /// `session` may NOT see: every row that is not explicitly stored
    /// ([`EXPLICIT_SOURCE_EVENTS`]) unless it has at least one `direct` /
    /// `inherited` lineage row and every such row has
    /// `source_session = session`. Explicitly stored rows are never hidden.
    ///
    /// Fail closed:
    /// * `session = None` or empty (the conversation is unknown) hides every
    ///   row that is not explicitly stored;
    /// * such a row with an empty or unknown `source_event`, no lineage
    ///   (written before P2-B), or a source outside this conversation
    ///   (another session, a dispatch run, `system:` / `untracked`), is
    ///   hidden;
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
            if is_explicitly_stored(event.as_deref().unwrap_or("")) {
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
    async fn non_explicit_rows_are_scoped_to_their_conversation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let a = put(&engine, "alpha fact", "conversation_distill", from_session("telegram:A", 1)).await;
        let b = put(&engine, "beta fact", "conversation_distill", from_session("telegram:B", 2)).await;
        let stored = put(&engine, "explicit", "mcp_internal", from_session("telegram:B", 3)).await;
        let imported = put(&engine, "imported", "import", Provenance::test_only()).await;
        let system = put(&engine, "system", "footprint_distill", Provenance::test_only()).await;
        let ids = vec![
            a.clone(),
            b.clone(),
            stored.clone(),
            imported.clone(),
            system.clone(),
            "missing".to_string(),
        ];

        let in_a = engine.ids_hidden_from_conversation("a1", &ids, Some("telegram:A")).await.unwrap();
        assert!(!in_a.contains(&a));
        assert!(in_a.contains(&b));
        assert!(!in_a.contains(&stored), "explicit rows are never hidden");
        assert!(!in_a.contains(&imported), "operator imports are never hidden");
        assert!(in_a.contains(&system), "a non-conversation source is not this conversation");
        assert!(in_a.contains("missing"));

        // Unknown conversation: every row that is not explicitly stored is hidden.
        for unknown in [None, Some(""), Some("  ")] {
            let hidden = engine.ids_hidden_from_conversation("a1", &ids, unknown).await.unwrap();
            assert!(hidden.contains(&a) && hidden.contains(&b), "{unknown:?}");
            assert!(!hidden.contains(&stored) && !hidden.contains(&imported), "{unknown:?}");
        }
    }

    /// Allow-list, not deny-list: an empty, unknown or never-enumerated
    /// `source_event` from another conversation is hidden; from this
    /// conversation it is returned like any other row.
    #[tokio::test]
    async fn unenumerated_source_events_fail_closed() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for event in ["", "never_enumerated_event", "decision_capture", "conversation_summary", "curated"] {
            let row = put(&engine, "row", event, from_session("telegram:A", 1)).await;
            let in_b = engine.ids_hidden_from_conversation("a1", &[row.clone()], Some("telegram:B")).await.unwrap();
            assert!(in_b.contains(&row), "{event:?}");
            let in_a = engine.ids_hidden_from_conversation("a1", &[row.clone()], Some("telegram:A")).await.unwrap();
            assert!(in_a.is_empty(), "{event:?}");
        }
        assert_eq!(EXPLICIT_SOURCE_EVENTS, &["mcp_internal", "import"]);
        assert!(is_explicitly_stored(" mcp_internal "));
        for event in ["", "mcp_external", "curated", "user_profile", "Import"] {
            assert!(!is_explicitly_stored(event), "{event:?}");
        }
    }

    /// A row without lineage (written before P2-B) is hidden;
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
