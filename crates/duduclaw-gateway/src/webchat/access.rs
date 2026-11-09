//! Dashboard-only conversation routing. Anonymous widgets retain their public route.
use super::*;
use crate::trigger_route::SenderAccess;

mod delivery;
pub(crate) use delivery::TargetPolicy;

/// Read the configured gateway default separately from channel fallbacks.
/// An unreadable configuration cannot establish that a target is non-main.
pub(crate) async fn gateway_default_id(home: &std::path::Path) -> Result<String, String> {
    let raw = match tokio::fs::read_to_string(home.join("config.toml")).await {
        Ok(raw) => raw,
        Err(e) => return Err(e.to_string()),
    };
    parse_gateway_default(&raw)
}

fn parse_gateway_default(raw: &str) -> Result<String, String> {
    let config: toml::Value = raw.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let general = match config.get("general") {
        None => return Ok(String::new()),
        Some(value) => value.as_table().ok_or_else(|| "invalid general configuration".to_owned())?,
    };
    match general.get("default_agent") {
        None => Ok(String::new()),
        Some(value) => value.as_str().map(str::to_owned).ok_or_else(|| "invalid default agent".into()),
    }
}

pub(super) fn assignment_error(reason: &str) -> ChatMessage {
    let mut frame = ChatMessage::error_coded(match reason {
        "multiple_agents" => "你有多位可用的 AI 員工，請在側欄「AI 員工」的選單中設為主要；若看不到此選項，請聯絡管理員。",
        "unavailable" => "目前無法確認你的對話權限，請稍後再試。",
        _ => "尚未指派可對話的 AI 員工，請聯絡管理員。",
    }, "no_assigned_agent");
    if let ChatMessage::Error { reason: out, .. } = &mut frame { *out = Some(reason.to_owned()); }
    frame
}

pub(crate) const AGENT_UNAVAILABLE_TEXT: &str = "此 AI 員工目前無法使用，請重新選擇或聯絡管理員。";

pub(super) fn agent_unavailable() -> ChatMessage {
    ChatMessage::error_coded(AGENT_UNAVAILABLE_TEXT, "agent_unavailable")
}

