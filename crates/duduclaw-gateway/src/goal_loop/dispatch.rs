//! Lease release, dispatch-failure backoff and stale-lease sweeps.
//! Moved verbatim out of `goal_loop.rs`.

use super::*;

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
        if let Some(removed) = inflight.remove(task_id) {
            self.release_lease(&removed);
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
