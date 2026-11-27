//! Write-side department rule validation. Runtime containment is wired separately.

use serde_json::{json, Value};

use super::*;
use crate::autopilot_store::AutopilotRuleRow;
use crate::handlers::{validate_autopilot_action, validate_autopilot_conditions};

const DENIED: &str = "permission denied";

/// Keep approval on the step and screening on the envelope when upgrading a
/// legacy single action. Department rules must fail closed on older engines.
pub(crate) fn normalize_department_action(mut action: Value) -> Result<Value, String> {
    if action.get("type").and_then(Value::as_str) == Some("steps") {
        return Ok(action);
    }
    validate_autopilot_action(&action)?;
    let screen = action.as_object_mut().and_then(|obj| obj.remove("screen"));
    let mut envelope = json!({"type": "steps", "steps": [action]});
    if let Some(screen) = screen { envelope["screen"] = screen; }
    Ok(envelope)
}

/// `None` is the Admin path, selected only by the authenticated RPC context.
/// Always validate the normalized row, including fields omitted by an update.
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
    let conditions: Value = serde_json::from_str(&row.conditions).map_err(|_| DENIED)?;
    validate_autopilot_conditions(&conditions)?;
    if let Some(window) = &row.active_window {
        crate::autopilot_window::parse_active_window(
            &serde_json::from_str::<Value>(window).map_err(|_| DENIED)?)?;
    }
    if let Some(policy) = &row.run_policy {
        crate::autopilot_window::parse_run_policy(
            &serde_json::from_str::<Value>(policy).map_err(|_| DENIED)?)?;
    }
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
    let envelope = action.as_object().ok_or(DENIED)?;
    if action.get("type").and_then(Value::as_str) != Some("steps")
        || envelope.keys().any(|key| !matches!(key.as_str(), "type" | "steps" | "screen"))
    { return Err(DENIED.into()); }
    if let Some(screen) = action.get("screen").filter(|v| !v.is_null()) {
        crate::autopilot_screen::validate_screen_spec(screen)?;
    }
    let steps = action.get("steps").and_then(Value::as_array).ok_or(DENIED)?;
    if steps.is_empty() || steps.len() > 5 { return Err(DENIED.into()); }
    let mut wait_minutes = 0;
    let mut previous_wait = false;
    for (index, step) in steps.iter().enumerate() {
        if step.get("type").and_then(Value::as_str) == Some("wait") {
            let minutes = step.get("minutes").and_then(Value::as_u64)
                .filter(|n| (1..=1440).contains(n)).ok_or(DENIED)?;
            wait_minutes += minutes;
            if previous_wait || index + 1 == steps.len() || wait_minutes > 1440
                || step.as_object().ok_or(DENIED)?.keys().any(|k| !matches!(k.as_str(), "type" | "minutes"))
            { return Err(DENIED.into()); }
            previous_wait = true;
            continue;
        }
        previous_wait = false;
        validate_autopilot_action(step)?;
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
