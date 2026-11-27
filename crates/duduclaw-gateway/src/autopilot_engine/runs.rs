//! Durable, gateway-owned multi-step execution. A wait is a persisted deadline,
//! never a sleeping task. External notifications with uncertain delivery stop.

use super::*;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crate::autopilot_approval::{event_ref, rule_revision_hash, run_dedupe_key};
use crate::autopilot_store::{
    AutopilotRunRow, AutopilotRunStepRow, RunInsertOutcome, RunNext,
    run_ts, step_idempotency_key, RUN_PENDING, RUN_RUNNING, RUN_AWAITING_APPROVAL,
    STEP_PENDING, STEP_RUNNING, STEP_DONE, STEP_FAILED, STEP_SKIPPED,
};
use crate::autopilot_window::{rule_window_decision, WindowDecision};

static RUN_LEASE_OWNER: OnceLock<String> = OnceLock::new();

pub(super) fn lease_owner() -> &'static str {
    RUN_LEASE_OWNER.get_or_init(|| format!("{}:{}", std::process::id(), uuid::Uuid::new_v4()))
}

pub(super) fn queue_id(run_id: &str, index: i64) -> String {
    format!("ap-{}", duduclaw_core::truncate_chars(&step_idempotency_key(run_id, index), 32))
}

fn steps_action(rule: &AutopilotRuleRow) -> Result<Vec<Value>, String> {
    let action: Value = serde_json::from_str(&rule.action).map_err(|e| format!("invalid action: {e}"))?;
    if action.get("type").and_then(Value::as_str) != Some("steps") {
        return Err("steps envelope required".into());
    }
    // Reuse write validation: corrupt/legacy rows do not gain runtime authority.
    crate::handlers::validate_autopilot_action(&action)?;
    action.get("steps").and_then(Value::as_array).cloned().ok_or("steps array required".into())
}

