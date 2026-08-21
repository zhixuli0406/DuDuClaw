// Data model + pure parsing/filtering for the "任務" page (p09/p10, S4b third
// wave). Split out of `tasks.rs` for the same file-size reason
// `goals.rs`/`goals_data.rs` are split — see that pair's own doc comments
// for the precedent this file-layout choice follows.
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ─────────────────────────────────────────
//   `tasks.list {"goal_mode": false}` → `{"tasks": [...]}`, each row shaped
//   by `task_row_to_json` (~L33422) plus a per-row `"channel"`/
//   `"channel_link"` field `handle_tasks_list` adds on top (~L28741) — same
//   shape `goals_data.rs`'s `GoalTask` already documents for the goal-mode
//   sibling RPC, minus the goal-loop-only fields (`revision_round`,
//   `acceptance_criteria*`, `pause_reason`) this page has no use for.
//   `tasks.list_page {"goal_mode": false, "archived": true, ...}` →
//   `{"tasks": [...], "total", "limit", "offset"}` (`handle_tasks_list_page`,
//   ~L29152) — the ONLY way to see archived rows: `tasks.list`'s backing
//   query hard-codes `AND archived = 0` (`task_store.rs::
//   list_tasks_filtered`), so the 封存 smart view is a genuinely separate
//   fetch, not a client-side filter over the same list (unlike the other
//   three smart views below).
//
// ── Why "指派給我" is defined the way it is (a real design decision, not a
// guess) ─────────────────────────────────────────────────────────────────
// A human operator is never the VALUE of `assigned_to` in this system — that
// column always holds an AI agent id (`task_store.rs`, every dispatch path).
// So "assigned to me" can't mean "assigned_to == my user id" the way it
// would on a human-only board. What the codebase actually models for "which
// AI staff are mine" is the ACL binding table (`duduclaw-auth::db::
// get_user_agents`, `UserAgentBinding { user_id, agent_name, access_level,
// bound_at }`), exposed read-only via `users.me` → `{"user": {...},
// "bindings": [...]}` (`handle_users_me`, ~L26751). This page fetches that
// once and defines 指派給我 as "the task's agent is one I'm bound to".
//
// The degenerate case matters: the common personal-edition install is a
// single admin account with ZERO explicit binding rows (an admin bypasses
// per-agent ACL by role, not by binding rows — `duduclaw-auth::acl::
// UserContext::is_admin`), so a literal "must appear in bindings" rule would
// make 指派給我 permanently empty for the majority of native-GUI users. This
// file's `TasksState::my_agent_ids` (in `tasks.rs`) therefore resolves to
// `None` ("no restriction — show everything") when the bindings list comes
// back empty, and `Some(ids)` (real restriction) only when the account
// actually has explicit bindings — see `parse_my_agent_ids` below.
use chrono::{DateTime, Local, NaiveDate};
use serde_json::Value;

// ── Smart views ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartView {
    Today,
    AssignedToMe,
    All,
    Archived,
}

impl Default for SmartView {
    /// The canvas's own resting-state selection (`Tasks.dc.html`'s "今天"
    /// row renders with the selected blue background) — not an arbitrary
    /// pick, same precedent `goals_data::SmartView`'s own default documents.
    fn default() -> Self {
        SmartView::Today
    }
}

impl SmartView {
    pub const ALL: [SmartView; 4] =
        [SmartView::Today, SmartView::AssignedToMe, SmartView::All, SmartView::Archived];

    pub fn label_key(self) -> &'static str {
        match self {
            SmartView::Today => "native.tasks.smartView.today",
            SmartView::AssignedToMe => "native.tasks.smartView.assignedToMe",
            SmartView::All => "native.tasks.smartView.all",
            SmartView::Archived => "native.tasks.smartView.archived",
        }
    }
}

