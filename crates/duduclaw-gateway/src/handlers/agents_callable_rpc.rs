//! `agents.callable`: the AI employees the caller may call from WebChat,
//! with where each right comes from.
//!
//! This is the WebChat `@` list and staff picker. It is computed with the
//! same predicate the WebChat router applies to every message
//! (`trigger_route::callable_grants` over the live identity), so anything
//! listed can be called and anything refused is not listed:
//! - Admin: every operational employee (`via: "admin"`);
//! - a direct binding at any level (`via: "binding"`);
//! - for a Manager, an employee bound to someone below them in the
//!   reporting tree (`via: "team"`, at most Operator, with the report it
//!   comes through);
//! - WebChat's default employee (`via: "default"`) and `[channels]
//!   public_trigger` employees (`via: "public"`), open to every account.
//!
//! `ctx` is the live context `dispatch` rebuilt from users.db for this call,
//! so a binding, role or manager change shows up without a re-login.
//! `selectable` marks the entries an explicit WebChat selection accepts
//! (admin, binding, team); the default employee is reached by selecting
//! nobody, and a public employee only by `@`. `assignable` is
//! `UserContext::can_assign_to` (Operator, directly or through a report).
//! This RPC never widens memory, history or settings access: those RPCs
//! keep checking direct bindings only.

use std::collections::HashMap;
use std::sync::Arc;

use duduclaw_auth::{AccessLevel, EffectiveAccess, GrantSource, UserContext, UserDb};
use serde_json::{Value, json};

use super::MethodHandler;
use crate::protocol::WsFrame;
use crate::trigger_route::{self, SenderAccess};

/// WebChat's session prefix: without a `session_id` the default employee is
/// resolved for this channel the way `webchat/route.rs` resolves it for a
/// turn in a fresh conversation.
const WEBCHAT_SESSION: &str = "webchat:webchat";

/// Longest `session_id` accepted (a WebChat session id is far shorter).
const MAX_SESSION_ID: usize = 256;

/// The session the default employee is resolved for: the caller's current
/// WebChat conversation when given (`session_id`, the id the chat socket
/// announced), else a fresh WebChat conversation. Only `webchat:` sessions
/// are accepted: the parameter only picks which employee counts as
/// "default" in the list, and the router recomputes it from the real
/// session on every message, so a forged value can at most mislabel the
/// caller's own list, never grant a call.
fn session_for(params: &Value) -> String {
    params
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| s.starts_with("webchat:") && s.len() <= MAX_SESSION_ID)
        .map(str::to_string)
        .unwrap_or_else(|| WEBCHAT_SESSION.to_string())
}

/// The live context as the router's [`EffectiveAccess`].
fn effective_of(ctx: &UserContext) -> EffectiveAccess {
    EffectiveAccess {
        user_id: ctx.user_id.clone(),
        role: ctx.role,
        direct: ctx.agent_access.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        team: ctx.team_access.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        generation: ctx.acl_generation,
    }
}

fn level_name(level: AccessLevel) -> &'static str {
    match level {
        AccessLevel::Owner => "owner",
        AccessLevel::Operator => "operator",
        AccessLevel::Viewer => "viewer",
    }
}

/// `via`, the report it comes through (team only), its depth, and the
/// access level the grant carries (none for default / public).
fn grant_fields(source: &GrantSource, access: &EffectiveAccess, agent: &str) -> Value {
    let level = access.level(agent).map(|(l, _)| level_name(l));
    match source {
        GrantSource::Admin => json!({ "via": "admin", "level": "owner", "selectable": true }),
        GrantSource::Direct(l) => {
            json!({ "via": "binding", "level": level_name(*l), "selectable": true })
        }
        GrantSource::Team { via_user, depth } => json!({
            "via": "team",
            "via_user": via_user,
            "depth": depth,
            "level": level,
            "selectable": true,
        }),
        GrantSource::Default => json!({ "via": "default", "level": Value::Null, "selectable": false }),
        GrantSource::Public => json!({ "via": "public", "level": Value::Null, "selectable": false }),
    }
}

/// Display names of the reports team grants come through. An unreadable
/// row just leaves the name out (the id is still there).
fn report_names(db: Option<&UserDb>, grants: &[(String, GrantSource)]) -> HashMap<String, String> {
    let Some(db) = db else {
        return HashMap::new();
    };
    let mut names = HashMap::new();
    for (_, source) in grants {
        if let GrantSource::Team { via_user, .. } = source {
            if names.contains_key(via_user) {
                continue;
            }
            match db.get_user(via_user) {
                Ok(Some(u)) => {
                    names.insert(via_user.clone(), u.display_name);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "agents.callable: report name unreadable"),
            }
        }
    }
    names
}

impl MethodHandler {
    pub(crate) async fn handle_agents_callable(&self, ctx: &UserContext, params: Value) -> WsFrame {
        let access = Arc::new(effective_of(ctx));
        let sender = SenderAccess::from_effective(access.clone());
        let reg = self.registry.read().await;
        let session = session_for(&params);
        let default_id = crate::channel_reply::resolve_fallback_agent_in(
            &self.home_dir,
            &reg,
            &session,
            &ctx.user_id,
        )
        .await
        .map(|a| a.config.agent.name.clone());
        let grants = trigger_route::callable_grants(&reg, &sender, default_id.as_deref());
        let ids: Vec<String> = grants.iter().map(|(id, _)| id.clone()).collect();
        let entries = trigger_route::list::callable_agents(&reg, &ids);
        let db = self.user_db.read().await.clone();
        let names = report_names(db.as_deref(), &grants);

        let agents: Vec<Value> = grants
            .iter()
            .zip(entries.iter())
            .filter_map(|((id, source), entry)| {
                let a = reg.get(id)?;
                let cfg = &a.config.agent;
                let mut v = json!({
                    "name": cfg.name,
                    "display_name": entry.display_name,
                    "call_text": entry.call_text,
                    "department": cfg.department,
                    "icon": cfg.icon,
                    "has_avatar": self.agent_has_avatar(&cfg.name),
                    "trigger": cfg.trigger,
                    "role": format!("{:?}", cfg.role).to_lowercase(),
                    "status": format!("{:?}", cfg.status).to_lowercase(),
                    "assignable": ctx.can_assign_to(id),
                    "manager_may_read": db.as_deref().is_some_and(|db| crate::team_oversight::manager_may_read(&self.home_dir,db,ctx,id)),
                });
                if let (Value::Object(out), Value::Object(extra)) =
                    (&mut v, grant_fields(source, &access, id))
                {
                    out.extend(extra);
                    if let GrantSource::Team { via_user, .. } = source {
                        if let Some(name) = names.get(via_user) {
                            out.insert("via_user_name".into(), json!(name));
                        }
                    }
                }
                Some(v)
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({ "agents": agents, "default_agent": default_id }),
        )
    }
}
