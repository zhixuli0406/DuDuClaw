//! G1 [SEC] (v1.81): the whole tool catalog through the REAL issuer and the
//! REAL connector factory, for one Personal and one Shared employee in the
//! same protected home.
//!
//! Real: users.db / apply.db / migration-confirmations.db, the Employee apply
//! path (`save_confirmed_receipt`, Personal mirror + owner binding), the A2
//! lifecycle read, the B1/B2 issuer with its H9/H10/HX5/HX6 selectors,
//! `tool_policy` eligibility, the private connector catalog file, F1's
//! `gateway_factory` + `EmployeeConnectorProviders`/`EmployeeConnectors`
//! with D3's CLI Odoo/channel executor factories and the F3 readiness sink.
//! Doubles (process/privilege transports only): the root allocator ReadBack
//! (inside `SharedEmployee`), the spawnd work broker and feature view, the
//! A/E process backends of the parent module (`Listing*`, panic if used),
//! the search source (needs the installed parse worker) and the in-memory
//! binding store. Nothing here is an issuer double.
//!
//! Fails closed: a new catalog tool without a policy or an exact boundary
//! route, a policy without the own Employee grant, an offered tool whose
//! factory is missing, a connector tool the per-employee registry cannot
//! serve, a pending selector that became eligible, or an unexplained
//! Personal/Shared eligibility difference all fail this test.
use super::*;
use std::collections::BTreeMap;
use duduclaw_gateway::isolation_connector_boot::{ConnectorExecutorFactories, InstalledConnectorProviders,
    ProviderRefusal, SearchProviderSource};
use duduclaw_gateway::isolation_connector_search::SearchConnectorProvider;
use duduclaw_gateway::isolation_connector_stack::{gateway_factory, installed_home_binding, readiness_report,
    ConnectorStackInputs, EmployeeBindingSource, EmployeeConnectorProviders, EmployeeKeyGoogleBridges};
use duduclaw_gateway::isolation_connectors::{provider_for_tool, BoundConnectorExecutor, ConnectorCatalog,
    LiveConnectorSelectionPolicy, ProductConnectorAccount};
use duduclaw_gateway::isolation_mcp::{test_support::SharedEmployee, DispatcherFactory, HostTurn};
use duduclaw_mcp_boundary::tool_policy::{self as policy_table, ResourceKind};

/// The reviewed catalog size (same pin as `isolated_round4_*`). A new tool
/// must be given a policy, a boundary route and a G1 review before this moves.
const CATALOG_TOOLS: usize = 263;
const SHARED_LABEL: &str = "team";
const PERSONAL_LABEL: &str = "alice-personal";
const SHARED_EMPLOYEE: EmployeeId = EmployeeId([0x21; 16]);
const PERSONAL_EMPLOYEE: EmployeeId = EmployeeId([0x37; 16]);
/// Tools whose eligibility may legitimately differ between a Personal and a
/// Shared employee in an interactive run started by the authenticated human
/// (H13/H13b: spawn selectors read the binding). Anything else differing is
/// an unreviewed binding-dependent policy.
const BINDING_DEPENDENT: [&str; 2] = ["spawn_agent", "spawn_ephemeral"];
/// The three File-route tools are the only policies without the own Employee
/// grant: their authority is the brokered `Workspace{employee-work}` alone.
const WORKSPACE_ONLY: [&str; 3] = ["file_read", "csv_read", "xlsx_read"];

