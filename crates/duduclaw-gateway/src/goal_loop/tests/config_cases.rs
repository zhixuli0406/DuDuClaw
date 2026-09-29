//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs` — config cases.

use super::*;

#[test]
fn default_direction_autonomy_level_defaults_to_approver_never_operator() {
    for body in [
        "",                                               // empty file
        "[capabilities]\n",                               // section, no key
        "[capabilities]\ncomputer_use = true\n",          // sibling only
        "[capabilities]\nautonomy_level = \"oprator\"\n", // typo
        "[capabilities]\nautonomy_level = \"\"\n",        // blank
        "[capabilities]\nautonomy_level = 3\n",           // wrong type
        "capabilities = \"scalar\"\n",                    // wrong-typed section
        "not toml [[[",                                   // malformed file
    ] {
        let home = home_with_agent("a", body);
        assert_eq!(
            AutonomyLevel::for_agent(home.path(), "a"),
            AutonomyLevel::Approver,
            "for {body:?}"
        );
    }

    // Missing agent directory entirely — same direction.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        AutonomyLevel::for_agent(empty.path(), "nope"),
        AutonomyLevel::Approver
    );
}

#[test]
fn default_direction_autonomy_level_recognised_values_still_apply() {
    for (raw, want) in [
        ("operator", AutonomyLevel::Operator),
        ("Collaborator", AutonomyLevel::Collaborator), // case-insensitive
        (" consultant ", AutonomyLevel::Consultant),   // trimmed
        ("observer", AutonomyLevel::Observer),
    ] {
        let home = home_with_agent(
            "a",
            &format!("[capabilities]\nautonomy_level = \"{raw}\"\n"),
        );
        assert_eq!(
            AutonomyLevel::for_agent(home.path(), "a"),
            want,
            "for {raw:?}"
        );
    }
}

/// Regression (review `goal_loop.rs:2486`): when another path froze the
/// spec first, `freeze_for_task` returns `AlreadyFrozen` and writes
/// nothing. The old code only matched `Frozen(..)`, so the local clone
/// kept `team_spec_json = None`, `frozen_spec(&task)?` bailed, and that
/// one round silently ran Solo while every later round ran as a Team.
#[tokio::test]
async fn regression_an_already_frozen_spec_is_backfilled_onto_the_local_row() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_enabled_team(home);
    let (store, queue) = open_stores(home).await;
    let mut row = goal_task("g-frozen", "alice");
    store.insert_task(&row).await.unwrap();

    // Another path (e.g. `tasks.goal_create`) gets there first.
    let first =
        crate::team_composer::freeze_for_task(home, &store, &row.id, &row.assigned_to).await;
    assert!(
        matches!(first, crate::team_composer::FreezeOutcome::Frozen(_)),
        "{first:?}"
    );
    assert!(
        row.team_spec_json.is_none(),
        "our local clone is deliberately the stale one"
    );

    let d = driver(store.clone(), queue, small_cfg()).with_home_dir(home.to_path_buf());
    d.ensure_frozen_team_spec(&mut row).await;

    let stored = store.get_task(&row.id).await.unwrap().unwrap();
    assert_eq!(
        row.team_spec_json, stored.team_spec_json,
        "the local row must end up holding the spec the store actually has"
    );
    assert!(
        crate::team_composer::frozen_spec(&row).is_some(),
        "and that spec must parse — otherwise the round still runs Solo"
    );
}

/// The same helper must stay a no-op for the cases it always was: a task
/// that already carries a spec, and a task past its first round.
#[tokio::test]
async fn the_freeze_backfill_never_touches_a_later_round_or_an_existing_spec() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_enabled_team(home);
    let (store, queue) = open_stores(home).await;
    let d = driver(store.clone(), queue, small_cfg()).with_home_dir(home.to_path_buf());

    let mut later = goal_task("g-later", "alice");
    later.revision_round = 2;
    store.insert_task(&later).await.unwrap();
    d.ensure_frozen_team_spec(&mut later).await;
    assert!(later.team_spec_json.is_none(), "round 2 never freezes");

    let mut already = goal_task("g-has", "alice");
    already.team_spec_json = Some("{\"already\":true}".into());
    d.ensure_frozen_team_spec(&mut already).await;
    assert_eq!(already.team_spec_json.as_deref(), Some("{\"already\":true}"));
}

#[tokio::test]
async fn with_no_team_section_the_round_dispatches_on_the_message_queue_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());

    d.tick_once().await.unwrap();

    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "the Solo path still enqueues a work message"
    );
    assert!(pending[0].payload.contains("tasks_claim"));
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert!(
        t.team_spec_json.is_none(),
        "no [team] section must freeze nothing at all"
    );
}

