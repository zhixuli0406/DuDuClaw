//! Read-only verification of the root migration state for the installer's
//! composite readiness check (v1.83 R4, plan §4.4 #9).
//!
//! Never takes the migration lock, never creates, writes or upgrades a file
//! (unlike [`super::RootMigration::open`]). The state directory, the plan and
//! the journal are opened by descriptor without following links; the journal
//! is checked by `fstat` (regular file, owner, mode 0600, one link, at most
//! [`MAX_JOURNAL_BYTES`]) and refused while any `-wal`, `-shm` or `-journal`
//! sidecar exists. SQLite then opens that same descriptor through
//! `/proc/self/fd` as `mode=ro&immutable=1`, with `query_only`,
//! `trusted_schema=OFF`, length/SQL/VDBE/attach limits and a 2 s progress
//! deadline; the exact schema is verified before any row is read, every value
//! is length-bounded, and the file must be unchanged afterwards.
//!
//! A reported state is complete evidence: no pending intent, and exactly the
//! receipts of every stage up to that state, each validating for its stage
//! with a matching digest. Any other journal is an error with its own code.
use std::{io::Read, path::Path, time::{Duration, Instant}};
use duduclaw_isolation::{ids::RequestId, migration::MigrationState};
use rusqlite::{Connection, OpenFlags, OptionalExtension, limits::Limit};
use rustix::{fd::{AsRawFd, OwnedFd}, fs::{self as rfs, AtFlags, FileType, Mode, OFlags}, io::Errno};
use sha2::{Digest, Sha256};
use crate::journal::Evidence;
use super::{RootPlan, next_stage};

/// Upper bound of the journal file (bytes).
pub const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PLAN_BYTES: u64 = 16 * 1024;
/// Directory entries examined when deciding whether migration artefacts exist.
const MAX_DIRECTORY_ENTRIES: usize = 4096;
/// Longest text value read from the journal (id, state, intent, receipt).
const MAX_VALUE_BYTES: usize = 4096;
const MAX_RECEIPTS: usize = 16;
const QUERY_DEADLINE: Duration = Duration::from_secs(2);
const JOURNAL: &str = "journal.db";
const SIDECARS: [&str; 3] = ["journal.db-wal", "journal.db-shm", "journal.db-journal"];

/// Why the migration state cannot be reported. Every variant blocks
/// readiness; [`ReadinessError::code`] is a stable ASCII reason code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadinessError {
    #[error("migration state unavailable")] Unavailable,
    #[error("migration state directory rejected")] StateDirUnsafe,
    /// Migration artefacts exist in the state directory but `plan.json` does not.
    #[error("migration plan missing")] PlanMissing,
    #[error("migration plan rejected")] PlanInvalid,
    #[error("migration journal missing")] JournalMissing,
    /// Not a regular root-owned 0600 file with exactly one link.
    #[error("migration journal rejected")] JournalUnsafe,
    #[error("migration journal too large")] JournalOversized,
    /// A `-wal`, `-shm` or hot `-journal` file exists next to the journal.
    #[error("migration journal has a sidecar file")] JournalSidecar,
    #[error("migration journal changed while read")] JournalChanged,
    #[error("migration journal schema rejected")] JournalSchema,
    #[error("migration journal unreadable")] JournalUnreadable,
    /// The journal belongs to another migration than the plan.
    #[error("migration journal conflict")] JournalConflict,
    /// A stage intent is recorded: an external effect may be half done.
    #[error("migration stage in progress")] IntentPending,
    /// A required stage receipt is missing, invalid or unexpected.
    #[error("migration receipts rejected")] ReceiptsInvalid,
}

impl ReadinessError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "migration_state_unreadable",
            Self::StateDirUnsafe => "migration_state_dir_unsafe",
            Self::PlanMissing => "migration_plan_missing",
            Self::PlanInvalid => "migration_plan_invalid",
            Self::JournalMissing => "migration_journal_missing",
            Self::JournalUnsafe => "migration_journal_unsafe",
            Self::JournalOversized => "migration_journal_oversized",
            Self::JournalSidecar => "migration_journal_sidecar",
            Self::JournalChanged => "migration_journal_changed",
            Self::JournalSchema => "migration_journal_schema",
            Self::JournalUnreadable => "migration_journal_unreadable",
            Self::JournalConflict => "migration_journal_conflict",
            Self::IntentPending => "migration_intent_pending",
            Self::ReceiptsInvalid => "migration_receipts_invalid",
        }
    }
}

