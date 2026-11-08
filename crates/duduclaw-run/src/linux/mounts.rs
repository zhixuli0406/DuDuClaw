//! Disposable mount/network namespaces for the root trampoline (design §4).
//! All names below are fixed; no employee-controlled path is accepted.

use std::ffi::{CStr, CString};
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

const AT_RECURSIVE: u32 = 0x8000;
const OPEN_TREE_CLONE: u32 = 1;
const MOVE_MOUNT_F_EMPTY_PATH: u32 = 4;
const FSOPEN_CLOEXEC: u32 = 1;
const FSCONFIG_SET_STRING: u32 = 1;
const FSCONFIG_CMD_CREATE: u32 = 6;
const MOUNT_RDONLY: u64 = 1;
const MOUNT_NOSUID: u64 = 2;
const MOUNT_NODEV: u64 = 4;
const MOUNT_NOEXEC: u64 = 8;
const MOUNT_NOSYMFOLLOW: u64 = 0x0020_0000;
const TMPFS_MAGIC: i64 = 0x0102_1994;
const DEVPTS_MAGIC: i64 = 0x1cd1;
const PROC_MAGIC: i64 = 0x9fa0;
const TIOCGPTPEER: libc::c_ulong = 0x5441;
const STAGE: &CStr = c"/tmp/duduclaw-root";

/// Sanitized failures; caller adds its fixed state-machine step, never a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountError {
    InvalidPolicy, Namespace, Propagation, Hostname, Filesystem, Directory,
    Mount, Identity, Flags, Device, Pty, Pivot, Loopback,
}

/// A detached mount produced by the authenticated root spawnd, with its readback.
/// The mount id belongs to the clone, not the original host mount (§4 R5-M1).
pub struct DetachedMount {
    pub fd: OwnedFd,
    pub expected_mount_id: u64,
    pub expected_fs_type: i64,
}

/// Ordered, fixed-purpose mount inputs. `etc` must be the minimal projection,
/// including an empty ssl/certs directory, never a clone of host /etc.
pub struct MountInputs {
    pub home: DetachedMount,
    pub work: DetachedMount,
    pub projection: DetachedMount,
    pub runtime: DetachedMount,
    pub etc: DetachedMount,
}

/// Trusted root-registry policy, never deserialized from a gateway request.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub employee_uid: u32,
    pub employee_gid: u32,
    pub pty: bool,
    pub pty_limit: u16,
    pub work_noexec: bool,
    pub tmp_noexec: bool,
    pub tmpfs_bytes: u64,
    pub tmpfs_inodes: u64,
}

impl RootConfig {
    pub fn validate(&self) -> Result<(), MountError> {
        if self.employee_uid == 0 || self.employee_gid == 0 || self.pty_limit == 0
            || self.pty_limit > 256 || self.tmpfs_bytes == 0
            || self.tmpfs_bytes > 1024 * 1024 * 1024 || self.tmpfs_inodes == 0
            || self.tmpfs_inodes > 1_000_000 {
            return Err(MountError::InvalidPolicy);
        }
        Ok(())
    }
}

/// Send master via SCM_RIGHTS to spawnd and close the local master immediately.
pub struct PtyPair {
    pub master: OwnedFd,
    pub slave: OwnedFd,
    pub mount_id: u64,
}

pub struct PreparedRoot {
    root: OwnedFd,
    pty: Option<PtyPair>,
    runtime_mount_id: u64,
    devpts_mount_id: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct RootReadback {
    pub runtime_mount_id: u64,
    pub devpts_mount_id: u64,
}

impl PreparedRoot {
    pub fn take_pty(&mut self) -> Option<PtyPair> { self.pty.take() }

    /// Consumes all mount/directory descriptors before returning (§4 step 5).
    /// The caller must already have returned the PTY master to spawnd.
    pub fn pivot(self) -> Result<RootReadback, MountError> {
        if self.pty.is_some() { return Err(MountError::Pty); }
        // SAFETY: all paths are static and the directory descriptor is owned.
        unsafe {
            cvt(libc::fchdir(self.root.as_raw_fd()), MountError::Pivot)?;
            cvt_long(libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".old".as_ptr()), MountError::Pivot)?;
            cvt(libc::chdir(c"/".as_ptr()), MountError::Pivot)?;
            cvt(libc::umount2(c"/.old".as_ptr(), libc::MNT_DETACH), MountError::Pivot)?;
            cvt(libc::rmdir(c"/.old".as_ptr()), MountError::Pivot)?;
        }
        set_attributes(self.root.as_raw_fd(), MOUNT_RDONLY, false)?;
        verify_mount(self.root.as_raw_fd(), None, TMPFS_MAGIC,
            MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV)?;
        Ok(RootReadback { runtime_mount_id: self.runtime_mount_id,
            devpts_mount_id: self.devpts_mount_id })
    }
}

/// HX helper view (D8 revision §2.1, §2.4; HX2 finding (a)). spawnd's helper
/// registry mount is a clone of the SAME generation directory that backs
/// `/runtime`. Landlock rules attach to inodes, so mounting that clone at
/// `/helpers` would put every helper file beneath the runtime's `/runtime` rule
/// (the directory inode is shared) and the runtime could read and exec them.
/// Instead `/helpers` is a small read-only tmpfs holding one read-only bind per
/// signed helper file: a file bind is its own mount root, so no path to it
/// passes through a directory the runtime has a rule on. The registry clone is
/// only staged to make its files cloneable from this namespace and is detached
/// again before pivot. Helper-only names inside `/runtime` are covered by an
/// empty root-owned mode-0 file.
pub struct HelperView {
    pub registry: DetachedMount,
    /// `(flat name, descriptor)` of every helper executable and script; each fd
    /// must already be verified on `registry`.
    pub files: Vec<(CString, RawFd)>,
    /// Runtime/forwarder executable and script descriptors: never masked.
    pub keep: Vec<RawFd>,
    pub tmpfs_bytes: u64,
}
/// Mount id of each `/helpers/<name>` bind, read back after attaching.
pub struct HelperReadback { pub binds: Vec<(CString, u64)> }

