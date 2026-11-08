//! Linux production composition. Missing packages deny admission, never select
//! an alternate executor. All slow operations run off the control socket thread.
use std::{collections::{BTreeMap, BTreeSet}, io, os::fd::{AsFd, OwnedFd}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind, PtySize, NetworkPolicy}, ids::{EmployeeId, RunId, RequestId}, ipc::{CleanupReadback, ErrorCode, SpawnRequest}};
use duduclaw_isolation::ipc::{HelperCallId, SpawndFeatures as Features};
use crate::{Result, admission::{BrokerJob, BrokerReply, BrokerStep, Host, HostProgress, LaunchPolicy, Ticket,
        HelperEvent, HelperSignal, HelperStart, HelperStarted},
    helper_host::{CallPlan, HelperAudit, HelperEngine},
    accounts::{self, InstallationEvidence, LinuxAccounts},
    audit::{Detail, Event, Record, disk::AuditWorker}, bootstrap::{BootstrapPlan, Trampoline},
    channels::{OutputRelay, RevocablePair, ConnectionRevoker}, fds::SnapshotAuthority,
    host_state::{RunJournal, RunRecord}, host_worker::{WorkerHost, Scan}, policy::RootPolicy,
    readiness::Readiness, registry::{AccountHost, RegistryStore, EmployeeAccount},
    storage::{Storage, Attachment, UnavailableStorage}, systemd::{UnitPolicy, linux::{Systemd, StartedUnit}}};
pub use crate::host_state::{EvidenceReport, SpawndFeatures, READINESS_PATH};
fn unavailable() -> ErrorCode { ErrorCode::IsolationUnavailable }
fn os<T>(r: io::Result<T>) -> Result<T> { r.map_err(|_| unavailable()) }

