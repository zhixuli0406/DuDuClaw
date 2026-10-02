use super::*;

impl DispatchEngine {
    /// Evaluate every `review` task through the judge.
    pub(super) async fn review_goal_tasks(&self) -> Result<(), String> {
        let Some(judge) = &self.judge else {
            // No evaluator configured — leave review tasks for later / human.
            let pending = self.store.tasks_in_status("review").await?;
            if !pending.is_empty() {
                debug!(
                    count = pending.len(),
                    "goal-mode review 任務等待中（尚未配置 judge）"
                );
            }
            return Ok(());
        };

        let now = Utc::now().to_rfc3339();
        for task in self.store.tasks_in_status("review").await? {
            // Attribute both adjudication stages to the same open iteration
            // the settle methods seal. A task counter can diverge from the
            // ledger after a repair; do not derive this from dispatch ordinals
            // or from the counter after a rejection increments it.
            let cost_round = match self.store.list_iterations(&task.id).await {
                Ok(rows) => rows.iter().rev()
                    .find(|row| row.judged_at.is_none() && row.verdict.is_none())
                    .map(|row| row.round),
                Err(error) => {
                    warn!(task = %task.id, %error, "review cost attribution: iteration lookup failed (non-fatal)");
                    None
                }
            };
            let attribution = crate::runtime::GoalRoundAttribution {
                episode_id: task.id.clone(),
                round: cost_round,
            };
            crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(attribution, async {
            // H9-G goal contract freeze (harness-borrowings 2026-08 WP-D):
            // the judge reads the immutable baseline snapshotted at goal
            // creation, not the mutable `acceptance_criteria` field a
            // dashboard operator may edit later. Falls back to the mutable
            // field for rows created before this column existed (or via a
            // creation path that doesn't freeze one) — value-source change
            // only, judge flow itself is untouched.
            let criteria = task
                .acceptance_criteria_baseline
                .clone()
                .or_else(|| task.acceptance_criteria.clone())
                .unwrap_or_default();
            let workspace_prefixes = self
                .home_dir
                .as_deref()
                .map(|h| workspace_prefixes_for(&h.join("agents").join(&task.assigned_to)))
                .unwrap_or_default();
            let result = strip_workspace_prefixes(
                &task.result_summary.clone().unwrap_or_default(),
                &workspace_prefixes,
            );
            // H1: the bare goal text, kept immutable. The MAV panel reads
            // `task_desc` (which accumulates evidence/contract blocks below);
            // the cheap first-stage evaluator reads this plus its own
            // transcript, so it never inherits panel-only additions.
            let task_text = format!("{}\n{}", task.title, task.description);
            let mut task_desc = task_text.clone();
            // This round's tool-evidence body, shared between the panel's
            // `<tool_activity>` block and the H1 evaluator transcript.
            let mut tool_activity_body: Option<String> = None;
            // Live round 8: this round's DETERMINISTIC evidence (one line per
            // declared artifact, with the sha256 of the bytes on disk), shared
            // by the same two consumers. `None` when the round declared no
            // artifacts — both consumers then look exactly as they did before.
            let mut artifact_receipts_block: Option<String> = None;

            // ── H5 follow-up (WP-B judge-input line, harness-borrowings
            // design §WP-B): fold the bail-pattern hint captured by
            // `goal_loop.rs::record_bail_pattern` into BOTH judge-facing
            // inputs below — the H1 pre-evaluator transcript and the MAV
            // panel's `task_desc` block. Same `GoalStateSnapshot.bail_hint`
            // the NEXT dispatch's `<state>` block already surfaces to the
            // AGENT (`goal_loop/state.rs::StateBlock::bail_hint`); this wires the
            // judge-facing half of the same H5 signal that
            // `record_bail_pattern`'s own doc comment flagged as deferred
            // (this file was mid-edit by a concurrent work package at the
            // time it was written). Read once here and shared verbatim by
            // both consumers so wording can never drift between them.
            // Wording is deliberately neutral — a nudge to double-check, not
            // a pre-judgment; the evaluator/judge still decide purely on
            // evidence.
            let bail_hint_note: Option<String> =
                crate::goal_state::GoalStateSnapshot::from_json(task.goal_state_json.as_deref())
                    .bail_hint
                    .as_deref()
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(|h| {
                        format!(
                            "疑似提前收工訊號：{h}\n（此提示僅供留意查核，並非預先判定，仍請依實際證據判斷任務是否完成。）"
                        )
                    });

            // ── BUG-2 fix (WP-A10 §6 復驗): take the WP-A4/A5/T10 native-tool
            // evidence for this round ONCE, up front, so B3 grounding, the
            // judge's `<tool_activity>` block, AND the A3 settle hook below
            // all share the same evidence — before this fix only A3 ever
            // saw it (`dispatcher.rs` bridges it unconditionally for every
            // goal-loop dispatch, independent of `[task_forward_model]
            // enabled`, so it is always safe to take here regardless of
            // whether A3 itself is on).
            //
            // `take_native_evidence` is remove-once semantics (its own doc
            // comment: "a round is only ever settled once"). `settle_forward_model`
            // below must NOT call it again — it now takes the value computed
            // here by reference, or it would silently observe `None` and A3
            // would degrade `full` back to `mcp_only` even though the
            // collector actually ran (this is the exact half-fix the WP-A10
            // report warned against).
            let round =
                crate::prediction::task_observe::evidence_round_for_revision(task.revision_round);
            let native_evidence: Option<Vec<NativeToolEvent>> =
                crate::prediction::task_observe::take_native_evidence(&task.id, round);
            let native_slice: &[NativeToolEvent] = native_evidence.as_deref().unwrap_or(&[]);

            // ── WP-A9: converge deterministic / grounding / judge outcome
            // into ONE settle call instead of three (design §4.2) — each
            // branch below sets `observed_outcome` (+ a feedback string for
            // the A3 transition write) instead of `continue`-ing
            // immediately; the actual `continue` happens once, after the
            // shared settle tail near the end of this loop body.
            // `new_confirmed_facts` collects this round's zero-LLM pass
            // signals for the A1 `confirmed_facts` wiring (see
            // `goal_loop/state.rs`'s "Honesty note" doc comment — this is that
            // follow-up).
            let mut observed_outcome: Option<crate::prediction::task_forward::ObservedOutcome> =
                None;
            let mut judge_feedback_for_settle: Option<String> = None;
            let mut new_confirmed_facts: Vec<String> = Vec::new();
            // ── P0/WP-C fault attribution (The Misattribution Gap,
            // arXiv:2605.22842): the two per-round signals the settle hook
            // below needs but that are only knowable HERE — the zero-LLM
            // grounding verdict and whether a judge actually ruled. Both
            // default to "no signal", which classifies as `Model` (today's
            // behavior) rather than manufacturing an exclusion.
            let mut fault_grounding: Option<crate::fault_attribution::GroundingVerdict> = None;
            let mut fault_judge_passed: Option<bool> = None;
            // WP-5D: a verdict produced by a non-MAV seam implementation
            // (today: `evaluator_only`'s `candidate_complete`). Set here and
            // consumed by the ONE verdict-handling `match` below, so accept /
            // reject / artifact-archiving / grant-revocation logic exists in
            // exactly one place regardless of which judge produced it.
            let mut preset_verdict: Option<AcceptanceVerdict> = None;

            // ── WP-5D judge seam: which acceptance implementation adjudicates
            // this task ("everything is a plugin" design §2 row 8 / §6-P1).
            // Read HERE, per task, on exactly the same schedule as the
            // pre-existing `[dispatch] two_stage_judge` below — one
            // `config.toml` read per review, no second hot-reload mechanism,
            // no `respawn_dispatch_engine` round-trip needed for a switch to
            // take effect. `home_dir` absent (test / legacy construction
            // paths) ⇒ `Mav` ⇒ everything below is byte-identical to the
            // pre-seam flow.
            let judge_mode = crate::judge_mode::JudgeMode::from_home(self.home_dir.as_deref());

            // A1 ledger (2026-09-30): harness knob snapshot sealed on this
            // round's `task_iterations` row, captured once per settle pass.
            // Read-only telemetry — nothing below consults it.
            let knobs_json = self.round_knobs_json(&task);

            // `human_only`: never machine-judged. Parked BEFORE any evidence
            // work or LLM/subprocess call so the mode is also the cheapest.
            // Uses the WP-A9 `observed_outcome` short-circuit (not a bare
            // `continue`) so the A3 settle tail still records the escalation.
            if judge_mode == crate::judge_mode::JudgeMode::HumanOnly {
                let reason = "依 [dispatch] judge = \"human_only\" 設定，本部署不做機器驗收，\
                              一律交由人工判定是否完成。"
                    .to_string();
                self.store
                    .mark_needs_human_sealing_round(
                        &task.id,
                        &reason,
                        crate::pause_reason::PauseReason::BlockedNeedsDecision,
                        knobs_json.as_deref(),
                    )
                    .await?;
                self.revoke_task_grants(&task.id).await;
                info!(task = %task.id, "judge seam: human_only → needs_human（不做機器驗收）");
                observed_outcome =
                    Some(crate::prediction::task_forward::ObservedOutcome::Escalated);
                judge_feedback_for_settle = Some(reason);
            }

            // ── WP2.4: deterministic outcome acceptance (BEFORE the judge) ──
            // A goal that declares a structured outcome contract (`json:` /
            // `files:`, persisted as an `outcome:<b64>` tag) is validated at
            // ZERO LLM cost here. A deterministic failure sends the task straight
            // back to `revising` with concrete defects and NEVER invokes the
            // judge — the guard against judge false-positives. A pass reaches the
            // judge with an explicit "deterministic 校驗已通過" note. Gated on a
            // wired `home_dir` (needed to resolve the agent working dir for
            // `files:` assertions); a corrupt tag yields `None` and falls through
            // to the judge unchanged (the judge remains a backstop).
            let mut deterministic_note: Option<String> = None;
            // WP-5D: the `observed_outcome` half of this pattern is new —
            // guarded like every later phase (the WP-A9 pattern) so the
            // `human_only` short-circuit above cannot be overwritten by a
            // deterministic verdict. In every other mode `observed_outcome`
            // is unconditionally `None` at this point, so the guard is
            // behavior-identical to the unguarded original.
            if let (None, Some(home)) = (observed_outcome, &self.home_dir) {
                if let Some(spec) = crate::outcome_spec::OutcomeSpec::from_tags(&task.tags) {
                    let worker = task
                        .claimed_by
                        .clone()
                        .unwrap_or_else(|| task.assigned_to.clone());
                    let work_dir = crate::outcome_spec::agent_work_dir(home, &worker);
                    let check = spec.validate(&result, &work_dir);
                    if !check.passed {
                        let feedback = format!(
                            "結構化產出驗收未通過（deterministic 零成本校驗，未進判官）：{}",
                            check.defects.join("；")
                        );
                        let status = self
                            .store
                            .reject_review_with_ledger(
                                &task.id,
                                &feedback,
                                self.soft_cap,
                                None,
                                knobs_json.as_deref(),
                            )
                            .await?;
                        // Phase closed (a rejection re-opens the loop) → revoke
                        // scoped grants, mirroring the judge-rejection path.
                        self.revoke_task_grants(&task.id).await;
                        info!(
                            task = %task.id, %status, defects = check.defects.len(),
                            "WP2.4 outcome 校驗未通過 → 跳過判官，直接退回 revising"
                        );
                        observed_outcome =
                            Some(crate::prediction::task_forward::ObservedOutcome::Rejected);
                        judge_feedback_for_settle = Some(feedback);
                    } else {
                        deterministic_note = Some(
                            "結構化產出驗收（outcome schema）已通過 deterministic 零成本校驗。"
                                .to_string(),
                        );
                        new_confirmed_facts.push(
                            "結構化產出驗收（outcome schema）已通過 deterministic 零成本校驗。"
                                .to_string(),
                        );
                    }
                }
            }

            // WP4 GroundEval / B3: read the task's claim→review tool-call
            // evidence once, then (1) run the zero-LLM grounding pre-check
            // (B3, before the judge) and (2) fold the same evidence into the
            // `<tool_activity>` prompt block (WP4, unchanged) — both read the
            // exact same window, so a single read keeps them consistent.
            // WP-A9: skipped once the deterministic check above already
            // decided this round's outcome (mirrors the old `continue`).
            if observed_outcome.is_none() {
                if let Some(home) = &self.home_dir {
                    let agent_id = task
                        .claimed_by
                        .clone()
                        .unwrap_or_else(|| task.assigned_to.clone());
                    // Review finding 14: `claimed_at.unwrap_or(created_at)`
                    // degenerated into "the whole task" for a team round,
                    // because a team task is NEVER claimed
                    // (`goal_loop::try_team_dispatch` completes it as
                    // `team-composer`). Round 3 could then be grounded on
                    // round 1's artifact receipts. A team round therefore
                    // resolves its own start — `task_iterations.dispatched_at`
                    // for this round, else the round's earliest
                    // `role_turns.jsonl` row — and, when neither exists, reads
                    // **no** window at all rather than falling back to
                    // `created_at`: grounding then degrades to `Skip` (the
                    // fail-closed direction) instead of vouching for this
                    // round with another round's evidence.
                    let team_round = task.team_spec_json.is_some();
                    let since: Option<String> = if team_round {
                        self.team_round_evidence_since(home, &task.id, round).await
                    } else {
                        Some(
                            task.claimed_at
                                .clone()
                                .unwrap_or_else(|| task.created_at.clone()),
                        )
                    };
                    // Team-as-Agent live round 3 E3: when this round was run
                    // by a team, the tool calls are in the ephemeral role
                    // members' audit rows, not the employee's. Union them in
                    // so grounding and the judge's `<tool_activity>` digest
                    // see the work that was actually done. A non-team task
                    // adds no ids and the evidence set is byte-identical.
                    //
                    // The round-scoped lookup falls back when the goal loop's
                    // in-flight `iter` and this settle's `revision_round + 1`
                    // diverge across a gateway restart. For a Solo task that
                    // fallback is the whole task (its claim→review window is
                    // real and already round-shaped); for a team task it is
                    // narrowed to the round's own window, so the widening
                    // cannot stack on top of the one just fixed above.
                    let mut evidence_agents = vec![agent_id.clone()];
                    let mut members =
                        crate::role_turns::member_ids_for_task_round(home, &task.id, round);
                    if members.is_empty() {
                        members = match (team_round, since.as_deref()) {
                            (true, Some(s)) => {
                                crate::role_turns::member_ids_for_task_since(home, &task.id, s)
                            }
                            (true, None) => Vec::new(),
                            (false, _) => crate::role_turns::member_ids_for_task(home, &task.id),
                        };
                    }
                    evidence_agents.extend(members);
                    let records = match since.as_deref() {
                        Some(since) => read_tool_activity_records_for_agents(
                            home,
                            &evidence_agents,
                            since,
                            &now,
                        ),
                        None => {
                            warn!(
                                task = %task.id, round,
                                "team round settle: no derivable evidence window for this round — \
                                 grounding and the judge digest degrade to no evidence rather than \
                                 reading the whole task"
                            );
                            Vec::new()
                        }
                    };

                    let grounding_config = GroundingPrecheckConfig::from_home(home);
                    match grounding_precheck(&result, &records, native_slice, grounding_config) {
                        GroundingPrecheck::Reject { feedback } => {
                            // P0/WP-C: deterministic evidence contradicts the
                            // claim for this round.
                            fault_grounding =
                                Some(crate::fault_attribution::GroundingVerdict::Fail);
                            let status = self
                                .store
                                .reject_review_with_ledger(
                                    &task.id,
                                    &feedback,
                                    self.soft_cap,
                                    None,
                                    knobs_json.as_deref(),
                                )
                                .await?;
                            // Phase closed (a rejection re-opens the loop) → revoke
                            // scoped grants, mirroring the judge-rejection path.
                            self.revoke_task_grants(&task.id).await;
                            info!(
                                task = %task.id, %status,
                                "B3 grounding 前置檢查未通過 → 跳過判官，直接退回 revising"
                            );
                            observed_outcome =
                                Some(crate::prediction::task_forward::ObservedOutcome::Rejected);
                            judge_feedback_for_settle = Some(feedback);
                        }
                        GroundingPrecheck::Grounded { tool_name } => {
                            // P0/WP-C: the answer IS backed by a real
                            // non-error tool result. Paired with a judge
                            // rejection below this becomes R1 (`Grader`).
                            fault_grounding =
                                Some(crate::fault_attribution::GroundingVerdict::Pass);
                            debug!(task = %task.id, tool = %tool_name, "B3 grounding 前置檢查通過");
                            // Fix-2 C1c: neutral wording (the previous
                            // "已通過…有工具佐證" overstated a pass-once
                            // check as a durable verified fact) and — as a
                            // second, belt-and-suspenders line of defense on
                            // top of C1a/C1b already keeping self-echo
                            // evidence out of `check_grounded` — only
                            // logged when the grounding tool is not itself
                            // on the self-echo deny-list.
                            if !duduclaw_core::grounding::is_self_echo_tool(&tool_name) {
                                new_confirmed_facts
                                    .push("本輪 grounding 前置檢查通過。".to_string());
                            }
                        }
                        GroundingPrecheck::Degraded { reason } => {
                            debug!(task = %task.id, reason, "B3 grounding 前置檢查 degrade（跳過，交由判官）");
                        }
                        GroundingPrecheck::Skip { reason } => {
                            debug!(task = %task.id, reason, "B3 grounding 前置檢查略過");
                        }
                    }

                    if let Some(block) = format_tool_activity(&records, native_slice) {
                        task_desc = format!("{task_desc}\n\n{block}");
                    }
                    // Live round 8: the deterministic half of the evidence.
                    // Built from the SAME `records` read above (the composer
                    // wrote one `artifact_receipt` row per declared artifact),
                    // so the judge, the evaluator and the team verifier are
                    // looking at one observation, not three re-derivations.
                    // A task that declared no artifacts adds nothing.
                    artifact_receipts_block = format_artifact_receipts_from_records(&records);
                    if let Some(block) = &artifact_receipts_block {
                        // Live round 9: receipts are workspace-relative; say so
                        // once, or the judge reads `notes/a.md` under
                        // `agents/<id>/` as "the wrong directory".
                        task_desc = match self.home_dir.as_deref() {
                            Some(home) => {
                                let workspace = home.join("agents").join(&task.assigned_to);
                                format!(
                                    "{task_desc}\n\n<workspace>\n{}\n</workspace>\n(receipt \
                                     paths are relative to this directory, which is the \
                                     assignee's working directory)\n{block}",
                                    workspace.display()
                                )
                            }
                            None => format!("{task_desc}\n\n{block}"),
                        };
                    }
                    // H1: the same evidence, un-wrapped, for the first-stage
                    // evaluator's own transcript (recomputed rather than
                    // unwrapped so neither consumer depends on the other's
                    // tag literal; the input is ≤20 aggregated rows).
                    tool_activity_body = format_tool_activity_body(&records, native_slice)
                        .map(|b| strip_workspace_prefixes(&b, &workspace_prefixes));
                }
            }

            // WP2.4: tell the judge the deterministic contract already passed, so
            // it focuses on the qualitative aspects rather than re-deriving what
            // the zero-cost check already verified.
            if let Some(note) = &deterministic_note {
                task_desc =
                    format!("{task_desc}\n\n<deterministic_check>{note}</deterministic_check>");
            }

            // ── G2 per-goal risk boundary (design §6, market-belief-loop
            // sister package): folded into the safety aspect's check basis
            // (see `aspect_instruction("safety")` below, which tells the
            // judge to treat this block as an automatic fail trigger) —
            // programmatic injection, never left to the judge to assume.
            // `task.risk_boundary` when the assign form explicitly set one,
            // else the deployment baseline. Fail-open: `home_dir` absent (a
            // handful of test/legacy construction paths) degrades to the
            // built-in default text directly rather than skipping the
            // block, and the underlying config read is itself fail-open
            // (see `goal_loop::baseline_boundary`) — this can never panic or
            // block a real judge call.
            let risk_boundary = match &self.home_dir {
                Some(home) => {
                    crate::goal_loop::effective_risk_boundary(task.risk_boundary.as_deref(), home)
                }
                None => task
                    .risk_boundary
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| crate::goal_loop::DEFAULT_BASELINE_BOUNDARY.to_string()),
            };
            task_desc =
                format!("{task_desc}\n\n<risk_boundary>\n{risk_boundary}\n</risk_boundary>");

            // H5 follow-up: fold the bail-pattern hint (computed above) into
            // the MAV panel's task block — same neutral note the H1
            // transcript item below carries.
            if let Some(note) = &bail_hint_note {
                task_desc = format!("{task_desc}\n\n<bail_hint>\n{note}\n</bail_hint>");
            }

            // ── P0/WP-B: judge-model routing audit ──
            // Advisory only — it cannot change a verdict. Placed BEFORE the
            // first-stage evaluator because both LLM stages share the judge
            // caller (and therefore the same routing), so one call per settle
            // covers the whole adjudication. Skipped once a zero-LLM phase has
            // already decided the round (no judge call will happen at all).
            if observed_outcome.is_none() {
                let worker = task
                    .claimed_by
                    .as_deref()
                    .unwrap_or(task.assigned_to.as_str());
                // Publish the worker for this round so the agent-less judge
                // caller can attribute a POST-spawn degrade (see
                // `judge_mode`'s attribution note). Sequential review loop ⇒
                // single writer.
                crate::judge_mode::set_settle_worker(worker);
                crate::judge_mode::audit_judge_model_routing(self.home_dir.as_deref(), worker)
                    .await;
            }

            // ── H1 first stage: cheap evaluator BEFORE the MAV panel ──
            // Skipped when a prior zero-LLM phase already decided this round
            // (deterministic outcome contract / B3 grounding), when no
            // evaluator is wired, or when `[dispatch] two_stage_judge = false`.
            // Every failure mode below degrades to the panel — never accepts,
            // never rejects on its own malfunction.
            //
            // WP-5D: under `[dispatch] judge = "evaluator_only"` this stage is
            // no longer a *pre*-filter — it is the entire acceptance decision,
            // and there is no panel behind it to degrade onto. So the two
            // "evaluator not available" conditions that are harmless in `mav`
            // mode (no evaluator wired / `two_stage_judge = false`) become a
            // fail-closed `needs_human` here: an unavailable judge must never
            // read as an unopposed pass.
            if observed_outcome.is_none() {
                let two_stage_enabled =
                    TwoStageJudgeConfig::from_home(self.home_dir.as_deref()).enabled;
                let evaluator_usable = self.evaluator.is_some() && two_stage_enabled;
                if judge_mode == crate::judge_mode::JudgeMode::EvaluatorOnly && !evaluator_usable {
                    let reason = format!(
                        "[dispatch] judge = \"evaluator_only\" 但第一階段評估器不可用（\
                         evaluator_wired={}, two_stage_judge={}）——本模式沒有 MAV 判官可退回，\
                         依 fail-closed 交由人工驗收。",
                        self.evaluator.is_some(),
                        two_stage_enabled
                    );
                    warn!(task = %task.id, "judge seam: evaluator_only 不可用 → needs_human（fail-closed）");
                    crate::judge_mode::log_judge_seam_event(
                        self.home_dir.as_deref(),
                        &task
                            .claimed_by
                            .clone()
                            .unwrap_or_else(|| task.assigned_to.clone()),
                        "judge_seam_unavailable",
                        judge_mode,
                        &reason,
                    );
                    self.store
                        .mark_needs_human_sealing_round(
                            &task.id,
                            &reason,
                            crate::pause_reason::PauseReason::Infra,
                            knobs_json.as_deref(),
                        )
                        .await?;
                    self.revoke_task_grants(&task.id).await;
                    observed_outcome =
                        Some(crate::prediction::task_forward::ObservedOutcome::Escalated);
                    judge_feedback_for_settle = Some(reason);
                } else if let Some(evaluator) = &self.evaluator {
                    if two_stage_enabled {
                        // Live round 10: the evaluator read `agents/<id>/notes/`
                        // as "not the working directory" and rejected a correct
                        // round; name the assignee's working directory as its
                        // own evidence item so relative paths resolve.
                        let workspace_item = self
                            .home_dir
                            .as_deref()
                            .map(|home| {
                                format!(
                                    "{} (the assignee's working directory; every relative path \
                                     in the criteria, the worker result, the tool activity and \
                                     the receipts is relative to it)",
                                    home.join("agents").join(&task.assigned_to).display()
                                )
                            })
                            .unwrap_or_default();
                        let transcript = build_evaluator_transcript(&[
                            ("workspace", workspace_item.as_str()),
                            ("worker_result", result.as_str()),
                            ("tool_activity", tool_activity_body.as_deref().unwrap_or("")),
                            // Live round 8: the evaluator's single failure mode
                            // is trusting a confident narrator, and the round it
                            // wrongly rejected had real files on disk. This item
                            // is the wrapped block (tag included) because it
                            // stands on its own as a distinct evidence source,
                            // and `build_evaluator_transcript` drops an empty
                            // item entirely.
                            (
                                "artifact_receipts",
                                artifact_receipts_block.as_deref().unwrap_or(""),
                            ),
                            (
                                "previous_round_feedback",
                                task.judge_feedback.as_deref().unwrap_or(""),
                            ),
                            // H5 follow-up: `build_evaluator_transcript`
                            // already truncates each item to
                            // `EVALUATOR_ITEM_MAX_BYTES` via
                            // `duduclaw_core::truncate_bytes` and drops empty
                            // items entirely, so an absent hint contributes
                            // nothing to the transcript.
                            ("bail_hint", bail_hint_note.as_deref().unwrap_or("")),
                        ]);
                        let eval_fut = evaluator.evaluate(&criteria, &task_text, &transcript);
                        let eval_result =
                            time::timeout(Duration::from_secs(EVALUATOR_TIMEOUT_SECS), eval_fut)
                                .await;
                        // A1 ledger: record the first-stage verdict on the
                        // round about to be sealed (NULL stays NULL when the
                        // evaluator errored or timed out). Pure bookkeeping —
                        // a failed write is logged and the decision below is
                        // unaffected.
                        if let Ok(Ok(ev)) = &eval_result {
                            if let Err(e) = self
                                .store
                                .record_iteration_evaluator_verdict(&task.id, ev.decision.as_str())
                                .await
                            {
                                warn!(task = %task.id, error = %e, "A1 ledger: evaluator verdict write failed (non-fatal)");
                            }
                        }
                        match eval_result {
                            Ok(Ok(ev)) => match ev.decision {
                                PreDecision::Continue => {
                                    // Not a completion candidate — retry with
                                    // `next_step` through the SAME path a judge
                                    // rejection takes, so this round counts
                                    // against `max_retries` and escalates to
                                    // `needs_human` once that budget is spent.
                                    let feedback = format_continue_feedback(&ev);
                                    let status = self
                                        .store
                                        .reject_review_with_ledger(
                                            &task.id,
                                            &feedback,
                                            self.soft_cap,
                                            None,
                                            knobs_json.as_deref(),
                                        )
                                        .await?;
                                    // Phase closed → revoke scoped grants,
                                    // mirroring every other rejection path.
                                    self.revoke_task_grants(&task.id).await;
                                    info!(
                                        task = %task.id, %status,
                                        "兩段式裁決：第一階段判定仍在進行中 → 跳過判官，帶下一步重新派工"
                                    );
                                    observed_outcome = Some(
                                        crate::prediction::task_forward::ObservedOutcome::Rejected,
                                    );
                                    judge_feedback_for_settle = Some(feedback);
                                }
                                PreDecision::Blocked => {
                                    let reason = format_blocked_reason(&ev);
                                    // H11: an external blocker the agent cannot
                                    // clear — classified at the call site (the
                                    // reason text itself is evaluator prose and
                                    // must never be re-parsed for the class).
                                    self.store
                                        .mark_needs_human_sealing_round(
                                            &task.id,
                                            &reason,
                                            crate::pause_reason::PauseReason::BlockedNeedsDecision,
                                            knobs_json.as_deref(),
                                        )
                                        .await?;
                                    self.revoke_task_grants(&task.id).await;
                                    warn!(
                                        task = %task.id,
                                        blocker = ev.blocker_key.as_deref().unwrap_or(""),
                                        "兩段式裁決：第一階段判定外部阻礙 → needs_human（待人工）"
                                    );
                                    observed_outcome = Some(
                                        crate::prediction::task_forward::ObservedOutcome::Escalated,
                                    );
                                    judge_feedback_for_settle = Some(reason);
                                }
                                PreDecision::CandidateComplete => {
                                    // WP-5D: in `evaluator_only` this IS the
                                    // acceptance decision — no panel follows.
                                    // The verdict is handed to the shared
                                    // verdict `match` below (rather than
                                    // duplicating the accept path) and is
                                    // labelled so nobody reading the round
                                    // timeline mistakes a low-cost
                                    // single-evaluator pass for a MAV panel
                                    // verdict.
                                    if judge_mode == crate::judge_mode::JudgeMode::EvaluatorOnly {
                                        info!(
                                            task = %task.id,
                                            "judge seam: evaluator_only 第一階段判定完成候選 → 直接驗收通過（未經 MAV 判官）"
                                        );
                                        preset_verdict = Some(AcceptanceVerdict {
                                            passed: true,
                                            feedback: format!(
                                                "驗收通過（[dispatch] judge = \"evaluator_only\" 低成本模式：\
                                                 僅第一階段評估器裁決，未經 MAV 判官，驗收強度較弱）：{}",
                                                ev.evidence
                                            ),
                                            // No panel ran ⇒ no aspects. Never
                                            // fabricate a panel record.
                                            aspects: None,
                                        });
                                    } else {
                                        debug!(
                                            task = %task.id,
                                            "兩段式裁決：第一階段判定為完成候選 → 交由 MAV 判官"
                                        );
                                    }
                                }
                            },
                            Ok(Err(e)) => {
                                // WP-5D: `mav` degrades to the panel (unchanged);
                                // `evaluator_only` has nothing to degrade onto, so
                                // an evaluator malfunction parks for a human.
                                if judge_mode == crate::judge_mode::JudgeMode::EvaluatorOnly {
                                    let reason = format!(
                                        "[dispatch] judge = \"evaluator_only\"：第一階段評估失敗且無 MAV 判官可退回，\
                                         依 fail-closed 交由人工驗收：{e}"
                                    );
                                    warn!(task = %task.id, error = %e, "judge seam: evaluator_only 評估失敗 → needs_human（fail-closed）");
                                    self.store
                                        .mark_needs_human_sealing_round(
                                            &task.id,
                                            &reason,
                                            crate::pause_reason::PauseReason::Infra,
                                            knobs_json.as_deref(),
                                        )
                                        .await?;
                                    self.revoke_task_grants(&task.id).await;
                                    observed_outcome = Some(
                                        crate::prediction::task_forward::ObservedOutcome::Escalated,
                                    );
                                    judge_feedback_for_settle = Some(reason);
                                } else {
                                    warn!(
                                        task = %task.id, error = %e,
                                        "兩段式裁決：第一階段評估失敗 → 降級直接走 MAV 判官（不影響裁決結果）"
                                    );
                                }
                            }
                            Err(_) => {
                                if judge_mode == crate::judge_mode::JudgeMode::EvaluatorOnly {
                                    let reason = format!(
                                        "[dispatch] judge = \"evaluator_only\"：第一階段評估逾時（{EVALUATOR_TIMEOUT_SECS}s）\
                                         且無 MAV 判官可退回，依 fail-closed 交由人工驗收。"
                                    );
                                    warn!(task = %task.id, secs = EVALUATOR_TIMEOUT_SECS, "judge seam: evaluator_only 評估逾時 → needs_human（fail-closed）");
                                    self.store
                                        .mark_needs_human_sealing_round(
                                            &task.id,
                                            &reason,
                                            crate::pause_reason::PauseReason::Infra,
                                            knobs_json.as_deref(),
                                        )
                                        .await?;
                                    self.revoke_task_grants(&task.id).await;
                                    observed_outcome = Some(
                                        crate::prediction::task_forward::ObservedOutcome::Escalated,
                                    );
                                    judge_feedback_for_settle = Some(reason);
                                } else {
                                    warn!(
                                        task = %task.id, secs = EVALUATOR_TIMEOUT_SECS,
                                        "兩段式裁決：第一階段評估逾時 → 降級直接走 MAV 判官（不影響裁決結果）"
                                    );
                                }
                            }
                        }
                    }
                }
            }

            // WP-A9: skipped once a prior phase already decided the outcome.
            if observed_outcome.is_none() {
                // ── WP-5D judge seam: resolve THIS round's verdict ──
                // Exactly one of three sources, and the `match` below (accept /
                // reject / artifact archive / grant revocation / A3 settle) is
                // shared by all of them:
                //   1. `preset_verdict` — an earlier seam stage already decided
                //      (today: `evaluator_only`'s `candidate_complete`).
                //   2. `external` — an operator-configured subprocess. EVERY
                //      defect (missing/malformed `judge_command`, spawn failure,
                //      timeout, non-zero exit, unparseable verdict,
                //      injection-flagged feedback) degrades to the MAV panel and
                //      is audited. A degrade is never a release: the strongest
                //      verifier decides, exactly as in `mav`.
                //   3. `mav` (default) — `judge.judge(...)`, byte-identical to
                //      the pre-seam flow.
                let verdict = match preset_verdict.take() {
                    Some(v) => Ok(v),
                    None if judge_mode == crate::judge_mode::JudgeMode::External => {
                        let audit_agent = task
                            .claimed_by
                            .clone()
                            .unwrap_or_else(|| task.assigned_to.clone());
                        match crate::judge_mode::ExternalJudgeConfig::from_home(
                            self.home_dir.as_deref(),
                        ) {
                            None => {
                                let detail = "[dispatch] judge = \"external\" 但 judge_command 未設定或格式不合法 → 降級走 MAV 判官".to_string();
                                warn!(task = %task.id, "judge seam: {detail}");
                                crate::judge_mode::log_judge_seam_event(
                                    self.home_dir.as_deref(),
                                    &audit_agent,
                                    "judge_seam_degraded",
                                    judge_mode,
                                    &detail,
                                );
                                judge.judge(&criteria, &task_desc, &result).await
                            }
                            Some(cfg) => {
                                let ext = crate::judge_mode::ExternalAcceptanceJudge::new(cfg);
                                match ext
                                    .judge_with_context(
                                        &criteria,
                                        &task_desc,
                                        &result,
                                        tool_activity_body.as_deref().unwrap_or(""),
                                    )
                                    .await
                                {
                                    Ok(v) => {
                                        info!(
                                            task = %task.id, passed = v.passed,
                                            "judge seam: external 判官回傳裁決"
                                        );
                                        Ok(v)
                                    }
                                    Err(e) => {
                                        warn!(
                                            task = %task.id, error = %e,
                                            "judge seam: external 判官失敗 → 降級走 MAV 判官（降級不放行）"
                                        );
                                        crate::judge_mode::log_judge_seam_event(
                                            self.home_dir.as_deref(),
                                            &audit_agent,
                                            "judge_seam_degraded",
                                            judge_mode,
                                            &e,
                                        );
                                        judge.judge(&criteria, &task_desc, &result).await
                                    }
                                }
                            }
                        }
                    }
                    None => judge.judge(&criteria, &task_desc, &result).await,
                };
                match verdict {
                    Ok(v) if v.passed => {
                        // P0/WP-C: a judge actually ruled this round.
                        fault_judge_passed = Some(true);
                        let verdict_json = v
                            .aspects
                            .as_ref()
                            .and_then(|a| serde_json::to_string(a).ok());
                        self.store
                            .accept_review_with_ledger(
                                &task.id,
                                &v.feedback,
                                verdict_json.as_deref(),
                                knobs_json.as_deref(),
                            )
                            .await?;
                        // WP3 (PORTICO): task phase closed → auto-revoke its grants.
                        self.revoke_task_grants(&task.id).await;
                        info!(task = %task.id, "goal-mode 驗收通過 → done");
                        // WP-4B: goal-loop settle archiving — a goal task's
                        // produced files previously existed only as an
                        // unarchived `task_changes.jsonl` breadcrumb (no
                        // download in the 產物 tab). Copy them into the
                        // agent's attachments/ now that the task is
                        // accepted, so the same `/api/files` surface the
                        // declared/swept channel-reply path already uses
                        // picks them up. Best-effort: failures are logged
                        // inside the helper and never affect the verdict
                        // already committed above.
                        if let Some(home) = self.home_dir.as_deref() {
                            let worker = task
                                .claimed_by
                                .clone()
                                .unwrap_or_else(|| task.assigned_to.clone());
                            let archive_report = crate::artifacts::archive_goal_task_artifacts(
                                home, &task.id, &worker,
                            )
                            .await;
                            if archive_report.archived > 0
                                || archive_report.skipped_oversize > 0
                                || archive_report.skipped_outside_root > 0
                            {
                                info!(
                                    task = %task.id,
                                    archived = archive_report.archived,
                                    already = archive_report.already_archived,
                                    skipped_oversize = archive_report.skipped_oversize,
                                    skipped_outside_root = archive_report.skipped_outside_root,
                                    "WP-4B: goal-loop settle archiving result"
                                );
                            }
                        }
                        observed_outcome =
                            Some(crate::prediction::task_forward::ObservedOutcome::Accepted);
                        judge_feedback_for_settle = Some(v.feedback.clone());
                    }
                    Ok(v) => {
                        // P0/WP-C: a judge ruled and rejected. If the
                        // zero-LLM grounding check had PASSED this round,
                        // R1 attributes the failure to the grader, not the
                        // model.
                        fault_judge_passed = Some(false);
                        let verdict_json = v
                            .aspects
                            .as_ref()
                            .and_then(|a| serde_json::to_string(a).ok());
                        let status = self
                            .store
                            .reject_review_with_ledger(
                                &task.id,
                                &v.feedback,
                                self.soft_cap,
                                verdict_json.as_deref(),
                                knobs_json.as_deref(),
                            )
                            .await?;
                        // WP3 (PORTICO): a rejection re-opens the loop for a retry,
                        // but the review phase closed — revoke so the retry must
                        // re-request any scoped tool it still needs.
                        self.revoke_task_grants(&task.id).await;
                        info!(task = %task.id, %status, "goal-mode 驗收未通過");
                        observed_outcome =
                            Some(crate::prediction::task_forward::ObservedOutcome::Rejected);
                        judge_feedback_for_settle = Some(v.feedback.clone());
                    }
                    Err(e) => {
                        // Fail-safe: judge itself failed — park for a human, do NOT
                        // auto-accept and do NOT loop.
                        warn!(task = %task.id, error = %e, "goal-mode judge 失敗 → needs_human（待人工）");
                        // H11: the platform failed, not the work — a distinct
                        // class from "the agent got stuck", so a human sees
                        // 「系統問題」rather than a false no-progress verdict.
                        self.store
                            .mark_needs_human_sealing_round(
                                &task.id,
                                &format!("judge unavailable: {e}"),
                                crate::pause_reason::PauseReason::Infra,
                                knobs_json.as_deref(),
                            )
                            .await?;
                        // WP3 (PORTICO): parked for a human → revoke task grants.
                        self.revoke_task_grants(&task.id).await;
                        observed_outcome =
                            Some(crate::prediction::task_forward::ObservedOutcome::Escalated);
                        judge_feedback_for_settle = Some(format!("judge unavailable: {e}"));
                    }
                }
            }

            // ── A1 leftover: persist this round's deterministic pass
            // signals into the task's `GoalStateSnapshot.confirmed_facts`
            // (goal_loop/state.rs) so the NEXT round's `<state>` block is no
            // longer permanently empty (see that module's "Honesty note"
            // doc comment). Unconditional — independent of the
            // `[task_forward_model]` toggle below. Best-effort: a failure
            // here must never affect the verdict already committed above.
            if !new_confirmed_facts.is_empty() {
                self.persist_confirmed_facts(&task.id, &new_confirmed_facts)
                    .await;
            }

            // ── WP-A9: A3 settle hook (design §4.2) ──
            // `forward_model` is `None` unless `[task_forward_model] enabled
            // = true` — with it `None`, this entire block is skipped and
            // review behavior (including every `continue` point above,
            // which are unconditional regardless of this hook) is
            // byte-identical to before A3 existed (design §7.3). A failure
            // inside is caught and logged, never allowed to alter the
            // verdict already committed above (R5).
            if let (Some(fm), Some(outcome)) = (self.forward_model.clone(), observed_outcome) {
                // ── P0/WP-C fault attribution: the per-round signals that
                // only exist in this loop body. `reply_claims_tool_use` is
                // the documented heuristic half of R3 — computed over the
                // worker's own final text. `failure_reason` is `None` here:
                // an infrastructure failure never produces a worker result
                // that reaches review at all, so R2 is structurally dormant
                // on THIS settle path (the field exists for the channel-reply
                // settle path, which does classify one).
                let fault_ctx = crate::fault_attribution::FaultContext {
                    grounding: fault_grounding,
                    judge_passed: fault_judge_passed,
                    reply_claims_tool_use: crate::fault_attribution::reply_claims_tool_use(&result),
                    failure_reason: None,
                };
                let settle_fut = self.settle_forward_model(
                    &fm,
                    &task,
                    outcome,
                    judge_feedback_for_settle.as_deref(),
                    native_evidence.as_deref(),
                    fault_ctx,
                );
                if let Err(e) = std::panic::AssertUnwindSafe(settle_fut)
                    .catch_unwind()
                    .await
                {
                    warn!(task = %task.id, "A3 forward-model settle hook panicked: {e:?}");
                }
            }

            // ── X1 方案 2: audit trail → causal evidence graph ──────────
            // Runs on EVERY failed settle, independent of
            // `[task_forward_model]` (which is off by default) — the audit
            // rows and the fault rule token exist either way, and this is
            // the only place both are in scope. Zero LLM, zero egress: it
            // transcribes bytes that are already on disk into a *candidate*
            // claim a human then reviews. Accepted rounds are skipped: a
            // success carries no failure mechanism to transcribe.
            if let (Some(home), Some(outcome)) = (self.home_dir.clone(), observed_outcome) {
                let ingest_outcome = match outcome {
                    crate::prediction::task_forward::ObservedOutcome::Rejected => {
                        Some(crate::causal_audit_ingest::SettleOutcome::Rejected)
                    }
                    crate::prediction::task_forward::ObservedOutcome::Escalated
                    | crate::prediction::task_forward::ObservedOutcome::Blocked => {
                        Some(crate::causal_audit_ingest::SettleOutcome::Escalated)
                    }
                    _ => None,
                };
                if let Some(ingest_outcome) = ingest_outcome {
                    let agent_id = task
                        .claimed_by
                        .clone()
                        .unwrap_or_else(|| task.assigned_to.clone());
                    let since = task
                        .claimed_at
                        .clone()
                        .unwrap_or_else(|| task.created_at.clone());
                    // Fidelity is recomputed here rather than reused from
                    // `observe_round` (which only runs under the forward
                    // model): native events present ⇒ `Full`, otherwise the
                    // project's documented main branch, `McpOnly`. This feeds
                    // ONE deterministic rule token; it never re-labels a round
                    // for the learning paths, which keep their own copy.
                    let native_count = native_evidence.as_ref().map_or(0, |e| e.len());
                    let fidelity = if native_count > 0 {
                        crate::prediction::task_forward::ObservationFidelity::Full
                    } else {
                        crate::prediction::task_forward::ObservationFidelity::McpOnly
                    };
                    let capability_blocked =
                        crate::fault_attribution::capability_blocked_in_window(
                            &home, &agent_id, &since,
                        );
                    let ctx = crate::fault_attribution::FaultContext {
                        grounding: fault_grounding,
                        judge_passed: fault_judge_passed,
                        reply_claims_tool_use:
                            crate::fault_attribution::reply_claims_tool_use(&result),
                        failure_reason: None,
                    };
                    let (_, fault_reason) =
                        ctx.classify(fidelity, native_count, capability_blocked);
                    let goal_kind =
                        crate::goal_loop::derive_goal_kind(&format!("{} {}", task.title, task.description));
                    let round = (task.revision_round as u32).saturating_add(1);
                    // Blocking SQLite + file work; keep it off the async
                    // runtime and never let it affect the committed verdict.
                    let owned = (
                        home.to_path_buf(),
                        agent_id.clone(),
                        task.id.clone(),
                        goal_kind.as_str().to_string(),
                        fault_reason.to_string(),
                        since.clone(),
                    );
                    let handle = tokio::task::spawn_blocking(move || {
                        crate::causal_audit_ingest::ingest_settle(
                            &owned.0,
                            &crate::causal_audit_ingest::SettleFacts {
                                agent_id: &owned.1,
                                task_id: &owned.2,
                                round,
                                outcome: ingest_outcome,
                                goal_kind: &owned.3,
                                fault_reason: &owned.4,
                                since: &owned.5,
                            },
                        )
                    });
                    match handle.await {
                        Ok(Some(claim)) => debug!(
                            task = %task.id, claim = %claim.claim_id,
                            cause = %claim.cause_variable, effect = %claim.effect_variable,
                            "稽核紀錄→因果證據圖：已建立候選主張（待人工審核）"
                        ),
                        Ok(None) => {}
                        Err(e) => {
                            warn!(task = %task.id, "causal audit ingest panicked (non-fatal): {e:?}")
                        }
                    }
                }
            }
            Ok::<(), String>(())
            }).await?;
        }
        Ok(())
    }
}