const HELPER_STAGE: &CStr = c".helper-registry";
const HELPER_STAGE_PATH: &CStr = c"/tmp/duduclaw-root/.helper-registry";
const HELPER_MASK: &CStr = c".helper-mask";
const HELPERS_TMPFS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileKey { inode: u64, major: u32, minor: u32 }
fn file_stat(fd: RawFd) -> Result<libc::statx, MountError> {
    let mask = libc::STATX_BASIC_STATS | libc::STATX_MNT_ID;
    // SAFETY: initialized result, AT_EMPTY_PATH addresses the owned fd itself.
    let mut result: libc::statx = unsafe { zeroed() };
    unsafe { cvt(libc::statx(fd, c"".as_ptr(), libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
        mask, &mut result), MountError::Identity)?; }
    if result.stx_mask & mask != mask { return Err(MountError::Identity); }
    Ok(result)
}
fn key(stat: &libc::statx) -> FileKey { FileKey { inode: stat.stx_ino, major: stat.stx_dev_major, minor: stat.stx_dev_minor } }
fn is_regular(stat: &libc::statx) -> bool { u32::from(stat.stx_mode) & libc::S_IFMT == libc::S_IFREG }
fn flat_name(name: &CStr) -> bool {
    let bytes = name.to_bytes();
    !bytes.is_empty() && bytes != b"." && bytes != b".." && bytes.len() <= 255
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
/// O_PATH, no symlinks anywhere, beneath `parent`; `same_mount` adds NO_XDEV.
fn open_entry(parent: RawFd, name: &CStr, same_mount: bool) -> Result<OwnedFd, std::io::Error> {
    let how = OpenHow { flags: (libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64, mode: 0,
        resolve: 0x08 | 0x04 | 0x02 | if same_mount { 0x01 } else { 0 } };
    // SAFETY: initialized open_how v0 and valid C string.
    let raw = unsafe { libc::syscall(libc::SYS_openat2, parent, name.as_ptr(), &how, size_of::<OpenHow>()) };
    if raw < 0 { return Err(std::io::Error::last_os_error()); }
    Ok(unsafe { OwnedFd::from_raw_fd(raw as RawFd) })
}
fn clone_file(fd: RawFd, name: Option<&CStr>) -> Result<OwnedFd, MountError> {
    // Non-recursive bind of exactly this file. The source mount must belong to
    // this namespace (5.15/6.1/6.8 refuse foreign ones), hence the staging.
    let (path, flags) = match name { Some(name) => (name, libc::AT_SYMLINK_NOFOLLOW as u32),
        None => (c"", libc::AT_EMPTY_PATH as u32) };
    owned(unsafe { libc::syscall(libc::SYS_open_tree, fd, path.as_ptr(),
        OPEN_TREE_CLONE | libc::O_CLOEXEC as u32 | flags) }, MountError::Mount)
}

impl PreparedRoot {
    /// Build `/helpers`, `/helper-tmp` and the `/runtime` masks. Before `pivot`, while root.
    pub fn attach_helpers(&self, view: HelperView, config: &RootConfig) -> Result<HelperReadback, MountError> {
        config.validate()?;
        if view.files.is_empty() || view.files.len() > 32 || view.tmpfs_bytes == 0
            || view.tmpfs_bytes > 1024 * 1024 * 1024 { return Err(MountError::InvalidPolicy); }
        let root = self.root.as_raw_fd();
        let mut names: Vec<(CString, RawFd, FileKey, u32)> = Vec::new();
        for (name, fd) in &view.files {
            if !flat_name(name) { return Err(MountError::InvalidPolicy); }
            let stat = file_stat(*fd)?;
            if !is_regular(&stat) || stat.stx_uid != 0 { return Err(MountError::Identity); }
            match names.iter().find(|(known, ..)| known == name) {
                Some((_, _, known, _)) if *known != key(&stat) => return Err(MountError::InvalidPolicy),
                Some(_) => continue,
                None => names.push((name.clone(), *fd, key(&stat), stat.stx_nlink)),
            }
        }
        let keep = view.keep.iter().map(|fd| file_stat(*fd).map(|stat| key(&stat))).collect::<Result<Vec<_>, _>>()?;
        // 1. Stage the registry clone so its files belong to this mount namespace.
        mkdir(root, HELPER_STAGE, 0o700)?;
        attach_input(view.registry, root, HELPER_STAGE, MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOSYMFOLLOW)?;
        // 2. /helpers: private tmpfs of placeholders, one read-only file bind each.
        mkdir(root, c"helpers", 0o755)?;
        let helpers_mount = tmpfs(HELPERS_TMPFS_BYTES, 64, "0755", MOUNT_NOSUID | MOUNT_NODEV)?;
        move_mount(helpers_mount, root, c"helpers")?;
        let helpers = open_dir(root, c"helpers")?;
        let helpers_mount_id = statx(helpers.as_raw_fd())?.stx_mnt_id;
        let file_attrs = MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV;
        let mut binds = Vec::new();
        for (name, fd, expected, _) in &names {
            // SAFETY: new file under the owned tmpfs, exclusive and no symlink.
            drop(owned(unsafe { libc::openat(helpers.as_raw_fd(), name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o444) } as i64,
                MountError::Directory)?);
            let bind = clone_file(*fd, None)?;
            if key(&file_stat(bind.as_raw_fd())?) != *expected { return Err(MountError::Identity); }
            set_private(bind.as_raw_fd(), false)?;
            set_attributes(bind.as_raw_fd(), file_attrs, false)?;
            move_mount(bind, helpers.as_raw_fd(), name)?;
            let readback = open_entry(helpers.as_raw_fd(), name, false).map_err(|_| MountError::Identity)?;
            let stat = file_stat(readback.as_raw_fd())?;
            if key(&stat) != *expected || !is_regular(&stat) || stat.stx_mnt_id == 0 || stat.stx_mnt_id == helpers_mount_id {
                return Err(MountError::Identity);
            }
            verify_flags(readback.as_raw_fd(), file_attrs)?;
            binds.push((name.clone(), stat.stx_mnt_id));
        }
        set_attributes(helpers.as_raw_fd(), MOUNT_RDONLY, false)?;
        verify_mount(helpers.as_raw_fd(), None, TMPFS_MAGIC, MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV)?;
        drop(helpers);
        // 3. Mask helper-only names in /runtime (same registry directory).
        let runtime = open_dir(root, c"runtime")?;
        let mut mask_key = None;
        for (name, _, helper, nlink) in &names {
            if keep.contains(helper) { continue; }
            let target = match open_entry(runtime.as_raw_fd(), name, true) {
                Ok(target) => target,
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => continue,
                Err(_) => return Err(MountError::Identity),
            };
            // A hard-link alias of a helper file would stay reachable unmasked.
            if !is_regular(&file_stat(target.as_raw_fd())?) || *nlink != 1 { return Err(MountError::Identity); }
            drop(target);
            if mask_key.is_none() {
                // SAFETY: new mode-0 root-owned file on the still-writable root tmpfs.
                let mask = owned(unsafe { libc::openat(root, HELPER_MASK.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0) } as i64,
                    MountError::Directory)?;
                unsafe { cvt(libc::fchmod(mask.as_raw_fd(), 0), MountError::Directory)?; }
                mask_key = Some(key(&file_stat(mask.as_raw_fd())?));
            }
            let mask = clone_file(root, Some(HELPER_MASK))?;
            set_private(mask.as_raw_fd(), false)?;
            set_attributes(mask.as_raw_fd(), MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC, false)?;
            move_mount(mask, runtime.as_raw_fd(), name)?;
            let readback = open_entry(runtime.as_raw_fd(), name, false).map_err(|_| MountError::Identity)?;
            if Some(key(&file_stat(readback.as_raw_fd())?)) != mask_key { return Err(MountError::Identity); }
        }
        drop(runtime);
        // 4. /helper-tmp: the launcher's scratch tmpfs, employee-owned, mode 0700.
        mkdir(root, c"helper-tmp", 0o755)?;
        let attrs = MOUNT_NOSUID | MOUNT_NODEV | if config.tmp_noexec { MOUNT_NOEXEC } else { 0 };
        let scratch = tmpfs(view.tmpfs_bytes, config.tmpfs_inodes, "0700", attrs)?;
        move_mount(scratch, root, c"helper-tmp")?;
        let scratch = open_dir(root, c"helper-tmp")?;
        // SAFETY: newly mounted root-owned directory, changed before privilege drop.
        unsafe { cvt(libc::fchown(scratch.as_raw_fd(), config.employee_uid, config.employee_gid), MountError::Directory)?; }
        verify_mount(scratch.as_raw_fd(), None, TMPFS_MAGIC, attrs)?;
        let owner = statx(scratch.as_raw_fd())?;
        if owner.stx_uid != config.employee_uid || owner.stx_mode & 0o7777 != 0o700 { return Err(MountError::Directory); }
        drop(scratch);
        // 5. Detach the staged registry (its directory inode is the runtime's) and the mask source.
        unsafe {
            cvt(libc::umount2(HELPER_STAGE_PATH.as_ptr(), libc::MNT_DETACH), MountError::Mount)?;
            cvt(libc::unlinkat(root, HELPER_STAGE.as_ptr(), libc::AT_REMOVEDIR), MountError::Directory)?;
            if mask_key.is_some() { cvt(libc::unlinkat(root, HELPER_MASK.as_ptr(), 0), MountError::Directory)?; }
        }
        if open_entry(root, HELPER_STAGE, true).is_ok() { return Err(MountError::Mount); }
        Ok(HelperReadback { binds })
    }
}

/// Must run before any cloning or mounting. No user namespace is created.
pub fn enter_namespaces() -> Result<(), MountError> {
    // SAFETY: mutates only this single-threaded trampoline and its descendants.
    unsafe {
        cvt(libc::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWIPC | libc::CLONE_NEWUTS
            | libc::CLONE_NEWNET), MountError::Namespace)?;
        cvt(libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()), MountError::Propagation)?;
        cvt(libc::sethostname(c"duduclaw-run".as_ptr(), 12), MountError::Hostname)?;
        let mut name = [0u8; 64];
        cvt(libc::gethostname(name.as_mut_ptr().cast(), name.len()), MountError::Hostname)?;
        if CStr::from_bytes_until_nul(&name).map_err(|_| MountError::Hostname)? != c"duduclaw-run" {
            return Err(MountError::Hostname);
        }
    }
    Ok(())
}

