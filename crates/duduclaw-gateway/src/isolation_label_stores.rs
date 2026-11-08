//! Label-keyed state outside `agents/<label>` (native Linux isolation v1.81,
//! F6 design r5 §3.3, §3.6, §3.10 step 7, §3.12; task F6h: H4, H5, R7, S7,
//! S9, S10).
//!
//! - [`LABEL_KEYED_STORES`]: the one registry (S9). Tombstone marks and
//!   strips, purge, re-key and the label-free check iterate it, never a
//!   hand-written list. Each entry has `count_under`, an idempotent `rekey`
//!   and `purge`.
//! - The writer gate ([`check_label_writable`] and its store adapters):
//!   memory, sessions, tasks and cron writes for a tombstoned or quarantined
//!   label are refused; an unreadable lifecycle index refuses (fail closed).
//!   Active only in protected mode ([`protected_active`]); elsewhere every
//!   adapter returns `Ok` before any I/O.
//! - [`tombstone_effects`]: §3.2 step 4 (S9): global routes to the label are
//!   stripped into `stripped`, label-keyed work is paused, store marks are
//!   recorded in the quarantine row.
//! - [`channel_bot_allowed`]: a protected employee's own bot starts only with
//!   a live lifecycle and an active catalog channel route (H5, A11).
//! - [`check_label_not_quarantined`]: `LABEL_QUARANTINED` (R7) for staging,
//!   approving or applying an Employee proposal on a held label.
//!
//! Labels and keys are compared by exact equality only, never by substring.
//! `[channels.*]` and `[capabilities] db_sources` live inside the employee
//! tree and are stripped by the F6a tombstone ([`IN_TREE_STRIPPED`]).
//! Everything here is blocking (SQLite, filesystem) except the async bot
//! gate wrapper.
use std::path::Path;

use duduclaw_isolation::ids::EmployeeId;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};

use crate::fs_safe::sqlite::Access;

/// R7 refusal shown to the admin (proposal) or returned to a writer.
pub(crate) const LABEL_QUARANTINED: &str = "此員工代號仍保留給已註銷的員工：在該員工的資料清除或由新員工接手之前，\
    不能用於新的員工，也不能再寫入資料。";
/// The lifecycle index or a store could not be read: refused (fail closed).
pub(crate) const LABEL_STATE_UNAVAILABLE: &str = "無法確認這個員工代號目前的狀態，已拒絕操作。";
const NOT_LIVE: &str = "這位員工已停用，不能再寫入資料。";

/// Quarantine states that no longer hold the label (S7).
pub(crate) const CLOSED_STATES: [&str; 3] = ["purged", "adopted", "restored"];
/// Label-keyed grants inside the tree, stripped by the F6a tombstone (H5).
pub(crate) const IN_TREE_STRIPPED: [&str; 2] = ["agent.toml [channels.*]", "agent.toml [capabilities] db_sources"];
/// §3.3 global routes this build does not yet model (listed in `stripped`).
pub(crate) const DEFERRED_ROUTES: [&str; 3] = ["webhook_automation_routes", "global_heartbeat_definitions",
    "channel_settings_agent_keys"];
/// Fence group written by [`tombstone_effects`] (H2 grouping).
pub(crate) const STORE_FENCE_GROUP: &str = "stores:";

const APPLY_DIR: &str = "migration-authority";
const APPLY_DB: &str = "apply.db";
/// Column names that hold an agent label in the SQLite stores. Discovery is
/// by exact column name over every table, so a table added later is covered
/// without editing this module.
/// `owner_agent_id`: tasks.db `responsibilities` (P2-A).
pub(crate) const AGENT_COLUMNS: [&str; 6] = ["agent_id", "assigned_to", "owner_agent_id", "agent", "agent_name", "agent_label"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreError {
    /// The store exists but could not be read or written. Never zero.
    Unavailable,
}

/// Why a write, a bot start or a proposal on a label is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateRefusal {
    /// A quarantine row on the label is not closed (S7), or a purged label
    /// still holds state (S10).
    Quarantined,
    /// The confirmed row of the label is disabled.
    NotLive,
    /// The lifecycle index or a store could not be read.
    Unavailable,
}

impl GateRefusal {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Quarantined => LABEL_QUARANTINED,
            Self::NotLive => NOT_LIVE,
            Self::Unavailable => LABEL_STATE_UNAVAILABLE,
        }
    }
}

// ── Protected mode ──────────────────────────────────────────────────────

