use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
use serde_json::json;

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn employee() -> ResourceGrant { ResourceGrant::Employee { id: EmployeeId([2; 16]) } }
fn context(tool: &str, resources: Vec<ResourceGrant>) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: text("owner"), generation: 1 },
        employee_binding: EmployeeBinding::Personal(text("owner")), lane: Lane::Standard,
        tools: List::new(vec![text(tool)]).unwrap(), resources: List::new(resources).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [0; 32], jti: [0; 16],
    })
}
struct Fixed(Vec<ResourceGrant>);
impl ResourceResolver for Fixed {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(self.0.clone())
    }
}

#[test]
fn policy_covers_exactly_the_declared_catalog() {
    // Compile-time source inclusion keeps this small crate independent of the
    // gateway/core dependency graph while pinning the actual schema inventory.
    let sources = [
        include_str!("../../duduclaw-core/src/mcp_tool_defs/agents.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/channels.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/data.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/exec.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/fork.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/google.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/inference.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/integrations.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/knowledge.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/memory.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/odoo.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/ops.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/os.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/recording.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/skills.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/tasks.rs"),
        include_str!("../../duduclaw-core/src/mcp_tool_defs/wiki.rs"),
    ];
    let group_registry = include_str!("../../duduclaw-core/src/mcp_tool_defs/mod.rs");
    assert_eq!(group_registry.matches("::TOOLS").count(), sources.len(), "update inventory for new tool families");
    let mut names = std::collections::BTreeSet::new();
    for source in sources {
        for declaration in source.split("ToolDef {").skip(1) {
            let field = declaration.trim_start().strip_prefix("name:").expect("tool name first")
                .trim_start().strip_prefix('"').expect("quoted tool name");
            let name = field.split('"').next().unwrap();
            assert!(names.insert(name), "duplicate schema {name}");
            assert!(policy(name).is_some(), "unclassified schema {name}");
        }
    }
    assert_eq!(names.len(), POLICIES.len());
    for entry in POLICIES {
        assert!(names.contains(entry.name));
        assert!(!entry.required.is_empty());
    }
}

#[test]
fn unknown_names_and_missing_tool_grants_deny() {
    let ctx = context("agent_status", vec![employee()]);
    assert!(eligible(&ctx, "agent_status"));
    for name in ["agent_status_extra", "Agent_status", "agent_remove", "other_tool"] {
        assert!(!eligible(&ctx, name));
        assert!(authorize(&ctx, name, &json!({}), &Fixed(vec![employee()])).is_err());
    }
}

#[test]
fn resource_resolver_cannot_widen_or_omit_mandatory_resources() {
    let task = ResourceGrant::Task { id: text("allowed") };
    let ctx = context("tasks_complete", vec![employee(), task.clone()]);
    assert!(authorize(&ctx, "tasks_complete", &json!({"task_id":"allowed"}),
        &Fixed(vec![employee(), task.clone()])).is_ok());
    for resources in [vec![], vec![employee()], vec![task.clone()],
        vec![employee(), task.clone(), task],
        vec![employee(), ResourceGrant::Task { id: text("not-granted") }]] {
        assert!(authorize(&ctx, "tasks_complete", &json!({"task_id":"allowed"}), &Fixed(resources)).is_err());
    }
}

#[test]
fn selected_task_and_dependency_ids_cannot_be_replaced_by_resolver() {
    let resources = vec![employee(), ResourceGrant::Task { id: text("allowed") }];
    let ctx = context("tasks_update", resources.clone());
    for args in [json!({"task_id":"other"}), json!({"task_id":"allowed","depends_on":["other"]})] {
        assert!(authorize(&ctx, "tasks_update", &args, &Fixed(resources.clone())).is_err());
    }
}

#[test]
fn create_task_boundary_pins_the_record_task() {
    let own = ResourceGrant::Task { id: text("run-task") };
    let other = ResourceGrant::Task { id: text("other-task") };
    let with_task = |task: Option<&str>, resources: Vec<ResourceGrant>| {
        let mut record = context("create_task", resources).record().clone();
        record.task = task.map(|task| text::<128>(task));
        RequestContext::from_verified_record(record)
    };
    let args = json!({"goal":"g", "steps":[{"description":"s"}]});
    let ctx = with_task(Some("run-task"), vec![employee(), own.clone(), other.clone()]);
    assert!(eligible(&ctx, "create_task"));
    assert_eq!(create_task_selector(&ctx).unwrap(), own);
    assert!(authorize(&ctx, "create_task", &args, &Fixed(vec![employee(), own.clone()])).is_ok());
    // A resolver may not substitute, add or omit the task.
    for resources in [vec![employee(), other.clone()], vec![employee(), own.clone(), other.clone()], vec![employee()]] {
        assert!(authorize(&ctx, "create_task", &args, &Fixed(resources)).is_err());
    }
    // Workspace is no longer the gate: a workspace-only record stays ineligible.
    let workspace = ResourceGrant::Workspace { handle: text("employee-work") };
    assert!(!eligible(&with_task(None, vec![employee(), workspace.clone()]), "create_task"));
    assert!(!eligible(&with_task(Some("run-task"), vec![employee(), workspace, other.clone()]), "create_task"));
    assert!(!eligible(&with_task(None, vec![employee(), own.clone()]), "create_task"));
    // Selector-shaped arguments are refused before resolution.
    for key in TASK_SELECTOR_KEYS {
        let mut forged = args.clone();
        forged[key] = json!("run-task");
        assert!(authorize(&ctx, "create_task", &forged, &Fixed(vec![employee(), own.clone()])).is_err(), "{key}");
    }
}

