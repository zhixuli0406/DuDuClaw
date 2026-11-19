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
//! `at.deny` rule still holds); every other error fails closed, including
//! `openat2` itself being unavailable (`ENOSYS`, or `EPERM` from a seccomp
//! filter): there is no fallback to an open that cannot refuse a symlinked
//! ancestor.
//!
//! R8w review round 1 (F1): an entry that was listed and then cannot be
//! inspected for ANY reason, `ENOENT` included, FAILS the round. A vanished
//! entry is not "a job that can no longer run": atd (at 3.2.5, Debian/Ubuntu)
//! opens a due job, UNLINKS it from `atjobs`, and only afterwards creates
//! its output file in `atspool`, so "the entry disappeared" can mean "atd
//! has just committed to run this job as its owner", and a scan that then
//! finishes `atspool` before the output file exists would otherwise pass
//! while the job runs. Nothing is retried within the round (a retry could
//! only turn the failure into a pass). The round is repeated at the
//! caller's normal refresh cadence (`Core::refresh`, allocation), so a benign
//! transient (another account's job dequeued or finished mid-scan) clears at
//! the next round; an employee's job is then seen by its output file in
//! `atspool`.
//!
//! The listing itself is a snapshot (`readdir` may or may not return an entry
//! created or removed after the directory was opened); an entry outside the
//! snapshot is judged by a later round like any other. Only what was listed
//! and could not be inspected is decided here, and it fails.
use std::{ffi::{CStr, CString}, io, mem, os::{fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd}, unix::ffi::OsStrExt}, path::Path};

/// The at spool directories inspected (Debian/Ubuntu, then RHEL/Fedora).
pub const AT_SPOOLS: [&str; 4] = ["/var/spool/cron/atjobs", "/var/spool/cron/atspool", "/var/spool/at", "/var/spool/at/spool"];
/// Entries per spool directory, at most; more fails closed.
pub const MAX_SPOOL_ENTRIES: usize = 65_536;

#[repr(C)]
struct OpenHow { flags: u64, mode: u64, resolve: u64 }

