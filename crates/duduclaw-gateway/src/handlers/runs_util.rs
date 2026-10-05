//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// How many most-recent session messages are scanned per listing (bounds the
/// full-table read; older turns simply fall off the "recent runs" horizon).
pub(crate) const RUNS_SCAN_MSG_CAP: usize = 4000;
/// An unanswered trailing user turn younger than this reads as "running";
/// older reads as "no_reply" (we never guess at a duration).
pub(crate) const RUN_RUNNING_WINDOW_SECS: i64 = 600;
/// Char caps (CJK-safe — `truncate_chars`, never byte slicing).
pub(crate) const RUN_LIST_PREVIEW_CHARS: usize = 120;
pub(crate) const RUN_TOOL_PREVIEW_CHARS: usize = 240;
pub(crate) const RUN_TEXT_PREVIEW_CHARS: usize = 2000;

/// One user/assistant turn row fetched from `session_messages`.
#[derive(Debug, Clone)]
pub(crate) struct RunMsgRow {
    pub rowid: i64,
    pub session_id: String,
    pub agent_id: String,
    /// "user" | "assistant" (other roles are filtered out in SQL).
    pub role: String,
    /// RFC3339.
    pub ts: String,
    /// Char-truncated content preview.
    pub preview: String,
}

/// A derived run (one user turn + its optional assistant reply).
#[derive(Debug, Clone)]
pub(crate) struct RunSummary {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub channel: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// "completed" | "running" | "no_reply"
    pub status: String,
    pub preview: String,
}

/// Channel token of a session id ("telegram:12345" → "telegram"). Ids without
/// a `:` separator get the honest bucket "other" — never a guessed channel.
pub(crate) fn run_channel_of(session_id: &str) -> String {
    match session_id.split_once(':') {
        Some((ch, _)) if !ch.is_empty() => ch.to_string(),
        _ => "other".to_string(),
    }
}

/// Status of a trailing unanswered user turn: recent ⇒ probably still being
/// answered ("running"); stale ⇒ "no_reply". Unparseable ts fails closed to
/// "no_reply" (never claims something is live without evidence).
pub(crate) fn run_open_status(started_at: &str, now: chrono::DateTime<Utc>) -> String {
    match parse_timeline_ts(started_at) {
        Some(t) if (now - t).num_seconds() <= RUN_RUNNING_WINDOW_SECS => "running".to_string(),
        _ => "no_reply".to_string(),
    }
}

/// Fold per-session-ordered message rows into runs. `rows` MUST be sorted by
/// `(session_id, rowid)`; a user turn opens a run, the next assistant turn of
/// the same session closes it. An older unanswered user turn that was
/// superseded by a newer user turn is "no_reply"; only the trailing
/// unanswered turn of a session gets the recency-based "running" check.
pub(crate) fn fold_session_runs(rows: &[RunMsgRow], now: chrono::DateTime<Utc>) -> Vec<RunSummary> {
    let mut runs: Vec<RunSummary> = Vec::new();
    let mut open: Option<RunSummary> = None;
    let mut open_session: Option<&str> = None;

    let mut finalize_open =
        |open: &mut Option<RunSummary>, superseded: bool, runs: &mut Vec<RunSummary>| {
            if let Some(mut r) = open.take() {
                r.status = if superseded {
                    "no_reply".to_string()
                } else {
                    run_open_status(&r.started_at, now)
                };
                runs.push(r);
            }
        };

    for row in rows {
        // Session boundary: whatever is still open belongs to the previous
        // session and is its trailing turn.
        if open_session.is_some_and(|s| s != row.session_id) {
            finalize_open(&mut open, false, &mut runs);
        }
        open_session = Some(&row.session_id);

        match row.role.as_str() {
            "user" => {
                // A newer user turn supersedes an unanswered one.
                finalize_open(&mut open, true, &mut runs);
                open = Some(RunSummary {
                    id: format!("{}#{}", row.session_id, row.rowid),
                    session_id: row.session_id.clone(),
                    agent_id: row.agent_id.clone(),
                    channel: run_channel_of(&row.session_id),
                    started_at: row.ts.clone(),
                    ended_at: None,
                    status: String::new(),
                    preview: row.preview.clone(),
                });
            }
            "assistant" => {
                if let Some(mut r) = open.take() {
                    r.ended_at = Some(row.ts.clone());
                    r.status = "completed".to_string();
                    runs.push(r);
                }
                // Assistant with no open user turn (e.g. system-injected
                // greeting) is not a run — skipped, never fabricated.
            }
            _ => {}
        }
    }
    finalize_open(&mut open, false, &mut runs);
    runs
}

