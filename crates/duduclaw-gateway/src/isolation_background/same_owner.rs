//! H13b, owner decisions 2026-11-09 (A, B): the one place where a Personal
//! employee may run delegated background work.
//!
//! B1 (`check_lifecycle`, `capture_binding`) still refuses every other
//! background record of a Personal employee (cron, reminder, heartbeat, task,
//! task_spec, and any delegation from a Shared employee or from another
//! owner's employee). The exception is exactly one shape:
//!
//! - a `delegation` receipt captured from a LIVE run of a Personal source
//!   employee (`parent_agent`, `caller`, `parent_policy` all present);
//! - the target's lifecycle binding is `Personal(owner)` and the source's
//!   lifecycle binding is the very same `Personal(owner)` (exact equality);
//! - the receipt owner (the parent run's verified human principal) is
//!   exactly `user:<owner>`.
//!
//! The target may be the source itself: that is a protected `spawn_ephemeral`
//! child (decision A, `ephemeral_child`). A Personal source delegating to a
//! Shared target is NOT this exception: it keeps the owner approval card
//! (`personal_delegation`).
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_mcp_boundary::VerificationError;
use super::RecordAuthority;

/// The delegation target binding rule at capture. A Shared target keeps the
/// existing path (tenant check, owner card for a Personal source). A Personal
/// target is allowed only from a Personal source of exactly the same owner
/// whose run was started by that owner (`principal_user`); anything else is
/// `BindingMismatch`, never a card.
pub(crate) fn personal_target_allowed(source: &EmployeeBinding, target: &EmployeeBinding, principal_user: &str)
    -> Result<(), VerificationError> {
    match (source, target) {
        (_, EmployeeBinding::Shared(_)) => Ok(()),
        (EmployeeBinding::Personal(source_owner), EmployeeBinding::Personal(target_owner))
            if !target_owner.as_str().is_empty() && source_owner.as_str() == target_owner.as_str()
                && principal_user == target_owner.as_str() => Ok(()),
        _ => Err(VerificationError::BindingMismatch),
    }
}

/// The users.db facts a background receipt's binding needs, on one fresh
/// snapshot read for `(receipt owner, agent)`. Shared: the pinned tenant is the
/// installation tenant (unchanged rule). Personal (H13b only): the snapshot's
/// human IS the owner, the Personal mirror records that owner, the owner is
/// the agent's only member and is bound to it — the same facts B1's
/// `personal_facts` requires of a live Personal run.
pub(crate) fn check_binding_facts(binding: &EmployeeBinding, agent: &str,
    current: &duduclaw_auth::IsolationAuthoritySnapshot) -> Result<(), VerificationError> {
    match binding {
        EmployeeBinding::Shared(pinned) if pinned.as_str() == current.tenant_id => Ok(()),
        EmployeeBinding::Personal(owner) => {
            let owner = owner.as_str();
            let human = current.human.as_ref().ok_or(VerificationError::Revoked)?;
            if owner.is_empty() || human.user_id != owner || current.personal_owner.as_deref() != Some(owner)
                || current.agent_member_count != 1 || !human.agent_access.contains_key(agent) {
                return Err(VerificationError::Revoked);
            }
            Ok(())
        }
        _ => Err(VerificationError::Revoked),
    }
}

/// Is `receipt` the one Personal background shape B1 admits? Pure (no store
/// read): the live lifecycle of the target and of the source are compared by
/// `check_lifecycle` / `recheck_receipt`, the users.db facts by
/// [`check_binding_facts`].
pub(crate) fn personal_receipt_allowed(receipt: &RecordAuthority) -> bool {
    let Some(EmployeeBinding::Personal(owner)) = &receipt.binding else { return false };
    receipt.kind == "delegation" && !owner.as_str().is_empty()
        && receipt.parent_agent.is_some() && receipt.parent_policy.is_some() && receipt.caller.is_some()
        && super::user_id(&receipt.owner).is_ok_and(|user| user == owner.as_str())
}

/// The issuer's view (`isolation_mcp::boot::issuance_facts`): a record-backed
/// run of a Personal employee whose lifecycle owner is `owner` is issued only
/// for a receipt of the admitted shape bound to exactly that owner.
pub fn personal_record_allowed(receipt: &RecordAuthority, owner: &str) -> bool {
    personal_receipt_allowed(receipt)
        && matches!(&receipt.binding, Some(EmployeeBinding::Personal(bound)) if bound.as_str() == owner)
}

/// At recheck: for a Personal receipt the delegating source's live binding
/// must still be exactly the receipt's Personal binding (same owner).
pub(crate) fn check_source_binding(receipt: &RecordAuthority, source: &EmployeeBinding)
    -> Result<(), VerificationError> {
    match &receipt.binding {
        Some(EmployeeBinding::Personal(_)) if receipt.binding.as_ref() != Some(source) => Err(VerificationError::Revoked),
        _ => Ok(()),
    }
}
