//! SQLite-backed persistent store for tasks and activity events.
//!
//! Provides CRUD operations for the Task Board (Kanban) and an append-only
//! activity feed. WAL mode + 5s busy_timeout for multi-process safety.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

/// Canonical column list for `tasks` SELECTs. Kept in one place so
/// `row_to_task`'s positional indices stay in lock-step with every query.
/// Order here == field order in `row_to_task`.
const TASK_COLUMNS: &str = "id, title, description, status, priority, assigned_to, created_by, \
     created_at, updated_at, completed_at, blocked_reason, parent_task_id, tags, message_id, \
     claimed_by, claimed_at, lease_expires_at, depends_on, retry_count, max_retries, \
     goal_mode, acceptance_criteria, result_summary, judge_feedback, goal_id, lease_renewed_at, \
     source_channel, source_chat_id, revision_round, diminishing, agent_seconds, goal_state_json, \
     source_discord_guild_id, deadline_at, risk_boundary, acceptance_criteria_baseline, \
     pause_reason, plan_pending, archived, pinned, team_spec_json";

/// I-3a marker stamped onto `judge_feedback` by [`TaskStore::continue_from_terminal`]
/// so [`crate::goal_loop::GoalLoopDriver::enqueue_work`] can tell a dashboard
/// "接著做" follow-up message apart from a genuine judge-rejection feedback
/// string and phrase the next dispatch prompt correctly. An unprintable
/// (NUL-delimited) prefix — never appears in real judge text or a pasted
/// human note — so a message that happens to start with the same words is
/// never misclassified. Never surfaced to a user: every dashboard view that
/// renders `task.judge_feedback` is gated on `status IN ('failed',
/// 'needs_human')`, and `continue_from_terminal` always leaves the row in
/// `pending`; the marker is fully overwritten the moment the task next
/// passes through `accept_review`/`reject_review`.
pub(crate) const CONTINUE_MESSAGE_PREFIX: &str = "\u{0}duduclaw:continue\u{0}";

// ── Task row ────────────────────────────────────────────────

mod activity;
mod claim;
mod goals;
mod iterations;
mod plans;
mod pure;
mod review;
mod schema;
mod tasks;

#[cfg(test)]
mod tests;

pub use plans::plan_order_for_insert;
pub use pure::{
    deps_satisfied, introduces_dependency_cycle, introduces_parent_cycle, lease_is_expired,
    parse_depends_on, zombie_action, zombie_reclaim_due,
};

