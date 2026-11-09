//! Tombstone quarantine of protected employees (native Linux isolation
//! v1.81, F6 design r5 §3.1–§3.2, tasks F6a/F6b).
//!
//! This module owns:
//! - the apply.db employee schema (`confirmed_employees` incl. the F6
//!   `incarnation` column, `employee_quarantine` keyed by
//!   `(employee_hex, incarnation)`, `root_allocations(employee_hex PK)`),
//!   idempotent, versioned in `employee_schema_version`, refusing a schema
//!   newer than this build ([`ensure_employee_schema`], [`check_readable`]);
//! - the quarantine row writers used by `isolation_lifecycle::tombstone_employee`;
//! - the `agent.toml` strip done before the move (`[channels.*]` bot
//!   sections, H5, and the label-keyed `[capabilities] db_sources` grants;
//!   names recorded, token values never);
//! - the seal: a descriptor walk of the quarantined tree through a held
//!   directory fd ([`seal`], M1/M2), run outside every database lock;
//! - the run-stop seam ([`RunStopper`], S13) and the C6 detail-route
//!   revocation ([`revoke_employee_detail_routes`], H2).
//!
//! Everything here is blocking (SQLite, filesystem) and runs on the
//! blocking pool.
use std::path::Path;

use duduclaw_isolation::ids::EmployeeId;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// apply.db employee schema version written by this build.
pub(crate) const EMPLOYEE_SCHEMA_VERSION: i64 = 1;
/// Domain tag of the quarantine content digest (M2).
pub(crate) const DIGEST_DOMAIN: &[u8] = b"duduclaw-quarantine-v1\0";
/// Seal bounds (§3.2 step 2): anything larger is `oversize`.
pub(crate) const MAX_ENTRIES: usize = 65_536;
pub(crate) const MAX_BYTES: u64 = 1 << 30;
pub(crate) const MAX_DEPTH: usize = 16;
/// Largest agent.toml the pre-move strip reads.
const MAX_AGENT_TOML: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchemaError {
    /// The store could not be read or written.
    Storage,
    /// The store carries an employee schema newer than this build.
    Future,
}

const CONFIRMED_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS confirmed_employees (
    label TEXT PRIMARY KEY,operation TEXT NOT NULL UNIQUE,digest BLOB NOT NULL,
    receipt TEXT NOT NULL,tenant_id TEXT);";

/// The F6 tables. `seal` is the quarantine state machine of §3.4
/// (`pending → sealed | oversize | invalid | absent`, later states written by
/// F6c/F6g/F6i). The root allocation is per employee id, not per row (S7).
const F6_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS employee_schema_version (
        id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS employee_quarantine (
        employee_hex TEXT NOT NULL CHECK (length(employee_hex) = 32 AND employee_hex NOT GLOB '*[^0-9a-f]*'),
        incarnation INTEGER NOT NULL CHECK (incarnation >= 0 AND incarnation <= 4294967295),
        label TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('personal', 'shared')),
        owner_user_id TEXT,
        tenant_id TEXT NOT NULL,
        quarantined_at INTEGER NOT NULL,
        dev INTEGER,
        ino INTEGER,
        digest TEXT,
        stat_manifest TEXT,
        seal TEXT NOT NULL CHECK (seal IN ('pending', 'sealed', 'oversize', 'invalid', 'absent', 'restoring',
            'restored', 'adopting', 'adopted', 'needs_reseal', 'purging', 'purged')),
        principal_snapshot TEXT NOT NULL,
        pending_fences TEXT NOT NULL DEFAULT '[]',
        stripped TEXT NOT NULL DEFAULT '{}',
        store_marks TEXT,
        closed_by_request TEXT,
        adopted_by_employee TEXT,
        early_purge TEXT,
        purged_at INTEGER,
        PRIMARY KEY (employee_hex, incarnation),
        CHECK ((kind = 'personal') = (owner_user_id IS NOT NULL)));
    CREATE INDEX IF NOT EXISTS idx_employee_quarantine_label ON employee_quarantine(label);
    CREATE TABLE IF NOT EXISTS root_allocations (
        employee_hex TEXT PRIMARY KEY CHECK (length(employee_hex) = 32 AND employee_hex NOT GLOB '*[^0-9a-f]*'),
        root_item TEXT NOT NULL,
        root_state TEXT NOT NULL CHECK (root_state IN ('allocated', 'retiring', 'retired', 'root_absent')),
        retire_proposal_id TEXT,
        retired_at INTEGER);";

/// The recorded employee schema version (`None` before F6).
pub(crate) fn schema_version(db: &Connection) -> Result<Option<i64>, SchemaError> {
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='employee_schema_version')",
        [], |r| r.get(0)).map_err(|_| SchemaError::Storage)?;
    if !table { return Ok(None); }
    db.query_row("SELECT version FROM employee_schema_version WHERE id=1", [], |r| r.get(0))
        .optional().map_err(|_| SchemaError::Storage)
}

/// Read paths: refuse a store written by a newer build (its rows may carry
/// meaning this build cannot check).
pub(crate) fn check_readable(db: &Connection) -> Result<(), SchemaError> {
    match schema_version(db)? {
        Some(version) if version > EMPLOYEE_SCHEMA_VERSION => Err(SchemaError::Future),
        _ => Ok(()),
    }
}

