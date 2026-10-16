//! File operations on the tmpfs key directory (`/run/duduclaw-maint-ssh`).
//!
//! Everything goes through a directory descriptor opened
//! `O_DIRECTORY|O_NOFOLLOW` and `*at()` calls on plain basenames, so a
//! symlink swapped in for the directory or a file is refused rather than
//! followed. The key is written to a fresh temp file
//! (`O_CREAT|O_EXCL|O_NOFOLLOW`), synced, chowned and chmodded, then moved
//! into place with `renameat2(RENAME_NOREPLACE)` (`renameatx_np(RENAME_EXCL)`
//! on macOS, where the unit tests run): an existing key is never replaced,
//! the caller gets `busy`. The result is read back and compared.

use std::ffi::CString;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Who must own a file or directory, and its exact permission bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ownership {
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}

/// Errors that callers must tell apart.
#[derive(Debug)]
pub enum FsError {
    /// The target already exists (`RENAME_NOREPLACE`).
    Exists,
    /// Ownership, mode or file type is not what it must be.
    Untrusted(String),
    Io(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::Exists => f.write_str("already exists"),
            FsError::Untrusted(m) | FsError::Io(m) => f.write_str(m),
        }
    }
}

fn cstr(s: &str) -> Result<CString, FsError> {
    CString::new(s).map_err(|_| FsError::Io(format!("{s:?} contains a NUL byte")))
}

fn last_err(what: &str) -> FsError {
    FsError::Io(format!("{what}: {}", std::io::Error::last_os_error()))
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Open `path` as a directory without following a final symlink.
fn open_dir_nofollow(path: &Path) -> Result<Option<OwnedFd>, FsError> {
    let c = cstr(&path.to_string_lossy())?;
    // SAFETY: `c` is a valid NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        if errno() == libc::ENOENT {
            return Ok(None);
        }
        return Err(last_err(&format!("open {}", path.display())));
    }
    // SAFETY: `fd` was just returned by open() and is owned by nobody else.
    Ok(Some(unsafe { OwnedFd::from_raw_fd(fd) }))
}

fn fstat(fd: &OwnedFd) -> Result<std::fs::Metadata, FsError> {
    // Borrow the fd as a File without taking ownership.
    let file = std::mem::ManuallyDrop::new(
        // SAFETY: the fd stays owned by `fd`; ManuallyDrop prevents a double close.
        unsafe { std::fs::File::from_raw_fd(fd.as_raw_fd()) },
    );
    file.metadata().map_err(|e| FsError::Io(format!("fstat: {e}")))
}

fn check_ownership(meta: &std::fs::Metadata, want: Ownership, what: &str) -> Result<(), FsError> {
    let mode = meta.mode() & 0o7777;
    if meta.uid() != want.uid || meta.gid() != want.gid || mode != want.mode {
        return Err(FsError::Untrusted(format!(
            "{what} is {}:{} {:04o}, expected {}:{} {:04o}",
            meta.uid(),
            meta.gid(),
            mode,
            want.uid,
            want.gid,
            want.mode
        )));
    }
    Ok(())
}

/// The key directory, opened and verified. Created when missing (`mkdir`,
/// then `fchown`/`fchmod` through the new descriptor).
pub struct KeyDir {
    fd: OwnedFd,
}

impl KeyDir {
    /// Open an existing directory and verify it; `Ok(None)` when it does not
    /// exist.
    pub fn open_existing(path: &Path, want: Ownership) -> Result<Option<KeyDir>, FsError> {
        let Some(fd) = open_dir_nofollow(path)? else { return Ok(None) };
        let meta = fstat(&fd)?;
        if !meta.is_dir() {
            return Err(FsError::Untrusted(format!("{} is not a directory", path.display())));
        }
        check_ownership(&meta, want, &path.display().to_string())?;
        Ok(Some(KeyDir { fd }))
    }

