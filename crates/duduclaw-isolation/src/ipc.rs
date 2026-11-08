//! spawnd v1 wire contract (§4, §4.4, §4.5). Transport is AF_UNIX SOCK_SEQPACKET.
//! Authenticate the fixed root-owned socket with SO_PEERCRED uid/gid, never PID.
//! One packet = u32 big-endian payload length + UTF-8 JSON. No trailing bytes.
//! Use recvmsg(MSG_CMSG_CLOEXEC); reject MSG_TRUNC/MSG_CTRUNC and unknown cmsgs,
//! close every received fd on rejection. Actual SCM_RIGHTS lives in package B.
use std::ops::RangeInclusive;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use crate::{bounded::{List, Text}, execution::{PtySize, RunHandle, RunOutcome, RuntimeKind}, ids::{EmployeeId, RequestId}, ContractError};

pub const PROTOCOL_VERSION: u16 = 1;

/// Stable operation registry (Package H, H1). The JSON wire carries the `op`
/// name, never the number; numbers exist so that every `SpawnRequest` variant
/// has one collision-checked identity for audit/telemetry and so that parallel
/// packages coordinate explicitly. Ranges, never reused once assigned:
/// - `OPCODES_CORE` 0x0001–0x00FF: v1 run lifecycle.
/// - `OPCODES_WORK` 0x0100–0x01FF: Package H root work broker (D-H1).
/// - `OPCODES_HELPER` 0x0200–0x02FF: HX in-run helpers (D8 revision, option A).
///   Numbers and names are reserved in `HX_RESERVED_OPS`; HX4 adds the
///   variants with exactly those values. Package H must not use this range.
/// `SpawnRequest::opcode` is an exhaustive match, so a new variant without a
/// registry entry does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum Opcode {
    ReserveRun = 0x0001, StartRun = 0x0002, Resize = 0x0003, PtyActivity = 0x0004,
    Cancel = 0x0005, Wait = 0x0006, Status = 0x0007, TakeIo = 0x0008,
    OpenWorkFile = 0x0101, WorkAnchor = 0x0102,
}
pub const OPCODES_CORE: RangeInclusive<u16> = 0x0001..=0x00ff;
pub const OPCODES_WORK: RangeInclusive<u16> = 0x0100..=0x01ff;
pub const OPCODES_HELPER: RangeInclusive<u16> = 0x0200..=0x02ff;
impl Opcode {
    pub const ALL: [Self; 10] = [Self::ReserveRun, Self::StartRun, Self::Resize, Self::PtyActivity,
        Self::Cancel, Self::Wait, Self::Status, Self::TakeIo, Self::OpenWorkFile, Self::WorkAnchor];
    pub const fn code(self) -> u16 { self as u16 }
    /// The serde `op` tag of the matching `SpawnRequest` variant.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::ReserveRun => "reserve_run", Self::StartRun => "start_run", Self::Resize => "resize",
            Self::PtyActivity => "pty_activity", Self::Cancel => "cancel", Self::Wait => "wait",
            Self::Status => "status", Self::TakeIo => "take_io",
            Self::OpenWorkFile => "open_work_file", Self::WorkAnchor => "work_anchor",
        }
    }
    pub fn from_code(code: u16) -> Option<Self> { Self::ALL.into_iter().find(|op| op.code() == code) }
}

/// A number + `op` name held for a later package; not decodable until it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedOp { pub code: u16, pub op: &'static str, pub owner: &'static str }
/// HX §5 IPC (`HelperCall`/`HelperWait`/`HelperStop`/`HelperCancel`), owner HX4.
pub const HX_RESERVED_OPS: [ReservedOp; 4] = [
    ReservedOp { code: 0x0201, op: "helper_call", owner: "HX4" },
    ReservedOp { code: 0x0202, op: "helper_wait", owner: "HX4" },
    ReservedOp { code: 0x0203, op: "helper_stop", owner: "HX4" },
    ReservedOp { code: 0x0204, op: "helper_cancel", owner: "HX4" },
];
/// HX §5 `SpawnResponse` result tags (`HelperStarted`, `HelperEnded`), owner HX4.
pub const HX_RESERVED_RESULTS: [&str; 2] = ["helper_started", "helper_ended"];
/// HX §5 `ErrorCode` names, owner HX4. Package H error codes must differ.
pub const HX_RESERVED_ERROR_CODES: [&str; 4] =
    ["helper_busy", "helper_unavailable", "run_time_exhausted", "helper_cgroup_unavailable"];