fn columns(db: &Connection, table: &str) -> Result<Vec<String>, SchemaError> {
    let mut statement = db.prepare(&format!("PRAGMA table_info({table})")).map_err(|_| SchemaError::Storage)?;
    let names = statement.query_map([], |r| r.get::<_, String>(1)).map_err(|_| SchemaError::Storage)?
        .collect::<Result<Vec<_>, _>>().map_err(|_| SchemaError::Storage)?;
    Ok(names)
}

/// Whether `confirmed_employees.incarnation` exists (F6m/F6b readers treat a
/// missing column as incarnation 0 for every row).
pub(crate) fn has_incarnation(db: &Connection) -> Result<bool, SchemaError> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('confirmed_employees') WHERE name='incarnation')",
        [], |r| r.get(0)).map_err(|_| SchemaError::Storage)
}

/// Create or upgrade the apply.db employee schema, idempotently, in one
/// IMMEDIATE transaction. Adds `tenant_id` (pre-v1.81 rows stay unpinned)
/// and `incarnation INTEGER NOT NULL DEFAULT 0` (every existing row reads 0:
/// no release shipped a restore). Refuses ([`SchemaError::Future`]) a store
/// whose recorded version is newer than [`EMPLOYEE_SCHEMA_VERSION`].
pub(crate) fn ensure_employee_schema(db: &Connection) -> Result<(), SchemaError> {
    // Outside the transaction: SQLite ignores this pragma inside one.
    db.execute_batch("PRAGMA synchronous=FULL;").map_err(|_| SchemaError::Storage)?;
    let tx = rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(|_| SchemaError::Storage)?;
    check_readable(&tx)?;
    tx.execute_batch(CONFIRMED_SCHEMA).map_err(|_| SchemaError::Storage)?;
    let existing = columns(&tx, "confirmed_employees")?;
    if !existing.iter().any(|c| c == "tenant_id") {
        tx.execute_batch("ALTER TABLE confirmed_employees ADD COLUMN tenant_id TEXT;").map_err(|_| SchemaError::Storage)?;
    }
    if !existing.iter().any(|c| c == "incarnation") {
        tx.execute_batch("ALTER TABLE confirmed_employees ADD COLUMN incarnation INTEGER NOT NULL DEFAULT 0;")
            .map_err(|_| SchemaError::Storage)?;
    }
    tx.execute_batch(F6_SCHEMA).map_err(|_| SchemaError::Storage)?;
    tx.execute("INSERT INTO employee_schema_version(id,version) VALUES(1,?1)
        ON CONFLICT(id) DO UPDATE SET version=excluded.version WHERE version<excluded.version",
        [EMPLOYEE_SCHEMA_VERSION]).map_err(|_| SchemaError::Storage)?;
    tx.commit().map_err(|_| SchemaError::Storage)
}

/// Quarantine directory name of one row: `<hex>-<incarnation>-<label>` (M4).
pub(crate) fn quarantine_name(label: &str, employee: EmployeeId, incarnation: u32) -> String {
    format!("{}-{incarnation}-{label}", hex::encode(employee.0))
}

/// What the tombstone writes for one quarantine row (§3.2).
pub(crate) struct NewQuarantine<'a> {
    pub(crate) employee: EmployeeId,
    pub(crate) incarnation: u32,
    pub(crate) label: &'a str,
    pub(crate) owner: Option<&'a str>,
    pub(crate) tenant: &'a str,
    pub(crate) quarantined_at: i64,
    pub(crate) root: Option<(u64, u64)>,
    pub(crate) seal: &'static str,
    pub(crate) principal_snapshot: &'a Value,
    pub(crate) stripped: &'a Value,
}

/// Insert the row once; a retried tombstone keeps the first row (and its
/// principal snapshot, taken before users.db was touched). `Ok(true)` when
/// this call created it. Only the `(employee_hex, incarnation)` key conflict
/// is ignored (`ON CONFLICT … DO NOTHING`): `INSERT OR IGNORE` would also
/// silently skip CHECK violations (unknown seal state, bad hex), turning a
/// refused row into `Ok(false)`.
pub(crate) fn insert_quarantine(tx: &Connection, row: &NewQuarantine<'_>) -> Result<bool, rusqlite::Error> {
    let (dev, ino) = row.root.map_or((None, None), |(dev, ino)| (Some(dev as i64), Some(ino as i64)));
    let changed = tx.execute(
        "INSERT INTO employee_quarantine(employee_hex,incarnation,label,kind,owner_user_id,tenant_id,
            quarantined_at,dev,ino,seal,principal_snapshot,stripped)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(employee_hex,incarnation) DO NOTHING",
        params![hex::encode(row.employee.0), row.incarnation, row.label,
            if row.owner.is_some() { "personal" } else { "shared" }, row.owner, row.tenant, row.quarantined_at,
            dev, ino, row.seal, row.principal_snapshot.to_string(), row.stripped.to_string()])?;
    Ok(changed == 1)
}

