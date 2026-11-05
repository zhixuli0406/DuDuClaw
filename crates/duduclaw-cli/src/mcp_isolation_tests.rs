use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Principal as RunPrincipal}, ids::{RunId, Sha256Digest}};
use duduclaw_mcp_boundary::server::Dispatcher;

#[path = "mcp_file_isolation_tests.rs"]
mod file_tests;

#[path = "mcp_isolation_parity_tests.rs"]
mod parity_tests;

#[test]
fn isolated_schema_preserves_original_record_target_selectors() {
    let mut tool = json!({"name":"agent_status", "inputSchema":{"properties":{
        "agent_id":{"type":"string"},"principal":{"type":"string"}},"required":["agent_id"]}});
    project_tool_schema(&mut tool);
    assert_eq!(tool["inputSchema"]["required"], json!(["agent_id"]));
    assert!(tool["inputSchema"]["properties"].get("agent_id").is_some());
    assert!(check_tool_arguments("agent_status", &json!({"agent_id":"other"})).is_ok());
    assert!(check_tool_arguments("agent_status", &json!({"principal":"other"})).is_err());
    let mut code = json!({"name":"code_map", "inputSchema":{"properties":{
        "query":{"type":"string"},"root":{"type":"string"}},"required":["query"]}});
    project_tool_schema(&mut code);
    assert_eq!(code["inputSchema"]["required"], json!(["query","workspace"]));
    assert!(code["inputSchema"]["properties"].get("root").is_none());
}

fn principal() -> Principal {
    Principal { client_id: duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID.into(),
        scopes: [crate::mcp_auth::Scope::FilesRead].into_iter().collect(),
        is_external: false, created_at: chrono::Utc::now() }
}
fn context() -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: RunPrincipal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Explore,
        tools: List::new(vec![Text::new("file_read".into()).unwrap(), Text::new("code_map".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [5; 32], jti: [6; 16],
    })
}
fn call() -> Arc<IsolatedCall> {
    Arc::new(IsolatedCall { forks: None, home: PathBuf::new(), execution: None, worker: None, record_resolver: None, context: context(), agent: "main".into(), roots: Arc::new(ResourceRoots::default()), parser: None, services: None })
}
fn adapter(home: &std::path::Path) -> IsolationDispatcher {
    std::fs::create_dir_all(home.join("agents/main")).unwrap();
    std::fs::write(home.join("agents/main/agent.toml"), "[agent]\nname = \"main\"\n").unwrap();
    IsolationDispatcher::new(home.to_owned(), EmployeeId([2; 16]), "main".into(),
        principal(), Arc::new(ResourceRoots::default())).unwrap()
}

#[tokio::test]
async fn isolated_identity_lane_and_scope_do_not_leak() {
    assert!(current().is_none());
    ISOLATED_CALL.scope(call(), async {
        assert_eq!(process_lane(), duduclaw_core::ProcessLane::Explore);
        assert!(crate::mcp::employee_process_from_env());
        assert_eq!(crate::mcp::resolve_audit_agent(|| "wrong".into()), "main");
        let home = tempfile::tempdir().unwrap();
        let actor = crate::mcp::record_actor_for(home.path(), "gateway-internal", "main");
        assert_eq!(actor.agent(), Some("main"));
        let wrong = crate::mcp::record_actor_for(home.path(), "gateway-internal", "other");
        assert_eq!(wrong.agent(), Some(duduclaw_core::UNTRUSTED_AGENT_ID));
        assert!(tool_allowed("file_read", &principal()));
        assert!(!tool_allowed("code_map", &principal()));
        assert!(!tool_allowed("office_script", &principal()));
        let mut denied = principal();
        denied.scopes.clear();
        assert!(!tool_allowed("file_read", &denied));
    }).await;
    assert!(current().is_none());
}