fn kind_name(kind: &ResourceKind) -> &'static str {
    match kind {
        ResourceKind::EmployeeScope => "employee_scope", ResourceKind::Employee => "employee",
        ResourceKind::Workspace => "workspace", ResourceKind::Mailbox => "mailbox", ResourceKind::Task => "task",
        ResourceKind::DatabaseTable => "database_table", ResourceKind::Wiki => "wiki",
        ResourceKind::Connector => "connector", ResourceKind::Recipient => "recipient",
        ResourceKind::UrlOrigin => "url_origin", ResourceKind::PublicOrigins => "public_origins",
        ResourceKind::NewTask => "new_task",
    }
}
fn route_name(route: Route) -> &'static str {
    match route {
        Route::File => "file", Route::Service => "service", Route::Execution => "execution",
        Route::Worker => "worker", Route::Connector => "connector", Route::InRunHelper => "in_run_helper",
    }
}
fn boundary_name(route: &BoundaryRoute) -> &'static str {
    match route {
        BoundaryRoute::File => "file", BoundaryRoute::GatewayHandler(_) => "gateway_handler",
        BoundaryRoute::Resource => "resource", BoundaryRoute::Execution => "execution", BoundaryRoute::Worker => "worker",
    }
}

/// The parity profile (wildcard `mcp__duduclaw__*`, `[isolation] public_web`)
/// under another label.
fn profile(label: &str) -> String {
    employee_config("").replace("name = \"main\"", &format!("name = \"{label}\""))
        .replace("trigger = \"@main\"", &format!("trigger = \"@{label}\""))
}

/// Lifecycle source of the connector stack, filled from the REAL A2
/// lifecycle reads of both fixture employees (production uses
/// `LifecycleBindings`, whose root ReadBack needs spawnd).
struct FixtureLifecycle(BTreeMap<String, (EmployeeId, EmployeeBinding)>);
impl EmployeeBindingSource for FixtureLifecycle {
    fn current(&self, agent: &str) -> Result<(EmployeeId, EmployeeBinding), VerificationError> {
        self.0.get(agent).cloned().ok_or(VerificationError::Revoked)
    }
    fn labels(&self) -> Result<Vec<String>, VerificationError> { Ok(self.0.keys().cloned().collect()) }
}

/// Search needs the installed parse worker (no local fallback in product).
struct NoSearchWorker;
impl SearchProviderSource for NoSearchWorker {
    fn search(&self, _: EmployeeId, _: ProductConnectorAccount) -> Result<Arc<SearchConnectorProvider>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}

fn spawnd_view() -> Arc<dyn Fn() -> duduclaw_isolation::ipc::SpawndFeatures + Send + Sync> {
    Arc::new(|| duduclaw_isolation::ipc::SpawndFeatures::HELPER_CALLS
        .with(duduclaw_isolation::ipc::SpawndFeatures::WORK_BROKER)
        .with(duduclaw_isolation::ipc::SpawndFeatures::HELPER_RECORDING))
}

/// The real issuer's capability for an interactive run of `fixture`'s human.
fn issue(home: &std::path::Path, fixture: &SharedEmployee, label: &str) -> (RequestContext, Arc<ResourceRoots>) {
    let snapshot = fixture.snapshot();
    let authorization = fixture.authorize(&snapshot, Lane::Standard,
        crate::mcp_isolation_boot::CliDispatcherFactory.ingress_tools(home, label, Lane::Standard).unwrap(),
        HostTurn { identity: duduclaw_gateway::isolation_egress::RunTurnIdentity {
            turn: format!("g1-{label}"), approval_session: None }, egress_turn: None,
            metadata: None, principal_context: None, background: None }, None)
        .unwrap_or_else(|error| panic!("{label}: the production issuer refused its own owner: {error:?}"));
    let roots = authorization.roots;
    let grant = authorization.grant;
    let mut record = context().record().clone();
    record.employee = fixture.employee();
    record.task = grant.task;
    record.principal = grant.principal;
    record.employee_binding = grant.binding;
    record.lane = grant.lane;
    record.tools = grant.tools;
    record.resources = grant.resources;
    record.policy_revision = grant.policy_revision;
    record.not_before = grant.not_before;
    record.expires_at = grant.expires_at;
    record.canonical_bytes().unwrap_or_else(|_| panic!("{label}: issuer output must be signable"));
    (live_context(record), roots)
}

