//! The activity feed and task comments.
//! Moved verbatim out of `task_store.rs`.

use super::*;

/// v1.81 P1c: page queries that exclude hidden Personal employees.
#[path = "personal_hiding.rs"]
mod personal_hiding;
use personal_hiding::hidden_json;

enum ActivityAccess<'a> {
    All,
    Principal(&'a str),
    Worker(Option<&'a str>),
}

impl TaskStore {
    pub async fn append_activity(&self, row: &ActivityRow) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = begin_live_write(&mut conn)?;
        tx.execute(
            "INSERT INTO activity (id, event_type, agent_id, task_id, summary, timestamp, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.id,
                row.event_type,
                row.agent_id,
                row.task_id,
                mask_derived_text(&row.summary),
                row.timestamp,
                row.metadata.as_deref().map(mask_derived_text),
            ],
        )
        .map_err(|e| format!("append activity: {e}"))?;
        commit_live_write(tx)?;
        Ok(())
    }

    pub async fn list_activity(
        &self,
        agent_id: Option<&str>,
        event_type: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<ActivityRow>, i64), String> {
        self.list_activity_for_principal(agent_id, event_type, limit, offset, None).await
    }

    pub(crate) async fn list_activity_for_principal(
        &self, agent_id: Option<&str>, event_type: Option<&str>, limit: i64,
        offset: i64, principal: Option<&str>,
    ) -> Result<(Vec<ActivityRow>, i64), String> {
        let access = principal.map(ActivityAccess::Principal).unwrap_or(ActivityAccess::All);
        self.list_activity_with_access(agent_id, event_type, limit, offset, access, None).await
    }

    /// v1.81 P1c: [`Self::list_activity_for_principal`] without the rows of
    /// `hidden` employees — by the row's employee and by the employee of the
    /// task it names — applied before LIMIT and COUNT, so `total` counts only
    /// what the caller can list. An empty set reads exactly as before.
    pub(crate) async fn list_activity_hiding(
        &self, agent_id: Option<&str>, event_type: Option<&str>, limit: i64,
        offset: i64, principal: &str, hidden: &std::collections::BTreeSet<String>,
    ) -> Result<(Vec<ActivityRow>, i64), String> {
        self.list_activity_with_access(agent_id, event_type, limit, offset,
            ActivityAccess::Principal(principal), hidden_json(hidden)).await
    }

    /// `worker` must be the caller's verified executor identity. No worker
    /// means an external MCP caller; removed A2A tasks are never admitted.
    pub async fn list_activity_for_worker(
        &self, agent_id: Option<&str>, event_type: Option<&str>, limit: i64,
        offset: i64, worker: Option<&str>,
    ) -> Result<(Vec<ActivityRow>, i64), String> {
        self.list_activity_with_access(agent_id, event_type, limit, offset,
            ActivityAccess::Worker(worker), None).await
    }

    async fn list_activity_with_access(
        &self, agent_id: Option<&str>, event_type: Option<&str>, limit: i64,
        offset: i64, access: ActivityAccess<'_>, hidden: Option<String>,
    ) -> Result<(Vec<ActivityRow>, i64), String> {
        let conn = self.conn.lock().await;

        // Count total
        let mut count_sql = "SELECT COUNT(*) FROM activity WHERE 1=1".to_string();
        let mut query_sql = "SELECT id, event_type, agent_id, task_id, summary, timestamp, metadata
                             FROM activity WHERE 1=1"
            .to_string();
        let mut binds: Vec<String> = Vec::new();
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            let clause = format!(" AND agent_id = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(t) = event_type {
            binds.push(t.to_string());
            let clause = format!(" AND event_type = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        let access_clause = match access {
            ActivityAccess::All => String::new(),
            ActivityAccess::Principal(principal) => {
                binds.push(principal.to_string());
                format!(" AND NOT EXISTS (SELECT 1 FROM task_access_principals p WHERE p.task_id = activity.task_id AND p.principal <> ?{})", binds.len())
            }
            ActivityAccess::Worker(None) => {
                " AND NOT EXISTS (SELECT 1 FROM task_access_principals p WHERE p.task_id = activity.task_id)".into()
            }
            ActivityAccess::Worker(Some(worker)) => {
                binds.push(worker.to_string());
                format!(" AND NOT EXISTS (SELECT 1 FROM task_access_principals p WHERE p.task_id = activity.task_id AND NOT EXISTS (SELECT 1 FROM tasks t WHERE t.id = p.task_id AND t.a2a_owner = p.principal AND t.assigned_to = ?{}))", binds.len())
            }
        };
        count_sql.push_str(&access_clause);
        query_sql.push_str(&access_clause);
        if let Some(hidden) = hidden {
            binds.push(hidden);
            let n = binds.len();
            let clause = format!(
                " AND activity.agent_id NOT IN (SELECT value FROM json_each(?{n})) AND NOT EXISTS \
                 (SELECT 1 FROM tasks h WHERE h.id = activity.task_id AND h.assigned_to IN (SELECT value FROM json_each(?{n})))"
            );
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        query_sql.push_str(&format!(
            " ORDER BY timestamp DESC LIMIT {} OFFSET {}",
            limit, offset
        ));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();

        let total: i64 = conn
            .query_row(&count_sql, params_ref.as_slice(), |r| r.get(0))
            .map_err(|e| format!("count activity: {e}"))?;

        let mut stmt = conn
            .prepare(&query_sql)
            .map_err(|e| format!("prepare activity: {e}"))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), |r| {
                Ok(ActivityRow {
                    id: r.get(0)?,
                    event_type: r.get(1)?,
                    agent_id: r.get(2)?,
                    task_id: r.get(3)?,
                    summary: r.get(4)?,
                    timestamp: r.get(5)?,
                    metadata: r.get(6)?,
                })
            })
            .map_err(|e| format!("query activity: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect activity: {e}"))?;

        Ok((rows, total))
    }

    /// Every activity row for one task, oldest first (chronological for the
    /// goal-loop timeline). Task-scoped where [`Self::list_activity`] is
    /// global — a long-running goal's kickoff/oscillation/needs_human events
    /// would be washed out of any bounded global window.
    pub async fn list_activity_for_task(
        &self,
        task_id: &str,
        limit: i64,
    ) -> Result<Vec<ActivityRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, event_type, agent_id, task_id, summary, timestamp, metadata
                 FROM activity WHERE task_id = ?1 ORDER BY timestamp ASC LIMIT ?2",
            )
            .map_err(|e| format!("prepare task activity: {e}"))?;
        let rows = stmt
            .query_map(params![task_id, limit.clamp(1, 1000)], |r| {
                Ok(ActivityRow {
                    id: r.get(0)?,
                    event_type: r.get(1)?,
                    agent_id: r.get(2)?,
                    task_id: r.get(3)?,
                    summary: r.get(4)?,
                    timestamp: r.get(5)?,
                    metadata: r.get(6)?,
                })
            })
            .map_err(|e| format!("query task activity: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect task activity: {e}"))?;
        Ok(rows)
    }

    /// H22: RFC3339 timestamp of the most recent Activity Feed event for a
    /// task, or `None` when the task has none.
    ///
    /// This is the goal loop's **progress signal** for the timeout notice.
    /// The obvious alternative, `tasks.updated_at`, is unusable: the dispatch
    /// engine's lease renewer calls [`Self::renew_lease`], which bumps
    /// `updated_at` on a timer for every `in_progress` task — so a silent
    /// agent's row looks freshly updated forever. The activity feed only
    /// moves when something actually happened (the driver dispatched a round,
    /// the engine judged one, or the agent itself posted via the
    /// `activity_post` MCP tool), which is exactly the definition of
    /// "reported progress".
    ///
    /// Served by `idx_activity_ts` (`timestamp DESC`); one row, one task.
    pub async fn latest_activity_at(&self, task_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT timestamp FROM activity WHERE task_id = ?1 ORDER BY timestamp DESC LIMIT 1",
            params![task_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("latest activity: {e}"))
    }

    // ── Task comments (L2) ──────────────────────────────────

    /// Append a comment. Caller is responsible for verifying the task exists and
    /// that `body` is non-empty and length-capped.
    pub async fn insert_comment(&self, row: &CommentRow) -> Result<(), String> {
        let mut connection = self.conn.lock().await;
        let conn = begin_live_write(&mut connection)?;
        conn.execute(
            "INSERT INTO task_comments (id, task_id, author_user, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                row.id,
                row.task_id,
                row.author_user,
                mask_derived_text(&row.body),
                row.created_at
            ],
        )
        .map_err(|e| format!("insert comment: {e}"))?;
        commit_live_write(conn)?;
        Ok(())
    }

    /// All comments for a task, oldest first (chronological for the timeline).
    pub async fn list_comments(&self, task_id: &str) -> Result<Vec<CommentRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, author_user, body, created_at
                 FROM task_comments WHERE task_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare comments: {e}"))?;
        let rows = stmt
            .query_map(params![task_id], |r| {
                Ok(CommentRow {
                    id: r.get(0)?,
                    task_id: r.get(1)?,
                    author_user: r.get(2)?,
                    body: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })
            .map_err(|e| format!("query comments: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect comments: {e}"))?;
        Ok(rows)
    }

    // ── U4 co-edited plans ──────────────────────────────────
}

