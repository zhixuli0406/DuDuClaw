//! Linux-only B1 preparation of private detached mounts and verified executable
//! descriptors. This does not enter the B2 namespace or release its start barrier.
use std::{collections::BTreeMap, ffi::{CString, OsStr}, fs::{self, File}, io::{self, Read}, mem,
    os::{fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd}, unix::{ffi::OsStrExt, fs::MetadataExt}},
    path::{Component, Path, PathBuf}, time::{Duration, Instant}};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{execution::RunHandle, ids::{EmployeeId, Sha256Digest}};
use crate::{bootstrap::{BootstrapPlan, ExecutablePlan, minimal_etc, ETC_MARKER_CONTENT, ETC_MARKER_DIR, ETC_MARKER_FILE},
    storage::AttachedStorage};

const OPEN_TREE_CLONE: u32 = 1;
const AT_RECURSIVE: u32 = 0x8000;
const MOUNT_ATTR_RDONLY: u64 = 1;
const MOUNT_ATTR_NOSUID: u64 = 2;
const MOUNT_ATTR_NODEV: u64 = 4;
const MOUNT_ATTR_NOEXEC: u64 = 8;
const MOUNT_ATTR_NOSYMFOLLOW: u64 = 0x0020_0000;
const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PROJECTION_FILES: usize = 5;
/// The /etc projection: the five identity files plus the R3-1 marker directory.
const MAX_ETC_ENTRIES: usize = MAX_PROJECTION_FILES + 1;
/// Attributes of the detached /etc clone handed to the runtime (R3-1: the
/// marker is read-only and its path is never followed through a symlink).
pub(crate) const ETC_MOUNT_ATTRIBUTES: u64 =
    MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOSYMFOLLOW | MOUNT_ATTR_NOEXEC;
#[repr(C)]
struct MountAttr { attr_set: u64, attr_clr: u64, propagation: u64, userns_fd: u64 }
#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }
fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid bootstrap publication") }
fn c_name(name: &OsStr) -> io::Result<CString> { CString::new(name.as_bytes()).map_err(|_| invalid()) }
pub(super) fn owned(raw: libc::c_long) -> io::Result<OwnedFd> {
    if raw < 0 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
}

/// Construct only from F's verified root publication. Paths are not serializable
/// and cannot come from gateway IPC. Every ancestor is checked without symlinks.
/// The projection state directory is precreated by the root installer (0700).
pub struct RootPublicationAnchors { runtime: PathBuf, etc_state: PathBuf, projection_root: PathBuf }
impl RootPublicationAnchors {
    pub fn new(runtime: PathBuf, etc_state: PathBuf, projection_root: PathBuf) -> io::Result<Self> {
        trusted_directory(&runtime)?;
        trusted_directory(&projection_root)?;
        let state = trusted_directory(&etc_state)?;
        if metadata(&state)?.mode() & 0o7777 != 0o700 { return Err(invalid()); }
        Ok(Self { runtime, etc_state, projection_root })
    }
}
fn metadata(fd: &OwnedFd) -> io::Result<fs::Metadata> { File::from(fd.try_clone()?).metadata() }
fn trusted_directory(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() { return Err(invalid()); }
    let mut current = owned(unsafe { libc::open(c"/".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) } as _)?;
    let root = metadata(&current)?;
    if root.uid() != 0 || root.mode() & 0o022 != 0 { return Err(invalid()); }
    for part in path.components() {
        match part {
            Component::RootDir => continue,
            Component::Normal(name) => {
                let name = c_name(name)?;
                current = owned(unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) } as _)?;
            }
            _ => return Err(invalid()),
        }
        let stat = metadata(&current)?;
        if !stat.is_dir() || stat.uid() != 0 || stat.mode() & 0o022 != 0 { return Err(invalid()); }
    }
    Ok(current)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableIdentity {
    pub inode: u64,
    pub mount_id: u64,
    pub bytes: u64,
    pub mode: u32,
    pub uid: u32,
    pub device_major: u32,
    pub device_minor: u32,
    pub modified_seconds: i64,
    pub modified_nanos: u32,
    pub changed_seconds: i64,
    pub changed_nanos: u32,
}
fn identity(fd: BorrowedFd<'_>) -> io::Result<ExecutableIdentity> {
    let mut stat: libc::statx = unsafe { mem::zeroed() };
    let mask = libc::STATX_BASIC_STATS | libc::STATX_MNT_ID;
    // Raw syscall: glibc's `statx` wrapper is above the release baseline.
    duduclaw_run::linux::syscalls::statx(fd.as_raw_fd(), c"",
        libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW, mask, &mut stat)?;
    if stat.stx_mask & mask != mask { return Err(invalid()); }
    Ok(ExecutableIdentity { inode: stat.stx_ino, mount_id: stat.stx_mnt_id, bytes: stat.stx_size,
        mode: u32::from(stat.stx_mode), uid: stat.stx_uid, device_major: stat.stx_dev_major,
        device_minor: stat.stx_dev_minor, modified_seconds: stat.stx_mtime.tv_sec,
        modified_nanos: stat.stx_mtime.tv_nsec, changed_seconds: stat.stx_ctime.tv_sec,
        changed_nanos: stat.stx_ctime.tv_nsec })
}

