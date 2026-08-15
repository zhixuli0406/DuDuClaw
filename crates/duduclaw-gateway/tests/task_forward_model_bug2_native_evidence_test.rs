//! BUG-2 regression test (WP-A10 §6 復驗,
//! `wiki/reports/memory-quality/2026-08/wp-a10-live-test-2026-08-06.md` §6.4/§6.7):
//! the WP-A4 native-tool collector's evidence for a goal-loop review round
//! used to be consumed ONLY by the A3 settle hook
//! (`dispatch_engine.rs::settle_forward_model`'s own `take_native_evidence`
//! call) — the B3 grounding pre-check and the judge's `<tool_activity>`
//! prompt block never saw it, so honest Read/Write/Bash work still read as
//! "zero tool call evidence" to the acceptance judge even after WP-A4
//! shipped. The fix hoists the (remove-once) `take_native_evidence` call to
//! the top of `review_goal_tasks`'s per-task loop body and shares the SAME
//! evidence across all three consumers.
//!
//! This test drives the real `GoalLoopDriver` (predict) +
//! `DispatchEngine` (settle) pair through one goal task's three rounds —
//! same harness shape as `task_forward_model_wp_a9_test.rs` — but injects
//! WP-A4 native-tool evidence via `task_observe::record_native_evidence`
//! before each round's review (mirroring exactly what `dispatcher.rs`
//! bridges over for a real goal-loop dispatch), and asserts:
//!
//! 1. `task_prediction_log.fidelity == "full"` for every settled round
//!    (proves the reference-passing fix didn't silently degrade A3 back to
//!    `mcp_only` — the exact half-fix the WP-A10 report warned against).
//! 2. The judge's `<tool_activity>` block (captured via the judge's `task`
//!    argument) contains the native events, tagged `(native)`.
//! 3. Grounding never spuriously rejects a round because of the merged
//!    evidence: with `grounding_precheck` left at its default (enabled),
//!    the MCP evidence in this fixture (`tasks_complete` — self-echo,
//!    `memory_search` — no captured `result_text`) plus native evidence
//!    (no `result_text` by construction) can only ever produce `Skip` /
//!    `Degraded`, never `Reject`/`Grounded` — so the scripted judge must
//!    still be invoked, and consume, exactly 3 verdicts.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use duduclaw_gateway::dispatch_engine::{AcceptanceJudge, AcceptanceVerdict, DispatchEngine};
use duduclaw_gateway::goal_loop::{GoalLoopConfig, GoalLoopDriver};
use duduclaw_gateway::message_queue::MessageQueue;
use duduclaw_gateway::prediction::task_forward_store::TaskForwardModel;
use duduclaw_gateway::prediction::task_observe::record_native_evidence;
use duduclaw_gateway::runtime::NativeToolEvent;
use duduclaw_gateway::task_store::{TaskRow, TaskStore};

const AGENT_ID: &str = "agent1";
const CLAIMED_AT: &str = "2020-01-01T00:00:00Z";

/// Scripted-verdict + capturing judge: pops one verdict per call (panics if
/// called more times than scripted — a miscounted round should fail loudly,
/// not silently repeat), and records every `task` argument it was called
/// with so the test can inspect the `<tool_activity>` block folded into it.
struct CapturingJudge {
    script: Mutex<Vec<Result<AcceptanceVerdict, String>>>,
    captured_tasks: Mutex<Vec<String>>,
}

impl CapturingJudge {
    fn new(script: Vec<Result<AcceptanceVerdict, String>>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            captured_tasks: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.captured_tasks.lock().unwrap().len()
    }

    fn captured_tasks(&self) -> Vec<String> {
        self.captured_tasks.lock().unwrap().clone()
    }
}

