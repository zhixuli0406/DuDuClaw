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
//! [`restore_label_stores`] (F6c calls it after the restore commit, §3.5
//! step 4) re-grants each held credential and re-applies each edge whose
//! child still exists and still reports to the root; anything else is
//! skipped and reported. Channel routes, unmodelled global routes, in-tree
//! grants and paused work are only listed (§3.3 "listed for admin re-enable
//! (never automatic)", §3.7 work items stay paused).
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

/// What [`restore_label_stores`] did. Entries are `<store>:<id>` names;
/// `skipped` entries end in `:<reason>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RestoreReport {
    pub(crate) regranted: Vec<String>,
    pub(crate) reapplied: Vec<String>,
    pub(crate) skipped: Vec<String>,
    /// Never re-applied here: listed for the admin (§3.3, §3.7).
    pub(crate) listed: Vec<String>,
}

impl RestoreReport {
    pub(crate) fn to_value(&self) -> Value {
        json!({"regranted": self.regranted, "reapplied": self.reapplied, "skipped": self.skipped, "listed": self.listed})
    }
}

fn text<'a>(entry: &'a Value, field: &str) -> Option<&'a str> { entry.get(field).and_then(Value::as_str) }

fn entries<'a>(stripped: &'a Value, section: &str, store: &str) -> Vec<&'a Value> {
    match stripped.get(section).and_then(|value| value.get(store)).and_then(Value::as_array) {
        Some(items) => items.iter().collect(),
        None => Vec::new(),
    }
}

/// Re-apply the restorable label-keyed state of `stripped` (the quarantine
/// row's JSON) for `label` (exact). Website grants held under the row key are
/// re-granted; `reports_to` edges are re-applied only where the child still
/// exists, still reports to the root and the label is in the org. Everything
/// else is listed. An unreadable store is `Err` (nothing is guessed); the
/// call is idempotent, so the caller retries.
pub(crate) fn restore_label_stores(home: &Path, label: &str, stripped: &Value) -> Result<RestoreReport, StoreError> {
    use crate::website_credentials::{regrant_held, Regrant};
    if label.is_empty() || !stripped.is_object() { return Err(StoreError::Unavailable); }
    let mut report = RestoreReport::default();
    for entry in entries(stripped, "restore", "website_credentials") {
        let (Some(id), Some(agent), Some(key)) = (text(entry, "id"), text(entry, "agent"), text(entry, "key")) else {
            report.skipped.push("website_credentials:?:malformed".into());
            continue;
        };
        if uuid::Uuid::parse_str(id).is_err() || !key.starts_with("quarantine:") {
            report.skipped.push("website_credentials:?:malformed".into());
            continue;
        }
        if agent != label { report.skipped.push(format!("website_credentials:{id}:label_mismatch")); continue; }
        match regrant_held(home, label, key, id).map_err(|_| StoreError::Unavailable)? {
            Regrant::Regranted | Regrant::AlreadyGranted => report.regranted.push(format!("website_credentials:{id}")),
            Regrant::Missing => report.skipped.push(format!("website_credentials:{id}:not_held")),
        }
    }
    restore_edges(home, label, stripped, &mut report)?;
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

fn restore_edges(home: &Path, label: &str, stripped: &Value, report: &mut RestoreReport) -> Result<(), StoreError> {
    use duduclaw_core::org_store::{self as org, OrgEntry};
    let edges = entries(stripped, "restore", "delegation_targets");
    if edges.is_empty() { return Ok(()); }
    let outcome = org::with_org_lock(home, |guard| {
        let mut done = Vec::new();
        for entry in &edges {
            let (Some(child), Some(parent)) = (text(entry, "agent"), text(entry, "parent")) else {
                done.push(Err("reports_to:?:malformed".to_owned()));
                continue;
            };
            if parent != label || child == label {
                done.push(Err(format!("reports_to:{child}:label_mismatch")));
                continue;
            }
            // Re-read under the lock for every edge (an applied edge is visible).
            let current = org::load_for_write(home).map_err(|_| std::io::Error::other("org store unreadable"))?;
            if !current.contains(label) { done.push(Err(format!("reports_to:{child}:parent_missing"))); continue; }
            let Some(entry_now) = current.get(child) else {
                done.push(Err(format!("reports_to:{child}:child_missing")));
                continue;
            };
            if entry_now.reports_to == label { done.push(Ok(format!("reports_to:{child}"))); continue; }
            if !entry_now.reports_to.is_empty() { done.push(Err(format!("reports_to:{child}:reparented"))); continue; }
            // The edge only: an admin's later department edit is kept.
            match org::upsert(guard, child, OrgEntry::new(label, &entry_now.department)) {
                Ok(_) => done.push(Ok(format!("reports_to:{child}"))),
                // A cycle (or another refusal) is not forced.
                Err(_) => done.push(Err(format!("reports_to:{child}:refused"))),
            }
        }
        Ok(done)
    }).map_err(|_| StoreError::Unavailable)?;
    for result in outcome {
        match result { Ok(name) => report.reapplied.push(name), Err(name) => report.skipped.push(name) }
    }
    Ok(())
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
