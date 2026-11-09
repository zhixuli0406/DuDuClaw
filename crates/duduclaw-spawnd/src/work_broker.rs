//! H2: root work broker core (D-H1). Opens one file beneath the live run's
//! attached `employee-work` mount and hands back a sealed read-only snapshot.
//!
//! Authority, in order, all from root state (never from the gateway):
//! 1. the path satisfies H1 `WorkPath` AND H6 `WorkRelativePath` rules (the
//!    intersection: H6 also refuses C1 controls, H1 also refuses `.forks`);
//! 2. the run is the caller's run, owned by `employee`, Running, with an
//!    observed-device storage receipt that equals the adapter's durable lease
//!    inventory (`Storage::attachments`), and no other lease of that employee;
//! 3. `openat2(RESOLVE_BENEATH|NO_SYMLINKS|NO_MAGICLINKS|NO_XDEV)` with
//!    `O_RDONLY|O_NOFOLLOW|O_CLOEXEC|O_NOCTTY|O_NONBLOCK` relative to the
//!    root-held work mount; the root itself is a directory on the lease device;
//! 4. statx of the opened fd: S_ISREG, nlink 1, employee uid, lease device,
//!    same mount as the root, size ≤ `MAX_WORK_FILE_BYTES`.
//!
//! Decision: snapshot, not the live fd. The opened file is copied (bounded by
//! `MAX_WORK_FILE_BYTES` during the copy) into a memfd that is sealed
//! (SEAL|SHRINK|GROW|WRITE, and EXEC where the kernel has MFD_NOEXEC_SEAL) and
//! reopened O_RDONLY; the live fd is closed before returning. Reasons:
//! - the employee process of the same run owns the inode and could append,
//!   truncate or rewrite it after the size check; the snapshot pins content
//!   and size, so every consumer (file_read, parse worker) sees ≤ 16 MiB of
//!   immutable bytes regardless of its own bounds;
//! - the snapshot references no inode or superblock on the employee loop
//!   filesystem, so a gateway- or worker-held fd can neither keep reading the
//!   employee's file after the run ends (D-H1: "for the live run only") nor
//!   keep the filesystem busy and block the lease detach at cleanup;
//! - the gateway never gets an fd it could `flock`/`fstat`-poll on the
//!   employee's inode (lock interference, change probing).
//! Cost: one bounded copy per call (≤ 16 MiB of reads plus ≤ 16 MiB of shmem,
//! charged to spawnd's memory cgroup until the receiver closes the fd). H4
//! must therefore serialize opens per run and bound in-flight snapshots.
//! A writer racing the copy is detected by comparing the full statx identity
//! (inode, size, mtime, ctime, nlink, owner, device, mount) before and after;
//! one retry, then `Refused`. Timestamp granularity means a same-size rewrite
//! inside one tick can still yield a torn snapshot: acceptable, because the
//! bytes are employee-controlled anyway and stay bounded and immutable.
//! Consequence for H5/H6: the received fd has nlink 0 (memfd) and a tmpfs
//! device/inode; the boundary must accept "sealed memfd" in place of nlink 1.
use std::{ffi::CString, fs::File, io::{self, Write}, mem,
    os::{fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd}, unix::fs::{FileExt, MetadataExt}},
    sync::Arc, time::{Duration, Instant}};
use duduclaw_isolation::{execution::RunHandle, ids::EmployeeId,
    ipc::{ErrorCode, WorkFileMetadata, WorkPath, MAX_WORK_FILE_BYTES, WORK_RESERVED_FIRST_COMPONENT}};
use duduclaw_mcp_boundary::resources::{WorkRelativePath, BROKERED_FILE_MAX_BYTES};
use crate::storage::{Attachment, Storage, valid_employee_uid};

// The IPC limit (H1) and the boundary limit (H6) must never drift apart.
const _: () = assert!(MAX_WORK_FILE_BYTES == BROKERED_FILE_MAX_BYTES);

const OPEN_ATTEMPTS: usize = 4;
const COPY_ATTEMPTS: usize = 2;
const COPY_CHUNK: usize = 64 * 1024;
const COPY_DEADLINE: Duration = Duration::from_secs(10);
/// Linux 6.3+; older kernels answer EINVAL and we retry without it.
const MFD_NOEXEC_SEAL: libc::c_uint = 0x0008;
const SEALS: libc::c_int = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;

/// Root-side refusal. Diagnostic only; the gateway sees `code()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkRefusal {
    /// No live run with this handle in the core.
    UnknownRun,
    /// The run belongs to another employee.
    WrongEmployee,
    /// Not Running, or no attached storage lease (or the lease is another run's).
    NotLive,
    /// Path outside the shared H1/H6 rules or under `.forks`.
    InvalidPath,
    /// ENOENT/ENOTDIR during resolution.
    NotFound,
    /// Symlink, magic link, mount crossing, not S_ISREG, nlink ≠ 1, foreign
    /// owner/device/mount, or the file kept changing during the copy.
    Refused,
    TooLarge,
    /// Inconsistent root state, missing work root, or a syscall failure.
    Unavailable,
}
impl WorkRefusal {
    /// Mapping documented on `ipc::ErrorCode` (H1).
    pub fn code(self) -> ErrorCode {
        match self {
            Self::UnknownRun => ErrorCode::NotFound,
            Self::WrongEmployee => ErrorCode::Unauthorized,
            Self::NotLive => ErrorCode::NotReady,
            Self::InvalidPath => ErrorCode::InvalidRequest,
            Self::NotFound => ErrorCode::WorkFileNotFound,
            Self::Refused => ErrorCode::WorkFileRefused,
            Self::TooLarge => ErrorCode::WorkFileTooLarge,
            Self::Unavailable => ErrorCode::WorkspaceBrokerUnavailable,
        }
    }
}

/// What H4 reads from its own core state for the requested run, under the
/// same worker turn as the open. `running` = admission state Running and the
/// `LiveRun` not closed. `receipt` = `LiveRun.attachment`. `work_root` = a
/// root-held detached clone of the H-attached work tree taken BEFORE
/// `select_workspace` narrows `/work` (so `.forks` really is the fork area),
/// dropped before `Storage::detach` at cleanup.
pub struct LiveRunView<'a> {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub uid: u32,
    pub running: bool,
    pub receipt: Option<&'a Attachment>,
    pub work_root: Option<BorrowedFd<'a>>,
}

