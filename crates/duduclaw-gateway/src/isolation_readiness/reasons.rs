//! Per-employee readiness reasons (native Linux isolation v1.81, task F3).
//!
//! Every reason is derived from the same reads the gates use, never from a
//! second copy of their rules:
//! - `isolation_lifecycle::read_lifecycle_with` (A2; apply.db, confirmations,
//!   root ReadBack, users.db) — Unavailable ⇒ `service_unavailable`,
//!   Tombstoned ⇒ `revoked`, Stale ⇒ `personal_binding_conflict` (Personal) or
//!   `binding_evidence_missing` (Shared), Denied ⇒ `binding_evidence_missing`;
//! - B1's `personal_facts` on one users.db snapshot (mirror owner, exactly one
//!   member, owner directly bound) ⇒ `personal_binding_conflict`;
//! - the connector factory's refusals as reported by its boot
//!   ([`record_connector_build`]; `ProviderRefusal::SetupRequired`, C6) ⇒
//!   `connector_setup_required{provider}`; no build for this employee (factory
//!   missing, matrix 12) ⇒ `connector_setup_required` without provider;
//! - E2's Google route resolution on the employee's catalog routes ⇒
//!   `connector_setup_required{google}`;
//! - C4 relink hints (an owner/home disconnect revoked a route that waits for
//!   reconnect) ⇒ `revoked`.
//!
//! Fail closed: an unreadable source is `service_unavailable`, never "ready".
//! The payload holds fixed codes, provider family names from the factory's
//! static table and employee labels (admins only); no paths, ids, tenant or
//! owner values, account ids or raw errors.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::VerificationError;
use serde::Serialize;

use crate::google_identity::isolation_routes::{GoogleAccountRoutes, GoogleRoutesError};
use crate::isolation_connector_boot::InstalledConnectorProviders;
use crate::isolation_connectors::{CATALOG_FILE, ConnectorCatalog, ConnectorKind, RouteScope};
use crate::isolation_lifecycle::{self as lifecycle, EmployeeLifecycle, LifecycleError, RootAllocation};

/// Upper bound of employees assessed per status read; more is `Unavailable`.
pub(crate) const MAX_EMPLOYEES: usize = 256;
const MAX_REPORTS: usize = 4096;

/// One readiness reason. Serialized as `{"code": .., "provider"?: ..}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Reason {
    /// A protected employee without confirmed lifecycle evidence.
    BindingEvidenceMissing,
    /// A connector the employee has routes for is not usable until an admin
    /// completes its setup. `provider` is a factory provider family name.
    ConnectorSetupRequired {
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<&'static str>,
    },
    /// users.db mirror/membership disagrees with the Personal lifecycle.
    PersonalBindingConflict,
    /// Employee tombstoned/disabled, or a connector route revoked.
    Revoked,
    /// A store or service needed to decide could not be read.
    ServiceUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall { Ready, ActionRequired, ServiceUnavailable }

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EmployeeReadiness {
    pub label: String,
    pub ready: bool,
    pub reasons: Vec<Reason>,
}

/// What one connector factory build of an employee reported (secret-free).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectorReport {
    pub(crate) employee: EmployeeId,
    /// Ok = providers installed as `SetupRequired` refusing adapters.
    pub(crate) outcome: Result<Vec<&'static str>, VerificationError>,
}

static CONNECTOR_REPORTS: Mutex<BTreeMap<String, ConnectorReport>> = Mutex::new(BTreeMap::new());

/// Called by the boot path (F1) after every `ConnectorProviderFactory::build`
/// for an employee, success or failure, so readiness shows the factory's own
/// refusals. A newer build of the same label replaces the older report; a
/// report for another employee id under the label is ignored at read time.
pub fn record_connector_build(home: &Path, agent: &str, employee: EmployeeId,
    outcome: Result<&InstalledConnectorProviders, VerificationError>) {
    let outcome = outcome.and_then(|installed| {
        installed.verify_identity(home, agent, employee)?;
        Ok(installed.setup_required().collect())
    });
    let Ok(mut reports) = CONNECTOR_REPORTS.lock() else { return };
    if reports.len() >= MAX_REPORTS && !reports.contains_key(agent) { return; }
    reports.insert(agent.to_owned(), ConnectorReport { employee, outcome });
}

