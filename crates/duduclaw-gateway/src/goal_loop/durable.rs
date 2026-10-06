//! P2-A C5: the durable dispatch path and the per-tick responsibility hooks.
//!
//! Only rounds of a responsibility occurrence, rounds carrying steering, and
//! a round left `intended` by a crash take this path (D9). Every other goal
//! task keeps its random message id, its payload and its timing unchanged.
//!
//! The handoff is the workflow-outbox shape: (1) one transaction opens an
//! `intended` intent `goal:<task>:<iter>` and marks steering `delivering`;
//! (2) the message is enqueued under that fixed id unless already present;
//! (3) one transaction marks the intent `enqueued` and the steering `applied`.
//! A crash between any two steps is repaired on start by
//! [`GoalLoopDriver::reconcile_durable_dispatch`]; the dispatcher re-checks
//! every `goal:` message before spawning anything.

use super::*;
use crate::responsibility::{CostSource, TelemetryCostSource, WakeContext};
use crate::task_store::{DurableInfo, IntentBegin};

/// What the durable path did for one round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DurableEnqueue {
    /// The round's message is in the queue under `message_id`.
    Enqueued { message_id: String, iter: u32 },
    /// The previous durable message is still waiting for pickup — nothing new
    /// was sent (a stall never duplicates a fixed-id round).
    AwaitingPickup { message_id: String, iter: u32 },
    /// The task can no longer be dispatched (stopped, paused, moved on).
    Refused(&'static str),
}

/// Single-occurrence cost verdict before a round is dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OccurrenceCost {
    Within,
    Exceeded {
        spent: i64,
        cap: i64,
    },
    /// Cost could not be read: do not dispatch this tick (fail closed).
    Unavailable,
    /// H-2(c): the employee's own budget cap (`agent.toml [budget]`) is
    /// spent — the outermost limit, also for an occurrence round.
    AgentBudget(String),
}

impl GoalLoopDriver {
    /// Inject a cost source (tests). Production reads `cost_telemetry.db`.
    pub fn with_cost_source(mut self, cost: Arc<dyn CostSource>) -> Self {
        self.cost_source = Some(cost);
        self
    }

    /// Inject the C8 scoring gate / delivery (tests). Production scores with
    /// a `ProactiveGate` of its own and delivers through `goal_notify`.
    pub fn with_notice_channel(
        mut self,
        scorer: Arc<dyn crate::responsibility::notify::NoticeScorer>,
        sender: Arc<dyn crate::responsibility::notify::NoticeSender>,
    ) -> Self {
        self.notice_scorer = scorer;
        self.notice_sender = sender;
        self
    }

    fn cost_source(&self) -> Arc<dyn CostSource> {
        self.cost_source
            .clone()
            .unwrap_or_else(|| Arc::new(TelemetryCostSource::new(&self.home_dir)))
    }

    /// Number of tracked in-flight goal tasks (test accessor).
    #[cfg(test)]
    pub(crate) async fn inflight_len(&self) -> usize {
        self.inflight.lock().await.len()
    }

