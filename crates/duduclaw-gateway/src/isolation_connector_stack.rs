//! Protected-mode connector stack wired at gateway boot (v1.81 task F1).
//!
//! One [`ConnectorCatalog`] handle is shared by the binding authority (C2),
//! the connector factory, the details reader (C6, [`CatalogAccountDetails`])
//! and every selection made through [`EmployeeConnectors`]. The factory is
//! [`GatewayConnectorFactory`] assembled with D3's CLI executor factories,
//! D2's native home providers, the E2/E4 Google bridge and the process
//! [`AccountCredentialVault`] that boot also installs into the connector
//! lifecycle (C3/C4 drop material through it after their catalog fence).
//!
//! Providers are per employee and immutable ([`InstalledConnectorProviders`]).
//! [`EmployeeConnectorProviders`] keeps the current build per agent label and
//! rebuilds it whenever
//! - the employee's lifecycle binding or employee id changed (A2 read, never
//!   an argument or capability field), or
//! - its route set changed: the active routes of its own binding and (for
//!   Personal employees) the SharedHome routes of the installed home binding,
//!   with their generations. Provision, revoke, `advance` and a C4 relink
//!   all change it, so a recreated route gets new providers without a
//!   gateway restart, or
//! - (Shared employees, SR4 item 7) its Google tenant-source digest changed:
//!   a delegation-key re-import or a `service_mailboxes` / `tenant_scopes` /
//!   identity edit gets a new Google bridge instead of one stuck refusing on
//!   its pinned source.
//!
//! Every build, failed or not, is reported through `record_connector_build`
//! (F3). Fail closed: no factory ⇒ every connector tool is a refusing adapter
//! and nothing is reported (readiness shows `connector_setup_required`);
//! an unreadable catalog or lifecycle ⇒ `Unavailable` and the cached build is
//! dropped; a build error is never replaced by an older build.
use std::{collections::BTreeMap, future::Future, path::{Path, PathBuf}, pin::Pin,
    sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}}};
use duduclaw_isolation::{capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_mcp_boundary::{bindings::BindingAuthority, tool_policy::{AuthorizedToolCall, POLICIES, Route as ToolRoute},
    RequestContext, VerificationError};
use serde_json::Value;
use crate::google_identity::isolation_bridge::GoogleIsolationBridge;
use crate::isolation_connector_boot::{ConnectorExecutorFactories, ConnectorProviderFactory, GatewayConnectorFactory,
    GatewayConnectorFactoryParts, GoogleBridgeFactory, InstalledConnectorProviders, SearchProviderSource};
use crate::isolation_connector_clients::AccountCredentialVault;
use crate::isolation_connector_details::CatalogAccountDetails;
use crate::isolation_connectors::{BoundConnectorExecutor, ConnectorCatalog, ConnectorKind,
    LiveConnectorSelectionPolicy, ProductConnectorAccount, SelectedConnector, provider_for_tool};

/// Every connector-routed tool of the policy table.
fn connector_tool(tool: &str) -> bool {
    provider_for_tool(tool).is_some()
        && POLICIES.iter().any(|policy| policy.route == ToolRoute::Connector && policy.name == tool)
}

/// The employee's lifecycle identity. Production: [`LifecycleBindings`].
pub trait EmployeeBindingSource: Send + Sync {
    /// Current employee id and binding of `agent`. Unreadable ⇒ `Unavailable`,
    /// any other lifecycle refusal ⇒ `Revoked`.
    fn current(&self, agent: &str) -> Result<(EmployeeId, EmployeeBinding), VerificationError>;
    /// Labels of the live confirmed employees (boot prewarm / refresh).
    fn labels(&self) -> Result<Vec<String>, VerificationError>;
}

/// A2 lifecycle with the root ReadBack (blocking SQLite + root IPC).
pub struct LifecycleBindings { home: PathBuf }
impl LifecycleBindings {
    pub fn new(home: PathBuf) -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        Ok(Self { home })
    }
}
impl EmployeeBindingSource for LifecycleBindings {
    fn current(&self, agent: &str) -> Result<(EmployeeId, EmployeeBinding), VerificationError> {
        use crate::isolation_lifecycle::LifecycleError;
        crate::isolation_lifecycle::read_lifecycle(&self.home, agent)
            .map(|lifecycle| (lifecycle.employee, lifecycle.binding))
            .map_err(|error| if matches!(error, LifecycleError::Unavailable) { VerificationError::Unavailable }
                else { VerificationError::Revoked })
    }
    fn labels(&self) -> Result<Vec<String>, VerificationError> {
        let entries = crate::isolation_lifecycle::confirmed_entries(&self.home, crate::isolation_readiness::reasons::MAX_EMPLOYEES)
            .map_err(|_| VerificationError::Unavailable)?;
        Ok(entries.into_iter().filter(|entry| !entry.disabled).map(|entry| entry.label).collect())
    }
}

