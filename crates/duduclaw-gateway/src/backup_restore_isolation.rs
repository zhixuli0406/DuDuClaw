//! L3: does a whole-home backup carry isolation state?
//!
//! On a protected single-binary host (protected mode on, isolation not
//! installed) a whole-home restore is allowed as in v1.80, but NOT of an
//! archive taken on an isolation host: it would bring isolation records onto a
//! host whose gates no longer enforce them (the Personal / lifecycle rules
//! follow `runtime_readiness::isolation_active`). The archive is inspected
//! after the safe extraction into the request's private temp directory and
//! before anything is published, and again at the boot swap.
//!
//! Isolation state is any of:
//! - `isolation-quarantine/` (any entry, of any type);
//! - `migration-authority/apply.db` with a row in any table (confirmed
//!   employees, quarantine rows and label holds, root allocations, Personal
//!   employee requests, the employee schema version);
//! - `migration-confirmations.db` with a row in any table (migration
//!   proposals, applies, confirmation audit, rotations);
//! - a row in the `users.db` Personal ownership mirror
//!   (`isolation_personal_agents`).
//!
//! Anything that cannot be ruled out is [`ArchiveIsolation::Unknown`] (the
//! caller refuses): a stat error other than NotFound, a non-file database, a
//! WAL / rollback journal without its database, a file SQLite cannot read, an
//! unreadable schema or table.
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveIsolation {
    Absent,
    Present,
    Unknown,
}

pub(crate) const QUARANTINE_DIR: &str = "isolation-quarantine";
pub(crate) const APPLY_DIR: &str = "migration-authority";
pub(crate) const APPLY_DB: &str = "apply.db";
pub(crate) const CONFIRMATIONS_DB: &str = "migration-confirmations.db";
pub(crate) const USERS_DB: &str = "users.db";
pub(crate) const PERSONAL_MIRROR: &str = "isolation_personal_agents";
/// More tables than any real store has: refuse rather than scan unbounded.
const MAX_TABLES: usize = 512;

type Check = fn(&Path) -> Result<bool, ()>;

/// Inspect the restore root (the directory whose entries the swap moves into
/// the home). `Present` wins over `Unknown`; both refuse.
pub(crate) fn inspect(root: &Path) -> ArchiveIsolation {
    let checks: [Check; 4] = [quarantine, apply_rows, confirmation_rows, personal_mirror_rows];
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

fn exists(path: &Path) -> Result<bool, ()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(()),
    }
}

fn quarantine(root: &Path) -> Result<bool, ()> {
    exists(&root.join(QUARANTINE_DIR))
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
    Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map(Some)
        .map_err(|_| ())
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

fn personal_mirror_rows(root: &Path) -> Result<bool, ()> {
    let Some(connection) = open(root, USERS_DB)? else { return Ok(false) };
    if !table_names(&connection)?.iter().any(|name| name == PERSONAL_MIRROR) {
        return Ok(false);
    }
    table_has_rows(&connection, PERSONAL_MIRROR)
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
        // An ordinary v1.80 home: users.db (even with an empty mirror table),
        // empty isolation stores, other databases.
        db(&root.path().join(USERS_DB), "CREATE TABLE users(id TEXT); INSERT INTO users VALUES('u1');
            CREATE TABLE isolation_personal_agents(agent_name TEXT, owner_user_id TEXT);");
        db(&root.path().join(APPLY_DIR).join(APPLY_DB), "CREATE TABLE confirmed_employees(label TEXT);");
        db(&root.path().join(CONFIRMATIONS_DB), "CREATE TABLE migration_proposals(id TEXT);");
        db(&root.path().join("tasks.db"), "CREATE TABLE tasks(id TEXT); INSERT INTO tasks VALUES('t');");
        std::fs::create_dir_all(root.path().join("agents/dudu")).unwrap();
        assert_eq!(inspect(root.path()), ArchiveIsolation::Absent);
    }

    #[test]
    fn l3_each_kind_of_isolation_state_is_present() {
        type Plant = fn(&Path);
        let plants: [(&str, Plant); 6] = [
            ("confirmed employee", |root| db(&root.join(APPLY_DIR).join(APPLY_DB),
                "CREATE TABLE confirmed_employees(label TEXT); INSERT INTO confirmed_employees VALUES('alice');")),
            ("quarantine row", |root| db(&root.join(APPLY_DIR).join(APPLY_DB),
                "CREATE TABLE employee_quarantine(label TEXT); INSERT INTO employee_quarantine VALUES('alice');")),
            ("personal mirror", |root| db(&root.join(USERS_DB),
                "CREATE TABLE isolation_personal_agents(agent_name TEXT, owner_user_id TEXT);
                 INSERT INTO isolation_personal_agents VALUES('alice', 'u1');")),
            ("migration confirmation", |root| db(&root.join(CONFIRMATIONS_DB),
                "CREATE TABLE migration_proposals(id TEXT); INSERT INTO migration_proposals VALUES('p');")),
            ("quarantine directory", |root| std::fs::create_dir_all(root.join(QUARANTINE_DIR).join("row")).unwrap()),
            ("empty quarantine directory", |root| std::fs::create_dir_all(root.join(QUARANTINE_DIR)).unwrap()),
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
}
