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

use crate::copy_policy::CopyPolicy;
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
    policy: CopyPolicy,
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

    /// Create with an explicit backend (used by tests to exercise both paths),
    /// under [`CopyPolicy::fork_default`].
    pub fn create_with(parent: impl AsRef<Path>, backend: OverlayBackend) -> Result<Self> {
        Self::create_with_policy(parent, backend, CopyPolicy::fork_default())
    }

    /// Create with an explicit backend and copy policy. Both backends end in the
    /// same state: the native clone gets a policy post-pass that removes
    /// excluded names, escaping symlinks and special files.
    pub fn create_with_policy(
        parent: impl AsRef<Path>,
        backend: OverlayBackend,
        policy: CopyPolicy,
    ) -> Result<Self> {
        let parent = parent.as_ref();
        if !parent.is_dir() {
            return Err(ForkError::Overlay(format!(
                "parent workspace is not a directory: {}",
                parent.display()
            )));
        }
        // Canonical parent: a symlinked parent path must not make `cp -R` clone
        // the link itself (which would alias the branch onto the parent).
        let parent = parent
            .canonicalize()
            .map_err(|e| ForkError::Overlay(format!("canonicalize parent: {e}")))?;
        // Snapshot readers must not observe a multi-file promotion halfway
        // through; the reserved sidecar is excluded even for custom policies.
        let publication_parent = parent.clone();
        crate::with_parent_publication(&publication_parent, |_| {
            let root = tempfile::Builder::new()
                .prefix("duduclaw_fork_")
                .tempdir()
                .map_err(|e| ForkError::Overlay(format!("create overlay tempdir: {e}")))?;
            crate::retention::set_private(root.path())?;
            let work = root.path().join("ws"); // must not exist for native clone

            let native_ok = backend == OverlayBackend::NativeCow
                && clone_tree_native(&parent, &work).is_ok()
                && match policy.sanitize_clone(&parent, &work) {
                    Ok(_) => true,
                    Err(e) => {
                        tracing::warn!("fork overlay: CoW post-pass failed, using snapshot: {e}");
                        false
                    }
                };
            let effective = if native_ok {
                OverlayBackend::NativeCow
            } else {
                // Snapshot (also the fallback when a native clone or its post-pass
                // fails mid-create). Start from an empty target.
                if std::fs::symlink_metadata(&work).is_ok() {
                    std::fs::remove_dir_all(&work)
                        .map_err(|e| ForkError::Overlay(format!("reset overlay dir: {e}")))?;
                }
                policy.copy_tree(&parent, &work)?;
                OverlayBackend::Snapshot
            };
            Ok(BranchOverlay { parent, _root: root, work, backend: effective, policy })
        })
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

    /// Last-resort recovery: relinquish automatic deletion of the private root.
    pub(crate) fn keep_source(self) -> PathBuf {
        let workspace = self.work.clone();
        self._root.keep();
        workspace
    }

    /// Merge this branch's writes back into the parent workspace (winner only).
    ///
    /// Overwrites parent files that the branch changed and adds new ones. Files the
    /// branch deleted are *not* propagated (additive merge). The copy policy
    /// applies: excluded names and escaping symlinks are never written back.
    pub fn promote(&self) -> Result<()> {
        promote_workspace(&self.work, &self.parent, &self.policy).map(|_| ())
    }

    /// Move this branch workspace out of its temp dir to `dest` (which must not
    /// exist) so it survives the overlay being dropped. Renames when `dest` is on
    /// the same filesystem, otherwise copies under the overlay's policy and lets
    /// the temp dir be removed on drop. Returns the persisted path.
    pub fn persist_to(self, dest: &Path) -> Result<PathBuf> {
        if std::fs::symlink_metadata(dest).is_ok() {
            return Err(ForkError::Overlay(format!(
                "retained workspace destination already exists: {}",
                dest.display()
            )));
        }
        if std::fs::rename(&self.work, dest).is_err() {
            if let Err(e) = self.policy.copy_tree(&self.work, dest) {
                let _ = std::fs::remove_dir_all(dest);
                return Err(e);
            }
        }
        Ok(dest.to_path_buf())
        // `self` drops here: the temp root (now without `ws` after a rename) goes.
    }
}