/// Protected mode as the neighbouring isolation code detects it
/// (`DUDUCLAW_PROTECTED_KEY_MODE=native-systemd` on Linux). Tests force it
/// per thread through [`TEST_PROTECTED`].
pub(crate) fn protected_active() -> bool {
    #[cfg(test)]
    if let Some(forced) = TEST_PROTECTED.with(std::cell::Cell::get) { return forced; }
    crate::runtime_readiness::is_protected_linux()
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_PROTECTED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

// ── What apply.db says about a label ────────────────────────────────────

/// The lifecycle index view of one label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LabelHold {
    /// `Some(disabled)` when a confirmed row holds the label.
    pub(crate) confirmed: Option<bool>,
    /// Quarantine rows on the label that are not closed: `(hex, incarnation, seal)`.
    pub(crate) open_rows: Vec<(String, u32, String)>,
    /// Quarantine rows on the label in state `purged`.
    pub(crate) purged: usize,
}

fn open_apply(home: &Path, access: Access) -> Result<Connection, StoreError> {
    let db = crate::fs_safe::sqlite::open_existing(&home.join(APPLY_DIR), APPLY_DB, access)
        .map_err(|_| StoreError::Unavailable)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| StoreError::Unavailable)?;
    Ok(db)
}

fn table_exists(db: &Connection, table: &str) -> Result<bool, StoreError> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [table], |r| r.get(0))
        .map_err(|_| StoreError::Unavailable)
}

/// Read the label's hold. No apply.db (nothing was ever confirmed or
/// quarantined) is an empty hold; an unsafe, unreadable or future-schema
/// store, or a corrupt row, is `Unavailable`.
pub(crate) fn label_hold(home: &Path, label: &str) -> Result<LabelHold, StoreError> {
    match std::fs::symlink_metadata(home.join(APPLY_DIR).join(APPLY_DB)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(LabelHold::default()),
        Err(_) => return Err(StoreError::Unavailable),
        Ok(_) => {}
    }
    let db = open_apply(home, Access::ReadOnly)?;
    crate::isolation_quarantine::check_readable(&db).map_err(|_| StoreError::Unavailable)?;
    let mut hold = LabelHold::default();
    if table_exists(&db, "confirmed_employees")? {
        let encoded: Option<String> = db.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [label],
            |r| r.get(0)).optional().map_err(|_| StoreError::Unavailable)?;
        if let Some(encoded) = encoded {
            let receipt: duduclaw_migration::allocation::Receipt =
                serde_json::from_str(&encoded).map_err(|_| StoreError::Unavailable)?;
            if receipt.label != label { return Err(StoreError::Unavailable); }
            hold.confirmed = Some(receipt.disabled);
        }
    }
    if table_exists(&db, "employee_quarantine")? {
        let mut query = db.prepare(
            "SELECT employee_hex,incarnation,seal FROM employee_quarantine WHERE label=?1 ORDER BY employee_hex,incarnation")
            .map_err(|_| StoreError::Unavailable)?;
        let rows = query.query_map([label], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))
            .map_err(|_| StoreError::Unavailable)?.collect::<Result<Vec<_>, _>>().map_err(|_| StoreError::Unavailable)?;
        for (hex, incarnation, seal) in rows {
            let incarnation = u32::try_from(incarnation).map_err(|_| StoreError::Unavailable)?;
            if seal == "purged" { hold.purged += 1; }
            if !CLOSED_STATES.contains(&seal.as_str()) { hold.open_rows.push((hex, incarnation, seal)); }
        }
    }
    Ok(hold)
}

/// The key a label's rows are re-keyed under at adoption (§3.3, N1).
pub(crate) fn quarantine_key(employee: EmployeeId, incarnation: u32) -> String {
    format!("quarantine:{}-{incarnation}", hex::encode(employee.0))
}

// ── Writer gate (H4) ────────────────────────────────────────────────────

/// Memory namespaces are the label itself or `agent:<label>` (exact token).
fn subject_label(key: &str) -> &str { key.strip_prefix("agent:").unwrap_or(key) }

/// The writer gate for one label: refuses while a quarantine row on it is
/// open or its confirmed row is disabled; refuses when the lifecycle index
/// cannot be read. Outside protected mode: `Ok` without I/O.
pub(crate) fn check_label_writable(home: &Path, label: &str) -> Result<(), GateRefusal> {
    if !protected_active() { return Ok(()); }
    let hold = label_hold(home, subject_label(label)).map_err(|_| GateRefusal::Unavailable)?;
    if !hold.open_rows.is_empty() { return Err(GateRefusal::Quarantined); }
    if hold.confirmed == Some(true) { return Err(GateRefusal::NotLive); }
    Ok(())
}

