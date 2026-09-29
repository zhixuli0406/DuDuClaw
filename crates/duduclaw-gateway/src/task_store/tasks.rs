//! Task CRUD: listing, reading, inserting, updating and removing rows.
//! Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
    pub async fn list_tasks(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
    ) -> Result<Vec<TaskRow>, String> {
        self.list_tasks_filtered(status, agent_id, priority, None)
            .await
    }

    /// `list_tasks` plus an optional `goal_mode` predicate, so goal-scoped
    /// consumers (the `/goals` dashboard page) don't pull the whole board
    /// over the wire just to keep a handful of rows.
    pub async fn list_tasks_filtered(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
        goal_mode: Option<bool>,
    ) -> Result<Vec<TaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = status {
            binds.push(s.to_string());
            sql.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            sql.push_str(&format!(" AND assigned_to = ?{}", binds.len()));
        }
        if let Some(p) = priority {
            binds.push(p.to_string());
            sql.push_str(&format!(" AND priority = ?{}", binds.len()));
        }
        if let Some(g) = goal_mode {
            sql.push_str(if g {
                " AND goal_mode = 1"
            } else {
                " AND goal_mode = 0"
            });
        }
        // I-3b: archived tasks are hidden from every general listing by
        // default — the board, the heartbeat task-board pull, the goal-loop
        // driver's enumeration, autopilot rule scans, and digests all go
        // through this method (or `list_tasks`). Archiving is a deliberate
        // "take this out of active consideration" action, so once archived
        // a task should stop surfacing here the same way a `done` task
        // isn't re-dispatched. Every pre-existing row defaults to
        // archived=0 (migration DEFAULT), so this is behavior-neutral until
        // a caller actually archives something. Callers that need to browse
        // the archive explicitly use `list_tasks_paginated` instead.
        sql.push_str(" AND archived = 0");
        // Pinned tasks float to the top of every list (I-3b "置頂").
        sql.push_str(" ORDER BY pinned DESC, updated_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare list: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_task)
            .map_err(|e| format!("query list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list: {e}"))?;
        Ok(rows)
    }

    /// I-3b: paginated task listing with a total count, for board views that
    /// need to page through more rows than a client-side slice can safely
    /// hold — the prior `/goals` UI hard-cut finished tasks at 20 with no
    /// way to see the rest (`web/src/pages/GoalsPage.tsx` `.slice(0, 20)`).
    /// Same filter set as [`Self::list_tasks_filtered`], plus an explicit
    /// `archived` tri-state so a caller can deliberately browse the
    /// archive instead of always excluding it: `None` or `Some(false)` ⇒
    /// non-archived only (same default as `list_tasks_filtered`),
    /// `Some(true)` ⇒ archived rows only. `limit` is clamped to `[1, 200]`
    /// so a malformed page size can't force an unbounded scan; `offset` is
    /// floored at 0. Ordering matches `list_tasks_filtered`: pinned rows
    /// first, then most-recently-updated.
    pub async fn list_tasks_paginated(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
        goal_mode: Option<bool>,
        archived: Option<bool>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<TaskRow>, i64), String> {
        let conn = self.conn.lock().await;
        let mut count_sql = "SELECT COUNT(*) FROM tasks WHERE 1=1".to_string();
        let mut query_sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = status {
            binds.push(s.to_string());
            let clause = format!(" AND status = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            let clause = format!(" AND assigned_to = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(p) = priority {
            binds.push(p.to_string());
            let clause = format!(" AND priority = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(g) = goal_mode {
            let clause = if g {
                " AND goal_mode = 1"
            } else {
                " AND goal_mode = 0"
            };
            count_sql.push_str(clause);
            query_sql.push_str(clause);
        }
        let archived_clause = if archived == Some(true) {
            " AND archived = 1"
        } else {
            " AND archived = 0"
        };
        count_sql.push_str(archived_clause);
        query_sql.push_str(archived_clause);

        let bounded_limit = limit.clamp(1, 200);
        let bounded_offset = offset.max(0);
        query_sql.push_str(&format!(
            " ORDER BY pinned DESC, updated_at DESC LIMIT {bounded_limit} OFFSET {bounded_offset}"
        ));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();

        let total: i64 = conn
            .query_row(&count_sql, params_ref.as_slice(), |r| r.get(0))
            .map_err(|e| format!("count tasks page: {e}"))?;

        let mut stmt = conn
            .prepare(&query_sql)
            .map_err(|e| format!("prepare list page: {e}"))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_task)
            .map_err(|e| format!("query list page: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list page: {e}"))?;
        Ok((rows, total))
    }

    pub async fn get_task(&self, id: &str) -> Result<Option<TaskRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
            params![id],
            row_to_task,
        )
        .optional()
        .map_err(|e| format!("get task: {e}"))
    }

    pub async fn insert_task(&self, row: &TaskRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO tasks
                (id, title, description, status, priority, assigned_to, created_by,
                 created_at, updated_at, completed_at, blocked_reason,
                 parent_task_id, tags, message_id,
                 claimed_by, claimed_at, lease_expires_at, depends_on, retry_count,
                 max_retries, goal_mode, acceptance_criteria, result_summary, judge_feedback,
                 goal_id, lease_renewed_at, source_channel, source_chat_id,
                 revision_round, diminishing, agent_seconds, source_discord_guild_id,
                 deadline_at, risk_boundary, acceptance_criteria_baseline, pause_reason,
                 plan_pending, archived, pinned, team_spec_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28,
                     ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40)",
            params![
                row.id,
                row.title,
                row.description,
                row.status,
                row.priority,
                row.assigned_to,
                row.created_by,
                row.created_at,
                row.updated_at,
                row.completed_at,
                row.blocked_reason,
                row.parent_task_id,
                row.tags,
                row.message_id,
                row.claimed_by,
                row.claimed_at,
                row.lease_expires_at,
                row.depends_on,
                row.retry_count,
                row.max_retries,
                row.goal_mode as i64,
                row.acceptance_criteria,
                row.result_summary,
                row.judge_feedback,
                row.goal_id,
                row.lease_renewed_at,
                row.source_channel,
                row.source_chat_id,
                row.revision_round,
                row.diminishing as i64,
                row.agent_seconds,
                row.source_discord_guild_id,
                row.deadline_at,
                row.risk_boundary,
                row.acceptance_criteria_baseline,
                row.pause_reason,
                row.plan_pending,
                row.archived as i64,
                row.pinned as i64,
                row.team_spec_json,
            ],
        )
        .map_err(|e| format!("insert task: {e}"))?;
        Ok(())
    }

    /// Freeze this task's team spec, **once**.
    ///
    /// Returns `true` when this call is the one that wrote it, `false` when a
    /// spec was already frozen (the stored one always wins). The `WHERE
    /// team_spec_json IS NULL` guard is what makes "freeze once" a property of
    /// the store rather than a convention two call sites have to remember —
    /// `acceptance_criteria_baseline` relies on having exactly two writers, and
    /// this column has more (goal creation plus the goal loop's
    /// pre-first-round backfill for tasks born on the chat / autopilot /
    /// intent / plan-first paths).
    ///
    /// There is deliberately **no** setter that overwrites an existing value.
    pub async fn freeze_team_spec(&self, id: &str, spec_json: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks SET team_spec_json = ?2, updated_at = ?3
                  WHERE id = ?1 AND team_spec_json IS NULL",
                params![id, spec_json, Utc::now().to_rfc3339()],
            )
            .map_err(|e| format!("freeze team spec: {e}"))?;
        Ok(n == 1)
    }

    /// The raw frozen team spec JSON for a task, if any. `Ok(None)` covers
    /// both "no such task" and "this task has no team" — callers treat them
    /// identically (Solo), so distinguishing them here would buy nothing.
    pub async fn team_spec_json(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT team_spec_json FROM tasks WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|outer| outer.flatten())
        .map_err(|e| format!("read team spec: {e}"))
    }

    /// RFC-26 §4.5 (P6.5): atomically claim an unassigned task. Compare-and-set on
    /// `assigned_to` — only succeeds if the task is currently unassigned (`''`).
    /// Returns `true` if this caller won the claim, `false` if already assigned.
    pub async fn claim_task(&self, id: &str, agent_id: &str, now: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks SET assigned_to=?2, updated_at=?3 WHERE id=?1 AND assigned_to=''",
                params![id, agent_id, now],
            )
            .map_err(|e| format!("claim task: {e}"))?;
        Ok(n > 0)
    }

    /// WP4 hand-off: reassign every *open* (not-`done`) task owned by
    /// `from_agent` to `to_agent`, and follow through on any active claim/lease
    /// so the successor holds the work outright. Returns the number of tasks
    /// moved. Idempotent — a re-run finds nothing left assigned to `from_agent`.
    pub async fn reassign_open_tasks(
        &self,
        from_agent: &str,
        to_agent: &str,
        now: &str,
    ) -> Result<u64, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks
                    SET assigned_to = ?2,
                        claimed_by = CASE WHEN claimed_by = ?1 THEN ?2 ELSE claimed_by END,
                        updated_at = ?3
                  WHERE assigned_to = ?1 AND status != 'done'",
                params![from_agent, to_agent, now],
            )
            .map_err(|e| format!("reassign open tasks: {e}"))?;
        Ok(n as u64)
    }

    /// All `(task_id, parent_task_id)` edges — for cycle detection.
    pub async fn parent_edges(&self) -> Result<Vec<(String, Option<String>)>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, parent_task_id FROM tasks")
            .map_err(|e| format!("prepare edges: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| format!("query edges: {e}"))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| format!("collect edges: {e}"))?;
        Ok(rows)
    }

    /// RFC-26 §4.5: would setting `child.parent = new_parent` create a cycle?
    pub async fn would_create_parent_cycle(
        &self,
        child: &str,
        new_parent: &str,
    ) -> Result<bool, String> {
        let edges = self.parent_edges().await?;
        Ok(introduces_parent_cycle(&edges, child, new_parent))
    }

    pub async fn update_task(
        &self,
        id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<TaskRow>, String> {
        // depends_on rewires the dependency graph — gate it fail-closed at the
        // store boundary: must be a JSON array of ids, no self-dependency, and
        // must not close a cycle (visited-set walk over the current edges).
        // Shape validation is pure; the cycle check runs INSIDE the write
        // transaction below so check and write cannot be raced apart (TOCTOU).
        let new_deps: Option<Vec<String>> = match fields.get("depends_on") {
            Some(deps_val) => {
                let Some(deps_json) = deps_val.as_str() else {
                    return Err("depends_on must be a JSON-array string of task ids".into());
                };
                let Ok(deps) = serde_json::from_str::<Vec<String>>(deps_json) else {
                    return Err("depends_on must be a JSON-array string of task ids".into());
                };
                Some(deps)
            }
            None => None,
        };
        // Scoped block ensures all non-Send refs are dropped before the next await.
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("update task: begin: {e}"))?;
            if let Some(deps) = &new_deps {
                let edges = depends_edges_conn(&tx)?;
                if introduces_dependency_cycle(&edges, id, deps) {
                    return Err(format!(
                        "dependency cycle rejected: task {id} would (transitively) depend on itself"
                    ));
                }
            }
            let now = Utc::now().to_rfc3339();
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<String> = vec![now];

            macro_rules! opt_field {
                ($key:expr, $col:expr) => {
                    if let Some(v) = fields.get($key).and_then(|v| v.as_str()) {
                        binds.push(v.to_string());
                        sets.push(format!("{} = ?{}", $col, binds.len()));
                    }
                };
            }
            opt_field!("title", "title");
            opt_field!("description", "description");
            opt_field!("status", "status");
            opt_field!("priority", "priority");
            opt_field!("assigned_to", "assigned_to");
            opt_field!("blocked_reason", "blocked_reason");
            opt_field!("depends_on", "depends_on");
            // H9-G goal contract freeze: the mutable acceptance_criteria copy
            // is updatable here (store layer is identity-agnostic, matching
            // every other field above) — authorization lives at the caller
            // boundary. The dashboard RPC path (`handlers.rs::handle_tasks_update`)
            // is Operator-ACL-gated and forwards this field through. The
            // agent-facing MCP path (`mcp.rs::handle_tasks_update`) explicitly
            // refuses to forward this field for `goal_mode` tasks before
            // reaching this function, so an agent identity can never exercise
            // this branch on a frozen goal contract. `acceptance_criteria_baseline`
            // deliberately has NO opt_field entry — no code path updates it
            // after `insert_task`.
            opt_field!("acceptance_criteria", "acceptance_criteria");
            if let Some(v) = fields.get("tags").and_then(|v| v.as_str()) {
                binds.push(v.to_string());
                sets.push(format!("tags = ?{}", binds.len()));
            }
            // I-3b task list operations: archived/pinned are booleans, not
            // strings, so they bypass the `opt_field!` macro (which only
            // reads `.as_str()`) — same shape as the `tags` special-case
            // above. `handlers.rs::handle_tasks_archive/unarchive/pin/unpin`
            // are thin wrappers that funnel through this generic update path
            // (same pattern as the existing `tasks.assign` → `handle_tasks_update`
            // delegation), so the HS4 agent-binding ACL check above already
            // covers these writes — no separate authorization branch needed.
            if let Some(v) = fields.get("archived").and_then(|v| v.as_bool()) {
                binds.push((v as i64).to_string());
                sets.push(format!("archived = ?{}", binds.len()));
            }
            if let Some(v) = fields.get("pinned").and_then(|v| v.as_bool()) {
                binds.push((v as i64).to_string());
                sets.push(format!("pinned = ?{}", binds.len()));
            }

            // Auto-set completed_at when status changes to done
            if fields.get("status").and_then(|v| v.as_str()) == Some("done") {
                binds.push(Utc::now().to_rfc3339());
                sets.push(format!("completed_at = ?{}", binds.len()));
            }

            binds.push(id.to_string());
            let sql = format!(
                "UPDATE tasks SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );

            let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            tx.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update task: {e}"))?;
            tx.commit()
                .map_err(|e| format!("update task: commit: {e}"))?;
        }

        self.get_task(id).await
    }

    /// A1 (StateAct): persist the self-reported `pending_hypotheses`
    /// snapshot for a goal-mode task so the next dispatch's `<state>` block
    /// carries it forward. `None` clears the column. Deliberately a small
    /// direct UPDATE rather than routing through [`Self::update_task`]'s
    /// generic field whitelist — keeps this narrow, best-effort write path
    /// isolated from that method's cycle-checking / dependency-rewrite
    /// logic, which has nothing to do with this column.
    pub async fn set_goal_state_json(
        &self,
        id: &str,
        state_json: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE tasks SET goal_state_json = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, state_json, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("set goal_state_json: {e}"))?;
        Ok(())
    }

    /// M7: read-merge-write update of `tasks.goal_state_json`, holding the
    /// store's single connection `Mutex` across the whole read→mutate→write
    /// sequence so two concurrent callers merging DIFFERENT keys into the
    /// same task's snapshot cannot lose either other's write.
    ///
    /// The bug this fixes: `goal_state_json` is a single JSON blob shared by
    /// multiple independent writers (`goal_loop.rs::capture_round_state`
    /// writes `pending_hypotheses`; `dispatch_engine.rs`'s acceptance review
    /// writes `confirmed_facts` — see [`Self::set_goal_state_json`]'s
    /// callers). A writer that does read-then-`set_goal_state_json`-the-whole-blob
    /// outside any lock can race another writer touching a DIFFERENT field:
    /// whichever `UPDATE` lands second wins with a value it computed from a
    /// stale read, silently discarding the other writer's field. Holding this
    /// crate's single `conn` mutex across the read AND the write closes that
    /// window for any two callers that both go through this method.
    ///
    /// `f` receives a mutable `serde_json::Value` — guaranteed to be a JSON
    /// object — to edit in place; whatever it leaves behind is persisted.
    /// Missing / malformed / non-object stored JSON degrades to an empty
    /// `{}` object first (same "never fabricate, just start from nothing"
    /// contract [`crate::goal_state::GoalStateSnapshot::from_json`] uses)
    /// rather than failing the merge.
    ///
    /// Both writers go through this API: `capture_round_state`
    /// (`pending_hypotheses`) and `DispatchEngine::persist_confirmed_facts`
    /// (`confirmed_facts`) — each merge must touch only its own key so
    /// concurrent merges from the two drivers cannot clobber each other.
    pub async fn merge_goal_state_json(
        &self,
        id: &str,
        f: impl FnOnce(&mut serde_json::Value),
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        let current: Option<String> = conn
            .query_row(
                "SELECT goal_state_json FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| format!("merge_goal_state_json read: {e}"))?
            .flatten();
        let mut value: serde_json::Value = current
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !value.is_object() {
            value = serde_json::json!({});
        }
        f(&mut value);
        let new_json = serde_json::to_string(&value)
            .map_err(|e| format!("merge_goal_state_json serialize: {e}"))?;
        conn.execute(
            "UPDATE tasks SET goal_state_json = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, new_json, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("merge_goal_state_json write: {e}"))?;
        Ok(())
    }

    pub async fn remove_task(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let count = conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])
            .map_err(|e| format!("remove task: {e}"))?;
        Ok(count > 0)
    }

    // ── G1 durable dispatch ─────────────────────────────────
    //
    // Migration direction: cross-agent delegation is moving off the legacy
    // file IPC (`bus_queue.jsonl`, consumed by `dispatcher.rs`) onto this
    // durable SQLite lifecycle. The file rail stays as a compatibility path
    // (see `dispatch_engine.rs` header); NEW durable work goes through these
    // methods: `pending` → atomic claim → `in_progress` (leased) →
    // `done` / `review` (goal mode) / `failed` / `needs_human`.
}

