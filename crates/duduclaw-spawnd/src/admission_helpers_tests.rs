//! HX4 coordinator tests (D8 revision §7 HX4). The host double records every
//! start and signal; `Rc` inputs make "the memfd was closed" observable.
use super::*;
use std::rc::Rc;
use duduclaw_isolation::{bounded::{List, Text}, execution::{PtySize, RunOutcome}, ids::RunId,
    ipc::{CleanupReadback, IoDelivery, QueueClass, RunStatus}};
use crate::admission::{HostProgress, LaunchPolicy, Limits};

type Input = Rc<()>;
#[derive(Default)]
struct Fake {
    killed: Vec<RunHandle>,
    revoked: Vec<RunHandle>,
    end: bool,
    runtime_ms: Option<u64>,
    starts: BTreeMap<Ticket, HelperStart<Input>>,
    started: Vec<HelperCallId>,
    hold: bool,
    fail_start: Option<ErrorCode>,
    refuse_poll: Option<ErrorCode>,
    signals: Vec<(RunHandle, HelperCallId, HelperSignal)>,
    events: Vec<HelperEvent>,
    not_ready: Option<ErrorCode>,
    /// The worker mailbox is full: every `helper_signal` is refused (finding 6).
    refuse_signals: bool,
}
const CLEAN: CleanupReadback = CleanupReadback { unit_inactive: true, cgroup_empty: true, employee_uid_absent: true };
impl Host for Fake {
    type Inputs = Input;
    type Io = ();
    fn ready(&mut self, _: EmployeeId) -> Result<()> { Ok(()) }
    fn policy(&mut self, _: EmployeeId, _: &RuntimeKind, _: bool) -> Result<LaunchPolicy> {
        Ok(LaunchPolicy { queue_class: QueueClass::Interactive, queue_ms: 10_000, startup_ms: 1_000,
            runtime_ms: self.runtime_ms.unwrap_or(600_000), pty_idle_ms: None, ptys: 0, pty_size: None,
            delivery: IoDelivery { mcp: true, pty: false } })
    }
    fn cleanup(&mut self, _: EmployeeId, _: Option<RunHandle>) -> Result<CleanupReadback> { Ok(CLEAN) }
    fn start(&mut self, _: RunHandle, _: EmployeeId, _: &SpawnRequest, _: &LaunchPolicy, _: Input) -> Result<()> { Ok(()) }
    fn poll(&mut self, _: RunHandle) -> Result<HostProgress> {
        Ok(if self.end { HostProgress::Ended(RunOutcome::Exited(0)) } else { HostProgress::Running { last_activity_ms: 100 } })
    }
    fn revoke(&mut self, run: RunHandle) -> Result<()> { self.revoked.push(run); Ok(()) }
    fn kill(&mut self, run: RunHandle) -> Result<()> { self.killed.push(run); Ok(()) }
    fn take_io(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
    fn resize(&mut self, _: RunHandle, _: PtySize) -> Result<()> { Ok(()) }
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> { Ok(vec![]) }
    fn forget(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
    fn helpers_ready(&mut self) -> Result<()> { self.not_ready.map_or(Ok(()), Err) }
    fn helper_start(&mut self, ticket: Ticket, job: HelperStart<Input>) -> Result<()> {
        if let Some(code) = self.fail_start { return Err(code); }
        self.started.push(job.call);
        if self.starts.insert(ticket, job).is_some() { return Err(ErrorCode::Internal); }
        Ok(())
    }
    fn helper_poll(&mut self, ticket: Ticket) -> Result<Option<HelperStarted<()>>> {
        if self.hold { return Ok(None); }
        let job = self.starts.remove(&ticket).ok_or(ErrorCode::Internal)?;
        if let Some(code) = self.refuse_poll { return Err(code); }
        // The memfd is handed to the launcher and closed here.
        drop(job.input);
        Ok(Some(HelperStarted { effective_timeout_ms: job.timeout_ms.min(30_000), pipes: () }))
    }
    fn helper_signal(&mut self, run: RunHandle, call: HelperCallId, signal: HelperSignal) -> Result<()> {
        if self.refuse_signals { return Err(ErrorCode::HelperUnavailable); }
        self.signals.push((run, call, signal)); Ok(())
    }
    fn helper_events(&mut self) -> Vec<HelperEvent> { std::mem::take(&mut self.events) }
}

fn conn() -> ControlConnectionId { ControlConnectionId([1; 16]) }
fn run(n: u8) -> RunHandle { RunHandle(RunId([n; 16])) }
fn employee(n: u8) -> EmployeeId { EmployeeId([n; 16]) }
fn id(n: u8) -> HelperCallId { HelperCallId::new([n; 16]).unwrap() }
fn program() -> RuntimeKind { RuntimeKind::Registered(Text::new("duduclaw-program-v1".into()).unwrap()) }
fn helper(n: u8, emp: u8, slot: HelperSlot, input_bytes: u64) -> SpawnRequest {
    SpawnRequest::HelperCall { run: run(n), employee_id: employee(emp), helper: program(), slot, timeout_ms: 60_000, input_bytes }
}
fn queue(c: &mut Coordinator<Fake>, n: u8, emp: u8, now: u64) {
    c.reserve(conn(), employee(emp), run(n), now).unwrap();
    c.start(conn(), SpawnRequest::StartRun { run: run(n), employee_id: employee(emp), runtime_request: RuntimeKind::Codex,
        task_ref: None, workspace: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false }, Rc::new(()), now).unwrap();
}
fn running(limits: Limits) -> Coordinator<Fake> {
    let mut c = Coordinator::new(Fake::default(), limits).unwrap();
    queue(&mut c, 1, 1, 100);
    c.tick(100).unwrap();
    c.tick(101).unwrap();
    assert_eq!(c.status(run(1), conn()).unwrap(), RunStatus::Running);
    c
}
fn submit(c: &mut Coordinator<Fake>, slot: HelperSlot, n: u8, now: u64) -> Result<Ticket> {
    c.helper_call(conn(), helper(1, 1, slot, 10), Rc::new(()), [n; 16], now)
}
fn reply(c: &mut Coordinator<Fake>, ticket: Ticket, now: u64) -> Option<SpawnResponse> {
    c.poll_helper_ticket(ticket, now).unwrap().map(|reply| reply.response)
}
fn started(c: &mut Coordinator<Fake>, slot: HelperSlot, n: u8, now: u64) -> HelperCallId {
    let ticket = submit(c, slot, n, now).unwrap();
    match reply(c, ticket, now) {
        Some(SpawnResponse::HelperStarted { run: started_run, call, effective_timeout_ms }) => {
            assert_eq!(started_run, run(1));
            assert!(effective_timeout_ms <= 60_000);
            call
        }
        other => panic!("not started: {other:?}"),
    }
}
fn error(code: ErrorCode) -> Option<SpawnResponse> { Some(SpawnResponse::Error { code }) }
fn end(c: &mut Coordinator<Fake>, call: HelperCallId, outcome: HelperOutcome, now: u64) {
    c.host.events.push(HelperEvent::Ended { run: run(1), call, outcome });
    c.tick(now).unwrap();
}

#[test]
fn helper_call_never_occupies_d8_slot() {
    let mut c = running(Limits { host_runs: 2, ..Limits::default() });
    // A second run of the same employee waits behind the first (D8)...
    queue(&mut c, 2, 1, 102);
    let call = started(&mut c, HelperSlot::Foreground, 7, 102);
    assert_eq!(call, id(7), "the call id is spawnd's, returned in HelperStarted");
    // ...and keeps waiting while a helper runs inside run 1: no entry, no reservation.
    assert_eq!(c.entries.len(), 2);
    c.tick(103).unwrap();
    match c.status(run(2), conn()).unwrap() {
        RunStatus::Queued(status) => assert!(status.deferred_until_current_run_ends),
        other => panic!("{other:?}"),
    }
    // The helper does not count against the host run budget: another employee still starts.
    queue(&mut c, 3, 2, 103);
    c.tick(104).unwrap();
    c.tick(105).unwrap();
    assert_eq!(c.status(run(3), conn()).unwrap(), RunStatus::Running);
    // The job is keyed (run, call), separate from run handles.
    assert_eq!(c.helper_wait(run(1), employee(1),call, conn()), Err(ErrorCode::NotReady));
    end(&mut c, call, HelperOutcome::Exited(0), 106);
    assert_eq!(c.helper_wait(run(1), employee(1),call, conn()).unwrap(),
        SpawnResponse::HelperEnded { run: run(1), call, outcome: HelperOutcome::Exited(0) });
    assert_eq!(c.helper_wait(run(1), employee(1),call, conn()), Err(ErrorCode::NotFound), "the outcome is answered once");
    assert_eq!(c.status(run(1), conn()).unwrap(), RunStatus::Running);
}

#[test]
fn helper_call_other_connection_refused() {
    let mut c = running(Limits::default());
    let other = ControlConnectionId([9; 16]);
    assert_eq!(c.helper_call(other, helper(1, 1, HelperSlot::Foreground, 10), Rc::new(()), [7; 16], 102).unwrap_err(),
        ErrorCode::NotFound);
    let call = started(&mut c, HelperSlot::Foreground, 7, 102);
    assert_eq!(c.helper_wait(run(1), employee(1),call, other), Err(ErrorCode::NotFound));
    assert_eq!(c.helper_cancel(run(1), employee(1),call, other, 103), Err(ErrorCode::NotFound));
    assert_eq!(c.helper_stop(run(1), employee(1),call, other, 103), Err(ErrorCode::NotFound));
    // Unknown run, unknown call.
    assert_eq!(c.helper_call(conn(), helper(5, 1, HelperSlot::Foreground, 10), Rc::new(()), [8; 16], 103).unwrap_err(),
        ErrorCode::NotFound);
    assert_eq!(c.helper_wait(run(1), employee(1),id(99), conn()), Err(ErrorCode::NotFound));
    assert!(c.host.signals.is_empty());
}

#[test]
fn helper_call_wrong_employee_refused() {
    let mut c = running(Limits::default());
    queue(&mut c, 2, 2, 102);
    c.tick(102).unwrap();
    c.tick(103).unwrap();
    // [F7] Same (shared) gateway connection, another employee's id, or another employee's run.
    assert_eq!(c.helper_call(conn(), helper(1, 2, HelperSlot::Foreground, 10), Rc::new(()), [7; 16], 104).unwrap_err(),
        ErrorCode::Unauthorized);
    assert_eq!(c.helper_call(conn(), helper(2, 1, HelperSlot::Foreground, 10), Rc::new(()), [8; 16], 104).unwrap_err(),
        ErrorCode::Unauthorized);
    assert!(c.host.started.is_empty(), "nothing reached the host");
    // The host job carries the entry's employee, not a value copied from the frame.
    let ticket = submit(&mut c, HelperSlot::Foreground, 9, 105).unwrap();
    assert_eq!(c.host.starts[&ticket].employee, employee(1));
    assert_eq!(c.host.starts[&ticket].run, run(1));
}

#[test]
fn helper_call_on_draining_run_refused() {
    // Not yet Running (Reserved / Queued).
    let mut c = Coordinator::new(Fake::default(), Limits::default()).unwrap();
    c.reserve(conn(), employee(1), run(1), 100).unwrap();
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 7, 100).unwrap_err(), ErrorCode::NotReady);
    // [F4] runtime exited: Draining from the host; Wait/Stop/Cancel still accepted.
    let mut c = running(Limits::default());
    let live = started(&mut c, HelperSlot::Recording, 6, 102);
    c.host.events.push(HelperEvent::Draining { run: run(1) });
    c.tick(103).unwrap();
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 7, 104).unwrap_err(), ErrorCode::NotReady);
    assert_eq!(c.helper_wait(run(1), employee(1),live, conn()), Err(ErrorCode::NotReady));
    c.helper_stop(run(1), employee(1),live, conn(), 104).unwrap();
    assert_eq!(c.host.signals, vec![(run(1), live, HelperSignal::Stop)]);
    // Admission Draining (cancel): refused, and the live call ends RunEnded.
    let mut c = running(Limits::default());
    let live = started(&mut c, HelperSlot::Foreground, 6, 102);
    c.cancel(run(1), conn(), 103).unwrap();
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 7, 104).unwrap_err(), ErrorCode::NotReady);
    assert_eq!(c.helper_wait(run(1), employee(1),live, conn()).unwrap(),
        SpawnResponse::HelperEnded { run: run(1), call: live, outcome: HelperOutcome::RunEnded });
    // An employee stop in progress also refuses.
    let mut c = running(Limits::default());
    c.stopping.insert(employee(1), 1);
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 7, 102).unwrap_err(), ErrorCode::NotReady);
}

