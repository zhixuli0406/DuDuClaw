//! Dashboard-only conversation routing. Anonymous widgets retain their public route.
use super::*;
use crate::trigger_route::SenderAccess;

/// Read the configured gateway default separately from channel fallbacks.
/// An unreadable configuration cannot establish that a target is non-main.
pub(crate) async fn gateway_default_id(home: &std::path::Path) -> Result<String, String> {
    let raw = match tokio::fs::read_to_string(home.join("config.toml")).await {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.to_string()),
    };
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
            let id = &a.config.agent.name;
            (a.config.agent.status.is_operational() && snapshot.access.may_call(id)
                && sender.webchat_allows(a) && sender.may_call(a, None)).then(|| id.clone())
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
        if snapshot.role() == duduclaw_auth::UserRole::Admin { return Ok(()); }
        if let AssignCommand::Text(text) = command {
            use duduclaw_agent::trigger_match::{self, MentionScan};
            let sender = SenderAccess::from_effective(snapshot.access)
                .for_webchat(gateway_default_id(&self.ctx.home_dir).await);
            let reg = self.ctx.registry.read().await;
            let targets = crate::trigger_route::assign::assignment_mention_targets(
                &reg, &sender, "webchat", None,
            );
            if trigger_match::has_named_mention(text)
                && !matches!(trigger_match::scan_mentions(text, &targets, &|_| true),
                    MentionScan::Hit(_) | MentionScan::Partial { .. })
            {
                tracing::warn!(user, "WebChat assignment target unavailable");
                return Err(agent_unavailable());
            }
            return Ok(());
        }
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
        self.authorize_target(user, &row.agent_id, false).await?;
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
    pub(super) async fn authorize_target(&self, user: &str, agent: &str, public: bool) -> Result<(), ChatMessage> {
        self.authorize_target_inner(user, agent, public).await
    }

    async fn authorize_target_inner(&self, user: &str, agent: &str, public: bool) -> Result<(), ChatMessage> {
        let denied = |reason: &str| {
            tracing::warn!(user, agent, reason, "WebChat target unavailable");
            agent_unavailable()
        };
        if is_widget_visitor(user) { return Ok(()); }
        let snapshot = self.acl_snapshot(user).await.map_err(|_| denied("identity_unavailable"))?;
        if snapshot.role() == duduclaw_auth::UserRole::Admin { return Ok(()); }
        let default = gateway_default_id(&self.ctx.home_dir).await.map_err(|_| denied("config_unavailable"))?;
        let sender = SenderAccess::from_effective(snapshot.access.clone()).for_webchat(Ok(default));
        let reg = self.ctx.registry.read().await;
        let loaded = reg.get(agent).ok_or_else(|| denied("unknown_agent"))?;
        if !sender.webchat_allows(loaded) {
            return Err(denied("main_agent_admin_only"));
        }
        if !loaded.config.agent.status.is_operational()
            || !sender.may_call(loaded, None)
            || (!public && !snapshot.access.may_call(agent))
        {
            return Err(denied("agent_forbidden"));
        }
        Ok(())
    }
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