/// One MCP tool-call receipt from `tool_calls.jsonl`.
#[derive(Debug, Clone)]
pub(crate) struct ToolCallRow {
    pub ts: String,
    pub agent_id: String,
    pub tool: String,
    pub ok: bool,
    pub preview: String,
    /// The goal task the call was made for, when the round stamped one.
    pub task_id: Option<String>,
}

/// Load the MCP tool audit trail (missing file / malformed lines ⇒ skipped;
/// rotated `tool_calls.jsonl.N` archives are NOT read — recent-runs horizon).
pub(crate) fn load_tool_call_rows(home_dir: &Path) -> Vec<ToolCallRow> {
    let path = home_dir.join("tool_calls.jsonl");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            Some(ToolCallRow {
                ts: v.get("timestamp")?.as_str()?.to_string(),
                agent_id: v.get("agent_id")?.as_str()?.to_string(),
                tool: v.get("tool_name")?.as_str()?.to_string(),
                ok: v.get("success").and_then(|s| s.as_bool()).unwrap_or(true),
                preview: v
                    .get("params_summary")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                task_id: v
                    .get("task_id")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
        })
        .collect()
}

/// The closing timestamp of a run's correlation window: the reply timestamp,
/// `now` for a running turn, or start + the running window for a no-reply
/// turn. `None` when the run's timestamps don't parse (fail closed).
pub(crate) fn run_window_end(
    run: &RunSummary,
    now: chrono::DateTime<Utc>,
) -> Option<chrono::DateTime<Utc>> {
    match &run.ended_at {
        Some(e) => parse_timeline_ts(e),
        None if run.status == "running" => Some(now),
        None => parse_timeline_ts(&run.started_at)
            .map(|s| s + chrono::Duration::seconds(RUN_RUNNING_WINDOW_SECS)),
    }
}

/// Does a tool receipt fall inside a run's window? Agent must match exactly
/// (token equality — project convention 2). Unparseable timestamps ⇒
/// excluded (fail closed).
pub(crate) fn tool_row_in_run_window(
    t: &ToolCallRow,
    run: &RunSummary,
    now: chrono::DateTime<Utc>,
) -> bool {
    // A call stamped with a goal task belongs to that task's round, not to
    // a conversation that happened to overlap it in time (F5-D, P-M3).
    if t.agent_id != run.agent_id || t.task_id.is_some() {
        return false;
    }
    let (Some(ts), Some(start)) = (parse_timeline_ts(&t.ts), parse_timeline_ts(&run.started_at))
    else {
        return false;
    };
    let Some(end) = run_window_end(run, now) else {
        return false;
    };
    ts >= start && ts <= end
}

// ── Persisted step events (run_steps.db, G12 upgrade) ───────

/// Rows fetched from `run_steps.db` per session for one `runs.get` merge.
pub(crate) const RUN_STEPS_SESSION_CAP: usize = 2_000;
/// Rows scanned for `runs.list` step counting (recent-runs horizon, mirrors
/// `RUNS_SCAN_MSG_CAP`'s honesty: older steps fall off the counter).
pub(crate) const RUN_STEPS_SCAN_CAP: usize = 20_000;

