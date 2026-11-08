//! HX6 security fix round, finding 4: the per-host recording probe of
//! `host::Core`, on the lifecycle thread.
//!
//! The `helper_recording` bit is no longer one constant for every host. Once
//! the build kill switch (`host_state::RECORDING_BUILD_ENABLED`) is open, the
//! first Running run whose signed helper table holds the recording binding
//! (`duduclaw-browser-record-v1`, derived from that run's own employee like
//! every call) gets ONE root-initiated call in its rec slot: the real launcher
//! layer (F_L, launcher Landlock), the helper child (slot cgroup, helper
//! Landlock, F_H) and the helper's nested browser sandbox (Landlock + seccomp
//! + subreaper), with the fixed input `ipc::RECORDING_PROBE_REQUEST`. The
//! helper renders a fixed local page (no request, no network, no artifacts)
//! and must answer exactly `ipc::recording_probe_reply(<call>)`, exit 0, with
//! empty stderr. The verdict is recorded with the boot id and the selected
//! Landlock ABI (`RecordingProbeEvidence`) and published in the readiness
//! report; `EvidenceReport::helper_recording` requires it.
//!
//! Fail closed: no binding, no Landlock evidence, a start refusal or a run
//! that ends under the probe leave the probe unrun (another run may try; each
//! run at most once); any Chromium-side failure (non-zero exit, timeout, OOM,
//! signal, start failure, wrong or oversized output, stderr) is a failed
//! probe for this spawnd process. Nothing here is reachable from IPC: the
//! gateway can neither start nor answer the probe.
use super::*;
use std::io::Read;
use duduclaw_isolation::ipc::HelperOutcome;
use crate::host_state::{RecordingProbeEvidence, RECORDING_BUILD_ENABLED};

const PROBE_RUNTIME: &str = "duduclaw-browser-record-v1";
/// Tool bound for the probe call (spawnd clamps it to the binding and the run).
const PROBE_TIMEOUT_MS: u64 = 60_000;
/// The reply is one short JSON line; anything longer fails the probe.
const MAX_PROBE_OUTPUT: usize = 4096;

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
    call: HelperCallId,
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

/// The probe's verdict from root's outcome and the bounded output. `None`:
/// inconclusive (the run ended under the probe), try again in another run.
pub(super) fn verdict(outcome: HelperOutcome, stdout: &[u8], tainted: bool, call: HelperCallId) -> Option<bool> {
    match outcome {
        HelperOutcome::RunEnded => None,
        HelperOutcome::Exited(0) => Some(!tainted && stdout == duduclaw_isolation::ipc::recording_probe_reply(&call.hex()).as_bytes()),
        _ => Some(false),
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

impl Core {
    /// Every lifecycle turn: start the probe in an eligible run while none
    /// ran yet, and read a running probe's output.
    pub(super) fn service_recording_probe(&mut self) {
        if !RECORDING_BUILD_ENABLED { return; }
        match std::mem::take(&mut self.recording_probe) {
            done @ RecordingProbe::Done(_) => self.recording_probe = done,
            RecordingProbe::Running(mut call) => {
                let live = self.runs.get(&call.run.0)
                    .is_some_and(|entry| !entry.closed && matches!(entry.helper, Some(RunHelper::Engine(_))));
                // The run went away without an end event: inconclusive (stays Idle).
                if !live { return; }
                read_output(&mut call);
                self.recording_probe = RecordingProbe::Running(call);
            }
            RecordingProbe::Idle => {
                let candidates: Vec<RunId> = self.runs.iter()
                    .filter(|(run, entry)| !entry.closed && entry.started_event && !self.probe_tried.contains(*run)
                        && matches!(entry.helper, Some(RunHelper::Engine(_))))
                    .map(|(run, _)| *run).collect();
                for run in candidates {
                    // Each run is tried at most once (its table may lack the binding).
                    self.probe_tried.insert(run);
                    if let Ok(call) = self.start_recording_probe(RunHandle(run)) {
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
        let remaining = u64::try_from(entry.runtime_deadline.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(u64::MAX);
        let call = HelperCallId::new(os(crate::server::random_id())?).map_err(|_| ErrorCode::Internal)?;
        let request = duduclaw_isolation::ipc::RECORDING_PROBE_REQUEST;
        let input = os(crate::fds::sealed_plan(request))?;
        let input_bytes = u64::try_from(request.len()).map_err(|_| ErrorCode::Internal)?;
        let plan = CallPlan { call, index: derived.index, slot: derived.binding.slot, runtime: derived.runtime.runtime.clone(),
            profile: derived.binding.profile, binding_digest, requested_ms: PROBE_TIMEOUT_MS, input_bytes,
            run_remaining_ms: remaining, termination_grace_ms: parent.timeouts.termination_grace_ms };
        let employee = entry.employee;
        let now = os(crate::server::coordination_time_ms())?;
        let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        let Some(RunHelper::Engine(engine)) = entry.helper.as_mut() else { return Err(ErrorCode::HelperUnavailable) };
        let started = engine.start(now, plan, input);
        for fact in engine.take_audit() { emit_helper(&self.audit, employee, run.0, fact); }
        let started = started?;
        Ok(ProbeCall { run, call, stdout: nonblocking(started.stdout)?, stderr: nonblocking(started.stderr)?, out: Vec::new(),
            tainted: false, boot_id, landlock_abi })
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
        self.recording_probe = match verdict(outcome, &probe.out, probe.tainted, probe.call) {
            None => RecordingProbe::Idle,
            Some(passed) => {
                eprintln!("duduclaw-spawnd: recording probe {} (landlock_abi={})", if passed { "passed" } else { "failed" }, probe.landlock_abi);
                RecordingProbe::Done(RecordingProbeEvidence { boot_id: probe.boot_id.clone(), landlock_abi: probe.landlock_abi, passed })
            }
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
        assert_eq!(verdict(HelperOutcome::Exited(0), reply.as_bytes(), false, call), Some(true));
        // Wrong call id, extra bytes, stderr/overflow, a recording result: failed.
        let other = duduclaw_isolation::ipc::recording_probe_reply(&HelperCallId::new([8; 16]).unwrap().hex());
        let trailing = format!("{reply}\n");
        for (bytes, tainted) in [(other.as_bytes(), false), (trailing.as_bytes(), false),
            (reply.as_bytes(), true), (br#"{"probe":"rendered"}"#.as_slice(), false), (b"".as_slice(), false)] {
            assert_eq!(verdict(HelperOutcome::Exited(0), bytes, tainted, call), Some(false));
        }
        // Chromium-side failures fail the probe; the run ending under it is inconclusive.
        for outcome in [HelperOutcome::Exited(78), HelperOutcome::Exited(75), HelperOutcome::TimedOut, HelperOutcome::OomKilled,
            HelperOutcome::Signalled(9), HelperOutcome::StartFailed] {
            assert_eq!(verdict(outcome, reply.as_bytes(), false, call), Some(false), "{outcome:?}");
        }
        assert_eq!(verdict(HelperOutcome::RunEnded, reply.as_bytes(), false, call), None);
        assert!(RecordingProbe::default().evidence().is_none());
    }
}
