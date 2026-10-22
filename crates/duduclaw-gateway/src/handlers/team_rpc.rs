//! `users.my_team`: the dashboard 我的團隊 page.
//!
//! The caller's reporting tree (everyone below them, at most
//! [`MAX_ORG_DEPTH`] levels, Offboarded excluded, Suspended kept: Q3), and
//! for each person the AI employees bound to them that the caller may call
//! through that person, with the level the manager derives
//! (`min(binding, Operator)`), whether the caller may hand the employee a
//! task, and how many tasks it has open.
//!
//! Computed live from users.db with the same traversal the team grants use
//! (`UserDb::team_members` and `org::derived_team_level` share `TEAM_CTE`
//! with `team_grants_on`), and every employee is filtered through the live
//! context's `can_call_agent`, so anything listed here can be called and
//! nothing else is. It never widens memory, history or settings access.
//!
//! Gates (in `dispatch_org.rs` / `gates.rs`): the `users` family is
//! Enterprise-only, so a Personal install refuses it before this runs, and
//! `require_manager!` refuses an Employee.

use std::collections::HashMap;

use duduclaw_auth::org::derived_team_level;
use duduclaw_auth::{AccessLevel, MAX_ORG_DEPTH, User, UserAgentBinding, UserContext, UserDb};
use serde_json::{Value, json};

use super::MethodHandler;
use crate::protocol::WsFrame;
use crate::trigger_route;
use duduclaw_agent::registry::AgentRegistry;

/// Plain-language refusal when the team cannot be read (detail goes to the
/// log, never to the client).
const TEAM_UNREADABLE: &str = "目前無法讀取團隊資料，請稍後再試。";

fn level_name(level: AccessLevel) -> &'static str {
    match level {
        AccessLevel::Owner => "owner",
        AccessLevel::Operator => "operator",
        AccessLevel::Viewer => "viewer",
    }
}

/// One person in the tree with the bindings read for them.
struct Member {
    user: User,
    depth: u8,
    bindings: Vec<UserAgentBinding>,
}

/// Read the tree and each member's bindings in one blocking task. Any read
/// error refuses the whole call: a partial team would look complete.
fn read_team(db: &UserDb, manager_id: &str) -> Result<Vec<Member>, String> {
    let members = db.team_members(manager_id, MAX_ORG_DEPTH)?;
    members
        .into_iter()
        .map(|(user, depth)| {
            let bindings = db.get_user_agents(&user.id)?;
            Ok(Member {
                user,
                depth,
                bindings,
            })
        })
        .collect()
}

/// The employee rows for one member: only employees that exist, are listed
/// (not deleted or archived) and that `ctx` may call.
fn agent_rows(
    handler: &MethodHandler,
    reg: &AgentRegistry,
    ctx: &UserContext,
    bindings: &[UserAgentBinding],
    open: &HashMap<String, i64>,
    counts_known: bool,
) -> Vec<Value> {
    let ids: Vec<String> = bindings
        .iter()
        .filter(|b| ctx.can_call_agent(&b.agent_name))
        .filter(|b| {
            reg.get(&b.agent_name)
                .is_some_and(|a| a.config.agent.status.is_listable(false))
        })
        .map(|b| b.agent_name.clone())
        .collect();
    let entries = trigger_route::list::callable_agents(reg, &ids);
    entries
        .iter()
        .filter_map(|entry| {
            let agent = reg.get(&entry.id)?;
            let binding = bindings.iter().find(|b| b.agent_name == entry.id)?;
            let operational = agent.config.agent.status.is_operational();
            Some(json!({
                "name": entry.id,
                "display_name": entry.display_name,
                "call_text": entry.call_text,
                "icon": entry.icon,
                "has_avatar": handler.agent_has_avatar(&entry.id),
                "status": format!("{:?}", agent.config.agent.status).to_lowercase(),
                "operational": operational,
                "level": level_name(derived_team_level(binding.access_level)),
                "assignable": operational && ctx.can_assign_to(&entry.id),
                "active_tasks": if counts_known {
                    json!(open.get(&entry.id).copied().unwrap_or(0))
                } else {
                    Value::Null
                },
            }))
        })
        .collect()
}

impl MethodHandler {
    pub(crate) async fn handle_users_my_team(&self, ctx: &UserContext) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "user system not initialized");
        };
        let manager_id = ctx.user_id.clone();
        let read = {
            let db = db.clone();
            let id = manager_id.clone();
            tokio::task::spawn_blocking(move || read_team(&db, &id)).await
        };
        let members = match read {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "users.my_team: team unreadable");
                return WsFrame::error_response("", TEAM_UNREADABLE);
            }
            Err(e) => {
                tracing::warn!(error = %e, "users.my_team: read task failed");
                return WsFrame::error_response("", TEAM_UNREADABLE);
            }
        };

        // Open-task counts are a convenience: without a task store (or on a
        // read error) the page shows the team without them.
        let all_agents: Vec<String> = members
            .iter()
            .flat_map(|m| m.bindings.iter().map(|b| b.agent_name.clone()))
            .collect();
        let (open, counts_known) = match self.task_store().await {
            Ok(store) => match store.open_task_counts(&all_agents).await {
                Ok(c) => (c, true),
                Err(e) => {
                    tracing::warn!(error = %e, "users.my_team: open task counts unreadable");
                    (HashMap::new(), false)
                }
            },
            Err(_) => (HashMap::new(), false),
        };

        let reg = self.registry.read().await;
        let rows: Vec<Value> = members
            .iter()
            .map(|m| {
                json!({
                    "id": m.user.id,
                    "display_name": m.user.display_name,
                    "role": m.user.role,
                    "status": m.user.status,
                    "department": m.user.department,
                    "manager_user_id": m.user.manager_user_id,
                    "depth": m.depth,
                    "agents": agent_rows(self, &reg, ctx, &m.bindings, &open, counts_known),
                })
            })
            .collect();
        let me = db
            .get_user(&manager_id)
            .ok()
            .flatten()
            .map(|u| json!({ "id": u.id, "display_name": u.display_name }));
        WsFrame::ok_response(
            "",
            json!({
                "manager": me,
                "max_depth": MAX_ORG_DEPTH,
                "members": rows,
            }),
        )
    }
}
