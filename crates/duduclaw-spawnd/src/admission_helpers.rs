//! HX4 helper calls, coordinator half (D8 revision §2.2, §2.5, §3 [F4, F5, F7,
//! F8], Q4 = A). Pure and host-independent: the coordinator owns call
//! identity, admission (connection, employee, `Running`, run time), the per-run
//! FIFO and its budgets; the host owns every root side effect (memfd
//! revalidation, helper derivation from the run's employee, slot limits,
//! pipes, `Start`, watchdog, `cgroup.kill` + `populated 0`, OOM readback).
//!
//! Rules enforced here:
//! - [F7] `HelperCall` is accepted only from the run's owning connection
//!   (`owned`) AND for the run's own employee (`entry.employee == employee_id`);
//!   the host re-derives the helper from its own record of the run's employee.
//! - [F4] only while the admission entry is `Running`, no employee stop is
//!   pending, and the host has not reported `Draining` (runtime exited).
//! - Call ids are spawnd-generated (16 random bytes from the server), never
//!   all-zero, never reused within the run, returned in `HelperStarted`.
//! - Q4 = A: one foreground and one recording slot per run; while a slot is
//!   busy, further calls wait in a per-run FIFO of at most
//!   [`MAX_QUEUED_CALLS`] (both slots together), each for at most
//!   [`HELPER_QUEUE_MS`], then `helper_busy`. Queued (and starting) input memfds
//!   count against `Limits::pending_input_bytes` / `input_descriptors`, shared
//!   with StartRun. The queue is dropped (fds closed) on run end / drain
//!   (cancel, revocation, intrusion, timeouts), runtime exit (`Draining`),
//!   launcher loss, a refused ticket and `HelperCancel`.
//! - [F8] in-run jobs are keyed `(run, call)`; `HelperStop`/`HelperCancel`
//!   only ever reach the host's per-call signal and NEVER `revoke`/`kill` of the
//!   run (there is no code path from these ops to `drain`).
//! - No helper call creates an admission entry or reservation, so it never
//!   occupies the employee's D8 slot or the host run budget.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind}, ids::EmployeeId,
    ipc::{AdmissionState, ControlConnectionId, ErrorCode, HelperCallId, HelperOutcome, HelperSlot, SpawnRequest,
        SpawnResponse, SpawndFeatures, MAX_HELPER_TIMEOUT_MS, MIN_HELPER_RUN_REMAINDER_MS}};
use super::{Coordinator, Host, Pending, Ticket};
use crate::Result;

/// Q4 = A: queued calls per run (both slots together).
pub const MAX_QUEUED_CALLS: usize = 4;
/// A queued call waits at most this long for its slot, then `helper_busy`.
/// Below `BROKER_REPLY_MS` so the coordinator, not the server's generic
/// deadline, answers every queued call.
pub const HELPER_QUEUE_MS: u64 = 3_000;
/// [F5] spawnd's own watchdog fires this long after the effective timeout.
pub const WATCHDOG_GRACE_MS: u64 = 2_000;
/// Result-collection reserve below the run deadline (§2.5; equals
/// `helper_policy::RESULT_GRACE_MS` and the gateway's value).
pub const RESULT_GRACE_MS: u64 = 5_000;
/// Ended calls whose `HelperWait` has not come yet, per run; oldest dropped.
pub const MAX_ENDED_RECORDS: usize = 16;
/// Distinct call ids per run (`helper_wire::MAX_CALLS_PER_RUN`).
pub const MAX_CALLS_PER_RUN: usize = 65_536;
/// Each queued or starting call holds exactly its input memfd.
pub const CALL_DESCRIPTORS: usize = 1;

/// §2.5: min(tool timeout, binding timeout, run deadline − now − termination
/// grace − result grace). A run budget below `MIN_HELPER_RUN_REMAINDER_MS`
/// refuses the call (`run_time_exhausted`); the result is never longer than
/// the request or `MAX_HELPER_TIMEOUT_MS`.
pub fn effective_timeout_ms(requested_ms: u64, binding_ms: u64, run_remaining_ms: u64,
    termination_grace_ms: u64) -> Result<u64> {
    if requested_ms == 0 || binding_ms == 0 { return Err(ErrorCode::InvalidRequest); }
    let run_budget = run_remaining_ms.saturating_sub(termination_grace_ms).saturating_sub(RESULT_GRACE_MS);
    if run_budget < MIN_HELPER_RUN_REMAINDER_MS { return Err(ErrorCode::RunTimeExhausted); }
    Ok(requested_ms.min(binding_ms).min(run_budget).min(MAX_HELPER_TIMEOUT_MS))
}

