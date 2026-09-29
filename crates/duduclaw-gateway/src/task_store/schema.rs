//! Opening the store and the self-healing schema / column migrations.
//! Moved verbatim out of `task_store.rs` (file-size split).

use super::*;

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
            (
                "dispatch_count",
                "dispatch_count INTEGER NOT NULL DEFAULT 1",
            ),
            ("state_hash", "state_hash TEXT"),
            ("repeat_streak", "repeat_streak INTEGER"),
            // WP-4F: a bounded, CJK-safe-truncated snapshot of the round's
            // own worker output (`tasks.result_summary` at verdict time),
            // taken because `reject_review_with_verdict` wipes
            // `result_summary` back to NULL on every "revising" rejection —
            // without this snapshot no round's actual output survives past
            // its own rejection, so a later budget-exhausted escalation has
            // nothing to attach (see `goal_loop/state.rs`).
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
            (
                "revision_round",
                "revision_round INTEGER NOT NULL DEFAULT 0",
            ),
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
            (
                "acceptance_criteria_baseline",
                "acceptance_criteria_baseline TEXT",
            ),
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
            // Team-as-Agent spec freeze (P1/WP-4, 2026-09): the role→
            // {runtime, model, effort} team frozen at goal-creation time.
            // Nullable on purpose — every pre-existing row, and every task
            // created without `[team] enabled`, reads back as "no team" =
            // Solo. Same idempotent ALTER TABLE ADD COLUMN pattern as every
            // migration above.
            ("team_spec_json", "team_spec_json TEXT"),
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
}
