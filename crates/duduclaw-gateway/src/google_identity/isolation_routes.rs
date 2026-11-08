//! Google routes are catalog facts, scoped by the live employee lifecycle.
//! Configuration may narrow source access but can never supply an account.
use std::{collections::BTreeMap, path::Path};

use duduclaw_isolation::{capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_mcp_boundary::VerificationError;

use crate::{
    isolation_connectors::{ConnectorCatalog, ConnectorKind, ProductConnectorAccount, Route},
    isolation_lifecycle::{self, EmployeeLifecycle, LifecycleError},
};

/// Kept distinct for readiness/UI callers; the MCP verification boundary has
/// no setup-required variant and therefore maps that refusal to Unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GoogleRoutesError {
    #[error("Google connector routes unavailable")]
    Unavailable,
    #[error("connector_setup_required: google")]
    SetupRequired,
    #[error("Google connector binding mismatch")]
    BindingMismatch,
}
impl GoogleRoutesError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "service_unavailable",
            Self::SetupRequired => "connector_setup_required",
            Self::BindingMismatch => "binding_evidence_missing",
        }
    }
}
impl From<GoogleRoutesError> for VerificationError {
    fn from(error: GoogleRoutesError) -> Self {
        match error {
            GoogleRoutesError::Unavailable | GoogleRoutesError::SetupRequired => Self::Unavailable,
            GoogleRoutesError::BindingMismatch => Self::BindingMismatch,
        }
    }
}
fn lifecycle_error(error: LifecycleError) -> GoogleRoutesError {
    match error {
        LifecycleError::Unavailable => GoogleRoutesError::Unavailable,
        _ => GoogleRoutesError::BindingMismatch,
    }
}

/// Immutable selection proof. In addition to C1's SelectedConnector pin, the
/// bridge retains this proof so a generation change between product selection
/// and C1's catalog read cannot silently select the replacement generation.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GoogleRoute {
    pub(crate) product: ProductConnectorAccount,
    route: Route,
}
impl GoogleRoute {
    pub(crate) fn generation(&self) -> u64 { self.route.generation }
    pub(crate) fn kind(&self) -> ConnectorKind { self.route.kind }
    pub(crate) fn recheck(&self, catalog: &ConnectorCatalog) -> Result<(), VerificationError> {
        let current = catalog.route(&self.route.account)?.ok_or(VerificationError::BindingMismatch)?;
        if current != self.route || !current.active { return Err(VerificationError::BindingMismatch); }
        Ok(())
    }
}

/// Constructible only by catalog/lifecycle reads. A Personal map has exactly
/// the lifecycle owner as its key and never contains a shared home fallback.
pub struct GoogleAccountRoutes {
    pub(crate) personal: BTreeMap<String, GoogleRoute>,
    pub(crate) home: Option<GoogleRoute>,
}
impl GoogleAccountRoutes {
    /// Blocking lifecycle (SQLite/root IPC) and catalog reads; call on the same
    /// blocking boundary as the other live protected authority checks.
    pub fn read(home: &Path, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        catalog: &ConnectorCatalog) -> Result<Self, GoogleRoutesError> {
        Self::read_with(agent, employee, binding, catalog,
            || isolation_lifecycle::read_lifecycle(home, agent))
    }

    fn read_with(agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        catalog: &ConnectorCatalog, read: impl Fn() -> Result<EmployeeLifecycle, LifecycleError>)
        -> Result<Self, GoogleRoutesError> {
        let lifecycle = read().map_err(lifecycle_error)?;
        if lifecycle.employee != employee || lifecycle.label != agent || &lifecycle.binding != binding {
            return Err(GoogleRoutesError::BindingMismatch);
        }
        let routes = Self::from_lifecycle(catalog, &lifecycle)?;
        // A handover/delete/recreate during the catalog read cannot authorize
        // an account using a stale owner or an old root allocation.
        if read().map_err(lifecycle_error)? != lifecycle {
            return Err(GoogleRoutesError::BindingMismatch);
        }
        Ok(routes)
    }

    fn from_lifecycle(catalog: &ConnectorCatalog, lifecycle: &EmployeeLifecycle)
        -> Result<Self, GoogleRoutesError> {
        let routes = catalog.active_routes_for(&lifecycle.binding).map_err(|_| GoogleRoutesError::Unavailable)?;
        // Shared employees admit only the home route. SharedTenant routes have
        // no tenant-scoped credential source yet (task E4); until one exists a
        // tenant route would be served from the home credential, so it is
        // ignored here and a tenant-only catalog is setup-required.
        let mut routes = routes.into_iter().filter(|route| route.provider == "google" && route.active
            && route.binding == lifecycle.binding && matches!((&lifecycle.binding, route.kind),
                (EmployeeBinding::Personal(_), ConnectorKind::Personal)
                | (EmployeeBinding::Shared(_), ConnectorKind::SharedHome)));
        let route = routes.next().ok_or(GoogleRoutesError::SetupRequired)?;
        // The sealed source has one account per owner/home. Multiple routes
        // have no trustworthy credential mapping: never choose the first one.
        if routes.next().is_some() { return Err(GoogleRoutesError::SetupRequired); }
        let account = route.account.as_str().to_owned();
        match &lifecycle.binding {
            EmployeeBinding::Personal(owner) => Ok(Self {
                personal: BTreeMap::from([(owner.as_str().to_owned(), GoogleRoute {
                    product: ProductConnectorAccount::Personal { account, owner: owner.as_str().to_owned() }, route,
                })]),
                home: None,
            }),
            EmployeeBinding::Shared(tenant) => Ok(Self {
                personal: BTreeMap::new(),
                home: Some(GoogleRoute { product: ProductConnectorAccount::SharedHome {
                    account, tenant: tenant.as_str().to_owned(),
                }, route }),
            }),
        }
    }
}

#[cfg(test)]
#[path = "isolation_routes_tests.rs"]
pub(super) mod tests;
