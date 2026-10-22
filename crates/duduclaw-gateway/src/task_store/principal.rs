//! Tasks a person handed over (`on_behalf_of`): the `/交辦 狀態` list and the
//! dashboard's "assigned by me" view. The principal is always written and
//! matched by the gateway (`user:<account id>`), never taken from input.

use super::*;

/// Statuses that still need something to happen (the "not finished" set the
/// chat status lists use).
pub const ACTIVE_STATUSES: &[&str] = &[
    "todo",
    "pending",
    "in_progress",
    "review",
    "revising",
    "needs_human",
    "blocked",
];

impl TaskStore {
    /// The home directory this store lives in (`<home>/tasks.db`).
    pub fn home_dir(&self) -> Option<&Path> {
        self.db_path.parent()
    }

    /// Tasks handed over for `principal`, most recently updated first.
    /// `active_only` keeps only [`ACTIVE_STATUSES`]; archived rows are never
    /// listed. `limit` is clamped to `[1, 200]`.
    pub async fn list_by_principal(
        &self,
        principal: &str,
        active_only: bool,
        limit: i64,
    ) -> Result<Vec<TaskRow>, String> {
        let status_clause = if active_only {
            let list = ACTIVE_STATUSES
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(",");
            format!(" AND status IN ({list})")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE on_behalf_of = ?1 AND archived = 0{status_clause} \
             ORDER BY updated_at DESC LIMIT {}",
            limit.clamp(1, 200)
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare principal list: {e}"))?;
        let rows = stmt
            .query_map(params![principal], row_to_task)
            .map_err(|e| format!("query principal list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect principal list: {e}"))?;
        Ok(rows)
    }

    /// One page of the tasks handed over for `principal`, with the total.
    /// Same filters and order as [`Self::list_tasks_paginated`] (pinned
    /// first, then most recently updated; archived only when asked for).
    #[allow(clippy::too_many_arguments)]
    pub async fn list_principal_page(
        &self,
        principal: &str,
        status: Option<&str>,
        agent_id: Option<&str>,
        goal_mode: Option<bool>,
        archived: Option<bool>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<TaskRow>, i64), String> {
        let mut binds: Vec<String> = vec![principal.to_string()];
        let mut filter = String::from(" WHERE on_behalf_of = ?1");
        if let Some(s) = status {
            binds.push(s.to_string());
            filter.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            filter.push_str(&format!(" AND assigned_to = ?{}", binds.len()));
        }
        match goal_mode {
            Some(true) => filter.push_str(" AND goal_mode = 1"),
            Some(false) => filter.push_str(" AND goal_mode = 0"),
            None => {}
        }
        filter.push_str(if archived == Some(true) {
            " AND archived = 1"
        } else {
            " AND archived = 0"
        });
        let conn = self.conn.lock().await;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM tasks{filter}"),
                params_ref.as_slice(),
                |r| r.get(0),
            )
            .map_err(|e| format!("count principal page: {e}"))?;
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks{filter} ORDER BY pinned DESC, updated_at DESC \
             LIMIT {} OFFSET {}",
            limit.clamp(1, 200),
            offset.max(0)
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare principal page: {e}"))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_task)
            .map_err(|e| format!("query principal page: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect principal page: {e}"))?;
        Ok((rows, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, principal: Option<&str>, status: &str) -> TaskRow {
        let mut r = TaskRow::new(
            id.into(),
            format!("title {id}"),
            String::new(),
            "medium".into(),
            "sales".into(),
            "assign:line".into(),
        );
        r.status = status.into();
        r.goal_mode = true;
        r.on_behalf_of = principal.map(str::to_string);
        r.created_via = principal.map(|_| "line".to_string());
        r
    }

    #[tokio::test]
    async fn principal_columns_round_trip_and_old_rows_read_null() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        store.insert_task(&row("a", Some("user:u1"), "todo")).await.unwrap();
        store.insert_task(&row("b", None, "todo")).await.unwrap();
        let a = store.get_task("a").await.unwrap().unwrap();
        assert_eq!(a.on_behalf_of.as_deref(), Some("user:u1"));
        assert_eq!(a.created_via.as_deref(), Some("line"));
        let b = store.get_task("b").await.unwrap().unwrap();
        assert!(b.on_behalf_of.is_none() && b.created_via.is_none());
        assert_eq!(store.home_dir(), Some(home.path()));
    }

    #[tokio::test]
    async fn an_old_database_gains_the_columns_without_losing_rows() {
        let home = tempfile::tempdir().unwrap();
        {
            let store = TaskStore::open(home.path()).unwrap();
            store.insert_task(&row("old", None, "done")).await.unwrap();
        }
        // Simulate a database from before this column existed.
        {
            let conn = Connection::open(home.path().join("tasks.db")).unwrap();
            conn.execute_batch(
                "DROP INDEX IF EXISTS idx_tasks_on_behalf_of; \
                 ALTER TABLE tasks DROP COLUMN on_behalf_of; \
                 ALTER TABLE tasks DROP COLUMN created_via;",
            )
            .unwrap();
        }
        let store = TaskStore::open(home.path()).unwrap();
        let old = store.get_task("old").await.unwrap().unwrap();
        assert!(old.on_behalf_of.is_none());
        assert!(store.list_by_principal("user:u1", false, 20).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn listing_by_principal_matches_exactly_and_filters_active() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        store.insert_task(&row("a", Some("user:u1"), "todo")).await.unwrap();
        store.insert_task(&row("b", Some("user:u1"), "done")).await.unwrap();
        store.insert_task(&row("c", Some("user:u10"), "todo")).await.unwrap();
        store.insert_task(&row("d", None, "todo")).await.unwrap();
        let active = store.list_by_principal("user:u1", true, 20).await.unwrap();
        assert_eq!(active.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a"]);
        let all = store.list_by_principal("user:u1", false, 20).await.unwrap();
        assert_eq!(all.len(), 2);
        let (page, total) = store
            .list_principal_page("user:u1", Some("done"), None, Some(true), None, 50, 0)
            .await
            .unwrap();
        assert_eq!((page.len(), total), (1, 1));
        assert_eq!(page[0].id, "b");
    }
}
