//! `BranchOverlay` — a copy-on-write workspace for one branch.
//!
//! RFC-26 §3.1: reads fall through to the parent workspace, writes stay local to
//! the branch. The winning branch's writes are merged back into the parent on
//! `promote()`; losing overlays are discarded on `Drop`.
//!
//! Backends (RFC-26 §4.3 / §6 Q1): a portable directory snapshot (works
//! everywhere) and a **native copy-on-write** clone — `clonefile(2)` via `cp -c`
//! on macOS/APFS, `cp --reflink` on Linux btrfs/XFS. CoW is auto-detected by a
//! one-time probe and falls back to the snapshot copy if unavailable, so a wrong
//! guess never breaks isolation — it only forgoes a speed/space optimization.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::error::{ForkError, Result};

/// Available copy-on-write workspace backends (RFC-26 §4.3 / §6 Q1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayBackend {
    /// Portable directory snapshot (recursive byte copy). Works everywhere.
    Snapshot,
    /// Native copy-on-write clone (`clonefile` / reflink). Instant + space-efficient.
    NativeCow,
}

/// Detect (once, cached) the preferred overlay backend by probing whether a native
/// CoW clone actually works on the host's temp filesystem. Fail-safe → `Snapshot`.
pub fn detect_backend() -> OverlayBackend {
    static CACHED: OnceLock<OverlayBackend> = OnceLock::new();
    *CACHED.get_or_init(probe_native_cow)
}

/// Probe: create a tiny tree in a temp dir and try to CoW-clone it. Any failure
/// (unsupported FS, missing flag, non-Unix) ⇒ `Snapshot`.
fn probe_native_cow() -> OverlayBackend {
    if !cfg!(unix) {
        return OverlayBackend::Snapshot;
    }
    let probe = || -> std::io::Result<bool> {
        let dir = tempfile::tempdir()?;
        let src = dir.path().join("src");
        std::fs::create_dir(&src)?;
        std::fs::write(src.join("f.txt"), b"probe")?;
        let dst = dir.path().join("dst"); // must not exist
        let ok = clone_tree_native(&src, &dst).is_ok() && dst.join("f.txt").is_file();
        Ok(ok)
    };
    match probe() {
        Ok(true) => {
            tracing::debug!("fork overlay: native CoW available");
            OverlayBackend::NativeCow
        }
        _ => OverlayBackend::Snapshot,
    }
}

