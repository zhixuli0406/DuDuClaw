//! `needs_human` escalation and the reconciliation of a human decision.
//! Moved verbatim out of `goal_loop.rs`.

use super::*;

impl GoalLoopDriver {
    /// Park a task for a human and drop its in-flight tracking.
    ///
    /// H11: `pause` is the closed classification of this escalation, supplied
    /// by the call site (the trigger is known statically there; `reason` is
    /// free text that in some paths embeds a task id or LLM prose and must
    /// never be re-parsed for a routing decision).
    ///
    /// WP-4F: a `BudgetExhausted` escalation (iteration cap / wall clock /
    /// per-task deadline — the only pause classes this driver's own caps
    /// ever raise) attaches the closest-to-done round's excerpt + gap list
    /// instead of leaving the human with a bare "we ran out of budget"
    /// reason (see `goal_loop/state.rs`). Every other pause class
    /// (`NoProgress`, `BlockedNeedsDecision`) passes `reason` through
    /// byte-identical to before this feature existed. Best-effort: a failed
    /// iteration-history read degrades to the bare `reason`, never blocks
    /// the escalation itself.
    pub(super) async fn escalate(
        &self,
        inflight: &mut HashMap<String, InFlight>,
        task: &TaskRow,
        reason: &str,
        pause: crate::pause_reason::PauseReason,
    ) -> Result<(), String> {
        let effective_reason = if pause == crate::pause_reason::PauseReason::BudgetExhausted {
            match self.store.list_iterations(&task.id).await {
                Ok(iterations) => {
                    match crate::goal_budget_best_round::pick_best_round(&iterations) {
                        Some(pick) => {
                            crate::goal_budget_best_round::compose_escalation_note(reason, &pick)
                        }
                        // 0 rounds ever judged (e.g. a wall-clock deadline
                        // hit before the first dispatch) — keep the bare
                        // pre-WP-4F reason, never fabricate a pick.
                        None => reason.to_string(),
                    }
                }
                Err(e) => {
                    warn!(
                        task = %task.id, error = %e,
                        "goal loop: iteration history read failed for WP-4F best-round pick (non-fatal, using bare reason)"
                    );
                    reason.to_string()
                }
            }
        } else {
            reason.to_string()
        };
        if task.status == "review" {
            self.store
                .mark_needs_human_sealing_round(&task.id, &effective_reason, pause, None)
                .await?;
        } else {
            self.store
                .mark_needs_human_with_pause(&task.id, &effective_reason, pause)
                .await?;
        }
        crate::dashboard_feedback::emit_live(&self.home_dir, "task.needs_human",
            serde_json::json!({ "task_id": task.id, "agent_id": task.assigned_to,
                "pause_reason": pause.as_str() }));
        // A1 ledger: keep the pause class on the latest round row, where it
        // survives `resolve_needs_human` clearing `tasks.pause_reason`. The
        // verdict is not touched. Bookkeeping only — failure is logged.
        if let Err(e) = self
            .store
            .stamp_iteration_pause(&task.id, pause.as_str())
            .await
        {
            warn!(task = %task.id, error = %e, "A1 ledger: iteration pause stamp failed (non-fatal)");
        }
        // WP3 (PORTICO): escalation ends the autonomous phase (iteration cap /
        // oscillation) → revoke the task's grants. Mirrors the DispatchEngine
        // needs_human revocation for the goal-loop-side escalation path.
        self.revoke_task_grants(&task.id, crate::capability_grants::REVOKE_REASON_PHASE_END)
            .await;
        if let Some(removed) = inflight.remove(&task.id) {
            self.release_lease(&removed); // RFC-27: free the slot on escalation
        }
        // A2 lifecycle: this driver's own escalation is a terminal phase end
        // — drop visit-graph tracking (the tracked-loop's terminal branches
        // also clear it, for escalations DispatchEngine triggers directly;
        // this call covers the driver-triggered path with no gap in
        // between).
        self.visit_graph.clear_task(&task.id).await;
        // L3: same `state_capture_seen` leak fix as the reconcile loop's
        // terminal branches — this driver's own escalation (iteration cap /
        // deadline / A2 oscillation) is ALSO a terminal phase end that never
        // re-enters the candidate set, so the top-of-tick prune alone would
        // never clear it.
        self.state_capture_seen.lock().await.remove(&task.id);
        self.post_activity(
            "goal_loop.needs_human",
            &task.assigned_to,
            Some(&task.id),
            &format!("goal-loop 轉人工:{reason} — {}", task.title),
        )
        .await;
        warn!(task = %task.id, %reason, "goal loop: escalated to needs_human");
        Ok(())
    }

