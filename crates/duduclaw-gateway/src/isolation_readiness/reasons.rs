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
//!   reconnect) ⇒ `revoked`;
//! - W1, host-wide: the protected backend's live spawnd feature source and
//!   boot's work broker installation ⇒ `workspace_broker_unavailable`; HX5's
//!   `helper_calls` bit and the published runtime ids ⇒
//!   `helper_launcher_unavailable{reason}` / `helper_runtime_missing{runtime}`.
//!   These are action-required only while some protected employee's
//!   configured tool list names a tool that needs them ([`ToolNeeds`]);
//!   otherwise `report` lists them as informational `notices`.
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
    /// HX5 (D8 revision §5, [F17]): in-run helper tools are refused because
    /// spawnd cannot start helpers; `reason` is a fixed code
    /// (`spawnd_feature_missing` = no `helper_calls` bit).
    HelperLauncherUnavailable { reason: &'static str },
    /// HX5 (H16): a helper runtime of an in-run tool is not in the signed
    /// publication; that tool stays hidden. `runtime` is a fixed runtime id.
    HelperRuntimeMissing { runtime: &'static str },
    /// W1 (H16): the brokered file tools (and the in-run helpers, which need
    /// the same Workspace grant) are hidden because spawnd's live readiness
    /// view lacks `work_broker`, or boot installed no work broker.
    WorkspaceBrokerUnavailable,
}

/// W1: host-wide reasons of the brokered work root and the in-run helpers.
/// `features` = spawnd's checked readiness bits (`None` = unreadable, read as
/// none); `broker_installed` = boot installed a work broker producer;
/// `published` = registered runtime ids of the fresh gateway publication
/// (`None` = unreadable, read as none published). Pure; fail closed.
pub fn feature_reasons(features: Option<duduclaw_isolation::ipc::SpawndFeatures>, broker_installed: bool,
    published: Option<&[&str]>) -> Vec<Reason> {
    use duduclaw_isolation::ipc::SpawndFeatures;
    let features = features.unwrap_or(SpawndFeatures::NONE);
    let mut reasons = Vec::new();
    if !broker_installed || !features.contains(SpawndFeatures::WORK_BROKER) {
        reasons.push(Reason::WorkspaceBrokerUnavailable);
    }
    reasons.extend(helper_reasons(features.contains(SpawndFeatures::HELPER_CALLS),
        published.unwrap_or_default().iter().copied()));
    reasons
}

/// File tools served from the brokered `employee-work` root.
const FILE_TOOLS: [&str; 3] = ["file_read", "csv_read", "xlsx_read"];
/// Browser recording tools (HX6; in-run helper + work root).
const RECORDING_TOOLS: [&str; 2] = ["browser_record_start", "browser_record_stop"];

/// W1 follow-up: which host-wide feature reasons concern any protected
/// employee. A reason only makes the status `action_required` when at least
/// one employee's configured `[capabilities]` list actually names a tool that
/// needs it; otherwise it is reported as an informational notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolNeeds {
    /// `file_read` / `csv_read` / `xlsx_read` (work broker).
    pub files: bool,
    /// `execute_program` (program helper runtime).
    pub program: bool,
    /// `code_map` (code-map helper runtime).
    pub code_map: bool,
    /// `office_script` (office helper runtimes).
    pub office: bool,
    /// `browser_record_*` (recording helper).
    pub recording: bool,
}

impl ToolNeeds {
    /// Fail closed: an unreadable profile counts as needing everything.
    pub const ALL: Self = Self { files: true, program: true, code_map: true, office: true, recording: true };

    /// From one employee's `[capabilities] denied_tools` / `allowed_tools`,
    /// with the same `tool_list_verdict` the gates use. Only an explicit
    /// allowlist "configures" tools: an empty `allowed_tools` names none.
    pub fn from_lists(denied: &[String], allowed: &[String]) -> Self {
        use duduclaw_core::tool_catalog::{tool_list_verdict, ToolListVerdict};
        let names = |tools: &[&str]| !allowed.is_empty() && tools.iter()
            .any(|tool| tool_list_verdict(tool, denied, allowed) == ToolListVerdict::Allowed);
        Self { files: names(&FILE_TOOLS), program: names(&["execute_program"]), code_map: names(&["code_map"]),
            office: names(&["office_script"]), recording: names(&RECORDING_TOOLS) }
    }

    pub fn union(self, other: Self) -> Self {
        Self { files: self.files || other.files, program: self.program || other.program,
            code_map: self.code_map || other.code_map, office: self.office || other.office,
            recording: self.recording || other.recording }
    }

    fn helpers(&self) -> bool { self.program || self.code_map || self.office }

    /// Whether `reason` (a [`feature_reasons`] entry) concerns these tools.
    /// Non-feature reasons always do.
    pub fn concerns(&self, reason: &Reason) -> bool {
        match reason {
            // The in-run helpers need the same Workspace grant as the file tools.
            Reason::WorkspaceBrokerUnavailable => self.files || self.helpers(),
            Reason::HelperLauncherUnavailable { .. } => self.helpers() || self.recording,
            Reason::HelperRuntimeMissing { runtime } => match *runtime {
                duduclaw_execution::program_helper::PROGRAM_RUNTIME => self.program,
                duduclaw_execution::code_map_helper::RUNTIME => self.code_map,
                _ => self.office,
            },
            _ => true,
        }
    }

    /// Split feature reasons into (action required, informational).
    pub fn split(&self, reasons: Vec<Reason>) -> (Vec<Reason>, Vec<Reason>) {
        reasons.into_iter().partition(|reason| self.concerns(reason))
    }
}