    /// Open (creating if needed) and verify.
    pub fn open_or_create(path: &Path, want: Ownership) -> Result<KeyDir, FsError> {
        if let Some(dir) = Self::open_existing(path, want)? {
            return Ok(dir);
        }
        let c = cstr(&path.to_string_lossy())?;
        // SAFETY: valid path; 0o700 until ownership and mode are set below.
        if unsafe { libc::mkdir(c.as_ptr(), 0o700) } != 0 && errno() != libc::EEXIST {
            return Err(last_err(&format!("mkdir {}", path.display())));
        }
        let fd = open_dir_nofollow(path)?.ok_or_else(|| FsError::Io(format!("{} vanished after mkdir", path.display())))?;
        // SAFETY: `fd` is a valid open directory descriptor.
        if unsafe { libc::fchown(fd.as_raw_fd(), want.uid, want.gid) } != 0 {
            return Err(last_err(&format!("fchown {}", path.display())));
        }
        // SAFETY: as above.
        if unsafe { libc::fchmod(fd.as_raw_fd(), want.mode as libc::mode_t) } != 0 {
            return Err(last_err(&format!("fchmod {}", path.display())));
        }
        let meta = fstat(&fd)?;
        check_ownership(&meta, want, &path.display().to_string())?;
        Ok(KeyDir { fd })
    }

    /// Whether `name` exists (any type, symlinks not followed).
    pub fn exists(&self, name: &str) -> Result<bool, FsError> {
        let c = cstr(name)?;
        // SAFETY: zeroed stat is valid POD; dirfd and name are valid.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatat(self.fd.as_raw_fd(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if rc == 0 {
            return Ok(true);
        }
        if errno() == libc::ENOENT {
            return Ok(false);
        }
        Err(last_err(&format!("stat {name}")))
    }

    /// Remove `name`; a missing file is success.
    pub fn remove(&self, name: &str) -> Result<bool, FsError> {
        let c = cstr(name)?;
        // SAFETY: valid dirfd and name.
        if unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) } == 0 {
            return Ok(true);
        }
        if errno() == libc::ENOENT {
            return Ok(false);
        }
        Err(last_err(&format!("unlink {name}")))
    }

    /// Write `content` to `name` atomically with the given ownership. With
    /// `no_replace`, an existing `name` makes this fail with
    /// [`FsError::Exists`] and leaves it untouched. The written file is read
    /// back and compared; when that read-back fails the file stays in place
    /// and the caller must remove it.
    pub fn write_atomic(&self, name: &str, content: &[u8], want: Ownership, no_replace: bool) -> Result<(), FsError> {
        let tmp = format!(".{name}.tmp");
        // A temp file left by a crash would make O_EXCL fail forever; the
        // directory is root-only writable, so removing it is safe.
        self.remove(&tmp)?;
        let c_tmp = cstr(&tmp)?;
        // SAFETY: valid dirfd and name; the new file starts at 0600.
        let raw = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c_tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if raw < 0 {
            return Err(last_err(&format!("create {tmp}")));
        }
        // SAFETY: `raw` was just returned by openat() and is owned here.
        let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
        let result = (|| {
            file.write_all(content).map_err(|e| FsError::Io(format!("write {tmp}: {e}")))?;
            file.sync_all().map_err(|e| FsError::Io(format!("fsync {tmp}: {e}")))?;
            // SAFETY: valid open descriptor.
            if unsafe { libc::fchown(file.as_raw_fd(), want.uid, want.gid) } != 0 {
                return Err(last_err(&format!("fchown {tmp}")));
            }
            // SAFETY: valid open descriptor.
            if unsafe { libc::fchmod(file.as_raw_fd(), want.mode as libc::mode_t) } != 0 {
                return Err(last_err(&format!("fchmod {tmp}")));
            }
            self.rename(&tmp, name, no_replace)
        })();
        drop(file);
        if result.is_err() {
            let _ = self.remove(&tmp);
            return result;
        }
        self.sync();
        let back = self.read(name, want, content.len() + 1)?;
        if back != content {
            return Err(FsError::Io(format!("{name} read back differs from what was written")));
        }
        Ok(())
    }

    fn rename(&self, from: &str, to: &str, no_replace: bool) -> Result<(), FsError> {
        let (c_from, c_to) = (cstr(from)?, cstr(to)?);
        let dirfd = self.fd.as_raw_fd();
        let rc = if no_replace {
            rename_noreplace(dirfd, &c_from, &c_to)
        } else {
            // SAFETY: valid dirfd and names.
            unsafe { libc::renameat(dirfd, c_from.as_ptr(), dirfd, c_to.as_ptr()) }
        };
        if rc == 0 {
            return Ok(());
        }
        if errno() == libc::EEXIST {
            return Err(FsError::Exists);
        }
        Err(last_err(&format!("rename {from} -> {to}")))
    }

    fn sync(&self) {
        // SAFETY: valid descriptor. Best effort: tmpfs ignores it anyway.
        let _ = unsafe { libc::fsync(self.fd.as_raw_fd()) };
    }

    /// Read `name` (no symlink following), verifying it is a regular file
    /// with the given ownership and at most `max_len` bytes.
    pub fn read(&self, name: &str, want: Ownership, max_len: usize) -> Result<Vec<u8>, FsError> {
        let c = cstr(name)?;
        // SAFETY: valid dirfd and name.
        let raw = unsafe {
            libc::openat(self.fd.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        };
        if raw < 0 {
            return Err(last_err(&format!("open {name}")));
        }
        // SAFETY: `raw` is a fresh descriptor owned here.
        let file = unsafe { std::fs::File::from_raw_fd(raw) };
        let meta = file.metadata().map_err(|e| FsError::Io(format!("fstat {name}: {e}")))?;
        if !meta.is_file() {
            return Err(FsError::Untrusted(format!("{name} is not a regular file")));
        }
        check_ownership(&meta, want, name)?;
        let mut out = Vec::new();
        file.take(max_len as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| FsError::Io(format!("read {name}: {e}")))?;
        if out.len() > max_len {
            return Err(FsError::Untrusted(format!("{name} is larger than {max_len} bytes")));
        }
        Ok(out)
    }
}

