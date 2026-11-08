//! Product parity: the transport changes the execution location, not employee policy.
//! Only A/E process transports are doubles. Gateway handlers, service readiness,
//! capability parsing, discovery and dispatch gates are the production code paths.
//! Missing production selectors are tracked explicitly; this is not full parity acceptance.
#![cfg(target_os = "linux")]
use super::*;
use std::{collections::BTreeSet, sync::Mutex};
use duduclaw_isolation::execution::*;
use duduclaw_mcp_boundary::{execution::{AuthorizedExecutionPlan, ExecutionPlanFactory, PolicyFuture},
    factories::{GatewayFactories, GatewayTableParser},
    tool_policy::{AuthorizedToolCall, Route},
    worker_execution::{WorkerPlan, WorkerPlanFactory, WorkerReply, WorkerToolBridge}};

// Discovery must never execute a process or acquire a credential. These doubles
// panic if the production list/manifest accidentally crosses that boundary.
struct ListingBackend;
impl ExecutionBackend for ListingBackend {
    fn supports_authorized_workspace(&self) -> bool { true }
    fn reserve(&self, _: EmployeeId) -> BackendFuture<'_, RunHandle> { panic!("listing reserved a run") }
    fn spawn(&self, _: SpawnPolicy) -> BackendFuture<'_, RunHandle> { panic!("listing spawned a run") }
    fn wait(&self, _: RunHandle, _: WaitOrigin) -> BackendFuture<'_, RunOutcome> { panic!("listing waited") }
    fn kill(&self, _: RunHandle) -> BackendFuture<'_, ()> { panic!("listing killed a run") }
    fn take_stdio(&self, _: RunHandle) -> BackendFuture<'_, RunStdio> { panic!("listing opened stdio") }
    fn take_pty(&self, _: RunHandle) -> BackendFuture<'_, RunPty> { panic!("listing opened a pty") }
    fn resize(&self, _: RunHandle, _: PtySize) -> BackendFuture<'_, ()> { panic!("listing resized a pty") }
    // HX5: the in-run helper route is installed; listing never starts one.
    fn supports_helper_calls(&self) -> bool { true }
    fn start_helper(&self, _: HelperPlan) -> HelperFuture<'_, HelperStart> { panic!("listing started a helper") }
    fn cancel_helper(&self, _: RunHandle, _: duduclaw_isolation::ipc::HelperCallId) -> HelperFuture<'_, ()> {
        panic!("listing cancelled a helper")
    }
}

// Discovery-only double: parity is an inventory assertion, not provider acceptance.
struct ListingConnectors;
impl duduclaw_gateway::isolation_connectors::BoundConnectorExecutor for ListingConnectors {
    fn supports(&self, tool: &str) -> bool {
        duduclaw_gateway::isolation_connectors::provider_for_tool(tool).is_some()
    }
    fn execute<'a>(&'a self, _: duduclaw_gateway::isolation_connectors::SelectedConnector,
        _: AuthorizedToolCall) -> PolicyFuture<'a, Value> {
        panic!("listing executed a connector")
    }
}

struct ListingSelection;
impl duduclaw_gateway::isolation_connectors::LiveConnectorSelectionPolicy for ListingSelection {
    fn supports(&self, tool: &str) -> bool {
        duduclaw_gateway::isolation_connectors::provider_for_tool(tool).is_some()
    }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str)
        -> Result<Vec<duduclaw_gateway::isolation_connectors::ProductConnectorAccount>, VerificationError> {
        panic!("listing selected credentials")
    }
}

struct ListingPlans;
impl ExecutionPlanFactory for ListingPlans {
    fn supports(&self, tool: &str) -> bool {
        duduclaw_mcp_boundary::tool_policy::policy(tool).is_some_and(|p| matches!(p.route, Route::Execution | Route::InRunHelper))
            || matches!(tool, "hardware_info" | "model_search" | "model_recommend")
    }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        panic!("listing authorized execution")
    }
    fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall, _: RunHandle) -> PolicyFuture<'a, AuthorizedExecutionPlan> {
        panic!("listing prepared execution")
    }
}
impl WorkerPlanFactory for ListingPlans {
    fn supports(&self, tool: &str) -> bool {
        duduclaw_mcp_boundary::tool_policy::policy(tool).is_some_and(|p| p.route == Route::Worker)
    }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        panic!("listing authorized parsing")
    }
    fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
        panic!("listing prepared parsing")
    }
    fn finish<'a>(&'a self, _: &'a AuthorizedToolCall, _: WorkerReply) -> PolicyFuture<'a, Value> {
        panic!("listing finished parsing")
    }
}

