use super::*;
use duduclaw_isolation::{bounded::List, ipc::InputFd};

struct FakeHost {
    proof: CleanupReadback,
    starts: Vec<RunHandle>,
    killed: Vec<RunHandle>,
    revoked: Vec<RunHandle>,
    end: bool,
    fail_start: bool,
    scan_fail: bool,
    fail_revoke: bool,
    pending_poll: bool,
}
impl Default for FakeHost {
    fn default() -> Self {
        Self { proof: CleanupReadback { unit_inactive: true, cgroup_empty: true, employee_uid_absent: true },
            starts: vec![], killed: vec![], revoked: vec![], end: false, fail_start: false, scan_fail: false, fail_revoke: false, pending_poll: false }
    }
}
impl Host for FakeHost {
    type Inputs = ();
    type Io = ();
    fn ready(&mut self, _: EmployeeId) -> Result<()> { Ok(()) }
    fn policy(&mut self, _: EmployeeId, runtime: &RuntimeKind, pty: bool) -> Result<LaunchPolicy> {
        Ok(LaunchPolicy { queue_class: if *runtime == RuntimeKind::Codex { QueueClass::Interactive } else { QueueClass::Background },
            queue_ms: 10_000, startup_ms: 1000, runtime_ms: 5000, pty_idle_ms: pty.then_some(2000),
            ptys: if pty { 16 } else { 0 }, pty_size: pty.then_some(PtySize { rows: 24, columns: 80 }), delivery: IoDelivery { mcp: true, pty } })
    }
    fn cleanup(&mut self, _: EmployeeId, _: Option<RunHandle>) -> Result<CleanupReadback> { Ok(self.proof) }
    fn start(&mut self, run: RunHandle, _: EmployeeId, _: &SpawnRequest, _: &LaunchPolicy, _: ()) -> Result<()> {
        self.starts.push(run);
        if self.fail_start { Err(ErrorCode::IsolationUnavailable) } else { Ok(()) }
    }
    fn poll(&mut self, _: RunHandle) -> Result<HostProgress> {
        if self.pending_poll { return Err(ErrorCode::NotReady); }
        Ok(if self.end { HostProgress::Ended(RunOutcome::Exited(0)) } else { HostProgress::Running { last_activity_ms: 100 } })
    }
    fn revoke(&mut self, run: RunHandle) -> Result<()> {
        self.revoked.push(run);
        if self.fail_revoke { Err(ErrorCode::Internal) } else { Ok(()) }
    }
    fn kill(&mut self, run: RunHandle) -> Result<()> { self.killed.push(run); Ok(()) }
    fn take_io(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
    fn resize(&mut self, _: RunHandle, _: PtySize) -> Result<()> { Ok(()) }
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> {
        if self.scan_fail { Err(ErrorCode::IsolationUnavailable) } else { Ok(vec![]) }
    }
    fn forget(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
}
fn conn() -> ControlConnectionId { ControlConnectionId([1; 16]) }
fn run(n: u8) -> RunHandle { RunHandle(RunId([n; 16])) }
fn employee(n: u8) -> EmployeeId { EmployeeId([n; 16]) }
fn request(n: u8, emp: u8, runtime: RuntimeKind) -> SpawnRequest {
    SpawnRequest::StartRun { run: run(n), employee_id: employee(emp), runtime_request: runtime,
        task_ref: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false }
}
fn queue(coordinator: &mut Coordinator<FakeHost>, n: u8, emp: u8, runtime: RuntimeKind) {
    coordinator.reserve(conn(), employee(emp), run(n), 100).unwrap();
    coordinator.start(conn(), request(n, emp, runtime), (), 100).unwrap();
}

#[test]
fn d8_holds_lease_until_all_three_readbacks_and_revocation() {
    for bits in 0..8 {
        let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
        queue(&mut c, 1, 1, RuntimeKind::Codex);
        queue(&mut c, 2, 1, RuntimeKind::Codex);
        c.tick(100).unwrap();
        assert_eq!(c.host.starts, vec![run(1)]);
        c.host.end = true;
        c.host.proof = CleanupReadback { unit_inactive: bits & 1 != 0, cgroup_empty: bits & 2 != 0, employee_uid_absent: bits & 4 != 0 };
        c.tick(101).unwrap();
        assert_eq!(c.host.starts.len(), if bits == 7 { 2 } else { 1 });
        assert!(c.host.revoked.contains(&run(1)));
        assert!(c.host.killed.contains(&run(1)));
    }
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Codex);
    queue(&mut c, 2, 1, RuntimeKind::Codex);
    c.tick(100).unwrap();
    c.host.end = true;
    c.host.fail_revoke = true;
    c.tick(101).unwrap();
    assert_eq!(c.host.starts.len(), 1);
}

#[test]
fn reservations_connection_employee_expiry_and_replay() {
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    c.reserve(conn(), employee(1), run(1), 100).unwrap();
    assert!(c.start(ControlConnectionId([9; 16]), request(1, 1, RuntimeKind::Codex), (), 101).is_err());
    assert!(c.start(conn(), request(1, 2, RuntimeKind::Codex), (), 102).is_err());
    assert!(c.start(conn(), request(1, 1, RuntimeKind::Codex), (), 103).is_ok());
    assert!(c.start(conn(), request(1, 1, RuntimeKind::Codex), (), 104).is_err());
    assert_eq!(c.status(run(1), ControlConnectionId([9; 16])), Err(ErrorCode::NotFound));
    c.reserve(conn(), employee(2), run(2), 105).unwrap();
    assert!(c.start(conn(), request(2, 2, RuntimeKind::Codex), (), 30_105).is_err());
}

#[test]
fn interactive_priority_and_host_limit() {
    let mut c = Coordinator::new(FakeHost::default(), Limits { host_runs: 1, ..Limits::default() }).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Claude);
    queue(&mut c, 2, 1, RuntimeKind::Codex);
    queue(&mut c, 3, 2, RuntimeKind::Codex);
    c.tick(100).unwrap();
    assert_eq!(c.host.starts, vec![run(2)]);
    assert!(matches!(c.status(run(1), conn()).unwrap(), RunStatus::Queued(QueueStatus { ahead: 1, .. })));
}

