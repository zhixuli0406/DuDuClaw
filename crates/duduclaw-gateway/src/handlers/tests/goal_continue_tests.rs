//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// I-3a (`DESIGN-dashboard-ux-workbuddy-2026-08.md` §3.3, backlog item I-3a):
/// a `done`/`failed`/`cancelled` **goal-mode** task can take a dashboard
/// follow-up message via `tasks.goal_decide` `action: "continue"` and get
/// reopened for another round — WorkBuddy's "已完成/失敗任務可續推" pattern,
/// gated the same way as every other `goal_decide` action (Operator ACL on
/// the task's assigned agent).
use super::*;

fn admin_ctx() -> UserContext {
    UserContext::admin_fallback()
}

/// A caller holding only a `Viewer` binding on the task's agent — below
/// the `Operator` bar every `goal_decide` action requires.
fn viewer_ctx(agent: &str) -> UserContext {
    let mut agent_access = std::collections::HashMap::new();
    agent_access.insert(agent.to_string(), AccessLevel::Viewer);
    UserContext {
        user_id: "u1".to_string(),
        email: "u1@test.local".to_string(),
        role: UserRole::Employee,
        agent_access,
        must_change_password: false,
    }
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

fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

async fn handler_with_task_store(home: &std::path::Path) -> MethodHandler {
    let handler = MethodHandler::new(home.to_path_buf()).await;
    handler
        .set_task_store(Arc::new(TaskStore::open(home).unwrap()))
        .await;
    handler
}

fn goal_task(id: &str, agent: &str, status: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        "整理客戶月報".into(),
        "把客戶資料整理成月報並寄出".into(),
        "medium".into(),
        agent.into(),
        "system".into(),
    );
    t.status = status.into();
    t.goal_mode = true;
    t
}

#[tokio::test]
async fn continue_reopens_a_done_goal_task_with_the_message() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let store = TaskStore::open(home.path()).unwrap();
    store
        .insert_task(&goal_task("g-done", "agent-c1", "done"))
        .await
        .unwrap();

    let frame = handler
        .handle_tasks_goal_decide(
            json!({ "task_id": "g-done", "action": "continue", "message": "請加註本月營收數字" }),
            &admin_ctx(),
        )
        .await;
    let p = payload(&frame);
    assert_eq!(
        p["task"]["status"], "pending",
        "a continued task must go back to pending: {p}"
    );
    assert!(
        p["task"]["judge_feedback"]
            .as_str()
            .unwrap()
            .contains("請加註本月營收數字"),
        "the follow-up message must reach judge_feedback for the next dispatch: {p}"
    );
}

#[tokio::test]
async fn continue_reopens_a_failed_goal_task_with_the_message() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let store = TaskStore::open(home.path()).unwrap();
    store
        .insert_task(&goal_task("g-failed", "agent-c2", "failed"))
        .await
        .unwrap();

    let frame = handler
        .handle_tasks_goal_decide(
            json!({ "task_id": "g-failed", "action": "continue", "message": "換一種方式重試" }),
            &admin_ctx(),
        )
        .await;
    let p = payload(&frame);
    assert_eq!(p["task"]["status"], "pending");
    assert!(
        p["task"]["judge_feedback"]
            .as_str()
            .unwrap()
            .contains("換一種方式重試")
    );
}

#[tokio::test]
async fn continue_denies_a_caller_without_operator_access() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let store = TaskStore::open(home.path()).unwrap();
    store
        .insert_task(&goal_task("g-guard", "agent-c3", "done"))
        .await
        .unwrap();

    let frame = handler
        .handle_tasks_goal_decide(
            json!({ "task_id": "g-guard", "action": "continue", "message": "再多做一點" }),
            &viewer_ctx("agent-c3"),
        )
        .await;
    assert!(
        error_text(&frame).contains("permission denied"),
        "a Viewer binding must not be enough to continue a task — got: {frame:?}"
    );
    // The denial must be enforced BEFORE the store mutation, not after.
    let still = store.get_task("g-guard").await.unwrap().unwrap();
    assert_eq!(still.status, "done");
}

#[tokio::test]
async fn continue_requires_a_non_empty_message() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let store = TaskStore::open(home.path()).unwrap();
    store
        .insert_task(&goal_task("g-empty", "agent-c4", "done"))
        .await
        .unwrap();

    let frame = handler
        .handle_tasks_goal_decide(
            json!({ "task_id": "g-empty", "action": "continue", "message": "   " }),
            &admin_ctx(),
        )
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "a blank message must be refused: {frame:?}"
    );
    let still = store.get_task("g-empty").await.unwrap().unwrap();
    assert_eq!(
        still.status, "done",
        "a refused continue must not touch the task"
    );
}

#[tokio::test]
async fn continue_refuses_a_non_goal_mode_task() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_task_store(home.path()).await;
    let store = TaskStore::open(home.path()).unwrap();
    let mut t = goal_task("g-board", "agent-c5", "done");
    t.goal_mode = false;
    store.insert_task(&t).await.unwrap();

    let frame = handler
        .handle_tasks_goal_decide(
            json!({ "task_id": "g-board", "action": "continue", "message": "再做一次" }),
            &admin_ctx(),
        )
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "an ordinary board task must not be reopenable via goal_decide continue: {frame:?}"
    );
    let still = store.get_task("g-board").await.unwrap().unwrap();
    assert_eq!(still.status, "done");
}
