//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;
use chrono::TimeZone;

fn ts(h: u32, m: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 11, h, m, 0).unwrap()
}

fn task(id: &str, status: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        format!("task {id}"),
        String::new(),
        "medium".into(),
        "bruno".into(),
        "boss".into(),
    );
    t.status = status.into();
    t.created_at = ts(1, 0).to_rfc3339();
    t.updated_at = ts(2, 0).to_rfc3339();
    t
}

fn activity(id: &str, event_type: &str, at: chrono::DateTime<Utc>) -> ActivityRow {
    ActivityRow {
        id: id.into(),
        event_type: event_type.into(),
        agent_id: "bruno".into(),
        task_id: None,
        summary: format!("event {id}"),
        timestamp: at.to_rfc3339(),
        metadata: None,
    }
}

#[test]
fn done_task_becomes_closed_bar_with_real_end() {
    let mut t = task("t1", "done");
    t.claimed_at = Some(ts(1, 30).to_rfc3339());
    t.completed_at = Some(ts(3, 0).to_rfc3339());
    let row = timeline_row_from_task(&t, ts(0, 0), ts(23, 0), ts(23, 0)).unwrap();
    assert_eq!(row.started_at, ts(1, 30).to_rfc3339()); // claim wins over creation
    assert_eq!(
        row.ended_at.as_deref(),
        Some(ts(3, 0).to_rfc3339().as_str())
    );
    assert_eq!(row.kind, "task");
    assert_eq!(row.status, "done");
}

#[test]
fn in_progress_task_is_open_ended_running_bar() {
    let mut t = task("t2", "in_progress");
    t.claimed_at = Some(ts(4, 0).to_rfc3339());
    let row = timeline_row_from_task(&t, ts(0, 0), ts(23, 0), ts(23, 0)).unwrap();
    assert_eq!(
        row.ended_at, None,
        "running bar must stay open (null = extends to now)"
    );
}

#[test]
fn todo_task_is_an_instant_not_an_invented_bar() {
    let t = task("t3", "todo");
    let row = timeline_row_from_task(&t, ts(0, 0), ts(23, 0), ts(23, 0)).unwrap();
    assert_eq!(row.ended_at.as_deref(), Some(row.started_at.as_str()));
}

#[test]
fn end_before_start_clamps_to_instant() {
    let mut t = task("t4", "done");
    t.claimed_at = Some(ts(5, 0).to_rfc3339());
    t.completed_at = Some(ts(4, 0).to_rfc3339()); // data noise: end < start
    let row = timeline_row_from_task(&t, ts(0, 0), ts(23, 0), ts(23, 0)).unwrap();
    assert_eq!(row.ended_at.as_deref(), Some(row.started_at.as_str()));
}

#[test]
fn window_filtering_keeps_running_bars_and_drops_outsiders() {
    let done_old = {
        let mut t = task("old", "done");
        t.completed_at = Some(ts(2, 30).to_rfc3339());
        t
    };
    let running = {
        let mut t = task("run", "in_progress");
        t.claimed_at = Some(ts(1, 0).to_rfc3339());
        t
    };
    // Window starts long after both tasks began.
    let from = ts(10, 0);
    let to = ts(23, 0);
    assert!(
        timeline_row_from_task(&done_old, from, to, to).is_none(),
        "closed bar entirely before the window must be dropped"
    );
    assert!(
        timeline_row_from_task(&running, from, to, to).is_some(),
        "running bar started before the window still overlaps it (extends to now)"
    );
}

#[test]
fn activity_instants_map_kinds_and_skip_task_created() {
    let from = ts(0, 0);
    let to = ts(23, 0);
    let deleg =
        timeline_row_from_activity(&activity("a1", "delegation_forwarded", ts(6, 0)), from, to)
            .unwrap();
    assert_eq!(deleg.kind, "delegation");
    assert_eq!(deleg.ended_at.as_deref(), Some(deleg.started_at.as_str()));
    let skill =
        timeline_row_from_activity(&activity("a2", "skill_activate", ts(6, 5)), from, to)
            .unwrap();
    assert_eq!(skill.kind, "skill");
    assert!(
        timeline_row_from_activity(&activity("a3", "task_created", ts(6, 10)), from, to)
            .is_none(),
        "task_created duplicates the task bar start edge"
    );
    assert!(
        timeline_row_from_activity(
            &activity("a4", "autopilot_triggered", ts(23, 30)),
            from,
            to
        )
        .is_none(),
        "outside window"
    );
}

#[test]
fn derive_merges_sorts_and_caps() {
    let tasks = vec![task("t1", "todo")];
    let acts = vec![activity("a1", "governance_violation", ts(0, 30))];
    let hbs = vec![
        ("bruno".to_string(), Some(ts(0, 10).to_rfc3339())),
        ("mia".to_string(), None), // never ran → no row
    ];
    let (rows, truncated) =
        derive_timeline_rows(&tasks, &acts, &hbs, ts(0, 0), ts(23, 0), ts(23, 0));
    assert!(!truncated);
    assert_eq!(rows.len(), 3);
    // Chronological: heartbeat 00:10 → governance 00:30 → task created 01:00.
    assert_eq!(rows[0].kind, "heartbeat");
    assert_eq!(rows[1].kind, "governance");
    assert_eq!(rows[2].kind, "task");
}

#[test]
fn cap_truncates_and_reports() {
    let acts: Vec<ActivityRow> = (0..(TIMELINE_ROW_CAP + 5))
        .map(|i| activity(&format!("a{i}"), "note", ts(1, 0)))
        .collect();
    let (rows, truncated) =
        derive_timeline_rows(&[], &acts, &[], ts(0, 0), ts(23, 0), ts(23, 0));
    assert!(truncated);
    assert_eq!(rows.len(), TIMELINE_ROW_CAP);
}

#[test]
fn capped_fetch_inside_window_still_reports_truncated() {
    // LOW (2026-07 review): exactly TIMELINE_ROW_CAP fetched activities,
    // all inside the window, oldest fetched (01:00) newer than `from`
    // (00:00) ⇒ activities between 00:00 and 01:00 were never fetched.
    // Derived rows == cap (not > cap), so the old `rows.len() > cap`
    // check reported false — the fetch-hit-cap signal must flag it.
    let acts: Vec<ActivityRow> = (0..TIMELINE_ROW_CAP)
        .map(|i| activity(&format!("a{i}"), "note", ts(1, 0)))
        .collect();
    let (rows, truncated) =
        derive_timeline_rows(&[], &acts, &[], ts(0, 0), ts(23, 0), ts(23, 0));
    assert_eq!(rows.len(), TIMELINE_ROW_CAP);
    assert!(
        truncated,
        "cap-full fetch newer than `from` hides older events"
    );
}

#[test]
fn uncapped_fetch_is_not_truncated() {
    // Fewer rows than the fetch cap ⇒ the window is fully covered even
    // though the oldest activity is newer than `from`.
    let acts: Vec<ActivityRow> = (0..3)
        .map(|i| activity(&format!("a{i}"), "note", ts(1, 0)))
        .collect();
    let (rows, truncated) =
        derive_timeline_rows(&[], &acts, &[], ts(0, 0), ts(23, 0), ts(23, 0));
    assert_eq!(rows.len(), 3);
    assert!(!truncated);
}