/// Does a persisted step row (session_key + ts) fall inside a run's window?
/// Session key must match exactly (steps are recorded under the very same
/// `sessions.db` key the run derives from). Unparseable timestamps ⇒
/// excluded (fail closed).
pub(crate) fn step_meta_in_run_window(
    session_key: &str,
    ts: &str,
    run: &RunSummary,
    now: chrono::DateTime<Utc>,
) -> bool {
    if session_key != run.session_id {
        return false;
    }
    let (Some(ts), Some(start)) = (parse_timeline_ts(ts), parse_timeline_ts(&run.started_at))
    else {
        return false;
    };
    let Some(end) = run_window_end(run, now) else {
        return false;
    };
    ts >= start && ts <= end
}

/// Shape the persisted step rows that fall inside `[ws, we]` (and belong to
/// `agent_id`) into `runs.get` wire events. Previews were secret-masked and
/// char-capped at write time; rows with unparseable timestamps are dropped
/// (an incomplete record must not render as a fabricated one).
/// Rows are already scoped to one session key by `recent_for_session`, and a
/// session key (`<channel>:<chat_id>`) belongs to exactly one conversation /
/// agent — so the session key is the authoritative match here. We deliberately
/// do NOT additionally filter by the run's sessions.db `agent_id`: the step
/// tee attributes with a work-dir fallback when the task-local agent id is
/// absent, which can differ from the stored agent name, and an extra agent
/// filter would then silently drop that run's real steps.
pub(crate) fn persisted_step_events_for_window(
    rows: &[crate::run_steps::RunStepRow],
    ws: chrono::DateTime<Utc>,
    we: chrono::DateTime<Utc>,
) -> Vec<Value> {
    rows.iter()
        .filter_map(|r| {
            let ts = parse_timeline_ts(&r.ts)?;
            if ts < ws || ts > we {
                return None;
            }
            Some(json!({
                "kind": r.kind,
                "label": r.label,
                "ts": r.ts,
                "seq": r.seq,
                "preview": r.payload_preview,
            }))
        })
        .collect()
}

/// Serialize one run for the wire.
pub(crate) fn run_summary_to_json(r: &RunSummary, step_count: usize) -> Value {
    json!({
        "id": r.id,
        "session_id": r.session_id,
        "agent_id": r.agent_id,
        "channel": r.channel,
        "started_at": r.started_at,
        "ended_at": r.ended_at,
        "status": r.status,
        "step_count": step_count,
        "preview": r.preview,
    })
}

/// Fetch the most recent user/assistant turns (joined to their owning agent),
/// newest-first up to `cap`, honouring the hide/undo tombstones when those
/// columns exist (pre-migration databases fall back to the plain query).
pub(crate) fn query_run_msg_rows(
    conn: &rusqlite::Connection,
    agent_filter: &str,
    cap: usize,
) -> rusqlite::Result<Vec<RunMsgRow>> {
    const WITH_TOMBSTONES: &str =
        "SELECT m.id, m.session_id, s.agent_id, m.role, substr(m.content, 1, 400), m.timestamp
         FROM session_messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.role IN ('user','assistant')
           AND (?1 = '' OR s.agent_id = ?1)
           AND COALESCE(m.hidden, 0) = 0 AND m.undone_at IS NULL
         ORDER BY m.id DESC LIMIT ?2";
    const PLAIN: &str =
        "SELECT m.id, m.session_id, s.agent_id, m.role, substr(m.content, 1, 400), m.timestamp
         FROM session_messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.role IN ('user','assistant')
           AND (?1 = '' OR s.agent_id = ?1)
         ORDER BY m.id DESC LIMIT ?2";

    let mut stmt = match conn.prepare(WITH_TOMBSTONES) {
        Ok(s) => s,
        // Older DB without the hidden/undone_at columns.
        Err(_) => conn.prepare(PLAIN)?,
    };
    let rows = stmt.query_map(params![agent_filter, cap as i64], |row| {
        Ok(RunMsgRow {
            rowid: row.get(0)?,
            session_id: row.get(1)?,
            agent_id: row.get(2)?,
            role: row.get(3)?,
            preview: duduclaw_core::truncate_chars(
                &row.get::<_, String>(4)?,
                RUN_LIST_PREVIEW_CHARS,
            ),
            ts: row.get(5)?,
        })
    })?;
    rows.collect()
}
