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
use super::refusal::{site, RefusalSite, Sited};

/// CLI supplies its adapter before start_gateway. The gateway owns every input;
/// the adapter must authenticate its original dispatcher and check completeness.
pub trait DispatcherFactory: Send + Sync {
    fn create(&self, input: DispatcherInput) -> Result<Arc<dyn Dispatcher>, VerificationError>;
    fn verify_producers(&self, _execution: &dyn ExecutionPlanFactory, _worker: &dyn WorkerPlanFactory,
        _connectors: &dyn BoundConnectorExecutor, _selection: &dyn LiveConnectorSelectionPolicy)
        -> Result<(), VerificationError> { Err(VerificationError::Unavailable) }
    /// R8n-2 B4: diagnostics only, never a readiness decision (that is
    /// `verify_producers`, which fails closed by default): the first tool, by
    /// its fixed tool-table name, that `verify_producers` finds unprovided.
    fn unprovided_tool(&self, _execution: &dyn ExecutionPlanFactory, _worker: &dyn WorkerPlanFactory,
        _connectors: &dyn BoundConnectorExecutor, _selection: &dyn LiveConnectorSelectionPolicy)
        -> Option<&'static str> { None }
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
    /// H9/H10: test-only override of the root work broker (a double). It does
    /// not exist in production builds: there W1's `assemble` always installs
    /// run-bound brokers over the protected backend (`run_broker.rs`, H5
    /// `SpawndWorkspaceBridge`). Without them the issuer never selects a
    /// Workspace grant (tools hidden, fail closed); it is not a
    /// boot-completeness producer.
    #[cfg(any(test, feature = "test-support"))]
    pub work_broker: Option<Arc<dyn duduclaw_mcp_boundary::resources::WorkBroker>>,
    /// F1: per-employee connector stack over `connector_catalog`. When boot
    /// creates it, `connectors`/`connector_selection` are this same object
    /// (inventory only) and each launch gets an employee-bound handle.
    connector_stack: Option<Arc<EmployeeConnectorProviders>>,
    connector_inputs: Option<ConnectorBootInputs>,
    /// R8j round 3 F3: the hardware detection for the connector factory, run
    /// before an attempt's shutdown-gate permit (bounded child processes, no
    /// process-wide state) and kept across attempts until the stack takes it.
    hardware: Option<duduclaw_inference::types::HardwareInfo>,
}

/// Gateway-side inputs of the production connector factory, collected in
/// `boot_protected` and consumed by `assemble` (after the worker exists).
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
impl MissingProducer {
    pub const ALL: [Self; 13] = [Self::Backend, Self::LiveAuthority, Self::DurableBindings, Self::RuntimePublication,
        Self::ExecutionPlans, Self::WorkerRetirement, Self::WorkerPlans, Self::TurnAuthority, Self::Dispatcher,
        Self::WorkerIntegration, Self::MainPlans, Self::ConnectorProviders, Self::Initialization];
    /// R8m 30b: the fixed name (the serde name) published to the root CLI.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Backend => "backend",
            Self::LiveAuthority => "live_authority",
            Self::DurableBindings => "durable_bindings",
            Self::RuntimePublication => "runtime_publication",
            Self::ExecutionPlans => "execution_plans",
            Self::WorkerRetirement => "worker_retirement",
            Self::WorkerPlans => "worker_plans",
            Self::TurnAuthority => "turn_authority",
            Self::Dispatcher => "dispatcher",
            Self::WorkerIntegration => "worker_integration",
            Self::MainPlans => "main_plans",
            Self::ConnectorProviders => "connector_providers",
            Self::Initialization => "initialization",
        }
    }
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
/// DEFECT 26: replaced by every boot attempt until the first success; never a
/// latched failure (it was a `OnceLock` holding the first attempt's reasons).
static STATE: retry::BootState = retry::BootState::new();
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
/// The inputs `fill_installed_sources` derived in this attempt (from the
/// updater's generation and spawnd's projection). A failed attempt forgets
/// them, so no later attempt reuses a stale projection; host-installed inputs
/// are kept.
#[derive(Default)]
struct Derived { worker_plans: bool, execution_plans: bool, main_plans: bool, runtime_registry: bool, worker_launcher: bool }
impl Derived {
    fn forget(self, sources: &mut BootSources) {
        sources.installed_generation = None;
        if self.worker_plans { sources.worker_plans = None; }
        if self.execution_plans { sources.execution_plans = None; }
        if self.main_plans { sources.main_plans = None; }
        if self.runtime_registry { sources.runtime_registry = None; }
        if self.worker_launcher { sources.worker_launcher = None; }
    }
}

/// R8m 30a: re-run by every attempt (nothing here is latched); an
/// `ExecutionPlans` refusal adds its fixed reason to `reasons`.
fn fill_installed_sources(sources: &mut BootSources, home: &Path, reasons: &mut Vec<retry::Detail>) -> Derived {
    let mut derived = Derived::default();
    let generation = crate::isolation_readiness::verified_generation();
    sources.installed_generation = generation;
    if sources.worker_plans.is_none() {
        if let Ok(factory) = crate::isolation_worker_plans::GatewayWorkerPlans::new(home.to_owned()) {
            sources.worker_plans = Some(Arc::new(factory));
            derived.worker_plans = true;
        }
    }
    if sources.execution_plans.is_none() {
        if let Some(generation) = generation {
            match crate::isolation_execution::InstalledProgramPlans::new_checked(
                generation, nix::unistd::getegid().as_raw()) {
                Ok(factory) => {
                    sources.execution_plans = Some(factory);
                    derived.execution_plans = true;
                }
                Err(failure) => reasons.push((MissingProducer::ExecutionPlans, failure.reason())),
            }
        }
    }
    if sources.main_plans.is_none() {
        if let Some(generation) = generation {
            if let Ok(factory) = duduclaw_spawnd::gateway_plan::InstalledMainPlanFactory::new(
                generation, nix::unistd::getegid().as_raw()) {
                sources.main_plans = Some(Arc::new(factory));
                derived.main_plans = true;
            }
        }
    }
    if sources.runtime_registry.is_none() {
        if let Some(generation) = generation {
            if let Ok(registry) = duduclaw_spawnd::gateway_mcp::InstalledRuntimeRegistry::new(
                generation, nix::unistd::getegid().as_raw()) {
                sources.runtime_registry = Some(Arc::new(registry));
                derived.runtime_registry = true;
            }
        }
    }
    if sources.worker_launcher.is_none() {
        // The launcher acquires its durable host slot before connecting and
        // requires exact worker identity plus proven retirement. Construction
        // alone is deliberately not composite readiness evidence.
        sources.worker_launcher = Some(Box::new(duduclaw_parse_worker::retirement::RetiringLauncher));
        derived.worker_launcher = true;
    }
    derived
}

/// Producers that cannot appear later in this process: the dispatcher
/// adapter is refused after the first attempt. Retrying cannot succeed; the
/// reasons stay published.
fn structural(missing: &[MissingProducer]) -> bool {
    missing.iter().any(|producer| matches!(producer, MissingProducer::Dispatcher))
}

fn missing(sources: &BootSources, dispatcher: bool, backend: bool) -> Vec<MissingProducer> {
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
        (dispatcher, MissingProducer::Dispatcher)] {
        if !present { absent.push(producer); }
    }
    absent
}

