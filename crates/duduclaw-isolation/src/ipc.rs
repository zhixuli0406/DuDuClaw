//! spawnd v1 wire contract (§4, §4.4, §4.5). Transport is AF_UNIX SOCK_SEQPACKET.
//! Authenticate the fixed root-owned socket with SO_PEERCRED uid/gid, never PID.
//! One packet = u32 big-endian payload length + UTF-8 JSON. No trailing bytes.
//! Use recvmsg(MSG_CMSG_CLOEXEC); reject MSG_TRUNC/MSG_CTRUNC and unknown cmsgs,
//! close every received fd on rejection. Actual SCM_RIGHTS lives in package B.
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use crate::{bounded::{List, Text}, execution::{PtySize, RunHandle, RunOutcome, RuntimeKind}, ids::{EmployeeId, RequestId}, ContractError};

pub const PROTOCOL_VERSION: u16 = 1;
/// Phase-zero hard payload cap; the four-byte header is additional (§4).
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_INPUT_FDS: usize = 64;
pub const MAX_PROMPT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_QUEUE_LENGTH: u16 = 32;
pub const CONTROL_TIMEOUT_MS: u64 = 5_000;
pub const RESERVATION_TIMEOUT_MS: u64 = 30_000;

/// Version and correlation wrapper for both directions (§4). Unknown versions fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> { pub version: u16, pub request_id: RequestId, pub message: T }

/// Host-generated connection identity, unrelated to PID or peer-supplied bytes (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlConnectionId(pub [u8; 16]);

/// Server-only reservation for the pre-start capability handshake (§4, §6.1).
/// This pure record enforces single consumption/binding/expiry; B additionally
/// enforces bounded reservation counts and destroys it on control disconnect.
#[derive(Debug)]
pub struct RunReservation {
    run: RunHandle,
    employee: EmployeeId,
    connection: ControlConnectionId,
    expires_at_unix_ms: u64,
    consumed: bool,
}
impl RunReservation {
    /// Called with a fresh unpredictable handle after peer/admission checks.
    pub fn new(run: RunHandle, employee: EmployeeId, connection: ControlConnectionId,
        now_unix_ms: u64) -> Result<Self, ContractError> {
        let expires_at_unix_ms = now_unix_ms.checked_add(RESERVATION_TIMEOUT_MS)
            .ok_or(ContractError::InvalidValue("reservation time"))?;
        Ok(Self { run, employee, connection, expires_at_unix_ms, consumed: false })
    }
    pub fn expires_at_unix_ms(&self) -> u64 { self.expires_at_unix_ms }
    /// Failed consumption leaves the reservation unchanged. Caller resolves the
    /// requested handle to this server record, never accepts a client record.
    pub fn consume(&mut self, employee: EmployeeId, connection: ControlConnectionId,
        now_unix_ms: u64) -> Result<RunHandle, ContractError> {
        if self.consumed || employee != self.employee || connection != self.connection
            || now_unix_ms >= self.expires_at_unix_ms {
            return Err(ContractError::InvalidValue("run reservation"));
        }
        self.consumed = true;
        Ok(self.run)
    }
}

/// Encode under the allocation cap, including serde expansion (§4).
pub fn encode<T: Serialize>(envelope: &Envelope<T>) -> Result<Vec<u8>, ContractError> {
    if envelope.version != PROTOCOL_VERSION { return Err(ContractError::UnsupportedVersion); }
    struct Limited(Vec<u8>);
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut payload = Limited(Vec::new());
    serde_json::to_writer(&mut payload, envelope).map_err(|_| ContractError::LimitExceeded("frame"))?;
    frame_payload(&payload.0)
}

