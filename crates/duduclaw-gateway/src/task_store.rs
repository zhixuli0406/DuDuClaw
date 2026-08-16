//! SQLite-backed persistent store for tasks and activity events.
//!
//! Provides CRUD operations for the Task Board (Kanban) and an append-only
//! activity feed. WAL mode + 5s busy_timeout for multi-process safety.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
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
     pause_reason, plan_pending, archived, pinned";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,       // todo | in_progress | done | blocked
    pub priority: String,     // low | medium | high | urgent
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
    /// (see `goal_state.rs::GoalStateSnapshot`), captured from the
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

impl TaskStore {
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("tasks.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open task store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "TaskStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS tasks (
                 id              TEXT PRIMARY KEY,
                 title           TEXT NOT NULL,
                 description     TEXT NOT NULL DEFAULT '',
                 status          TEXT NOT NULL DEFAULT 'todo',
                 priority        TEXT NOT NULL DEFAULT 'medium',
                 assigned_to     TEXT NOT NULL,
                 created_by      TEXT NOT NULL DEFAULT 'system',
                 created_at      TEXT NOT NULL,
                 updated_at      TEXT NOT NULL,
                 completed_at    TEXT,
                 blocked_reason  TEXT,
                 parent_task_id  TEXT,
                 tags            TEXT NOT NULL DEFAULT '',
                 message_id      TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_tasks_assigned ON tasks(assigned_to);
             CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority);

             CREATE TABLE IF NOT EXISTS activity (
                 id          TEXT PRIMARY KEY,
                 event_type  TEXT NOT NULL,
                 agent_id    TEXT NOT NULL,
                 task_id     TEXT,
                 summary     TEXT NOT NULL,
                 timestamp   TEXT NOT NULL,
                 metadata    TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_activity_agent ON activity(agent_id);
             CREATE INDEX IF NOT EXISTS idx_activity_type  ON activity(event_type);
             CREATE INDEX IF NOT EXISTS idx_activity_ts    ON activity(timestamp DESC);

             CREATE TABLE IF NOT EXISTS task_comments (
                 id          TEXT PRIMARY KEY,
                 task_id     TEXT NOT NULL,
                 author_user TEXT NOT NULL,
                 body        TEXT NOT NULL,
                 created_at  TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_comments_task ON task_comments(task_id, created_at);

             CREATE TABLE IF NOT EXISTS goals (
                 id              TEXT PRIMARY KEY,
                 title           TEXT NOT NULL,
                 description     TEXT NOT NULL DEFAULT '',
                 parent_goal_id  TEXT,
                 status          TEXT NOT NULL DEFAULT 'active',
                 created_at      TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_goals_parent ON goals(parent_goal_id);
             CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);",
        )
        .map_err(|e| format!("init task store schema: {e}"))?;

        // ── G1 durable dispatch: idempotent column migration ──
        // Adds lease/dependency/goal columns to pre-existing `tasks.db` without a
        // rewrite. Each ALTER is guarded by a column-existence check so re-running
        // is a no-op (rusqlite has no `ADD COLUMN IF NOT EXISTS`).
        Self::add_dispatch_columns(conn)?;
        // ── U4 co-edited plans: idempotent table creation ──
        Self::init_plan_schema(conn)?;
        // ── Iterative Kanban: iteration detail table (v1.45) ──
        Self::init_iteration_schema(conn)?;
        Ok(())
    }

    /// Iterative Kanban: idempotent iteration-detail schema. New table only
    /// (`CREATE TABLE IF NOT EXISTS`), so re-running on every open is a no-op.
    fn init_iteration_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_iterations (
                 id             INTEGER PRIMARY KEY AUTOINCREMENT,
                 task_id        TEXT NOT NULL,
                 round          INTEGER NOT NULL,
                 dispatched_at  TEXT NOT NULL,
                 submitted_at   TEXT,
                 judged_at      TEXT,
                 verdict        TEXT,
                 judge_feedback TEXT,
                 feedback_class TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_task_iterations_task
                 ON task_iterations(task_id, round);",
        )
        .map_err(|e| format!("init iteration schema: {e}"))?;

        // 2026-08-14 additive columns (audit-debt cleanup, same idempotent
        // pattern as `add_dispatch_columns`):
        // - verdict_json: per-aspect MAV panel results — previously flattened
        //   into one feedback string before persistence, so the timeline
        //   could never show "correctness ✓ / completeness ✗ / safety ✓".
        // - dispatch_count: how many times this round was actually dispatched
        //   (stall re-dispatches) — previously memory-only in the driver.
        // - state_hash / repeat_streak: the visit-graph oscillation signal at
        //   dispatch time — previously in-memory only, invisible after the
        //   fact.
        let existing: HashSet<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(task_iterations)")
                .map_err(|e| format!("pragma iteration table_info: {e}"))?;
            stmt.query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| format!("query iteration table_info: {e}"))?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(|e| format!("collect iteration table_info: {e}"))?
        };
        let migrations: &[(&str, &str)] = &[
            ("verdict_json", "verdict_json TEXT"),
            ("dispatch_count", "dispatch_count INTEGER NOT NULL DEFAULT 1"),
            ("state_hash", "state_hash TEXT"),
            ("repeat_streak", "repeat_streak INTEGER"),
            // WP-4F: a bounded, CJK-safe-truncated snapshot of the round's
            // own worker output (`tasks.result_summary` at verdict time),
            // taken because `reject_review_with_verdict` wipes
            // `result_summary` back to NULL on every "revising" rejection —
            // without this snapshot no round's actual output survives past
            // its own rejection, so a later budget-exhausted escalation has
            // nothing to attach (see `goal_budget_best_round.rs`).
            ("worker_excerpt", "worker_excerpt TEXT"),
        ];
        for (col, ddl) in migrations {
            if !existing.contains(*col) {
                conn.execute(&format!("ALTER TABLE task_iterations ADD COLUMN {ddl}"), [])
                    .map_err(|e| format!("add iteration column {col}: {e}"))?;
            }
        }
        Ok(())
    }

    /// U4: idempotent plan schema. New tables only (`CREATE TABLE IF NOT
    /// EXISTS`), so re-running on every open is a no-op.
    fn init_plan_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS plans (
                 id          TEXT PRIMARY KEY,
                 title       TEXT NOT NULL,
                 description TEXT NOT NULL DEFAULT '',
                 agent_id    TEXT NOT NULL,
                 goal_id     TEXT,
                 status      TEXT NOT NULL DEFAULT 'active',
                 created_by  TEXT NOT NULL DEFAULT 'system',
                 created_at  TEXT NOT NULL,
                 updated_at  TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_plans_agent  ON plans(agent_id);
             CREATE INDEX IF NOT EXISTS idx_plans_status ON plans(status);

             CREATE TABLE IF NOT EXISTS plan_steps (
                 id            TEXT PRIMARY KEY,
                 plan_id       TEXT NOT NULL,
                 text          TEXT NOT NULL,
                 assignee_kind TEXT NOT NULL DEFAULT 'agent',
                 assignee      TEXT NOT NULL DEFAULT '',
                 status        TEXT NOT NULL DEFAULT 'todo',
                 step_order    INTEGER NOT NULL,
                 created_at    TEXT NOT NULL,
                 updated_at    TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_plan_steps_plan ON plan_steps(plan_id, step_order);",
        )
        .map_err(|e| format!("init plan schema: {e}"))
    }

    /// Idempotently add the G1 dispatch columns. Safe to call on every open.
    fn add_dispatch_columns(conn: &Connection) -> Result<(), String> {
        let existing: HashSet<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(tasks)")
                .map_err(|e| format!("pragma table_info: {e}"))?;
            let cols = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| format!("query table_info: {e}"))?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(|e| format!("collect table_info: {e}"))?;
            cols
        };
        // (column, DDL fragment). NOT NULL columns carry a DEFAULT so the ALTER
        // succeeds against existing rows.
        let migrations: &[(&str, &str)] = &[
            ("claimed_by", "claimed_by TEXT"),
            ("claimed_at", "claimed_at TEXT"),
            ("lease_expires_at", "lease_expires_at TEXT"),
            ("depends_on", "depends_on TEXT NOT NULL DEFAULT '[]'"),
            ("retry_count", "retry_count INTEGER NOT NULL DEFAULT 0"),
            ("max_retries", "max_retries INTEGER NOT NULL DEFAULT 3"),
            ("goal_mode", "goal_mode INTEGER NOT NULL DEFAULT 0"),
            ("acceptance_criteria", "acceptance_criteria TEXT"),
            ("result_summary", "result_summary TEXT"),
            ("judge_feedback", "judge_feedback TEXT"),
            // G8 goal chain + G1 lease-renewal anchor (v1.36).
            ("goal_id", "goal_id TEXT"),
            ("lease_renewed_at", "lease_renewed_at TEXT"),
            // P5 goal-loop source write-back (v1.37).
            ("source_channel", "source_channel TEXT"),
            ("source_chat_id", "source_chat_id TEXT"),
            // Iterative Kanban (v1.45): revision-round cache columns.
            ("revision_round", "revision_round INTEGER NOT NULL DEFAULT 0"),
            ("diminishing", "diminishing INTEGER NOT NULL DEFAULT 0"),
            ("agent_seconds", "agent_seconds INTEGER NOT NULL DEFAULT 0"),
            // A1 StateAct self-report round-trip (v1.53).
            ("goal_state_json", "goal_state_json TEXT"),
            // W2-7 deep-link coordinate persistence (v1.55).
            ("source_discord_guild_id", "source_discord_guild_id TEXT"),
            // Goal assignment form v2 (design-market-belief-loop-2026-08.md
            // §6, G1, 2026-08-14): per-goal deadline + risk boundary.
            ("deadline_at", "deadline_at TEXT"),
            ("risk_boundary", "risk_boundary TEXT"),
            // H9-G goal contract freeze (harness-borrowings 2026-08 WP-D):
            // immutable snapshot of acceptance_criteria at goal-creation time.
            ("acceptance_criteria_baseline", "acceptance_criteria_baseline TEXT"),
            // H11 pause-reason classification (harness-borrowings 2026-08 §2):
            // WHY a task parked `needs_human`, as a closed-set token. Nullable
            // on purpose — every pre-existing row reads back as `Unknown`.
            ("pause_reason", "pause_reason TEXT"),
            // I-1c "想一想" plan-first mode: a generated plan awaiting human
            // approval, surviving `resolve_needs_human`'s retry write (which
            // overwrites `judge_feedback`) so the approved plan reaches the
            // first execution round.
            ("plan_pending", "plan_pending TEXT"),
            // I-3b task list operations (dashboard-ux-workbuddy 2026-08):
            // archive/pin flags for the `/goals` board. `archived` is
            // filtered out of the default list queries; `pinned` sorts
            // first. Both idempotent ALTER TABLE ADD COLUMN, same pattern
            // as every migration above.
            ("archived", "archived INTEGER NOT NULL DEFAULT 0"),
            ("pinned", "pinned INTEGER NOT NULL DEFAULT 0"),
        ];
        for (col, ddl) in migrations {
            if !existing.contains(*col) {
                conn.execute(&format!("ALTER TABLE tasks ADD COLUMN {ddl}"), [])
                    .map_err(|e| format!("add column {col}: {e}"))?;
            }
        }
        // Index for the dispatcher's zombie scan (status + lease).
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tasks_lease ON tasks(status, lease_expires_at)",
            [],
        )
        .map_err(|e| format!("create idx_tasks_lease: {e}"))?;
        // I-3b: supports the default "hide archived" filter in
        // list_tasks_filtered / list_tasks_paginated.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tasks_archived ON tasks(archived)",
            [],
        )
        .map_err(|e| format!("create idx_tasks_archived: {e}"))?;
        Ok(())
    }

    // ── Task CRUD ───────────────────────────────────────────

    pub async fn list_tasks(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
    ) -> Result<Vec<TaskRow>, String> {
        self.list_tasks_filtered(status, agent_id, priority, None).await
    }

    /// `list_tasks` plus an optional `goal_mode` predicate, so goal-scoped
    /// consumers (the `/goals` dashboard page) don't pull the whole board
    /// over the wire just to keep a handful of rows.
    pub async fn list_tasks_filtered(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
        goal_mode: Option<bool>,
    ) -> Result<Vec<TaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = status {
            binds.push(s.to_string());
            sql.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            sql.push_str(&format!(" AND assigned_to = ?{}", binds.len()));
        }
        if let Some(p) = priority {
            binds.push(p.to_string());
            sql.push_str(&format!(" AND priority = ?{}", binds.len()));
        }
        if let Some(g) = goal_mode {
            sql.push_str(if g { " AND goal_mode = 1" } else { " AND goal_mode = 0" });
        }
        // I-3b: archived tasks are hidden from every general listing by
        // default — the board, the heartbeat task-board pull, the goal-loop
        // driver's enumeration, autopilot rule scans, and digests all go
        // through this method (or `list_tasks`). Archiving is a deliberate
        // "take this out of active consideration" action, so once archived
        // a task should stop surfacing here the same way a `done` task
        // isn't re-dispatched. Every pre-existing row defaults to
        // archived=0 (migration DEFAULT), so this is behavior-neutral until
        // a caller actually archives something. Callers that need to browse
        // the archive explicitly use `list_tasks_paginated` instead.
        sql.push_str(" AND archived = 0");
        // Pinned tasks float to the top of every list (I-3b "置頂").
        sql.push_str(" ORDER BY pinned DESC, updated_at DESC");

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare list: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_task)
            .map_err(|e| format!("query list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list: {e}"))?;
        Ok(rows)
    }

    /// I-3b: paginated task listing with a total count, for board views that
    /// need to page through more rows than a client-side slice can safely
    /// hold — the prior `/goals` UI hard-cut finished tasks at 20 with no
    /// way to see the rest (`web/src/pages/GoalsPage.tsx` `.slice(0, 20)`).
    /// Same filter set as [`Self::list_tasks_filtered`], plus an explicit
    /// `archived` tri-state so a caller can deliberately browse the
    /// archive instead of always excluding it: `None` or `Some(false)` ⇒
    /// non-archived only (same default as `list_tasks_filtered`),
    /// `Some(true)` ⇒ archived rows only. `limit` is clamped to `[1, 200]`
    /// so a malformed page size can't force an unbounded scan; `offset` is
    /// floored at 0. Ordering matches `list_tasks_filtered`: pinned rows
    /// first, then most-recently-updated.
    pub async fn list_tasks_paginated(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
        goal_mode: Option<bool>,
        archived: Option<bool>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<TaskRow>, i64), String> {
        let conn = self.conn.lock().await;
        let mut count_sql = "SELECT COUNT(*) FROM tasks WHERE 1=1".to_string();
        let mut query_sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = status {
            binds.push(s.to_string());
            let clause = format!(" AND status = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            let clause = format!(" AND assigned_to = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(p) = priority {
            binds.push(p.to_string());
            let clause = format!(" AND priority = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(g) = goal_mode {
            let clause = if g { " AND goal_mode = 1" } else { " AND goal_mode = 0" };
            count_sql.push_str(clause);
            query_sql.push_str(clause);
        }
        let archived_clause = if archived == Some(true) {
            " AND archived = 1"
        } else {
            " AND archived = 0"
        };
        count_sql.push_str(archived_clause);
        query_sql.push_str(archived_clause);

        let bounded_limit = limit.clamp(1, 200);
        let bounded_offset = offset.max(0);
        query_sql.push_str(&format!(
            " ORDER BY pinned DESC, updated_at DESC LIMIT {bounded_limit} OFFSET {bounded_offset}"
        ));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();

        let total: i64 = conn
            .query_row(&count_sql, params_ref.as_slice(), |r| r.get(0))
            .map_err(|e| format!("count tasks page: {e}"))?;

        let mut stmt = conn
            .prepare(&query_sql)
            .map_err(|e| format!("prepare list page: {e}"))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_task)
            .map_err(|e| format!("query list page: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list page: {e}"))?;
        Ok((rows, total))
    }

    pub async fn get_task(&self, id: &str) -> Result<Option<TaskRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
            params![id],
            row_to_task,
        )
        .optional()
        .map_err(|e| format!("get task: {e}"))
    }

    pub async fn insert_task(&self, row: &TaskRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO tasks
                (id, title, description, status, priority, assigned_to, created_by,
                 created_at, updated_at, completed_at, blocked_reason,
                 parent_task_id, tags, message_id,
                 claimed_by, claimed_at, lease_expires_at, depends_on, retry_count,
                 max_retries, goal_mode, acceptance_criteria, result_summary, judge_feedback,
                 goal_id, lease_renewed_at, source_channel, source_chat_id,
                 revision_round, diminishing, agent_seconds, source_discord_guild_id,
                 deadline_at, risk_boundary, acceptance_criteria_baseline, pause_reason,
                 plan_pending, archived, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28,
                     ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39)",
            params![
                row.id,
                row.title,
                row.description,
                row.status,
                row.priority,
                row.assigned_to,
                row.created_by,
                row.created_at,
                row.updated_at,
                row.completed_at,
                row.blocked_reason,
                row.parent_task_id,
                row.tags,
                row.message_id,
                row.claimed_by,
                row.claimed_at,
                row.lease_expires_at,
                row.depends_on,
                row.retry_count,
                row.max_retries,
                row.goal_mode as i64,
                row.acceptance_criteria,
                row.result_summary,
                row.judge_feedback,
                row.goal_id,
                row.lease_renewed_at,
                row.source_channel,
                row.source_chat_id,
                row.revision_round,
                row.diminishing as i64,
                row.agent_seconds,
                row.source_discord_guild_id,
                row.deadline_at,
                row.risk_boundary,
                row.acceptance_criteria_baseline,
                row.pause_reason,
                row.plan_pending,
                row.archived as i64,
                row.pinned as i64,
            ],
        )
        .map_err(|e| format!("insert task: {e}"))?;
        Ok(())
    }

    /// RFC-26 §4.5 (P6.5): atomically claim an unassigned task. Compare-and-set on
    /// `assigned_to` — only succeeds if the task is currently unassigned (`''`).
    /// Returns `true` if this caller won the claim, `false` if already assigned.
    pub async fn claim_task(&self, id: &str, agent_id: &str, now: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks SET assigned_to=?2, updated_at=?3 WHERE id=?1 AND assigned_to=''",
                params![id, agent_id, now],
            )
            .map_err(|e| format!("claim task: {e}"))?;
        Ok(n > 0)
    }

    /// WP4 hand-off: reassign every *open* (not-`done`) task owned by
    /// `from_agent` to `to_agent`, and follow through on any active claim/lease
    /// so the successor holds the work outright. Returns the number of tasks
    /// moved. Idempotent — a re-run finds nothing left assigned to `from_agent`.
    pub async fn reassign_open_tasks(
        &self,
        from_agent: &str,
        to_agent: &str,
        now: &str,
    ) -> Result<u64, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks
                    SET assigned_to = ?2,
                        claimed_by = CASE WHEN claimed_by = ?1 THEN ?2 ELSE claimed_by END,
                        updated_at = ?3
                  WHERE assigned_to = ?1 AND status != 'done'",
                params![from_agent, to_agent, now],
            )
            .map_err(|e| format!("reassign open tasks: {e}"))?;
        Ok(n as u64)
    }

    /// All `(task_id, parent_task_id)` edges — for cycle detection.
    pub async fn parent_edges(&self) -> Result<Vec<(String, Option<String>)>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, parent_task_id FROM tasks")
            .map_err(|e| format!("prepare edges: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
            .map_err(|e| format!("query edges: {e}"))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| format!("collect edges: {e}"))?;
        Ok(rows)
    }

    /// RFC-26 §4.5: would setting `child.parent = new_parent` create a cycle?
    pub async fn would_create_parent_cycle(
        &self,
        child: &str,
        new_parent: &str,
    ) -> Result<bool, String> {
        let edges = self.parent_edges().await?;
        Ok(introduces_parent_cycle(&edges, child, new_parent))
    }

    pub async fn update_task(&self, id: &str, fields: &serde_json::Value) -> Result<Option<TaskRow>, String> {
        // depends_on rewires the dependency graph — gate it fail-closed at the
        // store boundary: must be a JSON array of ids, no self-dependency, and
        // must not close a cycle (visited-set walk over the current edges).
        // Shape validation is pure; the cycle check runs INSIDE the write
        // transaction below so check and write cannot be raced apart (TOCTOU).
        let new_deps: Option<Vec<String>> = match fields.get("depends_on") {
            Some(deps_val) => {
                let Some(deps_json) = deps_val.as_str() else {
                    return Err("depends_on must be a JSON-array string of task ids".into());
                };
                let Ok(deps) = serde_json::from_str::<Vec<String>>(deps_json) else {
                    return Err("depends_on must be a JSON-array string of task ids".into());
                };
                Some(deps)
            }
            None => None,
        };
        // Scoped block ensures all non-Send refs are dropped before the next await.
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("update task: begin: {e}"))?;
            if let Some(deps) = &new_deps {
                let edges = depends_edges_conn(&tx)?;
                if introduces_dependency_cycle(&edges, id, deps) {
                    return Err(format!(
                        "dependency cycle rejected: task {id} would (transitively) depend on itself"
                    ));
                }
            }
            let now = Utc::now().to_rfc3339();
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<String> = vec![now];

            macro_rules! opt_field {
                ($key:expr, $col:expr) => {
                    if let Some(v) = fields.get($key).and_then(|v| v.as_str()) {
                        binds.push(v.to_string());
                        sets.push(format!("{} = ?{}", $col, binds.len()));
                    }
                };
            }
            opt_field!("title", "title");
            opt_field!("description", "description");
            opt_field!("status", "status");
            opt_field!("priority", "priority");
            opt_field!("assigned_to", "assigned_to");
            opt_field!("blocked_reason", "blocked_reason");
            opt_field!("depends_on", "depends_on");
            // H9-G goal contract freeze: the mutable acceptance_criteria copy
            // is updatable here (store layer is identity-agnostic, matching
            // every other field above) — authorization lives at the caller
            // boundary. The dashboard RPC path (`handlers.rs::handle_tasks_update`)
            // is Operator-ACL-gated and forwards this field through. The
            // agent-facing MCP path (`mcp.rs::handle_tasks_update`) explicitly
            // refuses to forward this field for `goal_mode` tasks before
            // reaching this function, so an agent identity can never exercise
            // this branch on a frozen goal contract. `acceptance_criteria_baseline`
            // deliberately has NO opt_field entry — no code path updates it
            // after `insert_task`.
            opt_field!("acceptance_criteria", "acceptance_criteria");
            if let Some(v) = fields.get("tags").and_then(|v| v.as_str()) {
                binds.push(v.to_string());
                sets.push(format!("tags = ?{}", binds.len()));
            }
            // I-3b task list operations: archived/pinned are booleans, not
            // strings, so they bypass the `opt_field!` macro (which only
            // reads `.as_str()`) — same shape as the `tags` special-case
            // above. `handlers.rs::handle_tasks_archive/unarchive/pin/unpin`
            // are thin wrappers that funnel through this generic update path
            // (same pattern as the existing `tasks.assign` → `handle_tasks_update`
            // delegation), so the HS4 agent-binding ACL check above already
            // covers these writes — no separate authorization branch needed.
            if let Some(v) = fields.get("archived").and_then(|v| v.as_bool()) {
                binds.push((v as i64).to_string());
                sets.push(format!("archived = ?{}", binds.len()));
            }
            if let Some(v) = fields.get("pinned").and_then(|v| v.as_bool()) {
                binds.push((v as i64).to_string());
                sets.push(format!("pinned = ?{}", binds.len()));
            }

            // Auto-set completed_at when status changes to done
            if fields.get("status").and_then(|v| v.as_str()) == Some("done") {
                binds.push(Utc::now().to_rfc3339());
                sets.push(format!("completed_at = ?{}", binds.len()));
            }

            binds.push(id.to_string());
            let sql = format!(
                "UPDATE tasks SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );

            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            tx.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update task: {e}"))?;
            tx.commit().map_err(|e| format!("update task: commit: {e}"))?;
        }

        self.get_task(id).await
    }

    /// A1 (StateAct): persist the self-reported `pending_hypotheses`
    /// snapshot for a goal-mode task so the next dispatch's `<state>` block
    /// carries it forward. `None` clears the column. Deliberately a small
    /// direct UPDATE rather than routing through [`Self::update_task`]'s
    /// generic field whitelist — keeps this narrow, best-effort write path
    /// isolated from that method's cycle-checking / dependency-rewrite
    /// logic, which has nothing to do with this column.
    pub async fn set_goal_state_json(
        &self,
        id: &str,
        state_json: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE tasks SET goal_state_json = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, state_json, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("set goal_state_json: {e}"))?;
        Ok(())
    }

    /// M7: read-merge-write update of `tasks.goal_state_json`, holding the
    /// store's single connection `Mutex` across the whole read→mutate→write
    /// sequence so two concurrent callers merging DIFFERENT keys into the
    /// same task's snapshot cannot lose either other's write.
    ///
    /// The bug this fixes: `goal_state_json` is a single JSON blob shared by
    /// multiple independent writers (`goal_loop.rs::capture_round_state`
    /// writes `pending_hypotheses`; `dispatch_engine.rs`'s acceptance review
    /// writes `confirmed_facts` — see [`Self::set_goal_state_json`]'s
    /// callers). A writer that does read-then-`set_goal_state_json`-the-whole-blob
    /// outside any lock can race another writer touching a DIFFERENT field:
    /// whichever `UPDATE` lands second wins with a value it computed from a
    /// stale read, silently discarding the other writer's field. Holding this
    /// crate's single `conn` mutex across the read AND the write closes that
    /// window for any two callers that both go through this method.
    ///
    /// `f` receives a mutable `serde_json::Value` — guaranteed to be a JSON
    /// object — to edit in place; whatever it leaves behind is persisted.
    /// Missing / malformed / non-object stored JSON degrades to an empty
    /// `{}` object first (same "never fabricate, just start from nothing"
    /// contract [`crate::goal_state::GoalStateSnapshot::from_json`] uses)
    /// rather than failing the merge.
    ///
    /// Both writers go through this API: `capture_round_state`
    /// (`pending_hypotheses`) and `DispatchEngine::persist_confirmed_facts`
    /// (`confirmed_facts`) — each merge must touch only its own key so
    /// concurrent merges from the two drivers cannot clobber each other.
    pub async fn merge_goal_state_json(
        &self,
        id: &str,
        f: impl FnOnce(&mut serde_json::Value),
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        let current: Option<String> = conn
            .query_row(
                "SELECT goal_state_json FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| format!("merge_goal_state_json read: {e}"))?
            .flatten();
        let mut value: serde_json::Value = current
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !value.is_object() {
            value = serde_json::json!({});
        }
        f(&mut value);
        let new_json = serde_json::to_string(&value)
            .map_err(|e| format!("merge_goal_state_json serialize: {e}"))?;
        conn.execute(
            "UPDATE tasks SET goal_state_json = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, new_json, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("merge_goal_state_json write: {e}"))?;
        Ok(())
    }

    pub async fn remove_task(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let count = conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])
            .map_err(|e| format!("remove task: {e}"))?;
        Ok(count > 0)
    }

    // ── G1 durable dispatch ─────────────────────────────────
    //
    // Migration direction: cross-agent delegation is moving off the legacy
    // file IPC (`bus_queue.jsonl`, consumed by `dispatcher.rs`) onto this
    // durable SQLite lifecycle. The file rail stays as a compatibility path
    // (see `dispatch_engine.rs` header); NEW durable work goes through these
    // methods: `pending` → atomic claim → `in_progress` (leased) →
    // `done` / `review` (goal mode) / `failed` / `needs_human`.

    /// Atomically claim a `pending` task. Compare-and-set: only the caller
    /// whose `UPDATE` flips exactly one row wins — concurrent claimers on the
    /// same id get [`ClaimOutcome::NotClaimable`]. Sets the lease so a crashed
    /// worker is reclaimable.
    ///
    /// Dependency gating is enforced HERE, inside one IMMEDIATE transaction:
    /// a `pending` task whose `depends_on` ids are not all `done` returns
    /// [`ClaimOutcome::BlockedByDeps`] with the unmet ids — the deps check and
    /// the claim write cannot be raced apart, so the gate can't be bypassed
    /// (fail-closed: a dep referencing a missing task counts as unmet).
    pub async fn atomic_claim(
        &self,
        id: &str,
        agent_id: &str,
        now: &str,
        lease_expires_at: &str,
    ) -> Result<ClaimOutcome, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("atomic claim: begin: {e}"))?;

        // Load the claim-relevant state under the write lock.
        let row: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT status, claimed_by, depends_on FROM tasks WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| format!("atomic claim: load: {e}"))?;
        let Some((status, claimed_by, depends_on)) = row else {
            return Ok(ClaimOutcome::NotClaimable);
        };
        // `revising` (Iterative Kanban) is claimable exactly like `pending`: a
        // judge rejection parks the task there with claim/lease cleared, and the
        // goal loop re-dispatches it for the next round. Same fail-closed
        // dependency gate applies.
        if !matches!(status.as_str(), "pending" | "revising") || claimed_by.is_some() {
            return Ok(ClaimOutcome::NotClaimable);
        }

        // Dependency gate inside the same transaction (HIGH-1): every
        // depends_on id must be an existing task in status `done`.
        let deps = parse_depends_on(&depends_on);
        if !deps.is_empty() {
            let mut unmet: Vec<String> = Vec::new();
            for dep in &deps {
                let dep_status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM tasks WHERE id = ?1",
                        params![dep],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| format!("atomic claim: dep check: {e}"))?;
                if dep_status.as_deref() != Some("done") {
                    unmet.push(dep.clone());
                }
            }
            if !unmet.is_empty() {
                // Drop the transaction (rollback) — nothing was written.
                return Ok(ClaimOutcome::BlockedByDeps(unmet));
            }
        }

        let n = tx
            .execute(
                "UPDATE tasks
                    SET claimed_by = ?2, claimed_at = ?3, lease_expires_at = ?4,
                        lease_renewed_at = ?3,
                        status = 'in_progress', assigned_to = ?2, updated_at = ?3
                  WHERE id = ?1 AND status IN ('pending', 'revising') AND claimed_by IS NULL",
                params![id, agent_id, now, lease_expires_at],
            )
            .map_err(|e| format!("atomic claim: {e}"))?;
        tx.commit().map_err(|e| format!("atomic claim: commit: {e}"))?;
        Ok(if n == 1 {
            ClaimOutcome::Claimed
        } else {
            ClaimOutcome::NotClaimable
        })
    }

    /// Heartbeat: extend the lease of a task the caller currently holds.
    /// Guarded on `claimed_by` so a worker cannot renew someone else's lease.
    /// Also stamps `lease_renewed_at` — the renewal anchor zombie reclaim uses
    /// for its conservative grace window.
    pub async fn renew_lease(
        &self,
        id: &str,
        agent_id: &str,
        new_expiry: &str,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks SET lease_expires_at = ?3, lease_renewed_at = ?4, updated_at = ?4
                  WHERE id = ?1 AND claimed_by = ?2 AND status = 'in_progress'",
                params![id, agent_id, new_expiry, now],
            )
            .map_err(|e| format!("renew lease: {e}"))?;
        Ok(n == 1)
    }

    /// The set of task ids currently `done` — used for dependency gating.
    pub async fn done_task_ids(&self) -> Result<HashSet<String>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id FROM tasks WHERE status = 'done'")
            .map_err(|e| format!("prepare done ids: {e}"))?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("query done ids: {e}"))?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|e| format!("collect done ids: {e}"))?;
        Ok(ids)
    }

    /// All tasks in a given status (helper for the dispatcher's review pass).
    pub async fn tasks_in_status(&self, status: &str) -> Result<Vec<TaskRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE status = ?1 ORDER BY created_at ASC"
            ))
            .map_err(|e| format!("prepare status query: {e}"))?;
        let rows = stmt
            .query_map(params![status], row_to_task)
            .map_err(|e| format!("query status: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect status: {e}"))?;
        Ok(rows)
    }

    /// Pending tasks that are claimable *right now*: unclaimed, not
    /// archived, and with every `depends_on` id already `done`. Dependency
    /// filtering is done in Rust (parsing the JSON array) against the
    /// current `done` set. Archiving a still-pending/unclaimed task (the
    /// `/goals` board "take out of active consideration" action) must
    /// remove it from the dispatch engine's pickup queue, same as it's
    /// already hidden from `list_tasks_filtered`'s default view.
    pub async fn claimable_tasks(&self) -> Result<Vec<TaskRow>, String> {
        let done = self.done_task_ids().await?;
        let pending = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {TASK_COLUMNS} FROM tasks
                      WHERE status = 'pending' AND claimed_by IS NULL AND archived = 0
                      ORDER BY created_at ASC"
                ))
                .map_err(|e| format!("prepare claimable: {e}"))?;
            stmt.query_map([], row_to_task)
                .map_err(|e| format!("query claimable: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("collect claimable: {e}"))?
        };
        Ok(pending
            .into_iter()
            .filter(|t| deps_satisfied(&parse_depends_on(&t.depends_on), &done))
            .collect())
    }

    /// Reclaim zombie tasks: `in_progress` rows with a non-null, elapsed lease.
    /// Retries remaining → requeue to `pending` (lease/claim cleared,
    /// `retry_count` incremented); budget exhausted → `failed`. Tasks with a
    /// NULL lease (manual board tasks) are never touched.
    pub async fn reclaim_zombies(&self, now: &str) -> Result<Vec<ZombieOutcome>, String> {
        // Load candidates first, decide in Rust (robust RFC3339 comparison),
        // then apply guarded updates.
        let candidates: Vec<TaskRow> = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {TASK_COLUMNS} FROM tasks
                      WHERE status = 'in_progress'
                        AND lease_expires_at IS NOT NULL
                        AND claimed_by IS NOT NULL"
                ))
                .map_err(|e| format!("prepare zombie scan: {e}"))?;
            stmt.query_map([], row_to_task)
                .map_err(|e| format!("query zombie scan: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("collect zombie scan: {e}"))?
        };

        let mut outcomes = Vec::new();
        for t in candidates {
            let Some(lease) = t.lease_expires_at.as_deref() else {
                continue;
            };
            // Conservative reclaim: lease expired AND no renewal arrived within
            // a further full lease window (anchor = last renewal, or the claim
            // itself). A worker whose renewal ticker is still alive keeps
            // pushing `lease_expires_at` forward and is never reclaimed.
            let anchor = t.lease_renewed_at.as_deref().or(t.claimed_at.as_deref());
            if !zombie_reclaim_due(lease, anchor, now) {
                continue;
            }
            let claimer = t.claimed_by.clone().unwrap_or_default();
            match zombie_action(t.retry_count, t.max_retries) {
                ZombieAction::Requeue => {
                    let new_retry = t.retry_count + 1;
                    if self
                        .requeue_zombie_cas(&t.id, &claimer, lease, new_retry, now)
                        .await?
                    {
                        outcomes.push(ZombieOutcome {
                            task_id: t.id,
                            action: ZombieAction::Requeue,
                            retry_count: new_retry,
                        });
                    }
                }
                ZombieAction::Fail => {
                    if self.fail_zombie_cas(&t.id, &claimer, lease, now).await? {
                        outcomes.push(ZombieOutcome {
                            task_id: t.id,
                            action: ZombieAction::Fail,
                            retry_count: t.retry_count,
                        });
                    }
                }
            }
        }
        Ok(outcomes)
    }

    /// Requeue one zombie. Optimistic CAS on `lease_expires_at` (the value the
    /// zombie scan observed): a renewal that lands between scan and write moves
    /// the lease forward, the CAS misses, and the live worker keeps its claim.
    pub(crate) async fn requeue_zombie_cas(
        &self,
        id: &str,
        claimer: &str,
        scanned_lease: &str,
        new_retry: i64,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'pending', claimed_by = NULL, claimed_at = NULL,
                        lease_expires_at = NULL, retry_count = ?2, updated_at = ?3
                  WHERE id = ?1 AND claimed_by = ?4 AND status = 'in_progress'
                    AND lease_expires_at = ?5",
                params![id, new_retry, now, claimer, scanned_lease],
            )
            .map_err(|e| format!("requeue zombie: {e}"))?;
        Ok(n == 1)
    }

    /// Fail one zombie whose retry budget is spent. Same `lease_expires_at`
    /// CAS as [`Self::requeue_zombie_cas`] so a racing renewal is never failed.
    pub(crate) async fn fail_zombie_cas(
        &self,
        id: &str,
        claimer: &str,
        scanned_lease: &str,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'failed', lease_expires_at = NULL,
                        blocked_reason = ?2, updated_at = ?3
                  WHERE id = ?1 AND claimed_by = ?4 AND status = 'in_progress'
                    AND lease_expires_at = ?5",
                params![
                    id,
                    "lease expired; retry budget exhausted",
                    now,
                    claimer,
                    scanned_lease
                ],
            )
            .map_err(|e| format!("fail zombie: {e}"))?;
        Ok(n == 1)
    }

    /// Worker completion. Goal-mode tasks route to `review` (judge acceptance
    /// pending) carrying the result summary; others go straight to `done`.
    /// Returns the updated row, or `None` if the task does not exist.
    ///
    /// **Holder guard (HIGH-2):** a task with a non-null `claimed_by` can only
    /// be completed by that holder — `caller` must match, or the call errors.
    /// A reclaimed zombie worker therefore cannot clobber the result of the
    /// worker the task was re-dispatched to. Unclaimed / legacy board tasks
    /// (`claimed_by IS NULL`) keep the pre-guard behavior: any caller may
    /// complete them. Read-check-write runs in one IMMEDIATE transaction.
    pub async fn complete_task(
        &self,
        id: &str,
        summary: &str,
        caller: &str,
    ) -> Result<Option<TaskRow>, String> {
        let now = Utc::now().to_rfc3339();
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("complete: begin: {e}"))?;
            let row: Option<(bool, Option<String>, i64, Option<String>, String)> = tx
                .query_row(
                    "SELECT goal_mode, claimed_by, revision_round, claimed_at, created_at
                       FROM tasks WHERE id = ?1",
                    params![id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)? != 0,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| format!("complete: load: {e}"))?;
            let Some((goal_mode, claimed_by, revision_round, claimed_at, created_at)) = row else {
                return Ok(None);
            };
            if let Some(holder) = claimed_by.as_deref() {
                if holder != caller {
                    return Err(format!(
                        "task {id} is claimed by '{holder}'; only the claim holder may complete it (caller: '{caller}')"
                    ));
                }
            }
            // Guard: never overwrite a task that has already reached a terminal
            // state. Without this, a stale worker (e.g. one whose lease was
            // reclaimed and reassigned) could clobber the authoritative result
            // by calling complete on an already-`done`/`cancelled` task.
            if goal_mode {
                let affected = tx
                    .execute(
                        "UPDATE tasks
                        SET status = 'review', result_summary = ?2,
                            lease_expires_at = NULL, updated_at = ?3
                      WHERE id = ?1 AND status NOT IN ('done', 'cancelled')",
                        params![id, summary, now],
                    )
                    .map_err(|e| format!("complete (review): {e}"))?;
                // Iterative Kanban: stamp this round's submission and add the
                // per-round agent seconds (submitted − dispatched) to the task's
                // cumulative agent clock. Only when the completion actually took
                // effect (a terminal-state clobber attempt records nothing).
                if affected == 1 {
                    let fallback_dispatch = claimed_at.as_deref().unwrap_or(&created_at);
                    let secs = iter_submit_conn(
                        &tx,
                        id,
                        &now,
                        revision_round + 1,
                        fallback_dispatch,
                    )?;
                    if secs > 0 {
                        tx.execute(
                            "UPDATE tasks SET agent_seconds = agent_seconds + ?2 WHERE id = ?1",
                            params![id, secs],
                        )
                        .map_err(|e| format!("complete (agent_seconds): {e}"))?;
                    }
                }
            } else {
                tx.execute(
                    "UPDATE tasks
                        SET status = 'done', result_summary = ?2,
                            completed_at = ?3, lease_expires_at = NULL, updated_at = ?3
                      WHERE id = ?1 AND status NOT IN ('done', 'cancelled')",
                    params![id, summary, now],
                )
                .map_err(|e| format!("complete (done): {e}"))?;
            }
            tx.commit().map_err(|e| format!("complete: commit: {e}"))?;
        }
        self.get_task(id).await
    }

    /// Goal-mode acceptance passed: promote a `review` task to `done`.
    pub async fn accept_review(&self, id: &str, feedback: &str) -> Result<bool, String> {
        self.accept_review_with_verdict(id, feedback, None).await
    }

    /// [`Self::accept_review`] carrying the structured per-aspect panel
    /// verdict (`[{name, pass, reason}]` JSON) for the round timeline.
    pub async fn accept_review_with_verdict(
        &self,
        id: &str,
        feedback: &str,
        verdict_json: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'done', completed_at = ?2, judge_feedback = ?3, updated_at = ?2
                  WHERE id = ?1 AND status = 'review'",
                params![id, now, feedback],
            )
            .map_err(|e| format!("accept review: {e}"))?;
        if n == 1 {
            // Iterative Kanban: seal the current round's verdict. No
            // `worker_excerpt` snapshot needed on the accept path — an
            // accepted task never re-enters `needs_human`, so this round can
            // never be a WP-4F best-round candidate.
            iter_verdict_conn(&conn, id, "accepted", feedback, verdict_json, None, &now)?;
        }
        Ok(n == 1)
    }

    /// Goal-mode acceptance rejected. Iterative Kanban: send the task to the new
    /// `revising` state (not `pending`) for another round — claim/lease cleared,
    /// `revision_round` incremented, `diminishing` flag raised once the round
    /// count reaches `soft_cap` (the loop is NOT blocked, only flagged). When the
    /// judge retry budget (`max_retries`) is exhausted, escalate to `needs_human`
    /// instead (fail-safe — never loops indefinitely). Returns the status applied.
    ///
    /// `soft_cap` is the goal loop's soft cap (default 3); the diminishing flag
    /// only affects dashboard presentation, never dispatch.
    pub async fn reject_review(
        &self,
        id: &str,
        feedback: &str,
        soft_cap: i64,
    ) -> Result<String, String> {
        self.reject_review_with_verdict(id, feedback, soft_cap, None).await
    }

    /// [`Self::reject_review`] carrying the structured per-aspect panel
    /// verdict for the round timeline (`None` for deterministic pre-judge
    /// rejections, which have no panel).
    pub async fn reject_review_with_verdict(
        &self,
        id: &str,
        feedback: &str,
        soft_cap: i64,
        verdict_json: Option<&str>,
    ) -> Result<String, String> {
        let row = match self.get_task(id).await? {
            Some(r) => r,
            None => return Err(format!("task not found: {id}")),
        };
        let now = Utc::now().to_rfc3339();
        // WP-4F: snapshot THIS round's own worker output before it is wiped
        // (the "revising" branch below nulls `result_summary`) — the only
        // point in the whole rejection flow where the round's real output is
        // still readable. Bounded + CJK-safe so a multi-KB agent reply never
        // balloons the iteration history row. `None` when the round produced
        // no result text at all.
        let worker_excerpt: Option<String> = row
            .result_summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                duduclaw_core::truncate_bytes(
                    s,
                    crate::goal_budget_best_round::WORKER_EXCERPT_MAX_BYTES,
                )
                .to_string()
            });
        let conn = self.conn.lock().await;
        if row.retry_count < row.max_retries {
            let new_retry = row.retry_count + 1;
            let new_round = row.revision_round + 1;
            let diminishing = new_round >= soft_cap.max(1);
            let n = conn
                .execute(
                    "UPDATE tasks
                    SET status = 'revising', claimed_by = NULL, claimed_at = NULL,
                        lease_expires_at = NULL, retry_count = ?2, revision_round = ?3,
                        diminishing = ?4, judge_feedback = ?5, result_summary = NULL,
                        updated_at = ?6
                  WHERE id = ?1 AND status = 'review'",
                    params![id, new_retry, new_round, diminishing as i64, feedback, now],
                )
                .map_err(|e| format!("reject review (revising): {e}"))?;
            if n == 1 {
                iter_verdict_conn(
                    &conn,
                    id,
                    "rejected",
                    feedback,
                    verdict_json,
                    worker_excerpt.as_deref(),
                    &now,
                )?;
            }
            Ok("revising".to_string())
        } else {
            // H11: the retry budget is spent — a hard cap fired, not a fresh
            // blocker. Stamped here (not derived from `feedback`, which is
            // judge-authored prose) so the dashboard/channel chip is exact.
            let n = conn
                .execute(
                    "UPDATE tasks
                    SET status = 'needs_human', judge_feedback = ?2, pause_reason = ?4,
                        updated_at = ?3
                  WHERE id = ?1 AND status = 'review'",
                    params![
                        id,
                        feedback,
                        now,
                        crate::pause_reason::PauseReason::BudgetExhausted.as_str()
                    ],
                )
                .map_err(|e| format!("reject review (escalate): {e}"))?;
            if n == 1 {
                iter_verdict_conn(
                    &conn,
                    id,
                    "escalated",
                    feedback,
                    verdict_json,
                    worker_excerpt.as_deref(),
                    &now,
                )?;

                // WP-4F: this is the OTHER budget-exhausted escalation site
                // (the judge retry budget, `max_retries` — distinct from
                // `goal_loop::GoalLoopDriver::escalate`'s iteration-cap /
                // wall-clock checks, but the same `PauseReason::
                // BudgetExhausted` family per `pause_reason.rs`'s own doc
                // comment: "iteration cap, judge retry budget, the global
                // wall clock, or a per-goal deadline_at"). Attach the
                // closest-to-done round instead of leaving `judge_feedback`
                // as the bare last-round rejection text. Best-effort: any
                // failure below leaves `judge_feedback` exactly as already
                // written above, never blocks the escalation itself.
                if let Ok(iterations) = list_iterations_conn(&conn, id) {
                    if let Some(pick) = crate::goal_budget_best_round::pick_best_round(&iterations)
                    {
                        let enriched =
                            crate::goal_budget_best_round::compose_escalation_note(feedback, &pick);
                        let _ = conn.execute(
                            "UPDATE tasks SET judge_feedback = ?2 WHERE id = ?1",
                            params![id, enriched],
                        );
                    }
                }
            }
            Ok("needs_human".to_string())
        }
    }

    /// Fail-safe escalation: park a task for human attention without killing or
    /// looping it. Used when the judge itself errors (goal mode).
    ///
    /// H11: leaves `pause_reason` unclassified ([`PauseReason::Unknown`] at
    /// read time). Production escalation paths call
    /// [`Self::mark_needs_human_with_pause`] instead — this string-only form
    /// is kept for callers (and tests) that genuinely have no class to
    /// declare, and its fallback is the *safe* direction (「需要人工確認」).
    pub async fn mark_needs_human(&self, id: &str, reason: &str) -> Result<bool, String> {
        self.mark_needs_human_with_pause(id, reason, crate::pause_reason::PauseReason::Unknown)
            .await
    }

    /// H11: [`Self::mark_needs_human`] carrying the structured pause class.
    ///
    /// The class is supplied by the caller because only the call site knows
    /// the trigger statically — three of the `reason` strings this stores are
    /// built from LLM output or a transport error, so classifying them by
    /// substring afterwards would be a routing decision made on model-authored
    /// prose (coding convention 2). [`PauseReason::Unknown`] is written as a
    /// real token rather than `NULL` so "explicitly unclassified" and "row
    /// predates the column" are the same at read time and neither can be
    /// mistaken for a confident class.
    pub async fn mark_needs_human_with_pause(
        &self,
        id: &str,
        reason: &str,
        pause: crate::pause_reason::PauseReason,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks SET status = 'needs_human', judge_feedback = ?2, pause_reason = ?4,
                        updated_at = ?3
                  WHERE id = ?1",
                params![id, reason, now, pause.as_str()],
            )
            .map_err(|e| format!("mark needs_human: {e}"))?;
        Ok(n > 0)
    }

    /// I-1c "想一想": consume the pending plan-first plan after
    /// [`crate::goal_loop::GoalLoopDriver::enqueue_work`] has injected it into
    /// a round's dispatch payload, so it is injected exactly once (the first
    /// round after approval) rather than on every subsequent round. Not
    /// gated on task status — the caller (the driver, right after a
    /// successful enqueue) already knows this is the correct moment; a
    /// failed clear here is harmless (the plan is simply re-injected next
    /// dispatch, which repeats guidance rather than losing anything).
    pub async fn clear_plan_pending(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE tasks SET plan_pending = NULL WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("clear plan_pending: {e}"))?;
        Ok(())
    }

    /// P2a: apply a human decision to a `needs_human` goal task from a channel
    /// button. Only transitions FROM `needs_human` (fail-closed + idempotent:
    /// a second press on an already-resolved task affects 0 rows → `Ok(false)`).
    ///
    /// - `retry` → back to `pending`, claim/lease/result cleared. `note`
    ///   (optional human instruction) is written to `judge_feedback` so the next
    ///   driver dispatch carries it; an empty note clears `judge_feedback`.
    /// - `done`  → `done` + `completed_at`, `note` recorded in `judge_feedback`.
    /// - `abort` → `cancelled`, `note` recorded in `judge_feedback`.
    ///
    /// An unrecognised `decision` is rejected (never silently coerced).
    pub async fn resolve_needs_human(
        &self,
        id: &str,
        decision: &str,
        note: &str,
    ) -> Result<bool, String> {
        let note_opt: Option<&str> = if note.trim().is_empty() { None } else { Some(note) };
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        // H11: the pause is over on every branch — clear the class so a task
        // sent back around the loop (or closed out) never renders a stale
        // 「卡住沒進展」chip from the pause a human just resolved.
        let n = match decision {
            "retry" => conn
                .execute(
                    "UPDATE tasks
                        SET status = 'pending', claimed_by = NULL, claimed_at = NULL,
                            lease_expires_at = NULL, result_summary = NULL,
                            judge_feedback = ?2, pause_reason = NULL, updated_at = ?3
                      WHERE id = ?1 AND status = 'needs_human'",
                    params![id, note_opt, now],
                )
                .map_err(|e| format!("resolve needs_human (retry): {e}"))?,
            "done" => conn
                .execute(
                    "UPDATE tasks
                        SET status = 'done', completed_at = ?3, judge_feedback = ?2,
                            pause_reason = NULL, updated_at = ?3
                      WHERE id = ?1 AND status = 'needs_human'",
                    params![id, note_opt, now],
                )
                .map_err(|e| format!("resolve needs_human (done): {e}"))?,
            "abort" => conn
                .execute(
                    "UPDATE tasks
                        SET status = 'cancelled', judge_feedback = ?2, pause_reason = NULL,
                            updated_at = ?3
                      WHERE id = ?1 AND status = 'needs_human'",
                    params![id, note_opt, now],
                )
                .map_err(|e| format!("resolve needs_human (abort): {e}"))?,
            other => return Err(format!("unknown needs_human decision: {other}")),
        };
        Ok(n == 1)
    }

    /// I-3a: reopen a `done` / `failed` / `cancelled` **goal-mode** task for
    /// another round, carrying the user's follow-up message into the next
    /// dispatch's prompt — WorkBuddy's "a finished/failed task can take a
    /// follow-up message" pattern (`DESIGN-dashboard-ux-workbuddy-2026-08.md`
    /// §3.3, backlog item I-3a). Deliberately a separate method from
    /// [`Self::resolve_needs_human`] rather than widening its `retry` arm:
    /// that method also backs the **channel** decision buttons
    /// ([`crate::goal_notify::apply_needs_human`]), and a channel card is
    /// only ever rendered while a task sits in `needs_human` — widening its
    /// WHERE clause would let a stale button, pressed after the task later
    /// reached `done` through a legitimate unrelated path, silently reopen
    /// it. This method is reachable only from the dashboard's explicit
    /// "接著做" action (`tasks.goal_decide` with `action: "continue"`).
    ///
    /// `message` is required (unlike the optional `note` on
    /// `resolve_needs_human`'s retry) — "continue with nothing to add" is
    /// just `retry`, which already exists for `needs_human`. The message is
    /// stamped with [`CONTINUE_MESSAGE_PREFIX`] so the next dispatch's
    /// prompt-builder ([`crate::goal_loop::GoalLoopDriver::enqueue_work`])
    /// can tell it apart from a genuine judge-rejection `judge_feedback` and
    /// phrase the two differently — without the marker, a continued task
    /// would be told "your last round failed review", which is simply false
    /// for a task that had actually succeeded.
    ///
    /// `revision_round` / `agent_seconds` / `diminishing` are deliberately
    /// left untouched so the round counter and dual-clock history continue
    /// rather than reset (the design doc's "iteration 計數延續"
    /// requirement) — same reasoning as `resolve_needs_human`'s retry arm,
    /// which never touches them either. `completed_at` IS cleared: a
    /// `pending` task carrying a stale completion timestamp from a previous
    /// `done` round would misrepresent the row.
    pub async fn continue_from_terminal(&self, id: &str, message: &str) -> Result<bool, String> {
        let message = message.trim();
        if message.is_empty() {
            return Err("接著做需要附上訊息".into());
        }
        let stamped = format!("{CONTINUE_MESSAGE_PREFIX}{message}");
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'pending', claimed_by = NULL, claimed_at = NULL,
                        lease_expires_at = NULL, result_summary = NULL, completed_at = NULL,
                        judge_feedback = ?2, updated_at = ?3
                  WHERE id = ?1 AND status IN ('done', 'failed', 'cancelled')
                    AND COALESCE(goal_mode, 0) = 1",
                params![id, stamped, now],
            )
            .map_err(|e| format!("continue from terminal: {e}"))?;
        Ok(n == 1)
    }

    /// W1-5: mark a `needs_human` goal task as claimed by a human decider —
    /// the "Take over" half of the Submit/Take over pair (D6, Intercom
    /// `Loop in teammate`). Deliberately does NOT change `status`: a task
    /// sitting in `needs_human` is already excluded from
    /// `GoalLoopDriver::tick_once`'s dispatch-candidate query (only
    /// `todo`/`pending`/`revising` are ever picked up), so no further state
    /// is needed to stop the automatic loop from retrying it — the "stop
    /// auto-retry" half of takeover is a side effect of the task already
    /// being parked, not something this method has to enforce.
    ///
    /// Reuses the existing `claimed_by` column (the one worker-lease claims
    /// use elsewhere): safe to share because a `needs_human` row is never
    /// itself a claim/lease candidate (`claim_task` only matches
    /// `pending`/`revising`), so the two meanings never collide. Idempotent
    /// and repeatable — unlike [`Self::resolve_needs_human`] there is no
    /// terminal state to race against, so a second (or a different
    /// authorized decider's) take-over press simply re-stamps `claimed_by`.
    ///
    /// This is the button-driven half of takeover: it stops the loop for one
    /// parked task and records who is handling it by hand. Taking over the
    /// **conversation** — pausing inbound AI replies and every scheduled
    /// dispatch aimed at it — is W3-1's
    /// [`crate::takeover`] / [`duduclaw_core::takeover_state`], which claims
    /// the conversation's live goal tasks through
    /// [`Self::claim_conversation_tasks`] instead.
    pub async fn claim_needs_human(&self, id: &str, decider: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks SET claimed_by = ?2, updated_at = ?3
                  WHERE id = ?1 AND status = 'needs_human'",
                params![id, decider, now],
            )
            .map_err(|e| format!("claim needs_human: {e}"))?;
        Ok(n == 1)
    }

    /// W3-1 (D4): stamp `claimed_by` on every live goal task that came from
    /// one channel conversation, and return the ids that were stamped.
    ///
    /// This is step 2 of the atomic three-in-one a takeover performs (pause
    /// the conversation, claim its work, post to the Activity Feed). Without
    /// it, a human who takes over a conversation still shows up on the board
    /// as "the AI is on it", and the next person to look at the task has no
    /// way to know somebody is already handling it by hand.
    ///
    /// Scope is deliberately narrow:
    /// - **goal tasks only** (`goal_mode = 1`) — an ordinary board task is not
    ///   driven by this conversation and must not be silently reassigned.
    /// - **non-terminal only** — a finished task's `claimed_by` is history.
    /// - **unclaimed or already this decider's** — one human taking over must
    ///   not steal a row another worker holds a lease on
    ///   ([`Self::claim_task`]'s meaning of the same column).
    pub async fn claim_conversation_tasks(
        &self,
        channel: &str,
        chat_id: &str,
        decider: &str,
    ) -> Result<Vec<String>, String> {
        if channel.trim().is_empty() || chat_id.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM tasks
                  WHERE COALESCE(goal_mode, 0) = 1
                    AND source_channel = ?1 AND source_chat_id = ?2
                    AND status NOT IN ('done', 'cancelled', 'failed')
                    AND (claimed_by IS NULL OR claimed_by = ?3)",
            )
            .map_err(|e| format!("claim conversation tasks (prepare): {e}"))?;
        let ids: Vec<String> = stmt
            .query_map(params![channel.trim(), chat_id.trim(), decider], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| format!("claim conversation tasks (query): {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        for id in &ids {
            conn.execute(
                "UPDATE tasks SET claimed_by = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, decider, now],
            )
            .map_err(|e| format!("claim conversation tasks (update {id}): {e}"))?;
        }
        Ok(ids)
    }

    /// P2a: cancel a task that has not reached a terminal state. Used by the
    /// goal-loop kickoff gate when a human denies (or lets the approval expire)
    /// before the first dispatch. Idempotent: a task already
    /// `done`/`cancelled`/`failed` is left untouched (returns `Ok(false)`).
    pub async fn cancel_task(&self, id: &str, reason: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks
                    SET status = 'cancelled', judge_feedback = ?2, updated_at = ?3
                  WHERE id = ?1 AND status NOT IN ('done', 'cancelled', 'failed')",
                params![id, reason, now],
            )
            .map_err(|e| format!("cancel task: {e}"))?;
        Ok(n == 1)
    }

    // ── Iterative Kanban: iteration detail (v1.45) ──────────

    /// Open a work round for a goal-mode task (called by the goal loop driver on
    /// dispatch). Idempotent per `(task_id, round)`: a stall re-dispatch of the
    /// same round is a no-op, so a round row is created exactly once.
    pub async fn record_iteration_dispatch(
        &self,
        task_id: &str,
        round: i64,
        now: &str,
    ) -> Result<(), String> {
        self.record_iteration_dispatch_with_state(task_id, round, now, None, None)
            .await
    }

    /// Dispatch record carrying the visit-graph signal of the moment: the
    /// goal-state hash and the same-(state, action) repeat streak. A stall
    /// re-dispatch of an already-open round increments `dispatch_count`
    /// (previously that count lived only in the driver's memory) and
    /// refreshes the state signal.
    pub async fn record_iteration_dispatch_with_state(
        &self,
        task_id: &str,
        round: i64,
        now: &str,
        state_hash: Option<&str>,
        repeat_streak: Option<i64>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        iter_dispatch_conn(&conn, task_id, round, now, state_hash, repeat_streak)
    }

    /// All iteration rows for a task, oldest round first (the revision timeline).
    pub async fn list_iterations(&self, task_id: &str) -> Result<Vec<TaskIterationRow>, String> {
        let conn = self.conn.lock().await;
        list_iterations_conn(&conn, task_id)
    }

    /// Per-agent + board-level flow metrics for the Iterative Kanban analytics
    /// (P2). Computed over goal-mode tasks:
    /// - `first_pass_yield`: fraction of finished (`done`) goal tasks accepted on
    ///   round 1 (`revision_round == 0`);
    /// - `avg_rounds`: mean `revision_round + 1` over finished goal tasks;
    /// - `avg_agent_seconds` / `avg_cycle_seconds`: the dual clock means;
    /// - `review_queue_depth`: goal tasks currently in `review`.
    ///
    /// Board level also returns the `review` WIP total and the 7-day acceptance
    /// throughput (for the Little's-Law wait estimate). `accepts_last_7d` counts
    /// goal tasks whose `completed_at` is within the last 7 days.
    pub async fn flow_metrics(&self, now: &str) -> Result<FlowMetrics, String> {
        let tasks = self.list_tasks(None, None, None).await?;
        let cutoff = DateTime::parse_from_rfc3339(now)
            .map(|n| n.with_timezone(&Utc) - chrono::Duration::days(7));

        use std::collections::BTreeMap;
        let mut per: BTreeMap<String, AgentFlowAccum> = BTreeMap::new();
        let mut review_depth = 0i64;
        let mut accepts_7d = 0i64;

        for t in &tasks {
            if !t.goal_mode {
                continue;
            }
            let e = per.entry(t.assigned_to.clone()).or_default();
            if t.status == "review" {
                review_depth += 1;
                e.review_queue_depth += 1;
            }
            if t.status == "done" {
                e.finished += 1;
                e.sum_rounds += t.revision_round + 1;
                e.sum_agent_secs += t.agent_seconds;
                if t.revision_round == 0 {
                    e.first_pass += 1;
                }
                if let (Some(done), Ok(cut)) = (t.completed_at.as_deref(), &cutoff) {
                    if let Ok(d) = DateTime::parse_from_rfc3339(done) {
                        let cycle = (d.with_timezone(&Utc)
                            - DateTime::parse_from_rfc3339(&t.created_at)
                                .map(|c| c.with_timezone(&Utc))
                                .unwrap_or_else(|_| d.with_timezone(&Utc)))
                        .num_seconds()
                        .max(0);
                        e.sum_cycle_secs += cycle;
                        if d.with_timezone(&Utc) >= *cut {
                            accepts_7d += 1;
                        }
                    }
                }
            }
        }

        let agents = per
            .into_iter()
            .map(|(agent_id, a)| AgentFlow {
                agent_id,
                goal_tasks: a.finished + a.review_queue_depth,
                finished: a.finished,
                first_pass_yield: if a.finished > 0 {
                    a.first_pass as f64 / a.finished as f64
                } else {
                    0.0
                },
                avg_rounds: if a.finished > 0 {
                    a.sum_rounds as f64 / a.finished as f64
                } else {
                    0.0
                },
                avg_agent_seconds: if a.finished > 0 {
                    a.sum_agent_secs as f64 / a.finished as f64
                } else {
                    0.0
                },
                avg_cycle_seconds: if a.finished > 0 {
                    a.sum_cycle_secs as f64 / a.finished as f64
                } else {
                    0.0
                },
                review_queue_depth: a.review_queue_depth,
            })
            .collect();

        Ok(FlowMetrics {
            agents,
            review_queue_depth: review_depth,
            accepts_last_7d: accepts_7d,
            avg_daily_accepts_7d: accepts_7d as f64 / 7.0,
        })
    }

    // ── G8 goal chain ───────────────────────────────────────

    /// Insert a goal. Fail-closed validation at the single write boundary:
    /// a non-null `parent_goal_id` must reference an existing goal and must not
    /// close a cycle in the parent graph (visited-set walk). Check + write run
    /// in one IMMEDIATE transaction so they cannot be raced apart (TOCTOU).
    pub async fn insert_goal(&self, row: &GoalRow) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("insert goal: begin: {e}"))?;
        if let Some(parent) = row.parent_goal_id.as_deref() {
            if get_goal_conn(&tx, parent)?.is_none() {
                return Err(format!("parent goal not found: {parent}"));
            }
            let edges = goal_parent_edges_conn(&tx)?;
            if introduces_parent_cycle(&edges, &row.id, parent) {
                return Err(format!(
                    "goal cycle rejected: {} → {} would close a loop",
                    row.id, parent
                ));
            }
        }
        tx.execute(
            "INSERT INTO goals (id, title, description, parent_goal_id, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.id,
                row.title,
                row.description,
                row.parent_goal_id,
                row.status,
                row.created_at,
            ],
        )
        .map_err(|e| format!("insert goal: {e}"))?;
        tx.commit().map_err(|e| format!("insert goal: commit: {e}"))?;
        Ok(())
    }

    pub async fn get_goal(&self, id: &str) -> Result<Option<GoalRow>, String> {
        let conn = self.conn.lock().await;
        get_goal_conn(&conn, id)
    }

    pub async fn list_goals(&self, status: Option<&str>) -> Result<Vec<GoalRow>, String> {
        let conn = self.conn.lock().await;
        let (sql, binds): (String, Vec<String>) = match status {
            Some(s) => (
                "SELECT id, title, description, parent_goal_id, status, created_at
                   FROM goals WHERE status = ?1 ORDER BY created_at ASC"
                    .into(),
                vec![s.to_string()],
            ),
            None => (
                "SELECT id, title, description, parent_goal_id, status, created_at
                   FROM goals ORDER BY created_at ASC"
                    .into(),
                Vec::new(),
            ),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare goals: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_goal)
            .map_err(|e| format!("query goals: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect goals: {e}"))?;
        Ok(rows)
    }

    /// Update mutable goal fields. Re-parenting goes through the same
    /// fail-closed cycle gate as `insert_goal`, inside one IMMEDIATE
    /// transaction (check + write cannot be raced apart — TOCTOU).
    pub async fn update_goal(
        &self,
        id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<GoalRow>, String> {
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("update goal: begin: {e}"))?;
            if let Some(new_parent) = fields.get("parent_goal_id").and_then(|v| v.as_str()) {
                if get_goal_conn(&tx, new_parent)?.is_none() {
                    return Err(format!("parent goal not found: {new_parent}"));
                }
                let edges = goal_parent_edges_conn(&tx)?;
                if introduces_parent_cycle(&edges, id, new_parent) {
                    return Err(format!(
                        "goal cycle rejected: {id} → {new_parent} would close a loop"
                    ));
                }
            }
            let mut sets: Vec<String> = Vec::new();
            let mut binds: Vec<String> = Vec::new();
            for key in ["title", "description", "status", "parent_goal_id"] {
                if let Some(v) = fields.get(key).and_then(|v| v.as_str()) {
                    binds.push(v.to_string());
                    sets.push(format!("{key} = ?{}", binds.len()));
                }
            }
            if sets.is_empty() {
                return Err("no goal fields to update".into());
            }
            binds.push(id.to_string());
            let sql = format!("UPDATE goals SET {} WHERE id = ?{}", sets.join(", "), binds.len());
            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            tx.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update goal: {e}"))?;
            tx.commit().map_err(|e| format!("update goal: commit: {e}"))?;
        }
        self.get_goal(id).await
    }

    /// All `(goal_id, parent_goal_id)` edges — for cycle detection.
    pub async fn goal_parent_edges(&self) -> Result<Vec<(String, Option<String>)>, String> {
        let conn = self.conn.lock().await;
        goal_parent_edges_conn(&conn)
    }

    /// Walk a goal's ancestry root-first (Initiative → Project → Issue).
    /// Visited-set + depth cap make the walk loop-proof even on corrupted data
    /// (the chain is truncated, never spun). Unknown id ⇒ empty vec.
    pub async fn goal_ancestry(&self, goal_id: &str) -> Result<Vec<GoalRow>, String> {
        let mut chain: Vec<GoalRow> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cur = Some(goal_id.to_string());
        while let Some(id) = cur {
            if chain.len() >= GOAL_ANCESTRY_MAX_DEPTH || !seen.insert(id.clone()) {
                break; // depth cap / loop guard — fail-safe truncation
            }
            let Some(goal) = self.get_goal(&id).await? else {
                break;
            };
            cur = goal.parent_goal_id.clone();
            chain.push(goal);
        }
        chain.reverse(); // walked leaf→root; present root-first
        Ok(chain)
    }

    // ── Dependency graph (depends_on) ───────────────────────

    /// All `(task_id, depends_on ids)` edges — for dependency cycle detection.
    pub async fn depends_edges(&self) -> Result<Vec<(String, Vec<String>)>, String> {
        let conn = self.conn.lock().await;
        depends_edges_conn(&conn)
    }

    // ── Activity feed ───────────────────────────────────────

    pub async fn append_activity(&self, row: &ActivityRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO activity (id, event_type, agent_id, task_id, summary, timestamp, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.id,
                row.event_type,
                row.agent_id,
                row.task_id,
                row.summary,
                row.timestamp,
                row.metadata,
            ],
        )
        .map_err(|e| format!("append activity: {e}"))?;
        Ok(())
    }

    pub async fn list_activity(
        &self,
        agent_id: Option<&str>,
        event_type: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<ActivityRow>, i64), String> {
        let conn = self.conn.lock().await;

        // Count total
        let mut count_sql = "SELECT COUNT(*) FROM activity WHERE 1=1".to_string();
        let mut query_sql = "SELECT id, event_type, agent_id, task_id, summary, timestamp, metadata
                             FROM activity WHERE 1=1".to_string();
        let mut binds: Vec<String> = Vec::new();
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            let clause = format!(" AND agent_id = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        if let Some(t) = event_type {
            binds.push(t.to_string());
            let clause = format!(" AND event_type = ?{}", binds.len());
            count_sql.push_str(&clause);
            query_sql.push_str(&clause);
        }
        query_sql.push_str(&format!(
            " ORDER BY timestamp DESC LIMIT {} OFFSET {}",
            limit, offset
        ));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();

        let total: i64 = conn
            .query_row(&count_sql, params_ref.as_slice(), |r| r.get(0))
            .map_err(|e| format!("count activity: {e}"))?;

        let mut stmt = conn.prepare(&query_sql).map_err(|e| format!("prepare activity: {e}"))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), |r| {
                Ok(ActivityRow {
                    id: r.get(0)?,
                    event_type: r.get(1)?,
                    agent_id: r.get(2)?,
                    task_id: r.get(3)?,
                    summary: r.get(4)?,
                    timestamp: r.get(5)?,
                    metadata: r.get(6)?,
                })
            })
            .map_err(|e| format!("query activity: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect activity: {e}"))?;

        Ok((rows, total))
    }

    /// Every activity row for one task, oldest first (chronological for the
    /// goal-loop timeline). Task-scoped where [`Self::list_activity`] is
    /// global — a long-running goal's kickoff/oscillation/needs_human events
    /// would be washed out of any bounded global window.
    pub async fn list_activity_for_task(
        &self,
        task_id: &str,
        limit: i64,
    ) -> Result<Vec<ActivityRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, event_type, agent_id, task_id, summary, timestamp, metadata
                 FROM activity WHERE task_id = ?1 ORDER BY timestamp ASC LIMIT ?2",
            )
            .map_err(|e| format!("prepare task activity: {e}"))?;
        let rows = stmt
            .query_map(params![task_id, limit.clamp(1, 1000)], |r| {
                Ok(ActivityRow {
                    id: r.get(0)?,
                    event_type: r.get(1)?,
                    agent_id: r.get(2)?,
                    task_id: r.get(3)?,
                    summary: r.get(4)?,
                    timestamp: r.get(5)?,
                    metadata: r.get(6)?,
                })
            })
            .map_err(|e| format!("query task activity: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect task activity: {e}"))?;
        Ok(rows)
    }

    /// H22: RFC3339 timestamp of the most recent Activity Feed event for a
    /// task, or `None` when the task has none.
    ///
    /// This is the goal loop's **progress signal** for the timeout notice.
    /// The obvious alternative, `tasks.updated_at`, is unusable: the dispatch
    /// engine's lease renewer calls [`Self::renew_lease`], which bumps
    /// `updated_at` on a timer for every `in_progress` task — so a silent
    /// agent's row looks freshly updated forever. The activity feed only
    /// moves when something actually happened (the driver dispatched a round,
    /// the engine judged one, or the agent itself posted via the
    /// `activity_post` MCP tool), which is exactly the definition of
    /// "reported progress".
    ///
    /// Served by `idx_activity_ts` (`timestamp DESC`); one row, one task.
    pub async fn latest_activity_at(&self, task_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT timestamp FROM activity WHERE task_id = ?1 ORDER BY timestamp DESC LIMIT 1",
            params![task_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("latest activity: {e}"))
    }

    // ── Task comments (L2) ──────────────────────────────────

    /// Append a comment. Caller is responsible for verifying the task exists and
    /// that `body` is non-empty and length-capped.
    pub async fn insert_comment(&self, row: &CommentRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO task_comments (id, task_id, author_user, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![row.id, row.task_id, row.author_user, row.body, row.created_at],
        )
        .map_err(|e| format!("insert comment: {e}"))?;
        Ok(())
    }

    /// All comments for a task, oldest first (chronological for the timeline).
    pub async fn list_comments(&self, task_id: &str) -> Result<Vec<CommentRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, author_user, body, created_at
                 FROM task_comments WHERE task_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare comments: {e}"))?;
        let rows = stmt
            .query_map(params![task_id], |r| {
                Ok(CommentRow {
                    id: r.get(0)?,
                    task_id: r.get(1)?,
                    author_user: r.get(2)?,
                    body: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })
            .map_err(|e| format!("query comments: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect comments: {e}"))?;
        Ok(rows)
    }

    // ── U4 co-edited plans ──────────────────────────────────

    pub async fn insert_plan(&self, row: &PlanRow) -> Result<(), String> {
        if !PLAN_STATUSES.contains(&row.status.as_str()) {
            return Err(format!("invalid plan status: {}", row.status));
        }
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO plans (id, title, description, agent_id, goal_id, status, created_by,
                                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.id,
                row.title,
                row.description,
                row.agent_id,
                row.goal_id,
                row.status,
                row.created_by,
                row.created_at,
                row.updated_at,
            ],
        )
        .map_err(|e| format!("insert plan: {e}"))?;
        Ok(())
    }

    pub async fn get_plan(&self, id: &str) -> Result<Option<PlanRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {PLAN_COLUMNS} FROM plans WHERE id = ?1"),
            params![id],
            row_to_plan,
        )
        .optional()
        .map_err(|e| format!("get plan: {e}"))
    }

    /// Plans newest-activity-first. Optional agent / status filters.
    pub async fn list_plans(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<PlanRow>, String> {
        let conn = self.conn.lock().await;
        let mut sql = format!("SELECT {PLAN_COLUMNS} FROM plans WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(a) = agent_id {
            binds.push(a.to_string());
            sql.push_str(&format!(" AND agent_id = ?{}", binds.len()));
        }
        if let Some(s) = status {
            binds.push(s.to_string());
            sql.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        sql.push_str(" ORDER BY updated_at DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare plans: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_plan)
            .map_err(|e| format!("query plans: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect plans: {e}"))?;
        Ok(rows)
    }

    /// Update mutable plan fields (`title` / `description` / `status`).
    /// Status is validated fail-closed against [`PLAN_STATUSES`].
    pub async fn update_plan(
        &self,
        id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<PlanRow>, String> {
        if let Some(s) = fields.get("status").and_then(|v| v.as_str()) {
            if !PLAN_STATUSES.contains(&s) {
                return Err(format!("invalid plan status: {s}"));
            }
        }
        {
            let conn = self.conn.lock().await;
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<String> = vec![Utc::now().to_rfc3339()];
            for key in ["title", "description", "status"] {
                if let Some(v) = fields.get(key).and_then(|v| v.as_str()) {
                    binds.push(v.to_string());
                    sets.push(format!("{key} = ?{}", binds.len()));
                }
            }
            binds.push(id.to_string());
            let sql = format!(
                "UPDATE plans SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );
            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            conn.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update plan: {e}"))?;
        }
        self.get_plan(id).await
    }

    /// Delete a plan and all its steps in one transaction.
    pub async fn remove_plan(&self, id: &str) -> Result<bool, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("remove plan: begin: {e}"))?;
        tx.execute("DELETE FROM plan_steps WHERE plan_id = ?1", params![id])
            .map_err(|e| format!("remove plan steps: {e}"))?;
        let n = tx
            .execute("DELETE FROM plans WHERE id = ?1", params![id])
            .map_err(|e| format!("remove plan: {e}"))?;
        tx.commit().map_err(|e| format!("remove plan: commit: {e}"))?;
        Ok(n > 0)
    }

    /// Steps of a plan in display order. `step_order` ties break on
    /// `created_at, id` so the ordering is total and deterministic.
    pub async fn list_plan_steps(&self, plan_id: &str) -> Result<Vec<PlanStepRow>, String> {
        let conn = self.conn.lock().await;
        list_plan_steps_conn(&conn, plan_id)
    }

    pub async fn get_plan_step(&self, step_id: &str) -> Result<Option<PlanStepRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {PLAN_STEP_COLUMNS} FROM plan_steps WHERE id = ?1"),
            params![step_id],
            row_to_plan_step,
        )
        .optional()
        .map_err(|e| format!("get plan step: {e}"))
    }

    /// Append or insert a step. `position` = target display index (None ⇒
    /// append). The order key is computed inside one IMMEDIATE transaction:
    /// integer-gap midpoint between the neighbours; a collided gap triggers a
    /// renormalization of the whole plan first (see [`PLAN_STEP_ORDER_GAP`]).
    /// Fail-closed enum validation on `assignee_kind` / `status`.
    pub async fn add_plan_step(
        &self,
        plan_id: &str,
        step_id: &str,
        text: &str,
        assignee_kind: &str,
        assignee: &str,
        position: Option<usize>,
    ) -> Result<PlanStepRow, String> {
        if !PLAN_ASSIGNEE_KINDS.contains(&assignee_kind) {
            return Err(format!("invalid assignee_kind: {assignee_kind}"));
        }
        if text.trim().is_empty() {
            return Err("step text is required".into());
        }
        let now = Utc::now().to_rfc3339();
        let row = {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("add step: begin: {e}"))?;
            // Plan must exist — a step may never dangle.
            let plan_exists: Option<String> = tx
                .query_row("SELECT id FROM plans WHERE id = ?1", params![plan_id], |r| r.get(0))
                .optional()
                .map_err(|e| format!("add step: plan lookup: {e}"))?;
            if plan_exists.is_none() {
                return Err(format!("plan not found: {plan_id}"));
            }
            let orders = plan_step_orders_conn(&tx, plan_id)?;
            let index = position.unwrap_or(orders.len()).min(orders.len());
            let order = match plan_order_for_insert(&orders, index) {
                Some(o) => o,
                None => {
                    renormalize_plan_steps_conn(&tx, plan_id, &now)?;
                    let orders = plan_step_orders_conn(&tx, plan_id)?;
                    plan_order_for_insert(&orders, index)
                        .ok_or_else(|| "plan ordering renormalization failed".to_string())?
                }
            };
            let row = PlanStepRow {
                id: step_id.to_string(),
                plan_id: plan_id.to_string(),
                text: text.trim().to_string(),
                assignee_kind: assignee_kind.to_string(),
                assignee: assignee.to_string(),
                status: "todo".into(),
                step_order: order,
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            tx.execute(
                "INSERT INTO plan_steps (id, plan_id, text, assignee_kind, assignee, status,
                                         step_order, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row.id,
                    row.plan_id,
                    row.text,
                    row.assignee_kind,
                    row.assignee,
                    row.status,
                    row.step_order,
                    row.created_at,
                    row.updated_at,
                ],
            )
            .map_err(|e| format!("insert step: {e}"))?;
            tx.execute(
                "UPDATE plans SET updated_at = ?2 WHERE id = ?1",
                params![plan_id, now],
            )
            .map_err(|e| format!("touch plan: {e}"))?;
            tx.commit().map_err(|e| format!("add step: commit: {e}"))?;
            row
        };
        Ok(row)
    }

    /// Update step fields (`text` / `status` / `assignee_kind` / `assignee`).
    /// Enum fields are validated fail-closed. Returns the updated row.
    pub async fn update_plan_step(
        &self,
        step_id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<PlanStepRow>, String> {
        if let Some(s) = fields.get("status").and_then(|v| v.as_str()) {
            if !PLAN_STEP_STATUSES.contains(&s) {
                return Err(format!("invalid step status: {s}"));
            }
        }
        if let Some(k) = fields.get("assignee_kind").and_then(|v| v.as_str()) {
            if !PLAN_ASSIGNEE_KINDS.contains(&k) {
                return Err(format!("invalid assignee_kind: {k}"));
            }
        }
        if let Some(t) = fields.get("text").and_then(|v| v.as_str()) {
            if t.trim().is_empty() {
                return Err("step text must not be empty".into());
            }
        }
        {
            let conn = self.conn.lock().await;
            let now = Utc::now().to_rfc3339();
            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<String> = vec![now.clone()];
            for key in ["text", "status", "assignee_kind", "assignee"] {
                if let Some(v) = fields.get(key).and_then(|v| v.as_str()) {
                    binds.push(if key == "text" { v.trim().to_string() } else { v.to_string() });
                    sets.push(format!("{key} = ?{}", binds.len()));
                }
            }
            if sets.len() == 1 {
                return Err("no step fields to update".into());
            }
            binds.push(step_id.to_string());
            let sql = format!(
                "UPDATE plan_steps SET {} WHERE id = ?{}",
                sets.join(", "),
                binds.len()
            );
            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            conn.execute(&sql, params_ref.as_slice())
                .map_err(|e| format!("update step: {e}"))?;
            // Touch the parent plan so `updated_at` reflects the latest co-edit.
            conn.execute(
                "UPDATE plans SET updated_at = ?1
                  WHERE id = (SELECT plan_id FROM plan_steps WHERE id = ?2)",
                params![now, step_id],
            )
            .map_err(|e| format!("touch plan: {e}"))?;
        }
        self.get_plan_step(step_id).await
    }

    /// Move a step to a new display index within its plan. Integer-gap
    /// midpoint write; gap exhaustion renormalizes first — all inside one
    /// IMMEDIATE transaction so concurrent moves cannot interleave.
    pub async fn move_plan_step(
        &self,
        plan_id: &str,
        step_id: &str,
        new_index: usize,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("move step: begin: {e}"))?;
        let steps = list_plan_steps_conn(&tx, plan_id)?;
        let Some(cur_idx) = steps.iter().position(|s| s.id == step_id) else {
            return Ok(false);
        };
        let target = new_index.min(steps.len().saturating_sub(1));
        if target == cur_idx {
            return Ok(true); // no-op move
        }
        // Orders of the remaining steps once the moving one is lifted out.
        let orders: Vec<i64> = steps
            .iter()
            .filter(|s| s.id != step_id)
            .map(|s| s.step_order)
            .collect();
        let order = match plan_order_for_insert(&orders, target) {
            Some(o) => o,
            None => {
                renormalize_plan_steps_conn(&tx, plan_id, &now)?;
                let steps = list_plan_steps_conn(&tx, plan_id)?;
                let orders: Vec<i64> = steps
                    .iter()
                    .filter(|s| s.id != step_id)
                    .map(|s| s.step_order)
                    .collect();
                plan_order_for_insert(&orders, target)
                    .ok_or_else(|| "plan ordering renormalization failed".to_string())?
            }
        };
        tx.execute(
            "UPDATE plan_steps SET step_order = ?2, updated_at = ?3 WHERE id = ?1",
            params![step_id, order, now],
        )
        .map_err(|e| format!("move step: {e}"))?;
        tx.execute(
            "UPDATE plans SET updated_at = ?2 WHERE id = ?1",
            params![plan_id, now],
        )
        .map_err(|e| format!("touch plan: {e}"))?;
        tx.commit().map_err(|e| format!("move step: commit: {e}"))?;
        Ok(true)
    }

    /// Remove a step; returns the removed row (for event attribution).
    pub async fn remove_plan_step(&self, step_id: &str) -> Result<Option<PlanStepRow>, String> {
        let existing = self.get_plan_step(step_id).await?;
        let Some(row) = existing else {
            return Ok(None);
        };
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        conn.execute("DELETE FROM plan_steps WHERE id = ?1", params![step_id])
            .map_err(|e| format!("remove step: {e}"))?;
        conn.execute(
            "UPDATE plans SET updated_at = ?2 WHERE id = ?1",
            params![row.plan_id, now],
        )
        .map_err(|e| format!("touch plan: {e}"))?;
        Ok(Some(row))
    }

    /// Render the agent-facing "## Shared Plan" prompt section for `agent_id`.
    ///
    /// Deterministic, data-derived only (no timestamps, no counters that churn
    /// without a real edit) so the injected block stays **byte-stable** while
    /// the underlying rows are unchanged — prompt-cache friendly. Shows the
    /// most recently updated ACTIVE plan that has at least one step assigned
    /// to this agent; the agent's own open steps are listed explicitly, other
    /// steps as one-line context. `None` ⇒ callers skip the section.
    ///
    /// Wiring: append the returned string to the system prompt in
    /// `claude_runner.rs` next to `build_pending_tasks_section` (one line).
    pub async fn plan_prompt_section(&self, agent_id: &str) -> Result<Option<String>, String> {
        let plans = self.list_plans(Some(agent_id), Some("active")).await?;
        for plan in plans {
            let steps = self.list_plan_steps(&plan.id).await?;
            let mine_open: Vec<&PlanStepRow> = steps
                .iter()
                .filter(|s| {
                    s.assignee_kind == "agent"
                        && s.assignee == agent_id
                        && (s.status == "todo" || s.status == "doing")
                })
                .collect();
            if mine_open.is_empty() {
                continue;
            }
            let done = steps
                .iter()
                .filter(|s| s.status == "done" || s.status == "skipped")
                .count();
            let mut lines: Vec<String> = Vec::new();
            for (i, s) in steps.iter().enumerate() {
                let marker = match s.status.as_str() {
                    "done" => "[x]",
                    "doing" => "[~]",
                    "skipped" => "[-]",
                    _ => "[ ]",
                };
                let holder = if s.assignee.is_empty() {
                    format!("({})", s.assignee_kind)
                } else {
                    format!("({}: {})", s.assignee_kind, s.assignee)
                };
                let yours = if s.assignee_kind == "agent" && s.assignee == agent_id {
                    " ← yours"
                } else {
                    ""
                };
                lines.push(format!(
                    "{}. {marker} {} {holder}{yours}",
                    i + 1,
                    duduclaw_core::truncate_chars(&s.text, 120),
                ));
            }
            return Ok(Some(format!(
                "## Shared Plan: {} ({done}/{} steps done)\n{}\n\n\
                 This plan is co-edited with your user. Use `plan_get` to re-read it and \
                 `plan_update_step` to update the steps marked \"yours\" (status: todo / doing / \
                 done / skipped). Steps assigned to the user are theirs — do not change them.",
                duduclaw_core::truncate_chars(&plan.title, 80),
                steps.len(),
                lines.join("\n"),
            )));
        }
        Ok(None)
    }
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        assigned_to: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        completed_at: row.get(9)?,
        blocked_reason: row.get(10)?,
        parent_task_id: row.get(11)?,
        tags: row.get(12)?,
        message_id: row.get(13)?,
        claimed_by: row.get(14)?,
        claimed_at: row.get(15)?,
        lease_expires_at: row.get(16)?,
        depends_on: row.get(17)?,
        retry_count: row.get(18)?,
        max_retries: row.get(19)?,
        goal_mode: row.get::<_, i64>(20)? != 0,
        acceptance_criteria: row.get(21)?,
        result_summary: row.get(22)?,
        judge_feedback: row.get(23)?,
        goal_id: row.get(24)?,
        lease_renewed_at: row.get(25)?,
        source_channel: row.get(26)?,
        source_chat_id: row.get(27)?,
        revision_round: row.get(28)?,
        diminishing: row.get::<_, i64>(29)? != 0,
        agent_seconds: row.get(30)?,
        goal_state_json: row.get(31)?,
        source_discord_guild_id: row.get(32)?,
        deadline_at: row.get(33)?,
        risk_boundary: row.get(34)?,
        acceptance_criteria_baseline: row.get(35)?,
        pause_reason: row.get(36)?,
        plan_pending: row.get(37)?,
        archived: row.get::<_, i64>(38)? != 0,
        pinned: row.get::<_, i64>(39)? != 0,
    })
}