/// Binding store double holding exactly the issued binding and principal.
fn binding_store(context: &RequestContext, snapshot_generation: u64)
    -> Arc<dyn duduclaw_mcp_boundary::bindings::BindingAuthority> {
    let record = context.record();
    let mut bindings = duduclaw_mcp_boundary::bindings::BindingStore::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    if let EmployeeBinding::Shared(tenant_id) = &record.employee_binding {
        let tenant = RunPrincipal::Background { tenant_id: tenant_id.clone(), generation: snapshot_generation };
        bindings.register_principal(&tenant).unwrap();
        if record.principal != tenant { bindings.register_principal(&record.principal).unwrap(); }
        bindings.create_employee(record.employee, &record.employee_binding).unwrap();
        if let RunPrincipal::Human { user_id, .. } = &record.principal {
            bindings.add_shared_member(record.employee, user_id.as_str(), "owner").unwrap();
        }
    } else {
        bindings.register_principal(&record.principal).unwrap();
        bindings.create_employee(record.employee, &record.employee_binding).unwrap();
    }
    bindings.validate(record.employee, &record.employee_binding, &record.principal).unwrap();
    Arc::new(Mutex::new(bindings))
}

struct Employee {
    label: &'static str,
    kind: &'static str,
    context: RequestContext,
    dispatcher: IsolationDispatcher,
    installed: Arc<InstalledConnectorProviders>,
}

fn employee(home: &std::path::Path, fixture: &SharedEmployee, label: &'static str, kind: &'static str,
    stack: &Arc<EmployeeConnectorProviders>) -> Employee {
    let (context, roots) = issue(home, fixture, label);
    let authority = binding_store(&context, fixture.snapshot().acl_generation);
    // F1 production shape: one per-launch handle is both the resolver's
    // selection policy and the dispatcher's connector executor.
    let connectors = Arc::new(stack.for_employee(label, fixture.employee()).unwrap());
    let installed = stack.current(label, fixture.employee())
        .unwrap_or_else(|error| panic!("{label}: real connector factory build failed: {error:?}"));
    let resolver = Arc::new(duduclaw_gateway::isolation_resources::GatewayResourceResolver::new(
        home.to_owned(), fixture.employee(), label.into(), authority).unwrap()
        .with_connector_catalog(stack.catalog().clone())
        .with_connector_selection_policy(connectors.clone()));
    let parser = Arc::new(GatewayTableParser::new(Arc::new(|_, _, _| panic!("G1 parsed a file"))));
    let factories = GatewayFactories::assemble(Some(Arc::new(ListingBackend)), Some(Arc::new(ListingPlans)),
        resolver.clone(), Some(parser), roots.clone()).unwrap()
        .with_recording_jobs(duduclaw_mcp_boundary::recording_jobs::SqliteRecordingJobs::open(
            &home.join("isolation-execution-jobs.db")).unwrap())
        .with_forks(Arc::new(duduclaw_mcp_boundary::forks::ForkHostControl {
            store: duduclaw_mcp_boundary::forks::ForkStore::open(&home.join("isolation-forks.db")).unwrap(),
            backend: Arc::new(ListingBackend),
        })).with_worker(Arc::new(WorkerToolBridge::new(
            Arc::new(ListingPlans), resolver.clone(), Arc::new(|_| panic!("G1 launched a parser")))));
    let mut principal = principal();
    principal.client_id = label.into();
    principal.scopes = [crate::mcp_auth::Scope::Admin].into_iter().collect();
    let dispatcher = IsolationDispatcher::new(home.to_owned(), fixture.employee(), label.into(), principal, roots)
        .unwrap().with_gateway_services(resolver, factories, Some(connectors));
    Employee { label, kind, context, dispatcher, installed }
}

