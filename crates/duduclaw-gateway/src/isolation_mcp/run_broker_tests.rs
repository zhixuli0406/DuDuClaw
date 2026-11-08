//! W1 tests: boot's run-bound work brokers and the readiness wiring, over a
//! transport double for the protected backend (a real `SpawndBackend` needs
//! root's socket) and the real issuer / profile / apply.db stores.
use super::*;
use std::{collections::BTreeMap, sync::{Mutex, atomic::AtomicU32}};
use duduclaw_execution::spawnd::RunTransport;
use duduclaw_isolation::{bounded::{List, Text}, capability::{Lane, ResourceGrant}};
use duduclaw_mcp_boundary::resources::{WorkRelativePath, EMPLOYEE_WORK_ROOT};
use super::super::{selectors::SelectorSources, test_support};
use crate::isolation_readiness::reasons::{feature_reasons, Reason};

const LABEL: &str = "w1-employee";
const EMPLOYEE: EmployeeId = EmployeeId([41; 16]);
const RUN_A: RunId = RunId([43; 16]);
const RUN_B: RunId = RunId([47; 16]);
const CONTROL_A: ControlConnectionId = ControlConnectionId([53; 16]);
const CONTROL_B: ControlConnectionId = ControlConnectionId([59; 16]);

/// The protected backend: a readiness view, the runs it reserved (with each
/// connection's admission bits) and every open that reached "root".
struct Transport {
    source: bool,
    host: AtomicU32,
    runs: Mutex<BTreeMap<RunId, RunTransport>>,
    opens: Mutex<Vec<(RunId, EmployeeId, String)>>,
}
impl Transport {
    fn new(source: bool, host: SpawndFeatures) -> Arc<Self> {
        Arc::new(Self { source, host: AtomicU32::new(host.0), runs: Mutex::default(), opens: Mutex::default() })
    }
    fn reserve(&self, run: RunId, employee: EmployeeId, control: ControlConnectionId, features: SpawndFeatures) {
        self.runs.lock().unwrap().insert(run, RunTransport { employee, control, features });
    }
    fn opens(&self) -> Vec<(RunId, EmployeeId, String)> { self.opens.lock().unwrap().clone() }
}
impl RunWorkTransport for Transport {
    fn has_feature_source(&self) -> bool { self.source }
    fn host_features(&self) -> SpawndFeatures { SpawndFeatures(self.host.load(Ordering::SeqCst)) }
    fn run_transport(&self, run: RunHandle) -> Option<RunTransport> { self.runs.lock().unwrap().get(&run.0).copied() }
    /// Reaching root answers `NotFound`, which no local refusal produces.
    fn open_work_file(&self, request: &WorkFileRequest<'_>) -> Result<BrokeredWorkFile, WorkBrokerError> {
        self.opens.lock().unwrap().push((request.run(), request.employee(), request.path().as_str().to_owned()));
        Err(WorkBrokerError::NotFound)
    }
}

/// Live incarnation as the F6m reader would report it.
fn identity(live: Arc<Mutex<Option<(EmployeeId, u64)>>>) -> IncarnationReader {
    Arc::new(move |_: &Path, label: &str| if label == LABEL { *live.lock().unwrap() } else { None })
}

/// `Ok(())` never happens with this transport (root answers `NotFound`).
fn open(broker: &dyn WorkBroker, run: RunId, employee: EmployeeId) -> Result<(), WorkBrokerError> {
    let path = WorkRelativePath::parse("notes/a.txt").unwrap();
    broker.open_work_file(&WorkFileRequest::for_test(run, employee, &path)).map(|_| ())
}

fn tools(names: &[&str]) -> List<Text<128>, 256> {
    List::new(names.iter().map(|name| Text::new((*name).to_owned()).unwrap()).collect()).unwrap()
}
fn turn() -> super::super::HostTurn {
    super::super::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
        turn: "w1-turn".into(), approval_session: None }, egress_turn: None,
        metadata: None, principal_context: None, background: None }
}
fn workspace() -> ResourceGrant { ResourceGrant::Workspace { handle: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap() } }

