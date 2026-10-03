//! Goal-loop round bookkeeping and the derived flow metrics.
//! Moved verbatim out of `task_store.rs`.

use super::*;

impl TaskStore {
    /// Open a work round for a goal-mode task (called by the goal loop driver on
    /// dispatch). A stall re-dispatch reuses the current open attempt; a
    /// human retry after a sealed attempt opens a new row of the same round.
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
        self.record_iteration_dispatch_with_ledger(
            task_id,
            round,
            now,
            state_hash,
            repeat_streak,
            &IterationDispatchLedger::default(),
        )
        .await
    }

    /// A1 ledger completeness: [`Self::record_iteration_dispatch_with_state`]
    /// plus the dispatch-time facts that were previously memory-only
    /// (`iter_seq`, Solo/Team, gate inputs, the `<state>` block). Each field
    /// is `COALESCE`d on a stall re-dispatch, so a `None` never erases a value
    /// an earlier dispatch of the same round recorded.
    pub async fn record_iteration_dispatch_with_ledger(
        &self,
        task_id: &str,
        round: i64,
        now: &str,
        state_hash: Option<&str>,
        repeat_streak: Option<i64>,
        ledger: &IterationDispatchLedger,
    ) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("iteration dispatch: begin: {e}"))?;
        let previous_dispatches: i64 = tx.query_row(
            "SELECT COALESCE(SUM(dispatch_count),0) FROM task_iterations WHERE task_id=?1",
            params![task_id], |r| r.get(0),
        ).map_err(|e| format!("iteration dispatch: count prior dispatches: {e}"))?;
        iter_dispatch_conn(&tx, task_id, round, now, state_hash, repeat_streak, ledger)?;
        record_survival_difficulty_conn(&tx, task_id, previous_dispatches, ledger)?;
        tx.commit().map_err(|e| format!("iteration dispatch: commit: {e}"))
    }

    /// A1: stamp the two-stage pre-evaluator's verdict on the round that is
    /// about to be sealed (the latest un-judged round — the exact lookup
    /// `iter_verdict_conn` uses, so both writes land on the same row). No
    /// open round ⇒ no-op. Pure bookkeeping: callers log and ignore errors.
    pub async fn record_iteration_evaluator_verdict(
        &self,
        task_id: &str,
        evaluator_verdict: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        let row_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM task_iterations
                  WHERE task_id = ?1 AND judged_at IS NULL AND verdict IS NULL
                  ORDER BY round DESC, id DESC LIMIT 1",
                params![task_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("iter evaluator lookup: {e}"))?;
        if let Some(id) = row_id {
            conn.execute(
                "UPDATE task_iterations SET evaluator_verdict = ?2 WHERE id = ?1",
                params![id, evaluator_verdict],
            )
            .map_err(|e| format!("iter evaluator update: {e}"))?;
        }
        Ok(())
    }

    /// WP-G2: write the task's criteria ledger (latest state) and, in the
    /// same transaction, snapshot it onto the round about to be sealed (the
    /// latest un-judged iteration row — the lookup
    /// [`Self::record_iteration_evaluator_verdict`] uses). No open round ⇒
    /// only the task column is written.
    pub async fn set_criteria_ledger(&self, task_id: &str, ledger_json: &str) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("criteria ledger: begin: {e}"))?;
        tx.execute(
            "UPDATE tasks SET criteria_ledger = ?2 WHERE id = ?1",
            params![task_id, ledger_json],
        )
        .map_err(|e| format!("criteria ledger: task write: {e}"))?;
        let row_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM task_iterations
                  WHERE task_id = ?1 AND judged_at IS NULL AND verdict IS NULL
                  ORDER BY round DESC, id DESC LIMIT 1",
                params![task_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("criteria ledger: round lookup: {e}"))?;
        if let Some(id) = row_id {
            tx.execute(
                "UPDATE task_iterations SET criteria_ledger_json = ?2 WHERE id = ?1",
                params![id, ledger_json],
            )
            .map_err(|e| format!("criteria ledger: round snapshot: {e}"))?;
        }
        tx.commit().map_err(|e| format!("criteria ledger: commit: {e}"))
    }

    /// WP-G2: replace the stored worker reply with its tag-stripped form
    /// once the `<criteria_status>` report has been read, so every stored
    /// copy downstream (`result_summary` on the task row, the
    /// `worker_excerpt` each verdict seals from it, the dashboard's latest
    /// output) carries no raw tag. The parsed ledger keeps the content.
    /// Compare-and-set on the exact text read at settle: a newer submission
    /// that landed in between is never overwritten. Returns whether a row
    /// changed.
    pub async fn rewrite_result_summary(
        &self,
        task_id: &str,
        expected: &str,
        replacement: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE tasks SET result_summary = ?3
                  WHERE id = ?1 AND result_summary = ?2",
                params![task_id, expected, replacement],
            )
            .map_err(|e| format!("rewrite result_summary: {e}"))?;
        Ok(n > 0)
    }

    /// WP-G2: per-round ledger snapshots `(round, json)`, oldest first.
    pub async fn iteration_criteria_snapshots(
        &self,
        task_id: &str,
    ) -> Result<Vec<(i64, Option<String>)>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT round, criteria_ledger_json FROM task_iterations
                  WHERE task_id = ?1 ORDER BY round ASC, id ASC",
            )
            .map_err(|e| format!("prepare criteria snapshots: {e}"))?;
        stmt.query_map(params![task_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| format!("query criteria snapshots: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect criteria snapshots: {e}"))
    }

    /// A1: record the pause class on the task's latest iteration row when an
    /// escalation happens outside the settle path (the driver's own caps /
    /// oscillation, a team round asking for a human). Never touches the
    /// verdict; never overwrites a pause class already on the row. No row ⇒
    /// no-op. Pure bookkeeping: callers log and ignore errors.
    pub async fn stamp_iteration_pause(&self, task_id: &str, pause: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE task_iterations SET pause_reason = ?2
              WHERE id = (SELECT id FROM task_iterations WHERE task_id = ?1
                           ORDER BY round DESC, id DESC LIMIT 1)
                AND pause_reason IS NULL",
            params![task_id, pause],
        )
        .map_err(|e| format!("iter pause stamp: {e}"))?;
        Ok(())
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

