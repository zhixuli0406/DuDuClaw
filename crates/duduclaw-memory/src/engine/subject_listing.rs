//! RM-039: rows *about a person*, across every namespace in one database.
//!
//! A person's facts are keyed by `subject` (`user:<id>`, see
//! [`crate::user_profile::user_subject`]) but stored in the namespace of the
//! AI employee that learned them. The gateway's "my memory" RPCs resolve the
//! caller's own subjects and use these two reads to list and look up exactly
//! those rows — the subject filter is the authorization boundary, so it is
//! applied in SQL and never widened here.

use super::*;

/// Upper bound on the subjects one query may name.
pub const MAX_SUBJECTS: usize = 64;

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
}
