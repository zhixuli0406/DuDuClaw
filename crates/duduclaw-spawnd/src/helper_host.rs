//! HX4 root side of in-run helper calls on Linux (D8 revision §2.2, §2.4, §2.5
//! [F4, F5, F6, F14]). One [`HelperEngine`] per Running run, owned by the
//! lifecycle thread (`host::Core`), never by the control loop.
//!
//! Authority is root state only:
//! - the helper is the run-table position of the helper `Core` re-derived
//!   from the run's own employee; the slot must equal the signed binding's,
//!   and (security fix round, finding 1) the freshly derived runtime kind,
//!   profile and binding digest must equal the ones the run's table was built
//!   from, so a policy reload that reorders or withdraws helpers can never
//!   make a call run a different helper at a reused index;
//! - the gateway's input memfd is revalidated (tmpfs, seals
//!   `SEAL|SHRINK|GROW|WRITE`, exact size, CLOEXEC) right before it is sent;
//! - before `Start` the slot must be empty (`cgroup.events populated 0`) and
//!   the binding's `memory.max`/`pids.max`/`cpu.max` are written and read back;
//! - [F5] the outcome is decided here: spawnd's own watchdog fires at
//!   `effective + WATCHDOG_GRACE_MS` and writes the slot's `cgroup.kill` from
//!   outside the run (a SIGSTOPped launcher cannot extend a helper); OOM comes
//!   from the slot's `memory.events oom_kill` delta; after EVERY call, whatever
//!   the outcome, the slot is killed and drained (`populated 0`) and closed
//!   again before the end is reported. A slot that is not confirmed drained
//!   (or whose OOM counter is unreadable) never reports the launcher's
//!   outcome: the call ends `StartFailed` and the run's helpers close with
//!   `helper_cgroup_unavailable` (finding 5). Launcher frames are hints: only
//!   the exit status itself (which only the launcher can observe) is taken;
//! - [F14] launcher frames are received with `MSG_CMSG_CLOEXEC` and a
//!   minimal control buffer; any fd/cmsg, `MSG_TRUNC`/`MSG_CTRUNC`, unknown
//!   call, illegal frame, more than 64 frames/s or EOF closes the channel, both
//!   slots are killed and every later call is `helper_unavailable`;
//! - [F4] `rt-runtime` `populated 0` after it was populated (or the launcher's
//!   `Draining`) reports `Draining`; `Start` is refused from then on, and the
//!   run's MCP connection is revoked (finding 4: no runtime descendant or
//!   helper-reachable channel keeps MCP after the runtime exited).
use std::{os::fd::{AsFd, AsRawFd, OwnedFd}, sync::Arc};
use duduclaw_isolation::{egress::ProfileId, execution::{RunHandle, RuntimeKind}, ids::Sha256Digest,
    ipc::{ErrorCode, HelperCallId, HelperOutcome, HelperSlot}};
use duduclaw_run::helper_wire::{self, End, LauncherError, SpawndChannel, SpawndEvent, Start, WireError};
use crate::{Result, admission::{effective_timeout_ms, HelperEvent, HelperSignal, WATCHDOG_GRACE_MS},
    audit::{HelperCloseReason, HelperEndReason}, channels::ConnectionRevoker,
    helper_cgroup::{self, CgroupTree, Slot, SlotLimits, RT_RUNTIME}};

/// Launcher frames read per service turn; the channel's own sliding window
/// (`helper_wire::MAX_FRAMES_PER_SECOND`) closes a flooding launcher.
const FRAMES_PER_TURN: usize = 16;
/// `kill_and_wait` polls of `cgroup.events` (with a 10 ms settle: ≤ 1 s).
const DRAIN_ATTEMPTS: u32 = 100;
const SIGKILL: u32 = 9;

/// One helper of the run's launcher table, in table (position) order, with
/// the identity it was built from (finding 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedHelper {
    pub index: u8,
    pub slot: HelperSlot,
    pub limits: SlotLimits,
    pub timeout_ms: u64,
    pub runtime: RuntimeKind,
    pub profile: ProfileId,
    /// `helper_policy::binding_digest` of the signed binding at run start.
    pub binding_digest: Sha256Digest,
}
/// Moved out of the trampoline once the run is Running: spawnd's end of the
/// per-run control socketpair, the root-held unit cgroup tree, and the table.
pub struct HelperLink<T> { pub control: OwnedFd, pub tree: T, pub helpers: Vec<LinkedHelper> }

