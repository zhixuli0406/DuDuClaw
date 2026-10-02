//! Cross-process publication keyed by the canonical parent workspace.
//! Locks live in a host-owned registry, never inside immutable workspace trees.
//!
//! Callers acquire their home/store lock first, then this lock, and keep both
//! through promotion, the durable result and retained-workspace cleanup.

use std::path::{Path, PathBuf};

use crate::{CopyPolicy, CopyReport, ForkError, Result};

#[cfg(windows)]
#[path = "publication_windows.rs"]
mod windows;

#[cfg(windows)]
pub(crate) fn check_private_directory(path: &Path) -> Result<()> { windows::check_private_directory(path) }
#[cfg(windows)]
pub(crate) fn make_private_directory(path: &Path) -> Result<()> { windows::make_private_directory(path) }

/// Legacy sidecar name remains reserved so old files cannot replace held locks.
pub const PUBLICATION_LOCK_NAME: &str = ".duduclaw-fork-publication.lock";

/// Capability available only while the canonical parent's lock is held.
pub struct ParentPublication {
    parent: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(windows)]
    identity: Vec<u8>,
}

impl ParentPublication {
    pub fn promote(&self, workspace: &Path, policy: &CopyPolicy) -> Result<CopyReport> {
        self.verify_parent()?;
        let report = policy.copy_tree(workspace, &self.parent)?;
        self.verify_parent()?;
        Ok(report)
    }

    fn verify_parent(&self) -> Result<()> {
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            let current = std::fs::symlink_metadata(&self.parent).map_err(|error| ForkError::Overlay(error.to_string()))?;
            if !current.is_dir() || (current.dev(), current.ino()) != self.identity {
                return Err(ForkError::Overlay("publication parent identity changed".into()));
            }
        }
        #[cfg(windows)] if windows::parent_identity(&self.parent)? != self.identity {
            return Err(ForkError::Overlay("publication parent identity changed".into()));
        }
        Ok(())
    }
}

/// Serialize file application and its publication across fork IDs, processes,
/// homes and symlink aliases. This is mutual exclusion, not crash rollback.
pub fn with_parent_publication<T>(
    parent: &Path, action: impl FnOnce(&ParentPublication) -> Result<T>,
) -> Result<T> {
    #[cfg(windows)]
    { return windows::with_parent_publication(parent, action); }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, action);
        return Err(ForkError::Config("parent publication requires a validated Unix private lock registry".into()));
    }
    #[cfg(unix)]
    {
        use fs2::FileExt;
        use sha2::{Digest, Sha256};
        use std::fs::OpenOptions;
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
        let parent = parent.canonicalize()
            .map_err(|error| ForkError::Overlay(format!("canonicalize publication parent: {error}")))?;
        if !parent.is_dir() {
            return Err(ForkError::Overlay("publication parent is not a directory".into()));
        }
        // Pin the actual directory before waiting. dev/ino identifies aliases
        // of the same workspace and cannot be recycled while this fd is alive.
        let parent_directory = OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&parent).map_err(|error| ForkError::Overlay(format!("pin publication parent: {error}")))?;
        let parent_metadata = parent_directory.metadata().map_err(|error| ForkError::Overlay(error.to_string()))?;
        let identity = (parent_metadata.dev(), parent_metadata.ino());
        // A fixed OS location: ambient TMPDIR must not split cooperating processes
        // into different registries, nor redirect a trusted lock into an agent tree.
        let uid = unsafe { libc::geteuid() };
        let registry = Path::new("/tmp").canonicalize()
            .map_err(|error| ForkError::Overlay(format!("canonicalize OS lock root: {error}")))?
            .join(format!(".duduclaw-fork-locks-{uid}"));
        if parent.starts_with(&registry) {
            return Err(ForkError::Config("fork parent cannot be the host lock registry".into()));
        }
        match std::fs::DirBuilder::new().mode(0o700).create(&registry) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(ForkError::Overlay(format!("create publication registry: {error}"))),
        }
        validate_registry(&registry, uid)?;
        let directory = OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&registry).map_err(|error| ForkError::Overlay(format!("open publication registry: {error}")))?;
        let directory_meta = directory.metadata().map_err(|error| ForkError::Overlay(error.to_string()))?;
        validate_private_metadata(&directory_meta, uid, true)?;
        let digest = Sha256::digest(format!("{}:{}", identity.0, identity.1).as_bytes());
        let lock_path = registry.join(format!("{digest:x}.lock"));
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&lock_path)
            .map_err(|error| ForkError::Overlay(format!("open publication lock: {error}")))?;
        let file_meta = file.metadata().map_err(|error| ForkError::Overlay(error.to_string()))?;
        validate_private_metadata(&file_meta, uid, false)?;
        file.lock_exclusive().map_err(|error| ForkError::Overlay(format!("parent publication lock: {error}")))?;
        // Refuse a renamed registry or sidecar after waiting. Never publish using
        // an old inode while another entrant can acquire its replacement.
        let current_directory = validate_registry(&registry, uid)?;
        let current_file = std::fs::symlink_metadata(&lock_path)
            .map_err(|error| ForkError::Overlay(error.to_string()))?;
        validate_private_metadata(&current_file, uid, false)?;
        if current_directory.dev() != directory_meta.dev() || current_directory.ino() != directory_meta.ino()
            || current_file.dev() != file_meta.dev() || current_file.ino() != file_meta.ino() {
            return Err(ForkError::Overlay("publication registry or lock inode changed".into()));
        }
        let publication = ParentPublication { parent, identity };
        publication.verify_parent()?;
        let result = action(&publication);
        let _ = FileExt::unlock(&file);
        result
    }
}