#[test]
fn take_io_once_after_verified_running_and_self_wait_refused() {
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Codex);
    queue(&mut c, 2, 1, RuntimeKind::Codex);
    assert_eq!(c.take_io(run(1), conn()), Err(ErrorCode::NotReady));
    c.tick(100).unwrap();
    assert_eq!(c.take_io(run(1), conn()), Err(ErrorCode::NotReady));
    c.tick(101).unwrap();
    assert!(c.take_io(run(1), conn()).is_ok());
    assert_eq!(c.take_io(run(1), conn()), Err(ErrorCode::AlreadyTaken));
    assert_eq!(c.wait(run(2), conn(), Some(run(1))), Err(ErrorCode::WouldDeadlock));
    assert!(c.wait(run(2), conn(), None).is_ok());
}

#[test]
fn disconnect_and_partial_start_cannot_release_a_dirty_lease() {
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Codex);
    queue(&mut c, 2, 1, RuntimeKind::Codex);
    c.host.fail_start = true;
    c.tick(100).unwrap();
    c.host.proof.cgroup_empty = false;
    c.disconnect(conn(), 101);
    assert_eq!(c.entries.len(), 1);
    assert_eq!(c.entries[&run(1).0].state, AdmissionState::Draining);
    assert_eq!(c.host.starts.len(), 1);
    c.tick(102).unwrap();
    assert_eq!(c.entries[&run(1).0].state, AdmissionState::Draining);
}

#[test]
fn budgets_expiry_and_failed_process_scan_fail_closed() {
    let mut c = Coordinator::new(FakeHost::default(), Limits { employee_queue: 1, ..Limits::default() }).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Codex);
    assert_eq!(c.reserve(conn(), employee(1), run(2), 100), Err(ErrorCode::QueueFull));
    c.host.scan_fail = true;
    assert_eq!(c.tick(100), Err(ErrorCode::IsolationUnavailable));
    assert!(c.host.starts.is_empty());
    c.tick(10_100).ok();
    assert_eq!(c.status(run(1), conn()), Ok(RunStatus::Ended(RunOutcome::QueueTimeout)));
    assert!(c.tick(10_099).is_err());
    let mut c = Coordinator::new(FakeHost::default(), Limits { pending_input_bytes: 2, ..Limits::default() }).unwrap();
    c.reserve(conn(), employee(1), run(1), 100).unwrap();
    let mut req = request(1, 1, RuntimeKind::Codex);
    if let SpawnRequest::StartRun { input_fds, .. } = &mut req {
        *input_fds = List::new(vec![InputFd::SealedMemfd { bytes: 2 }]).unwrap();
    }
    assert_eq!(c.start(conn(), req, (), 100), Err(ErrorCode::ResourceLimit));
}

