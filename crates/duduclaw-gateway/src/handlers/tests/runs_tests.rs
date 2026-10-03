//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;
use chrono::TimeZone;

fn ts(h: u32, m: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 11, h, m, 0).unwrap()
}

fn msg(rowid: i64, session: &str, role: &str, at: chrono::DateTime<Utc>) -> RunMsgRow {
    RunMsgRow {
        rowid,
        session_id: session.into(),
        agent_id: "bruno".into(),
        role: role.into(),
        ts: at.to_rfc3339(),
        preview: format!("m{rowid}"),
    }
}

#[test]
fn user_then_assistant_becomes_completed_run() {
    let rows = vec![
        msg(1, "telegram:1", "user", ts(1, 0)),
        msg(2, "telegram:1", "assistant", ts(1, 2)),
    ];
    let runs = fold_session_runs(&rows, ts(9, 0));
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, "telegram:1#1");
    assert_eq!(runs[0].status, "completed");
    assert_eq!(
        runs[0].ended_at.as_deref(),
        Some(ts(1, 2).to_rfc3339().as_str())
    );
    assert_eq!(runs[0].channel, "telegram");
}

#[test]
fn superseded_user_turn_is_no_reply_and_trailing_recent_turn_is_running() {
    let rows = vec![
        msg(1, "discord:9", "user", ts(1, 0)), // superseded, never answered
        msg(2, "discord:9", "user", ts(8, 59)), // trailing, 1 min before "now"
    ];
    let runs = fold_session_runs(&rows, ts(9, 0));
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].status, "no_reply");
    assert!(runs[0].ended_at.is_none());
    assert_eq!(
        runs[1].status, "running",
        "recent unanswered trailing turn is live"
    );
}

#[test]
fn stale_trailing_turn_is_no_reply_not_running() {
    let rows = vec![msg(1, "line:5", "user", ts(1, 0))];
    let runs = fold_session_runs(&rows, ts(9, 0));
    assert_eq!(
        runs[0].status, "no_reply",
        "8h old with no reply is not 'running'"
    );
}

#[test]
fn session_boundary_closes_open_run_and_orphan_assistant_is_skipped() {
    let rows = vec![
        msg(1, "slack:a", "assistant", ts(0, 30)), // orphan — no run fabricated
        msg(2, "slack:a", "user", ts(1, 0)),       // trailing open of session a
        msg(3, "slack:b", "user", ts(2, 0)),
        msg(4, "slack:b", "assistant", ts(2, 1)),
    ];
    let runs = fold_session_runs(&rows, ts(9, 0));
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].session_id, "slack:a");
    assert_eq!(runs[0].status, "no_reply");
    assert_eq!(runs[1].session_id, "slack:b");
    assert_eq!(runs[1].status, "completed");
}

#[test]
fn tool_window_matches_agent_and_time_range_only() {
    let run = RunSummary {
        id: "telegram:1#1".into(),
        session_id: "telegram:1".into(),
        agent_id: "bruno".into(),
        channel: "telegram".into(),
        started_at: ts(1, 0).to_rfc3339(),
        ended_at: Some(ts(1, 5).to_rfc3339()),
        status: "completed".into(),
        preview: String::new(),
    };
    let inside = ToolCallRow {
        ts: ts(1, 2).to_rfc3339(),
        agent_id: "bruno".into(),
        tool: "wiki_read".into(),
        ok: true,
        preview: "path=sop".into(),
    };
    let wrong_agent = ToolCallRow {
        agent_id: "agnes".into(),
        ..inside.clone()
    };
    let too_late = ToolCallRow {
        ts: ts(2, 0).to_rfc3339(),
        ..inside.clone()
    };
    let now = ts(9, 0);
    assert!(tool_row_in_run_window(&inside, &run, now));
    assert!(!tool_row_in_run_window(&wrong_agent, &run, now));
    assert!(!tool_row_in_run_window(&too_late, &run, now));
}

#[test]
fn channel_of_handles_missing_separator() {
    assert_eq!(run_channel_of("telegram:123"), "telegram");
    assert_eq!(run_channel_of("weird-session-id"), "other");
    assert_eq!(run_channel_of(":123"), "other");
}