/// Receives every build outcome. Production: [`readiness_report`].
pub type BuildReport = Arc<dyn Fn(&Path, &str, EmployeeId, Result<&InstalledConnectorProviders, VerificationError>)
    + Send + Sync>;
/// F3's `record_connector_build`.
pub fn readiness_report() -> BuildReport {
    Arc::new(|home: &Path, agent: &str, employee: EmployeeId, outcome: Result<&InstalledConnectorProviders, VerificationError>|
        crate::isolation_readiness::record_connector_build(home, agent, employee, outcome))
}

/// Active routes (account, generation) that can reach an employee's build.
type RouteSet = Vec<(String, u64)>;
fn route_set(catalog: &ConnectorCatalog, binding: &EmployeeBinding, home: Option<&EmployeeBinding>)
    -> Result<RouteSet, VerificationError> {
    let mut routes: RouteSet = catalog.active_routes_for(binding)?.into_iter()
        .map(|route| (route.account.as_str().to_owned(), route.generation)).collect();
    // A Shared employee of the home tenant already read the home routes above.
    if let (EmployeeBinding::Personal(_), Some(home)) = (binding, home) {
        routes.extend(catalog.active_routes_for(home)?.into_iter().filter(|route| route.kind == ConnectorKind::SharedHome)
            .map(|route| (route.account.as_str().to_owned(), route.generation)));
    }
    routes.sort();
    Ok(routes)
}

/// SR4 item 7: the employee's Google tenant-source state as one comparable
/// key — its pinned digest (subject, service mailboxes, tenant scopes, key
/// material), or the reason there is none. The Google bridge pins the source
/// per tool for its lifetime, so a key re-import or a `service_mailboxes` /
/// `tenant_scopes` edit must replace the build, else the bridge stays refused
/// until a restart. Tenant sources are Shared-only: a Personal employee's key
/// is constant (no read). Blocking file reads.
fn tenant_source_key(home: &Path, agent: &str, binding: &EmployeeBinding) -> String {
    use crate::google_identity::tenant_source::{self, TenantSourceError};
    if !matches!(binding, EmployeeBinding::Shared(_)) { return String::new(); }
    match tenant_source::read(home, agent) {
        Ok(Some(source)) => source.digest(),
        Ok(None) => "none".into(),
        Err(TenantSourceError::Unavailable) => "unavailable".into(),
        Err(TenantSourceError::Refused) => "refused".into(),
    }
}

struct Installed {
    employee: EmployeeId,
    binding: EmployeeBinding,
    routes: RouteSet,
    /// SR4 item 7: [`tenant_source_key`] the build was made for.
    tenant_source: String,
    providers: Arc<InstalledConnectorProviders>,
}

