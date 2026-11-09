//! Root retire of a tombstoned, purged protected employee (native Linux
//! isolation v1.81, F6 design r5 §3.12, N2, task F6l).
//!
//! Retire frees the root allocation of one employee id: spawnd's journal
//! entry (and the label it holds), the loop image with its quota, and the
//! account's run eligibility. The UID is never recycled (§7 N4).
//!
//! - [`employee_retire_propose`] (Admin + step-up, `isolation.employee_retire_propose`
//!   in F6d) runs the R11 checks, a schema-2 root ReadBack of the stored
//!   `root_item`, and in ONE apply.db `BEGIN IMMEDIATE` transaction re-runs
//!   the checks, stages the `EmployeeRetire` proposal and sets
//!   `root_allocations.root_state = retiring`. An authoritative `NotFound`
//!   records `root_absent` without a proposal (operator alert).
//! - [`purge_hook`]: what F6i's purge calls right after its commit when the
//!   同時釋放主機資源 box is ticked.
//! - [`RetireBackend`]: the approval/apply side behind the existing proposal
//!   card (`handlers::migration_authority::retire::EmployeeRetireAuthority`
//!   adapts it to `MigrationAuthorityBackend`): approval and every apply step
//!   re-run R11 (apply.db before users.db, M6) and, for an early-purged row,
//!   the second-Admin rule ([`ApprovalRule`], F6c2) with requester = proposer
//!   (S12). Compensation is forward only (R14).
//! - [`root_precheck`] / [`classify_readback`]: the R5/S4/S5 root classes
//!   (`ROOT_RETIRED`, `ROOT_INCONSISTENT`, …) for F6c/F6g.
//! - [`label_free`]: the label-free decision of §3.12 "After retire" (R7),
//!   with the S10 positive zero count delegated to F6h ([`LabelStores`]).
//!
//! Everything here is blocking (SQLite, root IPC) and runs on the blocking pool.
use std::path::Path;

use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation::{self, Action, Answer, ErrorCode as Root, Phase, Receipt, Request, RetireSpec};
use duduclaw_migration::confirmation::{ConfirmationError, ConfirmationStore, ProposalValidator};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalBinding, ProposalOperationId};
use duduclaw_migration::ids::RequestId;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::fs_safe::sqlite::Access;

const AUTHORITY_DIR: &str = "migration-authority";
const APPLY_DB: &str = "apply.db";
const CONFIRMATIONS_DB: &str = "migration-confirmations.db";
/// S15: the gateway alerts the operator once a retire waited this long for an idle host.
pub(crate) const HOST_BUSY_ALERT_SECS: i64 = 24 * 60 * 60;
/// `quarantine_digest_prefix` for a purged row that never had a digest
/// (`absent`/`oversize`/`invalid` seal): sixteen zeros, never a real prefix.
const NO_DIGEST_PREFIX: &str = "0000000000000000";
/// Additive table (ignored by older builds, so no employee schema bump):
/// when a retire first found the host busy, and whether the 24 h alert fired.
const WAITS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS employee_retire_waits (
    employee_hex TEXT PRIMARY KEY CHECK (length(employee_hex) = 32 AND employee_hex NOT GLOB '*[^0-9a-f]*'),
    busy_since INTEGER NOT NULL, alerted INTEGER NOT NULL DEFAULT 0 CHECK (alerted IN (0, 1)));";

/// Root classes of §3.4 (R5, S4, S5); the lifecycle codes the RPCs report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootClass { Ok, Unavailable, Gone, Inconsistent, Retired }
impl RootClass {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unavailable => "ROOT_UNAVAILABLE",
            Self::Gone => "ROOT_GONE",
            Self::Inconsistent => "ROOT_INCONSISTENT",
            Self::Retired => "ROOT_RETIRED",
        }
    }
}

