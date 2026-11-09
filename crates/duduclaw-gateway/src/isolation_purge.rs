//! Quarantine cleanup of tombstoned protected employees (native Linux
//! isolation v1.81, F6 design r5 §3.8, Q5, N3, R10, S12; task F6i).
//!
//! `isolation.quarantine_purge` (Admin + fresh step-up) on one quarantine row
//! that is not `pending`/`restoring`/`adopting`/`restored`, never with an
//! open request on it:
//! - **≥ 90 days** after the tombstone: purged inside the call;
//! - **< 90 days** (owner decision S12): only with `confirm_early`; the call
//!   files an `early_purge` request (digest over the row, the seal digest,
//!   the store marks, what will be removed and the `retire` flag) and nothing
//!   is removed. `isolation.employee_restore_decide` approves it under the
//!   F6c2 rule ([`PurgeRule`]: another qualifying Admin, else sole-Admin
//!   self-approval with step-up, the 7-day cooling and the `self_approved`
//!   audit; the requester and the Personal owner are both parties), the
//!   digest is re-checked, and the purge runs inside that approval call; the
//!   row records `early_purge = request_id`.
//!
//! One apply.db transaction marks the row `purging` (requests on it are then
//! refused) and writes the purge journal; then the tree is removed through
//! held descriptors without following any symlink (`isolation_purge_tree.rs`),
//! the label-keyed stores sealed for the row (`quarantine:<hex>-<inc>`, F6h
//! `purge_row_stores`: held website grants deleted for good) and — while no
//! live row holds the label and no other row on it is open — everything
//! still under the label in a registered store (`purge_all`); both must then
//! report a positive zero (S10); then `purged`, `purged_at`. An `adopted` row
//! keeps only its sealed remainder and sealed stores, so that is what goes;
//! its `purging` state (journal `prior_seal = adopted`) stays closed for the
//! label (`isolation_label_stores::label_hold`, F6 b3 N1), so the live
//! employee that adopted the label keeps writing while the purge runs and
//! after a refused or incomplete one. A crash leaves the row `purging`: [`recover_purges`] finishes it before
//! anything else runs for the label (every purge entry point and boot), as it
//! does for a `purged` row whose tree or row stores are still present.
//! Purge never touches the root allocation; the RPC stages F6l's retire
//! right after the commit when 同時釋放主機資源 is ticked.
//!
//! The audit of a purge keeps the seal digest, the sealed manifest totals,
//! the store marks and what was removed, so what was destroyed stays
//! provable after the data is gone (M7). Notices go to the Personal owner's
//! tombstone-time address and every log-eligible Admin, at request and at
//! purge, never capped. Lock order apply.db before users.db (M6).
use std::path::Path;

use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::contract::ConfirmationStatus;
use duduclaw_security::audit::Severity;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::fs_safe::sqlite::Access;
use crate::isolation_label_stores::{self as stores, quarantine_key};
use crate::isolation_lifecycle::{self as lifecycle, EmployeeKind, CONFIRMATIONS_DB, USERS_DB};
use crate::isolation_restore::{self as restore, Quarantined, Refusal, RequestKind, RestoreEnv, RestoreRequest};
use crate::isolation_restore_wiring::{self as wiring, NoticeScope};

#[cfg(unix)]
#[path = "isolation_purge_tree.rs"]
pub(crate) mod tree;
#[path = "isolation_purge_rpc.rs"]
mod rpc;
pub(crate) use rpc::{decide_early, extend_view, quarantine_purge_rpc};

/// Q5: a row this old is purged inside the call; younger is an early purge.
pub(crate) const PURGE_AFTER_SECS: i64 = restore::RESTORE_WINDOW_SECS;
const RATE_WINDOW_SECS: i64 = 30 * 86_400;
const DIGEST_DOMAIN: &[u8] = b"duduclaw-early-purge-v1\0";
/// States a purge may start from (§3.4: any but pending/restoring/adopting/restored).
const PURGEABLE: [&str; 6] = ["sealed", "oversize", "invalid", "absent", "needs_reseal", "adopted"];

/// Additive apply.db tables (no employee schema bump): the purge journal
/// (what a started purge removes, so a crash can finish it and the audit can
/// prove it) and the early-purge request facts the request digest covers.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS employee_purge_journal (
        employee_hex TEXT NOT NULL CHECK (length(employee_hex) = 32 AND employee_hex NOT GLOB '*[^0-9a-f]*'),
        incarnation INTEGER NOT NULL CHECK (incarnation >= 0 AND incarnation <= 4294967295),
        prior_seal TEXT NOT NULL,
        request_id TEXT,
        preview TEXT NOT NULL,
        retire INTEGER NOT NULL CHECK (retire IN (0, 1)),
        actor_user_id TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        finished_at INTEGER,
        PRIMARY KEY (employee_hex, incarnation));
    CREATE TABLE IF NOT EXISTS employee_purge_requests (
        request_id TEXT PRIMARY KEY CHECK (length(request_id) = 32 AND request_id NOT GLOB '*[^0-9a-f]*'),
        retire INTEGER NOT NULL CHECK (retire IN (0, 1)),
        preview TEXT NOT NULL);";

