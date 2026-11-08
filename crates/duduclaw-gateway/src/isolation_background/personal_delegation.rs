//! SR2 finding 2, owner decision 2026-11-08: a delegation from a Personal
//! employee's live run to a Shared employee is not queued until the Personal
//! employee's OWNER approves an approval card (existing `ApprovalBroker`,
//! decided on the dashboard by that owner: `me.isolation.decide_personal_delegation`).
//!
//! - The card binds the source employee, the target employee, the FULL task
//!   text and the stable delivery fields of the delegation (SR2 round 3,
//!   N1/N2). The owner-only list RPC returns exactly that bound task text
//!   ([`shown_task`]); nothing the owner reads is truncated or masked
//!   relative to what is approved. A task above [`MAX_TASK_CHARS`], or one
//!   that credential masking or invisible formatting would change on screen,
//!   is refused before any card is filed ([`DelegationError::TaskTooLong`],
//!   [`DelegationError::TaskNotShowable`]).
//! - The generic card summary names only the two employees (no task text),
//!   and the generic approval centre never lists, returns or pushes this kind
//!   (`approval_visibility`: owner-only reader, N4).
//! - The approval is bound to the owner and is single-use: it is consumed in
//!   the same receipt-store transaction that stores the delegation receipt,
//!   so a replay (same or another delegation id) is refused. The resend the
//!   employee makes after the approval must repeat the bound fields exactly
//!   (target, task, reply channel, lane, on-behalf-of principal, spawned);
//!   only its turn/session ids may differ.
//! - Only `decided_by = "dashboard:<owner user id>"` counts; an Admin, a
//!   channel press or any other decider never authorizes the send.
//! - Denial or expiry (TTL = deny) means the delegation is not sent.
//! - Open cards are capped per source employee, per owner and in total (N3).
//!
//! Shared → anything is unchanged: [`gate`] returns `None` for a Shared source.
use std::path::Path;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_mcp_boundary::VerificationError;
use crate::approval::{ApprovalBroker, ApprovalRecord, ApprovalStatus};
use super::{store, RecordAuthority};

/// Approval kind of the owner's card.
pub const KIND: &str = "isolation_personal_delegation";
/// A card the owner did not decide within a day is a denial.
const TTL_SECONDS: i64 = 24 * 60 * 60;
/// Open cards of this kind across the installation (backstop).
pub(crate) const MAX_PENDING: usize = 100;
/// Open cards one source (Personal) employee may have at a time.
pub(crate) const MAX_PENDING_PER_SOURCE: usize = 5;
/// Open cards one owner may have to decide at a time (all their employees).
pub(crate) const MAX_PENDING_PER_OWNER: usize = 10;
/// The longest task text a card carries; the owner is shown all of it.
pub const MAX_TASK_CHARS: usize = 4000;

pub const PENDING_TEXT: &str = "這次交辦需要個人專屬員工的擁有者核准：已送出核准卡片。擁有者在自己的帳號頁核准後，請再交辦一次相同的工作（核准只能使用一次）。";
pub const DENIED_TEXT: &str = "個人專屬員工的擁有者拒絕了這次交辦，工作沒有送出。";
pub const TOO_LONG_TEXT: &str = "這次交辦的工作內容超過 4000 字，擁有者無法在核准卡片上看到完整內容，所以沒有送出核准卡片。請把工作內容縮短到 4000 字以內，或分成幾次交辦。";
pub const NOT_SHOWABLE_TEXT: &str = "這次交辦的工作內容含有疑似密碼、金鑰等憑證，或含有看不見的控制字元，擁有者在核准卡片上看到的內容會和實際送出的不同，所以沒有送出核准卡片。請移除這些內容後再交辦。";
pub const TOO_MANY_TEXT: &str = "目前已有太多交辦正在等待個人專屬員工的擁有者核准，這次沒有送出新的核准卡片。請等擁有者處理（核准、拒絕或逾期）後再交辦。";
/// `approvals.decide` refuses this kind for everyone: only the owner decides,
/// on the own account page.
pub const DECIDE_ELSEWHERE: &str = "這張交辦核准卡片只能由個人專屬員工的擁有者在自己的帳號頁決定。";

