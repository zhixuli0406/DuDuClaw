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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
use crate::task_store::{parse_depends_on, ActivityRow, TaskRow, TaskStore, CONTINUE_MESSAGE_PREFIX};

// `catch_unwind` for futures — same extension trait
// `subagent_prediction::spawn_record` uses (design R5: forward-model
// bookkeeping must never panic a hot path).
use futures_util::FutureExt as _;

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
const GOAL_KIND_RESEARCH_KEYWORDS: [&str; 10] =
    ["研究", "調查", "比較", "評估", "查詢", "research", "compare", "investigate", "分析", "評比"];
const GOAL_KIND_PLANNING_KEYWORDS: [&str; 8] =
    ["計畫", "規劃", "步驟", "方案", "plan", "roadmap", "strategy", "schedule"];
const GOAL_KIND_CODING_KEYWORDS: [&str; 10] = [
    "程式", "程式碼", "代碼", "寫程式", "bug", "測試", "重構", "code", "function", "implement",
];

/// L4: count DISTINCT topical signals in `lower` for one keyword list.
///
/// Two fixes over the old `list.iter().filter(|kw| lower.contains(*kw)).count()`:
///
/// 1. **Anchored matching** (project convention #2: no unanchored `contains`
///    for a routing/classification decision) — uses
///    [`duduclaw_core::word_contains_ci`] instead of raw `contains`, so e.g.
///    the OPS keyword `"send"` no longer matches inside `"sender"`.
/// 2. **Overlap de-duplication** — a single CJK phrase like `"寫程式碼"`
///    contains THREE of [`GOAL_KIND_CODING_KEYWORDS`] as substrings
///    (`"程式"`, `"程式碼"`, `"寫程式"`), all sharing the same characters.
///    Counting each independently inflates the score to 3 for what is one
///    coding-topic signal. This merges the first-occurrence byte span of
///    every matched keyword and counts each resulting overlap CLUSTER once,
///    not each keyword.
fn count_distinct_hits(lower: &str, list: &[&str]) -> usize {
    let mut spans: Vec<(usize, usize)> = list
        .iter()
        .filter(|kw| duduclaw_core::word_contains_ci(lower, kw))
        .filter_map(|kw| lower.find(kw).map(|start| (start, start + kw.len())))
        .collect();
    if spans.is_empty() {
        return 0;
    }
    spans.sort_unstable();
    let mut clusters = 0usize;
    let mut current_end: Option<usize> = None;
    for (start, end) in spans.drain(..) {
        match current_end {
            Some(ce) if start < ce => current_end = Some(ce.max(end)),
            _ => {
                clusters += 1;
                current_end = Some(end);
            }
        }
    }
    clusters
}

fn derive_goal_kind(text: &str) -> GoalKind {
    let difficulty = crate::dispatch_engine::classify_goal_difficulty(text);
    let lower = text.to_lowercase();

    let hits = |list: &[&str]| count_distinct_hits(&lower, list);

    let ops = hits(&GOAL_KIND_OPS_KEYWORDS);
    let research = hits(&GOAL_KIND_RESEARCH_KEYWORDS);
    let planning = hits(&GOAL_KIND_PLANNING_KEYWORDS);
    let coding = hits(&GOAL_KIND_CODING_KEYWORDS);

    // Ops/external signals dominate regardless of difficulty — an external
    // side-effect changes the expected tool classes (Net/Exec) and artifact
    // shape (ExternalEffect) more than length/complexity does.
    if ops > 0 && ops >= research && ops >= planning && ops >= coding {
        return GoalKind::OpsOrExternal;
    }
    if coding > 0 && coding >= research && coding >= planning {
        return match difficulty {
            crate::dispatch_engine::Difficulty::Simple => GoalKind::CodingSimple,
            crate::dispatch_engine::Difficulty::Complex => GoalKind::CodingComplex,
        };
    }
    if research > 0 && research >= planning {
        return GoalKind::ResearchOrQa;
    }
    if planning > 0 {
        return GoalKind::PlanningOrDoc;
    }
    // No topical keyword hit at all: fall back on the difficulty split alone
    // (coding is the modal goal-loop workload — Complex without any other
    // signal still gets the coarse Coding bucket rather than Unknown, which
    // would leave GoalKind's statistics permanently unbucketed for the
    // common case).
    match difficulty {
        crate::dispatch_engine::Difficulty::Simple => GoalKind::Unknown,
        crate::dispatch_engine::Difficulty::Complex => GoalKind::CodingComplex,
    }
}

/// Tuning for the goal loop driver. Read from `config.toml [goal_loop]`.
///
/// `#[serde(default)]` at the container level means every field falls back to
/// [`GoalLoopConfig::default`] when absent, so a missing or partial section is
/// always valid.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GoalLoopConfig {
    /// Hard cap on total dispatches per task **for Complex goals** (independent
    /// of the judge's `max_retries`; both apply, stricter wins). Exceed ⇒
    /// `needs_human`. Iterative Kanban lowered this 8→5: under critique-revise
    /// feedback two rounds capture 76-95% of the gain (arXiv:2604.10508) and
    /// rounds past ~4 trend to zero (Self-Refine 2303.17651), so 5 is the
    /// evidence-backed hard ceiling. Override in `config.toml [goal_loop]`.
    pub iteration_cap: u32,
    /// D4 item 3: iteration cap for **Simple goals** (MaAS dynamic depth — a
    /// simple goal that has not converged in a few tries is unlikely to, so it
    /// escalates sooner and cheaper). The per-task effective cap is chosen by
    /// [`crate::dispatch_engine::classify_goal_difficulty`].
    pub iteration_cap_simple: u32,
    /// Iterative Kanban soft cap: once a task's `revision_round` reaches this,
    /// it is flagged `diminishing` (amber "報酬遞減" badge) but NOT blocked —
    /// only `iteration_cap` blocks. Default 3 (2604.10508: gains flatten after
    /// round 2-3). Passed to `reject_review` via the dispatch engine.
    pub soft_cap: i64,
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
    /// H22 (workbuddy-codebuddy §2.5): after this many minutes with no
    /// observable progress signal, an already-picked-up (`in_progress`) goal
    /// task gets ONE "已執行 X 分鐘未回報進度" notice — Activity Feed plus the
    /// launching conversation — at most once per round.
    ///
    /// Strictly a report. It never re-dispatches, escalates, or cancels
    /// anything; `stalled_secs` / `iteration_cap` / `wall_clock_hours` remain
    /// the only guards that act. `0` (or any negative value) disables it
    /// entirely, and the disabled path costs zero queries.
    pub progress_report_minutes: i64,
    /// H6 (WP-B) / WP-E (2026-08 P1 rollout): `"auto"` or `"pause"`
    /// (**default since WP-E**). Read as a raw string (not a typed enum) so
    /// an unrecognized value degrades to the safe default instead of
    /// failing `GoalLoopConfig` deserialization for the whole `[goal_loop]`
    /// section — same lenient-string convention
    /// [`AutonomyLevel::from_toml_str`] uses elsewhere in this file. Resolve
    /// via [`GoalLoopConfig::resume_on_restart`].
    pub resume_on_restart: String,
    /// H10: whether `capture_round_state` computes and injects the tool-call
    /// streak advisory (`goal_tool_streak.rs`, deepseek-harness §2.16
    /// `repeat-tool-reminder`) into the next round's `<state>` block.
    /// Default `true` — this is purely advisory text (it can never change
    /// what the agent is allowed to do, only nudge what it is told), so
    /// unlike most goal-loop gates the safe default is ON, not off. Set
    /// `false` in `config.toml [goal_loop]` to silence it entirely.
    pub tool_streak_advisory: bool,
}

impl Default for GoalLoopConfig {
    fn default() -> Self {
        Self {
            iteration_cap: 5,
            iteration_cap_simple: 3,
            soft_cap: 3,
            wall_clock_hours: 24,
            max_concurrent: 3,
            tick_secs: 30,
            stalled_secs: 600,
            // H22: 10 minutes of silence is where a human starts wondering
            // whether anything is happening at all. Deliberately the same
            // intuition as `stalled_secs` (600s) about how long "quiet" is
            // tolerable — but this one only reports, on an already-picked-up
            // task, where `stalled_secs` re-dispatches an unclaimed one.
            progress_report_minutes: 10,
            // WP-E (2026-08 P1 rollout, user-approved spec change): default
            // flipped from "auto" to "pause". H6 shipped opt-in-only first;
            // this is the deliberate follow-up so an unattended gateway
            // restart/crash-recovery no longer silently resumes driving a
            // goal nobody re-confirmed is still safe. Set back to "auto" in
            // `config.toml [goal_loop]` (or via the dashboard's Automation
            // tab) to restore the pre-WP-E behavior.
            resume_on_restart: "pause".to_string(),
            // H10: on by default — advisory-only, never behavior-changing.
            tool_streak_advisory: true,
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

    /// H6: the parsed [`ResumeOnRestart`] policy. Unrecognized / empty /
    /// whitespace-only values fall back to [`ResumeOnRestart::Auto`] — the
    /// pre-H6, byte-identical-behavior default.
    pub fn resume_on_restart(&self) -> ResumeOnRestart {
        ResumeOnRestart::from_str_lenient(&self.resume_on_restart)
    }
}

/// H6: whether the goal loop resumes in-flight goal tasks automatically
/// after a gateway process restart, or requires human confirmation first.
///
/// Two independent harnesses (deepseek-harness's Ralph loop, grok-build's
/// `goal_tracker.rs`) converged on the same conclusion: a durable
/// autonomous loop must never resurrect itself after a process restart — an
/// unattended process crash/redeploy must not silently resume driving a
/// goal that a human has not re-confirmed is still safe to continue. See
/// H6 in `commercial/docs/DESIGN-harness-borrowings-2026-08.md`.
///
/// H6 shipped with [`GoalLoopConfig`]'s default string set to `"auto"`
/// (byte-identical to pre-H6 behavior) pending the P1 rollout decision in
/// that design doc. WP-E (2026-08, user-approved) is that rollout: the
/// config default is now `"pause"` — see [`GoalLoopConfig::default`]. This
/// enum's own `#[default]` stays [`ResumeOnRestart::Auto`] deliberately: it
/// is the fail-safe fallback [`ResumeOnRestart::from_str_lenient`] returns
/// for an unrecognized/malformed config string, which must never
/// double-negative into the *stricter* behavior on a typo — that is a
/// separate concept from "what a fresh install ships with".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResumeOnRestart {
    /// The driver picks up any non-terminal goal_mode task exactly as if
    /// the process had never stopped. Pre-H6 behavior; no longer the
    /// `GoalLoopConfig` default since WP-E, but still the safe fallback for
    /// an unparseable `resume_on_restart` string (see the enum doc above).
    #[default]
    Auto,
    /// At gateway boot, every non-terminal goal_mode task is escalated to
    /// `needs_human` (reason `gateway_restart`) instead of being silently
    /// resumed. See [`pause_inflight_on_restart`].
    Pause,
}

impl ResumeOnRestart {
    /// Unknown / empty / whitespace-only ⇒ [`ResumeOnRestart::Auto`] (the
    /// safe, behavior-preserving default) — a typo in config must never
    /// silently switch to the OTHER mode's semantics in either direction.
    fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "pause" => ResumeOnRestart::Pause,
            _ => ResumeOnRestart::Auto,
        }
    }
}

/// H6: boot-time reconciliation for `resume_on_restart = "pause"`. Scans
/// every genuinely in-flight (`revising` / `in_progress` / `review` /
/// `blocked`) `goal_mode` task and escalates it to `needs_human`
/// (reason `gateway_restart`), reusing [`GoalLoopDriver::escalate`]'s
/// well-tested path (grant revocation, activity post, visit-graph /
/// state-capture cleanup) via a throwaway driver instance — safe because at
/// boot time no live in-memory in-flight tracking exists yet for any task,
/// so an empty `inflight` map is equivalent to a freshly-started driver's
/// real state. The existing `needs_human` channel push then happens
/// naturally on the driver's own first tick via
/// [`GoalLoopDriver::reconcile_needs_human`] — no separate notify path is
/// duplicated here.
///
/// No-op (returns `0`) when `resume_on_restart` resolves to `Auto` —
/// byte-identical to pre-H6 behavior, but note this is no longer the
/// `GoalLoopConfig` default since WP-E (see [`ResumeOnRestart`]).
/// Deliberately called ONLY from the gateway boot path, never from a
/// hot-reload respawn — see the sole caller's doc comment
/// (`MethodHandler::pause_inflight_goal_tasks_on_restart` in `handlers.rs`)
/// for why conflating the two would be wrong.
pub async fn pause_inflight_on_restart(
    store: Arc<TaskStore>,
    queue: Arc<MessageQueue>,
    home_dir: &Path,
) -> usize {
    let cfg = GoalLoopConfig::from_home(home_dir);
    if cfg.resume_on_restart() != ResumeOnRestart::Pause {
        return 0;
    }
    let driver = GoalLoopDriver::new(store.clone(), queue, cfg).with_home_dir(home_dir.to_path_buf());
    // Queue states a goal task holds BEFORE its first dispatch (`todo`,
    // `pending`) are deliberately NOT escalated: the user confirmed the goal
    // at creation and no round has run yet, so dispatching it after boot is
    // starting the confirmed work — not silently resuming an interrupted,
    // unconfirmed run. The documented contract (CHANGELOG / goal-loop guide)
    // promises pausing tasks "still running"; live verification 2026-08-15
    // caught the earlier all-non-terminal scan pausing queued tasks too.
    const INFLIGHT_STATUSES: &[&str] = &["revising", "in_progress", "review", "blocked"];
    let mut paused = 0usize;
    for status in INFLIGHT_STATUSES {
        let tasks = match store.tasks_in_status(status).await {
            Ok(t) => t,
            Err(e) => {
                warn!(%status, error = %e, "goal loop: resume_on_restart scan failed for this status (continuing)");
                continue;
            }
        };
        for t in tasks {
            if !t.goal_mode {
                continue;
            }
            let mut dummy_inflight: HashMap<String, InFlight> = HashMap::new();
            if let Err(e) = driver
                .escalate(
                    &mut dummy_inflight,
                    &t,
                    "gateway_restart",
                    crate::pause_reason::PauseReason::Restart,
                )
                .await
            {
                warn!(task = %t.id, error = %e, "goal loop: resume_on_restart escalate failed for this task (continuing)");
                continue;
            }
            paused += 1;
        }
    }
    if paused > 0 {
        info!(
            paused,
            "goal loop: resume_on_restart=pause escalated in-flight goal tasks to needs_human at boot"
        );
    }
    paused
}

