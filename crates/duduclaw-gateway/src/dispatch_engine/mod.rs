//! G1: durable multi-agent dispatch engine (対標 Hermes Kanban swarm /
//! paperclip wakeup queue).
//!
//! ## Migration direction
//!
//! Cross-agent delegation historically flowed through the file-based IPC rail
//! (`bus_queue.jsonl`, consumed by [`crate::dispatcher`]). That rail is fragile:
//! no zombie recovery, no dependency graph, no atomic-claim guarantee. It stays
//! as a **compatibility path** — existing producers/consumers are untouched — but
//! NEW durable work goes through the SQLite task lifecycle in
//! [`crate::task_store`]: `pending` → [`TaskStore::atomic_claim`] →
//! `in_progress` (leased) → `done` / `review` (goal mode) / `failed` /
//! `needs_human`.
//!
//! ## What this engine owns
//!
//! A single background loop (mirrors the heartbeat scheduler's 30s cadence) that
//! provides the durability guarantees the file rail lacks:
//!
//! - **Atomic claim** — the primitive itself lives in `task_store`
//!   ([`TaskStore::atomic_claim`], a conditional `UPDATE`); workers call it via
//!   the `tasks_claim` MCP tool. Exactly one claimer wins.
//! - **Lease renewal** — a live worker keeps its claim alive two ways:
//!   in-process execution paths hold a [`LeaseRenewalGuard`] (background ticker
//!   at `lease_secs / 3`, stops when the guard drops / the task is released);
//!   external agent processes that claimed via the `tasks_claim` MCP tool
//!   heartbeat explicitly with the `tasks_renew` MCP tool.
//! - **Zombie reclaim** — leased tasks whose worker died (lease elapsed with no
//!   renewal) are requeued (retry budget permitting) or failed. This loop drives
//!   it every tick. Reclaim is *conservative*: a task is only reclaimed when its
//!   lease expired AND a further full lease window passed with no renewal
//!   ([`crate::task_store::zombie_reclaim_due`]), so a worker whose renewal
//!   ticker is still running is never falsely reclaimed.
//! - **Dependency unlock** — enforced at claim time via
//!   [`TaskStore::claimable_tasks`], which filters tasks whose `depends_on` ids
//!   are not all `done`.
//! - **Goal mode** — tasks marked `goal_mode` route their completion to a
//!   `review` state; this loop runs the injected [`AcceptanceJudge`] against the
//!   acceptance criteria. Pass → `done`; fail → requeue with feedback (or
//!   `needs_human` once the retry budget is spent). **Fail-safe:** if the judge
//!   itself errors, the task is parked as `needs_human` — never auto-accepted,
//!   never looped.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use tokio::time;
use tracing::{debug, info, warn};

use crate::runtime::NativeToolEvent;
use crate::task_store::{TaskRow, TaskStore};

// `catch_unwind` for futures — same extension trait
// `subagent_prediction::spawn_record` uses (design R5: forward-model
// bookkeeping must never panic the review hot path).
use futures_util::FutureExt as _;

/// Default worker lease. A claim not renewed within this window is a zombie.
pub const DEFAULT_LEASE_SECS: i64 = 300;
/// Default dispatcher tick.
pub const DEFAULT_TICK_SECS: u64 = 30;
/// Iterative Kanban soft cap (rounds before the `diminishing` flag is raised on
/// a rejected goal task). Default mirrors `GoalLoopConfig::soft_cap`.
pub const DEFAULT_SOFT_CAP: i64 = 3;

