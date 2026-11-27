//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs` — notify cases.

use super::*;

#[tokio::test]
async fn observer_autoclose_rechecks_assignee_after_refusal_token_is_lost() {
    // An unregistered id models a persisted token after restart or expiry,
    // without mutating the process registry used by parallel tests.
    let stale = "ENTRY_REFUSED:model_provider_mismatch:00000000000000000000000000000000: employee configuration refused";
    assert_eq!(crate::entry_gate::background_refusal_code(stale), None);
    for (model, feedback, expected_status) in [
        ("preferred='gpt-5.4'", stale, "needs_human"),
        ("preferred=42", stale, "needs_human"),
        ("preferred='claude-sonnet-4-6'", stale, "cancelled"),
        ("preferred='claude-sonnet-4-6'", "ordinary non-entry pause", "cancelled"),
    ] {
        let home = home_with_agent("worker", &format!(
            "[capabilities]\nautonomy_level='observer'\n[runtime]\nprovider='claude'\n[model]\n{model}\n"
        ));
        assert_eq!(AutonomyLevel::for_agent(home.path(), "worker"), AutonomyLevel::Observer);
        assert_eq!(crate::entry_gate::check_task_assignee(home.path(), "worker").is_ok(),
            expected_status == "cancelled");
        let (store, queue) = open_stores(home.path()).await;
        let mut task = goal_task("observer-parked", "worker");
        task.status = "needs_human".into();
        task.pause_reason = Some("blocked_needs_decision".into());
        task.judge_feedback = Some(feedback.into());
        store.insert_task(&task).await.unwrap();

        let d = driver(store.clone(), queue, small_cfg()).with_home_dir(home.path().to_path_buf());
        d.reconcile_needs_human().await;
        let actual = store.get_task(&task.id).await.unwrap().unwrap();
        assert_eq!(actual.status, expected_status, "{model}: {feedback}");
        if expected_status == "needs_human" {
            assert_eq!(actual.judge_feedback.as_deref(), Some(stale));
            // Repair the effective artifact, then simulate a fresh gateway life.
            write_agent_toml(home.path(), "worker",
                "[capabilities]\nautonomy_level='observer'\n[runtime]\nprovider='claude'\n[model]\npreferred='claude-sonnet-4-6'\n");
            let queue = Arc::new(MessageQueue::open(home.path()).unwrap());
            let restarted = driver(store.clone(), queue, small_cfg())
                .with_home_dir(home.path().to_path_buf());
            restarted.reconcile_needs_human().await;
            assert_eq!(store.get_task(&task.id).await.unwrap().unwrap().status, "cancelled");
        }
    }
}

#[tokio::test]
async fn persisted_queue_refusal_rechecks_current_assignee() {
    let stale = "ENTRY_REFUSED:model_provider_mismatch:00000000000000000000000000000000: employee configuration refused";
    assert_eq!(crate::entry_gate::background_refusal_code(stale), None);
    for (model, error, should_park) in [
        ("gpt-5.4", stale, true),
        ("claude-sonnet-4-6", stale, false),
        ("claude-sonnet-4-6", "ordinary provider failure", false),
    ] {
        let home = home_with_agent("worker", &format!(
            "[runtime]\nprovider='claude'\n[model]\npreferred='{model}'\n"
        ));
        let (store, queue) = open_stores(home.path()).await;
        let task = goal_task("queue-refusal", "worker");
        store.insert_task(&task).await.unwrap();
        let message_id = crate::goal_loop::enqueue::enqueue_goal_work(
            &queue, &store, &task, 1, "").await.unwrap();
        queue.fail(&message_id, error).await.unwrap();
        let d = driver(store.clone(), queue, small_cfg()).with_home_dir(home.path().to_path_buf());
        let mut entry = inflight_entry(1, Utc::now());
        entry.awaiting_pickup = true;
        entry.message_id = Some(message_id);
        let mut inflight = HashMap::from([(task.id.clone(), entry)]);
        let persisted = d.dispatch_failure_for(&inflight, &task.id).await.unwrap();
        assert_eq!(persisted, error);
        assert_eq!(d.on_dispatch_failed(&mut inflight, &task.id, &persisted).await.unwrap(), should_park);
        assert!(inflight.is_empty());
        assert_eq!(store.get_task(&task.id).await.unwrap().unwrap().status,
            if should_park { "needs_human" } else { "todo" });
        assert_eq!(d.dispatch_failures.lock().await.contains_key(&task.id), !should_park);
    }
}

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