#[test]
fn queued_call_counts_against_input_budgets() {
    // StartRun holds 1 byte + 1 fd; helpers 10 bytes + 1 fd each while queued or starting.
    let mut c = running(Limits { pending_input_bytes: 1 + 25, input_descriptors: 4, ..Limits::default() });
    c.host.hold = true;
    let first = submit(&mut c, HelperSlot::Foreground, 1, 102).unwrap();
    let _second = submit(&mut c, HelperSlot::Foreground, 2, 102).unwrap();
    assert_eq!(c.helper_queue_len(run(1)), 1);
    assert_eq!(c.helper_held(), (20, 2));
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 3, 102).unwrap_err(), ErrorCode::ResourceLimit, "bytes");
    // The same budget refuses a StartRun that no longer fits.
    c.reserve(conn(), employee(2), run(2), 102).unwrap();
    let big = SpawnRequest::StartRun { run: run(2), employee_id: employee(2), runtime_request: RuntimeKind::Codex,
        task_ref: None, workspace: None, prompt_bytes: 6, input_fds: List::new(vec![]).unwrap(), pty: false };
    assert_eq!(c.start(conn(), big, Rc::new(()), 102).unwrap_err(), ErrorCode::ResourceLimit);
    // Once the host sent `Start`, the first memfd is no longer held by spawnd.
    c.host.hold = false;
    assert!(matches!(reply(&mut c, first, 103), Some(SpawnResponse::HelperStarted { .. })));
    assert_eq!(c.helper_held(), (10, 1));
    // Descriptor budget: 1 (StartRun) + 1 (queued) + 2 more fit exactly 4.
    let mut c = running(Limits { input_descriptors: 3, ..Limits::default() });
    c.host.hold = true;
    submit(&mut c, HelperSlot::Foreground, 1, 102).unwrap();
    submit(&mut c, HelperSlot::Foreground, 2, 102).unwrap();
    assert_eq!(submit(&mut c, HelperSlot::Recording, 3, 102).unwrap_err(), ErrorCode::ResourceLimit, "descriptors");
}

