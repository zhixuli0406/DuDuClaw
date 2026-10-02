//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs` — kickoff cases.

use super::*;

// ── H6: resume_on_restart ──────────────────────────────────

/// WP-E: direct check that `GoalLoopConfig::default()` resolves to
/// `Pause` — the platform default flip, independent of the
/// boot-reconciliation behavior exercised by the tests below.
#[test]
fn goal_loop_config_default_resume_on_restart_is_pause() {
    assert_eq!(
        GoalLoopConfig::default().resume_on_restart(),
        ResumeOnRestart::Pause
    );
    assert_eq!(GoalLoopConfig::default().resume_on_restart, "pause");
}

#[tokio::test]
async fn resume_on_restart_auto_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    std::fs::write(
        dir.path().join("config.toml"),
        "[goal_loop]\nresume_on_restart = \"auto\"\n",
    )
    .unwrap();

    let mut t = goal_task("g1", "alice");
    t.status = "in_progress".into();
    store.insert_task(&t).await.unwrap();

    let paused = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
    assert_eq!(paused, 0, "explicit auto must never touch any task");
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(
        got.status, "in_progress",
        "task status must be untouched under auto"
    );
}

// WP-E (2026-08 P1 rollout, user-approved spec change — see
// `GoalLoopConfig::default`): the platform default for
// `resume_on_restart` flipped from "auto" to "pause". This test used to
// be named `resume_on_restart_default_config_is_a_noop` and assert the
// opposite (paused == 0); it is intentionally rewritten, not just
// relabeled, to lock in the new default as a spec change rather than
// silently deleting coverage of "what happens with no config.toml at all".
#[tokio::test]
async fn resume_on_restart_default_config_pauses_inflight_tasks() {
    // No config.toml at all ⇒ GoalLoopConfig::from_home defaults ⇒ Pause
    // (WP-E default, was Auto pre-WP-E).
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let mut t = goal_task("g1", "alice");
    t.status = "review".into();
    store.insert_task(&t).await.unwrap();

    let paused = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
    assert_eq!(
        paused, 1,
        "missing config.toml must default to pause (WP-E default) and escalate the in-flight task"
    );
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(
        got.status, "needs_human",
        "default pause must escalate the in-flight task at boot"
    );
}

#[tokio::test]
async fn resume_on_restart_pause_escalates_inflight_goal_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    std::fs::write(
        dir.path().join("config.toml"),
        "[goal_loop]\nresume_on_restart = \"pause\"\n",
    )
    .unwrap();

    // Non-terminal goal_mode tasks across a few different statuses.
    let mut t1 = goal_task("g1", "alice");
    t1.status = "in_progress".into();
    store.insert_task(&t1).await.unwrap();

    let mut t2 = goal_task("g2", "alice");
    t2.status = "review".into();
    store.insert_task(&t2).await.unwrap();

    let mut t3 = goal_task("g3", "alice"); // still "todo" — never dispatched, must NOT be paused
    store.insert_task(&t3).await.unwrap();

    // A terminal task must NOT be touched.
    let mut t4 = goal_task("g4", "alice");
    t4.status = "done".into();
    store.insert_task(&t4).await.unwrap();

    // A non-goal-mode task in a matching status must NOT be touched.
    let mut t5 = TaskRow::new(
        "t5".into(),
        "ordinary task".into(),
        "not a goal".into(),
        "medium".into(),
        "alice".into(),
        "system".into(),
    );
    t5.status = "in_progress".into();
    store.insert_task(&t5).await.unwrap();

    let paused = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
    assert_eq!(
        paused, 2,
        "exactly the 2 genuinely in-flight goal_mode tasks must be paused"
    );

    for id in ["g1", "g2"] {
        let got = store.get_task(id).await.unwrap().unwrap();
        assert_eq!(got.status, "needs_human", "{id} must be escalated");
        assert_eq!(got.judge_feedback.as_deref(), Some("gateway_restart"));
    }
    // A queued goal the user confirmed but that never dispatched is NOT
    // "still running" — it must survive the boot scan untouched and
    // dispatch normally afterwards (live-verification catch, 2026-08-15).
    assert_eq!(
        store.get_task("g3").await.unwrap().unwrap().status,
        "todo",
        "queued-but-never-dispatched goal must not be paused"
    );
    assert_eq!(
        store.get_task("g4").await.unwrap().unwrap().status,
        "done",
        "terminal task untouched"
    );
    assert_eq!(
        store.get_task("t5").await.unwrap().unwrap().status,
        "in_progress",
        "non-goal-mode task untouched"
    );
}

#[tokio::test]
async fn resume_on_restart_pause_is_idempotent_across_two_boots() {
    // A second "boot" (e.g. a crash loop) must not re-escalate an
    // already-`needs_human` task, and must not error.
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    std::fs::write(
        dir.path().join("config.toml"),
        "[goal_loop]\nresume_on_restart = \"pause\"\n",
    )
    .unwrap();

    let mut t = goal_task("g1", "alice");
    t.status = "in_progress".into();
    store.insert_task(&t).await.unwrap();

    let first = pause_inflight_on_restart(store.clone(), queue.clone(), dir.path()).await;
    assert_eq!(first, 1);
    let second = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
    assert_eq!(
        second, 0,
        "an already needs_human task must not be re-escalated"
    );
}