/// Identity every opened file must carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkExpectation { pub uid: u32, pub device_major: u32, pub device_minor: u32 }

/// Root audit identity of the employee file the snapshot was taken from. Never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceIdentity { pub device_major: u32, pub device_minor: u32, pub inode: u64, pub size: u64 }

/// `file` is the single `FdRole::WorkFileRead` fd: a sealed memfd opened
/// O_RDONLY|O_CLOEXEC at offset 0. `metadata` is the honest fstat of `file`
/// (the H1 `SpawnResponse::WorkFile` payload).
#[derive(Debug)]
pub struct WorkFileSnapshot { pub file: File, pub metadata: WorkFileMetadata, pub source: SourceIdentity }

/// H1 `WorkPath` rules ∩ H6 `WorkRelativePath` rules, plus the `.forks` refusal.
pub fn check_work_path(path: &str) -> Result<(), WorkRefusal> {
    WorkRelativePath::parse(path).map_err(|_| WorkRefusal::InvalidPath)?;
    WorkPath::new(path.to_owned()).map_err(|_| WorkRefusal::InvalidPath)?;
    if path.split('/').next() == Some(WORK_RESERVED_FIRST_COMPONENT) { return Err(WorkRefusal::InvalidPath); }
    Ok(())
}

/// Step 2: liveness and lease. `storage` is the adapter that attached the
/// receipt; its durable inventory is the authority, not the cached receipt.
pub fn verify_lease(view: &LiveRunView<'_>, storage: &mut dyn Storage, run: RunHandle, employee: EmployeeId)
    -> Result<WorkExpectation, WorkRefusal> {
    if view.run != run { return Err(WorkRefusal::UnknownRun); }
    if view.employee != employee { return Err(WorkRefusal::WrongEmployee); }
    if !valid_employee_uid(view.uid) { return Err(WorkRefusal::Unavailable); }
    if !view.running { return Err(WorkRefusal::NotLive); }
    let receipt = view.receipt.ok_or(WorkRefusal::NotLive)?;
    // Also requires an observed device: an attach intent is cleanup-only.
    receipt.check(employee, run).map_err(|_| WorkRefusal::NotLive)?;
    let inventory = storage.attachments().map_err(|_| WorkRefusal::Unavailable)?;
    let mut related = inventory.iter().filter(|lease| lease.run == run || lease.employee == employee);
    let lease = related.next().ok_or(WorkRefusal::NotLive)?;
    if related.next().is_some() { return Err(WorkRefusal::Unavailable); }
    if lease.employee == employee && lease.run != run { return Err(WorkRefusal::NotLive); }
    if lease != receipt { return Err(WorkRefusal::Unavailable); }
    Ok(WorkExpectation { uid: view.uid, device_major: receipt.device_major, device_minor: receipt.device_minor })
}

/// The whole broker: path, lease, open, check, sealed snapshot.
pub fn open_work_file(view: &LiveRunView<'_>, storage: &mut dyn Storage, run: RunHandle, employee: EmployeeId,
    path: &WorkPath) -> Result<WorkFileSnapshot, WorkRefusal> {
    check_work_path(path.as_str())?;
    let expected = verify_lease(view, storage, run, employee)?;
    let root = view.work_root.ok_or(WorkRefusal::Unavailable)?;
    snapshot(root, path.as_str(), &expected)
}

/// Steps 3–4 plus the copy. `root` must be the work mount root on the lease device.
pub fn snapshot(root: BorrowedFd<'_>, path: &str, expected: &WorkExpectation) -> Result<WorkFileSnapshot, WorkRefusal> {
    check_work_path(path)?;
    let root_stat = stat(root).map_err(|_| WorkRefusal::Unavailable)?;
    if root_stat.mode & libc::S_IFMT != libc::S_IFDIR
        || (root_stat.dev_major, root_stat.dev_minor) != (expected.device_major, expected.device_minor) {
        return Err(WorkRefusal::Unavailable);
    }
    let source = File::from(open_beneath(root, path)?);
    let deadline = Instant::now() + COPY_DEADLINE;
    for _ in 0..COPY_ATTEMPTS {
        let before = stat(source.as_fd()).map_err(|_| WorkRefusal::Unavailable)?;
        check_source(&before, expected, root_stat.mount_id)?;
        let mut copy = memfd()?;
        let copied = copy_bounded(|buffer, offset| source.read_at(buffer, offset), &mut copy, MAX_WORK_FILE_BYTES, deadline)?;
        let after = stat(source.as_fd()).map_err(|_| WorkRefusal::Unavailable)?;
        check_source(&after, expected, root_stat.mount_id)?;
        if before != after || copied != before.size { continue; }
        let (file, metadata) = seal_read_only(copy, copied)?;
        let identity = SourceIdentity { device_major: before.dev_major, device_minor: before.dev_minor,
            inode: before.inode, size: before.size };
        return Ok(WorkFileSnapshot { file, metadata, source: identity });
    }
    Err(WorkRefusal::Refused)
}

fn open_beneath(root: BorrowedFd<'_>, path: &str) -> Result<OwnedFd, WorkRefusal> {
    #[repr(C)]
    struct OpenHow { flags: u64, mode: u64, resolve: u64 }
    let name = CString::new(path).map_err(|_| WorkRefusal::InvalidPath)?;
    // O_NONBLOCK: a FIFO opens without waiting for a writer, then fails S_ISREG.
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NONBLOCK) as u64,
        mode: 0,
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_XDEV,
    };
    for _ in 0..OPEN_ATTEMPTS {
        let raw = unsafe { libc::syscall(libc::SYS_openat2, root.as_raw_fd(), name.as_ptr(),
            &how as *const OpenHow, mem::size_of::<OpenHow>()) };
        if raw >= 0 { return Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) }); }
        match io::Error::last_os_error().raw_os_error() {
            // EAGAIN: a concurrent rename/mount raced the BENEATH walk.
            Some(libc::EAGAIN | libc::EINTR) => continue,
            Some(libc::ENOENT | libc::ENOTDIR) => return Err(WorkRefusal::NotFound),
            Some(libc::ELOOP | libc::EXDEV | libc::EACCES | libc::EPERM | libc::ENXIO | libc::ENODEV
                | libc::ENAMETOOLONG | libc::EOPNOTSUPP | libc::ETXTBSY | libc::EISDIR) => return Err(WorkRefusal::Refused),
            // ENOSYS (no openat2), EINVAL, EMFILE, ENOMEM, …: the broker, not the path.
            _ => return Err(WorkRefusal::Unavailable),
        }
    }
    Err(WorkRefusal::Unavailable)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    mode: u32, nlink: u32, uid: u32, dev_major: u32, dev_minor: u32,
    inode: u64, size: u64, mount_id: u64, mtime: (i64, u32), ctime: (i64, u32),
}
fn stat(fd: BorrowedFd<'_>) -> io::Result<Stat> {
    let mut st: libc::statx = unsafe { mem::zeroed() };
    let mask = libc::STATX_BASIC_STATS | libc::STATX_MNT_ID;
    // Raw syscall: glibc's `statx` wrapper is above the release baseline.
    duduclaw_core::linux_syscalls::statx(fd.as_raw_fd(), c"", libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW, mask, &mut st)?;
    if st.stx_mask & mask != mask { return Err(io::ErrorKind::Unsupported.into()); }
    Ok(Stat { mode: u32::from(st.stx_mode), nlink: st.stx_nlink, uid: st.stx_uid,
        dev_major: st.stx_dev_major, dev_minor: st.stx_dev_minor, inode: st.stx_ino, size: st.stx_size,
        mount_id: st.stx_mnt_id, mtime: (st.stx_mtime.tv_sec, st.stx_mtime.tv_nsec),
        ctime: (st.stx_ctime.tv_sec, st.stx_ctime.tv_nsec) })
}

