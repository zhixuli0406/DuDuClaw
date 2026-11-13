//! RM-134 (v1.82): gateway-side lifecycle of the hashed MCP key store.
//!
//! - [`migrate_config_keys`] runs at every gateway start, after the internal
//!   key was provisioned (idempotent):
//!   1. it reads `config.toml` once and, in ONE transaction, records the
//!      fingerprints of its dashboard `[mcp_keys]` entries as "pending
//!      legacy" (`McpKeyStore::begin_legacy_migration`). Only the first
//!      start does this; the set is never extended, so an entry that appears
//!      in `config.toml` later is never pending;
//!   2. each pending entry is inserted into `<home>/mcp_keys.db` (digest
//!      only; client, flag and scopes kept; `expires_at` = the instant the
//!      pre-v1.82 rule started refusing it) and its `mcp_key_migrated` audit
//!      record written. Only then is it cleared from the pending set and
//!      removed from `config.toml`; a failed insert or audit leaves it in
//!      config and pending — where the legacy window still accepts it — and
//!      the next start retries;
//!   3. once nothing is pending the persistent completion marker is written.
//!      From then on `config.toml` is never consulted for a non-internal key
//!      (`duduclaw_core::mcp_key_store::resolve`);
//!   4. an entry that is neither pending nor in the store (added after step
//!      1, or a `gateway-internal` claim whose fingerprint the gateway did
//!      not record) is never imported: it is logged once as an "ignored
//!      legacy entry" by its display hint (`ddc_<env>_…<last 4>`, never a
//!      fingerprint) and listed on the keys page until an Admin removes it;
//!   5. a config copy of a key the store already holds (a restored backup)
//!      is removed from `config.toml`; the store row decides.
//!
//!   Third review: an internal entry is imported with an `agent_binding`
//!   only when its `client_id` exactly equals an existing employee directory
//!   id ([`employee_dir_ids`]); an entry dated further in the future than the
//!   clock-skew allowance is never imported (reported as ignored).
//!   Fourth review: and only when all its scopes are per-employee scopes
//!   ([`duduclaw_auth::mcp_keys::employee_key_scopes_allowed`]); otherwise
//!   it is imported unbound with its scopes as they were and flagged
//!   "needs re-issue" ([`needs_reissue`]).
//! - [`sweep_employee_gone_keys`] (fourth review, Medium 1) runs in the lock
//!   holder before every expiry-notice sweep and tombstones, with an audit
//!   record each, the keys whose bound employee is gone for good — removed
//!   outside the gateway (MCP `agent_remove`, a manual directory removal) or
//!   soft-deleted. The dashboard's own employee delete tombstones that
//!   employee's keys immediately ([`revoke_employee_keys`]). A key whose
//!   employee directory is younger than the key (re-created under the same
//!   id) is refused by every authenticator ([`ks::key_binding_state`]) but
//!   not tombstoned on that birth-time evidence alone.
//! - [`run_expiry_notices`] tells every Admin, through the existing push
//!   path, 7 days and 1 day before a key's `expires_at`. Each notice is
//!   claimed (a lease carrying a random claim token), attempted, and marked
//!   delivered — by a compare-and-set on that token and the key row — only
//!   when it reached an Admin (sent, or durably queued for quiet hours); a
//!   failed attempt is
//!   retried on later sweeps with backoff. Notices that can no longer be met
//!   are closed without sending. Only the process holding the gateway lock
//!   sends.
//! - [`audit`] writes the security audit record of every management action
//!   and of the migration (`security_audit.jsonl`). Never the key, never its
//!   digest.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use duduclaw_auth::mcp_keys::{ImportOutcome, LegacyKey, McpKeyStore, NoticeClaim, NoticeThreshold};
use duduclaw_core::mcp_key_store::{self as ks, MigrationState, StoredKey};
use duduclaw_security::audit::{AuditEvent, Severity};
use serde_json::{Value, json};
use tracing::{info, warn};

/// Interval between expiry-notice sweeps.
pub(crate) const NOTICE_SWEEP_SECS: u64 = 600;

/// Append one security audit record. `details` must never contain a key or
/// a digest; callers pass ids, names, scopes and timestamps only.
pub(crate) fn audit(home: &Path, event_type: &str, severity: Severity, details: Value) -> Result<(), String> {
    let event = AuditEvent::new(event_type, "dashboard", severity, details);
    duduclaw_security::audit::try_append_audit_event(home, &event)
}

