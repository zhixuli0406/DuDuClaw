//! Private native CLI home. Persistent employee data is a subordinate mount;
//! employee-controlled config is never used as a projection or read by root.
use std::{ffi::CString, io, os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd}};
use crate::bootstrap_mounts::owned;
fn denied() -> io::Error { io::ErrorKind::PermissionDenied.into() }
fn name(value: &str) -> io::Result<CString> { CString::new(value).map_err(|_| denied()) }
fn directory(parent: BorrowedFd<'_>, value: &str, uid: u32, gid: u32) -> io::Result<OwnedFd> {
    let value = name(value)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), value.as_ptr(), 0o755) } != 0 { return Err(io::Error::last_os_error()); }
    let fd = owned(unsafe { libc::openat(parent.as_raw_fd(), value.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) } as _)?;
    if unsafe { libc::fchmod(fd.as_raw_fd(), 0o755) } != 0
        || unsafe { libc::fchown(fd.as_raw_fd(), uid, gid) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(fd)
}
fn link(parent: BorrowedFd<'_>, target: &str, value: &str) -> io::Result<()> {
    let (target, value) = (name(target)?, name(value)?);
    if unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), value.as_ptr()) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}
fn exists(parent: BorrowedFd<'_>, path: &str) -> io::Result<bool> {
    let path = name(path)?;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), path.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd >= 0 { drop(owned(fd as _)?); return Ok(true); }
    if io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) { return Ok(false); }
    Err(io::Error::last_os_error())
}
/// PL-0: a project .codex entry of any type (including a dangling link) is an
/// external configuration source. Do not resolve or parse employee paths as root.
fn no_external_codex(directory: BorrowedFd<'_>) -> io::Result<()> {
    if exists(directory, ".codex")? { return Err(denied()); }
    Ok(())
}
pub(super) fn prepare(persistent: OwnedFd, work: BorrowedFd<'_>, projection: BorrowedFd<'_>, uid: u32, gid: u32)
    -> io::Result<OwnedFd> {
    prepare_with(persistent, work, projection, uid, gid, attach_data)
}
fn prepare_with(persistent: OwnedFd, work: BorrowedFd<'_>, projection: BorrowedFd<'_>, uid: u32, gid: u32,
    compose: fn(BorrowedFd<'_>, BorrowedFd<'_>) -> io::Result<()>) -> io::Result<OwnedFd> {
    let codex = exists(projection, "codex-home.toml")?;
    let grok = exists(projection, "grok-home.toml")?;
    if !codex && !grok { return Ok(persistent); }
    if codex && grok { return Err(denied()); }
    if codex { no_external_codex(work)?; }
    // A new bounded tmpfs, never the employee's persistent directory. Root owns
    // every config ancestor; only explicitly named cache directories are writable.
    let context = owned(unsafe { libc::syscall(libc::SYS_fsopen, c"tmpfs".as_ptr(), 1) })?;
    for (key, value) in [("size", "16777216"), ("nr_inodes", "4096"), ("mode", "0755")] {
        let (key, value) = (name(key)?, name(value)?);
        if unsafe { libc::syscall(libc::SYS_fsconfig, context.as_raw_fd(), 1, key.as_ptr(), value.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if unsafe { libc::syscall(libc::SYS_fsconfig, context.as_raw_fd(), 6,
        std::ptr::null::<libc::c_char>(), std::ptr::null::<libc::c_char>(), 0) } != 0 { return Err(io::Error::last_os_error()); }
    let home = owned(unsafe { libc::syscall(libc::SYS_fsmount, context.as_raw_fd(), 1, 2 | 4 | 8) })?;
    #[repr(C)]
    struct Attr { set: u64, clear: u64, propagation: u64, userns_fd: u64 }
    let private = Attr { set: 0, clear: 0, propagation: libc::MS_PRIVATE, userns_fd: 0 };
    if unsafe { libc::syscall(libc::SYS_mount_setattr, home.as_raw_fd(), c"".as_ptr(),
        libc::AT_EMPTY_PATH as u32 | 0x8000, &private, std::mem::size_of::<Attr>()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // Probe exactly this composition once. Older kernels require the target in
    // the caller's namespace (do_move_mount/check_mnt), not an anonymous tree.
    // Never retry permission/resource errors or change the original H mount.
    directory(home.as_fd(), "data", 0, 0)?;
    if crate::native_home_strategy::needs_attached_composition(compose(persistent.as_fd(), home.as_fd()))? {
        return compose_attached(persistent, home, codex, uid, gid);
    }
    configure(home.as_fd(), codex, uid, gid)?;
    Ok(home)
}
fn attach_data(persistent: BorrowedFd<'_>, home: BorrowedFd<'_>) -> io::Result<()> {
    if unsafe { libc::syscall(libc::SYS_move_mount, persistent.as_raw_fd(), c"".as_ptr(),
        home.as_raw_fd(), c"data".as_ptr(), 4) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}

/// Compose in a disposable namespace, then return a recursive detached clone.
/// Only this fresh thread changes namespaces: the lifecycle worker and B2 retain
/// their existing contract. No employee process is started in this namespace.
fn compose_attached(persistent: OwnedFd, home: OwnedFd, codex: bool, uid: u32, gid: u32) -> io::Result<OwnedFd> {
    std::thread::Builder::new().name("native-home-mount".into()).spawn(move || {
        // CLONE_FS unshares the filesystem context shared by Rust threads;
        // without it CLONE_NEWNS is refused. Never setns back into the daemon.
        if unsafe { libc::unshare(libc::CLONE_FS | libc::CLONE_NEWNS) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // Overmount an existing directory only in this private namespace. No
        // host scratch directory or persistent inode is created/modified. Pin
        // the target after unshare and refuse symlinks (including /tmp itself).
        let target = owned(unsafe { libc::open(c"/tmp".as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) } as _)?;
        if unsafe { libc::syscall(libc::SYS_move_mount, home.as_raw_fd(), c"".as_ptr(),
            target.as_raw_fd(), c"".as_ptr(), 4 | 0x40) } != 0 { return Err(io::Error::last_os_error()); }
        attach_data(persistent.as_fd(), home.as_fd())?;
        configure(home.as_fd(), codex, uid, gid)?;
        // Preserve each mount's flags (home noexec, persistent data unchanged).
        // On success or error the thread exits and its namespace is destroyed;
        // only the completed, private detached clone can reach the caller/B2.
        crate::bootstrap_mounts::detached_clone(home.as_fd(), 0)
    })?.join().map_err(|_| io::Error::other("native home mount worker panicked"))?
}

fn configure(home: BorrowedFd<'_>, codex: bool, uid: u32, gid: u32) -> io::Result<()> {
    let native = if codex { ".codex" } else { ".grok" };
    let config = directory(home.as_fd(), native, 0, 0)?;
    link(config.as_fd(), if codex { "/config/codex-home.toml" } else { "/config/grok-home.toml" }, "config.toml")?;
    if codex {
        link(config.as_fd(), "/home/employee/data/.codex/auth.json", "auth.json")?;
        for entry in ["sessions", "archived_sessions", "log", "tmp"] { directory(config.as_fd(), entry, uid, gid)?; }
    } else {
        // Grok's employee login remains in the employee data mount. No gateway
        // account/env is copied. Exact-version acceptance must exercise this path.
        link(config.as_fd(), "/home/employee/data/.grok/auth.json", "auth.json")?;
        for entry in ["sessions", "cache", "logs"] { directory(config.as_fd(), entry, uid, gid)?; }
    }
    for entry in [".cache", ".local"] { directory(home.as_fd(), entry, uid, gid)?; }
    Ok(())
}

/// Read back the actual home handed to B2, in addition to the immutable /config
/// source. A root receipt never attests an employee-writable native ancestor.
pub(super) fn verify(home: BorrowedFd<'_>, projection: BorrowedFd<'_>) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let codex = exists(projection, "codex-home.toml")?;
    let grok = exists(projection, "grok-home.toml")?;
    if !codex && !grok { return Ok(()); }
    if codex && grok { return Err(denied()); }
    let root = std::fs::File::from(home.try_clone_to_owned()?).metadata()?;
    if !root.is_dir() || root.uid() != 0 || root.mode() & 0o7777 != 0o755 { return Err(denied()); }
    let native = if codex { ".codex" } else { ".grok" };
    let path = format!("/proc/self/fd/{}/{native}", home.as_raw_fd());
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o7777 != 0o755 { return Err(denied()); }
    let path = std::path::Path::new(&path).join("config.toml");
    let metadata = std::fs::symlink_metadata(&path)?;
    let expected = if codex { "/config/codex-home.toml" } else { "/config/grok-home.toml" };
    if !metadata.is_symlink() || metadata.uid() != 0 || std::fs::read_link(&path)? != std::path::Path::new(expected) { return Err(denied()); }
    Ok(())
}

#[cfg(test)]
#[path = "native_home_tests.rs"]
mod tests;
