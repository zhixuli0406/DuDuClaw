//! v1.83.0: which memory rows a reader serving ONE conversation may see.
//!
//! The host writes many rows on its own, most of them from conversations:
//! distilled facts, user-profile traits and their summary, conversation
//! summaries, consolidated rules, recorded decisions, and any source event
//! added later. An employee serves many people, so a read made for one
//! conversation (the employee's MCP tools on a channel turn) must not return
//! such a row from another conversation. This is an allow-list: only the
//! explicitly stored rows of [`EXPLICIT_SOURCE_EVENTS`] skip the check. Every
//! other row needs recorded origins: each must be an operator import item or
//! come from this conversation (fail closed), regardless of `source_event`.
//!
//! The source conversation of a row is what the P2-B lineage recorded for it:
//! `memory_origins.source_session` of its `direct` and `inherited` rows
//! (`memory_store = 'memories'`, keyed by `memory_id` + `agent_id`).
//! `source_kind` identifies import items via [`crate::lineage::SourceKind::ImportItem`].
//! A `reaffirm` row only says another write repeated the fact; it is not where
//! the content came from, and lineage inheritance ignores it too
//! (`lineage::db::resolve`). No schema change: the table already exists.

use super::*;

/// `source_event` of the rows whose content was stored by an explicit act of
/// the employee or an operator, not derived by the host from conversation
/// content. These skip the lineage check by name:
///
/// | event          | writer                                              | why exempt |
/// |----------------|-----------------------------------------------------|------------|
/// | `mcp_internal` | `duduclaw-cli/src/mcp_memory_handlers.rs` (`handle_memory_store`) | the employee's own `memory_store` call |
/// | `import`       | `duduclaw-memory/src/import.rs` (`build_memory_entry`, run by `duduclaw-cli/src/wizard.rs`) | operator file import; also records import-item lineage |
///
/// Varying import event names are visible through their import-item lineage:
///
/// | event                     | writer | why visible |
/// |---------------------------|--------|-------------|
/// | `migrate-from-<platform>` | `duduclaw-cli/src/migrate_from/apply.rs` | operator platform migration with import-item origins |
/// | `agent-import-<format>`   | `duduclaw-cli/src/expert/agent_config/slots.rs` | operator agent import with import-item origins |
/// | `<migration id>`          | `duduclaw-gateway/src/migration_import.rs` | legacy migration with import-item origins |
///
/// Every other `source_event` is checked too. The remaining writers today,
/// and why their rows need same-conversation origins:
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
/// | `agent_mood`, `curated`, `fact` | no production writer (tests / older rows only) | require import-item or same-conversation origins |
/// | empty / any other value         | — | require import-item or same-conversation origins |
pub const EXPLICIT_SOURCE_EVENTS: &[&str] = &["mcp_internal", "import"];

/// Whether `source_event` names an explicitly stored row
/// ([`EXPLICIT_SOURCE_EVENTS`]), exempt from the same-conversation check.
///
/// v1.84.0: only ASCII spaces are trimmed, exactly what SQLite's one-argument
/// `TRIM()` strips, so this test and [`visible_to_conversation_sql`] agree on
/// every stored value (a tab or newline around the name no longer makes a row
/// explicit; it is checked like any other row — fail closed).
pub fn is_explicitly_stored(source_event: &str) -> bool {
    EXPLICIT_SOURCE_EVENTS.contains(&source_event.trim_matches(' '))
}

/// `memory_origins.role` values that say where a row's content came from. A
/// `reaffirm` row only says another write repeated the fact.
const CONTENT_ROLES_SQL: &str = "'direct', 'inherited'";

/// `memory_origins.source_kind` values of a conversation turn: a row whose
/// lineage names one of these is a row OF that conversation.
const CONVERSATION_KINDS: [crate::lineage::SourceKind; 2] = [
    crate::lineage::SourceKind::ChannelMessage,
    crate::lineage::SourceKind::McpTurn,
];

/// The conversation key a reader compares against: trimmed, `None` when
/// absent or blank (an unknown conversation — fail closed).
pub fn conversation_key(session: Option<&str>) -> Option<&str> {
    session.map(str::trim).filter(|s| !s.is_empty())
}