/// The writer gate on a store connection: the home is the directory of the
/// connection's database file (protected stores are never in-memory, so an
/// unknown file refuses).
pub(crate) fn check_label_writable_at(db: &Connection, label: &str) -> Result<(), String> {
    if !protected_active() { return Ok(()); }
    let home = db.path().filter(|path| !path.is_empty()).and_then(|path| Path::new(path).parent())
        .ok_or_else(|| GateRefusal::Unavailable.message().to_owned())?;
    check_label_writable(home, label).map_err(|refusal| refusal.message().to_owned())
}

/// Sessions writer gate for a message append: the session's agent decides.
/// An unknown session is left to the insert (it fails on its own).
pub(crate) fn check_session_writable(db: &Connection, session: &str) -> Result<(), String> {
    if !protected_active() { return Ok(()); }
    let agent: Option<String> = db.query_row("SELECT agent_id FROM sessions WHERE id=?1", [session], |r| r.get(0))
        .optional().map_err(|_| GateRefusal::Unavailable.message().to_owned())?;
    match agent { Some(agent) => check_label_writable_at(db, &agent), None => Ok(()) }
}

/// The memory crate's gate ([`duduclaw_memory::install_label_write_gate`]).
pub(crate) fn memory_write_gate(file: Option<&Path>, namespace: &str) -> Result<(), String> {
    if !protected_active() { return Ok(()); }
    let home = file.and_then(Path::parent).ok_or_else(|| GateRefusal::Unavailable.message().to_owned())?;
    check_label_writable(home, namespace).map_err(|refusal| refusal.message().to_owned())
}

/// Install the memory gate once per process (gateway boot).
pub(crate) fn install_memory_gate() {
    let _ = duduclaw_memory::install_label_write_gate(memory_write_gate);
}

// ── Registry (S9) ───────────────────────────────────────────────────────

/// What the tombstone does with a store (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TombstoneAction {
    /// Covered by the quarantine digest (`org:<label>`, H4); left in place.
    Digest,
    /// Writer gate; marks recorded.
    Gate,
    /// Writer gate, marks recorded, and the label's work paused (`enabled = 0`).
    Pause,
    /// Stripped, names kept in `stripped` (S9).
    Strip,
}

/// Purge removes these child rows first: rows of `parent` whose `label`
/// column is the label are referenced by `child` columns in other tables.
pub(crate) struct Parent {
    pub(crate) table: &'static str,
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) child: &'static str,
}

pub(crate) enum Backend {
    /// A home SQLite database; the label columns are discovered by name
    /// ([`AGENT_COLUMNS`]) in every table.
    Sqlite { parent: Option<Parent> },
    /// A config map: `count` and `detach` (strip / remove / re-root). A config
    /// entry cannot be sealed under the row, so re-key and purge both detach.
    Config { count: fn(&Path, &str) -> Result<u64, StoreError>, detach: fn(&Path, &str) -> Result<Vec<String>, StoreError> },
}

pub(crate) struct LabelStore {
    pub(crate) name: &'static str,
    /// Home-relative file holding the store.
    pub(crate) source: &'static str,
    pub(crate) action: TombstoneAction,
    pub(crate) backend: Backend,
}

/// Every store keyed by an agent label outside the employee tree (§3.3).
pub(crate) static LABEL_KEYED_STORES: &[LabelStore] = &[
    LabelStore { name: "org_entry", source: "org.toml", action: TombstoneAction::Digest,
        backend: Backend::Config { count: org_entry_count, detach: org_entry_remove } },
    LabelStore { name: "memory", source: "memory.db", action: TombstoneAction::Gate,
        backend: Backend::Sqlite { parent: None } },
    LabelStore { name: "sessions", source: "sessions.db", action: TombstoneAction::Gate,
        backend: Backend::Sqlite { parent: Some(Parent { table: "sessions", id: "id", label: "agent_id", child: "session_id" }) } },
    LabelStore { name: "tasks", source: "tasks.db", action: TombstoneAction::Gate,
        backend: Backend::Sqlite { parent: Some(Parent { table: "tasks", id: "id", label: "assigned_to", child: "task_id" }) } },
    LabelStore { name: "cron", source: "cron_tasks.db", action: TombstoneAction::Pause,
        backend: Backend::Sqlite { parent: Some(Parent { table: "cron_tasks", id: "id", label: "agent_id", child: "cron_id" }) } },
    LabelStore { name: "website_credentials", source: "secrets/website_credentials.enc", action: TombstoneAction::Strip,
        backend: Backend::Config { count: website_count, detach: website_strip } },
    LabelStore { name: "channel_routes", source: "agent_bindings.json", action: TombstoneAction::Strip,
        backend: Backend::Config { count: channel_route_count, detach: channel_route_strip } },
    LabelStore { name: "delegation_targets", source: "org.toml", action: TombstoneAction::Strip,
        backend: Backend::Config { count: delegation_count, detach: delegation_reroot } },
];

