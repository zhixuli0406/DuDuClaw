//! One driver pass — [`GoalLoopDriver::tick_once`].
//! Moved verbatim out of `goal_loop.rs`. This file holds a single,
//! indivisible function, so it is the one split file over the 800-line
//! target: splitting it further would mean refactoring the function.

use super::*;

impl GoalLoopDriver {
    /// One driver pass. Public for tests and one-shot recovery.
    pub async fn tick_once(&self) -> Result<(), String> {
        let now = Utc::now();

        // ── needs_human reconciliation ──────────────────────────
        // Detects the state transition INTO needs_human — from either this
        // driver's escalate() OR the DispatchEngine's judge-rejection path — and
        // pushes an approval to the agent's channel (Observer: notify-only, auto
        // close). Runs before dispatch so a task escalated this tick is notified
        // next tick (avoids double-processing within one tick).
        self.reconcile_needs_human().await;

        // Candidates: goal_mode tasks awaiting a run, assigned to a concrete
        // agent. `todo` = freshly created; `pending` = a durable claim awaiting
        // pickup; `revising` = returned from a judge rejection (Iterative Kanban)
        // for the next round. Reuses the existing status query so no new store
        // method is needed.
        let mut candidates: Vec<TaskRow> = Vec::new();
        for status in ["todo", "pending", "revising"] {
            for t in self.store.tasks_in_status(status).await? {
                if t.goal_mode && !t.assigned_to.trim().is_empty() {
                    candidates.push(t);
                }
            }
        }
        let candidate_ids: HashSet<String> = candidates.iter().map(|t| t.id.clone()).collect();

        // Prune kickoff bookkeeping for tasks that are no longer awaiting a run
        // (dispatched / terminal). A task still awaiting a run keeps its entry so
        // an already-approved kickoff deferred by the concurrency cap is not
        // re-requested next tick (poll of the terminal-approved approval simply
        // returns Approved again).
        {
            let mut kickoff = self.kickoff.lock().await;
            kickoff.retain(|id, _| candidate_ids.contains(id));
        }
        // Same pruning for the kickoff notification delivery/retry state —
        // a task that left the candidate set (dispatched or aborted) has no
        // further use for either.
        {
            let mut kn = self.kickoff_notified.lock().await;
            kn.retain(|id| candidate_ids.contains(id));
        }
        {
            let mut kr = self.kickoff_retry.lock().await;
            kr.retain(|id, _| candidate_ids.contains(id));
        }
        // A1/A2: a task back in the candidate set is no longer "sitting in
        // review" — clear its capture-done flag so the NEXT time it reaches
        // `review` (a later round), `capture_round_state` runs again.
        {
            let mut seen = self.state_capture_seen.lock().await;
            seen.retain(|id| !candidate_ids.contains(id));
        }

        let mut inflight = self.inflight.lock().await;

        // ── Reconcile: prune finished/escalated entries, and mark picked-up
        //    tasks (moved to in_progress/review) as no longer awaiting pickup so
        //    they still count against concurrency but are not re-dispatched. ──
        // Tasks escalated by the dispatch-failure path in THIS tick: the
        // candidate list above was read while they were still `todo`, so
        // without this set the loop below would re-dispatch a task that was
        // just parked `needs_human`.
        let mut escalated_this_tick: HashSet<String> = HashSet::new();
        let tracked: Vec<String> = inflight.keys().cloned().collect();
        for id in tracked {
            // ── Synchronous dispatch failure: the dispatcher could not even
            //    hand the round to the runtime (`message dispatch failed`).
            //    Free the slot NOW — before the candidate check below, because
            //    such a task is still `todo` and therefore a candidate — so the
            //    Personal-edition cap is not held by a round that never ran. ──
            if let Some(failed_error) = self.dispatch_failure_for(&inflight, &id).await {
                if self
                    .on_dispatch_failed(&mut inflight, &id, &failed_error)
                    .await?
                {
                    escalated_this_tick.insert(id.clone());
                }
                continue;
            }
            if candidate_ids.contains(&id) {
                continue; // still a candidate — handled below
            }
            let task_opt = self.store.get_task(&id).await?;
            let status = task_opt
                .as_ref()
                .map(|t| t.status.clone())
                .unwrap_or_else(|| "done".to_string());
            match status.as_str() {
                // Agent claimed it — keep counted as in-flight, stop awaiting a
                // fresh dispatch. No progress push (dispatched already said so).
                "in_progress" => {
                    if let Some(e) = inflight.get_mut(&id) {
                        e.awaiting_pickup = false;
                    }
                    // The round reached the runtime — an earlier synchronous
                    // failure streak is over.
                    self.dispatch_failures.lock().await.remove(&id);
                    // H22: the task IS claimed and running — the stall guard
                    // (`stalled_secs`) no longer applies here, so a silent
                    // long-runner had no visible signal at all until it
                    // finished. Report, never intervene.
                    if let Some(t) = &task_opt {
                        self.maybe_report_no_progress(&mut inflight, t, now).await;
                    }
                }
                // Under acceptance review — push the "驗收中" progress once,
                // and (A1/A2, once per review sitting) capture this round's
                // state for the visit graph + any self-reported hypotheses.
                "review" => {
                    if let Some(e) = inflight.get_mut(&id) {
                        e.awaiting_pickup = false;
                    }
                    if let Some(t) = &task_opt {
                        let first_capture = {
                            let mut seen = self.state_capture_seen.lock().await;
                            seen.insert(id.clone())
                        };
                        if first_capture {
                            self.capture_round_state(t).await;
                        }
                        self.push_progress(
                            t,
                            "review",
                            crate::goal_notify::GoalProgress::Reviewing,
                        )
                        .await;
                    }
                }
                // Judge-accepted / human-marked done — push the ✅ result and
                // drop all tracking (terminal) ONLY once the push is actually
                // delivered (or permanently given up on). A transient send
                // failure used to be indistinguishable from success here —
                // tracking was dropped immediately regardless, so the final
                // answer was lost for good the moment one HTTP call blipped.
                // Leaving the task tracked lets the next tick's "done" branch
                // retry the push.
                "done" => {
                    let handled = match &task_opt {
                        Some(t) => {
                            self.push_progress(t, "done", crate::goal_notify::GoalProgress::Done)
                                .await
                        }
                        None => true,
                    };
                    if handled {
                        if let Some(removed) = inflight.remove(&id) {
                            self.release_lease(&removed); // RFC-27: free the slot
                        }
                        self.progress_seen.lock().await.remove(&id);
                        self.clear_progress_retries(&id).await;
                        // A2 lifecycle: task reached a terminal state — drop
                        // its visit-graph tracking.
                        self.visit_graph.clear_task(&id).await;
                        // L3: `state_capture_seen` is pruned at the TOP of
                        // `tick_once` by `retain(|id| !candidate_ids.contains(id))`
                        // — which only clears an id when it RE-ENTERS the
                        // candidate set (todo/pending/revising). A task that
                        // goes review → done never becomes a candidate again,
                        // so without this explicit removal its entry would
                        // never be pruned and `state_capture_seen` would grow
                        // unboundedly over a long-running gateway's lifetime.
                        self.state_capture_seen.lock().await.remove(&id);
                    }
                }
                // Other terminal / escalated states (cancelled / failed /
                // needs_human) — no longer the driver's dispatch concern.
                // needs_human progress is pushed by reconcile_needs_human.
                _ => {
                    if let Some(removed) = inflight.remove(&id) {
                        self.release_lease(&removed); // RFC-27: free the slot
                    }
                    self.clear_progress_retries(&id).await;
                    // A2 lifecycle: cleanup on terminal/escalated states too
                    // (needs_human here may have come from DispatchEngine's
                    // own judge-retry-budget path, not this driver's
                    // `escalate()`, so it needs its own cleanup call).
                    self.visit_graph.clear_task(&id).await;
                    // L3: same leak as the `done` arm above — a task that
                    // lands in cancelled/failed/needs_human never re-enters
                    // the candidate set, so the top-of-tick prune never
                    // reaches it either.
                    self.state_capture_seen.lock().await.remove(&id);
                }
            }
        }

        // In-flight goal tasks currently tracked (drives the concurrency admission gate).
        let mut active = inflight.len();

        // ── RFC-27: renew the edition concurrency lease of every still-tracked
        //    task so a live long-running goal never loses its slot to the
        //    crash-recovery TTL. Runs after the reconcile loop above (terminal
        //    tasks already released their leases), so only survivors are
        //    renewed. No-op when the gate is unwired or a lease is unguarded. ──
        if self.concurrency_limit.is_some() {
            for entry in inflight.values() {
                if let Some(lease) = &entry.lease {
                    duduclaw_core::concurrency_renew(
                        &self.home_dir,
                        lease,
                        self.concurrency_ttl_secs,
                    );
                }
            }
        }

        // ── D4 item 1: dependency-status map (LLMCompiler DAG) ──
        // Only built when some candidate actually carries dependencies, so the
        // common (no-DAG) path stays a single query. Maps every task id → status
        // so a candidate's `depends_on` can be resolved to done / in-flight /
        // terminally-failed without N per-dep lookups.
        let any_deps = candidates
            .iter()
            .any(|t| !parse_depends_on(&t.depends_on).is_empty());
        let status_by_id: HashMap<String, String> = if any_deps {
            self.store
                .list_tasks(None, None, None)
                .await?
                .into_iter()
                .map(|t| (t.id, t.status))
                .collect()
        } else {
            HashMap::new()
        };

        // ── D4 item 2: roster (only when a non-default policy is wired) ──
        let roster: Vec<String> = if self.policy.is_some() {
            crate::dispatch_policy::list_roster(&self.home_dir)
        } else {
            Vec::new()
        };

        for task in &candidates {
            // ── W3-1 D5: a human holds this task's conversation ──
            // Freeze, do not escalate: the person who took over IS the human
            // an escalation would page, and parking the task `needs_human`
            // would fire a card at them mid-conversation. The task simply
            // waits; the next tick after the window closes picks it up
            // unchanged. Checked before the deadline guard so a long takeover
            // cannot silently burn a task's wall clock into an escalation.
            if let (Some(ch), Some(cid)) = (
                task.source_channel.as_deref(),
                task.source_chat_id.as_deref(),
            ) {
                if crate::takeover::is_target_paused(&self.home_dir, ch, cid) {
                    crate::takeover::log_skip("goal_loop.dispatch", ch, cid, &task.id);
                    continue;
                }
            }

            // ── Wall-clock guard (from created_at) + G3 per-task deadline ──
            // `deadline_at` (design §6 G3) overrides the global wall clock —
            // whichever is earlier fires first; the escalation message names
            // which one actually hit so a human sees a meaningful reason
            // rather than one generic "deadline" for both.
            if let Some(hit) = resolve_deadline_hit(
                &task.created_at,
                task.deadline_at.as_deref(),
                self.config.wall_clock_hours,
                now,
            ) {
                let reason = match hit {
                    DeadlineHit::TaskDeadline => "時限已到未通過驗收",
                    DeadlineHit::WallClock => "goal-loop deadline",
                };
                // H11: both halves are a time budget running out — one class,
                // two different human-readable reasons.
                self.escalate(
                    &mut inflight,
                    task,
                    reason,
                    crate::pause_reason::PauseReason::BudgetExhausted,
                )
                .await?;
                active = inflight.len();
                continue;
            }

            // ── D4 item 1: dependency gate (LLMCompiler DAG) ──
            // A task is dispatchable only when every `depends_on` id is `done`.
            // If a dependency is terminally stuck (failed / cancelled /
            // needs_human) or missing, the downstream task inherits the
            // escalation (never orphaned): it is parked `needs_human` too so a
            // human sees the whole blocked branch. If dependencies are merely
            // still running, the task is frozen (skipped) this tick.
            if any_deps {
                let deps = parse_depends_on(&task.depends_on);
                if !deps.is_empty() {
                    let mut unmet: Vec<String> = Vec::new();
                    let mut blocked_by: Option<String> = None;
                    for d in &deps {
                        match status_by_id.get(d).map(String::as_str) {
                            Some("done") => {}
                            // Terminally-failed / missing upstream ⇒ inherit escalate.
                            Some("failed") | Some("cancelled") | Some("needs_human") | None => {
                                blocked_by = Some(d.clone());
                                break;
                            }
                            // Still in progress (todo/pending/in_progress/review/blocked).
                            Some(_) => unmet.push(d.clone()),
                        }
                    }
                    if let Some(dep) = blocked_by {
                        let short = duduclaw_core::truncate_chars(&dep, 8);
                        self.post_activity(
                            "goal_loop.dep_blocked",
                            &task.assigned_to,
                            Some(&task.id),
                            &format!("上游依賴 #{short} 未能完成,凍結並轉人工 — {}", task.title),
                        )
                        .await;
                        // H11: nothing this agent can do — another task has to
                        // be unstuck first, which is a human decision.
                        self.escalate(
                            &mut inflight,
                            task,
                            &format!("goal-loop upstream dependency failed: {dep}"),
                            crate::pause_reason::PauseReason::BlockedNeedsDecision,
                        )
                        .await?;
                        active = inflight.len();
                        continue;
                    }
                    if !unmet.is_empty() {
                        debug!(
                            task = %task.id,
                            unmet = unmet.len(),
                            "goal loop: task frozen — dependencies not yet done"
                        );
                        continue; // frozen: deps still running
                    }
                }
            }

            // ── D4 item 2: resolve the agent via the dispatch policy ──
            // Default (no policy) ⇒ `task` unchanged (dispatch to `assigned_to`).
            // A policy may re-route to another roster member; the reassignment is
            // persisted so downstream (heartbeat pull, activity) is consistent.
            let reassigned;
            let task: &TaskRow = match &self.policy {
                Some(policy) => match policy.select(task, &roster).await {
                    Some(sel) if !sel.trim().is_empty() && sel != task.assigned_to => {
                        match self
                            .store
                            .update_task(
                                &task.id,
                                &serde_json::json!({ "assigned_to": sel.clone() }),
                            )
                            .await
                        {
                            Ok(_) => {
                                self.post_activity(
                                    "goal_loop.reassigned",
                                    &sel,
                                    Some(&task.id),
                                    &format!(
                                        "dispatch policy {} 改派 {} → {} — {}",
                                        policy.kind().as_str(),
                                        task.assigned_to,
                                        sel,
                                        task.title
                                    ),
                                )
                                .await;
                                let mut t = task.clone();
                                t.assigned_to = sel;
                                reassigned = t;
                                &reassigned
                            }
                            Err(e) => {
                                warn!(task = %task.id, error = %e, "goal loop: policy reassignment persist failed — keeping original assignment");
                                task
                            }
                        }
                    }
                    _ => task,
                },
                None => task,
            };

            // ── D4 item 3: per-task iteration cap (MaAS dynamic depth) ──
            let iter_cap = self.iteration_cap_for(task);

            // ── Autonomy level (per-agent, from agent.toml) ──
            let level = AutonomyLevel::for_agent(&self.home_dir, &task.assigned_to);

            // Operator: the loop never auto-drives this agent. Announce once,
            // then leave the task alone (a human drives it manually).
            if level == AutonomyLevel::Operator {
                let mut skipped = self.operator_skipped.lock().await;
                let first = skipped.insert(task.id.clone());
                drop(skipped);
                if first {
                    self.post_activity(
                        "goal_loop.operator_skipped",
                        &task.assigned_to,
                        Some(&task.id),
                        &format!("Operator 模式:goal loop 不自主驅動此任務 — {}", task.title),
                    )
                    .await;
                }
                continue;
            }

            if escalated_this_tick.contains(&task.id) {
                continue;
            }
            let entry = inflight.get(&task.id).cloned();
            let is_new = entry.is_none();

            if is_new && self.in_dispatch_backoff(&task.id, now).await {
                continue;
            }

            // Collaborator/Consultant: gate the FIRST dispatch behind a human
            // kickoff approval. Waiting/Aborted ⇒ do not dispatch this tick.
            if is_new && level.requires_kickoff() {
                match self.kickoff_gate(task).await? {
                    KickoffGate::Waiting | KickoffGate::Aborted => continue,
                    KickoffGate::Proceed => {
                        // WP3 (PORTICO): kickoff cleared → mint any task-scoped
                        // grants the task declared (tags `grant:<tool>`). Idempotent
                        // per (task, tool) so a concurrency-deferred re-entry is safe.
                        self.grant_kickoff_tools(task).await;
                    }
                }
            }

            // Should we dispatch this task on this tick?
            let should_dispatch = match &entry {
                None => true, // never dispatched
                Some(e) if e.awaiting_pickup => {
                    // Already enqueued and not yet picked up: only re-dispatch if
                    // the pickup has stalled.
                    (now - e.enqueued_at).num_seconds() >= self.config.stalled_secs
                }
                // Tracked but not awaiting pickup ⇒ it came back to a candidate
                // state (judge rejection returned it to `pending`): re-dispatch
                // immediately — this is the tight retry loop.
                Some(_) => true,
            };
            if !should_dispatch {
                continue;
            }

            // ── A1: build this round's structured `<state>` block ──
            // Computed once per candidate per tick (before any escalation
            // decision) and reused both for the A2 loop-detection checks
            // right below AND for the actual dispatch payload further down.
            let iterations = self.store.list_iterations(&task.id).await?;
            let goal_snapshot = GoalStateSnapshot::from_json(task.goal_state_json.as_deref());
            let mut state_block = goal_state::build_state_block(task, &iterations, &goal_snapshot);
            let state_hash = goal_state::state_hash(&state_block);

            // ── A2 no-progress guard (Graph-Based Exploration arXiv:2512.24156) ──
            // Replaces the old two-round identical-`judge_feedback`-text
            // oscillation check. `state_hash` folds in the goal, any
            // self-reported hypotheses, and the LATEST rejection reason
            // (see `goal_loop/state.rs::StateBlock::hash_input`), so "state
            // unchanged" is a strictly stronger signal than "feedback text
            // unchanged": a judge that rewords the same underlying problem
            // still counts as unchanged, and genuinely new information
            // (fresh rejection reason, or an updated self-reported
            // hypothesis) always resets it. M3: escalates when this round
            // WOULD be the 2nd consecutive dispatch with a byte-identical
            // state — i.e. this round's rejection would repeat the exact
            // same state the previous rejection already produced. Kept at
            // n=2 (not n=3) to match the pre-A2 guard's timing exactly: that
            // guard escalated the moment TWO consecutive judge rejections
            // carried identical feedback text, before ever attempting a 3rd
            // dispatch with the repeated information — an earlier n=3
            // threshold here let one extra, provably-useless round dispatch
            // before escalating.
            // Gated on `is_rejection_redispatch` exactly like the guard it
            // replaces: a stalled-pickup redispatch means the agent never
            // engaged this round, which is not evidence of "no progress".
            //
            // External contract kept byte-identical on purpose — same
            // activity `event_type` (`goal_loop.oscillation`) and same
            // `judge_feedback` reason text as before A2 — because
            // `topology_evolution.rs` (D5, out of scope for this change)
            // queries that exact event-type string for its own analytics.
            let is_rejection_redispatch = matches!(&entry, Some(e) if !e.awaiting_pickup);
            if is_rejection_redispatch {
                let would_be_streak = self.visit_graph.peek_streak(&task.id, &state_hash).await;
                if would_be_streak >= 2 {
                    self.post_activity(
                        "goal_loop.oscillation",
                        &task.assigned_to,
                        Some(&task.id),
                        &format!(
                            "goal-loop 偵測到狀態連續 {would_be_streak} 輪未變(目標／已確認事實／待驗證假設／最新駁回理由皆相同),無進展 — 轉人工 {}",
                            task.title
                        ),
                    )
                    .await;
                    self.escalate(
                        &mut inflight,
                        task,
                        "goal-loop no-progress oscillation",
                        crate::pause_reason::PauseReason::NoProgress,
                    )
                    .await?;
                    active = inflight.len();
                    continue;
                }
            }

            // ── A2 repeated-action annotation ──
            // Whenever this round's state already has SOME recorded action
            // repeated ≥2 times (from any earlier round, not gated to
            // rejection-redispatch), flag it explicitly in the dispatch
            // prompt's excluded-approaches section — a softer signal than
            // the escalate above: keep retrying, but stop repeating the
            // specific thing that already failed twice from this exact
            // state.
            if self
                .visit_graph
                .has_repeated_action(&task.id, &state_hash)
                .await
            {
                state_block.loop_warning =
                    Some("此狀態下已重複嘗試相同做法且失敗,請勿重複,改用不同做法".to_string());
            }

            // ── Iteration guard (difficulty-scaled cap, D4 item 3) ──
            let current_iter = entry.as_ref().map(|e| e.iter).unwrap_or(0);
            if current_iter >= iter_cap {
                // H11: a hard cap fired (same family as the deadline guard).
                self.escalate(
                    &mut inflight,
                    task,
                    "goal-loop iteration cap",
                    crate::pause_reason::PauseReason::BudgetExhausted,
                )
                .await?;
                active = inflight.len();
                continue;
            }

            // ── Concurrency guard (only gates NEW admissions; re-dispatch of an
            //    already-tracked task does not add to the in-flight count) ──
            if is_new && active >= self.config.max_concurrent {
                debug!(
                    task = %task.id,
                    active,
                    cap = self.config.max_concurrent,
                    "goal loop: concurrency cap reached, deferring new goal task"
                );
                continue;
            }

            // ── RFC-27 edition concurrency gate (cross-process, edition-aware) ──
            // Checked AFTER the cheap in-memory guard above so a candidate that
            // guard already deferred never touches the lease file. A NEW
            // admission takes a cross-process lease; `AtCapacity` defers (queue
            // semantics — a durable goal is a throughput throttle away from
            // running, never dropped). `None` limit ⇒ the whole block is a
            // no-op. A re-dispatch reuses the existing lease (set at the insert
            // site) and is never re-counted.
            let mut acquired_lease: Option<duduclaw_core::ConcurrencyLease> = None;
            if is_new {
                if let Some(limit) = self.concurrency_limit {
                    match duduclaw_core::concurrency_try_acquire(
                        &self.home_dir,
                        CONCURRENCY_CLASS_GOAL,
                        Some(limit),
                        self.concurrency_ttl_secs,
                    ) {
                        duduclaw_core::ConcurrencyAcquireOutcome::Admitted(lease) => {
                            acquired_lease = Some(lease);
                        }
                        duduclaw_core::ConcurrencyAcquireOutcome::AtCapacity {
                            active: gate_active,
                            limit: cap,
                        } => {
                            debug!(
                                task = %task.id,
                                active = gate_active,
                                cap,
                                "goal loop: edition concurrency cap reached, deferring new goal task"
                            );
                            continue;
                        }
                    }
                }
            }

            // ── WP-A9: A3 task-forward-model predict hook (design §4.1) ──
            // `forward_model` is `None` unless `[task_forward_model] enabled
            // = true` (see `handlers.rs`'s construction site) — with it
            // `None`, this entire block is skipped and dispatch behavior is
            // byte-identical to before A3 existed (design §7.3). Even when
            // wired, a failure here is caught and logged, never allowed to
            // block or fail a real dispatch (R5 — same
            // `catch_unwind`-over-`AssertUnwindSafe` discipline as
            // `subagent_prediction::spawn_record`).
            if let Some(fm) = self.forward_model.clone() {
                let phase = if is_rejection_redispatch {
                    RoundPhase::Retry
                } else if !is_new {
                    RoundPhase::Restall
                } else {
                    RoundPhase::First
                };
                let goal_text = format!("{}\n{}", task.title, task.description);
                let goal_kind = derive_goal_kind(&goal_text);
                let has_outcome_spec = crate::outcome_spec::OutcomeSpec::from_tags(&task.tags)
                    .map(|s| !matches!(s, crate::outcome_spec::OutcomeSpec::Text))
                    .unwrap_or(false);
                let state_key = TaskStateKey {
                    agent_id: task.assigned_to.clone(),
                    goal_kind,
                    phase,
                    has_outcome_spec,
                };
                let round = (task.revision_round as u32).saturating_add(1);
                let task_id = task.id.clone();
                let agent_id = task.assigned_to.clone();

                let predict_and_log = async move {
                    let prediction = fm.predict(&task_id, &agent_id, round, state_key).await;
                    if let Err(e) = fm.log_prediction(&prediction).await {
                        warn!(
                            task = %task_id, round, error = %e,
                            "A3 forward-model: predict log failed (non-fatal)"
                        );
                    }
                };
                if let Err(e) = std::panic::AssertUnwindSafe(predict_and_log)
                    .catch_unwind()
                    .await
                {
                    warn!(task = %task.id, "A3 forward-model predict hook panicked: {e:?}");
                }
            }

            // ── WP-A4 rule injection (design §6.5 item 2) ──
            // Independent of the A3 predict hook above (this queries
            // already-induced task-layer rules; it doesn't need this
            // round's fresh prediction) but gated on the SAME
            // `forward_model` presence (A4 is a strict downstream of A3 —
            // design §6.5) plus the `[task_forward_model] rule_induction`
            // sub-switch. Records which rule ids were injected (via
            // `fm.record_injected_task_rules`, an in-memory map on the SAME
            // shared `Arc<TaskForwardModel>` the `DispatchEngine` settle
            // hook reads from — see that field's doc comment in
            // `task_forward_store.rs` for why no new cross-struct wiring is
            // needed) so the settle step can credit/blame them next round.
            // A failure here is caught and logged, never allowed to block
            // or fail a real dispatch (same R5 discipline as the predict
            // hook above).
            let mut task_rule_section: Option<String> = None;
            if let Some(fm) = self.forward_model.clone() {
                let rule_induction_enabled =
                    crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(
                        &self.home_dir,
                    )
                    .rule_induction;
                if rule_induction_enabled {
                    let round = (task.revision_round as u32).saturating_add(1);
                    let task_id = task.id.clone();
                    let agent_id = task.assigned_to.clone();
                    let db_path = self.home_dir.join("memory.db");
                    let home_for_rules = self.home_dir.clone();

                    let inject = async move {
                        // H4: one construction point so `[memory] novelty_gate`
                        // (and the `w_vec` ranking signal it attaches) is honored
                        // here too.
                        let engine =
                            crate::memory_factory::build_memory_engine(&db_path, &home_for_rules)
                                .ok()?;
                        let rules = crate::prediction::rule_lifecycle::select_task_rules(
                            &engine,
                            &agent_id,
                            crate::prediction::rule_lifecycle::TASK_RULE_INJECTION_LIMIT,
                        )
                        .await;
                        if rules.is_empty() {
                            return None;
                        }
                        let ids: Vec<String> = rules.iter().map(|r| r.id.clone()).collect();
                        fm.record_injected_task_rules(&task_id, round, ids).await;
                        let body = rules
                            .iter()
                            .map(|r| format!("- {}", goal_state::xml_escape(&r.content)))
                            .collect::<Vec<_>>()
                            .join("\n");
                        Some(format!("## 任務經驗規則\n{body}"))
                    };
                    match std::panic::AssertUnwindSafe(inject).catch_unwind().await {
                        Ok(section) => task_rule_section = section,
                        Err(e) => warn!(task = %task.id, "A4 rule injection hook panicked: {e:?}"),
                    }
                }
            }

            // ── Belief Loop (design-market-belief-loop-2026-08.md WP3) ──
            // Pre-dispatch calibration section: a programmatic diff of the
            // agent's own settled-belief track record, never left to the
            // agent to recall from memory (§0-1 Honest Lying). Independent
            // of the A3/A4 forward-model gating above — this reads a
            // separate table (`belief_log`) and has no enable flag of
            // its own. Best-effort: a failure here must never block or fail
            // a real dispatch (same R5 discipline as the A3/A4 hooks).
            let mut belief_section: Option<String> = None;
            {
                let db_path = self.home_dir.join("prediction.db");
                let agent_id = task.assigned_to.clone();
                let inject = async move {
                    let stats_db = db_path.clone();
                    let stats_agent = agent_id.clone();
                    let stats = tokio::task::spawn_blocking(move || {
                        crate::prediction::belief::stats(&stats_db, &stats_agent)
                    })
                    .await
                    .ok()?;
                    let section = crate::prediction::belief::render_calibration_section(&stats)?;
                    // Only stamp the injection marker once the section is
                    // actually about to be used in a real dispatch prompt
                    // (design §3 WP3 / §0-2: an evaluable A/B, not an
                    // assumed-effective mechanism).
                    let mark_db = db_path.clone();
                    let mark_agent = agent_id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        crate::prediction::belief::mark_stats_injected(&mark_db, &mark_agent)
                    })
                    .await;
                    Some(section)
                };
                match std::panic::AssertUnwindSafe(inject).catch_unwind().await {
                    Ok(section) => belief_section = section,
                    Err(e) => warn!(
                        task = %task.id,
                        "belief calibration injection panicked: {e:?}"
                    ),
                }
            }