#[tokio::test]
async fn isolated_adapter_rejects_forged_authority_and_employee_mismatch() {
    let home = tempfile::tempdir().unwrap();
    let adapter = adapter(home.path());
    for key in ["agent_id", "principal", "root", "cwd", "run_id", "employee_id"] {
        let mut args = json!({"workspace": "work", "path": "file.txt"});
        args[key] = json!("other");
        let result = adapter.dispatch(context(), "main", json!({"jsonrpc":"2.0", "id":1,
            "method":"tools/call", "params":{"name":"file_read", "arguments":args}})).await;
        assert!(result.is_err(), "{key}");
    }
    assert!(adapter.dispatch(context(), "other", json!({"jsonrpc":"2.0", "id":1,
        "method":"ping"})).await.is_err());
    let mut wrong = context().record().clone();
    wrong.employee = EmployeeId([9; 16]);
    assert!(adapter.dispatch(RequestContext::from_verified_record(wrong), "main",
        json!({"jsonrpc":"2.0", "id":1, "method":"ping"})).await.is_err());
}

#[tokio::test]
async fn isolated_list_and_call_refuse_unmigrated_tools() {
    let home = tempfile::tempdir().unwrap();
    let adapter = adapter(home.path());
    let listed = adapter.dispatch(context(), "main", json!({"jsonrpc":"2.0", "id":1,
        "method":"tools/list"})).await.unwrap().unwrap();
    let names = crate::mcp::advertised_tool_names(&listed);
    assert_eq!(names, vec!["file_read"]);
    assert!(names.iter().all(|name| name == "file_read"));
    for name in ["code_map", "office_script", "codrive_run", "unknown"] {
        let result = adapter.dispatch(context(), "main", json!({"jsonrpc":"2.0", "id":2,
            "method":"tools/call", "params":{"name":name,"arguments":{}}})).await.unwrap().unwrap();
        assert!(result.get("error").is_some(), "{name}");
        assert!(!names.iter().any(|listed| listed == name));
    }
}

#[tokio::test]
async fn isolated_file_read_never_falls_back_to_host_paths() {
    let home = tempfile::tempdir().unwrap();
    let secret = home.path().join("secret.txt");
    std::fs::write(&secret, "SHOULD_NOT_BE_READ").unwrap();
    ISOLATED_CALL.scope(call(), async {
        for path in [secret.to_string_lossy().into_owned(), "../secret.txt".into(), "secret.txt".into()] {
            let result = crate::mcp_files::handle_file_read(&json!({"workspace":"work", "path":path}), home.path(), "main");
            assert_eq!(result["isError"], true);
            assert!(!result.to_string().contains("SHOULD_NOT_BE_READ"));
        }
        let result = crate::mcp_files::handle_file_read(&json!({"workspace":"work", "path":"sheet.csv"}), home.path(), "main");
        assert_eq!(result["isError"], true);
    }).await;
}

#[tokio::test]
async fn isolated_constructor_preserves_scope_ceiling_and_rejects_external() {
    let home = tempfile::tempdir().unwrap();
    let adapter = adapter(home.path());
    assert_eq!(adapter.principal.scopes, principal().scopes);
    let mut external = principal();
    external.is_external = true;
    assert!(IsolationDispatcher::new(home.path().to_owned(), EmployeeId([2; 16]), "main".into(),
        external, Arc::new(ResourceRoots::default())).is_err());
    let mut wrong = principal();
    wrong.client_id = "other".into();
    assert!(IsolationDispatcher::new(home.path().to_owned(), EmployeeId([2; 16]), "main".into(),
        wrong, Arc::new(ResourceRoots::default())).is_err());
}

