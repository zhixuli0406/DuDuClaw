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
//!
//! ## A1/A2: predict-act-verify instead of generate-then-judge
//!
//! Every dispatch payload now carries a structured [`crate::goal_state`]
//! `<state>` block (goal / confirmed facts / pending hypotheses / excluded
//! approaches — StateAct, arXiv:2410.02810) that the harness programmatically
//! fills and updates round to round, and every round is recorded into a
//! [`crate::goal_visit_graph`] `(state_hash, action)` graph (Graph-Based
//! Exploration, arXiv:2512.24156) that replaced the old two-round
//! identical-feedback oscillation guard with structural loop detection. See
//! those two modules' docs for the full design and the honesty/persistence
//! trade-offs made.

// ── Goal-loop submodules (audit O8, 2026-09-29) — the eight single-purpose
//    `goal_*` / `pause_reason` crate-root modules were merged into three
//    siblings under `goal_loop/`. The old crate-root paths stay available as
//    re-exports in `lib.rs` for one release. ──
/// Zero-LLM turn-signal extractors: gap fingerprint (H4), `(state, action)`
/// visit graph (A2), in-round tool-call streak advisory (H10), premature-stop
/// regex panel (H5).
pub mod signals;
/// The structured `<state>` block (A1), the closed `needs_human` pause-reason
/// classification (H11), and the budget-exhausted "best round" picker (WP-4F).
pub mod state;
/// Goal decomposition + plan-first ("想一想") planner (D4 / I-1c).
pub mod plan;
/// WP-G2: per-criterion acceptance ledger (`## 驗收帳本` + `<criteria_status>`).
pub mod criteria_ledger;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time;
use tracing::{debug, info, warn};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::dispatch_policy::DispatchPolicy;
use crate::goal_state::{self, GoalStateSnapshot};
use crate::goal_visit_graph::GoalVisitGraph;
use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
use crate::prediction::task_forward::{GoalKind, RoundPhase, TaskStateKey};
use crate::prediction::task_forward_store::TaskForwardModel;
use crate::task_store::{
    ActivityRow, CONTINUE_MESSAGE_PREFIX, TaskRow, TaskStore, parse_depends_on,
};

// `catch_unwind` for futures — same extension trait
// `subagent_prediction::spawn_record` uses (design R5: forward-model
// bookkeeping must never panic a hot path).
use futures_util::FutureExt as _;

/// TTL for a kickoff approval (Collaborator/Consultant autonomy gate). Expiry
/// counts as a denial (ApprovalBroker fail-closed) ⇒ the goal is aborted.
const KICKOFF_TTL_SECS: i64 = 3600;

// ── Driver internals (audit O6, 2026-09-29) — the driver body was split
//    into one file per phase for size only; every path below is re-exported
//    so `goal_loop::…` callers are unchanged. ──
mod config;
mod dispatch;
mod driver;
mod enqueue;
mod escalate;
mod kickoff;
mod notify_bridge;
mod tick;

#[cfg(test)]
mod tests;

pub use config::{
    DEFAULT_BASELINE_BOUNDARY, DEFAULT_REVIEW_WIP_LIMIT, GoalLoopConfig, ResumeOnRestart,
    baseline_boundary, effective_risk_boundary, pause_inflight_on_restart, review_wip_limit,
};
pub(crate) use config::{DeadlineHit, derive_goal_kind, no_progress_minutes, resolve_deadline_hit};
// `enqueue_goal_work` keeps its `goal_loop::` path (it is `pub(crate)`), but the
// only caller today is the test module, so the binding is test-gated to stay
// warning-free in a release build.
#[cfg(test)]
pub(crate) use enqueue::enqueue_goal_work;