/// Called once after protected TEAM mode and A's backend have been installed.
/// No employee work runs here; the existing composite gate stays authoritative.
/// DEFECT 26: while trusted inputs are missing (the updater's generation,
/// spawnd's projection, ...) the same all-or-nothing attempt is retried in the
/// background until it succeeds once. No restart is needed after install, boot
/// or update. Review round 1: the first attempt runs in that background task
/// too (it waits up to 5 s for the updater's generation, which the updater
/// gives only after its verification of this gateway's read-back), so it never
/// delays the gateway's own start-up and its local readiness latch.
/// `ATTEMPTED` is still decided here, synchronously.
pub async fn boot_protected(home: &Path, users: Arc<duduclaw_auth::UserDb>, agents: Arc<super::AgentAdmission>) {
    if crate::isolation_readiness::status().is_none() || ATTEMPTED.swap(true, Ordering::AcqRel) { return; }
    let started = std::time::Instant::now();
    let sources = SOURCES.lock().ok().and_then(|mut slot| slot.take()).unwrap_or_default();
    spawn_boot(InstalledBoot { home: home.to_owned(), users, agents, sources }, started);
}

/// DEFECT 26: the boot attempt and its background retry (2 s for the first
/// minute, then 10 s; each attempt bounded by its own waits). Ends at the
/// first success, when a missing producer can no longer appear, or once the
/// gateway's shutdown gate (`handlers::migration_shutdown_gate`) has closed.
/// R8j round 2 B3: the gate is peeked before the first attempt as well
/// (`retry::run`) and during its generation wait; R8j round 3 F3: every
/// process-wide side effect of an attempt and its publication run inside one
/// permit of that gate (`attempt_installed`, `retry::under_permit`). Starts
/// one tokio task.
fn spawn_boot(mut boot: InstalledBoot, started: std::time::Instant) {
    drop(tokio::spawn(async move {
        retry::run(&mut boot, &STATE, started, retry::SCHEDULE, shutdown_begun).await;
    }));
}

/// Review round 1: the spawnd backend connection retried after a failed
/// inline attempt at boot (`isolation_readiness::connect_backend`), so a
/// gateway started before spawnd's control socket is reachable converges
/// without a restart. Never replaces an installed backend; ends at the first
/// success (which marks the local readiness step `Backend`) or at shutdown.
/// The boot assembly above picks the backend up at its next attempt.
pub(crate) fn spawn_backend_retry() {
    let started = std::time::Instant::now();
    drop(tokio::spawn(async move {
        if retry::retry_backend(crate::isolation_readiness::connect_backend_once, &crate::update_readback::LOCAL,
            started, retry::BACKEND_SCHEDULE, shutdown_begun).await {
            tracing::info!(elapsed_secs = started.elapsed().as_secs(), "受保護團隊已連接隔離執行服務。");
        }
    }));
}

fn shutdown_begun() -> bool {
    use duduclaw_migration::confirmation::EffectGate;
    crate::handlers::migration_shutdown_gate().is_closed()
}

/// The production boot attempt. Host-installed sources and the once-per-process
/// authority / connector stack are kept across attempts; everything derived
/// from the updater's generation or spawnd's projection is re-read every time.
struct InstalledBoot {
    home: PathBuf,
    users: Arc<duduclaw_auth::UserDb>,
    agents: Arc<super::AgentAdmission>,
    sources: BootSources,
}
impl retry::BootAttempt for InstalledBoot {
    fn attempt(&mut self) -> retry::AttemptFuture<'_> { Box::pin(attempt_installed(self)) }
    /// R8d D3: once per process, after the first successful boot (it needs the
    /// spawnd connection), off the boot path (bounded retries until the
    /// isolation service answers): every confirmed live employee's root
    /// entitlement is re-sent from its approved settings.
    fn assembled(&mut self) { crate::handlers::spawn_entitlement_reconcile(self.home.clone()); }
}

async fn attempt_installed(boot: &mut InstalledBoot) -> Result<(), retry::Failure> {
    let InstalledBoot { home, users, agents, sources } = boot;
    let home = home.as_path();
    // F's first asynchronous probe may still be in flight during startup. Wait
    // a bounded interval before consuming the root report; never mark it ready
    // merely because the unit/socket exists. A shutdown ends the wait (B3).
    let (tries, interval) = retry::GENERATION_WAIT;
    retry::wait_for_generation(|| crate::isolation_readiness::verified_generation().is_some(),
        &shutdown_begun, tries, interval).await?;
    let mut reasons = Vec::new();
    let derived = fill_installed_sources(sources, home, &mut reasons);
    let backend = duduclaw_execution::selection::protected_backend_for_boot();
    // Reuse the gateway's initialized users.db and confirmed employee registry.
    // No parallel principal/binding database is created for the boundary.
    if sources.authority.is_none() && sources.bindings.is_none() {
        // C2: the authority checks connector uses against the production
        // catalog, and must read the very instance selection pins against.
        // R8m 30a: opened BEFORE the authority, on every attempt while it is
        // absent; the authority is never built without it (it was built once
        // with `None` and kept, so one failed open latched
        // `ConnectorProviders` missing for the life of the process).
        match catalog_for_authority(&mut sources.connector_catalog, || ConnectorCatalog::open_checked(home)) {
            Err(failure) => reasons.push((MissingProducer::ConnectorProviders, failure.reason())),
            Ok(catalog) => if let Ok(backend) = &backend {
                if let Ok(authority) = super::authority::GatewayAuthority::new(home.to_owned(), users.clone(), backend.clone(),
                    agents.clone(), Some(catalog)) {
                    let authority = Arc::new(authority);
                    sources.authority = Some(authority.clone());
                    sources.bindings = Some(authority.clone());
                    if sources.turns.is_none() { sources.turns = Some(authority.clone()); }
                    sources.product_authority = Some(authority);
                }
            },
        }
    }
    // R8j round 3 F3: the bounded hardware probe (≤ 2 × (5 s + 1 s)) has no
    // process-wide side effect: it runs here, before the permit, so the
    // permit covers only fast local construction. Once per process at most
    // (kept in `sources` until the connector stack takes it).
    if sources.connectors.is_none() && sources.connector_selection.is_none()
        && connector_stack_inputs(sources) && sources.hardware.is_none() {
        sources.hardware = Some(duduclaw_inference::hardware::detect_hardware().await);
    }
    // R8j round 3 F3: one shutdown-gate permit from here (before the first
    // process-wide side effect, the credential vault in
    // `install_connector_stack`) until the publication is complete.
    let users = users.clone();
    retry::under_permit(crate::handlers::migration_shutdown_gate(),
        || install_and_publish(home, users, sources, derived, backend.ok(), reasons))
}

/// R8m 30a: the catalog the product authority is built over. Opened (or
/// created) by `open` on every call while `slot` is empty and kept once
/// opened; a failure leaves `slot` empty (the caller reports
/// `ConnectorProviders(<reason>)` and builds no authority) so the next
/// attempt opens again.
fn catalog_for_authority(slot: &mut Option<Arc<ConnectorCatalog>>,
    open: impl FnOnce() -> Result<ConnectorCatalog, crate::isolation_connectors::CatalogOpenFailure>)
    -> Result<Arc<ConnectorCatalog>, crate::isolation_connectors::CatalogOpenFailure> {
    if let Some(catalog) = slot { return Ok(catalog.clone()); }
    let catalog = Arc::new(open()?);
    *slot = Some(catalog.clone());
    Ok(catalog)
}