/// Step 4. Device and mount are rechecked even though NO_XDEV already refuses
/// a crossing: a wrong root fd from the caller must not widen the authority.
fn check_source(stat: &Stat, expected: &WorkExpectation, root_mount: u64) -> Result<(), WorkRefusal> {
    if stat.mode & libc::S_IFMT != libc::S_IFREG || stat.nlink != 1 || stat.uid != expected.uid
        || (stat.dev_major, stat.dev_minor) != (expected.device_major, expected.device_minor)
        || stat.mount_id != root_mount {
        return Err(WorkRefusal::Refused);
    }
    if stat.size > MAX_WORK_FILE_BYTES { return Err(WorkRefusal::TooLarge); }
    Ok(())
}

/// Copies from offset 0 until EOF. Reads at most `limit + 1` bytes, so a file
/// grown past the limit after the size check is `TooLarge`, never truncated.
fn copy_bounded(mut read_at: impl FnMut(&mut [u8], u64) -> io::Result<usize>, sink: &mut impl Write,
    limit: u64, deadline: Instant) -> Result<u64, WorkRefusal> {
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut copied = 0u64;
    loop {
        if Instant::now() > deadline { return Err(WorkRefusal::Unavailable); }
        let want = (limit + 1 - copied).min(COPY_CHUNK as u64) as usize;
        let read = match read_at(&mut buffer[..want], copied) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(WorkRefusal::Unavailable),
        };
        if read == 0 { return Ok(copied); }
        let read = read.min(want);
        copied += read as u64;
        if copied > limit { return Err(WorkRefusal::TooLarge); }
        sink.write_all(&buffer[..read]).map_err(|_| WorkRefusal::Unavailable)?;
    }
}

fn memfd() -> Result<File, WorkRefusal> {
    let name = c"duduclaw-work-file";
    // Raw syscall: glibc's `memfd_create` wrapper is above the release baseline.
    use duduclaw_core::linux_syscalls::memfd_create;
    let fd = match memfd_create(name, libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING | MFD_NOEXEC_SEAL) {
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) =>
            memfd_create(name, libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING),
        other => other,
    };
    Ok(File::from(fd.map_err(|_| WorkRefusal::Unavailable)?))
}