impl WebChatState {
    /// None preserves the existing admin/widget default path byte for byte.
    /// P1h (N1): `None` picks no target by itself — the caller
    /// (`route::decide_webchat_route`) still runs `authorize_target` on the default
    /// it falls back to, whose Personal check precedes the Admin shortcut.
    pub(super) async fn resolve_new_agent(&self, user: &str) -> Result<Option<String>, &'static str> {
        if is_widget_visitor(user) { return Ok(None); }
        let snapshot = self.acl_snapshot(user).await.map_err(|_| "unavailable")?;
        if snapshot.role() == duduclaw_auth::UserRole::Admin { return Ok(None); }
        let default = gateway_default_id(&self.ctx.home_dir).await.map_err(|_| "unavailable")?;
        let sender = SenderAccess::from_effective(snapshot.access.clone()).for_webchat(Ok(default));
        let db = self.user_db.clone();
        let uid = user.to_owned();
        let primary = tokio::task::spawn_blocking(move || db.bound_primary_agent(&uid))
            .await.map_err(|_| "unavailable")?.map_err(|_| "unavailable")?;
        let reg = self.ctx.registry.read().await;
        let candidates: Vec<String> = reg.list().into_iter().filter_map(|a| {
            primary_candidate(&sender, a).then(|| a.config.agent.name.clone())
        }).collect();
        if let Some(primary) = primary.filter(|id| candidates.iter().any(|a| a == id)) {
            return Ok(Some(primary));
        }
        match candidates.as_slice() {
            [only] => Ok(Some(only.clone())),
            [] => Err("no_agents"),
            _ => Err("multiple_agents"),
        }
    }

    /// WebChat assignment targets use the same non-enumerating surface as
    /// conversation targets. Schedule controls resolve only the owner's rows.
    pub(super) async fn authorize_command(&self, user: &str, cmd: &mut crate::chat_commands::ChatCommand) -> Result<(), ChatMessage> {
        use crate::trigger_route::assign::{AssignCommand, ScheduleCommand};
        let crate::chat_commands::ChatCommand::Assign(command) = cmd else { return Ok(()); };
        if !matches!(command, AssignCommand::Text(_) | AssignCommand::Schedules(
            ScheduleCommand::Pause(_) | ScheduleCommand::Resume(_) | ScheduleCommand::Remove(_),
        )) { return Ok(()); }
        if is_widget_visitor(user) { return Ok(()); }
        let snapshot = self.acl_snapshot(user).await.map_err(|_| agent_unavailable())?;
        // P1h (N1): no Admin shortcut for assignment targets. An Admin's
        // targets differ from everyone's only by another person's Personal
        // employees (`assignment_text_allowed`), and resuming a schedule goes
        // through `authorize_target`, whose Personal check runs before its own
        // Admin shortcut. Pausing or removing one of the Admin's OWN schedules
        // only stops work, so it stays possible (a legacy schedule on someone
        // else's Personal employee can still be cleaned up).
        let admin = snapshot.role() == duduclaw_auth::UserRole::Admin;
        if let AssignCommand::Text(text) = command {
            let sender = SenderAccess::from_effective(snapshot.access)
                .for_webchat(gateway_default_id(&self.ctx.home_dir).await);
            let reg = self.ctx.registry.read().await;
            if !assignment_text_allowed(&reg, &sender, text, duduclaw_auth::acl::personal_restriction()) {
                tracing::warn!(user, "WebChat assignment target unavailable");
                return Err(agent_unavailable());
            }
            return Ok(());
        }
        let stops_work = matches!(command,
            AssignCommand::Schedules(ScheduleCommand::Pause(_) | ScheduleCommand::Remove(_)));
        let AssignCommand::Schedules(
            ScheduleCommand::Pause(arg) | ScheduleCommand::Resume(arg) | ScheduleCommand::Remove(arg),
        ) = command else { return Ok(()); };
        let store = crate::task_store::TaskStore::open(&self.ctx.home_dir)
            .map_err(|_| assignment_error("unavailable"))?;
        let rows = store.list_assign_schedules(Some(user), 200).await
            .map_err(|_| assignment_error("unavailable"))?;
        let not_found = || ChatMessage::error_coded("找不到這項排程，請重新查詢排程清單。", "schedule_not_found");
        let mut matches = rows.iter().filter(|r| r.id == *arg || duduclaw_core::truncate_chars(&r.id, 8) == *arg);
        let row = matches.next().ok_or_else(not_found)?;
        if matches.next().is_some() { return Err(not_found()); }
        if !(admin && stops_work) {
            self.authorize_target(user, &row.agent_id, false).await?;
        }
        // Bind subsequent command execution to the checked row, never resolve
        // the short display number against a second potentially changed list.
        *arg = row.id.clone();
        Ok(())
    }

    /// Preserve legacy detail only for administrators and anonymous widgets.
    /// Every other connection receives the same target-unavailable frame.
    pub(super) async fn target_error(&self, user: &str, detailed: ChatMessage) -> ChatMessage {
        if is_widget_visitor(user)
            || matches!(self.acl_snapshot(user).await, Ok(s) if s.role() == duduclaw_auth::UserRole::Admin)
        {
            detailed
        } else {
            agent_unavailable()
        }
    }

    /// Only call after ownership and the stored conversation partner were
    /// verified. Stored partners use the same non-enumerating refusal.
    pub(super) async fn authorize_resume_target(&self, user: &str, agent: &str) -> Result<(), ChatMessage> {
        self.authorize_target_inner(user, agent, true).await
    }

    /// Repeated before admission and after FIFO waiting. No default/public grant
    /// can turn a main agent into a callable dashboard target.
    pub(crate) async fn authorize_target(&self, user: &str, agent: &str, public: bool) -> Result<(), ChatMessage> {
        self.authorize_target_inner(user, agent, public).await
    }

    async fn authorize_target_inner(&self, user: &str, agent: &str, public: bool) -> Result<(), ChatMessage> {
        let denied = |reason: &str| {
            tracing::warn!(user, agent, reason, "WebChat target unavailable");
            agent_unavailable()
        };
        if is_widget_visitor(user) { return Ok(()); }
        let snapshot = self.acl_snapshot(user).await.map_err(|_| denied("identity_unavailable"))?;
        // v1.81 P1g: talking to a Personal employee makes it run work for the
        // caller — its owner only, whatever the role (read fresh; unreadable
        // ownership refuses). Checked before the Admin shortcut.
        if crate::runtime_readiness::is_protected_linux() {
            let (home, who, target) = (self.ctx.home_dir.clone(), user.to_owned(), agent.to_owned());
            let refused = tokio::task::spawn_blocking(move || {
                crate::personal_visibility::acl_denies(&home, true, &who, &target)
            })
            .await
            .unwrap_or(true);
            if refused {
                return Err(denied("personal_employee"));
            }
        }
        if snapshot.role() == duduclaw_auth::UserRole::Admin { return Ok(()); }
        let default = gateway_default_id(&self.ctx.home_dir).await.map_err(|_| denied("config_unavailable"))?;
        let sender = SenderAccess::from_effective(snapshot.access.clone()).for_webchat(Ok(default));
        let reg = self.ctx.registry.read().await;
        let loaded = reg.get(agent).ok_or_else(|| denied("unknown_agent"))?;
        if !target_allowed(&sender, loaded, public) {
            return Err(denied("agent_forbidden"));
        }
        Ok(())
    }
}