impl LabelStore {
    /// Rows or entries under `label`. A missing store is a positive 0; an
    /// unreadable one is `Err` (never zero, S10).
    pub(crate) fn count_under(&self, home: &Path, label: &str) -> Result<u64, StoreError> {
        match &self.backend {
            Backend::Sqlite { .. } => sqlite::count(home, self.source, label),
            Backend::Config { count, .. } => count(home, label),
        }
    }

    /// Move everything under `old` to `new` (adoption, §3.10 step 5).
    /// Idempotent: a store already moved is a no-op (`Ok(0)`). Config maps
    /// detach `old` (pointing them at a new id is an explicit admin edit).
    pub(crate) fn rekey(&self, home: &Path, old: &str, new: &str) -> Result<u64, StoreError> {
        if old == new || old.is_empty() || new.is_empty() { return Err(StoreError::Unavailable); }
        match &self.backend {
            Backend::Sqlite { .. } => sqlite::rekey(home, self.source, old, new),
            Backend::Config { detach, .. } => detach(home, old).map(|names| names.len() as u64),
        }
    }

    /// Remove everything under `label` (F6i purge, §3.8). Idempotent.
    pub(crate) fn purge(&self, home: &Path, label: &str) -> Result<u64, StoreError> {
        if label.is_empty() { return Err(StoreError::Unavailable); }
        match &self.backend {
            Backend::Sqlite { parent } => sqlite::purge(home, self.source, parent.as_ref(), label),
            Backend::Config { detach, .. } => detach(home, label).map(|names| names.len() as u64),
        }
    }

    /// The tombstone strip of a `Strip` store (names recorded).
    pub(crate) fn detach(&self, home: &Path, label: &str) -> Result<Vec<String>, StoreError> {
        match &self.backend {
            Backend::Config { detach, .. } => detach(home, label),
            Backend::Sqlite { .. } => Err(StoreError::Unavailable),
        }
    }
}

/// Per-store counts under `label` (diagnostics, store marks).
pub(crate) fn counts_under(home: &Path, label: &str) -> Vec<(&'static str, Result<u64, StoreError>)> {
    LABEL_KEYED_STORES.iter().map(|store| (store.name, store.count_under(home, label))).collect()
}

/// S10: the label holds nothing only with a **positive** zero count from
/// every registered store; an unreadable store is `Err`, never free.
pub(crate) fn label_free(home: &Path, label: &str) -> Result<bool, StoreError> {
    label_free_where(home, label, |_| true)
}

/// [`label_free`] over the stores `include` selects (every store is read;
/// an unreadable selected store is `Err`).
fn label_free_where(home: &Path, label: &str, include: impl Fn(&LabelStore) -> bool) -> Result<bool, StoreError> {
    let mut free = true;
    for store in LABEL_KEYED_STORES.iter().filter(|store| include(store)) {
        if store.count_under(home, label)? != 0 { free = false; }
    }
    Ok(free)
}

/// Re-key every registered store (adoption; F6g calls it before its commit
/// and in `adopting` recovery). Stops at the first failing store.
pub(crate) fn rekey_all(home: &Path, old: &str, new: &str) -> Result<Vec<(&'static str, u64)>, (&'static str, StoreError)> {
    LABEL_KEYED_STORES.iter().map(|store| store.rekey(home, old, new).map(|n| (store.name, n)).map_err(|e| (store.name, e)))
        .collect()
}

/// Purge every registered store (F6i). Stops at the first failing store.
pub(crate) fn purge_all(home: &Path, label: &str) -> Result<Vec<(&'static str, u64)>, (&'static str, StoreError)> {
    LABEL_KEYED_STORES.iter().map(|store| store.purge(home, label).map(|n| (store.name, n)).map_err(|e| (store.name, e)))
        .collect()
}

// ── Config map stores ───────────────────────────────────────────────────

fn org_entry_count(home: &Path, label: &str) -> Result<u64, StoreError> {
    let store = duduclaw_core::org_store::load_for_write(home).map_err(|_| StoreError::Unavailable)?;
    Ok(u64::from(store.contains(label)))
}

