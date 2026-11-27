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
    match dept_rule_contained(home, rule, step, fields) {
        Ok(()) => ScopeDecision::Allow,
        Err(reason) => ScopeDecision::Deny { reason },
    }
}

/// RM-142 replaces this body with the real predicate (reads org.toml live;
/// checks the event subject, each step's target_agent and notify destinations
/// stay inside owner_department). Until then any rule with an owning department
/// never runs.
fn dept_rule_contained(
    _home: &Path,
    _rule: &AutopilotRuleRow,
    _step: Option<&Value>,
    _fields: &Map<String, Value>,
) -> Result<(), String> {
    Err("department_scope_unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dept_rule_denied_until_predicate_wired() {
        let mut rule: AutopilotRuleRow = serde_json::from_value(serde_json::json!({
            "id": "r", "name": "rule", "enabled": true, "trigger_event": "task_created",
            "conditions": "{}", "action": "{}", "created_at": "", "trigger_count": 0
        })).unwrap();
        let fields = Map::new();
        assert_eq!(check(Path::new("."), &rule, None, &fields), ScopeDecision::Allow);
        rule.owner_department = Some("sales".into());
        assert_eq!(check(Path::new("."), &rule, None, &fields), ScopeDecision::Deny {
            reason: "department_scope_unavailable".into(),
        });
    }
}
