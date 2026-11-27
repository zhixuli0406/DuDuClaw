//! Per-card outbound review authority. Never persisted on an approval record.
use std::path::Path;
use duduclaw_auth::{acl::ContextOrigin, UserContext};
use duduclaw_core::HumanPrincipal;
use crate::dept_grants::{DeptGrantView, department_grant_allows, GrantGroup, GrantAction};
use super::{ApprovalBroker, ApprovalRecord};

pub(crate) const REFUSAL: &str = "outgoing data approval requires an authorized dashboard reviewer";
pub(crate) const NOTICE_SUMMARY: &str = "有一則對外操作等待審核。";

/// Lists and notifications must not disclose content before an audited open.
pub(crate) fn list_summary(rec: &ApprovalRecord) -> &str {
    if is_outbound_data_card(rec) { NOTICE_SUMMARY } else { &rec.summary }
}

pub(crate) fn is_outbound_data_card(rec: &ApprovalRecord) -> bool {
    rec.payload.get("egress_guard").is_some() || rec.action_kind == super::cli_egress::KIND
}

/// `ctx` must be freshly resolved by the caller; grants require a real login.
pub(crate) fn outbound_reviewer(view: &DeptGrantView, ctx: &UserContext, rec: &ApprovalRecord) -> bool {
    if ctx.member_key_id.is_some() || ctx.origin == ContextOrigin::MemberKey
        || ctx.requires_password_change() { return false; }
    if ctx.is_admin() { return true; }
    if ctx.origin != ContextOrigin::DashboardSession { return false; }
    let Some(me) = HumanPrincipal::user(&ctx.user_id) else { return false; };
    if let Some(raw) = rec.on_behalf_of.as_deref() {
        match HumanPrincipal::parse(raw) {
            Ok(requester) if requester != me => (),
            _ => return false,
        }
    }
    department_grant_allows(view, &rec.agent_id, GrantGroup::EgressReview, GrantAction::Decide)
}

pub(crate) fn fresh_context(home: &Path, ctx: &UserContext) -> Result<UserContext, String> {
    let mut fresh = crate::review_evidence::audience::fresh_dashboard_context(home, ctx)?;
    fresh.origin = ctx.origin;
    Ok(fresh)
}

/// This path requires a session, unlike notification_allows(user_id).
pub(crate) fn authorize(home: &Path, ctx: &UserContext, rec: &ApprovalRecord) -> Result<UserContext, String> {
    let fresh = fresh_context(home, ctx)?;
    let view = DeptGrantView::load_session_home(home, ctx);
    if outbound_reviewer(&view, &fresh, rec) { Ok(fresh) } else { Err(REFUSAL.into()) }
}

impl ApprovalBroker {
    pub(crate) fn require_outbound_reviewer(&self, ctx: &UserContext, rec: &ApprovalRecord) -> Result<UserContext, String> {
        let home = self.home_dir().ok_or(REFUSAL)?;
        if ctx.user_id == "system" {
            self.require_current_dashboard_role(ctx, duduclaw_auth::UserRole::Admin)?;
        }
        authorize(&home, ctx, rec)
    }
}

/// The full card may be released only after this synchronous write succeeds.
pub(crate) fn audit_opened(home: &Path, rec: &ApprovalRecord, user: &str) -> Result<(), String> {
    duduclaw_security::audit::try_append_audit_event(home,
        &duduclaw_security::audit::AuditEvent::new(
            "outbound_card_opened", &rec.agent_id, duduclaw_security::audit::Severity::Info,
            serde_json::json!({"approval_id": rec.id.as_str(), "user_id": user,
                "reviewer_type": "department_manager", "decision": null}),
        ))
}

/// Same approvals security audit stream as workflow_activation_decided.
/// No payload, summary, source names or arguments enter the audit.
pub(crate) fn audit(home: &Path, rec: &ApprovalRecord, user: &str, admin: bool, decision: Option<bool>) {
    crate::security_autopilot::audit_and_emit(home,
        &duduclaw_security::audit::AuditEvent::new(
            if decision.is_some() { "outbound_decided" } else { "outbound_card_opened" },
            &rec.agent_id, duduclaw_security::audit::Severity::Info,
            serde_json::json!({"approval_id": rec.id.as_str(), "user_id": user,
                "reviewer_type": if admin { "admin" } else { "department_manager" },
                "decision": decision.map(|approve| if approve { "approved" } else { "denied" })}),
        ));
}

#[cfg(test)]
#[path = "outbound_tests.rs"]
mod tests;