#[tokio::test]
async fn isolated_constructor_refuses_broken_redaction_config() {
    let home = tempfile::tempdir().unwrap();
    let _adapter = adapter(home.path());
    std::fs::write(home.path().join("config.toml"), "[redaction]\nenabled = \"bad\"\n").unwrap();
    assert!(IsolationDispatcher::new(home.path().to_owned(), EmployeeId([2; 16]), "main".into(),
        principal(), Arc::new(ResourceRoots::default())).is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn isolated_file_read_uses_granted_fd_and_keeps_bounded_result() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::write(work.path().join("note.txt"), "abcdef").unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), EmployeeId([2; 16]), std::fs::File::open(work.path()).unwrap()).unwrap();
    let call = Arc::new(IsolatedCall { forks: None, home: PathBuf::new(), execution: None, worker: None, record_resolver: None, context: context(), agent: "main".into(), roots: Arc::new(roots), parser: None, services: None });
    ISOLATED_CALL.scope(call, async {
        let value = crate::mcp_files::handle_file_read(
            &json!({"workspace":"work", "path":"note.txt", "max_bytes":3}), home.path(), "main");
        assert_ne!(value["isError"], true);
        let body: Value = serde_json::from_str(value["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["text"], "abc");
        assert_eq!(body["truncated"], true);
        let wrong = crate::mcp_files::handle_file_read(
            &json!({"workspace":"work", "path":"note.txt"}), home.path(), "other");
        assert_eq!(wrong["isError"], true);
    }).await;
}

#[tokio::test]
async fn isolated_denials_keep_existing_rate_limit_and_redacted_audit() {
    let home = tempfile::tempdir().unwrap();
    let adapter = adapter(home.path());
    let request = json!({"jsonrpc":"2.0", "id":1, "method":"tools/call",
        "params":{"name":"unknown", "arguments":{"data":"UNTRUSTED_SECRET_MARKER"}}});
    let denied = adapter.dispatch(context(), "main", request.clone()).await.unwrap().unwrap();
    assert_eq!(denied["error"]["code"], -32003);
    // Drain the shared legacy bucket without relying on wall-clock sleeps.
    while adapter.dispatcher.rate_limiter.check(&adapter.principal.client_id,
        crate::mcp_rate_limit::OpType::Read).is_ok() {}
    let limited = adapter.dispatch(context(), "main", request).await.unwrap().unwrap();
    assert_eq!(limited["error"]["code"], -32029);
    // The denial audit must not retain employee-controlled arguments.
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(audit.contains("isolation_boundary"));
    assert!(!audit.contains("UNTRUSTED_SECRET_MARKER"));
}

#[test]
fn isolated_every_tool_def_has_exact_boundary_route_and_effect() {
    let mut seen = std::collections::BTreeSet::new();
    // CLI tools_def re-exports this dependency-neutral registry. Iterate the
    // actual ToolDefs, not a source-text count or a duplicated name fixture.
    for tool in crate::mcp::tools() {
        assert!(seen.insert(tool.name), "duplicate ToolDef: {}", tool.name);
        assert!(boundary_route(tool.name).is_some(), "missing route: {}", tool.name);
        assert!(duduclaw_core::tool_effect::effect_of_builtin(tool.name).is_some(),
            "missing effect: {}", tool.name);
    }
    assert!(!seen.is_empty());
    for policy in duduclaw_mcp_boundary::tool_policy::POLICIES {
        assert!(seen.contains(policy.name), "stale boundary route: {}", policy.name);
    }
    for name in ["unknown", "tasks_list_extra", "xmemory_search", "TASKS_LIST"] {
        assert_eq!(boundary_route(name), None);
    }
    for name in ["hardware_info", "model_search", "model_recommend"] {
        if cfg!(target_os = "linux") {
            assert!(matches!(boundary_route(name), Some(BoundaryRoute::GatewayHandler(_))));
        } else { assert_eq!(boundary_route(name), Some(BoundaryRoute::Execution)); }
    }
    for name in ["execute_program"] {
        assert_eq!(boundary_route(name), Some(BoundaryRoute::Execution));
    }
    for name in ["cost_recent", "cost_users", "wiki_read", "identity_resolve", "computer_session_start", "computer_workspace_write"] {
        assert!(matches!(boundary_route(name), Some(BoundaryRoute::GatewayHandler(_))));
    }
    assert!(matches!(boundary_route("google_status"), Some(BoundaryRoute::GatewayHandler(_))));
}

struct RecordProbe;
impl duduclaw_mcp_boundary::tool_policy::ResourceResolver for RecordProbe {
    fn resolve(&self, context: &RequestContext, _: &duduclaw_mcp_boundary::tool_policy::ToolPolicy,
        arguments: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        let mut resources = vec![ResourceGrant::Employee { id: context.record().employee }];
        if let Some(task) = arguments.get("task_id").and_then(Value::as_str) {
            resources.push(ResourceGrant::Task { id: Text::new(task.to_string()).unwrap() });
        }
        Ok(resources)
    }
}

#[tokio::test]
async fn isolated_records_use_employee_authority_not_run_record_grants() {
    let home = tempfile::tempdir().unwrap();
    let mut record = context().record().clone();
    record.lane = Lane::Standard;
    record.tools = List::new(vec![Text::new("tasks_update".into()).unwrap()]).unwrap();
    record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
        ResourceGrant::Task { id: Text::new("task-a".into()).unwrap() }]).unwrap();
    let call = Arc::new(IsolatedCall { forks: None, home: home.path().into(), context: RequestContext::from_verified_record(record),
        agent: "main".into(), roots: Arc::new(ResourceRoots::default()), parser: None,
        services: None, execution: None, worker: None, record_resolver: Some(Arc::new(RecordProbe)) });
    ISOLATED_CALL.scope(call, async {
        assert!(dispatch_resource_tool(home.path(), "tasks_update", &json!({"task_id":"task-a"})).await.is_none());
        assert!(dispatch_resource_tool(home.path(), "tasks_update", &json!({"task_id":"task-b"})).await.is_none());
        // The original handler still checks record ownership/delegation.
        let denied = dispatch_resource_tool(home.path(), "tasks_update", &json!({"principal":"operator"})).await.unwrap();
        assert_eq!(denied["isError"], true);
    }).await;
}

