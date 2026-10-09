//! Occurrences: the only place a responsibility turns a wake fact into a new
//! goal task (`materialize_occurrence`), and the settlement of a finished one.

use super::responsibility::{get_responsibility_conn, pending_fires_conn};
use super::*;

/// Everything `materialize_occurrence` needs that is computed outside the
/// transaction (cost lives in another database; the task row is built by the
/// caller from the fact it peeked).
#[derive(Debug, Clone)]
pub struct MaterializeRequest {
    pub responsibility_id: String,
    /// The fact the caller peeked and built `task` from. If it is no longer
    /// the earliest pending fact, nothing is written (`FireChanged`).
    pub expected_fire_id: String,
    /// The epoch the caller read. A different epoch drops pending facts.
    pub expected_epoch: i64,
    pub task: TaskRow,
    pub period_key: String,
    /// Spent in the current window as read from cost telemetry, already
    /// floored at each occurrence's reservation / charge.
    pub period_spent_cents: i64,
    pub predecessor_task_id: Option<String>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializeOutcome {
    Created {
        task_id: String,
        coalesced: usize,
    },
    /// Responsibility missing, not `active`, or past `stop_at`.
    NotActive,
    /// Epoch moved since the caller read it; pending facts were dropped.
    EpochChanged,
    /// The peeked fact is no longer the earliest pending one.
    FireChanged,
    /// An occurrence is still open.
    OccurrenceOpen,
    /// `min_wake_interval_secs` has not elapsed since the last occurrence.
    IntervalNotElapsed,
    /// Window count or cost exhausted; the responsibility is now `budget_paused`.
    BudgetExhausted,
}

/// Facts the period check needs for one window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeriodUsage {
    pub occurrences: i64,
    /// `(task id, stored floor)` of every occurrence in the window, so the
    /// caller can floor measured cost per occurrence.
    pub entries: Vec<(String, i64)>,
    /// Σ charged (settled) + Σ reserved (open), the stored floor.
    pub stored_cents: i64,
}

impl TaskStore {
    /// One `BEGIN IMMEDIATE`: re-check the responsibility, the fact, the
    /// one-open-occurrence rule, the wake interval and the window budget,
    /// then insert the goal task, the occurrence link, consume the primary
    /// fact, coalesce the rest, consume one-shot subscriptions and stamp
    /// `last_occurrence_at`. The partial unique index is the last line
    /// against a second open occurrence (a constraint error ⇒ someone else
    /// already did it ⇒ `OccurrenceOpen`).
    pub async fn materialize_occurrence(
        &self,
        req: &MaterializeRequest,
    ) -> Result<MaterializeOutcome, String> {
        let now_s = resp_ts(req.now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("materialize: begin: {e}"))?;
        let Some(resp) = get_responsibility_conn(&tx, &req.responsibility_id)? else {
            return Ok(MaterializeOutcome::NotActive);
        };
        if resp.state != "active" || parse_ts(&resp.stop_at).is_none_or(|t| req.now >= t) {
            return Ok(MaterializeOutcome::NotActive);
        }
        let pending = pending_fires_conn(&tx, &resp.responsibility_id)?;
        let Some(first) = pending.first() else {
            return Ok(MaterializeOutcome::FireChanged);
        };
        if first.control_epoch != resp.control_epoch || req.expected_epoch != resp.control_epoch {
            tx.execute(
                "UPDATE wakeup_fires SET state='dropped', drop_reason='epoch_changed', settled_at=?2
                  WHERE responsibility_id=?1 AND state='pending' AND control_epoch<>?3",
                params![resp.responsibility_id, now_s, resp.control_epoch],
            )
            .map_err(|e| format!("materialize: drop stale fires: {e}"))?;
            tx.commit()
                .map_err(|e| format!("materialize: commit: {e}"))?;
            return Ok(MaterializeOutcome::EpochChanged);
        }
        if first.fire_id != req.expected_fire_id {
            return Ok(MaterializeOutcome::FireChanged);
        }
        let open: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM responsibility_occurrences
                   WHERE responsibility_id=?1 AND outcome IS NULL)",
                params![resp.responsibility_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("materialize: open check: {e}"))?;
        if open {
            return Ok(MaterializeOutcome::OccurrenceOpen);
        }
        if let Some(last) = resp.last_occurrence_at.as_deref().and_then(parse_ts) {
            if last + chrono::Duration::seconds(resp.min_wake_interval_secs) > req.now {
                return Ok(MaterializeOutcome::IntervalNotElapsed);
            }
        }
        let usage = period_usage_conn(&tx, &resp.responsibility_id, &req.period_key)?;
        let spent = usage.stored_cents.max(req.period_spent_cents);
        if usage.occurrences >= resp.period_occurrence_limit
            || spent.saturating_add(resp.occurrence_cost_cap_cents) > resp.period_cost_limit_cents
        {
            tx.execute(
                "UPDATE responsibilities SET state='budget_paused', state_reason=?2,
                        state_changed_by='system', state_changed_at=?3, updated_at=?3
                  WHERE responsibility_id=?1 AND state='active'",
                params![
                    resp.responsibility_id,
                    format!("budget:{}", req.period_key),
                    now_s
                ],
            )
            .map_err(|e| format!("materialize: budget pause: {e}"))?;
            tx.commit()
                .map_err(|e| format!("materialize: commit: {e}"))?;
            return Ok(MaterializeOutcome::BudgetExhausted);
        }

