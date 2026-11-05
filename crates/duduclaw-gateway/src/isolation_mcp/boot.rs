//! Trusted boot composition. Missing producers are explicit; no socket-default
//! worker, synthetic principal, or inferred runtime acceptance is installed.
use std::{path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock}, sync::atomic::{AtomicBool, Ordering}};
use duduclaw_isolation::{execution::{SpawnPolicy, WaitOrigin}, ids::EmployeeId};
use duduclaw_mcp_boundary::{bindings::BindingAuthority, execution::ExecutionPlanFactory,
    factories::GatewayFactories, resources::ResourceRoots, worker_execution::WorkerPlanFactory};
use duduclaw_parse_worker::client::WorkerLauncher;
use super::{Dispatcher, GrantPolicy, LiveAuthority, McpBoundaryService, ProtectedMcpLaunch,
    RuntimeBridgeRegistry, VerificationError};
use crate::{isolation_connectors::{BoundConnectorExecutor, ConnectorCatalog},
    isolation_egress::RunTurnResolver, isolation_resources::GatewayResourceResolver};

/// CLI supplies its adapter before start_gateway. The gateway owns every input;
/// the adapter must authenticate its original dispatcher and check completeness.
pub trait DispatcherFactory: Send + Sync {
    fn create(&self, input: DispatcherInput) -> Result<Arc<dyn Dispatcher>, VerificationError>;
    fn verify_producers(&self, _execution: &dyn ExecutionPlanFactory, _worker: &dyn WorkerPlanFactory)
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
    pub worker_launcher: Option<Box<dyn WorkerLauncher>>,
    pub worker_plans: Option<Arc<dyn WorkerPlanFactory>>,
    pub turns: Option<Arc<dyn RunTurnResolver>>,
    pub connector_catalog: Option<Arc<ConnectorCatalog>>,
    pub connectors: Option<Arc<dyn BoundConnectorExecutor>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingProducer {
    Backend, LiveAuthority, DurableBindings, RuntimePublication, ExecutionPlans,
    WorkerRetirement, WorkerPlans, TurnAuthority, Dispatcher, WorkerIntegration, MainPlans, Initialization,
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

pub fn install_dispatcher_factory(factory: Arc<dyn DispatcherFactory>) -> Result<(), VerificationError> {
    if ATTEMPTED.load(Ordering::Acquire) { return Err(VerificationError::Unavailable); }
    DISPATCHER.set(factory).map_err(|_| VerificationError::Unavailable)
}
pub fn install_boot_sources(sources: BootSources) -> Result<(), VerificationError> {
    let mut slot = SOURCES.lock().map_err(|_| VerificationError::Unavailable)?;
    if ATTEMPTED.load(Ordering::Acquire) || slot.is_some() { return Err(VerificationError::Unavailable); }
    *slot = Some(sources);
    Ok(())
}
fn fill_installed_sources(sources: &mut BootSources) {
    let generation = crate::isolation_readiness::verified_generation();
    sources.installed_generation = generation;
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
    fill_installed_sources(&mut sources);
    let backend = duduclaw_execution::selection::protected_backend_for_boot();
    // Reuse the gateway's initialized users.db and confirmed employee registry.
    // No parallel principal/binding database is created for the boundary.
    if sources.authority.is_none() && sources.bindings.is_none() {
        if let Ok(backend) = &backend {
            if let Ok(authority) = super::authority::GatewayAuthority::new(home.to_owned(), users, backend.clone(), agents) {
                let authority = Arc::new(authority);
                sources.authority = Some(authority.clone());
                sources.bindings = Some(authority.clone());
                if sources.turns.is_none() { sources.turns = Some(authority.clone()); }
                sources.product_authority = Some(authority);
            }
        }
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

pub fn boot_status() -> BootStatus {
    let mut status = STATUS.get().cloned().unwrap_or(BootStatus { constructed: false,
        missing: vec![MissingProducer::Initialization] });
    if status.constructed && OWNER.get().is_none_or(|owner| !owner.boundary.healthy()) {
        status.constructed = false;
        status.missing = vec![MissingProducer::Initialization];
    }
    status
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
    worker_plans: Arc<dyn WorkerPlanFactory>,
    #[cfg(feature = "isolation-worker-integration")]
    worker: crate::isolation_execution::InstalledWorkerServices,
    turns: Arc<dyn RunTurnResolver>,
    catalog: Option<Arc<ConnectorCatalog>>,
    connectors: Option<Arc<dyn BoundConnectorExecutor>>,
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
            owner.execution_plans.as_ref(), owner.worker_plans.as_ref()).map_err(|_| ProbeError::Unavailable)
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

/// Only the authenticated ingress module calls this after consuming its proof.
/// Gateway-owned namespaces are finite grants. Workspace/connector/recipient
/// grants require their own authenticated host selectors and are not inferred.
pub(crate) fn human_authorization(home: PathBuf, agent: String, user: String, epoch: u64,
    lane: duduclaw_isolation::capability::Lane, turn: super::HostTurn, credential_expiry: Option<u64>)
    -> Result<super::RuntimeAuthorization, VerificationError> {
    use duduclaw_isolation::{bounded::{List, Text}, capability::{EmployeeBinding, Principal, ResourceGrant}};
    let owner = OWNER.get().ok_or(VerificationError::Unavailable)?;
    if owner.home != home || !owner.boundary.healthy() { return Err(VerificationError::Unavailable); }
    let authority = owner.product_authority.as_ref().ok_or(VerificationError::Unavailable)?;
    let current = authority.ingress_snapshot(&user, &agent)?;
    super::ingress::require_epoch(epoch, current.human.as_ref().map(|human| human.credential_epoch))?;
    let receipt = crate::handlers::resolve_confirmed_employee(&home, &agent)
        .map_err(|_| VerificationError::Unavailable)?;
    if receipt.disabled { return Err(VerificationError::Revoked); }
    let policy = crate::approval::strict_policy_revision(&home, &agent)
        .map_err(|_| VerificationError::Unavailable)?;
    let tools = DISPATCHER.get().ok_or(VerificationError::Unavailable)?.ingress_tools(&home, &agent, lane)?;
    let text = |value| Text::new(value).map_err(|_| VerificationError::Invalid);
    let namespace = format!("agent:{agent}");
    let mut resources = vec![ResourceGrant::Employee { id: receipt.employee },
        ResourceGrant::Mailbox { id: text(namespace.clone())? }];
    for table in ["working_state", "beliefs", "user_profile", "decisions", "memory"] {
        resources.push(ResourceGrant::DatabaseTable { database: text(namespace.clone())?, table: text(table.into())? });
    }
    let mut roots = ResourceRoots::default();
    // These directories live in gateway-private state, never the writable
    // employee mount. Walk from a trusted home fd without following symlinks.
    if let Some(wiki) = super::ingress::own_wiki_root(&home, &agent)? {
        roots.insert_wiki(namespace.clone(), receipt.employee, wiki)?;
        resources.push(ResourceGrant::Wiki { namespace: text(namespace)? });
    }
    let after = authority.ingress_snapshot(&user, &agent)?;
    if current.acl_generation != after.acl_generation || current.policy_epoch != after.policy_epoch
        || current.tenant_id != after.tenant_id
        || after.human.as_ref().map(|human| human.credential_epoch) != Some(epoch)
        || crate::approval::strict_policy_revision(&home, &agent).ok().as_deref() != Some(policy.as_str()) {
        return Err(VerificationError::Revoked);
    }
    let now = duduclaw_mcp_boundary::authority::now_seconds()?;
    let expires_at = now.checked_add(30 * 60).ok_or(VerificationError::InvalidTime)?
        .min(credential_expiry.unwrap_or(u64::MAX));
    if expires_at <= now { return Err(VerificationError::InvalidTime); }
    Ok(super::RuntimeAuthorization { home, agent, grant: GrantPolicy {
        task: None, principal: Principal::Human { user_id: text(user)?, generation: current.acl_generation },
        binding: EmployeeBinding::Shared(text(current.tenant_id)?), lane, tools,
        resources: List::new(resources).map_err(|_| VerificationError::Invalid)?,
        policy_revision: current.policy_epoch, not_before: now, expires_at }, roots: Arc::new(roots),
        origin: WaitOrigin::Gateway, caller: None, turn: Some(turn), authenticated_epoch: Some(epoch) })
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
    let bindings = sources.bindings.take().ok_or(VerificationError::Unavailable)?;
    let run_policy = sources.authority.take().ok_or(VerificationError::Unavailable)?;
    let live: Arc<dyn LiveAuthority> = if sources.product_authority.is_some() { run_policy }
        else { Arc::new(duduclaw_mcp_boundary::bindings::BindingCheckedAuthority { run_policy, bindings: bindings.clone() }) };
    let boundary = McpBoundaryService::start(live)?;
    let worker = crate::isolation_execution::install_worker_services(
        sources.worker_launcher.take().ok_or(VerificationError::Unavailable)?)?;
    Ok(BootOwner { generation: sources.installed_generation.ok_or(VerificationError::Unavailable)?,
        product_authority: sources.product_authority, home: home.to_owned(), backend, boundary, bindings, worker,
        registry: sources.runtime_registry.take().ok_or(VerificationError::Unavailable)?,
        main_plans: sources.main_plans.take().ok_or(VerificationError::Unavailable)?,
        execution_plans: sources.execution_plans.take().ok_or(VerificationError::Unavailable)?,
        worker_plans: sources.worker_plans.take().ok_or(VerificationError::Unavailable)?,
        turns: sources.turns.take().ok_or(VerificationError::Unavailable)?,
        catalog: sources.connector_catalog, connectors: sources.connectors })
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
    let resolver = Arc::new(resolver);
    #[cfg(feature = "isolation-worker-integration")]
    {
        let factories = GatewayFactories::assemble(Some(owner.backend.clone()), Some(owner.execution_plans.clone()),
            resolver.clone(), Some(owner.worker.parser.clone()))?
            .with_worker(owner.worker.tools(owner.worker_plans.clone(), resolver.clone()));
        let dispatcher_input = DispatcherInput {
            home: owner.home.clone(), employee: input.plan.employee, agent: input.agent,
            roots: input.roots, resolver, factories, turns: owner.turns.clone(), connectors: owner.connectors.clone(),
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
    Err(VerificationError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolation_boot_missing_authority_never_reports_constructed() {
        let absent = missing(&BootSources::default(), true, true, true);
        for producer in [MissingProducer::LiveAuthority, MissingProducer::DurableBindings,
            MissingProducer::RuntimePublication, MissingProducer::MainPlans, MissingProducer::ExecutionPlans, MissingProducer::WorkerRetirement,
            MissingProducer::WorkerPlans, MissingProducer::TurnAuthority] { assert!(absent.contains(&producer)); }
        assert!(!absent.contains(&MissingProducer::Dispatcher));
    }
    #[test]
    fn isolation_boot_missing_backend_and_binary_adapter_are_independent() {
        let absent = missing(&BootSources::default(), false, false, false);
        assert!(absent.contains(&MissingProducer::Backend));
        assert!(absent.contains(&MissingProducer::Dispatcher));
        assert!(absent.contains(&MissingProducer::WorkerIntegration));
    }
}
