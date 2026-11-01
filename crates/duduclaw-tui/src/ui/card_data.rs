//! Interpret only explicit capability facts; gateway authorization remains final.
use crate::{auth::Role, sanitize::TerminalText, state::{InboxItem, Model}, strings};
use serde_json::Value;

pub(super) fn blocked(item: &InboxItem, model: &Model) -> Option<TerminalText> {
    let data = item.data();
    let reason = || TerminalText::new(format!("{}\n{}", strings::CARD_READ_ONLY,
        data.get("may_decide_reason").or_else(|| data.get("reason"))
            .or_else(|| data.get("invalidated_reason"))
            .and_then(Value::as_str).unwrap_or(strings::CARD_NO_PERMISSION)));
    match item {
        InboxItem::Approval(a) => {
            if a.decided_in_dashboard_only { return Some(TerminalText::new(strings::CARD_DASHBOARD.into())); }
            if data.get("may_decide").and_then(Value::as_bool) != Some(true)
                || !matches!(model.role, Some(Role::Admin | Role::Manager)) { return Some(reason()); }
            let kind = data.get("kind").and_then(Value::as_str).unwrap_or("");
            if duduclaw_gateway::governance::ApprovalKind::parse(kind).requires_board()
                && model.role != Some(Role::Admin) { return Some(reason()); }
            // Questions need answers; bound operations and standing authority need dashboard context.
            if a.request_kind.as_ref().is_some_and(|k| !k.is("approval"))
                || data.get("binding").is_some_and(|b| !b.is_null()) {
                return Some(TerminalText::new(strings::CARD_DASHBOARD.into()));
            }
        }
        InboxItem::Task(t) => {
            if !t.goal_mode { return Some(TerminalText::new(strings::CARD_NOT_GOAL.into())); }
            if data.get("may_decide").and_then(Value::as_bool) == Some(false)
                || !model.goal_operators.iter().any(|id| t.assigned_to.is(id)) {
                return Some(reason());
            }
        }
    }
    None
}
pub(super) fn high_risk(_data: &Value) -> bool {
    // approvals.list produces `kind`, but no trusted safety classification for
    // any action kind. Neither kind alone nor caller-controlled payload fields
    // establish low risk. Require full inspection for every approval.
    true
}