// ── RM-192 B6: no re-notify after a restart ─────────────────────────

/// Pushes the reconciler attempted for `id`: every attempt leaves one
/// `goal_loop.needs_human_notified` feed row (NoTarget here — the test home
/// has no destination, so nothing leaves the process).
async fn needs_human_attempts(store: &TaskStore, id: &str) -> usize {
    store
        .list_activity_for_task(id, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.event_type == "goal_loop.needs_human_notified")
        .count()
}

async fn park(store: &TaskStore, id: &str) {
    assert!(store.mark_needs_human(id, "stuck").await.unwrap());
}

#[tokio::test]
async fn a_restart_does_not_renotify_a_pending_task_but_a_reentered_one_is() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    for id in ["still", "again", "away"] {
        store.insert_task(&goal_task(id, "alice")).await.unwrap();
        park(&store, id).await;
    }

    // First driver life: the cards of all three were delivered (simulated
    // through the same marker the reconciler writes after a Sent push —
    // the test home has no channel to really send to).
    let first = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    for id in ["still", "again", "away"] {
        let task = store.get_task(id).await.unwrap().unwrap();
        let ep = first.needs_human_episode(id).await.unwrap();
        assert_eq!((ep.seq, ep.notified), (1, false));
        assert!(first.record_needs_human_notified(&task, ep.seq, "已推播需人工審批").await);
        assert!(first.needs_human_episode(id).await.unwrap().notified);
    }
    // "away" is decided while no driver runs (before the next life's
    // first pass), then parks again later.
    assert!(store.resolve_needs_human("away", "retry", "").await.unwrap());
    drop(first);

    // Restart: a new driver over the same store.
    let second = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    second.reconcile_needs_human().await;
    assert_eq!(needs_human_attempts(&store, "still").await, 1, "still pending: not re-notified");
    assert_eq!(needs_human_attempts(&store, "again").await, 1, "still pending: not re-notified");
    // The sweep ended "away"'s episode; re-entering starts a new one.
    park(&store, "away").await;
    second.reconcile_needs_human().await;
    assert_eq!(needs_human_attempts(&store, "away").await, 2, "re-entered: notified again");
    assert_eq!(second.needs_human_episode("away").await.unwrap().seq, 2);

    // Within this life: "again" leaves and re-enters; the exit is observed.
    assert!(store.resolve_needs_human("again", "retry", "").await.unwrap());
    second.reconcile_needs_human().await;
    park(&store, "again").await;
    second.reconcile_needs_human().await;
    assert_eq!(needs_human_attempts(&store, "again").await, 2, "re-entered: notified again");
    // A take-over press keeps the task needs_human: same episode, no push.
    assert!(store.claim_needs_human("still", "channel:googlechat:users/1").await.unwrap());
    let third = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    third.reconcile_needs_human().await;
    assert_eq!(needs_human_attempts(&store, "still").await, 1);
}