/// Build the new root using fd-based mount APIs. Caller must have just entered
/// fresh namespaces. Any error is terminal: do not reuse the partially built tree.
pub fn build_root(inputs: MountInputs, config: &RootConfig) -> Result<PreparedRoot, MountError> {
    config.validate()?;
    // Exclusive creation in systemd PrivateTmp avoids following an old tree.
    // SAFETY: static path and mode, with no recursive fallback on EEXIST.
    unsafe { cvt(libc::mkdir(STAGE.as_ptr(), 0o700), MountError::Directory)?; }
    let root_mount = tmpfs(1024 * 1024, 256, "0755", MOUNT_NOSUID | MOUNT_NODEV)?;
    move_mount(root_mount, libc::AT_FDCWD, STAGE)?;
    let root = open_dir(libc::AT_FDCWD, STAGE)?;
    for name in [c".old", c"usr", c"home", c"home/employee", c"work", c"config",
        c"runtime", c"etc", c"run", c"tmp", c"dev", c"proc"] {
        mkdir(root.as_raw_fd(), name, if name == c".old" { 0o700 } else { 0o755 })?;
    }
    system_tree(root.as_raw_fd())?;
    let writable = MOUNT_NOSUID | MOUNT_NODEV;
    attach_input(inputs.home, root.as_raw_fd(), c"home/employee", writable)?;
    attach_input(inputs.work, root.as_raw_fd(), c"work",
        writable | if config.work_noexec { MOUNT_NOEXEC } else { 0 })?;
    let readonly = MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOSYMFOLLOW;
    attach_input(inputs.projection, root.as_raw_fd(), c"config", readonly | MOUNT_NOEXEC)?;
    let runtime_mount_id = attach_input(inputs.runtime, root.as_raw_fd(), c"runtime", readonly)?;
    attach_input(inputs.etc, root.as_raw_fd(), c"etc", readonly | MOUNT_NOEXEC)?;
    // Only this public certificate directory is borrowed from host /etc.
    let ca = clone_tree(c"/etc/ssl/certs", true)?;
    set_private(ca.as_raw_fd(), true)?;
    // Distribution certificate stores use symlinks into /usr/share; unlike the
    // authority projections, CA certificates must not set NOSYMFOLLOW.
    set_attributes(ca.as_raw_fd(), MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV, true)?;
    let etc = open_dir(root.as_raw_fd(), c"etc")?;
    let ca_target = open_dir(etc.as_raw_fd(), c"ssl/certs")?;
    drop(ca_target);
    move_mount(ca, etc.as_raw_fd(), c"ssl/certs")?;
    let ca_readback = open_dir(etc.as_raw_fd(), c"ssl/certs")?;
    verify_flags(ca_readback.as_raw_fd(), MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV)?;
    drop(ca_readback);
    drop(etc);
    for (name, mode, attributes) in [
        (c"run", "0755", writable),
        (c"tmp", "1777", writable | if config.tmp_noexec { MOUNT_NOEXEC } else { 0 }),
    ] {
        let mount = tmpfs(config.tmpfs_bytes, config.tmpfs_inodes, mode, attributes)?;
        move_mount(mount, root.as_raw_fd(), name)?;
        let check = open_dir(root.as_raw_fd(), name)?;
        verify_mount(check.as_raw_fd(), None, TMPFS_MAGIC, attributes)?;
    }
    let run = open_dir(root.as_raw_fd(), c"run")?;
    mkdir(run.as_raw_fd(), c"duduclaw", 0o700)?;
    let xdg = open_dir(run.as_raw_fd(), c"duduclaw")?;
    // SAFETY: a newly created root-owned directory, changed before privilege drop.
    unsafe { cvt(libc::fchown(xdg.as_raw_fd(), config.employee_uid, config.employee_gid), MountError::Directory)?; }
    drop(xdg);
    native_project(root.as_raw_fd(), run.as_raw_fd())?;
    drop(run);
    let (devpts_mount_id, pty) = build_devices(root.as_raw_fd(), config)?;
    Ok(PreparedRoot { root, pty, runtime_mount_id, devpts_mount_id })
}