/// Write the root allocation of `employee` at its first tombstone (S7, R12):
/// the Applied allocation item and the receipt, so a retire can be staged
/// whatever later happens to the confirmed row. Later tombstones of the same
/// id (after a restore) keep the first row.
pub(crate) fn insert_root_allocation(tx: &Connection, employee: EmployeeId, root_item: &Value) -> Result<bool, rusqlite::Error> {
    let changed = tx.execute(
        "INSERT INTO root_allocations(employee_hex,root_item,root_state) VALUES(?1,?2,'allocated')
         ON CONFLICT(employee_hex) DO NOTHING",
        params![hex::encode(employee.0), root_item.to_string()])?;
    Ok(changed == 1)
}

/// One quarantine row as stored (tests, RPC result, later tasks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuarantineRow {
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) owner: Option<String>,
    pub(crate) tenant: String,
    pub(crate) root: Option<(u64, u64)>,
    pub(crate) digest: Option<String>,
    pub(crate) stat_manifest: Option<Value>,
    pub(crate) seal: String,
    pub(crate) principal_snapshot: Value,
    pub(crate) pending_fences: Vec<String>,
    pub(crate) stripped: Value,
}

pub(crate) fn read_quarantine(db: &Connection, employee: EmployeeId, incarnation: u32)
    -> Result<Option<QuarantineRow>, rusqlite::Error> {
    type Raw = (String, String, Option<String>, String, Option<i64>, Option<i64>, Option<String>, Option<String>,
        String, String, String, String);
    let row: Option<Raw> = db.query_row(
        "SELECT label,kind,owner_user_id,tenant_id,dev,ino,digest,stat_manifest,seal,principal_snapshot,
            pending_fences,stripped FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(employee.0), incarnation],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
            r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?))).optional()?;
    let invalid = |_: serde_json::Error| rusqlite::Error::InvalidQuery;
    row.map(|(label, kind, owner, tenant, dev, ino, digest, manifest, seal, snapshot, fences, stripped)|
        -> Result<QuarantineRow, rusqlite::Error> {
        Ok(QuarantineRow { label, kind, owner, tenant,
            root: dev.zip(ino).map(|(dev, ino)| (dev as u64, ino as u64)), digest,
            stat_manifest: manifest.map(|m| serde_json::from_str(&m)).transpose().map_err(invalid)?,
            seal, principal_snapshot: serde_json::from_str(&snapshot).map_err(invalid)?,
            pending_fences: serde_json::from_str(&fences).map_err(invalid)?,
            stripped: serde_json::from_str(&stripped).map_err(invalid)? })
    }).transpose()
}

/// The short transaction that writes a seal result (§3.2 step 2). Only a
/// `pending` row changes; `Ok(false)` when it was already sealed.
pub(crate) fn record_seal(db: &mut Connection, employee: EmployeeId, incarnation: u32, seal: &Sealed)
    -> Result<bool, rusqlite::Error> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE employee_quarantine SET seal=?3,digest=?4,stat_manifest=?5
         WHERE employee_hex=?1 AND incarnation=?2 AND seal='pending'",
        params![hex::encode(employee.0), incarnation, seal.state.as_str(), seal.digest,
            seal.stat_manifest.as_ref().map(Value::to_string)])?;
    tx.commit()?;
    Ok(changed == 1)
}

