use super::*;
use duduclaw_isolation::{bounded::List, capability::{CapabilityId, CapabilityRecord, EmployeeBinding,
    Lane, Principal}, ids::{RunId, Sha256Digest}};
use duduclaw_mcp_boundary::tool_policy::authorize;
use serde_json::json;
fn fixture() -> (tempfile::TempDir, GatewayResourceResolver, RequestContext, Arc<Mutex<BindingStore>>) {
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::Human { user_id: text("alice".into()).unwrap(), generation: 1 };
    let binding = EmployeeBinding::Personal(text("alice".into()).unwrap());
    let employee = EmployeeId([2; 16]);
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(employee, &binding).unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    let resolver = GatewayResourceResolver::new(home.path().to_owned(), employee, "employee-a".into(),
        bindings.clone()).unwrap();
    let connection = Connection::open(home.path().join("tasks.db")).unwrap();
    connection.execute_batch("CREATE TABLE tasks(id TEXT PRIMARY KEY, assigned_to TEXT NOT NULL);
        INSERT INTO tasks VALUES ('allowed','employee-a'),('other','employee-b');").unwrap();
    let context = RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee, run: RunId([3; 16]), task: None,
        principal, employee_binding: binding, lane: Lane::Standard,
        tools: List::new(vec![text("tasks_list".into()).unwrap(), text("tasks_complete".into()).unwrap(),
            text("memory_search".into()).unwrap(), text("wiki_read".into()).unwrap(), text("mail_send".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: employee },
            ResourceGrant::Task { id: text("allowed".into()).unwrap() },
            ResourceGrant::Task { id: text("other".into()).unwrap() },
            ResourceGrant::DatabaseTable { database: text("agent:employee-a".into()).unwrap(), table: text("memory".into()).unwrap() },
            ResourceGrant::Wiki { namespace: text("agent:employee-a".into()).unwrap() },
            ResourceGrant::Mailbox { id: text("agent:employee-a".into()).unwrap() },
            ResourceGrant::Recipient { id: text("employee-b".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    });
    (home, resolver, context, bindings)
}
#[test]
fn isolated_resolver_reads_live_task_ownership_and_never_expands_collection() {
    let (home, resolver, context, _bindings) = fixture();
    let call = authorize(&context, "tasks_list", &json!({}), &resolver).unwrap();
    let ids: Vec<_> = call.resources().iter().filter_map(|r| match r { ResourceGrant::Task { id } => Some(id.as_str()), _ => None }).collect();
    assert_eq!(ids, vec!["allowed"]);
    assert!(authorize(&context, "tasks_complete", &json!({"task_id":"other"}), &resolver).is_err());
    assert!(authorize(&context, "tasks_complete", &json!({"task_id":"allowed","depends_on":["other"]}), &resolver).is_err());
    Connection::open(home.path().join("tasks.db")).unwrap()
        .execute("UPDATE tasks SET assigned_to='employee-b' WHERE id='allowed'", []).unwrap();
    assert!(authorize(&context, "tasks_complete", &json!({"task_id":"allowed"}), &resolver).is_err());
}
#[test]
fn isolated_resolver_missing_store_does_not_create_it() {
    let (home, resolver, context, _bindings) = fixture();
    std::fs::remove_file(home.path().join("tasks.db")).unwrap();
    assert!(authorize(&context, "tasks_list", &json!({}), &resolver).is_err());
    assert!(!home.path().join("tasks.db").exists());
}
#[test]
fn isolated_resolver_uses_own_namespace_and_exact_recipients() {
    let (_home, resolver, context, _bindings) = fixture();
    assert!(authorize(&context, "memory_search", &json!({"query":"hello"}), &resolver).is_ok());
    assert!(authorize(&context, "wiki_read", &json!({"namespace":"agent:employee-b"}), &resolver).is_err());
    assert!(authorize(&context, "wiki_read", &json!({"scope":"shared"}), &resolver).is_err());
    assert!(authorize(&context, "mail_send", &json!({"to":"employee-b"}), &resolver).is_ok());
    for args in [json!({"to":"employee-c"}), json!({"to":"employee-b","bcc":"employee-c"}),
        json!({"to":"employee-b,employee-c"}), json!({"to":["employee-b",{"email":"employee-c"}]})] {
        assert!(authorize(&context, "mail_send", &args, &resolver).is_err());
    }
}
#[test]
fn isolated_resolver_rechecks_lifetime_binding() {
    let (_home, resolver, context, bindings) = fixture();
    assert!(authorize(&context, "memory_search", &json!({"query":"hello"}), &resolver).is_ok());
    bindings.lock().unwrap().tombstone_employee(context.record().employee).unwrap();
    assert!(authorize(&context, "memory_search", &json!({"query":"hello"}), &resolver).is_err());
}
#[test]
fn isolated_resolver_missing_connector_factory_refuses() {
    let (_home, resolver, context, _bindings) = fixture();
    assert!(resolver.connector(&context, "gmail_search", &json!({})).is_err());
}

#[test]
fn isolated_resolver_rejects_implicit_latest_plan_and_responsibility_collection() {
    let (_home, resolver, context, _bindings) = fixture();
    for tool in ["plan_get", "responsibility_get"] {
        let policy = duduclaw_mcp_boundary::tool_policy::policy(tool).unwrap();
        assert!(resolver.selected_tasks(&context, policy, &json!({})).is_err());
        let key = if tool == "plan_get" { "plan_id" } else { "responsibility_id" };
        assert!(resolver.selected_tasks(&context, policy, &json!({(key):""})).is_err());
    }
}

#[test]
fn isolated_resolver_assignment_cannot_escape_own_employee() {
    let (_home, resolver, context, _bindings) = fixture();
    for tool in ["tasks_create", "tasks_update", "tasks_list"] {
        let policy = duduclaw_mcp_boundary::tool_policy::policy(tool).unwrap();
        for assignment in [json!("employee-b"), Value::Null, json!(["employee-a"])] {
            assert!(resolver.resolve(&context, policy, &json!({"assigned_to":assignment})).is_err());
        }
    }
    let policy = duduclaw_mcp_boundary::tool_policy::policy("tasks_create").unwrap();
    assert!(resolver.resolve(&context, policy, &json!({"assigned_to":"employee-a","kind":"task"})).is_ok());
    assert!(resolver.resolve(&context, policy, &json!({"assigned_to":"*"})).is_err());
    for kind in ["discovery", "schedule", "unknown", ""] {
        assert!(resolver.resolve(&context, policy, &json!({"kind":kind})).is_err());
    }
    let policy = duduclaw_mcp_boundary::tool_policy::policy("tasks_list").unwrap();
    assert!(resolver.resolve(&context, policy, &json!({"assigned_to":"*"})).is_ok());
}