/// Why a tool is not eligible for this record; `None` = unexplained.
fn hidden_reason(context: &RequestContext, pending: &BTreeSet<String>, name: &str) -> Option<String> {
    if crate::mcp_isolation_disposition::intentional_refusal(name).is_some() { return Some("intentional_refusal".into()); }
    if pending.contains(name) { return Some("pending_selector".into()); }
    let record = context.record();
    if !record.tools.as_slice().iter().any(|tool| tool.as_str() == name) { return Some("not_signed".into()); }
    let policy = policy_table::policy(name)?;
    let missing: Vec<&str> = policy.required.iter()
        .filter(|kind| !record.io_resources().any(|grant| ResourceKind::of(grant) == **kind))
        .map(kind_name).collect();
    if !missing.is_empty() { return Some(format!("missing:{}", missing.join("+"))); }
    if policy_table::OWN_SKILL_TOOLS.contains(&name) && policy_table::own_skill_selector(context, name).is_err() {
        return Some("own_skill_selector".into());
    }
    if policy_table::OWN_SKILL_JOB_TOOLS.contains(&name) && policy_table::own_skill_job_selector(context, name).is_err() {
        return Some("own_skill_job_selector".into());
    }
    if policy_table::SPAWN_TOOLS.contains(&name) { return Some("spawn_selector".into()); }
    if name == "create_task" { return Some("task_selector".into()); }
    None
}

