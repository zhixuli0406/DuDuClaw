// Data model + fetch orchestration + write dispatch for the task detail
// page (p10, `tasks_detail.rs`). Split out purely to keep `tasks_detail.rs`
// itself under this crate's own <800-line convention — same file-size
// reasoning `tasks.rs`/`tasks_data.rs` document for their own split; no
// behavior differs from an unsplit version. `tasks_detail.rs`'s own module
// doc comment carries the full visual-authority description; this file just
// owns the RPC-shape documentation for the calls it makes.
//
// ── RPC shapes (read directly from `handlers.rs`, never guessed) ────────
//   `tasks.timeline {"task_id"}` → `{"task", "iterations", "activity",
//   "pending_kickoff", "runs"}` (`handle_tasks_timeline`, ~L29696). This
//   page uses `task` (re-parsed via `tasks_data::parse_tasks` — note this
//   RPC's `task` field is a bare `task_row_to_json(&row)`, WITHOUT the
//   `channel`/`channel_link` augmentation `tasks.list`/`tasks.list_page`
//   add — see `tasks_detail.rs::channel_link_for` for how that gap is
//   closed), `activity` (the 過程 timeline — `activity_row_to_json` shape:
//   id/type/agent_id/task_id/summary/timestamp/metadata) and `runs` (only
//   for its `step_count`, summed into the 權限與稽核 card's tool-call
//   count). `iterations`/`pending_kickoff` are deliberately NOT surfaced
//   here — the Iterative Kanban revision-round story is `goals.rs`'s page
//   (goal-mode tasks only); this page's `tasks.list {"goal_mode": false}`
//   feed means `iterations` is empty for effectively every row it can show.
//   `tasks.artifacts {"task_id"}` → `{"artifacts", "truncated",
//   "inferred_count"}` (`handle_tasks_artifacts`, ~L29404), `TaskArtifact::
//   to_wire_json` shape.
//   `tasks.update {"task_id","status":"done"}` → 標記完成.
//   `tasks.comment {"task_id","body"}` → 催一下進度 (see `tasks.rs`'s
//   module doc comment for why this reuses the generic comment verb).

use serde_json::{json, Value};

use crate::screens::dashboard::Loadable;
use crate::screens::goals::spawn_goal_call as spawn_call;
use crate::screens::tasks::TasksState;
use crate::screens::tasks_data::{parse_tasks, TaskItem};
use crate::ws_status::WsConnState;
use crate::RootView;
use gpui::Context;

// ── Data model (`tasks.timeline` / `tasks.artifacts`) ───────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActivityItem {
    pub at: String,
    pub event_type: String,
    pub summary: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelineData {
    pub task: Option<TaskItem>,
    /// Chronological (oldest first) — the server's own `list_activity_for_task`
    /// order isn't documented as stable, so this is sorted client-side by
    /// `at` on parse.
    pub activity: Vec<ActivityItem>,
    /// Sum of every linked dispatch run's `step_count` — the most literal
    /// available proxy for "N 次工具呼叫已記入審計紀錄" (`run_steps.rs`:
    /// `step_count` is literally the recorded tool-call step count for that
    /// run).
    pub tool_call_count: u64,
}

pub fn parse_timeline(v: &Value) -> TimelineData {
    let task = v
        .get("task")
        .filter(|t| !t.is_null())
        .and_then(|t| parse_tasks(&json!({ "tasks": [t] })).into_iter().next());
    let mut activity: Vec<ActivityItem> = v
        .get("activity")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|e| ActivityItem {
                    at: e.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
                    event_type: e.get("type").and_then(Value::as_str).unwrap_or("").to_string(),
                    summary: e.get("summary").and_then(Value::as_str).unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    activity.sort_by(|a, b| a.at.cmp(&b.at));
    let tool_call_count: u64 = v
        .get("runs")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|r| r.get("step_count").and_then(Value::as_u64)).sum())
        .unwrap_or(0);
    TimelineData { task, activity, tool_call_count }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskArtifactItem {
    pub name: String,
    pub attribution: String,
    pub produced_at: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactsData {
    pub items: Vec<TaskArtifactItem>,
}

pub fn parse_artifacts(v: &Value) -> ArtifactsData {
    let items = v
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|a| TaskArtifactItem {
                    name: a.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    attribution: a.get("attribution").and_then(Value::as_str).unwrap_or("").to_string(),
                    produced_at: a.get("produced_at").and_then(Value::as_str).unwrap_or("").to_string(),
                    size: a.get("size").and_then(Value::as_u64),
                })
                .collect()
        })
        .unwrap_or_default();
    ArtifactsData { items }
}

pub fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{n} B")
    }
}

// ── Fetch orchestration ───────────────────────────────────────────────

pub(super) fn maybe_fetch_detail(state: &RootView, cx: &mut Context<RootView>, task_id: &str) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<TasksState>().detail_loaded_for.as_deref() == Some(task_id) {
        return;
    }
    cx.global_mut::<TasksState>().detail_loaded_for = Some(task_id.to_string());
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "tasks.timeline", json!({"task_id": task_id}), |cx, result| {
        cx.default_global::<TasksState>().detail = result.map(|v| parse_timeline(&v)).into();
    });
}

pub(super) fn maybe_fetch_artifacts(state: &RootView, cx: &mut Context<RootView>, task_id: &str) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<TasksState>().artifacts_loaded_for.as_deref() == Some(task_id) {
        return;
    }
    cx.global_mut::<TasksState>().artifacts_loaded_for = Some(task_id.to_string());
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "tasks.artifacts", json!({"task_id": task_id}), |cx, result| {
        cx.default_global::<TasksState>().artifacts = result.map(|v| parse_artifacts(&v)).into();
    });
}

