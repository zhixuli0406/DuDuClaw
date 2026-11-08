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
/// - `OPCODES_WORK` 0x0100–0x01FF: Package H root work broker (D-H1) and the
///   employee-scoped root reads/stops Package H4 added for F6 (tombstone S13,
///   restore/retire idle check): `stop_employee_runs` 0x0103, `employee_idle` 0x0104.
/// - `OPCODES_HELPER` 0x0200–0x02FF: HX in-run helpers (D8 revision, option A).
///   Numbers and names are reserved in `HX_RESERVED_OPS`; HX4 adds the
///   variants with exactly those values. Package H must not use this range.
///
/// A gateway sends a Package H op only when spawnd advertises
/// `SpawndFeatures::WORK_BROKER`, and an HX op only with
/// `SpawndFeatures::HELPER_CALLS` (`Opcode::feature`); an older spawnd decodes an
/// unknown `op` as a malformed frame and closes the control connection.
/// `SpawnRequest::opcode` is an exhaustive match, so a new variant without a
/// registry entry does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum Opcode {
    ReserveRun = 0x0001, StartRun = 0x0002, Resize = 0x0003, PtyActivity = 0x0004,
    Cancel = 0x0005, Wait = 0x0006, Status = 0x0007, TakeIo = 0x0008,
    OpenWorkFile = 0x0101, WorkAnchor = 0x0102, StopEmployeeRuns = 0x0103, EmployeeIdle = 0x0104,
    /// HX (HX3a), numbers and names exactly as reserved in `HX_RESERVED_OPS`.
    HelperCall = 0x0201, HelperWait = 0x0202, HelperStop = 0x0203, HelperCancel = 0x0204,
}
pub const OPCODES_CORE: RangeInclusive<u16> = 0x0001..=0x00ff;
pub const OPCODES_WORK: RangeInclusive<u16> = 0x0100..=0x01ff;
pub const OPCODES_HELPER: RangeInclusive<u16> = 0x0200..=0x02ff;
impl Opcode {
    pub const ALL: [Self; 16] = [Self::ReserveRun, Self::StartRun, Self::Resize, Self::PtyActivity,
        Self::Cancel, Self::Wait, Self::Status, Self::TakeIo, Self::OpenWorkFile, Self::WorkAnchor,
        Self::StopEmployeeRuns, Self::EmployeeIdle,
        Self::HelperCall, Self::HelperWait, Self::HelperStop, Self::HelperCancel];
    /// Package H ops (`OPCODES_WORK`), gated by `SpawndFeatures::WORK_BROKER`.
    pub const WORK: [Self; 4] = [Self::OpenWorkFile, Self::WorkAnchor, Self::StopEmployeeRuns, Self::EmployeeIdle];
    /// HX ops (`OPCODES_HELPER`), gated by `SpawndFeatures::HELPER_CALLS`.
    pub const HELPER: [Self; 4] = [Self::HelperCall, Self::HelperWait, Self::HelperStop, Self::HelperCancel];
    /// The feature bit a gateway must see before it sends this op; core ops need none.
    pub const fn feature(self) -> SpawndFeatures {
        match self {
            Self::OpenWorkFile | Self::WorkAnchor | Self::StopEmployeeRuns | Self::EmployeeIdle => SpawndFeatures::WORK_BROKER,
            Self::HelperCall | Self::HelperWait | Self::HelperStop | Self::HelperCancel => SpawndFeatures::HELPER_CALLS,
            _ => SpawndFeatures::NONE,
        }
    }
    pub const fn code(self) -> u16 { self as u16 }
    /// The serde `op` tag of the matching `SpawnRequest` variant.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::ReserveRun => "reserve_run", Self::StartRun => "start_run", Self::Resize => "resize",
            Self::PtyActivity => "pty_activity", Self::Cancel => "cancel", Self::Wait => "wait",
            Self::Status => "status", Self::TakeIo => "take_io",
            Self::OpenWorkFile => "open_work_file", Self::WorkAnchor => "work_anchor",
            Self::StopEmployeeRuns => "stop_employee_runs", Self::EmployeeIdle => "employee_idle",
            Self::HelperCall => "helper_call", Self::HelperWait => "helper_wait",
            Self::HelperStop => "helper_stop", Self::HelperCancel => "helper_cancel",
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

/// HX §2.2/§2.5 bounds. A helper never runs longer than this (check 3), and
/// spawnd refuses a call whose remaining run time leaves less than
/// `MIN_HELPER_RUN_REMAINDER_MS` with `RunTimeExhausted`.
pub const MAX_HELPER_TIMEOUT_MS: u64 = 300_000;
pub const MIN_HELPER_RUN_REMAINDER_MS: u64 = 5_000;
/// The sealed helper input memfd has the prompt bound.
pub const MAX_HELPER_INPUT_BYTES: u64 = MAX_PROMPT_BYTES;
/// Readiness reason a gateway reports when spawnd does not advertise
/// `SpawndFeatures::HELPER_CALLS` [F17]: `helper_launcher_unavailable{reason}`.
pub const HELPER_FEATURE_MISSING_REASON: &str = "spawnd_feature_missing";

/// [F17, H4] The one set of protocol feature bits spawnd advertises. The same
/// value, computed in one place (`Host::features`), is sent the same way on
/// both channels a gateway reads: the root readiness report
/// (`EvidenceReport.features`, host-wide, read before any connection) and every
/// `SpawnResponse::Reserved` (authoritative for that control connection, so a
/// spawnd restarted with other bits between readiness and reservation is
/// noticed). Omitted when empty, so an old spawnd reads as "no features"; a
/// gateway that does not see a bit never sends that bit's ops (`Opcode::feature`;
/// an old spawnd would close the connection on the unknown `op`). Unknown bits
/// from a newer spawnd are carried but never tested, so they cannot fail a
/// reservation or a readiness read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpawndFeatures(pub u32);
impl SpawndFeatures {
    pub const NONE: Self = Self(0);
    /// `helper_calls` (HX4): spawnd dispatches `HelperCall`/`HelperWait`/
    /// `HelperStop`/`HelperCancel` AND can run helpers on this host (launcher
    /// plans active, slot cgroups available). Without it every helper op is
    /// refused (`helper_unavailable` / `helper_cgroup_unavailable`).
    pub const HELPER_CALLS: Self = Self(1 << 0);
    /// `work_broker` (H4): spawnd decodes and answers the four Package H ops
    /// (`Opcode::WORK`). A decode bit, not availability: each op still fails
    /// closed (`workspace_broker_unavailable`, …) on a host without the broker.
    pub const WORK_BROKER: Self = Self(1 << 1);
    /// Bit → wire name, for readiness and audit output.
    pub const NAMES: [(Self, &'static str); 2] =
        [(Self::HELPER_CALLS, "helper_calls"), (Self::WORK_BROKER, "work_broker")];
    pub const fn contains(self, feature: Self) -> bool { feature.0 != 0 && self.0 & feature.0 == feature.0 }
    pub const fn with(self, feature: Self) -> Self { Self(self.0 | feature.0) }
    pub const fn without(self, feature: Self) -> Self { Self(self.0 & !feature.0) }
    /// Whether a gateway may send `op` to a spawnd advertising `self`.
    pub const fn allows(self, op: Opcode) -> bool {
        let needed = op.feature();
        needed.0 == 0 || self.contains(needed)
    }
    pub fn is_empty(&self) -> bool { self.0 == 0 }
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMES.into_iter().filter(move |(bit, _)| self.contains(*bit)).map(|(_, name)| name)
    }
}

/// HX §2.2 [F7]: per-run call identity generated by spawnd (16 random bytes,
/// deduplicated within the run) and returned in `HelperStarted`. The gateway
/// never chooses it. All-zero is never generated, so it is refused everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct HelperCallId([u8; 16]);
impl HelperCallId {
    pub fn new(bytes: [u8; 16]) -> Result<Self, ContractError> {
        if bytes == [0; 16] { Err(ContractError::InvalidValue("helper call id")) } else { Ok(Self(bytes)) }
    }
    pub const fn bytes(&self) -> [u8; 16] { self.0 }
    /// 32 lowercase hex digits (the recording job id format, `recording_helper::valid_id`).
    pub fn hex(&self) -> String { self.0.iter().map(|b| format!("{b:02x}")).collect() }
}
impl<'de> Deserialize<'de> for HelperCallId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<[u8; 16]>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// `hx/fg` or `hx/rec` (Q4 = A: one of each per run). Shared with the signed
/// helper policy (`duduclaw_run::helper_policy` re-exports this type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperSlot { Foreground, Recording }

/// Root-determined end of one helper call (HX §5). spawnd derives it from
/// cgroup events, its watchdog and `memory.events`; launcher frames are hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum HelperOutcome { Exited(i32), Signalled(u32), TimedOut, OomKilled, RunEnded, StartFailed }
impl HelperOutcome {
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::Exited(code) if !(0..=255).contains(code) => Err(ContractError::InvalidValue("helper exit code")),
            Self::Signalled(signal) if !(1..=64).contains(signal) => Err(ContractError::InvalidValue("helper signal")),
            _ => Ok(()),
        }
    }
}

