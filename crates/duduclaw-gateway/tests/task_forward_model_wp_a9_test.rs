//! WP-A9 integration test (design
//! `commercial/docs/design-task-forward-model-2026-08-06.md` §9, WP-A10's
//! first half): drives the goal-loop dispatch (predict hook) and the
//! dispatch-engine review (settle hook) together — mirroring exactly how
//! `server.rs` / `handlers.rs` wire the SAME `Arc<TaskForwardModel>` into
//! both `GoalLoopDriver` and `DispatchEngine` in production — through one
//! goal task's three rounds (reject, reject, accept) and asserts:
//!
//! 1. `enabled = true` (a `TaskForwardModel` wired into both hooks):
//!    `task_prediction_log` picks up exactly 3 rows, all settled with
//!    `fidelity = 'mcp_only'` (the "primary branch, not an edge case" per
//!    design §8.2 — `tool_calls.jsonl` evidence is provided but no native
//!    per-runtime collector exists yet).
//! 2. `enabled = false` (no `TaskForwardModel` constructed at all — the
//!    real production gate, see `server.rs`'s `[task_forward_model]
//!    enabled` check): zero `task_prediction_log` rows, AND the entire
//!    observable dispatch/review outcome (final task status, judge
//!    feedback, and every dispatched message payload) is byte-identical to
//!    the `enabled = true` run — proving the two hooks are complete no-ops
//!    when disabled, not merely "inert but still doing work".
//!
//! No live LLM: the acceptance judge is a scripted stub (reject, reject,
//! accept) — the same pattern `dispatch_engine.rs`'s own `StubJudge` /
//! `CountingJudge` test helpers use.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use duduclaw_gateway::dispatch_engine::{AcceptanceJudge, AcceptanceVerdict, DispatchEngine};
use duduclaw_gateway::goal_loop::{GoalLoopConfig, GoalLoopDriver};
use duduclaw_gateway::message_queue::MessageQueue;
use duduclaw_gateway::prediction::task_forward_store::TaskForwardModel;
use duduclaw_gateway::task_store::{TaskRow, TaskStore};

const AGENT_ID: &str = "agent1";
const CLAIMED_AT: &str = "2020-01-01T00:00:00Z"; // safely in the past for any real test clock

/// Scripted judge: pops one verdict per call, in order. Calling it more
/// times than scripted is a test-authoring bug — panics loudly rather than
/// silently repeating, so a miscounted round shows up immediately.
struct ScriptedJudge {
    script: Mutex<Vec<Result<AcceptanceVerdict, String>>>,
}

impl ScriptedJudge {
    fn new(script: Vec<Result<AcceptanceVerdict, String>>) -> Arc<Self> {
        Arc::new(Self { script: Mutex::new(script) })
    }
}

#[async_trait]
impl AcceptanceJudge for ScriptedJudge {
    async fn judge(
        &self,
        _criteria: &str,
        _task: &str,
        _result: &str,
    ) -> Result<AcceptanceVerdict, String> {
        let mut script = self.script.lock().unwrap();
        assert!(!script.is_empty(), "judge called more times than scripted");
        script.remove(0)
    }
}

fn goal_task(id: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        format!("goal {id}"),
        "implement the widget end to end".into(),
        "medium".into(),
        AGENT_ID.into(),
        "system".into(),
    );
    // `atomic_claim` (called below to simulate the agent picking the task
    // up) only accepts `status IN ('pending', 'revising')` — start in
    // `pending`, not `todo` (goal_loop.rs's OWN tests use `todo` but never
    // drive a real claim, they short-circuit straight to `status=review`
    // via `store.update_task`; this test drives the real claim path, so it
    // needs the status `atomic_claim` actually accepts — matches
    // `dispatch_engine.rs`'s own `pending_goal()` test helper convention).
    t.status = "pending".into();
    t.goal_mode = true;
    t.max_retries = 5; // survive 2 rejections without self-escalating
    t.acceptance_criteria = Some("must be correct".into());
    t
}

fn small_cfg() -> GoalLoopConfig {
    GoalLoopConfig {
        iteration_cap: 10,
        iteration_cap_simple: 10,
        soft_cap: 10,
        wall_clock_hours: 24,
        max_concurrent: 3,
        tick_secs: 30,
        stalled_secs: 600,
        // H22: no-progress notice off in fixtures — this suite exercises the
        // A3 forward-model hooks, not the timeout report.
        progress_report_minutes: 0,
        resume_on_restart: "auto".to_string(),
        tool_streak_advisory: true,
    }
}

