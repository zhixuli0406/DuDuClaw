//! Pushing progress back to the goal's source conversation, the
//! no-progress reminder, round-state capture and the activity feed.
//! Moved verbatim out of `goal_loop.rs`.

use super::*;

impl GoalLoopDriver {
    /// P5: push one progress line to the goal's source conversation, deduped by
    /// `phase_key` so the same phase never double-posts. Best-effort — a
    /// transient send failure is retried (bounded) on later ticks rather than
    /// being silently treated as delivered.
    ///
    /// Returns `true` once the phase is "handled" (delivered, no destination
    /// configured, or retries exhausted) — callers that gate cleanup on
    /// delivery (the `done` phase) should only release tracking when this is
    /// `true`. Returns `false` while a transient failure is still being
    /// retried, so the caller keeps the task tracked for the next tick.
    pub(super) async fn push_progress(
        &self,
        task: &TaskRow,
        phase_key: &str,
        progress: crate::goal_notify::GoalProgress,
    ) -> bool {
        {
            let seen = self.progress_seen.lock().await;
            if seen.get(&task.id).map(|s| s == phase_key).unwrap_or(false) {
                return true; // already delivered (or given up) for this phase
            }
        }
        let retry_key = format!("{}::{phase_key}", task.id);
        let outcome =
            crate::goal_notify::notify_goal_progress(&self.home_dir, task, progress).await;
        if outcome.is_final() {
            self.progress_seen
                .lock()
                .await
                .insert(task.id.clone(), phase_key.to_string());
            self.progress_retry.lock().await.remove(&retry_key);
            if outcome == crate::goal_notify::NotifyOutcome::NoTarget {
                debug!(task = %task.id, phase = %phase_key, "goal loop: progress push has no notify target");
            }
            return true;
        }
        // SendFailed — bounded retry.
        let mut retries = self.progress_retry.lock().await;
        let count = retries.entry(retry_key.clone()).or_insert(0);
        *count += 1;
        if *count >= NOTIFY_PUSH_MAX_RETRIES {
            warn!(task = %task.id, phase = %phase_key, attempts = *count,
                  "goal loop: progress push failed after max retries, giving up");
            retries.remove(&retry_key);
            drop(retries);
            self.progress_seen
                .lock()
                .await
                .insert(task.id.clone(), phase_key.to_string());
            true
        } else {
            warn!(task = %task.id, phase = %phase_key, attempt = *count,
                  "goal loop: progress push failed, will retry next tick");
            false
        }
    }

