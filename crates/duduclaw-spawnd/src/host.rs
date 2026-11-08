//! Linux production composition. Missing packages deny admission, never select
//! an alternate executor. All slow operations run off the control socket thread.
use std::{collections::{BTreeMap, BTreeSet}, io, os::fd::{AsFd, OwnedFd}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind, PtySize, NetworkPolicy}, ids::{EmployeeId, RunId, RequestId}, ipc::{CleanupReadback, ErrorCode, SpawnRequest}};
use crate::{Result, admission::{Host, HostProgress, LaunchPolicy}, accounts::{self, InstallationEvidence, LinuxAccounts},
    audit::{Event, Record, disk::AuditWorker}, bootstrap::{BootstrapPlan, Trampoline},
    channels::{OutputRelay, RevocablePair, ConnectionRevoker}, fds::SnapshotAuthority,
    host_state::{RunJournal, RunRecord}, host_worker::{WorkerHost, Scan}, policy::RootPolicy,
    readiness::Readiness, registry::{AccountHost, RegistryStore, EmployeeAccount},
    storage::{Storage, Attachment, UnavailableStorage}, systemd::{UnitPolicy, linux::{Systemd, StartedUnit}}};
pub use crate::host_state::{EvidenceReport, READINESS_PATH};
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
#[derive(Clone)]
struct ScanEntry { account: EmployeeAccount, run: Option<RunId>, cgroup: Option<String>, revokers: Vec<Arc<ConnectionRevoker>>, cancelled: Arc<AtomicBool>, deadline: Option<Instant> }
struct ScanState { at: Instant, result: Result<Vec<EmployeeId>> }
struct Monitor { entries: Arc<Mutex<Vec<ScanEntry>>>, state: Arc<Mutex<ScanState>>, stop: Arc<AtomicBool> }
impl Monitor {
    fn start(accounts: &[EmployeeAccount], audit: AuditWorker) -> io::Result<Self> {
        let entries = Arc::new(Mutex::new(accounts.iter().cloned().map(|account|
            ScanEntry { account, run: None, cgroup: None, revokers: vec![], cancelled: Arc::new(AtomicBool::new(false)), deadline: None }).collect::<Vec<_>>()));
        let state = Arc::new(Mutex::new(ScanState { at: Instant::now(), result: Err(unavailable()) }));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_entries, worker_state, worker_stop) = (entries.clone(), state.clone(), stop.clone());
        let (kill_sender, kill_receiver) = std::sync::mpsc::sync_channel::<RunId>(128);
        std::thread::Builder::new().name("spawnd-emergency-stop".into()).spawn(move || {
            while let Ok(run) = kill_receiver.recv() {
                if let Ok(systemd) = Systemd::connect() { let _ = systemd.kill_run(run); }
            }
        })?;
        std::thread::Builder::new().name("spawnd-uid-monitor".into()).spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let snapshot = match worker_entries.lock() { Ok(entries) => entries.clone(), Err(_) => break };
                let result = (|| -> Result<Vec<EmployeeId>> {
                    let mut affected = Vec::new();
                    let mut scan_failed = false;
                    for entry in snapshot {
                        let intruders = os(accounts::unexpected_processes(entry.account.uid, entry.cgroup.as_deref()));
                        // Capture cancellation candidates before validating the
                        // generation. Never rescan this UID after dropping the
                        // map lock: a later admission may already own it.
                        let all_processes = accounts::unexpected_processes(entry.account.uid, None);
                        // Re-read the admission generation after the /proc scan.
                        // Compare before acting, then release the map lock. The
                        // process list is already fixed; pidfd/start-time checks
                        // below reject reuse. Unit kills name this immutable run,
                        // so they cannot terminate a later admission generation.
                        let current = worker_entries.lock().map_err(|_| unavailable())?;
                        let live = current.iter().find(|v| v.account.employee == entry.account.employee).ok_or_else(unavailable)?;
                        if live.run != entry.run || live.cgroup != entry.cgroup { continue; }
                        let entry = live.clone();
                        if entry.deadline.is_some_and(|until| Instant::now() >= until)
                            || all_processes.is_err() || intruders.as_ref().is_err() || intruders.as_ref().is_ok_and(|p| !p.is_empty()) {
                            entry.cancelled.store(true, Ordering::Release);
                        }
                        let cancelled = entry.cancelled.load(Ordering::Acquire);
                        drop(current);
                        if cancelled {
                            for revoker in &entry.revokers { let _ = revoker.revoke(); }
                            // PID 1 operations never delay the scanner. Independently
                            // kill every employee process with pidfds immediately;
                            // the queued unit stop also kills root/runsup helpers.
                            if let Some(run) = entry.run { let _ = kill_sender.try_send(run); }
                            match all_processes {
                                Ok(processes) => for process in processes {
                                    scan_failed |= accounts::terminate_intruder(&process, entry.account.uid, None).is_err();
                                },
                                Err(_) => scan_failed = true,
                            }
                        }
                        let intruders = match intruders { Ok(value) => value, Err(_) => { scan_failed = true; continue; } };
                        if !intruders.is_empty() {
                            affected.push(entry.account.employee);
                            emit(&audit, Event::Intruder, Some(entry.account.employee), entry.run);
                            for process in intruders { scan_failed |= accounts::terminate_intruder(&process, entry.account.uid, entry.cgroup.as_deref()).is_err(); }
                        }
                    }
                    if scan_failed { Err(unavailable()) } else { Ok(affected) }
                })();
                if result.is_err() {
                    // Fence every run on an unreadable host scan, including runs
                    // whose UID was after the failed entry in the inventory.
                    if let Ok(entries) = worker_entries.lock() {
                        for entry in entries.iter() {
                            entry.cancelled.store(true, Ordering::Release);
                            for revoker in &entry.revokers { let _ = revoker.revoke(); }
                            if let Some(run) = entry.run { let _ = kill_sender.try_send(run); }
                        }
                    }
                }
                if let Ok(mut state) = worker_state.lock() {
                    // Sticky affected identities until the control loop observes them.
                    let result = match (&state.result, result) {
                        (Ok(old), Ok(mut new)) => { new.extend(old); new.sort(); new.dedup(); Ok(new) },
                        (_, other) => other,
                    };
                    *state = ScanState { at: started, result };
                }
                // Schedule at <=1 second, leave margin for the two-second fence.
                while started.elapsed() < Duration::from_secs(1) && !worker_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        })?;
        Ok(Self { entries, state, stop })
    }
    fn cancellation(&self, run: RunHandle) -> Result<Arc<AtomicBool>> {
        let entries = self.entries.lock().map_err(|_| unavailable())?;
        Ok(entries.iter().find(|e| e.run == Some(run.0)).ok_or_else(unavailable)?.cancelled.clone())
    }
    fn check_run(&self, run: RunHandle) -> Result<()> {
        if self.cancellation(run)?.load(Ordering::Acquire) { Err(unavailable()) } else { Ok(()) }
    }
    fn running(&self, run: RunHandle, deadline: Instant) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| unavailable())?;
        entries.iter_mut().find(|e| e.run == Some(run.0)).ok_or_else(unavailable)?.deadline = Some(deadline);
        Ok(())
    }
    fn set_run(&self, employee: EmployeeId, run: Option<RunId>, cgroup: Option<String>, revokers: Vec<Arc<ConnectionRevoker>>) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| unavailable())?;
        let entry = entries.iter_mut().find(|entry| entry.account.employee == employee).ok_or_else(unavailable)?;
        if run.is_some() && entry.run != run { return Err(unavailable()); }
        entry.run = run; entry.cgroup = cgroup; entry.revokers = revokers;
        if run.is_none() { entry.deadline = None; }
        Ok(())
    }
}
struct ScanReader { state: Arc<Mutex<ScanState>>, entries: Arc<Mutex<Vec<ScanEntry>>> }
impl Scan for ScanReader {
    fn health(&mut self, employee: EmployeeId) -> Result<()> {
        let state = self.state.try_lock().map_err(|_| unavailable())?;
        if state.at.elapsed() >= Duration::from_secs(2) { return Err(unavailable()); }
        if state.result.as_ref().map_err(|e| *e)?.contains(&employee) { return Err(unavailable()); }
        Ok(())
    }
    fn register(&mut self, run: RunHandle, employee: EmployeeId, startup_ms: u64) -> Result<()> {
        let mut entries = self.entries.try_lock().map_err(|_| unavailable())?;
        let entry = entries.iter_mut().find(|v| v.account.employee == employee).ok_or_else(unavailable)?;
        if entry.run.is_some() { return Err(unavailable()); }
        entry.run = Some(run.0); entry.cancelled = Arc::new(AtomicBool::new(false));
        entry.deadline = Some(Instant::now() + Duration::from_millis(startup_ms));
        Ok(())
    }
    fn cancel(&mut self, run: RunHandle) -> Result<()> {
        let entries = self.entries.try_lock().map_err(|_| unavailable())?;
        if let Some(entry) = entries.iter().find(|v| v.run == Some(run.0)) {
            entry.cancelled.store(true, Ordering::Release);
            for revoker in &entry.revokers { os(revoker.revoke())?; }
        }
        Ok(())
    }
    fn scan(&mut self) -> Result<Vec<EmployeeId>> {
        let mut state = self.state.try_lock().map_err(|_| unavailable())?;
        if state.at.elapsed() >= Duration::from_secs(2) { return Err(unavailable()); }
        let result = state.result.clone();
        if result.is_ok() { state.result = Ok(vec![]); }
        result
    }
}
impl Drop for Monitor { fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); } }
fn proc_cgroup(path: &std::path::Path) -> Result<String> {
    let relative = path.strip_prefix("/sys/fs/cgroup").map_err(|_| unavailable())?;
    let text = relative.to_str().ok_or_else(unavailable)?;
    if text.is_empty() { return Err(unavailable()); }
    Ok(format!("/{text}"))
}
fn emit(audit: &AuditWorker, event: Event, employee: Option<EmployeeId>, run: Option<RunId>) {
    let at_unix_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let _ = audit.emit(1, Record { at_unix_ms, event, employee, run, suppressed: 0 });
}
struct LiveRun {
    employee: EmployeeId, unit: Option<StartedUnit>, attachment: Option<Attachment>,
    revokers: Vec<Arc<ConnectionRevoker>>, gateway_io: Option<Vec<OwnedFd>>,
    relays: Vec<OutputRelay>, egress: bool, pty: bool, pty_taken: bool, closed: bool, started_event: bool, truncated_event: bool,
    runtime_deadline: Instant, bootstrap_attempted: bool,
    projection: Option<crate::bootstrap_mounts::RootPublicationAnchors>,
}
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
            monitor, runs: BTreeMap::new(), audit, last_refresh: Instant::now(), poisoned: false, view: view.clone() };
        core.report.publish(gateway.1)?;
        // Cleanup is attempted even when C/H/F/G are absent. Failure is evidence,
        // never permission to skip an orphan or reinterpret a stale reservation.
        core.report.checks.startup_reconciled = core.reconcile().is_ok();
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
            runtime_deadline: Instant::now() + Duration::from_millis(runtime.timeouts.runtime_ms), bootstrap_attempted: false, projection: None });
        let mut storage = self.components.storage.attach(employee, account.uid, run, &runtime.resources.disk)?;
        storage.receipt.check(employee, run)?;
        self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?.attachment = Some(storage.receipt.clone());
        if let Some(selection) = workspace {
            let pin = inputs.pop().ok_or(ErrorCode::InvalidFds)?;
            os(crate::fds::validate_workspace_directory(pin.as_fd(), selection))?;
            os(crate::bootstrap_mounts::select_workspace(&mut storage, selection, account.uid))?;
            // The root-owned detached mount now pins the authorized inode.
            drop(pin);
        }
        let mut gateway_io = Vec::new();
        let mut revokers = Vec::new();
        let mcp = if runtime.mcp {
            let pair = os(RevocablePair::new())?; revokers.push(Arc::new(os(pair.revoker())?));
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
                let prepared = os(crate::bootstrap_mounts::prepare_helpers(anchors, &executables))?;
                let helpers = table.iter().zip(prepared.executables).map(|(entry, (executable, script))|
                    crate::native_trampoline::HelperRun { index: entry.index, binding: entry.binding.clone(),
                        runtime: entry.runtime.clone(), executable, script }).collect();
                native_profile.launcher = Some(crate::native_trampoline::LauncherProfile { helpers, mount: prepared.mount,
                    tmpfs_bytes: policy.helper_tmpfs_bytes(&runtime).ok_or_else(unavailable)? });
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
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> ScanEntry {
        ScanEntry { account: EmployeeAccount { employee: EmployeeId([1; 16]), uid: 70000, gid: 70000,
            state: crate::registry::AccountState::Ready }, run: None, cgroup: None, revokers: vec![],
            cancelled: Arc::new(AtomicBool::new(false)), deadline: None }
    }
    #[test]
    fn cgroup_receipt_is_converted_to_absolute_proc_membership() {
        assert_eq!(proc_cgroup(std::path::Path::new("/sys/fs/cgroup/duduclaw.slice/run.service")).unwrap(), "/duduclaw.slice/run.service");
        assert!(proc_cgroup(std::path::Path::new("/sys/fs/cgroup")).is_err());
        assert!(proc_cgroup(std::path::Path::new("/other/run.service")).is_err());
    }
    #[test]
    fn readiness_does_not_consume_intrusion_events_and_stale_scan_denies() {
        let employee = EmployeeId([1; 16]);
        let state = Arc::new(Mutex::new(ScanState { at: Instant::now(), result: Ok(vec![employee]) }));
        let mut reader = ScanReader { state: state.clone(), entries: Arc::new(Mutex::new(vec![entry()])) };
        assert!(reader.health(employee).is_err());
        assert!(reader.health(employee).is_err());
        assert_eq!(reader.scan().unwrap(), vec![employee]);
        assert!(reader.health(employee).is_ok());
        state.lock().unwrap().at = Instant::now() - Duration::from_secs(3);
        assert!(reader.health(employee).is_err());
        assert!(reader.scan().is_err());
    }
    #[test]
    fn cancelling_a_queued_start_sets_the_same_fence_given_to_trampoline() {
        let employee = EmployeeId([1; 16]); let run = RunHandle(RunId([2; 16]));
        let entries = Arc::new(Mutex::new(vec![entry()]));
        let state = Arc::new(Mutex::new(ScanState { at: Instant::now(), result: Ok(vec![]) }));
        let mut reader = ScanReader { state: state.clone(), entries: entries.clone() };
        let monitor = Monitor { entries, state, stop: Arc::new(AtomicBool::new(false)) };
        reader.register(run, employee, 1000).unwrap();
        let b2_fence = monitor.cancellation(run).unwrap();
        assert!(monitor.check_run(run).is_ok());
        reader.cancel(run).unwrap();
        assert!(b2_fence.load(Ordering::Acquire));
        assert!(monitor.check_run(run).is_err());
        assert!(reader.register(RunHandle(RunId([3; 16])), employee, 1000).is_err());
        monitor.set_run(employee, None, None, vec![]).unwrap();
        reader.register(RunHandle(RunId([3; 16])), employee, 1000).unwrap();
        assert!(b2_fence.load(Ordering::Acquire));
    }
}
