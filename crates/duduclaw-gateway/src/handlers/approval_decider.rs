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
    submitter_is_decider_id(rec, &decider.user_id)
}

/// Compare the authenticated account id without fabricating dashboard claims.
pub(crate) fn submitter_is_decider_id(rec: &ApprovalRecord, user_id: &str) -> bool {
    is_activation_record(rec)
        && rec.binding.as_ref().is_some_and(|b| {
            b.decision_context.channel == "dashboard"
                && b.decision_context.principal_id == user_id
        })
}

impl MethodHandler {
    pub(crate) async fn authorize_egress_decider(&self, rec: &ApprovalRecord, ctx: &UserContext) -> Result<(), String> {
        if crate::approval::is_outbound_data_card(rec) {
            let fresh = crate::approval::outbound::fresh_context(&self.home_dir, ctx)?;
            let view = crate::dept_grants::DeptGrantView::load(self, ctx).await;
            if !crate::approval::outbound_reviewer(&view, &fresh, rec) {
                return Err(crate::approval::outbound::REFUSAL.into());
            }
        }
        Ok(())
    }

    /// The fresh identity allowed to decide `rec`, or why not.
    pub(crate) async fn authorize_bound_decider(
        &self,
        rec: &ApprovalRecord,
        ctx: &UserContext,
    ) -> Result<UserContext, String> {
        let fresh = crate::approval::outbound::fresh_context(&self.home_dir, ctx)
            .map_err(|_| "permission denied".to_string())?;
        let binding = rec.binding.as_ref().ok_or("permission denied")?;
        if rec.action_kind == crate::admin_ai::card::ACTION_KIND {
            return if crate::admin_ai::may_decide(&self.home_dir, rec, &fresh.user_id) { Ok(fresh) }
                else { Err(crate::admin_ai::REFUSAL.into()) };
        }
        if crate::google_identity::member_card::is_member_card(rec) {
            return if crate::google_identity::member_card::may_decide(&self.home_dir, rec, &fresh.user_id) { Ok(fresh) }
                else { Err("Only the requesting member may confirm".into()) };
        }
        // Department review is card-scoped and needs no Operator binding.
        if crate::approval::is_outbound_data_card(rec) && !is_activation_record(rec) {
            self.authorize_egress_decider(rec, ctx).await?;
            return Ok(fresh);
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
        audit_activation_decision(&self.home_dir, rec, &decider.user_id, approve);
    }
}

/// Shared audit for dashboard and verified chat-admin activation decisions.
pub(crate) fn audit_activation_decision(
    home: &std::path::Path, rec: &ApprovalRecord, user_id: &str, approve: bool,
) {
    if !is_activation_record(rec) {
        return;
    }
    let self_approved = submitter_is_decider_id(rec, user_id);
    crate::security_autopilot::audit_and_emit(
        home,
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
                "decided_by": user_id,
                "decision": if approve { "approved" } else { "denied" },
                "submitted_by": rec.binding.as_ref().map(|b| &b.decision_context.principal_id),
                "submitter_is_decider": self_approved,
            }),
        ),
    );
}