// H9 owner decision 3: interactive `create_task` through the signed NewTask grant.
fn new_task(requester: &str) -> ResourceGrant { ResourceGrant::NewTask { requester: text(requester) } }
fn interactive(resources: Vec<ResourceGrant>) -> RequestContext { context("create_task", resources) }

#[test]
fn create_task_interactive_refuses_parent_and_goal_args() {
    let own = new_task("owner");
    let ctx = interactive(vec![scope(), employee(), own.clone()]);
    assert!(ctx.record().canonical_bytes().is_ok(), "an interactive employee-scope record is signable");
    assert!(eligible(&ctx, "create_task"));
    assert_eq!(new_task_grant(&ctx).unwrap(), own);
    let resolver = Fixed(vec![employee(), own.clone()]);
    let args = json!({"goal":"g", "steps":[{"description":"s", "depends_on":[0]}]});
    let call = authorize(&ctx, "create_task", &args, &resolver).unwrap();
    assert_eq!(call.resources(), &[employee(), own.clone()], "exactly the employee and the new-task grant");
    // A plan in the JSON-string form the handler also accepts.
    assert!(authorize(&ctx, "create_task", &json!({"goal":"g", "steps":"[{\"description\":\"s\"}]"}), &resolver).is_ok());
    // Selecting an existing task, goal or dependency is refused, even null.
    for key in TASK_SELECTOR_KEYS {
        for value in [json!("task-a"), json!(["task-a"]), Value::Null] {
            let mut forged = args.clone();
            forged[key] = value.clone();
            assert!(authorize(&ctx, "create_task", &forged, &resolver).is_err(), "{key}={value}");
        }
    }
    // Only the plan itself: no assignee, creator or other extra field.
    for key in ["assigned_to", "assignee", "created_by", "creator", "on_behalf_of", "requester", "audience", "owner",
        "schedule", "priority"] {
        let mut forged = args.clone();
        forged[key] = json!("someone");
        assert!(authorize(&ctx, "create_task", &forged, &resolver).is_err(), "{key}");
    }
    // Steps keep the existing step schema; no hidden parent/assignee per step.
    for step in [json!({"description":"s", "parent_task_id":"task-a"}), json!({"description":"s", "assigned_to":"other"}),
        json!({"description":"s", "agent_id":"other"}), json!("not an object")] {
        assert!(authorize(&ctx, "create_task", &json!({"goal":"g", "steps":[step]}), &resolver).is_err(), "{step}");
    }
    for plan in [json!({"goal":"g"}), json!({"goal":"", "steps":[{"description":"s"}]}), json!({"goal":"g", "steps":[]}),
        json!({"goal":"g", "steps":{"description":"s"}})] {
        assert!(authorize(&ctx, "create_task", &plan, &resolver).is_err(), "{plan}");
    }
    // The resolver can neither add a record task nor drop or swap the grant.
    for resources in [vec![employee()], vec![own.clone()], vec![employee(), own.clone(), ResourceGrant::Task { id: text("task-a") }],
        vec![employee(), new_task("other")], vec![employee(), ResourceGrant::Task { id: text("task-a") }]] {
        assert!(authorize(&ctx, "create_task", &args, &Fixed(resources)).is_err());
    }
    // Without the grant (or with one for another person) nothing is listed.
    for resources in [vec![scope(), employee()], vec![scope(), employee(), new_task("other")]] {
        let ctx = interactive(resources);
        assert!(!eligible(&ctx, "create_task"));
        assert!(authorize(&ctx, "create_task", &args, &resolver).is_err());
    }
}

