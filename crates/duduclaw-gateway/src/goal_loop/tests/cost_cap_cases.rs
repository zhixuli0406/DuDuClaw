use super::*;
use crate::responsibility::cost::test_support::FixedCost;

#[tokio::test]
async fn goal_loop_tool_streak_team_member_pauses_without_a_judge() {
    let home = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(home.path()).await;
    store
        .insert_task(&goal_task("streak", "alice"))
        .await
        .unwrap();
    let d =
        driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(home.path().to_path_buf());
    d.tick_once().await.unwrap();
    store
        .update_task("streak", &serde_json::json!({"status":"in_progress"}))
        .await
        .unwrap();
    // Freeze a team spec after the Solo enqueue, avoiding a real team runtime.
    let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
    conn.execute("UPDATE tasks SET team_spec_json='{}' WHERE id='streak'", [])
        .unwrap();
    let role = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(), "task_id":"streak", "agent_id":"alice",
        "round":1, "role":"executor", "member_id":"eph-executor", "runtime":"codex",
        "provider":"codex", "observation_fidelity":"mcp_only", "outcome":"completed",
        "config_fingerprint_hard":"test"
    });
    std::fs::write(home.path().join("role_turns.jsonl"), format!("{role}\n")).unwrap();
    write_tool_calls_jsonl(home.path(), "eph-executor", "web_fetch", "same args", 8);
    store
        .complete_task("streak", "team output", "team-composer")
        .await
        .unwrap();
    drop(d);
    let engine = crate::dispatch_engine::DispatchEngine::new(store.clone(), None)
        .with_home_dir(home.path().to_path_buf());
    engine.tick_once().await.unwrap();
    let task = store.get_task("streak").await.unwrap().unwrap();
    assert_eq!(task.status, "needs_human");
    assert_eq!(task.pause_reason.as_deref(), Some("no_progress"));
    assert!(task.judge_feedback.unwrap().contains("web_fetch"));
}

#[tokio::test]
async fn goal_loop_tool_streak_precedes_fast_acceptance_and_survives_restart() {
    use crate::dispatch_engine::{AcceptanceJudge, AcceptanceVerdict, DispatchEngine};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Judge(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl AcceptanceJudge for Judge {
        async fn judge(&self, _: &str, _: &str, _: &str) -> Result<AcceptanceVerdict, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(AcceptanceVerdict {
                passed: true,
                feedback: "accepted".into(),
                aspects: None,
            })
        }
    }

    for restart in [false, true] {
        for (count, threshold, pauses) in [(7, 8, false), (8, 8, true), (8, 0, false)] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(
                home.path().join("config.toml"),
                format!(
                    "[goal_loop]\nresume_on_restart = 'auto'\ntool_streak_pause = {threshold}\n"
                ),
            )
            .unwrap();
            let (store, queue) = open_stores(home.path()).await;
            store
                .insert_task(&goal_task("streak", "alice"))
                .await
                .unwrap();
            let d = driver(store.clone(), queue.clone(), small_cfg())
                .with_home_dir(home.path().to_path_buf());
            d.tick_once().await.unwrap();
            store
                .atomic_claim(
                    "streak",
                    "alice",
                    &Utc::now().to_rfc3339(),
                    &(Utc::now() + ChronoDuration::minutes(5)).to_rfc3339(),
                )
                .await
                .unwrap();
            write_tool_calls_jsonl(home.path(), "alice", "web_fetch", "same args", count);
            store
                .complete_task("streak", "round output", "alice")
                .await
                .unwrap();
            if restart {
                drop(d);
                let restarted = driver(store.clone(), queue, small_cfg())
                    .with_home_dir(home.path().to_path_buf());
                restarted.tick_once().await.unwrap();
            }
            let calls = Arc::new(AtomicUsize::new(0));
            let engine = DispatchEngine::new(store.clone(), Some(Arc::new(Judge(calls.clone()))))
                .with_home_dir(home.path().to_path_buf());
            engine.tick_once().await.unwrap();
            let after = store.get_task("streak").await.unwrap().unwrap();
            assert_eq!(
                after.status,
                if pauses { "needs_human" } else { "done" },
                "restart={restart}, count={count}, threshold={threshold}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(!pauses));
            if pauses {
                assert_eq!(after.pause_reason.as_deref(), Some("no_progress"));
                let rounds = store.list_iterations("streak").await.unwrap();
                assert_eq!(rounds[0].pause_reason.as_deref(), Some("no_progress"));
                assert_eq!(rounds[0].verdict.as_deref(), Some("escalated"));
                assert!(rounds[0].judged_at.is_some());
            }
        }
    }
}

