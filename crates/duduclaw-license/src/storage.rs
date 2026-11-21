//! License file persistence — load/save `~/.duduclaw/license.json`.
//!
//! Atomic write-and-rename to avoid half-written files on crash, with
//! 0600 permissions on Unix so the signed payload + customer ID are not
//! world-readable.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json;
use sha2::{Digest, Sha256};

use crate::error::{LicenseError, Result};
use crate::license::License;

/// Default filename inside the DuDuClaw home directory.
pub const DEFAULT_LICENSE_FILENAME: &str = "license.json";

/// Largest `license.json` / revocation marker read, in bytes. A signed
/// licence is well under 2 KiB (the licence object in
/// `tests/fixtures/license_response_v2.json` is about 0.6 KiB); 64 KiB leaves
/// ample room for long identifiers, URLs and future signed fields.
pub const MAX_LICENSE_FILE_BYTES: u64 = 64 * 1024;

/// Pause between attempts of [`LockedLicenseStore::acquire_within`].
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

/// Why a licence store file, or its lock, could not be used.
#[derive(Debug)]
pub enum StoreError {
    /// Another process held `license.json.lock` for the whole deadline.
    Busy,
    /// The file is not regular, exceeds [`MAX_LICENSE_FILE_BYTES`], or an
    /// internal path is a symbolic link. Retrying cannot help until it changes.
    Refused(&'static str),
    /// Any other I/O failure; worth retrying later.
    Io(io::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("license file is locked by another process"),
            Self::Refused(reason) => write!(f, "license file refused: {reason}"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<StoreError> for LicenseError {
    fn from(error: StoreError) -> Self {
        storage_error(error)
    }
}

/// Open `path` for reading without following a final symbolic link and
/// without blocking on a FIFO or device (Unix: `O_NOFOLLOW | O_NONBLOCK`).
fn open_no_follow(options: &mut fs::OpenOptions, path: &Path) -> std::result::Result<fs::File, StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open a link itself, never its target;
        // the regular-file check below then refuses it.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|error| {
        #[cfg(unix)]
        if error.raw_os_error() == Some(libc::ELOOP) {
            return StoreError::Refused("symbolic link");
        }
        StoreError::Io(error)
    })?;
    let meta = file.metadata().map_err(StoreError::Io)?;
    if !meta.file_type().is_file() {
        return Err(StoreError::Refused("not a regular file"));
    }
    #[cfg(unix)]
    clear_nonblocking(&file).map_err(StoreError::Io)?;
    Ok(file)
}

/// Back to blocking mode once the file is known to be a regular file.
#[cfg(unix)]
#[allow(unsafe_code)]
fn clear_nonblocking(file: &fs::File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid descriptor owned by `file` for the duration of
    // both calls; F_GETFL / F_SETFL only read and set its status flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read externally managed licence data, following links (including backups).
/// Only the opened descriptor decides file type; a linked FIFO cannot block.
pub fn read_store_file(path: &Path) -> std::result::Result<Option<Vec<u8>>, StoreError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    read_opened_store_file(options.open(path).map_err(StoreError::Io))
}

/// Internal files (locks and markers) are not operator-managed links, so
/// retain no-follow opens; licence data may be a Kubernetes mounted secret.
pub fn read_store_file_no_follow(path: &Path) -> std::result::Result<Option<Vec<u8>>, StoreError> {
    read_opened_store_file(open_no_follow(fs::OpenOptions::new().read(true), path))
}

fn read_opened_store_file(opened: std::result::Result<fs::File, StoreError>) -> std::result::Result<Option<Vec<u8>>, StoreError> {
    let file = match opened {
        Ok(file) => file,
        // Includes dangling links, as for a missing licence.
        Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let meta = file.metadata().map_err(StoreError::Io)?;
    if !meta.file_type().is_file() {
        return Err(StoreError::Refused("not a regular file"));
    }
    #[cfg(unix)]
    clear_nonblocking(&file).map_err(StoreError::Io)?;
    if meta.len() > MAX_LICENSE_FILE_BYTES {
        return Err(StoreError::Refused("too large"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_LICENSE_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(StoreError::Io)?;
    if bytes.len() as u64 > MAX_LICENSE_FILE_BYTES {
        return Err(StoreError::Refused("too large"));
    }
    Ok(Some(bytes))
}

fn parse_license(bytes: &[u8]) -> Result<License> {
    serde_json::from_slice(bytes)
        .map_err(|e| LicenseError::ParseError(format!("invalid license JSON: {e}")))
}

/// The previous `license.json` worth keeping as the backup, read only when
/// the final component is the regular directory entry itself: never through
/// a symbolic link (an operator-managed link may point at a file this process
/// can read and the link's owner cannot), and only when the descriptor opened
/// is that same entry (device + inode) and the content parses as a licence
/// document. `Ok(None)` = write no backup; `Err` = an I/O failure that aborts
/// the save, as before.
fn backup_candidate(path: &Path) -> std::result::Result<Option<Vec<u8>>, StoreError> {
    let entry = match fs::symlink_metadata(path) {
        Ok(entry) => entry,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::Io(error)),
    };
    if !entry.file_type().is_file() {
        // A link (followed for reads, never copied), FIFO, directory, …
        tracing::debug!("previous license file is not a regular directory entry; no backup written");
        return Ok(None);
    }
    let file = match open_no_follow(fs::OpenOptions::new().read(true), path) {
        Ok(file) => file,
        Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(StoreError::Refused(_)) => {
            tracing::debug!("previous license file changed type while opening; no backup written");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let opened = file.metadata().map_err(StoreError::Io)?;
    if !same_entry(&entry, &opened) {
        tracing::debug!("previous license file was replaced while opening; no backup written");
        return Ok(None);
    }
    let previous = match read_opened_store_file(Ok(file)) {
        Ok(Some(previous)) => previous,
        Ok(None) | Err(StoreError::Refused(_)) => {
            tracing::debug!("previous license file is not a usable regular file; no backup written");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    // Shape only: whether the old licence was valid does not matter here.
    if parse_license(&previous).is_err() {
        tracing::debug!("previous license file is not a license document; no backup written");
        return Ok(None);
    }
    Ok(Some(previous))
}

/// The opened descriptor is the directory entry examined before opening.
#[cfg(unix)]
fn same_entry(entry: &fs::Metadata, opened: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    entry.dev() == opened.dev() && entry.ino() == opened.ino()
}

/// No stable file identity in std; the reparse-point open plus the
/// regular-file check already refuse a link.
#[cfg(not(unix))]
fn same_entry(_entry: &fs::Metadata, _opened: &fs::Metadata) -> bool {
    true
}

/// Backup filename kept after a successful overwrite (for self-serve transfer).
pub const BACKUP_LICENSE_FILENAME: &str = "license.json.bak";

/// Return the directory in which license files live.
///
/// Resolves in this order:
/// 1. `$DUDUCLAW_HOME` if set
/// 2. `<dirs::home_dir()>/.duduclaw`
///
/// # Errors
/// Returns `LicenseError::FileNotFound` if no home directory can be resolved
/// (e.g. running as a system service with no HOME).
pub fn license_dir() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("DUDUCLAW_HOME") {
        return Ok(PathBuf::from(custom));
    }
    dirs::home_dir()
        .map(|p| p.join(".duduclaw"))
        .ok_or_else(|| LicenseError::FileNotFound("no HOME directory".into()))
}

/// Return the canonical path to the license file.
pub fn default_license_path() -> Result<PathBuf> {
    Ok(license_dir()?.join(DEFAULT_LICENSE_FILENAME))
}

/// Load a license from the default path (`$DUDUCLAW_HOME/license.json` or
/// `~/.duduclaw/license.json`).
///
/// # Errors
/// - `LicenseError::FileNotFound` — no license file present (caller should
///   fall back to `LicenseTier::OpenSource`)
/// - `LicenseError::ParseError` — file exists but is not valid JSON
pub fn load_default() -> Result<License> {
    load_from(&default_license_path()?)
}

/// Load a license from an explicit path, following links to regular files
/// only, at most [`MAX_LICENSE_FILE_BYTES`].
pub fn load_from(path: &Path) -> Result<License> {
    let bytes = read_store_file(path)
        .map_err(|e| LicenseError::ParseError(format!("failed to read {}: {e}", path.display())))?
        .ok_or_else(|| LicenseError::FileNotFound(path.display().to_string()))?;
    parse_license(&bytes)
}

/// Save a license to the default path atomically.
///
/// Writes to a temp file in the same directory, then renames over the target.
/// If a previous license existed as a regular file (not a link), it is
/// preserved as `license.json.bak`.
/// On Unix, the saved file is `chmod 0600`.
pub fn save_default(license: &License) -> Result<PathBuf> {
    let path = default_license_path()?;
    save_to(license, &path)?;
    Ok(path)
}

/// File marking a revoked installation. Every writer uses the license lock.
pub const REVOKED_FILENAME: &str = "license_revoked.marker";

/// Exact persisted state observed before an operation leaves for the network.
/// Includes the marker so a concurrent revocation also invalidates a response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LicenseSnapshot {
    license_digest: Option<[u8; 32]>,
    revocation_digest: Option<[u8; 32]>,
}

/// Exclusive cross-process guard for one installed license and its marker.
/// Drop the guard before network I/O, then reacquire and compare the snapshot.
pub struct LockedLicenseStore {
    path: PathBuf,
    _lock: fs::File,
}

/// The licence file and the revocation marker, read once under the lock.
#[derive(Clone, Debug)]
pub struct StoreContents {
    license: Option<Vec<u8>>,
    marker: Option<Vec<u8>>,
}

impl StoreContents {
    /// A revocation marker is present.
    pub fn revoked(&self) -> bool {
        self.marker.is_some()
    }

    /// The installed licence, `None` when there is no licence file.
    pub fn license(&self) -> Result<Option<License>> {
        self.license.as_deref().map(parse_license).transpose()
    }

    /// SHA-256 of the licence bytes read (at most [`MAX_LICENSE_FILE_BYTES`]),
    /// `None` when there is no licence file.
    pub fn license_digest(&self) -> Option<[u8; 32]> {
        self.license.as_deref().map(|bytes| Sha256::digest(bytes).into())
    }

    pub fn snapshot(&self) -> LicenseSnapshot {
        let digest = |bytes: &Option<Vec<u8>>| -> Option<[u8; 32]> {
            bytes.as_deref().map(|b| Sha256::digest(b).into())
        };
        LicenseSnapshot { license_digest: digest(&self.license), revocation_digest: digest(&self.marker) }
    }

    /// Compare every field, including unsigned metadata, with the disk artifact.
    pub fn same_license(&self, expected: &License) -> Result<bool> {
        if self.revoked() {
            return Ok(false);
        }
        let Some(current) = self.license()? else { return Ok(false) };
        Ok(serde_json::to_vec(&current).map_err(storage_error)?
            == serde_json::to_vec(expected).map_err(storage_error)?)
    }
}

impl LockedLicenseStore {
    /// Wait as long as it takes for the lock (CLI and other synchronous callers).
    pub fn acquire(path: &Path) -> Result<Self> {
        let lock = open_lock(path)?;
        fs2::FileExt::lock_exclusive(&lock).map_err(storage_error)?;
        Ok(Self { path: path.to_path_buf(), _lock: lock })
    }

    /// Try the lock until `deadline` has passed, then give up with
    /// [`StoreError::Busy`]. Blocks the calling thread while retrying, so
    /// async callers run it on a blocking thread.
    pub fn acquire_within(path: &Path, deadline: Duration) -> std::result::Result<Self, StoreError> {
        let lock = open_lock(path)?;
        let until = Instant::now() + deadline;
        let contended = fs2::lock_contended_error().raw_os_error();
        loop {
            match fs2::FileExt::try_lock_exclusive(&lock) {
                Ok(()) => return Ok(Self { path: path.to_path_buf(), _lock: lock }),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock
                    || (contended.is_some() && error.raw_os_error() == contended) => {
                    let now = Instant::now();
                    if now >= until {
                        return Err(StoreError::Busy);
                    }
                    std::thread::sleep(LOCK_RETRY_INTERVAL.min(until - now));
                }
                Err(error) => return Err(StoreError::Io(error)),
            }
        }
    }

    pub fn load(&self) -> Result<License> {
        load_from(&self.path)
    }

    /// Read licence data following links, and the internal marker without following links.
    pub fn read_contents(&self) -> std::result::Result<StoreContents, StoreError> {
        Ok(StoreContents {
            license: read_store_file(&self.path)?,
            marker: read_store_file_no_follow(&self.marker_path())?,
        })
    }

    pub fn snapshot(&self) -> Result<LicenseSnapshot> {
        Ok(self.read_contents()?.snapshot())
    }

    pub fn matches(&self, expected: &LicenseSnapshot) -> Result<bool> {
        Ok(self.snapshot()? == *expected)
    }

    /// Compare every field, including unsigned metadata, with the disk artifact.
    pub fn same_license(&self, expected: &License) -> Result<bool> {
        self.read_contents()?.same_license(expected)
    }

    pub fn save(&self, license: &License) -> Result<()> {
        // Preserve the previous complete file. Backup failure must not lose it
        // through a partial truncate, even though backup remains best effort.
        // Only a regular `license.json` holding a licence document is backed
        // up; a link, FIFO, oversized or non-licence file is replaced (the
        // rename swaps the directory entry, it never writes through a link)
        // without a backup.
        match backup_candidate(&self.path) {
            Ok(Some(previous)) => {
                let _ = atomic_write(&self.path.with_file_name(BACKUP_LICENSE_FILENAME), &previous);
            }
            Ok(None) => {}
            Err(error) => return Err(storage_error(error)),
        }
        let json = serde_json::to_vec_pretty(license).map_err(storage_error)?;
        atomic_write(&self.path, &json)?;
        remove_if_present(&self.marker_path())?;
        sync_parent(&self.path)
    }

    pub fn delete(&self) -> Result<()> {
        remove_if_present(&self.path)?;
        sync_parent(&self.path)
    }

    /// Persist the denial before deleting the license, including on delete error.
    pub fn revoke(&self, reason: &str) -> Result<()> {
        atomic_write(&self.marker_path(), format!("revoked: {reason}\n").as_bytes())?;
        self.delete()
    }

    fn marker_path(&self) -> PathBuf {
        self.path.with_file_name(REVOKED_FILENAME)
    }
}

/// Save a license to an explicit path under the shared advisory lock.
pub fn save_to(license: &License, path: &Path) -> Result<()> {
    LockedLicenseStore::acquire(path)?.save(license)
}

/// Delete the license file (used by license deactivate).
/// Missing files are successful; a revocation marker is intentionally retained.
pub fn delete_default() -> Result<()> {
    LockedLicenseStore::acquire(&default_license_path()?)?.delete()
}

fn storage_error(error: impl std::fmt::Display) -> LicenseError {
    LicenseError::ParseError(format!("license storage: {error}"))
}

fn parent_dir(path: &Path) -> Result<&Path> {
    path.parent().filter(|dir| !dir.as_os_str().is_empty()).ok_or_else(|| {
        LicenseError::ParseError(format!("license path has no parent: {}", path.display()))
    })
}

/// Create (if needed) and open `<license>.lock` — no-follow and regular
/// files only: the lock is internal, unlike externally managed licence data.
fn open_lock(path: &Path) -> std::result::Result<fs::File, StoreError> {
    let dir = parent_dir(path).map_err(|_| StoreError::Refused("license path has no parent"))?;
    fs::create_dir_all(dir).map_err(StoreError::Io)?;
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    open_no_follow(&mut options, &PathBuf::from(lock_path))
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(error)),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    // NamedTempFile uses create_new, random names and owner-only Unix mode.
    // Exclusive creation refuses existing links; only licence reads follow links.
    // Its drop removes only this writer's temporary file on any failure.
    let mut temporary = tempfile::NamedTempFile::new_in(parent_dir(path)?).map_err(storage_error)?;
    temporary.write_all(bytes).map_err(storage_error)?;
    temporary.as_file().sync_all().map_err(storage_error)?;
    persist_temporary(temporary, path)?;
    sync_parent(path)
}

fn persist_temporary(temporary: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    #[cfg(not(windows))]
    temporary.persist(path).map_err(storage_error)?;
    #[cfg(windows)]
    {
        // MoveFileExW can fail while a lock-free reader or scanner holds the
        // destination. Keep the same flushed file and the caller's advisory
        // lock; never delete/truncate the destination to force replacement.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut temporary = temporary;
        loop {
            match temporary.persist(path) {
                Ok(_) => break,
                Err(error) => {
                    // ACCESS_DENIED, SHARING_VIOLATION, LOCK_VIOLATION. A real
                    // ACL denial still fails closed when the deadline expires.
                    if !matches!(error.error.raw_os_error(), Some(5 | 32 | 33))
                        || std::time::Instant::now() >= deadline
                    {
                        return Err(storage_error(error));
                    }
                    temporary = error.file;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(parent_dir(path)?).and_then(|dir| dir.sync_all()).map_err(storage_error)?;
    // Windows does not expose directory fsync through std. The file is flushed
    // before tempfile's platform-specific atomic replacement.
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier::LicenseTier;
    use chrono::Duration;
    use tempfile::TempDir;

    fn fixture_license() -> License {
        License::new(
            "sub_storage_test",
            "cus_storage_test",
            LicenseTier::Studio,
            "fp_storage",
            Duration::days(30),
            "v1",
        )
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let license = fixture_license();

        save_to(&license, &path).unwrap();
        let loaded = load_from(&path).unwrap();

        assert_eq!(loaded.subscription_id, license.subscription_id);
        assert_eq!(loaded.customer_id, license.customer_id);
        assert_eq!(loaded.tier, license.tier);
        assert_eq!(loaded.machine_fingerprint, license.machine_fingerprint);
    }

    #[test]
    fn save_overwrites_existing_and_creates_backup() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");

        let v1 = fixture_license();
        save_to(&v1, &path).unwrap();

        let mut v2 = v1.clone();
        v2.subscription_id = "sub_v2".into();
        save_to(&v2, &path).unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.subscription_id, "sub_v2");

        let backup = path.with_file_name(BACKUP_LICENSE_FILENAME);
        assert!(backup.exists(), "backup should be created on overwrite");
        let backed_up = load_from(&backup).unwrap();
        assert_eq!(backed_up.subscription_id, "sub_storage_test");
    }

    #[test]
    fn delayed_response_snapshot_rejects_replacement_and_metadata_changes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let initial = fixture_license();
        save_to(&initial, &path).unwrap();
        let request_snapshot = LockedLicenseStore::acquire(&path).unwrap().snapshot().unwrap();

        let mut replacement = initial.clone();
        replacement.control_url = Some("https://replacement.example".into());
        save_to(&replacement, &path).unwrap();
        let locked = LockedLicenseStore::acquire(&path).unwrap();
        assert!(!locked.matches(&request_snapshot).unwrap());
        assert!(!locked.same_license(&initial).unwrap());
        assert!(locked.same_license(&replacement).unwrap());
        assert_eq!(locked.load().unwrap().control_url, replacement.control_url);
    }

    #[test]
    fn revocation_invalidates_snapshot_and_install_clears_marker_under_lock() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let initial = fixture_license();
        let locked = LockedLicenseStore::acquire(&path).unwrap();
        locked.save(&initial).unwrap();
        let request_snapshot = locked.snapshot().unwrap();
        locked.revoke("cancelled").unwrap();
        assert!(!locked.matches(&request_snapshot).unwrap());
        assert!(!locked.same_license(&initial).unwrap());
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(tmp.path().join(REVOKED_FILENAME)).unwrap(), "revoked: cancelled\n");

        locked.save(&initial).unwrap();
        assert!(locked.same_license(&initial).unwrap());
        assert!(!tmp.path().join(REVOKED_FILENAME).exists());
        locked.delete().unwrap();
        locked.delete().unwrap();
    }

    #[test]
    fn unique_temporary_file_does_not_touch_other_writers_and_cleans_failed_write() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let legacy_temporary = path.with_extension("json.tmp");
        fs::write(&legacy_temporary, b"another writer").unwrap();
        save_to(&fixture_license(), &path).unwrap();
        assert_eq!(fs::read(&legacy_temporary).unwrap(), b"another writer");

        let occupied = tmp.path().join("occupied");
        fs::create_dir(&occupied).unwrap();
        let count = fs::read_dir(tmp.path()).unwrap().count();
        assert!(atomic_write(&occupied, b"cannot replace directory").is_err());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), count);
    }

    #[cfg(windows)]
    #[test]
    fn replacement_retries_until_a_windows_reader_releases_the_destination() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        save_to(&fixture_license(), &path).unwrap();
        // Deliberately omit FILE_SHARE_DELETE, as an external reader may do.
        let reader = fs::OpenOptions::new().read(true).share_mode(0x1 | 0x2).open(&path).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(reader);
        });
        let mut replacement = fixture_license();
        replacement.subscription_id = "replacement".into();
        save_to(&replacement, &path).unwrap();
        release.join().unwrap();
        assert_eq!(load_from(&path).unwrap().subscription_id, "replacement");
        assert_eq!(load_from(&path.with_file_name(BACKUP_LICENSE_FILENAME)).unwrap().subscription_id, "sub_storage_test");
    }

    #[cfg(windows)]
    #[test]
    fn persistent_windows_sharing_denial_preserves_the_license_and_cleans_tempfiles() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        save_to(&fixture_license(), &path).unwrap();
        let reader = fs::OpenOptions::new().read(true).share_mode(0x1 | 0x2).open(&path).unwrap();
        let before = fs::read(&path).unwrap();
        let count = fs::read_dir(tmp.path()).unwrap().count();
        assert!(atomic_write(&path, b"replacement").is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), count);
        drop(reader);
    }

    #[test]
    fn snapshot_includes_marker_and_fails_closed_on_unreadable_state() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let locked = LockedLicenseStore::acquire(&path).unwrap();
        locked.save(&fixture_license()).unwrap();
        let snapshot = locked.snapshot().unwrap();
        fs::write(tmp.path().join(REVOKED_FILENAME), b"denied").unwrap();
        assert!(!locked.matches(&snapshot).unwrap());
        fs::remove_file(tmp.path().join(REVOKED_FILENAME)).unwrap();
        fs::create_dir(tmp.path().join(REVOKED_FILENAME)).unwrap();
        assert!(locked.matches(&snapshot).is_err());
    }

    #[test]
    fn storage_subprocess_writer() {
        let Some(path) = std::env::var_os("DDC_STORAGE_TEST_CHILD_PATH") else { return };
        let identity = std::env::var("DDC_STORAGE_TEST_CHILD_ID").unwrap();
        let path = PathBuf::from(path);
        fs::write(path.with_extension(format!("{identity}.ready")), b"ready").unwrap();
        let mut license = fixture_license();
        license.subscription_id = identity;
        for _ in 0..12 {
            save_to(&license, &path).unwrap();
        }
    }

    #[test]
    fn advisory_lock_serializes_processes_and_preserves_complete_backups() {
        use std::process::{Command, Stdio};
        use std::time::{Duration as StdDuration, Instant};
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let locked = LockedLicenseStore::acquire(&path).unwrap();
        locked.save(&fixture_license()).unwrap();

        let spawn = |identity: &str| {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "storage::tests::storage_subprocess_writer", "--nocapture"])
                .env("DDC_STORAGE_TEST_CHILD_PATH", &path)
                .env("DDC_STORAGE_TEST_CHILD_ID", identity)
                .stdout(Stdio::null()).spawn().unwrap()
        };
        let mut first = spawn("first");
        let mut second = spawn("second");
        let deadline = Instant::now() + StdDuration::from_secs(10);
        while !(path.with_extension("first.ready").exists() && path.with_extension("second.ready").exists()) {
            if Instant::now() >= deadline {
                let _ = first.kill();
                let _ = second.kill();
                panic!("child processes did not start");
            }
            std::thread::sleep(StdDuration::from_millis(10));
        }
        assert!(first.try_wait().unwrap().is_none(), "writer bypassed the held lock");
        assert!(second.try_wait().unwrap().is_none(), "writer bypassed the held lock");
        assert_eq!(locked.load().unwrap().subscription_id, "sub_storage_test");
        drop(locked);

        // Each child performs several fsync-backed atomic replacements. On a
        // busy Linux builder, storage contention can make this exceed 10s.
        let deadline = Instant::now() + StdDuration::from_secs(60);
        loop {
            // Every lock-free reader sees a full old/new JSON document.
            load_from(&path).unwrap();
            let one = first.try_wait().unwrap();
            let two = second.try_wait().unwrap();
            if let (Some(one), Some(two)) = (one, two) {
                assert!(one.success());
                assert!(two.success());
                break;
            }
            if Instant::now() >= deadline {
                let _ = first.kill();
                let _ = second.kill();
                panic!("child writers did not finish");
            }
            std::thread::sleep(StdDuration::from_millis(1));
        }
        let installed = load_from(&path).unwrap();
        assert!(matches!(installed.subscription_id.as_str(), "first" | "second"));
        let backup = load_from(&path.with_file_name(BACKUP_LICENSE_FILENAME)).unwrap();
        assert!(matches!(backup.subscription_id.as_str(), "first" | "second"));
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 5, "temporary files leaked");
    }

    #[test]
    fn load_returns_file_not_found_for_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.json");
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, LicenseError::FileNotFound(_)));
    }

    #[test]
    fn load_returns_parse_error_for_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bad.json");
        fs::write(&path, b"{ this is not valid json").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, LicenseError::ParseError(_)));
    }

    #[test]
    fn delete_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let license = fixture_license();
        save_to(&license, &path).unwrap();
        assert!(path.exists());

        std::fs::remove_file(&path).unwrap();
        // Calling again on missing file must not error
        assert!(!path.exists());
    }

    #[test]
    // SAFETY justification for the allow: Rust 2024 makes `env::set_var` /
    // `remove_var` unsafe (process-global, not thread-safe); this test-only
    // mutation is confined to DUDUCLAW_HOME with save/restore, and no safe
    // std alternative exists for env mutation.
    #[allow(unsafe_code)]
    fn license_dir_respects_env_override() {
        let tmp = TempDir::new().unwrap();
        let custom = tmp.path().to_path_buf();

        // SAFETY: tests run single-threaded under cargo test by default for
        // a single crate; we restore the env var afterwards.
        let original = std::env::var("DUDUCLAW_HOME").ok();
        // SAFETY: set_var is unsafe in Rust 2024 edition.
        unsafe {
            std::env::set_var("DUDUCLAW_HOME", &custom);
        }
        let resolved = license_dir().unwrap();
        assert_eq!(resolved, custom);

        // restore
        unsafe {
            match original {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
    }

    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn make_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a valid NUL-terminated path for the whole call.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    }

    /// A FIFO at `license.json` (nobody writing to it) must not block a
    /// reader: refused promptly by every read path, including the raw
    /// non-blocking open and descriptor metadata check.
    #[cfg(unix)]
    #[test]
    fn fifo_at_the_license_path_is_refused_without_blocking() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        make_fifo(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let reader_path = path.clone();
        std::thread::spawn(move || {
            let read = matches!(read_store_file(&reader_path), Err(StoreError::Refused(_)));
            let opened = matches!(open_no_follow(fs::OpenOptions::new().read(true), &reader_path),
                Err(StoreError::Refused("not a regular file")));
            let loaded = load_from(&reader_path).is_err();
            let store = LockedLicenseStore::acquire(&reader_path).unwrap();
            let contents = store.read_contents().is_err() && store.snapshot().is_err();
            let _ = tx.send(read && opened && loaded && contents);
        });
        let refused = rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("reading a FIFO licence path must return promptly");
        assert!(refused);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_regular_license_is_read_and_save_replaces_link() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("elsewhere.json");
        save_to(&fixture_license(), &real).unwrap();
        let path = tmp.path().join("license.json");
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert_eq!(load_from(&path).unwrap().subscription_id, fixture_license().subscription_id);
        assert!(LockedLicenseStore::acquire(&path).unwrap().read_contents().unwrap()
            .same_license(&fixture_license()).unwrap());
        let backup = tmp.path().join(BACKUP_LICENSE_FILENAME);
        std::os::unix::fs::symlink(&real, &backup).unwrap();
        assert!(read_store_file(&backup).unwrap().is_some());
        // Replacing the link installs a real file and never writes through it.
        let before = fs::read(&real).unwrap();
        let mut replacement = fixture_license();
        replacement.subscription_id = "replacement".into();
        save_to(&replacement, &path).unwrap();
        assert!(!fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(load_from(&path).unwrap().subscription_id, "replacement");
        assert_eq!(fs::read(&real).unwrap(), before);
    }

    /// M1: a `license.json` link pointing at a file the gateway can read is
    /// never copied into the DuDuClaw home as the backup.
    #[cfg(unix)]
    #[test]
    fn symlinked_license_to_a_sensitive_file_writes_no_backup() {
        let tmp = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let sensitive = elsewhere.path().join("sensitive.txt");
        fs::write(&sensitive, b"not for the link owner\n").unwrap();
        let path = tmp.path().join("license.json");
        std::os::unix::fs::symlink(&sensitive, &path).unwrap();
        save_to(&fixture_license(), &path).unwrap();
        assert!(!tmp.path().join(BACKUP_LICENSE_FILENAME).exists());
        assert_eq!(fs::read(&sensitive).unwrap(), b"not for the link owner\n");
        assert!(!fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(load_from(&path).unwrap().subscription_id, fixture_license().subscription_id);

        // A link to a real licence is not backed up either.
        let real = elsewhere.path().join("license.json");
        save_to(&fixture_license(), &real).unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        save_to(&fixture_license(), &path).unwrap();
        assert!(!tmp.path().join(BACKUP_LICENSE_FILENAME).exists());
    }

    #[test]
    fn regular_license_backup_is_the_previous_bytes_and_garbage_is_not_backed_up() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let backup = tmp.path().join(BACKUP_LICENSE_FILENAME);
        save_to(&fixture_license(), &path).unwrap();
        let previous = fs::read(&path).unwrap();
        let mut replacement = fixture_license();
        replacement.subscription_id = "replacement".into();
        save_to(&replacement, &path).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), previous);

        let garbage = TempDir::new().unwrap();
        let path = garbage.path().join("license.json");
        fs::write(&path, b"{\"not\": \"a licence\"}").unwrap();
        save_to(&replacement, &path).unwrap();
        assert!(!garbage.path().join(BACKUP_LICENSE_FILENAME).exists());
        assert_eq!(load_from(&path).unwrap().subscription_id, "replacement");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lock_is_refused() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("elsewhere.json.lock"), "").unwrap();
        let other = tmp.path().join("other.json");
        std::os::unix::fs::symlink(tmp.path().join("elsewhere.json.lock"), tmp.path().join("other.json.lock")).unwrap();
        assert!(LockedLicenseStore::acquire(&other).is_err());
        assert!(matches!(LockedLicenseStore::acquire_within(&other, std::time::Duration::from_millis(50)),
            Err(StoreError::Refused("symbolic link"))));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_fifo_is_refused_without_blocking() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("fifo");
        make_fifo(&target);
        let path = tmp.path().join("license.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(matches!(read_store_file(&path), Err(StoreError::Refused("not a regular file")))).unwrap();
        });
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).expect("linked FIFO must not block"));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_license_symlink_is_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        std::os::unix::fs::symlink(tmp.path().join("missing"), &path).unwrap();
        assert!(read_store_file(&path).unwrap().is_none());
        assert!(matches!(load_from(&path), Err(LicenseError::FileNotFound(_))));
        assert!(LockedLicenseStore::acquire(&path).unwrap().read_contents().unwrap().license().unwrap().is_none());
    }

    #[test]
    fn oversized_license_is_refused_and_one_at_the_cap_is_read() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        fs::write(&path, vec![b' '; MAX_LICENSE_FILE_BYTES as usize + 1]).unwrap();
        assert!(matches!(read_store_file(&path), Err(StoreError::Refused("too large"))));
        assert!(load_from(&path).is_err());
        fs::write(&path, vec![b' '; MAX_LICENSE_FILE_BYTES as usize]).unwrap();
        assert_eq!(read_store_file(&path).unwrap().unwrap().len(), MAX_LICENSE_FILE_BYTES as usize);
        fs::create_dir(tmp.path().join("dir.json")).unwrap();
        assert!(matches!(read_store_file(&tmp.path().join("dir.json")), Err(StoreError::Refused(_))));
        assert!(read_store_file(&tmp.path().join("absent.json")).unwrap().is_none());
    }

    #[test]
    fn held_lock_yields_busy_within_the_deadline() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let license = fixture_license();
        let held = LockedLicenseStore::acquire(&path).unwrap();
        held.save(&license).unwrap();
        let deadline = std::time::Duration::from_millis(200);
        let started = Instant::now();
        assert!(matches!(LockedLicenseStore::acquire_within(&path, deadline), Err(StoreError::Busy)));
        let waited = started.elapsed();
        assert!(waited >= deadline, "{waited:?}");
        assert!(waited < std::time::Duration::from_secs(5), "{waited:?}");
        drop(held);
        let store = LockedLicenseStore::acquire_within(&path, deadline).unwrap();
        let contents = store.read_contents().unwrap();
        assert!(!contents.revoked());
        assert!(contents.same_license(&license).unwrap());
        assert_eq!(contents.snapshot(), store.snapshot().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("license.json");
        let license = fixture_license();
        save_to(&license, &path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "license file must be owner-only (0o600)");
    }
}
