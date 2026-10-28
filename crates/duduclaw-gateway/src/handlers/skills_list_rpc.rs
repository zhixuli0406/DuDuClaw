//! `skills.list` — the dashboard skill library's read path.
//!
//! Every layer (company-wide, department, the employee's own) is re-read from
//! disk and composed with the same precedence the registry uses, so the
//! dashboard shows what the employee would load. Each entry also carries the
//! per-employee switch (`enabled`), activation counts from the audit index
//! (`use_count`, `last_used_at`), the file time (`modified_at`) and the
//! frontmatter description and tags for searching.
//!
//! B8: callers see only the employees they may view (Viewer or above, read
//! live); a single-employee request for an employee out of reach is refused.
//! Security verdicts are keyed by layer and owner, so two employees' skills
//! that share a name no longer share a verdict.

#[allow(unused_imports)]
use super::*;

use crate::evolution_events::skill_usage::SkillUsageMap;
use duduclaw_agent::registry::{AgentRegistry, LoadedAgent, SkillFile};
use duduclaw_agent::skill_state::{self, DisabledSkills};

/// Verdict-map key for a skill in one layer: `global/<name>`,
/// `department:<dept>/<name>` or `agent:<id>/<name>`.
pub(crate) fn verdict_key(layer: &str, name: &str) -> String {
    format!("{layer}/{name}")
}

/// Verdict-map layer for an install `scope` (`global`, `department:<d>` or an agent id).
pub(crate) fn verdict_layer_for_install_scope(scope: &str) -> String {
    if scope == "global" || scope.starts_with("department:") {
        scope.to_string()
    } else {
        format!("agent:{scope}")
    }
}

/// Context shared by every entry of one `skills.list` answer.
struct ListCtx<'a> {
    verdicts: &'a serde_json::Map<String, Value>,
    usage: &'a SkillUsageMap,
}

/// The JSON entry for one composed skill of one employee.
fn skill_entry(
    s: &SkillFile,
    scope: &str,
    verdict_layer: &str,
    agent_id: &str,
    state: &DisabledSkills,
    lc: &ListCtx<'_>,
) -> Value {
    let meta = duduclaw_agent::skill_loader::parse_skill_meta_from_content(&s.content, &s.name);
    let loc = duduclaw_agent::skill_loader::DEFAULT_SKILL_LOCALE;
    let usage = lc.usage.get(&(agent_id.to_string(), s.name.clone()));
    let mut obj = json!({
        "name": s.name,
        "display_name": meta.display_name(loc),
        "description": meta.display_description(loc),
        "tags": meta.tags,
        "size": s.content.len(),
        "scope": scope,
        // Include `content` so the dashboard can render a preview — the
        // SkillInfo frontend contract requires it.
        "content": s.content,
        "agent_id": agent_id,
        "modified_at": s.modified_unix,
        "enabled": !state.is_disabled(&s.name),
        "use_count": usage.map(|u| u.count).unwrap_or(0),
        "last_used_at": usage.map(|u| u.last_used_at.clone()),
    });
    if let Some(v) = lc.verdicts.get(&verdict_key(verdict_layer, &s.name)) {
        obj["security_status"] = v.clone();
    }
    obj
}

fn state_json(state: &DisabledSkills) -> Value {
    match state.error() {
        None => json!({ "ok": true }),
        Some(e) => json!({ "ok": false, "error": e }),
    }
}

impl MethodHandler {
    /// One entry of the `scanned` diagnostic block returned by `skills.list`:
    /// the exact directories walked plus their per-layer counts, so an empty
    /// list can be told apart from "the folder I expected does not exist".
    pub(crate) fn skill_layer_diag(layer: &str, dir: &std::path::Path, count: usize) -> Value {
        json!({
            "layer": layer,
            "path": dir.display().to_string(),
            "exists": dir.is_dir(),
            "count": count,
        })
    }