/// Serializes card filing and consumption for this kind in this gateway.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Why a Personal → Shared delegation was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationError {
    /// An owner card is waiting (just filed, or already pending).
    OwnerApprovalPending,
    /// The owner denied this exact delegation.
    OwnerDenied,
    /// The task text is longer than [`MAX_TASK_CHARS`]; no card is filed.
    TaskTooLong,
    /// Masking or invisible formatting would change the text the owner is
    /// shown relative to the bound text; no card is filed.
    TaskNotShowable,
    /// Too many open cards for this source employee, owner or installation.
    TooManyPending,
    Failed(VerificationError),
}
impl From<VerificationError> for DelegationError {
    fn from(error: VerificationError) -> Self { Self::Failed(error) }
}
impl DelegationError {
    /// User-facing reason, when one is more specific than a generic failure.
    pub fn user_message(&self) -> Option<&'static str> {
        match self {
            Self::OwnerApprovalPending => Some(PENDING_TEXT),
            Self::OwnerDenied => Some(DENIED_TEXT),
            Self::TaskTooLong => Some(TOO_LONG_TEXT),
            Self::TaskNotShowable => Some(NOT_SHOWABLE_TEXT),
            Self::TooManyPending => Some(TOO_MANY_TEXT),
            Self::Failed(_) => None,
        }
    }
}

/// One Personal → Shared delegation awaiting (or holding) its owner approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gate {
    pub(crate) owner: String,
    pub(crate) source_agent: String,
    pub(crate) source_employee: EmployeeId,
    pub(crate) target_agent: String,
    pub(crate) target_employee: EmployeeId,
    pub(crate) task_digest: String,
    task: String,
    /// The stable delivery fields of the delegation besides the task text
    /// (`material::delegation_resend_binding` without `payload`).
    delivery: Value,
}

/// Digest of the exact task text the owner approves.
pub fn task_digest(task: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-personal-delegation-task-v1\0");
    hash.update(task.as_bytes());
    hex::encode(hash.finalize())
}

