//! D8 admission. Only this coordinator may release an employee lease.
use std::collections::BTreeMap;
use duduclaw_isolation::{bounded::List, execution::{RunHandle, RunOutcome, RuntimeKind, PtySize}, ids::{EmployeeId, RunId},
    ipc::{AdmissionState, CleanupReadback, ControlConnectionId, EmployeeIdleReport, ErrorCode, IoDelivery, QueueClass,
        QueueStatus, RunReservation, RunStatus, SpawnRequest, SpawnResponse, WorkAnchorIdentity, WorkFileMetadata,
        WorkPath, DEFAULT_QUEUE_LENGTH, MAX_STOPPED_RUNS, HelperCallId, SpawndFeatures}};
use crate::Result;

#[path = "admission_helpers.rs"]
mod helpers;
pub use helpers::{effective_timeout_ms, HelperEvent, HelperReply, HelperSignal, HelperStart, HelperStarted,
    CALL_DESCRIPTORS, HELPER_QUEUE_MS, MAX_ENDED_RECORDS, MAX_QUEUED_CALLS, RESULT_GRACE_MS, WATCHDOG_GRACE_MS};

/// H4 caps on outstanding work-file snapshots: submitted to the host and not
/// yet delivered, refused or abandoned. The host copies one snapshot at a time
/// in submission order on its own copy worker, so the opens of one run are
/// serialized; these caps bound root-held snapshot memory (≤ 16 MiB each).
pub const MAX_SNAPSHOTS_PER_RUN: usize = 4;
pub const MAX_SNAPSHOTS: usize = 16;
/// All outstanding H4 tickets (snapshots, anchors, idle reads, stops).
pub const MAX_BROKER_TICKETS: usize = 64;
/// The server answers every deferred H4 reply within this bound, below the
/// gateway client's `CONTROL_TIMEOUT_MS` (5 s); a late host result is dropped.
pub const BROKER_REPLY_MS: u64 = 4_000;
/// `StopEmployeeRuns` waits at most this long, then replies `confirmed = false`.
pub const STOP_WAIT_MS: u64 = 3_500;

/// H4 root broker job. The coordinator has already checked what only it knows
/// (connection ownership, admission state, employee of the run); the host
/// re-derives everything else (lease, uid, anchor, allocation) from root state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerJob {
    OpenWorkFile { run: RunHandle, employee: EmployeeId, path: WorkPath },
    WorkAnchor { employee: EmployeeId },
    EmployeeIdle { employee: EmployeeId },
}
#[derive(Debug)]
pub enum BrokerReply {
    /// `file` is the sealed read-only snapshot (H2); `metadata` its fstat;
    /// `source` the root-side identity of the employee file, for audit only.
    WorkFile { metadata: WorkFileMetadata, file: std::fs::File, source: Option<crate::audit::Detail> },
    WorkAnchor(WorkAnchorIdentity),
    /// Host-side counts; the coordinator adds its own non-terminal entries.
    Idle(EmployeeIdleReport),
}
/// Result of the synchronous host step on the adapter's lifecycle thread.
pub enum BrokerStep {
    Done(BrokerReply),
    /// A bounded, self-contained copy that must not occupy the lifecycle
    /// thread (H2 copies take up to 10 s); run on the adapter's copy worker.
    Deferred(Box<dyn FnOnce() -> Result<BrokerReply> + Send>),
}
/// Server-chosen identity of one outstanding H4 job. Never on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ticket(pub u64);
/// A finished H4 reply; `file` is the single `WorkFileRead` fd, if any;
/// `detail` is audit-only (never sent).
#[derive(Debug)]
pub struct BrokerOutcome { pub response: SpawnResponse, pub file: Option<std::fs::File>, pub detail: Option<crate::audit::Detail> }
impl BrokerOutcome {
    fn error(code: ErrorCode) -> Self { Self { response: SpawnResponse::Error { code }, file: None, detail: None } }
    fn reply(response: SpawnResponse) -> Self { Self { response, file: None, detail: None } }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;

/// Root policy, derived from the registry, never deserialized from an RPC.
#[derive(Clone)]
pub struct LaunchPolicy {
    pub queue_class: QueueClass,
    pub queue_ms: u64,
    pub startup_ms: u64,
    pub runtime_ms: u64,
    pub pty_idle_ms: Option<u64>,
    pub ptys: u16,
    pub pty_size: Option<PtySize>,
    pub delivery: IoDelivery,
}
impl LaunchPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.pty_size.is_some() != self.delivery.pty || self.pty_size.is_some_and(|size| size.validate().is_err()) {
            return Err(ErrorCode::RuntimeDenied);
        }
        if self.queue_ms == 0 || self.queue_ms > 3_600_000 || self.startup_ms == 0
            || self.startup_ms > 300_000 || self.runtime_ms == 0 || self.runtime_ms > 86_400_000
            || self.ptys > 64 || (self.delivery.pty && (self.ptys == 0 || self.pty_idle_ms.is_none()))
            || self.pty_idle_ms.is_some_and(|ms| ms == 0 || ms > self.runtime_ms) {
            return Err(ErrorCode::RuntimeDenied);
        }
        Ok(())
    }
}