    /// Re-read an employee's department + own layers from disk and compose
    /// them over the (already re-read) global layer. Returns the entries, the
    /// scanned-directory diagnostics and the employee's switch state.
    async fn read_agent_skill_layers(
        &self,
        agent: &LoadedAgent,
        global_skills: &[SkillFile],
        lc: &ListCtx<'_>,
    ) -> (Vec<Value>, Vec<Value>, DisabledSkills) {
        let agent_id = agent.config.agent.name.as_str();
        let local_dir = agent.dir.join("SKILLS");
        let local_skills = AgentRegistry::load_skills(&local_dir).await;

        let dept = agent.config.agent.department.trim();
        let dept_dir = (!dept.is_empty() && duduclaw_core::is_valid_department(dept))
            .then(|| duduclaw_agent::skill_loader::department_skills_dir(&self.home_dir, dept));
        let dept_skills = match &dept_dir {
            Some(d) => AgentRegistry::load_skills(d).await,
            None => Vec::new(),
        };

        // Layer membership decides the badge (an own override of a global
        // skill is `agent`, not `global`).
        let local_names: std::collections::HashSet<String> =
            local_skills.iter().map(|s| s.name.clone()).collect();
        let dept_names: std::collections::HashSet<String> =
            dept_skills.iter().map(|s| s.name.clone()).collect();

        let mut diag = vec![Self::skill_layer_diag("agent", &local_dir, local_skills.len())];
        if let Some(d) = &dept_dir {
            diag.push(Self::skill_layer_diag("department", d, dept_skills.len()));
        }

        let state = skill_state::read_disabled(&self.home_dir, agent_id);
        let composed = AgentRegistry::compose_skill_layers(global_skills, dept_skills, local_skills);
        let skills = composed
            .iter()
            .map(|s| {
                let (scope, layer) = if local_names.contains(&s.name) {
                    ("agent", format!("agent:{agent_id}"))
                } else if dept_names.contains(&s.name) {
                    ("department", format!("department:{dept}"))
                } else {
                    ("global", "global".to_string())
                };
                skill_entry(s, scope, &layer, agent_id, &state, lc)
            })
            .collect();
        (skills, diag, state)
    }

    pub(crate) async fn handle_skills_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let fresh = match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
            Ok(f) => f,
            Err(_) => return WsFrame::error_response("", "permission denied"),
        };
        let global_dir = self.home_dir.join("skills");
        let global_skills = AgentRegistry::load_skills(&global_dir).await;
        let global_diag = Self::skill_layer_diag("global", &global_dir, global_skills.len());
        let verdicts = self.load_skill_scan_verdicts();
        let (summary, usage_error) = self.skill_usage_map().await;
        let usage_truncated = summary.truncated;
        let lc = ListCtx { verdicts: &verdicts, usage: &summary.usage };
        let reg = self.registry.read().await;

        if let Some(id) = params.get("agent_id").and_then(|v| v.as_str()) {
            if !fresh.has_agent_access(id, AccessLevel::Viewer) {
                return WsFrame::error_response("", "permission denied");
            }
            let Some(agent) = reg.get(id) else {
                return WsFrame::error_response("", &format!("Agent not found: {id}"));
            };
            let (skills, mut scanned, state) = self.read_agent_skill_layers(agent, &global_skills, &lc).await;
            scanned.push(global_diag);
            return WsFrame::ok_response(
                "",
                json!({
                    "agent_id": id,
                    "skills": skills,
                    "scanned": scanned,
                    "skill_state": state_json(&state),
                    "can_manage": fresh.has_agent_access(id, AccessLevel::Owner),
                    "usage_error": usage_error,
                    "usage_truncated": usage_truncated,
                }),
            );
        }

        let visible = |name: &str| fresh.has_agent_access(name, AccessLevel::Viewer);
        let mut holders: HashMap<String, Vec<Value>> = HashMap::new();
        let mut all_agents = Vec::new();
        let mut scanned = vec![global_diag];
        for agent in reg.list().into_iter().filter(|a| visible(&a.config.agent.name)) {
            let (skills, diag, state) = self.read_agent_skill_layers(agent, &global_skills, &lc).await;
            scanned.extend(diag);
            let (global_rows, own): (Vec<Value>, Vec<Value>) =
                skills.into_iter().partition(|s| s["scope"] == "global");
            for g in global_rows {
                let name = g["name"].as_str().unwrap_or_default().to_string();
                holders.entry(name).or_default().push(json!({
                    "agent_id": agent.config.agent.name,
                    "display_name": agent.config.agent.display_name,
                    "enabled": g["enabled"],
                    "use_count": g["use_count"],
                    "last_used_at": g["last_used_at"],
                }));
            }
            all_agents.push(json!({
                "agent_id": agent.config.agent.name,
                "display_name": agent.config.agent.display_name,
                "skills": own,
                "skill_state": state_json(&state),
                "can_manage": fresh.has_agent_access(&agent.config.agent.name, AccessLevel::Owner),
            }));
        }
        let global: Vec<Value> = global_skills
            .iter()
            .map(|s| global_entry(s, holders.remove(&s.name).unwrap_or_default(), &lc))
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "global_skills": global,
                "agents": all_agents,
                "scanned": scanned,
                "usage_error": usage_error,
                "usage_truncated": usage_truncated,
            }),
        )
    }
}