#[tokio::test]
async fn isolated_record_route_reuses_original_dispatch_and_handler() {
    let home = tempfile::tempdir().unwrap();
    let mut adapter = adapter(home.path());
    adapter.principal.scopes = [crate::mcp_auth::Scope::Admin].into_iter().collect();
    adapter = adapter.with_record_resolver(Arc::new(RecordProbe));
    std::fs::write(home.path().join("agents/main/agent.toml"),
        "[agent]\nname = \"main\"\n[capabilities]\nallowed_tools = [\"mcp__duduclaw__*\"]\n").unwrap();
    let mut record = context().record().clone();
    record.tools = List::new(vec![Text::new("plan_start".into()).unwrap()]).unwrap();
    record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee }]).unwrap();
    let request = json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"plan_start","arguments":{"task":"isolation plan"}}});
    let response = adapter.dispatch(RequestContext::from_verified_record(record.clone()), "main", request.clone()).await.unwrap().unwrap();
    assert!(response.get("error").is_none(), "{response}");
    let body: Value = serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["task"], "isolation plan");
    assert_eq!(body["clarify_first"], true);
    std::fs::write(home.path().join("agents/main/agent.toml"),
        "[agent]\nname = \"main\"\n[capabilities]\ndenied_tools = [\"plan_start\"]\n").unwrap();
    let response = adapter.dispatch(RequestContext::from_verified_record(record), "main", request).await.unwrap().unwrap();
    assert!(response.get("error").is_some(), "{response}");
}

#[test]
fn isolated_round4_inventory_has_no_implicit_resource_fallback() {
    use crate::mcp_isolation_disposition::intentional_refusal;
    let tools: Vec<_> = crate::mcp::tools().collect();
    assert_eq!(tools.len(), 259, "review every new tool's isolation disposition");
    for tool in tools {
        let route = boundary_route(tool.name).expect("exact route");
        if intentional_refusal(tool.name).is_some() { continue; }
        if route == BoundaryRoute::Resource {
            assert!(tool.name == "wiki_read"
                || duduclaw_gateway::isolation_connectors::provider_for_tool(tool.name).is_some(),
                "resource tool has neither production adapter nor intentional refusal: {}", tool.name);
        }
    }
}

#[tokio::test]
async fn isolated_round4_intentional_refusals_stay_hidden_with_grants() {
    use crate::mcp_isolation_disposition::intentional_refusal;
    for tool in crate::mcp::tools() {
        if let Some(reason) = intentional_refusal(tool.name) {
            let mut record = context().record().clone();
            record.tools = List::new(vec![Text::new(tool.name.into()).unwrap()]).unwrap();
            record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee }]).unwrap();
            let call = Arc::new(IsolatedCall { forks: None, home: PathBuf::new(), context: RequestContext::from_verified_record(record),
                agent: "main".into(), roots: Arc::new(ResourceRoots::default()), parser: None,
                services: None, execution: None, worker: None, record_resolver: Some(Arc::new(RecordProbe)) });
            ISOLATED_CALL.scope(call, async {
                assert!(!tool_allowed(tool.name, &principal()), "{}", tool.name);
                let result = dispatch_resource_tool(std::path::Path::new(""), tool.name, &json!({})).await.unwrap();
                assert_eq!(result["isError"], true);
                assert_eq!(result["content"][0]["text"], reason.message());
            }).await;
        }
    }
}