    /// H22 (workbuddy-codebuddy §2.5): emit ONE "已執行 X 分鐘未回報進度"
    /// notice for an `in_progress` goal task that has gone quiet.
    ///
    /// ## What counts as "progress"
    ///
    /// The most recent Activity Feed row for this task, floored at the
    /// round's dispatch time. That covers all three producers of a real
    /// signal — the driver's own `goal_loop.dispatched`, the dispatch
    /// engine's review/verdict events, and anything the agent posts itself
    /// via the `activity_post` MCP tool.
    ///
    /// `tasks.updated_at` is deliberately NOT the signal: the dispatch
    /// engine's lease renewer calls `renew_lease` on a timer, which bumps
    /// `updated_at` for every claimed task, so a completely silent agent's
    /// row keeps looking fresh (see [`TaskStore::latest_activity_at`]).
    ///
    /// ## Guarantees
    ///
    /// - **Report only.** Nothing here changes a task's status, re-dispatches
    ///   it, or counts against any cap.
    /// - **At most once per round.** Guarded by `InFlight::progress_reported_round`,
    ///   which a re-dispatch resets along with the rest of the entry.
    /// - **Zero cost when off.** `progress_report_minutes <= 0` returns before
    ///   touching the store.
    /// - **Fail-quiet.** A store error just skips this tick; the notice is a
    ///   courtesy, never a correctness signal.
    /// - Delivery follows [`Self::push_progress`]'s existing retry contract —
    ///   the round is only marked reported once the push is handled, so a
    ///   transient send failure retries next tick instead of vanishing.
    pub(super) async fn maybe_report_no_progress(
        &self,
        inflight: &mut HashMap<String, InFlight>,
        task: &TaskRow,
        now: DateTime<Utc>,
    ) {
        let threshold = self.config.progress_report_minutes;
        if threshold <= 0 {
            return; // disabled — no query, no allocation
        }
        let Some(entry) = inflight.get(&task.id) else {
            return; // not tracked by this driver (e.g. dispatched pre-restart)
        };
        let round = entry.iter;
        if entry.progress_reported_round == Some(round) {
            return; // already reported for this round
        }
        let enqueued_at = entry.enqueued_at;

        let last_signal = match self.store.latest_activity_at(&task.id).await {
            Ok(Some(ts)) => DateTime::parse_from_rfc3339(&ts)
                .ok()
                .map(|d| d.with_timezone(&Utc))
                // An activity row older than this round's dispatch is not a
                // signal *for this round* — floor at the dispatch instant so a
                // re-dispatched task never inherits the previous round's silence.
                .filter(|d| *d > enqueued_at)
                .unwrap_or(enqueued_at),
            Ok(None) => enqueued_at,
            Err(e) => {
                debug!(task = %task.id, error = %e, "goal loop: progress-signal lookup failed (skipping notice)");
                return;
            }
        };
        let Some(minutes) = no_progress_minutes(last_signal, now, threshold) else {
            return;
        };

        let delivered = self
            .push_progress(
                task,
                &format!("stalled:{round}"),
                crate::goal_notify::GoalProgress::NoProgressReport { minutes },
            )
            .await;
        if !delivered {
            return; // transient send failure — push_progress retries next tick
        }
        if let Some(e) = inflight.get_mut(&task.id) {
            e.progress_reported_round = Some(round);
        }
        // Posted last, and only once: this row itself becomes the newest
        // activity signal, so posting it before the dedup flag was set would
        // reset the very clock that decides whether to post again.
        self.post_activity(
            "goal_loop.progress_report",
            &task.assigned_to,
            Some(&task.id),
            &format!("goal-loop 已執行 {minutes} 分鐘未回報進度 — {}", task.title),
        )
        .await;
        info!(task = %task.id, round, minutes, "goal loop: no-progress notice pushed");
    }

    /// Drop any in-flight progress-push retry counters for `task_id` — called
    /// when a task leaves the driver's dispatch concern entirely (terminal
    /// cleanup), so a stale counter never lingers keyed to a task that no
    /// longer exists in any live state.
    pub(super) async fn clear_progress_retries(&self, task_id: &str) {
        let prefix = format!("{task_id}::");
        let mut retries = self.progress_retry.lock().await;
        retries.retain(|k, _| !k.starts_with(&prefix));
    }

