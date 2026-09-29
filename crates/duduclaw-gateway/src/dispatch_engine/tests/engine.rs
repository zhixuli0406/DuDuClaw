use super::*;

    #[tokio::test]
    async fn review_prompt_includes_tool_activity_when_audit_present() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g5").await; // claimed_by="w", claimed_at="2026-07-11T10:00:00Z"

        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":true}\n",
                "{\"timestamp\":\"2026-07-11T10:03:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":false}\n",
                "{\"timestamp\":\"2026-07-11T10:02:30Z\",\"agent_id\":\"other\",\"tool_name\":\"Bash\",\"success\":true}\n",
            ),
        )
        .unwrap();

        let judge = Arc::new(CapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
            captured_task: std::sync::Mutex::new(None),
        });
        let engine = DispatchEngine::new(
            store.clone(),
            Some(judge.clone() as Arc<dyn AcceptanceJudge>),
        )
        .with_home_dir(dir.path().to_path_buf());
        engine.tick_once().await.unwrap();

        let captured = judge.captured_task.lock().unwrap().clone().unwrap();
        assert!(captured.contains("<tool_activity>"), "{captured}");
        assert!(
            captured.contains("memory_search: 1 ok, 1 err"),
            "{captured}"
        );
        assert!(!captured.contains("Bash"), "{captured}");
    }

    #[tokio::test]
    async fn review_prompt_omits_tool_activity_without_home_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g6").await;
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":true}\n",
        )
        .unwrap();

        let judge = Arc::new(CapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
            captured_task: std::sync::Mutex::new(None),
        });
        // No `.with_home_dir(...)` — behavior must match pre-WP4 (no block).
        let engine = DispatchEngine::new(
            store.clone(),
            Some(judge.clone() as Arc<dyn AcceptanceJudge>),
        );
        engine.tick_once().await.unwrap();

        let captured = judge.captured_task.lock().unwrap().clone().unwrap();
        assert!(!captured.contains("<tool_activity>"), "{captured}");
    }

    // ── G2 per-goal risk boundary (design-market-belief-loop-2026-08.md §6,
    // sister package, 2026-08-14) ───────────────────────────────

    /// A task with no explicit `risk_boundary` gets the built-in baseline
    /// text folded into the judge's task block (no `config.toml
    /// [goal_defaults]` present in the temp home dir, so `baseline_boundary`
    /// fails open to `DEFAULT_BASELINE_BOUNDARY`) — never silently omitted.
    #[tokio::test]
    async fn review_prompt_includes_baseline_risk_boundary_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g7").await;

        let judge = Arc::new(CapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
            captured_task: std::sync::Mutex::new(None),
        });
        let engine = DispatchEngine::new(
            store.clone(),
            Some(judge.clone() as Arc<dyn AcceptanceJudge>),
        )
        .with_home_dir(dir.path().to_path_buf());
        engine.tick_once().await.unwrap();

        let captured = judge.captured_task.lock().unwrap().clone().unwrap();
        assert!(captured.contains("<risk_boundary>"), "{captured}");
        assert!(captured.contains("遵循當地法規"), "{captured}");
    }

    /// An explicit per-task `risk_boundary` overrides the baseline text in
    /// the judge's task block.
    #[tokio::test]
    async fn review_prompt_includes_explicit_task_risk_boundary_override() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let mut g = pending_goal("g8");
        g.risk_boundary = Some("不得動用生產資料庫寫入權限".to_string());
        store.insert_task(&g).await.unwrap();
        store
            .atomic_claim("g8", "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap()
            .is_claimed();
        store.complete_task("g8", "my result", "w").await.unwrap();

        let judge = Arc::new(CapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
            captured_task: std::sync::Mutex::new(None),
        });
        let engine = DispatchEngine::new(
            store.clone(),
            Some(judge.clone() as Arc<dyn AcceptanceJudge>),
        )
        .with_home_dir(dir.path().to_path_buf());
        engine.tick_once().await.unwrap();

        let captured = judge.captured_task.lock().unwrap().clone().unwrap();
        assert!(
            captured.contains("不得動用生產資料庫寫入權限"),
            "{captured}"
        );
        assert!(
            !captured.contains("遵循當地法規"),
            "explicit risk_boundary replaces, not appends to, the baseline: {captured}"
        );
    }

    /// Fail-open: with no `home_dir` wired at all (a handful of legacy
    /// construction paths), the risk boundary still injects the built-in
    /// default rather than being skipped.
    #[tokio::test]
    async fn review_prompt_includes_risk_boundary_without_home_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g9").await;

        let judge = Arc::new(CapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
            captured_task: std::sync::Mutex::new(None),
        });
        // No `.with_home_dir(...)`.
        let engine = DispatchEngine::new(
            store.clone(),
            Some(judge.clone() as Arc<dyn AcceptanceJudge>),
        );
        engine.tick_once().await.unwrap();

        let captured = judge.captured_task.lock().unwrap().clone().unwrap();
        assert!(captured.contains("<risk_boundary>"), "{captured}");
        assert!(captured.contains("遵循當地法規"), "{captured}");
    }

    // WP3 (PORTICO): a task reaching a terminal review phase (accept) revokes
    // every capability grant bound to it. Requires a wired home_dir.
    #[tokio::test]
    async fn task_completion_revokes_grants() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g7").await; // claimed_by = "w"

        // Mint a grant bound to this task for agent "w".
        let grants = crate::capability_grants::CapabilityGrantStore::open(dir.path()).unwrap();
        grants
            .grant("w", Some("g7"), "send_message", "capability_request", 3600)
            .await
            .unwrap();
        assert!(grants.has_active_grant("w", "send_message").await);

        let judge = Arc::new(StubJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
        });
        let engine =
            DispatchEngine::new(store.clone(), Some(judge)).with_home_dir(dir.path().to_path_buf());
        engine.tick_once().await.unwrap();

        assert_eq!(store.get_task("g7").await.unwrap().unwrap().status, "done");
        // The task-scoped grant is revoked once its phase closed.
        assert!(
            !grants.has_active_grant("w", "send_message").await,
            "task completion must revoke its capability grants"
        );
    }

    #[tokio::test]
    async fn tick_reclaims_zombies() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let mut t = TaskRow::new(
            "z".into(),
            "z".into(),
            String::new(),
            "medium".into(),
            String::new(),
            "system".into(),
        );
        t.status = "pending".into();
        store.insert_task(&t).await.unwrap();
        // Claim with an already-past lease (and long-elapsed grace window)
        // ⇒ zombie on next tick. Dated well in the past so the test is not
        // sensitive to the wall clock.
        store
            .atomic_claim("z", "w", "2026-07-01T08:00:00Z", "2026-07-01T08:05:00Z")
            .await
            .unwrap()
            .is_claimed();

        let engine = DispatchEngine::new(store.clone(), None);
        engine.tick_once().await.unwrap();
        // Default max_retries = 3, retry 0 ⇒ requeued to pending.
        let z = store.get_task("z").await.unwrap().unwrap();
        assert_eq!(z.status, "pending");
        assert_eq!(z.retry_count, 1);
    }

    // ── G1 lease renewal e2e ────────────────────────────────

    /// A worker held past multiple lease windows with a live renewal ticker is
    /// NEVER reclaimed; the same claim without a ticker (abandoned) is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn renewal_ticker_prevents_reclaim_across_lease_windows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let mut t = TaskRow::new(
            "long".into(),
            "long-running".into(),
            String::new(),
            "medium".into(),
            String::new(),
            "system".into(),
        );
        t.status = "pending".into();
        store.insert_task(&t).await.unwrap();

        // 1-second lease; the guard ticks every ~333ms.
        let lease_secs: i64 = 1;
        let now = Utc::now();
        let lease = (now + chrono::Duration::seconds(lease_secs)).to_rfc3339();
        assert!(
            store
                .atomic_claim("long", "w", &now.to_rfc3339(), &lease)
                .await
                .unwrap()
                .is_claimed()
        );
        let guard = LeaseRenewalGuard::spawn(store.clone(), "long".into(), "w".into(), lease_secs);

        let engine = DispatchEngine::new(store.clone(), None).with_lease_secs(lease_secs);
        // Hold the task for >2 full lease windows, reclaiming on every pass.
        for _ in 0..5 {
            time::sleep(Duration::from_millis(500)).await;
            engine.tick_once().await.unwrap();
            let t = store.get_task("long").await.unwrap().unwrap();
            assert_eq!(
                t.status, "in_progress",
                "renewed task must never be reclaimed while its ticker runs"
            );
            assert_eq!(t.claimed_by.as_deref(), Some("w"));
        }
        drop(guard);
    }

    #[tokio::test]
    async fn abandoned_claim_is_reclaimed_after_expiry_plus_grace() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let mut t = TaskRow::new(
            "gone".into(),
            "abandoned".into(),
            String::new(),
            "medium".into(),
            String::new(),
            "system".into(),
        );
        t.status = "pending".into();
        store.insert_task(&t).await.unwrap();

        // Claimed with a 5-minute lease, then the worker vanishes (no ticker,
        // no tasks_renew). All timestamps crafted — deterministic.
        assert!(
            store
                .atomic_claim("gone", "w", "2026-07-01T10:00:00Z", "2026-07-01T10:05:00Z")
                .await
                .unwrap()
                .is_claimed()
        );

        // At expiry (10:05) and inside the grace window (< 10:10): NOT yet
        // reclaimed — conservative reclaim waits one further full window.
        let out = store.reclaim_zombies("2026-07-01T10:06:00Z").await.unwrap();
        assert!(out.is_empty(), "still inside the grace window");
        assert_eq!(
            store.get_task("gone").await.unwrap().unwrap().status,
            "in_progress"
        );

        // After expiry + one full lease window with zero renewals: reclaimed.
        let out2 = store.reclaim_zombies("2026-07-01T10:10:00Z").await.unwrap();
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].task_id, "gone");
        let z = store.get_task("gone").await.unwrap().unwrap();
        assert_eq!(z.status, "pending");
        assert_eq!(z.retry_count, 1);
        assert!(z.claimed_by.is_none());
    }

    // ── H2: MAV judge discipline clauses ────────────────────

    #[test]
    fn judge_prompt_carries_the_four_discipline_clauses() {
        let p = build_acceptance_prompt("crit", "task", "result");
        // Anti-ratchet: the bar may not rise between rounds.
        assert!(p.contains("反棘輪"));
        assert!(p.contains("驗收門檻不得跨輪升高"));
        // Audit, don't author.
        assert!(p.contains("只稽核、不自創"));
        assert!(p.contains("不得自行編造"));
        // No expansion beyond the contract.
        assert!(p.contains("反契約外擴張"));
        assert!(p.contains("驗收標準沒寫的事項不得作為否決理由"));
        // Self-reported completion is not evidence.
        assert!(p.contains("agent 自稱完成不是證據"));
    }

    #[test]
    fn simple_depth_prompt_keeps_discipline_without_leaking_aspect_names() {
        // The clauses must not smuggle an aspect name the shallow panel does
        // not judge (guards the existing Simple-depth invariant).
        let p = build_acceptance_prompt_for("crit", "task", "result", Difficulty::Simple);
        assert!(p.contains("反棘輪"));
        assert!(!p.contains("completeness"));
    }

