use crate::mcp_isolation::{IsolatedCall, ISOLATED_CALL};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{RequestContext, resources::ResourceRoots};
use serde_json::{Value, json};
use std::sync::Arc;

fn call(home: &std::path::Path) -> Arc<IsolatedCall> {
    let context = RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![ResourceGrant::Task {
            id: Text::new("allowed".into()).unwrap(),
        }]).unwrap(), policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1,
        not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
    });
    Arc::new(IsolatedCall { home: home.to_owned(), context, agent: "alice".into(),
        roots: Arc::new(ResourceRoots::default()), parser: None, services: None,
        execution: None, worker: None, record_resolver: None })
}
fn payload(value: &Value) -> Value {
    serde_json::from_str(value["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn isolated_collection_handlers_preserve_employee_visibility() {
    use duduclaw_gateway::task_store::{ActivityRow, TaskRow, TaskStore};
    let home = tempfile::tempdir().unwrap();
    let store = TaskStore::open(home.path()).unwrap();
    for id in ["allowed", "ungranted"] {
        store.insert_task(&TaskRow::new(id.into(), id.into(), String::new(), "medium".into(),
            "alice".into(), "operator".into())).await.unwrap();
        store.append_activity(&ActivityRow { id: id.into(), event_type: "test".into(),
            agent_id: "alice".into(), task_id: Some(id.into()), summary: id.into(),
            timestamp: "2026-11-05T00:00:00Z".into(), metadata: None }).await.unwrap();
    }
    let actor = crate::mcp::RecordActor::Agent("alice");
    let expected_tasks = super::handle_tasks_list(&json!({"assigned_to":"*"}), home.path(), "alice", actor).await;
    let expected_activity = crate::mcp::handle_activity_list(&json!({"agent_id":"*"}), home.path(), "alice", actor).await;
    let expected_other = super::handle_tasks_list(&json!({"assigned_to":"bob"}), home.path(), "alice", actor).await;
    let expected_goals = crate::mcp::handle_goals_list(&json!({}), home.path()).await;
    ISOLATED_CALL.scope(call(home.path()), async {
        let actor = crate::mcp::RecordActor::Agent("alice");
        let listed = payload(&super::handle_tasks_list(&json!({"assigned_to":"*"}),
            home.path(), "alice", actor).await);
        assert_eq!(listed, payload(&expected_tasks));
        assert_eq!(listed["total"], 2);
        let activity = payload(&crate::mcp::handle_activity_list(&json!({"agent_id":"*"}),
            home.path(), "alice", actor).await);
        assert_eq!(activity, payload(&expected_activity));
        assert_eq!(super::handle_tasks_list(&json!({"assigned_to":"bob"}),
            home.path(), "alice", actor).await, expected_other);
        assert_eq!(crate::mcp::handle_goals_list(&json!({}), home.path()).await, expected_goals);
    }).await;
}