/// Why a retire step was refused. Every variant is a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetireError {
    /// A store could not be read or written (or holds a row this build cannot check).
    Unavailable,
    /// No root allocation or quarantine row for the employee.
    NotFound,
    /// A row of the id is not closed, or the newest row is not purged.
    RowsOpen,
    OpenRequest,
    RestoreInProgress,
    /// The old id holds a live confirmed row (a restored employee).
    LiveOldId,
    InProgress,
    Retired,
    RootAbsent,
    RootUnavailable,
    RootInconsistent,
    /// spawnd answered `NotReady`: a run is live; retried with backoff (S15).
    HostBusy,
    /// Compensation after the root started (R14).
    ForwardOnly,
    Approval(ApprovalRefusal),
    /// The approver (live session or approval record) is unknown.
    NoApprover,
    Stale,
    Invalid,
}
impl RetireError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "STORE_UNAVAILABLE",
            Self::NotFound => "EMPLOYEE_NOT_FOUND",
            Self::RowsOpen => "RETIRE_ROWS_OPEN",
            Self::OpenRequest => "RETIRE_REQUEST_OPEN",
            Self::RestoreInProgress => "RESTORE_IN_PROGRESS",
            Self::LiveOldId => "RETIRE_LIVE_EMPLOYEE",
            Self::InProgress => "RETIRE_IN_PROGRESS",
            Self::Retired => RootClass::Retired.code(),
            Self::RootAbsent => "ROOT_ABSENT",
            Self::RootUnavailable => RootClass::Unavailable.code(),
            Self::RootInconsistent => RootClass::Inconsistent.code(),
            Self::HostBusy => "RETIRE_HOST_BUSY",
            Self::ForwardOnly => "RETIRE_FORWARD_ONLY",
            Self::Approval(refusal) => refusal.code(),
            Self::NoApprover | Self::Stale => "STALE",
            Self::Invalid => "RETIRE_INVALID",
        }
    }
    /// zh-TW wording of design §4 (F6e shows it).
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "員工資料目前無法讀取，已拒絕操作。",
            Self::NotFound => "找不到這位員工，請重新載入。",
            Self::RowsOpen => "這位員工的隔離區資料尚未清除，請先清除隔離區。",
            Self::OpenRequest => "有還原申請進行中",
            Self::RestoreInProgress => "另一個還原正在進行",
            Self::LiveOldId => "這位員工已還原，請先註銷後再釋放主機資源。",
            Self::InProgress => "主機資源釋放中",
            Self::Retired => "主機資源已釋放，無法還原",
            Self::RootAbsent => "主機資源狀態不明",
            Self::RootUnavailable => "主機暫時無法連線，請稍後再試",
            Self::RootInconsistent => "主機資源與紀錄不一致，請聯絡維運",
            Self::HostBusy => "主機正在執行其他工作，稍後會自動重試",
            Self::ForwardOnly => "主機資源已開始釋放，無法取消；系統會繼續完成釋放。",
            Self::Approval(refusal) => refusal.message(),
            Self::NoApprover | Self::Stale => "申請內容已變更，請重新載入後再試。",
            Self::Invalid => "釋放主機資源的提案內容不正確。",
        }
    }
}

/// F6c2's shared second-Admin rule (`duduclaw-auth` `admin_approval_rule`,
/// R1/R2/S1/S2/S16), taken as a seam. It must read the eligibility log and
/// the Admin count in one users.db read, which the caller runs while holding
/// its apply.db transaction (lock order apply.db → users.db, M6).
pub(crate) trait ApprovalRule: Send + Sync {
    /// `owner`: the Personal owner of the purged row (`approver != owner` too).
    fn decide(&self, home: &Path, requester: &str, approver: &str, owner: Option<&str>) -> Result<Approval, ApprovalRefusal>;
    /// The binding decision of the stage step (S12, F6fa), for **every**
    /// approval of an early-purged row: decided in a users.db write
    /// transaction the caller opens while holding its apply.db IMMEDIATE
    /// transaction (M6), so the Admin count is read with the apply, and — for
    /// a sole-Admin self-approval — F6c2's users.db audit row for `subject`
    /// (the proposal id) written there, at most once per proposal. `Err`
    /// refuses the root retire.
    fn confirm_at_apply(&self, home: &Path, requester: &str, approver: &str, owner: Option<&str>, subject: &str)
        -> Result<Approval, ApprovalRefusal>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Approval {
    /// Another qualifying Admin approved.
    OtherAdmin,
    /// Sole qualifying Admin, fresh step-up, outside the cooling window:
    /// audited `self_approved`.
    SelfApproved,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalRefusal {
    /// Another qualifying Admin exists: 需由另一位管理員核准.
    NeedsOtherAdmin,
    /// `APPROVER_NOT_QUALIFIED` (S2).
    NotQualified,
    /// R2/S1 cooling: an Admin lost eligibility in the last 7 days.
    Cooling,
    /// `ADMIN_LOG_UNAVAILABLE`: unreadable log or under 7 days of coverage.
    LogUnavailable,
    /// `STEP_UP_REQUIRED`: this call carries no fresh step-up proof (F6fa:
    /// the rule receives the real freshness, never an assumed one).
    StepUpRequired,
}
impl ApprovalRefusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::NeedsOtherAdmin => "APPROVER_IS_REQUESTER",
            Self::NotQualified => "APPROVER_NOT_QUALIFIED",
            Self::Cooling => "SELF_APPROVAL_COOLING",
            Self::LogUnavailable => "ADMIN_LOG_UNAVAILABLE",
            Self::StepUpRequired => "STEP_UP_REQUIRED",
        }
    }
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::NeedsOtherAdmin => "需由另一位管理員核准",
            Self::NotQualified => "擔任管理員未滿 7 天，或由申請人設為管理員，無法核准",
            Self::Cooling => "7 天內有管理員失去權限，暫時不能自行核准",
            Self::LogUnavailable => "管理員權限紀錄無法讀取，暫時不能自行核准",
            Self::StepUpRequired => "請先以密碼與雙因素驗證確認身分。",
        }
    }
}