/// Integration seam for B2/C/H. Implementations must recheck live account,
/// storage and policy evidence before start. An error may have partial effects:
/// the coordinator keeps the lease until all three cleanup readbacks succeed.
pub trait Host {
    type Inputs;
    type Io;
    fn ready(&mut self, employee: EmployeeId) -> Result<()>;
    fn policy(&mut self, employee: EmployeeId, runtime: &RuntimeKind, pty: bool) -> Result<LaunchPolicy>;
    /// Before returning complete, close original input/mount/bootstrap descriptors
    /// and verify unit inactive, cgroup empty and uid absent. Terminal output
    /// relays must continue pumping independently until forget; their gateway
    /// endpoints remain available for a short command's one-shot TakeIo.
    fn cleanup(&mut self, employee: EmployeeId, previous: Option<RunHandle>) -> Result<CleanupReadback>;
    /// Start only the fixed trampoline with a sealed fd plan. Return does not
    /// mean Running: poll must verify all trampoline and unit readbacks first.
    fn start(&mut self, run: RunHandle, employee: EmployeeId, request: &SpawnRequest,
        policy: &LaunchPolicy, inputs: Self::Inputs) -> Result<()>;
    fn poll(&mut self, run: RunHandle) -> Result<HostProgress>;
    /// Revoke MCP and egress before termination, even if killing later fails.
    fn revoke(&mut self, run: RunHandle) -> Result<()>;
    fn kill(&mut self, run: RunHandle) -> Result<()>;
    fn take_io(&mut self, run: RunHandle) -> Result<Self::Io>;
    fn resize(&mut self, run: RunHandle, size: PtySize) -> Result<()>;
    /// Check every employee uid on the host, including currently idle employees.
    /// Kill intruders via pidfds; return affected employees to fence their runs.
    /// An unreadable scan must return an error, never an empty success.
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>>;
    /// Drop retained I/O and release systemd references after cleanup/expiry.
    fn forget(&mut self, run: RunHandle) -> Result<()>;
    /// Adapter background work; never called on the control socket thread.
    fn maintenance(&mut self) -> Result<()> { Ok(()) }
    fn allocation(&mut self, _: &duduclaw_migration::allocation::Request) -> Result<duduclaw_migration::allocation::Receipt> {
        Err(ErrorCode::IsolationUnavailable)
    }
    /// H4 synchronous broker step. Called only on an adapter's own lifecycle
    /// thread, never on the control socket thread. Missing broker ⇒ refuse.
    fn broker(&mut self, _: BrokerJob) -> Result<BrokerStep> { Err(ErrorCode::WorkspaceBrokerUnavailable) }
    /// H4 non-blocking facade used by the coordinator: queue one job.
    fn broker_submit(&mut self, _: Ticket, _: BrokerJob) -> Result<()> { Err(ErrorCode::WorkspaceBrokerUnavailable) }
    /// `Ok(None)` while pending; the result (`Ok(Some)` or `Err`) exactly once.
    fn broker_poll(&mut self, _: Ticket) -> Result<Option<BrokerReply>> { Err(ErrorCode::WorkspaceBrokerUnavailable) }
    /// Forget a ticket; a result that arrives later (and its fd) is dropped.
    fn broker_abandon(&mut self, _: Ticket) {}
    /// HX4: whether this host can run helpers now (launcher plans active,
    /// slot cgroups not known to be unavailable). `Err` is the per-call code
    /// (`helper_unavailable` / `helper_cgroup_unavailable`).
    fn helpers_ready(&mut self) -> Result<()> { Err(ErrorCode::HelperUnavailable) }
    /// [F17, H4] The one feature set this spawnd advertises, identical in the
    /// readiness report and in every `Reserved`. This binary always decodes the
    /// Package H ops; `helper_calls` only when [`Self::helpers_ready`].
    fn features(&mut self) -> SpawndFeatures {
        let features = SpawndFeatures::WORK_BROKER;
        if self.helpers_ready().is_ok() { features.with(SpawndFeatures::HELPER_CALLS) } else { features }
    }
    /// HX4 synchronous start, on an adapter's lifecycle thread only: re-derive
    /// the helper from the run's own employee, revalidate the memfd, write the
    /// slot limits, create the pipes and send `Start`.
    fn helper_run(&mut self, _: HelperStart<Self::Inputs>) -> Result<HelperStarted<Self::Io>> { Err(ErrorCode::HelperUnavailable) }
    /// HX4 non-blocking facade used by the coordinator (one ticket per call).
    fn helper_start(&mut self, _: Ticket, _: HelperStart<Self::Inputs>) -> Result<()> { Err(ErrorCode::HelperUnavailable) }
    /// `Ok(None)` while pending; the result (`Ok(Some)` or `Err`) exactly once.
    fn helper_poll(&mut self, _: Ticket) -> Result<Option<HelperStarted<Self::Io>>> { Err(ErrorCode::HelperUnavailable) }
    /// [F8] Per-call stop/kill. Must never revoke or kill the run.
    fn helper_signal(&mut self, _: RunHandle, _: HelperCallId, _: HelperSignal) -> Result<()> { Err(ErrorCode::HelperUnavailable) }
    /// Root-determined ends, runtime exits and channel losses since the last call.
    fn helper_events(&mut self) -> Vec<HelperEvent> { Vec::new() }
}

/// One outstanding H4 job as the coordinator tracks it.
enum Pending {
    Open { run: RunHandle, connection: ControlConnectionId, employee: EmployeeId },
    Anchor { employee: EmployeeId },
    Idle { employee: EmployeeId },
    Stop(StopProgress),
    /// HX4: one outstanding `HelperCall` reply (`poll_helper_ticket`).
    Helper(helpers::HelperTicket),
}
struct StopProgress { employee: EmployeeId, runs: Vec<RunHandle>, deadline: u64, probe: Option<Ticket> }