/// Iterative Kanban default `review` WIP limit. The board flags the review
/// column amber and shows a Little's-Law wait estimate once the queue depth
/// exceeds this. Override in `config.toml [task_board] review_wip_limit`.
pub const DEFAULT_REVIEW_WIP_LIMIT: i64 = 10;

/// Read `[task_board] review_wip_limit` from `<home>/config.toml`. Absent /
/// malformed / non-positive ⇒ [`DEFAULT_REVIEW_WIP_LIMIT`] (a WIP limit ≤ 0 is
/// meaningless, so it falls back rather than disabling the guard silently).
pub fn review_wip_limit(home_dir: &Path) -> i64 {
    let path = home_dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return DEFAULT_REVIEW_WIP_LIMIT;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return DEFAULT_REVIEW_WIP_LIMIT;
    };
    table
        .get("task_board")
        .and_then(|s| s.get("review_wip_limit"))
        .and_then(|v| v.as_integer())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_REVIEW_WIP_LIMIT)
}

// ── Goal assignment form v2 (design-market-belief-loop-2026-08.md §6,
// G1) ────────────────────────────────────────────────────────────

/// Built-in baseline risk-boundary text (design §6 G1): the five-line default
/// applied to every goal-mode task whose assign form left `risk_boundary`
/// blank. Used both as the deployment default and as the fail-open fallback
/// when `config.toml [goal_defaults] baseline_boundary` is absent, malformed,
/// or unreadable — a bad/missing config must never leave a goal task with NO
/// boundary text injected.
pub const DEFAULT_BASELINE_BOUNDARY: &str = "\
- 遵循當地法規。\n\
- 資安紅線：不得外洩秘密或憑證。\n\
- 不得繞過或說服自己繞過任何硬性風控與平台護欄。\n\
- 金流與不可逆動作須經人審。\n\
- 對外公開發言須經人審。";

/// Read `[goal_defaults] baseline_boundary` from `<home>/config.toml`.
/// Absent / malformed / unreadable / blank ⇒ [`DEFAULT_BASELINE_BOUNDARY`]
/// (fail-open — same "parsed in isolation, defaults on any failure" pattern
/// as [`GoalLoopConfig::from_home`] and [`review_wip_limit`], so a broken
/// unrelated config section can never take this down and a goal task is
/// never dispatched with zero boundary text). Deployment-customizable so an
/// operator can tailor the default to local regulatory / industry context.
pub fn baseline_boundary(home_dir: &Path) -> String {
    let path = home_dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return DEFAULT_BASELINE_BOUNDARY.to_string();
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return DEFAULT_BASELINE_BOUNDARY.to_string();
    };
    table
        .get("goal_defaults")
        .and_then(|s| s.get("baseline_boundary"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_BASELINE_BOUNDARY.to_string())
}

/// Resolve the effective risk-boundary text for a task: its own explicit
/// `risk_boundary` when non-blank, else the deployment baseline. Shared by
/// both G2 injection points (the goal-loop work message and the MAV judge
/// prompt) so the two never drift out of sync.
pub fn effective_risk_boundary(task_risk_boundary: Option<&str>, home_dir: &Path) -> String {
    task_risk_boundary
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| baseline_boundary(home_dir))
}

/// G3: which deadline actually fired — the escalation message tells a human
/// whether it was the global wall-clock budget or the goal's own explicit
/// `deadline_at` override, instead of one generic "goal-loop deadline" for
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeadlineHit {
    /// The global `[goal_loop] wall_clock_hours` budget (from `created_at`).
    WallClock,
    /// The per-task `deadline_at` override (design §6 G3).
    TaskDeadline,
}