/// Frame already encoded bytes; useful for non-JSON bounded data-plane adapters (§4).
pub fn frame_payload(payload: &[u8]) -> Result<Vec<u8>, ContractError> {
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES { return Err(ContractError::LimitExceeded("frame")); }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Reject invalid length before parsing/allocating a payload (§4).
pub fn payload(frame: &[u8]) -> Result<&[u8], ContractError> {
    let header: [u8; 4] = frame.get(..4).ok_or(ContractError::InvalidFrame)?.try_into().map_err(|_| ContractError::InvalidFrame)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES { return Err(ContractError::LimitExceeded("frame")); }
    if frame.len() != length + 4 { return Err(ContractError::InvalidFrame); }
    Ok(&frame[4..])
}

/// Structural decode only; caller must validate message semantics, fds and peer (§4).
pub fn decode<T: DeserializeOwned>(frame: &[u8]) -> Result<Envelope<T>, ContractError> {
    let envelope: Envelope<T> = serde_json::from_slice(payload(frame)?).map_err(|_| ContractError::InvalidFrame)?;
    if envelope.version != PROTOCOL_VERSION { return Err(ContractError::UnsupportedVersion); }
    Ok(envelope)
}

/// Attachment backing; fd sizes and seals must be verified against actual fds (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backing", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputFd { SealedMemfd { bytes: u64 }, ReadOnlySnapshot { bytes: u64 } }

/// Narrow root RPC. No uid, argv, env, cwd, profile, paths or unit properties (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpawnRequest {
    /// Phase-zero handshake to satisfy §6.1's pre-StartRun capability creation.
    /// Server generates the id, binds it to employee + authenticated connection,
    /// and expires it after RESERVATION_TIMEOUT_MS. Reservations count against
    /// the per-employee queue cap and host admission budget; no fds are carried.
    ReserveRun { employee_id: EmployeeId },
    StartRun {
        /// Must be an unused reservation for this employee and connection.
        run: RunHandle,
        employee_id: EmployeeId,
        runtime_request: RuntimeKind,
        task_ref: Option<Text<128>>,
        prompt_bytes: u64,
        input_fds: List<InputFd, MAX_INPUT_FDS>,
        pty: bool,
    },
    Resize { run: RunHandle, size: PtySize },
    Cancel { run: RunHandle },
    /// `caller_run` is host-derived. Same-employee waits return WouldDeadlock;
    /// the queued run remains asynchronous. None is reserved for gateway callers.
    Wait { run: RunHandle, caller_run: Option<RunHandle> },
    Status { run: RunHandle },
    /// Take gateway endpoint and output descriptors once after admission. A queued
    /// StartRun cannot deliver live capabilities. No fd is sent in this request.
    TakeIo { run: RunHandle },
}

/// Ordered ancillary roles, never OS descriptor numbers (§4 fd matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdRole { PromptSealed, AttachmentSealed, AttachmentSnapshot, McpGateway, StdoutRead, StderrRead, StdinWrite, PtyMaster }

impl SpawnRequest {
    /// Exact fd order accompanying this message. There are no caller-supplied
    /// mount, executable, MCP or egress fds. B creates the latter pairs itself.
    pub fn fd_plan(&self) -> Result<Vec<FdRole>, ContractError> {
        self.validate()?;
        let mut roles = Vec::new();
        if let Self::StartRun { input_fds, .. } = self {
            roles.push(FdRole::PromptSealed);
            roles.extend(input_fds.as_slice().iter().map(|input| match input {
                InputFd::SealedMemfd { .. } => FdRole::AttachmentSealed,
                InputFd::ReadOnlySnapshot { .. } => FdRole::AttachmentSnapshot,
            }));
        }
        Ok(roles)
    }
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::StartRun { prompt_bytes, input_fds, .. } => {
                if *prompt_bytes > MAX_PROMPT_BYTES { return Err(ContractError::LimitExceeded("prompt")); }
                let mut total = 0u64;
                let mut memfd_total = *prompt_bytes;
                for input in input_fds.as_slice() {
                    let bytes = match input {
                        InputFd::SealedMemfd { bytes } => {
                            if *bytes > 16 * 1024 * 1024 { return Err(ContractError::LimitExceeded("memfd")); }
                            memfd_total = memfd_total.checked_add(*bytes).ok_or(ContractError::LimitExceeded("memfds"))?;
                            *bytes
                        }
                        InputFd::ReadOnlySnapshot { bytes } => *bytes,
                    };
                    total = total.checked_add(bytes).ok_or(ContractError::LimitExceeded("attachments"))?;
                }
                if total > MAX_ATTACHMENT_TOTAL_BYTES || memfd_total > 64 * 1024 * 1024 {
                    return Err(ContractError::LimitExceeded("inputs"));
                }
            }
            Self::Resize { size, .. } => size.validate()?,
            _ => {}
        }
        Ok(())
    }
}