impl DispatchEngine {
    /// A1 ledger: the harness knob snapshot for this task's worker, as JSON.
    /// `None` without a wired `home_dir` (test / legacy construction) or on a
    /// serialization failure — telemetry only, never consulted by settle.
    fn round_knobs_json(&self, task: &TaskRow) -> Option<String> {
        let home = self.home_dir.as_deref()?;
        let worker = task.claimed_by.as_deref().unwrap_or(task.assigned_to.as_str());
        let agent_dir = home.join("agents").join(worker);
        let snapshot = crate::gvu::knob_snapshot::capture(home, &agent_dir);
        // The retry cap is task-local, so the global/agent snapshot alone
        // cannot reconstruct the guard that settled this task.
        let snapshot = serde_json::to_value(snapshot).and_then(|mut value| {
            value["max_retries"] = serde_json::Value::from(task.max_retries);
            value["task_knob_snapshot_version"] = serde_json::Value::from(1);
            serde_json::to_string(&value)
        });
        match snapshot {
            Ok(json) => Some(json),
            Err(e) => {
                warn!(task = %task.id, error = %e, "A1 ledger: knob snapshot serialize failed (non-fatal)");
                None
            }
        }
    }
}

#[cfg(test)]
mod survival_snapshot_tests {
    use super::*;
    struct Accept;
    #[async_trait]
    impl AcceptanceJudge for Accept {
        async fn judge(&self, _: &str, _: &str, _: &str) -> Result<AcceptanceVerdict, String> {
            Ok(AcceptanceVerdict { passed: true, feedback: "PASS".into(), aspects: None })
        }
    }
    #[tokio::test]
    async fn survival_evidence_sealed_knobs_include_task_retry_cap_without_reclassifying_current_task() {
        let home=tempfile::tempdir().unwrap();
        let store=Arc::new(TaskStore::open(home.path()).unwrap());
        let mut task=TaskRow::new("snapshot".into(),"Send report".into(),"".into(),"medium".into(),"alice".into(),"system".into());
        task.goal_mode=true; task.status="pending".into(); task.max_retries=7;
        task.acceptance_criteria=Some("Report sent".into());
        store.insert_task(&task).await.unwrap();
        store.record_iteration_dispatch("snapshot",1,"2026-10-01T00:00:00Z").await.unwrap();
        assert!(store.atomic_claim("snapshot","alice","2026-10-01T00:00:00Z","2026-10-01T00:05:00Z").await.unwrap().is_claimed());
        store.complete_task("snapshot","Report sent","alice").await.unwrap();
        let engine=DispatchEngine::new(store.clone(),Some(Arc::new(Accept))).with_home_dir(home.path().to_path_buf());
        engine.review_goal_tasks().await.unwrap();
        let reopened=TaskStore::open(home.path()).unwrap();
        let rows=reopened.list_iterations("snapshot").await.unwrap();
        assert_eq!(rows[0].verdict.as_deref(),Some("accepted"));
        let snapshot:serde_json::Value=serde_json::from_str(rows[0].knobs_json.as_deref().unwrap()).unwrap();
        assert_eq!(snapshot["max_retries"],7,"historical task retry cap must be in the sealed snapshot");
    }
}
