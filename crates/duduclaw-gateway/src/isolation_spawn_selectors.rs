//! H13: live host half of the `spawn_agent` / `spawn_ephemeral` selectors
//! (`duduclaw_mcp_boundary::tool_policy::spawn`). A child module of
//! `isolation_resources`, so it shares the resolver's live stores and its
//! private `task_owned` rule.
//!
//! Every call re-reads, never caches:
//! - the record task (when the run has one) is still assigned to this
//!   employee (`tasks.db` + audience rule, the H8 rule). A reassigned or
//!   deleted task refuses both tools and writes `isolation_task_run_refused`;
//! - `spawn_agent`: the target is allowed by the live delegation policy
//!   (`config.toml [delegation]` + org view, the same predicate the dispatcher
//!   gate applies at dequeue) AND by the isolation bindings: the target must be
//!   a confirmed, enabled protected employee; a Personal target is allowed
//!   only from a Personal source of exactly the same owner whose run that
//!   owner started (H13b, owner decision B 2026-11-09 — the same rule
//!   `isolation_background::same_owner` applies when the receipt is captured
//!   and rechecked); another owner's Personal target, or any Personal target
//!   of a Shared source, is refused; a Shared target must be in the source's
//!   own tenant. A Personal source delegating to a Shared target is allowed
//!   here: the handler's `record_bus_delegation` then files the existing owner
//!   approval card (`personal_delegation`) and queues nothing until the owner
//!   approves;
//! - `spawn_ephemeral`: the tool intersection must be non-empty and the
//!   Personal-parent owner rule hold (`spawn_selector`).
//!
//! H13c: the record principal of a run started from a background receipt is
//! also the receipt owner's `Human`, so the record cannot tell a live owner
//! session from a delegated background run or an ephemeral child. The issuer
//! therefore never lists either spawn tool in a record-backed run of a
//! Personal employee (`same_owner::record_run_tools`: not discovered, not
//! eligible), and the queue captures refuse such a source for a Personal
//! target (`same_owner::personal_target_from_run`, `ephemeral_parent_live`).
//!
//! The confirmed-row read is a refusal source only; the authority for the
//! queued delegation remains the full lifecycle read of `record_bus_delegation`.
use duduclaw_isolation::capability::{EmployeeBinding, ResourceGrant};
use duduclaw_mcp_boundary::{tool_policy::{self, ToolPolicy}, RequestContext, VerificationError};
use duduclaw_migration::allocation::Receipt;
use serde_json::Value;
use super::GatewayResourceResolver;

impl GatewayResourceResolver {
    /// Exactly the boundary's record-side selection, after the live checks.
    pub(super) fn spawn_selection(&self, context: &RequestContext, policy: &ToolPolicy, args: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        tool_policy::check_spawn_arguments(policy.name, args)?;
        let selection = tool_policy::spawn_selector(context, policy.name)?;
        if selection.first() != Some(&ResourceGrant::Employee { id: self.employee }) {
            return Err(VerificationError::BindingMismatch);
        }
        for grant in &selection {
            if let ResourceGrant::Task { id } = grant {
                if !self.task_owned(policy.name, id.as_str())? {
                    crate::isolation_background::record_foreign_task(&self.home, &self.agent, id.as_str(), policy.name);
                    return Err(VerificationError::Invalid);
                }
            }
        }
        if policy.name == "spawn_agent" { self.spawn_target(context, args)?; }
        else { tool_policy::ephemeral_tools(context, args)?; }
        Ok(selection)
    }

    fn spawn_target(&self, context: &RequestContext, args: &Value) -> Result<(), VerificationError> {
        let target = args.get("agent_id").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
        if target == self.agent || !duduclaw_core::is_valid_agent_id(target) || duduclaw_core::is_reserved_agent_id(target)
            || target == duduclaw_core::UNTRUSTED_AGENT_ID || crate::ephemeral::is_ephemeral_id(target) {
            return Err(VerificationError::Invalid);
        }
        let rules = duduclaw_core::delegation_rules_from_home(&self.home);
        let org = crate::delegation_gate::DispatchOrgView::new(&self.home, None);
        duduclaw_core::can_delegate_rules(&rules, &org, &self.agent, target).map_err(|_| VerificationError::Invalid)?;
        let receipt = crate::isolation_lifecycle::confirmed_receipt(&self.home, target).map_err(|error| match error {
            crate::isolation_lifecycle::LifecycleError::Unavailable => VerificationError::Unavailable,
            _ => VerificationError::Revoked,
        })?;
        target_binding_allowed(&context.record().employee_binding, receipt.as_ref())?;
        // H13b: a same-owner Personal target also needs the owner to have
        // started this run (the record principal is the Human owner).
        if matches!(receipt.as_ref().and_then(|receipt| receipt.principal_binding.as_ref()),
            Some(EmployeeBinding::Personal(_))) {
            tool_policy::spawn::ephemeral_parent_allowed(context)?;
        }
        Ok(())
    }
}

/// Binding rule for a `spawn_agent` target's confirmed receipt: absent ⇒
/// `Invalid` (not a protected employee), disabled ⇒ `Revoked`, Personal ⇒
/// `BindingMismatch` unless the source is a Personal employee of exactly the
/// same owner (H13b), Shared in another tenant than a Shared source ⇒
/// `BindingMismatch`. A legacy receipt without a principal binding is Shared
/// in the pinned tenant; the handler's full lifecycle read decides it.
pub(super) fn target_binding_allowed(source: &EmployeeBinding, target: Option<&Receipt>)
    -> Result<(), VerificationError> {
    let receipt = target.ok_or(VerificationError::Invalid)?;
    if receipt.disabled { return Err(VerificationError::Revoked); }
    match (&receipt.principal_binding, source) {
        (Some(EmployeeBinding::Personal(target_owner)), EmployeeBinding::Personal(own))
            if !own.as_str().is_empty() && target_owner.as_str() == own.as_str() => Ok(()),
        (Some(EmployeeBinding::Personal(_)), _) => Err(VerificationError::BindingMismatch),
        (Some(EmployeeBinding::Shared(tenant)), EmployeeBinding::Shared(own)) if tenant != own =>
            Err(VerificationError::BindingMismatch),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "isolation_spawn_selectors_tests.rs"]
mod tests;