use config::dispatch_backoff_secs;
#[cfg(test)]
use config::count_distinct_hits;

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
    ///
    /// Goes through the shared typed parse point
    /// ([`duduclaw_core::agent_toml`]) rather than a hand-rolled `toml::Value`
    /// walk. The value stays a raw `String` on
    /// [`duduclaw_core::types::CapabilitiesConfig`] precisely so that
    /// [`Self::from_toml_str`]'s lenient "unknown ⇒ Approver" mapping keeps
    /// running here instead of becoming a hard deserialization error.
    pub fn for_agent(home_dir: &Path, agent_id: &str) -> Self {
        duduclaw_core::agent_toml::load_for_agent(home_dir, agent_id)
            .capabilities
            .autonomy_level
            .as_deref()
            .map(AutonomyLevel::from_toml_str)
            .unwrap_or(AutonomyLevel::Approver)
    }

    /// Levels whose first dispatch is gated behind a human kickoff approval.
    fn requires_kickoff(self) -> bool {
        matches!(
            self,
            AutonomyLevel::Collaborator | AutonomyLevel::Consultant
        )
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

/// WP-A9: derive a coarse [`GoalKind`] for the A3 `TaskStateKey` (design
/// §2.2, §9 U3 "顆粒度取捨,可在 WP-A2 實作時用真實 goal 文字樣本試跑決定").
///
/// Zero-LLM, deterministic. Kept as its own small classifier here rather
/// than reusing `prediction::outcome::detect_task_type` — that function
/// classifies *conversational* turns from `SessionMessage` history (a
/// different input shape and a different mission: "what kind of user
/// request is this"), whereas this classifies one goal-loop task's
/// title+description text once at dispatch time. `task_forward.rs`'s
/// `ArtifactShape`/`GoalKind` doc comments explicitly deferred this
/// derivation to WP-A9 (this module, at the call site) rather than baking a
/// specific keyword table into the otherwise dependency-free
/// `task_forward` module.
const GOAL_KIND_OPS_KEYWORDS: [&str; 12] = [
    "部署", "上線", "發佈", "發布", "通知", "寄信", "email", "傳送", "webhook", "deploy", "notify",
    "send",
];
const GOAL_KIND_RESEARCH_KEYWORDS: [&str; 10] = [
    "研究",
    "調查",
    "比較",
    "評估",
    "查詢",
    "research",
    "compare",
    "investigate",
    "分析",
    "評比",
];
const GOAL_KIND_PLANNING_KEYWORDS: [&str; 8] = [
    "計畫", "規劃", "步驟", "方案", "plan", "roadmap", "strategy", "schedule",
];
const GOAL_KIND_CODING_KEYWORDS: [&str; 10] = [
    "程式",
    "程式碼",
    "代碼",
    "寫程式",
    "bug",
    "測試",
    "重構",
    "code",
    "function",
    "implement",
];

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
    /// RFC-27: the edition concurrency-gate lease this task holds while
    /// in-flight. `None` when the gate does not apply (unlimited edition, or
    /// the gate is unwired in tests) — carried forward across re-dispatch,
    /// renewed each tick, released when the task reaches a terminal state.
    lease: Option<duduclaw_core::ConcurrencyLease>,
    /// Queue id of the work message this round dispatched. Lets the tick
    /// notice a SYNCHRONOUS dispatch failure (the dispatcher marked the
    /// message `failed` — runtime not installed, local engine down, auth
    /// refused …) and free the slot at once instead of holding it until
    /// `stalled_secs` / the no-progress escalation.
    message_id: Option<String>,
    /// H22: the round (`iter`) for which the no-progress notice has already
    /// been emitted, so a long-running task reports at most once per round
    /// instead of once per tick. Reset to `None` implicitly on every
    /// re-dispatch, since dispatch rebuilds the whole [`InFlight`] entry.
    progress_reported_round: Option<u32>,
}