// ── TaskItem (`tasks.list` / `tasks.list_page`) ──────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct TaskItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub assigned_to: String,
    /// `claimed_by` when set (an agent actually picked the row up), falling
    /// back to `assigned_to` — same precedent `goals_data::GoalTask::
    /// agent_id` documents (mirrors `handle_tasks_changes`/
    /// `handle_tasks_artifacts`'s own server-side attribution rule).
    pub agent_id: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub judge_feedback: Option<String>,
    pub blocked_reason: Option<String>,
    pub parent_task_id: Option<String>,
    pub tags: Vec<String>,
    /// Agent self-reported progress/result text (`TaskRow::result_summary`)
    /// — the most literal available match for the canvas's quick-view
    /// "最新進度" box (see `dispatch_engine.rs`'s own doc comment: "the
    /// agent's self-reported final answer").
    pub result_summary: Option<String>,
    pub deadline_at: Option<String>,
    pub archived: bool,
    pub pinned: bool,
    /// Present only when this task originated from a channel `/goal`-style
    /// entry point (`handle_tasks_list`'s per-row augmentation) — `None` for
    /// dashboard/manually-created tasks.
    pub channel: Option<String>,
    /// Resolved deep link back to the source conversation, when both a
    /// channel AND a reachable conversation exist. Independent of
    /// `channel` being `Some` (resolution can still fail).
    pub channel_link: Option<String>,
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Parses either RPC's `{"tasks": [...]}` shape (identical row shape for
/// both — `handle_tasks_list`/`handle_tasks_list_page` both call
/// `task_row_to_json` plus the same channel/channel_link augmentation). A
/// row missing `id` is dropped, never panics.
pub fn parse_tasks(v: &Value) -> Vec<TaskItem> {
    v.get("tasks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let id = t.get("id")?.as_str()?.to_string();
                    let claimed_by = t.get("claimed_by").and_then(Value::as_str);
                    let assigned_to = t.get("assigned_to").and_then(Value::as_str).unwrap_or("");
                    let agent_id =
                        claimed_by.filter(|s| !s.is_empty()).unwrap_or(assigned_to).to_string();
                    let tags = t
                        .get("tags")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter().filter_map(Value::as_str).map(str::to_string).collect()
                        })
                        .unwrap_or_default();
                    Some(TaskItem {
                        id,
                        title: t.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                        description: t.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        status: t.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
                        priority: t.get("priority").and_then(Value::as_str).unwrap_or("medium").to_string(),
                        assigned_to: assigned_to.to_string(),
                        agent_id,
                        created_by: t.get("created_by").and_then(Value::as_str).unwrap_or("").to_string(),
                        created_at: t.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        updated_at: t.get("updated_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        judge_feedback: non_empty(t.get("judge_feedback").and_then(Value::as_str)),
                        blocked_reason: non_empty(t.get("blocked_reason").and_then(Value::as_str)),
                        parent_task_id: non_empty(t.get("parent_task_id").and_then(Value::as_str)),
                        tags,
                        result_summary: non_empty(t.get("result_summary").and_then(Value::as_str)),
                        deadline_at: non_empty(t.get("deadline_at").and_then(Value::as_str)),
                        archived: t.get("archived").and_then(Value::as_bool).unwrap_or(false),
                        pinned: t.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                        channel: non_empty(t.get("channel").and_then(Value::as_str)),
                        channel_link: non_empty(t.get("channel_link").and_then(Value::as_str)),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `users.me` → `{"user": {...}, "bindings": [{"agent_name": ..., ...}]}` —
/// see this file's module doc comment for why an empty bindings list
/// resolves to `None` ("no restriction"), not `Some(vec![])`.
pub fn parse_my_agent_ids(v: &Value) -> Option<Vec<String>> {
    let ids: Vec<String> = v
        .get("bindings")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.get("agent_name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

/// UTC ISO 8601 → local calendar date. Deliberately a small local duplicate
/// of `goals_data::local_date` rather than reaching into that sibling
/// module's `pub(super)` item — same "thin intentional duplication over
/// widening visibility for one extra caller" call `goals_inspector.rs`'s own
/// `goals_status_dot` already makes for this crate.
fn local_date(iso: &str) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(iso).ok().map(|dt| dt.with_timezone(&Local).date_naive())
}

/// Filters the (non-archived) task list into one of the three client-side
/// smart views. Never called with `SmartView::Archived` — that view's rows
/// come from a wholly separate fetch (`tasks.list_page`) and are rendered
/// as-is; see `tasks.rs`'s fetch orchestration. Deliberately does NOT
/// re-sort: both backing RPCs already return `ORDER BY pinned DESC,
/// updated_at DESC` (`task_store.rs::list_tasks_filtered`/
/// `list_tasks_paginated`), and filtering preserves that order.
pub fn filtered<'a>(tasks: &'a [TaskItem], view: SmartView, today: NaiveDate, my_agent_ids: Option<&[String]>) -> Vec<&'a TaskItem> {
    tasks
        .iter()
        .filter(|t| match view {
            SmartView::All | SmartView::Archived => true,
            SmartView::Today => local_date(&t.updated_at).map(|d| d == today).unwrap_or(false),
            SmartView::AssignedToMe => match my_agent_ids {
                None => true,
                Some(ids) => ids.iter().any(|a| a == &t.agent_id),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str, agent_id: &str, updated_at: &str) -> TaskItem {
        TaskItem {
            id: id.to_string(),
            title: format!("task {id}"),
            description: String::new(),
            status: "todo".to_string(),
            priority: "medium".to_string(),
            assigned_to: agent_id.to_string(),
            agent_id: agent_id.to_string(),
            created_by: "system".to_string(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            judge_feedback: None,
            blocked_reason: None,
            parent_task_id: None,
            tags: Vec::new(),
            result_summary: None,
            deadline_at: None,
            archived: false,
            pinned: false,
            channel: None,
            channel_link: None,
        }
    }

    #[test]
    fn parse_tasks_reads_the_task_row_to_json_shape() {
        let v = json!({ "tasks": [{
            "id": "t1", "title": "整理本週客訴摘要", "description": "彙整客訴",
            "status": "in_progress", "priority": "high",
            "assigned_to": "cs-lead", "claimed_by": "cs-lead",
            "created_by": "u1", "created_at": "2026-08-19T00:00:00Z", "updated_at": "2026-08-21T10:00:00Z",
            "judge_feedback": null, "blocked_reason": null, "parent_task_id": null,
            "tags": ["客服", "本週"], "result_summary": "27 筆已分 6 類",
            "deadline_at": "2026-08-21T18:00:00Z", "archived": false, "pinned": true,
            "channel": "telegram", "channel_link": "https://t.me/x",
        }]});
        let tasks = parse_tasks(&v);
        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!(t.id, "t1");
        assert_eq!(t.agent_id, "cs-lead");
        assert_eq!(t.tags, vec!["客服".to_string(), "本週".to_string()]);
        assert_eq!(t.result_summary.as_deref(), Some("27 筆已分 6 類"));
        assert!(t.pinned);
        assert_eq!(t.channel.as_deref(), Some("telegram"));
        assert_eq!(t.channel_link.as_deref(), Some("https://t.me/x"));
    }

    #[test]
    fn parse_tasks_prefers_claimed_by_falls_back_to_assigned_to() {
        let v = json!({ "tasks": [
            { "id": "a", "assigned_to": "x", "claimed_by": "y" },
            { "id": "b", "assigned_to": "x", "claimed_by": null },
            { "id": "c", "assigned_to": "x", "claimed_by": "" },
        ]});
        let tasks = parse_tasks(&v);
        assert_eq!(tasks[0].agent_id, "y");
        assert_eq!(tasks[1].agent_id, "x");
        assert_eq!(tasks[2].agent_id, "x");
    }

    #[test]
    fn parse_tasks_missing_id_is_dropped_not_panicking() {
        let v = json!({ "tasks": [{"title": "no id"}] });
        assert!(parse_tasks(&v).is_empty());
    }

    #[test]
    fn parse_tasks_malformed_payload_is_empty_not_panicking() {
        assert!(parse_tasks(&json!({})).is_empty());
        assert!(parse_tasks(&json!(null)).is_empty());
        assert!(parse_tasks(&json!({"tasks": "nope"})).is_empty());
    }

    #[test]
    fn parse_my_agent_ids_empty_bindings_means_no_restriction() {
        assert_eq!(parse_my_agent_ids(&json!({"user": {}, "bindings": []})), None);
        assert_eq!(parse_my_agent_ids(&json!({"user": {}})), None);
    }

    #[test]
    fn parse_my_agent_ids_reads_agent_name_from_each_binding() {
        let v = json!({ "user": {}, "bindings": [
            {"user_id": "u1", "agent_name": "cs-lead", "access_level": "operator"},
            {"user_id": "u1", "agent_name": "finance", "access_level": "viewer"},
        ]});
        assert_eq!(parse_my_agent_ids(&v), Some(vec!["cs-lead".to_string(), "finance".to_string()]));
    }

    #[test]
    fn filtered_today_matches_local_calendar_date_regardless_of_status() {
        use chrono::TimeZone;
        let today = Local::now().date_naive();
        let yesterday = today.pred_opt().expect("today is not the minimum representable date");
        let today_iso = Local
            .from_local_datetime(&today.and_hms_opt(12, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc)
            .to_rfc3339();
        let yesterday_iso = Local
            .from_local_datetime(&yesterday.and_hms_opt(12, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc)
            .to_rfc3339();
        let tasks = vec![task("a", "x", &today_iso), task("b", "x", &yesterday_iso)];
        let ids: Vec<&str> = filtered(&tasks, SmartView::Today, today, None).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a"]);
    }

    #[test]
    fn filtered_assigned_to_me_none_restriction_shows_everything() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let tasks = vec![task("a", "cs-lead", "2026-08-01T00:00:00Z"), task("b", "finance", "2026-08-01T00:00:00Z")];
        let ids: Vec<&str> =
            filtered(&tasks, SmartView::AssignedToMe, today, None).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn filtered_assigned_to_me_restricts_to_bound_agents() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let tasks = vec![task("a", "cs-lead", "2026-08-01T00:00:00Z"), task("b", "finance", "2026-08-01T00:00:00Z")];
        let mine = vec!["cs-lead".to_string()];
        let ids: Vec<&str> =
            filtered(&tasks, SmartView::AssignedToMe, today, Some(&mine)).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a"]);
    }

    #[test]
    fn filtered_all_and_archived_are_unfiltered() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let tasks = vec![task("a", "x", "2026-08-01T00:00:00Z"), task("b", "y", "2020-01-01T00:00:00Z")];
        assert_eq!(filtered(&tasks, SmartView::All, today, None).len(), 2);
        assert_eq!(filtered(&tasks, SmartView::Archived, today, None).len(), 2);
    }
}
