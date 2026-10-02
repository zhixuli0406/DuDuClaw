//! Claiming, leasing and zombie reclamation — the compare-and-set paths.
//! Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
    /// Atomically claim a `pending` task. Compare-and-set: only the caller
    /// whose `UPDATE` flips exactly one row wins — concurrent claimers on the
    /// same id get [`ClaimOutcome::NotClaimable`]. Sets the lease so a crashed
    /// worker is reclaimable.
    ///
    /// Dependency gating is enforced HERE, inside one IMMEDIATE transaction:
    /// a `pending` task whose `depends_on` ids are not all `done` returns
    /// [`ClaimOutcome::BlockedByDeps`] with the unmet ids — the deps check and
    /// the claim write cannot be raced apart, so the gate can't be bypassed
    /// (fail-closed: a dep referencing a missing task counts as unmet).
    pub async fn atomic_claim(
        &self,
        id: &str,
        agent_id: &str,
        now: &str,
        lease_expires_at: &str,
    ) -> Result<ClaimOutcome, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("atomic claim: begin: {e}"))?;

        // Load the claim-relevant state under the write lock.
        let row: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT status, claimed_by, depends_on FROM tasks WHERE id = ?1 AND kind IN ('task','goal')",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| format!("atomic claim: load: {e}"))?;
        let Some((status, claimed_by, depends_on)) = row else {
            return Ok(ClaimOutcome::NotClaimable);
        };
        // `revising` (Iterative Kanban) is claimable exactly like `pending`: a
        // judge rejection parks the task there with claim/lease cleared, and the
        // goal loop re-dispatches it for the next round. Same fail-closed
        // dependency gate applies.
        if !matches!(status.as_str(), "pending" | "revising") || claimed_by.is_some() {
            return Ok(ClaimOutcome::NotClaimable);
        }

        // Dependency gate inside the same transaction (HIGH-1): every
        // depends_on id must be an existing task in status `done`.
        let deps = parse_depends_on(&depends_on);
        if !deps.is_empty() {
            let mut unmet: Vec<String> = Vec::new();
            for dep in &deps {
                let dep_status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM tasks WHERE id = ?1",
                        params![dep],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| format!("atomic claim: dep check: {e}"))?;
                if dep_status.as_deref() != Some("done") {
                    unmet.push(dep.clone());
                }
            }
            if !unmet.is_empty() {
                // Drop the transaction (rollback) — nothing was written.
                return Ok(ClaimOutcome::BlockedByDeps(unmet));
            }
        }

        let n = tx
            .execute(
                "UPDATE tasks
                    SET claimed_by = ?2, claimed_at = ?3, lease_expires_at = ?4,
                        lease_renewed_at = ?3,
                        status = 'in_progress', assigned_to = ?2, updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal') AND status IN ('pending', 'revising') AND claimed_by IS NULL",
                params![id, agent_id, now, lease_expires_at],
            )
            .map_err(|e| format!("atomic claim: {e}"))?;
        tx.commit()
            .map_err(|e| format!("atomic claim: commit: {e}"))?;
        Ok(if n == 1 {
            ClaimOutcome::Claimed
        } else {
            ClaimOutcome::NotClaimable
        })
    }

    /// Heartbeat: extend the lease of a task the caller currently holds.
    /// Guarded on `claimed_by` so a worker cannot renew someone else's lease.
    /// Also stamps `lease_renewed_at` — the renewal anchor zombie reclaim uses
    /// for its conservative grace window.
    pub async fn renew_lease(
        &self,
        id: &str,
        agent_id: &str,
        new_expiry: &str,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks SET lease_expires_at = ?3, lease_renewed_at = ?4, updated_at = ?4
                  WHERE id = ?1 AND kind IN ('task','goal') AND claimed_by = ?2 AND status = 'in_progress'",
                params![id, agent_id, new_expiry, now],
            )
            .map_err(|e| format!("renew lease: {e}"))?;
        Ok(n == 1)
    }

    /// The set of task ids currently `done` — used for dependency gating.
    pub async fn done_task_ids(&self) -> Result<HashSet<String>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id FROM tasks WHERE status = 'done'")
            .map_err(|e| format!("prepare done ids: {e}"))?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("query done ids: {e}"))?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|e| format!("collect done ids: {e}"))?;
        Ok(ids)
    }

    /// All tasks in a given status (helper for the dispatcher's review pass).
    pub async fn tasks_in_status(&self, status: &str) -> Result<Vec<TaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE status = ?1 ORDER BY created_at ASC"
            ))
            .map_err(|e| format!("prepare status query: {e}"))?;
        let rows = stmt
            .query_map(params![status], row_to_task)
            .map_err(|e| format!("query status: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect status: {e}"))?;
        Ok(rows)
    }

    /// Pending tasks that are claimable *right now*: unclaimed, not
    /// archived, and with every `depends_on` id already `done`. Dependency
    /// filtering is done in Rust (parsing the JSON array) against the
    /// current `done` set. Archiving a still-pending/unclaimed task (the
    /// `/goals` board "take out of active consideration" action) must
    /// remove it from the dispatch engine's pickup queue, same as it's
    /// already hidden from `list_tasks_filtered`'s default view.
    pub async fn claimable_tasks(&self) -> Result<Vec<TaskRow>, String> {
        let done = self.done_task_ids().await?;
        let pending = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {TASK_COLUMNS} FROM tasks
                      WHERE kind IN ('task','goal') AND status = 'pending' AND claimed_by IS NULL AND archived = 0
                      ORDER BY created_at ASC"
                ))
                .map_err(|e| format!("prepare claimable: {e}"))?;
            stmt.query_map([], row_to_task)
                .map_err(|e| format!("query claimable: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("collect claimable: {e}"))?
        };
        Ok(pending
            .into_iter()
            .filter(|t| deps_satisfied(&parse_depends_on(&t.depends_on), &done))
            .collect())
    }

    /// Reclaim zombie tasks: `in_progress` rows with a non-null, elapsed lease.
    /// Retries remaining → requeue to `pending` (lease/claim cleared,
    /// `retry_count` incremented); budget exhausted → `failed`. Tasks with a
    /// NULL lease (manual board tasks) are never touched.
    pub async fn reclaim_zombies(&self, now: &str) -> Result<Vec<ZombieOutcome>, String> {
        // Load candidates first, decide in Rust (robust RFC3339 comparison),
        // then apply guarded updates.
        let candidates: Vec<TaskRow> = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {TASK_COLUMNS} FROM tasks
                      WHERE kind IN ('task','goal') AND status = 'in_progress'
                        AND lease_expires_at IS NOT NULL
                        AND claimed_by IS NOT NULL"
                ))
                .map_err(|e| format!("prepare zombie scan: {e}"))?;
            stmt.query_map([], row_to_task)
                .map_err(|e| format!("query zombie scan: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("collect zombie scan: {e}"))?
        };

        let mut outcomes = Vec::new();
        for t in candidates {
            let Some(lease) = t.lease_expires_at.as_deref() else {
                continue;
            };
            // Conservative reclaim: lease expired AND no renewal arrived within
            // a further full lease window (anchor = last renewal, or the claim
            // itself). A worker whose renewal ticker is still alive keeps
            // pushing `lease_expires_at` forward and is never reclaimed.
            let anchor = t.lease_renewed_at.as_deref().or(t.claimed_at.as_deref());
            if !zombie_reclaim_due(lease, anchor, now) {
                continue;
            }
            let claimer = t.claimed_by.clone().unwrap_or_default();
            match zombie_action(t.retry_count, t.max_retries) {
                ZombieAction::Requeue => {
                    let new_retry = t.retry_count + 1;
                    if self
                        .requeue_zombie_cas(&t.id, &claimer, lease, new_retry, now)
                        .await?
                    {
                        outcomes.push(ZombieOutcome {
                            task_id: t.id,
                            action: ZombieAction::Requeue,
                            retry_count: new_retry,
                        });
                    }
                }
                ZombieAction::Fail => {
                    if self.fail_zombie_cas(&t.id, &claimer, lease, now).await? {
                        outcomes.push(ZombieOutcome {
                            task_id: t.id,
                            action: ZombieAction::Fail,
                            retry_count: t.retry_count,
                        });
                    }
                }
            }
        }
        Ok(outcomes)
    }

    /// Requeue one zombie. Optimistic CAS on `lease_expires_at` (the value the
    /// zombie scan observed): a renewal that lands between scan and write moves
    /// the lease forward, the CAS misses, and the live worker keeps its claim.
    pub(crate) async fn requeue_zombie_cas(
        &self,
        id: &str,
        claimer: &str,
        scanned_lease: &str,
        new_retry: i64,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'pending', claimed_by = NULL, claimed_at = NULL,
                        lease_expires_at = NULL, retry_count = ?2, updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal') AND claimed_by = ?4 AND status = 'in_progress'
                    AND lease_expires_at = ?5",
                params![id, new_retry, now, claimer, scanned_lease],
            )
            .map_err(|e| format!("requeue zombie: {e}"))?;
        Ok(n == 1)
    }

    /// Fail one zombie whose retry budget is spent. Same `lease_expires_at`
    /// CAS as [`Self::requeue_zombie_cas`] so a racing renewal is never failed.
    pub(crate) async fn fail_zombie_cas(
        &self,
        id: &str,
        claimer: &str,
        scanned_lease: &str,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'failed', lease_expires_at = NULL,
                        blocked_reason = ?2, updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal') AND claimed_by = ?4 AND status = 'in_progress'
                    AND lease_expires_at = ?5",
                params![
                    id,
                    "lease expired; retry budget exhausted",
                    now,
                    claimer,
                    scanned_lease
                ],
            )
            .map_err(|e| format!("fail zombie: {e}"))?;
        Ok(n == 1)
    }
}