#[cfg(test)]
mod a2a_activity_worker_tests {
    use super::*;

    #[tokio::test]
    async fn worker_filter_precedes_limit_and_count_and_honors_tombstones() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        let mut private = TaskRow::new("private".into(), "Private".into(), String::new(),
            "medium".into(), "sales".into(), "a2a:user:alice".into());
        private.a2a_owner = Some("user:alice".into());
        store.insert_task(&private).await.unwrap();
        for (id, task, timestamp) in [
            ("older", None, "2026-11-05T00:00:00Z"),
            ("newer", None, "2026-11-05T00:01:00Z"),
            ("secret", Some("private"), "2026-11-05T00:02:00Z"),
        ] {
            store.append_activity(&ActivityRow {
                id: id.into(), event_type: "task_completed".into(), agent_id: "sales".into(),
                task_id: task.map(str::to_string), summary: id.into(), timestamp: timestamp.into(), metadata: None,
            }).await.unwrap();
        }
        for worker in [None, Some("other")] {
            let (page, total) = store.list_activity_for_worker(None, None, 1, 0, worker).await.unwrap();
            assert_eq!(total, 2);
            assert_eq!(page[0].id, "newer");
            let (page, total) = store.list_activity_for_worker(None, None, 1, 1, worker).await.unwrap();
            assert_eq!(total, 2);
            assert_eq!(page[0].id, "older");
        }
        let (page, total) = store.list_activity_for_worker(None, None, 1, 0, Some("sales")).await.unwrap();
        assert_eq!(total, 3);
        assert_eq!(page[0].id, "secret");
        store.remove_task("private").await.unwrap();
        let (page, total) = store.list_activity_for_worker(None, None, 1, 0, Some("sales")).await.unwrap();
        assert_eq!(total, 2);
        assert_eq!(page[0].id, "newer");
    }

    #[tokio::test]
    async fn ordinary_activity_total_remains_full_count_beyond_page() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        for id in ["one", "two", "three"] {
            store.append_activity(&ActivityRow {
                id: id.into(), event_type: "agent_comment".into(), agent_id: "sales".into(),
                task_id: None, summary: id.into(), timestamp: "2026-11-05T00:00:00Z".into(), metadata: None,
            }).await.unwrap();
        }
        let (rows, total) = store.list_activity_for_worker(Some("sales"), Some("agent_comment"), 1, 0, None)
            .await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(total, 3);
        assert_eq!(store.list_activity(Some("sales"), Some("agent_comment"), 1, 0).await.unwrap().1, total);
    }
}
