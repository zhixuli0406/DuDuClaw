//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Activity handlers ───────────────────────────────────

    pub(crate) async fn handle_activity_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let event_type = params.get("type").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
        let offset = params.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);

        // Task-scoped mode: every event for one task (chronological), no
        // global-window washout. Same response shape. The task itself is the
        // unit of access: its agent, its audience, the live identity.
        if let Some(task_id) = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if let Err(f) = self
                .authorize_task_content_read(&store, ctx, task_id, AccessLevel::Viewer)
                .await
            {
                return f;
            }
            return match store.list_activity_for_task(task_id, limit.max(1)).await {
                Ok(rows) => {
                    let total = rows.len() as i64;
                    let events: Vec<Value> = rows.iter().map(activity_row_to_json).collect();
                    WsFrame::ok_response("", json!({ "events": events, "total": total }))
                }
                Err(e) => WsFrame::error_response("", &format!("list task activity: {e}")),
            };
        }

        let reader = match self.task_list_reader(ctx, agent_id) {
            Ok(r) => r,
            Err(f) => return f,
        };
        match store
            .list_activity(agent_id, event_type, limit, offset)
            .await
        {
            Ok((rows, total)) => {
                let owners = activity_task_owners(&store, &rows).await;
                let events: Vec<Value> = rows
                    .iter()
                    .map(|r| {
                        let owner = r.task_id.as_deref().and_then(|t| owners.get(t));
                        reader.activity_json(r, owner.map(String::as_str))
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "events": events, "total": total }))
            }
            Err(e) => WsFrame::error_response("", &format!("list activity: {e}")),
        }
    }

    // ── Work Timeline (G11) ─────────────────────────────────
    //
    // Company-level Gantt rows derived from the stores that already carry
    // real timestamps: the task board (ranged: created/claimed → completed),
    // the activity feed (instants), and the in-memory heartbeat scheduler
    // (`last_run` instants only — heartbeat run durations are NOT persisted
    // anywhere, so heartbeats are honestly rendered as dots, never bars).

    pub(crate) async fn handle_timeline_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let now = Utc::now();
        let to = params
            .get("to")
            .and_then(|v| v.as_str())
            .and_then(parse_timeline_ts)
            .unwrap_or(now);
        let from = params
            .get("from")
            .and_then(|v| v.as_str())
            .and_then(parse_timeline_ts)
            .unwrap_or_else(|| to - chrono::Duration::hours(24));
        if from >= to {
            return WsFrame::error_response("", "invalid range: `from` must be earlier than `to`");
        }
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let reader = match self.task_list_reader(ctx, agent_id) {
            Ok(r) => r,
            Err(f) => return f,
        };
        let tasks = match store.list_tasks(None, agent_id, None).await {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("timeline tasks: {e}")),
        };
        // Most-recent TIMELINE_ROW_CAP activity events; events older than the
        // newest cap-full may be missing from wide windows — the `truncated`
        // flag + `cap` in the response make that visible to the client.
        let (activities, _total) = match store
            .list_activity(agent_id, None, TIMELINE_ROW_CAP as i64, 0)
            .await
        {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("timeline activity: {e}")),
        };
        // Task titles are board-card fields; activity text tied to a task the
        // reader may not read is dropped (task_privacy gate).
        let owners = activity_task_owners(&store, &activities).await;
        let activities: Vec<ActivityRow> = activities
            .into_iter()
            .map(|mut a| {
                if let Some(t) = a.task_id.as_deref().filter(|t| !t.is_empty()) {
                    let readable = reader.can_read_owned(t, owners.get(t).map(String::as_str));
                    if !readable {
                        a.summary.clear();
                        a.metadata = None;
                    }
                }
                a
            })
            .collect();
        let heartbeats: Vec<(String, Option<String>)> = {
            let hb = self.heartbeat.read().await;
            match hb.as_ref() {
                Some(scheduler) => scheduler
                    .status()
                    .await
                    .into_iter()
                    .filter(|h| agent_id.is_none_or(|a| a == h.agent_id))
                    .map(|h| (h.agent_id, h.last_run))
                    .collect(),
                None => Vec::new(),
            }
        };

        let (rows, truncated) =
            derive_timeline_rows(&tasks, &activities, &heartbeats, from, to, now);
        WsFrame::ok_response(
            "",
            json!({
                "rows": rows,
                "cap": TIMELINE_ROW_CAP,
                "truncated": truncated,
                "from": from.to_rfc3339(),
                "to": to.to_rfc3339(),
            }),
        )
    }
}