#[test]
fn create_task_background_principal_refused() {
    // The record of a Background principal (no human requester) carrying a
    // NewTask grant is refused by decode and signing alike…
    let mut record = interactive(vec![scope(), employee(), new_task("owner")]).record().clone();
    record.principal = Principal::Background { tenant_id: text("owner"), generation: 1 };
    record.employee_binding = EmployeeBinding::Shared(text("owner"));
    assert!(record.canonical_bytes().is_err());
    assert!(serde_json::from_value::<CapabilityRecord>(serde_json::to_value(&record).unwrap()).is_err());
    // …and the boundary neither lists nor authorizes it.
    let ctx = RequestContext::from_verified_record(record);
    assert!(new_task_grant(&ctx).is_err());
    assert!(!eligible(&ctx, "create_task"));
    let args = json!({"goal":"g", "steps":[{"description":"s"}]});
    assert!(authorize(&ctx, "create_task", &args, &Fixed(vec![employee(), new_task("owner")])).is_err());
    // A confined or task-limited NewTask grant is not authority either.
    let mut record = interactive(vec![scope(), employee(),
        ResourceGrant::Confined { resource: Box::new(new_task("owner")) }]).record().clone();
    assert!(record.canonical_bytes().is_err());
    assert!(!eligible(&RequestContext::from_verified_record(record.clone()), "create_task"));
    record.resources = List::new(vec![employee(), new_task("owner")]).unwrap();
    record.task = Some(text("task-a"));
    assert!(record.canonical_bytes().is_err());
    assert!(!eligible(&RequestContext::from_verified_record(record), "create_task"));
}

#[test]
fn file_resources_preserve_workspace_only_and_reject_escape() {
    let resources = vec![ResourceGrant::Workspace { handle: text("work") }];
    let ctx = context("file_read", resources.clone());
    assert!(eligible(&ctx, "file_read"));
    assert!(authorize(&ctx, "file_read", &json!({"workspace":"work","path":"hello.txt"}), &Fixed(resources.clone())).is_ok());
    for args in [json!({"workspace":"other","path":"hello.txt"}),
        json!({"workspace":"work","path":"../secret"}), json!({"workspace":"work","path":"/secret"}),
        json!({"workspace":"work","path":"ok","agent_id":"other"})] {
        assert!(authorize(&ctx, "file_read", &args, &Fixed(resources.clone())).is_err());
    }
}

#[test]
fn db_source_and_table_are_exact_and_queries_require_finite_table_grants() {
    let resources = vec![employee(), ResourceGrant::Connector { account: text("db"), binding: EmployeeBinding::Personal(text("owner")) },
        ResourceGrant::DatabaseTable { database: text("sales"), table: text("orders") }];
    let ctx = context("db_select", resources.clone());
    assert!(authorize(&ctx, "db_select", &json!({"source":"sales","table":"orders"}), &Fixed(resources.clone())).is_ok());
    for args in [json!({"source":"other","table":"orders"}), json!({"source":"sales","table":"secrets"})] {
        assert!(authorize(&ctx, "db_select", &args, &Fixed(resources.clone())).is_err());
    }
}

#[test]
fn canonical_origins_refuse_credentials_paths_and_equivalent_spellings() {
    for origin in ["https://example.com", "http://127.0.0.1:8080", "https://[::1]:8080"] {
        assert!(validate_origin(origin).is_ok(), "{origin}");
    }
    for origin in ["https://example.com/", "https://example.com:443", "https://EXAMPLE.com",
        "https://user@example.com", "https://example.com/path", "https://example.com?q=1",
        "https://example.com#x", "file:///tmp/x", "data:text/plain,x"] {
        assert!(validate_origin(origin).is_err(), "{origin}");
    }
}

#[test]
fn url_call_cannot_substitute_a_granted_origin_or_smuggle_credentials() {
    let resources = vec![employee(), ResourceGrant::UrlOrigin { origin: text("https://example.com") }];
    let ctx = context("web_extract", resources.clone());
    assert!(authorize(&ctx, "web_extract", &json!({"url":"https://example.com/page"}), &Fixed(resources.clone())).is_ok());
    for url in ["https://example.com.evil.test/page", "https://user@example.com/", "file:///etc/passwd"] {
        assert!(authorize(&ctx, "web_extract", &json!({"url":url}), &Fixed(resources.clone())).is_err());
    }
}

#[test]
fn optional_recipients_and_wiki_output_require_additional_grants() {
    let resources = vec![employee(), ResourceGrant::Connector { account: text("google"), binding: EmployeeBinding::Personal(text("owner")) }];
    let ctx = context("calendar_create_event", resources.clone());
    assert!(authorize(&ctx, "calendar_create_event", &json!({"attendees":["other@example.test"]}), &Fixed(resources)).is_err());
    let ctx = context("meeting_transcribe", vec![employee()]);
    assert!(authorize(&ctx, "meeting_transcribe", &json!({"write_wiki":true}), &Fixed(vec![employee()])).is_err());
}

#[test]
fn shared_employee_cannot_use_personal_connector_even_with_forged_grant() {
    let resources = vec![employee(), ResourceGrant::Connector { account: text("personal"), binding: EmployeeBinding::Personal(text("owner")) }];
    let mut record = context("google_status", resources.clone()).record().clone();
    record.employee_binding = EmployeeBinding::Shared(text("tenant"));
    let ctx = RequestContext::from_verified_record(record);
    assert!(authorize(&ctx, "google_status", &json!({}), &Fixed(resources)).is_err());
}