use goals::depends_edges_conn;
use iterations::{iter_submit_conn, iter_verdict_conn, list_iterations_conn};
use tasks::row_to_task;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,   // todo | in_progress | done | blocked
    pub priority: String, // low | medium | high | urgent
    pub assigned_to: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub blocked_reason: Option<String>,
    pub parent_task_id: Option<String>,
    pub tags: String, // comma-separated
    pub message_id: Option<String>,

    // ── G1 durable dispatch fields (v1.36) ──────────────────
    /// Worker that atomically claimed this task (NULL = unclaimed).
    #[serde(default)]
    pub claimed_by: Option<String>,
    /// When the current claim was taken (RFC3339).
    #[serde(default)]
    pub claimed_at: Option<String>,
    /// Lease deadline (RFC3339). A claimed task whose lease has elapsed with no
    /// renewal is a zombie and gets reclaimed. NULL ⇒ not lease-managed
    /// (e.g. dashboard board tasks) and never reclaimed.
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    /// JSON array of task ids that must be `done` before this task is claimable.
    #[serde(default = "empty_deps")]
    pub depends_on: String,
    /// How many times this task has been requeued after a zombie reclaim / goal
    /// rejection.
    #[serde(default)]
    pub retry_count: i64,
    /// Requeue cap. When `retry_count >= max_retries`, reclaim marks `failed`.
    #[serde(default = "default_max_retries")]
    pub max_retries: i64,
    /// Goal mode: completion goes through judge acceptance before `done`.
    #[serde(default)]
    pub goal_mode: bool,
    /// Acceptance criteria fed to the judge when `goal_mode` is set.
    #[serde(default)]
    pub acceptance_criteria: Option<String>,
    /// The worker's completion summary — the artifact the judge evaluates.
    #[serde(default)]
    pub result_summary: Option<String>,
    /// Latest judge feedback when a goal-mode task is rejected / escalated.
    #[serde(default)]
    pub judge_feedback: Option<String>,
    /// G8 goal chain: the goal this task serves (NULL = no goal linkage).
    /// Walking `goals.parent_goal_id` from here yields the why-chain
    /// (Initiative → Project → Issue) injected into the agent system prompt.
    #[serde(default)]
    pub goal_id: Option<String>,
    /// When the lease was last renewed (RFC3339) — stamped at claim time and on
    /// every `renew_lease`. Zombie reclaim uses it as the renewal anchor: a
    /// claimed task is only reclaimed when the lease expired AND a further full
    /// lease window (`lease_expires_at - lease_renewed_at`) elapsed with no
    /// renewal, so a live worker's ticker is never raced.
    #[serde(default)]
    pub lease_renewed_at: Option<String>,

    // ── P5 goal loop source write-back (v1.37) ──────────────
    /// Originating channel of a `/goal` command (e.g. `telegram`), so goal-loop
    /// progress / needs_human notices push back to the conversation that
    /// launched the goal rather than only the agent's `[proactive]` channel.
    /// NULL for tasks not created from a channel `/goal` entry.
    #[serde(default)]
    pub source_channel: Option<String>,
    /// Originating chat id of a `/goal` command (the `chat_id` segment of the
    /// launching session). NULL when no source conversation is known.
    #[serde(default)]
    pub source_chat_id: Option<String>,

    // ── Iterative Kanban (v1.45) ────────────────────────────
    /// Judge-rejection round counter for a goal-mode task (distinct from
    /// `retry_count`, which conflates zombie-reclaim requeues with rejections).
    /// 0 for a first attempt; incremented on every judge rejection. The
    /// authoritative per-round detail lives in `task_iterations`; this is a
    /// board-display cache. Old rows migrate to 0.
    #[serde(default)]
    pub revision_round: i64,
    /// Soft-cap flag: set once `revision_round` reaches the goal loop's
    /// `soft_cap` (default 3). Does NOT block the loop — it flags diminishing
    /// returns for the dashboard (amber badge). Cleared only by a fresh task.
    #[serde(default)]
    pub diminishing: bool,
    /// Cumulative agent processing seconds across all rounds
    /// (Σ submitted_at − dispatched_at). The "agent clock" half of the dual
    /// clock; the "wall clock" half is `completed_at − created_at`.
    #[serde(default)]
    pub agent_seconds: i64,

    // ── A1 StateAct self-report round-trip (arXiv:2410.02810, v1.53) ──
    /// JSON snapshot of the agent's self-reported `pending_hypotheses`
    /// (see `goal_loop/state.rs::GoalStateSnapshot`), captured from the
    /// `<state_update>` marker in `result_summary` while a goal-mode task
    /// sits in `review` (before `DispatchEngine::review_goal_tasks` clears
    /// `result_summary` on rejection). `None` until the first successful
    /// capture. No dedicated free-form metadata/notes column existed on
    /// this row prior to A1 — `tags` is comma-separated and user/dashboard
    /// facing (unsuitable for a JSON blob) — so this is a purpose-built
    /// column rather than repurposing an existing field.
    #[serde(default)]
    pub goal_state_json: Option<String>,

    // ── W2-7 deep-link coordinate persistence (v1.55) ───────────
    /// Discord guild id the `/goal` command's source channel belonged to at
    /// task-creation time, when known (`discord.rs` caches `channel_id ->
    /// guild_id` from inbound Gateway events; see
    /// [`crate::discord::guild_id_for_channel`]). `None` for non-Discord
    /// tasks and for Discord tasks created before any message from that
    /// channel reached this gateway (fail-safe: the "在通道中開啟" link
    /// just doesn't render — see `channel_link.rs`). Snapshotted at
    /// creation time rather than looked up live at list time so the link
    /// still resolves after the bot leaves the guild or the cache is
    /// pruned.
    #[serde(default)]
    pub source_discord_guild_id: Option<String>,

    // ── Goal assignment form v2 (design-market-belief-loop-2026-08.md §6,
    // G1) ────────────────────────────────────────────────────────────
    /// Optional per-goal wall-clock deadline (RFC3339), derived from the
    /// assign form's `duration_hours` at creation time (`now + duration`).
    /// `None` ⇒ only the global `[goal_loop] wall_clock_hours` budget
    /// applies. See [`crate::goal_loop::GoalLoopDriver`]'s deadline guard,
    /// which takes the earlier of this and the global wall clock.
    #[serde(default)]
    pub deadline_at: Option<String>,
    /// Optional per-goal risk boundary text the user explicitly typed into
    /// the assign form (≤2000 chars, `duduclaw_core::truncate_chars`).
    /// `None` ⇒ the deployment's baseline boundary
    /// ([`crate::goal_loop::baseline_boundary`]) applies instead — the
    /// baseline is intentionally NOT stored here, so an operator changing
    /// `config.toml [goal_defaults] baseline_boundary` retroactively
    /// affects every task that never overrode it.
    #[serde(default)]
    pub risk_boundary: Option<String>,

    // ── Goal contract freeze (H9-G, harness-borrowings 2026-08 WP-D) ────
    /// Immutable snapshot of `acceptance_criteria` taken at goal-creation
    /// time (`/goal` chat command and `tasks.goal_create` dashboard RPC —
    /// the only two writers; see those call sites). Once set, this column
    /// is NEVER updated again by any code path — it is the frozen contract
    /// the judge evaluates against, so a later edit to the mutable
    /// `acceptance_criteria` field (operator-only, via `tasks.update`)
    /// cannot retroactively change what a task is judged on. `None` for
    /// tasks created before this column existed, or created through a path
    /// that doesn't freeze a baseline (e.g. the generic `tasks_create` MCP
    /// tool) — readers fall back to `acceptance_criteria` in that case,
    /// which for those rows is equally immutable in practice: agent-identity
    /// callers are refused write access to `acceptance_criteria` on
    /// `goal_mode` tasks regardless of which path created them (see
    /// `duduclaw-cli::mcp::handle_tasks_update`).
    #[serde(default)]
    pub acceptance_criteria_baseline: Option<String>,

    // ── H11 pause-reason classification (harness-borrowings 2026-08 §2) ──
    /// Closed classification of WHY this task is parked `needs_human` —
    /// the wire token of a [`crate::pause_reason::PauseReason`], stamped at
    /// the escalation call site (never parsed back out of `judge_feedback`,
    /// which is partly LLM-authored prose). `None` for tasks that were never
    /// escalated, for rows written before this column existed, and after a
    /// human resolves the pause (`resolve_needs_human` clears it, so a
    /// retried task never carries a stale class). Readers must go through
    /// `PauseReason::from_stored`, which maps `None` / unrecognised values
    /// to `Unknown` = 「需要人工確認」.
    #[serde(default)]
    pub pause_reason: Option<String>,

    // ── I-1c "想一想" plan-first mode (2026-08) ─────────────────────────
    /// A generated execution plan awaiting human approval
    /// ([`crate::goal_plan::apply_plan_first_result`]). Deliberately a
    /// SEPARATE column from `judge_feedback` (which also carries a copy of
    /// the same text purely for display, so the existing "why is this
    /// parked" surfaces — dashboard chip, channel decision card — show it
    /// with zero further change): `resolve_needs_human`'s `retry` arm
    /// overwrites `judge_feedback` with the human's own (often empty)
    /// approval note, which would silently lose the plan before the next
    /// dispatch ever read it. This column is untouched by that write, so it
    /// survives approval and lets
    /// [`crate::goal_loop::GoalLoopDriver::enqueue_work`] inject the
    /// approved plan into the very first execution round — then clear this
    /// column so it is injected exactly once, not on every later round.
    /// `None` for every task that never went through plan-first (the
    /// overwhelming majority), and for a plan-first task once its plan has
    /// been consumed by that first dispatch (or the task never got a plan at
    /// all — the planner-failure fail-closed path never sets this).
    #[serde(default)]
    pub plan_pending: Option<String>,

    // ── I-3b task list operations (dashboard-ux-workbuddy 2026-08) ─────
    /// Archived tasks are deliberately taken out of active consideration:
    /// hidden from the general board/list queries
    /// ([`TaskStore::list_tasks_filtered`] / [`TaskStore::list_tasks`]) by
    /// default, but still explicitly queryable via
    /// [`TaskStore::list_tasks_paginated`]. `false` for every pre-existing
    /// row (migration DEFAULT 0).
    #[serde(default)]
    pub archived: bool,
    /// Pinned tasks sort first in list queries (`ORDER BY pinned DESC,
    /// updated_at DESC`) — a lightweight "keep this at the top" flag, no
    /// other query-shape effect. `false` for every pre-existing row.
    #[serde(default)]
    pub pinned: bool,

    // ── Team-as-Agent spec freeze (P1/WP-4, 2026-09) ───────────────────
    /// Immutable snapshot of the role→`{runtime, model, effort}` team this
    /// task runs with, taken once at goal-creation time
    /// ([`crate::team_composer::FrozenTeamSpec`], JSON). Written only through
    /// [`TaskStore::freeze_team_spec`], whose `WHERE team_spec_json IS NULL`
    /// guard makes the write set-once even under a race — the same frozen-
    /// contract discipline as `acceptance_criteria_baseline`, and for the
    /// same reason: a role→model matrix or a bandit that re-ranks models
    /// tomorrow must affect the *next* task, never re-shape one already
    /// mid-flight (design §3.1).
    ///
    /// `None` for every task created without `[team] enabled` — i.e. the
    /// overwhelming majority, and every row written before this column
    /// existed. `None` means Solo, and so does a value this build cannot
    /// parse (`FrozenTeamSpec::parse`): there is no "partial team".
    #[serde(default)]
    pub team_spec_json: Option<String>,
}