/// Whether the background dispatch engine (zombie reclaim + goal-mode review)
/// runs. **Default ON** since v1.59 (the conservative default-off rollout ended
/// when the `/goals` + `/foresight` dashboard pages made the goal loop a
/// first-class surface; an explicit `[dispatch] enabled = false` opts out).
///
/// History: this gate was introduced because `renew_lease` had zero callers —
/// any task outliving the fixed lease would have been falsely reclaimed and
/// re-executed (HIGH finding, 2026-07 review). That gap is now closed:
/// ① in-process execution paths hold a [`LeaseRenewalGuard`] renewal ticker,
/// ② external MCP workers heartbeat via the `tasks_renew` tool, and
/// ③ reclaim itself is conservative (lease expired AND one further full lease
/// window with no renewal — `task_store::zombie_reclaim_due`). Enabling the
/// engine is safe.
///
/// Disable path: set `config.toml [dispatch] enabled = false` in the DuDuClaw
/// home dir, or export `DUDUCLAW_DISPATCH_ENGINE=0` (env wins); the dashboard
/// automation settings expose the same switch (hot reload, no restart). The
/// synchronous primitives (`atomic_claim`, dependency gating via
/// `claimable_tasks`, `complete_task`) reached through the MCP task tools work
/// regardless of this flag; the flag only gates the background reclaim/review
/// loop and the goal-loop driver.
pub fn dispatch_engine_enabled(home_dir: &std::path::Path) -> bool {
    if let Ok(val) = std::env::var("DUDUCLAW_DISPATCH_ENGINE") {
        return matches!(val.as_str(), "1" | "true" | "yes");
    }
    let config_path = home_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(section) = table.get("dispatch").and_then(|v| v.as_table()) {
                if let Some(val) = section.get("enabled").and_then(|v| v.as_bool()) {
                    return val;
                }
            }
        }
    }
    // Default ON since v1.59: the goal-task board + foresight pages are a
    // headline surface, and an idle engine costs only periodic SQLite polls
    // (the acceptance judge runs an LLM call only when a goal-mode task
    // actually reaches `review`).
    true
}

// ── Lease renewal (G1) ──────────────────────────────────────

/// RAII lease-renewal ticker for an in-process worker holding a claimed task.
///
/// Any gateway-side execution path that claims a task and runs the work itself
/// (e.g. spawning a CLI subprocess for it) must hold one of these alongside the
/// child for the task's whole runtime: it renews the lease every
/// `lease_secs / 3` while the worker is genuinely alive, and stops
/// automatically when
/// - the guard is dropped (worker finished / caller scope ended), or
/// - [`LeaseRenewalGuard::stop`] is called, or
/// - the store reports the task is no longer held by this agent (renewal
///   returns `false` — reclaimed, completed elsewhere, or reassigned).
///
/// External agent processes that claim via the `tasks_claim` MCP tool cannot
/// hold an in-process guard; they heartbeat with the `tasks_renew` MCP tool
/// instead.
pub struct LeaseRenewalGuard {
    handle: tokio::task::JoinHandle<()>,
}

impl LeaseRenewalGuard {
    /// Spawn the renewal ticker for `task_id` held by `agent_id`.
    /// Tick interval = `lease_secs / 3` (min 1s in whole-second terms, computed
    /// in millis so short test leases still tick multiple times per window).
    pub fn spawn(
        store: Arc<TaskStore>,
        task_id: String,
        agent_id: String,
        lease_secs: i64,
    ) -> Self {
        let tick = Duration::from_millis(((lease_secs.max(1) * 1000) / 3).max(50) as u64);
        let handle = tokio::spawn(async move {
            loop {
                time::sleep(tick).await;
                let now = Utc::now();
                let new_expiry = (now + chrono::Duration::seconds(lease_secs)).to_rfc3339();
                match store
                    .renew_lease(&task_id, &agent_id, &new_expiry, &now.to_rfc3339())
                    .await
                {
                    Ok(true) => {
                        debug!(task = %task_id, %new_expiry, "lease renewed");
                    }
                    Ok(false) => {
                        // No longer ours (done / reclaimed / reassigned) — stop
                        // heartbeating rather than fight the store.
                        debug!(task = %task_id, "lease no longer held — renewal ticker stops");
                        break;
                    }
                    Err(e) => {
                        // Transient store error: keep trying — the conservative
                        // reclaim grace window absorbs a missed tick.
                        warn!(task = %task_id, error = %e, "lease renewal failed (will retry)");
                    }
                }
            }
        });
        Self { handle }
    }

    /// Stop renewing immediately (idempotent; also happens on drop).
    pub fn stop(&self) {
        self.handle.abort();
    }
}

impl Drop for LeaseRenewalGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}


// ── Submodules (audit O6 file split; pure code motion) ──────
//
// This module was one 7,263-line file. It is now a directory module
// whose submodules hold the same code verbatim; every path that used to
// resolve through `crate::dispatch_engine::…` still does, via the
// re-exports below.

mod grounding;
mod judge;
mod pre_evaluator;
mod review;
mod settle;
mod tool_activity_fmt;

