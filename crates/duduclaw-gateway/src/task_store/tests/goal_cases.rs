//! Unit tests for [`super`], moved verbatim out of the former `task_store.rs` — goal cases.

use super::*;

#[tokio::test]
async fn plan_step_front_inserts_renormalize_when_gap_exhausted() {
    let (store, _dir) = temp_store();
    store.insert_plan(&plan("p1", "bot")).await.unwrap();
    store
        .add_plan_step("p1", "base", "base", "agent", "bot", None)
        .await
        .unwrap();
    // Repeated front inserts halve the head gap (1024 → 512 → 256 → …);
    // past ~10 inserts the midpoint collides and renormalization must kick
    // in transparently. 16 inserts forces at least one renormalize pass.
    for i in 0..16 {
        store
            .add_plan_step(
                "p1",
                &format!("f{i}"),
                &format!("front {i}"),
                "user",
                "u",
                Some(0),
            )
            .await
            .unwrap();
    }
    let steps = store.list_plan_steps("p1").await.unwrap();
    assert_eq!(steps.len(), 17);
    // Newest front insert leads; original base is last.
    assert_eq!(steps.first().unwrap().text, "front 15");
    assert_eq!(steps.last().unwrap().text, "base");
    // Orders are strictly increasing (total order held through renorms).
    let orders: Vec<i64> = steps.iter().map(|s| s.step_order).collect();
    assert!(
        orders.windows(2).all(|w| w[0] < w[1]),
        "orders strictly ascend: {orders:?}"
    );
}

