//! HX6 security fix round, finding 4: the per-host recording probe of
//! `host::Core`, on the lifecycle thread.
//!
//! The `helper_recording` bit is no longer one constant for every host. Once
//! the build kill switch (`host_state::RECORDING_BUILD_ENABLED`) is open, the
//! first Running run whose signed helper table holds the recording binding
//! (`duduclaw-browser-record-v1`, derived from that run's own employee like
//! every call) and that has at least `PROBE_TIMEOUT_MS` + its termination
//! grace left gets ONE root-initiated call in its rec slot: the real launcher
//! layer (F_L, launcher Landlock), the helper child (slot cgroup, helper
//! Landlock, F_H) and the helper's nested browser sandbox (Landlock + seccomp
//! + subreaper), with the fixed input `ipc::RECORDING_PROBE_REQUEST`. The
//! helper renders a fixed local page (no request, no network, no artifacts)
//! and must answer exactly `ipc::recording_probe_reply(<call>)`, exit 0, with
//! empty stderr. The verdict is recorded with the boot id and the selected
//! Landlock ABI (`RecordingProbeEvidence`) and published in the readiness
//! report; `EvidenceReport::helper_recording` requires it.
//!
//! The probe borrows that employee's run (D8 revision §6/T5): its rec slot
//! (busy for up to 60 s), slot memory/tasks and its egress forwarder. Start
//! and verdict are audited with the run and employee ids
//! (`recording_probe_started` / `recording_probe_verdict`).
//!
//! Fail closed: no binding, no Landlock evidence, too little run time left, a
//! start refusal or a run that ends under the probe leave the probe unrun
//! (another run may try; each run at most once). A timeout, OOM kill or
//! signal is inconclusive (host load, not necessarily Chromium): it is retried
//! with backoff in another employee's run, at most `MAX_INCONCLUSIVE` attempts
//! in all, then the probe fails. A clean exit with the wrong or oversized
//! output or any stderr, exit 78 (`helper_recording_unsupported`), any other
//! exit or a start failure is a failed probe for this spawnd process. Nothing
//! here is reachable from IPC: the gateway can neither start nor answer the probe.
use super::*;
use std::io::Read;
use duduclaw_isolation::ipc::HelperOutcome;
use crate::audit::ProbeVerdict;
use crate::host_state::{RecordingProbeEvidence, RECORDING_BUILD_ENABLED};

const PROBE_RUNTIME: &str = "duduclaw-browser-record-v1";
/// Tool bound for the probe call (spawnd clamps it to the binding and the run).
const PROBE_TIMEOUT_MS: u64 = 60_000;
/// The reply is one short JSON line; anything longer fails the probe.
const MAX_PROBE_OUTPUT: usize = 4096;
/// Round 3 (N1): inconclusive attempts (timeout/OOM/signal) before the probe
/// fails for this spawnd process.
pub(super) const MAX_INCONCLUSIVE: u8 = 3;
/// Backoff after the 1st and 2nd inconclusive attempt.
const RETRY_BACKOFF: [Duration; 2] = [Duration::from_secs(30), Duration::from_secs(120)];

/// Probe state of this spawnd process.
#[derive(Default)]
pub(super) enum RecordingProbe {
    #[default]
    Idle,
    Running(Box<ProbeCall>),
    Done(RecordingProbeEvidence),
}
pub(super) struct ProbeCall {
    run: RunHandle,
    employee: EmployeeId,
    call: HelperCallId,
    attempt: u8,
    stdout: std::fs::File,
    stderr: std::fs::File,
    out: Vec<u8>,
    /// stderr produced anything, or stdout overflowed / failed.
    tainted: bool,
    boot_id: String,
    landlock_abi: u32,
}
impl RecordingProbe {
    pub(super) fn evidence(&self) -> Option<RecordingProbeEvidence> {
        match self { Self::Done(evidence) => Some(evidence.clone()), _ => None }
    }
}