        super::tasks::insert_task_tx(&tx, &req.task)?;
        let inserted = tx.execute(
            "INSERT INTO responsibility_occurrences (responsibility_id, occurrence_key, task_id,
                    contract_revision, control_epoch, period_key, reserved_cents, charged_cents,
                    cost_basis, outcome, predecessor_task_id, created_at, settled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL, ?8, ?9, NULL)",
            params![
                resp.responsibility_id,
                first.fire_id,
                req.task.id,
                resp.contract_revision,
                resp.control_epoch,
                req.period_key,
                resp.occurrence_cost_cap_cents,
                req.predecessor_task_id,
                now_s
            ],
        );
        if let Err(e) = inserted {
            if matches!(&e, rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation)
            {
                // Someone else already holds the open occurrence. Rolled back
                // by dropping `tx`.
                return Ok(MaterializeOutcome::OccurrenceOpen);
            }
            return Err(format!("materialize: insert occurrence: {e}"));
        }
        tx.execute(
            "UPDATE wakeup_fires SET state='consumed', occurrence_task_id=?2, settled_at=?3
              WHERE fire_id=?1 AND state='pending'",
            params![first.fire_id, req.task.id, now_s],
        )
        .map_err(|e| format!("materialize: consume fire: {e}"))?;
        let coalesced = tx
            .execute(
                "UPDATE wakeup_fires SET state='coalesced', occurrence_task_id=?2, settled_at=?3
                  WHERE responsibility_id=?1 AND state='pending'",
                params![resp.responsibility_id, req.task.id, now_s],
            )
            .map_err(|e| format!("materialize: coalesce: {e}"))?;
        // One-shot subscriptions are spent once any of their facts became
        // (part of) this occurrence. Recurring schedule slots and operator
        // event subscriptions stay armed.
        tx.execute(
            "UPDATE task_wakeups SET state='consumed', updated_at=?3
              WHERE responsibility_id=?1 AND state='armed' AND recurring=0 AND kind IN ('time','decision')
                AND wakeup_id IN (SELECT wakeup_id FROM wakeup_fires WHERE occurrence_task_id=?2)",
            params![resp.responsibility_id, req.task.id, now_s],
        )
        .map_err(|e| format!("materialize: consume wakeups: {e}"))?;
        tx.execute(
            "UPDATE responsibilities SET last_occurrence_at=?2, updated_at=?2
              WHERE responsibility_id=?1",
            params![resp.responsibility_id, now_s],
        )
        .map_err(|e| format!("materialize: stamp: {e}"))?;
        tx.commit()
            .map_err(|e| format!("materialize: commit: {e}"))?;
        Ok(MaterializeOutcome::Created {
            task_id: req.task.id.clone(),
            coalesced,
        })
    }

    /// Window usage from the stored ledger (no cost telemetry involved).
    pub async fn period_usage(
        &self,
        responsibility_id: &str,
        period_key: &str,
    ) -> Result<PeriodUsage, String> {
        let conn = self.conn.lock().await;
        period_usage_conn(&conn, responsibility_id, period_key)
    }

    /// The occurrence link of a task, with its responsibility.
    pub async fn occurrence_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<(OccurrenceRow, ResponsibilityRow)>, String> {
        let conn = self.conn.lock().await;
        let occ = conn
            .query_row(
                &format!("SELECT {OCC_COLUMNS} FROM responsibility_occurrences WHERE task_id = ?1"),
                params![task_id],
                row_to_occurrence,
            )
            .optional()
            .map_err(|e| format!("occurrence for task: {e}"))?;
        let Some(occ) = occ else { return Ok(None) };
        let resp = get_responsibility_conn(&conn, &occ.responsibility_id)?
            .ok_or("occurrence without responsibility")?;
        Ok(Some((occ, resp)))
    }

    /// The responsibilities whose occurrence is `task_id` itself or one of
    /// its ancestors (parent chain, up to [`STOP_ANCESTRY_DEPTH`] levels):
    /// every responsibility run whose tree contains the task.
    pub async fn occurrence_tree_responsibilities(
        &self,
        task_id: &str,
    ) -> Result<Vec<ResponsibilityRow>, String> {
        let conn = self.conn.lock().await;
        let ids: Vec<String> = {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT DISTINCT o.responsibility_id FROM responsibility_occurrences o \
                     WHERE o.task_id IN (WITH RECURSIVE anc(id, depth) AS ( \
                       SELECT ?1, 0 UNION ALL SELECT t.parent_task_id, anc.depth + 1 FROM tasks t \
                       JOIN anc ON t.id = anc.id WHERE t.parent_task_id IS NOT NULL \
                         AND anc.depth < {STOP_ANCESTRY_DEPTH}) SELECT id FROM anc)"
                ))
                .map_err(|e| format!("occurrence tree: {e}"))?;
            stmt.query_map(params![task_id], |r| r.get(0))
                .map_err(|e| format!("occurrence tree: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("occurrence tree: {e}"))?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(
                get_responsibility_conn(&conn, &id)?
                    .ok_or("occurrence without responsibility")?,
            );
        }
        Ok(out)
    }

    pub async fn list_occurrences(
        &self,
        responsibility_id: &str,
    ) -> Result<Vec<OccurrenceRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {OCC_COLUMNS} FROM responsibility_occurrences WHERE responsibility_id = ?1
                 ORDER BY created_at DESC, task_id DESC"
            ))
            .map_err(|e| format!("list occurrences: {e}"))?;
        stmt.query_map(params![responsibility_id], row_to_occurrence)
            .map_err(|e| format!("list occurrences: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list occurrences: {e}"))
    }

    /// Open occurrences joined with their task's current status (`None` when
    /// the task row is gone).
    pub async fn open_occurrences(&self) -> Result<Vec<(OccurrenceRow, Option<String>)>, String> {
        let conn = self.conn.lock().await;
        let cols = OCC_COLUMNS
            .split(", ")
            .map(|c| format!("o.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {cols}, t.status FROM responsibility_occurrences o
                   LEFT JOIN tasks t ON t.id = o.task_id
                  WHERE o.outcome IS NULL ORDER BY o.created_at"
            ))
            .map_err(|e| format!("open occurrences: {e}"))?;
        stmt.query_map([], |r| {
            Ok((row_to_occurrence(r)?, r.get::<_, Option<String>>(13)?))
        })
        .map_err(|e| format!("open occurrences: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("open occurrences: {e}"))
    }

    /// Settle one open occurrence (CAS on `outcome IS NULL`). `outcome` is one
    /// of done / failed / cancelled / stopped / stopped_counted / blocked. Updates the failure streak and
    /// parks the responsibility `failure_paused` when the streak reaches its
    /// limit. Returns the responsibility row after settlement, or `None` when
    /// the occurrence was already settled.
    pub async fn settle_occurrence(
        &self,
        task_id: &str,
        outcome: &str,
        charged_cents: i64,
        cost_basis: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<ResponsibilityRow>, String> {
        if !matches!(
            outcome,
            "done" | "failed" | "cancelled" | "stopped" | "stopped_counted" | "blocked"
        ) {
            return Err(format!("invalid occurrence outcome: {outcome}"));
        }
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("settle occurrence: begin: {e}"))?;
        let resp_id: Option<String> = tx
            .query_row(
                "SELECT responsibility_id FROM responsibility_occurrences
                  WHERE task_id=?1 AND outcome IS NULL",
                params![task_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("settle occurrence: load: {e}"))?;
        let Some(resp_id) = resp_id else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE responsibility_occurrences SET outcome=?2, charged_cents=?3, cost_basis=?4,
                    settled_at=?5 WHERE task_id=?1 AND outcome IS NULL",
            params![task_id, outcome, charged_cents, cost_basis, now_s],
        )
        .map_err(|e| format!("settle occurrence: {e}"))?;
        match outcome {
            "done" => {
                tx.execute(
                    "UPDATE responsibilities SET consecutive_failures=0, updated_at=?2
                      WHERE responsibility_id=?1",
                    params![resp_id, now_s],
                )
                .map_err(|e| format!("settle occurrence: streak reset: {e}"))?;
            }
            // `stopped_counted`: stopped, but not by a manager; `blocked`: the
            // employee parked the run (E-M3). Both count as unsuccessful.
            "failed" | "cancelled" | "stopped_counted" | "blocked" => {
                tx.execute(
                    "UPDATE responsibilities SET consecutive_failures=consecutive_failures+1,
                            updated_at=?2 WHERE responsibility_id=?1",
                    params![resp_id, now_s],
                )
                .map_err(|e| format!("settle occurrence: streak: {e}"))?;
                tx.execute(
                    "UPDATE responsibilities SET state='failure_paused',
                            state_reason='consecutive_failures', state_changed_by='system',
                            state_changed_at=?2, updated_at=?2
                      WHERE responsibility_id=?1 AND state IN ('active','paused')
                        AND consecutive_failures >= max_consecutive_failures",
                    params![resp_id, now_s],
                )
                .map_err(|e| format!("settle occurrence: failure pause: {e}"))?;
            }
            // Operator-stopped: not the employee's failure, streak untouched.
            _ => {}
        }
        let row = get_responsibility_conn(&tx, &resp_id)?;
        tx.commit()
            .map_err(|e| format!("settle occurrence: commit: {e}"))?;
        Ok(row)
    }

    /// The most recently settled occurrence's task id (predecessor of the next).
    pub async fn last_occurrence_task(
        &self,
        responsibility_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT task_id FROM responsibility_occurrences WHERE responsibility_id = ?1
              ORDER BY created_at DESC, task_id DESC LIMIT 1",
            params![responsibility_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("last occurrence: {e}"))
    }
}

pub(super) fn period_usage_conn(
    conn: &Connection,
    responsibility_id: &str,
    period_key: &str,
) -> Result<PeriodUsage, String> {
    let mut stmt = conn
        .prepare(
            "SELECT task_id, reserved_cents, charged_cents, outcome FROM responsibility_occurrences
              WHERE responsibility_id = ?1 AND period_key = ?2",
        )
        .map_err(|e| format!("period usage: {e}"))?;
    let rows = stmt
        .query_map(params![responsibility_id, period_key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|e| format!("period usage: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("period usage: {e}"))?;
    let mut usage = PeriodUsage::default();
    for (task_id, reserved, charged, outcome) in rows {
        usage.occurrences += 1;
        let floor = match outcome {
            None => reserved,
            Some(_) => charged.unwrap_or(reserved),
        };
        usage.stored_cents = usage.stored_cents.saturating_add(floor);
        usage.entries.push((task_id, floor));
    }
    Ok(usage)
}