pub struct VerifiedExecutable { pub fd: OwnedFd, pub identity: ExecutableIdentity, pub digest: Sha256Digest }
pub struct PreparedMounts {
    pub home: OwnedFd,
    pub work: OwnedFd,
    pub projection: OwnedFd,
    pub etc: OwnedFd,
    pub runtime: OwnedFd,
    pub executable: VerifiedExecutable,
    pub script: Option<VerifiedExecutable>,
    /// Keep projection files until cleanup confirms inactive/empty/no-UID.
    pub projection_run: RunHandle,
}

/// At most two bounded 512 MiB hashes, five tiny projection files and five
/// detached mount clones. Invoke in the supervised B1 preparation worker; the
/// CPU/read loop checks its deadline, but blocked kernel I/O needs worker fencing.
/// H supplies only home/work mounts. No image/block device enters this result.
pub fn prepare(plan: &BootstrapPlan, storage: &AttachedStorage, anchors: &RootPublicationAnchors) -> io::Result<PreparedMounts> {
    if unsafe { libc::geteuid() } != 0 { return Err(io::ErrorKind::PermissionDenied.into()); }
    plan.validate().map_err(|_| invalid())?;
    storage.receipt.check(plan.employee, plan.run).map_err(|_| invalid())?;
    let persistent_home = detached_clone(storage.home.as_fd(), MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV)?;
    // The initial B1 seam narrows all work mounts to noexec. Profiles requiring
    // executable workspace content must declare a reviewed explicit policy.
    let work = detached_clone(storage.work.as_fd(), MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC)?;
    let runtime_anchor = trusted_directory(&anchors.runtime)?;
    let readonly = MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOSYMFOLLOW;
    let runtime = detached_clone(runtime_anchor.as_fd(), readonly)?;
    let projection = employee_projection(anchors, plan.employee)?;
    let home = super::native_home::prepare(persistent_home, work.as_fd(), projection.as_fd(), plan.uid, plan.gid)?;
    let etc_anchor = publish_projection(anchors, plan)?;
    let etc = detached_clone(etc_anchor.as_fd(), ETC_MOUNT_ATTRIBUTES)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let (executable, script) = match &plan.executable {
        ExecutablePlan::Elf { entry, digest } => (open_verified(runtime.as_fd(), entry, *digest, true, deadline)?, None),
        ExecutablePlan::Script { interpreter, interpreter_digest, script, script_digest } =>
            (open_verified(runtime.as_fd(), interpreter, *interpreter_digest, true, deadline)?,
                Some(open_verified(runtime.as_fd(), script, *script_digest, false, deadline)?)),
    };
    Ok(PreparedMounts { home, work, projection, etc, runtime, executable, script, projection_run: plan.run })
}

