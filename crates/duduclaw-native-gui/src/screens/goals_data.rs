// Data model + pure parsing/filtering for the "目標" page — split out of
// `goals.rs` purely to keep that file under this crate's own <800-line
// convention (with `goals_inspector.rs` already split off too, `goals.rs`
// itself still landed at 969 lines; this second split brings it back under
// the limit). No behavior differs from an unsplit version: `SmartView` /
// `GoalTask` / `IterationRow` and their parse/filter functions have no
// dependency on `GoalsState`, fetch orchestration, or rendering — they're a
// clean, self-contained layer `goals.rs` and `goals_inspector.rs` both
// import from. See `goals.rs`'s own module doc comment for the page's full
// design (visual authority, RPC shapes, the three-button decision mapping).

use chrono::{DateTime, Local, NaiveDate};
use serde_json::Value;

// ── Smart views ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartView {
    NeedsHuman,
    InProgress,
    Today,
    Done,
}

impl Default for SmartView {
    /// The canvas's own resting-state selection (its "今天" row is the one
    /// rendered with the selected blue background) — not an arbitrary pick.
    fn default() -> Self {
        SmartView::Today
    }
}

impl SmartView {
    pub const ALL: [SmartView; 4] =
        [SmartView::NeedsHuman, SmartView::InProgress, SmartView::Today, SmartView::Done];

    pub fn label_key(self) -> &'static str {
        match self {
            SmartView::NeedsHuman => "native.goals.smartView.needsHuman",
            SmartView::InProgress => "native.goals.smartView.inProgress",
            SmartView::Today => "native.goals.smartView.today",
            SmartView::Done => "native.goals.smartView.done",
        }
    }

    /// `ACTIVE_STATUSES`/`WAITING_STATUSES`/`FINISHED_STATUSES` bucketing
    /// ported verbatim from `web/src/pages/GoalsPage.tsx` (same product,
    /// same taxonomy — not reinvented). "今天" has no web equivalent (a
    /// native-only smart view per the visual authority): any status whose
    /// `updated_at` falls on today's LOCAL calendar date, regardless of
    /// which bucket it's otherwise in — mirrors `chat/conversation_row.rs`'s
    /// `classify_group` local-date approach, not a new date convention.
    pub fn matches(self, task: &GoalTask, today: NaiveDate) -> bool {
        match self {
            SmartView::NeedsHuman => task.status == "needs_human",
            SmartView::InProgress => {
                matches!(task.status.as_str(), "todo" | "pending" | "in_progress" | "review" | "revising")
            }
            SmartView::Done => matches!(task.status.as_str(), "done" | "cancelled" | "failed" | "blocked"),
            SmartView::Today => local_date(&task.updated_at).map(|d| d == today).unwrap_or(false),
        }
    }
}

