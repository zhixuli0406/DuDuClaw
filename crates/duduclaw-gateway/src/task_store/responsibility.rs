//! Responsibility rows, subscriptions (`task_wakeups`), wake facts
//! (`wakeup_fires`) and the event cursor. Every state change is a CAS on
//! `control_epoch` / `state` / `contract_revision` inside one `BEGIN
//! IMMEDIATE`, so concurrent writers serialize on the SQLite write lock.

use super::*;

/// Contract fields an operator may change through `update_contract`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsibilityContract {
    pub objective: String,
    pub acceptance_template: String,
    pub scope_json: String,
    pub source_refs_json: String,
    pub notification_policy_json: String,
    pub schedule_json: Option<String>,
    pub occurrence_hours: i64,
    pub occurrence_cost_cap_cents: i64,
    pub budget_period: String,
    pub budget_timezone: String,
    pub period_cost_limit_cents: i64,
    pub period_occurrence_limit: i64,
    pub min_wake_interval_secs: i64,
    pub max_consecutive_failures: i64,
    pub stop_at: String,
    pub contract_hash: String,
}

fn get_resp_conn(conn: &Connection, id: &str) -> Result<Option<ResponsibilityRow>, String> {
    conn.query_row(
        &format!("SELECT {RESP_COLUMNS} FROM responsibilities WHERE responsibility_id = ?1"),
        params![id],
        row_to_responsibility,
    )
    .optional()
    .map_err(|e| format!("get responsibility: {e}"))
}

pub(super) fn insert_wakeup_conn(conn: &Connection, w: &WakeupRow) -> Result<(), String> {
    conn.execute(
        &format!("INSERT INTO task_wakeups ({WAKEUP_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"),
        params![
            w.wakeup_id,
            w.responsibility_id,
            w.control_epoch,
            w.kind,
            w.recurring as i64,
            w.due_at,
            w.event_name,
            w.event_filter_json,
            w.approval_id,
            w.armed_by,
            w.state,
            w.created_at,
            w.updated_at,
            w.armed_after_event_id
        ],
    )
    .map_err(|e| format!("insert wakeup: {e}"))?;
    Ok(())
}

/// The operator/schedule subscriptions disarmed most recently (the newest
/// epoch that has any): what `enable` re-arms with their original filters and
/// timeouts. Agent-armed one-shots are never revived.
fn disarmed_subscriptions_conn(conn: &Connection, id: &str) -> Result<Vec<WakeupRow>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {WAKEUP_COLUMNS} FROM task_wakeups
              WHERE responsibility_id = ?1 AND state = 'cancelled' AND armed_by NOT LIKE 'agent:%'
                AND control_epoch = (SELECT MAX(control_epoch) FROM task_wakeups
                                      WHERE responsibility_id = ?1 AND armed_by NOT LIKE 'agent:%')
              ORDER BY created_at, wakeup_id"
        ))
        .map_err(|e| format!("disarmed subscriptions: {e}"))?;
    stmt.query_map(params![id], row_to_wakeup)
        .map_err(|e| format!("disarmed subscriptions: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("disarmed subscriptions: {e}"))
}

/// Cancel every armed subscription and drop every pending fact of one
/// responsibility (epoch change). Shared by disable / contract resubscribe.
fn retire_subscriptions_conn(
    conn: &Connection,
    id: &str,
    wakeup_state: &str,
    drop_reason: &str,
    now: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE task_wakeups SET state = ?2, updated_at = ?3
          WHERE responsibility_id = ?1 AND state = 'armed'",
        params![id, wakeup_state, now],
    )
    .map_err(|e| format!("retire wakeups: {e}"))?;
    conn.execute(
        "UPDATE wakeup_fires SET state = 'dropped', drop_reason = ?2, settled_at = ?3
          WHERE responsibility_id = ?1 AND state = 'pending'",
        params![id, drop_reason, now],
    )
    .map_err(|e| format!("drop pending fires: {e}"))?;
    Ok(())
}

