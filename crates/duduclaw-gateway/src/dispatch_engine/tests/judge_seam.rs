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

    /// v1.69.0: `evaluator_only` was removed. A deployment that still has it
    /// in `config.toml` (or the `evaluator` alias) adjudicates exactly like
    /// `mav` — the evaluator runs first, the panel decides the completion
    /// candidate — and the removal is reported once (audit + Activity Feed).
    #[tokio::test]
    async fn judge_seam_removed_evaluator_only_runs_the_mav_panel() {
        for raw in ["evaluator_only", "evaluator"] {
            let dir = tempfile::tempdir().unwrap();
            write_dispatch_config(dir.path(), &format!("judge = \"{raw}\""));
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
            assert_eq!(t.status, "done", "raw = {raw}");
            assert_eq!(
                judge_calls.load(Ordering::SeqCst),
                1,
                "a leftover evaluator_only must be judged by the MAV panel (raw = {raw})"
            );
            assert_eq!(evaluator.calls.load(Ordering::SeqCst), 1, "raw = {raw}");
            let fb = t.judge_feedback.unwrap_or_default();
            assert!(
                !fb.contains("驗收強度較弱"),
                "no weaker single-evaluator accept may survive the removal: {fb}"
            );

            let audit =
                std::fs::read_to_string(dir.path().join("security_audit.jsonl")).unwrap_or_default();
            assert!(
                audit.contains("judge_mode_removed") && audit.contains("evaluator_only"),
                "the removal must be audited (raw = {raw}): {audit}"
            );
            let (rows, _) = store
                .list_activity(None, Some("judge_mode_removed"), 10, 0)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "one Activity Feed notice (raw = {raw})");
            assert!(rows[0].summary.contains("v1.69.0"), "{}", rows[0].summary);
        }
    }

    /// v1.69.0: with the removed `evaluator_only` an evaluator malfunction or
    /// `two_stage_judge = false` behaves as in `mav` — straight to the panel,
    /// never a `needs_human` park and never an unopposed pass.
    #[tokio::test]
    async fn judge_seam_removed_evaluator_only_degrades_onto_the_panel_like_mav() {
        for (id, body, outcome) in [
            (
                "eo2",
                "judge = \"evaluator_only\"",
                Err::<PreEvaluation, String>("llm unreachable".into()),
            ),
            (
                "eo3",
                "judge = \"evaluator_only\"\ntwo_stage_judge = false",
                Ok(pre_eval(PreDecision::CandidateComplete, None)),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write_dispatch_config(dir.path(), body);
            let store = Arc::new(TaskStore::open(dir.path()).unwrap());
            seed_review(&store, id).await;
            let (engine, judge_calls, _) = seam_engine(dir.path(), store.clone(), outcome).await;
            engine.tick_once().await.unwrap();
            assert_eq!(store.get_task(id).await.unwrap().unwrap().status, "done", "{body}");
            assert_eq!(judge_calls.load(Ordering::SeqCst), 1, "{body}");
        }
    }

    /// v1.69.0: `human_only` was removed, but a deployment that still has it
    /// must NOT fall back to machine acceptance (that would drop a human
    /// gate). Every review parks as `needs_human` before any model call, the
    /// pause is classified `infra` (a platform setting, not the work), the
    /// operator-facing reason names the fix, and the removal is reported
    /// (audit + Activity Feed).
    #[tokio::test]
    async fn judge_seam_removed_human_only_parks_without_any_judge_call() {
        for raw in ["human_only", "human", " Human_Only "] {
            let dir = tempfile::tempdir().unwrap();
            write_dispatch_config(dir.path(), &format!("judge = \"{raw}\""));
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
            assert_eq!(t.status, "needs_human", "raw = {raw}");
            assert_eq!(judge_calls.load(Ordering::SeqCst), 0, "raw = {raw}");
            assert_eq!(
                evaluator.calls.load(Ordering::SeqCst),
                0,
                "a leftover human_only must not pay for any model call (raw = {raw})"
            );
            assert_eq!(
                t.pause_reason.as_deref(),
                Some(crate::pause_reason::PauseReason::Infra.as_str()),
                "raw = {raw}"
            );
            let fb = t.judge_feedback.unwrap_or_default();
            for needle in [
                "v1.69.0",
                "judge = \"mav\"",
                "autonomy_level",
                "approval_required_tools",
            ] {
                assert!(fb.contains(needle), "reason must name {needle}: {fb}");
            }

            let audit =
                std::fs::read_to_string(dir.path().join("security_audit.jsonl")).unwrap_or_default();
            assert!(
                audit.contains("judge_mode_removed") && audit.contains("human_only"),
                "the removal must be audited (raw = {raw}): {audit}"
            );
            let (rows, _) = store
                .list_activity(None, Some("judge_mode_removed"), 10, 0)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "one Activity Feed notice (raw = {raw})");
        }
    }

    /// The removal notice is once per process per home, not once per task:
    /// a second parked task in the same home adds no second audit row or
    /// Activity Feed notice.
    #[tokio::test]
    async fn judge_seam_removed_mode_notice_is_written_once_per_home() {
        let dir = tempfile::tempdir().unwrap();
        write_dispatch_config(dir.path(), "judge = \"human_only\"");
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        seed_review(&store, "ho2").await;
        let (engine, _, _) = seam_engine(
            dir.path(),
            store.clone(),
            Ok(pre_eval(PreDecision::CandidateComplete, None)),
        )
        .await;
        engine.tick_once().await.unwrap();
        seed_review(&store, "ho3").await;
        engine.tick_once().await.unwrap();

        assert_eq!(store.get_task("ho2").await.unwrap().unwrap().status, "needs_human");
        assert_eq!(store.get_task("ho3").await.unwrap().unwrap().status, "needs_human");
        let (rows, _) = store
            .list_activity(None, Some("judge_mode_removed"), 10, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let audit = std::fs::read_to_string(dir.path().join("security_audit.jsonl")).unwrap();
        assert_eq!(audit.matches("judge_mode_removed").count(), 1, "{audit}");
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
