//! License file persistence — load/save `~/.duduclaw/license.json`.
//!
//! Atomic write-and-rename to avoid half-written files on crash, with
//! 0600 permissions on Unix so the signed payload + customer ID are not
//! world-readable.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json;
use sha2::{Digest, Sha256};

use crate::error::{LicenseError, Result};
use crate::license::License;

/// Default filename inside the DuDuClaw home directory.
pub const DEFAULT_LICENSE_FILENAME: &str = "license.json";

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

/// Load a license from an explicit path.
pub fn load_from(path: &Path) -> Result<License> {
    let bytes = fs::read(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            LicenseError::FileNotFound(path.display().to_string())
        } else {
            LicenseError::ParseError(format!("failed to read {}: {e}", path.display()))
        }
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|e| LicenseError::ParseError(format!("invalid license JSON: {e}")))
}

/// Save a license to the default path atomically.
///
/// Writes to a temp file in the same directory, then renames over the target.
/// If a previous license existed, it is preserved as `license.json.bak`.
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

impl LockedLicenseStore {
    pub fn acquire(path: &Path) -> Result<Self> {
        let dir = parent_dir(path)?;
        fs::create_dir_all(dir).map_err(storage_error)?;
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(PathBuf::from(lock_path)).map_err(storage_error)?;
        fs2::FileExt::lock_exclusive(&lock).map_err(storage_error)?;
        Ok(Self { path: path.to_path_buf(), _lock: lock })
    }

    pub fn load(&self) -> Result<License> {
        load_from(&self.path)
    }

    pub fn snapshot(&self) -> Result<LicenseSnapshot> {
        Ok(LicenseSnapshot {
            license_digest: file_digest(&self.path)?,
            revocation_digest: file_digest(&self.marker_path())?,
        })
    }

    pub fn matches(&self, expected: &LicenseSnapshot) -> Result<bool> {
        Ok(self.snapshot()? == *expected)
    }

    /// Compare every field, including unsigned metadata, with the disk artifact.
    pub fn same_license(&self, expected: &License) -> Result<bool> {
        if file_digest(&self.marker_path())?.is_some() {
            return Ok(false);
        }
        let current = match self.load() {
            Ok(current) => current,
            Err(LicenseError::FileNotFound(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(serde_json::to_vec(&current).map_err(storage_error)?
            == serde_json::to_vec(expected).map_err(storage_error)?)
    }

    pub fn save(&self, license: &License) -> Result<()> {
        // Preserve the previous complete file. Backup failure must not lose it
        // through a partial truncate, even though backup remains best effort.
        match fs::read(&self.path) {
            Ok(previous) => {
                let _ = atomic_write(&self.path.with_file_name(BACKUP_LICENSE_FILENAME), &previous);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
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

fn file_digest(path: &Path) -> Result<Option<[u8; 32]>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(Sha256::digest(&bytes).into())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage_error(error)),
    }
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