/// spawnd allocation socket, schema 2. `Err(())` = transport failure or no
/// (valid) reply: `ROOT_UNAVAILABLE`, never `NotFound` (S5d).
pub(crate) trait RetireRoot: Send + Sync {
    fn exchange(&self, request: &Request) -> Result<Answer, ()>;
}
/// The root `duduclaw-spawnd` allocation socket (Linux only).
pub(crate) struct SpawndRetireRoot;
impl RetireRoot for SpawndRetireRoot {
    fn exchange(&self, request: &Request) -> Result<Answer, ()> {
        #[cfg(target_os = "linux")]
        { allocation::transport::exchange(request).map_err(|_| ()) }
        #[cfg(not(target_os = "linux"))]
        { let _ = request; Err(()) }
    }
}

/// Notices of §3.12 ("notify every log-eligible Admin and the Personal
/// owner") and operator alerts (S15, `root_absent`, `ROOT_INCONSISTENT`).
/// Delivery (addresses from the F6c2 eligibility log, caps) is F6d's.
pub(crate) trait RetireNotifier: Send + Sync {
    fn notify(&self, home: &Path, event: &str, owner: Option<&str>, details: &Value);
}

/// F6h's `LABEL_KEYED_STORES` registry, seen from the label-free decision
/// (S10): a positive count of rows still under the label; an unreadable
/// store is `Err`, never zero.
pub(crate) trait LabelStores {
    fn count_under(&self, home: &Path, label: &str) -> Result<u64, ()>;
}

fn now_unix() -> i64 { chrono::Utc::now().timestamp() }

fn open_apply(home: &Path, access: Access) -> Result<Connection, RetireError> {
    let db = crate::fs_safe::sqlite::open_existing(&home.join(AUTHORITY_DIR), APPLY_DB, access)
        .map_err(|_| RetireError::Unavailable)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| RetireError::Unavailable)?;
    Ok(db)
}

fn writable_apply(home: &Path) -> Result<Connection, RetireError> {
    let db = open_apply(home, Access::ReadWrite)?;
    crate::isolation_quarantine::ensure_employee_schema(&db).map_err(|_| RetireError::Unavailable)?;
    db.execute_batch(WAITS_SCHEMA).map_err(|_| RetireError::Unavailable)?;
    Ok(db)
}

fn readable_apply(home: &Path) -> Result<Connection, RetireError> {
    let db = open_apply(home, Access::ReadOnly)?;
    crate::isolation_quarantine::check_readable(&db).map_err(|_| RetireError::Unavailable)?;
    Ok(db)
}

fn table_exists(db: &Connection, table: &str) -> Result<bool, RetireError> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [table], |r| r.get(0))
        .map_err(|_| RetireError::Unavailable)
}

/// The confirmation store, never created here (a retire needs the Applied
/// allocation item that lives in it).
fn confirmations(home: &Path) -> Result<ConfirmationStore, RetireError> {
    let path = home.join(CONFIRMATIONS_DB);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= 64 * 1024 * 1024 => {}
        _ => return Err(RetireError::Unavailable),
    }
    ConfirmationStore::open_existing(&path).map_err(|_| RetireError::Unavailable)
}