/// The two system calls of the scan whose failures decide the outcome: the
/// production [`System`]; tests inject failures (`ENOSYS`, a vanished or
/// unreadable entry) around it. `readdir` itself is always the real one.
trait SpoolOps {
    /// `openat2(AT_FDCWD, path, how)`.
    fn openat2(&mut self, path: &CStr, how: &OpenHow) -> io::Result<OwnedFd>;
    /// The owner uid of `name` in the open `directory` (`fstatat`, `AT_SYMLINK_NOFOLLOW`).
    fn owner(&mut self, directory: RawFd, name: &CStr) -> io::Result<u32>;
}
/// The real system calls.
struct System;
impl SpoolOps for System {
    fn openat2(&mut self, path: &CStr, how: &OpenHow) -> io::Result<OwnedFd> {
        let raw = unsafe { libc::syscall(libc::SYS_openat2, libc::AT_FDCWD, path.as_ptr(), how, mem::size_of::<OpenHow>()) };
        if raw < 0 { return Err(io::Error::last_os_error()); }
        Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
    }
    fn owner(&mut self, directory: RawFd, name: &CStr) -> io::Result<u32> {
        let mut stat: libc::stat = unsafe { mem::zeroed() };
        if unsafe { libc::fstatat(directory, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(stat.st_uid)
    }
}

/// The spool directory, or `None` when it does not exist (`ENOENT` only;
/// every other error, `ENOSYS` included, is returned).
fn open_spool(ops: &mut dyn SpoolOps, spool: &Path) -> io::Result<Option<OwnedFd>> {
    let path = CString::new(spool.as_os_str().as_bytes()).map_err(|_| super::denied())?;
    let how = OpenHow { flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64, mode: 0,
        resolve: libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS };
    match ops.openat2(&path, &how) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Fails when an entry of the open spool `directory` is owned by `uid`, when
/// a listed entry cannot be inspected (any errno, `ENOENT` included: see the
/// module docs), or when it has more than `limit` entries.
fn spool_clear(ops: &mut dyn SpoolOps, directory: OwnedFd, uid: u32, limit: usize) -> io::Result<()> {
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
        match ops.owner(fd, name) {
            Ok(owner) if owner == uid => return Err(super::denied()),
            Ok(_) => {}
            // Listed, then not inspectable (vanished, EACCES, EIO, …): this
            // round fails closed with the same fixed error (labelled
            // `at_spool` by the caller). Never skipped, never retried here.
            Err(_) => return Err(super::denied()),
        }
    }
}

fn spool_clear_with(ops: &mut dyn SpoolOps, spool: &Path, uid: u32, limit: usize) -> io::Result<()> {
    match open_spool(ops, spool)? {
        None => Ok(()),
        Some(directory) => spool_clear(ops, directory, uid, limit),
    }
}

fn spools_clear_with(ops: &mut dyn SpoolOps, spools: &[&Path], uid: u32, limit: usize) -> io::Result<()> {
    for spool in spools { spool_clear_with(ops, spool, uid, limit)?; }
    Ok(())
}

/// One spool: absent ⇒ `Ok`; otherwise every listed entry must be inspected
/// and none may be owned by `uid`.
pub fn at_spool_clear(spool: &Path, uid: u32, limit: usize) -> io::Result<()> {
    spool_clear_with(&mut System, spool, uid, limit)
}

/// Every spool in `spools` ([`at_spool_clear`]).
pub fn at_spools_clear(spools: &[&Path], uid: u32, limit: usize) -> io::Result<()> {
    spools_clear_with(&mut System, spools, uid, limit)
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

    /// The real system calls, except: `open_error` fails `openat2` with that
    /// errno; an entry in `vanish` is unlinked right before its stat (atd
    /// dequeuing the job between our `readdir` and `fstatat`); an entry in
    /// `stat_errors` fails its stat with that errno. Every stat is recorded.
    #[derive(Default)]
    struct Scripted { open_error: Option<i32>, vanish: Vec<&'static str>, stat_errors: Vec<(&'static str, i32)>, stated: Vec<String> }
    impl SpoolOps for Scripted {
        fn openat2(&mut self, path: &CStr, how: &OpenHow) -> io::Result<OwnedFd> {
            if let Some(errno) = self.open_error { return Err(io::Error::from_raw_os_error(errno)); }
            System.openat2(path, how)
        }
        fn owner(&mut self, directory: RawFd, name: &CStr) -> io::Result<u32> {
            let text = name.to_str().unwrap().to_string();
            self.stated.push(text.clone());
            if self.vanish.iter().any(|entry| *entry == text) {
                assert_eq!(unsafe { libc::unlinkat(directory, name.as_ptr(), 0) }, 0, "fixture unlink of {text}");
            }
            if let Some((_, errno)) = self.stat_errors.iter().find(|(entry, _)| *entry == text) {
                return Err(io::Error::from_raw_os_error(*errno));
            }
            System.owner(directory, name)
        }
    }
    /// A spool with daemon-style entries the checked uid does NOT own: only
    /// an inspection failure can fail it.
    fn foreign_spool() -> (tempfile::TempDir, std::path::PathBuf, u32) {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("atjobs");
        fs::create_dir(&spool).unwrap();
        for name in [".SEQ", "a0000101a2b3c4", "=0000201a2b3c4"] { fs::write(spool.join(name), b"").unwrap(); }
        let other = own_uid(root.path()).wrapping_add(1);
        (root, spool, other)
    }

    /// R8w round 1 (F1): a listed entry that is gone by its stat (a real
    /// ENOENT: atd unlinked the job it is about to run) fails the round; the
    /// next round, with the entry gone from the listing, passes.
    #[test]
    fn at_spool_entry_that_vanishes_before_its_stat_fails_closed() {
        let (_root, spool, other) = foreign_spool();
        let mut ops = Scripted { vanish: vec!["a0000101a2b3c4"], ..Scripted::default() };
        let error = spool_clear_with(&mut ops, &spool, other, MAX_SPOOL_ENTRIES).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        assert!(ops.stated.contains(&"a0000101a2b3c4".to_string()));
        assert_eq!(fs::symlink_metadata(spool.join("a0000101a2b3c4")).unwrap_err().kind(), io::ErrorKind::NotFound,
            "the stat really saw ENOENT");
        // Also through a list of spools (no later spool can turn it into a pass).
        fs::write(spool.join("a0000101a2b3c4"), b"").unwrap();
        let empty = spool.with_file_name("atspool");
        fs::create_dir(&empty).unwrap();
        let mut ops = Scripted { vanish: vec!["a0000101a2b3c4"], ..Scripted::default() };
        assert!(spools_clear_with(&mut ops, &[spool.as_path(), empty.as_path()], other, MAX_SPOOL_ENTRIES).is_err());
        // The next round (the caller's refresh cadence) no longer lists it.
        spool_clear_with(&mut Scripted::default(), &spool, other, MAX_SPOOL_ENTRIES).unwrap();
        at_spool_clear(&spool, other, MAX_SPOOL_ENTRIES).unwrap();
    }

    /// R8w round 1 (F1): a listed entry whose stat fails with any errno
    /// (ENOENT, EACCES, EIO, …) fails the round with the fixed error.
    #[test]
    fn at_spool_entry_that_cannot_be_stated_fails_closed() {
        let (_root, spool, other) = foreign_spool();
        for errno in [libc::ENOENT, libc::EACCES, libc::EIO, libc::ELOOP, libc::ENOMEM] {
            for entry in [".SEQ", "a0000101a2b3c4", "=0000201a2b3c4"] {
                let mut ops = Scripted { stat_errors: vec![(entry, errno)], ..Scripted::default() };
                let error = spool_clear_with(&mut ops, &spool, other, MAX_SPOOL_ENTRIES).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "errno {errno} on {entry}");
                assert_eq!(error.to_string(), super::super::denied().to_string());
            }
        }
    }

