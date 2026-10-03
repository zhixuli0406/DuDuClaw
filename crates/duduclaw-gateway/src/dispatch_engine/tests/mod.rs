    use super::*;
    // Test-only: `filter_tool_activity` has no production caller left in
    // this module after WP-A3's extraction (production code now goes
    // through `read_tool_activity_records`, which calls it internally in
    // `tool_activity.rs`) — imported here explicitly so the outer `use` in
    // this file doesn't trigger an unused-import warning on a plain
    // `cargo check` (which excludes `#[cfg(test)]` code).
    use crate::task_store::{TaskRow, TaskStore};
    use crate::tool_activity::filter_tool_activity;

    mod a1_ledger;
    mod criteria_ledger;
    mod engine;
    mod grounding;
    mod judge_failure;
    mod judge_seam;
    mod review;
    mod tool_activity;
    mod two_stage;
    mod verdict;

    /// Test helper: a text-less `NativeToolEvent` (pre-R1 shape) — most
    /// existing tests only care about `tool_name`/`success`.
    fn native(tool_name: &str, success: bool) -> NativeToolEvent {
        NativeToolEvent {
            tool_name: tool_name.to_string(),
            success,
            result_text: None,
            input_text: None,
        }
    }

    /// R1 test helper: a `NativeToolEvent` carrying masked result/input
    /// text, as a producer that captured the runtime's own event stream
    /// would build it.
    fn native_with_text(
        tool_name: &str,
        success: bool,
        result_text: &str,
        input_text: Option<&str>,
    ) -> NativeToolEvent {
        NativeToolEvent {
            tool_name: tool_name.to_string(),
            success,
            result_text: Some(result_text.to_string()),
            input_text: input_text.map(String::from),
        }
    }

    fn pending_goal(id: &str) -> TaskRow {
        let mut t = TaskRow::new(
            id.into(),
            format!("goal {id}"),
            "do the work".into(),
            "medium".into(),
            String::new(),
            "system".into(),
        );
        t.status = "pending".into();
        t.goal_mode = true;
        t.max_retries = 1;
        t.acceptance_criteria = Some("must be correct".into());
        t
    }

    /// Judge stub: fixed verdict, or an error to exercise the fail-safe path.
    struct StubJudge {
        outcome: Result<AcceptanceVerdict, String>,
    }

    #[async_trait]
    impl AcceptanceJudge for StubJudge {
        async fn judge(
            &self,
            _criteria: &str,
            _task: &str,
            _result: &str,
        ) -> Result<AcceptanceVerdict, String> {
            self.outcome.clone()
        }
    }


    /// Stub `LlmCaller` for the `LlmAcceptanceJudge` adapter: fixed reply.
    struct StubCaller(String);
    #[async_trait]
    impl duduclaw_fork::judge::LlmCaller for StubCaller {
        async fn complete(&self, _prompt: &str) -> duduclaw_fork::Result<String> {
            Ok(self.0.clone())
        }
    }


    async fn seed_review(store: &TaskStore, id: &str) {
        let g = pending_goal(id);
        store.insert_task(&g).await.unwrap();
        // Claim + complete → goal-mode routes to `review`.
        store
            .atomic_claim(id, "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap()
            .is_claimed();
        store.complete_task(id, "my result", "w").await.unwrap();
        assert_eq!(store.get_task(id).await.unwrap().unwrap().status, "review");
    }


    // ── WP2.4 deterministic outcome acceptance (before the judge) ──

    /// Judge that counts how many times it is asked to rule — lets a test prove
    /// the deterministic outcome gate short-circuits the (expensive) LLM judge.
    struct CountingJudge {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        verdict: AcceptanceVerdict,
    }

    #[async_trait]
    impl AcceptanceJudge for CountingJudge {
        async fn judge(
            &self,
            _criteria: &str,
            _task: &str,
            _result: &str,
        ) -> Result<AcceptanceVerdict, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.verdict.clone())
        }
    }

    /// Seed a `review` goal task carrying `tags` and a worker `result_summary`.
    async fn seed_review_with(store: &TaskStore, id: &str, tags: &str, result: &str) {
        let mut g = pending_goal(id);
        g.tags = tags.to_string();
        store.insert_task(&g).await.unwrap();
        store
            .atomic_claim(id, "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap()
            .is_claimed();
        store.complete_task(id, result, "w").await.unwrap();
        assert_eq!(store.get_task(id).await.unwrap().unwrap().status, "review");
    }


    // ── Team-as-Agent (live round 3 E3): evidence over a set of agents ──

    fn team_audit_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            concat!(
                // The employee itself did nothing but open the round.
                "{\"timestamp\":\"2026-09-24T19:30:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_update\",\"success\":true}\n",
                // The work happened under the members' own ids.
                "{\"timestamp\":\"2026-09-24T19:31:00Z\",\"agent_id\":\"eph-r1-exec\",\"tool_name\":\"Write\",\"success\":true}\n",
                "{\"timestamp\":\"2026-09-24T19:32:00Z\",\"agent_id\":\"eph-r1-exec\",\"tool_name\":\"Write\",\"success\":false}\n",
                "{\"timestamp\":\"2026-09-24T19:33:00Z\",\"agent_id\":\"eph-r1-plan\",\"tool_name\":\"team_handoff\",\"success\":true}\n",
                // Another employee's member must never leak in.
                "{\"timestamp\":\"2026-09-24T19:33:30Z\",\"agent_id\":\"eph-other\",\"tool_name\":\"Bash\",\"success\":true}\n",
            ),
        )
        .unwrap();
        dir
    }

    const T_START: &str = "2026-09-24T19:00:00Z";
    const T_END: &str = "2026-09-24T20:00:00Z";


    // ── B3: grounding pre-check (`grounding_precheck`) ──────────

    fn ok_record(tool: &str, result_text: &str) -> ToolActivityRecord {
        ToolActivityRecord {
            tool_name: tool.to_string(),
            success: true,
            result_text: Some(result_text.to_string()),
            input_text: None,
        }
    }

    /// Fix-2 C1b variant: also carries the call's own masked input text, so
    /// tests can exercise `shares_contiguous_run_excluding_echo` through the
    /// full `grounding_precheck` path.
    fn ok_record_with_input(tool: &str, result_text: &str, input_text: &str) -> ToolActivityRecord {
        ToolActivityRecord {
            tool_name: tool.to_string(),
            success: true,
            result_text: Some(result_text.to_string()),
            input_text: Some(input_text.to_string()),
        }
    }


    /// Judge stub that records the `task` string it was called with, so the
    /// integration test can assert the `<tool_activity>` block actually
    /// reached the judge prompt (not just that the pure functions work in
    /// isolation).
    struct CapturingJudge {
        outcome: Result<AcceptanceVerdict, String>,
        captured_task: std::sync::Mutex<Option<String>>,
    }

    #[async_trait]
    impl AcceptanceJudge for CapturingJudge {
        async fn judge(
            &self,
            _criteria: &str,
            task: &str,
            _result: &str,
        ) -> Result<AcceptanceVerdict, String> {
            *self.captured_task.lock().unwrap() = Some(task.to_string());
            self.outcome.clone()
        }
    }


    // ── H1: two-stage adjudication ──────────────────────────

    fn pre_eval(decision: PreDecision, blocker: Option<&str>) -> PreEvaluation {
        PreEvaluation {
            decision,
            evidence: "工具紀錄顯示報表尚未產出".into(),
            next_step: "先產出 report.md 再回報".into(),
            blocker_key: blocker.map(String::from),
        }
    }

    /// First-stage evaluator stub: fixed outcome, counts calls, records the
    /// last transcript it was handed.
    struct StubPreEvaluator {
        outcome: Result<PreEvaluation, String>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        last_transcript: std::sync::Mutex<String>,
    }

    impl StubPreEvaluator {
        fn new(outcome: Result<PreEvaluation, String>) -> Arc<Self> {
            Arc::new(Self {
                outcome,
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                last_transcript: std::sync::Mutex::new(String::new()),
            })
        }
    }

    #[async_trait]
    impl PreAcceptanceEvaluator for StubPreEvaluator {
        async fn evaluate(
            &self,
            _criteria: &str,
            _task: &str,
            transcript: &str,
        ) -> Result<PreEvaluation, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_transcript.lock().unwrap() = transcript.to_string();
            self.outcome.clone()
        }
    }

    /// Counting judge wired to always accept — any test asserting "the panel
    /// was never consulted" fails loudly if the routing leaks through.
    fn accepting_counting_judge() -> (Arc<CountingJudge>, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let judge = Arc::new(CountingJudge {
            calls: calls.clone(),
            verdict: AcceptanceVerdict {
                passed: true,
                feedback: "would have passed".into(),
                aspects: None,
            },
        });
        (judge, calls)
    }


    // ── H3: every MAV failure path fails toward reject / needs_human ──

    /// `LlmCaller` that always errors — a transport failure.
    struct ErrCaller;
    #[async_trait]
    impl duduclaw_fork::judge::LlmCaller for ErrCaller {
        async fn complete(&self, _prompt: &str) -> duduclaw_fork::Result<String> {
            Err(duduclaw_fork::ForkError::Executor("transport reset".into()))
        }
    }


    /// Judge stub that records the `criteria` string it was called with.
    /// H9-G goal contract freeze: distinguishes "the judge read the frozen
    /// baseline" from "the judge read the mutable field" — the two tests
    /// below deliberately diverge them.
    struct CriteriaCapturingJudge {
        outcome: Result<AcceptanceVerdict, String>,
        captured_criteria: std::sync::Mutex<Option<String>>,
    }

    #[async_trait]
    impl AcceptanceJudge for CriteriaCapturingJudge {
        async fn judge(
            &self,
            criteria: &str,
            _task: &str,
            _result: &str,
        ) -> Result<AcceptanceVerdict, String> {
            *self.captured_criteria.lock().unwrap() = Some(criteria.to_string());
            self.outcome.clone()
        }
    }


    // ── WP-5D: the judge seam, end to end through `review_goal_tasks` ────
    //
    // `judge_mode.rs`'s own unit tests cover parsing, the external subprocess
    // contract, and feedback sanitization in isolation. These drive the real
    // review loop so the *routing* is proven: which implementation is asked,
    // whether the MAV panel was consulted, and where each failure lands.

    /// Write `<home>/config.toml` with a `[dispatch]` body.
    fn write_dispatch_config(home: &std::path::Path, body: &str) {
        std::fs::write(home.join("config.toml"), format!("[dispatch]\n{body}\n")).unwrap();
    }

    /// Engine wired with home dir (so config is read), an accepting counting
    /// judge, and an evaluator returning `candidate_complete`.
    async fn seam_engine(
        home: &std::path::Path,
        store: Arc<TaskStore>,
        evaluator_outcome: Result<PreEvaluation, String>,
    ) -> (
        DispatchEngine,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<StubPreEvaluator>,
    ) {
        let (judge, judge_calls) = accepting_counting_judge();
        let evaluator = StubPreEvaluator::new(evaluator_outcome);
        let engine = DispatchEngine::new(store, Some(judge))
            .with_evaluator(evaluator.clone())
            .with_home_dir(home.to_path_buf());
        (engine, judge_calls, evaluator)
    }


    // ── ③ external: success / timeout degrade / bad-JSON degrade ─────────
    //
    // Unix-only: these need a real spawnable test double. The production path
    // itself is platform-neutral (`tokio::process::Command`); what is
    // Unix-specific is the `#!/bin/sh` stand-in, not the code under test.

    #[cfg(unix)]
    fn judge_script(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh\n{body}").unwrap();
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