fn empty_deps() -> String {
    "[]".to_string()
}

fn default_max_retries() -> i64 {
    3
}

impl TaskRow {
    pub fn new(
        id: String,
        title: String,
        description: String,
        priority: String,
        assigned_to: String,
        created_by: String,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id,
            title,
            description,
            status: "todo".into(),
            priority,
            assigned_to,
            created_by,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
            blocked_reason: None,
            parent_task_id: None,
            tags: String::new(),
            message_id: None,
            claimed_by: None,
            claimed_at: None,
            lease_expires_at: None,
            depends_on: empty_deps(),
            retry_count: 0,
            max_retries: default_max_retries(),
            goal_mode: false,
            acceptance_criteria: None,
            result_summary: None,
            judge_feedback: None,
            goal_id: None,
            lease_renewed_at: None,
            source_channel: None,
            source_chat_id: None,
            revision_round: 0,
            diminishing: false,
            agent_seconds: 0,
            goal_state_json: None,
            source_discord_guild_id: None,
            deadline_at: None,
            risk_boundary: None,
            acceptance_criteria_baseline: None,
            pause_reason: None,
            plan_pending: None,
            archived: false,
            pinned: false,
            team_spec_json: None,
        }
    }
}

// ── Iterative Kanban: iteration detail row (v1.45) ──────────