#[test]
fn cancellation_keeps_queued_run_asynchronous_and_releases_inputs() {
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Codex);
    c.cancel(run(1), conn(), 101).unwrap();
    assert_eq!(c.status(run(1), conn()).unwrap(), RunStatus::Ended(RunOutcome::Cancelled));
    assert_eq!(c.entries[&run(1).0].input_bytes, 0);
    assert!(c.host.killed.is_empty());
}

#[test]
fn zero_byte_inputs_still_consume_descriptor_budget() {
    let mut c = Coordinator::new(FakeHost::default(), Limits { input_descriptors: 65, ..Limits::default() }).unwrap();
    c.reserve(conn(), employee(1), run(1), 100).unwrap();
    let mut req = request(1, 1, RuntimeKind::Codex);
    if let SpawnRequest::StartRun { input_fds, prompt_bytes, .. } = &mut req {
        *prompt_bytes = 0;
        *input_fds = List::new(vec![InputFd::SealedMemfd { bytes: 0 }; 64]).unwrap();
    }
    c.start(conn(), req, (), 100).unwrap();
    c.reserve(conn(), employee(2), run(2), 100).unwrap();
    assert_eq!(c.start(conn(), request(2, 2, RuntimeKind::Codex), (), 100), Err(ErrorCode::ResourceLimit));
    c.cancel(run(1), conn(), 101).unwrap();
    assert!(c.start(conn(), request(2, 2, RuntimeKind::Codex), (), 102).is_ok());
}

#[test]
fn short_run_can_deliver_output_once_after_exit_with_mcp_revoked() {
    for running_observed in [false, true] {
        let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
        queue(&mut c, 1, 1, RuntimeKind::Codex);
        c.tick(100).unwrap();
        if running_observed { c.tick(101).unwrap(); }
        c.host.end = true;
        c.tick(102).unwrap();
        assert_eq!(c.status(run(1), conn()), Ok(RunStatus::Ended(RunOutcome::Exited(0))));
        assert!(c.host.revoked.contains(&run(1)));
        assert!(c.take_io(run(1), conn()).is_ok());
        assert_eq!(c.take_io(run(1), conn()), Err(ErrorCode::AlreadyTaken));
    }
}

#[test]
fn asynchronous_poll_keeps_lease_and_shutdown_cancels_the_queue() {
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Codex);
    queue(&mut c, 2, 1, RuntimeKind::Codex);
    c.tick(100).unwrap();
    c.host.pending_poll = true;
    c.tick(101).unwrap();
    assert!(matches!(c.status(run(1), conn()).unwrap(), RunStatus::Starting));
    assert!(c.host.killed.is_empty());
    c.host.pending_poll = false;
    c.tick(102).unwrap();
    c.host.pending_poll = true;
    c.tick(103).unwrap();
    assert!(matches!(c.status(run(1), conn()).unwrap(), RunStatus::Running));
    c.begin_shutdown(104);
    c.tick(105).unwrap();
    assert!(!c.has_active_runs());
    assert_eq!(c.host.starts, vec![run(1)]);
    assert!(matches!(c.status(run(2), conn()).unwrap(), RunStatus::Ended(RunOutcome::Cancelled)));
}

fn running_pty() -> Coordinator<FakeHost> {
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    c.reserve(conn(), employee(1), run(1), 100).unwrap();
    let mut q = request(1, 1, RuntimeKind::Grok);
    if let SpawnRequest::StartRun { pty, .. } = &mut q { *pty = true; }
    c.start(conn(), q, (), 100).unwrap();
    c.tick(100).unwrap(); c.tick(101).unwrap();
    c
}

