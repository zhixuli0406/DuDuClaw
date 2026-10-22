//! Open-task counts per AI employee, for the dashboard 我的團隊 page
//! (`users.my_team`). Read-only.

use std::collections::HashMap;

use super::*;

/// Most employees counted in one call (a team page never shows more).
const MAX_COUNTED_AGENTS: usize = 512;

impl TaskStore {
    /// Tasks and goals assigned to each of `agents` that are still open:
    /// not `done` / `cancelled` / `failed` and not archived (the same
    /// "open" the stop-tree and board listings use). Agents with no open
    /// work are absent from the map. At most [`MAX_COUNTED_AGENTS`] names
    /// are counted; the rest are ignored.
    pub async fn open_task_counts(
        &self,
        agents: &[String],
    ) -> Result<HashMap<String, i64>, String> {
        let names: Vec<&String> = agents.iter().take(MAX_COUNTED_AGENTS).collect();
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        let names_json = serde_json::to_string(&names).map_err(|e| e.to_string())?;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT assigned_to, COUNT(*) FROM tasks
                  WHERE assigned_to IN (SELECT value FROM json_each(?1))
                    AND kind IN ('task','goal') AND archived = 0
                    AND status NOT IN ('done','cancelled','failed')
                  GROUP BY assigned_to",
            )
            .map_err(|e| format!("prepare open task counts: {e}"))?;
        let rows = stmt
            .query_map(params![names_json], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("query open task counts: {e}"))?;
        let mut counts = HashMap::new();
        for row in rows {
            let (agent, n) = row.map_err(|e| format!("open task count row: {e}"))?;
            counts.insert(agent, n);
        }
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, agent: &str, status: &str) -> TaskRow {
        let mut r = TaskRow::new(
            id.to_string(),
            format!("t-{id}"),
            String::new(),
            "medium".to_string(),
            agent.to_string(),
            "u1".to_string(),
        );
        r.status = status.to_string();
        r
    }

    #[tokio::test]
    async fn counts_only_open_unarchived_work_per_agent() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        for (id, agent, status) in [
            ("1", "sales", "todo"),
            ("2", "sales", "in_progress"),
            ("3", "sales", "done"),
            ("4", "sales", "cancelled"),
            ("5", "hr", "blocked"),
            ("6", "finance", "todo"),
        ] {
            store.insert_task(&row(id, agent, status)).await.unwrap();
        }
        let archived = row("7", "hr", "todo");
        store.insert_task(&archived).await.unwrap();
        store
            .update_task("7", &serde_json::json!({ "archived": true }))
            .await
            .unwrap();

        let counts = store
            .open_task_counts(&["sales".to_string(), "hr".to_string(), "nobody".to_string()])
            .await
            .unwrap();
        assert_eq!(counts.get("sales"), Some(&2));
        assert_eq!(
            counts.get("hr"),
            Some(&1),
            "archived and closed work is not counted"
        );
        assert_eq!(counts.get("nobody"), None);
        assert_eq!(counts.get("finance"), None, "only the agents asked about");
        assert!(store.open_task_counts(&[]).await.unwrap().is_empty());
    }
}