/// HX helper runtimes (D8 revision §2.4 "Helper runtimes"). Every helper is
/// published in the same signed generation registry as the runtime, so the
/// launcher gets ONE separate read-only, nosuid, nodev, nosymfollow clone of it
/// (deduplicated by registry mount, check 5) plus per-helper verified executables.
/// HX3b attaches this clone at `/helpers`, outside the runtime's Landlock list.
/// `files`: per helper, the further signed files (HX6 security fix round,
/// finding 5: the recording's node / Playwright bundle / Chromium), verified
/// on the same registry clone by sha256 like the executables.
pub struct PreparedHelpers {
    pub mount: OwnedFd,
    pub executables: Vec<(VerifiedExecutable, Option<VerifiedExecutable>)>,
    pub files: Vec<Vec<(&'static str, VerifiedExecutable)>>,
}
pub fn prepare_helpers(anchors: &RootPublicationAnchors, helpers: &[&crate::policy::Executable],
    files: &[Vec<(&'static str, Sha256Digest)>]) -> io::Result<PreparedHelpers> {
    if unsafe { libc::geteuid() } != 0 { return Err(io::ErrorKind::PermissionDenied.into()); }
    if helpers.is_empty() || helpers.len() > crate::helper_policy::MAX_HELPER_BINDINGS || files.len() != helpers.len() {
        return Err(invalid());
    }
    let anchor = trusted_directory(&anchors.runtime)?;
    let mount = detached_clone(anchor.as_fd(), MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOSYMFOLLOW)?;
    // Same per-file bound as the runtime; a helper table hashes at most 32 files.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut executables = Vec::with_capacity(helpers.len());
    for executable in helpers {
        executables.push(match executable {
            crate::policy::Executable::Elf { entry, digest } =>
                (open_verified(mount.as_fd(), entry.as_str(), *digest, true, deadline)?, None),
            crate::policy::Executable::Script { interpreter, interpreter_digest, script, script_digest } =>
                (open_verified(mount.as_fd(), interpreter.as_str(), *interpreter_digest, true, deadline)?,
                    Some(open_verified(mount.as_fd(), script.as_str(), *script_digest, false, deadline)?)),
        });
    }
    let mut verified = Vec::with_capacity(files.len());
    for list in files {
        let mut opened = Vec::with_capacity(list.len());
        // node and Chromium are ELF executables; the Playwright bundle is a script.
        for (name, digest) in list { opened.push((*name, open_verified(mount.as_fd(), name, *digest, *name != "playwright.cjs", deadline)?)); }
        verified.push(opened);
    }
    Ok(PreparedHelpers { mount, executables, files: verified })
}

/// Resolve only a reviewed workspace selector under the just-attached H work
/// filesystem. The fd remains pinned across open_tree; no gateway host path or
/// supplied mount descriptor is trusted. The selected cwd becomes /work.
/// H4 (H2 note): root-held, read-only detached clone of the run's WHOLE H
/// work tree, taken BEFORE `select_workspace` narrows `storage.work` (so
/// `.forks` is really the fork area beneath it). Never handed to the
/// trampoline or the gateway; the broker resolves beneath it with
/// `RESOLVE_BENEATH|NO_XDEV`. It pins the employee filesystem, so the host
/// drops it (and waits for in-flight copies) before `Storage::detach`.
pub fn work_broker_root(storage: &AttachedStorage) -> io::Result<OwnedFd> {
    if unsafe { libc::geteuid() } != 0 { return Err(io::ErrorKind::PermissionDenied.into()); }
    detached_clone(storage.work.as_fd(), MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC)
}
pub fn select_workspace(storage: &mut AttachedStorage, selection: &duduclaw_isolation::ipc::WorkspaceSelection,
    uid: u32) -> io::Result<()> {
    let fd = workspace_directory(storage.work.as_fd(), &storage.receipt, selection, uid)?;
    storage.work = detached_clone(fd.as_fd(), MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC)?;
    Ok(())
}
fn workspace_directory(work: BorrowedFd<'_>, receipt: &crate::storage::Attachment,
    selection: &duduclaw_isolation::ipc::WorkspaceSelection, uid: u32) -> io::Result<OwnedFd> {
    let relative = selection.storage_relative().map_err(|_| invalid())?;
    let name = CString::new(relative).map_err(|_| invalid())?;
    let how = OpenHow { flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64,
        mode: 0, resolve: 0x08 | 0x04 | 0x02 | 0x01 };
    let fd = owned(unsafe { libc::syscall(libc::SYS_openat2, work.as_raw_fd(), name.as_ptr(),
        &how, mem::size_of::<OpenHow>()) })?;
    let stat = metadata(&fd)?;
    if !stat.is_dir() || stat.uid() != uid || stat.dev() != selection.device || stat.ino() != selection.inode
        || libc::major(stat.dev()) != receipt.device_major || libc::minor(stat.dev()) != receipt.device_minor {
        return Err(invalid());
    }
    Ok(fd)
}

/// F/G publishes a root-owned immutable projection for each employee beneath
/// this anchor. The identity selects an exact hex component; no gateway path,
/// employee workspace or symlink can become the projection source.
fn employee_projection(anchors: &RootPublicationAnchors, employee: EmployeeId) -> io::Result<OwnedFd> {
    let name: String = employee.0.iter().map(|byte| format!("{byte:02x}")).collect();
    let source = trusted_directory(&anchors.projection_root.join(name))?;
    detached_clone(source.as_fd(), MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV
        | MOUNT_ATTR_NOEXEC | MOUNT_ATTR_NOSYMFOLLOW)
}

/// Clone first, then change propagation/attributes: a bad H descriptor can never
/// cause mount_setattr to modify its original attached host mount.
pub(super) fn detached_clone(source: BorrowedFd<'_>, attributes: u64) -> io::Result<OwnedFd> {
    let clone = owned(unsafe { libc::syscall(libc::SYS_open_tree, source.as_raw_fd(), c"".as_ptr(),
        OPEN_TREE_CLONE | libc::O_CLOEXEC as u32 | AT_RECURSIVE | libc::AT_EMPTY_PATH as u32) })?;
    let private = MountAttr { attr_set: 0, attr_clr: 0, propagation: libc::MS_PRIVATE, userns_fd: 0 };
    set_attributes(clone.as_fd(), &private)?;
    let restricted = MountAttr { attr_set: attributes, attr_clr: 0, propagation: 0, userns_fd: 0 };
    set_attributes(clone.as_fd(), &restricted)?;
    let stat = nix::sys::statfs::fstatfs(clone.as_fd())?;
    let flags = stat.flags().bits();
    let expected = (if attributes & MOUNT_ATTR_RDONLY != 0 { libc::ST_RDONLY } else { 0 })
        | (if attributes & MOUNT_ATTR_NOSUID != 0 { libc::ST_NOSUID } else { 0 })
        | (if attributes & MOUNT_ATTR_NODEV != 0 { libc::ST_NODEV } else { 0 })
        | (if attributes & MOUNT_ATTR_NOEXEC != 0 { libc::ST_NOEXEC } else { 0 });
    if flags & expected != expected { return Err(invalid()); }
    // Detached mounts must not be visible in the caller's mount namespace.
    let id = crate::fds::mount_id(clone.as_fd())?;
    let mut table = String::new();
    File::open("/proc/thread-self/mountinfo")?.take(16 * 1024 * 1024 + 1).read_to_string(&mut table)?;
    if table.len() > 16 * 1024 * 1024 || table.lines().any(|line| line.split_whitespace().next()
        .and_then(|value| value.parse::<u64>().ok()) == Some(id)) { return Err(invalid()); }
    Ok(clone)
}
fn set_attributes(fd: BorrowedFd<'_>, attr: &MountAttr) -> io::Result<()> {
    let result = unsafe { libc::syscall(libc::SYS_mount_setattr, fd.as_raw_fd(), c"".as_ptr(),
        libc::AT_EMPTY_PATH as u32 | AT_RECURSIVE, attr as *const MountAttr, mem::size_of::<MountAttr>()) };
    if result != 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}
pub(crate) fn open_verified(runtime: BorrowedFd<'_>, name: &str, digest: Sha256Digest, elf: bool, deadline: Instant) -> io::Result<VerifiedExecutable> {
    if name.is_empty() || matches!(name, "." | "..") || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) {
        return Err(invalid());
    }
    let name = CString::new(name).map_err(|_| invalid())?;
    let how = OpenHow { flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64, mode: 0,
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_XDEV };
    let fd = owned(unsafe { libc::syscall(libc::SYS_openat2, runtime.as_raw_fd(), name.as_ptr(), &how, mem::size_of::<OpenHow>()) })?;
    let before = identity(fd.as_fd())?;
    if before.mode & libc::S_IFMT != libc::S_IFREG || before.uid != 0 || before.mode & 0o022 != 0
        || before.bytes == 0 || before.bytes > MAX_EXECUTABLE_BYTES || before.mount_id != crate::fds::mount_id(runtime)? {
        return Err(invalid());
    }
    let mut file = File::from(fd);
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    let mut header = [0u8; 4];
    loop {
        if Instant::now() >= deadline { return Err(io::ErrorKind::TimedOut.into()); }
        let read = file.read(&mut buffer)?;
        if read == 0 { break; }
        if total == 0 && read >= 4 { header.copy_from_slice(&buffer[..4]); }
        total += read as u64;
        if total > before.bytes { return Err(invalid()); }
        hash.update(&buffer[..read]);
    }
    if total != before.bytes || identity(file.as_fd())? != before || Sha256Digest(hash.finalize().into()) != digest
        || (elf && (header != *b"\x7fELF" || before.mode & 0o111 == 0)) { return Err(invalid()); }
    Ok(VerifiedExecutable { fd: file.into(), identity: before, digest })
}

fn run_name(run: RunHandle) -> String { run.0.0.iter().map(|byte| format!("{byte:02x}")).collect() }
fn publish_projection(anchors: &RootPublicationAnchors, plan: &BootstrapPlan) -> io::Result<OwnedFd> {
    let parent = trusted_directory(&anchors.etc_state)?;
    if metadata(&parent)?.mode() & 0o7777 != 0o700 { return Err(invalid()); }
    let name = CString::new(run_name(plan.run)).map_err(|_| invalid())?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
    if result != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) { return Err(io::Error::last_os_error()); }
    let directory = owned(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) } as _)?;
    let stat = metadata(&directory)?;
    if stat.uid() != 0 { return Err(invalid()); }
    if result == 0 && unsafe { libc::fchmod(directory.as_raw_fd(), 0o755) } != 0 { return Err(io::Error::last_os_error()); }
    let stat = metadata(&directory)?;
    if stat.uid() != 0 || stat.mode() & 0o7777 != 0o755 { return Err(invalid()); }
    let content = minimal_etc(plan.uid, plan.gid).map_err(|_| invalid())?;
    for (name, value) in &content { publish_file(directory.as_fd(), name, value.as_bytes())?; }
    // SR5 round 3 follow-up R3-1: `/etc/duduclaw/protected-mode` inside the
    // runtime (see `bootstrap::ETC_MARKER_DIR`), root-owned and read-only.
    publish_marker(directory.as_fd())?;
    let mut top_level = content.clone();
    top_level.insert(ETC_MARKER_DIR, String::new());
    verify_projection_entries(directory.as_fd(), &top_level, MAX_ETC_ENTRIES)?;
    File::from(directory.try_clone()?).sync_all()?;
    File::from(parent).sync_all()?;
    Ok(directory)
}
/// R3-1: `<projection>/duduclaw/protected-mode` (root 0755 dir, root 0444
/// single-link file with exactly [`ETC_MARKER_CONTENT`], nothing else).
fn publish_marker(directory: BorrowedFd<'_>) -> io::Result<()> {
    let name = CString::new(ETC_MARKER_DIR).map_err(|_| invalid())?;
    let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) };
    if result != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) { return Err(io::Error::last_os_error()); }
    let marker_dir = owned(unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) } as _)?;
    if metadata(&marker_dir)?.uid() != 0 { return Err(invalid()); }
    // The unit's UMask narrows mkdirat; the runtime uid must be able to traverse.
    if result == 0 && unsafe { libc::fchmod(marker_dir.as_raw_fd(), 0o755) } != 0 { return Err(io::Error::last_os_error()); }
    let stat = metadata(&marker_dir)?;
    if stat.uid() != 0 || stat.mode() & 0o7777 != 0o755 { return Err(invalid()); }
    publish_file(marker_dir.as_fd(), ETC_MARKER_FILE, ETC_MARKER_CONTENT.as_bytes())?;
    verify_projection_entries(marker_dir.as_fd(),
        &BTreeMap::from([(ETC_MARKER_FILE, ETC_MARKER_CONTENT.to_owned())]), 1)?;
    File::from(marker_dir).sync_all()
}
fn publish_file(directory: BorrowedFd<'_>, name: &str, value: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let name = CString::new(name).map_err(|_| invalid())?;
    let raw = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW, 0o444) };
    if raw >= 0 {
        let mut file = File::from(owned(raw as _)?);
        file.write_all(value)?;
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o444) } != 0 { return Err(io::Error::last_os_error()); }
        file.sync_all()?;
    } else if io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) { return Err(io::Error::last_os_error()); }
    let fd = owned(unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) } as _)?;
    let stat = metadata(&fd)?;
    if !stat.is_file() || stat.uid() != 0 || stat.mode() & 0o7777 != 0o444 || stat.nlink() != 1 || stat.len() != value.len() as u64 { return Err(invalid()); }
    let mut actual = Vec::new(); File::from(fd).take(4096).read_to_end(&mut actual)?;
    if actual != value { return Err(invalid()); }
    Ok(())
}
fn verify_projection_entries(directory: BorrowedFd<'_>, expected: &BTreeMap<&str, String>, limit: usize) -> io::Result<()> {
    // /proc/self/fd is used only for this owned root dirfd, never caller paths.
    let mut count = 0usize;
    for entry in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
        let entry = entry?; count += 1;
        if count > limit || !entry.file_name().to_str().is_some_and(|name| expected.contains_key(name)) { return Err(invalid()); }
    }
    if count != expected.len() { return Err(invalid()); }
    Ok(())
}