    /// R8w round 1 (F1): the pass is "every listed entry was stat'ed and none
    /// is owned by the uid".
    #[test]
    fn at_spool_passes_only_after_every_listed_entry_is_stated() {
        let (_root, spool, other) = foreign_spool();
        let mut ops = Scripted::default();
        spool_clear_with(&mut ops, &spool, other, MAX_SPOOL_ENTRIES).unwrap();
        ops.stated.sort();
        assert_eq!(ops.stated, [".SEQ", "=0000201a2b3c4", "a0000101a2b3c4"]);
    }

    /// R8w round 1: `openat2` unavailable (ENOSYS: an old kernel; EPERM: a
    /// seccomp filter) fails closed, never "no spool"; only ENOENT is absent.
    #[test]
    fn at_spool_without_openat2_fails_closed() {
        let (_root, spool, other) = foreign_spool();
        for errno in [libc::ENOSYS, libc::EPERM, libc::EINVAL, libc::EACCES] {
            let mut ops = Scripted { open_error: Some(errno), ..Scripted::default() };
            let error = spool_clear_with(&mut ops, &spool, other, MAX_SPOOL_ENTRIES).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(errno));
            assert!(ops.stated.is_empty());
            let mut ops = Scripted { open_error: Some(errno), ..Scripted::default() };
            assert!(spools_clear_with(&mut ops, &[spool.as_path()], other, MAX_SPOOL_ENTRIES).is_err());
        }
        let mut ops = Scripted { open_error: Some(libc::ENOENT), ..Scripted::default() };
        spool_clear_with(&mut ops, &spool, other, MAX_SPOOL_ENTRIES).unwrap();
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
