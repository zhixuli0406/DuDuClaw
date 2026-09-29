//! U4 plans and their ordered steps.
//! Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
    pub async fn insert_plan(&self, row: &PlanRow) -> Result<(), String> {
        if !PLAN_STATUSES.contains(&row.status.as_str()) {
            return Err(format!("invalid plan status: {}", row.status));
        }
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO plans (id, title, description, agent_id, goal_id, status, created_by,
                                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.id,
                row.title,
                row.description,
                row.agent_id,
                row.goal_id,
                row.status,
                row.created_by,
                row.created_at,
                row.updated_at,
            ],
        )
        .map_err(|e| format!("insert plan: {e}"))?;
        Ok(())
    }

    pub async fn get_plan(&self, id: &str) -> Result<Option<PlanRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {PLAN_COLUMNS} FROM plans WHERE id = ?1"),
            params![id],
            row_to_plan,
        )
        .optional()
        .map_err(|e| format!("get plan: {e}"))
    }

    /// Plans newest-activity-first. Optional agent / status filters.
    pub async fn list_plans(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<PlanRow>, String> {
        let conn = self.conn.lock().await;
        let mut sql = format!("SELECT {PLAN_COLUMNS} FROM plans WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            sql.push_str(&format!(" AND agent_id = ?{}", binds.len()));
        }
        if let Some(s) = status {
            binds.push(s.to_string());
            sql.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        sql.push_str(" ORDER BY updated_at DESC");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare plans: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_plan)
            .map_err(|e| format!("query plans: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect plans: {e}"))?;
        Ok(rows)
    }

    /// Update mutable plan fields (`title` / `description` / `status`).
    /// Status is validated fail-closed against [`PLAN_STATUSES`].
    pub async fn update_plan(
        &self,
        id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<PlanRow>, String> {
        if let Some(s) = fields.get("status").and_then(|v| v.as_str()) {
            if !PLAN_STATUSES.contains(&s) {
                return Err(format!("invalid plan status: {s}"));
            }
        }
        {
            let conn = self.conn.lock().await;
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<String> = vec![Utc::now().to_rfc3339()];
            for key in ["title", "description", "status"] {
                if let Some(v) = fields.get(key).and_then(|v| v.as_str()) {
                    binds.push(v.to_string());
                    sets.push(format!("{key} = ?{}", binds.len()));
                }
            }
            binds.push(id.to_string());
            let sql = format!(
                "UPDATE plans SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );
            let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            conn.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update plan: {e}"))?;
        }
        self.get_plan(id).await
    }

    /// Delete a plan and all its steps in one transaction.
    pub async fn remove_plan(&self, id: &str) -> Result<bool, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("remove plan: begin: {e}"))?;
        tx.execute("DELETE FROM plan_steps WHERE plan_id = ?1", params![id])
            .map_err(|e| format!("remove plan steps: {e}"))?;
        let n = tx
            .execute("DELETE FROM plans WHERE id = ?1", params![id])
            .map_err(|e| format!("remove plan: {e}"))?;
        tx.commit()
            .map_err(|e| format!("remove plan: commit: {e}"))?;
        Ok(n > 0)
    }

    /// Steps of a plan in display order. `step_order` ties break on
    /// `created_at, id` so the ordering is total and deterministic.
    pub async fn list_plan_steps(&self, plan_id: &str) -> Result<Vec<PlanStepRow>, String> {
        let conn = self.conn.lock().await;
        list_plan_steps_conn(&conn, plan_id)
    }

    pub async fn get_plan_step(&self, step_id: &str) -> Result<Option<PlanStepRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {PLAN_STEP_COLUMNS} FROM plan_steps WHERE id = ?1"),
            params![step_id],
            row_to_plan_step,
        )
        .optional()
        .map_err(|e| format!("get plan step: {e}"))
    }

    /// Append or insert a step. `position` = target display index (None ⇒
    /// append). The order key is computed inside one IMMEDIATE transaction:
    /// integer-gap midpoint between the neighbours; a collided gap triggers a
    /// renormalization of the whole plan first (see [`PLAN_STEP_ORDER_GAP`]).
    /// Fail-closed enum validation on `assignee_kind` / `status`.
    pub async fn add_plan_step(
        &self,
        plan_id: &str,
        step_id: &str,
        text: &str,
        assignee_kind: &str,
        assignee: &str,
        position: Option<usize>,
    ) -> Result<PlanStepRow, String> {
        if !PLAN_ASSIGNEE_KINDS.contains(&assignee_kind) {
            return Err(format!("invalid assignee_kind: {assignee_kind}"));
        }
        if text.trim().is_empty() {
            return Err("step text is required".into());
        }
        let now = Utc::now().to_rfc3339();
        let row = {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("add step: begin: {e}"))?;
            // Plan must exist — a step may never dangle.
            let plan_exists: Option<String> = tx
                .query_row(
                    "SELECT id FROM plans WHERE id = ?1",
                    params![plan_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| format!("add step: plan lookup: {e}"))?;
            if plan_exists.is_none() {
                return Err(format!("plan not found: {plan_id}"));
            }
            let orders = plan_step_orders_conn(&tx, plan_id)?;
            let index = position.unwrap_or(orders.len()).min(orders.len());
            let order = match plan_order_for_insert(&orders, index) {
                Some(o) => o,
                None => {
                    renormalize_plan_steps_conn(&tx, plan_id, &now)?;
                    let orders = plan_step_orders_conn(&tx, plan_id)?;
                    plan_order_for_insert(&orders, index)
                        .ok_or_else(|| "plan ordering renormalization failed".to_string())?
                }
            };
            let row = PlanStepRow {
                id: step_id.to_string(),
                plan_id: plan_id.to_string(),
                text: text.trim().to_string(),
                assignee_kind: assignee_kind.to_string(),
                assignee: assignee.to_string(),
                status: "todo".into(),
                step_order: order,
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            tx.execute(
                "INSERT INTO plan_steps (id, plan_id, text, assignee_kind, assignee, status,
                                         step_order, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row.id,
                    row.plan_id,
                    row.text,
                    row.assignee_kind,
                    row.assignee,
                    row.status,
                    row.step_order,
                    row.created_at,
                    row.updated_at,
                ],
            )
            .map_err(|e| format!("insert step: {e}"))?;
            tx.execute(
                "UPDATE plans SET updated_at = ?2 WHERE id = ?1",
                params![plan_id, now],
            )
            .map_err(|e| format!("touch plan: {e}"))?;
            tx.commit().map_err(|e| format!("add step: commit: {e}"))?;
            row
        };
        Ok(row)
    }

    /// Update step fields (`text` / `status` / `assignee_kind` / `assignee`).
    /// Enum fields are validated fail-closed. Returns the updated row.
    pub async fn update_plan_step(
        &self,
        step_id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<PlanStepRow>, String> {
        if let Some(s) = fields.get("status").and_then(|v| v.as_str()) {
            if !PLAN_STEP_STATUSES.contains(&s) {
                return Err(format!("invalid step status: {s}"));
            }
        }
        if let Some(k) = fields.get("assignee_kind").and_then(|v| v.as_str()) {
            if !PLAN_ASSIGNEE_KINDS.contains(&k) {
                return Err(format!("invalid assignee_kind: {k}"));
            }
        }
        if let Some(t) = fields.get("text").and_then(|v| v.as_str()) {
            if t.trim().is_empty() {
                return Err("step text must not be empty".into());
            }
        }
        {
            let conn = self.conn.lock().await;
            let now = Utc::now().to_rfc3339();
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<String> = vec![now.clone()];
            for key in ["text", "status", "assignee_kind", "assignee"] {
                if let Some(v) = fields.get(key).and_then(|v| v.as_str()) {
                    binds.push(if key == "text" {
                        v.trim().to_string()
                    } else {
                        v.to_string()
                    });
                    sets.push(format!("{key} = ?{}", binds.len()));
                }
            }
            if sets.len() == 1 {
                return Err("no step fields to update".into());
            }
            binds.push(step_id.to_string());
            let sql = format!(
                "UPDATE plan_steps SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );
            let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            conn.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update step: {e}"))?;
            // Touch the parent plan so `updated_at` reflects the latest co-edit.
            conn.execute(
                "UPDATE plans SET updated_at = ?1
                  WHERE id = (SELECT plan_id FROM plan_steps WHERE id = ?2)",
                params![now, step_id],
            )
            .map_err(|e| format!("touch plan: {e}"))?;
        }
        self.get_plan_step(step_id).await
    }

    /// Move a step to a new display index within its plan. Integer-gap
    /// midpoint write; gap exhaustion renormalizes first — all inside one
    /// IMMEDIATE transaction so concurrent moves cannot interleave.
    pub async fn move_plan_step(
        &self,
        plan_id: &str,
        step_id: &str,
        new_index: usize,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("move step: begin: {e}"))?;
        let steps = list_plan_steps_conn(&tx, plan_id)?;
        let Some(cur_idx) = steps.iter().position(|s| s.id == step_id) else {
            return Ok(false);
        };
        let target = new_index.min(steps.len().saturating_sub(1));
        if target == cur_idx {
            return Ok(true); // no-op move
        }
        // Orders of the remaining steps once the moving one is lifted out.
        let orders: Vec<i64> = steps
            .iter()
            .filter(|s| s.id != step_id)
            .map(|s| s.step_order)
            .collect();
        let order = match plan_order_for_insert(&orders, target) {
            Some(o) => o,
            None => {
                renormalize_plan_steps_conn(&tx, plan_id, &now)?;
                let steps = list_plan_steps_conn(&tx, plan_id)?;
                let orders: Vec<i64> = steps
                    .iter()
                    .filter(|s| s.id != step_id)
                    .map(|s| s.step_order)
                    .collect();
                plan_order_for_insert(&orders, target)
                    .ok_or_else(|| "plan ordering renormalization failed".to_string())?
            }
        };
        tx.execute(
            "UPDATE plan_steps SET step_order = ?2, updated_at = ?3 WHERE id = ?1",
            params![step_id, order, now],
        )
        .map_err(|e| format!("move step: {e}"))?;
        tx.execute(
            "UPDATE plans SET updated_at = ?2 WHERE id = ?1",
            params![plan_id, now],
        )
        .map_err(|e| format!("touch plan: {e}"))?;
        tx.commit().map_err(|e| format!("move step: commit: {e}"))?;
        Ok(true)
    }

    /// Remove a step; returns the removed row (for event attribution).
    pub async fn remove_plan_step(&self, step_id: &str) -> Result<Option<PlanStepRow>, String> {
        let existing = self.get_plan_step(step_id).await?;
        let Some(row) = existing else {
            return Ok(None);
        };
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        conn.execute("DELETE FROM plan_steps WHERE id = ?1", params![step_id])
            .map_err(|e| format!("remove step: {e}"))?;
        conn.execute(
            "UPDATE plans SET updated_at = ?2 WHERE id = ?1",
            params![row.plan_id, now],
        )
        .map_err(|e| format!("touch plan: {e}"))?;
        Ok(Some(row))
    }

    /// Render the agent-facing "## Shared Plan" prompt section for `agent_id`.
    ///
    /// Deterministic, data-derived only (no timestamps, no counters that churn
    /// without a real edit) so the injected block stays **byte-stable** while
    /// the underlying rows are unchanged — prompt-cache friendly. Shows the
    /// most recently updated ACTIVE plan that has at least one step assigned
    /// to this agent; the agent's own open steps are listed explicitly, other
    /// steps as one-line context. `None` ⇒ callers skip the section.
    ///
    /// Wiring: append the returned string to the system prompt in
    /// `claude_runner.rs` next to `build_pending_tasks_section` (one line).
    pub async fn plan_prompt_section(&self, agent_id: &str) -> Result<Option<String>, String> {
        let plans = self.list_plans(Some(agent_id), Some("active")).await?;
        for plan in plans {
            let steps = self.list_plan_steps(&plan.id).await?;
            let mine_open: Vec<&PlanStepRow> = steps
                .iter()
                .filter(|s| {
                    s.assignee_kind == "agent"
                        && s.assignee == agent_id
                        && (s.status == "todo" || s.status == "doing")
                })
                .collect();
            if mine_open.is_empty() {
                continue;
            }
            let done = steps
                .iter()
                .filter(|s| s.status == "done" || s.status == "skipped")
                .count();
            let mut lines: Vec<String> = Vec::new();
            for (i, s) in steps.iter().enumerate() {
                let marker = match s.status.as_str() {
                    "done" => "[x]",
                    "doing" => "[~]",
                    "skipped" => "[-]",
                    _ => "[ ]",
                };
                let holder = if s.assignee.is_empty() {
                    format!("({})", s.assignee_kind)
                } else {
                    format!("({}: {})", s.assignee_kind, s.assignee)
                };
                let yours = if s.assignee_kind == "agent" && s.assignee == agent_id {
                    " ← yours"
                } else {
                    ""
                };
                lines.push(format!(
                    "{}. {marker} {} {holder}{yours}",
                    i + 1,
                    duduclaw_core::truncate_chars(&s.text, 120),
                ));
            }
            return Ok(Some(format!(
                "## Shared Plan: {} ({done}/{} steps done)\n{}\n\n\
                 This plan is co-edited with your user. Use `plan_get` to re-read it and \
                 `plan_update_step` to update the steps marked \"yours\" (status: todo / doing / \
                 done / skipped). Steps assigned to the user are theirs — do not change them.",
                duduclaw_core::truncate_chars(&plan.title, 80),
                steps.len(),
                lines.join("\n"),
            )));
        }
        Ok(None)
    }
}

