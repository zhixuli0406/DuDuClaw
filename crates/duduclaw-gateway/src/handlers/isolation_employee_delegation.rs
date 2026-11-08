//! SR2 finding 2 (owner decision 2026-11-08): the Personal employee's OWNER
//! decides the approval card for a delegation from its live run to a Shared
//! employee (`crate::isolation_background::personal_delegation`).
//!
//! - `me.isolation.list_personal_delegations`: the caller's own pending cards.
//! - `me.isolation.decide_personal_delegation {id, approve}`: only the owner
//!   named on the card; approving also requires a fresh lifecycle read that
//!   the caller still owns exactly the source employee id on the card. The
//!   decision is recorded as `dashboard:<owner>` (the only decider the send
//!   path accepts). `approvals.decide` refuses this kind for everyone.
use super::*;
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::isolation_background::PERSONAL_DELEGATION_KIND as KIND;

pub(super) const DELEGATIONS: &str = "me.isolation.list_personal_delegations";
pub(super) const DECIDE_DELEGATION: &str = "me.isolation.decide_personal_delegation";
const NOT_FOUND: &str = "找不到這張交辦核准卡片，或它不屬於你的個人專屬員工。";
const NOT_PENDING: &str = "這張交辦核准卡片已經決定或已逾期，交辦不會送出。";
const OWNER_CHANGED: &str = "無法確認你仍是這位個人專屬員工的擁有者，已拒絕核准。";
const DECISION_INVALID: &str = "請選擇核准或拒絕這張交辦卡片。";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Decision { id: String, approve: bool }

pub(super) fn parse_decision(params: Value) -> Result<Decision, &'static str> {
    let decision: Decision = serde_json::from_value(params).map_err(|_| DECISION_INVALID)?;
    if decision.id.is_empty() || decision.id.len() > 64 { return Err(DECISION_INVALID); }
    Ok(decision)
}

fn owned_by(card: &crate::approval::ApprovalRecord, user: &str) -> bool {
    card.action_kind == KIND && card.payload.get("owner_user_id").and_then(Value::as_str) == Some(user)
}

/// The caller's own pending delegation cards.
pub(super) async fn list(home: &Path, user: &str) -> Result<Value, &'static str> {
    let broker = ApprovalBroker::open(home).map_err(|_| SERVICE_UNAVAILABLE)?;
    let _ = broker.expire_stale().await;
    let now = chrono::Utc::now();
    let cards = broker.list_by_kind(KIND).await.map_err(|_| SERVICE_UNAVAILABLE)?;
    let mine: Vec<Value> = cards.iter()
        .filter(|card| owned_by(card, user) && card.status == ApprovalStatus::Pending && !card.is_stale(now))
        .map(|card| json!({
            "id": card.id.as_str(), "summary": card.summary,
            "source_agent": card.payload["source_agent"], "target_agent": card.payload["target_agent"],
            "created_at": card.created_at,
            "expires_at": card.expires_at_epoch(),
        })).collect();
    Ok(json!({ "delegations": mine }))
}

pub(super) async fn decide(home: &Path, user: &str, input: Decision) -> Result<Value, &'static str> {
    let broker = ApprovalBroker::open(home).map_err(|_| SERVICE_UNAVAILABLE)?;
    let id = ApprovalId::from(input.id);
    let card = broker.get(&id).await.map_err(|_| SERVICE_UNAVAILABLE)?.ok_or(NOT_FOUND)?;
    if !owned_by(&card, user) { return Err(NOT_FOUND); }
    if card.status != ApprovalStatus::Pending || card.is_stale(chrono::Utc::now()) {
        let _ = broker.poll(&id).await;
        return Err(NOT_PENDING);
    }
    if input.approve {
        // Fresh: the caller still owns exactly the source employee on the card.
        let source = card.payload["source_agent"].as_str().ok_or(NOT_FOUND)?.to_owned();
        let employee = card.payload["source_employee"].as_str().ok_or(NOT_FOUND)?.to_owned();
        let (home, owner) = (home.to_path_buf(), user.to_owned());
        blocking(move || {
            let current = lifecycle::read_lifecycle(&home, &source).map_err(|_| OWNER_CHANGED)?;
            if current.personal_owner() != Some(owner.as_str()) || hex::encode(current.employee.0) != employee {
                return Err(OWNER_CHANGED);
            }
            Ok(())
        }).await?;
    }
    broker.decide(&id, input.approve, &format!("dashboard:{user}")).await.map_err(|_| NOT_PENDING)?;
    Ok(json!({ "id": id.as_str(), "decided": if input.approve { "approved" } else { "denied" } }))
}
