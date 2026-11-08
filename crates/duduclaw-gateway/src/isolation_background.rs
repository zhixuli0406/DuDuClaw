//! Durable, record-bound authority for protected background work.
//! A stored owner label is a claim awaiting approval, never authentication.
use std::{future::Future, path::{Path, PathBuf}, sync::Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use duduclaw_isolation::{capability::{EmployeeBinding, Lane}, execution::{RunHandle, WaitOrigin}, ids::EmployeeId};
use duduclaw_mcp_boundary::VerificationError;
use duduclaw_migration::contract::ProposalBinding;
use crate::isolation_lifecycle::{self as lifecycle, EmployeeLifecycle, LifecycleError, RootAllocation};

pub mod material;
mod store;
// Reached from `record_delegation_inner` on Linux only; tests run everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod personal_delegation;
pub use personal_delegation::{DelegationError,
    DECIDE_ELSEWHERE as PERSONAL_DELEGATION_DECIDE_ELSEWHERE, KIND as PERSONAL_DELEGATION_KIND};
pub(crate) use personal_delegation::shown_task as personal_delegation_shown_task;
#[cfg(test)]
pub(crate) use personal_delegation::test_support as personal_delegation_test_support;
#[cfg(test)]
mod lifecycle_tests;
/// Shown when background work is asked of a Personal employee. Such an
/// employee only acts for its owner in a live session (never a Background
/// principal), so no approval card is filed.
pub const PERSONAL_REFUSAL: &str =
    "個人專屬員工不能執行背景工作（排程、提醒、定期檢查或自動交辦）；請由擁有者直接在對話中交辦。";
/// Shown when a receipt approved before v1.81 (without the employee binding
/// and allocation) is found. It never authorizes a run.
pub const LEGACY_REFUSAL: &str =
    "這筆背景工作的執行授權是在目前版本之前核准的，需要由管理員到儀表板重新核准後才會執行。";
/// Shown when a task run (task/schedule) is asked of an employee that no
/// longer owns the task: it was reassigned to another employee or deleted.
/// Owner decision 2026-11-08: such a run is refused outright (no approval card,
/// no run without the task grant).
pub const FOREIGN_TASK_REFUSAL: &str =
    "這項工作已改派給其他員工或已被刪除，不會再以原本的員工執行；請到儀表板確認工作的負責員工。";
/// Machine-readable reason recorded with a refused foreign task run.
pub const FOREIGN_TASK_REASON: &str = "task_not_owned";
const REQUEST_TEXT: &str =
    "請於 24 小時內決定；核准後最多執行 30 天。內容、權限或交辦人憑證變更、取消工作後立即失效。";
const LEGACY_REQUEST_TEXT: &str = "這筆背景工作的執行授權是在目前版本之前核准的，需要重新核准。\
    請於 24 小時內決定；核准後最多執行 30 天。內容、權限或交辦人憑證變更、取消工作後立即失效。";
pub const APPROVAL_KIND: &str = "isolation_background_execution";
pub const APPROVAL_SPEC: crate::approval::operator_cli_gate::KindSpec = crate::approval::operator_cli_gate::KindSpec {
    kind: APPROVAL_KIND,
    validity: crate::approval::operator_cli_gate::Validity::Fixed(30 * 24 * 60),
    consume: crate::approval::operator_cli_gate::Consume::Never,
    reminders: false, max_pending_per_target: 1, max_pending_per_kind: 20,
    push_cap_per_hour: Some(2), legacy_scope_key: "agent_id",
    admin_refusal: "背景執行授權只能由管理員在儀表板核准。",
    expired_text: "這筆背景執行授權已逾期；請重新設定工作後送審。",
    channel_refusal: Some("請到儀表板確認交辦人與工作內容，再決定這筆背景執行授權。"),
    notice: |_, _, deadline| format!("有一筆背景工作需要確認交辦人與執行內容，請由管理員到儀表板審核。期限：{deadline}"),
    on_push_suppressed: |_, _, _| {},
};
const MAX_AUTHORITY_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Host-private receipt. No RPC or employee environment accepts this value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordAuthority {
    home: PathBuf,
    kind: String,
    id: String,
    agent: String,
    employee: EmployeeId,
    /// v1.81: the principal the employee was allocated for (A1 root receipt
    /// `principal_binding`, read through the A2 lifecycle). Always Shared:
    /// a Personal employee never runs background work. `None` only in a
    /// receipt stored before v1.81; such a receipt never authorizes a run.
    #[serde(default)]
    binding: Option<EmployeeBinding>,
    /// v1.81: the confirmed Employee proposal the root allocation was made
    /// for (A2 lifecycle `allocation`). A delete/recreate under the same
    /// label never matches. `None` only in a receipt stored before v1.81.
    #[serde(default)]
    allocation: Option<ProposalBinding>,
    owner: String,
    epoch: u64,
    acl_generation: u64,
    policy_epoch: u64,
    policy: String,
    material: String,
    approval: Option<String>,
    expires_at: u64,
    lane: Lane,
    caller: Option<EmployeeId>,
    parent_run: Option<RunHandle>,
    parent_agent: Option<String>,
    parent_policy: Option<String>,
    parent: Option<Box<RecordAuthority>>,
    detached_bus: bool,
    /// F6b (A3): the incarnation of `employee` at capture. Every recheck
    /// requires the live lifecycle's incarnation to equal it, so a receipt
    /// captured before a restore never authorizes the restored employee.
    /// Omitted when 0, so receipts (and the approval payloads that embed
    /// them) stored before F6 — when every employee was incarnation 0 —
    /// serialize byte-identically and stay valid.
    #[serde(default, skip_serializing_if = "is_zero")]
    incarnation: u32,
    /// F6b (A3): the incarnation of `caller` (the delegating source
    /// employee) at capture; compared with the source's live lifecycle.
    #[serde(default, skip_serializing_if = "is_zero")]
    caller_incarnation: u32,
}

fn is_zero(value: &u32) -> bool { *value == 0 }

impl RecordAuthority {
    /// Stored before v1.81: approved without the employee binding and
    /// allocation. Readable only so a new request can replace it.
    fn is_legacy(&self) -> bool { self.binding.is_none() && self.allocation.is_none() }
}

/// Why a background authority request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// A Personal employee: background runs are refused entirely.
    PersonalEmployee,
    Failed(VerificationError),
}
impl From<VerificationError> for RequestError {
    fn from(error: VerificationError) -> Self { Self::Failed(error) }
}
impl RequestError {
    /// User-facing reason, when one is more specific than a generic failure.
    pub fn user_message(&self) -> Option<&'static str> {
        match self { Self::PersonalEmployee => Some(PERSONAL_REFUSAL), Self::Failed(_) => None }
    }
}

