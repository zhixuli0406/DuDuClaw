//! Hardened open of a private SQLite file in the DuDuClaw home, for the
//! stores that `duduclaw-core` readers and the gateway share (RM-134:
//! `mcp_keys.db`, `mcp_keys_usage.db`).
//!
//! The rules follow the gateway's `fs_safe::sqlite` (users.db,
//! migration-confirmations.db) at file level; that helper's descriptor walk of
//! every ancestor depends on the gateway-only `SafeDir`, so it is not used
//! here. On Unix:
//!
//! - the home is canonicalised once (SQLite then opens with
//!   `SQLITE_OPEN_NOFOLLOW`, which refuses a symlink anywhere in the path);
//! - the home itself must be a directory owned by the effective uid and not
//!   group/other writable (only this uid can then replace a file in it);
//! - the database and any present `-journal` / `-wal` / `-shm` side file must
//!   be a regular, single-link file owned by the effective uid with no
//!   group/other permission bits at all (`mode & 0o077 == 0`);
//! - a missing database is created only by [`Access::Create`], exclusively
//!   (`O_CREAT|O_EXCL|O_NOFOLLOW`, mode `0600`), never by SQLite (SQLite
//!   never gets `SQLITE_OPEN_CREATE`); SQLite creates its side files with the
//!   database's own mode, so they are `0600` too;
//! - no extra descriptor is ever opened (and closed) on an existing database:
//!   POSIX `close()` drops every record lock this process holds on the inode,
//!   including another live connection's. Instead the directory and file are
//!   `lstat`ed before the open and again after it, and their dev/inode must be
//!   identical (the residual race needs write access to the private home,
//!   i.e. this uid).
//!
//! Windows: only "regular file, not a link" is checked, plus
//! `SQLITE_OPEN_NOFOLLOW`. Owner and ACL are not inspected: the store relies
//! on the home directory's ACL, as `fs_safe::sqlite` does by placing its
//! databases below a protected directory. This is a documented limitation.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// How to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Existing file only, `SQLITE_OPEN_READ_ONLY`. A missing file is `Ok(None)`.
    ReadOnly,
    /// Existing file only, read-write. A missing file is `Ok(None)`.
    ReadWrite,
    /// Create (exclusively, `0600`) when missing, then read-write.
    Create,
}

/// Why the open was refused. Never carries file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenError(pub String);

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn refused(what: impl Into<String>) -> OpenError {
    OpenError(what.into())
}

/// Open `<home>/<name>` under the rules in the module docs. `Ok(None)` only
/// for [`Access::ReadOnly`] / [`Access::ReadWrite`] when the file does not
/// exist (and is not a dangling link: a link of any kind is an error).
pub fn open(home: &Path, name: &str, access: Access) -> Result<Option<Connection>, OpenError> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err(refused("invalid database name"));
    }
    let home = match std::fs::canonicalize(home) {
        Ok(home) => home,
        // No home yet means no database yet (never created by a reader).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && access != Access::Create => return Ok(None),
        Err(e) => return Err(refused(format!("resolve home: {e}"))),
    };
    let path = home.join(name);
    let dir_before = check_dir(&home)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match access {
            Access::ReadOnly | Access::ReadWrite => return Ok(None),
            Access::Create => create_private(&path)?,
        },
        Err(e) => return Err(refused(format!("stat {name}: {e}"))),
    }
    let file_before = check_file(&path)?;
    check_side_files(&path)?;
    let flags = match access {
        Access::ReadOnly => OpenFlags::SQLITE_OPEN_READ_ONLY,
        Access::ReadWrite | Access::Create => OpenFlags::SQLITE_OPEN_READ_WRITE,
    } | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(&path, flags).map_err(|e| refused(format!("open {name}: {e}")))?;
    // Same directory and file after the open as before it.
    let dir_after = check_dir(&home)?;
    let file_after = check_file(&path)?;
    if dir_before != dir_after || file_before != file_after {
        return Err(refused(format!("{name} was replaced while it was opened")));
    }
    check_side_files(&path)?;
    Ok(Some(conn))
}

/// `(dev, inode)` on Unix; nothing to compare elsewhere.
type Identity = (u64, u64);