/// F/G supply a signature-verified root publication, never a JSON flag or IPC
/// policy. Snapshot mount authority must be derived from an opened gateway mount.
pub trait Publication: Send {
    fn load(&mut self) -> Result<RootPolicy>;
    fn snapshot_authority(&mut self, gateway_uid: u32) -> Result<SnapshotAuthority>;
    fn supervisor_identity(&mut self) -> Result<(u32, u32)>;
    fn anchors(&mut self) -> Result<crate::bootstrap_mounts::RootPublicationAnchors>;
    fn native_profile(&mut self, _: &crate::policy::PublishedRuntime) -> Result<crate::native_trampoline::NativeProfile> { Err(unavailable()) }
    /// Installed root publications enforce MCP projection readback; fixture
    /// publications may implement their own independent launch contract.
    fn prepare_mcp_projection(&mut self, _: &BootstrapPlan, _: &crate::policy::PublishedRuntime) -> Result<()> { Ok(()) }
    fn record_mcp_projection(&mut self, _: &BootstrapPlan, _: &crate::policy::PublishedRuntime,
        _: &crate::bootstrap_mounts::PreparedMounts) -> Result<()> { Ok(()) }
    fn retire_mcp_projection(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
}
pub struct MissingPublication;
impl Publication for MissingPublication {
    fn load(&mut self) -> Result<RootPolicy> { Err(unavailable()) }
    fn snapshot_authority(&mut self, _: u32) -> Result<SnapshotAuthority> { Err(unavailable()) }
    fn supervisor_identity(&mut self) -> Result<(u32, u32)> { Err(unavailable()) }
    fn anchors(&mut self) -> Result<crate::bootstrap_mounts::RootPublicationAnchors> { Err(unavailable()) }
}
/// C's DNS/secondary BPF probe is independent of successful control connection.
pub trait Egress: Send {
    fn readiness(&mut self) -> Result<crate::readiness::EgressSecondLayer>;
    fn register(&mut self, run: RunId, profile: duduclaw_isolation::egress::ProfileId, fd: OwnedFd) -> Result<()>;
    fn unregister(&mut self, run: RunId) -> Result<()>;
}
pub trait DnsEvidence: Send { fn verify(&mut self) -> Result<crate::readiness::EgressSecondLayer>; }
pub struct MissingDnsEvidence;
impl DnsEvidence for MissingDnsEvidence { fn verify(&mut self) -> Result<crate::readiness::EgressSecondLayer> { Err(unavailable()) } }
pub struct LinuxEgress { dns: Box<dyn DnsEvidence>, client: Option<crate::egress::EgressControlClient> }
impl LinuxEgress {
    pub fn new(dns: Box<dyn DnsEvidence>) -> Self { Self { dns, client: None } }
    fn client(&mut self) -> Result<&mut crate::egress::EgressControlClient> {
        if self.client.is_none() {
            let (uid, gid) = os(crate::server::named_identity(c"duduclaw-egress"))?;
            self.client = Some(os(crate::egress::EgressControlClient::connect(uid, gid))?);
        }
        self.client.as_mut().ok_or_else(unavailable)
    }
}
impl Egress for LinuxEgress {
    fn readiness(&mut self) -> Result<crate::readiness::EgressSecondLayer> { let _ = self.client()?; self.dns.verify() }
    fn register(&mut self, run: RunId, profile: duduclaw_isolation::egress::ProfileId, fd: OwnedFd) -> Result<()> {
        let request = RequestId(os(crate::server::random_id())?);
        let result = os(self.client()?.register_owned(request, run, profile, fd));
        if result.is_err() { self.client = None; }
        result
    }
    fn unregister(&mut self, run: RunId) -> Result<()> {
        let request = RequestId(os(crate::server::random_id())?);
        let result = os(self.client()?.unregister(request, run));
        if result.is_err() { self.client = None; }
        result
    }
}
pub struct Components {
    pub installation: Arc<dyn InstallationEvidence>,
    pub publication: Box<dyn Publication>,
    pub storage: Box<dyn Storage>,
    pub trampoline: Box<dyn Trampoline>,
    pub egress: Box<dyn Egress>,
}
impl Default for Components {
    fn default() -> Self { Self { installation: Arc::new(accounts::MissingInstallationEvidence),
        publication: Box::new(MissingPublication), storage: Box::new(UnavailableStorage),
        trampoline: Box::new(crate::native_trampoline::NativeTrampoline::default()), egress: Box::new(LinuxEgress::new(Box::new(MissingDnsEvidence))) } }
}
/// Fixed root installer layout. These are not request/config/environment inputs.
/// The installer creates 0700 root-owned directories and makes the image anchor
/// a private mount on H-supported backing. Boot never creates/mounts that anchor.
pub const STORAGE_IMAGES: &str = "/var/lib/duduclaw-spawnd/images";
pub const STORAGE_JOURNAL: &str = "/var/lib/duduclaw-spawnd/storage";
impl Components {
    pub fn installed() -> Self {
        let mut components = Self::default();
        crate::installed::install(&mut components);
        // H3: the allocation/retire hook wraps H's storage so every provisioned
        // employee has its root-owned work anchor (`employee-work` identity).
        let storage = crate::storage::LoopStorage::open(std::path::Path::new(STORAGE_IMAGES), std::path::Path::new(STORAGE_JOURNAL))
            .and_then(|storage| Ok(crate::work_anchor::AnchoredStorage::new(Box::new(storage),
                crate::work_anchor::WorkAnchors::open_installed()?)));
        match storage {
            Ok(storage) => components.storage = Box::new(storage),
            Err(_) => eprintln!("duduclaw-spawnd: installed storage unavailable"),
        }
        components
    }
}
#[path = "host_monitor.rs"]
mod monitor;
use monitor::{Monitor, ScanEntry, ScanReader};
#[cfg(test)]
use monitor::ScanState;
/// HX4 helper glue of `Core` (engine link, per-call start/signal, audit).
#[path = "host_helpers.rs"]
mod helpers;
use helpers::emit_helper;
/// HX6 security fix round (finding 4): the per-host recording probe.
#[path = "host_probe.rs"]
mod probe;
fn proc_cgroup(path: &std::path::Path) -> Result<String> {
    let relative = path.strip_prefix("/sys/fs/cgroup").map_err(|_| unavailable())?;
    let text = relative.to_str().ok_or_else(unavailable)?;
    if text.is_empty() { return Err(unavailable()); }
    Ok(format!("/{text}"))
}
fn emit(audit: &AuditWorker, event: Event, employee: Option<EmployeeId>, run: Option<RunId>) {
    emit_detail(audit, event, employee, run, None);
}
fn emit_detail(audit: &AuditWorker, event: Event, employee: Option<EmployeeId>, run: Option<RunId>, detail: Option<Detail>) {
    let at_unix_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let _ = audit.emit(1, Record { at_unix_ms, event, employee, run, suppressed: 0, detail });
}
/// A root run record as `idle_report` sees it.
struct KnownRun { run: RunId, employee: EmployeeId, open: bool }
/// H4 `EmployeeIdle` counts from already-read root sources (pure, so the
/// rules are testable without root): open root runs and crash-journal records
/// of the employee (by id or uid), its durable leases, the uid/gid process
/// count, and every `duduclaw-run-*` unit that maps to it or to nobody.
fn idle_report(employee: EmployeeId, uid: u32, known: &[KnownRun], journal: &[RunRecord], attachments: &[Attachment],
    processes: usize, units: &[RunId]) -> duduclaw_isolation::ipc::EmployeeIdleReport {
    let count = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    let mut runs: BTreeSet<RunId> = known.iter().filter(|r| r.employee == employee && r.open).map(|r| r.run).collect();
    runs.extend(journal.iter().filter(|r| r.employee == employee || r.uid == uid).map(|r| r.run));
    let leases = attachments.iter().filter(|a| a.employee == employee).count();
    let units = units.iter().filter(|unit| {
        let owner = known.iter().find(|r| r.run == **unit).map(|r| r.employee)
            .or_else(|| journal.iter().find(|r| r.run == **unit).map(|r| r.employee));
        owner.is_none_or(|owner| owner == employee)
    }).count();
    duduclaw_isolation::ipc::EmployeeIdleReport { runs: count(runs.len()), attachments: count(leases),
        processes: count(processes), units: count(units) }
}
struct LiveRun {
    employee: EmployeeId, unit: Option<StartedUnit>, attachment: Option<Attachment>,
    revokers: Vec<Arc<ConnectionRevoker>>, gateway_io: Option<Vec<OwnedFd>>,
    relays: Vec<OutputRelay>, egress: bool, pty: bool, pty_taken: bool, closed: bool, started_event: bool, truncated_event: bool,
    runtime_deadline: Instant, bootstrap_attempted: bool,
    projection: Option<crate::bootstrap_mounts::RootPublicationAnchors>,
    /// H4: root-held RO detached clone of the whole work tree (taken before
    /// `select_workspace`) and the pins of in-flight snapshot copies; released
    /// (and drained) before any `Storage::detach` of this run.
    broker: crate::work_broker::BrokerRoot,
    /// HX4: the parent runtime the helper table is derived from (per call,
    /// from this run's employee) and the run's helper engine, linked from the
    /// trampoline once Running; shut down before the trampoline is closed.
    helper_parent: Option<crate::policy::PublishedRuntime>,
    helper: Option<RunHelper>,
    /// The run's MCP revoker (also in `revokers`): revoked when the helper
    /// engine reports the runtime exited (security fix round, finding 4).
    mcp_revoker: Option<Arc<ConnectionRevoker>>,
}
/// HX4 per-run helper state on the lifecycle thread.
enum RunHelper { Engine(Box<HelperEngine<crate::helper_cgroup::linux::UnitTree>>), Unavailable(ErrorCode) }
/// The core owns every privileged side effect. WorkerHost supplies bounded
/// mailboxes and the independent UID monitor supplies live fail-closed fencing.
struct PublishedView { report: EvidenceReport, policy: Option<RootPolicy>, employees: BTreeSet<EmployeeId> }
pub struct LinuxHost { worker: WorkerHost, view: Arc<Mutex<PublishedView>>, scanner: ScanReader, pub snapshots: SnapshotAuthority, pub host_ptys: u32 }
struct Core {
    allocator: crate::allocation::Allocator,
    registry: RegistryStore, systemd: Systemd, journal: RunJournal, components: Components,
    policy: Option<RootPolicy>, report: EvidenceReport, gateway: (u32, u32), monitor: Monitor,
    runs: BTreeMap<RunId, LiveRun>, audit: AuditWorker, last_refresh: Instant, poisoned: bool,
    view: Arc<Mutex<PublishedView>>,
    /// HX4 events of engines already shut down (run cleanup).
    helper_events: Vec<HelperEvent>,
    /// HX6 finding 4: this host's recording probe and the runs it was tried in.
    recording_probe: probe::RecordingProbe,
    probe_tried: BTreeSet<RunId>,
}
impl LinuxHost {
    pub fn open(gateway: (u32, u32), components: Components, audit: AuditWorker) -> io::Result<Self> {
        let registry = RegistryStore::initialize()?;
        let mut accounts = LinuxAccounts { evidence: components.installation.as_ref() };
        registry.registry().validate(&accounts.policy()?)?;
        let monitor = Monitor::start(&registry.registry().employees, audit.clone())?;
        let scanner = ScanReader { state: monitor.state.clone(), entries: monitor.entries.clone() };
        let worker_scanner = Box::new(ScanReader { state: monitor.state.clone(), entries: monitor.entries.clone() });
        let view = Arc::new(Mutex::new(PublishedView {
            report: EvidenceReport::current(Readiness::default(), 0)?, policy: None,
            employees: registry.registry().employees.iter().filter(|a| a.state == crate::registry::AccountState::Ready).map(|a| a.employee).collect(),
        }));
        let mut core = Core { allocator: crate::allocation::Allocator::open()?, registry, systemd: Systemd::connect()?, journal: RunJournal::open()?,
            components, policy: None, report: EvidenceReport::current(Readiness::default(), 0)?, gateway,
            monitor, runs: BTreeMap::new(), audit, last_refresh: Instant::now(), poisoned: false, view: view.clone(),
            helper_events: Vec::new(), recording_probe: probe::RecordingProbe::default(), probe_tried: BTreeSet::new() };
        core.report.publish(gateway.1)?;
        // Cleanup is attempted even when C/H/F/G are absent. Failure is evidence,
        // never permission to skip an orphan or reinterpret a stale reservation.
        core.report.checks.startup_reconciled = core.reconcile().is_ok();
        // H4: anchors for allocations made before H3 (idempotent, audited).
        core.backfill_anchors();
        let snapshots = core.components.publication.snapshot_authority(gateway.0)
            .unwrap_or(SnapshotAuthority { gateway_uid: gateway.0, mount_id: 0 });
        let host_ptys = crate::systemd::linux::host_pty_budget().unwrap_or(0);
        core.refresh();
        let worker = WorkerHost::spawn(core, worker_scanner)?;
        crate::allocation::serve(gateway, worker.allocation_sender())?;
        Ok(Self { worker, view, scanner, snapshots, host_ptys })
    }
}
impl Core {
    fn reconcile(&mut self) -> Result<()> {
        let prior_units = os(self.systemd.list_runs())?;
        let mut runs: BTreeSet<_> = prior_units.into_iter().collect();
        for record in &self.journal.records {
            if !self.registry.registry().employees.iter().any(|a| a.employee == record.employee && a.uid == record.uid) { return Err(unavailable()); }
            runs.insert(record.run);
        }
        let attachments = self.components.storage.attachments();
        if let Ok(ref attachments) = attachments { for attachment in attachments { runs.insert(attachment.run.0); } }
        let mut revoked = true;
        for run in &runs { revoked &= self.components.egress.unregister(*run).is_ok(); }
        let units = self.systemd.reconcile_all();
        // Do not short circuit account/process quarantine on an unavailable C/H.
        let accounts = self.registry.reconcile(&mut LinuxAccounts { evidence: self.components.installation.as_ref() });
        let mut absent = true;
        for account in &self.registry.registry().employees {
            for intruder in os(accounts::unexpected_processes(account.uid, None))? {
                os(accounts::terminate_intruder(&intruder, account.uid, None))?;
            }
            absent &= os(accounts::unexpected_processes(account.uid, None))?.is_empty();
        }
        os(units)?; os(accounts)?;
        if !revoked || !absent { return Err(unavailable()); }
        for attachment in attachments? {
            if !self.registry.registry().employees.iter().any(|a| a.employee == attachment.employee) { return Err(unavailable()); }
            self.components.storage.detach(&attachment)?;
        }
        if !self.components.storage.attachments()?.is_empty() { return Err(unavailable()); }
        let anchors = self.components.publication.anchors()?;
        for run in os(crate::bootstrap_mounts::projection_runs(&anchors))? { os(crate::bootstrap_mounts::cleanup_projection(&anchors, run))?; }
        for run in runs { os(self.journal.remove(run))?; }
        Ok(())
    }
    fn refresh(&mut self) {
        let probe_started = crate::host_state::boottime_ms().unwrap_or(0);
        let startup_reconciled = self.report.checks.startup_reconciled && !self.poisoned;
        let mut checks = Readiness { startup_reconciled, ..Readiness::default() };
        let registry = self.registry.registry().verify(&mut LinuxAccounts { evidence: self.components.installation.as_ref() }).is_ok();
        self.policy = self.components.publication.load().ok();
        let snapshot = self.components.publication.snapshot_authority(self.gateway.0);
        checks.registry_verified = registry && self.policy.is_some() && snapshot.is_ok_and(|s| s.gateway_uid == self.gateway.0 && s.mount_id != 0);
        checks.accounts_locked = registry;
        checks.ssh_live_negative_checks = registry;
        checks.cron_at_acl_and_jobs_verified = registry;
        checks.external_iam_reservation_verified = registry;
        let mut landlock = None;
        if let Ok(evidence) = self.components.trampoline.readiness() {
            checks.trampoline_verified = evidence.check().is_ok();
            checks.landlock_self_test = evidence.landlock_self_test;
            checks.namespace_self_test = evidence.namespace_self_test;
            if checks.trampoline_verified {
                static LOG_ABI: std::sync::Once = std::sync::Once::new();
                LOG_ABI.call_once(|| eprintln!("duduclaw-spawnd: Landlock detected_abi={} selected_abi={}",
                    evidence.detected_landlock_abi, evidence.selected_landlock_abi));
                landlock = Some(crate::host_state::LandlockEvidence { detected_abi: evidence.detected_landlock_abi,
                    selected_abi: evidence.selected_landlock_abi });
            }
        }
        if let Ok(layer) = self.components.egress.readiness() {
            checks.egress_verified = true; checks.dns_route_verified = true; checks.egress_second_layer = layer;
        }
        checks.storage_verified = self.components.storage.readiness().is_ok();
        checks.process_scan_verified = self.monitor.state.lock().is_ok_and(|state|
            state.at.elapsed() < Duration::from_secs(1) && state.result.as_ref().is_ok_and(Vec::is_empty));
        let ptys = self.systemd.resources_ready();
        checks.resources_verified = ptys.is_ok();
        match EvidenceReport::current(checks, ptys.unwrap_or(0)) {
            Ok(mut report) => {
                report.checked_boottime_ms = probe_started;
                report.landlock = landlock;
                // HX6 finding 4: this host's probe, bound to this boot and ABI.
                report.recording_probe = self.recording_probe.evidence();
                // [F17, H4] the same set every `Reserved` carries (LinuxHost::features).
                let features = self.features().without(Features::HELPER_RECORDING);
                report.features = if features.contains(Features::HELPER_CALLS) && report.recording_probe_passed() {
                    features.with(Features::HELPER_RECORDING)
                } else { features };
                self.report = report;
                if self.report.publish(self.gateway.1).is_err() { self.poisoned = true; self.report.checks.startup_reconciled = false; }
            },
            Err(_) => self.poisoned = true,
        }
        if let Ok(mut view) = self.view.lock() {
            view.report = self.report.clone(); view.policy = if self.poisoned { None } else { self.policy.clone() };
        } else { self.poisoned = true; }
        self.last_refresh = Instant::now();
    }
    fn account(&self, employee: EmployeeId) -> Result<EmployeeAccount> { os(self.registry.registry().lookup(employee)).cloned() }
    /// H4 upgrade step (H3 open item 1): employees allocated before H3 have
    /// no anchor. Create the missing ones for every allocation Ready in both
    /// the journal and the registry; idempotent, never repairs; audited.
    fn backfill_anchors(&mut self) {
        let ready: BTreeSet<EmployeeId> = self.allocator.ready_employees().into_iter().collect();
        let employees: Vec<EmployeeId> = self.registry.registry().employees.iter()
            .filter(|a| a.state == crate::registry::AccountState::Ready && ready.contains(&a.employee))
            .map(|a| a.employee).collect();
        for (employee, outcome) in crate::work_anchor::backfill(self.components.storage.as_mut(), &employees) {
            match outcome {
                crate::work_anchor::Backfilled::Present => {}
                crate::work_anchor::Backfilled::Created => emit(&self.audit, Event::WorkAnchorBackfilled, Some(employee), None),
                crate::work_anchor::Backfilled::Failed(code) => emit_detail(&self.audit, Event::WorkAnchorBackfillFailed,
                    Some(employee), None, Some(Detail::Code { code })),
            }
        }
    }
    /// H4 OpenWorkFile, lifecycle-thread half: path, liveness and lease in
    /// this worker turn, from root state only; the copy is deferred.
    fn broker_open(&mut self, run: RunHandle, employee: EmployeeId, path: duduclaw_isolation::ipc::WorkPath) -> Result<BrokerStep> {
        use crate::work_broker::{check_work_path, copy_snapshot, verify_lease, LiveRunView};
        check_work_path(path.as_str()).map_err(|refusal| refusal.code())?;
        let account = self.account(employee).map_err(|_| ErrorCode::WorkspaceBrokerUnavailable)?;
        let entry = self.runs.get(&run.0).ok_or(ErrorCode::NotFound)?;
        // The same fence the trampoline and the UID monitor use; raised on
        // revoke/kill/cancel/intrusion, so a draining run is never Running here.
        let cancelled = self.monitor.cancellation(run).map_err(|_| ErrorCode::NotReady)?;
        let running = !entry.closed && entry.started_event && entry.unit.is_some() && !cancelled.load(Ordering::Acquire);
        let view = LiveRunView { run, employee: entry.employee, uid: account.uid, running,
            receipt: entry.attachment.as_ref(), work_root: entry.broker.root() };
        let expected = verify_lease(&view, self.components.storage.as_mut(), run, employee).map_err(|refusal| refusal.code())?;
        let lease = entry.broker.lease().map_err(|refusal| refusal.code())?;
        Ok(BrokerStep::Deferred(Box::new(move || {
            let live = || !cancelled.load(Ordering::Acquire);
            let snapshot = copy_snapshot(lease, &path, &expected, &live).map_err(|refusal| refusal.code())?;
            let source = Detail::WorkSource { device_major: snapshot.source.device_major,
                device_minor: snapshot.source.device_minor, inode: snapshot.source.inode, size: snapshot.source.size };
            Ok(BrokerReply::WorkFile { metadata: snapshot.metadata, file: snapshot.file, source: Some(source) })
        })))
    }
    /// H4 WorkAnchor: allocated employees only (registry Ready and journal
    /// Ready); a missing or wrong anchor is `WorkspaceBrokerUnavailable`.
    fn broker_anchor(&mut self, employee: EmployeeId) -> Result<duduclaw_isolation::ipc::WorkAnchorIdentity> {
        let account = self.registry.registry().employees.iter().find(|a| a.employee == employee).cloned().ok_or(ErrorCode::NotFound)?;
        match account.state {
            crate::registry::AccountState::Ready => {}
            crate::registry::AccountState::Tombstone => return Err(ErrorCode::NotFound),
            crate::registry::AccountState::Reserved => return Err(ErrorCode::NotReady),
        }
        if !self.allocator.ready_employees().contains(&employee) { return Err(ErrorCode::NotReady); }
        self.components.storage.work_anchor(employee).map_err(|_| ErrorCode::WorkspaceBrokerUnavailable)
    }
    /// H4 EmployeeIdle (F6 §3.12 step 4, M8, S14): every source is read or
    /// the whole answer is an error. Any registry account state (a tombstoned
    /// employee is exactly what restore/retire ask about); no account ⇒ NotFound.
    fn employee_idle(&mut self, employee: EmployeeId) -> Result<duduclaw_isolation::ipc::EmployeeIdleReport> {
        let account = self.registry.registry().employees.iter().find(|a| a.employee == employee).cloned().ok_or(ErrorCode::NotFound)?;
        let known: Vec<KnownRun> = self.runs.iter().map(|(id, r)| KnownRun { run: *id, employee: r.employee, open: !r.closed }).collect();
        let attachments = self.components.storage.attachments()?;
        let processes = os(accounts::employee_process_count(account.uid, account.gid))?;
        let units = os(self.systemd.list_runs())?;
        Ok(idle_report(employee, account.uid, &known, &self.journal.records, &attachments, processes, &units))
    }
    fn cleanup_run(&mut self, employee: EmployeeId, previous: Option<RunHandle>) -> Result<CleanupReadback> {
        let account = self.account(employee)?;
        let uid_absent = os(accounts::unexpected_processes(account.uid, None))?.is_empty();
        let (unit_inactive, cgroup_empty) = if let Some(run) = previous {
            let Some(entry) = self.runs.get(&run.0) else {
                if self.journal.records.iter().any(|r| r.run == run.0) || os(self.systemd.list_runs())?.contains(&run.0) { return Err(unavailable()); }
                if uid_absent { self.monitor.set_run(employee, None, None, vec![])?; }
                return Ok(CleanupReadback { unit_inactive: true, cgroup_empty: true, employee_uid_absent: uid_absent });
            };
            if entry.employee != employee { return Err(unavailable()); }
            match &entry.unit {
                Some(unit) => os(self.systemd.cleanup(unit))?,
                None => {
                    // A StartTransientUnit reply can be lost after PID 1 accepted
                    // it. Inspect by root-generated name; never assume no receipt
                    // means no unit.
                    if os(self.systemd.list_runs())?.contains(&run.0) {
                        let unit = os(self.systemd.inspect_run(run.0))?;
                        let proof = os(self.systemd.cleanup(&unit))?;
                        self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.unit = Some(unit);
                        proof
                    } else { (true, true) }
                }
            }
        } else {
            (!self.runs.values().any(|r| r.employee == employee && !r.closed), true)
        };
        let proof = CleanupReadback { unit_inactive, cgroup_empty, employee_uid_absent: uid_absent };
        if proof.complete() {
            if let Some(run) = previous {
                let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
                if !entry.closed {
                    // HX4: live helper calls end RunEnded and the launcher
                    // channel closes before the trampoline and its fds go.
                    if let Some(RunHelper::Engine(mut engine)) = entry.helper.take() {
                        engine.shutdown();
                        self.helper_events.extend(engine.take_events());
                        for fact in engine.take_audit() { emit_helper(&self.audit, employee, run.0, fact); }
                    }
                    entry.helper = Some(RunHelper::Unavailable(ErrorCode::NotReady));
                    // H4: the broker clone pins the employee filesystem. Drop it
                    // first, and wait for in-flight snapshot copies (each holds a
                    // dup and a pin; bounded by the 10 s copy deadline) before
                    // anything is detached.
                    entry.broker.release()?;
                    if entry.bootstrap_attempted { self.components.trampoline.close(run)?; }
                    // An attach reply can be lost after H persisted the mount.
                    // Its durable inventory, not our optional receipt, is authority.
                    for attachment in self.components.storage.attachments()?.into_iter().filter(|a| a.run == run) {
                        attachment.check_identity(employee, run)?;
                        self.components.storage.detach(&attachment)?;
                    }
                    if self.components.storage.attachments()?.iter().any(|a| a.run == run) { return Err(unavailable()); }
                    if let Some(anchors) = &entry.projection { os(crate::bootstrap_mounts::cleanup_projection(anchors, run))?; }
                    self.components.publication.retire_mcp_projection(run)?;
                    // Detach/close can be slow. Do not timestamp the earlier
                    // cleanup observation as a fresh proof after those jobs.
                    if !os(accounts::unexpected_processes(account.uid, None))?.is_empty() { return Err(ErrorCode::NotReady); }
                    if let Some(unit) = &entry.unit {
                        if os(self.systemd.cleanup(unit))? != (true, true) { return Err(ErrorCode::NotReady); }
                    } else if os(self.systemd.list_runs())?.contains(&run.0) { return Err(ErrorCode::NotReady); }
                    entry.attachment = None;
                    os(self.journal.remove(run.0)).inspect_err(|_| self.poisoned = true)?;
                    entry.closed = true;
                    self.monitor.set_run(employee, None, None, vec![])?;
                    emit(&self.audit, Event::Ended, Some(employee), Some(run.0));
                }
            }
        }
        Ok(proof)
    }
}
impl Host for Core {
    fn allocation(&mut self, request: &duduclaw_migration::allocation::Request) -> Result<duduclaw_migration::allocation::Receipt> {
        if self.poisoned { return Err(unavailable()); }
        // Allocation never races an active run or the lifecycle's H journal.
        if request.action != duduclaw_migration::allocation::Action::ReadBack && !self.runs.is_empty() { return Err(ErrorCode::NotReady); }
        let mut accounts = LinuxAccounts { evidence: self.components.installation.as_ref() };
        let result = self.allocator.handle(request, &mut self.registry, &mut accounts, self.components.storage.as_mut());
        let mut entries = self.monitor.entries.lock().map_err(|_| unavailable())?;
        // Even a failed allocation may have created a durable UID reservation.
        // Monitor every such identity before returning its failure to the gateway.
        for account in &self.registry.registry().employees {
            if let Some(entry) = entries.iter_mut().find(|e| e.account.employee == account.employee) {
                entry.account = account.clone();
                if account.state != crate::registry::AccountState::Ready { entry.cancelled.store(true, Ordering::Release); }
            } else {
                entries.push(ScanEntry { account: account.clone(), run: None, cgroup: None, revokers: vec![],
                    cancelled: Arc::new(AtomicBool::new(account.state != crate::registry::AccountState::Ready)), deadline: None });
            }
        }
        drop(entries);
        self.last_refresh = Instant::now() - Duration::from_secs(10);
        result
    }
    fn broker(&mut self, job: BrokerJob) -> Result<BrokerStep> {
        if self.poisoned {
            return Err(if matches!(job, BrokerJob::EmployeeIdle { .. }) { unavailable() } else { ErrorCode::WorkspaceBrokerUnavailable });
        }
        match job {
            BrokerJob::OpenWorkFile { run, employee, path } => self.broker_open(run, employee, path),
            BrokerJob::WorkAnchor { employee } => self.broker_anchor(employee).map(|anchor| BrokerStep::Done(BrokerReply::WorkAnchor(anchor))),
            BrokerJob::EmployeeIdle { employee } => self.employee_idle(employee).map(|report| BrokerStep::Done(BrokerReply::Idle(report))),
        }
    }
    /// HX4 host-level availability, the source of the `helper_calls` bit:
    /// the trampoline forks launchers, the publication has launcher plans, and
    /// the slot cgroups are not known to lack open-time credentials (probed
    /// on the first helper-enabled run; a failed probe clears the bit).
    fn helpers_ready(&mut self) -> Result<()> {
        if crate::helper_cgroup::linux::open_time_credentials_observed() == Some(false) { return Err(ErrorCode::HelperCgroupUnavailable); }
        if self.poisoned || !duduclaw_run::linux::runner::LAUNCHER_SUPPORTED
            || !self.policy.as_ref().is_some_and(RootPolicy::launchers_active) { return Err(ErrorCode::HelperUnavailable); }
        Ok(())
    }
    fn features(&mut self) -> Features {
        let features = crate::host_state::DECODED_FEATURES;
        if self.helpers_ready().is_err() { return features; }
        let features = features.with(Features::HELPER_CALLS);
        if self.recording_ready() { features.with(Features::HELPER_RECORDING) } else { features }
    }
    /// HX6 gate (security fix round, finding 4): `helper_recording` only after
    /// THIS host's probe (`host_probe.rs`) launched the signed recording helper
    /// through a run's real launcher (F_L ∩ F_H, nested browser sandbox) and
    /// Chromium rendered and exited cleanly, for this boot and Landlock ABI,
    /// AND the build kill switch is open. Otherwise `helper_recording_unsupported`.
    fn recording_ready(&mut self) -> bool { self.report.recording_probe_passed() }
    fn helper_run(&mut self, job: HelperStart<Vec<OwnedFd>>) -> Result<HelperStarted<Vec<OwnedFd>>> { self.run_helper(job) }
    fn helper_signal(&mut self, run: RunHandle, call: HelperCallId, signal: HelperSignal) -> Result<()> {
        self.signal_helper(run, call, signal)
    }
    fn helper_events(&mut self) -> Vec<HelperEvent> { self.collect_helper_events() }
    type Inputs = Vec<OwnedFd>; type Io = Vec<OwnedFd>;
    fn ready(&mut self, employee: EmployeeId) -> Result<()> {
        if self.poisoned { return Err(unavailable()); }
        if self.last_refresh.elapsed() > Duration::from_secs(1) { self.refresh(); }
        if self.poisoned { return Err(unavailable()); }
        self.report.check()?;
        let account = self.account(employee)?;
        os(LinuxAccounts { evidence: self.components.installation.as_ref() }.verify_locked(&account))?;
        self.report.check()

    }
    fn policy(&mut self, employee: EmployeeId, runtime: &RuntimeKind, pty: bool) -> Result<LaunchPolicy> {
        self.ready(employee)?;
        Ok(self.policy.as_ref().ok_or_else(unavailable)?.derive(employee, runtime, pty)?.1)
    }
    fn cleanup(&mut self, employee: EmployeeId, previous: Option<RunHandle>) -> Result<CleanupReadback> { self.cleanup_run(employee, previous) }
    fn start(&mut self, run: RunHandle, employee: EmployeeId, request: &SpawnRequest, launch_policy: &LaunchPolicy, mut inputs: Self::Inputs) -> Result<()> {
        self.monitor.check_run(run)?;
        self.ready(employee)?;
        if !self.cleanup_run(employee, None)?.complete() { return Err(unavailable()); }
        let SpawnRequest::StartRun { runtime_request, pty, prompt_bytes, input_fds, workspace, .. } = request else { return Err(ErrorCode::InvalidRequest); };
        let account = self.account(employee)?;
        let (runtime, _) = self.policy.as_ref().ok_or_else(unavailable)?.derive(employee, runtime_request, *pty)?;
        let runtime = runtime.clone();
        self.components.storage.usage(employee)?.check(employee, &runtime.resources.disk)?;
        // WAL before any external attachment, endpoint or transient unit exists.
        os(self.journal.add(RunRecord { run: run.0, employee, uid: account.uid })).inspect_err(|_| self.poisoned = true)?;
        self.runs.insert(run.0, LiveRun { employee, unit: None, attachment: None, revokers: vec![], gateway_io: Some(vec![]),
            relays: vec![], egress: false, pty: *pty, pty_taken: false, closed: false, started_event: false, truncated_event: false,
            runtime_deadline: Instant::now() + Duration::from_millis(runtime.timeouts.runtime_ms), bootstrap_attempted: false, projection: None,
            broker: crate::work_broker::BrokerRoot::new(None), helper_parent: Some(runtime.clone()), helper: None,
            mcp_revoker: None });
        let mut storage = self.components.storage.attach(employee, account.uid, run, &runtime.resources.disk)?;
        storage.receipt.check(employee, run)?;
        self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.attachment = Some(storage.receipt.clone());
        // H4: the broker's root, before `select_workspace` narrows `storage.work`.
        // A failed clone fails the broker for this run (refusals), not the run.
        let work_root = crate::bootstrap_mounts::work_broker_root(&storage).ok();
        self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.broker = crate::work_broker::BrokerRoot::new(work_root);
        if let Some(selection) = workspace {
            let pin = inputs.pop().ok_or(ErrorCode::InvalidFds)?;
            os(crate::fds::validate_workspace_directory(pin.as_fd(), selection))?;
            os(crate::bootstrap_mounts::select_workspace(&mut storage, selection, account.uid))?;
            // The root-owned detached mount now pins the authorized inode.
            drop(pin);
        }
        let mut gateway_io = Vec::new();
        let mut revokers = Vec::new();
        let mut mcp_revoker = None;
        let mcp = if runtime.mcp {
            let pair = os(RevocablePair::new())?;
            let revoker = Arc::new(os(pair.revoker())?);
            revokers.push(revoker.clone()); mcp_revoker = Some(revoker);
            gateway_io.push(pair.gateway); Some(pair.runtime)
        } else { None };
        let egress = match runtime.network {
            NetworkPolicy::Offline => None,
            NetworkPolicy::Egress(profile) => {
                let pair = os(RevocablePair::new())?; revokers.push(Arc::new(os(pair.revoker())?));
                // Mark before registration: a timeout can follow successful C commit.
                self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.egress = true;
                self.components.egress.register(run.0, profile, pair.gateway)?;
                Some(pair.runtime)
            }
        };
        let output_budget = Arc::new(std::sync::atomic::AtomicU64::new(runtime.resources.output_total_bytes));
        let (out, stdout, out_reader) = os(OutputRelay::pipes_shared(runtime.resources.output_buffer_bytes as usize, output_budget.clone()))?;
        let (err, stderr, err_reader) = os(OutputRelay::pipes_shared(runtime.resources.output_buffer_bytes as usize, output_budget))?;
        let stdin = if *pty { None } else {
            let (reader, writer) = os(nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(Into::into))?;
            gateway_io.extend([writer, out_reader, err_reader]); Some(reader)
        };
        let (supervisor_uid, supervisor_gid) = self.components.publication.supervisor_identity()?;
        let mut nonce = [0; 32]; nonce[..16].copy_from_slice(&os(crate::server::random_id())?); nonce[16..].copy_from_slice(&os(crate::server::random_id())?);
        let mut plan = BootstrapPlan::from_runtime(run, employee, account.uid, account.gid, supervisor_uid, supervisor_gid,
            nonce, &runtime, *pty, *prompt_bytes, input_fds.as_slice(), if matches!(runtime.network, NetworkPolicy::Egress(_)) { Some(18080) } else { None })?;
        plan.pty_size = launch_policy.pty_size;
        plan.validate()?;
        let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        entry.gateway_io = Some(gateway_io); entry.relays = vec![out, err]; entry.revokers = revokers.clone();
        entry.mcp_revoker = mcp_revoker;
        self.monitor.set_run(employee, Some(run.0), None, revokers.clone())?;
        self.components.publication.prepare_mcp_projection(&plan, &runtime)?;
        let anchors = self.components.publication.anchors()?;
        self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.projection = Some(anchors);
        let anchors = self.runs.get(&run.0).and_then(|entry| entry.projection.as_ref()).ok_or_else(unavailable)?;
        let mounts = os(crate::bootstrap_mounts::prepare(&plan, &storage, anchors))?;
        self.components.publication.record_mcp_projection(&plan, &runtime, &mounts)?;
        self.monitor.check_run(run)?;
        self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.bootstrap_attempted = true;
        let mut native_profile = self.components.publication.native_profile(&runtime)?;
        // HX: the helper table is derived from this run's employee, never from
        // the request (D8 revision §2.3). Planned only once the trampoline has a launcher.
        if duduclaw_run::linux::runner::LAUNCHER_SUPPORTED {
            let policy = self.policy.as_ref().ok_or_else(unavailable)?;
            let table = policy.helper_table(employee, &runtime);
            if !table.is_empty() {
                let anchors = self.runs.get(&run.0).and_then(|entry| entry.projection.as_ref()).ok_or_else(unavailable)?;
                let executables: Vec<_> = table.iter().map(|entry| &entry.runtime.executable).collect();
                // HX6 security fix round (finding 5): the recording's signed browser files.
                let files: Vec<_> = table.iter().map(|entry|
                    crate::helper_policy::recording_files(&entry.runtime).unwrap_or_default()).collect();
                let prepared = os(crate::bootstrap_mounts::prepare_helpers(anchors, &executables, &files))?;
                let helpers = table.iter().zip(prepared.executables).zip(prepared.files)
                    .map(|((entry, (executable, script)), files)|
                    crate::native_trampoline::HelperRun { index: entry.index, binding: entry.binding.clone(),
                        runtime: entry.runtime.clone(), executable, script, files }).collect();
                native_profile.launcher = Some(crate::native_trampoline::LauncherProfile { helpers, mount: prepared.mount,
                    tmpfs_bytes: policy.helper_tmpfs_bytes(&runtime).ok_or_else(unavailable)?,
                    drain_ms: crate::native_trampoline::launcher_drain_ms(runtime.timeouts.termination_grace_ms) });
            }
        }
        let sealed_plan = plan.seal()?;
        let prepared = self.components.trampoline.prepare(plan, sealed_plan, mounts, inputs, stdin, mcp, egress, native_profile)?;
        os(crate::fds::validate_bootstrap(prepared.bootstrap_for_unit.as_fd()))?;
        let policy = os(UnitPolicy::new(run.0, employee, &runtime.resources, &runtime.timeouts, storage.receipt.device_major, storage.receipt.device_minor))?;
        self.monitor.check_run(run)?;
        let unit = os(self.systemd.start(&policy, prepared.bootstrap_for_unit.as_fd(), stdout.as_fd(), stderr.as_fd()))?;
        let cgroup = proc_cgroup(&unit.cgroup)?;
        // The B2 barrier prevents employee-uid processes before this allowlist exists.
        self.monitor.set_run(employee, Some(run.0), Some(cgroup), revokers)?;
        self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.unit = Some(unit);
        let unit = self.runs.get(&run.0).and_then(|r| r.unit.as_ref()).ok_or_else(unavailable)?;
        self.monitor.check_run(run)?;
        self.components.trampoline.bind_unit(run, unit, self.monitor.cancellation(run)?)?;
        drop(prepared); drop(storage); drop(stdout); drop(stderr);
        Ok(())
    }
    fn poll(&mut self, run: RunHandle) -> Result<HostProgress> {
        let progress = self.components.trampoline.poll(run)?;
        if matches!(progress, HostProgress::Running { .. } | HostProgress::Ended(_)) {
            let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
            if entry.pty && !entry.pty_taken {
                let master = self.components.trampoline.take_pty(run)?;
                entry.gateway_io.as_mut().ok_or(ErrorCode::AlreadyTaken)?.push(master); entry.pty_taken = true;
            }
            if !entry.started_event {
                self.monitor.running(run, entry.runtime_deadline)?;
                emit(&self.audit, Event::Started, Some(entry.employee), Some(run.0)); entry.started_event = true; }
            // HX4: the launcher link moves out of the trampoline once, when Running.
            if matches!(progress, HostProgress::Running { .. }) && entry.helper.is_none() { self.link_helpers(run)?; }
        }
        Ok(progress)
    }
    fn revoke(&mut self, run: RunHandle) -> Result<()> {
        let Some(entry) = self.runs.get(&run.0) else {
            return if self.journal.records.iter().any(|r| r.run == run.0) { Err(unavailable()) } else { Ok(()) };
        };
        let mut failed = false;
        for revoker in &entry.revokers { failed |= revoker.revoke().is_err(); }
        if entry.egress { failed |= self.components.egress.unregister(run.0).is_err(); }
        if failed { return Err(unavailable()); }
        emit(&self.audit, Event::Revoked, Some(entry.employee), Some(run.0)); Ok(())
    }
    fn kill(&mut self, run: RunHandle) -> Result<()> { os(self.systemd.kill_run(run.0)) }
    fn take_io(&mut self, run: RunHandle) -> Result<Self::Io> {
        let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        if entry.pty && !entry.pty_taken { return Err(ErrorCode::NotReady); }
        entry.gateway_io.take().ok_or(ErrorCode::AlreadyTaken)
    }
    fn resize(&mut self, run: RunHandle, size: PtySize) -> Result<()> { self.components.trampoline.resize(run, size) }
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> { ScanReader { state: self.monitor.state.clone(), entries: self.monitor.entries.clone() }.scan() }
    fn forget(&mut self, run: RunHandle) -> Result<()> {
        if let Some(entry) = self.runs.get(&run.0) {
            if !entry.closed { return Err(unavailable()); }
            if let Some(unit) = &entry.unit { os(self.systemd.release_reference(unit))?; }
        }
        self.runs.remove(&run.0); Ok(())
    }
    fn maintenance(&mut self) -> Result<()> {
        // HX4: launcher frames, spawnd's watchdog and the runtime cgroup, every turn.
        self.service_helpers();
        for (run, entry) in &mut self.runs {
            for relay in &mut entry.relays {
                if relay.pump().is_err() {
                    self.poisoned = true;
                    if let Ok(fence) = self.monitor.cancellation(RunHandle(*run)) { fence.store(true, Ordering::Release); }
                }
            }
            if !entry.truncated_event && entry.relays.iter().any(OutputRelay::truncated) {
                emit(&self.audit, Event::OutputTruncated, Some(entry.employee), Some(*run)); entry.truncated_event = true;
            }
        }
        if self.last_refresh.elapsed() >= Duration::from_millis(250) { self.refresh(); }
        Ok(())
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        let runs: Vec<_> = self.runs.keys().copied().collect();
        for run in runs { let _ = self.revoke(RunHandle(run)); let _ = self.kill(RunHandle(run)); }
        self.report.checks.startup_reconciled = false; let _ = self.report.publish(self.gateway.1);
    }
}
impl Host for LinuxHost {
    type Inputs = Vec<OwnedFd>; type Io = Vec<OwnedFd>;
    fn ready(&mut self, e: EmployeeId) -> Result<()> {
        self.scanner.health(e)?;
        let view = self.view.try_lock().map_err(|_| unavailable())?;
        if !view.employees.contains(&e) || view.policy.is_none() { return Err(unavailable()); }
        view.report.check()
    }
    fn policy(&mut self, e: EmployeeId, r: &RuntimeKind, p: bool) -> Result<LaunchPolicy> {
        self.ready(e)?;
        let view = self.view.try_lock().map_err(|_| unavailable())?;
        Ok(view.policy.as_ref().ok_or_else(unavailable)?.derive(e, r, p)?.1)
    }
    fn cleanup(&mut self, e: EmployeeId, r: Option<RunHandle>) -> Result<CleanupReadback> { self.worker.cleanup(e, r) }
    fn start(&mut self, r: RunHandle, e: EmployeeId, q: &SpawnRequest, p: &LaunchPolicy, f: Self::Inputs) -> Result<()> { self.worker.start(r, e, q, p, f) }
    fn poll(&mut self, r: RunHandle) -> Result<HostProgress> { self.worker.poll(r) }
    fn revoke(&mut self, r: RunHandle) -> Result<()> { self.worker.revoke(r) }
    fn kill(&mut self, r: RunHandle) -> Result<()> { self.worker.kill(r) }
    fn take_io(&mut self, r: RunHandle) -> Result<Self::Io> { self.worker.take_io(r) }
    fn resize(&mut self, r: RunHandle, s: PtySize) -> Result<()> { self.worker.resize(r, s) }
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> { self.worker.detect_intruders() }
    fn forget(&mut self, r: RunHandle) -> Result<()> { self.worker.forget(r) }
    fn broker_submit(&mut self, t: Ticket, j: BrokerJob) -> Result<()> { self.worker.broker_submit(t, j) }
    fn broker_poll(&mut self, t: Ticket) -> Result<Option<BrokerReply>> { self.worker.broker_poll(t) }
    fn broker_abandon(&mut self, t: Ticket) { self.worker.broker_abandon(t) }
    /// [F17, H4] exactly the published readiness report's set (one source).
    fn features(&mut self) -> Features {
        self.view.try_lock().map(|view| view.report.features).unwrap_or(crate::host_state::DECODED_FEATURES)
    }
    fn helpers_ready(&mut self) -> Result<()> {
        if self.features().contains(Features::HELPER_CALLS) { return Ok(()); }
        Err(if crate::helper_cgroup::linux::open_time_credentials_observed() == Some(false) { ErrorCode::HelperCgroupUnavailable }
            else { ErrorCode::HelperUnavailable })
    }
    /// HX6 finding 4: the coordinator's recording gate reads the published
    /// report: the bit AND this host's matching probe AND the kill switch.
    fn recording_ready(&mut self) -> bool {
        self.view.try_lock().map(|view| view.report.helper_recording()).unwrap_or(false)
    }
    fn helper_start(&mut self, t: Ticket, job: HelperStart<Vec<OwnedFd>>) -> Result<()> { self.worker.helper_start(t, job) }
    fn helper_poll(&mut self, t: Ticket) -> Result<Option<HelperStarted<Vec<OwnedFd>>>> { self.worker.helper_poll(t) }
    fn helper_signal(&mut self, r: RunHandle, c: HelperCallId, s: HelperSignal) -> Result<()> { self.worker.helper_signal(r, c, s) }
    fn helper_events(&mut self) -> Vec<HelperEvent> { self.worker.helper_events() }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
