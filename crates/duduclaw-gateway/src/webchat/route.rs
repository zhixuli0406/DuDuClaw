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

/// Decide which employee answers `text`. `selected` is the employee the
/// browser addressed (already ACL-checked), `default_id` the default one.
/// `Ok(None)` = the default path; `Ok(Some(id))` = answer as `id`;
/// `Err(frame)` = send that frame and stop (no model call).
pub(super) async fn decide_webchat_route(
    state: &WebChatState,
    auth_user: &str,
    text: &str,
    selected: Option<&str>,
    default_id: &str,
) -> Result<Option<String>, Box<ChatMessage>> {
    let access = state.sender_access(auth_user).await;
    let reg = state.ctx.registry.read().await;
    let fallback = selected.unwrap_or(default_id);
    let route = trigger_route::decide(&reg, text, &access, Some(fallback));
    let home = &state.ctx.home_dir;
    match route {
        TriggerRoute::None => {
            if selected.is_none() {
                let default_ok = reg
                    .get(default_id)
                    .is_some_and(|a| access.may_call(a, Some(default_id)));
                if !default_ok {
                    trigger_route::audit(
                        home, "webchat", &access, auth_user, default_id, "refused",
                    );
                    return Err(Box::new(ChatMessage::error_coded(
                        trigger_route::DEFAULT_FORBIDDEN_TEXT,
                        AGENT_FORBIDDEN_CODE,
                    )));
                }
            }
            Ok(selected.map(str::to_string))
        }
        TriggerRoute::Routed { agent, .. } => {
            trigger_route::audit(home, "webchat", &access, auth_user, &agent, "allowed");
            if selected.is_none() && agent == default_id {
                return Ok(None);
            }
            Ok(Some(agent))
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
