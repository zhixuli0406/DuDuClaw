//! v1.81 F1 tests: one catalog handle, CLI executor factories, rebuild on
//! route/binding change, generation-keyed vault, readiness on every build,
//! missing factory fails closed.
use super::*;
use std::sync::atomic::AtomicUsize;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane, LiveRequestVerifier,
    Principal, ResourceGrant}, ids::{RunId, Sha256Digest}};
use duduclaw_mcp_boundary::bindings::BindingStore;
use rusqlite::Connection;
use serde_json::json;
use crate::isolation_connector_boot::{ChannelExecutorFactory, GoogleBridgeInputs, OdooExecutorFactory, ProviderRefusal,
    connector_executor_factories};
use crate::isolation_connector_channel::ChannelConnectorProvider;
use crate::isolation_connector_clients::{AccountCredential, AccountCredentialProvider, VaultedCredentials};
use crate::isolation_connector_odoo::OdooConnectorProvider;
use crate::isolation_connector_search::SearchConnectorProvider;
use crate::isolation_connectors::AccountId;

const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
const AGENT: &str = "employee-a";
fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn alice() -> EmployeeBinding { EmployeeBinding::Personal(text("alice")) }
fn bob() -> EmployeeBinding { EmployeeBinding::Personal(text("bob")) }
fn company() -> EmployeeBinding { EmployeeBinding::Shared(text("company")) }

struct Live;
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
fn context(binding: EmployeeBinding, user: &str) -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run: RunId([3; 16]), task: None,
        principal: Principal::Human { user_id: text(user), generation: 1 }, employee_binding: binding, lane: Lane::Standard,
        tools: List::new(["model_search", "odoo_status", "db_sources", "gmail_search"].iter().map(|tool| text(tool)).collect())
            .unwrap(),
        resources: List::new(vec![ResourceGrant::EmployeeScope { id: EMPLOYEE }]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Live))
}
fn ids(result: Result<Vec<ProductConnectorAccount>, VerificationError>) -> Vec<String> {
    let mut ids: Vec<_> = result.unwrap().iter().map(|account| account.account().to_owned()).collect();
    ids.sort();
    ids
}

const ODOO_CONFIG: &str = "[odoo]\nurl='https://odoo.example.com'\ndb='demo'\nusername='shared'\napi_key_enc='shared-reference'\n";
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let agent = home.path().join("agents").join(AGENT);
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("agent.toml"), "[capabilities]\ndb_sources=['demo']\n").unwrap();
    std::fs::write(home.path().join("config.toml"), ODOO_CONFIG).unwrap();
    home
}
fn memory_catalog() -> Arc<ConnectorCatalog> {
    Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap())
}
fn bindings() -> Arc<dyn BindingAuthority> {
    Arc::new(Mutex::new(BindingStore::new(Connection::open_in_memory().unwrap()).unwrap()))
}