/// The audit / RPC projection of a key's metadata. No digest, no hint.
pub(crate) fn key_summary(key: &StoredKey) -> Value {
    json!({
        "key_id": key.id,
        "name": key.name,
        "client_id": key.client_id,
        "is_external": key.is_external,
        "agent_binding": key.agent_binding,
        "scopes": key.scopes,
        "expires_at": key.expires_at.map(ks::timestamp),
    })
}

/// What one migration pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct MigrationReport {
    /// Newly inserted into the store.
    pub inserted: usize,
    /// Already in the store (an earlier pass inserted it, or it was deleted
    /// there since — the tombstone wins).
    pub already_present: usize,
    /// Removed from `config.toml`.
    pub removed_from_config: usize,
    /// Left in `config.toml` (and pending) because the insert or its audit
    /// record failed; retried at the next start.
    pub kept_in_config: usize,
    /// Display hints of entries that are never imported (see module docs).
    pub ignored: Vec<String>,
    /// The completion marker is (now) written.
    pub complete: bool,
}

struct Candidate {
    raw: String,
    client_id: String,
    is_external: bool,
    scopes: Vec<String>,
    created_at: DateTime<Utc>,
    label: Option<String>,
    fingerprint: String,
}

/// Entries the migration moves: well-formed keys with a client id and a
/// parseable `created_at` (the legacy authenticator skips anything else, so
/// such a row never authenticated and is left alone), except every entry of
/// the gateway's own internal client — that key stays in config by design
/// and is accepted only through its recorded fingerprint.
fn candidates(table: &toml::Table) -> Vec<Candidate> {
    let Some(keys) = table.get("mcp_keys").and_then(toml::Value::as_table) else { return Vec::new() };
    let mut out = Vec::new();
    for (raw, entry) in keys {
        let Some(t) = entry.as_table() else { continue };
        let Some(client_id) = t.get("client_id").and_then(toml::Value::as_str) else { continue };
        if client_id == ks::RESERVED_INTERNAL_CLIENT_ID || !ks::is_key_format(raw) {
            continue;
        }
        let Some(created_at) = t
            .get("created_at")
            .and_then(toml::Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
        else {
            continue;
        };
        let scopes = t
            .get("scopes")
            .and_then(toml::Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let label = ["name", "label"]
            .iter()
            .find_map(|field| t.get(*field).and_then(toml::Value::as_str))
            .map(str::to_owned);
        out.push(Candidate {
            raw: raw.clone(),
            client_id: client_id.to_owned(),
            is_external: t.get("is_external").and_then(toml::Value::as_bool).unwrap_or(false),
            scopes,
            created_at,
            label,
            fingerprint: duduclaw_core::mcp_oauth_store::legacy_entry_fingerprint(entry),
        });
    }
    out
}

/// Well-formed `[mcp_keys]` entries claiming the gateway-internal client.
fn internal_claims(table: &toml::Table) -> Vec<String> {
    let Some(keys) = table.get("mcp_keys").and_then(toml::Value::as_table) else { return Vec::new() };
    keys.iter()
        .filter(|(raw, entry)| {
            ks::is_key_format(raw)
                && entry.get("client_id").and_then(toml::Value::as_str) == Some(ks::RESERVED_INTERNAL_CLIENT_ID)
        })
        .map(|(raw, _)| raw.clone())
        .collect()
}

fn read_config(home: &Path) -> Result<Option<String>, String> {
    match crate::handlers::config_commit::read_regular_config(&home.join("config.toml")) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("config.toml unreadable: {e}")),
    }
}

fn parse_config(text: Option<&str>) -> Result<toml::Table, String> {
    match text {
        None => Ok(toml::Table::new()),
        Some(text) => text.parse().map_err(|_| "config.toml is not valid TOML; MCP key migration skipped".to_string()),
    }
}

fn code(e: duduclaw_auth::mcp_keys::McpKeyError) -> String {
    e.code().to_string()
}

/// The `mcp_key_migrated` record. Must be written before the entry leaves
/// `config.toml` (L1).
fn audit_migrated(home: &Path, key: &StoredKey) -> bool {
    let mut details = key_summary(key);
    details["source"] = json!("config.toml [mcp_keys]");
    details["actor"] = json!("system:migration");
    match audit(home, "mcp_key_migrated", Severity::Info, details) {
        Ok(()) => true,
        Err(error) => {
            warn!(key_id = key.id.as_str(), %error,
                "MCP key migration audit record not written; the entry stays in config.toml and pending until the next start");
            false
        }
    }
}

