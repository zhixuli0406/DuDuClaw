//! B8 — per-employee skill management: switch a skill off or back on for one
//! employee, remove (archive) a skill from an employee's own layer, list and
//! restore removed skills.
//!
//! Permissions follow the employee profile files: Viewer of that employee (or
//! an admin) may read, the employee's Owner (or an admin) may change.
//! Department Skills grants are an alternative for these operations. Every
//! call re-reads the caller from `users.db`. A request naming several
//! employees is refused as a whole when any one of them is out of reach.
//!
//! Switching off only narrows (see `duduclaw_agent::skill_state`); removing
//! moves files to `<home>/skill_archive/` (see `duduclaw_agent::skill_archive`).
//! Company-wide and department skills are shared, so they can only be
//! switched off per employee, never removed from here.

#[allow(unused_imports)]
use super::*;

use crate::agent_files::flow::require_live_agent_access;
use crate::dept_grants::{department_grant_allows, record_grant_use, DeptGrantView, GrantAction, GrantGroup};
use duduclaw_agent::{skill_archive, skill_state};
use duduclaw_security::audit::{AuditEvent, Severity};

/// Upper bound on items in one bulk request.
const MAX_BULK_ITEMS: usize = 200;

/// The change was written but the running employees were not reloaded.
pub(crate) const RELOAD_FAILED: &str = "已儲存，但重新載入失敗，請重試";

/// One `(employee, skill)` target of a bulk request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillTarget {
    pub agent_id: String,
    pub skill_name: String,
}