/// Per-employee provider registry of one gateway epoch (see module docs).
pub struct EmployeeConnectorProviders {
    home: PathBuf,
    catalog: Arc<ConnectorCatalog>,
    bindings: Arc<dyn BindingAuthority>,
    home_binding: Option<EmployeeBinding>,
    lifecycle: Arc<dyn EmployeeBindingSource>,
    report: BuildReport,
    factory: OnceLock<Arc<dyn ConnectorProviderFactory>>,
    installed: Mutex<BTreeMap<String, Installed>>,
    refresh_pending: AtomicBool,
}
impl EmployeeConnectorProviders {
    /// `home_binding` must be the factory's (the installed home's
    /// `Shared(tenant)`). It is read once at boot: an installation tenant
    /// change makes every lifecycle read and every C2 home check refuse, so a
    /// stale value can never admit a route.
    pub fn new(home: PathBuf, catalog: Arc<ConnectorCatalog>, bindings: Arc<dyn BindingAuthority>,
        home_binding: Option<EmployeeBinding>, lifecycle: Arc<dyn EmployeeBindingSource>, report: BuildReport)
        -> Result<Self, VerificationError> {
        if !home.is_absolute() || home_binding.as_ref().is_some_and(|binding| !matches!(binding, EmployeeBinding::Shared(_))) {
            return Err(VerificationError::Invalid);
        }
        Ok(Self { home, catalog, bindings, home_binding, lifecycle, report, factory: OnceLock::new(),
            installed: Mutex::new(BTreeMap::new()), refresh_pending: AtomicBool::new(false) })
    }
    /// Install the factory once. Its static inventory must pass first;
    /// otherwise no factory is installed and every tool stays refusing.
    pub fn install_factory(&self, factory: Arc<dyn ConnectorProviderFactory>) -> Result<(), VerificationError> {
        factory.inventory()?;
        self.factory.set(factory).map_err(|_| VerificationError::Unavailable)
    }
    pub fn has_factory(&self) -> bool { self.factory.get().is_some() }
    /// The one catalog handle of this stack.
    pub fn catalog(&self) -> &Arc<ConnectorCatalog> { &self.catalog }
    pub fn home(&self) -> &Path { &self.home }

    /// Live selection/execution handle for one launched employee. No I/O.
    pub fn for_employee(self: &Arc<Self>, agent: &str, employee: EmployeeId) -> Result<EmployeeConnectors, VerificationError> {
        if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        Ok(EmployeeConnectors { stack: self.clone(), agent: agent.to_owned(), employee })
    }

    /// The employee's current providers; rebuilds when its lifecycle binding
    /// or route set changed. Blocking (catalog, and on rebuild the lifecycle
    /// root ReadBack and provider construction).
    pub fn current(&self, agent: &str, employee: EmployeeId) -> Result<Arc<InstalledConnectorProviders>, VerificationError> {
        // Missing factory: refusing adapter, no build and so no report.
        let factory = self.factory.get().ok_or(VerificationError::Unavailable)?.clone();
        let cached = self.installed.lock().map_err(|_| VerificationError::Unavailable)?.get(agent)
            .filter(|installed| installed.employee == employee)
            .map(|installed| (installed.binding.clone(), installed.routes.clone(), installed.tenant_source.clone(),
                installed.providers.clone()));
        if let Some((binding, routes, tenant_source, providers)) = cached {
            if route_set(&self.catalog, &binding, self.home_binding.as_ref()).ok().as_ref() == Some(&routes)
                && tenant_source_key(&self.home, agent, &binding) == tenant_source {
                return Ok(providers);
            }
        }
        self.rebuild(factory.as_ref(), agent, employee)
    }

    /// Re-read the employee's lifecycle and forget a build whose employee id
    /// or binding no longer matches (or whose lifecycle cannot be read); the
    /// next use rebuilds. Never builds. Called at every launch (blocking).
    pub fn observe_lifecycle(&self, agent: &str) {
        let current = self.lifecycle.current(agent);
        self.observe(agent, &current);
    }
    fn observe(&self, agent: &str, current: &Result<(EmployeeId, EmployeeBinding), VerificationError>) {
        let Ok(mut installed) = self.installed.lock() else { return };
        let stale = installed.get(agent).is_some_and(|build| current.as_ref().ok()
            .is_none_or(|(employee, binding)| *employee != build.employee || *binding != build.binding));
        if stale { installed.remove(agent); }
    }

    /// The last successful build for exactly this employee, without I/O.
    fn cached(&self, agent: &str, employee: EmployeeId) -> Option<Arc<InstalledConnectorProviders>> {
        self.installed.lock().ok()?.get(agent).filter(|installed| installed.employee == employee)
            .map(|installed| installed.providers.clone())
    }

