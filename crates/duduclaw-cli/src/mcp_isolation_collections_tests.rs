use super::*;
use crate::mcp_isolation::{IsolatedCall, ISOLATED_CALL};
use duduclaw_gateway::task_store::{ActivityRow, TaskRow};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, LiveRequestVerifier, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, resources::ResourceRoots,
    tool_policy::{self, ResourceResolver, ToolPolicy}};
use std::{path::Path, sync::{Arc, atomic::{AtomicBool, Ordering}}};

struct Live(Arc<AtomicBool>);
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
        if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Unavailable) }
    }
}
struct Grants;
impl ResourceResolver for Grants {
    fn resolve(&self, context: &RequestContext, _: &ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(context.record().resources.as_slice().to_vec())
    }
}
fn current(home: &Path, tasks: &[&str], scope: bool) -> (Arc<IsolatedCall>, Arc<AtomicBool>) {
    let employee = EmployeeId([2; 16]);
    let mut resources = vec![ResourceGrant::Employee { id: employee }];
    if scope { resources.push(ResourceGrant::EmployeeScope { id: employee }); }
    else { resources.extend(tasks.iter().map(|id| ResourceGrant::Task { id: Text::new((*id).into()).unwrap() })); }
    let live = Arc::new(AtomicBool::new(true));
    let context = RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee, run: RunId([3; 16]), task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(["tasks_list", "activity_list", "goals_list", "list_reminders", "responsibility_get"]
            .iter().map(|name| Text::new((*name).into()).unwrap()).collect()).unwrap(),
        resources: List::new(resources).unwrap(), policy_revision: 1, gateway_epoch: [4; 16],
        issued_at: 1, not_before: 1, expires_at: u64::MAX - 1, nonce: [5; 32], jti: [6; 16],
    }, Arc::new(Live(live.clone())));
    (Arc::new(IsolatedCall { forks: None, home: home.to_owned(), context, agent: "alice".into(),
        roots: Arc::new(ResourceRoots::default()), parser: None, services: None,
        worker: None, execution: None, record_resolver: None }), live)
}
fn authorized(current: &IsolatedCall, name: &str, arguments: Value) -> AuthorizedToolCall {
    tool_policy::authorize(&current.context, name, &arguments, &Grants).unwrap()
}
fn payload(output: Value) -> Value {
    assert_ne!(output["isError"], true, "{output}");
    serde_json::from_str(output["content"][0]["text"].as_str().unwrap()).unwrap()
}
async fn seed(store: &TaskStore, id: &str, agent: &str) {
    store.insert_task(&TaskRow::new(id.into(), id.into(), "description".into(), "medium".into(),
        agent.into(), "operator".into())).await.unwrap();
}
async fn event(store: &TaskStore, id: &str, task: Option<&str>, agent: &str, kind: &str) {
    store.append_activity(&ActivityRow { id: id.into(), task_id: task.map(str::to_owned),
        agent_id: agent.into(), event_type: kind.into(), summary: id.into(),
        timestamp: "2026-11-05T00:00:00Z".into(), metadata: None }).await.unwrap();
}

#[tokio::test]
async fn explicit_task_collection_preserves_shape_limits_and_audience_with_finite_sql_scope() {
    let home = tempfile::tempdir().unwrap();
    let store = TaskStore::open(home.path()).unwrap();
    for (id, agent) in [("a", "alice"), ("b", "alice"), ("foreign", "bob")] { seed(&store, id, agent).await; }
    let (current, _) = current(home.path(), &["a", "b", "foreign"], false);
    let call = authorized(&current, "tasks_list", json!({"assigned_to":"*", "limit":1}));
    // A newly added task after authorization cannot join the finite collection.
    seed(&store, "ungranted", "alice").await;
    // Existing audience behavior lets the task owner read even malformed packets.
    let packet_dir = home.path().join(duduclaw_core::task_packet::TEAM_PACKETS_DIR).join("a").join("1");
    std::fs::create_dir_all(&packet_dir).unwrap();
    std::fs::write(packet_dir.join("broken.json"), "{").unwrap();
    ISOLATED_CALL.scope(current.clone(), async {
        let output = payload(execute(&call).await.unwrap());
        assert_eq!(output["total"], 2);
        assert_eq!(output["tasks"].as_array().unwrap().len(), 1);
        assert!(matches!(output["tasks"][0]["id"].as_str(), Some("a" | "b")));
        assert!(output["filtered_by_agent"].is_null());
        let own = authorized(&current, "tasks_list", json!({"limit":100}));
        let output = payload(execute(&own).await.unwrap());
        assert_eq!(output["filtered_by_agent"], "alice");
        assert_eq!(output["tasks"].as_array().unwrap().len(), 2);
        assert_eq!(output["tasks"][0]["description"], "description");
        store.update_task("a", &json!({"assigned_to":"bob"})).await.unwrap();
        let output = payload(execute(&own).await.unwrap());
        assert_eq!(output["total"], 1);
        assert_eq!(output["tasks"][0]["id"], "b");
        let filtered = authorized(&current, "tasks_list", json!({"status":"done"}));
        assert_eq!(payload(execute(&filtered).await.unwrap())["total"], 0);
        let other = authorized(&current, "tasks_list", json!({"assigned_to":"bob"}));
        assert_eq!(payload(execute(&other).await.unwrap())["total"], 0);
    }).await;
}

