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
     goal_mode, acceptance_criteria, result_summary, judge_feedback, goal_id, lease_renewed_at";

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
        }
    }
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
        Ok(())
    }

    // ── Task CRUD ───────────────────────────────────────────

    pub async fn list_tasks(
        &self,
        status: Option<&str>,
        agent_id: Option<&str>,
        priority: Option<&str>,
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
        sql.push_str(" ORDER BY updated_at DESC");

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
                 goal_id, lease_renewed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
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
            if let Some(v) = fields.get("tags").and_then(|v| v.as_str()) {
                binds.push(v.to_string());
                sets.push(format!("tags = ?{}", binds.len()));
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
        if status != "pending" || claimed_by.is_some() {
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
                  WHERE id = ?1 AND status = 'pending' AND claimed_by IS NULL",
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

    /// Pending tasks that are claimable *right now*: unclaimed and with every
    /// `depends_on` id already `done`. Dependency filtering is done in Rust
    /// (parsing the JSON array) against the current `done` set.
    pub async fn claimable_tasks(&self) -> Result<Vec<TaskRow>, String> {
        let done = self.done_task_ids().await?;
        let pending = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {TASK_COLUMNS} FROM tasks
                      WHERE status = 'pending' AND claimed_by IS NULL
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
            let row: Option<(bool, Option<String>)> = tx
                .query_row(
                    "SELECT goal_mode, claimed_by FROM tasks WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?)),
                )
                .optional()
                .map_err(|e| format!("complete: load: {e}"))?;
            let Some((goal_mode, claimed_by)) = row else {
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
                tx.execute(
                    "UPDATE tasks
                        SET status = 'review', result_summary = ?2,
                            lease_expires_at = NULL, updated_at = ?3
                      WHERE id = ?1 AND status NOT IN ('done', 'cancelled')",
                    params![id, summary, now],
                )
                .map_err(|e| format!("complete (review): {e}"))?;
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
        Ok(n == 1)
    }

    /// Goal-mode acceptance rejected: send the task back to `pending` for
    /// another attempt (retry budget permitting), attaching judge feedback.
    /// When retries are exhausted, escalate to `needs_human` (fail-safe — never
    /// loops indefinitely). Returns the terminal status applied.
    pub async fn reject_review(&self, id: &str, feedback: &str) -> Result<String, String> {
        let row = match self.get_task(id).await? {
            Some(r) => r,
            None => return Err(format!("task not found: {id}")),
        };
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        if row.retry_count < row.max_retries {
            let new_retry = row.retry_count + 1;
            conn.execute(
                "UPDATE tasks
                    SET status = 'pending', claimed_by = NULL, claimed_at = NULL,
                        lease_expires_at = NULL, retry_count = ?2, judge_feedback = ?3,
                        result_summary = NULL, updated_at = ?4
                  WHERE id = ?1 AND status = 'review'",
                params![id, new_retry, feedback, now],
            )
            .map_err(|e| format!("reject review (requeue): {e}"))?;
            Ok("pending".to_string())
        } else {
            conn.execute(
                "UPDATE tasks
                    SET status = 'needs_human', judge_feedback = ?2, updated_at = ?3
                  WHERE id = ?1 AND status = 'review'",
                params![id, feedback, now],
            )
            .map_err(|e| format!("reject review (escalate): {e}"))?;
            Ok("needs_human".to_string())
        }
    }

    /// Fail-safe escalation: park a task for human attention without killing or
    /// looping it. Used when the judge itself errors (goal mode).
    pub async fn mark_needs_human(&self, id: &str, reason: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE tasks SET status = 'needs_human', judge_feedback = ?2, updated_at = ?3
                  WHERE id = ?1",
                params![id, reason, now],
            )
            .map_err(|e| format!("mark needs_human: {e}"))?;
        Ok(n > 0)
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
    })
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
        parse_depends_on, zombie_action, zombie_reclaim_due, CommentRow, GoalRow, TaskRow,
        TaskStore, ZombieAction,
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

        // Reject → requeues to pending (retry 0 < 1) with feedback.
        let status = store.reject_review("goal", "criteria not met").await.unwrap();
        assert_eq!(status, "pending");
        let t = store.get_task("goal").await.unwrap().unwrap();
        assert_eq!(t.retry_count, 1);
        assert_eq!(t.judge_feedback.as_deref(), Some("criteria not met"));

        // Complete again → review → reject at cap ⇒ needs_human (fail-safe).
        store.complete_task("goal", "attempt 2", "w").await.unwrap();
        let status2 = store.reject_review("goal", "still failing").await.unwrap();
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
}