tokio::task_local! {
    static RECORD: Mutex<Option<RecordAuthority>>;
    static PENDING: bool;
    static REFUSAL: Option<&'static str>;
}
static REQUEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unavailable() -> VerificationError { VerificationError::Unavailable }
fn now() -> Result<u64, VerificationError> { duduclaw_mcp_boundary::authority::now_seconds() }
fn user_id(owner: &str) -> Result<String, VerificationError> {
    duduclaw_core::HumanPrincipal::parse(owner).ok()
        .and_then(|p| p.user_id().map(str::to_owned)).ok_or(VerificationError::Invalid)
}
fn snapshot(home: &Path, owner: &str, agent: &str)
    -> Result<duduclaw_auth::IsolationAuthoritySnapshot, VerificationError> {
    let user = user_id(owner)?;
    crate::decision_notify::open_user_db(home).map_err(|_| unavailable())?
        .ok_or_else(unavailable)?.isolation_authority_snapshot(Some(&user), agent)
        .map_err(|_| VerificationError::Revoked)
}

/// File one bounded approval for the exact record and captured owner epoch.
/// Repeated ticks reuse the same receipt, including a denied/stale receipt;
/// they never refresh its credential epoch or silently renew authorization.
pub async fn request_record(home: &Path, kind: &str, id: &str, agent: &str,
    owner: Option<&str>, material: &Value) -> Result<(), VerificationError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Ok(()); }
    if kind == "delegation" { return Err(VerificationError::Invalid); }
    request_record_inner(home, kind, id, agent, owner, material, false).await.map_err(|error| match error {
        RequestError::PersonalEmployee => VerificationError::Invalid,
        RequestError::Failed(error) => error,
    })
}

/// Explicit authenticated settings saves may request a new approval after a
/// previous receipt expires or is revoked. Scheduler ticks cannot renew it.
pub async fn request_record_from_settings(home: &Path, kind: &str, id: &str, agent: &str,
    owner: &str, material: &Value) -> Result<(), RequestError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Ok(()); }
    if kind != "heartbeat" { return Err(VerificationError::Invalid.into()); }
    request_record_inner(home, kind, id, agent, Some(owner), material, true).await
}

/// Outcome of the blocking capture step.
enum Capture {
    /// The stored receipt stays; nothing is filed.
    Reuse,
    /// A Personal employee: refused before any approval card is filed.
    Personal,
    /// A new receipt awaiting an approval card. `legacy` when it replaces a
    /// receipt stored before v1.81.
    New { receipt: RecordAuthority, legacy: bool },
}