/// R8j round 3 F3: every process-wide side effect of one attempt and its
/// publication (`OWNER`, `STATE`, the integration probe). Called only inside
/// the attempt's shutdown-gate permit (`retry::under_permit`). Synchronous
/// local construction only: no network or child-process wait.
fn install_and_publish(home: &Path, users: Arc<duduclaw_auth::UserDb>, sources: &mut BootSources, derived: Derived,
    backend: Option<Arc<duduclaw_execution::SpawndBackend>>, mut reasons: Vec<retry::Detail>) -> Result<(), retry::Failure> {
    // F1: the per-employee connector stack, over the very catalog handle the
    // authority above checks connector uses against. Only when the host did
    // not supply its own singletons; a missing catalog leaves the producer
    // missing (boot stays paused, every connector use Unavailable). Built at
    // most once: a later attempt finds it in `sources`.
    if sources.connectors.is_none() && sources.connector_selection.is_none() {
        if let Some(reason) = install_connector_stack(sources, home, users) {
            reasons.push((MissingProducer::ConnectorProviders, reason));
        }
    }
    let absent = missing(sources, DISPATCHER.get().is_some(), backend.is_some());
    if !absent.is_empty() {
        derived.forget(sources);
        // Only reasons of producers that are actually missing are reported.
        reasons.retain(|(producer, _)| absent.contains(producer));
        return Err(retry::Failure { retry: !structural(&absent), missing: absent, reasons });
    }
    // Side-effect-free checks of `assemble` first: a failure here is
    // retried. `assemble` itself installs process-wide producers, so a
    // failure there stays final (as before).
    if let Err(reasons) = precheck(home, sources) {
        derived.forget(sources);
        return Err(retry::Failure { missing: vec![MissingProducer::Initialization], retry: true, reasons });
    }
    let final_failure = || retry::Failure { missing: vec![MissingProducer::Initialization], retry: false, reasons: Vec::new() };
    let owner = assemble(home, backend.ok_or_else(final_failure)?, std::mem::take(sources))
        .map_err(|_| final_failure())?;
    let generation = owner.generation;
    OWNER.set(Arc::new(owner)).map_err(|_| final_failure())?;
    STATE.constructed();
    install_probes(crate::isolation_readiness::install_integration_probe);
    // R8n-1 review F2: the probes read only what this one observer stores.
    probes::start_observer(generation, shutdown_begun);
    Ok(())
}

/// The three integration probes of the assembled owner (R8n-1: ParseWorker
/// and RuntimeAdapters beside the McpBoundary one). A probe that cannot be
/// registered leaves its component `PendingIntegration` (never ready).
fn install_probes(mut install: impl FnMut(crate::isolation_readiness::IntegrationComponent,
    Arc<dyn crate::isolation_readiness::IntegrationProbe>) -> Result<(), crate::isolation_readiness::ProbeError>) {
    use crate::isolation_readiness::IntegrationComponent;
    let probes: [(IntegrationComponent, Arc<dyn crate::isolation_readiness::IntegrationProbe>); 3] = [
        (IntegrationComponent::McpBoundary, Arc::new(BoundaryReadiness)),
        (IntegrationComponent::ParseWorker, Arc::new(probes::ParseWorkerReadiness::default())),
        (IntegrationComponent::RuntimeAdapters, Arc::new(probes::RuntimeAdapterReadiness::default())),
    ];
    for (component, probe) in probes {
        if install(component, probe).is_err() {
            tracing::error!(?component, "隔離服務的狀態檢查無法註冊，團隊執行維持暫停。");
        }
    }
}

/// The inputs [`install_connector_stack`] needs before it builds anything.
fn connector_stack_inputs(sources: &BootSources) -> bool {
    sources.connector_catalog.is_some() && sources.bindings.is_some() && sources.turns.is_some()
        && sources.product_authority.is_some()
}

/// R8n-2 B4: the fixed reason of a failed boot-time producer verification.
const PRODUCER_VERIFICATION: &str = "producer_verification";

/// The checks `assemble` makes before its first process-wide installation.
/// A refusal answers its fixed `Initialization` reasons (possibly none).
fn precheck(home: &Path, sources: &BootSources) -> Result<(), Vec<retry::Detail>> {
    if !home.is_absolute() || sources.connector_catalog.is_none() || sources.installed_generation.is_none() {
        return Err(Vec::new());
    }
    let (Some(dispatcher), Some(execution), Some(worker), Some(connectors), Some(selection)) = (DISPATCHER.get(),
        sources.execution_plans.as_deref(), sources.worker_plans.as_deref(), sources.connectors.as_deref(),
        sources.connector_selection.as_deref()) else { return Err(Vec::new()) };
    verify_producers_with_reason(&**dispatcher, execution, worker, connectors, selection)
}

/// `verify_producers` decides; only on its refusal is `unprovided_tool` asked,
/// for the log line. The reasons are `Initialization(producer_verification)`
/// plus, when the adapter names one, the first unprovided tool's fixed name
/// (`Initialization(producer_verification: skill_extract)` in the log). The
/// published view carries producer names only (`initialization`).
fn verify_producers_with_reason(dispatcher: &dyn DispatcherFactory, execution: &dyn ExecutionPlanFactory,
    worker: &dyn WorkerPlanFactory, connectors: &dyn BoundConnectorExecutor,
    selection: &dyn LiveConnectorSelectionPolicy) -> Result<(), Vec<retry::Detail>> {
    dispatcher.verify_producers(execution, worker, connectors, selection).map_err(|_| {
        let mut reasons = vec![(MissingProducer::Initialization, PRODUCER_VERIFICATION)];
        if let Some(tool) = dispatcher.unprovided_tool(execution, worker, connectors, selection) {
            reasons.push((MissingProducer::Initialization, tool));
        }
        reasons
    })
}

/// F1 step 1 (before the producer check): the per-employee stack over the
/// boot catalog and the process credential vault, installed into the
/// connector lifecycle so C3 revoke / C4 events drop exactly the material the
/// executors read. Any failure leaves the connector producer missing.
/// R8j round 3 F3: runs inside the attempt's shutdown-gate permit; the
/// hardware detection it needs ran before the permit (`sources.hardware`,
/// none ⇒ nothing is built).
/// R8m 30a: answers the fixed reason when it could have built the stack but
/// a step refused (nothing is kept; the next attempt builds again), `None`
/// when it built the stack or an input belonging to another producer is absent.
fn install_connector_stack(sources: &mut BootSources, home: &Path, users: Arc<duduclaw_auth::UserDb>)
    -> Option<&'static str> {
    use crate::isolation_connector_stack::{LifecycleBindings, installed_home_binding, readiness_report};
    let (Some(catalog), Some(bindings), Some(turns)) =
        (sources.connector_catalog.clone(), sources.bindings.clone(), sources.turns.clone()) else { return None };
    if sources.product_authority.is_none() || sources.hardware.is_none() { return None; }
    // R8m 30a: an unreadable users.db (or no installation tenant yet) was
    // kept as "no home binding" for the life of the process (the stack is
    // built once). Now the stack waits for the next attempt instead.
    let Ok(home_binding) = installed_home_binding(home).map(Some) else { return Some("home_binding_unavailable") };
    let Ok(lifecycle) = LifecycleBindings::new(home.to_owned()) else { return Some("lifecycle_unavailable") };
    let Ok(stack) = EmployeeConnectorProviders::new(home.to_owned(), catalog, bindings, home_binding.clone(),
        Arc::new(lifecycle), readiness_report()) else { return Some("connector_stack_unavailable") };
    let stack = Arc::new(stack);
    let vault = Arc::new(crate::isolation_connector_clients::AccountCredentialVault::default());
    // The vault the executors use must be the one revoke drops from.
    if crate::isolation_connector_lifecycle::install_credential_material(vault.clone()).is_err() {
        tracing::error!("隔離連接器的憑證保管區已被其他元件註冊，連接器維持停用。");
        return Some("credential_vault_unavailable");
    }
    let weak = Arc::downgrade(&stack);
    if crate::isolation_connector_lifecycle::install_route_change_hook(Arc::new(move ||
        if let Some(stack) = weak.upgrade() { stack.schedule_refresh(); })).is_err() {
        tracing::warn!("隔離連接器的路由變更通知無法註冊；連接器仍會在每次使用時重新檢查。");
    }
    let hardware = sources.hardware.take()?;
    sources.connectors = Some(stack.clone());
    sources.connector_selection = Some(stack.clone());
    sources.connector_stack = Some(stack);
    sources.connector_inputs = Some(ConnectorBootInputs { users, turns, home_binding, hardware, vault });
    None
}