fn row_to_iteration(row: &rusqlite::Row) -> rusqlite::Result<TaskIterationRow> {
    Ok(TaskIterationRow {
        id: row.get(0)?,
        task_id: row.get(1)?,
        round: row.get(2)?,
        dispatched_at: row.get(3)?,
        submitted_at: row.get(4)?,
        judged_at: row.get(5)?,
        verdict: row.get(6)?,
        judge_feedback: row.get(7)?,
        feedback_class: row.get(8)?,
        verdict_json: row.get(9)?,
        dispatch_count: row.get(10)?,
        state_hash: row.get(11)?,
        repeat_streak: row.get(12)?,
        worker_excerpt: row.get(13)?,
    })
}

/// Sync twin of [`TaskStore::list_iterations`] — usable inside a caller that
/// already holds `self.conn`'s lock (e.g. `reject_review_with_verdict`'s
/// WP-4F best-round pick, which must not re-lock the same `Mutex` and
/// deadlock).
fn list_iterations_conn(conn: &Connection, task_id: &str) -> Result<Vec<TaskIterationRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, task_id, round, dispatched_at, submitted_at, judged_at,
                    verdict, judge_feedback, feedback_class, verdict_json,
                    dispatch_count, state_hash, repeat_streak, worker_excerpt
               FROM task_iterations WHERE task_id = ?1 ORDER BY round ASC, id ASC",
        )
        .map_err(|e| format!("prepare iterations: {e}"))?;
    let rows = stmt
        .query_map(params![task_id], row_to_iteration)
        .map_err(|e| format!("query iterations: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("collect iterations: {e}"))?;
    Ok(rows)
}

// ── Iterative Kanban: iteration sync helpers (usable in a tx) ──

/// Whole seconds between two RFC3339 stamps, floored at 0 (a bad stamp ⇒ 0 so
/// telemetry never goes negative or panics).
fn round_seconds(dispatched_at: &str, submitted_at: &str) -> i64 {
    match (
        DateTime::parse_from_rfc3339(dispatched_at),
        DateTime::parse_from_rfc3339(submitted_at),
    ) {
        (Ok(d), Ok(s)) => (s.with_timezone(&Utc) - d.with_timezone(&Utc))
            .num_seconds()
            .max(0),
        _ => 0,
    }
}

/// Open round `round` for `task_id` if it does not already exist. Idempotent
/// per `(task_id, round)` in the timeline sense — a stall re-dispatch of the
/// same round keeps the original `dispatched_at` but increments
/// `dispatch_count` and refreshes the visit-graph signal (the count was
/// previously memory-only in the driver and lost on every restart).
fn iter_dispatch_conn(
    conn: &Connection,
    task_id: &str,
    round: i64,
    now: &str,
    state_hash: Option<&str>,
    repeat_streak: Option<i64>,
) -> Result<(), String> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM task_iterations WHERE task_id = ?1 AND round = ?2",
            params![task_id, round],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("iter dispatch lookup: {e}"))?;
    if let Some(id) = exists {
        conn.execute(
            "UPDATE task_iterations
                SET dispatch_count = dispatch_count + 1,
                    state_hash = COALESCE(?2, state_hash),
                    repeat_streak = COALESCE(?3, repeat_streak)
              WHERE id = ?1",
            params![id, state_hash, repeat_streak],
        )
        .map_err(|e| format!("iter dispatch bump: {e}"))?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO task_iterations (task_id, round, dispatched_at, state_hash, repeat_streak)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![task_id, round, now, state_hash, repeat_streak],
    )
    .map_err(|e| format!("iter dispatch insert: {e}"))?;
    Ok(())
}