/// One judge-review round of a goal-mode task (先例: vibe-kanban
/// `coding_agent_turn` / Linear `AgentSession`). The `task_iterations` table is
/// the source of truth for the revision timeline; `tasks.revision_round` /
/// `diminishing` / `agent_seconds` are display caches derived from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIterationRow {
    pub id: i64,
    pub task_id: String,
    /// 1-based attempt number.
    pub round: i64,
    /// When this round's work was dispatched.
    pub dispatched_at: String,
    /// When the worker submitted (NULL ⇒ round in progress).
    pub submitted_at: Option<String>,
    /// When the judge ruled (NULL ⇒ not yet judged).
    pub judged_at: Option<String>,
    /// `accepted` | `rejected` | `escalated` | NULL.
    pub verdict: Option<String>,
    /// The judge's rejection reason for this round.
    pub judge_feedback: Option<String>,
    /// P3 (reserved): ODC injection-source label for the defect.
    pub feedback_class: Option<String>,
    /// Per-aspect MAV panel results as JSON `[{name, pass, reason}]` —
    /// `None` for deterministic (pre-judge) rejections and legacy rows.
    pub verdict_json: Option<String>,
    /// How many times this round was dispatched (stall re-dispatches).
    pub dispatch_count: i64,
    /// Goal-state hash at dispatch time (visit-graph signal), when known.
    pub state_hash: Option<String>,
    /// Same-(state, action) repeat streak observed at dispatch time.
    pub repeat_streak: Option<i64>,
    /// WP-4F: a bounded, CJK-safe-truncated snapshot of this round's own
    /// worker output, taken at verdict time (before `result_summary` is
    /// wiped on rejection). `None` for accepted rounds (never needed — an
    /// accepted task never re-enters `needs_human`) and for rows sealed
    /// before this column existed.
    pub worker_excerpt: Option<String>,
}