/// Every AI employee's exact directory id: an `agents/<id>` entry whose name
/// is a valid agent id (not an internal / hidden directory) and that holds an
/// `agent.toml`. Names as the filesystem lists them — never an `[agent]
/// name`, never case-folded. An unreadable `agents/` directory is an error.
pub(crate) fn employee_dir_ids(home: &Path) -> Result<HashSet<String>, String> {
    let mut ids = HashSet::new();
    let entries = match std::fs::read_dir(home.join("agents")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(e) => return Err(format!("agents directory unreadable: {e}")),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("agents directory unreadable: {e}"))?;
        let Some(dir) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if duduclaw_core::is_internal_agent_dir(&dir) || !duduclaw_core::is_valid_agent_id(&dir) {
            continue;
        }
        if entry.path().join("agent.toml").is_file() {
            ids.insert(dir);
        }
    }
    Ok(ids)
}

/// The keys-page / log token for a migrated key that must be re-issued.
pub(crate) const NEEDS_REISSUE_EMPLOYEE_SCOPES: &str = "scopes_not_allowed_for_employee_key";

/// Fourth review (Medium 2): why a key must be re-issued, if it must. A
/// migrated internal key without an employee binding whose client id is an
/// existing employee directory id (`client_is_employee`) but whose scopes are
/// not all per-employee scopes: the migration deliberately left it unbound
/// (it carries `admin` or another scope a per-employee key may not), so the
/// MCP HTTP server refuses it. Shown as "needs re-issue: scopes not allowed
/// for a per-employee key".
pub(crate) fn needs_reissue(key: &StoredKey, client_is_employee: bool) -> Option<&'static str> {
    (key.migrated
        && !key.is_external
        && key.agent_binding.is_none()
        && client_is_employee
        && !duduclaw_auth::mcp_keys::employee_key_scopes_allowed(&key.scopes))
    .then_some(NEEDS_REISSUE_EMPLOYEE_SCOPES)
}

/// The `mcp_key_deleted` record of a key tombstoned because its employee is
/// gone. A failure is logged; the tombstone stays (a tightening).
fn audit_employee_key_revoked(home: &Path, key: &StoredKey, reason: &str, source: &str) {
    let mut details = key_summary(key);
    details["actor"] = json!(duduclaw_auth::mcp_keys::EMPLOYEE_GONE_ACTOR);
    details["reason"] = json!(reason);
    details["source"] = json!(source);
    details["permanent"] = json!(true);
    if let Err(error) = audit(home, "mcp_key_deleted", Severity::Warning, details) {
        tracing::error!(key_id = key.id.as_str(), %error,
            "MCP key of a removed AI employee was revoked, but its audit record could not be written");
    }
}

/// Fourth review (Medium 1): the dashboard deletes employee `agent_id` —
/// permanently revoke (tombstone) every key bound to it, in the same
/// operation, with one audit record per key. The tombstones keep the
/// digests, so these keys never authenticate again, whatever employee later
/// gets the same id. No store file = no key could be bound. Blocking.
pub(crate) fn revoke_employee_keys(home: &Path, agent_id: &str, source: &str) -> Result<usize, String> {
    if !ks::store_path(home).exists() {
        return Ok(0);
    }
    let store = McpKeyStore::open(home)?;
    let changes = store
        .revoke_employee_keys(agent_id, duduclaw_auth::mcp_keys::EMPLOYEE_GONE_ACTOR, Utc::now())
        .map_err(code)?;
    drop(store);
    for change in &changes {
        audit_employee_key_revoked(home, &change.before, ks::BindingState::Deleted.as_str(), source);
    }
    if !changes.is_empty() {
        info!(agent = agent_id, revoked = changes.len(), "MCP keys of a deleted AI employee permanently revoked");
    }
    Ok(changes.len())
}

/// The not-deleted keys whose bound employee is gone for good
/// ([`ks::BindingState::employee_gone`]), with the reason.
fn employee_gone_keys(home: &Path, store: &McpKeyStore) -> Result<Vec<(StoredKey, ks::BindingState)>, String> {
    let mut gone = Vec::new();
    for key in store.list().map_err(code)? {
        let Some(binding) = key.agent_binding.as_deref() else { continue };
        let state = ks::key_binding_state(home, binding, key.created_at);
        if state.employee_gone() {
            gone.push((key, state));
        }
    }
    Ok(gone)
}