/// Stamp the worker submission on the latest open round (max round with a NULL
/// `submitted_at`) and return that round's agent seconds. When no open round
/// exists (e.g. a direct claim→complete path that skipped the driver dispatch),
/// one is created retroactively anchored at `fallback_dispatch` (claim time) so
/// the agent clock is still captured. Returns 0 seconds when the elapsed time is
/// non-positive / unparseable.
fn iter_submit_conn(
    conn: &Connection,
    task_id: &str,
    now: &str,
    fallback_round: i64,
    fallback_dispatch: &str,
) -> Result<i64, String> {
    let open: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, dispatched_at FROM task_iterations
              WHERE task_id = ?1 AND submitted_at IS NULL
              ORDER BY round DESC LIMIT 1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("iter submit lookup: {e}"))?;
    let (row_id, dispatched_at) = match open {
        Some((id, d)) => (id, d),
        None => {
            conn.execute(
                "INSERT INTO task_iterations (task_id, round, dispatched_at) VALUES (?1, ?2, ?3)",
                params![task_id, fallback_round, fallback_dispatch],
            )
            .map_err(|e| format!("iter submit backfill: {e}"))?;
            (conn.last_insert_rowid(), fallback_dispatch.to_string())
        }
    };
    conn.execute(
        "UPDATE task_iterations SET submitted_at = ?2 WHERE id = ?1",
        params![row_id, now],
    )
    .map_err(|e| format!("iter submit update: {e}"))?;
    Ok(round_seconds(&dispatched_at, now))
}

