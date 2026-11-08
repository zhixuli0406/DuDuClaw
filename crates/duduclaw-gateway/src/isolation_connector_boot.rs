//! Formal, immutable connector assembly for protected gateway boot. All tool
//! registrations are derived from the policy table, with no hand-maintained
//! partial list and no generic-vault substitution for Google's sealed owner.
//!
//! Providers are per employee (v1.81 D1). [`ConnectorProviderFactory::build`]
//! derives every account list from that employee's *active* catalog routes
//! ([`plan_connector_accounts`]); nothing is read from the gateway's legacy
//! home account, configuration or credential files to decide which accounts
//! exist. A provider with no route for the employee is installed as an
//! explicit refusing adapter, so the registry stays complete while every call
//! for that provider refuses before any credential is touched.
use std::{collections::{BTreeMap, BTreeSet}, future::Future, path::{Path, PathBuf}, pin::Pin, sync::{Arc, OnceLock}};
use duduclaw_isolation::{capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_mcp_boundary::{bindings::BindingAuthority, tool_policy::{AuthorizedToolCall, POLICIES, Route as ToolRoute},
    RequestContext, VerificationError};
use serde_json::Value;
use crate::{google_identity::isolation_bridge::GoogleIsolationBridge,
    isolation_connectors::{AccountId, BoundConnectorExecutor, ConnectorCatalog, ConnectorKind,
        LiveConnectorSelectionPolicy, ProductConnectorAccount, Route, SelectedConnector, provider_for_tool},
    isolation_connector_clients::{AccountCredentialProvider, NativeConnectorExecutor},
    isolation_connector_database::DatabaseConnectorProvider,
    isolation_connector_channel::{ChannelAccount, ChannelConnectorProvider},
    isolation_connector_model::ModelConnectorProvider,
    isolation_connector_computer::ComputerConnectorProvider,
    isolation_connector_native_home::{GithubPolicy, HomeOAuthCredentialProvider, NotionPolicy},
    isolation_connector_odoo::OdooConnectorProvider,
    isolation_connector_registry::ConnectorProviderRegistry,
    isolation_connector_search::SearchConnectorProvider,
    isolation_resources::GatewayResourceResolver};

/// CLI-owned business handlers cannot be imported by the gateway. Their boot
/// adapters retain the concrete gateway policy and an exact bound executor.
/// `None` is never an omission: it installs an explicit adapter that refuses
/// every tool of that provider (`Unavailable`), because the employee has no
/// active catalog route for it.
pub struct ConnectorBootProviders {
    pub google: Arc<GoogleIsolationBridge>,
    pub notion_policy: Option<Arc<dyn LiveConnectorSelectionPolicy>>,
    pub github_policy: Option<Arc<dyn LiveConnectorSelectionPolicy>>,
    pub native_credentials: Arc<dyn AccountCredentialProvider>,
    pub database: Option<Arc<DatabaseConnectorProvider>>,
    /// Gateway policy plus the CLI executor bound to that same provider.
    pub odoo: Option<(Arc<OdooConnectorProvider>, Arc<dyn BoundConnectorExecutor>)>,
    /// Gateway policy plus the CLI executor bound to that same provider.
    pub channel: Option<(Arc<ChannelConnectorProvider>, Arc<dyn BoundConnectorExecutor>)>,
    pub model: Option<Arc<ModelConnectorProvider>>,
    pub computer: Option<Arc<ComputerConnectorProvider>>,
    pub search: Option<Arc<SearchConnectorProvider>>,
}

type Adapter = (Arc<dyn LiveConnectorSelectionPolicy>, Arc<dyn BoundConnectorExecutor>);
fn both<T: LiveConnectorSelectionPolicy + BoundConnectorExecutor + 'static>(provider: Arc<T>) -> Adapter {
    (provider.clone() as Arc<dyn LiveConnectorSelectionPolicy>, provider as Arc<dyn BoundConnectorExecutor>)
}