/// Native CLIs start in a root-owned project, so writable work/home files
/// cannot add another MCP configuration. The workspace stays reachable by link.
fn native_project(root: RawFd, run: RawFd) -> Result<(), MountError> {
    let config = open_dir(root, c"config")?;
    let has = |name: &CStr| -> Result<bool, MountError> {
        let fd = unsafe { libc::openat(config.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd >= 0 { drop(owned(fd as _, MountError::Directory)?); return Ok(true); }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) { return Ok(false); }
        Err(MountError::Directory)
    };
    let codex = has(c"codex-home.toml")?;
    let grok = has(c"grok-home.toml")?;
    if !codex && !grok { return Ok(()); }
    if codex && grok { return Err(MountError::InvalidPolicy); }
    mkdir(run, c"native-project", 0o755)?;
    let project = open_dir(run, c"native-project")?;
    symlink(project.as_raw_fd(), c"/work", c"workspace")?;
    if grok {
        mkdir(project.as_raw_fd(), c".grok", 0o755)?;
        let native = open_dir(project.as_raw_fd(), c".grok")?;
        symlink(native.as_raw_fd(), c"/config/grok-project.toml", c"config.toml")?;
        symlink(project.as_raw_fd(), c"/config/project-mcp.json", c".mcp.json")?;
    }
    Ok(())
}

/// PID 1 calls this after clone3 and before dropping privileges (§4 step 8a).
/// `subset_pid=false` requires an explicit root-published runtime compatibility
/// exception; this boolean must never come from gateway/employee arguments.
pub fn mount_proc(subset_pid: bool) -> Result<(), MountError> {
    let mut parameters = vec![("hidepid", "invisible".to_owned())];
    if subset_pid { parameters.push(("subset", "pid".to_owned())); }
    let attrs = MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC;
    let mount = filesystem(c"proc", &parameters, attrs)?;
    move_mount(mount, libc::AT_FDCWD, c"/proc")?;
    let proc = open_dir(libc::AT_FDCWD, c"/proc")?;
    verify_mount(proc.as_raw_fd(), None, PROC_MAGIC, attrs)?;
    Ok(())
}

fn system_tree(root: RawFd) -> Result<(), MountError> {
    let attrs = MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV;
    let usr = clone_tree(c"/usr", true)?;
    set_private(usr.as_raw_fd(), true)?;
    set_attributes(usr.as_raw_fd(), attrs, true)?;
    move_mount(usr, root, c"usr")?;
    let usr_readback = open_dir(root, c"usr")?;
    verify_flags(usr_readback.as_raw_fd(), attrs)?;
    drop(usr_readback);
    // Preserve usrmerge aliases exactly. Non-usrmerged hosts get separate RO trees.
    for (source, target, alias) in [(c"/bin", c"bin", c"usr/bin"),
        (c"/sbin", c"sbin", c"usr/sbin"), (c"/lib", c"lib", c"usr/lib"),
        (c"/lib64", c"lib64", c"usr/lib64")] {
        // SAFETY: initialized output and static source paths.
        let mut stat: libc::stat = unsafe { zeroed() };
        if unsafe { libc::lstat(source.as_ptr(), &mut stat) } != 0 {
            if source == c"/lib64" && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
                continue;
            }
            return Err(MountError::Identity);
        }
        if stat.st_uid != 0 || stat.st_mode & 0o022 != 0 && stat.st_mode & libc::S_IFMT != libc::S_IFLNK {
            return Err(MountError::Identity);
        }
        match stat.st_mode & libc::S_IFMT {
            libc::S_IFLNK => {
                let mut text = [0u8; 64];
                // SAFETY: bounded initialized buffer; readlink never adds NUL.
                let size = unsafe { libc::readlink(source.as_ptr(), text.as_mut_ptr().cast(), text.len()) };
                if size <= 0 || size as usize >= text.len() { return Err(MountError::Identity); }
                let text = &text[..size as usize];
                let relative = alias.to_bytes();
                let absolute = [b"/".as_slice(), relative].concat();
                if text != relative && text != absolute { return Err(MountError::Identity); }
                symlink(root, alias, target)?;
            }
            libc::S_IFDIR => {
                mkdir(root, target, 0o755)?;
                let mount = clone_tree(source, true)?;
                set_private(mount.as_raw_fd(), true)?;
                set_attributes(mount.as_raw_fd(), attrs, true)?;
                move_mount(mount, root, target)?;
                let readback = open_dir(root, target)?;
                verify_flags(readback.as_raw_fd(), attrs)?;
            }
            _ => return Err(MountError::Identity),
        }
    }
    Ok(())
}