async fn request_record_inner(home: &Path, kind: &str, id: &str, agent: &str,
    owner: Option<&str>, material: &Value, renew: bool) -> Result<(), RequestError> {
    let owner = owner.ok_or(VerificationError::Invalid)?.to_owned();
    let digest = store::material_digest(material)?;
    let _lock = REQUEST_LOCK.try_lock().map_err(|_| unavailable())?;
    let (capture_home, capture_kind, capture_id, capture_agent) =
        (home.to_owned(), kind.to_owned(), id.to_owned(), agent.to_owned());
    let captured = duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let (home, kind, id, agent) = (&capture_home, capture_kind.as_str(), capture_id.as_str(), capture_agent.as_str());
        let root = &lifecycle::RootAllocator;
        // Up front, before any stored receipt is reused or a card is filed: a
        // Personal employee is never authorized for background work (B1).
        let Some((employee, binding, allocation, incarnation)) = capture_binding(home, agent, root)? else {
            return Ok(Capture::Personal);
        };
        let mut legacy = false;
        if let Some(existing) = store::load(home, kind, id)? {
            if existing.is_legacy() {
                // Approved before v1.81 without the binding/allocation, so its
                // approval never covered them and it can never authorize a run.
                // Treated like a missing receipt: one new card replaces it, and
                // later ticks reuse the replacement under the rules below.
                legacy = true;
            } else if existing.material == digest && existing.agent == agent && existing.owner == owner {
                if !renew { return Ok(Capture::Reuse); }
                // A still-pending card can already be stale after credential or
                // policy changes. Only an explicitly authenticated settings save
                // can replace it; polling never refreshes captured authority.
                if recheck_receipt(&existing, false, false, root).is_ok()
                    && (store::approval_pending(&existing)? || recheck(&existing).is_ok()) { return Ok(Capture::Reuse); }
            }
        }
        let current = snapshot(home, &owner, agent)?;
        check_tenant(&binding, &current.tenant_id)?;
        let epoch = current.human.as_ref().ok_or(VerificationError::Invalid)?.credential_epoch;
        let captured = RecordAuthority { home: home.to_owned(), kind: kind.into(), id: id.into(),
            agent: agent.into(), employee, binding: Some(binding), allocation: Some(allocation),
            owner, epoch, acl_generation: current.acl_generation,
            policy_epoch: current.policy_epoch,
            policy: crate::approval::strict_policy_revision(home, agent).map_err(|_| unavailable())?,
            material: digest, approval: None, expires_at: now()?.checked_add(MAX_AUTHORITY_SECONDS)
                .ok_or(VerificationError::InvalidTime)?, lane: Lane::Standard,
            caller: None, parent_run: None, parent_agent: None, parent_policy: None, parent: None, detached_bus: false,
            incarnation, caller_incarnation: 0 };
        store::validate_current(&captured)?;
        Ok(Capture::New { receipt: captured, legacy })
    }).await?;
    let (captured, legacy) = match captured {
        Capture::Reuse => return Ok(()),
        Capture::Personal => {
            tracing::warn!(agent, kind, "background work refused: personal employees never run background work");
            return Err(RequestError::PersonalEmployee);
        }
        Capture::New { receipt, legacy } => (receipt, legacy),
    };
    if legacy {
        tracing::warn!(agent, kind, "background receipt approved before v1.81 requires re-approval");
    }
    let broker = crate::approval::ApprovalBroker::open(home).map_err(|_| unavailable())?;
    // Cap pending cards independently of polling cadence and record churn.
    let pending = broker.list_pending(None).await.map_err(|_| unavailable())?;
    if pending.iter().filter(|r| r.action_kind == APPROVAL_KIND).count() >= 20 { return Err(unavailable().into()); }
    let payload = json!({"authority": captured, "record": material});
    let approval = broker.request(agent, APPROVAL_KIND,
        if legacy { LEGACY_REQUEST_TEXT } else { REQUEST_TEXT }, payload, 24 * 60 * 60)
        .await.map_err(|_| unavailable())?;
    let mut captured = captured;
    captured.approval = Some(approval.to_string());
    Ok(duduclaw_mcp_boundary::authority::verify_blocking(move || store::save(&captured)).await?)
}

/// Employee identity, binding and allocation for a new background receipt,
/// from the A2 lifecycle. `None` for a Personal employee: background runs
/// are refused entirely, so no receipt or approval card is ever created.
fn capture_binding(home: &Path, agent: &str, root: &dyn RootAllocation)
    -> Result<Option<(EmployeeId, EmployeeBinding, ProposalBinding, u32)>, VerificationError> {
    let current = current_lifecycle(home, agent, root)?;
    match &current.binding {
        EmployeeBinding::Shared(_) => Ok(Some((current.employee, current.binding.clone(), current.allocation,
            current.incarnation))),
        EmployeeBinding::Personal(_) => Ok(None),
    }
}