fn step_row(
    agent: &str,
    session: &str,
    at: chrono::DateTime<Utc>,
    kind: &str,
    label: &str,
    seq: i64,
) -> crate::run_steps::RunStepRow {
    crate::run_steps::RunStepRow {
        agent_id: agent.into(),
        session_key: session.into(),
        ts: at.to_rfc3339(),
        kind: kind.into(),
        label: label.into(),
        payload_preview: format!("preview-{label}"),
        seq,
    }
}

#[test]
fn persisted_steps_merge_within_window_regardless_of_agent_label() {
    let ws = ts(1, 0);
    let we = ts(1, 5);
    // Rows arrive already session-key-scoped from recent_for_session; the
    // window is the only remaining filter. The last row carries a
    // divergent agent label (the step-tee work-dir fallback) and MUST
    // still show — an agent filter here would silently drop it.
    let rows = vec![
        step_row("bruno", "telegram:1", ts(1, 1), "tool_step", "Read", 1),
        step_row("bruno", "telegram:1", ts(1, 2), "todo_update", "1/3", 2),
        // Outside the window — excluded.
        step_row("bruno", "telegram:1", ts(2, 0), "tool_step", "Bash", 3),
        // Divergent agent label (work-dir fallback) — still included.
        step_row("bruno-dir", "telegram:1", ts(1, 3), "tool_step", "Grep", 4),
    ];
    let events = persisted_step_events_for_window(&rows, ws, we);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["label"], "Read");
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[2]["label"], "Grep");
}

#[test]
fn persisted_steps_with_bad_timestamps_are_dropped_not_fabricated() {
    let mut row = step_row("bruno", "telegram:1", ts(1, 1), "tool_step", "Read", 1);
    row.ts = "not-a-time".into();
    assert!(persisted_step_events_for_window(&[row], ts(1, 0), ts(2, 0)).is_empty());
}

#[test]
fn step_meta_window_requires_exact_session_and_time_range() {
    let run = RunSummary {
        id: "telegram:1#1".into(),
        session_id: "telegram:1".into(),
        agent_id: "bruno".into(),
        channel: "telegram".into(),
        started_at: ts(1, 0).to_rfc3339(),
        ended_at: Some(ts(1, 5).to_rfc3339()),
        status: "completed".into(),
        preview: String::new(),
    };
    let now = ts(9, 0);
    let inside = ts(1, 2).to_rfc3339();
    assert!(step_meta_in_run_window("telegram:1", &inside, &run, now));
    // Exact session key equality — never substring (convention 2).
    assert!(!step_meta_in_run_window("telegram:12", &inside, &run, now));
    assert!(!step_meta_in_run_window(
        "telegram:1",
        &ts(2, 0).to_rfc3339(),
        &run,
        now
    ));
    assert!(!step_meta_in_run_window("telegram:1", "garbage", &run, now));
}

#[test]
fn run_window_end_covers_reply_running_and_no_reply_shapes() {
    let base = RunSummary {
        id: "telegram:1#1".into(),
        session_id: "telegram:1".into(),
        agent_id: "bruno".into(),
        channel: "telegram".into(),
        started_at: ts(1, 0).to_rfc3339(),
        ended_at: Some(ts(1, 5).to_rfc3339()),
        status: "completed".into(),
        preview: String::new(),
    };
    let now = ts(9, 0);
    assert_eq!(run_window_end(&base, now), Some(ts(1, 5)));
    let running = RunSummary {
        ended_at: None,
        status: "running".into(),
        ..base.clone()
    };
    assert_eq!(run_window_end(&running, now), Some(now));
    let no_reply = RunSummary {
        ended_at: None,
        status: "no_reply".into(),
        ..base.clone()
    };
    assert_eq!(
        run_window_end(&no_reply, now),
        Some(ts(1, 0) + chrono::Duration::seconds(RUN_RUNNING_WINDOW_SECS)),
    );
    let bad_end = RunSummary {
        ended_at: Some("garbage".into()),
        ..base
    };
    assert_eq!(
        run_window_end(&bad_end, now),
        None,
        "unparseable end fails closed"
    );
}
