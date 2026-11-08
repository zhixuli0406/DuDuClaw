//! Trusted boot composition. Missing producers are explicit; no socket-default
//! worker, synthetic principal, or inferred runtime acceptance is installed.
use std::{path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock}, sync::atomic::{AtomicBool, Ordering}};
use duduclaw_isolation::{execution::{SpawnPolicy, WaitOrigin}, ids::EmployeeId};
use duduclaw_mcp_boundary::{bindings::BindingAuthority, execution::ExecutionPlanFactory,
    factories::GatewayFactories, resources::ResourceRoots, worker_execution::WorkerPlanFactory};
use duduclaw_parse_worker::client::WorkerLauncher;
use super::{Dispatcher, GrantPolicy, LiveAuthority, McpBoundaryService, ProtectedMcpLaunch,
    RuntimeBridgeRegistry, VerificationError};
use crate::{isolation_connectors::{BoundConnectorExecutor, ConnectorCatalog, LiveConnectorSelectionPolicy},
    isolation_egress::RunTurnResolver, isolation_resources::GatewayResourceResolver};
use crate::isolation_connector_boot::ConnectorProviderFactory;
use crate::isolation_connector_stack::EmployeeConnectorProviders;
use crate::isolation_lifecycle::{self as lifecycle, EmployeeLifecycle, RootAllocation, RootAllocator};
use super::authority::{lifecycle_refusal, personal_facts};
use super::selectors::{self, SelectorInput, SelectorSources};

/// CLI supplies its adapter before start_gateway. The gateway owns every input;
/// the adapter must authenticate its original dispatcher and check completeness.
pub trait DispatcherFactory: Send + Sync {
    fn create(&self, input: DispatcherInput) -> Result<Arc<dyn Dispatcher>, VerificationError>;
    fn verify_producers(&self, _execution: &dyn ExecutionPlanFactory, _worker: &dyn WorkerPlanFactory,
        _connectors: &dyn BoundConnectorExecutor, _selection: &dyn LiveConnectorSelectionPolicy)
        -> Result<(), VerificationError> { Err(VerificationError::Unavailable) }
    /// Snapshot the real product catalog under the employee's current policy.
    /// Completeness is still checked independently when creating a dispatcher.
    fn ingress_tools(&self, _home: &Path, _agent: &str, _lane: duduclaw_isolation::capability::Lane)
        -> Result<duduclaw_isolation::bounded::List<duduclaw_isolation::bounded::Text<128>, 256>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}
pub struct DispatcherInput {
    pub home: PathBuf,
    pub employee: EmployeeId,
    pub agent: String,
    pub roots: Arc<ResourceRoots>,
    pub resolver: Arc<GatewayResourceResolver>,
    pub factories: GatewayFactories,
    pub turns: Arc<dyn RunTurnResolver>,
    pub connectors: Option<Arc<dyn BoundConnectorExecutor>>,
}

/// All fields are populated by trusted host integration, never deserialized.
/// `authority` must provide fresh A control/run status and gateway policy state;
/// the boot wrapper additionally validates durable principal/member bindings.
/// The worker launcher must own host budgets through proven retirement. E's
/// SocketActivatedLauncher alone does not satisfy this contract.
#[derive(Default)]
pub struct BootSources {
    product_authority: Option<Arc<super::authority::GatewayAuthority>>,
    installed_generation: Option<duduclaw_isolation::ids::Sha256Digest>,
    pub authority: Option<Arc<dyn LiveAuthority>>,
    pub bindings: Option<Arc<dyn BindingAuthority>>,
    pub runtime_registry: Option<Arc<dyn RuntimeBridgeRegistry>>,
    pub main_plans: Option<Arc<duduclaw_spawnd::gateway_plan::InstalledMainPlanFactory>>,
    pub execution_plans: Option<Arc<dyn ExecutionPlanFactory>>,
    pub fork_tools: Option<Arc<dyn duduclaw_mcp_boundary::forks::ForkToolService>>,
    pub worker_launcher: Option<Box<dyn WorkerLauncher>>,
    pub worker_plans: Option<Arc<dyn WorkerPlanFactory>>,
    pub turns: Option<Arc<dyn RunTurnResolver>>,
    pub connector_catalog: Option<Arc<ConnectorCatalog>>,
    pub connectors: Option<Arc<dyn BoundConnectorExecutor>>,
    pub connector_selection: Option<Arc<dyn LiveConnectorSelectionPolicy>>,
    /// H9/H10: optional host override of the root work broker. Production
    /// leaves it `None`: W1's `assemble` then installs run-bound brokers over
    /// the protected backend (`run_broker.rs`, H5 `SpawndWorkspaceBridge`).
    /// Without either the issuer never selects a Workspace grant (tools hidden,
    /// fail closed); it is not a boot-completeness producer.
    pub work_broker: Option<Arc<dyn duduclaw_mcp_boundary::resources::WorkBroker>>,
    /// F1: per-employee connector stack over `connector_catalog`. When boot
    /// creates it, `connectors`/`connector_selection` are this same object
    /// (inventory only) and each launch gets an employee-bound handle.
    connector_stack: Option<Arc<EmployeeConnectorProviders>>,
    #[cfg_attr(not(feature = "isolation-worker-integration"), allow(dead_code))]
    connector_inputs: Option<ConnectorBootInputs>,
}