/// Per-agent slice of [`FlowMetrics`] (Iterative Kanban analytics, P2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFlow {
    pub agent_id: String,
    /// Goal tasks currently finished (`done`) or in `review`.
    pub goal_tasks: i64,
    pub finished: i64,
    /// Fraction of finished goal tasks accepted on the first round (0..1).
    pub first_pass_yield: f64,
    pub avg_rounds: f64,
    pub avg_agent_seconds: f64,
    pub avg_cycle_seconds: f64,
    pub review_queue_depth: i64,
}

/// Board-level + per-agent flow metrics returned by [`TaskStore::flow_metrics`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowMetrics {
    pub agents: Vec<AgentFlow>,
    pub review_queue_depth: i64,
    pub accepts_last_7d: i64,
    pub avg_daily_accepts_7d: f64,
}

/// Mutable accumulator used while folding tasks into per-agent [`AgentFlow`].
#[derive(Default)]
struct AgentFlowAccum {
    finished: i64,
    first_pass: i64,
    sum_rounds: i64,
    sum_agent_secs: i64,
    sum_cycle_secs: i64,
    review_queue_depth: i64,
}

// ── G1 dispatch value types ─────────────────────────────────

/// What zombie reclaim decided for one expired-lease task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZombieAction {
    /// Lease expired but retries remain — requeue to `pending`.
    Requeue,
    /// Retry budget exhausted — mark `failed`.
    Fail,
}

/// Outcome record returned by [`TaskStore::reclaim_zombies`].
#[derive(Debug, Clone)]
pub struct ZombieOutcome {
    pub task_id: String,
    pub action: ZombieAction,
    /// `retry_count` after the reclaim.
    pub retry_count: i64,
}

/// Result of [`TaskStore::atomic_claim`]. Dependency gating is enforced at the
/// claim boundary itself (inside the claim transaction), so a claim can never
/// bypass an unfinished `depends_on` graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This caller won the claim; the task is now `in_progress` and leased.
    Claimed,
    /// The task is `pending` and unclaimed, but one or more `depends_on`
    /// tasks are not `done` yet (their ids are listed). Fail-closed: a dep id
    /// that references a missing task also counts as unmet.
    BlockedByDeps(Vec<String>),
    /// Already claimed / not `pending` / does not exist.
    NotClaimable,
}

impl ClaimOutcome {
    /// `true` only when this caller won the claim.
    pub fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed)
    }
}

// ── Goal row (G8 goal chain) ────────────────────────────────

/// G8: a node in the goal hierarchy (Initiative → Project → Issue). Tasks link
/// to a goal via `tasks.goal_id`; walking `parent_goal_id` yields the why-chain
/// agents see in their system prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalRow {
    pub id: String,
    pub title: String,
    /// The "why" — rationale carried down to agents working linked tasks.
    pub description: String,
    pub parent_goal_id: Option<String>,
    pub status: String, // active | done | archived
    pub created_at: String,
}

