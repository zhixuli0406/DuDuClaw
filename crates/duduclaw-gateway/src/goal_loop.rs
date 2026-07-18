//! Autonomous goal loop — the **outer loop driver** (P1).
//!
//! ## Where this sits
//!
//! [`crate::dispatch_engine::DispatchEngine`] is architecturally a *maintenance*
//! loop: zombie reclaim + goal-mode acceptance review. It does **not** drive task
//! execution. This module is the missing half — the driver that:
//!
//! 1. finds `goal_mode` tasks that are waiting to run (`todo` / `pending`,
//!    assigned to a concrete agent), and
//! 2. **re-uses the existing wake-up rail** to make them run: it enqueues a work
//!    message into `message_queue.db` (exactly like the heartbeat's
//!    `poll_assigned_tasks`), which the existing `AgentDispatcher` 5-second poll
//!    routes to the agent through the same code path a channel message uses.
//!
//! The closed loop then is:
//! ```text
//!   driver enqueue ─▶ dispatcher ─▶ agent (tasks_claim → work → tasks_complete)
//!        ▲                                              │
//!        │                                              ▼
//!        │                                     goal_mode → review
//!        │                                              │
//!        │                          DispatchEngine judge acceptance
//!        │                                              │
//!        └──── reject → pending (+judge_feedback) ◀─────┤
//!                                                       │
//!                                                  pass → done
//! ```
//! On rejection the task returns to `pending` with `judge_feedback`; the very
//! next driver tick re-dispatches it, carrying that feedback into the work
//! message (Generator-Verifier retry with feedback). That is the whole loop.
//!
//! ## Termination guards (paper 2607.01641: bound every feedback path)
//!
//! The driver — not the model — owns the hard bounds, so a stuck goal cannot
//! loop forever:
//! - **In-flight de-dup**: a task already dispatched and not yet advanced by the
//!   agent is not re-enqueued until a stall timeout elapses.
//! - **Iteration cap**: total dispatches per task (independent of the judge's
//!   `max_retries`; both apply, whichever is stricter). Exceed ⇒ `needs_human`.
//! - **Wall-clock cap**: measured from `created_at`. Exceed ⇒ `needs_human`.
//! - **Concurrency cap**: bounds simultaneously in-flight goal tasks to avoid a
//!   spawn storm from a batch of goals.
//!
//! Everything is opt-in: the driver only runs when the dispatch engine is
//! enabled (`[dispatch] enabled = true`), and only acts on `goal_mode` tasks —
//! which are themselves opt-in. Constants live in [`GoalLoopConfig`], read from
//! `config.toml [goal_loop]` with serde defaults (absent / partial section ⇒
//! built-in defaults; the section is parsed in isolation so it can never break
//! deserialization of the rest of `config.toml`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time;
use tracing::{debug, info, warn};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
use crate::task_store::{ActivityRow, TaskRow, TaskStore};

/// TTL for a kickoff approval (Collaborator/Consultant autonomy gate). Expiry
/// counts as a denial (ApprovalBroker fail-closed) ⇒ the goal is aborted.
const KICKOFF_TTL_SECS: i64 = 3600;

/// P2a autonomy level — how much the goal loop may drive an agent on its own.
/// Parsed from `agent.toml [capabilities] autonomy_level` (raw-toml additive
/// gate, same convention as `approval_required_tools`). Missing / unparseable /
/// unknown ⇒ [`AutonomyLevel::Approver`] (the conservative default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyLevel {
    /// The loop does not auto-drive this agent's goal tasks at all.
    Operator,
    /// First dispatch is gated behind a human kickoff approval.
    Collaborator,
    /// Same kickoff gate as Collaborator at this stage (diverges in later
    /// phases: per-action approval depth).
    Consultant,
    /// Default: no kickoff gate; relies on the needs_human exit (and, in P2b,
    /// irreversible-action approval).
    Approver,
    /// Fully autonomous; needs_human is notify-only (the loop never waits).
    Observer,
}

impl AutonomyLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            AutonomyLevel::Operator => "operator",
            AutonomyLevel::Collaborator => "collaborator",
            AutonomyLevel::Consultant => "consultant",
            AutonomyLevel::Approver => "approver",
            AutonomyLevel::Observer => "observer",
        }
    }

    /// Parse a raw string. Unknown / empty ⇒ `Approver` (conservative default).
    pub fn from_toml_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "operator" => AutonomyLevel::Operator,
            "collaborator" => AutonomyLevel::Collaborator,
            "consultant" => AutonomyLevel::Consultant,
            "approver" => AutonomyLevel::Approver,
            "observer" => AutonomyLevel::Observer,
            _ => AutonomyLevel::Approver,
        }
    }

    /// Read `agent.toml [capabilities] autonomy_level` for one agent. A missing
    /// file, missing key, or malformed toml ⇒ `Approver` (fail-safe: the
    /// conservative level, never the most-autonomous one).
    pub fn for_agent(home_dir: &Path, agent_id: &str) -> Self {
        let path = home_dir.join("agents").join(agent_id).join("agent.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return AutonomyLevel::Approver;
        };
        let Ok(value) = toml::from_str::<toml::Value>(&text) else {
            return AutonomyLevel::Approver;
        };
        value
            .get("capabilities")
            .and_then(|c| c.get("autonomy_level"))
            .and_then(|v| v.as_str())
            .map(AutonomyLevel::from_toml_str)
            .unwrap_or(AutonomyLevel::Approver)
    }

    /// Levels whose first dispatch is gated behind a human kickoff approval.
    fn requires_kickoff(self) -> bool {
        matches!(self, AutonomyLevel::Collaborator | AutonomyLevel::Consultant)
    }
}

