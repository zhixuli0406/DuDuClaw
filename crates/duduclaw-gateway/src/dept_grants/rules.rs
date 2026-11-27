//! Write-side department rule validation. Runtime containment is wired separately.

use serde_json::Value;

use super::*;
use crate::autopilot_store::AutopilotRuleRow;

const DENIED: &str = "permission denied";

/// `None` is the Admin path, selected only by the authenticated RPC context.
/// Apply containment to the shared builder's validated, normalized row,
/// including fields omitted by an update.
pub(crate) async fn validate_department_rule(
    h: &MethodHandler, view: Option<&DeptGrantView>, row: &AutopilotRuleRow,
) -> Result<(), String> {
    let department = row.owner_department.as_deref().ok_or(DENIED)?;
    if !duduclaw_core::is_valid_department(department)
        || view.is_some_and(|v| v.is_empty()
            || !v.departments_for(GrantGroup::Automation).iter().any(|d| d == department))
        || !matches!(row.trigger_event.as_str(), "task_created" | "task_updated"
            | "task_status_changed" | "activity_new" | "channel_message" | "agent_idle" | "run_at_risk")
        || row.sequence.is_some() || row.metadata.is_some()
    { return Err(DENIED.into()); }
    let admin_targets;
    let targets = if let Some(view) = view { view } else {
        admin_targets = DeptGrantView::load_targets(h).await.ok_or(DENIED)?;
        &admin_targets
    };
    // Unlike the caller-dependent ACL shortcut, even the personal employee's
    // own Admin/Manager cannot put it in a shared department rule.
    let owners = crate::personal_visibility::personal_owners(h.home_dir()).map_err(|_| DENIED)?;
    let target_in_department = |agent: &str| -> Result<bool, String> {
        Ok(!owners.contains_key(agent)
            && grant_target_department(targets, agent).map_err(|_| DENIED)?.as_deref() == Some(department))
    };
    if view.is_none() {
        let mut authoritative = false;
        for agent in targets.loaded_agents.keys() {
            authoritative |= target_in_department(agent)?;
        }
        if !authoritative { return Err(DENIED.into()); }
    }

    let action: Value = serde_json::from_str(&row.action).map_err(|_| DENIED)?;
    // The shared save validator has checked the normalized envelope, templates,
    // screen, window and run policy. Only department containment remains here.
    let steps = action.get("steps").and_then(Value::as_array).ok_or(DENIED)?;
    for step in steps {
        if step.get("type").and_then(Value::as_str) == Some("wait") { continue; }
        match step.get("type").and_then(Value::as_str) {
            Some("delegate" | "run_skill") => {
                let target = step.get("target_agent").and_then(Value::as_str).ok_or(DENIED)?;
                if !target_in_department(target)? || view.is_some_and(|v|
                    !department_grant_allows(v, target, GrantGroup::Automation, GrantAction::Create))
                { return Err(DENIED.into()); }
            }
            Some("notify" | "proactive_notify") => {
                let channel = step.get("channel").and_then(Value::as_str).ok_or(DENIED)?;
                let chat_id = step.get("chat_id").and_then(Value::as_str).ok_or(DENIED)?;
                let owner = row.created_by_user_id.as_deref().ok_or(DENIED)?;
                let db = h.user_db().await.ok_or(DENIED)?;
                let destinations = db.verified_dm_destinations_for_user(owner).map_err(|_| DENIED)?;
                let mut allowed = destinations.iter().any(|dest|
                    dest.channel == channel && dest.channel_user_id == chat_id);
                // Read the entire candidate set: a failed authority/config read
                // never becomes an absent destination or a fallback allow.
                for agent in targets.loaded_agents.keys() {
                    if !target_in_department(agent)? { continue; }
                    let target = crate::goal_notify::try_agent_notify_target(h.home_dir(), agent)
                        .map_err(|_| DENIED)?;
                    allowed |= target.is_some_and(|(ch, chat)| ch == channel && chat == chat_id);
                }
                if !allowed { return Err(DENIED.into()); }
            }
            _ => return Err(DENIED.into()),
        }
    }
    Ok(())
}
