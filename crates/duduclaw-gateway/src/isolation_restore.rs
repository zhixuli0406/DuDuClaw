//! Same-id restore engine for tombstoned protected employees (native Linux
//! isolation v1.81, F6 design r5 §3.4–§3.7, task F6c).
//!
//! This module owns:
//! - the request journal `employee_restores` in apply.db (§3.4; one open
//!   request per quarantine row by a partial unique index, L1 caps);
//! - the **common checks** run at request, in the approval call (tx1) and in
//!   the apply transaction (tx2) (`isolation_restore_checks.rs`): quarantine
//!   row `sealed` and inside 90 days, `root_state = allocated`, tombstone
//!   finished on the users.db side (retried once, H1a), fences empty and no
//!   active route detail naming the employee (retried first, H2), no marker /
//!   open proposal / `agents/<label>`, tenant == pin == users.db, agent cap
//!   (L2), Personal owner checks (owner is the receipt principal, factor
//!   proof, `owner_eligible`, Q3 credential snapshot with the
//!   [`duduclaw_auth::credential_changes`] log);
//! - the R1 approval hook [`ApprovalRule`] (the rule itself is F6c2) and
//!   [`restore_precheck`] with the R5/S4/S5 result classes ([`RootClass`])
//!   (`isolation_restore_root.rs`);
//! - apply (§3.5) and crash recovery (§3.6) (`isolation_restore_apply.rs`).
//!
//! Lock order is always apply.db (`BEGIN IMMEDIATE`) before users.db (M6).
//! Every gate fails closed: an unreadable store, journal or root answer
//! refuses; a root transport error is `ROOT_UNAVAILABLE`, never `ROOT_GONE`.
//! RPCs (F6d), Shared request filing (F6k) and adoption (F6g) are not here;
//! the apply and recovery paths already handle the Shared kind.
//!
//! Everything here is blocking (SQLite, filesystem, root IPC).
use std::path::Path;

use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation::{self, Receipt};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalValue};
use duduclaw_security::audit::Severity;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::fs_safe::sqlite::Access;
use crate::isolation_lifecycle::{self as lifecycle, EmployeeKind, LifecycleError, APPLY_DB, CONFIRMATIONS_DB, USERS_DB};
use crate::isolation_quarantine as quarantine;

#[path = "isolation_restore_root.rs"]
mod root;
pub(crate) use root::*;
#[path = "isolation_restore_checks.rs"]
mod checks;
pub(crate) use checks::*;
#[path = "isolation_restore_apply.rs"]
mod apply;
pub(crate) use apply::{approve, recover_label, HookPoint, Recovered, Restored};
#[cfg(test)]
pub(crate) use apply::RESTORE_HOOK;

/// Q5: restore is possible for 90 days after the tombstone.
pub(crate) const RESTORE_WINDOW_SECS: i64 = 90 * 86_400;
/// §3.4: a request lives at most 72 hours (and never past the window).
pub(crate) const REQUEST_TTL_SECS: i64 = 72 * 3_600;
/// L1: requests per quarantine row in any 30 days (cancelled, denied and
/// expired ones count).
pub(crate) const MAX_REQUESTS_PER_ROW: i64 = 3;
const RATE_WINDOW_SECS: i64 = 30 * 86_400;
const REQUEST_DOMAIN: &[u8] = b"duduclaw-restore-request-v1\0";
const ROUTE_DOMAIN: &[u8] = b"duduclaw-restore-routes-v1\0";

const RESTORE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS employee_restores (
        request_id TEXT PRIMARY KEY CHECK (length(request_id) = 32 AND request_id NOT GLOB '*[^0-9a-f]*'),
        kind TEXT NOT NULL CHECK (kind IN ('personal_restore', 'shared_restore', 'personal_adopt', 'shared_adopt', 'early_purge')),
        employee_hex TEXT NOT NULL CHECK (length(employee_hex) = 32 AND employee_hex NOT GLOB '*[^0-9a-f]*'),
        incarnation INTEGER NOT NULL CHECK (incarnation >= 0 AND incarnation <= 4294967295),
        label TEXT NOT NULL,
        target_label TEXT,
        owner_user_id TEXT,
        tenant_id TEXT NOT NULL,
        requester_user_id TEXT NOT NULL,
        digest TEXT NOT NULL,
        route_digest TEXT NOT NULL,
        member_selection TEXT,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('requested', 'applying', 'done', 'failed', 'cancelled', 'denied', 'expired')),
        approver_user_id TEXT,
        self_approved INTEGER CHECK (self_approved IS NULL OR self_approved IN (0, 1)),
        proposal_id TEXT,
        decided_at INTEGER,
        failure TEXT);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_employee_restores_open ON employee_restores(employee_hex, incarnation)
        WHERE state IN ('requested', 'applying');
    CREATE INDEX IF NOT EXISTS idx_employee_restores_label ON employee_restores(label);";

