//! F6m (S8): the live employee-lifecycle read behind agent token v2.
//!
//! A **protected employee** is a label with a row in the gateway's confirmed
//! employee index, `<home>/migration-authority/apply.db` table
//! `confirmed_employees`. That row is written only by the Admin-approved
//! employee apply path (`duduclaw-gateway` `handlers/migration_authority_employee.rs`
//! `save_confirmed_receipt`) and disabled only by the tombstone
//! (`isolation_lifecycle.rs`). Any row (live or tombstoned) makes the label
//! protected; ordinary installs have no `apply.db` at all, so their labels
//! are [`ProtectedEmployee::Unprotected`] and keep v1 tokens unchanged.
//!
//! This is the token binding source only: the employee id and incarnation the
//! token must carry, and whether the row is tombstoned. Every protected
//! authority still runs the gateway's full `isolation_lifecycle::resolve`
//! (root ReadBack, proposal, users.db) at its own use.
//!
//! **Incarnation.** F6a/F6b add `confirmed_employees.incarnation INTEGER NOT
//! NULL DEFAULT 0` (design §3.1, only restore writes it, by +1). Until that
//! column exists every row reads as incarnation 0, the value F6a gives every
//! legacy row; once it exists a NULL or negative value is a read failure.
//!
//! Fail closed: an `apply.db` (or its directory) that exists but is a symlink,
//! a reparse point, not a regular file, foreign-owned, hard-linked, group/other
//! writable, unopenable, replaced during the open, or holds a corrupt row is
//! [`LifecycleUnavailable`], and the caller refuses every token for that label.
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// `<home>/migration-authority`, the gateway's private authority directory.
pub const APPLY_DB_DIR: &str = "migration-authority";
/// `<home>/migration-authority/apply.db`.
pub const APPLY_DB_FILE: &str = "apply.db";

/// The tuple a v2 token, a v2 delegation context and (through the token) an
/// internal request signature are bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentIncarnation {
    /// Immutable root registry identity (`EmployeeId`), never reused.
    pub employee: [u8; 16],
    /// Bumped by one on every same-id restore (F6c); 0 for a new id.
    pub incarnation: u64,
}

impl AgentIncarnation {
    /// Lowercase hex of the employee id (32 characters).
    pub fn employee_hex(&self) -> String {
        super::hex_lower(&self.employee)
    }
}

/// What the confirmed employee index says about one label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedEmployee {
    /// No confirmed row (or no index at all): v1 behaviour, unchanged.
    Unprotected,
    /// A live confirmed row: only a v2 token for exactly this tuple verifies.
    Live(AgentIncarnation),
    /// The row is disabled (tombstoned): no token verifies, none is minted.
    Tombstoned,
}

/// The index exists but could not be read safely. Never "unprotected".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleUnavailable;

impl std::fmt::Display for LifecycleUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("protected employee lifecycle unavailable")
    }
}

impl std::error::Error for LifecycleUnavailable {}

/// The fields of the root receipt copy this read needs; the full `Receipt`
/// lives in `duduclaw-migration`, which this crate does not depend on.
#[derive(serde::Deserialize)]
struct ReceiptView {
    label: String,
    employee: duduclaw_isolation::ids::EmployeeId,
    disabled: bool,
}

/// Read the confirmed employee index for `label`. See the module docs.
pub fn protected_employee(home: &Path, label: &str) -> Result<ProtectedEmployee, LifecycleUnavailable> {
    if label.is_empty() {
        return Ok(ProtectedEmployee::Unprotected);
    }
    let directory = home.join(APPLY_DB_DIR);
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ProtectedEmployee::Unprotected),
        Err(_) => return Err(LifecycleUnavailable),
        Ok(metadata) if !metadata.is_dir() || is_reparse_point(&metadata) => return Err(LifecycleUnavailable),
        Ok(_) => {}
    }
    let path = directory.join(APPLY_DB_FILE);
    let before = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ProtectedEmployee::Unprotected),
        Err(_) => return Err(LifecycleUnavailable),
        Ok(metadata) => metadata,
    };
    check_store(&before)?;
    let connection = open_read_only(&path)?;
    // SQLite owns its descriptor; re-check the path identity right after open.
    let after = std::fs::symlink_metadata(&path).map_err(|_| LifecycleUnavailable)?;
    check_store(&after)?;
    same_file(&before, &after)?;
    read_row(&connection, label)
}

