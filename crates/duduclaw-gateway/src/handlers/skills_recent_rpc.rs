use super::*;

impl MethodHandler {
    pub(crate) async fn handle_skills_recent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let fresh = match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
            Ok(f) => f,
            Err(_) => return WsFrame::error_response("", "permission denied"),
        };
        let agent = params.get("agent_id").and_then(Value::as_str);
        let assigning = params.get("assigning").and_then(Value::as_bool).unwrap_or(false);
        if agent.is_some_and(|id| !fresh.has_agent_access(id, AccessLevel::Viewer)) {
            return WsFrame::error_response("", "permission denied");
        }
        let history = match crate::recent_skills::list(&self.home_dir, &fresh.user_id) {
            Ok(h) => h,
            Err(_) => return WsFrame::error_response("", "recent skills unavailable"),
        };
        let mut skills = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (_, name) in history {
            if !seen.insert(name.clone()) { continue; }
            let (holdings, _) = self.skill_holdings(&fresh, &name).await;
            let mut eligible = None;
            for holding in holdings {
                if !holding.enabled || holding.security_failed
                    || agent.is_some_and(|id| id != holding.agent_id)
                    || (assigning && !fresh.can_assign_to(&holding.agent_id))
                    || !fresh.can_call_agent(&holding.agent_id)
                    || self.deny_main_for_non_admin(&fresh, &holding.agent_id).await.is_err()
                { continue; }
                eligible = Some(holding);
                break;
            }
            if let Some(holding) = eligible {
                skills.push(json!({ "name": name, "display_name": holding.display_name,
                    "description": duduclaw_core::truncate_chars(&holding.description, 160) }));
            }
            if skills.len() == 5 { break; }
        }
        WsFrame::ok_response("", json!({ "skills": skills }))
    }
}