/// Caller-safe errors; diagnostic detail goes only to bounded root logs (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode { UnsupportedVersion, InvalidRequest, Unauthorized, InvalidFds, QueueFull, QueueExpired, NotFound, NotReady, AlreadyTaken, WouldDeadlock, RuntimeDenied, ResourceLimit, IsolationUnavailable, Internal }

/// Queue priority is host-derived from verified task context (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueClass { Interactive, Background }

/// Public queue progress, including same-employee deferral (§4.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueStatus {
    pub ahead: u16,
    pub expires_at_unix_ms: u64,
    pub deferred_until_current_run_ends: bool,
}

/// Limited public state; no PID, command line or cwd (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunStatus { Reserved { expires_at_unix_ms: u64 }, Queued(QueueStatus), Starting, Running, Ended(RunOutcome) }

/// Exact gateway fd set in TakeIo, with MCP enabled only by root policy (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IoDelivery { pub mcp: bool, pub pty: bool }
impl IoDelivery {
    pub fn fd_plan(self) -> Vec<FdRole> {
        let mut plan = Vec::new();
        if self.mcp { plan.push(FdRole::McpGateway); }
        if self.pty { plan.push(FdRole::PtyMaster); }
        else { plan.extend([FdRole::StdinWrite, FdRole::StdoutRead, FdRole::StderrRead]); }
        plan
    }
}

/// Versioned through Envelope; only Io carries fds, all other replies carry zero (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpawnResponse {
    Reserved { run: RunHandle, expires_at_unix_ms: u64 },
    Accepted { run: RunHandle, status: RunStatus },
    Status { run: RunHandle, status: RunStatus },
    Io { run: RunHandle, delivery: IoDelivery },
    Ack,
    Error { code: ErrorCode },
}
impl SpawnResponse {
    pub fn fd_plan(&self) -> Vec<FdRole> {
        match self { Self::Io { delivery, .. } => delivery.fd_plan(), _ => vec![] }
    }
}

/// Root-owned lease lifecycle. `Draining` continues to hold the employee lease (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionState { Reserved, Queued, Starting, Running, Draining, Finished, Cancelled, Expired }