// ── U4 plan helpers ─────────────────────────────────────────

fn row_to_plan(row: &rusqlite::Row) -> rusqlite::Result<PlanRow> {
    Ok(PlanRow {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        agent_id: row.get(3)?,
        goal_id: row.get(4)?,
        status: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_plan_step(row: &rusqlite::Row) -> rusqlite::Result<PlanStepRow> {
    Ok(PlanStepRow {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        text: row.get(2)?,
        assignee_kind: row.get(3)?,
        assignee: row.get(4)?,
        status: row.get(5)?,
        step_order: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// Sync twin usable under the store Mutex and inside a `Transaction`.
fn list_plan_steps_conn(conn: &Connection, plan_id: &str) -> Result<Vec<PlanStepRow>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {PLAN_STEP_COLUMNS} FROM plan_steps
              WHERE plan_id = ?1 ORDER BY step_order ASC, created_at ASC, id ASC"
        ))
        .map_err(|e| format!("prepare steps: {e}"))?;
    let rows = stmt
        .query_map(params![plan_id], row_to_plan_step)
        .map_err(|e| format!("query steps: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("collect steps: {e}"))?;
    Ok(rows)
}

fn plan_step_orders_conn(conn: &Connection, plan_id: &str) -> Result<Vec<i64>, String> {
    Ok(list_plan_steps_conn(conn, plan_id)?
        .iter()
        .map(|s| s.step_order)
        .collect())
}

/// Rewrite a plan's step orders back to clean gap multiples (1×GAP, 2×GAP, …)
/// preserving the current display order. Called inside the caller's
/// transaction when a midpoint insert would collide.
fn renormalize_plan_steps_conn(conn: &Connection, plan_id: &str, now: &str) -> Result<(), String> {
    let steps = list_plan_steps_conn(conn, plan_id)?;
    for (i, s) in steps.iter().enumerate() {
        conn.execute(
            "UPDATE plan_steps SET step_order = ?2, updated_at = ?3 WHERE id = ?1",
            params![s.id, ((i as i64) + 1) * PLAN_STEP_ORDER_GAP, now],
        )
        .map_err(|e| format!("renormalize step: {e}"))?;
    }
    Ok(())
}

/// Compute the `step_order` key for inserting at display `index` among the
/// existing sorted `orders`. Integer-gap semantics:
/// - append (index ≥ len) ⇒ `last + GAP` (always succeeds);
/// - front / between ⇒ midpoint of the neighbours (`prev` = 0 for the front);
/// - `None` ⇒ the gap is exhausted (midpoint would collide) — the caller must
///   renormalize the plan and retry. Pure + unit-tested.
pub fn plan_order_for_insert(orders: &[i64], index: usize) -> Option<i64> {
    if index >= orders.len() {
        return Some(orders.last().copied().unwrap_or(0) + PLAN_STEP_ORDER_GAP);
    }
    let prev = if index == 0 { 0 } else { orders[index - 1] };
    let next = orders[index];
    let mid = prev + (next - prev) / 2;
    if mid > prev && mid < next {
        Some(mid)
    } else {
        None
    }
}
