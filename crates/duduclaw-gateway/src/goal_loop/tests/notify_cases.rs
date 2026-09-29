//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs` — notify cases.

use super::*;

#[tokio::test]
async fn goal_dispatch_freezes_while_a_human_holds_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store
        .insert_task(&goal_task_from_chat("g1", "12345"))
        .await
        .unwrap();
    begin_takeover(dir.path(), "telegram", "12345");

    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.tick_once().await.unwrap();
    assert!(
        queue.pending_messages(10).await.unwrap().is_empty(),
        "no dispatch into a conversation a human is running"
    );

    // Frozen, not escalated: parking it `needs_human` would page the very
    // person who is already handling it.
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(t.status, "todo");

    // Handback resumes the loop unchanged.
    duduclaw_core::takeover_state::end(dir.path(), "telegram:12345", chrono::Utc::now())
        .unwrap();
    d.tick_once().await.unwrap();
    assert_eq!(
        queue.pending_messages(10).await.unwrap().len(),
        1,
        "the goal is dispatched once the human hands back"
    );
}

#[tokio::test]
async fn goal_dispatch_takeover_is_scoped_to_the_held_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store
        .insert_task(&goal_task_from_chat("g-other", "99999"))
        .await
        .unwrap();
    store
        .insert_task(&goal_task("g-nosource", "alice"))
        .await
        .unwrap();
    begin_takeover(dir.path(), "telegram", "12345");

    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.tick_once().await.unwrap();

    let dispatched: Vec<String> = queue
        .pending_messages(10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.target)
        .collect();
    assert_eq!(
        dispatched.len(),
        2,
        "a takeover on one conversation must not freeze the whole board"
    );
}

#[tokio::test]
async fn iteration_cap_escalation_is_classified_budget_exhausted() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let cfg = GoalLoopConfig {
        iteration_cap: 2,
        stalled_secs: 0,
        ..small_cfg()
    };
    let d = driver(store.clone(), queue.clone(), cfg);

    d.tick_once().await.unwrap();
    d.tick_once().await.unwrap();
    d.tick_once().await.unwrap(); // cap reached ⇒ escalate

    assert_eq!(
        pause_class_of(&store, "g1").await,
        crate::pause_reason::PauseReason::BudgetExhausted
    );
}

#[tokio::test]
async fn both_deadline_flavours_are_classified_budget_exhausted() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // Global wall clock (created 48h ago, budget 24h).
    let mut wall = goal_task("g-wall", "alice");
    wall.created_at = (Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
    store.insert_task(&wall).await.unwrap();
    // Per-goal deadline that already lapsed, wall clock nowhere near.
    let mut task_dl = goal_task("g-dl", "alice");
    task_dl.deadline_at = Some((Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
    store.insert_task(&task_dl).await.unwrap();

    driver(store.clone(), queue.clone(), small_cfg())
        .tick_once()
        .await
        .unwrap();

    // Two different human-readable reasons, one class — a person triaging
    // 「次數或時限用盡」 does not care which clock ran out, only that one did.
    let wall_row = store.get_task("g-wall").await.unwrap().unwrap();
    assert_eq!(
        wall_row.judge_feedback.as_deref(),
        Some("goal-loop deadline")
    );
    let dl_row = store.get_task("g-dl").await.unwrap().unwrap();
    assert_eq!(dl_row.judge_feedback.as_deref(), Some("時限已到未通過驗收"));
    for id in ["g-wall", "g-dl"] {
        assert_eq!(
            pause_class_of(&store, id).await,
            crate::pause_reason::PauseReason::BudgetExhausted,
            "{id}"
        );
    }
}

#[tokio::test]
async fn oscillation_escalation_is_classified_no_progress() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let cfg = GoalLoopConfig {
        iteration_cap: 10,
        iteration_cap_simple: 10,
        ..small_cfg()
    };
    let mut t = goal_task("g1", "alice");
    t.max_retries = 100; // only the A2 guard may escalate here
    store.insert_task(&t).await.unwrap();
    let d = driver(store.clone(), queue.clone(), cfg);

    d.tick_once().await.unwrap();
    agent_round_then_reject(&d, &store, "g1", "same reason").await;
    d.tick_once().await.unwrap();
    agent_round_then_reject(&d, &store, "g1", "same reason").await;
    d.tick_once().await.unwrap();

    // Distinct from the cap escalations above: the loop still had budget,
    // it just stopped making progress.
    assert_eq!(
        pause_class_of(&store, "g1").await,
        crate::pause_reason::PauseReason::NoProgress
    );
}

#[tokio::test]
async fn dependency_failure_escalation_is_classified_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let mut g1 = goal_task("g1", "alice");
    g1.status = "failed".into();
    store.insert_task(&g1).await.unwrap();
    let mut g2 = goal_task("g2", "alice");
    g2.depends_on = r#"["g1"]"#.into();
    store.insert_task(&g2).await.unwrap();

    driver(store.clone(), queue.clone(), small_cfg())
        .tick_once()
        .await
        .unwrap();

    assert_eq!(
        pause_class_of(&store, "g2").await,
        crate::pause_reason::PauseReason::BlockedNeedsDecision
    );
}

