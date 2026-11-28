//! Lease release, dispatch-failure backoff and stale-lease sweeps.
//! Moved verbatim out of `goal_loop.rs`.

use super::*;

/// Only the typed tick paths may carry an in-process configuration refusal.
/// Durable queue text must always be revalidated against current authority.
#[derive(Clone, Copy)]
pub(super) enum DispatchFailure<'a> {
    Persisted(&'a str),
    Entry(&'a crate::entry_gate::EntryRefusal),
    TeamConfig(&'a str),
}

impl GoalLoopDriver {
    /// RFC-27: release the edition concurrency lease a terminal task held.
    /// No-op when the gate did not apply (unguarded / `None`). Best-effort — the
    /// lease TTL reclaims the slot even if the file write fails.
    pub(super) fn release_lease(&self, entry: &InFlight) {
        if let Some(lease) = &entry.lease {
            duduclaw_core::concurrency_release(&self.home_dir, lease);
        }
    }

    /// If the tracked round for `task_id` is still awaiting pickup and its
    /// work message is marked `failed` in the queue, return the error text.
    pub(super) async fn dispatch_failure_for(
        &self,
        inflight: &HashMap<String, InFlight>,
        task_id: &str,
    ) -> Option<String> {
        let entry = inflight.get(task_id)?;
        if !entry.awaiting_pickup {
            return None;
        }
        let message_id = entry.message_id.as_deref()?;
        let msg = self.queue.get_by_id(message_id).await.ok().flatten()?;
        if msg.status != MessageStatus::Failed {
            return None;
        }
        Some(msg.error.unwrap_or_else(|| "dispatch failed".to_string()))
    }

    /// A round's work message failed before any agent ran it. Free the slot
    /// and the edition lease immediately, record a back-off so the task is
    /// not re-dispatched every tick, and after [`DISPATCH_FAILURE_LIMIT`]
    /// consecutive failures park the task `needs_human` with the error —
    /// the person can then fix the runtime / local model / credentials and
    /// resume, instead of the loop silently retrying into the same wall.
    pub(super) async fn on_dispatch_failed(
        &self,
        inflight: &mut HashMap<String, InFlight>,
        task_id: &str,
        error: &str,
    ) -> Result<bool, String> {
        self.handle_dispatch_failure(inflight, task_id, DispatchFailure::Persisted(error)).await
    }

    pub(super) async fn handle_dispatch_failure(
        &self,
        inflight: &mut HashMap<String, InFlight>,
        task_id: &str,
        failure: DispatchFailure<'_>,
    ) -> Result<bool, String> {
        let entry_error;
        let error = match failure {
            DispatchFailure::Persisted(error) | DispatchFailure::TeamConfig(error) => error,
            DispatchFailure::Entry(refusal) => {
                entry_error = crate::entry_gate::background_refusal(refusal);
                &entry_error
            }
        };
        let persisted = matches!(failure, DispatchFailure::Persisted(_));
        if let Some(outcome) = crate::work_outcome::WorkOutcome::decode(error).filter(|_| persisted).or_else(|| {
            (persisted && crate::work_outcome::is_work_result(error)).then_some(crate::work_outcome::WorkOutcome::TimeoutTotal { seconds: 0, partial: String::new() })
        }) {
            let pause = match outcome {
                crate::work_outcome::WorkOutcome::MaxTurns { .. } => crate::pause_reason::PauseReason::MaxTurns,
                crate::work_outcome::WorkOutcome::TimeoutTotal { .. } => crate::pause_reason::PauseReason::TimeoutTotal,
                crate::work_outcome::WorkOutcome::TimeoutNoProgress { .. } => crate::pause_reason::PauseReason::TimeoutNoProgress,
            };
            if let Some(removed) = inflight.remove(task_id) { self.release_lease(&removed); }
            self.dispatch_failures.lock().await.remove(task_id);
            let Some(task) = self.store.get_task(task_id).await? else { return Ok(false); };
            self.escalate(inflight, &task, &crate::channel_reply::failure_notice::work_notice(&outcome, None), pause).await?;
            return Ok(true);
        }
        if let Some(removed) = inflight.remove(task_id) {
            self.release_lease(&removed);
            // E-M2 / H-1: a round that failed before any agent ran it gives
            // its directions back and its intent is abandoned (no iteration,
            // no presumed spend). Only fixed-id durable rounds have intents.
            let msg_id = removed
                .message_id
                .clone()
                .unwrap_or_else(|| format!("goal:{task_id}:{}", removed.iter));
            if let Err(e) = self.store.return_unrun_round(&msg_id, Utc::now()).await {
                warn!(task = %task_id, error = %e, "could not return undelivered directions");
            }
        }
        // P2-A: the dispatcher fenced a stale durable `goal:` round (task
        // stopped, paused or moved on). Nothing failed to run — free the slot
        // without counting a dispatch failure. Only fixed-id durable rounds
        // can carry this text.
        if persisted && error.starts_with(crate::responsibility::FENCE_ERROR_PREFIX) {
            debug!(task = %task_id, "goal loop: durable round fenced by the dispatcher — slot released");
            return Ok(false);
        }
        // Only persisted failures need a fresh assignee check. A typed refusal
        // computed by this tick also covers a frozen role, even if the durable
        // assignee itself is healthy. Never authenticate queue text by prefix.
        if let Some(task) = self.store.get_task(task_id).await? {
            let refusal = match failure {
                DispatchFailure::Persisted(_) => {
                    crate::entry_gate::check_task_assignee(&self.home_dir, &task.assigned_to)
                        .err().map(|refusal| crate::entry_gate::background_refusal(&refusal))
                }
                DispatchFailure::Entry(_) | DispatchFailure::TeamConfig(_) => Some(error.to_owned()),
            };
            if let Some(error) = refusal {
                let code = crate::entry_gate::background_refusal_code(&error).unwrap_or("team_config_refused");
                self.dispatch_failures.lock().await.remove(task_id);
                self.escalate(inflight, &task, &error,
                    crate::pause_reason::PauseReason::BlockedNeedsDecision).await?;
                self.post_activity("goal_loop.entry_refused", &task.assigned_to, Some(task_id),
                    &format!("blocked_needs_decision: {code}")).await;
                return Ok(true);
            }
        }
        // Admission may change after enqueue but before the runtime handoff.
        // These refusals are not infrastructure failures and must not enter
        // the failure counter/backoff ladder or spend another dispatch round.
        let refusal = crate::budget::budget_refusal_message(error)
            .map(|message| (message, crate::pause_reason::PauseReason::BlockedNeedsDecision))
            .or_else(|| error.starts_with("AUTONOMY_PAUSED:")
                .then_some((error, crate::pause_reason::PauseReason::OperatorPaused)));
        if let Some((message, pause)) = refusal {
            self.dispatch_failures.lock().await.remove(task_id);
            let Some(task) = self.store.get_task(task_id).await? else {
                return Ok(false);
            };
            self.escalate(inflight, &task, message, pause).await?;
            return Ok(true);
        }
        let now = Utc::now();
        let failures = {
            let mut map = self.dispatch_failures.lock().await;
            let entry = map.entry(task_id.to_string()).or_insert((0, now));
            entry.0 += 1;
            entry.1 = now + chrono::Duration::seconds(dispatch_backoff_secs(entry.0));
            entry.0
        };
        let short_error: String = error.chars().take(200).collect();
        warn!(
            task = %task_id,
            failures,
            limit = DISPATCH_FAILURE_LIMIT,
            error = %short_error,
            "goal loop: work message failed to dispatch — slot released"
        );
        let Some(task) = self.store.get_task(task_id).await? else {
            return Ok(false);
        };
        if failures >= DISPATCH_FAILURE_LIMIT {
            self.post_activity(
                "goal_loop.dispatch_failed",
                &task.assigned_to,
                Some(task_id),
                &format!(
                    "goal-loop 連續 {failures} 次派工失敗，轉人工 — {}：{short_error}",
                    task.title
                ),
            )
            .await;
            self.dispatch_failures.lock().await.remove(task_id);
            self.escalate(
                inflight,
                &task,
                &format!("goal-loop dispatch failed {failures}x: {short_error}"),
                crate::pause_reason::PauseReason::Infra,
            )
            .await?;
            return Ok(true);
        } else {
            self.post_activity(
                "goal_loop.dispatch_failed",
                &task.assigned_to,
                Some(task_id),
                &format!(
                    "goal-loop 派工失敗（第 {failures}/{DISPATCH_FAILURE_LIMIT} 次，{} 秒後重試）— {}：{short_error}",
                    dispatch_backoff_secs(failures),
                    task.title
                ),
            )
            .await;
        }
        Ok(false)
    }

    /// `true` while `task_id` is inside its dispatch-failure back-off window.
    pub(super) async fn in_dispatch_backoff(&self, task_id: &str, now: DateTime<Utc>) -> bool {
        let map = self.dispatch_failures.lock().await;
        map.get(task_id)
            .is_some_and(|(_, not_before)| now < *not_before)
    }

    /// Restart recovery for the edition concurrency gate: this driver is the
    /// only holder of the `goal` lease class and its in-memory in-flight map
    /// is empty at start, so every lease still in the file is an orphan of a
    /// previous process. Dropping them is what keeps a restart from deferring
    /// every new goal task for the lease TTL.
    pub(super) fn release_stale_goal_leases(&self) {
        if self.concurrency_limit.is_none() {
            return;
        }
        let dropped =
            duduclaw_core::concurrency_release_class(&self.home_dir, CONCURRENCY_CLASS_GOAL);
        if dropped > 0 {
            info!(
                dropped,
                "goal loop: released orphaned edition concurrency leases from a previous process"
            );
        }
    }
}

/// Exercise the real failed-queue reconciliation without invoking the planner.
#[cfg(test)]
pub(crate) async fn assert_entry_refusal_tick(
    home: &Path, store: Arc<crate::task_store::TaskStore>, error: &str,
) {
    let code = crate::entry_gate::background_refusal_code(error).unwrap();
    let mut task = crate::task_store::TaskRow::new("entry-goal".into(), "Entry gate".into(),
        "work".into(), "medium".into(), "worker".into(), "system".into());
    task.goal_mode = true;
    task.status = "todo".into();
    store.insert_task(&task).await.unwrap();
    let queue = Arc::new(MessageQueue::open(home).unwrap());
    let message_id = super::enqueue::enqueue_goal_work(&queue, &store, &task, 1, "").await.unwrap();
    queue.fail(&message_id, error).await.unwrap();
    let driver = GoalLoopDriver::new(store.clone(), queue.clone(), GoalLoopConfig::default())
        .with_home_dir(home.to_path_buf());
    driver.inflight.lock().await.insert(task.id.clone(), InFlight {
        iter: 1, enqueued_at: Utc::now(), awaiting_pickup: true, lease: None,
        message_id: Some(message_id), progress_reported_round: None,
    });
    driver.tick_once().await.unwrap();
    let task = store.get_task(&task.id).await.unwrap().unwrap();
    assert_eq!(task.status, "needs_human");
    assert_eq!(task.pause_reason.as_deref(), Some("blocked_needs_decision"));
    assert!(store.list_activity_for_task(&task.id, 100).await.unwrap().iter().any(|row|
        row.event_type == "goal_loop.entry_refused" && row.summary == format!("blocked_needs_decision: {code}")));
    assert!(driver.inflight.lock().await.is_empty());
    assert!(driver.dispatch_failures.lock().await.is_empty());
    driver.tick_once().await.unwrap();
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
    assert!(driver.dispatch_failures.lock().await.is_empty());
    assert_eq!(store.get_task(&task.id).await.unwrap().unwrap().status, "needs_human");
}