#[cfg(unix)]
fn validate_registry(path: &Path, uid: u32) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| ForkError::Overlay(error.to_string()))?;
    validate_private_metadata(&metadata, uid, true)?;
    Ok(metadata)
}

#[cfg(unix)]
fn validate_private_metadata(metadata: &std::fs::Metadata, uid: u32, directory: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let expected_type = if directory { metadata.file_type().is_dir() } else { metadata.file_type().is_file() };
    if !expected_type || metadata.uid() != uid || metadata.mode() & 0o7777 != if directory { 0o700 } else { 0o600 }
        || (!directory && metadata.nlink() != 1) {
        return Err(ForkError::Overlay("publication registry/lock has unsafe type, owner, mode or links".into()));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn parent_lock_serializes_conflicting_publication_across_processes_and_aliases() {
        if let Some(parent) = std::env::var_os("DUDU_TEST_FORK_PUBLICATION_PARENT") {
            let source = PathBuf::from(std::env::var_os("DUDU_TEST_FORK_PUBLICATION_SOURCE").unwrap());
            std::fs::write(source.join("ready"), "ready").unwrap();
            crate::promote_workspace(&source, Path::new(&parent), &CopyPolicy::fork_default()).unwrap();
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let first = root.path().join("first");
        let second = root.path().join("second");
        for dir in [&parent, &first, &second] { std::fs::create_dir(dir).unwrap(); }
        for name in ["winner.txt", "paired.txt"] {
            std::fs::write(first.join(name), "first").unwrap();
            std::fs::write(second.join(name), "second").unwrap();
        }
        let alias = root.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&parent, &alias).unwrap();
        #[cfg(not(unix))]
        let alias = parent.join(".");
        let mut child = None;
        let observed = with_parent_publication(&parent, |publication| {
            publication.promote(&first, &CopyPolicy::fork_default())?;
            child = Some(ChildGuard(std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact").arg("publication::tests::parent_lock_serializes_conflicting_publication_across_processes_and_aliases")
                .env_clear().env("DUDU_TEST_FORK_PUBLICATION_PARENT", &alias)
                .env("DUDU_TEST_FORK_PUBLICATION_SOURCE", &second)
                .spawn().unwrap()));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !second.join("ready").exists() && std::time::Instant::now() < deadline {
                assert!(child.as_mut().unwrap().0.try_wait().unwrap().is_none());
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(second.join("ready").exists(), "child reached the real promotion path");
            std::thread::sleep(std::time::Duration::from_millis(100));
            let waiting = child.as_mut().unwrap().0.try_wait().unwrap().is_none();
            let contents = ["winner.txt", "paired.txt"].map(|name| std::fs::read_to_string(parent.join(name)).unwrap());
            // Keep the lock through the publication callback, after copying.
            std::fs::write(parent.join("committed.txt"), "first committed").unwrap();
            Ok((waiting, contents))
        }).unwrap();
        let mut child = child.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() { break status; }
            assert!(std::time::Instant::now() < deadline, "publication child did not finish after release");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(status.success());
        assert!(observed.0, "the second process must wait through commit");
        assert_eq!(observed.1, ["first", "first"]);
        for name in ["winner.txt", "paired.txt"] {
            assert_eq!(std::fs::read_to_string(parent.join(name)).unwrap(), "second");
        }
        assert_eq!(std::fs::read_to_string(parent.join("committed.txt")).unwrap(), "first committed");
    }

    #[test]
    fn same_parent_case_alias_cannot_bypass_publication_lock() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("MixedCaseParent");
        let alias = root.path().join("mixedcaseparent");
        std::fs::create_dir(&parent).unwrap();
        if !alias.is_dir() { return; } // A case-sensitive filesystem has no such alias.
        let original = parent.metadata().unwrap();
        let alternate = alias.metadata().unwrap();
        assert_eq!((original.dev(), original.ino()), (alternate.dev(), alternate.ino()));
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("winner.txt"), "second").unwrap();
        std::fs::write(parent.join("winner.txt"), "first").unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let mut worker = None;
        let observed = with_parent_publication(&parent, |_| {
            worker = Some(std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                let result = crate::promote_workspace(&source, &alias, &CopyPolicy::fork_default());
                finished_tx.send(result).unwrap();
            }));
            ready_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            let completed_while_held = finished_rx.recv_timeout(std::time::Duration::from_millis(200)).ok();
            let contents = std::fs::read_to_string(parent.join("winner.txt")).unwrap();
            Ok((completed_while_held, contents))
        }).unwrap();
        worker.unwrap().join().unwrap();
        if let Some(result) = &observed.0 { assert!(result.is_ok()); }
        else { assert!(finished_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().is_ok()); }
        assert!(observed.0.is_none(), "a case alias of the same inode must wait for publication");
        assert_eq!(observed.1, "first", "the locked parent must remain unchanged");
        assert_eq!(std::fs::read_to_string(parent.join("winner.txt")).unwrap(), "second");
    }

    #[test]
    fn waiter_rejects_parent_directory_replaced_while_waiting() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let source = root.path().join("source");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("winner.txt"), "stale branch").unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let target = parent.clone();
        let mut worker = None;
        with_parent_publication(&parent, |_| {
            worker = Some(std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                done_tx.send(crate::promote_workspace(&source, &target, &CopyPolicy::fork_default())).unwrap();
            }));
            ready_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert!(done_rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "entrant is waiting on the held parent lock");
            std::fs::rename(&parent, root.path().join("original-parent")).unwrap();
            std::fs::create_dir(&parent).unwrap();
            std::fs::write(parent.join("winner.txt"), "replacement parent").unwrap();
            Ok(())
        }).unwrap();
        worker.unwrap().join().unwrap();
        assert!(done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().is_err(),
            "a waiter must refuse a different parent inode after acquiring its lock");
        assert_eq!(std::fs::read_to_string(parent.join("winner.txt")).unwrap(), "replacement parent");
    }

    #[test]
    fn parent_lock_releases_on_error_without_changing_immutable_workspace() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::write(parent.path().join("seed.txt"), "immutable").unwrap();
        let failed: Result<()> = with_parent_publication(parent.path(), |_| Err(ForkError::Executor("fixture".into())));
        assert!(failed.is_err());
        with_parent_publication(parent.path(), |_| Ok(())).unwrap();
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_to_string(parent.path().join("seed.txt")).unwrap(), "immutable");
    }

    #[test]
    fn registry_validation_rejects_symlinks_broad_modes_and_hardlinked_lock_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let uid = unsafe { libc::geteuid() };
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_registry(root.path(), uid).is_ok());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_registry(root.path(), uid).is_err());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias_root = tempfile::tempdir().unwrap();
        let alias = alias_root.path().join("registry");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        assert!(validate_registry(&alias, uid).is_err());
        assert!(validate_registry(root.path(), uid.wrapping_add(1)).is_err());
        let file = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        assert!(validate_private_metadata(&file.as_file().metadata().unwrap(), uid, false).is_ok());
        std::fs::hard_link(file.path(), root.path().join("alias.lock")).unwrap();
        assert!(validate_private_metadata(&file.as_file().metadata().unwrap(), uid, false).is_err());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn windows_parent_mutex_serializes_real_promotion_and_preserves_snapshot() {
        let parent = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(parent.path().join("winner.txt"), "first").unwrap();
        std::fs::write(source.path().join("winner.txt"), "second").unwrap();
        let target = parent.path().join(".");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let mut worker = None;
        let observation = with_parent_publication(parent.path(), |_| {
            worker = Some(std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                let result = crate::promote_workspace(source.path(), &target, &CopyPolicy::fork_default());
                done_tx.send(result).unwrap();
            }));
            ready_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            Ok((done_rx.recv_timeout(std::time::Duration::from_millis(200)).ok(),
                std::fs::read_to_string(parent.path().join("winner.txt")).unwrap()))
        }).unwrap();
        worker.unwrap().join().unwrap();
        assert!(observation.0.is_none(), "another thread must wait through publication");
        assert_eq!(observation.1, "first");
        assert!(done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().is_ok());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1, "no lock inside immutable parent");
        assert_eq!(std::fs::read_to_string(parent.path().join("winner.txt")).unwrap(), "second");
        let failed: Result<()> = with_parent_publication(parent.path(), |_| Err(ForkError::Executor("fixture".into())));
        assert!(failed.is_err());
        with_parent_publication(parent.path(), |_| Ok(())).unwrap();
    }
}
