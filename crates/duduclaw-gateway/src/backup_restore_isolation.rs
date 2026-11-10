//! L3: does a whole-home backup carry isolation state?
//!
//! On a protected single-binary host (protected mode on, isolation not
//! installed) a whole-home restore is allowed as in v1.80, but NOT of an
//! archive taken on an isolation host: it would bring isolation records onto a
//! host whose gates no longer enforce them (the Personal / lifecycle rules
//! follow `runtime_readiness::isolation_active`). The dashboard restore
//! inspects the archive after the safe extraction into the request's private
//! temp directory and before anything is published, and again at the boot
//! swap. `duduclaw import` (duduclaw-cli `portability`) runs the same
//! [`inspect`] on a private extraction of the entries [`entry_kind`] selects,
//! before anything is moved into the home.
//!
//! Isolation state is any of:
//! - any top-level entry named `isolation-*` or `isolation_*` (ASCII case
//!   insensitive), of any type: `isolation-quarantine/`, the isolation stores
//!   (`isolation-connectors.db`, `isolation-forks.db`,
//!   `isolation-execution-jobs.db`, `isolation-ephemeral-jobs.db`,
//!   `isolation_background.db`) and their sidecars. None of these is created
//!   on a single-binary host: every opener is behind the isolation gate
//!   (`execution_gate::isolation_required` / `runtime_readiness::isolation_active`)
//!   or the isolation boot, which needs the spawnd backend;
//! - `migration-authority/apply.db` with a row in any table (confirmed
//!   employees, quarantine rows and label holds, root allocations, Personal
//!   employee requests, the employee schema version);
//! - `migration-confirmations.db` with a row in any table (migration
//!   proposals, applies, confirmation audit, rotations);
//! - a row in the `users.db` Personal ownership mirror
//!   (`isolation_personal_agents`) or its removal grants
//!   (`isolation_personal_agent_removals`). `users.db` creates both tables
//!   on every host, so only rows count.
//!
//! Anything that cannot be ruled out is [`ArchiveIsolation::Unknown`] (the
//! caller refuses): a stat or directory-read error other than NotFound, a
//! non-file database, a WAL / rollback journal without its database, a file
//! SQLite cannot read, an unreadable schema or table.
//!
//! Side effects: databases are opened `SQLITE_OPEN_READ_ONLY` (never
//! `immutable=1`, so a WAL is honoured), with `trusted_schema=OFF`,
//! `SQLITE_DBCONFIG_DEFENSIVE` and `query_only`. The database file itself is
//! never written, but for a WAL-mode database SQLite may create the `-wal`
//! and `-shm` sidecars next to it when they are missing (the directory is
//! writable). That only happens inside the extraction directory being
//! inspected (the restore's private temp / staging tree, or the CLI import's
//! private inspection directory), never in the live home; on the dashboard
//! path an empty sidecar created there is published with its database by the
//! swap, which SQLite treats as no pending WAL content.
use rusqlite::{Connection, OpenFlags, config::DbConfig};
use std::path::{Component, Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveIsolation {
    Absent,
    Present,
    Unknown,
}

/// What [`inspect`] needs from one archive entry (path relative to the
/// restore root), for callers that extract selectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationEntry {
    /// Not read by the inspection.
    Ignored,
    /// Only the presence of its top-level name matters (an `isolation-*` /
    /// `isolation_*` entry or anything below one).
    TopLevelName,
    /// Read as is: one of the inspected databases, its sidecars, or the
    /// `migration-authority` entry itself.
    Content,
}

/// Covered by the `isolation-*` prefix; named for the tests.
#[cfg(test)]
pub(crate) const QUARANTINE_DIR: &str = "isolation-quarantine";
pub(crate) const APPLY_DIR: &str = "migration-authority";
pub(crate) const APPLY_DB: &str = "apply.db";
pub(crate) const CONFIRMATIONS_DB: &str = "migration-confirmations.db";
pub(crate) const USERS_DB: &str = "users.db";
pub(crate) const PERSONAL_MIRROR: &str = "isolation_personal_agents";
pub(crate) const PERSONAL_REMOVALS: &str = "isolation_personal_agent_removals";
/// More tables than any real store has: refuse rather than scan unbounded.
const MAX_TABLES: usize = 512;
const SIDECARS: [&str; 4] = ["", "-wal", "-journal", "-shm"];