/// Seal the judge verdict on the latest un-judged round (max round with a NULL
/// `judged_at`). No open round ⇒ no-op (best-effort telemetry).
/// `worker_excerpt` (WP-4F): a bounded, CJK-safe-truncated snapshot of this
/// round's own worker output — `None` for the accept path (never needed,
/// see `TaskIterationRow::worker_excerpt`'s doc) and for callers with no
/// result text to snapshot.
fn iter_verdict_conn(
    conn: &Connection,
    task_id: &str,
    verdict: &str,
    feedback: &str,
    verdict_json: Option<&str>,
    worker_excerpt: Option<&str>,
    now: &str,
) -> Result<(), String> {
    let row_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM task_iterations
              WHERE task_id = ?1 AND judged_at IS NULL
              ORDER BY round DESC LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("iter verdict lookup: {e}"))?;
    if let Some(id) = row_id {
        conn.execute(
            "UPDATE task_iterations
                SET judged_at = ?2, verdict = ?3, judge_feedback = ?4,
                    verdict_json = ?5, worker_excerpt = ?6
              WHERE id = ?1",
            params![id, now, verdict, feedback, verdict_json, worker_excerpt],
        )
        .map_err(|e| format!("iter verdict update: {e}"))?;
    }
    Ok(())
}

// ── Connection-level read helpers ───────────────────────────
//
// Sync twins of the async read methods, usable both under the store's Mutex
// lock and inside a `Transaction` (which derefs to `Connection`) — the TOCTOU
// fixes run their cycle/existence checks through these inside the same
// IMMEDIATE transaction as the write.

