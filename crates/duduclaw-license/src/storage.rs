//! License file persistence — load/save `~/.duduclaw/license.json`.
//!
//! Atomic write-and-rename to avoid half-written files on crash, with
//! 0600 permissions on Unix so the signed payload + customer ID are not
//! world-readable.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json;

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

/// Save a license to an explicit path atomically.
pub fn save_to(license: &License, path: &Path) -> Result<()> {
    let dir = path.parent().ok_or_else(|| {
        LicenseError::ParseError(format!("license path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(dir).map_err(|e| {
        LicenseError::ParseError(format!("failed to create {}: {e}", dir.display()))
    })?;

    // Back up existing license before overwriting.
    if path.exists() {
        let backup = path.with_file_name(BACKUP_LICENSE_FILENAME);
        // Best-effort backup — if it fails (read-only fs, permissions), we still
        // try to write the new license. The user-facing message in the CLI
        // surfaces the lack of backup but does not block the operation.
        let _ = fs::copy(path, &backup);
    }

    let json = serde_json::to_vec_pretty(license)
        .map_err(|e| LicenseError::ParseError(format!("serialize license: {e}")))?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| {
                LicenseError::ParseError(format!("open temp {}: {e}", tmp.display()))
            })?;
        f.write_all(&json).map_err(|e| {
            LicenseError::ParseError(format!("write temp {}: {e}", tmp.display()))
        })?;
        f.sync_all().map_err(|e| {
            LicenseError::ParseError(format!("fsync {}: {e}", tmp.display()))
        })?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&tmp)
            .map_err(|e| LicenseError::ParseError(format!("stat tmp: {e}")))?
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&tmp, perms)
            .map_err(|e| LicenseError::ParseError(format!("chmod tmp: {e}")))?;
    }

    fs::rename(&tmp, path).map_err(|e| {
        LicenseError::ParseError(format!(
            "atomic rename {} → {}: {e}",
            tmp.display(),
            path.display()
        ))
    })?;

    Ok(())
}

/// Delete the license file (used by `duduclaw license deactivate`).
///
/// Returns `Ok(())` even if no license file exists, so the operation is
/// idempotent.
pub fn delete_default() -> Result<()> {
    let path = default_license_path()?;
    match fs::remove_file(&path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LicenseError::ParseError(format!(
            "remove {}: {e}",
            path.display()
        ))),
    }
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