    /// Push a channel approval for every goal task newly parked `needs_human`.
    /// Catches BOTH escalation paths (this driver's caps AND the DispatchEngine
    /// judge rejection at retry budget) with one detector. For `Observer`
    /// agents the loop does not wait: the task is auto-closed (`cancelled`) and
    /// the human is notified after the fact. Best-effort — never fails the tick.
    pub(super) async fn reconcile_needs_human(&self) {
        let tasks = match self.store.tasks_in_status("needs_human").await {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "goal loop: needs_human scan failed (will retry)");
                return;
            }
        };
        let live: HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
        let mut notified = self.notified_needs_human.lock().await;
        // RM-192 B6: a task seen leaving needs_human ends its episode, so a
        // later re-entry notifies again (also across a restart).
        let left: Vec<String> = notified.iter().filter(|id| !live.contains(*id)).cloned().collect();
        notified.retain(|id| live.contains(id));
        self.needs_human_retry
            .lock()
            .await
            .retain(|id, _| live.contains(id));
        for id in &left {
            self.close_needs_human_episode(id).await;
        }
        // Tasks that left needs_human while no driver was watching (a
        // restart, a decision taken before this life's first pass).
        if !self.needs_human_swept.swap(true, std::sync::atomic::Ordering::SeqCst) {
            match self
                .store
                .tasks_with_open_markers([NOTIFIED_MARKER, MIRRORED_MARKER], CLEARED_MARKER)
                .await
            {
                Ok(ids) => {
                    for id in ids.iter().filter(|id| !live.contains(*id)) {
                        self.close_needs_human_episode(id).await;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "goal loop: needs_human marker sweep failed (will retry)");
                    self.needs_human_swept
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }

        for task in &tasks {
            if !task.goal_mode || notified.contains(&task.id) {
                continue;
            }
            // RM-192 B6: pushed for this same episode by an earlier driver
            // life → do not push again. A read error falls back to the
            // in-memory behaviour (notify once this life).
            let episode = match self.needs_human_episode(&task.id).await {
                Ok(episode) => Some(episode),
                Err(e) => {
                    warn!(task = %task.id, error = %e, "goal loop: needs_human marker read failed");
                    None
                }
            };
            if episode.is_some_and(|e| e.notified) {
                notified.insert(task.id.clone());
                self.mark_needs_human_mirror_seen(&task.id).await;
                continue;
            }
            crate::dashboard_feedback::emit_live(&self.home_dir, "task.needs_human",
                serde_json::json!({ "task_id": task.id, "agent_id": task.assigned_to,
                    "pause_reason": task.pause_reason }));
            let level = AutonomyLevel::for_agent(&self.home_dir, &task.assigned_to);
            if level == AutonomyLevel::Observer
                // Persisted feedback outlives refusal tokens; check the durable assignee now.
                && crate::entry_gate::check_task_assignee(&self.home_dir, &task.assigned_to).is_ok()
                && crate::pause_reason::PauseReason::from_stored(task.pause_reason.as_deref())
                    .observer_may_autoclose()
            {
                // Observer: notify-only, no waiting — resolve straight to cancelled.
                match self
                    .store
                    .resolve_needs_human(
                        &task.id,
                        "abort",
                        "Observer 全自動模式:需人工需求自動結束",
                    )
                    .await
                {
                    Ok(_) => {
                        crate::goal_notify::notify_goal_observer(
                            &self.home_dir,
                            task,
                            "已自動結束 (cancelled)",
                        )
                        .await;
                        self.post_activity(
                            "goal_loop.observer_autoclose",
                            &task.assigned_to,
                            Some(&task.id),
                            &format!("Observer 模式:needs_human 自動結束 — {}", task.title),
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!(task = %task.id, error = %e, "goal loop: observer auto-close failed")
                    }
                }
                // Observer resolves the task out of needs_human synchronously
                // above (or logs+leaves it for a later retry on store error);
                // either way there is no channel-push retry state to track.
                notified.insert(task.id.clone());
            } else {
                // Operator/Collaborator/Consultant/Approver: push the
                // retry/done/abort/take-over card to the goal's destination
                // (`goal_notify::goal_target`: its source conversation, else
                // the agent's `[proactive]` channel), with buttons where that
                // channel can show them. The progress mirror then goes out
                // only when it would land in a different conversation
                // (`push_needs_human_mirror`). A transient
                // SendFailed is retried (bounded) on a later tick instead of
                // being marked `notified` immediately — previously ANY
                // outcome (including a network blip) inserted into `notified`
                // unconditionally, so a failed push was never retried and the
                // human never learned the task was stuck.
                use crate::goal_notify::NotifyOutcome;
                let notice =
                    crate::goal_notify::notify_goal_needs_human_detailed(&self.home_dir, task)
                        .await;
                let outcome = notice.outcome;
                match outcome {
                    // `Deferred` cannot occur here in practice — needs_human
                    // is L3 and quiet hours never hold it back — but it is
                    // handled explicitly rather than by a wildcard so that a
                    // future re-classification is a compile-time decision, not
                    // a silently-swallowed notification.
                    NotifyOutcome::Sent | NotifyOutcome::NoTarget | NotifyOutcome::Deferred => {
                        self.needs_human_retry.lock().await.remove(&task.id);
                        let sent = outcome != NotifyOutcome::NoTarget;
                        let summary = format!(
                            "已推播需人工審批 — {}(推播{})",
                            task.title,
                            if sent {
                                "成功"
                            } else {
                                "無通知目標(設定缺漏)"
                            }
                        );
                        // RM-192 B6: a delivered card is recorded for this
                        // episode before anything else, so a restart from
                        // here on never sends it again.
                        let recorded = match (sent, episode) {
                            (true, Some(ep)) => {
                                self.record_needs_human_notified(task, ep.seq, &summary).await
                            }
                            _ => false,
                        };
                        if !recorded {
                            self.post_activity(
                                "goal_loop.needs_human_notified",
                                &task.assigned_to,
                                Some(&task.id),
                                &summary,
                            )
                            .await;
                        }
                        self.push_needs_human_mirror(task, &notice, episode).await;
                        notified.insert(task.id.clone());
                    }
                    NotifyOutcome::SendFailed => {
                        let mut retries = self.needs_human_retry.lock().await;
                        let count = retries.entry(task.id.clone()).or_insert(0);
                        *count += 1;
                        if *count >= NOTIFY_PUSH_MAX_RETRIES {
                            warn!(task = %task.id, attempts = *count,
                                  "goal loop: needs_human push failed after max retries, giving up");
                            retries.remove(&task.id);
                            drop(retries);
                            self.push_needs_human_mirror(task, &notice, episode).await;
                            self.post_activity(
                                "goal_loop.needs_human_notified",
                                &task.assigned_to,
                                Some(&task.id),
                                &format!("需人工審批推播多次失敗，放棄重試 — {}", task.title),
                            )
                            .await;
                            notified.insert(task.id.clone());
                        } else {
                            warn!(task = %task.id, attempt = *count,
                                  "goal loop: needs_human push failed, will retry next tick");
                            // Not inserted into `notified` — reconcile_needs_human
                            // retries this task again next tick.
                        }
                    }
                }
            }
        }
    }
}

// ── RM-192 B6: persistent needs_human episode markers ─────────────────
//
// `notified_needs_human` / `progress_seen` live in memory only, so before
// this a restart re-sent the card (and the mirror) of every task still
// waiting for a human. Episodes are persisted as activity rows with
// deterministic ids (`TaskStore::append_activity_once`):
//
// - `goal_needs_human_notified:<task>:<n>` — episode n's card was delivered
//   (Sent, or queued by quiet hours / a takeover). This is the existing
//   "已推播需人工審批" feed row, now with a stable id.
// - `goal_needs_human_mirrored:<task>:<n>` — episode n's progress mirror
//   was delivered.
// - `goal_needs_human_cleared:<task>:<n>` — the driver saw the task leave
//   needs_human after episode n; the next entry is episode n + 1.
//
// Why not a timestamp of the entry: every entry path stamps
// `tasks.updated_at`, but a take-over press (`claim_needs_human`) and
// ordinary task edits bump it again while the task stays needs_human, and
// there is no entry-only column (the tasks.db schema is fingerprinted by the
// migration importer, so none is added here). The episode number instead
// moves only when the driver observes an exit: every pass for the tasks it
// tracks, plus one sweep per driver life for tasks that left while no
// driver was watching.
//
// Trust boundary (accepted limitation, v1.84 security review finding 3):
// the markers live in tasks.db next to the task itself. They are written only
// by this driver (`append_marker`, deterministic ids); no MCP / RPC path lets
// a caller choose an activity row id — `activity_post` and every other
// writer mint a UUID or use their own fixed prefix (pinned by the cli test
// `activity_post_cannot_choose_the_row_id`) — so an employee cannot forge one
// through the tools. A process with direct write access to tasks.db can
// forge one (e.g. a "notified" marker for an episode ahead of the task's
// real state, which suppresses that task's card). That process can already
// rewrite the task itself, so this is not defended here. What is guaranteed:
// a marker is only ever read for the task it names — its id must be exactly
// `<kind><task id>:<digits>` AND its `task_id` column must be that task
// (`activity_ids_with_prefix`, `max_marker_seq`, `tasks_with_open_markers`)
// — so a forged or malformed row never affects another task.

/// Marker row id prefixes — see the trust boundary above.
const NOTIFIED_MARKER: &str = "goal_needs_human_notified:";
const MIRRORED_MARKER: &str = "goal_needs_human_mirrored:";
const CLEARED_MARKER: &str = "goal_needs_human_cleared:";

/// The persistent state of a task's current needs_human episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NeedsHumanEpisode {
    /// 1-based; one more than the last cleared episode.
    pub(super) seq: u64,
    /// This episode's card was delivered by some driver life.
    pub(super) notified: bool,
    /// This episode's progress mirror was delivered.
    pub(super) mirrored: bool,
}

