//! `needs_human` escalation and the reconciliation of a human decision.
//! Moved verbatim out of `goal_loop.rs`.

use super::*;

impl GoalLoopDriver {
    /// Park a task for a human and drop its in-flight tracking.
    ///
    /// H11: `pause` is the closed classification of this escalation, supplied
    /// by the call site (the trigger is known statically there; `reason` is
    /// free text that in some paths embeds a task id or LLM prose and must
    /// never be re-parsed for a routing decision).
    ///
    /// WP-4F: a `BudgetExhausted` escalation (iteration cap / wall clock /
    /// per-task deadline — the only pause classes this driver's own caps
    /// ever raise) attaches the closest-to-done round's excerpt + gap list
    /// instead of leaving the human with a bare "we ran out of budget"
    /// reason (see `goal_loop/state.rs`). Every other pause class
    /// (`NoProgress`, `BlockedNeedsDecision`) passes `reason` through
    /// byte-identical to before this feature existed. Best-effort: a failed
    /// iteration-history read degrades to the bare `reason`, never blocks
    /// the escalation itself.
    pub(super) async fn escalate(
        &self,
        inflight: &mut HashMap<String, InFlight>,
        task: &TaskRow,
        reason: &str,
        pause: crate::pause_reason::PauseReason,
    ) -> Result<(), String> {
        let effective_reason = if pause == crate::pause_reason::PauseReason::BudgetExhausted {
            match self.store.list_iterations(&task.id).await {
                Ok(iterations) => {
                    match crate::goal_budget_best_round::pick_best_round(&iterations) {
                        Some(pick) => {
                            crate::goal_budget_best_round::compose_escalation_note(reason, &pick)
                        }
                        // 0 rounds ever judged (e.g. a wall-clock deadline
                        // hit before the first dispatch) — keep the bare
                        // pre-WP-4F reason, never fabricate a pick.
                        None => reason.to_string(),
                    }
                }
                Err(e) => {
                    warn!(
                        task = %task.id, error = %e,
                        "goal loop: iteration history read failed for WP-4F best-round pick (non-fatal, using bare reason)"
                    );
                    reason.to_string()
                }
            }
        } else {
            reason.to_string()
        };
        self.store
            .mark_needs_human_with_pause(&task.id, &effective_reason, pause)
            .await?;
        // A1 ledger: keep the pause class on the latest round row, where it
        // survives `resolve_needs_human` clearing `tasks.pause_reason`. The
        // verdict is not touched. Bookkeeping only — failure is logged.
        if let Err(e) = self
            .store
            .stamp_iteration_pause(&task.id, pause.as_str())
            .await
        {
            warn!(task = %task.id, error = %e, "A1 ledger: iteration pause stamp failed (non-fatal)");
        }
        // WP3 (PORTICO): escalation ends the autonomous phase (iteration cap /
        // oscillation) → revoke the task's grants. Mirrors the DispatchEngine
        // needs_human revocation for the goal-loop-side escalation path.
        self.revoke_task_grants(&task.id, crate::capability_grants::REVOKE_REASON_PHASE_END)
            .await;
        if let Some(removed) = inflight.remove(&task.id) {
            self.release_lease(&removed); // RFC-27: free the slot on escalation
        }
        // A2 lifecycle: this driver's own escalation is a terminal phase end
        // — drop visit-graph tracking (the tracked-loop's terminal branches
        // also clear it, for escalations DispatchEngine triggers directly;
        // this call covers the driver-triggered path with no gap in
        // between).
        self.visit_graph.clear_task(&task.id).await;
        // L3: same `state_capture_seen` leak fix as the reconcile loop's
        // terminal branches — this driver's own escalation (iteration cap /
        // deadline / A2 oscillation) is ALSO a terminal phase end that never
        // re-enters the candidate set, so the top-of-tick prune alone would
        // never clear it.
        self.state_capture_seen.lock().await.remove(&task.id);
        self.post_activity(
            "goal_loop.needs_human",
            &task.assigned_to,
            Some(&task.id),
            &format!("goal-loop 轉人工:{reason} — {}", task.title),
        )
        .await;
        warn!(task = %task.id, %reason, "goal loop: escalated to needs_human");
        Ok(())
    }