#[test]
fn isolated_round4_boot_manifest_distinguishes_refusal_from_missing_factory() {
    let home = tempfile::tempdir().unwrap();
    let dispatcher = adapter(home.path());
    let manifest = dispatcher.service_manifest();
    assert_eq!(manifest.len(), 259);
    assert_eq!(manifest.iter().find(|(name, _)| *name == "file_read").unwrap().1, ServiceState::Ready);
    assert_eq!(manifest.iter().find(|(name, _)| *name == "execute_program").unwrap().1, ServiceState::MissingFactory);
    assert_eq!(manifest.iter().find(|(name, _)| *name == "transcribe_audio").unwrap().1, ServiceState::MissingFactory);
    assert!(matches!(manifest.iter().find(|(name, _)| *name == "os_power").unwrap().1,
        ServiceState::IntentionalRefusal(_)));
    assert!(dispatcher.require_complete_services().is_err());
}

#[tokio::test]
async fn isolated_mail_read_preserves_original_employee_ownership_rule() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\ndefault_agent = \"main\"\n[mail]\nenabled = true\n").unwrap();
    let row: duduclaw_gateway::mail::MailRow = serde_json::from_value(json!({
        "kind":"received", "mail_id":"unowned", "at":"2026-11-05T00:00:00Z", "agent_id":"other",
        "from":"sender@example.com", "subject":"private", "body":"PRIVATE_UNOWNED_BODY", "source":"dropfolder"
    })).unwrap();
    duduclaw_gateway::mail::append_row(home.path(), &row);
    let isolated = Arc::new(IsolatedCall { forks: None, home: home.path().into(), context: context(), agent: "main".into(),
        roots: Arc::new(ResourceRoots::default()), parser: None, services: None,
        execution: None, worker: None, record_resolver: None });
    ISOLATED_CALL.scope(isolated, async {
        let result = crate::mcp::handle_mail_read(&json!({"mail_id":"unowned"}), home.path(), "main").await;
        assert_eq!(result["isError"], true);
        assert!(!result.to_string().contains("PRIVATE_UNOWNED_BODY"));
    }).await;
    assert!(!duduclaw_gateway::mail::get_inbox_item(home.path(), "unowned").unwrap().read);
}

#[tokio::test]
async fn isolated_round5_workflow_session_is_host_only_and_bound_to_one_run() {
    let home = tempfile::tempdir().unwrap();
    let mut adapter = adapter(home.path());
    adapter.principal.scopes = [crate::mcp_auth::Scope::Admin].into_iter().collect();
    std::fs::write(home.path().join("agents/main/agent.toml"),
        "[agent]\nname = \"main\"\n[capabilities]\nallowed_tools = [\"mcp__duduclaw__*\"]\n").unwrap();
    let mut record = context().record().clone();
    record.lane = Lane::Standard;
    record.tools = List::new(vec![Text::new("plan_start".into()).unwrap()]).unwrap();
    let secret = "ab".repeat(32);
    let adapter = adapter.with_workflow_session(record.run, "host-session".into(), secret.clone()).unwrap();
    let request = json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
        "name":"plan_start", "arguments":{"task":"workflow parity"},
        "_meta":{"duduclaw_workflow":{"phase":"prepare", "version":1, "context":{
            "run_id":"workflow-run", "step_key":"step", "input_hash":"hash",
            "session_id":"different-session", "expires_at":"2099-01-01T00:00:00Z", "mac":"invalid"
        }}}
    }});
    let matching = adapter.dispatch(RequestContext::from_verified_record(record.clone()), "main", request.clone())
        .await.unwrap().unwrap();
    assert!(matching.to_string().contains("workflow session mismatch"), "{matching}");
    assert!(!matching.to_string().contains(&secret));
    record.run = RunId([9; 16]);
    let other = adapter.dispatch(RequestContext::from_verified_record(record), "main", request)
        .await.unwrap().unwrap();
    assert!(other.to_string().contains("workflow extension unavailable"), "{other}");
    assert!(!other.to_string().contains(&secret));
}

