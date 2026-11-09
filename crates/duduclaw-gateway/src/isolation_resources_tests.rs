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
fn isolated_resolver_rejects_implicit_latest_plan() {
    let (_home, resolver, context, _bindings) = fixture();
    let policy = duduclaw_mcp_boundary::tool_policy::policy("plan_get").unwrap();
    for args in [json!({}), json!({"plan_id":""})] {
        assert!(resolver.selected_tasks(&context, policy, &args).is_err());
    }
}

#[test]
fn isolated_resolver_responsibility_collection_intersects_grants_and_live_ownership() {
    let (home, resolver, context, _bindings) = fixture();
    let connection = Connection::open(home.path().join("tasks.db")).unwrap();
    connection.execute_batch("CREATE TABLE responsibilities(responsibility_id TEXT PRIMARY KEY, owner_agent_id TEXT NOT NULL);
        INSERT INTO responsibilities VALUES ('allowed','employee-a'),('unsigned','employee-a'),('foreign','employee-b');").unwrap();
    let policy = duduclaw_mcp_boundary::tool_policy::policy("responsibility_get").unwrap();
    assert!(resolver.selected_tasks(&context, policy, &json!({})).unwrap().is_empty());
    let mut record = context.record().clone();
    record.resources = List::new(vec![
        ResourceGrant::Task { id: text("responsibility:allowed".into()).unwrap() },
        ResourceGrant::Task { id: text("responsibility:foreign".into()).unwrap() },
    ]).unwrap();
    let context = RequestContext::from_verified_record(record);
    for args in [json!({}), json!({"responsibility_id":""}), json!({"responsibility_id":"  "}),
        json!({"responsibility_id":"allowed"})] {
        assert_eq!(resolver.selected_tasks(&context, policy, &args).unwrap(),
            vec![ResourceGrant::Task { id: text("responsibility:allowed".into()).unwrap() }]);
    }
    for id in ["foreign", "unsigned", "missing"] {
        assert!(resolver.selected_tasks(&context, policy, &json!({"responsibility_id":id})).is_err());
    }
    connection.execute("UPDATE responsibilities SET owner_agent_id='employee-b' WHERE responsibility_id='allowed'", []).unwrap();
    assert!(resolver.selected_tasks(&context, policy, &json!({})).unwrap().is_empty());
    assert!(resolver.selected_tasks(&context, policy, &json!({"responsibility_id":"allowed"})).is_err());
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

// H8: create_task live task selector.
fn create_task_context(context: &RequestContext, task: Option<&str>) -> RequestContext {
    let mut record = context.record().clone();
    record.task = task.map(|task| text(task.into()).unwrap());
    record.tools = List::new(vec![text("create_task".into()).unwrap()]).unwrap();
    RequestContext::from_verified_record(record)
}
fn plan() -> Value { json!({"goal":"ship the report", "steps":[{"description":"draft", "depends_on":[]}]}) }
fn task_grants(call: &duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall) -> Vec<&str> {
    call.resources().iter().filter_map(|r| match r { ResourceGrant::Task { id } => Some(id.as_str()), _ => None }).collect()
}

#[test]
fn create_task_selector_requires_live_ownership() {
    use duduclaw_mcp_boundary::tool_policy::eligible;
    let (home, resolver, context, _bindings) = fixture();
    let owned = create_task_context(&context, Some("allowed"));
    assert!(eligible(&owned, "create_task"));
    let call = authorize(&owned, "create_task", &plan(), &resolver).unwrap();
    assert_eq!(task_grants(&call), vec!["allowed"], "exactly the run's own task, nothing else");
    assert!(call.resources().contains(&ResourceGrant::Employee { id: owned.record().employee }));
    // No run task: hidden and refused (the caller cannot supply one).
    let taskless = create_task_context(&context, None);
    assert!(!eligible(&taskless, "create_task"));
    assert!(authorize(&taskless, "create_task", &plan(), &resolver).is_err());
    // A run task without the exact signed grant is not a selector.
    let mut record = owned.record().clone();
    record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
        ResourceGrant::Task { id: text("other".into()).unwrap() }]).unwrap();
    let unsigned = RequestContext::from_verified_record(record);
    assert!(!eligible(&unsigned, "create_task"));
    assert!(authorize(&unsigned, "create_task", &plan(), &resolver).is_err());
    // Ownership is read from the task store at use time, on every call.
    let tasks = Connection::open(home.path().join("tasks.db")).unwrap();
    tasks.execute("UPDATE tasks SET assigned_to='employee-b' WHERE id='allowed'", []).unwrap();
    assert!(authorize(&owned, "create_task", &plan(), &resolver).is_err(), "reassigned mid-run");
    tasks.execute("UPDATE tasks SET assigned_to='employee-a' WHERE id='allowed'", []).unwrap();
    assert!(authorize(&owned, "create_task", &plan(), &resolver).is_ok(), "no cached refusal or grant");
    tasks.execute("DELETE FROM tasks WHERE id='allowed'", []).unwrap();
    assert!(authorize(&owned, "create_task", &plan(), &resolver).is_err(), "deleted task");
    drop(tasks);
    // An unreadable store fails closed and is never created.
    std::fs::remove_file(home.path().join("tasks.db")).unwrap();
    assert!(authorize(&owned, "create_task", &plan(), &resolver).is_err());
    assert!(!home.path().join("tasks.db").exists());
}

