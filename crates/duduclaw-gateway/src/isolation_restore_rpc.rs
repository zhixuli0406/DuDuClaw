//! Blocking bodies of the F6 restore RPCs (native Linux isolation v1.81, F6
//! design r5 §3.4–§3.8, §3.11, §5; task F6d). The dashboard handler
//! (`handlers/isolation_employee_restore.rs`) authenticates the caller (not
//! `system`; Admin + MFA + migration step-up for `isolation.*`; the session
//! credential epoch for `me.isolation.*`) and runs these on the blocking pool.
//!
//! - Owner (`me.isolation.*`): [`list_restorable`], [`restore_challenge`],
//!   [`request_restore`], [`cancel_own`]. Only the receipt owner, exact id
//!   equality; any other row answers `NOT_FOUND` (no existence oracle, §5).
//!   Request and cancel need a fresh proof of the owner's strongest factor
//!   (Q2: passkey assertion / TOTP / email code), bound to the row's purpose
//!   `restore:<employee_id>:<incarnation>` and verified before anything is
//!   filed; a failed proof counts toward the login lockout.
//!   The only Admin RPCs that file a Personal request are
//!   [`request_restore_for_owner`] / [`adoption_request_for_owner`] (owner
//!   decision 2026-11-09, F6fa2): only for an owner whose tombstone snapshot
//!   shows no second factor, Admin + fresh step-up, a different qualifying
//!   Admin approves (no sole-Admin path), uncapped notice to the
//!   tombstone-time address at filing and at approval, the row marked
//!   `filed_by_admin` + `owner_unverified`.
//! - Admin (`isolation.*`): [`decide`] (approve runs the F6c engine with the
//!   F6c2 rule inside the users.db transaction; `self_approved` is recorded
//!   on the request), [`admin_cancel`], [`regrant`] (one listed label-keyed
//!   item, §3.3, never automatic), [`retire_propose`] (F6l), [`admin_view`].
//! - Lock order: every path that takes both stores takes apply.db
//!   (`BEGIN IMMEDIATE`) before users.db (M6, S17); the factor proof commits
//!   its users.db transaction before any apply.db lock is taken.
//! - Notices (L1, S16) go to the apply.db outbox (`isolation_restore_wiring`).
//! - Adoption under a new employee id (F6g engine `isolation_adopt`, §3.10
//!   step 1): owner [`adoption_challenge`] + [`request_adoption`] (same
//!   ownership-first and strongest-factor rule, purpose
//!   `adopt:<employee_id>:<incarnation>`), Admin [`shared_adoption_request`];
//!   [`adoption_committed`] after the confirmation apply commits one;
//!   [`recover_adoptions_at_boot`] for every `adopting` row.
use std::path::Path;

use duduclaw_auth::mfa::{OwnerFactor, OwnerFactorError};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation::Receipt;
use duduclaw_security::audit::Severity;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::isolation_adopt::{self as adopt, AdoptEnv, AdoptError, Filer};
use crate::isolation_lifecycle::EmployeeKind;
use crate::isolation_restore::{self as engine, Close, OwnerProof, Refusal, RestoreEnv, RESTORE_WINDOW_SECS};
use crate::isolation_restore_wiring::{self as wiring, NoticeScope};

/// Most rows one listing returns.
const MAX_ROWS: i64 = 500;

/// Why an RPC refused. [`Self::code`] is stable (F6e maps it), [`Self::message`]
/// is the zh-TW text of design §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcError {
    Restore(Refusal),
    Factor(OwnerFactorError),
    Retire(crate::isolation_retire::RetireError),
    /// F6g adoption engine (`isolation_adopt::AdoptError`).
    Adopt(AdoptError),
    /// F6k Shared restore (`isolation_restore_shared::SharedError`).
    Shared(crate::isolation_restore_shared::SharedError),
    /// F6i quarantine purge (`isolation_purge::PurgeError`, never its `Restore` variant).
    Purge(crate::isolation_purge::PurgeError),
    Invalid,
}

impl From<Refusal> for RpcError { fn from(r: Refusal) -> Self { Self::Restore(r) } }
impl From<crate::isolation_restore_shared::SharedError> for RpcError {
    fn from(e: crate::isolation_restore_shared::SharedError) -> Self {
        match e { crate::isolation_restore_shared::SharedError::Restore(r) => Self::Restore(r), other => Self::Shared(other) }
    }
}
impl From<AdoptError> for RpcError {
    fn from(e: AdoptError) -> Self { match e { AdoptError::Restore(r) => Self::Restore(r), other => Self::Adopt(other) } }
}