/// Pause between finding a key whose employee looks gone and re-checking it
/// before the tombstone (so a directory caught mid-replacement is not
/// mistaken for a removal).
pub(crate) const EMPLOYEE_GONE_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// Fourth review (Medium 1): the key sweep of the lock holder. Tombstones
/// (with an `mcp_key_deleted` audit record each) every key whose bound
/// employee is gone for good: its directory removed outside the gateway
/// (MCP `agent_remove` moves it to `agents/_trash`, or a manual removal) or
/// soft-deleted (`status = "deleted"`) — [`ks::BindingState::employee_gone`].
/// Each candidate is re-checked after `recheck` before it is tombstoned. An unreadable
/// employee is left alone (refused meanwhile, decided by a later sweep), and
/// so is everything when `agents/` itself is missing or not a directory — a
/// home being moved or restored, not every employee removed. Blocking.
pub(crate) fn sweep_employee_gone_keys(home: &Path, recheck: std::time::Duration) -> Result<usize, String> {
    if !ks::store_path(home).exists() {
        return Ok(0);
    }
    match std::fs::symlink_metadata(home.join("agents")) {
        Ok(meta) if meta.is_dir() => {}
        _ => return Ok(0),
    }
    let store = McpKeyStore::open(home)?;
    let candidates = employee_gone_keys(home, &store)?;
    if candidates.is_empty() {
        return Ok(0);
    }
    if !recheck.is_zero() {
        std::thread::sleep(recheck);
    }
    let mut revoked = 0;
    for (key, _) in candidates {
        let Some(binding) = key.agent_binding.as_deref() else { continue };
        let state = ks::key_binding_state(home, binding, key.created_at);
        if !state.employee_gone() {
            continue;
        }
        match store.delete(&key.id, duduclaw_auth::mcp_keys::EMPLOYEE_GONE_ACTOR, Utc::now()) {
            Ok(change) => {
                revoked += 1;
                audit_employee_key_revoked(home, &change.before, state.as_str(), "key_sweep");
                info!(key_id = key.id.as_str(), reason = state.as_str(),
                    "MCP key permanently revoked: the AI employee it was bound to is gone");
            }
            // Deleted meanwhile (by an Admin or the dashboard's employee delete).
            Err(duduclaw_auth::mcp_keys::McpKeyError::NotFound) => {}
            Err(error) => warn!(key_id = key.id.as_str(), error = error.code(),
                "MCP key of a removed AI employee not revoked yet; it stays refused and the next sweep retries"),
        }
    }
    Ok(revoked)
}