#[derive(Clone)]
pub enum HostProgress {
    Starting,
    Running { last_activity_ms: u64 },
    Ended(RunOutcome),
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub host_runs: usize,
    pub host_requests: usize,
    pub employee_queue: u16,
    pub pending_input_bytes: u64,
    /// Includes queued and running inputs; reserve descriptor headroom for IPC,
    /// systemd, output relays and root service state below LimitNOFILE=8192.
    pub input_descriptors: usize,
    /// floor((kernel.pty.max - kernel.pty.reserve) * 0.9).
    pub host_ptys: u32,
    pub terminal_retention_ms: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self { host_runs: 8, host_requests: 128, employee_queue: DEFAULT_QUEUE_LENGTH,
            pending_input_bytes: 256 * 1024 * 1024, input_descriptors: 4096,
            host_ptys: 128, terminal_retention_ms: 60_000 }
    }
}

struct Entry<I> {
    employee: EmployeeId,
    connection: ControlConnectionId,
    reservation: RunReservation,
    state: AdmissionState,
    request: Option<SpawnRequest>,
    policy: Option<LaunchPolicy>,
    inputs: Option<I>,
    input_bytes: u64,
    input_descriptors: usize,
    sequence: u64,
    deadline: u64,
    started: u64,
    terminal_at: u64,
    outcome: Option<RunOutcome>,
    io_taken: bool,
    pty_activity_ms: u64,
    initial_pty_size: Option<PtySize>,
}
impl<I> Entry<I> {
    fn active(&self) -> bool {
        matches!(self.state, AdmissionState::Starting | AdmissionState::Running | AdmissionState::Draining)
    }
    fn terminal(&self) -> bool {
        matches!(self.state, AdmissionState::Finished | AdmissionState::Cancelled | AdmissionState::Expired)
    }
    fn transition(&mut self, next: AdmissionState, evidence: Option<CleanupReadback>) -> Result<()> {
        self.state = self.state.transition(next, evidence).map_err(|_| ErrorCode::Internal)?;
        Ok(())
    }
}

