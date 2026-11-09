//! HX launcher ↔ spawnd control channel (D8 revision §2.2 [F5, F7, F14]).
//! One `SOCK_SEQPACKET` socketpair per run; one packet = one frame. The codec
//! and both state tables are pure and host-testable; `linux` holds the only
//! socket I/O. Any error from a receive path means "close the channel".
//!
//! Frame = `DCH1` magic (4) ‖ kind (1) ‖ fixed body, big-endian, exact length
//! per kind, ≤ `MAX_FRAME_BYTES`. No names, paths, argv or env ever cross: the
//! helper is an index into the root-signed `LauncherPlan` table.
//!
//! | kind | dir | body | fds |
//! |---|---|---|---|
//! | 0x01 `Start` | spawnd → launcher | call[16] helper:u8 slot:u8 timeout_ms:u64 | exactly 3: input memfd, stdout w, stderr w |
//! | 0x02 `Stop` | spawnd → launcher | call[16] (recording slot only) | 0 |
//! | 0x03 `Kill` | spawnd → launcher | call[16] | 0 |
//! | 0x81 `Started` | launcher → spawnd | call[16] | 0 |
//! | 0x82 `Ended` | launcher → spawnd | call[16] kind:u8 code:i32 | 0 |
//! | 0x83 `Draining` | launcher → spawnd | — | 0 |
//! | 0x84 `Error` | launcher → spawnd | code:u16 | 0 |
//!
//! Launcher → spawnd frames are advisory hints [F5]: spawnd never takes
//! timeout, OOM or exit from them. They never carry fds or other ancillary
//! data [F14]; spawnd receives with a one-fd control buffer and
//! `MSG_CMSG_CLOEXEC`, closes anything that arrives, and closes the channel on
//! any cmsg, `MSG_CTRUNC`, `MSG_TRUNC`, unknown call, illegal state or more
//! than `MAX_FRAMES_PER_SECOND`.
use std::collections::BTreeSet;
use duduclaw_isolation::ipc::{HelperCallId, MAX_HELPER_TIMEOUT_MS};
use crate::helper_policy::{HelperSlot, MAX_HELPER_BINDINGS};

pub const MAGIC: [u8; 4] = *b"DCH1";
/// Hard frame bound; every defined frame is far smaller, and a receiver's
/// buffer of this size turns anything longer into `MSG_TRUNC`.
pub const MAX_FRAME_BYTES: usize = 4096;
/// `Start` carries exactly these, in this order: input memfd, stdout write end, stderr write end.
pub const START_FDS: usize = 3;
/// Receive bound on fds per frame (launcher side): one more than `START_FDS`,
/// so a surplus fd is delivered, counted and closed rather than silently dropped.
pub const MAX_FRAME_FDS: usize = 4;
/// spawnd closes the channel when the launcher sends more frames than this in any 1 s window.
pub const MAX_FRAMES_PER_SECOND: usize = 64;
/// Distinct call ids per run before the channel refuses further `Start`s.
pub const MAX_CALLS_PER_RUN: usize = 65_536;
/// Calls spawnd ended root-side whose launcher frames may still arrive.
pub const MAX_ABANDONED_CALLS: usize = 8;

const KIND_START: u8 = 0x01;
const KIND_STOP: u8 = 0x02;
const KIND_KILL: u8 = 0x03;
const KIND_STARTED: u8 = 0x81;
const KIND_ENDED: u8 = 0x82;
const KIND_DRAINING: u8 = 0x83;
const KIND_ERROR: u8 = 0x84;
const HEADER: usize = 5;
const START_LEN: usize = HEADER + 16 + 1 + 1 + 8;
const CALL_LEN: usize = HEADER + 16;
const ENDED_LEN: usize = HEADER + 16 + 1 + 4;
const DRAINING_LEN: usize = HEADER;
const ERROR_LEN: usize = HEADER + 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("helper frame truncated (MSG_TRUNC)")] Truncated,
    #[error("helper frame control data truncated (MSG_CTRUNC)")] ControlTruncated,
    #[error("helper frame carried unexpected ancillary data")] Ancillary,
    #[error("helper frame carried the wrong number of descriptors")] Descriptors,
    #[error("helper frame has an invalid length")] Length,
    #[error("helper frame has a bad magic")] Magic,
    #[error("helper frame kind is unknown or for the other direction")] Kind,
    #[error("helper frame field is invalid: {0}")] Field(&'static str),
    #[error("helper frame names an unknown call")] UnknownCall,
    #[error("helper frame is illegal in the call's state")] IllegalState,
    #[error("helper control channel rate exceeded")] Rate,
    #[error("helper launcher reported a fatal error")] Launcher(LauncherError),
    #[error("helper control channel is closed")] Closed,
}

/// Ancillary facts of one `recvmsg`, produced by `linux::recv` (or a test).
/// `fds` counts SCM_RIGHTS descriptors the receiver now owns and must close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Receipt { pub fds: usize, pub foreign_cmsgs: usize, pub truncated: bool, pub control_truncated: bool }
impl Receipt {
    pub const fn plain() -> Self { Self { fds: 0, foreign_cmsgs: 0, truncated: false, control_truncated: false } }
    pub const fn with_fds(fds: usize) -> Self { Self { fds, foreign_cmsgs: 0, truncated: false, control_truncated: false } }
    fn check(self, expected_fds: Option<usize>) -> Result<(), WireError> {
        if self.truncated { return Err(WireError::Truncated); }
        if self.control_truncated { return Err(WireError::ControlTruncated); }
        if self.foreign_cmsgs != 0 { return Err(WireError::Ancillary); }
        match expected_fds {
            // Launcher frames: any fd at all is ancillary data, never a miscount [F14].
            None if self.fds != 0 => Err(WireError::Ancillary),
            Some(expected) if self.fds != expected => Err(WireError::Descriptors),
            _ => Ok(()),
        }
    }
}

/// Fatal launcher conditions (`Error` frame) and `StartFailed` reasons (`Ended` code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum LauncherError {
    /// spawnd sent a malformed or illegal frame; the launcher closes the channel.
    Protocol = 1,
    /// The slot still holds a helper (or its group/cgroup is not yet empty).
    SlotBusy = 2,
    /// The launcher received SIGTERM and refuses every `Start` (§2.5 (b)).
    Draining = 3,
    /// Start fds failed identity/seal checks, or the exec fd failed `verify_executable`.
    Descriptors = 4,
    /// Fork, cgroup move, rlimits, Landlock or F_H could not be applied.
    Sandbox = 5,
    /// `execveat` failed.
    Exec = 6,
    Internal = 7,
}
impl LauncherError {
    pub const ALL: [Self; 7] = [Self::Protocol, Self::SlotBusy, Self::Draining, Self::Descriptors,
        Self::Sandbox, Self::Exec, Self::Internal];
    pub const fn code(self) -> u16 { self as u16 }
    pub fn from_code(code: u16) -> Option<Self> { Self::ALL.into_iter().find(|e| e.code() == code) }
}

fn encode_slot(slot: HelperSlot) -> u8 { match slot { HelperSlot::Foreground => 0, HelperSlot::Recording => 1 } }
fn decode_slot(byte: u8) -> Result<HelperSlot, WireError> {
    match byte { 0 => Ok(HelperSlot::Foreground), 1 => Ok(HelperSlot::Recording), _ => Err(WireError::Field("slot")) }
}
fn slot_index(slot: HelperSlot) -> usize { encode_slot(slot) as usize }

