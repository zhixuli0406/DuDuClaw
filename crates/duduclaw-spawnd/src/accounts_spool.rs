//! R8w: the at job check as a direct, read-only inspection of the at spool as
//! root, never by executing `at` (Debian/Ubuntu ship `/usr/bin/at` setuid and
//! setgid `daemon:daemon`; root executes no binary another account owns, see
//! `accounts.rs::linux::command`).
//!
//! Layouts covered ([`AT_SPOOLS`]):
//! - Debian/Ubuntu `at` 3.2.x (jobdir `/var/spool/cron/atjobs`, atspool
//!   `/var/spool/cron/atspool`, both `daemon:daemon` 1770): queued jobs and
//!   the daemon-owned `.SEQ` in `atjobs`, the output file of a running job in
//!   `atspool`.
//! - RHEL/Fedora `at` 3.1/3.2 (jobdir `/var/spool/at`, atspool
//!   `/var/spool/at/spool`): jobs and `.SEQ` in `/var/spool/at`, running-job
//!   output in `spool` (the `spool` entry itself is a root/daemon directory).
//!
//! Why the owner is the whole rule: `at` gives every job file to the
//! submitting user (`fchown(real_uid, real_gid)`), atd runs a job only as the
//! owner of its file (a header/owner mismatch is refused), and atd creates a
//! running job's output file owned by that user. So "no spool entry owned by
//! the employee uid" is "no queued or running at job of the employee". No job
//! content is parsed. Lock and sequence files are owned by daemon/root and
//! simply never match.
//!
//! Each existing spool is opened with `openat2(RESOLVE_NO_SYMLINKS |
//! RESOLVE_NO_MAGICLINKS)` and `O_DIRECTORY | O_NOFOLLOW` (a symlink anywhere
//! in the path, or anything but a directory, fails), enumerated through that
//! descriptor with at most [`MAX_SPOOL_ENTRIES`] entries, and every entry is
//! inspected with `fstatat(AT_SYMLINK_NOFOLLOW)` relative to it. A spool that
//! does not exist contributes nothing (at is not installed there; the
//! `at.deny` rule still holds); every other error fails closed. An entry that
//! vanishes between `readdir` and `fstatat` (a job that just finished) is
//! skipped: it can no longer run.
use std::{ffi::{CStr, CString}, io, mem, os::{fd::{FromRawFd, IntoRawFd, OwnedFd}, unix::ffi::OsStrExt}, path::Path};

/// The at spool directories inspected (Debian/Ubuntu, then RHEL/Fedora).
pub const AT_SPOOLS: [&str; 4] = ["/var/spool/cron/atjobs", "/var/spool/cron/atspool", "/var/spool/at", "/var/spool/at/spool"];
/// Entries per spool directory, at most; more fails closed.
pub const MAX_SPOOL_ENTRIES: usize = 65_536;

#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }

/// The spool directory, or `None` when it does not exist.
fn open_spool(spool: &Path) -> io::Result<Option<OwnedFd>> {
    let path = CString::new(spool.as_os_str().as_bytes()).map_err(|_| super::denied())?;
    let how = OpenHow { flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64, mode: 0,
        resolve: libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS };
    let raw = unsafe { libc::syscall(libc::SYS_openat2, libc::AT_FDCWD, path.as_ptr(), &how, mem::size_of::<OpenHow>()) };
    if raw < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound { Ok(None) } else { Err(error) };
    }
    Ok(Some(unsafe { OwnedFd::from_raw_fd(raw as i32) }))
}

/// Fails when an entry of the open spool `directory` is owned by `uid`, or
/// when it has more than `limit` entries.
fn spool_clear(directory: OwnedFd, uid: u32, limit: usize) -> io::Result<()> {
    let raw = directory.into_raw_fd();
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(raw); }
        return Err(error);
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }
    let stream = Stream(stream);
    let fd = unsafe { libc::dirfd(stream.0) };
    let mut entries = 0usize;
    loop {
        unsafe { *libc::__errno_location() = 0; }
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) { return Err(error); }
            return Ok(());
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") { continue; }
        entries += 1;
        if entries > limit { return Err(super::denied()); }
        let mut stat: libc::stat = unsafe { mem::zeroed() };
        if unsafe { libc::fstatat(fd, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound { continue; }
            return Err(error);
        }
        if stat.st_uid == uid { return Err(super::denied()); }
    }
}

/// One spool: absent ⇒ `Ok`; otherwise no entry may be owned by `uid`.
pub fn at_spool_clear(spool: &Path, uid: u32, limit: usize) -> io::Result<()> {
    match open_spool(spool)? {
        None => Ok(()),
        Some(directory) => spool_clear(directory, uid, limit),
    }
}

/// Every spool in `spools` ([`at_spool_clear`]).
pub fn at_spools_clear(spools: &[&Path], uid: u32, limit: usize) -> io::Result<()> {
    for spool in spools { at_spool_clear(spool, uid, limit)?; }
    Ok(())
}