type Check = fn(&Path) -> Result<bool, ()>;

/// Inspect the restore root (the directory whose entries the swap moves into
/// the home). `Present` wins over `Unknown`; both refuse.
pub(crate) fn inspect(root: &Path) -> ArchiveIsolation {
    let checks: [Check; 4] = [isolation_entries, apply_rows, confirmation_rows, personal_rows];
    let mut unknown = false;
    for check in checks {
        match check(root) {
            Ok(true) => return ArchiveIsolation::Present,
            Ok(false) => {}
            Err(()) => unknown = true,
        }
    }
    if unknown { ArchiveIsolation::Unknown } else { ArchiveIsolation::Absent }
}

/// `isolation-*` / `isolation_*`, ASCII case insensitive.
fn is_isolation_name(name: &str) -> bool {
    name.get(..10).is_some_and(|prefix| prefix.eq_ignore_ascii_case("isolation-")
        || prefix.eq_ignore_ascii_case("isolation_"))
}

fn with_sidecar(name: &str, database: &str) -> bool {
    SIDECARS.iter().any(|suffix| name.strip_prefix(database) == Some(*suffix))
}

/// Classify an archive path relative to the restore root. A path that is not
/// a plain relative path (`..`, root, prefix) is `Content` so a selective
/// extractor reaches it and fails closed.
pub fn entry_kind(path: &Path) -> IsolationEntry {
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => match name.to_str() {
                Some(name) => names.push(name),
                None => return IsolationEntry::Content,
            },
            _ => return IsolationEntry::Content,
        }
    }
    match names.as_slice() {
        [] => IsolationEntry::Ignored,
        [first, ..] if is_isolation_name(first) => IsolationEntry::TopLevelName,
        [name] if *name == APPLY_DIR || with_sidecar(name, USERS_DB) || with_sidecar(name, CONFIRMATIONS_DB) =>
            IsolationEntry::Content,
        [dir, name] if *dir == APPLY_DIR && with_sidecar(name, APPLY_DB) => IsolationEntry::Content,
        _ => IsolationEntry::Ignored,
    }
}

fn exists(path: &Path) -> Result<bool, ()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(()),
    }
}

/// Any top-level `isolation-*` / `isolation_*` entry. A name that is not
/// UTF-8 is checked on its lossy form (the prefix is ASCII).
fn isolation_entries(root: &Path) -> Result<bool, ()> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
    };
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        if is_isolation_name(&entry.file_name().to_string_lossy()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `directory/name` opened read-only, never following a link. `Ok(None)`:
/// neither the database nor a WAL / rollback journal of it exists.
fn open(directory: &Path, name: &str) -> Result<Option<Connection>, ()> {
    let path = directory.join(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            for suffix in ["-wal", "-journal"] {
                if exists(&directory.join(format!("{name}{suffix}")))? {
                    return Err(());
                }
            }
            return Ok(None);
        }
        Err(_) => return Err(()),
    };
    if !metadata.is_file() {
        return Err(());
    }
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map_err(|_| ())?;
    harden(&connection)?;
    Ok(Some(connection))
}

/// The archive's schema is untrusted: no schema-defined SQL functions or
/// virtual tables run, defensive mode forbids schema corruption tricks, and
/// the connection cannot write.
fn harden(connection: &Connection) -> Result<(), ()> {
    if !connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(|_| ())? {
        return Err(());
    }
    connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;").map_err(|_| ())
}

fn table_names(connection: &Connection) -> Result<Vec<String>, ()> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND substr(name, 1, 7) != 'sqlite_'")
        .map_err(|_| ())?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0)).map_err(|_| ())?;
    let names: Vec<String> = rows.collect::<Result<_, _>>().map_err(|_| ())?;
    if names.len() > MAX_TABLES { Err(()) } else { Ok(names) }
}

fn table_has_rows(connection: &Connection, table: &str) -> Result<bool, ()> {
    let quoted = table.replace('"', "\"\"");
    connection
        .query_row(&format!("SELECT EXISTS(SELECT 1 FROM \"{quoted}\")"), [], |row| row.get(0))
        .map_err(|_| ())
}