/// FX5: the boot-time pause records `restart` on the interrupted round's
/// ledger row (via `escalate` → `stamp_iteration_pause`) without inventing a
/// verdict — the round was dispatched and never judged.
#[tokio::test]
async fn resume_on_restart_pause_stamps_restart_on_the_open_round() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    std::fs::write(
        dir.path().join("config.toml"),
        "[goal_loop]\nresume_on_restart = \"pause\"\n",
    )
    .unwrap();

    let mut t = goal_task("g1", "alice");
    t.status = "in_progress".into();
    store.insert_task(&t).await.unwrap();
    store
        .record_iteration_dispatch_with_state("g1", 1, "2026-09-30T00:00:00Z", None, None)
        .await
        .unwrap();

    let paused = pause_inflight_on_restart(store.clone(), queue, dir.path()).await;
    assert_eq!(paused, 1);

    let rows = store.list_iterations("g1").await.unwrap();
    assert_eq!(rows.len(), 1, "no extra row is created by the pause");
    assert_eq!(rows[0].round, 1);
    assert_eq!(rows[0].pause_reason.as_deref(), Some("restart"));
    assert_eq!(rows[0].verdict, None, "a restart is not a verdict");
    assert_eq!(rows[0].judged_at, None, "the round stays open for the retry");
}

// ── H7: continuation feedback is single-instance, not accumulated ──

/// Audit finding (H7): `enqueue_work`'s `<judge_feedback>` block is
/// built from `task.judge_feedback` alone — a single `Option<String>`
/// column that `TaskStore::reject_review`/`accept_review` OVERWRITE on
/// every judge verdict (`SET ... judge_feedback = ?5 ...`, never
/// concatenated — see `task_store.rs`). There was nothing to change;
/// this regression test locks the "already single-instance" finding in
/// so a future edit accidentally reintroducing accumulation (e.g.
/// appending instead of overwriting) trips a red test immediately.
#[tokio::test]
async fn continuation_feedback_never_accumulates_across_rounds() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    let cfg = GoalLoopConfig {
        iteration_cap: 10,
        iteration_cap_simple: 10,
        ..small_cfg()
    };
    let mut t = goal_task("g1", "alice");
    t.max_retries = 100;
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), cfg);
    d.tick_once().await.unwrap();

    agent_round_then_reject(&d, &store, "g1", "round one feedback: fix the widget shape").await;
    d.tick_once().await.unwrap(); // re-dispatch carrying round-one feedback

    agent_round_then_reject(
        &d,
        &store,
        "g1",
        "round two feedback: fix the widget color instead",
    )
    .await;
    d.tick_once().await.unwrap(); // re-dispatch carrying round-two feedback

    let pending = queue.pending_messages(10).await.unwrap();
    let latest = pending.last().expect("at least one pending message");
    // Exactly one `<judge_feedback>` block per payload — never doubled up.
    assert_eq!(latest.payload.matches("<judge_feedback>").count(), 1);
    // Isolate the `<judge_feedback>...</judge_feedback>` block itself —
    // deliberately NOT a whole-payload substring check, because the
    // SEPARATE `<excluded_approaches>` section of the `<state>` block
    // intentionally accumulates up to 6 historical rejection reasons
    // (see `goal_loop/state.rs::excluded_from_iterations`) and legitimately
    // still contains round-one's text there. The H7 claim under test is
    // specifically about the single dedicated continuation-feedback
    // block, not that field.
    let start = latest
        .payload
        .find("<judge_feedback>")
        .expect("judge_feedback block present")
        + "<judge_feedback>".len();
    let end = latest.payload[start..]
        .find("</judge_feedback>")
        .expect("judge_feedback close tag present")
        + start;
    let feedback_block = &latest.payload[start..end];
    assert!(
        feedback_block.contains("round two feedback: fix the widget color instead"),
        "the <judge_feedback> block must carry the latest feedback"
    );
    assert!(
        !feedback_block.contains("round one feedback: fix the widget shape"),
        "the <judge_feedback> block must NOT still carry the stale round-one text \
             (that is the H7 single-instance claim under test) — got: {feedback_block:?}"
    );
}

// ── Iterative Kanban: revising re-dispatch + cap ordering ──

#[tokio::test]
async fn revising_task_is_re_dispatched_and_opens_new_round() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    // A task the judge already rejected once → revising, round 1.
    let mut t = goal_task("g1", "alice");
    t.status = "revising".into();
    t.revision_round = 1;
    t.judge_feedback = Some("add the missing section".into());
    store.insert_task(&t).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    // Re-dispatched with feedback, and iteration round 2 opened (round =
    // revision_round + 1).
    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].payload.contains("add the missing section"));
    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters.len(), 1);
    assert_eq!(
        iters[0].round, 2,
        "revising re-dispatch opens round revision_round+1"
    );
}