#[test]
fn helper_busy_after_queue_bound() {
    let mut c = running(Limits::default());
    let live = started(&mut c, HelperSlot::Foreground, 1, 102);
    let queued: Vec<Ticket> = (2..=5).map(|n| submit(&mut c, HelperSlot::Foreground, n, 102).unwrap()).collect();
    assert_eq!(c.helper_queue_len(run(1)), MAX_QUEUED_CALLS);
    // The fifth waiting call is refused at once; its memfd is closed.
    let input = Rc::new(());
    assert_eq!(c.helper_call(conn(), helper(1, 1, HelperSlot::Foreground, 10), input.clone(), [6; 16], 102).unwrap_err(),
        ErrorCode::HelperBusy);
    assert_eq!(Rc::strong_count(&input), 1);
    // A free slot never queues: the recording slot starts even with a full fg queue.
    let rec = started(&mut c, HelperSlot::Recording, 7, 102);
    assert!(c.host.started.contains(&rec));
    // FIFO: the slot frees after the root-side end, then the oldest queued call starts.
    for ticket in &queued { assert_eq!(reply(&mut c, *ticket, 103), None, "still queued"); }
    end(&mut c, live, HelperOutcome::Exited(0), 104);
    assert_eq!(c.host.started.last(), Some(&id(2)));
    assert!(matches!(reply(&mut c, queued[0], 104), Some(SpawnResponse::HelperStarted { call, .. }) if call == id(2)));
    // Each queued call waits at most HELPER_QUEUE_MS, then helper_busy.
    let late = 102 + HELPER_QUEUE_MS;
    for ticket in &queued[1..] { assert_eq!(reply(&mut c, *ticket, late), error(ErrorCode::HelperBusy)); }
    assert_eq!(c.helper_queue_len(run(1)), 0);
    assert_eq!(c.helper_held(), (0, 0));
}