fn proposal_key(binding: &ProposalBinding) -> String { hex::encode(binding.operation_id.proposal_id.0) }

/// The `root_allocations` row of one employee id (S7, R12).
#[derive(Clone)]
pub(crate) struct RootRow {
    pub(crate) item: ConfirmationItem,
    pub(crate) receipt: Receipt,
    pub(crate) state: String,
    pub(crate) proposal: Option<String>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRootItem { item: ConfirmationItem, receipt: Receipt }

fn root_row(db: &Connection, employee: EmployeeId) -> Result<Option<RootRow>, RetireError> {
    let row: Option<(String, String, Option<String>)> = db.query_row(
        "SELECT root_item,root_state,retire_proposal_id FROM root_allocations WHERE employee_hex=?1",
        [hex::encode(employee.0)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(|_| RetireError::Unavailable)?;
    let Some((encoded, state, proposal)) = row else { return Ok(None) };
    let stored: StoredRootItem = serde_json::from_str(&encoded).map_err(|_| RetireError::Unavailable)?;
    // The stored item must be the confirmed allocation of this very receipt.
    let spec = allocation::specification(&stored.item).map_err(|_| RetireError::Unavailable)?;
    if stored.item.category != ConfirmationCategory::Employee || stored.receipt.employee != employee
        || stored.receipt.binding != stored.item.binding || stored.receipt.label != spec.label {
        return Err(RetireError::Unavailable);
    }
    Ok(Some(RootRow { item: stored.item, receipt: stored.receipt, state, proposal }))
}

/// One quarantine row as the retire checks read it.
struct QuarantineRow { label: String, seal: String, digest: Option<String>, early_purge: Option<String>, owner: Option<String> }

fn quarantine_rows(db: &Connection, employee: EmployeeId) -> Result<Vec<QuarantineRow>, RetireError> {
    let mut statement = db.prepare(
        "SELECT label,seal,digest,early_purge,owner_user_id FROM employee_quarantine WHERE employee_hex=?1 ORDER BY incarnation")
        .map_err(|_| RetireError::Unavailable)?;
    let rows = statement.query_map([hex::encode(employee.0)], |r| Ok(QuarantineRow {
        label: r.get(0)?, seal: r.get(1)?, digest: r.get(2)?, early_purge: r.get(3)?, owner: r.get(4)? }))
        .map_err(|_| RetireError::Unavailable)?.collect::<Result<Vec<_>, _>>().map_err(|_| RetireError::Unavailable)?;
    Ok(rows)
}

fn closed(seal: &str) -> bool { matches!(seal, "purged" | "adopted" | "restored") }

/// What the R11 checks established, for the proposal fields.
pub(crate) struct Checked {
    pub(crate) root: RootRow,
    pub(crate) label: String,
    pub(crate) early: bool,
    pub(crate) digest_prefix: String,
    pub(crate) owner: Option<String>,
    /// `root_state = retired` under this very proposal (an idempotent re-apply).
    pub(crate) retired: bool,
}
#[derive(Clone, Copy)]
enum Expect<'a> { Allocated, Proposal(&'a str) }

/// R11/S7/S12 (§3.12 "When and who"), on one consistent apply.db view.
fn check(db: &Connection, employee: EmployeeId, expect: Expect<'_>) -> Result<Checked, RetireError> {
    use RetireError::*;
    let root = root_row(db, employee)?.ok_or(NotFound)?;
    let retired = match (root.state.as_str(), expect) {
        ("allocated", Expect::Allocated) => false,
        ("retiring", Expect::Proposal(id)) if root.proposal.as_deref() == Some(id) => false,
        ("retired", Expect::Proposal(id)) if root.proposal.as_deref() == Some(id) => true,
        ("retiring", _) => return Err(InProgress),
        ("retired", _) => return Err(Retired),
        ("root_absent", _) => return Err(RootAbsent),
        _ => return Err(Stale),
    };
    let rows = quarantine_rows(db, employee)?;
    let newest = rows.last().ok_or(NotFound)?;
    if newest.seal != "purged" || rows.iter().any(|row| !closed(&row.seal)) { return Err(RowsOpen); }
    let label = newest.label.clone();
    if root.receipt.label != label { return Err(Unavailable); }
    let digest_prefix = match &newest.digest {
        None => NO_DIGEST_PREFIX.to_owned(),
        Some(digest) if digest.len() >= allocation::DIGEST_PREFIX_CHARS
            && digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            duduclaw_core::truncate_bytes(digest, allocation::DIGEST_PREFIX_CHARS).to_owned(),
        Some(_) => return Err(Unavailable),
    };
    if !retired {
        if table_exists(db, "employee_restores")? {
            let open: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM employee_restores WHERE (employee_hex=?1 OR label=?2) AND state IN ('requested','applying'))",
                params![hex::encode(employee.0), label], |r| r.get(0)).map_err(|_| Unavailable)?;
            if open { return Err(OpenRequest); }
        }
        let busy: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM employee_quarantine WHERE label=?1 AND seal IN ('restoring','adopting'))",
            [&label], |r| r.get(0)).map_err(|_| Unavailable)?;
        if busy { return Err(RestoreInProgress); }
        let current: Option<String> = db.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [&label], |r| r.get(0))
            .optional().map_err(|_| Unavailable)?;
        if let Some(encoded) = current {
            let current: Receipt = serde_json::from_str(&encoded).map_err(|_| Unavailable)?;
            if current.employee == employee && !current.disabled { return Err(LiveOldId); }
        }
    }
    Ok(Checked { early: newest.early_purge.is_some(), owner: newest.owner.clone(), root, label, digest_prefix, retired })
}