fn marker_id(kind: &str, task_id: &str, seq: u64) -> String {
    format!("{kind}{task_id}:{seq}")
}

/// The largest episode number among `ids` that are exactly `prefix` + a
/// decimal number; 0 when there is none.
fn max_marker_seq(ids: &[String], prefix: &str) -> u64 {
    ids.iter()
        .filter_map(|id| id.strip_prefix(prefix))
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|n| n.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    #[test]
    fn marker_ids_parse_exactly() {
        let prefix = format!("{NOTIFIED_MARKER}t1:");
        assert_eq!(marker_id(NOTIFIED_MARKER, "t1", 3), format!("{prefix}3"));
        let ids: Vec<String> = [
            format!("{prefix}1"),
            format!("{prefix}12"),
            format!("{prefix}0"),
            // Another task whose id starts with "t1:" — never this task's.
            format!("{prefix}9:4"),
            format!("{prefix}+7"),
            prefix.clone(),
            format!("{MIRRORED_MARKER}t1:99"),
        ]
        .into();
        assert_eq!(max_marker_seq(&ids, &prefix), 12);
        assert_eq!(max_marker_seq(&[], &prefix), 0);
    }
}

impl GoalLoopDriver {
    /// Read the current episode of `task_id` from its marker rows.
    pub(super) async fn needs_human_episode(
        &self,
        task_id: &str,
    ) -> Result<NeedsHumanEpisode, String> {
        let mut seqs = [0u64; 3];
        for (slot, kind) in [NOTIFIED_MARKER, MIRRORED_MARKER, CLEARED_MARKER].iter().enumerate() {
            let prefix = format!("{kind}{task_id}:");
            let ids = self.store.activity_ids_with_prefix(task_id, &prefix).await?;
            seqs[slot] = max_marker_seq(&ids, &prefix);
        }
        let [notified, mirrored, cleared] = seqs;
        let open = notified.max(mirrored);
        let seq = if open > cleared { open } else { cleared.saturating_add(1) };
        Ok(NeedsHumanEpisode {
            seq,
            notified: notified == seq,
            mirrored: mirrored == seq,
        })
    }