    fn rebuild(&self, factory: &dyn ConnectorProviderFactory, agent: &str, employee: EmployeeId)
        -> Result<Arc<InstalledConnectorProviders>, VerificationError> {
        let outcome = (|| {
            let (current, binding) = self.lifecycle.current(agent)?;
            // A recreated label is another employee: never serve it here.
            if current != employee { return Err(VerificationError::Revoked); }
            let routes = route_set(&self.catalog, &binding, self.home_binding.as_ref())?;
            let tenant_source = tenant_source_key(&self.home, agent, &binding);
            let providers = factory.build(&self.home, agent, employee, &binding, self.catalog.clone(), self.bindings.clone())?;
            // A change while building: refuse rather than cache a set for a
            // route or tenant-source state that no longer exists (the next
            // use rebuilds).
            if route_set(&self.catalog, &binding, self.home_binding.as_ref())? != routes
                || tenant_source_key(&self.home, agent, &binding) != tenant_source {
                return Err(VerificationError::Unavailable);
            }
            Ok(Installed { employee, binding, routes, tenant_source, providers: Arc::new(providers) })
        })();
        (self.report)(&self.home, agent, employee, outcome.as_ref().map(|installed| installed.providers.as_ref()).map_err(|error| *error));
        let mut slot = self.installed.lock().map_err(|_| VerificationError::Unavailable)?;
        match outcome {
            Ok(installed) => {
                let providers = installed.providers.clone();
                slot.insert(agent.to_owned(), installed);
                Ok(providers)
            }
            Err(error) => {
                // Never keep serving an older build after a failed one.
                slot.remove(agent);
                Err(error)
            }
        }
    }

    /// Build (or confirm) every listed employee's providers: boot prewarm and
    /// the refresh after a catalog change. Errors are reported per employee;
    /// an employee whose lifecycle cannot be read is skipped (F3 reports the
    /// lifecycle reason itself).
    pub fn refresh(&self, labels: &[String]) {
        if !self.has_factory() { return; }
        for label in labels {
            let current = self.lifecycle.current(label);
            self.observe(label, &current);
            if let Ok((employee, _)) = current { let _ = self.current(label, employee); }
        }
        // Drop builds of employees that are no longer listed.
        if let Ok(mut installed) = self.installed.lock() { installed.retain(|label, _| labels.contains(label)); }
    }
    pub fn refresh_all(&self) {
        match self.lifecycle.labels() {
            Ok(labels) => self.refresh(&labels),
            // Unreadable listing: keep nothing that could be stale.
            Err(_) => if let Ok(mut installed) = self.installed.lock() { installed.clear(); },
        }
    }
    /// Non-blocking: queue one [`refresh_all`](Self::refresh_all) pass on the
    /// blocking pool (or a thread outside a runtime). Changes while a pass is
    /// queued are covered by it; a change after it started queues another.
    pub fn schedule_refresh(self: &Arc<Self>) {
        if self.refresh_pending.swap(true, Ordering::AcqRel) { return; }
        let stack = self.clone();
        let pass = move || {
            stack.refresh_pending.store(false, Ordering::Release);
            stack.refresh_all();
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => { runtime.spawn_blocking(pass); }
            Err(_) => {
                if std::thread::Builder::new().name("isolation-connector-refresh".into()).spawn(pass).is_err() {
                    self.refresh_pending.store(false, Ordering::Release);
                }
            }
        }
    }
}