/// Count dispatch occurrences, not judge rounds. Any missing snapshot makes
/// the historical classification unknown permanently instead of filling a gap
/// from today's editable task text.
fn record_survival_difficulty_conn(conn: &Connection, task_id: &str, previous_dispatches: i64, ledger: &IterationDispatchLedger) -> Result<(), String> {
    let difficulty = ledger.gate_inputs_json.as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("goal_difficulty").and_then(|v| v.as_str()).map(str::to_owned))
        .filter(|value| matches!(value.as_str(), "simple" | "complex"));
    conn.execute(
        "UPDATE task_survival_evidence
         SET difficulty=CASE WHEN difficulty_dispatches=?2 AND ?3 IS NOT NULL THEN
                 CASE WHEN difficulty_dispatches=0 THEN ?3
                      WHEN difficulty=?3 THEN difficulty ELSE 'mixed' END
                 ELSE NULL END,
             difficulty_dispatches=CASE WHEN difficulty_dispatches=?2 AND ?3 IS NOT NULL
                 THEN difficulty_dispatches+1 ELSE difficulty_dispatches END
         WHERE task_id=?1 AND evidence_version=1",
        params![task_id,previous_dispatches,difficulty],
    ).map_err(|e| format!("iteration dispatch: survival difficulty receipt: {e}"))?;
    Ok(())
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
        evaluator_verdict: row.get(14)?,
        iter_seq: row.get(15)?,
        team_mode: row.get(16)?,
        gate_inputs_json: row.get(17)?,
        state_block_json: row.get(18)?,
        knobs_json: row.get(19)?,
        pause_reason: row.get(20)?,
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
                    dispatch_count, state_hash, repeat_streak, worker_excerpt,
                    evaluator_verdict, iter_seq, team_mode, gate_inputs_json,
                    state_block_json, knobs_json, pause_reason
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

/// Open an attempt for `round`. A stall re-dispatch of the latest open attempt
/// keeps its original `dispatched_at`, increments `dispatch_count`, and refreshes
/// the visit-graph signal. A sealed attempt is immutable: a human retry creates
/// a new row even when the logical round number has not advanced.
fn iter_dispatch_conn(
    conn: &Connection,
    task_id: &str,
    round: i64,
    now: &str,
    state_hash: Option<&str>,
    repeat_streak: Option<i64>,
    ledger: &IterationDispatchLedger,
) -> Result<(), String> {
    let latest: Option<(i64, bool)> = conn
        .query_row(
            "SELECT id, judged_at IS NOT NULL OR verdict IS NOT NULL
               FROM task_iterations WHERE task_id = ?1 AND round = ?2
               ORDER BY id DESC LIMIT 1",
            params![task_id, round],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("iter dispatch lookup: {e}"))?;
    if let Some((id, false)) = latest {
        conn.execute(
            "UPDATE task_iterations
                SET dispatch_count = dispatch_count + 1,
                    state_hash = COALESCE(?2, state_hash),
                    repeat_streak = COALESCE(?3, repeat_streak),
                    iter_seq = COALESCE(?4, iter_seq),
                    team_mode = COALESCE(?5, team_mode),
                    gate_inputs_json = COALESCE(?6, gate_inputs_json),
                    state_block_json = COALESCE(?7, state_block_json)
              WHERE id = ?1",
            params![
                id,
                state_hash,
                repeat_streak,
                ledger.iter_seq,
                ledger.team_mode.as_deref(),
                ledger.gate_inputs_json.as_deref(),
                ledger.state_block_json.as_deref()
            ],
        )
        .map_err(|e| format!("iter dispatch bump: {e}"))?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO task_iterations (task_id, round, dispatched_at, state_hash, repeat_streak,
                                      iter_seq, team_mode, gate_inputs_json, state_block_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            task_id,
            round,
            now,
            state_hash,
            repeat_streak,
            ledger.iter_seq,
            ledger.team_mode.as_deref(),
            ledger.gate_inputs_json.as_deref(),
            ledger.state_block_json.as_deref()
        ],
    )
    .map_err(|e| format!("iter dispatch insert: {e}"))?;
    Ok(())
}