fn build_devices(root: RawFd, config: &RootConfig) -> Result<(u64, Option<PtyPair>), MountError> {
    let dev_mount = tmpfs(1024 * 1024, 128, "0755", MOUNT_NOSUID | MOUNT_NOEXEC)?;
    move_mount(dev_mount, root, c"dev")?;
    let dev = open_dir(root, c"dev")?;
    for (source, name, major, minor) in [
        (c"/dev/null", c"null", 1, 3), (c"/dev/zero", c"zero", 1, 5),
        (c"/dev/full", c"full", 1, 7), (c"/dev/random", c"random", 1, 8),
        (c"/dev/urandom", c"urandom", 1, 9), (c"/dev/tty", c"tty", 5, 0),
    ] {
        let mount = clone_tree(source, false)?;
        let stat = statx(mount.as_raw_fd())?;
        if stat.stx_mode as u32 & libc::S_IFMT != libc::S_IFCHR
            || stat.stx_rdev_major != major || stat.stx_rdev_minor != minor {
            return Err(MountError::Device);
        }
        set_private(mount.as_raw_fd(), false)?;
        set_attributes(mount.as_raw_fd(), MOUNT_NOSUID | MOUNT_NOEXEC, false)?;
        // SAFETY: new file under an owned tmpfs directory, exclusive and no symlink.
        let empty = owned(unsafe { libc::openat(dev.as_raw_fd(), name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) } as i64,
            MountError::Device)?;
        drop(empty);
        move_mount(mount, dev.as_raw_fd(), name)?;
    }
    mkdir(dev.as_raw_fd(), c"pts", 0o755)?;
    let pts = filesystem(c"devpts", &[("ptmxmode", "0666".into()), ("mode", "0620".into()),
        ("max", config.pty_limit.to_string())], MOUNT_NOSUID | MOUNT_NOEXEC)?;
    move_mount(pts, dev.as_raw_fd(), c"pts")?;
    let pts_dir = open_dir(dev.as_raw_fd(), c"pts")?;
    let mount_id = verify_mount(pts_dir.as_raw_fd(), None, DEVPTS_MAGIC, MOUNT_NOSUID | MOUNT_NOEXEC)?;
    drop(pts_dir);
    mkdir(dev.as_raw_fd(), c"shm", 0o1777)?;
    let shm = tmpfs(config.tmpfs_bytes, config.tmpfs_inodes, "1777",
        MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC)?;
    move_mount(shm, dev.as_raw_fd(), c"shm")?;
    let shm_readback = open_dir(dev.as_raw_fd(), c"shm")?;
    verify_mount(shm_readback.as_raw_fd(), None, TMPFS_MAGIC,
        MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC)?;
    drop(shm_readback);
    for (target, name) in [(c"pts/ptmx", c"ptmx"), (c"/proc/self/fd", c"fd"),
        (c"/proc/self/fd/0", c"stdin"), (c"/proc/self/fd/1", c"stdout"),
        (c"/proc/self/fd/2", c"stderr")] {
        symlink(dev.as_raw_fd(), target, name)?;
    }
    let pty = if config.pty { Some(allocate_pty(dev.as_raw_fd(), config.employee_uid, mount_id)?) } else { None };
    // Only the dev root is read-only; devpts and shm remain writable mounts.
    set_attributes(dev.as_raw_fd(), MOUNT_RDONLY, false)?;
    verify_mount(dev.as_raw_fd(), None, TMPFS_MAGIC, MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NOEXEC)?;
    Ok((mount_id, pty))
}