/// Why a restore step refused. [`Refusal::code`] is the stable code F6d maps
/// to zh-TW text; none of them is a hint that a weaker check may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A store, the journal or a collaborator could not be read or written.
    Unavailable,
    NotFound,
    NotOwner,
    FactorRequired,
    OwnerIneligible,
    /// Quarantine not `sealed` (pending, oversize, invalid, absent, …), Q6.
    NotSealed,
    /// Past the 90-day window or the request's own expiry.
    Expired,
    TombstoneIncomplete,
    FencePending,
    /// A marker row or `agents/<label>` exists.
    LabelTaken,
    /// An open Employee proposal names the label.
    LabelPending,
    TenantMismatch,
    QuotaExceeded,
    CredentialChanged,
    RoutesChanged,
    StoresChanged,
    /// Store marks unrecorded or unreadable (F6h registry), never "unchanged".
    StoresUnavailable,
    OrgChanged,
    Integrity,
    RestoreInProgress,
    RetireInProgress,
    RateLimited,
    /// A request kind this engine does not file (Shared: F6k, adoption: F6g).
    NotSupported,
    Root(RootClass),
    /// The approval rule refused (code from F6c2, e.g. `APPROVER_NOT_QUALIFIED`).
    Approval(&'static str),
    /// Records disagree in a way only an operator may resolve (alerted).
    Inconsistent,
}

impl Refusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "RESTORE_UNAVAILABLE",
            Self::NotFound => "NOT_FOUND",
            Self::NotOwner => "NOT_OWNER",
            Self::FactorRequired => "FACTOR_REQUIRED",
            Self::OwnerIneligible => "OWNER_INELIGIBLE",
            Self::NotSealed => "NOT_SEALED",
            Self::Expired => "RESTORE_EXPIRED",
            Self::TombstoneIncomplete => "TOMBSTONE_INCOMPLETE",
            Self::FencePending => "FENCE_PENDING",
            Self::LabelTaken => "LABEL_TAKEN",
            Self::LabelPending => "LABEL_PENDING",
            Self::TenantMismatch => "TENANT_MISMATCH",
            Self::QuotaExceeded => "QUOTA_EXCEEDED",
            Self::CredentialChanged => "CREDENTIAL_CHANGED",
            Self::RoutesChanged => "ROUTES_CHANGED",
            Self::StoresChanged => "STORES_CHANGED",
            Self::StoresUnavailable => "STORES_UNAVAILABLE",
            Self::OrgChanged => "ORG_CHANGED",
            Self::Integrity => "INTEGRITY",
            Self::RestoreInProgress => "RESTORE_IN_PROGRESS",
            Self::RetireInProgress => "RETIRE_IN_PROGRESS",
            Self::RateLimited => "RATE_LIMITED",
            Self::NotSupported => "NOT_SUPPORTED",
            Self::Root(class) => class.code(),
            Self::Approval(code) => code,
            Self::Inconsistent => "RESTORE_INCONSISTENT",
        }
    }
}

fn unavailable<E>(_: E) -> Refusal { Refusal::Unavailable }

fn from_lifecycle(error: LifecycleError) -> Refusal {
    match error {
        LifecycleError::Unavailable => Refusal::Unavailable,
        LifecycleError::Stale => Refusal::Inconsistent,
        LifecycleError::Denied | LifecycleError::Tombstoned => Refusal::NotFound,
    }
}

// ── Journal ─────────────────────────────────────────────────────────────

pub(crate) fn ensure_restore_schema(db: &Connection) -> Result<(), Refusal> {
    quarantine::ensure_employee_schema(db).map_err(unavailable)?;
    db.execute_batch(RESTORE_SCHEMA).map_err(unavailable)
}

pub(crate) fn open_apply(home: &Path) -> Result<Connection, Refusal> {
    let db = lifecycle::open_store(home, APPLY_DB, Access::ReadWrite).map_err(from_lifecycle)?;
    ensure_restore_schema(&db)?;
    Ok(db)
}