/// Gateway-side inputs of the production connector factory, collected in
/// `boot_protected` and consumed by `assemble` (after the worker exists).
#[cfg_attr(not(feature = "isolation-worker-integration"), allow(dead_code))]
struct ConnectorBootInputs {
    users: Arc<duduclaw_auth::UserDb>,
    turns: Arc<dyn RunTurnResolver>,
    home_binding: Option<duduclaw_isolation::capability::EmployeeBinding>,
    hardware: duduclaw_inference::types::HardwareInfo,
    vault: Arc<crate::isolation_connector_clients::AccountCredentialVault>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingProducer {
    Backend, LiveAuthority, DurableBindings, RuntimePublication, ExecutionPlans,
    WorkerRetirement, WorkerPlans, TurnAuthority, Dispatcher, WorkerIntegration, MainPlans, ConnectorProviders, Initialization,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct BootStatus {
    /// Constructed services are not root/runtime acceptance evidence.
    pub constructed: bool,
    pub missing: Vec<MissingProducer>,
}

static ATTEMPTED: AtomicBool = AtomicBool::new(false);
static SOURCES: Mutex<Option<BootSources>> = Mutex::new(None);
static DISPATCHER: OnceLock<Arc<dyn DispatcherFactory>> = OnceLock::new();
static OWNER: OnceLock<Arc<BootOwner>> = OnceLock::new();
static STATUS: OnceLock<BootStatus> = OnceLock::new();
static CONNECTOR_FACTORY: OnceLock<Arc<dyn ConnectorProviderFactory>> = OnceLock::new();

pub fn install_dispatcher_factory(factory: Arc<dyn DispatcherFactory>) -> Result<(), VerificationError> {
    if ATTEMPTED.load(Ordering::Acquire) { return Err(VerificationError::Unavailable); }
    DISPATCHER.set(factory).map_err(|_| VerificationError::Unavailable)
}
/// F1: optional host override of the per-employee connector factory, before
/// `start_gateway`. Without it boot assembles `GatewayConnectorFactory` with
/// the CLI executor factories (`connector_executor_factories()`). Either way
/// its inventory must pass, or every connector tool stays a refusing adapter.
pub fn install_connector_factory(factory: Arc<dyn ConnectorProviderFactory>) -> Result<(), VerificationError> {
    if ATTEMPTED.load(Ordering::Acquire) { return Err(VerificationError::Unavailable); }
    CONNECTOR_FACTORY.set(factory).map_err(|_| VerificationError::Unavailable)
}
pub fn install_boot_sources(sources: BootSources) -> Result<(), VerificationError> {
    let mut slot = SOURCES.lock().map_err(|_| VerificationError::Unavailable)?;
    if ATTEMPTED.load(Ordering::Acquire) || slot.is_some() { return Err(VerificationError::Unavailable); }
    *slot = Some(sources);
    Ok(())
}
fn fill_installed_sources(sources: &mut BootSources, home: &Path) {
    let generation = crate::isolation_readiness::verified_generation();
    sources.installed_generation = generation;
    if sources.worker_plans.is_none() {
        if let Ok(factory) = crate::isolation_worker_plans::GatewayWorkerPlans::new(home.to_owned()) {
            sources.worker_plans = Some(Arc::new(factory));
        }
    }
    if sources.execution_plans.is_none() {
        if let Some(generation) = generation {
            if let Ok(factory) = crate::isolation_execution::InstalledProgramPlans::new(
                generation, nix::unistd::getegid().as_raw()) {
                sources.execution_plans = Some(factory);
            }
        }
    }
    if sources.main_plans.is_none() {
        if let Some(generation) = generation {
            if let Ok(factory) = duduclaw_spawnd::gateway_plan::InstalledMainPlanFactory::new(
                generation, nix::unistd::getegid().as_raw()) {
                sources.main_plans = Some(Arc::new(factory));
            }
        }
    }
    if sources.runtime_registry.is_none() {
        if let Some(generation) = generation {
            if let Ok(registry) = duduclaw_spawnd::gateway_mcp::InstalledRuntimeRegistry::new(
                generation, nix::unistd::getegid().as_raw()) {
                sources.runtime_registry = Some(Arc::new(registry));
            }
        }
    }
    if sources.worker_launcher.is_none() {
        // The launcher acquires its durable host slot before connecting and
        // requires exact worker identity plus proven retirement. Construction
        // alone is deliberately not composite readiness evidence.
        sources.worker_launcher = Some(Box::new(duduclaw_parse_worker::retirement::RetiringLauncher));
    }
}

fn missing(sources: &BootSources, dispatcher: bool, backend: bool, worker: bool) -> Vec<MissingProducer> {
    let mut absent = Vec::new();
    for (present, producer) in [(backend, MissingProducer::Backend),
        (sources.authority.is_some(), MissingProducer::LiveAuthority),
        (sources.bindings.is_some(), MissingProducer::DurableBindings),
        (sources.runtime_registry.is_some(), MissingProducer::RuntimePublication),
        (sources.main_plans.is_some(), MissingProducer::MainPlans),
        (sources.execution_plans.is_some(), MissingProducer::ExecutionPlans),
        (sources.worker_launcher.is_some(), MissingProducer::WorkerRetirement),
        (sources.worker_plans.is_some(), MissingProducer::WorkerPlans),
        (sources.turns.is_some(), MissingProducer::TurnAuthority),
        (sources.connector_catalog.is_some() && sources.connectors.is_some()
            && sources.connector_selection.is_some(), MissingProducer::ConnectorProviders),
        (dispatcher, MissingProducer::Dispatcher), (worker, MissingProducer::WorkerIntegration)] {
        if !present { absent.push(producer); }
    }
    absent
}

/// Called once after protected TEAM mode and A's backend have been installed.
/// No employee work runs here; the existing composite gate stays authoritative.
pub async fn boot_protected(home: &Path, users: Arc<duduclaw_auth::UserDb>, agents: Arc<super::AgentAdmission>) {
    if crate::isolation_readiness::status().is_none() || ATTEMPTED.swap(true, Ordering::AcqRel) { return; }
    let mut sources = SOURCES.lock().ok().and_then(|mut slot| slot.take()).unwrap_or_default();
    // F's first asynchronous probe may still be in flight during startup. Wait
    // a bounded interval before consuming the root report; never mark it ready
    // merely because the unit/socket exists.
    for _ in 0..50 {
        if crate::isolation_readiness::verified_generation().is_some() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    fill_installed_sources(&mut sources, home);
    let backend = duduclaw_execution::selection::protected_backend_for_boot();
    // Reuse the gateway's initialized users.db and confirmed employee registry.
    // No parallel principal/binding database is created for the boundary.
    if sources.authority.is_none() && sources.bindings.is_none() {
        if let Ok(backend) = &backend {
            // C2: the authority checks connector uses against the production
            // catalog, and must read the very instance selection pins against.
            // Without one, every connector use is Unavailable.
            if sources.connector_catalog.is_none() {
                sources.connector_catalog = ConnectorCatalog::open(home).ok().map(Arc::new);
            }
            if let Ok(authority) = super::authority::GatewayAuthority::new(home.to_owned(), users.clone(), backend.clone(), agents,
                sources.connector_catalog.clone()) {
                let authority = Arc::new(authority);
                sources.authority = Some(authority.clone());
                sources.bindings = Some(authority.clone());
                if sources.turns.is_none() { sources.turns = Some(authority.clone()); }
                sources.product_authority = Some(authority);
            }
        }
    }
    // F1: the per-employee connector stack, over the very catalog handle the
    // authority above checks connector uses against. Only when the host did
    // not supply its own singletons; a missing catalog leaves the producer
    // missing (boot stays paused, every connector use Unavailable).
    if sources.connectors.is_none() && sources.connector_selection.is_none() {
        install_connector_stack(&mut sources, home, users).await;
    }
    let absent = missing(&sources, DISPATCHER.get().is_some(), backend.is_ok(), cfg!(feature = "isolation-worker-integration"));
    if !absent.is_empty() {
        tracing::error!(missing = ?absent, "隔離工具服務尚缺可信任的啟動資料，團隊執行維持暫停。");
        let _ = STATUS.set(BootStatus { constructed: false, missing: absent });
        return;
    }
    #[cfg(feature = "isolation-worker-integration")]
    {
        let result = assemble(home, backend.expect("checked backend"), sources);
        match result {
            Ok(owner) => {
                if OWNER.set(Arc::new(owner)).is_ok() {
                    let _ = STATUS.set(BootStatus { constructed: true, missing: vec![] });
                    if crate::isolation_readiness::install_integration_probe(
                        crate::isolation_readiness::IntegrationComponent::McpBoundary, Arc::new(BoundaryReadiness)).is_err() {
                        tracing::error!("隔離工具服務的狀態檢查無法註冊，團隊執行維持暫停。");
                    }
                }
            }
            Err(_) => {
                tracing::error!("隔離工具服務初始化未通過，團隊執行維持暫停。");
                let _ = STATUS.set(BootStatus { constructed: false, missing: vec![MissingProducer::Initialization] });
            }
        }
    }
}

/// F1 step 1 (before the producer check): the per-employee stack over the
/// boot catalog and the process credential vault, installed into the
/// connector lifecycle so C3 revoke / C4 events drop exactly the material the
/// executors read. Any failure leaves the connector producer missing.
async fn install_connector_stack(sources: &mut BootSources, home: &Path, users: Arc<duduclaw_auth::UserDb>) {
    use crate::isolation_connector_stack::{LifecycleBindings, installed_home_binding, readiness_report};
    let (Some(catalog), Some(bindings), Some(turns)) =
        (sources.connector_catalog.clone(), sources.bindings.clone(), sources.turns.clone()) else { return };
    if sources.product_authority.is_none() { return; }
    // Unreadable users.db: no home routes are listed (narrower, never wider);
    // every lifecycle read refuses as well.
    let home_binding = installed_home_binding(home).ok();
    let Ok(lifecycle) = LifecycleBindings::new(home.to_owned()) else { return };
    let Ok(stack) = EmployeeConnectorProviders::new(home.to_owned(), catalog, bindings, home_binding.clone(),
        Arc::new(lifecycle), readiness_report()) else { return };
    let stack = Arc::new(stack);
    let vault = Arc::new(crate::isolation_connector_clients::AccountCredentialVault::default());
    // The vault the executors use must be the one revoke drops from.
    if crate::isolation_connector_lifecycle::install_credential_material(vault.clone()).is_err() {
        tracing::error!("隔離連接器的憑證保管區已被其他元件註冊，連接器維持停用。");
        return;
    }
    let weak = Arc::downgrade(&stack);
    if crate::isolation_connector_lifecycle::install_route_change_hook(Arc::new(move ||
        if let Some(stack) = weak.upgrade() { stack.schedule_refresh(); })).is_err() {
        tracing::warn!("隔離連接器的路由變更通知無法註冊；連接器仍會在每次使用時重新檢查。");
    }
    let hardware = duduclaw_inference::hardware::detect_hardware().await;
    sources.connectors = Some(stack.clone());
    sources.connector_selection = Some(stack.clone());
    sources.connector_stack = Some(stack);
    sources.connector_inputs = Some(ConnectorBootInputs { users, turns, home_binding, hardware, vault });
}

/// F1 step 2 (in `assemble`, after the worker exists): install the factory —
/// the host override or `GatewayConnectorFactory` with D3's CLI executor
/// factories, D2 native home providers, the E2/E4 Google bridge and the boot
/// vault — then build every confirmed employee once (reported to F3). A
/// factory that cannot be assembled leaves every connector tool refusing and
/// every employee `connector_setup_required`; boot itself continues.
#[cfg(feature = "isolation-worker-integration")]
fn install_stack_factory(home: &Path, stack: &Arc<EmployeeConnectorProviders>, inputs: ConnectorBootInputs,
    worker: &crate::isolation_execution::InstalledWorkerServices) {
    use crate::isolation_connector_stack::{ConnectorStackInputs, EmployeeKeyGoogleBridges, gateway_factory};
    let factory = match CONNECTOR_FACTORY.get() {
        Some(factory) => Ok(factory.clone()),
        None => gateway_factory(home, stack.catalog(), inputs.home_binding, ConnectorStackInputs {
            google: Arc::new(EmployeeKeyGoogleBridges { users: Some(inputs.users), turns: inputs.turns }),
            search: Arc::new(worker.clone()),
            hardware: inputs.hardware,
            computer_sessions: crate::computer_use_sessions::workspace_admin::admin_sessions(home),
            executors: crate::isolation_connector_boot::connector_executor_factories(),
            vault: inputs.vault,
        }).map(|assembled| assembled.factory as Arc<dyn ConnectorProviderFactory>),
    };
    match factory.and_then(|factory| stack.install_factory(factory)) {
        Ok(()) => stack.schedule_refresh(),
        Err(_) => tracing::error!("隔離連接器工廠無法建立，所有連接器工具維持拒絕；請檢查連接器設定後重新啟動服務。"),
    }
}

pub fn boot_status() -> BootStatus {
    let mut status = STATUS.get().cloned().unwrap_or(BootStatus { constructed: false,
        missing: vec![MissingProducer::Initialization] });
    if status.constructed && OWNER.get().is_none_or(|owner| !owner.boundary.healthy()) {
        status.constructed = false;
        status.missing = vec![MissingProducer::Initialization];
    }
    status
}

/// A's v1.80 producer hook excludes exactly the documented fork mutations.
/// Recheck the installed helpers on every observation; source presence alone
/// does not override missing/replaced signed runtime publications.
pub fn a_producers_ready() -> bool {
    OWNER.get().is_some_and(|owner| owner.boundary.healthy()
        && duduclaw_mcp_boundary::fork_policy::remaining_producers_ready(
            |name| owner.execution_plans.supports(name),
            |name| owner.fork_tools.supports(name)))
}

pub struct LaunchInput {
    pub agent: String,
    pub plan: SpawnPolicy,
    pub grant: GrantPolicy,
    pub roots: Arc<ResourceRoots>,
    pub origin: WaitOrigin,
    pub caller: Option<EmployeeId>,
    pub turn: Option<super::authority::HostTurn>,
    pub authenticated_epoch: Option<u64>,
}
struct BootOwner {
    generation: duduclaw_isolation::ids::Sha256Digest,
    product_authority: Option<Arc<super::authority::GatewayAuthority>>,
    home: PathBuf,
    backend: Arc<duduclaw_execution::SpawndBackend>,
    boundary: McpBoundaryService,
    bindings: Arc<dyn BindingAuthority>,
    registry: Arc<dyn RuntimeBridgeRegistry>,
    main_plans: Arc<duduclaw_spawnd::gateway_plan::InstalledMainPlanFactory>,
    execution_plans: Arc<dyn ExecutionPlanFactory>,
    execution_jobs: Arc<duduclaw_mcp_boundary::execution_jobs::SqliteExecutionJobs>,
    /// HX6: `(run, call)` rows of in-run browser recordings (same DB file).
    recording_jobs: Arc<duduclaw_mcp_boundary::recording_jobs::SqliteRecordingJobs>,
    fork_tools: Arc<dyn duduclaw_mcp_boundary::forks::ForkToolService>,
    worker_plans: Arc<dyn WorkerPlanFactory>,
    #[cfg(feature = "isolation-worker-integration")]
    worker: crate::isolation_execution::InstalledWorkerServices,
    turns: Arc<dyn RunTurnResolver>,
    catalog: Option<Arc<ConnectorCatalog>>,
    connectors: Option<Arc<dyn BoundConnectorExecutor>>,
    connector_selection: Option<Arc<dyn LiveConnectorSelectionPolicy>>,
    /// F1: when present, each launch selects/executes through an
    /// employee-bound handle of this stack instead of the singletons above.
    connector_stack: Option<Arc<EmployeeConnectorProviders>>,
    /// H9 selector producers, read by the issuer on every authorization.
    selectors: SelectorSources,
}

/// D proves its live owner and the binary's actual complete routing inventory.
/// E's containment and all configured runtimes have separate mandatory probes.
struct BoundaryReadiness;
impl crate::isolation_readiness::IntegrationProbe for BoundaryReadiness {
    fn verify_producers(&self) -> Result<(), crate::isolation_readiness::ProbeError> {
        use crate::isolation_readiness::ProbeError;
        let owner = OWNER.get().ok_or(ProbeError::Unavailable)?;
        if owner.product_authority.is_none() || !cfg!(feature = "isolation-worker-integration") {
            return Err(ProbeError::Unavailable);
        }
        DISPATCHER.get().ok_or(ProbeError::Unavailable)?.verify_producers(
            owner.execution_plans.as_ref(), owner.worker_plans.as_ref(),
            owner.connectors.as_deref().ok_or(ProbeError::Unavailable)?,
            owner.connector_selection.as_deref().ok_or(ProbeError::Unavailable)?)
            .map_err(|_| ProbeError::Unavailable)
    }
    fn verify_service(&self, generation: duduclaw_isolation::ids::Sha256Digest)
        -> Result<crate::isolation_readiness::ServiceObservation, crate::isolation_readiness::ProbeError> {
        use crate::isolation_readiness::{ProbeError, ServiceObservation};
        let started = std::time::Instant::now();
        let owner = OWNER.get().ok_or(ProbeError::Unavailable)?;
        if !owner.boundary.healthy() || owner.generation != generation { return Err(ProbeError::Unavailable); }
        let backend = duduclaw_execution::selection::protected_backend_for_boot().map_err(|_| ProbeError::Unavailable)?;
        if !Arc::ptr_eq(&owner.backend, &backend) { return Err(ProbeError::Unavailable); }
        let publication = duduclaw_spawnd::installed::GatewayPublication::read(nix::unistd::getegid().as_raw())
            .map_err(|_| ProbeError::Unavailable)?;
        if publication.generation != generation { return Err(ProbeError::Unavailable); }
        // B1's authenticated receipt is independently required by the composite
        // gate. This proof adds the exact owner/control instance and D lifetime.
        Ok(ServiceObservation::checked(publication.generation, started))
    }
}

/// Authenticated ingress and approved background records share this issuer.
/// The wrapper owns live store reads. Additional IO grants come only from the
/// H9 selectors (`selectors.rs`), which read their inputs at issuance from the
/// authoritative stores; `task` is the host-pinned record task of a
/// task/schedule run (`None` for interactive ingress), never a tool argument.
pub(crate) fn record_authorization(home: PathBuf, agent: String, user: String, epoch: u64,
    lane: duduclaw_isolation::capability::Lane, turn: super::HostTurn, credential_expiry: Option<u64>,
    task: Option<String>) -> Result<super::RuntimeAuthorization, VerificationError> {
    let owner = OWNER.get().ok_or(VerificationError::Unavailable)?;
    if owner.home != home || !owner.boundary.healthy() { return Err(VerificationError::Unavailable); }
    let authority = owner.product_authority.as_ref().ok_or(VerificationError::Unavailable)?;
    let lifecycle = lifecycle::read_lifecycle(&home, &agent).map_err(lifecycle_refusal)?;
    let current = authority.ingress_snapshot(&user, &agent)?;
    super::ingress::require_epoch(epoch, current.human.as_ref().map(|human| human.credential_epoch))?;
    let principal = issuance_principal(&user, &current)?;
    issuance_facts(&agent, &lifecycle, &principal, &current, turn.background.is_some())?;
    let policy = crate::approval::strict_policy_revision(&home, &agent)
        .map_err(|_| VerificationError::Unavailable)?;
    let tools = DISPATCHER.get().ok_or(VerificationError::Unavailable)?.ingress_tools(&home, &agent, lane)?;
    // Keep the lifecycle pinned before the policy/catalog I/O; never repin to
    // a replacement employee in the constructor.
    let authorization = human_authorization_with(home.clone(), agent.clone(), lifecycle.employee,
        user.clone(), epoch, lane, &current, tools, turn, credential_expiry, &lifecycle, &RootAllocator,
        &owner.selectors, task.as_deref())?;
    if crate::approval::strict_policy_revision(&home, &agent).ok().as_deref() != Some(policy.as_str()) {
        return Err(VerificationError::Revoked);
    }
    lifecycle::recheck_with(&home, &lifecycle, &RootAllocator).map_err(lifecycle_refusal)?;
    let after = authority.ingress_snapshot(&user, &agent)?;
    recheck_issuance_facts(&agent, &lifecycle, &principal, epoch, &current, &after,
        authorization.turn.as_ref().is_some_and(|turn| turn.background.is_some()))?;
    Ok(authorization)
}

fn issuance_principal(user: &str, current: &duduclaw_auth::IsolationAuthoritySnapshot)
    -> Result<duduclaw_isolation::capability::Principal, VerificationError> {
    Ok(duduclaw_isolation::capability::Principal::Human {
        user_id: duduclaw_isolation::bounded::Text::new(user.to_owned()).map_err(|_| VerificationError::Invalid)?,
        generation: current.acl_generation,
    })
}

/// Record-backed work must not impersonate a Personal owner's live ingress.
/// All Personal identity comparisons stay in B1's shared gate.
fn issuance_facts(agent: &str, lifecycle: &EmployeeLifecycle,
    principal: &duduclaw_isolation::capability::Principal,
    current: &duduclaw_auth::IsolationAuthoritySnapshot, record: bool) -> Result<(), VerificationError> {
    use duduclaw_isolation::capability::EmployeeBinding;
    match &lifecycle.binding {
        EmployeeBinding::Personal(owner) => {
            if record { return Err(VerificationError::Revoked); }
            personal_facts(agent, owner.as_str(), principal, current)
        }
        EmployeeBinding::Shared(tenant) => {
            if current.tenant_id != tenant.as_str() || current.personal_owner.is_some() {
                return Err(VerificationError::Revoked);
            }
            Ok(())
        }
    }
}

fn recheck_issuance_facts(agent: &str, lifecycle: &EmployeeLifecycle,
    principal: &duduclaw_isolation::capability::Principal, epoch: u64,
    before: &duduclaw_auth::IsolationAuthoritySnapshot, after: &duduclaw_auth::IsolationAuthoritySnapshot,
    record: bool) -> Result<(), VerificationError> {
    if before.acl_generation != after.acl_generation || before.policy_epoch != after.policy_epoch
        || before.tenant_id != after.tenant_id
        || after.human.as_ref().is_none_or(|human| human.credential_epoch != epoch
            || human.must_change_password || !human.can_assign_to(agent)) {
        return Err(VerificationError::Revoked);
    }
    issuance_facts(agent, lifecycle, principal, after, record)
}

/// Production capability construction from authenticated host facts. Callers
/// must authenticate these facts. Lifecycle and users.db are rechecked after
/// resource I/O. No MCP, environment or deserialization entry point.
/// Inventory tests call this exact constructor, never append resource grants.
/// SR2 finding 8: tests and `test-support` only; production goes through
/// `record_authorization` / the boot issuer (`human_authorization_with`).
#[cfg(any(test, feature = "test-support"))]
pub fn human_authorization(home: PathBuf, agent: String, employee: EmployeeId,
    user: String, epoch: u64, lane: duduclaw_isolation::capability::Lane,
    current: &duduclaw_auth::IsolationAuthoritySnapshot,
    tools: duduclaw_isolation::bounded::List<duduclaw_isolation::bounded::Text<128>, 256>,
    turn: super::HostTurn, credential_expiry: Option<u64>)
    -> Result<super::RuntimeAuthorization, VerificationError> {
    let lifecycle = lifecycle::read_lifecycle(&home, &agent).map_err(lifecycle_refusal)?;
    human_authorization_with(home, agent, employee, user, epoch, lane, current, tools, turn,
        credential_expiry, &lifecycle, &RootAllocator, &SelectorSources::default(), None)
}

#[allow(clippy::too_many_arguments)]
fn human_authorization_with(home: PathBuf, agent: String, employee: EmployeeId,
    user: String, epoch: u64, lane: duduclaw_isolation::capability::Lane,
    current: &duduclaw_auth::IsolationAuthoritySnapshot,
    tools: duduclaw_isolation::bounded::List<duduclaw_isolation::bounded::Text<128>, 256>,
    turn: super::HostTurn, credential_expiry: Option<u64>, lifecycle: &EmployeeLifecycle, root: &dyn RootAllocation,
    sources: &SelectorSources, task: Option<&str>)
    -> Result<super::RuntimeAuthorization, VerificationError> {
    use duduclaw_isolation::{bounded::{List, Text}, capability::ResourceGrant};
    if lifecycle.employee != employee || lifecycle.label != agent { return Err(VerificationError::Revoked); }
    if current.human.as_ref().is_none_or(|human| human.user_id != user
        || human.credential_epoch != epoch || human.must_change_password
        || !human.can_assign_to(&agent)) {
        return Err(VerificationError::Revoked);
    }
    let principal = issuance_principal(&user, current)?;
    issuance_facts(&agent, lifecycle, &principal, current, turn.background.is_some())?;
    // H10: the H9 selectors run after the identity/binding gates and before the
    // lifecycle/users.db rechecks below, so those rechecks cover their I/O. A
    // failed read may be a tombstone moving `agents/<label>` away: the durable
    // lifecycle decides first (as for the wiki walk below).
    let record = turn.background.is_some();
    let select = || selectors::select(sources, &SelectorInput { home: &home, agent: &agent, user: &user,
        tools: &tools, task, record })
        .map_err(|error| lifecycle::recheck_with(&home, lifecycle, root).err().map_or(error, lifecycle_refusal));
    let selection = select()?;
    let signed_tools = selection.signed_tools(&tools)?; // Workspace signs File (+ HX5 helpers), never pending A/E
    let text = |value| Text::new(value).map_err(|_| VerificationError::Invalid);
    let namespace = format!("agent:{agent}");
    let confined = |resource| ResourceGrant::Confined { resource: Box::new(resource) };
    let mut resources = vec![ResourceGrant::Employee { id: employee },
        ResourceGrant::EmployeeScope { id: employee },
        confined(ResourceGrant::Mailbox { id: text(namespace.clone())? })];
    for table in ["working_state", "beliefs", "user_profile", "decisions", "memory"] {
        resources.push(confined(ResourceGrant::DatabaseTable {
            database: text(namespace.clone())?, table: text(table.into())? }));
    }
    let mut roots = ResourceRoots::default();
    // These directories live in gateway-private state, never the writable
    // employee mount. Walk from a trusted home fd without following symlinks.
    // A tombstone moves `agents/<label>` into quarantine before disabling the
    // apply.db row, so a failed walk may be a lifecycle change rather than an
    // unreadable store: the durable lifecycle decides first (Tombstoned/Stale
    // ⇒ Revoked); only a still-live employee keeps the walk's own error.
    let wiki = super::ingress::own_wiki_root(&home, &agent).map_err(|error|
        lifecycle::recheck_with(&home, lifecycle, root).err().map_or(error, lifecycle_refusal))?;
    if let Some(wiki) = wiki {
        roots.insert_wiki(namespace.clone(), employee, wiki)?;
        resources.push(confined(ResourceGrant::Wiki { namespace: text(namespace)? }));
    }
    // Workspace{employee-work} + brokered root, PublicOrigins, NewTask: plain
    // grants in the signed resource set, so the capability MAC covers them.
    selection.apply(sources, lifecycle, &mut resources, &mut roots)?; // W1: broker bound to employee+incarnation
    let now = duduclaw_mcp_boundary::authority::now_seconds()?;
    let expires_at = now.checked_add(30 * 60).ok_or(VerificationError::InvalidTime)?
        .min(credential_expiry.unwrap_or(u64::MAX));
    if expires_at <= now { return Err(VerificationError::InvalidTime); }
    // Root/store reads and resource resolution may race with handover or
    // revocation. Re-read the pinned lifecycle, then the latest users.db facts.
    lifecycle::recheck_with(&home, lifecycle, root).map_err(lifecycle_refusal)?;
    // SR2 finding 7: the hardened private-SQLite opener, as on every
    // lifecycle path (no-follow walk, owner/mode/link/inode checks).
    let mut users = crate::fs_safe::sqlite::open_existing(&home, "users.db", crate::fs_safe::sqlite::Access::ReadOnly)
        .map_err(|_| VerificationError::Unavailable)?;
    users.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| VerificationError::Unavailable)?;
    let after = duduclaw_auth::read_isolation_authority(&mut users, Some(&user), &agent)
        .map_err(|_| VerificationError::Revoked)?;
    recheck_issuance_facts(&agent, lifecycle, &principal, epoch, current, &after, turn.background.is_some())?;
    // H10: re-read every selector input after all issuance I/O; a profile,
    // public-web switch or task ownership that changed meanwhile refuses the run.
    if select()? != selection { return Err(VerificationError::Revoked); }
    let mut authorization = super::RuntimeAuthorization { home, agent, grant: GrantPolicy {
        task: None, principal, binding: lifecycle.binding.clone(), lane, tools: signed_tools,
        resources: List::new(resources).map_err(|_| VerificationError::Invalid)?,
        policy_revision: current.policy_epoch, not_before: now, expires_at }, roots: Arc::new(roots),
        origin: WaitOrigin::Gateway, caller: None, turn: Some(turn), authenticated_epoch: Some(epoch) };
    // Task/schedule runs are task-limited (no EmployeeScope, PublicOrigins or
    // NewTask) with the record Task grant (H8); `select` has already refused a
    // task the employee no longer owns.
    if let Some(task) = &selection.task {
        authorization.limit_to_task(task.id.clone())?;
    }
    Ok(authorization)
}

pub(crate) fn delegation_source(home: &Path, sender: &str, context: &duduclaw_mcp_boundary::RequestContext)
    -> Result<crate::isolation_background::DelegationSource, VerificationError> {
    let owner = OWNER.get().ok_or(VerificationError::Unavailable)?;
    if owner.home != home || !owner.boundary.healthy() { return Err(VerificationError::Unavailable); }
    let source = owner.product_authority.as_ref().ok_or(VerificationError::Unavailable)?.delegation_source(context)?;
    if source.agent != sender { return Err(VerificationError::BindingMismatch); }
    Ok(source)
}
#[cfg(feature = "isolation-worker-integration")]
fn assemble(home: &Path, backend: Arc<duduclaw_execution::SpawndBackend>, mut sources: BootSources)
    -> Result<BootOwner, VerificationError> {
    if !home.is_absolute() { return Err(VerificationError::Invalid); }
    if sources.connector_catalog.is_none() { return Err(VerificationError::Unavailable); }
    DISPATCHER.get().ok_or(VerificationError::Unavailable)?.verify_producers(
        sources.execution_plans.as_deref().ok_or(VerificationError::Unavailable)?,
        sources.worker_plans.as_deref().ok_or(VerificationError::Unavailable)?,
        sources.connectors.as_deref().ok_or(VerificationError::Unavailable)?,
        sources.connector_selection.as_deref().ok_or(VerificationError::Unavailable)?)?;
    let bindings = sources.bindings.take().ok_or(VerificationError::Unavailable)?;
    let run_policy = sources.authority.take().ok_or(VerificationError::Unavailable)?;
    let live: Arc<dyn LiveAuthority> = if sources.product_authority.is_some() { run_policy }
        else { Arc::new(duduclaw_mcp_boundary::bindings::BindingCheckedAuthority { run_policy, bindings: bindings.clone() }) };
    let boundary = McpBoundaryService::start(live)?;
    let worker = crate::isolation_execution::install_worker_services(
        sources.worker_launcher.take().ok_or(VerificationError::Unavailable)?)?;
    // F1: the stack must read the very catalog handle the authority reads.
    if let Some(stack) = &sources.connector_stack {
        if sources.connector_catalog.as_ref().is_none_or(|catalog| !Arc::ptr_eq(catalog, stack.catalog())) {
            return Err(VerificationError::Unavailable);
        }
        if let Some(inputs) = sources.connector_inputs.take() { install_stack_factory(home, stack, inputs, &worker); }
    }
    let execution_jobs = duduclaw_mcp_boundary::execution_jobs::SqliteExecutionJobs::open(&home.join("isolation-execution-jobs.db"))?;
    let recording_jobs = duduclaw_mcp_boundary::recording_jobs::SqliteRecordingJobs::open(&home.join("isolation-execution-jobs.db"))?;
    let fork_tools = match sources.fork_tools.take() {
        Some(service) => service,
        None => Arc::new(duduclaw_mcp_boundary::forks::ForkHostControl {
            store: duduclaw_mcp_boundary::forks::ForkStore::open(&home.join("isolation-forks.db"))?,
            backend: backend.clone(),
        }),
    };
    Ok(BootOwner { generation: sources.installed_generation.ok_or(VerificationError::Unavailable)?, fork_tools, execution_jobs, recording_jobs, product_authority: sources.product_authority, home: home.to_owned(), backend: backend.clone(), boundary, bindings, worker,
        registry: sources.runtime_registry.take().ok_or(VerificationError::Unavailable)?,
        main_plans: sources.main_plans.take().ok_or(VerificationError::Unavailable)?,
        execution_plans: sources.execution_plans.take().ok_or(VerificationError::Unavailable)?,
        worker_plans: sources.worker_plans.take().ok_or(VerificationError::Unavailable)?,
        turns: sources.turns.take().ok_or(VerificationError::Unavailable)?,
        // W1: run-bound work brokers (unless a host broker is supplied) + HX5 feature view.
        selectors: SelectorSources::installed(home, sources.work_broker.take(), backend),
        catalog: sources.connector_catalog, connectors: sources.connectors,
        connector_selection: sources.connector_selection, connector_stack: sources.connector_stack })
}

pub(super) async fn claude_plan(home: PathBuf, agent: String, class: duduclaw_isolation::execution::ExecutionClass,
    prompt: Vec<u8>, system_prompt: Vec<u8>, model: String) -> Result<SpawnPolicy, VerificationError> {
    crate::isolation_readiness::check_execution().map_err(|_| VerificationError::Unavailable)?;
    let owner = OWNER.get().cloned().ok_or(VerificationError::Unavailable)?;
    if owner.home != home { return Err(VerificationError::BindingMismatch); }
    duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let employee = crate::handlers::resolve_confirmed_employee(&owner.home, &agent)
            .map_err(|_| VerificationError::Unavailable)?;
        owner.main_plans.claude(employee.employee, class, prompt, system_prompt, &model)
            .map_err(|_| VerificationError::Unavailable)
    }).await
}

