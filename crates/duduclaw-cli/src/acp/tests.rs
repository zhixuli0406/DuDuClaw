//! Tests for the ACP module — Agent Card generation and A2A task lifecycle.

#[test]
fn test_agent_card_well_known_structure() {
    use super::server::AcpServer;

    let card = AcpServer::generate_agent_card(
        "DuDuClaw Agent",
        "AI agent with channel routing and self-evolution",
        "/.well-known/agent.json",
    );

    let json = serde_json::to_value(&card).unwrap();

    // Verify .well-known/agent.json structure matches A2A spec
    assert!(json.get("name").is_some());
    assert!(json.get("description").is_some());
    assert!(json.get("url").is_some());
    assert!(json.get("version").is_some());
    assert!(json.get("capabilities").is_some());
    assert!(json.get("skills").is_some());

    // Verify URL matches .well-known path
    assert_eq!(json["url"], "/.well-known/agent.json");

    // Verify skills are non-empty
    let skills = json["skills"].as_array().unwrap();
    assert!(!skills.is_empty());

    // Each skill should have name, description, tags
    for skill in skills {
        assert!(skill.get("name").is_some());
        assert!(skill.get("description").is_some());
        assert!(skill.get("tags").is_some());
    }
}

#[test]
fn test_a2a_task_manager_lifecycle() {
    use super::handlers::{A2ATaskManager, A2ATaskState};

    let mut mgr = A2ATaskManager::new();

    // Create a task
    let task = mgr.create_task("ctx_1", "Summarize document");
    let task_id = task.id.clone();
    assert_eq!(task.state, A2ATaskState::Working);
    assert_eq!(task.context_id, "ctx_1");
    assert_eq!(task.description, "Summarize document");

    // Get task
    assert!(mgr.get_task(&task_id).is_some());
    assert!(mgr.get_task("nonexistent").is_none());

    // Complete task
    assert!(mgr.complete_task(&task_id, "Summary done".to_string()));
    let completed = mgr.get_task(&task_id).unwrap();
    assert_eq!(completed.state, A2ATaskState::Completed);
    assert_eq!(completed.result.as_deref(), Some("Summary done"));

    // Cancel a new task
    let task2 = mgr.create_task("ctx_2", "Another task");
    let task2_id = task2.id.clone();
    assert!(mgr.cancel_task(&task2_id));
    let cancelled = mgr.get_task(&task2_id).unwrap();
    assert_eq!(cancelled.state, A2ATaskState::Canceled);

    // Cancel nonexistent returns false
    assert!(!mgr.cancel_task("nonexistent"));
}

#[tokio::test]
async fn test_tasks_send_via_handler() {
    use super::handlers::A2ATaskManager;

    let mut mgr = A2ATaskManager::new();
    let params = serde_json::json!({
        "message": "Hello, agent!",
        "context_id": "test_ctx",
    });
    let id = serde_json::json!(1);

    // RFC-25 Phase 3: handle_tasks_send is now async and executes the target
    // agent. With an empty home (no agents), execution fails gracefully and the
    // task still completes with the error text — so the envelope shape holds.
    let home = std::env::temp_dir().join("duduclaw-acp-test-empty-home");
    let response = super::server::handle_tasks_send(&id, &params, &mut mgr, &home).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);

    let result = &response["result"];
    assert!(result.get("task").is_some());
    assert!(result.get("artifacts").is_some());

    let task = &result["task"];
    // Empty test home → no agents → execution fails → state "failed"; a real
    // home with agents → "completed". Accept both (RFC-25 Phase 3 audit fix).
    let state = task["state"].as_str().unwrap();
    assert!(state == "completed" || state == "failed", "unexpected state: {state}");
    assert!(task["result"].as_str().is_some());

    let artifacts = result["artifacts"].as_array().unwrap();
    assert!(!artifacts.is_empty());
    assert_eq!(artifacts[0]["type"], "text");
}

#[tokio::test]
async fn test_tasks_send_missing_message() {
    use super::handlers::A2ATaskManager;

    let mut mgr = A2ATaskManager::new();
    let params = serde_json::json!({});
    let id = serde_json::json!(2);

    let home = std::env::temp_dir().join("duduclaw-acp-test-empty-home");
    let response = super::server::handle_tasks_send(&id, &params, &mut mgr, &home).await;
    assert!(response.get("error").is_some());
    assert_eq!(response["error"]["code"], -32602);
}

#[test]
fn test_tasks_get_via_handler() {
    use super::handlers::A2ATaskManager;

    let mgr = A2ATaskManager::new();
    let params = serde_json::json!({ "task_id": "nonexistent" });
    let id = serde_json::json!(3);

    let response = super::server::handle_tasks_get(&id, &params, &mgr);
    assert!(response.get("error").is_some());
    assert_eq!(response["error"]["code"], -32001);
}

#[test]
fn test_agent_discover() {
    let id = serde_json::json!(4);
    let response = super::server::handle_agent_discover(&id);

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 4);

    let result = &response["result"];
    assert_eq!(result["name"], "DuDuClaw Agent");
    assert!(result.get("capabilities").is_some());
    assert!(result.get("skills").is_some());
    assert!(result["capabilities"]["streaming"].as_bool().unwrap());
}