/// Outcome of the kickoff gate for a Collaborator/Consultant goal task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KickoffGate {
    /// Human approved (or no broker to gate with) — dispatch may proceed.
    Proceed,
    /// Approval still pending — do not dispatch this tick.
    Waiting,
    /// Denied / expired — the task was aborted; skip it.
    Aborted,
}

/// Tuning for the goal loop driver. Read from `config.toml [goal_loop]`.
///
/// `#[serde(default)]` at the container level means every field falls back to
/// [`GoalLoopConfig::default`] when absent, so a missing or partial section is
/// always valid.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GoalLoopConfig {
    /// Hard cap on total dispatches per task (independent of the judge's
    /// `max_retries`; both apply, stricter wins). Exceed ⇒ `needs_human`.
    pub iteration_cap: u32,
    /// Wall-clock budget measured from the task's `created_at`, in hours.
    /// Exceed ⇒ `needs_human`.
    pub wall_clock_hours: i64,
    /// Max simultaneously in-flight goal tasks (spawn-storm guard).
    pub max_concurrent: usize,
    /// Driver tick cadence (seconds).
    pub tick_secs: u64,
    /// A dispatched task the agent has not picked up within this many seconds is
    /// considered stalled and may be re-dispatched (counts as an iteration).
    pub stalled_secs: i64,
}

impl Default for GoalLoopConfig {
    fn default() -> Self {
        Self {
            iteration_cap: 8,
            wall_clock_hours: 24,
            max_concurrent: 3,
            tick_secs: 30,
            stalled_secs: 600,
        }
    }
}

impl GoalLoopConfig {
    /// Load `[goal_loop]` from `<home>/config.toml`. The section is parsed in
    /// isolation (from a generic `toml::Table`), so unrelated config sections
    /// can never make this fail — absent / malformed ⇒ defaults.
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("goal_loop") {
            Some(section) => section
                .clone()
                .try_into::<GoalLoopConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }
}

/// Per-task driver bookkeeping (in memory; the durable state is the task row).
#[derive(Debug, Clone)]
struct InFlight {
    /// Total dispatches so far (drives the iteration cap).
    iter: u32,
    /// When the current dispatch was enqueued (drives the stall timeout).
    enqueued_at: DateTime<Utc>,
    /// True while we are waiting for the agent to advance the task out of
    /// `todo` / `pending` (i.e. `tasks_claim`). Flipped false once it moves to
    /// `in_progress` / `review`.
    awaiting_pickup: bool,
    /// Normalized `judge_feedback` carried on the *previous* rejection re-dispatch
    /// (lowercase + trim). Used to detect no-progress oscillation: two
    /// consecutive rejections with identical feedback ⇒ the retry loop is not
    /// converging (possible LoopTrap / stuck agent), so escalate to a human
    /// instead of burning the iteration budget. `None` until the first rejection.
    last_feedback: Option<String>,
}

/// Normalize judge feedback for oscillation comparison: lowercase + trim. Kept
/// deliberately simple (the design calls for lowercase+trim, not fuzzy match).
fn normalize_feedback(fb: &str) -> String {
    fb.trim().to_lowercase()
}

