//! In-flight steering (`task_steering`) and durable dispatch intents
//! (`task_dispatch_intents`). Steering never touches the `tasks` row, so the
//! frozen acceptance baseline and `authority_revision` cannot move because of
//! it. "applied" means "placed in the payload of round N that was handed to
//! the employee" — never "the employee adopted it".

use super::responsibility::get_responsibility_conn;
use super::*;

/// Result of a steering submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteeringSubmit {
    /// A new entry was recorded.
    Created(SteeringRow),
    /// The same `client_request_id` was sent before; the original entry.
    Duplicate(SteeringRow),
}

/// Input for a steering submission (body already scanned by the caller).
#[derive(Debug, Clone)]
pub struct NewSteering<'a> {
    pub task_id: &'a str,
    pub body: &'a str,
    pub body_hash: &'a str,
    pub guard_flags_json: &'a str,
    pub submitted_by: &'a str,
    pub client_request_id: &'a str,
}

/// Outcome of opening a durable dispatch intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentBegin {
    /// The intent is `intended`; these steering entries are now `delivering`
    /// with it and must go into this round's payload.
    Ready {
        intent: DispatchIntentRow,
        steering: Vec<SteeringRow>,
    },
    /// Nothing was written; the reason is a closed token.
    Refused(&'static str),
}

/// What a task needs from the durable path at dispatch time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableInfo {
    /// `(occurrence, responsibility)` when the task is an occurrence.
    pub occurrence: Option<(OccurrenceRow, ResponsibilityRow)>,
    /// Pending or delivering steering exists.
    pub open_steering: bool,
    /// Non-abandoned intents (the persistent iteration count).
    pub persisted_iters: i64,
    /// An `intended` intent left over (crash or failed enqueue).
    pub intended: Option<DispatchIntentRow>,
    /// The latest `enqueued` intent.
    pub last_enqueued: Option<DispatchIntentRow>,
}

impl DurableInfo {
    /// The round must go through the durable path (D9).
    pub fn is_durable(&self) -> bool {
        self.occurrence.is_some() || self.open_steering || self.intended.is_some()
    }
}

/// The same set, for comparisons in Rust (no string matching).
const CANDIDATE_STATUS_LIST: [&str; 3] = ["todo", "pending", "revising"];

/// Responsibility states in which a *running* occurrence may get its next
/// round (they only affect the next occurrence). `paused` freezes it;
/// `expired` (or a missing row) ends it.
const OCCURRENCE_RUNNABLE_STATES: [&str; 4] =
    ["active", "budget_paused", "failure_paused", "disabled"];