/// Pending fences (H2) are grouped by prefix: `runs:` (S13 run stop),
/// `route` (`route:<account>:…` and `routes:…`, C6 detail routes) and
/// `approvals:` (broker invalidation, run by the RPC after the tombstone).
/// Each step replaces exactly its own group with the failures of its latest
/// attempt (an empty list clears it), so a successful retry removes its
/// fence and never another step's. The set stays sorted and unique; an
/// empty set means fully fenced. A missing row is an error.
pub(crate) fn replace_fence_group(db: &mut Connection, employee: EmployeeId, incarnation: u32, group: &str,
    fences: &[String]) -> Result<(), rusqlite::Error> {
    if fences.iter().any(|fence| !fence.starts_with(group)) { return Err(rusqlite::Error::InvalidQuery); }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: String = tx.query_row(
        "SELECT pending_fences FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(employee.0), incarnation], |r| r.get(0))?;
    let mut all: Vec<String> = serde_json::from_str(&current).map_err(|_| rusqlite::Error::InvalidQuery)?;
    all.retain(|fence| !fence.starts_with(group));
    all.extend(fences.iter().cloned());
    all.sort();
    all.dedup();
    tx.execute("UPDATE employee_quarantine SET pending_fences=?3 WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(employee.0), incarnation, json!(all).to_string()])?;
    tx.commit()
}

// ── agent.toml strip (H5, §3.3) ─────────────────────────────────────────

/// What the pre-move strip removed from `agent.toml`: names only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Stripped {
    /// `[channels.<name>]` sections (per-agent bots and their tokens).
    pub(crate) channels: Vec<String>,
    /// `[capabilities] db_sources` grants (label-keyed `[db_sources]` access).
    pub(crate) db_sources: Vec<String>,
    /// `absent`, `unchanged`, `stripped`, `unparsed` or `not_a_directory`.
    pub(crate) agent_toml: &'static str,
}

impl Stripped {
    pub(crate) fn status(status: &'static str) -> Self { Self { agent_toml: status, ..Self::default() } }
    /// The `stripped` column. Website credential grants
    /// (`website_credentials.allowed_agents`) and global routes are F6h's
    /// `LABEL_KEYED_STORES` (not stripped here; listed as `deferred`).
    pub(crate) fn to_value(&self) -> Value {
        json!({"channels": self.channels, "db_sources": self.db_sources, "agent_toml": self.agent_toml,
            "deferred": ["website_credentials.allowed_agents", "global_routes"]})
    }
}

/// Remove every `[channels.*]` section and the `[capabilities] db_sources`
/// grant list from agent.toml text, format-preserving. `Ok(None)` when there
/// was nothing to remove; `Err` when the text is not TOML.
pub(crate) fn strip_agent_toml(text: &str) -> Result<(Option<String>, Stripped), ()> {
    #[allow(unused_imports)]
    use toml_edit::TableLike;
    let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|_| ())?;
    let mut stripped = Stripped::status("unchanged");
    if let Some(channels) = doc.as_table_mut().remove("channels") {
        if let Some(table) = channels.as_table_like() {
            stripped.channels = table.iter().map(|(name, _)| name.to_owned()).collect();
        }
        stripped.channels.sort();
        stripped.agent_toml = "stripped";
    }
    if let Some(capabilities) = doc.get_mut("capabilities").and_then(|item| item.as_table_like_mut()) {
        if let Some(grants) = capabilities.remove("db_sources") {
            stripped.db_sources = grants.as_array().map(|array| array.iter()
                .filter_map(|value| value.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            stripped.agent_toml = "stripped";
        }
    }
    if stripped.agent_toml == "stripped" { Ok((Some(doc.to_string()), stripped)) } else { Ok((None, stripped)) }
}

/// Strip `agents/<label>/agent.toml` in place through the pinned `agents`
/// directory (no symlink followed; atomic replace). Never fails the
/// tombstone for a file it cannot parse: that is recorded (`unparsed`), and
/// the registry refuses to load such a file anyway.
#[cfg(unix)]
pub(crate) fn strip_agent_dir(agents: &crate::fs_safe::SafeDir, label: &str) -> Result<Stripped, std::io::Error> {
    use crate::fs_safe::EntryKind;
    match agents.entry(label)? {
        None => return Ok(Stripped::status("absent")),
        Some(entry) if entry.kind != EntryKind::Dir => return Ok(Stripped::status("not_a_directory")),
        Some(_) => {}
    }
    let Some(dir) = agents.child_dir(label, false)? else { return Ok(Stripped::status("absent")) };
    let Some(bytes) = dir.read_bytes_checked("agent.toml", MAX_AGENT_TOML, true)? else {
        return Ok(Stripped::status("absent"));
    };
    let Ok(text) = String::from_utf8(bytes) else { return Ok(Stripped::status("unparsed")) };
    match strip_agent_toml(&text) {
        Err(()) => Ok(Stripped::status("unparsed")),
        Ok((None, stripped)) => Ok(stripped),
        Ok((Some(updated), stripped)) => {
            dir.write_file_atomic("agent.toml", &updated)?;
            Ok(stripped)
        }
    }
}

// ── Run stop (S13) ──────────────────────────────────────────────────────

/// Stops every live run of a tombstoned employee and confirms the stop
/// (no run record, no unit). `Ok(ids)` = confirmed stopped (possibly none
/// were live); `Err(())` = not confirmed, recorded as the pending fence
/// [`RUN_STOP_FENCE`] (new runs already refuse on the disabled row).
pub(crate) trait RunStopper {
    fn stop_employee_runs(&self, employee: EmployeeId) -> Result<Vec<String>, ()>;
}

/// Pending fence recorded while the run stop is unconfirmed.
pub(crate) const RUN_STOP_FENCE: &str = "runs:stop_unconfirmed";

/// Production stopper. spawnd has no cancel-by-employee (or list-runs-of-
/// employee) request yet — only `SpawnRequest::Cancel { run }` for a run
/// handle the caller already holds — so this fails closed: the tombstone
/// completes (row disabled, nothing new may start) and records
/// [`RUN_STOP_FENCE`], which blocks every restore until a later retry
/// confirms the stop. F6l/F6c add the spawnd request (stop-and-confirm by
/// employee) and replace this body.
pub(crate) struct SpawndRunStopper;
impl RunStopper for SpawndRunStopper {
    fn stop_employee_runs(&self, employee: EmployeeId) -> Result<Vec<String>, ()> {
        tracing::warn!(employee = %hex::encode(employee.0),
            "employee tombstone: spawnd has no stop-by-employee request yet; live runs recorded as a pending fence");
        Err(())
    }
}

// ── C6 detail routes (H2) ───────────────────────────────────────────────

/// Revoke every catalog route whose C6 detail row names `employee`. Tenant
/// routes without such a row (SharedTenant/SharedHome used by other
/// employees) are never touched. Returns (revoked account ids, pending
/// fences). No catalog file means nothing was ever provisioned.
pub(crate) fn revoke_employee_detail_routes(home: &Path, employee: EmployeeId, actor: &str) -> (Vec<String>, Vec<String>) {
    use crate::isolation_connector_lifecycle::{self as connectors, LifecycleError as ConnectorError};
    use crate::isolation_connectors::{AccountId, ConnectorCatalog, CATALOG_FILE};
    match std::fs::symlink_metadata(home.join(CATALOG_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), Vec::new()),
        Err(_) => return (Vec::new(), vec!["routes:catalog_unavailable".into()]),
        Ok(_) => {}
    }
    let Ok(actor) = connectors::AdminActor::verified(actor) else {
        return (Vec::new(), vec!["routes:actor_invalid".into()]);
    };
    let named = (|| -> Result<Vec<AccountId>, ()> {
        let catalog = ConnectorCatalog::open(home).map_err(|_| ())?;
        let mut named = Vec::new();
        let mut after: Option<AccountId> = None;
        loop {
            let page = catalog.list(after.as_ref(), 500).map_err(|_| ())?;
            let Some(last) = page.last().map(|route| route.account.clone()) else { break };
            for route in page {
                if !route.active { continue; }
                if let Some((_, Some(record))) = catalog.route_details(&route.account).map_err(|_| ())? {
                    if record.employee == employee { named.push(route.account.clone()); }
                }
            }
            after = Some(last);
        }
        Ok(named)
    })();
    let Ok(named) = named else { return (Vec::new(), vec!["routes:catalog_unavailable".into()]) };
    let (mut revoked, mut fences) = (Vec::new(), Vec::new());
    for account in named {
        match connectors::revoke(home, &account, &actor) {
            Ok(_) => revoked.push(account.as_str().to_owned()),
            Err(ConnectorError::MaterialPending) => {
                revoked.push(account.as_str().to_owned());
                fences.push(format!("route:{}:material_pending", account.as_str()));
            }
            Err(_) => fences.push(format!("route:{}:unavailable", account.as_str())),
        }
    }
    (revoked, fences)
}

// ── Seal (M1, M2) ───────────────────────────────────────────────────────

/// Quarantine state written by the seal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SealState { Sealed, Oversize, Invalid, Absent }
impl SealState {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::Sealed => "sealed", Self::Oversize => "oversize", Self::Invalid => "invalid", Self::Absent => "absent" }
    }
}