/// Finding 3 (accepted limitation): a marker forged straight into tasks.db
/// — even one "ahead" of the task's real state, with no matching entry the
/// driver observed and no prior "cleared" marker — is only ever interpreted
/// for the task it names. A row whose id names one task but whose `task_id`
/// column is another is a marker of neither.
#[tokio::test]
async fn forged_markers_only_affect_the_task_they_name() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    for id in ["victim", "other", "victim:9"] {
        store.insert_task(&goal_task(id, "alice")).await.unwrap();
        park(&store, id).await;
    }
    // Not waiting for a human: the restart sweep's candidate set.
    store.insert_task(&goal_task("bystander", "alice")).await.unwrap();
    let forge = |id: &str, task: &str| crate::task_store::ActivityRow {
        id: id.into(),
        event_type: "forged".into(),
        agent_id: "alice".into(),
        task_id: Some(task.into()),
        summary: "forged".into(),
        timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        metadata: None,
    };
    for (id, task) in [
        // Id names "victim", column says "other" (and the reverse).
        ("goal_needs_human_notified:victim:7", "other"),
        ("goal_needs_human_mirrored:other:5", "victim"),
        // Id names "victim", column says "bystander" (not needs_human).
        ("goal_needs_human_notified:victim:3", "bystander"),
        ("goal_needs_human_cleared:victim:30", "bystander"),
        // The task "victim:9"'s marker is not "victim"'s episode 9.
        ("goal_needs_human_notified:victim:9:1", "victim:9"),
        // A task-id prefix ending in a colon is not a complete marker suffix.
        ("goal_needs_human_notified:victim:9:2", "victim"),
        ("goal_needs_human_cleared:victim:9:1", "victim"),
        ("goal_needs_human_notified:victim:1x", "victim"),
        ("goal_needs_human_notified:victim:", "victim"),
        // A valid victim marker uses a distinct primary key from the forged bystander row.
        ("goal_needs_human_notified:victim:4", "victim"),
        // A well-formed "notified" marker far ahead of victim's real state
        // (no entry observed, no cleared episode 41).
        ("goal_needs_human_notified:victim:42", "victim"),
    ] {
        store.append_activity(&forge(id, task)).await.unwrap();
    }

    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    let victim = d.needs_human_episode("victim").await.unwrap();
    assert_eq!((victim.seq, victim.notified, victim.mirrored), (42, true, false));
    let other = d.needs_human_episode("other").await.unwrap();
    assert_eq!((other.seq, other.notified, other.mirrored), (1, false, false));
    let nine = d.needs_human_episode("victim:9").await.unwrap();
    assert_eq!((nine.seq, nine.notified), (1, true), "its own episode 1, nothing else");
    let bystander = d.needs_human_episode("bystander").await.unwrap();
    assert_eq!((bystander.seq, bystander.notified, bystander.mirrored), (1, false, false));

    // The sweep never picks a task through a row that names another task.
    let open = store
        .tasks_with_open_markers(
            ["goal_needs_human_notified:", "goal_needs_human_mirrored:"],
            "goal_needs_human_cleared:",
        )
        .await
        .unwrap();
    assert!(open.is_empty(), "{open:?}");

    // One pass: the forged "ahead" marker suppresses victim's card (the
    // accepted limitation) and nothing else; the other parked task is
    // notified as usual, and no task gains a "cleared" marker.
    let mut cleared_before = Vec::new();
    for task in ["victim", "other", "victim:9", "bystander"] {
        let mut ids = store.activity_ids_with_prefix(task, &format!("goal_needs_human_cleared:{task}:"))
            .await.unwrap();
        ids.sort();
        cleared_before.push((task, ids));
    }
    d.reconcile_needs_human().await;
    assert_eq!(needs_human_attempts(&store, "victim").await, 0);
    assert_eq!(needs_human_attempts(&store, "other").await, 1);
    for (task, before) in cleared_before {
        let mut cleared = store
            .activity_ids_with_prefix(task, &format!("goal_needs_human_cleared:{task}:"))
            .await
            .unwrap();
        cleared.sort();
        // The forged rows above remain; reconciliation must add no cleared marker.
        assert_eq!(cleared, before, "{task}: no new cleared markers");
    }
    assert_eq!(d.needs_human_episode("other").await.unwrap().seq, 1);
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