/// The fixed installation the plan must name.
#[derive(Debug, Clone, Copy)]
pub struct ExpectedLayout<'a> {
    pub state_dir: &'a Path,
    pub legacy_home: &'a Path,
    pub root_key_path: &'a Path,
}

/// Root only. `Ok(None)`: no migration (no state directory, or an empty one).
/// `Ok(Some(state))`: the verified journal state (see the module docs). The
/// caller decides what each state means for readiness.
pub fn verify_read_only(layout: &ExpectedLayout<'_>) -> Result<Option<MigrationState>, ReadinessError> {
    super::root().map_err(|_| ReadinessError::Unavailable)?;
    super::trusted_parents(layout.state_dir).map_err(|_| ReadinessError::StateDirUnsafe)?;
    verify_owned_by(layout, 0)
}

/// [`verify_read_only`] with the expected owner of the state directory, plan
/// and journal (root in production; the test account in unit tests).
fn verify_owned_by(layout: &ExpectedLayout<'_>, owner: u32) -> Result<Option<MigrationState>, ReadinessError> {
    use ReadinessError as E;
    let dir = match rfs::open(layout.state_dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty()) {
        Ok(dir) => dir,
        Err(error) if error == Errno::NOENT => return Ok(None),
        Err(_) => return Err(E::StateDirUnsafe),
    };
    let stat = rfs::fstat(&dir).map_err(|_| E::StateDirUnsafe)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory || stat.st_uid != owner
        || stat.st_mode & 0o7777 != 0o700 { return Err(E::StateDirUnsafe); }
    let plan = match open_file(&dir, "plan.json") {
        Ok(plan) => plan,
        // The journal or any other artefact without its plan: never "no migration".
        Err(error) if error == Errno::NOENT => return if has_entries(&dir)? { Err(E::PlanMissing) } else { Ok(None) },
        Err(_) => return Err(E::PlanInvalid),
    };
    let plan = read_plan(plan, owner)?;
    if plan.state_dir != layout.state_dir || plan.legacy_home != layout.legacy_home
        || plan.root_key_path != layout.root_key_path { return Err(E::PlanInvalid); }
    no_sidecars(&dir)?;
    let journal = match open_file(&dir, JOURNAL) {
        Ok(journal) => journal,
        Err(error) if error == Errno::NOENT => return Err(E::JournalMissing),
        Err(_) => return Err(E::JournalUnsafe),
    };
    let before = journal_stat(&journal, owner)?;
    let state = read_journal(&journal, plan.migration)?;
    no_sidecars(&dir)?;
    if journal_stat(&journal, owner)? != before { return Err(E::JournalChanged); }
    Ok(Some(state))
}

fn open_file(dir: &OwnedFd, name: &str) -> rustix::io::Result<OwnedFd> {
    rfs::openat(dir, name, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK, Mode::empty())
}

fn has_entries(dir: &OwnedFd) -> Result<bool, ReadinessError> {
    let mut entries = rfs::Dir::read_from(dir).map_err(|_| ReadinessError::StateDirUnsafe)?;
    for _ in 0..MAX_DIRECTORY_ENTRIES {
        let Some(entry) = entries.read() else { return Ok(false) };
        let entry = entry.map_err(|_| ReadinessError::StateDirUnsafe)?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") { return Ok(true); }
    }
    // Fail closed: a listing longer than the bound is never "empty".
    Ok(true)
}

fn no_sidecars(dir: &OwnedFd) -> Result<(), ReadinessError> {
    for name in SIDECARS {
        match rfs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(error) if error == Errno::NOENT => {}
            Ok(_) => return Err(ReadinessError::JournalSidecar),
            Err(_) => return Err(ReadinessError::JournalUnreadable),
        }
    }
    Ok(())
}

#[allow(clippy::useless_conversion)]
fn read_plan(fd: OwnedFd, owner: u32) -> Result<RootPlan, ReadinessError> {
    let stat = rfs::fstat(&fd).map_err(|_| ReadinessError::PlanInvalid)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_uid != owner
        || u64::from(stat.st_nlink) != 1 || stat.st_mode & 0o7777 != 0o600
        || u64::try_from(stat.st_size).map_or(true, |size| size > MAX_PLAN_BYTES) {
        return Err(ReadinessError::PlanInvalid);
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd).take(MAX_PLAN_BYTES + 1).read_to_end(&mut bytes).map_err(|_| ReadinessError::PlanInvalid)?;
    if bytes.len() as u64 > MAX_PLAN_BYTES { return Err(ReadinessError::PlanInvalid); }
    let plan: RootPlan = serde_json::from_slice(&bytes).map_err(|_| ReadinessError::PlanInvalid)?;
    plan.validate().map_err(|_| ReadinessError::PlanInvalid)?;
    Ok(plan)
}