fn org_entry_remove(home: &Path, label: &str) -> Result<Vec<String>, StoreError> {
    if org_entry_count(home, label)? == 0 { return Ok(Vec::new()); }
    duduclaw_core::org_store::with_org_lock(home, |guard| duduclaw_core::org_store::remove(guard, label))
        .map_err(|_| StoreError::Unavailable)
        .map(|removed| if removed { vec![format!("org:{label}")] } else { Vec::new() })
}

/// S9 "other employees' delegation targets": delegation follows the org
/// tree, so an employee whose `reports_to` is the label would be delegated
/// to by whoever later holds the label.
fn delegation_count(home: &Path, label: &str) -> Result<u64, StoreError> {
    let store = duduclaw_core::org_store::load_for_write(home).map_err(|_| StoreError::Unavailable)?;
    Ok(store.iter().filter(|(id, entry)| id.as_str() != label && entry.reports_to == label).count() as u64)
}

/// Re-root every employee reporting to the label (names recorded); the admin
/// re-parents them explicitly (never automatic).
fn delegation_reroot(home: &Path, label: &str) -> Result<Vec<String>, StoreError> {
    use duduclaw_core::org_store::{self as org, OrgEntry};
    let store = org::load_for_write(home).map_err(|_| StoreError::Unavailable)?;
    let reports: Vec<(String, String)> = store.iter()
        .filter(|(id, entry)| id.as_str() != label && entry.reports_to == label)
        .map(|(id, entry)| (id.clone(), entry.department.clone())).collect();
    if reports.is_empty() { return Ok(Vec::new()); }
    org::with_org_lock(home, |guard| {
        let mut names = Vec::new();
        for (id, department) in &reports {
            // Re-read under the lock: only an edge that still points at the label moves.
            let current = org::load_for_write(home).map_err(|_| std::io::Error::other("org store unreadable"))?;
            if current.get(id).is_some_and(|entry| entry.reports_to == label) {
                org::upsert(guard, id, OrgEntry::new("", department))?;
                names.push(format!("reports_to:{id}"));
            }
        }
        Ok(names)
    }).map_err(|_| StoreError::Unavailable)
}

fn website_count(home: &Path, label: &str) -> Result<u64, StoreError> {
    crate::website_credentials::agent_grant_count(home, label).map_err(|_| StoreError::Unavailable)
}

fn website_strip(home: &Path, label: &str) -> Result<Vec<String>, StoreError> {
    crate::website_credentials::strip_agent(home, label).map_err(|_| StoreError::Unavailable)
}

fn channel_route_count(home: &Path, label: &str) -> Result<u64, StoreError> {
    crate::agent_binding::label_route_count(&home.join("agent_bindings.json"), label).map_err(|_| StoreError::Unavailable)
}

fn channel_route_strip(home: &Path, label: &str) -> Result<Vec<String>, StoreError> {
    crate::agent_binding::strip_label_routes(&home.join("agent_bindings.json"), label).map_err(|_| StoreError::Unavailable)
}

// ── SQLite stores ───────────────────────────────────────────────────────

pub(crate) mod sqlite {
    //! Generic label-column operations over one home SQLite database.
    use super::{Parent, StoreError, AGENT_COLUMNS};
    use crate::fs_safe::sqlite::Access;
    use rusqlite::{Connection, TransactionBehavior};
    use std::path::Path;

    fn quoted(name: &str) -> String { format!("\"{}\"", name.replace('"', "\"\"")) }