fn parse_employee(value: &str) -> Result<EmployeeId, Refusal> {
    let bytes = hex::decode(value).map_err(unavailable)?;
    Ok(EmployeeId(<[u8; 16]>::try_from(bytes.as_slice()).map_err(unavailable)?))
}

/// One quarantine row with the F6c columns.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Quarantined {
    pub(crate) employee: EmployeeId,
    pub(crate) incarnation: u32,
    pub(crate) label: String,
    pub(crate) kind: EmployeeKind,
    pub(crate) owner: Option<String>,
    pub(crate) tenant: String,
    pub(crate) quarantined_at: i64,
    pub(crate) root: Option<(u64, u64)>,
    pub(crate) digest: Option<String>,
    pub(crate) stat_manifest: Option<Value>,
    pub(crate) seal: String,
    pub(crate) principal_snapshot: Value,
    pub(crate) pending_fences: Vec<String>,
    pub(crate) store_marks: Option<Value>,
    pub(crate) closed_by_request: Option<String>,
}

impl Quarantined {
    pub(crate) fn name(&self) -> String { quarantine::quarantine_name(&self.label, self.employee, self.incarnation) }
    pub(crate) fn binding(&self) -> Result<EmployeeBinding, Refusal> {
        use duduclaw_isolation::bounded::Text;
        Ok(match (&self.kind, &self.owner) {
            (EmployeeKind::Personal, Some(owner)) => EmployeeBinding::Personal(Text::new(owner.clone()).map_err(unavailable)?),
            (EmployeeKind::Shared, None) => EmployeeBinding::Shared(Text::new(self.tenant.clone()).map_err(unavailable)?),
            _ => return Err(Refusal::Unavailable),
        })
    }
}

pub(crate) fn read_row(db: &Connection, employee: EmployeeId, incarnation: u32) -> Result<Option<Quarantined>, Refusal> {
    let Some(row) = quarantine::read_quarantine(db, employee, incarnation).map_err(unavailable)? else { return Ok(None) };
    let (at, marks, closed): (i64, Option<String>, Option<String>) = db.query_row(
        "SELECT quarantined_at,store_marks,closed_by_request FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(employee.0), incarnation], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(unavailable)?;
    let kind = match row.kind.as_str() {
        "personal" => EmployeeKind::Personal,
        "shared" => EmployeeKind::Shared,
        _ => return Err(Refusal::Unavailable),
    };
    Ok(Some(Quarantined {
        employee, incarnation, label: row.label, kind, owner: row.owner, tenant: row.tenant, quarantined_at: at,
        root: row.root, digest: row.digest, stat_manifest: row.stat_manifest, seal: row.seal,
        principal_snapshot: row.principal_snapshot, pending_fences: row.pending_fences,
        store_marks: marks.map(|m| serde_json::from_str(&m)).transpose().map_err(unavailable)?, closed_by_request: closed,
    }))
}

/// Every quarantine row of `label`: `(employee, incarnation, seal)`.
pub(crate) fn rows_for_label(db: &Connection, label: &str) -> Result<Vec<(EmployeeId, u32, String)>, Refusal> {
    let mut statement = db.prepare("SELECT employee_hex,incarnation,seal FROM employee_quarantine WHERE label=?1 ORDER BY quarantined_at")
        .map_err(unavailable)?;
    let rows = statement.query_map([label], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))
        .map_err(unavailable)?.collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    rows.into_iter().map(|(employee, incarnation, seal)|
        Ok((parse_employee(&employee)?, u32::try_from(incarnation).map_err(unavailable)?, seal))).collect()
}

/// S7: a row is closed when `purged`, `adopted` or `restored`. Every row of
/// the label closed is the quarantine half of the label-free and retire
/// conditions (R6/R7, R11); F6h/F6l add the store and root halves.
pub(crate) fn all_rows_closed(home: &Path, label: &str) -> Result<bool, Refusal> {
    let db = open_apply(home)?;
    Ok(rows_for_label(&db, label)?.iter().all(|(_, _, seal)| matches!(seal.as_str(), "purged" | "adopted" | "restored")))
}

/// Mutual exclusion (§3.5) for F6d's tombstone/propose/purge/retire RPCs:
/// a quarantine row of the label is `restoring`/`adopting` or a request on
/// it is `applying`.
pub(crate) fn restore_in_progress(home: &Path, label: &str) -> Result<bool, Refusal> {
    let db = open_apply(home)?;
    let busy = rows_for_label(&db, label)?.iter().any(|(_, _, seal)| matches!(seal.as_str(), "restoring" | "adopting"));
    let applying: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM employee_restores WHERE label=?1 AND state='applying')",
        [label], |r| r.get(0)).map_err(unavailable)?;
    Ok(busy || applying)
}