impl RpcError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Restore(r) => r.code(),
            Self::Factor(f) => f.code(),
            Self::Retire(r) => r.code(),
            Self::Adopt(a) => a.code(),
            Self::Shared(s) => s.code(),
            Self::Purge(p) => p.code(),
            Self::Invalid => "INVALID_REQUEST",
        }
    }
    pub(crate) fn message(self) -> &'static str {
        use crate::isolation_restore::RootClass as C;
        match self {
            Self::Retire(r) => r.message(),
            Self::Adopt(a) => a.message(),
            Self::Shared(s) => s.message(),
            Self::Purge(crate::isolation_purge::PurgeError::Restore(r)) => Self::Restore(r).message(),
            Self::Purge(p) => p.message(),
            Self::Invalid => "申請內容不正確，請重新載入後再試。",
            Self::Factor(OwnerFactorError::NoFactor) => "請先設定兩步驟驗證",
            Self::Factor(OwnerFactorError::NotStrongest(_)) =>
                "請使用你目前最強的登入驗證方式（通行金鑰優先，其次為驗證器 App，最後為 Email 驗證碼）。",
            Self::Factor(OwnerFactorError::Failed) => "驗證失敗或已過期，或嘗試次數過多已暫時鎖定，請重新取得驗證後再試。",
            Self::Factor(OwnerFactorError::RateLimited) => "寄送太頻繁，請稍後再試。",
            Self::Factor(OwnerFactorError::Denied) => "登入憑證已失效，請重新登入。",
            Self::Factor(OwnerFactorError::Unavailable) => "無法讀取帳號資料。",
            Self::Factor(OwnerFactorError::BaselineMissing) =>
                "這位員工註銷時沒有留下主人的驗證方式紀錄，無法確認主人身分，因此無法還原或接手。",
            Self::Factor(OwnerFactorError::NotPreexisting) =>
                "請使用註銷當時已設定、之後未曾變更的驗證方式；註銷後才新增或重新設定的驗證方式不能用來證明身分。",
            Self::Restore(refusal) => match refusal {
                Refusal::NotFound | Refusal::NotOwner => "找不到這位員工，或這筆申請。",
                Refusal::FactorRequired => "請先完成兩步驟驗證。",
                Refusal::OwnerIneligible => "原主人目前無法使用帳號",
                Refusal::SnapshotIncomplete => "這位員工註銷時的帳號紀錄不完整，無法確認主人的登入方式未被他人變更，因此無法還原。",
                Refusal::CredentialChanged => "主人的登入方式在註銷後被他人變更",
                Refusal::LabelTaken | Refusal::LabelPending => "已有同名員工或提案",
                Refusal::Integrity | Refusal::OrgChanged | Refusal::StoresChanged => "隔離區資料與註銷時不一致",
                Refusal::Expired => "已超過保留期限",
                Refusal::RoutesChanged => "連線清單已變更，請重新申請",
                Refusal::RestoreInProgress => "另一個還原正在進行",
                Refusal::RetireInProgress => "主機資源釋放中",
                Refusal::RateLimited => "這位員工 30 天內的還原申請已達上限，請稍後再申請。",
                Refusal::NotSealed => "此員工的資料無法完整封存，之後無法還原",
                Refusal::Root(C::Unavailable) => "主機暫時無法連線，請稍後再試",
                Refusal::Root(C::Busy) => "主機正在執行其他工作，稍後會自動重試",
                Refusal::Root(C::Retired) => "主機資源已釋放，無法還原",
                Refusal::Root(C::Inconsistent) => "主機資源與紀錄不一致，請聯絡維運",
                Refusal::Root(C::Gone) => "原本的主機資源已不存在，資料會交給一位新的員工，舊的授權與連線都不會帶過去",
                Refusal::Approval("SECOND_ADMIN_REQUIRED") => "需由另一位管理員核准",
                Refusal::Approval("APPROVER_NOT_QUALIFIED") => "擔任管理員未滿 7 天，或由申請人設為管理員，無法核准",
                Refusal::Approval("SELF_APPROVAL_COOLING") => "7 天內有管理員失去權限，暫時不能自行核准",
                Refusal::Approval("ADMIN_LOG_UNAVAILABLE") => "管理員權限紀錄無法讀取，暫時不能自行核准",
                Refusal::Approval("STEP_UP_REQUIRED") => "請先以密碼與雙因素驗證確認身分。",
                Refusal::Approval("NO_SECOND_ADMIN") =>
                    "代主人申請需由另一位管理員核准，目前沒有其他可核准的管理員（擔任管理員滿 7 天且不是由申請人設定），因此無法代為申請或核准。",
                Refusal::Approval("FILER_NOT_ELIGIBLE") => "只有目前已啟用、設定兩步驟驗證的管理員可以代主人申請。",
                Refusal::OwnerHadSecondFactor =>
                    "主人在註銷時已設定兩步驟驗證，必須由主人本人以該驗證方式申請，管理員不能代為申請。",
                Refusal::FilerIsOwner => "你是這位員工的主人，請在「我的帳號」以兩步驟驗證自行申請。",
                Refusal::Approval(_) => "無法核准這筆申請，請重新載入後再試。",
                Refusal::TombstoneIncomplete | Refusal::FencePending => "註銷尚未完成，系統會重試，請稍後再試。",
                Refusal::TenantMismatch => "租戶與註銷時不一致，無法還原。",
                Refusal::QuotaExceeded => "AI 員工數量已達授權上限。",
                Refusal::StoresUnavailable | Refusal::Unavailable => "員工資料目前無法讀取，已拒絕操作。",
                Refusal::NotSupported => "這種申請目前無法處理。",
                Refusal::Inconsistent | Refusal::Root(C::Ok) => "紀錄不一致，已通知維運。",
            },
        }
    }
}

fn hex16(value: &str) -> Result<EmployeeId, RpcError> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return Err(RpcError::Invalid); }
    let bytes = hex::decode(value).map_err(|_| RpcError::Invalid)?;
    Ok(EmployeeId(<[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| RpcError::Invalid)?))
}

fn request_id(value: &str) -> Result<&str, RpcError> {
    if value.len() == 32 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { Ok(value) } else { Err(RpcError::Invalid) }
}

/// The owner's factor check `(factor, purpose, code)`, run before anything is
/// filed. `purpose` (`restore:<employee_id>:<incarnation>`) is derived here
/// from the verified row, never from the client. Production:
/// `UserDb::restore_owner_factor` (its users.db transaction commits first).
pub(crate) type FactorCheck<'a> = &'a dyn Fn(OwnerFactor, &str, &str) -> Result<OwnerFactor, OwnerFactorError>;