    async fn append_marker(
        &self,
        id: String,
        event_type: &str,
        agent_id: &str,
        task_id: &str,
        summary: &str,
    ) -> bool {
        let row = ActivityRow {
            id,
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            task_id: Some(task_id.to_string()),
            summary: summary.to_string(),
            // One fixed format for every marker: the restart sweep compares
            // marker timestamps with each other.
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            metadata: None,
        };
        match self.store.append_activity_once(&row).await {
            Ok(_) => true,
            Err(e) => {
                warn!(task = %task_id, error = %e, "goal loop: needs_human marker write failed");
                false
            }
        }
    }

    /// Record that episode `seq`'s card was delivered, as the feed row
    /// `summary`. `false` when the store refused the write (the caller then
    /// keeps the in-memory behaviour).
    pub(super) async fn record_needs_human_notified(
        &self,
        task: &TaskRow,
        seq: u64,
        summary: &str,
    ) -> bool {
        self.append_marker(
            marker_id(NOTIFIED_MARKER, &task.id, seq),
            "goal_loop.needs_human_notified",
            &task.assigned_to,
            &task.id,
            summary,
        )
        .await
    }

    /// End the current episode of a task seen outside needs_human, if
    /// anything was delivered for it. A deleted task is left alone.
    async fn close_needs_human_episode(&self, task_id: &str) {
        let Ok(episode) = self.needs_human_episode(task_id).await else {
            return;
        };
        if !(episode.notified || episode.mirrored) {
            return;
        }
        let Ok(Some(task)) = self.store.get_task(task_id).await else {
            return;
        };
        if task.status == "needs_human" {
            // Re-entered between the scan and now: it did leave, so the
            // episode still ends here and the next pass notifies again.
            debug!(task = %task_id, "goal loop: needs_human re-entered before its episode closed");
        }
        self.append_marker(
            marker_id(CLEARED_MARKER, task_id, episode.seq),
            "goal_loop.needs_human_cleared",
            &task.assigned_to,
            task_id,
            &format!("需人工決定已結束 — {}（之後若再次卡住會重新通知）", task.title),
        )
        .await;
    }