/// Read exact immutable MCP bytes from the detached mount which B2 receives.
/// Native destinations have closed flat publication names; their actual home
/// ancestors are read back separately. Writable native config is never accepted.
pub(crate) fn verify_mcp_projection(mounts: &PreparedMounts,
    projection: &duduclaw_mcp_boundary::config::RuntimeProjection) -> io::Result<u64> {
    let mut expected = BTreeMap::new();
    for entry in &projection.files {
        let name = duduclaw_mcp_boundary::config::projection_name(entry).map_err(|_| invalid())?;
        if expected.insert(name, entry.contents.clone()).is_some() { return Err(invalid()); }
    }
    if expected.is_empty() { return Err(invalid()); }
    verify_projection_entries(mounts.projection.as_fd(), &expected, MAX_PROJECTION_FILES)?;
    super::native_home::verify(mounts.home.as_fd(), mounts.projection.as_fd())?;
    let mut flags: libc::statvfs = unsafe { mem::zeroed() };
    if unsafe { libc::fstatvfs(mounts.projection.as_raw_fd(), &mut flags) } != 0
        || flags.f_flag & libc::ST_RDONLY == 0 { return Err(invalid()); }
    let mount_id = crate::fds::mount_id(mounts.projection.as_fd())?;
    for (name, contents) in expected {
        let digest = Sha256Digest(Sha256::digest(contents.as_bytes()).into());
        let opened = open_verified(mounts.projection.as_fd(), name, digest, false, Instant::now() + Duration::from_secs(5))?;
        if opened.identity.bytes != contents.len() as u64 || opened.identity.mount_id != mount_id { return Err(invalid()); }
    }
    Ok(mount_id)
}