fn allocate_pty(dev: RawFd, uid: u32, mount_id: u64) -> Result<PtyPair, MountError> {
    // SAFETY: new devpts instance; fixed path cannot resolve outside our /dev.
    let master = owned(unsafe { libc::openat(dev, c"pts/ptmx".as_ptr(),
        libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NOFOLLOW) } as i64, MountError::Pty)?;
    let mut unlock: libc::c_int = 0;
    // TIOCSPTLCK is the direct unlockpt operation, without helper execution.
    unsafe { cvt(libc::ioctl(master.as_raw_fd(), libc::TIOCSPTLCK, &mut unlock), MountError::Pty)?; }
    let slave = owned(unsafe { libc::ioctl(master.as_raw_fd(), TIOCGPTPEER,
        libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) } as i64, MountError::Pty)?;
    // R7-M1: root is still owner for chmod. CHOWN, not FOWNER, is required next.
    unsafe {
        cvt(libc::fchmod(slave.as_raw_fd(), 0o620), MountError::Pty)?;
        cvt(libc::fchown(slave.as_raw_fd(), uid, !0 as libc::gid_t), MountError::Pty)?;
    }
    let size = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { cvt(libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size), MountError::Pty)?; }
    let stat = statx(slave.as_raw_fd())?;
    if stat.stx_mode as u32 & libc::S_IFMT != libc::S_IFCHR
        || !(136..=143).contains(&stat.stx_rdev_major) || stat.stx_mnt_id != mount_id
        || stat.stx_uid != uid || stat.stx_mode & 0o7777 != 0o620 {
        return Err(MountError::Pty);
    }
    let master_stat = statx(master.as_raw_fd())?;
    if master_stat.stx_mode as u32 & libc::S_IFMT != libc::S_IFCHR
        || master_stat.stx_rdev_major != 5 || master_stat.stx_rdev_minor != 2
        || master_stat.stx_mnt_id != mount_id {
        return Err(MountError::Pty);
    }
    Ok(PtyPair { master, slave, mount_id })
}

fn attach_input(input: DetachedMount, root: RawFd, name: &CStr, attrs: u64) -> Result<u64, MountError> {
    if input.expected_mount_id == 0 { return Err(MountError::Identity); }
    // Do not turn a normal directory into a source mount: move_mount requires
    // the supplied detached mount root. Both before/after identities must match.
    verify_mount(input.fd.as_raw_fd(), Some(input.expected_mount_id), input.expected_fs_type, attrs)?;
    // Reassert PRIVATE recursively even if spawnd already did so; no shared peer
    // group may survive OPEN_TREE_CLONE (R5-M2). This only narrows propagation.
    set_private(input.fd.as_raw_fd(), true)?;
    set_attributes(input.fd.as_raw_fd(), attrs, true)?;
    move_mount(input.fd, root, name)?;
    let target = open_dir(root, name)?;
    verify_mount(target.as_raw_fd(), Some(input.expected_mount_id), input.expected_fs_type, attrs)
}

fn tmpfs(bytes: u64, inodes: u64, mode: &str, attrs: u64) -> Result<OwnedFd, MountError> {
    filesystem(c"tmpfs", &[("size", bytes.to_string()), ("nr_inodes", inodes.to_string()),
        ("mode", mode.to_owned())], attrs)
}

fn filesystem(kind: &CStr, parameters: &[(&str, String)], attrs: u64) -> Result<OwnedFd, MountError> {
    // SAFETY: borrowed C strings live through each syscall; all returned fds owned.
    let context = owned(unsafe { libc::syscall(libc::SYS_fsopen, kind.as_ptr(), FSOPEN_CLOEXEC) }, MountError::Filesystem)?;
    for (key, value) in parameters {
        let key = CString::new(*key).map_err(|_| MountError::InvalidPolicy)?;
        let value = CString::new(value.as_str()).map_err(|_| MountError::InvalidPolicy)?;
        unsafe { cvt_long(libc::syscall(libc::SYS_fsconfig, context.as_raw_fd(),
            FSCONFIG_SET_STRING, key.as_ptr(), value.as_ptr(), 0), MountError::Filesystem)?; }
    }
    unsafe { cvt_long(libc::syscall(libc::SYS_fsconfig, context.as_raw_fd(), FSCONFIG_CMD_CREATE,
        std::ptr::null::<libc::c_char>(), std::ptr::null::<libc::c_char>(), 0), MountError::Filesystem)?; }
    let mount = owned(unsafe { libc::syscall(libc::SYS_fsmount, context.as_raw_fd(), FSOPEN_CLOEXEC, attrs as u32) },
        MountError::Filesystem)?;
    set_private(mount.as_raw_fd(), true)?;
    Ok(mount)
}

fn clone_tree(path: &CStr, recursive: bool) -> Result<OwnedFd, MountError> {
    // SAFETY: static path from the fixed allowlist, never employee controlled.
    owned(unsafe { libc::syscall(libc::SYS_open_tree, libc::AT_FDCWD, path.as_ptr(),
        OPEN_TREE_CLONE | libc::O_CLOEXEC as u32 | if recursive { AT_RECURSIVE } else { 0 }) }, MountError::Mount)
}

#[repr(C)]
#[derive(Default)]
struct MountAttr { set: u64, clear: u64, propagation: u64, userns_fd: u64 }

fn attributes(fd: RawFd, attr: &MountAttr, recursive: bool) -> Result<(), MountError> {
    // SAFETY: exact kernel mount_attr v0 layout; no uninitialized fields.
    unsafe { cvt_long(libc::syscall(libc::SYS_mount_setattr, fd, c"".as_ptr(),
        libc::AT_EMPTY_PATH as u32 | if recursive { AT_RECURSIVE } else { 0 }, attr,
        size_of::<MountAttr>()), MountError::Flags)?; }
    Ok(())
}

