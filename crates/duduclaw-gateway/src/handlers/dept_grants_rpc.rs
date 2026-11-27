//! Department management metadata; grants never widen ordinary employee access.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use duduclaw_agent::skill_loader::authoritative_department;
use duduclaw_core::types::AgentRole;

use super::*;
use crate::dept_grants::{department_grant_allows, DeptGrantView, GrantAction, GrantGroup};

const UNAVAILABLE: &str = "無法讀取部門管理授權，請稍後再試。";
const INVALID_GROUP: &str = "請選擇有效的管理功能：排程、自動化規則、技能、用量或對外傳送審核。";

pub(super) fn acknowledgement_required() -> WsFrame {
    WsFrame::Response {
        id: String::new(), ok: false, payload: None,
        error: Some(json!({
            "code": "acknowledgement_required",
            "message": "請先閱讀風險說明並勾選「我了解」後再授權。",
        })),
    }
}

/// Only serialize authority already admitted by the request-local view.
pub(super) fn effective_grants(view: &DeptGrantView) -> Value {
    let mut grants: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for group in GrantGroup::ALL {
        for department in view.departments_for(group) {
            grants.entry(department).or_default().push(group.as_str());
        }
    }
    json!(grants.into_iter().map(|(department, groups)| {
        json!({"department": department, "groups": groups})
    }).collect::<Vec<_>>())
}

fn store_message(error: &str) -> &'static str {
    match error {
        duduclaw_auth::dept_grants::ACKNOWLEDGEMENT_REQUIRED => duduclaw_auth::dept_grants::ACKNOWLEDGEMENT_REQUIRED,
        "user not found" => "找不到指定的成員。",
        "department grants require an active Manager" => "只能授權給使用中的主管帳號。",
        "invalid department name" => "部門名稱格式不正確。",
        _ => "無法儲存部門管理授權，請稍後再試。",
    }
}

/// Do not infer a role from an unreadable mirror or use a stale registry role.
fn non_main_role(home: &Path, id: &str) -> bool {
    if !duduclaw_core::is_valid_agent_id(id) { return false; }
    let Ok(raw) = std::fs::read_to_string(home.join("agents").join(id).join("agent.toml")) else {
        return false;
    };
    let Ok(config) = toml::from_str::<toml::Value>(&raw) else { return false; };
    let role: Option<AgentRole> = config.get("agent").and_then(|a| a.get("role"))
        .and_then(|role| role.clone().try_into().ok());
    matches!(role, Some(role) if role != AgentRole::Main)
}

impl MethodHandler {
    /// Admin-only preview. Use the same locked effective entry and canonical
    /// target as org.apply_change, even when a display mirror has drifted.
    pub(crate) async fn handle_dept_grants_reach_preview(&self, params: Value) -> WsFrame {
        let Some(agent_id) = params["agent_id"].as_str().filter(|id| duduclaw_core::is_valid_agent_id(id)) else {
            return WsFrame::error_response("", "請指定有效的 AI 員工。");
        };
        let Some(department) = params["department"].as_str() else {
            return WsFrame::error_response("", "請選擇部門。");
        };
        if !duduclaw_core::org_change::agent_exists(&self.home_dir, agent_id) {
            return WsFrame::error_response("", "找不到指定的 AI 員工。");
        }
        let guard = match super::org_change_rpc::lock_org(&self.home_dir).await {
            Ok(guard) => guard,
            Err(_) => return WsFrame::error_response("", "暫時無法讀取組織資料，請稍後再試。"),
        };
        let store = match guard.load() {
            Ok(store) => store,
            Err(_) => return WsFrame::error_response("", "暫時無法讀取組織資料，請稍後再試。"),
        };
        let before = duduclaw_core::org_change::effective_in(&store, &self.home_dir, agent_id).unwrap_or_default();
        let after = duduclaw_core::org_store::OrgEntry::new(&before.reports_to, department);
        if !after.department.is_empty() && !duduclaw_core::department::is_valid_department(&after.department) {
            return WsFrame::error_response("", "部門名稱格式不正確。");
        }
        match crate::dept_grants::reach::for_move(&self.home_dir, &before.department, &after.department) {
            Ok(lines) => WsFrame::ok_response("", crate::dept_grants::reach::preview(&lines)),
            Err(_) => WsFrame::error_response("", crate::dept_grants::reach::UNAVAILABLE),
        }
    }