/// The production rule: every [`AT_SPOOLS`] directory, [`MAX_SPOOL_ENTRIES`] each.
pub fn installed_at_spools_clear(uid: u32) -> io::Result<()> {
    let spools: Vec<&Path> = AT_SPOOLS.iter().map(|spool| Path::new(*spool)).collect();
    at_spools_clear(&spools, uid, MAX_SPOOL_ENTRIES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::{MetadataExt, PermissionsExt}};

    /// The uid owning what this test creates (no root needed: the checked
    /// uid is the test user's own).
    fn own_uid(directory: &Path) -> u32 { fs::metadata(directory).unwrap().uid() }

    #[test]
    fn at_spool_layouts_and_bound_are_fixed() {
        assert_eq!(AT_SPOOLS, ["/var/spool/cron/atjobs", "/var/spool/cron/atspool", "/var/spool/at", "/var/spool/at/spool"]);
        assert_eq!(MAX_SPOOL_ENTRIES, 65_536);
    }

    #[test]
    fn at_spool_absent_or_empty_passes() {
        let root = tempfile::tempdir().unwrap();
        let uid = own_uid(root.path());
        let missing = root.path().join("atjobs");
        at_spool_clear(&missing, uid, MAX_SPOOL_ENTRIES).unwrap();
        // A missing parent is "no spool" too.
        at_spool_clear(&root.path().join("cron/atspool"), uid, MAX_SPOOL_ENTRIES).unwrap();
        let empty = root.path().join("spool");
        fs::create_dir(&empty).unwrap();
        at_spool_clear(&empty, uid, MAX_SPOOL_ENTRIES).unwrap();
        at_spools_clear(&[missing.as_path(), empty.as_path()], uid, MAX_SPOOL_ENTRIES).unwrap();
    }

    #[test]
    fn at_spool_entry_owned_by_the_checked_uid_fails() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("atjobs");
        fs::create_dir(&spool).unwrap();
        let uid = own_uid(root.path());
        let other = uid.wrapping_add(1);
        // A job file (name and content are never read).
        fs::write(spool.join("a0000101a2b3c4"), b"#!/bin/sh\n# atrun uid=1 gid=1\n").unwrap();
        let error = at_spool_clear(&spool, uid, MAX_SPOOL_ENTRIES).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        // Owned by someone else (daemon's `.SEQ`, root's lock): passes.
        at_spool_clear(&spool, other, MAX_SPOOL_ENTRIES).unwrap();
        // Any spool of the list fails the whole rule.
        let empty = root.path().join("atspool");
        fs::create_dir(&empty).unwrap();
        assert!(at_spools_clear(&[empty.as_path(), spool.as_path()], uid, MAX_SPOOL_ENTRIES).is_err());
        fs::remove_file(spool.join("a0000101a2b3c4")).unwrap();
        at_spool_clear(&spool, uid, MAX_SPOOL_ENTRIES).unwrap();
        // A dangling symlink entry is judged by its own owner (lstat, never followed).
        std::os::unix::fs::symlink(root.path().join("absent-target"), spool.join("=link")).unwrap();
        assert!(at_spool_clear(&spool, uid, MAX_SPOOL_ENTRIES).is_err());
        at_spool_clear(&spool, other, MAX_SPOOL_ENTRIES).unwrap();
        fs::remove_file(spool.join("=link")).unwrap();
        // A subdirectory entry (a running job's output dir) counts the same way.
        fs::create_dir(spool.join("output")).unwrap();
        assert!(at_spool_clear(&spool, uid, MAX_SPOOL_ENTRIES).is_err());
    }

    #[test]
    fn at_spool_that_is_not_a_real_readable_directory_fails() {
        let root = tempfile::tempdir().unwrap();
        let uid = own_uid(root.path());
        let other = uid.wrapping_add(1);
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        // A symlink in place of the spool, even to an empty directory.
        let link = root.path().join("atjobs");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(at_spool_clear(&link, other, MAX_SPOOL_ENTRIES).is_err());
        // A symlink above the spool (the directory behind it exists).
        fs::create_dir(real.join("inner")).unwrap();
        assert!(at_spool_clear(&link.join("inner"), other, MAX_SPOOL_ENTRIES).is_err());
        // A regular file in place of the spool.
        let file = root.path().join("at");
        fs::write(&file, b"").unwrap();
        assert!(at_spool_clear(&file, other, MAX_SPOOL_ENTRIES).is_err());
        // An unreadable spool (only observable without DAC override).
        if uid != 0 {
            let closed = root.path().join("closed");
            fs::create_dir(&closed).unwrap();
            fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
            let result = at_spool_clear(&closed, other, MAX_SPOOL_ENTRIES);
            fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(result.is_err());
        }
    }

    #[test]
    fn at_spool_entry_count_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("atjobs");
        fs::create_dir(&spool).unwrap();
        let other = own_uid(root.path()).wrapping_add(1);
        for name in [".SEQ", "a1", "a2"] { fs::write(spool.join(name), b"").unwrap(); }
        at_spool_clear(&spool, other, 3).unwrap();
        let error = at_spool_clear(&spool, other, 2).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