/// The owning scheduler reads the record again on each fire and supplies its
/// execution material. This scope crosses only the explicit future boundary.
pub async fn scope_record<F: Future>(home: &Path, kind: &str, id: &str, agent: &str,
    owner: Option<&str>, material: &Value, future: F) -> F::Output {
    let future = Box::pin(future);
    if !duduclaw_core::execution_gate::isolation_required() { return future.await; }
    if RECORD.try_with(|_| ()).is_ok() { return future.await; }
    // Owner decision 2026-11-08: a task run whose task this employee no longer
    // owns is refused outright, before any receipt is reused or a card filed.
    // An unreadable task store also refuses (fail closed), without a reason.
    if kind == "task" {
        let (check_home, check_id, check_agent) = (home.to_owned(), id.to_owned(), agent.to_owned());
        let precheck = duduclaw_mcp_boundary::authority::verify_blocking(move ||
            Ok(task_run_precheck(&check_home, &check_id, &check_agent))).await.unwrap_or(Err(None));
        if let Err(reason) = precheck {
            return REFUSAL.scope(reason, PENDING.scope(false, RECORD.scope(Mutex::new(None), future))).await;
        }
    }
    let mut refusal = None;
    if kind != "delegation" {
        refusal = request_record_inner(home, kind, id, agent, owner, material, false).await
            .err().and_then(|error| error.user_message());
    }
    let (home, kind, id, agent, owner, material) = (home.to_owned(), kind.to_owned(), id.to_owned(),
        agent.to_owned(), owner.map(str::to_owned), material.clone());
    let (captured, pending, legacy) = duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let (mut pending, mut legacy) = (false, false);
        let captured = (|| {
            let receipt = store::load(&home, &kind, &id)?.ok_or_else(unavailable)?;
            legacy = receipt.is_legacy();
            check_material(&receipt, &agent, owner.as_deref(), &store::material_digest(&material)?)?;
            pending = store::approval_pending(&receipt).unwrap_or(false);
            recheck(&receipt)?;
            Ok::<_, VerificationError>(receipt)
        })().ok();
        Ok((captured, pending, legacy))
    }).await.unwrap_or((None, false, false));
    // A receipt still in the pre-v1.81 form (its replacement could not be
    // filed) is refused by recheck; tell the scheduler why.
    if legacy && refusal.is_none() { refusal = Some(LEGACY_REFUSAL); }
    REFUSAL.scope(refusal, PENDING.scope(pending, RECORD.scope(Mutex::new(captured), future))).await
}

/// Live task ownership, the rule the H8 resolver applies on every `create_task`
/// call (`isolation_resources` `task_owned` for the task domain) and the H9
/// issuer applies at issuance: a `tasks.db` row with this id assigned to
/// `agent`, then the existing audience rule. The store must already exist and
/// pass the private-SQLite checks; it is never created here. Unreadable ⇒
/// `Unavailable`; a malformed id ⇒ `Invalid`.
pub(crate) fn task_owned_now(home: &Path, agent: &str, id: &str) -> Result<bool, VerificationError> {
    use rusqlite::{params, OptionalExtension};
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) { return Err(VerificationError::Invalid); }
    let db = crate::fs_safe::sqlite::open_existing(home, "tasks.db", crate::fs_safe::sqlite::Access::ReadOnly)
        .map_err(|_| unavailable())?;
    db.busy_timeout(std::time::Duration::from_millis(500)).map_err(|_| unavailable())?;
    let found = db.query_row("SELECT 1 FROM tasks WHERE id=?1 AND assigned_to=?2", params![id, agent], |_| Ok(()))
        .optional().map_err(|_| unavailable())?.is_some();
    if !found { return Ok(false); }
    let audience = crate::review_evidence::audience::task_audience(home, id);
    Ok(crate::review_evidence::audience::agent_may_read_task(agent, &[agent], &audience))
}

/// Record why a task run was refused: one security-audit row
/// (`isolation_task_run_refused`, reason [`FOREIGN_TASK_REASON`]) scoped to the
/// employee, naming the task and the stage that refused it (`schedule` before
/// any approval card, `issuance` in the capability issuer).
pub(crate) fn record_foreign_task(home: &Path, agent: &str, task: &str, stage: &str) {
    tracing::warn!(agent, task, stage, "protected task run refused: the task is no longer this employee's");
    duduclaw_security::audit::append_audit_event(home, &duduclaw_security::audit::AuditEvent::new(
        "isolation_task_run_refused", agent, duduclaw_security::audit::Severity::Warning,
        json!({"task": task, "reason": FOREIGN_TASK_REASON, "stage": stage})));
}

