//! Private security databases: no-follow at both the filesystem and SQLite
//! layers. SQLite opens by pathname (including later journals), so every
//! ancestor must prevent another uid from replacing the next path component.
use std::path::Path;
#[cfg(not(windows))]
pub(crate) use rusqlite::Connection;
#[cfg(windows)]
#[path = "sqlite_windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::Connection;

// Windows databases and SQLite sidecars live below this protected DACL
// boundary, so later changes to the home's inherited ACL cannot expose them.
#[cfg(windows)]
pub(super) const PRIVATE_DIRECTORY: &str = "secure-db";

/// Resolve the platform layout for diagnostics and cross-crate tests only.
/// This does not validate the path or replace the secure open operation.
#[doc(hidden)]
pub fn database_path(home: &Path, name: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    { home.join(PRIVATE_DIRECTORY).join(name) }
    #[cfg(not(windows))]
    { home.join(name) }
}

pub(crate) fn open(home: &Path, name: &str) -> Result<Connection, String> {
    #[cfg(unix)]
    { open_with(home, name, || {}) }
    #[cfg(windows)]
    { windows::open(home, name) }
    #[cfg(not(any(unix, windows)))]
    { let _ = (home, name); Err("secure database open unavailable".into()) }
}

/// SQLite access for [`open_existing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access { ReadOnly, ReadWrite }

/// Open an authority database that must already exist, e.g. `users.db` or
/// `migration-confirmations.db` in the DuDuClaw home, or `apply.db` in
/// `<home>/migration-authority`. Same checks as [`open`] (no-follow
/// descriptor walk from `/` through `directory`, every ancestor owned by the
/// effective uid or root and not group/other writable, `directory` itself
/// owned by the effective uid; the database and any present `-journal`,
/// `-wal`, `-shm` sidecar a regular, single-link file owned by the effective
/// uid without group/other write; dev/inode of the directory and database
/// identical before and after the open; `SQLITE_OPEN_NOFOLLOW`), but it never
/// creates the file and SQLite never gets `CREATE`: a missing or replaced
/// store fails closed. Unix only; every other platform refuses.
pub(crate) fn open_existing(directory: &Path, name: &str, access: Access) -> Result<rusqlite::Connection, String> {
    #[cfg(unix)]
    { open_unix(directory, name, Some(access), || {}) }
    #[cfg(not(unix))]
    { let _ = (directory, name, access); Err("secure database open unavailable".into()) }
}

/// [`open_existing`] with a hook between the last pre-open check and the
/// SQLite open, for race tests (e.g. swapping the database inode).
#[cfg(all(test, unix))]
pub(crate) fn open_existing_with(directory: &Path, name: &str, access: Access, before_sqlite: impl FnOnce())
    -> Result<rusqlite::Connection, String> {
    open_unix(directory, name, Some(access), before_sqlite)
}

#[cfg(unix)]
fn canonical_home(home: &Path) -> Result<std::path::PathBuf, String> {
    use std::path::Component;
    let absolute = if home.is_absolute() { home.to_owned() } else {
        std::env::current_dir().map_err(|e| format!("cannot resolve database home: {e}"))?.join(home)
    };
    if absolute.components().any(|c| !matches!(c, Component::RootDir | Component::Normal(_))) {
        return Err("secure database open refused: invalid home path component".into());
    }
    // Canonicalize once, before the descriptor walk. Only fixed OS root
    // aliases may change the spelling; arbitrary home/ancestor links fail.
    let canonical = absolute.canonicalize().map_err(|e| format!("cannot resolve database home: {e}"))?;
    let expected = absolute.clone();
    #[cfg(target_os = "macos")]
    let expected = {
        use std::os::unix::fs::MetadataExt;
        let mut expected = expected;
        for (alias, target) in [("/var", "/private/var"), ("/tmp", "/private/tmp"), ("/etc", "/private/etc")] {
            if let Ok(tail) = absolute.strip_prefix(alias) {
                let meta = std::fs::symlink_metadata(alias).map_err(|e| e.to_string())?;
                if meta.file_type().is_symlink() && meta.uid() == 0
                    && Path::new("/").join(std::fs::read_link(alias).map_err(|e| e.to_string())?) == Path::new(target) {
                    expected = Path::new(target).join(tail);
                }
                break;
            }
        }
        expected
    };
    if canonical != expected {
        return Err("secure database open refused: home path contains a non-system symlink".into());
    }
    Ok(canonical)
}