/// One call handed to the host. `employee` is the admission entry's employee
/// (already compared with the request); the host compares it with its own
/// record and derives the helper from it. `run_remaining_ms` is measured from
/// the admission start and the policy's runtime bound.
pub struct HelperStart<I> {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub call: HelperCallId,
    pub helper: RuntimeKind,
    pub slot: HelperSlot,
    pub timeout_ms: u64,
    pub input_bytes: u64,
    pub input: I,
    pub run_remaining_ms: u64,
}
/// The host sent `Start`: `pipes` are exactly `[StdoutRead, StderrRead]`.
pub struct HelperStarted<Io> { pub effective_timeout_ms: u64, pub pipes: Io }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperSignal {
    /// Recording: ask the helper to flush and exit (`stop\n`).
    Stop,
    /// Kill the call's slot cgroup (root `cgroup.kill`), never the run.
    Cancel,
}
/// Root-determined helper facts reported by the host (never launcher frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperEvent {
    /// After `cgroup.kill` + `populated 0` of the slot; the slot is free.
    Ended { run: RunHandle, call: HelperCallId, outcome: HelperOutcome },
    /// The runtime exited (`rt-runtime` `populated 0`, or the launcher said so).
    Draining { run: RunHandle },
    /// The control channel is gone (EOF, protocol violation, rate) or the slot
    /// cgroups failed; every later call of the run gets `code`.
    Closed { run: RunHandle, code: ErrorCode },
}
/// A finished `HelperCall` reply. `pipes` accompany `HelperStarted` only.
pub struct HelperReply<Io> { pub response: SpawnResponse, pub pipes: Option<Io> }
impl<Io> HelperReply<Io> {
    fn error(code: ErrorCode) -> Self { Self { response: SpawnResponse::Error { code }, pipes: None } }
}

/// The ticket record of one outstanding `HelperCall` reply.
pub(super) struct HelperTicket { pub(super) run: RunHandle, pub(super) call: HelperCallId, pub(super) refused: Option<ErrorCode> }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallState {
    Queued,
    /// Submitted to the host; the reply (or its loss) is pending.
    Starting,
    Live,
    Ended(HelperOutcome),
}
struct Call {
    slot: HelperSlot,
    state: CallState,
    ticket: Ticket,
    timeout_ms: u64,
    input_bytes: u64,
    /// Requested before the host answered the start; applied on `Started`.
    stop_pending: bool,
    cancel_pending: bool,
    /// Nobody waits for the start reply any more (run ended, ticket abandoned):
    /// a late success is cancelled and its pipes dropped.
    orphaned: bool,
}
struct Queued<I> { call: HelperCallId, slot: HelperSlot, helper: RuntimeKind, input: I, deadline: u64 }

