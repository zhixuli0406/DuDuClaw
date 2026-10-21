//! `@` routing for one WebChat message (owner decision D6).
//!
//! WebChat decides the route itself, before the reply pipeline, because it
//! knows the dashboard account and must tell the browser who answers:
//! - a message that calls an employee the account may call goes to that
//!   employee for this message only, even when another employee is selected;
//! - calling an employee the account may not call is refused (error frame,
//!   no model call, audited);
//! - a message that calls nobody goes to the selected employee, else the
//!   default one, but only if the account may use the default employee;
//! - a lone `@` or a partial name answers with the callable list.
//!
//! A public web-widget visitor is treated like an unmapped messaging-app
//! sender: the default employee plus `[channels] public_trigger` employees.

use crate::trigger_route::{self, SenderAccess, TriggerRoute};

use super::{AclSnapshot, ChatMessage, WebChatState, load_acl_snapshot};

/// Error code on a refused `@` call or default-employee refusal.
pub(super) const AGENT_FORBIDDEN_CODE: &str = "agent_forbidden";
/// Error code on a `@` list / ambiguous name answer.
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
            Ok(s) => SenderAccess::from_role(
                auth_user,
                s.role,
                s.bound_agents.iter().cloned().collect(),
                false,
            ),
            Err(e) => {
                tracing::warn!(user = %auth_user, "WebChat ACL lookup failed — no employee callable: {e}");
                SenderAccess {
                    sender: trigger_route::Sender::Account {
                        user_id: auth_user.to_string(),
                        bound: Default::default(),
                    },
                    channel: false,
                }
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
/// `agent: None` = the default path, used only when the default employee IS the
/// main one and nothing was selected (the reply pipeline then resolves the
/// same employee); `agent: Some(id)` = answer as `id`, so the employee checked
/// here is the employee that answers; `Err(frame)` = send that frame and
/// stop (no model call). An empty `text` runs only the default-employee
/// check (used before chat commands).
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
    let fallback: String = selected
        .map(str::to_string)
        .unwrap_or_else(|| default_id.clone());
    let route = trigger_route::decide(&reg, text, &access, Some(&fallback));
    let home = &state.ctx.home_dir;
    let answer_as = |id: String| WebRoute {
        switched: id != fallback,
        agent: (selected.is_some() || id != main_id).then_some(id),
    };
    match route {
        TriggerRoute::None => {
            if let Some(sel) = selected {
                return Ok(WebRoute {
                    agent: Some(sel.to_string()),
                    switched: false,
                });
            }
            let default_ok = reg
                .get(&default_id)
                .is_some_and(|a| access.may_call(a, Some(&default_id)));
            if !default_ok {
                trigger_route::audit(home, "webchat", &access, auth_user, &default_id, "refused");
                return Err(Box::new(ChatMessage::error_coded(
                    trigger_route::DEFAULT_FORBIDDEN_TEXT,
                    AGENT_FORBIDDEN_CODE,
                )));
            }
            Ok(answer_as(default_id))
        }
        TriggerRoute::Routed { agent, .. } => {
            trigger_route::audit(home, "webchat", &access, auth_user, &agent, "allowed");
            Ok(answer_as(agent))
        }
        TriggerRoute::Forbidden { agent } => {
            trigger_route::audit(home, "webchat", &access, auth_user, &agent, "refused");
            Err(Box::new(ChatMessage::error_coded(
                trigger_route::FORBIDDEN_TEXT,
                AGENT_FORBIDDEN_CODE,
            )))
        }
        TriggerRoute::List { candidates, .. } => {
            let agents = trigger_route::list::callable_agents(&reg, &candidates);
            Err(Box::new(ChatMessage::error_coded(
                trigger_route::list::text_list(&agents),
                MENTION_CHOICES_CODE,
            )))
        }
    }
}