/// The goal loop background driver.
pub struct GoalLoopDriver {
    store: Arc<TaskStore>,
    queue: Arc<MessageQueue>,
    config: GoalLoopConfig,
    /// DuDuClaw home dir — used to read per-agent `autonomy_level` and to push
    /// channel notifications (via `goal_notify`). Defaults to `.` so the 3-arg
    /// [`GoalLoopDriver::new`] stays usable in tests; production wires the real
    /// home dir via [`GoalLoopDriver::with_home_dir`].
    home_dir: PathBuf,
    /// HITL broker for the Collaborator/Consultant kickoff gate. `None` ⇒ no
    /// gate (Collaborator/Consultant fall back to proceeding — fail-safe: a
    /// missing broker never strands a task).
    broker: Option<Arc<ApprovalBroker>>,
    /// Per-task in-flight bookkeeping. Held behind a mutex so `tick_once` can
    /// take `&self`; there is only ever one tick in flight, so contention is nil.
    inflight: Mutex<HashMap<String, InFlight>>,
    /// Task ids whose kickoff approval is outstanding (task_id → approval id).
    kickoff: Mutex<HashMap<String, ApprovalId>>,
    /// needs_human goal tasks already pushed to a channel this process life, so
    /// the reconciler does not re-notify every tick. Pruned to the live
    /// needs_human set each pass.
    notified_needs_human: Mutex<HashSet<String>>,
    /// Operator-level goal tasks already announced as skipped (dedup).
    operator_skipped: Mutex<HashSet<String>>,
    /// P5 outer progress board dedup: task_id → last progress phase key pushed
    /// to the source conversation, so the same phase is not pushed twice. Pruned
    /// when a task reaches a terminal state (entry removed on `done`).
    progress_seen: Mutex<HashMap<String, String>>,
    running: Arc<AtomicBool>,
}

impl GoalLoopDriver {
    pub fn new(store: Arc<TaskStore>, queue: Arc<MessageQueue>, config: GoalLoopConfig) -> Self {
        Self {
            store,
            queue,
            config,
            home_dir: PathBuf::from("."),
            broker: None,
            inflight: Mutex::new(HashMap::new()),
            kickoff: Mutex::new(HashMap::new()),
            notified_needs_human: Mutex::new(HashSet::new()),
            operator_skipped: Mutex::new(HashSet::new()),
            progress_seen: Mutex::new(HashMap::new()),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Set the DuDuClaw home dir (per-agent autonomy + channel push).
    pub fn with_home_dir(mut self, home_dir: PathBuf) -> Self {
        self.home_dir = home_dir;
        self
    }

    /// Wire the HITL broker used for the Collaborator/Consultant kickoff gate.
    pub fn with_broker(mut self, broker: Arc<ApprovalBroker>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// Stop the loop after the current tick.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Run the driver loop. Mirrors the dispatch engine cadence: sleep, then one
    /// tick of goal-task dispatching.
    pub async fn run(self: Arc<Self>) {
        self.running.store(true, Ordering::SeqCst);
        info!(
            iteration_cap = self.config.iteration_cap,
            wall_clock_hours = self.config.wall_clock_hours,
            max_concurrent = self.config.max_concurrent,
            tick_secs = self.config.tick_secs,
            "Goal loop driver started (autonomous goal_mode dispatch)"
        );
        while self.running.load(Ordering::SeqCst) {
            time::sleep(Duration::from_secs(self.config.tick_secs.max(1))).await;
            if let Err(e) = self.tick_once().await {
                warn!(error = %e, "goal loop tick failed (will retry next tick)");
            }
        }
        warn!("Goal loop driver stopped");
    }

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
        // agent. `todo` = freshly created; `pending` = returned from a judge
        // rejection (or a durable claim awaiting pickup). Reuses the existing
        // status query so no new store method is needed.
        let mut candidates: Vec<TaskRow> = Vec::new();
        for status in ["todo", "pending"] {
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

        let mut inflight = self.inflight.lock().await;

        // ── Reconcile: prune finished/escalated entries, and mark picked-up
        //    tasks (moved to in_progress/review) as no longer awaiting pickup so
        //    they still count against concurrency but are not re-dispatched. ──
        let tracked: Vec<String> = inflight.keys().cloned().collect();
        for id in tracked {
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
                }
                // Under acceptance review — push the "驗收中" progress once.
                "review" => {
                    if let Some(e) = inflight.get_mut(&id) {
                        e.awaiting_pickup = false;
                    }
                    if let Some(t) = &task_opt {
                        self.push_progress(t, "review", crate::goal_notify::GoalProgress::Reviewing)
                            .await;
                    }
                }
                // Judge-accepted / human-marked done — push the ✅ result and
                // drop all tracking (terminal).
                "done" => {
                    if let Some(t) = &task_opt {
                        self.push_progress(t, "done", crate::goal_notify::GoalProgress::Done)
                            .await;
                    }
                    inflight.remove(&id);
                    self.progress_seen.lock().await.remove(&id);
                }
                // Other terminal / escalated states (cancelled / failed /
                // needs_human) — no longer the driver's dispatch concern.
                // needs_human progress is pushed by reconcile_needs_human.
                _ => {
                    inflight.remove(&id);
                }
            }
        }

        // In-flight goal tasks currently tracked (drives the concurrency admission gate).
        let mut active = inflight.len();

        for task in &candidates {
            // ── Wall-clock guard (from created_at) ──
            if self.deadline_exceeded(&task.created_at, now) {
                self.escalate(&mut inflight, task, "goal-loop deadline").await?;
                active = inflight.len();
                continue;
            }

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
                        &format!(
                            "Operator 模式:goal loop 不自主驅動此任務 — {}",
                            task.title
                        ),
                    )
                    .await;
                }
                continue;
            }

