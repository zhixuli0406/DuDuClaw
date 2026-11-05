use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::ffi::CStr;
use crate::{Error, Result, fd_plan::{Destination, FdPlan, HIGH_FD_MIN}, validation::*};

pub fn observe(fd: RawFd) -> Result<Observation> {
    // SAFETY: fixed-size kernel output structures and valid NUL-terminated empty path.
    unsafe {
        let mut stat: libc::statx = std::mem::zeroed();
        if libc::statx(fd, c"".as_ptr(), libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_MNT_ID, &mut stat) != 0 { return Err(Error::Descriptors); }
        let mut fs: libc::statfs64 = std::mem::zeroed();
        if libc::fstatfs64(fd, &mut fs) != 0 { return Err(Error::Descriptors); }
        let flags = libc::fcntl(fd, libc::F_GETFL);
        let fdflags = libc::fcntl(fd, libc::F_GETFD);
        if flags == -1 || fdflags == -1 { return Err(Error::Descriptors); }
        let seals = libc::fcntl(fd, libc::F_GET_SEALS);
        let access = if flags & libc::O_PATH != 0 { Access::Path }
            else { match flags & libc::O_ACCMODE {
                libc::O_RDONLY => Access::Read, libc::O_WRONLY => Access::Write,
                libc::O_RDWR => Access::ReadWrite, _ => return Err(Error::Descriptors),
            }};
        let kind = match u32::from(stat.stx_mode) & libc::S_IFMT {
            libc::S_IFREG => FileKind::Regular, libc::S_IFDIR => FileKind::Directory,
            libc::S_IFIFO => FileKind::Fifo, libc::S_IFSOCK => FileKind::Socket,
            libc::S_IFCHR => FileKind::Character, _ => FileKind::Other,
        };
        Ok(Observation { identity: Identity { inode: stat.stx_ino, mount: stat.stx_mnt_id,
            dev_major: stat.stx_dev_major, dev_minor: stat.stx_dev_minor },
            statx_mask: stat.stx_mask, kind, uid: stat.stx_uid, mode: stat.stx_mode,
            nlink: stat.stx_nlink, size: stat.stx_size, access, cloexec: fdflags & libc::FD_CLOEXEC != 0,
            fs_type: fs.f_type as i64, fs_flags: fs.f_flags as u64,
            seals: (seals >= 0).then_some(seals as u32), rdev_major: stat.stx_rdev_major,
            rdev_minor: stat.stx_rdev_minor })
    }
}

pub fn set_cloexec(fd: RawFd) -> Result<()> {
    // SAFETY: fcntl changes only the specified descriptor, checked on return.
    super::checked(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, Error::Descriptors)
}

pub fn soft_limit() -> Result<u64> {
    // SAFETY: initialized output pointer to a kernel resource-limit structure.
    unsafe {
        let mut limit: libc::rlimit = std::mem::zeroed();
        super::checked(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), Error::DescriptorLimit)?;
        Ok(limit.rlim_cur)
    }
}

pub fn close_range(first: u32, last: u32) -> Result<()> {
    if first > last { return Ok(()); }
    // SAFETY: Linux close_range has no pointer arguments; caller owns process fd table.
    let rc = unsafe { libc::syscall(libc::SYS_close_range, first, last, 0u32) };
    if rc == 0 { Ok(()) } else { Err(Error::Descriptors) }
}

/// Close everything except exact descriptors; no /proc enumeration or new fd.
/// Callers must relinquish all other OwnedFd objects before using this function.
pub fn close_except(keep: &[RawFd]) -> Result<()> {
    let mut keep = keep.to_vec();
    keep.sort_unstable(); keep.dedup();
    if keep.iter().any(|fd| *fd < 0) { return Err(Error::Descriptors); }
    let mut first = 0u32;
    for fd in keep {
        if first < fd as u32 { close_range(first, fd as u32 - 1)?; }
        first = fd as u32 + 1;
    }
    close_range(first, u32::MAX)
}

/// Validate connected unnamed socketpairs; a pathname or abstract peer is refused.
pub fn socketpair_endpoint(fd: RawFd) -> Result<()> {
    if socket_option(fd, libc::SO_DOMAIN)? != libc::AF_UNIX
        || socket_option(fd, libc::SO_TYPE)? != libc::SOCK_STREAM { return Err(Error::Descriptors); }
    // SAFETY: buffers are the exact socket address size, length passed by pointer.
    unsafe {
        for peer in [false, true] {
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            let mut len = std::mem::size_of_val(&addr) as libc::socklen_t;
            let rc = if peer { libc::getpeername(fd, (&mut addr as *mut libc::sockaddr_un).cast(), &mut len) }
                else { libc::getsockname(fd, (&mut addr as *mut libc::sockaddr_un).cast(), &mut len) };
            if rc != 0 || len as usize != std::mem::size_of::<libc::sa_family_t>()
                || addr.sun_family as i32 != libc::AF_UNIX { return Err(Error::Descriptors); }
        }
    }
    Ok(())
}

pub fn socket_option(fd: RawFd, option: i32) -> Result<i32> {
    let mut value = 0i32;
    let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
    // SAFETY: option expects an int; kernel receives its exact buffer size.
    if unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, option,
        (&mut value as *mut i32).cast(), &mut len) } != 0 || len as usize != std::mem::size_of::<i32>() {
        return Err(Error::Descriptors);
    }
    Ok(value)
}

