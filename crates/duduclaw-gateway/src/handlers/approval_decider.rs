//! Who may decide, and who may read the decision material of, a bound
//! approval card (F1b: A-M-3 and U3).
//!
//! Every answer is computed from a fresh identity read, never from the
//! long-lived session claims:
//! - a workflow activation: a current Admin who can open the source draft
//!   (task access plus audience). Self-approval is allowed and reported.
//! - any other bound card: a current Manager (or Admin) with Operator access
//!   to the employee the card belongs to;
//! - a workflow step card additionally: a member of the run's audience
//!   (the same check that guards `workflow_runs.get`).
use super::*;
use crate::approval::ApprovalRecord;
pub(crate) use crate::approval::{activation_facts, is_activation_record};

/// For an activation: the dashboard user who submitted it decides it too.
pub(crate) fn submitter_is_decider(rec: &ApprovalRecord, decider: &UserContext) -> bool {
    is_activation_record(rec)
        && rec.binding.as_ref().is_some_and(|b| {
            b.decision_context.channel == "dashboard"
                && b.decision_context.principal_id == decider.user_id
        })
}


impl MethodHandler {
    pub(crate) fn authorize_egress_decider(&self, rec: &ApprovalRecord, ctx: &UserContext) -> Result<(), String> {
        if rec.payload.get("egress_guard").is_some() {
            let fresh = crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)?;
            if !fresh.is_admin() { return Err("outgoing data approval requires an Admin".into()); }
        }
        Ok(())
    }

    /// The fresh identity allowed to decide `rec`, or why not.
    pub(crate) async fn authorize_bound_decider(
        &self,
        rec: &ApprovalRecord,
        ctx: &UserContext,
    ) -> Result<UserContext, String> {
        let fresh = crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)
            .map_err(|_| "permission denied".to_string())?;
        let binding = rec.binding.as_ref().ok_or("permission denied")?;
        // Egress cards disclose full private arguments only to Admins.
        // Keep decision authority aligned with that visibility rule.
        if rec.payload.get("egress_guard").is_some() && !fresh.is_admin() {
            return Err("outgoing data approval requires an Admin".into());
        }
        if is_activation_record(rec) {
            if !fresh.is_admin() {
                return Err("workflow activation can only be decided by an Admin".into());
            }
            self.authorize_workflow_activation_record(&rec.payload, &fresh)
                .await
                .map_err(|_| "permission denied".to_string())?;
            return Ok(fresh);
        }
        if !fresh.has_role(UserRole::Manager) {
            return Err("permission denied".into());
        }
        if !fresh.has_agent_access(&rec.agent_id, AccessLevel::Operator) {
            return Err("you do not have access to this AI employee".into());
        }
        if binding.run_origin_kind == "workflow" {
            Box::pin(self.accessible_run(&json!({ "run_id": binding.run_id }), &fresh))
                .await
                .map_err(|_| "you are not in this workflow run's audience".to_string())?;
        }
        Ok(fresh)
    }

    /// Audit a dashboard decision on a workflow activation; the row says
    /// whether the submitter approved their own request.
    pub(crate) fn audit_activation_decision(
        &self,
        rec: &ApprovalRecord,
        decider: &UserContext,
        approve: bool,
    ) {
        if !is_activation_record(rec) {
            return;
        }
        let self_approved = submitter_is_decider(rec, decider);
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "workflow_activation_decided",
                rec.agent_id.as_str(),
                if self_approved && approve {
                    duduclaw_security::audit::Severity::Warning
                } else {
                    duduclaw_security::audit::Severity::Info
                },
                json!({
                    "approval_id": rec.id.as_str(),
                    "decided_by": decider.user_id,
                    "decision": if approve { "approved" } else { "denied" },
                    "submitted_by": rec.binding.as_ref().map(|b| &b.decision_context.principal_id),
                    "submitter_is_decider": self_approved,
                }),
            ),
        );
    }
}
