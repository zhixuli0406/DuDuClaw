//! Putting a work message on the agent wake-up rail, including the
//! Team-as-Agent dispatch. Moved verbatim out of `goal_loop.rs`.

use super::*;

impl GoalLoopDriver {
    /// Enqueue a work message for `task` onto `message_queue.db` — the same rail
    /// the heartbeat's task-board pull uses, so the existing dispatcher routes it
    /// to the agent unchanged. Carries `judge_feedback` (if any) so a rejected
    /// task is retried *with* the reviewer's feedback, and (I-1c) an approved
    /// plan-first plan so the very first round after approval executes it.
    /// Returns the queue id of the work message so the in-flight record can
    /// watch it for a synchronous dispatch failure.
    pub(super) async fn enqueue_work(
        &self,
        task: &TaskRow,
        iter: u32,
        state_text: &str,
    ) -> Result<String, String> {
        enqueue_goal_work(&self.queue, &self.store, task, iter, state_text).await
    }

    /// Team-as-Agent P1/WP-4: take over this round with a planner → executor(s)
    /// → verifier composition, when the task has a frozen team spec and the
    /// decomposability gate says so.
    ///
    /// Returns `Some(tracking_id)` when the team path took the round (the
    /// caller records it in `inflight` exactly as it records a queued message
    /// id), `None` when the task runs Solo — in which case the caller falls
    /// through to [`Self::enqueue_work`] and this round is **byte-identical**
    /// to the pre-WP-4 path.
    ///
    /// Everything expensive happens in a detached task: a round is three CLI
    /// spawns deep and would otherwise stall the 30 s tick for every other
    /// goal. The detached task lands its result the same way an agent would —
    /// `complete_task` puts the round into `review` and the existing
    /// `DispatchEngine` settle path adjudicates it — so no second
    /// adjudication story exists.
    /// Freeze-on-first-round backfill: `tasks.goal_create` freezes at
    /// creation, but a goal born on the chat `/goal`, autopilot, intent or
    /// plan-first path does not pass through that handler. Freezing here —
    /// before the task's first round, through the store's set-once
    /// `WHERE team_spec_json IS NULL` guard — keeps "one spec per task,
    /// decided before any work happens" true for every creation path without
    /// reaching into modules this work package does not own.
    ///
    /// Leaves `task.team_spec_json` holding whatever the **store** ended up
    /// with, which is the whole point of the `AlreadyFrozen` arm (review
    /// `goal_loop.rs:2486`): that outcome means the set-once guard refused
    /// *our* write because another path had already frozen a spec, so the
    /// local clone is the stale one. Before this, the caller's
    /// `frozen_spec(&task)?` then read `None`, that one round silently ran
    /// Solo, and every later round — reading a fresh row — ran as a Team. Same
    /// task, two execution shapes, no trace anywhere.
    ///
    /// Split out of [`Self::try_team_dispatch`] so the backfill is testable
    /// without driving a whole round (which would spawn CLIs).
    pub(super) async fn ensure_frozen_team_spec(&self, task: &mut TaskRow) {
        if task.team_spec_json.is_some() || task.revision_round != 0 {
            return;
        }
        match crate::team_composer::freeze_for_task(
            &self.home_dir,
            &self.store,
            &task.id,
            &task.assigned_to,
        )
        .await
        {
            crate::team_composer::FreezeOutcome::Frozen(spec) => {
                task.team_spec_json = serde_json::to_string(&spec).ok();
            }
            crate::team_composer::FreezeOutcome::AlreadyFrozen => {
                match self.store.get_task(&task.id).await {
                    Ok(Some(fresh)) => {
                        task.team_spec_json = fresh.team_spec_json;
                        info!(
                            task = %task.id,
                            has_spec = task.team_spec_json.is_some(),
                            "goal loop: team spec was already frozen by another path — \
                             re-read the stored spec for this round"
                        );
                    }
                    Ok(None) => warn!(
                        task = %task.id,
                        "goal loop: team spec already frozen but the task row vanished — \
                         this round runs Solo"
                    ),
                    Err(e) => warn!(
                        task = %task.id, error = %e,
                        "goal loop: team spec already frozen but the row could not be \
                         re-read — this round runs Solo"
                    ),
                }
            }
            // `Disabled` / `Refused` / `SoloByDefault` are already logged
            // (and, for a refusal, audited) inside `freeze_for_task`; the task
            // runs Solo.
            crate::team_composer::FreezeOutcome::Disabled
            | crate::team_composer::FreezeOutcome::Refused { .. }
            | crate::team_composer::FreezeOutcome::SoloByDefault { .. } => {}
        }
    }