#[cfg(unix)]
fn euid() -> u32 {
    // SAFETY: geteuid has no arguments or preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn check_dir(home: &Path) -> Result<Identity, OpenError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(home).map_err(|e| refused(format!("stat home: {e}")))?;
    if !meta.is_dir() || meta.uid() != euid() || meta.mode() & 0o022 != 0 {
        return Err(refused(format!(
            "home directory must be owned by this user and not group/other writable (mode {:04o})",
            meta.mode() & 0o7777
        )));
    }
    Ok((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn check_dir(home: &Path) -> Result<Identity, OpenError> {
    let meta = std::fs::symlink_metadata(home).map_err(|e| refused(format!("stat home: {e}")))?;
    if !meta.is_dir() {
        return Err(refused("home is not a directory"));
    }
    Ok((0, 0))
}

#[cfg(unix)]
fn check_file(path: &Path) -> Result<Identity, OpenError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|e| refused(format!("stat database: {e}")))?;
    if !meta.file_type().is_file() || meta.nlink() != 1 || meta.uid() != euid() || meta.mode() & 0o077 != 0 {
        return Err(refused(format!(
            "database must be a single-link regular file owned by this user, without group/other access (mode {:04o})",
            meta.mode() & 0o7777
        )));
    }
    Ok((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn check_file(path: &Path) -> Result<Identity, OpenError> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| refused(format!("stat database: {e}")))?;
    if !meta.file_type().is_file() {
        return Err(refused("database is not a regular file"));
    }
    Ok((0, 0))
}

/// Present side files must pass the same rule as the database.
fn check_side_files(path: &Path) -> Result<(), OpenError> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", path.display()));
        match std::fs::symlink_metadata(&side) {
            Ok(_) => {
                check_file(&side).map_err(|_| refused(format!("unsafe database side file ({suffix})")))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(refused(format!("stat side file: {e}"))),
        }
    }
    Ok(())
}

/// Exclusive, no-follow, owner-only creation. A concurrent creator winning
/// the race is fine (the file is then checked like any existing one).
fn create_private(path: &Path) -> Result<(), OpenError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(refused(format!("create database: {e}"))),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn home() -> tempfile::TempDir {
        tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o700)).tempdir().unwrap()
    }

    #[test]
    fn create_is_owner_only_and_read_only_cannot_write() {
        let home = home();
        assert!(open(home.path(), "x.db", Access::ReadOnly).unwrap().is_none());
        assert!(open(home.path(), "x.db", Access::ReadWrite).unwrap().is_none());
        assert!(!home.path().join("x.db").exists(), "never created by a reader");
        let conn = open(home.path(), "x.db", Access::Create).unwrap().unwrap();
        conn.execute_batch("CREATE TABLE t(v); INSERT INTO t VALUES (1);").unwrap();
        drop(conn);
        let mode = std::fs::metadata(home.path().join("x.db")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let ro = open(home.path(), "x.db", Access::ReadOnly).unwrap().unwrap();
        assert!(ro.execute("INSERT INTO t VALUES (2)", []).is_err(), "read-only connection");
        assert_eq!(ro.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn insecure_mode_link_hardlink_and_parent_are_refused() {
        let home = home();
        open(home.path(), "x.db", Access::Create).unwrap();
        let path = home.path().join("x.db");
        for mode in [0o640, 0o604, 0o660, 0o666] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(open(home.path(), "x.db", Access::ReadOnly).is_err(), "{mode:o}");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&path, home.path().join("alias")).unwrap();
        assert!(open(home.path(), "x.db", Access::ReadOnly).is_err(), "hard link");
        std::fs::remove_file(home.path().join("alias")).unwrap();
        // A symlinked database, even to a valid private file.
        std::fs::rename(&path, home.path().join("real.db")).unwrap();
        std::os::unix::fs::symlink(home.path().join("real.db"), &path).unwrap();
        assert!(open(home.path(), "x.db", Access::ReadOnly).is_err(), "symlink");
        assert!(open(home.path(), "x.db", Access::Create).is_err(), "never created through a link");
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(home.path().join("real.db"), &path).unwrap();
        // Side files follow the same rule.
        let side = home.path().join("x.db-journal");
        std::fs::write(&side, b"").unwrap();
        std::fs::set_permissions(&side, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open(home.path(), "x.db", Access::ReadOnly).is_err(), "readable side file");
        std::fs::remove_file(&side).unwrap();
        assert!(open(home.path(), "x.db", Access::ReadOnly).unwrap().is_some());
        // A group-writable home.
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(open(home.path(), "x.db", Access::ReadOnly).is_err(), "group-writable home");
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