/// The task text is shown to the owner exactly as bound: within
/// [`MAX_TASK_CHARS`], unchanged by the credential masking every approval
/// surface applies, and free of control or bidirectional-formatting
/// characters that would make the rendered text differ from the bytes.
fn showable(task: &str) -> Result<(), DelegationError> {
    if task.chars().count() > MAX_TASK_CHARS { return Err(DelegationError::TaskTooLong); }
    // Zero-width joiners (U+200C/U+200D) stay allowed: scripts and emoji use them.
    let invisible = |c: char| (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        || matches!(c, '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}' | '\u{feff}');
    if task.chars().any(invisible) || duduclaw_security::credential_detect::mask(task) != task {
        return Err(DelegationError::TaskNotShowable);
    }
    Ok(())
}

/// The exact task text bound by `card`, for the owner's list. `None` unless
/// the card is this kind, its text is showable and matches its digest; such
/// a card is neither shown nor decidable.
pub(crate) fn shown_task(card: &ApprovalRecord) -> Option<&str> {
    let task = card.payload.get("task")?.as_str()?;
    (card.action_kind == KIND && showable(task).is_ok()
        && card.payload.get("task_digest").and_then(Value::as_str) == Some(task_digest(task).as_str()))
        .then_some(task)
}

/// `None` for a Shared source (unchanged path). For a Personal source the
/// live run's Human principal must be the owner (B1); the task text is the
/// delegation material's `payload`, and the bound delivery fields are the
/// stable resend subset of the material (never its turn/session ids).
pub(crate) fn gate(source_binding: &EmployeeBinding, principal_user: &str, source_agent: &str,
    source_employee: EmployeeId, target_agent: &str, target_employee: EmployeeId, material: &Value)
    -> Result<Option<Gate>, VerificationError> {
    let EmployeeBinding::Personal(owner) = source_binding else { return Ok(None) };
    if owner.as_str() != principal_user || principal_user.is_empty() { return Err(VerificationError::Revoked); }
    let task = material.get("payload").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
    let mut delivery = super::material::delegation_resend_binding(material).ok_or(VerificationError::Invalid)?;
    if delivery.get("target").and_then(Value::as_str) != Some(target_agent) { return Err(VerificationError::BindingMismatch); }
    delivery.as_object_mut().ok_or(VerificationError::Invalid)?.remove("payload");
    Ok(Some(Gate { owner: owner.as_str().to_owned(), source_agent: source_agent.to_owned(), source_employee,
        target_agent: target_agent.to_owned(), target_employee, task_digest: task_digest(task), task: task.to_owned(),
        delivery }))
}

impl Gate {
    /// The exact binding of the card; compared whole on consumption. It
    /// carries the full task text: what the owner is shown is what is bound.
    pub(crate) fn payload(&self) -> Value {
        json!({
            "owner_user_id": self.owner,
            "source_agent": self.source_agent,
            "source_employee": hex::encode(self.source_employee.0),
            "target_agent": self.target_agent,
            "target_employee": hex::encode(self.target_employee.0),
            "task": self.task,
            "task_digest": self.task_digest,
            "delivery": self.delivery,
        })
    }

    /// The generic summary: no task text (the owner reads it through the
    /// owner-only list, `me.isolation.list_personal_delegations`).
    fn card_text(&self) -> String {
        format!("個人專屬員工「{}」想把一項工作交辦給共用員工「{}」。工作內容只會顯示給擁有者；\
            只有擁有者可以在自己的帳號頁決定。核准後只會送出這一次，拒絕或逾期都不會送出。",
            self.source_agent, self.target_agent)
    }
}

fn binds(record: &ApprovalRecord, gate: &Gate) -> bool {
    record.action_kind == KIND && record.agent_id == gate.source_agent && record.payload == gate.payload()
}

/// The card is inside its TTL (created_at + ttl > now).
fn live(record: &ApprovalRecord, now: i64) -> bool {
    record.expires_at_epoch().is_some_and(|expires| now < expires)
}

/// Decided by exactly the owner on the dashboard, inside the card's window.
fn decided_by_owner(record: &ApprovalRecord, gate: &Gate, now: i64) -> bool {
    let parse = |value: &str| chrono::DateTime::parse_from_rfc3339(value).ok().map(|t| t.timestamp());
    let (Some(created), Some(decided), Some(expires)) = (parse(&record.created_at),
        record.decided_at.as_deref().and_then(parse), record.expires_at_epoch()) else { return false };
    record.decided_by.as_deref() == Some(format!("dashboard:{}", gate.owner).as_str())
        && record.invalidated_reason.is_none()
        && created <= decided && decided <= now && decided < expires
}

/// Store `value` (the delegation receipt) only by consuming one owner
/// approval for exactly `gate`; otherwise file (or keep) the owner's card and
/// refuse. Nothing is queued unless this returns `Ok`.
pub(crate) async fn admit(home: &Path, gate: &Gate, value: &RecordAuthority) -> Result<(), DelegationError> {
    if value.kind != "delegation" || value.home != home || value.agent != gate.target_agent
        || value.employee != gate.target_employee || value.caller != Some(gate.source_employee) {
        return Err(VerificationError::BindingMismatch.into());
    }
    // N1: a task the owner cannot be shown exactly is never filed or sent.
    showable(&gate.task)?;
    let _lock = LOCK.lock().await;
    {
        let (home, id) = (home.to_owned(), value.id.clone());
        // A delegation id is stored once; a replayed id never consumes an approval.
        if duduclaw_mcp_boundary::authority::verify_blocking(move || store::load(&home, "delegation", &id)).await?.is_some() {
            return Err(VerificationError::Revoked.into());
        }
    }
    let broker = ApprovalBroker::open(home).map_err(|_| VerificationError::Unavailable)?;
    let cards = broker.list_by_kind(KIND).await.map_err(|_| VerificationError::Unavailable)?;
    let now = chrono::Utc::now().timestamp();
    let mine: Vec<&ApprovalRecord> = cards.iter().filter(|card| binds(card, gate)).collect();
    for card in mine.iter().filter(|card| card.status == ApprovalStatus::Approved
        && live(card, now) && decided_by_owner(card, gate, now)) {
        let (receipt, approval) = (value.clone(), card.id.to_string());
        match duduclaw_mcp_boundary::authority::verify_blocking(move || store::save_consuming(&receipt, &approval)).await {
            Ok(()) => {
                tracing::info!(source_agent = gate.source_agent.as_str(), target_agent = gate.target_agent.as_str(),
                    approval = card.id.as_str(), "personal delegation queued after owner approval");
                return Ok(());
            }
            // Already consumed: single use. Try another live approval, if any.
            Err(VerificationError::Revoked) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    if mine.iter().any(|card| card.status == ApprovalStatus::Pending && live(card, now)) {
        return Err(DelegationError::OwnerApprovalPending);
    }
    if mine.iter().any(|card| card.status == ApprovalStatus::Denied && live(card, now)
        && decided_by_owner(card, gate, now)) {
        return Err(DelegationError::OwnerDenied);
    }
    // N3: per source employee, per owner, then the installation backstop.
    let pending: Vec<&ApprovalRecord> = cards.iter()
        .filter(|card| card.status == ApprovalStatus::Pending && live(card, now)).collect();
    fn field<'a>(card: &'a ApprovalRecord, name: &str) -> Option<&'a str> {
        card.payload.get(name).and_then(Value::as_str)
    }
    let source = hex::encode(gate.source_employee.0);
    let per_source = pending.iter().filter(|card| field(card, "source_employee") == Some(source.as_str())).count();
    let per_owner = pending.iter().filter(|card| field(card, "owner_user_id") == Some(gate.owner.as_str())).count();
    if per_source >= MAX_PENDING_PER_SOURCE || per_owner >= MAX_PENDING_PER_OWNER || pending.len() >= MAX_PENDING {
        tracing::warn!(source_agent = gate.source_agent.as_str(), per_source, per_owner, total = pending.len(),
            "personal delegation card refused: too many open cards");
        return Err(DelegationError::TooManyPending);
    }
    broker.request(&gate.source_agent, KIND, &gate.card_text(), gate.payload(), TTL_SECONDS).await
        .map_err(|_| VerificationError::Unavailable)?;
    tracing::info!(source_agent = gate.source_agent.as_str(), target_agent = gate.target_agent.as_str(),
        "personal delegation held for owner approval");
    Err(DelegationError::OwnerApprovalPending)
}

/// Files an owner card through the real [`gate`] and [`admit`] path, for
/// the handler tests (owner list, generic approval visibility).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    pub(crate) use super::{task_digest, MAX_TASK_CHARS};

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn file_card(home: &Path, owner: &str, source_agent: &str, source_employee: EmployeeId,
        target_agent: &str, target_employee: EmployeeId, task: &str) -> Result<(), DelegationError> {
        let binding = EmployeeBinding::Personal(duduclaw_isolation::bounded::Text::new(owner.to_owned())
            .map_err(|_| VerificationError::Invalid)?);
        let principal = format!("user:{owner}");
        let material = super::super::material::delegation(source_agent, target_agent, task, Some("turn"),
            Some("session"), None, 1, None, Some("dashboard:chat"), Some(source_agent), false, 0, false, Some(&principal));
        let gate = gate(&binding, owner, source_agent, source_employee, target_agent, target_employee, &material)?
            .ok_or(VerificationError::Invalid)?;
        let mut value = super::super::tests::receipt(home);
        value.kind = "delegation".into();
        value.id = format!("test-{}", uuid::Uuid::new_v4());
        value.agent = target_agent.into();
        value.employee = target_employee;
        value.approval = None;
        value.caller = Some(source_employee);
        admit(home, &gate, &value).await
    }
}

#[cfg(test)]
#[path = "personal_delegation_tests.rs"]
mod tests;
