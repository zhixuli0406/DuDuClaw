//! RM-142 P11: a dashboard member's read-only view of rules acting on
//! directly bound employees. Never serialize a stored action or history row.

use super::*;
use std::collections::BTreeSet;

const DENIED: &str = "permission denied";
const INVALID_RULE: &str = "autopilot rule data unavailable";

impl MethodHandler {
    pub(crate) async fn handle_autopilot_affecting_me(&self, params: Value, ctx: &UserContext) -> WsFrame {
        match self.autopilot_affecting_result(&params, ctx, crate::runtime_readiness::isolation_active()).await {
            Ok(payload) => WsFrame::ok_response("", payload),
            Err(error) => WsFrame::error_response("", &error),
        }
    }

    // Explicit flag lets protected-mode tests use isolated homes without
    // changing the process-wide isolation flag or installing a global ACL.
    async fn autopilot_affecting_result(
        &self, params: &Value, ctx: &UserContext, protected: bool,
    ) -> Result<Value, String> {
        if ctx.origin != duduclaw_auth::acl::ContextOrigin::DashboardSession
            || ctx.member_key_id.is_some() || !params.is_object()
        { return Err(DENIED.into()); }
        let requested = match params.get("agent_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if !id.is_empty() && id.trim() == id => Some(id.as_str()),
            _ => return Err(DENIED.into()),
        };
        let live = crate::review_evidence::audience::fresh_dashboard_context(self.home_dir(), ctx)?;
        acl::require_role(&live, UserRole::Employee)?;
        // Owner-only even if a request obtained an unrelated break-glass or
        // avatar grant. Errors reading ownership must withhold the response.
        let hidden = crate::personal_visibility::hidden_from(self.home_dir(), protected, &live.user_id)
            .map_err(|_| crate::personal_visibility::OWNERSHIP_UNAVAILABLE)?;
        let mut visible: BTreeSet<String> = live.agent_access.iter()
            .filter(|(agent, level)| level.at_least(AccessLevel::Viewer) && !hidden.contains(*agent))
            .map(|(agent, _)| agent.clone()).collect();
        if let Some(agent) = requested {
            if !visible.contains(agent) { return Err(DENIED.into()); }
            visible.retain(|id| id == agent);
        }
        let store = self.autopilot_store.read().await.clone().ok_or("Autopilot store unavailable")?;
        let mut rules = Vec::new();
        let mut display = crate::dept_grants::DisplayAutopilotCreatorAccessCache::load(self).await;
        for row in store.list_rules().await? {
            let Some(mut rule) = affecting_rule(&row, &visible, &hidden)? else { continue; };
            let history: Vec<Value> = store.list_history(Some(&row.id), 20).await?.into_iter()
                .map(|entry| json!({"triggered_at": entry.triggered_at, "result": entry.result})).collect();
            rule["history"] = json!(history);
            rule["creator_access_lost"] = json!(display.display_creator_access_lost(
                row.created_by_user_id.as_deref(), row.owner_department.as_deref()));
            rules.push(rule);
        }
        Ok(json!({"rules": rules}))
    }
}

fn affecting_rule(
    row: &AutopilotRuleRow, visible: &BTreeSet<String>, hidden: &BTreeSet<String>,
) -> Result<Option<Value>, String> {
    let action: Value = serde_json::from_str(&row.action).map_err(|_| INVALID_RULE)?;
    let steps: Vec<&Value> = if action.get("type").and_then(Value::as_str) == Some("steps") {
        action.get("steps").and_then(Value::as_array).ok_or(INVALID_RULE)?.iter().collect()
    } else { vec![&action] };
    if steps.is_empty() { return Err(INVALID_RULE.into()); }
    // Hide the entire rule if it names another person's Personal employee,
    // including mixed rules whose other steps target a visible employee.
    if super::personal_p1g_gate::names_hidden(&action, hidden) { return Ok(None); }
    let mut kinds = Vec::new();
    let mut targets = BTreeSet::new();
    let mut actions = Vec::new();
    let mut require_approval = false;
    for step in steps {
        let kind = step.get("type").and_then(Value::as_str).ok_or(INVALID_RULE)?;
        if !matches!(kind, "delegate" | "run_skill" | "notify" | "proactive_notify" | "wait") {
            return Err(INVALID_RULE.into());
        }
        kinds.push(kind);
        let approval = crate::approval::rule_requires_approval(step);
        require_approval |= approval;
        if !matches!(kind, "delegate" | "run_skill") { continue; }
        let target = step.get("target_agent").and_then(Value::as_str).ok_or(INVALID_RULE)?;
        if !visible.contains(target) { continue; }
        let field = if kind == "delegate" { "prompt" } else { "skill_name" };
        let template = step.get(field).and_then(Value::as_str).ok_or(INVALID_RULE)?;
        let mut item = json!({"type": kind, "target_agent": target, "require_approval": approval});
        item[field] = json!(template);
        actions.push(item);
        targets.insert(target);
    }
    if targets.is_empty() { return Ok(None); }
    let conditions: Value = serde_json::from_str(&row.conditions).map_err(|_| INVALID_RULE)?;
    Ok(Some(json!({
        "id": row.id, "name": row.name, "enabled": row.enabled,
        "trigger_event": row.trigger_event, "conditions": conditions,
        "action_kinds": kinds, "target_agents": targets, "actions": actions,
        "require_approval": require_approval, "owner_department": row.owner_department,
    })))
}

#[cfg(test)]
#[path = "tests/autopilot_affecting_tests.rs"]
mod tests;
