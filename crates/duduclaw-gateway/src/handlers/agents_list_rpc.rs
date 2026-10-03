//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Filtered agent list (respects UserContext) ────────────

    pub(crate) async fn handle_agents_list_filtered(&self, ctx: &UserContext, params: Value) -> WsFrame {
        // Re-scan to pick up changes
        if let Ok(mut reg) =
            tokio::time::timeout(std::time::Duration::from_millis(500), self.registry.write()).await
        {
            let _ = reg.scan().await;
        }

        // WP4: archived agents are hidden by default; `include_archived=true`
        // surfaces them (still flagged). Soft-deleted agents are ALWAYS hidden.
        let include_archived = params
            .get("include_archived")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let reg = self.registry.read().await;
        let visible = ctx.visible_agents();

        // Real per-agent month-to-date spend from CostTelemetry. Computed once
        // per visible agent below (the rotator counter is unusable — see
        // `telemetry_spent_cents_for_agent`).
        let mut agent_spent: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for a in reg.list().iter() {
            let name = a.config.agent.name.clone();
            let visible_here = match &visible {
                None => true,
                Some(names) => names.contains(&name),
            };
            if visible_here && !agent_spent.contains_key(&name) {
                let spent = self.telemetry_spent_cents_for_agent(&name).await;
                agent_spent.insert(name, spent);
            }
        }

        let agents: Vec<Value> = reg.list().iter()
            .filter(|a| {
                match &visible {
                    None => true, // Admin sees all
                    Some(names) => names.contains(&a.config.agent.name),
                }
            })
            .filter(|a| a.config.agent.status.is_listable(include_archived))
            .map(|a| {
                let cfg = &a.config;
                let archived = matches!(cfg.agent.status, duduclaw_core::types::AgentStatus::Archived);
                json!({
                    "name": cfg.agent.name,
                    "display_name": cfg.agent.display_name,
                    "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                    "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                    "archived": archived,
                    "has_avatar": self.agent_has_avatar(&cfg.agent.name),
                    "outfit": read_agent_outfit(&self.home_dir.join("agents").join(&cfg.agent.name)),
                    "department": cfg.agent.department,
                    "trigger": cfg.agent.trigger,
                    "icon": cfg.agent.icon,
                    "reports_to": cfg.agent.reports_to,
                    "model": {
                        "preferred": cfg.model.preferred,
                        "fallback": cfg.model.fallback,
                        "account_pool": cfg.model.account_pool,
                        "api_mode": cfg.model.api_mode,
                        "local": cfg.model.local.as_ref().map(|l| json!({
                            "model": l.model,
                            "backend": l.backend,
                            "context_length": l.context_length,
                            "gpu_layers": l.gpu_layers,
                            "prefer_local": l.prefer_local,
                            "use_router": l.use_router,
                        })),
                    },
                    "budget": {
                        "monthly_limit_cents": cfg.budget.monthly_limit_cents,
                        "spent_cents": agent_spent.get(&cfg.agent.name).copied().unwrap_or(0),
                        "warn_threshold_percent": cfg.budget.warn_threshold_percent,
                        "hard_stop": cfg.budget.hard_stop,
                    },
                    "heartbeat": {
                        "enabled": cfg.heartbeat.enabled,
                        "interval_seconds": cfg.heartbeat.interval_seconds,
                    },
                    "skills": a.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
                    "permissions": {
                        "can_create_agents": cfg.permissions.can_create_agents,
                        "can_send_cross_agent": cfg.permissions.can_send_cross_agent,
                        "can_modify_own_skills": cfg.permissions.can_modify_own_skills,
                        "can_modify_own_soul": cfg.permissions.can_modify_own_soul,
                        "can_schedule_tasks": cfg.permissions.can_schedule_tasks,
                    },
                    // Evolution needs to be present here (not just in
                    // agents.inspect) because the dashboard's edit dialog
                    // initialises from the list response and silently
                    // falls back to hardcoded defaults when these are absent —
                    // making fields like `skill_auto_activate` (default `false`
                    // in JS, but typically `true` on disk) appear to never
                    // persist when in fact only the UI was misreading.
                    "evolution": {
                        "gvu_enabled": cfg.evolution.gvu_enabled,
                        "cognitive_memory": cfg.evolution.cognitive_memory_enabled(),
                        // v1.68: `skill_auto_activate` / `skill_security_scan` removed (no reader).
                        "max_silence_hours": cfg.evolution.max_silence_hours,
                    },
                })
            }).collect();

        info!(
            "agents.list: returning {} agents for user {}",
            agents.len(),
            ctx.email
        );
        WsFrame::ok_response("", json!({ "agents": agents }))
    }
}