#[test]
fn high_risk_tools_are_never_classified_as_plain_gateway_services() {
    for name in ["codrive_run", "fork_run", "spawn_agent", "os_open"] {
        assert_eq!(policy(name).unwrap().route, Route::Execution);
    }
    // HX5/HX6: in-run helpers (foreground and recording slot), never Service.
    for name in ["code_map", "execute_program", "office_script", "browser_record_start", "browser_record_stop"] {
        assert_eq!(policy(name).unwrap().route, Route::InRunHelper);
    }
    for name in ["transcribe_audio", "model_load", "llamafile_start"] {
        assert_eq!(policy(name).unwrap().route, Route::Worker);
    }
}

#[test]
fn job_control_and_pure_routing_do_not_require_parser_inputs() {
    for name in ["meeting_upload_link", "meeting_transcribe_status", "meeting_transcribe_cancel", "route_query"] {
        let entry = policy(name).unwrap();
        assert_eq!(entry.route, Route::Service, "{name}");
        assert_eq!(entry.required, &[ResourceKind::Employee], "{name}");
    }
    // Creation is authenticated job control; the background media stage requires E.
    assert_eq!(policy("meeting_transcribe").unwrap().route, Route::Service);
}

#[test]
fn raw_web_fetch_keeps_origin_authority_and_html_parsing_requires_worker() {
    let fetch = policy("web_fetch_cached").unwrap();
    assert_eq!(fetch.route, Route::Service);
    assert_eq!(fetch.required, &[ResourceKind::Employee, ResourceKind::UrlOrigin]);
    let extract = policy("web_extract").unwrap();
    assert_eq!(extract.route, Route::Worker);
    assert_eq!(extract.required, fetch.required);
}

#[test]
fn wildcard_and_empty_resource_identifiers_are_not_authority() {
    for resource in [ResourceGrant::Workspace { handle: text("*") },
        ResourceGrant::Task { id: text("") }, ResourceGrant::Recipient { id: text("*") },
        ResourceGrant::DatabaseTable { database: text("sales"), table: text("*") }] {
        assert!(validate_resource(&resource).is_err());
    }
    let ctx = context("file_read", vec![ResourceGrant::Workspace { handle: text("*") }]);
    assert!(!eligible(&ctx, "file_read"));
}

#[test]
fn target_employee_is_an_exact_resource_not_caller_identity() {
    let resources = vec![employee(), ResourceGrant::Employee { id: duduclaw_isolation::ids::EmployeeId([9; 16]) }];
    let ctx = context("agent_status", resources.clone());
    assert!(authorize(&ctx, "agent_status", &json!({"target_employee":"09090909090909090909090909090909"}), &Fixed(resources.clone())).is_ok());
    for target in ["main", "090909090909090909090909090909", "08080808080808080808080808080808"] {
        assert!(authorize(&ctx, "agent_status", &json!({"target_employee":target}), &Fixed(resources.clone())).is_err());
    }
    assert!(authorize(&ctx, "agent_status", &json!({"agent_id":"main"}), &Fixed(resources)).is_err());
}

#[test]
fn selected_connector_recipient_and_attachment_cannot_escape_grants() {
    let resources = vec![employee(), ResourceGrant::Connector { account: text("google-alice"),
        binding: EmployeeBinding::Personal(text("owner")) }, ResourceGrant::Mailbox { id: text("mail") },
        ResourceGrant::Recipient { id: text("allowed@example.org") }];
    let ctx = context("gmail_create_draft", resources.clone());
    assert!(authorize(&ctx, "gmail_create_draft", &json!({"to":"allowed@example.org","connector_account":"google-alice"}), &Fixed(resources.clone())).is_ok());
    for args in [json!({"to":"other@example.org"}), json!({"bcc":[{"email":"other@example.org"}]}),
        json!({"connector_account":"home"}), json!({"attachments":[{"path":"../secret"}]}),
        json!({"page_path":"/secret"}), json!({"output_path":"../secret"})] {
        assert!(authorize(&ctx, "gmail_create_draft", &args, &Fixed(resources.clone())).is_err());
    }
}

