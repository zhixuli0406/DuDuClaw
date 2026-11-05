//! WebChat routing: dashboard accounts may call their authorized non-main
//! employees; admins keep the default path. Anonymous widgets keep their
//! server-selected conversation partner and cannot choose an employee.

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
    /// most, and only while its edition epoch is current. When the generation
    /// cannot be read the cache is bypassed. A
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
                if snapshot.generation() == g && snapshot.edition.is_current()
                    && now.duration_since(*at) < super::ACL_CACHE_TTL {
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
            if snapshot.generation() == g && snapshot.edition.is_current() {
                let mut cache = self.acl_cache.lock().await;
                if cache.len() >= ACL_CACHE_MAX && !cache.contains_key(user_id) {
                    cache.retain(|_, (s, _)| s.generation() == g && s.edition.is_current());
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
        if super::is_widget_visitor(auth_user) {
            return SenderAccess::unmapped();
        }
        match self.acl_snapshot(auth_user).await {
            Ok(s) => SenderAccess::from_effective(s.access)
                .for_webchat(super::access::gateway_default_id(&self.ctx.home_dir).await),
            Err(e) => {
                tracing::warn!(user = %auth_user, "WebChat ACL lookup failed — denying calls: {e}");
                SenderAccess::no_bindings(auth_user).for_webchat(Err(e))
            }
        }
    }
}

impl WebChatState {
    /// May this connection resume a stored session whose employee is
    /// `stored_agent`? The same `SenderAccess::may_call` the `@` route and
    /// `agents.callable` use, with the session's own default employee, so a
    /// conversation started by `@` with a public or default employee resumes
    /// like one with a bound or team employee. An unreadable account or a
    /// non-admin main-agent target is refused before considering public grants.
    pub(super) async fn may_resume_into(
        &self,
        auth_user: &str,
        stored_agent: &str,
        sid: &str,
        user_id: &str,
    ) -> bool {
        if self.authorize_target(auth_user, stored_agent, true).await.is_err() {
            return false;
        }
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
    /// The message used an allowed `@` call, including the selected employee.
    pub called: bool,
}

/// Decide which employee answers `text`. `selected` is the employee the
/// browser addressed (already ACL-checked), `main_id` the main employee,
/// `sid`/`user_id` the turn's session and connection ids (they decide the
/// conversation's real default employee, which may be a channel-bound or
/// `allowed_channels` employee rather than the main one).
///
/// Non-admin default selection on every edition is resolved from the account, never main.
/// The same WebChat-scoped grants filter mentions and assignment commands.
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
    if super::is_widget_visitor(auth_user) {
        return Ok(WebRoute {
            agent: (default_id != main_id).then_some(default_id),
            switched: false,
            called: false,
        });
    }
    let current: String = selected
        .map(str::to_string)
        .unwrap_or_else(|| default_id.clone());
    let route = decide_private_webchat_route(&reg, text, &access, &default_id);
    let outcome = match route_outcome(state, &reg, &access, auth_user, &default_id, route)? {
            None => WebRoute {
                agent: selected
                    .map(str::to_string)
                    .or_else(|| (default_id != main_id).then(|| default_id.clone())),
                switched: false,
                called: false,
            },
            Some(id) => WebRoute {
                switched: id != current,
                called: true,
                agent: (selected.is_some() || id != main_id).then_some(id),
            },
        };
    drop(reg);
    let mut outcome = outcome;
    if outcome.agent.is_none() && !outcome.called {
        match state.resolve_new_agent(auth_user).await {
            Ok(Some(id)) => outcome.agent = Some(id),
            Ok(None) => {},
            Err(reason) => return Err(Box::new(super::access::assignment_error(reason))),
        }
    }
    state.authorize_target(auth_user, outcome.agent.as_deref().unwrap_or(main_id), true)
        .await.map_err(Box::new)?;
    Ok(outcome)
}

/// Hide inaccessible names from matching itself, not only from the error
/// string. Otherwise `@private @` and `@missing @` differ (refusal vs list),
/// and an armed assignment may consume an unknown mention as its task body.
fn decide_private_webchat_route(
    reg: &duduclaw_agent::registry::AgentRegistry,
    text: &str,
    access: &SenderAccess,
    default_id: &str,
) -> TriggerRoute {
    if matches!(access.sender, trigger_route::Sender::Admin { .. })
        || crate::chat_commands::is_command(text)
    {
        return trigger_route::decide(reg, text, access, Some(default_id));
    }
    use duduclaw_agent::trigger_match::{self, MentionScan};
    let targets: Vec<_> = trigger_route::mention_targets(reg).into_iter()
        .filter(|target| reg.get(&target.id).is_some_and(|a| access.may_call(a, Some(default_id))))
        .collect();
    match trigger_match::scan_mentions(text, &targets, &|_| true) {
        MentionScan::Hit(hit) => TriggerRoute::Routed {
            bare: trigger_match::task_text_without(text, &hit).is_empty(), agent: hit.id,
        },
        MentionScan::Partial { token, candidates } => TriggerRoute::List { token, candidates },
        _ if trigger_match::has_named_mention(text) => {
            // Retain an exact matched reason only in the server-side audit.
            match trigger_route::decide(reg, text, access, Some(default_id)) {
                refused @ TriggerRoute::Forbidden { .. } => refused,
                _ => TriggerRoute::Forbidden { agent: "<unknown>".into() },
            }
        }
        _ => TriggerRoute::None,
    }
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
            Err(Box::new(if matches!(access.sender, trigger_route::Sender::Admin { .. }) {
                ChatMessage::error_coded(trigger_route::FORBIDDEN_TEXT, AGENT_FORBIDDEN_CODE)
            } else {
                super::access::agent_unavailable()
            }))
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
    seq: i64,
) -> Option<ChatMessage> {
    use crate::trigger_route::pending::{self, PendingKey, PendingKind};
    if super::is_widget_visitor(auth_user) {
        return None;
    }
    let key = PendingKey::new("webchat", sid, auth_user);
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
        (decide_private_webchat_route(&reg, text, &access, &default_id), default_id)
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