/// spawnd → launcher `Start`. `timeout_ms` is the effective timeout spawnd
/// computed (§2.5); the launcher arms its timerfd with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Start { pub call: HelperCallId, pub helper: u8, pub slot: HelperSlot, pub timeout_ms: u64 }
impl Start {
    fn validate(&self) -> Result<(), WireError> {
        if usize::from(self.helper) >= MAX_HELPER_BINDINGS { return Err(WireError::Field("helper")); }
        if self.timeout_ms == 0 || self.timeout_ms > MAX_HELPER_TIMEOUT_MS { return Err(WireError::Field("timeout")); }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToLauncher { Start(Start), Stop { call: HelperCallId }, Kill { call: HelperCallId } }

/// Launcher-observed end of a call. A hint only: spawnd decides the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Exited(u8),
    /// Signal number 1..=64.
    Signalled(u8),
    /// The launcher's timerfd fired and it killed the process group.
    TimedOut,
    /// No helper ran (refused `Start`, sandbox or exec failure).
    StartFailed(LauncherError),
}
impl End {
    fn encode(self) -> (u8, i32) {
        match self {
            Self::Exited(code) => (0, i32::from(code)),
            Self::Signalled(signal) => (1, i32::from(signal)),
            Self::TimedOut => (2, 0),
            Self::StartFailed(reason) => (3, i32::from(reason.code())),
        }
    }
    fn decode(kind: u8, code: i32) -> Result<Self, WireError> {
        match (kind, code) {
            (0, 0..=255) => Ok(Self::Exited(code as u8)),
            (1, 1..=64) => Ok(Self::Signalled(code as u8)),
            (2, 0) => Ok(Self::TimedOut),
            (3, _) => u16::try_from(code).ok().and_then(LauncherError::from_code)
                .filter(|reason| *reason != LauncherError::Protocol)
                .map(Self::StartFailed).ok_or(WireError::Field("start failure")),
            _ => Err(WireError::Field("end")),
        }
    }
    fn validate(self) -> Result<(), WireError> {
        let (kind, code) = self.encode();
        Self::decode(kind, code).map(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromLauncher {
    Started { call: HelperCallId },
    Ended { call: HelperCallId, end: End },
    Draining,
    /// Fatal: the launcher is closing the channel. Per-call failures use `Ended(StartFailed)`.
    Error { code: LauncherError },
}

fn header(kind: u8, len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    bytes.extend_from_slice(&MAGIC);
    bytes.push(kind);
    bytes
}
fn finish(bytes: Vec<u8>, len: usize) -> Vec<u8> { debug_assert_eq!(bytes.len(), len); bytes }

impl ToLauncher {
    /// Exact fd count accompanying the frame.
    pub const fn fd_count(&self) -> usize { match self { Self::Start(_) => START_FDS, _ => 0 } }
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        Ok(match self {
            Self::Start(start) => {
                start.validate()?;
                let mut bytes = header(KIND_START, START_LEN);
                bytes.extend_from_slice(&start.call.bytes());
                bytes.push(start.helper);
                bytes.push(encode_slot(start.slot));
                bytes.extend_from_slice(&start.timeout_ms.to_be_bytes());
                finish(bytes, START_LEN)
            }
            Self::Stop { call } | Self::Kill { call } => {
                let mut bytes = header(if matches!(self, Self::Stop { .. }) { KIND_STOP } else { KIND_KILL }, CALL_LEN);
                bytes.extend_from_slice(&call.bytes());
                finish(bytes, CALL_LEN)
            }
        })
    }
}
impl FromLauncher {
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        Ok(match self {
            Self::Started { call } => {
                let mut bytes = header(KIND_STARTED, CALL_LEN);
                bytes.extend_from_slice(&call.bytes());
                finish(bytes, CALL_LEN)
            }
            Self::Ended { call, end } => {
                end.validate()?;
                let (kind, code) = end.encode();
                let mut bytes = header(KIND_ENDED, ENDED_LEN);
                bytes.extend_from_slice(&call.bytes());
                bytes.push(kind);
                bytes.extend_from_slice(&code.to_be_bytes());
                finish(bytes, ENDED_LEN)
            }
            Self::Draining => header(KIND_DRAINING, DRAINING_LEN),
            Self::Error { code } => {
                let mut bytes = header(KIND_ERROR, ERROR_LEN);
                bytes.extend_from_slice(&code.code().to_be_bytes());
                finish(bytes, ERROR_LEN)
            }
        })
    }
}

/// Magic, bounded length and kind; the exact per-kind length is checked by the caller.
fn kind_of(bytes: &[u8]) -> Result<u8, WireError> {
    if bytes.len() < HEADER || bytes.len() > MAX_FRAME_BYTES { return Err(WireError::Length); }
    if bytes[..4] != MAGIC { return Err(WireError::Magic); }
    Ok(bytes[4])
}
fn exact(bytes: &[u8], len: usize) -> Result<(), WireError> {
    if bytes.len() == len { Ok(()) } else { Err(WireError::Length) }
}
fn call_at(bytes: &[u8], offset: usize) -> Result<HelperCallId, WireError> {
    let raw: [u8; 16] = bytes[offset..offset + 16].try_into().map_err(|_| WireError::Length)?;
    HelperCallId::new(raw).map_err(|_| WireError::Field("call"))
}

/// Launcher side. `receipt` describes the ancillary data that came with the
/// packet; `Start` must carry exactly `START_FDS`, every other frame none.
/// On error the caller closes the received fds and the channel.
pub fn decode_to_launcher(bytes: &[u8], receipt: Receipt) -> Result<ToLauncher, WireError> {
    receipt.check(Some(receipt.fds))?; // truncation and foreign cmsgs first
    let message = match kind_of(bytes)? {
        KIND_START => {
            exact(bytes, START_LEN)?;
            let start = Start { call: call_at(bytes, HEADER)?, helper: bytes[HEADER + 16], slot: decode_slot(bytes[HEADER + 17])?,
                timeout_ms: u64::from_be_bytes(bytes[HEADER + 18..START_LEN].try_into().map_err(|_| WireError::Length)?) };
            start.validate()?;
            ToLauncher::Start(start)
        }
        KIND_STOP => { exact(bytes, CALL_LEN)?; ToLauncher::Stop { call: call_at(bytes, HEADER)? } }
        KIND_KILL => { exact(bytes, CALL_LEN)?; ToLauncher::Kill { call: call_at(bytes, HEADER)? } }
        _ => return Err(WireError::Kind),
    };
    receipt.check(Some(message.fd_count()))?;
    Ok(message)
}

/// spawnd side [F14]: launcher frames never carry fds or other cmsgs.
pub fn decode_from_launcher(bytes: &[u8], receipt: Receipt) -> Result<FromLauncher, WireError> {
    receipt.check(None)?;
    Ok(match kind_of(bytes)? {
        KIND_STARTED => { exact(bytes, CALL_LEN)?; FromLauncher::Started { call: call_at(bytes, HEADER)? } }
        KIND_ENDED => {
            exact(bytes, ENDED_LEN)?;
            let code = i32::from_be_bytes(bytes[HEADER + 17..ENDED_LEN].try_into().map_err(|_| WireError::Length)?);
            FromLauncher::Ended { call: call_at(bytes, HEADER)?, end: End::decode(bytes[HEADER + 16], code)? }
        }
        KIND_DRAINING => { exact(bytes, DRAINING_LEN)?; FromLauncher::Draining }
        KIND_ERROR => {
            exact(bytes, ERROR_LEN)?;
            let code = u16::from_be_bytes([bytes[HEADER], bytes[HEADER + 1]]);
            FromLauncher::Error { code: LauncherError::from_code(code).ok_or(WireError::Field("error code"))? }
        }
        _ => return Err(WireError::Kind),
    })
}

/// Sliding-window limiter: more than `MAX_FRAMES_PER_SECOND` frames within any
/// 1000 ms closes the channel. Time is spawnd's monotonic clock in ms.
#[derive(Debug, Clone)]
struct FrameRate { stamps: [u64; MAX_FRAMES_PER_SECOND], next: usize, len: usize }
impl FrameRate {
    fn new() -> Self { Self { stamps: [0; MAX_FRAMES_PER_SECOND], next: 0, len: 0 } }
    fn admit(&mut self, now_ms: u64) -> Result<(), WireError> {
        if self.len == MAX_FRAMES_PER_SECOND {
            let oldest = self.stamps[self.next];
            if now_ms < oldest.saturating_add(1000) { return Err(WireError::Rate); }
        } else { self.len += 1; }
        self.stamps[self.next] = now_ms;
        self.next = (self.next + 1) % MAX_FRAMES_PER_SECOND;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase { Starting, Running }
#[derive(Debug, Clone, Copy)]
struct Live { call: HelperCallId, phase: Phase, stop_sent: bool, kill_sent: bool }

/// What spawnd learns from one valid launcher frame (all advisory [F5]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawndEvent {
    Started { call: HelperCallId, slot: HelperSlot },
    Ended { call: HelperCallId, slot: HelperSlot, end: End },
    Draining,
    /// A frame for a call spawnd already finished root-side; nothing to do.
    Late { call: HelperCallId },
}

/// spawnd's per-run state table. `receive` errors are fatal (the table is
/// closed and spawnd must close the socket; later calls are
/// `helper_unavailable`). `start`/`stop`/`kill`/`abandon` errors refuse only
/// that local action.
///
/// Per call: (none) —Start→ Starting —Started→ Running —Ended→ (none);
/// Starting —Ended(StartFailed)→ (none). Anything else from the launcher —
/// Started twice, Ended(exit) before Started, StartFailed after Started, an id
/// never started, a second Draining — closes the channel.
#[derive(Debug, Clone)]
pub struct SpawndChannel {
    table: Vec<HelperSlot>,
    live: [Option<Live>; 2],
    abandoned: Vec<(HelperCallId, Phase)>,
    used: BTreeSet<HelperCallId>,
    draining: bool,
    closed: bool,
    rate: FrameRate,
}
impl SpawndChannel {
    /// `table[i]` = signed slot of helper index `i` (`LauncherPlan.helpers[i].slot`).
    pub fn new(table: Vec<HelperSlot>) -> Result<Self, WireError> {
        if table.is_empty() || table.len() > MAX_HELPER_BINDINGS { return Err(WireError::Field("helper table")); }
        Ok(Self { table, live: [None, None], abandoned: Vec::new(), used: BTreeSet::new(), draining: false,
            closed: false, rate: FrameRate::new() })
    }
    pub fn is_closed(&self) -> bool { self.closed }
    pub fn is_draining(&self) -> bool { self.draining }
    pub fn slot_busy(&self, slot: HelperSlot) -> bool { self.live[slot_index(slot)].is_some() }
    /// Control-socket EOF or any local failure: refuse everything from now on.
    pub fn close(&mut self) { self.closed = true; }