#[test]
fn queue_dropped_on_run_end_revocation_cancel() {
    // (a) runtime exit (host Draining): queued calls answer not_ready, memfds closed.
    let mut c = running(Limits::default());
    started(&mut c, HelperSlot::Foreground, 1, 102);
    let input = Rc::new(());
    let queued = c.helper_call(conn(), helper(1, 1, HelperSlot::Foreground, 10), input.clone(), [2; 16], 102).unwrap();
    c.host.events.push(HelperEvent::Draining { run: run(1) });
    c.tick(103).unwrap();
    assert_eq!(Rc::strong_count(&input), 1, "queued memfd closed");
    assert_eq!(reply(&mut c, queued, 103), error(ErrorCode::NotReady));
    // (b) run cancel (revoke + kill via drain) or run end: dropped, the live call ends RunEnded.
    for run_end in [false, true] {
        let mut c = running(Limits::default());
        let live = started(&mut c, HelperSlot::Foreground, 1, 102);
        let queued = submit(&mut c, HelperSlot::Foreground, 2, 102).unwrap();
        if run_end { c.host.end = true; c.tick(103).unwrap(); } else { c.cancel(run(1), conn(), 103).unwrap(); }
        assert_eq!(reply(&mut c, queued, 103), error(ErrorCode::NotReady));
        assert_eq!(c.helper_wait(run(1), employee(1),live, conn()).unwrap(),
            SpawnResponse::HelperEnded { run: run(1), call: live, outcome: HelperOutcome::RunEnded });
        assert_eq!(c.helper_held(), (0, 0));
    }
    // (c) launcher lost: queue refused helper_unavailable, every later call too.
    let mut c = running(Limits::default());
    started(&mut c, HelperSlot::Foreground, 1, 102);
    let queued = submit(&mut c, HelperSlot::Foreground, 2, 102).unwrap();
    c.host.events.push(HelperEvent::Closed { run: run(1), code: ErrorCode::HelperUnavailable });
    c.tick(103).unwrap();
    assert_eq!(reply(&mut c, queued, 103), error(ErrorCode::HelperUnavailable));
    assert_eq!(submit(&mut c, HelperSlot::Recording, 3, 104).unwrap_err(), ErrorCode::HelperUnavailable);
    // (d) HelperCancel of a queued call, and a server-abandoned ticket (disconnect/deadline).
    let mut c = running(Limits::default());
    started(&mut c, HelperSlot::Foreground, 1, 102);
    let cancelled = submit(&mut c, HelperSlot::Foreground, 2, 102).unwrap();
    c.helper_cancel(run(1), employee(1),id(2), conn(), 102).unwrap();
    assert_eq!(reply(&mut c, cancelled, 102), error(ErrorCode::NotReady));
    let abandoned = submit(&mut c, HelperSlot::Foreground, 3, 102).unwrap();
    c.abandon_ticket(abandoned);
    assert_eq!(c.helper_queue_len(run(1)), 0);
    assert_eq!(c.helper_held(), (0, 0));
    assert!(c.host.killed.is_empty() && c.host.revoked.is_empty(), "no queue drop touches the run");
}