#[test]
fn wiki_page_uses_wiki_grant_and_still_rejects_path_escape() {
    let resources = vec![employee(), ResourceGrant::Wiki { namespace: text("agent:worker") }];
    let ctx = context("wiki_read", resources.clone());
    assert!(authorize(&ctx, "wiki_read", &json!({"page_path":"notes/page.md"}), &Fixed(resources.clone())).is_ok());
    for path in ["../secret", "/secret", "notes/../secret"] {
        assert!(authorize(&ctx, "wiki_read", &json!({"page_path":path}), &Fixed(resources.clone())).is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn async_resource_authority_is_fresh_and_off_the_reactor_thread() {
    struct Changing {
        reactor: std::thread::ThreadId,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl ResourceResolver for Changing {
        fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value)
            -> Result<Vec<ResourceGrant>, VerificationError> {
            assert_ne!(std::thread::current().id(), self.reactor);
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                Ok(vec![employee()])
            } else { Err(VerificationError::Revoked) }
        }
    }
    let resolver = std::sync::Arc::new(Changing { reactor: std::thread::current().id(),
        calls: std::sync::atomic::AtomicUsize::new(0) });
    let context = context("agent_status", vec![employee()]);
    assert!(authorize_async(&context, "agent_status", &json!({}), resolver.clone()).await.is_ok());
    assert!(authorize_async(&context, "agent_status", &json!({}), resolver.clone()).await.is_err());
    assert_eq!(resolver.calls.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[test]
fn explicit_task_grants_refuse_other_task_before_handler() {
    let resources = vec![employee(), ResourceGrant::Task { id: text("task-a") }];
    let ctx = context("tasks_update", resources.clone());
    let mut handler_calls = 0;
    for (task, allowed) in [("task-b", false), ("task-a", true)] {
        let resolved = vec![employee(), ResourceGrant::Task { id: text(task) }];
        let result = authorize(&ctx, "tasks_update", &json!({"task_id":task}), &Fixed(resolved));
        assert_eq!(result.is_ok(), allowed);
        if result.is_ok() { handler_calls += 1; }
    }
    assert_eq!(handler_calls, 1);
}

struct UnusedResolver;
impl ResourceResolver for UnusedResolver {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        panic!("employee-scope service uses the live legacy handler policy")
    }
}
#[test]
fn employee_scope_retains_verified_identity_and_live_service_authority() {
    let ctx = context("tasks_update", vec![ResourceGrant::EmployeeScope { id: EmployeeId([2;16]) }]);
    let call = authorize(&ctx, "tasks_update", &json!({"task_id":"task-b"}), &UnusedResolver).unwrap();
    assert!(call.employee_scope());
    assert_eq!(call.context().record().employee, ctx.record().employee);
    assert_eq!(call.arguments()["task_id"], "task-b");
    for key in ["principal", "root", "cwd", "employee_id", "run_id"] {
        assert!(authorize(&ctx, "tasks_update", &json!({key:"forged"}), &UnusedResolver).is_err());
    }
    assert!(!eligible(&ctx, "agent_remove"));
}
#[test]
fn employee_scope_does_not_grant_files_workers_or_execution() {
    for tool in ["file_read", "execute_program", "skill_hub_install"] {
        let ctx = context(tool, vec![ResourceGrant::EmployeeScope { id: EmployeeId([2;16]) }]);
        assert!(!eligible(&ctx, tool));
    }
}
#[test]
fn employee_scope_connectors_still_require_selected_account() {
    let ctx = context("google_status", vec![ResourceGrant::EmployeeScope { id: EmployeeId([2;16]) }]);
    assert!(authorize(&ctx, "google_status", &json!({}), &Fixed(vec![employee()])).is_err());
    let selected = vec![employee(), ResourceGrant::Connector { account: text("personal"),
        binding: EmployeeBinding::Personal(text("owner")) }];
    assert!(authorize(&ctx, "google_status", &json!({}), &Fixed(selected)).is_ok());
}

#[test]
fn employee_scope_keeps_explicit_file_and_worker_confinement() {
    let scope = ResourceGrant::EmployeeScope { id: EmployeeId([2;16]) };
    let workspace = ResourceGrant::Workspace { handle: text("work") };
    let origin = ResourceGrant::UrlOrigin { origin: text("https://example.com") };
    let ctx = context("file_read", vec![scope.clone(), workspace.clone()]);
    assert!(authorize(&ctx, "file_read", &json!({"workspace":"work","path":"note.txt"}),
        &Fixed(vec![workspace])).is_ok());
    let ctx = context("web_extract", vec![scope, employee(), origin.clone()]);
    assert!(authorize(&ctx, "web_extract", &json!({"url":"https://example.com/page"}),
        &Fixed(vec![employee(), origin.clone()])).is_ok());
    assert!(authorize(&ctx, "web_extract", &json!({"url":"https://other.example/page"}),
        &Fixed(vec![employee(), origin])).is_err());
}
#[test]
fn employee_scope_database_calls_keep_selected_account_and_live_table_policy() {
    let ctx = context("db_select", vec![ResourceGrant::EmployeeScope { id: EmployeeId([2;16]) }]);
    let selected = vec![employee(), ResourceGrant::Connector { account: text("database"),
        binding: EmployeeBinding::Personal(text("owner")) }];
    assert!(authorize(&ctx, "db_select", &json!({"source":"sales","table":"orders"}), &Fixed(selected.clone())).is_ok());
    assert!(authorize(&ctx, "db_select", &json!({"connector_account":"other"}), &Fixed(selected)).is_err());
}

#[test]
fn employee_scope_worker_outputs_need_separate_confined_grants() {
    let scope = ResourceGrant::EmployeeScope { id: EmployeeId([2;16]) };
    let wiki = ResourceGrant::Wiki { namespace: text("agent:worker") };
    let connector = ResourceGrant::Connector { account: text("google"), binding: EmployeeBinding::Personal(text("owner")) };
    let ctx = context("meeting_transcribe", vec![scope.clone(), employee()]);
    assert!(authorize(&ctx, "meeting_transcribe", &json!({"write_wiki":true}),
        &Fixed(vec![employee(), wiki.clone()])).is_err());
    let ctx = context("meeting_transcribe", vec![scope, employee(),
        ResourceGrant::Confined { resource: Box::new(wiki.clone()) },
        ResourceGrant::Confined { resource: Box::new(connector.clone()) }]);
    ctx.record().canonical_bytes().unwrap();
    assert!(authorize(&ctx, "meeting_transcribe", &json!({"write_wiki":true,"drive_file_id":"file-a"}),
        &Fixed(vec![employee(), wiki, connector])).is_ok());
    assert!(authorize(&ctx, "meeting_transcribe", &json!({"write_wiki":true}),
        &Fixed(vec![employee(), ResourceGrant::Wiki { namespace: text("shared") }])).is_err());
}

#[test]
fn i1c_source_proposals_are_host_control_and_skill_extraction_requires_own_wiki_grant() {
    assert_eq!(policy("knowledge_add_source").unwrap().route, Route::Service);
    for tool in ["skill_extract"] {
        assert_eq!(policy(tool).unwrap().route, Route::Worker, "{tool}");
    }
    let wiki = ResourceGrant::Wiki { namespace: text("agent:alice") };
    let without_wiki = vec![scope(), employee()];
    assert!(!eligible(&context("skill_extract", without_wiki), "skill_extract"));
    let with_wiki = vec![scope(), employee(), ResourceGrant::Confined { resource: Box::new(wiki.clone()) }];
    assert!(eligible(&context("skill_extract", with_wiki), "skill_extract"));
    // H12a: a finite (task-limited) record without EmployeeScope never lists it,
    // and a workspace grant is no longer the authority for skill files.
    let finite = vec![employee(), ResourceGrant::Workspace { handle: text("work") }, wiki];
    assert!(!eligible(&context("skill_extract", finite), "skill_extract"));
}

#[test]
fn source_and_skill_producers_require_finite_wiki_authority_in_employee_scope() {
    let wiki = ResourceGrant::Wiki { namespace: text("agent:alice") };
    let base = vec![ResourceGrant::EmployeeScope { id: EmployeeId([2; 16]) },
        employee(), ResourceGrant::Workspace { handle: text("work") }];
    for name in ["knowledge_add_source", "skill_extract"] {
        let missing = context(name, base.clone());
        assert!(missing.record().canonical_bytes().is_ok());
        assert!(!eligible(&missing, name));
        let mut ambiguous = base.clone();
        ambiguous.push(wiki.clone());
        let ambiguous = context(name, ambiguous);
        assert!(ambiguous.record().canonical_bytes().is_err());
        assert!(!eligible(&ambiguous, name));
        let mut confined = base.clone();
        confined.push(ResourceGrant::Confined { resource: Box::new(wiki.clone()) });
        let confined = context(name, confined);
        confined.record().canonical_bytes().unwrap();
        assert!(eligible(&confined, name));
        // H12a: skill_extract selects exactly its own employee + own wiki and
        // takes its skill name; knowledge_add_source keeps its workspace input.
        let (selected, args) = if name == "skill_extract" {
            (vec![employee(), wiki.clone()], json!({"skill_name":"notes"}))
        } else { (vec![employee(), base[2].clone(), wiki.clone()], json!({})) };
        assert!(authorize(&confined, name, &args, &Fixed(selected.clone())).is_ok());
        assert!(authorize(&missing, name, &args, &Fixed(selected)).is_err());
        assert!(authorize(&confined, name, &args, &Fixed(vec![employee(), base[2].clone(),
            ResourceGrant::Wiki { namespace: text("agent:other") }])).is_err());
    }
}

#[test]
fn i1c_host_controls_keep_real_worker_stages_and_original_resource_grants() {
    for name in ["skill_from_recording", "skill_hub_install", "meeting_transcribe", "skill_curator_status", "skill_synthesis_run"] {
        assert_eq!(policy(name).unwrap().route, Route::Service);
        assert!(policy(name).unwrap().required.contains(&ResourceKind::Employee));
    }
    // Parser stages stay in the background/control implementation. Routing
    // alone is never evidence that full synthesis or protected boot is ready.
}

#[test]
fn i1c_host_control_routes_cannot_widen_employee_scope() {
    for name in ["skill_from_recording", "skill_synthesis_run", "skill_hub_install",
        "skill_curator_status", "meeting_transcribe", "meeting_transcribe_status",
        "meeting_transcribe_cancel", "meeting_upload_link", "knowledge_add_source",
        "web_fetch_cached", "route_query"] {
        let ctx = context(name, vec![ResourceGrant::EmployeeScope { id: EmployeeId([2;16]) }]);
        assert!(!eligible(&ctx, name), "{name}");
        assert!(authorize(&ctx, name, &json!({}), &UnusedResolver).is_err(), "{name}");
    }
}

// ── H7 PublicOrigins (D-H2) ────────────────────────────────────────────────
fn public_origins(entries: &[&str]) -> ResourceGrant {
    use duduclaw_isolation::capability::PublicOrigin;
    ResourceGrant::PublicOrigins { origins: List::new(entries.iter().map(|e| PublicOrigin::parse(e).unwrap()).collect()).unwrap() }
}
fn scope() -> ResourceGrant { ResourceGrant::EmployeeScope { id: EmployeeId([2; 16]) } }

#[test]
fn web_fetch_refuses_origin_not_granted() {
    let grant = public_origins(&["https://example.com", "https://docs.example.org"]);
    for tool in PUBLIC_ORIGIN_TOOLS {
        let ctx = context(tool, vec![scope(), grant.clone()]);
        ctx.record().canonical_bytes().unwrap();
        assert!(eligible(&ctx, tool), "{tool}");
        let args = |url: &str| if *tool == "web_extract" { json!({"url":url,"selector":"p"}) } else { json!({"url":url}) };
        // No resolver is consulted: the selection is exactly the signed grants.
        for url in ["https://example.com/page?q=1", "https://docs.example.org/a#b", "https://EXAMPLE.com/x"] {
            let call = authorize(&ctx, tool, &args(url), &UnusedResolver).unwrap_or_else(|_| panic!("{tool} {url}"));
            assert_eq!(call.resources(), &[scope(), grant.clone()]);
        }
        for url in ["https://other.example.net/page", "https://sub.example.com/", "https://example.com.evil.net/",
            "https://evil.net/?https://example.com/", "http://example.com/", "https://example.com:8443/",
            "ftp://example.com/", "https://example.org/"] {
            assert!(authorize(&ctx, tool, &args(url), &UnusedResolver).is_err(), "{tool} {url}");
        }
        // The grant is not authority for any other tool or selector.
        for extra in [json!({"url":"https://example.com/","workspace":"work"}), json!({"url":"https://example.com/","to":"a@b.c"}),
            json!({"url_or_path":"https://example.com/"}), json!({"url":"https://example.com/","agent_id":"other"}), json!({})] {
            assert!(authorize(&ctx, tool, &extra, &UnusedResolver).is_err(), "{tool} {extra}");
        }
    }
    let ctx = context("web_fetch_cached", vec![scope(), grant.clone()]);
    assert!(authorize(&ctx, "web_fetch_cached", &json!({"url":"https://example.com/","selector":"p"}), &UnusedResolver).is_err());
    for tool in ["agent_status", "file_read", "skill_hub_install"] {
        assert!(!eligible(&context(tool, vec![grant.clone()]), tool), "{tool}");
    }
    // Without employee scope the record itself is invalid; no web authority.
    let ctx = context("web_extract", vec![employee(), grant]);
    assert!(!eligible(&ctx, "web_extract"));
    assert!(authorize(&ctx, "web_extract", &json!({"url":"https://example.com/","selector":"p"}), &UnusedResolver).is_err());
}

#[test]
fn public_origins_wildcard_still_refuses_ip_literal_userinfo_and_private_names() {
    let ctx = context("web_fetch_cached", vec![scope(), public_origins(&["*"])]);
    assert!(authorize(&ctx, "web_fetch_cached", &json!({"url":"https://news.example.org/a"}), &UnusedResolver).is_ok());
    for url in ["https://127.0.0.1/", "https://[::1]/", "https://8.8.8.8/", "https://0x7f.1/", "https://2130706433/",
        "https://user@example.org/", "https://user:pw@example.org/", "https://:pw@example.org/",
        "https://localhost/", "https://metadata.google.internal/", "https://printer.local/", "https://com/",
        "http://example.org/", "https://example.org:444/", " https://example.org/", "https://example.org/\n"] {
        assert!(authorize(&ctx, "web_fetch_cached", &json!({"url":url}), &UnusedResolver).is_err(), "{url:?}");
    }
}

#[test]
fn i1c_curator_run_flag_is_data_and_keeps_finite_grants() {
    // H12a: the curator is an own-employee service under EmployeeScope; the
    // selection is exactly the record's own Employee grant.
    let ctx = context("skill_curator_status", vec![scope(), employee()]);
    for args in [json!({}), json!({"run":true}), json!({"run":false})] {
        let call = authorize(&ctx, "skill_curator_status", &args, &Fixed(vec![employee()])).unwrap();
        assert_eq!(call.resources(), &[employee()]);
    }
    for args in [json!({"run":"true"}), json!({"run":null}), json!({"agent_id":"other"}),
        json!({"scope":"agent:bob"}), json!({"run":true,"employee_id":"x"})] {
        assert!(authorize(&ctx, "skill_curator_status", &args, &Fixed(vec![employee()])).is_err());
    }
    // A resolver cannot widen the selection to a workspace or another employee.
    for resources in [vec![employee(), ResourceGrant::Workspace { handle: text("work") }],
        vec![ResourceGrant::Employee { id: EmployeeId([7; 16]) }], vec![]] {
        assert!(authorize(&ctx, "skill_curator_status", &json!({"run":true}), &Fixed(resources)).is_err());
    }
    // Finite grants without EmployeeScope no longer reach the curator.
    let finite = context("skill_curator_status", vec![employee(), ResourceGrant::Workspace { handle: text("work") }]);
    assert!(!eligible(&finite, "skill_curator_status"));
    assert!(authorize(&finite, "skill_curator_status", &json!({}), &Fixed(vec![employee()])).is_err());
}

// ── H12a own-skill tools ───────────────────────────────────────────────────
fn own_wiki(namespace: &str) -> ResourceGrant {
    ResourceGrant::Confined { resource: Box::new(ResourceGrant::Wiki { namespace: text(namespace) }) }
}

#[test]
fn h12a_identity_never_from_args() {
    // The record's own employee is the only identity; arguments are data.
    for tool in OWN_SKILL_TOOLS {
        let ctx = context(tool, vec![scope(), employee(), own_wiki("agent:alice")]);
        ctx.record().canonical_bytes().unwrap();
        assert!(eligible(&ctx, tool), "{tool}");
        let selection = if *tool == "skill_extract" {
            vec![employee(), ResourceGrant::Wiki { namespace: text("agent:alice") }]
        } else { vec![employee()] };
        let good = if *tool == "skill_curator_status" { json!({"run":true}) } else { json!({"skill_name":"notes"}) };
        let call = authorize(&ctx, tool, &good, &Fixed(selection.clone())).unwrap_or_else(|_| panic!("{tool}"));
        assert!(call.resources().contains(&employee()), "{tool}");
        let mut forged = Vec::new();
        for key in ["employee_id", "principal", "run_id", "root", "cwd", "agent", "target_agent",
            "target_employee", "scope", "namespace", "workspace", "path", "recipient"] {
            let mut args = good.clone();
            args[key] = json!("bob");
            forged.push(args);
        }
        // An identity value in the legacy agent_id slot is shape-checked here and
        // must equal the bound employee in the producer; traversal never passes.
        let mut traversal = good.clone();
        traversal["agent_id"] = json!("../bob");
        forged.push(traversal);
        forged.push(json!([]));
        for args in forged {
            assert!(authorize(&ctx, tool, &args, &Fixed(selection.clone())).is_err(), "{tool} {args}");
        }
        // Another employee's grant can never be selected for this record.
        let other = ResourceGrant::Employee { id: EmployeeId([7; 16]) };
        assert!(authorize(&ctx, tool, &good, &Fixed(vec![other])).is_err(), "{tool}");
        // A record of another employee (wrong own grant) is not eligible.
        let foreign = context(tool, vec![scope(), ResourceGrant::Employee { id: EmployeeId([7; 16]) }, own_wiki("agent:alice")]);
        assert!(!eligible(&foreign, tool), "{tool}");
    }
    for tool in ["skill_security_scan", "skill_graduate", "skill_extract"] {
        assert!(check_own_skill_arguments(tool, &json!({"skill_name":"notes","agent_id":"alice"})).is_ok());
    }
    assert!(check_own_skill_arguments("shared_skill_adopt", &json!({"skill_name":"notes","target_agent":"alice"})).is_err());
    assert!(check_own_skill_arguments("shared_skill_adopt", &json!({"skill_name":"notes","agent_id":"alice"})).is_err());
    assert!(check_own_skill_arguments("skill_security_scan", &json!({"skill_name":"../notes"})).is_err());
    assert!(check_own_skill_arguments("skill_security_scan", &json!({})).is_err());
}

#[test]
fn h12a_skill_extract_requires_exactly_one_own_namespace_wiki() {
    let name = "skill_extract";
    let ctx = context(name, vec![scope(), employee(), own_wiki("agent:alice")]);
    let wiki = ResourceGrant::Wiki { namespace: text("agent:alice") };
    assert!(authorize(&ctx, name, &json!({"skill_name":"notes"}), &Fixed(vec![employee(), wiki.clone()])).is_ok());
    // Never the shared namespace, never an extra workspace or recipient.
    for selection in [vec![employee()], vec![employee(), wiki.clone(), ResourceGrant::Workspace { handle: text("work") }],
        vec![employee(), ResourceGrant::Wiki { namespace: text("shared") }]] {
        assert!(authorize(&ctx, name, &json!({"skill_name":"notes"}), &Fixed(selection)).is_err());
    }
    for resources in [vec![scope(), employee(), own_wiki("shared")],
        vec![scope(), employee(), own_wiki("agent:alice"), own_wiki("agent:bob")]] {
        assert!(!eligible(&context(name, resources), name));
    }
}
