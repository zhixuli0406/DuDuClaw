//! Goal rows, their parent tree and the dependency edges, plus the
//! connection-level read twins. Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
    /// Insert a goal. Fail-closed validation at the single write boundary:
    /// a non-null `parent_goal_id` must reference an existing goal and must not
    /// close a cycle in the parent graph (visited-set walk). Check + write run
    /// in one IMMEDIATE transaction so they cannot be raced apart (TOCTOU).
    pub async fn insert_goal(&self, row: &GoalRow) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("insert goal: begin: {e}"))?;
        if let Some(parent) = row.parent_goal_id.as_deref() {
            if get_goal_conn(&tx, parent)?.is_none() {
                return Err(format!("parent goal not found: {parent}"));
            }
            let edges = goal_parent_edges_conn(&tx)?;
            if introduces_parent_cycle(&edges, &row.id, parent) {
                return Err(format!(
                    "goal cycle rejected: {} → {} would close a loop",
                    row.id, parent
                ));
            }
        }
        tx.execute(
            "INSERT INTO goals (id, title, description, parent_goal_id, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.id,
                row.title,
                row.description,
                row.parent_goal_id,
                row.status,
                row.created_at,
            ],
        )
        .map_err(|e| format!("insert goal: {e}"))?;
        tx.commit()
            .map_err(|e| format!("insert goal: commit: {e}"))?;
        Ok(())
    }

    pub async fn get_goal(&self, id: &str) -> Result<Option<GoalRow>, String> {
        let conn = self.conn.lock().await;
        get_goal_conn(&conn, id)
    }

    pub async fn list_goals(&self, status: Option<&str>) -> Result<Vec<GoalRow>, String> {
        let conn = self.conn.lock().await;
        let (sql, binds): (String, Vec<String>) = match status {
            Some(s) => (
                "SELECT id, title, description, parent_goal_id, status, created_at
                   FROM goals WHERE status = ?1 ORDER BY created_at ASC"
                    .into(),
                vec![s.to_string()],
            ),
            None => (
                "SELECT id, title, description, parent_goal_id, status, created_at
                   FROM goals ORDER BY created_at ASC"
                    .into(),
                Vec::new(),
            ),
        };
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare goals: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_goal)
            .map_err(|e| format!("query goals: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect goals: {e}"))?;
        Ok(rows)
    }

    /// Update mutable goal fields. Re-parenting goes through the same
    /// fail-closed cycle gate as `insert_goal`, inside one IMMEDIATE
    /// transaction (check + write cannot be raced apart — TOCTOU).
    pub async fn update_goal(
        &self,
        id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<GoalRow>, String> {
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("update goal: begin: {e}"))?;
            if let Some(new_parent) = fields.get("parent_goal_id").and_then(|v| v.as_str()) {
                if get_goal_conn(&tx, new_parent)?.is_none() {
                    return Err(format!("parent goal not found: {new_parent}"));
                }
                let edges = goal_parent_edges_conn(&tx)?;
                if introduces_parent_cycle(&edges, id, new_parent) {
                    return Err(format!(
                        "goal cycle rejected: {id} → {new_parent} would close a loop"
                    ));
                }
            }
            let mut sets: Vec<String> = Vec::new();
            let mut binds: Vec<String> = Vec::new();
            for key in ["title", "description", "status", "parent_goal_id"] {
                if let Some(v) = fields.get(key).and_then(|v| v.as_str()) {
                    binds.push(v.to_string());
                    sets.push(format!("{key} = ?{}", binds.len()));
                }
            }
            if sets.is_empty() {
                return Err("no goal fields to update".into());
            }
            binds.push(id.to_string());
            let sql = format!(
                "UPDATE goals SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );
            let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            tx.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update goal: {e}"))?;
            tx.commit()
                .map_err(|e| format!("update goal: commit: {e}"))?;
        }
        self.get_goal(id).await
    }

    /// All `(goal_id, parent_goal_id)` edges — for cycle detection.
    pub async fn goal_parent_edges(&self) -> Result<Vec<(String, Option<String>)>, String> {
        let conn = self.conn.lock().await;
        goal_parent_edges_conn(&conn)
    }

    /// Walk a goal's ancestry root-first (Initiative → Project → Issue).
    /// Visited-set + depth cap make the walk loop-proof even on corrupted data
    /// (the chain is truncated, never spun). Unknown id ⇒ empty vec.
    pub async fn goal_ancestry(&self, goal_id: &str) -> Result<Vec<GoalRow>, String> {
        let mut chain: Vec<GoalRow> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cur = Some(goal_id.to_string());
        while let Some(id) = cur {
            if chain.len() >= GOAL_ANCESTRY_MAX_DEPTH || !seen.insert(id.clone()) {
                break; // depth cap / loop guard — fail-safe truncation
            }
            let Some(goal) = self.get_goal(&id).await? else {
                break;
            };
            cur = goal.parent_goal_id.clone();
            chain.push(goal);
        }
        chain.reverse(); // walked leaf→root; present root-first
        Ok(chain)
    }

    // ── Dependency graph (depends_on) ───────────────────────

    /// All `(task_id, depends_on ids)` edges — for dependency cycle detection.
    pub async fn depends_edges(&self) -> Result<Vec<(String, Vec<String>)>, String> {
        let conn = self.conn.lock().await;
        depends_edges_conn(&conn)
    }

    // ── Activity feed ───────────────────────────────────────
}

// ── Connection-level read helpers ───────────────────────────
//
// Sync twins of the async read methods, usable both under the store's Mutex
// lock and inside a `Transaction` (which derefs to `Connection`) — the TOCTOU
// fixes run their cycle/existence checks through these inside the same
// IMMEDIATE transaction as the write.

fn get_goal_conn(conn: &Connection, id: &str) -> Result<Option<GoalRow>, String> {
    conn.query_row(
        "SELECT id, title, description, parent_goal_id, status, created_at
           FROM goals WHERE id = ?1",
        params![id],
        row_to_goal,
    )
    .optional()
    .map_err(|e| format!("get goal: {e}"))
}

fn goal_parent_edges_conn(conn: &Connection) -> Result<Vec<(String, Option<String>)>, String> {
    let mut stmt = conn
        .prepare("SELECT id, parent_goal_id FROM goals")
        .map_err(|e| format!("prepare goal edges: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map_err(|e| format!("query goal edges: {e}"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("collect goal edges: {e}"))?;
    Ok(rows)
}

pub(super) fn depends_edges_conn(conn: &Connection) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut stmt = conn
        .prepare("SELECT id, depends_on FROM tasks")
        .map_err(|e| format!("prepare dep edges: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| format!("query dep edges: {e}"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("collect dep edges: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(id, deps)| (id, parse_depends_on(&deps)))
        .collect())
}

fn row_to_goal(row: &rusqlite::Row) -> rusqlite::Result<GoalRow> {
    Ok(GoalRow {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        parent_goal_id: row.get(3)?,
        status: row.get(4)?,
        created_at: row.get(5)?,
    })
}