#[tokio::test]
async fn restart_pause_is_classified_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    std::fs::write(
        dir.path().join("config.toml"),
        "[goal_loop]\nresume_on_restart = \"pause\"\n",
    )
    .unwrap();
    let mut t = goal_task("g1", "alice");
    t.status = "in_progress".into();
    store.insert_task(&t).await.unwrap();

    assert_eq!(
        pause_inflight_on_restart(store.clone(), queue, dir.path()).await,
        1
    );
    assert_eq!(
        pause_class_of(&store, "g1").await,
        crate::pause_reason::PauseReason::Restart
    );
}

/// The safety net: a task parked through a path that declares no class
/// (any future caller of the string-only `mark_needs_human`, plus every
/// row that predates the column) reads as `Unknown` = 「需要人工確認」,
/// never as a confident bucket.
#[tokio::test]
async fn unclassified_and_legacy_pauses_read_as_needs_human_review() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    store
        .mark_needs_human("g1", "something odd happened")
        .await
        .unwrap();
    assert_eq!(
        pause_class_of(&store, "g1").await,
        crate::pause_reason::PauseReason::Unknown
    );

    // A legacy row: parked with no class column value at all.
    let mut legacy = goal_task("g2", "alice");
    legacy.status = "needs_human".into();
    legacy.pause_reason = None;
    store.insert_task(&legacy).await.unwrap();
    assert_eq!(
        pause_class_of(&store, "g2").await,
        crate::pause_reason::PauseReason::Unknown
    );
}

// ── H22: no-progress timeout report ─────────────────────────────────

#[test]
fn no_progress_minutes_thresholds() {
    let now = Utc::now();
    let twelve_ago = now - chrono::Duration::minutes(12);

    // Disabled: 0 and any negative value never report, however long the
    // silence has been.
    assert_eq!(no_progress_minutes(twelve_ago, now, 0), None);
    assert_eq!(no_progress_minutes(twelve_ago, now, -5), None);

    // Below / at / above the threshold.
    assert_eq!(
        no_progress_minutes(now - chrono::Duration::minutes(9), now, 10),
        None
    );
    assert_eq!(
        no_progress_minutes(now - chrono::Duration::minutes(10), now, 10),
        Some(10)
    );
    assert_eq!(no_progress_minutes(twelve_ago, now, 10), Some(12));

    // A signal timestamped in the FUTURE (clock skew / hand-edited row)
    // degrades to silence rather than reporting a negative duration.
    assert_eq!(
        no_progress_minutes(now + chrono::Duration::hours(1), now, 10),
        None
    );
}