/// R8m 30a: every fixed reason the boot assembly can attach to a missing
/// producer (the log line's `Producer(<reason>)`).
#[cfg(test)]
fn fixed_reasons() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = crate::isolation_connectors::CatalogOpenFailure::ALL.iter()
        .map(|failure| failure.reason()).collect();
    all.extend(crate::isolation_execution::PlansFailure::ALL.iter().map(|failure| failure.reason()));
    all.extend(["home_binding_unavailable", "lifecycle_unavailable", "connector_stack_unavailable",
        "credential_vault_unavailable", PRODUCER_VERIFICATION]);
    all
}

/// F1 step 2 (in `assemble`, after the worker exists): install the factory —
/// the host override or `GatewayConnectorFactory` with D3's CLI executor
/// factories, D2 native home providers, the E2/E4 Google bridge and the boot
/// vault — then build every confirmed employee once (reported to F3). A
/// factory that cannot be assembled leaves every connector tool refusing and
/// every employee `connector_setup_required`; boot itself continues.
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

/// Re-evaluated on every read: the latest attempt's reasons until the first
/// success, then the live owner's health.
pub fn boot_status() -> BootStatus {
    effective_status(STATE.status(), OWNER.get().is_some_and(|owner| owner.boundary.healthy()))
}

fn effective_status(status: Option<BootStatus>, owner_healthy: bool) -> BootStatus {
    let mut status = status.unwrap_or(BootStatus { constructed: false,
        missing: vec![MissingProducer::Initialization] });
    if status.constructed && !owner_healthy {
        status.constructed = false;
        status.missing = vec![MissingProducer::Initialization];
    }
    status
}

/// A's v1.80 producer hook excludes exactly the documented fork mutations.
/// Recheck the installed helpers on every observation; source presence alone
/// does not override missing/replaced signed runtime publications.
/// R8n-1: only the helper families this host offers (fixed at assembly from
/// the signed policy) need their producer; the three fork readers always do.
/// With no helper offered this is the healthy owner plus the fork readers;
/// the main runtimes are attested by the RuntimeAdapters probe (`probes`).
pub fn a_producers_ready() -> bool {
    OWNER.get().is_some_and(|owner| owner.boundary.healthy()
        && duduclaw_mcp_boundary::fork_policy::remaining_producers_ready(
            |name| owner.execution_plans.offers(name),
            |name| owner.execution_plans.supports(name),
            |name| owner.fork_tools.supports(name)))
}

