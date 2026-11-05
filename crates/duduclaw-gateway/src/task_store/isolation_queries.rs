//! Native-isolation collections consume finite signed task ids in the query.
//! Current assignment is part of the same SELECT, never an earlier snapshot.
use super::*;

pub(super) fn validate_scope(agent: &str, ids: &[String]) -> Result<(), String> {
    if !duduclaw_core::is_valid_agent_id(agent)
        || duduclaw_core::is_reserved_agent_id(agent)
        || agent == duduclaw_core::UNTRUSTED_AGENT_ID
        || ids.len() > 128
        || ids.iter().any(|id| id.is_empty() || id.len() > 128 || id == "*" || id.chars().any(char::is_control))
        || ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err("invalid isolated collection scope".into());
    }
    Ok(())
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count).collect::<Vec<_>>().join(",")
}

impl TaskStore {
    /// Narrow isolated collection reads to currently assigned, signed tasks.
    /// This intentionally does not expand to public tasks, creators or claimers.
    pub async fn list_tasks_isolated(&self, agent: &str, ids: &[String],
        status: Option<&str>, priority: Option<&str>) -> Result<Vec<TaskRow>, String> {
        validate_scope(agent, ids)?;
        if ids.is_empty() { return Ok(Vec::new()); }
        let conn = self.conn.lock().await;
        verify_live_io()?;
        let mut sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE assigned_to = ? AND archived = 0 AND id IN ({})", placeholders(ids.len()));
        let mut binds = vec![agent.to_owned()];
        binds.extend_from_slice(ids);
        if let Some(status) = status { sql.push_str(" AND status = ?"); binds.push(status.to_owned()); }
        if let Some(priority) = priority { sql.push_str(" AND priority = ?"); binds.push(priority.to_owned()); }
        sql.push_str(" ORDER BY pinned DESC, updated_at DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare isolated tasks: {e}"))?;
        stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_to_task)
            .map_err(|e| format!("query isolated tasks: {e}"))?
            .collect::<Result<Vec<_>, _>>().map_err(|e| format!("read isolated tasks: {e}"))
    }

    /// Count and page use one read transaction and the identical scoped clause.
    /// Null-task/global activity is intentionally absent from an isolated run.
    pub async fn list_activity_isolated(&self, agent: &str, ids: &[String],
        event_type: Option<&str>, task_id: Option<&str>, limit: i64)
        -> Result<(Vec<ActivityRow>, i64), String> {
        validate_scope(agent, ids)?;
        if ids.is_empty() { return Ok((Vec::new(), 0)); }
        let mut conn = self.conn.lock().await;
        verify_live_io()?;
        let tx = conn.transaction().map_err(|e| format!("begin isolated activity: {e}"))?;
        let mut clause = format!(" FROM activity a JOIN tasks t ON t.id = a.task_id WHERE a.agent_id = ? AND t.assigned_to = ? AND t.id IN ({})", placeholders(ids.len()));
        let mut binds = vec![agent.to_owned(), agent.to_owned()];
        binds.extend_from_slice(ids);
        if let Some(kind) = event_type { clause.push_str(" AND a.event_type = ?"); binds.push(kind.to_owned()); }
        if let Some(id) = task_id { clause.push_str(" AND a.task_id = ?"); binds.push(id.to_owned()); }
        let total = tx.query_row(&format!("SELECT COUNT(*){clause}"),
            rusqlite::params_from_iter(binds.iter()), |row| row.get(0))
            .map_err(|e| format!("count isolated activity: {e}"))?;
        let sql = format!("SELECT a.id,a.event_type,a.agent_id,a.task_id,a.summary,a.timestamp,a.metadata{clause} ORDER BY a.timestamp DESC LIMIT {}", limit.clamp(1, 100));
        let rows = {
            let mut stmt = tx.prepare(&sql).map_err(|e| format!("prepare isolated activity: {e}"))?;
            stmt.query_map(rusqlite::params_from_iter(binds.iter()), |row| Ok(ActivityRow {
                id: row.get(0)?, event_type: row.get(1)?, agent_id: row.get(2)?,
                task_id: row.get(3)?, summary: row.get(4)?, timestamp: row.get(5)?, metadata: row.get(6)?,
            })).map_err(|e| format!("query isolated activity: {e}"))?
                .collect::<Result<Vec<_>, _>>().map_err(|e| format!("read isolated activity: {e}"))?
        };
        commit_live_write(tx)?;
        Ok((rows, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, agent: &str) -> TaskRow {
        TaskRow::new(id.into(), id.into(), String::new(), "medium".into(), agent.into(), "operator".into())
    }
    fn activity(id: &str, task: Option<&str>, agent: &str) -> ActivityRow {
        ActivityRow { id: id.into(), task_id: task.map(str::to_owned), agent_id: agent.into(),
            event_type: "test".into(), summary: id.into(), timestamp: "2026-11-05T00:00:00Z".into(), metadata: None }
    }

    #[tokio::test]
    async fn isolated_collection_uses_signed_ids_and_live_assignment() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        for (id, agent) in [("allowed", "alice"), ("foreign", "bob")] {
            store.insert_task(&task(id, agent)).await.unwrap();
        }
        let grants = vec!["allowed".into(), "foreign".into()];
        // A row inserted after capability issuance is still outside its scope.
        store.insert_task(&task("new", "alice")).await.unwrap();
        let rows = store.list_tasks_isolated("alice", &grants, None, None).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["allowed"]);
        store.update_task("allowed", &serde_json::json!({"assigned_to":"bob"})).await.unwrap();
        assert!(store.list_tasks_isolated("alice", &grants, None, None).await.unwrap().is_empty());
        assert!(store.list_tasks_isolated("alice", &[], None, None).await.unwrap().is_empty());
        assert!(store.list_tasks_isolated("alice", &["*".into()], None, None).await.is_err());
        assert!(store.list_tasks_isolated("alice", &["allowed".into(), "allowed".into()], None, None).await.is_err());
    }

    #[tokio::test]
    async fn isolated_activity_count_and_page_share_scope() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        for (id, agent) in [("a", "alice"), ("b", "bob"), ("ungranted", "alice")] {
            store.insert_task(&task(id, agent)).await.unwrap();
        }
        for row in [activity("one", Some("a"), "alice"), activity("two", Some("a"), "alice"),
            activity("foreign", Some("b"), "alice"), activity("other-author", Some("a"), "bob"),
            activity("ungranted", Some("ungranted"), "alice"), activity("global", None, "alice")] {
            store.append_activity(&row).await.unwrap();
        }
        let grants = vec!["a".into(), "b".into()];
        let (rows, total) = store.list_activity_isolated("alice", &grants, None, None, 1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(total, 2);
        assert_eq!(rows[0].task_id.as_deref(), Some("a"));
        let (rows, total) = store.list_activity_isolated("alice", &grants, None, Some("b"), 100).await.unwrap();
        assert!(rows.is_empty());
        assert_eq!(total, 0);
        store.update_task("a", &serde_json::json!({"assigned_to":"bob"})).await.unwrap();
        let (rows, total) = store.list_activity_isolated("alice", &grants, None, None, 100).await.unwrap();
        assert!(rows.is_empty());
        assert_eq!(total, 0);
        assert_eq!(store.list_activity_isolated("alice", &[], None, None, 100).await.unwrap().1, 0);
    }
}
