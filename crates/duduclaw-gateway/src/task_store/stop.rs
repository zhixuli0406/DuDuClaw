//! "Stop this run (and its sub-tasks)": the durable stop request and the one
//! transaction that cancels a task tree. Cancelling bumps every cancelled
//! task's `authority_revision` through the P0-B trigger, so prepared
//! operations and kickoff approvals bound to those tasks can no longer be
//! claimed or executed.

use super::*;

/// Result of [`TaskStore::stop_task_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopTreeOutcome {
    /// The tree was cancelled and a `cancel_pending` request recorded.
    Requested(StopRequestRow),
    /// A stop request for this root already exists.
    AlreadyRequested(StopRequestRow),
    /// The root's `authority_revision` moved (or it does not exist).
    Conflict { current_revision: Option<i64> },
    /// The root already reached a terminal state; nothing to stop.
    AlreadyFinished { status: String },
}

impl TaskStore {
    /// The `tasks.db` path (its parent is the home directory).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// One `BEGIN IMMEDIATE`: CAS the root on `authority_revision`, collect the
    /// `parent_task_id` tree, cancel every non-terminal member, discard their
    /// open steering, abandon `intended` intents and record the request.
    pub async fn stop_task_tree(
        &self,
        root_task_id: &str,
        expected_authority_revision: i64,
        requested_by: &str,
        counts_as_failure: bool,
        now: DateTime<Utc>,
    ) -> Result<StopTreeOutcome, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("stop tree: begin: {e}"))?;
        if let Some(existing) = tx
            .query_row(
                &format!("SELECT {STOP_COLUMNS} FROM task_stop_requests WHERE root_task_id = ?1"),
                params![root_task_id],
                row_to_stop,
            )
            .optional()
            .map_err(|e| format!("stop tree: existing: {e}"))?
        {
            return Ok(StopTreeOutcome::AlreadyRequested(existing));
        }
        let root: Option<(i64, String)> = tx
            .query_row(
                "SELECT authority_revision, status FROM tasks WHERE id = ?1 AND kind IN ('task','goal')",
                params![root_task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("stop tree: root: {e}"))?;
        let Some((revision, status)) = root else {
            return Ok(StopTreeOutcome::Conflict {
                current_revision: None,
            });
        };
        if revision != expected_authority_revision {
            return Ok(StopTreeOutcome::Conflict {
                current_revision: Some(revision),
            });
        }
        if matches!(status.as_str(), "done" | "failed" | "cancelled") {
            return Ok(StopTreeOutcome::AlreadyFinished { status });
        }
        let tree: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "WITH RECURSIVE tree(id) AS (
                        SELECT ?1 UNION SELECT t.id FROM tasks t JOIN tree ON t.parent_task_id = tree.id)
                     SELECT id FROM tree LIMIT ?2",
                )
                .map_err(|e| format!("stop tree: walk: {e}"))?;
            stmt.query_map(params![root_task_id, (STOP_TREE_LIMIT + 1) as i64], |r| {
                r.get(0)
            })
            .map_err(|e| format!("stop tree: walk: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("stop tree: walk: {e}"))?
        };
        // S-M2: a tree too large for one pass is never left untouched. The
        // root and the first batch are cancelled now and the request is
        // recorded (the ancestry check then refuses any new child or
        // dispatch under the root); reconciliation cancels the rest in
        // batches ([`TaskStore::cancel_stop_tree_batch`]).
        let mut tree = tree;
        let truncated = tree.len() > STOP_TREE_LIMIT;
        tree.truncate(STOP_TREE_LIMIT);
        let ids_json = serde_json::to_string(&tree).map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE tasks SET status='cancelled', judge_feedback='stopped by operator',
                    pause_reason=NULL, updated_at=?2
              WHERE id IN (SELECT value FROM json_each(?1)) AND kind IN ('task','goal')
                AND status NOT IN ('done','cancelled','failed')",
            params![ids_json, now_s],
        )
        .map_err(|e| format!("stop tree: cancel: {e}"))?;
        tx.execute(
            "UPDATE task_steering SET state='discarded', discard_reason='task_stopped', updated_at=?2
              WHERE task_id IN (SELECT value FROM json_each(?1)) AND state IN ('pending','delivering')",
            params![ids_json, now_s],
        )
        .map_err(|e| format!("stop tree: steering: {e}"))?;
        tx.execute(
            "UPDATE task_dispatch_intents SET state='abandoned', updated_at=?2
              WHERE task_id IN (SELECT value FROM json_each(?1)) AND state='intended'",
            params![ids_json, now_s],
        )
        .map_err(|e| format!("stop tree: intents: {e}"))?;
        let row = StopRequestRow {
            root_task_id: root_task_id.to_string(),
            requested_by: requested_by.to_string(),
            requested_at: now_s.clone(),
            expected_authority_revision,
            affected_task_ids: tree,
            state: "cancel_pending".into(),
            detail_json: truncated.then(|| r#"{"tree_truncated":true}"#.to_string()),
            updated_at: now_s.clone(),
            counts_as_failure,
        };
        tx.execute(
            &format!(
                "INSERT INTO task_stop_requests ({STOP_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
            ),
            params![
                row.root_task_id,
                row.requested_by,
                row.requested_at,
                row.expected_authority_revision,
                ids_json,
                row.state,
                row.detail_json,
                row.updated_at,
                row.counts_as_failure as i64
            ],
        )
        .map_err(|e| format!("stop tree: record: {e}"))?;
        tx.commit().map_err(|e| format!("stop tree: commit: {e}"))?;
        Ok(StopTreeOutcome::Requested(row))
    }

    pub async fn get_stop_request(&self, root: &str) -> Result<Option<StopRequestRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {STOP_COLUMNS} FROM task_stop_requests WHERE root_task_id = ?1"),
            params![root],
            row_to_stop,
        )
        .optional()
        .map_err(|e| format!("get stop request: {e}"))
    }

    /// Stop requests still waiting for a running turn / external action.
    pub async fn pending_stop_requests(&self) -> Result<Vec<StopRequestRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {STOP_COLUMNS} FROM task_stop_requests WHERE state = 'cancel_pending'
                 ORDER BY requested_at"
            ))
            .map_err(|e| format!("pending stop requests: {e}"))?;
        stmt.query_map([], row_to_stop)
            .map_err(|e| format!("pending stop requests: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("pending stop requests: {e}"))
    }

    /// Record the latest reconciliation. Only `cancel_pending` rows move; a
    /// settled request is never reopened.
    pub async fn update_stop_request(
        &self,
        root: &str,
        state: &str,
        detail_json: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        if !matches!(state, "cancel_pending" | "stopped" | "stopped_uncertain") {
            return Err(format!("invalid stop state: {state}"));
        }
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE task_stop_requests SET state=?2, detail_json=?3, updated_at=?4
                  WHERE root_task_id=?1 AND state='cancel_pending'",
                params![root, state, detail_json, resp_ts(now)],
            )
            .map_err(|e| format!("update stop request: {e}"))?;
        Ok(n == 1)
    }

    /// Whether a task id is inside any stop request's tree.
    pub async fn in_stop_tree(&self, task_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {}", in_stop_tree_sql("?1")),
            params![task_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("stop tree lookup: {e}"))
    }

    /// M-5: across `ids`, whether any carries a team spec and the latest
    /// lease, in one query.
    pub async fn team_and_lease_in(
        &self,
        ids: &[String],
    ) -> Result<(bool, Option<String>), String> {
        if ids.is_empty() {
            return Ok((false, None));
        }
        let ids_json = serde_json::to_string(ids).map_err(|e| e.to_string())?;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COALESCE(MAX(team_spec_json IS NOT NULL), 0), MAX(lease_expires_at)
               FROM tasks WHERE id IN (SELECT value FROM json_each(?1))",
            params![ids_json],
            |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, Option<String>>(1)?)),
        )
        .map_err(|e| format!("team and lease: {e}"))
    }

    /// Open (not done / cancelled / failed) direct children of a task: the
    /// per-task sub-task cap counts only these (M3-4), so a long-lived task
    /// is not locked out by children it already finished.
    pub async fn child_count(&self, parent: &str) -> Result<i64, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE parent_task_id = ?1 \
               AND status NOT IN ('done','cancelled','failed')",
            params![parent],
            |r| r.get(0),
        )
        .map_err(|e| format!("child count: {e}"))
    }

    /// Whether `ancestor` is `task_id` itself or one of its ancestors
    /// (parent chain, up to [`STOP_ANCESTRY_DEPTH`] levels).
    pub async fn is_descendant_of(&self, task_id: &str, ancestor: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT EXISTS (WITH RECURSIVE anc(id, depth) AS ( \
                   SELECT ?1, 0 UNION ALL SELECT t.parent_task_id, anc.depth + 1 FROM tasks t \
                   JOIN anc ON t.id = anc.id WHERE t.parent_task_id IS NOT NULL \
                     AND anc.depth < {STOP_ANCESTRY_DEPTH}) SELECT 1 FROM anc WHERE id = ?2)"
            ),
            params![task_id, ancestor],
            |r| r.get(0),
        )
        .map_err(|e| format!("ancestry: {e}"))
    }

    /// Whether `task_id` or one of its ancestors is a responsibility
    /// occurrence, i.e. the task belongs to a responsibility run's tree.
    pub async fn in_occurrence_tree(&self, task_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM responsibility_occurrences o WHERE o.task_id IN ( \
                   WITH RECURSIVE anc(id, depth) AS ( \
                     SELECT ?1, 0 UNION ALL SELECT t.parent_task_id, anc.depth + 1 FROM tasks t \
                     JOIN anc ON t.id = anc.id WHERE t.parent_task_id IS NOT NULL \
                       AND anc.depth < {STOP_ANCESTRY_DEPTH}) SELECT id FROM anc))"
            ),
            params![task_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("occurrence ancestry: {e}"))
    }

    /// Whether the stop covering `task_id` (its own or an ancestor's) counts
    /// an ended occurrence as unsuccessful.
    pub async fn stop_counts_as_failure(&self, task_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM task_stop_requests sr WHERE sr.counts_as_failure = 1 \
                   AND sr.root_task_id IN (WITH RECURSIVE anc(id, depth) AS ( \
                     SELECT ?1, 0 UNION ALL SELECT t.parent_task_id, anc.depth + 1 FROM tasks t \
                     JOIN anc ON t.id = anc.id WHERE t.parent_task_id IS NOT NULL \
                       AND anc.depth < {STOP_ANCESTRY_DEPTH}) SELECT id FROM anc))"
            ),
            params![task_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("stop failure flag: {e}"))
    }

    /// Up to `limit` ids of the tree under `root` (root first).
    pub async fn stop_tree_ids(&self, root: &str, limit: usize) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "WITH RECURSIVE tree(id) AS (
                    SELECT ?1 UNION SELECT t.id FROM tasks t JOIN tree ON t.parent_task_id = tree.id)
                 SELECT id FROM tree LIMIT ?2",
            )
            .map_err(|e| format!("stop tree ids: {e}"))?;
        stmt.query_map(params![root, limit as i64], |r| r.get(0))
            .map_err(|e| format!("stop tree ids: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("stop tree ids: {e}"))
    }

    /// Cancel up to [`STOP_TREE_LIMIT`] still-open members of a stopped tree;
    /// returns `(cancelled, still_open)`.
    pub async fn cancel_stop_tree_batch(
        &self,
        root: &str,
        now: DateTime<Utc>,
    ) -> Result<(usize, usize), String> {
        let ids = self.stop_tree_ids(root, STOP_TREE_SCAN_LIMIT).await?;
        let conn = self.conn.lock().await;
        let open: Vec<String> = {
            let ids_json = serde_json::to_string(&ids).map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM tasks WHERE id IN (SELECT value FROM json_each(?1))
                       AND kind IN ('task','goal') AND status NOT IN ('done','cancelled','failed')",
                )
                .map_err(|e| format!("stop batch: {e}"))?;
            stmt.query_map(params![ids_json], |r| r.get(0))
                .map_err(|e| format!("stop batch: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("stop batch: {e}"))?
        };
        let batch: Vec<&String> = open.iter().take(STOP_TREE_LIMIT).collect();
        let batch_json = serde_json::to_string(&batch).map_err(|e| e.to_string())?;
        let n = conn
            .execute(
                "UPDATE tasks SET status='cancelled', judge_feedback='stopped by operator',
                        pause_reason=NULL, updated_at=?2
                  WHERE id IN (SELECT value FROM json_each(?1)) AND kind IN ('task','goal')
                    AND status NOT IN ('done','cancelled','failed')",
                params![batch_json, resp_ts(now)],
            )
            .map_err(|e| format!("stop batch: cancel: {e}"))?;
        Ok((n, open.len().saturating_sub(n)))
    }

    /// Tree members still holding a claim: `(running, lapsed)` — a claim
    /// whose lease has not expired may still be executing a turn on any
    /// path (heartbeat, delegation, …); a claim whose lease lapsed without
    /// being released cannot be confirmed either way (E-H3).
    pub async fn claimed_in_tree(
        &self,
        ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<(Vec<(String, String)>, Vec<String>), String> {
        let ids_json = serde_json::to_string(ids).map_err(|e| e.to_string())?;
        let now_s = resp_ts(now);
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, COALESCE(lease_expires_at, '') FROM tasks
                  WHERE id IN (SELECT value FROM json_each(?1)) AND claimed_by IS NOT NULL
                    AND status = 'cancelled'",
            )
            .map_err(|e| format!("claimed in tree: {e}"))?;
        let rows: Vec<(String, String)> = stmt
            .query_map(params![ids_json], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| format!("claimed in tree: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("claimed in tree: {e}"))?;
        let (mut running, mut lapsed) = (Vec::new(), Vec::new());
        for (id, lease) in rows {
            match crate::task_store::parse_ts(&lease) {
                Some(l) if resp_ts(l) > now_s => running.push((id, resp_ts(l))),
                _ => lapsed.push(id),
            }
        }
        Ok((running, lapsed))
    }
}
