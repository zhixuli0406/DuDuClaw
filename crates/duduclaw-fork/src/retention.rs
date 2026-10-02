//! Retained branch workspaces (`<home>/fork_ws/<fork_id>/<branch_id>/`).
//!
//! A fork that finishes without a final resolution (operator confirmation
//! pending, or `Manual` mode) keeps each surviving branch workspace here so a
//! later `merge_or_select` can actually promote files. A fork resolved at
//! execution time retains nothing.
//!
//! Every id is validated before it is joined into a path, and callers check
//! containment with `canonicalize` before trusting a stored path.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::{ForkError, Result};

/// Directory name under the DuDuClaw home that holds retained workspaces.
pub const RETAINED_DIR_NAME: &str = "fork_ws";

/// `<home>/fork_ws`.
pub fn retained_root(home_dir: &Path) -> PathBuf {
    home_dir.join(RETAINED_DIR_NAME)
}

/// Validate a fork / branch id for use as a single path component: non-empty,
/// at most 128 bytes, ASCII alphanumerics plus `-` and `_` only. That rules out
/// separators, `..`, drive prefixes and NUL (fail closed on anything else).
pub fn validate_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(ForkError::Config(format!("invalid fork/branch id: {id:?}")))
    }
}

/// `<root>/<fork_id>` after validating the id.
pub fn fork_dir(root: &Path, fork_id: &str) -> Result<PathBuf> {
    validate_id(fork_id)?;
    Ok(root.join(fork_id))
}

/// `<root>/<fork_id>/<branch_id>` after validating both ids.
pub fn branch_dir(root: &Path, fork_id: &str, branch_id: &str) -> Result<PathBuf> {
    validate_id(branch_id)?;
    Ok(fork_dir(root, fork_id)?.join(branch_id))
}

/// Create `path` (and missing ancestors) with owner-only permissions on Unix.
pub fn ensure_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|e| ForkError::Overlay(format!("create {}: {e}", path.display())))?;
        set_private(path)
    }
    #[cfg(windows)]
    { crate::publication::make_private_directory(path) }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::create_dir_all(path)
            .map_err(|e| ForkError::Overlay(format!("create {}: {e}", path.display())))
    }
}

/// Create a private recovery directory without following or chmodding an
/// existing unsafe entry. All recovery ancestors are checked by the caller.
pub fn ensure_checked_private_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(ForkError::Overlay("recovery path is not a private directory".into()));
            }
            #[cfg(unix)] {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o7777 != 0o700 {
                    return Err(ForkError::Overlay("recovery directory owner/mode is unsafe".into()));
                }
            }
            #[cfg(windows)] crate::publication::check_private_directory(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_private_dir(path)?;
            ensure_checked_private_dir(path)
        }
        Err(error) => Err(ForkError::Overlay(error.to_string())),
    }
}

/// Tighten an existing directory to 0700 (no-op off Unix).
pub fn set_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| ForkError::Overlay(format!("chmod {}: {e}", path.display())))?;
    }
    #[cfg(windows)]
    crate::publication::make_private_directory(path)?;
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
    }
    Ok(())
}

/// Whether `candidate` exists and canonically lies inside `container`.
pub fn is_contained(container: &Path, candidate: &Path) -> bool {
    match (container.canonicalize(), candidate.canonicalize()) {
        (Ok(c), Ok(p)) => p.starts_with(&c) && p != c,
        _ => false,
    }
}

/// Remove `<root>/<fork_id>/` entirely. Missing ⇒ `Ok(false)`.
pub fn remove_fork(root: &Path, fork_id: &str) -> Result<bool> {
    let dir = fork_dir(root, fork_id)?;
    remove_dir_entry(&dir)
}

/// Remove `<root>/<fork_id>/<branch_id>/`, then the fork dir too if it is left
/// empty. Missing ⇒ `Ok(false)`.
pub fn remove_branch(root: &Path, fork_id: &str, branch_id: &str) -> Result<bool> {
    let dir = branch_dir(root, fork_id, branch_id)?;
    let removed = remove_dir_entry(&dir)?;
    let parent = fork_dir(root, fork_id)?;
    if let Ok(mut it) = std::fs::read_dir(&parent) {
        if it.next().is_none() {
            let _ = std::fs::remove_dir(&parent);
        }
    }
    Ok(removed)
}

/// Remove a directory without following a symlink at `path` (a symlink is
/// unlinked, never recursed into).
fn remove_dir_entry(path: &Path) -> Result<bool> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ForkError::Overlay(format!("lstat {}: {e}", path.display()))),
    };
    let res = if meta.file_type().is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    res.map_err(|e| ForkError::Overlay(format!("remove {}: {e}", path.display())))?;
    Ok(true)
}

/// Delete retained fork dirs under `root` whose mtime is older than `ttl`.
/// Returns how many were removed. Best-effort: per-entry errors are logged.
pub fn sweep_expired(root: &Path, ttl: Duration) -> usize {
    sweep_expired_at(root, ttl, SystemTime::now())
}

/// [`sweep_expired`] with an explicit clock (testable).
pub fn sweep_expired_at(root: &Path, ttl: Duration, now: SystemTime) -> usize {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let valid = name.to_str().map(|n| validate_id(n).is_ok()).unwrap_or(false);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Anything that is not a retained fork dir (a stray symlink or file)
        // does not belong here; unlink it without following.
        let expired = if !valid || !meta.file_type().is_dir() {
            true
        } else {
            match meta.modified() {
                Ok(m) => now.duration_since(m).map(|age| age > ttl).unwrap_or(false),
                Err(_) => false,
            }
        };
        if !expired {
            continue;
        }
        match remove_dir_entry(&path) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!("fork_ws sweep: {e}"),
        }
    }
    if removed > 0 {
        tracing::info!("fork_ws sweep removed {removed} expired retained workspace(s)");
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_id_rejects_traversal() {
        assert!(validate_id("fork-0a1b_2").is_ok());
        for bad in ["", "../x", "..", "a/b", "a\\b", "x\0y", "/abs", ".", "a.b"] {
            assert!(validate_id(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(branch_dir(Path::new("/r"), "f1", "../x").is_err());
        assert!(fork_dir(Path::new("/r"), "../f").is_err());
    }

    #[test]
    fn sweep_removes_expired_and_keeps_fresh() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("fork-a/b1")).unwrap();
        // Fresh at "now": kept.
        assert_eq!(sweep_expired(root.path(), Duration::from_secs(3600)), 0);
        assert!(root.path().join("fork-a").exists());
        // Two days later with a 24h TTL: removed.
        let later = SystemTime::now() + Duration::from_secs(48 * 3600);
        assert_eq!(sweep_expired_at(root.path(), Duration::from_secs(24 * 3600), later), 1);
        assert!(!root.path().join("fork-a").exists());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_old_dir_removed_fresh_dir_kept() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("fork-old");
        let fresh = root.path().join("fork-fresh");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        let past = SystemTime::now() - Duration::from_secs(48 * 3600);
        std::fs::File::open(&old).unwrap().set_modified(past).unwrap();

        assert_eq!(sweep_expired(root.path(), Duration::from_secs(24 * 3600)), 1);
        assert!(!old.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn remove_branch_cleans_empty_fork_dir() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("f1/b1")).unwrap();
        assert!(remove_branch(root.path(), "f1", "b1").unwrap());
        assert!(!root.path().join("f1").exists());
        assert!(!remove_fork(root.path(), "f1").unwrap());
    }
}