/// Patches a fresh `TaskItem` (returned by `tasks.update`) into every place
/// this page's own state might be holding a stale copy — the timeline's
/// `task`, and the list/archived caches `tasks.rs` owns, so navigating back
/// to the list shows the update without a full re-fetch.
fn patch_task_everywhere(g: &mut TasksState, updated: TaskItem) {
    if let Loadable::Ready(td) = &mut g.detail {
        if let Some(t) = &mut td.task {
            if t.id == updated.id {
                *t = updated.clone();
            }
        }
    }
    if let Loadable::Ready(list) = &mut g.tasks {
        if let Some(x) = list.iter_mut().find(|x| x.id == updated.id) {
            *x = updated.clone();
        }
    }
    if let Loadable::Ready(list) = &mut g.archived_tasks {
        if let Some(x) = list.iter_mut().find(|x| x.id == updated.id) {
            *x = updated;
        }
    }
}

pub(super) fn dispatch_mark_done(cx: &mut Context<RootView>, session_tx: tokio::sync::mpsc::UnboundedSender<crate::ws_status::Command>, task_id: String) {
    {
        let g = cx.global_mut::<TasksState>();
        g.action_busy = true;
        g.action_result = None;
    }
    let params = json!({"task_id": task_id, "status": "done"});
    spawn_call(cx, session_tx, "tasks.update", params, |cx, result| {
        let g = cx.default_global::<TasksState>();
        g.action_busy = false;
        match result {
            Ok(payload) => {
                if let Some(updated) = payload
                    .get("task")
                    .filter(|t| !t.is_null())
                    .and_then(|t| parse_tasks(&json!({"tasks": [t]})).into_iter().next())
                {
                    patch_task_everywhere(g, updated);
                }
                g.action_result = Some(Ok("native.tasks.detail.markDoneOk"));
            }
            Err(e) => g.action_result = Some(Err(e)),
        }
    });
}

pub(super) fn dispatch_nudge(cx: &mut Context<RootView>, session_tx: tokio::sync::mpsc::UnboundedSender<crate::ws_status::Command>, task_id: String, body: String) {
    {
        let g = cx.global_mut::<TasksState>();
        g.action_busy = true;
        g.action_result = None;
    }
    spawn_call(cx, session_tx, "tasks.comment", json!({"task_id": task_id, "body": body}), |cx, result| {
        let g = cx.default_global::<TasksState>();
        g.action_busy = false;
        g.action_result = match result {
            Ok(_) => Some(Ok("native.tasks.detail.nudgeOk")),
            Err(e) => Some(Err(e)),
        };
    });
}

pub(super) fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "cancelled" | "failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_timeline_reads_task_activity_and_run_step_counts() {
        let v = json!({
            "task": {"id": "t1", "title": "x", "status": "in_progress"},
            "iterations": [],
            "activity": [
                {"id":"a1","type":"task_completed","agent_id":"cs","task_id":"t1","summary":"完成了","timestamp":"2026-08-21T10:00:00Z"},
                {"id":"a2","type":"task_blocked","agent_id":"cs","task_id":"t1","summary":"","timestamp":"2026-08-20T10:00:00Z"},
            ],
            "pending_kickoff": null,
            "runs": [{"id":"r1","round":1,"status":"done","started_at":null,"ended_at":null,"step_count":5}, {"id":"r2","round":2,"status":"done","started_at":null,"ended_at":null,"step_count":7}],
        });
        let d = parse_timeline(&v);
        assert_eq!(d.task.map(|t| t.id), Some("t1".to_string()));
        assert_eq!(d.activity.len(), 2);
        // sorted oldest first
        assert_eq!(d.activity[0].at, "2026-08-20T10:00:00Z");
        assert_eq!(d.tool_call_count, 12);
    }

    #[test]
    fn parse_timeline_missing_task_is_none_not_panicking() {
        let d = parse_timeline(&json!({}));
        assert!(d.task.is_none());
        assert!(d.activity.is_empty());
        assert_eq!(d.tool_call_count, 0);
    }

    #[test]
    fn parse_artifacts_reads_the_to_wire_json_shape() {
        let v = json!({ "artifacts": [
            {"name": "客訴摘要_W33.md", "archived_name": "abc", "agent_id": "cs", "origin": "dispatch", "attribution": "exact", "produced_at": "2026-08-21T10:00:00Z", "size": 18432, "round": 1, "channel": null, "source_path": null},
        ], "truncated": false, "inferred_count": 0 });
        let d = parse_artifacts(&v);
        assert_eq!(d.items.len(), 1);
        assert_eq!(d.items[0].name, "客訴摘要_W33.md");
        assert_eq!(d.items[0].size, Some(18432));
        assert_eq!(d.items[0].attribution, "exact");
    }

    #[test]
    fn format_bytes_scales_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(18_432), "18 KB");
        assert_eq!(format_bytes(2_097_152), "2.0 MB");
    }

    #[test]
    fn is_terminal_covers_done_cancelled_failed_only() {
        assert!(is_terminal("done"));
        assert!(is_terminal("cancelled"));
        assert!(is_terminal("failed"));
        assert!(!is_terminal("in_progress"));
        assert!(!is_terminal("needs_human"));
    }
}