#[tokio::test]
async fn goal_cost_cap_counts_children_and_pauses_at_cap_then_resumes_after_raise() {
    let home = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(home.path()).await;
    let mut task = goal_task("capped", "alice");
    task.cost_cap_cents = Some(100);
    store.insert_task(&task).await.unwrap();
    let mut child = goal_task("child", "alice");
    child.goal_mode = false;
    child.parent_task_id = Some(task.id.clone());
    store.insert_task(&child).await.unwrap();
    let cost = Arc::new(FixedCost::default());
    cost.set("capped", 40);
    cost.set("child", 60);
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(home.path().to_path_buf())
        .with_cost_source(cost);
    d.tick_once().await.unwrap();
    let paused = store.get_task("capped").await.unwrap().unwrap();
    assert_eq!(paused.status, "needs_human");
    assert_eq!(paused.pause_reason.as_deref(), Some("budget_exhausted"));
    let detail = paused.judge_feedback.unwrap();
    assert!(detail.contains("100"), "{detail}");
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
    store
        .update_task("capped", &serde_json::json!({"cost_cap_cents": 200}))
        .await
        .unwrap();
    store
        .resolve_needs_human("capped", "retry", "raise cap")
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    assert_eq!(queue.pending_messages(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn goal_cost_cap_below_limit_dispatches_and_unreadable_cost_defers() {
    for fail in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(home.path()).await;
        let mut task = goal_task("capped", "alice");
        task.cost_cap_cents = Some(100);
        store.insert_task(&task).await.unwrap();
        let cost = Arc::new(FixedCost::default());
        cost.set("capped", 99);
        cost.set_fail(fail);
        let d = driver(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(home.path().to_path_buf())
            .with_cost_source(cost);
        d.tick_once().await.unwrap();
        assert_eq!(
            queue.pending_messages(10).await.unwrap().len(),
            usize::from(!fail)
        );
        assert_ne!(
            store.get_task("capped").await.unwrap().unwrap().status,
            "needs_human"
        );
    }
}

#[tokio::test]
async fn goal_default_cost_cap_applies_only_without_explicit_cap() {
    for explicit in [None, Some(200)] {
        let home = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(home.path()).await;
        let mut task = goal_task("capped", "alice");
        task.cost_cap_cents = explicit;
        store.insert_task(&task).await.unwrap();
        let cost = Arc::new(FixedCost::default());
        cost.set("capped", 100);
        let mut cfg = small_cfg();
        cfg.default_cost_cap_cents = 100;
        let d = driver(store.clone(), queue.clone(), cfg)
            .with_home_dir(home.path().to_path_buf())
            .with_cost_source(cost);
        d.tick_once().await.unwrap();
        assert_eq!(
            queue.pending_messages(10).await.unwrap().len(),
            usize::from(explicit.is_some())
        );
    }
}

#[tokio::test]
async fn round_tool_streak_below_threshold_hints_at_threshold_pauses_and_zero_disables() {
    for (count, threshold, advisory, pauses) in [
        (7, 8, true, false),
        (8, 8, true, true),
        (9, 0, true, false),
        (8, 8, false, true),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(home.path()).await;
        let task = claimed_review_task("streak", "alice");
        store.insert_task(&task).await.unwrap();
        write_tool_calls_jsonl(home.path(), "alice", "web_fetch", "same args", count);
        let mut cfg = small_cfg();
        cfg.tool_streak_pause = threshold;
        cfg.tool_streak_advisory = advisory;
        let d = driver(store.clone(), queue.clone(), cfg).with_home_dir(home.path().to_path_buf());
        d.inflight
            .lock()
            .await
            .insert(task.id.clone(), inflight_entry(1, Utc::now()));
        d.tick_once().await.unwrap();
        let after = store.get_task("streak").await.unwrap().unwrap();
        assert_eq!(
            after.status == "needs_human",
            pauses,
            "count={count}, threshold={threshold}"
        );
        if pauses {
            assert_eq!(after.pause_reason.as_deref(), Some("no_progress"));
            let detail = after.judge_feedback.unwrap();
            assert!(
                detail.contains("web_fetch") && detail.contains(&count.to_string()),
                "{detail}"
            );
        } else {
            assert!(
                GoalStateSnapshot::from_json(after.goal_state_json.as_deref())
                    .tool_streak_hint
                    .is_some()
            );
        }
        assert!(queue.pending_messages(10).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn tool_streak_pause_seals_round_and_retry_gets_fresh_evidence_window() {
    let home = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(home.path()).await;
    store
        .insert_task(&goal_task("streak", "alice"))
        .await
        .unwrap();
    let d =
        driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(home.path().to_path_buf());
    d.tick_once().await.unwrap();
    store
        .update_task("streak", &serde_json::json!({"status":"in_progress"}))
        .await
        .unwrap();
    write_tool_calls_jsonl(home.path(), "alice", "web_fetch", "same args", 8);
    store
        .complete_task("streak", "round one", "alice")
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    assert_eq!(
        pause_class_of(&store, "streak").await,
        crate::pause_reason::PauseReason::NoProgress
    );
    let rounds = store.list_iterations("streak").await.unwrap();
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].verdict.as_deref(), Some("escalated"));
    assert!(rounds[0].judged_at.is_some());
    store
        .resolve_needs_human("streak", "retry", "continue")
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    let rounds = store.list_iterations("streak").await.unwrap();
    assert_eq!(rounds.len(), 2);
    assert!(rounds[1].submitted_at.is_none());
    assert!(rounds[1].pause_reason.is_none());
    store
        .update_task("streak", &serde_json::json!({"status":"in_progress"}))
        .await
        .unwrap();
    store
        .complete_task("streak", "round two without tools", "alice")
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    assert_eq!(
        store.get_task("streak").await.unwrap().unwrap().status,
        "review"
    );
    assert_eq!(store.list_iterations("streak").await.unwrap().len(), 2);
}

#[tokio::test]
async fn tool_streak_fast_judge_rejection_is_checked_before_next_dispatch() {
    let home = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(home.path()).await;
    store
        .insert_task(&goal_task("streak", "alice"))
        .await
        .unwrap();
    for agent in ["alice", "bob"] {
        write_agent_toml(
            home.path(),
            agent,
            "[capabilities]\nautonomy_level = \"approver\"\n",
        );
    }
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(home.path().to_path_buf())
        .with_policy(Arc::new(crate::dispatch_policy::RoundRobin::new()));
    d.tick_once().await.unwrap();
    store
        .update_task("streak", &serde_json::json!({"status":"in_progress"}))
        .await
        .unwrap();
    write_tool_calls_jsonl(home.path(), "alice", "web_fetch", "same args", 8);
    store
        .complete_task("streak", "round one", "alice")
        .await
        .unwrap();
    store
        .reject_review("streak", "more work", 99)
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    assert_eq!(
        pause_class_of(&store, "streak").await,
        crate::pause_reason::PauseReason::NoProgress
    );
    assert_eq!(
        store.get_task("streak").await.unwrap().unwrap().assigned_to,
        "alice"
    );
    assert_eq!(
        queue.pending_messages(10).await.unwrap().len(),
        1,
        "only the first round was enqueued"
    );
}

#[tokio::test]
async fn malformed_cost_configuration_defers_even_with_no_explicit_cap() {
    let home = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(home.path()).await;
    store
        .insert_task(&goal_task("capped", "alice"))
        .await
        .unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[goal_loop]\ndefault_cost_cap_cents = 'bad'",
    )
    .unwrap();
    let d =
        driver(store.clone(), queue.clone(), small_cfg()).with_home_dir(home.path().to_path_buf());
    d.tick_once().await.unwrap();
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn goal_cost_cap_unmeasured_usage_pauses_visibly_instead_of_zero() {
    let home = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(home.path()).await;
    let mut task = goal_task("capped", "alice");
    task.cost_cap_cents = Some(100);
    store.insert_task(&task).await.unwrap();
    let cost = Arc::new(FixedCost::default());
    cost.set_unmeasured("capped");
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(home.path().to_path_buf())
        .with_cost_source(cost);
    d.tick_once().await.unwrap();
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
    let paused = store.get_task("capped").await.unwrap().unwrap();
    assert_eq!(paused.status, "needs_human");
    assert_eq!(paused.pause_reason.as_deref(), Some("blocked_needs_decision"));
    assert!(paused.judge_feedback.unwrap().contains("runtime 未回報"));
    store
        .update_task("capped", &serde_json::json!({"cost_cap_cents": 200}))
        .await
        .unwrap();
    store.resolve_needs_human("capped", "retry", "raise cap").await.unwrap();
    d.tick_once().await.unwrap();
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
    assert_eq!(
        pause_class_of(&store, "capped").await,
        crate::pause_reason::PauseReason::BlockedNeedsDecision,
    );
}

#[tokio::test]
async fn goal_loop_streak_store_error_skips_only_that_goal() {
    for review in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let (store, queue) = open_stores(home.path()).await;
        let broken = if review {
            claimed_review_task("broken", "alice")
        } else {
            goal_task("broken", "alice")
        };
        store.insert_task(&broken).await.unwrap();
        // Autonomous dispatch requires a readable, active employee profile.
        write_agent_toml(home.path(), "bob", "");
        store.insert_task(&goal_task("healthy", "bob")).await.unwrap();
        // A corrupt ledger row fails list_iterations for just this task,
        // while the board, queue and other goals remain readable.
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute(
            "INSERT INTO task_iterations(task_id,round,dispatched_at) VALUES ('broken','invalid','2026-10-28')",
            [],
        ).unwrap();
        assert!(store.list_iterations("broken").await.is_err());
        let d = driver(store.clone(), queue.clone(), small_cfg())
            .with_home_dir(home.path().to_path_buf());
        if review {
            d.inflight.lock().await.insert("broken".into(), inflight_entry(1, Utc::now()));
        }
        d.tick_once().await.unwrap();
        let messages = queue.pending_messages(10).await.unwrap();
        assert_eq!(messages.len(), 1, "review={review}");
        assert_eq!(messages[0].target, "bob");
        assert_eq!(store.get_task("broken").await.unwrap().unwrap().status, broken.status);
    }
}

#[tokio::test]
async fn goal_loop_review_streak_store_error_does_not_stall_other_reviews() {
    use crate::dispatch_engine::{AcceptanceJudge, AcceptanceVerdict, DispatchEngine};

    struct Judge;
    #[async_trait::async_trait]
    impl AcceptanceJudge for Judge {
        async fn judge(&self, _: &str, _: &str, _: &str) -> Result<AcceptanceVerdict, String> {
            panic!("a completed-round streak must pause before the judge runs");
        }
    }

    for with_judge in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let (store, _) = open_stores(home.path()).await;
        write_agent_toml(home.path(), "bob", "");
        for (id, agent) in [("broken", "alice"), ("healthy", "bob")] {
            store.insert_task(&claimed_review_task(id, agent)).await.unwrap();
        }
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute(
            "INSERT INTO task_iterations(task_id,round,dispatched_at) VALUES ('broken','invalid','2026-10-28')",
            [],
        ).unwrap();
        write_tool_calls_jsonl(home.path(), "bob", "web_fetch", "same args", 8);
        let judge = with_judge.then(|| Arc::new(Judge) as Arc<dyn AcceptanceJudge>);
        let engine = DispatchEngine::new(store.clone(), judge)
            .with_home_dir(home.path().to_path_buf());
        // Even an empty collector receipt distinguishes full evidence from
        // missing evidence. A deferred review must keep it for the retry.
        crate::prediction::task_observe::record_native_evidence("broken", 1, vec![]);
        engine.tick_once().await.unwrap();
        assert!(
            crate::prediction::task_observe::take_native_evidence("broken", 1).is_some(),
            "a deferred review must retain native evidence (with_judge={with_judge})",
        );
        assert_eq!(store.get_task("broken").await.unwrap().unwrap().status, "review");
        assert_eq!(
            pause_class_of(&store, "healthy").await,
            crate::pause_reason::PauseReason::NoProgress,
            "with_judge={with_judge}",
        );
    }
}

#[tokio::test]
async fn known_spend_at_cap_pauses_even_when_other_usage_is_unmeasured() {
    let home = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(home.path()).await;
    let mut task = goal_task("capped", "alice");
    task.cost_cap_cents = Some(100);
    store.insert_task(&task).await.unwrap();
    let cost = Arc::new(FixedCost::default());
    cost.by_episode.lock().unwrap().insert(
        "capped".into(),
        crate::responsibility::EpisodeCost {
            cost: 100,
            rows: 1,
            unmeasured: 1,
        },
    );
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(home.path().to_path_buf())
        .with_cost_source(cost);
    d.tick_once().await.unwrap();
    assert_eq!(
        pause_class_of(&store, "capped").await,
        crate::pause_reason::PauseReason::BudgetExhausted
    );
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
}