/// The A2 no-progress guard must win over the iteration cap when BOTH
/// would fire on the very same tick (the guard runs before the cap
/// check in `tick_once`). M3 lowered the A2 threshold to a 2nd
/// consecutive identical-state dispatch, so `iteration_cap` is set to
/// exactly 2 so that, at the tick where that 2nd identical-state
/// dispatch is evaluated, `current_iter (2) >= cap (2)` is ALSO true —
/// a genuine simultaneous boundary, not merely "the cap happens to be
/// generous enough to never matter".
#[tokio::test]
async fn oscillation_takes_precedence_over_iteration_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    let cfg = GoalLoopConfig {
        iteration_cap: 2,
        iteration_cap_simple: 2,
        ..small_cfg()
    };
    let mut t = goal_task("g1", "alice");
    t.max_retries = 100; // don't let reject_review self-escalate
    store.insert_task(&t).await.unwrap();
    let d = driver(store.clone(), queue.clone(), cfg);

    d.tick_once().await.unwrap(); // dispatch #1 (iter 1, state_hash A)
    agent_round_then_reject(&d, &store, "g1", "same").await;
    d.tick_once().await.unwrap(); // dispatch #2 (iter 2 == cap, state_hash B, streak 1)
    agent_round_then_reject(&d, &store, "g1", "same").await;
    // This tick: current_iter (2) >= cap (2) AND the would-be streak (2)
    // both hold — the A2 guard must fire first.
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "needs_human");
    assert_eq!(
        got.judge_feedback.as_deref(),
        Some("goal-loop no-progress oscillation"),
        "the A2 no-progress guard must win over the iteration-cap reason"
    );
}

#[test]
fn autonomy_level_parses_and_defaults_conservative() {
    assert_eq!(
        AutonomyLevel::from_toml_str("operator"),
        AutonomyLevel::Operator
    );
    assert_eq!(
        AutonomyLevel::from_toml_str("  Collaborator "),
        AutonomyLevel::Collaborator
    );
    assert_eq!(
        AutonomyLevel::from_toml_str("CONSULTANT"),
        AutonomyLevel::Consultant
    );
    assert_eq!(
        AutonomyLevel::from_toml_str("observer"),
        AutonomyLevel::Observer
    );
    // Unknown / empty ⇒ Approver (never the most-autonomous level).
    assert_eq!(AutonomyLevel::from_toml_str("wat"), AutonomyLevel::Approver);
    assert_eq!(AutonomyLevel::from_toml_str(""), AutonomyLevel::Approver);
}

#[test]
fn autonomy_for_agent_reads_toml_and_fails_safe() {
    let dir = tempfile::tempdir().unwrap();
    // Missing agent.toml ⇒ Approver.
    assert_eq!(
        AutonomyLevel::for_agent(dir.path(), "ghost"),
        AutonomyLevel::Approver
    );
    write_agent_toml(
        dir.path(),
        "alice",
        "[capabilities]\nautonomy_level = \"operator\"\n",
    );
    assert_eq!(
        AutonomyLevel::for_agent(dir.path(), "alice"),
        AutonomyLevel::Operator
    );
    // Malformed toml ⇒ Approver (fail-safe).
    write_agent_toml(dir.path(), "bob", "not = valid [[[");
    assert_eq!(
        AutonomyLevel::for_agent(dir.path(), "bob"),
        AutonomyLevel::Approver
    );
}

#[tokio::test]
async fn operator_agent_is_not_auto_dispatched() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    write_agent_toml(
        dir.path(),
        "alice",
        "[capabilities]\nautonomy_level = \"operator\"\n",
    );
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let d = GoalLoopDriver::new(store, queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.tick_once().await.unwrap();

    assert!(
        queue.pending_messages(10).await.unwrap().is_empty(),
        "operator-level agent is never auto-driven"
    );
}

#[tokio::test]
async fn collaborator_kickoff_gates_then_dispatches_on_approve() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    write_agent_toml(
        dir.path(),
        "alice",
        "[capabilities]\nautonomy_level = \"collaborator\"\n",
    );
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let broker = Arc::new(crate::approval::ApprovalBroker::open(dir.path()).unwrap());
    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf())
        .with_broker(broker.clone());

    // Tick 1: kickoff filed, task NOT dispatched.
    d.tick_once().await.unwrap();
    assert!(
        queue.pending_messages(10).await.unwrap().is_empty(),
        "no dispatch before kickoff approval"
    );
    let pending = broker.list_pending(Some("alice")).await.unwrap();
    assert_eq!(pending.len(), 1, "kickoff approval filed");
    assert_eq!(pending[0].action_kind, "goal_kickoff");
    let approval_id = pending[0].id.clone();

    // Human approves → tick 2 dispatches.
    broker
        .decide(&approval_id, true, "test:alice")
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    let dispatched = queue.pending_messages(10).await.unwrap();
    assert_eq!(dispatched.len(), 1, "dispatched after kickoff approval");
    assert_eq!(dispatched[0].target, "alice");
}