/// Grounding pre-check off, so its zero-LLM gate never interferes with the
/// scripted judge sequencing (the test cares about the A3 hooks, not
/// GroundEval) — the WP2.4 deterministic outcome-spec gate is already
/// inert here since the test task carries no `outcome:` tag.
fn write_config_disabling_grounding(home: &Path) {
    std::fs::write(
        home.join("config.toml"),
        "[dispatch]\ngrounding_precheck_enabled = false\n",
    )
    .unwrap();
}

/// A `tool_calls.jsonl` line inside the `[CLAIMED_AT, now]` window every
/// round shares (fixed far-past `since`, real `until`) — this is what gives
/// the A3 settle hook `fidelity = McpOnly` evidence (design §5.3: the only
/// source this version reads).
fn write_tool_calls_evidence(home: &Path) {
    std::fs::write(
        home.join("tool_calls.jsonl"),
        concat!(
            "{\"timestamp\":\"2020-06-01T00:00:00Z\",\"agent_id\":\"agent1\",\"tool_name\":\"tasks_complete\",\"success\":true}\n",
            "{\"timestamp\":\"2020-06-01T00:01:00Z\",\"agent_id\":\"agent1\",\"tool_name\":\"memory_search\",\"success\":true}\n",
        ),
    )
    .unwrap();
}

fn count_prediction_log_rows(db_path: &Path) -> i64 {
    let Ok(conn) = rusqlite::Connection::open(db_path) else {
        return 0;
    };
    conn.query_row("SELECT COUNT(*) FROM task_prediction_log", [], |r| r.get(0))
        .unwrap_or(0)
}

fn settled_fidelities(db_path: &Path) -> Vec<Option<String>> {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT fidelity FROM task_prediction_log ORDER BY round")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// Outcome of one full scenario run, restricted to the fields that must be
/// identical regardless of whether the A3 forward model is wired.
#[derive(Debug, PartialEq)]
struct ScenarioOutcome {
    final_status: String,
    final_judge_feedback: Option<String>,
    dispatched: Vec<(String, String)>, // (target, payload)
}

/// Drive the task through 3 rounds (reject, reject, accept), sharing one
/// `TaskForwardModel` between the goal-loop driver's predict hook and the
/// dispatch engine's settle hook exactly the way `server.rs` /
/// `handlers.rs` wire the same `Arc` in production (see
/// `MethodHandler::forward_model`'s doc comment for why that sharing
/// matters — two independently-constructed models would never see each
/// other's settled buckets).
async fn run_scenario(dir: &Path, forward_model: Option<Arc<TaskForwardModel>>) -> ScenarioOutcome {
    let store = Arc::new(TaskStore::open(dir).unwrap());
    let queue = Arc::new(MessageQueue::open(dir).unwrap());
    store.insert_task(&goal_task("g1")).await.unwrap();

    let mut goal_driver = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg());
    if let Some(fm) = &forward_model {
        goal_driver = goal_driver.with_forward_model(fm.clone());
    }

    let judge = ScriptedJudge::new(vec![
        Ok(AcceptanceVerdict { passed: false, feedback: "round 1 not good enough".into(), aspects: None }),
        Ok(AcceptanceVerdict { passed: false, feedback: "round 2 still missing X".into(), aspects: None }),
        Ok(AcceptanceVerdict { passed: true, feedback: "round 3 accepted".into(), aspects: None }),
    ]);
    let mut dispatch_engine = DispatchEngine::new(store.clone(), Some(judge)).with_home_dir(dir.to_path_buf());
    if let Some(fm) = &forward_model {
        dispatch_engine = dispatch_engine.with_forward_model(fm.clone());
    }

    for _round in 0..3 {
        // 1) Predict hook: driver dispatches the task (fresh, or a rejection
        //    retry — `is_rejection_redispatch` covers the latter).
        goal_driver.tick_once().await.unwrap();

        // 2) Simulate the agent: claim + complete synchronously (no live CLI).
        store
            .atomic_claim("g1", AGENT_ID, CLAIMED_AT, "2020-01-01T01:00:00Z")
            .await
            .unwrap();
        store.complete_task("g1", "did the work", AGENT_ID).await.unwrap();

        // 3) Reconcile tick: the driver observes `status=review` and flips
        //    `awaiting_pickup=false` so the NEXT tick's rejection-redispatch
        //    phase computation (Retry vs Restall) is correct — mirrors
        //    `goal_loop.rs`'s own `agent_round_then_reject` test helper.
        goal_driver.tick_once().await.unwrap();

        // 4) Settle hook: dispatch engine reviews (scripted judge verdict).
        dispatch_engine.tick_once().await.unwrap();
    }

    let task = store.get_task("g1").await.unwrap().unwrap();
    let mut dispatched: Vec<(String, String)> = queue
        .pending_messages(100)
        .await
        .unwrap()
        .into_iter()
        .map(|m| (m.target, m.payload))
        .collect();
    dispatched.sort();

    ScenarioOutcome {
        final_status: task.status,
        final_judge_feedback: task.judge_feedback,
        dispatched,
    }
}