            // ── G2 per-goal risk boundary (design §6, belief-loop
            // sister package) ──
            // Appended UNCONDITIONALLY on every dispatch — this is a
            // programmatic injection, not something the agent is trusted to
            // recall from an earlier turn (Honest Lying, same discipline as
            // the `<state>` block / recent-actions feed). `effective_risk_boundary`
            // is pure string handling (no I/O beyond `baseline_boundary`'s
            // already-fail-open config read) so no panic is reachable here —
            // it can never block or fail a real dispatch.
            let risk_boundary_section = format!(
                "## 本目標風險邊界\n{}\n\n（違反任一條將被驗收判官退回。）",
                effective_risk_boundary(task.risk_boundary.as_deref(), &self.home_dir)
            );

            // ── Dispatch: enqueue a work message on the existing wake-up rail ──
            let next_iter = current_iter + 1;
            let mut state_text = state_block.render();
            if let Some(section) = &task_rule_section {
                state_text.push_str("\n\n");
                state_text.push_str(section);
            }
            if let Some(section) = &belief_section {
                state_text.push_str("\n\n");
                state_text.push_str(section);
            }
            state_text.push_str("\n\n");
            state_text.push_str(&risk_boundary_section);
            // Team-as-Agent P1/WP-4: a task with a frozen team spec whose gate
            // says Team runs this round as planner → executor(s) → verifier
            // instead of one wake-up message. `None` — no spec, gate says
            // Solo, or `[team]` absent entirely (the default) — falls through
            // to the unchanged single-agent dispatch below.
            // A1 ledger: the `<state>` block exactly as dispatched (loop
            // warning included), serialized before the dispatch consumes it.
            let state_block_json = goal_state::state_block_ledger_json(&state_block, &state_hash);
            let (team_dispatch, gate_inputs_json) =
                self.try_team_dispatch(task, next_iter, &state_text).await;
            // Freeze the same title/description/criteria classifier the
            // difficulty-scaled guard used for this actual dispatch.
            let difficulty_text = format!("{}\n{}\n{}", task.title, task.description,
                task.acceptance_criteria.as_deref().unwrap_or(""));
            let difficulty = match crate::dispatch_engine::classify_goal_difficulty(&difficulty_text) {
                crate::dispatch_engine::Difficulty::Simple => "simple",
                crate::dispatch_engine::Difficulty::Complex => "complex",
            };
            let mut gate_inputs = gate_inputs_json.as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| serde_json::json!({}));
            gate_inputs["goal_difficulty"] = serde_json::Value::String(difficulty.to_string());
            let gate_inputs_json = Some(gate_inputs.to_string());
            let ran_as_team = team_dispatch.is_some();
            let (dispatched_message_id, team_dispatched) = match team_dispatch {
                Some((tracking_id, confirmed_team)) => (tracking_id, confirmed_team),
                None => (
                    self.enqueue_work(task, next_iter, &state_text).await?,
                    false,
                ),
            };
            // A2: commit this round's state as the latest dispatched state
            // for the unchanged-streak comparison the NEXT rejection
            // re-dispatch will make (see the peek/commit split in the guard
            // above — commit happens once the dispatch decision is final,
            // mirroring the pre-A2 `last_feedback` commit timing).
            let committed_streak = self
                .visit_graph
                .commit_dispatch(&task.id, &state_hash)
                .await;
            // Iterative Kanban: open this round in the iteration timeline. Round
            // is the judge-rejection counter + 1 (revision_round is bumped by
            // reject_review), idempotent per round so a stall re-dispatch of the
            // same round adds no duplicate. Best-effort telemetry — a failure
            // here must not break dispatch.
            // Carries the visit-graph signal of this dispatch (state hash +
            // the streak `commit_dispatch` just computed) so the round
            // timeline can show "why no progress" after the fact —
            // previously that signal was memory-only and vanished on
            // restart.
            //
            // A1 ledger (2026-09-30): also persists the dispatch ordinal the
            // iteration guard compares against the cap (`next_iter`, stored
            // as `InFlight.iter` below — the guard itself is unchanged),
            // Solo/Team (a grey-band round that later resolves Solo inside the
            // composer is recorded `team` with gate decision `grey_band`),
            // the gate record and the `<state>` block.
            let ledger = crate::task_store::IterationDispatchLedger {
                iter_seq: Some(i64::from(next_iter)),
                team_mode: Some(if ran_as_team { "team" } else { "solo" }.to_string()),
                gate_inputs_json,
                state_block_json: Some(state_block_json),
            };
            if let Err(e) = self
                .store
                .record_iteration_dispatch_with_ledger(
                    &task.id,
                    task.revision_round + 1,
                    &now.to_rfc3339(),
                    Some(&state_hash),
                    Some(committed_streak as i64),
                    &ledger,
                )
                .await
            {
                debug!(task = %task.id, error = %e, "goal loop: iteration dispatch record failed (non-fatal)");
            }
            if is_new {
                active += 1;
            }
            // RFC-27: a NEW admission carries the lease just acquired; a
            // re-dispatch carries forward the lease the tracked entry already
            // holds (never re-acquired, never double-counted).
            let lease = if is_new {
                acquired_lease.take()
            } else {
                inflight.get(&task.id).and_then(|e| e.lease.clone())
            };
            inflight.insert(
                task.id.clone(),
                InFlight {
                    iter: next_iter,
                    enqueued_at: now,
                    awaiting_pickup: true,
                    lease,
                    // H22: a fresh round starts its own silence window.
                    progress_reported_round: None,
                    message_id: Some(dispatched_message_id),
                },
            );

            let has_feedback = task
                .judge_feedback
                .as_deref()
                .map(|f| !f.trim().is_empty())
                .unwrap_or(false);
            let verb = if has_feedback { "重試" } else { "派工" };
            self.post_activity(
                "goal_loop.dispatched",
                &task.assigned_to,
                Some(&task.id),
                &format!(
                    "goal-loop {verb} iter {next_iter}/{iter_cap} — {}",
                    task.title
                ),
            )
            .await;
            // ── P5 outer progress board ──────────────────────
            // A rejection re-dispatch (task returned to `pending` with fresh
            // judge feedback) reads as a single "未通過，重試中" line; a fresh /
            // stall dispatch reads as "開始執行 / 重試". Keyed by iteration so each
            // round posts exactly once.
            let cap = iter_cap;
            if is_rejection_redispatch && has_feedback {
                self.push_progress(
                    task,
                    &format!("rejected:{next_iter}"),
                    crate::goal_notify::GoalProgress::Rejected {
                        iter: next_iter,
                        cap,
                    },
                )
                .await;
            } else if team_dispatched {
                let minutes = self.config.progress_report_minutes.max(1).saturating_mul(3);
                self.push_progress(
                    task,
                    &format!("team-dispatched:{next_iter}"),
                    crate::goal_notify::GoalProgress::TeamDispatched { minutes },
                )
                .await;
            } else {
                self.push_progress(
                    task,
                    &format!("dispatched:{next_iter}"),
                    crate::goal_notify::GoalProgress::Dispatched {
                        iter: next_iter,
                        cap,
                        retry: has_feedback,
                    },
                )
                .await;
            }
            info!(
                task = %task.id,
                agent = %task.assigned_to,
                iter = next_iter,
                retry = has_feedback,
                "goal loop: dispatched work message"
            );
        }

        Ok(())
    }
}
