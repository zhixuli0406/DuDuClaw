//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// H9-G goal contract freeze (harness-borrowings 2026-08 WP-D): `tasks.goal_create`
/// (dashboard RPC) freezes an immutable acceptance-criteria baseline at
/// creation time; the dashboard/operator `tasks.update` RPC may still edit the
/// mutable copy — distinct from the agent-facing MCP `tasks_update` tool
/// (`duduclaw-cli::mcp::handle_tasks_update`), which refuses that same edit
/// on a `goal_mode` task.
use super::*;

fn admin_ctx() -> UserContext {
    UserContext::admin_fallback()
}

#[tokio::test]
async fn goal_loop_bad_cost_config_is_visible_on_capped_and_uncapped_goals_and_clears() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let store = TaskStore::open(home.path()).unwrap();
    for (id, goal_mode, cap) in [
        ("uncapped", true, None),
        ("capped", true, Some(100)),
        ("ordinary", false, None),
    ] {
        let mut task = TaskRow::new(id.into(), id.into(), String::new(), "medium".into(), "agent-x".into(), "system".into());
        task.goal_mode = goal_mode;
        task.cost_cap_cents = cap;
        task.blocked_reason = Some("existing reason".into());
        store.insert_task(&task).await.unwrap();
    }
    std::fs::write(home.path().join("config.toml"), "[goal_loop]\ndefault_cost_cap_cents = 'bad'").unwrap();
    let listed = payload(&handler.handle_tasks_list(json!({}), &admin_ctx()).await);
    for row in listed["tasks"].as_array().unwrap() {
        let reason = row["blocked_reason"].as_str().unwrap();
        assert_eq!(reason.contains("default_cost_cap_cents"), row["goal_mode"] == true);
        assert!(reason.ends_with("existing reason"));
    }
    // The notice is computed from the live config, never persisted as a task change.
    assert_eq!(store.get_task("uncapped").await.unwrap().unwrap().blocked_reason.as_deref(), Some("existing reason"));
    std::fs::write(home.path().join("config.toml"), "[goal_loop]\ndefault_cost_cap_cents = 0").unwrap();
    let listed = payload(&handler.handle_tasks_list(json!({}), &admin_ctx()).await);
    for row in listed["tasks"].as_array().unwrap() {
        assert_eq!(row["blocked_reason"], "existing reason");
    }
}

#[tokio::test]
async fn goal_loop_dashboard_cost_cap_validates_persists_displays_and_resumes() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    for cap in [json!(0), json!(-1), json!(1.5), json!("100"), Value::Null] {
        let frame = handler
            .handle_tasks_goal_create(
                json!({"agent_id":"agent-x", "description":"整理報表", "cost_cap_cents":cap}),
                &admin_ctx(),
            )
            .await;
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }));
    }
    let created = payload(
        &handler
            .handle_tasks_goal_create(
                json!({"agent_id":"agent-x", "description":"整理報表", "cost_cap_cents":100}),
                &admin_ctx(),
            )
            .await,
    );
    assert_eq!(created["task"]["cost_cap_cents"], 100);
    let id = created["task"]["id"].as_str().unwrap();
    let store = TaskStore::open(home.path()).unwrap();
    store
        .mark_needs_human_with_pause(
            id,
            "spent 100 cap 100",
            crate::pause_reason::PauseReason::BudgetExhausted,
        )
        .await
        .unwrap();

    // Missing telemetry must remain unknown on the dashboard too.
    let listed = payload(&handler.handle_tasks_list(json!({}), &admin_ctx()).await);
    let row = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(row["effective_cost_cap_cents"], 100);
    assert!(row["measured_spent_cents"].is_null());
    assert_eq!(row["judge_feedback"], "spent 100 cap 100");

    // Exercise the real shared telemetry reader, including child-task spend.
    let _telemetry =
        crate::cost_telemetry::CostTelemetry::new(&home.path().join("cost_telemetry.db")).unwrap();
    let mut child = TaskRow::new(
        "cost-child".into(),
        "child".into(),
        String::new(),
        "medium".into(),
        "agent-x".into(),
        "system".into(),
    );
    child.parent_task_id = Some(id.into());
    store.insert_task(&child).await.unwrap();
    let conn = rusqlite::Connection::open(home.path().join("cost_telemetry.db")).unwrap();
    for (episode, cost) in [(id, 40), ("cost-child", 60)] {
        conn.execute("INSERT INTO token_usage(agent_id,request_type,created_at,episode_id,input_tokens,cost_millicents) VALUES ('agent-x','test','2026-10-28',?1,1,?2)",
            rusqlite::params![episode, cost]).unwrap();
    }
    let listed = payload(&handler.handle_tasks_list(json!({}), &admin_ctx()).await);
    let row = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(row["measured_spent_cents"], 100);
    let updated = payload(
        &handler
            .handle_tasks_update(json!({"task_id":id,"cost_cap_cents":200}), &admin_ctx())
            .await,
    );
    assert_eq!(updated["task"]["cost_cap_cents"], 200);
    assert_eq!(updated["task"]["status"], "needs_human");
    payload(
        &handler
            .handle_tasks_goal_decide(json!({"task_id":id,"action":"retry"}), &admin_ctx())
            .await,
    );
    let resumed = store.get_task(id).await.unwrap().unwrap();
    assert_eq!(resumed.status, "pending");
    assert_eq!(resumed.cost_cap_cents, Some(200));
}

fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        WsFrame::Response {
            ok: false, error, ..
        } => {
            panic!("RPC returned an error frame: {error:?}")
        }
        other => panic!("unexpected frame shape: {other:?}"),
    }
}

/// `MethodHandler::new` leaves `task_store` unset (`RwLock::new(None)`) —
/// production wires it via `set_task_store` during gateway boot
/// (`server.rs`). Every test in this module needs the same wiring.
async fn handler_with_task_store(home: &std::path::Path) -> MethodHandler {
    let handler = MethodHandler::new(home.to_path_buf()).await;
    handler
        .set_task_store(Arc::new(TaskStore::open(home).unwrap()))
        .await;
    handler
}

#[tokio::test]
async fn goal_create_freezes_an_immutable_baseline() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let frame = handler
        .handle_tasks_goal_create(
            json!({
                "agent_id": "agent-x",
                "description": "整理報表",
                "acceptance_criteria": "含營收圖表",
            }),
            &admin_ctx(),
        )
        .await;
    let p = payload(&frame);
    assert_eq!(p["task"]["acceptance_criteria"], "含營收圖表");
    assert_eq!(
        p["task"]["acceptance_criteria_baseline"], "含營收圖表",
        "baseline must be frozen to the same value at creation: {p}"
    );
}

#[tokio::test]
async fn goal_create_without_explicit_criteria_still_freezes_the_goal_text_as_baseline() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let frame = handler
        .handle_tasks_goal_create(
            json!({ "agent_id": "agent-x", "description": "整理報表" }),
            &admin_ctx(),
        )
        .await;
    let p = payload(&frame);
    assert_eq!(p["task"]["acceptance_criteria"], "整理報表");
    assert_eq!(p["task"]["acceptance_criteria_baseline"], "整理報表");
}

#[tokio::test]
async fn operator_can_edit_the_mutable_acceptance_criteria_via_tasks_update() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let created = payload(
        &handler
            .handle_tasks_goal_create(
                json!({
                    "agent_id": "agent-y",
                    "description": "整理報表",
                    "acceptance_criteria": "含營收圖表",
                }),
                &admin_ctx(),
            )
            .await,
    );
    let task_id = created["task"]["id"].as_str().unwrap();

    let updated = payload(
        &handler
            .handle_tasks_update(
                json!({
                    "task_id": task_id,
                    "acceptance_criteria": "含營收圖表與客訴摘要",
                }),
                &admin_ctx(),
            )
            .await,
    );
    assert_eq!(
        updated["task"]["acceptance_criteria"], "含營收圖表與客訴摘要",
        "an operator via the dashboard RPC may edit the mutable copy: {updated}"
    );
    assert_eq!(
        updated["task"]["acceptance_criteria_baseline"], "含營收圖表",
        "the frozen baseline must stay untouched even when an operator edits \
             the mutable field: {updated}"
    );
}