/// One `employee_restores` row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RestoreRequest {
    pub(crate) request_id: String,
    pub(crate) kind: RequestKind,
    pub(crate) employee: EmployeeId,
    pub(crate) incarnation: u32,
    pub(crate) label: String,
    pub(crate) owner: Option<String>,
    pub(crate) tenant: String,
    pub(crate) requester: String,
    pub(crate) digest: String,
    pub(crate) route_digest: String,
    pub(crate) member_selection: Option<Value>,
    pub(crate) created_at: i64,
    pub(crate) expires_at: i64,
    pub(crate) state: String,
    pub(crate) approver: Option<String>,
    pub(crate) self_approved: Option<bool>,
    pub(crate) failure: Option<String>,
}

const REQUEST_COLUMNS: &str = "request_id,kind,employee_hex,incarnation,label,owner_user_id,tenant_id,requester_user_id,
    digest,route_digest,member_selection,created_at,expires_at,state,approver_user_id,self_approved,failure";

type RawRequest = (Vec<Option<String>>, [i64; 4]);

fn request_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRequest> {
    let texts = [0usize, 1, 2, 4, 5, 6, 7, 8, 9, 10, 13, 14, 16].iter()
        .map(|index| r.get::<_, Option<String>>(*index)).collect::<rusqlite::Result<Vec<_>>>()?;
    let numbers: [i64; 4] = [r.get(3)?, r.get(11)?, r.get(12)?, r.get::<_, Option<i64>>(15)?.unwrap_or(-1)];
    Ok((texts, numbers))
}

fn decode_request((texts, numbers): RawRequest) -> Result<RestoreRequest, Refusal> {
    let text = |index: usize| texts.get(index).cloned().flatten().ok_or(Refusal::Unavailable);
    let optional = |index: usize| texts.get(index).cloned().flatten();
    Ok(RestoreRequest {
        request_id: text(0)?, kind: RequestKind::parse(&text(1)?).ok_or(Refusal::Unavailable)?,
        employee: parse_employee(&text(2)?)?, incarnation: u32::try_from(numbers[0]).map_err(unavailable)?,
        label: text(3)?, owner: optional(4), tenant: text(5)?, requester: text(6)?,
        digest: text(7)?, route_digest: text(8)?,
        member_selection: optional(9).map(|m| serde_json::from_str(&m)).transpose().map_err(unavailable)?,
        created_at: numbers[1], expires_at: numbers[2], state: text(10)?, approver: optional(11),
        self_approved: match numbers[3] { -1 => None, 0 => Some(false), 1 => Some(true), _ => return Err(Refusal::Unavailable) },
        failure: optional(12),
    })
}

pub(crate) fn read_request(db: &Connection, request_id: &str) -> Result<Option<RestoreRequest>, Refusal> {
    let raw = db.query_row(&format!("SELECT {REQUEST_COLUMNS} FROM employee_restores WHERE request_id=?1"),
        [request_id], request_from_row).optional().map_err(unavailable)?;
    raw.map(decode_request).transpose()
}