    /// Push a channel approval for every goal task newly parked `needs_human`.
    /// Catches BOTH escalation paths (this driver's caps AND the DispatchEngine
    /// judge rejection at retry budget) with one detector. For `Observer`
    /// agents the loop does not wait: the task is auto-closed (`cancelled`) and
    /// the human is notified after the fact. Best-effort — never fails the tick.
    pub(super) async fn reconcile_needs_human(&self) {
        let tasks = match self.store.tasks_in_status("needs_human").await {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "goal loop: needs_human scan failed (will retry)");
                return;
            }
        };
        let live: HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
        let mut notified = self.notified_needs_human.lock().await;
        notified.retain(|id| live.contains(id));
        self.needs_human_retry
            .lock()
            .await
            .retain(|id, _| live.contains(id));

        for task in &tasks {
            if !task.goal_mode || notified.contains(&task.id) {
                continue;
            }
            let level = AutonomyLevel::for_agent(&self.home_dir, &task.assigned_to);
            if level == AutonomyLevel::Observer {
                // Observer: notify-only, no waiting — resolve straight to cancelled.
                match self
                    .store
                    .resolve_needs_human(
                        &task.id,
                        "abort",
                        "Observer 全自動模式:需人工需求自動結束",
                    )
                    .await
                {
                    Ok(_) => {
                        crate::goal_notify::notify_goal_observer(
                            &self.home_dir,
                            task,
                            "已自動結束 (cancelled)",
                        )
                        .await;
                        self.post_activity(
                            "goal_loop.observer_autoclose",
                            &task.assigned_to,
                            Some(&task.id),
                            &format!("Observer 模式:needs_human 自動結束 — {}", task.title),
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!(task = %task.id, error = %e, "goal loop: observer auto-close failed")
                    }
                }
                // Observer resolves the task out of needs_human synchronously
                // above (or logs+leaves it for a later retry on store error);
                // either way there is no channel-push retry state to track.
                notified.insert(task.id.clone());
            } else {
                // Operator/Collaborator/Consultant/Approver: push retry/done/abort
                // buttons to the agent control channel, and mirror a plain
                // heads-up to the goal's source conversation. A transient
                // SendFailed is retried (bounded) on a later tick instead of
                // being marked `notified` immediately — previously ANY
                // outcome (including a network blip) inserted into `notified`
                // unconditionally, so a failed push was never retried and the
                // human never learned the task was stuck.
                use crate::goal_notify::NotifyOutcome;
                let outcome =
                    crate::goal_notify::notify_goal_needs_human(&self.home_dir, task).await;
                match outcome {
                    // `Deferred` cannot occur here in practice — needs_human
                    // is L3 and quiet hours never hold it back — but it is
                    // handled explicitly rather than by a wildcard so that a
                    // future re-classification is a compile-time decision, not
                    // a silently-swallowed notification.
                    NotifyOutcome::Sent | NotifyOutcome::NoTarget | NotifyOutcome::Deferred => {
                        self.push_progress(
                            task,
                            "needs_human",
                            crate::goal_notify::GoalProgress::NeedsHuman,
                        )
                        .await;
                        self.needs_human_retry.lock().await.remove(&task.id);
                        let sent = outcome != NotifyOutcome::NoTarget;
                        self.post_activity(
                            "goal_loop.needs_human_notified",
                            &task.assigned_to,
                            Some(&task.id),
                            &format!(
                                "已推播需人工審批 — {}(推播{})",
                                task.title,
                                if sent {
                                    "成功"
                                } else {
                                    "無通知目標(設定缺漏)"
                                }
                            ),
                        )
                        .await;
                        notified.insert(task.id.clone());
                    }
                    NotifyOutcome::SendFailed => {
                        let mut retries = self.needs_human_retry.lock().await;
                        let count = retries.entry(task.id.clone()).or_insert(0);
                        *count += 1;
                        if *count >= NOTIFY_PUSH_MAX_RETRIES {
                            warn!(task = %task.id, attempts = *count,
                                  "goal loop: needs_human push failed after max retries, giving up");
                            retries.remove(&task.id);
                            drop(retries);
                            self.push_progress(
                                task,
                                "needs_human",
                                crate::goal_notify::GoalProgress::NeedsHuman,
                            )
                            .await;
                            self.post_activity(
                                "goal_loop.needs_human_notified",
                                &task.assigned_to,
                                Some(&task.id),
                                &format!("需人工審批推播多次失敗，放棄重試 — {}", task.title),
                            )
                            .await;
                            notified.insert(task.id.clone());
                        } else {
                            warn!(task = %task.id, attempt = *count,
                                  "goal loop: needs_human push failed, will retry next tick");
                            // Not inserted into `notified` — reconcile_needs_human
                            // retries this task again next tick.
                        }
                    }
                }
            }
        }
    }
}
