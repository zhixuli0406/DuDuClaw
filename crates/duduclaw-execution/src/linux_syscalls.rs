//! Raw-syscall replacements for glibc wrappers that are newer than the glibc in
//! the `cross` images used for Linux release builds (`memfd_create` 2.27,
//! `statx`/`renameat2` 2.28, `getrandom` 2.25). A strong reference to any of
//! those symbols fails to link there (`undefined reference`), and would also
//! raise the binary's required GLIBC version (scripts/check_glibc.py). The
//! kernel ABI is identical, so these helpers issue the syscalls directly via
//! `libc::syscall`, which every supported glibc exports.
//!
//! This file is the single source: `duduclaw-core` itself depends on this
//! crate, so `duduclaw_core::linux_syscalls` is a re-export. The minimal root
//! binaries (`duduclaw-run`, `duduclaw-parse-worker`) include this same file
//! with `#[path]` instead of gaining a dependency on this crate, so it must
//! only use `std` and `libc` (tests included).
//!
//! Every integer argument is widened to `c_long`: `syscall` is variadic and the
//! kernel reads full registers.
#![allow(unsafe_code)]

use std::ffi::CStr;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use libc::{c_int, c_long, c_uint};

/// `memfd_create(2)`; returns an owned descriptor.
pub fn memfd_create(name: &CStr, flags: c_uint) -> io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated and outlives the call; no other pointers.
    let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags as c_long) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel just returned a fresh descriptor owned only by us.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// `statx(2)` into `buf`.
pub fn statx(dirfd: RawFd, path: &CStr, flags: c_int, mask: c_uint, buf: &mut libc::statx) -> io::Result<()> {
    // SAFETY: `path` is NUL-terminated and `buf` is a valid, exclusively
    // borrowed `struct statx` for the duration of the call.
    let rc = unsafe {
        libc::syscall(libc::SYS_statx, dirfd as c_long, path.as_ptr(), flags as c_long, mask as c_long,
                      buf as *mut libc::statx)
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `renameat2(2)`.
pub fn renameat2(old_dirfd: RawFd, old: &CStr, new_dirfd: RawFd, new: &CStr, flags: c_uint) -> io::Result<()> {
    // SAFETY: both names are NUL-terminated and outlive the call.
    let rc = unsafe {
        libc::syscall(libc::SYS_renameat2, old_dirfd as c_long, old.as_ptr(), new_dirfd as c_long, new.as_ptr(),
                      flags as c_long)
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `getrandom(2)`; returns the number of bytes written (may be short or
/// `EINTR`, exactly like the syscall).
pub fn getrandom(buf: &mut [u8], flags: c_uint) -> io::Result<usize> {
    // SAFETY: `buf` is a valid writable region of `buf.len()` bytes.
    let rc = unsafe { libc::syscall(libc::SYS_getrandom, buf.as_mut_ptr(), buf.len(), flags as c_long) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn raw_wrappers_work() {
        let fd = memfd_create(c"linux-syscalls-test", libc::MFD_CLOEXEC).unwrap();
        // SAFETY: zeroed `struct statx` is plain integers.
        let mut st: libc::statx = unsafe { std::mem::zeroed() };
        statx(fd.as_raw_fd(), c"", libc::AT_EMPTY_PATH, libc::STATX_TYPE, &mut st).unwrap();
        assert_eq!(u32::from(st.stx_mode) & libc::S_IFMT, libc::S_IFREG);
        let mut bytes = [0u8; 16];
        assert_eq!(getrandom(&mut bytes, 0).unwrap(), bytes.len());
        let mut nonce = [0u8; 8];
        getrandom(&mut nonce, 0).unwrap();
        let dir = std::env::temp_dir().join(format!("linux-syscalls-{}-{}", std::process::id(), u64::from_ne_bytes(nonce)));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a"), b"x").unwrap();
        std::fs::write(dir.join("b"), b"y").unwrap();
        let handle = std::fs::File::open(&dir).unwrap();
        let error = renameat2(handle.as_raw_fd(), c"a", handle.as_raw_fd(), c"b", libc::RENAME_NOREPLACE).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        renameat2(handle.as_raw_fd(), c"a", handle.as_raw_fd(), c"c", libc::RENAME_NOREPLACE).unwrap();
        let moved = dir.join("c").exists();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(moved);
    }
}