/// The `task` record pre-check of [`scope_record`]: `Ok` when the employee
/// owns the task now; `Err(Some(FOREIGN_TASK_REFUSAL))` (and an audit row) when
/// it was reassigned or deleted; `Err(None)` when ownership cannot be read.
pub(crate) fn task_run_precheck(home: &Path, id: &str, agent: &str) -> Result<(), Option<&'static str>> {
    match task_owned_now(home, agent, id) {
        Ok(true) => Ok(()),
        Ok(false) => {
            record_foreign_task(home, agent, id, "schedule");
            Err(Some(FOREIGN_TASK_REFUSAL))
        }
        Err(_) => Err(None),
    }
}

fn check_material(receipt: &RecordAuthority, agent: &str, owner: Option<&str>, digest: &str)
    -> Result<(), VerificationError> {
    if receipt.agent != agent || Some(receipt.owner.as_str()) != owner || receipt.material != digest {
        return Err(VerificationError::BindingMismatch);
    }
    Ok(())
}

fn check_identity(receipt: &RecordAuthority, epoch: Option<u64>, acl: u64, policy_epoch: u64, policy: &str)
    -> Result<(), VerificationError> {
    if epoch != Some(receipt.epoch) || acl != receipt.acl_generation
        || policy_epoch != receipt.policy_epoch || policy != receipt.policy {
        return Err(VerificationError::Revoked);
    }
    Ok(())
}

/// The A2 lifecycle of the confirmed employee holding `agent`. Disabled,
/// tombstoned, stale or denied employees are `Revoked`; unreadable stores or
/// an unreachable root allocator are `Unavailable`.
fn current_lifecycle(home: &Path, agent: &str, root: &dyn RootAllocation)
    -> Result<EmployeeLifecycle, VerificationError> {
    let current = lifecycle::read_lifecycle_with(home, agent, root).map_err(|error| match error {
        LifecycleError::Unavailable => unavailable(),
        LifecycleError::Denied | LifecycleError::Stale | LifecycleError::Tombstoned => VerificationError::Revoked,
    })?;
    if current.label != agent { return Err(VerificationError::Revoked); }
    Ok(current)
}

/// The receipt's employee, binding and allocation must equal the live
/// lifecycle exactly, and the binding must still be Shared.
fn check_lifecycle(receipt: &RecordAuthority, current: &EmployeeLifecycle) -> Result<(), VerificationError> {
    let (Some(binding), Some(allocation)) = (&receipt.binding, &receipt.allocation) else {
        return Err(VerificationError::Revoked);
    };
    // F6b (A3): a receipt of a prior incarnation never matches.
    if current.employee != receipt.employee || current.label != receipt.agent
        || current.allocation != *allocation || current.binding != *binding
        || current.incarnation != receipt.incarnation {
        return Err(VerificationError::Revoked);
    }
    // B1: a Personal employee is never authorized for background work.
    if !matches!(current.binding, EmployeeBinding::Shared(_)) { return Err(VerificationError::Revoked); }
    Ok(())
}

/// Legacy receipts are refused before any store is read; otherwise the
/// lifecycle is re-read and compared.
fn check_receipt_lifecycle(receipt: &RecordAuthority, root: &dyn RootAllocation) -> Result<(), VerificationError> {
    if receipt.is_legacy() { return Err(VerificationError::Revoked); }
    check_lifecycle(receipt, &current_lifecycle(&receipt.home, &receipt.agent, root)?)
}

/// A Shared binding's tenant must equal the installation tenant read now.
fn check_tenant(binding: &EmployeeBinding, tenant: &str) -> Result<(), VerificationError> {
    match binding {
        EmployeeBinding::Shared(pinned) if pinned.as_str() == tenant => Ok(()),
        _ => Err(VerificationError::Revoked),
    }
}

fn take_record() -> Result<RecordAuthority, VerificationError> {
    RECORD.try_with(|slot| slot.lock().ok().and_then(|mut s| s.take()))
        .ok().flatten().ok_or_else(unavailable)
}

/// Used only to recover the owner of a previously submitted heartbeat approval.
/// The returned label still requires the full record/approval/epoch recheck.
pub fn record_owner(home: &Path, kind: &str, id: &str) -> Option<String> {
    store::load(home, kind, id).ok().flatten().map(|r| r.owner)
}