    /// Returns the team dispatch (tracking id, confirmed-team flag) when the
    /// round runs as a team, plus — for the A1 round ledger — the team gate's
    /// inputs/decision JSON whenever the gate was evaluated (`None` when no
    /// frozen spec exists, so the gate was never consulted). The ledger half
    /// is observation only; the dispatch decision is unchanged.
    pub(super) async fn try_team_dispatch(
        &self,
        task: &TaskRow,
        iter: u32,
        state_text: &str,
    ) -> (Option<(String, bool)>, Option<String>) {
        let mut task = task.clone();
        self.ensure_frozen_team_spec(&mut task).await;
        let Some(spec) = crate::team_composer::frozen_spec(&task) else {
            return (None, None);
        };
        // Said once per process, the first time a team is actually considered
        // (design §3.8 fix ③): whether `[dispatch] ephemeral_max_active` can
        // hold the concurrent role members this configuration can need.
        crate::team_composer::warn_role_team_capacity_once(&self.home_dir);

        let (decision, mut gate_record) = crate::team_composer::decide_gate_recorded(
            &self.home_dir,
            &task,
            &spec,
            crate::team_composer::PlannerSignals::default(),
        );
        let grey_band = matches!(
            decision,
            duduclaw_core::team_gate::GateDecision::GreyBand { .. }
        );
        if decision.is_solo() {
            // The Solo branch is not "do nothing": the gate's effort
            // suggestion and the signal vector are already audited by
            // `decide_gate`. The round itself is unchanged.
            return (None, Some(gate_record.to_string()));
        }

        let home_dir = self.home_dir.clone();
        let store = Arc::clone(&self.store);
        let queue = Arc::clone(&self.queue);
        let state_text = state_text.to_string();
        let tracking_id = format!("team:{}:r{iter}", task.id);
        let spawns_used = crate::team_composer::spawns_used_for_task(&self.home_dir, &task.id);
        // Budget safety net for the default-on flip: a `[dispatch.team_budget]`
        // that cannot pay for even a fully degraded first round runs Solo
        // rather than parking a human over a task that produced nothing. A
        // task that already spent role spawns keeps the documented
        // `needs_human(budget_exhausted)` path inside `run_team_round`.
        let budget = crate::team_composer::TeamBudgetConfig::from_home(&self.home_dir);
        if crate::team_composer::budget_forces_solo(
            &budget,
            spawns_used,
            spec.executor_fanout,
            spec.planner.is_some(),
            &budget.degrade_order,
        ) {
            debug!(
                task = %task.id,
                max_spawns_per_task = budget.max_spawns_per_task,
                "team spawn budget cannot pay for a minimal round — this task runs Solo"
            );
            // A1 ledger: the gate said Team/grey band but the budget forced
            // Solo — record that override next to the gate's own verdict.
            gate_record["budget_forces_solo"] = serde_json::Value::Bool(true);
            return (None, Some(gate_record.to_string()));
        }
        // A1-3 ledger: the team round's token usage carries the task id and
        // its `task_iterations.round` (same value the tick records).
        let goal_attr = crate::runtime::GoalRoundAttribution {
            episode_id: task.id.clone(),
            round: Some(task.revision_round + 1),
        };
        // P2-A: registered before the spawn so a stop never reports
        // "stopped" while this round may still run (the queue never sees it).
        let round_guard = crate::responsibility::team_activity::RoundGuard::register(&task.id);
        tokio::spawn(async move {
            let _round_guard = round_guard;
            let outcome = crate::runtime::GOAL_ROUND_ATTRIBUTION
                .scope(
                    goal_attr,
                    crate::team_composer::run_team_round(crate::team_composer::TeamRoundContext {
                        home_dir: &home_dir,
                        task: &task,
                        round: iter,
                        spec: &spec,
                        state_text: &state_text,
                        spawns_used,
                        grey_band,
                    }),
                )
                .await;
            match outcome {
                crate::team_composer::TeamRoundOutcome::Submitted { summary, .. } => {
                    // Lands exactly where an agent's `tasks_complete` would:
                    // `review`, for the existing two-stage evaluator + MAV
                    // panel to adjudicate. The caller id is the composer's own
                    // — a team task is never claimed by an agent, so the
                    // claim-holder guard does not apply.
                    if let Err(e) = store
                        .complete_task(&task.id, &summary, "team-composer")
                        .await
                    {
                        warn!(task = %task.id, "team round result could not be submitted: {e}");
                    }
                }
                crate::team_composer::TeamRoundOutcome::NeedsHuman { reason, pause } => {
                    if let Err(e) = store
                        .mark_needs_human_with_pause(&task.id, &reason, pause)
                        .await
                    {
                        warn!(task = %task.id, "team round needs_human write failed: {e}");
                    } else if let Err(e) = store.stamp_iteration_pause(&task.id, pause.as_str()).await
                    {
                        // A1 ledger: bookkeeping only — never affects the park.
                        warn!(task = %task.id, "A1 ledger: iteration pause stamp failed (non-fatal): {e}");
                    }
                }
                crate::team_composer::TeamRoundOutcome::SoloFallback { reason } => {
                    // The grey band resolved to Solo after the planner ran.
                    // Dispatch the ordinary work message so this round is the
                    // single-agent round it would have been — the planner's
                    // packets stay on disk for a later round to read.
                    info!(task = %task.id, reason, "team grey band resolved Solo — dispatching single agent");
                    if let Err(e) =
                        enqueue_goal_work(&queue, &store, &task, iter, &state_text).await
                    {
                        warn!(task = %task.id, "solo fallback dispatch failed: {e}");
                    }
                }
                crate::team_composer::TeamRoundOutcome::Failed { error } => {
                    // Left in flight on purpose: the driver's `stalled_secs`
                    // guard re-dispatches an un-picked-up round exactly as it
                    // does for a work message nobody consumed, and
                    // `DISPATCH_FAILURE_LIMIT` still ends the loop.
                    warn!(task = %task.id, round = iter, "team round failed: {error}");
                }
            }
        });
        // The grey band can still resolve Solo after the planner runs. Until
        // then, do not tell the user the task has definitively formed a team.
        (Some((tracking_id, !grey_band)), Some(gate_record.to_string()))
    }
}