use grounding::*;
pub use judge::*;
pub use pre_evaluator::*;
pub(crate) use tool_activity_fmt::*;

// ── Engine ──────────────────────────────────────────────────

/// The durable dispatch engine background task.
pub struct DispatchEngine {
    store: Arc<TaskStore>,
    /// Goal-mode acceptance judge. `None` ⇒ goal-mode `review` tasks are left
    /// in place (no evaluator configured) rather than auto-accepted.
    judge: Option<Arc<dyn AcceptanceJudge>>,
    /// H1 first-stage evaluator (two-stage adjudication). `None` ⇒ every
    /// review goes straight to the MAV panel, byte-identical to the behavior
    /// before this feature existed. The `[dispatch] two_stage_judge` config
    /// flag gates it a second time at review time (hot-reloadable).
    evaluator: Option<Arc<dyn PreAcceptanceEvaluator>>,
    lease_secs: i64,
    tick_secs: u64,
    running: Arc<AtomicBool>,
    /// Home dir to read `tool_calls.jsonl` from for the WP4 `<tool_activity>`
    /// judge evidence block. `None` ⇒ the block is never built (same
    /// behavior as a missing audit file).
    home_dir: Option<std::path::PathBuf>,
    /// Iterative Kanban soft cap passed to `reject_review` (drives the
    /// `diminishing` flag; does NOT block the loop).
    soft_cap: i64,
    /// WP-A9: A3 task-forward-model (design §4.2). `None` ⇒ the settle hook
    /// is a complete no-op — same as before this field existed (design
    /// §7.3's `enabled = false` default-off contract). Shared with the
    /// `GoalLoopDriver`'s predict hook via the same `Arc` (see the
    /// caller-side wiring notes in `handlers.rs`) so both hooks read/write
    /// the same in-memory statistical-bucket cache.
    forward_model: Option<Arc<crate::prediction::task_forward_store::TaskForwardModel>>,
    /// HTTP client for the Y8-3 T1 update-report reconciliation sweep's
    /// channel notification delivery (`reminder_scheduler::send_channel_
    /// message`). Reused across ticks rather than constructed per-sweep —
    /// same reasoning as any other long-lived `reqwest::Client` in this
    /// codebase (connection pooling), just newly relevant here because this
    /// is the first thing `DispatchEngine` does that makes an outbound HTTP
    /// call.
    http: reqwest::Client,
}

impl DispatchEngine {
    pub fn new(store: Arc<TaskStore>, judge: Option<Arc<dyn AcceptanceJudge>>) -> Self {
        Self {
            store,
            judge,
            evaluator: None,
            lease_secs: DEFAULT_LEASE_SECS,
            tick_secs: DEFAULT_TICK_SECS,
            running: Arc::new(AtomicBool::new(false)),
            home_dir: None,
            soft_cap: DEFAULT_SOFT_CAP,
            forward_model: None,
            http: reqwest::Client::new(),
        }
    }

    /// Inject a specific `reqwest::Client` (tests / a caller that wants
    /// connection-pool sharing with another subsystem). Omit to keep the
    /// default `reqwest::Client::new()` built in [`Self::new`].
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// H1: wire the cheap first-stage evaluator. Omit (default `None`) to keep
    /// every review on the single-stage MAV path.
    pub fn with_evaluator(mut self, evaluator: Arc<dyn PreAcceptanceEvaluator>) -> Self {
        self.evaluator = Some(evaluator);
        self
    }

    /// WP-A9: wire the A3 task-forward-model settle hook. Omit (default
    /// `None`) to keep the hook a no-op — the `[task_forward_model] enabled`
    /// gate (design §7.3) is enforced by the caller deciding whether to
    /// construct a `TaskForwardModel` at all, not by a flag read here.
    pub fn with_forward_model(
        mut self,
        forward_model: Arc<crate::prediction::task_forward_store::TaskForwardModel>,
    ) -> Self {
        self.forward_model = Some(forward_model);
        self
    }

    pub fn with_lease_secs(mut self, secs: i64) -> Self {
        self.lease_secs = secs;
        self
    }

    /// Set the Iterative Kanban soft cap (rounds → `diminishing` flag). Wired
    /// from `GoalLoopConfig::soft_cap` at startup.
    pub fn with_soft_cap(mut self, soft_cap: i64) -> Self {
        self.soft_cap = soft_cap;
        self
    }