impl AutopilotEngine {
    pub(super) async fn start_run(
        &self, rule: &AutopilotRuleRow, event_name: &str,
        fields: &serde_json::Map<String, Value>, now: DateTime<Utc>,
    ) -> Result<(), String> {
        let actions = match steps_action(rule) {
            Ok(actions) => actions,
            Err(detail) => {
                return self.store.append_step_history(&AutopilotHistoryRow {
                    id: uuid::Uuid::new_v4().to_string(), rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(), triggered_at: run_ts(now),
                    result: "invalid_action".into(), details: Some(detail),
                }).await;
            }
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        let rule_hash = rule_revision_hash(rule);
        let run = AutopilotRunRow {
            dedupe_key: run_dedupe_key(&rule.id, &rule_hash, event_name, fields),
            event_ref: event_ref(event_name, fields), run_id: run_id.clone(),
            rule_id: rule.id.clone(), rule_name: rule.name.clone(), rule_hash,
            event_name: event_name.into(), fields_json: serde_json::to_string(fields).map_err(|e| e.to_string())?,
            state: RUN_PENDING.into(), step_index: 0, step_count: actions.len() as i64,
            next_at: None, lease_owner: None, lease_until: None,
            owner_department: rule.owner_department.clone(), outcome: None, detail: None,
            created_at: run_ts(now), updated_at: run_ts(now),
            notice_pending: false, notice_push_state: None, notice_push_attempts: 0, notice_push_next_at: None,
        };
        let steps: Vec<_> = actions.iter().enumerate().map(|(i, action)| AutopilotRunStepRow {
            run_id: run_id.clone(), step_index: i as i64,
            step_type: action["type"].as_str().unwrap_or_default().into(), state: STEP_PENDING.into(),
            idempotency_key: step_idempotency_key(&run_id, i as i64), dispatch_id: None,
            queue_message_id: None, started_at: None, finished_at: None, detail: None,
        }).collect();
        match self.store.insert_run(&run, &steps, now).await? {
            RunInsertOutcome::Inserted => {
                // The only fire-counter increment for an admitted steps run.
                self.store.append_history(&AutopilotHistoryRow {
                    id: format!("run:{run_id}:started"), rule_id: rule.id.clone(), rule_name: rule.name.clone(),
                    triggered_at: run_ts(now), result: "run_started".into(),
                    details: Some(format!("run {run_id}: {} steps ({event_name})", steps.len())),
                }).await?;
                if duduclaw_core::gateway_instance::held(&self.home_dir)
                    && self.store.claim_run(&run_id, lease_owner(), now).await?
                {
                    self.advance_run(&run_id, now).await?;
                }
            }
            RunInsertOutcome::Duplicate => debug!(%run_id, "autopilot: duplicate event run ignored"),
            outcome => {
                let result = match outcome {
                    RunInsertOutcome::RuleLimit | RunInsertOutcome::GlobalLimit => "run_limit",
                    RunInsertOutcome::FieldsTooLarge => "fields_too_large",
                    RunInsertOutcome::Inserted | RunInsertOutcome::Duplicate => unreachable!(),
                };
                self.store.append_step_history(&AutopilotHistoryRow {
                    id: format!("run:{run_id}:rejected"), rule_id: rule.id.clone(), rule_name: rule.name.clone(),
                    triggered_at: run_ts(now), result: result.into(), details: Some(format!("{outcome:?}")),
                }).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn resume_runs_once(&self, now: DateTime<Utc>) -> Result<(), String> {
        // Defense in depth for callers outside the main resume tick.
        if !duduclaw_core::gateway_instance::held(&self.home_dir) {
            return Ok(());
        }
        self.reconcile_approval_runs_once(now).await?;
        self.announce_run_notices_once(now).await?;
        for run in self.store.list_unsupported_runs(50).await? {
            if self.store.settle_unclaimed_run(&run.run_id, &[&run.state], "unsupported_state", None, now).await? {
                self.announce_run_notices_once(now).await?;
            }
        }
        for id in self.store.list_expired_running_runs(now, 50).await? {
            if let Err(error) = self.recover_run(&id, now).await {
                warn!(run_id = %id, %error, "autopilot: run recovery failed");
            }
        }
        for id in self.store.list_due_runs(now, 50).await? {
            match self.store.claim_run(&id, lease_owner(), now).await {
                Ok(true) => {
                    if let Err(error) = self.advance_run(&id, now).await {
                        warn!(run_id = %id, %error, "autopilot: run advancement failed");
                    }
                }
                Ok(false) => {}
                Err(error) => warn!(run_id = %id, %error, "autopilot: run claim failed"),
            }
        }
        Ok(())
    }

    /// Durable bridge from the approval ledger to the run state machine.
    /// A crash after dispatch settlement is repaired by the next lock-holder tick.
    pub(super) async fn reconcile_approval_runs_once(&self, now: DateTime<Utc>) -> Result<(), String> {
        use crate::autopilot_approval as gate;
        use crate::autopilot_store::{DISPATCH_AWAITING, DISPATCH_EXECUTING, DISPATCH_REQUESTING, DISPATCH_SETTLED};
        if !duduclaw_core::gateway_instance::held(&self.home_dir) { return Ok(()); }
        for run in self.store.list_awaiting_approval_runs().await? {
            let steps = self.store.list_run_steps(&run.run_id).await?;
            let step = steps.iter().find(|step| step.step_index == run.step_index);
            let dispatch = match step.and_then(|step| step.dispatch_id.as_deref()) {
                Some(id) => self.store.get_dispatch(id).await?,
                None => None,
            };
            let valid = dispatch.as_ref().filter(|row| {
                row.run_id.as_deref() == Some(run.run_id.as_str()) && row.step_index == Some(run.step_index)
                    && row.rule_id == run.rule_id && row.rule_hash == run.rule_hash
                    && step.is_some_and(|step| step.state == STEP_RUNNING)
            });
            let (outcome, detail) = match valid {
                Some(row) if row.state == DISPATCH_SETTLED => {
                    let detail = row.detail.as_deref();
                    let outcome = match row.outcome.as_deref() {
                        Some(gate::RESULT_SUCCESS) => "completed",
                        Some(gate::RESULT_DENIED) => "denied",
                        Some(gate::RESULT_EXPIRED) => "expired",
                        Some(gate::RESULT_STALE) => "stale_rule_changed",
                        Some(gate::RESULT_BREAKER_OPEN) => "blocked_breaker_open",
                        Some(gate::RESULT_SKIPPED) if detail == Some("scope_denied") => "scope_denied",
                        Some(gate::RESULT_SKIPPED) => "autonomy_paused",
                        Some(gate::RESULT_FAILURE) => "step_failed",
                        Some(gate::RESULT_UNCERTAIN) => "execution_uncertain",
                        _ => "approval_error",
                    };
                    (outcome, detail)
                }
                Some(row) if matches!(row.state.as_str(), DISPATCH_REQUESTING | DISPATCH_AWAITING | DISPATCH_EXECUTING) => continue,
                _ => ("approval_error", Some("核准紀錄不存在或與流程步驟不符")),
            };
            // Stable ID makes re-driving a crash between this audit and the
            // run CAS harmless; neither write increments trigger_count.
            self.store.append_step_history(&AutopilotHistoryRow {
                id: format!("run:{}:step:{}", run.run_id, run.step_index),
                rule_id: run.rule_id.clone(), rule_name: run.rule_name.clone(), triggered_at: run_ts(now),
                result: if outcome == "completed" { "step_done" } else { "step_failed" }.into(),
                details: detail.map(str::to_owned),
            }).await?;
            if outcome == "completed" {
                let row = valid.expect("success requires a bound dispatch");
                self.store.resume_after_approval(&run.run_id, run.step_index, &row.dispatch_id, now).await?;
            } else {
                self.store.settle_unclaimed_run(&run.run_id, &[RUN_AWAITING_APPROVAL], outcome, detail, now).await?;
            }
        }
        self.announce_run_notices_once(now).await
    }

    async fn recover_run(&self, id: &str, now: DateTime<Utc>) -> Result<(), String> {
        if !self.store.reclaim_expired_run(id, lease_owner(), now).await? {
            return Ok(());
        }
        let run = self.store.get_run(id).await?.ok_or("reclaimed run missing")?;
        if let Some(dispatch) = self.store.get_step_dispatch(id, run.step_index).await? {
            if dispatch.state == crate::autopilot_store::DISPATCH_SETTLED
                && dispatch.outcome.as_deref() == Some(crate::autopilot_approval::RESULT_APPROVAL_ERROR)
            {
                // Filing recovery could not bind this run. Once its lease is
                // reclaimable, settle it without rendering or filing again.
                if self.finish_run_step(&run, STEP_FAILED, dispatch.detail.as_deref(), None,
                    RunNext::Settle { outcome: "approval_error".into(), detail: dispatch.detail.clone() }, now)
                    .await?
                {
                    return Ok(());
                }
                // A missing/non-running step can itself make binding impossible.
                return self.settle_run(&run, "approval_error", dispatch.detail.as_deref(), now).await;
            }
        }
        let steps = self.store.list_run_steps(id).await?;
        if let Some(step) = steps.iter().find(|s| s.step_index == run.step_index) {
            if step.state == STEP_RUNNING
                && matches!(step.step_type.as_str(), "notify" | "proactive_notify")
            {
                let gated = self.store.get_rule(&run.rule_id).await?
                    .and_then(|rule| steps_action(&rule).ok())
                    .and_then(|steps| usize::try_from(run.step_index).ok().and_then(|i| steps.get(i).cloned()))
                    .is_some_and(|step| crate::approval::rule_requires_approval(&step));
                if !gated {
                    return self.settle_run(&run, "execution_uncertain", None, now).await;
                }
            }
        }
        // Pending steps advance normally. Running queue actions are reconciled
        // in advance_run, after checking the rule and exact rendered delivery.
        self.advance_run(id, now).await
    }

    /// Preparation must precede this fence: prepare_run_skill reads the store.
    /// Only the fence's own connection may write the point of no return here.
    async fn checked_run_step(
        &self, run: &AutopilotRunRow, fields: &serde_json::Map<String, Value>, now: DateTime<Utc>,
        wait_deadline: Option<DateTime<Utc>>, reconcile_only: bool,
    ) -> Result<Result<bool, (String, String)>, String> {
        let (fence, rule) = self.store.fence_rule(&run.rule_id).await?;
        let checked: Result<(), (String, String)> = async {
            let rule = rule.ok_or_else(|| ("rule_removed".into(), "規則已刪除".into()))?;
            if !rule.enabled {
                return Err(("rule_disabled".into(), "規則已停用".into()));
            }
            if rule_revision_hash(&rule) != run.rule_hash || rule.owner_department != run.owner_department {
                return Err(("stale_rule_changed".into(), "規則在等待期間被修改，後續步驟已取消".into()));
            }
            if self.breaker_blocks_execution(&run.rule_id).await {
                return Err(("blocked_breaker_open".into(), "規則斷路器已開啟".into()));
            }
            let steps = steps_action(&rule).map_err(|e| ("invalid_action".into(), e))?;
            if run.step_count != steps.len() as i64 {
                return Err(("invalid_action".into(), "stored step count mismatch".into()));
            }
            let step = usize::try_from(run.step_index).ok().and_then(|i| steps.get(i)).cloned()
                .ok_or_else(|| ("invalid_action".into(), "stored step index invalid".into()))?;
            self.autonomy_check(&step, fields).map_err(|e| ("autonomy_paused".into(), e))?;
            match rule_window_decision(&self.home_dir, &rule, now) {
                WindowDecision::Inside => {}
                WindowDecision::Outside { detail } => return Err(("outside_window".into(), detail)),
                WindowDecision::ZoneUnresolved { detail } => return Err(("outside_window_tz_unresolved".into(), detail)),
                WindowDecision::Invalid { detail } => return Err(("active_window_invalid".into(), detail)),
            }
            if let crate::autopilot_scope::ScopeDecision::Deny { reason } =
                crate::autopilot_scope::check(&self.home_dir, &rule, Some(&step), fields)
            {
                return Err(("scope_denied".into(), reason));
            }
            Ok(())
        }.await;
        if let Err(denied) = checked {
            // Roll back and release the mutex before the caller settles the run.
            drop(fence);
            return Ok(Err(denied));
        }
        let changed = if let Some(next_at) = wait_deadline {
            fence.complete_wait_step(&run.run_id, run.step_index, lease_owner(), next_at, now)?
        } else if reconcile_only {
            true
        } else {
            fence.mark_step_running(&run.run_id, run.step_index, lease_owner(), now)?
        };
        // A failed COMMIT must never authorize a side effect. Propagate the
        // storage error and leave the lease/state for recovery, not settlement.
        fence.release()?;
        Ok(Ok(changed))
    }

    async fn advance_run(&self, id: &str, now: DateTime<Utc>) -> Result<(), String> {
        // Valid envelopes contain at most five steps; malformed persistence must
        // never turn this into an unbounded loop.
        for _ in 0..5 {
            if !duduclaw_core::gateway_instance::held(&self.home_dir) {
                return Ok(());
            }
            let run = self.store.get_run(id).await?.ok_or("claimed run missing")?;
            if run.state != RUN_RUNNING || run.lease_owner.as_deref() != Some(lease_owner()) {
                return Ok(());
            }
            let fields: serde_json::Map<String, Value> = match serde_json::from_str(&run.fields_json) {
                Ok(fields) => fields,
                Err(_) => return self.settle_run(&run, "fields_unreadable", None, now).await,
            };
            let rule = match self.store.get_rule(&run.rule_id).await? {
                Some(rule) => rule,
                None => return self.settle_run(&run, "rule_removed", None, now).await,
            };
            if !rule.enabled {
                return self.settle_run(&run, "rule_disabled", None, now).await;
            }
            if rule_revision_hash(&rule) != run.rule_hash || rule.owner_department != run.owner_department {
                return self.settle_run(&run, "stale_rule_changed", None, now).await;
            }
            let actions = match steps_action(&rule) {
                Ok(actions) if actions.len() as i64 == run.step_count => actions,
                _ => return self.settle_run(&run, "invalid_action", None, now).await,
            };
            let action = match usize::try_from(run.step_index).ok().and_then(|i| actions.get(i)) {
                Some(action) => action,
                None => return self.settle_run(&run, "invalid_action", None, now).await,
            };
            let rows = self.store.list_run_steps(id).await?;
            let Some(step) = rows.iter().find(|s| s.step_index == run.step_index) else {
                return self.settle_run(&run, "invalid_state", Some("step missing"), now).await;
            };
            if rows.len() as i64 != run.step_count
                || step.step_type != action["type"].as_str().unwrap_or_default()
                || step.idempotency_key != step_idempotency_key(id, run.step_index)
            {
                return self.settle_run(&run, "invalid_state", Some("step identity mismatch"), now).await;
            }
            // An interrupted legacy wait uses its own started_at deadline,
            // not the wake-up timestamp left by an earlier wait.
            if !(step.state == STEP_RUNNING && step.step_type == "wait") {
                if let Some(next_at) = &run.next_at {
                    match DateTime::parse_from_rfc3339(next_at) {
                        Ok(due) if now.signed_duration_since(due.with_timezone(&Utc)) > ChronoDuration::minutes(60) => {
                            return self.settle_run(&run, "missed_late", None, now).await;
                        }
                        Ok(_) => {}
                        Err(_) => return self.settle_run(&run, "invalid_state", Some("invalid next_at"), now).await,
                    }
                }
            }
            let gated = crate::approval::rule_requires_approval(action);
            let queue_action = matches!(step.step_type.as_str(), "delegate" | "run_skill");
            let message_id = queue_id(id, run.step_index);
            let wait_deadline = if step.step_type == "wait" {
                let minutes = action["minutes"].as_i64().filter(|n| (1..=1440).contains(n))
                    .ok_or("invalid wait minutes")?;
                let started = if step.state == STEP_RUNNING {
                    match step.started_at.as_deref().and_then(|s| DateTime::parse_from_rfc3339(s).ok()) {
                        Some(started) => started.with_timezone(&Utc),
                        None => return self.settle_run(&run, "invalid_state", Some("invalid wait started_at"), now).await,
                    }
                } else { now };
                let deadline = started.checked_add_signed(ChronoDuration::minutes(minutes)).ok_or("wait timestamp overflow")?;
                if now.signed_duration_since(deadline) > ChronoDuration::minutes(60) {
                    return self.settle_run(&run, "missed_late", None, now).await;
                }
                Some(deadline)
            } else { None };
            let perception = sanitize_perception_fields(&fields, &self.home_dir);
            let effective = perception.as_ref().map(|(f, _)| f).unwrap_or(&fields);
            let banner = perception.as_ref().and_then(|(_, b)| b.as_deref());
            let prepared = if wait_deadline.is_some() {
                Ok(None)
            } else {
                self.prepare_action(&rule.id, &rule.name, action, &fields, effective, banner).await
            };
            // Reuse the approval executor's one-shot race seam after rendering.
            #[cfg(test)]
            {
                let hook = self.after_claim_hook.lock().unwrap_or_else(|p| p.into_inner()).take();
                if let Some(hook) = hook { hook(); }
            }
            if gated {
                if !matches!(step.state.as_str(), STEP_PENDING | STEP_RUNNING) {
                    return self.settle_run(&run, "invalid_state", Some("unexpected gated step state"), now).await;
                }
                match self.checked_run_step(&run, &fields, now, None, step.state == STEP_RUNNING).await? {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err((outcome, detail)) => return self.settle_run(&run, &outcome, Some(&detail), now).await,
                }
                match prepared {
                    Ok(None) => {
                        if !self.finish_run_step(&run, STEP_SKIPPED, None, None, Self::next_run_step(&run), now).await? {
                            return Ok(());
                        }
                        if !self.store.claim_run(id, lease_owner(), now).await? { return Ok(()); }
                        continue;
                    }
                    Err(detail) => {
                        self.finish_run_step(&run, STEP_FAILED, Some(&detail), None,
                            RunNext::Settle { outcome: "step_failed".into(), detail: Some(detail.clone()) }, now).await?;
                    }
                    Ok(Some(frozen)) => {
                        let failure = if self.approval_broker.is_none() {
                            Some((crate::autopilot_approval::RESULT_BLOCKED,
                                "需要核准，但核准服務沒有接上；動作沒有執行".to_string()))
                        } else {
                            self.gate_step_on_approval(&rule, &run, frozen, &fields, lease_owner(), now).await
                                .err().map(|detail| (crate::autopilot_approval::RESULT_APPROVAL_ERROR, detail))
                        };
                        if let Some((outcome, detail)) = failure {
                            self.approval_notice(&run.rule_id, &run.rule_name, outcome, &detail).await;
                            self.finish_run_step(&run, STEP_FAILED, Some(&detail), None,
                                RunNext::Settle { outcome: outcome.into(), detail: Some(detail.clone()) }, now).await?;
                        }
                    }
                }
                return Ok(());
            }
            let mut reconciled = None;
            if step.state == STEP_RUNNING && wait_deadline.is_none() {
                if !queue_action {
                    let outcome = if matches!(step.step_type.as_str(), "notify" | "proactive_notify") {
                        "execution_uncertain"
                    } else { "unsupported_state" };
                    return self.settle_run(&run, outcome, None, now).await;
                }
                let queue = self.message_queue.as_ref().ok_or("message queue not available")?;
                match queue.get_by_id(&message_id).await? {
                    Some(existing) => {
                        use crate::autopilot_approval::FrozenAction;
                        // `prepared` was rendered above from the persisted fields,
                        // using the same sanitization and banners as a new step.
                        reconciled = Some(match &prepared {
                            Ok(Some(FrozenAction::Delegate { target_agent, prompt, lane }))
                            | Ok(Some(FrozenAction::RunSkill { target_agent, prompt, lane, .. })) => {
                                let expected = Self::prompt_queue_message(target_agent, prompt,
                                    lane.clone(), Some(&message_id), now);
                                if existing.matches_enqueue(&expected) { Ok(()) }
                                else { Err("queue id delivery mismatch".to_string()) }
                            }
                            Err(error) => Err(error.clone()),
                            _ => Err("queue recovery could not render a delivery".to_string()),
                        });
                    }
                    None => {
                        if !self.store.reset_running_step(id, run.step_index, lease_owner(), now).await? {
                            return Ok(());
                        }
                    }
                }
            } else if step.state != STEP_PENDING && !(step.state == STEP_RUNNING && wait_deadline.is_some()) {
                return self.settle_run(&run, "unsupported_state", Some("unexpected current step state"), now).await;
            }
            match self.checked_run_step(&run, &fields, now, wait_deadline, reconciled.is_some()).await {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => return Ok(()),
                Ok(Err((outcome, detail))) => return self.settle_run(&run, &outcome, Some(&detail), now).await,
                Err(error) => {
                    warn!(run_id = id, %error, "autopilot: fenced step transition failed; left for recovery");
                    return Err(error);
                }
            }
            if wait_deadline.is_some() {
                self.store.append_step_history(&AutopilotHistoryRow {
                    id: format!("run:{}:step:{}", run.run_id, run.step_index),
                    rule_id: run.rule_id.clone(), rule_name: run.rule_name.clone(), triggered_at: run_ts(now),
                    result: "step_done".into(), details: None,
                }).await?;
                return Ok(());
            }
            if let Some(verified) = reconciled {
                if let Err(detail) = verified {
                    return self.finish_run_step(&run, STEP_FAILED, Some(&detail), None,
                        RunNext::Settle { outcome: "step_failed".into(), detail: Some(detail.clone()) }, now).await.map(|_| ());
                }
                if !self.finish_run_step(&run, STEP_DONE, None, Some(&message_id), Self::next_run_step(&run), now).await? {
                    return Ok(());
                }
                if !self.store.claim_run(id, lease_owner(), now).await? { return Ok(()); }
                continue;
            }
            let queue_message_id = queue_action.then_some(message_id.as_str());
            let outcome = match prepared {
                Ok(None) => Ok(STEP_SKIPPED),
                Ok(Some(prepared)) => self.perform_action_with_queue_id(&prepared, queue_message_id, now).await.map(|()| STEP_DONE),
                Err(error) => Err(error),
            };
            match outcome {
                Ok(state) => {
                    if !self.finish_run_step(&run, state, None, queue_message_id, Self::next_run_step(&run), now).await? {
                        return Ok(());
                    }
                }
                Err(detail) => {
                    self.finish_run_step(&run, STEP_FAILED, Some(&detail), None,
                        RunNext::Settle { outcome: "step_failed".into(), detail: Some(detail.clone()) }, now).await?;
                    return Ok(());
                }
            }
            if !self.store.claim_run(id, lease_owner(), now).await? {
                return Ok(());
            }
        }
        Err("run exceeded bounded step advancement".into())
    }

    fn next_run_step(run: &AutopilotRunRow) -> RunNext {
        if run.step_index + 1 == run.step_count {
            RunNext::Settle { outcome: "completed".into(), detail: None }
        } else {
            RunNext::Continue { step_index: run.step_index + 1 }
        }
    }

    async fn finish_run_step(
        &self, run: &AutopilotRunRow, state: &str, detail: Option<&str>, queue_id: Option<&str>,
        next: RunNext, now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let settling = matches!(&next, RunNext::Settle { .. });
        if !self.store.complete_step(&run.run_id, lease_owner(), run.step_index, state, detail, queue_id, next, now).await? {
            return Ok(false);
        }
        if settling { self.announce_run_notices_once(now).await?; }
        self.store.append_step_history(&AutopilotHistoryRow {
            id: format!("run:{}:step:{}", run.run_id, run.step_index),
            rule_id: run.rule_id.clone(), rule_name: run.rule_name.clone(), triggered_at: run_ts(now),
            result: match state {
                STEP_DONE => "step_done", STEP_SKIPPED => "step_skipped", _ => "step_failed",
            }.into(), details: detail.map(str::to_owned),
        }).await?;
        Ok(true)
    }

    async fn settle_run(&self, run: &AutopilotRunRow, outcome: &str, detail: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
        if self.store.settle_claimed_run(&run.run_id, lease_owner(), outcome, detail, now).await? {
            self.announce_run_notices_once(now).await?;
        }
        Ok(())
    }

    /// Gateway-lock-owned outbox sweep. Retrying either durable audit write is safe.
    pub(super) async fn announce_run_notices_once(&self, now: DateTime<Utc>) -> Result<(), String> {
        if !duduclaw_core::gateway_instance::held(&self.home_dir) { return Ok(()); }
        for run in self.store.list_run_notices_due(now, crate::autopilot_approval::NOTICE_SWEEP_BATCH).await? {
            if let Err(error) = self.announce_run_notice(&run, now).await {
                warn!(run_id = %run.run_id, %error, "autopilot: run notice remains in outbox");
            }
        }
        Ok(())
    }

    async fn announce_run_notice(&self, run: &AutopilotRunRow, now: DateTime<Utc>) -> Result<(), String> {
        let outcome = run.outcome.as_deref().ok_or("settled run has no outcome")?;
        let summary = match outcome {
            "missed_late" => format!("自動化規則「{}」系統停機超過一小時，後續步驟沒有補做", run.rule_name),
            "execution_uncertain" => format!("自動化規則「{}」在執行步驟時中斷，為避免重複沒有重送，請確認", run.rule_name),
            "completed" => format!("自動化規則「{}」已完成所有步驟", run.rule_name),
            _ => format!("自動化規則「{}」已停止，後續步驟沒有執行", run.rule_name),
        };
        if run.notice_pending {
            self.store.append_step_history(&AutopilotHistoryRow {
                id: format!("run:{}:outcome", run.run_id), rule_id: run.rule_id.clone(), rule_name: run.rule_name.clone(),
                triggered_at: run.updated_at.clone(), result: outcome.into(), details: run.detail.clone(),
            }).await?;
            self.task_store.append_activity_once(&ActivityRow {
                id: format!("run:{}:outcome", run.run_id), event_type: "autopilot_triggered".into(),
                agent_id: "autopilot".into(), task_id: None, summary: summary.clone(), timestamp: run.updated_at.clone(),
                metadata: Some(serde_json::json!({"rule_id": run.rule_id, "run_id": run.run_id,
                    "outcome": outcome, "success": outcome == "completed"}).to_string()),
            }).await?;
            self.store.clear_run_notice_pending(&run.run_id).await?;
        }
        if run.notice_push_state.as_deref() != Some("pending")
            || !self.store.lease_run_notice_push(&run.run_id, now).await?
        {
            return Ok(());
        }
        let attempt = run.notice_push_attempts + 1;
        let body = crate::goal_state::xml_escape(&summary);
        #[cfg(test)]
        {
            // Never deliver to real channels from a unit test, even without a hook.
            let pusher = self.notice_pusher.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let result = pusher.map(|push| push(&run.rule_id, &body))
                .unwrap_or(crate::autopilot_approval::NoticePush::NoTarget);
            self.store.record_run_notice_push(&run.run_id, attempt, result, now).await?;
        }
        #[cfg(not(test))]
        {
            let store = self.store.clone();
            let home = self.home_dir.clone();
            let run_id = run.run_id.clone();
            let rule_id = run.rule_id.clone();
            tokio::spawn(async move {
                let result = crate::autopilot_approval::push_admin_notice(&home, &rule_id, &body).await;
                if let Err(error) = store.record_run_notice_push(&run_id, attempt, result, Utc::now()).await {
                    warn!(%run_id, %error, "autopilot: run notice result not recorded; lease will expire");
                }
            });
        }
        Ok(())
    }
}
