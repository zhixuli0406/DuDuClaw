//! Restore and purge of the label-keyed state a tombstone held (native Linux
//! isolation v1.81, F6 design r5 §3.3, §3.7, §3.8; task F6h, for F6c / F6i).
//!
//! At tombstone two `Strip` stores keep what a same-id restore needs, both
//! in the quarantine row's `stripped.restore.<store>` (exact entries) and,
//! for credentials, in the store itself:
//! - `website_credentials`: the label's grant on each record becomes a held
//!   grant `{agent: <label>, key: quarantine:<hex>-<inc>}` on that record
//!   ([`crate::website_credentials::hold_agent`]); the record is never
//!   deleted while held (a held-only record is valid but unusable). Entry:
//!   `{id, credential, agent, key}`.
//! - `delegation_targets`: each employee reporting to the label is moved to
//!   the root, department kept. Entry: `{agent, parent: <label>, department}`.
//!
//! [`restore_label_stores`] (the F6d decide RPC calls it after the restore
//! commit, §3.5 step 4) **only lists** (§3.3 "listed for admin re-grant /
//! re-enable, never automatic"): held credential grants and `reports_to`
//! edges become `regrant` entries — the held grants stay held, nothing is
//! deleted — and channel routes, unmodelled global routes, in-tree grants
//! and paused work are `listed` (§3.7 work items stay paused). An Admin
//! re-applies one `regrant` entry at a time with [`regrant_item`]
//! (`isolation.employee_restore_regrant`, Admin + step-up, audited): a held
//! grant is moved back, an edge is re-applied only if the child still exists
//! and still reports to the root.
//!
//! Purge (F6i): [`purge_row_stores`] = `purge_all` under the row key, which
//! deletes the held grants (and a credential left with no grant) for good.
use std::path::Path;

use duduclaw_isolation::ids::EmployeeId;
use serde_json::{json, Value};

use crate::isolation_label_stores::{purge_all, quarantine_key, StoreError};

/// What a store's tombstone hold detached: names for
/// `stripped.global_routes.<store>` and exact entries for
/// `stripped.restore.<store>`.
pub(crate) struct Held {
    pub(crate) names: Vec<String>,
    pub(crate) restore: Vec<Value>,
    /// `false`: the store failed part-way; what moved is still recorded
    /// (never lost) and the tombstone raises a `stores:` fence for a retry.
    pub(crate) complete: bool,
}

impl Held {
    pub(crate) fn names_only(names: Vec<String>) -> Self { Held { names, restore: Vec::new(), complete: true } }
}

/// `hold(home, label, key)`: detach `label`, keeping what a restore needs
/// under the quarantine row key `key`. Idempotent.
pub(crate) type HoldFn = fn(&Path, &str, &str) -> Result<Held, StoreError>;

// ── Tombstone holds ─────────────────────────────────────────────────────

pub(crate) fn website_hold(home: &Path, label: &str, key: &str) -> Result<Held, StoreError> {
    if label.is_empty() || key == label { return Err(StoreError::Unavailable); }
    let (moved, held) = crate::website_credentials::hold_agent(home, label, key).map_err(|_| StoreError::Unavailable)?;
    // Names: moved now (a re-key counts them). Entries: everything held under
    // the key, so a retry after a failed row write records them again.
    let mut names: Vec<String> = moved.into_iter().map(|(_, credential)| credential).collect();
    names.sort();
    let restore = held.into_iter()
        .map(|(id, credential)| json!({"id": id, "credential": credential, "agent": label, "key": key})).collect();
    // One locked re-seal of the whole store: all or nothing.
    Ok(Held { names, restore, complete: true })
}

/// Purge / adoption detach: grants of `key` removed, held grants under `key`
/// deleted, records left with nothing deleted.
pub(crate) fn website_purge(home: &Path, key: &str) -> Result<Vec<String>, StoreError> {
    crate::website_credentials::purge_agent(home, key).map_err(|_| StoreError::Unavailable)
}