/// Boot-level inventory object (`DispatcherFactory::verify_producers`): it
/// declares every connector tool and serves none. Calls go through the
/// per-launch [`EmployeeConnectors`].
impl LiveConnectorSelectionPolicy for EmployeeConnectorProviders {
    fn supports(&self, tool: &str) -> bool { connector_tool(tool) }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
    fn candidates_for_call(&self, _: &RequestContext, _: &str, _: &str, _: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}
impl BoundConnectorExecutor for EmployeeConnectorProviders {
    fn supports(&self, tool: &str) -> bool { connector_tool(tool) }
    fn execute<'a>(&'a self, _: SelectedConnector, _: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
}

/// One launched employee's view of the stack. Every selection and recheck
/// reads the current build (rebuilding after a route/binding change), so a
/// running employee follows provision/revoke/relink without a restart; the
/// capability record must name exactly this employee.
pub struct EmployeeConnectors { stack: Arc<EmployeeConnectorProviders>, agent: String, employee: EmployeeId }
impl EmployeeConnectors {
    pub fn agent(&self) -> &str { &self.agent }
    pub fn employee(&self) -> EmployeeId { self.employee }
}
impl LiveConnectorSelectionPolicy for EmployeeConnectors {
    fn supports(&self, tool: &str) -> bool { connector_tool(tool) }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        self.candidates_for_call(context, tool, provider, &Value::Object(Default::default()))
    }
    fn candidates_for_call(&self, context: &RequestContext, tool: &str, provider: &str, arguments: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        if context.record().employee != self.employee { return Err(VerificationError::BindingMismatch); }
        self.stack.current(&self.agent, self.employee)?.registry().candidates_for_call(context, tool, provider, arguments)
    }
}
impl BoundConnectorExecutor for EmployeeConnectors {
    fn supports(&self, tool: &str) -> bool { connector_tool(tool) }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            if selected.context().record().employee != self.employee || call.context().record().employee != self.employee {
                return Err(VerificationError::BindingMismatch);
            }
            // The selection was made (and is rechecked) through `current`;
            // execution never builds on the async path. A build replaced
            // since then is still fenced by the selection's own rechecks.
            let providers = self.stack.cached(&self.agent, self.employee).ok_or(VerificationError::Unavailable)?;
            providers.registry().execute(selected, call).await
        })
    }
}

/// Production Google bridge inputs (E2/E4): the bridge authorizes each tool
/// against the employee's own MCP key reference (a usable `mcp_keys.db` key,
/// or — only in the RM-134 legacy window — a `[mcp_keys]` entry, whose
/// `client_id` is the agent and that carries a `google:` scope). No key, or
/// more than one, means no bridge (`Ok(None)` ⇒ setup required while the
/// employee has a Google route); the gateway never picks among several.
pub struct EmployeeKeyGoogleBridges {
    pub users: Option<Arc<duduclaw_auth::UserDb>>,
    pub turns: Arc<dyn crate::isolation_egress::RunTurnResolver>,
}
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
/// The employee's unique Google-scoped legacy MCP key reference (digest only).
pub(crate) fn employee_key_reference(home: &Path, agent: &str)
    -> Result<Option<duduclaw_core::google_identity::CredentialRef>, VerificationError> {
    use duduclaw_core::google_identity::{CredentialKind, CredentialRef};
    let google = |scope: &str| scope.starts_with("google:");
    // RM-134: usable dashboard keys in the hashed store first; an unreadable
    // store means no decision (Unavailable), never "no key".
    let mut found: Vec<String> = duduclaw_core::mcp_key_store::usable_keys(home, chrono::Utc::now())
        .map_err(|_| VerificationError::Unavailable)?
        .into_iter()
        .filter(|key| key.client_id == agent && key.scopes.iter().any(|s| google(s)))
        .map(|key| key.key_hash)
        .collect();
    let path = home.join("config.toml");
    let config = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(VerificationError::Unavailable),
        Ok(meta) if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES => return Err(VerificationError::Unavailable),
        Ok(_) => Some(zeroize::Zeroizing::new(std::fs::read_to_string(&path).map_err(|_| VerificationError::Unavailable)?)),
    };
    if let Some(raw) = config {
        let doc: toml::Table = toml::from_str(&raw).map_err(|_| VerificationError::Unavailable)?;
        if let Some(keys) = doc.get("mcp_keys") {
            let keys = keys.as_table().ok_or(VerificationError::Unavailable)?;
            for (key, value) in keys {
                let Some(entry) = value.as_table() else { continue };
                if entry.get("client_id").and_then(toml::Value::as_str) != Some(agent)
                    || !duduclaw_core::mcp_key_store::is_key_format(key) {
                    continue;
                }
                let has_google = entry.get("scopes").and_then(toml::Value::as_array).is_some_and(|scopes|
                    scopes.iter().any(|scope| scope.as_str().is_some_and(google)));
                if !has_google { continue; }
                let hash = duduclaw_core::mcp_key_store::digest(key);
                // RM-134: a config entry counts only in the documented legacy
                // window (no store yet, or recorded as pending legacy). A key
                // the store knows is decided there (and was already counted
                // above if it is usable); an unknown one is ignored.
                match duduclaw_core::mcp_key_store::resolve_hash(home, &hash)
                    .map_err(|_| VerificationError::Unavailable)? {
                    duduclaw_core::mcp_key_store::Authority::Legacy(_) => found.push(hash),
                    _ => continue,
                }
            }
        }
    }
    // No key, or more than one: no bridge (the gateway never picks).
    if found.len() != 1 { return Ok(None); }
    Ok(found.pop().map(|sha256| CredentialRef { kind: CredentialKind::Legacy, sha256 }))
}
impl GoogleBridgeFactory for EmployeeKeyGoogleBridges {
    fn bridge(&self, home: &Path, agent: &str, employee: EmployeeId, catalog: Arc<ConnectorCatalog>)
        -> Result<Arc<GoogleIsolationBridge>, VerificationError> {
        self.bridge_for(home, agent, employee, catalog)?.ok_or(VerificationError::Unavailable)
    }
    fn bridge_for(&self, home: &Path, agent: &str, employee: EmployeeId, catalog: Arc<ConnectorCatalog>)
        -> Result<Option<Arc<GoogleIsolationBridge>>, VerificationError> {
        let Some(reference) = employee_key_reference(home, agent)? else { return Ok(None) };
        GoogleIsolationBridge::new(home.to_path_buf(), agent.to_owned(), employee, self.users.clone(), reference,
            catalog, self.turns.clone()).map(|bridge| Some(Arc::new(bridge)))
    }
}