/// Shared by the intent transaction and the dispatcher fence. `Ok(Ok(()))`
/// for a task that is not an occurrence, or an occurrence that is still
/// open, carries its responsibility's epoch lineage (never ahead of it), and
/// whose responsibility is in a runnable state. `disable` moves the epoch on
/// purpose without stopping the running occurrence (§6.3), so the
/// occurrence's own epoch is only checked for lineage, not equality.
fn occurrence_gate_conn(
    conn: &Connection,
    task_id: &str,
) -> Result<Result<(), &'static str>, String> {
    type GateRow = (
        Option<String>,
        i64,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<GateRow> = conn
        .query_row(
            "SELECT o.outcome, o.control_epoch, r.control_epoch, r.state, r.owner_agent_id,
                    (SELECT assigned_to FROM tasks WHERE id = o.task_id)
               FROM responsibility_occurrences o
               LEFT JOIN responsibilities r ON r.responsibility_id = o.responsibility_id
              WHERE o.task_id = ?1",
            params![task_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("occurrence gate: {e}"))?;
    let Some((outcome, occ_epoch, resp_epoch, state, owner, assignee)) = row else {
        return Ok(Ok(()));
    };
    let Some(state) = state else {
        return Ok(Err("responsibility_missing"));
    };
    // E-M5: an occurrence always runs as the responsibility's owner.
    if owner.is_none() || owner != assignee {
        return Ok(Err("occurrence_reassigned"));
    }
    if outcome.is_some() {
        return Ok(Err("occurrence_settled"));
    }
    if resp_epoch.is_none_or(|e| occ_epoch > e) {
        return Ok(Err("epoch_mismatch"));
    }
    if state == "paused" {
        return Ok(Err("responsibility_paused"));
    }
    if !OCCURRENCE_RUNNABLE_STATES.contains(&state.as_str()) {
        return Ok(Err("responsibility_not_runnable"));
    }
    Ok(Ok(()))
}

fn steering_for_task_conn(
    conn: &Connection,
    task_id: &str,
    states: &[&str],
) -> Result<Vec<SteeringRow>, String> {
    let states_json = serde_json::to_string(states).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {STEERING_COLUMNS} FROM task_steering
              WHERE task_id = ?1 AND state IN (SELECT value FROM json_each(?2))
             ORDER BY seq"
        ))
        .map_err(|e| format!("steering for task: {e}"))?;
    stmt.query_map(params![task_id, states_json], row_to_steering)
        .map_err(|e| format!("steering for task: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("steering for task: {e}"))
}

fn intent_conn(
    conn: &Connection,
    task_id: &str,
    state: &str,
) -> Result<Option<DispatchIntentRow>, String> {
    conn.query_row(
        &format!(
            "SELECT {INTENT_COLUMNS} FROM task_dispatch_intents WHERE task_id = ?1 AND state = ?2
             ORDER BY iter DESC LIMIT 1"
        ),
        params![task_id, state],
        row_to_intent,
    )
    .optional()
    .map_err(|e| format!("dispatch intent: {e}"))
}

impl TaskStore {
    /// Record one steering entry. Refused (`Err` with a closed token) when the
    /// task is missing, not a goal, finished, being stopped, or already has
    /// [`STEERING_OPEN_LIMIT`] open entries. A repeated `client_request_id`
    /// returns the original entry (same `seq`, nothing new written).
    pub async fn submit_steering(
        &self,
        new: &NewSteering<'_>,
        now: DateTime<Utc>,
    ) -> Result<SteeringSubmit, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("submit steering: begin: {e}"))?;
        if let Some(existing) = tx
            .query_row(
                &format!(
                    "SELECT {STEERING_COLUMNS} FROM task_steering
                      WHERE task_id = ?1 AND client_request_id = ?2"
                ),
                params![new.task_id, new.client_request_id],
                row_to_steering,
            )
            .optional()
            .map_err(|e| format!("submit steering: dedup: {e}"))?
        {
            return Ok(SteeringSubmit::Duplicate(existing));
        }
        let task: Option<(bool, String, i64, bool)> = tx
            .query_row(
                &format!(
                    "SELECT COALESCE(goal_mode,0), status, authority_revision, {}
                       FROM tasks WHERE id = ?1 AND kind IN ('task','goal')",
                    in_stop_tree_sql("?1")
                ),
                params![new.task_id],
                |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| format!("submit steering: task: {e}"))?;
        let Some((goal_mode, status, authority_revision, stopped)) = task else {
            return Err("task_not_found".into());
        };
        if !goal_mode {
            return Err("not_a_goal_task".into());
        }
        if stopped {
            return Err("task_stopped".into());
        }
        if matches!(status.as_str(), "done" | "failed" | "cancelled") {
            return Err("task_finished".into());
        }
        let open: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM task_steering WHERE task_id = ?1
                   AND state IN ('pending','delivering')",
                params![new.task_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("submit steering: count: {e}"))?;
        if open >= STEERING_OPEN_LIMIT {
            return Err("steering_limit".into());
        }
        let seq: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(seq),0)+1 FROM task_steering WHERE task_id = ?1",
                params![new.task_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("submit steering: seq: {e}"))?;
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO task_steering (steering_id, task_id, seq, body, body_hash,
                    guard_flags_json, submitted_by, submitted_via, submitted_authority_revision,
                    client_request_id, state, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'dashboard',?8,?9,'pending',?10,?10)",
            params![
                id,
                new.task_id,
                seq,
                new.body,
                new.body_hash,
                new.guard_flags_json,
                new.submitted_by,
                authority_revision,
                new.client_request_id,
                now_s
            ],
        )
        .map_err(|e| format!("submit steering: insert: {e}"))?;
        let row = tx
            .query_row(
                &format!("SELECT {STEERING_COLUMNS} FROM task_steering WHERE steering_id = ?1"),
                params![id],
                row_to_steering,
            )
            .map_err(|e| format!("submit steering: reload: {e}"))?;
        tx.commit()
            .map_err(|e| format!("submit steering: commit: {e}"))?;
        Ok(SteeringSubmit::Created(row))
    }

    /// Every steering entry of a task, oldest first.
    pub async fn list_steering(&self, task_id: &str) -> Result<Vec<SteeringRow>, String> {
        let conn = self.conn.lock().await;
        steering_for_task_conn(
            &conn,
            task_id,
            &["pending", "delivering", "applied", "discarded"],
        )
    }

    /// Discard open steering of tasks that finished (`task_finished`) or were
    /// cancelled (`task_cancelled`; stop requests discard their own tree).
    /// Cheap no-op when no steering exists.
    pub async fn discard_steering_of_finished_tasks(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE task_steering SET state='discarded', updated_at=?1,
                    discard_reason = CASE
                      WHEN (SELECT status FROM tasks WHERE id = task_steering.task_id) = 'cancelled'
                        THEN 'task_cancelled' ELSE 'task_finished' END
              WHERE state IN ('pending','delivering')
                AND COALESCE((SELECT status FROM tasks WHERE id = task_steering.task_id), 'done')
                    IN ('done','failed','cancelled')",
            params![resp_ts(now)],
        )
        .map_err(|e| format!("discard finished steering: {e}"))
    }

    /// Facts the driver needs to decide whether a round is durable.
    pub async fn durable_info(&self, task_id: &str) -> Result<DurableInfo, String> {
        let conn = self.conn.lock().await;
        let occurrence = {
            let occ = conn
                .query_row(
                    &format!(
                        "SELECT {OCC_COLUMNS} FROM responsibility_occurrences WHERE task_id = ?1"
                    ),
                    params![task_id],
                    row_to_occurrence,
                )
                .optional()
                .map_err(|e| format!("durable info: occurrence: {e}"))?;
            match occ {
                Some(o) => {
                    let r = get_responsibility_conn(&conn, &o.responsibility_id)?
                        .ok_or("occurrence without responsibility")?;
                    Some((o, r))
                }
                None => None,
            }
        };
        let open_steering: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM task_steering WHERE task_id = ?1
                   AND state IN ('pending','delivering'))",
                params![task_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("durable info: steering: {e}"))?;
        let persisted_iters: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_dispatch_intents WHERE task_id = ?1 AND state <> 'abandoned'",
                params![task_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("durable info: iters: {e}"))?;
        Ok(DurableInfo {
            occurrence,
            open_steering,
            persisted_iters,
            intended: intent_conn(&conn, task_id, "intended")?,
            last_enqueued: intent_conn(&conn, task_id, "enqueued")?,
        })
    }

    /// Step 1 of the durable handoff (one `BEGIN IMMEDIATE`): re-read the
    /// task (must be a dispatch candidate, not in any stop tree), the
    /// responsibility of an occurrence (must not be `paused`), then open an
    /// `intended` intent `goal:<task>:<iter>` — reusing a leftover `intended`
    /// one, so a crash or a failed enqueue resends the same id — and move
    /// pending steering to `delivering` with it. `requested_iter` is the
    /// driver's view; the stored iteration is never lower than it nor than
    /// any earlier intent's.
    pub async fn begin_dispatch_intent(
        &self,
        task_id: &str,
        requested_iter: i64,
        now: DateTime<Utc>,
    ) -> Result<IntentBegin, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("begin intent: begin: {e}"))?;
        let task: Option<(String, i64, bool)> = tx
            .query_row(
                &format!(
                    "SELECT status, authority_revision, {} FROM tasks WHERE id = ?1",
                    in_stop_tree_sql("?1")
                ),
                params![task_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| format!("begin intent: task: {e}"))?;
        let Some((status, authority_revision, stopped)) = task else {
            return Ok(IntentBegin::Refused("task_missing"));
        };
        if stopped {
            return Ok(IntentBegin::Refused("task_stopped"));
        }
        if !matches!(status.as_str(), "todo" | "pending" | "revising") {
            return Ok(IntentBegin::Refused("task_not_dispatchable"));
        }
        if let Err(reason) = occurrence_gate_conn(&tx, task_id)? {
            return Ok(IntentBegin::Refused(reason));
        }
        let intent = match intent_conn(&tx, task_id, "intended")? {
            Some(existing) => existing,
            None => {
                let max_iter: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(iter),0) FROM task_dispatch_intents WHERE task_id = ?1",
                        params![task_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| format!("begin intent: iter: {e}"))?;
                let iter = requested_iter.max(max_iter + 1).max(1);
                let row = DispatchIntentRow {
                    intent_id: format!("goal:{task_id}:{iter}"),
                    task_id: task_id.to_string(),
                    iter,
                    authority_revision,
                    state: "intended".into(),
                    created_at: now_s.clone(),
                    updated_at: now_s.clone(),
                };
                tx.execute(
                    &format!(
                        "INSERT INTO task_dispatch_intents ({INTENT_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7)"
                    ),
                    params![
                        row.intent_id,
                        row.task_id,
                        row.iter,
                        row.authority_revision,
                        row.state,
                        row.created_at,
                        row.updated_at
                    ],
                )
                .map_err(|e| format!("begin intent: insert: {e}"))?;
                row
            }
        };
        tx.execute(
            "UPDATE task_steering SET state='delivering', intent_id=?2, updated_at=?3
              WHERE task_id=?1 AND state='pending'",
            params![task_id, intent.intent_id, now_s],
        )
        .map_err(|e| format!("begin intent: steering: {e}"))?;
        let steering: Vec<SteeringRow> = steering_for_task_conn(&tx, task_id, &["delivering"])?
            .into_iter()
            .filter(|s| s.intent_id.as_deref() == Some(intent.intent_id.as_str()))
            .collect();
        tx.commit()
            .map_err(|e| format!("begin intent: commit: {e}"))?;
        Ok(IntentBegin::Ready { intent, steering })
    }

    /// Step 3 of the durable handoff: the message with this id is in the
    /// queue. `intended → enqueued`; steering delivering with it →
    /// `applied` (round, message id, the task's current authority revision —
    /// recorded for display, never compared). Idempotent: a second call
    /// changes nothing. Returns how many steering entries became `applied`.
    pub async fn complete_dispatch_intent(
        &self,
        intent_id: &str,
        applied_round: i64,
        now: DateTime<Utc>,
    ) -> Result<usize, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("complete intent: begin: {e}"))?;
        tx.execute(
            "UPDATE task_dispatch_intents SET state='enqueued', updated_at=?2
              WHERE intent_id=?1 AND state='intended'",
            params![intent_id, now_s],
        )
        .map_err(|e| format!("complete intent: {e}"))?;
        let applied = tx
            .execute(
                "UPDATE task_steering SET state='applied', applied_round=?2,
                        applied_message_id=?1, updated_at=?3,
                        applied_authority_revision=(SELECT authority_revision FROM tasks
                                                     WHERE id = task_steering.task_id)
                  WHERE intent_id=?1 AND state='delivering'",
                params![intent_id, applied_round, now_s],
            )
            .map_err(|e| format!("complete intent: steering: {e}"))?;
        tx.commit()
            .map_err(|e| format!("complete intent: commit: {e}"))?;
        Ok(applied)
    }

    /// Abandon an `intended` intent. Its steering returns to `pending` when
    /// the task can still run, or is discarded when it finished.
    pub async fn abandon_dispatch_intent(
        &self,
        intent_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("abandon intent: begin: {e}"))?;
        let n = tx
            .execute(
                "UPDATE task_dispatch_intents SET state='abandoned', updated_at=?2
                  WHERE intent_id=?1 AND state='intended'",
                params![intent_id, now_s],
            )
            .map_err(|e| format!("abandon intent: {e}"))?;
        tx.execute(
            "UPDATE task_steering SET
                    state = CASE WHEN COALESCE((SELECT status FROM tasks WHERE id = task_steering.task_id),'done')
                                     IN ('done','failed','cancelled') THEN 'discarded' ELSE 'pending' END,
                    discard_reason = CASE WHEN COALESCE((SELECT status FROM tasks WHERE id = task_steering.task_id),'done')
                                     IN ('done','failed','cancelled') THEN 'task_finished' ELSE NULL END,
                    intent_id = NULL, updated_at=?2
              WHERE intent_id=?1 AND state='delivering'",
            params![intent_id, now_s],
        )
        .map_err(|e| format!("abandon intent: steering: {e}"))?;
        tx.commit()
            .map_err(|e| format!("abandon intent: commit: {e}"))?;
        Ok(n == 1)
    }

    /// Intents in one state (restart reconciliation).
    pub async fn dispatch_intents_in_state(
        &self,
        state: &str,
    ) -> Result<Vec<DispatchIntentRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {INTENT_COLUMNS} FROM task_dispatch_intents WHERE state = ?1
                 ORDER BY task_id, iter"
            ))
            .map_err(|e| format!("intents in state: {e}"))?;
        stmt.query_map(params![state], row_to_intent)
            .map_err(|e| format!("intents in state: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("intents in state: {e}"))
    }

    pub async fn get_dispatch_intent(
        &self,
        intent_id: &str,
    ) -> Result<Option<DispatchIntentRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {INTENT_COLUMNS} FROM task_dispatch_intents WHERE intent_id = ?1"),
            params![intent_id],
            row_to_intent,
        )
        .optional()
        .map_err(|e| format!("get intent: {e}"))
    }

    /// Dispatcher fence: may the `goal:` message with this id still be handed
    /// to an employee? `Ok(())` only when the intent is live (`intended` or
    /// `enqueued`), the task is still a dispatch candidate, no stop request
    /// covers it, and an occurrence's responsibility is not `paused`.
    /// Every other answer, including a read error, is a refusal.
    pub async fn fence_goal_dispatch(&self, intent_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT state, task_id FROM task_dispatch_intents WHERE intent_id = ?1",
                params![intent_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("fence: {e}"))?;
        let Some((intent_state, task_id)) = row else {
            return Err("stale_dispatch_fenced: no intent".into());
        };
        if !matches!(intent_state.as_str(), "intended" | "enqueued") {
            return Err(format!("stale_dispatch_fenced: intent {intent_state}"));
        }
        task_dispatchable_conn(&conn, &task_id)?;
        if intent_id
            != format!(
                "goal:{task_id}:{}",
                intent_id.rsplit(':').next().unwrap_or("")
            )
        {
            return Err("stale_dispatch_fenced: intent id does not name its task".into());
        }
        occurrence_gate_conn(&conn, &task_id)?
            .map_err(|reason| format!("stale_dispatch_fenced: {reason}"))
    }

    /// E-H2: the fence for every goal-loop message, durable or not. A round
    /// is never started for a task that is not a dispatch candidate any more
    /// or that sits under a stop request (its own or an ancestor's).
    /// M-1: the driver's per-candidate re-check — stop tree and dispatchable
    /// status only. The occurrence gate (paused / expired / reassigned) is
    /// left to the durable path and the dispatcher, after the deadline guard,
    /// so such an occurrence still reaches `needs_human` at its deadline.
    pub async fn fence_candidate(&self, task_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        task_dispatchable_conn(&conn, task_id)
    }

    pub async fn fence_plain_goal_dispatch(&self, task_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        task_dispatchable_conn(&conn, task_id)?;
        occurrence_gate_conn(&conn, task_id)?
            .map_err(|reason| format!("stale_dispatch_fenced: {reason}"))
    }
}