/// D-H1 relative path bounds. The sum of components is also bounded by
/// `MAX_WORK_PATH_BYTES`, so an `OpenWorkFile` frame is always far below
/// `MAX_FRAME_BYTES`.
pub const MAX_WORK_PATH_BYTES: usize = 4096;
pub const MAX_WORK_PATH_COMPONENTS: usize = 64;
/// Linux NAME_MAX; a longer component can never name an entry.
pub const MAX_WORK_COMPONENT_BYTES: usize = 255;
/// Largest file the H2 broker hands out (D-H1, ≤ 16 MiB). Larger is `WorkFileTooLarge`.
pub const MAX_WORK_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// First component reserved for fork workspaces (`WorkspaceSelection::storage_relative`).
/// A fork has its own `fork-<id>` resource; `employee-work` paths never reach it.
pub const WORK_RESERVED_FIRST_COMPONENT: &str = ".forks";

/// Normalized path relative to the root of the live run's attached
/// `employee-work` mount. Validated on construction and on deserialization,
/// so an unsafe path is unrepresentable on the sender and fails frame decode
/// on the receiver. Rules: non-empty, ≤ 4096 bytes, ≤ 64 components, each
/// component non-empty, ≤ 255 bytes, not `.`/`..`, no NUL, `\` or ASCII
/// control byte; no leading `/` (absolute), no trailing or doubled `/`; first
/// component not `.forks`. Syntax only: the H2 broker still resolves it with
/// `openat2(RESOLVE_BENEATH|NO_SYMLINKS|NO_MAGICLINKS|NO_XDEV)` and checks the
/// opened inode, so symlinks, hard links and mounts are not decided here.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct WorkPath(String);
impl WorkPath {
    pub fn new(path: String) -> Result<Self, ContractError> {
        validate_work_path(&path)?;
        Ok(Self(path))
    }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn components(&self) -> impl Iterator<Item = &str> { self.0.split('/') }
    pub fn validate(&self) -> Result<(), ContractError> { validate_work_path(&self.0) }
}
/// Root logs must not carry employee file names; like `Text`, print sizes only.
impl std::fmt::Debug for WorkPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkPath").field("bytes", &self.0.len())
            .field("components", &self.components().count()).finish()
    }
}
impl<'de> Deserialize<'de> for WorkPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
fn validate_work_path(path: &str) -> Result<(), ContractError> {
    const INVALID: ContractError = ContractError::InvalidValue("work path");
    if path.is_empty() || path.starts_with('/') { return Err(INVALID); }
    if path.len() > MAX_WORK_PATH_BYTES { return Err(ContractError::LimitExceeded("work path")); }
    for (index, part) in path.split('/').enumerate() {
        if index >= MAX_WORK_PATH_COMPONENTS { return Err(ContractError::LimitExceeded("work path components")); }
        if part.len() > MAX_WORK_COMPONENT_BYTES { return Err(ContractError::LimitExceeded("work path component")); }
        if part.is_empty() || part == "." || part == ".."
            || (index == 0 && part == WORK_RESERVED_FIRST_COMPONENT)
            || part.bytes().any(|b| b == b'\\' || b.is_ascii_control()) {
            return Err(INVALID);
        }
    }
    Ok(())
}

/// The only open mode in v1. Write access to employee files is never brokered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkOpenMode { ReadOnly }

/// Identity of the file behind the single `WorkFileRead` fd, read by root with
/// `fstat` on that fd. The receiver must `fstat` the received fd and compare
/// `device`/`inode`/`size`, and require S_ISREG and O_RDONLY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkFileMetadata { pub size: u64, pub inode: u64, pub device: u64 }
impl WorkFileMetadata {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.size > MAX_WORK_FILE_BYTES { return Err(ContractError::LimitExceeded("work file")); }
        if self.inode == 0 || self.device == 0 { return Err(ContractError::InvalidValue("work file identity")); }
        Ok(())
    }
}

/// Identity of an employee's root-owned work anchor directory: the stable
/// `employee-work` resource identity, valid with or without a live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkAnchorIdentity { pub device: u64, pub inode: u64 }
impl WorkAnchorIdentity {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.inode == 0 || self.device == 0 { return Err(ContractError::InvalidValue("work anchor")); }
        Ok(())
    }
}

