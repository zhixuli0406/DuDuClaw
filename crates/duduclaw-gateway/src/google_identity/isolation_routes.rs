//! Google routes are catalog facts, scoped by the live employee lifecycle.
//! Configuration may narrow source access but can never supply an account.
use std::{collections::BTreeMap, path::Path};

use duduclaw_isolation::{capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_mcp_boundary::VerificationError;

use super::tenant_source::TenantSourceKind;
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
    pub(crate) fn binding(&self) -> &EmployeeBinding { &self.route.binding }
    pub(crate) fn recheck(&self, catalog: &ConnectorCatalog) -> Result<(), VerificationError> {
        let current = catalog.route(&self.route.account)?.ok_or(VerificationError::BindingMismatch)?;
        if current != self.route || !current.active { return Err(VerificationError::BindingMismatch); }
        Ok(())
    }
}

/// Constructible only by catalog/lifecycle reads. A Personal map has exactly
/// the lifecycle owner as its key and never contains a shared home fallback.
/// `tenant` (v1.81 E4) is the employee's own SharedTenant route, present only
/// for a Shared employee and only while a real tenant credential source
/// (delegation key or service account) is configured.
pub struct GoogleAccountRoutes {
    pub(crate) personal: BTreeMap<String, GoogleRoute>,
    pub(crate) home: Option<GoogleRoute>,
    pub(crate) tenant: Option<GoogleRoute>,
    /// F6b (A5): the `(employee, incarnation)` this selection was read for.
    pub(crate) pinned: (EmployeeId, u32),
}
impl GoogleAccountRoutes {
    /// F6b (A5): whether this selection belongs to exactly `lifecycle`'s
    /// employee id and incarnation; a selection of a prior incarnation (or
    /// of another employee on the label) never serves the current one.
    pub(crate) fn pinned_to(&self, lifecycle: &EmployeeLifecycle) -> bool {
        self.pinned == (lifecycle.employee, lifecycle.incarnation)
    }

    /// Blocking lifecycle (SQLite/root IPC) and catalog reads; call on the same
    /// blocking boundary as the other live protected authority checks.
    /// `tenant_source` is the kind of the tenant credential source currently
    /// configured for this employee (from `tenant_source::read`); without one
    /// SharedTenant routes are never admitted.
    pub(crate) fn read(home: &Path, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        catalog: &ConnectorCatalog, tenant_source: Option<TenantSourceKind>) -> Result<Self, GoogleRoutesError> {
        Self::read_with(agent, employee, binding, catalog, tenant_source,
            || isolation_lifecycle::read_lifecycle(home, agent))
    }

    fn read_with(agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        catalog: &ConnectorCatalog, tenant_source: Option<TenantSourceKind>,
        read: impl Fn() -> Result<EmployeeLifecycle, LifecycleError>) -> Result<Self, GoogleRoutesError> {
        let lifecycle = read().map_err(lifecycle_error)?;
        if lifecycle.employee != employee || lifecycle.label != agent || &lifecycle.binding != binding {
            return Err(GoogleRoutesError::BindingMismatch);
        }
        let routes = Self::from_lifecycle(catalog, &lifecycle, tenant_source)?;
        // A handover/delete/recreate during the catalog read cannot authorize
        // an account using a stale owner or an old root allocation.
        if read().map_err(lifecycle_error)? != lifecycle {
            return Err(GoogleRoutesError::BindingMismatch);
        }
        Ok(routes)
    }

    /// Readiness (F3): the same route resolution as [`Self::read`] for a
    /// lifecycle the caller has just read; only the typed outcome is kept.
    /// The tenant source is read exactly as `Self::read`'s caller (the bridge)
    /// reads it: `tenant_source::read(home, agent)` with the employee's label.
    /// Fail closed: an unreadable source is `Unavailable`, a refused one
    /// `BindingMismatch` (the same mapping as its `VerificationError`).
    pub(crate) fn readiness(home: &Path, catalog: &ConnectorCatalog, lifecycle: &EmployeeLifecycle)
        -> Result<(), GoogleRoutesError> {
        let tenant_source = super::tenant_source::read(home, &lifecycle.label).map_err(|error| match error {
            super::tenant_source::TenantSourceError::Unavailable => GoogleRoutesError::Unavailable,
            super::tenant_source::TenantSourceError::Refused => GoogleRoutesError::BindingMismatch,
        })?;
        Self::from_lifecycle(catalog, lifecycle, tenant_source.as_ref().map(|source| source.kind())).map(|_| ())
    }

    fn from_lifecycle(catalog: &ConnectorCatalog, lifecycle: &EmployeeLifecycle,
        tenant_source: Option<TenantSourceKind>) -> Result<Self, GoogleRoutesError> {
        let routes = catalog.active_routes_for(&lifecycle.binding).map_err(|_| GoogleRoutesError::Unavailable)?;
        // Only this exact binding's active Google rows. For a Shared employee
        // the binding is Shared(tenant pinned in apply.db), so a route of any
        // other tenant never appears here.
        let google: Vec<Route> = routes.into_iter().filter(|route| route.provider == "google" && route.active
            && route.binding == lifecycle.binding).collect();
        // The sealed source has one account per owner/home/tenant. Multiple
        // routes of one kind have no trustworthy credential mapping: never
        // choose the first one.
        let unique = |kind: ConnectorKind| -> Result<Option<Route>, GoogleRoutesError> {
            let mut matching = google.iter().filter(|route| route.kind == kind);
            let first = matching.next().cloned();
            if matching.next().is_some() { return Err(GoogleRoutesError::SetupRequired); }
            Ok(first)
        };
        match &lifecycle.binding {
            EmployeeBinding::Personal(owner) => {
                let route = unique(ConnectorKind::Personal)?.ok_or(GoogleRoutesError::SetupRequired)?;
                let account = route.account.as_str().to_owned();
                Ok(Self {
                    personal: BTreeMap::from([(owner.as_str().to_owned(), GoogleRoute {
                        product: ProductConnectorAccount::Personal { account, owner: owner.as_str().to_owned() }, route,
                    })]),
                    home: None,
                    tenant: None,
                    pinned: (lifecycle.employee, lifecycle.incarnation),
                })
            }
            EmployeeBinding::Shared(tenant) => {
                let home = unique(ConnectorKind::SharedHome)?;
                // v1.81 E4: a SharedTenant route is admitted only while a real
                // tenant credential source exists. Without one it would be
                // served from the home credential (SR3), so it is ignored and
                // a tenant-only catalog stays setup-required.
                let tenant_route = match tenant_source {
                    Some(_) => unique(ConnectorKind::SharedTenant)?,
                    None => None,
                };
                if home.is_none() && tenant_route.is_none() { return Err(GoogleRoutesError::SetupRequired); }
                let product = |route: Route| GoogleRoute { product: ProductConnectorAccount::SharedHome {
                    account: route.account.as_str().to_owned(), tenant: tenant.as_str().to_owned(),
                }, route };
                Ok(Self { personal: BTreeMap::new(), home: home.map(product), tenant: tenant_route.map(product),
                    pinned: (lifecycle.employee, lifecycle.incarnation) })
            }
        }
    }
}

#[cfg(test)]
#[path = "isolation_routes_tests.rs"]
pub(super) mod tests;