/// Per-run helper state in the coordinator.
pub(super) struct RunHelpers<I> {
    queue: VecDeque<Queued<I>>,
    calls: BTreeMap<HelperCallId, Call>,
    ended: VecDeque<HelperCallId>,
    used: BTreeSet<HelperCallId>,
    /// Runtime exited, or the run left `Running`: no new call, queue dropped.
    draining: bool,
    /// Launcher or slot cgroups gone: every new call gets this code.
    closed: Option<ErrorCode>,
    /// HX security fix round (finding 6): per-call signals the host's mailbox
    /// refused. Retried every tick until delivered or the host reports the
    /// call ended root-side (slot killed and drained). One entry per call id
    /// (a `Cancel` replaces a `Stop`), so bounded by `MAX_CALLS_PER_RUN`.
    pending_signals: Vec<(HelperCallId, HelperSignal)>,
}
impl<I> RunHelpers<I> {
    fn new() -> Self {
        Self { queue: VecDeque::new(), calls: BTreeMap::new(), ended: VecDeque::new(), used: BTreeSet::new(),
            draining: false, closed: None, pending_signals: Vec::new() }
    }
    /// Record an undelivered signal; `Cancel` always wins over `Stop`.
    fn hold_signal(&mut self, call: HelperCallId, signal: HelperSignal) {
        match self.pending_signals.iter_mut().find(|(pending, _)| *pending == call) {
            Some(entry) => if signal == HelperSignal::Cancel { entry.1 = HelperSignal::Cancel },
            None => self.pending_signals.push((call, signal)),
        }
    }
    fn slot_active(&self, slot: HelperSlot) -> bool {
        self.calls.values().any(|call| call.slot == slot && matches!(call.state, CallState::Starting | CallState::Live))
    }
    /// (input bytes, descriptors) held for queued and starting calls.
    fn held(&self) -> (u64, usize) {
        self.calls.values().filter(|call| matches!(call.state, CallState::Queued | CallState::Starting))
            .fold((0, 0), |(bytes, fds), call| (bytes.saturating_add(call.input_bytes), fds + CALL_DESCRIPTORS))
    }
    fn end(&mut self, call: HelperCallId, outcome: HelperOutcome) {
        let Some(record) = self.calls.get_mut(&call) else { return };
        record.state = CallState::Ended(outcome);
        self.ended.push_back(call);
        while self.ended.len() > MAX_ENDED_RECORDS {
            if let Some(oldest) = self.ended.pop_front() { self.calls.remove(&oldest); }
        }
    }
    #[cfg(test)]
    pub(super) fn queued(&self) -> usize { self.queue.len() }
}