#[test]
fn create_task_refuses_foreign_task() {
    let (_home, resolver, context, _bindings) = fixture();
    // The run's task is signed but assigned to another employee's agent.
    let foreign = create_task_context(&context, Some("other"));
    assert!(duduclaw_mcp_boundary::tool_policy::eligible(&foreign, "create_task"), "record-side half only");
    assert!(authorize(&foreign, "create_task", &plan(), &resolver).is_err());
    let policy = duduclaw_mcp_boundary::tool_policy::policy("create_task").unwrap();
    assert!(resolver.resolve(&foreign, policy, &plan()).is_err());
    // A signed grant for a task that does not exist is not ownership.
    let mut record = context.record().clone();
    record.task = Some(text("missing".into()).unwrap());
    record.tools = List::new(vec![text("create_task".into()).unwrap()]).unwrap();
    record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
        ResourceGrant::Task { id: text("missing".into()).unwrap() }]).unwrap();
    assert!(authorize(&RequestContext::from_verified_record(record), "create_task", &plan(), &resolver).is_err());
    // Arguments can never redirect the plan to another task, even an owned one.
    let owned = create_task_context(&context, Some("allowed"));
    for key in ["task_id", "parent_task_id", "goal_id", "parent_goal_id", "depends_on"] {
        for value in [json!("other"), json!("allowed"), json!(["other"]), Value::Null] {
            let mut args = plan();
            args[key] = value.clone();
            assert!(authorize(&owned, "create_task", &args, &resolver).is_err(), "{key}={value}");
            assert!(resolver.resolve(&owned, policy, &args).is_err(), "{key}={value}");
        }
    }
}

// H9 owner decision 3: interactive create_task (NewTask grant).
fn interactive_context(context: &RequestContext, requester: &str) -> RequestContext {
    let mut record = context.record().clone();
    record.task = None;
    record.tools = List::new(vec![text("create_task".into()).unwrap()]).unwrap();
    record.resources = List::new(vec![ResourceGrant::EmployeeScope { id: record.employee },
        ResourceGrant::Employee { id: record.employee },
        ResourceGrant::NewTask { requester: text(requester.into()).unwrap() }]).unwrap();
    RequestContext::from_verified_record(record)
}

