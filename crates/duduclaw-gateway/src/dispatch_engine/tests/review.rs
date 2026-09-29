use super::*;

    #[tokio::test]
    async fn review_pass_promotes_to_done() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g1").await;

        let judge = Arc::new(StubJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
        });
        let engine = DispatchEngine::new(store.clone(), Some(judge));
        engine.tick_once().await.unwrap();

        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "done");
    }

    #[tokio::test]
    async fn review_reject_requeues_then_escalates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g2").await; // max_retries = 1

        let judge = Arc::new(StubJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: false,
                feedback: "nope".into(),
                aspects: None,
            }),
        });
        let engine = DispatchEngine::new(store.clone(), Some(judge));

        // First reject: retry 0 < 1 ⇒ back to `revising` (Iterative Kanban) with
        // feedback and the round counter bumped.
        engine.tick_once().await.unwrap();
        let t = store.get_task("g2").await.unwrap().unwrap();
        assert_eq!(t.status, "revising");
        assert_eq!(t.retry_count, 1);
        assert_eq!(t.revision_round, 1);
        assert_eq!(t.judge_feedback.as_deref(), Some("nope"));

        // Worker re-completes → review; second reject at cap ⇒ needs_human.
        store
            .atomic_claim("g2", "w", "2026-07-11T11:00:00Z", "2026-07-11T11:05:00Z")
            .await
            .unwrap()
            .is_claimed();
        store.complete_task("g2", "attempt 2", "w").await.unwrap();
        engine.tick_once().await.unwrap();
        assert_eq!(
            store.get_task("g2").await.unwrap().unwrap().status,
            "needs_human"
        );
    }

    #[tokio::test]
    async fn judge_error_parks_needs_human_fail_safe() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g3").await;

        let judge = Arc::new(StubJudge {
            outcome: Err("llm timeout".into()),
        });
        let engine = DispatchEngine::new(store.clone(), Some(judge));
        engine.tick_once().await.unwrap();

        let t = store.get_task("g3").await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human", "judge failure never auto-accepts");
        assert!(
            t.judge_feedback
                .as_deref()
                .unwrap_or("")
                .contains("judge unavailable")
        );
    }

    #[tokio::test]
    async fn no_judge_leaves_review_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "g4").await;

        let engine = DispatchEngine::new(store.clone(), None);
        engine.tick_once().await.unwrap();
        // No evaluator ⇒ still in review, not auto-accepted.
        assert_eq!(
            store.get_task("g4").await.unwrap().unwrap().status,
            "review"
        );
    }


    #[tokio::test]
    async fn outcome_check_failure_skips_judge_and_revises() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        // A files: contract for a file the worker never produced.
        let tag = crate::outcome_spec::OutcomeSpec::parse("files:report.docx")
            .unwrap()
            .to_tag()
            .unwrap();
        seed_review_with(&store, "og1", &tag, "我覺得應該算完成了").await;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let judge = Arc::new(CountingJudge {
            calls: calls.clone(),
            verdict: AcceptanceVerdict {
                passed: true,
                feedback: "would have passed".into(),
                aspects: None,
            },
        });
        let engine =
            DispatchEngine::new(store.clone(), Some(judge)).with_home_dir(dir.path().to_path_buf());
        engine.tick_once().await.unwrap();

        // Deterministic failure → back to revising, judge NEVER consulted.
        let t = store.get_task("og1").await.unwrap().unwrap();
        assert_eq!(t.status, "revising");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "judge must not be called");
        assert!(
            t.judge_feedback
                .as_deref()
                .unwrap_or("")
                .contains("report.docx")
        );
    }

    #[tokio::test]
    async fn outcome_check_json_missing_field_skips_judge() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let tag = crate::outcome_spec::OutcomeSpec::parse(
            r#"json:{"type":"object","required":["total"]}"#,
        )
        .unwrap()
        .to_tag()
        .unwrap();
        // Reply parses as JSON but is missing the required `total` field.
        seed_review_with(
            &store,
            "og2",
            &tag,
            r#"結果：```json
{"subtotal": 100}
```"#,
        )
        .await;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let judge = Arc::new(CountingJudge {
            calls: calls.clone(),
            verdict: AcceptanceVerdict {
                passed: true,
                feedback: "x".into(),
                aspects: None,
            },
        });
        let engine =
            DispatchEngine::new(store.clone(), Some(judge)).with_home_dir(dir.path().to_path_buf());
        engine.tick_once().await.unwrap();

        assert_eq!(
            store.get_task("og2").await.unwrap().unwrap().status,
            "revising"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn outcome_check_pass_reaches_judge_and_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        // The worker actually produced the declared file.
        let work_dir = dir.path().join("agents").join("w");
        std::fs::create_dir_all(&work_dir).unwrap();
        std::fs::write(work_dir.join("report.docx"), b"content").unwrap();
        let tag = crate::outcome_spec::OutcomeSpec::parse("files:report.docx")
            .unwrap()
            .to_tag()
            .unwrap();
        seed_review_with(&store, "og3", &tag, "報表已產出 report.docx").await;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let judge = Arc::new(CountingJudge {
            calls: calls.clone(),
            verdict: AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            },
        });
        let engine =
            DispatchEngine::new(store.clone(), Some(judge)).with_home_dir(dir.path().to_path_buf());
        engine.tick_once().await.unwrap();

        // Deterministic gate passed → judge consulted once → accepted.
        assert_eq!(store.get_task("og3").await.unwrap().unwrap().status, "done");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "judge runs exactly once after a passing gate"
        );
    }

    // ── WP-A9 item 4: `confirmed_facts` wiring (A1 leftover) ────

    #[tokio::test]
    async fn confirmed_facts_persisted_after_deterministic_outcome_pass() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let work_dir = dir.path().join("agents").join("w");
        std::fs::create_dir_all(&work_dir).unwrap();
        std::fs::write(work_dir.join("report.docx"), b"content").unwrap();
        let tag = crate::outcome_spec::OutcomeSpec::parse("files:report.docx")
            .unwrap()
            .to_tag()
            .unwrap();
        seed_review_with(&store, "og-cf", &tag, "報表已產出 report.docx").await;

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

        let t = store.get_task("og-cf").await.unwrap().unwrap();
        assert_eq!(t.status, "done");
        let snapshot =
            crate::goal_state::GoalStateSnapshot::from_json(t.goal_state_json.as_deref());
        assert_eq!(
            snapshot.confirmed_facts.len(),
            1,
            "the deterministic outcome-spec pass must record exactly one confirmed fact"
        );
        assert!(
            snapshot.confirmed_facts[0].contains("outcome schema"),
            "confirmed fact must describe the deterministic check that passed, got: {:?}",
            snapshot.confirmed_facts
        );
    }

    #[tokio::test]
    async fn confirmed_facts_caps_to_six_most_recent_and_truncates_cjk_safely() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        store.insert_task(&pending_goal("cf2")).await.unwrap();

        let existing = crate::goal_state::GoalStateSnapshot {
            pending_hypotheses: Vec::new(),
            confirmed_facts: (0..6).map(|i| format!("old fact {i}")).collect(),
            // H5 (WP-B) / H10: `GoalStateSnapshot` gained `bail_hint` and
            // `tool_streak_hint` fields — these literals are updated to keep
            // compiling, unrelated to what this test actually exercises
            // (confirmed_facts capping).
            bail_hint: None,
            tool_streak_hint: None,
        };
        store
            .set_goal_state_json("cf2", Some(&existing.to_json()))
            .await
            .unwrap();

        let engine = DispatchEngine::new(store.clone(), None);
        // A CJK string well past the 120-char cap — must not panic on a
        // mid-codepoint byte slice (project convention: CJK-safe truncation).
        let long_cjk_fact = "測試".repeat(200);
        // M7: no longer takes a caller-supplied snapshot — reads the fresh
        // DB state itself under `merge_goal_state_json`'s lock.
        engine
            .persist_confirmed_facts("cf2", &[long_cjk_fact.clone()])
            .await;

        let t = store.get_task("cf2").await.unwrap().unwrap();
        let snap = crate::goal_state::GoalStateSnapshot::from_json(t.goal_state_json.as_deref());
        assert_eq!(
            snap.confirmed_facts.len(),
            6,
            "capped to the 6 most recent entries"
        );
        assert!(
            !snap.confirmed_facts.contains(&"old fact 0".to_string()),
            "oldest entry must be dropped once the cap is exceeded"
        );
        assert!(snap.confirmed_facts.last().unwrap().chars().count() <= 120);
    }

    /// M7 regression: `persist_confirmed_facts` must never clobber a
    /// `pending_hypotheses` key that already exists on the SAME
    /// `goal_state_json` blob (written by `goal_loop.rs::capture_round_state`
    /// via the same `merge_goal_state_json` API) — the exact lost-update the
    /// migration off the old read-then-`set_goal_state_json`-the-whole-blob
    /// pattern closes. Simulates the interleaving directly (both writers
    /// targeting the store, not relying on real concurrency timing) by
    /// seeding `pending_hypotheses` first, then persisting confirmed facts,
    /// and asserting BOTH keys survive.
    #[tokio::test]
    async fn persist_confirmed_facts_does_not_clobber_pending_hypotheses() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        store.insert_task(&pending_goal("cf-merge")).await.unwrap();

        // Simulates `goal_loop.rs::capture_round_state`'s write landing
        // first, going through the SAME merge API.
        store
            .merge_goal_state_json("cf-merge", |v| {
                v["pending_hypotheses"] = serde_json::json!(["hyp A", "hyp B"]);
            })
            .await
            .unwrap();

        let engine = DispatchEngine::new(store.clone(), None);
        engine
            .persist_confirmed_facts("cf-merge", &["fact one".to_string()])
            .await;

        let t = store.get_task("cf-merge").await.unwrap().unwrap();
        let snap = crate::goal_state::GoalStateSnapshot::from_json(t.goal_state_json.as_deref());
        assert_eq!(snap.confirmed_facts, vec!["fact one".to_string()]);
        assert_eq!(
            snap.pending_hypotheses,
            vec!["hyp A".to_string(), "hyp B".to_string()],
            "concurrently-written pending_hypotheses must survive the confirmed_facts merge"
        );
    }

    #[tokio::test]
    async fn persist_confirmed_facts_is_a_noop_on_empty_facts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        store.insert_task(&pending_goal("cf-empty")).await.unwrap();
        let engine = DispatchEngine::new(store.clone(), None);
        engine.persist_confirmed_facts("cf-empty", &[]).await;
        let t = store.get_task("cf-empty").await.unwrap().unwrap();
        assert!(t.goal_state_json.is_none(), "no facts ⇒ no write at all");
    }

    /// Fix-2 C1c: end-to-end `review_goal_tasks` wiring — genuine (non-echo)
    /// grounding evidence records the NEW neutral wording, not the old
    /// overstated "已通過…有工具佐證" claim.
    #[tokio::test]
    async fn confirmed_facts_neutral_wording_for_genuine_grounded_evidence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":true,\"result_text\":\"Refund policy: 30 days from purchase, receipt required.\"}\n",
        )
        .unwrap();

        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        store.insert_task(&pending_goal("cf-ground")).await.unwrap();
        store
            .atomic_claim(
                "cf-ground",
                "w",
                "2026-07-11T10:00:00Z",
                "2026-07-11T10:05:00Z",
            )
            .await
            .unwrap()
            .is_claimed();
        store
            .complete_task(
                "cf-ground",
                "Refund policy: 30 days from purchase, receipt required.",
                "w",
            )
            .await
            .unwrap();

        let judge = Arc::new(StubJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
        });
        let engine =
            DispatchEngine::new(store.clone(), Some(judge)).with_home_dir(dir.path().to_path_buf());
        engine.review_goal_tasks().await.unwrap();

        let t = store.get_task("cf-ground").await.unwrap().unwrap();
        assert_eq!(t.status, "done");
        let snapshot =
            crate::goal_state::GoalStateSnapshot::from_json(t.goal_state_json.as_deref());
        assert_eq!(snapshot.confirmed_facts.len(), 1);
        assert_eq!(snapshot.confirmed_facts[0], "本輪 grounding 前置檢查通過。");
        assert!(
            !snapshot.confirmed_facts[0].contains("有工具佐證"),
            "must use the C1c neutral wording, not the old overstated claim"
        );
    }

    /// Fix-2 C1c belt-and-suspenders: even in the hypothetical case where a
    /// self-echo tool's `result_text` WAS captured (bypassing the C1a
    /// source-level suppression — simulated here by hand-writing the audit
    /// row directly, since the real MCP dispatch path no longer produces
    /// one), `review_goal_tasks` must not record a `confirmed_facts` entry
    /// for it, even though `grounding_precheck` itself still reports
    /// `Grounded`.
    #[tokio::test]
    async fn confirmed_facts_not_recorded_when_grounding_tool_is_self_echo() {
        let dir = tempfile::tempdir().unwrap();
        let echoed = "refund #4821 approved for customer";
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"mcp__duduclaw__tasks_complete\",\"success\":true,\"result_text\":\"{echoed}\"}}\n"
            ),
        )
        .unwrap();

        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        store.insert_task(&pending_goal("cf-echo")).await.unwrap();
        store
            .atomic_claim(
                "cf-echo",
                "w",
                "2026-07-11T10:00:00Z",
                "2026-07-11T10:05:00Z",
            )
            .await
            .unwrap()
            .is_claimed();
        store
            .complete_task("cf-echo", &format!("Done: {echoed}"), "w")
            .await
            .unwrap();

        let judge = Arc::new(StubJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
        });
        let engine =
            DispatchEngine::new(store.clone(), Some(judge)).with_home_dir(dir.path().to_path_buf());
        engine.review_goal_tasks().await.unwrap();

        let t = store.get_task("cf-echo").await.unwrap().unwrap();
        assert_eq!(t.status, "done", "the judge still accepts independently");
        let snapshot =
            crate::goal_state::GoalStateSnapshot::from_json(t.goal_state_json.as_deref());
        assert!(
            snapshot.confirmed_facts.is_empty(),
            "self-echo tool evidence must never be credited as a confirmed fact: {:?}",
            snapshot.confirmed_facts
        );
    }