#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }

pub fn open_beneath(dir: RawFd, relative: &CStr, flags: i32) -> Result<OwnedFd> {
    let how = OpenHow { flags: (flags | libc::O_CLOEXEC) as u64, mode: 0,
        resolve: 0x08 | 0x04 | 0x02 | 0x01 }; // BENEATH, NO_SYMLINKS, NO_MAGICLINKS, NO_XDEV.
    // SAFETY: immutable pathname and correctly sized open_how, returned fd owned here.
    let fd = unsafe { libc::syscall(libc::SYS_openat2, dir, relative.as_ptr(), &how, std::mem::size_of_val(&how)) };
    if fd < 0 { Err(Error::Descriptors) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) }) }
}

pub fn verify_script(fd: RawFd, relative: &CStr, registry_mount: u64) -> Result<()> {
    // Fixed mount path, not an employee-controlled parent; every subsequent component uses openat2.
    let root = unsafe { libc::open(c"/runtime".as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if root < 0 { return Err(Error::Executable); }
    let root = unsafe { OwnedFd::from_raw_fd(root) };
    let root_identity = observe(root.as_raw_fd())?.identity;
    let reopened = open_beneath(root.as_raw_fd(), relative, libc::O_RDONLY)?;
    let expected = observe(fd)?;
    if root_identity.mount != registry_mount { return Err(Error::Executable); }
    validate(observe(reopened.as_raw_fd())?, Expected::Script { identity: expected.identity, registry_mount })
        .map_err(|_| Error::Executable)
}

/// Consumes every source, breaking cycles before dup3. Script identity is checked
/// after moving the sources high and before closing script/directory descriptors.
/// No descriptor is allocated after the two final close_range calls.
pub fn reorder(plan: FdPlan, owned: Vec<OwnedFd>, script: Option<(&CStr, u64)>) -> Result<OwnedFd> {
    if plan.sources().iter().any(|source| source.destination == Destination::ExecStatus) {
        return Err(Error::Descriptors);
    }
    Ok(reorder_for_exec(plan, owned, script)?.0)
}

/// Preserve one CLOEXEC notification pipe until execveat. It carries no
/// authority and disappears on successful exec; the namespace init waits for
/// EOF before reporting runtime readiness.
pub fn reorder_for_exec(plan: FdPlan, owned: Vec<OwnedFd>, script: Option<(&CStr, u64)>) -> Result<(OwnedFd, Option<OwnedFd>)> {
    if owned.len() != plan.sources().len() || owned.iter().zip(plan.sources()).any(|(fd, s)| fd.as_raw_fd() != s.fd)
        || script.is_some() != plan.sources().iter().any(|s| s.destination == Destination::Script) {
        return Err(Error::Descriptors);
    }
    // Re-check the live soft limit immediately before duplication.
    FdPlan::new(plan.sources().to_vec(), soft_limit()?)?;
    let mut copies = Vec::with_capacity(owned.len());
    let mut identities = Vec::with_capacity(owned.len());
    for fd in &owned {
        identities.push(observe(fd.as_raw_fd())?.identity);
        let high = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, HIGH_FD_MIN) };
        if high < 0 { return Err(Error::Descriptors); }
        copies.push(unsafe { OwnedFd::from_raw_fd(high) });
    }
    drop(owned);
    let mut exec = None;
    let mut exec_status = None;
    let mut expected = Vec::new();
    for ((source, high), identity) in plan.sources().iter().zip(copies).zip(identities) {
        match &source.destination {
            Destination::Runtime(targets) => {
                for target in targets {
                    if unsafe { libc::dup3(high.as_raw_fd(), *target, 0) } != *target { return Err(Error::Descriptors); }
                    expected.push((*target, identity));
                }
            }
            Destination::Executable => { exec = Some(high); }
            Destination::ExecStatus => { exec_status = Some(high); }
            Destination::Script => {
                let (path, mount) = script.ok_or(Error::Executable)?;
                verify_script(high.as_raw_fd(), path, mount)?;
            }
        }
    }
    let exec = exec.ok_or(Error::Descriptors)?;
    let exec_fd = exec.as_raw_fd();
    let status_fd = exec_status.as_ref().map(AsRawFd::as_raw_fd);
    let mut keep: Vec<_> = (0..=plan.last_target()).collect();
    keep.push(exec_fd);
    if let Some(status) = status_fd { keep.push(status); }
    close_except(&keep)?;
    for fd in 0..=exec_fd.max(status_fd.unwrap_or(0)) + 1 {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if fd <= plan.last_target() {
            if flags < 0 || flags & libc::FD_CLOEXEC != 0 { return Err(Error::Descriptors); }
        } else if fd == exec_fd || status_fd == Some(fd) {
            if flags < 0 || flags & libc::FD_CLOEXEC == 0 { return Err(Error::Descriptors); }
        } else if flags != -1 || super::errno() != libc::EBADF { return Err(Error::Descriptors); }
    }
    for (fd, identity) in expected {
        if observe(fd)?.identity != identity { return Err(Error::Descriptors); }
    }
    Ok((exec, exec_status))
}

/// Useful for supervisor/init: consuming handles avoids double-closes after a
/// close_range sweep. The process must immediately enter its terminal loop.
pub fn relinquish(fds: Vec<OwnedFd>) { for fd in fds { let _ = fd.into_raw_fd(); } }