/// Installed for a provider the employee has no active route for. Supports
/// exactly that provider's tools (so the inventory is complete and no other
/// adapter can be reached for them) and refuses every selection and call.
struct UnprovisionedProvider(&'static str);
impl LiveConnectorSelectionPolicy for UnprovisionedProvider {
    fn supports(&self, tool: &str) -> bool { provider_for_tool(tool) == Some(self.0) }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
    fn candidates_for_call(&self, _: &RequestContext, _: &str, _: &str, _: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}
impl BoundConnectorExecutor for UnprovisionedProvider {
    fn supports(&self, tool: &str) -> bool { provider_for_tool(tool) == Some(self.0) }
    fn execute<'a>(&'a self, _: SelectedConnector, _: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
}
fn unprovisioned(provider: &'static str) -> Adapter { both(Arc::new(UnprovisionedProvider(provider))) }

/// Keep this installed value for the employee's gateway epoch. Catalog and
/// binding authority are the already-provisioned stores owned by the lifecycle
/// host; boot never manufactures an account grant from credentials it happens
/// to find.
pub struct InstalledConnectorProviders {
    registry: Arc<ConnectorProviderRegistry>,
    resolver: Arc<GatewayResourceResolver>,
    home: PathBuf,
    agent: String,
    employee: EmployeeId,
}
impl InstalledConnectorProviders {
    pub fn assemble(home: PathBuf, agent: String, employee: EmployeeId,
        bindings: Arc<dyn BindingAuthority>, catalog: Arc<ConnectorCatalog>,
        providers: ConnectorBootProviders) -> Result<Self, VerificationError> {
        let native = Arc::new(NativeConnectorExecutor::new(home.clone(), agent.clone(), providers.native_credentials)?);
        let native_with = |policy: Option<Arc<dyn LiveConnectorSelectionPolicy>>| -> Option<Adapter> {
            policy.map(|policy| (policy, native.clone() as Arc<dyn BoundConnectorExecutor>))
        };
        let mut registry = ConnectorProviderRegistry::default();
        for (provider, adapter) in [
            ("google", Some(both(providers.google))),
            ("notion", native_with(providers.notion_policy)),
            ("github", native_with(providers.github_policy)),
            ("database", providers.database.map(both)),
            ("odoo", providers.odoo.map(|(policy, executor)|
                (policy as Arc<dyn LiveConnectorSelectionPolicy>, executor))),
            ("channel", providers.channel.map(|(policy, executor)|
                (policy as Arc<dyn LiveConnectorSelectionPolicy>, executor))),
            ("model", providers.model.map(both)),
            ("computer", providers.computer.map(both)),
            ("search", providers.search.map(both)),
        ] {
            let (policy, executor) = adapter.unwrap_or_else(|| unprovisioned(provider));
            registry.register_provider(provider, policy, executor)?;
        }
        registry.require_complete()?;
        let registry = Arc::new(registry);
        let resolver = Arc::new(GatewayResourceResolver::new(home.clone(), employee, agent.clone(), bindings)?
            .with_connector_catalog(catalog).with_connector_selection_policy(registry.clone()));
        Ok(Self { registry, resolver, home, agent, employee })
    }
    pub fn resolver(&self) -> Arc<GatewayResourceResolver> { self.resolver.clone() }
    pub fn registry(&self) -> Arc<ConnectorProviderRegistry> { self.registry.clone() }
    pub fn verify_identity(&self, home: &std::path::Path, agent: &str, employee: EmployeeId)
        -> Result<(), VerificationError> {
        if self.home != home || self.agent != agent || self.employee != employee { return Err(VerificationError::BindingMismatch); }
        self.registry.require_complete()
    }
}

/// Builds one employee's immutable connector providers.
pub trait ConnectorProviderFactory: Send + Sync {
    /// Static readiness: every connector tool has a builder or an explicit
    /// refusing adapter, and each available executor covers its provider.
    /// `build` repeats this first.
    fn inventory(&self) -> Result<(), VerificationError>;
    /// `binding` must be the employee's lifecycle binding (never an argument
    /// or capability field). Account lists come only from `catalog`'s active
    /// routes; any store error refuses the whole build.
    fn build(&self, home: &Path, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        catalog: Arc<ConnectorCatalog>, bindings: Arc<dyn BindingAuthority>)
        -> Result<InstalledConnectorProviders, VerificationError>;
}

/// Every provider the factory builds; must equal the connector policy table.
pub const FACTORY_PROVIDERS: [&str; 9] =
    ["google", "notion", "github", "database", "odoo", "channel", "model", "computer", "search"];
/// Matches the per-provider bound of the existing provider constructors.
pub const MAX_ACCOUNTS_PER_PROVIDER: usize = 128;

/// Route kinds a provider's product adapter can actually serve. Notion/GitHub
/// are home-wide integrations; search is a home service; the Google bridge
/// has Personal and Home OAuth sources only (tenant sources are E4).
fn provider_admits(provider: &str, kind: ConnectorKind) -> bool {
    match provider {
        "google" => matches!(kind, ConnectorKind::Personal | ConnectorKind::SharedHome),
        "notion" | "github" | "search" => kind == ConnectorKind::SharedHome,
        "database" | "computer" | "channel" | "odoo" | "model" => true,
        _ => false,
    }
}

/// One catalog route as read during planning. `via_home` marks a SharedHome
/// route reached through the installed home binding rather than the
/// employee's own binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAccount {
    pub route: Route,
    pub via_home: bool,
}
impl PlannedAccount {
    /// The product selection for exactly this route: Personal routes keep
    /// their owner, shared routes their tenant.
    pub fn product(&self) -> ProductConnectorAccount {
        let account = self.route.account.as_str().to_owned();
        match &self.route.binding {
            EmployeeBinding::Personal(owner) => ProductConnectorAccount::Personal { account, owner: owner.as_str().to_owned() },
            EmployeeBinding::Shared(tenant) => ProductConnectorAccount::SharedHome { account, tenant: tenant.as_str().to_owned() },
        }
    }
}

/// Per-provider accounts for one employee, derived only from catalog routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorAccountPlan { accounts: BTreeMap<&'static str, Vec<PlannedAccount>> }
impl ConnectorAccountPlan {
    pub fn accounts(&self, provider: &str) -> &[PlannedAccount] {
        self.accounts.get(provider).map(Vec::as_slice).unwrap_or(&[])
    }
    /// For single-account providers: the employee's own route if there is
    /// one, otherwise the explicit home route. Two routes in the chosen tier
    /// are ambiguous and refuse (never pick one).
    pub fn single(&self, provider: &str) -> Result<Option<&PlannedAccount>, VerificationError> {
        let accounts = self.accounts(provider);
        for via_home in [false, true] {
            let mut tier = accounts.iter().filter(|account| account.via_home == via_home);
            let first = tier.next();
            if tier.next().is_some() { return Err(VerificationError::Invalid); }
            if first.is_some() { return Ok(first); }
        }
        Ok(None)
    }
    fn admit(&mut self, route: Route, expected: &EmployeeBinding, via_home: bool, seen: &mut BTreeSet<AccountId>)
        -> Result<(), VerificationError> {
        // `active_routes_for` already guarantees these; disagreement is damage.
        if !route.active || &route.binding != expected || !route.kind.admits(&route.binding)
            || !seen.insert(route.account.clone()) {
            return Err(VerificationError::Unavailable);
        }
        if !provider_admits(route.provider, route.kind) { return Ok(()); }
        let accounts = self.accounts.get_mut(route.provider).ok_or(VerificationError::Unavailable)?;
        if accounts.len() >= MAX_ACCOUNTS_PER_PROVIDER { return Err(VerificationError::Invalid); }
        accounts.push(PlannedAccount { route, via_home });
        Ok(())
    }
}

/// The only source of per-employee connector accounts.
///
/// - Own routes: `active_routes_for(binding)` of kind Personal (Personal
///   binding: exactly that owner) or SharedTenant (Shared binding: exactly
///   that tenant). A SharedHome route is never taken from the own binding.
/// - Home routes: when `home_binding` (the installed home's `Shared(tenant)`)
///   is set and the employee is Personal or is Shared with that same tenant,
///   `active_routes_for(home_binding)` of kind SharedHome. Whether a Personal
///   employee may use one is still decided per call by the binding authority's
///   product-home check; this only lists it.
/// - Then filtered by what each provider can serve ([`provider_admits`]).
///
/// Revoked routes, other owners/tenants and other providers never appear.
/// Any catalog error refuses.
pub fn plan_connector_accounts(catalog: &ConnectorCatalog, binding: &EmployeeBinding,
    home_binding: Option<&EmployeeBinding>) -> Result<ConnectorAccountPlan, VerificationError> {
    let mut plan = ConnectorAccountPlan {
        accounts: FACTORY_PROVIDERS.iter().map(|provider| (*provider, Vec::new())).collect(),
    };
    let mut seen = BTreeSet::new();
    let own_kind = match binding {
        EmployeeBinding::Personal(_) => ConnectorKind::Personal,
        EmployeeBinding::Shared(_) => ConnectorKind::SharedTenant,
    };
    for route in catalog.active_routes_for(binding)? {
        if route.kind == own_kind { plan.admit(route, binding, false, &mut seen)?; }
    }
    if let Some(home) = home_binding {
        if !matches!(home, EmployeeBinding::Shared(_)) { return Err(VerificationError::Invalid); }
        if matches!(binding, EmployeeBinding::Personal(_)) || binding == home {
            for route in catalog.active_routes_for(home)? {
                if route.kind == ConnectorKind::SharedHome { plan.admit(route, home, true, &mut seen)?; }
            }
        }
    }
    Ok(plan)
}

/// Provider-specific configuration of one provisioned catalog account. No
/// Debug/Serialize: a channel route may carry a bot credential.
#[derive(Clone)]
pub enum ConnectorAccountDetail {
    /// `[db_sources.<source>]` the account is bound to.
    Database { source: String },
    /// Website credential id (`website_credentials`) the account is bound to.
    Computer { credential: String },
    /// Exact channel route; see [`ChannelAccount::new`].
    Channel { channel: String, scope: String, status_label: String, token: Option<String>, extra_id: Option<String> },
}
impl ConnectorAccountDetail {
    fn provider(&self) -> &'static str {
        match self { Self::Database { .. } => "database", Self::Computer { .. } => "computer", Self::Channel { .. } => "channel" }
    }
}

/// Gateway-owned store of per-account provisioning details, written by the
/// connector lifecycle when an account is provisioned. It is only ever asked
/// about a route the catalog just returned, so it can narrow but never add an
/// account. `Ok(None)` (not provisioned) and `Err` both refuse the build.
pub trait ConnectorAccountDetails: Send + Sync {
    fn detail(&self, route: &Route) -> Result<Option<ConnectorAccountDetail>, VerificationError>;
}

/// Non-account Google bridge inputs. The bridge reads its live lifecycle and catalog.
pub trait GoogleBridgeFactory: Send + Sync {
    fn bridge(&self, home: &Path, agent: &str, employee: EmployeeId, catalog: Arc<ConnectorCatalog>)
        -> Result<Arc<GoogleIsolationBridge>, VerificationError>;
}
pub struct GoogleBridgeInputs {
    pub users: Option<Arc<duduclaw_auth::UserDb>>,
    pub reference: duduclaw_core::google_identity::CredentialRef,
    pub turns: Arc<dyn crate::isolation_egress::RunTurnResolver>,
}
impl GoogleBridgeFactory for GoogleBridgeInputs {
    fn bridge(&self, home: &Path, agent: &str, employee: EmployeeId, catalog: Arc<ConnectorCatalog>)
        -> Result<Arc<GoogleIsolationBridge>, VerificationError> {
        GoogleIsolationBridge::new(home.to_path_buf(), agent.to_owned(), employee, self.users.clone(),
            self.reference.clone(), catalog, self.turns.clone()).map(Arc::new)
    }
}

/// Search needs the installed parse worker; there is no local parser fallback.
pub trait SearchProviderSource: Send + Sync {
    fn search(&self, employee: EmployeeId, account: ProductConnectorAccount)
        -> Result<Arc<SearchConnectorProvider>, VerificationError>;
}
#[cfg(feature = "isolation-worker-integration")]
impl SearchProviderSource for crate::isolation_execution::InstalledWorkerServices {
    fn search(&self, employee: EmployeeId, account: ProductConnectorAccount)
        -> Result<Arc<SearchConnectorProvider>, VerificationError> {
        SearchConnectorProvider::new(employee, account, self).map(Arc::new)
    }
}

/// CLI-owned Odoo business executor (D3). It is bound to the per-employee
/// gateway provider it is given and must use only that provider.
pub trait OdooExecutorFactory: Send + Sync {
    /// Missing host integration leaves this provider as a refusing adapter.
    fn available(&self) -> bool { true }
    /// Declared coverage, checked by `inventory` before any build.
    fn supports(&self, tool: &str) -> bool;
    fn executor(&self, provider: Arc<OdooConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError>;
}
/// CLI-owned channel executor (D3), bound to the given per-employee provider.
pub trait ChannelExecutorFactory: Send + Sync {
    fn available(&self) -> bool { true }
    fn supports(&self, tool: &str) -> bool;
    fn executor(&self, provider: Arc<ChannelConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError>;
}

/// CLI-owned factories only; account and employee selection stays in the gateway.
#[derive(Clone)]
pub struct ConnectorExecutorFactories {
    pub odoo: Arc<dyn OdooExecutorFactory>,
    pub channel: Arc<dyn ChannelExecutorFactory>,
}
static EXECUTOR_FACTORIES: OnceLock<ConnectorExecutorFactories> = OnceLock::new();

/// Install once before gateway startup. Never replace factories of a live epoch.
pub fn install_connector_executor_factories(factories: ConnectorExecutorFactories) -> Result<(), VerificationError> {
    EXECUTOR_FACTORIES.set(factories).map_err(|_| VerificationError::Unavailable)
}
/// F1 supplies these factories to `GatewayConnectorFactoryParts`. Missing CLI
/// integration preserves a complete registry with refusing Odoo/channel adapters.
pub fn connector_executor_factories() -> ConnectorExecutorFactories {
    executor_factories_from(&EXECUTOR_FACTORIES)
}
fn executor_factories_from(slot: &OnceLock<ConnectorExecutorFactories>) -> ConnectorExecutorFactories {
    slot.get().cloned().unwrap_or_else(|| ConnectorExecutorFactories {
        odoo: Arc::new(MissingExecutorFactory), channel: Arc::new(MissingExecutorFactory),
    })
}
struct MissingExecutorFactory;
impl OdooExecutorFactory for MissingExecutorFactory {
    fn available(&self) -> bool { false }
    fn supports(&self, _: &str) -> bool { false }
    fn executor(&self, _: Arc<OdooConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}
impl ChannelExecutorFactory for MissingExecutorFactory {
    fn available(&self) -> bool { false }
    fn supports(&self, _: &str) -> bool { false }
    fn executor(&self, _: Arc<ChannelConnectorProvider>) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> {
        Err(VerificationError::Unavailable)
    }
}

/// Everything the gateway factory needs that is not an account. All fields
/// are required; there is no default.
pub struct GatewayConnectorFactoryParts {
    /// The installed home's `Shared(tenant)` binding (tenant pinned in
    /// `apply.db`), or `None` when no home fallback exists.
    pub home_binding: Option<EmployeeBinding>,
    pub details: Arc<dyn ConnectorAccountDetails>,
    pub google: Arc<dyn GoogleBridgeFactory>,
    pub search: Arc<dyn SearchProviderSource>,
    /// Boot hardware snapshot for model discovery.
    pub hardware: duduclaw_inference::types::HardwareInfo,
    pub computer_sessions: Arc<crate::computer_use_sessions::ComputerUseSessions>,
    pub odoo_executors: Arc<dyn OdooExecutorFactory>,
    pub channel_executors: Arc<dyn ChannelExecutorFactory>,
}

/// Gateway-side [`ConnectorProviderFactory`]: builds every provider except the
/// CLI-owned Odoo/channel executors, which are injected.
pub struct GatewayConnectorFactory { parts: GatewayConnectorFactoryParts }
impl GatewayConnectorFactory {
    pub fn new(parts: GatewayConnectorFactoryParts) -> Result<Self, VerificationError> {
        if parts.home_binding.as_ref().is_some_and(|binding| !matches!(binding, EmployeeBinding::Shared(_))) {
            return Err(VerificationError::Invalid);
        }
        Ok(Self { parts })
    }
    fn detail(&self, account: &PlannedAccount) -> Result<ConnectorAccountDetail, VerificationError> {
        match self.parts.details.detail(&account.route) {
            Ok(Some(detail)) if detail.provider() == account.route.provider => Ok(detail),
            Ok(Some(_)) => Err(VerificationError::Invalid),
            Ok(None) | Err(_) => Err(VerificationError::Unavailable),
        }
    }
}
impl ConnectorProviderFactory for GatewayConnectorFactory {
    fn inventory(&self) -> Result<(), VerificationError> {
        let connector_tools = || POLICIES.iter().filter(|policy| policy.route == ToolRoute::Connector).map(|policy| policy.name);
        if connector_tools().any(|tool| !provider_for_tool(tool).is_some_and(|provider| FACTORY_PROVIDERS.contains(&provider)))
            || FACTORY_PROVIDERS.iter().any(|provider| !connector_tools().any(|tool| provider_for_tool(tool) == Some(*provider))) {
            return Err(VerificationError::Unavailable);
        }
        for tool in connector_tools() {
            // Absent CLI integration builds only the refusing adapter below.
            // An available but incomplete factory must still fail inventory.
            let covered = match provider_for_tool(tool) {
                Some("odoo") => !self.parts.odoo_executors.available() || self.parts.odoo_executors.supports(tool),
                Some("channel") => !self.parts.channel_executors.available() || self.parts.channel_executors.supports(tool),
                _ => true,
            };
            if !covered { return Err(VerificationError::Unavailable); }
        }
        Ok(())
    }

    fn build(&self, home: &Path, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        catalog: Arc<ConnectorCatalog>, bindings: Arc<dyn BindingAuthority>)
        -> Result<InstalledConnectorProviders, VerificationError> {
        self.inventory()?;
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        let parts = &self.parts;
        let plan = plan_connector_accounts(&catalog, binding, parts.home_binding.as_ref())?;
        let (home_dir, name) = (home.to_path_buf(), agent.to_owned());

        let google = parts.google.bridge(home, agent, employee, catalog.clone())?;

        let notion_policy = match plan.single("notion")? {
            Some(account) => Some(Arc::new(NotionPolicy::new(home_dir.clone(), name.clone(), employee,
                account.product(), catalog.clone())?) as Arc<dyn LiveConnectorSelectionPolicy>),
            None => None,
        };
        let github_policy = match plan.single("github")? {
            Some(account) => Some(Arc::new(GithubPolicy::new(home_dir.clone(), name.clone(), employee,
                account.product(), catalog.clone())?) as Arc<dyn LiveConnectorSelectionPolicy>),
            None => None,
        };
        let native_credentials: Arc<dyn AccountCredentialProvider> =
            Arc::new(HomeOAuthCredentialProvider::new(home_dir.clone(), catalog.clone())?);

        let mut sources = Vec::new();
        for account in plan.accounts("database") {
            let ConnectorAccountDetail::Database { source } = self.detail(account)? else { return Err(VerificationError::Invalid) };
            sources.push((account.product(), source));
        }
        let database = if sources.is_empty() { None } else {
            Some(Arc::new(DatabaseConnectorProvider::new(home_dir.clone(), name.clone(), employee, sources)?))
        };

        let mut credentials = Vec::new();
        for account in plan.accounts("computer") {
            let ConnectorAccountDetail::Computer { credential } = self.detail(account)? else { return Err(VerificationError::Invalid) };
            credentials.push((account.product(), credential));
        }
        let computer = if credentials.is_empty() { None } else {
            Some(Arc::new(ComputerConnectorProvider::new(home_dir.clone(), name.clone(), employee, credentials,
                parts.computer_sessions.clone())?))
        };

        let mut channels = Vec::new();
        for account in plan.accounts("channel").iter().filter(|_| parts.channel_executors.available()) {
            let ConnectorAccountDetail::Channel { channel, scope, status_label, token, extra_id } = self.detail(account)?
                else { return Err(VerificationError::Invalid) };
            channels.push(ChannelAccount::new(account.product(), channel, scope, status_label, token, extra_id)?);
        }
        let channel = if channels.is_empty() { None } else {
            let provider = Arc::new(ChannelConnectorProvider::new(home_dir.clone(), name.clone(), employee, channels)?);
            let executor = parts.channel_executors.executor(provider.clone())?;
            Some((provider, executor))
        };

        let odoo = match if parts.odoo_executors.available() { plan.single("odoo")? } else { None } {
            Some(account) => {
                let provider = Arc::new(OdooConnectorProvider::new(home_dir.clone(), name.clone(), employee, account.product())?);
                let executor = parts.odoo_executors.executor(provider.clone())?;
                Some((provider, executor))
            }
            None => None,
        };
        let model = match plan.single("model")? {
            Some(account) => Some(Arc::new(ModelConnectorProvider::new(home_dir.clone(), name.clone(), employee,
                account.product(), parts.hardware.clone())?)),
            None => None,
        };
        let search = match plan.single("search")? {
            Some(account) => Some(parts.search.search(employee, account.product())?),
            None => None,
        };

        // A revoke/advance/relink while providers were being built changes the
        // plan (generations are part of it): refuse instead of installing a
        // provider set for a catalog state that no longer exists.
        if plan_connector_accounts(&catalog, binding, parts.home_binding.as_ref())? != plan {
            return Err(VerificationError::Unavailable);
        }
        InstalledConnectorProviders::assemble(home_dir, name, employee, bindings, catalog, ConnectorBootProviders {
            google, notion_policy, github_policy, native_credentials, database, odoo, channel, model, computer, search,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane,
        LiveRequestVerifier, Principal}, ids::{RunId, Sha256Digest}};
    use duduclaw_mcp_boundary::bindings::BindingStore;
    use rusqlite::Connection;

    /// Test adapter: the standalone BindingStore double behind the production
    /// `BindingAuthority` trait object that `assemble`/`build` take.
    fn standalone(store: BindingStore) -> Arc<dyn BindingAuthority> { Arc::new(Mutex::new(store)) }
    fn empty_bindings() -> Arc<dyn BindingAuthority> {
        standalone(BindingStore::new(Connection::open_in_memory().unwrap()).unwrap())
    }

    #[test]
    fn installed_boot_cannot_be_reused_for_another_employee_home_or_name() {
        let home = PathBuf::from("/installed-home");
        let registry = Arc::new(ConnectorProviderRegistry::default());
        let bindings = Arc::new(Mutex::new(BindingStore::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap()));
        let resolver = Arc::new(GatewayResourceResolver::new(home.clone(), EmployeeId([1;16]), "bot".into(), bindings).unwrap());
        let installed = InstalledConnectorProviders { home:home.clone(), agent:"bot".into(), employee:EmployeeId([1;16]), registry, resolver };
        for (path, name, employee) in [
            (home.as_path(), "other", EmployeeId([1;16])),
            (home.as_path(), "bot", EmployeeId([2;16])),
            (std::path::Path::new("/another-home"), "bot", EmployeeId([1;16])),
        ] { assert_eq!(installed.verify_identity(path, name, employee), Err(VerificationError::BindingMismatch)); }
        assert_eq!(installed.verify_identity(&home, "bot", EmployeeId([1;16])), Err(VerificationError::Unavailable),
            "the right identity does not make an incomplete provider registry ready");
    }

    const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
    const AGENT: &str = "employee-a";
    fn text<const N: usize>(s: &str) -> Text<N> { Text::new(s.into()).unwrap() }
    fn alice() -> EmployeeBinding { EmployeeBinding::Personal(text("alice")) }
    fn bob() -> EmployeeBinding { EmployeeBinding::Personal(text("bob")) }
    fn company() -> EmployeeBinding { EmployeeBinding::Shared(text("company")) }
    fn other() -> EmployeeBinding { EmployeeBinding::Shared(text("other")) }

    struct Live;
    impl LiveRequestVerifier for Live {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
    }
    fn context(binding: EmployeeBinding, principal: Principal) -> RequestContext {
        RequestContext::with_live_verifier(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run: RunId([3; 16]), task: None,
            principal, employee_binding: binding, lane: Lane::Standard,
            tools: List::new(["db_sources", "model_search", "notion_search", "computer_login", "odoo_status"]
                .iter().map(|tool| text(tool)).collect()).unwrap(),
            resources: List::new(Vec::new()).unwrap(),
            policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
            nonce: [0; 32], jti: [0; 16],
        }, Arc::new(Live))
    }

    /// Catalog with routes for several owners, tenants, kinds and providers.
    struct Routes { catalog: Arc<ConnectorCatalog>, ids: BTreeMap<&'static str, AccountId> }
    impl Routes {
        fn new(extra: &[(&'static str, &'static str, ConnectorKind, EmployeeBinding)]) -> Self {
            let catalog = Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap());
            let mut ids = BTreeMap::new();
            let base = [
                ("alice_db", "database", ConnectorKind::Personal, alice()),
                ("alice_db_revoked", "database", ConnectorKind::Personal, alice()),
                ("alice_model", "model", ConnectorKind::Personal, alice()),
                ("alice_google", "google", ConnectorKind::Personal, alice()),
                // Notion is home-wide only: a Personal Notion route is never served.
                ("alice_notion", "notion", ConnectorKind::Personal, alice()),
                ("bob_db", "database", ConnectorKind::Personal, bob()),
                ("bob_google", "google", ConnectorKind::Personal, bob()),
                ("company_db", "database", ConnectorKind::SharedTenant, company()),
                // The Google bridge has no tenant source.
                ("company_google", "google", ConnectorKind::SharedTenant, company()),
                ("home_db", "database", ConnectorKind::SharedHome, company()),
                ("home_google", "google", ConnectorKind::SharedHome, company()),
                ("home_notion", "notion", ConnectorKind::SharedHome, company()),
                ("other_home_db", "database", ConnectorKind::SharedHome, other()),
            ];
            for (name, provider, kind, binding) in base.into_iter().chain(extra.iter().cloned()) {
                ids.insert(name, catalog.register(name, provider, kind, &binding).unwrap());
            }
            catalog.revoke(&ids["alice_db_revoked"]).unwrap();
            Self { catalog, ids }
        }
        fn name(&self, account: &str) -> &'static str {
            self.ids.iter().find(|(_, id)| id.as_str() == account).map(|(name, _)| *name).unwrap()
        }
        fn planned(&self, accounts: &[PlannedAccount]) -> Vec<(&'static str, bool)> {
            let mut names: Vec<_> = accounts.iter().map(|a| (self.name(a.route.account.as_str()), a.via_home)).collect();
            names.sort();
            names
        }
        fn offered(&self, accounts: Vec<ProductConnectorAccount>) -> Vec<&'static str> {
            let mut names: Vec<_> = accounts.iter().map(|a| self.name(a.account())).collect();
            names.sort();
            names
        }
    }

    struct Details { map: BTreeMap<AccountId, ConnectorAccountDetail>, asked: Mutex<Vec<AccountId>>, fail: bool }
    impl Details {
        fn new(map: Vec<(AccountId, ConnectorAccountDetail)>) -> Arc<Self> {
            Arc::new(Self { map: map.into_iter().collect(), asked: Mutex::new(Vec::new()), fail: false })
        }
        fn asked(&self, routes: &Routes) -> Vec<&'static str> {
            let mut names: Vec<_> = self.asked.lock().unwrap().iter().map(|id| routes.name(id.as_str())).collect();
            names.sort();
            names
        }
    }
    impl ConnectorAccountDetails for Details {
        fn detail(&self, route: &Route) -> Result<Option<ConnectorAccountDetail>, VerificationError> {
            self.asked.lock().unwrap().push(route.account.clone());
            if self.fail { return Err(VerificationError::Unavailable); }
            Ok(self.map.get(&route.account).cloned())
        }
    }
    fn db(source: &str) -> ConnectorAccountDetail { ConnectorAccountDetail::Database { source: source.into() } }

    struct Executors { provider: &'static str, missing: Option<&'static str>, bound: AtomicUsize }
    impl Executors {
        fn new(provider: &'static str, missing: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self { provider, missing, bound: AtomicUsize::new(0) })
        }
        fn covers(&self, tool: &str) -> bool { provider_for_tool(tool) == Some(self.provider) && self.missing != Some(tool) }
        fn bind(&self) -> Result<Arc<dyn BoundConnectorExecutor>, VerificationError> {
            self.bound.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(UnprovisionedProvider(self.provider)))
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

    struct NoSearch(AtomicUsize);
    impl SearchProviderSource for NoSearch {
        fn search(&self, _: EmployeeId, _: ProductConnectorAccount) -> Result<Arc<SearchConnectorProvider>, VerificationError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(VerificationError::Unavailable)
        }
    }
    struct NoTurns;
    impl crate::isolation_egress::RunTurnResolver for NoTurns {
        fn resolve(&self, _: &RequestContext) -> Result<crate::isolation_egress::RunTurnIdentity, VerificationError> {
            Err(VerificationError::Unavailable)
        }
    }
    /// Real bridge construction; records the exact catalog handed to the bridge.
    struct RecordingGoogle { inputs: GoogleBridgeInputs, seen: Mutex<Option<Arc<ConnectorCatalog>>> }
    impl GoogleBridgeFactory for RecordingGoogle {
        fn bridge(&self, home: &Path, agent: &str, employee: EmployeeId, catalog: Arc<ConnectorCatalog>)
            -> Result<Arc<GoogleIsolationBridge>, VerificationError> {
            *self.seen.lock().unwrap() = Some(catalog.clone());
            self.inputs.bridge(home, agent, employee, catalog)
        }
    }

    fn home_fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents").join(AGENT)).unwrap();
        std::fs::write(home.path().join("agents").join(AGENT).join("agent.toml"),
            "[capabilities]\ndb_sources=['demo','demo2']\n").unwrap();
        let url = |name: &str| format!("{:?}", home.path().join(name).to_str().unwrap());
        std::fs::write(home.path().join("config.toml"), format!(
            "[db_sources.demo]\ndriver='sqlite'\nurl={}\nallowed_tables=['*']\n\n[db_sources.demo2]\ndriver='sqlite'\nurl={}\nallowed_tables=['*']\n",
            url("demo.sqlite"), url("demo2.sqlite"))).unwrap();
        home
    }

    struct Setup {
        home: tempfile::TempDir, factory: GatewayConnectorFactory, details: Arc<Details>,
        odoo: Arc<Executors>, channel: Arc<Executors>, google: Arc<RecordingGoogle>, search: Arc<NoSearch>,
    }
    fn setup(details: Arc<Details>, odoo: Arc<Executors>, channel: Arc<Executors>) -> Setup {
        let home = home_fixture();
        let google = Arc::new(RecordingGoogle { seen: Mutex::new(None), inputs: GoogleBridgeInputs {
            users: None,
            reference: duduclaw_core::google_identity::CredentialRef {
                kind: duduclaw_core::google_identity::CredentialKind::Legacy, sha256: "a".repeat(64) },
            turns: Arc::new(NoTurns),
        } });
        let search = Arc::new(NoSearch(AtomicUsize::new(0)));
        let factory = GatewayConnectorFactory::new(GatewayConnectorFactoryParts {
            home_binding: Some(company()),
            details: details.clone(),
            google: google.clone(),
            search: search.clone(),
            hardware: duduclaw_inference::types::HardwareInfo {
                gpu_type: duduclaw_inference::types::GpuType::None, gpu_name: String::new(),
                vram_total_mb: 0, vram_available_mb: 0, ram_total_mb: 4096, ram_available_mb: 4096, cpu_cores: 1,
                recommended_backend: duduclaw_inference::types::BackendType::OpenAiCompat, recommended_max_model_gb: 0.0,
            },
            computer_sessions: crate::computer_use_sessions::workspace_admin::admin_sessions(home.path()),
            odoo_executors: odoo.clone(),
            channel_executors: channel.clone(),
        }).unwrap();
        Setup { home, factory, details, odoo, channel, google, search }
    }

    #[test]
    fn factory_builds_only_catalog_accounts_for_binding() {
        let routes = Routes::new(&[]);
        let id = |name: &str| routes.ids[name].clone();
        // Details exist for other employees' accounts too; they must never be asked for.
        let details = Details::new(vec![(id("alice_db"), db("demo")), (id("home_db"), db("demo2")),
            (id("bob_db"), db("demo")), (id("company_db"), db("demo")), (id("other_home_db"), db("demo"))]);
        let s = setup(details, Executors::new("odoo", None), Executors::new("channel", None));
        assert_eq!(s.factory.inventory(), Ok(()));

        let installed = s.factory.build(s.home.path(), AGENT, EMPLOYEE, &alice(), routes.catalog.clone(), empty_bindings()).unwrap();
        assert_eq!(installed.verify_identity(s.home.path(), AGENT, EMPLOYEE), Ok(()),
            "providers without a route still leave a complete, refusing registry");
        assert_eq!(s.details.asked(&routes), vec!["alice_db", "home_db"],
            "only this employee's own and home routes are resolved");
        assert!(Arc::ptr_eq(s.google.seen.lock().unwrap().as_ref().unwrap(), &routes.catalog),
            "Google receives the live catalog, never a precomputed fallback account list");
        let ctx = context(alice(), Principal::Human { user_id: text("alice"), generation: 1 });
        let registry = installed.registry();
        assert_eq!(routes.offered(registry.candidates(&ctx, "db_sources", "database").unwrap()), vec!["alice_db", "home_db"]);
        assert_eq!(routes.offered(registry.candidates(&ctx, "model_search", "model").unwrap()), vec!["alice_model"]);
        assert_eq!(routes.offered(registry.candidates(&ctx, "notion_search", "notion").unwrap()), vec!["home_notion"]);
        for (tool, provider) in [("computer_login", "computer"), ("odoo_status", "odoo")] {
            assert_eq!(registry.candidates(&ctx, tool, provider).err(), Some(VerificationError::Unavailable),
                "{provider} has no route for this employee");
        }
        assert_eq!((s.odoo.bound.load(Ordering::SeqCst), s.channel.bound.load(Ordering::SeqCst),
            s.search.0.load(Ordering::SeqCst)), (0, 0, 0), "nothing is built for an unrouted provider");

        // The same factory for the Shared company employee: tenant + home routes.
        s.details.asked.lock().unwrap().clear();
        let installed = s.factory.build(s.home.path(), AGENT, EMPLOYEE, &company(), routes.catalog.clone(), empty_bindings()).unwrap();
        assert_eq!(s.details.asked(&routes), vec!["company_db", "home_db"]);
        assert!(Arc::ptr_eq(s.google.seen.lock().unwrap().as_ref().unwrap(), &routes.catalog));
        let ctx = context(company(), Principal::Background { tenant_id: text("company"), generation: 1 });
        assert_eq!(routes.offered(installed.registry().candidates(&ctx, "db_sources", "database").unwrap()),
            vec!["company_db", "home_db"]);
        assert_eq!(installed.registry().candidates(&ctx, "model_search", "model").err(), Some(VerificationError::Unavailable));
    }

    #[test]
    fn missing_factory_refuses() {
        // Use a local empty slot so this test cannot depend on process-global boot order.
        let missing = executor_factories_from(&OnceLock::new());
        assert!(!missing.odoo.available() && !missing.channel.available());
        let routes = Routes::new(&[
            ("alice_odoo", "odoo", ConnectorKind::Personal, alice()),
            ("alice_channel", "channel", ConnectorKind::Personal, alice()),
        ]);
        let mut s = setup(Details::new(vec![(routes.ids["alice_db"].clone(), db("demo")),
            (routes.ids["home_db"].clone(), db("demo2"))]),
            Executors::new("odoo", None), Executors::new("channel", None));
        s.factory.parts.odoo_executors = missing.odoo;
        s.factory.parts.channel_executors = missing.channel;
        let installed = s.factory.build(s.home.path(), AGENT, EMPLOYEE, &alice(), routes.catalog,
            empty_bindings()).unwrap();
        let ctx = context(alice(), Principal::Human { user_id: text("alice"), generation: 1 });
        for (tool, provider) in [("odoo_status", "odoo"), ("channel_status", "channel")] {
            assert_eq!(installed.registry().candidates(&ctx, tool, provider).err(), Some(VerificationError::Unavailable));
        }
        assert_eq!(installed.verify_identity(s.home.path(), AGENT, EMPLOYEE), Ok(()));
        assert_eq!(s.details.asked.lock().unwrap().len(), 2, "missing channel factory never reads channel credentials");
    }

    #[test]
    fn factory_refuses_incomplete_inventory() {
        let routes = Routes::new(&[]);
        for (odoo, channel) in [(Executors::new("odoo", Some("odoo_execute")), Executors::new("channel", None)),
            (Executors::new("odoo", None), Executors::new("channel", Some("send_photo"))),
            // An executor declaring another provider's tools covers nothing here.
            (Executors::new("channel", None), Executors::new("channel", None))] {
            let s = setup(Details::new(vec![]), odoo, channel);
            assert_eq!(s.factory.inventory(), Err(VerificationError::Unavailable));
            assert_eq!(s.factory.build(s.home.path(), AGENT, EMPLOYEE, &alice(), routes.catalog.clone(), empty_bindings()).err(),
                Some(VerificationError::Unavailable));
            assert!(s.details.asked.lock().unwrap().is_empty() && s.google.seen.lock().unwrap().is_none(),
                "an incomplete inventory refuses before any store or provider is touched");
        }
        let s = setup(Details::new(vec![]), Executors::new("odoo", None), Executors::new("channel", None));
        assert_eq!(s.factory.inventory(), Ok(()));
        // The home fallback binding must be a tenant, never a person.
        let parts = GatewayConnectorFactoryParts { home_binding: Some(alice()), ..s.factory.parts };
        assert!(GatewayConnectorFactory::new(parts).is_err());
    }

    #[test]
    fn personal_binding_sees_only_owner_personal_and_home_shared_routes() {
        let routes = Routes::new(&[
            ("alice_odoo", "odoo", ConnectorKind::Personal, alice()),
            ("home_odoo", "odoo", ConnectorKind::SharedHome, company()),
            ("home_odoo_2", "odoo", ConnectorKind::SharedHome, company()),
            ("home_search", "search", ConnectorKind::SharedHome, company()),
            ("alice_search", "search", ConnectorKind::Personal, alice()),
        ]);
        let plan = |binding: &EmployeeBinding, home: Option<&EmployeeBinding>|
            plan_connector_accounts(&routes.catalog, binding, home).unwrap();

        let alice_plan = plan(&alice(), Some(&company()));
        assert_eq!(routes.planned(alice_plan.accounts("database")), vec![("alice_db", false), ("home_db", true)],
            "no revoked, bob, tenant or other-home database route");
        assert_eq!(routes.planned(alice_plan.accounts("google")), vec![("alice_google", false), ("home_google", true)]);
        assert_eq!(routes.planned(alice_plan.accounts("notion")), vec![("home_notion", true)]);
        assert_eq!(routes.planned(alice_plan.accounts("search")), vec![("home_search", true)],
            "a Personal search route is not a servable kind");
        assert_eq!(routes.planned(alice_plan.accounts("model")), vec![("alice_model", false)]);
        // Single-account providers: the own route wins over the home fallback.
        assert_eq!(alice_plan.single("odoo").unwrap().map(|a| routes.name(a.route.account.as_str())), Some("alice_odoo"));
        assert!(alice_plan.single("channel").unwrap().is_none());

        // Without an installed home binding, only the owner's Personal routes.
        let alone = plan(&alice(), None);
        assert_eq!(routes.planned(alone.accounts("database")), vec![("alice_db", false)]);
        assert_eq!(routes.planned(alone.accounts("google")), vec![("alice_google", false)]);
        assert!(alone.accounts("notion").is_empty() && alone.accounts("search").is_empty());

        // Another owner never sees alice's routes.
        let bob_plan = plan(&bob(), Some(&company()));
        assert_eq!(routes.planned(bob_plan.accounts("database")), vec![("bob_db", false), ("home_db", true)]);
        assert!(bob_plan.accounts("model").is_empty());
        // Two home Odoo routes and no own route: ambiguous, refused.
        assert_eq!(bob_plan.single("odoo").err(), Some(VerificationError::Invalid));

        // Shared: own tenant routes plus home routes only for the home tenant.
        let company_plan = plan(&company(), Some(&company()));
        assert_eq!(routes.planned(company_plan.accounts("database")), vec![("company_db", false), ("home_db", true)]);
        assert_eq!(routes.planned(company_plan.accounts("google")), vec![("home_google", true)]);
        let other_plan = plan(&other(), Some(&company()));
        for provider in FACTORY_PROVIDERS {
            assert!(other_plan.accounts(provider).is_empty(),
                "{provider}: another tenant gets no home route, and its own SharedHome route is not reached via the own binding");
        }
        assert_eq!(plan_connector_accounts(&routes.catalog, &alice(), Some(&bob())).err(), Some(VerificationError::Invalid));
    }

    #[test]
    fn factory_store_error_refuses() {
        // Catalog store error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        let catalog = Arc::new(ConnectorCatalog::new(Connection::open(&path).unwrap()).unwrap());
        let alice_db = catalog.register("alice_db", "database", ConnectorKind::Personal, &alice()).unwrap();
        let s = setup(Details::new(vec![(alice_db.clone(), db("demo"))]), Executors::new("odoo", None), Executors::new("channel", None));
        assert!(s.factory.build(s.home.path(), AGENT, EMPLOYEE, &alice(), catalog.clone(), empty_bindings()).is_ok());
        Connection::open(&path).unwrap().execute_batch("DROP TABLE isolation_connector_routes").unwrap();
        s.details.asked.lock().unwrap().clear();
        assert_eq!(s.factory.build(s.home.path(), AGENT, EMPLOYEE, &alice(), catalog, empty_bindings()).err(),
            Some(VerificationError::Unavailable));
        assert!(s.details.asked.lock().unwrap().is_empty());

        // Detail store error, missing detail, detail for another provider.
        let routes = Routes::new(&[]);
        let id = |name: &str| routes.ids[name].clone();
        let build = |details: Arc<Details>| {
            let s = setup(details, Executors::new("odoo", None), Executors::new("channel", None));
            s.factory.build(s.home.path(), AGENT, EMPLOYEE, &alice(), routes.catalog.clone(), empty_bindings()).err()
        };
        let failing = Arc::new(Details { map: BTreeMap::new(), asked: Mutex::new(Vec::new()), fail: true });
        assert_eq!(build(failing), Some(VerificationError::Unavailable));
        assert_eq!(build(Details::new(vec![(id("alice_db"), db("demo"))])), Some(VerificationError::Unavailable),
            "the home database route has no provisioned detail");
        assert_eq!(build(Details::new(vec![(id("alice_db"), db("demo")),
            (id("home_db"), ConnectorAccountDetail::Computer { credential: "x".into() })])), Some(VerificationError::Invalid));
        assert_eq!(build(Details::new(vec![(id("alice_db"), db("demo")), (id("home_db"), db("demo2"))])), None);

        // The search worker source is unavailable while a search route exists.
        let routes = Routes::new(&[("home_search", "search", ConnectorKind::SharedHome, company())]);
        let s = setup(Details::new(vec![(routes.ids["alice_db"].clone(), db("demo")), (routes.ids["home_db"].clone(), db("demo2"))]),
            Executors::new("odoo", None), Executors::new("channel", None));
        assert_eq!(s.factory.build(s.home.path(), AGENT, EMPLOYEE, &alice(), routes.catalog.clone(), empty_bindings()).err(),
            Some(VerificationError::Unavailable));
        assert_eq!(s.search.0.load(Ordering::SeqCst), 1);
    }
}
