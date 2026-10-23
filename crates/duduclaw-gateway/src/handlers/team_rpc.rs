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

use std::collections::{BTreeSet, HashMap};

use duduclaw_auth::org::derived_team_level;
use duduclaw_auth::{AccessLevel, MAX_ORG_DEPTH, User, UserAgentBinding, UserContext, UserDb};
use serde_json::{Value, json};

use super::MethodHandler;
use crate::protocol::WsFrame;
use crate::task_store::MAX_COUNTED_AGENTS;
use crate::trigger_route;
use duduclaw_agent::registry::AgentRegistry;

/// Plain-language refusal when the team cannot be read (detail goes to the
/// log, never to the client).
const TEAM_UNREADABLE: &str = "目前無法讀取團隊資料，請稍後再試。";

type HistoryFlags = HashMap<String, BTreeSet<String>>;

/// Advisory buttons use the same live gate as actual disclosure. An audit or
/// window failure hides every button, while the ordinary callable team remains
/// available. All database work runs with the existing blocking team read.
fn history_flags(home: &std::path::Path, db: &UserDb, ctx: &UserContext, members: &[Member]) -> (Option<bool>, HistoryFlags) {
    use crate::team_oversight::{self, OversightDenied};
    let Ok(window) = team_oversight::observe_window(home, db, "gateway") else {
        return (None, HashMap::new());
    };
    if !window.enabled {
        return (Some(false), HashMap::new());
    }
    let agents: BTreeSet<_> = members.iter().flat_map(|m| &m.bindings)
        .filter(|b| ctx.can_call_agent(&b.agent_name)).map(|b| b.agent_name.clone()).collect();
    if agents.len() > MAX_COUNTED_AGENTS {
        return (None, HashMap::new());
    }
    let mut readable = HashMap::new();
    for agent in agents {
        match team_oversight::can_view_team_history(home, db, ctx, &agent) {
            Ok(scope) => { readable.insert(agent, scope.reports.into_keys().collect()); }
            Err(OversightDenied::ReadError) => return (None, HashMap::new()),
            Err(_) => {}
        }
    }
    (Some(true), readable)
}

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

/// The caller's own row and the tree with each member's bindings, read in
/// one blocking task. Any read error refuses the whole call: a partial team
/// would look complete.
///
/// One binding query per member (N+1): `UserDb` has no batch binding read
/// yet, and the tree is bounded by the org depth and the size of a team.
fn read_team(db: &UserDb, manager_id: &str) -> Result<(Option<User>, Vec<Member>), String> {
    let me = db.get_user(manager_id)?;
    let members = db
        .team_members(manager_id, MAX_ORG_DEPTH)?
        .into_iter()
        .map(|(user, depth)| {
            let bindings = db.get_user_agents(&user.id)?;
            Ok(Member {
                user,
                depth,
                bindings,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((me, members))
}

/// The employees of one member that may be listed: they exist, are listed
/// (not deleted or archived) and `ctx` may call them.
fn listed_ids(
    reg: &AgentRegistry,
    ctx: &UserContext,
    bindings: &[UserAgentBinding],
) -> Vec<String> {
    bindings
        .iter()
        .filter(|b| ctx.can_call_agent(&b.agent_name))
        .filter(|b| {
            reg.get(&b.agent_name)
                .is_some_and(|a| a.config.agent.status.is_listable(false))
        })
        .map(|b| b.agent_name.clone())
        .collect()
}

/// Open-task counts for exactly the employees shown, or `None` when they
/// cannot be given honestly (no task store, a read error, or more
/// employees than one count covers): the page then shows no number rather
/// than a zero that only looks known.
async fn open_counts(handler: &MethodHandler, agents: &[String]) -> Option<HashMap<String, i64>> {
    if agents.len() > MAX_COUNTED_AGENTS {
        tracing::warn!(
            agents = agents.len(),
            "users.my_team: too many employees to count open tasks"
        );
        return None;
    }
    let store = handler.task_store().await.ok()?;
    match store.open_task_counts(agents).await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, "users.my_team: open task counts unreadable");
            None
        }
    }
}

/// The employee rows for one member (`ids` from [`listed_ids`]).
fn agent_rows(
    handler: &MethodHandler,
    reg: &AgentRegistry,
    ctx: &UserContext,
    bindings: &[UserAgentBinding],
    ids: &[String],
    open: Option<&HashMap<String, i64>>,
    history: &HistoryFlags,
    report_id: &str,
) -> Vec<Value> {
    let entries = trigger_route::list::callable_agents(reg, ids);
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
                "history_readable": history.get(&entry.id).is_some_and(|reports| reports.contains(report_id)),
                "active_tasks": open
                    .map(|c| json!(c.get(&entry.id).copied().unwrap_or(0)))
                    .unwrap_or(Value::Null),
            }))
        })
        .collect()
}

impl MethodHandler {
    pub(crate) async fn handle_users_my_team(&self, ctx: &UserContext) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "user system not initialized");
        };
        let read = {
            let id = ctx.user_id.clone();
            let home = self.home_dir.clone();
            let ctx = ctx.clone();
            tokio::task::spawn_blocking(move || {
                read_team(&db, &id).map(|(me, members)| {
                    let (history_enabled, history) = history_flags(&home, &db, &ctx, &members);
                    (me, members, history_enabled, history)
                })
            }).await
        };
        let (me, members, history_enabled, history) = match read {
            Ok(Ok(team)) => team,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "users.my_team: team unreadable");
                return WsFrame::error_response("", TEAM_UNREADABLE);
            }
            Err(e) => {
                tracing::warn!(error = %e, "users.my_team: read task failed");
                return WsFrame::error_response("", TEAM_UNREADABLE);
            }
        };

        let reg = self.registry.read().await;
        let listed: Vec<Vec<String>> = members
            .iter()
            .map(|m| listed_ids(&reg, ctx, &m.bindings))
            .collect();
        let mut shown: Vec<String> = listed.iter().flatten().cloned().collect();
        shown.sort();
        shown.dedup();
        let open = open_counts(self, &shown).await;

        let rows: Vec<Value> = members
            .iter()
            .zip(listed.iter())
            .map(|(m, ids)| {
                json!({
                    "id": m.user.id,
                    "display_name": m.user.display_name,
                    "role": m.user.role,
                    "status": m.user.status,
                    "department": m.user.department,
                    "manager_user_id": m.user.manager_user_id,
                    "depth": m.depth,
                    "agents": agent_rows(self, &reg, ctx, &m.bindings, ids, open.as_ref(), &history, &m.user.id),
                })
            })
            .collect();
        let me = me.map(|u| json!({ "id": u.id, "display_name": u.display_name }));
        WsFrame::ok_response(
            "",
            json!({
                "manager": me,
                "max_depth": MAX_ORG_DEPTH,
                "members": rows,
                "history_enabled": history_enabled,
            }),
        )
    }
}