/// The open (`requested`/`applying`) request of a quarantine row.
pub(crate) fn open_request(db: &Connection, employee: EmployeeId, incarnation: u32) -> Result<Option<RestoreRequest>, Refusal> {
    let raw = db.query_row(&format!("SELECT {REQUEST_COLUMNS} FROM employee_restores
        WHERE employee_hex=?1 AND incarnation=?2 AND state IN ('requested','applying')"),
        params![hex::encode(employee.0), incarnation], request_from_row).optional().map_err(unavailable)?;
    raw.map(decode_request).transpose()
}

/// Move a request from one of `from` to `to` (`failure` kept when `None`).
pub(crate) fn set_request_state(db: &Connection, request_id: &str, from: &[&str], to: &str, failure: Option<&str>, now: i64)
    -> Result<(), Refusal> {
    let current: String = db.query_row("SELECT state FROM employee_restores WHERE request_id=?1", [request_id], |r| r.get(0))
        .optional().map_err(unavailable)?.ok_or(Refusal::NotFound)?;
    if !from.contains(&current.as_str()) { return Err(Refusal::NotFound); }
    db.execute("UPDATE employee_restores SET state=?2,failure=COALESCE(?3,failure),decided_at=?4 WHERE request_id=?1",
        params![request_id, to, failure, now]).map_err(unavailable)?;
    Ok(())
}

/// Move a quarantine row from one of `from` to `to` (§3.4 state machine).
pub(crate) fn set_seal(db: &Connection, employee: EmployeeId, incarnation: u32, from: &[&str], to: &str) -> Result<(), Refusal> {
    let current: String = db.query_row("SELECT seal FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(employee.0), incarnation], |r| r.get(0)).optional().map_err(unavailable)?.ok_or(Refusal::NotFound)?;
    if !from.contains(&current.as_str()) { return Err(Refusal::Inconsistent); }
    db.execute("UPDATE employee_quarantine SET seal=?3 WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(employee.0), incarnation, to]).map_err(unavailable)?;
    Ok(())
}

/// Every transition goes through `audit_and_emit` (§3.8, M7).
pub(crate) fn audit(home: &Path, event: &str, label: &str, severity: Severity, details: Value) {
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(event, label, severity, details));
}

pub(crate) fn audit_details(request: &RestoreRequest, extra: Value) -> Value {
    let mut details = json!({
        "request_id": request.request_id, "kind": request.kind.as_str(), "label": request.label,
        "employee": hex::encode(request.employee.0), "incarnation": request.incarnation,
        "digest_prefix": duduclaw_core::truncate_chars(&request.digest, 16), "requester_user_id": request.requester,
        "approver_user_id": request.approver, "self_approved": request.self_approved,
    });
    if let (Some(target), Value::Object(more)) = (details.as_object_mut(), extra) { target.extend(more); }
    details
}

// ── Digests ─────────────────────────────────────────────────────────────

fn put(hash: &mut Sha256, tag: &str, bytes: &[u8]) {
    hash.update((tag.len() as u64).to_be_bytes());
    hash.update(tag.as_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// Q4: digest of the full route list the employee would use again.
pub(crate) fn route_digest(routes: &[(String, u64)]) -> String {
    let mut hash = Sha256::new();
    hash.update(ROUTE_DOMAIN);
    put(&mut hash, "count", &(routes.len() as u64).to_be_bytes());
    for (account, generation) in routes {
        put(&mut hash, "account", account.as_bytes());
        put(&mut hash, "generation", &generation.to_be_bytes());
    }
    hex::encode(hash.finalize())
}

/// §3.4 request digest: request id, kind, employee, incarnation, quarantine
/// digest, tenant, owner, member selection and the route digest.
pub(crate) fn request_digest(request: &RestoreRequest, quarantine_digest: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(REQUEST_DOMAIN);
    put(&mut hash, "request", request.request_id.as_bytes());
    put(&mut hash, "kind", request.kind.as_str().as_bytes());
    put(&mut hash, "employee", &request.employee.0);
    put(&mut hash, "incarnation", &request.incarnation.to_be_bytes());
    put(&mut hash, "quarantine", quarantine_digest.as_bytes());
    put(&mut hash, "tenant", request.tenant.as_bytes());
    put(&mut hash, "owner", request.owner.as_deref().unwrap_or("").as_bytes());
    put(&mut hash, "members", request.member_selection.as_ref().map(Value::to_string).unwrap_or_default().as_bytes());
    put(&mut hash, "routes", request.route_digest.as_bytes());
    hex::encode(hash.finalize())
}

// ── Requests ────────────────────────────────────────────────────────────

/// The owner's factor proof, verified by the RPC (Q2, F6d).
#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnerProof<'a> {
    pub(crate) user_id: &'a str,
    /// `passkey`, `totp`, `email` or `google_reauth`.
    pub(crate) factor: &'a str,
}

/// A filed request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Filed {
    pub(crate) request: RestoreRequest,
}

/// `me.isolation.request_restore` engine (§3.4): the owner files for one of
/// their tombstoned Personal employees. No request field names an owner.
pub(crate) fn file_personal_request(home: &Path, env: &RestoreEnv<'_>, employee: EmployeeId, incarnation: u32,
    proof: Option<OwnerProof<'_>>) -> Result<Filed, Refusal> {
    let peek = { let db = open_apply(home)?; read_row(&db, employee, incarnation)?.ok_or(Refusal::NotFound)? };
    let proof = proof.ok_or(Refusal::FactorRequired)?;
    // Not-owned rows answer NOT_FOUND (no existence oracle).
    if peek.kind != EmployeeKind::Personal || peek.owner.as_deref() != Some(proof.user_id) { return Err(Refusal::NotFound); }
    if !matches!(proof.factor, "passkey" | "totp" | "email" | "google_reauth") { return Err(Refusal::FactorRequired); }
    recover_label(home, env, &peek.label)?;
    let q = load_fenced(home, env, employee, incarnation)?;
    let routes = check_outside(home, env, &q)?;
    let mut db = open_apply(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    check_apply_db(&tx, &q, Stage::Request, env.now)?;
    let recent: i64 = tx.query_row("SELECT count(*) FROM employee_restores WHERE employee_hex=?1 AND incarnation=?2 AND created_at>?3",
        params![hex::encode(employee.0), incarnation, env.now - RATE_WINDOW_SECS], |r| r.get(0)).map_err(unavailable)?;
    if recent >= MAX_REQUESTS_PER_ROW { return Err(Refusal::RateLimited); }
    let unsigned = RestoreRequest {
        request_id: hex::encode(uuid::Uuid::new_v4().as_bytes()), kind: RequestKind::PersonalRestore, employee, incarnation,
        label: q.label.clone(), owner: q.owner.clone(), tenant: q.tenant.clone(), requester: proof.user_id.to_owned(),
        digest: String::new(), route_digest: route_digest(&routes), member_selection: None, created_at: env.now,
        expires_at: env.now.saturating_add(REQUEST_TTL_SECS).min(q.quarantined_at.saturating_add(RESTORE_WINDOW_SECS)),
        state: "requested".into(), approver: None, self_approved: None, failure: None,
    };
    let digest = request_digest(&unsigned, q.digest.as_deref().ok_or(Refusal::NotSealed)?);
    let request = RestoreRequest { digest, ..unsigned };
    tx.execute("INSERT INTO employee_restores(request_id,kind,employee_hex,incarnation,label,owner_user_id,tenant_id,
            requester_user_id,digest,route_digest,created_at,expires_at,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'requested')",
        params![request.request_id, request.kind.as_str(), hex::encode(employee.0), incarnation, request.label, request.owner,
            request.tenant, request.requester, request.digest, request.route_digest, request.created_at, request.expires_at])
        .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    audit(home, "isolation.employee_restore_requested", &request.label, Severity::Info,
        audit_details(&request, json!({"factor": proof.factor, "routes": routes.len()})));
    Ok(Filed { request })
}

/// How a request is closed without applying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Close { Cancelled, Denied }

/// Cancel (requester or Admin) or deny (Admin) an open, not yet applying
/// request; the caller authorized the actor.
pub(crate) fn close_request(home: &Path, request_id: &str, actor: &str, close: Close, now: i64) -> Result<(), Refusal> {
    let mut db = open_apply(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    let request = read_request(&tx, request_id)?.ok_or(Refusal::NotFound)?;
    let (state, event) = match close {
        Close::Cancelled => ("cancelled", "isolation.employee_restore_cancelled"),
        Close::Denied => ("denied", "isolation.employee_restore_denied"),
    };
    set_request_state(&tx, request_id, &["requested"], state, None, now)?;
    if close == Close::Denied {
        tx.execute("UPDATE employee_restores SET approver_user_id=?2 WHERE request_id=?1", params![request_id, actor])
            .map_err(unavailable)?;
    }
    tx.commit().map_err(unavailable)?;
    let closed = RestoreRequest { state: state.into(), ..request };
    audit(home, event, &closed.label, Severity::Info, audit_details(&closed, json!({"actor_user_id": actor})));
    Ok(())
}

/// Expire every `requested` request past its `expires_at`; returns how many.
pub(crate) fn expire_requests(home: &Path, now: i64) -> Result<usize, Refusal> {
    let mut db = open_apply(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    let ids: Vec<String> = {
        let mut statement = tx.prepare("SELECT request_id FROM employee_restores WHERE state='requested' AND expires_at<?1")
            .map_err(unavailable)?;
        let ids = statement.query_map([now], |r| r.get(0)).map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
        ids
    };
    let expired = ids.iter().map(|id| {
        set_request_state(&tx, id, &["requested"], "expired", None, now)?;
        read_request(&tx, id)?.ok_or(Refusal::Unavailable)
    }).collect::<Result<Vec<_>, Refusal>>()?;
    tx.commit().map_err(unavailable)?;
    for request in &expired {
        audit(home, "isolation.employee_restore_expired", &request.label, Severity::Info, audit_details(request, json!({})));
    }
    Ok(expired.len())
}
