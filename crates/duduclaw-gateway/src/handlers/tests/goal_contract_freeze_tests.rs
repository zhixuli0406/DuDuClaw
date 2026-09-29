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
