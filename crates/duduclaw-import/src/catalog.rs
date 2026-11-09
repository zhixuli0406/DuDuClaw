//! Pinned catalogs for the legacy stores whose schema changed between the
//! releases a migration may start from (sessions, tasks, cron, memory,
//! approvals).
//!
//! A source is accepted only when its complete `sqlite_schema` is equal to one
//! pinned catalog; the matching version then selects that version's own column
//! mapping. There is no "close enough": the closest catalog is used solely to
//! name the differences in the refusal report. Names in the report come from
//! the trusted catalogs, except unexpected object names, which are shown only
//! when they are short plain identifiers. No SQL text or row content is ever
//! part of a report.
use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}, fmt};
use rusqlite::Connection;
use crate::{Error, Result, sqlite::{schema, SchemaRow}};

pub(crate) const SESSION_V1793: &str = include_str!("session-v1793.sql");
pub(crate) const SESSION_V180: &str = include_str!("session-v180.sql");
pub(crate) const TASKS_V1793: &str = include_str!("tasks-v1793.sql");
pub(crate) const TASKS_V180: &str = include_str!("tasks-v180.sql");
pub(crate) const TASKS_V181: &str = include_str!("tasks-v181.sql");
pub(crate) const CRON_V1793: &str = include_str!("cron-v1793.sql");
pub(crate) const CRON_V180: &str = include_str!("cron-v180.sql");
pub(crate) const MEMORY_V1793: &str = include_str!("memory-v1793.sql");
pub(crate) const MEMORY_V180: &str = include_str!("memory-v180.sql");
pub(crate) const APPROVALS_V1793: &str = include_str!("approvals-v1793.sql");
pub(crate) const APPROVALS_V180: &str = include_str!("approvals-v180.sql");
pub(crate) const APPROVALS_V181: &str = include_str!("approvals-v181.sql");
const MAX_REPORTED: usize = 16;

/// A legacy store with more than one accepted schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store { Sessions, Tasks, Cron, Memory, Approvals }
impl Store {
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Sessions => "sessions.db", Self::Tasks => "tasks.db", Self::Cron => "cron_tasks.db",
            Self::Memory => "memory.db", Self::Approvals => "approvals.db",
        }
    }
}

/// Known source releases. v1.80.0 and v1.80.1 share one catalog (1.80.1 had
/// no store change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchemaVersion { V1793, V180 }
impl SchemaVersion {
    pub fn label(self) -> &'static str { match self { Self::V1793 => "v1.79.3", Self::V180 => "v1.80.x" } }
}

struct Catalog { version: SchemaVersion, sql: &'static str, label: &'static str }
impl Catalog {
    const fn new(version: SchemaVersion, sql: &'static str) -> Self {
        Catalog { version, sql, label: match version { SchemaVersion::V1793 => "v1.79.3", SchemaVersion::V180 => "v1.80.x" } }
    }
}
// Ordered oldest first; adding a release means a new catalog file, a new
// variant and its own mapping in every reader (the matches are exhaustive).
static SESSIONS: [Catalog; 2] = [Catalog::new(SchemaVersion::V1793, SESSION_V1793), Catalog::new(SchemaVersion::V180, SESSION_V180)];
// Tasks: the v1.81 catalog is the v1.80.x one plus the `task_store_markers`
// table of the goal-creator backfill. It has no readable content for the
// importer (the authorizer denies that table), so it maps as V180.
static TASKS: [Catalog; 3] = [Catalog::new(SchemaVersion::V1793, TASKS_V1793), Catalog::new(SchemaVersion::V180, TASKS_V180),
    Catalog { version: SchemaVersion::V180, sql: TASKS_V181, label: "v1.81" }];
static CRON: [Catalog; 2] = [Catalog::new(SchemaVersion::V1793, CRON_V1793), Catalog::new(SchemaVersion::V180, CRON_V180)];
// Memory and approvals: the newest catalog adds only an index (memory) and two
// decision-lease columns (approvals) that neither reader is authorized to read.
static MEMORY: [Catalog; 2] = [Catalog::new(SchemaVersion::V1793, MEMORY_V1793), Catalog::new(SchemaVersion::V180, MEMORY_V180)];
// Approvals v1.81 = v1.80.x plus the SR5 `approvals.employee_incarnation`
// column; the reader never authorizes it, so it maps as V180.
static APPROVALS: [Catalog; 3] = [Catalog::new(SchemaVersion::V1793, APPROVALS_V1793), Catalog::new(SchemaVersion::V180, APPROVALS_V180),
    Catalog { version: SchemaVersion::V180, sql: APPROVALS_V181, label: "v1.81" }];
fn catalogs(store: Store) -> &'static [Catalog] {
    match store {
        Store::Sessions => &SESSIONS, Store::Tasks => &TASKS, Store::Cron => &CRON,
        Store::Memory => &MEMORY, Store::Approvals => &APPROVALS,
    }
}

/// The exact catalog a source matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Detected { pub(crate) version: SchemaVersion, pub(crate) extension: bool }

/// One named difference between a source and the closest known catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    /// An object of the catalog is absent from the source.
    Missing { object: String, name: String },
    /// The source has an object the catalog does not have.
    Unexpected { object: String, name: String },
    /// Same object, different definition. `missing_columns` lists catalog
    /// columns whose name does not occur in the source definition.
    Changed { object: String, name: String, missing_columns: Vec<String> },
}
impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { object, name } => write!(f, "missing {object} {name}"),
            Self::Unexpected { object, name } => write!(f, "unexpected {object} {name}"),
            Self::Changed { object, name, missing_columns } if missing_columns.is_empty() => write!(f, "changed {object} {name}"),
            Self::Changed { object, name, missing_columns } => write!(f, "changed {object} {name} (missing column {})", missing_columns.join(", ")),
        }
    }
}