fn spec_for(checked: &Checked, employee: EmployeeId, requested_by: &str) -> Result<RetireSpec, RetireError> {
    allocation::validate_actor_id(requested_by).map_err(|_| RetireError::Invalid)?;
    Ok(RetireSpec { label: checked.label.clone(), target: checked.root.item.binding, employee, uid: checked.root.receipt.uid,
        early: checked.early, quarantine_digest_prefix: checked.digest_prefix.clone(), requested_by: requested_by.to_owned() })
}

fn nonce() -> RequestId { RequestId(*uuid::Uuid::new_v4().as_bytes()) }

fn readback_request(item: &ConfirmationItem) -> Request {
    Request { schema: allocation::SCHEMA_V2, nonce: nonce(), action: Action::ReadBack, item: item.clone(), approver: None }
}

/// R5/S4/S5 class of a schema-2 ReadBack of the stored allocation, with the
/// S17 `idle` of a Ready entry. A mismatch in UID, label, quota, binding or
/// principal (the root anchor, M3) is `Inconsistent`, never `Gone`.
pub(crate) fn classify_readback(expected: &Receipt, answer: Result<Answer, ()>) -> (RootClass, Option<bool>) {
    match answer {
        Err(()) => (RootClass::Unavailable, None),
        Ok(Answer::Refused(Root::NotFound)) => (RootClass::Gone, None),
        Ok(Answer::Refused(Root::NotReady | Root::IsolationUnavailable)) => (RootClass::Unavailable, None),
        Ok(Answer::Refused(Root::Inconsistent | Root::InvalidRequest)) => (RootClass::Inconsistent, None),
        Ok(Answer::Reply(reply)) => {
            let r = &reply.receipt;
            if r.employee != expected.employee || r.uid != expected.uid || r.label != expected.label || r.binding != expected.binding
                || r.bytes != expected.bytes || r.inodes != expected.inodes || r.principal_binding != expected.principal_binding {
                return (RootClass::Inconsistent, None);
            }
            match reply.phase {
                Phase::Ready => (RootClass::Ok, reply.idle),
                Phase::Retiring | Phase::Retired => (RootClass::Retired, None),
                Phase::Planned | Phase::Disabling | Phase::Disabled => (RootClass::Inconsistent, None),
            }
        }
    }
}

/// What F6c/F6g need from the root for one employee id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RootCheck { pub(crate) class: RootClass, pub(crate) idle: Option<bool> }