#[test]
fn isolated_i1b_host_routes_preserve_full_inventory_and_missing_helpers() {
    let home = tempfile::tempdir().unwrap();
    let dispatcher = adapter(home.path());
    let manifest = dispatcher.service_manifest();
    // The original 22 A operations remain advertised in the implementation
    // inventory. Only concrete queue/store/control/host-query handlers are ready here.
    for name in ["create_task", "spawn_agent", "spawn_ephemeral", "discovery_catalog", "discovery_list",
        "discovery_tree", "discovery_cancel"] {
        assert!(matches!(boundary_route(name), Some(BoundaryRoute::GatewayHandler(_))), "{name}");
        assert_eq!(manifest.iter().find(|(tool, _)| *tool == name).unwrap().1,
            ServiceState::Ready, "{name}");
    }
    for name in ["hardware_info", "model_search", "model_recommend"] {
        if cfg!(target_os = "linux") {
            assert!(matches!(boundary_route(name), Some(BoundaryRoute::GatewayHandler(_))), "{name}");
            assert_eq!(manifest.iter().find(|(tool, _)| *tool == name).unwrap().1, ServiceState::Ready, "{name}");
        } else {
            assert_eq!(boundary_route(name), Some(BoundaryRoute::Execution), "{name}");
            assert_eq!(manifest.iter().find(|(tool, _)| *tool == name).unwrap().1, ServiceState::MissingFactory, "{name}");
        }
    }
    for name in ["fork_run", "merge_or_select", "terminate_branch"] {
        assert!(matches!(boundary_route(name), Some(BoundaryRoute::GatewayHandler(_))), "{name}");
        assert_eq!(manifest.iter().find(|(tool, _)| *tool == name).unwrap().1,
            ServiceState::IntentionalRefusal(crate::mcp_isolation_disposition::Refusal::ForkNextRelease), "{name}");
    }
    for name in ["inspect_branches", "diff_branches", "fork_cost"] {
        assert!(matches!(boundary_route(name), Some(BoundaryRoute::GatewayHandler(_))), "{name}");
        assert_eq!(manifest.iter().find(|(tool, _)| *tool == name).unwrap().1, ServiceState::MissingFactory, "{name}");
    }
    for name in ["execute_program", "office_script", "browser_record_start", "browser_record_stop",
        "code_map"] {
        assert_eq!(boundary_route(name), Some(BoundaryRoute::Execution), "{name}");
        assert_eq!(manifest.iter().find(|(tool, _)| *tool == name).unwrap().1,
            ServiceState::MissingFactory, "{name}");
    }
    if cfg!(target_os = "linux") {
        assert!(matches!(boundary_route("discovery_artifact"), Some(BoundaryRoute::GatewayHandler(_))));
        assert_eq!(manifest.iter().find(|(tool, _)| *tool == "discovery_artifact").unwrap().1, ServiceState::Ready);
    } else {
        assert_eq!(manifest.iter().find(|(tool, _)| *tool == "discovery_artifact").unwrap().1, ServiceState::MissingFactory);
    }
    assert!(dispatcher.require_complete_services().is_err());
}

#[tokio::test]
async fn isolated_i1b_round6_dispatch_localizes_deferred_forks_even_with_explicit_grants() {
    let home = tempfile::tempdir().unwrap();
    let mut dispatcher = adapter(home.path());
    dispatcher.principal.scopes = [crate::mcp_auth::Scope::Admin].into_iter().collect();
    let mut record = context().record().clone();
    record.lane = Lane::Standard;
    record.tools = List::new(duduclaw_mcp_boundary::fork_policy::DEFERRED_TOOLS.into_iter()
        .map(|name| Text::new(name.into()).unwrap()).collect()).unwrap();
    record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
        ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }]).unwrap();
    for language in ["zh-TW", "en", "ja"] {
        std::fs::write(home.path().join("config.toml"),
            format!("[general]\ndefault_language = '{language}'\n")).unwrap();
        for name in duduclaw_mcp_boundary::fork_policy::DEFERRED_TOOLS {
            let response = dispatcher.dispatch(RequestContext::from_verified_record(record.clone()), "main",
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":name,"arguments":{}}})).await.unwrap().unwrap();
            assert_eq!(response["error"]["code"], -32003, "{response}");
            assert_eq!(response["error"]["message"], duduclaw_mcp_boundary::fork_policy::message(Some(language)));
        }
    }
    assert!(!home.path().join("fork_store.db").exists());
    assert!(!home.path().join("fork_ws").exists());
}

