//! The activity feed and task comments.
//! Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
    pub async fn append_activity(&self, row: &ActivityRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO activity (id, event_type, agent_id, task_id, summary, timestamp, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.id,
                row.event_type,
                row.agent_id,
                row.task_id,
                row.summary,
                row.timestamp,
                row.metadata,
            ],
        )
        .map_err(|e| format!("append activity: {e}"))?;
        Ok(())
    }

    pub async fn list_activity(
        &self,
        agent_id: Option<&str>,
        event_type: Option<&str>,
        limit: i64,
        offset: i64,
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
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO task_comments (id, task_id, author_user, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                row.id,
                row.task_id,
                row.author_user,
                row.body,
                row.created_at
            ],
        )
        .map_err(|e| format!("insert comment: {e}"))?;
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