pub(crate) fn recheck(receipt: &RecordAuthority) -> Result<(), VerificationError> {
    recheck_receipt(receipt, false, true, &lifecycle::RootAllocator)
}
fn recheck_receipt(receipt: &RecordAuthority, ancestor: bool, check_decision: bool, root: &dyn RootAllocation)
    -> Result<(), VerificationError> {
    store::validate(receipt)?;
    // Approved before v1.81: needs a new approval, whatever else is current.
    if receipt.is_legacy() { return Err(VerificationError::Revoked); }
    if now()? >= receipt.expires_at { return Err(VerificationError::InvalidTime); }
    check_receipt_lifecycle(receipt, root)?;
    if store::load(&receipt.home, &receipt.kind, &receipt.id)?.as_ref() != Some(receipt) {
        return Err(VerificationError::Revoked);
    }
    if !receipt.detached_bus {
        let material = if ancestor { material::read_ancestor(&receipt.home, &receipt.kind, &receipt.id) }
            else { material::read_current(&receipt.home, &receipt.kind, &receipt.id) }
            .map_err(|_| VerificationError::Revoked)?;
        if store::material_digest(&material)? != receipt.material { return Err(VerificationError::Revoked); }
    }
    let current = snapshot(&receipt.home, &receipt.owner, &receipt.agent)?;
    check_tenant(receipt.binding.as_ref().ok_or(VerificationError::Revoked)?, &current.tenant_id)?;
    let policy = crate::approval::strict_policy_revision(&receipt.home, &receipt.agent).map_err(|_| unavailable())?;
    check_identity(receipt, current.human.as_ref().map(|h| h.credential_epoch),
        current.acl_generation, current.policy_epoch, &policy)?;
    if let Some(approval) = &receipt.approval { if check_decision { store::check_approval(receipt, approval)?; } }
    else if receipt.kind != "delegation" { return Err(VerificationError::Invalid); }
    if let Some(parent) = &receipt.parent { recheck_receipt(parent, true, true, root)?; }
    if let (Some(agent), Some(policy)) = (&receipt.parent_agent, &receipt.parent_policy) {
        // F6b (A3): the delegating source must still be the same employee id
        // and the same incarnation it was when the delegation was captured.
        let caller = current_lifecycle(&receipt.home, agent, root)?;
        if Some(caller.employee) != receipt.caller || caller.incarnation != receipt.caller_incarnation {
            return Err(VerificationError::Revoked);
        }
        let origin = snapshot(&receipt.home, &receipt.owner, agent)?;
        if origin.human.as_ref().map(|h| h.credential_epoch) != Some(receipt.epoch)
            || origin.acl_generation != receipt.acl_generation || origin.policy_epoch != receipt.policy_epoch
            || crate::approval::strict_policy_revision(&receipt.home, agent).ok().as_deref() != Some(policy) {
            return Err(VerificationError::Revoked);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) async fn authorize_current(agent_dir: &Path, principal_context: Option<String>)
    -> Result<crate::isolation_mcp::RuntimeAuthorization, VerificationError> {
    let receipt = take_record()?;
    if agent_dir != receipt.home.join("agents").join(&receipt.agent)
        || crate::on_behalf_of::current().as_deref() != Some(&receipt.owner) {
        return Err(VerificationError::BindingMismatch);
    }
    let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone).ok().flatten()
        .unwrap_or_else(|| format!("background:{}", uuid::Uuid::new_v4()));
    let session = duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(Clone::clone).ok().flatten();
    let egress_turn = crate::runtime_turn::current();
    let metadata = crate::isolation_egress::RunMetadata {
        dispatch_run: crate::memory_provenance::DISPATCH_RUN.try_with(Clone::clone).ok().flatten(),
        user_message: crate::memory_provenance::TURN_USER_MESSAGE.try_with(Clone::clone).ok().flatten(),
        reply_channel: crate::claude_runner::REPLY_CHANNEL.try_with(Clone::clone).ok(),
        reply_sender: crate::claude_runner::REPLY_SENDER.try_with(Clone::clone).ok(),
        upstream_unknown: crate::memory_provenance::UPSTREAM_UNKNOWN.try_with(|v| *v).unwrap_or(false),
        ..delegation_metadata(&receipt)?
    };
    let task = crate::runtime::round_task_env().map(|(_, task)| task)
        .or_else(|| (receipt.kind == "task").then(|| receipt.id.clone()));
    let lane = if crate::explore_lane::in_explore() || receipt.lane == Lane::Explore { Lane::Explore } else { Lane::Standard };
    duduclaw_mcp_boundary::authority::verify_blocking(move || {
        recheck(&receipt)?;
        // H10: the issuer limits the run to `task` and adds the record Task
        // grant only after its H9 selector read live task ownership; a task
        // the employee no longer owns refuses the run outright (Revoked,
        // recorded as `isolation_task_run_refused`, stage `issuance`).
        let mut auth = crate::isolation_mcp::record_authorization(receipt.home.clone(), receipt.agent.clone(),
            user_id(&receipt.owner)?, receipt.epoch, lane,
            crate::isolation_mcp::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
                turn, approval_session: session }, metadata: Some(metadata), principal_context,
                egress_turn, background: Some(receipt.clone()) }, Some(receipt.expires_at), task)?;
        // Queue consumers are detached host waiters. The initiating employee
        // and run remain in the receipt; treating them as a synchronous waiter
        // would deadlock/refuse a valid self-delegation after its parent exits.
        auth.origin = WaitOrigin::Gateway;
        auth.caller = None;
        Ok(auth)
    }).await
}