/// What `Core` derived for one call (never copied from the request except the
/// tool's own timeout bound and the size the memfd must have).
#[derive(Debug, Clone)]
pub struct CallPlan {
    pub call: HelperCallId,
    /// Signed index in the parent's helper list (`RootPolicy::derive_helper`).
    pub index: u8,
    pub slot: HelperSlot,
    /// Identity of the freshly derived entry; must equal the run's table entry.
    pub runtime: RuntimeKind,
    pub profile: ProfileId,
    pub binding_digest: Sha256Digest,
    pub requested_ms: u64,
    pub input_bytes: u64,
    pub run_remaining_ms: u64,
    pub termination_grace_ms: u64,
}
pub struct Started { pub effective_timeout_ms: u64, pub stdout: OwnedFd, pub stderr: OwnedFd }

/// Content-free audit facts for `Core` (run, helper index, slot, sizes, outcome).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperAudit {
    Started { call: HelperCallId, index: u8, slot: HelperSlot, input_bytes: u64, effective_timeout_ms: u64 },
    /// `reason`: watchdog vs launcher timeout, start-failure cause (7c/d);
    /// `drained`: the slot was confirmed empty after the kill.
    Ended { call: HelperCallId, index: u8, slot: HelperSlot, input_bytes: u64, outcome: HelperOutcome,
        reason: HelperEndReason, drained: bool },
    /// The channel closed: the code later calls get and why (7e).
    Closed { code: ErrorCode, reason: HelperCloseReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason { Launcher(End), Watchdog, Cancelled, ChannelLost, RunEnded }
#[derive(Debug, Clone, Copy)]
struct Live { call: HelperCallId, index: u8, input_bytes: u64, watchdog_at: u64, oom_before: u64, started: bool }

fn position(slot: HelperSlot) -> usize { match slot { HelperSlot::Foreground => 0, HelperSlot::Recording => 1 } }
fn cgroup_slot(slot: HelperSlot) -> Slot { match slot { HelperSlot::Foreground => Slot::Foreground, HelperSlot::Recording => Slot::Recording } }
const SLOTS: [HelperSlot; 2] = [HelperSlot::Foreground, HelperSlot::Recording];

fn end_reason(reason: Reason) -> HelperEndReason {
    match reason {
        Reason::Launcher(End::Exited(_) | End::Signalled(_)) => HelperEndReason::LauncherExit,
        Reason::Launcher(End::TimedOut) => HelperEndReason::LauncherTimeout,
        Reason::Launcher(End::StartFailed(cause)) => match cause {
            LauncherError::SlotBusy => HelperEndReason::StartFailedSlotBusy,
            LauncherError::Draining => HelperEndReason::StartFailedDraining,
            LauncherError::Descriptors => HelperEndReason::StartFailedDescriptors,
            LauncherError::Sandbox => HelperEndReason::StartFailedSandbox,
            LauncherError::Exec => HelperEndReason::StartFailedExec,
            LauncherError::Protocol | LauncherError::Internal => HelperEndReason::StartFailedInternal,
        },
        Reason::Watchdog => HelperEndReason::WatchdogKill,
        Reason::Cancelled => HelperEndReason::Cancelled,
        Reason::ChannelLost => HelperEndReason::ChannelLost,
        Reason::RunEnded => HelperEndReason::RunEnded,
    }
}
fn close_reason(error: WireError) -> HelperCloseReason {
    match error {
        WireError::Truncated => HelperCloseReason::Truncated,
        WireError::ControlTruncated => HelperCloseReason::ControlTruncated,
        WireError::Ancillary => HelperCloseReason::Ancillary,
        WireError::Descriptors => HelperCloseReason::Descriptors,
        WireError::Length | WireError::Magic | WireError::Kind | WireError::Field(_) => HelperCloseReason::Malformed,
        WireError::UnknownCall => HelperCloseReason::UnknownCall,
        WireError::IllegalState => HelperCloseReason::IllegalState,
        WireError::Rate => HelperCloseReason::Rate,
        WireError::Launcher(_) => HelperCloseReason::LauncherFatal,
        WireError::Closed => HelperCloseReason::Closed,
    }
}

pub struct HelperEngine<T: CgroupTree> {
    run: RunHandle,
    link: HelperLink<T>,
    channel: SpawndChannel,
    live: [Option<Live>; 2],
    events: Vec<HelperEvent>,
    audit: Vec<HelperAudit>,
    runtime_seen: bool,
    draining: bool,
    closed: bool,
    shutting_down: bool,
    /// The run's MCP connection, revoked when the runtime exits (finding 4).
    mcp_revoker: Option<Arc<ConnectionRevoker>>,
    mcp_revoke_failed: bool,
    settle: fn(),
}
fn settle_10ms() { std::thread::sleep(std::time::Duration::from_millis(10)); }

impl<T: CgroupTree> HelperEngine<T> {
    pub fn new(run: RunHandle, link: HelperLink<T>) -> Result<Self> {
        let table = link.helpers.iter().map(|helper| helper.slot).collect();
        let channel = SpawndChannel::new(table).map_err(|_| ErrorCode::HelperUnavailable)?;
        Ok(Self { run, link, channel, live: [None, None], events: Vec::new(), audit: Vec::new(),
            runtime_seen: false, draining: false, closed: false, shutting_down: false,
            mcp_revoker: None, mcp_revoke_failed: false, settle: settle_10ms })
    }
    /// The run's MCP revoker (a clone of the one `Core::revoke` uses).
    pub fn with_mcp_revoker(mut self, revoker: Option<Arc<ConnectionRevoker>>) -> Self { self.mcp_revoker = revoker; self }
    #[cfg(test)]
    fn with_settle(mut self, settle: fn()) -> Self { self.settle = settle; self }
    pub fn is_closed(&self) -> bool { self.closed }
    pub fn is_draining(&self) -> bool { self.draining }
    /// The MCP revocation on drain failed: `Core` fences the whole run.
    pub fn mcp_revoke_failed(&self) -> bool { self.mcp_revoke_failed }
    pub fn take_events(&mut self) -> Vec<HelperEvent> { std::mem::take(&mut self.events) }
    pub fn take_audit(&mut self) -> Vec<HelperAudit> { std::mem::take(&mut self.audit) }
    /// Run-table position and binding of a signed index; `None` = not in this run's table.
    fn table(&self, index: u8) -> Option<(u8, LinkedHelper)> {
        let at = self.link.helpers.iter().position(|helper| helper.index == index)?;
        Some((u8::try_from(at).ok()?, self.link.helpers.get(at)?.clone()))
    }

    /// Validate and send one `Start` with `[input memfd, stdout w, stderr w]`.
    /// Returns the effective timeout and the two read ends for the gateway.
    pub fn start(&mut self, now_ms: u64, plan: CallPlan, input: OwnedFd) -> Result<Started> {
        if self.closed { return Err(ErrorCode::HelperUnavailable); }
        if self.draining { return Err(ErrorCode::NotReady); }
        let (at, helper) = self.table(plan.index).ok_or(ErrorCode::HelperUnavailable)?;
        // Finding 1: the current policy's entry must be the one this run's
        // launcher table (and its exec fds) were built from.
        if helper.runtime != plan.runtime || helper.profile != plan.profile || helper.binding_digest != plan.binding_digest {
            return Err(ErrorCode::RuntimeDenied);
        }
        if helper.slot != plan.slot { return Err(ErrorCode::InvalidRequest); }
        // The gateway checked its own memfd; root checks it again right before use.
        crate::fds::validate_sealed(input.as_fd(), plan.input_bytes).map_err(|_| ErrorCode::InvalidFds)?;
        let effective = effective_timeout_ms(plan.requested_ms, helper.timeout_ms, plan.run_remaining_ms, plan.termination_grace_ms)?;
        if self.live[position(plan.slot)].is_some() || self.channel.slot_busy(plan.slot) { return Err(ErrorCode::HelperBusy); }
        let slot = cgroup_slot(plan.slot);
        // §2.4: refuse Start while the slot still holds processes; then the binding's limits.
        match self.slot_empty(slot) {
            Ok(true) => {}
            Ok(false) => return Err(ErrorCode::HelperBusy),
            Err(code) => return Err(self.fail_cgroups(code)),
        }
        if helper_cgroup::write_slot_limits(&mut self.link.tree, slot, helper.limits).is_err() {
            return Err(self.fail_cgroups(ErrorCode::HelperCgroupUnavailable));
        }
        let oom_before = match self.oom_kills(slot) { Ok(count) => count, Err(code) => return Err(self.fail_cgroups(code)) };
        let pipe = || nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(|_| ErrorCode::Internal);
        let (stdout, stdout_w) = pipe()?;
        let (stderr, stderr_w) = pipe()?;
        let frame = self.channel.start(Start { call: plan.call, helper: at, slot: plan.slot, timeout_ms: effective })
            .map_err(|error| if error == WireError::Closed { ErrorCode::HelperUnavailable } else { ErrorCode::Internal })?;
        let bytes = frame.encode().map_err(|_| ErrorCode::Internal)?;
        if helper_wire::linux::send(self.link.control.as_fd(), &bytes, &[input.as_fd(), stdout_w.as_fd(), stderr_w.as_fd()]).is_err() {
            // The call is recorded in the table; closing the channel ends it.
            self.live[position(plan.slot)] = Some(Live { call: plan.call, index: plan.index, input_bytes: plan.input_bytes,
                watchdog_at: now_ms, oom_before, started: false });
            self.close_channel(HelperCloseReason::Send);
            return Err(ErrorCode::HelperUnavailable);
        }
        // spawnd's copies close here; only the launcher (and the helper) hold them now.
        drop((input, stdout_w, stderr_w));
        let watchdog_at = now_ms.saturating_add(effective).saturating_add(WATCHDOG_GRACE_MS);
        self.live[position(plan.slot)] = Some(Live { call: plan.call, index: plan.index, input_bytes: plan.input_bytes,
            watchdog_at, oom_before, started: false });
        self.audit.push(HelperAudit::Started { call: plan.call, index: plan.index, slot: plan.slot,
            input_bytes: plan.input_bytes, effective_timeout_ms: effective });
        Ok(Started { effective_timeout_ms: effective, stdout, stderr })
    }

    fn live_slot(&self, call: HelperCallId) -> Option<HelperSlot> {
        SLOTS.into_iter().find(|slot| self.live[position(*slot)].is_some_and(|live| live.call == call))
    }
    /// [F8] Per-call only. `Cancel` sends the launcher's kill hint and then
    /// kills the slot cgroup as root; `Stop` (recording) asks for a flush. An
    /// ended call is a no-op.
    pub fn signal(&mut self, call: HelperCallId, signal: HelperSignal) -> Result<()> {
        let Some(slot) = self.live_slot(call) else { return Ok(()) };
        match signal {
            HelperSignal::Stop => {
                if self.closed { return Err(ErrorCode::HelperUnavailable); }
                match self.channel.stop(call) {
                    Ok(Some(frame)) => self.send(frame),
                    Ok(None) => Ok(()),
                    Err(WireError::IllegalState) => Err(ErrorCode::InvalidRequest),
                    Err(_) => Err(ErrorCode::HelperUnavailable),
                }
            }
            HelperSignal::Cancel => {
                if !self.closed {
                    if let Ok(Some(frame)) = self.channel.kill(call) { let _ = self.send(frame); }
                }
                self.finish(slot, Reason::Cancelled);
                Ok(())
            }
        }
    }
    fn send(&mut self, frame: helper_wire::ToLauncher) -> Result<()> {
        let bytes = frame.encode().map_err(|_| ErrorCode::Internal)?;
        if helper_wire::linux::send(self.link.control.as_fd(), &bytes, &[]).is_err() {
            self.close_channel(HelperCloseReason::Send);
            return Err(ErrorCode::HelperUnavailable);
        }
        Ok(())
    }

    /// One lifecycle turn: launcher frames, the watchdog, the runtime cgroup.
    pub fn service(&mut self, now_ms: u64) {
        if !self.closed { self.receive(now_ms); }
        for slot in SLOTS {
            if self.live[position(slot)].is_some_and(|live| now_ms >= live.watchdog_at) {
                let call = self.live[position(slot)].map(|live| live.call);
                if let (false, Some(call)) = (self.closed, call) {
                    if let Ok(Some(frame)) = self.channel.kill(call) { let _ = self.send(frame); }
                }
                self.finish(slot, Reason::Watchdog);
            }
        }
        if !self.draining && !self.shutting_down {
            match self.link.tree.read(&format!("{RT_RUNTIME}/cgroup.events")).ok()
                .and_then(|events| crate::systemd::cgroup_empty(&events).ok()) {
                Some(false) => self.runtime_seen = true,
                Some(true) if self.runtime_seen => self.begin_draining(),
                _ => {}
            }
        }
    }
    fn receive(&mut self, now_ms: u64) {
        for _ in 0..FRAMES_PER_TURN {
            match helper_wire::linux::recv(self.link.control.as_fd(), 0, true) {
                // EOF: the launcher died (not fatal to the run, §2.5).
                Ok(None) => { self.close_channel(HelperCloseReason::Eof); return; }
                Ok(Some((bytes, fds, receipt))) => {
                    // [F14] any delivered fd is closed at once, whatever the frame.
                    drop(fds);
                    match self.channel.receive(now_ms, &bytes, receipt) {
                        Ok(SpawndEvent::Started { call, slot }) => {
                            if let Some(live) = self.live[position(slot)].as_mut().filter(|live| live.call == call) { live.started = true; }
                        }
                        Ok(SpawndEvent::Ended { call, slot, end }) => {
                            if self.live[position(slot)].is_some_and(|live| live.call == call) { self.finish(slot, Reason::Launcher(end)); }
                        }
                        Ok(SpawndEvent::Draining) => self.begin_draining(),
                        Ok(SpawndEvent::Late { .. }) => {}
                        Err(error) => { self.close_channel(close_reason(error)); return; }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => { self.close_channel(HelperCloseReason::Receive); return; }
            }
        }
    }
    fn begin_draining(&mut self) {
        if self.draining { return; }
        self.draining = true;
        // Finding 4: channels outside Landlock (abstract unix sockets, SysV
        // IPC) cannot relay MCP once the runtime is gone.
        if let Some(revoker) = &self.mcp_revoker { self.mcp_revoke_failed |= revoker.revoke().is_err(); }
        self.events.push(HelperEvent::Draining { run: self.run });
    }

    /// End one call root-side: kill + drain the slot, OOM from `memory.events`,
    /// close the slot again, then report. Exactly once per call.
    fn finish(&mut self, slot: HelperSlot, reason: Reason) {
        let Some(live) = self.live[position(slot)].take() else { return };
        let cgroup = cgroup_slot(slot);
        let drained = helper_cgroup::kill_and_wait(&mut self.link.tree, cgroup.path(), DRAIN_ATTEMPTS, self.settle).is_ok();
        let oom = self.oom_kills(cgroup).ok();
        let outcome = if reason == Reason::RunEnded { HelperOutcome::RunEnded }
            // Finding 5: never report the launcher's outcome for a slot that may
            // still hold helper processes, or whose OOM counter is unknown.
            else if !drained || oom.is_none() { HelperOutcome::StartFailed }
            else if oom.is_some_and(|after| after > live.oom_before) { HelperOutcome::OomKilled } else {
                match reason {
                    Reason::Launcher(End::Exited(code)) => HelperOutcome::Exited(i32::from(code)),
                    Reason::Launcher(End::Signalled(signal)) => HelperOutcome::Signalled(u32::from(signal)),
                    Reason::Launcher(End::TimedOut) | Reason::Watchdog => HelperOutcome::TimedOut,
                    Reason::Launcher(End::StartFailed(_)) => HelperOutcome::StartFailed,
                    Reason::Cancelled | Reason::ChannelLost if live.started => HelperOutcome::Signalled(SIGKILL),
                    Reason::Cancelled | Reason::ChannelLost | Reason::RunEnded => HelperOutcome::StartFailed,
                }
            };
        // Ended root-side before the launcher said so: free its table slot and
        // accept its late frames in order (too many ⇒ unresponsive ⇒ closed).
        let abandoned = if matches!(reason, Reason::Launcher(_)) || self.closed { Ok(()) }
            else { self.channel.abandon(live.call).map(|_| ()) };
        let closed_again = helper_cgroup::write_slot_limits(&mut self.link.tree, cgroup, SlotLimits::CLOSED).is_ok();
        self.events.push(HelperEvent::Ended { run: self.run, call: live.call, outcome });
        self.audit.push(HelperAudit::Ended { call: live.call, index: live.index, slot, input_bytes: live.input_bytes, outcome,
            reason: end_reason(reason), drained });
        if let Err(error) = abandoned {
            self.close_channel(if error == WireError::Rate { HelperCloseReason::LateFrames } else { close_reason(error) });
        }
        if !(drained && closed_again && oom.is_some()) && !self.shutting_down { self.fail_cgroups(ErrorCode::HelperCgroupUnavailable); }
    }
    /// The launcher is gone or misbehaved: close, kill both slots, refuse later calls.
    fn close_channel(&mut self, reason: HelperCloseReason) {
        if self.closed { return; }
        self.closed = true;
        self.channel.close();
        // The launcher sees EOF and exits once idle (HX3b).
        unsafe { libc::shutdown(self.link.control.as_raw_fd(), libc::SHUT_RDWR) };
        self.kill_both_slots();
        if !self.shutting_down {
            self.events.push(HelperEvent::Closed { run: self.run, code: ErrorCode::HelperUnavailable });
            self.audit.push(HelperAudit::Closed { code: ErrorCode::HelperUnavailable, reason });
        }
    }
    /// Slot cgroups failed a write/readback: no more helpers in this run.
    fn fail_cgroups(&mut self, code: ErrorCode) -> ErrorCode {
        let code = if code == ErrorCode::HelperBusy { code } else { ErrorCode::HelperCgroupUnavailable };
        if code == ErrorCode::HelperCgroupUnavailable && !self.closed {
            self.closed = true;
            self.channel.close();
            unsafe { libc::shutdown(self.link.control.as_raw_fd(), libc::SHUT_RDWR) };
            self.kill_both_slots();
            self.events.push(HelperEvent::Closed { run: self.run, code });
            self.audit.push(HelperAudit::Closed { code, reason: HelperCloseReason::Cgroup });
        }
        code
    }
    /// Live calls end root-side; idle slots are killed too (a helper the
    /// launcher forked but never reported is still in its slot cgroup).
    fn kill_both_slots(&mut self) {
        for slot in SLOTS {
            if self.live[position(slot)].is_some() { self.finish(slot, Reason::ChannelLost); }
            else { let _ = helper_cgroup::kill_and_wait(&mut self.link.tree, cgroup_slot(slot).path(), DRAIN_ATTEMPTS, self.settle); }
        }
    }
    fn slot_empty(&mut self, slot: Slot) -> Result<bool> {
        let events = self.link.tree.read(&format!("{}/cgroup.events", slot.path())).map_err(|_| ErrorCode::HelperCgroupUnavailable)?;
        crate::systemd::cgroup_empty(&events).map_err(|_| ErrorCode::HelperCgroupUnavailable)
    }
    fn oom_kills(&mut self, slot: Slot) -> Result<u64> {
        let events = self.link.tree.read(&format!("{}/memory.events", slot.path())).map_err(|_| ErrorCode::HelperCgroupUnavailable)?;
        helper_cgroup::oom_kills(&events).map_err(|_| ErrorCode::HelperCgroupUnavailable)
    }
    /// Run cleanup (before the trampoline is closed): every live call ends
    /// `RunEnded`, both slots are killed best-effort (the unit stop kills them
    /// anyway), the channel closes. No `Closed` event: the run is over.
    pub fn shutdown(&mut self) {
        self.shutting_down = true;
        for slot in SLOTS { self.finish(slot, Reason::RunEnded); }
        if !self.closed {
            self.closed = true;
            self.channel.close();
            unsafe { libc::shutdown(self.link.control.as_raw_fd(), libc::SHUT_RDWR) };
        }
    }
}

#[cfg(test)]
#[path = "helper_host_tests.rs"]
mod tests;
