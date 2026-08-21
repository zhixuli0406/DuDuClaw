// S4b (console, second wave) — unit tests for `console.rs`'s parsing,
// merge/filter, selection-resolution, and RPC-request-builder logic. Split
// into its own file purely to keep `console.rs` itself under this crate's
// <800-line convention (same reasoning that split the rendering half into
// `console_queue.rs`/`console_detail.rs`) — declared via `mod tests;` in
// `console.rs`, so this is still the SAME `console::tests` module, not a
// separate crate-visible one.
//
// Per this pass's task brief ("決策類 RPC 以單元測試驗 request 組裝"), the
// `build_approval_decide_params`/`build_goal_decide_params` tests below are
// this page's ENTIRE verification of the two decision RPCs — no live click
// against a real gateway was ever exercised (see `console.rs`'s own header
// comment on why: this pass's smoke test only ever reached the
// not-Authenticated empty state, since no real login credentials were
// available/sought in that environment).

use super::*;

fn approval_json(id: &str, created_at: &str) -> Value {
    json!({
        "id": id, "agent_id": "finance-bot", "kind": "tool_call",
        "summary": "8 月電費單付款", "payload": {"amount": 2180},
        "created_at": created_at, "channel": "line", "channel_link": "https://line.me/x",
    })
}

fn goal_task_json(id: &str, status: &str, updated_at: &str, feedback: &str) -> Value {
    json!({
        "id": id, "title": "每日營收日報", "status": status, "assigned_to": "report-bot",
        "updated_at": updated_at, "revision_round": 2, "judge_feedback": feedback,
        "pause_reason": "blocked_needs_decision",
    })
}

#[test]
fn parse_approvals_reads_every_field() {
    let v = json!({ "approvals": [approval_json("a-1", "2026-08-21T10:00:00Z")], "count": 1 });
    let items = parse_approvals(&v);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "a-1");
    assert_eq!(items[0].agent_id, "finance-bot");
    assert_eq!(items[0].summary, "8 月電費單付款");
    assert_eq!(items[0].channel_link.as_deref(), Some("https://line.me/x"));
    assert_eq!(items[0].payload, json!({"amount": 2180}));
}

#[test]
fn parse_approvals_missing_array_is_empty_not_a_panic() {
    assert!(parse_approvals(&json!({})).is_empty());
}

#[test]
fn parse_goal_tasks_filters_to_needs_human_only() {
    let v = json!({ "tasks": [
        goal_task_json("g-1", "needs_human", "2026-08-21T09:00:00Z", "判官指出數字落差"),
        goal_task_json("g-2", "in_progress", "2026-08-21T09:30:00Z", ""),
        goal_task_json("g-3", "done", "2026-08-21T08:00:00Z", ""),
    ]});
    let items = parse_goal_tasks(&v);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "g-1");
    assert_eq!(items[0].judge_feedback.as_deref(), Some("判官指出數字落差"));
    assert_eq!(items[0].pause_reason.as_deref(), Some("blocked_needs_decision"));
}

#[test]
fn parse_goal_tasks_empty_judge_feedback_becomes_none() {
    let v = json!({ "tasks": [goal_task_json("g-1", "needs_human", "2026-08-21T09:00:00Z", "")] });
    assert_eq!(parse_goal_tasks(&v)[0].judge_feedback, None);
}

#[test]
fn merged_queue_sorts_newest_first_across_both_kinds() {
    let approvals = vec![ApprovalItem {
        id: "a-1".into(), agent_id: "x".into(), kind: "tool_call".into(), summary: "s".into(),
        payload: Value::Null, created_at: "2026-08-21T10:00:00Z".into(), channel: None, channel_link: None,
    }];
    let goals = vec![GoalItem {
        id: "g-1".into(), title: "t".into(), assigned_to: "x".into(),
        updated_at: "2026-08-21T11:00:00Z".into(), revision_round: 0, judge_feedback: None, pause_reason: None,
    }];
    let merged = merged_queue(&approvals, &goals, Filter::All);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].selection(), Selection::Goal("g-1".into())); // newer (11:00) first
    assert_eq!(merged[1].selection(), Selection::Approval("a-1".into()));
}

#[test]
fn merged_queue_filter_waiting_approval_excludes_goals() {
    let approvals = vec![ApprovalItem {
        id: "a-1".into(), agent_id: "x".into(), kind: "tool_call".into(), summary: "s".into(),
        payload: Value::Null, created_at: "2026-08-21T10:00:00Z".into(), channel: None, channel_link: None,
    }];
    let goals = vec![GoalItem {
        id: "g-1".into(), title: "t".into(), assigned_to: "x".into(),
        updated_at: "2026-08-21T11:00:00Z".into(), revision_round: 0, judge_feedback: None, pause_reason: None,
    }];
    let merged = merged_queue(&approvals, &goals, Filter::WaitingApproval);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].selection(), Selection::Approval("a-1".into()));
}