/// Run the `[mcp_keys]` → hashed store migration (see the module docs).
/// Idempotent and crash-safe.
///
/// Third review: an internal entry whose `client_id` exactly equals an
/// existing employee directory id is imported with that id as its
/// `agent_binding`; every other entry gets no binding (and so is refused by
/// the HTTP MCP server). An entry whose `created_at` lies further in the
/// future than the clock-skew allowance never authenticated under the legacy
/// rule; it is not imported and is reported as ignored.
pub(crate) fn migrate_config_keys(home: &Path) -> Result<MigrationReport, String> {
    let mut report = MigrationReport::default();
    let text = read_config(home)?;
    let table = parse_config(text.as_deref())?;
    let now = Utc::now();
    let (found, future_dated): (Vec<Candidate>, Vec<Candidate>) =
        candidates(&table).into_iter().partition(|c| ks::legacy_created_at_credible(c.created_at, now));
    let employees = employee_dir_ids(home)?;
    let store = McpKeyStore::open(home)?;
    let hashes: Vec<String> = found.iter().map(|c| ks::digest(&c.raw)).collect();
    // Step 1: the pending set, from config as read now, in one transaction.
    let state = store.begin_legacy_migration(&hashes, now).map_err(code)?;
    let pending = store.pending_hashes().map_err(code)?;
    let mut confirmed: Vec<String> = Vec::new();
    let mut ignored: Vec<(String, String)> = Vec::new();
    for c in &found {
        let hash = ks::digest(&c.raw);
        let is_pending = pending.contains(&hash);
        match store.by_hash(&hash).map_err(code)? {
            // A config copy of a key the store decides (step 5).
            Some(_) if !is_pending => {
                report.already_present += 1;
                confirmed.push(c.raw.clone());
                continue;
            }
            // Imported by an earlier pass whose audit record failed.
            Some(key) => {
                report.already_present += 1;
                if audit_migrated(home, &key) {
                    store.clear_pending(&hash).map_err(code)?;
                    confirmed.push(c.raw.clone());
                } else {
                    report.kept_in_config += 1;
                }
                continue;
            }
            None => {}
        }
        if state == MigrationState::Complete || !is_pending {
            ignored.push((hash, ks::key_hint(&c.raw)));
            continue;
        }
        // Fourth review (Medium 2): bound only when every scope is one a
        // per-employee key may carry. An employee's entry with `admin` or any
        // other scope is imported unbound (refused over HTTP), its scopes
        // kept as they were, and flagged for re-issue (log by display hint;
        // the keys page shows `needs_reissue`).
        let employee_entry = !c.is_external && employees.contains(&c.client_id);
        let scopes_allowed = duduclaw_auth::mcp_keys::employee_key_scopes_allowed(&c.scopes);
        if employee_entry && !scopes_allowed {
            warn!(entry = %ks::key_hint(&c.raw),
                "legacy [mcp_keys] entry of an AI employee needs re-issue: scopes not allowed for a per-employee key; migrated without an employee binding, so the MCP HTTP server refuses it");
        }
        let entry = LegacyKey {
            raw: &c.raw,
            client_id: &c.client_id,
            is_external: c.is_external,
            scopes: c.scopes.clone(),
            created_at: c.created_at,
            label: c.label.as_deref(),
            legacy_entry_hash: c.fingerprint.clone(),
            agent_binding: (employee_entry && scopes_allowed).then_some(c.client_id.as_str()),
        };
        match store.import_legacy(&entry, now) {
            Ok(ImportOutcome::Inserted(key)) => {
                report.inserted += 1;
                if audit_migrated(home, &key) {
                    store.clear_pending(&hash).map_err(code)?;
                    confirmed.push(c.raw.clone());
                } else {
                    report.kept_in_config += 1;
                }
            }
            Ok(ImportOutcome::AlreadyPresent) => {
                // Raced with another pass between the lookup and the insert.
                report.already_present += 1;
                report.kept_in_config += 1;
            }
            Err(error) => {
                report.kept_in_config += 1;
                warn!(error = error.code(), "MCP key not migrated; it stays in config.toml and pending");
            }
        }
    }
    // Pending fingerprints whose entry left config: settle them.
    let in_config: HashSet<&String> = hashes.iter().collect();
    for hash in pending.iter().filter(|h| !in_config.contains(h)) {
        match store.by_hash(hash).map_err(code)? {
            Some(key) if !audit_migrated(home, &key) => {}
            _ => store.clear_pending(hash).map_err(code)?,
        }
    }
    // Future-dated entries the store does not hold: never imported.
    for c in &future_dated {
        let hash = ks::digest(&c.raw);
        if !store.contains_hash(&hash).map_err(code)? {
            ignored.push((hash, ks::key_hint(&c.raw)));
        }
    }
    // Step 4: gateway-internal claims the gateway never recorded.
    let recorded = store.internal_key_hashes().map_err(code)?;
    for raw in internal_claims(&table) {
        let hash = ks::digest(&raw);
        if !recorded.contains(&hash) && !store.contains_hash(&hash).map_err(code)? {
            ignored.push((hash, ks::key_hint(&raw)));
        }
    }
    for (hash, hint) in &ignored {
        if store.note_ignored(hash, hint, now).map_err(code)? {
            warn!(entry = hint.as_str(),
                "ignored legacy [mcp_keys] entry in config.toml: the MCP key store does not know it, so it never authenticates; an Admin should remove it");
        }
    }
    report.ignored = ignored.into_iter().map(|(_, hint)| hint).collect();
    // Step 3: the completion marker.
    report.complete = match store.migration_state().map_err(code)? {
        MigrationState::Complete => true,
        _ => store.complete_migration(now).map_err(code)?,
    };
    drop(store);
    if !confirmed.is_empty()
        && let Some(text) = text
    {
        report.removed_from_config = remove_from_config(home, text, &confirmed)?;
    }
    if report.inserted + report.removed_from_config + report.kept_in_config > 0 {
        info!(
            inserted = report.inserted,
            already_present = report.already_present,
            removed = report.removed_from_config,
            kept = report.kept_in_config,
            ignored = report.ignored.len(),
            complete = report.complete,
            "MCP keys moved from config.toml [mcp_keys] to the hashed key store"
        );
    }
    Ok(report)
}