#[cfg(unix)]
fn ancestor_policy(owner: u32, mode: u32, uid: u32, after_sticky: bool) -> bool {
    (owner == uid || owner == 0)
        && (!after_sticky || owner == uid)
        && (mode & 0o022 == 0 || (owner == 0 && mode & 0o1000 != 0))
}

#[cfg(unix)]
fn trusted_directory(home: &Path) -> Result<super::SafeDir, String> {
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;
    let uid = nix::unistd::geteuid().as_raw();
    let mut directory = super::SafeDir::open_root(Path::new("/")).map_err(|e| e.to_string())?;
    let mut path = std::path::PathBuf::from("/");
    let mut after_sticky = false;
    // Include / itself. child_dir uses lstat-at + openat(O_NOFOLLOW); the
    // policy is checked on the opened descriptor, never just a pathname.
    for component in home.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                path.push(name);
                let name = name.to_str().ok_or("secure database open refused: non-UTF-8 directory")?;
                directory = directory.child_dir(name, false)
                    .map_err(|e| format!("secure database ancestor {}: {e}", path.display()))?
                    .ok_or_else(|| format!("secure database ancestor {} is missing", path.display()))?;
            }
            _ => return Err("secure database open refused: non-absolute home path".into()),
        }
        let meta = directory.metadata().map_err(|e| e.to_string())?;
        if !meta.is_dir() || !ancestor_policy(meta.uid(), meta.mode(), uid, after_sticky) {
            return Err(format!("secure database open refused: unsafe ancestor {} (uid {}, mode {:04o}); ancestors must be owned by the effective uid or root and not group/other writable; a writable root-owned sticky directory requires the next component to be owned by the effective uid", path.display(), meta.uid(), meta.mode() & 0o7777));
        }
        after_sticky = meta.mode() & 0o022 != 0;
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_with(home: &Path, name: &str, before_sqlite: impl FnOnce()) -> Result<Connection, String> {
    open_unix(home, name, None, before_sqlite)
}