    /// A1/A2: capture one completed round's state, called exactly once per
    /// review sitting (see the `"review"` reconcile branch's
    /// `state_capture_seen` gate).
    ///
    /// Two things happen here, both best-effort (a failure logs and the
    /// driver moves on — this is observability/quality-of-signal, never
    /// control flow):
    ///
    /// 1. **A2 visit-graph recording**: hash the state that was ACTUALLY
    ///    dispatched for this round (recomputed here from `task_iterations`
    ///    + the snapshot as they stood BEFORE this round's self-report is
    ///    persisted below — i.e. byte-identical to what was hashed at
    ///    dispatch-commit time, since neither input has changed yet) paired
    ///    with an [`crate::goal_visit_graph::action_digest`] of what the
    ///    agent actually did this round.
    /// 2. **A1 self-report persistence**: parse `<state_update>` out of
    ///    `result_summary` and, on success, persist it as the new
    ///    `goal_state_json` snapshot for the NEXT round's `<state>` block.
    ///
    /// ## The race this method accepts
    ///
    /// `DispatchEngine::review_goal_tasks` (out of scope for this change)
    /// runs independently and, on rejection, wipes `result_summary` back to
    /// `NULL`. If that judge pass completes before THIS driver's tick
    /// observes the task sitting in `review`, this capture never runs for
    /// that round — silently, by design: the next round's `<state>` block
    /// simply falls back to whatever `goal_state_json` already held
    /// (StateAct's "parse failure / miss ⇒ keep the previous round's value,
    /// never fabricate" rule, applied to the whole capture, not just JSON
    /// parsing). No error is raised because losing one round's self-report
    /// to a race is an accepted degradation, not a fault.
    pub(super) async fn capture_round_state(&self, task: &TaskRow) {
        let iterations = match self.store.list_iterations(&task.id).await {
            Ok(v) => v,
            Err(e) => {
                debug!(task = %task.id, error = %e, "goal loop: state capture list_iterations failed (non-fatal)");
                Vec::new()
            }
        };
        let snapshot = GoalStateSnapshot::from_json(task.goal_state_json.as_deref());
        let state_block = goal_state::build_state_block(task, &iterations, &snapshot);
        let state_hash = goal_state::state_hash(&state_block);

        if self.has_real_home() {
            let agent_id = task
                .claimed_by
                .clone()
                .unwrap_or_else(|| task.assigned_to.clone());
            let since = task
                .claimed_at
                .clone()
                .unwrap_or_else(|| task.created_at.clone());
            let until = Utc::now().to_rfc3339();
            let result_text = task.result_summary.clone().unwrap_or_default();
            let digest = crate::goal_visit_graph::action_digest(
                &self.home_dir,
                &agent_id,
                &since,
                &until,
                &result_text,
            );
            self.visit_graph
                .record_round(&task.id, &state_hash, &digest)
                .await;

            // ── H10: tool-call streak advisory (deepseek-harness §2.16
            //    repeat-tool-reminder) — same `(agent_id, since, until)`
            //    window as the A2 action digest above, read via the shared
            //    `tool_activity` evidence reader. Config-gated (default on,
            //    advisory-only) — see `GoalLoopConfig::tool_streak_advisory`.
            if self.config.tool_streak_advisory {
                if let Some(hit) = crate::goal_tool_streak::detect_tool_streak(
                    &self.home_dir,
                    &agent_id,
                    &since,
                    &until,
                ) {
                    self.record_tool_streak_hint(task, &hit).await;
                }
            }
        }

        if let Some(result_text) = task.result_summary.as_deref() {
            match goal_state::parse_state_update(result_text) {
                Some(hyps) => {
                    // M7: `confirmed_facts` is written independently by
                    // `dispatch_engine.rs`'s settle path (see
                    // `goal_loop/state.rs`'s "Honesty note"). The previous
                    // read-then-`set_goal_state_json`-the-whole-blob pattern
                    // here read `snapshot.confirmed_facts` at the TOP of
                    // this method, then wrote a brand-new blob combining
                    // that (possibly by-now-stale) value with the fresh
                    // hypotheses — a concurrent `confirmed_facts` write
                    // landing in between would be silently clobbered.
                    // `merge_goal_state_json` holds the store's connection
                    // lock across its own read-mutate-write, so it only
                    // ever touches the `pending_hypotheses` key here and
                    // leaves whatever `confirmed_facts` is ACTUALLY stored
                    // at write time untouched, whichever writer got there
                    // first.
                    let hyps_value = serde_json::json!(hyps);
                    if let Err(e) = self
                        .store
                        .merge_goal_state_json(&task.id, move |v| {
                            v["pending_hypotheses"] = hyps_value;
                        })
                        .await
                    {
                        debug!(task = %task.id, error = %e, "goal loop: state_update persist failed (non-fatal, next round keeps prior snapshot)");
                    }
                }
                // No marker / invalid JSON / wrong shape ⇒ degrade: leave
                // `goal_state_json` untouched, the next round's snapshot
                // read carries forward whatever was already stored.
                None => {}
            }
        }

        // ── H5 (WP-B): bail-pattern panel ──────────────────────
        // Runs against the SAME `result_summary` read above, before the
        // race window `DispatchEngine::review_goal_tasks` can clear it (see
        // "The race this method accepts" above) — a miss here degrades
        // exactly like the state_update capture does: silently, by design,
        // no error.
        if let Some(result_text) = task.result_summary.as_deref() {
            if let Some(pattern) = crate::goal_bail_detect::detect_bail_pattern(result_text) {
                self.record_bail_pattern(task, pattern).await;
            }
        }
    }