    /// Check and record a `Start` before sending it with its 3 fds. `call` must
    /// be fresh (spawnd's random id, never reused in the run).
    pub fn start(&mut self, start: Start) -> Result<ToLauncher, WireError> {
        if self.closed { return Err(WireError::Closed); }
        start.validate()?;
        if self.draining { return Err(WireError::IllegalState); }
        if self.table.get(usize::from(start.helper)) != Some(&start.slot) { return Err(WireError::Field("helper")); }
        if self.slot_busy(start.slot) { return Err(WireError::IllegalState); }
        if self.used.contains(&start.call) { return Err(WireError::Field("call")); }
        if self.used.len() >= MAX_CALLS_PER_RUN { return Err(WireError::IllegalState); }
        self.used.insert(start.call);
        self.live[slot_index(start.slot)] = Some(Live { call: start.call, phase: Phase::Starting, stop_sent: false, kill_sent: false });
        Ok(ToLauncher::Start(start))
    }
    fn find(&mut self, call: HelperCallId) -> Option<(HelperSlot, &mut Live)> {
        let [fg, rec] = &mut self.live;
        [(HelperSlot::Foreground, fg), (HelperSlot::Recording, rec)].into_iter()
            .find_map(|(slot, live)| live.as_mut().filter(|live| live.call == call).map(|live| (slot, live)))
    }
    /// Recording only (`stop\n` to the helper). `Ok(None)`: already sent.
    pub fn stop(&mut self, call: HelperCallId) -> Result<Option<ToLauncher>, WireError> {
        if self.closed { return Err(WireError::Closed); }
        let (slot, live) = self.find(call).ok_or(WireError::UnknownCall)?;
        if slot != HelperSlot::Recording { return Err(WireError::IllegalState); }
        if live.stop_sent || live.kill_sent { return Ok(None); }
        live.stop_sent = true;
        Ok(Some(ToLauncher::Stop { call }))
    }
    /// Fast-path kill hint; spawnd's root `cgroup.kill` is the enforcement. `Ok(None)`: already sent.
    pub fn kill(&mut self, call: HelperCallId) -> Result<Option<ToLauncher>, WireError> {
        if self.closed { return Err(WireError::Closed); }
        let (_, live) = self.find(call).ok_or(WireError::UnknownCall)?;
        if live.kill_sent { return Ok(None); }
        live.kill_sent = true;
        Ok(Some(ToLauncher::Kill { call }))
    }
    /// spawnd ended the call root-side (slot `cgroup.kill` + `populated 0`)
    /// before the launcher's `Ended`: free the slot and accept the launcher's
    /// late frames for this call without effect. Too many outstanding
    /// abandoned calls means an unresponsive launcher: the channel closes.
    pub fn abandon(&mut self, call: HelperCallId) -> Result<HelperSlot, WireError> {
        if self.closed { return Err(WireError::Closed); }
        let (slot, live) = self.find(call).ok_or(WireError::UnknownCall)?;
        let phase = live.phase;
        if self.abandoned.len() >= MAX_ABANDONED_CALLS { self.closed = true; return Err(WireError::Rate); }
        self.live[slot_index(slot)] = None;
        self.abandoned.push((call, phase));
        Ok(slot)
    }