    /// `None` when the store file does not exist (positive zero).
    fn open(home: &Path, file: &str, access: Access) -> Result<Option<Connection>, StoreError> {
        match std::fs::symlink_metadata(home.join(file)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(StoreError::Unavailable),
            Ok(_) => {}
        }
        let db = crate::fs_safe::sqlite::open_existing(home, file, access).map_err(|_| StoreError::Unavailable)?;
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| StoreError::Unavailable)?;
        // Any read proves the file is a database (a non-database file errors here).
        db.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(|_| StoreError::Unavailable)?;
        Ok(Some(db))
    }

    fn tables(db: &Connection) -> Result<Vec<String>, StoreError> {
        let mut query = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|_| StoreError::Unavailable)?;
        let names = query.query_map([], |r| r.get::<_, String>(0)).map_err(|_| StoreError::Unavailable)?
            .collect::<Result<Vec<_>, _>>().map_err(|_| StoreError::Unavailable);
        names
    }

    fn columns(db: &Connection, table: &str) -> Result<Vec<String>, StoreError> {
        let mut query = db.prepare("SELECT name FROM pragma_table_info(?1)").map_err(|_| StoreError::Unavailable)?;
        let names = query.query_map([table], |r| r.get::<_, String>(0)).map_err(|_| StoreError::Unavailable)?
            .collect::<Result<Vec<_>, _>>().map_err(|_| StoreError::Unavailable);
        names
    }

    /// Every `(table, column)` whose column name is in [`AGENT_COLUMNS`].
    pub(crate) fn keyed_columns(db: &Connection) -> Result<Vec<(String, String)>, StoreError> {
        let mut keyed = Vec::new();
        for table in tables(db)? {
            for column in columns(db, &table)? {
                if AGENT_COLUMNS.contains(&column.as_str()) { keyed.push((table.clone(), column)); }
            }
        }
        Ok(keyed)
    }

    /// Every table (other than `except`) carrying a column named `column`.
    fn tables_with(db: &Connection, column: &str, except: &str) -> Result<Vec<String>, StoreError> {
        let mut found = Vec::new();
        for table in tables(db)? {
            if table != except && columns(db, &table)?.iter().any(|name| name == column) { found.push(table); }
        }
        Ok(found)
    }

    pub(crate) fn count(home: &Path, file: &str, label: &str) -> Result<u64, StoreError> {
        let Some(db) = open(home, file, Access::ReadOnly)? else { return Ok(0) };
        let mut total = 0u64;
        for (table, column) in keyed_columns(&db)? {
            let rows: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {} WHERE {} = ?1", quoted(&table), quoted(&column)),
                [label], |r| r.get(0)).map_err(|_| StoreError::Unavailable)?;
            total = total.saturating_add(u64::try_from(rows).map_err(|_| StoreError::Unavailable)?);
        }
        Ok(total)
    }

    pub(crate) fn rekey(home: &Path, file: &str, old: &str, new: &str) -> Result<u64, StoreError> {
        let Some(mut db) = open(home, file, Access::ReadWrite)? else { return Ok(0) };
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| StoreError::Unavailable)?;
        let mut moved = 0u64;
        for (table, column) in keyed_columns(&tx)? {
            let changed = tx.execute(&format!("UPDATE {} SET {col} = ?2 WHERE {col} = ?1", quoted(&table), col = quoted(&column)),
                [old, new]).map_err(|_| StoreError::Unavailable)?;
            moved += changed as u64;
        }
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(moved)
    }

    pub(crate) fn purge(home: &Path, file: &str, parent: Option<&Parent>, label: &str) -> Result<u64, StoreError> {
        let Some(mut db) = open(home, file, Access::ReadWrite)? else { return Ok(0) };
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| StoreError::Unavailable)?;
        let mut removed = 0u64;
        if let Some(parent) = parent {
            for table in tables_with(&tx, parent.child, parent.table)? {
                let sql = format!("DELETE FROM {} WHERE {} IN (SELECT {} FROM {} WHERE {} = ?1)", quoted(&table),
                    quoted(parent.child), quoted(parent.id), quoted(parent.table), quoted(parent.label));
                match tx.execute(&sql, [label]) {
                    Ok(changed) => removed += changed as u64,
                    // Append-only tombstone tables (e.g. task authority
                    // epochs) refuse deletes by trigger; they hold no label
                    // and no content, so they stay.
                    Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::ConstraintViolation => {
                        tracing::info!(table = %table, "label purge: append-only child rows kept");
                    }
                    Err(_) => return Err(StoreError::Unavailable),
                }
            }
        }
        for (table, column) in keyed_columns(&tx)? {
            let changed = tx.execute(&format!("DELETE FROM {} WHERE {} = ?1", quoted(&table), quoted(&column)), [label])
                .map_err(|_| StoreError::Unavailable)?;
            removed += changed as u64;
        }
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(removed)
    }

    /// Pause the label's enabled rows of `table` (`enabled = 1 → 0`), returning
    /// their ids. Used for cron at tombstone (reason `employee_tombstoned`).
    pub(crate) fn pause(home: &Path, file: &str, parent: &Parent, label: &str) -> Result<Vec<String>, StoreError> {
        let Some(mut db) = open(home, file, Access::ReadWrite)? else { return Ok(Vec::new()) };
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| StoreError::Unavailable)?;
        let (table, id, column) = (quoted(parent.table), quoted(parent.id), quoted(parent.label));
        let ids = {
            let mut query = tx.prepare(&format!("SELECT {id} FROM {table} WHERE {column} = ?1 AND enabled = 1 ORDER BY {id}"))
                .map_err(|_| StoreError::Unavailable)?;
            let ids = query.query_map([label], |r| r.get::<_, String>(0)).map_err(|_| StoreError::Unavailable)?
                .collect::<Result<Vec<_>, _>>().map_err(|_| StoreError::Unavailable)?;
            ids
        };
        tx.execute(&format!("UPDATE {table} SET enabled = 0, last_error = 'employee_tombstoned' WHERE {column} = ?1 AND enabled = 1"),
            [label]).map_err(|_| StoreError::Unavailable)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(ids)
    }
}

