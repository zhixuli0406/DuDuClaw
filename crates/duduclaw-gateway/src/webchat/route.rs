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
//!
//! "May call" for an account is its live effective access (direct bindings
//! and, for a Manager, team grants through `users.manager_user_id`), the
//! same set the `agents.callable` RPC lists for the `@` popup.

use crate::trigger_route::{self, SenderAccess, TriggerRoute};

use super::{AclSnapshot, ChatMessage, WebChatState, load_acl_snapshot};

/// Error code on a refused `@` call (the dashboard renders its own text).
pub(super) const AGENT_FORBIDDEN_CODE: &str = "mention_forbidden";
/// Error code on a `@` list / ambiguous name answer; `choices` carries the
/// `@name` texts (the dashboard renders the heading).
pub(super) const MENTION_CHOICES_CODE: &str = "mention_choices";
/// Most cached ACL snapshots; beyond it stale generations go first, then
/// everything.
const ACL_CACHE_MAX: usize = 4_096;

impl WebChatState {
    /// What the account may call, through the ACL cache. An entry is reused
    /// only while `auth_meta.acl_generation` is unchanged (every binding,
    /// role, status or manager write bumps it) and for `ACL_CACHE_TTL` at
    /// most. When the generation cannot be read the cache is bypassed. A
    /// failed lookup is not cached and returns `Err` (callers deny).
    pub(super) async fn acl_snapshot(&self, user_id: &str) -> Result<AclSnapshot, String> {
        let now = std::time::Instant::now();
        // SQLite reads are blocking: keep them off the runtime workers.
        let db = self.user_db.clone();
        let generation = match tokio::task::spawn_blocking(move || db.acl_generation()).await {
            Ok(Ok(g)) => Some(g),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "WebChat ACL generation unreadable — bypassing the cache");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "WebChat ACL generation task failed — bypassing the cache");
                None
            }
        };
        if let Some(g) = generation {
            let cache = self.acl_cache.lock().await;
            if let Some((snapshot, at)) = cache.get(user_id) {
                if snapshot.generation() == g && now.duration_since(*at) < super::ACL_CACHE_TTL {
                    return Ok(snapshot.clone());
                }
            }
        }
        let (db, home, uid) = (
            self.user_db.clone(),
            self.ctx.home_dir.clone(),
            user_id.to_string(),
        );
        let lookup = tokio::task::spawn_blocking(move || load_acl_snapshot(&home, &db, &uid, generation))
            .await
            .unwrap_or_else(|e| Err(format!("ACL lookup task failed: {e}")));
        if let (Ok(snapshot), Some(g)) = (&lookup, generation) {
            // Cache only a snapshot read at the generation seen before the
            // read; one taken after a concurrent write would carry a newer
            // generation and is simply not cached.
            if snapshot.generation() == g {
                let mut cache = self.acl_cache.lock().await;
                if cache.len() >= ACL_CACHE_MAX && !cache.contains_key(user_id) {
                    cache.retain(|_, (s, _)| s.generation() == g);
                    if cache.len() >= ACL_CACHE_MAX {
                        cache.clear();
                    }
                }
                cache.insert(user_id.to_string(), (snapshot.clone(), now));
            }
        }
        lookup
    }

    /// Who may this connection call?
    pub(super) async fn sender_access(&self, auth_user: &str) -> SenderAccess {
        if auth_user.starts_with("widget-visitor:") {
            return SenderAccess::unmapped();
        }
        match self.acl_snapshot(auth_user).await {
            Ok(s) => SenderAccess::from_effective(s.access),
            Err(e) => {
                tracing::warn!(user = %auth_user, "WebChat ACL lookup failed — only the default employee is callable: {e}");
                SenderAccess::no_bindings(auth_user)
            }
        }
    }
}