/// Pair a Package H reply with its request and the number of received fds.
/// The caller still checks each fd against `FdRole` and the metadata.
pub fn check_work_reply(request: &SpawnRequest, response: &SpawnResponse, received_fds: usize)
    -> Result<(), ContractError> {
    request.validate()?;
    response.validate()?;
    if received_fds != response.fd_plan().len() { return Err(ContractError::InvalidFrame); }
    let paired = match (request, response) {
        (SpawnRequest::OpenWorkFile { .. } | SpawnRequest::WorkAnchor { .. }, SpawnResponse::Error { .. }) => true,
        (SpawnRequest::OpenWorkFile { run, .. }, SpawnResponse::WorkFile { run: actual, .. }) => run == actual,
        (SpawnRequest::WorkAnchor { employee_id }, SpawnResponse::WorkAnchor { employee_id: actual, .. }) =>
            employee_id == actual,
        (SpawnRequest::OpenWorkFile { .. } | SpawnRequest::WorkAnchor { .. }, _) => false,
        _ => return Err(ContractError::InvalidValue("not a work request")),
    };
    if paired { Ok(()) } else { Err(ContractError::InvalidFrame) }
}

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

/// Additive v1 workspace selector. No host path or mount descriptor is accepted.
/// Root resolves the fixed handle beneath the employee's H work mount and
/// compares the opened directory identity to D's authorization readback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSelection {
    pub handle: Text<128>,
    pub relative_cwd: Text<4096>,
    pub device: u64,
    pub inode: u64,
}
impl WorkspaceSelection {
    pub fn validate(&self) -> Result<(), ContractError> {
        let handle = self.handle.as_str();
        let branch = handle.strip_prefix("fork-").is_some_and(|id| id.len() == 32
            && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        let cwd = self.relative_cwd.as_str();
        if (handle != "employee-work" && !branch) || self.device == 0 || self.inode == 0
            || (!matches!(cwd, "" | ".") && (cwd.len() > 4096 || cwd.split('/').any(|part|
                part.is_empty() || part == "." || part == ".." || part.bytes().any(|b| b == 0 || b == b'\\')))) {
            return Err(ContractError::InvalidValue("workspace"));
        }
        Ok(())
    }
    pub fn storage_relative(&self) -> Result<String, ContractError> {
        self.validate()?;
        let root = if self.handle.as_str() == "employee-work" { ".".to_owned() }
            else { format!(".forks/{}", &self.handle.as_str()[5..]) };
        Ok(if matches!(self.relative_cwd.as_str(), "" | ".") { root }
            else { format!("{root}/{}", self.relative_cwd.as_str()) })
    }
}

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<WorkspaceSelection>,
        prompt_bytes: u64,
        input_fds: List<InputFd, MAX_INPUT_FDS>,
        pty: bool,
    },
    /// Before StartRun, bind the initial PTY size to this reservation. It does
    /// not start work; a following non-PTY StartRun is refused. During Running,
    /// change the existing PTY size. Queued/Starting/terminal runs refuse it.
    Resize { run: RunHandle, size: PtySize },
    /// Additive I1e control message. Only the authenticated gateway may report
    /// a successful nonempty PTY read/write after TakeIo. Root supplies time;
    /// this message cannot revive a terminal/draining run or change a deadline.
    PtyActivity { run: RunHandle },
    Cancel { run: RunHandle },
    /// `caller_run` is host-derived. Same-employee waits return WouldDeadlock;
    /// the queued run remains asynchronous. None is reserved for gateway callers.
    Wait { run: RunHandle, caller_run: Option<RunHandle> },
    Status { run: RunHandle },
    /// Take gateway endpoint and output descriptors once after admission. A queued
    /// StartRun cannot deliver live capabilities. No fd is sent in this request.
    TakeIo { run: RunHandle },
    /// Package H, D-H1 (opcode 0x0101). Root opens one regular file beneath the
    /// live run's attached `employee-work` storage mount and returns it. `run`
    /// must be Running and owned by this connection, and `employee_id` must
    /// equal the run's admitted employee; root never derives the employee or the
    /// mount from this message. Carries no fds. Reply: `WorkFile` with exactly
    /// one `WorkFileRead` fd, or `Error`.
    OpenWorkFile { run: RunHandle, employee_id: EmployeeId, path: WorkPath, mode: WorkOpenMode },
    /// Package H, D-H1 (opcode 0x0102). Identity of the employee's root-owned
    /// work anchor directory (the `employee-work` resource identity). No fds in
    /// either direction. Reply: `WorkAnchor`, or `Error`.
    WorkAnchor { employee_id: EmployeeId },
}