// ── Tombstone (§3.2 step 4, S9) ─────────────────────────────────────────

/// Run every store's tombstone action for `label` and record the result in
/// the quarantine row `(employee, incarnation)`: `Strip` stores are detached
/// (names merged into `stripped.global_routes`), `Pause` stores paused,
/// `Gate`/`Pause` stores counted into `store_marks`. A store that fails
/// becomes a `stores:` pending fence (blocks every restore until a retry
/// clears it). Idempotent: a retry merges names and keeps the first marks.
/// Returns the audit summary.
pub(crate) fn tombstone_effects(home: &Path, label: &str, employee: EmployeeId, incarnation: u32) -> Value {
    let (mut marks, mut routes, mut paused, mut fences) = (Map::new(), Map::new(), Map::new(), Vec::new());
    for store in LABEL_KEYED_STORES {
        match store.action {
            TombstoneAction::Digest => {}
            TombstoneAction::Strip => match store.detach(home, label) {
                Ok(names) => { routes.insert(store.name.into(), json!(names)); }
                Err(_) => fences.push(format!("{STORE_FENCE_GROUP}{}:strip_unavailable", store.name)),
            },
            TombstoneAction::Gate | TombstoneAction::Pause => {
                if let (TombstoneAction::Pause, Backend::Sqlite { parent: Some(parent) }) = (store.action, &store.backend) {
                    match sqlite::pause(home, store.source, parent, label) {
                        Ok(ids) => { paused.insert(store.name.into(), json!(ids)); }
                        Err(_) => fences.push(format!("{STORE_FENCE_GROUP}{}:pause_unavailable", store.name)),
                    }
                }
                match store.count_under(home, label) {
                    Ok(count) => { marks.insert(store.name.into(), json!(count)); }
                    Err(_) => fences.push(format!("{STORE_FENCE_GROUP}{}:unreadable", store.name)),
                }
            }
        }
    }
    fences.sort();
    let recorded = record_effects(home, employee, incarnation, &marks, &routes, &paused, &fences).is_ok();
    if !recorded { tracing::warn!(agent = label, "tombstone: label store effects not recorded on the quarantine row"); }
    json!({"store_marks": marks, "global_routes": routes, "paused": paused, "fences": fences, "recorded": recorded})
}

fn merge_names(existing: Option<&Value>, new: &Value) -> Value {
    let mut all: Vec<String> = existing.and_then(Value::as_array).into_iter().flatten()
        .chain(new.as_array().into_iter().flatten()).filter_map(|v| v.as_str().map(str::to_owned)).collect();
    all.sort();
    all.dedup();
    json!(all)
}