/// A seal result: the content digest (hex) and the stat manifest
/// (`ino`, `size`, `ctime_ns` per entry, root `dev`/`ino`) for `sealed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sealed {
    pub(crate) state: SealState,
    pub(crate) digest: Option<String>,
    pub(crate) stat_manifest: Option<Value>,
    /// Why the tree is `invalid` (diagnostic, never shown to an employee).
    pub(crate) reason: Option<&'static str>,
}
impl Sealed {
    fn bare(state: SealState, reason: Option<&'static str>) -> Self {
        Self { state, digest: None, stat_manifest: None, reason }
    }
    pub(crate) fn absent() -> Self { Self::bare(SealState::Absent, None) }
}

/// The org.toml entry of the label, digested as the synthetic `org:<label>`
/// entry (H4).
fn org_entry(home: &Path, label: &str) -> Vec<u8> {
    match duduclaw_core::org_store::load(home).get(label) {
        Some(org) => [org.reports_to.as_bytes(), b"\0", org.department.as_bytes()].concat(),
        None => b"\0absent".to_vec(),
    }
}

fn put(hash: &mut Sha256, tag: &str, bytes: &[u8]) {
    hash.update((tag.len() as u64).to_be_bytes());
    hash.update(tag.as_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// One manifest entry (M2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Raw path bytes relative to the quarantine root, `/`-separated.
    pub(crate) path: Vec<u8>,
    /// `f` regular file, `d` directory, `l` symlink.
    pub(crate) kind: u8,
    pub(crate) mode: u32,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) size: u64,
    pub(crate) nlink: u64,
    pub(crate) ino: u64,
    pub(crate) ctime_ns: i128,
    /// SHA-256 of a regular file, or the `readlink` bytes of a symlink.
    pub(crate) content: Vec<u8>,
}

/// The walked tree of one quarantine root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) root_dev: u64,
    pub(crate) root_ino: u64,
    pub(crate) root_mode: u32,
    /// Sorted by raw path bytes.
    pub(crate) entries: Vec<Entry>,
}

impl Manifest {
    /// Content digest: domain, label, root mode, every entry's M2 fields in
    /// raw-byte path order, then the synthetic `org:<label>` entry; every
    /// field length-prefixed (`put` framing).
    pub(crate) fn digest(&self, label: &str, org: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(DIGEST_DOMAIN);
        put(&mut hash, "label", label.as_bytes());
        put(&mut hash, "root.mode", &self.root_mode.to_be_bytes());
        put(&mut hash, "entries", &(self.entries.len() as u64).to_be_bytes());
        for entry in &self.entries {
            put(&mut hash, "path", &entry.path);
            put(&mut hash, "type", &[entry.kind]);
            put(&mut hash, "mode", &entry.mode.to_be_bytes());
            put(&mut hash, "uid", &entry.uid.to_be_bytes());
            put(&mut hash, "gid", &entry.gid.to_be_bytes());
            put(&mut hash, "size", &entry.size.to_be_bytes());
            put(&mut hash, "nlink", &entry.nlink.to_be_bytes());
            put(&mut hash, if entry.kind == b'l' { "readlink" } else { "sha256" }, &entry.content);
        }
        put(&mut hash, &format!("org:{label}"), org);
        hex::encode(hash.finalize())
    }

    /// The stat manifest a restore re-walks after the move (§3.5 b).
    pub(crate) fn stat_manifest(&self) -> Value {
        json!({
            "root": {"dev": self.root_dev, "ino": self.root_ino},
            "entries": self.entries.iter().map(|entry| json!([hex::encode(&entry.path), entry.ino, entry.size,
                entry.ctime_ns.to_string()])).collect::<Vec<_>>(),
        })
    }
}