fn validate_helper_timeout(timeout_ms: u64) -> Result<(), ContractError> {
    if timeout_ms == 0 || timeout_ms > MAX_HELPER_TIMEOUT_MS { return Err(ContractError::LimitExceeded("helper timeout")); }
    Ok(())
}

/// Pair an HX reply with its request and the number of received fds. The
/// caller still checks each fd against `FdRole` (pipe read ends, CLOEXEC).
pub fn check_helper_reply(request: &SpawnRequest, response: &SpawnResponse, received_fds: usize)
    -> Result<(), ContractError> {
    request.validate()?;
    response.validate()?;
    if received_fds != response.fd_plan().len() { return Err(ContractError::InvalidFrame); }
    use SpawnRequest as Q;
    use SpawnResponse as R;
    let paired = match (request, response) {
        (Q::HelperCall { .. } | Q::HelperWait { .. } | Q::HelperStop { .. } | Q::HelperCancel { .. }, R::Error { .. }) => true,
        (Q::HelperCall { run, timeout_ms, .. }, R::HelperStarted { run: actual, effective_timeout_ms, .. }) =>
            run == actual && effective_timeout_ms <= timeout_ms,
        (Q::HelperWait { run, call, .. }, R::HelperEnded { run: actual, call: actual_call, .. }) =>
            run == actual && call == actual_call,
        (Q::HelperStop { .. } | Q::HelperCancel { .. }, R::Ack) => true,
        (Q::HelperCall { .. } | Q::HelperWait { .. } | Q::HelperStop { .. } | Q::HelperCancel { .. }, _) => false,
        _ => return Err(ContractError::InvalidValue("not a helper request")),
    };
    if paired { Ok(()) } else { Err(ContractError::InvalidFrame) }
}

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
/// component non-empty, ≤ 255 bytes, not `.`/`..`, no `\` and no
/// `char::is_control` character (C0 incl. NUL, DEL, C1 incl. U+0085 — the
/// same rule as mcp-boundary's `WorkRelativePath`); no leading `/`
/// (absolute), no trailing or doubled `/`; first
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
            || part.chars().any(|c| c == '\\' || c.is_control()) {
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

/// Largest `EmployeeRunsStopped` list. While a stop is pending root refuses new
/// reservations for that employee, so the set is the employee's non-terminal
/// entries when the stop began (≤ `DEFAULT_QUEUE_LENGTH`); 64 leaves headroom.
pub const MAX_STOPPED_RUNS: usize = 64;

/// Root liveness readback of one employee (F6 §3.12 step 4, M8). Counts, not
/// identities; `idle()` only when every count is zero. Root answers an
/// `Error` instead of a report whenever any source cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmployeeIdleReport {
    /// Admission entries that are not terminal, open root run records and
    /// `runs.json` crash-journal records of this employee.
    pub runs: u16,
    /// Durable storage leases (attachments) of this employee.
    pub attachments: u16,
    /// Processes whose uid or gid fields equal the employee's (any of real,
    /// effective, saved, filesystem).
    pub processes: u16,
    /// `duduclaw-run-*` units mapped to this employee, plus every unit root
    /// cannot map to any employee (fail closed).
    pub units: u16,
}
impl EmployeeIdleReport {
    pub fn idle(&self) -> bool { self.runs == 0 && self.attachments == 0 && self.processes == 0 && self.units == 0 }
}

