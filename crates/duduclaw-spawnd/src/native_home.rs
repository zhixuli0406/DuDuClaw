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
    // The source is a detached private clone. Attaching it inside another
    // detached tree cannot mutate the original H mount or its propagation.
    directory(home.as_fd(), "data", 0, 0)?;
    if unsafe { libc::syscall(libc::SYS_move_mount, persistent.as_raw_fd(), c"".as_ptr(),
        home.as_raw_fd(), c"data".as_ptr(), 4) } != 0 { return Err(io::Error::last_os_error()); }
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
    Ok(home)
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
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn external_codex_detection_refuses_any_project_entry_without_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let fd = std::fs::File::open(dir.path()).unwrap();
        assert!(no_external_codex(fd.as_fd()).is_ok());
        symlink("/definitely-missing-config",dir.path().join(".codex")).unwrap();
        assert!(no_external_codex(fd.as_fd()).is_err());
    }
    #[test]
    #[ignore = "requires root with mount API privileges; detached native home, no provider"]
    fn iso_root_native_home_projects_root_configs_and_refuses_external_codex() {
        use std::io::Write;
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let root = tempfile::tempdir().unwrap();
        for kind in ["codex", "grok"] {
            let base = root.path().join(kind);
            std::fs::create_dir(&base).unwrap();
            for name in ["home", "work", "projection"] { std::fs::create_dir(base.join(name)).unwrap(); }
            let projection_path = base.join("projection");
            let mut file = std::fs::File::create(projection_path.join(format!("{kind}-home.toml"))).unwrap();
            file.write_all(b"test-only-root-config").unwrap();
            drop(file);
            let projection = std::fs::File::open(&projection_path).unwrap();
            let work = std::fs::File::open(base.join("work")).unwrap();
            let persistent = std::fs::File::open(base.join("home")).unwrap();
            let cloned = crate::bootstrap_mounts::detached_clone(persistent.as_fd(), 2 | 4).unwrap();
            let home = prepare(cloned, work.as_fd(), projection.as_fd(), 70000, 70000).unwrap();
            verify(home.as_fd(),projection.as_fd()).unwrap();
            let path = format!("/proc/self/fd/{}/.{kind}/config.toml",home.as_raw_fd());
            assert_eq!(std::fs::read_link(path).unwrap(),std::path::PathBuf::from(format!("/config/{kind}-home.toml")));
            if kind == "codex" {
                symlink("/missing",base.join("work/.codex")).unwrap();
                let cloned = crate::bootstrap_mounts::detached_clone(persistent.as_fd(), 2 | 4).unwrap();
                assert!(prepare(cloned,work.as_fd(),projection.as_fd(),70000,70000).is_err());
            }
        }
    }

}