#[cfg(target_os = "linux")]
fn delegation_metadata(receipt: &RecordAuthority) -> Result<crate::isolation_egress::RunMetadata, VerificationError> {
    if receipt.kind != "delegation" { return Ok(crate::isolation_egress::RunMetadata::root()); }
    crate::claude_runner::DELEGATION_ENV.try_with(|env| {
        let number = |key| env.get(key).and_then(|v| v.parse::<u8>().ok()).ok_or(VerificationError::Invalid);
        Ok(crate::isolation_egress::RunMetadata {
            delegation_depth: number(duduclaw_core::ENV_DELEGATION_DEPTH)?,
            hop_depth: env.get(duduclaw_core::ENV_HOP_DEPTH).and_then(|v| v.parse().ok()).unwrap_or(0),
            delegation_origin: env.get(duduclaw_core::ENV_DELEGATION_ORIGIN).cloned(),
            ..crate::isolation_egress::RunMetadata::root()
        })
    }).map_err(|_| unavailable())?
}

pub(crate) fn has_scope() -> bool { RECORD.try_with(|_| ()).is_ok() }
pub fn approval_pending() -> bool { PENDING.try_with(|pending| *pending).unwrap_or(false) }
/// User-facing reason the current background scope holds no authority, when
/// one is known (Personal employee, receipt approved before v1.81). Only an
/// explanation for the owning scheduler; its absence is never a grant.
pub fn refusal_reason() -> Option<&'static str> { REFUSAL.try_with(|reason| *reason).ok().flatten() }

#[cfg(target_os = "linux")]
pub(crate) struct DelegationSource {
    pub owner: String, pub epoch: u64, pub acl_generation: u64, pub policy_epoch: u64,
    pub policy: String, pub agent: String, pub employee: EmployeeId, pub run: RunHandle,
    pub expires_at: u64, pub lane: Lane, pub parent: Option<RecordAuthority>,
    /// The source employee's lifecycle principal pinned for this run (SR2:
    /// a Personal source needs its owner's approval card).
    pub binding: EmployeeBinding,
    /// F6b (A3/A4): the source employee's incarnation pinned for this run.
    pub incarnation: u32,
}

/// Called by the original product send handler after its delegation/action
/// gates, before inserting the exact queue row. Authority is read back from D's
/// live run association, never from queue fields or a freshly read user epoch.
/// A Personal source (owner decision 2026-11-08) is not queued until its
/// owner approves the card: `Err(OwnerApprovalPending)` until then.
pub async fn record_delegation(home: &Path, sender: &str,
    context: &duduclaw_mcp_boundary::RequestContext, target: &str, id: &str, material: &Value)
    -> Result<(), DelegationError> {
    record_delegation_inner(home, sender, context, target, id, material, false).await
}

/// The bus consumer removes its input under a file lock. Its original material
/// is therefore sealed by the immutable host receipt at enqueue, and compared
/// again by scope_record at dequeue. Cancellation remains tied to the original
/// turn and parent approval; a drained pathname is never an authority source.
pub async fn record_bus_delegation(home: &Path, sender: &str,
    context: &duduclaw_mcp_boundary::RequestContext, target: &str, id: &str, material: &Value)
    -> Result<(), DelegationError> {
    record_delegation_inner(home, sender, context, target, id, material, true).await
}