/// SR5 round 3 N3: whether the confirmed employee index of `home` holds any
/// row (live or tombstoned) — the host-owned durable fact behind
/// [`crate::protected_mode`]. No directory / no store / no table: `Ok(false)`.
/// A store that exists but cannot be read safely (foreign-owned for this
/// uid, symlinked, unreadable, …): `Err` (the latch treats it as protected).
pub fn any_confirmed_employee(home: &Path) -> Result<bool, LifecycleUnavailable> {
    let directory = home.join(APPLY_DB_DIR);
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(LifecycleUnavailable),
        Ok(metadata) if !metadata.is_dir() || is_reparse_point(&metadata) => return Err(LifecycleUnavailable),
        Ok(_) => {}
    }
    let path = directory.join(APPLY_DB_FILE);
    let before = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(LifecycleUnavailable),
        Ok(metadata) => metadata,
    };
    check_store(&before)?;
    let connection = open_read_only(&path)?;
    let after = std::fs::symlink_metadata(&path).map_err(|_| LifecycleUnavailable)?;
    check_store(&after)?;
    same_file(&before, &after)?;
    let table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='confirmed_employees')",
        [], |r| r.get(0)).map_err(|_| LifecycleUnavailable)?;
    if !table {
        return Ok(false);
    }
    connection.query_row("SELECT EXISTS(SELECT 1 FROM confirmed_employees)", [], |r| r.get(0))
        .map_err(|_| LifecycleUnavailable)
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT (symlinks and junctions).
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn check_store(metadata: &std::fs::Metadata) -> Result<(), LifecycleUnavailable> {
    if !metadata.file_type().is_file() || is_reparse_point(metadata) {
        return Err(LifecycleUnavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Same policy as the gateway's hardened opener for the store itself.
        if metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o022 != 0
        {
            return Err(LifecycleUnavailable);
        }
    }
    Ok(())
}

fn same_file(before: &std::fs::Metadata, after: &std::fs::Metadata) -> Result<(), LifecycleUnavailable> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(LifecycleUnavailable);
        }
    }
    #[cfg(not(unix))]
    let _ = (before, after);
    Ok(())
}

fn open_read_only(path: &Path) -> Result<Connection, LifecycleUnavailable> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    #[cfg(unix)]
    let flags = flags | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(path, flags).map_err(|_| LifecycleUnavailable)?;
    connection.busy_timeout(std::time::Duration::from_secs(2)).map_err(|_| LifecycleUnavailable)?;
    Ok(connection)
}

fn read_row(db: &Connection, label: &str) -> Result<ProtectedEmployee, LifecycleUnavailable> {
    let unavailable = |_| LifecycleUnavailable;
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='confirmed_employees')",
        [], |r| r.get(0)).map_err(unavailable)?;
    if !table {
        return Ok(ProtectedEmployee::Unprotected);
    }
    // F6a introduces the column; before it exists every row is incarnation 0.
    let has_incarnation: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('confirmed_employees') WHERE name='incarnation')",
        [], |r| r.get(0)).map_err(unavailable)?;
    let sql = if has_incarnation {
        "SELECT receipt,incarnation FROM confirmed_employees WHERE label=?1"
    } else {
        "SELECT receipt,0 FROM confirmed_employees WHERE label=?1"
    };
    // A NULL or non-integer incarnation fails the typed read (fail closed).
    let row: Option<(String, i64)> = db.query_row(sql, [label], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional().map_err(unavailable)?;
    let Some((encoded, incarnation)) = row else {
        return Ok(ProtectedEmployee::Unprotected);
    };
    let receipt: ReceiptView = serde_json::from_str(&encoded).map_err(|_| LifecycleUnavailable)?;
    if receipt.label != label {
        return Err(LifecycleUnavailable);
    }
    let incarnation = u64::try_from(incarnation).map_err(|_| LifecycleUnavailable)?;
    if receipt.disabled {
        return Ok(ProtectedEmployee::Tombstoned);
    }
    Ok(ProtectedEmployee::Live(AgentIncarnation { employee: receipt.employee.0, incarnation }))
}
