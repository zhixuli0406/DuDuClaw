//! Task audience (owner decision U6) applied to the employee-facing MCP
//! listing tools: `tasks_list` and `activity_list` hide a task whose
//! hand-off packets limit who may see it from employees that neither own it
//! nor are named, and leave operators unrestricted.

use super::*;
use duduclaw_gateway::task_store::{ActivityRow, TaskRow, TaskStore};

async fn seed(home: &std::path::Path, assigned_to: &str, title: &str) -> String {
    let store = TaskStore::open(home).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let row = TaskRow::new(
        id.clone(),
        title.into(),
        "description".into(),
        "medium".into(),
        assigned_to.into(),
        assigned_to.into(),
    );
    store.insert_task(&row).await.unwrap();
    store
        .append_activity(&ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "task.progress".into(),
            agent_id: assigned_to.into(),
            task_id: Some(id.clone()),
            summary: format!("progress on {title}"),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: None,
        })
        .await
        .unwrap();
    id
}

/// File one hand-off packet for `task_id` naming `audience`.
fn limit(home: &std::path::Path, task_id: &str, audience: &[&str]) {
    let dir = home
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join(task_id)
        .join("1");
    std::fs::create_dir_all(&dir).unwrap();
    let raw = serde_json::json!({
        "packet_id": "p1",
        "goal_id": task_id,
        "round": 1,
        "from_role": "executor",
        "to_role": "verifier",
        "objective": "Review report",
        "output_format": "files",
        "audience": audience,
    });
    std::fs::write(dir.join("p1.json"), raw.to_string()).unwrap();
}

fn titles(out: &Value, list: &str, field: &str) -> Vec<String> {
    let text = out["content"][0]["text"].as_str().expect("tool text");
    let v: Value = serde_json::from_str(text).unwrap();
    let mut t: Vec<String> = v[list]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[field].as_str().unwrap().to_string())
        .collect();
    t.sort();
    t
}

#[tokio::test(flavor = "current_thread")]
async fn limited_task_is_hidden_from_other_employees_but_not_owner_named_or_operator() {
    let home = tempfile::tempdir().unwrap();
    let private = seed(home.path(), "sales", "private").await;
    seed(home.path(), "sales", "public").await;
    limit(home.path(), &private, &["user:alice", "role:support", "verifier"]);
    assert!(matches!(
        duduclaw_gateway::review_evidence::audience::task_audience(home.path(), &private),
        duduclaw_gateway::review_evidence::audience::TaskAudience::Limited(_)
    ));
    let all = serde_json::json!({ "assigned_to": "*" });

    let other = handle_tasks_list(&all, home.path(), "other", RecordActor::Agent("other")).await;
    assert_eq!(titles(&other, "tasks", "title"), ["public"]);
    let owner = handle_tasks_list(&all, home.path(), "sales", RecordActor::Agent("sales")).await;
    assert_eq!(titles(&owner, "tasks", "title"), ["private", "public"]);
    let named = handle_tasks_list(&all, home.path(), "support", RecordActor::Agent("support")).await;
    assert_eq!(titles(&named, "tasks", "title"), ["private", "public"]);
    let op = handle_tasks_list(&all, home.path(), "dudu", RecordActor::Operator("dudu")).await;
    assert_eq!(titles(&op, "tasks", "title"), ["private", "public"]);

    let every = serde_json::json!({ "agent_id": "*" });
    let other = handle_activity_list(&every, home.path(), "other", RecordActor::Agent("other")).await;
    assert_eq!(titles(&other, "activities", "summary"), ["progress on public"]);
    let owner = handle_activity_list(&every, home.path(), "sales", RecordActor::Agent("sales")).await;
    assert_eq!(
        titles(&owner, "activities", "summary"),
        ["progress on private", "progress on public"]
    );
    let op = handle_activity_list(&every, home.path(), "dudu", RecordActor::Operator("dudu")).await;
    assert_eq!(
        titles(&op, "activities", "summary"),
        ["progress on private", "progress on public"]
    );
}

/// Packets that cannot be read hide the task from every employee but its
/// owners (fail closed), as the dashboard hides it from everyone but admins.
#[tokio::test(flavor = "current_thread")]
async fn unreadable_packets_leave_the_task_to_its_owners() {
    let home = tempfile::tempdir().unwrap();
    let broken = seed(home.path(), "sales", "broken").await;
    let dir = home
        .path()
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join(&broken)
        .join("1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("corrupt.json"), "{").unwrap();
    let all = serde_json::json!({ "assigned_to": "*" });
    let other = handle_tasks_list(&all, home.path(), "other", RecordActor::Agent("other")).await;
    assert!(titles(&other, "tasks", "title").is_empty());
    let owner = handle_tasks_list(&all, home.path(), "sales", RecordActor::Agent("sales")).await;
    assert_eq!(titles(&owner, "tasks", "title"), ["broken"]);
}
