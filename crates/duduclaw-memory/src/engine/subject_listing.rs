//! RM-039: rows *about a person*, across every namespace in one database.
//!
//! A person's facts are keyed by `subject` (`user:<id>`, see
//! [`crate::user_profile::user_subject`]) but stored in the namespace of the
//! AI employee that learned them. The gateway's "my memory" RPCs resolve the
//! caller's own subjects and use these two reads to list and look up exactly
//! those rows — the subject filter is the authorization boundary, so it is
//! applied in SQL and never widened here.
//!
//! The listing is served by `idx_memories_subject_ts`
//! (`memories(subject, timestamp DESC, id DESC)`, created in `init_tables`),
//! so a page is an index range scan per subject rather than a table scan.
//!
//! [`SqliteMemoryEngine::forget_subject_triple`] is the matching delete: a
//! person removing a fact about themselves removes every version of it.

use super::*;

/// Upper bound on the subjects one query may name.
pub const MAX_SUBJECTS: usize = 64;

/// SQLite IN-list chunk size (well under the 999 bound-variable limit).
const ID_CHUNK: usize = 400;

/// Which rows a subject forget selects.
#[derive(Debug, Clone, Copy)]
enum Selector<'a> {
    /// Every version of `(subject, predicate)`.
    Triple(&'a str),
    /// One predicate-less row about the subject, by id.
    Row(&'a str),
}

/// What [`SqliteMemoryEngine::forget_subject_triple`] removed. Counts and ids
/// only — never content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SubjectTripleForgetReport {
    /// Rows removed from `memories` (current, superseded, quarantined and
    /// held-claim versions of the triple).
    pub memories_deleted: u64,
    /// Copies removed from `memories_archive` (left there by an earlier
    /// soft `forget` or by decay).
    pub archive_deleted: u64,
    /// `memory_origins` lineage rows removed.
    pub lineage_deleted: u64,
    /// Ids of the removed `memories` rows.
    pub deleted_ids: Vec<String>,
    /// The subset of `deleted_ids` that was quarantined / held for review —
    /// the caller withdraws their review cards.
    pub pending_ids: Vec<String>,
}

/// One memory row whose `subject` names a person.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubjectFact {
    pub id: String,
    /// Namespace (AI employee) the row lives in.
    pub agent_id: String,
    pub content: String,
    /// Stored timestamp (RFC 3339), the keyset cursor's first half.
    pub timestamp: String,
    /// `""` when the row has no subject (only possible from [`SqliteMemoryEngine::subject_fact_by_id`]).
    pub subject: String,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub origin: Option<String>,
    pub source_event: String,
    pub metadata: serde_json::Value,
}

const COLS: &str = "m.id, m.agent_id, m.content, m.timestamp, COALESCE(m.subject, ''), \
                    m.predicate, m.object, m.origin, COALESCE(m.source_event, ''), \
                    COALESCE(m.metadata, '{}')";

fn row_to_subject_fact(r: &rusqlite::Row<'_>) -> rusqlite::Result<SubjectFact> {
    let metadata: String = r.get(9)?;
    Ok(SubjectFact {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        content: r.get(2)?,
        timestamp: r.get(3)?,
        subject: r.get(4)?,
        predicate: r.get(5)?,
        object: r.get(6)?,
        origin: r.get(7)?,
        source_event: r.get(8)?,
        metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| serde_json::json!({})),
    })
}

impl SqliteMemoryEngine {
    /// Currently valid, non-quarantined rows whose `subject` is exactly one of
    /// `subjects` (any namespace), newest first. `before` is a keyset cursor
    /// `(timestamp, id)` of the last row of the previous page.
    pub async fn list_current_by_subjects(
        &self,
        subjects: &[String],
        before: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<Vec<SubjectFact>> {
        if subjects.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if subjects.len() > MAX_SUBJECTS {
            return Err(DuDuClawError::Memory("too many subjects".into()));
        }
        use rusqlite::types::Value as Sql;
        let mut args: Vec<Sql> = vec![
            Sql::Text(Utc::now().to_rfc3339()),
            Sql::Integer(limit.min(1_000) as i64),
        ];
        let first_subject = args.len() + 1;
        let placeholders = (0..subjects.len())
            .map(|i| format!("?{}", first_subject + i))
            .collect::<Vec<_>>()
            .join(", ");
        args.extend(subjects.iter().map(|s| Sql::Text(s.clone())));
        let cursor = match before {
            Some((ts, id)) => {
                let t = args.len() + 1;
                args.push(Sql::Text(ts.to_string()));
                args.push(Sql::Text(id.to_string()));
                format!(
                    "AND (m.timestamp < ?{t} OR (m.timestamp = ?{t} AND m.id < ?{i}))",
                    i = t + 1
                )
            }
            None => String::new(),
        };
        let sql = format!(
            "SELECT {COLS} FROM memories AS m
             WHERE m.subject IN ({placeholders})
               AND (m.valid_until IS NULL OR m.valid_until > ?1)
               AND m.quarantined = 0
               {cursor}
             ORDER BY m.timestamp DESC, m.id DESC
             LIMIT ?2"
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), row_to_subject_fact)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(out)
    }