/// Why a purge step refused. [`Self::code`] is stable (F6e maps it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PurgeError {
    /// A restore-engine refusal (`NOT_FOUND`, `RESTORE_IN_PROGRESS`,
    /// `RETIRE_IN_PROGRESS`, `RATE_LIMITED`, `RESTORE_EXPIRED`, approval codes, …).
    Restore(Refusal),
    /// The row is `pending`, `restored`, `purging` or `purged`.
    NotPurgeable,
    /// A request on the row is open (purge never runs beside one).
    OpenRequest,
    /// Younger than 90 days without `confirm_early`.
    ConfirmEarly,
    /// What the purge would remove changed since the request (digest re-check).
    Changed,
    /// The row's quarantine name does not hold the recorded tree: refused, alerted.
    TreeRefused,
    /// The removal or a store purge failed; the row stays `purging` and the
    /// next call (or boot) finishes it.
    Incomplete,
}

impl From<Refusal> for PurgeError { fn from(r: Refusal) -> Self { Self::Restore(r) } }

impl PurgeError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Restore(refusal) => refusal.code(),
            Self::NotPurgeable => "PURGE_NOT_ALLOWED",
            Self::OpenRequest => "PURGE_REQUEST_OPEN",
            Self::ConfirmEarly => "EARLY_PURGE_CONFIRM_REQUIRED",
            Self::Changed => "PURGE_CHANGED",
            Self::TreeRefused => "PURGE_TREE_REFUSED",
            Self::Incomplete => "PURGE_INCOMPLETE",
        }
    }
    /// zh-TW wording (design §4). `Restore(_)` is worded by the RPC layer.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Restore(_) => "員工資料目前無法讀取，已拒絕操作。",
            Self::NotPurgeable => "這筆隔離區資料目前不能清除：尚未完成封存、已還原，或已清除。",
            Self::OpenRequest => "這筆隔離區資料有申請進行中，請先核准、拒絕或取消該申請。",
            Self::ConfirmEarly => "註銷未滿 90 天的資料需確認提前清除：清除後無法還原，也不能再由新員工接手，並需由另一位管理員核准。",
            Self::Changed => "要清除的資料已變更，請重新載入後再試（提前清除需重新申請）。",
            Self::TreeRefused => "隔離區資料與註銷時不一致，已拒絕清除並通知維運。",
            Self::Incomplete => "清除尚未完成，系統會在下次操作時繼續完成，請稍後再試。",
        }
    }
}

fn unavailable<E>(_: E) -> PurgeError { PurgeError::Restore(Refusal::Unavailable) }

// ── Approval rule (F6c2, S12) ──────────────────────────────────────────

/// F6c2's rule for kind `early_purge`, inside the users.db write transaction
/// of the approval (`users`). Parties are the requester and, for a Personal
/// row, the owner (`owner`): another qualifying Admin approves, else a party
/// self-approves as sole Admin (step-up, 7-day cooling). `Ok(true)` =
/// self-approved. `Err` is the F6c2 refusal code.
pub(crate) trait PurgeRule {
    fn decide_purge(&self, users: &Connection, requester: &str, owner: Option<&str>, approver: &str) -> Result<bool, &'static str>;
    /// F6c2 `record_self_approval` (users.db `isolation.employee_restore_self_approved`,
    /// target = the request id) on the same transaction.
    fn record_purge_self_approval(&self, users: &Connection, requester: &str, owner: Option<&str>, approver: &str, subject: &str)
        -> Result<(), &'static str>;
}

/// Production: the decide RPC's [`wiring::LogRule`] (its `fresh_step_up` is
/// the call's verified step-up) over F6c2's `EligibilityLogRule`.
impl PurgeRule for wiring::LogRule {
    fn decide_purge(&self, users: &Connection, requester: &str, owner: Option<&str>, approver: &str) -> Result<bool, &'static str> {
        use duduclaw_auth::admin_approval_rule::{AdminApprovalRule, ApprovalDecision, EligibilityLogRule};
        match EligibilityLogRule.decide(users, &purge_request(requester, owner, approver, self.fresh_step_up, self.now)) {
            ApprovalDecision::Allowed { self_approved, .. } => Ok(self_approved),
            ApprovalDecision::Refused(refusal) => Err(refusal.code()),
        }
    }
    fn record_purge_self_approval(&self, users: &Connection, requester: &str, owner: Option<&str>, approver: &str, subject: &str)
        -> Result<(), &'static str> {
        duduclaw_auth::admin_approval_rule::record_self_approval(users,
            &purge_request(requester, owner, approver, self.fresh_step_up, self.now), subject)
            .map_err(|_| wiring::SELF_APPROVAL_AUDIT_UNAVAILABLE)
    }
}

pub(crate) fn purge_request<'a>(requester: &'a str, owner: Option<&'a str>, approver: &'a str, fresh_step_up: bool, now: i64)
    -> duduclaw_auth::admin_approval_rule::ApprovalRequest<'a> {
    duduclaw_auth::admin_approval_rule::ApprovalRequest { kind: duduclaw_auth::admin_approval_rule::ApprovalKind::EarlyPurge,
        requester_user_id: requester, owner_user_id: owner, approver_user_id: approver, fresh_step_up, now }
}

// ── Test hook ───────────────────────────────────────────────────────────

/// Where a test may simulate a crash (the call returns at once, no cleanup).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PurgeHook { AfterStart, AfterTree, AfterStores }

#[cfg(test)]
thread_local! {
    pub(crate) static PURGE_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(PurgeHook) -> bool>>> = std::cell::RefCell::new(None);
}

fn crash_at(point: PurgeHook) -> bool {
    #[cfg(test)]
    { PURGE_HOOK.with(|hook| hook.borrow_mut().as_mut().is_some_and(|hook| hook(point))) }
    #[cfg(not(test))]
    { let _ = point; false }
}

// ── Rows, journal, preview ──────────────────────────────────────────────