/// Consecutive synchronous dispatch failures after which a goal task is
/// parked `needs_human` instead of being retried again.
const DISPATCH_FAILURE_LIMIT: u32 = 3;

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
    /// D4 item 2: agent-selection policy. `None` ⇒ `FixedHierarchy` (dispatch to
    /// the task's stored `assigned_to`) — the pre-D4 default, byte-identical.
    /// `Some` ⇒ the configured policy may re-route a task to a different roster
    /// member before dispatch.
    policy: Option<Arc<dyn DispatchPolicy>>,
    /// Per-task in-flight bookkeeping. Held behind a mutex so `tick_once` can
    /// take `&self`; there is only ever one tick in flight, so contention is nil.
    inflight: Mutex<HashMap<String, InFlight>>,
    /// Per-task synchronous dispatch failures: (consecutive count, not-before).
    /// A task listed here is skipped by the candidate loop until `not-before`
    /// and is escalated once the count reaches [`DISPATCH_FAILURE_LIMIT`].
    /// Cleared the moment a round is picked up (`in_progress`) or escalated.
    dispatch_failures: Mutex<HashMap<String, (u32, DateTime<Utc>)>>,
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
    /// Retry counter for a progress push that failed transiently
    /// ([`crate::goal_notify::NotifyOutcome::SendFailed`]), keyed by
    /// `"<task_id>::<phase_key>"`. A phase is only marked `progress_seen`
    /// once delivered OR once this counter exhausts [`PROGRESS_PUSH_MAX_RETRIES`]
    /// — a transient network blip no longer looks identical to "delivered".
    progress_retry: Mutex<HashMap<String, u32>>,
    /// Retry counter for a `needs_human` approval push that failed
    /// transiently, keyed by task id. Mirrors `progress_retry`'s semantics.
    needs_human_retry: Mutex<HashMap<String, u32>>,
    /// Task ids whose kickoff approval push has been delivered (or
    /// permanently given up on) — separate from `kickoff` (which tracks the
    /// durable `ApprovalBroker` row so a second tick never re-requests it).
    /// A task can be `kickoff`-tracked but NOT yet `kickoff_notified` when its
    /// initial notification send failed; the next `Pending` poll retries it.
    kickoff_notified: Mutex<HashSet<String>>,
    /// Retry counter for a kickoff notification that failed transiently,
    /// keyed by task id.
    kickoff_retry: Mutex<HashMap<String, u32>>,
    /// A2: the `(state_hash, action)` visit graph — always-on, in-memory,
    /// scoped to this driver's lifetime (see `goal_loop/signals.rs` module
    /// docs for the persistence rationale).
    visit_graph: Arc<GoalVisitGraph>,
    /// A1/A2: task ids for which this round's `<state>` capture
    /// (self-reported hypotheses + visit-graph recording, see
    /// [`Self::capture_round_state`]) has already run while the task sits in
    /// `review` — pruned back to the live candidate set every tick so the
    /// NEXT time a task re-enters `review` (a later round) it captures
    /// again.
    state_capture_seen: Mutex<HashSet<String>>,
    running: Arc<AtomicBool>,
    /// WP-A9: A3 task-forward-model (design §4.1). `None` ⇒ the predict
    /// hook is a complete no-op — same as before this field existed
    /// (design §7.3's `enabled = false` default-off contract). Shared with
    /// the `DispatchEngine`'s settle hook via the same `Arc` so the
    /// in-memory statistical-bucket cache the two hooks read/write stays
    /// coherent (see the caller-side wiring notes in `handlers.rs`).
    forward_model: Option<Arc<TaskForwardModel>>,
    /// RFC-27: resolved effective edition concurrency limit for goal dispatch.
    /// `None` ⇒ the gate is a complete no-op (unlimited edition, or unwired in
    /// tests) — byte-identical to before this field existed. `Some(cap)` ⇒ a
    /// NEW admission first acquires a cross-process lease and defers when the
    /// class is at `cap`. Resolved at driver (re)spawn from the active edition
    /// (see `handlers.rs::respawn_goal_loop_driver`).
    concurrency_limit: Option<u32>,
    /// RFC-27: crash-recovery TTL (seconds) for concurrency leases, renewed for
    /// every held lease each tick.
    concurrency_ttl_secs: u64,
}

/// Retry cap for a transient ([`crate::goal_notify::NotifyOutcome::SendFailed`])
/// channel push before the driver gives up and marks the phase "handled" so
/// it does not retry forever. Applies uniformly to the progress board,
/// needs_human approval, and kickoff approval pushes.
const NOTIFY_PUSH_MAX_RETRIES: u32 = 3;

/// RFC-27: concurrency-gate class label for goal dispatch. Scopes the in-flight
/// lease budget so a future second consumer cannot starve the goal budget.
const CONCURRENCY_CLASS_GOAL: &str = "goal";
