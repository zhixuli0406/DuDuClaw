//! W1: production wiring of the H5/H6 work broker and spawnd's feature source.
//!
//! * **Feature source** ([`readiness_features`]): installed on the protected
//!   backend at connect (`isolation_readiness::connect_backend`) with
//!   `SpawndBackend::with_feature_source`. It reads spawnd's own checked
//!   readiness report (`EvidenceReport.features`), never config. It only
//!   narrows: every work/helper op still needs the bit in the run's own
//!   `Reserved.features` (HX5 `connection_features`).
//! * **Run-bound brokers** ([`RunBrokers`]): boot keeps one factory over the
//!   protected backend; it is not a broker and serves nothing. The issuer
//!   mints a fresh broker per capability ([`RunBrokers::bind`]) bound to the
//!   employee id and incarnation it pinned, and that broker latches the first
//!   run it serves together with that run's control connection id. Every open
//!   then requires exact equality of all four (request employee, live
//!   incarnation, run, control connection) plus the `work_broker` bit of that
//!   run's admission, before H5's `SpawndWorkspaceBridge` sends `OpenWorkFile`
//!   on that same connection: the latched control id is handed to the open
//!   (`SpawndBackend::open_work_file_on`), which refuses `NotLive` when the
//!   run's connection is no longer that one. A broker is reachable only through the
//!   `ResourceRoots` of the one authorization that minted it; another run (or
//!   a later run of the same employee) is refused even if it obtained it.
//! * Fail closed: no backend, no feature source, a readiness view without
//!   `work_broker`, an unreadable incarnation, or any identity mismatch ⇒ no
//!   Workspace grant (tools hidden) or a refused open; readiness reports
//!   `workspace_broker_unavailable` (`isolation_readiness::reasons`).
use std::{path::{Path, PathBuf}, sync::{Arc, OnceLock, atomic::{AtomicBool, Ordering}}};
use duduclaw_isolation::{execution::{BackendError, RunHandle}, ids::{EmployeeId, RunId},
    ipc::{ControlConnectionId, SpawndFeatures}};
use duduclaw_mcp_boundary::resources::{BrokeredWorkFile, WorkBroker, WorkBrokerError, WorkFileRequest};

