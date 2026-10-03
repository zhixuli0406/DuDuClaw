//! Terminal transitions: completion, goal-mode acceptance review,
//! `needs_human` and cancellation. Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
    /// Worker completion. Goal-mode tasks route to `review` (judge acceptance
    /// pending) carrying the result summary; others go straight to `done`.
    /// Returns the updated row, or `None` if the task does not exist.
    ///
    /// **Holder guard (HIGH-2):** a task with a non-null `claimed_by` can only
    /// be completed by that holder — `caller` must match, or the call errors.
    /// A reclaimed zombie worker therefore cannot clobber the result of the
    /// worker the task was re-dispatched to. Unclaimed / legacy board tasks
    /// (`claimed_by IS NULL`) keep the pre-guard behavior: any caller may
    /// complete them. Read-check-write runs in one IMMEDIATE transaction.
    pub async fn complete_task(
        &self,
        id: &str,
        summary: &str,
        caller: &str,
    ) -> Result<Option<TaskRow>, String> {
        let now = Utc::now().to_rfc3339();
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("complete: begin: {e}"))?;
            let row: Option<(bool, Option<String>, i64, Option<String>, String)> = tx
                .query_row(
                    "SELECT goal_mode, claimed_by, revision_round, claimed_at, created_at
                       FROM tasks WHERE id = ?1 AND kind IN ('task','goal')",
                    params![id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)? != 0,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| format!("complete: load: {e}"))?;
            let Some((goal_mode, claimed_by, revision_round, claimed_at, created_at)) = row else {
                return Ok(None);
            };
            if let Some(holder) = claimed_by.as_deref() {
                if holder != caller {
                    return Err(format!(
                        "task {id} is claimed by '{holder}'; only the claim holder may complete it (caller: '{caller}')"
                    ));
                }
            }
            // Guard: never overwrite a task that has already reached a terminal
            // state. Without this, a stale worker (e.g. one whose lease was
            // reclaimed and reassigned) could clobber the authoritative result
            // by calling complete on an already-`done`/`cancelled` task.
            if goal_mode {
                let affected = tx
                    .execute(
                        "UPDATE tasks
                        SET status = 'review', result_summary = ?2,
                            lease_expires_at = NULL, updated_at = ?3
                      WHERE id = ?1 AND kind IN ('task','goal') AND status NOT IN ('done', 'cancelled')",
                        params![id, summary, now],
                    )
                    .map_err(|e| format!("complete (review): {e}"))?;
                // Iterative Kanban: stamp this round's submission and add the
                // per-round agent seconds (submitted − dispatched) to the task's
                // cumulative agent clock. Only when the completion actually took
                // effect (a terminal-state clobber attempt records nothing).
                if affected == 1 {
                    let fallback_dispatch = claimed_at.as_deref().unwrap_or(&created_at);
                    let secs =
                        iter_submit_conn(&tx, id, &now, revision_round + 1, fallback_dispatch)?;
                    if secs > 0 {
                        tx.execute(
                            "UPDATE tasks SET agent_seconds = agent_seconds + ?2 WHERE id = ?1 AND kind IN ('task','goal')",
                            params![id, secs],
                        )
                        .map_err(|e| format!("complete (agent_seconds): {e}"))?;
                    }
                }
            } else {
                tx.execute(
                    "UPDATE tasks
                        SET status = 'done', result_summary = ?2,
                            completed_at = ?3, lease_expires_at = NULL, updated_at = ?3
                      WHERE id = ?1 AND kind IN ('task','goal') AND status NOT IN ('done', 'cancelled')",
                    params![id, summary, now],
                )
                .map_err(|e| format!("complete (done): {e}"))?;
            }
            tx.commit().map_err(|e| format!("complete: commit: {e}"))?;
        }
        self.get_task(id).await
    }

    /// Goal-mode acceptance passed: promote a `review` task to `done`.
    pub async fn accept_review(&self, id: &str, feedback: &str) -> Result<bool, String> {
        self.accept_review_with_verdict(id, feedback, None).await
    }

    /// [`Self::accept_review`] carrying the structured per-aspect panel
    /// verdict (`[{name, pass, reason}]` JSON) for the round timeline.
    pub async fn accept_review_with_verdict(
        &self,
        id: &str,
        feedback: &str,
        verdict_json: Option<&str>,
    ) -> Result<bool, String> {
        self.accept_review_with_ledger(id, feedback, verdict_json, None)
            .await
    }

    /// [`Self::accept_review_with_verdict`] plus the A1 ledger's harness knob
    /// snapshot (`knobs_json`) sealed on the accepted round. Task-state
    /// transition is identical.
    pub async fn accept_review_with_ledger(
        &self,
        id: &str,
        feedback: &str,
        verdict_json: Option<&str>,
        knobs_json: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        // A1-2: read the accepted round's own output for its excerpt. The
        // UPDATE below never touches `result_summary`, so reading it first is
        // equivalent to reading it after. A read failure only loses the
        // excerpt, never the acceptance.
        let result_summary: Option<String> = conn
            .query_row(
                "SELECT result_summary FROM tasks WHERE id = ?1 AND kind IN ('task','goal')",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or_else(|e| {
                tracing::warn!(task = id, error = %e, "accept: excerpt read failed (non-fatal)");
                None
            })
            .flatten();
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'done', completed_at = ?2, judge_feedback = ?3, updated_at = ?2
                  WHERE id = ?1 AND kind IN ('task','goal') AND status = 'review'",
                params![id, now, feedback],
            )
            .map_err(|e| format!("accept review: {e}"))?;
        if n == 1 {
            // Iterative Kanban: seal the current round's verdict. A1-2
            // (2026-09-30): the accepted round now keeps the same bounded
            // `worker_excerpt` snapshot the reject path stores, so every
            // sealed round in the ledger carries its own output (it is still
            // never a WP-4F best-round candidate — `pick_best_round` only
            // considers rejected/escalated rounds).
            let excerpt =
                crate::goal_budget_best_round::worker_excerpt(result_summary.as_deref());
            iter_verdict_conn(
                &conn,
                id,
                "accepted",
                feedback,
                verdict_json,
                excerpt.as_deref(),
                knobs_json,
                None,
                &now,
            )?;
        }
        Ok(n == 1)
    }

    /// Goal-mode acceptance rejected. Iterative Kanban: send the task to the new
    /// `revising` state (not `pending`) for another round — claim/lease cleared,
    /// `revision_round` incremented, `diminishing` flag raised once the round
    /// count reaches `soft_cap` (the loop is NOT blocked, only flagged). When the
    /// judge retry budget (`max_retries`) is exhausted, escalate to `needs_human`
    /// instead (fail-safe — never loops indefinitely). Returns the status applied.
    ///
    /// `soft_cap` is the goal loop's soft cap (default 3); the diminishing flag
    /// only affects dashboard presentation, never dispatch.
    pub async fn reject_review(
        &self,
        id: &str,
        feedback: &str,
        soft_cap: i64,
    ) -> Result<String, String> {
        self.reject_review_with_verdict(id, feedback, soft_cap, None)
            .await
    }

    /// [`Self::reject_review`] carrying the structured per-aspect panel
    /// verdict for the round timeline (`None` for deterministic pre-judge
    /// rejections, which have no panel).
    pub async fn reject_review_with_verdict(
        &self,
        id: &str,
        feedback: &str,
        soft_cap: i64,
        verdict_json: Option<&str>,
    ) -> Result<String, String> {
        self.reject_review_with_ledger(id, feedback, soft_cap, verdict_json, None)
            .await
    }

    /// [`Self::reject_review_with_verdict`] plus the A1 ledger's harness knob
    /// snapshot sealed on the rejected/escalated round. On the escalation
    /// branch the round row also records `pause_reason = budget_exhausted`.
    /// Task-state transitions are identical.
    pub async fn reject_review_with_ledger(
        &self,
        id: &str,
        feedback: &str,
        soft_cap: i64,
        verdict_json: Option<&str>,
        knobs_json: Option<&str>,
    ) -> Result<String, String> {
        let row = match self.get_task(id).await? {
            Some(r) => r,
            None => return Err(format!("task not found: {id}")),
        };
        let now = Utc::now().to_rfc3339();
        // WP-4F: snapshot THIS round's own worker output before it is wiped
        // (the "revising" branch below nulls `result_summary`) — the only
        // point in the whole rejection flow where the round's real output is
        // still readable. Bounded + CJK-safe so a multi-KB agent reply never
        // balloons the iteration history row. `None` when the round produced
        // no result text at all.
        let worker_excerpt: Option<String> =
            crate::goal_budget_best_round::worker_excerpt(row.result_summary.as_deref());
        let conn = self.conn.lock().await;
        if row.retry_count < row.max_retries {
            let new_retry = row.retry_count + 1;
            let new_round = row.revision_round + 1;
            let diminishing = new_round >= soft_cap.max(1);
            let n = conn
                .execute(
                    "UPDATE tasks
                    SET status = 'revising', claimed_by = NULL, claimed_at = NULL,
                        lease_expires_at = NULL, retry_count = ?2, revision_round = ?3,
                        diminishing = ?4, judge_feedback = ?5, result_summary = NULL,
                        updated_at = ?6
                  WHERE id = ?1 AND kind IN ('task','goal') AND status = 'review'",
                    params![id, new_retry, new_round, diminishing as i64, feedback, now],
                )
                .map_err(|e| format!("reject review (revising): {e}"))?;
            if n == 1 {
                iter_verdict_conn(
                    &conn,
                    id,
                    "rejected",
                    feedback,
                    verdict_json,
                    worker_excerpt.as_deref(),
                    knobs_json,
                    None,
                    &now,
                )?;
            }
            Ok("revising".to_string())
        } else {
            // H11: the retry budget is spent — a hard cap fired, not a fresh
            // blocker. Stamped here (not derived from `feedback`, which is
            // judge-authored prose) so the dashboard/channel chip is exact.
            let n = conn
                .execute(
                    "UPDATE tasks
                    SET status = 'needs_human', judge_feedback = ?2, pause_reason = ?4,
                        updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal') AND status = 'review'",
                    params![
                        id,
                        feedback,
                        now,
                        crate::pause_reason::PauseReason::BudgetExhausted.as_str()
                    ],
                )
                .map_err(|e| format!("reject review (escalate): {e}"))?;
            if n == 1 {
                iter_verdict_conn(
                    &conn,
                    id,
                    "escalated",
                    feedback,
                    verdict_json,
                    worker_excerpt.as_deref(),
                    knobs_json,
                    Some(crate::pause_reason::PauseReason::BudgetExhausted.as_str()),
                    &now,
                )?;

                // WP-4F: this is the OTHER budget-exhausted escalation site
                // (the judge retry budget, `max_retries` — distinct from
                // `goal_loop::GoalLoopDriver::escalate`'s iteration-cap /
                // wall-clock checks, but the same `PauseReason::
                // BudgetExhausted` family per `goal_loop/state.rs`'s own doc
                // comment: "iteration cap, judge retry budget, the global
                // wall clock, or a per-goal deadline_at"). Attach the
                // closest-to-done round instead of leaving `judge_feedback`
                // as the bare last-round rejection text. Best-effort: any
                // failure below leaves `judge_feedback` exactly as already
                // written above, never blocks the escalation itself.
                if let Ok(iterations) = list_iterations_conn(&conn, id) {
                    if let Some(pick) = crate::goal_budget_best_round::pick_best_round(&iterations)
                    {
                        let enriched =
                            crate::goal_budget_best_round::compose_escalation_note(feedback, &pick);
                        let _ = conn.execute(
                            "UPDATE tasks SET judge_feedback = ?2 WHERE id = ?1 AND kind IN ('task','goal')",
                            params![id, enriched],
                        );
                    }
                }
            }
            Ok("needs_human".to_string())
        }
    }

    /// Fail-safe escalation: park a task for human attention without killing or
    /// looping it. Used when the judge itself errors (goal mode).
    ///
    /// H11: leaves `pause_reason` unclassified ([`PauseReason::Unknown`] at
    /// read time). Production escalation paths call
    /// [`Self::mark_needs_human_with_pause`] instead — this string-only form
    /// is kept for callers (and tests) that genuinely have no class to
    /// declare, and its fallback is the *safe* direction (「需要人工確認」).
    pub async fn mark_needs_human(&self, id: &str, reason: &str) -> Result<bool, String> {
        self.mark_needs_human_with_pause(id, reason, crate::pause_reason::PauseReason::Unknown)
            .await
    }

    /// H11: [`Self::mark_needs_human`] carrying the structured pause class.
    ///
    /// The class is supplied by the caller because only the call site knows
    /// the trigger statically — three of the `reason` strings this stores are
    /// built from LLM output or a transport error, so classifying them by
    /// substring afterwards would be a routing decision made on model-authored
    /// prose (coding convention 2). [`PauseReason::Unknown`] is written as a
    /// real token rather than `NULL` so "explicitly unclassified" and "row
    /// predates the column" are the same at read time and neither can be
    /// mistaken for a confident class.
    pub async fn mark_needs_human_with_pause(
        &self,
        id: &str,
        reason: &str,
        pause: crate::pause_reason::PauseReason,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks SET status = 'needs_human', judge_feedback = ?2, pause_reason = ?4,
                        updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal')",
                params![id, reason, now, pause.as_str()],
            )
            .map_err(|e| format!("mark needs_human: {e}"))?;
        Ok(n > 0)
    }

    /// A1-2: [`Self::mark_needs_human_with_pause`] for the **settle** path —
    /// a round that ends in `needs_human` without a judge ruling (evaluator
    /// `blocked`, judge error, a leftover removed `human_only`).
    ///
    /// The task-row write is byte-identical to `mark_needs_human_with_pause`
    /// (same statement, same result). Afterwards, only when that write took
    /// effect, the latest un-judged round is sealed `escalated` with the pause
    /// class, the round's own output excerpt and the knob snapshot (see
    /// `iter_escalate_seal_conn` for why `judge_feedback` stays NULL). The
    /// seal is pure bookkeeping: its failure is logged and never returned.
    pub async fn mark_needs_human_sealing_round(
        &self,
        id: &str,
        reason: &str,
        pause: crate::pause_reason::PauseReason,
        knobs_json: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks SET status = 'needs_human', judge_feedback = ?2, pause_reason = ?4,
                        updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal')",
                params![id, reason, now, pause.as_str()],
            )
            .map_err(|e| format!("mark needs_human: {e}"))?;
        if n > 0 {
            let seal = conn
                .query_row(
                    "SELECT result_summary FROM tasks WHERE id = ?1 AND kind IN ('task','goal')",
                    params![id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(|e| format!("escalate seal: excerpt read: {e}"))
                .and_then(|summary| {
                    let excerpt = crate::goal_budget_best_round::worker_excerpt(
                        summary.flatten().as_deref(),
                    );
                    iter_escalate_seal_conn(
                        &conn,
                        id,
                        pause.as_str(),
                        excerpt.as_deref(),
                        knobs_json,
                        &now,
                    )
                });
            if let Err(e) = seal {
                tracing::warn!(task = id, error = %e, "A1 ledger: escalated-round seal failed (non-fatal)");
            }
        }
        Ok(n > 0)
    }

    /// I-1c "想一想": consume the pending plan-first plan after
    /// [`crate::goal_loop::GoalLoopDriver::enqueue_work`] has injected it into
    /// a round's dispatch payload, so it is injected exactly once (the first
    /// round after approval) rather than on every subsequent round. Not
    /// gated on task status — the caller (the driver, right after a
    /// successful enqueue) already knows this is the correct moment; a
    /// failed clear here is harmless (the plan is simply re-injected next
    /// dispatch, which repeats guidance rather than losing anything).
    pub async fn clear_plan_pending(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE tasks SET plan_pending = NULL WHERE id = ?1 AND kind IN ('task','goal')",
            params![id],
        )
        .map_err(|e| format!("clear plan_pending: {e}"))?;
        Ok(())
    }

    /// P2a: apply a human decision to a `needs_human` goal task from a channel
    /// button. Only transitions FROM `needs_human` (fail-closed + idempotent:
    /// a second press on an already-resolved task affects 0 rows → `Ok(false)`).
    ///
    /// - `retry` → back to `pending`, claim/lease/result cleared. `note`
    ///   (optional human instruction) is written to `judge_feedback` so the next
    ///   driver dispatch carries it; an empty note clears `judge_feedback`.
    /// - `done`  → `done` + `completed_at`, `note` recorded in `judge_feedback`.
    /// - `abort` → `cancelled`, `note` recorded in `judge_feedback`.
    ///
    /// An unrecognised `decision` is rejected (never silently coerced).
    pub async fn resolve_needs_human(
        &self,
        id: &str,
        decision: &str,
        note: &str,
    ) -> Result<bool, String> {
        self.resolve_needs_human_inner(id, decision, note, false).await
    }

    /// Called only after the dashboard/channel authorization gate succeeds.
    pub(crate) async fn resolve_needs_human_with_survival_evidence(
        &self, id: &str, decision: &str, note: &str,
    ) -> Result<bool, String> {
        self.resolve_needs_human_inner(id, decision, note, true).await
    }

    async fn resolve_needs_human_inner(
        &self, id: &str, decision: &str, note: &str, authenticated: bool,
    ) -> Result<bool, String> {
        let note_opt: Option<&str> = if note.trim().is_empty() {
            None
        } else {
            Some(note)
        };
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("resolve needs_human: begin: {e}"))?;
        let now = Utc::now().to_rfc3339();
        // H11: the pause is over on every branch — clear the class so a task
        // sent back around the loop (or closed out) never renders a stale
        // 「卡住沒進展」chip from the pause a human just resolved.
        let n = match decision {
            "retry" => tx
                .execute(
                    "UPDATE tasks
                        SET status = 'pending', claimed_by = NULL, claimed_at = NULL,
                            lease_expires_at = NULL, result_summary = NULL,
                            judge_feedback = ?2, pause_reason = NULL, updated_at = ?3
                      WHERE id = ?1 AND kind IN ('task','goal') AND status = 'needs_human'",
                    params![id, note_opt, now],
                )
                .map_err(|e| format!("resolve needs_human (retry): {e}"))?,
            "done" => tx
                .execute(
                    "UPDATE tasks
                        SET status = 'done', completed_at = ?3, judge_feedback = ?2,
                            pause_reason = NULL, updated_at = ?3
                      WHERE id = ?1 AND kind IN ('task','goal') AND status = 'needs_human'",
                    params![id, note_opt, now],
                )
                .map_err(|e| format!("resolve needs_human (done): {e}"))?,
            "abort" => tx
                .execute(
                    "UPDATE tasks
                        SET status = 'cancelled', judge_feedback = ?2, pause_reason = NULL,
                            updated_at = ?3
                      WHERE id = ?1 AND kind IN ('task','goal') AND status = 'needs_human'",
                    params![id, note_opt, now],
                )
                .map_err(|e| format!("resolve needs_human (abort): {e}"))?,
            other => return Err(format!("unknown needs_human decision: {other}")),
        };
        if n == 1 && authenticated {
            survival_decision_receipt_conn(&tx, id, decision)?;
        }
        tx.commit().map_err(|e| format!("resolve needs_human: commit: {e}"))?;
        Ok(n == 1)
    }

    /// I-3a: reopen a `done` / `failed` / `cancelled` **goal-mode** task for
    /// another round, carrying the user's follow-up message into the next
    /// dispatch's prompt — WorkBuddy's "a finished/failed task can take a
    /// follow-up message" pattern (`DESIGN-dashboard-ux-workbuddy-2026-08.md`
    /// §3.3, backlog item I-3a). Deliberately a separate method from
    /// [`Self::resolve_needs_human`] rather than widening its `retry` arm:
    /// that method also backs the **channel** decision buttons
    /// ([`crate::goal_notify::apply_needs_human`]), and a channel card is
    /// only ever rendered while a task sits in `needs_human` — widening its
    /// WHERE clause would let a stale button, pressed after the task later
    /// reached `done` through a legitimate unrelated path, silently reopen
    /// it. This method is reachable only from the dashboard's explicit
    /// "接著做" action (`tasks.goal_decide` with `action: "continue"`).
    ///
    /// `message` is required (unlike the optional `note` on
    /// `resolve_needs_human`'s retry) — "continue with nothing to add" is
    /// just `retry`, which already exists for `needs_human`. The message is
    /// stamped with [`CONTINUE_MESSAGE_PREFIX`] so the next dispatch's
    /// prompt-builder ([`crate::goal_loop::GoalLoopDriver::enqueue_work`])
    /// can tell it apart from a genuine judge-rejection `judge_feedback` and
    /// phrase the two differently — without the marker, a continued task
    /// would be told "your last round failed review", which is simply false
    /// for a task that had actually succeeded.
    ///
    /// `revision_round` / `agent_seconds` / `diminishing` are deliberately
    /// left untouched so the round counter and dual-clock history continue
    /// rather than reset (the design doc's "iteration 計數延續"
    /// requirement) — same reasoning as `resolve_needs_human`'s retry arm,
    /// which never touches them either. `completed_at` IS cleared: a
    /// `pending` task carrying a stale completion timestamp from a previous
    /// `done` round would misrepresent the row.
    pub async fn continue_from_terminal(&self, id: &str, message: &str) -> Result<bool, String> {
        self.continue_from_terminal_inner(id, message, false).await
    }

    /// The verified dashboard decision path; agent-facing bare calls do not assert a human retry.
    pub(crate) async fn continue_from_terminal_with_survival_evidence(
        &self, id: &str, message: &str,
    ) -> Result<bool, String> {
        self.continue_from_terminal_inner(id, message, true).await
    }

    async fn continue_from_terminal_inner(&self, id: &str, message: &str, authenticated: bool) -> Result<bool, String> {
        let message = message.trim();
        if message.is_empty() {
            return Err("接著做需要附上訊息".into());
        }
        let stamped = format!("{CONTINUE_MESSAGE_PREFIX}{message}");
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("continue from terminal: begin: {e}"))?;
        let now = Utc::now().to_rfc3339();
        let n = tx
            .execute(
                "UPDATE tasks
                    SET status = 'pending', claimed_by = NULL, claimed_at = NULL,
                        lease_expires_at = NULL, result_summary = NULL, completed_at = NULL,
                        judge_feedback = ?2, updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal') AND status IN ('done', 'failed', 'cancelled')
                    AND COALESCE(goal_mode, 0) = 1",
                params![id, stamped, now],
            )
            .map_err(|e| format!("continue from terminal: {e}"))?;
        if n == 1 && authenticated {
            survival_decision_receipt_conn(&tx, id, "retry")?;
        }
        tx.commit().map_err(|e| format!("continue from terminal: commit: {e}"))?;
        Ok(n == 1)
    }

    /// W1-5: mark a `needs_human` goal task as claimed by a human decider —
    /// the "Take over" half of the Submit/Take over pair (D6, Intercom
    /// `Loop in teammate`). Deliberately does NOT change `status`: a task
    /// sitting in `needs_human` is already excluded from
    /// `GoalLoopDriver::tick_once`'s dispatch-candidate query (only
    /// `todo`/`pending`/`revising` are ever picked up), so no further state
    /// is needed to stop the automatic loop from retrying it — the "stop
    /// auto-retry" half of takeover is a side effect of the task already
    /// being parked, not something this method has to enforce.
    ///
    /// Reuses the existing `claimed_by` column (the one worker-lease claims
    /// use elsewhere): safe to share because a `needs_human` row is never
    /// itself a claim/lease candidate (`claim_task` only matches
    /// `pending`/`revising`), so the two meanings never collide. Idempotent
    /// and repeatable — unlike [`Self::resolve_needs_human`] there is no
    /// terminal state to race against, so a second (or a different
    /// authorized decider's) take-over press simply re-stamps `claimed_by`.
    ///
    /// This is the button-driven half of takeover: it stops the loop for one
    /// parked task and records who is handling it by hand. Taking over the
    /// **conversation** — pausing inbound AI replies and every scheduled
    /// dispatch aimed at it — is W3-1's
    /// [`crate::takeover`] / [`duduclaw_core::takeover_state`], which claims
    /// the conversation's live goal tasks through
    /// [`Self::claim_conversation_tasks`] instead.
    pub async fn claim_needs_human(&self, id: &str, decider: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks SET claimed_by = ?2, updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal') AND status = 'needs_human'",
                params![id, decider, now],
            )
            .map_err(|e| format!("claim needs_human: {e}"))?;
        Ok(n == 1)
    }

    /// W3-1 (D4): stamp `claimed_by` on every live goal task that came from
    /// one channel conversation, and return the ids that were stamped.
    ///
    /// This is step 2 of the atomic three-in-one a takeover performs (pause
    /// the conversation, claim its work, post to the Activity Feed). Without
    /// it, a human who takes over a conversation still shows up on the board
    /// as "the AI is on it", and the next person to look at the task has no
    /// way to know somebody is already handling it by hand.
    ///
    /// Scope is deliberately narrow:
    /// - **goal tasks only** (`goal_mode = 1`) — an ordinary board task is not
    ///   driven by this conversation and must not be silently reassigned.
    /// - **non-terminal only** — a finished task's `claimed_by` is history.
    /// - **unclaimed or already this decider's** — one human taking over must
    ///   not steal a row another worker holds a lease on
    ///   ([`Self::claim_task`]'s meaning of the same column).
    pub async fn claim_conversation_tasks(
        &self,
        channel: &str,
        chat_id: &str,
        decider: &str,
    ) -> Result<Vec<String>, String> {
        if channel.trim().is_empty() || chat_id.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM tasks
                  WHERE kind IN ('task','goal') AND COALESCE(goal_mode, 0) = 1
                    AND source_channel = ?1 AND source_chat_id = ?2
                    AND status NOT IN ('done', 'cancelled', 'failed')
                    AND (claimed_by IS NULL OR claimed_by = ?3)",
            )
            .map_err(|e| format!("claim conversation tasks (prepare): {e}"))?;
        let ids: Vec<String> = stmt
            .query_map(params![channel.trim(), chat_id.trim(), decider], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| format!("claim conversation tasks (query): {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        for id in &ids {
            conn.execute(
                "UPDATE tasks SET claimed_by = ?2, updated_at = ?3 WHERE id = ?1 AND kind IN ('task','goal')",
                params![id, decider, now],
            )
            .map_err(|e| format!("claim conversation tasks (update {id}): {e}"))?;
        }
        Ok(ids)
    }

    /// P2a: cancel a task that has not reached a terminal state. Used by the
    /// goal-loop kickoff gate when a human denies (or lets the approval expire)
    /// before the first dispatch. Idempotent: a task already
    /// `done`/`cancelled`/`failed` is left untouched (returns `Ok(false)`).
    pub async fn cancel_task(&self, id: &str, reason: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'cancelled', judge_feedback = ?2, updated_at = ?3
                  WHERE id = ?1 AND kind IN ('task','goal') AND status NOT IN ('done', 'cancelled', 'failed')",
                params![id, reason, now],
            )
            .map_err(|e| format!("cancel task: {e}"))?;
        Ok(n == 1)
    }

    // ── Iterative Kanban: iteration detail (v1.45) ──────────
}

/// Private receipt writer. Neither arbitrary Activity events nor public task
/// mutation methods can enter this path. The caller owns the transition's tx.
fn survival_decision_receipt_conn(conn: &Connection, task_id: &str, decision: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO task_survival_evidence(task_id,evidence_version)
         SELECT id,1 FROM tasks WHERE id=?1 AND goal_mode=1 AND kind IN ('task','goal')
         ON CONFLICT(task_id) DO NOTHING",
        params![task_id],
    ).map_err(|e| format!("survival evidence: initialize partial receipt: {e}"))?;
    conn.execute(
        "UPDATE task_survival_evidence
         SET manual_retry=CASE WHEN ?2='retry' THEN 1 ELSE manual_retry END,
             human_approved=CASE WHEN ?2='done' THEN 1 ELSE 0 END,
             human_approved_iteration_id=CASE WHEN ?2='done' THEN
                 (SELECT id FROM task_iterations WHERE task_id=?1 ORDER BY id DESC LIMIT 1)
                 ELSE NULL END
         WHERE task_id=?1 AND evidence_version=1",
        params![task_id,decision],
    ).map_err(|e| format!("survival evidence: write decision receipt: {e}"))?;
    Ok(())
}
