use super::*;

impl DispatchEngine {
    /// The real start of one **team** round, for the settle path's evidence
    /// window (review finding 14).
    ///
    /// Two durable sources, in order, and never a third:
    ///
    /// 1. `task_iterations.dispatched_at` for this round — stamped by the goal
    ///    loop's tick clock *before* `try_team_dispatch` spawns the composer,
    ///    so it precedes every member's first tool call and never reaches back
    ///    into the previous round (which had already settled).
    /// 2. The round's earliest `role_turns.jsonl` timestamp — the fallback for
    ///    a task whose iteration row is missing (a gateway restarted between
    ///    dispatch and settle). Rows are stamped at stage end, so this can be
    ///    later than the true start: the window then under-reports, which is
    ///    the honest direction.
    ///
    /// `None` when neither exists. The caller must then read **no** window;
    /// falling back to `created_at` is exactly the defect this replaces.
    pub(super) async fn team_round_evidence_since(
        &self,
        home: &std::path::Path,
        task_id: &str,
        round: u32,
    ) -> Option<String> {
        match self.store.list_iterations(task_id).await {
            Ok(rows) => {
                if let Some(it) = rows.iter().find(|r| r.round == round as i64) {
                    let stamped = it.dispatched_at.trim();
                    if !stamped.is_empty() {
                        return Some(stamped.to_string());
                    }
                }
            }
            Err(e) => {
                debug!(task = task_id, round, error = %e, "team settle: iteration lookup failed");
            }
        }
        crate::role_turns::round_started_at(home, task_id, round)
    }