#[tokio::test]
async fn silent_task_gets_one_notice_and_is_deduped_within_the_round() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let mut t = goal_task("g1", "alice");
    t.status = "in_progress".into();
    store.insert_task(&t).await.unwrap();

    let cfg = GoalLoopConfig {
        progress_report_minutes: 10,
        ..small_cfg()
    };
    let d = driver(store.clone(), queue.clone(), cfg).with_home_dir(dir.path().to_path_buf());

    let now = Utc::now();
    let mut map: HashMap<String, InFlight> = HashMap::from([(
        "g1".to_string(),
        inflight_entry(3, now - chrono::Duration::minutes(45)),
    )]);

    d.maybe_report_no_progress(&mut map, &t, now).await;
    assert_eq!(progress_report_events(&store, "g1").await, 1);
    assert_eq!(
        map["g1"].progress_reported_round,
        Some(3),
        "the round must be marked reported so later ticks stay quiet"
    );
    let summary = store
        .list_activity_for_task("g1", 100)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.event_type == "goal_loop.progress_report")
        .unwrap()
        .summary;
    assert!(
        summary.contains("45 分鐘"),
        "elapsed minutes must be named: {summary}"
    );
    assert!(summary.contains("未回報進度"), "{summary}");

    // Ten hours later, still the same round: the dedup flag alone must
    // hold (the elapsed time is now enormous, so nothing else would).
    d.maybe_report_no_progress(&mut map, &t, now + chrono::Duration::hours(10))
        .await;
    assert_eq!(
        progress_report_events(&store, "g1").await,
        1,
        "at most one notice per round"
    );

    // A NEW round (a re-dispatch rebuilds the entry) reports again. The
    // round-4 dispatch is placed AFTER the round-3 notice's own activity
    // row, so that row cannot masquerade as this round's progress signal —
    // which is precisely the `> enqueued_at` floor being exercised.
    let after_first_notice = Utc::now();
    map.insert(
        "g1".to_string(),
        inflight_entry(4, after_first_notice + chrono::Duration::seconds(1)),
    );
    d.maybe_report_no_progress(
        &mut map,
        &t,
        after_first_notice + chrono::Duration::minutes(30),
    )
    .await;
    assert_eq!(progress_report_events(&store, "g1").await, 2);
}

#[tokio::test]
async fn progress_report_stays_silent_when_disabled_or_recently_active() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let mut t = goal_task("g1", "alice");
    t.status = "in_progress".into();
    store.insert_task(&t).await.unwrap();
    let now = Utc::now();

    // progress_report_minutes = 0 ⇒ off, even after 45 minutes of silence.
    let off = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    assert_eq!(off.config.progress_report_minutes, 0);
    let mut map: HashMap<String, InFlight> = HashMap::from([(
        "g1".to_string(),
        inflight_entry(1, now - chrono::Duration::minutes(45)),
    )]);
    off.maybe_report_no_progress(&mut map, &t, now).await;
    assert_eq!(
        progress_report_events(&store, "g1").await,
        0,
        "0 disables the notice"
    );
    assert!(map["g1"].progress_reported_round.is_none());

    // Enabled, but the agent posted to the feed 1 minute ago ⇒ that IS
    // progress; the long-ago dispatch time must not win.
    let on = driver(
        store.clone(),
        queue.clone(),
        GoalLoopConfig {
            progress_report_minutes: 10,
            ..small_cfg()
        },
    )
    .with_home_dir(dir.path().to_path_buf());
    store
        .append_activity(&crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "agent.progress".into(),
            agent_id: "alice".into(),
            task_id: Some("g1".into()),
            summary: "還在跑第三步".into(),
            timestamp: (now - chrono::Duration::minutes(1)).to_rfc3339(),
            metadata: None,
        })
        .await
        .unwrap();
    on.maybe_report_no_progress(&mut map, &t, now).await;
    assert_eq!(
        progress_report_events(&store, "g1").await,
        0,
        "a fresh activity row is a progress signal — stay quiet"
    );
}

/// End-to-end through `tick_once`, so the reconcile-loop wiring (not just
/// the helper) is covered: an `in_progress` tracked task that has gone
/// quiet produces the notice on a real tick.
#[tokio::test]
async fn tick_reports_no_progress_for_a_tracked_in_progress_task() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let mut t = goal_task("g1", "alice");
    t.status = "in_progress".into();
    store.insert_task(&t).await.unwrap();

    let cfg = GoalLoopConfig {
        progress_report_minutes: 10,
        ..small_cfg()
    };
    let d = driver(store.clone(), queue.clone(), cfg).with_home_dir(dir.path().to_path_buf());
    d.inflight.lock().await.insert(
        "g1".to_string(),
        inflight_entry(1, Utc::now() - chrono::Duration::minutes(30)),
    );

    d.tick_once().await.unwrap();
    assert_eq!(progress_report_events(&store, "g1").await, 1);
    // Purely a report: the task keeps running, untouched.
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "in_progress", "the notice must never intervene");
    assert!(got.pause_reason.is_none());
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
}
