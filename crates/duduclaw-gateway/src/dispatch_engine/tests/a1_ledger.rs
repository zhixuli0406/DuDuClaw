//! A1 ledger completeness (2026-09-30): every settle ending seals its round
//! with a verdict, the two-stage evaluator's verdict is recorded, and the
//! accepted round keeps its own output excerpt. Task-state outcomes are the
//! ones the pre-A1 tests already pin; these cases only look at the ledger.

use super::*;

async fn only_round(store: &TaskStore, id: &str) -> crate::task_store::TaskIterationRow {
    let iters = store.list_iterations(id).await.unwrap();
    assert_eq!(iters.len(), 1, "exactly one round row: {iters:?}");
    iters.into_iter().next().unwrap()
}

#[tokio::test]
async fn a1_evaluator_blocked_round_is_sealed_escalated() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    seed_review(&store, "a1b").await;

    let (judge, _) = accepting_counting_judge();
    let evaluator = StubPreEvaluator::new(Ok(pre_eval(PreDecision::Blocked, Some("no_access"))));
    let engine = DispatchEngine::new(store.clone(), Some(judge)).with_evaluator(evaluator);
    engine.tick_once().await.unwrap();

    assert_eq!(
        store.get_task("a1b").await.unwrap().unwrap().status,
        "needs_human"
    );
    let it = only_round(&store, "a1b").await;
    assert_eq!(it.verdict.as_deref(), Some("escalated"));
    assert!(it.judged_at.is_some());
    assert_eq!(it.pause_reason.as_deref(), Some("blocked_needs_decision"));
    assert_eq!(it.evaluator_verdict.as_deref(), Some("blocked"));
    // No judge ruled: the round must not feed `excluded_approaches`.
    assert!(it.judge_feedback.is_none());
    assert_eq!(it.worker_excerpt.as_deref(), Some("my result"));
}

#[tokio::test]
async fn a1_judge_error_round_is_sealed_escalated() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    seed_review(&store, "a1e").await;

    let judge: Arc<dyn AcceptanceJudge> = Arc::new(LlmAcceptanceJudge::new(ErrCaller));
    let engine = DispatchEngine::new(store.clone(), Some(judge));
    engine.tick_once().await.unwrap();

    assert_eq!(
        store.get_task("a1e").await.unwrap().unwrap().status,
        "needs_human"
    );
    let it = only_round(&store, "a1e").await;
    assert_eq!(it.verdict.as_deref(), Some("escalated"));
    assert_eq!(it.pause_reason.as_deref(), Some("infra"));
    assert!(it.judge_feedback.is_none());
    // No evaluator wired ⇒ no evaluator verdict.
    assert!(it.evaluator_verdict.is_none());
}

#[tokio::test]
async fn a1_human_only_round_is_sealed_escalated() {
    let dir = tempfile::tempdir().unwrap();
    write_dispatch_config(dir.path(), "judge = \"human_only\"");
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    seed_review(&store, "a1h").await;

    let (engine, _, _) = seam_engine(
        dir.path(),
        store.clone(),
        Ok(pre_eval(PreDecision::CandidateComplete, None)),
    )
    .await;
    engine.tick_once().await.unwrap();

    assert_eq!(
        store.get_task("a1h").await.unwrap().unwrap().status,
        "needs_human"
    );
    let it = only_round(&store, "a1h").await;
    assert_eq!(it.verdict.as_deref(), Some("escalated"));
    assert_eq!(it.pause_reason.as_deref(), Some("blocked_needs_decision"));
    // human_only parks before the evaluator runs.
    assert!(it.evaluator_verdict.is_none());
    // A wired home dir ⇒ the knob snapshot is sealed with the verdict.
    let knobs: serde_json::Value =
        serde_json::from_str(it.knobs_json.as_deref().expect("knobs_json")).unwrap();
    assert!(knobs.get("iteration_cap").is_some(), "{knobs}");
}

#[tokio::test]
async fn a1_accepted_round_keeps_worker_excerpt_and_evaluator_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    seed_review(&store, "a1a").await;

    let (judge, _) = accepting_counting_judge();
    let evaluator = StubPreEvaluator::new(Ok(pre_eval(PreDecision::CandidateComplete, None)));
    let engine = DispatchEngine::new(store.clone(), Some(judge)).with_evaluator(evaluator);
    engine.tick_once().await.unwrap();

    assert_eq!(store.get_task("a1a").await.unwrap().unwrap().status, "done");
    let it = only_round(&store, "a1a").await;
    assert_eq!(it.verdict.as_deref(), Some("accepted"));
    assert_eq!(it.worker_excerpt.as_deref(), Some("my result"));
    assert_eq!(it.evaluator_verdict.as_deref(), Some("candidate_complete"));
    assert!(it.pause_reason.is_none());
}

#[tokio::test]
async fn a1_continue_round_records_evaluator_verdict_on_the_rejected_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    seed_review(&store, "a1c").await;

    let (judge, _) = accepting_counting_judge();
    let evaluator = StubPreEvaluator::new(Ok(pre_eval(PreDecision::Continue, None)));
    let engine = DispatchEngine::new(store.clone(), Some(judge)).with_evaluator(evaluator);
    engine.tick_once().await.unwrap();

    let it = only_round(&store, "a1c").await;
    assert_eq!(it.verdict.as_deref(), Some("rejected"));
    assert_eq!(it.evaluator_verdict.as_deref(), Some("continue"));
}

