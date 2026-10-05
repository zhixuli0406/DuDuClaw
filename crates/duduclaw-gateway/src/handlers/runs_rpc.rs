//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Run inspector (G12) ─────────────────────────────────
    //
    // A "run" is one user turn → the next assistant turn of the same session
    // in `sessions.db` (the only place per-run conversation turns are
    // persisted). Tool events come from the MCP tool audit trail
    // (`tool_calls.jsonl`), correlated by agent + time window.
    //
    // HONESTY NOTE (what per-run event persistence actually is): session
    // text turns come from sessions.db, MCP-tool-call receipts from
    // tool_calls.jsonl, and — since the G12 upgrade — CLI-native tool steps
    // (`StepTracker` Start boundaries) plus `TodoWrite` board snapshots from
    // the bounded `run_steps.db` (written best-effort on the channel-reply
    // fresh-spawn CLI path; PTY-pool sessions and thinking summaries are
    // still NOT captured). `runs.get` states the remaining limits in its
    // response instead of fabricating events.

    pub(crate) async fn handle_runs_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let agent_filter = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !agent_filter.is_empty() && !is_valid_agent_id(&agent_filter) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(RUNS_LIST_DEFAULT_LIMIT as u64)
            .min(RUNS_LIST_MAX_LIMIT as u64) as usize;

        let reader = match self
            .task_list_reader(ctx, Some(agent_filter.as_str()).filter(|a| !a.is_empty()))
        {
            Ok(r) => r,
            Err(f) => return f,
        };
        let db_path = self.home_dir.join("sessions.db");
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "runs": [] }));
        }
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("open sessions db: {e}")),
        };

        let mut rows = match query_run_msg_rows(&conn, &agent_filter, RUNS_SCAN_MSG_CAP) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("runs query: {e}")),
        };
        // Fold expects per-session chronological order.
        rows.sort_by(|a, b| (&a.session_id, a.rowid).cmp(&(&b.session_id, b.rowid)));
        let now = Utc::now();
        let mut runs = fold_session_runs(&rows, now);
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs.truncate(limit);

        // Step counts: prefer the persisted CLI-native step stream
        // (run_steps.db — real `tool_use` boundaries, matched by session key
        // + window). Runs predating the step store (or PTY-pool runs, which
        // don't stream through the fresh-spawn parser) have no rows there
        // and fall back to the MCP tool audit trail as before. The two are
        // never summed — MCP calls also appear as CLI tool_use boundaries,
        // so adding them would double-count.
        let tool_rows = load_tool_call_rows(&self.home_dir);
        let step_metas = crate::run_steps::shared_store(&self.home_dir)
            .and_then(|s| {
                s.recent_tool_step_meta(&agent_filter, RUN_STEPS_SCAN_CAP)
                    .ok()
            })
            .unwrap_or_default();
        let mut runs_json: Vec<Value> = runs
            .iter()
            .map(|r| {
                let native = step_metas
                    .iter()
                    .filter(|(sk, ts)| step_meta_in_run_window(sk, ts, r, now))
                    .count();
                let steps = if native > 0 {
                    native
                } else {
                    tool_rows
                        .iter()
                        .filter(|t| tool_row_in_run_window(t, r, now))
                        .count()
                };
                run_summary_to_json(r, steps)
            })
            .collect();

        // Cron/dispatch runs (dispatch_runs in run_steps.db) — the scheduled
        // twin of the session-folded channel runs above. Previously these
        // invocations left zero rows anywhere runs.list could read (LWM D4:
        // 202 intraday cron runs, an empty run inspector). Merged newest-first
        // on started_at; `channel` carries the source ("cron"/"dispatch").
        let dispatch_runs = crate::run_steps::shared_store(&self.home_dir)
            .and_then(|s| s.list_dispatch_runs(&agent_filter, limit).ok())
            .unwrap_or_default();
        let tasks = self.task_store().await.ok();
        let mut readable = HashMap::new();
        for task_id in dispatch_runs.iter().filter_map(|r| r.task_id.as_deref()) {
            if readable.contains_key(task_id) {
                continue;
            }
            let owner = match &tasks {
                Some(t) => t.get_task(task_id).await.ok().flatten().map(|t| t.assigned_to),
                None => None,
            };
            let ok = reader.can_read_owned(task_id, owner.as_deref());
            readable.insert(task_id.to_string(), ok);
        }
        runs_json.extend(dispatch_runs.iter().map(|r| {
            // A round prompt of a task the reader may not read: no preview.
            let preview = match r.task_id.as_deref() {
                Some(t) if !readable.get(t).copied().unwrap_or(false) => None,
                _ => Some(r.preview_in.clone()),
            };
            json!({
                "id": format!("dispatch:{}", r.id),
                "session_id": format!("dispatch:{}", r.id),
                "agent_id": r.agent_id,
                "channel": r.source,
                "started_at": r.started_at,
                "ended_at": r.ended_at,
                "status": r.status,
                "step_count": r.step_count,
                "preview": preview,
                "task_id": r.task_id,
                "round": r.round,
            })
        }));
        runs_json.sort_by(|a, b| {
            b["started_at"]
                .as_str()
                .unwrap_or("")
                .cmp(a["started_at"].as_str().unwrap_or(""))
        });
        runs_json.truncate(limit);
        WsFrame::ok_response("", json!({ "runs": runs_json }))
    }

    pub(crate) async fn handle_runs_get(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let run_id = params.get("run_id").and_then(|v| v.as_str()).unwrap_or("");

        // Cron/dispatch run (`dispatch:<id>`) — served entirely from
        // run_steps.db (dispatch_runs + its `dispatch:<id>` step stream);
        // these invocations have no sessions.db presence. Same fail-closed
        // authz bar as the session branch below.
        if let Some(rest) = run_id.strip_prefix("dispatch:") {
            let Ok(id) = rest.parse::<i64>() else {
                return WsFrame::error_response("", "Missing or invalid 'run_id' parameter");
            };
            let Some(store) = crate::run_steps::shared_store(&self.home_dir) else {
                return WsFrame::error_response("", "Run not found");
            };
            let Ok(Some(run)) = store.get_dispatch_run(id) else {
                return WsFrame::error_response("", "Run not found");
            };
            if !ctx.is_admin() {
                if let Err(e) = acl::require_agent_access(ctx, &run.agent_id, AccessLevel::Viewer) {
                    return WsFrame::error_response("", &e);
                }
            }
            // A task round's transcript is task content: same gate as the
            // task's own RPCs (live identity + audience).
            if let Some(task_id) = run.task_id.as_deref().filter(|t| !t.is_empty()) {
                let tasks = match self.task_store().await {
                    Ok(s) => s,
                    Err(_) => return WsFrame::error_response("", PERMISSION_DENIED),
                };
                if let Err(f) = self
                    .authorize_task_content_read(&tasks, ctx, task_id, AccessLevel::Viewer)
                    .await
                {
                    return f;
                }
            }
            let mut events = vec![json!({
                "kind": "text",
                "role": "user",
                "ts": run.started_at,
                "preview": run.preview_in,
            })];
            if let Ok(rows) = store.recent_for_session(run_id, RUN_STEPS_SESSION_CAP) {
                events.extend(rows.iter().map(|r| {
                    json!({
                        "kind": r.kind,
                        "label": r.label,
                        "ts": r.ts,
                        "seq": r.seq,
                        "preview": r.payload_preview,
                    })
                }));
            }
            events.push(json!({
                "kind": "text",
                "role": "assistant",
                "ts": run.ended_at,
                "preview": run.preview_out,
            }));
            return WsFrame::ok_response(
                "",
                json!({
                    "run": {
                        "id": run_id,
                        "session_id": run_id,
                        "agent_id": run.agent_id,
                        "channel": run.source,
                        "started_at": run.started_at,
                        "ended_at": run.ended_at,
                        "status": run.status,
                    },
                    "events": events,
                    "event_sources": {
                        "text": "run_steps.db (dispatch_runs)",
                        "tool_step": "run_steps.db",
                    },
                    "not_persisted": ["thinking_summaries"],
                }),
            );
        }

        let Some((session_id, rowid_str)) = run_id.rsplit_once('#') else {
            return WsFrame::error_response("", "Missing or invalid 'run_id' parameter");
        };
        let Ok(rowid) = rowid_str.parse::<i64>() else {
            return WsFrame::error_response("", "Missing or invalid 'run_id' parameter");
        };

        let db_path = self.home_dir.join("sessions.db");
        if !db_path.exists() {
            return WsFrame::error_response("", "Run not found");
        }
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("open sessions db: {e}")),
        };

        // Resolve the run's user turn + owning agent. Fail closed: an unknown
        // run id yields "not found" BEFORE any payload is exposed.
        //
        // 2026-07 MED: honour the /undo //rollback tombstones like runs.list
        // does (`query_run_msg_rows`) — runs.get is the user-facing transcript
        // view, not an audit surface, so undone turns must not resolve.
        // Pre-migration DBs (no hidden/undone_at columns) fall back to the
        // plain query, mirroring `query_run_msg_rows`.
        let head_row = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        };
        let head = conn
            .query_row(
                "SELECT s.agent_id, m.role, m.content, m.timestamp
                 FROM session_messages m JOIN sessions s ON s.id = m.session_id
                 WHERE m.id = ?1 AND m.session_id = ?2
                   AND COALESCE(m.hidden, 0) = 0 AND m.undone_at IS NULL",
                params![rowid, session_id],
                head_row,
            )
            .or_else(|e| match e {
                // Older DB without the tombstone columns → plain query.
                rusqlite::Error::SqliteFailure(..) => conn.query_row(
                    "SELECT s.agent_id, m.role, m.content, m.timestamp
                     FROM session_messages m JOIN sessions s ON s.id = m.session_id
                     WHERE m.id = ?1 AND m.session_id = ?2",
                    params![rowid, session_id],
                    head_row,
                ),
                other => Err(other),
            });
        let (agent_id, role, user_content, started_at) = match head {
            Ok(v) => v,
            Err(_) => return WsFrame::error_response("", "Run not found"),
        };
        if role != "user" {
            return WsFrame::error_response("", "Run not found");
        }
        // Agent-scoped authz — same intent as activity.list, resolved from
        // the run's owning agent (viewer level, fail-closed for non-admins).
        if !ctx.is_admin() {
            if let Err(e) = acl::require_agent_access(ctx, &agent_id, AccessLevel::Viewer) {
                return WsFrame::error_response("", &e);
            }
        }

        // Window boundaries: the next user turn bounds this run; the first
        // assistant turn inside the bound is the reply.
        let next_user: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, timestamp FROM session_messages
                 WHERE session_id = ?1 AND id > ?2 AND role = 'user'
                   AND COALESCE(hidden, 0) = 0 AND undone_at IS NULL
                 ORDER BY id LIMIT 1",
                params![session_id, rowid],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .or_else(|e| match e {
                rusqlite::Error::SqliteFailure(..) => conn.query_row(
                    "SELECT id, timestamp FROM session_messages
                     WHERE session_id = ?1 AND id > ?2 AND role = 'user'
                     ORDER BY id LIMIT 1",
                    params![session_id, rowid],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                ),
                other => Err(other),
            })
            .ok();
        let bound_id = next_user.as_ref().map(|(id, _)| *id).unwrap_or(i64::MAX);
        let assistant: Option<(String, String)> = conn
            .query_row(
                "SELECT content, timestamp FROM session_messages
                 WHERE session_id = ?1 AND id > ?2 AND id < ?3 AND role = 'assistant'
                   AND COALESCE(hidden, 0) = 0 AND undone_at IS NULL
                 ORDER BY id LIMIT 1",
                params![session_id, rowid, bound_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .or_else(|e| match e {
                rusqlite::Error::SqliteFailure(..) => conn.query_row(
                    "SELECT content, timestamp FROM session_messages
                     WHERE session_id = ?1 AND id > ?2 AND id < ?3 AND role = 'assistant'
                     ORDER BY id LIMIT 1",
                    params![session_id, rowid, bound_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ),
                other => Err(other),
            })
            .ok();

        let now = Utc::now();
        let (status, ended_at) = match &assistant {
            Some((_, ts)) => ("completed".to_string(), Some(ts.clone())),
            None => {
                if next_user.is_some() {
                    ("no_reply".to_string(), None)
                } else {
                    (run_open_status(&started_at, now), None)
                }
            }
        };

        // Tool events (MCP audit trail) inside the run window.
        let window_end: Option<chrono::DateTime<Utc>> = match &ended_at {
            Some(ts) => parse_timeline_ts(ts),
            None => match &next_user {
                Some((_, ts)) => parse_timeline_ts(ts),
                None => {
                    if status == "running" {
                        Some(now)
                    } else {
                        parse_timeline_ts(&started_at)
                            .map(|t| t + chrono::Duration::seconds(RUN_RUNNING_WINDOW_SECS))
                    }
                }
            },
        };
        let window_start = parse_timeline_ts(&started_at);

        let mut events: Vec<Value> = Vec::new();
        events.push(json!({
            "kind": "text",
            "role": "user",
            "ts": started_at,
            "preview": duduclaw_core::truncate_chars(&user_content, RUN_TEXT_PREVIEW_CHARS),
        }));
        if let (Some(ws), Some(we)) = (window_start, window_end) {
            for t in load_tool_call_rows(&self.home_dir) {
                // Calls stamped with a goal task belong to that task's round
                // (its own gate applies via the dispatch branch), not to
                // this conversation (F5-D, P-M3).
                if t.agent_id != agent_id || t.task_id.is_some() {
                    continue;
                }
                let Some(ts) = parse_timeline_ts(&t.ts) else {
                    continue;
                };
                if ts >= ws && ts <= we {
                    events.push(json!({
                        "kind": "tool_use",
                        "tool": t.tool,
                        "ok": t.ok,
                        "ts": t.ts,
                        "preview": duduclaw_core::truncate_chars(&t.preview, RUN_TOOL_PREVIEW_CHARS),
                    }));
                }
            }
        }
        // Persisted CLI-native step events (run_steps.db) inside the window:
        // tool_step Start boundaries + todo_update board snapshots. Matched
        // by session key (exact) + agent + time window; a missing/empty
        // store simply contributes nothing (older runs stay as they were).
        if let (Some(ws), Some(we)) = (window_start, window_end) {
            if let Some(store) = crate::run_steps::shared_store(&self.home_dir) {
                if let Ok(rows) = store.recent_for_session(session_id, RUN_STEPS_SESSION_CAP) {
                    events.extend(persisted_step_events_for_window(&rows, ws, we));
                }
            }
        }
        if let Some((content, ts)) = &assistant {
            events.push(json!({
                "kind": "text",
                "role": "assistant",
                "ts": ts,
                "preview": duduclaw_core::truncate_chars(content, RUN_TEXT_PREVIEW_CHARS),
            }));
        }
        // Chronological order. Compare by the PARSED instant, not the raw
        // string — sources format the same time differently (sessions.db uses
        // `+00:00`, run_steps uses `Z`), so a raw string compare mis-orders
        // same-instant cross-source events. `seq` breaks remaining ties
        // (monotonic within the run_steps stream); a stable sort then keeps
        // insertion order (user turn first) for events with neither signal.
        let ts_instant = |v: &serde_json::Value| -> Option<chrono::DateTime<chrono::Utc>> {
            v.get("ts")
                .and_then(|t| t.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc))
        };
        events.sort_by(|a, b| {
            let ia = ts_instant(a);
            let ib = ts_instant(b);
            ia.cmp(&ib).then_with(|| {
                let sa = a.get("seq").and_then(|v| v.as_i64());
                let sb = b.get("seq").and_then(|v| v.as_i64());
                sa.cmp(&sb)
            })
        });

        let run = json!({
            "id": run_id,
            "session_id": session_id,
            "agent_id": agent_id,
            "channel": run_channel_of(session_id),
            "started_at": started_at,
            "ended_at": ended_at,
            "status": status,
        });
        WsFrame::ok_response(
            "",
            json!({
                "run": run,
                "events": events,
                // Honest provenance: which store produced which event kind,
                // and which live-stream kinds are NOT persisted anywhere.
                // Thinking summaries are still only counted (never captured)
                // by the stream parser, so they remain unpersisted.
                "event_sources": {
                    "text": "sessions.db",
                    "tool_use": "tool_calls.jsonl (MCP audit)",
                    "tool_step": "run_steps.db",
                    "todo_update": "run_steps.db",
                },
                "not_persisted": ["thinking_summaries"],
            }),
        )
    }
}