            let entry = inflight.get(&task.id).cloned();
            let is_new = entry.is_none();

            // Collaborator/Consultant: gate the FIRST dispatch behind a human
            // kickoff approval. Waiting/Aborted ⇒ do not dispatch this tick.
            if is_new && level.requires_kickoff() {
                match self.kickoff_gate(task).await? {
                    KickoffGate::Waiting | KickoffGate::Aborted => continue,
                    KickoffGate::Proceed => {}
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

            // ── No-progress oscillation guard (LoopTrap arXiv:2605.05846) ──
            // A *rejection* re-dispatch (task came back to `pending` with fresh
            // judge_feedback, not merely a stalled pickup) whose feedback is
            // identical to the previous rejection means the Generator-Verifier
            // loop is not converging. Escalate to a human instead of spending the
            // rest of the iteration budget on the same failure.
            let current_fb_norm = task
                .judge_feedback
                .as_deref()
                .map(normalize_feedback)
                .filter(|s| !s.is_empty());
            let is_rejection_redispatch = matches!(&entry, Some(e) if !e.awaiting_pickup);
            if is_rejection_redispatch
                && let (Some(cur), Some(prev)) = (
                    current_fb_norm.as_ref(),
                    entry.as_ref().and_then(|e| e.last_feedback.as_ref()),
                )
                && cur == prev
            {
                self.post_activity(
                    "goal_loop.oscillation",
                    &task.assigned_to,
                    Some(&task.id),
                    &format!(
                        "goal-loop 偵測到連續兩輪駁回且回饋雷同,無進展 — 轉人工 {}",
                        task.title
                    ),
                )
                .await;
                self.escalate(&mut inflight, task, "goal-loop no-progress oscillation")
                    .await?;
                active = inflight.len();
                continue;
            }

            // ── Iteration guard ──
            let current_iter = entry.as_ref().map(|e| e.iter).unwrap_or(0);
            if current_iter >= self.config.iteration_cap {
                self.escalate(&mut inflight, task, "goal-loop iteration cap")
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

            // ── Dispatch: enqueue a work message on the existing wake-up rail ──
            let next_iter = current_iter + 1;
            self.enqueue_work(task, next_iter).await?;
            if is_new {
                active += 1;
            }
            // Remember this round's feedback (only meaningful on a rejection
            // re-dispatch) so the next rejection can be compared against it. On a
            // fresh dispatch / stall re-dispatch this is either `None` or a
            // carried-over value; the oscillation guard only fires on
            // `is_rejection_redispatch`, so a stale carry cannot false-trigger.
            let next_last_feedback = current_fb_norm.clone();
            inflight.insert(
                task.id.clone(),
                InFlight {
                    iter: next_iter,
                    enqueued_at: now,
                    awaiting_pickup: true,
                    last_feedback: next_last_feedback,
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
                    "goal-loop {verb} iter {next_iter}/{} — {}",
                    self.config.iteration_cap, task.title
                ),
            )
            .await;
            // ── P5 outer progress board ──────────────────────
            // A rejection re-dispatch (task returned to `pending` with fresh
            // judge feedback) reads as a single "未通過，重試中" line; a fresh /
            // stall dispatch reads as "開始執行 / 重試". Keyed by iteration so each
            // round posts exactly once.
            let cap = self.config.iteration_cap;
            if is_rejection_redispatch && has_feedback {
                self.push_progress(
                    task,
                    &format!("rejected:{next_iter}"),
                    crate::goal_notify::GoalProgress::Rejected { iter: next_iter, cap },
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

    /// True when `now` is more than `wall_clock_hours` past `created_at`.
    /// An unparseable timestamp is treated as *not* expired (fail-open on the
    /// deadline only — the iteration cap still bounds the loop).
    fn deadline_exceeded(&self, created_at: &str, now: DateTime<Utc>) -> bool {
        match DateTime::parse_from_rfc3339(created_at) {
            Ok(created) => {
                (now - created.with_timezone(&Utc)).num_hours() >= self.config.wall_clock_hours
            }
            Err(_) => false,
        }
    }

    /// Park a task for a human and drop its in-flight tracking.
    async fn escalate(
        &self,
        inflight: &mut HashMap<String, InFlight>,
        task: &TaskRow,
        reason: &str,
    ) -> Result<(), String> {
        self.store.mark_needs_human(&task.id, reason).await?;
        inflight.remove(&task.id);
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
    async fn reconcile_needs_human(&self) {
        let tasks = match self.store.tasks_in_status("needs_human").await {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "goal loop: needs_human scan failed (will retry)");
                return;
            }
        };
        let live: HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
        let mut notified = self.notified_needs_human.lock().await;
        notified.retain(|id| live.contains(id));

        for task in &tasks {
            if !task.goal_mode || notified.contains(&task.id) {
                continue;
            }
            let level = AutonomyLevel::for_agent(&self.home_dir, &task.assigned_to);
            if level == AutonomyLevel::Observer {
                // Observer: notify-only, no waiting — resolve straight to cancelled.
                match self
                    .store
                    .resolve_needs_human(&task.id, "abort", "Observer 全自動模式:需人工需求自動結束")
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
                    Err(e) => warn!(task = %task.id, error = %e, "goal loop: observer auto-close failed"),
                }
            } else {
                // Operator/Collaborator/Consultant/Approver: push retry/done/abort
                // buttons to the agent control channel, and mirror a plain
                // heads-up to the goal's source conversation.
                let sent = crate::goal_notify::notify_goal_needs_human(&self.home_dir, task).await;
                self.push_progress(task, "needs_human", crate::goal_notify::GoalProgress::NeedsHuman)
                    .await;
                self.post_activity(
                    "goal_loop.needs_human_notified",
                    &task.assigned_to,
                    Some(&task.id),
                    &format!(
                        "已推播需人工審批 — {}(推播{})",
                        task.title,
                        if sent { "成功" } else { "略過" }
                    ),
                )
                .await;
            }
            notified.insert(task.id.clone());
        }
    }

    /// Kickoff gate for a Collaborator/Consultant goal task: on first sight,
    /// file a kickoff approval + push it to the channel and WAIT; on later ticks
    /// poll it — approved ⇒ proceed, denied/expired ⇒ abort the task.
    async fn kickoff_gate(&self, task: &TaskRow) -> Result<KickoffGate, String> {
        let Some(broker) = &self.broker else {
            warn!(task = %task.id, "goal loop: kickoff requested but no ApprovalBroker; proceeding");
            return Ok(KickoffGate::Proceed);
        };
        let mut kickoff = self.kickoff.lock().await;
        match kickoff.get(&task.id).cloned() {
            None => {
                // First encounter: request approval, push, and wait.
                let summary = format!(
                    "目標:{} — 最多 {} 輪自主嘗試",
                    task.title, self.config.iteration_cap
                );
                let payload = json!({ "task_id": task.id, "agent": task.assigned_to });
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
                crate::goal_notify::notify_goal_kickoff(
                    &self.home_dir,
                    &task.assigned_to,
                    id.as_str(),
                    &summary,
                )
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
            Some(id) => match broker.poll(&id).await? {
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
                ApprovalStatus::Pending => Ok(KickoffGate::Waiting),
                // Denied / Expired (TTL = deny, fail-closed) ⇒ abort the goal.
                other => {
                    kickoff.remove(&task.id);
                    let reason = format!("kickoff {} — 目標未啟動", other.as_str());
                    if let Err(e) = self.store.cancel_task(&task.id, &reason).await {
                        warn!(task = %task.id, error = %e, "goal loop: kickoff abort cancel failed");
                    }
                    self.post_activity(
                        "goal_loop.kickoff_denied",
                        &task.assigned_to,
                        Some(&task.id),
                        &format!("人工未核准({})— 目標放棄 {}", other.as_str(), task.title),
                    )
                    .await;
                    Ok(KickoffGate::Aborted)
                }
            },
        }
    }

    /// Enqueue a work message for `task` onto `message_queue.db` — the same rail
    /// the heartbeat's task-board pull uses, so the existing dispatcher routes it
    /// to the agent unchanged. Carries `judge_feedback` (if any) so a rejected
    /// task is retried *with* the reviewer's feedback.
    async fn enqueue_work(&self, task: &TaskRow, iter: u32) -> Result<(), String> {
        let marker = format!("[goal-loop task_id={} iter={iter}]", task.id);
        let feedback_block = match task.judge_feedback.as_deref() {
            Some(fb) if !fb.trim().is_empty() => format!(
                "\n\n上一輪驗收未通過,驗收判官的回饋如下,請據此修正後再回報:\n\
                 <judge_feedback>\n{fb}\n</judge_feedback>"
            ),
            _ => String::new(),
        };
        let criteria_block = match task.acceptance_criteria.as_deref() {
            Some(c) if !c.trim().is_empty() => {
                format!("\n• 驗收標準: {c}")
            }
            _ => String::new(),
        };
        let payload = format!(
            "{marker} 你有一個自主目標任務要推進:\n\
             • Task ID: {}\n\
             • 標題: {}\n\
             • 說明: {}{criteria_block}\n\n\
             請使用 MCP 工具 `tasks_claim` 認領這項任務,執行後用 `tasks_complete` \
             回報結果(務必在 result_summary 寫清楚你做了什麼、產出在哪),\
             系統會由驗收判官檢核是否達成驗收標準。若受阻無法完成,使用 `tasks_block` \
             說明原因。{feedback_block}",
            task.id, task.title, task.description,
        );

        let msg = QueueMessage {
            id: uuid::Uuid::new_v4().to_string(),
            sender: "goal-loop-driver".to_string(),
            target: task.assigned_to.clone(),
            payload,
            status: MessageStatus::Pending,
            retry_count: 0,
            delegation_depth: 0,
            origin_agent: None,
            sender_agent: None,
            error: None,
            response: None,
            created_at: Utc::now().to_rfc3339(),
            acked_at: None,
            completed_at: None,
            reply_channel: None,
            turn_id: None,
            session_id: None,
        };
        self.queue.enqueue(&msg).await
    }

    /// P5: push one progress line to the goal's source conversation, deduped by
    /// `phase_key` so the same phase never double-posts. Best-effort — a failed
    /// push is silent (the Activity Feed already recorded the transition).
    async fn push_progress(
        &self,
        task: &TaskRow,
        phase_key: &str,
        progress: crate::goal_notify::GoalProgress,
    ) {
        {
            let mut seen = self.progress_seen.lock().await;
            if seen.get(&task.id).map(|s| s == phase_key).unwrap_or(false) {
                return;
            }
            seen.insert(task.id.clone(), phase_key.to_string());
        }
        crate::goal_notify::notify_goal_progress(&self.home_dir, task, progress).await;
    }

    /// Best-effort append to the dashboard Activity Feed. A failure here must not
    /// break the loop — it is progress telemetry, not control flow.
    async fn post_activity(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_store::TaskRow;

    fn driver(store: Arc<TaskStore>, queue: Arc<MessageQueue>, cfg: GoalLoopConfig) -> GoalLoopDriver {
        GoalLoopDriver::new(store, queue, cfg)
    }

    fn small_cfg() -> GoalLoopConfig {
        GoalLoopConfig {
            iteration_cap: 2,
            wall_clock_hours: 24,
            max_concurrent: 3,
            tick_secs: 30,
            stalled_secs: 600,
        }
    }

    /// A todo goal task assigned to `agent`.
    fn goal_task(id: &str, agent: &str) -> TaskRow {
        let mut t = TaskRow::new(
            id.into(),
            format!("goal {id}"),
            "do the work".into(),
            "medium".into(),
            agent.into(),
            "system".into(),
        );
        t.status = "todo".into();
        t.goal_mode = true;
        t.acceptance_criteria = Some("must be correct".into());
        t
    }

    async fn open_stores(dir: &Path) -> (Arc<TaskStore>, Arc<MessageQueue>) {
        let store = Arc::new(TaskStore::open(dir).unwrap());
        let queue = Arc::new(MessageQueue::open(dir).unwrap());
        (store, queue)
    }

    #[test]
    fn config_defaults_and_partial_section() {
        // Absent section ⇒ defaults.
        let d = GoalLoopConfig::default();
        assert_eq!(d.iteration_cap, 8);
        assert_eq!(d.max_concurrent, 3);

        // Partial section ⇒ only the given field overrides; the rest default.
        let toml = "[goal_loop]\niteration_cap = 5\n";
        let table: toml::Table = toml.parse().unwrap();
        let cfg: GoalLoopConfig =
            table.get("goal_loop").unwrap().clone().try_into().unwrap();
        assert_eq!(cfg.iteration_cap, 5);
        assert_eq!(cfg.max_concurrent, 3, "unspecified field keeps its default");
        assert_eq!(cfg.wall_clock_hours, 24);
    }

    #[tokio::test]
    async fn candidate_selection_enqueues_only_assigned_goal_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // (1) assigned goal task → should dispatch.
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();
        // (2) goal task with no assignee → skipped.
        store.insert_task(&goal_task("g2", "  ")).await.unwrap();
        // (3) non-goal task assigned → skipped (not goal_mode).
        let mut plain = goal_task("g3", "alice");
        plain.goal_mode = false;
        store.insert_task(&plain).await.unwrap();

        let d = driver(store, queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1, "only the assigned goal task is dispatched");
        assert_eq!(pending[0].target, "alice");
        assert!(pending[0].payload.contains("[goal-loop task_id=g1 iter=1]"));
    }

    #[tokio::test]
    async fn in_flight_dedup_does_not_re_enqueue_while_awaiting_pickup() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let d = driver(store, queue.clone(), small_cfg());
        // Two ticks back-to-back: the task is still `todo` (agent hasn't picked
        // it up) and the stall timeout has not elapsed ⇒ only one enqueue.
        d.tick_once().await.unwrap();
        d.tick_once().await.unwrap();

        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1, "no duplicate enqueue while awaiting pickup");
    }

    #[tokio::test]
    async fn iteration_cap_escalates_to_needs_human() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        // iteration_cap = 2, stall = 0 so every tick re-dispatches the (never
        // picked up) task, counting an iteration each time.
        let cfg = GoalLoopConfig {
            iteration_cap: 2,
            stalled_secs: 0,
            ..small_cfg()
        };
        let d = driver(store.clone(), queue.clone(), cfg);

        d.tick_once().await.unwrap(); // iter 1
        d.tick_once().await.unwrap(); // iter 2 (== cap after this dispatch)
        d.tick_once().await.unwrap(); // current_iter 2 >= cap ⇒ escalate

        let t = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human");
        assert_eq!(
            t.judge_feedback.as_deref(),
            Some("goal-loop iteration cap")
        );
        // Two work messages enqueued (iter 1 and 2); the 3rd tick escalated
        // instead of dispatching.
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 2);
    }