/// Single owner; callers serialize access. Time is monotonic-derived Unix ms.
/// No request is allowed to supply a time, priority, cleanup proof or policy.
pub struct Coordinator<H: Host> {
    host: H,
    limits: Limits,
    entries: BTreeMap<RunId, Entry<H::Inputs>>,
    sequence: u64,
    last_now: u64,
    last_scan: Option<u64>,
    scan_ok: bool,
    /// H4 outstanding jobs and the employees with a pending stop (count of stops).
    tickets: BTreeMap<Ticket, Pending>,
    next_ticket: u64,
    stopping: BTreeMap<EmployeeId, usize>,
    /// HX4 per-run helper calls (FIFO, call ids, outcomes); never an admission entry.
    helpers: BTreeMap<RunId, helpers::RunHelpers<H::Inputs>>,
}
impl<H: Host> Coordinator<H> {
    pub fn new(host: H, limits: Limits) -> Result<Self> {
        if limits.host_runs == 0 || limits.host_requests < limits.host_runs || limits.host_requests > 1024
            || limits.employee_queue == 0 || limits.employee_queue > DEFAULT_QUEUE_LENGTH
            || limits.pending_input_bytes == 0 || limits.input_descriptors == 0
            || limits.input_descriptors > 4096 || limits.terminal_retention_ms > 300_000 {
            return Err(ErrorCode::ResourceLimit);
        }
        Ok(Self { host, limits, entries: BTreeMap::new(), sequence: 0,
            last_now: 0, last_scan: None, scan_ok: false,
            tickets: BTreeMap::new(), next_ticket: 0, stopping: BTreeMap::new(), helpers: BTreeMap::new() })
    }
    fn host_ticket(&mut self) -> Result<Ticket> {
        self.next_ticket = self.next_ticket.checked_add(1).ok_or(ErrorCode::ResourceLimit)?;
        Ok(Ticket(self.next_ticket))
    }
    fn ticket(&mut self) -> Result<Ticket> {
        if self.tickets.len() >= MAX_BROKER_TICKETS { return Err(ErrorCode::ResourceLimit); }
        self.host_ticket()
    }
    /// (outstanding snapshots of `run`, outstanding snapshots overall).
    fn snapshot_counts(&self, run: RunHandle) -> (usize, usize) {
        let mut per_run = 0;
        let mut total = 0;
        for pending in self.tickets.values() {
            if let Pending::Open { run: other, .. } = pending {
                total += 1;
                if *other == run { per_run += 1; }
            }
        }
        (per_run, total)
    }
    fn live_entries(&self, employee: EmployeeId) -> usize {
        self.entries.values().filter(|entry| entry.employee == employee && !entry.terminal()).count()
    }
    /// H4 OpenWorkFile: only the owning connection, the run's own employee,
    /// and only while Running; then the host checks the lease and opens.
    pub fn open_work_file(&mut self, connection: ControlConnectionId, run: RunHandle, employee: EmployeeId,
        path: WorkPath, now: u64) -> Result<Ticket> {
        self.time(now)?;
        path.validate().map_err(|_| ErrorCode::InvalidRequest)?;
        let entry = self.owned(run, connection)?;
        if entry.employee != employee { return Err(ErrorCode::Unauthorized); }
        if entry.state != AdmissionState::Running || self.stopping.contains_key(&employee) { return Err(ErrorCode::NotReady); }
        let (per_run, total) = self.snapshot_counts(run);
        if per_run >= MAX_SNAPSHOTS_PER_RUN || total >= MAX_SNAPSHOTS { return Err(ErrorCode::ResourceLimit); }
        let ticket = self.ticket()?;
        self.host.broker_submit(ticket, BrokerJob::OpenWorkFile { run, employee, path })?;
        self.tickets.insert(ticket, Pending::Open { run, connection, employee });
        Ok(ticket)
    }
    /// H4 WorkAnchor: no run needed; the host requires an allocated employee.
    pub fn work_anchor(&mut self, employee: EmployeeId, now: u64) -> Result<Ticket> {
        self.time(now)?;
        let ticket = self.ticket()?;
        self.host.broker_submit(ticket, BrokerJob::WorkAnchor { employee })?;
        self.tickets.insert(ticket, Pending::Anchor { employee });
        Ok(ticket)
    }
    /// H4 EmployeeIdle: read-only liveness of one employee.
    pub fn employee_idle(&mut self, employee: EmployeeId, now: u64) -> Result<Ticket> {
        self.time(now)?;
        let ticket = self.ticket()?;
        self.host.broker_submit(ticket, BrokerJob::EmployeeIdle { employee })?;
        self.tickets.insert(ticket, Pending::Idle { employee });
        Ok(ticket)
    }
    /// H4 StopEmployeeRuns (F6 S13): cancel every non-terminal run of the
    /// employee through the per-run cancel path, whichever connection owns it,
    /// and refuse new reservations for it until this stop has replied.
    pub fn stop_employee_runs(&mut self, employee: EmployeeId, now: u64) -> Result<Ticket> {
        self.time(now)?;
        let runs: Vec<RunHandle> = self.entries.iter().filter(|(_, entry)| entry.employee == employee && !entry.terminal())
            .map(|(id, _)| RunHandle(*id)).collect();
        if runs.len() > MAX_STOPPED_RUNS { return Err(ErrorCode::ResourceLimit); }
        let deadline = now.checked_add(STOP_WAIT_MS).ok_or(ErrorCode::InvalidRequest)?;
        let ticket = self.ticket()?;
        let count = self.stopping.entry(employee).or_insert(0);
        *count = count.saturating_add(1);
        self.cancel_stopping(employee, now);
        self.tickets.insert(ticket, Pending::Stop(StopProgress { employee, runs, deadline, probe: None }));
        Ok(ticket)
    }
    /// Per-run cancel for every entry of `employee` that is neither terminal
    /// nor already draining (a draining run keeps its original outcome).
    fn cancel_stopping(&mut self, employee: EmployeeId, now: u64) {
        let open: Vec<_> = self.entries.iter().filter(|(_, entry)| entry.employee == employee && !entry.terminal()
            && entry.state != AdmissionState::Draining).map(|(id, _)| RunHandle(*id)).collect();
        for run in open { let _ = self.cancel_entry(run, now); }
    }
    fn release_stop(&mut self, employee: EmployeeId) {
        if let Some(count) = self.stopping.get_mut(&employee) {
            *count = count.saturating_sub(1);
            if *count == 0 { self.stopping.remove(&employee); }
        }
    }
    /// `Ok(None)` while pending. A finished ticket is removed; an unknown
    /// ticket is `NotFound`. Never blocks.
    pub fn poll_ticket(&mut self, ticket: Ticket, now: u64) -> Result<Option<BrokerOutcome>> {
        self.time(now)?;
        match self.tickets.get(&ticket).ok_or(ErrorCode::NotFound)? {
            Pending::Stop(_) => return self.poll_stop(ticket, now),
            // Answered with pipes by `poll_helper_ticket`, never by the broker.
            Pending::Helper(_) => return Err(ErrorCode::InvalidRequest),
            _ => {}
        }
        let reply = match self.host.broker_poll(ticket) {
            Ok(None) => return Ok(None),
            Ok(Some(reply)) => Ok(reply),
            Err(code) => Err(code),
        };
        let pending = self.tickets.remove(&ticket).ok_or(ErrorCode::Internal)?;
        let reply = match reply { Ok(reply) => reply, Err(code) => return Ok(Some(BrokerOutcome::error(code))) };
        Ok(Some(match (pending, reply) {
            (Pending::Open { run, connection, employee }, BrokerReply::WorkFile { metadata, file, source }) => {
                // The run can end while root copies: never hand out a
                // snapshot for a run that is no longer this caller's Running run.
                let live = self.owned(run, connection).is_ok_and(|entry|
                    entry.employee == employee && entry.state == AdmissionState::Running);
                if !live { BrokerOutcome::error(ErrorCode::NotReady) }
                else if metadata.validate().is_err() { BrokerOutcome::error(ErrorCode::WorkspaceBrokerUnavailable) }
                else { BrokerOutcome { response: SpawnResponse::WorkFile { run, file: metadata }, file: Some(file), detail: source } }
            }
            (Pending::Anchor { employee }, BrokerReply::WorkAnchor(anchor)) => {
                if anchor.validate().is_err() { BrokerOutcome::error(ErrorCode::WorkspaceBrokerUnavailable) }
                else { BrokerOutcome::reply(SpawnResponse::WorkAnchor { employee_id: employee, anchor }) }
            }
            (Pending::Idle { employee }, BrokerReply::Idle(mut report)) => {
                let admitted = u16::try_from(self.live_entries(employee)).unwrap_or(u16::MAX);
                report.runs = report.runs.saturating_add(admitted);
                BrokerOutcome::reply(SpawnResponse::EmployeeIdle { employee_id: employee, report })
            }
            _ => BrokerOutcome::error(ErrorCode::Internal),
        }))
    }
    fn poll_stop(&mut self, ticket: Ticket, now: u64) -> Result<Option<BrokerOutcome>> {
        let (employee, deadline, probe, listed) = match self.tickets.get(&ticket) {
            Some(Pending::Stop(progress)) => (progress.employee, progress.deadline, progress.probe, progress.runs.len()),
            _ => return Err(ErrorCode::Internal),
        };
        // Idempotent: anything that slipped past the first pass is cancelled too.
        self.cancel_stopping(employee, now);
        let active = self.live_entries(employee) > 0;
        let expired = now >= deadline;
        let mut confirmed = false;
        let mut refusal = None;
        let mut next_probe = probe;
        if !active {
            match probe {
                None if !expired => {
                    let probe = self.host_ticket()?;
                    if self.host.broker_submit(probe, BrokerJob::EmployeeIdle { employee }).is_ok() { next_probe = Some(probe); }
                }
                None => {}
                Some(probe) => match self.host.broker_poll(probe) {
                    Ok(None) => {}
                    // Confirmed only on a positive root readback: no entry, no
                    // run record, no lease, no process, no unit.
                    Ok(Some(BrokerReply::Idle(report))) => { next_probe = None; confirmed = report.idle(); }
                    Ok(Some(_)) => next_probe = None,
                    Err(ErrorCode::NotFound) if listed == 0 => { next_probe = None; refusal = Some(ErrorCode::NotFound); }
                    Err(_) => next_probe = None,
                },
            }
        }
        if !(confirmed || refusal.is_some() || expired) {
            if let Some(Pending::Stop(progress)) = self.tickets.get_mut(&ticket) { progress.probe = next_probe; }
            return Ok(None);
        }
        let Some(Pending::Stop(progress)) = self.tickets.remove(&ticket) else { return Err(ErrorCode::Internal) };
        if let Some(probe) = next_probe { self.host.broker_abandon(probe); }
        self.release_stop(employee);
        if let Some(code) = refusal { return Ok(Some(BrokerOutcome::error(code))); }
        let runs = List::new(progress.runs).map_err(|_| ErrorCode::Internal)?;
        Ok(Some(BrokerOutcome::reply(SpawnResponse::EmployeeRunsStopped { employee_id: employee, runs, confirmed })))
    }
    /// The server gave up waiting (deadline or disconnect).
    pub fn abandon_ticket(&mut self, ticket: Ticket) {
        match self.tickets.remove(&ticket) {
            Some(Pending::Stop(progress)) => {
                if let Some(probe) = progress.probe { self.host.broker_abandon(probe); }
                self.release_stop(progress.employee);
            }
            Some(Pending::Helper(record)) => self.abandon_helper(record),
            Some(_) => self.host.broker_abandon(ticket),
            None => {}
        }
    }
    fn time(&mut self, now: u64) -> Result<()> {
        if now < self.last_now { return Err(ErrorCode::IsolationUnavailable); }
        self.last_now = now;
        Ok(())
    }
    fn owned(&self, run: RunHandle, connection: ControlConnectionId) -> Result<&Entry<H::Inputs>> {
        self.entries.get(&run.0).filter(|entry| entry.connection == connection).ok_or(ErrorCode::NotFound)
    }
    pub fn reserve(&mut self, connection: ControlConnectionId, employee: EmployeeId,
        random_run: RunHandle, now: u64) -> Result<SpawnResponse> {
        self.time(now)?;
        // H4: no new run for an employee whose runs are being stopped (F6 S13).
        if self.stopping.contains_key(&employee) { return Err(ErrorCode::NotReady); }
        self.host.ready(employee)?;
        // The random id is generated by the daemon, never copied from a frame.
        if self.entries.contains_key(&random_run.0) { return Err(ErrorCode::Internal); }
        if self.entries.len() >= self.limits.host_requests || self.entries.values()
            .filter(|entry| entry.employee == employee && !entry.terminal()).count() >= self.limits.employee_queue as usize {
            return Err(ErrorCode::QueueFull);
        }
        let reservation = RunReservation::new(random_run, employee, connection, now).map_err(|_| ErrorCode::Internal)?;
        let expires = reservation.expires_at_unix_ms();
        self.sequence = self.sequence.checked_add(1).ok_or(ErrorCode::ResourceLimit)?;
        self.entries.insert(random_run.0, Entry { employee, connection, reservation, state: AdmissionState::Reserved,
            request: None, policy: None, inputs: None, input_bytes: 0, input_descriptors: 0, sequence: self.sequence,
            deadline: expires, started: 0, terminal_at: 0, outcome: None, io_taken: false, pty_activity_ms: 0, initial_pty_size: None });
        // [F17, H4] the same feature set the readiness report carries.
        Ok(SpawnResponse::Reserved { run: random_run, expires_at_unix_ms: expires, features: self.host.features() })
    }
    pub fn start(&mut self, connection: ControlConnectionId, request: SpawnRequest, inputs: H::Inputs,
        now: u64) -> Result<SpawnResponse> {
        self.time(now)?;
        request.validate().map_err(|_| ErrorCode::InvalidRequest)?;
        let SpawnRequest::StartRun { run, employee_id, ref runtime_request, prompt_bytes, ref input_fds, pty, .. } = request
            else { return Err(ErrorCode::InvalidRequest); };
        let entry = self.owned(run, connection)?;
        if entry.employee != employee_id || entry.state != AdmissionState::Reserved || now >= entry.deadline {
            return Err(ErrorCode::InvalidRequest);
        }
        let initial_size = entry.initial_pty_size;
        if !pty && initial_size.is_some() { return Err(ErrorCode::InvalidRequest); }
        let mut policy = self.host.policy(employee_id, runtime_request, pty)?;
        if let Some(size) = initial_size { policy.pty_size = Some(size); }
        policy.validate()?;
        if policy.delivery.pty != pty { return Err(ErrorCode::RuntimeDenied); }
        let input_bytes = input_fds.as_slice().iter().fold(prompt_bytes, |sum, input| sum + match input {
            duduclaw_isolation::ipc::InputFd::SealedMemfd { bytes }
            | duduclaw_isolation::ipc::InputFd::ReadOnlySnapshot { bytes } => *bytes,
        });
        // HX4: queued/starting helper memfds share these budgets.
        let (helper_bytes, helper_fds) = self.helper_held();
        let pending: u64 = self.entries.values().map(|entry| entry.input_bytes).sum::<u64>().saturating_add(helper_bytes);
        if input_bytes > self.limits.pending_input_bytes.saturating_sub(pending) { return Err(ErrorCode::ResourceLimit); }
        let descriptors = request.fd_plan().map_err(|_| ErrorCode::InvalidRequest)?.len();
        let held: usize = self.entries.values().map(|entry| entry.input_descriptors).sum::<usize>() + helper_fds;
        if descriptors > self.limits.input_descriptors.saturating_sub(held) { return Err(ErrorCode::ResourceLimit); }
        let deadline = now.checked_add(policy.queue_ms).ok_or(ErrorCode::InvalidRequest)?;
        let entry = self.entries.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        entry.reservation.consume(employee_id, connection, now).map_err(|_| ErrorCode::InvalidRequest)?;
        entry.transition(AdmissionState::Queued, None)?;
        entry.deadline = deadline;
        entry.policy = Some(policy);
        entry.inputs = Some(inputs);
        entry.input_bytes = input_bytes;
        entry.input_descriptors = descriptors;
        entry.request = Some(request);
        // Always return an asynchronous handle; tick performs admission separately.
        Ok(SpawnResponse::Accepted { run, status: self.status(run, connection)? })
    }
    pub fn status(&self, run: RunHandle, connection: ControlConnectionId) -> Result<RunStatus> {
        let entry = self.owned(run, connection)?;
        Ok(match entry.state {
            AdmissionState::Reserved => RunStatus::Reserved { expires_at_unix_ms: entry.deadline },
            AdmissionState::Queued => {
                let priority = Self::priority(entry);
                let ahead = self.entries.values().filter(|other| other.employee == entry.employee
                    && (other.active() || (other.state == AdmissionState::Queued && Self::priority(other) < priority))).count();
                RunStatus::Queued(QueueStatus { ahead: ahead.min(u16::MAX as usize) as u16,
                    expires_at_unix_ms: entry.deadline,
                    deferred_until_current_run_ends: self.entries.values().any(|other| other.employee == entry.employee && other.active()) })
            }
            AdmissionState::Starting => RunStatus::Starting,
            AdmissionState::Running | AdmissionState::Draining => RunStatus::Running,
            _ => RunStatus::Ended(entry.outcome.clone().ok_or(ErrorCode::Internal)?),
        })
    }
    fn priority(entry: &Entry<H::Inputs>) -> (u8, u64) {
        (if entry.policy.as_ref().is_some_and(|policy| policy.queue_class == QueueClass::Interactive) { 0 } else { 1 }, entry.sequence)
    }
    /// Stop admission and request cancellation before the server exits. The
    /// caller keeps ticking until all leases are clean or the shutdown deadline.
    pub fn begin_shutdown(&mut self, now: u64) {
        let owners: Vec<_> = self.entries.iter().map(|(id, e)| (RunHandle(*id), e.connection)).collect();
        for (run, connection) in owners { let _ = self.cancel(run, connection, now); }
    }
    pub fn has_active_runs(&self) -> bool { self.entries.values().any(Entry::active) }
    /// Wait is a bounded status poll on wire v1. A implements asynchronous waiting.
    pub fn wait(&self, run: RunHandle, connection: ControlConnectionId, caller: Option<RunHandle>) -> Result<RunStatus> {
        let target = self.owned(run, connection)?;
        if let Some(caller) = caller {
            let source = self.owned(caller, connection)?;
            if !target.terminal() && source.employee == target.employee { return Err(ErrorCode::WouldDeadlock); }
        }
        self.status(run, connection)
    }
    pub fn take_io(&mut self, run: RunHandle, connection: ControlConnectionId) -> Result<(IoDelivery, H::Io)> {
        let entry = self.owned(run, connection)?;
        if entry.io_taken { return Err(ErrorCode::AlreadyTaken); }
        let completed_output = entry.state == AdmissionState::Finished
            && matches!(entry.outcome, Some(RunOutcome::Exited(_) | RunOutcome::Signalled(_)));
        if entry.state != AdmissionState::Running && !completed_output { return Err(ErrorCode::NotReady); }
        let delivery = entry.policy.as_ref().ok_or(ErrorCode::Internal)?.delivery;
        let io = self.host.take_io(run)?;
        self.entries.get_mut(&run.0).ok_or(ErrorCode::Internal)?.io_taken = true;
        Ok((delivery, io))
    }
    pub fn resize(&mut self, run: RunHandle, connection: ControlConnectionId, size: PtySize) -> Result<()> {
        size.validate().map_err(|_| ErrorCode::InvalidRequest)?;
        let entry = self.owned(run, connection)?;
        if entry.state == AdmissionState::Reserved && self.last_now < entry.deadline {
            self.entries.get_mut(&run.0).ok_or(ErrorCode::Internal)?.initial_pty_size = Some(size);
            return Ok(());
        }
        if entry.state != AdmissionState::Running || !entry.policy.as_ref().is_some_and(|p| p.delivery.pty) {
            return Err(ErrorCode::NotReady);
        }
        self.host.resize(run, size)
    }
    /// Activity is reported by A on the original authenticated control socket,
    /// never by the employee or an arbitrary timestamp supplied on the wire.
    pub fn pty_activity(&mut self, run: RunHandle, connection: ControlConnectionId, now: u64) -> Result<()> {
        self.time(now)?;
        let entry = self.owned(run, connection)?;
        if entry.state != AdmissionState::Running || !entry.io_taken
            || !entry.policy.as_ref().is_some_and(|p| p.delivery.pty) { return Err(ErrorCode::NotReady); }
        let idle = entry.policy.as_ref().and_then(|p| p.pty_idle_ms).ok_or(ErrorCode::Internal)?;
        if now.saturating_sub(entry.pty_activity_ms.max(entry.started)) >= idle {
            // A late message must not win a race against an expired idle timer.
            let _ = self.drain(run, RunOutcome::IdleTimeout);
            return Err(ErrorCode::NotReady);
        }
        self.entries.get_mut(&run.0).ok_or(ErrorCode::Internal)?.pty_activity_ms = now;
        Ok(())
    }
    fn drain(&mut self, run: RunHandle, reason: RunOutcome) -> Result<()> {
        // HX4: leaving Running drops the helper queue; live calls end RunEnded.
        self.end_helpers(run, true);
        let entry = self.entries.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        if entry.state != AdmissionState::Draining { entry.transition(AdmissionState::Draining, None)?; }
        entry.outcome = Some(reason);
        // Try both operations even when the first fails. Retain lease on either error.
        let revoke = self.host.revoke(run);
        let kill = self.host.kill(run);
        revoke.and(kill)
    }
    pub fn cancel(&mut self, run: RunHandle, connection: ControlConnectionId, now: u64) -> Result<()> {
        self.time(now)?;
        self.owned(run, connection)?;
        self.cancel_entry(run, now)
    }
    /// The per-run cancel itself; callers have established the authority
    /// (owning connection, or an employee-wide stop from the gateway).
    fn cancel_entry(&mut self, run: RunHandle, now: u64) -> Result<()> {
        let entry = self.entries.get(&run.0).ok_or(ErrorCode::NotFound)?;
        if entry.terminal() { return Ok(()); }
        if entry.active() { return self.drain(run, RunOutcome::Cancelled); }
        let entry = self.entries.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        entry.transition(AdmissionState::Cancelled, None)?;
        entry.inputs = None;
        entry.input_bytes = 0;
        entry.input_descriptors = 0;
        entry.terminal_at = now;
        entry.outcome = Some(RunOutcome::Cancelled);
        Ok(())
    }
    pub fn disconnect(&mut self, connection: ControlConnectionId, now: u64) {
        let runs: Vec<_> = self.entries.iter().filter(|(_, e)| e.connection == connection).map(|(id, _)| RunHandle(*id)).collect();
        for run in runs { let _ = self.cancel(run, connection, now); }
        let released: Vec<_> = self.entries.iter().filter(|(_, e)| e.connection == connection && e.terminal())
            .map(|(id, _)| *id).collect();
        for id in released {
            if self.host.forget(RunHandle(id)).is_ok() { self.entries.remove(&id); }
        }
    }
    /// Called at least every two seconds. No failures free a lease optimistically.
    pub fn tick(&mut self, now: u64) -> Result<()> {
        self.time(now)?;
        if self.last_scan.is_none_or(|last| now.saturating_sub(last) >= 2_000) {
            self.last_scan = Some(now);
            match self.host.detect_intruders() {
                Ok(employees) => {
                    self.scan_ok = true;
                    let affected: Vec<_> = self.entries.iter().filter(|(_, e)| e.active() && employees.contains(&e.employee))
                        .map(|(id, _)| RunHandle(*id)).collect();
                    for run in affected { let _ = self.drain(run, RunOutcome::IsolationFailure); }
                }
                Err(_) => {
                    self.scan_ok = false;
                    let active: Vec<_> = self.entries.iter().filter(|(_, e)| e.active()).map(|(id, _)| RunHandle(*id)).collect();
                    for run in active { let _ = self.drain(run, RunOutcome::IsolationFailure); }
                }
            }
        }
        let ids: Vec<_> = self.entries.keys().copied().collect();
        for id in ids {
            let run = RunHandle(id);
            let entry = self.entries.get_mut(&id).ok_or(ErrorCode::Internal)?;
            match entry.state {
                AdmissionState::Reserved | AdmissionState::Queued if now >= entry.deadline => {
                    entry.transition(AdmissionState::Expired, None)?;
                    entry.inputs = None; entry.input_bytes = 0; entry.input_descriptors = 0; entry.terminal_at = now;
                    entry.outcome = Some(RunOutcome::QueueTimeout);
                }
                AdmissionState::Starting | AdmissionState::Running => {
                    let policy = entry.policy.as_ref().ok_or(ErrorCode::Internal)?;
                    let budget = if entry.state == AdmissionState::Starting { policy.startup_ms } else { policy.runtime_ms };
                    let expired = now.saturating_sub(entry.started) >= budget;
                    let progress = self.host.poll(run);
                    let reason = if expired { Some(RunOutcome::RuntimeTimeout) } else { match progress {
                        Ok(HostProgress::Starting) if entry.state == AdmissionState::Starting => None,
                        Ok(HostProgress::Running { last_activity_ms }) => {
                            if last_activity_ms > now { Some(RunOutcome::IsolationFailure) }
                            else if policy.pty_idle_ms.is_some_and(|idle| now.saturating_sub(if entry.state == AdmissionState::Starting { now } else { last_activity_ms.max(entry.pty_activity_ms) }) >= idle) { Some(RunOutcome::IdleTimeout) }
                            else { if entry.state == AdmissionState::Starting { entry.transition(AdmissionState::Running, None)?; entry.pty_activity_ms = now; } None }
                        }
                        Ok(HostProgress::Ended(outcome)) => Some(outcome),
                        Err(ErrorCode::NotReady) => None,
                        _ => Some(RunOutcome::IsolationFailure),
                    }};
                    if let Some(reason) = reason { let _ = self.drain(run, reason); }
                }
                _ => {}
            }
            let entry = self.entries.get_mut(&id).ok_or(ErrorCode::Internal)?;
            if entry.state == AdmissionState::Draining {
                let revoked = self.host.revoke(run);
                let killed = self.host.kill(run);
                if revoked.is_ok() && killed.is_ok() {
                    if let Ok(proof) = self.host.cleanup(entry.employee, Some(run)) {
                        if proof.complete() {
                            entry.transition(AdmissionState::Finished, Some(proof))?;
                            entry.terminal_at = now; entry.input_bytes = 0; entry.input_descriptors = 0;
                        }
                    }
                }
            }
        }
        let expired: Vec<_> = self.entries.iter().filter(|(_, entry)| entry.terminal()
            && now.saturating_sub(entry.terminal_at) >= self.limits.terminal_retention_ms).map(|(id, _)| *id).collect();
        for id in expired {
            if self.host.forget(RunHandle(id)).is_ok() { self.entries.remove(&id); }
        }
        // HX4: host events, drops for runs that left Running, then the queues.
        self.tick_helpers(now);
        if !self.scan_ok { return Err(ErrorCode::IsolationUnavailable); }
        let mut candidates: Vec<_> = self.entries.iter().filter(|(_, e)| e.state == AdmissionState::Queued)
            .map(|(id, e)| (Self::priority(e), *id)).collect();
        candidates.sort();
        for (_, id) in candidates {
            if self.entries.values().filter(|e| e.active()).count() >= self.limits.host_runs { break; }
            let entry = self.entries.get(&id).ok_or(ErrorCode::Internal)?;
            if self.entries.values().any(|e| e.employee == entry.employee && e.active()) { continue; }
            let ptys: u32 = self.entries.values().filter(|e| e.active()).filter_map(|e| e.policy.as_ref()).map(|p| p.ptys as u32).sum();
            let policy = entry.policy.as_ref().ok_or(ErrorCode::Internal)?;
            if policy.ptys as u32 > self.limits.host_ptys.saturating_sub(ptys) { continue; }
            // Fresh /proc/lockdown/storage checks for EVERY admission, not just the periodic scan.
            if self.host.ready(entry.employee).is_err() { continue; }
            let Ok(proof) = self.host.cleanup(entry.employee, None) else { continue; };
            if !proof.complete() { continue; }
            let entry = self.entries.get_mut(&id).ok_or(ErrorCode::Internal)?;
            entry.transition(AdmissionState::Starting, Some(proof))?;
            entry.started = now;
            let inputs = entry.inputs.take().ok_or(ErrorCode::Internal)?;
            if self.host.start(RunHandle(id), entry.employee, entry.request.as_ref().ok_or(ErrorCode::Internal)?,
                entry.policy.as_ref().ok_or(ErrorCode::Internal)?, inputs).is_err() {
                let _ = self.drain(RunHandle(id), RunOutcome::IsolationFailure);
            }
        }
        Ok(())
    }
}

impl<H: Host> Drop for Coordinator<H> {
    fn drop(&mut self) {
        for (id, entry) in &self.entries {
            if entry.active() {
                let _ = self.host.revoke(RunHandle(*id));
                let _ = self.host.kill(RunHandle(*id));
            }
        }
        // SIGKILL/power loss cannot run Drop. New instances must reconcile all
        // previous units and uid processes before the readiness gate can pass.
    }
}