/// Why a source of a multi-version store was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRefusal {
    pub store: Store,
    /// The known catalog with the fewest differences; it was NOT accepted.
    pub closest: SchemaVersion,
    /// Whether the closest catalog included the reviewed extension
    /// (channel settings in sessions.db).
    pub closest_extension: bool,
    pub differences: Vec<Difference>,
    /// Differences beyond the reported bound.
    pub omitted: usize,
}
impl fmt::Display for SchemaRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let known = catalogs(self.store).iter().map(|c| c.label).collect::<Vec<_>>().join(", ");
        write!(f, "unsupported {} schema: it matches none of the known catalogs ({known}); differences from the closest ({}{}): ",
            self.store.file_name(), self.closest.label(), if self.closest_extension { " with channel settings" } else { "" })?;
        if self.differences.is_empty() { write!(f, "catalog rows differ")?; }
        for (index, difference) in self.differences.iter().enumerate() {
            if index > 0 { write!(f, "; ")?; }
            write!(f, "{difference}")?;
        }
        if self.omitted > 0 { write!(f, "; and {} more", self.omitted)?; }
        Ok(())
    }
}

thread_local! {
    // The worker is single-threaded and handles one source per process; the
    // report is only diagnostics and never feeds a decision.
    static LAST_REFUSAL: RefCell<Option<SchemaRefusal>> = const { RefCell::new(None) };
}
/// The refusal report of the last multi-version schema check on this thread
/// that returned `Error::Schema`. Taking it clears it.
pub fn take_schema_refusal() -> Option<SchemaRefusal> { LAST_REFUSAL.with(|last| last.borrow_mut().take()) }
pub(crate) fn clear_schema_refusal() { LAST_REFUSAL.with(|last| *last.borrow_mut() = None); }

fn catalog_connection(sql: &str, extension: Option<&str>) -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(sql)?;
    if let Some(extension) = extension { conn.execute_batch(extension)?; }
    Ok(conn)
}

/// Accept the source only if its catalog equals one pinned catalog exactly
/// (optionally with the exact reviewed `extension`). Otherwise refuse with
/// `Error::Schema` and record a report naming the differences.
pub(crate) fn detect(conn: &Connection, store: Store, extension: Option<&str>) -> Result<Detected> {
    clear_schema_refusal();
    let actual = schema(conn)?;
    let mut closest: Option<(SchemaVersion, bool, Vec<Difference>)> = None;
    for catalog in catalogs(store) {
        for with_extension in [false, true] {
            let ext = if with_extension { match extension { Some(ext) => Some(ext), None => continue } } else { None };
            let expected_conn = catalog_connection(catalog.sql, ext)?;
            let expected = schema(&expected_conn)?;
            if actual == expected { return Ok(Detected { version: catalog.version, extension: with_extension }); }
            let found = differences(&actual, &expected, &expected_conn)?;
            // Fewest differences wins; ties keep the earlier (older) catalog.
            if closest.as_ref().is_none_or(|(_, _, best)| found.len() < best.len()) {
                closest = Some((catalog.version, with_extension, found));
            }
        }
    }
    if let Some((version, with_extension, mut differences)) = closest {
        let omitted = differences.len().saturating_sub(MAX_REPORTED);
        differences.truncate(MAX_REPORTED);
        let refusal = SchemaRefusal { store, closest: version, closest_extension: with_extension, differences, omitted };
        LAST_REFUSAL.with(|last| *last.borrow_mut() = Some(refusal));
    }
    Err(Error::Schema)
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

fn printable(value: &str) -> String {
    if !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        value.to_owned()
    } else {
        "<unprintable name>".to_owned()
    }
}

/// Identifier-like tokens of a definition. Used only to test whether a
/// trusted catalog column name occurs; tokens themselves are never reported.
fn tokens(sql: &str) -> BTreeSet<String> {
    sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty()).map(str::to_ascii_lowercase).collect()
}

fn differences(actual: &[SchemaRow], expected: &[SchemaRow], expected_conn: &Connection) -> Result<Vec<Difference>> {
    // (type, name) is unique in sqlite_schema; the value is (tbl_name, sql).
    fn index(rows: &[SchemaRow]) -> BTreeMap<(&str, &str), (&str, Option<&str>)> {
        rows.iter().map(|(kind, name, table, sql)| ((kind.as_str(), name.as_str()), (table.as_str(), sql.as_deref()))).collect()
    }
    let (actual_index, expected_index) = (index(actual), index(expected));
    let mut result = Vec::new();
    for (&(kind, name), &definition) in &expected_index {
        match actual_index.get(&(kind, name)) {
            None => result.push(Difference::Missing { object: kind.to_owned(), name: name.to_owned() }),
            Some(&found) if found != definition => {
                let mut missing_columns = Vec::new();
                if kind == "table" {
                    let present = tokens(found.1.unwrap_or(""));
                    let mut statement = expected_conn.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?;
                    let columns = statement.query_map([name], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
                    missing_columns = columns.into_iter().filter(|column| !present.contains(&column.to_ascii_lowercase())).collect();
                }
                result.push(Difference::Changed { object: kind.to_owned(), name: name.to_owned(), missing_columns });
            }
            Some(_) => {}
        }
    }
    for &(kind, name) in actual_index.keys() {
        if !expected_index.contains_key(&(kind, name)) {
            result.push(Difference::Unexpected { object: printable(kind), name: printable(name) });
        }
    }
    Ok(result)
}