/// Enqueue one goal-loop work message on the agent wake-up rail.
///
/// Lifted out of [`GoalLoopDriver::enqueue_work`] (which now delegates here
/// unchanged) so the Team-as-Agent grey-band Solo fallback — which runs in a
/// detached task that does not hold `&self` — dispatches through the exact
/// same code rather than a second copy of the payload. The payload text is
/// character-for-character what it was before the extraction.
pub(crate) async fn enqueue_goal_work(
    queue: &Arc<MessageQueue>,
    store: &Arc<TaskStore>,
    task: &TaskRow,
    iter: u32,
    state_text: &str,
) -> Result<String, String> {
    let payload = build_goal_payload(task, iter, state_text);
    let msg = goal_queue_message(uuid::Uuid::new_v4().to_string(), task, payload);
    let result = queue.enqueue(&msg).await.map(|()| msg.id.clone());
    // I-1c: the plan has now been injected into this round's payload —
    // consume it so it is not re-injected on every later round. Cleared
    // only after a successful enqueue (an enqueue failure leaves it in
    // place, so the next tick's retry still carries the plan). A failed
    // clear is logged and otherwise harmless: the plan is simply
    // re-injected next dispatch, which repeats guidance rather than
    // losing anything.
    if result.is_ok() {
        clear_consumed_plan(store, task).await;
    }
    result
}

/// I-1c: clear an injected plan-first plan once its round is enqueued.
pub(super) async fn clear_consumed_plan(store: &Arc<TaskStore>, task: &TaskRow) {
    if task.plan_pending.is_some() {
        if let Err(e) = store.clear_plan_pending(&task.id).await {
            warn!(
                task = %task.id,
                error = %e,
                "goal loop: failed to clear plan_pending after injecting it — will re-inject next dispatch (harmless)"
            );
        }
    }
}