/// `existing == None`: create-if-missing private database, read-write.
/// `Some(access)`: the database must already exist; never created.
#[cfg(unix)]
fn open_unix(home: &Path, name: &str, existing: Option<Access>, before_sqlite: impl FnOnce()) -> Result<Connection, String> {
    use std::os::unix::fs::MetadataExt;
    use rusqlite::OpenFlags;
    let denied = || "secure database open unavailable".to_owned();
    super::check_name(name).map_err(|_| denied())?;
    let canonical = canonical_home(home)?;
    let home = canonical.as_path();
    let directory = trusted_directory(home)?;
    let parent = directory.metadata().map_err(|_| denied())?;
    let uid = nix::unistd::geteuid().as_raw();
    if !parent.is_dir() || parent.uid() != uid || parent.mode() & 0o022 != 0 {
        return Err(denied());
    }
    // Exclusive creation establishes mode 0600 before SQLite opens anything.
    // Do not give SQLite CREATE: a disappeared main file must fail closed.
    // An existing-only open never creates: a missing store is refused below.
    if existing.is_none() {
        directory.create_file_new(name, &[]).map_err(|_| denied())?;
    }
    let path = home.join(name);
    // Do not open/close an extra descriptor on an existing database: POSIX
    // close() drops all of this process's record locks on the inode, including
    // locks held by another live SQLite connection. SQLite owns its descriptors.
    // lstat plus NOFOLLOW and the identity checks below preserve the path gate.
    let original = std::fs::symlink_metadata(&path).map_err(|_| denied())?;
    let valid_file = |m: &std::fs::Metadata| {
        m.is_file() && m.nlink() == 1 && m.uid() == uid && m.mode() & 0o022 == 0
    };
    if !valid_file(&original) { return Err(denied()); }
    let verify = || -> Result<(), String> {
        let current_parent = std::fs::symlink_metadata(home).map_err(|_| denied())?;
        if !current_parent.is_dir() || current_parent.dev() != parent.dev() || current_parent.ino() != parent.ino()
            || current_parent.uid() != uid || current_parent.mode() & 0o022 != 0 {
            return Err(denied());
        }
        let current = std::fs::symlink_metadata(&path).map_err(|_| denied())?;
        if !valid_file(&current) || current.dev() != original.dev() || current.ino() != original.ino() {
            return Err(denied());
        }
        for suffix in ["-journal", "-wal", "-shm"] {
            match std::fs::symlink_metadata(home.join(format!("{name}{suffix}"))) {
                Ok(metadata) if valid_file(&metadata) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(denied()),
            }
        }
        Ok(())
    };
    verify()?;
    before_sqlite();
    // NOFOLLOW also rejects symlinks in SQLite's full-path resolution. The
    // bundled Unix VFS opens main/journal/WAL/SHM with O_NOFOLLOW as well.
    // SQLite owns its descriptors (no fd to fstat, see above), so the
    // dev/inode identity is re-checked by lstat right after the open.
    let access = match existing {
        Some(Access::ReadOnly) => OpenFlags::SQLITE_OPEN_READ_ONLY,
        None | Some(Access::ReadWrite) => OpenFlags::SQLITE_OPEN_READ_WRITE,
    };
    let connection = Connection::open_with_flags(&path,
        access | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map_err(|_| denied())?;
    verify()?;
    Ok(connection)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};

    #[test]
    fn private_sqlite_creates_0600_and_reopens() {
        let home = crate::fs_safe::private_test_home().unwrap();
        open(home.path(), "state.db").unwrap().execute_batch("CREATE TABLE example(value)").unwrap();
        assert_eq!(std::fs::metadata(home.path().join("state.db")).unwrap().permissions().mode() & 0o777, 0o600);
        open(home.path(), "state.db").unwrap().execute("INSERT INTO example VALUES (1)", []).unwrap();
    }

    #[test]
    fn private_sqlite_accepts_private_home_under_sticky_temp_root() {
        // Use the OS's actual root-owned sticky directory; an unprivileged
        // fixture cannot manufacture root ownership. /tmp is a macOS alias.
        use std::os::unix::fs::MetadataExt;
        let root = Path::new("/tmp").canonicalize().unwrap();
        let meta = std::fs::metadata(&root).unwrap();
        assert_eq!(meta.uid(), 0);
        assert_eq!(meta.mode() & 0o1777, 0o1777);
        let home = tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&root).unwrap();
        assert_eq!(std::fs::metadata(home.path()).unwrap().permissions().mode() & 0o777, 0o700);
        let db = open(home.path(), "state.db").unwrap();
        db.execute_batch("CREATE TABLE example(value); PRAGMA journal_mode=WAL; INSERT INTO example VALUES(1)").unwrap();
        for suffix in ["", "-wal", "-shm"] {
            assert_eq!(std::fs::metadata(home.path().join(format!("state.db{suffix}"))).unwrap()
                .permissions().mode() & 0o777, 0o600, "{suffix}");
        }
        drop(db);
        let name = format!("unsafe-{}.db", uuid::Uuid::new_v4());
        assert!(open(&root, &name).is_err());
        assert!(!root.join(name).exists());
    }

    #[test]
    fn private_sqlite_rejects_writable_ancestors_before_open() {
        for mode in [0o777, 0o775] {
            let root = crate::fs_safe::private_test_home().unwrap();
            let ancestor = root.path().join("ancestor");
            let home = ancestor.join("middle/home");
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&home).unwrap();
            std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(mode)).unwrap();
            let error = open_with(&home, "state.db", || panic!("unsafe path reached SQLite")).unwrap_err();
            assert!(error.contains("ancestor"), "{error}");
            assert!(!home.join("state.db").exists());
        }
    }

    #[test]
    fn private_sqlite_accepts_private_and_readable_ancestor_chains() {
        for mode in [0o700, 0o755] {
            let root = crate::fs_safe::private_test_home().unwrap();
            let ancestor = root.path().join("ancestor");
            let home = ancestor.join("home");
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&home).unwrap();
            std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(mode)).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(mode)).unwrap();
            open(&home, "state.db").unwrap();
        }
    }

    #[test]
    fn sticky_ancestor_requires_root_owner_and_our_next_entry() {
        assert!(ancestor_policy(0, 0o1777, 501, false));
        assert!(ancestor_policy(501, 0o700, 501, true));
        assert!(!ancestor_policy(0, 0o755, 501, true));
        assert!(!ancestor_policy(502, 0o700, 501, true));
        assert!(!ancestor_policy(501, 0o1777, 501, false));
        assert!(!ancestor_policy(502, 0o755, 501, false));
        let root = crate::fs_safe::private_test_home().unwrap();
        let home = root.path().join("home");
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
        if !nix::unistd::geteuid().is_root() {
            assert!(open(&home, "state.db").unwrap_err().contains("unsafe ancestor"));
            assert!(!home.join("state.db").exists());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_sqlite_accepts_only_system_root_aliases() {
        let home = tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(std::env::temp_dir()).unwrap();
        open(home.path(), "state.db").unwrap();
        let alias = home.path().join("alias");
        symlink(home.path(), &alias).unwrap();
        assert!(open(&alias, "state.db").is_err());
    }

    // POSIX locks are process-wide. Only a separate process can observe them;
    // asking through another connection in this process would miss the bug.
    fn shared_range_locked_by_parent(path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the child only invokes async-signal-safe system calls and
        // _exit; it never calls Rust destructors or SQLite after fork.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                let fd = libc::open(path.as_ptr(), libc::O_RDONLY);
                if fd < 0 { libc::_exit(2); }
                let mut lock: libc::flock = std::mem::zeroed();
                lock.l_type = libc::F_WRLCK as _;
                lock.l_whence = libc::SEEK_SET as _;
                lock.l_start = 0x4000_0002; // SQLite SHARED range, PENDING_BYTE + 2.
                lock.l_len = 510;
                if libc::fcntl(fd, libc::F_GETLK, &mut lock) != 0 { libc::_exit(2); }
                libc::_exit(if i32::from(lock.l_type) == i32::from(libc::F_UNLCK) { 1 } else { 0 });
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "lock probe child crashed");
        match libc::WEXITSTATUS(status) {
            0 => true,
            1 => false,
            _ => panic!("lock probe child failed"),
        }
    }

    #[test]
    fn private_sqlite_reopen_preserves_live_connection_locks() {
        for wal in [false, true] {
            let home = crate::fs_safe::private_test_home().unwrap();
            let live = open(home.path(), "state.db").unwrap();
            live.execute_batch("CREATE TABLE example(value)").unwrap();
            if wal { live.execute_batch("PRAGMA journal_mode=WAL").unwrap(); }
            live.execute_batch("BEGIN IMMEDIATE; INSERT INTO example VALUES(1)").unwrap();
            let path = home.path().join("state.db");
            assert!(shared_range_locked_by_parent(&path), "initial lock, WAL={wal}");
            drop(open(home.path(), "state.db").unwrap());
            assert!(shared_range_locked_by_parent(&path), "reopen lost live lock, WAL={wal}");
            live.execute_batch("COMMIT").unwrap();
            assert_eq!(open(home.path(), "state.db").unwrap()
                .query_row("SELECT count(*) FROM example", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
    }

    #[test]
    fn private_sqlite_rejects_links_specials_and_writable_parent() {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let home = crate::fs_safe::private_test_home().unwrap();
            let outside = home.path().join("outside");
            std::fs::write(&outside, b"untouched").unwrap();
            symlink(&outside, home.path().join(format!("state.db{suffix}"))).unwrap();
            assert!(open(home.path(), "state.db").is_err(), "{suffix}");
            assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
        }
        let home = crate::fs_safe::private_test_home().unwrap();
        std::fs::create_dir(home.path().join("state.db")).unwrap();
        assert!(open(home.path(), "state.db").is_err());
        std::fs::remove_dir(home.path().join("state.db")).unwrap();
        for mode in [0o775, 0o777] {
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(open(home.path(), "state.db").is_err());
            assert!(!home.path().join("state.db").exists());
        }
    }

    #[test]
    fn private_sqlite_rejects_parent_and_database_symlink_swaps_at_open() {
        let root = crate::fs_safe::private_test_home().unwrap();
        let home = root.path().join("home");
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let other = root.path().join("other");
        std::fs::DirBuilder::new().mode(0o700).create(&other).unwrap();
        open(&other, "state.db").unwrap().execute_batch("CREATE TABLE sentinel(value)").unwrap();
        assert!(open_with(&home, "state.db", || {
            std::fs::remove_file(home.join("state.db")).unwrap();
            symlink(other.join("state.db"), home.join("state.db")).unwrap();
        }).is_err());
        std::fs::remove_file(home.join("state.db")).unwrap();
        assert!(open_with(&home, "state.db", || {
            std::fs::rename(&home, root.path().join("original")).unwrap();
            symlink(&other, &home).unwrap();
        }).is_err());
        assert!(open(&home, "state.db").is_err());
        let count: i64 = open(&other, "state.db").unwrap().query_row("SELECT count(*) FROM sentinel", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn private_sqlite_rejects_hardlinks_and_foreign_owner() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let path = home.path().join("state.db");
        std::fs::write(&path, b"").unwrap();
        std::fs::hard_link(&path, home.path().join("alias")).unwrap();
        assert!(open(home.path(), "state.db").is_err());
        // Ownership itself is enforced by lstat; setting a foreign uid needs
        // privileges and is exercised only when the test runner has them.
        if nix::unistd::geteuid().is_root() {
            std::fs::remove_file(home.path().join("alias")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            nix::unistd::chown(&path, Some(nix::unistd::Uid::from_raw(65534)), None).unwrap();
            assert!(open(home.path(), "state.db").is_err());
        }
    }

    #[test]
    fn existing_open_never_creates_and_honours_read_only() {
        let home = crate::fs_safe::private_test_home().unwrap();
        assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_err());
        assert!(open_existing(home.path(), "state.db", Access::ReadWrite).is_err());
        assert!(!home.path().join("state.db").exists());
        open(home.path(), "state.db").unwrap().execute_batch("CREATE TABLE example(value)").unwrap();
        let read = open_existing(home.path(), "state.db", Access::ReadOnly).unwrap();
        assert!(read.is_readonly("main").unwrap());
        assert!(read.execute("INSERT INTO example VALUES (1)", []).is_err());
        open_existing(home.path(), "state.db", Access::ReadWrite).unwrap().execute("INSERT INTO example VALUES (1)", []).unwrap();
        // A subdirectory store (e.g. migration-authority/apply.db) is walked the same way.
        let nested = home.path().join("nested");
        std::fs::DirBuilder::new().mode(0o700).create(&nested).unwrap();
        open(&nested, "state.db").unwrap();
        assert!(open_existing(&nested, "state.db", Access::ReadOnly).is_ok());
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(open_existing(&nested, "state.db", Access::ReadOnly).is_err());
    }

    #[test]
    fn existing_open_rejects_inode_swap_between_check_and_open() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let path = home.path().join("state.db");
        open(home.path(), "state.db").unwrap().execute_batch("CREATE TABLE current(value)").unwrap();
        // An older, otherwise valid copy (same owner, 0600, single link).
        let older = home.path().join("older.db");
        open(home.path(), "older.db").unwrap().execute_batch("CREATE TABLE stale(value)").unwrap();
        for access in [Access::ReadOnly, Access::ReadWrite] {
            let result = open_existing_with(home.path(), "state.db", access, || {
                std::fs::rename(&path, home.path().join("current.db")).unwrap();
                std::fs::rename(&older, &path).unwrap();
            });
            assert!(result.is_err(), "{access:?}");
            // Restore for the next round.
            std::fs::rename(&path, &older).unwrap();
            std::fs::rename(home.path().join("current.db"), &path).unwrap();
        }
        assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_ok());
    }

    #[test]
    fn existing_open_rejects_links_modes_and_sidecars() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let path = home.path().join("state.db");
        open(home.path(), "state.db").unwrap().execute_batch("CREATE TABLE example(value)").unwrap();
        for mode in [0o620, 0o602, 0o666] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_err(), "{mode:o}");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_ok(), "read bits alone are fine");
        std::fs::hard_link(&path, home.path().join("alias")).unwrap();
        assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_err());
        std::fs::remove_file(home.path().join("alias")).unwrap();
        let outside = home.path().join("outside");
        std::fs::write(&outside, b"untouched").unwrap();
        for suffix in ["-journal", "-wal", "-shm"] {
            let sidecar = home.path().join(format!("state.db{suffix}"));
            symlink(&outside, &sidecar).unwrap();
            assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_err(), "{suffix}");
            std::fs::remove_file(&sidecar).unwrap();
            std::fs::write(&sidecar, b"").unwrap();
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_err(), "{suffix} mode");
            std::fs::remove_file(&sidecar).unwrap();
        }
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
        std::fs::rename(&path, home.path().join("real.db")).unwrap();
        symlink(home.path().join("real.db"), &path).unwrap();
        assert!(open_existing(home.path(), "state.db", Access::ReadOnly).is_err());
    }
}

#[cfg(all(test, not(any(unix, windows))))]
#[test]
fn private_sqlite_fails_closed_without_creating_database() {
    let home = crate::fs_safe::private_test_home().unwrap();
    assert!(open(home.path(), "state.db").is_err());
    assert!(!home.path().join("state.db").exists());
}