#[test]
fn helper_queue_depth_4_and_drop_rules() {
    let mut c = running(Limits::default());
    let fg = started(&mut c, HelperSlot::Foreground, 1, 102);
    let rec = started(&mut c, HelperSlot::Recording, 2, 102);
    // Depth 4 counts both slots together.
    let inputs: Vec<Input> = (0..4).map(|_| Rc::new(())).collect();
    let tickets: Vec<Ticket> = inputs.iter().enumerate().map(|(i, input)| {
        let slot = if i % 2 == 0 { HelperSlot::Foreground } else { HelperSlot::Recording };
        c.helper_call(conn(), helper(1, 1, slot, 10), input.clone(), [10 + i as u8; 16], 102).unwrap()
    }).collect();
    assert_eq!(c.helper_queue_len(run(1)), 4);
    assert_eq!(submit(&mut c, HelperSlot::Recording, 20, 102).unwrap_err(), ErrorCode::HelperBusy);
    // A recording end starts the oldest queued recording call, not a foreground one.
    end(&mut c, rec, HelperOutcome::Exited(0), 103);
    assert_eq!(c.host.started.last(), Some(&id(11)));
    assert!(matches!(reply(&mut c, tickets[1], 103), Some(SpawnResponse::HelperStarted { .. })));
    // Run end drops the rest and closes every queued memfd.
    c.host.end = true;
    c.tick(104).unwrap();
    for (index, ticket) in tickets.iter().enumerate().filter(|(i, _)| *i != 1) {
        assert_eq!(reply(&mut c, *ticket, 104), error(ErrorCode::NotReady), "{index}");
    }
    for (index, input) in inputs.iter().enumerate() { assert_eq!(Rc::strong_count(input), 1, "{index}"); }
    assert_eq!(c.helper_wait(run(1), employee(1),fg, conn()).unwrap(),
        SpawnResponse::HelperEnded { run: run(1), call: fg, outcome: HelperOutcome::RunEnded });
    assert_eq!(c.helper_held(), (0, 0));
}

