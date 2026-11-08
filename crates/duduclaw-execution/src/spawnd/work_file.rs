//! H5: gateway client for spawnd `OpenWorkFile` (D-H1). Root (H2 core, H4
//! wiring) opens one regular file beneath the live run's attached work mount
//! and answers with a sealed, read-only memfd snapshot of it. This client only
//! forwards the run and employee its caller took from the verified capability,
//! on that run's own authenticated control connection, and only while spawnd
//! advertises the `work_broker` feature. It never opens a host path.
//!
//! The `duduclaw_mcp_boundary::resources::WorkBroker` implementation over this
//! client is `duduclaw_mcp_boundary::work_bridge::SpawndWorkspaceBridge` (the
//! boundary already depends on this crate, so the trait impl lives there).
use std::{fs::File, os::fd::{AsRawFd, OwnedFd}, sync::Arc, time::Duration};
use duduclaw_isolation::{execution::{BackendError, RunHandle}, ids::EmployeeId,
    ipc::{self, ErrorCode, SpawnRequest, SpawnResponse, WorkOpenMode, WorkPath}};
use nix::{fcntl::{fcntl, FcntlArg, SealFlag}, sys::stat::{fstat, SFlag}};
use super::SpawndBackend;

/// spawnd readiness feature name for the Package H work ops (H1 follow-up,
/// next to HX's `helper_calls`). H4/H16 publish it; H5 only consumes it.
pub const WORK_BROKER_FEATURE: &str = "work_broker";

/// Root budget for one open: H2 copies ≤ 16 MiB with a 10 s deadline. Longer
/// than `CONTROL_TIMEOUT_MS`; a timeout still closes the run's control socket.
const WORK_FILE_TIMEOUT: Duration = Duration::from_secs(12);

/// HX4/HX5: the one feature set is the `ipc::SpawndFeatures` bitset
/// (`helper_calls` bit 0, `work_broker` bit 1), as in `readiness.json` and
/// every `SpawnResponse::Reserved`.
pub use duduclaw_isolation::ipc::SpawndFeatures;

/// Bits for advertised wire names (`SpawndFeatures::NAMES`). Unknown names (a
/// newer spawnd) enable nothing; names are exact (`WORK_BROKER` is not one).
pub fn features_from_advertised<'a>(names: impl IntoIterator<Item = &'a str>) -> SpawndFeatures {
    names.into_iter().fold(SpawndFeatures::NONE, |features, name| {
        SpawndFeatures::NAMES.into_iter().find(|(_, known)| *known == name)
            .map_or(features, |(bit, _)| features.with(bit))
    })
}

/// The gateway's readiness view (`EvidenceReport.features`), read on every
/// call (spawnd may restart as an older build). An error reads as "no
/// features". It can only narrow what the run's own control connection was
/// told in `Reserved.features`; it never enables a bit root did not send.
pub type FeatureSource = Arc<dyn Fn() -> Result<SpawndFeatures, BackendError> + Send + Sync>;

/// Typed refusal of one brokered open. Carries no path or root diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkFileError {
    /// Unknown run, not Running, storage not attached, or not this employee's run.
    NotLive,
    /// `WorkFileNotFound`.
    NotFound,
    /// `WorkFileRefused` (symlink, mount crossing, not regular, linked, owner,
    /// device) or a path that is not a valid `WorkPath`.
    Refused,
    /// `WorkFileTooLarge`, or larger than the caller's limit.
    TooLarge,
    /// `WorkspaceBrokerUnavailable`, or spawnd does not advertise `work_broker`.
    BrokerUnavailable,
    /// Transport failure or a reply that failed validation (control is closed).
    Unavailable,
}

/// A verified snapshot: sealed (SEAL|SHRINK|GROW|WRITE) read-only memfd,
/// nlink 0, whose fstat equals root's reply. `nlink` is reported as read.
#[derive(Debug)]
pub struct WorkFileSnapshot {
    pub file: File,
    pub size: u64,
    pub device: u64,
    pub inode: u64,
    pub nlink: u64,
}

impl SpawndBackend {
    /// Install the source of spawnd's advertised features (H4/H16). Without
    /// one, every work op is refused locally as `BrokerUnavailable`.
    pub fn with_feature_source(mut self, source: FeatureSource) -> Self {
        self.features = Some(source);
        self
    }

    /// The installed readiness view; no source reads as no features.
    pub fn features(&self) -> SpawndFeatures {
        self.features.as_ref().and_then(|source| source().ok()).unwrap_or(SpawndFeatures::NONE)
    }

    /// Bits usable on `run`'s own control connection: what root sent in that
    /// connection's `Reserved.features`, narrowed by the readiness source when
    /// one is installed. Unknown run: none.
    pub(super) fn connection_features(&self, run: RunHandle) -> SpawndFeatures {
        let Ok(entry) = self.entry(run) else { return SpawndFeatures::NONE };
        if entry.connection.is_closed() { return SpawndFeatures::NONE; }
        match &self.features {
            None => entry.features,
            Some(source) => source().map_or(SpawndFeatures::NONE, |view| SpawndFeatures(view.0 & entry.features.0)),
        }
    }