/// S9 "other employees' delegation targets": re-root every employee whose
/// `reports_to` is the label (department kept), recording the exact edge.
pub(crate) fn delegation_hold(home: &Path, label: &str, _key: &str) -> Result<Held, StoreError> {
    use duduclaw_core::org_store::{self as org, OrgEntry};
    if label.is_empty() { return Err(StoreError::Unavailable); }
    let store = org::load_for_write(home).map_err(|_| StoreError::Unavailable)?;
    let reports = store.iter().any(|(id, entry)| id.as_str() != label && entry.reports_to == label);
    if !reports { return Ok(Held::names_only(Vec::new())); }
    org::with_org_lock(home, |guard| {
        // Re-read under the lock: only an edge that still points at the label moves.
        let current = org::load_for_write(home).map_err(|_| std::io::Error::other("org store unreadable"))?;
        let edges: Vec<(String, String)> = current.iter()
            .filter(|(id, entry)| id.as_str() != label && entry.reports_to == label)
            .map(|(id, entry)| (id.clone(), entry.department.clone())).collect();
        let mut held = Held::names_only(Vec::new());
        for (id, department) in edges {
            // A failed write stops here; the edges already moved stay recorded.
            if org::upsert(guard, &id, OrgEntry::new("", &department)).is_err() {
                held.complete = false;
                break;
            }
            held.names.push(format!("reports_to:{id}"));
            held.restore.push(json!({"agent": id, "parent": label, "department": department}));
        }
        Ok(held)
    }).map_err(|_| StoreError::Unavailable)
}

// ── Restore (F6c) ───────────────────────────────────────────────────────

/// What [`restore_label_stores`] lists. Entries are `<store>:<id>` names;
/// `skipped` entries end in `:<reason>`. Nothing here was applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RestoreReport {
    /// Re-grantable by an Admin, one at a time ([`regrant_item`]):
    /// `website_credentials:<id>` and `reports_to:<child>`.
    pub(crate) regrant: Vec<String>,
    pub(crate) skipped: Vec<String>,
    /// For the admin to re-enable by hand (§3.3, §3.7); no RPC re-applies them.
    pub(crate) listed: Vec<String>,
}

impl RestoreReport {
    pub(crate) fn to_value(&self) -> Value {
        json!({"regrant": self.regrant, "skipped": self.skipped, "listed": self.listed})
    }
}

/// One admin re-grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Regranted {
    /// Applied now (or already in place: idempotent).
    Done,
    /// Not applied; the reason (`not_held`, `parent_missing`, `child_missing`,
    /// `reparented`, `refused`).
    Skipped(&'static str),
    /// `item` is not one of the row's `regrant` entries (exact match).
    NotListed,
}

fn text<'a>(entry: &'a Value, field: &str) -> Option<&'a str> { entry.get(field).and_then(Value::as_str) }

fn entries<'a>(stripped: &'a Value, section: &str, store: &str) -> Vec<&'a Value> {
    match stripped.get(section).and_then(|value| value.get(store)).and_then(Value::as_array) {
        Some(items) => items.iter().collect(),
        None => Vec::new(),
    }
}

/// A well-formed held website grant of `label`: `(id, key)`.
fn website_entry<'a>(entry: &'a Value, label: &str) -> Result<(&'a str, &'a str), String> {
    let (Some(id), Some(agent), Some(key)) = (text(entry, "id"), text(entry, "agent"), text(entry, "key")) else {
        return Err("website_credentials:?:malformed".into());
    };
    if uuid::Uuid::parse_str(id).is_err() || !key.starts_with("quarantine:") { return Err("website_credentials:?:malformed".into()); }
    if agent != label { return Err(format!("website_credentials:{id}:label_mismatch")); }
    Ok((id, key))
}

/// A well-formed `reports_to` edge to `label`: the child.
fn edge_entry<'a>(entry: &'a Value, label: &str) -> Result<&'a str, String> {
    let (Some(child), Some(parent)) = (text(entry, "agent"), text(entry, "parent")) else {
        return Err("reports_to:?:malformed".into());
    };
    if parent != label || child == label { return Err(format!("reports_to:{child}:label_mismatch")); }
    Ok(child)
}