pub(super) fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        assigned_to: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        completed_at: row.get(9)?,
        blocked_reason: row.get(10)?,
        parent_task_id: row.get(11)?,
        tags: row.get(12)?,
        message_id: row.get(13)?,
        claimed_by: row.get(14)?,
        claimed_at: row.get(15)?,
        lease_expires_at: row.get(16)?,
        depends_on: row.get(17)?,
        retry_count: row.get(18)?,
        max_retries: row.get(19)?,
        goal_mode: row.get::<_, i64>(20)? != 0,
        acceptance_criteria: row.get(21)?,
        result_summary: row.get(22)?,
        judge_feedback: row.get(23)?,
        goal_id: row.get(24)?,
        lease_renewed_at: row.get(25)?,
        source_channel: row.get(26)?,
        source_chat_id: row.get(27)?,
        revision_round: row.get(28)?,
        diminishing: row.get::<_, i64>(29)? != 0,
        agent_seconds: row.get(30)?,
        goal_state_json: row.get(31)?,
        source_discord_guild_id: row.get(32)?,
        deadline_at: row.get(33)?,
        risk_boundary: row.get(34)?,
        acceptance_criteria_baseline: row.get(35)?,
        pause_reason: row.get(36)?,
        plan_pending: row.get(37)?,
        archived: row.get::<_, i64>(38)? != 0,
        pinned: row.get::<_, i64>(39)? != 0,
        team_spec_json: row.get(40)?,
    })
}
