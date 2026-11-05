//! Formal, immutable connector assembly for protected gateway boot. All tool
//! registrations are derived from the policy table, with no hand-maintained
//! partial list and no generic-vault substitution for Google's sealed owner.
use std::{path::PathBuf, sync::{Arc, Mutex}};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::{bindings::BindingStore, VerificationError};
use crate::{google_identity::isolation_bridge::GoogleIsolationBridge,
    isolation_connectors::{BoundConnectorExecutor, ConnectorCatalog, LiveConnectorSelectionPolicy},
    isolation_connector_clients::{AccountCredentialProvider, NativeConnectorExecutor},
    isolation_connector_database::DatabaseConnectorProvider,
    isolation_connector_channel::ChannelConnectorProvider,
    isolation_connector_model::ModelConnectorProvider,
    isolation_connector_computer::ComputerConnectorProvider,
    isolation_connector_odoo::OdooConnectorProvider,
    isolation_connector_registry::ConnectorProviderRegistry,
    isolation_connector_search::SearchConnectorProvider,
    isolation_resources::GatewayResourceResolver};

/// CLI-owned business handlers cannot be imported by the gateway. Their boot
/// adapters retain the concrete gateway policy and an exact bound executor.
/// No absent/default fields can silently omit one of the required providers.
pub struct ConnectorBootProviders {
    pub google: Arc<GoogleIsolationBridge>,
    pub notion_policy: Arc<dyn LiveConnectorSelectionPolicy>,
    pub github_policy: Arc<dyn LiveConnectorSelectionPolicy>,
    pub native_credentials: Arc<dyn AccountCredentialProvider>,
    pub database: Arc<DatabaseConnectorProvider>,
    pub odoo_policy: Arc<OdooConnectorProvider>,
    pub odoo_executor: Arc<dyn BoundConnectorExecutor>,
    pub channel_policy: Arc<ChannelConnectorProvider>,
    pub channel_executor: Arc<dyn BoundConnectorExecutor>,
    pub model: Arc<ModelConnectorProvider>,
    pub computer: Arc<ComputerConnectorProvider>,
    pub search: Arc<SearchConnectorProvider>,
}

/// Keep this installed value for the employee's gateway epoch. Catalog and
/// BindingStore are the already-provisioned stores owned by the lifecycle host;
/// boot never manufactures an account grant from credentials it happens to find.
pub struct InstalledConnectorProviders {
    registry: Arc<ConnectorProviderRegistry>,
    resolver: Arc<GatewayResourceResolver>,
    home: PathBuf,
    agent: String,
    employee: EmployeeId,
}
impl InstalledConnectorProviders {
    pub fn assemble(home: PathBuf, agent: String, employee: EmployeeId,
        bindings: Arc<Mutex<BindingStore>>, catalog: Arc<ConnectorCatalog>,
        providers: ConnectorBootProviders) -> Result<Self, VerificationError> {
        let native = Arc::new(NativeConnectorExecutor::new(home.clone(), agent.clone(), providers.native_credentials)?);
        let mut registry = ConnectorProviderRegistry::default();
        registry.register_provider("google", providers.google.clone(), providers.google)?;
        registry.register_provider("notion", providers.notion_policy, native.clone())?;
        registry.register_provider("github", providers.github_policy, native)?;
        registry.register_provider("database", providers.database.clone(), providers.database)?;
        registry.register_provider("odoo", providers.odoo_policy, providers.odoo_executor)?;
        registry.register_provider("channel", providers.channel_policy, providers.channel_executor)?;
        registry.register_provider("model", providers.model.clone(), providers.model)?;
        registry.register_provider("computer", providers.computer.clone(), providers.computer)?;
        registry.register_provider("search", providers.search.clone(), providers.search)?;
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