/// A poisoned registry is unreadable: every employee then reads unavailable.
fn connector_reports() -> Option<BTreeMap<String, ConnectorReport>> {
    CONNECTOR_REPORTS.lock().ok().map(|reports| reports.clone())
}

/// The assessed state of every confirmed protected employee.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Assessment {
    /// Union of all employee reasons, plus `service_unavailable` when the
    /// employee listing itself could not be read.
    pub(crate) reasons: BTreeSet<Reason>,
    pub(crate) employees: Vec<EmployeeReadiness>,
}

/// Production sources (blocking SQLite + root IPC; call off the async runtime).
pub(crate) fn assess_installed(home: &Path) -> Assessment {
    assess(home, &lifecycle::RootAllocator, connector_reports().as_ref())
}

pub(crate) fn assess(home: &Path, root: &dyn RootAllocation, reports: Option<&BTreeMap<String, ConnectorReport>>)
    -> Assessment {
    let mut assessment = Assessment::default();
    let entries = match lifecycle::confirmed_entries(home, MAX_EMPLOYEES) {
        Ok(entries) => entries,
        Err(_) => { assessment.reasons.insert(Reason::ServiceUnavailable); return assessment; }
    };
    // One catalog open for all employees; absence is "no routes", not an error.
    let catalog = if entries.is_empty() { Ok(None) } else { open_catalog(home) };
    for entry in entries {
        let reasons = employee_reasons(home, &entry, root, reports, catalog.as_ref().map(Option::as_ref));
        assessment.reasons.extend(reasons.iter().copied());
        assessment.employees.push(EmployeeReadiness { ready: reasons.is_empty(), label: entry.label,
            reasons: reasons.into_iter().collect() });
    }
    assessment
}