/// Derive only from the fresh root publication and the confirmed employee.
pub(super) async fn other_runtime_plan(home: PathBuf, agent: String,
    class: duduclaw_isolation::execution::ExecutionClass,
    runtime: duduclaw_isolation::execution::RuntimeKind, request: Vec<u8>) -> Result<SpawnPolicy, VerificationError> {
    crate::isolation_readiness::check_execution().map_err(|_| VerificationError::Unavailable)?;
    let owner = OWNER.get().cloned().ok_or(VerificationError::Unavailable)?;
    if owner.home != home { return Err(VerificationError::BindingMismatch); }
    duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let employee = crate::handlers::resolve_confirmed_employee(&owner.home, &agent)
            .map_err(|_| VerificationError::Unavailable)?;
        owner.main_plans.other_runtime(employee.employee, class, runtime, request)
            .map_err(|_| VerificationError::Unavailable)
    }).await
}

pub(super) async fn runtime_available(runtime: duduclaw_isolation::execution::RuntimeKind) -> bool {
    let Some(owner) = OWNER.get().cloned() else { return false; };
    if crate::isolation_readiness::check_execution().is_err() { return false; }
    duduclaw_mcp_boundary::authority::verify_blocking(move || owner.registry.registration(&runtime))
        .await.is_ok()
}