/// Seal the copy, reopen it read-only through our own procfs fd entry (a new
/// open file description, offset 0), verify, and drop the writable fd.
fn seal_read_only(writable: File, size: u64) -> Result<(File, WorkFileMetadata), WorkRefusal> {
    const U: WorkRefusal = WorkRefusal::Unavailable;
    if unsafe { libc::fcntl(writable.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 { return Err(U); }
    let entry = CString::new(format!("/proc/thread-self/fd/{}", writable.as_raw_fd())).map_err(|_| U)?;
    let raw = unsafe { libc::open(entry.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NONBLOCK) };
    if raw < 0 { return Err(U); }
    let file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    let (original, reopened) = (writable.metadata().map_err(|_| U)?, file.metadata().map_err(|_| U)?);
    if (original.dev(), original.ino()) != (reopened.dev(), reopened.ino()) || !reopened.is_file() || reopened.len() != size {
        return Err(U);
    }
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if seals < 0 || seals & SEALS != SEALS || flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY { return Err(U); }
    drop(writable);
    let metadata = WorkFileMetadata { size: reopened.len(), inode: reopened.ino(), device: reopened.dev() };
    metadata.validate().map_err(|_| U)?;
    Ok((file, metadata))
}

/// H4: the root-held work tree clone of one live run (`bootstrap_mounts::
/// work_broker_root`, taken before `select_workspace`) and the pins of the
/// snapshot copies still running against it. The clone keeps the employee
/// filesystem mounted, so the host must `release` it — and see no copy in
/// flight — before `Storage::detach`.
#[derive(Debug)]
pub struct BrokerRoot { root: Option<OwnedFd>, pin: Arc<()> }
impl BrokerRoot {
    pub fn new(root: Option<OwnedFd>) -> Self { Self { root, pin: Arc::new(()) } }
    /// The clone, for `LiveRunView::work_root` (`None` once released).
    pub fn root(&self) -> Option<BorrowedFd<'_>> { self.root.as_ref().map(AsFd::as_fd) }
    /// One deferred copy's hold: a dup of the clone plus a pin.
    pub fn lease(&self) -> Result<CopyLease, WorkRefusal> {
        let root = self.root.as_ref().ok_or(WorkRefusal::Unavailable)?.try_clone().map_err(|_| WorkRefusal::Unavailable)?;
        Ok(CopyLease { root, _pin: self.pin.clone() })
    }
    pub fn in_flight(&self) -> usize { Arc::strong_count(&self.pin).saturating_sub(1) }
    /// Close the clone (idempotent); `NotReady` while any copy still holds a
    /// dup. The cleanup retries until it is `Ok`, then detaches.
    pub fn release(&mut self) -> crate::Result<()> {
        self.root = None;
        if self.in_flight() > 0 { Err(ErrorCode::NotReady) } else { Ok(()) }
    }
}
/// Field order is drop order: the root dup is closed before the pin goes.
#[derive(Debug)]
pub struct CopyLease { root: OwnedFd, _pin: Arc<()> }

/// H4: the copy half of `open_work_file`, run on the host's copy worker after
/// `check_work_path` and `verify_lease` passed on the lifecycle thread (same
/// turn as the `LiveRunView`). `live` (the run's fence not raised) is re-read
/// before and after the copy; a snapshot finished after the run stopped is
/// dropped, never returned. The lease is released before returning.
pub fn copy_snapshot(lease: CopyLease, path: &WorkPath, expected: &WorkExpectation, live: &dyn Fn() -> bool)
    -> Result<WorkFileSnapshot, WorkRefusal> {
    if !live() { return Err(WorkRefusal::NotLive); }
    let result = snapshot(lease.root.as_fd(), path.as_str(), expected);
    drop(lease);
    if !live() { return Err(WorkRefusal::NotLive); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read, os::unix::fs::symlink, path::Path};
    use duduclaw_isolation::{execution::DiskQuota, ids::{RequestId, RunId, Sha256Digest}};
    use duduclaw_mcp_boundary::resources::{WORK_PATH_CONFORMANCE, WORK_PATH_MAX_COMPONENT_BYTES, WORK_PATH_MAX_DEPTH};
    use crate::storage::{AttachedStorage, ProvisionRequest, StorageUsage};

    /// Durable lease inventory double: only `attachments` is meaningful.
    struct Inventory(crate::Result<Vec<Attachment>>);
    impl Storage for Inventory {
        fn readiness(&mut self) -> crate::Result<()> { Err(ErrorCode::IsolationUnavailable) }
        fn provision(&mut self, _: &ProvisionRequest) -> crate::Result<StorageUsage> { Err(ErrorCode::IsolationUnavailable) }
        fn attach(&mut self, _: EmployeeId, _: u32, _: RunHandle, _: &DiskQuota) -> crate::Result<AttachedStorage> { Err(ErrorCode::IsolationUnavailable) }
        fn detach(&mut self, _: &Attachment) -> crate::Result<()> { Err(ErrorCode::IsolationUnavailable) }
        fn destroy(&mut self, _: EmployeeId, _: RequestId, _: Sha256Digest) -> crate::Result<()> { Err(ErrorCode::IsolationUnavailable) }
        fn usage(&mut self, _: EmployeeId) -> crate::Result<StorageUsage> { Err(ErrorCode::IsolationUnavailable) }
        fn attachments(&mut self) -> crate::Result<Vec<Attachment>> { self.0.clone() }
    }

    struct Fixture { dir: tempfile::TempDir, root: File, expected: WorkExpectation }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = File::open(dir.path()).unwrap();
            let meta = root.metadata().unwrap();
            let expected = WorkExpectation { uid: meta.uid(), device_major: libc::major(meta.dev()), device_minor: libc::minor(meta.dev()) };
            Self { dir, root, expected }
        }
        fn path(&self, relative: &str) -> std::path::PathBuf { self.dir.path().join(relative) }
        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.path(relative);
            if let Some(parent) = path.parent() { fs::create_dir_all(parent).unwrap(); }
            fs::write(path, bytes).unwrap();
        }
        fn open(&self, relative: &str) -> Result<WorkFileSnapshot, WorkRefusal> { self.open_as(relative, &self.expected) }
        fn open_as(&self, relative: &str, expected: &WorkExpectation) -> Result<WorkFileSnapshot, WorkRefusal> {
            snapshot(self.root.as_fd(), relative, expected)
        }
    }
    fn contents(mut snapshot: WorkFileSnapshot) -> Vec<u8> {
        let mut bytes = Vec::new(); snapshot.file.read_to_end(&mut bytes).unwrap(); bytes
    }

    #[test]
    fn work_broker_refuses_symlink_component() {
        let f = Fixture::new();
        f.write("real/file.txt", b"secret");
        symlink("real", f.path("link")).unwrap();
        symlink("real/file.txt", f.path("alias.txt")).unwrap();
        symlink("/etc/passwd", f.path("absolute")).unwrap();
        symlink("loop", f.path("loop")).unwrap();
        fs::create_dir(f.path("nested")).unwrap();
        symlink("../real", f.path("nested/up")).unwrap();
        assert_eq!(contents(f.open("real/file.txt").unwrap()), b"secret");
        for path in ["link/file.txt", "alias.txt", "absolute", "loop", "link", "nested/up/file.txt"] {
            assert_eq!(f.open(path).unwrap_err(), WorkRefusal::Refused, "{path}");
        }
        // Escapes are refused as paths before any resolution.
        assert_eq!(f.open("real/../real/file.txt").unwrap_err(), WorkRefusal::InvalidPath);
        assert_eq!(f.open("/etc/passwd").unwrap_err(), WorkRefusal::InvalidPath);
    }

    #[test]
    fn work_broker_refuses_hardlinked_file() {
        let f = Fixture::new();
        f.write("a.txt", b"x");
        fs::hard_link(f.path("a.txt"), f.path("b.txt")).unwrap();
        assert_eq!(f.open("a.txt").unwrap_err(), WorkRefusal::Refused);
        assert_eq!(f.open("b.txt").unwrap_err(), WorkRefusal::Refused);
        fs::remove_file(f.path("b.txt")).unwrap();
        assert_eq!(contents(f.open("a.txt").unwrap()), b"x");
    }

    #[test]
    fn work_broker_refuses_foreign_owner() {
        let f = Fixture::new();
        f.write("mine.txt", b"x");
        let other = WorkExpectation { uid: f.expected.uid.wrapping_add(1), ..f.expected };
        assert_eq!(f.open_as("mine.txt", &other).unwrap_err(), WorkRefusal::Refused);
        assert!(f.open("mine.txt").is_ok());
        if unsafe { libc::geteuid() } == 0 {
            let employee = WorkExpectation { uid: 70_001, ..f.expected };
            std::os::unix::fs::chown(f.path("mine.txt"), Some(70_001), Some(70_001)).unwrap();
            assert!(f.open_as("mine.txt", &employee).is_ok());
            std::os::unix::fs::chown(f.path("mine.txt"), Some(70_002), Some(70_002)).unwrap();
            assert_eq!(f.open_as("mine.txt", &employee).unwrap_err(), WorkRefusal::Refused);
        }
    }

    #[test]
    fn work_broker_refuses_cross_device() {
        let f = Fixture::new();
        f.write("f.txt", b"x");
        // A root that is not on the lease device is a broker fault, never an open.
        let wrong = WorkExpectation { device_minor: f.expected.device_minor.wrapping_add(1), ..f.expected };
        assert_eq!(f.open_as("f.txt", &wrong).unwrap_err(), WorkRefusal::Unavailable);
        let wrong = WorkExpectation { device_major: f.expected.device_major.wrapping_add(1), ..f.expected };
        assert_eq!(f.open_as("f.txt", &wrong).unwrap_err(), WorkRefusal::Unavailable);
        // A file whose device or mount differs from the root's is refused even
        // if openat2 had let it through (real EXDEV: iso_root_work_broker_*).
        let file = File::open(f.path("f.txt")).unwrap();
        let actual = stat(file.as_fd()).unwrap();
        let root_mount = stat(f.root.as_fd()).unwrap().mount_id;
        assert_eq!(check_source(&actual, &f.expected, root_mount), Ok(()));
        assert_eq!(check_source(&Stat { dev_minor: actual.dev_minor.wrapping_add(1), ..actual }, &f.expected, root_mount),
            Err(WorkRefusal::Refused));
        assert_eq!(check_source(&Stat { dev_major: actual.dev_major.wrapping_add(1), ..actual }, &f.expected, root_mount),
            Err(WorkRefusal::Refused));
        assert_eq!(check_source(&actual, &f.expected, root_mount.wrapping_add(1)), Err(WorkRefusal::Refused));
    }

    #[test]
    fn work_broker_refuses_oversize() {
        let f = Fixture::new();
        File::create(f.path("limit.bin")).unwrap().set_len(MAX_WORK_FILE_BYTES).unwrap();
        File::create(f.path("big.bin")).unwrap().set_len(MAX_WORK_FILE_BYTES + 1).unwrap();
        let limit = f.open("limit.bin").unwrap();
        assert_eq!(limit.metadata.size, MAX_WORK_FILE_BYTES);
        assert_eq!(limit.file.metadata().unwrap().len(), MAX_WORK_FILE_BYTES);
        assert_eq!(f.open("big.bin").unwrap_err(), WorkRefusal::TooLarge);
        // Growth after the size check: the copy itself enforces the cap.
        let deadline = Instant::now() + COPY_DEADLINE;
        let endless = |buffer: &mut [u8], _: u64| { buffer.fill(b'a'); Ok::<usize, io::Error>(buffer.len()) };
        assert_eq!(copy_bounded(endless, &mut Vec::new(), MAX_WORK_FILE_BYTES, deadline), Err(WorkRefusal::TooLarge));
        let exact = |data: &'static [u8]| move |buffer: &mut [u8], offset: u64| {
            let rest = data.get(offset as usize..).unwrap_or_default();
            let n = rest.len().min(buffer.len()); buffer[..n].copy_from_slice(&rest[..n]); Ok::<usize, io::Error>(n)
        };
        let mut sink = Vec::new();
        assert_eq!(copy_bounded(exact(&b"0123456789"[..]), &mut sink, 10, deadline), Ok(10));
        assert_eq!(sink, b"0123456789");
        assert_eq!(copy_bounded(exact(&b"0123456789!"[..]), &mut Vec::new(), 10, deadline), Err(WorkRefusal::TooLarge));
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(copy_bounded(exact(&b"x"[..]), &mut Vec::new(), 10, past), Err(WorkRefusal::Unavailable));
    }

    #[test]
    fn work_broker_refuses_non_regular_and_reports_missing() {
        let f = Fixture::new();
        f.write("file.txt", b"x");
        fs::create_dir(f.path("dir")).unwrap();
        let fifo = CString::new(f.path("pipe").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        // O_NONBLOCK: the FIFO open returns at once and fails S_ISREG.
        assert_eq!(f.open("pipe").unwrap_err(), WorkRefusal::Refused);
        assert_eq!(f.open("dir").unwrap_err(), WorkRefusal::Refused);
        assert_eq!(f.open("missing.txt").unwrap_err(), WorkRefusal::NotFound);
        assert_eq!(f.open("file.txt/inside").unwrap_err(), WorkRefusal::NotFound);
    }

    #[test]
    fn work_broker_snapshot_is_sealed_read_only() {
        let f = Fixture::new();
        f.write("doc.txt", b"version one");
        let inode = fs::metadata(f.path("doc.txt")).unwrap().ino();
        let mut snapshot = f.open("doc.txt").unwrap();
        assert_eq!(snapshot.source, SourceIdentity { device_major: f.expected.device_major,
            device_minor: f.expected.device_minor, inode, size: 11 });
        // The employee rewrites the same inode in place afterwards.
        fs::write(f.path("doc.txt"), b"version two, which is longer").unwrap();
        assert_eq!(fs::metadata(f.path("doc.txt")).unwrap().ino(), inode);
        let actual = snapshot.file.metadata().unwrap();
        assert_eq!(snapshot.metadata, WorkFileMetadata { size: 11, inode: actual.ino(), device: actual.dev() });
        assert!(snapshot.metadata.validate().is_ok());
        assert!(actual.is_file());
        assert_ne!((actual.dev(), actual.ino()), (fs::metadata(f.path("doc.txt")).unwrap().dev(), inode));
        let seals = unsafe { libc::fcntl(snapshot.file.as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(seals & SEALS, SEALS);
        let flags = unsafe { libc::fcntl(snapshot.file.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        let fd_flags = unsafe { libc::fcntl(snapshot.file.as_raw_fd(), libc::F_GETFD) };
        assert_eq!(fd_flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
        assert!(snapshot.file.write_all(b"x").is_err());
        assert!(snapshot.file.set_len(0).is_err());
        let mut bytes = Vec::new(); snapshot.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"version one");
        // Not even a writable reopen of the snapshot can change it.
        let entry = format!("/proc/thread-self/fd/{}", snapshot.file.as_raw_fd());
        if let Ok(mut writable) = fs::OpenOptions::new().write(true).open(Path::new(&entry)) {
            assert!(writable.write_all(b"x").is_err());
        }
    }

    const RUN: RunHandle = RunHandle(RunId([2; 16]));
    const EMPLOYEE: EmployeeId = EmployeeId([1; 16]);
    const UID: u32 = 70_000;
    fn receipt() -> Attachment {
        Attachment { employee: EMPLOYEE, run: RUN, token: RequestId([3; 16]), device_major: 7, device_minor: 4 }
    }
    fn view(receipt: Option<&Attachment>) -> LiveRunView<'_> {
        LiveRunView { run: RUN, employee: EMPLOYEE, uid: UID, running: true, receipt, work_root: None }
    }

    #[test]
    fn work_broker_requires_live_lease() {
        let lease = receipt();
        let leased = || Inventory(Ok(vec![receipt()]));
        let ok = WorkExpectation { uid: UID, device_major: 7, device_minor: 4 };
        assert_eq!(verify_lease(&view(Some(&lease)), &mut leased(), RUN, EMPLOYEE), Ok(ok));
        // Wrong run or employee.
        assert_eq!(verify_lease(&view(Some(&lease)), &mut leased(), RunHandle(RunId([9; 16])), EMPLOYEE), Err(WorkRefusal::UnknownRun));
        assert_eq!(verify_lease(&view(Some(&lease)), &mut leased(), RUN, EmployeeId([9; 16])), Err(WorkRefusal::WrongEmployee));
        // Not Running, no receipt, or an intent without an observed device.
        let stopped = LiveRunView { running: false, ..view(Some(&lease)) };
        assert_eq!(verify_lease(&stopped, &mut leased(), RUN, EMPLOYEE), Err(WorkRefusal::NotLive));
        assert_eq!(verify_lease(&view(None), &mut leased(), RUN, EMPLOYEE), Err(WorkRefusal::NotLive));
        let intent = Attachment { device_major: 0, device_minor: 0, ..receipt() };
        assert_eq!(verify_lease(&view(Some(&intent)), &mut Inventory(Ok(vec![intent.clone()])), RUN, EMPLOYEE),
            Err(WorkRefusal::NotLive));
        let foreign = Attachment { employee: EmployeeId([8; 16]), ..receipt() };
        assert_eq!(verify_lease(&view(Some(&foreign)), &mut leased(), RUN, EMPLOYEE), Err(WorkRefusal::NotLive));
        // The durable inventory, not the cached receipt, is authority.
        assert_eq!(verify_lease(&view(Some(&lease)), &mut Inventory(Ok(vec![])), RUN, EMPLOYEE), Err(WorkRefusal::NotLive));
        assert_eq!(verify_lease(&view(Some(&lease)), &mut Inventory(Err(ErrorCode::IsolationUnavailable)), RUN, EMPLOYEE),
            Err(WorkRefusal::Unavailable));
        let newer = Attachment { token: RequestId([4; 16]), ..receipt() };
        assert_eq!(verify_lease(&view(Some(&lease)), &mut Inventory(Ok(vec![newer])), RUN, EMPLOYEE), Err(WorkRefusal::Unavailable));
        let moved = Attachment { device_minor: 5, ..receipt() };
        assert_eq!(verify_lease(&view(Some(&lease)), &mut Inventory(Ok(vec![moved])), RUN, EMPLOYEE), Err(WorkRefusal::Unavailable));
        let elsewhere = Attachment { run: RunHandle(RunId([6; 16])), ..receipt() };
        assert_eq!(verify_lease(&view(Some(&lease)), &mut Inventory(Ok(vec![elsewhere])), RUN, EMPLOYEE), Err(WorkRefusal::NotLive));
        assert_eq!(verify_lease(&view(Some(&lease)), &mut Inventory(Ok(vec![receipt(), receipt()])), RUN, EMPLOYEE),
            Err(WorkRefusal::Unavailable));
        let unrelated = Attachment { employee: EmployeeId([5; 16]), run: RunHandle(RunId([5; 16])), ..receipt() };
        assert_eq!(verify_lease(&view(Some(&lease)), &mut Inventory(Ok(vec![unrelated, receipt()])), RUN, EMPLOYEE), Ok(ok));
        // Never an employee uid outside the allocator's range.
        let system = LiveRunView { uid: 0, ..view(Some(&lease)) };
        assert_eq!(verify_lease(&system, &mut leased(), RUN, EMPLOYEE), Err(WorkRefusal::Unavailable));

        // The full entry point checks the lease before touching the filesystem.
        let f = Fixture::new();
        f.write("a.txt", b"x");
        let path = WorkPath::new("a.txt".into()).unwrap();
        let stopped = LiveRunView { running: false, work_root: Some(f.root.as_fd()), ..view(Some(&lease)) };
        assert_eq!(open_work_file(&stopped, &mut leased(), RUN, EMPLOYEE, &path).unwrap_err(), WorkRefusal::NotLive);
        assert_eq!(open_work_file(&view(Some(&lease)), &mut leased(), RUN, EMPLOYEE, &path).unwrap_err(), WorkRefusal::Unavailable);
        // A root fd on another device than the lease is refused, not opened.
        let wrong_root = LiveRunView { work_root: Some(f.root.as_fd()), ..view(Some(&lease)) };
        if (f.expected.device_major, f.expected.device_minor) != (7, 4) {
            assert_eq!(open_work_file(&wrong_root, &mut leased(), RUN, EMPLOYEE, &path).unwrap_err(), WorkRefusal::Unavailable);
        }
        // End to end where the fixture filesystem can stand in for a lease
        // device (non-zero major) and the test uid is an employee-range uid.
        if f.expected.device_major != 0 && valid_employee_uid(f.expected.uid) {
            let real = Attachment { device_major: f.expected.device_major, device_minor: f.expected.device_minor, ..receipt() };
            let live = LiveRunView { uid: f.expected.uid, work_root: Some(f.root.as_fd()), ..view(Some(&real)) };
            let opened = open_work_file(&live, &mut Inventory(Ok(vec![real.clone()])), RUN, EMPLOYEE, &path).unwrap();
            assert_eq!(contents(opened), b"x");
        } else {
            eprintln!("work_broker_requires_live_lease: end-to-end half skipped (fixture device major 0 or uid outside employee range)");
        }
        // Gateway-visible codes (H1 `ErrorCode` contract).
        for (refusal, code) in [(WorkRefusal::UnknownRun, ErrorCode::NotFound), (WorkRefusal::WrongEmployee, ErrorCode::Unauthorized),
            (WorkRefusal::NotLive, ErrorCode::NotReady), (WorkRefusal::InvalidPath, ErrorCode::InvalidRequest),
            (WorkRefusal::NotFound, ErrorCode::WorkFileNotFound), (WorkRefusal::Refused, ErrorCode::WorkFileRefused),
            (WorkRefusal::TooLarge, ErrorCode::WorkFileTooLarge), (WorkRefusal::Unavailable, ErrorCode::WorkspaceBrokerUnavailable)] {
            assert_eq!(refusal.code(), code);
        }
    }

    #[test]
    fn work_broker_conformance_vectors() {
        // H6's shared vectors, verbatim from mcp-boundary.
        for (path, ok) in WORK_PATH_CONFORMANCE {
            assert_eq!(check_work_path(path).is_ok(), *ok, "{path:?}");
            if *ok { assert!(WorkPath::new((*path).to_owned()).is_ok(), "H1 must accept {path:?}"); }
        }
        // `.forks` is refused only as the first component.
        for path in [".forks", ".forks/abc/x"] { assert_eq!(check_work_path(path), Err(WorkRefusal::InvalidPath), "{path}"); }
        for path in ["x/.forks", "forks/a", ".forksx/a", "a/.forks/b"] { assert!(check_work_path(path).is_ok(), "{path}"); }
        // C1 controls: H6 refuses them, so the broker does too.
        for path in ["a\u{85}b", "a\u{9b}b", "\u{80}"] { assert_eq!(check_work_path(path), Err(WorkRefusal::InvalidPath)); }
        // Bounds.
        let component = "a".repeat(WORK_PATH_MAX_COMPONENT_BYTES);
        assert!(check_work_path(&component).is_ok());
        assert!(check_work_path(&format!("{component}a")).is_err());
        let deep = vec!["d"; WORK_PATH_MAX_DEPTH];
        assert!(check_work_path(&deep.join("/")).is_ok());
        assert!(check_work_path(&format!("{}/d", deep.join("/"))).is_err());
        let body = vec!["b".repeat(100); 40].join("/");
        assert!(check_work_path(&format!("{body}/{}", "x".repeat(56))).is_ok());
        assert!(check_work_path(&format!("{body}/{}", "x".repeat(57))).is_err());
        // The snapshot entry point applies the same rules before any open,
        // even when the refused entry exists.
        let f = Fixture::new();
        f.write(".forks/abc/x", b"fork");
        assert_eq!(f.open(".forks/abc/x").unwrap_err(), WorkRefusal::InvalidPath);
        f.write("x/.forks", b"ok");
        assert_eq!(contents(f.open("x/.forks").unwrap()), b"ok");
        for (path, _) in WORK_PATH_CONFORMANCE.iter().filter(|(_, ok)| !*ok) {
            assert_eq!(f.open(path).unwrap_err(), WorkRefusal::InvalidPath, "{path:?}");
        }
    }

    /// Descriptors of this process that refer to `dir` (unique tempdir, so no
    /// other test thread opens it).
    fn descriptors_on(dir: &Path) -> usize {
        let target = fs::canonicalize(dir).unwrap();
        fs::read_dir("/proc/self/fd").unwrap().filter_map(Result::ok)
            .filter(|entry| fs::read_link(entry.path()).is_ok_and(|link| link == target)).count()
    }

    #[test]
    fn h4_clone_dropped_before_detach() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"in flight").unwrap();
        let meta = fs::metadata(dir.path()).unwrap();
        let expected = WorkExpectation { uid: meta.uid(), device_major: libc::major(meta.dev()), device_minor: libc::minor(meta.dev()) };
        let mut root = BrokerRoot::new(Some(File::open(dir.path()).unwrap().into()));
        assert_eq!(descriptors_on(dir.path()), 1);
        assert!(root.root().is_some());
        // A deferred copy holds its own dup and a pin.
        let lease = root.lease().unwrap();
        assert_eq!((root.in_flight(), descriptors_on(dir.path())), (1, 2));
        // Cleanup: the clone itself closes at once, but detach must wait.
        assert_eq!(root.release(), Err(ErrorCode::NotReady));
        assert!(root.root().is_none());
        assert_eq!(descriptors_on(dir.path()), 1, "only the in-flight copy still holds the tree");
        assert_eq!(root.lease().unwrap_err(), WorkRefusal::Unavailable, "no new copy after release");
        // The copy finishes; its dup is closed before it returns.
        let path = WorkPath::new("a.txt".into()).unwrap();
        let snapshot = copy_snapshot(lease, &path, &expected, &|| true).unwrap();
        assert_eq!(descriptors_on(dir.path()), 0, "the sealed snapshot references nothing of the tree");
        assert_eq!(root.in_flight(), 0);
        assert_eq!(root.release(), Ok(()), "now the storage may be detached");
        assert_eq!(root.release(), Ok(()), "idempotent on cleanup retries");
        assert_eq!(contents(snapshot), b"in flight");
        // A copy that ends after the run's fence went up is discarded, and
        // still gives its hold back.
        let mut root = BrokerRoot::new(Some(File::open(dir.path()).unwrap().into()));
        let lease = root.lease().unwrap();
        let checks = std::cell::Cell::new(0);
        let live = || { checks.set(checks.get() + 1); checks.get() == 1 };
        assert_eq!(copy_snapshot(lease, &path, &expected, &live).unwrap_err(), WorkRefusal::NotLive);
        assert_eq!(root.release(), Ok(()));
        assert_eq!(descriptors_on(dir.path()), 0);
        let root = BrokerRoot::new(Some(File::open(dir.path()).unwrap().into()));
        assert_eq!(copy_snapshot(root.lease().unwrap(), &path, &expected, &|| false).unwrap_err(), WorkRefusal::NotLive);
        assert_eq!(root.in_flight(), 0);
        // A run whose clone could not be taken never opens anything.
        assert_eq!(BrokerRoot::new(None).lease().unwrap_err(), WorkRefusal::Unavailable);
    }

    /// Root, private mount namespace of this test thread, backing directory on
    /// a real block device (non-zero major) under ISO_STORAGE_TEST_ROOT.
    fn root_fixture(tag: &str) -> std::path::PathBuf {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWNS) }, 0);
        assert_eq!(unsafe { libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) }, 0);
        let base = std::path::PathBuf::from(std::env::var_os("ISO_STORAGE_TEST_ROOT").expect("set isolated ISO_STORAGE_TEST_ROOT"));
        let dir = base.join(format!("h2-{tag}-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        std::os::unix::fs::chown(&dir, Some(70_001), Some(70_001)).unwrap();
        dir
    }
    fn cpath(path: &Path) -> CString { CString::new(path.as_os_str().as_encoded_bytes()).unwrap() }
    fn employee_file(path: &Path, bytes: &[u8], uid: u32) {
        fs::write(path, bytes).unwrap();
        std::os::unix::fs::chown(path, Some(uid), Some(uid)).unwrap();
    }

    #[test]
    #[ignore = "root disposable Linux VM; ISO_STORAGE_TEST_ROOT on ext4/XFS (non-zero device major)"]
    fn iso_root_work_broker_refuses_mount_crossing_and_devices() {
        let dir = root_fixture("crossing");
        employee_file(&dir.join("file.txt"), b"employee", 70_001);
        fs::create_dir(dir.join("sub")).unwrap();
        assert_eq!(unsafe { libc::mount(c"tmpfs".as_ptr(), cpath(&dir.join("sub")).as_ptr(), c"tmpfs".as_ptr(),
            0, c"size=1m,mode=0700".as_ptr().cast()) }, 0);
        employee_file(&dir.join("sub/inner.txt"), b"other mount", 70_001);
        assert_eq!(unsafe { libc::mknod(cpath(&dir.join("null")).as_ptr(), libc::S_IFCHR | 0o600, libc::makedev(1, 3)) }, 0);
        std::os::unix::fs::chown(dir.join("null"), Some(70_001), Some(70_001)).unwrap();
        // Production shape: a detached recursive nosuid/nodev/noexec clone.
        let source = File::open(&dir).unwrap();
        let work = crate::bootstrap_mounts::detached_clone(source.as_fd(), 2 | 4 | 8).unwrap();
        drop(source);
        let meta = fs::metadata(&dir).unwrap();
        assert_ne!(libc::major(meta.dev()), 0, "ISO_STORAGE_TEST_ROOT must be on a block device");
        let lease = Attachment { device_major: libc::major(meta.dev()), device_minor: libc::minor(meta.dev()), ..receipt() };
        let live = LiveRunView { run: RUN, employee: EMPLOYEE, uid: 70_001, running: true, receipt: Some(&lease), work_root: Some(work.as_fd()) };
        let open = |path: &str| open_work_file(&live, &mut Inventory(Ok(vec![lease.clone()])), RUN, EMPLOYEE,
            &WorkPath::new(path.into()).unwrap());
        assert_eq!(contents(open("file.txt").unwrap()), b"employee");
        assert_eq!(open("sub/inner.txt").unwrap_err(), WorkRefusal::Refused);
        assert_eq!(open("sub").unwrap_err(), WorkRefusal::Refused);
        assert_eq!(open("null").unwrap_err(), WorkRefusal::Refused);
        std::os::unix::fs::chown(dir.join("file.txt"), Some(70_002), Some(70_002)).unwrap();
        assert_eq!(open("file.txt").unwrap_err(), WorkRefusal::Refused);
        drop(work);
        assert_eq!(unsafe { libc::umount2(cpath(&dir.join("sub")).as_ptr(), 0) }, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[ignore = "root disposable Linux VM; ISO_STORAGE_TEST_ROOT on ext4/XFS (non-zero device major)"]
    fn iso_root_work_broker_snapshot_does_not_pin_employee_mount() {
        let dir = root_fixture("pin");
        let volume = dir.join("volume");
        fs::create_dir(&volume).unwrap();
        assert_eq!(unsafe { libc::mount(c"tmpfs".as_ptr(), cpath(&volume).as_ptr(), c"tmpfs".as_ptr(),
            0, c"size=1m,mode=0700".as_ptr().cast()) }, 0);
        std::os::unix::fs::chown(&volume, Some(70_001), Some(70_001)).unwrap();
        employee_file(&volume.join("report.csv"), b"a,b\n1,2\n", 70_001);
        let root = File::open(&volume).unwrap();
        let meta = root.metadata().unwrap();
        let expected = WorkExpectation { uid: 70_001, device_major: libc::major(meta.dev()), device_minor: libc::minor(meta.dev()) };
        let held = snapshot(root.as_fd(), "report.csv", &expected).unwrap();
        drop(root);
        // A non-lazy unmount succeeds while the snapshot is held: no reference
        // to the employee filesystem left root.
        assert_eq!(unsafe { libc::umount2(cpath(&volume).as_ptr(), 0) }, 0);
        assert_eq!(contents(held), b"a,b\n1,2\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