    pub fn with_tick_secs(mut self, secs: u64) -> Self {
        self.tick_secs = secs;
        self
    }

    /// Enable the WP4 `<tool_activity>` judge evidence block, read from
    /// `<home_dir>/tool_calls.jsonl`.
    pub fn with_home_dir(mut self, home_dir: std::path::PathBuf) -> Self {
        self.home_dir = Some(home_dir);
        self
    }

    /// Lease deadline for a claim taken `now`. Exposed so the MCP `tasks_claim`
    /// handler stamps a consistent lease.
    pub fn lease_secs(&self) -> i64 {
        self.lease_secs
    }

    /// Stop the loop after the current tick.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Run the dispatcher loop. Mirrors the heartbeat scheduler: sleep, then a
    /// tick of durable maintenance (zombie reclaim + goal-mode review).
    pub async fn run(self: Arc<Self>) {
        self.running.store(true, Ordering::SeqCst);
        info!(
            lease_secs = self.lease_secs,
            tick_secs = self.tick_secs,
            "Dispatch engine started (durable SQLite派工)"
        );
        while self.running.load(Ordering::SeqCst) {
            time::sleep(Duration::from_secs(self.tick_secs)).await;
            if let Err(e) = self.tick_once().await {
                warn!(error = %e, "派工引擎 tick 失敗（將於下一輪重試）");
            }
        }
        warn!("Dispatch engine stopped");
    }

    /// One maintenance pass. Public for tests and one-shot recovery.
    pub async fn tick_once(&self) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();

        // 1) Zombie reclaim — durability guarantee.
        let reclaimed = self.store.reclaim_zombies(&now).await?;
        for z in &reclaimed {
            match z.action {
                crate::task_store::ZombieAction::Requeue => {
                    info!(task = %z.task_id, retry = z.retry_count, "殭屍任務回收：已重新排入 pending");
                }
                crate::task_store::ZombieAction::Fail => {
                    warn!(task = %z.task_id, "殭屍任務回收：重試上限耗盡，標記 failed");
                }
            }
        }

        // 2) Goal-mode acceptance review.
        self.review_goal_tasks().await?;

        // 3) WP3 (PORTICO): sweep expired capability grants (hard-TTL backstop).
        // Piggy-backs on this existing periodic tick — no new timer. Gated on a
        // wired home_dir (tests without one skip it); best-effort (a sweep error
        // never fails the tick, active-grant checks already exclude expired rows).
        if let Some(home) = &self.home_dir {
            match crate::capability_grants::CapabilityGrantStore::open(home) {
                Ok(store) => {
                    if let Err(e) = store.expire_stale().await {
                        warn!(error = %e, "capability grant expire_stale sweep failed");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "capability grant store open failed for expire sweep")
                }
            }
        }

        // 4) Maintenance-mode Entry A (`DESIGN-maintenance-mode-2026-08.md`
        // §2.4): TTL sweep. Same "piggy-back on the existing tick, no new
        // timer" reasoning as the capability-grant sweep above — this is the
        // ONE other place in the codebase the design doc explicitly names as
        // a home for this ("唯二現成的 TTL sweep 宿主之一"). Absolute-time
        // comparison lives inside `expire_stale` itself; a sweep failure here
        // never fails the tick (the active-window read already excludes
        // expired rows on its own, so a missed sweep only delays the close
        // action + audit line, never lets `status()` lie about being active).
        if let Some(home) = &self.home_dir {
            crate::maintenance::sweep_expired_maintenance_window(home).await;
        }

        // 5) Y8-3 T1 (`commercial/docs/DESIGN-agent-body-update-2026-08.md`
        // §3.4/§13): agent-body update vertical slice's cross-restart result
        // reconciliation. Same "piggy-back on the existing tick, no new
        // timer" reasoning as steps 3/4 above — this is also the module that
        // actually triggers the gateway's own self-restart for an
        // agent-initiated `system`-target update (the MCP tool path runs in
        // a different, short-lived process and cannot do that itself; see
        // `update_report_reconcile.rs`'s module doc for the full chain of
        // reasoning). Best-effort: failures are logged inside the sweep
        // itself and never propagate here.
        if let Some(home) = &self.home_dir {
            crate::update_report_reconcile::sweep(home, &self.http).await;
        }
        Ok(())
    }

}

#[cfg(test)]
mod tests;