/// Parse `{items: [{agent_id, skill_name}, …]}`.
pub(crate) fn parse_targets(params: &Value) -> Result<Vec<SkillTarget>, String> {
    let items = params
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or("`items` is required")?;
    if items.is_empty() {
        return Err("`items` is empty".into());
    }
    if items.len() > MAX_BULK_ITEMS {
        return Err(format!("at most {MAX_BULK_ITEMS} items per request"));
    }
    let mut out: Vec<SkillTarget> = Vec::with_capacity(items.len());
    for item in items {
        let agent_id = item.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let skill_name = item.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
        if !is_valid_agent_id(agent_id) {
            return Err(format!("invalid agent_id '{agent_id}'"));
        }
        skill_state::validate_skill_name(skill_name)?;
        let t = SkillTarget { agent_id: agent_id.into(), skill_name: skill_name.into() };
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// Distinct employees named in `targets`, in first-seen order.
fn agents_of(targets: &[SkillTarget]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for t in targets {
        if !seen.contains(&t.agent_id) {
            seen.push(t.agent_id.clone());
        }
    }
    seen
}

fn audit(home: &Path, event: &str, agent_id: &str, details: Value) {
    crate::security_autopilot::audit_and_emit(
        home,
        &AuditEvent::new(event, agent_id, Severity::Info, details),
    );
}

fn frame(r: Result<Value, String>) -> WsFrame {
    match r {
        Ok(v) => WsFrame::ok_response("", v),
        Err(e) => WsFrame::error_response("", &e),
    }
}

impl MethodHandler {
    /// Check every employee before writing; retain exactly the grant-only targets.
    fn require_owner_of_all(
        &self, ctx: &UserContext, agents: &[String], view: &DeptGrantView, action: GrantAction,
    ) -> Result<(UserContext, Vec<String>), String> {
        let mut fresh: Option<UserContext> = None;
        let mut granted = Vec::new();
        for a in agents {
            let live = match require_live_agent_access(&self.home_dir, ctx, a, AccessLevel::Owner) {
                Ok(live) => live,
                Err(e) => {
                    if !department_grant_allows(view, a, GrantGroup::Skills, action) {
                        return Err(e);
                    }
                    // Same live identity source as require_live_agent_access;
                    // failures remain closed even when the view has a grant.
                    let live = crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)
                        .map_err(|_| "permission denied".to_string())?;
                    granted.push(a.clone());
                    live
                }
            };
            fresh = Some(live);
        }
        Ok((fresh.ok_or_else(|| "no employee named".to_string())?, granted))
    }

    async fn audit_skill_grants(
        &self, ctx: &UserContext, changed: &[String], granted: &[String],
        action: GrantAction, method: &str,
    ) {
        for agent in changed.iter().filter(|agent| granted.contains(agent)) {
            record_grant_use(self, ctx, agent, GrantGroup::Skills, action, method).await;
        }
    }

    /// Rescan the registry so prompts pick up the change, and tell open
    /// dashboards to refetch. A failed rescan is an error for the caller: the
    /// change is on disk but the running employees still use the old set, so
    /// reporting success would show a switch that has not taken effect.
    async fn after_skill_change(&self, action: &str, agents: &[String]) -> Result<(), String> {
        let scanned = {
            let mut reg = self.registry.write().await;
            reg.scan().await
        };
        self.notify_skill_change(action, agents).await;
        scanned.map_err(|e| {
            warn!("rescan after skill {action} failed: {e}");
            RELOAD_FAILED.to_string()
        })
    }

    async fn notify_skill_change(&self, action: &str, agents: &[String]) {
        for a in agents {
            crate::dashboard_feedback::emit(
                &self.home_dir,
                crate::dashboard_feedback::EV_SKILL_CHANGED,
                json!({ "action": action, "agent_id": a }),
            )
            .await;
        }
    }

    /// `skills.set_enabled {items, enabled}`.
    pub(crate) async fn handle_skills_set_enabled(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.skills_set_enabled(&params, ctx, None).await)
    }

    pub(crate) async fn handle_admin_ai_skills_set_enabled(
        &self, params: Value, ctx: &UserContext, expected_hash: &str,
    ) -> WsFrame {
        if !ctx.is_admin() { return WsFrame::error_response("", "permission denied"); }
        frame(self.skills_set_enabled(&params, ctx, Some(expected_hash)).await)
    }

    async fn skills_set_enabled(&self, params: &Value, ctx: &UserContext, expected_hash: Option<&str>) -> Result<Value, String> {
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .ok_or("`enabled` (true or false) is required")?;
        let targets = parse_targets(params)?;
        let agents = agents_of(&targets);
        if expected_hash.is_some() && agents.len() != 1 { return Err(crate::admin_ai::proposal::STALE.into()); }
        let view = DeptGrantView::load(self, ctx).await;
        let (fresh, granted) = self.require_owner_of_all(ctx, &agents, &view, GrantAction::Toggle)?;
        let mut state_hash = None;
        let mut results: Vec<Value> = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        for agent in &agents {
            let names: Vec<String> = targets
                .iter()
                .filter(|t| &t.agent_id == agent)
                .map(|t| t.skill_name.clone())
                .collect();
            let outcome = skill_state::set_disabled_checked(&self.home_dir, agent, &names, !enabled, |state| {
                if let Some(expected) = expected_hash {
                    let actual = crate::admin_ai::proposal::hash_skills(state)
                        .map_err(|_| crate::admin_ai::proposal::STALE.to_string())?;
                    if actual != expected { return Err(crate::admin_ai::proposal::STALE.into()); }
                }
                Ok(())
            });
            if expected_hash.is_some() {
                let state = outcome.as_ref().map_err(Clone::clone)?;
                // This is the exact state returned by the locked write, never a
                // later reread that could accept somebody else's concurrent edit.
                state_hash = Some(crate::admin_ai::proposal::hash_skills(state)
                    .map_err(|_| crate::admin_ai::proposal::STALE.to_string())?);
            }
            let error = outcome.as_ref().err().cloned();
            if error.is_none() {
                changed.push(agent.clone());
                audit(
                    &self.home_dir,
                    if enabled { "skill_switched_on" } else { "skill_switched_off" },
                    agent,
                    json!({ "agent_id": agent, "skills": names, "user_id": fresh.user_id }),
                );
            }
            for n in &names {
                results.push(json!({ "agent_id": agent, "skill_name": n, "ok": error.is_none(), "error": error }));
            }
        }
        if !changed.is_empty() {
            self.audit_skill_grants(&fresh, &changed, &granted, GrantAction::Toggle, "skills.set_enabled").await;
            self.after_skill_change(if enabled { "enabled" } else { "disabled" }, &changed).await?;
        }
        let mut result = json!({ "results": results, "enabled": enabled });
        if let Some(hash) = state_hash { result["state_hash"] = json!(hash); }
        Ok(result)
    }

    /// `skills.remove {items}` — archive skills from employees' own layers.
    pub(crate) async fn handle_skills_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.skills_remove(&params, ctx).await)
    }

    async fn skills_remove(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let targets = parse_targets(params)?;
        let agents = agents_of(&targets);
        let view = DeptGrantView::load(self, ctx).await;
        let (fresh, granted) = self.require_owner_of_all(ctx, &agents, &view, GrantAction::Remove)?;
        let dirs = self.agent_skill_dirs(&agents).await?;
        let mut results: Vec<Value> = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        for t in &targets {
            let dir = &dirs[&t.agent_id];
            match skill_archive::archive(&self.home_dir, &t.agent_id, dir, &t.skill_name, &fresh.user_id) {
                Ok(m) => {
                    if !changed.contains(&t.agent_id) {
                        changed.push(t.agent_id.clone());
                    }
                    audit(
                        &self.home_dir,
                        "skill_removed",
                        &t.agent_id,
                        json!({
                            "agent_id": t.agent_id, "skill": t.skill_name,
                            "archive_id": m.archive_id, "user_id": fresh.user_id,
                        }),
                    );
                    results.push(json!({
                        "agent_id": t.agent_id, "skill_name": t.skill_name, "ok": true,
                        "archive_id": m.archive_id,
                    }));
                }
                Err(e) => results.push(json!({
                    "agent_id": t.agent_id, "skill_name": t.skill_name, "ok": false, "error": e,
                })),
            }
        }
        if !changed.is_empty() {
            self.audit_skill_grants(&fresh, &changed, &granted, GrantAction::Remove, "skills.remove").await;
            self.after_skill_change("removed", &changed).await?;
        }
        Ok(json!({ "results": results }))
    }

    /// `<agent_dir>/SKILLS` for each employee; errors when one is unknown.
    async fn agent_skill_dirs(
        &self,
        agents: &[String],
    ) -> Result<HashMap<String, PathBuf>, String> {
        let reg = self.registry.read().await;
        let mut out = HashMap::new();
        for a in agents {
            let agent = reg.get(a).ok_or_else(|| format!("Agent not found: {a}"))?;
            out.insert(a.clone(), agent.dir.join("SKILLS"));
        }
        Ok(out)
    }

    /// `skills.archived {agent_id}` — removed skills of one employee.
    pub(crate) async fn handle_skills_archived(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.skills_archived(&params, ctx).await)
    }

    async fn skills_archived(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if !is_valid_agent_id(agent_id) {
            return Err("`agent_id` is required".into());
        }
        let view = DeptGrantView::load(self, ctx).await;
        let fresh = match require_live_agent_access(&self.home_dir, ctx, agent_id, AccessLevel::Viewer) {
            Ok(fresh) => fresh,
            Err(e) => {
                if !department_grant_allows(&view, agent_id, GrantGroup::Skills, GrantAction::Read) {
                    return Err(e);
                }
                crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)
                    .map_err(|_| "permission denied".to_string())?
            }
        };
        let (can_manage, can_restore) = super::skills_list_rpc::skill_management_flags(&fresh, &view, agent_id);
        let items = skill_archive::list(&self.home_dir, agent_id)?;
        Ok(json!({
            "agent_id": agent_id,
            "can_manage": can_manage,
            "can_restore": can_restore,
            "archived": items.iter().map(|m| json!({
                "archive_id": m.archive_id,
                "skill_name": m.skill_name,
                "archived_at": m.archived_at,
                "archived_by": m.archived_by,
                "can_manage": can_manage,
                "can_restore": can_restore,
            })).collect::<Vec<_>>(),
        }))
    }

    /// `skills.restore {agent_id, archive_id}`.
    pub(crate) async fn handle_skills_restore(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.skills_restore(&params, ctx).await)
    }

    async fn skills_restore(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let archive_id = params.get("archive_id").and_then(|v| v.as_str()).unwrap_or("");
        if !is_valid_agent_id(agent_id) || archive_id.is_empty() {
            return Err("`agent_id` and `archive_id` are required".into());
        }
        let agents = vec![agent_id.to_string()];
        let view = DeptGrantView::load(self, ctx).await;
        let (fresh, granted) = self.require_owner_of_all(ctx, &agents, &view, GrantAction::Restore)?;
        let dir = self.agent_skill_dirs(&agents).await?.remove(agent_id).ok_or("Agent not found")?;
        let m = skill_archive::restore(&self.home_dir, agent_id, &dir, archive_id)?;
        audit(
            &self.home_dir,
            "skill_restored",
            agent_id,
            json!({ "agent_id": agent_id, "skill": m.skill_name, "archive_id": archive_id, "user_id": fresh.user_id }),
        );
        self.audit_skill_grants(&fresh, &agents, &granted, GrantAction::Restore, "skills.restore").await;
        self.after_skill_change("restored", &agents).await?;
        Ok(json!({ "agent_id": agent_id, "skill_name": m.skill_name, "restored": true }))
    }

    /// Activation counts per (employee, skill), plus the reason they are
    /// missing when the audit index cannot be read.
    pub(crate) async fn skill_usage_map(
        &self,
    ) -> (crate::evolution_events::skill_usage::SkillUsageSummary, Option<String>) {
        let idx = match self.audit_index().await {
            Ok(i) => i,
            Err(e) => {
                warn!("skill usage: audit index unavailable: {e}");
                return (Default::default(), Some("usage records unavailable".into()));
            }
        };
        match idx.skill_usage().await {
            Ok(m) => (m, None),
            Err(e) => {
                warn!("skill usage query failed: {e}");
                (Default::default(), Some("usage records unavailable".into()))
            }
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parse_targets_validates_and_dedups() {
        let p = json!({ "items": [
            { "agent_id": "alice", "skill_name": "pdf" },
            { "agent_id": "alice", "skill_name": "pdf" },
            { "agent_id": "bob", "skill_name": "報價" },
        ]});
        let t = parse_targets(&p).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(agents_of(&t), vec!["alice", "bob"]);
        assert!(parse_targets(&json!({})).is_err());
        assert!(parse_targets(&json!({ "items": [] })).is_err());
        assert!(parse_targets(&json!({ "items": [{ "agent_id": "../x", "skill_name": "a" }] })).is_err());
        assert!(parse_targets(&json!({ "items": [{ "agent_id": "a", "skill_name": "x/y" }] })).is_err());
        let many: Vec<Value> = (0..201)
            .map(|i| json!({ "agent_id": "a", "skill_name": format!("s{i}") }))
            .collect();
        assert!(parse_targets(&json!({ "items": many })).is_err());
    }
}