/// What one probe call says about Chromium on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    Passed,
    /// Hard failure: exit 78 or any other exit, wrong output, stderr, start failure.
    Failed,
    /// Timeout, OOM kill or signal: retried (bounded) in another employee's run.
    Inconclusive,
    /// The run ended under the probe: not counted, another run may try.
    RunEnded,
}
/// The probe's verdict from root's outcome and the bounded output.
pub(super) fn verdict(outcome: HelperOutcome, stdout: &[u8], tainted: bool, call: HelperCallId) -> Verdict {
    match outcome {
        HelperOutcome::RunEnded => Verdict::RunEnded,
        HelperOutcome::TimedOut | HelperOutcome::OomKilled | HelperOutcome::Signalled(_) => Verdict::Inconclusive,
        HelperOutcome::Exited(0) if !tainted && stdout == duduclaw_isolation::ipc::recording_probe_reply(&call.hex()).as_bytes() => Verdict::Passed,
        HelperOutcome::Exited(_) | HelperOutcome::StartFailed => Verdict::Failed,
    }
}

/// Round 3 (N1, N5): which runs the probe may use next. Pure, so the rules
/// are testable without root.
#[derive(Default)]
pub(super) struct ProbeSchedule {
    /// Runs the probe was started in (each at most once); cleared on Done.
    tried_runs: BTreeSet<RunId>,
    /// Employees whose run gave an inconclusive result: retries go elsewhere.
    inconclusive_employees: BTreeSet<EmployeeId>,
    inconclusive: u8,
    not_before: Option<Instant>,
}
impl ProbeSchedule {
    /// Remaining run time the probe needs so its timeout is never clamped by
    /// the run deadline and the slot can be drained within the grace.
    pub(super) fn required_ms(termination_grace_ms: u64) -> u64 { PROBE_TIMEOUT_MS.saturating_add(termination_grace_ms) }
    pub(super) fn eligible(&self, run: RunId, employee: EmployeeId, remaining_ms: u64, termination_grace_ms: u64, now: Instant) -> bool {
        !self.tried_runs.contains(&run) && !self.inconclusive_employees.contains(&employee)
            && self.not_before.is_none_or(|not_before| now >= not_before)
            && remaining_ms >= Self::required_ms(termination_grace_ms)
    }
    pub(super) fn tried(&mut self, run: RunId) { self.tried_runs.insert(run); }
    /// Drop runs that no longer exist (the set stays bounded by live runs).
    pub(super) fn retain_runs(&mut self, live: impl Fn(&RunId) -> bool) { self.tried_runs.retain(|run| live(run)); }
    /// This attempt's number for the audit (counted inconclusive attempts + 1).
    pub(super) fn attempt(&self) -> u8 { self.inconclusive.saturating_add(1) }
    /// Record one ended call: the audit verdict and, when final, `Some(passed)`.
    pub(super) fn record(&mut self, verdict: Verdict, employee: EmployeeId, now: Instant) -> (ProbeVerdict, Option<bool>) {
        let decided = match verdict {
            Verdict::Passed => (ProbeVerdict::Passed, Some(true)),
            Verdict::Failed => (ProbeVerdict::Failed, Some(false)),
            Verdict::RunEnded => (ProbeVerdict::RunEnded, None),
            Verdict::Inconclusive => {
                self.inconclusive = self.inconclusive.saturating_add(1);
                if self.inconclusive >= MAX_INCONCLUSIVE { (ProbeVerdict::Exhausted, Some(false)) } else {
                    self.inconclusive_employees.insert(employee);
                    let backoff = RETRY_BACKOFF.get(usize::from(self.inconclusive.saturating_sub(1)))
                        .or(RETRY_BACKOFF.last()).copied().unwrap_or_default();
                    self.not_before = Some(now.checked_add(backoff).unwrap_or(now));
                    (ProbeVerdict::Inconclusive, None)
                }
            }
        };
        // N5: a decided probe keeps no run or employee state.
        if decided.1.is_some() { *self = Self::default(); }
        decided
    }
}