#[test]
fn in_run_job_cancel_never_kills_run() {
    let mut c = running(Limits::default());
    let fg = started(&mut c, HelperSlot::Foreground, 1, 102);
    let rec = started(&mut c, HelperSlot::Recording, 2, 102);
    // [F8] Cancel reaches only the call's slot; the run is never revoked or killed.
    c.helper_cancel(run(1), employee(1),fg, conn(), 103).unwrap();
    assert_eq!(c.host.signals, vec![(run(1), fg, HelperSignal::Cancel)]);
    // Stop is recording-only.
    assert_eq!(c.helper_stop(run(1), employee(1),fg, conn(), 103), Err(ErrorCode::InvalidRequest));
    c.helper_stop(run(1), employee(1),rec, conn(), 103).unwrap();
    assert_eq!(c.host.signals.last(), Some(&(run(1), rec, HelperSignal::Stop)));
    end(&mut c, fg, HelperOutcome::Signalled(9), 104);
    c.tick(105).unwrap();
    assert!(c.host.killed.is_empty() && c.host.revoked.is_empty());
    assert_eq!(c.status(run(1), conn()).unwrap(), RunStatus::Running);
    assert_eq!(c.entries[&run(1).0].state, AdmissionState::Running);
    // Idempotent after the end; the fg slot is free for the next call.
    c.helper_cancel(run(1), employee(1),fg, conn(), 105).unwrap();
    assert_eq!(c.host.signals.len(), 2, "no signal for an ended call");
    started(&mut c, HelperSlot::Foreground, 3, 106);
    // A cancel that races the host's start is applied as soon as the start lands.
    end(&mut c, rec, HelperOutcome::Exited(0), 107);
    c.host.hold = true;
    let ticket = submit(&mut c, HelperSlot::Recording, 4, 107).unwrap();
    assert_eq!(reply(&mut c, ticket, 107), None);
    c.helper_cancel(run(1), employee(1),id(4), conn(), 107).unwrap();
    c.host.hold = false;
    assert_eq!(reply(&mut c, ticket, 108), error(ErrorCode::NotReady));
    assert_eq!(c.host.signals.last(), Some(&(run(1), id(4), HelperSignal::Cancel)));
    assert!(c.host.killed.is_empty() && c.host.revoked.is_empty());
    assert_eq!(c.status(run(1), conn()).unwrap(), RunStatus::Running);
}