    /// Tick hook between the needs_human reconciliation and the candidate
    /// scan: discard steering of finished tasks, reconcile pending stops,
    /// then the responsibility wake pass (config-gated). All SQL; zero
    /// model calls. Failures are logged and never fail the tick.
    pub(super) async fn p2a_pre_candidates(&self, now: DateTime<Utc>) {
        // S9: a second gateway on the same home leaves this to the lock holder.
        if !self.holds_instance_lock() {
            debug!("P2-A housekeeping skipped: another gateway holds this home");
            return;
        }
        if self
            .durable_repair_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.repair_durable_handoffs().await;
        }
        if let Err(e) = self.store.discard_steering_of_finished_tasks(now).await {
            debug!(error = %e, "steering sweep failed (retry next tick)");
        }
        let notifier = crate::responsibility::notify::Notifier {
            home: &self.home_dir,
            store: &self.store,
            scorer: self.notice_scorer.as_ref(),
            sender: self.notice_sender.as_ref(),
        };
        crate::responsibility::stop::reconcile_pending(
            &self.store,
            &self.queue,
            self.broker.as_deref(),
            Some(&notifier),
            now,
        )
        .await;
        let enabled =
            crate::responsibility::ResponsibilityConfig::from_home(&self.home_dir).enabled;
        let was = self
            .resp_feature_seen
            .lock()
            .map(|mut g| g.replace(enabled))
            .unwrap_or(None);
        if !enabled && was != Some(false) {
            if let Err(e) = self.store.mark_event_cursor_paused(now).await {
                warn!(error = %e, "could not mark the event cursor paused");
            }
        }
        let free_slots = self
            .config
            .max_concurrent
            .saturating_sub(self.inflight.lock().await.len());
        let cost = self.cost_source();
        let ctx = WakeContext {
            home: &self.home_dir,
            store: &self.store,
            broker: self.broker.as_deref(),
            cost: cost.as_ref(),
            free_slots,
            notifier: Some(&notifier),
        };
        match crate::responsibility::wake_pass(&ctx, now).await {
            Ok(r) if !r.created.is_empty() => {
                info!(
                    created = r.created.len(),
                    "responsibility wake pass created occurrences"
                )
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "responsibility wake pass failed (retry next tick)"),
        }
    }

    /// Durable facts for one candidate. `Err` ⇒ the caller skips the task
    /// this tick (fail closed — never dispatched on unknown state).
    pub(super) async fn durable_check(
        &self,
        task: &TaskRow,
    ) -> Result<Option<DurableInfo>, String> {
        let info = self.store.durable_info(&task.id).await?;
        if info.is_durable() {
            return Ok(Some(info));
        }
        // A fixed-id round from an earlier steering delivery that is still
        // waiting in the queue keeps the task on the durable path until it
        // is picked up, so a stall never sends a second copy.
        if let Some(prev) = &info.last_enqueued {
            if let Some(msg) = self.queue.get_by_id(&prev.intent_id).await? {
                if msg.status == MessageStatus::Pending {
                    return Ok(Some(info));
                }
            }
        }
        Ok(None)
    }

    /// An occurrence of a `paused` responsibility does not get its next round
    /// (the deadline keeps running — pausing never extends it).
    pub(super) fn durable_frozen(info: &DurableInfo) -> bool {
        info.occurrence
            .as_ref()
            .is_some_and(|(_, r)| r.state == "paused")
    }

    /// The persistent iteration count: never below what the intent ledger
    /// recorded, so a restart or hot respawn cannot reset the cap. A leftover
    /// `intended` intent is not counted (it was never sent).
    pub(super) fn durable_iter(in_memory: u32, info: &DurableInfo) -> u32 {
        let persisted = info.persisted_iters - info.intended.is_some() as i64;
        in_memory.max(u32::try_from(persisted.max(0)).unwrap_or(u32::MAX))
    }

    /// Single-occurrence cost check before a round: measured spend of this
    /// task's tree against the per-occurrence cap. Once a round has really
    /// run and nothing was measured, the spend is unknown and counts as the
    /// full reservation (E-M1, the same rule settlement uses). "Really ran"
    /// (H-1) means a sent round whose queue message the dispatcher picked up
    /// and which was not returned as unrun: a round fenced, failed before
    /// any agent ran it, or still waiting in the queue costs nothing.
    pub(super) async fn occurrence_cost(
        &self,
        task: &TaskRow,
        info: &DurableInfo,
    ) -> OccurrenceCost {
        let Some((occ, resp)) = &info.occurrence else {
            return OccurrenceCost::Within;
        };
        // H-2(c): the Claude CLI dispatch path does not consult the employee's
        // budget breaker (the multi-runtime path does); an occurrence round
        // checks it here, so every runtime meets the same outermost limit.
        let agent_dir = self.home_dir.join("agents").join(&task.assigned_to);
        let budget = crate::budget::check_agent_budget(
            &self.home_dir,
            Some(agent_dir.as_path()),
            &task.assigned_to,
        )
        .await;
        if budget.is_denied() {
            return OccurrenceCost::AgentBudget(budget.user_message());
        }
        let cost = self.cost_source();
        match crate::responsibility::cost::tree_spent(
            &self.store,
            cost.as_ref(),
            std::slice::from_ref(&task.id),
        )
        .await
        {
            Ok(m) => {
                let spent = match m.get(&task.id) {
                    Some(c) => crate::responsibility::cost::running_spend(*c, occ.reserved_cents),
                    None => match self.rounds_really_ran(&task.id).await {
                        Ok(true) => occ.reserved_cents,
                        Ok(false) => 0,
                        Err(e) => {
                            warn!(task = %task.id, error = %e, "round history unreadable — round deferred");
                            return OccurrenceCost::Unavailable;
                        }
                    },
                };
                if spent >= resp.occurrence_cost_cap_cents {
                    OccurrenceCost::Exceeded {
                        spent,
                        cap: resp.occurrence_cost_cap_cents,
                    }
                } else {
                    OccurrenceCost::Within
                }
            }
            Err(e) => {
                warn!(task = %task.id, error = %e, "occurrence cost unreadable — round deferred");
                OccurrenceCost::Unavailable
            }
        }
    }

    /// H-1 / M3-1: whether any round of this task was handed to a runtime,
    /// by the durable mark the dispatcher writes before starting one
    /// (`TaskStore::mark_round_started`). The queue message's later state
    /// (failed before a claim, reset to pending after a crash) does not
    /// change the answer.
    pub(super) async fn rounds_really_ran(&self, task_id: &str) -> Result<bool, String> {
        self.store.any_round_started(task_id).await
    }

    /// Steps 1–3 of the durable handoff for one round.
    pub(super) async fn durable_enqueue(
        &self,
        task: &TaskRow,
        info: &DurableInfo,
        requested_iter: u32,
        state_text: &str,
        now: DateTime<Utc>,
    ) -> Result<DurableEnqueue, String> {
        if info.intended.is_none() {
            if let Some(prev) = &info.last_enqueued {
                if let Some(msg) = self.queue.get_by_id(&prev.intent_id).await? {
                    if msg.status == MessageStatus::Pending {
                        return Ok(DurableEnqueue::AwaitingPickup {
                            message_id: prev.intent_id.clone(),
                            iter: u32::try_from(prev.iter).unwrap_or(u32::MAX),
                        });
                    }
                }
            }
        }
        let (intent, steering) = match self
            .store
            .begin_dispatch_intent(&task.id, i64::from(requested_iter), now)
            .await?
        {
            IntentBegin::Ready { intent, steering } => (intent, steering),
            IntentBegin::Refused(reason) => return Ok(DurableEnqueue::Refused(reason)),
        };
        crate::responsibility::test_hooks::pause_point("intent_committed", &task.id).await;
        let iter = u32::try_from(intent.iter).unwrap_or(u32::MAX);
        if self.queue.get_by_id(&intent.intent_id).await?.is_none() {
            crate::responsibility::test_hooks::pause_point("before_enqueue", &task.id).await;
            let mut payload = enqueue::build_goal_payload(task, iter, state_text);
            payload.push_str(&crate::responsibility::steering::render_block(&steering));
            let msg = enqueue::goal_queue_message(intent.intent_id.clone(), task, payload);
            self.queue.enqueue(&msg).await?;
            enqueue::clear_consumed_plan(&self.store, task).await;
        }
        self.store
            .complete_dispatch_intent(&intent.intent_id, task.revision_round + 1, now)
            .await?;
        Ok(DurableEnqueue::Enqueued {
            message_id: intent.intent_id,
            iter,
        })
    }

    /// Restart / hot-respawn repair of the durable handoff (run once before
    /// the first tick):
    /// - `intended` whose message is already queued ⇒ mark it sent
    ///   (`enqueued`, steering `applied`) — never sent twice;
    /// - `intended` with no message, task still dispatchable ⇒ left as is: the
    ///   next tick resends it under the same id with a payload built from the
    ///   current task state;
    /// - `intended` with no message, task no longer dispatchable ⇒
    ///   `abandoned` (steering back to `pending`, or discarded if finished);
    /// - the latest `enqueued` intent of a task still waiting for pickup ⇒
    ///   tracked again as in flight, so it is not dispatched a second time.
    ///
    /// Returns how many intents were repaired or re-tracked.
    /// [`Self::reconcile_durable_dispatch`] with its outcome logged.
    pub(super) async fn repair_durable_handoffs(&self) {
        match self.reconcile_durable_dispatch().await {
            Ok(0) => {}
            Ok(n) => info!(
                repaired = n,
                "goal loop: durable dispatch intents reconciled"
            ),
            Err(e) => warn!(error = %e, "goal loop: durable dispatch reconcile failed"),
        }
    }

    pub async fn reconcile_durable_dispatch(&self) -> Result<usize, String> {
        let now = Utc::now();
        let mut repaired = 0usize;
        for intent in self.store.dispatch_intents_in_state("intended").await? {
            let queued = self.queue.get_by_id(&intent.intent_id).await?;
            let task = self.store.get_task(&intent.task_id).await?;
            let round = task.as_ref().map_or(1, |t| t.revision_round + 1);
            if queued.is_some() {
                self.store
                    .complete_dispatch_intent(&intent.intent_id, round, now)
                    .await?;
                repaired += 1;
                continue;
            }
            let dispatchable = task
                .as_ref()
                .is_some_and(|t| matches!(t.status.as_str(), "todo" | "pending" | "revising"))
                && !self.store.in_stop_tree(&intent.task_id).await?;
            if !dispatchable {
                self.store
                    .abandon_dispatch_intent(&intent.intent_id, now)
                    .await?;
                repaired += 1;
            }
        }
        let mut inflight = self.inflight.lock().await;
        for intent in self.store.dispatch_intents_in_state("enqueued").await? {
            let Some(task) = self.store.get_task(&intent.task_id).await? else {
                continue;
            };
            if !matches!(task.status.as_str(), "todo" | "pending" | "revising") {
                continue;
            }
            let Some(msg) = self.queue.get_by_id(&intent.intent_id).await? else {
                continue;
            };
            if !matches!(
                msg.status,
                MessageStatus::Pending | MessageStatus::Acked | MessageStatus::Failed
            ) {
                continue;
            }
            let iter = u32::try_from(intent.iter).unwrap_or(u32::MAX);
            let keep = inflight.get(&intent.task_id).is_none_or(|e| e.iter < iter);
            if keep {
                let enqueued_at = crate::task_store::parse_ts(&intent.updated_at).unwrap_or(now);
                inflight.insert(
                    intent.task_id.clone(),
                    InFlight {
                        iter,
                        enqueued_at,
                        awaiting_pickup: true,
                        lease: None,
                        message_id: Some(intent.intent_id.clone()),
                        progress_reported_round: None,
                    },
                );
                repaired += 1;
            }
        }
        Ok(repaired)
    }
}