/// Non-blocking bounded read of whatever is in `file`; `Err` = overflow or I/O error.
fn drain(file: &mut std::fs::File, sink: &mut Vec<u8>, limit: usize) -> std::result::Result<(), ()> {
    let mut buffer = [0u8; 1024];
    for _ in 0..64 {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                let chunk = buffer.get(..read).ok_or(())?;
                if sink.len().saturating_add(chunk.len()) > limit { return Err(()); }
                sink.extend_from_slice(chunk);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(()),
        }
    }
    Ok(())
}
/// Read both pipes (bounded); anything on stderr, an overflow or an I/O error taints the probe.
fn read_output(probe: &mut ProbeCall) {
    if drain(&mut probe.stdout, &mut probe.out, MAX_PROBE_OUTPUT).is_err() { probe.tainted = true; }
    let mut noise = Vec::new();
    if drain(&mut probe.stderr, &mut noise, MAX_PROBE_OUTPUT).is_err() || !noise.is_empty() { probe.tainted = true; }
}
fn nonblocking(fd: OwnedFd) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    // SAFETY: flag query/update of a descriptor this function owns.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(ErrorCode::Internal);
    }
    Ok(std::fs::File::from(fd))
}
fn remaining_ms(deadline: Instant) -> u64 {
    u64::try_from(deadline.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(u64::MAX)
}

impl Core {
    /// N2: the probe's start/verdict through the spawnd audit worker.
    fn audit_probe(&self, event: Event, probe: &ProbeCall, outcome: Option<HelperOutcome>, verdict: Option<ProbeVerdict>) {
        emit_detail(&self.audit, event, Some(probe.employee), Some(probe.run.0), Some(Detail::RecordingProbe {
            call: probe.call, attempt: probe.attempt, landlock_abi: probe.landlock_abi, outcome, verdict }));
    }
    /// Every lifecycle turn: start the probe in an eligible run while none
    /// ran yet, and read a running probe's output.
    pub(super) fn service_recording_probe(&mut self) {
        if !RECORDING_BUILD_ENABLED { return; }
        match std::mem::take(&mut self.recording_probe) {
            done @ RecordingProbe::Done(_) => self.recording_probe = done,
            RecordingProbe::Running(mut call) => {
                let live = self.runs.get(&call.run.0)
                    .is_some_and(|entry| !entry.closed && matches!(entry.helper, Some(RunHelper::Engine(_))));
                // The run went away without an end event: inconclusive, not counted (stays Idle).
                if !live {
                    let (verdict, _) = self.probe_schedule.record(Verdict::RunEnded, call.employee, Instant::now());
                    self.audit_probe(Event::RecordingProbeVerdict, &call, None, Some(verdict));
                    return;
                }
                read_output(&mut call);
                self.recording_probe = RecordingProbe::Running(call);
            }
            RecordingProbe::Idle => {
                let runs = &self.runs;
                self.probe_schedule.retain_runs(|run| runs.contains_key(run));
                let now = Instant::now();
                let candidates: Vec<RunId> = self.runs.iter()
                    .filter(|(run, entry)| !entry.closed && entry.started_event
                        && matches!(entry.helper, Some(RunHelper::Engine(_)))
                        && entry.helper_parent.as_ref().is_some_and(|parent| self.probe_schedule.eligible(**run, entry.employee,
                            remaining_ms(entry.runtime_deadline), parent.timeouts.termination_grace_ms, now)))
                    .map(|(run, _)| *run).collect();
                for run in candidates {
                    // Each run is tried at most once (its table may lack the binding).
                    self.probe_schedule.tried(run);
                    if let Ok(call) = self.start_recording_probe(RunHandle(run)) {
                        self.audit_probe(Event::RecordingProbeStarted, &call, None, None);
                        self.recording_probe = RecordingProbe::Running(Box::new(call));
                        return;
                    }
                }
            }
        }
    }

    /// One root-initiated recording call in `run`'s rec slot (see module docs).
    fn start_recording_probe(&mut self, run: RunHandle) -> Result<ProbeCall> {
        let landlock_abi = self.report.landlock.as_ref().map(|landlock| landlock.selected_abi).ok_or(ErrorCode::HelperUnavailable)?;
        let boot_id = self.report.boot_id.clone();
        let entry = self.runs.get(&run.0).ok_or(ErrorCode::NotFound)?;
        let cancelled = self.monitor.cancellation(run).map_err(|_| ErrorCode::NotReady)?;
        if entry.closed || !entry.started_event || cancelled.load(Ordering::Acquire) { return Err(ErrorCode::NotReady); }
        let parent = entry.helper_parent.as_ref().ok_or(ErrorCode::HelperUnavailable)?;
        let policy = self.policy.as_ref().ok_or(ErrorCode::HelperUnavailable)?;
        let kind = RuntimeKind::Registered(duduclaw_isolation::bounded::Text::new(PROBE_RUNTIME.into()).map_err(|_| ErrorCode::Internal)?);
        // Derived from the run's own employee, exactly like a gateway call.
        let derived = policy.derive_helper(entry.employee, parent, &kind)?;
        if derived.binding.slot != duduclaw_isolation::ipc::HelperSlot::Recording { return Err(ErrorCode::InvalidRequest); }
        let binding_digest = crate::helper_policy::binding_digest(derived.binding).ok_or(ErrorCode::RuntimeDenied)?;
        let remaining = remaining_ms(entry.runtime_deadline);
        // N1: never a probe the run deadline would clamp (a false timeout).
        if remaining < ProbeSchedule::required_ms(parent.timeouts.termination_grace_ms) { return Err(ErrorCode::NotReady); }
        let call = HelperCallId::new(os(crate::server::random_id())?).map_err(|_| ErrorCode::Internal)?;
        let request = duduclaw_isolation::ipc::RECORDING_PROBE_REQUEST;
        let input = os(crate::fds::sealed_plan(request))?;
        let input_bytes = u64::try_from(request.len()).map_err(|_| ErrorCode::Internal)?;
        let plan = CallPlan { call, index: derived.index, slot: derived.binding.slot, runtime: derived.runtime.runtime.clone(),
            profile: derived.binding.profile, binding_digest, requested_ms: PROBE_TIMEOUT_MS, input_bytes,
            run_remaining_ms: remaining, termination_grace_ms: parent.timeouts.termination_grace_ms };
        let employee = entry.employee;
        let attempt = self.probe_schedule.attempt();
        let now = os(crate::server::coordination_time_ms())?;
        let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        let Some(RunHelper::Engine(engine)) = entry.helper.as_mut() else { return Err(ErrorCode::HelperUnavailable) };
        let started = engine.start(now, plan, input);
        for fact in engine.take_audit() { emit_helper(&self.audit, employee, run.0, fact); }
        let started = started?;
        Ok(ProbeCall { run, employee, call, attempt, stdout: nonblocking(started.stdout)?, stderr: nonblocking(started.stderr)?,
            out: Vec::new(), tainted: false, boot_id, landlock_abi })
    }

    /// Root's end of the probe call (from the engines' events): read what is
    /// left (the slot is drained, so the pipes are at EOF) and decide.
    pub(super) fn observe_recording_probe(&mut self, events: &[HelperEvent]) {
        if !matches!(self.recording_probe, RecordingProbe::Running(_)) { return; }
        let RecordingProbe::Running(mut probe) = std::mem::take(&mut self.recording_probe) else { return };
        let ended = events.iter().find_map(|event| match event {
            HelperEvent::Ended { run, call, outcome } if *run == probe.run && *call == probe.call => Some(*outcome),
            _ => None,
        });
        let Some(outcome) = ended else { self.recording_probe = RecordingProbe::Running(probe); return };
        read_output(&mut probe);
        let (audited, decided) = self.probe_schedule.record(verdict(outcome, &probe.out, probe.tainted, probe.call),
            probe.employee, Instant::now());
        self.audit_probe(Event::RecordingProbeVerdict, &probe, Some(outcome), Some(audited));
        self.recording_probe = match decided {
            None => RecordingProbe::Idle,
            Some(passed) => RecordingProbe::Done(RecordingProbeEvidence { boot_id: probe.boot_id.clone(), landlock_abi: probe.landlock_abi, passed }),
        };
        // Publish the verdict with the next readiness refresh.
        self.last_refresh = Instant::now() - Duration::from_secs(10);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hx6fix_recording_probe_verdict_is_exact_and_fails_closed() {
        let call = HelperCallId::new([7; 16]).unwrap();
        let reply = duduclaw_isolation::ipc::recording_probe_reply(&call.hex());
        assert_eq!(verdict(HelperOutcome::Exited(0), reply.as_bytes(), false, call), Verdict::Passed);
        // Wrong call id, extra bytes, stderr/overflow, a recording result: failed.
        let other = duduclaw_isolation::ipc::recording_probe_reply(&HelperCallId::new([8; 16]).unwrap().hex());
        let trailing = format!("{reply}\n");
        for (bytes, tainted) in [(other.as_bytes(), false), (trailing.as_bytes(), false),
            (reply.as_bytes(), true), (br#"{"probe":"rendered"}"#.as_slice(), false), (b"".as_slice(), false)] {
            assert_eq!(verdict(HelperOutcome::Exited(0), bytes, tainted, call), Verdict::Failed);
        }
        // Chromium-side exits and start failures fail the probe (round 3:
        // timeout/OOM/signal are inconclusive, see `hx6r3_*`); the run ending under it is not counted.
        for outcome in [HelperOutcome::Exited(78), HelperOutcome::Exited(75), HelperOutcome::StartFailed] {
            assert_eq!(verdict(outcome, reply.as_bytes(), false, call), Verdict::Failed, "{outcome:?}");
        }
        assert_eq!(verdict(HelperOutcome::RunEnded, reply.as_bytes(), false, call), Verdict::RunEnded);
        assert!(RecordingProbe::default().evidence().is_none());
    }
    /// Round 3 (N1): timeout, OOM and signal are inconclusive whatever the
    /// output; exit 78 and a wrong reply stay hard failures.
    #[test]
    fn hx6r3_probe_timeout_oom_signal_are_inconclusive() {
        let call = HelperCallId::new([7; 16]).unwrap();
        let reply = duduclaw_isolation::ipc::recording_probe_reply(&call.hex());
        for outcome in [HelperOutcome::TimedOut, HelperOutcome::OomKilled, HelperOutcome::Signalled(9), HelperOutcome::Signalled(31)] {
            for (bytes, tainted) in [(reply.as_bytes(), false), (b"".as_slice(), true)] {
                assert_eq!(verdict(outcome, bytes, tainted, call), Verdict::Inconclusive, "{outcome:?}");
            }
        }
        assert_eq!(verdict(HelperOutcome::Exited(78), b"", false, call), Verdict::Failed);
        assert_eq!(verdict(HelperOutcome::Exited(0), b"{}", false, call), Verdict::Failed);
    }
    /// Round 3 (N1): only runs with the probe timeout + termination grace
    /// left; a tried run is never reused.
    #[test]
    fn hx6r3_probe_needs_remaining_time_for_timeout_and_grace() {
        let schedule = ProbeSchedule::default();
        let now = Instant::now();
        let (run, employee) = (RunId([1; 16]), EmployeeId([2; 16]));
        let grace = 12_000;
        assert_eq!(ProbeSchedule::required_ms(grace), PROBE_TIMEOUT_MS + grace);
        assert!(!schedule.eligible(run, employee, PROBE_TIMEOUT_MS + grace - 1, grace, now));
        assert!(!schedule.eligible(run, employee, PROBE_TIMEOUT_MS, grace, now));
        assert!(schedule.eligible(run, employee, PROBE_TIMEOUT_MS + grace, grace, now));
        assert!(!schedule.eligible(run, employee, u64::MAX - 1, u64::MAX, now), "an absurd grace never overflows into eligible");
        let mut schedule = schedule;
        schedule.tried(run);
        assert!(!schedule.eligible(run, employee, u64::MAX, grace, now));
        schedule.retain_runs(|_| false);
        assert!(schedule.eligible(run, employee, u64::MAX, grace, now), "a forgotten run id leaves the set");
    }
    /// Round 3 (N1, N5): inconclusive results retry in other employees' runs
    /// with backoff, at most `MAX_INCONCLUSIVE` attempts; hard failures and
    /// passes are final at once; a decided probe keeps no tried-run state.
    #[test]
    fn hx6r3_probe_retries_inconclusive_across_employees_with_backoff() {
        let grace = 12_000;
        let enough = PROBE_TIMEOUT_MS + grace;
        let employees = [EmployeeId([1; 16]), EmployeeId([2; 16]), EmployeeId([3; 16]), EmployeeId([4; 16])];
        let runs = [RunId([11; 16]), RunId([12; 16]), RunId([13; 16]), RunId([14; 16])];
        let mut schedule = ProbeSchedule::default();
        let start = Instant::now();
        // The run ending under the probe is not counted and blocks no employee.
        schedule.tried(runs[0]);
        assert_eq!(schedule.record(Verdict::RunEnded, employees[0], start), (ProbeVerdict::RunEnded, None));
        assert_eq!(schedule.attempt(), 1);
        assert!(schedule.eligible(runs[1], employees[0], enough, grace, start));
        // 1st inconclusive: that employee is out, everyone waits the first backoff.
        schedule.tried(runs[1]);
        assert_eq!(schedule.record(Verdict::Inconclusive, employees[0], start), (ProbeVerdict::Inconclusive, None));
        assert_eq!(schedule.attempt(), 2);
        assert!(!schedule.eligible(runs[2], employees[1], enough, grace, start));
        assert!(!schedule.eligible(runs[2], employees[1], enough, grace, start + RETRY_BACKOFF[0] - Duration::from_millis(1)));
        let later = start + RETRY_BACKOFF[0];
        assert!(schedule.eligible(runs[2], employees[1], enough, grace, later));
        assert!(!schedule.eligible(runs[3], employees[0], enough, grace, later), "retries go to another employee");
        // 2nd inconclusive: a longer backoff.
        schedule.tried(runs[2]);
        assert_eq!(schedule.record(Verdict::Inconclusive, employees[1], later), (ProbeVerdict::Inconclusive, None));
        assert!(!schedule.eligible(runs[3], employees[2], enough, grace, later + RETRY_BACKOFF[0]));
        let last = later + RETRY_BACKOFF[1];
        assert!(schedule.eligible(runs[3], employees[2], enough, grace, last));
        assert!(!schedule.eligible(runs[3], employees[1], enough, grace, last));
        // 3rd inconclusive: exhausted, final failure, and the state is cleared (N5).
        schedule.tried(runs[3]);
        assert_eq!(schedule.attempt(), MAX_INCONCLUSIVE);
        assert_eq!(schedule.record(Verdict::Inconclusive, employees[2], last), (ProbeVerdict::Exhausted, Some(false)));
        assert!(schedule.tried_runs.is_empty() && schedule.inconclusive_employees.is_empty() && schedule.not_before.is_none());
        // Hard failure (exit 78 / wrong output) and a pass are final on the first attempt.
        for (verdict, expected) in [(Verdict::Failed, (ProbeVerdict::Failed, Some(false))), (Verdict::Passed, (ProbeVerdict::Passed, Some(true)))] {
            let mut schedule = ProbeSchedule::default();
            schedule.tried(runs[0]);
            assert_eq!(schedule.record(verdict, employees[3], start), expected);
            assert!(schedule.tried_runs.is_empty(), "N5: Done clears the tried runs");
        }
    }
}