#[tokio::test]
async fn isolated_i1b_host_controls_still_require_the_exact_capability() {
    ISOLATED_CALL.scope(call(), async {
        for name in ["create_task", "spawn_agent", "spawn_ephemeral", "inspect_branches",
            "diff_branches", "fork_cost", "discovery_catalog", "discovery_list",
            "discovery_tree", "discovery_cancel"] {
            assert!(!tool_allowed(name, &principal()), "{name}");
            let denied = dispatch_resource_tool(std::path::Path::new(""), name, &json!({})).await.unwrap();
            assert_eq!(denied["isError"], true, "{name}");
        }
    }).await;
}

#[test]
fn isolated_i1b_helper_control_schema_preserves_start_fields_and_host_controls() {
    let mut tool = json!({"name":"execute_program", "inputSchema":{
        "required":["code","language"], "properties":{"code":{"type":"string"},"language":{"type":"string"}}
    }});
    project_tool_schema(&mut tool);
    assert_eq!(tool["inputSchema"]["required"], json!([]));
    assert_eq!(tool["inputSchema"]["anyOf"][0]["required"], json!(["code","language"]));
    assert_eq!(tool["inputSchema"]["anyOf"][0]["not"]["required"], json!(["action"]));
    assert_eq!(tool["inputSchema"]["anyOf"][1]["required"], json!(["action","job"]));
    assert_eq!(tool["inputSchema"]["properties"]["action"]["enum"], json!(["status","cancel"]));
    assert_eq!(tool["inputSchema"]["properties"]["job"]["pattern"], "^[0-9a-f]{32}$");

    let mut tool = json!({"name":"code_map", "inputSchema":{"properties":{"root":{"type":"string"}, "query":{"type":"string"}}}});
    project_tool_schema(&mut tool);
    assert!(tool["inputSchema"]["properties"].get("root").is_none());
    assert_eq!(tool["inputSchema"]["required"], json!(["workspace"]));
    assert!(tool["inputSchema"]["properties"].get("action").is_some());
}

#[test]
fn isolated_i1b_recording_schema_requires_headless_and_reports_host_stop_semantics() {
    let mut start = json!({"name":"browser_record_start","inputSchema":{"required":["url"],"properties":{"url":{"type":"string"}}}});
    project_tool_schema(&mut start);
    assert_eq!(start["inputSchema"]["properties"]["headless"]["const"], true);
    assert_eq!(start["inputSchema"]["properties"]["max_seconds"]["maximum"], 300);
    assert_eq!(start["inputSchema"]["anyOf"][0]["required"], json!(["url","headless"]));
    assert_eq!(start["inputSchema"]["anyOf"][1]["required"], json!(["action","job"]));
    let mut stop = json!({"name":"browser_record_stop","inputSchema":{"required":["id"],"properties":{"id":{"type":"string"}}}});
    project_tool_schema(&mut stop);
    assert_eq!(stop["inputSchema"]["properties"]["id"]["pattern"], "^[0-9a-f]{32}$");
    assert!(stop["description"].as_str().unwrap().contains("stop_requested"));
}

#[test]
fn isolated_i1b_fork_selection_schema_requires_exact_parent_cas() {
    let mut selection = json!({"name":"merge_or_select","inputSchema":{"properties":{},"required":[]}});
    project_tool_schema(&mut selection);
    assert_eq!(selection["inputSchema"]["required"],
        json!(["fork_id","branch_id","expected_revision","expected_parent_digest"]));
    assert_eq!(selection["inputSchema"]["additionalProperties"], false);
    assert!(selection["description"].as_str().unwrap().contains("does not overwrite"));
    let mut diff = json!({"name":"diff_branches","inputSchema":{"properties":{},"required":[]}});
    project_tool_schema(&mut diff);
    assert_eq!(diff["inputSchema"]["required"], json!(["fork_id","left_branch_id","right_branch_id"]));
    assert!(diff["inputSchema"]["properties"].get("branch_a").is_none());
}

#[test]
fn isolated_i1b_round6_cost_schema_marks_missing_measurement() {
    let mut tool = json!({"name":"fork_cost","inputSchema":{"properties":{"fork_id":{"type":"string"}},"required":["fork_id"]}});
    project_tool_schema(&mut tool);
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("unavailable") && description.contains("null") && description.contains("never zero"));
    assert_eq!(tool["inputSchema"]["required"], json!(["fork_id"]));
}