#[async_trait]
impl AcceptanceJudge for CapturingJudge {
    async fn judge(
        &self,
        _criteria: &str,
        task: &str,
        _result: &str,
    ) -> Result<AcceptanceVerdict, String> {
        self.captured_tasks.lock().unwrap().push(task.to_string());
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
    t.status = "pending".into();
    t.goal_mode = true;
    t.max_retries = 5;
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

/// Same fixture shape as `task_forward_model_wp_a9_test.rs`: `tasks_complete`
/// is on the MCP self-echo deny-list (never captures `result_text`),
/// `memory_search` here simply never had one captured either (today's
/// universal writer gap) — i.e. MCP evidence exists but is unusable for
/// grounding. This is deliberately the SAME shape the live-test report hit
/// (report §6.3): a review window whose only MCP evidence is a self-echo
/// task_board tool.
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

#[tokio::test]
async fn native_evidence_reaches_grounding_and_tool_activity_without_breaking_a3_fidelity() {
    let dir = tempfile::tempdir().unwrap();
    // Grounding pre-check left at its DEFAULT (enabled) — part of what this
    // test proves is that merging native evidence in never turns a
    // Degraded/Skip round into a spurious Reject.
    write_tool_calls_evidence(dir.path());

    let db_path = dir.path().join("prediction.db");
    let forward_model = Arc::new(TaskForwardModel::new(db_path.clone()));

    let store = Arc::new(TaskStore::open(dir.path()).unwrap());
    let queue = Arc::new(MessageQueue::open(dir.path()).unwrap());
    store.insert_task(&goal_task("g1")).await.unwrap();

    let goal_driver = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_forward_model(forward_model.clone());

    let judge = CapturingJudge::new(vec![
        Ok(AcceptanceVerdict { passed: false, feedback: "round 1 not good enough".into(), aspects: None }),
        Ok(AcceptanceVerdict { passed: false, feedback: "round 2 still missing X".into(), aspects: None }),
        Ok(AcceptanceVerdict { passed: true, feedback: "round 3 accepted".into(), aspects: None }),
    ]);
    let dispatch_engine = DispatchEngine::new(store.clone(), Some(judge.clone()))
        .with_home_dir(dir.path().to_path_buf())
        .with_forward_model(forward_model.clone());

    for _round in 0..3 {
        // 1) Predict hook: driver dispatches (fresh, or a rejection retry).
        goal_driver.tick_once().await.unwrap();

        // ── BUG-2 regression: bridge WP-A4 native-tool evidence for THIS
        // round exactly the way `dispatcher.rs` does right after a
        // goal-loop dispatch call returns (see its doc comment) — Read +
        // Write, both successful, neither carrying `result_text` — this
        // fixture deliberately keeps the pre-R1 shape (a producer that
        // hasn't captured any text for this round) so this test still
        // exercises the ①②③ assertions below unchanged; R1's
        // text-bearing-native-evidence path is covered separately in
        // `dispatch_engine.rs`'s
        // `grounding_precheck_native_evidence_with_result_text_reaches_grounded`.
        let task = store.get_task("g1").await.unwrap().unwrap();
        let round = (task.revision_round as u32).saturating_add(1);
        record_native_evidence(
            "g1",
            round,
            vec![
                NativeToolEvent {
                    tool_name: "Read".to_string(),
                    success: true,
                    result_text: None,
                    input_text: None,
                },
                NativeToolEvent {
                    tool_name: "Write".to_string(),
                    success: true,
                    result_text: None,
                    input_text: None,
                },
            ],
        );

        // 2) Simulate the agent: claim + complete synchronously (no live CLI).
        store
            .atomic_claim("g1", AGENT_ID, CLAIMED_AT, "2020-01-01T01:00:00Z")
            .await
            .unwrap();
        store.complete_task("g1", "did the work", AGENT_ID).await.unwrap();

        // 3) Reconcile tick (mirrors task_forward_model_wp_a9_test.rs).
        goal_driver.tick_once().await.unwrap();

        // 4) Settle hook: dispatch engine reviews (scripted judge verdict).
        dispatch_engine.tick_once().await.unwrap();
    }

    let task = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(task.status, "done", "round 3's scripted accept must land");

    // ① A3 fidelity: the reference-passing fix must not have degraded any
    // round from `full` back to `mcp_only` (the exact half-fix risk the
    // WP-A10 report warned about — a `take_native_evidence` call left
    // in TWO places would make the second one always observe `None`).
    let fidelities = settled_fidelities(&db_path);
    assert_eq!(fidelities.len(), 3, "one settled row per round");
    for f in &fidelities {
        assert_eq!(
            f.as_deref(),
            Some("full"),
            "native evidence was bridged every round ⇒ every round must settle at Full fidelity: {fidelities:?}"
        );
    }

    // ② <tool_activity> block: the judge must have SEEN the native evidence,
    // not just MCP records — this is the actual BUG-2 defect (only A3 saw
    // it before the fix). Every captured `task` argument should carry the
    // merged block with the `(native)` tag.
    let captured = judge.captured_tasks();
    assert_eq!(
        judge.calls(),
        3,
        "grounding must never have short-circuited a round before the judge \
         (native-only evidence has no result_text and must stay Degraded/Skip, never Reject)"
    );
    for (i, task_desc) in captured.iter().enumerate() {
        assert!(
            task_desc.contains("<tool_activity>"),
            "round {i}: judge prompt is missing the <tool_activity> block entirely: {task_desc}"
        );
        assert!(
            task_desc.contains("Read (native)"),
            "round {i}: judge prompt's <tool_activity> block is missing native Read evidence: {task_desc}"
        );
        assert!(
            task_desc.contains("Write (native)"),
            "round {i}: judge prompt's <tool_activity> block is missing native Write evidence: {task_desc}"
        );
        // The pre-existing MCP evidence must still be present too — the fix
        // merges, it does not replace.
        assert!(
            task_desc.contains("memory_search"),
            "round {i}: merging native evidence must not drop the pre-existing MCP evidence: {task_desc}"
        );
    }

    // ③ Grounding produced a reasonable (non-blocking) outcome: all 3
    // scripted verdicts were consumed (see judge.calls() assertion above),
    // and the final status/feedback reflect the SCRIPTED judge's own
    // verdicts, not a B3 grounding short-circuit substituting its own
    // rejection feedback.
    assert_eq!(task.judge_feedback.as_deref(), Some("round 3 accepted"));
}