    /// WP-A9 settle-side helper (design §4.2): observe this round's tool
    /// evidence, diff it against the WP-A9 predict hook's stored prediction,
    /// persist the error / fold it into the statistical bucket, and
    /// (fidelity permitting) write a WP-B2 transition sample to episodic
    /// memory. Entirely best-effort — every failure path here is logged and
    /// swallowed; the review verdict (`accept_review` / `reject_review` /
    /// `mark_needs_human`) has already been durably committed by the time
    /// this runs.
    ///
    /// P0/WP-C: `fault_ctx` carries the caller's per-round attribution
    /// signals. Once this hook knows the observation fidelity it classifies
    /// the round's fault side; anything other than
    /// [`crate::fault_attribution::FaultSide::Model`] excludes the round from
    /// the learning-credit paths below (see the two gates marked `WP-C`).
    pub(super) async fn settle_forward_model(
        &self,
        fm: &Arc<crate::prediction::task_forward_store::TaskForwardModel>,
        task: &TaskRow,
        outcome: crate::prediction::task_forward::ObservedOutcome,
        judge_feedback: Option<&str>,
        native_evidence: Option<&[NativeToolEvent]>,
        fault_ctx: crate::fault_attribution::FaultContext,
    ) {
        let round = (task.revision_round as u32).saturating_add(1);
        let Some(prediction) = fm.get_prediction(&task.id, round).await else {
            debug!(
                task = %task.id, round,
                "A3 settle: no logged prediction for this round (predict hook off, or a gap) — skipping"
            );
            return;
        };

        let agent_id = task
            .claimed_by
            .clone()
            .unwrap_or_else(|| task.assigned_to.clone());
        let since = task
            .claimed_at
            .clone()
            .unwrap_or_else(|| task.created_at.clone());
        let until = Utc::now().to_rfc3339();

        let runtime = self
            .home_dir
            .as_deref()
            .map(|home| {
                let agent_dir = crate::outcome_spec::agent_work_dir(home, &agent_id);
                crate::runtime_config::agent_runtime_provider(&agent_dir)
            })
            .unwrap_or_default();

        // Deterministic artifact-shape hint from the same `OutcomeSpec` the
        // WP2.4 check above already parsed from `task.tags` (design §4.2
        // point 2: "outcome spec 通過 ⇒ 至少 StructuredJson/FileWrite").
        let observed_artifact = match crate::outcome_spec::OutcomeSpec::from_tags(&task.tags) {
            Some(crate::outcome_spec::OutcomeSpec::Json(_)) => {
                crate::prediction::task_forward::ArtifactShape::StructuredJson
            }
            Some(crate::outcome_spec::OutcomeSpec::Files(_)) => {
                crate::prediction::task_forward::ArtifactShape::FileWrite
            }
            _ => crate::prediction::task_forward::ArtifactShape::TextOnly,
        };

        // WP-A4/A5/T10: `native_evidence` is whatever `dispatcher.rs` bridged
        // over for this exact (task_id, round) — see
        // `task_observe::record_native_evidence`'s doc comment for why this
        // is a process-lifetime in-memory hop rather than a SQL read.
        // `None` (no collector ran, or the entry was never bridged — the
        // common case until every dispatch path opts in) falls back to the
        // pre-existing `McpOnly`/`None` behavior inside `observe_round`
        // unchanged.
        //
        // BUG-2 fix (WP-A10 §6 復驗): this is now passed IN by the caller
        // (`review_goal_tasks`, taken ONCE at the top of that loop body and
        // shared with B3 grounding + the judge's `<tool_activity>` block)
        // rather than taken here. `take_native_evidence` is remove-once —
        // calling it a second time here would always observe `None` (the
        // caller already removed the entry), silently degrading every
        // settled round from `full` back to `mcp_only`.
        let observation = crate::prediction::task_observe::observe_round(
            self.home_dir.as_deref(),
            &agent_id,
            runtime,
            &task.id,
            round,
            (&since, &until),
            outcome,
            observed_artifact,
            None,
            native_evidence,
        );

        // ── P0/WP-C fault attribution (The Misattribution Gap,
        // arXiv:2605.22842; Model or Harness?, arXiv:2607.28802) ──
        // The observation fidelity only exists once `observe_round` has run,
        // so the write-time classification happens here — one deterministic,
        // zero-LLM call. `config.toml [evolution] fault_attribution`
        // (default true) gates it; with it `false` the side is forced to
        // `Model` and every gate below is byte-identical to the pre-WP-C
        // pipeline. `home_dir` absent (test construction paths) ⇒ `Model`
        // for the same reason.
        let (fault_side, fault_reason) = match self.home_dir.as_deref() {
            Some(home) if crate::fault_attribution::enabled_from_home(home) => {
                // A capability gate refusing a tool call inside this round's
                // window is a harness fault (R3). Read from the same
                // `tool_calls.jsonl` audit trail every other evidence
                // consumer uses — the denial rows carry `error_class`, which
                // the `ToolActivityRecord` projection deliberately drops.
                let capability_blocked =
                    crate::fault_attribution::capability_blocked_in_window(home, &agent_id, &since);
                let (side, reason) = fault_ctx.classify(
                    observation.fidelity,
                    native_evidence.map(|e| e.len()).unwrap_or(0),
                    capability_blocked,
                );
                if side != crate::fault_attribution::FaultSide::Model {
                    // Audited (not merely logged) because this decision
                    // SUPPRESSES learning — an operator must be able to see
                    // which rounds were excluded and why.
                    crate::fault_attribution::log_fault_attributed(
                        home, &agent_id, &task.id, round, side, reason,
                    );
                    info!(
                        task = %task.id, round, fault_side = side.as_str(), reason,
                        "WP-C 歸因：本輪失敗不歸咎模型 → 排除學習信號"
                    );
                }
                (side, reason)
            }
            _ => (crate::fault_attribution::FaultSide::Model, "model"),
        };

        // WP-P2 (commercial/docs/DESIGN-lwm-calibration-2026-08-10.md §4):
        // loaded fresh here — same convention as `rule_induction_enabled`
        // below — rather than threaded through `TaskForwardModel`'s
        // constructor, since this is the one place both the settle-time
        // score and the transition-write score are needed. `home_dir`
        // absent (never happens in production, but tests construct this
        // struct without one) ⇒ config default ⇒ `false`.
        let calibration_enabled = self
            .home_dir
            .as_deref()
            .map(crate::prediction::task_forward_store::TaskForwardModelConfig::from_home)
            .unwrap_or_default()
            .calibration_enabled;

        let thresholds = crate::prediction::metacognition::AdaptiveThresholds::default();
        match crate::prediction::task_forward::diff(prediction, observation, &thresholds) {
            crate::prediction::task_forward::DiffOutcome::Unobservable { reason } => {
                debug!(task = %task.id, round, reason, "A3 settle: unobservable this round");
            }
            crate::prediction::task_forward::DiffOutcome::Computed(error) => {
                if let Err(e) = fm.settle_prediction(&error, calibration_enabled).await {
                    warn!(task = %task.id, round, error = %e, "A3 settle_prediction failed (non-fatal)");
                }
                if let Some(home) = &self.home_dir {
                    let db_path = home.join("memory.db");
                    match duduclaw_memory::SqliteMemoryEngine::new(&db_path) {
                        Ok(engine) => {
                            // WP-P3 + v1.54: read the held-out gate once, shared
                            // by the injected-rule settle routing below AND the
                            // shadow→promotion pass further down. Defaults ON
                            // (v1.54); when off, the injected path runs the
                            // unchanged `ErrorCategory`-credit lifecycle and the
                            // shadow pass is skipped entirely (byte-identical).
                            let held_out_gate_enabled =
                                crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(home)
                                    .held_out_gate_enabled;

                            if crate::prediction::transition::should_write_transition(&error) {
                                if let Err(e) = crate::prediction::transition::write_transition(
                                    &engine,
                                    &agent_id,
                                    &error,
                                    judge_feedback,
                                    calibration_enabled,
                                )
                                .await
                                {
                                    warn!(task = %task.id, round, error = %e, "A3 transition write failed (non-fatal)");
                                }
                            }

                            // ── WP-A4 prune: settle whichever task rules were
                            // injected into THIS round's dispatch prompt
                            // (`goal_loop.rs`'s injection step, bookkept via
                            // `fm.record_injected_task_rules`). Reuses the
                            // SAME unmodified `rule_lifecycle::
                            // settle_injected_rules` channel-reply's F2a
                            // already uses — task-layer rules ride the
                            // identical rule_stats/Janus-probation/
                            // net-zero-retirement lifecycle (design §6.5
                            // T9). Empty when nothing was injected this
                            // round (rule_induction off, or no active rules
                            // existed) — the settle call is then a no-op.
                            // ── P0/WP-C gate 1 ──
                            // `take_injected_task_rules` is remove-once, so
                            // it is called unconditionally (the bookkeeping
                            // entry must be consumed either way); only the
                            // *credit assignment* below is suppressed. A
                            // round whose failure was the grader's / the
                            // environment's / the harness's fault — or whose
                            // fault side is `Unknown` — carries no signal
                            // about whether the injected rules helped, so it
                            // settles as NO evidence: no `harmful` increment,
                            // and deliberately NOT re-labelled as `helpful`
                            // either (mapping it to a benign ErrorCategory
                            // would credit the rules for a round they never
                            // influenced). Scoped to the two error
                            // categories that actually produce a harmful
                            // increment / a high-risk label; a
                            // Negligible/Moderate round keeps its existing
                            // helpful credit, which no misattribution can
                            // turn into a false rule.
                            let harmful_class = matches!(
                                error.category,
                                crate::prediction::engine::ErrorCategory::Significant
                                    | crate::prediction::engine::ErrorCategory::Critical
                            );
                            //
                            // Review finding 13 (P2): `Unknown` is the BLIND
                            // case (`fidelity == None` ⇒ R0), which every
                            // team round hits structurally — the members'
                            // audit rows are under their own ids and
                            // `observe_round` still reads the employee alone.
                            // Suppressing only the harmful side there gave
                            // "只記功不記過": a Negligible/Moderate blind
                            // round kept crediting its injected rules
                            // `helpful` on an observation that saw nothing.
                            // A blind round carries no evidence in EITHER
                            // direction, so it settles as no evidence at all.
                            let blind_observation =
                                fault_side == crate::fault_attribution::FaultSide::Unknown;
                            let skip_learning = blind_observation
                                || (harmful_class
                                    && fault_side != crate::fault_attribution::FaultSide::Model);
                            if skip_learning {
                                debug!(
                                    task = %task.id, round,
                                    fault_side = fault_side.as_str(), reason = fault_reason,
                                    "WP-C：跳過本輪規則信用結算與 shadow 評分（非模型過失）"
                                );
                            }

                            let injected_ids = fm.take_injected_task_rules(&task.id, round).await;
                            if !injected_ids.is_empty() && !skip_learning {
                                // WP-P3: when the held-out rule gate is on,
                                // settlement routes through the numeric-oracle
                                // gate (`settle_injected_rules_held_out`)
                                // instead of the pure ErrorCategory-credit
                                // lifecycle. Gate off ⇒ the unchanged
                                // `settle_injected_rules` runs, byte-identical.
                                let retired = if held_out_gate_enabled {
                                    crate::prediction::rule_lifecycle::settle_injected_rules_held_out(
                                        &engine,
                                        &agent_id,
                                        &injected_ids,
                                        error.category,
                                        // Family size for the Bonferroni
                                        // correction: the batch of rules
                                        // trialed together this round (a
                                        // conservative proxy for the concurrent
                                        // candidate family).
                                        injected_ids.len(),
                                        crate::prediction::rule_gate::DEFAULT_BASELINE_HIT_RATE,
                                        Utc::now().timestamp().max(0) as u64,
                                    )
                                    .await
                                } else {
                                    crate::prediction::rule_lifecycle::settle_injected_rules(
                                        &engine,
                                        &agent_id,
                                        &injected_ids,
                                        error.category,
                                    )
                                    .await
                                };
                                if !retired.is_empty() {
                                    debug!(
                                        task = %task.id, round, retired = retired.len(),
                                        "A4 task-rule settle retired net-zero rules"
                                    );
                                }
                            }

                            // ── v1.54 shadow → promotion pass (DESIGN-lwm-
                            // calibration §6). Scores the *other* population:
                            // active shadow task-layer rules whose goal_kind
                            // signal matches THIS round's situation. A shadow
                            // rule's implicit prediction is "signal match ⇒
                            // high-risk", so its out-of-sample hit is the round
                            // actually being high-risk; when its record beats
                            // the frozen climatology baseline it is promoted
                            // out of shadow. Runs BEFORE the induce step below,
                            // so a rule born THIS settle is never scored this
                            // settle ("誕生於本次 settle 之前"). Gate off ⇒
                            // skipped ⇒ byte-identical. Best-effort like every
                            // other side-effect in this hook.
                            //
                            // P0/WP-C gate 2: a shadow candidate's implicit
                            // prediction is "signal match ⇒ high risk", and
                            // its out-of-sample hit is the round actually
                            // being high-risk. When the round's high-risk
                            // label came from the grader / the environment /
                            // the harness, grading against it would promote
                            // or retire candidates on a label the model never
                            // produced — the same misattribution, one layer
                            // deeper. Same `skip_learning` predicate as gate 1.
                            if held_out_gate_enabled && !skip_learning {
                                // Frozen climatology baseline (fraction of this
                                // agent's settled rounds that were high-risk).
                                // Falls back to the domain-agnostic 0.5
                                // coin-flip until enough history accumulates.
                                let baseline = fm
                                    .high_risk_base_rate(
                                        &agent_id,
                                        crate::prediction::rule_gate::MIN_HELD_OUT_SAMPLES,
                                    )
                                    .await
                                    .unwrap_or(
                                        crate::prediction::rule_gate::DEFAULT_BASELINE_HIT_RATE,
                                    );
                                let match_tag = crate::prediction::task_rule_induce::goal_kind_tag(
                                    error.prediction.state_key.goal_kind,
                                );
                                let now_seq = Utc::now().timestamp().max(0) as u64;
                                let pass =
                                    crate::prediction::rule_lifecycle::score_shadow_candidates_for_task(
                                        &engine,
                                        &agent_id,
                                        &match_tag,
                                        error.category,
                                        baseline,
                                        now_seq,
                                    )
                                    .await;
                                if pass.scored > 0 {
                                    debug!(
                                        task = %task.id, round,
                                        scored = pass.scored,
                                        promoted = pass.promoted.len(),
                                        retired = pass.retired.len(),
                                        baseline,
                                        "v1.54 shadow→promotion pass"
                                    );
                                }
                            }
                        }
                        Err(e) => warn!(
                            task = %task.id, round, error = %e,
                            "A3 transition: memory.db open failed (non-fatal)"
                        ),
                    }

                    // ── WP-A4 induce: opens its own embedder-attached engine
                    // handle internally (mirrors `reflexion.rs`'s own
                    // independent-instance convention) — gated on the
                    // `[task_forward_model] rule_induction` sub-switch
                    // (design §6.5; defaults true, A3 `enabled` already
                    // gates the outer `if let Some(fm) = ...` this whole
                    // match lives inside).
                    let rule_induction_enabled =
                        crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(
                            home,
                        )
                        .rule_induction;
                    if rule_induction_enabled {
                        if let Err(e) = crate::prediction::task_rule_induce::maybe_induce_task_rule(
                            &db_path, &error,
                        )
                        .await
                        {
                            warn!(task = %task.id, round, error = %e, "A4 induce failed (non-fatal)");
                        }
                    }
                }
            }
        }
    }