#[tokio::test]
async fn explicit_activity_count_page_and_task_filter_share_finite_scope() {
    let home = tempfile::tempdir().unwrap();
    let store = TaskStore::open(home.path()).unwrap();
    for (id, agent) in [("a", "alice"), ("b", "alice"), ("foreign", "bob"), ("ungranted", "alice")] {
        seed(&store, id, agent).await;
    }
    for (id, task, agent, kind) in [("a1", Some("a"), "alice", "progress"),
        ("a2", Some("a"), "alice", "progress"), ("b1", Some("b"), "alice", "done"),
        ("foreign", Some("foreign"), "alice", "progress"), ("ungranted", Some("ungranted"), "alice", "progress"),
        ("other-author", Some("a"), "bob", "progress"), ("global", None, "alice", "progress")] {
        event(&store, id, task, agent, kind).await;
    }
    let (current, _) = current(home.path(), &["a", "b", "foreign"], false);
    let call = authorized(&current, "activity_list", json!({"event_type":"progress", "limit":1}));
    ISOLATED_CALL.scope(current.clone(), async {
        let output = payload(execute(&call).await.unwrap());
        assert_eq!(output["total"], 2);
        assert_eq!(output["activities"].as_array().unwrap().len(), 1);
        assert_eq!(output["activities"][0]["task_id"], "a");
        assert_eq!(output["activities"][0]["type"], "progress");
        let filtered = authorized(&current, "activity_list", json!({"task_id":"b", "limit":100}));
        let output = payload(execute(&filtered).await.unwrap());
        assert_eq!(output["total"], 1);
        assert_eq!(output["activities"][0]["id"], "b1");
        store.update_task("a", &json!({"assigned_to":"bob"})).await.unwrap();
        let output = payload(execute(&call).await.unwrap());
        assert_eq!(output["total"], 0);
        assert!(output["activities"].as_array().unwrap().is_empty());
    }).await;
}

#[tokio::test]
async fn employee_scope_defers_and_missing_mismatched_or_revoked_authority_refuses() {
    let home = tempfile::tempdir().unwrap();
    let (scoped, _) = current(home.path(), &[], true);
    let broad = authorized(&scoped, "tasks_list", json!({}));
    assert!(execute(&broad).await.is_none());
    let (current, live) = current(home.path(), &["a"], false);
    let call = authorized(&current, "tasks_list", json!({}));
    assert_eq!(execute(&call).await.unwrap()["isError"], true);
    ISOLATED_CALL.scope(scoped, async {
        assert_eq!(execute(&call).await.unwrap()["isError"], true);
    }).await;
    live.store(false, Ordering::SeqCst);
    ISOLATED_CALL.scope(current, async {
        assert_eq!(execute(&call).await.unwrap()["isError"], true);
    }).await;
    assert!(!home.path().join("tasks.db").exists(), "refused requests must not open/create the task database");
}


#[tokio::test]
async fn finite_goal_renderer_returns_only_goals_linked_to_signed_currently_owned_tasks() {
    let home = tempfile::tempdir().unwrap();
    let store = TaskStore::open(home.path()).unwrap();
    for id in ["goal-a", "goal-b"] {
        store.insert_goal(&duduclaw_gateway::task_store::GoalRow::new(id.into(), id.into(), "why".into())).await.unwrap();
    }
    for (id, goal) in [("a", "goal-a"), ("b", "goal-b")] {
        let mut row = TaskRow::new(id.into(), id.into(), "description".into(), "medium".into(), "alice".into(), "operator".into());
        row.goal_id = Some(goal.into()); store.insert_task(&row).await.unwrap();
    }
    let (current, _) = current(home.path(), &["a"], false);
    let call = authorized(&current, "goals_list", json!({"limit":1}));
    ISOLATED_CALL.scope(current, async {
        let output = payload(execute(&call).await.unwrap());
        assert_eq!(output["total"], 1);
        assert_eq!(output["goals"][0]["id"], "goal-a");
        assert_eq!(output["goals"][0]["description"], "why");
    }).await;
}

#[tokio::test]
async fn finite_reminder_renderer_keeps_legacy_shape_without_ungranted_rows() {
    let home = tempfile::tempdir().unwrap();
    let rows: Vec<String> = [("one", "alice"), ("other", "alice"), ("foreign", "bob")].iter().map(|(id, agent)|
        json!({"id":id,"agent_id":agent,"trigger_at":"2026-11-05T00:00:00Z",
            "channel":"telegram","chat_id":"chat","message":"remind","mode":"direct","status":"pending"}).to_string()).collect();
    std::fs::write(home.path().join("reminders.jsonl"), rows.join("\n")).unwrap();
    let (current, _) = current(home.path(), &["reminder:one", "reminder:foreign"], false);
    let call = authorized(&current, "list_reminders", json!({}));
    ISOLATED_CALL.scope(current, async {
        let output = payload(execute(&call).await.unwrap());
        assert_eq!(output.as_array().unwrap().len(), 1);
        assert_eq!(output[0]["id"], "one");
        assert_eq!(output[0]["message"], "remind");
        assert!(output[0].get("isolation_bound").is_none());
    }).await;
}