fn set_private(fd: RawFd, recursive: bool) -> Result<(), MountError> {
    attributes(fd, &MountAttr { propagation: libc::MS_PRIVATE, ..Default::default() }, recursive)
        .map_err(|_| MountError::Propagation)
}

fn set_attributes(fd: RawFd, set: u64, recursive: bool) -> Result<(), MountError> {
    attributes(fd, &MountAttr { set, ..Default::default() }, recursive)
}

fn move_mount(source: OwnedFd, target: RawFd, name: &CStr) -> Result<(), MountError> {
    // SAFETY: source is owned; target is a live fd or AT_FDCWD; names are fixed.
    unsafe { cvt_long(libc::syscall(libc::SYS_move_mount, source.as_raw_fd(), c"".as_ptr(),
        target, name.as_ptr(), MOVE_MOUNT_F_EMPTY_PATH), MountError::Mount)?; }
    // source closes here, immediately after move_mount, not after pivot/fork.
    Ok(())
}

#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }

fn open_dir(parent: RawFd, name: &CStr) -> Result<OwnedFd, MountError> {
    // RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS; relative opens also BENEATH.
    let how = OpenHow { flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0, resolve: 0x02 | 0x04 | if parent == libc::AT_FDCWD { 0 } else { 0x08 } };
    // SAFETY: initialized open_how v0 and valid C string.
    owned(unsafe { libc::syscall(libc::SYS_openat2, parent, name.as_ptr(), &how, size_of::<OpenHow>()) },
        MountError::Directory)
}

fn mkdir(parent: RawFd, name: &CStr, mode: u32) -> Result<(), MountError> {
    // SAFETY: owned parent or known new-root fd and fixed name.
    unsafe { cvt(libc::mkdirat(parent, name.as_ptr(), mode), MountError::Directory)?; }
    // The unit has UMask=0077. Explicit chmod is required for traversable fixed
    // parent directories (notably /home), before any employee ownership exists.
    let directory = open_dir(parent, name)?;
    unsafe { cvt(libc::fchmod(directory.as_raw_fd(), mode), MountError::Directory)?; }
    Ok(())
}

fn symlink(parent: RawFd, value: &CStr, name: &CStr) -> Result<(), MountError> {
    // SAFETY: all link values/names are fixed, newly created namespace paths.
    unsafe { cvt(libc::symlinkat(value.as_ptr(), parent, name.as_ptr()), MountError::Directory)?; }
    Ok(())
}

fn statx(fd: RawFd) -> Result<libc::statx, MountError> {
    let required = libc::STATX_TYPE | libc::STATX_MODE | libc::STATX_UID | libc::STATX_INO | libc::STATX_MNT_ID;
    // SAFETY: initialized result, AT_EMPTY_PATH addresses the owned fd itself.
    let mut result: libc::statx = unsafe { zeroed() };
    unsafe { cvt(libc::statx(fd, c"".as_ptr(), libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
        required, &mut result), MountError::Identity)?; }
    if result.stx_mask & required != required { return Err(MountError::Identity); }
    Ok(result)
}

fn verify_mount(fd: RawFd, expected_id: Option<u64>, expected_type: i64, attrs: u64) -> Result<u64, MountError> {
    let stat = statx(fd)?;
    if stat.stx_mode as u32 & libc::S_IFMT != libc::S_IFDIR || stat.stx_mnt_id == 0
        || expected_id.is_some_and(|id| stat.stx_mnt_id != id) {
        return Err(MountError::Identity);
    }
    // SAFETY: initialized statfs buffer and live descriptor.
    let mut fs: libc::statfs64 = unsafe { zeroed() };
    unsafe { cvt(libc::fstatfs64(fd, &mut fs), MountError::Identity)?; }
    if fs.f_type as i64 != expected_type { return Err(MountError::Identity); }
    let required = attrs & (MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC);
    if fs.f_flags as u64 & required != required { return Err(MountError::Flags); }
    Ok(stat.stx_mnt_id)
}

fn verify_flags(fd: RawFd, attrs: u64) -> Result<(), MountError> {
    // SAFETY: initialized statfs buffer and live descriptor.
    let mut fs: libc::statfs64 = unsafe { zeroed() };
    unsafe { cvt(libc::fstatfs64(fd, &mut fs), MountError::Identity)?; }
    let required = attrs & (MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC);
    if fs.f_flags as u64 & required != required { return Err(MountError::Flags); }
    Ok(())
}

fn owned(value: libc::c_long, code: MountError) -> Result<OwnedFd, MountError> {
    cvt_long(value, code)?;
    // SAFETY: successful fd-returning syscall gives sole ownership.
    Ok(unsafe { OwnedFd::from_raw_fd(value as RawFd) })
}

fn cvt(value: libc::c_int, code: MountError) -> Result<(), MountError> {
    if value < 0 { Err(code) } else { Ok(()) }
}

fn cvt_long(value: libc::c_long, code: MountError) -> Result<(), MountError> {
    if value < 0 { Err(code) } else { Ok(()) }
}