    /// One brokered open of `path` under `run`'s attached work mount. `run`
    /// and `employee` must come from the caller's verified capability; they
    /// are sent unchanged, on the connection that reserved `run`, and root
    /// decides liveness and ownership. Blocking (≤ 12 s): blocking pool only.
    pub fn open_work_file(&self, run: RunHandle, employee: EmployeeId, path: &str, max_bytes: u64)
        -> Result<WorkFileSnapshot, WorkFileError> {
        let entry = self.entry(run).map_err(|error| match error {
            BackendError::NotFound => WorkFileError::NotLive,
            _ => WorkFileError::Unavailable,
        })?;
        // Never send an op an older spawnd cannot decode (it would drop the
        // run's whole control connection). H5 keeps its default: without an
        // installed readiness source no work op is sent.
        if self.features.is_none() || !self.connection_features(run).allows(ipc::Opcode::OpenWorkFile) {
            return Err(WorkFileError::BrokerUnavailable);
        }
        let path = WorkPath::new(path.to_owned()).map_err(|_| WorkFileError::Refused)?;
        // This gateway reserved `run` for `entry.employee`; another employee's
        // capability never reaches root on this run's connection.
        if entry.employee != employee || entry.connection.is_closed() { return Err(WorkFileError::NotLive); }
        let request = SpawnRequest::OpenWorkFile { run, employee_id: employee, path, mode: WorkOpenMode::ReadOnly };
        // The transport already paired the reply (`check_work_reply`) and
        // checked the fd role; both are re-checked here against the packet.
        let packet = entry.connection.work_request(request.clone(), WORK_FILE_TIMEOUT)
            .map_err(|_| WorkFileError::Unavailable)?;
        if ipc::check_work_reply(&request, &packet.response, packet.fds.len()).is_err() {
            entry.connection.close_now();
            return Err(WorkFileError::Unavailable);
        }
        let file = match packet.response {
            SpawnResponse::Error { code } => return Err(refusal(code)),
            SpawnResponse::WorkFile { run: actual, file } if actual == run => file,
            _ => { entry.connection.close_now(); return Err(WorkFileError::Unavailable); }
        };
        let mut fds = packet.fds.into_iter();
        let (Some(fd), None) = (fds.next(), fds.next()) else {
            entry.connection.close_now();
            return Err(WorkFileError::Unavailable);
        };
        let stat = match sealed_snapshot(&fd) {
            Ok(stat) if (stat.size, stat.device, stat.inode) == (file.size, file.device, file.inode) => stat,
            _ => { entry.connection.close_now(); return Err(WorkFileError::Unavailable); }
        };
        if stat.size > max_bytes.min(ipc::MAX_WORK_FILE_BYTES) { return Err(WorkFileError::TooLarge); }
        Ok(WorkFileSnapshot { file: File::from(fd), size: stat.size, device: stat.device, inode: stat.inode,
            nlink: stat.nlink })
    }
}

/// Root's work-op codes. Existing codes keep their H1 meaning for these ops
/// (`NotFound` unknown run, `Unauthorized` connection/employee, `NotReady`).
fn refusal(code: ErrorCode) -> WorkFileError {
    match code {
        ErrorCode::WorkFileNotFound => WorkFileError::NotFound,
        ErrorCode::WorkFileRefused => WorkFileError::Refused,
        ErrorCode::WorkFileTooLarge => WorkFileError::TooLarge,
        ErrorCode::WorkspaceBrokerUnavailable => WorkFileError::BrokerUnavailable,
        ErrorCode::NotFound | ErrorCode::NotReady | ErrorCode::Unauthorized => WorkFileError::NotLive,
        _ => WorkFileError::Unavailable,
    }
}

pub(super) struct SnapshotStat { pub size: u64, pub device: u64, pub inode: u64, pub nlink: u64 }

/// Every seal H2 adds before handing the snapshot out.
const REQUIRED_SEALS: SealFlag = SealFlag::F_SEAL_SEAL.union(SealFlag::F_SEAL_SHRINK)
    .union(SealFlag::F_SEAL_GROW).union(SealFlag::F_SEAL_WRITE);

/// The `WorkFileRead` role: a sealed memfd snapshot, not a live employee
/// inode. Regular, nlink 0 (memfds are unlinked shmem), ≤ 16 MiB, all four
/// seals (an ordinary or O_TMPFILE tmpfs file cannot acquire them), on tmpfs.
/// Access mode and CLOEXEC are checked by the transport.
pub(super) fn sealed_snapshot(fd: &OwnedFd) -> Result<SnapshotStat, BackendError> {
    let raw = fd.as_raw_fd();
    let stat = fstat(raw).map_err(|_| BackendError::Protocol)?;
    let kind = SFlag::from_bits_retain(stat.st_mode) & SFlag::S_IFMT;
    let size = u64::try_from(stat.st_size).map_err(|_| BackendError::Protocol)?;
    if kind != SFlag::S_IFREG || stat.st_nlink != 0 || size > ipc::MAX_WORK_FILE_BYTES {
        return Err(BackendError::Protocol);
    }
    let seals = fcntl(raw, FcntlArg::F_GET_SEALS).map_err(|_| BackendError::Protocol)?;
    if !SealFlag::from_bits_retain(seals).contains(REQUIRED_SEALS) { return Err(BackendError::Protocol); }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs writes exactly one statfs into the provided storage and
    // reads only the borrowed live descriptor; it is read only after success.
    #[allow(unsafe_code)]
    let filesystem = unsafe {
        if libc::fstatfs(raw, filesystem.as_mut_ptr()) != 0 { return Err(BackendError::Protocol); }
        filesystem.assume_init()
    };
    // f_type's width differs by libc/arch; magic numbers fit 32 bits.
    if filesystem.f_type as u32 != libc::TMPFS_MAGIC as u32 { return Err(BackendError::Protocol); }
    Ok(SnapshotStat { size, device: stat.st_dev as u64, inode: stat.st_ino as u64, nlink: stat.st_nlink as u64 })
}

#[cfg(test)]
#[path = "work_file_tests.rs"]
mod tests;