/// Non-catalog inputs of the gateway factory.
pub struct ConnectorStackInputs {
    pub google: Arc<dyn GoogleBridgeFactory>,
    pub search: Arc<dyn SearchProviderSource>,
    pub hardware: duduclaw_inference::types::HardwareInfo,
    pub computer_sessions: Arc<crate::computer_use_sessions::ComputerUseSessions>,
    /// Production: `isolation_connector_boot::connector_executor_factories()`.
    pub executors: ConnectorExecutorFactories,
    /// The process vault also installed into the connector lifecycle.
    pub vault: Arc<AccountCredentialVault>,
}

/// The assembled factory plus the details reader built over the same handle.
pub struct ConnectorStack {
    pub factory: Arc<GatewayConnectorFactory>,
    pub details: Arc<CatalogAccountDetails>,
}

/// Assemble [`GatewayConnectorFactory`] over exactly `catalog`: the C6
/// details reader reads it, and the factory passes it to every build (and so
/// to the Google bridge, native home providers and selections).
pub fn gateway_factory(home: &Path, catalog: &Arc<ConnectorCatalog>, home_binding: Option<EmployeeBinding>,
    inputs: ConnectorStackInputs) -> Result<ConnectorStack, VerificationError> {
    let details = Arc::new(CatalogAccountDetails::new(home.to_path_buf(), catalog.clone())?);
    let ConnectorStackInputs { google, search, hardware, computer_sessions, executors, vault } = inputs;
    let factory = GatewayConnectorFactory::new(GatewayConnectorFactoryParts {
        home_binding, details: details.clone(), google, search, hardware, computer_sessions,
        odoo_executors: executors.odoo, channel_executors: executors.channel, credential_vault: vault,
    })?;
    Ok(ConnectorStack { factory: Arc::new(factory), details })
}

/// The installed home's `Shared(tenant)` binding: the users.db installation
/// tenant, which every lifecycle read requires to equal the `apply.db` pin
/// and C2 requires of every home route. `Unavailable` when unreadable.
pub fn installed_home_binding(home: &Path) -> Result<EmployeeBinding, VerificationError> {
    crate::isolation_lifecycle::connector_route_binding(home, None).map_err(|_| VerificationError::Unavailable)
}

#[cfg(test)]
#[path = "isolation_connector_stack_tests.rs"]
mod tests;