/// `/交辦 <text>` on WebChat: a named mention must hit (or partially match)
/// an employee this sender may assign to; otherwise the same
/// non-enumerating "unavailable" frame as an unknown name. P1h (N1): applies
/// to Admins too — another person's Personal employee is not a target.
pub(crate) fn assignment_text_allowed(
    reg: &duduclaw_agent::registry::AgentRegistry,
    sender: &SenderAccess,
    text: &str,
    restriction: Option<&dyn duduclaw_auth::acl::PersonalRestriction>,
) -> bool {
    use crate::trigger_route::assign::{AssignRight, assign_right_under, assignment_mention_targets_under};
    use duduclaw_agent::trigger_match::{self, MentionScan};
    if !trigger_match::has_named_mention(text) {
        return true;
    }
    if matches!(sender.sender, crate::trigger_route::Sender::Admin { .. }) {
        // Admins keep the legacy detail for unknown names (usage reply); only
        // a mention that resolves to nothing but a refused employee — another
        // person's Personal one — is stopped here.
        let all = crate::trigger_route::mention_targets(reg);
        let allowed = |id: &str| matches!(assign_right_under(reg, sender, id, None, restriction), AssignRight::Allowed(_));
        return !matches!(trigger_match::scan_mentions(text, &all, &allowed), MentionScan::Forbidden(_));
    }
    let targets = assignment_mention_targets_under(reg, sender, "webchat", None, restriction);
    matches!(trigger_match::scan_mentions(text, &targets, &|_| true), MentionScan::Hit(_) | MentionScan::Partial { .. })
}