/// Root class of one employee id (§3.4 root precheck, §3.12 "After
/// retire"): `retiring`/`retired` answer `ROOT_RETIRED` and `root_absent`
/// `ROOT_GONE` without asking the root; `allocated` asks for a schema-2
/// ReadBack of the stored allocation.
pub(crate) fn root_precheck(home: &Path, employee: EmployeeId, root: &dyn RetireRoot) -> Result<RootCheck, RetireError> {
    let row = root_row(&readable_apply(home)?, employee)?.ok_or(RetireError::NotFound)?;
    let class = match row.state.as_str() {
        "retiring" | "retired" => return Ok(RootCheck { class: RootClass::Retired, idle: None }),
        "root_absent" => return Ok(RootCheck { class: RootClass::Gone, idle: None }),
        "allocated" => classify_readback(&row.receipt, root.exchange(&readback_request(&row.item))),
        _ => return Err(RetireError::Unavailable),
    };
    Ok(RootCheck { class: class.0, idle: class.1 })
}

fn audit(home: &Path, event: &str, label: &str, warning: bool, details: Value) {
    let severity = if warning { duduclaw_security::audit::Severity::Warning } else { duduclaw_security::audit::Severity::Info };
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(event, label, severity, details));
}

/// Collaborators of a propose call.
pub(crate) struct ProposeEnv<'a> {
    /// The verified Admin (step-up checked by the caller); the requester.
    pub(crate) actor: &'a str,
    pub(crate) root: &'a dyn RetireRoot,
    pub(crate) notifier: &'a dyn RetireNotifier,
    /// Unix seconds.
    pub(crate) now: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProposeOutcome {
    /// The `EmployeeRetire` proposal waits on the card; `root_state = retiring`.
    Staged { proposal: ProposalOperationId, early: bool },
    /// Authoritative `NotFound` (S5b): `root_absent`, no proposal, operator alerted.
    RootAbsent,
    /// The root already reports the entry Retired (S5c): reconciled.
    AlreadyRetired,
}

/// The retire item's own staging check (the R11 checks ran in the caller's
/// apply.db transaction; this one must not open apply.db).
struct StagingCheck<'a>(&'a RetireSpec);
impl ProposalValidator for StagingCheck<'_> {
    fn validate_fresh(&self, item: &ConfirmationItem) -> Result<(), ConfirmationError> {
        if allocation::retire_specification(item).map_err(|_| ConfirmationError::Invalid)? != *self.0 {
            return Err(ConfirmationError::Stale);
        }
        Ok(())
    }
}