/// Outcome of a walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Walked {
    Tree(Manifest),
    Oversize,
    Invalid(&'static str),
}

/// Seal one quarantined tree through `root`, a directory descriptor held
/// since the move, after checking it still is the moved directory
/// (`expected` root `(dev, ino)`). Runs outside every database lock (M1).
#[cfg(unix)]
pub(crate) fn seal(home: &Path, label: &str, root: &crate::fs_safe::SafeDir, expected: (u64, u64)) -> Sealed {
    #[cfg(test)]
    SEAL_HOOK.with(|hook| if let Some(hook) = hook.borrow_mut().as_mut() { hook() });
    let euid = nix::unistd::geteuid().as_raw();
    let fd = match root.duplicate_fd() { Ok(fd) => fd, Err(_) => return Sealed::bare(SealState::Invalid, Some("root_unreadable")) };
    match walk::walk(fd, euid, MAX_ENTRIES, MAX_BYTES, MAX_DEPTH) {
        Ok(Walked::Tree(manifest)) => {
            if (manifest.root_dev, manifest.root_ino) != expected {
                return Sealed::bare(SealState::Invalid, Some("root_replaced"));
            }
            let digest = manifest.digest(label, &org_entry(home, label));
            Sealed { state: SealState::Sealed, digest: Some(digest), stat_manifest: Some(manifest.stat_manifest()), reason: None }
        }
        Ok(Walked::Oversize) => Sealed::bare(SealState::Oversize, None),
        Ok(Walked::Invalid(reason)) => Sealed::bare(SealState::Invalid, Some(reason)),
        Err(_) => Sealed::bare(SealState::Invalid, Some("io_error")),
    }
}

/// F6 Q5 / N3: a tombstoned label stays reserved (and restorable) this many
/// days after the tombstone.
pub(crate) const LABEL_RESERVED_DAYS: u32 = 90;

/// What a tombstone of the live `agents/<label>` tree would seal now (F5
/// UI-RPC tombstone preview). `bytes` is the regular-file total the walk
/// counted; `complete = false` when the walk stopped early (oversize,
/// invalid), so `bytes` is then a lower bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SealPreview {
    pub(crate) state: SealState,
    pub(crate) bytes: u64,
    pub(crate) complete: bool,
}

/// Read-only preview of [`seal`] on the live tree with the seal's bounds.
#[cfg(unix)]
pub(crate) fn preview_live_tree(home: &Path, label: &str) -> Result<SealPreview, ()> {
    preview_live_tree_with(home, label, MAX_ENTRIES, MAX_BYTES, MAX_DEPTH)
}

/// [`preview_live_tree`] with explicit bounds (tests). Mirrors what the
/// tombstone would record: no `agents/<label>` ⇒ `absent`; a symlink or file
/// there (moved as-is, never sealed) ⇒ `invalid`; otherwise the seal's
/// descriptor walk (no-follow, same filesystem, owner, hardlinks, special
/// files, xattrs; entries/bytes/depth bounds) without hashing. Opens nothing
/// for writing and creates nothing (no quarantine directory, no row). The
/// pre-move `agent.toml` strip is not applied, so the estimate is a slight
/// over-estimate. `Err` = the home or `agents/` could not be read safely.
#[cfg(unix)]
pub(crate) fn preview_live_tree_with(home: &Path, label: &str, max_entries: usize, max_bytes: u64, max_depth: usize)
    -> Result<SealPreview, ()> {
    use crate::fs_safe::{EntryKind, SafeDir};
    let bare = |state| SealPreview { state, bytes: 0, complete: true };
    let home_dir = SafeDir::open_nofollow(home, false).map_err(|_| ())?;
    let Some(agents) = home_dir.child_dir("agents", false).map_err(|_| ())? else { return Ok(bare(SealState::Absent)) };
    match agents.entry(label).map_err(|_| ())? {
        None => return Ok(bare(SealState::Absent)),
        Some(entry) if entry.kind != EntryKind::Dir => return Ok(SealPreview { complete: false, ..bare(SealState::Invalid) }),
        Some(_) => {}
    }
    let root = agents.child_dir(label, false).map_err(|_| ())?.ok_or(())?;
    let fd = root.duplicate_fd().map_err(|_| ())?;
    let euid = nix::unistd::geteuid().as_raw();
    match walk::measure(fd, euid, max_entries, max_bytes, max_depth) {
        Ok((Walked::Tree(_), bytes)) => Ok(SealPreview { state: SealState::Sealed, bytes, complete: true }),
        Ok((Walked::Oversize, bytes)) => Ok(SealPreview { state: SealState::Oversize, bytes, complete: false }),
        Ok((Walked::Invalid(_), bytes)) => Ok(SealPreview { state: SealState::Invalid, bytes, complete: false }),
        // The seal would record `invalid` (io_error); the preview cannot
        // tell the size, so it refuses instead of guessing.
        Err(_) => Err(()),
    }
}

#[cfg(not(unix))]
pub(crate) fn preview_live_tree(_: &Path, _: &str) -> Result<SealPreview, ()> { Err(()) }

