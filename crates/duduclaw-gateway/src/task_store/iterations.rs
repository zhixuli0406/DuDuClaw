//! Goal-loop round bookkeeping and the derived flow metrics.
//! Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
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
pub(super) fn list_iterations_conn(conn: &Connection, task_id: &str) -> Result<Vec<TaskIterationRow>, String> {
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
pub(super) fn iter_submit_conn(
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
pub(super) fn iter_verdict_conn(
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