/// `isolation.employee_retire_propose` (R11, R12): stage the retire of one
/// tombstoned, purged employee id. The caller verified Admin + step-up.
pub(crate) fn employee_retire_propose(home: &Path, employee: EmployeeId, env: &ProposeEnv<'_>)
    -> Result<ProposeOutcome, RetireError> {
    allocation::validate_actor_id(env.actor).map_err(|_| RetireError::Invalid)?;
    // Fail fast before asking the root (no lock held across the IPC).
    let checked = check(&readable_apply(home)?, employee, Expect::Allocated)?;
    let (class, _) = classify_readback(&checked.root.receipt, env.root.exchange(&readback_request(&checked.root.item)));
    let hex = hex::encode(employee.0);
    match class {
        RootClass::Ok => {}
        RootClass::Unavailable => return Err(RetireError::RootUnavailable),
        RootClass::Inconsistent => {
            // S4: stays allocated and blocks the label until operator recovery.
            audit(home, "isolation.employee_root_inconsistent", &checked.label, true,
                json!({"employee": hex, "actor_user_id": env.actor, "stage": "retire_propose"}));
            env.notifier.notify(home, "isolation.employee_root_inconsistent", None, &json!({"employee": hex, "label": checked.label}));
            return Err(RetireError::RootInconsistent);
        }
        RootClass::Gone => {
            let mut db = writable_apply(home)?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| RetireError::Unavailable)?;
            check(&tx, employee, Expect::Allocated)?;
            let changed = tx.execute("UPDATE root_allocations SET root_state='root_absent' WHERE employee_hex=?1 AND root_state='allocated'",
                [&hex]).map_err(|_| RetireError::Unavailable)?;
            if changed != 1 { return Err(RetireError::Stale); }
            tx.commit().map_err(|_| RetireError::Unavailable)?;
            // S5e: shown as 主機資源狀態不明, never 已釋放.
            audit(home, "isolation.employee_root_absent", &checked.label, true, json!({"employee": hex, "actor_user_id": env.actor}));
            env.notifier.notify(home, "isolation.employee_root_absent", checked.owner.as_deref(), &json!({"employee": hex, "label": checked.label}));
            return Ok(ProposeOutcome::RootAbsent);
        }
        RootClass::Retired => {
            record_retired(home, employee, None, &checked.root.receipt, env.now)?;
            audit(home, "isolation.employee_retired", &checked.label, false,
                json!({"employee": hex, "reconciled": true, "actor_user_id": env.actor}));
            return Ok(ProposeOutcome::AlreadyRetired);
        }
    }
    let mut db = writable_apply(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| RetireError::Unavailable)?;
    let checked = check(&tx, employee, Expect::Allocated)?;
    let spec = spec_for(&checked, employee, env.actor)?;
    let operation = ProposalOperationId { proposal_id: nonce(), revision: 1 };
    // The confirmation store is a separate database; a failure there rolls back
    // this apply.db transaction (nothing is retiring). A commit failure after it
    // leaves a Pending proposal that can never validate (root_state allocated).
    let mut store = confirmations(home)?;
    let item = store.stage_proposal(operation, ConfirmationCategory::EmployeeRetire, allocation::retire_fields(&spec),
        &StagingCheck(&spec)).map_err(|error| match error {
            ConfirmationError::Storage => RetireError::Unavailable,
            ConfirmationError::Stale => RetireError::Stale,
            _ => RetireError::Invalid,
        })?;
    let changed = tx.execute(
        "UPDATE root_allocations SET root_state='retiring',retire_proposal_id=?2 WHERE employee_hex=?1 AND root_state='allocated'",
        params![hex, proposal_key(&item.binding)]).map_err(|_| RetireError::Unavailable)?;
    if changed != 1 { return Err(RetireError::Stale); }
    tx.commit().map_err(|_| RetireError::Unavailable)?;
    let details = json!({"employee": hex, "proposal_id": proposal_key(&item.binding), "actor_user_id": env.actor,
        "early": spec.early, "digest_prefix": spec.quarantine_digest_prefix, "uid": spec.uid});
    audit(home, "isolation.employee_retire_proposed", &spec.label, false, details.clone());
    env.notifier.notify(home, "isolation.employee_retire_proposed", checked.owner.as_deref(), &details);
    Ok(ProposeOutcome::Staged { proposal: operation, early: spec.early })
}

/// F6i: called by the quarantine purge right after its commit. `retire` is
/// the dialog's 同時釋放主機資源 box (checked by default); unticked does nothing.
pub(crate) fn purge_hook(home: &Path, employee: EmployeeId, retire: bool, env: &ProposeEnv<'_>)
    -> Result<Option<ProposeOutcome>, RetireError> {
    if !retire { return Ok(None); }
    employee_retire_propose(home, employee, env).map(Some)
}

/// Mark the employee id retired (`root_state`, `retired_at`) and replace the
/// label's confirmed row with the Retired receipt while it still holds the
/// old id. `proposal: Some` requires that proposal (apply); `None` is the
/// S5c reconcile. `Ok(true)` when this call changed the state.
fn record_retired(home: &Path, employee: EmployeeId, proposal: Option<&str>, receipt: &Receipt, now: i64)
    -> Result<bool, RetireError> {
    let mut db = writable_apply(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| RetireError::Unavailable)?;
    let row = root_row(&tx, employee)?.ok_or(RetireError::NotFound)?;
    let allowed = match (row.state.as_str(), proposal) {
        ("retired", Some(id)) => return if row.proposal.as_deref() == Some(id) { Ok(false) } else { Err(RetireError::Stale) },
        ("retired", None) => return Ok(false),
        ("retiring", Some(id)) => row.proposal.as_deref() == Some(id),
        ("allocated" | "retiring", None) => true,
        _ => false,
    };
    if !allowed { return Err(RetireError::Stale); }
    let hex = hex::encode(employee.0);
    tx.execute("UPDATE root_allocations SET root_state='retired',retired_at=?2 WHERE employee_hex=?1", params![hex, now])
        .map_err(|_| RetireError::Unavailable)?;
    let mut dead = receipt.clone();
    dead.disabled = true;
    let current: Option<String> = tx.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [&row.receipt.label], |r| r.get(0))
        .optional().map_err(|_| RetireError::Unavailable)?;
    if let Some(encoded) = current {
        let stored: Receipt = serde_json::from_str(&encoded).map_err(|_| RetireError::Unavailable)?;
        // Only the old id's (already disabled) row; a new employee on the label is never touched.
        if stored.employee == employee && stored.disabled {
            tx.execute("UPDATE confirmed_employees SET receipt=?1 WHERE label=?2 AND receipt=?3",
                params![serde_json::to_string(&dead).map_err(|_| RetireError::Unavailable)?, row.receipt.label, encoded])
                .map_err(|_| RetireError::Unavailable)?;
        }
    }
    tx.execute("DELETE FROM employee_retire_waits WHERE employee_hex=?1", [&hex]).map_err(|_| RetireError::Unavailable)?;
    tx.commit().map_err(|_| RetireError::Unavailable)?;
    Ok(true)
}