/// Ordered ancillary roles, never OS descriptor numbers (§4 fd matrix).
/// `WorkFileRead`: regular file, O_RDONLY, FD_CLOEXEC, opened by the H2 broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdRole { PromptSealed, WorkspaceDirectory, AttachmentSealed, AttachmentSnapshot, McpGateway, StdoutRead, StderrRead, StdinWrite, PtyMaster, WorkFileRead }

impl SpawnRequest {
    /// Registry identity; exhaustive so new variants must be registered.
    pub fn opcode(&self) -> Opcode {
        match self {
            Self::ReserveRun { .. } => Opcode::ReserveRun, Self::StartRun { .. } => Opcode::StartRun,
            Self::Resize { .. } => Opcode::Resize, Self::PtyActivity { .. } => Opcode::PtyActivity,
            Self::Cancel { .. } => Opcode::Cancel, Self::Wait { .. } => Opcode::Wait,
            Self::Status { .. } => Opcode::Status, Self::TakeIo { .. } => Opcode::TakeIo,
            Self::OpenWorkFile { .. } => Opcode::OpenWorkFile, Self::WorkAnchor { .. } => Opcode::WorkAnchor,
        }
    }
    /// Exact fd order accompanying this message. There are no caller-supplied
    /// mount, executable, MCP or egress fds. B creates the latter pairs itself.
    pub fn fd_plan(&self) -> Result<Vec<FdRole>, ContractError> {
        self.validate()?;
        let mut roles = Vec::new();
        if let Self::StartRun { input_fds, workspace, .. } = self {
            roles.push(FdRole::PromptSealed);
            roles.extend(input_fds.as_slice().iter().map(|input| match input {
                InputFd::SealedMemfd { .. } => FdRole::AttachmentSealed,
                InputFd::ReadOnlySnapshot { .. } => FdRole::AttachmentSnapshot,
            }));
            if workspace.is_some() { roles.push(FdRole::WorkspaceDirectory); }
        }
        Ok(roles)
    }
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::StartRun { prompt_bytes, input_fds, workspace, .. } => {
                if let Some(selection) = workspace { selection.validate()?; }
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
            Self::OpenWorkFile { path, mode: WorkOpenMode::ReadOnly, .. } => path.validate()?,
            _ => {}
        }
        Ok(())
    }
}

/// Caller-safe errors; diagnostic detail goes only to bounded root logs (§4.4).
/// Package H (D-H1) additions; existing codes keep their meaning for the work
/// ops (`NotFound` unknown run, `Unauthorized` connection/employee mismatch,
/// `NotReady` run not Running or storage not attached):
/// - `WorkFileNotFound`: no entry at the path (ENOENT/ENOTDIR on resolution).
/// - `WorkFileRefused`: symlink, magic link, mount crossing, not S_ISREG,
///   nlink ≠ 1, foreign owner or device (H2 checks).
/// - `WorkFileTooLarge`: size > `MAX_WORK_FILE_BYTES`.
/// - `WorkspaceBrokerUnavailable`: broker or anchor not available on this
///   host (readiness reason `workspace_broker_unavailable`, H16).
///
/// Names in `HX_RESERVED_ERROR_CODES` belong to HX4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode { UnsupportedVersion, InvalidRequest, Unauthorized, InvalidFds, QueueFull, QueueExpired, NotFound, NotReady, AlreadyTaken, WouldDeadlock, RuntimeDenied, ResourceLimit, IsolationUnavailable, Internal,
    WorkFileNotFound, WorkFileRefused, WorkFileTooLarge, WorkspaceBrokerUnavailable }

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