/// What the keys page shows about `[mcp_keys]`: entries still waiting to be
/// moved (accepted meanwhile) and ignored entries (display hints only).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LegacyConfigReport {
    pub pending: usize,
    pub ignored: Vec<String>,
}

/// Classify the `[mcp_keys]` entries of `config.toml` against the store, as
/// the authenticators would decide them now.
pub(crate) fn legacy_config_report(home: &Path, store: &McpKeyStore) -> Result<LegacyConfigReport, String> {
    let text = read_config(home)?;
    let table = parse_config(text.as_deref())?;
    let state = store.migration_state().map_err(code)?;
    let pending = store.pending_hashes().map_err(code)?;
    let internal = store.internal_key_hashes().map_err(code)?;
    let mut report = LegacyConfigReport::default();
    let now = Utc::now();
    for c in candidates(&table) {
        let hash = ks::digest(&c.raw);
        if store.contains_hash(&hash).map_err(code)? {
            continue;
        }
        if !ks::legacy_created_at_credible(c.created_at, now) {
            report.ignored.push(ks::key_hint(&c.raw));
            continue;
        }
        match state {
            MigrationState::NotStarted => report.pending += 1,
            MigrationState::Pending if pending.contains(&hash) => report.pending += 1,
            _ => report.ignored.push(ks::key_hint(&c.raw)),
        }
    }
    for raw in internal_claims(&table) {
        let hash = ks::digest(&raw);
        if !internal.contains(&hash) && !store.contains_hash(&hash).map_err(code)? {
            report.ignored.push(ks::key_hint(&raw));
        }
    }
    report.ignored.sort();
    Ok(report)
}

/// Remove `confirmed` entries through the CAS + format-preserving commit
/// path. One retry on a concurrent edit; a failure leaves both copies, which
/// is safe (the store row decides) and is retried at the next start.
fn remove_from_config(home: &Path, mut text: String, confirmed: &[String]) -> Result<usize, String> {
    for attempt in 0..2 {
        let mut removed = 0usize;
        let expected = crate::handlers::config_commit::content_hash(&text);
        let result = crate::handlers::config_commit::commit_local_config_rendered(home, &expected, |current| {
            let mut table: toml::Table = current.parse().map_err(|_| "config.toml is not valid TOML".to_string())?;
            if let Some(keys) = table.get_mut("mcp_keys").and_then(toml::Value::as_table_mut) {
                for raw in confirmed {
                    if keys.remove(raw).is_some() {
                        removed += 1;
                    }
                }
                if keys.is_empty() {
                    table.remove("mcp_keys");
                }
            }
            crate::handlers::config_commit::render_preserving(current, &table)
        });
        match result {
            Ok(()) => return Ok(removed),
            Err(error) if attempt == 0 => {
                warn!(%error, "config.toml changed during MCP key migration; retrying once");
                text = read_config(home)?.unwrap_or_default();
            }
            Err(error) => return Err(format!("migrated MCP keys stay in config.toml until the next start: {error}")),
        }
    }
    Ok(0)
}

/// Bilingual notice for one key and threshold. The key's name is the
/// Admin's own label; nothing secret.
pub(crate) fn notice_text(key: &StoredKey, threshold: NoticeThreshold) -> String {
    let date = key.expires_at.map(ks::timestamp).unwrap_or_default();
    let name = duduclaw_core::truncate_chars(&key.name, ks::NAME_MAX_CHARS);
    match threshold {
        NoticeThreshold::SevenDays => format!(
            "MCP 金鑰「{name}」將於 {date} 到期（剩 7 天）。請到儀表板「MCP 金鑰」延長到期日或換發新金鑰。\n\
             MCP key \"{name}\" expires at {date} (7 days left). Extend it or issue a replacement on the dashboard's MCP keys page."
        ),
        NoticeThreshold::OneDay => format!(
            "MCP 金鑰「{name}」將於 {date} 到期（剩 1 天）。到期後使用這把金鑰的外部工具將無法連線。\n\
             MCP key \"{name}\" expires at {date} (1 day left). External tools using it will be refused after that."
        ),
    }
}