impl WebChatState {
    /// May this connection resume a stored session whose employee is
    /// `stored_agent`? The same `SenderAccess::may_call` the `@` route and
    /// `agents.callable` use, with the session's own default employee, so a
    /// conversation started by `@` with a public or default employee resumes
    /// like one with a bound or team employee. Fails closed to "only what
    /// everyone may call" when the account cannot be read.
    pub(super) async fn may_resume_into(
        &self,
        auth_user: &str,
        stored_agent: &str,
        sid: &str,
        user_id: &str,
    ) -> bool {
        let access = self.sender_access(auth_user).await;
        let reg = self.ctx.registry.read().await;
        let default_id = crate::channel_reply::resolve_fallback_agent(&self.ctx, &reg, sid, user_id)
            .await
            .map(|a| a.config.agent.name.clone());
        reg.get(stored_agent)
            .filter(|a| a.config.agent.status.is_operational())
            .is_some_and(|a| access.may_call(a, default_id.as_deref()))
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
        match route_outcome(state, &reg, &access, auth_user, &default_id, route)? {
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
    default_id: &str,
    route: TriggerRoute,
) -> Result<Option<String>, Box<ChatMessage>> {
    let home = &state.ctx.home_dir;
    match route {
        TriggerRoute::None => Ok(None),
        TriggerRoute::Routed { agent, .. } => {
            let via = trigger_route::grant_for(reg, access, &agent, Some(default_id));
            trigger_route::audit(home, "webchat", access, auth_user, &agent, "allowed", via.as_ref());
            Ok(Some(agent))
        }
        TriggerRoute::Forbidden { agent } => {
            trigger_route::audit(home, "webchat", access, auth_user, &agent, "refused", None);
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

/// When an armed `/交辦 @name` is waiting for this account's next message
/// in this conversation and `text` calls nobody, turn `text` into that task
/// and return the reply frame. A message that calls someone cancels the
/// wait (it is the next message, and it was not the task). `None` = normal
/// routing.
#[allow(clippy::too_many_arguments)]
pub(super) async fn pending_assign_reply(
    state: &WebChatState,
    auth_user: &str,
    text: &str,
    selected: Option<&str>,
    main_id: &str,
    sid: &str,
    user_id: &str,
    conv: Option<String>,
    assign_event: Option<&str>,
) -> Option<ChatMessage> {
    use crate::trigger_route::pending::{self, PendingKey, PendingKind};
    if super::is_widget_visitor(auth_user) {
        return None;
    }
    let key = PendingKey::new("webchat", sid, auth_user);
    let seq = pending::next_seq();
    let event = uuid::Uuid::new_v4().to_string();
    if !matches!(pending::peek_kind(&key, seq), Some((_, PendingKind::Assign))) {
        return None;
    }
    let access = state.sender_access(auth_user).await;
    let (route, default_id) = {
        let reg = state.ctx.registry.read().await;
        let default_id = crate::channel_reply::resolve_fallback_agent(&state.ctx, &reg, sid, user_id)
            .await
            .map(|a| a.config.agent.name.clone())
            .unwrap_or_else(|| main_id.to_string());
        (trigger_route::decide(&reg, text, &access, Some(&default_id)), default_id)
    };
    if route != TriggerRoute::None {
        pending::cancel(&key, &event, seq);
        return None;
    }
    let agent = pending::claim_kind(&key, &event, seq, PendingKind::Assign)?;
    let fallback = selected.unwrap_or(&default_id);
    let caller = crate::trigger_route::assign::AssignCaller {
        home: &state.ctx.home_dir,
        channel: "webchat",
        conversation: sid,
        sender: auth_user,
        event: assign_event,
        seq,
        fallback: Some(fallback),
        access,
    };
    let reply =
        Box::pin(crate::trigger_route::assign::run_pending(&state.ctx, &agent, text, &caller)).await;
    Some(ChatMessage::AssistantDone {
        content: reply.text,
        tokens_used: 0,
        conv,
        model: None,
        artifact: None,
        agent: None,
        agent_name: None,
        citations: None,
        thinking: None,
        task: reply
            .task
            .map(|t| serde_json::json!({ "id": t.id, "agent": t.agent })),
    })
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