#[tokio::test]
async fn an_enabled_team_whose_gate_says_solo_still_dispatches_on_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    write_enabled_team(dir.path());
    let (store, queue) = open_stores(dir.path()).await;
    // One acceptance criterion, no artifact words ⇒ at most one signal ⇒
    // the gate's `insufficient_signals` branch.
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());

    d.tick_once().await.unwrap();

    assert_eq!(
        queue.pending_messages(10).await.unwrap().len(),
        1,
        "a Solo verdict must fall through to the unchanged single-agent dispatch"
    );
    // The spec IS frozen (the freeze is about the task's configuration,
    // not about the gate's verdict) — the next round re-gates against the
    // same frozen roles.
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert!(t.team_spec_json.is_some());
    assert!(crate::team_composer::frozen_spec(&t).is_some());
}

#[tokio::test]
async fn an_invalid_enabled_team_runs_solo_and_freezes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // Verifier shares the executor's family ⇒ refused (decision C).
    std::fs::write(
        dir.path().join("config.toml"),
        "[team]\nenabled = true\n\
             [team.roles.executor]\nruntime = \"gemini\"\nmodel = \"gemini-3.7-pro\"\n\
             [team.roles.verifier]\nruntime = \"antigravity\"\nmodel = \"gemini-3.7-flash\"\n",
    )
    .unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());

    d.tick_once().await.unwrap();

    assert_eq!(queue.pending_messages(10).await.unwrap().len(), 1);
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert!(
        t.team_spec_json.is_none(),
        "a refused team must leave no spec behind"
    );
}

#[tokio::test]
async fn enqueue_goal_work_builds_the_same_payload_as_the_driver_method() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let task = goal_task("g1", "alice");
    store.insert_task(&task).await.unwrap();
    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());

    let via_method = d.enqueue_work(&task, 1, "<state/>").await.unwrap();
    let via_free_fn = enqueue_goal_work(&queue, &store, &task, 1, "<state/>")
        .await
        .unwrap();

    let msgs = queue.pending_messages(10).await.unwrap();
    let a = msgs.iter().find(|m| m.id == via_method).unwrap();
    let b = msgs.iter().find(|m| m.id == via_free_fn).unwrap();
    assert_eq!(
        a.payload, b.payload,
        "the extraction must not have changed the dispatch payload"
    );
}

#[test]
fn config_defaults_and_partial_section() {
    // Absent section ⇒ defaults.
    let d = GoalLoopConfig::default();
    assert_eq!(d.iteration_cap, 5);
    assert_eq!(d.iteration_cap_simple, 3);
    assert_eq!(d.soft_cap, 3);
    assert_eq!(d.max_concurrent, 3);
    // H10: advisory-only, so the safe default is ON (unlike most
    // goal-loop gates, which default off).
    assert!(d.tool_streak_advisory);

    // Partial section ⇒ only the given field overrides; the rest default.
    let toml = "[goal_loop]\niteration_cap = 7\n";
    let table: toml::Table = toml.parse().unwrap();
    let cfg: GoalLoopConfig = table.get("goal_loop").unwrap().clone().try_into().unwrap();
    assert_eq!(cfg.iteration_cap, 7);
    assert_eq!(
        cfg.iteration_cap_simple, 3,
        "unspecified field keeps its default"
    );
    assert_eq!(cfg.soft_cap, 3, "unspecified field keeps its default");
    assert_eq!(cfg.max_concurrent, 3, "unspecified field keeps its default");
    assert_eq!(cfg.wall_clock_hours, 24);
    assert!(
        cfg.tool_streak_advisory,
        "unspecified field keeps its default (on)"
    );

    // H10: explicit `false` in config.toml is honored.
    let toml_off = "[goal_loop]\ntool_streak_advisory = false\n";
    let table_off: toml::Table = toml_off.parse().unwrap();
    let cfg_off: GoalLoopConfig = table_off
        .get("goal_loop")
        .unwrap()
        .clone()
        .try_into()
        .unwrap();
    assert!(!cfg_off.tool_streak_advisory);
}