fn record_effects(home: &Path, employee: EmployeeId, incarnation: u32, marks: &Map<String, Value>,
    routes: &Map<String, Value>, paused: &Map<String, Value>, fences: &[String]) -> Result<(), StoreError> {
    let mut db = open_apply(home, Access::ReadWrite)?;
    {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| StoreError::Unavailable)?;
        let row: Option<(String, Option<String>)> = tx.query_row(
            "SELECT stripped,store_marks FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(employee.0), incarnation], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional().map_err(|_| StoreError::Unavailable)?;
        let (stripped, stored_marks) = row.ok_or(StoreError::Unavailable)?;
        let mut stripped: Value = serde_json::from_str(&stripped).map_err(|_| StoreError::Unavailable)?;
        let mut stored_marks: Map<String, Value> = match stored_marks {
            Some(text) => serde_json::from_str(&text).map_err(|_| StoreError::Unavailable)?,
            None => Map::new(),
        };
        let object = stripped.as_object_mut().ok_or(StoreError::Unavailable)?;
        for (key, names) in [("global_routes", routes), ("paused_work", paused)] {
            let mut merged = object.get(key).and_then(Value::as_object).cloned().unwrap_or_default();
            for (store, new) in names { let value = merge_names(merged.get(store), new); merged.insert(store.clone(), value); }
            object.insert(key.into(), Value::Object(merged));
        }
        object.insert("deferred".into(), json!(DEFERRED_ROUTES));
        object.insert("in_tree".into(), json!(IN_TREE_STRIPPED));
        // The first readable mark of each store is kept (writes are gated since).
        for (store, mark) in marks { stored_marks.entry(store.clone()).or_insert_with(|| mark.clone()); }
        tx.execute("UPDATE employee_quarantine SET stripped=?3,store_marks=?4 WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(employee.0), incarnation, stripped.to_string(), Value::Object(stored_marks).to_string()])
            .map_err(|_| StoreError::Unavailable)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
    }
    crate::isolation_quarantine::replace_fence_group(&mut db, employee, incarnation, STORE_FENCE_GROUP, fences)
        .map_err(|_| StoreError::Unavailable)
}

// ── Proposals (R7, §3.10 step 7) ────────────────────────────────────────

/// Staging, approving or applying an Employee proposal for `label` refuses
/// `Quarantined` while a quarantine row on it is not closed (S7), unless it
/// is the row `adopting` names (the adoption proposal of that row, F6g). A
/// label whose rows were purged and that no live row holds frees only with a
/// positive zero count from every store (S10, §3.6 "finish the purge first"),
/// except the `Digest` store: the org entry is the one the proposal itself
/// states (the propose flow writes it before staging).
pub(crate) fn check_label_not_quarantined(home: &Path, label: &str, adopting: Option<(EmployeeId, u32)>)
    -> Result<(), GateRefusal> {
    let hold = label_hold(home, label).map_err(|_| GateRefusal::Unavailable)?;
    let adopted = adopting.map(|(employee, incarnation)| (hex::encode(employee.0), incarnation));
    if hold.open_rows.iter().any(|(hex, incarnation, _)| adopted.as_ref() != Some(&(hex.clone(), *incarnation))) {
        return Err(GateRefusal::Quarantined);
    }
    if hold.purged > 0 && hold.confirmed != Some(false) {
        match label_free_where(home, label, |store| store.action != TombstoneAction::Digest) {
            Ok(true) => {}
            Ok(false) => return Err(GateRefusal::Quarantined),
            Err(_) => return Err(GateRefusal::Unavailable),
        }
    }
    Ok(())
}

/// The RPC form: the refusal text for `prepare_propose` / `stage_prepared`.
pub(crate) fn proposal_refusal(home: &Path, label: &str) -> Result<(), &'static str> {
    check_label_not_quarantined(home, label, None).map_err(GateRefusal::message)
}

// ── Channel bots (H5, A11) ──────────────────────────────────────────────

/// Whether a per-employee bot of `label` on `channel` may start. Outside
/// protected mode: always (unchanged). In protected mode: no open quarantine
/// row, a live lifecycle and an active catalog route (provider `channel`)
/// whose C6 detail names this employee, this label, this channel and the
/// employee's own token. Any read failure refuses.
pub(crate) async fn channel_bot_allowed(home: &Path, label: &str, channel: &'static str) -> bool {
    if !protected_active() { return true; }
    let (home, label) = (home.to_path_buf(), label.to_owned());
    tokio::task::spawn_blocking(move || bot_gate(&home, &label, channel, &crate::isolation_lifecycle::RootAllocator))
        .await.unwrap_or(false)
}

/// The protected-mode decision of [`channel_bot_allowed`] (blocking).
pub(crate) fn bot_gate(home: &Path, label: &str, channel: &str, root: &dyn crate::isolation_lifecycle::RootAllocation) -> bool {
    match label_hold(home, label) {
        Ok(hold) if hold.open_rows.is_empty() => {}
        _ => return false,
    }
    let Ok(lifecycle) = crate::isolation_lifecycle::read_lifecycle_with(home, label, root) else { return false };
    live_channel_route(home, &lifecycle, channel).unwrap_or(false)
}

fn live_channel_route(home: &Path, lifecycle: &crate::isolation_lifecycle::EmployeeLifecycle, channel: &str) -> Result<bool, ()> {
    use crate::isolation_connector_details::{ChannelTokenRef, DetailReference};
    use crate::isolation_connectors::{ConnectorCatalog, CATALOG_FILE};
    match std::fs::symlink_metadata(home.join(CATALOG_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
        Ok(_) => {}
    }
    let catalog = ConnectorCatalog::open(home).map_err(|_| ())?;
    for route in catalog.active_routes_for(&lifecycle.binding).map_err(|_| ())? {
        if route.provider != "channel" { continue; }
        let Some((current, Some(record))) = catalog.route_details(&route.account).map_err(|_| ())? else { continue };
        if !current.active || current.generation != route.generation { continue; }
        if record.employee != lifecycle.employee || record.agent != lifecycle.label { continue; }
        if let DetailReference::Channel { channel: kind, token: ChannelTokenRef::Agent, .. } = &record.reference {
            if kind == channel { return Ok(true); }
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "isolation_label_stores_tests.rs"]
mod tests;