/// v1.84.0: the visibility rule of [`SqliteMemoryEngine::ids_hidden_from_conversation`]
/// as one SQL boolean expression, so a query can apply it BEFORE its `LIMIT`.
///
/// `row` is the alias (or table name) of the `memories` row being judged;
/// `session` is the SQL expression holding the conversation key (a bound
/// parameter such as `?5`; bind `NULL` for an unknown conversation). Both
/// are fixed SQL fragments chosen by the caller, never user input; the
/// conversation key itself is always bound.
///
/// True when the row is explicitly stored ([`EXPLICIT_SOURCE_EVENTS`]), or
/// has at least one `direct` / `inherited` origin and every such origin is
/// an operator import item or comes from `session`.
///
/// Built from the same pieces as the Rust rule ([`EXPLICIT_SOURCE_EVENTS`],
/// [`CONTENT_ROLES_SQL`], `SourceKind::ImportItem`); the parity test
/// `sql_predicate_matches_the_rust_rule` runs both over every branch.
pub fn visible_to_conversation_sql(row: &str, session: &str) -> String {
    let explicit = EXPLICIT_SOURCE_EVENTS
        .iter()
        .map(|e| format!("'{e}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let import = crate::lineage::SourceKind::ImportItem.as_str();
    format!(
        "(TRIM(COALESCE({row}.source_event, '')) IN ({explicit})
          OR (EXISTS (SELECT 1 FROM memory_origins vis_any
                      WHERE vis_any.memory_store = 'memories' AND vis_any.memory_id = {row}.id
                        AND vis_any.agent_id = {row}.agent_id
                        AND vis_any.role IN ({CONTENT_ROLES_SQL}))
              AND NOT EXISTS (SELECT 1 FROM memory_origins vis_foreign
                      WHERE vis_foreign.memory_store = 'memories' AND vis_foreign.memory_id = {row}.id
                        AND vis_foreign.agent_id = {row}.agent_id
                        AND vis_foreign.role IN ({CONTENT_ROLES_SQL})
                        AND vis_foreign.source_kind != '{import}'
                        AND ({session} IS NULL OR vis_foreign.source_session != {session}))))"
    )
}

/// Which rows a read (or a write's duplicate / supersession lookup) may
/// consider (v1.84.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowScope<'a> {
    /// Every row of the namespace (employee-wide readers; behaviour unchanged).
    All,
    /// Only the rows a reader serving this conversation may see
    /// ([`visible_to_conversation_sql`]); `None` = the conversation is
    /// unknown (only explicitly stored and import-only rows).
    Conversation(Option<&'a str>),
}

impl<'a> RowScope<'a> {
    /// [`RowScope::Conversation`] with the key normalised ([`conversation_key`]).
    pub fn conversation(session: Option<&'a str>) -> Self {
        Self::Conversation(conversation_key(session))
    }

    /// Whether a visibility filter applies.
    pub fn is_scoped(&self) -> bool {
        matches!(self, Self::Conversation(_))
    }

    /// The conversation key to bind at the parameter named in
    /// [`Self::and_visible`] (only bound when [`Self::is_scoped`]).
    pub fn session(&self) -> Option<&'a str> {
        match self {
            Self::All => None,
            Self::Conversation(s) => conversation_key(*s),
        }
    }

    /// `" AND <predicate>"` for the row `row` with the key bound at `param`,
    /// or `""` for [`RowScope::All`].
    pub fn and_visible(&self, row: &str, param: &str) -> String {
        match self {
            Self::All => String::new(),
            Self::Conversation(_) => format!(" AND {}", visible_to_conversation_sql(row, param)),
        }
    }
}

/// The one conversation a write belongs to, judged from its resolved
/// lineage the way a reader judges a row: `Some(session)` when every
/// non-import `direct` / `inherited` source has that session and at least one
/// of them is a conversation turn (channel message or MCP turn). `None` for
/// a host write with no conversation (system producer, dispatch run,
/// footprint, untracked parent), an import-only write, or a write derived
/// from several conversations.
pub(crate) fn conversation_of_lineage(rows: &[crate::lineage::db::OriginRow]) -> Option<String> {
    let import = crate::lineage::SourceKind::ImportItem.as_str();
    let mut session: Option<&str> = None;
    let mut has_turn = false;
    for row in rows {
        if row.role == crate::lineage::db::Role::Reaffirm || row.kind == import {
            continue;
        }
        match session {
            None => session = Some(row.session.as_str()),
            Some(s) if s == row.session => {}
            Some(_) => return None,
        }
        has_turn |= CONVERSATION_KINDS.iter().any(|k| k.as_str() == row.kind);
    }
    let session = conversation_key(session)?;
    has_turn.then(|| session.to_string())
}