#[tokio::test]
async fn capture_round_state_injects_escalating_tool_streak_hint() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let t = claimed_review_task("g1", "alice");
    store.insert_task(&t).await.unwrap();
    write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 5);

    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.capture_round_state(&t).await;

    let after = store.get_task("g1").await.unwrap().unwrap();
    let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
    let hint = snap
        .tool_streak_hint
        .expect("a streak of 5 must inject a hint");
    assert!(hint.contains("bash"));
    assert!(hint.contains('5'));
    // Tier 5 wording ("switch approach"), not tier 3's ("re-read the result").
    assert!(hint.contains("換一個方法") || hint.contains("換個方法"));

    // H10: also recorded to the Activity Feed for dashboard observability.
    let activity = store.list_activity_for_task("g1", 10).await.unwrap();
    assert!(
        activity
            .iter()
            .any(|a| a.event_type == "goal_loop.tool_call_streak" && a.summary.contains('5')),
        "streak count must be recorded to the activity feed: {activity:?}"
    );
}

#[tokio::test]
async fn capture_round_state_tier_8_hint_mentions_tasks_block() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let t = claimed_review_task("g1", "alice");
    store.insert_task(&t).await.unwrap();
    write_tool_calls_jsonl(
        dir.path(),
        "alice",
        "web_fetch",
        "{\"url\":\"https://x\"}",
        9,
    );

    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.capture_round_state(&t).await;

    let after = store.get_task("g1").await.unwrap().unwrap();
    let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
    let hint = snap
        .tool_streak_hint
        .expect("a streak of 9 must inject a hint");
    assert!(
        hint.contains("tasks_block"),
        "tier 8 must point at the escape hatch: {hint}"
    );
}

#[tokio::test]
async fn capture_round_state_no_hint_below_lowest_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let t = claimed_review_task("g1", "alice");
    store.insert_task(&t).await.unwrap();
    // Only 2 in a row — below the tier-3 floor.
    write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 2);

    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.capture_round_state(&t).await;

    let after = store.get_task("g1").await.unwrap().unwrap();
    let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
    assert!(
        snap.tool_streak_hint.is_none(),
        "a streak below 3 must not inject anything"
    );
}

#[tokio::test]
async fn capture_round_state_different_params_never_form_a_streak() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let t = claimed_review_task("g1", "alice");
    store.insert_task(&t).await.unwrap();
    // 4 calls to the same tool, but every one has a distinct argument —
    // real exploration, not a stuck loop.
    let lines: Vec<String> = (0..4)
        .map(|i| {
            serde_json::json!({
                "timestamp": Utc::now().to_rfc3339(),
                "agent_id": "alice",
                "tool_name": "bash",
                "success": true,
                "input": format!("cat file_{i}.txt"),
            })
            .to_string()
        })
        .collect();
    std::fs::write(
        dir.path().join("tool_calls.jsonl"),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.capture_round_state(&t).await;

    let after = store.get_task("g1").await.unwrap().unwrap();
    let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
    assert!(
        snap.tool_streak_hint.is_none(),
        "distinct params each round must never register as a streak"
    );
}

#[tokio::test]
async fn capture_round_state_config_off_yields_zero_injection() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let t = claimed_review_task("g1", "alice");
    store.insert_task(&t).await.unwrap();
    // Well past every threshold — would inject the tier-8 hint if enabled.
    write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 10);

    let cfg = GoalLoopConfig {
        tool_streak_advisory: false,
        ..small_cfg()
    };
    let d = driver(store.clone(), queue.clone(), cfg).with_home_dir(dir.path().to_path_buf());
    d.capture_round_state(&t).await;

    let after = store.get_task("g1").await.unwrap().unwrap();
    let snap = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
    assert!(
        snap.tool_streak_hint.is_none(),
        "tool_streak_advisory = false must produce zero injection"
    );

    // No activity event either — config off means the whole feature is silent.
    let activity = store.list_activity_for_task("g1", 10).await.unwrap();
    assert!(
        !activity
            .iter()
            .any(|a| a.event_type == "goal_loop.tool_call_streak"),
        "config off must not even post the activity event: {activity:?}"
    );
}

#[tokio::test]
async fn capture_round_state_tool_streak_hint_renders_in_next_round_state_block() {
    // End-to-end: the persisted hint round-trips through
    // `GoalStateSnapshot` into the next dispatch round's rendered
    // `<state>` block, exactly like `bail_hint` already does.
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let t = claimed_review_task("g1", "alice");
    store.insert_task(&t).await.unwrap();
    write_tool_calls_jsonl(dir.path(), "alice", "bash", "ls -la", 5);

    let d = driver(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf());
    d.capture_round_state(&t).await;

    let after = store.get_task("g1").await.unwrap().unwrap();
    let snapshot = GoalStateSnapshot::from_json(after.goal_state_json.as_deref());
    let block = goal_state::build_state_block(&after, &[], &snapshot);
    let rendered = block.render();
    assert!(
        rendered.contains("bash"),
        "the tool-streak hint must surface in the rendered <state> block: {rendered}"
    );
}
