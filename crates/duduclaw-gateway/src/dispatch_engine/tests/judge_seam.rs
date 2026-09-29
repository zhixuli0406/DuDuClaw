use super::*;

    /// ① Default (no key) and an unrecognized value must BOTH land on `mav`:
    /// the panel is consulted and its verdict is what settles the task. An
    /// unknown value must never be read as "skip the expensive judge".
    #[tokio::test]
    async fn judge_seam_defaults_and_unknown_values_use_the_mav_panel() {
        for body in [
            "enabled = true",
            "judge = \"chaos_monkey\"",
            "judge = \"mav\"",
        ] {
            let dir = tempfile::tempdir().unwrap();
            write_dispatch_config(dir.path(), body);
            let store = Arc::new(TaskStore::open(dir.path()).unwrap());
            seed_review(&store, "sm1").await;

            let (engine, judge_calls, evaluator) = seam_engine(
                dir.path(),
                store.clone(),
                Ok(pre_eval(PreDecision::CandidateComplete, None)),
            )
            .await;
            engine.tick_once().await.unwrap();

            let t = store.get_task("sm1").await.unwrap().unwrap();
            assert_eq!(t.status, "done", "body = {body}");
            assert_eq!(
                judge_calls.load(Ordering::SeqCst),
                1,
                "the MAV panel must decide in mav mode (body = {body})"
            );
            assert_eq!(evaluator.calls.load(Ordering::SeqCst), 1, "body = {body}");
        }
    }

    /// ④ `mav` regression: an explicit `judge = "mav"` and a home dir with no
    /// `[dispatch] judge` at all must produce byte-identical observable
    /// outcomes (status, feedback, round counters) — the seam adds routing,
    /// never behavior, in the default mode.
    #[tokio::test]
    async fn judge_seam_mav_is_byte_identical_to_the_pre_seam_flow() {
        async fn run(body: Option<&str>) -> (String, String, i64, i64) {
            let dir = tempfile::tempdir().unwrap();
            if let Some(b) = body {
                write_dispatch_config(dir.path(), b);
            }
            let store = Arc::new(TaskStore::open(dir.path()).unwrap());
            seed_review(&store, "mv1").await;
            let judge = Arc::new(StubJudge {
                outcome: Ok(AcceptanceVerdict {
                    passed: false,
                    feedback: "缺少測試".into(),
                    aspects: None,
                }),
            });
            let engine = DispatchEngine::new(store.clone(), Some(judge))
                .with_home_dir(dir.path().to_path_buf());
            engine.tick_once().await.unwrap();
            let t = store.get_task("mv1").await.unwrap().unwrap();
            (
                t.status,
                t.judge_feedback.unwrap_or_default(),
                t.revision_round,
                t.retry_count,
            )
        }
        assert_eq!(run(None).await, run(Some("judge = \"mav\"")).await);
    }

    /// ② `evaluator_only` accepts on `candidate_complete` WITHOUT paying for
    /// the panel, and labels the verdict as the weaker low-cost mode.
    #[tokio::test]
    async fn judge_seam_evaluator_only_accepts_without_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        write_dispatch_config(dir.path(), "judge = \"evaluator_only\"");
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "eo1").await;

        let (engine, judge_calls, evaluator) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        let t = store.get_task("eo1").await.unwrap().unwrap();
        assert_eq!(t.status, "done");
        assert_eq!(
            judge_calls.load(Ordering::SeqCst),
            0,
            "evaluator_only must never pay for the MAV panel"
        );
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 1);
        let fb = t.judge_feedback.unwrap_or_default();
        assert!(
            fb.contains("evaluator_only") && fb.contains("驗收強度較弱"),
            "the accept must self-label as the weaker low-cost mode: {fb}"
        );
    }

    /// ② `evaluator_only` still rejects/escalates exactly as before on the
    /// evaluator's own `continue` / `blocked` verdicts (those paths are mode
    /// independent — the mode only changes what `candidate_complete` means).
    #[tokio::test]
    async fn judge_seam_evaluator_only_keeps_continue_and_blocked_routing() {
        let dir = tempfile::tempdir().unwrap();
        write_dispatch_config(dir.path(), "judge = \"evaluator_only\"");
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "eo2").await;
        let (engine, judge_calls, _) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::Continue, None)),
        )
        .await;
        engine.tick_once().await.unwrap();
        assert_eq!(
            store.get_task("eo2").await.unwrap().unwrap().status,
            "revising"
        );
        assert_eq!(judge_calls.load(Ordering::SeqCst), 0);
    }

    /// ② fail-closed: an evaluator that ERRORS under `evaluator_only` has no
    /// panel to degrade onto, so the task parks for a human — it must never
    /// read as an unopposed pass, and must not silently fall through to the
    /// panel either (that would make the mode a lie).
    #[tokio::test]
    async fn judge_seam_evaluator_only_error_fails_closed_to_needs_human() {
        let dir = tempfile::tempdir().unwrap();
        write_dispatch_config(dir.path(), "judge = \"evaluator_only\"");
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "eo3").await;

        let (engine, judge_calls, _) =
            seam_engine(dir.path(), store.clone(), Err("llm unreachable".into())).await;
        engine.tick_once().await.unwrap();

        let t = store.get_task("eo3").await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human");
        assert_eq!(
            judge_calls.load(Ordering::SeqCst),
            0,
            "a failed evaluator_only must not silently borrow the MAV panel"
        );
        assert_eq!(
            t.pause_reason.as_deref(),
            Some(crate::pause_reason::PauseReason::Infra.as_str())
        );
    }

    /// ② fail-closed: `evaluator_only` with the evaluator switched off via
    /// `[dispatch] two_stage_judge = false` leaves NO judge at all. It parks
    /// for a human rather than accepting or quietly using the panel.
    #[tokio::test]
    async fn judge_seam_evaluator_only_without_a_usable_evaluator_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        write_dispatch_config(
            dir.path(),
            "judge = \"evaluator_only\"\ntwo_stage_judge = false",
        );
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "eo4").await;

        let (engine, judge_calls, evaluator) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        let t = store.get_task("eo4").await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human");
        assert_eq!(judge_calls.load(Ordering::SeqCst), 0);
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 0);

        // And the same config WITHOUT the mode is unchanged: two_stage off in
        // `mav` mode simply means "straight to the panel".
        let dir2 = tempfile::tempdir().unwrap();
        write_dispatch_config(dir2.path(), "two_stage_judge = false");
        let store2 = Arc::new(TaskStore::open(dir2.path()).unwrap());
        seed_review(&store2, "eo5").await;
        let (engine2, judge_calls2, _) = seam_engine(
            dir2.path(),
            store2.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine2.tick_once().await.unwrap();
        assert_eq!(
            store2.get_task("eo5").await.unwrap().unwrap().status,
            "done"
        );
        assert_eq!(judge_calls2.load(Ordering::SeqCst), 1);
    }

    /// `human_only` (design §6-P1's third mode): never machine-judged, and it
    /// must ACTUALLY stop the task — "必須真的攔下、不得自動放行".
    #[tokio::test]
    async fn judge_seam_human_only_never_auto_accepts() {
        let dir = tempfile::tempdir().unwrap();
        write_dispatch_config(dir.path(), "judge = \"human_only\"");
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "ho1").await;

        let (engine, judge_calls, evaluator) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        let t = store.get_task("ho1").await.unwrap().unwrap();
        assert_eq!(t.status, "needs_human");
        assert_eq!(judge_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            evaluator.calls.load(Ordering::SeqCst),
            0,
            "human_only must not pay for any model call"
        );
    }


    #[cfg(unix)]
    #[tokio::test]
    async fn judge_seam_external_verdict_settles_the_task_without_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let bin = judge_script(
            dir.path(),
            "ext-pass.sh",
            "cat > /dev/null\necho '{\"pass\": true, \"feedback\": \"外部判官確認交付\"}'",
        );
        write_dispatch_config(
            dir.path(),
            &format!("judge = \"external\"\njudge_command = [\"{bin}\"]"),
        );
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "ex1").await;

        let (engine, judge_calls, _) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        let t = store.get_task("ex1").await.unwrap().unwrap();
        assert_eq!(t.status, "done");
        assert_eq!(
            judge_calls.load(Ordering::SeqCst),
            0,
            "a healthy external judge replaces the panel"
        );
        let fb = t.judge_feedback.unwrap_or_default();
        assert!(fb.contains("外部判官確認交付"), "{fb}");
        assert!(
            fb.contains("未受信資料"),
            "external feedback must carry its provenance label: {fb}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn judge_seam_external_rejection_settles_the_task_without_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let bin = judge_script(
            dir.path(),
            "ext-fail.sh",
            "cat > /dev/null\necho '{\"pass\": false, \"feedback\": \"驗收條件三未達成\"}'",
        );
        write_dispatch_config(
            dir.path(),
            &format!("judge = \"external\"\njudge_command = [\"{bin}\"]"),
        );
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "ex2").await;

        let (engine, judge_calls, _) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        let t = store.get_task("ex2").await.unwrap().unwrap();
        assert_eq!(t.status, "revising");
        assert_eq!(judge_calls.load(Ordering::SeqCst), 0);
        assert!(
            t.judge_feedback
                .unwrap_or_default()
                .contains("驗收條件三未達成")
        );
    }

    /// ③ timeout ⇒ degrade to the MAV panel (a degrade must be *stricter*, not
    /// a release), and the degrade is audited.
    #[cfg(unix)]
    #[tokio::test]
    async fn judge_seam_external_timeout_degrades_to_the_panel_and_audits() {
        let dir = tempfile::tempdir().unwrap();
        let bin = judge_script(dir.path(), "ext-slow.sh", "sleep 30");
        write_dispatch_config(
            dir.path(),
            &format!("judge = \"external\"\njudge_command = [\"{bin}\"]\njudge_timeout_secs = 1"),
        );
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "ex3").await;

        let (engine, judge_calls, _) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        assert_eq!(store.get_task("ex3").await.unwrap().unwrap().status, "done");
        assert_eq!(
            judge_calls.load(Ordering::SeqCst),
            1,
            "a timed-out external judge must hand the decision to the MAV panel"
        );
        let audit = std::fs::read_to_string(dir.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("judge_seam_degraded"), "{audit}");
        assert!(audit.contains("timed out"), "{audit}");
    }

    /// ③ unparseable stdout ⇒ degrade to the panel. "LGTM" is not a verdict.
    #[cfg(unix)]
    #[tokio::test]
    async fn judge_seam_external_bad_json_degrades_to_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let bin = judge_script(dir.path(), "ext-junk.sh", "cat > /dev/null\necho 'LGTM'");
        write_dispatch_config(
            dir.path(),
            &format!("judge = \"external\"\njudge_command = [\"{bin}\"]"),
        );
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "ex4").await;

        let (engine, judge_calls, _) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        assert_eq!(store.get_task("ex4").await.unwrap().unwrap().status, "done");
        assert_eq!(judge_calls.load(Ordering::SeqCst), 1);
        let audit = std::fs::read_to_string(dir.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("judge_seam_degraded"), "{audit}");
    }

    /// ③ `external` selected but never configured ⇒ degrade to the panel +
    /// audit. A misconfigured seam is never a free pass.
    #[tokio::test]
    async fn judge_seam_external_without_a_command_degrades_to_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        write_dispatch_config(dir.path(), "judge = \"external\"");
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "ex5").await;

        let (engine, judge_calls, _) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();

        assert_eq!(store.get_task("ex5").await.unwrap().unwrap().status, "done");
        assert_eq!(judge_calls.load(Ordering::SeqCst), 1);
        let audit = std::fs::read_to_string(dir.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("judge_seam_degraded"), "{audit}");
    }