#[tokio::test]
async fn enabled_true_settles_three_rounds_with_mcp_only_fidelity() {
    let dir = tempfile::tempdir().unwrap();
    write_config_disabling_grounding(dir.path());
    write_tool_calls_evidence(dir.path());

    let db_path = dir.path().join("prediction.db");
    let forward_model = Arc::new(TaskForwardModel::new(db_path.clone()));

    let outcome = run_scenario(dir.path(), Some(forward_model)).await;

    assert_eq!(outcome.final_status, "done", "round 3's scripted accept must land");
    assert_eq!(outcome.final_judge_feedback.as_deref(), Some("round 3 accepted"));

    assert_eq!(
        count_prediction_log_rows(&db_path),
        3,
        "one task_prediction_log row per round (predict hook fired 3 times)"
    );
    let fidelities = settled_fidelities(&db_path);
    assert_eq!(fidelities.len(), 3);
    for f in &fidelities {
        assert_eq!(
            f.as_deref(),
            Some("mcp_only"),
            "every round settled with McpOnly fidelity (tool_calls.jsonl evidence was provided): {fidelities:?}"
        );
    }
}

#[tokio::test]
async fn enabled_false_writes_zero_rows_and_is_byte_identical_otherwise() {
    // Two SEPARATE temp dirs / stores — the point is not "the same task
    // mutated twice" (that would let round 2's real-world side effects like
    // task status leak between runs) but "the same SCRIPTED scenario,
    // replayed once with the A3 hooks wired and once without, produces the
    // same observable dispatch/review behavior".
    let dir_enabled = tempfile::tempdir().unwrap();
    write_config_disabling_grounding(dir_enabled.path());
    write_tool_calls_evidence(dir_enabled.path());
    let db_path = dir_enabled.path().join("prediction.db");
    let forward_model = Arc::new(TaskForwardModel::new(db_path.clone()));
    let enabled_outcome = run_scenario(dir_enabled.path(), Some(forward_model)).await;

    let dir_disabled = tempfile::tempdir().unwrap();
    write_config_disabling_grounding(dir_disabled.path());
    write_tool_calls_evidence(dir_disabled.path());
    // The production gate: `enabled = false` means `server.rs` never
    // constructs a `TaskForwardModel` at all — reproduced here by simply
    // passing `None` through both builders, exactly like
    // `respawn_goal_loop_driver`'s `if let Some(fm) = self.forward_model().await`
    // no-ops when nothing was injected.
    let disabled_outcome = run_scenario(dir_disabled.path(), None).await;

    assert_eq!(
        count_prediction_log_rows(&dir_disabled.path().join("prediction.db")),
        0,
        "disabled ⇒ no prediction.db task_prediction_log table is even populated"
    );

    assert_eq!(
        enabled_outcome.final_status, disabled_outcome.final_status,
        "final task status must be identical regardless of the A3 toggle"
    );
    assert_eq!(
        enabled_outcome.final_judge_feedback, disabled_outcome.final_judge_feedback,
        "judge feedback must be identical regardless of the A3 toggle"
    );
    assert_eq!(
        enabled_outcome.dispatched, disabled_outcome.dispatched,
        "every dispatched message (target + payload) must be byte-identical — the predict/settle \
         hooks must never perturb the dispatch payload or the review verdict (design §7.3)"
    );
}
