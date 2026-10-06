//! Routine-specific occurrence and durable cursor invariants.
use super::*;

fn instant(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn routine(expression: &str, timezone: &str) -> CronTaskRow {
    let mut row = CronTaskRow::new(
        "routine".into(),
        "Routine".into(),
        "worker".into(),
        expression.into(),
        "legacy prompt must not run".into(),
    );
    row.cron_timezone = Some(timezone.into());
    row
}

#[test]
fn routine_slot_taipei_preserves_seconds_and_canonical_instant() {
    let row = routine("17 0 9 * * Mon *", "Asia/Taipei");
    let slot = latest_routine_slot(
        &row,
        instant("2026-10-05T00:59:00Z").timestamp(),
        instant("2026-10-05T01:00:45Z"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(slot, instant("2026-10-05T01:00:17Z"));
    assert!(routine_slot_matches(&row.cron, "Asia/Taipei", "2026-10-05T09:00:17+08:00").unwrap());
    assert!(!routine_slot_matches(&row.cron, "Asia/Taipei", "2026-10-05T01:00:00Z").unwrap());
    assert!(!routine_slot_matches(&row.cron, "Asia/Taipei", "2026-10-05T01:00:17.001Z").unwrap());
}

#[test]
fn routine_slot_dst_fold_has_two_distinct_utc_occurrences() {
    let row = routine("0 30 1 * * * *", "America/New_York");
    let first = latest_routine_slot(
        &row,
        instant("2026-11-01T05:00:00Z").timestamp(),
        instant("2026-11-01T05:45:00Z"),
    )
    .unwrap()
    .unwrap();
    let second = latest_routine_slot(&row, first.timestamp(), instant("2026-11-01T06:45:00Z"))
        .unwrap()
        .unwrap();
    assert_eq!(first, instant("2026-11-01T05:30:00Z"));
    assert_eq!(second, instant("2026-11-01T06:30:00Z"));
    assert_ne!(first, second);
}

#[test]
fn routine_slot_dst_gap_does_not_invent_local_time() {
    let row = routine("0 30 2 * * * *", "America/New_York");
    assert!(
        latest_routine_slot(
            &row,
            instant("2026-03-08T05:00:00Z").timestamp(),
            instant("2026-03-08T10:00:00Z")
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn routine_slot_missed_occurrences_coalesce_and_cursor_dedupes() {
    let row = routine("0 * * * * * *", "Asia/Taipei");
    let now = instant("2026-10-05T01:00:45Z");
    let latest = latest_routine_slot(&row, instant("2026-10-01T00:00:00Z").timestamp(), now)
        .unwrap()
        .unwrap();
    assert_eq!(latest, instant("2026-10-05T01:00:00Z"));
    assert!(
        latest_routine_slot(&row, latest.timestamp(), now)
            .unwrap()
            .is_none()
    );
    assert!(
        latest_routine_slot(&row, now.timestamp() + 60, now)
            .unwrap()
            .is_none()
    );
}

#[test]
fn routine_slot_invalid_timezone_fails_closed_while_legacy_keeps_fallback() {
    let row = routine("0 * * * * * *", "not/a/timezone");
    assert!(latest_routine_slot(&row, 0, instant("2026-10-05T01:00:45Z")).is_err());
    assert!(resolve_task_cron_tz(&row).is_none());
}

#[tokio::test]
async fn routine_cursor_survives_restart_and_two_store_competition() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = routine("0 * * * * * *", "Asia/Taipei");
    row.enabled = false;
    store.insert(&row).await.unwrap();
    store
        .bind_workflow(&row.id, "activation", "fixed-material")
        .await
        .unwrap();
    let initial = store
        .routine_cursor(&row.id, "fixed-material")
        .await
        .unwrap();
    let other = Arc::new(CronStore::open(home.path()).unwrap());
    let (first, second) = tokio::join!(
        store.advance_routine_cursor(&row.id, "fixed-material", initial + 120),
        other.advance_routine_cursor(&row.id, "fixed-material", initial + 60),
    );
    first.unwrap();
    second.unwrap();
    drop(store);
    drop(other);
    let reopened = CronStore::open(home.path()).unwrap();
    assert_eq!(
        reopened
            .routine_cursor(&row.id, "fixed-material")
            .await
            .unwrap(),
        initial + 120
    );
    assert!(
        reopened
            .bind_workflow(&row.id, "another-activation", "other-material")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn routine_disabled_manual_refuses_before_service_or_legacy_invocation() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = routine("0 * * * * * *", "Asia/Taipei");
    row.enabled = false;
    store.insert(&row).await.unwrap();
    store
        .bind_workflow(&row.id, "activation", "material")
        .await
        .unwrap();
    let result = enqueue_routine_trigger(
        home.path(),
        &store,
        &row.id,
        crate::workflow::Trigger::Manual {
            request_id: "manual-request".into(),
        },
        None,
    )
    .await;
    assert_eq!(result.unwrap_err(), "workflow cron disabled");
    assert_eq!(store.get(&row.id).await.unwrap().unwrap().run_count, 0);
}

// ── F1a E-M5 / E-M6: enable starts from now, given-up slots are recorded,
// the latest-slot search walks the schedule instead of every second. ──

#[test]
fn routine_slot_search_is_bounded_for_a_per_second_schedule() {
    let row = routine("* * * * * * *", "UTC");
    let now = instant("2026-10-05T01:00:45Z");
    let started = std::time::Instant::now();
    let slot = latest_routine_slot(&row, (now - chrono::Duration::hours(30)).timestamp(), now)
        .unwrap()
        .unwrap();
    assert_eq!(slot, now);
    // Old implementation: 86,400 includes() calls per tick.
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
}

#[test]
fn routine_slots_beyond_catch_up_window_are_counted_once() {
    let row = routine("0 0 9 * * * *", "Asia/Taipei");
    let now = instant("2026-10-05T03:00:00Z"); // 11:00 Taipei
    let cursor = instant("2026-10-01T00:00:00Z").timestamp(); // 08:00 Taipei
    let (from, to, slots) = routine_skipped_slots(&row, cursor, now).unwrap().unwrap();
    assert_eq!(from, cursor);
    assert_eq!(to, instant("2026-10-04T03:00:00Z").timestamp());
    // 09:00 Taipei on Oct 1-4 (Oct 4 09:00 is 01:00Z, before the window
    // start at 03:00Z); only Oct 5 09:00 is inside the window.
    assert_eq!(slots, 4);
    assert_eq!(
        latest_routine_slot(&row, to, now).unwrap(),
        Some(instant("2026-10-05T01:00:00Z"))
    );
    // Cursor inside the window: nothing given up.
    assert!(routine_skipped_slots(&row, to, now).unwrap().is_none());
}

#[tokio::test]
async fn enabling_a_routine_moves_its_cursor_to_now_and_skip_is_recorded() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = routine("0 0 9 * * * *", "Asia/Taipei");
    row.enabled = false;
    store.insert(&row).await.unwrap();
    store.bind_workflow(&row.id, "activation", "material").await.unwrap();
    // Pretend the routine was bound long ago and disabled since.
    rusqlite::Connection::open(home.path().join("cron_tasks.db"))
        .unwrap()
        .execute("UPDATE cron_routine_cursors SET cursor_unix=100", [])
        .unwrap();
    assert_eq!(store.routine_cursor(&row.id, "material").await.unwrap(), 100);
    let before = Utc::now().timestamp();
    store.set_enabled(&row.id, true).await.unwrap();
    assert!(store.routine_cursor(&row.id, "material").await.unwrap() >= before);
    // Disable/enable again never moves it backwards.
    store.set_enabled(&row.id, false).await.unwrap();
    store.advance_routine_cursor(&row.id, "material", before + 3600).await.unwrap();
    store.set_enabled(&row.id, true).await.unwrap();
    assert_eq!(store.routine_cursor(&row.id, "material").await.unwrap(), before + 3600);

    store
        .record_routine_skip(&row.id, "material", 100, before + 7200, 3)
        .await
        .unwrap();
    assert_eq!(store.routine_cursor(&row.id, "material").await.unwrap(), before + 7200);
    let stored = store.get(&row.id).await.unwrap().unwrap();
    assert_eq!(stored.last_status.as_deref(), Some("skipped"));
    assert!(stored.last_error.unwrap().starts_with("workflow_routine_slots_skipped: 3 "));
    assert_eq!(stored.failure_count, 0);
}

// ── F1b E-H4: a workflow routine's manual run needs the caller's request id. ──

#[tokio::test]
async fn bound_routine_manual_run_without_request_id_is_refused_everywhere() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(CronStore::open(home.path()).unwrap());
    let mut row = routine("0 0 9 * * * *", "Asia/Taipei");
    row.enabled = false;
    store.insert(&row).await.unwrap();
    store
        .bind_workflow(&row.id, "activation", "material")
        .await
        .unwrap();
    // The MCP `run_cron_task` path (standalone runner) never invents an id.
    let err = run_cron_task_now_standalone(home.path(), &row.id)
        .await
        .unwrap_err();
    assert_eq!(err, WORKFLOW_MANUAL_REQUEST_ID_REQUIRED);
    // Nor does a scheduled-dispatch call without a slot.
    let registry = Arc::new(RwLock::new(AgentRegistry::new(home.path().join("agents"))));
    dispatch_cron_task_with_slot(home.path(), &store, &registry, &row, &RealAgentInvoker, None)
        .await;
    let after = store.get(&row.id).await.unwrap().unwrap();
    assert_eq!(after.run_count, 1);
    assert_eq!(after.failure_count, 1);
    assert_eq!(
        after.last_error.as_deref(),
        Some(WORKFLOW_MANUAL_REQUEST_ID_REQUIRED)
    );
}