#[test]
fn g1_real_issuer_and_factory_cover_every_catalog_tool_for_personal_and_shared() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    for label in [SHARED_LABEL, PERSONAL_LABEL] {
        std::fs::create_dir_all(home.join("agents").join(label).join("wiki")).unwrap();
        std::fs::write(home.join("agents").join(label).join("agent.toml"), profile(label)).unwrap();
        let typed: duduclaw_core::types::AgentConfig = toml::from_str(&profile(label)).unwrap();
        assert_eq!(typed.capabilities.allowed_tools, ["mcp__duduclaw__*"], "production-shaped wildcard allowlist");
        assert!(typed.isolation.public_web);
    }
    std::fs::write(home.join("config.toml"), format!(r#"[general]
default_agent = "{SHARED_LABEL}"
[integrations]
google_workspace = true
github = true
"#)).unwrap();

    // ── real stores, real apply path, real lifecycle ──────────────────────
    let shared = SharedEmployee::new(home, SHARED_LABEL, SHARED_EMPLOYEE)
        .with_work_broker(Arc::new(ListingBroker)).with_spawnd_features(spawnd_view());
    let personal = SharedEmployee::new_personal(home, PERSONAL_LABEL, PERSONAL_EMPLOYEE)
        .with_work_broker(Arc::new(ListingBroker)).with_spawnd_features(spawnd_view());
    let (shared_id, shared_binding) = shared.lifecycle_binding().unwrap();
    let (personal_id, personal_binding) = personal.lifecycle_binding().unwrap();
    assert_eq!(shared_id, SHARED_EMPLOYEE);
    assert_eq!(personal_id, PERSONAL_EMPLOYEE);
    let EmployeeBinding::Shared(tenant) = &shared_binding else { panic!("team must be Shared(tenant)") };
    assert_eq!(personal_binding, EmployeeBinding::Personal(Text::new(personal.user().to_owned()).unwrap()),
        "alice-personal must be Personal(owner)");

    // ── real connector stack (F1) over the private catalog file ──────────
    let catalog = Arc::new(ConnectorCatalog::open(home).ok().expect("private isolation-connectors.db"));
    let home_binding = installed_home_binding(home).ok();
    assert_eq!(home_binding.as_ref(), Some(&EmployeeBinding::Shared(tenant.clone())),
        "home binding = users.db installation tenant = the Shared pin");
    let inputs = ConnectorStackInputs {
        google: Arc::new(EmployeeKeyGoogleBridges { users: None, turns: Arc::new(BackgroundTurn) }),
        search: Arc::new(NoSearchWorker),
        hardware: duduclaw_inference::types::HardwareInfo {
            gpu_type: duduclaw_inference::types::GpuType::None, gpu_name: String::new(),
            vram_total_mb: 0, vram_available_mb: 0, ram_total_mb: 4096, ram_available_mb: 4096, cpu_cores: 1,
            recommended_backend: duduclaw_inference::types::BackendType::OpenAiCompat, recommended_max_model_gb: 0.0,
        },
        computer_sessions: duduclaw_gateway::computer_use_sessions::workspace_admin::admin_sessions(home),
        // D3: the CLI's real executor factories (production installs these
        // once before `start_gateway`).
        executors: ConnectorExecutorFactories {
            odoo: Arc::new(crate::mcp_isolation_connector_factory::CliOdooExecutorFactory),
            channel: Arc::new(crate::mcp_isolation_connector_factory::CliChannelExecutorFactory),
        },
        vault: Arc::new(duduclaw_gateway::isolation_connector_clients::AccountCredentialVault::default()),
    };
    let built = gateway_factory(home, &catalog, home_binding.clone(), inputs).unwrap();
    let lifecycle = Arc::new(FixtureLifecycle(BTreeMap::from([
        (SHARED_LABEL.to_owned(), (shared_id, shared_binding.clone())),
        (PERSONAL_LABEL.to_owned(), (personal_id, personal_binding.clone())),
    ])));
    // The stack's own binding authority is only consulted per selection; the
    // issuance-time membership is proven by `binding_store` per employee.
    let stack_bindings: Arc<dyn duduclaw_mcp_boundary::bindings::BindingAuthority> = Arc::new(Mutex::new(
        duduclaw_mcp_boundary::bindings::BindingStore::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap()));
    let stack = Arc::new(EmployeeConnectorProviders::new(home.to_owned(), catalog.clone(), stack_bindings,
        home_binding, lifecycle, readiness_report()).unwrap());
    stack.install_factory(built.factory.clone()).expect("factory inventory: every connector tool has a builder");
    assert!(stack.has_factory());

    let employees = [
        employee(home, &shared, SHARED_LABEL, "shared", &stack),
        employee(home, &personal, PERSONAL_LABEL, "personal", &stack),
    ];
    assert!(matches!(employees[0].context.record().employee_binding, EmployeeBinding::Shared(_)));
    assert_eq!(employees[1].context.record().employee_binding, personal_binding);
    assert!(matches!(&employees[1].context.record().principal,
        RunPrincipal::Human { user_id, .. } if user_id.as_str() == personal.user()), "Personal run = its owner");

    // ── per employee: issuer output, factory build, service completeness ──
    for employee in &employees {
        let record = employee.context.record();
        let label = employee.label;
        for (kind, present) in [
            ("workspace", record.resources.as_slice().iter().any(|g| matches!(ResourceKind::of(g), ResourceKind::Workspace))),
            ("public_origins", record.resources.as_slice().iter().any(|g| matches!(g, ResourceGrant::PublicOrigins { .. }))),
            ("new_task", record.resources.as_slice().iter().any(|g| matches!(g, ResourceGrant::NewTask { .. }))),
            ("employee_scope", record.employee_scope()),
        ] {
            assert!(present, "{label}: the real issuer did not select {kind} for the production fixture");
        }
        employee.installed.verify_identity(home, label, record.employee).unwrap();
        assert!(employee.installed.registry().missing_tools().is_empty(), "{label}: registry incomplete");
        assert_eq!(employee.installed.setup_required().count(), 0, "{label}: no route provisioned ⇒ nothing is setup-required");
        for (provider, refusal) in employee.installed.refusals() {
            assert!(*refusal == ProviderRefusal::NoRoute, "{label}/{provider}: {}", refusal.reason());
        }
        employee.dispatcher.require_complete_services()
            .unwrap_or_else(|_| panic!("{label}: a catalog tool has no installed factory"));
        employee.dispatcher.require_context_services(&employee.context).unwrap();
    }

    // ── every catalog tool ────────────────────────────────────────────────
    let tools: Vec<&'static str> = crate::mcp::tools().map(|tool| tool.name).collect();
    assert_eq!(tools.len(), CATALOG_TOOLS, "new tool: review its policy, route, grants and both employee kinds (G1)");
    let pending = pending_production_selectors();
    let manifests: Vec<BTreeMap<&'static str, ServiceState>> =
        employees.iter().map(|employee| employee.dispatcher.service_manifest().into_iter().collect()).collect();
    let mut unexplained = Vec::new();
    let mut differing = Vec::new();
    let mut offered_counts = [0usize; 2];
    println!("G1\ttool\tpolicy_route\tboundary\trequired\tshared\tpersonal");
    for name in &tools {
        let name = *name;
        let policy = policy_table::policy(name).unwrap_or_else(|| panic!("{name}: catalog tool without a tool_policy entry"));
        let boundary = boundary_route(name).unwrap_or_else(|| panic!("{name}: catalog tool without an exact boundary route"));
        // Required grants: never empty; the own Employee grant first, except
        // the brokered File tools.
        assert!(!policy.required.is_empty(), "{name}: policy requires no grant");
        if WORKSPACE_ONLY.contains(&name) {
            assert!(policy.required == [ResourceKind::Workspace], "{name}: File tools require exactly the Workspace grant");
            assert!(matches!(boundary, BoundaryRoute::File), "{name}");
        } else {
            assert!(policy.required[0] == ResourceKind::Employee, "{name}: policy must start with the own Employee grant");
        }
        if policy.route == Route::Connector {
            assert!(provider_for_tool(name).is_some(), "{name}: connector policy without a provider");
        }
        if policy_table::IN_RUN_HELPER_TOOLS.contains(&name) || policy_table::IN_RUN_RECORDING_TOOLS.contains(&name) {
            assert!(policy.route == Route::InRunHelper, "{name}: in-run helper tools stay on the in-run route");
        }
        let mut cells = Vec::new();
        let mut eligible = [false; 2];
        for (index, employee) in employees.iter().enumerate() {
            let context = &employee.context;
            let is_eligible = policy_table::eligible(context, name);
            let refused = crate::mcp_isolation_disposition::intentional_refusal(name).is_some();
            let state = manifests[index].get(name).cloned().unwrap_or_else(|| panic!("{name}: absent from the manifest"));
            if pending.contains(name) {
                assert!(!is_eligible, "{name}: pending selector became eligible for {} ({})", employee.label, employee.kind);
            }
            let offered = is_eligible && !refused;
            eligible[index] = offered;
            if offered {
                offered_counts[index] += 1;
                assert!(state == ServiceState::Ready,
                    "{name}: eligible for {} ({}) but its factory is not ready", employee.label, employee.kind);
                if policy.route == Route::Connector && matches!(boundary, BoundaryRoute::Resource) {
                    let registry = employee.installed.registry();
                    assert!(LiveConnectorSelectionPolicy::supports(registry.as_ref(), name)
                        && BoundConnectorExecutor::supports(registry.as_ref(), name),
                        "{name}: the real per-employee connector registry cannot serve it for {}", employee.label);
                }
                cells.push("offered".to_owned());
            } else {
                match hidden_reason(context, &pending, name) {
                    Some(reason) => cells.push(reason),
                    None => { unexplained.push(format!("{name} ({})", employee.kind)); cells.push("UNEXPLAINED".into()); }
                }
            }
        }
        if eligible[0] != eligible[1] && !BINDING_DEPENDENT.contains(&name) {
            differing.push(format!("{name}: shared={} personal={}", cells[0], cells[1]));
        }
        let required: Vec<&str> = policy.required.iter().map(kind_name).collect();
        println!("G1\t{name}\t{}\t{}\t{}\t{}\t{}", route_name(policy.route), boundary_name(&boundary),
            required.join("+"), cells[0], cells[1]);
    }
    println!("G1 offered: shared={} personal={} of {}", offered_counts[0], offered_counts[1], tools.len());
    assert!(unexplained.is_empty(), "tools hidden for no reviewed reason (new selector gap?): {unexplained:?}");
    assert!(differing.is_empty(), "Personal/Shared eligibility differs outside the reviewed set: {differing:?}");
    // The fixture is production-shaped: both employees must actually be
    // offered the bulk of the catalog, not a trivially empty set.
    for (index, employee) in employees.iter().enumerate() {
        assert!(offered_counts[index] > tools.len() / 2, "{}: only {} tools offered", employee.label, offered_counts[index]);
    }
}