/// Stamp the worker submission on the latest unsealed, unsubmitted attempt
/// (round DESC, id DESC) and return that attempt's agent seconds. When no open round
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
                AND judged_at IS NULL AND verdict IS NULL
              ORDER BY round DESC, id DESC LIMIT 1",
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
/// round's own worker output (see [`crate::goal_budget_best_round::worker_excerpt`]);
/// `None` for callers with no result text to snapshot.
/// `knobs_json` / `pause_reason` (A1 ledger): the harness knob snapshot at
/// sealing time and, for an escalating verdict, the pause class. A `None`
/// pause never erases one already on the row.
#[allow(clippy::too_many_arguments)]
pub(super) fn iter_verdict_conn(
    conn: &Connection,
    task_id: &str,
    verdict: &str,
    feedback: &str,
    verdict_json: Option<&str>,
    worker_excerpt: Option<&str>,
    knobs_json: Option<&str>,
    pause_reason: Option<&str>,
    now: &str,
) -> Result<(), String> {
    let row_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM task_iterations
              WHERE task_id = ?1 AND judged_at IS NULL AND verdict IS NULL
              ORDER BY round DESC, id DESC LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("iter verdict lookup: {e}"))?;
    if let Some(id) = row_id {
        conn.execute(
            "UPDATE task_iterations
                SET judged_at = ?2, verdict = ?3, judge_feedback = ?4,
                    verdict_json = ?5, worker_excerpt = ?6,
                    knobs_json = ?7, pause_reason = COALESCE(?8, pause_reason)
              WHERE id = ?1",
            params![
                id,
                now,
                verdict,
                feedback,
                verdict_json,
                worker_excerpt,
                knobs_json,
                pause_reason
            ],
        )
        .map_err(|e| format!("iter verdict update: {e}"))?;
    }
    Ok(())
}

/// A1-2: seal the latest un-judged round as `escalated` when the settle path
/// parks the task through `mark_needs_human_with_pause` (evaluator `blocked`,
/// judge error, `human_only` / `evaluator_only` fail-closed) — previously such
/// a round kept `verdict = NULL` forever.
///
/// `judge_feedback` is deliberately left **NULL**: no judge ruled, and the
/// goal loop derives its `<state>` block's `excluded_approaches` (and so the
/// A2 `state_hash`) from every row's `judge_feedback`. Writing the pause
/// reason there would change the next round's prompt and fingerprint. The
/// reason lives on the task row as before; the class goes to `pause_reason`.
/// [`crate::goal_budget_best_round::pick_best_round`] skips rows of this
/// shape (`escalated` + NULL feedback) for the same no-behavior-change reason.
pub(super) fn iter_escalate_seal_conn(
    conn: &Connection,
    task_id: &str,
    pause_reason: &str,
    worker_excerpt: Option<&str>,
    knobs_json: Option<&str>,
    now: &str,
) -> Result<(), String> {
    let row_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM task_iterations
              WHERE task_id = ?1 AND judged_at IS NULL AND verdict IS NULL
              ORDER BY round DESC, id DESC LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("iter escalate lookup: {e}"))?;
    if let Some(id) = row_id {
        conn.execute(
            "UPDATE task_iterations
                SET judged_at = ?2, verdict = 'escalated', worker_excerpt = ?3,
                    knobs_json = ?4, pause_reason = ?5
              WHERE id = ?1",
            params![id, now, worker_excerpt, knobs_json, pause_reason],
        )
        .map_err(|e| format!("iter escalate update: {e}"))?;
    }
    Ok(())
}

/// A1 ledger completeness: dispatch-time facts recorded alongside the round
/// row. Every field optional — `Default` reproduces the pre-A1 write exactly.
#[derive(Debug, Clone, Default)]
pub struct IterationDispatchLedger {
    /// The driver's dispatch ordinal (`InFlight.iter` of this dispatch).
    pub iter_seq: Option<i64>,
    /// `solo` | `team`.
    pub team_mode: Option<String>,
    /// Team gate inputs + decision JSON, when the gate was evaluated.
    pub gate_inputs_json: Option<String>,
    /// Size-capped JSON of the `<state>` block inputs.
    pub state_block_json: Option<String>,
}
