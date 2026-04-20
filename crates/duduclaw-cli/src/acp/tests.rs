//! Tests for the ACP module — reverse-RPC types and Agent Card generation.

#[test]
fn test_reverse_rpc_types() {
    use super::types::*;

    // Test FsReadFileParams serialization
    let params = FsReadFileParams {
        path: "/tmp/test.txt".to_string(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert_eq!(json["path"], "/tmp/test.txt");

    // Test FsReadFileResult
    let result = FsReadFileResult {
        content: "hello world".to_string(),
    };
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["content"], "hello world");

    // Test FsWriteFileParams
    let params = FsWriteFileParams {
        path: "/tmp/output.txt".to_string(),
        content: "new content".to_string(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert_eq!(json["path"], "/tmp/output.txt");
    assert_eq!(json["content"], "new content");

    // Test RequestPermissionParams
    let params = RequestPermissionParams {
        tool_name: "Bash".to_string(),
        description: "Run git status".to_string(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert_eq!(json["tool_name"], "Bash");

    // Test RequestPermissionResult
    let result = RequestPermissionResult { granted: true };
    let json = serde_json::to_value(&result).unwrap();
    assert!(json["granted"].as_bool().unwrap());
}

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

#[test]
fn test_tasks_send_via_handler() {
    use super::handlers::A2ATaskManager;

    let mut mgr = A2ATaskManager::new();
    let params = serde_json::json!({
        "message": "Hello, agent!",
        "context_id": "test_ctx",
    });
    let id = serde_json::json!(1);

    let response = super::server::handle_tasks_send(&id, &params, &mut mgr);
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);

    let result = &response["result"];
    assert!(result.get("task").is_some());
    assert!(result.get("artifacts").is_some());

    let task = &result["task"];
    assert_eq!(task["state"], "completed");
    assert!(task["result"].as_str().is_some());

    let artifacts = result["artifacts"].as_array().unwrap();
    assert!(!artifacts.is_empty());
    assert_eq!(artifacts[0]["type"], "text");
}

#[test]
fn test_tasks_send_missing_message() {
    use super::handlers::A2ATaskManager;

    let mut mgr = A2ATaskManager::new();
    let params = serde_json::json!({});
    let id = serde_json::json!(2);

    let response = super::server::handle_tasks_send(&id, &params, &mut mgr);
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
