use crate::{Error, Result};

pub const SECUREBITS: u64 = 1 | 2 | 4 | 8 | 32 | 64 | 128;
pub const CAP_SETPCAP: u32 = 8;
/// R7-M1: CAP_CHOWN is needed before the drop for the fresh devpts slave.
/// HX [F2]: CAP_SYS_RESOURCE (24) lets the root supervisor take
/// `oom_score_adj = -1000` and lets init reset both the value and the inherited
/// `oom_score_adj_min` to 0 before any employee process exists.
pub const REQUIRED_BOUNDING_CAPABILITIES: &[u32] = &[0, 6, 7, 8, 12, 21, 24];
pub const SUPERVISOR_OOM_SCORE_ADJ: i32 = -1000;

#[repr(C)]
struct CapHeader { version: u32, pid: i32 }
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CapData { effective: u32, permitted: u32, inheritable: u32 }

fn prctl(option: i32, a: u64, b: u64) -> Result<i32> {
    let result = unsafe { libc::prctl(option, a as libc::c_ulong, b as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong) };
    if result == -1 { Err(Error::Privileges) } else { Ok(result) }
}

pub fn non_dumpable() -> Result<()> {
    prctl(libc::PR_SET_DUMPABLE, 0, 0)?;
    if prctl(libc::PR_GET_DUMPABLE, 0, 0)? != 0 { return Err(Error::Privileges); }
    Ok(())
}

pub fn set_securebits() -> Result<()> {
    prctl(libc::PR_SET_SECUREBITS, SECUREBITS, 0)?;
    if prctl(libc::PR_GET_SECUREBITS, 0, 0)? as u64 != SECUREBITS { return Err(Error::Privileges); }
    Ok(())
}

fn last_capability() -> Result<u32> {
    for cap in 0..64u32 {
        let result = unsafe { libc::prctl(libc::PR_CAPBSET_READ, cap as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong) };
        if result == -1 {
            if super::errno() == libc::EINVAL && cap > 21 { return Ok(cap - 1); }
            return Err(Error::Privileges);
        }
    }
    Err(Error::Privileges)
}

pub fn require_root_capabilities() -> Result<()> {
    if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 { return Err(Error::Privileges); }
    let data = capabilities()?;
    for cap in REQUIRED_BOUNDING_CAPABILITIES {
        if data[(*cap / 32) as usize].effective & (1 << (*cap % 32)) == 0
            || prctl(libc::PR_CAPBSET_READ, *cap as u64, 0)? != 1 { return Err(Error::Privileges); }
    }
    Ok(())
}

pub fn drop_bounding() -> Result<()> {
    let last = last_capability()?;
    for cap in 0..=last {
        if cap != CAP_SETPCAP { prctl(libc::PR_CAPBSET_DROP, cap as u64, 0)?; }
    }
    prctl(libc::PR_CAPBSET_DROP, CAP_SETPCAP as u64, 0)?;
    Ok(())
}

pub fn set_identity(uid: u32, gid: u32) -> Result<()> {
    if uid == 0 || gid == 0 { return Err(Error::Privileges); }
    unsafe {
        super::checked(libc::setgroups(0, std::ptr::null()), Error::Privileges)?;
        super::checked(libc::setresgid(gid, gid, gid), Error::Privileges)?;
        super::checked(libc::setresuid(uid, uid, uid), Error::Privileges)?;
    }
    Ok(())
}

pub fn clear_capabilities() -> Result<()> {
    let header = CapHeader { version: 0x20080522, pid: 0 };
    let data = [CapData::default(); 2];
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } != 0 { return Err(Error::Privileges); }
    prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL as u64, 0)?;
    Ok(())
}

fn capabilities() -> Result<[CapData; 2]> {
    let mut header = CapHeader { version: 0x20080522, pid: 0 };
    let mut data = [CapData::default(); 2];
    if unsafe { libc::syscall(libc::SYS_capget, &mut header, data.as_mut_ptr()) } != 0 { return Err(Error::Privileges); }
    Ok(data)
}

pub fn no_new_privileges() -> Result<()> {
    prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0)?;
    Ok(())
}

pub fn verify(uid: u32, gid: u32) -> Result<()> {
    if uid == 0 || gid == 0 { return Err(Error::Privileges); }
    let caps = capabilities()?;
    if caps.iter().any(|c| c.effective != 0 || c.permitted != 0 || c.inheritable != 0) { return Err(Error::Privileges); }
    for cap in 0..=last_capability()? {
        if prctl(libc::PR_CAPBSET_READ, cap as u64, 0)? != 0
            || prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_IS_SET as u64, cap as u64)? != 0 {
            return Err(Error::Privileges);
        }
    }
    unsafe {
        let (mut real, mut effective, mut saved) = (0, 0, 0);
        super::checked(libc::getresuid(&mut real, &mut effective, &mut saved), Error::Privileges)?;
        if [real, effective, saved] != [uid; 3] { return Err(Error::Privileges); }
        super::checked(libc::getresgid(&mut real, &mut effective, &mut saved), Error::Privileges)?;
        if [real, effective, saved] != [gid; 3] || libc::getgroups(0, std::ptr::null_mut()) != 0 { return Err(Error::Privileges); }
    }
    if prctl(libc::PR_GET_DUMPABLE, 0, 0)? != 0
        || prctl(libc::PR_GET_SECUREBITS, 0, 0)? as u64 != SECUREBITS
        || prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0)? != 1 { return Err(Error::Privileges); }
    Ok(())
}

/// Write and read back this process's `oom_score_adj` through a fixed procfs path.
/// Only for the root trampoline: with CAP_SYS_RESOURCE the kernel also sets
/// `oom_score_adj_min` to the written value, which every later fork inherits.
fn set_oom_score_adj(path: &std::ffi::CStr, value: i32) -> Result<()> {
    let text = format!("{value}\n");
    // SAFETY: fixed path; descriptor closed on every path below.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if fd < 0 { return Err(Error::Privileges); }
    let fd = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
    if unsafe { libc::write(raw, text.as_ptr().cast(), text.len()) } != text.len() as isize { return Err(Error::Privileges); }
    let mut back = [0u8; 16];
    let read = unsafe { libc::pread(raw, back.as_mut_ptr().cast(), back.len(), 0) };
    if read <= 0 || back[..read as usize] != *text.as_bytes() { return Err(Error::Privileges); }
    Ok(())
}
/// HX [F2]: only the supervisor (outside the run's PID namespace) is OOM-protected.
/// Called by the root trampoline before pivot (host procfs) and before clone3.
pub fn protect_supervisor_from_oom() -> Result<()> {
    set_oom_score_adj(c"/proc/self/oom_score_adj", SUPERVISOR_OOM_SCORE_ADJ)
}
/// HX [F2]: namespace init resets the inherited protection to 0 while it still
/// holds CAP_SYS_RESOURCE (so `oom_score_adj_min` becomes 0 too), before it forks
/// the forwarder, launcher or runtime. Requires its own procfs (after `mount_proc`).
pub fn reset_oom_score_adj() -> Result<()> { set_oom_score_adj(c"/proc/self/oom_score_adj", 0) }

pub fn drop_all(uid: u32, gid: u32) -> Result<()> {
    set_securebits()?;
    drop_bounding()?;
    set_identity(uid, gid)?;
    clear_capabilities()?;
    non_dumpable()?;
    no_new_privileges()?;
    verify(uid, gid)
}