impl TaskStore {
    /// Insert a new responsibility and its initial subscriptions atomically.
    /// `init_event_cursor` seeds the global event cursor when it does not
    /// exist yet (first event subscription: no history is replayed).
    pub async fn insert_responsibility(
        &self,
        row: &ResponsibilityRow,
        wakeups: &[WakeupRow],
        init_event_cursor: Option<i64>,
    ) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("insert responsibility: begin: {e}"))?;
        tx.execute(
            &format!("INSERT INTO responsibilities ({RESP_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29)"),
            params![
                row.responsibility_id, row.owner_agent_id, row.created_by, row.objective,
                row.acceptance_template, row.scope_json, row.source_refs_json,
                row.notification_policy_json, row.schedule_json, row.occurrence_hours,
                row.occurrence_cost_cap_cents, row.budget_period, row.budget_timezone,
                row.period_cost_limit_cents, row.period_occurrence_limit, row.min_wake_interval_secs,
                row.max_consecutive_failures, row.stop_at, row.state, row.state_reason,
                row.state_changed_by, row.state_changed_at, row.contract_revision,
                row.contract_hash, row.control_epoch, row.consecutive_failures,
                row.last_occurrence_at, row.created_at, row.updated_at
            ],
        )
        .map_err(|e| format!("insert responsibility: {e}"))?;
        for w in wakeups {
            insert_wakeup_conn(&tx, w)?;
        }
        if let Some(cursor) = init_event_cursor {
            tx.execute(
                "INSERT OR IGNORE INTO responsibility_event_cursor (singleton, last_event_id, updated_at)
                 VALUES (1, ?1, ?2)",
                params![cursor, row.created_at],
            )
            .map_err(|e| format!("init event cursor: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("insert responsibility: commit: {e}"))
    }

    pub async fn get_responsibility(&self, id: &str) -> Result<Option<ResponsibilityRow>, String> {
        let conn = self.conn.lock().await;
        get_resp_conn(&conn, id)
    }

    /// All responsibilities, optionally narrowed to one owner, newest first.
    pub async fn list_responsibilities(
        &self,
        owner: Option<&str>,
    ) -> Result<Vec<ResponsibilityRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {RESP_COLUMNS} FROM responsibilities
                  WHERE (?1 IS NULL OR owner_agent_id = ?1) ORDER BY created_at DESC"
            ))
            .map_err(|e| format!("list responsibilities: {e}"))?;
        stmt.query_map(params![owner], row_to_responsibility)
            .map_err(|e| format!("list responsibilities: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list responsibilities: {e}"))
    }

    /// Generic single-statement CAS used by pause / resume / clear-failures.
    async fn resp_state_cas(
        &self,
        id: &str,
        sql: &str,
        binds: &[&(dyn rusqlite::types::ToSql + Sync)],
    ) -> Result<RespCas, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("responsibility cas: begin: {e}"))?;
        let n = tx
            .execute(sql, rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| format!("responsibility cas: {e}"))?;
        let row = get_resp_conn(&tx, id)?;
        tx.commit()
            .map_err(|e| format!("responsibility cas: commit: {e}"))?;
        Ok(match (n, row) {
            (1, Some(r)) => RespCas::Applied(r),
            (_, r) => RespCas::Conflict(r),
        })
    }

    /// `active → paused` (operator "pause coordination"). Epoch unchanged.
    pub async fn pause_responsibility(
        &self,
        id: &str,
        expected_epoch: i64,
        actor: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<RespCas, String> {
        let now = resp_ts(now);
        self.resp_state_cas(
            id,
            "UPDATE responsibilities SET state='paused', state_reason=?3, state_changed_by=?4,
                    state_changed_at=?5, updated_at=?5
              WHERE responsibility_id=?1 AND state='active' AND control_epoch=?2",
            &[&id, &expected_epoch, &reason, &actor, &now],
        )
        .await
    }

    /// `paused → active`. Epoch unchanged; pending facts coalesce on the next wake.
    pub async fn resume_responsibility(
        &self,
        id: &str,
        expected_epoch: i64,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<RespCas, String> {
        let now = resp_ts(now);
        self.resp_state_cas(
            id,
            "UPDATE responsibilities SET state='active', state_reason=NULL, state_changed_by=?3,
                    state_changed_at=?4, updated_at=?4
              WHERE responsibility_id=?1 AND state='paused' AND control_epoch=?2",
            &[&id, &expected_epoch, &actor, &now],
        )
        .await
    }

    /// `failure_paused → active`, failure streak reset.
    pub async fn clear_responsibility_failures(
        &self,
        id: &str,
        expected_epoch: i64,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<RespCas, String> {
        let now = resp_ts(now);
        self.resp_state_cas(
            id,
            "UPDATE responsibilities SET state='active', consecutive_failures=0, state_reason=NULL,
                    state_changed_by=?3, state_changed_at=?4, updated_at=?4
              WHERE responsibility_id=?1 AND state='failure_paused' AND control_epoch=?2",
            &[&id, &expected_epoch, &actor, &now],
        )
        .await
    }

    /// "Disable future schedule": epoch +1, armed subscriptions cancelled,
    /// pending facts dropped (`disabled`). The running occurrence is untouched.
    pub async fn disable_responsibility(
        &self,
        id: &str,
        expected_epoch: i64,
        actor: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<RespCas, String> {
        let now = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("disable responsibility: begin: {e}"))?;
        let n = tx
            .execute(
                "UPDATE responsibilities SET state='disabled', control_epoch=control_epoch+1,
                        state_reason=?3, state_changed_by=?4, state_changed_at=?5, updated_at=?5
                  WHERE responsibility_id=?1 AND control_epoch=?2
                    AND state IN ('active','paused','budget_paused','failure_paused')",
                params![id, expected_epoch, reason, actor, now],
            )
            .map_err(|e| format!("disable responsibility: {e}"))?;
        if n == 1 {
            retire_subscriptions_conn(&tx, id, "cancelled", "disabled", &now)?;
        }
        let row = get_resp_conn(&tx, id)?;
        tx.commit()
            .map_err(|e| format!("disable responsibility: commit: {e}"))?;
        Ok(match (n, row) {
            (1, Some(r)) => RespCas::Applied(r),
            (_, r) => RespCas::Conflict(r),
        })
    }

    /// `disabled → active`, epoch +1, fresh subscriptions built by `make`
    /// for the new epoch. Missed slots are not replayed.
    pub async fn enable_responsibility(
        &self,
        id: &str,
        expected_epoch: i64,
        actor: &str,
        now: DateTime<Utc>,
        make: impl FnOnce(&ResponsibilityRow, &[WakeupRow]) -> Result<Vec<WakeupRow>, String>,
    ) -> Result<RespCas, String> {
        let now = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("enable responsibility: begin: {e}"))?;
        let n = tx
            .execute(
                // Third review LOW: re-enabling keeps the failure streak; only
                // `clear_failures` (Manager) resets it. A streak at the limit
                // comes back as `failure_paused`, not `active`.
                "UPDATE responsibilities SET
                        state = CASE WHEN consecutive_failures >= max_consecutive_failures
                                     THEN 'failure_paused' ELSE 'active' END,
                        state_reason = CASE WHEN consecutive_failures >= max_consecutive_failures
                                     THEN 'consecutive_failures' ELSE NULL END,
                        control_epoch=control_epoch+1, state_changed_by=?3,
                        state_changed_at=?4, updated_at=?4
                  WHERE responsibility_id=?1 AND state='disabled' AND control_epoch=?2",
                params![id, expected_epoch, actor, now],
            )
            .map_err(|e| format!("enable responsibility: {e}"))?;
        let row = get_resp_conn(&tx, id)?;
        if n == 1 {
            let fresh = row.as_ref().ok_or("enabled responsibility vanished")?;
            let prior = disarmed_subscriptions_conn(&tx, id)?;
            for w in make(fresh, &prior)? {
                insert_wakeup_conn(&tx, &w)?;
            }
        }
        tx.commit()
            .map_err(|e| format!("enable responsibility: commit: {e}"))?;
        Ok(match (n, row) {
            (1, Some(r)) => RespCas::Applied(r),
            (_, r) => RespCas::Conflict(r),
        })
    }

    /// Operator contract update (CAS on `contract_revision`). When `resubscribe`
    /// is `Some`, the schedule/event subscriptions changed: epoch +1, old
    /// subscriptions cancelled, pending facts dropped (`epoch_changed`), and
    /// the new subscriptions armed — all in this one transaction. A running
    /// occurrence keeps the contract it was created with.
    pub async fn update_responsibility_contract(
        &self,
        id: &str,
        expected_contract_revision: i64,
        contract: &ResponsibilityContract,
        resubscribe: Option<&(dyn Fn(i64) -> Result<Vec<WakeupRow>, String> + Sync)>,
        now: DateTime<Utc>,
    ) -> Result<RespCas, String> {
        let now = resp_ts(now);
        let epoch_bump: i64 = resubscribe.is_some() as i64;
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("update contract: begin: {e}"))?;
        let c = contract;
        let n = tx
            .execute(
                "UPDATE responsibilities SET objective=?3, acceptance_template=?4, scope_json=?5,
                        source_refs_json=?6, notification_policy_json=?7, schedule_json=?8,
                        occurrence_hours=?9, occurrence_cost_cap_cents=?10, budget_period=?11,
                        budget_timezone=?12, period_cost_limit_cents=?13, period_occurrence_limit=?14,
                        min_wake_interval_secs=?15, max_consecutive_failures=?16, stop_at=?17,
                        contract_hash=?18, contract_revision=contract_revision+1,
                        control_epoch=control_epoch+?19, updated_at=?20
                  WHERE responsibility_id=?1 AND contract_revision=?2 AND state<>'expired'",
                params![
                    id,
                    expected_contract_revision,
                    c.objective,
                    c.acceptance_template,
                    c.scope_json,
                    c.source_refs_json,
                    c.notification_policy_json,
                    c.schedule_json,
                    c.occurrence_hours,
                    c.occurrence_cost_cap_cents,
                    c.budget_period,
                    c.budget_timezone,
                    c.period_cost_limit_cents,
                    c.period_occurrence_limit,
                    c.min_wake_interval_secs,
                    c.max_consecutive_failures,
                    c.stop_at,
                    c.contract_hash,
                    epoch_bump,
                    now
                ],
            )
            .map_err(|e| format!("update contract: {e}"))?;
        let row = get_resp_conn(&tx, id)?;
        if n == 1 {
            if let Some(make) = resubscribe {
                retire_subscriptions_conn(&tx, id, "cancelled", "epoch_changed", &now)?;
                let epoch = row
                    .as_ref()
                    .map(|r| r.control_epoch)
                    .ok_or("row vanished")?;
                // A disabled responsibility keeps its new subscriptions
                // disarmed; `enable` re-arms exactly these rows later.
                let disabled = row.as_ref().is_some_and(|r| r.state == "disabled");
                for mut w in make(epoch)? {
                    if disabled {
                        w.state = "cancelled".into();
                    }
                    insert_wakeup_conn(&tx, &w)?;
                }
            }
        }
        tx.commit()
            .map_err(|e| format!("update contract: commit: {e}"))?;
        Ok(match (n, row) {
            (1, Some(r)) => RespCas::Applied(r),
            (_, r) => RespCas::Conflict(r),
        })
    }

    /// Every non-expired responsibility whose `stop_at` has passed becomes
    /// `expired` (terminal): epoch +1, subscriptions expired, pending facts
    /// dropped (`stop_at`). Returns the ids that expired in this call.
    pub async fn expire_due_responsibilities(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ResponsibilityRow>, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("expire responsibilities: begin: {e}"))?;
        let due: Vec<ResponsibilityRow> = {
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT {RESP_COLUMNS} FROM responsibilities WHERE state <> 'expired'"
                ))
                .map_err(|e| format!("expire scan: {e}"))?;
            stmt.query_map([], row_to_responsibility)
                .map_err(|e| format!("expire scan: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("expire scan: {e}"))?
                .into_iter()
                // A malformed stop_at counts as already passed (fail closed).
                .filter(|r| parse_ts(&r.stop_at).is_none_or(|t| t <= now))
                .collect()
        };
        let mut expired = Vec::new();
        for r in due {
            let n = tx
                .execute(
                    "UPDATE responsibilities SET state='expired', control_epoch=control_epoch+1,
                            state_reason='stop_at', state_changed_by='system', state_changed_at=?2,
                            updated_at=?2
                      WHERE responsibility_id=?1 AND state<>'expired'",
                    params![r.responsibility_id, now_s],
                )
                .map_err(|e| format!("expire responsibility: {e}"))?;
            if n == 1 {
                retire_subscriptions_conn(&tx, &r.responsibility_id, "expired", "stop_at", &now_s)?;
                expired.push(r);
            }
        }
        tx.commit()
            .map_err(|e| format!("expire responsibilities: commit: {e}"))?;
        Ok(expired)
    }

    /// `active → budget_paused` with the window that ran out recorded in
    /// `state_reason` (`budget:<period_key>`), so a later window can resume it.
    pub async fn mark_budget_paused(
        &self,
        id: &str,
        period_key: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE responsibilities SET state='budget_paused', state_reason=?2,
                        state_changed_by='system', state_changed_at=?3, updated_at=?3
                  WHERE responsibility_id=?1 AND state='active'",
                params![id, format!("budget:{period_key}"), resp_ts(now)],
            )
            .map_err(|e| format!("budget pause: {e}"))?;
        Ok(n == 1)
    }

    /// `budget_paused → active` once the current window differs from the one
    /// recorded at pause time.
    pub async fn resume_budget_window(
        &self,
        id: &str,
        current_period_key: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE responsibilities SET state='active', state_reason=NULL,
                        state_changed_by='system', state_changed_at=?3, updated_at=?3
                  WHERE responsibility_id=?1 AND state='budget_paused'
                    AND COALESCE(state_reason,'') <> ?2",
                params![id, format!("budget:{current_period_key}"), resp_ts(now)],
            )
            .map_err(|e| format!("budget resume: {e}"))?;
        Ok(n == 1)
    }

    /// Armed subscriptions of one kind whose owning responsibility is live
    /// (not expired) and still on the subscription's epoch.
    pub async fn armed_wakeups(&self, kind: &str) -> Result<Vec<WakeupRow>, String> {
        let conn = self.conn.lock().await;
        let cols = WAKEUP_COLUMNS
            .split(", ")
            .map(|c| format!("w.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {cols} FROM task_wakeups w JOIN responsibilities r
                   ON r.responsibility_id = w.responsibility_id
                 WHERE w.state = 'armed' AND w.kind = ?1 AND w.control_epoch = r.control_epoch
                   AND r.state <> 'expired'
                 ORDER BY w.created_at, w.wakeup_id"
            ))
            .map_err(|e| format!("armed wakeups: {e}"))?;
        stmt.query_map(params![kind], row_to_wakeup)
            .map_err(|e| format!("armed wakeups: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("armed wakeups: {e}"))
    }

    pub async fn list_wakeups(&self, responsibility_id: &str) -> Result<Vec<WakeupRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {WAKEUP_COLUMNS} FROM task_wakeups WHERE responsibility_id = ?1
                 ORDER BY created_at, wakeup_id"
            ))
            .map_err(|e| format!("list wakeups: {e}"))?;
        stmt.query_map(params![responsibility_id], row_to_wakeup)
            .map_err(|e| format!("list wakeups: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list wakeups: {e}"))
    }

    /// Record one wake fact. Writes nothing when the subscription is no longer
    /// armed on its responsibility's current epoch, or the responsibility has
    /// expired; a repeated `(responsibility, fire_key)` is ignored. When
    /// `advance` is `Some((old_due, new_due))` the recurring subscription's
    /// `due_at` moves forward in the same transaction (CAS on the old value).
    /// Returns whether a new fact was written.
    pub async fn record_fire(
        &self,
        fire: &NewFire,
        advance: Option<(&str, &str)>,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("record fire: begin: {e}"))?;
        // S-L6: settled facts older than the retention are pruned; past the
        // pending cap a new fact is kept only as a dropped row without data.
        let resp_id: Option<String> = tx
            .query_row(
                "SELECT responsibility_id FROM task_wakeups WHERE wakeup_id = ?1",
                params![fire.wakeup_id],
                |r| r.get(0),
            )
            .ok();
        let mut over_cap = false;
        if let Some(rid) = &resp_id {
            let cutoff = resp_ts(now - chrono::Duration::days(FIRE_RETENTION_DAYS));
            tx.execute(
                "DELETE FROM wakeup_fires WHERE responsibility_id = ?1 AND state <> 'pending'
                   AND settled_at IS NOT NULL AND settled_at < ?2",
                params![rid, cutoff],
            )
            .map_err(|e| format!("record fire: prune: {e}"))?;
            let pending: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM wakeup_fires WHERE responsibility_id = ?1 AND state = 'pending'",
                    params![rid],
                    |r| r.get(0),
                )
                .map_err(|e| format!("record fire: count: {e}"))?;
            over_cap = fire.dropped.is_none() && pending >= MAX_PENDING_FIRES;
        }
        let (state, drop_reason, settled): (&str, Option<&str>, Option<&str>) = match &fire.dropped
        {
            Some(reason) => ("dropped", Some(reason.as_str()), Some(now_s.as_str())),
            None if over_cap => ("dropped", Some("pending_cap"), Some(now_s.as_str())),
            None => ("pending", None, None),
        };
        let (data_json, guard_flags_json) = if over_cap {
            (None, None)
        } else {
            (fire.data_json.as_deref(), fire.guard_flags_json.as_deref())
        };
        let n = tx
            .execute(
                "INSERT INTO wakeup_fires (fire_id, wakeup_id, responsibility_id, control_epoch,
                        fire_key, reason, data_json, guard_flags_json, state, drop_reason,
                        occurrence_task_id, observed_at, settled_at)
                 SELECT ?1, w.wakeup_id, w.responsibility_id, w.control_epoch, ?3, ?4, ?5, ?6, ?7,
                        ?8, NULL, ?9, ?10
                   FROM task_wakeups w JOIN responsibilities r
                     ON r.responsibility_id = w.responsibility_id
                  WHERE w.wakeup_id = ?2 AND w.state = 'armed'
                    AND w.control_epoch = r.control_epoch AND r.state <> 'expired'
                 ON CONFLICT(responsibility_id, fire_key) DO NOTHING",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    fire.wakeup_id,
                    fire.fire_key,
                    fire.reason,
                    data_json,
                    guard_flags_json,
                    state,
                    drop_reason,
                    now_s,
                    settled
                ],
            )
            .map_err(|e| format!("record fire: {e}"))?;
        if let Some((old_due, new_due)) = advance {
            tx.execute(
                "UPDATE task_wakeups SET due_at = ?3, updated_at = ?4
                  WHERE wakeup_id = ?1 AND state = 'armed' AND due_at = ?2",
                params![fire.wakeup_id, old_due, new_due, now_s],
            )
            .map_err(|e| format!("advance wakeup: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("record fire: commit: {e}"))?;
        Ok(n == 1)
    }

    /// Move a recurring subscription's `due_at` without recording a fact
    /// (the slot it pointed at fell outside the 24-hour catch-up window).
    pub async fn advance_wakeup_due(
        &self,
        wakeup_id: &str,
        old_due: &str,
        new_due: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE task_wakeups SET due_at = ?3, updated_at = ?4
                  WHERE wakeup_id = ?1 AND state = 'armed' AND due_at = ?2",
                params![wakeup_id, old_due, new_due, resp_ts(now)],
            )
            .map_err(|e| format!("advance wakeup: {e}"))?;
        Ok(n == 1)
    }

    /// Whether any fact (any state) has been recorded for a subscription.
    pub async fn wakeup_has_fire(&self, wakeup_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wakeup_fires WHERE wakeup_id = ?1)",
            params![wakeup_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("wakeup has fire: {e}"))
    }

    /// Arm one subscription for a live responsibility on its current epoch.
    /// `agent_limit` caps agent-armed live subscriptions per responsibility
    /// (D4); exceeding it returns `Err("agent_followup_limit")`.
    pub async fn arm_wakeup(
        &self,
        wakeup: &WakeupRow,
        agent_limit: Option<i64>,
    ) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("arm wakeup: begin: {e}"))?;
        let resp =
            get_resp_conn(&tx, &wakeup.responsibility_id)?.ok_or("responsibility_not_found")?;
        if resp.control_epoch != wakeup.control_epoch || resp.state == "expired" {
            return Err("epoch_changed".into());
        }
        if let Some(limit) = agent_limit {
            let live: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM task_wakeups WHERE responsibility_id = ?1
                       AND state = 'armed' AND armed_by LIKE 'agent:%'",
                    params![wakeup.responsibility_id],
                    |r| r.get(0),
                )
                .map_err(|e| format!("arm wakeup: count: {e}"))?;
            if live >= limit {
                return Err("agent_followup_limit".into());
            }
        }
        insert_wakeup_conn(&tx, wakeup)?;
        tx.commit().map_err(|e| format!("arm wakeup: commit: {e}"))
    }

    /// Facts of one responsibility, newest first (debug / dashboard).
    pub async fn list_fires(
        &self,
        responsibility_id: &str,
        limit: usize,
    ) -> Result<Vec<FireRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {FIRE_COLUMNS} FROM wakeup_fires WHERE responsibility_id = ?1
                 ORDER BY observed_at DESC, fire_id DESC LIMIT ?2"
            ))
            .map_err(|e| format!("list fires: {e}"))?;
        stmt.query_map(
            params![responsibility_id, limit.clamp(1, 1000) as i64],
            row_to_fire,
        )
        .map_err(|e| format!("list fires: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("list fires: {e}"))
    }

    /// Pending facts of one responsibility, oldest first.
    pub async fn pending_fires(&self, responsibility_id: &str) -> Result<Vec<FireRow>, String> {
        let conn = self.conn.lock().await;
        pending_fires_conn(&conn, responsibility_id)
    }

    /// Responsibility ids that currently have at least one pending fact.
    pub async fn responsibilities_with_pending_fires(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT responsibility_id FROM wakeup_fires WHERE state = 'pending'
                 ORDER BY responsibility_id",
            )
            .map_err(|e| format!("pending fire scan: {e}"))?;
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("pending fire scan: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("pending fire scan: {e}"))
    }
}

pub(super) fn pending_fires_conn(conn: &Connection, id: &str) -> Result<Vec<FireRow>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {FIRE_COLUMNS} FROM wakeup_fires WHERE responsibility_id = ?1 AND state = 'pending'
             ORDER BY observed_at, fire_id"
        ))
        .map_err(|e| format!("pending fires: {e}"))?;
    stmt.query_map(params![id], row_to_fire)
        .map_err(|e| format!("pending fires: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("pending fires: {e}"))
}

pub(super) fn get_responsibility_conn(
    conn: &Connection,
    id: &str,
) -> Result<Option<ResponsibilityRow>, String> {
    get_resp_conn(conn, id)
}