/// One employee's composed copy of a skill, read exactly the way `skills.list`
/// reads it (same layers, same switches, same usage index, same verdicts).
/// Used by `tasks.skill_assign` to pick who runs a skill.
#[derive(Debug, Clone)]
pub(crate) struct SkillHolding {
    pub agent_id: String,
    pub agent_display_name: String,
    pub enabled: bool,
    /// RFC 3339 time of the latest activation, `None` when never run.
    pub last_used_at: Option<String>,
    pub display_name: String,
    pub description: String,
    pub content: String,
    /// The install scan rated this copy `fail`.
    pub security_failed: bool,
}

impl MethodHandler {
    /// Every employee visible to `fresh` (Viewer or above, the same rule as
    /// the aggregate `skills.list`) whose composed skill set has a skill named
    /// exactly `skill_name`, in registry order (by employee id).
    pub(crate) async fn skill_holdings(
        &self,
        fresh: &UserContext,
        skill_name: &str,
    ) -> (Vec<SkillHolding>, Option<String>) {
        let global_skills = AgentRegistry::load_skills(&self.home_dir.join("skills")).await;
        let verdicts = self.load_skill_scan_verdicts();
        let (summary, usage_error) = self.skill_usage_map().await;
        let lc = ListCtx {
            verdicts: &verdicts,
            usage: &summary.usage,
        };
        let reg = self.registry.read().await;
        let mut out = Vec::new();
        for agent in reg
            .list()
            .into_iter()
            .filter(|a| fresh.has_agent_access(&a.config.agent.name, AccessLevel::Viewer))
        {
            let (skills, _, _) = self
                .read_agent_skill_layers(agent, &global_skills, &lc)
                .await;
            let Some(entry) = skills.into_iter().find(|s| s["name"] == skill_name) else {
                continue;
            };
            let text = |k: &str| entry[k].as_str().unwrap_or_default().to_string();
            out.push(SkillHolding {
                agent_id: agent.config.agent.name.clone(),
                agent_display_name: agent.config.agent.display_name.clone(),
                enabled: entry["enabled"] == true,
                last_used_at: entry["last_used_at"].as_str().map(str::to_string),
                display_name: text("display_name"),
                description: text("description"),
                content: text("content"),
                security_failed: entry["security_status"] == "fail",
            });
        }
        (out, usage_error)
    }
}

/// A company-wide skill in the aggregate view: who has it and their totals.
fn global_entry(s: &SkillFile, holders: Vec<Value>, lc: &ListCtx<'_>) -> Value {
    let use_count: u64 = holders.iter().filter_map(|h| h["use_count"].as_u64()).sum();
    let last_used_at = holders
        .iter()
        .filter_map(|h| h["last_used_at"].as_str())
        .max()
        .map(str::to_string);
    let mut obj = skill_entry(s, "global", "global", "", &DisabledSkills::none(), lc);
    let enabled_count = holders.iter().filter(|h| h["enabled"] == true).count();
    obj["agent_id"] = Value::Null;
    obj["use_count"] = json!(use_count);
    obj["last_used_at"] = json!(last_used_at);
    obj["enabled"] = json!(enabled_count > 0 || holders.is_empty());
    obj["enabled_count"] = json!(enabled_count);
    obj["holders"] = json!(holders);
    obj
}