#[test]
fn deadline_refuses_below_five_seconds() {
    // min(tool, binding, run deadline − grace − 5 s).
    assert_eq!(effective_timeout_ms(60_000, 30_000, 600_000, 15_000), Ok(30_000));
    assert_eq!(effective_timeout_ms(10_000, 30_000, 600_000, 15_000), Ok(10_000));
    assert_eq!(effective_timeout_ms(60_000, 300_000, 40_000, 15_000), Ok(20_000));
    assert_eq!(effective_timeout_ms(600_000, 600_000, u64::MAX, 0), Ok(MAX_HELPER_TIMEOUT_MS));
    // A run budget below 5 s refuses; exactly 5 s is enough.
    assert_eq!(effective_timeout_ms(60_000, 60_000, 15_000 + RESULT_GRACE_MS + 4_999, 15_000), Err(ErrorCode::RunTimeExhausted));
    assert_eq!(effective_timeout_ms(60_000, 60_000, 15_000 + RESULT_GRACE_MS + 5_000, 15_000), Ok(5_000));
    assert_eq!(effective_timeout_ms(60_000, 60_000, 0, 0), Err(ErrorCode::RunTimeExhausted));
    assert_eq!(effective_timeout_ms(0, 60_000, 600_000, 0), Err(ErrorCode::InvalidRequest));
    // The coordinator refuses before the host when the run itself has < 5 s left.
    let mut c = Coordinator::new(Fake { runtime_ms: Some(10_000), ..Fake::default() }, Limits::default()).unwrap();
    queue(&mut c, 1, 1, 100);
    c.tick(100).unwrap();
    c.tick(101).unwrap();
    assert!(submit(&mut c, HelperSlot::Foreground, 1, 100 + 4_000).is_ok());
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 2, 100 + 5_001).unwrap_err(), ErrorCode::RunTimeExhausted);
    // A host refusal (its own, grace-aware computation) reaches the caller unchanged.
    let mut c = running(Limits::default());
    c.host.refuse_poll = Some(ErrorCode::RunTimeExhausted);
    let ticket = submit(&mut c, HelperSlot::Foreground, 3, 102).unwrap();
    assert_eq!(reply(&mut c, ticket, 102), error(ErrorCode::RunTimeExhausted));
    // ...and frees the slot for the next call.
    c.host.refuse_poll = None;
    started(&mut c, HelperSlot::Foreground, 4, 103);
}

#[test]
fn hx4_call_ids_and_feature_bit() {
    let mut c = running(Limits::default());
    // Never all-zero, never reused within the run.
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 0, 102).unwrap_err(), ErrorCode::Internal);
    let first = started(&mut c, HelperSlot::Foreground, 7, 102);
    end(&mut c, first, HelperOutcome::Exited(0), 103);
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 7, 104).unwrap_err(), ErrorCode::Internal);
    // [F17] Reserved carries the host's one feature set.
    let reserved = c.reserve(conn(), employee(2), run(2), 104).unwrap();
    assert!(matches!(reserved, SpawnResponse::Reserved { features, .. }
        if features == SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS)));
    // Without launcher plans / slot cgroups: no bit, and every call gets the host's code.
    c.host.not_ready = Some(ErrorCode::HelperCgroupUnavailable);
    let reserved = c.reserve(conn(), employee(3), run(3), 104).unwrap();
    assert!(matches!(reserved, SpawnResponse::Reserved { features, .. } if features == SpawndFeatures::WORK_BROKER));
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 8, 104).unwrap_err(), ErrorCode::HelperCgroupUnavailable);
    // A run-wide start refusal closes that run's helpers.
    c.host.not_ready = None;
    c.host.fail_start = Some(ErrorCode::HelperUnavailable);
    let ticket = submit(&mut c, HelperSlot::Foreground, 9, 105).unwrap();
    assert_eq!(reply(&mut c, ticket, 105), error(ErrorCode::HelperUnavailable));
    c.host.fail_start = None;
    assert_eq!(submit(&mut c, HelperSlot::Foreground, 10, 106).unwrap_err(), ErrorCode::HelperUnavailable);
}