/// The protected backend as a run broker needs it. Production:
/// [`SpawndRunTransport`] over the one `SpawndBackend` that reserves runs.
pub(crate) trait RunWorkTransport: Send + Sync {
    /// Whether the backend has a readiness source (no source ⇒ no work op).
    fn has_feature_source(&self) -> bool;
    /// Host-wide readiness view (issuance-time selection only).
    fn host_features(&self) -> SpawndFeatures;
    /// The run's own control connection as this backend reserved it.
    fn run_transport(&self, run: RunHandle) -> Option<duduclaw_execution::spawnd::RunTransport>;
    /// H5 `OpenWorkFile` on `request.run()`'s own control connection, only
    /// while that connection is still `control` (else `NotLive`, nothing sent).
    fn open_work_file(&self, request: &WorkFileRequest<'_>, control: ControlConnectionId)
        -> Result<BrokeredWorkFile, WorkBrokerError>;
}

/// [`RunWorkTransport`] over the protected backend and H5's bridge.
pub(crate) struct SpawndRunTransport {
    backend: Arc<duduclaw_execution::SpawndBackend>,
    bridge: duduclaw_mcp_boundary::work_bridge::SpawndWorkspaceBridge,
}
impl SpawndRunTransport {
    pub(crate) fn new(backend: Arc<duduclaw_execution::SpawndBackend>) -> Self {
        Self { bridge: duduclaw_mcp_boundary::work_bridge::SpawndWorkspaceBridge::new(backend.clone()), backend }
    }
}
impl RunWorkTransport for SpawndRunTransport {
    fn has_feature_source(&self) -> bool { self.backend.has_feature_source() }
    fn host_features(&self) -> SpawndFeatures { self.backend.features() }
    fn run_transport(&self, run: RunHandle) -> Option<duduclaw_execution::spawnd::RunTransport> {
        self.backend.run_transport(run)
    }
    fn open_work_file(&self, request: &WorkFileRequest<'_>, control: ControlConnectionId)
        -> Result<BrokeredWorkFile, WorkBrokerError> {
        self.bridge.open_on_control(request, control)
    }
}

/// Live `(employee, incarnation)` of a label; `None` = not a live protected
/// employee or unreadable (fail closed).
pub(crate) type IncarnationReader = Arc<dyn Fn(&Path, &str) -> Option<(EmployeeId, u64)> + Send + Sync>;

/// F6m's index reader (apply.db `confirmed_employees`; no root I/O). The
/// boundary's `verify_live` already re-reads the full lifecycle; this binds
/// the broker itself to the incarnation the issuer pinned.
fn installed_incarnation(home: &Path, label: &str) -> Option<(EmployeeId, u64)> {
    use duduclaw_core::identity_token::{protected_employee, ProtectedEmployee};
    match protected_employee(home, label) {
        Ok(ProtectedEmployee::Live(binding)) => Some((EmployeeId(binding.employee), binding.incarnation)),
        _ => None,
    }
}

/// Boot's broker factory. Holds the backend, never a run; `bind` mints a
/// fresh run-bound broker per capability.
#[derive(Clone)]
pub(crate) struct RunBrokers {
    transport: Arc<dyn RunWorkTransport>,
    home: PathBuf,
    identity: IncarnationReader,
}

impl RunBrokers {
    /// `None` when the bridge cannot be built: the backend has no readiness
    /// source, so it would never send a work op (tools stay hidden).
    pub(crate) fn new(home: &Path, transport: Arc<dyn RunWorkTransport>) -> Option<Self> {
        Self::with_identity(home, transport, Arc::new(installed_incarnation))
    }
    pub(crate) fn with_identity(home: &Path, transport: Arc<dyn RunWorkTransport>, identity: IncarnationReader)
        -> Option<Self> {
        if !home.is_absolute() || !transport.has_feature_source() { return None; }
        Some(Self { transport, home: home.to_owned(), identity })
    }
    /// Issuance gate: spawnd advertises `work_broker` right now.
    pub(crate) fn available(&self) -> bool { self.transport.host_features().contains(SpawndFeatures::WORK_BROKER) }
    /// Host-wide readiness view (HX5 helper selector).
    pub(crate) fn host_features(&self) -> SpawndFeatures { self.transport.host_features() }
    /// A fresh broker for one capability of `agent` = (`employee`,
    /// `incarnation`), as pinned by the issuer's lifecycle read.
    pub(crate) fn bind(&self, agent: &str, employee: EmployeeId, incarnation: u32) -> Arc<dyn WorkBroker> {
        Arc::new(RunBoundBroker { transport: self.transport.clone(), home: self.home.clone(), agent: agent.to_owned(),
            identity: self.identity.clone(), employee, incarnation: u64::from(incarnation), run: OnceLock::new() })
    }
}

/// One capability's broker. Not `Clone`; reachable only through the
/// `ResourceRoots` of the authorization that minted it.
struct RunBoundBroker {
    transport: Arc<dyn RunWorkTransport>,
    home: PathBuf,
    agent: String,
    identity: IncarnationReader,
    employee: EmployeeId,
    incarnation: u64,
    /// The first served run and its control connection; never rebound.
    run: OnceLock<(RunId, ControlConnectionId)>,
}

impl RunBoundBroker {
    /// The latched control connection the open must still be on.
    fn admit(&self, request: &WorkFileRequest<'_>) -> Result<ControlConnectionId, WorkBrokerError> {
        if request.employee() != self.employee { return Err(WorkBrokerError::NotLive); }
        if (self.identity)(&self.home, &self.agent) != Some((self.employee, self.incarnation)) {
            return Err(WorkBrokerError::NotLive);
        }
        let live = self.transport.run_transport(RunHandle(request.run())).ok_or(WorkBrokerError::NotLive)?;
        if live.employee != self.employee { return Err(WorkBrokerError::NotLive); }
        // [F17] the run's own admission (narrowed by readiness) decides.
        if !live.features.contains(SpawndFeatures::WORK_BROKER) { return Err(WorkBrokerError::Unavailable); }
        let bound = *self.run.get_or_init(|| (request.run(), live.control));
        if bound != (request.run(), live.control) { return Err(WorkBrokerError::NotLive); }
        Ok(bound.1)
    }
}

impl WorkBroker for RunBoundBroker {
    fn open_work_file(&self, request: &WorkFileRequest<'_>) -> Result<BrokeredWorkFile, WorkBrokerError> {
        let control = self.admit(request)?;
        // The open re-looks the run up; it must still be on the latched
        // connection (a reconnect between admit and send is refused).
        self.transport.open_work_file(request, control)
    }
}

/// Test fixtures only (`BootSources.work_broker` / `SharedEmployee::
/// with_work_broker` doubles): a per-capability employee check and first-run
/// latch. Deliberately weaker than [`RunBoundBroker`] (no incarnation pin, no
/// control-connection latch, no per-run `work_broker` bit, and the latch is
/// set before the inner broker accepts), so it is not compiled into
/// production builds.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn latched(inner: Arc<dyn WorkBroker>, employee: EmployeeId) -> Arc<dyn WorkBroker> {
    Arc::new(Latched { inner, employee, run: OnceLock::new() })
}
#[cfg(any(test, feature = "test-support"))]
struct Latched { inner: Arc<dyn WorkBroker>, employee: EmployeeId, run: OnceLock<RunId> }
#[cfg(any(test, feature = "test-support"))]
impl WorkBroker for Latched {
    fn open_work_file(&self, request: &WorkFileRequest<'_>) -> Result<BrokeredWorkFile, WorkBrokerError> {
        if request.employee() != self.employee || *self.run.get_or_init(|| request.run()) != request.run() {
            return Err(WorkBrokerError::NotLive);
        }
        self.inner.open_work_file(request)
    }
}

/// Boot installed a broker producer (run-bound factory, or a test host broker).
static INSTALLED: AtomicBool = AtomicBool::new(false);
pub(crate) fn mark_installed(installed: bool) { if installed { INSTALLED.store(true, Ordering::Release); } }
/// Readiness: whether boot installed a work broker producer.
pub(crate) fn work_broker_installed() -> bool { INSTALLED.load(Ordering::Acquire) }

/// spawnd's checked readiness report bits (`EvidenceReport.features`). An
/// unreadable, stale or failing report is an error (reads as no features).
pub(crate) fn readiness_features() -> Result<SpawndFeatures, BackendError> {
    let report = duduclaw_spawnd::host::EvidenceReport::read(nix::unistd::getegid().as_raw())
        .map_err(|_| BackendError::Unavailable)?;
    report.check().map_err(|_| BackendError::Unavailable)?;
    Ok(report.features)
}

/// The `FeatureSource` boot installs on the protected backend.
pub(crate) fn readiness_feature_source() -> duduclaw_execution::spawnd::FeatureSource {
    Arc::new(readiness_features)
}

#[cfg(test)]
#[path = "run_broker_tests.rs"]
mod tests;
