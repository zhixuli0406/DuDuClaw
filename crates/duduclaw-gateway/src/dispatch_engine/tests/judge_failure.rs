use super::*;

    #[tokio::test]
    async fn judge_transport_error_surfaces_as_err_not_a_pass() {
        let judge = LlmAcceptanceJudge::new(ErrCaller);
        let out = judge.judge("crit", "task", "result").await;
        assert!(out.is_err(), "a transport failure must never parse as PASS");
    }

    #[tokio::test]
    async fn judge_transport_error_parks_needs_human_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "fp1").await;

        let judge: Arc<dyn AcceptanceJudge> = Arc::new(LlmAcceptanceJudge::new(ErrCaller));
        let engine = DispatchEngine::new(store.clone(), Some(judge));
        engine.tick_once().await.unwrap();

        let t = store.get_task("fp1").await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human");
        assert!(
            t.judge_feedback
                .as_deref()
                .unwrap_or("")
                .contains("judge unavailable")
        );
    }

    #[tokio::test]
    async fn judge_empty_reply_rejects_never_accepts() {
        // Empty, whitespace-only, and control-only replies all fail closed.
        for raw in ["", "   ", "\n\n", "\u{200b}"] {
            let judge = LlmAcceptanceJudge::new(StubCaller(raw.into()));
            let v = judge.judge("crit", "task", "result").await.unwrap();
            assert!(!v.passed, "empty-ish judge reply {raw:?} must not accept");
        }
    }

    #[tokio::test]
    async fn judge_truncated_or_garbage_json_rejects() {
        // Truncated panel JSON (`}` present but the object never closes).
        // REGRESSION: this exact reply used to be ACCEPTED — the broken
        // fragment fell through to the legacy token scanner, whose `PASS`
        // match fired on the JSON key `"pass"`.
        let judge = LlmAcceptanceJudge::new(StubCaller(
            r#"{"correctness": {"pass": true, "reason": "ok"}"#.into(),
        ));
        assert!(
            !judge.judge("crit", "task", "result").await.unwrap().passed,
            "a truncated panel reply must fail closed, never accept"
        );

        // Valid JSON but not one required aspect ⇒ unusable panel ⇒ fail closed.
        let judge = LlmAcceptanceJudge::new(StubCaller(r#"{"verdict": "looks fine"}"#.into()));
        assert!(!judge.judge("crit", "task", "result").await.unwrap().passed);

        // A JSON object whose only `pass` is a bare key (no aspect at all) —
        // the shape that most directly exercised the old hole.
        let judge = LlmAcceptanceJudge::new(StubCaller(r#"{"pass": true}"#.into()));
        assert!(!judge.judge("crit", "task", "result").await.unwrap().passed);

        // String "true" instead of a boolean ⇒ that aspect fails closed.
        let judge = LlmAcceptanceJudge::new(StubCaller(
            r#"{"correctness": {"pass": "true"}, "completeness": {"pass": true}, "safety": {"pass": true}}"#
                .into(),
        ));
        assert!(!judge.judge("crit", "task", "result").await.unwrap().passed);
    }

    #[test]
    fn panel_broken_json_fails_closed_and_says_so() {
        // Cut off mid-object, no closing brace at all.
        let v = parse_panel_verdict(r#"{"correctness": {"pass": true"#);
        assert!(!v.passed);
        assert!(
            v.feedback.contains("fail-closed"),
            "the feedback must name the reason: {}",
            v.feedback
        );
        // No fabricated per-aspect rows for a reply we could not read.
        assert!(v.aspects.is_none());

        // Cut off after the inner object closed (one `}` present) — the
        // original regression input.
        let v = parse_panel_verdict(r#"{"correctness": {"pass": true, "reason": "ok"}"#);
        assert!(!v.passed);
        assert!(v.feedback.contains("fail-closed"));

        // The most dangerous truncation of all: the fragment LEADS with the
        // `pass` key, so the legacy scanner's leading-token rule would not
        // have saved us either.
        let v = parse_panel_verdict(r#"{"pass": true, "correctness": {"#);
        assert!(
            !v.passed,
            "a truncated fragment leading with a `pass` key must never accept"
        );
    }

    #[test]
    fn parse_verdict_requires_pass_to_lead_the_first_line() {
        // REGRESSION: prose arguing the OPPOSITE used to be read as accept
        // because `PASS` appeared somewhere on the first line.
        assert!(!parse_verdict("The result does not pass the acceptance criteria").passed);
        assert!(!parse_verdict("Unable to pass judgement without the artifact").passed);
        // The genuine legacy shapes still work.
        assert!(parse_verdict("PASS").passed);
        assert!(parse_verdict("PASS — all criteria met").passed);
        assert!(parse_verdict("  pass.\nreason").passed);
    }

    #[tokio::test]
    async fn judge_rejection_never_leaves_a_task_done() {
        // Belt-and-suspenders on the engine side: a rejecting panel routes to
        // revising / needs_human, never `done`.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "fp2").await;
        let judge: Arc<dyn AcceptanceJudge> = Arc::new(LlmAcceptanceJudge::new(StubCaller(
            String::new(), // empty reply ⇒ conservative FAIL
        )));
        let engine = DispatchEngine::new(store.clone(), Some(judge));
        engine.tick_once().await.unwrap();
        let status = store.get_task("fp2").await.unwrap().unwrap().status;
        assert!(
            status == "revising" || status == "needs_human",
            "empty judge reply must not accept, got {status}"
        );
    }


    /// H9-G goal contract freeze: when a task carries a frozen baseline that
    /// differs from the (hypothetically edited) mutable `acceptance_criteria`
    /// field, the judge must see the baseline — never the mutable copy. This
    /// is the value-source change in `review_goal_tasks` (dispatch_engine.rs).
    #[tokio::test]
    async fn judge_reads_frozen_baseline_not_the_mutable_field() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let mut g = pending_goal("baseline1");
        // Baseline frozen at creation; acceptance_criteria diverges from it
        // to simulate a later operator edit to the mutable copy.
        g.acceptance_criteria_baseline = Some("ORIGINAL frozen criteria".into());
        g.acceptance_criteria = Some("EDITED mutable criteria".into());
        store.insert_task(&g).await.unwrap();
        store
            .atomic_claim(
                "baseline1",
                "w",
                "2026-07-11T10:00:00Z",
                "2026-07-11T10:05:00Z",
            )
            .await
            .unwrap()
            .is_claimed();
        store
            .complete_task("baseline1", "my result", "w")
            .await
            .unwrap();

        let judge = Arc::new(CriteriaCapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
            captured_criteria: std::sync::Mutex::new(None),
        });
        let engine = DispatchEngine::new(
            store.clone(),
            Some(judge.clone() as Arc<dyn AcceptanceJudge>),
        );
        engine.tick_once().await.unwrap();

        let captured = judge.captured_criteria.lock().unwrap().clone().unwrap();
        assert!(captured.contains("ORIGINAL frozen criteria"), "{captured}");
        assert!(!captured.contains("EDITED mutable criteria"), "{captured}");
    }

    /// Backward compatibility: a task with no baseline (created before this
    /// column existed, or via a path that never freezes one) falls back to
    /// the mutable `acceptance_criteria` field — never an empty criteria block.
    #[tokio::test]
    async fn judge_falls_back_to_mutable_field_when_no_baseline_exists() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let mut g = pending_goal("baseline2");
        g.acceptance_criteria_baseline = None;
        g.acceptance_criteria = Some("only mutable criteria present".into());
        store.insert_task(&g).await.unwrap();
        store
            .atomic_claim(
                "baseline2",
                "w",
                "2026-07-11T10:00:00Z",
                "2026-07-11T10:05:00Z",
            )
            .await
            .unwrap()
            .is_claimed();
        store
            .complete_task("baseline2", "my result", "w")
            .await
            .unwrap();

        let judge = Arc::new(CriteriaCapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "ok".into(),
                aspects: None,
            }),
            captured_criteria: std::sync::Mutex::new(None),
        });
        let engine = DispatchEngine::new(
            store.clone(),
            Some(judge.clone() as Arc<dyn AcceptanceJudge>),
        );
        engine.tick_once().await.unwrap();

        let captured = judge.captured_criteria.lock().unwrap().clone().unwrap();
        assert!(
            captured.contains("only mutable criteria present"),
            "{captured}"
        );
    }