impl GoalRow {
    pub fn new(id: String, title: String, description: String) -> Self {
        Self {
            id,
            title,
            description,
            parent_goal_id: None,
            status: "active".into(),
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

/// Max depth when walking a goal's ancestry. Anything deeper is treated as a
/// data anomaly and the walk stops (fail-safe: chain is truncated, never loops).
const GOAL_ANCESTRY_MAX_DEPTH: usize = 16;

// ── Activity row ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityRow {
    pub id: String,
    pub event_type: String,
    pub agent_id: String,
    pub task_id: Option<String>,
    pub summary: String,
    pub timestamp: String,
    pub metadata: Option<String>, // JSON string
}

// ── Comment row ─────────────────────────────────────────────

/// L2: a human-authored comment on a task. Distinct from `ActivityRow`
/// (system-generated events) — comments are free-text notes left by a logged-in
/// user, rendered in the task detail "discussion" tab interleaved with activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentRow {
    pub id: String,
    pub task_id: String,
    /// The authoring user id (from the authenticated `UserContext`).
    pub author_user: String,
    pub body: String,
    pub created_at: String,
}

// ── Plan rows (U4 interactive co-edited plan) ───────────────
//
// A plan is an ordered list of steps co-edited by the user (dashboard) and an
// AI employee (MCP tools). Plans live in their OWN tables — deliberately NOT
// rows in `tasks` — because the tasks table carries the durable dispatch
// lifecycle (atomic claim, leases, zombie reclaim, heartbeat task-board pulls,
// capability auto-revoke on done, autopilot events). Plan steps stored as
// tasks would surface on the Kanban board, be double-injected into agent
// prompts, and risk being claimed by the dispatch engine. Lean tables keep
// plan semantics (ordered, co-edited checklist) orthogonal and fail-safe.

/// One shared plan. `agent_id` is the owning AI employee — RPC authorization
/// scopes to it exactly like `tasks.assigned_to` (HS4 pattern).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRow {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Owning agent — the AI employee this plan is shared with.
    pub agent_id: String,
    /// Optional G8 goal linkage (the plan's WHY).
    pub goal_id: Option<String>,
    pub status: String, // active | done | archived
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

impl PlanRow {
    pub fn new(id: String, title: String, agent_id: String, created_by: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id,
            title,
            description: String::new(),
            agent_id,
            goal_id: None,
            status: "active".into(),
            created_by,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// One step of a shared plan, assignable to a person or an AI employee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepRow {
    pub id: String,
    pub plan_id: String,
    pub text: String,
    /// Who kind of holder this step belongs to: `user` | `agent`.
    pub assignee_kind: String,
    /// User id (assignee_kind = user) or agent id (assignee_kind = agent).
    /// Empty = unassigned.
    pub assignee: String,
    pub status: String, // todo | doing | done | skipped
    /// Integer-gap ordering key (see [`PLAN_STEP_ORDER_GAP`]).
    pub step_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Ordering strategy: **integer-gap ordering.** Steps are keyed by a sparse
/// `step_order` (1024, 2048, 3072 …). Inserting between neighbours takes the
/// midpoint; when the gap between two neighbours is exhausted (midpoint would
/// collide) the whole plan is renormalized back to multiples of the gap inside
/// the same transaction. Chosen over fractional ordering because it stays in
/// i64 (no float drift / precision cliff) and renormalization is trivially
/// cheap at plan scale (tens of steps).
pub const PLAN_STEP_ORDER_GAP: i64 = 1024;

const PLAN_COLUMNS: &str =
    "id, title, description, agent_id, goal_id, status, created_by, created_at, updated_at";
const PLAN_STEP_COLUMNS: &str =
    "id, plan_id, text, assignee_kind, assignee, status, step_order, created_at, updated_at";

/// Allowed plan step statuses (fail-closed validation at the write boundary).
pub const PLAN_STEP_STATUSES: &[&str] = &["todo", "doing", "done", "skipped"];
/// Allowed step assignee kinds.
pub const PLAN_ASSIGNEE_KINDS: &[&str] = &["user", "agent"];
/// Allowed plan statuses.
pub const PLAN_STATUSES: &[&str] = &["active", "done", "archived"];

// ── Store ───────────────────────────────────────────────────

pub struct TaskStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}
