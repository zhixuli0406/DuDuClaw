//! D8 admission. Only this coordinator may release an employee lease.
use std::collections::BTreeMap;
use duduclaw_isolation::{execution::{RunHandle, RunOutcome, RuntimeKind, PtySize}, ids::{EmployeeId, RunId},
    ipc::{AdmissionState, CleanupReadback, ControlConnectionId, ErrorCode, IoDelivery, QueueClass,
        QueueStatus, RunReservation, RunStatus, SpawnRequest, SpawnResponse, DEFAULT_QUEUE_LENGTH}};
use crate::Result;

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
    pub delivery: IoDelivery,
}
impl LaunchPolicy {
    pub fn validate(&self) -> Result<()> {
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
}

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
            last_now: 0, last_scan: None, scan_ok: false })
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
            deadline: expires, started: 0, terminal_at: 0, outcome: None, io_taken: false });
        Ok(SpawnResponse::Reserved { run: random_run, expires_at_unix_ms: expires })
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
        let policy = self.host.policy(employee_id, runtime_request, pty)?;
        policy.validate()?;
        if policy.delivery.pty != pty { return Err(ErrorCode::RuntimeDenied); }
        let input_bytes = input_fds.as_slice().iter().fold(prompt_bytes, |sum, input| sum + match input {
            duduclaw_isolation::ipc::InputFd::SealedMemfd { bytes }
            | duduclaw_isolation::ipc::InputFd::ReadOnlySnapshot { bytes } => *bytes,
        });
        let pending: u64 = self.entries.values().map(|entry| entry.input_bytes).sum();
        if input_bytes > self.limits.pending_input_bytes.saturating_sub(pending) { return Err(ErrorCode::ResourceLimit); }
        let descriptors = 1 + input_fds.as_slice().len();
        let held: usize = self.entries.values().map(|entry| entry.input_descriptors).sum();
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
        if entry.state != AdmissionState::Running || !entry.policy.as_ref().is_some_and(|p| p.delivery.pty) {
            return Err(ErrorCode::NotReady);
        }
        self.host.resize(run, size)
    }
    fn drain(&mut self, run: RunHandle, reason: RunOutcome) -> Result<()> {
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
        let entry = self.owned(run, connection)?;
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
                            else if policy.pty_idle_ms.is_some_and(|idle| now.saturating_sub(last_activity_ms) >= idle) { Some(RunOutcome::IdleTimeout) }
                            else { if entry.state == AdmissionState::Starting { entry.transition(AdmissionState::Running, None)?; } None }
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