/// Test-only pause point at the start of [`seal`] (proves no lock is held).
#[cfg(test)]
thread_local! {
    pub(crate) static SEAL_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> = std::cell::RefCell::new(None);
}

#[cfg(unix)]
pub(crate) mod walk {
    //! Descriptor-relative tree walk: every child is `fstatat(NOFOLLOW)`ed
    //! and opened `O_NOFOLLOW` relative to its parent's held descriptor, and
    //! the opened descriptor must be the stat'ed inode. Never follows a
    //! symlink; never leaves the root's filesystem.
    use super::{Entry, Manifest, Walked};
    use nix::errno::Errno;
    use nix::fcntl::{openat, AtFlags, OFlag};
    use nix::sys::stat::{fstatat, FileStat, Mode, SFlag};
    use sha2::{Digest, Sha256};
    use std::io::{self, Read};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    fn to_io(error: Errno) -> io::Error { io::Error::from_raw_os_error(error as i32) }

    /// `hash = false` (F5 tombstone preview): the same walk, checks and
    /// bounds, without reading file contents.
    struct State { euid: u32, dev: u64, entries: Vec<Entry>, bytes: u64, max_entries: usize, max_bytes: u64, max_depth: usize,
        hash: bool }

    #[allow(clippy::unnecessary_cast)]
    fn ctime_ns(st: &FileStat) -> i128 { (st.st_ctime as i128) * 1_000_000_000 + st.st_ctime_nsec as i128 }

