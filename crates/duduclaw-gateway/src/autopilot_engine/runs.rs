//! Durable, gateway-owned multi-step execution. A wait is a persisted deadline,
//! never a sleeping task. External notifications with uncertain delivery stop.

use super::*;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crate::autopilot_approval::{event_ref, rule_revision_hash, run_dedupe_key};
use crate::autopilot_store::{
    AutopilotRunRow, AutopilotRunStepRow, RunInsertOutcome, RunNext,
    run_ts, step_idempotency_key, RUN_PENDING, RUN_RUNNING,
    STEP_PENDING, STEP_RUNNING, STEP_DONE, STEP_FAILED, STEP_SKIPPED,
};
use crate::autopilot_window::{rule_window_decision, WindowDecision};

static RUN_LEASE_OWNER: OnceLock<String> = OnceLock::new();

fn lease_owner() -> &'static str {
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
        for run in self.store.list_unsupported_runs(50).await? {
            if self.store.settle_unclaimed_run(&run.run_id, &[&run.state], "unsupported_state", None, now).await? {
                self.record_run_outcome(&run, "unsupported_state", None, now).await?;
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

    async fn recover_run(&self, id: &str, now: DateTime<Utc>) -> Result<(), String> {
        if !self.store.reclaim_expired_run(id, lease_owner(), now).await? {
            return Ok(());
        }
        let run = self.store.get_run(id).await?.ok_or("reclaimed run missing")?;
        let steps = self.store.list_run_steps(id).await?;
        if let Some(step) = steps.iter().find(|s| s.step_index == run.step_index) {
            if step.state == STEP_RUNNING
                && matches!(step.step_type.as_str(), "notify" | "proactive_notify" | "wait")
            {
                return self.settle_run(&run, "execution_uncertain", None, now).await;
            }
        }
        // Pending steps advance normally. Running queue actions are reconciled
        // in advance_run, after checking the current rule and exact recipient.
        self.advance_run(id, now).await
    }

    /// No store calls are made while the rule fence is alive. Return owned data
    /// so the guard is released before marking a step or performing an action.
    async fn checked_run_step(
        &self, run: &AutopilotRunRow, fields: &serde_json::Map<String, Value>, now: DateTime<Utc>,
    ) -> Result<(AutopilotRuleRow, Value), (String, String)> {
        let (fence, rule) = self.store.fence_rule(&run.rule_id).await
            .map_err(|e| ("rule_unreadable".into(), e))?;
        let checked: Result<(AutopilotRuleRow, Value), (String, String)> = async {
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
            Ok((rule, step))
        }.await;
        // Residual window: a rule can change after release and before the side
        // effect. Holding this fence across store writes would deadlock. The
        // revision snapshot and a fresh fenced check before EVERY step bound it.
        fence.release().map_err(|e| ("rule_unreadable".into(), e))?;
        checked
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
            if let Some(next_at) = &run.next_at {
                match DateTime::parse_from_rfc3339(next_at) {
                    Ok(due) if now.signed_duration_since(due.with_timezone(&Utc)) > ChronoDuration::minutes(60) => {
                        return self.settle_run(&run, "missed_late", None, now).await;
                    }
                    Ok(_) => {}
                    Err(_) => return self.settle_run(&run, "invalid_state", Some("invalid next_at"), now).await,
                }
            }
            let fields: serde_json::Map<String, Value> = match serde_json::from_str(&run.fields_json) {
                Ok(fields) => fields,
                Err(_) => return self.settle_run(&run, "fields_unreadable", None, now).await,
            };
            let (rule, action) = match self.checked_run_step(&run, &fields, now).await {
                Ok(checked) => checked,
                Err((outcome, detail)) => return self.settle_run(&run, &outcome, Some(&detail), now).await,
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
            // 141A-P6: render/freeze this step and enter the approval ledger.
            // Until that package lands, no approval-gated step may execute.
            if crate::approval::rule_requires_approval(&action) {
                return self.settle_run(&run, "approval_step_unavailable", None, now).await;
            }
            let queue_action = matches!(step.step_type.as_str(), "delegate" | "run_skill");
            let message_id = queue_id(id, run.step_index);
            if step.state == STEP_RUNNING {
                if !queue_action {
                    return self.settle_run(&run, "execution_uncertain", None, now).await;
                }
                let queue = self.message_queue.as_ref().ok_or("message queue not available")?;
                match queue.get_by_id(&message_id).await? {
                    Some(existing) => {
                        if Some(existing.target.as_str()) != action.get("target_agent").and_then(Value::as_str) {
                            return self.finish_run_step(&run, STEP_FAILED, Some("queue id target mismatch"), None,
                                RunNext::Settle { outcome: "step_failed".into(), detail: Some("queue id target mismatch".into()) }, now).await.map(|_| ());
                        }
                        if !self.finish_run_step(&run, STEP_DONE, None, Some(&message_id), Self::next_run_step(&run), now).await? {
                            return Ok(());
                        }
                        if !self.store.claim_run(id, lease_owner(), now).await? { return Ok(()); }
                        continue;
                    }
                    None => {
                        if !self.store.reset_running_step(id, run.step_index, lease_owner(), now).await? {
                            return Ok(());
                        }
                    }
                }
            } else if step.state != STEP_PENDING {
                return self.settle_run(&run, "unsupported_state", Some("unexpected current step state"), now).await;
            }
            if !self.store.mark_step_running(id, run.step_index, lease_owner(), now).await? {
                return Ok(());
            }
            if step.step_type == "wait" {
                let minutes = action["minutes"].as_i64().filter(|n| (1..=1440).contains(n))
                    .ok_or("invalid wait minutes")?;
                let next_at = now.checked_add_signed(ChronoDuration::minutes(minutes)).ok_or("wait timestamp overflow")?;
                self.finish_run_step(&run, STEP_DONE, None, None,
                    RunNext::Wait { step_index: run.step_index + 1, next_at }, now).await?;
                return Ok(());
            }
            let perception = sanitize_perception_fields(&fields, &self.home_dir);
            let effective = perception.as_ref().map(|(f, _)| f).unwrap_or(&fields);
            let banner = perception.as_ref().and_then(|(_, b)| b.as_deref());
            let prepared = self.prepare_action(&rule.id, &rule.name, &action, &fields, effective, banner).await;
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
        let settlement = match &next {
            RunNext::Settle { outcome, detail } => Some((outcome.clone(), detail.clone())),
            _ => None,
        };
        if !self.store.complete_step(&run.run_id, lease_owner(), run.step_index, state, detail, queue_id, next, now).await? {
            return Ok(false);
        }
        self.store.append_step_history(&AutopilotHistoryRow {
            id: format!("run:{}:step:{}", run.run_id, run.step_index),
            rule_id: run.rule_id.clone(), rule_name: run.rule_name.clone(), triggered_at: run_ts(now),
            result: match state {
                STEP_DONE => "step_done", STEP_SKIPPED => "step_skipped", _ => "step_failed",
            }.into(), details: detail.map(str::to_owned),
        }).await?;
        if let Some((outcome, detail)) = settlement {
            self.record_run_outcome(run, &outcome, detail.as_deref(), now).await?;
        }
        Ok(true)
    }

    async fn settle_run(&self, run: &AutopilotRunRow, outcome: &str, detail: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
        if self.store.settle_claimed_run(&run.run_id, lease_owner(), outcome, detail, now).await? {
            self.record_run_outcome(run, outcome, detail, now).await?;
        }
        Ok(())
    }

    async fn record_run_outcome(&self, run: &AutopilotRunRow, outcome: &str, detail: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
        self.store.append_step_history(&AutopilotHistoryRow {
            id: format!("run:{}:outcome", run.run_id), rule_id: run.rule_id.clone(), rule_name: run.rule_name.clone(),
            triggered_at: run_ts(now), result: outcome.into(), details: detail.map(str::to_owned),
        }).await?;
        let summary = match outcome {
            "missed_late" => format!("自動化規則「{}」系統停機超過一小時，後續步驟沒有補做", run.rule_name),
            "execution_uncertain" => format!("自動化規則「{}」在執行步驟時中斷，為避免重複沒有重送，請確認", run.rule_name),
            "completed" => format!("自動化規則「{}」已完成所有步驟", run.rule_name),
            _ => format!("自動化規則「{}」已停止，後續步驟沒有執行", run.rule_name),
        };
        self.task_store.append_activity(&ActivityRow {
            id: format!("run:{}:outcome", run.run_id), event_type: "autopilot_triggered".into(),
            agent_id: "autopilot".into(), task_id: None, summary: summary.clone(), timestamp: run_ts(now),
            metadata: Some(serde_json::json!({"rule_id": run.rule_id, "run_id": run.run_id,
                "outcome": outcome, "success": outcome == "completed"}).to_string()),
        }).await?;
        if matches!(outcome, "missed_late" | "execution_uncertain") {
            let body = crate::goal_state::xml_escape(&summary);
            let result = crate::autopilot_approval::push_admin_notice(&self.home_dir, &run.rule_id, &body).await;
            if result == crate::autopilot_approval::NoticePush::Failed {
                warn!(run_id = %run.run_id, "autopilot: admin notice push failed; activity retained");
            }
        }
        Ok(())
    }
}