/// The work payload of one goal round (P2-A: shared by the random-id rail
/// above and the fixed-id durable rail, so the text is one implementation).
pub(super) fn build_goal_payload(task: &TaskRow, iter: u32, state_text: &str) -> String {
    let marker = format!("[goal-loop task_id={} iter={iter}]", task.id);
    // I-3a: a task continued from `done`/`failed`/`cancelled` via the
    // dashboard's "接著做" action stamps `judge_feedback` with
    // `CONTINUE_MESSAGE_PREFIX` (see `TaskStore::continue_from_terminal`)
    // instead of a real judge verdict. Telling the agent "上一輪驗收未
    // 通過" would be false for a task that had actually succeeded, so
    // this is rendered as a distinct follow-up-instruction block.
    let feedback_block = match task.judge_feedback.as_deref() {
        Some(fb) if !fb.trim().is_empty() => {
            if let Some(user_msg) = fb.strip_prefix(CONTINUE_MESSAGE_PREFIX) {
                format!(
                    "\n\n這項任務先前已結束(完成或失敗),使用者要求你接著做,補充指示如下\
                         (這一輪的結果仍會經過驗收判官檢核):\n\
                         <user_message>\n{user_msg}\n</user_message>"
                )
            } else {
                format!(
                    "\n\n上一輪驗收未通過,驗收判官的回饋如下,請據此修正後再回報:\n\
                         <judge_feedback>\n{fb}\n</judge_feedback>"
                )
            }
        }
        _ => String::new(),
    };
    // I-1c "想一想": a plan generated at goal-create time and approved via
    // the same needs_human `retry` a human uses for any other pause —
    // `plan_pending` is the ONE column that action does not overwrite
    // (see the field's doc comment on `TaskRow`), so its presence here
    // reliably means "this is the first round after approval". Rendered
    // as its own block (not folded into `feedback_block`, which is about
    // review/continuation, not a plan the agent has not started yet).
    let plan_block = match task.plan_pending.as_deref() {
        Some(p) if !p.trim().is_empty() => format!(
            "\n\n這是你先前為此任務擬定、已獲人工核准的執行計畫,請依此計畫開始執行\
                 (仍會經過驗收判官檢核,計畫本身不是免驗收的保證):\n\
                 <execution_plan>\n{}\n</execution_plan>",
            goal_state::xml_escape(p)
        ),
        _ => String::new(),
    };
    let criteria_block = match task.acceptance_criteria.as_deref() {
        Some(c) if !c.trim().is_empty() => {
            // H9-G contract discipline (harness-borrowings 2026-08 WP-D):
            // reassure the executing agent that the criteria are judged as
            // written — a different but valid approach is not grounds for
            // rejection, the bar will not tighten mid-task, and anything
            // not listed is out of scope rather than an implicit extra
            // requirement to satisfy.
            format!(
                "\n• 驗收標準: {c}\n\
                     （驗收標準看的是最終結果,不是實作路徑,用不同但正確的做法達成一樣算數；\
                     標準已定案,不會在過程中被無故加嚴；沒列在標準內的事不在驗收範圍內。）"
            )
        }
        _ => String::new(),
    };
    // A1 (StateAct): the structured state block + the self-report
    // protocol instructions. Plain text, runtime-neutral — any CLI
    // backend (Claude / Codex / Gemini / Antigravity / openai-compat)
    // reads this the same way, and the self-report marker is parsed by
    // `goal_state::parse_state_update` regardless of which runtime
    // produced it.
    let payload = format!(
        "{marker} 你有一個自主目標任務要推進:\n\
             • Task ID: {}\n\
             • 標題: {}\n\
             • 說明: {}{criteria_block}\n\n\
             {state_text}\n\n\
             若你在推進過程中形成了新的『待驗證假設』,請在回覆最後附上下列標記(純文字,任何 \
             AI 執行環境皆可產出;省略此標記則系統會沿用上一輪的假設清單,絕不自行臆測):\n\
             <state_update>{{\"pending_hypotheses\": [\"假設一\", \"假設二\"]}}</state_update>\n\n\
             請使用 MCP 工具 `tasks_claim` 認領這項任務,執行後用 `tasks_complete` \
             回報結果(務必在 result_summary 寫清楚你做了什麼、產出在哪),\
             系統會由驗收判官檢核是否達成驗收標準。若受阻無法完成,使用 `tasks_block` \
             說明原因。{feedback_block}{plan_block}",
        task.id, task.title, task.description,
    );
    payload
}

/// The queue row of one goal round with the given message id.
pub(super) fn goal_queue_message(id: String, task: &TaskRow, payload: String) -> QueueMessage {
    QueueMessage {
        id,
        sender: "goal-loop-driver".to_string(),
        target: task.assigned_to.clone(),
        payload,
        status: MessageStatus::Pending,
        retry_count: 0,
        delegation_depth: 0,
        // WP21 C1: the dispatcher's delegation gate judges `sender_agent`
        // (falling back to `origin_agent`); leaving both `None` put every
        // goal-loop dispatch through the v1.52 legacy-warn path instead of
        // being judged like any other sender. Stamped consistently with
        // the `sender` column above — this rail has exactly one sender
        // identity, the goal-loop driver itself, which is already in
        // `SYSTEM_SENDERS` and so always clears the gate.
        origin_agent: Some("goal-loop-driver".to_string()),
        sender_agent: Some("goal-loop-driver".to_string()),
        error: None,
        response: None,
        created_at: Utc::now().to_rfc3339(),
        acked_at: None,
        completed_at: None,
        reply_channel: None,
        turn_id: None,
        session_id: None,
        // P2-B: a goal round carries no upstream channel turn to lose.
        upstream_unknown: false,
    }
}