impl<H: Host> Coordinator<H> {
    /// Input bytes and descriptors held by helper calls of every run.
    pub(super) fn helper_held(&self) -> (u64, usize) {
        self.helpers.values().map(RunHelpers::held)
            .fold((0, 0), |(bytes, fds), (b, f)| (bytes.saturating_add(b), fds + f))
    }
    /// The one feature set this spawnd advertises (also in readiness).
    pub fn features(&mut self) -> SpawndFeatures { self.host.features() }
    fn run_remaining(&self, run: RunHandle, now: u64) -> Result<u64> {
        let entry = self.entries.get(&run.0).ok_or(ErrorCode::NotFound)?;
        let policy = entry.policy.as_ref().ok_or(ErrorCode::Internal)?;
        Ok(entry.started.saturating_add(policy.runtime_ms).saturating_sub(now))
    }
    /// HX4 `HelperCall`. `random_call` comes from the server's CSPRNG. Returns
    /// a ticket answered by [`Self::poll_helper_ticket`]. `input` is exactly
    /// the request's `PromptSealed` memfd (the server checked the fd plan).
    pub fn helper_call(&mut self, connection: ControlConnectionId, request: SpawnRequest, input: H::Inputs,
        random_call: [u8; 16], now: u64) -> Result<Ticket> {
        self.time(now)?;
        request.validate().map_err(|_| ErrorCode::InvalidRequest)?;
        let SpawnRequest::HelperCall { run, employee_id, helper, slot, timeout_ms, input_bytes } = request
            else { return Err(ErrorCode::InvalidRequest); };
        let entry = self.owned(run, connection)?;
        // [F7] the connection is shared by every employee; the run's employee is the authority.
        if entry.employee != employee_id { return Err(ErrorCode::Unauthorized); }
        if entry.state != AdmissionState::Running || self.stopping.contains_key(&employee_id) { return Err(ErrorCode::NotReady); }
        if self.run_remaining(run, now)? < MIN_HELPER_RUN_REMAINDER_MS { return Err(ErrorCode::RunTimeExhausted); }
        self.host.helpers_ready()?;
        if let Some(helpers) = self.helpers.get(&run.0) {
            if helpers.draining { return Err(ErrorCode::NotReady); }
            if let Some(code) = helpers.closed { return Err(code); }
            if helpers.used.len() >= MAX_CALLS_PER_RUN { return Err(ErrorCode::ResourceLimit); }
        }
        let call = HelperCallId::new(random_call).map_err(|_| ErrorCode::Internal)?;
        // Budgets shared with StartRun: queued/starting memfds are admission inputs.
        let (helper_bytes, helper_fds) = self.helper_held();
        let pending: u64 = self.entries.values().map(|entry| entry.input_bytes).sum::<u64>().saturating_add(helper_bytes);
        let held: usize = self.entries.values().map(|entry| entry.input_descriptors).sum::<usize>() + helper_fds;
        if input_bytes > self.limits.pending_input_bytes.saturating_sub(pending)
            || CALL_DESCRIPTORS > self.limits.input_descriptors.saturating_sub(held) { return Err(ErrorCode::ResourceLimit); }
        let helpers = self.helpers.entry(run.0).or_insert_with(RunHelpers::new);
        if helpers.used.contains(&call) { return Err(ErrorCode::Internal); }
        let wait = helpers.slot_active(slot) || helpers.queue.iter().any(|queued| queued.slot == slot);
        if wait && helpers.queue.len() >= MAX_QUEUED_CALLS { return Err(ErrorCode::HelperBusy); }
        let ticket = self.ticket()?;
        let helpers = self.helpers.get_mut(&run.0).ok_or(ErrorCode::Internal)?;
        helpers.used.insert(call);
        helpers.calls.insert(call, Call { slot, state: CallState::Queued, ticket, timeout_ms, input_bytes,
            stop_pending: false, cancel_pending: false, orphaned: false });
        helpers.queue.push_back(Queued { call, slot, helper, input, deadline: now.saturating_add(HELPER_QUEUE_MS) });
        self.tickets.insert(ticket, Pending::Helper(HelperTicket { run, call, refused: None }));
        self.pump_helpers(run, now);
        Ok(ticket)
    }
    /// Start the head of the FIFO for every free slot of `run`.
    fn pump_helpers(&mut self, run: RunHandle, now: u64) {
        let running = self.entries.get(&run.0).is_some_and(|entry| entry.state == AdmissionState::Running);
        let remaining = self.run_remaining(run, now).unwrap_or(0);
        let employee = match self.entries.get(&run.0) { Some(entry) => entry.employee, None => return };
        loop {
            let Some(helpers) = self.helpers.get_mut(&run.0) else { return };
            if !running || helpers.draining || helpers.closed.is_some() { return; }
            // FIFO per slot: the oldest unexpired queued call whose slot is free
            // (an earlier call of the same slot can only be expired: its own
            // poll answers it `helper_busy`).
            let Some(position) = helpers.queue.iter().position(|queued| now < queued.deadline && !helpers.slot_active(queued.slot))
                else { return };
            let Some(queued) = helpers.queue.remove(position) else { return };
            let Some(record) = helpers.calls.get_mut(&queued.call) else { continue };
            record.state = CallState::Starting;
            let (ticket, timeout_ms, input_bytes) = (record.ticket, record.timeout_ms, record.input_bytes);
            let job = HelperStart { run, employee, call: queued.call, helper: queued.helper, slot: queued.slot,
                timeout_ms, input_bytes, input: queued.input, run_remaining_ms: remaining };
            if let Err(code) = self.host.helper_start(ticket, job) { self.refuse_call(run, queued.call, code); }
        }
    }
    /// Forget a call that never started and answer its ticket with `code`.
    fn refuse_call(&mut self, run: RunHandle, call: HelperCallId, code: ErrorCode) {
        let Some(helpers) = self.helpers.get_mut(&run.0) else { return };
        helpers.queue.retain(|queued| queued.call != call);
        if let Some(record) = helpers.calls.remove(&call) {
            if let Some(Pending::Helper(ticket)) = self.tickets.get_mut(&record.ticket) { ticket.refused = Some(code); }
        }
        // A run-wide refusal closes the run's helpers for every later call.
        if matches!(code, ErrorCode::HelperUnavailable | ErrorCode::HelperCgroupUnavailable) { self.close_helpers(run, code); }
    }
    /// Drop the whole queue (closing each memfd) and refuse its tickets.
    fn drop_queue(&mut self, run: RunHandle, code: ErrorCode) {
        let Some(helpers) = self.helpers.get_mut(&run.0) else { return };
        let dropped: Vec<HelperCallId> = helpers.queue.drain(..).map(|queued| queued.call).collect();
        for call in dropped {
            if let Some(record) = helpers.calls.remove(&call) {
                if let Some(Pending::Helper(ticket)) = self.tickets.get_mut(&record.ticket) { ticket.refused = Some(code); }
            }
        }
    }
    fn close_helpers(&mut self, run: RunHandle, code: ErrorCode) {
        if let Some(helpers) = self.helpers.get_mut(&run.0) { helpers.closed.get_or_insert(code); }
        self.drop_queue(run, code);
    }
    /// The run left `Running` (drain: cancel, revocation, intrusion, timeout,
    /// exit) or the runtime exited: no new call, queue dropped; when the run
    /// itself ended, live calls end `RunEnded` (§3 step 6).
    pub(super) fn end_helpers(&mut self, run: RunHandle, run_ended: bool) {
        let Some(helpers) = self.helpers.get_mut(&run.0) else { return };
        helpers.draining = true;
        if run_ended {
            let live: Vec<HelperCallId> = helpers.calls.iter().filter(|(_, call)| call.state == CallState::Live)
                .map(|(id, _)| *id).collect();
            for call in live { helpers.end(call, HelperOutcome::RunEnded); }
            for call in helpers.calls.values_mut().filter(|call| call.state == CallState::Starting) { call.orphaned = true; }
        }
        self.drop_queue(run, ErrorCode::NotReady);
    }
    /// Root-side facts from the host. Unknown runs/calls are ignored.
    fn apply_helper_event(&mut self, event: HelperEvent, now: u64) {
        match event {
            HelperEvent::Ended { run, call, outcome } => {
                let Some(helpers) = self.helpers.get_mut(&run.0) else { return };
                // The slot was killed and drained root-side: nothing left to signal.
                helpers.pending_signals.retain(|(pending, _)| *pending != call);
                if helpers.calls.get(&call).is_some_and(|record| record.state == CallState::Live) {
                    helpers.end(call, outcome);
                    self.pump_helpers(run, now);
                }
            }
            HelperEvent::Draining { run } => self.end_helpers(run, false),
            HelperEvent::Closed { run, code } => self.close_helpers(run, code),
        }
    }
    /// Called by `tick`: host events, run-state changes, expired waits,
    /// orphaned starts, then the queues.
    pub(super) fn tick_helpers(&mut self, now: u64) {
        for event in self.host.helper_events() { self.apply_helper_event(event, now); }
        let runs: Vec<_> = self.helpers.keys().copied().collect();
        for id in runs {
            let run = RunHandle(id);
            match self.entries.get(&id).map(|entry| entry.state) {
                None => {
                    // The entry is gone: every outstanding ticket answers NotReady.
                    self.end_helpers(run, true);
                    let orphans = self.helpers.get(&id).is_some_and(|helpers|
                        helpers.calls.values().any(|call| call.state == CallState::Starting));
                    if !orphans { self.helpers.remove(&id); }
                }
                Some(AdmissionState::Running) => {}
                Some(_) => self.end_helpers(run, true),
            }
            self.poll_orphans(run);
            self.retry_signals(run);
            self.pump_helpers(run, now);
        }
    }
    /// [finding 6] A per-call signal is never dropped: a refused delivery
    /// (full worker mailbox) is held and retried here.
    fn signal_call(&mut self, run: RunHandle, call: HelperCallId, signal: HelperSignal) {
        if self.host.helper_signal(run, call, signal).is_ok() { return; }
        if let Some(helpers) = self.helpers.get_mut(&run.0) { helpers.hold_signal(call, signal); }
    }
    fn retry_signals(&mut self, run: RunHandle) {
        let Some(helpers) = self.helpers.get_mut(&run.0) else { return };
        let pending = std::mem::take(&mut helpers.pending_signals);
        let mut undelivered = Vec::new();
        for (call, signal) in pending {
            if self.host.helper_signal(run, call, signal).is_err() { undelivered.push((call, signal)); }
        }
        if let Some(helpers) = self.helpers.get_mut(&run.0) {
            for (call, signal) in undelivered { helpers.hold_signal(call, signal); }
        }
    }
    /// Starts whose ticket the server abandoned: nobody receives the reply, so
    /// drop the pipes and kill the call. (While the server still holds the
    /// ticket, `poll_helper_ticket` alone collects the start.)
    fn poll_orphans(&mut self, run: RunHandle) {
        let Some(helpers) = self.helpers.get(&run.0) else { return };
        let orphans: Vec<(HelperCallId, Ticket)> = helpers.calls.iter()
            .filter(|(_, call)| call.state == CallState::Starting && !self.tickets.contains_key(&call.ticket))
            .map(|(id, call)| (*id, call.ticket)).collect();
        for (call, ticket) in orphans {
            match self.host.helper_poll(ticket) {
                Ok(None) => {}
                Ok(Some(started)) => {
                    drop(started.pipes);
                    self.signal_call(run, call, HelperSignal::Cancel);
                    if let Some(helpers) = self.helpers.get_mut(&run.0) {
                        if let Some(record) = helpers.calls.get_mut(&call) { record.state = CallState::Live; }
                        if helpers.draining { helpers.end(call, HelperOutcome::RunEnded); }
                    }
                }
                Err(_) => { if let Some(helpers) = self.helpers.get_mut(&run.0) { helpers.calls.remove(&call); } }
            }
        }
    }
    /// `Ok(None)` while pending; the reply exactly once; an unknown ticket is `NotFound`.
    pub fn poll_helper_ticket(&mut self, ticket: Ticket, now: u64) -> Result<Option<HelperReply<H::Io>>> {
        self.time(now)?;
        let (run, call, refused) = match self.tickets.get(&ticket).ok_or(ErrorCode::NotFound)? {
            Pending::Helper(record) => (record.run, record.call, record.refused),
            _ => return Err(ErrorCode::Internal),
        };
        if let Some(code) = refused {
            self.tickets.remove(&ticket);
            return Ok(Some(HelperReply::error(code)));
        }
        self.pump_helpers(run, now);
        let state = self.helpers.get(&run.0).and_then(|helpers| helpers.calls.get(&call)).map(|record| (record.state, record.orphaned));
        let reply = match state {
            None => HelperReply::error(ErrorCode::NotReady),
            Some((CallState::Queued, _)) => {
                let expired = self.helpers.get(&run.0).and_then(|helpers| helpers.queue.iter().find(|queued| queued.call == call))
                    .is_none_or(|queued| now >= queued.deadline);
                if !expired { return Ok(None); }
                self.refuse_call(run, call, ErrorCode::HelperBusy);
                HelperReply::error(ErrorCode::HelperBusy)
            }
            Some((CallState::Starting, orphaned)) => match self.host.helper_poll(ticket) {
                Ok(None) => return Ok(None),
                Ok(Some(started)) => self.started(run, call, orphaned, started),
                Err(code) => {
                    if let Some(helpers) = self.helpers.get_mut(&run.0) { helpers.calls.remove(&call); }
                    if matches!(code, ErrorCode::HelperUnavailable | ErrorCode::HelperCgroupUnavailable) { self.close_helpers(run, code); }
                    self.pump_helpers(run, now);
                    HelperReply::error(code)
                }
            },
            Some((CallState::Live | CallState::Ended(_), _)) => HelperReply::error(ErrorCode::Internal),
        };
        self.tickets.remove(&ticket);
        Ok(Some(reply))
    }
    fn started(&mut self, run: RunHandle, call: HelperCallId, orphaned: bool, started: HelperStarted<H::Io>) -> HelperReply<H::Io> {
        let Some(record) = self.helpers.get_mut(&run.0).and_then(|helpers| helpers.calls.get_mut(&call))
            else { return HelperReply::error(ErrorCode::NotReady) };
        record.state = CallState::Live;
        let (stop, cancel, requested) = (record.stop_pending, record.cancel_pending, record.timeout_ms);
        // The host's effective timeout can only narrow the request.
        let valid = started.effective_timeout_ms != 0 && started.effective_timeout_ms <= requested.min(MAX_HELPER_TIMEOUT_MS);
        if orphaned || cancel || !valid {
            drop(started.pipes);
            self.signal_call(run, call, HelperSignal::Cancel);
            if let Some(helpers) = self.helpers.get_mut(&run.0) {
                if orphaned { helpers.end(call, HelperOutcome::RunEnded); }
            }
            return HelperReply::error(if !valid { ErrorCode::Internal } else { ErrorCode::NotReady });
        }
        if stop { self.signal_call(run, call, HelperSignal::Stop); }
        HelperReply { response: SpawnResponse::HelperStarted { run, call, effective_timeout_ms: started.effective_timeout_ms },
            pipes: Some(started.pipes) }
    }
    /// The server gave up on a `HelperCall` reply (deadline or disconnect).
    pub(super) fn abandon_helper(&mut self, record: HelperTicket) {
        let Some(helpers) = self.helpers.get_mut(&record.run.0) else { return };
        match helpers.calls.get(&record.call).map(|call| call.state) {
            Some(CallState::Queued) => {
                helpers.queue.retain(|queued| queued.call != record.call);
                helpers.calls.remove(&record.call);
            }
            // `poll_orphans` collects the late start and cancels it.
            Some(CallState::Starting) => { if let Some(call) = helpers.calls.get_mut(&record.call) { call.orphaned = true; } }
            _ => {}
        }
    }
    /// [F7, finding 8] Wait/Stop/Cancel are scoped like `HelperCall`: the
    /// owning connection AND the run's own employee.
    fn helper_record(&self, run: RunHandle, employee: EmployeeId, call: HelperCallId, connection: ControlConnectionId)
        -> Result<(HelperSlot, CallState)> {
        let entry = self.owned(run, connection)?;
        if entry.employee != employee { return Err(ErrorCode::Unauthorized); }
        let record = self.helpers.get(&run.0).and_then(|helpers| helpers.calls.get(&call)).ok_or(ErrorCode::NotFound)?;
        Ok((record.slot, record.state))
    }
    /// The admission entry's employee of `run` (audit attribution only; the
    /// server logs it instead of a request's claim).
    pub fn run_employee(&self, run: RunHandle) -> Option<EmployeeId> { self.entries.get(&run.0).map(|entry| entry.employee) }
    /// HX4 `HelperWait`: a bounded poll like `Wait` on wire v1 (no reply is
    /// held). `NotReady` while queued/starting/live; the outcome exactly once.
    /// Allowed in every admission state while the entry exists.
    pub fn helper_wait(&mut self, run: RunHandle, employee: EmployeeId, call: HelperCallId, connection: ControlConnectionId)
        -> Result<SpawnResponse> {
        match self.helper_record(run, employee, call, connection)?.1 {
            CallState::Ended(outcome) => {
                if let Some(helpers) = self.helpers.get_mut(&run.0) {
                    helpers.calls.remove(&call);
                    helpers.ended.retain(|id| *id != call);
                }
                Ok(SpawnResponse::HelperEnded { run, call, outcome })
            }
            _ => Err(ErrorCode::NotReady),
        }
    }
    /// HX4 `HelperStop` (recording only): flush and exit. Never touches the run.
    /// A refused delivery is held and retried each tick (finding 6), so the
    /// stop is accepted once the call is known and live.
    pub fn helper_stop(&mut self, run: RunHandle, employee: EmployeeId, call: HelperCallId, connection: ControlConnectionId,
        now: u64) -> Result<()> {
        self.time(now)?;
        let (slot, state) = self.helper_record(run, employee, call, connection)?;
        if slot != HelperSlot::Recording { return Err(ErrorCode::InvalidRequest); }
        match state {
            CallState::Queued => { self.refuse_call(run, call, ErrorCode::NotReady); Ok(()) }
            CallState::Starting => { self.mark(run, call, |record| record.stop_pending = true); Ok(()) }
            CallState::Live => { self.signal_call(run, call, HelperSignal::Stop); Ok(()) }
            CallState::Ended(_) => Ok(()),
        }
    }
    /// HX4 `HelperCancel`: kill the call's slot cgroup, queued or running
    /// [F8]. Idempotent. The run, its lease, MCP and egress are never touched.
    /// A refused delivery is held and retried until the call ends root-side.
    pub fn helper_cancel(&mut self, run: RunHandle, employee: EmployeeId, call: HelperCallId, connection: ControlConnectionId,
        now: u64) -> Result<()> {
        self.time(now)?;
        match self.helper_record(run, employee, call, connection)?.1 {
            CallState::Queued => { self.refuse_call(run, call, ErrorCode::NotReady); self.pump_helpers(run, now); Ok(()) }
            CallState::Starting => { self.mark(run, call, |record| record.cancel_pending = true); Ok(()) }
            CallState::Live => { self.signal_call(run, call, HelperSignal::Cancel); Ok(()) }
            CallState::Ended(_) => Ok(()),
        }
    }
    fn mark(&mut self, run: RunHandle, call: HelperCallId, update: impl FnOnce(&mut Call)) {
        if let Some(record) = self.helpers.get_mut(&run.0).and_then(|helpers| helpers.calls.get_mut(&call)) { update(record); }
    }
    #[cfg(test)]
    pub(super) fn helper_queue_len(&self, run: RunHandle) -> usize { self.helpers.get(&run.0).map_or(0, RunHelpers::queued) }
}

#[cfg(test)]
#[path = "admission_helpers_tests.rs"]
mod tests;