/// Trusted runtime entry points supply host-selected policy and resource roots.
/// Native command arguments and environment never become launch authority.
pub async fn prepare_launch(mut input: LaunchInput) -> Result<Arc<dyn duduclaw_execution::command::ProtectedLaunch>, VerificationError> {
    crate::isolation_readiness::check_execution().map_err(|_| VerificationError::Unavailable)?;
    let owner = OWNER.get().cloned().ok_or(VerificationError::Unavailable)?;
    if !owner.boundary.healthy() { return Err(VerificationError::Unavailable); }
    let checking_owner = owner.clone();
    let (input, prepared) = duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let mapping = crate::handlers::resolve_confirmed_employee(&checking_owner.home, &input.agent)
            .map_err(|_| VerificationError::Unavailable)?;
        if mapping.employee != input.plan.employee { return Err(VerificationError::BindingMismatch); }
        // F1: a changed binding/employee forgets the connector build (rebuilt at first use).
        if let Some(stack) = &checking_owner.connector_stack { stack.observe_lifecycle(&input.agent); }
        let prepared = match &checking_owner.product_authority {
            Some(authority) => {
                let prepared = authority.prepare(&input.agent, &mut input.grant, input.authenticated_epoch)?;
                if prepared.employee() != input.plan.employee { return Err(VerificationError::BindingMismatch); }
                Some(match input.turn.take() { Some(turn) => prepared.with_turn(turn), None => prepared })
            }
            None => None,
        };
        checking_owner.bindings.validate(input.plan.employee, &input.grant.binding, &input.grant.principal)
            .map_err(|_| VerificationError::BindingMismatch)?;
        Ok((input, prepared))
    }).await?;
    let mut resolver = GatewayResourceResolver::new(owner.home.clone(), input.plan.employee, input.agent.clone(), owner.bindings.clone())?;
    if let Some(catalog) = &owner.catalog { resolver = resolver.with_connector_catalog(catalog.clone()); }
    // F1: this employee's providers (rebuilt on route/binding change); the
    // boot singletons are inventory objects and never serve a call.
    let (selection, connectors) = match &owner.connector_stack {
        Some(stack) => {
            let handle = Arc::new(stack.for_employee(&input.agent, input.plan.employee)?);
            (Some(handle.clone() as Arc<dyn LiveConnectorSelectionPolicy>), Some(handle as Arc<dyn BoundConnectorExecutor>))
        }
        None => (owner.connector_selection.clone(), owner.connectors.clone()),
    };
    if let Some(selection) = selection { resolver = resolver.with_connector_selection_policy(selection); }
    let resolver = Arc::new(resolver);
    #[cfg(feature = "isolation-worker-integration")]
    {
        let defaults = crate::isolation_worker_plans::GatewayWorkerPlans::for_employee(
            owner.home.clone(), input.agent.clone(), input.plan.employee)?.with_roots(input.roots.clone());
        let worker_plans = Arc::new(duduclaw_mcp_boundary::worker_execution::CompositeWorkerPlans::new(
            vec![owner.worker_plans.clone(), Arc::new(defaults)])?);
        let factories = GatewayFactories::assemble(Some(owner.backend.clone()), Some(owner.execution_plans.clone()),
            resolver.clone(), Some(owner.worker.parser.clone()), input.roots.clone())?
            .with_execution_jobs(owner.execution_jobs.clone())
            .with_recording_jobs(owner.recording_jobs.clone())
            .with_recording_audit(Arc::new(recording::GatewayRecordingAudit::new(owner.home.clone())))
            .with_forks(owner.fork_tools.clone())
            .with_worker(owner.worker.tools(worker_plans, resolver.clone()));
        let dispatcher_input = DispatcherInput {
            home: owner.home.clone(), employee: input.plan.employee, agent: input.agent,
            roots: input.roots, resolver, factories, turns: owner.turns.clone(), connectors,
        };
        let dispatcher = duduclaw_mcp_boundary::authority::verify_blocking(move ||
            DISPATCHER.get().ok_or(VerificationError::Unavailable)?.create(dispatcher_input)).await?;
        let launch = ProtectedMcpLaunch::new(owner.backend.clone(), owner.boundary.boundary().clone(),
            owner.registry.clone(), dispatcher, input.plan, input.grant, input.origin, input.caller)
            .map_err(|_| VerificationError::Unavailable)?;
        return match (&owner.product_authority, prepared) {
            (Some(authority), Some(prepared)) => Ok(Arc::new(super::launch::AuthorizedLaunch::new(
                launch, authority.clone(), prepared))),
            (None, None) => Ok(launch),
            _ => Err(VerificationError::Unavailable),
        };
    }
    #[cfg(not(feature = "isolation-worker-integration"))]
    { let _ = connectors; Err(VerificationError::Unavailable) }
}