#[cfg(target_os = "linux")]
fn rename_noreplace(dirfd: libc::c_int, from: &CString, to: &CString) -> libc::c_int {
    // SAFETY: valid dirfd and NUL-terminated names. tmpfs supports
    // RENAME_NOREPLACE (Linux 3.17+).
    unsafe { libc::renameat2(dirfd, from.as_ptr(), dirfd, to.as_ptr(), libc::RENAME_NOREPLACE) }
}

#[cfg(target_os = "macos")]
fn rename_noreplace(dirfd: libc::c_int, from: &CString, to: &CString) -> libc::c_int {
    // SAFETY: valid dirfd and NUL-terminated names.
    unsafe { libc::renameatx_np(dirfd, from.as_ptr(), dirfd, to.as_ptr(), libc::RENAME_EXCL) }
}

/// Other Unix targets: `linkat` refuses an existing target the same way, then
/// the temp name is dropped.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_noreplace(dirfd: libc::c_int, from: &CString, to: &CString) -> libc::c_int {
    // SAFETY: valid dirfd and names.
    let rc = unsafe { libc::linkat(dirfd, from.as_ptr(), dirfd, to.as_ptr(), 0) };
    if rc == 0 {
        // SAFETY: as above.
        unsafe { libc::unlinkat(dirfd, from.as_ptr(), 0) };
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> (u32, u32) {
        // SAFETY: no preconditions.
        unsafe { (libc::getuid(), libc::getgid()) }
    }

    fn dir_own() -> Ownership {
        let (uid, gid) = me();
        Ownership { uid, gid, mode: 0o750 }
    }

    fn file_own() -> Ownership {
        let (uid, gid) = me();
        Ownership { uid, gid, mode: 0o640 }
    }

    #[test]
    fn creates_the_directory_with_exact_mode_and_reopens_it() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("maint");
        let _ = KeyDir::open_or_create(&p, dir_own()).unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o750);
        assert!(KeyDir::open_existing(&p, dir_own()).unwrap().is_some());
        // Wrong mode is refused, not repaired.
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(matches!(KeyDir::open_existing(&p, dir_own()), Err(FsError::Untrusted(_))));
        assert!(matches!(KeyDir::open_or_create(&p, dir_own()), Err(FsError::Untrusted(_))));
    }

    #[test]
    fn a_symlinked_directory_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = t.path().join("maint");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(KeyDir::open_existing(&link, dir_own()).is_err());
        assert!(KeyDir::open_or_create(&link, dir_own()).is_err());
    }

    #[test]
    fn atomic_write_reads_back_and_refuses_to_replace() {
        let t = tempfile::tempdir().unwrap();
        let dir = KeyDir::open_or_create(&t.path().join("maint"), dir_own()).unwrap();
        dir.write_atomic("authorized_keys", b"one\n", file_own(), true).unwrap();
        let meta = std::fs::metadata(t.path().join("maint/authorized_keys")).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o640);
        assert!(matches!(dir.write_atomic("authorized_keys", b"two\n", file_own(), true), Err(FsError::Exists)));
        assert_eq!(dir.read("authorized_keys", file_own(), 100).unwrap(), b"one\n");
        assert!(!dir.exists(".authorized_keys.tmp").unwrap(), "temp file cleaned up after busy");
        // Replace allowed when asked (window.json).
        dir.write_atomic("authorized_keys", b"three\n", file_own(), false).unwrap();
        assert_eq!(dir.read("authorized_keys", file_own(), 100).unwrap(), b"three\n");
    }

    #[test]
    fn a_stale_temp_file_does_not_block_writing() {
        let t = tempfile::tempdir().unwrap();
        let dir = KeyDir::open_or_create(&t.path().join("maint"), dir_own()).unwrap();
        std::fs::write(t.path().join("maint/.authorized_keys.tmp"), b"junk").unwrap();
        dir.write_atomic("authorized_keys", b"k\n", file_own(), true).unwrap();
    }

    #[test]
    fn symlinked_files_are_never_followed() {
        let t = tempfile::tempdir().unwrap();
        let dir = KeyDir::open_or_create(&t.path().join("maint"), dir_own()).unwrap();
        let outside = t.path().join("outside");
        std::fs::write(&outside, b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, t.path().join("maint/authorized_keys")).unwrap();
        assert!(dir.read("authorized_keys", file_own(), 100).is_err());
        // RENAME_NOREPLACE treats the symlink as an existing name.
        assert!(matches!(dir.write_atomic("authorized_keys", b"k\n", file_own(), true), Err(FsError::Exists)));
        assert_eq!(std::fs::read(&outside).unwrap(), b"secret");
        // A symlink planted at the temp name is removed, not written through.
        std::fs::remove_file(t.path().join("maint/authorized_keys")).unwrap();
        std::os::unix::fs::symlink(&outside, t.path().join("maint/.authorized_keys.tmp")).unwrap();
        dir.write_atomic("authorized_keys", b"k\n", file_own(), true).unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"secret");
    }

    #[test]
    fn wrong_file_mode_is_refused_on_read_and_removal_is_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let dir = KeyDir::open_or_create(&t.path().join("maint"), dir_own()).unwrap();
        dir.write_atomic("authorized_keys", b"k\n", file_own(), true).unwrap();
        std::fs::set_permissions(t.path().join("maint/authorized_keys"), std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
        assert!(matches!(dir.read("authorized_keys", file_own(), 100), Err(FsError::Untrusted(_))));
        assert!(dir.remove("authorized_keys").unwrap());
        assert!(!dir.remove("authorized_keys").unwrap());
        assert!(!dir.exists("authorized_keys").unwrap());
    }
}
