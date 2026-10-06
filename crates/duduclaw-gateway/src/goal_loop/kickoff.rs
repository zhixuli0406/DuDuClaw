//! The Collaborator/Consultant kickoff approval gate and the task-scoped
//! capability grants it hands out. Moved verbatim out of `goal_loop.rs`.

use super::*;

impl GoalLoopDriver {
    /// Kickoff gate for a Collaborator/Consultant goal task: on first sight,
    /// file a kickoff approval + push it to the channel and WAIT; on later ticks
    /// poll it — approved ⇒ proceed, denied/expired ⇒ abort the task.
    pub(super) async fn kickoff_gate(&self, task: &TaskRow) -> Result<KickoffGate, String> {
        let Some(broker) = &self.broker else {
            warn!(task = %task.id, "goal loop: kickoff requested but no ApprovalBroker; proceeding");
            return Ok(KickoffGate::Proceed);
        };
        let mut kickoff = self.kickoff.lock().await;
        // Reconstruct only this registered goal handler's exact task epoch.
        // A legacy row without the host epoch never gains resume authority.
        if !kickoff.contains_key(&task.id) {
            let snapshot = task.authority_snapshot_hash();
            if let Some(rec) = broker
                .list_by_kind("goal_kickoff")
                .await?
                .into_iter()
                .rev()
                .find(|r| {
                    r.payload["task_id"].as_str() == Some(task.id.as_str())
                        && r.payload["task_authority_revision"].as_i64()
                            == Some(task.authority_revision)
                        && r.payload["task_snapshot_hash"].as_str() == Some(snapshot.as_str())
                        && matches!(
                            r.status,
                            ApprovalStatus::Pending
                                | ApprovalStatus::Approved
                                | ApprovalStatus::Expired
                        )
                })
            {
                kickoff.insert(task.id.clone(), rec.id);
            }
        }
        match kickoff.get(&task.id).cloned() {
            None => {
                // First encounter: request approval, push, and wait.
                let summary = format!(
                    "目標:{} — 最多 {} 輪自主嘗試",
                    task.title,
                    self.iteration_cap_for(task)
                );
                let payload = json!({
                    "task_id": task.id,
                    "agent": task.assigned_to,
                    "task_authority_revision": task.authority_revision,
                    "task_snapshot_hash": task.authority_snapshot_hash(),
                    "resume_handler": "goal_kickoff_v1"
                });
                let id = broker
                    .request(
                        &task.assigned_to,
                        "goal_kickoff",
                        &summary,
                        payload,
                        KICKOFF_TTL_SECS,
                    )
                    .await?;
                kickoff.insert(task.id.clone(), id.clone());
                drop(kickoff);
                // The ApprovalBroker row above is already durably created —
                // a failed notification here must NOT be re-requested (that
                // would spam duplicate approvals); only the notification
                // itself is retried, via the Pending arm below on later ticks.
                self.notify_kickoff_with_retry(task, id.as_str(), &summary)
                    .await;
                self.post_activity(
                    "goal_loop.kickoff_requested",
                    &task.assigned_to,
                    Some(&task.id),
                    &format!("等待人工核准啟動自主目標 — {}", task.title),
                )
                .await;
                self.push_progress(task, "kickoff", crate::goal_notify::GoalProgress::Kickoff)
                    .await;
                Ok(KickoffGate::Waiting)
            }
            Some(id) => {
                let rec = broker.get(&id).await?.ok_or("kickoff approval missing")?;
                let current = self
                    .store
                    .get_task(&task.id)
                    .await?
                    .ok_or("kickoff task missing")?;
                if rec.payload["task_authority_revision"].as_i64() != Some(task.authority_revision)
                    || rec.payload["task_snapshot_hash"].as_str()
                        != Some(task.authority_snapshot_hash().as_str())
                    || current.authority_revision != task.authority_revision
                    || current.authority_snapshot_hash() != task.authority_snapshot_hash()
                    || !current.approval_eligible()
                {
                    kickoff.remove(&task.id);
                    let _ = broker
                        .invalidate_request(&id, "task_contract_changed")
                        .await;
                    return Ok(KickoffGate::Waiting);
                }
                let status = if rec
                    .expires_at_epoch()
                    .is_some_and(|t| chrono::Utc::now().timestamp() < t)
                {
                    broker.poll(&id).await?
                } else {
                    ApprovalStatus::Expired
                };
                match status {
                    ApprovalStatus::Approved => {
                        // Keep the (terminal-approved) approval in the map: if the
                        // dispatch is deferred this tick by the concurrency cap, the
                        // next tick re-polls the SAME approval (Approved) instead of
                        // filing a fresh one. Pruned once the task leaves candidates.
                        self.post_activity(
                            "goal_loop.kickoff_approved",
                            &task.assigned_to,
                            Some(&task.id),
                            &format!("人工已核准 — 開始自主執行 {}", task.title),
                        )
                        .await;
                        Ok(KickoffGate::Proceed)
                    }
                    ApprovalStatus::Pending => {
                        drop(kickoff);
                        // Retry a previously-failed notification (bounded) — the
                        // approval already exists, so this only re-sends the push.
                        if !self.kickoff_notified.lock().await.contains(&task.id) {
                            let summary = format!(
                                "目標:{} — 最多 {} 輪自主嘗試",
                                task.title,
                                self.iteration_cap_for(task)
                            );
                            self.notify_kickoff_with_retry(task, id.as_str(), &summary)
                                .await;
                        }
                        Ok(KickoffGate::Waiting)
                    }
                    // Denied / Expired (TTL = deny, fail-closed) ⇒ abort the goal.
                    other => {
                        kickoff.remove(&task.id);
                        let reason = format!("kickoff {} — 目標未啟動", other.as_str());
                        if let Err(e) = self.store.cancel_task(&task.id, &reason).await {
                            warn!(task = %task.id, error = %e, "goal loop: kickoff abort cancel failed");
                        }
                        // WP3 (PORTICO): task abandoned at kickoff → revoke any grants.
                        self.revoke_task_grants(
                            &task.id,
                            crate::capability_grants::REVOKE_REASON_PHASE_END,
                        )
                        .await;
                        self.post_activity(
                            "goal_loop.kickoff_denied",
                            &task.assigned_to,
                            Some(&task.id),
                            &format!("人工未核准({})— 目標放棄 {}", other.as_str(), task.title),
                        )
                        .await;
                        Ok(KickoffGate::Aborted)
                    }
                }
            }
        }
    }