/// Push one notice to every Admin's verified channel. `true` only when it
/// reached at least one (sent now, or durably queued for quiet hours).
async fn deliver_notice(home: &Path, key: &StoredKey, threshold: NoticeThreshold) -> bool {
    let targets = match crate::notify_push::admin_targets(home) {
        Ok(targets) => targets,
        Err(error) => {
            warn!(%error, "MCP key expiry notice: Admin destinations unavailable");
            return false;
        }
    };
    if targets.is_empty() {
        return false;
    }
    let receipt = crate::notify_push::push_plain_notice(home, "mcp_key.expiry", &notice_text(key, threshold), targets).await;
    receipt.delivered.is_some()
}

/// One sweep through the real push path. Returns how many notices were
/// delivered.
pub(crate) async fn sweep_expiry_notices(home: &Path, now: DateTime<Utc>) -> usize {
    let target = home.to_path_buf();
    sweep_expiry_notices_with(home, now, move |key, threshold| {
        let home = target.clone();
        async move { deliver_notice(&home, &key, threshold).await }
    })
    .await
}

/// One sweep: claim each notice owed at `now`, attempt it with `send`, and
/// record the outcome — delivered (never sent again) or failed (retried
/// after a backoff). Returns how many were delivered.
pub(crate) async fn sweep_expiry_notices_with<F, Fut>(home: &Path, now: DateTime<Utc>, mut send: F) -> usize
where
    F: FnMut(StoredKey, NoticeThreshold) -> Fut,
    Fut: Future<Output = bool>,
{
    if !ks::store_path(home).exists() {
        return 0;
    }
    let path = home.to_path_buf();
    // The sweep's clock: `now` advanced by the real time spent in this sweep,
    // so a claim whose lease ran out while an earlier send was slow is seen
    // as lapsed by the confirm / finalise compare-and-set.
    let started = std::time::Instant::now();
    let clock = move || {
        chrono::Duration::from_std(started.elapsed())
            .ok()
            .and_then(|elapsed| now.checked_add_signed(elapsed))
            .unwrap_or(now)
    };
    let claimed = tokio::task::spawn_blocking(move || -> Result<Vec<(StoredKey, NoticeThreshold, NoticeClaim)>, String> {
        let store = McpKeyStore::open(&path)?;
        let due = store.due_notices(now).map_err(code)?;
        let mut claimed = Vec::new();
        for (key, threshold) in due {
            match store.claim_notice(&key, threshold, now) {
                Ok(Some(claim)) => claimed.push((key, threshold, claim)),
                Ok(None) => {}
                Err(e) => warn!(key_id = key.id.as_str(), error = e.code(), "MCP key expiry notice not claimed"),
            }
        }
        Ok(claimed)
    })
    .await;
    let claimed = match claimed {
        Ok(Ok(claimed)) => claimed,
        Ok(Err(error)) => {
            warn!(%error, "MCP key expiry sweep skipped");
            return 0;
        }
        Err(_) => return 0,
    };
    let mut delivered = 0;
    for (key, threshold, claim) in claimed {
        // Second review (Low): re-verify the key (id + expiry + row version)
        // and the claim by CAS immediately before the send. An expiry change,
        // disable or delete since the claim closed the notice in its own
        // transaction; such a notice is dropped here, never sent with a stale
        // date. A store error also drops it (fail closed; the lease lapses
        // and a later sweep decides afresh).
        //
        // Third review (Low): the confirm and the finalise below are bound to
        // THIS sweep's claim (its random token, `claimed_until > now`), so a
        // sweep whose lease lapsed while it was stalled cannot confirm,
        // finalise or release a notice another sweep has claimed since.
        //
        // Accepted window: the key may still change after this final confirm
        // and before the transport accepts the message (a rename, disable,
        // expiry change or delete in that instant). The message then goes
        // out describing the key as it was a moment earlier; the finalise
        // CAS below fails (the key row moved on), so the notice is not
        // counted or audited as delivered, and — unless the change closed it
        // — it is owed again after the claim lease.
        let path = home.to_path_buf();
        let check_key = key.clone();
        let check_claim = claim.clone();
        let at = clock();
        let still_current = tokio::task::spawn_blocking(move || -> Result<bool, String> {
            McpKeyStore::open(&path)?.confirm_notice_claim(&check_key, threshold, &check_claim, at).map_err(code)
        })
        .await;
        match still_current {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                info!(key_id = key.id.as_str(), threshold = threshold.as_str(),
                    "MCP key expiry notice dropped: the key changed after the notice was claimed");
                continue;
            }
            _ => {
                warn!(key_id = key.id.as_str(), threshold = threshold.as_str(),
                    "MCP key expiry notice not sent: its key could not be re-checked; it is retried after the claim lease");
                continue;
            }
        }
        let ok = send(key.clone(), threshold).await;
        let path = home.to_path_buf();
        let record_key = key.clone();
        let at = clock();
        let recorded = tokio::task::spawn_blocking(move || -> Result<NoticeOutcome, String> {
            let store = McpKeyStore::open(&path)?;
            if ok {
                store.notice_delivered(&record_key, threshold, &claim, at).map_err(code).map(NoticeOutcome::Delivered)
            } else {
                store.notice_failed(&record_key, threshold, &claim, at).map_err(code).map(NoticeOutcome::Retry)
            }
        })
        .await;
        match recorded {
            // Counted and audited only when the CAS actually marked it.
            Ok(Ok(NoticeOutcome::Delivered(true))) => {
                delivered += 1;
                let mut details = key_summary(&key);
                details["threshold"] = json!(threshold.as_str());
                details["actor"] = json!("system:expiry_notice");
                details["outcome"] = json!("delivered");
                if let Err(error) = audit(home, "mcp_key_expiry_notice", Severity::Info, details) {
                    warn!(key_id = key.id.as_str(), %error, "MCP key expiry notice audit record not written");
                }
                info!(key_id = key.id.as_str(), threshold = threshold.as_str(), "MCP key expiry notice delivered");
            }
            Ok(Ok(NoticeOutcome::Delivered(false))) => {
                warn!(key_id = key.id.as_str(), threshold = threshold.as_str(),
                    "MCP key expiry notice sent, but its claim or key changed before it was recorded; not counted as delivered");
            }
            Ok(Ok(NoticeOutcome::Retry(Some(next)))) => {
                warn!(key_id = key.id.as_str(), threshold = threshold.as_str(), retry_at = %ks::timestamp(next),
                    "MCP key expiry notice could not be delivered; it will be retried");
            }
            // Fourth review (Low): the failure CAS matched nothing — the
            // lease lapsed during the send, the claim moved on, or the key
            // changed. Nothing was rescheduled and no failure is recorded;
            // whoever holds the notice now (or a later sweep) decides.
            Ok(Ok(NoticeOutcome::Retry(None))) => {
                info!(key_id = key.id.as_str(), threshold = threshold.as_str(),
                    "MCP key expiry notice attempt failed after its claim or key changed; not rescheduled");
            }
            _ => {
                warn!(key_id = key.id.as_str(), "MCP key expiry notice outcome not recorded; the claim lease expires and it is retried");
            }
        }
    }
    delivered
}