#[test]
fn create_task_interactive_assignee_and_creator_fixed() {
    use duduclaw_mcp_boundary::tool_policy::eligible;
    let (home, resolver, context, _bindings) = fixture();
    let ctx = interactive_context(&context, "alice");
    ctx.record().canonical_bytes().unwrap();
    assert!(eligible(&ctx, "create_task"));
    let new_task = ResourceGrant::NewTask { requester: text("alice".into()).unwrap() };
    // Steps for this employee (named or defaulted) are a new plan of its own,
    // filed for the signed requester; tasks.db is never consulted.
    std::fs::remove_file(home.path().join("tasks.db")).unwrap();
    for steps in [json!([{"description":"draft"}]), json!([{"description":"draft", "agent":"employee-a"}, {"description":"send", "agent":""}])] {
        let call = authorize(&ctx, "create_task", &json!({"goal":"ship", "steps":steps}), &resolver).unwrap();
        assert_eq!(call.resources(), &[ResourceGrant::Employee { id: ctx.record().employee }, new_task.clone()]);
        assert!(task_grants(&call).is_empty(), "no existing task is selected");
    }
    assert!(!home.path().join("tasks.db").exists());
    // The assignee is fixed: no step may target another employee.
    let policy = duduclaw_mcp_boundary::tool_policy::policy("create_task").unwrap();
    for step in [json!({"description":"draft", "agent":"employee-b"}), json!({"description":"draft", "agent":"*"}),
        json!({"description":"draft", "agent":7})] {
        let args = json!({"goal":"ship", "steps":[step.clone()]});
        assert!(authorize(&ctx, "create_task", &args, &resolver).is_err(), "{step}");
        assert_eq!(resolver.resolve(&ctx, policy, &args).err(), Some(VerificationError::BindingMismatch), "{step}");
    }
    for key in ["agent_id", "assigned_to", "assignee"] {
        let mut step = json!({"description":"draft"});
        step[key] = json!("employee-b");
        let args = json!({"goal":"ship", "steps":[step]});
        assert!(resolver.resolve(&ctx, policy, &args).is_err(), "{key}");
    }
    // The creator is fixed: the grant must be the signed human principal's own,
    // so a grant naming someone else is no authority…
    let forged = interactive_context(&context, "mallory");
    assert!(!eligible(&forged, "create_task"));
    assert!(authorize(&forged, "create_task", &plan(), &resolver).is_err());
    assert!(resolver.resolve(&forged, policy, &plan()).is_err());
    // …and a Personal employee's requester must be its owner (B1), even with a
    // matching principal/grant pair for another person.
    let mut record = interactive_context(&context, "bob").record().clone();
    record.principal = Principal::Human { user_id: text("bob".into()).unwrap(), generation: 1 };
    let other_person = RequestContext::from_verified_record(record);
    assert!(authorize(&other_person, "create_task", &plan(), &resolver).is_err());
    assert!(resolver.resolve(&other_person, policy, &plan()).is_err());
}

// HX6: the recording's URL authority is the signed PublicOrigins grant.
fn recording_context(context: &RequestContext, public_web: bool) -> RequestContext {
    use duduclaw_isolation::capability::PublicOrigin;
    let mut record = context.record().clone();
    record.task = None;
    record.tools = List::new(vec![text("browser_record_start".into()).unwrap(), text("browser_record_stop".into()).unwrap()]).unwrap();
    let mut resources = vec![ResourceGrant::EmployeeScope { id: record.employee }, ResourceGrant::Employee { id: record.employee },
        ResourceGrant::Workspace { handle: text("employee-work".into()).unwrap() }];
    if public_web { resources.push(ResourceGrant::PublicOrigins { origins: List::new(vec![PublicOrigin::any()]).unwrap() }); }
    record.resources = List::new(resources).unwrap();
    RequestContext::from_verified_record(record)
}

#[test]
fn hx6url_gateway_resolver_adds_no_url_origin_for_recording_start() {
    let (_home, resolver, context, _bindings) = fixture();
    let ctx = recording_context(&context, true);
    ctx.record().canonical_bytes().unwrap();
    let args = json!({"url":"https://news.example.org/article?id=1","headless":true});
    let policy = duduclaw_mcp_boundary::tool_policy::policy("browser_record_start").unwrap();
    let resolved = resolver.resolve(&ctx, policy, &args).unwrap();
    assert!(!resolved.iter().any(|r| matches!(r, ResourceGrant::UrlOrigin { .. } | ResourceGrant::PublicOrigins { .. })), "{resolved:?}");
    let call = authorize(&ctx, "browser_record_start", &args, &resolver).unwrap();
    assert!(call.resources().iter().any(|r| matches!(r, ResourceGrant::PublicOrigins { .. })));
    assert!(call.resources().contains(&ResourceGrant::EmployeeScope { id: ctx.record().employee }));
    assert!(!call.resources().iter().any(|r| matches!(r, ResourceGrant::UrlOrigin { .. })));
    // public_web off ⇒ no PublicOrigins signed ⇒ the start is refused.
    assert!(authorize(&recording_context(&context, false), "browser_record_start", &args, &resolver).is_err());
    // Private, loopback and credentialed URLs are refused at the boundary.
    for url in ["https://127.0.0.1/", "https://10.0.0.1/", "https://[::1]/", "https://localhost/",
        "https://user:pw@news.example.org/", "http://news.example.org/"] {
        assert!(authorize(&ctx, "browser_record_start", &json!({"url":url}), &resolver).is_err(), "{url}");
    }
    // Every other tool keeps its per-URL UrlOrigin from the resolver.
    let web = duduclaw_mcp_boundary::tool_policy::policy("web_extract").unwrap();
    let resolved = resolver.resolve(&ctx, web, &json!({"url":"https://news.example.org/a"})).unwrap();
    assert!(resolved.contains(&ResourceGrant::UrlOrigin { origin: text("https://news.example.org".into()).unwrap() }));
}