/// Status is a dispatch candidate and no stop covers the task.
fn task_dispatchable_conn(conn: &Connection, task_id: &str) -> Result<(), String> {
    let row: Option<(String, bool)> = conn
        .query_row(
            &format!(
                "SELECT status, {} FROM tasks WHERE id = ?1",
                in_stop_tree_sql("?1")
            ),
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("fence: {e}"))?;
    let Some((status, stopped)) = row else {
        return Err("stale_dispatch_fenced: task missing".into());
    };
    if stopped {
        return Err("stale_dispatch_fenced: task stopped".into());
    }
    if !CANDIDATE_STATUS_LIST.contains(&status.as_str()) {
        return Err(format!("stale_dispatch_fenced: task {status}"));
    }
    Ok(())
}

impl TaskStore {
    /// A round that never reached the employee (fenced by the dispatcher,
    /// or failed before any agent ran it): its intent becomes `abandoned`
    /// (it neither counts as an iteration nor as a round that may have cost
    /// money — H-1), and the directions marked handed over with it go back
    /// to `pending` (E-M2), unless the task already ended. One transaction.
    /// Returns how many directions went back.
    pub async fn return_unrun_round(
        &self,
        message_id: &str,
        now: DateTime<Utc>,
    ) -> Result<usize, String> {
        let now_s = resp_ts(now);
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("unrun round: begin: {e}"))?;
        // M3-1: a round the dispatcher marked as started did run (the model
        // may have been called, its directions were in the prompt): it stays
        // counted and its directions stay delivered.
        let started: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM task_dispatch_intents
                                WHERE intent_id = ?1 AND started_at IS NOT NULL)",
                params![message_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("unrun round: started: {e}"))?;
        if started {
            return Ok(0);
        }
        tx.execute(
            "UPDATE task_dispatch_intents SET state='abandoned', updated_at=?2
              WHERE intent_id=?1 AND state='enqueued'",
            params![message_id, now_s],
        )
        .map_err(|e| format!("unrun round: intent: {e}"))?;
        let n = tx
            .execute(
                "UPDATE task_steering SET state='pending', intent_id=NULL, applied_round=NULL,
                        applied_message_id=NULL, applied_authority_revision=NULL, updated_at=?2
                  WHERE applied_message_id = ?1 AND state = 'applied'
                    AND COALESCE((SELECT status FROM tasks WHERE id = task_steering.task_id), 'done')
                        NOT IN ('done','failed','cancelled')",
                params![message_id, now_s],
            )
            .map_err(|e| format!("unrun round: steering: {e}"))?;
        tx.commit()
            .map_err(|e| format!("unrun round: commit: {e}"))?;
        Ok(n)
    }

    /// M3-1: record, before a runtime is started, that this durable round is
    /// being handed to an agent. Idempotent (the first time is kept). Returns
    /// whether a durable intent with that id exists.
    pub async fn mark_round_started(
        &self,
        intent_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE task_dispatch_intents SET started_at = COALESCE(started_at, ?2)
              WHERE intent_id = ?1",
            params![intent_id, resp_ts(now)],
        )
        .map(|n| n > 0)
        .map_err(|e| format!("mark round started: {e}"))
    }

    /// M3-1: whether any round of this task was handed to a runtime (the
    /// durable start mark), whatever happened to its queue message since.
    pub async fn any_round_started(&self, task_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_dispatch_intents
                            WHERE task_id = ?1 AND started_at IS NOT NULL)",
            params![task_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("round started: {e}"))
    }

    /// Ids of this task's `enqueued` (sent, not abandoned) round intents.
    pub async fn enqueued_intent_ids(&self, task_id: &str) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT intent_id FROM task_dispatch_intents WHERE task_id = ?1 AND state = 'enqueued'",
            )
            .map_err(|e| format!("enqueued intents: {e}"))?;
        stmt.query_map(params![task_id], |r| r.get(0))
            .map_err(|e| format!("enqueued intents: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("enqueued intents: {e}"))
    }
}