/// `Ok(None)`: no catalog file. The status read never creates one.
fn open_catalog(home: &Path) -> Result<Option<ConnectorCatalog>, ()> {
    match std::fs::symlink_metadata(home.join(CATALOG_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
        Ok(_) => ConnectorCatalog::open(home).map(Some).map_err(|_| ()),
    }
}

fn employee_reasons(home: &Path, entry: &lifecycle::ConfirmedEntry, root: &dyn RootAllocation,
    reports: Option<&BTreeMap<String, ConnectorReport>>, catalog: Result<Option<&ConnectorCatalog>, &()>)
    -> BTreeSet<Reason> {
    let mut reasons = BTreeSet::new();
    if entry.disabled { reasons.insert(Reason::Revoked); return reasons; }
    let lifecycle = match lifecycle::read_lifecycle_with(home, &entry.label, root) {
        Ok(lifecycle) => lifecycle,
        Err(error) => { reasons.insert(lifecycle_reason(error, entry.personal)); return reasons; }
    };
    if let Some(owner) = lifecycle.personal_owner() {
        match personal_binding_holds(home, &lifecycle, owner) {
            Ok(true) => {}
            Ok(false) => { reasons.insert(Reason::PersonalBindingConflict); }
            Err(_) => { reasons.insert(Reason::ServiceUnavailable); }
        }
    }
    factory_reasons(&lifecycle, reports, &mut reasons);
    match catalog {
        Err(_) => { reasons.insert(Reason::ServiceUnavailable); }
        Ok(None) => {}
        Ok(Some(catalog)) => catalog_reasons(catalog, &lifecycle, &mut reasons),
    }
    reasons
}

fn lifecycle_reason(error: LifecycleError, personal: bool) -> Reason {
    match error {
        LifecycleError::Unavailable => Reason::ServiceUnavailable,
        LifecycleError::Tombstoned => Reason::Revoked,
        LifecycleError::Stale if personal => Reason::PersonalBindingConflict,
        LifecycleError::Stale | LifecycleError::Denied => Reason::BindingEvidenceMissing,
    }
}

#[cfg(target_os = "linux")]
fn personal_binding_holds(home: &Path, lifecycle: &EmployeeLifecycle, owner: &str) -> Result<bool, LifecycleError> {
    use duduclaw_isolation::{bounded::Text, capability::Principal};
    let Some(snapshot) = lifecycle::personal_snapshot(home, &lifecycle.label, owner)? else { return Ok(false) };
    let user_id = Text::new(owner.to_owned()).map_err(|_| LifecycleError::Denied)?;
    let principal = Principal::Human { user_id, generation: snapshot.acl_generation };
    Ok(crate::isolation_mcp::personal_facts(&lifecycle.label, owner, &principal, &snapshot).is_ok())
}

/// The B1 rule lives in the Linux-only authority; elsewhere nothing can be
/// verified, so the employee reads unavailable (fail closed).
#[cfg(not(target_os = "linux"))]
fn personal_binding_holds(_: &Path, _: &EmployeeLifecycle, _: &str) -> Result<bool, LifecycleError> {
    Err(LifecycleError::Unavailable)
}

fn factory_reasons(lifecycle: &EmployeeLifecycle, reports: Option<&BTreeMap<String, ConnectorReport>>,
    reasons: &mut BTreeSet<Reason>) {
    let Some(reports) = reports else { reasons.insert(Reason::ServiceUnavailable); return };
    match reports.get(&lifecycle.label).filter(|report| report.employee == lifecycle.employee) {
        // No factory build for this employee (no factory installed, or the
        // label was recreated since): its connectors cannot be used.
        None => { reasons.insert(Reason::ConnectorSetupRequired { provider: None }); }
        Some(ConnectorReport { outcome: Ok(providers), .. }) => {
            reasons.extend(providers.iter().map(|provider| Reason::ConnectorSetupRequired { provider: Some(*provider) }));
        }
        Some(ConnectorReport { outcome: Err(error), .. }) => {
            reasons.insert(match error {
                VerificationError::Unavailable => Reason::ServiceUnavailable,
                VerificationError::Revoked => Reason::Revoked,
                VerificationError::BindingMismatch => Reason::BindingEvidenceMissing,
                // Invalid (e.g. two routes where one is required) is an admin
                // setup problem, not an outage.
                VerificationError::Invalid | VerificationError::InvalidTime =>
                    Reason::ConnectorSetupRequired { provider: None },
            });
        }
    }
}

fn catalog_reasons(catalog: &ConnectorCatalog, lifecycle: &EmployeeLifecycle, reasons: &mut BTreeSet<Reason>) {
    let binding = &lifecycle.binding;
    // C4: a revoked route still waiting for its owner/home to reconnect.
    let kind = match binding {
        EmployeeBinding::Personal(_) => ConnectorKind::Personal,
        EmployeeBinding::Shared(_) => ConnectorKind::SharedHome,
    };
    match catalog.pending_relinks(RouteScope { kind, binding: Some(binding), provider: None }) {
        Ok(0) => {}
        Ok(_) => { reasons.insert(Reason::Revoked); }
        Err(_) => { reasons.insert(Reason::ServiceUnavailable); }
    }
    // E2: only for an employee that has Google routes at all; an employee
    // without Google is not asked to set it up.
    let routes = match catalog.active_routes_for(binding) {
        Ok(routes) => routes,
        Err(_) => { reasons.insert(Reason::ServiceUnavailable); return; }
    };
    if !routes.iter().any(|route| route.provider == "google") { return; }
    match GoogleAccountRoutes::readiness(catalog, lifecycle) {
        Ok(()) => {}
        Err(GoogleRoutesError::SetupRequired) => {
            reasons.insert(Reason::ConnectorSetupRequired { provider: Some("google") });
        }
        Err(GoogleRoutesError::Unavailable) => { reasons.insert(Reason::ServiceUnavailable); }
        Err(GoogleRoutesError::BindingMismatch) => { reasons.insert(Reason::BindingEvidenceMissing); }
    }
}