fn get_goal_conn(conn: &Connection, id: &str) -> Result<Option<GoalRow>, String> {
    conn.query_row(
        "SELECT id, title, description, parent_goal_id, status, created_at
           FROM goals WHERE id = ?1",
        params![id],
        row_to_goal,
    )
    .optional()
    .map_err(|e| format!("get goal: {e}"))
}

fn goal_parent_edges_conn(conn: &Connection) -> Result<Vec<(String, Option<String>)>, String> {
    let mut stmt = conn
        .prepare("SELECT id, parent_goal_id FROM goals")
        .map_err(|e| format!("prepare goal edges: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
        .map_err(|e| format!("query goal edges: {e}"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("collect goal edges: {e}"))?;
    Ok(rows)
}

fn depends_edges_conn(conn: &Connection) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut stmt = conn
        .prepare("SELECT id, depends_on FROM tasks")
        .map_err(|e| format!("prepare dep edges: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| format!("query dep edges: {e}"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("collect dep edges: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(id, deps)| (id, parse_depends_on(&deps)))
        .collect())
}

// ── U4 plan helpers ─────────────────────────────────────────

fn row_to_plan(row: &rusqlite::Row) -> rusqlite::Result<PlanRow> {
    Ok(PlanRow {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        agent_id: row.get(3)?,
        goal_id: row.get(4)?,
        status: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_plan_step(row: &rusqlite::Row) -> rusqlite::Result<PlanStepRow> {
    Ok(PlanStepRow {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        text: row.get(2)?,
        assignee_kind: row.get(3)?,
        assignee: row.get(4)?,
        status: row.get(5)?,
        step_order: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// Sync twin usable under the store Mutex and inside a `Transaction`.
fn list_plan_steps_conn(conn: &Connection, plan_id: &str) -> Result<Vec<PlanStepRow>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {PLAN_STEP_COLUMNS} FROM plan_steps
              WHERE plan_id = ?1 ORDER BY step_order ASC, created_at ASC, id ASC"
        ))
        .map_err(|e| format!("prepare steps: {e}"))?;
    let rows = stmt
        .query_map(params![plan_id], row_to_plan_step)
        .map_err(|e| format!("query steps: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("collect steps: {e}"))?;
    Ok(rows)
}

fn plan_step_orders_conn(conn: &Connection, plan_id: &str) -> Result<Vec<i64>, String> {
    Ok(list_plan_steps_conn(conn, plan_id)?
        .iter()
        .map(|s| s.step_order)
        .collect())
}

/// Rewrite a plan's step orders back to clean gap multiples (1×GAP, 2×GAP, …)
/// preserving the current display order. Called inside the caller's
/// transaction when a midpoint insert would collide.
fn renormalize_plan_steps_conn(
    conn: &Connection,
    plan_id: &str,
    now: &str,
) -> Result<(), String> {
    let steps = list_plan_steps_conn(conn, plan_id)?;
    for (i, s) in steps.iter().enumerate() {
        conn.execute(
            "UPDATE plan_steps SET step_order = ?2, updated_at = ?3 WHERE id = ?1",
            params![s.id, ((i as i64) + 1) * PLAN_STEP_ORDER_GAP, now],
        )
        .map_err(|e| format!("renormalize step: {e}"))?;
    }
    Ok(())
}

/// Compute the `step_order` key for inserting at display `index` among the
/// existing sorted `orders`. Integer-gap semantics:
/// - append (index ≥ len) ⇒ `last + GAP` (always succeeds);
/// - front / between ⇒ midpoint of the neighbours (`prev` = 0 for the front);
/// - `None` ⇒ the gap is exhausted (midpoint would collide) — the caller must
///   renormalize the plan and retry. Pure + unit-tested.
pub fn plan_order_for_insert(orders: &[i64], index: usize) -> Option<i64> {
    if index >= orders.len() {
        return Some(orders.last().copied().unwrap_or(0) + PLAN_STEP_ORDER_GAP);
    }
    let prev = if index == 0 { 0 } else { orders[index - 1] };
    let next = orders[index];
    let mid = prev + (next - prev) / 2;
    if mid > prev && mid < next {
        Some(mid)
    } else {
        None
    }
}

fn row_to_goal(row: &rusqlite::Row) -> rusqlite::Result<GoalRow> {
    Ok(GoalRow {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        parent_goal_id: row.get(3)?,
        status: row.get(4)?,
        created_at: row.get(5)?,
    })
}

// ── G1 pure helpers (no I/O, fully unit-tested) ─────────────

/// Parse a `depends_on` JSON array of task ids. Malformed / non-array input is
/// treated as "no dependencies" (fail-open on the *shape*, not on gating — an
/// empty dep list just means immediately claimable).
pub fn parse_depends_on(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

/// Are every dependency id present in the `done` set? Empty deps ⇒ satisfied.
pub fn deps_satisfied(depends_on: &[String], done: &HashSet<String>) -> bool {
    depends_on.iter().all(|d| done.contains(d))
}

/// Has a lease (RFC3339) elapsed relative to `now` (RFC3339)? Unparseable
/// timestamps are treated as *expired* so a corrupt lease can't pin a zombie
/// forever (fail-safe toward reclaim).
pub fn lease_is_expired(lease_expires_at: &str, now: &str) -> bool {
    match (
        DateTime::parse_from_rfc3339(lease_expires_at),
        DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(lease), Ok(now)) => now >= lease,
        _ => true,
    }
}

/// Conservative zombie-reclaim decision (G1 lease renewal, v1.36).
///
/// A claimed task is reclaim-due only when its lease has expired AND a further
/// full lease window has elapsed since expiry with no renewal. The window is
/// derived per task as `lease_expires_at - renewal_anchor` (anchor = last
/// renewal, falling back to the claim time), so the store needs no lease-length
/// config. A live worker's renewal ticker keeps pushing `lease_expires_at`
/// forward, so it never reaches expiry in the first place; the grace window
/// additionally absorbs a tick that is late or in flight.
///
/// Corrupt / unparseable lease or `now` ⇒ due (a corrupt lease must not pin a
/// zombie forever — same fail-safe direction as [`lease_is_expired`]). A
/// missing / unparseable anchor degrades to a zero grace window (legacy rows:
/// reclaim at plain expiry).
pub fn zombie_reclaim_due(
    lease_expires_at: &str,
    renewal_anchor: Option<&str>,
    now: &str,
) -> bool {
    let (lease, now_ts) = match (
        DateTime::parse_from_rfc3339(lease_expires_at),
        DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(l), Ok(n)) => (l, n),
        _ => return true,
    };
    if now_ts < lease {
        return false; // lease still live
    }
    let window = renewal_anchor
        .and_then(|a| DateTime::parse_from_rfc3339(a).ok())
        .map(|a| (lease - a).max(chrono::Duration::zero()))
        .unwrap_or_else(chrono::Duration::zero);
    now_ts >= lease + window
}

/// Would setting `task_id.depends_on = new_deps` introduce a dependency cycle?
/// DFS from each new dep over the current `depends_on` edges with a visited
/// set; reaching `task_id` (or a direct self-dependency) closes a loop.
/// Pure + deterministic — fail-closed callers reject on `true`.
pub fn introduces_dependency_cycle(
    edges: &[(String, Vec<String>)],
    task_id: &str,
    new_deps: &[String],
) -> bool {
    if new_deps.iter().any(|d| d == task_id) {
        return true; // trivial self-dependency
    }
    use std::collections::HashMap;
    let dep_map: HashMap<&str, &[String]> = edges
        .iter()
        .map(|(id, deps)| (id.as_str(), deps.as_slice()))
        .collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&str> = new_deps.iter().map(|s| s.as_str()).collect();
    while let Some(node) = stack.pop() {
        if node == task_id {
            return true;
        }
        if !visited.insert(node) {
            continue;
        }
        if let Some(deps) = dep_map.get(node) {
            for d in deps.iter() {
                stack.push(d.as_str());
            }
        }
    }
    false
}

/// Decide what to do with an expired-lease task given its retry state.
/// `retry_count < max_retries` ⇒ requeue (one more attempt); otherwise fail.
pub fn zombie_action(retry_count: i64, max_retries: i64) -> ZombieAction {
    if retry_count < max_retries {
        ZombieAction::Requeue
    } else {
        ZombieAction::Fail
    }
}

/// RFC-26 §4.5: would setting `child.parent = new_parent` introduce a cycle in the
/// task parent graph? Walks up from `new_parent` via the existing edges; a cycle
/// exists if the walk reaches `child` (or loops). Pure + deterministic.
///
/// `edges` is the current `(id, parent)` set. A self-parent (`child == new_parent`)
/// is a trivial cycle.
pub fn introduces_parent_cycle(
    edges: &[(String, Option<String>)],
    child: &str,
    new_parent: &str,
) -> bool {
    if child == new_parent {
        return true;
    }
    use std::collections::HashMap;
    let parent_of: HashMap<&str, Option<&str>> = edges
        .iter()
        .map(|(id, p)| (id.as_str(), p.as_deref()))
        .collect();

    // Walk ancestors of new_parent; if we hit `child`, adding the edge closes a loop.
    let mut seen = std::collections::HashSet::new();
    let mut cur = Some(new_parent);
    while let Some(node) = cur {
        if node == child {
            return true;
        }
        if !seen.insert(node) {
            // Pre-existing cycle in the data — treat as unsafe.
            return true;
        }
        cur = parent_of.get(node).copied().flatten();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        deps_satisfied, introduces_dependency_cycle, introduces_parent_cycle, lease_is_expired,
        parse_depends_on, zombie_action, zombie_reclaim_due, ActivityRow, CommentRow, GoalRow,
        TaskRow, TaskStore, ZombieAction,
    };
    use std::collections::HashSet;

    fn temp_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open store");
        (store, dir)
    }

    fn comment(id: &str, task: &str, at: &str, body: &str) -> CommentRow {
        CommentRow {
            id: id.into(),
            task_id: task.into(),
            author_user: "user-1".into(),
            body: body.into(),
            created_at: at.into(),
        }
    }

    // ── W2-7: source_discord_guild_id column round trip ──────

    #[tokio::test]
    async fn source_discord_guild_id_round_trips_through_insert_and_get() {
        let (store, _dir) = temp_store();
        let mut task = TaskRow::new(
            "t-discord".into(),
            "Goal from Discord".into(),
            String::new(),
            "medium".into(),
            "bot".into(),
            "goal:discord".into(),
        );
        task.source_channel = Some("discord".into());
        task.source_chat_id = Some("chan-1".into());
        task.source_discord_guild_id = Some("guild-1".into());
        store.insert_task(&task).await.expect("insert task");

        let got = store.get_task("t-discord").await.expect("get task").expect("row exists");
        assert_eq!(got.source_discord_guild_id.as_deref(), Some("guild-1"));

        let listed = store.list_tasks(None, None, None).await.expect("list tasks");
        let row = listed.iter().find(|t| t.id == "t-discord").expect("row in list");
        assert_eq!(row.source_discord_guild_id.as_deref(), Some("guild-1"));
    }

    #[tokio::test]
    async fn source_discord_guild_id_defaults_to_none() {
        let (store, _dir) = temp_store();
        // A non-Discord (or Discord-but-unknown-guild) task never fabricates
        // a value — the column stays NULL / None.
        let task = TaskRow::new(
            "t-telegram".into(),
            "Goal from Telegram".into(),
            String::new(),
            "medium".into(),
            "bot".into(),
            "goal:telegram".into(),
        );
        store.insert_task(&task).await.expect("insert task");
        let got = store.get_task("t-telegram").await.expect("get task").expect("row exists");
        assert_eq!(got.source_discord_guild_id, None);
    }

    // ── I-3b: archived/pinned task list operations ───────────

    #[tokio::test]
    async fn archived_and_pinned_default_to_false_on_insert() {
        let (store, _dir) = temp_store();
        let task = TaskRow::new(
            "t-defaults".into(),
            "Fresh task".into(),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert task");
        let got = store.get_task("t-defaults").await.expect("get task").expect("row exists");
        assert!(!got.archived, "archived must default to false");
        assert!(!got.pinned, "pinned must default to false");
    }

    /// Idempotent migration: opening the same on-disk `tasks.db` a second
    /// time (simulating a gateway restart against a pre-existing store)
    /// must not error and must leave a pre-existing row's archived/pinned
    /// state untouched — same contract as every other `add_dispatch_columns`
    /// migration.
    #[tokio::test]
    async fn archived_pinned_migration_is_idempotent_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = TaskStore::open(dir.path()).expect("open store");
            let task = TaskRow::new(
                "t-migrate".into(),
                "Pre-migration task".into(),
                String::new(),
                "medium".into(),
                "bot".into(),
                "user-1".into(),
            );
            store.insert_task(&task).await.expect("insert task");
        }
        // Reopen — add_dispatch_columns runs again; ALTER TABLE ADD COLUMN
        // must be a no-op the second time, not an error.
        let store = TaskStore::open(dir.path()).expect("reopen store");
        let got = store.get_task("t-migrate").await.expect("get task").expect("row exists");
        assert!(!got.archived);
        assert!(!got.pinned);
    }

    #[tokio::test]
    async fn update_task_sets_archived_and_pinned_booleans() {
        let (store, _dir) = temp_store();
        let task = TaskRow::new(
            "t-toggle".into(),
            "Toggle me".into(),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert task");

        store
            .update_task("t-toggle", &serde_json::json!({ "archived": true }))
            .await
            .expect("archive");
        let got = store.get_task("t-toggle").await.unwrap().unwrap();
        assert!(got.archived);
        assert!(!got.pinned, "pinning must be untouched by an archive-only update");

        store
            .update_task("t-toggle", &serde_json::json!({ "pinned": true }))
            .await
            .expect("pin");
        let got = store.get_task("t-toggle").await.unwrap().unwrap();
        assert!(got.archived, "archiving must be untouched by a pin-only update");
        assert!(got.pinned);

        store
            .update_task("t-toggle", &serde_json::json!({ "archived": false, "pinned": false }))
            .await
            .expect("unarchive+unpin");
        let got = store.get_task("t-toggle").await.unwrap().unwrap();
        assert!(!got.archived);
        assert!(!got.pinned);
    }

    #[tokio::test]
    async fn list_tasks_filtered_excludes_archived_by_default() {
        let (store, _dir) = temp_store();
        for id in ["visible-1", "visible-2", "archived-1"] {
            let task = TaskRow::new(
                id.into(),
                format!("Task {id}"),
                String::new(),
                "medium".into(),
                "bot".into(),
                "user-1".into(),
            );
            store.insert_task(&task).await.expect("insert");
        }
        store
            .update_task("archived-1", &serde_json::json!({ "archived": true }))
            .await
            .expect("archive");

        let listed = store.list_tasks(None, None, None).await.expect("list");
        let ids: Vec<&str> = listed.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"visible-1"));
        assert!(ids.contains(&"visible-2"));
        assert!(
            !ids.contains(&"archived-1"),
            "archived task must be hidden from the default list: {ids:?}"
        );

        // list_tasks_filtered shares the same default.
        let filtered = store
            .list_tasks_filtered(None, None, None, None)
            .await
            .expect("list filtered");
        assert!(!filtered.iter().any(|t| t.id == "archived-1"));
    }

    #[tokio::test]
    async fn list_tasks_paginated_can_browse_the_archive_explicitly() {
        let (store, _dir) = temp_store();
        for id in ["p-1", "p-2"] {
            let task = TaskRow::new(
                id.into(),
                format!("Task {id}"),
                String::new(),
                "medium".into(),
                "bot".into(),
                "user-1".into(),
            );
            store.insert_task(&task).await.expect("insert");
        }
        store
            .update_task("p-2", &serde_json::json!({ "archived": true }))
            .await
            .expect("archive p-2");

        // Default (archived=None) excludes the archived row.
        let (rows, total) = store
            .list_tasks_paginated(None, None, None, None, None, 50, 0)
            .await
            .expect("paginated default");
        assert_eq!(total, 1, "only p-1 is non-archived");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "p-1");

        // Explicit archived=true surfaces only the archived row.
        let (rows, total) = store
            .list_tasks_paginated(None, None, None, None, Some(true), 50, 0)
            .await
            .expect("paginated archived-only");
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, "p-2");
    }

    #[tokio::test]
    async fn list_tasks_paginated_reports_total_across_pages() {
        let (store, _dir) = temp_store();
        for i in 0..5 {
            let task = TaskRow::new(
                format!("page-{i}"),
                format!("Task {i}"),
                String::new(),
                "medium".into(),
                "bot".into(),
                "user-1".into(),
            );
            store.insert_task(&task).await.expect("insert");
        }
        let (first_page, total) = store
            .list_tasks_paginated(None, None, None, None, None, 2, 0)
            .await
            .expect("page 1");
        assert_eq!(total, 5, "total reflects the whole filtered set, not just this page");
        assert_eq!(first_page.len(), 2);

        let (second_page, total2) = store
            .list_tasks_paginated(None, None, None, None, None, 2, 2)
            .await
            .expect("page 2");
        assert_eq!(total2, 5);
        assert_eq!(second_page.len(), 2);

        // Pages don't overlap.
        let first_ids: HashSet<&str> = first_page.iter().map(|t| t.id.as_str()).collect();
        let second_ids: HashSet<&str> = second_page.iter().map(|t| t.id.as_str()).collect();
        assert!(first_ids.is_disjoint(&second_ids));
    }

    #[tokio::test]
    async fn pinned_tasks_sort_first() {
        let (store, _dir) = temp_store();
        for id in ["older", "newer"] {
            let task = TaskRow::new(
                id.into(),
                format!("Task {id}"),
                String::new(),
                "medium".into(),
                "bot".into(),
                "user-1".into(),
            );
            store.insert_task(&task).await.expect("insert");
        }
        // "newer" would naturally sort first (updated_at DESC on insert
        // order in a fresh store with monotonic timestamps isn't
        // guaranteed within the same tick, so pin "older" explicitly to
        // prove the ORDER BY clause, not insertion order, decides this).
        store
            .update_task("older", &serde_json::json!({ "pinned": true }))
            .await
            .expect("pin older");

        let listed = store.list_tasks(None, None, None).await.expect("list");
        assert_eq!(listed[0].id, "older", "pinned task must sort first regardless of recency");
    }

    #[tokio::test]
    async fn rename_via_update_task_title_field() {
        let (store, _dir) = temp_store();
        let task = TaskRow::new(
            "t-rename".into(),
            "Original title".into(),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert");

        let updated = store
            .update_task("t-rename", &serde_json::json!({ "title": "Renamed title" }))
            .await
            .expect("rename")
            .expect("row exists");
        assert_eq!(updated.title, "Renamed title");

        let got = store.get_task("t-rename").await.unwrap().unwrap();
        assert_eq!(got.title, "Renamed title");
    }

    #[tokio::test]
    async fn comment_insert_and_list_roundtrip_is_chronological() {
        let (store, _dir) = temp_store();
        // Seed a task so the comment references a real row.
        let task = TaskRow::new(
            "t1".into(),
            "Task One".into(),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert task");

        // Insert out of chronological order; list must return oldest-first.
        store
            .insert_comment(&comment("c2", "t1", "2026-07-10T10:05:00Z", "second"))
            .await
            .expect("insert c2");
        store
            .insert_comment(&comment("c1", "t1", "2026-07-10T10:00:00Z", "first"))
            .await
            .expect("insert c1");

        let rows = store.list_comments("t1").await.expect("list");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].body, "first", "oldest comment leads");
        assert_eq!(rows[1].body, "second");
        assert_eq!(rows[0].author_user, "user-1");
    }

    #[tokio::test]
    async fn comment_list_unknown_task_is_empty() {
        let (store, _dir) = temp_store();
        let rows = store.list_comments("does-not-exist").await.expect("list");
        assert!(rows.is_empty(), "no comments for an unknown task");
    }

    #[tokio::test]
    async fn reassign_open_tasks_moves_only_unfinished_work() {
        let (store, _dir) = temp_store();
        // Two open tasks + one done task, all owned by alice.
        for id in ["open1", "open2", "done1"] {
            let t = TaskRow::new(
                id.into(),
                format!("Task {id}"),
                String::new(),
                "medium".into(),
                "alice".into(),
                "user-1".into(),
            );
            store.insert_task(&t).await.expect("insert");
        }
        store
            .update_task("done1", &serde_json::json!({ "status": "done" }))
            .await
            .expect("mark done");

        let moved = store
            .reassign_open_tasks("alice", "bob", "2026-07-12T00:00:00Z")
            .await
            .expect("reassign");
        assert_eq!(moved, 2, "only the two open tasks move");

        // Bob now owns the open tasks; alice keeps the completed one.
        let bob = store.list_tasks(None, Some("bob"), None).await.unwrap();
        assert_eq!(bob.len(), 2);
        let alice = store.list_tasks(None, Some("alice"), None).await.unwrap();
        assert_eq!(alice.len(), 1, "done task stays with the original owner");
        assert_eq!(alice[0].id, "done1");

        // Idempotent: a re-run finds nothing left open for alice.
        let again = store
            .reassign_open_tasks("alice", "bob", "2026-07-12T00:01:00Z")
            .await
            .expect("reassign again");
        assert_eq!(again, 0);
    }

    fn edges(pairs: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        pairs
            .iter()
            .map(|(id, p)| (id.to_string(), p.map(|s| s.to_string())))
            .collect()
    }

    #[test]
    fn self_parent_is_cycle() {
        assert!(introduces_parent_cycle(&[], "a", "a"));
    }

    #[test]
    fn simple_acyclic_is_safe() {
        // a -> b -> c (root). Adding d's parent = a is safe.
        let e = edges(&[("a", Some("b")), ("b", Some("c")), ("c", None)]);
        assert!(!introduces_parent_cycle(&e, "d", "a"));
    }

    #[test]
    fn direct_back_edge_is_cycle() {
        // b's parent is a. Setting a's parent = b closes a 2-cycle.
        let e = edges(&[("b", Some("a")), ("a", None)]);
        assert!(introduces_parent_cycle(&e, "a", "b"));
    }

    #[test]
    fn deep_back_edge_is_cycle() {
        // a -> b -> c. Setting c's parent = a closes a 3-cycle.
        let e = edges(&[("a", Some("b")), ("b", Some("c")), ("c", None)]);
        assert!(introduces_parent_cycle(&e, "c", "a"));
    }

    #[test]
    fn unrelated_parent_is_safe() {
        let e = edges(&[("a", None), ("b", None), ("c", None)]);
        assert!(!introduces_parent_cycle(&e, "a", "b"));
    }

    // ── G1 dispatch: pure helpers ───────────────────────────

    #[test]
    fn parse_depends_on_handles_valid_and_malformed() {
        assert_eq!(parse_depends_on("[]"), Vec::<String>::new());
        assert_eq!(parse_depends_on(r#"["a","b"]"#), vec!["a", "b"]);
        // Malformed / non-array ⇒ empty (no deps), never a panic.
        assert!(parse_depends_on("not json").is_empty());
        assert!(parse_depends_on("{}").is_empty());
    }

    #[test]
    fn deps_satisfied_semantics() {
        let done: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        assert!(deps_satisfied(&[], &done), "no deps ⇒ satisfied");
        assert!(deps_satisfied(&["a".into(), "b".into()], &done));
        assert!(!deps_satisfied(&["a".into(), "c".into()], &done), "c not done");
    }

    #[test]
    fn lease_expiry_compares_timestamps() {
        assert!(lease_is_expired(
            "2026-07-11T10:00:00Z",
            "2026-07-11T10:00:01Z"
        ));
        assert!(!lease_is_expired(
            "2026-07-11T10:00:05Z",
            "2026-07-11T10:00:01Z"
        ));
        // Corrupt lease ⇒ treated as expired (fail-safe toward reclaim).
        assert!(lease_is_expired("garbage", "2026-07-11T10:00:01Z"));
    }

    #[test]
    fn zombie_action_respects_retry_budget() {
        assert_eq!(zombie_action(0, 3), ZombieAction::Requeue);
        assert_eq!(zombie_action(2, 3), ZombieAction::Requeue);
        assert_eq!(zombie_action(3, 3), ZombieAction::Fail);
        assert_eq!(zombie_action(5, 3), ZombieAction::Fail);
        assert_eq!(zombie_action(0, 0), ZombieAction::Fail, "zero budget");
    }

    // ── G1 dispatch: SQLite lifecycle ───────────────────────

    fn pending_task(id: &str) -> TaskRow {
        let mut t = TaskRow::new(
            id.into(),
            format!("task {id}"),
            String::new(),
            "medium".into(),
            String::new(),
            "system".into(),
        );
        t.status = "pending".into();
        t
    }

    #[tokio::test]
    async fn atomic_claim_is_exclusive_under_concurrency() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(TaskStore::open(dir.path()).expect("open"));
        store.insert_task(&pending_task("t1")).await.expect("insert");

        // Two workers race for the same task; exactly one may win.
        let s1 = store.clone();
        let s2 = store.clone();
        let now = "2026-07-11T10:00:00Z";
        let lease = "2026-07-11T10:05:00Z";
        let (r1, r2) = tokio::join!(
            async move { s1.atomic_claim("t1", "worker-a", now, lease).await.unwrap().is_claimed() },
            async move { s2.atomic_claim("t1", "worker-b", now, lease).await.unwrap().is_claimed() },
        );
        assert_ne!(r1, r2, "exactly one claimer wins");
        assert!(r1 ^ r2, "one true, one false");

        let t = store.get_task("t1").await.unwrap().unwrap();
        assert_eq!(t.status, "in_progress");
        assert!(matches!(t.claimed_by.as_deref(), Some("worker-a") | Some("worker-b")));

        // A third claim on an already-claimed task fails.
        assert!(!store
            .atomic_claim("t1", "worker-c", now, lease)
            .await
            .unwrap().is_claimed());
    }

    // ── M7: merge_goal_state_json read-merge-write ──────────

    #[tokio::test]
    async fn merge_goal_state_json_concurrent_merges_do_not_lose_either_write() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(TaskStore::open(dir.path()).expect("open"));
        store.insert_task(&pending_task("g1")).await.expect("insert");

        // Two concurrent merges, each touching a DIFFERENT key of the same
        // JSON blob — before M7 a naive read-then-`set_goal_state_json`
        // pattern would let whichever write lands second clobber the other's
        // key with a stale read.
        let s1 = store.clone();
        let s2 = store.clone();
        let (r1, r2) = tokio::join!(
            async move {
                s1.merge_goal_state_json("g1", |v| {
                    v["confirmed_facts"] = serde_json::json!(["fact one"]);
                })
                .await
            },
            async move {
                s2.merge_goal_state_json("g1", |v| {
                    v["pending_hypotheses"] = serde_json::json!(["hypothesis one"]);
                })
                .await
            },
        );
        r1.unwrap();
        r2.unwrap();

        let t = store.get_task("g1").await.unwrap().unwrap();
        let value: serde_json::Value =
            serde_json::from_str(t.goal_state_json.as_deref().unwrap()).unwrap();
        assert_eq!(value["confirmed_facts"], serde_json::json!(["fact one"]), "first writer's key must survive");
        assert_eq!(
            value["pending_hypotheses"],
            serde_json::json!(["hypothesis one"]),
            "second writer's key must survive too — neither merge may clobber the other"
        );
    }

    #[tokio::test]
    async fn merge_goal_state_json_degrades_malformed_existing_json_to_empty_object() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");
        store.insert_task(&pending_task("g2")).await.expect("insert");
        store.set_goal_state_json("g2", Some("not json at all")).await.unwrap();

        store
            .merge_goal_state_json("g2", |v| {
                v["confirmed_facts"] = serde_json::json!(["a"]);
            })
            .await
            .unwrap();

        let t = store.get_task("g2").await.unwrap().unwrap();
        let value: serde_json::Value =
            serde_json::from_str(t.goal_state_json.as_deref().unwrap()).unwrap();
        assert_eq!(value["confirmed_facts"], serde_json::json!(["a"]));
    }

    #[tokio::test]
    async fn merge_goal_state_json_starts_from_empty_object_when_column_is_null() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");
        store.insert_task(&pending_task("g3")).await.expect("insert");
        // goal_state_json starts NULL for a freshly inserted task.
        assert_eq!(store.get_task("g3").await.unwrap().unwrap().goal_state_json, None);

        store
            .merge_goal_state_json("g3", |v| {
                v["pending_hypotheses"] = serde_json::json!(["h"]);
            })
            .await
            .unwrap();

        let t = store.get_task("g3").await.unwrap().unwrap();
        let value: serde_json::Value =
            serde_json::from_str(t.goal_state_json.as_deref().unwrap()).unwrap();
        assert_eq!(value["pending_hypotheses"], serde_json::json!(["h"]));
    }

    #[tokio::test]
    async fn zombie_reclaim_requeues_then_fails_at_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");
        let mut t = pending_task("z1");
        t.max_retries = 1; // one requeue, then fail
        store.insert_task(&t).await.expect("insert");

        // Claim with a lease in the past so it's immediately a zombie.
        let past_lease = "2026-07-11T09:00:00Z";
        assert!(store
            .atomic_claim("z1", "w", "2026-07-11T08:55:00Z", past_lease)
            .await
            .unwrap().is_claimed());

        // First reclaim: retry_count 0 < 1 ⇒ requeue.
        let out = store.reclaim_zombies("2026-07-11T10:00:00Z").await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].action, ZombieAction::Requeue);
        let t1 = store.get_task("z1").await.unwrap().unwrap();
        assert_eq!(t1.status, "pending");
        assert_eq!(t1.retry_count, 1);
        assert!(t1.claimed_by.is_none() && t1.lease_expires_at.is_none());

        // Re-claim, expire again: retry_count 1 == max 1 ⇒ fail.
        assert!(store
            .atomic_claim("z1", "w", "2026-07-11T10:00:00Z", "2026-07-11T10:01:00Z")
            .await
            .unwrap().is_claimed());
        let out2 = store.reclaim_zombies("2026-07-11T11:00:00Z").await.unwrap();
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].action, ZombieAction::Fail);
        assert_eq!(store.get_task("z1").await.unwrap().unwrap().status, "failed");
    }

    #[tokio::test]
    async fn zombie_reclaim_ignores_unexpired_and_unleased() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");

        // Fresh lease far in the future — not a zombie.
        store.insert_task(&pending_task("live")).await.unwrap();
        assert!(store
            .atomic_claim("live", "w", "2026-07-11T10:00:00Z", "2026-07-11T23:00:00Z")
            .await
            .unwrap().is_claimed());

        // Manual board task: in_progress but NULL lease — must be left alone.
        let mut manual = pending_task("manual");
        manual.status = "in_progress".into();
        store.insert_task(&manual).await.unwrap();

        let out = store.reclaim_zombies("2026-07-11T10:05:00Z").await.unwrap();
        assert!(out.is_empty(), "nothing reclaimed");
        assert_eq!(store.get_task("live").await.unwrap().unwrap().status, "in_progress");
        assert_eq!(store.get_task("manual").await.unwrap().unwrap().status, "in_progress");
    }

    #[tokio::test]
    async fn dependency_gating_blocks_until_deps_done() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");

        store.insert_task(&pending_task("dep")).await.unwrap();
        let mut child = pending_task("child");
        child.depends_on = r#"["dep"]"#.into();
        store.insert_task(&child).await.unwrap();

        // While `dep` is pending, only `dep` is claimable.
        let claimable = store.claimable_tasks().await.unwrap();
        let ids: HashSet<_> = claimable.iter().map(|t| t.id.clone()).collect();
        assert!(ids.contains("dep"));
        assert!(!ids.contains("child"), "child gated by unmet dep");

        // Complete `dep` → child unlocks.
        store.complete_task("dep", "done", "system").await.unwrap();
        let claimable2 = store.claimable_tasks().await.unwrap();
        let ids2: HashSet<_> = claimable2.iter().map(|t| t.id.clone()).collect();
        assert!(ids2.contains("child"), "child claimable once dep done");
    }

    /// WP-10B: archiving a pending/unclaimed task (the `/goals` board
    /// "take out of active consideration" action) must remove it from the
    /// dispatch engine's pickup queue — mirrors the existing
    /// `list_tasks_filtered_excludes_archived_by_default` guarantee.
    #[tokio::test]
    async fn claimable_tasks_excludes_archived() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");

        store.insert_task(&pending_task("visible")).await.unwrap();
        store.insert_task(&pending_task("archived")).await.unwrap();
        store
            .update_task("archived", &serde_json::json!({ "archived": true }))
            .await
            .unwrap();

        let claimable = store.claimable_tasks().await.unwrap();
        let ids: HashSet<_> = claimable.iter().map(|t| t.id.clone()).collect();
        assert!(ids.contains("visible"), "non-archived task stays claimable");
        assert!(
            !ids.contains("archived"),
            "archived task must not be claimable by the dispatch engine: {ids:?}"
        );
    }

    #[tokio::test]
    async fn goal_mode_completion_routes_to_review_then_accept_reject() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");

        let mut g = pending_task("goal");
        g.goal_mode = true;
        g.max_retries = 1;
        g.acceptance_criteria = Some("must compile".into());
        store.insert_task(&g).await.unwrap();

        // Completion of a goal-mode task parks in `review`, not `done`.
        let updated = store.complete_task("goal", "did the thing", "w").await.unwrap().unwrap();
        assert_eq!(updated.status, "review");
        assert_eq!(updated.result_summary.as_deref(), Some("did the thing"));

        // Reject → routes to `revising` (Iterative Kanban, retry 0 < 1) with
        // feedback and the round counter bumped.
        let status = store.reject_review("goal", "criteria not met", 3).await.unwrap();
        assert_eq!(status, "revising");
        let t = store.get_task("goal").await.unwrap().unwrap();
        assert_eq!(t.retry_count, 1);
        assert_eq!(t.revision_round, 1);
        assert_eq!(t.judge_feedback.as_deref(), Some("criteria not met"));

        // Complete again → review → reject at cap ⇒ needs_human (fail-safe).
        store.complete_task("goal", "attempt 2", "w").await.unwrap();
        let status2 = store.reject_review("goal", "still failing", 3).await.unwrap();
        assert_eq!(status2, "needs_human");
        assert_eq!(store.get_task("goal").await.unwrap().unwrap().status, "needs_human");
    }

    #[tokio::test]
    async fn complete_task_does_not_overwrite_terminal_state() {
        // A stale worker completing an already-`done` task must not clobber the
        // authoritative result (the `status NOT IN ('done','cancelled')` guard).
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");
        store.insert_task(&pending_task("t")).await.unwrap();

        let first = store.complete_task("t", "authoritative result", "w").await.unwrap().unwrap();
        assert_eq!(first.status, "done");
        assert_eq!(first.result_summary.as_deref(), Some("authoritative result"));

        // Second (stale) completion is a no-op on the terminal row.
        let second = store.complete_task("t", "stale overwrite", "w").await.unwrap().unwrap();
        assert_eq!(second.status, "done", "still done");
        assert_eq!(
            second.result_summary.as_deref(),
            Some("authoritative result"),
            "stale complete must not overwrite the first result"
        );
    }

    #[tokio::test]
    async fn goal_mode_accept_promotes_to_done() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");
        let mut g = pending_task("goal2");
        g.goal_mode = true;
        store.insert_task(&g).await.unwrap();
        store.complete_task("goal2", "result", "w").await.unwrap();
        assert!(store.accept_review("goal2", "criteria met").await.unwrap());
        let t = store.get_task("goal2").await.unwrap().unwrap();
        assert_eq!(t.status, "done");
        assert!(t.completed_at.is_some());
    }

    // ── G1 lease renewal: conservative reclaim ──────────────

    #[test]
    fn zombie_reclaim_due_semantics() {
        let lease = "2026-07-11T10:05:00Z";
        let anchor = Some("2026-07-11T10:00:00Z"); // window = 5 min
        // Lease still live ⇒ never due.
        assert!(!zombie_reclaim_due(lease, anchor, "2026-07-11T10:04:00Z"));
        // Expired but within the grace window (one further full lease window).
        assert!(!zombie_reclaim_due(lease, anchor, "2026-07-11T10:06:00Z"));
        assert!(!zombie_reclaim_due(lease, anchor, "2026-07-11T10:09:59Z"));
        // Expired + full extra window elapsed with no renewal ⇒ due.
        assert!(zombie_reclaim_due(lease, anchor, "2026-07-11T10:10:00Z"));
        // Legacy row (no anchor): zero grace ⇒ due at plain expiry.
        assert!(zombie_reclaim_due(lease, None, "2026-07-11T10:05:00Z"));
        // Corrupt lease ⇒ due (must not pin a zombie forever).
        assert!(zombie_reclaim_due("garbage", anchor, "2026-07-11T10:00:00Z"));
        // Corrupt anchor degrades to zero grace, not a panic.
        assert!(zombie_reclaim_due(lease, Some("garbage"), "2026-07-11T10:05:00Z"));
    }

    #[tokio::test]
    async fn renewed_lease_survives_reclaim_and_abandoned_claim_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");
        store.insert_task(&pending_task("held")).await.unwrap();
        store.insert_task(&pending_task("abandoned")).await.unwrap();

        // Both claimed at 10:00 with a 5-minute lease.
        for id in ["held", "abandoned"] {
            assert!(store
                .atomic_claim(id, "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
                .await
                .unwrap().is_claimed());
        }
        // `held`'s worker heartbeats at 10:04 → lease pushed to 10:09.
        assert!(store
            .renew_lease("held", "w", "2026-07-11T10:09:00Z", "2026-07-11T10:04:00Z")
            .await
            .unwrap());

        // At 10:11: `abandoned` expired at 10:05 with a 5-min window ⇒ due at
        // 10:10 ⇒ reclaimed. `held` expires at 10:09, window 5 min ⇒ due only
        // at 10:14 ⇒ untouched.
        let out = store.reclaim_zombies("2026-07-11T10:11:00Z").await.unwrap();
        let ids: Vec<_> = out.iter().map(|o| o.task_id.as_str()).collect();
        assert_eq!(ids, vec!["abandoned"]);
        assert_eq!(store.get_task("held").await.unwrap().unwrap().status, "in_progress");
        assert_eq!(
            store.get_task("abandoned").await.unwrap().unwrap().status,
            "pending"
        );
    }

    #[tokio::test]
    async fn renew_lease_is_guarded_to_the_holder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open");
        store.insert_task(&pending_task("t")).await.unwrap();
        assert!(store
            .atomic_claim("t", "owner", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap().is_claimed());
        // Another agent cannot renew someone else's lease.
        assert!(!store
            .renew_lease("t", "intruder", "2026-07-11T10:30:00Z", "2026-07-11T10:01:00Z")
            .await
            .unwrap());
        let t = store.get_task("t").await.unwrap().unwrap();
        assert_eq!(t.lease_expires_at.as_deref(), Some("2026-07-11T10:05:00Z"));
    }

    // ── G8 goal chain ───────────────────────────────────────

    fn goal(id: &str, title: &str, parent: Option<&str>) -> GoalRow {
        let mut g = GoalRow::new(id.into(), title.into(), format!("why of {id}"));
        g.parent_goal_id = parent.map(String::from);
        g
    }

    #[tokio::test]
    async fn goal_crud_and_ancestry_is_root_first() {
        let (store, _dir) = temp_store();
        store.insert_goal(&goal("init", "Initiative", None)).await.unwrap();
        store.insert_goal(&goal("proj", "Project", Some("init"))).await.unwrap();
        store.insert_goal(&goal("issue", "Issue", Some("proj"))).await.unwrap();

        let chain = store.goal_ancestry("issue").await.unwrap();
        let titles: Vec<_> = chain.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, vec!["Initiative", "Project", "Issue"]);

        // Unknown goal ⇒ empty chain, not an error.
        assert!(store.goal_ancestry("nope").await.unwrap().is_empty());

        let active = store.list_goals(Some("active")).await.unwrap();
        assert_eq!(active.len(), 3);
        assert!(store.list_goals(Some("done")).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn goal_create_rejects_missing_parent_and_update_rejects_cycle() {
        let (store, _dir) = temp_store();
        // Missing parent ⇒ fail-closed.
        assert!(store
            .insert_goal(&goal("orphan", "Orphan", Some("ghost")))
            .await
            .is_err());

        store.insert_goal(&goal("a", "A", None)).await.unwrap();
        store.insert_goal(&goal("b", "B", Some("a"))).await.unwrap();
        // Re-parenting a under b closes a 2-cycle ⇒ rejected.
        let err = store
            .update_goal("a", &serde_json::json!({ "parent_goal_id": "b" }))
            .await;
        assert!(err.is_err(), "cycle must be rejected");
        // Self-parent is a trivial cycle.
        assert!(store
            .update_goal("a", &serde_json::json!({ "parent_goal_id": "a" }))
            .await
            .is_err());
        // Legit update still works.
        let g = store
            .update_goal("b", &serde_json::json!({ "status": "done" }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(g.status, "done");
    }

    #[tokio::test]
    async fn task_goal_id_roundtrips() {
        let (store, _dir) = temp_store();
        store.insert_goal(&goal("g", "Goal", None)).await.unwrap();
        let mut t = pending_task("t");
        t.goal_id = Some("g".into());
        store.insert_task(&t).await.unwrap();
        let got = store.get_task("t").await.unwrap().unwrap();
        assert_eq!(got.goal_id.as_deref(), Some("g"));
    }

    // ── depends_on cycle validation ─────────────────────────

    #[test]
    fn dependency_cycle_detection() {
        let edges = vec![
            ("a".to_string(), vec!["b".to_string()]),
            ("b".to_string(), vec!["c".to_string()]),
            ("c".to_string(), Vec::new()),
        ];
        // Self-dependency.
        assert!(introduces_dependency_cycle(&edges, "a", &["a".into()]));
        // c → a closes a 3-cycle (a → b → c already exists).
        assert!(introduces_dependency_cycle(&edges, "c", &["a".into()]));
        // Unrelated / forward deps are fine.
        assert!(!introduces_dependency_cycle(&edges, "d", &["a".into()]));
        assert!(!introduces_dependency_cycle(&edges, "a", &["c".into()]));
        assert!(!introduces_dependency_cycle(&edges, "a", &[]));
    }

    #[tokio::test]
    async fn update_task_rejects_dependency_cycle() {
        let (store, _dir) = temp_store();
        store.insert_task(&pending_task("t1")).await.unwrap();
        let mut t2 = pending_task("t2");
        t2.depends_on = r#"["t1"]"#.into();
        store.insert_task(&t2).await.unwrap();

        // t1 depending on t2 would close t1 → t2 → t1.
        let res = store
            .update_task("t1", &serde_json::json!({ "depends_on": "[\"t2\"]" }))
            .await;
        assert!(res.is_err(), "dependency cycle must be rejected");

        // Malformed depends_on is rejected fail-closed, not silently stored.
        assert!(store
            .update_task("t1", &serde_json::json!({ "depends_on": "not json" }))
            .await
            .is_err());

        // A legal rewire is accepted.
        let ok = store
            .update_task("t2", &serde_json::json!({ "depends_on": "[]" }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ok.depends_on, "[]");
    }

    // ── HIGH-1: dependency gating at the claim boundary ──────

    #[tokio::test]
    async fn atomic_claim_is_gated_by_unfinished_dependencies() {
        let (store, _dir) = temp_store();
        store.insert_task(&pending_task("dep")).await.unwrap();
        let mut child = pending_task("child");
        child.depends_on = r#"["dep","ghost"]"#.into();
        store.insert_task(&child).await.unwrap();

        // Unmet deps (including a dep referencing a MISSING task — fail-closed)
        // block the claim and are named in the outcome.
        let out = store
            .atomic_claim("child", "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap();
        match out {
            super::ClaimOutcome::BlockedByDeps(unmet) => {
                assert_eq!(unmet, vec!["dep".to_string(), "ghost".to_string()]);
            }
            other => panic!("expected BlockedByDeps, got {other:?}"),
        }
        let t = store.get_task("child").await.unwrap().unwrap();
        assert_eq!(t.status, "pending", "blocked claim must not mutate the task");
        assert!(t.claimed_by.is_none());

        // Finish `dep`; `ghost` still missing ⇒ still blocked (fail-closed).
        store.complete_task("dep", "done", "system").await.unwrap();
        assert!(matches!(
            store
                .atomic_claim("child", "w", "2026-07-11T10:06:00Z", "2026-07-11T10:11:00Z")
                .await
                .unwrap(),
            super::ClaimOutcome::BlockedByDeps(ref unmet) if unmet == &vec!["ghost".to_string()]
        ));

        // Drop the ghost dep → claimable.
        store
            .update_task("child", &serde_json::json!({ "depends_on": "[\"dep\"]" }))
            .await
            .unwrap();
        assert!(store
            .atomic_claim("child", "w", "2026-07-11T10:07:00Z", "2026-07-11T10:12:00Z")
            .await
            .unwrap()
            .is_claimed());
    }

    // ── HIGH-2: holder-guarded completion ────────────────────

    #[tokio::test]
    async fn complete_task_is_guarded_to_the_claim_holder() {
        let (store, _dir) = temp_store();
        store.insert_task(&pending_task("t")).await.unwrap();
        assert!(store
            .atomic_claim("t", "owner", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap()
            .is_claimed());

        // A zombie worker (reclaimed elsewhere, stale identity) cannot clobber
        // the holder's in_progress task.
        let err = store.complete_task("t", "stale result", "zombie").await;
        assert!(err.is_err(), "non-holder completion must error");
        let t = store.get_task("t").await.unwrap().unwrap();
        assert_eq!(t.status, "in_progress", "task untouched by the intruder");
        assert!(t.result_summary.is_none());

        // The holder completes normally.
        let done = store
            .complete_task("t", "real result", "owner")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.status, "done");
        assert_eq!(done.result_summary.as_deref(), Some("real result"));
    }

    #[tokio::test]
    async fn complete_task_unclaimed_keeps_legacy_any_caller_behavior() {
        let (store, _dir) = temp_store();
        store.insert_task(&pending_task("legacy")).await.unwrap();
        // Unclaimed (claimed_by IS NULL) → any caller may complete (legacy
        // board-task behavior preserved).
        let done = store
            .complete_task("legacy", "ok", "anyone")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.status, "done");
    }

    // ── MED: zombie reclaim lease CAS ────────────────────────

    #[tokio::test]
    async fn zombie_reclaim_cas_misses_when_renewal_landed_after_scan() {
        let (store, _dir) = temp_store();
        store.insert_task(&pending_task("t")).await.unwrap();
        let scanned_lease = "2026-07-11T10:05:00Z";
        assert!(store
            .atomic_claim("t", "w", "2026-07-11T10:00:00Z", scanned_lease)
            .await
            .unwrap()
            .is_claimed());

        // A renewal lands between the zombie scan and the requeue write —
        // the CAS on the scanned lease value must miss and leave the claim.
        assert!(store
            .renew_lease("t", "w", "2026-07-11T10:20:00Z", "2026-07-11T10:04:00Z")
            .await
            .unwrap());
        let requeued = store
            .requeue_zombie_cas("t", "w", scanned_lease, 1, "2026-07-11T10:16:00Z")
            .await
            .unwrap();
        assert!(!requeued, "stale scanned lease must not requeue a renewed claim");
        let t = store.get_task("t").await.unwrap().unwrap();
        assert_eq!(t.status, "in_progress");
        assert_eq!(t.claimed_by.as_deref(), Some("w"));
        assert_eq!(t.retry_count, 0);

        // Same race on the fail path.
        let failed = store
            .fail_zombie_cas("t", "w", scanned_lease, "2026-07-11T10:16:00Z")
            .await
            .unwrap();
        assert!(!failed, "stale scanned lease must not fail a renewed claim");
        assert_eq!(
            store.get_task("t").await.unwrap().unwrap().status,
            "in_progress"
        );

        // With the CURRENT lease value the CAS applies (the genuine zombie path).
        let requeued2 = store
            .requeue_zombie_cas("t", "w", "2026-07-11T10:20:00Z", 1, "2026-07-11T10:30:00Z")
            .await
            .unwrap();
        assert!(requeued2);
        assert_eq!(store.get_task("t").await.unwrap().unwrap().status, "pending");
    }

    // ── U4 co-edited plans ───────────────────────────────────

    use super::{plan_order_for_insert, PlanRow, PLAN_STEP_ORDER_GAP};

    fn plan(id: &str, agent: &str) -> PlanRow {
        PlanRow::new(id.into(), format!("Plan {id}"), agent.into(), "user-1".into())
    }

    #[test]
    fn plan_order_for_insert_semantics() {
        // Empty plan: first step lands at one gap.
        assert_eq!(plan_order_for_insert(&[], 0), Some(PLAN_STEP_ORDER_GAP));
        // Append always succeeds at last + GAP.
        assert_eq!(
            plan_order_for_insert(&[1024, 2048], 2),
            Some(2048 + PLAN_STEP_ORDER_GAP)
        );
        // Between two neighbours ⇒ midpoint.
        assert_eq!(plan_order_for_insert(&[1024, 2048], 1), Some(1536));
        // Front ⇒ midpoint of (0, first).
        assert_eq!(plan_order_for_insert(&[1024, 2048], 0), Some(512));
        // Exhausted gap (adjacent keys) ⇒ None — caller renormalizes.
        assert_eq!(plan_order_for_insert(&[5, 6], 1), None);
        assert_eq!(plan_order_for_insert(&[1], 0), None);
    }

    #[tokio::test]
    async fn plan_steps_append_insert_and_move_keep_order() {
        let (store, _dir) = temp_store();
        store.insert_plan(&plan("p1", "bot")).await.unwrap();

        // Append three steps.
        for (id, text) in [("s1", "first"), ("s2", "second"), ("s3", "third")] {
            store
                .add_plan_step("p1", id, text, "agent", "bot", None)
                .await
                .unwrap();
        }
        let texts = |steps: &[super::PlanStepRow]| -> Vec<String> {
            steps.iter().map(|s| s.text.clone()).collect()
        };
        let steps = store.list_plan_steps("p1").await.unwrap();
        assert_eq!(texts(&steps), vec!["first", "second", "third"]);

        // Insert at index 1 (between first and second).
        store
            .add_plan_step("p1", "s4", "one-point-five", "user", "louis", Some(1))
            .await
            .unwrap();
        let steps = store.list_plan_steps("p1").await.unwrap();
        assert_eq!(texts(&steps), vec!["first", "one-point-five", "second", "third"]);

        // Move "third" to the front.
        assert!(store.move_plan_step("p1", "s3", 0).await.unwrap());
        let steps = store.list_plan_steps("p1").await.unwrap();
        assert_eq!(texts(&steps), vec!["third", "first", "one-point-five", "second"]);

        // Move front step to the end (index clamps to len-1).
        assert!(store.move_plan_step("p1", "s3", 99).await.unwrap());
        let steps = store.list_plan_steps("p1").await.unwrap();
        assert_eq!(texts(&steps), vec!["first", "one-point-five", "second", "third"]);

        // Moving an unknown step is a no-op `false`, not an error.
        assert!(!store.move_plan_step("p1", "ghost", 0).await.unwrap());
    }

    #[tokio::test]
    async fn plan_step_front_inserts_renormalize_when_gap_exhausted() {
        let (store, _dir) = temp_store();
        store.insert_plan(&plan("p1", "bot")).await.unwrap();
        store
            .add_plan_step("p1", "base", "base", "agent", "bot", None)
            .await
            .unwrap();
        // Repeated front inserts halve the head gap (1024 → 512 → 256 → …);
        // past ~10 inserts the midpoint collides and renormalization must kick
        // in transparently. 16 inserts forces at least one renormalize pass.
        for i in 0..16 {
            store
                .add_plan_step("p1", &format!("f{i}"), &format!("front {i}"), "user", "u", Some(0))
                .await
                .unwrap();
        }
        let steps = store.list_plan_steps("p1").await.unwrap();
        assert_eq!(steps.len(), 17);
        // Newest front insert leads; original base is last.
        assert_eq!(steps.first().unwrap().text, "front 15");
        assert_eq!(steps.last().unwrap().text, "base");
        // Orders are strictly increasing (total order held through renorms).
        let orders: Vec<i64> = steps.iter().map(|s| s.step_order).collect();
        assert!(orders.windows(2).all(|w| w[0] < w[1]), "orders strictly ascend: {orders:?}");
    }

    #[tokio::test]
    async fn plan_step_update_validates_enums_fail_closed() {
        let (store, _dir) = temp_store();
        store.insert_plan(&plan("p1", "bot")).await.unwrap();
        store
            .add_plan_step("p1", "s1", "step", "agent", "bot", None)
            .await
            .unwrap();

        // Invalid enum values are rejected, valid ones apply.
        assert!(store
            .update_plan_step("s1", &serde_json::json!({ "status": "nonsense" }))
            .await
            .is_err());
        assert!(store
            .update_plan_step("s1", &serde_json::json!({ "assignee_kind": "alien" }))
            .await
            .is_err());
        assert!(store
            .update_plan_step("s1", &serde_json::json!({ "text": "   " }))
            .await
            .is_err());
        let updated = store
            .update_plan_step(
                "s1",
                &serde_json::json!({ "status": "done", "assignee_kind": "user", "assignee": "louis" }),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, "done");
        assert_eq!(updated.assignee_kind, "user");
        assert_eq!(updated.assignee, "louis");

        // Invalid add-time assignee_kind also rejected.
        assert!(store
            .add_plan_step("p1", "s2", "x", "robot", "", None)
            .await
            .is_err());
        // Unknown plan rejected (no dangling steps).
        assert!(store
            .add_plan_step("ghost-plan", "s3", "x", "agent", "bot", None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn plan_crud_and_remove_cascades_steps() {
        let (store, _dir) = temp_store();
        store.insert_plan(&plan("p1", "bot")).await.unwrap();
        store
            .add_plan_step("p1", "s1", "step", "agent", "bot", None)
            .await
            .unwrap();

        // Update plan fields; invalid status fail-closed.
        assert!(store
            .update_plan("p1", &serde_json::json!({ "status": "bogus" }))
            .await
            .is_err());
        let p = store
            .update_plan("p1", &serde_json::json!({ "title": "Renamed", "status": "done" }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(p.title, "Renamed");
        assert_eq!(p.status, "done");

        // Remove step returns the removed row.
        let removed = store.remove_plan_step("s1").await.unwrap().unwrap();
        assert_eq!(removed.plan_id, "p1");
        assert!(store.remove_plan_step("s1").await.unwrap().is_none());

        // Remove plan cascades remaining steps.
        store
            .add_plan_step("p1", "s2", "another", "user", "", None)
            .await
            .unwrap();
        assert!(store.remove_plan("p1").await.unwrap());
        assert!(store.get_plan("p1").await.unwrap().is_none());
        assert!(store.list_plan_steps("p1").await.unwrap().is_empty());
        assert!(store.get_plan_step("s2").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn plan_prompt_section_is_byte_stable_and_scoped_to_agent_steps() {
        let (store, _dir) = temp_store();
        store.insert_plan(&plan("p1", "bot")).await.unwrap();
        store
            .add_plan_step("p1", "s1", "agent does this", "agent", "bot", None)
            .await
            .unwrap();
        store
            .add_plan_step("p1", "s2", "user does that", "user", "louis", None)
            .await
            .unwrap();

        let a = store.plan_prompt_section("bot").await.unwrap().unwrap();
        let b = store.plan_prompt_section("bot").await.unwrap().unwrap();
        assert_eq!(a, b, "byte-stable when rows unchanged (prompt-cache friendly)");
        assert!(a.contains("← yours"), "agent's own step marked");
        assert!(a.contains("plan_update_step"));

        // Another agent with no steps in the plan gets nothing.
        assert!(store.plan_prompt_section("other").await.unwrap().is_none());

        // Once the agent's steps are all done, the section disappears.
        store
            .update_plan_step("s1", &serde_json::json!({ "status": "done" }))
            .await
            .unwrap();
        assert!(store.plan_prompt_section("bot").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn migration_is_idempotent_across_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Open, close, reopen — the ALTER guard must not error on second run.
        {
            let s = TaskStore::open(dir.path()).expect("first open");
            s.insert_task(&pending_task("m1")).await.unwrap();
        }
        let s2 = TaskStore::open(dir.path()).expect("reopen");
        assert_eq!(s2.get_task("m1").await.unwrap().unwrap().status, "pending");
    }

    // ── Goal assignment form v2 (design-market-belief-loop-2026-08.md §6,
    // G1, 2026-08-14) ────────────────────────────────────────

    #[tokio::test]
    async fn deadline_and_risk_boundary_round_trip_through_insert_and_read() {
        let (store, _dir) = temp_store();
        let mut t = pending_task("g1");
        t.deadline_at = Some("2026-08-20T00:00:00Z".to_string());
        t.risk_boundary = Some("不得動用生產資料庫寫入權限".to_string());
        store.insert_task(&t).await.unwrap();

        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.deadline_at.as_deref(), Some("2026-08-20T00:00:00Z"));
        assert_eq!(got.risk_boundary.as_deref(), Some("不得動用生產資料庫寫入權限"));

        // A task that never sets them stays NULL (baseline applies at the
        // injection layer, never stored here — see the field doc comments).
        let bare = store.get_task("m1_never_inserted").await.unwrap();
        assert!(bare.is_none());
        let plain = pending_task("g2");
        store.insert_task(&plain).await.unwrap();
        let got2 = store.get_task("g2").await.unwrap().unwrap();
        assert!(got2.deadline_at.is_none());
        assert!(got2.risk_boundary.is_none());
    }

    // ── H9-G goal contract freeze (harness-borrowings 2026-08 WP-D) ─────

    #[tokio::test]
    async fn acceptance_criteria_baseline_round_trips_through_insert_and_read() {
        let (store, _dir) = temp_store();
        let mut t = pending_task("gb1");
        t.acceptance_criteria = Some("current criteria".into());
        t.acceptance_criteria_baseline = Some("frozen criteria".into());
        store.insert_task(&t).await.unwrap();

        let got = store.get_task("gb1").await.unwrap().unwrap();
        assert_eq!(got.acceptance_criteria.as_deref(), Some("current criteria"));
        assert_eq!(
            got.acceptance_criteria_baseline.as_deref(),
            Some("frozen criteria")
        );

        // A task that never sets a baseline stays NULL — readers fall back to
        // `acceptance_criteria` at the consumer layer (dispatch_engine.rs).
        let plain = pending_task("gb2");
        store.insert_task(&plain).await.unwrap();
        let got2 = store.get_task("gb2").await.unwrap().unwrap();
        assert!(got2.acceptance_criteria_baseline.is_none());
    }

    #[tokio::test]
    async fn update_task_can_edit_acceptance_criteria_but_never_touches_the_baseline() {
        // Store layer is identity-agnostic (authorization lives at the MCP /
        // dashboard boundary — see mcp.rs::handle_tasks_update and
        // handlers.rs::handle_tasks_update); this only asserts the SQL-level
        // invariant: `acceptance_criteria` is updatable, `acceptance_criteria_baseline`
        // has no write path at all after `insert_task`.
        let (store, _dir) = temp_store();
        let mut t = pending_task("gb3");
        t.acceptance_criteria = Some("original".into());
        t.acceptance_criteria_baseline = Some("frozen forever".into());
        store.insert_task(&t).await.unwrap();

        let updated = store
            .update_task(
                "gb3",
                &serde_json::json!({ "acceptance_criteria": "edited by operator" }),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.acceptance_criteria.as_deref(),
            Some("edited by operator")
        );
        assert_eq!(
            updated.acceptance_criteria_baseline.as_deref(),
            Some("frozen forever"),
            "the baseline must never change, regardless of who calls update_task"
        );
    }

    #[tokio::test]
    async fn deadline_and_risk_boundary_migration_idempotent_and_old_rows_default_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Simulate a pre-goal-form-v2 db: insert a task, close, reopen twice —
        // the ALTER guard must not error on re-run, and a row inserted before
        // the columns existed must read back as NULL (never a spurious
        // default) rather than panicking on a missing column.
        {
            let s = TaskStore::open(dir.path()).expect("first open");
            s.insert_task(&pending_task("old1")).await.unwrap();
        }
        {
            // Re-running the ALTER guard on an already-migrated db must not
            // error (idempotency across reopens, mirrors
            // `migration_is_idempotent_across_reopens`).
            let s2 = TaskStore::open(dir.path()).expect("second open");
            let t = s2.get_task("old1").await.unwrap().unwrap();
            assert!(t.deadline_at.is_none());
            assert!(t.risk_boundary.is_none());
        }
        let s3 = TaskStore::open(dir.path()).expect("third open (re-run ALTER again)");
        let t = s3.get_task("old1").await.unwrap().unwrap();
        assert!(t.deadline_at.is_none());
        assert!(t.risk_boundary.is_none());
    }

    // ── Iterative Kanban (v1.45) ────────────────────────────

    /// A goal-mode task in `review`, ready for a judge verdict.
    fn goal_review_task(id: &str) -> TaskRow {
        let mut t = TaskRow::new(
            id.into(),
            format!("goal {id}"),
            "do the work".into(),
            "medium".into(),
            "alice".into(),
            "system".into(),
        );
        t.status = "review".into();
        t.goal_mode = true;
        t.max_retries = 5;
        t.acceptance_criteria = Some("must be correct".into());
        t.result_summary = Some("attempt".into());
        t
    }

    #[tokio::test]
    async fn reject_review_routes_to_revising_and_bumps_round() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        store.insert_task(&goal_review_task("g1")).await.unwrap();
        // Round 1 was dispatched (driver) before the judge ruled.
        store.record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z").await.unwrap();

        let status = store.reject_review("g1", "missing summary", 3).await.unwrap();
        assert_eq!(status, "revising");
        let t = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(t.status, "revising");
        assert_eq!(t.revision_round, 1);
        assert!(!t.diminishing, "one round is below soft cap 3");
        assert_eq!(t.retry_count, 1);
        assert_eq!(t.judge_feedback.as_deref(), Some("missing summary"));
        // Claim/lease/result cleared so the loop can re-dispatch it.
        assert!(t.claimed_by.is_none());
        assert!(t.result_summary.is_none());
        // A rejection verdict is sealed in the iteration timeline.
        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters.len(), 1);
        assert_eq!(iters[0].verdict.as_deref(), Some("rejected"));
        assert_eq!(iters[0].judge_feedback.as_deref(), Some("missing summary"));
    }

    /// 2026-08-14: the structured panel verdict is persisted verbatim, a
    /// stall re-dispatch bumps `dispatch_count` while keeping the original
    /// `dispatched_at`, and the visit-graph signal lands on the row.
    #[tokio::test]
    async fn iteration_rows_carry_verdict_json_and_dispatch_signal() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        store.insert_task(&goal_review_task("g1")).await.unwrap();
        store
            .record_iteration_dispatch_with_state(
                "g1", 1, "2026-08-14T10:00:00Z", Some("hash-a"), Some(1),
            )
            .await
            .unwrap();
        // Stall re-dispatch of the same round: count bumps, timestamp stays,
        // refreshed streak wins.
        store
            .record_iteration_dispatch_with_state(
                "g1", 1, "2026-08-14T10:30:00Z", Some("hash-a"), Some(2),
            )
            .await
            .unwrap();
        let aspects = r#"[{"name":"correctness","pass":true,"reason":""},{"name":"safety","pass":false,"reason":"deleted prod"}]"#;
        store
            .reject_review_with_verdict("g1", "[safety] deleted prod", 3, Some(aspects))
            .await
            .unwrap();

        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters.len(), 1);
        let it = &iters[0];
        assert_eq!(it.dispatched_at, "2026-08-14T10:00:00Z");
        assert_eq!(it.dispatch_count, 2);
        assert_eq!(it.state_hash.as_deref(), Some("hash-a"));
        assert_eq!(it.repeat_streak, Some(2));
        let parsed: serde_json::Value =
            serde_json::from_str(it.verdict_json.as_deref().unwrap()).unwrap();
        assert_eq!(parsed[1]["name"], "safety");
        assert_eq!(parsed[1]["pass"], false);
        // Legacy wrapper still writes rows without a panel.
        store.insert_task(&goal_review_task("g2")).await.unwrap();
        store.record_iteration_dispatch("g2", 1, "2026-08-14T11:00:00Z").await.unwrap();
        store.reject_review("g2", "nope", 3).await.unwrap();
        let g2 = store.list_iterations("g2").await.unwrap();
        assert!(g2[0].verdict_json.is_none());
        assert_eq!(g2[0].dispatch_count, 1);
    }

    #[tokio::test]
    async fn revising_task_is_claimable_for_next_round() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        store.insert_task(&goal_review_task("g1")).await.unwrap();
        store.reject_review("g1", "again", 3).await.unwrap();

        // atomic_claim accepts `revising` exactly like `pending`: round+1 work
        // moves it back to in_progress under the claimer.
        let out = store
            .atomic_claim("g1", "alice", "2026-07-25T10:00:00Z", "2026-07-25T10:05:00Z")
            .await
            .unwrap();
        assert!(out.is_claimed());
        let t = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(t.status, "in_progress");
        assert_eq!(t.claimed_by.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn soft_cap_raises_diminishing_flag() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.revision_round = 2; // next reject → round 3 == soft cap
        store.insert_task(&t).await.unwrap();

        store.reject_review("g1", "still wrong", 3).await.unwrap();
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.revision_round, 3);
        assert!(got.diminishing, "reaching soft cap 3 raises the diminishing flag");
        assert_eq!(got.status, "revising", "soft cap flags but never blocks");
    }

    #[tokio::test]
    async fn reject_review_escalates_when_retry_budget_spent() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.max_retries = 1;
        t.retry_count = 1; // budget already spent
        store.insert_task(&t).await.unwrap();
        store.record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z").await.unwrap();

        let status = store.reject_review("g1", "give up", 3).await.unwrap();
        assert_eq!(status, "needs_human");
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "needs_human");
        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters[0].verdict.as_deref(), Some("escalated"));
    }

    /// WP-4F ③: the round's own `result_summary` is snapshotted into
    /// `task_iterations.worker_excerpt` via `duduclaw_core::truncate_bytes`
    /// (never a raw byte slice, which panics on a multi-byte CJK boundary),
    /// AND the task-level `judge_feedback` is enriched with the composed
    /// best-round note when the retry budget is exhausted on a CJK-heavy
    /// result. Exercises the real `reject_review` → `iter_verdict_conn` →
    /// `goal_budget_best_round` path end-to-end (not just the pure-function
    /// unit tests in `goal_budget_best_round.rs`).
    #[tokio::test]
    async fn reject_review_escalate_truncates_cjk_excerpt_safely() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.max_retries = 1;
        t.retry_count = 1; // budget already spent ⇒ this rejection escalates
        // Deliberately over the WORKER_EXCERPT_MAX_BYTES (500) budget with a
        // 3-byte-per-char CJK string so a naive `&s[..500]` byte slice would
        // panic (500 does not land on a char boundary for an all-3-byte
        // string — see the constant's own test in goal_budget_best_round.rs).
        t.result_summary = Some("驗".repeat(400)); // 1200 bytes, all 3-byte chars
        store.insert_task(&t).await.unwrap();
        store.record_iteration_dispatch("g1", 1, "2026-08-15T10:00:00Z").await.unwrap();

        // Must not panic.
        let status = store.reject_review("g1", "見 goal_loop.rs:120 缺邊界檢查", 3).await.unwrap();
        assert_eq!(status, "needs_human");

        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters.len(), 1);
        let excerpt = iters[0]
            .worker_excerpt
            .as_deref()
            .expect("a non-empty result_summary must be snapshotted");
        assert!(
            excerpt.len() <= crate::goal_budget_best_round::WORKER_EXCERPT_MAX_BYTES,
            "excerpt must respect the byte budget: {} bytes",
            excerpt.len()
        );
        assert!(!excerpt.is_empty());
        assert!(excerpt.chars().all(|c| c == '驗'), "truncation must land on a char boundary");

        // The task-level judge_feedback was enriched with the best-round
        // note (this round is the only candidate, so priority 1 picks it via
        // its own verdict_json... actually this round has no verdict_json,
        // so priority 2/3 applies — either way `pick_best_round` finds this
        // sole round and the note is composed).
        let task = store.get_task("g1").await.unwrap().unwrap();
        let fb = task.judge_feedback.expect("judge_feedback must be set");
        assert!(fb.contains("已附上第 1 輪最接近完成的成果"));
        assert!(fb.contains(excerpt));
        assert!(fb.contains("goal_loop.rs:120"), "extracted gap token must be listed");
    }

    // ── H11: pause-reason classification ────────────────────────────────

    /// `mark_needs_human_with_pause` stamps the class alongside the free-text
    /// reason, and the string-only wrapper degrades to the SAFE class rather
    /// than guessing one from the text.
    #[tokio::test]
    async fn mark_needs_human_stamps_the_pause_class() {
        use crate::pause_reason::PauseReason;
        let (store, _dir) = temp_store();
        store.insert_task(&goal_review_task("g1")).await.unwrap();
        store.insert_task(&goal_review_task("g2")).await.unwrap();

        store
            .mark_needs_human_with_pause("g1", "judge unavailable: connect timeout", PauseReason::Infra)
            .await
            .unwrap();
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human");
        assert_eq!(got.pause_reason.as_deref(), Some("infra"));
        assert_eq!(
            PauseReason::from_stored(got.pause_reason.as_deref()),
            PauseReason::Infra
        );

        // The un-classified wrapper must NOT sniff the reason text — it writes
        // `unknown`, which reads as 「需要人工確認」.
        store
            .mark_needs_human("g2", "goal-loop iteration cap")
            .await
            .unwrap();
        let got2 = store.get_task("g2").await.unwrap().unwrap();
        assert_eq!(
            PauseReason::from_stored(got2.pause_reason.as_deref()),
            PauseReason::Unknown
        );
    }

    /// A judge rejection at the spent retry budget is a hard cap firing, so
    /// the escalate branch classifies it `budget_exhausted` — while the
    /// `revising` branch (budget left) writes no class at all.
    #[tokio::test]
    async fn reject_review_classifies_only_the_escalating_branch() {
        use crate::pause_reason::PauseReason;
        let (store, _dir) = temp_store();

        let mut spent = goal_review_task("g1");
        spent.max_retries = 1;
        spent.retry_count = 1;
        store.insert_task(&spent).await.unwrap();
        assert_eq!(store.reject_review("g1", "give up", 3).await.unwrap(), "needs_human");
        assert_eq!(
            PauseReason::from_stored(
                store.get_task("g1").await.unwrap().unwrap().pause_reason.as_deref()
            ),
            PauseReason::BudgetExhausted
        );

        // Budget remaining ⇒ back around the loop, no pause at all.
        store.insert_task(&goal_review_task("g2")).await.unwrap();
        assert_eq!(store.reject_review("g2", "try again", 3).await.unwrap(), "revising");
        assert!(store.get_task("g2").await.unwrap().unwrap().pause_reason.is_none());
    }

    /// Every `resolve_needs_human` branch ends the pause, so the class is
    /// cleared — a retried task must never re-render the chip of the pause a
    /// human just resolved.
    #[tokio::test]
    async fn resolving_a_pause_clears_the_class() {
        use crate::pause_reason::PauseReason;
        for (decision, expect_status) in
            [("retry", "pending"), ("done", "done"), ("abort", "cancelled")]
        {
            let (store, _dir) = temp_store();
            store.insert_task(&goal_review_task("g1")).await.unwrap();
            store
                .mark_needs_human_with_pause("g1", "stuck", PauseReason::NoProgress)
                .await
                .unwrap();
            assert_eq!(
                store.get_task("g1").await.unwrap().unwrap().pause_reason.as_deref(),
                Some("no_progress")
            );

            assert!(store.resolve_needs_human("g1", decision, "").await.unwrap());
            let got = store.get_task("g1").await.unwrap().unwrap();
            assert_eq!(got.status, expect_status);
            assert!(
                got.pause_reason.is_none(),
                "{decision}: the pause is over — the class must be cleared"
            );
        }
    }

    /// Legacy rows (written before the column existed) and rows that were
    /// never escalated both read back as `Unknown` — the safe direction.
    #[tokio::test]
    async fn pause_reason_round_trips_and_legacy_rows_are_unknown() {
        use crate::pause_reason::PauseReason;
        let (store, _dir) = temp_store();

        let mut t = pending_task("p1");
        t.pause_reason = Some("restart".into());
        store.insert_task(&t).await.unwrap();
        assert_eq!(
            store.get_task("p1").await.unwrap().unwrap().pause_reason.as_deref(),
            Some("restart")
        );

        let plain = pending_task("p2");
        store.insert_task(&plain).await.unwrap();
        let got = store.get_task("p2").await.unwrap().unwrap();
        assert!(got.pause_reason.is_none());
        assert_eq!(
            PauseReason::from_stored(got.pause_reason.as_deref()),
            PauseReason::Unknown
        );
    }

    /// The `pause_reason` ALTER is idempotent across reopens (the shared
    /// migration-loop contract), and rows written before it existed survive.
    #[tokio::test]
    async fn pause_reason_migration_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = TaskStore::open(dir.path()).unwrap();
            store.insert_task(&pending_task("old")).await.unwrap();
        }
        for _ in 0..2 {
            let store = TaskStore::open(dir.path()).unwrap();
            let got = store.get_task("old").await.unwrap().unwrap();
            assert!(got.pause_reason.is_none());
        }
    }

    // ── I-1c "想一想" plan-first: `plan_pending` round-trip + survival ──

    /// `plan_pending` round-trips through insert/read like every other
    /// column, and is `None` when never set (the overwhelming majority of
    /// tasks never go through plan-first).
    #[tokio::test]
    async fn plan_pending_round_trips_and_defaults_to_none() {
        let (store, _dir) = temp_store();
        let mut t = pending_task("p1");
        t.plan_pending = Some("- 步驟一\n- 步驟二".into());
        store.insert_task(&t).await.unwrap();
        assert_eq!(
            store.get_task("p1").await.unwrap().unwrap().plan_pending.as_deref(),
            Some("- 步驟一\n- 步驟二")
        );

        store.insert_task(&pending_task("p2")).await.unwrap();
        assert!(store.get_task("p2").await.unwrap().unwrap().plan_pending.is_none());
    }

    /// The whole point of the separate column: `resolve_needs_human`'s
    /// `retry` arm overwrites `judge_feedback` (with the human's own,
    /// possibly-empty, approval note) but must NEVER touch `plan_pending` —
    /// otherwise the approved plan would vanish before the next dispatch
    /// ever reads it.
    #[tokio::test]
    async fn plan_pending_survives_a_needs_human_retry_that_clears_judge_feedback() {
        let (store, _dir) = temp_store();
        let mut t = goal_review_task("g1");
        t.status = "needs_human".into();
        t.judge_feedback = Some("- 步驟一\n- 步驟二".into());
        t.plan_pending = Some("- 步驟一\n- 步驟二".into());
        store.insert_task(&t).await.unwrap();

        // Human approves with no extra note (the common "同意執行" click).
        assert!(store.resolve_needs_human("g1", "retry", "").await.unwrap());
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.status, "pending");
        assert!(
            got.judge_feedback.is_none(),
            "an empty approval note clears judge_feedback as usual"
        );
        assert_eq!(
            got.plan_pending.as_deref(),
            Some("- 步驟一\n- 步驟二"),
            "plan_pending must survive the retry write untouched"
        );
    }

    /// `clear_plan_pending` is the driver's one-time-injection consumer —
    /// idempotent, unconditional on status.
    #[tokio::test]
    async fn clear_plan_pending_nulls_the_column() {
        let (store, _dir) = temp_store();
        let mut t = pending_task("p1");
        t.plan_pending = Some("plan text".into());
        store.insert_task(&t).await.unwrap();

        store.clear_plan_pending("p1").await.unwrap();
        assert!(store.get_task("p1").await.unwrap().unwrap().plan_pending.is_none());
        // Idempotent — clearing an already-clear column is a no-op, not an error.
        store.clear_plan_pending("p1").await.unwrap();
    }

    // ── H22: latest activity timestamp (the progress signal) ────────────

    #[tokio::test]
    async fn latest_activity_at_returns_the_newest_row_or_none() {
        let (store, _dir) = temp_store();
        assert!(store.latest_activity_at("t1").await.unwrap().is_none());

        for ts in ["2026-08-15T10:00:00Z", "2026-08-15T10:30:00Z", "2026-08-15T10:05:00Z"] {
            store
                .append_activity(&ActivityRow {
                    id: uuid::Uuid::new_v4().to_string(),
                    event_type: "goal_loop.dispatched".into(),
                    agent_id: "alice".into(),
                    task_id: Some("t1".into()),
                    summary: "x".into(),
                    timestamp: ts.into(),
                    metadata: None,
                })
                .await
                .unwrap();
        }
        assert_eq!(
            store.latest_activity_at("t1").await.unwrap().as_deref(),
            Some("2026-08-15T10:30:00Z"),
            "newest wins regardless of insertion order"
        );
        // Scoped per task — another task's events are not this task's signal.
        assert!(store.latest_activity_at("t2").await.unwrap().is_none());
    }

    // ── W1-5: claim_needs_human (take over) ─────────────────────────────

    #[tokio::test]
    async fn claim_needs_human_stamps_claimed_by_without_changing_status() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "needs_human".into();
        store.insert_task(&t).await.unwrap();

        let changed = store.claim_needs_human("g1", "channel:telegram:555").await.unwrap();
        assert!(changed);
        let got = store.get_task("g1").await.unwrap().unwrap();
        // Status stays `needs_human` — GoalLoopDriver's candidate query never
        // reads this status, so the loop is already stopped without a
        // dedicated status transition.
        assert_eq!(got.status, "needs_human");
        assert_eq!(got.claimed_by.as_deref(), Some("channel:telegram:555"));
    }

    #[tokio::test]
    async fn claim_needs_human_is_idempotent_and_repeatable() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "needs_human".into();
        store.insert_task(&t).await.unwrap();

        assert!(store.claim_needs_human("g1", "channel:telegram:1").await.unwrap());
        // A second (even different) decider re-stamps rather than failing —
        // unlike `resolve_needs_human` there is no terminal state to guard.
        assert!(store.claim_needs_human("g1", "channel:slack:2").await.unwrap());
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.claimed_by.as_deref(), Some("channel:slack:2"));
        assert_eq!(got.status, "needs_human");
    }

    #[tokio::test]
    async fn claim_needs_human_fails_closed_off_needs_human() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "pending".into();
        store.insert_task(&t).await.unwrap();

        let changed = store.claim_needs_human("g1", "channel:telegram:1").await.unwrap();
        assert!(!changed, "a task not in needs_human must not be claimable");
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert!(got.claimed_by.is_none());
    }

    #[tokio::test]
    async fn claim_needs_human_on_missing_task_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let changed = store.claim_needs_human("ghost", "channel:telegram:1").await.unwrap();
        assert!(!changed);
    }

    // ── I-3a: continue_from_terminal ("接著做") ─────────────────────────

    #[tokio::test]
    async fn continue_from_terminal_reopens_done_failed_and_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        for (id, status) in [("g-done", "done"), ("g-failed", "failed"), ("g-cancelled", "cancelled")] {
            let mut t = goal_review_task(id);
            t.status = status.into();
            t.completed_at = Some("2026-08-01T00:00:00Z".into());
            t.claimed_by = Some("worker".into());
            store.insert_task(&t).await.unwrap();

            let changed = store.continue_from_terminal(id, "再補一份摘要").await.unwrap();
            assert!(changed, "{status} must be reopenable");
            let got = store.get_task(id).await.unwrap().unwrap();
            assert_eq!(got.status, "pending");
            assert!(got.claimed_by.is_none(), "a stale claim must be cleared");
            assert!(got.completed_at.is_none(), "a stale completion timestamp must be cleared");
            assert!(got.judge_feedback.as_deref().unwrap().contains("再補一份摘要"));
        }
    }

    #[tokio::test]
    async fn continue_from_terminal_preserves_revision_round_for_iteration_continuity() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "failed".into();
        t.revision_round = 4;
        store.insert_task(&t).await.unwrap();

        store.continue_from_terminal("g1", "再試一次").await.unwrap();
        let got = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(got.revision_round, 4, "the round counter must continue, not reset");
    }

    #[tokio::test]
    async fn continue_from_terminal_rejects_a_blank_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "done".into();
        store.insert_task(&t).await.unwrap();

        let err = store.continue_from_terminal("g1", "   ").await.unwrap_err();
        assert!(err.contains("訊息"), "got: {err}");
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "done");
    }

    #[tokio::test]
    async fn continue_from_terminal_fails_closed_on_non_terminal_status() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "needs_human".into();
        store.insert_task(&t).await.unwrap();

        let changed = store.continue_from_terminal("g1", "再多做一點").await.unwrap();
        assert!(!changed, "needs_human already has its own retry/done/abort path");
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "needs_human");
    }

    #[tokio::test]
    async fn continue_from_terminal_fails_closed_on_non_goal_mode_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "done".into();
        t.goal_mode = false;
        store.insert_task(&t).await.unwrap();

        let changed = store.continue_from_terminal("g1", "再多做一點").await.unwrap();
        assert!(!changed, "an ordinary board task must not be reopenable via this path");
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "done");
    }

    #[tokio::test]
    async fn full_round_records_dispatch_submit_verdict_and_agent_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = goal_review_task("g1");
        t.status = "pending".into();
        t.result_summary = None;
        store.insert_task(&t).await.unwrap();

        // Round 1: dispatch (driver, in the past), claim, complete → review.
        // A past dispatch stamp makes the agent-seconds delta clearly positive
        // against the real `Utc::now()` complete_task uses.
        store
            .record_iteration_dispatch("g1", 1, "2020-01-01T00:00:00Z")
            .await
            .unwrap();
        store
            .atomic_claim("g1", "alice", "2026-07-25T10:00:05Z", "2026-07-25T10:05:00Z")
            .await
            .unwrap();
        store.complete_task("g1", "done round 1", "alice").await.unwrap();

        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters.len(), 1);
        assert_eq!(iters[0].round, 1);
        assert!(iters[0].submitted_at.is_some(), "submit stamped on complete");
        // agent_seconds accumulated on the task (submit − dispatch).
        let after = store.get_task("g1").await.unwrap().unwrap();
        assert!(after.agent_seconds > 0, "agent clock accumulated");
        assert_eq!(after.status, "review");

        // Reject → verdict sealed on round 1, task revising.
        store.reject_review("g1", "fix it", 3).await.unwrap();
        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters[0].verdict.as_deref(), Some("rejected"));
    }

    #[tokio::test]
    async fn dispatch_iteration_is_idempotent_per_round() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        store.insert_task(&pending_task("g1")).await.unwrap();
        // A stall re-dispatch of the same round must not open a duplicate row.
        store.record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z").await.unwrap();
        store.record_iteration_dispatch("g1", 1, "2026-07-25T10:01:00Z").await.unwrap();
        assert_eq!(store.list_iterations("g1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn accept_review_seals_accepted_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        store.insert_task(&goal_review_task("g1")).await.unwrap();
        store.record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z").await.unwrap();

        assert!(store.accept_review("g1", "looks good").await.unwrap());
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "done");
        let iters = store.list_iterations("g1").await.unwrap();
        assert_eq!(iters[0].verdict.as_deref(), Some("accepted"));
    }

    #[tokio::test]
    async fn iteration_migration_idempotent_and_old_rows_default_zero() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate a pre-Iterative-Kanban db: insert a task, close, reopen twice.
        {
            let s = TaskStore::open(dir.path()).unwrap();
            s.insert_task(&pending_task("old1")).await.unwrap();
        }
        let s2 = TaskStore::open(dir.path()).unwrap();
        // Reopening runs the ALTERs again — must not error, and the old row's new
        // columns default to zero.
        let t = s2.get_task("old1").await.unwrap().unwrap();
        assert_eq!(t.revision_round, 0);
        assert!(!t.diminishing);
        assert_eq!(t.agent_seconds, 0);
        // task_iterations table exists and is queryable (empty for the old task).
        assert!(s2.list_iterations("old1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn flow_metrics_computes_first_pass_yield_and_review_depth() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();

        // done, first pass (revision_round 0).
        let mut d1 = goal_review_task("d1");
        d1.status = "done".into();
        d1.completed_at = Some(chrono::Utc::now().to_rfc3339());
        d1.revision_round = 0;
        d1.agent_seconds = 10;
        store.insert_task(&d1).await.unwrap();
        // done, second round (revision_round 1 → not first pass).
        let mut d2 = goal_review_task("d2");
        d2.status = "done".into();
        d2.completed_at = Some(chrono::Utc::now().to_rfc3339());
        d2.revision_round = 1;
        d2.agent_seconds = 30;
        store.insert_task(&d2).await.unwrap();
        // one still in review (queue depth).
        store.insert_task(&goal_review_task("r1")).await.unwrap();

        let m = store.flow_metrics(&chrono::Utc::now().to_rfc3339()).await.unwrap();
        assert_eq!(m.review_queue_depth, 1);
        assert_eq!(m.accepts_last_7d, 2);
        let alice = m.agents.iter().find(|a| a.agent_id == "alice").unwrap();
        assert_eq!(alice.finished, 2);
        assert!((alice.first_pass_yield - 0.5).abs() < 1e-9, "1 of 2 first-pass");
        assert!((alice.avg_rounds - 1.5).abs() < 1e-9, "rounds 1 and 2 → avg 1.5");
        assert_eq!(alice.review_queue_depth, 1);
    }
}