/// R8n-1 1.6: the fixed names of the helper families the assembled tool
/// service does not offer; empty until the assembly succeeded.
pub fn tools_unavailable() -> Vec<&'static str> {
    OWNER.get().map(|owner| crate::isolation_execution::HelperFamilies::from_offers(
        |name| owner.execution_plans.offers(name)).unavailable()).unwrap_or_default()
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
        if owner.product_authority.is_none() {
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

/// `step!(<label literal>)`: [`issue_step`] with a compile-time checked site.
macro_rules! step {
    ($label:literal) => { issue_step(site!($label)) };
}

/// Authenticated ingress and approved background records share this issuer.
/// The wrapper owns live store reads. Additional IO grants come only from the
/// H9 selectors (`selectors.rs`), which read their inputs at issuance from the
/// authoritative stores; `task` is the host-pinned record task of a
/// task/schedule run (`None` for interactive ingress), never a tool argument.
pub(crate) fn record_authorization(home: PathBuf, agent: String, user: String, epoch: u64,
    lane: duduclaw_isolation::capability::Lane, turn: super::HostTurn, credential_expiry: Option<u64>,
    task: Option<String>) -> Result<super::RuntimeAuthorization, VerificationError> {
    // R8x: each step names itself (fixed label) on this blocking thread; an
    // ingress caller reads the last one reached when issuing fails.
    step!("authorization.issue.boot_owner");
    let owner = OWNER.get().ok_or(VerificationError::Unavailable)?;
    if owner.home != home || !owner.boundary.healthy() { return Err(VerificationError::Unavailable); }
    step!("authorization.issue.product_authority");
    let authority = owner.product_authority.as_ref().ok_or(VerificationError::Unavailable)?;
    step!("authorization.issue.lifecycle");
    let lifecycle = lifecycle::read_lifecycle(&home, &agent).map_err(lifecycle_refusal)?;
    step!("authorization.issue.snapshot");
    let current = authority.ingress_snapshot(&user, &agent)?;
    step!("authorization.issue.epoch");
    super::ingress::require_epoch(epoch, current.human.as_ref().map(|human| human.credential_epoch))?;
    step!("authorization.issue.principal");
    let principal = issuance_principal(&user, &current)?;
    step!("authorization.issue.binding");
    issuance_facts(&agent, &lifecycle, &principal, &current, turn.background.as_ref())?;
    step!("authorization.issue.policy_revision");
    let policy = crate::approval::strict_policy_revision(&home, &agent)
        .map_err(|_| VerificationError::Unavailable)?;
    step!("authorization.issue.tools");
    let tools = DISPATCHER.get().ok_or(VerificationError::Unavailable)?.ingress_tools(&home, &agent, lane)?;
    // Keep the lifecycle pinned before the policy/catalog I/O; never repin to
    // a replacement employee in the constructor.
    let authorization = human_authorization_with(home.clone(), agent.clone(), lifecycle.employee,
        user.clone(), epoch, lane, &current, tools, turn, credential_expiry, &lifecycle, &RootAllocator,
        &owner.selectors, task.as_deref())?;
    step!("authorization.issue.policy_changed");
    if crate::approval::strict_policy_revision(&home, &agent).ok().as_deref() != Some(policy.as_str()) {
        return Err(VerificationError::Revoked);
    }
    step!("authorization.issue.lifecycle_recheck");
    lifecycle::recheck_with(&home, &lifecycle, &RootAllocator).map_err(lifecycle_refusal)?;
    step!("authorization.issue.snapshot_recheck");
    let after = authority.ingress_snapshot(&user, &agent)?;
    step!("authorization.issue.facts_recheck");
    recheck_issuance_facts(&agent, &lifecycle, &principal, epoch, &current, &after,
        authorization.turn.as_ref().and_then(|turn| turn.background.as_ref()))?;
    Ok(authorization)
}

/// R8x: the first issuing step (also the value before any step was reached).
const FIRST_ISSUE_STEP: RefusalSite = site!("authorization.issue.boot_owner");
thread_local! {
    /// R8x: the issuing step [`record_authorization`] last reached on this
    /// thread (it runs synchronously on one blocking thread). Every exit of
    /// the issuer is attributed to the step it was reached in, so a new exit
    /// there is labelled by construction.
    static ISSUE_STEP: std::cell::Cell<RefusalSite> = const { std::cell::Cell::new(FIRST_ISSUE_STEP) };
}
fn issue_step(step: RefusalSite) { ISSUE_STEP.with(|current| current.set(step)); }
/// R8x: read on the same blocking thread right after issuing failed.
pub(super) fn issue_step_reached() -> RefusalSite { ISSUE_STEP.with(|current| current.get()) }

fn issuance_principal(user: &str, current: &duduclaw_auth::IsolationAuthoritySnapshot)
    -> Result<duduclaw_isolation::capability::Principal, VerificationError> {
    Ok(duduclaw_isolation::capability::Principal::Human {
        user_id: duduclaw_isolation::bounded::Text::new(user.to_owned()).map_err(|_| VerificationError::Invalid)?,
        generation: current.acl_generation,
    })
}

/// Record-backed work must not impersonate a Personal owner's live ingress, except H13b's same-owner
/// delegation receipt (`personal_record_allowed`). All Personal identity comparisons stay in B1's shared gate.
fn issuance_facts(agent: &str, lifecycle: &EmployeeLifecycle, principal: &duduclaw_isolation::capability::Principal,
    current: &duduclaw_auth::IsolationAuthoritySnapshot, record: Option<&crate::isolation_background::RecordAuthority>)
    -> Result<(), VerificationError> {
    use duduclaw_isolation::capability::EmployeeBinding;
    match &lifecycle.binding {
        EmployeeBinding::Personal(owner) => {
            if record.is_some_and(|receipt| !crate::isolation_background::personal_record_allowed(receipt, owner.as_str())) { return Err(VerificationError::Revoked); }
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
    record: Option<&crate::isolation_background::RecordAuthority>) -> Result<(), VerificationError> {
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
    step!("authorization.issue.human");
    if lifecycle.employee != employee || lifecycle.label != agent { return Err(VerificationError::Revoked); }
    if current.human.as_ref().is_none_or(|human| human.user_id != user
        || human.credential_epoch != epoch || human.must_change_password
        || !human.can_assign_to(&agent)) {
        return Err(VerificationError::Revoked);
    }
    let principal = issuance_principal(&user, current)?;
    issuance_facts(&agent, lifecycle, &principal, current, turn.background.as_ref())?;
    // H10: the H9 selectors run after the identity/binding gates and before the
    // lifecycle/users.db rechecks below, so those rechecks cover their I/O. A
    // failed read may be a tombstone moving `agents/<label>` away: the durable
    // lifecycle decides first (as for the wiki walk below).
    let record = turn.background.is_some();
    let select = || selectors::select(sources, &SelectorInput { home: &home, agent: &agent, user: &user,
        tools: &tools, task, record })
        .map_err(|error| lifecycle::recheck_with(&home, lifecycle, root).err().map_or(error, lifecycle_refusal));
    step!("authorization.issue.selectors");
    let selection = select()?;
    step!("authorization.issue.capability");
    let signed_tools = selection.signed_tools(&tools)?; // Workspace signs File (+ HX5 helpers), never pending A/E
    // H13c: a background run of a Personal employee never lists spawn tools.
    let signed_tools = crate::isolation_background::same_owner::record_run_tools(&lifecycle.binding, record, signed_tools)?;
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
    // R8x: this read waits up to 5 s on a locked users.db (busy timeout).
    step!("authorization.issue.users_recheck");
    let mut users = crate::fs_safe::sqlite::open_existing(&home, "users.db", crate::fs_safe::sqlite::Access::ReadOnly)
        .map_err(|_| VerificationError::Unavailable)?;
    users.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| VerificationError::Unavailable)?;
    let after = duduclaw_auth::read_isolation_authority(&mut users, Some(&user), &agent)
        .map_err(|_| VerificationError::Revoked)?;
    recheck_issuance_facts(&agent, lifecycle, &principal, epoch, current, &after, turn.background.as_ref())?;
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
    #[cfg(any(test, feature = "test-support"))]
    let host_broker = sources.work_broker.take();
    let owner = BootOwner { generation: sources.installed_generation.ok_or(VerificationError::Unavailable)?, fork_tools, execution_jobs, recording_jobs, product_authority: sources.product_authority, home: home.to_owned(), backend: backend.clone(), boundary, bindings, worker,
        registry: sources.runtime_registry.take().ok_or(VerificationError::Unavailable)?,
        main_plans: sources.main_plans.take().ok_or(VerificationError::Unavailable)?,
        execution_plans: sources.execution_plans.take().ok_or(VerificationError::Unavailable)?,
        worker_plans: sources.worker_plans.take().ok_or(VerificationError::Unavailable)?,
        turns: sources.turns.take().ok_or(VerificationError::Unavailable)?,
        // W1: run-bound work brokers + HX5 feature view (evaluated last, after
        // every fallible producer; a test build may swap in a host double).
        selectors: {
            let selectors = SelectorSources::installed(home, backend);
            #[cfg(any(test, feature = "test-support"))]
            let selectors = match host_broker { Some(broker) => selectors.with_host_broker(broker), None => selectors };
            selectors
        },
        catalog: sources.connector_catalog, connectors: sources.connectors,
        connector_selection: sources.connector_selection, connector_stack: sources.connector_stack };
    // SFR: HX6's recording store becomes H12d's `skill_from_recording` source
    // (owner-keyed reads, verified artifacts), after every fallible producer.
    // The tool is still signed only behind the recording gate (selectors).
    crate::isolation_skill_jobs::install_recording_source(owner.recording_jobs.clone())?;
    Ok(owner)
}

/// R8x: a refusal of a protected helper with its site (recorded for the
/// current protected turn; no-op outside one). F3: the helpers on the
/// protected path return [`Sited`], so an exit without a site does not compile.
fn sited(site: RefusalSite, error: VerificationError) -> Sited<VerificationError> {
    Sited::new(site, error)
}

/// R8x: [`duduclaw_mcp_boundary::authority::verify_blocking`] whose work names
/// its failing step. A blocking thread cannot reach the turn's task-local
/// carrier, so the label travels back with the error and is recorded on the
/// calling task; `blocking` names a failure of the blocking call itself (its
/// slot wait or deadline). Same values and errors as `verify_blocking`.
pub(super) async fn labelled_blocking<T, F>(blocking: RefusalSite, work: F) -> Result<T, Sited<VerificationError>>
where T: Send + 'static, F: FnOnce() -> Result<T, (RefusalSite, VerificationError)> + Send + 'static {
    match duduclaw_mcp_boundary::authority::verify_blocking(move || Ok(work())).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err((site, error))) => Err(sited(site, error)),
        Err(error) => Err(sited(blocking, error)),
    }
}