    /// A1 leftover (see `goal_loop/state.rs`'s "Honesty note" doc comment):
    /// read-merge-write append `facts` onto the task's persisted
    /// `GoalStateSnapshot.confirmed_facts`, CJK-safe truncated to ≤120
    /// chars each and capped to the 6 most recent overall. A single call
    /// per review pass (the caller batches this round's facts into one
    /// `Vec` first) — avoids the read-merge-write race that calling this
    /// once per fact would hit (each call would read the DB row as it
    /// stood before any of this round's writes, so a second call would
    /// clobber the first). Best-effort: a store failure here must never
    /// affect the review verdict.
    ///
    /// M7 migration: previously did its own read-then-`set_goal_state_json`-
    /// the-whole-blob, which raced `goal_loop.rs::capture_round_state`'s
    /// `pending_hypotheses` write onto the SAME `goal_state_json` column —
    /// whichever writer's `UPDATE` landed second won with a value computed
    /// from a stale read, silently discarding the other writer's field (the
    /// exact lost-update `task_store.rs::merge_goal_state_json` was built to
    /// close; see that method's doc comment, which named this call site as
    /// the follow-up). Now touches ONLY the `confirmed_facts` key inside the
    /// merge closure — reading it fresh under the store's connection lock
    /// rather than trusting the caller-supplied `goal_state_json` snapshot,
    /// so a concurrent `pending_hypotheses` write is never clobbered.
    pub(super) async fn persist_confirmed_facts(&self, task_id: &str, facts: &[String]) {
        if facts.is_empty() {
            return;
        }
        let capped_facts: Vec<String> = facts
            .iter()
            .map(|f| duduclaw_core::truncate_chars(f, 120))
            .collect();
        const MAX_CONFIRMED_FACTS: usize = 6;
        if let Err(e) = self
            .store
            .merge_goal_state_json(task_id, move |v| {
                let mut existing: Vec<String> = v
                    .get("confirmed_facts")
                    .and_then(|cf| cf.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                existing.extend(capped_facts);
                if existing.len() > MAX_CONFIRMED_FACTS {
                    let drop = existing.len() - MAX_CONFIRMED_FACTS;
                    existing.drain(0..drop);
                }
                v["confirmed_facts"] = serde_json::json!(existing);
            })
            .await
        {
            debug!(
                task = task_id, error = %e,
                "dispatch_engine: confirmed_facts persist failed (non-fatal)"
            );
        }
    }

    /// WP3 (PORTICO): revoke every capability grant bound to a task when its
    /// phase closes (accept / reject / needs_human). No-op when no `home_dir`
    /// is wired (tests) or when the store cannot be opened — a grant that fails
    /// to revoke still dies at its hard TTL (bounded), so a store error here
    /// degrades gracefully rather than failing the review tick.
    pub(super) async fn revoke_task_grants(&self, task_id: &str) {
        let Some(home) = &self.home_dir else {
            return;
        };
        match crate::capability_grants::CapabilityGrantStore::open(home) {
            Ok(store) => {
                if let Err(e) = store
                    .revoke_for_task(task_id, crate::capability_grants::REVOKE_REASON_PHASE_END)
                    .await
                {
                    warn!(task = %task_id, error = %e, "capability grant revoke on task phase end failed");
                }
            }
            Err(e) => {
                warn!(task = %task_id, error = %e, "capability grant store open failed on task phase end")
            }
        }
    }
}