/// Versioned through Envelope; only Io and WorkFile carry fds, all other
/// replies carry zero (§4). Result tags in `HX_RESERVED_RESULTS` belong to HX4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpawnResponse {
    Reserved { run: RunHandle, expires_at_unix_ms: u64 },
    Accepted { run: RunHandle, status: RunStatus },
    Status { run: RunHandle, status: RunStatus },
    Io { run: RunHandle, delivery: IoDelivery },
    Ack,
    Error { code: ErrorCode },
    /// Reply to `OpenWorkFile`: exactly one `WorkFileRead` fd.
    WorkFile { run: RunHandle, file: WorkFileMetadata },
    /// Reply to `WorkAnchor`: no fd.
    WorkAnchor { employee_id: EmployeeId, anchor: WorkAnchorIdentity },
}
impl SpawnResponse {
    pub fn fd_plan(&self) -> Vec<FdRole> {
        match self {
            Self::Io { delivery, .. } => delivery.fd_plan(),
            Self::WorkFile { .. } => vec![FdRole::WorkFileRead],
            _ => vec![],
        }
    }
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::WorkFile { file, .. } => file.validate(),
            Self::WorkAnchor { anchor, .. } => anchor.validate(),
            _ => Ok(()),
        }
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
            task_ref: None, workspace: None, prompt_bytes: 1, input_fds: List::new(vec![]).unwrap(), pty: false }
    }
    #[test]
    fn pty_activity_is_narrow_and_has_no_rights_or_caller_clock() {
        let request = SpawnRequest::PtyActivity { run: RunHandle(crate::ids::RunId([3; 16])) };
        assert!(request.fd_plan().unwrap().is_empty());
        let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: request.clone() };
        assert_eq!(decode::<SpawnRequest>(&encode(&envelope).unwrap()).unwrap(), envelope);
        for name in ["timestamp", "bytes", "uid", "employee_id", "profile", "deadline"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[name] = 42.into();
            assert!(serde_json::from_value::<SpawnRequest>(value).is_err());
        }
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
    fn workspace_selector_is_additive_bounded_and_has_a_pinned_fd_role() {
        let legacy = serde_json::to_value(start()).unwrap();
        assert!(legacy.get("workspace").is_none());
        assert!(serde_json::from_value::<SpawnRequest>(legacy).is_ok());
        let selection = WorkspaceSelection { handle: Text::new(format!("fork-{}", "a".repeat(32))).unwrap(),
            relative_cwd: Text::new("src/tests".into()).unwrap(), device: 7, inode: 9 };
        assert_eq!(selection.storage_relative().unwrap(), format!(".forks/{}/src/tests", "a".repeat(32)));
        let mut request = start();
        if let SpawnRequest::StartRun { workspace, .. } = &mut request { *workspace = Some(selection.clone()); }
        assert_eq!(request.fd_plan().unwrap(), vec![FdRole::PromptSealed, FdRole::WorkspaceDirectory]);
        for cwd in ["/etc", "../escape", "a/../b", "a//b", "a\\b", "a/."] {
            let mut bad = selection.clone(); bad.relative_cwd = Text::new(cwd.into()).unwrap();
            assert!(bad.validate().is_err(), "{cwd}");
        }
        for handle in ["/work", "fork-../etc", "fork-ABCDEF", "employee-work-extra"] {
            let mut bad = selection.clone(); bad.handle = Text::new(handle.into()).unwrap();
            assert!(bad.validate().is_err(), "{handle}");
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
    fn run3() -> RunHandle { RunHandle(crate::ids::RunId([3; 16])) }
    fn open(path: &str) -> SpawnRequest {
        SpawnRequest::OpenWorkFile { run: run3(), employee_id: EmployeeId([1; 16]),
            path: WorkPath::new(path.into()).unwrap(), mode: WorkOpenMode::ReadOnly }
    }
    fn work_file() -> SpawnResponse {
        SpawnResponse::WorkFile { run: run3(), file: WorkFileMetadata { size: 12, inode: 9, device: 7 } }
    }
    fn anchor_reply() -> SpawnResponse {
        SpawnResponse::WorkAnchor { employee_id: EmployeeId([1; 16]), anchor: WorkAnchorIdentity { device: 7, inode: 5 } }
    }
    /// Exactly `len` bytes, components of ≤ 200 bytes, never an empty component.
    fn path_of_len(len: usize) -> String {
        let mut path = String::new();
        while path.len() < len {
            if !path.is_empty() { path.push('/'); }
            path.push_str(&"x".repeat((len - path.len()).clamp(1, 200)));
        }
        assert_eq!(path.len(), len);
        path
    }
    fn every_request() -> Vec<SpawnRequest> {
        vec![SpawnRequest::ReserveRun { employee_id: EmployeeId([1; 16]) }, start(),
            SpawnRequest::Resize { run: run3(), size: PtySize { rows: 24, columns: 80 } },
            SpawnRequest::PtyActivity { run: run3() }, SpawnRequest::Cancel { run: run3() },
            SpawnRequest::Wait { run: run3(), caller_run: None }, SpawnRequest::Status { run: run3() },
            SpawnRequest::TakeIo { run: run3() }, open("a.csv"),
            SpawnRequest::WorkAnchor { employee_id: EmployeeId([1; 16]) }]
    }

    #[test]
    fn h1_open_work_file_codec_roundtrip() {
        let request = open("報告/2026/q1.xlsx");
        let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: request.clone() };
        assert_eq!(decode::<SpawnRequest>(&encode(&envelope).unwrap()).unwrap(), envelope);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["op"], "open_work_file");
        assert_eq!(value["mode"], "read_only");
        assert_eq!(value["path"], "報告/2026/q1.xlsx");
        assert_eq!(request.opcode(), Opcode::OpenWorkFile);
        assert!(request.fd_plan().unwrap().is_empty(), "the request carries no fd");
        let anchor = Envelope { version: 1, request_id: RequestId([2; 16]),
            message: SpawnRequest::WorkAnchor { employee_id: EmployeeId([1; 16]) } };
        assert_eq!(decode::<SpawnRequest>(&encode(&anchor).unwrap()).unwrap(), anchor);
        assert!(anchor.message.fd_plan().unwrap().is_empty());
        for reply in [work_file(), anchor_reply()] {
            let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: reply };
            assert_eq!(decode::<SpawnResponse>(&encode(&envelope).unwrap()).unwrap(), envelope);
        }
        assert_eq!(serde_json::to_value(work_file()).unwrap()["result"], "work_file");
        assert_eq!(serde_json::to_value(anchor_reply()).unwrap()["result"], "work_anchor");
        // Strict schema: no host-chosen flags, fds, uids, absolute roots or write modes.
        for forbidden in ["flags", "dirfd", "uid", "root", "mount", "follow_symlinks", "create"] {
            let mut value = serde_json::to_value(open("a")).unwrap();
            value[forbidden] = "unsafe".into();
            assert!(serde_json::from_value::<SpawnRequest>(value).is_err(), "{forbidden}");
        }
        for mode in ["read_write", "write_only", "append", "ReadOnly", ""] {
            let mut value = serde_json::to_value(open("a")).unwrap();
            value["mode"] = mode.into();
            assert!(serde_json::from_value::<SpawnRequest>(value).is_err(), "{mode}");
        }
        for missing in ["run", "employee_id", "path", "mode"] {
            let mut value = serde_json::to_value(open("a")).unwrap();
            value.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<SpawnRequest>(value).is_err(), "{missing}");
        }
        let mut extra = serde_json::to_value(work_file()).unwrap();
        extra["file"]["path"] = "a".into();
        assert!(serde_json::from_value::<SpawnResponse>(extra).is_err());
        let mut extra = serde_json::to_value(anchor_reply()).unwrap();
        extra["anchor"]["path"] = "/var/lib".into();
        assert!(serde_json::from_value::<SpawnResponse>(extra).is_err());
        // Debug never prints the employee's file name.
        assert!(!format!("{request:?}").contains("q1.xlsx"));
    }

    #[test]
    fn h1_rejects_unsafe_paths() {
        let unsafe_paths = ["", "/", "/etc/passwd", "//a", "..", "../x", "a/../b", "a/..", ".", "./a",
            "a/.", "a//b", "a/", "a\0b", "a\\b", "..\\x", "a\nb", "a\rb", "a\u{7f}b", "a\tb",
            ".forks", ".forks/abc/x"];
        for path in unsafe_paths {
            assert!(WorkPath::new(path.into()).is_err(), "{path:?}");
            let mut value = serde_json::to_value(open("a")).unwrap();
            value["path"] = path.into();
            assert!(serde_json::from_value::<SpawnRequest>(value.clone()).is_err(), "{path:?}");
            let frame = frame_payload(&serde_json::to_vec(&serde_json::json!({
                "version": 1, "request_id": RequestId([2; 16]), "message": value })).unwrap()).unwrap();
            assert_eq!(decode::<SpawnRequest>(&frame), Err(ContractError::InvalidFrame), "{path:?}");
        }
        for path in ["a", "dir/file.csv", "報告/q1.xlsx", ".hidden", "a..b", "...", "x/.forks", "forks/a", "a b/c-d_e.txt"] {
            let parsed = WorkPath::new(path.into()).unwrap();
            assert_eq!(parsed.as_str(), path);
            assert!(open(path).validate().is_ok(), "{path}");
        }
        assert_eq!(WorkPath::new("a/b/c".into()).unwrap().components().collect::<Vec<_>>(), ["a", "b", "c"]);
    }

    #[test]
    fn h1_response_carries_exactly_one_fd() {
        let request = open("a.csv");
        assert_eq!(work_file().fd_plan(), vec![FdRole::WorkFileRead]);
        assert_eq!(check_work_reply(&request, &work_file(), 1), Ok(()));
        for count in [0, 2, MAX_INPUT_FDS] {
            assert_eq!(check_work_reply(&request, &work_file(), count), Err(ContractError::InvalidFrame), "{count}");
        }
        let anchor = SpawnRequest::WorkAnchor { employee_id: EmployeeId([1; 16]) };
        assert!(anchor_reply().fd_plan().is_empty());
        assert_eq!(check_work_reply(&anchor, &anchor_reply(), 0), Ok(()));
        assert!(check_work_reply(&anchor, &anchor_reply(), 1).is_err());
        let refused = SpawnResponse::Error { code: ErrorCode::WorkFileRefused };
        assert_eq!(check_work_reply(&request, &refused, 0), Ok(()));
        assert!(check_work_reply(&request, &refused, 1).is_err(), "an error never carries an fd");
        // Pairing: wrong run, wrong employee, wrong reply kind, non-work request.
        let other_run = SpawnResponse::WorkFile { run: RunHandle(crate::ids::RunId([4; 16])),
            file: WorkFileMetadata { size: 12, inode: 9, device: 7 } };
        assert!(check_work_reply(&request, &other_run, 1).is_err());
        let other_employee = SpawnRequest::WorkAnchor { employee_id: EmployeeId([2; 16]) };
        assert!(check_work_reply(&other_employee, &anchor_reply(), 0).is_err());
        assert!(check_work_reply(&request, &anchor_reply(), 0).is_err());
        assert!(check_work_reply(&anchor, &work_file(), 1).is_err());
        assert!(check_work_reply(&request, &SpawnResponse::Ack, 0).is_err());
        assert!(check_work_reply(&request, &SpawnResponse::Io { run: run3(),
            delivery: IoDelivery { mcp: false, pty: false } }, 3).is_err());
        assert!(check_work_reply(&SpawnRequest::Status { run: run3() }, &work_file(), 1).is_err());
        // Every other reply keeps its existing fd count; only Io and WorkFile carry fds.
        for reply in [SpawnResponse::Ack, refused, anchor_reply(),
            SpawnResponse::Status { run: run3(), status: RunStatus::Running }] {
            assert!(reply.fd_plan().is_empty());
        }
    }

    #[test]
    fn h1_opcode_space_reserved_for_hx() {
        let mut codes = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        for op in Opcode::ALL {
            assert!(codes.insert(op.code()) && names.insert(op.wire_name()), "{op:?} duplicated");
            assert_eq!(Opcode::from_code(op.code()), Some(op));
            assert!(OPCODES_CORE.contains(&op.code()) || OPCODES_WORK.contains(&op.code())
                || OPCODES_HELPER.contains(&op.code()), "{op:?} outside every range");
        }
        assert!(OPCODES_WORK.contains(&Opcode::OpenWorkFile.code()) && OPCODES_WORK.contains(&Opcode::WorkAnchor.code()));
        for op in Opcode::ALL.into_iter().filter(|op| !matches!(op, Opcode::OpenWorkFile | Opcode::WorkAnchor)) {
            assert!(OPCODES_CORE.contains(&op.code()) || OPCODES_HELPER.contains(&op.code()), "{op:?}");
        }
        // Every request maps to its registry entry, and the registry name is the wire tag.
        let requests = every_request();
        assert_eq!(requests.len(), Opcode::ALL.len(), "every_request must cover every opcode");
        for request in requests {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["op"], request.opcode().wire_name());
        }
        // HX reservations: inside the helper range, distinct, and either still
        // unassigned or assigned with exactly the reserved name (HX4).
        for (index, reserved) in HX_RESERVED_OPS.iter().enumerate() {
            assert!(OPCODES_HELPER.contains(&reserved.code), "{reserved:?}");
            assert!(HX_RESERVED_OPS[..index].iter().all(|r| r.code != reserved.code && r.op != reserved.op));
            match Opcode::from_code(reserved.code) {
                Some(op) => assert_eq!(op.wire_name(), reserved.op),
                None => assert!(Opcode::ALL.iter().all(|op| op.wire_name() != reserved.op), "{reserved:?}"),
            }
        }
        for op in Opcode::ALL.into_iter().filter(|op| OPCODES_HELPER.contains(&op.code())) {
            assert!(HX_RESERVED_OPS.iter().any(|r| r.code == op.code()), "unreserved helper opcode {op:?}");
        }
        // Package H claims none of HX's result tags or error-code names.
        for tag in ["work_file", "work_anchor"] { assert!(!HX_RESERVED_RESULTS.contains(&tag)); }
        for code in [ErrorCode::WorkFileNotFound, ErrorCode::WorkFileRefused, ErrorCode::WorkFileTooLarge,
            ErrorCode::WorkspaceBrokerUnavailable] {
            let name = serde_json::to_value(code).unwrap();
            assert!(!HX_RESERVED_ERROR_CODES.contains(&name.as_str().unwrap()), "{name}");
        }
    }

    #[test]
    fn h1_bounds_enforced() {
        assert!(WorkPath::new(path_of_len(MAX_WORK_PATH_BYTES)).is_ok());
        assert_eq!(WorkPath::new(path_of_len(MAX_WORK_PATH_BYTES + 1)), Err(ContractError::LimitExceeded("work path")));
        let components = |count: usize| vec!["a"; count].join("/");
        assert!(WorkPath::new(components(MAX_WORK_PATH_COMPONENTS)).is_ok());
        assert_eq!(WorkPath::new(components(MAX_WORK_PATH_COMPONENTS + 1)),
            Err(ContractError::LimitExceeded("work path components")));
        assert!(WorkPath::new("y".repeat(MAX_WORK_COMPONENT_BYTES)).is_ok());
        assert_eq!(WorkPath::new("y".repeat(MAX_WORK_COMPONENT_BYTES + 1)),
            Err(ContractError::LimitExceeded("work path component")));
        // Byte, not char, limits: 85 × 3-byte chars = 255 bytes; 86 overflows.
        assert!(WorkPath::new("報".repeat(85)).is_ok());
        assert!(WorkPath::new("報".repeat(86)).is_err());
        let mut value = serde_json::to_value(open("a")).unwrap();
        value["path"] = path_of_len(MAX_WORK_PATH_BYTES + 1).into();
        assert!(serde_json::from_value::<SpawnRequest>(value).is_err());
        // The largest legal request is far below the frame cap.
        let largest = Envelope { version: 1, request_id: RequestId([2; 16]), message: open(&path_of_len(MAX_WORK_PATH_BYTES)) };
        assert!(encode(&largest).unwrap().len() < MAX_FRAME_BYTES / 16);
        // Reply metadata bounds.
        let file = |size, inode, device| WorkFileMetadata { size, inode, device };
        assert!(file(MAX_WORK_FILE_BYTES, 1, 1).validate().is_ok());
        assert!(file(0, 1, 1).validate().is_ok());
        assert_eq!(file(MAX_WORK_FILE_BYTES + 1, 1, 1).validate(), Err(ContractError::LimitExceeded("work file")));
        assert!(file(1, 0, 1).validate().is_err() && file(1, 1, 0).validate().is_err());
        assert!(WorkAnchorIdentity { device: 0, inode: 1 }.validate().is_err());
        assert!(WorkAnchorIdentity { device: 1, inode: 0 }.validate().is_err());
        let oversized = SpawnResponse::WorkFile { run: run3(), file: file(u64::MAX, 1, 1) };
        assert!(check_work_reply(&open("a"), &oversized, 1).is_err());
        let zero_anchor = SpawnResponse::WorkAnchor { employee_id: EmployeeId([1; 16]),
            anchor: WorkAnchorIdentity { device: 0, inode: 0 } };
        assert!(check_work_reply(&SpawnRequest::WorkAnchor { employee_id: EmployeeId([1; 16]) }, &zero_anchor, 0).is_err());
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