    // These three handlers are Admin-gated by dispatch_org.
    pub(crate) async fn handle_dept_grants_list(&self) -> WsFrame {
        let Some(db) = self.user_db().await else { return WsFrame::error_response("", UNAVAILABLE); };
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<Value>, ()> {
            let grants = db.list_dept_grants().map_err(|_| ())?;
            let users: BTreeMap<_, _> = db.list_users().map_err(|_| ())?.into_iter()
                .map(|user| (user.id.clone(), user)).collect();
            grants.into_iter().map(|grant| {
                let user = users.get(&grant.user_id).ok_or(())?;
                Ok(json!({
                    "user_id": grant.user_id, "display_name": user.display_name,
                    "email": user.email, "department": grant.department,
                    "groups": grant.groups.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
                    "granted_by": grant.granted_by, "granted_at": grant.granted_at,
                }))
            }).collect()
        }).await;
        match result {
            Ok(Ok(rows)) => WsFrame::ok_response("", json!(rows)),
            _ => WsFrame::error_response("", UNAVAILABLE),
        }
    }

    pub(crate) async fn handle_dept_grants_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(user_id) = params.get("user_id").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
            return WsFrame::error_response("", "請指定要設定部門管理授權的成員。");
        };
        let Some(department) = params.get("department").and_then(Value::as_str) else {
            return WsFrame::error_response("", "請選擇部門。");
        };
        let Some(raw_groups) = params.get("groups").and_then(Value::as_array) else {
            return WsFrame::error_response("", INVALID_GROUP);
        };
        let mut groups = BTreeSet::new();
        for value in raw_groups {
            let Some(group) = value.as_str().and_then(GrantGroup::parse) else {
                return WsFrame::error_response("", INVALID_GROUP);
            };
            groups.insert(group);
        }
        let Some(db) = self.user_db().await else { return WsFrame::error_response("", UNAVAILABLE); };
        let home = self.home_dir().to_path_buf();
        let actor = ctx.user_id.clone();
        let acknowledged = params.get("acknowledged").and_then(Value::as_bool) == Some(true);
        let target = user_id.to_owned();
        let department = department.to_owned();
        let groups: Vec<_> = groups.into_iter().collect();
        let result = tokio::task::spawn_blocking(move || -> Result<(), &'static str> {
            // Revocation must also work after a department disappears or its mirrors drift.
            if !groups.is_empty() {
                let org = duduclaw_core::org_store::load_for_write(&home).map_err(|_| UNAVAILABLE)?;
                let authoritative: BTreeSet<_> = org.iter()
                    .filter_map(|(id, _)| authoritative_department(&home, id)).collect();
                if !authoritative.contains(&department) {
                    return Err("此部門尚未確認或已不存在，請先由管理員同步部門資料後再授權。");
                }
            }
            db.set_dept_grants(&actor, &target, &department, &groups, acknowledged)
                .map_err(|error| store_message(&error))
        }).await;
        match result {
            Ok(Ok(())) => WsFrame::ok_response("", json!({"ok": true})),
            Ok(Err(duduclaw_auth::dept_grants::ACKNOWLEDGEMENT_REQUIRED)) => acknowledgement_required(),
            Ok(Err(message)) => WsFrame::error_response("", message),
            Err(_) => WsFrame::error_response("", "無法儲存部門管理授權，請稍後再試。"),
        }
    }

    pub(crate) async fn handle_dept_grants_options(&self) -> WsFrame {
        let home = self.home_dir().to_path_buf();
        let default_id = match crate::webchat::access::gateway_default_id(&home).await {
            Ok(id) => id,
            Err(_) => return WsFrame::error_response("", UNAVAILABLE),
        };
        let loaded: BTreeSet<String> = {
            let registry = self.registry().read().await;
            registry.list().into_iter().filter(|agent| {
                agent.config.agent.role != AgentRole::Main && agent.config.agent.name != default_id
            }).filter_map(|agent| agent.dir.file_name()?.to_str().map(str::to_owned)).collect()
        };
        let result = tokio::task::spawn_blocking(move || -> Result<Value, ()> {
            let org = duduclaw_core::org_store::load_for_write(&home).map_err(|_| ())?;
            let mut departments: BTreeMap<String, usize> = BTreeMap::new();
            let mut mirror_mismatch_count = 0;
            for (id, entry) in org.iter() {
                if entry.department.is_empty() { continue; }
                match authoritative_department(&home, id) {
                    Some(department) => {
                        // Keep authoritative departments selectable even with zero reachable agents.
                        let count = departments.entry(department).or_default();
                        if id != &default_id && loaded.contains(id) && non_main_role(&home, id) {
                            *count += 1;
                        }
                    }
                    None => mirror_mismatch_count += 1,
                }
            }
            Ok(json!({
                "departments": departments.into_iter().map(|(name, agent_count)| {
                    json!({"name": name, "agent_count": agent_count})
                }).collect::<Vec<_>>(),
                "mirror_mismatch_count": mirror_mismatch_count,
            }))
        }).await;
        match result {
            Ok(Ok(options)) => WsFrame::ok_response("", options),
            _ => WsFrame::error_response("", UNAVAILABLE),
        }
    }

    pub(crate) async fn handle_dept_grants_mine(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let groups = match params.get("group") {
            None => GrantGroup::ALL.to_vec(),
            Some(value) => match value.as_str().and_then(GrantGroup::parse) {
                Some(group) => vec![group],
                None => return WsFrame::error_response("", INVALID_GROUP),
            },
        };
        let view = DeptGrantView::load(self, ctx).await;
        if view.is_empty() {
            return WsFrame::ok_response("", json!({"grants": [], "agents": [], "needs_sync": []}));
        }
        let home = self.home_dir().to_path_buf();
        let default_id = match crate::webchat::access::gateway_default_id(&home).await {
            Ok(id) => id,
            Err(_) => return WsFrame::error_response("", UNAVAILABLE),
        };
        let agents: Vec<_> = {
            let registry = self.registry().read().await;
            registry.list().into_iter().filter(|agent| {
                agent.config.agent.role != AgentRole::Main && agent.config.agent.name != default_id
            }).filter_map(|agent| Some((
                agent.dir.file_name()?.to_str()?.to_owned(), agent.config.agent.display_name.clone(),
            ))).collect()
        };
        let ctx = ctx.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, ()> {
            let org = duduclaw_core::org_store::load_for_write(&home).map_err(|_| ())?;
            let granted_departments: BTreeSet<_> = groups.iter()
                .flat_map(|group| view.departments_for(*group)).collect();
            let mut manageable = Vec::new();
            let mut needs_sync = Vec::new();
            let principal = format!("user:{}", ctx.user_id);
            for (id, display_name) in agents {
                let allowed: Vec<_> = groups.iter().copied().filter(|group| {
                    department_grant_allows(&view, &id, *group, GrantAction::Read)
                }).map(GrantGroup::as_str).collect();
                if !allowed.is_empty() {
                    manageable.push(json!({"id": id, "display_name": display_name, "groups": allowed}));
                } else if id != default_id && non_main_role(&home, &id)
                    && !ctx.owner_only_denied(&id)
                    && crate::machine_employee::check_target(&home, &id, Some(&principal)).is_ok()
                    && org.get(&id).is_some_and(|entry| granted_departments.contains(&entry.department))
                    && authoritative_department(&home, &id).is_none()
                {
                    // Metadata only: never attach the mismatched configuration or department.
                    needs_sync.push(json!({"id": id, "display_name": display_name}));
                }
            }
            manageable.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            needs_sync.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            Ok(json!({"grants": effective_grants(&view), "agents": manageable, "needs_sync": needs_sync}))
        }).await;
        match result {
            Ok(Ok(mine)) => WsFrame::ok_response("", mine),
            _ => WsFrame::error_response("", UNAVAILABLE),
        }
    }
}