// ── GoalTask (`tasks.list {goal_mode:true}`) ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct GoalTask {
    pub id: String,
    pub title: String,
    pub status: String,
    /// `claimed_by` (who actually picked it up) falling back to
    /// `assigned_to` — the same precedent `handle_tasks_changes`/
    /// `handle_tasks_artifacts` already establish server-side for "which
    /// agent is this row's evidence attributed to".
    pub agent_id: String,
    pub revision_round: i64,
    pub judge_feedback: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub acceptance_criteria_baseline: Option<String>,
    /// Already a resolved closed-set token from the server
    /// (`PauseReason::as_str`), or `None` for a non-`needs_human` task. Never
    /// re-derived client-side — see `handlers.rs`'s own comment on why this
    /// is stamped, not guessed, at read time.
    pub pause_reason: Option<String>,
    pub updated_at: String,
    pub created_at: String,
    /// `source_channel`, when this goal originated from a channel `/goal`
    /// command (`handle_tasks_list`'s per-row augmentation, not part of
    /// `task_row_to_json` itself). `None` for dashboard-created goals — no
    /// "created_by"/source field is exposed by this RPC at all, so the
    /// inspector's meta line honestly omits a source line rather than
    /// fabricating one when this is `None`.
    pub channel: Option<String>,
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Parses the `tasks.list` response shape. A row missing `id` is dropped
/// (never panics); every other field defaults per its JSON optionality —
/// see `goals.rs`'s own module header comment for the exact source-verified
/// shape (field names read directly from `handlers.rs::task_row_to_json`).
pub fn parse_goal_tasks(v: &Value) -> Vec<GoalTask> {
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
                    Some(GoalTask {
                        id,
                        title: t.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                        status: t.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
                        agent_id,
                        revision_round: t.get("revision_round").and_then(Value::as_i64).unwrap_or(0),
                        judge_feedback: non_empty(t.get("judge_feedback").and_then(Value::as_str)),
                        acceptance_criteria: non_empty(t.get("acceptance_criteria").and_then(Value::as_str)),
                        acceptance_criteria_baseline: non_empty(
                            t.get("acceptance_criteria_baseline").and_then(Value::as_str),
                        ),
                        pause_reason: non_empty(t.get("pause_reason").and_then(Value::as_str)),
                        updated_at: t.get("updated_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        created_at: t.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        channel: non_empty(t.get("channel").and_then(Value::as_str)),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `pub(super)`: `goals.rs`'s own `short_date` (a rendering helper) reuses
/// this exact UTC→local-calendar-date conversion rather than re-parsing.
pub(super) fn local_date(iso: &str) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(iso).ok().map(|dt| dt.with_timezone(&Local).date_naive())
}

/// Priority tier for cross-view sort ordering — needs_human first, then
/// active work, then queued, then terminal-with-a-problem, then done last.
/// Same ranking PRINCIPLE `dashboard.rs::parse_goal_card`'s `tier()` already
/// applies (pick the most-actionable task to highlight); reused here as a
/// SORT key rather than a single pick, and duplicated in this file (not
/// imported from `dashboard.rs`) since that function returns `Option<u8>`
/// shaped for "is this a highlight candidate at all", not a total order
/// including `done`.
fn tier(status: &str) -> u8 {
    match status {
        "needs_human" => 0,
        "in_progress" | "review" | "revising" => 1,
        "todo" | "pending" => 2,
        "blocked" | "failed" | "cancelled" => 3,
        "done" => 4,
        _ => 5,
    }
}

/// Filter tasks into one smart view, then sort by `(tier, updated_at desc)`
/// — reproduces the canvas's own "今天" example ordering exactly (a
/// needs_human row first even though it's the OLDEST of the four rows
/// shown, then active rows newest-first, then the done row last).
/// `updated_at` is an ISO 8601 string — lexicographic string compare sorts
/// it correctly (`dashboard.rs`'s own established comment on this).
pub fn filtered_sorted(tasks: &[GoalTask], view: SmartView, today: NaiveDate) -> Vec<&GoalTask> {
    let mut v: Vec<&GoalTask> = tasks.iter().filter(|t| view.matches(t, today)).collect();
    v.sort_by(|a, b| tier(&a.status).cmp(&tier(&b.status)).then_with(|| b.updated_at.cmp(&a.updated_at)));
    v
}

// ── IterationRow (`tasks.iterations`) ────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct IterationRow {
    pub round: i64,
    pub verdict: Option<String>,
    pub judge_feedback: Option<String>,
    pub submitted_at: Option<String>,
    pub judged_at: Option<String>,
}

pub fn parse_iterations(v: &Value) -> Vec<IterationRow> {
    v.get("iterations")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|r| IterationRow {
                    round: r.get("round").and_then(Value::as_i64).unwrap_or(0),
                    verdict: non_empty(r.get("verdict").and_then(Value::as_str)),
                    judge_feedback: non_empty(r.get("judge_feedback").and_then(Value::as_str)),
                    submitted_at: non_empty(r.get("submitted_at").and_then(Value::as_str)),
                    judged_at: non_empty(r.get("judged_at").and_then(Value::as_str)),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str, status: &str, updated_at: &str, round: i64) -> GoalTask {
        GoalTask {
            id: id.to_string(),
            title: format!("goal {id}"),
            status: status.to_string(),
            agent_id: "dudu".to_string(),
            revision_round: round,
            judge_feedback: None,
            acceptance_criteria: None,
            acceptance_criteria_baseline: None,
            pause_reason: None,
            updated_at: updated_at.to_string(),
            created_at: updated_at.to_string(),
            channel: None,
        }
    }

    #[test]
    fn parse_goal_tasks_reads_the_task_row_to_json_shape() {
        let v = json!({ "tasks": [{
            "id": "t1", "title": "日報自動化", "status": "needs_human",
            "assigned_to": "finance", "claimed_by": "finance",
            "revision_round": 2, "judge_feedback": "數字對不上",
            "acceptance_criteria": "每日送出", "acceptance_criteria_baseline": "每日 09:00 前送出",
            "pause_reason": "blocked_needs_decision",
            "updated_at": "2026-08-21T10:00:00Z", "created_at": "2026-08-12T00:00:00Z",
            "channel": "telegram",
        }]});
        let tasks = parse_goal_tasks(&v);
        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!(t.id, "t1");
        assert_eq!(t.agent_id, "finance");
        assert_eq!(t.revision_round, 2);
        assert_eq!(t.judge_feedback.as_deref(), Some("數字對不上"));
        assert_eq!(t.acceptance_criteria_baseline.as_deref(), Some("每日 09:00 前送出"));
        assert_eq!(t.pause_reason.as_deref(), Some("blocked_needs_decision"));
        assert_eq!(t.channel.as_deref(), Some("telegram"));
    }

    #[test]
    fn parse_goal_tasks_prefers_claimed_by_falls_back_to_assigned_to() {
        let v = json!({ "tasks": [
            { "id": "a", "assigned_to": "x", "claimed_by": "y" },
            { "id": "b", "assigned_to": "x", "claimed_by": null },
            { "id": "c", "assigned_to": "x", "claimed_by": "" },
        ]});
        let tasks = parse_goal_tasks(&v);
        assert_eq!(tasks[0].agent_id, "y");
        assert_eq!(tasks[1].agent_id, "x");
        assert_eq!(tasks[2].agent_id, "x");
    }

    #[test]
    fn parse_goal_tasks_missing_id_is_dropped_not_panicking() {
        let v = json!({ "tasks": [{"title": "no id"}] });
        assert!(parse_goal_tasks(&v).is_empty());
    }

    #[test]
    fn parse_goal_tasks_malformed_payload_is_empty_not_panicking() {
        assert!(parse_goal_tasks(&json!({})).is_empty());
        assert!(parse_goal_tasks(&json!(null)).is_empty());
        assert!(parse_goal_tasks(&json!({"tasks": "nope"})).is_empty());
    }

    #[test]
    fn parse_iterations_reads_the_task_iteration_to_json_shape() {
        let v = json!({ "iterations": [
            { "round": 1, "verdict": "rejected", "judge_feedback": "缺對比", "submitted_at": "2026-08-20T01:00:00Z", "judged_at": "2026-08-20T01:05:00Z" },
            { "round": 2, "verdict": null, "judge_feedback": null, "submitted_at": null, "judged_at": null },
        ]});
        let rows = parse_iterations(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].round, 1);
        assert_eq!(rows[0].verdict.as_deref(), Some("rejected"));
        assert_eq!(rows[1].verdict, None);
        assert_eq!(rows[1].submitted_at, None);
    }

    #[test]
    fn smart_view_needs_human_matches_only_needs_human_status() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        assert!(SmartView::NeedsHuman.matches(&task("a", "needs_human", "2026-08-01T00:00:00Z", 0), today));
        assert!(!SmartView::NeedsHuman.matches(&task("a", "in_progress", "2026-08-01T00:00:00Z", 0), today));
    }

    #[test]
    fn smart_view_in_progress_matches_active_statuses_bucket() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        for status in ["todo", "pending", "in_progress", "review", "revising"] {
            assert!(SmartView::InProgress.matches(&task("a", status, "2026-08-01T00:00:00Z", 0), today), "{status}");
        }
        assert!(!SmartView::InProgress.matches(&task("a", "needs_human", "2026-08-01T00:00:00Z", 0), today));
    }

    #[test]
    fn smart_view_done_matches_finished_statuses_bucket() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        for status in ["done", "cancelled", "failed", "blocked"] {
            assert!(SmartView::Done.matches(&task("a", status, "2026-08-01T00:00:00Z", 0), today), "{status}");
        }
    }

    /// Host-timezone-independent by construction: derives `today`/`yesterday`
    /// from the ACTUAL local clock the test runs under (not a hardcoded UTC
    /// literal near a day boundary — an earlier draft used a fixed
    /// `"...T23:00:00Z"` and failed on any host west of UTC, e.g. this
    /// crate's own UTC+8 dev box, where 23:00 UTC is already tomorrow
    /// locally), then converts that local noon to its UTC wire form the same
    /// way the server actually produces `updated_at` timestamps.
    #[test]
    fn smart_view_today_matches_local_calendar_date_regardless_of_status() {
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
        assert!(SmartView::Today.matches(&task("a", "done", &today_iso, 0), today));
        assert!(!SmartView::Today.matches(&task("a", "done", &yesterday_iso, 0), today));
    }

    #[test]
    fn filtered_sorted_reproduces_the_canvas_ordering_needs_human_first_then_active_then_done() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let tasks = vec![
            task("done1", "done", "2026-08-21T05:00:00Z", 4),
            task("active-old", "in_progress", "2026-08-21T04:30:00Z", 0),
            task("active-new", "in_progress", "2026-08-21T06:00:00Z", 0),
            task("waiting", "needs_human", "2026-08-21T01:00:00Z", 2),
        ];
        let ids: Vec<&str> = filtered_sorted(&tasks, SmartView::Today, today).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["waiting", "active-new", "active-old", "done1"]);
    }
}