/// The union of [`ToolNeeds`] over the given protected employees' current
/// profiles (gateway-private `agents/<label>/agent.toml`, the same no-follow
/// reader the H9 selectors use). Blocking.
#[cfg(target_os = "linux")]
pub(crate) fn installed_tool_needs<'a>(home: &Path, labels: impl IntoIterator<Item = &'a str>) -> ToolNeeds {
    labels.into_iter().fold(ToolNeeds::default(), |needs, label| {
        needs.union(crate::isolation_mcp::read_employee_profile(home, label).map_or(ToolNeeds::ALL, |profile|
            ToolNeeds::from_lists(&profile.capabilities.denied_tools, &profile.capabilities.allowed_tools)))
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn installed_tool_needs<'a>(_: &Path, _: impl IntoIterator<Item = &'a str>) -> ToolNeeds {
    ToolNeeds::ALL
}

/// [`feature_reasons`] from the live sources the gates read: the backend's
/// readiness feature source, boot's broker installation and the fresh
/// gateway publication's runtime ids. Blocking (two small root-owned files).
#[cfg(target_os = "linux")]
pub(crate) fn installed_feature_reasons() -> Vec<Reason> {
    use duduclaw_isolation::execution::RuntimeKind;
    let features = crate::isolation_mcp::readiness_features().ok();
    let publication = duduclaw_spawnd::installed::GatewayPublication::read(nix::unistd::getegid().as_raw()).ok();
    let published: Option<Vec<&str>> = publication.as_ref().map(|publication| publication.runtimes.iter()
        .filter_map(|record| match &record.runtime { RuntimeKind::Registered(id) => Some(id.as_str()), _ => None })
        .collect());
    feature_reasons(features, crate::isolation_mcp::work_broker_installed(), published.as_deref())
}

/// Protected mode exists only on Linux; elsewhere `report` never runs.
#[cfg(not(target_os = "linux"))]
pub(crate) fn installed_feature_reasons() -> Vec<Reason> { Vec::new() }

/// Fixed ids reported by `helper_runtime_missing` (office scripts are one
/// family of `duduclaw-office-<skill>-<script>-v1` runtimes).
pub const HELPER_RUNTIMES: [&str; 3] = [duduclaw_execution::program_helper::PROGRAM_RUNTIME,
    duduclaw_execution::code_map_helper::RUNTIME, "duduclaw-office-*-v1"];

/// HX5: helper readiness of the three in-run tools from spawnd's advertised
/// `helper_calls` bit and the published runtime ids. Pure; the caller reads
/// both from the same sources the gates use (readiness report, publication).
pub fn helper_reasons<'a>(helper_calls: bool, published: impl IntoIterator<Item = &'a str>) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if !helper_calls {
        reasons.push(Reason::HelperLauncherUnavailable {
            reason: duduclaw_isolation::ipc::HELPER_FEATURE_MISSING_REASON });
    }
    let published: Vec<&str> = published.into_iter().collect();
    for runtime in HELPER_RUNTIMES {
        let present = match runtime.strip_suffix("*-v1") {
            Some(prefix) => published.iter().any(|id| id.starts_with(prefix) && id.ends_with("-v1") && id.len() > prefix.len() + 3),
            None => published.contains(&runtime),
        };
        if !present { reasons.push(Reason::HelperRuntimeMissing { runtime }); }
    }
    reasons
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
        Ok(Some(catalog)) => catalog_reasons(home, catalog, &lifecycle, &mut reasons),
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

fn catalog_reasons(home: &Path, catalog: &ConnectorCatalog, lifecycle: &EmployeeLifecycle,
    reasons: &mut BTreeSet<Reason>) {
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
    // E4: the tenant source is read like the bridge reads it, so a Shared
    // employee's SharedTenant route counts only while a real source exists.
    match GoogleAccountRoutes::readiness(home, catalog, lifecycle) {
        Ok(()) => {}
        Err(GoogleRoutesError::SetupRequired) => {
            reasons.insert(Reason::ConnectorSetupRequired { provider: Some("google") });
        }
        Err(GoogleRoutesError::Unavailable) => { reasons.insert(Reason::ServiceUnavailable); }
        Err(GoogleRoutesError::BindingMismatch) => { reasons.insert(Reason::BindingEvidenceMissing); }
    }
}

#[cfg(test)]
mod hx5_tests {
    use super::*;

    /// [F17] No `helper_calls` bit ⇒ a named launcher reason; each missing
    /// helper runtime is reported by its fixed id, never a path.
    #[test]
    fn gateway_helper_readiness_names_missing_feature_and_runtimes() {
        let all = ["duduclaw-program-v1", "duduclaw-code-map-v1", "duduclaw-office-docx-render-v1"];
        assert!(helper_reasons(true, all).is_empty());
        let missing = helper_reasons(false, ["duduclaw-program-v1"]);
        assert_eq!(missing, vec![Reason::HelperLauncherUnavailable { reason: "spawnd_feature_missing" },
            Reason::HelperRuntimeMissing { runtime: "duduclaw-code-map-v1" },
            Reason::HelperRuntimeMissing { runtime: "duduclaw-office-*-v1" }]);
        assert_eq!(serde_json::to_value(missing[0]).unwrap(),
            serde_json::json!({"code":"helper_launcher_unavailable","reason":"spawnd_feature_missing"}));
        assert_eq!(serde_json::to_value(missing[1]).unwrap(),
            serde_json::json!({"code":"helper_runtime_missing","runtime":"duduclaw-code-map-v1"}));
        // A bare prefix is not an office runtime.
        assert!(helper_reasons(true, ["duduclaw-program-v1", "duduclaw-code-map-v1", "duduclaw-office--v1"])
            .contains(&Reason::HelperRuntimeMissing { runtime: "duduclaw-office-*-v1" }));
    }
}