/// [`conversation_of_lineage`] for a provenance that has not been resolved
/// yet, from its direct sources only. A provenance with parent rows cannot
/// be judged without the database, so it is `None` (callers then keep the
/// employee-wide behaviour, as for any host write without one conversation).
pub fn provenance_conversation(prov: &crate::lineage::Provenance) -> Option<String> {
    use crate::lineage::Provenance;
    let sources = match prov {
        Provenance::Connectors { provenance, .. } => return provenance_conversation(provenance),
        Provenance::Sources(v) => v.as_slice(),
        Provenance::Derived { parents, extra } if parents.is_empty() => extra.as_slice(),
        Provenance::Derived { .. } | Provenance::System { .. } => return None,
    };
    let rows: Vec<crate::lineage::db::OriginRow> = sources
        .iter()
        .map(|s| crate::lineage::db::OriginRow {
            kind: s.kind.as_str().to_string(),
            session: s.session.clone(),
            message: s.message.clone(),
            seq: s.seq,
            observed_at: String::new(),
            hash: None,
            role: crate::lineage::db::Role::Direct,
            via: None,
            connectors: Vec::new(),
        })
        .collect();
    conversation_of_lineage(&rows)
}

impl SqliteMemoryEngine {
    /// Of `ids` (rows of `agent_id`), the ones a reader serving conversation
    /// `session` may NOT see: every row that is not explicitly stored
    /// ([`EXPLICIT_SOURCE_EVENTS`]) unless it has at least one `direct` /
    /// `inherited` lineage row and every such row is an import item or has
    /// `source_session = session`. Explicitly stored rows are never hidden.
    ///
    /// Fail closed:
    /// * `session = None` or empty (the conversation is unknown) hides every
    ///   non-explicit row with any non-import origin;
    /// * a non-explicit row with no lineage (written before P2-B), or a
    ///   non-import source outside this conversation (another session, a
    ///   dispatch run, `system:` / `untracked`), is hidden;
    /// * an id that is not a row of `agent_id` is hidden (callers only pass
    ///   ids they just read, so this only matters for a concurrent delete).
    pub async fn ids_hidden_from_conversation(
        &self,
        agent_id: &str,
        ids: &[String],
        session: Option<&str>,
    ) -> Result<std::collections::HashSet<String>> {
        let session = conversation_key(session);
        let conn = self.conn.lock().await;
        crate::isolation_io::verify()?;
        let mut event_stmt = conn
            .prepare_cached("SELECT source_event FROM memories WHERE agent_id = ?1 AND id = ?2")
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let origin_sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(
                 source_kind != ?4 AND (?3 IS NULL OR source_session != ?3)
             ), 0)
             FROM memory_origins
             WHERE memory_store = 'memories' AND memory_id = ?2 AND agent_id = ?1
               AND role IN ({CONTENT_ROLES_SQL})"
        );
        let mut origin_stmt = conn
            .prepare_cached(&origin_sql)
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
            let (total, foreign): (i64, i64) = origin_stmt
                .query_row(
                    params![agent_id, id, session, crate::lineage::SourceKind::ImportItem.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
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

        // Unknown conversation: non-explicit conversation rows are hidden.
        for unknown in [None, Some(""), Some("  ")] {
            let hidden = engine.ids_hidden_from_conversation("a1", &ids, unknown).await.unwrap();
            assert!(hidden.contains(&a) && hidden.contains(&b), "{unknown:?}");
            assert!(!hidden.contains(&stored) && !hidden.contains(&imported), "{unknown:?}");
        }
    }

    #[tokio::test]
    async fn operator_import_lineage_is_visible_in_any_conversation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for event in ["migrate-from-openclaw", "agent-import-x", "migration-20261120-42"] {
            let imported = put(
                &engine,
                event,
                event,
                Provenance::source(SourceRef::import_item("archive", event.as_bytes(), Utc::now())),
            )
            .await;
            let inherited = put(&engine, &format!("inherited {event}"), event, Provenance::derived(vec![imported.clone()])).await;
            for session in [Some("telegram:A"), Some("telegram:B"), None, Some(""), Some("  ")] {
                let hidden = engine.ids_hidden_from_conversation("a1", &[imported.clone(), inherited.clone()], session).await.unwrap();
                assert!(hidden.is_empty(), "{event:?} {session:?}: {hidden:?}");
            }
        }
    }

    #[tokio::test]
    async fn unknown_event_without_origins_is_hidden() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let row = put(&engine, "untracked import", "unknown_event", Provenance::test_only()).await;
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("DELETE FROM memory_origins WHERE memory_id = ?1", params![row])
                .unwrap();
        }
        for session in [Some("telegram:A"), Some("telegram:B"), None] {
            let hidden = engine.ids_hidden_from_conversation("a1", &[row.clone()], session).await.unwrap();
            assert!(hidden.contains(&row), "{session:?}");
        }
    }

    #[tokio::test]
    async fn mixed_import_and_conversation_origins_stay_in_their_conversation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let imported = put(
            &engine,
            "imported policy",
            "migrate-from-openclaw",
            Provenance::source(SourceRef::import_item("archive", b"policy", Utc::now())),
        )
        .await;
        let conversation = put(&engine, "conversation fact", "conversation_distill", from_session("telegram:A", 1)).await;
        let mixed = put(
            &engine,
            "combined policy",
            "unknown_event",
            Provenance::derived(vec![imported, conversation.clone()]),
        )
        .await;
        let ids = vec![mixed.clone(), conversation.clone()];
        let in_a = engine.ids_hidden_from_conversation("a1", &ids, Some("telegram:A")).await.unwrap();
        assert!(in_a.is_empty());
        for session in [Some("telegram:B"), None] {
            let hidden = engine.ids_hidden_from_conversation("a1", &ids, session).await.unwrap();
            assert!(hidden.contains(&mixed), "{session:?}");
            assert!(hidden.contains(&conversation), "{session:?}");
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

    // ── v1.84.0 ─────────────────────────────────────────────────────────────

    /// The visible set of `ids` for `session` through the SQL predicate.
    async fn sql_visible(
        engine: &SqliteMemoryEngine,
        session: Option<&str>,
    ) -> std::collections::BTreeSet<String> {
        let conn = engine.conn_for_maintenance().await;
        let sql = format!(
            "SELECT m.id FROM memories AS m WHERE m.agent_id = 'a1'{}",
            RowScope::conversation(session).and_visible("m", "?1")
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows = stmt
            .query_map(params![conversation_key(session)], |r| r.get::<_, String>(0))
            .unwrap();
        let visible: std::collections::BTreeSet<String> = rows.map(|r| r.unwrap()).collect();
        visible
    }

    /// The SQL predicate (`visible_to_conversation_sql`, used inside every
    /// scoped query) and the Rust rule (`ids_hidden_from_conversation`) give
    /// the same answer on every branch: explicit events (padded, tabbed,
    /// NULL), direct / inherited / reaffirm roles, import-item origins, no
    /// origins, mixed sessions, MCP turns, and known / unknown sessions.
    #[tokio::test]
    async fn sql_predicate_matches_the_rust_rule() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let a = put(&engine, "direct a", "conversation_distill", from_session("telegram:A", 1)).await;
        let b = put(&engine, "direct b", "conversation_distill", from_session("telegram:B", 2)).await;
        let explicit = put(&engine, "explicit", "mcp_internal", from_session("telegram:B", 3)).await;
        // Spelled exactly as stored below (set directly, not through a writer).
        let padded = put(&engine, "padded explicit", "conversation_distill", from_session("telegram:B", 4)).await;
        let tabbed = put(&engine, "tabbed explicit", "conversation_distill", from_session("telegram:B", 5)).await;
        let import_event = put(&engine, "import event", "import", Provenance::test_only()).await;
        let explicit_bare = put(&engine, "explicit bare", "import", Provenance::test_only()).await;
        let inherited_a =
            put(&engine, "inherited a", "user_profile_consolidation", Provenance::derived(vec![a.clone()])).await;
        let inherited_ab = put(
            &engine,
            "inherited ab",
            "user_profile_consolidation",
            Provenance::derived(vec![a.clone(), b.clone()]),
        )
        .await;
        let reaffirmed = put(&engine, "reaffirmed", "user_profile", from_session("telegram:A", 6)).await;
        let import_only = put(
            &engine,
            "import only",
            "migrate-from-x",
            Provenance::source(SourceRef::import_item("archive", b"x", Utc::now())),
        )
        .await;
        let import_and_a =
            put(&engine, "import and a", "unknown_event", Provenance::derived(vec![import_only.clone(), a.clone()])).await;
        let no_origins = put(&engine, "no origins", "conversation_distill", from_session("telegram:A", 7)).await;
        let system = put(&engine, "system", "footprint_distill", Provenance::test_only()).await;
        let reaffirm_only = put(&engine, "reaffirm only", "conversation_distill", from_session("telegram:A", 8)).await;
        let null_event = put(&engine, "null event", "conversation_distill", from_session("telegram:A", 9)).await;
        let turn = put(
            &engine,
            "mcp turn",
            "user_profile",
            Provenance::source(SourceRef::other(crate::lineage::SourceKind::McpTurn, "telegram:A", "turn:t1", Utc::now())),
        )
        .await;
        {
            let conn = engine.conn_for_maintenance().await;
            let now = crate::lineage::format_ts(Utc::now());
            conn.execute(
                "INSERT INTO memory_origins (memory_store, memory_id, agent_id, source_kind,
                    source_session, source_message, source_seq, source_observed_at, role, created_at)
                 VALUES ('memories', ?1, 'a1', 'channel_message', 'telegram:B', 'm:99', 99, ?2, 'reaffirm', ?2)",
                params![reaffirmed, now],
            )
            .unwrap();
            for id in [&no_origins, &explicit_bare] {
                conn.execute("DELETE FROM memory_origins WHERE memory_id = ?1", params![id]).unwrap();
            }
            conn.execute("UPDATE memory_origins SET role = 'reaffirm' WHERE memory_id = ?1", params![reaffirm_only])
                .unwrap();
            conn.execute("UPDATE memories SET source_event = NULL WHERE id = ?1", params![null_event]).unwrap();
            for (id, event) in [(&padded, " mcp_internal "), (&tabbed, "\tmcp_internal")] {
                conn.execute("UPDATE memories SET source_event = ?1 WHERE id = ?2", params![event, id]).unwrap();
            }
        }
        let ids = vec![
            a.clone(), b.clone(), explicit.clone(), padded.clone(), tabbed.clone(), import_event.clone(),
            explicit_bare.clone(), inherited_a.clone(), inherited_ab.clone(), reaffirmed.clone(),
            import_only.clone(), import_and_a.clone(), no_origins.clone(), system.clone(),
            reaffirm_only.clone(), null_event.clone(), turn.clone(),
        ];
        for session in [
            Some("telegram:A"),
            Some("telegram:B"),
            Some("telegram:C"),
            Some(" telegram:A "),
            None,
            Some(""),
            Some("  "),
        ] {
            let hidden = engine.ids_hidden_from_conversation("a1", &ids, session).await.unwrap();
            let rust: std::collections::BTreeSet<String> =
                ids.iter().filter(|id| !hidden.contains(*id)).cloned().collect();
            assert_eq!(rust, sql_visible(&engine, session).await, "{session:?}");
        }

        // The branches are really exercised (not vacuously equal).
        let in_a = sql_visible(&engine, Some("telegram:A")).await;
        for id in [&a, &explicit, &padded, &import_event, &explicit_bare, &inherited_a, &reaffirmed, &import_only, &import_and_a, &null_event, &turn] {
            assert!(in_a.contains(id), "{id}");
        }
        for id in [&b, &tabbed, &inherited_ab, &no_origins, &system, &reaffirm_only] {
            assert!(!in_a.contains(id), "{id}");
        }
        let unknown = sql_visible(&engine, None).await;
        let expected: std::collections::BTreeSet<String> =
            [&explicit, &padded, &import_event, &explicit_bare, &import_only].into_iter().cloned().collect();
        assert_eq!(unknown, expected);
        // Constants are spliced into SQL: they must stay plain identifiers.
        for e in EXPLICIT_SOURCE_EVENTS {
            assert!(e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{e}");
        }
    }

    fn triple(subject: &str, predicate: &str, object: &str, origin: &str) -> TemporalMeta {
        TemporalMeta {
            subject: Some(subject.into()),
            predicate: Some(predicate.into()),
            object: Some(object.into()),
            origin: Some(origin.into()),
            ..Default::default()
        }
    }

    async fn put_triple(
        engine: &SqliteMemoryEngine,
        event: &str,
        meta: TemporalMeta,
        prov: Provenance,
    ) -> crate::supersession_guard::TemporalWriteOutcome {
        let content = format!(
            "{}: {}",
            meta.predicate.clone().unwrap_or_default(),
            meta.object.clone().unwrap_or_default()
        );
        engine.store_temporal_outcome("a1", entry(&content, event), meta, prov).await.unwrap()
    }

    fn stored(o: crate::supersession_guard::TemporalWriteOutcome) -> String {
        match o {
            crate::supersession_guard::TemporalWriteOutcome::Stored(id) => id,
            other => panic!("not stored: {other:?}"),
        }
    }

    async fn valid_ids(engine: &SqliteMemoryEngine, subject: &str, predicate: &str) -> Vec<String> {
        engine
            .get_history("a1", subject, predicate)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.valid_until.is_none())
            .map(|r| r.id)
            .collect()
    }

    /// Item 5: a host-derived write of conversation B does not expire A's
    /// value for the same triple; each conversation reads its own value
    /// (`get_at_scoped`); a later write of A replaces only A's; an explicit
    /// `mcp_internal` write still supersedes every conversation's value.
    #[tokio::test]
    async fn host_supersession_stays_in_its_conversation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let s = "user:telegram:alice";
        let a = stored(put_triple(&engine, "user_profile", triple(s, "prefers", "tea", "user_profile"), from_session("telegram:A", 1)).await);
        let b = stored(put_triple(&engine, "user_profile", triple(s, "prefers", "coffee", "user_profile"), from_session("telegram:B", 2)).await);
        let mut both = valid_ids(&engine, s, "prefers").await;
        both.sort();
        let mut expected = vec![a.clone(), b.clone()];
        expected.sort();
        assert_eq!(both, expected, "B's write must not expire A's row");

        let at = Utc::now();
        for (session, id) in [("telegram:A", &a), ("telegram:B", &b)] {
            let got = engine
                .get_at_scoped("a1", s, "prefers", at, RowScope::conversation(Some(session)))
                .await
                .unwrap()
                .expect(session);
            assert_eq!(&got.id, id, "{session}");
        }
        assert!(engine
            .get_at_scoped("a1", s, "prefers", at, RowScope::conversation(Some("telegram:C")))
            .await
            .unwrap()
            .is_none());

        let a2 = stored(put_triple(&engine, "user_profile", triple(s, "prefers", "green tea", "user_profile"), from_session("telegram:A", 3)).await);
        let mut now_valid = valid_ids(&engine, s, "prefers").await;
        now_valid.sort();
        let mut expected = vec![a2.clone(), b.clone()];
        expected.sort();
        assert_eq!(now_valid, expected, "A's update replaces A's value only");

        let explicit = stored(put_triple(&engine, "mcp_internal", triple(s, "prefers", "water", "user_profile"), from_session("telegram:A", 4)).await);
        assert_eq!(valid_ids(&engine, s, "prefers").await, vec![explicit], "explicit writes supersede globally");
    }

    /// Item 2 / 5: the supersession trust guard only compares against rows
    /// the writing conversation may see — a more trusted value in A neither
    /// refuses B's write nor is named to it; inside A it still protects.
    #[tokio::test]
    async fn trust_guard_compares_only_visible_rows() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let s = "user:telegram:alice";
        stored(put_triple(&engine, "user_profile", triple(s, "allergy", "peanuts", "operator"), from_session("telegram:A", 1)).await);
        let in_b = put_triple(&engine, "user_profile", triple(s, "allergy", "none", "channel"), from_session("telegram:B", 2)).await;
        assert!(matches!(in_b, crate::supersession_guard::TemporalWriteOutcome::Stored(_)), "{in_b:?}");
        let in_a = put_triple(&engine, "user_profile", triple(s, "allergy", "none", "channel"), from_session("telegram:A", 3)).await;
        assert!(matches!(in_a, crate::supersession_guard::TemporalWriteOutcome::Refused(_)), "{in_a:?}");
    }

    /// Item 3: many newer / better-ranked rows of other conversations and
    /// one own row; with the rule inside the SQL a limit of 1 returns the own
    /// row (`search_scoped`, `search_layer_scoped`), and an unknown
    /// conversation gets nothing.
    #[tokio::test]
    async fn scoped_search_is_not_crowded_out() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut own = entry("gannetcliff own note", "conversation_distill");
        own.importance = 1.0;
        let own = engine
            .store_temporal("a1", own, TemporalMeta::default(), from_session("telegram:A", 1))
            .await
            .unwrap();
        for i in 0..40 {
            let mut other = entry(&format!("gannetcliff other note {i}"), "conversation_distill");
            other.importance = 9.0;
            engine
                .store_temporal("a1", other, TemporalMeta::default(), from_session("telegram:B", 10 + i))
                .await
                .unwrap();
        }
        let got = engine
            .search_scoped("a1", "gannetcliff", 1, RowScope::conversation(Some("telegram:A")))
            .await
            .unwrap();
        assert_eq!(got.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec![own.clone()]);
        let got = engine
            .search_layer_scoped("a1", "gannetcliff", &MemoryLayer::Semantic, 1, RowScope::conversation(Some("telegram:A")))
            .await
            .unwrap();
        assert_eq!(got.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec![own]);
        assert!(engine
            .search_scoped("a1", "gannetcliff", 5, RowScope::conversation(None))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(engine.search_scoped("a1", "gannetcliff", 5, RowScope::All).await.unwrap().len(), 5);
    }

    /// Item 2: a near-duplicate that only another conversation holds does not
    /// block this conversation's write; one this conversation can see does,
    /// and the refusal is recognisable without parsing its id or cosine.
    #[tokio::test]
    async fn novelty_gate_only_sees_this_conversations_rows() {
        let engine = SqliteMemoryEngine::in_memory()
            .unwrap()
            .with_embedder(std::sync::Arc::new(crate::vector::NgramHashEmbedder::new()));
        let text = "the customer prefers invoices in Traditional Chinese";
        engine
            .store_temporal("a1", entry(text, "conversation_distill"), TemporalMeta::default(), from_session("telegram:A", 1))
            .await
            .unwrap();
        // B cannot see A's row: its identical fact is stored.
        engine
            .store_temporal("a1", entry(text, "conversation_distill"), TemporalMeta::default(), from_session("telegram:B", 2))
            .await
            .expect("a row hidden from B must not block B's write");
        // A sees its own row: refused.
        let err = engine
            .store_temporal("a1", entry(text, "conversation_distill"), TemporalMeta::default(), from_session("telegram:A", 3))
            .await
            .unwrap_err();
        assert!(crate::novelty_gate::is_novelty_rejection(&err), "{err}");
        assert!(!crate::novelty_gate::is_novelty_rejection(&DuDuClawError::Memory(
            "xnovelty gate rejected write: y".into()
        )));
    }

    /// Review L2: in a conversation the graph ranking is built from the
    /// triples that conversation may see. A hidden triple of B that links the
    /// query's seed entity to A's second row (which would hand it PPR mass
    /// and re-order A's two rows employee-wide) changes neither A's graph
    /// scores (bit for bit) nor the order of A's results; employee-wide the
    /// hidden row is part of the graph.
    #[tokio::test]
    async fn scoped_graph_ranking_ignores_hidden_triples() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let put_row = |content: &'static str, meta: TemporalMeta, session: &'static str, seq: i64| {
            let engine = &engine;
            async move {
                stored(
                    engine
                        .store_temporal_outcome("a1", entry(content, "user_profile"), meta, from_session(session, seq))
                        .await
                        .unwrap(),
                )
            }
        };
        let x = put_row("zephyr note likes tea", triple("zephyr", "likes", "tea", "user_profile"), "telegram:A", 1).await;
        let y = put_row(
            "zephyr note kumquat orchard",
            triple("kumquat", "grows_in", "orchard", "user_profile"),
            "telegram:A",
            2,
        )
        .await;
        let in_a = RowScope::conversation(Some("telegram:A"));
        let graph_bits = |scope: RowScope<'static>| {
            let engine = &engine;
            async move {
                let conn = engine.conn_for_maintenance().await;
                let now = Utc::now().to_rfc3339();
                let ranked = engine.graph_rank_scoped(&conn, "a1", "zephyr note", &now, scope).unwrap();
                let bits: Vec<(String, u64)> =
                    ranked.unwrap_or_default().into_iter().map(|(id, s)| (id, s.to_bits())).collect();
                bits
            }
        };
        let order = || {
            let engine = &engine;
            async move {
                let got = engine.search_scoped("a1", "zephyr note", 2, in_a).await.unwrap();
                got.into_iter().map(|e| e.id).collect::<Vec<_>>()
            }
        };
        let graph_before = graph_bits(in_a).await;
        assert!(graph_before.iter().any(|(id, _)| *id == x), "the seed reaches A's row: {graph_before:?}");
        let order_before = order().await;
        assert_eq!(order_before.len(), 2);
        assert!(order_before.contains(&x) && order_before.contains(&y));

        let hidden =
            put_row("unrelated text", triple("zephyr", "owns", "kumquat", "user_profile"), "telegram:B", 3).await;
        assert_eq!(graph_bits(in_a).await, graph_before, "a hidden triple changes no scoped graph score");
        assert_eq!(order().await, order_before, "a hidden triple does not re-order A's rows");
        let employee = graph_bits(RowScope::All).await;
        assert!(employee.iter().any(|(id, _)| *id == hidden), "employee-wide the triple is in the graph");
    }

    /// Review M2: a held claim of conversation B does not fold into
    /// conversation A's pending held claim for the same triple — two held
    /// rows, each with its own conversation's lineage; a repeat inside A
    /// still dedups against A's own row; an explicit claim dedups
    /// employee-wide as before.
    #[tokio::test]
    async fn pending_held_claim_dedup_stays_in_its_conversation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let s = "policy:refund";
        let hold = |session: &'static str, seq: i64, event: &'static str| {
            let engine = &engine;
            async move {
                engine
                    .hold_refused_claim_outcome(
                        "a1",
                        entry("window: forever", event),
                        triple(s, "window", "forever", "channel"),
                        from_session(session, seq),
                    )
                    .await
                    .unwrap()
            }
        };
        let a = hold("telegram:A", 1, "conversation_distill").await;
        let b = hold("telegram:B", 2, "conversation_distill").await;
        assert!(a.newly_held && b.newly_held, "{a:?} {b:?}");
        assert_ne!(a.id, b.id, "B must not receive A's held id");
        let again = hold("telegram:A", 3, "conversation_distill").await;
        assert_eq!((again.id.clone(), again.newly_held), (a.id.clone(), false));

        let sessions = |id: String| {
            let engine = &engine;
            async move {
                let conn = engine.conn_for_maintenance().await;
                let mut stmt = conn
                    .prepare(
                        "SELECT DISTINCT source_session FROM memory_origins
                         WHERE memory_store = 'memories' AND memory_id = ?1
                           AND role IN ('direct', 'inherited')",
                    )
                    .unwrap();
                let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0)).unwrap();
                let got: Vec<String> = rows.map(|r| r.unwrap()).collect();
                got
            }
        };
        assert_eq!(sessions(a.id.clone()).await, vec!["telegram:A".to_string()]);
        assert_eq!(sessions(b.id.clone()).await, vec!["telegram:B".to_string()]);

        // Explicitly stored claims keep the employee-wide dedup.
        let explicit = hold("telegram:C", 4, "mcp_internal").await;
        assert_eq!((explicit.id, explicit.newly_held), (a.id, false));
    }

    #[test]
    fn write_conversation_follows_the_lineage() {
        let row = |kind: &str, session: &str, role: crate::lineage::db::Role| crate::lineage::db::OriginRow {
            kind: kind.into(),
            session: session.into(),
            message: "m:1".into(),
            seq: None,
            observed_at: String::new(),
            hash: None,
            role,
            via: None,
            connectors: Vec::new(),
        };
        use crate::lineage::db::Role::{Direct, Inherited, Reaffirm};
        let one = |rows: Vec<crate::lineage::db::OriginRow>| conversation_of_lineage(&rows);
        assert_eq!(one(vec![row("channel_message", "telegram:A", Direct)]), Some("telegram:A".into()));
        assert_eq!(
            one(vec![row("mcp_turn", "telegram:A", Direct), row("import_item", "import:x", Inherited)]),
            Some("telegram:A".into())
        );
        assert_eq!(
            one(vec![row("channel_message", "telegram:A", Direct), row("channel_message", "telegram:B", Reaffirm)]),
            Some("telegram:A".into())
        );
        assert_eq!(one(vec![row("channel_message", "telegram:A", Direct), row("channel_message", "telegram:B", Inherited)]), None);
        assert_eq!(one(vec![row("system", "system:x", Direct)]), None);
        assert_eq!(one(vec![row("dispatch_run", "dispatch:a1", Direct)]), None);
        assert_eq!(one(vec![row("import_item", "import:x", Direct)]), None);
        assert_eq!(one(vec![]), None);
    }
}