    /// H5 (WP-B): record one bail-pattern hit — per-pattern telemetry +
    /// activity feed event + a best-effort hint carried into the NEXT
    /// dispatch round's `<state>` block (`GoalStateSnapshot.bail_hint`,
    /// surfaced to the AGENT via `StateBlock::render`).
    ///
    /// Note on scope: the source design (H5) also calls for folding this
    /// signal into "the judge's input". The MAV judge / evaluator prompt is
    /// built entirely in `dispatch_engine.rs`, which is out of scope for
    /// this change (a concurrent work package owns it) — so this only
    /// reaches the goal-loop-owned dispatch prompt for now, not the judge
    /// prompt itself. Best-effort; a store failure here never blocks the
    /// driver.
    async fn record_bail_pattern(&self, task: &TaskRow, pattern: &'static str) {
        crate::metrics::global_metrics()
            .goal_loop_bail_pattern_hit(pattern)
            .await;
        self.post_activity(
            "goal_loop.premature_stop_suspected",
            &task.assigned_to,
            Some(&task.id),
            &format!("偵測到疑似提前收工訊號(pattern={pattern}) — {}", task.title),
        )
        .await;
        let hint = format!(
            "上一輪疑似提前收工(pattern={pattern}),請確認任務是否真的完成,或誠實回報實際受阻原因,勿在未完成時提前結束。"
        );
        if let Err(e) = self
            .store
            .merge_goal_state_json(&task.id, move |v| {
                v["bail_hint"] = serde_json::json!(hint);
            })
            .await
        {
            debug!(task = %task.id, error = %e, "goal loop: bail hint persist failed (non-fatal)");
        }
    }

    /// H10: record one round's tool-call streak — mirrors
    /// `record_bail_pattern`'s shape (activity feed for dashboard
    /// observability + a best-effort hint carried into the NEXT dispatch
    /// round's `<state>` block, `GoalStateSnapshot.tool_streak_hint`,
    /// surfaced via `StateBlock::render`). Advisory only: never blocks,
    /// never retries, never changes what gets dispatched — a no-op below
    /// the lowest threshold (`goal_tool_streak::advisory_text` returns
    /// `None`), so a `StreakHit` of 1 or 2 produces zero activity/hint.
    async fn record_tool_streak_hint(
        &self,
        task: &TaskRow,
        hit: &crate::goal_tool_streak::StreakHit,
    ) {
        let Some(text) = crate::goal_tool_streak::advisory_text(hit) else {
            return;
        };
        self.post_activity(
            "goal_loop.tool_call_streak",
            &task.assigned_to,
            Some(&task.id),
            &format!(
                "偵測到連續 {} 次呼叫同一工具「{}」且參數相同 — {}",
                hit.len, hit.tool_name, task.title
            ),
        )
        .await;
        if let Err(e) = self
            .store
            .merge_goal_state_json(&task.id, move |v| {
                v["tool_streak_hint"] = serde_json::json!(text);
            })
            .await
        {
            debug!(task = %task.id, error = %e, "goal loop: tool streak hint persist failed (non-fatal)");
        }
    }

    /// Best-effort append to the dashboard Activity Feed. A failure here must not
    /// break the loop — it is progress telemetry, not control flow.
    pub(super) async fn post_activity(
        &self,
        event_type: &str,
        agent_id: &str,
        task_id: Option<&str>,
        summary: &str,
    ) {
        let row = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            task_id: task_id.map(str::to_string),
            summary: summary.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            metadata: None,
        };
        if let Err(e) = self.store.append_activity(&row).await {
            debug!(error = %e, "goal loop: activity append failed (non-fatal)");
        }
    }
}