/// Promote a (possibly retained) branch workspace into `parent` under `policy`.
/// Shared by [`BranchOverlay::promote`] and the deferred `merge_or_select` path.
pub fn promote_workspace(
    workspace: &Path,
    parent: &Path,
    policy: &CopyPolicy,
) -> Result<crate::copy_policy::CopyReport> {
    if !parent.is_dir() {
        return Err(ForkError::Overlay(format!(
            "parent workspace is not a directory: {}",
            parent.display()
        )));
    }
    crate::with_parent_publication(parent, |publication| publication.promote(workspace, policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn publication_lock_inode_is_never_copied_or_replaced_by_a_branch() {
        let parent = tempfile::tempdir().unwrap();
        let reserved = ".duduclaw-fork-publication.lock";
        fs::write(parent.path().join(reserved), "host-owned lock").unwrap();
        let overlay = BranchOverlay::create_with_policy(parent.path(), OverlayBackend::Snapshot,
            CopyPolicy::with_excludes(Vec::<String>::new())).unwrap();
        assert!(!overlay.workspace().join(reserved).exists());
        fs::write(overlay.workspace().join(reserved), "branch replacement").unwrap();
        overlay.promote().unwrap();
        assert_eq!(fs::read_to_string(parent.path().join(reserved)).unwrap(), "host-owned lock");
    }

    #[test]
    fn parent_equal_to_home_cannot_replace_held_resolution_lock_and_admit_new_writer() {
        let home = tempfile::tempdir().unwrap();
        let key = home.path().join("fork_resolution.lock");
        let (admitted, await_admitted) = std::sync::mpsc::channel();
        let mut writer = None;
        let entered_early = duduclaw_core::with_file_lock(&key, || {
            let overlay = BranchOverlay::create_with_policy(home.path(), OverlayBackend::Snapshot,
                CopyPolicy::with_excludes(Vec::<String>::new())).unwrap();
            // A branch can invent the reserved file even after a safe snapshot.
            fs::write(overlay.workspace().join("fork_resolution.lock.lock"), "replace host inode").unwrap();
            overlay.promote().unwrap();
            let key = key.clone();
            writer = Some(std::thread::spawn(move || {
                duduclaw_core::with_file_lock(&key, || {
                    admitted.send(()).unwrap();
                    Ok(())
                }).unwrap();
            }));
            Ok(await_admitted.recv_timeout(std::time::Duration::from_millis(200)).is_ok())
        }).unwrap();
        writer.unwrap().join().unwrap();
        assert!(!entered_early, "a new writer must wait for the original HOME lock holder");
    }

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

    fn backends() -> Vec<OverlayBackend> {
        // NativeCow silently degrades to Snapshot where unsupported, so both
        // variants are always safe to request.
        vec![OverlayBackend::Snapshot, OverlayBackend::NativeCow]
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_outside_file_is_not_copied_on_any_backend() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "outside-secret").unwrap();
        for backend in backends() {
            let parent = tempfile::tempdir().unwrap();
            fs::write(parent.path().join("a.txt"), "a").unwrap();
            fs::create_dir_all(parent.path().join("sub")).unwrap();
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                parent.path().join("sub/leak"),
            )
            .unwrap();
            std::os::unix::fs::symlink(outside.path(), parent.path().join("leakdir")).unwrap();

            let overlay = BranchOverlay::create_with(parent.path(), backend).unwrap();
            let ws = overlay.workspace();
            assert!(fs::symlink_metadata(ws.join("sub/leak")).is_err(), "{backend:?}");
            assert!(fs::symlink_metadata(ws.join("leakdir")).is_err(), "{backend:?}");
            assert_eq!(fs::read_to_string(ws.join("a.txt")).unwrap(), "a");
        }
    }

    #[test]
    fn excluded_names_absent_on_any_backend() {
        for backend in backends() {
            let parent = tempfile::tempdir().unwrap();
            fs::write(parent.path().join(".env"), "TOKEN=x").unwrap();
            fs::write(parent.path().join(".env.production"), "TOKEN=y").unwrap();
            fs::create_dir_all(parent.path().join("certs")).unwrap();
            fs::write(parent.path().join("certs/server.pem"), "pem").unwrap();
            fs::write(parent.path().join("certs/id_rsa"), "key").unwrap();
            fs::write(parent.path().join(".netrc"), "machine").unwrap();
            fs::create_dir_all(parent.path().join(".claude")).unwrap();
            fs::write(parent.path().join(".claude/settings.json"), "{}").unwrap();
            fs::write(parent.path().join("main.rs"), "fn main(){}").unwrap();

            let overlay = BranchOverlay::create_with(parent.path(), backend).unwrap();
            let ws = overlay.workspace();
            for gone in [".env", ".env.production", "certs/server.pem", "certs/id_rsa", ".netrc"] {
                assert!(fs::symlink_metadata(ws.join(gone)).is_err(), "{backend:?}: {gone}");
            }
            // Hooks and ordinary files are kept.
            assert!(ws.join(".claude/settings.json").is_file(), "{backend:?}");
            assert!(ws.join("main.rs").is_file(), "{backend:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn promote_drops_branch_created_escaping_symlink_and_secret() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("host_secret"), "host").unwrap();
        for backend in backends() {
            let parent = tempfile::tempdir().unwrap();
            fs::write(parent.path().join("a.txt"), "orig").unwrap();
            let overlay = BranchOverlay::create_with(parent.path(), backend).unwrap();
            let ws = overlay.workspace();
            fs::write(ws.join("a.txt"), "changed").unwrap();
            fs::write(ws.join(".env"), "LEAK=1").unwrap();
            std::os::unix::fs::symlink(outside.path().join("host_secret"), ws.join("grab"))
                .unwrap();
            // A symlink pointing at the parent's absolute path is outside the
            // branch root too.
            std::os::unix::fs::symlink(parent.path().join("a.txt"), ws.join("to_parent")).unwrap();

            overlay.promote().unwrap();
            assert_eq!(fs::read_to_string(parent.path().join("a.txt")).unwrap(), "changed");
            assert!(fs::symlink_metadata(parent.path().join(".env")).is_err(), "{backend:?}");
            assert!(fs::symlink_metadata(parent.path().join("grab")).is_err(), "{backend:?}");
            assert!(fs::symlink_metadata(parent.path().join("to_parent")).is_err(), "{backend:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn in_root_relative_symlink_survives_copy_and_promote() {
        for backend in backends() {
            let parent = tempfile::tempdir().unwrap();
            fs::create_dir_all(parent.path().join("docs")).unwrap();
            fs::write(parent.path().join("docs/guide.md"), "guide").unwrap();
            std::os::unix::fs::symlink("docs/guide.md", parent.path().join("GUIDE")).unwrap();

            let overlay = BranchOverlay::create_with(parent.path(), backend).unwrap();
            let link = overlay.workspace().join("GUIDE");
            assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "{backend:?}");
            assert_eq!(fs::read_link(&link).unwrap(), Path::new("docs/guide.md"));
            assert_eq!(fs::read_to_string(&link).unwrap(), "guide");

            // Branch adds its own in-root link; promote recreates it as a link.
            std::os::unix::fs::symlink("docs", overlay.workspace().join("d")).unwrap();
            overlay.promote().unwrap();
            let promoted = parent.path().join("d");
            assert!(fs::symlink_metadata(&promoted).unwrap().file_type().is_symlink());
            assert_eq!(fs::read_to_string(promoted.join("guide.md")).unwrap(), "guide");
        }
    }

    #[test]
    fn persist_to_moves_workspace_out_of_temp() {
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join("a.txt"), "a").unwrap();
        let keep = tempfile::tempdir().unwrap();
        let dest = keep.path().join("retained");
        let overlay = BranchOverlay::create_with(parent.path(), OverlayBackend::Snapshot).unwrap();
        fs::write(overlay.workspace().join("new.txt"), "branch").unwrap();
        let temp_ws = overlay.workspace().to_path_buf();
        let got = overlay.persist_to(&dest).unwrap();
        assert_eq!(got, dest);
        assert_eq!(fs::read_to_string(dest.join("new.txt")).unwrap(), "branch");
        assert!(!temp_ws.exists(), "temp workspace is gone after persist");
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