/// What recording one attempted notice did.
enum NoticeOutcome {
    /// `notice_delivered`: whether the CAS marked this claim's notice.
    Delivered(bool),
    /// `notice_failed`: when it is due again (`None` = stale: no longer this
    /// claim's, or the key changed; nothing written).
    Retry(Option<DateTime<Utc>>),
}

/// Key lifecycle loop: the employee-gone key sweep, then the expiry
/// notices. Runs only in the process holding the gateway lock; the first
/// tick is at start.
pub(crate) async fn run_expiry_notices(home: PathBuf) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(NOTICE_SWEEP_SECS));
    loop {
        tick.tick().await;
        if !duduclaw_core::gateway_instance::held(&home) {
            continue;
        }
        // Fourth review (Medium 1): before anyone could re-create the id of
        // an employee removed outside the gateway, its keys are tombstoned.
        let path = home.clone();
        match tokio::task::spawn_blocking(move || sweep_employee_gone_keys(&path, EMPLOYEE_GONE_RECHECK)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => warn!(%error, "MCP key sweep for removed AI employees skipped; their keys stay refused"),
            Err(_) => warn!("MCP key sweep for removed AI employees did not finish; their keys stay refused"),
        }
        sweep_expiry_notices(&home, Utc::now()).await;
    }
}

#[cfg(test)]
#[path = "mcp_keys_lifecycle_tests.rs"]
mod tests;