/// Enumerate crash residues without accepting arbitrary directory names. The
/// caller reconciles units/UIDs/storage before deleting any of these projections.
pub fn projection_runs(anchors: &RootPublicationAnchors) -> io::Result<Vec<RunHandle>> {
    let parent = trusted_directory(&anchors.etc_state)?;
    let mut runs = Vec::new();
    for entry in fs::read_dir(format!("/proc/self/fd/{}", parent.as_raw_fd()))? {
        let entry = entry?;
        if runs.len() >= 4096 { return Err(invalid()); }
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(invalid)?;
        let run = crate::systemd::parse_run_unit(&format!("duduclaw-run-{name}.service")).ok_or_else(invalid)?;
        let stat = fs::symlink_metadata(entry.path())?;
        if !stat.is_dir() || stat.uid() != 0 || !matches!(stat.mode() & 0o7777, 0o700 | 0o755) { return Err(invalid()); }
        runs.push(RunHandle(run));
    }
    Ok(runs)
}

/// Run only after B1 has complete cleanup proof. Never recursively delete unknown
/// content; unexpected files or types retain the residue and fail readiness.
pub fn cleanup_projection(anchors: &RootPublicationAnchors, run: RunHandle) -> io::Result<()> {
    let parent = trusted_directory(&anchors.etc_state)?;
    let name = CString::new(run_name(run)).map_err(|_| invalid())?;
    let raw = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if raw < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) { return Ok(()); }
    let directory = owned(raw as _)?;
    if metadata(&directory)?.uid() != 0 || !matches!(metadata(&directory)?.mode() & 0o7777, 0o700 | 0o755) { return Err(invalid()); }
    let expected = minimal_etc(70000, 70000).map_err(|_| invalid())?;
    // Interrupted creation can leave a subset. Enumerate and validate before
    // unlinking anything; no unknown path is removed during recovery.
    let mut entries = Vec::new();
    let mut marker_dir = false;
    for entry in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
        let entry = entry?;
        // R3-1: the marker directory (possibly partial: empty or just the marker).
        if entry.file_name().to_str() == Some(ETC_MARKER_DIR) && !marker_dir {
            let stat = fs::symlink_metadata(entry.path())?;
            if !stat.is_dir() || stat.uid() != 0 || !matches!(stat.mode() & 0o7777, 0o700 | 0o755) { return Err(invalid()); }
            marker_dir = true;
            continue;
        }
        if entries.len() >= MAX_PROJECTION_FILES || !entry.file_name().to_str().is_some_and(|name| expected.contains_key(name)) { return Err(invalid()); }
        let stat = fs::symlink_metadata(entry.path())?;
        if !stat.is_file() || stat.uid() != 0 || !matches!(stat.mode() & 0o7777, 0o400 | 0o444) || stat.nlink() != 1 { return Err(invalid()); }
        entries.push(c_name(&entry.file_name())?);
    }
    if marker_dir { cleanup_marker(directory.as_fd())?; }
    for entry in entries {
        if unsafe { libc::unlinkat(directory.as_raw_fd(), entry.as_ptr(), 0) } != 0 { return Err(io::Error::last_os_error()); }
    }
    File::from(directory).sync_all()?;
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 { return Err(io::Error::last_os_error()); }
    File::from(parent).sync_all()
}