async fn record_delegation_inner(home: &Path, sender: &str,
    context: &duduclaw_mcp_boundary::RequestContext, target: &str, id: &str, material: &Value, detached_bus: bool)
    -> Result<(), DelegationError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Ok(()); }
    #[cfg(target_os = "linux")]
    {
        let outer_home = home.to_owned();
        let (home, sender, context, target, id, material) =
            (home.to_owned(), sender.to_owned(), context.clone(), target.to_owned(), id.to_owned(), material.clone());
        let held = duduclaw_mcp_boundary::authority::verify_blocking(move || {
            let source = crate::isolation_mcp::delegation_source(&home, &sender, &context)?;
            let current = snapshot(&home, &source.owner, &target)?;
            // A queued delegation runs as background work of the target: a
            // Personal target is refused before any receipt is stored.
            let Some((employee, binding, allocation, incarnation)) = capture_binding(&home, &target, &lifecycle::RootAllocator)? else {
                tracing::warn!(agent = target.as_str(), "delegation refused: personal employees never run background work");
                return Err(VerificationError::Invalid);
            };
            check_tenant(&binding, &current.tenant_id)?;
            if current.human.as_ref().map(|h| h.credential_epoch) != Some(source.epoch)
                || current.acl_generation != source.acl_generation || current.policy_epoch != source.policy_epoch {
                return Err(VerificationError::Revoked);
            }
            let policy = crate::approval::strict_policy_revision(&home, &target).map_err(|_| unavailable())?;
            // SR2: Personal source → owner approval card first; Shared → None.
            let gate = personal_delegation::gate(&source.binding, &user_id(&source.owner)?, &source.agent,
                source.employee, &target, employee, &material)?
                .map(|gate| gate.with_source_incarnation(source.incarnation));
            let value = RecordAuthority { home, kind: "delegation".into(), id, agent: target.clone(), employee,
                binding: Some(binding), allocation: Some(allocation), owner: source.owner, epoch: source.epoch, acl_generation: source.acl_generation,
                policy_epoch: source.policy_epoch,
                policy,
                material: store::material_digest(&material)?, approval: None, expires_at: source.expires_at,
                lane: source.lane, caller: Some(source.employee), parent_run: Some(source.run),
                parent_agent: Some(source.agent), parent_policy: Some(source.policy), parent: source.parent.map(Box::new), detached_bus,
                incarnation, caller_incarnation: source.incarnation };
            if let Some(gate) = gate { return Ok(Some((value, gate))); }
            if store::load(&value.home, "delegation", &value.id)?.is_some() { return Err(VerificationError::Revoked); }
            store::save(&value).map(|()| None)
        }).await?;
        return match held {
            None => Ok(()),
            Some((value, gate)) => personal_delegation::admit(&outer_home, &gate, &value).await,
        };
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = (home, sender, context, target, id, material, detached_bus); Err(unavailable().into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn allocation(id: u8) -> ProposalBinding {
        ProposalBinding { operation_id: duduclaw_migration::contract::ProposalOperationId {
            proposal_id: duduclaw_migration::ids::RequestId([id; 16]), revision: 1 },
            proposal_digest: duduclaw_isolation::ids::Sha256Digest([id; 32]) }
    }
    pub(super) fn receipt(home: &Path) -> RecordAuthority {
        RecordAuthority { home: home.to_owned(), kind: "cron".into(), id: "original-record".into(),
            agent: "alice".into(), employee: EmployeeId([1;16]),
            binding: Some(EmployeeBinding::Shared(duduclaw_isolation::bounded::Text::new("tenant-1".into()).unwrap())),
            allocation: Some(allocation(1)), owner: "user:original-user".into(), epoch: 7,
            acl_generation: 11, policy_epoch: 13, policy: "original-policy".into(),
            material: store::material_digest(&json!({"task":"original"})).unwrap(),
            approval: Some("approval-id".into()), expires_at: now().unwrap() + 3600,
            lane: Lane::Standard, caller: None, parent_run: None, parent_agent: None,
            parent_policy: None, parent: None, detached_bus: false, incarnation: 0, caller_incarnation: 0 }
    }

    #[test]
    fn background_authority_binds_owner_record_and_original_epochs() {
        let receipt = receipt(Path::new("/trusted-home"));
        assert!(check_material(&receipt, "alice", Some("user:original-user"), &receipt.material).is_ok());
        for (agent, owner, digest) in [("bob", Some("user:original-user"), receipt.material.as_str()),
            ("alice", None, receipt.material.as_str()), ("alice", Some("user:other"), receipt.material.as_str()),
            ("alice", Some("user:original-user"), "modified")] {
            assert!(check_material(&receipt, agent, owner, digest).is_err());
        }
        assert!(check_identity(&receipt, Some(7), 11, 13, "original-policy").is_ok());
        for (epoch, acl, policy_epoch, policy) in [(None,11,13,"original-policy"),
            (Some(8),11,13,"original-policy"), (Some(7),12,13,"original-policy"),
            (Some(7),11,14,"original-policy"), (Some(7),11,13,"replacement-policy")] {
            assert_eq!(check_identity(&receipt, epoch, acl, policy_epoch, policy), Err(VerificationError::Revoked));
        }
        for owner in ["", "dashboard", "unknown:owner", "unmapped:telegram:123"] { assert!(user_id(owner).is_err()); }
    }

    #[tokio::test]
    async fn background_authority_is_one_use_and_does_not_cross_spawn() {
        assert!(take_record().is_err());
        RECORD.scope(Mutex::new(Some(receipt(Path::new("/trusted-home")))), async {
            assert!(tokio::spawn(async { take_record().is_err() }).await.unwrap());
            assert_eq!(take_record().unwrap().epoch, 7);
            assert!(take_record().is_err());
        }).await;
        assert!(take_record().is_err());
    }

    #[tokio::test]
    async fn background_pending_signal_is_scoped_and_not_a_grant() {
        assert!(!approval_pending());
        PENDING.scope(true, RECORD.scope(Mutex::new(None), async {
            assert!(approval_pending());
            assert!(take_record().is_err());
            assert!(!tokio::spawn(async { approval_pending() }).await.unwrap());
        })).await;
        assert!(!approval_pending());
    }
}