    /// Validate one received launcher frame. Every frame, valid or not, counts
    /// against the rate limit. Any `Err` closes the table.
    pub fn receive(&mut self, now_ms: u64, bytes: &[u8], receipt: Receipt) -> Result<SpawndEvent, WireError> {
        if self.closed { return Err(WireError::Closed); }
        let result = self.rate.admit(now_ms).and_then(|()| decode_from_launcher(bytes, receipt))
            .and_then(|frame| self.apply(frame));
        if result.is_err() { self.closed = true; }
        result
    }
    fn apply(&mut self, frame: FromLauncher) -> Result<SpawndEvent, WireError> {
        match frame {
            FromLauncher::Draining => {
                if self.draining { return Err(WireError::IllegalState); }
                self.draining = true;
                Ok(SpawndEvent::Draining)
            }
            FromLauncher::Error { code } => Err(WireError::Launcher(code)),
            FromLauncher::Started { call } => {
                if let Some((slot, live)) = self.find(call) {
                    if live.phase != Phase::Starting { return Err(WireError::IllegalState); }
                    live.phase = Phase::Running;
                    return Ok(SpawndEvent::Started { call, slot });
                }
                self.late(call, Phase::Starting, Some(Phase::Running))
            }
            FromLauncher::Ended { call, end } => {
                let start_failed = matches!(end, End::StartFailed(_));
                if let Some((slot, live)) = self.find(call) {
                    let legal = if start_failed { live.phase == Phase::Starting } else { live.phase == Phase::Running };
                    if !legal { return Err(WireError::IllegalState); }
                    self.live[slot_index(slot)] = None;
                    return Ok(SpawndEvent::Ended { call, slot, end });
                }
                let expected = if start_failed { Phase::Starting } else { Phase::Running };
                self.late(call, expected, None)
            }
        }
    }
    /// A frame for an abandoned call must still follow the call's order;
    /// `Ended` retires it, `Started` advances it.
    fn late(&mut self, call: HelperCallId, expected: Phase, next: Option<Phase>) -> Result<SpawndEvent, WireError> {
        let index = self.abandoned.iter().position(|(id, _)| *id == call).ok_or(WireError::UnknownCall)?;
        if self.abandoned[index].1 != expected { return Err(WireError::IllegalState); }
        match next {
            Some(phase) => self.abandoned[index].1 = phase,
            None => { self.abandoned.swap_remove(index); }
        }
        Ok(SpawndEvent::Late { call })
    }
}

/// What the launcher must do for one valid spawnd frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Fork, sandbox and exec helper `start.helper` with the 3 received fds.
    Start(Start),
    /// Refuse without running anything: close the 3 fds and send `Ended(StartFailed(reason))`.
    Refuse { call: HelperCallId, reason: LauncherError },
    /// Write `stop\n` to the recording helper's stdin.
    Stop { call: HelperCallId, slot: HelperSlot },
    /// `kill(-pgid, SIGKILL)` the call's process group.
    Kill { call: HelperCallId, slot: HelperSlot },
    /// Stop/Kill for a call that already ended here (race with `Ended`).
    Ignore,
}

/// The launcher's per-run state table. `receive` errors are protocol
/// violations by spawnd: send `Error(Protocol)` and close.
#[derive(Debug, Clone)]
pub struct LauncherChannel {
    table: Vec<HelperSlot>,
    live: [Option<HelperCallId>; 2],
    seen: BTreeSet<HelperCallId>,
    draining: bool,
    closed: bool,
}
impl LauncherChannel {
    pub fn new(table: Vec<HelperSlot>) -> Result<Self, WireError> {
        if table.is_empty() || table.len() > MAX_HELPER_BINDINGS { return Err(WireError::Field("helper table")); }
        Ok(Self { table, live: [None, None], seen: BTreeSet::new(), draining: false, closed: false })
    }
    pub fn is_draining(&self) -> bool { self.draining }
    pub fn live(&self, slot: HelperSlot) -> Option<HelperCallId> { self.live[slot_index(slot)] }

    pub fn receive(&mut self, bytes: &[u8], receipt: Receipt) -> Result<Command, WireError> {
        if self.closed { return Err(WireError::Closed); }
        let result = decode_to_launcher(bytes, receipt).and_then(|frame| self.apply(frame));
        if result.is_err() { self.closed = true; }
        result
    }
    fn slot_of(&self, call: HelperCallId) -> Option<HelperSlot> {
        [HelperSlot::Foreground, HelperSlot::Recording].into_iter().find(|slot| self.live[slot_index(*slot)] == Some(call))
    }
    fn apply(&mut self, frame: ToLauncher) -> Result<Command, WireError> {
        match frame {
            ToLauncher::Start(start) => {
                // spawnd is root and checked all of this; a mismatch is a protocol error.
                if self.table.get(usize::from(start.helper)) != Some(&start.slot) { return Err(WireError::Field("helper")); }
                if self.seen.contains(&start.call) || self.seen.len() >= MAX_CALLS_PER_RUN { return Err(WireError::Field("call")); }
                self.seen.insert(start.call);
                // Races that are not violations: SIGTERM arrived, or the previous
                // helper of the slot is not reaped yet after spawnd's cgroup.kill.
                if self.draining { return Ok(Command::Refuse { call: start.call, reason: LauncherError::Draining }); }
                if self.live[slot_index(start.slot)].is_some() {
                    return Ok(Command::Refuse { call: start.call, reason: LauncherError::SlotBusy });
                }
                self.live[slot_index(start.slot)] = Some(start.call);
                Ok(Command::Start(start))
            }
            ToLauncher::Stop { call } | ToLauncher::Kill { call } => {
                let stop = matches!(frame, ToLauncher::Stop { .. });
                match self.slot_of(call) {
                    Some(HelperSlot::Foreground) if stop => Err(WireError::IllegalState),
                    Some(slot) => Ok(if stop { Command::Stop { call, slot } } else { Command::Kill { call, slot } }),
                    None if self.seen.contains(&call) => Ok(Command::Ignore),
                    None => Err(WireError::UnknownCall),
                }
            }
        }
    }
    /// The helper was exec'd (fork + exec succeeded).
    pub fn started(&self, call: HelperCallId) -> Result<FromLauncher, WireError> {
        self.slot_of(call).ok_or(WireError::UnknownCall)?;
        Ok(FromLauncher::Started { call })
    }
    /// The call is over (reaped, or never started): free its slot.
    pub fn ended(&mut self, call: HelperCallId, end: End) -> Result<FromLauncher, WireError> {
        end.validate()?;
        let slot = self.slot_of(call).ok_or(WireError::UnknownCall)?;
        self.live[slot_index(slot)] = None;
        Ok(FromLauncher::Ended { call, end })
    }
    /// The `Ended(StartFailed)` frame answering a `Command::Refuse` (that call never held a slot).
    pub fn refused(&self, call: HelperCallId, reason: LauncherError) -> Result<FromLauncher, WireError> {
        if !self.seen.contains(&call) || self.slot_of(call).is_some() { return Err(WireError::UnknownCall); }
        let end = End::StartFailed(reason);
        end.validate()?;
        Ok(FromLauncher::Ended { call, end })
    }
    /// After SIGTERM: refuse further `Start`s; returns the single `Draining` frame.
    pub fn begin_draining(&mut self) -> Option<FromLauncher> {
        if self.draining { return None; }
        self.draining = true;
        Some(FromLauncher::Draining)
    }
}

/// The only socket I/O. Linux `SOCK_SEQPACKET`; one call = one packet.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub mod linux {
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
    use super::{Receipt, MAX_FRAME_BYTES, MAX_FRAME_FDS};