/// All three independent host readbacks must succeed before releasing a lease (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReadback { pub unit_inactive: bool, pub cgroup_empty: bool, pub employee_uid_absent: bool }
impl CleanupReadback {
    pub fn complete(self) -> bool { self.unit_inactive && self.cgroup_empty && self.employee_uid_absent }
}
impl AdmissionState {
    /// Only the daemon may supply cleanup evidence, including on initial admission.
    pub fn transition(self, next: Self, readback: Option<CleanupReadback>) -> Result<Self, ContractError> {
        use AdmissionState::*;
        let valid = match (self, next) {
            (Queued, Starting) | (Draining, Finished) => readback.is_some_and(CleanupReadback::complete),
            (Reserved, Queued | Cancelled | Expired) | (Queued, Cancelled | Expired)
            | (Starting, Running | Draining) | (Running, Draining) => true,
            _ => false,
        };
        if valid { Ok(next) } else { Err(ContractError::IllegalTransition) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn start() -> SpawnRequest {
        SpawnRequest::StartRun { run: RunHandle(crate::ids::RunId([3; 16])), employee_id: EmployeeId([1; 16]), runtime_request: RuntimeKind::Codex,
            task_ref: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false }
    }
    #[test]
    fn serde_round_trip_and_fd_plan() {
        let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: start() };
        assert_eq!(decode::<SpawnRequest>(&encode(&envelope).unwrap()).unwrap(), envelope);
        assert_eq!(envelope.message.fd_plan().unwrap(), vec![FdRole::PromptSealed]);
        for forbidden in ["profile", "uid", "argv", "env", "cwd", "mount", "unit_properties"] {
            let mut value = serde_json::to_value(start()).unwrap();
            value[forbidden] = "unsafe".into();
            assert!(serde_json::from_value::<SpawnRequest>(value).is_err());
        }
    }
    #[test]
    fn frame_size_limits_and_trailing_data() {
        assert!(frame_payload(&vec![b'x'; MAX_FRAME_BYTES]).is_ok());
        assert!(frame_payload(&vec![b'x'; MAX_FRAME_BYTES + 1]).is_err());
        assert!(payload(&u32::MAX.to_be_bytes()).is_err());
        assert!(payload(&[0, 0, 0]).is_err());
        let mut packet = frame_payload(b"{}").unwrap();
        packet.push(0);
        assert!(payload(&packet).is_err());
        let message = Envelope { version: 99, request_id: RequestId([0; 16]), message: SpawnResponse::Ack };
        assert_eq!(encode(&message), Err(ContractError::UnsupportedVersion));
        let raw = serde_json::to_vec(&message).unwrap();
        assert_eq!(decode::<SpawnResponse>(&frame_payload(&raw).unwrap()), Err(ContractError::UnsupportedVersion));
        let oversized = Envelope { version: 1, request_id: RequestId([0; 16]), message: "x".repeat(MAX_FRAME_BYTES) };
        assert_eq!(encode(&oversized), Err(ContractError::LimitExceeded("frame")));
    }
    #[test]
    fn reservation_is_single_use_expiring_and_connection_bound() {
        let employee = EmployeeId([1; 16]);
        let connection = ControlConnectionId([2; 16]);
        let run = RunHandle(crate::ids::RunId([3; 16]));
        let mut reservation = RunReservation::new(run, employee, connection, 100).unwrap();
        assert!(reservation.consume(EmployeeId([9; 16]), connection, 101).is_err());
        assert!(reservation.consume(employee, ControlConnectionId([9; 16]), 101).is_err());
        assert_eq!(reservation.consume(employee, connection, 101), Ok(run));
        assert!(reservation.consume(employee, connection, 102).is_err());
        let mut expired = RunReservation::new(run, employee, connection, 100).unwrap();
        assert!(expired.consume(employee, connection, 100 + RESERVATION_TIMEOUT_MS).is_err());
        assert!(RunReservation::new(run, employee, connection, u64::MAX).is_err());
    }
    #[test]
    fn admission_transition_table_requires_all_three_readbacks() {
        use AdmissionState::*;
        let states = [Reserved, Queued, Starting, Running, Draining, Finished, Cancelled, Expired];
        let clear = CleanupReadback { unit_inactive: true, cgroup_empty: true, employee_uid_absent: true };
        let edges = [(Reserved, Queued), (Reserved, Cancelled), (Reserved, Expired),
            (Queued, Starting), (Queued, Cancelled), (Queued, Expired), (Starting, Running),
            (Starting, Draining), (Running, Draining), (Draining, Finished)];
        for from in states { for to in states {
            assert_eq!(from.transition(to, Some(clear)).is_ok(), edges.contains(&(from, to)));
        }}
        for bits in 0..7 {
            let incomplete = CleanupReadback { unit_inactive: bits & 1 != 0, cgroup_empty: bits & 2 != 0, employee_uid_absent: bits & 4 != 0 };
            assert!(Queued.transition(Starting, Some(incomplete)).is_err());
            assert!(Draining.transition(Finished, Some(incomplete)).is_err());
        }
    }
}