#[tokio::test]
async fn a1_evaluator_error_leaves_evaluator_verdict_null() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    seed_review(&store, "a1x").await;

    let (judge, _) = accepting_counting_judge();
    let evaluator = StubPreEvaluator::new(Err("evaluator down".into()));
    let engine = DispatchEngine::new(store.clone(), Some(judge)).with_evaluator(evaluator);
    engine.tick_once().await.unwrap();

    let it = only_round(&store, "a1x").await;
    assert_eq!(it.verdict.as_deref(), Some("accepted"), "degrades to the panel");
    assert!(it.evaluator_verdict.is_none());
}

/// Exercise both production judge adapters through the engine and the actual
/// SQLite recorder. The caller is deterministic, so this needs no live API
/// account and never substitutes a guessed role for nullable attribution.
struct ReviewUsageCaller {
    telemetry: Arc<crate::cost_telemetry::CostTelemetry>,
    agent: &'static str,
    reply: &'static str,
}

#[async_trait]
impl duduclaw_fork::judge::LlmCaller for ReviewUsageCaller {
    async fn complete(&self, _prompt: &str) -> duduclaw_fork::Result<String> {
        // Cross an await boundary just as the production utility caller does.
        tokio::task::yield_now().await;
        self.telemetry.record(
            self.agent, crate::cost_telemetry::RequestType::Dispatch,
            "claude-sonnet-4-6", &crate::cost_telemetry::TokenUsage {
                input_tokens: 37, output_tokens: 11,
                cache_read_tokens: 0, cache_creation_tokens: 0,
            },
        ).await;
        Ok(self.reply.to_string())
    }
}

#[tokio::test]
async fn a1_review_llm_callers_record_current_iteration_without_scope_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    let cost_path = dir.path().join("review_cost.db");
    let telemetry = Arc::new(crate::cost_telemetry::CostTelemetry::new(&cost_path).unwrap());

    for (id, revision, ledger_round) in [("cost-first", 0, 1), ("cost-other", 2, 7)] {
        let mut task = pending_goal(id);
        task.revision_round = revision;
        task.max_retries = 20;
        store.insert_task(&task).await.unwrap();
        // A repaired ledger can differ from the task counter. Review seals
        // the latest open row, so that row is the attribution authority.
        store.record_iteration_dispatch(id, ledger_round, "2026-09-30T10:00:00Z").await.unwrap();
        assert!(store.atomic_claim(id, "worker", "2026-09-30T10:00:00Z", "2026-09-30T10:05:00Z")
            .await.unwrap().is_claimed());
        store.complete_task(id, "delivered result", "worker").await.unwrap();
    }

    let judge = LlmAcceptanceJudge::new(ReviewUsageCaller {
        telemetry: telemetry.clone(), agent: "review-panel", reply: "FAIL\nmissing evidence",
    });
    let evaluator = LlmPreEvaluator::new(ReviewUsageCaller {
        telemetry: telemetry.clone(), agent: "review-evaluator",
        reply: r#"{"decision":"candidate_complete","evidence":"delivered result","next_step":"check the evidence","blocker_key":null}"#,
    });
    let engine = DispatchEngine::new(store.clone(), Some(Arc::new(judge)))
        .with_evaluator(Arc::new(evaluator));
    engine.review_goal_tasks().await.unwrap();

    // Rejection advances the task counter, but the already measured calls
    // must retain the round they judged. The next review gets a fresh scope.
    assert_eq!(store.get_task("cost-first").await.unwrap().unwrap().revision_round, 1);
    assert_eq!(store.list_iterations("cost-other").await.unwrap()[0].round, 7);
    store.record_iteration_dispatch("cost-first", 2, "2026-09-30T11:00:00Z").await.unwrap();
    assert!(store.atomic_claim("cost-first", "worker", "2026-09-30T11:00:00Z", "2026-09-30T11:05:00Z")
        .await.unwrap().is_claimed());
    store.complete_task("cost-first", "second delivered result", "worker").await.unwrap();
    engine.review_goal_tasks().await.unwrap();

    // A later, unrelated use of the same caller must stay unattributed.
    duduclaw_fork::judge::LlmCaller::complete(&ReviewUsageCaller {
        telemetry, agent: "unrelated", reply: "PASS",
    }, "outside review").await.unwrap();
    let conn = rusqlite::Connection::open(cost_path).unwrap();
    let rows: Vec<(String, Option<String>, Option<i64>, Option<String>, i64, i64)> = conn
        .prepare("SELECT agent_id,episode_id,round,role,input_tokens,output_tokens FROM token_usage ORDER BY id")
        .unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(rows.len(), 7, "two paid stages per reviewed iteration, plus unrelated call");
    for (id, round) in [("cost-first", 1), ("cost-other", 7), ("cost-first", 2)] {
        let attributed: Vec<_> = rows.iter().filter(|r| r.1.as_deref() == Some(id) && r.2 == Some(round)).collect();
        assert_eq!(attributed.len(), 2, "{id} round {round}: {rows:?}");
        assert!(attributed.iter().any(|r| r.0 == "review-panel"));
        assert!(attributed.iter().any(|r| r.0 == "review-evaluator"));
    }
    assert!(rows.iter().all(|r| r.3.is_none() && r.4 == 37 && r.5 == 11));
    assert_eq!(rows.last().unwrap().1, None);
    assert_eq!(rows.last().unwrap().2, None);
}