/// CoW-clone the directory tree `src` → `dst` (which must NOT exist) using the
/// platform's reflink mechanism. Returns `Err` when unavailable so callers fall
/// back to a snapshot copy.
fn clone_tree_native(src: &Path, dst: &Path) -> Result<()> {
    let mut cmd = Command::new("cp");
    #[cfg(target_os = "macos")]
    {
        // -c = clonefile(2) (APFS copy-on-write), -R = recursive.
        cmd.arg("-cR");
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux/btrfs/XFS reflink. `=always` fails (non-zero) when unsupported,
        // so the probe correctly degrades to Snapshot.
        cmd.arg("--reflink=always").arg("-R");
    }
    let out = cmd
        .arg(src)
        .arg(dst)
        .output()
        .map_err(|e| ForkError::Overlay(format!("spawn cp for CoW clone: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(ForkError::Overlay(format!(
            "CoW clone unavailable: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// An isolated, writable copy of a parent workspace.
#[derive(Debug)]
pub struct BranchOverlay {
    parent: PathBuf,
    /// Owned temp root; removed on drop. The branch workspace is `root/ws`.
    _root: tempfile::TempDir,
    work: PathBuf,
    backend: OverlayBackend,
}

impl BranchOverlay {
    /// Create an overlay over `parent`, materializing a private writable copy via
    /// the detected backend (native CoW when available, else snapshot).
    ///
    /// Fail-closed: a non-existent or non-directory parent is an error rather
    /// than an empty silent workspace.
    pub fn create(parent: impl AsRef<Path>) -> Result<Self> {
        Self::create_with(parent, detect_backend())
    }

    /// Create with an explicit backend (used by tests to exercise both paths).
    pub fn create_with(parent: impl AsRef<Path>, backend: OverlayBackend) -> Result<Self> {
        let parent = parent.as_ref().to_path_buf();
        if !parent.is_dir() {
            return Err(ForkError::Overlay(format!(
                "parent workspace is not a directory: {}",
                parent.display()
            )));
        }
        let root = tempfile::Builder::new()
            .prefix("duduclaw_fork_")
            .tempdir()
            .map_err(|e| ForkError::Overlay(format!("create overlay tempdir: {e}")))?;
        let work = root.path().join("ws"); // must not exist for native clone

        let effective = match backend {
            OverlayBackend::NativeCow if clone_tree_native(&parent, &work).is_ok() => {
                OverlayBackend::NativeCow
            }
            _ => {
                // Snapshot (also the fallback when a native clone fails mid-create).
                copy_tree(&parent, &work)?;
                OverlayBackend::Snapshot
            }
        };
        Ok(BranchOverlay { parent, _root: root, work, backend: effective })
    }

    /// The branch's private writable root. The agent subprocess runs against this.
    pub fn workspace(&self) -> &Path {
        &self.work
    }

    /// The shared read-only parent (for diffing).
    pub fn parent(&self) -> &Path {
        &self.parent
    }

    /// The backend that actually materialized this overlay.
    pub fn backend(&self) -> OverlayBackend {
        self.backend
    }

    /// Merge this branch's writes back into the parent workspace (winner only).
    ///
    /// Overwrites parent files that the branch changed and adds new ones. Files the
    /// branch deleted are *not* propagated (additive merge).
    pub fn promote(&self) -> Result<()> {
        copy_tree(&self.work, &self.parent)
    }
}

/// Recursively copy `src` into `dst`, creating `dst` subdirectories as needed.
/// Symlinks are copied as their target contents to keep branch writes contained
/// within the overlay (no escape via symlink into the parent during a run).
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .map_err(|e| ForkError::Overlay(format!("create {}: {e}", dst.display())))?;

    for entry in std::fs::read_dir(src)
        .map_err(|e| ForkError::Overlay(format!("read_dir {}: {e}", src.display())))?
    {
        let entry =
            entry.map_err(|e| ForkError::Overlay(format!("dir entry in {}: {e}", src.display())))?;
        let file_type = entry
            .file_type()
            .map_err(|e| ForkError::Overlay(format!("file_type: {e}")))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            // Regular file or symlink-to-file: copy contents.
            std::fs::copy(&from, &to)
                .map_err(|e| ForkError::Overlay(format!("copy {} -> {}: {e}", from.display(), to.display())))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detect_backend_is_deterministic_and_safe() {
        // Whatever the host supports, detection is stable and yields one of the
        // two valid backends (never panics / never an undefined state).
        let b = detect_backend();
        assert!(b == OverlayBackend::Snapshot || b == OverlayBackend::NativeCow);
        assert_eq!(detect_backend(), b); // cached/stable
    }

    #[test]
    fn snapshot_backend_isolates_writes() {
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join("a.txt"), "orig").unwrap();
        let overlay = BranchOverlay::create_with(parent.path(), OverlayBackend::Snapshot).unwrap();
        assert_eq!(overlay.backend(), OverlayBackend::Snapshot);
        assert_eq!(fs::read_to_string(overlay.workspace().join("a.txt")).unwrap(), "orig");
        fs::write(overlay.workspace().join("a.txt"), "changed").unwrap();
        assert_eq!(fs::read_to_string(parent.path().join("a.txt")).unwrap(), "orig");
    }

    #[test]
    fn native_cow_isolates_writes_when_available() {
        // Only meaningful where CoW is supported (e.g. APFS); skip otherwise.
        if detect_backend() != OverlayBackend::NativeCow {
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join("a.txt"), "orig").unwrap();
        fs::create_dir_all(parent.path().join("sub")).unwrap();
        fs::write(parent.path().join("sub/b.txt"), "deep").unwrap();

        let overlay = BranchOverlay::create_with(parent.path(), OverlayBackend::NativeCow).unwrap();
        assert_eq!(overlay.backend(), OverlayBackend::NativeCow);
        // Clone saw the parent contents (read-through via the CoW copy).
        assert_eq!(fs::read_to_string(overlay.workspace().join("a.txt")).unwrap(), "orig");
        assert_eq!(fs::read_to_string(overlay.workspace().join("sub/b.txt")).unwrap(), "deep");
        // Writes stay local (CoW divergence).
        fs::write(overlay.workspace().join("a.txt"), "changed").unwrap();
        assert_eq!(fs::read_to_string(parent.path().join("a.txt")).unwrap(), "orig");
        // promote merges back.
        overlay.promote().unwrap();
        assert_eq!(fs::read_to_string(parent.path().join("a.txt")).unwrap(), "changed");
    }

    #[test]
    fn create_rejects_nonexistent_parent() {
        let err = BranchOverlay::create("/nonexistent/path/duduclaw_fork_test");
        assert!(err.is_err());
    }

    #[test]
    fn overlay_reads_parent_contents() {
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join("a.txt"), "hello").unwrap();
        let overlay = BranchOverlay::create(parent.path()).unwrap();
        let got = fs::read_to_string(overlay.workspace().join("a.txt")).unwrap();
        assert_eq!(got, "hello");
    }

    #[test]
    fn writes_stay_local_until_promote() {
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join("a.txt"), "orig").unwrap();
        let overlay = BranchOverlay::create(parent.path()).unwrap();

        // Branch writes locally.
        fs::write(overlay.workspace().join("a.txt"), "changed").unwrap();
        fs::write(overlay.workspace().join("new.txt"), "added").unwrap();

        // Parent unchanged before promote.
        assert_eq!(fs::read_to_string(parent.path().join("a.txt")).unwrap(), "orig");
        assert!(!parent.path().join("new.txt").exists());

        // Promote merges writes through.
        overlay.promote().unwrap();
        assert_eq!(fs::read_to_string(parent.path().join("a.txt")).unwrap(), "changed");
        assert_eq!(fs::read_to_string(parent.path().join("new.txt")).unwrap(), "added");
    }

    #[test]
    fn nested_dirs_copied() {
        let parent = tempfile::tempdir().unwrap();
        fs::create_dir_all(parent.path().join("sub/deep")).unwrap();
        fs::write(parent.path().join("sub/deep/x.txt"), "y").unwrap();
        let overlay = BranchOverlay::create(parent.path()).unwrap();
        assert_eq!(
            fs::read_to_string(overlay.workspace().join("sub/deep/x.txt")).unwrap(),
            "y"
        );
    }
}