/// The proof purpose of one quarantine row.
fn purpose(employee: EmployeeId, incarnation: u32) -> Result<String, RpcError> {
    duduclaw_auth::mfa::owner_factor::restore_purpose(&hex::encode(employee.0), incarnation).ok_or(RpcError::Invalid)
}

/// The tombstone-time factor baseline (F6fa) of the caller's own Personal
/// row named by a server-built proof purpose (`restore:` or
/// `adopt:<employee_id>:<incarnation>`). The production [`FactorCheck`] and
/// challenge issuer pass it to `UserDb::restore_owner_factor` /
/// `restore_challenge`, so the strongest factor and its credential are the
/// tombstone's, never the live account's. A snapshot recorded before the
/// baseline existed refuses `FACTOR_BASELINE_MISSING` (no weaker fallback).
/// Read before the factor's users.db transaction, holding no lock (M6).
pub(crate) fn factor_baseline(home: &Path, caller: &str, purpose: &str)
    -> Result<duduclaw_auth::mfa::owner_factor::FactorBaseline, OwnerFactorError> {
    let rest = purpose.strip_prefix("restore:").or_else(|| purpose.strip_prefix("adopt:")).ok_or(OwnerFactorError::Denied)?;
    let (hex, incarnation) = rest.split_once(':').ok_or(OwnerFactorError::Denied)?;
    let employee = hex16(hex).map_err(|_| OwnerFactorError::Denied)?;
    let incarnation: u32 = incarnation.parse().map_err(|_| OwnerFactorError::Denied)?;
    if !is_row_purpose(purpose, employee, incarnation) { return Err(OwnerFactorError::Denied); }
    let q = match own_row(home, caller, employee, incarnation) {
        Ok(q) => q,
        Err(RpcError::Restore(Refusal::Unavailable)) => return Err(OwnerFactorError::Unavailable),
        Err(_) => return Err(OwnerFactorError::Denied),
    };
    duduclaw_auth::mfa::owner_factor::FactorBaseline::from_snapshot(&q.principal_snapshot).ok_or(OwnerFactorError::BaselineMissing)
}

/// `purpose` is exactly the canonical restore or adoption purpose of the row.
fn is_row_purpose(purpose: &str, employee: EmployeeId, incarnation: u32) -> bool {
    use duduclaw_auth::mfa::owner_factor::{adopt_purpose, restore_purpose};
    let hex = hex::encode(employee.0);
    restore_purpose(&hex, incarnation).as_deref() == Some(purpose) || adopt_purpose(&hex, incarnation).as_deref() == Some(purpose)
}

/// Who gets a request-side notice: the log-eligible Admins (S16) and, for a
/// Personal row, the tombstone-time address. An uncapped (security) notice
/// that cannot be queued is audited Critical (`wiring::queue_or_alert`).
fn notify(home: &Path, q: &engine::Quarantined, event: &str, details: Value, capped: bool, now: i64) -> bool {
    let mut recipients = wiring::admin_notice_addresses(home, &q.label, event, now);
    if let Some(address) = q.principal_snapshot["notice_address"].as_str() { recipients.push(address.to_owned()); }
    let mut details = details;
    details["label"] = json!(q.label);
    wiring::queue_or_alert(home, &scope_of(q), event, &recipients, &details, capped, now)
}

fn scope_of(q: &engine::Quarantined) -> NoticeScope {
    match &q.owner {
        Some(owner) => NoticeScope::Owner(owner.clone()),
        None => NoticeScope::Tenant(q.tenant.clone()),
    }
}

fn row_of(home: &Path, employee: EmployeeId, incarnation: u32) -> Result<Option<engine::Quarantined>, Refusal> {
    let db = engine::open_apply(home)?;
    engine::read_row(&db, employee, incarnation)
}

/// The caller's own Personal row; anything else is `NOT_FOUND` (§5).
fn own_row(home: &Path, caller: &str, employee: EmployeeId, incarnation: u32) -> Result<engine::Quarantined, RpcError> {
    let q = row_of(home, employee, incarnation)?.ok_or(Refusal::NotFound)?;
    if q.kind != EmployeeKind::Personal || q.owner.as_deref() != Some(caller) { return Err(Refusal::NotFound.into()); }
    Ok(q)
}

fn verify_factor(check: FactorCheck<'_>, factor: &str, purpose: &str, code: &str) -> Result<OwnerFactor, RpcError> {
    let factor = OwnerFactor::parse(factor).ok_or(RpcError::Factor(OwnerFactorError::Denied))?;
    check(factor, purpose, code).map_err(RpcError::Factor)
}

/// `me.isolation.restore_challenge`: ownership first (`NOT_FOUND` for every
/// other row, no factor oracle), then `issue(purpose)` — production
/// `UserDb::restore_challenge` (the strongest factor's passkey ceremony or
/// email code, or nothing for TOTP; users.db only).
pub(crate) fn restore_challenge<T>(home: &Path, caller: &str, employee: &str, incarnation: u32,
    issue: &dyn Fn(&str) -> Result<T, OwnerFactorError>) -> Result<T, RpcError> {
    let employee = hex16(employee)?;
    own_row(home, caller, employee, incarnation)?;
    issue(&purpose(employee, incarnation)?).map_err(RpcError::Factor)
}

/// `root_state` of each employee id (no root IPC).
fn root_state(db: &rusqlite::Connection, employee: EmployeeId) -> Result<Option<String>, Refusal> {
    db.query_row("SELECT root_state FROM root_allocations WHERE employee_hex=?1", [hex::encode(employee.0)], |r| r.get(0))
        .optional().map_err(|_| Refusal::Unavailable)
}