/// G3: resolves whether `now` has passed either the global wall-clock budget
/// (`created_at + wall_clock_hours`) or an explicit per-task `deadline_at` —
/// whichever is EARLIER wins, i.e. `deadline_at` can only *tighten* the
/// effective deadline, never loosen it past the global budget (design §6:
/// "deadline 覆蓋全域 wall-clock（取較早者）"). Pure and unit-testable without
/// constructing a [`GoalLoopDriver`]. Unparseable timestamps degrade to "does
/// not apply" for that half of the check (fail-open on the deadline only —
/// same contract the pre-G3 wall-clock-only check had; the iteration cap
/// still bounds the loop regardless).
pub(crate) fn resolve_deadline_hit(
    created_at: &str,
    deadline_at: Option<&str>,
    wall_clock_hours: i64,
    now: DateTime<Utc>,
) -> Option<DeadlineHit> {
    let wall_clock_deadline = DateTime::parse_from_rfc3339(created_at)
        .ok()
        .map(|c| c.with_timezone(&Utc) + ChronoDuration::hours(wall_clock_hours));
    let task_deadline = deadline_at
        .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
        .map(|d| d.with_timezone(&Utc));

    match (wall_clock_deadline, task_deadline) {
        (Some(wc), Some(td)) => {
            let effective = wc.min(td);
            if now < effective {
                None
            } else if td <= wc {
                Some(DeadlineHit::TaskDeadline)
            } else {
                Some(DeadlineHit::WallClock)
            }
        }
        (Some(wc), None) => (now >= wc).then_some(DeadlineHit::WallClock),
        (None, Some(td)) => (now >= td).then_some(DeadlineHit::TaskDeadline),
        (None, None) => None,
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
    /// RFC-27: the edition concurrency-gate lease this task holds while
    /// in-flight. `None` when the gate does not apply (unlimited edition, or
    /// the gate is unwired in tests) — carried forward across re-dispatch,
    /// renewed each tick, released when the task reaches a terminal state.
    lease: Option<duduclaw_core::ConcurrencyLease>,
    /// H22: the round (`iter`) for which the no-progress notice has already
    /// been emitted, so a long-running task reports at most once per round
    /// instead of once per tick. Reset to `None` implicitly on every
    /// re-dispatch, since dispatch rebuilds the whole [`InFlight`] entry.
    progress_reported_round: Option<u32>,
}

/// H22: pure predicate — how many whole minutes a task has gone without an
/// observable progress signal, when that exceeds the configured threshold.
///
/// `Some(elapsed_minutes)` ⇒ report; `None` ⇒ stay quiet. Disabled
/// (`threshold_minutes <= 0`) and clocks that run backwards (a `last_signal`
/// in the future — NTP correction, a hand-edited row) both return `None`:
/// the notice is a courtesy, so every ambiguous case degrades to silence
/// rather than to a wrong number in a user's chat.
pub(crate) fn no_progress_minutes(
    last_signal: DateTime<Utc>,
    now: DateTime<Utc>,
    threshold_minutes: i64,
) -> Option<i64> {
    if threshold_minutes <= 0 {
        return None;
    }
    let elapsed = (now - last_signal).num_minutes();
    (elapsed >= threshold_minutes).then_some(elapsed)
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
    /// D4 item 2: agent-selection policy. `None` ⇒ `FixedHierarchy` (dispatch to
    /// the task's stored `assigned_to`) — the pre-D4 default, byte-identical.
    /// `Some` ⇒ the configured policy may re-route a task to a different roster
    /// member before dispatch.
    policy: Option<Arc<dyn DispatchPolicy>>,
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
    /// scoped to this driver's lifetime (see `goal_visit_graph.rs` module
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

impl GoalLoopDriver {
    pub fn new(store: Arc<TaskStore>, queue: Arc<MessageQueue>, config: GoalLoopConfig) -> Self {
        Self {
            store,
            queue,
            config,
            home_dir: PathBuf::from("."),
            broker: None,
            policy: None,
            inflight: Mutex::new(HashMap::new()),
            kickoff: Mutex::new(HashMap::new()),
            notified_needs_human: Mutex::new(HashSet::new()),
            operator_skipped: Mutex::new(HashSet::new()),
            progress_seen: Mutex::new(HashMap::new()),
            progress_retry: Mutex::new(HashMap::new()),
            needs_human_retry: Mutex::new(HashMap::new()),
            kickoff_notified: Mutex::new(HashSet::new()),
            kickoff_retry: Mutex::new(HashMap::new()),
            visit_graph: Arc::new(GoalVisitGraph::new()),
            state_capture_seen: Mutex::new(HashSet::new()),
            running: Arc::new(AtomicBool::new(false)),
            forward_model: None,
            // RFC-27: gate disabled by default (None) — production wires the
            // resolved edition limit via `with_concurrency_limit`. Tests and the
            // 3-arg constructor stay on the untouched, unlimited path.
            concurrency_limit: None,
            concurrency_ttl_secs: duduclaw_core::ConcurrencyGateConfig::default()
                .concurrency_lease_ttl_secs,
        }
    }

    /// Set the DuDuClaw home dir (per-agent autonomy + channel push).
    pub fn with_home_dir(mut self, home_dir: PathBuf) -> Self {
        self.home_dir = home_dir;
        self
    }

    /// RFC-27: wire the resolved edition concurrency limit + lease TTL. `limit`
    /// is `None` for an unlimited edition (Enterprise, or a Personal cap of 0),
    /// in which case the gate stays a no-op. Called from
    /// `handlers.rs::respawn_goal_loop_driver` with the edition resolved via the
    /// existing `resolve_edition_profile()` chain.
    pub fn with_concurrency_limit(mut self, limit: Option<u32>, ttl_secs: u64) -> Self {
        self.concurrency_limit = limit;
        self.concurrency_ttl_secs = ttl_secs;
        self
    }

    /// WP-A9: wire the A3 task-forward-model predict hook. Omit (default
    /// `None`) to keep the hook a no-op — the `[task_forward_model] enabled`
    /// gate (design §7.3) is enforced by the caller deciding whether to
    /// construct a `TaskForwardModel` at all, not by a flag read here.
    pub fn with_forward_model(mut self, forward_model: Arc<TaskForwardModel>) -> Self {
        self.forward_model = Some(forward_model);
        self
    }

    /// Wire the HITL broker used for the Collaborator/Consultant kickoff gate.
    pub fn with_broker(mut self, broker: Arc<ApprovalBroker>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// Wire a non-default [`DispatchPolicy`] (D4 item 2). Omit for the default
    /// `FixedHierarchy` behavior (dispatch to `assigned_to` unchanged).
    pub fn with_policy(mut self, policy: Arc<dyn DispatchPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    /// The effective iteration cap for a task, chosen by its difficulty (MaAS
    /// dynamic depth, D4 item 3): Simple goals get the cheaper `iteration_cap_simple`.
    fn iteration_cap_for(&self, task: &TaskRow) -> u32 {
        let text = format!(
            "{}\n{}\n{}",
            task.title,
            task.description,
            task.acceptance_criteria.as_deref().unwrap_or("")
        );
        match crate::dispatch_engine::classify_goal_difficulty(&text) {
            crate::dispatch_engine::Difficulty::Simple => self.config.iteration_cap_simple,
            crate::dispatch_engine::Difficulty::Complex => self.config.iteration_cap,
        }
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
                        self.push_progress(t, "review", crate::goal_notify::GoalProgress::Reviewing)
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
            // (see `goal_state.rs::StateBlock::hash_input`), so "state
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
            if self.visit_graph.has_repeated_action(&task.id, &state_hash).await {
                state_block.loop_warning = Some(
                    "此狀態下已重複嘗試相同做法且失敗,請勿重複,改用不同做法".to_string(),
                );
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
                let rule_induction_enabled = crate::prediction::task_forward_store::
                    TaskForwardModelConfig::from_home(&self.home_dir)
                    .rule_induction;
                if rule_induction_enabled {
                    let round = (task.revision_round as u32).saturating_add(1);
                    let task_id = task.id.clone();
                    let agent_id = task.assigned_to.clone();
                    let db_path = self.home_dir.join("memory.db");

                    let inject = async move {
                        let engine = duduclaw_memory::SqliteMemoryEngine::new(&db_path).ok()?;
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
                    let section =
                        crate::prediction::belief::render_calibration_section(&stats)?;
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
            self.enqueue_work(task, next_iter, &state_text).await?;
            // A2: commit this round's state as the latest dispatched state
            // for the unchanged-streak comparison the NEXT rejection
            // re-dispatch will make (see the peek/commit split in the guard
            // above — commit happens once the dispatch decision is final,
            // mirroring the pre-A2 `last_feedback` commit timing).
            let committed_streak = self.visit_graph.commit_dispatch(&task.id, &state_hash).await;
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
            if let Err(e) = self
                .store
                .record_iteration_dispatch_with_state(
                    &task.id,
                    task.revision_round + 1,
                    &now.to_rfc3339(),
                    Some(&state_hash),
                    Some(committed_streak as i64),
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

    /// RFC-27: release the edition concurrency lease a terminal task held.
    /// No-op when the gate did not apply (unguarded / `None`). Best-effort — the
    /// lease TTL reclaims the slot even if the file write fails.
    fn release_lease(&self, entry: &InFlight) {
        if let Some(lease) = &entry.lease {
            duduclaw_core::concurrency_release(&self.home_dir, lease);
        }
    }

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
    /// reason (see `goal_budget_best_round.rs`). Every other pause class
    /// (`NoProgress`, `BlockedNeedsDecision`) passes `reason` through
    /// byte-identical to before this feature existed. Best-effort: a failed
    /// iteration-history read degrades to the bare `reason`, never blocks
    /// the escalation itself.
    async fn escalate(
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
        self.store
            .mark_needs_human_with_pause(&task.id, &effective_reason, pause)
            .await?;
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
        self.needs_human_retry.lock().await.retain(|id, _| live.contains(id));

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
                // Observer resolves the task out of needs_human synchronously
                // above (or logs+leaves it for a later retry on store error);
                // either way there is no channel-push retry state to track.
                notified.insert(task.id.clone());
            } else {
                // Operator/Collaborator/Consultant/Approver: push retry/done/abort
                // buttons to the agent control channel, and mirror a plain
                // heads-up to the goal's source conversation. A transient
                // SendFailed is retried (bounded) on a later tick instead of
                // being marked `notified` immediately — previously ANY
                // outcome (including a network blip) inserted into `notified`
                // unconditionally, so a failed push was never retried and the
                // human never learned the task was stuck.
                use crate::goal_notify::NotifyOutcome;
                let outcome = crate::goal_notify::notify_goal_needs_human(&self.home_dir, task).await;
                match outcome {
                    // `Deferred` cannot occur here in practice — needs_human
                    // is L3 and quiet hours never hold it back — but it is
                    // handled explicitly rather than by a wildcard so that a
                    // future re-classification is a compile-time decision, not
                    // a silently-swallowed notification.
                    NotifyOutcome::Sent | NotifyOutcome::NoTarget | NotifyOutcome::Deferred => {
                        self.push_progress(
                            task, "needs_human", crate::goal_notify::GoalProgress::NeedsHuman,
                        )
                        .await;
                        self.needs_human_retry.lock().await.remove(&task.id);
                        let sent = outcome != NotifyOutcome::NoTarget;
                        self.post_activity(
                            "goal_loop.needs_human_notified",
                            &task.assigned_to,
                            Some(&task.id),
                            &format!(
                                "已推播需人工審批 — {}(推播{})",
                                task.title,
                                if sent { "成功" } else { "無通知目標(設定缺漏)" }
                            ),
                        )
                        .await;
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
                            self.push_progress(
                                task, "needs_human", crate::goal_notify::GoalProgress::NeedsHuman,
                            )
                            .await;
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
                    task.title,
                    self.iteration_cap_for(task)
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
                // The ApprovalBroker row above is already durably created —
                // a failed notification here must NOT be re-requested (that
                // would spam duplicate approvals); only the notification
                // itself is retried, via the Pending arm below on later ticks.
                self.notify_kickoff_with_retry(task, id.as_str(), &summary).await;
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
                ApprovalStatus::Pending => {
                    drop(kickoff);
                    // Retry a previously-failed notification (bounded) — the
                    // approval already exists, so this only re-sends the push.
                    if !self.kickoff_notified.lock().await.contains(&task.id) {
                        let summary = format!(
                            "目標:{} — 最多 {} 輪自主嘗試",
                            task.title,
                            self.iteration_cap_for(task)
                        );
                        self.notify_kickoff_with_retry(task, id.as_str(), &summary).await;
                    }
                    Ok(KickoffGate::Waiting)
                }
                // Denied / Expired (TTL = deny, fail-closed) ⇒ abort the goal.
                other => {
                    kickoff.remove(&task.id);
                    let reason = format!("kickoff {} — 目標未啟動", other.as_str());
                    if let Err(e) = self.store.cancel_task(&task.id, &reason).await {
                        warn!(task = %task.id, error = %e, "goal loop: kickoff abort cancel failed");
                    }
                    // WP3 (PORTICO): task abandoned at kickoff → revoke any grants.
                    self.revoke_task_grants(&task.id, crate::capability_grants::REVOKE_REASON_PHASE_END)
                        .await;
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

    /// Push the kickoff approval, retrying a transient send failure (bounded)
    /// via [`kickoff_gate`]'s `Pending` poll branch on later ticks. The
    /// underlying `ApprovalBroker` row is already durably created by the
    /// caller — this only manages the notification's own delivery state.
    /// `NoTarget` / exhausted retries are treated as "handled" so the loop
    /// does not attempt the push forever.
    async fn notify_kickoff_with_retry(&self, task: &TaskRow, approval_id: &str, summary: &str) {
        use crate::goal_notify::NotifyOutcome;
        let outcome =
            crate::goal_notify::notify_goal_kickoff(&self.home_dir, &task.assigned_to, approval_id, summary)
                .await;
        match outcome {
            // `Deferred` = the kickoff card is queued behind quiet hours (it
            // is L2). Handled, not lost: retrying would queue a duplicate.
            NotifyOutcome::Sent | NotifyOutcome::Deferred => {
                self.kickoff_notified.lock().await.insert(task.id.clone());
                self.kickoff_retry.lock().await.remove(&task.id);
            }
            NotifyOutcome::NoTarget => {
                warn!(task = %task.id, "goal loop: kickoff push has no notify target");
                self.kickoff_notified.lock().await.insert(task.id.clone());
                self.kickoff_retry.lock().await.remove(&task.id);
            }
            NotifyOutcome::SendFailed => {
                let mut retries = self.kickoff_retry.lock().await;
                let count = retries.entry(task.id.clone()).or_insert(0);
                *count += 1;
                if *count >= NOTIFY_PUSH_MAX_RETRIES {
                    warn!(task = %task.id, attempts = *count,
                          "goal loop: kickoff push failed after max retries, giving up");
                    retries.remove(&task.id);
                    drop(retries);
                    self.kickoff_notified.lock().await.insert(task.id.clone());
                } else {
                    warn!(task = %task.id, attempt = *count,
                          "goal loop: kickoff push failed, will retry next tick");
                }
            }
        }
    }

    /// Whether a real (non-test) home dir is wired. The driver defaults
    /// `home_dir` to `"."` in tests; touching the shared `approvals.db` under
    /// that sentinel would pollute the working tree, so all capability-grant
    /// side effects are gated on this.
    fn has_real_home(&self) -> bool {
        self.home_dir != Path::new(".")
    }

    /// WP3 (PORTICO): revoke every capability grant bound to a task when the
    /// goal loop abandons it (kickoff denial → `cancel_task`). Best-effort;
    /// a store error just lets the grants die at their hard TTL.
    async fn revoke_task_grants(&self, task_id: &str, reason: &str) {
        if !self.has_real_home() {
            return;
        }
        match crate::capability_grants::CapabilityGrantStore::open(&self.home_dir) {
            Ok(store) => {
                if let Err(e) = store.revoke_for_task(task_id, reason).await {
                    warn!(task = %task_id, error = %e, "goal loop: capability grant revoke failed");
                }
            }
            Err(e) => {
                warn!(task = %task_id, error = %e, "goal loop: capability grant store open failed for revoke")
            }
        }
    }

    /// WP3 (PORTICO): when a kickoff approval clears, atomically mint the
    /// task-scoped grants the task declared via `tags` entries of the form
    /// `grant:<tool>`. Idempotent per (task, tool): a grant already bound to
    /// THIS task for that tool is not re-minted (so a dispatch deferred by the
    /// concurrency cap, which re-enters this path next tick, does not stack
    /// duplicate rows). Best-effort + fail-safe: a store error is logged and
    /// the agent falls back to `capability_request`.
    async fn grant_kickoff_tools(&self, task: &TaskRow) {
        if !self.has_real_home() {
            return;
        }
        let tools: Vec<String> = task
            .tags
            .split(',')
            .filter_map(|t| t.trim().strip_prefix("grant:"))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if tools.is_empty() {
            return;
        }
        let store = match crate::capability_grants::CapabilityGrantStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                warn!(task = %task.id, error = %e, "goal loop: grant store open failed for kickoff grants");
                return;
            }
        };
        let agent_dir = self.home_dir.join("agents").join(&task.assigned_to);
        let ttl = crate::capability_grants::grant_ttl_secs(&agent_dir);
        // Existing grants already bound to THIS task (for per-task idempotency).
        let existing = store.active_grants(&task.assigned_to).await.unwrap_or_default();
        for tool in tools {
            let already = existing.iter().any(|g| {
                g.task_id.as_deref() == Some(task.id.as_str())
                    && crate::capability_grants::tool_token_matches(&g.tool, &tool)
            });
            if already {
                continue;
            }
            match store
                .grant(
                    &task.assigned_to,
                    Some(&task.id),
                    &tool,
                    crate::capability_grants::GRANTED_BY_KICKOFF,
                    ttl,
                )
                .await
            {
                Ok(grant_id) => {
                    duduclaw_security::audit::append_tool_call_with_extras(
                        &self.home_dir,
                        &task.assigned_to,
                        "capability_request",
                        &format!("kickoff grant {tool}"),
                        true,
                        &[
                            ("grant_id", json!(grant_id)),
                            ("granted_tool", json!(tool)),
                            ("task_id", json!(task.id)),
                            (
                                "granted_by",
                                json!(crate::capability_grants::GRANTED_BY_KICKOFF),
                            ),
                        ],
                    );
                    info!(task = %task.id, %tool, "goal loop: kickoff-approved capability grant minted");
                }
                Err(e) => {
                    warn!(task = %task.id, %tool, error = %e, "goal loop: kickoff grant write failed")
                }
            }
        }
    }

    /// Enqueue a work message for `task` onto `message_queue.db` — the same rail
    /// the heartbeat's task-board pull uses, so the existing dispatcher routes it
    /// to the agent unchanged. Carries `judge_feedback` (if any) so a rejected
    /// task is retried *with* the reviewer's feedback, and (I-1c) an approved
    /// plan-first plan so the very first round after approval executes it.
    async fn enqueue_work(&self, task: &TaskRow, iter: u32, state_text: &str) -> Result<(), String> {
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

        let msg = QueueMessage {
            id: uuid::Uuid::new_v4().to_string(),
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
        };
        let result = self.queue.enqueue(&msg).await;
        // I-1c: the plan has now been injected into this round's payload —
        // consume it so it is not re-injected on every later round. Cleared
        // only after a successful enqueue (an enqueue failure leaves it in
        // place, so the next tick's retry still carries the plan). A failed
        // clear is logged and otherwise harmless: the plan is simply
        // re-injected next dispatch, which repeats guidance rather than
        // losing anything.
        if result.is_ok() && task.plan_pending.is_some() {
            if let Err(e) = self.store.clear_plan_pending(&task.id).await {
                warn!(
                    task = %task.id,
                    error = %e,
                    "goal loop: failed to clear plan_pending after injecting it — will re-inject next dispatch (harmless)"
                );
            }
        }
        result
    }

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
    async fn push_progress(
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
        let outcome = crate::goal_notify::notify_goal_progress(&self.home_dir, task, progress).await;
        if outcome.is_final() {
            self.progress_seen.lock().await.insert(task.id.clone(), phase_key.to_string());
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
            self.progress_seen.lock().await.insert(task.id.clone(), phase_key.to_string());
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
    async fn maybe_report_no_progress(
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
    async fn clear_progress_retries(&self, task_id: &str) {
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
    async fn capture_round_state(&self, task: &TaskRow) {
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
            let agent_id = task.claimed_by.clone().unwrap_or_else(|| task.assigned_to.clone());
            let since = task.claimed_at.clone().unwrap_or_else(|| task.created_at.clone());
            let until = Utc::now().to_rfc3339();
            let result_text = task.result_summary.clone().unwrap_or_default();
            let digest = crate::goal_visit_graph::action_digest(
                &self.home_dir,
                &agent_id,
                &since,
                &until,
                &result_text,
            );
            self.visit_graph.record_round(&task.id, &state_hash, &digest).await;

            // ── H10: tool-call streak advisory (deepseek-harness §2.16
            //    repeat-tool-reminder) — same `(agent_id, since, until)`
            //    window as the A2 action digest above, read via the shared
            //    `tool_activity` evidence reader. Config-gated (default on,
            //    advisory-only) — see `GoalLoopConfig::tool_streak_advisory`.
            if self.config.tool_streak_advisory {
                if let Some(hit) =
                    crate::goal_tool_streak::detect_tool_streak(&self.home_dir, &agent_id, &since, &until)
                {
                    self.record_tool_streak_hint(task, &hit).await;
                }
            }
        }

        if let Some(result_text) = task.result_summary.as_deref() {
            match goal_state::parse_state_update(result_text) {
                Some(hyps) => {
                    // M7: `confirmed_facts` is written independently by
                    // `dispatch_engine.rs`'s settle path (see
                    // `goal_state.rs`'s "Honesty note"). The previous
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
    async fn record_tool_streak_hint(&self, task: &TaskRow, hit: &crate::goal_tool_streak::StreakHit) {
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

    // ── R5: `[capabilities] autonomy_level` direction, pinned ────────────
    //
    // absent / malformed / wrong-typed / unrecognised ⇒ `Approver` — the
    // conservative level, NEVER the most-autonomous one. Two separate
    // fallbacks point the same way on purpose: the missing-key fallback here
    // and `from_toml_str`'s unknown-string fallback. The value stays a raw
    // String on the typed section so the second one keeps running instead of
    // a strict serde enum making a typo fatal to the whole `AgentConfig`
    // (which would drop the agent from the registry entirely).

    fn home_with_agent(agent_id: &str, body: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join(agent_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), body).unwrap();
        home
    }

    #[test]
    fn default_direction_autonomy_level_defaults_to_approver_never_operator() {
        for body in [
            "",                                            // empty file
            "[capabilities]\n",                            // section, no key
            "[capabilities]\ncomputer_use = true\n",       // sibling only
            "[capabilities]\nautonomy_level = \"oprator\"\n", // typo
            "[capabilities]\nautonomy_level = \"\"\n",     // blank
            "[capabilities]\nautonomy_level = 3\n",        // wrong type
            "capabilities = \"scalar\"\n",                 // wrong-typed section
            "not toml [[[",                                // malformed file
        ] {
            let home = home_with_agent("a", body);
            assert_eq!(
                AutonomyLevel::for_agent(home.path(), "a"),
                AutonomyLevel::Approver,
                "for {body:?}"
            );
        }

        // Missing agent directory entirely — same direction.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            AutonomyLevel::for_agent(empty.path(), "nope"),
            AutonomyLevel::Approver
        );
    }

    #[test]
    fn default_direction_autonomy_level_recognised_values_still_apply() {
        for (raw, want) in [
            ("operator", AutonomyLevel::Operator),
            ("Collaborator", AutonomyLevel::Collaborator), // case-insensitive
            (" consultant ", AutonomyLevel::Consultant),   // trimmed
            ("observer", AutonomyLevel::Observer),
        ] {
            let home = home_with_agent(
                "a",
                &format!("[capabilities]\nautonomy_level = \"{raw}\"\n"),
            );
            assert_eq!(AutonomyLevel::for_agent(home.path(), "a"), want, "for {raw:?}");
        }
    }

    fn driver(store: Arc<TaskStore>, queue: Arc<MessageQueue>, cfg: GoalLoopConfig) -> GoalLoopDriver {
        GoalLoopDriver::new(store, queue, cfg)
    }

    fn small_cfg() -> GoalLoopConfig {
        GoalLoopConfig {
            iteration_cap: 2,
            // Kept equal to `iteration_cap` so the short test goal texts (which
            // classify as Simple) exercise the same effective cap as before D4.
            iteration_cap_simple: 2,
            soft_cap: 3,
            wall_clock_hours: 24,
            max_concurrent: 3,
            tick_secs: 30,
            stalled_secs: 600,
            // H22 off by default in tests — the timeout-report tests build
            // their own config so every other test's tick stays query-free.
            progress_report_minutes: 0,
            resume_on_restart: "auto".to_string(),
            tool_streak_advisory: true,
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
        assert_eq!(d.iteration_cap, 5);
        assert_eq!(d.iteration_cap_simple, 3);
        assert_eq!(d.soft_cap, 3);
        assert_eq!(d.max_concurrent, 3);
        // H10: advisory-only, so the safe default is ON (unlike most
        // goal-loop gates, which default off).
        assert!(d.tool_streak_advisory);

        // Partial section ⇒ only the given field overrides; the rest default.
        let toml = "[goal_loop]\niteration_cap = 7\n";
        let table: toml::Table = toml.parse().unwrap();
        let cfg: GoalLoopConfig =
            table.get("goal_loop").unwrap().clone().try_into().unwrap();
        assert_eq!(cfg.iteration_cap, 7);
        assert_eq!(cfg.iteration_cap_simple, 3, "unspecified field keeps its default");
        assert_eq!(cfg.soft_cap, 3, "unspecified field keeps its default");
        assert_eq!(cfg.max_concurrent, 3, "unspecified field keeps its default");
        assert_eq!(cfg.wall_clock_hours, 24);
        assert!(cfg.tool_streak_advisory, "unspecified field keeps its default (on)");

        // H10: explicit `false` in config.toml is honored.
        let toml_off = "[goal_loop]\ntool_streak_advisory = false\n";
        let table_off: toml::Table = toml_off.parse().unwrap();
        let cfg_off: GoalLoopConfig =
            table_off.get("goal_loop").unwrap().clone().try_into().unwrap();
        assert!(!cfg_off.tool_streak_advisory);
    }

    // ── H10: tool-call streak advisory — capture_round_state integration ──

    /// Write `count` identical `(tool, input)` calls for `agent` into
    /// `<dir>/tool_calls.jsonl`, timestamped "now" — paired with a task
    /// `claimed_at` fixed safely in the past, this lands every write inside
    /// `capture_round_state`'s `[since, until]` evidence window.
    fn write_tool_calls_jsonl(dir: &Path, agent: &str, tool: &str, input: &str, count: usize) {
        let lines: Vec<String> = (0..count)
            .map(|_| {
                serde_json::json!({
                    "timestamp": Utc::now().to_rfc3339(),
                    "agent_id": agent,
                    "tool_name": tool,
                    "success": true,
                    "input": input,
                })
                .to_string()
            })
            .collect();
        std::fs::write(dir.join("tool_calls.jsonl"), format!("{}\n", lines.join("\n"))).unwrap();
    }

    fn claimed_review_task(id: &str, agent: &str) -> TaskRow {
        let mut t = goal_task(id, agent);
        t.claimed_by = Some(agent.to_string());
        // Safely in the past so any "now"-stamped tool_calls.jsonl row lands
        // inside `capture_round_state`'s `[since, until]` window.
        t.claimed_at = Some("2020-01-01T00:00:00Z".into());
        t.status = "review".into();
        t.result_summary = Some("did the thing".into());
        t
    }

    #[tokio::test]
    async fn capture_round_state_injects_escalating_tool_streak_hint() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let t = claimed_review_task("g1", "alice");
        store.insert_task(&t).await.unwrap();
        write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 5);

        let d = driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(dir.path().to_path_buf());
        d.capture_round_state(&t).await;

        let after = store.get_task("g1").await.unwrap().unwrap();
        let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
        let hint = snap.tool_streak_hint.expect("a streak of 5 must inject a hint");
        assert!(hint.contains("bash"));
        assert!(hint.contains('5'));
        // Tier 5 wording ("switch approach"), not tier 3's ("re-read the result").
        assert!(hint.contains("換一個方法") || hint.contains("換個方法"));

        // H10: also recorded to the Activity Feed for dashboard observability.
        let activity = store.list_activity_for_task("g1", 10).await.unwrap();
        assert!(
            activity
                .iter()
                .any(|a| a.event_type == "goal_loop.tool_call_streak" && a.summary.contains('5')),
            "streak count must be recorded to the activity feed: {activity:?}"
        );
    }

    #[tokio::test]
    async fn capture_round_state_tier_8_hint_mentions_tasks_block() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let t = claimed_review_task("g1", "alice");
        store.insert_task(&t).await.unwrap();
        write_tool_calls_jsonl(dir.path(), "alice", "web_fetch", "{\"url\":\"https://x\"}", 9);

        let d = driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(dir.path().to_path_buf());
        d.capture_round_state(&t).await;

        let after = store.get_task("g1").await.unwrap().unwrap();
        let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
        let hint = snap.tool_streak_hint.expect("a streak of 9 must inject a hint");
        assert!(hint.contains("tasks_block"), "tier 8 must point at the escape hatch: {hint}");
    }

    #[tokio::test]
    async fn capture_round_state_no_hint_below_lowest_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let t = claimed_review_task("g1", "alice");
        store.insert_task(&t).await.unwrap();
        // Only 2 in a row — below the tier-3 floor.
        write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 2);

        let d = driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(dir.path().to_path_buf());
        d.capture_round_state(&t).await;

        let after = store.get_task("g1").await.unwrap().unwrap();
        let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
        assert!(snap.tool_streak_hint.is_none(), "a streak below 3 must not inject anything");
    }

    #[tokio::test]
    async fn capture_round_state_different_params_never_form_a_streak() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let t = claimed_review_task("g1", "alice");
        store.insert_task(&t).await.unwrap();
        // 4 calls to the same tool, but every one has a distinct argument —
        // real exploration, not a stuck loop.
        let lines: Vec<String> = (0..4)
            .map(|i| {
                serde_json::json!({
                    "timestamp": Utc::now().to_rfc3339(),
                    "agent_id": "alice",
                    "tool_name": "bash",
                    "success": true,
                    "input": format!("cat file_{i}.txt"),
                })
                .to_string()
            })
            .collect();
        std::fs::write(dir.path().join("tool_calls.jsonl"), format!("{}\n", lines.join("\n"))).unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(dir.path().to_path_buf());
        d.capture_round_state(&t).await;

        let after = store.get_task("g1").await.unwrap().unwrap();
        let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
        assert!(snap.tool_streak_hint.is_none(), "distinct params each round must never register as a streak");
    }

    #[tokio::test]
    async fn capture_round_state_config_off_yields_zero_injection() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let t = claimed_review_task("g1", "alice");
        store.insert_task(&t).await.unwrap();
        // Well past every threshold — would inject the tier-8 hint if enabled.
        write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 10);

        let cfg = GoalLoopConfig { tool_streak_advisory: false, ..small_cfg() };
        let d = driver(store.clone(), queue.clone(), cfg).with_home_dir(dir.path().to_path_buf());
        d.capture_round_state(&t).await;

        let after = store.get_task("g1").await.unwrap().unwrap();
        let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
        assert!(snap.tool_streak_hint.is_none(), "tool_streak_advisory = false must produce zero injection");

        // No activity event either — config off means the whole feature is silent.
        let activity = store.list_activity_for_task("g1", 10).await.unwrap();
        assert!(
            !activity.iter().any(|a| a.event_type == "goal_loop.tool_call_streak"),
            "config off must not even post the activity event: {activity:?}"
        );
    }

    #[tokio::test]
    async fn capture_round_state_tool_streak_hint_renders_in_next_round_state_block() {
        // End-to-end: the persisted hint round-trips through
        // `GoalStateSnapshot` into the next dispatch round's rendered
        // `<state>` block, exactly like `bail_hint` already does.
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let t = claimed_review_task("g1", "alice");
        store.insert_task(&t).await.unwrap();
        write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 5);

        let d = driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(dir.path().to_path_buf());
        d.capture_round_state(&t).await;

        let after = store.get_task("g1").await.unwrap().unwrap();
        let snapshot = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
        let block = goal_state::build_state_block(&after, &[], &snapshot);
        let rendered = block.render();
        assert!(
            rendered.contains("bash"),
            "the tool-streak hint must surface in the rendered <state> block: {rendered}"
        );
    }

    // ── G3: resolve_deadline_hit (design §6, market-belief-loop sister
    // package) ────────────────────────────────────────────────

    #[test]
    fn resolve_deadline_hit_neither_deadline_reached_is_none() {
        let created = "2026-08-01T00:00:00Z";
        let now = DateTime::parse_from_rfc3339("2026-08-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // wall clock budget 24h, no task deadline ⇒ 12h in, nothing fires.
        assert_eq!(resolve_deadline_hit(created, None, 24, now), None);
    }

    #[test]
    fn resolve_deadline_hit_wall_clock_only_matches_pre_g3_behavior() {
        let created = "2026-08-01T00:00:00Z";
        let now = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // No deadline_at set ⇒ byte-identical to the old wall-clock-only check.
        assert_eq!(
            resolve_deadline_hit(created, None, 24, now),
            Some(DeadlineHit::WallClock)
        );
    }

    #[test]
    fn resolve_deadline_hit_task_deadline_earlier_fires_first() {
        let created = "2026-08-01T00:00:00Z";
        // Wall clock budget is 24h (deadline 2026-08-02T00:00Z); the task's own
        // deadline_at is much tighter — 4h from creation.
        let deadline_at = "2026-08-01T04:00:00Z";
        let now = DateTime::parse_from_rfc3339("2026-08-01T05:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            resolve_deadline_hit(created, Some(deadline_at), 24, now),
            Some(DeadlineHit::TaskDeadline),
            "the tighter per-task deadline overrides the looser global wall clock"
        );
    }

    #[test]
    fn resolve_deadline_hit_wall_clock_still_wins_when_task_deadline_is_looser() {
        let created = "2026-08-01T00:00:00Z";
        // Task deadline is LATER than the global wall-clock budget (720h vs
        // 24h) — deadline_at can only tighten, never loosen, so the global
        // budget must still fire at 24h.
        let deadline_at = "2026-08-31T00:00:00Z";
        let now = DateTime::parse_from_rfc3339("2026-08-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            resolve_deadline_hit(created, Some(deadline_at), 24, now),
            Some(DeadlineHit::WallClock)
        );
    }

    #[test]
    fn resolve_deadline_hit_unparseable_deadline_at_degrades_to_wall_clock_only() {
        let created = "2026-08-01T00:00:00Z";
        let now = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // Garbage deadline_at must never panic and must never block the
        // wall-clock half of the check (fail-open on the deadline only).
        assert_eq!(
            resolve_deadline_hit(created, Some("not-a-timestamp"), 24, now),
            Some(DeadlineHit::WallClock)
        );
    }

    #[test]
    fn resolve_deadline_hit_unparseable_created_at_with_valid_task_deadline() {
        let now = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // created_at itself unparseable (legacy/corrupt row) but deadline_at
        // is valid and past ⇒ the task-level deadline still fires.
        assert_eq!(
            resolve_deadline_hit("garbage", Some("2026-08-02T00:00:00Z"), 24, now),
            Some(DeadlineHit::TaskDeadline)
        );
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

    /// G3: a task-level `deadline_at` in the past escalates even though the
    /// global wall-clock budget has not been reached, with a distinct
    /// escalation reason so a human sees WHY (design §6 G3).
    #[tokio::test]
    async fn task_deadline_escalates_before_wall_clock_with_distinct_reason() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // Created 1h ago (wall-clock budget is 24h — nowhere near tripping),
        // but the assign form's own deadline already lapsed.
        let mut t = goal_task("g1", "alice");
        t.created_at = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        t.deadline_at = Some((Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human");
        assert_eq!(got.judge_feedback.as_deref(), Some("時限已到未通過驗收"));
        assert!(queue.pending_messages(10).await.unwrap().is_empty());
    }

    /// G3: a task-level `deadline_at` set in the FUTURE (looser than the
    /// global wall clock, or simply not yet reached) must not escalate —
    /// only actually-past deadlines fire.
    #[tokio::test]
    async fn task_deadline_in_the_future_does_not_escalate() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        let mut t = goal_task("g1", "alice");
        t.deadline_at = Some((Utc::now() + chrono::Duration::hours(10)).to_rfc3339());
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_ne!(got.status, "needs_human");
        assert!(!queue.pending_messages(10).await.unwrap().is_empty());
    }

    /// G2: every dispatch carries the risk boundary section, programmatically
    /// — a task with no explicit `risk_boundary` gets the built-in baseline
    /// text (no `config.toml [goal_defaults]` present in the test cwd, so
    /// `baseline_boundary` fails open to `DEFAULT_BASELINE_BOUNDARY`), never
    /// silently omitted.
    #[tokio::test]
    async fn dispatch_always_injects_risk_boundary_section() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].payload.contains("## 本目標風險邊界"));
        assert!(pending[0].payload.contains("遵循當地法規"));
        assert!(pending[0].payload.contains("驗收判官退回"));
    }

    /// G2: an explicit per-task `risk_boundary` overrides the baseline text
    /// in the injected section.
    #[tokio::test]
    async fn dispatch_injects_explicit_task_risk_boundary_over_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let mut t = goal_task("g1", "alice");
        t.risk_boundary = Some("不得動用生產資料庫寫入權限".to_string());
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].payload.contains("不得動用生產資料庫寫入權限"));
        assert!(
            !pending[0].payload.contains("遵循當地法規"),
            "explicit risk_boundary replaces, not appends to, the baseline"
        );
    }

    // ── I-1c "想一想" plan-first: end-to-end through the driver ──────────
    //
    // These simulate exactly what `handlers.rs::handle_tasks_goal_create` +
    // `goal_plan::apply_plan_first_result` produce on the `Ok` branch — a task
    // born directly in `needs_human` with `plan_pending` set — without going
    // through the real (network-calling) planner, matching this file's own
    // testing convention (the concrete LLM caller is exercised by live
    // verification, not a unit test; see `goal_plan.rs`'s `StubCaller` tests
    // for the generation logic itself).
    fn plan_first_pending_task(id: &str, agent: &str, plan: &str) -> TaskRow {
        let mut t = goal_task(id, agent);
        t.status = "needs_human".into();
        t.pause_reason = Some(crate::pause_reason::PauseReason::BlockedNeedsDecision.as_str().into());
        t.judge_feedback = Some(plan.into());
        t.plan_pending = Some(plan.into());
        t
    }

    /// A plan awaiting approval must NEVER execute — the whole point of
    /// "想一想" is that nothing runs before a human decides. `needs_human` is
    /// not one of the driver's dispatch-candidate statuses
    /// (`todo`/`pending`/`revising`), so this is really testing that the
    /// plan-first creation path (parking directly in `needs_human`) actually
    /// keeps the task out of the loop — not a new guard, the existing
    /// candidate-status filter already provides it.
    #[tokio::test]
    async fn plan_pending_task_is_not_dispatched_before_approval() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store
            .insert_task(&plan_first_pending_task("g1", "alice", "- 查資料\n- 寫報告"))
            .await
            .unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        assert!(
            queue.pending_messages(10).await.unwrap().is_empty(),
            "a plan awaiting human approval must not execute"
        );
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "needs_human");
    }

    /// Approving the plan (the dashboard/channel "重試" action on a
    /// `needs_human` task — no new button kind) must both start execution AND
    /// carry the approved plan into that very first round's prompt; the plan
    /// is then consumed so a later round never repeats it.
    #[tokio::test]
    async fn approving_a_pending_plan_dispatches_it_into_round_one_then_consumes_it() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store
            .insert_task(&plan_first_pending_task("g1", "alice", "- 查資料\n- 寫報告"))
            .await
            .unwrap();

        // Human clicks "重試" (= approve and start) — the SAME resolution
        // path any other needs_human task uses, with no note.
        assert!(store.resolve_needs_human("g1", "retry", "").await.unwrap());
        let approved = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(approved.status, "pending");
        assert_eq!(
            approved.plan_pending.as_deref(),
            Some("- 查資料\n- 寫報告"),
            "plan_pending must have survived the approval write"
        );

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let dispatched = queue.pending_messages(10).await.unwrap();
        assert_eq!(dispatched.len(), 1, "round 1 must dispatch once approved");
        assert!(
            dispatched[0].payload.contains("查資料") && dispatched[0].payload.contains("寫報告"),
            "the approved plan must reach round 1's prompt: {}",
            dispatched[0].payload
        );
        assert!(
            dispatched[0].payload.contains("<execution_plan>"),
            "the plan is rendered as its own distinct block, not folded into judge feedback"
        );

        // Consumed — a later round (e.g. a judge rejection re-dispatch) must
        // not keep repeating the same plan block forever.
        assert!(
            store.get_task("g1").await.unwrap().unwrap().plan_pending.is_none(),
            "plan_pending must be cleared after being injected once"
        );
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

    // ── RFC-27: edition concurrency gate (cross-process in-flight cap) ──

    #[tokio::test]
    async fn edition_concurrency_gate_defers_new_goals_then_frees_slot_on_release() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        for i in 0..3 {
            store
                .insert_task(&goal_task(&format!("g{i}"), "alice"))
                .await
                .unwrap();
        }

        // In-process guard set high (3) so ONLY the edition gate (cap 1) bites —
        // this isolates the new gate from the pre-existing `max_concurrent` one.
        let cfg = GoalLoopConfig {
            max_concurrent: 3,
            ..small_cfg()
        };
        let d = GoalLoopDriver::new(store.clone(), queue.clone(), cfg)
            .with_home_dir(dir.path().to_path_buf())
            .with_concurrency_limit(Some(1), 1800);

        // Tick 1: edition cap 1 admits exactly one of the three new goals; the
        // other two defer (queue semantics — not dropped).
        d.tick_once().await.unwrap();
        assert_eq!(
            queue.pending_messages(10).await.unwrap().len(),
            1,
            "edition cap 1 admits at most one new goal per tick"
        );
        assert_eq!(
            duduclaw_core::concurrency_active_count(dir.path(), CONCURRENCY_CLASS_GOAL),
            1,
            "exactly one cross-process lease is held"
        );

        // The admitted task reaches a terminal state → its lease is released.
        let held: Vec<String> = d.inflight.lock().await.keys().cloned().collect();
        assert_eq!(held.len(), 1);
        store
            .update_task(&held[0], &serde_json::json!({ "status": "done" }))
            .await
            .unwrap();

        // Tick 2: reconcile releases the finished task's lease, then a deferred
        // goal is admitted into the freed slot (still capped at 1).
        d.tick_once().await.unwrap();
        assert_eq!(
            duduclaw_core::concurrency_active_count(dir.path(), CONCURRENCY_CLASS_GOAL),
            1,
            "the freed slot is taken by exactly one previously-deferred goal"
        );
        assert_eq!(
            queue.pending_messages(10).await.unwrap().len(),
            2,
            "one more goal dispatched after the first released its slot"
        );
    }

    #[tokio::test]
    async fn edition_concurrency_unlimited_is_byte_identical() {
        // `None` limit ⇒ the gate is a complete no-op: all three admit under the
        // in-process cap of 3, and the lease file is never even created.
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        for i in 0..3 {
            store
                .insert_task(&goal_task(&format!("g{i}"), "alice"))
                .await
                .unwrap();
        }
        let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf())
            .with_concurrency_limit(None, 1800);
        d.tick_once().await.unwrap();
        assert_eq!(
            queue.pending_messages(10).await.unwrap().len(),
            3,
            "unlimited edition dispatches all three under the in-process cap"
        );
        assert!(
            !dir.path().join("concurrency_leases.json").exists(),
            "the unlimited path must never touch the lease file"
        );
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

    // ── A2 no-progress guard (structural, replaces the old P3 two-round
    //    identical-judge-feedback oscillation guard) ─────────

    /// Drive one full rejection round for a task already tracked in-flight and
    /// awaiting pickup: (1) agent moves it to `review` and a tick observes that
    /// (flips `awaiting_pickup=false`, does not re-dispatch); (2) the judge
    /// rejects with `feedback` (→ `revising`, `judge_feedback` set); (3) the next
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
        // Judge rejects → revising + judge_feedback (soft_cap 3 — a high value so
        // the diminishing flag never interferes with these oscillation tests).
        store.reject_review(id, feedback, 99).await.unwrap();
    }

    /// A2 semantics (M3: re-aligned to the pre-A2 guard's exact timing):
    /// escalation fires the moment this round's `<state>` hash (goal +
    /// confirmed facts + self-reported hypotheses + LATEST judge feedback —
    /// `goal_state::StateBlock::hash_input`) would repeat for a 2nd
    /// consecutive dispatch — i.e. two consecutive rejections already
    /// carried the identical underlying state, so a 3rd dispatch would be
    /// provably useless. This matches the pre-A2 guard byte-for-byte in
    /// timing (it escalated the moment two consecutive judge rejections
    /// carried identical feedback text, never attempting a 3rd dispatch
    /// first) while keeping A2's stronger "state" comparison (goal +
    /// confirmed facts + hypotheses + latest rejection, not just the raw
    /// feedback string). Same external contract as before (event_type
    /// `goal_loop.oscillation`, reason text `"goal-loop no-progress
    /// oscillation"`).
    #[tokio::test]
    async fn identical_feedback_two_rounds_escalates_oscillation() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // High iteration cap (both difficulty tiers) so ONLY the A2 guard can
        // escalate here — the test goal text classifies as Simple.
        let cfg = GoalLoopConfig { iteration_cap: 10, iteration_cap_simple: 10, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100; // don't let reject_review self-escalate
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), cfg);

        // Initial dispatch (iter 1, awaiting pickup) — commits state_hash A
        // (no rejection history yet).
        d.tick_once().await.unwrap();

        // Round 1: agent works, judge rejects "same". Next tick re-dispatches
        // — state_hash changes A→B (∅ → "same reason" as the latest excluded
        // entry), so the streak resets to 1: no escalation yet.
        agent_round_then_reject(&d, &store, "g1", "same reason").await;
        d.tick_once().await.unwrap();
        assert_ne!(
            store.get_task("g1").await.unwrap().unwrap().status,
            "needs_human",
            "first rejection must not escalate"
        );

        // Round 2: identical feedback again ⇒ state_hash stays B for what
        // would be a 2nd consecutive dispatch of that exact state ⇒
        // escalate now, WITHOUT ever attempting a 3rd dispatch.
        agent_round_then_reject(&d, &store, "g1", "same reason").await;
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human");
        assert_eq!(
            got.judge_feedback.as_deref(),
            Some("goal-loop no-progress oscillation"),
            "external judge_feedback text kept byte-identical to the pre-A2 guard"
        );
        // Exactly 2 work messages were ever enqueued (iter 1 and iter 2) — no
        // 3rd dispatch happened before escalation, matching the pre-A2 timing.
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 2, "no 3rd dispatch before the A2 guard fires");

        let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
        assert!(
            acts.iter().any(|a| a.event_type == "goal_loop.oscillation"),
            "an oscillation activity must be recorded under the same event_type \
             topology_evolution.rs (D5) queries"
        );
    }

    #[tokio::test]
    async fn differing_feedback_keeps_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // High cap on both tiers (the test goal classifies as Simple) so the
        // iteration guard never pre-empts the differing-feedback retry path.
        let cfg = GoalLoopConfig { iteration_cap: 10, iteration_cap_simple: 10, ..small_cfg() };
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
        assert_eq!(got.status, "revising");
        let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
        assert!(
            !acts.iter().any(|a| a.event_type == "goal_loop.oscillation"),
            "no oscillation should be recorded for differing feedback"
        );
    }

    // ── H4: gap fingerprinting integrated into the A2 no-progress guard ──

    /// The DoD's "same gap, reworded" case, end to end: two rejections that
    /// cite the SAME `path:line` but with completely different prose must
    /// now escalate exactly like literally-identical feedback would — this
    /// was NOT true before H4 (each reworded rejection produced a distinct
    /// `state_hash`, so the guard never fired).
    #[tokio::test]
    async fn reworded_feedback_citing_the_same_gap_escalates_oscillation() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        let cfg = GoalLoopConfig { iteration_cap: 10, iteration_cap_simple: 10, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100;
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), cfg);
        d.tick_once().await.unwrap();

        agent_round_then_reject(
            &d,
            &store,
            "g1",
            "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120, please add a check.",
        )
        .await;
        d.tick_once().await.unwrap();
        assert_ne!(
            store.get_task("g1").await.unwrap().unwrap().status,
            "needs_human",
            "first rejection must not escalate"
        );

        // Same underlying gap (same path:line), completely reworded prose.
        agent_round_then_reject(
            &d,
            &store,
            "g1",
            "You forgot proper error handling at crates/duduclaw-gateway/src/goal_loop.rs:120 — add validation.",
        )
        .await;
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human", "reworded feedback citing the same gap must still escalate");
        let (acts, _) = store.list_activity(None, None, 100, 0).await.unwrap();
        assert!(acts.iter().any(|a| a.event_type == "goal_loop.oscillation"));
    }

    /// Counterpart: two rejections citing DIFFERENT `path:line` gaps must
    /// keep retrying, not escalate — the fingerprint must be sensitive to a
    /// genuinely different gap, not just insensitive to rewording.
    #[tokio::test]
    async fn feedback_citing_different_gaps_keeps_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        let cfg = GoalLoopConfig { iteration_cap: 10, iteration_cap_simple: 10, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100;
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), cfg);
        d.tick_once().await.unwrap();

        agent_round_then_reject(
            &d,
            &store,
            "g1",
            "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120.",
        )
        .await;
        d.tick_once().await.unwrap();

        agent_round_then_reject(
            &d,
            &store,
            "g1",
            "Missing error handling in crates/duduclaw-gateway/src/goal_state.rs:42.",
        )
        .await;
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_ne!(got.status, "needs_human", "citations to different gaps must keep retrying");
    }

    // ── H6: resume_on_restart ──────────────────────────────────

    /// WP-E: direct check that `GoalLoopConfig::default()` resolves to
    /// `Pause` — the platform default flip, independent of the
    /// boot-reconciliation behavior exercised by the tests below.
    #[test]
    fn goal_loop_config_default_resume_on_restart_is_pause() {
        assert_eq!(GoalLoopConfig::default().resume_on_restart(), ResumeOnRestart::Pause);
        assert_eq!(GoalLoopConfig::default().resume_on_restart, "pause");
    }

    #[tokio::test]
    async fn resume_on_restart_auto_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        std::fs::write(dir.path().join("config.toml"), "[goal_loop]\nresume_on_restart = \"auto\"\n").unwrap();

        let mut t = goal_task("g1", "alice");
        t.status = "in_progress".into();
        store.insert_task(&t).await.unwrap();

        let paused = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
        assert_eq!(paused, 0, "explicit auto must never touch any task");
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "in_progress", "task status must be untouched under auto");
    }

    // WP-E (2026-08 P1 rollout, user-approved spec change — see
    // `GoalLoopConfig::default`): the platform default for
    // `resume_on_restart` flipped from "auto" to "pause". This test used to
    // be named `resume_on_restart_default_config_is_a_noop` and assert the
    // opposite (paused == 0); it is intentionally rewritten, not just
    // relabeled, to lock in the new default as a spec change rather than
    // silently deleting coverage of "what happens with no config.toml at all".
    #[tokio::test]
    async fn resume_on_restart_default_config_pauses_inflight_tasks() {
        // No config.toml at all ⇒ GoalLoopConfig::from_home defaults ⇒ Pause
        // (WP-E default, was Auto pre-WP-E).
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let mut t = goal_task("g1", "alice");
        t.status = "review".into();
        store.insert_task(&t).await.unwrap();

        let paused = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
        assert_eq!(paused, 1, "missing config.toml must default to pause (WP-E default) and escalate the in-flight task");
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human", "default pause must escalate the in-flight task at boot");
    }

    #[tokio::test]
    async fn resume_on_restart_pause_escalates_inflight_goal_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        std::fs::write(dir.path().join("config.toml"), "[goal_loop]\nresume_on_restart = \"pause\"\n").unwrap();

        // Non-terminal goal_mode tasks across a few different statuses.
        let mut t1 = goal_task("g1", "alice");
        t1.status = "in_progress".into();
        store.insert_task(&t1).await.unwrap();

        let mut t2 = goal_task("g2", "alice");
        t2.status = "review".into();
        store.insert_task(&t2).await.unwrap();

        let mut t3 = goal_task("g3", "alice"); // still "todo" — never dispatched, must NOT be paused
        store.insert_task(&t3).await.unwrap();

        // A terminal task must NOT be touched.
        let mut t4 = goal_task("g4", "alice");
        t4.status = "done".into();
        store.insert_task(&t4).await.unwrap();

        // A non-goal-mode task in a matching status must NOT be touched.
        let mut t5 = TaskRow::new(
            "t5".into(),
            "ordinary task".into(),
            "not a goal".into(),
            "medium".into(),
            "alice".into(),
            "system".into(),
        );
        t5.status = "in_progress".into();
        store.insert_task(&t5).await.unwrap();

        let paused = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
        assert_eq!(paused, 2, "exactly the 2 genuinely in-flight goal_mode tasks must be paused");

        for id in ["g1", "g2"] {
            let got = store.get_task(id).await.unwrap().unwrap();
            assert_eq!(got.status, "needs_human", "{id} must be escalated");
            assert_eq!(got.judge_feedback.as_deref(), Some("gateway_restart"));
        }
        // A queued goal the user confirmed but that never dispatched is NOT
        // "still running" — it must survive the boot scan untouched and
        // dispatch normally afterwards (live-verification catch, 2026-08-15).
        assert_eq!(
            store.get_task("g3").await.unwrap().unwrap().status,
            "todo",
            "queued-but-never-dispatched goal must not be paused"
        );
        assert_eq!(store.get_task("g4").await.unwrap().unwrap().status, "done", "terminal task untouched");
        assert_eq!(
            store.get_task("t5").await.unwrap().unwrap().status,
            "in_progress",
            "non-goal-mode task untouched"
        );
    }

    #[tokio::test]
    async fn resume_on_restart_pause_is_idempotent_across_two_boots() {
        // A second "boot" (e.g. a crash loop) must not re-escalate an
        // already-`needs_human` task, and must not error.
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        std::fs::write(dir.path().join("config.toml"), "[goal_loop]\nresume_on_restart = \"pause\"\n").unwrap();

        let mut t = goal_task("g1", "alice");
        t.status = "in_progress".into();
        store.insert_task(&t).await.unwrap();

        let first = pause_inflight_on_restart(store.clone(), queue.clone(), dir.path()).await;
        assert_eq!(first, 1);
        let second = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
        assert_eq!(second, 0, "an already needs_human task must not be re-escalated");
    }

    // ── H7: continuation feedback is single-instance, not accumulated ──

    /// Audit finding (H7): `enqueue_work`'s `<judge_feedback>` block is
    /// built from `task.judge_feedback` alone — a single `Option<String>`
    /// column that `TaskStore::reject_review`/`accept_review` OVERWRITE on
    /// every judge verdict (`SET ... judge_feedback = ?5 ...`, never
    /// concatenated — see `task_store.rs`). There was nothing to change;
    /// this regression test locks the "already single-instance" finding in
    /// so a future edit accidentally reintroducing accumulation (e.g.
    /// appending instead of overwriting) trips a red test immediately.
    #[tokio::test]
    async fn continuation_feedback_never_accumulates_across_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        let cfg = GoalLoopConfig { iteration_cap: 10, iteration_cap_simple: 10, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100;
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), cfg);
        d.tick_once().await.unwrap();

        agent_round_then_reject(&d, &store, "g1", "round one feedback: fix the widget shape").await;
        d.tick_once().await.unwrap(); // re-dispatch carrying round-one feedback

        agent_round_then_reject(&d, &store, "g1", "round two feedback: fix the widget color instead").await;
        d.tick_once().await.unwrap(); // re-dispatch carrying round-two feedback

        let pending = queue.pending_messages(10).await.unwrap();
        let latest = pending.last().expect("at least one pending message");
        // Exactly one `<judge_feedback>` block per payload — never doubled up.
        assert_eq!(latest.payload.matches("<judge_feedback>").count(), 1);
        // Isolate the `<judge_feedback>...</judge_feedback>` block itself —
        // deliberately NOT a whole-payload substring check, because the
        // SEPARATE `<excluded_approaches>` section of the `<state>` block
        // intentionally accumulates up to 6 historical rejection reasons
        // (see `goal_state.rs::excluded_from_iterations`) and legitimately
        // still contains round-one's text there. The H7 claim under test is
        // specifically about the single dedicated continuation-feedback
        // block, not that field.
        let start = latest.payload.find("<judge_feedback>").expect("judge_feedback block present") + "<judge_feedback>".len();
        let end = latest.payload[start..].find("</judge_feedback>").expect("judge_feedback close tag present") + start;
        let feedback_block = &latest.payload[start..end];
        assert!(
            feedback_block.contains("round two feedback: fix the widget color instead"),
            "the <judge_feedback> block must carry the latest feedback"
        );
        assert!(
            !feedback_block.contains("round one feedback: fix the widget shape"),
            "the <judge_feedback> block must NOT still carry the stale round-one text \
             (that is the H7 single-instance claim under test) — got: {feedback_block:?}"
        );
    }

    // ── Iterative Kanban: revising re-dispatch + cap ordering ──

    #[tokio::test]
    async fn revising_task_is_re_dispatched_and_opens_new_round() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // A task the judge already rejected once → revising, round 1.
        let mut t = goal_task("g1", "alice");
        t.status = "revising".into();
        t.revision_round = 1;
        t.judge_feedback = Some("add the missing section".into());
        store.insert_task(&t).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        // Re-dispatched with feedback, and iteration round 2 opened (round =
        // revision_round + 1).
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].payload.contains("add the missing section"));
        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters.len(), 1);
        assert_eq!(iters[0].round, 2, "revising re-dispatch opens round revision_round+1");
    }

    /// The A2 no-progress guard must win over the iteration cap when BOTH
    /// would fire on the very same tick (the guard runs before the cap
    /// check in `tick_once`). M3 lowered the A2 threshold to a 2nd
    /// consecutive identical-state dispatch, so `iteration_cap` is set to
    /// exactly 2 so that, at the tick where that 2nd identical-state
    /// dispatch is evaluated, `current_iter (2) >= cap (2)` is ALSO true —
    /// a genuine simultaneous boundary, not merely "the cap happens to be
    /// generous enough to never matter".
    #[tokio::test]
    async fn oscillation_takes_precedence_over_iteration_cap() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        let cfg = GoalLoopConfig { iteration_cap: 2, iteration_cap_simple: 2, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100; // don't let reject_review self-escalate
        store.insert_task(&t).await.unwrap();
        let d = driver(store.clone(), queue.clone(), cfg);

        d.tick_once().await.unwrap(); // dispatch #1 (iter 1, state_hash A)
        agent_round_then_reject(&d, &store, "g1", "same").await;
        d.tick_once().await.unwrap(); // dispatch #2 (iter 2 == cap, state_hash B, streak 1)
        agent_round_then_reject(&d, &store, "g1", "same").await;
        // This tick: current_iter (2) >= cap (2) AND the would-be streak (2)
        // both hold — the A2 guard must fire first.
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human");
        assert_eq!(
            got.judge_feedback.as_deref(),
            Some("goal-loop no-progress oscillation"),
            "the A2 no-progress guard must win over the iteration-cap reason"
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

    // WP3 (PORTICO): when a kickoff approval clears, the task's declared
    // `grant:<tool>` tags are atomically minted as task-scoped grants.
    #[tokio::test]
    async fn kickoff_approval_mints_declared_grants() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        write_agent_toml(
            dir.path(),
            "alice",
            "[capabilities]\nautonomy_level = \"collaborator\"\n",
        );
        let mut task = goal_task("g1", "alice");
        task.tags = "grant:send_message, other-tag".into();
        store.insert_task(&task).await.unwrap();

        let broker = Arc::new(crate::approval::ApprovalBroker::open(dir.path()).unwrap());
        let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf())
            .with_broker(broker.clone());

        let grants =
            crate::capability_grants::CapabilityGrantStore::open(dir.path()).unwrap();

        // Tick 1: kickoff filed, no grant yet.
        d.tick_once().await.unwrap();
        assert!(!grants.has_active_grant("alice", "send_message").await);
        let approval_id = broker.list_pending(Some("alice")).await.unwrap()[0].id.clone();

        // Approve → tick 2 dispatches AND mints the declared grant.
        broker.decide(&approval_id, true, "test:alice").await.unwrap();
        d.tick_once().await.unwrap();
        assert!(
            grants.has_active_grant("alice", "send_message").await,
            "kickoff approval must mint the declared grant:send_message"
        );
        // A non-grant tag never becomes a grant.
        assert!(!grants.has_active_grant("alice", "other-tag").await);
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

    // ── D4 item 1: dependency DAG gating ────────────────────

    /// A goal task with `depends_on` set is frozen until every dependency is
    /// `done`, then dispatched. The dependency itself dispatches immediately.
    #[tokio::test]
    async fn dependent_task_is_frozen_until_dep_done() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        store.insert_task(&goal_task("g1", "alice")).await.unwrap();
        let mut g2 = goal_task("g2", "alice");
        g2.depends_on = r#"["g1"]"#.into();
        store.insert_task(&g2).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();
        // Only g1 dispatched; g2 frozen (dep not done).
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].payload.contains("task_id=g1"));

        // Mark g1 done → g2 becomes dispatchable next tick.
        store
            .update_task("g1", &serde_json::json!({ "status": "done" }))
            .await
            .unwrap();
        d.tick_once().await.unwrap();
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 2, "g2 dispatched once its dependency is done");
        assert!(pending.iter().any(|m| m.payload.contains("task_id=g2")));
    }

    /// A downstream task whose dependency ends terminally (failed / needs_human /
    /// cancelled / missing) inherits the escalation — it is parked `needs_human`
    /// rather than frozen forever (never orphaned).
    #[tokio::test]
    async fn dependency_failure_escalates_downstream() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        let mut g1 = goal_task("g1", "alice");
        g1.status = "failed".into();
        store.insert_task(&g1).await.unwrap();
        let mut g2 = goal_task("g2", "alice");
        g2.depends_on = r#"["g1"]"#.into();
        store.insert_task(&g2).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        let got = store.get_task("g2").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human", "downstream inherits escalation");
        assert!(got
            .judge_feedback
            .as_deref()
            .unwrap_or("")
            .contains("upstream dependency failed"));
        // No work message enqueued for the frozen/escalated downstream task.
        assert!(queue.pending_messages(10).await.unwrap().is_empty());
    }

    /// A missing dependency id (never resolvable) also escalates downstream —
    /// fail-closed, does not wait forever.
    #[tokio::test]
    async fn missing_dependency_escalates_downstream() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let mut g2 = goal_task("g2", "alice");
        g2.depends_on = r#"["ghost"]"#.into();
        store.insert_task(&g2).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();
        assert_eq!(
            store.get_task("g2").await.unwrap().unwrap().status,
            "needs_human"
        );
    }

    // ── D4 item 2: dispatch policy integration ──────────────

    /// With a RoundRobin policy wired, a task assigned to a non-roster agent is
    /// re-routed to a roster member and the reassignment is persisted.
    #[tokio::test]
    async fn round_robin_policy_reassigns_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        // Roster = {alice, bob} (approver so no kickoff gate).
        write_agent_toml(dir.path(), "alice", "[capabilities]\nautonomy_level = \"approver\"\n");
        write_agent_toml(dir.path(), "bob", "[capabilities]\nautonomy_level = \"approver\"\n");

        // Task assigned to someone NOT in the roster ⇒ policy must re-route.
        store.insert_task(&goal_task("g1", "zzz")).await.unwrap();

        let policy: Arc<dyn DispatchPolicy> = Arc::new(crate::dispatch_policy::RoundRobin::new());
        let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf())
            .with_policy(policy);
        d.tick_once().await.unwrap();

        // RoundRobin picks the first roster member (sorted): "alice".
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.assigned_to, "alice", "reassignment persisted to the roster member");
        let pending = queue.pending_messages(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].target, "alice", "work dispatched to the re-routed agent");
    }

    /// The default (no policy) path is unchanged: dispatch to the stored
    /// `assigned_to`, no reassignment.
    #[tokio::test]
    async fn default_policy_keeps_assigned_to() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        assert_eq!(store.get_task("g1").await.unwrap().unwrap().assigned_to, "alice");
        assert_eq!(queue.pending_messages(10).await.unwrap()[0].target, "alice");
    }

    // ── L4: derive_goal_kind / count_distinct_hits ──────────

    #[test]
    fn count_distinct_hits_dedupes_overlapping_cjk_keywords() {
        // "程式", "程式碼", "寫程式" are three separate entries of
        // GOAL_KIND_CODING_KEYWORDS, but all three match inside "寫程式碼"
        // and mutually overlap the same characters — must count as ONE
        // cluster, not three.
        let lower = "幫我寫程式碼".to_lowercase();
        assert_eq!(count_distinct_hits(&lower, &GOAL_KIND_CODING_KEYWORDS), 1);
    }

    #[test]
    fn count_distinct_hits_counts_non_overlapping_separately() {
        let lower = "寫程式碼順便修一個 bug".to_lowercase();
        // "寫程式碼" cluster (1) + separate "bug" (1) = 2, not 4 raw keyword
        // matches (程式/程式碼/寫程式/bug).
        assert_eq!(count_distinct_hits(&lower, &GOAL_KIND_CODING_KEYWORDS), 2);
    }

    #[test]
    fn count_distinct_hits_is_anchored_not_substring() {
        // Old `contains`-based counting matched "send" inside "sender" and
        // "email" inside "emailed" — both false positives for an OPS signal.
        let lower = "check the sender field, already emailed them".to_lowercase();
        assert_eq!(
            count_distinct_hits(&lower, &GOAL_KIND_OPS_KEYWORDS),
            0,
            "unanchored substrings inside sender/emailed must not count as OPS hits"
        );
        // A real word-boundary hit still counts.
        let real_hit = "please send it now".to_lowercase();
        assert_eq!(count_distinct_hits(&real_hit, &GOAL_KIND_OPS_KEYWORDS), 1);
    }

    #[test]
    fn count_distinct_hits_empty_on_no_match() {
        let lower = "just chatting, nothing special".to_lowercase();
        assert_eq!(count_distinct_hits(&lower, &GOAL_KIND_CODING_KEYWORDS), 0);
    }

    #[test]
    fn derive_goal_kind_classifies_coding_despite_overlapping_keywords() {
        // Before L4 this text scored coding=3 (dominating trivially); after
        // the dedup fix it scores coding=1 — still correctly the dominant
        // (only) topical signal, still classified as a Coding variant.
        let kind = derive_goal_kind("請幫我寫程式碼");
        assert!(
            matches!(kind, GoalKind::CodingSimple | GoalKind::CodingComplex),
            "unexpected kind: {kind:?}"
        );
    }

    #[test]
    fn derive_goal_kind_ops_dominates_over_research() {
        let kind = derive_goal_kind("請部署新版本並通知團隊");
        assert_eq!(kind, GoalKind::OpsOrExternal);
    }

    #[test]
    fn derive_goal_kind_no_signal_falls_back_on_difficulty() {
        let kind = derive_goal_kind("哈囉");
        assert_eq!(kind, GoalKind::Unknown, "short, no-keyword text ⇒ Simple ⇒ Unknown");
    }

    // ── L3: state_capture_seen must not leak across terminal states ──

    #[tokio::test]
    async fn state_capture_seen_is_cleared_when_task_reaches_done() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap(); // dispatch iter 1

        // Agent moves it to review — the next tick's reconcile loop should
        // record a first capture.
        store
            .update_task("g1", &serde_json::json!({ "status": "review" }))
            .await
            .unwrap();
        d.tick_once().await.unwrap();
        assert!(
            d.state_capture_seen.lock().await.contains("g1"),
            "review sitting must be recorded as captured"
        );

        // Task reaches a terminal state (done) without ever re-entering the
        // candidate set (todo/pending/revising) — the top-of-tick prune
        // alone can never clear it; the `done` branch's explicit removal
        // must.
        store
            .update_task("g1", &serde_json::json!({ "status": "done" }))
            .await
            .unwrap();
        d.tick_once().await.unwrap();

        assert!(
            !d.state_capture_seen.lock().await.contains("g1"),
            "state_capture_seen must not leak once the task is terminal"
        );
    }

    #[tokio::test]
    async fn state_capture_seen_is_cleared_when_task_reaches_needs_human_directly() {
        // Simulates DispatchEngine's own judge-retry-budget path setting
        // needs_human directly (not via this driver's `escalate()`) — the
        // reconcile loop's `_` catch-all branch must clear the flag too.
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap();

        store
            .update_task("g1", &serde_json::json!({ "status": "review" }))
            .await
            .unwrap();
        d.tick_once().await.unwrap();
        assert!(d.state_capture_seen.lock().await.contains("g1"));

        store
            .update_task("g1", &serde_json::json!({ "status": "needs_human" }))
            .await
            .unwrap();
        d.tick_once().await.unwrap();

        assert!(
            !d.state_capture_seen.lock().await.contains("g1"),
            "state_capture_seen must be cleared on a directly-set needs_human too"
        );
    }

    #[tokio::test]
    async fn state_capture_seen_is_cleared_on_driver_escalate() {
        // Exercises this driver's OWN `escalate()` path — here via the A2
        // no-progress guard, the cheapest trigger to set up without
        // fighting `update_task`'s field whitelist (which doesn't allow
        // rewriting `created_at` post-insert for a deadline trigger). Note:
        // under the current `tick_once` control flow the top-of-tick prune
        // already clears a candidate task's entry before `escalate()` runs
        // in the same tick (a task must be a candidate to reach
        // `escalate()` at all, and the prune runs first) — so
        // `escalate()`'s own removal is defense-in-depth for future call
        // sites/orderings rather than the only thing keeping this specific
        // scenario clean today. The end-to-end invariant asserted below (no
        // leak once terminal) must hold either way.
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();

        let d = driver(store.clone(), queue.clone(), small_cfg());
        d.tick_once().await.unwrap(); // dispatch iter 1 (commits state_hash A)

        store
            .update_task("g1", &serde_json::json!({ "status": "review" }))
            .await
            .unwrap();
        d.tick_once().await.unwrap(); // captures review state
        assert!(d.state_capture_seen.lock().await.contains("g1"));

        // Return it to `pending` WITHOUT going through a real judge
        // rejection (no new `judge_feedback`) — the recomputed `<state>`
        // hash is therefore byte-identical to state_hash A, so the A2
        // no-progress guard's `would_be_streak` reaches 2 on this very next
        // tick and `escalate()` fires.
        store
            .update_task("g1", &serde_json::json!({ "status": "pending" }))
            .await
            .unwrap();
        d.tick_once().await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human", "A2 guard must have escalated");
        assert!(
            !d.state_capture_seen.lock().await.contains("g1"),
            "escalate() must clear state_capture_seen too"
        );
    }

    // ── W3-1 (D5): the goal loop is the highest-volume dispatch path into a
    //    conversation. A human who takes over must freeze it — and only it. ──

    fn begin_takeover(home: &Path, channel: &str, chat_id: &str) {
        duduclaw_core::takeover_state::begin(
            home,
            &duduclaw_core::takeover_state::BeginRequest {
                conversation: format!("{channel}:{chat_id}"),
                agent_id: "alice".into(),
                holder_user_id: "555".into(),
                holder_display: "王小明".into(),
            },
            &duduclaw_core::takeover_state::TakeoverConfig::default(),
            chrono::Utc::now(),
        )
        .unwrap();
    }

    fn goal_task_from_chat(id: &str, chat_id: &str) -> TaskRow {
        let mut t = goal_task(id, "alice");
        t.source_channel = Some("telegram".into());
        t.source_chat_id = Some(chat_id.to_string());
        t
    }

    #[tokio::test]
    async fn goal_dispatch_freezes_while_a_human_holds_the_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store
            .insert_task(&goal_task_from_chat("g1", "12345"))
            .await
            .unwrap();
        begin_takeover(dir.path(), "telegram", "12345");

        let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf());
        d.tick_once().await.unwrap();
        assert!(
            queue.pending_messages(10).await.unwrap().is_empty(),
            "no dispatch into a conversation a human is running"
        );

        // Frozen, not escalated: parking it `needs_human` would page the very
        // person who is already handling it.
        let t = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(t.status, "todo");

        // Handback resumes the loop unchanged.
        duduclaw_core::takeover_state::end(dir.path(), "telegram:12345", chrono::Utc::now())
            .unwrap();
        d.tick_once().await.unwrap();
        assert_eq!(
            queue.pending_messages(10).await.unwrap().len(),
            1,
            "the goal is dispatched once the human hands back"
        );
    }

    #[tokio::test]
    async fn goal_dispatch_takeover_is_scoped_to_the_held_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store
            .insert_task(&goal_task_from_chat("g-other", "99999"))
            .await
            .unwrap();
        store.insert_task(&goal_task("g-nosource", "alice")).await.unwrap();
        begin_takeover(dir.path(), "telegram", "12345");

        let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf());
        d.tick_once().await.unwrap();

        let dispatched: Vec<String> = queue
            .pending_messages(10)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.target)
            .collect();
        assert_eq!(
            dispatched.len(),
            2,
            "a takeover on one conversation must not freeze the whole board"
        );
    }

    // ── H11: every escalation path stamps its pause class ───────────────
    //
    // One test per trigger, asserting BOTH the (unchanged) free-text reason
    // and the new class — the whole point of H11 is that a human triages on
    // the class, so a trigger silently landing in the wrong bucket is a real
    // regression, not a cosmetic one.

    async fn pause_class_of(store: &TaskStore, id: &str) -> crate::pause_reason::PauseReason {
        let t = store.get_task(id).await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human", "{id} should be parked");
        crate::pause_reason::PauseReason::from_stored(t.pause_reason.as_deref())
    }

    #[tokio::test]
    async fn iteration_cap_escalation_is_classified_budget_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();
        let cfg = GoalLoopConfig { iteration_cap: 2, stalled_secs: 0, ..small_cfg() };
        let d = driver(store.clone(), queue.clone(), cfg);

        d.tick_once().await.unwrap();
        d.tick_once().await.unwrap();
        d.tick_once().await.unwrap(); // cap reached ⇒ escalate

        assert_eq!(
            pause_class_of(&store, "g1").await,
            crate::pause_reason::PauseReason::BudgetExhausted
        );
    }

    #[tokio::test]
    async fn both_deadline_flavours_are_classified_budget_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;

        // Global wall clock (created 48h ago, budget 24h).
        let mut wall = goal_task("g-wall", "alice");
        wall.created_at = (Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        store.insert_task(&wall).await.unwrap();
        // Per-goal deadline that already lapsed, wall clock nowhere near.
        let mut task_dl = goal_task("g-dl", "alice");
        task_dl.deadline_at = Some((Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        store.insert_task(&task_dl).await.unwrap();

        driver(store.clone(), queue.clone(), small_cfg()).tick_once().await.unwrap();

        // Two different human-readable reasons, one class — a person triaging
        // 「次數或時限用盡」 does not care which clock ran out, only that one did.
        let wall_row = store.get_task("g-wall").await.unwrap().unwrap();
        assert_eq!(wall_row.judge_feedback.as_deref(), Some("goal-loop deadline"));
        let dl_row = store.get_task("g-dl").await.unwrap().unwrap();
        assert_eq!(dl_row.judge_feedback.as_deref(), Some("時限已到未通過驗收"));
        for id in ["g-wall", "g-dl"] {
            assert_eq!(
                pause_class_of(&store, id).await,
                crate::pause_reason::PauseReason::BudgetExhausted,
                "{id}"
            );
        }
    }

    #[tokio::test]
    async fn oscillation_escalation_is_classified_no_progress() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let cfg = GoalLoopConfig { iteration_cap: 10, iteration_cap_simple: 10, ..small_cfg() };
        let mut t = goal_task("g1", "alice");
        t.max_retries = 100; // only the A2 guard may escalate here
        store.insert_task(&t).await.unwrap();
        let d = driver(store.clone(), queue.clone(), cfg);

        d.tick_once().await.unwrap();
        agent_round_then_reject(&d, &store, "g1", "same reason").await;
        d.tick_once().await.unwrap();
        agent_round_then_reject(&d, &store, "g1", "same reason").await;
        d.tick_once().await.unwrap();

        // Distinct from the cap escalations above: the loop still had budget,
        // it just stopped making progress.
        assert_eq!(
            pause_class_of(&store, "g1").await,
            crate::pause_reason::PauseReason::NoProgress
        );
    }

    #[tokio::test]
    async fn dependency_failure_escalation_is_classified_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let mut g1 = goal_task("g1", "alice");
        g1.status = "failed".into();
        store.insert_task(&g1).await.unwrap();
        let mut g2 = goal_task("g2", "alice");
        g2.depends_on = r#"["g1"]"#.into();
        store.insert_task(&g2).await.unwrap();

        driver(store.clone(), queue.clone(), small_cfg()).tick_once().await.unwrap();

        assert_eq!(
            pause_class_of(&store, "g2").await,
            crate::pause_reason::PauseReason::BlockedNeedsDecision
        );
    }

    #[tokio::test]
    async fn restart_pause_is_classified_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        std::fs::write(
            dir.path().join("config.toml"),
            "[goal_loop]\nresume_on_restart = \"pause\"\n",
        )
        .unwrap();
        let mut t = goal_task("g1", "alice");
        t.status = "in_progress".into();
        store.insert_task(&t).await.unwrap();

        assert_eq!(pause_inflight_on_restart(store.clone(), queue, dir.path()).await, 1);
        assert_eq!(
            pause_class_of(&store, "g1").await,
            crate::pause_reason::PauseReason::Restart
        );
    }

    /// The safety net: a task parked through a path that declares no class
    /// (any future caller of the string-only `mark_needs_human`, plus every
    /// row that predates the column) reads as `Unknown` = 「需要人工確認」,
    /// never as a confident bucket.
    #[tokio::test]
    async fn unclassified_and_legacy_pauses_read_as_needs_human_review() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _queue) = open_stores(dir.path()).await;
        store.insert_task(&goal_task("g1", "alice")).await.unwrap();
        store.mark_needs_human("g1", "something odd happened").await.unwrap();
        assert_eq!(
            pause_class_of(&store, "g1").await,
            crate::pause_reason::PauseReason::Unknown
        );

        // A legacy row: parked with no class column value at all.
        let mut legacy = goal_task("g2", "alice");
        legacy.status = "needs_human".into();
        legacy.pause_reason = None;
        store.insert_task(&legacy).await.unwrap();
        assert_eq!(
            pause_class_of(&store, "g2").await,
            crate::pause_reason::PauseReason::Unknown
        );
    }

    // ── H22: no-progress timeout report ─────────────────────────────────

    #[test]
    fn no_progress_minutes_thresholds() {
        let now = Utc::now();
        let twelve_ago = now - chrono::Duration::minutes(12);

        // Disabled: 0 and any negative value never report, however long the
        // silence has been.
        assert_eq!(no_progress_minutes(twelve_ago, now, 0), None);
        assert_eq!(no_progress_minutes(twelve_ago, now, -5), None);

        // Below / at / above the threshold.
        assert_eq!(no_progress_minutes(now - chrono::Duration::minutes(9), now, 10), None);
        assert_eq!(no_progress_minutes(now - chrono::Duration::minutes(10), now, 10), Some(10));
        assert_eq!(no_progress_minutes(twelve_ago, now, 10), Some(12));

        // A signal timestamped in the FUTURE (clock skew / hand-edited row)
        // degrades to silence rather than reporting a negative duration.
        assert_eq!(
            no_progress_minutes(now + chrono::Duration::hours(1), now, 10),
            None
        );
    }

    /// Test-only: put a task into the driver's in-flight map with a chosen
    /// dispatch instant, so the silence window can be exercised without
    /// waiting real minutes (and without any activity row the driver's own
    /// dispatch would have stamped at "now").
    fn inflight_entry(iter: u32, enqueued_at: DateTime<Utc>) -> InFlight {
        InFlight {
            iter,
            enqueued_at,
            awaiting_pickup: false,
            lease: None,
            progress_reported_round: None,
        }
    }

    async fn progress_report_events(store: &TaskStore, id: &str) -> usize {
        store
            .list_activity_for_task(id, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a.event_type == "goal_loop.progress_report")
            .count()
    }

    #[tokio::test]
    async fn silent_task_gets_one_notice_and_is_deduped_within_the_round() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let mut t = goal_task("g1", "alice");
        t.status = "in_progress".into();
        store.insert_task(&t).await.unwrap();

        let cfg = GoalLoopConfig { progress_report_minutes: 10, ..small_cfg() };
        let d = driver(store.clone(), queue.clone(), cfg).with_home_dir(dir.path().to_path_buf());

        let now = Utc::now();
        let mut map: HashMap<String, InFlight> =
            HashMap::from([("g1".to_string(), inflight_entry(3, now - chrono::Duration::minutes(45)))]);

        d.maybe_report_no_progress(&mut map, &t, now).await;
        assert_eq!(progress_report_events(&store, "g1").await, 1);
        assert_eq!(
            map["g1"].progress_reported_round,
            Some(3),
            "the round must be marked reported so later ticks stay quiet"
        );
        let summary = store
            .list_activity_for_task("g1", 100)
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.event_type == "goal_loop.progress_report")
            .unwrap()
            .summary;
        assert!(summary.contains("45 分鐘"), "elapsed minutes must be named: {summary}");
        assert!(summary.contains("未回報進度"), "{summary}");

        // Ten hours later, still the same round: the dedup flag alone must
        // hold (the elapsed time is now enormous, so nothing else would).
        d.maybe_report_no_progress(&mut map, &t, now + chrono::Duration::hours(10)).await;
        assert_eq!(
            progress_report_events(&store, "g1").await,
            1,
            "at most one notice per round"
        );

        // A NEW round (a re-dispatch rebuilds the entry) reports again. The
        // round-4 dispatch is placed AFTER the round-3 notice's own activity
        // row, so that row cannot masquerade as this round's progress signal —
        // which is precisely the `> enqueued_at` floor being exercised.
        let after_first_notice = Utc::now();
        map.insert(
            "g1".to_string(),
            inflight_entry(4, after_first_notice + chrono::Duration::seconds(1)),
        );
        d.maybe_report_no_progress(&mut map, &t, after_first_notice + chrono::Duration::minutes(30))
            .await;
        assert_eq!(progress_report_events(&store, "g1").await, 2);
    }

    #[tokio::test]
    async fn progress_report_stays_silent_when_disabled_or_recently_active() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let mut t = goal_task("g1", "alice");
        t.status = "in_progress".into();
        store.insert_task(&t).await.unwrap();
        let now = Utc::now();

        // progress_report_minutes = 0 ⇒ off, even after 45 minutes of silence.
        let off = driver(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(dir.path().to_path_buf());
        assert_eq!(off.config.progress_report_minutes, 0);
        let mut map: HashMap<String, InFlight> =
            HashMap::from([("g1".to_string(), inflight_entry(1, now - chrono::Duration::minutes(45)))]);
        off.maybe_report_no_progress(&mut map, &t, now).await;
        assert_eq!(progress_report_events(&store, "g1").await, 0, "0 disables the notice");
        assert!(map["g1"].progress_reported_round.is_none());

        // Enabled, but the agent posted to the feed 1 minute ago ⇒ that IS
        // progress; the long-ago dispatch time must not win.
        let on = driver(store.clone(), queue.clone(), GoalLoopConfig { progress_report_minutes: 10, ..small_cfg() })
            .with_home_dir(dir.path().to_path_buf());
        store
            .append_activity(&crate::task_store::ActivityRow {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "agent.progress".into(),
                agent_id: "alice".into(),
                task_id: Some("g1".into()),
                summary: "還在跑第三步".into(),
                timestamp: (now - chrono::Duration::minutes(1)).to_rfc3339(),
                metadata: None,
            })
            .await
            .unwrap();
        on.maybe_report_no_progress(&mut map, &t, now).await;
        assert_eq!(
            progress_report_events(&store, "g1").await,
            0,
            "a fresh activity row is a progress signal — stay quiet"
        );
    }

    /// End-to-end through `tick_once`, so the reconcile-loop wiring (not just
    /// the helper) is covered: an `in_progress` tracked task that has gone
    /// quiet produces the notice on a real tick.
    #[tokio::test]
    async fn tick_reports_no_progress_for_a_tracked_in_progress_task() {
        let dir = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(dir.path()).await;
        let mut t = goal_task("g1", "alice");
        t.status = "in_progress".into();
        store.insert_task(&t).await.unwrap();

        let cfg = GoalLoopConfig { progress_report_minutes: 10, ..small_cfg() };
        let d = driver(store.clone(), queue.clone(), cfg).with_home_dir(dir.path().to_path_buf());
        d.inflight.lock().await.insert(
            "g1".to_string(),
            inflight_entry(1, Utc::now() - chrono::Duration::minutes(30)),
        );

        d.tick_once().await.unwrap();
        assert_eq!(progress_report_events(&store, "g1").await, 1);
        // Purely a report: the task keeps running, untouched.
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "in_progress", "the notice must never intervene");
        assert!(got.pause_reason.is_none());
        assert!(queue.pending_messages(10).await.unwrap().is_empty());
    }
}