/// List the restorable label-keyed state of `stripped` (the quarantine row's
/// JSON) for `label` (exact). Reads nothing outside `stripped` and writes
/// nothing (§3.3: never automatic); a malformed row is `Err`.
pub(crate) fn restore_label_stores(_home: &Path, label: &str, stripped: &Value) -> Result<RestoreReport, StoreError> {
    if label.is_empty() || !stripped.is_object() { return Err(StoreError::Unavailable); }
    let mut report = RestoreReport::default();
    for entry in entries(stripped, "restore", "website_credentials") {
        match website_entry(entry, label) {
            Ok((id, _)) => report.regrant.push(format!("website_credentials:{id}")),
            Err(skipped) => report.skipped.push(skipped),
        }
    }
    for entry in entries(stripped, "restore", "delegation_targets") {
        match edge_entry(entry, label) {
            Ok(child) => report.regrant.push(format!("reports_to:{child}")),
            Err(skipped) => report.skipped.push(skipped),
        }
    }
    report.regrant.dedup();
    for name in entries(stripped, "global_routes", "channel_routes").into_iter().filter_map(Value::as_str) {
        report.listed.push(format!("channel_routes:{name}"));
    }
    for (section, prefix) in [("deferred", "deferred"), ("in_tree", "in_tree")] {
        for name in stripped.get(section).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            report.listed.push(format!("{prefix}:{name}"));
        }
    }
    if let Some(paused) = stripped.get("paused_work").and_then(Value::as_object) {
        for (store, ids) in paused {
            for id in ids.as_array().into_iter().flatten().filter_map(Value::as_str) {
                report.listed.push(format!("paused_work:{store}:{id}"));
            }
        }
    }
    Ok(report)
}

/// Re-apply exactly one `regrant` entry of the row (`item` must equal one of
/// [`restore_label_stores`]' `regrant` names; exact string equality). A held
/// website grant under the recorded row key is moved back to `label`; a
/// `reports_to` edge is re-applied only if `label` is in the org, the child
/// exists and still reports to the root (department: the current one is
/// kept). Idempotent; an unreadable store is `Err`.
pub(crate) fn regrant_item(home: &Path, label: &str, stripped: &Value, item: &str) -> Result<Regranted, StoreError> {
    use crate::website_credentials::{regrant_held, Regrant};
    let listed = restore_label_stores(home, label, stripped)?;
    if !listed.regrant.iter().any(|name| name == item) { return Ok(Regranted::NotListed); }
    if let Some(id) = item.strip_prefix("website_credentials:") {
        let entry = entries(stripped, "restore", "website_credentials").into_iter()
            .filter_map(|entry| website_entry(entry, label).ok()).find(|(entry_id, _)| *entry_id == id);
        let Some((id, key)) = entry else { return Ok(Regranted::NotListed) };
        return Ok(match regrant_held(home, label, key, id).map_err(|_| StoreError::Unavailable)? {
            Regrant::Regranted | Regrant::AlreadyGranted => Regranted::Done,
            Regrant::Missing => Regranted::Skipped("not_held"),
        });
    }
    let Some(child) = item.strip_prefix("reports_to:") else { return Ok(Regranted::NotListed) };
    regrant_edge(home, label, child)
}

fn regrant_edge(home: &Path, label: &str, child: &str) -> Result<Regranted, StoreError> {
    use duduclaw_core::org_store::{self as org, OrgEntry};
    org::with_org_lock(home, |guard| {
        // Re-read under the lock (an applied edge is visible).
        let current = org::load_for_write(home).map_err(|_| std::io::Error::other("org store unreadable"))?;
        if !current.contains(label) { return Ok(Regranted::Skipped("parent_missing")); }
        let Some(entry_now) = current.get(child) else { return Ok(Regranted::Skipped("child_missing")) };
        if entry_now.reports_to == label { return Ok(Regranted::Done); }
        if !entry_now.reports_to.is_empty() { return Ok(Regranted::Skipped("reparented")); }
        // The edge only: an admin's later department edit is kept.
        Ok(match org::upsert(guard, child, OrgEntry::new(label, &entry_now.department)) {
            Ok(_) => Regranted::Done,
            // A cycle (or another refusal) is not forced.
            Err(_) => Regranted::Skipped("refused"),
        })
    }).map_err(|_| StoreError::Unavailable)
}

// ── Purge (F6i) ─────────────────────────────────────────────────────────

/// Purge the label-keyed state sealed for the quarantine row
/// `(employee, incarnation)`: every registered store under the row key,
/// including the website grants held for it (a credential left with no
/// grant is deleted for good). Anything still under the label itself is
/// `purge_all(home, label)`, which F6i runs only while no live row holds the
/// label (§3.8). Stops at the first failing store.
pub(crate) fn purge_row_stores(home: &Path, employee: EmployeeId, incarnation: u32)
    -> Result<Vec<(&'static str, u64)>, (&'static str, StoreError)> {
    purge_all(home, &quarantine_key(employee, incarnation))
}