fn root_flag(state: Option<&str>) -> Option<&'static str> {
    match state {
        Some("root_absent") => Some("ROOT_GONE"),
        Some("retiring" | "retired") => Some("ROOT_RETIRED"),
        Some("allocated") => None,
        _ => Some("ROOT_INCONSISTENT"),
    }
}

fn row_value(db: &rusqlite::Connection, q: &engine::Quarantined, now: i64) -> Result<Value, Refusal> {
    let open = engine::open_request(db, q.employee, q.incarnation)?;
    let deadline = q.quarantined_at.saturating_add(RESTORE_WINDOW_SECS);
    let state = root_state(db, q.employee)?;
    let flag = root_flag(state.as_deref());
    let allocated = state.as_deref() == Some("allocated");
    Ok(json!({
        "employee_id": hex::encode(q.employee.0), "incarnation": q.incarnation, "label": q.label,
        "kind": match q.kind { EmployeeKind::Personal => "personal", EmployeeKind::Shared => "shared" },
        "owner_user_id": q.owner, "seal": q.seal, "tombstoned_at": q.quarantined_at, "restore_deadline": deadline,
        "restorable": q.seal == "sealed" && now <= deadline && open.is_none() && allocated,
        "root_state": state, "root_flag": flag,
        // F6fa2: an Admin may file for this owner (no second factor at the tombstone).
        "admin_may_file_for_owner": engine::admin_may_file_for_owner(q),
        "open_request": open.map(|r| json!({"request_id": r.request_id, "kind": r.kind.as_str(), "state": r.state,
            "requester_user_id": r.requester, "created_at": r.created_at, "expires_at": r.expires_at,
            "filed_by_admin": r.filed_by_admin, "owner_unverified": r.owner_unverified()})),
    }))
}