#[path = "isolation_retire_apply.rs"]
mod apply;
pub(crate) use apply::{RetireBackend, Step};

/// §3.12 "After retire" (R7): the label is free for a new employee only when
/// no live row holds it, every quarantine row on it is closed (S7), no
/// request or proposal is open for it, every employee id that held it is
/// `retired` or `root_absent`, and every registered store reports a
/// positive zero (S10). Any unreadable source is `Err` (not free).
pub(crate) fn label_free(home: &Path, label: &str, stores: &dyn LabelStores) -> Result<bool, RetireError> {
    use RetireError::Unavailable;
    let db = readable_apply(home)?;
    if let Some(encoded) = db.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [label], |r| r.get::<_, String>(0))
        .optional().map_err(|_| Unavailable)? {
        let receipt: Receipt = serde_json::from_str(&encoded).map_err(|_| Unavailable)?;
        if !receipt.disabled { return Ok(false); }
    }
    let open_rows: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM employee_quarantine WHERE label=?1 AND seal NOT IN ('purged','adopted','restored'))",
        [label], |r| r.get(0)).map_err(|_| Unavailable)?;
    if open_rows { return Ok(false); }
    if table_exists(&db, "employee_restores")? {
        let open: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM employee_restores WHERE label=?1 AND state IN ('requested','applying'))",
            [label], |r| r.get(0)).map_err(|_| Unavailable)?;
        if open { return Ok(false); }
    }
    let mut statement = db.prepare("SELECT employee_hex FROM root_allocations LIMIT 4097").map_err(|_| Unavailable)?;
    let ids = statement.query_map([], |r| r.get::<_, String>(0)).map_err(|_| Unavailable)?
        .collect::<Result<Vec<_>, _>>().map_err(|_| Unavailable)?;
    if ids.len() > 4096 { return Err(Unavailable); }
    for id in ids {
        let bytes: [u8; 16] = hex::decode(&id).ok().and_then(|b| b.try_into().ok()).ok_or(Unavailable)?;
        let row = root_row(&db, EmployeeId(bytes))?.ok_or(Unavailable)?;
        if row.receipt.label == label && !matches!(row.state.as_str(), "retired" | "root_absent") { return Ok(false); }
    }
    drop(statement);
    // No open Employee or retire proposal names the label (a Failed retire is
    // forward only, so it still holds it).
    let path = home.join(CONFIRMATIONS_DB);
    if !matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
        let store = ConfirmationStore::from_read_only_connection(
            crate::fs_safe::sqlite::open_existing(home, CONFIRMATIONS_DB, Access::ReadOnly).map_err(|_| Unavailable)?)
            .map_err(|_| Unavailable)?;
        let open = store.list().map_err(|_| Unavailable)?.iter().any(|item| {
            let names = item.fields.iter().any(|f| f.name == "employee_id"
                && matches!(&f.value, duduclaw_migration::contract::ProposalValue::Text { value } if value == label));
            names && match item.category {
                ConfirmationCategory::Employee => matches!(item.status, ConfirmationStatus::Pending | ConfirmationStatus::Approved
                    | ConfirmationStatus::Applying | ConfirmationStatus::Rejecting),
                ConfirmationCategory::EmployeeRetire => !matches!(item.status, ConfirmationStatus::Applied
                    | ConfirmationStatus::Rejected | ConfirmationStatus::ExpiredRejected),
                _ => false,
            }
        });
        if open { return Ok(false); }
    }
    Ok(stores.count_under(home, label).map_err(|()| Unavailable)? == 0)
}