/// Bring up only the newly created namespace's loopback through rtnetlink.
/// Request ACK and then read back IFF_UP; any unexpected/truncated reply refuses.
pub fn bring_loopback_up() -> Result<(), MountError> {
    // SAFETY: socket is local to the new namespace and OwnedFd closes on failure.
    let socket = owned(unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC,
        libc::NETLINK_ROUTE) } as i64, MountError::Loopback)?;
    let timeout = libc::timeval { tv_sec: 5, tv_usec: 0 };
    unsafe { cvt(libc::setsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVTIMEO,
        (&timeout as *const libc::timeval).cast(), size_of::<libc::timeval>() as libc::socklen_t), MountError::Loopback)?; }
    let index = unsafe { libc::if_nametoindex(c"lo".as_ptr()) };
    if index == 0 { return Err(MountError::Loopback); }
    let mut set = link_request(libc::RTM_NEWLINK, 1, index);
    set.link.ifi_flags = libc::IFF_UP as u32;
    set.link.ifi_change = libc::IFF_UP as u32;
    let response = netlink_request(socket.as_raw_fd(), &set)?;
    let header = read_header(&response)?;
    if header.nlmsg_type != libc::NLMSG_ERROR as u16 || header.nlmsg_seq != 1
        || response.len() < size_of::<libc::nlmsghdr>() + size_of::<libc::nlmsgerr>() {
        return Err(MountError::Loopback);
    }
    // SAFETY: reply length checked, unaligned read permits arbitrary byte alignment.
    let error = unsafe { std::ptr::read_unaligned(response.as_ptr().add(size_of::<libc::nlmsghdr>()).cast::<libc::nlmsgerr>()) };
    if error.error != 0 { return Err(MountError::Loopback); }
    let mut get = link_request(libc::RTM_GETLINK, 2, index);
    get.header.nlmsg_flags = libc::NLM_F_REQUEST as u16;
    let response = netlink_request(socket.as_raw_fd(), &get)?;
    let header = read_header(&response)?;
    if header.nlmsg_type != libc::RTM_NEWLINK || header.nlmsg_seq != 2
        || response.len() < size_of::<LinkRequest>() { return Err(MountError::Loopback); }
    let link = unsafe { std::ptr::read_unaligned(response.as_ptr().add(size_of::<libc::nlmsghdr>()).cast::<libc::ifinfomsg>()) };
    if link.ifi_index != index as i32 || link.ifi_flags & libc::IFF_UP as u32 == 0
        || link.ifi_flags & libc::IFF_LOOPBACK as u32 == 0 { return Err(MountError::Loopback); }
    Ok(())
}

#[repr(C)]
struct LinkRequest { header: libc::nlmsghdr, link: libc::ifinfomsg }

fn link_request(kind: u16, sequence: u32, index: u32) -> LinkRequest {
    // SAFETY: zero is valid for all integer fields and padding.
    let mut message: LinkRequest = unsafe { zeroed() };
    message.header.nlmsg_len = size_of::<LinkRequest>() as u32;
    message.header.nlmsg_type = kind;
    message.header.nlmsg_flags = (libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16;
    message.header.nlmsg_seq = sequence;
    message.link.ifi_family = libc::AF_UNSPEC as u8;
    message.link.ifi_index = index as i32;
    message
}

fn netlink_request(socket: RawFd, request: &LinkRequest) -> Result<Vec<u8>, MountError> {
    // SAFETY: zeroed sockaddr_nl targets kernel pid 0 with no multicast groups.
    let mut address: libc::sockaddr_nl = unsafe { zeroed() };
    address.nl_family = libc::AF_NETLINK as u16;
    let sent = unsafe { libc::sendto(socket, (request as *const LinkRequest).cast(), size_of::<LinkRequest>(),
        libc::MSG_NOSIGNAL, (&address as *const libc::sockaddr_nl).cast(), size_of::<libc::sockaddr_nl>() as libc::socklen_t) };
    if sent != size_of::<LinkRequest>() as isize { return Err(MountError::Loopback); }
    let mut data = vec![0u8; 8192];
    let mut length = size_of::<libc::sockaddr_nl>() as libc::socklen_t;
    let got = unsafe { libc::recvfrom(socket, data.as_mut_ptr().cast(), data.len(), libc::MSG_TRUNC,
        (&mut address as *mut libc::sockaddr_nl).cast(), &mut length) };
    if got <= 0 || got as usize > data.len() || length as usize != size_of::<libc::sockaddr_nl>()
        || address.nl_family != libc::AF_NETLINK as u16 || address.nl_pid != 0 {
        return Err(MountError::Loopback);
    }
    data.truncate(got as usize);
    Ok(data)
}

fn read_header(data: &[u8]) -> Result<libc::nlmsghdr, MountError> {
    if data.len() < size_of::<libc::nlmsghdr>() { return Err(MountError::Loopback); }
    // SAFETY: checked fixed-width header; arbitrary alignment handled explicitly.
    let header = unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<libc::nlmsghdr>()) };
    if header.nlmsg_len as usize > data.len() || (header.nlmsg_len as usize) < size_of::<libc::nlmsghdr>() {
        return Err(MountError::Loopback);
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_policy_has_finite_inode_memory_and_pty_limits() {
        let mut policy = RootConfig { employee_uid: 70000, employee_gid: 70000,
            pty: true, pty_limit: 16, work_noexec: true, tmp_noexec: true,
            tmpfs_bytes: 16 * 1024 * 1024, tmpfs_inodes: 1024 };
        assert_eq!(policy.validate(), Ok(()));
        policy.pty_limit = 0;
        assert_eq!(policy.validate(), Err(MountError::InvalidPolicy));
        policy.pty_limit = 16;
        policy.employee_uid = 0;
        assert_eq!(policy.validate(), Err(MountError::InvalidPolicy));
        policy.employee_uid = 70000;
        policy.tmpfs_bytes = u64::MAX;
        assert_eq!(policy.validate(), Err(MountError::InvalidPolicy));
    }

    #[test]
    fn rtnetlink_request_changes_only_loopback_up_bit() {
        let request = link_request(libc::RTM_NEWLINK, 42, 1);
        assert_eq!(request.header.nlmsg_len as usize, size_of::<LinkRequest>());
        assert_eq!(request.header.nlmsg_seq, 42);
        assert_eq!(request.link.ifi_index, 1);
        assert_eq!(request.link.ifi_change, 0);
        assert_eq!(read_header(&[]).err(), Some(MountError::Loopback));
    }
}