    #[tokio::test]
    async fn deadline_cap_escalates_to_needs_human() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // Task created 48h ago, wall-clock budget 24h ⇒ deadline exceeded.
        let mut t = goal_task("g1", "alice");
        t.created_at = (Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human");
        assert_eq!(got.judge_feedback.as_deref(), Some("goal-loop deadline"));
        // No work message enqueued — the deadline guard fired before dispatch.
        assert!(queue.pending_messages(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn concurrency_cap_bounds_new_dispatches_per_tick() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        for i in 0..5 {
            store
                .insert_task(&goal_task(&format!("g{i}"), "alice"))
                .await
                .unwrap();
        }

        let cfg = GoalLoopConfig {
            max_concurrent: 2,
            ..small_cfg()
        };
        let d = driver(store, queue.clone(), cfg);
        d.tick_once().await.unwrap();

        // Only 2 of the 5 goal tasks admitted this tick.
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 2, "concurrency cap admits at most 2 new tasks");
    }

    #[tokio::test]
    async fn rejected_task_is_re_dispatched_with_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // Simulate a task that came back from a judge rejection: pending, with
        // judge_feedback and a prior retry.
        let mut t = goal_task("g1", "alice");
        t.status = "pending".into();
        t.judge_feedback = Some("missing the summary section".into());
        store.insert_task(&t).await.unwrap();

        let d = driver(store, queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0].payload.contains("missing the summary section"),
            "retry message must carry the judge feedback"
        );
        assert!(pending[0].payload.contains("上一輪驗收未通過"));
    }

    // ── P3 no-progress oscillation guard ────────────────────

    /// Drive one full rejection round for a task already tracked in-flight and
    /// awaiting pickup: (1) agent moves it to `review` and a tick observes that
    /// (flips `awaiting_pickup=false`, does not re-dispatch); (2) the judge
    /// rejects with `feedback` (→ `pending`, `judge_feedback` set); (3) the next
    /// tick is the rejection re-dispatch the caller runs. This helper performs
    /// steps 1–2 and returns; the caller ticks for step 3.
    async fn agent_round_then_reject(
        d: &GoalLoopDriver,
        store: &Arc<TaskStore>,
        id: &str,
        feedback: &str,
    ) {
        // Agent picked it up and produced work → review.
        store
            .update_task(id, &serde_json::json!({ "status": "review" }))
            .await
            .unwrap();
        // Tick while in review so the driver marks it no-longer-awaiting-pickup.
        d.tick_once().await.unwrap();
        // Judge rejects → pending + judge_feedback.
        store.reject_review(id, feedback).await.unwrap();
    }

    #[tokio::test]
    async fn identical_feedback_two_rounds_escalates_oscillation() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // High iteration cap so ONLY the oscillation guard can escalate here.
        let cfg = GoalLoopConfig { iteration_cap: 10, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100; // don't let reject_review self-escalate
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), cfg);

        // Initial dispatch (iter 1, awaiting pickup).
        d.tick_once().await.unwrap();

        // Round 1: agent works, judge rejects "same". Next tick re-dispatches
        // (first rejection — records the feedback, no oscillation yet).
        agent_round_then_reject(&d, &store, "g1", "same reason").await;
        d.tick_once().await.unwrap();
        assert_ne!(
            store.get_task("g1").await.unwrap().unwrap().status,
            "needs_human",
            "first rejection must not escalate"
        );

        // Round 2: identical feedback again ⇒ oscillation on the next tick.
        agent_round_then_reject(&d, &store, "g1", "same reason").await;
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human");
        assert_eq!(
            got.judge_feedback.as_deref(),
            Some("goal-loop no-progress oscillation")
        );

        let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
        assert!(
            acts.iter().any(|a| a.event_type == "goal_loop.oscillation"),
            "an oscillation activity must be recorded"
        );
    }

    #[tokio::test]
    async fn differing_feedback_keeps_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        let cfg = GoalLoopConfig { iteration_cap: 10, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100;
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), cfg);

        d.tick_once().await.unwrap();

        agent_round_then_reject(&d, &store, "g1", "first problem").await;
        d.tick_once().await.unwrap();

        // Second rejection has DIFFERENT feedback ⇒ NOT oscillation.
        agent_round_then_reject(&d, &store, "g1", "a completely different problem").await;
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_ne!(got.status, "needs_human", "differing feedback must keep retrying");
        assert_eq!(got.status, "pending");
        let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
        assert!(
            !acts.iter().any(|a| a.event_type == "goal_loop.oscillation"),
            "no oscillation should be recorded for differing feedback"
        );
    }

    // ── P2a autonomy level + kickoff gate ───────────────────

    fn write_agent_toml(home: &Path, agent: &str, body: &str) {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), body).unwrap();
    }

    #[test]
    fn autonomy_level_parses_and_defaults_conservative() {
        assert_eq!(AutonomyLevel::from_toml_str("operator"), AutonomyLevel::Operator);
        assert_eq!(
            AutonomyLevel::from_toml_str("  Collaborator "),
            AutonomyLevel::Collaborator
        );
        assert_eq!(AutonomyLevel::from_toml_str("CONSULTANT"), AutonomyLevel::Consultant);
        assert_eq!(AutonomyLevel::from_toml_str("observer"), AutonomyLevel::Observer);
        // Unknown / empty ⇒ Approver (never the most-autonomous level).
        assert_eq!(AutonomyLevel::from_toml_str("wat"), AutonomyLevel::Approver);
        assert_eq!(AutonomyLevel::from_toml_str(""), AutonomyLevel::Approver);
    }

    #[test]
    fn autonomy_for_agent_reads_toml_and_fails_safe() {
        let dir = tempfile::tempdir().unwrap();
        // Missing agent.toml ⇒ Approver.
        assert_eq!(
            AutonomyLevel::for_agent(dir.path(), "ghost"),
            AutonomyLevel::Approver
        );
        write_agent_toml(
            dir.path(),
            "alice",
            "[capabilities]\nautonomy_level = \"operator\"\n",
        );
        assert_eq!(
            AutonomyLevel::for_agent(dir.path(), "alice"),
            AutonomyLevel::Operator
        );
        // Malformed toml ⇒ Approver (fail-safe).
        write_agent_toml(dir.path(), "bob", "not = valid [[[");
        assert_eq!(
            AutonomyLevel::for_agent(dir.path(), "bob"),
            AutonomyLevel::Approver
        );
    }

    #[tokio::test]
    async fn operator_agent_is_not_auto_dispatched() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        write_agent_toml(
            dir.path(),
            "alice",
            "[capabilities]\nautonomy_level = \"operator\"\n",
        );
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let d = GoalLoopDriver::new(store, queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf());
        d.tick_once().await.unwrap();

        assert!(
            queue.pending_messages(10).await.unwrap().is_empty(),
            "operator-level agent is never auto-driven"
        );
    }

    #[tokio::test]
    async fn collaborator_kickoff_gates_then_dispatches_on_approve() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        write_agent_toml(
            dir.path(),
            "alice",
            "[capabilities]\nautonomy_level = \"collaborator\"\n",
        );
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let broker = Arc::new(crate::approval::ApprovalBroker::open(dir.path()).unwrap());
        let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf())
            .with_broker(broker.clone());

        // Tick 1: kickoff filed, task NOT dispatched.
        d.tick_once().await.unwrap();
        assert!(
            queue.pending_messages(10).await.unwrap().is_empty(),
            "no dispatch before kickoff approval"
        );
        let pending = broker.list_pending(Some("alice")).await.unwrap();
        assert_eq!(pending.len(), 1, "kickoff approval filed");
        assert_eq!(pending[0].action_kind, "goal_kickoff");
        let approval_id = pending[0].id.clone();

        // Human approves → tick 2 dispatches.
        broker.decide(&approval_id, true, "test:alice").await.unwrap();
        d.tick_once().await.unwrap();
        let dispatched = queue.pending_messages(10).await.unwrap();
        assert_eq!(dispatched.len(), 1, "dispatched after kickoff approval");
        assert_eq!(dispatched[0].target, "alice");
    }

    #[tokio::test]
    async fn consultant_kickoff_denied_aborts_the_goal() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        write_agent_toml(
            dir.path(),
            "alice",
            "[capabilities]\nautonomy_level = \"consultant\"\n",
        );
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let broker = Arc::new(crate::approval::ApprovalBroker::open(dir.path()).unwrap());
        let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf())
            .with_broker(broker.clone());

        d.tick_once().await.unwrap(); // kickoff filed
        let approval_id = broker.list_pending(Some("alice")).await.unwrap()[0].id.clone();
        broker.decide(&approval_id, false, "test:alice").await.unwrap(); // deny (== TTL fail-closed)

        d.tick_once().await.unwrap(); // poll → denied → abort
        assert_eq!(
            store.get_task("g1").await.unwrap().unwrap().status,
            "cancelled"
        );
        assert!(
            queue.pending_messages(10).await.unwrap().is_empty(),
            "denied kickoff never dispatches"
        );
    }
}