/// Whether `agent` may be what a dashboard account talks to by default: the
/// filter WebChat applies to a non-admin account's candidates (and so to its
/// 主要 AI 員工) — live, the account may hand it work (Operator or above, or
/// Admin), and the WebChat target rules allow it (no main-role or gateway
/// default employee for non-admins; `sender` should carry
/// [`SenderAccess::for_webchat`] so those rules apply). The Telegram shared
/// bot's primary-employee route (RM-172) uses the same check, so an employee
/// the account may no longer use is skipped there exactly as in WebChat.
///
/// v1.81 P1h (N1): another person's Personal employee is never a primary
/// candidate (owner only, whatever the role; unreadable ownership refuses).
/// Checked explicitly because `may_assign` ignores the Personal restriction
/// and `may_call` lets an owner-set `public_trigger` through — that opens
/// calling, never routing the sender's work to it. This keeps the Telegram
/// primary route under the same rule WebChat's `authorize_target` enforces.
pub(crate) fn primary_candidate(sender: &SenderAccess, agent: &duduclaw_agent::registry::LoadedAgent) -> bool {
    primary_candidate_under(sender, agent, duduclaw_auth::acl::personal_restriction())
}

/// [`primary_candidate`] under an explicit Personal restriction (tests; the
/// production path uses the restriction installed at gateway boot).
pub(crate) fn primary_candidate_under(
    sender: &SenderAccess,
    agent: &duduclaw_agent::registry::LoadedAgent,
    restriction: Option<&dyn duduclaw_auth::acl::PersonalRestriction>,
) -> bool {
    let name = agent.config.agent.name.as_str();
    let may_assign = match &sender.sender {
        crate::trigger_route::Sender::Admin { .. } => true,
        crate::trigger_route::Sender::Account { access, .. } => access.may_assign(name),
        crate::trigger_route::Sender::Unmapped => false,
    };
    agent.config.agent.status.is_operational()
        && may_assign
        && !sender.personal_denied_under(name, restriction)
        && sender.webchat_allows(agent)
        && sender.may_call_under(agent, None, restriction)
}

/// Shared by async admission and the synchronous final-write policy fence.
fn target_allowed(sender: &SenderAccess, loaded: &duduclaw_agent::registry::LoadedAgent, public: bool) -> bool {
    sender.webchat_allows(loaded)
        && loaded.config.agent.status.is_operational()
        && sender.may_call(loaded, None)
        && matches!(&sender.sender,
            crate::trigger_route::Sender::Account { access, .. }
                if !access.level(&loaded.config.agent.name).is_some_and(|(level, _)|
                    !level.at_least(duduclaw_auth::AccessLevel::Operator))
                    && (public || access.may_assign(&loaded.config.agent.name)))
}

pub(super) async fn user_session_info(
    state: &Arc<WebChatState>, sid: &str, agent: Option<&str>, user: &str,
) -> ChatMessage {
    if is_widget_visitor(user)
        || matches!(state.acl_snapshot(user).await, Ok(s) if s.role() == duduclaw_auth::UserRole::Admin) {
        return build_session_info_frame(state, sid, agent).await;
    }
    let resolution = match agent {
        Some(id) => state.authorize_target(user, id, true).await
            .map(|()| Some(id.to_owned())).map_err(|_| "unavailable"),
        None => state.resolve_new_agent(user).await,
    };
    let mut info = build_session_info_frame(state, sid, resolution.as_ref().ok().and_then(|id| id.as_deref())).await;
    if let ChatMessage::SessionInfo { agent_name, agent_icon, model, supports_vision, resolved_agent, assignment_error: error, .. } = &mut info {
        match resolution {
            Ok(Some(id)) => {
                *resolved_agent = Some(serde_json::json!({"id": id, "name": agent_name, "icon": agent_icon}));
                *error = Some(serde_json::Value::Null);
            }
            other => {
                let reason = other.err().unwrap_or("no_agents");
                let message = match assignment_error(reason) { ChatMessage::Error { message, .. } => message, _ => unreachable!() };
                *resolved_agent = Some(serde_json::Value::Null);
                *error = Some(serde_json::json!({"code": "no_assigned_agent", "message": message, "reason": reason}));
                agent_name.clear();
                agent_icon.clear();
                model.clear();
                *supports_vision = false;
            }
        }
    }
    info
}