// H9/H10: stands in for H5's spawnd work broker so the production issuer can
// select the brokered `employee-work` root. Listing never opens a file.
struct ListingBroker;
impl duduclaw_mcp_boundary::resources::WorkBroker for ListingBroker {
    fn open_work_file(&self, _: &duduclaw_mcp_boundary::resources::WorkFileRequest<'_>)
        -> Result<duduclaw_mcp_boundary::resources::BrokeredWorkFile, duduclaw_mcp_boundary::resources::WorkBrokerError> {
        panic!("listing opened a work file")
    }
}

fn documented_refusals() -> BTreeSet<String> {
    // Independent of the implementation's refusal predicate: an accidental new
    // refusal must change the reviewed product matrix before it can pass parity.
    let matrix = include_str!("../../../docs/architecture/native-linux-isolation-tools.md");
    let names: BTreeSet<_> = matrix.lines().filter(|line| line.starts_with("| `") && (line.contains("Refused: HostAuthority")
            || line.contains("Refused: ProposalRequired") || line.contains("Refused: ForkNextRelease")))
        .filter_map(|line| line.split('`').nth(1)).map(str::to_owned).collect();
    assert!(names.contains("os_power"), "matrix must enumerate the host-authority exception");
    let proposal: BTreeSet<_> = matrix.lines().filter(|line| line.starts_with("| `") && line.contains("Refused: ProposalRequired"))
        .filter_map(|line| line.split('`').nth(1)).collect();
    assert_eq!(proposal, BTreeSet::from(["create_agent"]), "protected employee creation is proposal-only");
    let deferred: BTreeSet<_> = matrix.lines().filter(|line| line.starts_with("| `") && line.contains("Refused: ForkNextRelease"))
        .filter_map(|line| line.split('`').nth(1)).collect();
    assert_eq!(deferred, BTreeSet::from(["fork_run", "merge_or_select", "terminate_branch"]));
    assert!(!names.contains("computer_session_start"));
    assert!(!names.contains("computer_workspace_write"));
    names
}

// Missing trusted File/A/E selectors are unfinished production wiring, not
// intentional product refusals. Keep this list independent of eligible() so
// a policy regression cannot silently expand the accepted gap.
// H9/H10 removed file_read/csv_read/xlsx_read (brokered Workspace grant) and
// web_fetch_cached/web_extract (PublicOrigins grant): the fixture installs a work
// broker and the admin sets `[isolation] public_web = true`, so the production
// issuer selects both. create_task is no longer task-run only: this interactive
// run carries the issuer's NewTask grant (H9 owner decision 3).
// `employee-work` is also the A workspace handle: when the issuer selects it for
// the file tools it leaves out of the signed snapshot every tool below whose
// eligibility would rest on that grant (`Selection::signed_tools`). Browser
// recording is not routed in protected mode yet (HX6), so browser_record_start/
// stop stay hidden and refused here.
fn pending_production_selectors() -> BTreeSet<String> {
    // H12a removed skill_extract, shared_skill_adopt, skill_security_scan,
    // skill_graduate and skill_curator_status: they select the issuer's own
    // Employee (+ own Confined wiki) under EmployeeScope, not a workspace.
    // HX5 removed execute_program, office_script and code_map: they are routed
    // in-run (`Route::InRunHelper`) on the run's own `Workspace{employee-work}`.
    ["spawn_agent", "spawn_ephemeral", "inspect_branches", "diff_branches", "fork_cost", "discovery_catalog",
        "discovery_list", "discovery_tree", "discovery_artifact", "discovery_cancel",
        "browser_record_start", "browser_record_stop", "skill_from_recording",
        "skill_synthesis_run", "skill_hub_install"]
        .into_iter().map(str::to_owned).collect()
}

/// H12a tools the production issuer must make eligible without new selectors.
const H12A_OWN_SKILL_TOOLS: [&str; 5] = ["skill_extract", "shared_skill_adopt", "skill_security_scan",
    "skill_graduate", "skill_curator_status"];

fn employee_config(extra_capabilities: &str) -> String {
    format!(r#"[agent]
name = "main"
display_name = "Parity employee"
role = "main"
status = "active"
trigger = "@main"
reports_to = ""
icon = "robot"
[model]
preferred = "claude-sonnet-4-6"
fallback = ""
api_mode = "cli"
account_pool = []
[budget]
monthly_limit_cents = 1000
warn_threshold_percent = 80
hard_stop = false
[container]
sandbox_enabled = false
network_access = true
timeout_ms = 60000
max_concurrent = 2
readonly_project = false
additional_mounts = []
[heartbeat]
enabled = false
interval_seconds = 300
max_concurrent_runs = 1
cron = ""
[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = true
allowed_channels = []
[evolution]
skill_auto_activate = false
skill_security_scan = false
[isolation]
public_web = true
[capabilities]
allowed_tools = ["mcp__duduclaw__*"]
os_native = true
recording = true
system_operator = true
codrive = true
computer_use = true
db_sources = ["demo"]
{extra_capabilities}
[capabilities.computer_use_config]
workspace = true
[fork]
enabled = true
[google]
identity = "home"
[[channels.discord.bindings]]
kind = "channel"
id = "parity-group"
"#)
}

fn fixture(home: &std::path::Path) -> (IsolationDispatcher, RequestContext) {
    fixture_with_principal(home, false)
}

fn fixture_with_principal(home: &std::path::Path, background: bool) -> (IsolationDispatcher, RequestContext) {
    std::fs::create_dir_all(home.join("agents/main/wiki")).unwrap();
    let raw = employee_config("");
    let typed: duduclaw_core::types::AgentConfig = toml::from_str(&raw).unwrap();
    assert_eq!(typed.agent.name, "main");
    assert!(typed.agent.status.is_operational());
    assert_eq!(typed.capabilities.allowed_tools, ["mcp__duduclaw__*"]);
    assert!(typed.capabilities.os_native && typed.capabilities.recording && typed.capabilities.system_operator);
    assert!(typed.capabilities.computer_use && typed.capabilities.computer_use_config.workspace);
    assert_eq!(typed.capabilities.db_sources, ["demo"]);
    assert!(typed.permissions.can_schedule_tasks && typed.permissions.can_send_cross_agent);
    assert_eq!(typed.channels.unwrap().discord.unwrap().bindings[0].id, "parity-group");
    std::fs::write(home.join("agents/main/agent.toml"), raw).unwrap();
    std::fs::write(home.join("config.toml"), r#"[general]
default_agent = "main"
[integrations]
google_workspace = true
github = true
[responsibilities]
enabled = true
[computer_use.workspaces]
enabled = true
[knowledge]
enabled = true
"#).unwrap();
    assert!(duduclaw_gateway::google_workspace::integration_enabled(home));
    assert!(duduclaw_gateway::github_workspace::integration_enabled(home));
    assert!(duduclaw_gateway::responsibility::ResponsibilityConfig::from_home(home).enabled);
    assert!(crate::mcp_fork::load_fork_settings(home, "main").enabled);
    let _settings = duduclaw_gateway::channel_settings::ChannelSettingsManager::new(&home.join("sessions.db")).unwrap();
    duduclaw_gateway::group_context::set(home, &duduclaw_gateway::group_context::GroupConfig {
        channel: "discord".into(), group_id: "parity-group".into(), scope_id: "parity-guild".into(),
        max_messages: 50, max_age_hours: 24,
    }).unwrap();
    let mut knowledge = duduclaw_knowledge::schema::open_home(home).unwrap();
    let collection = duduclaw_knowledge::store::create_collection(&mut knowledge,
        &duduclaw_knowledge::store::NewCollection { name: "parity".into(), ..Default::default() },
        "owner", &duduclaw_knowledge::limits::KnowledgeConfig::default()).unwrap();
    use duduclaw_knowledge::acl::{PrincipalKind, Role};
    duduclaw_knowledge::acl::set_acl(&mut knowledge, &collection.id, &[
        (PrincipalKind::User, "owner".into(), Role::Owner),
        (PrincipalKind::Agent, "main".into(), Role::Viewer),
    ], 0, "owner").unwrap();
    let mut principal = principal();
    // Registered per-employee keys are an existing production caller shape.
    // Admin is the key's scope ceiling, not RecordActor::Operator authority.
    principal.client_id = if std::env::var_os("DUDUCLAW_PARITY_CHILD_HOME").is_some() {
        duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID.into()
    } else { "main".into() };
    principal.scopes = [crate::mcp_auth::Scope::Admin].into_iter().collect();
    assert_eq!(crate::mcp::record_actor_for(home, &principal.client_id, "main").agent(), Some("main"));
    // Only process/provider transports and allocator ReadBack are doubles here.
    // The issuer reads real Shared lifecycle and users.db evidence.
    use duduclaw_gateway::isolation_mcp::{test_support::SharedEmployee, DispatcherFactory, HostTurn};
    let employee = context().record().employee;
    // H9: the admin enabled protected public web (`[isolation] public_web`)
    // in agent.toml above; the broker double stands in for H5's work broker.
    assert!(typed.isolation.public_web);
    let shared = SharedEmployee::new(home, "main", employee).with_work_broker(Arc::new(ListingBroker));
    let snapshot = shared.snapshot();
    let authorization = shared.authorize(&snapshot, Lane::Standard,
        crate::mcp_isolation_boot::CliDispatcherFactory.ingress_tools(home, "main", Lane::Standard).unwrap(),
        HostTurn { identity: duduclaw_gateway::isolation_egress::RunTurnIdentity {
            turn: "parity-turn".into(), approval_session: None }, egress_turn: None,
            metadata: None, principal_context: None, background: None }, None).unwrap();
    let roots = authorization.roots;
    let grant = authorization.grant;
    let mut record = context().record().clone();
    record.task = grant.task;
    record.principal = grant.principal;
    record.employee_binding = grant.binding;
    record.lane = grant.lane;
    record.tools = grant.tools;
    record.resources = grant.resources;
    record.policy_revision = grant.policy_revision;
    record.not_before = grant.not_before;
    record.expires_at = grant.expires_at;
    if background {
        // Exercise the existing background principal verifier separately. The
        // production record-based scheduler currently retains its human owner.
        record.principal = RunPrincipal::Background { tenant_id: Text::new(snapshot.tenant_id.clone()).unwrap(),
            generation: snapshot.acl_generation };
        // A Background principal has no human requester, so it can never hold
        // the interactive NewTask grant (the capability would not verify).
        record.resources = List::new(record.resources.as_slice().iter()
            .filter(|grant| !matches!(grant, ResourceGrant::NewTask { .. })).cloned().collect()).unwrap();
    }
    record.canonical_bytes().expect("production issuer must produce a signable capability");
    let context = live_context(record);
    let mut bindings = duduclaw_mcp_boundary::bindings::BindingStore::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    // The production issuer binds employees to the tenant. The store double
    // needs that tenant principal and the human's explicit shared membership.
    let EmployeeBinding::Shared(tenant_id) = &context.record().employee_binding else {
        panic!("production issuer must retain its shared tenant binding");
    };
    let tenant = RunPrincipal::Background { tenant_id: tenant_id.clone(), generation: snapshot.acl_generation };
    bindings.register_principal(&tenant).unwrap();
    if context.record().principal != tenant {
        bindings.register_principal(&context.record().principal).unwrap();
    }
    bindings.create_employee(context.record().employee, &context.record().employee_binding).unwrap();
    if let RunPrincipal::Human { user_id, .. } = &context.record().principal {
        assert!(bindings.validate(context.record().employee, &context.record().employee_binding,
            &context.record().principal).is_err(), "shared employees require explicit membership");
        bindings.add_shared_member(context.record().employee, user_id.as_str(), "owner").unwrap();
    }
    bindings.validate(context.record().employee, &context.record().employee_binding,
        &context.record().principal).unwrap();
    let resolver = Arc::new(duduclaw_gateway::isolation_resources::GatewayResourceResolver::new(
        home.to_owned(), context.record().employee, "main".into(), Arc::new(Mutex::new(bindings))).unwrap()
        .with_connector_catalog(Arc::new(duduclaw_gateway::isolation_connectors::ConnectorCatalog::new(
            rusqlite::Connection::open_in_memory().unwrap()).unwrap()))
        .with_connector_selection_policy(Arc::new(ListingSelection)));
    let parser = Arc::new(GatewayTableParser::new(Arc::new(|_, _, _| panic!("listing parsed a file"))));
    let factories = GatewayFactories::assemble(Some(Arc::new(ListingBackend)), Some(Arc::new(ListingPlans)),
        resolver.clone(), Some(parser), roots.clone()).unwrap()
        .with_forks(Arc::new(duduclaw_mcp_boundary::forks::ForkHostControl {
            store: duduclaw_mcp_boundary::forks::ForkStore::open(&home.join("isolation-forks.db")).unwrap(),
            backend: Arc::new(ListingBackend),
        })).with_worker(Arc::new(WorkerToolBridge::new(
            Arc::new(ListingPlans), resolver.clone(), Arc::new(|_| panic!("listing launched a parser")))));
    let dispatcher = IsolationDispatcher::new(home.to_owned(), context.record().employee, "main".into(),
        principal, roots).unwrap().with_gateway_services(resolver, factories, Some(Arc::new(ListingConnectors)));
    (dispatcher, context)
}

fn names(response: &Value) -> BTreeSet<String> {
    assert!(response.get("error").is_none(), "{response}");
    response["result"]["tools"].as_array().unwrap().iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn isolated_round6_production_inventory_tracks_selector_gaps() {
    // A dedicated test process models a real gateway-spawned MCP child, including
    // signed employee identity, without racing global env mutations in libtest.
    if let Some(home) = std::env::var_os("DUDUCLAW_PARITY_CHILD_HOME") {
        tokio::runtime::Runtime::new().unwrap().block_on(employee_tool_parity(std::path::Path::new(&home)));
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let key = duduclaw_core::identity_token::ensure_key(home.path()).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("isolated_round6_production_inventory_tracks_selector_gaps")
        .arg("--test-threads=1").arg("--nocapture")
        .env("DUDUCLAW_PARITY_CHILD_HOME", home.path())
        .env(duduclaw_core::ENV_AGENT_ID, "main")
        // The child's fixture confirms "main" as a protected employee (the
        // context's employee id, incarnation 0), so only a v2 token verifies (F6m).
        .env(duduclaw_core::ENV_AGENT_TOKEN, duduclaw_core::identity_token::mint_token_v2(&key, "main",
            &duduclaw_core::identity_token::AgentIncarnation { employee: context().record().employee.0, incarnation: 0 }))
        .env_remove(duduclaw_core::ENV_LANE)
        .output().unwrap();
    assert!(output.status.success(), "parity subprocess failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the parity subprocess must execute its test, not pass an empty filter");
}

async fn employee_tool_parity(home: &std::path::Path) {
    let (dispatcher, context) = fixture(home);
    assert_eq!(crate::mcp_namespace::verified_employee_from_env(home).as_deref(), Some("main"));
    dispatcher.require_complete_services().unwrap();
    dispatcher.require_context_services(&context).unwrap();
    let host = documented_refusals();
    // Hidden in this interactive run: the tracked selector gaps only.
    let pending = pending_production_selectors();
    for tool in &pending {
        assert!(!duduclaw_mcp_boundary::tool_policy::eligible(&context, tool),
            "{tool}: production selector gap changed; review this inventory");
    }
    assert!(duduclaw_mcp_boundary::tool_policy::eligible(&context, "knowledge_add_source"));
    assert_eq!(duduclaw_mcp_boundary::tool_policy::OWN_SKILL_TOOLS, &H12A_OWN_SKILL_TOOLS[..]);
    for tool in H12A_OWN_SKILL_TOOLS {
        assert!(duduclaw_mcp_boundary::tool_policy::eligible(&context, tool),
            "{tool}: the production issuer's EmployeeScope + own Employee (+ own wiki) must suffice");
        assert!(!pending.contains(tool), "{tool}");
    }
    // H9/H10: the production issuer selected the brokered Workspace, the
    // PublicOrigins grant and the interactive NewTask grant for this employee;
    // nothing was appended by the test.
    for tool in ["file_read", "csv_read", "xlsx_read", "web_fetch_cached", "web_extract", "create_task"] {
        assert!(duduclaw_mcp_boundary::tool_policy::eligible(&context, tool), "{tool}: H10 issuer selector missing");
    }
    // HX5: the in-run helper tools ride the same Workspace{employee-work} grant.
    for tool in ["execute_program", "code_map"] {
        assert!(duduclaw_mcp_boundary::tool_policy::eligible(&context, tool), "{tool}: HX5 in-run route not eligible");
        assert!(!pending.contains(tool), "{tool}");
    }
    assert!(context.record().resources.as_slice().iter().any(|grant| matches!(grant,
        ResourceGrant::NewTask { requester } if matches!(&context.record().principal,
            RunPrincipal::Human { user_id, .. } if user_id == requester))), "NewTask is the run's own human");
    // Missing producer authority must narrow discovery, not become a transport exception.
    let mut without_wiki = context.record().clone();
    without_wiki.resources = List::new(without_wiki.resources.as_slice().iter()
        .filter(|grant| !matches!(grant, ResourceGrant::Confined { resource }
            if matches!(resource.as_ref(), ResourceGrant::Wiki { .. }))).cloned().collect()).unwrap();
    let without_wiki = live_context(without_wiki);
    dispatcher.require_context_services(&without_wiki).unwrap();
    let limited = names(&dispatcher.dispatch(without_wiki, "main", json!({
        "jsonrpc":"2.0", "id":1, "method":"tools/list"})).await.unwrap().unwrap());
    assert!(limited.contains("plan_start"), "missing Wiki must not invalidate unrelated tools");
    for tool in ["knowledge_add_source", "skill_extract"] {
        assert!(!limited.contains(tool), "{tool} requires employee wiki authority");
    }
    for extra in ["", "denied_tools = [\"odoo_search\", \"wiki_write\", \"send_to_agent\", \"knowledge_add_source\", \"skill_extract\"]\naction_rules = [{tool=\"plan_start\", verdict=\"block\"}]"] {
        std::fs::write(home.join("agents/main/agent.toml"), employee_config(extra)).unwrap();
        let ordinary = names(&crate::mcp::handle_tools_list_for_agent(&json!(1), &dispatcher.principal, home, "main").await);
        let isolated = names(&dispatcher.dispatch(context.clone(), "main", json!({"jsonrpc":"2.0", "id":1,
            "method":"tools/list"})).await.unwrap().unwrap());
        assert!(!isolated.is_empty(), "valid fixture unexpectedly lost every tool");
        let expected = ordinary.difference(&host).filter(|tool| !pending.contains(*tool))
            .cloned().collect::<BTreeSet<_>>();
        assert_eq!(ordinary.difference(&host).filter(|tool| !isolated.contains(*tool))
            .cloned().collect::<BTreeSet<_>>(), ordinary.intersection(&pending).cloned().collect(),
            "only the explicitly tracked producer-selector gap may narrow the production inventory");
        assert_eq!(isolated, expected, "transport must preserve existing employee gates");
        for tool in ["fork_run", "merge_or_select", "terminate_branch"] {
            assert!(ordinary.contains(tool), "non-protected installations retain {tool}");
            assert!(!isolated.contains(tool), "protected TEAM defers {tool}");
        }
        if extra.is_empty() {
            for tool in ["odoo_connect", "odoo_execute", "send_message", "send_photo", "send_sticker", "channel_status",
                "send_to_agent", "team_handoff", "plan_start", "task_status", "session_restore_context", "memory_improve",
                "list_cron_tasks", "run_cron_task", "create_reminder", "cancel_reminder", "web_search", "mail_send",
                "db_sources", "db_tables", "db_select", "db_query", "wiki_read", "wiki_write", "wiki_share",
                "cost_summary", "list_agents", "computer_session_start", "computer_workspace_list", "computer_workspace_write",
                "group_history_read", "knowledge_search", "knowledge_get_passage", "knowledge_list_collections", "knowledge_add_source",
                "meeting_transcribe", "meeting_upload_link", "file_read", "csv_read", "xlsx_read", "code_map", "execute_program",
                "skill_extract", "web_fetch_cached", "web_extract", "model_search", "model_recommend",
                "shared_skill_adopt", "skill_security_scan", "skill_graduate", "skill_curator_status"] {
                assert!(ordinary.contains(tool), "production fixture did not enable {tool}");
                assert_eq!(isolated.contains(tool), !pending.contains(tool),
                    "production inventory disagrees with tracked selector gap: {tool}");
            }
        } else {
            for tool in ["odoo_search", "wiki_write", "send_to_agent", "plan_start", "knowledge_add_source", "skill_extract"] {
                assert!(!isolated.contains(tool), "existing policy was bypassed for {tool}");
            }
        }
    }
}

#[tokio::test]
async fn isolated_round5_gateway_handler_dispatch_keeps_existing_capability_denials() {
    let home = tempfile::tempdir().unwrap();
    let (dispatcher, context) = fixture(home.path());
    for tool in ["plan_start", "belief_stats"] {
        let mut request = json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":tool}});
        if tool == "plan_start" { request["params"]["arguments"] = json!({"task":"parity plan"}); }
        let ordinary = dispatcher.dispatcher.dispatch_tool_call(&dispatcher.principal, &dispatcher.namespace,
            &request["params"], &request["id"]).await;
        let response = dispatcher.dispatch(context.clone(), "main", request.clone()).await.unwrap().unwrap();
        assert!(response.get("error").is_none(), "{tool}: {response}");
        assert_ne!(response["result"]["isError"], true, "{tool}: {response}");
        assert_eq!(response["result"], ordinary["result"], "same existing handler: {tool}");
        let policy = format!("denied_tools = [\"{tool}\"]");
        std::fs::write(home.path().join("agents/main/agent.toml"), employee_config(&policy)).unwrap();
        let ordinary_denied = dispatcher.dispatcher.dispatch_tool_call(&dispatcher.principal, &dispatcher.namespace,
            &request["params"], &request["id"]).await;
        let denied = dispatcher.dispatch(context.clone(), "main", request).await.unwrap().unwrap();
        assert!(denied.get("error").is_some(), "{tool}: {denied}");
        assert_eq!(denied["error"], ordinary_denied["error"], "same existing denial: {tool}");
        assert!(!denied.to_string().contains("isolation_boundary"), "must use the original gate: {denied}");
        std::fs::write(home.path().join("agents/main/agent.toml"), employee_config("")).unwrap();
    }
}

struct BackgroundTurn;
impl duduclaw_gateway::isolation_egress::RunTurnResolver for BackgroundTurn {
    fn resolve(&self, _: &RequestContext) -> Result<duduclaw_gateway::isolation_egress::RunTurnIdentity, VerificationError> {
        Ok(duduclaw_gateway::isolation_egress::RunTurnIdentity {
            turn: "parity-background-run".into(), approval_session: None,
        })
    }
    fn metadata(&self, _: &RequestContext) -> Result<Option<duduclaw_gateway::isolation_egress::RunMetadata>, VerificationError> {
        let mut metadata = duduclaw_gateway::isolation_egress::RunMetadata::root();
        metadata.dispatch_run = Some(("cron:main".into(), "parity-run".into()));
        Ok(Some(metadata))
    }
}

#[test]
fn isolated_round5_background_memory_store_reuses_handler_with_dispatch_provenance() {
    std::thread::Builder::new().name("isolated-memory-2mib".into())
        .stack_size(2 * 1024 * 1024).spawn(|| {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
                .block_on(background_memory_store_with_dispatch_provenance());
        }).unwrap().join().unwrap();
}

async fn background_memory_store_with_dispatch_provenance() {
    let home = tempfile::tempdir().unwrap();
    let (dispatcher, context) = fixture_with_principal(home.path(), true);
    let dispatcher = dispatcher.with_turn_resolver(Arc::new(BackgroundTurn));
    let request = json!({"jsonrpc":"2.0", "id":3, "method":"tools/call",
        "params":{"name":"memory_store", "arguments":{"content":"The daily report is prepared at nine."}}});
    let ordinary = dispatcher.dispatcher.dispatch_tool_call(&dispatcher.principal, &dispatcher.namespace,
        &request["params"], &request["id"]).await;
    let isolated = dispatcher.dispatch(context, "main", request).await.unwrap().unwrap();
    let database = rusqlite::Connection::open(home.path().join("memory.db")).unwrap();
    for response in [&ordinary, &isolated] {
        assert!(response.get("error").is_none(), "{response}");
        assert_ne!(response["result"]["isError"], true, "{response}");
        let id = response["result"]["memory_id"].as_str().unwrap();
        let (employee, content): (String, String) = database.query_row(
            "SELECT agent_id,content FROM memories WHERE id=?1", [id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(employee, "main");
        assert_eq!(content, "The daily report is prepared at nine.");
    }
    let id = isolated["result"]["memory_id"].as_str().unwrap();
    let dispatch_origin: i64 = database.query_row(
        "SELECT COUNT(*) FROM memory_origins WHERE memory_id=?1 AND agent_id='main'
         AND source_kind='dispatch_run' AND source_session='cron:main' AND source_message='run:parity-run'",
        [id], |row| row.get(0)).unwrap();
    assert_eq!(dispatch_origin, 1, "background memory must retain its gateway-issued dispatch source");
}

#[test]
fn every_connector_checks_executor_for_its_effective_route() {
    let home = tempfile::tempdir().unwrap();
    let (mut dispatcher, _) = fixture(home.path());
    assert!(dispatcher.require_complete_services().is_ok());
    struct WithoutOne { omitted: &'static str, inner: Arc<dyn ResourceToolService> }
    impl ResourceToolService for WithoutOne {
        fn supports(&self, name: &str) -> bool { name != self.omitted && self.inner.supports(name) }
        fn resolver(&self) -> &dyn duduclaw_mcp_boundary::tool_policy::ResourceResolver { self.inner.resolver() }
        fn check_handler_policy<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
            self.inner.check_handler_policy(call)
        }
        fn execute<'a>(&'a self, call: AuthorizedToolCall) -> PolicyFuture<'a, Value> { self.inner.execute(call) }
    }
    let complete = dispatcher.services.clone().unwrap();
    let connectors: Vec<_> = duduclaw_mcp_boundary::tool_policy::POLICIES.iter()
        .filter(|p| p.route == Route::Connector).collect();
    assert!(connectors.len() > 28, "must cover providers beyond Google/Notion/GitHub");
    for policy in connectors {
        assert!(duduclaw_gateway::isolation_connectors::provider_for_tool(policy.name).is_some(), "{}", policy.name);
        dispatcher.services = Some(Arc::new(WithoutOne { omitted: policy.name, inner: complete.clone() }));
        let state = dispatcher.service_manifest().iter().find(|(n, _)| *n == policy.name).unwrap().1;
        if cfg!(target_os = "linux") && matches!(policy.name, "model_search" | "model_recommend") {
            assert!(matches!(boundary_route(policy.name), Some(BoundaryRoute::GatewayHandler(_))), "{}", policy.name);
            assert_eq!(state, ServiceState::Ready, "bundled catalog needs no connector executor: {}", policy.name);
            dispatcher.require_complete_services().unwrap();
        } else {
            assert_eq!(boundary_route(policy.name), Some(BoundaryRoute::Resource), "{}", policy.name);
            assert_eq!(dispatcher.require_complete_services(), Err(VerificationError::Unavailable), "{}", policy.name);
            assert_eq!(state, ServiceState::MissingFactory, "{}", policy.name);
        }
    }
}


#[test]
fn startup_readiness_checks_finite_adapter_coverage_for_the_actual_capability_mode() {
    let home = tempfile::tempdir().unwrap();
    let (dispatcher, scope) = fixture(home.path());
    assert!(dispatcher.require_complete_services().is_ok());
    assert!(dispatcher.require_context_services(&scope).is_ok());
    let mut ambiguous = scope.record().clone();
    ambiguous.resources = List::new(scope.record().io_resources().cloned().collect()).unwrap();
    assert!(ambiguous.canonical_bytes().is_err(), "top-level Wiki cannot mix with EmployeeScope");
    assert_eq!(dispatcher.require_context_services(&live_context(ambiguous)), Err(VerificationError::Invalid));
    for name in ["tasks_list", "activity_list", "tasks_update", "goals_list", "list_cron_tasks", "list_reminders", "responsibility_get", "db_query"] {
        let mut record = scope.record().clone();
        record.tools = List::new(vec![Text::new(name.into()).unwrap()]).unwrap();
        record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
            if name == "db_query" { ResourceGrant::DatabaseTable { database: Text::new("source".into()).unwrap(), table: Text::new("customers".into()).unwrap() } }
            else { ResourceGrant::Task { id: Text::new("task-a".into()).unwrap() } }]).unwrap();
        let context = live_context(record);
        assert!(dispatcher.require_context_services(&context).is_ok(), "{name}");
        assert!(duduclaw_mcp_boundary::server::Dispatcher::require_context_services(&dispatcher, &context).is_ok(), "trait admission for {name}");
    }
}