pub(super) async fn claude_plan(home: PathBuf, agent: String, class: duduclaw_isolation::execution::ExecutionClass,
    prompt: Vec<u8>, system_prompt: Vec<u8>, model: String, max_turns: u16)
    -> Result<SpawnPolicy, Sited<VerificationError>> {
    crate::isolation_readiness::check_execution()
        .map_err(|_| sited(site!("plan.readiness"), VerificationError::Unavailable))?;
    let owner = OWNER.get().cloned().ok_or_else(|| sited(site!("plan.boot_owner"), VerificationError::Unavailable))?;
    if owner.home != home { return Err(sited(site!("plan.home_mismatch"), VerificationError::BindingMismatch)); }
    labelled_blocking(site!("plan.blocking"), move || {
        let employee = crate::handlers::resolve_confirmed_employee(&owner.home, &agent)
            .map_err(|_| (site!("plan.employee_unconfirmed"), VerificationError::Unavailable))?;
        owner.main_plans.claude(employee.employee, class, prompt, system_prompt, &model, max_turns)
            .map_err(|_| (site!("plan.root_publication"), VerificationError::Unavailable))
    }).await
}

/// R6: the step limit root will enforce for this employee's Claude run, from
/// the fresh root publication (same rule spawnd applies at StartRun). R8x:
/// every refusal is `step_limit.unavailable` (same errors as before; the
/// blocking call is `verify_blocking` through [`labelled_blocking`]).
pub(super) async fn claude_max_turns(home: PathBuf, agent: String, desired: u64)
    -> Result<u16, Sited<VerificationError>> {
    const UNAVAILABLE: RefusalSite = site!("step_limit.unavailable");
    crate::isolation_readiness::check_execution().map_err(|_| sited(UNAVAILABLE, VerificationError::Unavailable))?;
    let owner = OWNER.get().cloned().ok_or_else(|| sited(UNAVAILABLE, VerificationError::Unavailable))?;
    if owner.home != home { return Err(sited(UNAVAILABLE, VerificationError::BindingMismatch)); }
    labelled_blocking(UNAVAILABLE, move || {
        let employee = crate::handlers::resolve_confirmed_employee(&owner.home, &agent)
            .map_err(|_| (UNAVAILABLE, VerificationError::Unavailable))?;
        owner.main_plans.claude_max_turns(employee.employee, desired)
            .map_err(|_| (UNAVAILABLE, VerificationError::Unavailable))
    }).await
}