/// R3-1 cleanup: remove `<projection>/duduclaw/{protected-mode}` and the
/// directory. Only the one known root-owned file is accepted; anything else
/// keeps the residue (and readiness fails), as for the flat files.
fn cleanup_marker(directory: BorrowedFd<'_>) -> io::Result<()> {
    let name = CString::new(ETC_MARKER_DIR).map_err(|_| invalid())?;
    let marker_dir = owned(unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) } as _)?;
    if metadata(&marker_dir)?.uid() != 0 { return Err(invalid()); }
    let mut present = false;
    for entry in fs::read_dir(format!("/proc/self/fd/{}", marker_dir.as_raw_fd()))? {
        let entry = entry?;
        if present || entry.file_name().to_str() != Some(ETC_MARKER_FILE) { return Err(invalid()); }
        let stat = fs::symlink_metadata(entry.path())?;
        if !stat.is_file() || stat.uid() != 0 || !matches!(stat.mode() & 0o7777, 0o400 | 0o444) || stat.nlink() != 1 { return Err(invalid()); }
        present = true;
    }
    if present {
        let file = CString::new(ETC_MARKER_FILE).map_err(|_| invalid())?;
        if unsafe { libc::unlinkat(marker_dir.as_raw_fd(), file.as_ptr(), 0) } != 0 { return Err(io::Error::last_os_error()); }
    }
    File::from(marker_dir).sync_all()?;
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sr5r3f_etc_mount_is_read_only_and_never_follows_the_marker_path() {
        for flag in [MOUNT_ATTR_RDONLY, MOUNT_ATTR_NOSUID, MOUNT_ATTR_NODEV, MOUNT_ATTR_NOEXEC, MOUNT_ATTR_NOSYMFOLLOW] {
            assert_eq!(ETC_MOUNT_ATTRIBUTES & flag, flag);
        }
        assert!(MAX_ETC_ENTRIES > minimal_etc(70000, 70000).unwrap().len());
        assert!(!minimal_etc(70000, 70000).unwrap().contains_key(ETC_MARKER_DIR));
    }
    /// R3-1 end-to-end: publish the real /etc projection, clone it exactly as
    /// `prepare` does (read-only/nosymfollow), and read the marker through the
    /// clone at `duduclaw/protected-mode`; then clean it up.
    #[test]
    #[ignore = "run on Linux acceptance host: requires root and CAP_SYS_ADMIN on a disposable VM"]
    fn sr5r3f_etc_projection_publishes_and_cleans_the_marker() {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let root = PathBuf::from(format!("/run/duduclaw-sr5r3f-etc-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let anchors = RootPublicationAnchors::new(root.clone(), root.clone(), root.clone()).unwrap();
        let run = RunHandle(duduclaw_isolation::ids::RunId([0x93; 16]));
        let directory = root.join(run_name(run));
        // Publish via the same helpers `publish_projection` uses.
        fs::create_dir(&directory).unwrap(); fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let dir: OwnedFd = File::open(&directory).unwrap().into();
        for (name, value) in &minimal_etc(70000, 70000).unwrap() { publish_file(dir.as_fd(), name, value.as_bytes()).unwrap(); }
        publish_marker(dir.as_fd()).unwrap();
        let mut top_level = minimal_etc(70000, 70000).unwrap();
        top_level.insert(ETC_MARKER_DIR, String::new());
        verify_projection_entries(dir.as_fd(), &top_level, MAX_ETC_ENTRIES).unwrap();
        let clone = detached_clone(dir.as_fd(), ETC_MOUNT_ATTRIBUTES).unwrap();
        let path = CString::new(format!("{ETC_MARKER_DIR}/{ETC_MARKER_FILE}")).unwrap();
        let fd = owned(unsafe { libc::openat(clone.as_raw_fd(), path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) } as _).unwrap();
        let mut marker = String::new();
        File::from(fd).read_to_string(&mut marker).unwrap();
        assert_eq!(marker, ETC_MARKER_CONTENT);
        // Read-only: the runtime cannot remove or rewrite it.
        let write = unsafe { libc::openat(clone.as_raw_fd(), path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
        assert!(write < 0);
        drop(clone); drop(dir);
        cleanup_projection(&anchors, run).unwrap();
        assert!(!directory.exists());
        fs::remove_dir(root).unwrap();
    }
    /// R3-1 end-to-end on a real mount namespace: with the runtime's /etc
    /// projection attached at /etc (as duduclaw-run does), `/var/lib` hidden
    /// and no `DUDUCLAW_PROTECTED_KEY_MODE`, the latch's system sources — what
    /// a sandboxed `duduclaw mcp-server` evaluates — say "protected". Control:
    /// an empty /etc in the same setup says "unprotected". Forks: run with
    /// `--test-threads=1`.
    #[test]
    #[ignore = "run on Linux acceptance host: requires root and CAP_SYS_ADMIN; run with --test-threads=1"]
    fn sr5r3f_mcp_server_in_sandbox_sees_protected_mode() {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let root = PathBuf::from(format!("/run/duduclaw-sr5r3f-mcp-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = root.join("etc");
        fs::create_dir(&directory).unwrap(); fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let dir: OwnedFd = File::open(&directory).unwrap().into();
        for (name, value) in &minimal_etc(70000, 70000).unwrap() { publish_file(dir.as_fd(), name, value.as_bytes()).unwrap(); }
        publish_marker(dir.as_fd()).unwrap();
        let projection = detached_clone(dir.as_fd(), ETC_MOUNT_ATTRIBUTES).unwrap();
        let projection_fd = projection.as_raw_fd();
        // What `Sources::system()` is inside the sandbox when the env flag is
        // absent or stripped by an MCP `env` block.
        let sandboxed = || duduclaw_core::protected_mode::Sources {
            env: None,
            marker: PathBuf::from(duduclaw_core::protected_mode::MODE_MARKER),
            service_home: PathBuf::from(duduclaw_core::protected_mode::SERVICE_HOME),
        }.decide();
        let child = |with_projection: bool| -> i32 {
            match unsafe { libc::fork() } {
                0 => {
                    let outcome = (|| -> Option<bool> {
                        unsafe {
                            if libc::unshare(libc::CLONE_NEWNS) != 0 { return None; }
                            if libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
                                libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) != 0 { return None; }
                            // The runtime never sees the protected service home.
                            if libc::mount(c"tmpfs".as_ptr(), c"/var/lib".as_ptr(), c"tmpfs".as_ptr(),
                                0, std::ptr::null()) != 0 { return None; }
                            if with_projection {
                                if libc::syscall(libc::SYS_move_mount, projection_fd, c"".as_ptr(),
                                    libc::AT_FDCWD, c"/etc".as_ptr(), 4u32) != 0 { return None; }
                            } else if libc::mount(c"tmpfs".as_ptr(), c"/etc".as_ptr(), c"tmpfs".as_ptr(),
                                0, std::ptr::null()) != 0 { return None; }
                        }
                        Some(sandboxed())
                    })();
                    unsafe { libc::_exit(match outcome { Some(true) => 0, Some(false) => 1, None => 2 }) }
                }
                pid if pid > 0 => {
                    let mut status = 0;
                    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                    assert!(libc::WIFEXITED(status));
                    libc::WEXITSTATUS(status)
                }
                _ => panic!("fork failed"),
            }
        };
        assert_eq!(child(false), 1, "control: without the projection marker the sandbox is unprotected");
        assert_eq!(child(true), 0, "the runtime's /etc projection keeps protected mode on");
        drop(projection); drop(dir);
        fs::remove_file(directory.join(ETC_MARKER_DIR).join(ETC_MARKER_FILE)).ok();
        fs::remove_dir(directory.join(ETC_MARKER_DIR)).ok();
        for name in minimal_etc(70000, 70000).unwrap().keys() { fs::remove_file(directory.join(name)).ok(); }
        fs::remove_dir(&directory).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[test]
    fn workspace_selection_reopens_only_the_exact_h_directory() {
        use duduclaw_isolation::{bounded::Text, ids::{RunId, RequestId}, ipc::WorkspaceSelection};
        let base = std::env::temp_dir().join(format!("duduclaw-workspace-r5-{}", std::process::id()));
        fs::create_dir(&base).unwrap();
        let forks = base.join(".forks"); fs::create_dir(&forks).unwrap();
        let branch = forks.join("a".repeat(32)); fs::create_dir(&branch).unwrap();
        let root = File::open(&base).unwrap();
        let stat = fs::metadata(&branch).unwrap();
        let receipt = crate::storage::Attachment { employee: EmployeeId([1;16]), run: RunHandle(RunId([2;16])),
            token: RequestId([3;16]), device_major: libc::major(stat.dev()), device_minor: libc::minor(stat.dev()) };
        let mut selection = WorkspaceSelection { handle: Text::new(format!("fork-{}", "a".repeat(32))).unwrap(),
            relative_cwd: Text::new(".".into()).unwrap(), device: stat.dev(), inode: stat.ino() };
        assert!(workspace_directory(root.as_fd(), &receipt, &selection, stat.uid()).is_ok());
        selection.inode += 1;
        assert!(workspace_directory(root.as_fd(), &receipt, &selection, stat.uid()).is_err());
        selection.inode -= 1;
        assert!(workspace_directory(root.as_fd(), &receipt, &selection, stat.uid().saturating_add(1)).is_err());
        let saved = forks.join("saved"); fs::rename(&branch, &saved).unwrap();
        std::os::unix::fs::symlink(&saved, &branch).unwrap();
        assert!(workspace_directory(root.as_fd(), &receipt, &selection, stat.uid()).is_err());
        fs::remove_file(&branch).unwrap(); fs::remove_dir(&saved).unwrap();
        fs::remove_dir(&forks).unwrap(); fs::remove_dir(&base).unwrap();
    }
    #[test]
    fn publication_anchor_rejects_relative_and_symlink_paths() {
        assert!(trusted_directory(Path::new("relative")).is_err());
        assert!(trusted_directory(Path::new("/proc/self/root")).is_err());
    }
    #[test]
    #[ignore = "requires root on disposable Linux VM; creates isolated projection crash residue"]
    fn iso_root_mounts_projection_residue_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let root = PathBuf::from(format!("/run/duduclaw-b1-projection-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let anchors = RootPublicationAnchors::new(root.clone(), root.clone(), root.clone()).unwrap();
        let run = RunHandle(duduclaw_isolation::ids::RunId([0x91; 16]));
        let residue = root.join(run_name(run));
        fs::create_dir(&residue).unwrap(); fs::set_permissions(&residue, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(residue.join("passwd"), b"partial").unwrap();
        fs::set_permissions(residue.join("passwd"), fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(projection_runs(&anchors).unwrap(), vec![run]);
        cleanup_projection(&anchors, run).unwrap();
        assert!(projection_runs(&anchors).unwrap().is_empty());
        cleanup_projection(&anchors, run).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[test]
    #[ignore = "requires root and CAP_SYS_ADMIN on disposable Linux VM"]
    fn iso_root_mounts_private_recursive_readonly_and_exec_identity() {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let root = PathBuf::from(format!("/run/duduclaw-b1-mount-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = fs::read("/usr/bin/true").unwrap();
        fs::write(root.join("fixture"), &executable).unwrap();
        fs::set_permissions(root.join("fixture"), fs::Permissions::from_mode(0o500)).unwrap();
        let digest = Sha256Digest(Sha256::digest(&executable).into());
        let anchors = RootPublicationAnchors::new(root.clone(), root.clone(), root.clone()).unwrap();
        let employee = EmployeeId([0x71; 16]);
        let employee_name: String = employee.0.iter().map(|byte| format!("{byte:02x}")).collect();
        fs::create_dir(root.join(&employee_name)).unwrap();
        fs::set_permissions(root.join(&employee_name), fs::Permissions::from_mode(0o755)).unwrap();
        let projection = employee_projection(&anchors, employee).unwrap();
        let projection_flags = nix::sys::statfs::fstatfs(projection.as_fd()).unwrap().flags().bits();
        assert_eq!(projection_flags & libc::ST_NOEXEC, libc::ST_NOEXEC);
        assert_eq!(projection_flags & libc::ST_RDONLY, libc::ST_RDONLY);
        let other_employee = EmployeeId([0x72; 16]);
        let other_name: String = other_employee.0.iter().map(|byte| format!("{byte:02x}")).collect();
        std::os::unix::fs::symlink(root.join(&employee_name), root.join(&other_name)).unwrap();
        assert!(employee_projection(&anchors, other_employee).is_err());
        let source = trusted_directory(&root).unwrap();
        let mount = detached_clone(source.as_fd(), MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV).unwrap();
        let verified = open_verified(mount.as_fd(), "fixture", digest, true, Instant::now() + Duration::from_secs(15)).unwrap();
        assert_eq!(verified.identity.mount_id, crate::fds::mount_id(mount.as_fd()).unwrap());
        assert_ne!(verified.identity.mount_id, crate::fds::mount_id(source.as_fd()).unwrap());
        assert!(open_verified(mount.as_fd(), "../fixture", digest, true, Instant::now() + Duration::from_secs(15)).is_err());
        assert!(open_verified(mount.as_fd(), "fixture", Sha256Digest([0; 32]), true, Instant::now() + Duration::from_secs(15)).is_err());
        drop(verified); drop(mount); drop(source); drop(projection);
        fs::remove_file(root.join(&other_name)).unwrap(); fs::remove_dir(root.join(&employee_name)).unwrap();
        fs::remove_file(root.join("fixture")).unwrap(); fs::remove_dir(root).unwrap();
    }
}