fn any_row_in_any_table(connection: &Connection) -> Result<bool, ()> {
    for table in table_names(connection)? {
        if table_has_rows(connection, &table)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn apply_rows(root: &Path) -> Result<bool, ()> {
    match open(&root.join(APPLY_DIR), APPLY_DB)? {
        Some(connection) => any_row_in_any_table(&connection),
        None => Ok(false),
    }
}

fn confirmation_rows(root: &Path) -> Result<bool, ()> {
    match open(root, CONFIRMATIONS_DB)? {
        Some(connection) => any_row_in_any_table(&connection),
        None => Ok(false),
    }
}

/// A row in the Personal ownership mirror or in its removal grants.
fn personal_rows(root: &Path) -> Result<bool, ()> {
    let Some(connection) = open(root, USERS_DB)? else { return Ok(false) };
    let tables = table_names(&connection)?;
    for table in [PERSONAL_MIRROR, PERSONAL_REMOVALS] {
        if tables.iter().any(|name| name == table) && table_has_rows(&connection, table)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(path: &Path, sql: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }

    #[test]
    fn l3_archive_without_isolation_state_is_absent() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(inspect(root.path()), ArchiveIsolation::Absent);
        // An ordinary v1.80 / v1.81 single-binary home: users.db (even with
        // the empty mirror and removal tables), empty isolation stores, other
        // databases, names that only resemble the isolation prefix.
        db(&root.path().join(USERS_DB), "CREATE TABLE users(id TEXT); INSERT INTO users VALUES('u1');
            CREATE TABLE isolation_personal_agents(agent_name TEXT, owner_user_id TEXT);
            CREATE TABLE isolation_personal_agent_removals(agent_name TEXT, owner_user_id TEXT);");
        db(&root.path().join(APPLY_DIR).join(APPLY_DB), "CREATE TABLE confirmed_employees(label TEXT);");
        db(&root.path().join(CONFIRMATIONS_DB), "CREATE TABLE migration_proposals(id TEXT);");
        db(&root.path().join("tasks.db"), "CREATE TABLE tasks(id TEXT); INSERT INTO tasks VALUES('t');");
        std::fs::create_dir_all(root.path().join("agents/dudu")).unwrap();
        std::fs::create_dir_all(root.path().join("agents/isolation-notes")).unwrap();
        std::fs::write(root.path().join("isolation"), b"x").unwrap();
        std::fs::write(root.path().join("isolationx.db"), b"x").unwrap();
        assert_eq!(inspect(root.path()), ArchiveIsolation::Absent);
    }

    #[test]
    fn l3_each_kind_of_isolation_state_is_present() {
        type Plant = fn(&Path);
        let plants: [(&str, Plant); 14] = [
            ("confirmed employee", |root| db(&root.join(APPLY_DIR).join(APPLY_DB),
                "CREATE TABLE confirmed_employees(label TEXT); INSERT INTO confirmed_employees VALUES('alice');")),
            ("quarantine row", |root| db(&root.join(APPLY_DIR).join(APPLY_DB),
                "CREATE TABLE employee_quarantine(label TEXT); INSERT INTO employee_quarantine VALUES('alice');")),
            ("personal mirror", |root| db(&root.join(USERS_DB),
                "CREATE TABLE isolation_personal_agents(agent_name TEXT, owner_user_id TEXT);
                 INSERT INTO isolation_personal_agents VALUES('alice', 'u1');")),
            ("personal removal grant", |root| db(&root.join(USERS_DB),
                "CREATE TABLE isolation_personal_agents(agent_name TEXT, owner_user_id TEXT);
                 CREATE TABLE isolation_personal_agent_removals(agent_name TEXT, owner_user_id TEXT);
                 INSERT INTO isolation_personal_agent_removals VALUES('alice', 'u1');")),
            ("migration confirmation", |root| db(&root.join(CONFIRMATIONS_DB),
                "CREATE TABLE migration_proposals(id TEXT); INSERT INTO migration_proposals VALUES('p');")),
            ("quarantine directory", |root| std::fs::create_dir_all(root.join(QUARANTINE_DIR).join("row")).unwrap()),
            ("empty quarantine directory", |root| std::fs::create_dir_all(root.join(QUARANTINE_DIR)).unwrap()),
            ("connector catalog", |root| std::fs::write(root.join("isolation-connectors.db"), b"").unwrap()),
            ("fork store", |root| db(&root.join("isolation-forks.db"), "CREATE TABLE forks(id TEXT);")),
            ("execution jobs", |root| std::fs::write(root.join("isolation-execution-jobs.db"), b"").unwrap()),
            ("ephemeral jobs", |root| std::fs::write(root.join("isolation-ephemeral-jobs.db"), b"").unwrap()),
            ("background receipts", |root| std::fs::write(root.join("isolation_background.db"), b"").unwrap()),
            ("isolation sidecar only", |root| std::fs::write(root.join("isolation-forks.db-wal"), b"").unwrap()),
            ("isolation directory, any case", |root| std::fs::create_dir_all(root.join("Isolation_Future")).unwrap()),
        ];
        for (name, plant) in plants {
            let root = tempfile::tempdir().unwrap();
            plant(root.path());
            assert_eq!(inspect(root.path()), ArchiveIsolation::Present, "{name}");
        }
    }

    #[test]
    fn l3_unreadable_or_unknown_state_fails_closed() {
        type Plant = fn(&Path);
        let plants: [(&str, Plant); 5] = [
            ("garbage apply.db", |root| {
                std::fs::create_dir_all(root.join(APPLY_DIR)).unwrap();
                std::fs::write(root.join(APPLY_DIR).join(APPLY_DB), b"not a SQLite database").unwrap();
            }),
            ("garbage users.db", |root| std::fs::write(root.join(USERS_DB), b"not a SQLite database").unwrap()),
            ("directory in place of a database", |root| std::fs::create_dir_all(root.join(CONFIRMATIONS_DB)).unwrap()),
            ("WAL without its database", |root| std::fs::write(root.join(format!("{CONFIRMATIONS_DB}-wal")), b"x").unwrap()),
            ("file in place of migration-authority", |root| std::fs::write(root.join(APPLY_DIR), b"x").unwrap()),
        ];
        for (name, plant) in plants {
            let root = tempfile::tempdir().unwrap();
            plant(root.path());
            assert_eq!(inspect(root.path()), ArchiveIsolation::Unknown, "{name}");
        }
        // Present wins over unknown.
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(USERS_DB), b"not a SQLite database").unwrap();
        std::fs::create_dir_all(root.path().join(QUARANTINE_DIR)).unwrap();
        assert_eq!(inspect(root.path()), ArchiveIsolation::Present);
    }

    #[test]
    fn l3_inspection_connection_is_hardened() {
        let root = tempfile::tempdir().unwrap();
        db(&root.path().join(USERS_DB), "CREATE TABLE users(id TEXT);");
        let connection = open(root.path(), USERS_DB).unwrap().unwrap();
        assert!(connection.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap());
        let trusted: i64 = connection.query_row("PRAGMA trusted_schema", [], |row| row.get(0)).unwrap();
        assert_eq!(trusted, 0);
        assert!(connection.execute_batch("INSERT INTO users VALUES('x')").is_err(), "read-only");
    }

    #[test]
    fn l3_entry_kind_selects_every_inspected_path() {
        let kind = |path: &str| entry_kind(Path::new(path));
        for path in ["isolation-quarantine", "isolation-quarantine/a/b", "isolation-forks.db",
            "isolation_background.db", "./ISOLATION-x"] {
            assert_eq!(kind(path), IsolationEntry::TopLevelName, "{path}");
        }
        for path in ["users.db", "users.db-wal", "users.db-shm", "users.db-journal", "migration-confirmations.db",
            "migration-confirmations.db-wal", "migration-authority", "migration-authority/apply.db",
            "migration-authority/apply.db-journal", "../users.db"] {
            assert_eq!(kind(path), IsolationEntry::Content, "{path}");
        }
        for path in ["agents/dudu/SOUL.md", "users.db.bak", "migration-authority/other.db", "tasks.db",
            "agents/isolation-x", "isolation", ""] {
            assert_eq!(kind(path), IsolationEntry::Ignored, "{path}");
        }
    }
}
