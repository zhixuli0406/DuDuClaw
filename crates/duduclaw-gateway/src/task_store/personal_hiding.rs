//! Listings and totals without another person's Personal employees (native
//! Linux isolation v1.81, task P1c).
//!
//! The dashboard drops hidden employees' rows (`TaskReader::hides`); a page
//! total computed before that drop still counted them, and dropping after
//! LIMIT shortened pages. These queries exclude the hidden set in SQL, before
//! LIMIT and COUNT, so the total equals what the caller can list. An empty
//! set (unprotected installs, the owner, an admitted scope) adds no clause
//! and reads exactly as before.
use std::collections::BTreeSet;

use super::super::*;

/// The hidden set as one JSON array bind for `json_each`, or `None` when
/// nothing is hidden.
pub(crate) fn hidden_json(hidden: &BTreeSet<String>) -> Option<String> {
    if hidden.is_empty() {
        return None;
    }
    serde_json::to_string(&hidden.iter().collect::<Vec<_>>()).ok()
}

impl TaskStore {
    /// One dashboard page of tasks with its total: the owner restriction
    /// (`principal`) and the hidden employees are applied before LIMIT and
    /// COUNT. Filters and order as `list_tasks_paginated`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn list_tasks_page_hiding(
        &self, status: Option<&str>, agent_id: Option<&str>, priority: Option<&str>,
        goal_mode: Option<bool>, archived: Option<bool>, limit: i64, offset: i64,
        principal: Option<&str>, hidden: &BTreeSet<String>,
    ) -> Result<(Vec<TaskRow>, i64), String> {
        let conn = self.conn.lock().await;
        let mut filter = String::from(" WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = status {
            binds.push(s.to_string());
            filter.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            filter.push_str(&format!(" AND assigned_to = ?{}", binds.len()));
        }
        if let Some(p) = priority {
            binds.push(p.to_string());
            filter.push_str(&format!(" AND priority = ?{}", binds.len()));
        }
        match goal_mode {
            Some(true) => filter.push_str(" AND goal_mode = 1"),
            Some(false) => filter.push_str(" AND goal_mode = 0"),
            None => {}
        }
        filter.push_str(if archived == Some(true) { " AND archived = 1" } else { " AND archived = 0" });
        if let Some(principal) = principal {
            binds.push(principal.to_string());
            filter.push_str(&format!(" AND (a2a_owner IS NULL OR a2a_owner = ?{})", binds.len()));
        }
        if let Some(hidden) = hidden_json(hidden) {
            binds.push(hidden);
            filter.push_str(&format!(" AND assigned_to NOT IN (SELECT value FROM json_each(?{}))", binds.len()));
        }
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let total: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM tasks{filter}"), params_ref.as_slice(), |r| r.get(0))
            .map_err(|e| format!("count tasks page: {e}"))?;
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks{filter} ORDER BY pinned DESC, updated_at DESC LIMIT {} OFFSET {}",
            limit.clamp(1, 200),
            offset.max(0)
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare list page: {e}"))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_task)
            .map_err(|e| format!("query list page: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list page: {e}"))?;
        Ok((rows, total))
    }
}