/// Derive only from the fresh root publication and the confirmed employee.
pub(super) async fn other_runtime_plan(home: PathBuf, agent: String,
    class: duduclaw_isolation::execution::ExecutionClass,
    runtime: duduclaw_isolation::execution::RuntimeKind, request: Vec<u8>)
    -> Result<SpawnPolicy, Sited<VerificationError>> {
    crate::isolation_readiness::check_execution()
        .map_err(|_| sited(site!("plan.readiness"), VerificationError::Unavailable))?;
    let owner = OWNER.get().cloned().ok_or_else(|| sited(site!("plan.boot_owner"), VerificationError::Unavailable))?;
    if owner.home != home { return Err(sited(site!("plan.home_mismatch"), VerificationError::BindingMismatch)); }
    labelled_blocking(site!("plan.blocking"), move || {
        let employee = crate::handlers::resolve_confirmed_employee(&owner.home, &agent)
            .map_err(|_| (site!("plan.employee_unconfirmed"), VerificationError::Unavailable))?;
        owner.main_plans.other_runtime(employee.employee, class, runtime, request)
            .map_err(|_| (site!("plan.root_publication"), VerificationError::Unavailable))
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
pub async fn prepare_launch(mut input: LaunchInput)
    -> Result<Arc<dyn duduclaw_execution::command::ProtectedLaunch>, Sited<VerificationError>> {
    // R8x: every refusal below carries a fixed `launch.*` site (same errors).
    crate::isolation_readiness::check_execution()
        .map_err(|_| sited(site!("launch.readiness"), VerificationError::Unavailable))?;
    let owner = OWNER.get().cloned().ok_or_else(|| sited(site!("launch.boot_owner"), VerificationError::Unavailable))?;
    if !owner.boundary.healthy() {
        return Err(sited(site!("launch.boundary_unhealthy"), VerificationError::Unavailable));
    }
    let checking_owner = owner.clone();
    let (input, prepared) = labelled_blocking(site!("launch.blocking"), move || {
        let mapping = crate::handlers::resolve_confirmed_employee(&checking_owner.home, &input.agent)
            .map_err(|_| (site!("launch.employee_unconfirmed"), VerificationError::Unavailable))?;
        if mapping.employee != input.plan.employee {
            return Err((site!("launch.employee_mismatch"), VerificationError::BindingMismatch));
        }
        // F1: a changed binding/employee forgets the connector build (rebuilt at first use).
        if let Some(stack) = &checking_owner.connector_stack { stack.observe_lifecycle(&input.agent); }
        let prepared = match &checking_owner.product_authority {
            Some(authority) => {
                let prepared = authority.prepare(&input.agent, &mut input.grant, input.authenticated_epoch)
                    .map_err(|error| (site!("launch.product_prepare"), error))?;
                if prepared.employee() != input.plan.employee {
                    return Err((site!("launch.product_prepare"), VerificationError::BindingMismatch));
                }
                Some(match input.turn.take() { Some(turn) => prepared.with_turn(turn), None => prepared })
            }
            None => None,
        };
        checking_owner.bindings.validate(input.plan.employee, &input.grant.binding, &input.grant.principal)
            .map_err(|_| (site!("launch.binding"), VerificationError::BindingMismatch))?;
        Ok((input, prepared))
    }).await?;
    let mut resolver = GatewayResourceResolver::new(owner.home.clone(), input.plan.employee, input.agent.clone(), owner.bindings.clone())
        .map_err(|error| sited(site!("launch.resolver"), error))?;
    if let Some(catalog) = &owner.catalog { resolver = resolver.with_connector_catalog(catalog.clone()); }
    // F1: this employee's providers (rebuilt on route/binding change); the
    // boot singletons are inventory objects and never serve a call.
    let (selection, connectors) = match &owner.connector_stack {
        Some(stack) => {
            let handle = Arc::new(stack.for_employee(&input.agent, input.plan.employee)
                .map_err(|error| sited(site!("launch.connectors"), error))?);
            (Some(handle.clone() as Arc<dyn LiveConnectorSelectionPolicy>), Some(handle as Arc<dyn BoundConnectorExecutor>))
        }
        None => (owner.connector_selection.clone(), owner.connectors.clone()),
    };
    if let Some(selection) = selection { resolver = resolver.with_connector_selection_policy(selection); }
    let resolver = Arc::new(resolver);
    {
        let defaults = crate::isolation_worker_plans::GatewayWorkerPlans::for_employee(
            owner.home.clone(), input.agent.clone(), input.plan.employee)
            .map_err(|error| sited(site!("launch.worker_plans"), error))?.with_roots(input.roots.clone());
        let worker_plans = Arc::new(duduclaw_mcp_boundary::worker_execution::CompositeWorkerPlans::new(
            vec![owner.worker_plans.clone(), Arc::new(defaults)])
            .map_err(|error| sited(site!("launch.worker_plans"), error))?);
        let factories = GatewayFactories::assemble(Some(owner.backend.clone()), Some(owner.execution_plans.clone()),
            resolver.clone(), Some(owner.worker.parser.clone()), input.roots.clone())
            .map_err(|error| sited(site!("launch.factories"), error))?
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
            DISPATCHER.get().ok_or(VerificationError::Unavailable)?.create(dispatcher_input)).await
            .map_err(|error| sited(site!("launch.dispatcher"), error))?;
        let launch = ProtectedMcpLaunch::new(owner.backend.clone(), owner.boundary.boundary().clone(),
            owner.registry.clone(), dispatcher, input.plan, input.grant, input.origin, input.caller)
            .map_err(|_| sited(site!("launch.protected_launch"), VerificationError::Unavailable))?;
        return match (&owner.product_authority, prepared) {
            (Some(authority), Some(prepared)) => Ok(Arc::new(super::launch::AuthorizedLaunch::new(
                launch, authority.clone(), prepared))),
            (None, None) => Ok(launch),
            _ => Err(sited(site!("launch.authority_pairing"), VerificationError::Unavailable)),
        };
    }
}

/// DEFECT 26: the bounded boot retry driver and its tests.
#[path = "boot_retry.rs"]
mod retry;

/// R8n-1: the ParseWorker and RuntimeAdapters integration probes.
#[path = "boot_probes.rs"]
mod probes;

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
    fn isolation_boot_worker_integration_is_present_with_any_build_features() {
        let absent = missing(&BootSources::default(), true, true);
        assert!(!absent.contains(&MissingProducer::WorkerIntegration),
            "worker integration must be compiled even with --no-default-features");
    }
    #[test]
    fn isolation_boot_missing_authority_never_reports_constructed() {
        let absent = missing(&BootSources::default(), true, true);
        for producer in [MissingProducer::LiveAuthority, MissingProducer::DurableBindings,
            MissingProducer::RuntimePublication, MissingProducer::MainPlans, MissingProducer::ExecutionPlans, MissingProducer::WorkerRetirement,
            MissingProducer::WorkerPlans, MissingProducer::TurnAuthority,
            MissingProducer::ConnectorProviders] { assert!(absent.contains(&producer)); }
        assert!(!absent.contains(&MissingProducer::Dispatcher));
    }
    #[test]
    fn isolation_boot_missing_backend_and_binary_adapter_are_independent() {
        let absent = missing(&BootSources::default(), false, false);
        assert!(absent.contains(&MissingProducer::Backend));
        assert!(absent.contains(&MissingProducer::Dispatcher));
    }
    #[test]
    fn isolation_boot_requires_catalog_execution_and_live_selection_together() {
        let registry = Arc::new(crate::isolation_connector_registry::ConnectorProviderRegistry::default());
        let mut sources = BootSources::default();
        sources.connector_catalog = Some(Arc::new(ConnectorCatalog::new(
            rusqlite::Connection::open_in_memory().unwrap()).unwrap()));
        sources.connectors = Some(registry.clone());
        assert!(missing(&sources, true, true).contains(&MissingProducer::ConnectorProviders));
        sources.connector_selection = Some(registry);
        // Presence is only the first gate; assemble separately checks every tool.
        assert!(!missing(&sources, true, true).contains(&MissingProducer::ConnectorProviders));
        sources.connectors = None;
        assert!(missing(&sources, true, true).contains(&MissingProducer::ConnectorProviders));
    }
    /// DEFECT 26: a failed attempt forgets what it derived from the updater's
    /// generation / spawnd's projection; host-installed inputs survive.
    #[test]
    fn boot_failed_attempt_forgets_derived_inputs_and_keeps_host_inputs() {
        let mut sources = BootSources::default();
        sources.installed_generation = Some(duduclaw_isolation::ids::Sha256Digest([7; 32]));
        sources.worker_launcher = Some(Box::new(duduclaw_parse_worker::retirement::RetiringLauncher));
        Derived { worker_launcher: true, ..Derived::default() }.forget(&mut sources);
        assert!(sources.installed_generation.is_none());
        assert!(sources.worker_launcher.is_none());
        sources.worker_launcher = Some(Box::new(duduclaw_parse_worker::retirement::RetiringLauncher));
        Derived::default().forget(&mut sources);
        assert!(sources.worker_launcher.is_some(), "a host-installed launcher is kept");
    }
    #[test]
    fn boot_only_structural_producers_end_the_retry() {
        assert!(!structural(&[MissingProducer::WorkerIntegration]));
        assert!(structural(&[MissingProducer::RuntimePublication, MissingProducer::Dispatcher]));
        assert!(!structural(&[MissingProducer::RuntimePublication, MissingProducer::MainPlans,
            MissingProducer::ExecutionPlans, MissingProducer::ConnectorProviders]));
        assert!(!structural(&[MissingProducer::Initialization]));
    }
    /// R8m 30a: mirrors `attempt_installed`'s catalog/authority step over the
    /// real `catalog_for_authority`, driven by the real retry loop.
    struct CatalogBoot {
        catalog: Option<Arc<ConnectorCatalog>>,
        /// The catalog the (fake) authority was built over.
        authority: Option<Arc<ConnectorCatalog>>,
        opens: usize,
        failing_opens: usize,
        authorities_built: usize,
        state: Arc<retry::BootState>,
    }
    impl retry::BootAttempt for CatalogBoot {
        fn attempt(&mut self) -> retry::AttemptFuture<'_> {
            Box::pin(async move {
                let mut reasons = Vec::new();
                if self.authority.is_none() {
                    let (opens, failing) = (&mut self.opens, self.failing_opens);
                    let open = || {
                        *opens += 1;
                        if *opens <= failing { return Err(crate::isolation_connectors::CatalogOpenFailure::FileUnsafe); }
                        ConnectorCatalog::new(rusqlite::Connection::open_in_memory().unwrap())
                            .map_err(|_| crate::isolation_connectors::CatalogOpenFailure::Schema)
                    };
                    match catalog_for_authority(&mut self.catalog, open) {
                        Err(failure) => reasons.push((MissingProducer::ConnectorProviders, failure.reason())),
                        Ok(catalog) => { self.authorities_built += 1; self.authority = Some(catalog); }
                    }
                }
                if self.authority.is_none() {
                    return Err(retry::Failure { missing: vec![MissingProducer::ConnectorProviders], retry: true, reasons });
                }
                self.state.constructed();
                Ok(())
            })
        }
        fn assembled(&mut self) {}
    }

    /// R8m 30a: the catalog's first two opens fail, the third succeeds. A
    /// failed attempt builds no authority and keeps nothing (the next attempt
    /// opens again); the later attempt assembles without a restart, with the
    /// authority over the very catalog that is kept.
    #[tokio::test]
    async fn boot_catalog_open_failure_is_retried_and_never_latched() {
        const FAST: retry::Schedule = retry::Schedule { early: std::time::Duration::from_millis(2),
            early_for: std::time::Duration::from_secs(60), late: std::time::Duration::from_millis(5),
            poll: std::time::Duration::from_millis(1) };
        let state = Arc::new(retry::BootState::new());
        let mut boot = CatalogBoot { catalog: None, authority: None, opens: 0, failing_opens: 2,
            authorities_built: 0, state: state.clone() };
        let started = std::time::Instant::now();
        assert!(matches!(retry::step(&mut boot, &state, started).await, retry::Step::Retry));
        assert!(boot.catalog.is_none() && boot.authority.is_none(), "no authority without a catalog");
        assert!(state.status().is_some_and(|status| !status.constructed
            && status.missing == vec![MissingProducer::ConnectorProviders]));
        assert!(retry::retry_until_assembled(&mut boot, &state, started, FAST, || false).await);
        assert_eq!((boot.opens, boot.authorities_built), (3, 1), "opened on every attempt until it succeeded");
        assert!(Arc::ptr_eq(boot.authority.as_ref().unwrap(), boot.catalog.as_ref().unwrap()));
        assert!(state.status().is_some_and(|status| status.constructed));
        // Once opened it is kept: no second open.
        let kept = catalog_for_authority(&mut boot.catalog, || panic!("never reopened")).unwrap();
        assert!(Arc::ptr_eq(&kept, boot.authority.as_ref().unwrap()));
    }

    /// R8m 30a: every reason the assembly can log is a fixed, distinct
    /// snake_case identifier.
    #[test]
    fn boot_fixed_reasons_are_distinct_identifiers() {
        let reasons = fixed_reasons();
        let mut unique = reasons.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), reasons.len());
        for reason in reasons {
            assert!(!reason.is_empty() && reason.len() <= 64
                && reason.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_'), "{reason}");
        }
        assert!(fixed_reasons().contains(&"catalog_home_unsafe"));
        assert!(fixed_reasons().contains(&"invalid_helper_runtime"));
        // R8n-1: a policy without helper runtimes is no longer a missing producer.
        assert!(!fixed_reasons().contains(&"no_helper_runtime_in_policy"));
    }

    /// R8n-1 2.4: the assembly installs exactly the three integration probes
    /// (McpBoundary, ParseWorker, RuntimeAdapters), and does so inside
    /// `install_and_publish` (the shutdown-gate permit), after the owner is set.
    #[test]
    fn r8n_assembly_installs_the_three_integration_probes() {
        use crate::isolation_readiness::IntegrationComponent;
        let mut installed = Vec::new();
        install_probes(|component, _| { installed.push(component); Ok(()) });
        assert_eq!(installed, [IntegrationComponent::McpBoundary, IntegrationComponent::ParseWorker,
            IntegrationComponent::RuntimeAdapters]);
        // A refused registration is logged, never retried as "ready".
        install_probes(|_, _| Err(crate::isolation_readiness::ProbeError::AlreadyInstalled));
        let source = include_str!("boot.rs");
        let publish = source.split("fn install_and_publish(").nth(1).unwrap().split("\n}\n").next().unwrap();
        let owner_set = publish.find("OWNER.set(").unwrap();
        let probes = publish.find("install_probes(crate::isolation_readiness::install_integration_probe)").unwrap();
        assert!(owner_set < probes, "probes are installed after the owner exists");
    }

    /// R8n-2 B4: a refused boot producer verification publishes its fixed
    /// reason (and the adapter's first unprovided tool) instead of an empty
    /// `Initialization`; a passing one answers no reason; the decision is
    /// always `verify_producers` (fail closed by default).
    #[test]
    fn r8n2_precheck_refusal_names_producer_verification_and_the_first_unprovided_tool() {
        use duduclaw_mcp_boundary::{execution::{AuthorizedExecutionPlan, PolicyFuture}, tool_policy::AuthorizedToolCall};
        struct NoPlans;
        impl ExecutionPlanFactory for NoPlans {
            fn supports(&self, _: &str) -> bool { false }
            fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
                Box::pin(async { Err(VerificationError::Unavailable) })
            }
            fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall, _: duduclaw_isolation::execution::RunHandle)
                -> PolicyFuture<'a, AuthorizedExecutionPlan> { Box::pin(async { Err(VerificationError::Unavailable) }) }
        }
        /// `None`: only the trait defaults (refuses, names nothing).
        /// `Some(None)`: passes. `Some(Some(tool))`: refuses naming `tool`.
        struct Adapter(Option<Option<&'static str>>);
        impl DispatcherFactory for Adapter {
            fn create(&self, _: DispatcherInput) -> Result<Arc<dyn Dispatcher>, VerificationError> {
                Err(VerificationError::Unavailable)
            }
            fn verify_producers(&self, _: &dyn ExecutionPlanFactory, _: &dyn WorkerPlanFactory,
                _: &dyn BoundConnectorExecutor, _: &dyn LiveConnectorSelectionPolicy) -> Result<(), VerificationError> {
                match self.0 { Some(None) => Ok(()), _ => Err(VerificationError::Unavailable) }
            }
            fn unprovided_tool(&self, _: &dyn ExecutionPlanFactory, _: &dyn WorkerPlanFactory,
                _: &dyn BoundConnectorExecutor, _: &dyn LiveConnectorSelectionPolicy) -> Option<&'static str> {
                self.0.flatten()
            }
        }
        struct Defaults;
        impl DispatcherFactory for Defaults {
            fn create(&self, _: DispatcherInput) -> Result<Arc<dyn Dispatcher>, VerificationError> {
                Err(VerificationError::Unavailable)
            }
        }
        let home = tempfile::tempdir().unwrap();
        let worker = crate::isolation_worker_plans::GatewayWorkerPlans::new(home.path().to_owned()).unwrap();
        let registry = crate::isolation_connector_registry::ConnectorProviderRegistry::default();
        let check = |adapter: &dyn DispatcherFactory| verify_producers_with_reason(adapter, &NoPlans, &worker,
            &registry, &registry);
        let init = MissingProducer::Initialization;
        assert_eq!(check(&Defaults), Err(vec![(init, "producer_verification")]));
        assert_eq!(check(&Adapter(None)), Err(vec![(init, "producer_verification")]));
        assert_eq!(check(&Adapter(Some(Some("skill_extract")))),
            Err(vec![(init, "producer_verification"), (init, "skill_extract")]));
        assert_eq!(check(&Adapter(Some(None))), Ok(()));
        assert_eq!(retry::Listed(&[init], &check(&Adapter(Some(Some("skill_extract")))).unwrap_err()).to_string(),
            "[Initialization(producer_verification: skill_extract)]");
        // The published view stays producer names only (the strict reader is unchanged).
        assert_eq!(init.name(), "initialization");
        assert!(fixed_reasons().contains(&PRODUCER_VERIFICATION));
    }

    /// The readiness reads `boot_status`: not constructed (closed reasons)
    /// until an attempt succeeded, then constructed while the owner is healthy.
    #[test]
    fn boot_status_follows_the_latest_attempt_without_a_restart() {
        let state = retry::BootState::new();
        let initial = effective_status(state.status(), false);
        assert!(!initial.constructed && initial.missing == vec![MissingProducer::Initialization]);
        state.constructed();
        let ready = effective_status(state.status(), true);
        assert!(ready.constructed && ready.missing.is_empty());
        let unhealthy = effective_status(state.status(), false);
        assert!(!unhealthy.constructed && unhealthy.missing == vec![MissingProducer::Initialization]);
    }
}

/// HX6 Admin recording control and the recording audit sink (security fix
/// round, finding 6); split out to keep this file below 800 lines. Merge
/// P1h × HX6: P1h's withholding of a non-owned Personal recording result
/// lives in `boot_recording.rs` on the HX6 recording store.
#[path = "boot_recording.rs"]
mod recording;
pub(crate) use recording::control_recording;

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
#[path = "test_support.rs"]
pub mod test_support;
