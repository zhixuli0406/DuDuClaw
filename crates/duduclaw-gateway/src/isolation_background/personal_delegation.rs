//! SR2 finding 2, owner decision 2026-11-08: a delegation from a Personal
//! employee's live run to a Shared employee is not queued until the Personal
//! employee's OWNER approves an approval card (existing `ApprovalBroker`,
//! decided on the dashboard by that owner: `me.isolation.decide_personal_delegation`).
//!
//! - The card names the source employee, the target employee and the task
//!   text (truncated with `duduclaw_core::truncate_chars`).
//! - The approval is bound to (source employee id, target employee id, task
//!   digest) plus the owner, and is single-use: it is consumed in the same
//!   receipt-store transaction that stores the delegation receipt, so a
//!   replay (same or another delegation id) is refused.
//! - Only `decided_by = "dashboard:<owner user id>"` counts; an Admin, a
//!   channel press or any other decider never authorizes the send.
//! - Denial or expiry (TTL = deny) means the delegation is not sent.
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
/// Open cards of this kind across the installation.
const MAX_PENDING: usize = 20;
const MAX_TASK_CHARS: usize = 200;

pub const PENDING_TEXT: &str = "這次交辦需要個人專屬員工的擁有者核准：已送出核准卡片。擁有者在自己的帳號頁核准後，請再交辦一次相同的工作（核准只能使用一次）。";
pub const DENIED_TEXT: &str = "個人專屬員工的擁有者拒絕了這次交辦，工作沒有送出。";
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
}

/// Digest of the exact task text the owner approves.
pub fn task_digest(task: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-personal-delegation-task-v1\0");
    hash.update(task.as_bytes());
    hex::encode(hash.finalize())
}

/// `None` for a Shared source (unchanged path). For a Personal source the
/// live run's Human principal must be the owner (B1); the task text is the
/// delegation material's `payload`.
pub(crate) fn gate(source_binding: &EmployeeBinding, principal_user: &str, source_agent: &str,
    source_employee: EmployeeId, target_agent: &str, target_employee: EmployeeId, material: &Value)
    -> Result<Option<Gate>, VerificationError> {
    let EmployeeBinding::Personal(owner) = source_binding else { return Ok(None) };
    if owner.as_str() != principal_user || principal_user.is_empty() { return Err(VerificationError::Revoked); }
    let task = material.get("payload").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
    Ok(Some(Gate { owner: owner.as_str().to_owned(), source_agent: source_agent.to_owned(), source_employee,
        target_agent: target_agent.to_owned(), target_employee, task_digest: task_digest(task), task: task.to_owned() }))
}

impl Gate {
    /// The exact binding of the card; compared whole on consumption. The
    /// task text itself is shown only in the (credential-masked) summary,
    /// so a task that merely mentions a secret never breaks the binding.
    pub(crate) fn payload(&self) -> Value {
        json!({
            "owner_user_id": self.owner,
            "source_agent": self.source_agent,
            "source_employee": hex::encode(self.source_employee.0),
            "target_agent": self.target_agent,
            "target_employee": hex::encode(self.target_employee.0),
            "task_digest": self.task_digest,
        })
    }

    fn card_text(&self) -> String {
        format!("個人專屬員工「{}」想把一項工作交辦給共用員工「{}」。工作內容：{}\n\
            只有你（擁有者）可以決定；核准後只會送出這一次，拒絕或逾期都不會送出。",
            self.source_agent, self.target_agent, duduclaw_core::truncate_chars(&self.task, MAX_TASK_CHARS))
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
    let pending = broker.list_pending(None).await.map_err(|_| VerificationError::Unavailable)?;
    if pending.iter().filter(|card| card.action_kind == KIND).count() >= MAX_PENDING {
        return Err(VerificationError::Unavailable.into());
    }
    broker.request(&gate.source_agent, KIND, &gate.card_text(), gate.payload(), TTL_SECONDS).await
        .map_err(|_| VerificationError::Unavailable)?;
    tracing::info!(source_agent = gate.source_agent.as_str(), target_agent = gate.target_agent.as_str(),
        "personal delegation held for owner approval");
    Err(DelegationError::OwnerApprovalPending)
}

#[cfg(test)]
#[path = "personal_delegation_tests.rs"]
mod tests;