    /// Push the kickoff approval, retrying a transient send failure (bounded)
    /// via [`kickoff_gate`]'s `Pending` poll branch on later ticks. The
    /// underlying `ApprovalBroker` row is already durably created by the
    /// caller — this only manages the notification's own delivery state.
    /// `NoTarget` / exhausted retries are treated as "handled" so the loop
    /// does not attempt the push forever.
    async fn notify_kickoff_with_retry(&self, task: &TaskRow, approval_id: &str, summary: &str) {
        use crate::goal_notify::NotifyOutcome;
        let outcome = crate::goal_notify::notify_goal_kickoff(
            &self.home_dir,
            &task.assigned_to,
            approval_id,
            summary,
        )
        .await;
        match outcome {
            // `Deferred` = the kickoff card is queued behind quiet hours (it
            // is L2). Handled, not lost: retrying would queue a duplicate.
            NotifyOutcome::Sent | NotifyOutcome::Deferred => {
                self.kickoff_notified.lock().await.insert(task.id.clone());
                self.kickoff_retry.lock().await.remove(&task.id);
            }
            NotifyOutcome::NoTarget => {
                warn!(task = %task.id, "goal loop: kickoff push has no notify target");
                self.kickoff_notified.lock().await.insert(task.id.clone());
                self.kickoff_retry.lock().await.remove(&task.id);
            }
            NotifyOutcome::SendFailed => {
                let mut retries = self.kickoff_retry.lock().await;
                let count = retries.entry(task.id.clone()).or_insert(0);
                *count += 1;
                if *count >= NOTIFY_PUSH_MAX_RETRIES {
                    warn!(task = %task.id, attempts = *count,
                          "goal loop: kickoff push failed after max retries, giving up");
                    retries.remove(&task.id);
                    drop(retries);
                    self.kickoff_notified.lock().await.insert(task.id.clone());
                } else {
                    warn!(task = %task.id, attempt = *count,
                          "goal loop: kickoff push failed, will retry next tick");
                }
            }
        }
    }