fn rows_where(home: &Path, owner: Option<&str>) -> Result<Vec<engine::Quarantined>, Refusal> {
    let db = engine::open_apply(home)?;
    let keys: Vec<(String, i64)> = {
        let mut statement = db.prepare("SELECT employee_hex,incarnation FROM employee_quarantine
            WHERE ?1 IS NULL OR (kind='personal' AND owner_user_id=?1) ORDER BY quarantined_at DESC LIMIT ?2")
            .map_err(|_| Refusal::Unavailable)?;
        let rows = statement.query_map(params![owner, MAX_ROWS], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|_| Refusal::Unavailable)?.collect::<Result<Vec<_>, _>>().map_err(|_| Refusal::Unavailable)?;
        rows
    };
    keys.into_iter().map(|(hex, incarnation)| {
        let employee = hex16(&hex).map_err(|_| Refusal::Unavailable)?;
        let incarnation = u32::try_from(incarnation).map_err(|_| Refusal::Unavailable)?;
        engine::read_row(&db, employee, incarnation)?.ok_or(Refusal::Unavailable)
    }).collect()
}

/// `me.isolation.list_restorable` (我的帳號 → 已註銷): the caller's own
/// tombstoned Personal employees only (exact owner id).
pub(crate) fn list_restorable(home: &Path, caller: &str, now: i64) -> Result<Value, RpcError> {
    // A home that never confirmed an employee has nothing tombstoned.
    if matches!(std::fs::symlink_metadata(home.join(crate::isolation_lifecycle::APPLY_DB)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
        return Ok(json!({"restorable": []}));
    }
    engine::expire_requests(home, now)?;
    let db = engine::open_apply(home)?;
    let rows: Vec<Value> = rows_where(home, Some(caller))?.iter()
        .filter(|q| q.owner.as_deref() == Some(caller))
        .map(|q| row_value(&db, q, now)).collect::<Result<_, _>>()?;
    Ok(json!({"restorable": rows}))
}

/// Admin view (`isolation.employee_list` `restores`): every quarantine row
/// and the recent requests with `self_approved`.
pub(crate) fn admin_view(home: &Path, now: i64) -> Result<Value, RpcError> {
    engine::expire_requests(home, now)?;
    let db = engine::open_apply(home)?;
    let rows: Vec<Value> = rows_where(home, None)?.iter()
        // F6i: each row's `purge` block (state, 90-day date, early_purge, purged_at).
        .map(|q| row_value(&db, q, now).and_then(|value| crate::isolation_purge::extend_view(&db, q, value, now)))
        .collect::<Result<_, _>>()?;
    let mut statement = db.prepare("SELECT request_id FROM employee_restores ORDER BY created_at DESC LIMIT ?1")
        .map_err(|_| Refusal::Unavailable)?;
    let ids: Vec<String> = statement.query_map([MAX_ROWS], |r| r.get(0)).map_err(|_| Refusal::Unavailable)?
        .collect::<Result<_, _>>().map_err(|_| Refusal::Unavailable)?;
    let requests: Vec<Value> = ids.iter().map(|id| {
        let r = engine::read_request(&db, id)?.ok_or(Refusal::Unavailable)?;
        Ok(json!({"request_id": r.request_id, "kind": r.kind.as_str(), "label": r.label, "employee_id": hex::encode(r.employee.0),
            "incarnation": r.incarnation, "requester_user_id": r.requester, "state": r.state, "created_at": r.created_at,
            "expires_at": r.expires_at, "approver_user_id": r.approver, "self_approved": r.self_approved, "failure": r.failure,
            "owner_user_id": r.owner, "filed_by_admin": r.filed_by_admin, "owner_unverified": r.owner_unverified()}))
    }).collect::<Result<_, Refusal>>()?;
    Ok(json!({"quarantine": rows, "requests": requests}))
}

/// `me.isolation.request_restore`: the owner files a Personal restore.
pub(crate) fn request_restore(home: &Path, env: &RestoreEnv<'_>, caller: &str, employee: &str, incarnation: u32,
    factor: &str, code: &str, check: FactorCheck<'_>) -> Result<Value, RpcError> {
    let employee = hex16(employee)?;
    // Ownership first (NOT_FOUND for every other row), then the factor.
    own_row(home, caller, employee, incarnation)?;
    let factor = verify_factor(check, factor, &purpose(employee, incarnation)?, code)?;
    let filed = engine::file_personal_request(home, env, employee, incarnation,
        Some(OwnerProof { user_id: caller, factor: factor.as_str() }))?;
    let request = filed.request;
    let q = own_row(home, caller, employee, incarnation)?;
    let notified = notify(home, &q, "isolation.employee_restore_requested",
        json!({"request_id": request.request_id, "expires_at": request.expires_at}), true, env.now);
    Ok(json!({"request_id": request.request_id, "state": request.state, "label": request.label,
        "expires_at": request.expires_at, "approval_required": true, "notified": notified}))
}

/// `me.isolation.cancel_restore`: the requester (the owner) cancels, with
/// a factor proof.
pub(crate) fn cancel_own(home: &Path, caller: &str, id: &str, factor: &str, code: &str, check: FactorCheck<'_>, now: i64)
    -> Result<Value, RpcError> {
    let id = request_id(id)?;
    let request = { let db = engine::open_apply(home)?; engine::read_request(&db, id)? }.ok_or(Refusal::NotFound)?;
    if request.requester != caller || request.owner.as_deref() != Some(caller) { return Err(Refusal::NotFound.into()); }
    verify_factor(check, factor, &purpose(request.employee, request.incarnation)?, code)?;
    engine::close_request(home, id, caller, Close::Cancelled, now)?;
    if let Some(q) = row_of(home, request.employee, request.incarnation)? {
        notify(home, &q, "isolation.employee_restore_cancelled", json!({"request_id": id, "actor_user_id": caller}), true, now);
    }
    Ok(json!({"request_id": id, "state": "cancelled"}))
}

/// `isolation.employee_restore_cancel` (Admin + step-up, checked by the caller).
pub(crate) fn admin_cancel(home: &Path, actor: &str, id: &str, now: i64) -> Result<Value, RpcError> {
    let id = request_id(id)?;
    let request = { let db = engine::open_apply(home)?; engine::read_request(&db, id)? }.ok_or(Refusal::NotFound)?;
    engine::close_request(home, id, actor, Close::Cancelled, now)?;
    if let Some(q) = row_of(home, request.employee, request.incarnation)? {
        notify(home, &q, "isolation.employee_restore_cancelled", json!({"request_id": id, "actor_user_id": actor}), true, now);
    }
    Ok(json!({"request_id": id, "state": "cancelled"}))
}

/// `isolation.employee_restore_decide` (Admin + step-up, checked by the
/// caller; `env.rule` carries `fresh_step_up = true`). Approve runs the F6c
/// apply (apply.db before users.db); the label-keyed state is then only
/// listed for the admin (§3.3). Deny closes the request.
pub(crate) fn decide(home: &Path, env: &RestoreEnv<'_>, approver: &str, id: &str, approve: bool,
    notices: &dyn Fn() -> Vec<String>) -> Result<Value, RpcError> {
    let id = request_id(id)?;
    let request = { let db = engine::open_apply(home)?; engine::read_request(&db, id)? }.ok_or(Refusal::NotFound)?;
    if !approve {
        engine::close_request(home, id, approver, Close::Denied, env.now)?;
        if let Some(q) = row_of(home, request.employee, request.incarnation)? {
            notify(home, &q, "isolation.employee_restore_denied", json!({"request_id": id, "approver_user_id": approver}), true, env.now);
        }
        return Ok(json!({"request_id": id, "state": "denied"}));
    }
    // F6k: a Shared request is re-checked (members, ticks) and approved with
    // only its ticked tenant routes; every other kind goes to the engine as is.
    let restored = crate::isolation_restore_shared::approve(home, env, &request, approver)?;
    let sealed = {
        let db = engine::open_apply(home)?;
        crate::isolation_quarantine::read_quarantine(&db, request.employee, request.incarnation).map_err(|_| Refusal::Unavailable)?
    }.ok_or(Refusal::Unavailable)?;
    let label_state = crate::isolation_label_restore::restore_label_stores(home, &request.label, &sealed.stripped)
        .map(|report| report.to_value())
        .unwrap_or_else(|_| json!({"regrant": [], "skipped": [], "listed": [], "unavailable": true}));
    if let Some(q) = row_of(home, request.employee, request.incarnation)? {
        // Never dropped; reaches the tombstone-time address (F6fa2: also the
        // approval notice of an Admin-filed request).
        notify(home, &q, "isolation.employee_restored", json!({"request_id": id, "approver_user_id": approver,
            "self_approved": restored.self_approved, "incarnation": restored.incarnation,
            "filed_by_admin": request.filed_by_admin, "owner_unverified": request.owner_unverified()}), false, env.now);
        if restored.self_approved {
            // S16: the addresses the rule read from the log in the apply tx,
            // plus (Personal, §3.4) the tombstone-time notice address.
            let mut recipients = notices();
            let mut details = json!({"request_id": id, "approver_user_id": approver, "label": q.label});
            details["log_addresses"] = json!(recipients.len());
            if let Some(address) = q.principal_snapshot["notice_address"].as_str() { recipients.push(address.to_owned()); }
            wiring::queue_or_alert(home, &scope_of(&q), "isolation.employee_restore_self_approved", &recipients, &details, false, env.now);
        }
    }
    let mut reply = json!({"request_id": id, "state": "done", "label": request.label, "incarnation": restored.incarnation,
        "self_approved": restored.self_approved, "label_state": label_state,
        "work_paused": true, "routes_need_provisioning": true});
    // F6k (§3.7, §3.9): Shared re-bound members, re-approved tenant routes, member notices.
    crate::isolation_restore_shared::after_restore(home, &request, restored.incarnation, &mut reply, env.now);
    Ok(reply)
}

/// `isolation.employee_restore_regrant` (Admin + step-up, audited): re-apply
/// one listed label-keyed item of a restored row. Only while the restored
/// employee is live at exactly the next incarnation.
pub(crate) fn regrant(home: &Path, actor: &str, employee: &str, incarnation: u32, item: &str) -> Result<Value, RpcError> {
    use crate::isolation_label_restore::{regrant_item, Regranted};
    let employee = hex16(employee)?;
    if item.is_empty() || item.len() > 512 || item.chars().any(char::is_control) { return Err(RpcError::Invalid); }
    let db = engine::open_apply(home)?;
    let q = crate::isolation_quarantine::read_quarantine(&db, employee, incarnation).map_err(|_| Refusal::Unavailable)?
        .ok_or(Refusal::NotFound)?;
    if q.seal != "restored" { return Err(Refusal::NotFound.into()); }
    let live: Option<(String, i64)> = db.query_row("SELECT receipt,incarnation FROM confirmed_employees WHERE label=?1",
        [&q.label], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|_| Refusal::Unavailable)?;
    let (encoded, live_incarnation) = live.ok_or(Refusal::NotFound)?;
    let receipt: Receipt = serde_json::from_str(&encoded).map_err(|_| Refusal::Unavailable)?;
    if receipt.employee != employee || receipt.disabled || receipt.label != q.label
        || i64::from(incarnation) + 1 != live_incarnation {
        return Err(Refusal::NotFound.into());
    }
    drop(db);
    if engine::restore_in_progress(home, &q.label)? { return Err(Refusal::RestoreInProgress.into()); }
    let outcome = regrant_item(home, &q.label, &q.stripped, item).map_err(|_| Refusal::Unavailable)?;
    let (state, reason) = match outcome {
        Regranted::Done => ("regranted", None),
        Regranted::Skipped(reason) => ("skipped", Some(reason)),
        Regranted::NotListed => return Err(Refusal::NotFound.into()),
    };
    engine::audit(home, "isolation.employee_restore_regranted", &q.label, Severity::Warning, json!({
        "employee": hex::encode(employee.0), "incarnation": incarnation, "item": duduclaw_core::truncate_chars(item, 160),
        "outcome": state, "reason": reason, "actor_user_id": actor}));
    Ok(json!({"item": item, "state": state, "reason": reason}))
}

/// `isolation.employee_retire_propose` (Admin + step-up): F6l's propose with
/// the production root and notifier, refused while a restore is applying.
pub(crate) fn retire_propose(home: &Path, actor: &str, employee: &str, now: i64) -> Result<Value, RpcError> {
    use crate::isolation_retire::{employee_retire_propose, ProposeEnv, ProposeOutcome, SpawndRetireRoot};
    let employee = hex16(employee)?;
    let labels: Vec<String> = {
        let db = engine::open_apply(home)?;
        let mut statement = db.prepare("SELECT DISTINCT label FROM employee_quarantine WHERE employee_hex=?1")
            .map_err(|_| Refusal::Unavailable)?;
        let rows = statement.query_map([hex::encode(employee.0)], |r| r.get(0)).map_err(|_| Refusal::Unavailable)?
            .collect::<Result<_, _>>().map_err(|_| Refusal::Unavailable)?;
        rows
    };
    for label in &labels {
        if engine::restore_in_progress(home, label)? { return Err(Refusal::RestoreInProgress.into()); }
    }
    let env = ProposeEnv { actor, root: &SpawndRetireRoot, notifier: &wiring::OutboxNotifier, now };
    match employee_retire_propose(home, employee, &env).map_err(RpcError::Retire)? {
        ProposeOutcome::Staged { proposal, early } => Ok(json!({"state": "staged",
            "proposal_id": hex::encode(proposal.proposal_id.0), "revision": proposal.revision, "early": early,
            "approval_required": true})),
        ProposeOutcome::RootAbsent => Ok(json!({"state": "root_absent"})),
        ProposeOutcome::AlreadyRetired => Ok(json!({"state": "retired"})),
    }
}

// ── F6g adoption requests (§3.10 step 1, Q2, Q8) ───────────────────────

/// The proof purpose of an adoption of one row (never a restore's).
fn adopt_purpose(employee: EmployeeId, incarnation: u32) -> Result<String, RpcError> {
    duduclaw_auth::mfa::owner_factor::adopt_purpose(&hex::encode(employee.0), incarnation).ok_or(RpcError::Invalid)
}

/// A requested target label: `None` = the original label; else a valid,
/// non-reserved agent id (the engine checks that it is free, R6).
fn target_label(target: Option<&str>) -> Result<Option<&str>, RpcError> {
    match target {
        Some(t) if !duduclaw_core::is_valid_agent_id(t) || duduclaw_core::is_reserved_agent_id(t) => Err(RpcError::Invalid),
        other => Ok(other),
    }
}

/// `me.isolation.restore_challenge` with `for_adoption: true`: ownership
/// first, then the strongest factor's challenge bound to
/// `adopt:<employee_id>:<incarnation>` (same issuer as a restore).
pub(crate) fn adoption_challenge<T>(home: &Path, caller: &str, employee: &str, incarnation: u32,
    issue: &dyn Fn(&str) -> Result<T, OwnerFactorError>) -> Result<T, RpcError> {
    let employee = hex16(employee)?;
    own_row(home, caller, employee, incarnation)?;
    issue(&adopt_purpose(employee, incarnation)?).map_err(RpcError::Factor)
}

/// The reply and the request-side notice (L1-capped) of a filed adoption.
fn adoption_filed(home: &Path, request: &engine::RestoreRequest, now: i64) -> Result<Value, RpcError> {
    let (target, _) = { let db = engine::open_apply(home)?; adopt::adoption_columns(&db, &request.request_id)? };
    let notified = match row_of(home, request.employee, request.incarnation)? {
        Some(q) => notify(home, &q, "isolation.employee_adoption_requested",
            json!({"request_id": request.request_id, "expires_at": request.expires_at, "target_label": target}), true, now),
        None => false,
    };
    Ok(json!({"request_id": request.request_id, "state": request.state, "label": request.label, "target_label": target,
        "expires_at": request.expires_at, "approval_required": true, "notified": notified}))
}

/// `me.isolation.request_adoption`: the owner files a Personal adoption
/// (F6g `file_adoption`). Ownership first (`NOT_FOUND` for every other row,
/// no factor oracle), then the owner's strongest factor bound to
/// `adopt:<employee_id>:<incarnation>`; the engine re-checks the owner.
pub(crate) fn request_adoption(home: &Path, env: &AdoptEnv<'_>, caller: &str, employee: &str, incarnation: u32,
    target: Option<&str>, factor: &str, code: &str, check: FactorCheck<'_>) -> Result<Value, RpcError> {
    let employee = hex16(employee)?;
    let target = target_label(target)?;
    own_row(home, caller, employee, incarnation)?;
    let factor = verify_factor(check, factor, &adopt_purpose(employee, incarnation)?, code)?;
    let request = adopt::file_adoption(home, env, employee, incarnation, target,
        Filer::Personal(Some(OwnerProof { user_id: caller, factor: factor.as_str() })))?;
    adoption_filed(home, &request, env.now)
}

/// `isolation.shared_adoption_request` (Admin + MFA + step-up, checked by
/// the caller): a Shared adoption with the member selection
/// `[{user_id, access_level}]` ⊆ the tombstone snapshot (F6g `check_members`).
pub(crate) fn shared_adoption_request(home: &Path, env: &AdoptEnv<'_>, actor: &str, employee: &str, incarnation: u32,
    target: Option<&str>, members: Value) -> Result<Value, RpcError> {
    let employee = hex16(employee)?;
    let target = target_label(target)?;
    if !members.is_array() { return Err(RpcError::Invalid); }
    let request = adopt::file_adoption(home, env, employee, incarnation, target, Filer::Shared { requester: actor, members })?;
    adoption_filed(home, &request, env.now)
}

// ── F6fa2: an Admin files for an owner without a second factor ─────────

/// The filing notice of an Admin-filed request (owner decision 2026-11-09):
/// **uncapped** (a security notice, never dropped by the L1 cap), to the
/// log-eligible Admins and the owner's tombstone-time notice address.
fn notify_filed_for_owner(home: &Path, request: &engine::RestoreRequest, event: &str, extra: Value, now: i64) -> Result<bool, RpcError> {
    let Some(q) = row_of(home, request.employee, request.incarnation)? else { return Ok(false) };
    let mut details = json!({"request_id": request.request_id, "kind": request.kind.as_str(), "expires_at": request.expires_at,
        "owner_user_id": request.owner, "filed_by_admin": request.filed_by_admin, "owner_unverified": request.owner_unverified()});
    if let (Some(target), Value::Object(more)) = (details.as_object_mut(), extra) { target.extend(more); }
    Ok(notify(home, &q, event, details, false, now))
}

/// The reply of an Admin-filed request (UI: 主人未用兩步驟驗證確認).
fn filed_for_owner_reply(request: &engine::RestoreRequest, target: Option<&str>, notified: bool) -> Value {
    let mut reply = json!({"request_id": request.request_id, "kind": request.kind.as_str(), "state": request.state,
        "label": request.label, "owner_user_id": request.owner, "expires_at": request.expires_at, "approval_required": true,
        "second_admin_required": true, "filed_by_admin": request.filed_by_admin, "owner_unverified": request.owner_unverified(),
        "notified": notified});
    if let Some(target) = target { reply["target_label"] = json!(target); }
    reply
}

/// `isolation.employee_restore_request_for_owner` (Admin + MFA + migration
/// step-up, checked by the caller; `fresh_step_up` = that proof's
/// freshness): the Admin files a Personal restore for the owner of a row
/// whose tombstone snapshot shows no second factor (F6c
/// `file_personal_request_for_owner`). The owner is named by the row, never
/// by a request field.
pub(crate) fn request_restore_for_owner(home: &Path, env: &RestoreEnv<'_>, actor: &str, employee: &str, incarnation: u32,
    fresh_step_up: bool) -> Result<Value, RpcError> {
    let employee = hex16(employee)?;
    let filed = engine::file_personal_request_for_owner(home, env, employee, incarnation,
        engine::AdminFiler { admin_user_id: actor, fresh_step_up })?;
    let request = filed.request;
    let notified = notify_filed_for_owner(home, &request, "isolation.employee_restore_requested_for_owner", json!({}), env.now)?;
    Ok(filed_for_owner_reply(&request, None, notified))
}

/// `isolation.adoption_request_for_owner` (Admin + MFA + migration step-up):
/// the Admin files a Personal adoption (F6g, `Personal(same owner)`) for an
/// owner without a second factor at the tombstone (`Filer::AdminForOwner`).
pub(crate) fn adoption_request_for_owner(home: &Path, env: &AdoptEnv<'_>, actor: &str, employee: &str, incarnation: u32,
    target: Option<&str>, fresh_step_up: bool) -> Result<Value, RpcError> {
    let employee = hex16(employee)?;
    let target = target_label(target)?;
    let request = adopt::file_adoption(home, env, employee, incarnation, target,
        Filer::AdminForOwner(engine::AdminFiler { admin_user_id: actor, fresh_step_up }))?;
    let (target, _) = { let db = engine::open_apply(home)?; adopt::adoption_columns(&db, &request.request_id)? };
    let notified = notify_filed_for_owner(home, &request, "isolation.employee_adoption_requested_for_owner",
        json!({"target_label": target}), env.now)?;
    Ok(filed_for_owner_reply(&request, target.as_deref(), notified))
}

/// After the confirmation store applied `item` (the Employee backend commits
/// an adoption inside that call): `true` when it was an adoption, so the
/// caller rescans the registry; the approval notice (never dropped) goes to
/// the log-eligible Admins and the owner's tombstone-time address.
pub(crate) fn adoption_committed(home: &Path, item: &duduclaw_migration::contract::ConfirmationItem, approver: &str, now: i64) -> bool {
    let Ok(Some(reference)) = adopt::proposal_ref(item) else { return false };
    let key = adopt::proposal_key(item);
    let found = engine::open_apply(home).ok().and_then(|db| {
        let request = adopt::request_for_proposal(&db, reference.employee, reference.incarnation, &key).ok().flatten()?;
        let target = adopt::adoption_columns(&db, &request.request_id).ok().and_then(|(target, _)| target);
        Some((request, target))
    });
    if let (Some((request, target)), Ok(Some(q))) = (found, row_of(home, reference.employee, reference.incarnation)) {
        notify(home, &q, "isolation.employee_adopted", json!({"request_id": request.request_id, "approver_user_id": approver,
            "self_approved": request.self_approved, "target_label": target,
            "filed_by_admin": request.filed_by_admin, "owner_unverified": request.owner_unverified()}), false, now);
    }
    true
}

/// What boot recovery did: `(old label, request id, action)` per adoption,
/// and `(label, code)` per label whose recovery refused (retried at the
/// next request, proposal or boot).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct BootAdoptions {
    pub(crate) done: Vec<(String, String, &'static str)>,
    pub(crate) failed: Vec<(String, &'static str)>,
}

/// Boot (F6g §3.6, §3.10 step 6): `recover_adoptions` for every label with
/// an `adopting` row. A home without apply.db has nothing to recover.
pub(crate) fn recover_adoptions_at_boot(home: &Path, env: &AdoptEnv<'_>) -> Result<BootAdoptions, RpcError> {
    if matches!(std::fs::symlink_metadata(home.join(crate::isolation_lifecycle::APPLY_DB)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
        return Ok(BootAdoptions::default());
    }
    let labels: Vec<String> = {
        let db = engine::open_apply(home)?;
        let mut statement = db.prepare("SELECT DISTINCT label FROM employee_quarantine WHERE seal='adopting' ORDER BY label")
            .map_err(|_| Refusal::Unavailable)?;
        let rows = statement.query_map([], |r| r.get(0)).map_err(|_| Refusal::Unavailable)?
            .collect::<Result<_, _>>().map_err(|_| Refusal::Unavailable)?;
        rows
    };
    let mut out = BootAdoptions::default();
    for label in labels {
        match adopt::recover_adoptions(home, env, &label) {
            Ok(done) => out.done.extend(done.into_iter().map(|(request, action)| (label.clone(), request, action))),
            Err(error) => out.failed.push((label, error.code())),
        }
    }
    Ok(out)
}

/// Production boot recovery (protected mode): the schema-2 root, catalog
/// routes, run stopper and F6c2 rule (`isolation_adopt::with_production_env`).
pub(crate) fn recover_adoptions_on_boot(home: &Path, now: i64) -> Result<BootAdoptions, RpcError> {
    adopt::with_production_env(home, "system", now, |env| recover_adoptions_at_boot(home, env))
}

/// §3.5 mutual exclusion (`RESTORE_IN_PROGRESS`) for the tombstone and
/// propose RPCs, read on the caller's own apply.db connection (which may hold
/// the writer lock): a quarantine row of the label `restoring`/`adopting`, or
/// a request on it `applying`. Never creates a table; a store without the F6
/// tables has nothing restoring. Same rule as `isolation_restore::restore_in_progress`.
pub(crate) fn refuse_while_restoring(db: &rusqlite::Connection, label: &str) -> Result<(), &'static str> {
    let table = |name: &str| db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name], |r| r.get::<_, bool>(0));
    let unavailable = RpcError::Restore(Refusal::Unavailable).message();
    let mut busy = false;
    if table("employee_quarantine").map_err(|_| unavailable)? {
        busy |= db.query_row("SELECT EXISTS(SELECT 1 FROM employee_quarantine WHERE label=?1 AND seal IN ('restoring','adopting'))",
            [label], |r| r.get::<_, bool>(0)).map_err(|_| unavailable)?;
    }
    if table("employee_restores").map_err(|_| unavailable)? {
        busy |= db.query_row("SELECT EXISTS(SELECT 1 FROM employee_restores WHERE label=?1 AND state='applying')",
            [label], |r| r.get::<_, bool>(0)).map_err(|_| unavailable)?;
    }
    if busy { Err(RpcError::Restore(Refusal::RestoreInProgress).message()) } else { Ok(()) }
}