/// Identity and change fingerprint of an acceptable journal file.
type Fingerprint = (u64, u64, i64, i64, i64, i64, i64);

// `stat` field widths differ per architecture; every value fits i64/u64.
#[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
fn journal_stat(fd: &OwnedFd, owner: u32) -> Result<Fingerprint, ReadinessError> {
    let stat = rfs::fstat(fd).map_err(|_| ReadinessError::JournalUnreadable)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_uid != owner
        || u64::from(stat.st_nlink) != 1 || stat.st_mode & 0o7777 != 0o600 {
        return Err(ReadinessError::JournalUnsafe);
    }
    let size = u64::try_from(stat.st_size).map_err(|_| ReadinessError::JournalUnsafe)?;
    if size > MAX_JOURNAL_BYTES { return Err(ReadinessError::JournalOversized); }
    Ok((stat.st_dev as u64, stat.st_ino as u64, stat.st_size as i64, stat.st_mtime as i64,
        stat.st_mtime_nsec as i64, stat.st_ctime as i64, stat.st_ctime_nsec as i64))
}

fn unreadable(_: rusqlite::Error) -> ReadinessError { ReadinessError::JournalUnreadable }
fn schema(_: rusqlite::Error) -> ReadinessError { ReadinessError::JournalSchema }

/// The journal of `migration`: open the verified descriptor immutable and
/// read-only, verify the schema, then the singleton row and every receipt.
fn read_journal(fd: &OwnedFd, migration: RequestId) -> Result<MigrationState, ReadinessError> {
    let uri = format!("file:/proc/self/fd/{}?mode=ro&immutable=1", fd.as_raw_fd());
    let conn = Connection::open_with_flags(&uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(unreadable)?;
    harden(&conn)?;
    verify_schema(&conn)?;
    journal_state(&conn, migration)
}

fn harden(conn: &Connection) -> Result<(), ReadinessError> {
    for (limit, value) in [(Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES as i32), (Limit::SQLITE_LIMIT_SQL_LENGTH, 4096),
        (Limit::SQLITE_LIMIT_VDBE_OP, 200_000), (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 32), (Limit::SQLITE_LIMIT_COLUMN, 16)] {
        conn.set_limit(limit, value).map_err(unreadable)?;
    }
    conn.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;").map_err(unreadable)?;
    let query_only: i64 = conn.query_row("PRAGMA query_only", [], |row| row.get(0)).map_err(unreadable)?;
    let trusted_schema: i64 = conn.query_row("PRAGMA trusted_schema", [], |row| row.get(0)).map_err(unreadable)?;
    if (query_only, trusted_schema) != (1, 0) || !conn.is_readonly("main").map_err(unreadable)? {
        return Err(ReadinessError::JournalUnreadable);
    }
    let deadline = Instant::now() + QUERY_DEADLINE;
    conn.progress_handler(1000, Some(move || Instant::now() >= deadline)).map_err(unreadable)?;
    Ok(())
}

/// `sql` with every whitespace run collapsed to one space.
fn normalized(sql: &str) -> String { sql.split_whitespace().collect::<Vec<_>>().join(" ") }

const MIGRATION_SQL: &str =
    "CREATE TABLE migration ( singleton INTEGER PRIMARY KEY CHECK(singleton=1), id TEXT NOT NULL, state TEXT NOT NULL, intent TEXT)";
const RECEIPTS_SQL: &str =
    "CREATE TABLE migration_receipts ( target TEXT PRIMARY KEY, evidence TEXT NOT NULL, digest BLOB NOT NULL)";
/// `pragma_table_info`: (name, declared type, notnull, pk) in column order.
const MIGRATION_COLUMNS: [(&str, &str, i64, i64); 4] =
    [("singleton", "INTEGER", 0, 1), ("id", "TEXT", 1, 0), ("state", "TEXT", 1, 0), ("intent", "TEXT", 0, 0)];
const RECEIPT_COLUMNS: [(&str, &str, i64, i64); 3] =
    [("target", "TEXT", 0, 1), ("evidence", "TEXT", 1, 0), ("digest", "BLOB", 1, 0)];

/// Exactly the two tables `Journal::from_connection` creates (and the
/// receipts table's primary-key index): no other table, index, view or trigger.
fn verify_schema(conn: &Connection) -> Result<(), ReadinessError> {
    let mut statement = conn.prepare("SELECT type, name, tbl_name, sql FROM main.sqlite_schema ORDER BY type, name")
        .map_err(unreadable)?;
    let mut rows = statement.query([]).map_err(unreadable)?;
    let mut objects = Vec::new();
    while let Some(row) = rows.next().map_err(unreadable)? {
        if objects.len() == 4 { return Err(ReadinessError::JournalSchema); }
        let object: (String, String, String, Option<String>) =
            (row.get(0).map_err(schema)?, row.get(1).map_err(schema)?, row.get(2).map_err(schema)?, row.get(3).map_err(schema)?);
        objects.push((object.0, object.1, object.2, object.3.as_deref().map(normalized)));
    }
    let expected = [
        ("index", "sqlite_autoindex_migration_receipts_1", "migration_receipts", None),
        ("table", "migration", "migration", Some(MIGRATION_SQL)),
        ("table", "migration_receipts", "migration_receipts", Some(RECEIPTS_SQL)),
    ];
    if objects.len() != expected.len() || objects.iter().zip(expected).any(|(found, (kind, name, table, sql))|
        found.0 != kind || found.1 != name || found.2 != table || found.3.as_deref() != sql) {
        return Err(ReadinessError::JournalSchema);
    }
    for (table, columns) in [("migration", &MIGRATION_COLUMNS[..]), ("migration_receipts", &RECEIPT_COLUMNS[..])] {
        let mut statement = conn.prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid")
            .map_err(unreadable)?;
        let found = statement.query_map([table], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))).map_err(schema)?
            .take(columns.len() + 1).collect::<Result<Vec<_>, _>>().map_err(schema)?;
        if found.len() != columns.len() || found.iter().zip(columns).any(|(found, (name, kind, not_null, pk))|
            found.0 != *name || found.1 != *kind || found.2 != *not_null || found.3 != *pk) {
            return Err(ReadinessError::JournalSchema);
        }
    }
    Ok(())
}