/// A confirmed Shared employee whose profile allows every tool.
fn employee(dir: &tempfile::TempDir) -> test_support::SharedEmployee {
    std::fs::create_dir_all(dir.path().join("agents").join(LABEL)).unwrap();
    std::fs::write(dir.path().join("agents").join(LABEL).join("agent.toml"),
        test_support::agent_toml(LABEL, false, "")).unwrap();
    test_support::SharedEmployee::new(dir.path(), LABEL, EMPLOYEE)
}
fn issue(employee: &test_support::SharedEmployee, names: &[&str]) -> super::super::RuntimeAuthorization {
    employee.authorize(&employee.snapshot(), Lane::Standard, tools(names), turn(), None).unwrap()
}
fn has_workspace(auth: &super::super::RuntimeAuthorization) -> bool {
    auth.grant.resources.as_slice().contains(&workspace())
}

#[test]
fn w1_production_boot_installs_run_bound_work_broker() {
    let dir = tempfile::tempdir().unwrap();
    let shared = employee(&dir);
    let transport = Transport::new(true, SpawndFeatures::WORK_BROKER);
    transport.reserve(RUN_A, EMPLOYEE, CONTROL_A, SpawndFeatures::WORK_BROKER);
    // Boot's composition without a host broker: the run-bound factory.
    let sources = SelectorSources::installed_over(dir.path(), None, transport.clone());
    assert!(sources.work_broker.is_none(), "no shared broker handle is installed");
    let brokers = sources.run_brokers.clone().expect("run-bound brokers over the protected backend");
    // The issuer now selects the Workspace grant with the brokered root.
    let auth = issue(&shared.with_selector_sources(sources), &["file_read", "csv_read", "xlsx_read"]);
    assert!(has_workspace(&auth));
    assert!(auth.roots.is_brokered(EMPLOYEE_WORK_ROOT), "brokered root, never a dirfd");
    // The broker `apply` mints, with the production F6m incarnation reader
    // over the fixture's apply.db: incarnation 0 of this employee id.
    let broker = brokers.bind(LABEL, EMPLOYEE, 0);
    assert_eq!(open(broker.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::NotFound, "reached root");
    assert_eq!(transport.opens(), [(RUN_A, EMPLOYEE, "notes/a.txt".to_owned())]);
    // Another pinned incarnation (a restore since issuance) never reaches root.
    let stale = brokers.bind(LABEL, EMPLOYEE, 1);
    assert_eq!(open(stale.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    assert_eq!(transport.opens().len(), 1);

    // The bridge cannot be built (backend without a readiness source): no
    // factory, no grant, no root.
    let dir = tempfile::tempdir().unwrap();
    let sources = SelectorSources::installed_over(dir.path(), None, Transport::new(false, SpawndFeatures::WORK_BROKER));
    assert!(sources.run_brokers.is_none() && sources.work_broker.is_none());
    let auth = issue(&employee(&dir).with_selector_sources(sources), &["file_read"]);
    assert!(!has_workspace(&auth) && !auth.roots.is_brokered(EMPLOYEE_WORK_ROOT));
}

#[test]
fn w1_feature_source_comes_from_run_admission() {
    let live = Arc::new(Mutex::new(Some((EMPLOYEE, 0))));
    let transport = Transport::new(true, SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS));
    let brokers = RunBrokers::with_identity(Path::new("/home-w1"), transport.clone(), identity(live)).unwrap();
    assert!(brokers.available(), "issuance reads the host readiness view");
    // Readiness advertises `work_broker`, but this run's own admission
    // (`Reserved.features`) did not: nothing is sent on its connection.
    transport.reserve(RUN_A, EMPLOYEE, CONTROL_A, SpawndFeatures::HELPER_CALLS);
    let broker = brokers.bind(LABEL, EMPLOYEE, 0);
    assert_eq!(open(broker.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::Unavailable);
    assert_eq!(WorkBrokerError::Unavailable.code(), "workspace_broker_unavailable");
    assert!(transport.opens().is_empty());
    // With the bit in the run's admission the same run is served, and the
    // refused attempt did not latch the broker to anything.
    transport.reserve(RUN_A, EMPLOYEE, CONTROL_A, SpawndFeatures::WORK_BROKER);
    assert_eq!(open(broker.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::NotFound);
    assert_eq!(transport.opens().len(), 1);
    // The selectors' helper view is the same live readiness source.
    let sources = SelectorSources::installed_over(Path::new("/home-w1"), None, transport.clone());
    let view = sources.spawnd_features.as_ref().unwrap();
    assert!(view().contains(SpawndFeatures::HELPER_CALLS));
    transport.host.store(SpawndFeatures::NONE.0, Ordering::SeqCst);
    assert_eq!(view(), SpawndFeatures::NONE);
}

#[test]
fn w1_missing_feature_bit_keeps_tools_hidden_and_reports_reason() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Transport::new(true, SpawndFeatures::HELPER_CALLS);
    let shared = employee(&dir).with_selector_sources(
        SelectorSources::installed_over(dir.path(), None, transport.clone()));
    let names = ["file_read", "csv_read", "xlsx_read", "execute_program", "code_map", "office_script"];
    // spawnd lacks `work_broker` (older build): no grant, no root, so the file
    // tools and the in-run helpers (same grant) stay hidden.
    let auth = issue(&shared, &names);
    assert!(!has_workspace(&auth) && !auth.roots.is_brokered(EMPLOYEE_WORK_ROOT));
    assert_eq!(feature_reasons(Some(SpawndFeatures::HELPER_CALLS), true, Some(&[] as &[&str])).first(),
        Some(&Reason::WorkspaceBrokerUnavailable));
    // Bit advertised again: the next issuance selects it (and helpers too).
    transport.host.store(SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS).0, Ordering::SeqCst);
    let auth = issue(&shared, &names);
    assert!(has_workspace(&auth) && auth.roots.is_brokered(EMPLOYEE_WORK_ROOT));
    // Reason table: unreadable view, missing bit, or no broker installed.
    let both = SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS);
    let all: &[&str] = &["duduclaw-program-v1", "duduclaw-code-map-v1", "duduclaw-office-docx-render-v1"];
    assert!(feature_reasons(Some(both), true, Some(all)).is_empty());
    for (features, installed) in [(None, true), (Some(SpawndFeatures::HELPER_CALLS), true), (Some(both), false)] {
        assert!(feature_reasons(features, installed, Some(all)).contains(&Reason::WorkspaceBrokerUnavailable),
            "{features:?} {installed}");
    }
    assert_eq!(serde_json::to_value(Reason::WorkspaceBrokerUnavailable).unwrap(),
        serde_json::json!({"code": "workspace_broker_unavailable"}));
    // Without `helper_calls` the HX5 launcher reason is reported too.
    assert!(feature_reasons(Some(SpawndFeatures::WORK_BROKER), true, Some(all))
        .contains(&Reason::HelperLauncherUnavailable { reason: "spawnd_feature_missing" }));
}

#[test]
fn w1_broker_never_shared_across_runs() {
    let live = Arc::new(Mutex::new(Some((EMPLOYEE, 0))));
    let transport = Transport::new(true, SpawndFeatures::WORK_BROKER);
    let brokers = RunBrokers::with_identity(Path::new("/home-w1"), transport.clone(), identity(live.clone())).unwrap();
    let work = SpawndFeatures::WORK_BROKER;
    transport.reserve(RUN_A, EMPLOYEE, CONTROL_A, work);
    transport.reserve(RUN_B, EMPLOYEE, CONTROL_B, work);
    let first = brokers.bind(LABEL, EMPLOYEE, 0);
    assert_eq!(open(first.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::NotFound);
    // Same employee, another run (even a later run of the same employee):
    // the capability's broker is latched to its first run.
    assert_eq!(open(first.as_ref(), RUN_B, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    // The run handle now maps to another control connection: refused.
    transport.reserve(RUN_A, EMPLOYEE, CONTROL_B, work);
    assert_eq!(open(first.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    transport.reserve(RUN_A, EMPLOYEE, CONTROL_A, work);
    // A request for another employee, or a run reserved for another employee.
    let other = EmployeeId([61; 16]);
    assert_eq!(open(first.as_ref(), RUN_A, other).unwrap_err(), WorkBrokerError::NotLive);
    let foreign = brokers.bind(LABEL, EMPLOYEE, 0);
    transport.reserve(RUN_B, other, CONTROL_B, work);
    assert_eq!(open(foreign.as_ref(), RUN_B, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    // Each capability gets its own broker: a second issuance serves its own run.
    transport.reserve(RUN_B, EMPLOYEE, CONTROL_B, work);
    let second = brokers.bind(LABEL, EMPLOYEE, 0);
    assert_eq!(open(second.as_ref(), RUN_B, EMPLOYEE).unwrap_err(), WorkBrokerError::NotFound);
    assert!(!Arc::ptr_eq(&first, &second));
    // Unknown (fenced) run, tombstone, restore: nothing reaches root.
    transport.runs.lock().unwrap().remove(&RUN_A);
    assert_eq!(open(first.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    *live.lock().unwrap() = Some((EMPLOYEE, 1));
    assert_eq!(open(second.as_ref(), RUN_B, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    *live.lock().unwrap() = None;
    assert_eq!(open(second.as_ref(), RUN_B, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    assert_eq!(transport.opens(), [(RUN_A, EMPLOYEE, "notes/a.txt".to_owned()),
        (RUN_B, EMPLOYEE, "notes/a.txt".to_owned())]);
    // A host-supplied broker gets the same employee check and run latch.
    let host = latched(Arc::new(RunBoundBroker { transport: transport.clone(), home: "/home-w1".into(),
        agent: LABEL.into(), identity: identity(Arc::new(Mutex::new(Some((EMPLOYEE, 0))))), employee: EMPLOYEE,
        incarnation: 0, run: OnceLock::new() }), EMPLOYEE);
    assert_eq!(open(host.as_ref(), RUN_B, EMPLOYEE).unwrap_err(), WorkBrokerError::NotFound);
    assert_eq!(open(host.as_ref(), RUN_A, EMPLOYEE).unwrap_err(), WorkBrokerError::NotLive);
    assert_eq!(open(host.as_ref(), RUN_B, other).unwrap_err(), WorkBrokerError::NotLive);
}

#[test]
fn w1_readiness_reports_helper_runtime_missing() {
    let both = SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS);
    // Only the program helper is published: the other two are named by id.
    let reasons = feature_reasons(Some(both), true, Some(&["claude", "duduclaw-program-v1"][..]));
    assert_eq!(reasons, vec![Reason::HelperRuntimeMissing { runtime: "duduclaw-code-map-v1" },
        Reason::HelperRuntimeMissing { runtime: "duduclaw-office-*-v1" }]);
    assert_eq!(serde_json::to_value(reasons[0]).unwrap(),
        serde_json::json!({"code": "helper_runtime_missing", "runtime": "duduclaw-code-map-v1"}));
    // An unreadable publication reads as nothing published (fail closed).
    let unreadable = feature_reasons(Some(both), true, None);
    for runtime in crate::isolation_readiness::reasons::HELPER_RUNTIMES {
        assert!(unreadable.contains(&Reason::HelperRuntimeMissing { runtime }), "{runtime}");
    }
    // Published office runtimes match the family; a bare prefix does not.
    assert!(feature_reasons(Some(both), true,
        Some(&["duduclaw-program-v1", "duduclaw-code-map-v1", "duduclaw-office-xlsx-fill-v1"][..])).is_empty());
    assert!(feature_reasons(Some(both), true,
        Some(&["duduclaw-program-v1", "duduclaw-code-map-v1", "duduclaw-office--v1"][..]))
        .contains(&Reason::HelperRuntimeMissing { runtime: "duduclaw-office-*-v1" }));
}

/// Reverse check: outside protected mode nothing here runs. The readiness
/// report (the only caller of these reasons) is absent, and a backend
/// without a readiness source never gets a broker.
#[test]
fn w1_unprotected_mode_untouched() {
    if crate::isolation_readiness::status().is_none() {
        assert!(crate::isolation_readiness::report(Path::new("/nonexistent-w1"), true).is_none());
    }
    assert!(RunBrokers::new(Path::new("/home-w1"), Transport::new(false, SpawndFeatures::WORK_BROKER)).is_none());
    assert!(RunBrokers::new(Path::new("relative"), Transport::new(true, SpawndFeatures::WORK_BROKER)).is_none());
    // Default selector sources (non-protected issuance fixtures) carry nothing.
    let sources = SelectorSources::default();
    assert!(sources.work_broker.is_none() && sources.run_brokers.is_none() && sources.spawnd_features.is_none());
}