/// Bound executor stub covering exactly one provider's tools.
struct Refusing(&'static str);
impl BoundConnectorExecutor for Refusing {
    fn supports(&self, tool: &str) -> bool { provider_for_tool(tool) == Some(self.0) }
    fn execute<'a>(&'a self, _: SelectedConnector, _: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
}
/// Stand-in for D3's CLI executor factories; counts bindings.
struct Executors { provider: &'static str, incomplete: bool, bound: AtomicUsize }
fn executors(provider: &'static str) -> Arc<Executors> {
    Arc::new(Executors { provider, incomplete: false, bound: AtomicUsize::new(0) })
}
impl Executors {
    fn covers(&self, tool: &str) -> bool {
        provider_for_tool(tool) == Some(self.provider) && !(self.incomplete && tool == "odoo_execute")
    }
    fn bind(&self) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> {
        self.bound.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Refusing(self.provider)))
    }
}
impl OdooExecutorFactory for Executors {
    fn supports(&self, tool: &str) -> bool { self.covers(tool) }
    fn executor(&self, _: Arc<OdooConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> { self.bind() }
}
impl ChannelExecutorFactory for Executors {
    fn supports(&self, tool: &str) -> bool { self.covers(tool) }
    fn executor(&self, _: Arc<ChannelConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> { self.bind() }
}
fn cli_executors(odoo: Arc<Executors>, channel: Arc<Executors>) -> ConnectorExecutorFactories {
    ConnectorExecutorFactories { odoo, channel }
}

struct NoSearch;
impl SearchProviderSource for NoSearch {
    fn search(&self, _: EmployeeId, _: ProductConnectorAccount) -> Result<Arc<SearchConnectorProvider>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}
struct NoTurns;
impl crate::isolation_egress::RunTurnResolver for NoTurns {
    fn resolve(&self, _: &RequestContext) -> Result<crate::isolation_egress::RunTurnIdentity, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}
/// Real bridge construction; records the catalog handle it was given.
struct RecordingGoogle { inputs: GoogleBridgeInputs, seen: Mutex<Option<Arc<ConnectorCatalog>>> }
impl GoogleBridgeFactory for RecordingGoogle {
    fn bridge(&self, home: &Path, agent: &str, employee: EmployeeId, catalog: Arc<ConnectorCatalog>)
        -> Result<Arc<GoogleIsolationBridge>, VerificationError> {
        *self.seen.lock().unwrap() = Some(catalog.clone());
        self.inputs.bridge(home, agent, employee, catalog)
    }
}
fn google() -> Arc<RecordingGoogle> {
    Arc::new(RecordingGoogle { seen: Mutex::new(None), inputs: GoogleBridgeInputs {
        users: None,
        reference: duduclaw_core::google_identity::CredentialRef {
            kind: duduclaw_core::google_identity::CredentialKind::Legacy, sha256: "a".repeat(64) },
        turns: Arc::new(NoTurns),
    } })
}
fn inputs(home: &Path, google: Arc<dyn GoogleBridgeFactory>, executors: ConnectorExecutorFactories,
    vault: Arc<AccountCredentialVault>) -> ConnectorStackInputs {
    ConnectorStackInputs {
        google, search: Arc::new(NoSearch),
        hardware: duduclaw_inference::types::HardwareInfo {
            gpu_type: duduclaw_inference::types::GpuType::None, gpu_name: String::new(),
            vram_total_mb: 0, vram_available_mb: 0, ram_total_mb: 4096, ram_available_mb: 4096, cpu_cores: 1,
            recommended_backend: duduclaw_inference::types::BackendType::OpenAiCompat, recommended_max_model_gb: 0.0,
        },
        computer_sessions: crate::computer_use_sessions::workspace_admin::admin_sessions(home),
        executors, vault,
    }
}
fn factory(home: &Path, catalog: &Arc<ConnectorCatalog>) -> ConnectorStack {
    gateway_factory(home, catalog, Some(company()), inputs(home, google(),
        cli_executors(executors("odoo"), executors("channel")), Arc::default())).unwrap()
}

/// Lifecycle double: label ⇒ (employee id, binding).
#[derive(Default)]
struct Lifecycle(Mutex<BTreeMap<String, (EmployeeId, EmployeeBinding)>>);
impl Lifecycle {
    fn with(employee: EmployeeId, binding: EmployeeBinding) -> Arc<Self> {
        let lifecycle = Arc::new(Self::default());
        lifecycle.set(employee, binding);
        lifecycle
    }
    fn set(&self, employee: EmployeeId, binding: EmployeeBinding) {
        self.0.lock().unwrap().insert(AGENT.into(), (employee, binding));
    }
}
impl EmployeeBindingSource for Lifecycle {
    fn current(&self, agent: &str) -> Result<(EmployeeId, EmployeeBinding), VerificationError> {
        self.0.lock().unwrap().get(agent).cloned().ok_or(VerificationError::Revoked)
    }
    fn labels(&self) -> Result<Vec<String>, VerificationError> { Ok(self.0.lock().unwrap().keys().cloned().collect()) }
}

/// Every reported build: (agent, employee, setup-required providers | error).
type Reported = (String, EmployeeId, Result<Vec<&'static str>, VerificationError>);
#[derive(Default)]
struct Reports(Mutex<Vec<Reported>>);
impl Reports {
    fn sink(self: &Arc<Self>) -> BuildReport {
        let reports = self.clone();
        Arc::new(move |_: &Path, agent: &str, employee: EmployeeId, outcome: Result<&InstalledConnectorProviders, VerificationError>|
            reports.0.lock().unwrap().push((agent.to_owned(), employee, outcome.map(|installed| installed.setup_required().collect()))))
    }
    fn count(&self) -> usize { self.0.lock().unwrap().len() }
    fn last(&self) -> Reported { self.0.lock().unwrap().last().cloned().unwrap() }
}

struct Counting { inner: Arc<dyn ConnectorProviderFactory>, builds: AtomicUsize }
impl ConnectorProviderFactory for Counting {
    fn inventory(&self) -> Result<(), VerificationError> { self.inner.inventory() }
    fn build(&self, home: &Path, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        catalog: Arc<ConnectorCatalog>, bindings: Arc<dyn BindingAuthority>) -> Result<InstalledConnectorProviders, VerificationError> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        self.inner.build(home, agent, employee, binding, catalog, bindings)
    }
}

fn stack(home: &Path, catalog: &Arc<ConnectorCatalog>, bindings: Arc<dyn BindingAuthority>, lifecycle: Arc<Lifecycle>,
    reports: &Arc<Reports>) -> Arc<EmployeeConnectorProviders> {
    Arc::new(EmployeeConnectorProviders::new(home.to_path_buf(), catalog.clone(), bindings, Some(company()), lifecycle,
        reports.sink()).unwrap())
}

#[test]
fn f1_boot_shares_one_catalog_handle() {
    let home = home();
    let catalog = memory_catalog();
    let model = catalog.register("alice model", "model", ConnectorKind::Personal, &alice()).unwrap();
    let principal = Principal::Human { user_id: text("alice"), generation: 1 };
    let mut store = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    store.register_principal(&principal).unwrap();
    store.create_employee(EMPLOYEE, &alice()).unwrap();
    store.register_connector(model.as_str(), &alice()).unwrap();
    let bindings: Arc<dyn BindingAuthority> = Arc::new(Mutex::new(store));

    let recording = google();
    let assembled = gateway_factory(home.path(), &catalog, Some(company()), inputs(home.path(), recording.clone(),
        cli_executors(executors("odoo"), executors("channel")), Arc::default())).unwrap();
    // C6 details reader over the boot handle, never a second open.
    assert!(assembled.details.shares_catalog(&catalog));
    assert!(!assembled.details.shares_catalog(&memory_catalog()));

    let reports = Arc::new(Reports::default());
    let stack = stack(home.path(), &catalog, bindings.clone(), Lifecycle::with(EMPLOYEE, alice()), &reports);
    assert!(Arc::ptr_eq(stack.catalog(), &catalog), "the stack keeps the authority's handle");
    stack.install_factory(assembled.factory.clone()).unwrap();
    let handle = Arc::new(stack.for_employee(AGENT, EMPLOYEE).unwrap());
    let ctx = context(alice(), "alice");
    assert_eq!(ids(handle.candidates(&ctx, "model_search", "model")), vec![model.as_str().to_owned()]);
    assert!(Arc::ptr_eq(recording.seen.lock().unwrap().as_ref().unwrap(), &catalog),
        "the factory hands the same handle to the Google bridge");

    // Selection pins against that same handle and follows its revoke.
    let selected = SelectedConnector::select_with_policy(&ctx, "model_search", &json!({}), bindings, catalog.clone(),
        Some(handle.clone())).unwrap();
    assert!(selected.recheck().is_ok());
    catalog.revoke(&model).unwrap();
    assert!(selected.recheck().is_err());
    // A launch handle is bound to one employee.
    let mut other = ctx.record().clone();
    other.employee = EmployeeId([9; 16]);
    assert_eq!(handle.candidates(&RequestContext::with_live_verifier(other, Arc::new(Live)), "model_search", "model").err(),
        Some(VerificationError::BindingMismatch));
}

#[test]
fn f1_factory_uses_cli_executor_factories() {
    let home = home();
    let catalog = memory_catalog();
    catalog.register("home odoo", "odoo", ConnectorKind::SharedHome, &company()).unwrap();
    let (odoo, channel) = (executors("odoo"), executors("channel"));
    let assembled = gateway_factory(home.path(), &catalog, Some(company()), inputs(home.path(), google(),
        cli_executors(odoo.clone(), channel.clone()), Arc::default())).unwrap();
    let installed = assembled.factory.build(home.path(), AGENT, EMPLOYEE, &alice(), catalog.clone(), bindings()).unwrap();
    assert_eq!(odoo.bound.load(Ordering::SeqCst), 1, "the CLI Odoo factory is bound to this employee's provider");
    assert_eq!(channel.bound.load(Ordering::SeqCst), 0, "no channel route: nothing bound");
    assert!(!installed.refusals().contains_key("odoo"));
    assert_eq!(installed.refusals().get("channel"), Some(&ProviderRefusal::NoRoute));

    // The factories' declared coverage is the factory inventory.
    let incomplete = Arc::new(Executors { provider: "odoo", incomplete: true, bound: AtomicUsize::new(0) });
    let broken = gateway_factory(home.path(), &catalog, Some(company()), inputs(home.path(), google(),
        cli_executors(incomplete.clone(), executors("channel")), Arc::default())).unwrap();
    assert_eq!(broken.factory.inventory(), Err(VerificationError::Unavailable));
    assert_eq!(incomplete.bound.load(Ordering::SeqCst), 0);

    // Boot passes `connector_executor_factories()`; without CLI integration
    // that is the refusing default, so Odoo is never built.
    let defaults = connector_executor_factories();
    if !defaults.odoo.available() {
        let assembled = gateway_factory(home.path(), &catalog, Some(company()),
            inputs(home.path(), google(), defaults, Arc::default())).unwrap();
        let installed = assembled.factory.build(home.path(), AGENT, EMPLOYEE, &alice(), catalog.clone(), bindings()).unwrap();
        assert_eq!(installed.refusals().get("odoo"), Some(&ProviderRefusal::NoRoute));
    }
}

#[test]
fn f1_rebuild_on_route_revoke_and_relink() {
    let home = home();
    let catalog = memory_catalog();
    let first = catalog.register("alice model", "model", ConnectorKind::Personal, &alice()).unwrap();
    let reports = Arc::new(Reports::default());
    let lifecycle = Lifecycle::with(EMPLOYEE, alice());
    let stack = stack(home.path(), &catalog, bindings(), lifecycle.clone(), &reports);
    let counting = Arc::new(Counting { inner: factory(home.path(), &catalog).factory, builds: AtomicUsize::new(0) });
    stack.install_factory(counting.clone()).unwrap();
    let handle = stack.for_employee(AGENT, EMPLOYEE).unwrap();
    let ctx = context(alice(), "alice");
    let builds = || counting.builds.load(Ordering::SeqCst);

    assert_eq!(ids(handle.candidates(&ctx, "model_search", "model")), vec![first.as_str().to_owned()]);
    assert_eq!(ids(handle.candidates(&ctx, "model_search", "model")), vec![first.as_str().to_owned()]);
    assert_eq!(builds(), 1, "an unchanged route set reuses the build");

    // Revoke: the next use rebuilds; the provider is now a refusing adapter.
    catalog.revoke(&first).unwrap();
    assert_eq!(handle.candidates(&ctx, "model_search", "model").err(), Some(VerificationError::Unavailable));
    assert_eq!(builds(), 2);
    // Relink (same label, new id): rebuilt without a restart, never the old id.
    let relinked = catalog.register("alice model", "model", ConnectorKind::Personal, &alice()).unwrap();
    assert_ne!(relinked, first);
    assert_eq!(ids(handle.candidates(&ctx, "model_search", "model")), vec![relinked.as_str().to_owned()]);
    assert_eq!(builds(), 3);
    // A generation move rebuilds as well.
    catalog.advance(&relinked).unwrap();
    assert_eq!(ids(handle.candidates(&ctx, "model_search", "model")), vec![relinked.as_str().to_owned()]);
    assert_eq!(builds(), 4);
    // Another owner's route cannot reach this employee and changes nothing.
    let bob_model = catalog.register("bob model", "model", ConnectorKind::Personal, &bob()).unwrap();
    handle.candidates(&ctx, "model_search", "model").unwrap();
    assert_eq!(builds(), 4);

    // Lifecycle binding change: the launch observation forgets the build and
    // the next use builds for the new binding only.
    lifecycle.set(EMPLOYEE, bob());
    stack.observe_lifecycle(AGENT);
    assert_eq!(ids(handle.candidates(&context(bob(), "bob"), "model_search", "model")), vec![bob_model.as_str().to_owned()]);
    assert_eq!(builds(), 5);
    // The route-change refresh rebuilds without any call.
    catalog.revoke(&bob_model).unwrap();
    let before = reports.count();
    stack.refresh_all();
    assert_eq!((builds(), reports.count()), (6, before + 1));
    // A recreated employee under the label is never served the old build.
    lifecycle.set(EmployeeId([7; 16]), bob());
    stack.observe_lifecycle(AGENT);
    assert_eq!(handle.candidates(&context(bob(), "bob"), "model_search", "model").err(), Some(VerificationError::Revoked));
    assert!(stack.cached(AGENT, EMPLOYEE).is_none());
}

/// Live credential source standing in for the home OAuth provider.
struct Source { account: AccountId, provider: &'static str, calls: AtomicUsize,
    revoke_during: Option<Arc<ConnectorCatalog>> }
impl AccountCredentialProvider for Source {
    fn acquire<'a>(&'a self, account: &'a str, provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!((account, provider), (self.account.as_str(), self.provider));
            if let Some(catalog) = &self.revoke_during { catalog.revoke(&self.account).unwrap(); }
            AccountCredential::new(account.into(), provider.into(), "fixture-material".into(), u64::MAX)
        })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn f1_vault_material_dropped_on_revoke() {
    use crate::isolation_connector_lifecycle::{AdminActor, CredentialMaterial, provision, revoke_with};
    let home = crate::fs_safe::private_test_home().unwrap();
    let actor = AdminActor::verified("0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10").unwrap();
    let account = provision(home.path(), "notion", ConnectorKind::SharedHome, &company(), "team wiki", None, &actor).unwrap();
    let catalog = Arc::new(ConnectorCatalog::open(home.path()).unwrap());
    let generation = catalog.route(&account).unwrap().unwrap().generation;
    let vault = Arc::new(AccountCredentialVault::default());
    let source = Arc::new(Source { account: account.clone(), provider: "notion", calls: AtomicUsize::new(0), revoke_during: None });
    let credentials = VaultedCredentials::new(vault.clone(), source.clone(), catalog.clone());

    assert!(credentials.acquire_at(account.as_str(), "notion", generation).await.is_ok());
    assert!(credentials.acquire_at(account.as_str(), "notion", generation).await.is_ok());
    assert_eq!(source.calls.load(Ordering::SeqCst), 1, "the second acquisition is served from the vault");
    assert!(vault.acquire_at(account.as_str(), "notion", generation).await.is_ok());

    // C3 revoke drops exactly this vault's material after the catalog fence.
    revoke_with(home.path(), &account, &actor, Some(vault.as_ref() as &dyn CredentialMaterial)).unwrap();
    assert!(vault.acquire_at(account.as_str(), "notion", generation).await.is_err());
    assert!(credentials.acquire_at(account.as_str(), "notion", generation).await.is_err());
    assert_eq!(source.calls.load(Ordering::SeqCst), 1, "a revoked route never reaches the live source");

    // A revoke racing the acquisition never leaves material behind.
    let racing = provision(home.path(), "notion", ConnectorKind::SharedHome, &company(), "team wiki", None, &actor).unwrap();
    let generation = catalog.route(&racing).unwrap().unwrap().generation;
    let source = Arc::new(Source { account: racing.clone(), provider: "notion", calls: AtomicUsize::new(0),
        revoke_during: Some(catalog.clone()) });
    let credentials = VaultedCredentials::new(vault.clone(), source, catalog.clone());
    assert!(credentials.acquire_at(racing.as_str(), "notion", generation).await.is_err());
    assert!(vault.acquire_at(racing.as_str(), "notion", generation).await.is_err());
}

#[tokio::test]
async fn f1_vault_keyed_by_generation() {
    let catalog = memory_catalog();
    let account = catalog.register("alice mail", "google", ConnectorKind::Personal, &alice()).unwrap();
    let first = catalog.route(&account).unwrap().unwrap().generation;
    let vault = Arc::new(AccountCredentialVault::default());
    let material = || AccountCredential::new(account.as_str().into(), "google".into(), "fixture-material".into(), u64::MAX).unwrap();
    vault.install(material(), first).unwrap();
    assert!(vault.acquire_at(account.as_str(), "google", first).await.is_ok());
    assert!(vault.acquire(account.as_str(), "google").await.is_err(), "no generation, no material");
    assert_eq!(vault.install(material(), 0), Err(VerificationError::Invalid));

    // advance (re-consent / rotation): the old generation's material is
    // never served for the new one, nor kept once the old one is asked for.
    let second = catalog.advance(&account).unwrap();
    assert!(second > first);
    assert!(vault.acquire_at(account.as_str(), "google", second).await.is_err());
    let source = Arc::new(Source { account: account.clone(), provider: "google", calls: AtomicUsize::new(0), revoke_during: None });
    let credentials = VaultedCredentials::new(vault.clone(), source.clone(), catalog.clone());
    assert_eq!(credentials.acquire_at(account.as_str(), "google", first).await.err(), Some(VerificationError::BindingMismatch));
    assert!(vault.acquire_at(account.as_str(), "google", first).await.is_err(), "stale material was dropped");
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert!(credentials.acquire_at(account.as_str(), "google", second).await.is_ok());
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert!(vault.acquire_at(account.as_str(), "google", second).await.is_ok());
    // Never roll back to an older generation.
    assert_eq!(vault.install(material(), first), Err(VerificationError::BindingMismatch));
    assert!(vault.acquire_at(account.as_str(), "google", second).await.is_ok());
}

#[test]
fn f1_readiness_reports_every_build() {
    let home = home();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let catalog = Arc::new(ConnectorCatalog::new(Connection::open(&path).unwrap()).unwrap());
    // A database route without provisioned details (C6) ⇒ setup required.
    let database = catalog.register("alice db", "database", ConnectorKind::Personal, &alice()).unwrap();
    let reports = Arc::new(Reports::default());
    let lifecycle = Lifecycle::with(EMPLOYEE, alice());
    let stack = stack(home.path(), &catalog, bindings(), lifecycle.clone(), &reports);
    stack.install_factory(factory(home.path(), &catalog).factory).unwrap();

    stack.refresh_all();
    assert_eq!(reports.last(), (AGENT.to_owned(), EMPLOYEE, Ok(vec!["database"])), "boot prewarm reports");
    stack.current(AGENT, EMPLOYEE).unwrap();
    assert_eq!(reports.count(), 1, "a reused build is not a new build");
    catalog.advance(&database).unwrap();
    stack.current(AGENT, EMPLOYEE).unwrap();
    assert_eq!((reports.count(), reports.last().2), (2, Ok(vec!["database"])), "a rebuild reports");

    // Failed builds report too: recreated employee, then an unreadable catalog.
    lifecycle.set(EmployeeId([7; 16]), alice());
    stack.observe_lifecycle(AGENT);
    assert_eq!(stack.current(AGENT, EMPLOYEE).err(), Some(VerificationError::Revoked));
    assert_eq!(reports.last(), (AGENT.to_owned(), EMPLOYEE, Err(VerificationError::Revoked)));
    lifecycle.set(EMPLOYEE, alice());
    stack.current(AGENT, EMPLOYEE).unwrap();
    Connection::open(&path).unwrap().execute_batch("DROP TABLE isolation_connector_routes").unwrap();
    assert_eq!(stack.current(AGENT, EMPLOYEE).err(), Some(VerificationError::Unavailable));
    assert_eq!(reports.last(), (AGENT.to_owned(), EMPLOYEE, Err(VerificationError::Unavailable)));
    assert_eq!(reports.count(), 5);
    assert!(stack.cached(AGENT, EMPLOYEE).is_none(), "a failed build never leaves an older one in service");
}

#[test]
fn f1_missing_factory_setup_required() {
    let home = home();
    let catalog = memory_catalog();
    catalog.register("alice model", "model", ConnectorKind::Personal, &alice()).unwrap();
    let reports = Arc::new(Reports::default());
    let stack = stack(home.path(), &catalog, bindings(), Lifecycle::with(EMPLOYEE, alice()), &reports);
    // An incomplete factory is never installed.
    let incomplete = Arc::new(Executors { provider: "odoo", incomplete: true, bound: AtomicUsize::new(0) });
    let broken = gateway_factory(home.path(), &catalog, Some(company()), inputs(home.path(), google(),
        cli_executors(incomplete, executors("channel")), Arc::default())).unwrap();
    assert!(stack.install_factory(broken.factory).is_err());
    assert!(!stack.has_factory());

    // Refusing adapter for every connector tool (the inventory stays complete).
    let handle = stack.for_employee(AGENT, EMPLOYEE).unwrap();
    for policy in POLICIES.iter().filter(|policy| policy.route == ToolRoute::Connector) {
        assert!(LiveConnectorSelectionPolicy::supports(&handle, policy.name), "{}", policy.name);
        assert!(BoundConnectorExecutor::supports(&handle, policy.name), "{}", policy.name);
        assert!(LiveConnectorSelectionPolicy::supports(stack.as_ref(), policy.name));
    }
    assert!(!LiveConnectorSelectionPolicy::supports(&handle, "file_read"));
    let ctx = context(alice(), "alice");
    assert_eq!(handle.candidates(&ctx, "model_search", "model").err(), Some(VerificationError::Unavailable));
    assert_eq!(stack.candidates(&ctx, "model_search", "model").err(), Some(VerificationError::Unavailable),
        "the boot inventory object never serves");
    stack.refresh_all();
    // No build ⇒ no report: F3 then reads `connector_setup_required` for the employee.
    assert_eq!(reports.count(), 0);
}

#[test]
fn f1_google_bridge_uses_unique_employee_key() {
    let home = home();
    let config = |keys: &str| std::fs::write(home.path().join("config.toml"), format!("{ODOO_CONFIG}{keys}")).unwrap();
    let key = |suffix: &str, client: &str, scope: &str| format!(
        "[mcp_keys.ddc_dev_{suffix}]\nclient_id = '{client}'\nscopes = ['{scope}']\ncreated_at = '2026-11-01T00:00:00Z'\n");
    let first = "0123456789abcdef0123456789abcdef";
    let second = "fedcba9876543210fedcba9876543210";
    assert_eq!(employee_key_reference(home.path(), AGENT).unwrap(), None);
    config(&format!("{}{}{}", key(first, AGENT, "google:read"), key(second, "employee-b", "google:read"),
        key("00000000000000000000000000000000", AGENT, "odoo:read")));
    let reference = employee_key_reference(home.path(), AGENT).unwrap().unwrap();
    {
        use sha2::Digest;
        assert_eq!(reference.sha256, format!("{:x}", sha2::Sha256::digest(format!("ddc_dev_{first}").as_bytes())));
    }
    // Two Google keys for the employee: ambiguous, never pick one.
    config(&format!("{}{}", key(first, AGENT, "google:read"), key(second, AGENT, "google:write")));
    assert_eq!(employee_key_reference(home.path(), AGENT).unwrap(), None);
    std::fs::write(home.path().join("config.toml"), "mcp_keys = [").unwrap();
    assert_eq!(employee_key_reference(home.path(), AGENT).err(), Some(VerificationError::Unavailable));

    // No key while the employee has a Google route ⇒ refusing, setup required.
    config("");
    let catalog = memory_catalog();
    let bridges: Arc<dyn GoogleBridgeFactory> = Arc::new(EmployeeKeyGoogleBridges { users: None, turns: Arc::new(NoTurns) });
    let assembled = gateway_factory(home.path(), &catalog, Some(company()), inputs(home.path(), bridges.clone(),
        cli_executors(executors("odoo"), executors("channel")), Arc::default())).unwrap();
    let installed = assembled.factory.build(home.path(), AGENT, EMPLOYEE, &alice(), catalog.clone(), bindings()).unwrap();
    assert_eq!(installed.refusals().get("google"), Some(&ProviderRefusal::NoRoute));
    catalog.register("alice mail", "google", ConnectorKind::Personal, &alice()).unwrap();
    let installed = assembled.factory.build(home.path(), AGENT, EMPLOYEE, &alice(), catalog.clone(), bindings()).unwrap();
    assert_eq!(installed.refusals().get("google"), Some(&ProviderRefusal::SetupRequired));
    assert_eq!(installed.setup_required().collect::<Vec<_>>(), vec!["google"]);
    assert_eq!(installed.registry().candidates(&context(alice(), "alice"), "gmail_search", "google").err(),
        Some(VerificationError::Unavailable));
    // With the employee's key the bridge is installed.
    config(&key(first, AGENT, "google:read"));
    let installed = assembled.factory.build(home.path(), AGENT, EMPLOYEE, &alice(), catalog.clone(), bindings()).unwrap();
    assert!(!installed.refusals().contains_key("google"));
}
