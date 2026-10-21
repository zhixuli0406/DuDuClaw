//! `@` routing for one WebChat message (owner decision D6).
//!
//! WebChat decides the route itself, before the reply pipeline, because it
//! knows the dashboard account and must tell the browser who answers:
//! - a message that calls an employee the account may call goes to that
//!   employee for this message only, even when another employee is selected;
//! - calling an employee the account may not call is refused (error frame,
//!   no model call, audited);
//! - a message that calls nobody goes to the selected employee, else the
//!   default one (reachable by every signed-in account, as before);
//! - a lone `@` or a partial name answers with the callable list.
//!
//! A public web-widget visitor is treated like an unmapped messaging-app
//! sender: the default employee plus `[channels] public_trigger` employees.

use crate::trigger_route::{self, SenderAccess, TriggerRoute};

use super::{AclSnapshot, ChatMessage, WebChatState, load_acl_snapshot};

/// Error code on a refused `@` call (the dashboard renders its own text).
pub(super) const AGENT_FORBIDDEN_CODE: &str = "mention_forbidden";
/// Error code on a `@` list / ambiguous name answer; `choices` carries the
/// `@name` texts (the dashboard renders the heading).
pub(super) const MENTION_CHOICES_CODE: &str = "mention_choices";

impl WebChatState {
    /// The account's role and bindings, through the short-TTL ACL cache. A
    /// failed lookup is not cached and returns `Err` (callers deny).
    pub(super) async fn acl_snapshot(&self, user_id: &str) -> Result<AclSnapshot, String> {
        let now = std::time::Instant::now();
        {
            let cache = self.acl_cache.lock().await;
            if let Some((snapshot, at)) = cache.get(user_id) {
                if now.duration_since(*at) < super::ACL_CACHE_TTL {
                    return Ok(snapshot.clone());
                }
            }
        }
        let lookup = load_acl_snapshot(&self.user_db, user_id);
        if let Ok(snapshot) = &lookup {
            let mut cache = self.acl_cache.lock().await;
            cache.insert(user_id.to_string(), (snapshot.clone(), now));
        }
        lookup
    }

    /// Who may this connection call?
    pub(super) async fn sender_access(&self, auth_user: &str) -> SenderAccess {
        if auth_user.starts_with("widget-visitor:") {
            return SenderAccess::unmapped();
        }
        match self.acl_snapshot(auth_user).await {
            Ok(s) => {
                SenderAccess::from_role(auth_user, s.role, s.bound_agents.iter().cloned().collect())
            }
            Err(e) => {
                tracing::warn!(user = %auth_user, "WebChat ACL lookup failed — only the default employee is callable: {e}");
                SenderAccess::no_bindings(auth_user)
            }
        }
    }
}

/// Who answers one WebChat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WebRoute {
    /// `None` = the default path (the main employee, nothing selected).
    pub agent: Option<String>,
    /// The message called someone other than the selected / default
    /// employee: the reply goes to that employee's own session bucket.
    pub switched: bool,
}

/// Decide which employee answers `text`. `selected` is the employee the
/// browser addressed (already ACL-checked), `main_id` the main employee,
/// `sid`/`user_id` the turn's session and connection ids (they decide the
/// conversation's real default employee, which may be a channel-bound or
/// `allowed_channels` employee rather than the main one).
///
/// The default employee stays reachable by every signed-in account, exactly
/// as before `@` routing (the C-ACL decision on `resume_needs_acl_check`);
/// only calling *another* employee by `@` needs a binding.
/// `agent: None` = the default path (the main employee, nothing selected);
/// `agent: Some(id)` = answer as `id`; `Err(frame)` = send that frame and
/// stop (no model call).
pub(super) async fn decide_webchat_route(
    state: &WebChatState,
    auth_user: &str,
    text: &str,
    selected: Option<&str>,
    main_id: &str,
    sid: &str,
    user_id: &str,
) -> Result<WebRoute, Box<ChatMessage>> {
    let access = state.sender_access(auth_user).await;
    let reg = state.ctx.registry.read().await;
    let default_id = crate::channel_reply::resolve_fallback_agent(&state.ctx, &reg, sid, user_id)
        .await
        .map(|a| a.config.agent.name.clone())
        .unwrap_or_else(|| main_id.to_string());
    let current: String = selected
        .map(str::to_string)
        .unwrap_or_else(|| default_id.clone());
    let route = trigger_route::decide(&reg, text, &access, Some(&default_id));
    Ok(
        match route_outcome(state, &reg, &access, auth_user, route)? {
            None => WebRoute {
                agent: selected
                    .map(str::to_string)
                    .or_else(|| (default_id != main_id).then(|| default_id.clone())),
                switched: false,
            },
            Some(id) => WebRoute {
                switched: id != current,
                agent: (selected.is_some() || id != main_id).then_some(id),
            },
        },
    )
}

/// `Ok(None)` = calls nobody; `Ok(Some(id))` = calls `id` (allowed);
/// `Err(frame)` = refusal or list.
fn route_outcome(
    state: &WebChatState,
    reg: &duduclaw_agent::registry::AgentRegistry,
    access: &SenderAccess,
    auth_user: &str,
    route: TriggerRoute,
) -> Result<Option<String>, Box<ChatMessage>> {
    let home = &state.ctx.home_dir;
    match route {
        TriggerRoute::None => Ok(None),
        TriggerRoute::Routed { agent, .. } => {
            trigger_route::audit(home, "webchat", access, auth_user, &agent, "allowed");
            Ok(Some(agent))
        }
        TriggerRoute::Forbidden { agent } => {
            trigger_route::audit(home, "webchat", access, auth_user, &agent, "refused");
            Err(Box::new(ChatMessage::error_coded(
                trigger_route::FORBIDDEN_TEXT,
                AGENT_FORBIDDEN_CODE,
            )))
        }
        TriggerRoute::List { candidates, .. } => {
            let agents = trigger_route::list::callable_agents(reg, &candidates);
            Err(Box::new(ChatMessage::error_choices(
                trigger_route::list::text_list(&agents, trigger_route::texts::Lang::ZhTw),
                MENTION_CHOICES_CODE,
                agents.into_iter().map(|a| a.call_text).collect(),
            )))
        }
    }
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