#[test]
fn hxfix_wait_stop_cancel_wrong_employee_refused() {
    // Finding 8: same owning connection, another employee's id ⇒ Unauthorized, nothing signalled.
    let mut c = running(Limits::default());
    let rec = started(&mut c, HelperSlot::Recording, 1, 102);
    assert_eq!(c.helper_wait(run(1), employee(2), rec, conn()), Err(ErrorCode::Unauthorized));
    assert_eq!(c.helper_stop(run(1), employee(2), rec, conn(), 103), Err(ErrorCode::Unauthorized));
    assert_eq!(c.helper_cancel(run(1), employee(2), rec, conn(), 103), Err(ErrorCode::Unauthorized));
    assert!(c.host.signals.is_empty());
    // Ownership is still checked first (another connection: NotFound, as before).
    assert_eq!(c.helper_cancel(run(1), employee(1), rec, ControlConnectionId([9; 16]), 103), Err(ErrorCode::NotFound));
    // The run's own employee is accepted.
    c.helper_stop(run(1), employee(1), rec, conn(), 103).unwrap();
    assert_eq!(c.host.signals, vec![(run(1), rec, HelperSignal::Stop)]);
    assert_eq!(c.run_employee(run(1)), Some(employee(1)));
    assert_eq!(c.run_employee(run(9)), None);
}

#[test]
fn hxfix_dropped_cancel_is_retried_until_delivered() {
    // Finding 6: a full worker mailbox never silently drops a Cancel.
    let mut c = running(Limits::default());
    let fg = started(&mut c, HelperSlot::Foreground, 1, 102);
    c.host.refuse_signals = true;
    c.helper_cancel(run(1), employee(1), fg, conn(), 103).unwrap();
    c.tick(104).unwrap();
    assert!(c.host.signals.is_empty(), "still refused");
    c.host.refuse_signals = false;
    c.tick(105).unwrap();
    assert_eq!(c.host.signals, vec![(run(1), fg, HelperSignal::Cancel)]);
    c.tick(106).unwrap();
    assert_eq!(c.host.signals.len(), 1, "delivered exactly once");
    // A cancel that raced the start, applied when the start lands into a full mailbox.
    end(&mut c, fg, HelperOutcome::Signalled(9), 107);
    c.host.hold = true;
    let ticket = submit(&mut c, HelperSlot::Foreground, 2, 107).unwrap();
    c.helper_cancel(run(1), employee(1), id(2), conn(), 107).unwrap();
    c.host.hold = false;
    c.host.refuse_signals = true;
    assert_eq!(reply(&mut c, ticket, 108), error(ErrorCode::NotReady));
    assert_eq!(c.host.signals.len(), 1);
    c.host.refuse_signals = false;
    c.tick(109).unwrap();
    assert_eq!(c.host.signals.last(), Some(&(run(1), id(2), HelperSignal::Cancel)));
    // A held signal is dropped once the host ended the call root-side (slot drained).
    end(&mut c, id(2), HelperOutcome::Signalled(9), 110);
    let rec = started(&mut c, HelperSlot::Recording, 3, 110);
    c.host.refuse_signals = true;
    c.helper_stop(run(1), employee(1), rec, conn(), 110).unwrap();
    end(&mut c, rec, HelperOutcome::Exited(0), 111);
    c.host.refuse_signals = false;
    c.tick(112).unwrap();
    assert!(!c.host.signals.iter().any(|(_, call, _)| *call == rec), "nothing left to signal");
    assert!(c.host.killed.is_empty() && c.host.revoked.is_empty(), "never the run");
}