    /// Any extended attribute (incl. POSIX ACLs, which are xattrs) makes the
    /// tree `invalid`. `security.selinux` labels are the one exception: an
    /// SELinux host labels every file (verify on the Linux box).
    #[cfg(target_os = "linux")]
    fn has_xattr(fd: &OwnedFd) -> io::Result<bool> {
        let size = unsafe { libc::flistxattr(fd.as_raw_fd(), std::ptr::null_mut(), 0) };
        if size < 0 { return Err(io::Error::last_os_error()); }
        if size == 0 { return Ok(false); }
        let mut buffer = vec![0u8; size as usize];
        // SAFETY: the buffer is `size` bytes and outlives the call.
        let read = unsafe { libc::flistxattr(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 { return Err(io::Error::last_os_error()); }
        buffer.truncate(read as usize);
        Ok(buffer.split(|b| *b == 0).any(|name| !name.is_empty() && name != b"security.selinux"))
    }
    #[cfg(not(target_os = "linux"))]
    fn has_xattr(_fd: &OwnedFd) -> io::Result<bool> { Ok(false) }

    fn names(dir: &OwnedFd) -> io::Result<Vec<Vec<u8>>> {
        let raw = openat(Some(dir.as_raw_fd()), ".", OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC, Mode::empty())
            .map_err(to_io)?;
        // SAFETY: freshly returned descriptor; ownership moves to the DIR stream.
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };
        let stream = unsafe { libc::fdopendir(owned.into_raw_fd()) };
        if stream.is_null() { return Err(io::Error::last_os_error()); }
        let mut names = Vec::new();
        loop {
            // SAFETY: `stream` is a valid open stream until `closedir` below.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() { break; }
            // SAFETY: `d_name` is NUL-terminated per readdir(3).
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes().to_vec();
            if name != b"." && name != b".." { names.push(name); }
        }
        // SAFETY: `stream` came from `fdopendir` and is closed exactly once.
        unsafe { libc::closedir(stream) };
        names.sort();
        Ok(names)
    }

    fn open_child(dir: &OwnedFd, name: &[u8], directory: bool) -> io::Result<OwnedFd> {
        let mut flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK | OFlag::O_NOCTTY;
        if directory { flags |= OFlag::O_DIRECTORY; }
        let raw = openat(Some(dir.as_raw_fd()), std::ffi::OsStr::from_bytes(name), flags, Mode::empty()).map_err(to_io)?;
        // SAFETY: freshly returned descriptor, owned only here.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    fn readlink(dir: &OwnedFd, name: &[u8]) -> io::Result<Vec<u8>> {
        let path = std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let mut buffer = vec![0u8; 4097];
        // SAFETY: `path` is NUL-terminated; the buffer is writable for its length.
        let read = unsafe { libc::readlinkat(dir.as_raw_fd(), path.as_ptr(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 { return Err(io::Error::last_os_error()); }
        if read as usize >= buffer.len() { return Err(io::Error::from(io::ErrorKind::InvalidData)); }
        buffer.truncate(read as usize);
        Ok(buffer)
    }

    /// Hash a regular file through its descriptor, reading at most `size`
    /// bytes plus one (a file that grew under the walk is `None`).
    fn hash_file(fd: OwnedFd, size: u64) -> io::Result<Option<[u8; 32]>> {
        let mut file = std::fs::File::from(fd).take(size.saturating_add(1));
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 { break; }
            total += read as u64;
            hash.update(&buffer[..read]);
        }
        if total != size { return Ok(None); }
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash.finalize());
        Ok(Some(out))
    }

    #[allow(clippy::unnecessary_cast)]
    fn walk_dir(dir: &OwnedFd, prefix: &[u8], depth: usize, state: &mut State) -> io::Result<Option<Walked>> {
        for name in names(dir)? {
            let path = if prefix.is_empty() { name.clone() } else { [prefix, b"/", &name].concat() };
            if state.entries.len() >= state.max_entries { return Ok(Some(Walked::Oversize)); }
            let st = match fstatat(Some(dir.as_raw_fd()), std::ffi::OsStr::from_bytes(&name), AtFlags::AT_SYMLINK_NOFOLLOW) {
                Ok(st) => st,
                Err(Errno::ENOENT) => return Ok(Some(Walked::Invalid("raced"))),
                Err(error) => return Err(to_io(error)),
            };
            if st.st_dev as u64 != state.dev { return Ok(Some(Walked::Invalid("other_filesystem"))); }
            if st.st_uid as u32 != state.euid { return Ok(Some(Walked::Invalid("foreign_uid"))); }
            let kind = SFlag::from_bits_truncate(st.st_mode & SFlag::S_IFMT.bits());
            let mut entry = Entry { path: path.clone(), kind: 0, mode: st.st_mode as u32 & 0o7777, uid: st.st_uid as u32,
                gid: st.st_gid as u32, size: st.st_size.max(0) as u64, nlink: st.st_nlink as u64, ino: st.st_ino as u64,
                ctime_ns: ctime_ns(&st), content: Vec::new() };
            if kind == SFlag::S_IFREG {
                if entry.nlink > 1 { return Ok(Some(Walked::Invalid("hardlink"))); }
                state.bytes = state.bytes.saturating_add(entry.size);
                if state.bytes > state.max_bytes { return Ok(Some(Walked::Oversize)); }
                let fd = open_child(dir, &name, false)?;
                let meta = std::fs::File::from(fd.try_clone()?).metadata()?;
                if meta.ino() != entry.ino || meta.dev() != state.dev || !meta.is_file() || meta.nlink() > 1 {
                    return Ok(Some(Walked::Invalid("raced")));
                }
                if has_xattr(&fd)? { return Ok(Some(Walked::Invalid("xattr"))); }
                if state.hash {
                    let Some(sha) = hash_file(fd, entry.size)? else { return Ok(Some(Walked::Invalid("raced"))) };
                    entry.content = sha.to_vec();
                }
                entry.kind = b'f';
                state.entries.push(entry);
            } else if kind == SFlag::S_IFDIR {
                if depth + 1 > state.max_depth { return Ok(Some(Walked::Oversize)); }
                let fd = open_child(dir, &name, true)?;
                let meta = std::fs::File::from(fd.try_clone()?).metadata()?;
                if meta.ino() != entry.ino || meta.dev() != state.dev { return Ok(Some(Walked::Invalid("raced"))); }
                if has_xattr(&fd)? { return Ok(Some(Walked::Invalid("xattr"))); }
                entry.kind = b'd';
                state.entries.push(entry);
                if let Some(stop) = walk_dir(&fd, &path, depth + 1, state)? { return Ok(Some(stop)); }
            } else if kind == SFlag::S_IFLNK {
                entry.kind = b'l';
                entry.content = readlink(dir, &name)?;
                state.entries.push(entry);
            } else {
                // Sockets, FIFOs, devices.
                return Ok(Some(Walked::Invalid("special_file")));
            }
        }
        Ok(None)
    }

    /// Walk the tree under `root` (bounds: entries, regular-file bytes,
    /// depth). The root itself must be a directory owned by `euid`.
    pub(crate) fn walk(root: OwnedFd, euid: u32, max_entries: usize, max_bytes: u64, max_depth: usize) -> io::Result<Walked> {
        walk_with(root, euid, max_entries, max_bytes, max_depth, true).map(|(walked, _)| walked)
    }

    /// F5 tombstone preview: [`walk`] without hashing (no file content is
    /// read), plus the regular-file bytes counted when the walk ended — the
    /// whole tree's for a `Tree`, the running total at the stop otherwise
    /// (for `Oversize` by bytes it already exceeds `max_bytes`). Entries of a
    /// `Tree` carry an empty `content`, so its digest is meaningless.
    pub(crate) fn measure(root: OwnedFd, euid: u32, max_entries: usize, max_bytes: u64, max_depth: usize)
        -> io::Result<(Walked, u64)> {
        walk_with(root, euid, max_entries, max_bytes, max_depth, false)
    }

    fn walk_with(root: OwnedFd, euid: u32, max_entries: usize, max_bytes: u64, max_depth: usize, hash: bool)
        -> io::Result<(Walked, u64)> {
        let meta = std::fs::File::from(root.try_clone()?).metadata()?;
        if !meta.is_dir() { return Ok((Walked::Invalid("root_not_directory"), 0)); }
        if meta.uid() != euid { return Ok((Walked::Invalid("foreign_uid"), 0)); }
        if has_xattr(&root)? { return Ok((Walked::Invalid("xattr"), 0)); }
        let mut state = State { euid, dev: meta.dev(), entries: Vec::new(), bytes: 0, max_entries, max_bytes, max_depth, hash };
        if let Some(stop) = walk_dir(&root, b"", 0, &mut state)? { return Ok((stop, state.bytes)); }
        let mut entries = state.entries;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok((Walked::Tree(Manifest { root_dev: meta.dev(), root_ino: meta.ino(), root_mode: meta.mode() & 0o7777, entries }),
            state.bytes))
    }
}

#[cfg(test)]
#[path = "isolation_quarantine_tests.rs"]
mod tests;
