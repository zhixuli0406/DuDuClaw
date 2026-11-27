//! The single department containment seam for automation admission and steps.

use std::path::Path;
use serde_json::{Map, Value};
use crate::autopilot_store::AutopilotRuleRow;

#[derive(Debug, PartialEq, Eq)]
pub enum ScopeDecision {
    Allow,
    Deny { reason: String },
}

pub fn check(
    home: &Path,
    rule: &AutopilotRuleRow,
    step: Option<&Value>,
    fields: &Map<String, Value>,
    origin: crate::autopilot_engine::EventOrigin,
) -> ScopeDecision {
    if rule.owner_department.is_none() {
        return ScopeDecision::Allow;
    }
    match crate::dept_grants::rules::dept_rule_contained(home, rule, step, fields, origin) {
        Ok(()) => ScopeDecision::Allow,
        Err(reason) => ScopeDecision::Deny { reason },
    }
}

/// A queue tag only narrows authority, regardless of sender, lane or payload.
pub(crate) async fn queued_work_allowed(
    home: &Path, message: &crate::message_queue::QueueMessage,
) -> bool {
    let Some(department) = message.autopilot_department.as_deref() else { return true; };
    if !duduclaw_core::is_valid_department(department) { return false; }
    let Some(rule_id) = message.autopilot_rule_id.as_deref() else { return false; };
    let Ok(store) = crate::autopilot_store::AutopilotStore::open(home) else { return false; };
    let Ok(Some(rule)) = store.get_rule(rule_id).await else { return false; };
    rule.enabled && rule.owner_department.as_deref() == Some(department)
        && crate::dept_grants::rules::contained_step(home, &rule, department,
            &serde_json::json!({"type":"delegate", "target_agent":message.target})).is_ok()
}

/// Re-read authority before any outbox Activity or push. A department notice
/// goes only to its eligible owner's verified DMs, never the global Admin list.
/// `None` retains the company notice destination; errors never fall back to it.
pub(crate) async fn notice_targets(
    home: &Path, store: &crate::autopilot_store::AutopilotStore, rule_id: &str,
    department: Option<&str>,
) -> Result<Option<Vec<(String, String)>>, String> {
    let rule = store.get_rule(rule_id).await.map_err(|_| "scope_denied")?
        .ok_or("scope_denied")?;
    if rule.owner_department.as_deref() != department { return Err("scope_denied".into()); }
    let Some(department) = department else { return Ok(None); };
    if !rule.enabled || !duduclaw_core::is_valid_department(department) {
        return Err("scope_denied".into());
    }
    crate::dept_grants::rules::owner_notice_targets(home, &rule, department).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_rule_allowed_department_requires_event_subject() {
        let mut rule: AutopilotRuleRow = serde_json::from_value(serde_json::json!({
            "id": "r", "name": "rule", "enabled": true, "trigger_event": "task_created",
            "conditions": "{}", "action": "{}", "created_at": "", "trigger_count": 0
        })).unwrap();
        let fields = Map::new();
        assert_eq!(check(Path::new("."), &rule, None, &fields, crate::autopilot_engine::EventOrigin::gateway()), ScopeDecision::Allow);
        rule.owner_department = Some("sales".into());
        assert_eq!(check(Path::new("."), &rule, None, &fields, crate::autopilot_engine::EventOrigin::gateway()), ScopeDecision::Deny {
            reason: "subject_missing".into(),
        });
    }
}