    /// One row by id in any namespace (the caller checks its `subject`).
    pub async fn subject_fact_by_id(&self, memory_id: &str) -> Result<Option<SubjectFact>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {COLS} FROM memories AS m WHERE m.id = ?1 LIMIT 1"),
            params![memory_id],
            row_to_subject_fact,
        )
        .optional()
        .map_err(|e| DuDuClawError::Memory(e.to_string()))
    }

    /// RM-039 `me.memory.forget`: **hard**-delete every version of the fact
    /// `(subject, predicate)` in `agent_id`'s namespace — the current row, all
    /// superseded / expired versions, quarantined rows pending review, and
    /// held claims whose triple lives in `metadata.held_claim` — so neither
    /// `get_history` / `get_at` (MCP `memory_get_history` / `memory_get_at`,
    /// `memory.history`) nor a later quarantine approval can bring any of them
    /// back.
    ///
    /// Unlike [`forget`](SqliteMemoryEngine::forget) nothing is copied into
    /// `memories_archive`; archive copies an earlier soft forget or decay left
    /// behind are removed too — by id, for the deleted rows and for any
    /// version the chain's `superseded_by` / `supersedes` links still name —
    /// as are the rows' FTS entries, `memory_origins` lineage and connector
    /// stamps. (An archived version no live row links to any more cannot be
    /// attributed — the archive keeps no triple — and is left to the decay
    /// retention sweep.) Links from other rows into the deleted chain are
    /// cleared. One `BEGIN IMMEDIATE` transaction; idempotent (a second call
    /// reports zero).
    pub async fn forget_subject_triple(
        &self,
        agent_id: &str,
        subject: &str,
        predicate: &str,
    ) -> Result<SubjectTripleForgetReport> {
        if subject.is_empty() || predicate.is_empty() {
            return Err(DuDuClawError::Memory(
                "forget_subject_triple: subject and predicate are required".into(),
            ));
        }
        self.forget_subject_locked_txn(agent_id, subject, Selector::Triple(predicate))
            .await
    }

    /// [`forget_subject_triple`](Self::forget_subject_triple) for a row about
    /// `subject` that has **no predicate** (so it has no version chain): the
    /// one row is hard-deleted with the same cleanup and no archive copy.
    /// Nothing happens unless the row's `subject` is exactly `subject`.
    pub async fn forget_subject_row(
        &self,
        agent_id: &str,
        subject: &str,
        memory_id: &str,
    ) -> Result<SubjectTripleForgetReport> {
        if subject.is_empty() || memory_id.is_empty() {
            return Err(DuDuClawError::Memory(
                "forget_subject_row: subject and id are required".into(),
            ));
        }
        self.forget_subject_locked_txn(agent_id, subject, Selector::Row(memory_id))
            .await
    }

    async fn forget_subject_locked_txn(
        &self,
        agent_id: &str,
        subject: &str,
        selector: Selector<'_>,
    ) -> Result<SubjectTripleForgetReport> {
        let conn = self.conn.lock().await;
        let report = Self::in_immediate_txn(&conn, || {
            Self::forget_subject_triple_locked(&conn, agent_id, subject, selector)
        })?;
        drop(conn);
        if report.memories_deleted > 0 {
            self.bump_graph_generation(agent_id);
        }
        Ok(report)
    }

    fn forget_subject_triple_locked(
        conn: &Connection,
        agent_id: &str,
        subject: &str,
        selector: Selector<'_>,
    ) -> Result<SubjectTripleForgetReport> {
        let e = |e: rusqlite::Error| DuDuClawError::Memory(e.to_string());
        let mut report = SubjectTripleForgetReport::default();
        type Row = (String, String, Option<String>, Option<String>, i64);
        let (sql, third) = match selector {
            Selector::Triple(predicate) => (
                "SELECT id, content, superseded_by, supersedes, quarantined FROM memories
                 WHERE agent_id = ?1 AND (
                     (subject = ?2 AND predicate = ?3)
                  OR ((CASE WHEN json_valid(metadata)
                            THEN json_extract(metadata, '$.held_claim.subject') END) = ?2
                      AND (CASE WHEN json_valid(metadata)
                                THEN json_extract(metadata, '$.held_claim.predicate') END) = ?3))",
                predicate,
            ),
            Selector::Row(id) => (
                "SELECT id, content, superseded_by, supersedes, quarantined FROM memories
                 WHERE agent_id = ?1 AND subject = ?2 AND id = ?3",
                id,
            ),
        };
        let rows: Vec<Row> = {
            let mut stmt = conn.prepare(sql).map_err(e)?;
            let rows = stmt
                .query_map(params![agent_id, subject, third], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    ))
                })
                .map_err(e)?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row.map_err(e)?);
            }
            v
        };
        let ids: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        report.pending_ids = rows
            .iter()
            .filter(|r| r.4 != 0)
            .map(|r| r.0.clone())
            .collect();
        // Supersession only links versions of one triple, so an id a deleted
        // row points at that is no longer in `memories` is an older version
        // a soft forget or decay already moved to the archive.
        let mut linked: Vec<String> = Vec::new();
        for (_, _, by, sup, _) in &rows {
            for l in [by, sup].into_iter().flatten() {
                if !ids.contains(l) && !linked.contains(l) {
                    linked.push(l.clone());
                }
            }
        }
        let has_archive = crate::lineage::db::table_exists(conn, "main", "memories_archive")?;
        let has_stamps = crate::lineage::db::table_exists(conn, "main", "memory_connector_stamps")?;

        for chunk in ids.chunks(ID_CHUNK) {
            // ?1 = agent, ?2.. = ids
            let in_list = (2..=chunk.len() + 1)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&agent_id];
            bind.extend(chunk.iter().map(|id| id as &dyn rusqlite::ToSql));
            conn.execute(
                &format!("DELETE FROM memories_fts WHERE agent_id = ?1 AND memory_id IN ({in_list})"),
                bind.as_slice(),
            )
            .map_err(e)?;
            report.memories_deleted += conn
                .execute(
                    &format!("DELETE FROM memories WHERE agent_id = ?1 AND id IN ({in_list})"),
                    bind.as_slice(),
                )
                .map_err(e)? as u64;
            if has_archive {
                report.archive_deleted += conn
                    .execute(
                        &format!(
                            "DELETE FROM memories_archive WHERE agent_id = ?1 AND id IN ({in_list})"
                        ),
                        bind.as_slice(),
                    )
                    .map_err(e)? as u64;
            }
            report.lineage_deleted += conn
                .execute(
                    &format!(
                        "DELETE FROM memory_origins
                         WHERE memory_store = 'memories' AND agent_id = ?1 AND memory_id IN ({in_list})"
                    ),
                    bind.as_slice(),
                )
                .map_err(e)? as u64;
            if has_stamps {
                // Stamps are keyed by (store, id) only; ?1 is unused there.
                conn.execute(
                    &format!(
                        "DELETE FROM memory_connector_stamps
                         WHERE ?1 IS NOT NULL AND memory_store = 'memories' AND memory_id IN ({in_list})"
                    ),
                    bind.as_slice(),
                )
                .map_err(e)?;
            }
            // Rows outside the chain that still point into it.
            conn.execute(
                &format!(
                    "UPDATE memories SET superseded_by = NULL
                     WHERE agent_id = ?1 AND superseded_by IN ({in_list})"
                ),
                bind.as_slice(),
            )
            .map_err(e)?;
            conn.execute(
                &format!(
                    "UPDATE memories SET supersedes = NULL
                     WHERE agent_id = ?1 AND supersedes IN ({in_list})"
                ),
                bind.as_slice(),
            )
            .map_err(e)?;
        }

        // The archive keeps no triple (and profile rows read `predicate:
        // object`, without the subject), so it is never matched by text — that
        // could remove another person's identical archived row. Older versions
        // are found through the chain's links instead: their
        // archive copy and their (orphaned) lineage. A linked id that is still
        // a live row is left alone.
        for chunk in linked.chunks(ID_CHUNK) {
            let in_list = (2..=chunk.len() + 1)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&agent_id];
            bind.extend(chunk.iter().map(|id| id as &dyn rusqlite::ToSql));
            if has_archive {
                report.archive_deleted += conn
                    .execute(
                        &format!(
                            "DELETE FROM memories_archive WHERE agent_id = ?1 AND id IN ({in_list})"
                        ),
                        bind.as_slice(),
                    )
                    .map_err(e)? as u64;
            }
            report.lineage_deleted += conn
                .execute(
                    &format!(
                        "DELETE FROM memory_origins
                         WHERE memory_store = 'memories' AND agent_id = ?1 AND memory_id IN ({in_list})
                           AND NOT EXISTS (SELECT 1 FROM memories WHERE memories.id = memory_origins.memory_id)"
                    ),
                    bind.as_slice(),
                )
                .map_err(e)? as u64;
        }
        report.deleted_ids = ids;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TemporalMeta;
    use duduclaw_core::types::MemoryLayer;

    async fn put(engine: &SqliteMemoryEngine, agent: &str, subject: &str, pred: &str, obj: &str) -> String {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: format!("{pred}: {obj}"),
            timestamp: Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "user_profile".to_string(),
        };
        let meta = TemporalMeta {
            subject: Some(subject.to_string()),
            predicate: Some(pred.to_string()),
            object: Some(obj.to_string()),
            ..TemporalMeta::default()
        };
        engine
            .store_temporal(agent, entry, meta, crate::lineage::Provenance::test_only())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn lists_only_the_named_subjects_across_namespaces_with_a_cursor() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        put(&engine, "a1", "user:alice", "prefers", "tea").await;
        put(&engine, "a2", "user:alice", "prefers_food", "noodles").await;
        put(&engine, "a1", "user:bob", "prefers", "coffee").await;
        put(&engine, "a1", "user:alice2", "prefers", "juice").await;

        let alice = vec!["user:alice".to_string()];
        let all = engine.list_current_by_subjects(&alice, None, 10).await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|f| f.subject == "user:alice"));
        let agents: std::collections::BTreeSet<_> = all.iter().map(|f| f.agent_id.as_str()).collect();
        assert_eq!(agents.len(), 2, "both namespaces");

        let page1 = engine.list_current_by_subjects(&alice, None, 1).await.unwrap();
        let last = &page1[0];
        let page2 = engine
            .list_current_by_subjects(&alice, Some((&last.timestamp, &last.id)), 1)
            .await
            .unwrap();
        assert_eq!(page2.len(), 1);
        assert_ne!(page2[0].id, last.id);

        assert!(engine.list_current_by_subjects(&[], None, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn superseded_rows_are_not_current() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        put(&engine, "a1", "user:alice", "preferred_name", "老王").await;
        put(&engine, "a1", "user:alice", "preferred_name", "老李").await;
        let rows = engine
            .list_current_by_subjects(&["user:alice".to_string()], None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].object.as_deref(), Some("老李"));
        let by_id = engine.subject_fact_by_id(&rows[0].id).await.unwrap().unwrap();
        assert_eq!(by_id.subject, "user:alice");
    }

    async fn count(engine: &SqliteMemoryEngine, sql: &str, id: &str) -> i64 {
        let conn = engine.conn_for_maintenance().await;
        conn.query_row(sql, params![id], |r| r.get(0)).unwrap()
    }

    #[tokio::test]
    async fn forget_subject_triple_removes_every_version_without_an_archive_copy() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let v1 = put(&engine, "a1", "user:alice", "preferred_name", "老王").await;
        let v2 = put(&engine, "a1", "user:alice", "preferred_name", "老李").await;
        // An even older version a soft forget already moved to the archive.
        let v0 = put(&engine, "a1", "user:alice", "preferred_name", "阿王").await;
        assert!(engine.forget("a1", &v0).await.unwrap());
        // A quarantined (pending-review) version of the same triple.
        let quarantined = {
            let entry = MemoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                agent_id: "a1".into(),
                content: "preferred_name: 小李".into(),
                timestamp: Utc::now(),
                tags: vec![],
                embedding: None,
                layer: MemoryLayer::Semantic,
                importance: 5.0,
                access_count: 0,
                last_accessed: None,
                source_event: "user_profile".into(),
            };
            let meta = TemporalMeta {
                subject: Some("user:alice".into()),
                predicate: Some("preferred_name".into()),
                object: Some("小李".into()),
                quarantined: true,
                ..TemporalMeta::default()
            };
            engine
                .store_temporal("a1", entry, meta, crate::lineage::Provenance::test_only())
                .await
                .unwrap()
        };
        // Untouched: another predicate, another person, another namespace.
        let other_pred = put(&engine, "a1", "user:alice", "reply_style", "簡短").await;
        let other_user = put(&engine, "a1", "user:bob", "preferred_name", "老李").await;
        let other_agent = put(&engine, "a2", "user:alice", "preferred_name", "老李").await;
        assert!(engine.get_history("a1", "user:alice", "preferred_name").await.unwrap().len() >= 2);

        let report = engine
            .forget_subject_triple("a1", "user:alice", "preferred_name")
            .await
            .unwrap();
        assert_eq!(report.memories_deleted, 3, "{report:?}");
        assert!(report.deleted_ids.contains(&v1) && report.deleted_ids.contains(&v2));
        assert!(report.deleted_ids.contains(&quarantined));
        assert_eq!(report.pending_ids, vec![quarantined.clone()]);
        assert!(report.archive_deleted >= 1, "the soft-forget copy went too: {report:?}");

        assert!(engine.get_history("a1", "user:alice", "preferred_name").await.unwrap().is_empty());
        assert!(engine
            .get_at("a1", "user:alice", "preferred_name", Utc::now())
            .await
            .unwrap()
            .is_none());
        let past = Utc::now() - chrono::Duration::days(1);
        assert!(engine.get_at("a1", "user:alice", "preferred_name", past).await.unwrap().is_none());
        for id in [&v0, &v1, &v2, &quarantined] {
            assert_eq!(count(&engine, "SELECT COUNT(*) FROM memories WHERE id = ?1", id).await, 0);
            assert_eq!(
                count(&engine, "SELECT COUNT(*) FROM memories_archive WHERE id = ?1", id).await,
                0,
                "no archive copy"
            );
            assert_eq!(
                count(&engine, "SELECT COUNT(*) FROM memories_fts WHERE memory_id = ?1", id).await,
                0
            );
            assert_eq!(
                count(&engine, "SELECT COUNT(*) FROM memory_origins WHERE memory_id = ?1", id).await,
                0,
                "lineage cleaned"
            );
        }
        let conn = engine.conn_for_maintenance().await;
        let archived_text: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_archive WHERE content LIKE '%王%' OR content LIKE '%李%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(archived_text, 0, "no copy of any version survives in the archive");

        for id in [&other_pred, &other_user] {
            assert!(engine.get_by_id("a1", id).await.unwrap().is_some());
        }
        assert!(engine.get_by_id("a2", &other_agent).await.unwrap().is_some());

        let again = engine
            .forget_subject_triple("a1", "user:alice", "preferred_name")
            .await
            .unwrap();
        assert_eq!(again.memories_deleted, 0, "idempotent");
        assert!(engine.forget_subject_triple("a1", "", "p").await.is_err());
    }

    #[tokio::test]
    async fn forget_subject_triple_removes_held_claims_of_the_triple() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let entry = |id: &str, content: &str| MemoryEntry {
            id: id.to_string(),
            agent_id: "a".into(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "user_profile".into(),
        };
        let meta = |obj: &str, origin: &str| TemporalMeta {
            subject: Some("user:alice".into()),
            predicate: Some("prefers".into()),
            object: Some(obj.to_string()),
            origin: Some(origin.to_string()),
            ..TemporalMeta::default()
        };
        engine
            .store_temporal("a", entry("op", "prefers: tea"), meta("tea", "operator"),
                crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        let held = engine
            .hold_refused_claim("a", entry("held", "prefers: coffee"), meta("coffee", "channel"),
                crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        let report = engine.forget_subject_triple("a", "user:alice", "prefers").await.unwrap();
        assert_eq!(report.memories_deleted, 2);
        assert!(engine.get_by_id("a", &held).await.unwrap().is_none(), "held claim gone");
        let p = engine.promote_quarantined("a", &[held], "operator").await.unwrap();
        assert_eq!((p.promoted, p.stale), (0, 0), "nothing left to approve");
    }

    #[tokio::test]
    async fn subject_listing_uses_the_subject_index() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let conn = engine.conn_for_maintenance().await;
        let plan: Vec<String> = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT id FROM memories AS m
                 WHERE m.subject IN ('user:a', 'user:b') AND m.quarantined = 0
                 ORDER BY m.timestamp DESC, m.id DESC LIMIT 10",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            plan.iter().any(|d| d.contains("idx_memories_subject_ts")),
            "{plan:?}"
        );
    }
}