    /// Whether a real (non-test) home dir is wired. The driver defaults
    /// `home_dir` to `"."` in tests; touching the shared `approvals.db` under
    /// that sentinel would pollute the working tree, so all capability-grant
    /// side effects are gated on this.
    pub(super) fn has_real_home(&self) -> bool {
        self.home_dir != Path::new(".")
    }

    /// WP3 (PORTICO): revoke every capability grant bound to a task when the
    /// goal loop abandons it (kickoff denial → `cancel_task`). Best-effort;
    /// a store error just lets the grants die at their hard TTL.
    pub(super) async fn revoke_task_grants(&self, task_id: &str, reason: &str) {
        if !self.has_real_home() {
            return;
        }
        match crate::capability_grants::CapabilityGrantStore::open(&self.home_dir) {
            Ok(store) => {
                if let Err(e) = store.revoke_for_task(task_id, reason).await {
                    warn!(task = %task_id, error = %e, "goal loop: capability grant revoke failed");
                }
            }
            Err(e) => {
                warn!(task = %task_id, error = %e, "goal loop: capability grant store open failed for revoke")
            }
        }
    }

    /// WP3 (PORTICO): when a kickoff approval clears, atomically mint the
    /// task-scoped grants the task declared via `tags` entries of the form
    /// `grant:<tool>`. Idempotent per (task, tool): a grant already bound to
    /// THIS task for that tool is not re-minted (so a dispatch deferred by the
    /// concurrency cap, which re-enters this path next tick, does not stack
    /// duplicate rows). Best-effort + fail-safe: a store error is logged and
    /// the agent falls back to `capability_request`.
    pub(super) async fn grant_kickoff_tools(&self, task: &TaskRow) {
        if !self.has_real_home() {
            return;
        }
        let tools: Vec<String> = task
            .tags
            .split(',')
            .filter_map(|t| t.trim().strip_prefix("grant:"))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if tools.is_empty() {
            return;
        }
        let store = match crate::capability_grants::CapabilityGrantStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                warn!(task = %task.id, error = %e, "goal loop: grant store open failed for kickoff grants");
                return;
            }
        };
        let agent_dir = self.home_dir.join("agents").join(&task.assigned_to);
        let ttl = crate::capability_grants::grant_ttl_secs(&agent_dir);
        // Existing grants already bound to THIS task (for per-task idempotency).
        let existing = store
            .active_grants(&task.assigned_to)
            .await
            .unwrap_or_default();
        for tool in tools {
            let already = existing.iter().any(|g| {
                g.task_id.as_deref() == Some(task.id.as_str())
                    && crate::capability_grants::tool_token_matches(&g.tool, &tool)
            });
            if already {
                continue;
            }
            match store
                .grant(
                    &task.assigned_to,
                    Some(&task.id),
                    &tool,
                    crate::capability_grants::GRANTED_BY_KICKOFF,
                    ttl,
                )
                .await
            {
                Ok(grant_id) => {
                    duduclaw_security::audit::append_tool_call_with_extras(
                        &self.home_dir,
                        &task.assigned_to,
                        "capability_request",
                        &format!("kickoff grant {tool}"),
                        true,
                        &[
                            ("grant_id", json!(grant_id)),
                            ("granted_tool", json!(tool)),
                            ("task_id", json!(task.id)),
                            (
                                "granted_by",
                                json!(crate::capability_grants::GRANTED_BY_KICKOFF),
                            ),
                        ],
                    );
                    info!(task = %task.id, %tool, "goal loop: kickoff-approved capability grant minted");
                }
                Err(e) => {
                    warn!(task = %task.id, %tool, error = %e, "goal loop: kickoff grant write failed")
                }
            }
        }
    }
}