fn open(home: &Path) -> Result<Connection, PurgeError> {
    let db = restore::open_apply(home)?;
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    Ok(db)
}

fn read_row(home: &Path, employee: EmployeeId, incarnation: u32) -> Result<Quarantined, PurgeError> {
    let db = open(home)?;
    Ok(restore::read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)?)
}

/// What a started purge removes (written in the transaction that marks the row `purging`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Journal {
    pub(crate) prior_seal: String,
    pub(crate) request_id: Option<String>,
    pub(crate) preview: Value,
    pub(crate) retire: bool,
    pub(crate) actor: String,
}

fn read_journal(db: &Connection, employee: EmployeeId, incarnation: u32) -> Result<Option<Journal>, PurgeError> {
    let row: Option<(String, Option<String>, String, bool, String)> = db.query_row(
        "SELECT prior_seal,request_id,preview,retire,actor_user_id FROM employee_purge_journal WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(employee.0), incarnation], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .optional().map_err(unavailable)?;
    row.map(|(prior_seal, request_id, preview, retire, actor)| Ok(Journal {
        prior_seal, request_id, preview: serde_json::from_str(&preview).map_err(unavailable)?, retire, actor }))
        .transpose()
}

/// `(retire, preview)` of an early-purge request.
fn request_facts(db: &Connection, request_id: &str) -> Result<(bool, Value), PurgeError> {
    let (retire, preview): (bool, String) = db.query_row("SELECT retire,preview FROM employee_purge_requests WHERE request_id=?1",
        [request_id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(unavailable)?.ok_or(Refusal::Unavailable)?;
    Ok((retire, serde_json::from_str(&preview).map_err(unavailable)?))
}

/// What was removed from the tree (directories, files and links below the root).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Removed { pub(crate) entries: u64, pub(crate) file_bytes: u64 }

/// The sealed stat manifest's totals: entries and the sum of its size
/// column (directories included, as recorded at the tombstone).
fn manifest_totals(q: &Quarantined) -> Value {
    match q.stat_manifest.as_ref().and_then(|manifest| manifest["entries"].as_array()) {
        Some(entries) => json!({"entries": entries.len(),
            "bytes": entries.iter().filter_map(|entry| entry.get(2).and_then(Value::as_u64)).fold(0u64, u64::saturating_add)}),
        None => Value::Null,
    }
}

fn counts(home: &Path, key: &str) -> Result<Value, PurgeError> {
    let mut map = serde_json::Map::new();
    for (name, count) in stores::counts_under(home, key) {
        map.insert(name.into(), json!(count.map_err(|_| Refusal::StoresUnavailable)?));
    }
    Ok(Value::Object(map))
}

/// The label's own stores go with the row only while no live row holds the
/// label and no other row on it is open (their state is theirs, R7/N3).
fn label_stores_go(home: &Path, q: &Quarantined) -> Result<bool, PurgeError> {
    let hold = stores::label_hold(home, &q.label).map_err(unavailable)?;
    let own = hex::encode(q.employee.0);
    Ok(hold.confirmed != Some(false) && hold.open_rows.iter().all(|(hex, incarnation, _)| *hex == own && *incarnation == q.incarnation))
}

/// What a purge of `q` would destroy, as the dialog names it and the
/// early-purge digest covers it. Any unreadable store refuses.
pub(crate) fn preview(home: &Path, q: &Quarantined) -> Result<Value, PurgeError> {
    let label_stores = if label_stores_go(home, q)? { counts(home, &q.label)? } else { Value::Null };
    Ok(json!({
        "seal": q.seal, "seal_digest": q.digest, "manifest": manifest_totals(q), "store_marks": q.store_marks,
        "row_stores": counts(home, &quarantine_key(q.employee, q.incarnation))?, "label_stores": label_stores,
    }))
}

fn put(hash: &mut Sha256, tag: &str, bytes: &[u8]) {
    hash.update((tag.len() as u64).to_be_bytes());
    hash.update(tag.as_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// S12 request digest: the request, the row, the seal digest, the store
/// marks and what will be removed (inside `preview`), and the `retire` flag.
pub(crate) fn purge_digest(request: &RestoreRequest, preview: &Value, retire: bool) -> String {
    let mut hash = Sha256::new();
    hash.update(DIGEST_DOMAIN);
    put(&mut hash, "request", request.request_id.as_bytes());
    put(&mut hash, "kind", request.kind.as_str().as_bytes());
    put(&mut hash, "employee", &request.employee.0);
    put(&mut hash, "incarnation", &request.incarnation.to_be_bytes());
    put(&mut hash, "label", request.label.as_bytes());
    put(&mut hash, "tenant", request.tenant.as_bytes());
    put(&mut hash, "owner", request.owner.as_deref().unwrap_or("").as_bytes());
    put(&mut hash, "requester", request.requester.as_bytes());
    put(&mut hash, "preview", preview.to_string().as_bytes());
    put(&mut hash, "retire", if retire { b"1" } else { b"0" });
    hex::encode(hash.finalize())
}

// ── Checks ──────────────────────────────────────────────────────────────

/// Inside the caller's apply.db transaction: the row as read outside, in a
/// purgeable state, no open request on it (`own`: the approving
/// early-purge request), nothing restoring/adopting/purging/applying on the
/// label, no retire in progress for any id that held it. Returns the row.
fn check_purgeable(db: &Connection, q: &Quarantined, own: Option<&str>) -> Result<Quarantined, PurgeError> {
    let fresh = restore::read_row(db, q.employee, q.incarnation)?.ok_or(Refusal::NotFound)?;
    if fresh.label != q.label || fresh.digest != q.digest || fresh.root != q.root { return Err(Refusal::Inconsistent.into()); }
    match fresh.seal.as_str() {
        "restoring" | "adopting" => return Err(Refusal::RestoreInProgress.into()),
        seal if PURGEABLE.contains(&seal) => {}
        _ => return Err(PurgeError::NotPurgeable),
    }
    if fresh.seal != q.seal { return Err(PurgeError::Changed); }
    for (employee, incarnation, seal) in restore::rows_for_label(db, &q.label)? {
        if (employee, incarnation) != (q.employee, q.incarnation) && matches!(seal.as_str(), "restoring" | "adopting" | "purging") {
            return Err(Refusal::RestoreInProgress.into());
        }
    }
    match (restore::open_request(db, q.employee, q.incarnation)?, own) {
        (None, None) => {}
        (Some(open), Some(id)) if open.request_id == id && open.state == "requested" && open.kind == RequestKind::EarlyPurge => {}
        _ => return Err(PurgeError::OpenRequest),
    }
    let applying: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM employee_restores WHERE label=?1 AND state='applying')",
        [&q.label], |r| r.get(0)).map_err(unavailable)?;
    if applying { return Err(Refusal::RestoreInProgress.into()); }
    let retiring: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM root_allocations WHERE root_state='retiring'
            AND employee_hex IN (SELECT employee_hex FROM employee_quarantine WHERE label=?1))",
        [&q.label], |r| r.get(0)).map_err(unavailable)?;
    if retiring { return Err(Refusal::RetireInProgress.into()); }
    Ok(fresh)
}

/// An `adopted` row is purged only once its adoption proposal is Applied
/// (an adoption still finishing validates against the remainder).
fn adoption_settled(home: &Path, q: &Quarantined) -> Result<(), PurgeError> {
    if q.seal != "adopted" { return Ok(()); }
    let request = q.closed_by_request.as_deref().ok_or(Refusal::Inconsistent)?;
    let proposal: Option<Option<String>> = open(home)?.query_row("SELECT proposal_id FROM employee_restores
        WHERE request_id=?1 AND state='done' AND kind IN ('personal_adopt','shared_adopt')", [request], |r| r.get(0))
        .optional().map_err(unavailable)?;
    let Some(Some(proposal)) = proposal else { return Err(Refusal::RestoreInProgress.into()) };
    let store = duduclaw_migration::confirmation::ConfirmationStore::from_read_only_connection(
        lifecycle::open_store(home, CONFIRMATIONS_DB, Access::ReadOnly).map_err(unavailable)?).map_err(unavailable)?;
    let status = store.list().map_err(unavailable)?.into_iter()
        .filter(|item| hex::encode(item.binding.operation_id.proposal_id.0) == proposal)
        .max_by_key(|item| item.binding.operation_id.revision).map(|item| item.status);
    if matches!(status, Some(ConfirmationStatus::Applied)) { Ok(()) } else { Err(Refusal::RestoreInProgress.into()) }
}

// ── The tree (unix) ─────────────────────────────────────────────────────

#[cfg(unix)]
fn quarantine_root(home: &Path) -> Result<Option<std::os::fd::OwnedFd>, PurgeError> {
    use std::os::unix::fs::MetadataExt;
    let home_dir = crate::fs_safe::SafeDir::open_nofollow(home, false).map_err(unavailable)?;
    let Some(root) = home_dir.child_dir(lifecycle::QUARANTINE_DIR, false).map_err(unavailable)? else { return Ok(None) };
    let metadata = root.metadata().map_err(unavailable)?;
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 { return Err(unavailable(())); }
    Ok(Some(root.duplicate_fd().map_err(unavailable)?))
}

#[cfg(unix)]
fn expect(q: &Quarantined, prior_seal: &str) -> tree::Expect {
    match (prior_seal, q.root) {
        ("absent", _) | (_, None) => tree::Expect::Nothing,
        ("invalid", Some(_)) => tree::Expect::AnyDir,
        (_, Some(root)) => tree::Expect::Root(root),
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn tree_refused(home: &Path, q: &Quarantined, reason: &str) -> PurgeError {
    restore::audit(home, "isolation.quarantine_purge_tree_refused", &q.label, Severity::Critical, json!({
        "employee": hex::encode(q.employee.0), "incarnation": q.incarnation, "reason": reason}));
    PurgeError::TreeRefused
}

/// Before anything changes: the row's name holds what the row recorded.
/// `Ok(true)` when a tree is present.
fn inspect_tree(home: &Path, q: &Quarantined, prior_seal: &str) -> Result<bool, PurgeError> {
    #[cfg(unix)]
    {
        let Some(root) = quarantine_root(home)? else { return Ok(false) };
        match tree::inspect(&root, &q.name(), expect(q, prior_seal)) {
            Ok(present) => Ok(present),
            Err(tree::TreeError::Refused(reason)) => Err(tree_refused(home, q, reason)),
            Err(tree::TreeError::Io) => Err(unavailable(())),
        }
    }
    #[cfg(not(unix))]
    { let _ = (home, q, prior_seal); Err(unavailable(())) }
}

fn remove_tree(home: &Path, q: &Quarantined, prior_seal: &str) -> Result<Removed, PurgeError> {
    #[cfg(unix)]
    {
        let Some(root) = quarantine_root(home)? else { return Ok(Removed::default()) };
        match tree::remove(&root, &q.name(), expect(q, prior_seal), nix::unistd::geteuid().as_raw()) {
            Ok(removed) => Ok(removed.unwrap_or_default()),
            Err(tree::TreeError::Refused(reason)) => Err(tree_refused(home, q, reason)),
            Err(tree::TreeError::Io) => Err(PurgeError::Incomplete),
        }
    }
    #[cfg(not(unix))]
    { let _ = (home, q, prior_seal); Err(unavailable(())) }
}

// ── Notices ─────────────────────────────────────────────────────────────

/// Uncapped notice to every log-eligible Admin (S16) and, for a Personal
/// row, the owner's tombstone-time address.
fn notify(home: &Path, q: &Quarantined, event: &str, mut details: Value, now: i64) -> bool {
    let mut recipients = wiring::admin_notice_addresses(home, &q.label, event, now);
    if let Some(address) = q.principal_snapshot["notice_address"].as_str() { recipients.push(address.to_owned()); }
    details["label"] = json!(q.label);
    details["employee"] = json!(hex::encode(q.employee.0));
    details["incarnation"] = json!(q.incarnation);
    let scope = match &q.owner { Some(owner) => NoticeScope::Owner(owner.clone()), None => NoticeScope::Tenant(q.tenant.clone()) };
    wiring::queue_or_alert(home, &scope, event, &recipients, &details, false, now)
}

// ── Purge ───────────────────────────────────────────────────────────────

/// The dialog's choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PurgeOptions {
    /// The Admin confirmed an early purge (< 90 days) in the dialog.
    pub(crate) confirm_early: bool,
    /// 同時釋放主機資源: stage the F6l retire right after the purge commits.
    pub(crate) retire: bool,
}

/// A finished purge.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Purged {
    pub(crate) employee: EmployeeId,
    pub(crate) incarnation: u32,
    pub(crate) label: String,
    /// The approved `early_purge` request (`None`: purged after 90 days).
    pub(crate) request_id: Option<String>,
    pub(crate) approver: Option<String>,
    pub(crate) self_approved: Option<bool>,
    pub(crate) removed: Removed,
    pub(crate) row_stores: Value,
    pub(crate) label_stores: Value,
    pub(crate) purged_at: i64,
    pub(crate) retire: bool,
}

/// What `isolation.quarantine_purge` did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PurgeOutcome {
    Purged(Purged),
    /// Early purge: the request waits for a second qualifying Admin.
    Requested { request: RestoreRequest, preview: Value, notified: bool },
}

/// `isolation.quarantine_purge` engine. `env.actor` is the verified Admin
/// (fresh step-up checked by the caller); `env.now` decides the 90 days.
pub(crate) fn purge(home: &Path, env: &RestoreEnv<'_>, employee: EmployeeId, incarnation: u32, options: PurgeOptions)
    -> Result<PurgeOutcome, PurgeError> {
    let peek = read_row(home, employee, incarnation)?;
    // §3.6: interrupted restores and purges on the label finish first.
    restore::recover_label(home, env, &peek.label)?;
    if let Some(done) = recover_purges(home, &peek.label, env.now)?.into_iter()
        .find(|purged| purged.employee == employee && purged.incarnation == incarnation) {
        return Ok(PurgeOutcome::Purged(done));
    }
    let q = read_row(home, employee, incarnation)?;
    { let db = open(home)?; check_purgeable(&db, &q, None)?; }
    adoption_settled(home, &q)?;
    inspect_tree(home, &q, &q.seal)?;
    let preview = preview(home, &q)?;
    if env.now < q.quarantined_at { return Err(Refusal::Inconsistent.into()); }
    if env.now - q.quarantined_at < PURGE_AFTER_SECS {
        if !options.confirm_early { return Err(PurgeError::ConfirmEarly); }
        return file_early(home, env, &q, preview, options.retire);
    }
    let journal = Journal { prior_seal: q.seal.clone(), request_id: None, preview, retire: options.retire, actor: env.actor.to_owned() };
    {
        let mut db = open(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        check_purgeable(&tx, &q, None)?;
        mark_started(&tx, &q, &journal, env.now)?;
        tx.commit().map_err(unavailable)?;
    }
    if crash_at(PurgeHook::AfterStart) { return Err(unavailable(())); }
    finish(home, &q, &journal, env.now, false).map(PurgeOutcome::Purged)
}

/// File the early-purge request (S12): nothing is removed.
fn file_early(home: &Path, env: &RestoreEnv<'_>, q: &Quarantined, preview: Value, retire: bool) -> Result<PurgeOutcome, PurgeError> {
    let request = {
        let mut db = open(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        check_purgeable(&tx, q, None)?;
        let recent: i64 = tx.query_row("SELECT count(*) FROM employee_restores WHERE employee_hex=?1 AND incarnation=?2 AND created_at>?3",
            params![hex::encode(q.employee.0), q.incarnation, env.now - RATE_WINDOW_SECS], |r| r.get(0)).map_err(unavailable)?;
        if recent >= restore::MAX_REQUESTS_PER_ROW { return Err(Refusal::RateLimited.into()); }
        let unsigned = RestoreRequest {
            request_id: hex::encode(uuid::Uuid::new_v4().as_bytes()), kind: RequestKind::EarlyPurge, employee: q.employee,
            incarnation: q.incarnation, label: q.label.clone(), owner: q.owner.clone(), tenant: q.tenant.clone(),
            requester: env.actor.to_owned(), digest: String::new(), route_digest: restore::route_digest(&[]), member_selection: None,
            created_at: env.now,
            expires_at: env.now.saturating_add(restore::REQUEST_TTL_SECS).min(q.quarantined_at.saturating_add(PURGE_AFTER_SECS)),
            state: "requested".into(), approver: None, self_approved: None, failure: None, filed_by_admin: None,
        };
        let request = RestoreRequest { digest: purge_digest(&unsigned, &preview, retire), ..unsigned };
        tx.execute("INSERT INTO employee_restores(request_id,kind,employee_hex,incarnation,label,owner_user_id,tenant_id,
                requester_user_id,digest,route_digest,created_at,expires_at,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'requested')",
            params![request.request_id, request.kind.as_str(), hex::encode(q.employee.0), q.incarnation, request.label, request.owner,
                request.tenant, request.requester, request.digest, request.route_digest, request.created_at, request.expires_at])
            .map_err(unavailable)?;
        tx.execute("INSERT INTO employee_purge_requests(request_id,retire,preview) VALUES(?1,?2,?3)",
            params![request.request_id, retire, preview.to_string()]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        request
    };
    restore::audit(home, "isolation.quarantine_purge_requested", &q.label, Severity::Warning,
        restore::audit_details(&request, json!({"retire": retire, "lost": preview})));
    let notified = notify(home, q, "isolation.quarantine_purge_requested", json!({"request_id": request.request_id,
        "requester_user_id": request.requester, "expires_at": request.expires_at, "early": true, "retire": retire}), env.now);
    Ok(PurgeOutcome::Requested { request, preview, notified })
}

fn refused(home: &Path, request: &RestoreRequest, error: PurgeError) -> PurgeError {
    restore::audit(home, "isolation.quarantine_purge_refused", &request.label, Severity::Warning,
        restore::audit_details(request, json!({"refusal": error.code()})));
    error
}

/// `isolation.employee_restore_decide` approve of an `early_purge` request
/// (Admin + step-up checked by the caller; `rule` carries that step-up).
/// The purge runs inside this call after the digest re-check.
pub(crate) fn approve_early(home: &Path, env: &RestoreEnv<'_>, rule: &dyn PurgeRule, request_id: &str, approver: &str)
    -> Result<Purged, PurgeError> {
    let read = || -> Result<RestoreRequest, PurgeError> { Ok(restore::read_request(&open(home)?, request_id)?.ok_or(Refusal::NotFound)?) };
    let request = read()?;
    if request.kind != RequestKind::EarlyPurge { return Err(Refusal::NotSupported.into()); }
    restore::recover_label(home, env, &request.label)?;
    recover_purges(home, &request.label, env.now)?;
    let request = read()?;
    match request.state.as_str() {
        "requested" if env.now <= request.expires_at => {}
        "requested" | "expired" => return Err(Refusal::Expired.into()),
        _ => return Err(Refusal::NotFound.into()),
    }
    let q = read_row(home, request.employee, request.incarnation)?;
    let (retire, _) = request_facts(&open(home)?, request_id)?;
    let outside = (|| -> Result<Value, PurgeError> {
        adoption_settled(home, &q)?;
        inspect_tree(home, &q, &q.seal)?;
        preview(home, &q)
    })();
    let preview = outside.map_err(|error| refused(home, &request, error))?;
    let journal = Journal { prior_seal: q.seal.clone(), request_id: Some(request_id.to_owned()), preview: preview.clone(), retire,
        actor: request.requester.clone() };
    let mut db = open(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    if let Err(error) = check_purgeable(&tx, &q, Some(request_id)) {
        drop(tx);
        return Err(refused(home, &request, error));
    }
    if purge_digest(&request, &preview, retire) != request.digest {
        // Terminal: what would be removed is not what the request named.
        restore::set_request_state(&tx, request_id, &["requested"], "failed", Some(PurgeError::Changed.code()), env.now)?;
        tx.commit().map_err(unavailable)?;
        restore::audit(home, "isolation.quarantine_purge_failed", &q.label, Severity::Warning,
            restore::audit_details(&request, json!({"failure": PurgeError::Changed.code(), "lost": preview})));
        return Err(PurgeError::Changed);
    }
    // R1/S12 in the users.db write transaction, inside the apply.db one (M6).
    let self_approved = match decide_in_users(home, rule, &request, approver) {
        Ok(self_approved) => self_approved,
        Err(error) => { drop(tx); return Err(refused(home, &request, error)); }
    };
    restore::set_request_state(&tx, request_id, &["requested"], "applying", None, env.now)?;
    tx.execute("UPDATE employee_restores SET approver_user_id=?2,self_approved=?3 WHERE request_id=?1",
        params![request_id, approver, self_approved]).map_err(unavailable)?;
    mark_started(&tx, &q, &journal, env.now)?;
    tx.commit().map_err(unavailable)?;
    drop(db);
    let approved = RestoreRequest { state: "applying".into(), approver: Some(approver.to_owned()), self_approved: Some(self_approved), ..request };
    restore::audit(home, "isolation.quarantine_purge_approved", &q.label, Severity::Warning, restore::audit_details(&approved, json!({})));
    if self_approved {
        restore::audit(home, "isolation.quarantine_purge_self_approved", &q.label, Severity::Warning, restore::audit_details(&approved, json!({})));
    }
    if crash_at(PurgeHook::AfterStart) { return Err(unavailable(())); }
    finish(home, &q, &journal, env.now, false)
}

/// The approval rule in one users.db IMMEDIATE transaction; a self-approval
/// writes F6c2's users.db row there, once per request (a retried approval
/// after a failed apply.db commit finds it).
fn decide_in_users(home: &Path, rule: &dyn PurgeRule, request: &RestoreRequest, approver: &str) -> Result<bool, PurgeError> {
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadWrite).map_err(unavailable)?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    let owner = request.owner.as_deref();
    let self_approved = rule.decide_purge(&tx, &request.requester, owner, approver).map_err(Refusal::Approval)?;
    // A party (requester or owner) approves only as sole Admin, never anyone else.
    if self_approved != (approver == request.requester || owner == Some(approver)) {
        return Err(Refusal::Approval("APPROVAL_RULE_INCONSISTENT").into());
    }
    if self_approved {
        let written: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM auth_audit_log WHERE action=?1 AND target=?2)",
            params![duduclaw_auth::admin_approval_rule::SELF_APPROVED_AUDIT_ACTION, request.request_id], |r| r.get(0))
            .map_err(unavailable)?;
        if !written {
            rule.record_purge_self_approval(&tx, &request.requester, owner, approver, &request.request_id).map_err(Refusal::Approval)?;
        }
    }
    tx.commit().map_err(unavailable)?;
    Ok(self_approved)
}

/// The transaction that starts a purge: row `purging` (and `early_purge`),
/// journal row written.
fn mark_started(tx: &Connection, q: &Quarantined, journal: &Journal, now: i64) -> Result<(), PurgeError> {
    restore::set_seal(tx, q.employee, q.incarnation, &[journal.prior_seal.as_str()], "purging")?;
    if let Some(id) = &journal.request_id {
        tx.execute("UPDATE employee_quarantine SET early_purge=?3 WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(q.employee.0), q.incarnation, id]).map_err(unavailable)?;
    }
    tx.execute("INSERT INTO employee_purge_journal(employee_hex,incarnation,prior_seal,request_id,preview,retire,actor_user_id,started_at)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![hex::encode(q.employee.0), q.incarnation, journal.prior_seal, journal.request_id, journal.preview.to_string(),
            journal.retire, journal.actor, now]).map_err(unavailable)?;
    Ok(())
}

fn incomplete(home: &Path, q: &Quarantined, step: &str, store: Option<&str>) -> PurgeError {
    restore::audit(home, "isolation.quarantine_purge_failed", &q.label, Severity::Warning, json!({
        "employee": hex::encode(q.employee.0), "incarnation": q.incarnation, "failure": PurgeError::Incomplete.code(),
        "step": step, "store": store}));
    PurgeError::Incomplete
}

fn store_map(purged: &[(&'static str, u64)]) -> Value {
    Value::Object(purged.iter().map(|(name, count)| ((*name).to_owned(), json!(count))).collect())
}

/// Remove the tree and the stores, then `purged` (idempotent: a crash at
/// any point is finished by the next run). `recovered`: run by recovery.
fn finish(home: &Path, q: &Quarantined, journal: &Journal, now: i64, recovered: bool) -> Result<Purged, PurgeError> {
    let removed = remove_tree(home, q, &journal.prior_seal)?;
    if crash_at(PurgeHook::AfterTree) { return Err(unavailable(())); }
    let row_stores = crate::isolation_label_restore::purge_row_stores(home, q.employee, q.incarnation)
        .map_err(|(store, _)| incomplete(home, q, "row_stores", Some(store)))?;
    let label_go = label_stores_go(home, q)?;
    let label_stores = if label_go {
        Some(stores::purge_all(home, &q.label).map_err(|(store, _)| incomplete(home, q, "label_stores", Some(store)))?)
    } else { None };
    if crash_at(PurgeHook::AfterStores) { return Err(unavailable(())); }
    // S10: a positive zero under the row key, and under the label when it went too.
    let zero = |key: &str| matches!(stores::label_free(home, key), Ok(true));
    if !zero(&quarantine_key(q.employee, q.incarnation)) || (label_go && !zero(&q.label)) {
        return Err(incomplete(home, q, "zero_count", None));
    }
    let (purged_at, request) = {
        let mut db = open(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        let current = restore::read_row(&tx, q.employee, q.incarnation)?.ok_or(Refusal::NotFound)?;
        match current.seal.as_str() {
            "purging" => restore::set_seal(&tx, q.employee, q.incarnation, &["purging"], "purged")?,
            "purged" => {}
            _ => return Err(Refusal::Inconsistent.into()),
        }
        tx.execute("UPDATE employee_quarantine SET purged_at=COALESCE(purged_at,?3) WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(q.employee.0), q.incarnation, now]).map_err(unavailable)?;
        tx.execute("UPDATE employee_purge_journal SET finished_at=COALESCE(finished_at,?3) WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(q.employee.0), q.incarnation, now]).map_err(unavailable)?;
        let request = match &journal.request_id {
            Some(id) => {
                let request = restore::read_request(&tx, id)?.ok_or(Refusal::Inconsistent)?;
                if request.state == "applying" { restore::set_request_state(&tx, id, &["applying"], "done", None, now)?; }
                Some(RestoreRequest { state: "done".into(), ..request })
            }
            None => None,
        };
        let purged_at: i64 = tx.query_row("SELECT purged_at FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(q.employee.0), q.incarnation], |r| r.get(0)).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        (purged_at, request)
    };
    // The row is closed now: writer-gate answers cached for the label are stale.
    duduclaw_memory::label_gate::invalidate_cached_answers();
    // SR5 item 2: a purged Personal row no longer carries an owner.
    crate::personal_visibility::invalidate_owner_cache(home);
    let purged = Purged {
        employee: q.employee, incarnation: q.incarnation, label: q.label.clone(), request_id: journal.request_id.clone(),
        approver: request.as_ref().and_then(|r| r.approver.clone()), self_approved: request.as_ref().and_then(|r| r.self_approved),
        removed, row_stores: store_map(&row_stores), label_stores: label_stores.as_deref().map_or(Value::Null, store_map),
        purged_at, retire: journal.retire,
    };
    restore::audit(home, "isolation.quarantine_purged", &q.label, Severity::Warning, json!({
        "employee": hex::encode(q.employee.0), "incarnation": q.incarnation, "label": q.label,
        "kind": match q.kind { EmployeeKind::Personal => "personal", EmployeeKind::Shared => "shared" }, "owner_user_id": q.owner,
        "early": purged.request_id.is_some(), "request_id": purged.request_id, "requester_user_id": journal.actor,
        "approver_user_id": purged.approver, "self_approved": purged.self_approved, "prior_seal": journal.prior_seal,
        "seal_digest": q.digest, "digest_prefix": q.digest.as_deref().map(|d| duduclaw_core::truncate_chars(d, 16)),
        "sealed_manifest": manifest_totals(q), "store_marks": q.store_marks,
        "removed": {"entries": removed.entries, "file_bytes": removed.file_bytes},
        "row_stores": purged.row_stores, "label_stores": purged.label_stores, "lost": journal.preview,
        "retire": journal.retire, "purged_at": purged_at, "recovered": recovered}));
    if !recovered || journal.prior_seal != "purged" {
        notify(home, q, "isolation.quarantine_purged", json!({"request_id": purged.request_id, "early": purged.request_id.is_some(),
            "approver_user_id": purged.approver, "self_approved": purged.self_approved,
            "digest_prefix": q.digest.as_deref().map(|d| duduclaw_core::truncate_chars(d, 16)), "purged_at": purged_at}), now);
    }
    Ok(purged)
}

// ── Crash recovery (§3.6) ──────────────────────────────────────────────

/// Is anything of a `purged` row still there (its tree or its row stores)?
fn leftovers(home: &Path, q: &Quarantined) -> Result<bool, PurgeError> {
    #[cfg(unix)]
    let tree_present = match quarantine_root(home)? {
        Some(root) => match tree::inspect(&root, &q.name(), tree::Expect::AnyDir) {
            Ok(present) => present,
            Err(tree::TreeError::Refused(_)) => true,
            Err(tree::TreeError::Io) => return Err(unavailable(())),
        },
        None => false,
    };
    #[cfg(not(unix))]
    let tree_present = false;
    let stores_empty = stores::label_free(home, &quarantine_key(q.employee, q.incarnation)).map_err(|_| Refusal::StoresUnavailable)?;
    Ok(tree_present || !stores_empty)
}

/// Finish every interrupted purge on `label` (`purging`, or `purged` with
/// its tree or row stores still present). Run before every purge step and
/// at boot; a row without its journal alerts and refuses (operator only).
pub(crate) fn recover_purges(home: &Path, label: &str, now: i64) -> Result<Vec<Purged>, PurgeError> {
    let rows = { let db = open(home)?; restore::rows_for_label(&db, label)? };
    let mut done = Vec::new();
    for (employee, incarnation, seal) in rows {
        if !matches!(seal.as_str(), "purging" | "purged") { continue; }
        let (q, journal) = {
            let db = open(home)?;
            (restore::read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)?, read_journal(&db, employee, incarnation)?)
        };
        if seal == "purged" && !leftovers(home, &q)? { continue; }
        let Some(mut journal) = journal else {
            restore::audit(home, "isolation.quarantine_purge_inconsistent", label, Severity::Critical,
                json!({"employee": hex::encode(employee.0), "incarnation": incarnation, "seal": seal, "reason": "journal_missing"}));
            return Err(Refusal::Inconsistent.into());
        };
        if seal == "purged" { journal.prior_seal = "purged".into(); }
        restore::audit(home, "isolation.quarantine_purge_recovered", label, Severity::Warning,
            json!({"employee": hex::encode(employee.0), "incarnation": incarnation, "seal": seal, "request_id": journal.request_id}));
        done.push(finish(home, &q, &journal, now, true)?);
    }
    Ok(done)
}

/// Boot: finish every `purging` row (protected mode, blocking pool). Returns
/// the labels it finished and the ones that refused (retried on the next
/// purge step for the label).
pub(crate) fn recover_purges_on_boot(home: &Path, now: i64) -> Result<(Vec<String>, Vec<(String, &'static str)>), PurgeError> {
    if matches!(std::fs::symlink_metadata(home.join(lifecycle::APPLY_DB)), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
        return Ok((Vec::new(), Vec::new()));
    }
    let labels: Vec<String> = {
        let db = open(home)?;
        let mut statement = db.prepare("SELECT DISTINCT label FROM employee_quarantine WHERE seal='purging' ORDER BY label LIMIT 4096")
            .map_err(unavailable)?;
        let rows = statement.query_map([], |r| r.get(0)).map_err(unavailable)?.collect::<Result<_, _>>().map_err(unavailable)?;
        rows
    };
    let (mut finished, mut failed) = (Vec::new(), Vec::new());
    for label in labels {
        match recover_purges(home, &label, now) {
            Ok(_) => finished.push(label),
            Err(error) => failed.push((label, error.code())),
        }
    }
    Ok((finished, failed))
}

#[cfg(test)]
pub(crate) fn journal_of(home: &Path, employee: EmployeeId, incarnation: u32) -> Option<Journal> {
    read_journal(&open(home).ok()?, employee, incarnation).ok().flatten()
}
