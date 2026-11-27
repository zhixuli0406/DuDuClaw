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
) -> ScopeDecision {
    if rule.owner_department.is_none() {
        return ScopeDecision::Allow;
    }
    match crate::dept_grants::rules::dept_rule_contained(home, rule, step, fields) {
        Ok(()) => ScopeDecision::Allow,
        Err(reason) => ScopeDecision::Deny { reason },
    }
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
        assert_eq!(check(Path::new("."), &rule, None, &fields), ScopeDecision::Allow);
        rule.owner_department = Some("sales".into());
        assert_eq!(check(Path::new("."), &rule, None, &fields), ScopeDecision::Deny {
            reason: "subject_missing".into(),
        });
    }
}