    /// Send one frame with exactly `fds` (≤ `MAX_FRAME_FDS`) as SCM_RIGHTS.
    pub fn send(socket: BorrowedFd<'_>, frame: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        if frame.is_empty() || frame.len() > MAX_FRAME_BYTES || fds.len() > MAX_FRAME_FDS {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let raw: Vec<libc::c_int> = fds.iter().map(AsRawFd::as_raw_fd).collect();
        let payload = std::mem::size_of_val(raw.as_slice());
        // SAFETY: CMSG_SPACE is a pure size computation.
        let space = if raw.is_empty() { 0 } else { (unsafe { libc::CMSG_SPACE(payload as u32) }) as usize };
        // usize storage satisfies cmsghdr alignment.
        let mut control = vec![0usize; space.div_ceil(std::mem::size_of::<usize>())];
        let mut iov = libc::iovec { iov_base: frame.as_ptr().cast_mut().cast(), iov_len: frame.len() };
        // SAFETY: zeroed msghdr is a valid empty header; every pointer set below
        // refers to storage alive for the whole call, with matching lengths.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        if !raw.is_empty() {
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = space as _;
            // SAFETY: the control buffer holds exactly one cmsghdr of CMSG_LEN(payload).
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                if header.is_null() { return Err(io::ErrorKind::InvalidInput.into()); }
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(payload as u32) as _;
                std::ptr::copy_nonoverlapping(raw.as_ptr().cast::<u8>(), libc::CMSG_DATA(header), payload);
            }
        }
        loop {
            // SAFETY: message describes live buffers; the kernel only reads them.
            let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &message, libc::MSG_NOSIGNAL) };
            if sent < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted { continue; }
                return Err(error);
            }
            return if sent as usize == frame.len() { Ok(()) } else { Err(io::ErrorKind::WriteZero.into()) };
        }
    }

    /// Receive one packet with `MSG_CMSG_CLOEXEC`. The control buffer is
    /// `CMSG_SPACE` of `max_fds` descriptors, or of one when `max_fds` is 0
    /// (spawnd's minimal buffer; cmsg padding lets 64-bit kernels deliver two).
    /// Stray fds that fit are delivered, owned here and closed by the caller's
    /// drop; anything beyond sets `MSG_CTRUNC` and is never installed. The
    /// caller passes the `Receipt` to `decode_*`, which rejects any surplus.
    /// `Ok(None)` is EOF (or a zero-length packet: also a closed channel).
    pub fn recv(socket: BorrowedFd<'_>, max_fds: usize, nonblocking: bool)
        -> io::Result<Option<(Vec<u8>, Vec<OwnedFd>, Receipt)>> {
        let room = max_fds.clamp(1, MAX_FRAME_FDS);
        let mut bytes = vec![0u8; MAX_FRAME_BYTES];
        // SAFETY: pure size computation.
        let space = unsafe { libc::CMSG_SPACE((room * std::mem::size_of::<libc::c_int>()) as u32) } as usize;
        let mut control = vec![0usize; space.div_ceil(std::mem::size_of::<usize>())];
        let mut iov = libc::iovec { iov_base: bytes.as_mut_ptr().cast(), iov_len: bytes.len() };
        // SAFETY: as in `send`; buffers outlive the call and lengths match.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        let flags = libc::MSG_CMSG_CLOEXEC | if nonblocking { libc::MSG_DONTWAIT } else { 0 };
        let read = loop {
            // SAFETY: the kernel writes at most the described lengths.
            let read = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, flags) };
            if read >= 0 { break read as usize; }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
        };
        let mut receipt = Receipt {
            truncated: message.msg_flags & libc::MSG_TRUNC != 0,
            control_truncated: message.msg_flags & libc::MSG_CTRUNC != 0,
            ..Receipt::plain()
        };
        let mut fds = Vec::new();
        // SAFETY: msg_controllen is kernel-produced and bounded by our buffer;
        // each header is bounds-checked before its data is read.
        unsafe {
            let base = control.as_ptr() as usize;
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                let length = (*header).cmsg_len as usize;
                let offset = header as usize - base;
                let empty = libc::CMSG_LEN(0) as usize;
                if length < empty || length > (message.msg_controllen as usize).saturating_sub(offset) {
                    receipt.foreign_cmsgs += 1;
                    break;
                }
                if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(header);
                    let count = (length - empty) / std::mem::size_of::<libc::c_int>();
                    if (length - empty) % std::mem::size_of::<libc::c_int>() != 0 { receipt.foreign_cmsgs += 1; }
                    for index in 0..count {
                        let raw = std::ptr::read_unaligned(data.add(index * std::mem::size_of::<libc::c_int>()).cast::<libc::c_int>());
                        // SCM_RIGHTS installed a fresh descriptor; own it now so every path closes it.
                        if raw >= 0 { fds.push(OwnedFd::from_raw_fd(raw)); } else { receipt.foreign_cmsgs += 1; }
                    }
                } else {
                    receipt.foreign_cmsgs += 1;
                }
                header = libc::CMSG_NXTHDR(&message, header);
            }
        }
        receipt.fds = fds.len();
        if read == 0 && !receipt.truncated { return Ok(None); }
        bytes.truncate(read);
        Ok(Some((bytes, fds, receipt)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(byte: u8) -> HelperCallId { HelperCallId::new([byte; 16]).unwrap() }
    fn start(byte: u8, helper: u8, slot: HelperSlot) -> Start { Start { call: call(byte), helper, slot, timeout_ms: 30_000 } }
    fn table() -> Vec<HelperSlot> { vec![HelperSlot::Foreground, HelperSlot::Recording, HelperSlot::Foreground] }
    fn bytes(frame: FromLauncher) -> Vec<u8> { frame.encode().unwrap() }

    #[test]
    fn wire_rejects_fd_on_launcher_frame() {
        let frames = [FromLauncher::Started { call: call(1) }, FromLauncher::Ended { call: call(1), end: End::Exited(0) },
            FromLauncher::Draining, FromLauncher::Error { code: LauncherError::Internal }];
        for frame in frames {
            let encoded = bytes(frame);
            assert_eq!(decode_from_launcher(&encoded, Receipt::plain()), Ok(frame));
            for fds in [1, 2, 3, MAX_FRAME_FDS] {
                assert_eq!(decode_from_launcher(&encoded, Receipt::with_fds(fds)), Err(WireError::Ancillary), "{frame:?} {fds}");
            }
            let foreign = Receipt { foreign_cmsgs: 1, ..Receipt::plain() };
            assert_eq!(decode_from_launcher(&encoded, foreign), Err(WireError::Ancillary));
            let ctrunc = Receipt { control_truncated: true, ..Receipt::plain() };
            assert_eq!(decode_from_launcher(&encoded, ctrunc), Err(WireError::ControlTruncated));
            let trunc = Receipt { truncated: true, ..Receipt::plain() };
            assert_eq!(decode_from_launcher(&encoded, trunc), Err(WireError::Truncated));
        }
        // The state table closes the channel on the first fd-bearing frame.
        let mut channel = SpawndChannel::new(table()).unwrap();
        channel.start(start(1, 0, HelperSlot::Foreground)).unwrap();
        let started = bytes(FromLauncher::Started { call: call(1) });
        assert_eq!(channel.receive(0, &started, Receipt::with_fds(1)), Err(WireError::Ancillary));
        assert!(channel.is_closed());
        assert_eq!(channel.receive(1, &started, Receipt::plain()), Err(WireError::Closed));
        assert_eq!(channel.start(start(2, 2, HelperSlot::Foreground)).err(), Some(WireError::Closed));
    }

    #[test]
    fn wire_exact_fd_counts_and_layout() {
        let message = ToLauncher::Start(start(9, 1, HelperSlot::Recording));
        let encoded = message.encode().unwrap();
        assert_eq!(&encoded[..4], b"DCH1");
        assert_eq!(encoded.len(), START_LEN);
        assert_eq!(encoded[4], 0x01);
        assert_eq!(&encoded[5..21], &[9; 16]);
        assert_eq!((encoded[21], encoded[22]), (1, 1));
        assert_eq!(&encoded[23..31], &30_000u64.to_be_bytes());
        assert_eq!(message.fd_count(), START_FDS);
        assert_eq!(decode_to_launcher(&encoded, Receipt::with_fds(3)), Ok(message));
        for fds in [0, 1, 2, 4] {
            assert_eq!(decode_to_launcher(&encoded, Receipt::with_fds(fds)), Err(WireError::Descriptors), "{fds}");
        }
        for message in [ToLauncher::Stop { call: call(9) }, ToLauncher::Kill { call: call(9) }] {
            let encoded = message.encode().unwrap();
            assert_eq!(encoded.len(), CALL_LEN);
            assert_eq!(message.fd_count(), 0);
            assert_eq!(decode_to_launcher(&encoded, Receipt::plain()), Ok(message));
            assert_eq!(decode_to_launcher(&encoded, Receipt::with_fds(3)), Err(WireError::Descriptors));
        }
        for receipt in [Receipt { truncated: true, ..Receipt::with_fds(3) }, Receipt { control_truncated: true, ..Receipt::with_fds(3) },
            Receipt { foreign_cmsgs: 1, ..Receipt::with_fds(3) }] {
            assert!(decode_to_launcher(&encoded, receipt).is_err());
        }
        let ended = bytes(FromLauncher::Ended { call: call(2), end: End::Signalled(9) });
        assert_eq!(ended.len(), ENDED_LEN);
        assert_eq!(&ended[21..26], &[1, 0, 0, 0, 9]);
        assert_eq!(bytes(FromLauncher::Draining), b"DCH1\x83");
        assert_eq!(bytes(FromLauncher::Error { code: LauncherError::Sandbox }), b"DCH1\x84\x00\x05");
        // Every frame is far below the bound.
        assert!([START_LEN, CALL_LEN, ENDED_LEN, DRAINING_LEN, ERROR_LEN].iter().all(|len| *len < MAX_FRAME_BYTES / 64));
    }

    #[test]
    fn wire_strict_decoding() {
        let good = ToLauncher::Start(start(9, 1, HelperSlot::Recording)).encode().unwrap();
        let mutate = |index: usize, value: u8| { let mut b = good.clone(); b[index] = value; b };
        // Magic, direction, unknown kinds.
        assert_eq!(decode_to_launcher(&mutate(0, b'X'), Receipt::with_fds(3)), Err(WireError::Magic));
        assert_eq!(decode_to_launcher(&mutate(3, b'2'), Receipt::with_fds(3)), Err(WireError::Magic));
        for kind in [0x00, 0x04, 0x7f, 0x81, 0x82, 0x83, 0x84, 0xff] {
            assert_eq!(decode_to_launcher(&mutate(4, kind), Receipt::with_fds(3)), Err(WireError::Kind), "{kind:#x}");
        }
        for kind in [0x01, 0x02, 0x03, 0x80, 0x85, 0xff] {
            let mut frame = bytes(FromLauncher::Started { call: call(1) });
            frame[4] = kind;
            assert!(decode_from_launcher(&frame, Receipt::plain()).is_err(), "{kind:#x}");
        }
        // Exact length: one byte short or long, empty, header only, oversized.
        let mut long = good.clone(); long.push(0);
        for frame in [good[..good.len() - 1].to_vec(), long, vec![], b"DCH1".to_vec(), b"DCH1\x01".to_vec(),
            vec![0; MAX_FRAME_BYTES + 1]] {
            assert!(decode_to_launcher(&frame, Receipt::with_fds(3)).is_err());
        }
        // Fields: zero call id, helper index ≥ 16, unknown slot, timeout 0 or > 300 s.
        let mut zero = good.clone(); zero[5..21].fill(0);
        assert_eq!(decode_to_launcher(&zero, Receipt::with_fds(3)), Err(WireError::Field("call")));
        assert_eq!(decode_to_launcher(&mutate(21, 16), Receipt::with_fds(3)), Err(WireError::Field("helper")));
        assert_eq!(decode_to_launcher(&mutate(22, 2), Receipt::with_fds(3)), Err(WireError::Field("slot")));
        let with_timeout = |ms: u64| { let mut b = good.clone(); b[23..31].copy_from_slice(&ms.to_be_bytes()); b };
        for ms in [0, MAX_HELPER_TIMEOUT_MS + 1, u64::MAX] {
            assert_eq!(decode_to_launcher(&with_timeout(ms), Receipt::with_fds(3)), Err(WireError::Field("timeout")));
        }
        assert!(decode_to_launcher(&with_timeout(MAX_HELPER_TIMEOUT_MS), Receipt::with_fds(3)).is_ok());
        assert!(ToLauncher::Start(Start { timeout_ms: 0, ..start(1, 0, HelperSlot::Foreground) }).encode().is_err());
        assert!(ToLauncher::Start(Start { helper: 16, ..start(1, 0, HelperSlot::Foreground) }).encode().is_err());
        // Ended kinds and codes.
        let ended = |kind: u8, code: i32| {
            let mut b = bytes(FromLauncher::Ended { call: call(1), end: End::TimedOut });
            b[21] = kind; b[22..26].copy_from_slice(&code.to_be_bytes()); b
        };
        assert_eq!(decode_from_launcher(&ended(0, 255), Receipt::plain()).unwrap(),
            FromLauncher::Ended { call: call(1), end: End::Exited(255) });
        assert_eq!(decode_from_launcher(&ended(3, 6), Receipt::plain()).unwrap(),
            FromLauncher::Ended { call: call(1), end: End::StartFailed(LauncherError::Exec) });
        for (kind, code) in [(0, 256), (0, -1), (1, 0), (1, 65), (2, 1), (3, 0), (3, 1 /* Protocol */), (3, 8), (4, 0)] {
            assert!(decode_from_launcher(&ended(kind, code), Receipt::plain()).is_err(), "{kind} {code}");
        }
        assert!(FromLauncher::Ended { call: call(1), end: End::Signalled(0) }.encode().is_err());
        assert!(FromLauncher::Ended { call: call(1), end: End::StartFailed(LauncherError::Protocol) }.encode().is_err());
        for code in [0u16, 8, u16::MAX] {
            let mut frame = bytes(FromLauncher::Error { code: LauncherError::Internal });
            frame[5..7].copy_from_slice(&code.to_be_bytes());
            assert_eq!(decode_from_launcher(&frame, Receipt::plain()), Err(WireError::Field("error code")));
        }
        for error in LauncherError::ALL { assert_eq!(LauncherError::from_code(error.code()), Some(error)); }
    }

    #[test]
    fn wire_spawnd_state_table() {
        let mut channel = SpawndChannel::new(table()).unwrap();
        // Start checks: helper index in the signed table with its signed slot; one per slot; fresh ids.
        assert_eq!(channel.start(start(1, 3, HelperSlot::Foreground)).err(), Some(WireError::Field("helper")));
        assert_eq!(channel.start(start(1, 0, HelperSlot::Recording)).err(), Some(WireError::Field("helper")));
        assert!(channel.start(start(1, 0, HelperSlot::Foreground)).is_ok());
        assert_eq!(channel.start(start(2, 2, HelperSlot::Foreground)).err(), Some(WireError::IllegalState), "fg busy");
        assert_eq!(channel.start(start(1, 1, HelperSlot::Recording)).err(), Some(WireError::Field("call")), "id reuse");
        assert!(channel.start(start(3, 1, HelperSlot::Recording)).is_ok());
        assert!(!channel.is_closed(), "local refusals never close the channel");
        // Stop is recording-only and sent once; Kill once.
        assert_eq!(channel.stop(call(1)).err(), Some(WireError::IllegalState));
        assert_eq!(channel.stop(call(3)).unwrap(), Some(ToLauncher::Stop { call: call(3) }));
        assert_eq!(channel.stop(call(3)).unwrap(), None);
        assert_eq!(channel.kill(call(1)).unwrap(), Some(ToLauncher::Kill { call: call(1) }));
        assert_eq!(channel.kill(call(1)).unwrap(), None);
        assert_eq!(channel.kill(call(9)).err(), Some(WireError::UnknownCall));
        // Launcher frames follow the per-call order.
        let mut t = 0;
        let mut recv = |channel: &mut SpawndChannel, frame: FromLauncher| { t += 1; channel.receive(t, &bytes(frame), Receipt::plain()) };
        assert_eq!(recv(&mut channel, FromLauncher::Started { call: call(1) }),
            Ok(SpawndEvent::Started { call: call(1), slot: HelperSlot::Foreground }));
        assert_eq!(recv(&mut channel, FromLauncher::Ended { call: call(1), end: End::Exited(3) }),
            Ok(SpawndEvent::Ended { call: call(1), slot: HelperSlot::Foreground, end: End::Exited(3) }));
        assert!(!channel.slot_busy(HelperSlot::Foreground));
        assert_eq!(recv(&mut channel, FromLauncher::Ended { call: call(3), end: End::StartFailed(LauncherError::Sandbox) }),
            Ok(SpawndEvent::Ended { call: call(3), slot: HelperSlot::Recording, end: End::StartFailed(LauncherError::Sandbox) }));
        assert_eq!(recv(&mut channel, FromLauncher::Draining), Ok(SpawndEvent::Draining));
        assert!(channel.is_draining());
        assert_eq!(channel.start(start(4, 0, HelperSlot::Foreground)).err(), Some(WireError::IllegalState), "draining");
        // Each illegal launcher frame closes a fresh channel.
        let illegal: [(&[FromLauncher], WireError); 7] = [
            (&[FromLauncher::Started { call: call(1) }, FromLauncher::Started { call: call(1) }], WireError::IllegalState),
            (&[FromLauncher::Ended { call: call(1), end: End::Exited(0) }], WireError::IllegalState),
            (&[FromLauncher::Started { call: call(1) }, FromLauncher::Ended { call: call(1), end: End::StartFailed(LauncherError::Exec) }],
                WireError::IllegalState),
            (&[FromLauncher::Started { call: call(7) }], WireError::UnknownCall),
            (&[FromLauncher::Started { call: call(1) }, FromLauncher::Ended { call: call(1), end: End::TimedOut },
                FromLauncher::Ended { call: call(1), end: End::TimedOut }], WireError::UnknownCall),
            (&[FromLauncher::Draining, FromLauncher::Draining], WireError::IllegalState),
            (&[FromLauncher::Error { code: LauncherError::Internal }], WireError::Launcher(LauncherError::Internal)),
        ];
        for (frames, expected) in illegal {
            let mut channel = SpawndChannel::new(table()).unwrap();
            channel.start(start(1, 0, HelperSlot::Foreground)).unwrap();
            let (last, prefix) = frames.split_last().unwrap();
            for (index, frame) in prefix.iter().enumerate() {
                channel.receive(index as u64, &bytes(*frame), Receipt::plain()).unwrap();
            }
            assert_eq!(channel.receive(100, &bytes(*last), Receipt::plain()), Err(expected), "{frames:?}");
            assert!(channel.is_closed());
        }
        // Malformed bytes close it too.
        let mut channel = SpawndChannel::new(table()).unwrap();
        assert_eq!(channel.receive(0, b"DCH1", Receipt::plain()), Err(WireError::Length));
        assert!(channel.is_closed());
        assert!(SpawndChannel::new(vec![]).is_err() && SpawndChannel::new(vec![HelperSlot::Foreground; 17]).is_err());
    }

    #[test]
    fn wire_abandoned_calls_accept_late_frames_in_order() {
        let mut channel = SpawndChannel::new(table()).unwrap();
        channel.start(start(1, 0, HelperSlot::Foreground)).unwrap();
        // spawnd's watchdog killed the slot root-side; the launcher is stopped.
        assert_eq!(channel.abandon(call(1)), Ok(HelperSlot::Foreground));
        assert!(!channel.slot_busy(HelperSlot::Foreground));
        channel.start(start(2, 2, HelperSlot::Foreground)).unwrap();
        let started = bytes(FromLauncher::Started { call: call(1) });
        let ended = bytes(FromLauncher::Ended { call: call(1), end: End::Signalled(9) });
        assert_eq!(channel.receive(1, &started, Receipt::plain()), Ok(SpawndEvent::Late { call: call(1) }));
        assert_eq!(channel.receive(2, &ended, Receipt::plain()), Ok(SpawndEvent::Late { call: call(1) }));
        // Retired: a further frame for it is an unknown call.
        assert_eq!(channel.receive(3, &ended, Receipt::plain()), Err(WireError::UnknownCall));
        // A late frame must still respect the order (no Ended(exit) before Started).
        let mut channel = SpawndChannel::new(table()).unwrap();
        channel.start(start(1, 0, HelperSlot::Foreground)).unwrap();
        channel.abandon(call(1)).unwrap();
        assert_eq!(channel.receive(1, &ended, Receipt::plain()), Err(WireError::IllegalState));
        // An unresponsive launcher: too many abandoned calls closes the channel.
        let mut channel = SpawndChannel::new(table()).unwrap();
        for byte in 1..=MAX_ABANDONED_CALLS as u8 {
            channel.start(start(byte, 0, HelperSlot::Foreground)).unwrap();
            channel.abandon(call(byte)).unwrap();
        }
        channel.start(start(99, 0, HelperSlot::Foreground)).unwrap();
        assert_eq!(channel.abandon(call(99)), Err(WireError::Rate));
        assert!(channel.is_closed());
    }

    #[test]
    fn wire_rate_limit_64_frames_per_second() {
        let mut channel = SpawndChannel::new(table()).unwrap();
        // Alternate Started/Ended for fresh calls: every frame is legal.
        let mut frames = 0usize;
        let mut byte = 1u8;
        let mut send = |channel: &mut SpawndChannel, now: u64| -> Result<(), WireError> {
            if frames % 2 == 0 { channel.start(start(byte, 0, HelperSlot::Foreground))?; }
            let frame = if frames % 2 == 0 { FromLauncher::Started { call: call(byte) } }
                else { let f = FromLauncher::Ended { call: call(byte), end: End::Exited(0) }; byte += 1; f };
            frames += 1;
            channel.receive(now, &bytes(frame), Receipt::plain()).map(|_| ())
        };
        for index in 0..MAX_FRAMES_PER_SECOND as u64 { send(&mut channel, 1_000 + index).unwrap(); }
        // The 65th frame inside the same second closes the channel.
        assert_eq!(send(&mut channel, 1_999), Err(WireError::Rate));
        assert!(channel.is_closed());
        // A steady stream just under 64/s is fine for any length; a burst after it is not.
        let mut rate = FrameRate::new();
        for index in 0..1_000u64 { rate.admit(index * 16).unwrap(); }
        let last = 999 * 16;
        let burst = (1..=MAX_FRAMES_PER_SECOND as u64).map(|step| rate.admit(last + step)).find(Result::is_err);
        assert_eq!(burst, Some(Err(WireError::Rate)));
        // After a quiet second the window is free again.
        let mut rate = FrameRate::new();
        for _ in 0..MAX_FRAMES_PER_SECOND { rate.admit(5).unwrap(); }
        assert_eq!(rate.admit(1_004), Err(WireError::Rate));
        assert_eq!(rate.admit(1_005), Ok(()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[allow(unsafe_code)]
    fn wire_linux_seqpacket_fds_cmsgs_truncation_and_eof() {
        use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
        let mut pair = [0; 2];
        // SAFETY: two-element output array for the new descriptors, owned below.
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, pair.as_mut_ptr()) }, 0);
        let (spawnd, launcher) = unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) };
        let null = std::fs::File::open("/dev/null").unwrap();
        // Start + exactly 3 fds reaches the launcher intact, CLOEXEC.
        let message = ToLauncher::Start(start(1, 0, HelperSlot::Foreground));
        linux::send(spawnd.as_fd(), &message.encode().unwrap(), &[null.as_fd(), null.as_fd(), null.as_fd()]).unwrap();
        let (bytes, fds, receipt) = linux::recv(launcher.as_fd(), MAX_FRAME_FDS, false).unwrap().unwrap();
        assert_eq!(receipt, Receipt::with_fds(3));
        assert_eq!(fds.len(), 3);
        for fd in &fds {
            // SAFETY: F_GETFD on an owned descriptor.
            assert_ne!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
        }
        assert_eq!(decode_to_launcher(&bytes, receipt), Ok(message));
        drop(fds);
        // A launcher frame with fds into spawnd's minimal buffer: never accepted.
        let started = FromLauncher::Started { call: call(1) }.encode().unwrap();
        linux::send(launcher.as_fd(), &started, &[null.as_fd(), null.as_fd(), null.as_fd()]).unwrap();
        let (bytes, fds, receipt) = linux::recv(spawnd.as_fd(), 0, false).unwrap().unwrap();
        assert!(fds.len() <= 2 && receipt.control_truncated, "{receipt:?}");
        assert!(decode_from_launcher(&bytes, receipt).is_err());
        linux::send(launcher.as_fd(), &started, &[null.as_fd()]).unwrap();
        let (bytes, fds, receipt) = linux::recv(spawnd.as_fd(), 0, false).unwrap().unwrap();
        assert_eq!((fds.len(), receipt.control_truncated), (1, false));
        assert_eq!(decode_from_launcher(&bytes, receipt), Err(WireError::Ancillary));
        drop(fds);
        // Oversized packet ⇒ MSG_TRUNC; `send` itself refuses to build one.
        let big = vec![0u8; MAX_FRAME_BYTES + 1];
        assert!(linux::send(launcher.as_fd(), &big, &[]).is_err());
        // SAFETY: plain send of a live buffer on an owned socket.
        assert_eq!(unsafe { libc::send(launcher.as_raw_fd(), big.as_ptr().cast(), big.len(), 0) }, big.len() as isize);
        let (bytes, _, receipt) = linux::recv(spawnd.as_fd(), 0, false).unwrap().unwrap();
        assert!(receipt.truncated);
        assert_eq!(decode_from_launcher(&bytes, receipt), Err(WireError::Truncated));
        // A clean frame passes; nonblocking with nothing queued is WouldBlock; EOF is None.
        linux::send(launcher.as_fd(), &started, &[]).unwrap();
        let (bytes, fds, receipt) = linux::recv(spawnd.as_fd(), 0, true).unwrap().unwrap();
        assert!(fds.is_empty());
        assert_eq!(decode_from_launcher(&bytes, receipt), Ok(FromLauncher::Started { call: call(1) }));
        assert_eq!(linux::recv(spawnd.as_fd(), 0, true).err().map(|e| e.kind()), Some(std::io::ErrorKind::WouldBlock));
        drop(launcher);
        assert!(linux::recv(spawnd.as_fd(), 0, false).unwrap().is_none());
    }

    #[test]
    fn wire_launcher_state_table() {
        let mut launcher = LauncherChannel::new(table()).unwrap();
        let receive = |launcher: &mut LauncherChannel, message: ToLauncher| {
            launcher.receive(&message.encode().unwrap(), Receipt::with_fds(message.fd_count()))
        };
        let fg = start(1, 0, HelperSlot::Foreground);
        assert_eq!(receive(&mut launcher, ToLauncher::Start(fg)), Ok(Command::Start(fg)));
        assert_eq!(launcher.live(HelperSlot::Foreground), Some(call(1)));
        // Slot busy is a race, not a violation: refused, the channel stays open.
        assert_eq!(receive(&mut launcher, ToLauncher::Start(start(2, 2, HelperSlot::Foreground))),
            Ok(Command::Refuse { call: call(2), reason: LauncherError::SlotBusy }));
        assert_eq!(launcher.refused(call(2), LauncherError::SlotBusy),
            Ok(FromLauncher::Ended { call: call(2), end: End::StartFailed(LauncherError::SlotBusy) }));
        assert_eq!(launcher.refused(call(1), LauncherError::SlotBusy), Err(WireError::UnknownCall), "call 1 holds the slot");
        assert_eq!(launcher.refused(call(2), LauncherError::Protocol).err(), Some(WireError::Field("start failure")));
        assert_eq!(receive(&mut launcher, ToLauncher::Kill { call: call(1) }),
            Ok(Command::Kill { call: call(1), slot: HelperSlot::Foreground }));
        assert_eq!(launcher.started(call(1)), Ok(FromLauncher::Started { call: call(1) }));
        assert_eq!(launcher.ended(call(1), End::Signalled(9)), Ok(FromLauncher::Ended { call: call(1), end: End::Signalled(9) }));
        assert_eq!(launcher.ended(call(1), End::Signalled(9)), Err(WireError::UnknownCall));
        // Kill/Stop racing with Ended are ignored.
        assert_eq!(receive(&mut launcher, ToLauncher::Kill { call: call(1) }), Ok(Command::Ignore));
        assert_eq!(receive(&mut launcher, ToLauncher::Stop { call: call(2) }), Ok(Command::Ignore));
        let rec = start(3, 1, HelperSlot::Recording);
        assert_eq!(receive(&mut launcher, ToLauncher::Start(rec)), Ok(Command::Start(rec)));
        assert_eq!(receive(&mut launcher, ToLauncher::Stop { call: call(3) }),
            Ok(Command::Stop { call: call(3), slot: HelperSlot::Recording }));
        // After SIGTERM: one Draining frame, then every Start is refused.
        assert_eq!(launcher.begin_draining(), Some(FromLauncher::Draining));
        assert_eq!(launcher.begin_draining(), None);
        assert_eq!(receive(&mut launcher, ToLauncher::Start(start(4, 0, HelperSlot::Foreground))),
            Ok(Command::Refuse { call: call(4), reason: LauncherError::Draining }));
        // Protocol violations close it: id reuse, wrong slot for the helper,
        // index outside the table, Stop on a foreground call, unknown call, fd surplus.
        let violations: [&dyn Fn(&mut LauncherChannel) -> Result<Command, WireError>; 6] = [
            &|l| receive(l, ToLauncher::Start(start(1, 0, HelperSlot::Foreground))),
            &|l| receive(l, ToLauncher::Start(start(5, 1, HelperSlot::Foreground))),
            &|l| receive(l, ToLauncher::Start(start(5, 3, HelperSlot::Foreground))),
            &|l| { receive(l, ToLauncher::Start(start(6, 0, HelperSlot::Foreground))).unwrap(); receive(l, ToLauncher::Stop { call: call(6) }) },
            &|l| receive(l, ToLauncher::Kill { call: call(8) }),
            &|l| l.receive(&ToLauncher::Kill { call: call(1) }.encode().unwrap(), Receipt::with_fds(1)),
        ];
        for (index, violation) in violations.iter().enumerate() {
            let mut launcher = LauncherChannel::new(table()).unwrap();
            receive(&mut launcher, ToLauncher::Start(fg)).unwrap();
            launcher.ended(call(1), End::Exited(0)).unwrap();
            assert!(violation(&mut launcher).is_err(), "violation {index}");
            assert_eq!(launcher.receive(&ToLauncher::Kill { call: call(1) }.encode().unwrap(), Receipt::plain()),
                Err(WireError::Closed), "violation {index} closes");
        }
    }
}