fn bounded(value: String) -> Result<String, ReadinessError> {
    if value.is_empty() || value.len() > MAX_VALUE_BYTES { return Err(ReadinessError::JournalUnreadable); }
    Ok(value)
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, ReadinessError> {
    serde_json::to_string(value).map_err(|_| ReadinessError::Unavailable)
}

fn journal_state(conn: &Connection, migration: RequestId) -> Result<MigrationState, ReadinessError> {
    use ReadinessError as E;
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM migration", [], |row| row.get(0)).map_err(unreadable)?;
    if rows != 1 { return Err(E::JournalUnreadable); }
    let (id, state, intent): (String, String, Option<String>) = conn.query_row(
        "SELECT id, state, intent FROM migration WHERE singleton=1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(unreadable)?;
    if bounded(id)? != encode(&migration)? { return Err(E::JournalConflict); }
    let state_text = bounded(state)?;
    let state: MigrationState = serde_json::from_str(&state_text).map_err(|_| E::JournalUnreadable)?;
    if encode(&state)? != state_text { return Err(E::JournalUnreadable); }
    // A recorded intent means an effect may be half done; only the locked
    // runner may reconcile it.
    if intent.is_some() { return Err(E::IntentPending); }
    verify_receipts(conn, state)?;
    Ok(state)
}

/// Exactly one valid receipt per stage from the first one up to `state`.
fn verify_receipts(conn: &Connection, state: MigrationState) -> Result<(), ReadinessError> {
    use ReadinessError as E;
    let mut expected = Vec::new();
    let mut current = MigrationState::Planned;
    while current != state {
        current = next_stage(current).ok_or(E::JournalUnreadable)?;
        expected.push(current);
    }
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM migration_receipts", [], |row| row.get(0)).map_err(unreadable)?;
    if usize::try_from(count).map_or(true, |count| count != expected.len() || count > MAX_RECEIPTS) {
        return Err(E::ReceiptsInvalid);
    }
    for target in expected {
        let receipt: Option<(String, Vec<u8>)> = conn.query_row(
            "SELECT evidence, digest FROM migration_receipts WHERE target=?1", [encode(&target)?],
            |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(unreadable)?;
        let (evidence, digest) = receipt.ok_or(E::ReceiptsInvalid)?;
        let evidence = bounded(evidence)?;
        if digest.as_slice() != Sha256::digest(evidence.as_bytes()).as_slice() { return Err(E::ReceiptsInvalid); }
        serde_json::from_str::<Evidence>(&evidence).map_err(|_| E::ReceiptsInvalid)?
            .validate(target).map_err(|_| E::ReceiptsInvalid)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "root_readiness_tests.rs"]
mod tests;