#[test]
fn pty_activity_requires_original_connection_running_pty_and_taken_io() {
    let mut c = running_pty();
    assert_eq!(c.pty_activity(run(1), conn(), 102), Err(ErrorCode::NotReady));
    c.take_io(run(1), conn()).unwrap();
    assert_eq!(c.pty_activity(run(1), ControlConnectionId([9; 16]), 103), Err(ErrorCode::NotFound));
    c.pty_activity(run(1), conn(), 104).unwrap();
    assert_eq!(c.entries[&run(1).0].pty_activity_ms, 104);
    c.cancel(run(1), conn(), 105).unwrap();
    assert_eq!(c.pty_activity(run(1), conn(), 106), Err(ErrorCode::NotReady));
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    queue(&mut c, 1, 1, RuntimeKind::Codex); c.tick(100).unwrap(); c.tick(101).unwrap();
    c.take_io(run(1), conn()).unwrap();
    assert_eq!(c.pty_activity(run(1), conn(), 102), Err(ErrorCode::NotReady));
}

#[test]
fn pty_bytes_extend_idle_but_status_and_resize_do_not() {
    let mut c = running_pty(); c.take_io(run(1), conn()).unwrap();
    c.pty_activity(run(1), conn(), 1500).unwrap();
    c.tick(2101).unwrap();
    assert_eq!(c.status(run(1), conn()), Ok(RunStatus::Running));
    c.resize(run(1), conn(), PtySize { rows: 37, columns: 93 }).unwrap();
    c.tick(3500).unwrap();
    assert_eq!(c.status(run(1), conn()), Ok(RunStatus::Ended(RunOutcome::IdleTimeout)));
    assert!(c.host.revoked.contains(&run(1)) && c.host.killed.contains(&run(1)));
}

#[test]
fn pty_late_activity_cannot_revive_expired_deadline() {
    let mut c = running_pty(); c.take_io(run(1), conn()).unwrap();
    assert_eq!(c.pty_activity(run(1), conn(), 2101), Err(ErrorCode::NotReady));
    assert_eq!(c.entries[&run(1).0].state, AdmissionState::Draining);
    c.tick(2102).unwrap();
    assert_eq!(c.status(run(1), conn()), Ok(RunStatus::Ended(RunOutcome::IdleTimeout)));
}

#[test]
fn pty_cancel_keeps_lease_until_all_retirement_readbacks_succeed() {
    let mut c = running_pty(); c.take_io(run(1), conn()).unwrap();
    // Queue the same employee while the PTY is still live.
    c.reserve(conn(), employee(1), run(2), 102).unwrap();
    c.start(conn(), request(2, 1, RuntimeKind::Grok), (), 102).unwrap();
    c.host.proof.cgroup_empty = false;
    c.cancel(run(1), conn(), 103).unwrap(); c.tick(104).unwrap();
    assert_eq!(c.entries[&run(1).0].state, AdmissionState::Draining);
    assert!(matches!(c.status(run(2), conn()), Ok(RunStatus::Queued(_))));
    assert_eq!(c.pty_activity(run(1), conn(), 105), Err(ErrorCode::NotReady));
    c.host.proof.cgroup_empty = true; c.tick(106).unwrap();
    assert_eq!(c.status(run(1), conn()), Ok(RunStatus::Ended(RunOutcome::Cancelled)));
    assert_eq!(c.host.starts, vec![run(1), run(2)]);
}

#[test]
fn initial_pty_size_is_bound_before_start_and_cannot_reconfigure_a_queued_run() {
    let mut c = Coordinator::new(FakeHost::default(), Limits::default()).unwrap();
    c.reserve(conn(), employee(1), run(1), 100).unwrap();
    let size = PtySize { rows: 31, columns: 113 };
    assert_eq!(c.resize(run(1), ControlConnectionId([9; 16]), size), Err(ErrorCode::NotFound));
    assert!(c.resize(run(1), conn(), PtySize { rows: 0, columns: 113 }).is_err());
    c.resize(run(1), conn(), size).unwrap();
    assert!(c.start(conn(), request(1, 1, RuntimeKind::Grok), (), 101).is_err(), "configured PTY reservation cannot become a pipe run");
    let mut q = request(1, 1, RuntimeKind::Grok);
    if let SpawnRequest::StartRun { pty, .. } = &mut q { *pty = true; }
    c.start(conn(), q, (), 101).unwrap();
    assert_eq!(c.entries[&run(1).0].policy.as_ref().unwrap().pty_size, Some(size));
    assert_eq!(c.resize(run(1), conn(), size), Err(ErrorCode::NotReady));
    c.reserve(conn(), employee(2), run(2), 102).unwrap();
    c.tick(30_102).unwrap();
    assert_eq!(c.resize(run(2), conn(), size), Err(ErrorCode::NotReady));
}