#[tokio::test]
async fn plan_step_update_validates_enums_fail_closed() {
    let (store, _dir) = temp_store();
    store.insert_plan(&plan("p1", "bot")).await.unwrap();
    store
        .add_plan_step("p1", "s1", "step", "agent", "bot", None)
        .await
        .unwrap();

    // Invalid enum values are rejected, valid ones apply.
    assert!(
        store
            .update_plan_step("s1", &serde_json::json!({ "status": "nonsense" }))
            .await
            .is_err()
    );
    assert!(
        store
            .update_plan_step("s1", &serde_json::json!({ "assignee_kind": "alien" }))
            .await
            .is_err()
    );
    assert!(
        store
            .update_plan_step("s1", &serde_json::json!({ "text": "   " }))
            .await
            .is_err()
    );
    let updated = store
        .update_plan_step(
            "s1",
            &serde_json::json!({ "status": "done", "assignee_kind": "user", "assignee": "louis" }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, "done");
    assert_eq!(updated.assignee_kind, "user");
    assert_eq!(updated.assignee, "louis");

    // Invalid add-time assignee_kind also rejected.
    assert!(
        store
            .add_plan_step("p1", "s2", "x", "robot", "", None)
            .await
            .is_err()
    );
    // Unknown plan rejected (no dangling steps).
    assert!(
        store
            .add_plan_step("ghost-plan", "s3", "x", "agent", "bot", None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn plan_crud_and_remove_cascades_steps() {
    let (store, _dir) = temp_store();
    store.insert_plan(&plan("p1", "bot")).await.unwrap();
    store
        .add_plan_step("p1", "s1", "step", "agent", "bot", None)
        .await
        .unwrap();

    // Update plan fields; invalid status fail-closed.
    assert!(
        store
            .update_plan("p1", &serde_json::json!({ "status": "bogus" }))
            .await
            .is_err()
    );
    let p = store
        .update_plan(
            "p1",
            &serde_json::json!({ "title": "Renamed", "status": "done" }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.title, "Renamed");
    assert_eq!(p.status, "done");

    // Remove step returns the removed row.
    let removed = store.remove_plan_step("s1").await.unwrap().unwrap();
    assert_eq!(removed.plan_id, "p1");
    assert!(store.remove_plan_step("s1").await.unwrap().is_none());

    // Remove plan cascades remaining steps.
    store
        .add_plan_step("p1", "s2", "another", "user", "", None)
        .await
        .unwrap();
    assert!(store.remove_plan("p1").await.unwrap());
    assert!(store.get_plan("p1").await.unwrap().is_none());
    assert!(store.list_plan_steps("p1").await.unwrap().is_empty());
    assert!(store.get_plan_step("s2").await.unwrap().is_none());
}

#[tokio::test]
async fn plan_prompt_section_is_byte_stable_and_scoped_to_agent_steps() {
    let (store, _dir) = temp_store();
    store.insert_plan(&plan("p1", "bot")).await.unwrap();
    store
        .add_plan_step("p1", "s1", "agent does this", "agent", "bot", None)
        .await
        .unwrap();
    store
        .add_plan_step("p1", "s2", "user does that", "user", "louis", None)
        .await
        .unwrap();

    let a = store.plan_prompt_section("bot").await.unwrap().unwrap();
    let b = store.plan_prompt_section("bot").await.unwrap().unwrap();
    assert_eq!(
        a, b,
        "byte-stable when rows unchanged (prompt-cache friendly)"
    );
    assert!(a.contains("← yours"), "agent's own step marked");
    assert!(a.contains("plan_update_step"));

    // Another agent with no steps in the plan gets nothing.
    assert!(store.plan_prompt_section("other").await.unwrap().is_none());

    // Once the agent's steps are all done, the section disappears.
    store
        .update_plan_step("s1", &serde_json::json!({ "status": "done" }))
        .await
        .unwrap();
    assert!(store.plan_prompt_section("bot").await.unwrap().is_none());
}

#[tokio::test]
async fn migration_is_idempotent_across_reopens() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Open, close, reopen — the ALTER guard must not error on second run.
    {
        let s = TaskStore::open(dir.path()).expect("first open");
        s.insert_task(&pending_task("m1")).await.unwrap();
    }
    let s2 = TaskStore::open(dir.path()).expect("reopen");
    assert_eq!(s2.get_task("m1").await.unwrap().unwrap().status, "pending");
}

// ── Goal assignment form v2 (design-market-belief-loop-2026-08.md §6,
// G1, 2026-08-14) ────────────────────────────────────────

#[tokio::test]
async fn deadline_and_risk_boundary_round_trip_through_insert_and_read() {
    let (store, _dir) = temp_store();
    let mut t = pending_task("g1");
    t.deadline_at = Some("2026-08-20T00:00:00Z".to_string());
    t.risk_boundary = Some("不得動用生產資料庫寫入權限".to_string());
    store.insert_task(&t).await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.deadline_at.as_deref(), Some("2026-08-20T00:00:00Z"));
    assert_eq!(
        got.risk_boundary.as_deref(),
        Some("不得動用生產資料庫寫入權限")
    );

    // A task that never sets them stays NULL (baseline applies at the
    // injection layer, never stored here — see the field doc comments).
    let bare = store.get_task("m1_never_inserted").await.unwrap();
    assert!(bare.is_none());
    let plain = pending_task("g2");
    store.insert_task(&plain).await.unwrap();
    let got2 = store.get_task("g2").await.unwrap().unwrap();
    assert!(got2.deadline_at.is_none());
    assert!(got2.risk_boundary.is_none());
}

// ── Team-as-Agent spec freeze (P1/WP-4, 2026-09) ───────────────────

#[tokio::test]
async fn team_spec_json_defaults_to_none_and_round_trips_through_insert() {
    let (store, _dir) = temp_store();
    let plain = pending_task("ts0");
    store.insert_task(&plain).await.unwrap();
    assert!(
        store
            .get_task("ts0")
            .await
            .unwrap()
            .unwrap()
            .team_spec_json
            .is_none(),
        "a task created without a team must read back as Solo"
    );

    let mut t = pending_task("ts1");
    t.team_spec_json = Some("{\"schema\":1}".into());
    store.insert_task(&t).await.unwrap();
    assert_eq!(
        store
            .get_task("ts1")
            .await
            .unwrap()
            .unwrap()
            .team_spec_json
            .as_deref(),
        Some("{\"schema\":1}")
    );
}

#[tokio::test]
async fn freeze_team_spec_writes_once_and_never_overwrites() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("ts2")).await.unwrap();

    assert!(store.freeze_team_spec("ts2", "{\"v\":1}").await.unwrap());
    assert!(
        !store.freeze_team_spec("ts2", "{\"v\":2}").await.unwrap(),
        "a second freeze must report that it did not write"
    );
    assert_eq!(
        store.team_spec_json("ts2").await.unwrap().as_deref(),
        Some("{\"v\":1}"),
        "the first frozen spec wins forever"
    );
}

#[tokio::test]
async fn freeze_team_spec_on_a_missing_task_writes_nothing() {
    let (store, _dir) = temp_store();
    assert!(!store.freeze_team_spec("nope", "{}").await.unwrap());
    assert!(store.team_spec_json("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn a_frozen_team_spec_survives_an_operator_update() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("ts3")).await.unwrap();
    store.freeze_team_spec("ts3", "{\"v\":1}").await.unwrap();
    store
        .update_task("ts3", &serde_json::json!({ "title": "renamed" }))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.team_spec_json("ts3").await.unwrap().as_deref(),
        Some("{\"v\":1}"),
        "update_task has no write path to the frozen spec"
    );
}

// ── H9-G goal contract freeze (harness-borrowings 2026-08 WP-D) ─────

#[tokio::test]
async fn acceptance_criteria_baseline_round_trips_through_insert_and_read() {
    let (store, _dir) = temp_store();
    let mut t = pending_task("gb1");
    t.acceptance_criteria = Some("current criteria".into());
    t.acceptance_criteria_baseline = Some("frozen criteria".into());
    store.insert_task(&t).await.unwrap();

    let got = store.get_task("gb1").await.unwrap().unwrap();
    assert_eq!(got.acceptance_criteria.as_deref(), Some("current criteria"));
    assert_eq!(
        got.acceptance_criteria_baseline.as_deref(),
        Some("frozen criteria")
    );

    // A task that never sets a baseline stays NULL — readers fall back to
    // `acceptance_criteria` at the consumer layer (dispatch_engine.rs).
    let plain = pending_task("gb2");
    store.insert_task(&plain).await.unwrap();
    let got2 = store.get_task("gb2").await.unwrap().unwrap();
    assert!(got2.acceptance_criteria_baseline.is_none());
}

#[tokio::test]
async fn update_task_can_edit_acceptance_criteria_but_never_touches_the_baseline() {
    // Store layer is identity-agnostic (authorization lives at the MCP /
    // dashboard boundary — see mcp.rs::handle_tasks_update and
    // handlers.rs::handle_tasks_update); this only asserts the SQL-level
    // invariant: `acceptance_criteria` is updatable, `acceptance_criteria_baseline`
    // has no write path at all after `insert_task`.
    let (store, _dir) = temp_store();
    let mut t = pending_task("gb3");
    t.acceptance_criteria = Some("original".into());
    t.acceptance_criteria_baseline = Some("frozen forever".into());
    store.insert_task(&t).await.unwrap();

    let updated = store
        .update_task(
            "gb3",
            &serde_json::json!({ "acceptance_criteria": "edited by operator" }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        updated.acceptance_criteria.as_deref(),
        Some("edited by operator")
    );
    assert_eq!(
        updated.acceptance_criteria_baseline.as_deref(),
        Some("frozen forever"),
        "the baseline must never change, regardless of who calls update_task"
    );
}

#[tokio::test]
async fn deadline_and_risk_boundary_migration_idempotent_and_old_rows_default_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Simulate a pre-goal-form-v2 db: insert a task, close, reopen twice —
    // the ALTER guard must not error on re-run, and a row inserted before
    // the columns existed must read back as NULL (never a spurious
    // default) rather than panicking on a missing column.
    {
        let s = TaskStore::open(dir.path()).expect("first open");
        s.insert_task(&pending_task("old1")).await.unwrap();
    }
    {
        // Re-running the ALTER guard on an already-migrated db must not
        // error (idempotency across reopens, mirrors
        // `migration_is_idempotent_across_reopens`).
        let s2 = TaskStore::open(dir.path()).expect("second open");
        let t = s2.get_task("old1").await.unwrap().unwrap();
        assert!(t.deadline_at.is_none());
        assert!(t.risk_boundary.is_none());
    }
    let s3 = TaskStore::open(dir.path()).expect("third open (re-run ALTER again)");
    let t = s3.get_task("old1").await.unwrap().unwrap();
    assert!(t.deadline_at.is_none());
    assert!(t.risk_boundary.is_none());
}

#[tokio::test]
async fn reject_review_routes_to_revising_and_bumps_round() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    store.insert_task(&goal_review_task("g1")).await.unwrap();
    // Round 1 was dispatched (driver) before the judge ruled.
    store
        .record_iteration_dispatch("g1", 1, "2026-07-25T10:00:00Z")
        .await
        .unwrap();

    let status = store
        .reject_review("g1", "missing summary", 3)
        .await
        .unwrap();
    assert_eq!(status, "revising");
    let t = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(t.status, "revising");
    assert_eq!(t.revision_round, 1);
    assert!(!t.diminishing, "one round is below soft cap 3");
    assert_eq!(t.retry_count, 1);
    assert_eq!(t.judge_feedback.as_deref(), Some("missing summary"));
    // Claim/lease/result cleared so the loop can re-dispatch it.
    assert!(t.claimed_by.is_none());
    assert!(t.result_summary.is_none());
    // A rejection verdict is sealed in the iteration timeline.
    let iters = store.list_iterations("g1").await.unwrap();
    assert_eq!(iters.len(), 1);
    assert_eq!(iters[0].verdict.as_deref(), Some("rejected"));
    assert_eq!(iters[0].judge_feedback.as_deref(), Some("missing summary"));
}