/// Pair a Package H reply with its request and the number of received fds.
/// The caller still checks each fd against `FdRole` and the metadata.
pub fn check_work_reply(request: &SpawnRequest, response: &SpawnResponse, received_fds: usize)
    -> Result<(), ContractError> {
    request.validate()?;
    response.validate()?;
    if received_fds != response.fd_plan().len() { return Err(ContractError::InvalidFrame); }
    let paired = match (request, response) {
        (SpawnRequest::OpenWorkFile { .. } | SpawnRequest::WorkAnchor { .. }
            | SpawnRequest::StopEmployeeRuns { .. } | SpawnRequest::EmployeeIdle { .. }, SpawnResponse::Error { .. }) => true,
        (SpawnRequest::OpenWorkFile { run, .. }, SpawnResponse::WorkFile { run: actual, .. }) => run == actual,
        (SpawnRequest::WorkAnchor { employee_id }, SpawnResponse::WorkAnchor { employee_id: actual, .. }) =>
            employee_id == actual,
        (SpawnRequest::StopEmployeeRuns { employee_id }, SpawnResponse::EmployeeRunsStopped { employee_id: actual, .. }) =>
            employee_id == actual,
        (SpawnRequest::EmployeeIdle { employee_id }, SpawnResponse::EmployeeIdle { employee_id: actual, .. }) =>
            employee_id == actual,
        (SpawnRequest::OpenWorkFile { .. } | SpawnRequest::WorkAnchor { .. }
            | SpawnRequest::StopEmployeeRuns { .. } | SpawnRequest::EmployeeIdle { .. }, _) => false,
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
    /// Package H4 for F6 (opcode 0x0103; tombstone S13). Root cancels every
    /// non-terminal run of the employee, whichever gateway connection owns it
    /// (the same per-run Cancel path), refuses new reservations for it while
    /// the stop is pending, and waits (bounded, below `CONTROL_TIMEOUT_MS`)
    /// until each reached a terminal state and the employee reads idle. Reply:
    /// `EmployeeRunsStopped` (`confirmed = false` after the bounded wait: the
    /// caller records a pending fence and retries; a retry is idempotent), or
    /// `Error` (`NotFound` for an employee root never allocated).
    StopEmployeeRuns { employee_id: EmployeeId },
    /// Package H4 for F6 (opcode 0x0104; F6c restore precheck, F6l retire):
    /// root liveness readback, no side effect. Reply: `EmployeeIdle`, or
    /// `Error` (`NotFound` unknown employee, `IsolationUnavailable` when any
    /// source is unreadable — never a zero report on uncertainty).
    EmployeeIdle { employee_id: EmployeeId },
    /// HX (opcode 0x0201). Run one root-signed helper inside the live run.
    /// Carries exactly one `PromptSealed` fd: the sealed input memfd of
    /// `input_bytes` (spawnd revalidates seals and size). `run` must be Running
    /// and owned by this connection and `employee_id` must equal the run's
    /// admitted employee [F7]; spawnd re-derives the helper from the run's
    /// employee and maps it to an index in the signed table, never trusting
    /// `helper` beyond a lookup key. `timeout_ms` is the tool's own bound; spawnd
    /// replies with the effective value. Reply: `HelperStarted` with
    /// `[StdoutRead, StderrRead]`, or `Error` (`HelperBusy`, `RunTimeExhausted`,
    /// `HelperUnavailable`, `HelperCgroupUnavailable`, …).
    HelperCall { run: RunHandle, employee_id: EmployeeId, helper: RuntimeKind, slot: HelperSlot,
        timeout_ms: u64, input_bytes: u64 },
    /// HX (0x0202). Wait for a started call. Reply: `HelperEnded`, or `Error`.
    /// `employee_id` must equal the run's admitted employee (as for
    /// `HelperCall` [F7]); otherwise `Unauthorized`.
    HelperWait { run: RunHandle, employee_id: EmployeeId, call: HelperCallId },
    /// HX (0x0203). Recording: ask the helper to flush and exit (`stop\n`).
    /// Employee-scoped like `HelperWait`. Reply: `Ack`.
    HelperStop { run: RunHandle, employee_id: EmployeeId, call: HelperCallId },
    /// HX (0x0204). Kill the call's slot cgroup, queued or running.
    /// Employee-scoped like `HelperWait`. Reply: `Ack`.
    HelperCancel { run: RunHandle, employee_id: EmployeeId, call: HelperCallId },
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
            Self::StopEmployeeRuns { .. } => Opcode::StopEmployeeRuns, Self::EmployeeIdle { .. } => Opcode::EmployeeIdle,
            Self::HelperCall { .. } => Opcode::HelperCall, Self::HelperWait { .. } => Opcode::HelperWait,
            Self::HelperStop { .. } => Opcode::HelperStop, Self::HelperCancel { .. } => Opcode::HelperCancel,
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
        if let Self::HelperCall { .. } = self { roles.push(FdRole::PromptSealed); }
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
            Self::HelperCall { helper, timeout_ms, input_bytes, .. } => {
                // Helpers are registered runtime records only, never a built-in AI runtime.
                if !matches!(helper, RuntimeKind::Registered(_)) { return Err(ContractError::InvalidValue("helper runtime")); }
                validate_helper_timeout(*timeout_ms)?;
                if *input_bytes > MAX_HELPER_INPUT_BYTES { return Err(ContractError::LimitExceeded("helper input")); }
            }
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
/// HX additions (names exactly `HX_RESERVED_ERROR_CODES`; existing codes keep
/// their meaning: `Unauthorized` connection/employee mismatch, `NotReady` run
/// not Running, `NotFound` unknown run or call):
/// - `HelperBusy`: slot occupied and the per-run FIFO (≤ 4) full or its deadline passed.
/// - `HelperUnavailable`: no launcher, launcher dead, or control channel closed (F14).
/// - `RunTimeExhausted`: less than `MIN_HELPER_RUN_REMAINDER_MS` of run time remains.
/// - `HelperCgroupUnavailable`: slot cgroups or open-time credentials unavailable (Q2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode { UnsupportedVersion, InvalidRequest, Unauthorized, InvalidFds, QueueFull, QueueExpired, NotFound, NotReady, AlreadyTaken, WouldDeadlock, RuntimeDenied, ResourceLimit, IsolationUnavailable, Internal,
    WorkFileNotFound, WorkFileRefused, WorkFileTooLarge, WorkspaceBrokerUnavailable,
    HelperBusy, HelperUnavailable, RunTimeExhausted, HelperCgroupUnavailable }

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

/// Versioned through Envelope; only Io, WorkFile and HelperStarted carry fds,
/// all other replies carry zero (§4). HX result tags are `HX_RESERVED_RESULTS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpawnResponse {
    Reserved { run: RunHandle, expires_at_unix_ms: u64,
        /// [F17] spawnd capability bits; omitted (and read as none) when empty.
        #[serde(default, skip_serializing_if = "SpawndFeatures::is_empty")]
        features: SpawndFeatures },
    Accepted { run: RunHandle, status: RunStatus },
    Status { run: RunHandle, status: RunStatus },
    Io { run: RunHandle, delivery: IoDelivery },
    Ack,
    Error { code: ErrorCode },
    /// Reply to `OpenWorkFile`: exactly one `WorkFileRead` fd.
    WorkFile { run: RunHandle, file: WorkFileMetadata },
    /// Reply to `WorkAnchor`: no fd.
    WorkAnchor { employee_id: EmployeeId, anchor: WorkAnchorIdentity },
    /// Reply to `StopEmployeeRuns`: the runs root cancelled (or found already
    /// stopping) and whether every one is terminal and the employee is idle.
    EmployeeRunsStopped { employee_id: EmployeeId, runs: List<RunHandle, MAX_STOPPED_RUNS>, confirmed: bool },
    /// Reply to `EmployeeIdle`: no fd.
    EmployeeIdle { employee_id: EmployeeId, report: EmployeeIdleReport },
    /// Reply to `HelperCall`: exactly `[StdoutRead, StderrRead]` (the call's
    /// pipe read ends). `call` is spawnd-generated; `effective_timeout_ms` ≤
    /// the request's `timeout_ms` (min with the binding and the run deadline).
    HelperStarted { run: RunHandle, call: HelperCallId, effective_timeout_ms: u64 },
    /// Reply to `HelperWait`: no fd.
    HelperEnded { run: RunHandle, call: HelperCallId, outcome: HelperOutcome },
}
impl SpawnResponse {
    pub fn fd_plan(&self) -> Vec<FdRole> {
        match self {
            Self::Io { delivery, .. } => delivery.fd_plan(),
            Self::WorkFile { .. } => vec![FdRole::WorkFileRead],
            Self::HelperStarted { .. } => vec![FdRole::StdoutRead, FdRole::StderrRead],
            _ => vec![],
        }
    }
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::WorkFile { file, .. } => file.validate(),
            Self::WorkAnchor { anchor, .. } => anchor.validate(),
            Self::HelperStarted { effective_timeout_ms, .. } => validate_helper_timeout(*effective_timeout_ms),
            Self::HelperEnded { outcome, .. } => outcome.validate(),
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
#[path = "ipc_hxfix_tests.rs"]
mod hxfix_tests;

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
            SpawnRequest::WorkAnchor { employee_id: EmployeeId([1; 16]) },
            SpawnRequest::StopEmployeeRuns { employee_id: EmployeeId([1; 16]) },
            SpawnRequest::EmployeeIdle { employee_id: EmployeeId([1; 16]) },
            helper_call(), SpawnRequest::HelperWait { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() },
            SpawnRequest::HelperStop { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() },
            SpawnRequest::HelperCancel { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() }]
    }
    fn call7() -> HelperCallId { HelperCallId::new([7; 16]).unwrap() }
    fn helper_call() -> SpawnRequest {
        SpawnRequest::HelperCall { run: run3(), employee_id: EmployeeId([1; 16]),
            helper: RuntimeKind::Registered(Text::new("duduclaw-program-v1".into()).unwrap()),
            slot: HelperSlot::Foreground, timeout_ms: 60_000, input_bytes: 12 }
    }
    fn helper_started() -> SpawnResponse {
        SpawnResponse::HelperStarted { run: run3(), call: call7(), effective_timeout_ms: 30_000 }
    }
    fn helper_ended(outcome: HelperOutcome) -> SpawnResponse {
        SpawnResponse::HelperEnded { run: run3(), call: call7(), outcome }
    }

    #[test]
    fn hx3a_helper_ops_use_reserved_numbers_names_and_codes() {
        for reserved in HX_RESERVED_OPS {
            let op = Opcode::from_code(reserved.code).expect("HX op assigned");
            assert_eq!(op.wire_name(), reserved.op);
        }
        for request in every_request().into_iter().filter(|r| OPCODES_HELPER.contains(&r.opcode().code())) {
            let value = serde_json::to_value(&request).unwrap();
            assert!(HX_RESERVED_OPS.iter().any(|r| r.op == value["op"] && r.code == request.opcode().code()));
        }
        let tags: Vec<_> = [helper_started(), helper_ended(HelperOutcome::TimedOut)].iter()
            .map(|r| serde_json::to_value(r).unwrap()["result"].as_str().unwrap().to_owned()).collect();
        assert_eq!(tags, HX_RESERVED_RESULTS);
        let codes: Vec<_> = [ErrorCode::HelperBusy, ErrorCode::HelperUnavailable, ErrorCode::RunTimeExhausted,
            ErrorCode::HelperCgroupUnavailable].iter()
            .map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_owned()).collect();
        assert_eq!(codes, HX_RESERVED_ERROR_CODES);
    }

    #[test]
    fn hx3a_helper_call_codec_and_strict_schema() {
        for request in every_request().into_iter().filter(|r| OPCODES_HELPER.contains(&r.opcode().code())) {
            let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: request };
            assert_eq!(decode::<SpawnRequest>(&encode(&envelope).unwrap()).unwrap(), envelope);
        }
        for reply in [helper_started(), helper_ended(HelperOutcome::Exited(0)), helper_ended(HelperOutcome::OomKilled)] {
            let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: reply };
            assert_eq!(decode::<SpawnResponse>(&encode(&envelope).unwrap()).unwrap(), envelope);
        }
        let call = helper_call();
        assert_eq!(call.fd_plan().unwrap(), vec![FdRole::PromptSealed], "exactly the sealed input memfd");
        for other in [SpawnRequest::HelperWait { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() },
            SpawnRequest::HelperStop { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() }, SpawnRequest::HelperCancel { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() }] {
            assert!(other.fd_plan().unwrap().is_empty());
        }
        // No names, paths, argv, env, index, uid or caller-chosen call id on the wire.
        for forbidden in ["argv", "env", "path", "cwd", "index", "uid", "call", "profile", "seccomp", "deadline"] {
            let mut value = serde_json::to_value(helper_call()).unwrap();
            value[forbidden] = 1.into();
            assert!(serde_json::from_value::<SpawnRequest>(value).is_err(), "{forbidden}");
        }
        for missing in ["run", "employee_id", "helper", "slot", "timeout_ms", "input_bytes"] {
            let mut value = serde_json::to_value(helper_call()).unwrap();
            value.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<SpawnRequest>(value).is_err(), "{missing}");
        }
        let mut value = serde_json::to_value(helper_call()).unwrap();
        value["slot"] = "background".into();
        assert!(serde_json::from_value::<SpawnRequest>(value).is_err());
        // Bounds: built-in runtimes are never helpers; timeout 1..=300 s; input ≤ 16 MiB.
        let with = |helper: RuntimeKind, timeout_ms: u64, input_bytes: u64| SpawnRequest::HelperCall { run: run3(),
            employee_id: EmployeeId([1; 16]), helper, slot: HelperSlot::Recording, timeout_ms, input_bytes };
        let registered = || RuntimeKind::Registered(Text::new("duduclaw-browser-record-v1".into()).unwrap());
        assert!(with(registered(), MAX_HELPER_TIMEOUT_MS, MAX_HELPER_INPUT_BYTES).validate().is_ok());
        for bad in [with(RuntimeKind::Claude, 1, 0), with(RuntimeKind::Codex, 1, 0), with(registered(), 0, 0),
            with(registered(), MAX_HELPER_TIMEOUT_MS + 1, 0), with(registered(), 1, MAX_HELPER_INPUT_BYTES + 1)] {
            assert!(bad.validate().is_err() && bad.fd_plan().is_err(), "{bad:?}");
            let frame = frame_payload(&serde_json::to_vec(&Envelope { version: 1, request_id: RequestId([2; 16]),
                message: bad }).unwrap()).unwrap();
            // Structural decode succeeds; the server's fd_plan/validate refuses it before any fd use.
            assert!(decode::<SpawnRequest>(&frame).unwrap().message.validate().is_err());
        }
        // Call ids: the all-zero id is unrepresentable, also on the wire.
        assert!(HelperCallId::new([0; 16]).is_err());
        let mut value = serde_json::to_value(SpawnRequest::HelperWait { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() }).unwrap();
        value["call"] = serde_json::to_value([0u8; 16]).unwrap();
        assert!(serde_json::from_value::<SpawnRequest>(value).is_err());
        assert_eq!(call7().hex(), "07".repeat(16));
        // Outcomes are bounded wait statuses.
        for outcome in [HelperOutcome::Exited(256), HelperOutcome::Exited(-1), HelperOutcome::Signalled(0), HelperOutcome::Signalled(65)] {
            assert!(helper_ended(outcome).validate().is_err(), "{outcome:?}");
        }
        for timeout in [0, MAX_HELPER_TIMEOUT_MS + 1] {
            assert!(SpawnResponse::HelperStarted { run: run3(), call: call7(), effective_timeout_ms: timeout }.validate().is_err());
        }
    }

    #[test]
    fn hx3a_helper_reply_pairing_and_fd_counts() {
        let call = helper_call();
        assert_eq!(helper_started().fd_plan(), vec![FdRole::StdoutRead, FdRole::StderrRead]);
        assert_eq!(check_helper_reply(&call, &helper_started(), 2), Ok(()));
        for count in [0, 1, 3] { assert!(check_helper_reply(&call, &helper_started(), count).is_err(), "{count}"); }
        for code in [ErrorCode::HelperBusy, ErrorCode::RunTimeExhausted, ErrorCode::HelperCgroupUnavailable] {
            assert_eq!(check_helper_reply(&call, &SpawnResponse::Error { code }, 0), Ok(()));
            assert!(check_helper_reply(&call, &SpawnResponse::Error { code }, 1).is_err());
        }
        // effective ≤ requested; reply run must match.
        let longer = SpawnResponse::HelperStarted { run: run3(), call: call7(), effective_timeout_ms: 60_001 };
        assert!(check_helper_reply(&call, &longer, 2).is_err());
        let other_run = SpawnResponse::HelperStarted { run: RunHandle(crate::ids::RunId([4; 16])), call: call7(),
            effective_timeout_ms: 1 };
        assert!(check_helper_reply(&call, &other_run, 2).is_err());
        let wait = SpawnRequest::HelperWait { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() };
        assert_eq!(check_helper_reply(&wait, &helper_ended(HelperOutcome::RunEnded), 0), Ok(()));
        assert!(check_helper_reply(&wait, &helper_ended(HelperOutcome::RunEnded), 1).is_err());
        let other_call = SpawnResponse::HelperEnded { run: run3(), call: HelperCallId::new([8; 16]).unwrap(),
            outcome: HelperOutcome::Exited(0) };
        assert!(check_helper_reply(&wait, &other_call, 0).is_err());
        for request in [SpawnRequest::HelperStop { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() }, SpawnRequest::HelperCancel { run: run3(), employee_id: EmployeeId([1; 16]), call: call7() }] {
            assert_eq!(check_helper_reply(&request, &SpawnResponse::Ack, 0), Ok(()));
            assert!(check_helper_reply(&request, &helper_started(), 2).is_err());
        }
        assert!(check_helper_reply(&call, &SpawnResponse::Ack, 0).is_err());
        assert!(check_helper_reply(&wait, &helper_started(), 2).is_err());
        assert!(check_helper_reply(&SpawnRequest::Status { run: run3() }, &helper_started(), 2).is_err());
        // Work replies keep their own pairing; helper replies never pair with work requests.
        assert!(check_work_reply(&open("a"), &helper_started(), 2).is_err());
    }

    #[test]
    fn hx3a_helper_calls_feature_bit_is_additive() {
        let run = run3();
        let old = SpawnResponse::Reserved { run, expires_at_unix_ms: 9, features: SpawndFeatures::NONE };
        let value = serde_json::to_value(&old).unwrap();
        assert!(value.get("features").is_none(), "an empty set is omitted, as an old spawnd sends it");
        assert_eq!(serde_json::from_value::<SpawnResponse>(value).unwrap(), old);
        let new = SpawnResponse::Reserved { run, expires_at_unix_ms: 9, features: SpawndFeatures::NONE.with(SpawndFeatures::HELPER_CALLS) };
        let value = serde_json::to_value(&new).unwrap();
        assert_eq!(value["features"], 1);
        let SpawnResponse::Reserved { features, .. } = serde_json::from_value::<SpawnResponse>(value).unwrap() else { panic!() };
        assert!(features.contains(SpawndFeatures::HELPER_CALLS));
        assert_eq!(features.names().collect::<Vec<_>>(), ["helper_calls"]);
        assert!(!SpawndFeatures::NONE.contains(SpawndFeatures::HELPER_CALLS));
        assert!(!SpawndFeatures::HELPER_CALLS.contains(SpawndFeatures::NONE), "the empty set is never a feature");
        // An unknown future bit neither fails the reservation nor reads as helper_calls.
        let future: SpawnResponse = serde_json::from_value(serde_json::json!({
            "result": "reserved", "run": run, "expires_at_unix_ms": 9, "features": 1 << 7 })).unwrap();
        let SpawnResponse::Reserved { features, .. } = future else { panic!() };
        assert!(!features.contains(SpawndFeatures::HELPER_CALLS));
        assert_eq!(HELPER_FEATURE_MISSING_REASON, "spawnd_feature_missing");
    }

    #[test]
    fn hx4_one_feature_set_gates_work_and_helper_ops() {
        // One bitset carries both bits; names are stable and distinct.
        let both = SpawndFeatures::NONE.with(SpawndFeatures::WORK_BROKER).with(SpawndFeatures::HELPER_CALLS);
        assert_eq!(both.names().collect::<Vec<_>>(), ["helper_calls", "work_broker"]);
        assert_eq!(serde_json::to_value(both).unwrap(), 3);
        assert_eq!(both.without(SpawndFeatures::HELPER_CALLS), SpawndFeatures::WORK_BROKER);
        // Every op maps to exactly the bit of its range; core ops need none.
        for op in Opcode::ALL {
            let expected = if OPCODES_WORK.contains(&op.code()) { SpawndFeatures::WORK_BROKER }
                else if OPCODES_HELPER.contains(&op.code()) { SpawndFeatures::HELPER_CALLS }
                else { SpawndFeatures::NONE };
            assert_eq!(op.feature(), expected, "{op:?}");
            assert!(both.allows(op));
            assert_eq!(SpawndFeatures::NONE.allows(op), expected == SpawndFeatures::NONE, "{op:?}");
        }
        for op in Opcode::WORK {
            assert!(SpawndFeatures::WORK_BROKER.allows(op) && !SpawndFeatures::HELPER_CALLS.allows(op));
        }
        for op in Opcode::HELPER {
            assert!(SpawndFeatures::HELPER_CALLS.allows(op) && !SpawndFeatures::WORK_BROKER.allows(op));
        }
        // Reserved carries the same value the readiness report carries.
        let reserved = SpawnResponse::Reserved { run: run3(), expires_at_unix_ms: 9, features: both };
        assert_eq!(serde_json::to_value(&reserved).unwrap()["features"], 3);
    }

    #[test]
    fn h4_employee_ops_codec_and_pairing() {
        let employee = EmployeeId([1; 16]);
        let stop = SpawnRequest::StopEmployeeRuns { employee_id: employee };
        let idle = SpawnRequest::EmployeeIdle { employee_id: employee };
        assert_eq!(stop.opcode().code(), 0x0103);
        assert_eq!(idle.opcode().code(), 0x0104);
        for request in [stop.clone(), idle.clone()] {
            assert!(request.fd_plan().unwrap().is_empty());
            let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: request.clone() };
            assert_eq!(decode::<SpawnRequest>(&encode(&envelope).unwrap()).unwrap(), envelope);
            // No caller-chosen run list, uid, deadline or force flag.
            for forbidden in ["runs", "uid", "deadline_ms", "force", "connection"] {
                let mut value = serde_json::to_value(&request).unwrap();
                value[forbidden] = 1.into();
                assert!(serde_json::from_value::<SpawnRequest>(value).is_err(), "{forbidden}");
            }
        }
        let stopped = SpawnResponse::EmployeeRunsStopped { employee_id: employee,
            runs: List::new(vec![run3()]).unwrap(), confirmed: true };
        let report = EmployeeIdleReport { runs: 0, attachments: 0, processes: 0, units: 0 };
        assert!(report.idle());
        for busy in [EmployeeIdleReport { runs: 1, ..report }, EmployeeIdleReport { attachments: 1, ..report },
            EmployeeIdleReport { processes: 1, ..report }, EmployeeIdleReport { units: 1, ..report }] {
            assert!(!busy.idle());
        }
        let idle_reply = SpawnResponse::EmployeeIdle { employee_id: employee, report };
        for reply in [stopped.clone(), idle_reply.clone()] {
            assert!(reply.fd_plan().is_empty());
            let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: reply };
            assert_eq!(decode::<SpawnResponse>(&encode(&envelope).unwrap()).unwrap(), envelope);
        }
        assert_eq!(serde_json::to_value(&stopped).unwrap()["result"], "employee_runs_stopped");
        assert_eq!(check_work_reply(&stop, &stopped, 0), Ok(()));
        assert_eq!(check_work_reply(&idle, &idle_reply, 0), Ok(()));
        assert!(check_work_reply(&stop, &idle_reply, 0).is_err());
        assert!(check_work_reply(&idle, &stopped, 0).is_err());
        assert!(check_work_reply(&SpawnRequest::StopEmployeeRuns { employee_id: EmployeeId([2; 16]) }, &stopped, 0).is_err());
        assert_eq!(check_work_reply(&stop, &SpawnResponse::Error { code: ErrorCode::NotFound }, 0), Ok(()));
        // The run list is bounded on decode.
        let mut value = serde_json::to_value(&stopped).unwrap();
        value["runs"] = serde_json::Value::Array(vec![serde_json::to_value(run3()).unwrap(); MAX_STOPPED_RUNS + 1]);
        assert!(serde_json::from_value::<SpawnResponse>(value).is_err());
        let mut value = serde_json::to_value(&idle_reply).unwrap();
        value["report"]["pids"] = 1.into();
        assert!(serde_json::from_value::<SpawnResponse>(value).is_err());
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

    /// Copy of `duduclaw_mcp_boundary::resources::WORK_PATH_CONFORMANCE` (H6).
    /// duduclaw-isolation cannot depend on mcp-boundary, so the vectors are
    /// duplicated; keep both lists identical.
    const H6_WORK_PATH_CONFORMANCE: &[(&str, bool)] = &[
        ("a", true), ("notes/員工.md", true), ("a/b/c.csv", true), (".hidden", true), ("..a/b..", true),
        ("a b/c-d_e.txt", true),
        ("", false), (".", false), ("..", false), ("/", false), ("/etc/passwd", false), ("../x", false),
        ("a/../b", false), ("a/./b", false), ("./a", false), ("a/.", false), ("a//b", false), ("a/", false),
        ("a\\b", false), ("a\0b", false), ("a\nb", false), ("a\tb", false), ("a\u{7f}b", false), ("a\u{85}b", false),
    ];

    #[test]
    fn work_path_refuses_unicode_controls() {
        // Every `char::is_control` scalar (C0, DEL, C1 incl. U+0085 NEL) is
        // refused, in any component, both at construction and at decode.
        let controls = (0u32..=0x1f).chain(0x7f..=0x9f).filter_map(char::from_u32);
        for control in controls {
            assert!(control.is_control());
            for path in [format!("a{control}b"), format!("dir/{control}"), format!("{control}/x")] {
                assert!(WorkPath::new(path.clone()).is_err(), "{:?}", path);
                let mut value = serde_json::to_value(open("a")).unwrap();
                value["path"] = path.clone().into();
                assert!(serde_json::from_value::<SpawnRequest>(value).is_err(), "{:?}", path);
            }
        }
        // Not controls: format characters are outside this rule (H6 agrees).
        for path in ["a\u{a0}b", "a\u{200b}b", "名\u{3000}稱"] {
            assert!(WorkPath::new(path.into()).is_ok(), "{path:?}");
        }
    }

    #[test]
    fn work_path_conformance_vectors_match_h6() {
        for (path, ok) in H6_WORK_PATH_CONFORMANCE {
            assert_eq!(WorkPath::new((*path).into()).is_ok(), *ok, "{path:?}");
            let mut value = serde_json::to_value(open("a")).unwrap();
            value["path"] = (*path).into();
            assert_eq!(serde_json::from_value::<SpawnRequest>(value).is_ok(), *ok, "{path:?}");
        }
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
        for op in Opcode::WORK { assert!(OPCODES_WORK.contains(&op.code()), "{op:?}"); }
        for op in Opcode::ALL.into_iter().filter(|op| !Opcode::WORK.contains(op)) {
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
        for tag in ["work_file", "work_anchor", "employee_runs_stopped", "employee_idle"] { assert!(!HX_RESERVED_RESULTS.contains(&tag)); }
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
