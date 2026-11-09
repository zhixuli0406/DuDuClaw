//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Organization structure only. This does not grant access to an agent's
    /// configuration, conversations or actions; those retain binding checks.
    pub(crate) async fn handle_agents_org_chart(&self, ctx: &UserContext) -> WsFrame {
        // A human's department comes from users.db, never their bound agent,
        // request parameters or the connection's stale identity snapshot.
        let department = if ctx.is_admin() {
            None
        } else {
            let Some(db) = self.user_db.read().await.clone() else {
                return WsFrame::error_response("", PERMISSION_DENIED);
            };
            let user_id = ctx.user_id.clone();
            match tokio::task::spawn_blocking(move || db.get_user(&user_id)).await {
                Ok(Ok(Some(user))) if user.status == duduclaw_auth::UserStatus::Active
                    && !user.must_change_password => user.department.filter(|d| !d.is_empty()),
                _ => return WsFrame::error_response("", PERMISSION_DENIED),
            }
        };
        // v1.81 P1h (N6): another person's Personal employees (protected
        // mode) are never full rows — not for an Admin, not by department.
        // One that is the ancestor of a visible employee stays a name + role
        // stub (the chain must not break); otherwise it is dropped. Read
        // before the registry lock; unreadable ownership refuses.
        let hidden = match self.personal_hidden_agents(ctx).await {
            Ok(hidden) => hidden,
            Err(e) => return WsFrame::error_response("", e),
        };
        let mut reg = match tokio::time::timeout(
            std::time::Duration::from_millis(500), self.registry.write(),
        ).await {
            Ok(reg) => reg,
            Err(_) => return WsFrame::error_response("", "Organization is temporarily unavailable"),
        };
        if reg.scan().await.is_err() {
            return WsFrame::error_response("", "Organization is temporarily unavailable");
        }
        let listed = reg.list();
        let listable: std::collections::HashMap<&str, _> = listed.iter()
            .filter(|a| a.config.agent.status.is_listable(false))
            .filter(|a| ctx.is_admin() || !crate::machine_employee::requires_admin_for_id(
                &self.home_dir, &a.config.agent.name,
            ))
            .map(|a| (a.config.agent.name.as_str(), a)).collect();
        let full: std::collections::HashSet<&str> = org_chart_full(&listable, &hidden, |name, a| ctx.is_admin()
            || ctx.has_agent_access(name, AccessLevel::Viewer)
            || department.as_deref().is_some_and(|d| d == a.config.agent.department));
        let mut visible = full.clone();
        // Bound the walk by visited names; malformed cycles cannot loop or
        // pull in descendants/siblings. Hidden ancestors stop the chain.
        for name in &full {
            let mut cursor = *name;
            let mut seen = std::collections::HashSet::new();
            while seen.insert(cursor) {
                let Some(agent) = listable.get(cursor) else { break; };
                let parent = agent.config.agent.reports_to.as_str();
                if !listable.contains_key(parent) { break; }
                visible.insert(parent);
                cursor = parent;
            }
        }
        let agents: Vec<Value> = listed.iter()
            .filter(|a| visible.contains(a.config.agent.name.as_str()))
            .map(|a| {
                let cfg = &a.config.agent;
                if !full.contains(cfg.name.as_str()) {
                    // Ancestors outside the reader's scope are name + role
                    // only: no department, status, display name or parent.
                    return json!({ "name": cfg.name,
                        "role": format!("{:?}", cfg.role).to_lowercase() });
                }
                json!({
                    "name": cfg.name,
                    "display_name": cfg.display_name,
                    "department": cfg.department,
                    "role": format!("{:?}", cfg.role).to_lowercase(),
                    "status": format!("{:?}", cfg.status).to_lowercase(),
                    "icon": cfg.icon,
                    "reports_to": if visible.contains(cfg.reports_to.as_str()) {
                        cfg.reports_to.as_str()
                    } else { "" },
                })
            }).collect();
        WsFrame::ok_response("", json!({ "agents": agents }))
    }

    // ── Filtered agent list (respects UserContext) ────────────

    /// Agents the caller is directly bound to (all of them for an Admin).
    ///
    /// `ctx` is the live context `dispatch` rebuilt from users.db for this
    /// call (`live_context.rs`), not the handshake snapshot: an unbinding or
    /// a role change shows up on the next `agents.list` without a re-login.
    /// Team grants (`ctx.team_access`) are deliberately not listed here; this
    /// list also drives the per-agent settings pages, which stay limited to
    /// direct bindings.
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

        // v1.81 P1g: another person's Personal employee is listed to an
        // Admin as metadata only (label, owner, status) — the allowlisted
        // employee-list metadata; everything else needs the owner or the
        // audited step-up of the per-agent RPCs. Non-Admins never see it
        // (`visible_agents` drops it). Unreadable ownership refuses.
        let hidden = match self.personal_hidden_agents(ctx).await {
            Ok(hidden) => hidden,
            Err(e) => return WsFrame::error_response("", e),
        };
        let owners = if hidden.is_empty() {
            Default::default()
        } else {
            let home = self.home_dir.clone();
            match tokio::task::spawn_blocking(move || crate::personal_visibility::personal_owners(&home)).await {
                Ok(Ok(owners)) => owners,
                _ => return WsFrame::error_response("", crate::personal_visibility::OWNERSHIP_UNAVAILABLE),
            }
        };
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
            if visible_here && !hidden.contains(&name) && !agent_spent.contains_key(&name) {
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
                if hidden.contains(&cfg.agent.name) {
                    return personal_list_metadata(cfg, archived, owners.get(&cfg.agent.name), self.agent_has_avatar(&cfg.agent.name));
                }
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
                    "runtime": {
                        "provider": if let Some(removed) = cfg.runtime.provider.as_deref()
                            .and_then(duduclaw_core::runtime_catalog::removed_runtime) {
                            removed.id
                        } else {
                            cfg.runtime.provider.as_deref()
                                .and_then(duduclaw_core::types::RuntimeType::parse)
                                .unwrap_or_default().as_str()
                        },
                    },
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
                        "hard_stop": cfg.budget.hard_stop, "when_unavailable": cfg.budget.when_unavailable,
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

/// The org-chart rows shown in full: `in_scope` (Admin, binding, own
/// department) minus `hidden` (P1h N6: another person's Personal employees
/// are at most an ancestor stub, never a full row).
pub(super) fn org_chart_full<'a, A>(
    listable: &std::collections::HashMap<&'a str, A>,
    hidden: &std::collections::BTreeSet<String>,
    in_scope: impl Fn(&str, &A) -> bool,
) -> std::collections::HashSet<&'a str> {
    listable.iter()
        .filter(|(name, a)| !hidden.contains(**name) && in_scope(name, a))
        .map(|(name, _)| *name)
        .collect()
}

/// v1.81 P1g: the `agents.list` row of another person's Personal employee —
/// label, status, owner and the avatar flag only. No model, budget, skills,
/// permissions, heartbeat or evolution settings.
pub(super) fn personal_list_metadata(
    cfg: &duduclaw_core::types::AgentConfig,
    archived: bool,
    owner: Option<&String>,
    has_avatar: bool,
) -> Value {
    json!({
        "name": cfg.agent.name,
        "display_name": cfg.agent.display_name,
        "status": format!("{:?}", cfg.agent.status).to_lowercase(),
        "archived": archived,
        "has_avatar": has_avatar,
        "personal_employee": true,
        "owner_user_id": owner,
        "restricted": true,
    })
}