#[cfg(test)]
#[path = "authorization_tests.rs"]
mod authorization_tests;

#[cfg(test)]
#[path = "boot_personal_tests.rs"]
mod boot_personal_tests;

#[cfg(test)]
#[path = "selectors_tests.rs"]
mod selectors_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolation_boot_missing_authority_never_reports_constructed() {
        let absent = missing(&BootSources::default(), true, true, true);
        for producer in [MissingProducer::LiveAuthority, MissingProducer::DurableBindings,
            MissingProducer::RuntimePublication, MissingProducer::MainPlans, MissingProducer::ExecutionPlans, MissingProducer::WorkerRetirement,
            MissingProducer::WorkerPlans, MissingProducer::TurnAuthority,
            MissingProducer::ConnectorProviders] { assert!(absent.contains(&producer)); }
        assert!(!absent.contains(&MissingProducer::Dispatcher));
    }
    #[test]
    fn isolation_boot_missing_backend_and_binary_adapter_are_independent() {
        let absent = missing(&BootSources::default(), false, false, false);
        assert!(absent.contains(&MissingProducer::Backend));
        assert!(absent.contains(&MissingProducer::Dispatcher));
        assert!(absent.contains(&MissingProducer::WorkerIntegration));
    }
    #[test]
    fn isolation_boot_requires_catalog_execution_and_live_selection_together() {
        let registry = Arc::new(crate::isolation_connector_registry::ConnectorProviderRegistry::default());
        let mut sources = BootSources::default();
        sources.connector_catalog = Some(Arc::new(ConnectorCatalog::new(
            rusqlite::Connection::open_in_memory().unwrap()).unwrap()));
        sources.connectors = Some(registry.clone());
        assert!(missing(&sources, true, true, true).contains(&MissingProducer::ConnectorProviders));
        sources.connector_selection = Some(registry);
        // Presence is only the first gate; assemble separately checks every tool.
        assert!(!missing(&sources, true, true, true).contains(&MissingProducer::ConnectorProviders));
        sources.connectors = None;
        assert!(missing(&sources, true, true, true).contains(&MissingProducer::ConnectorProviders));
    }
}

/// HX6 Admin recording control and the recording audit sink (security fix
/// round, finding 6); split out to keep this file below 800 lines.
#[path = "boot_recording.rs"]
mod recording;
pub(crate) use recording::control_recording;

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
#[path = "test_support.rs"]
pub mod test_support;