    async fn mark_needs_human_mirror_seen(&self, task_id: &str) {
        self.progress_seen
            .lock()
            .await
            .insert(task_id.to_string(), "needs_human".to_string());
    }

    /// RM-192 B5: the needs_human progress line after the card push.
    ///
    /// - The card was delivered (or queued) to the very conversation the
    ///   line would go to → no second message there.
    /// - Otherwise the line goes out, and says buttons were pushed elsewhere
    ///   only when a card with buttons really was.
    ///
    /// Delivered once per episode, also across restarts (B6).
    async fn push_needs_human_mirror(
        &self,
        task: &TaskRow,
        notice: &crate::goal_notify::NeedsHumanNotice,
        episode: Option<NeedsHumanEpisode>,
    ) {
        use crate::goal_notify::NotifyOutcome;
        if episode.is_some_and(|e| e.mirrored) {
            self.mark_needs_human_mirror_seen(&task.id).await;
            return;
        }
        let card_delivered = matches!(notice.outcome, NotifyOutcome::Sent | NotifyOutcome::Deferred);
        let mirror_destination = crate::goal_notify::goal_card_target(&self.home_dir, task);
        if card_delivered && notice.destination.is_some() && notice.destination == mirror_destination {
            self.mark_needs_human_mirror_seen(&task.id).await;
            return;
        }
        let buttons_elsewhere = card_delivered && notice.buttons && notice.destination.is_some();
        let (_, outcome) = self
            .push_progress_reporting(
                task,
                "needs_human",
                crate::goal_notify::GoalProgress::NeedsHuman { buttons_elsewhere },
            )
            .await;
        if let (Some(NotifyOutcome::Sent | NotifyOutcome::Deferred), Some(ep)) = (outcome, episode) {
            self.append_marker(
                marker_id(MIRRORED_MARKER, &task.id, ep.seq),
                "goal_loop.needs_human_mirrored",
                &task.assigned_to,
                &task.id,
                &format!("已在來源對話提醒需人工決定 — {}", task.title),
            )
            .await;
        }
    }
}