#[test]
fn merged_queue_filter_stuck_excludes_approvals() {
    let approvals = vec![ApprovalItem {
        id: "a-1".into(), agent_id: "x".into(), kind: "tool_call".into(), summary: "s".into(),
        payload: Value::Null, created_at: "2026-08-21T10:00:00Z".into(), channel: None, channel_link: None,
    }];
    let goals = vec![GoalItem {
        id: "g-1".into(), title: "t".into(), assigned_to: "x".into(),
        updated_at: "2026-08-21T11:00:00Z".into(), revision_round: 0, judge_feedback: None, pause_reason: None,
    }];
    let merged = merged_queue(&approvals, &goals, Filter::Stuck);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].selection(), Selection::Goal("g-1".into()));
}

#[test]
fn resolve_selection_falls_back_to_first_row_when_explicit_is_stale() {
    let goals = vec![GoalItem {
        id: "g-1".into(), title: "t".into(), assigned_to: "x".into(),
        updated_at: "2026-08-21T11:00:00Z".into(), revision_round: 0, judge_feedback: None, pause_reason: None,
    }];
    let merged = merged_queue(&[], &goals, Filter::All);
    let stale = Some(Selection::Approval("gone".into()));
    assert_eq!(resolve_selection(&stale, &merged), Some(Selection::Goal("g-1".into())));
}

#[test]
fn resolve_selection_keeps_a_still_valid_explicit_choice() {
    let goals = vec![
        GoalItem {
            id: "g-1".into(), title: "t".into(), assigned_to: "x".into(),
            updated_at: "2026-08-21T11:00:00Z".into(), revision_round: 0, judge_feedback: None, pause_reason: None,
        },
        GoalItem {
            id: "g-2".into(), title: "t2".into(), assigned_to: "x".into(),
            updated_at: "2026-08-21T12:00:00Z".into(), revision_round: 0, judge_feedback: None, pause_reason: None,
        },
    ];
    let merged = merged_queue(&[], &goals, Filter::All);
    let explicit = Some(Selection::Goal("g-1".into()));
    assert_eq!(resolve_selection(&explicit, &merged), Some(Selection::Goal("g-1".into())));
}

#[test]
fn resolve_selection_empty_queue_is_none() {
    assert_eq!(resolve_selection(&None, &[]), None);
}

fn empty_snapshot() -> ConsoleSnapshot {
    ConsoleSnapshot {
        approvals: Loadable::Loading,
        goals: Loadable::Loading,
        filter: Filter::All,
        selected: None,
        goal_note_open: false,
        goal_note: String::new(),
        decision_busy: None,
        decision_error: None,
    }
}

#[test]
fn queue_load_state_loading_while_either_source_pending() {
    let mut snap = empty_snapshot();
    assert!(matches!(queue_load_state(&snap), QueueLoad::Loading));
    snap.approvals = Loadable::Ready(vec![]);
    assert!(matches!(queue_load_state(&snap), QueueLoad::Loading), "goals still Loading");
}

#[test]
fn queue_load_state_ready_with_one_source_failed_keeps_the_other() {
    let mut snap = empty_snapshot();
    snap.approvals = Loadable::Failed("讀取失敗".to_string());
    snap.goals = Loadable::Ready(vec![GoalItem {
        id: "g-1".into(), title: "t".into(), assigned_to: "x".into(),
        updated_at: "2026-08-21T11:00:00Z".into(), revision_round: 0, judge_feedback: None, pause_reason: None,
    }]);
    match queue_load_state(&snap) {
        QueueLoad::Ready { entries, approvals_error, goals_error } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(approvals_error, Some("讀取失敗"));
            assert_eq!(goals_error, None);
        }
        QueueLoad::Loading => panic!("expected Ready"),
    }
}

#[test]
fn build_approval_decide_params_shape() {
    assert_eq!(build_approval_decide_params("a-1", true), json!({"id": "a-1", "approve": true}));
    assert_eq!(build_approval_decide_params("a-1", false), json!({"id": "a-1", "approve": false}));
}

#[test]
fn build_goal_decide_params_retry_without_note_omits_the_key() {
    let v = build_goal_decide_params("g-1", "retry", None);
    assert_eq!(v, json!({"task_id": "g-1", "action": "retry"}));
    assert!(v.get("note").is_none(), "note key must be absent, not null, when no note is given");
}

#[test]
fn build_goal_decide_params_retry_with_note_includes_it() {
    let v = build_goal_decide_params("g-1", "retry", Some("先確認金額"));
    assert_eq!(v, json!({"task_id": "g-1", "action": "retry", "note": "先確認金額"}));
}

#[test]
fn build_goal_decide_params_abort_has_no_note_key() {
    let v = build_goal_decide_params("g-1", "abort", None);
    assert_eq!(v, json!({"task_id": "g-1", "action": "abort"}));
}

#[test]
fn short_time_extracts_hh_mm() {
    assert_eq!(short_time("2026-08-21T13:45:00Z"), "13:45");
    assert_eq!(short_time("not-a-timestamp"), "not-a-timestamp");
}

#[test]
fn truncate_chars_is_codepoint_safe_on_cjk() {
    let s = "判官指出數字落差，需要你決定要不要重新跑一輪";
    let out = truncate_chars(s, 6);
    assert_eq!(out.chars().count(), 7); // 6 + the appended ellipsis
    assert!(out.starts_with("判官指出數字"));
}

#[test]
fn truncate_chars_no_op_under_the_limit() {
    assert_eq!(truncate_chars("hi", 10), "hi");
}
