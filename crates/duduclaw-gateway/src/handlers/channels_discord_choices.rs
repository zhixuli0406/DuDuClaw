//! Administrator-owned Discord employee choices.
use super::*;

fn guild_param(params: &Value, required: bool) -> Result<String, WsFrame> {
    let guild = params.get("guild_id").and_then(Value::as_str).unwrap_or("");
    if (!required && guild.is_empty()) || (!guild.is_empty() && guild.chars().all(|c| c.is_ascii_digit()) && guild.len() <= 64) {
        Ok(guild.to_string())
    } else {
        Err(WsFrame::error_response("", "請輸入有效的 Discord 伺服器 ID。"))
    }
}

impl MethodHandler {
    pub(crate) async fn handle_channels_discord_agent_choices_get(&self, params: Value) -> WsFrame {
        let guild = match guild_param(&params, false) { Ok(g) => g, Err(f) => return f };
        if let Err(e) = crate::product_policy::require_channel(&self.home_dir, "discord") {
            return WsFrame::error_response("", &e);
        }
        let mgr = match self.open_channel_settings() { Ok(m) => m, Err(e) => return WsFrame::error_response("", &e) };
        let rows = match mgr.refresh_channel_snapshot("discord", &guild).await {
            Ok(r) => r, Err(e) => return WsFrame::error_response("", &e),
        };
        let agents: Vec<String> = rows.iter()
            .find(|(scope, key, _)| scope == &guild && key == crate::channel_settings::keys::DISCORD_AGENT_CHOICES)
            .and_then(|(_, _, v)| serde_json::from_str(v).ok()).unwrap_or_default();
        let mut available_agents: Vec<String> = self.registry.read().await.list().iter()
            .filter(|a| a.config.agent.status.is_operational() && a.config.agent.role != duduclaw_core::types::AgentRole::Main)
            .map(|a| a.config.agent.name.clone()).collect();
        available_agents.sort();
        let mut guilds = vec![];
        for id in mgr.list_scopes("discord").await {
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) { continue; }
            let name = mgr.get("discord", &id, crate::channel_settings::keys::GUILD_NAME).await.unwrap_or_else(|| id.clone());
            guilds.push(json!({"id":id,"name":name}));
        }
        WsFrame::ok_response("", json!({"success":true,"guild_id":guild,"agents":agents,"available_agents":available_agents,"guilds":guilds}))
    }

    pub(crate) async fn handle_channels_discord_agent_choices_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let guild = match guild_param(&params, true) { Ok(g) => g, Err(f) => return f };
        if let Err(e) = crate::product_policy::require_channel(&self.home_dir, "discord") {
            return WsFrame::error_response("", &e);
        }
        let agents: Vec<String> = match params.get("agents").cloned().and_then(|v| serde_json::from_value(v).ok()) {
            Some(v) => v, None => return WsFrame::error_response("", "請提供 AI 員工名單。"),
        };
        let value = serde_json::to_string(&agents).expect("string array serialization");
        if crate::channel_settings::validate_setting_value(crate::channel_settings::keys::DISCORD_AGENT_CHOICES, &value).is_err() {
            return WsFrame::error_response("", "可選名單最多包含 25 位有效的 AI 員工。");
        }
        let registry = self.registry.read().await;
        if agents.iter().any(|name| registry.get(name).is_none_or(|a| !a.config.agent.status.is_operational() || a.config.agent.role == duduclaw_core::types::AgentRole::Main)) {
            return WsFrame::error_response("", "名單包含不存在或不可選擇的 AI 員工；主要 AI 員工不能透過 Discord 切換。");
        }
        let mgr = match self.open_channel_settings() { Ok(m) => m, Err(e) => return WsFrame::error_response("", &e) };
        if let Err(e) = mgr.set("discord", &guild, crate::channel_settings::keys::DISCORD_AGENT_CHOICES, &value).await {
            return WsFrame::error_response("", &e);
        }
        drop(registry);
        crate::security_autopilot::audit_and_emit(&self.home_dir, &duduclaw_security::audit::AuditEvent::new(
            "discord_agent_choices_set", "discord", duduclaw_security::audit::Severity::Info,
            json!({"guild_id":guild,"agents":agents,"actor":ctx.user_id,"source":"dashboard"}),
        ));
        WsFrame::ok_response("", json!({"success":true,"guild_id":guild,"agents":agents}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discord_choices_rpc_refuses_non_operational_ids_after_status_change() {
        let home=tempfile::tempdir().unwrap();
        let handler=MethodHandler::new(home.path().to_path_buf()).await;
        let created=handler.handle_agents_create(json!({"name":"worker","display_name":"Worker","role":"specialist"})).await;
        assert!(matches!(created,WsFrame::Response {ok:true,..}),"{created:?}");
        let path=home.path().join("agents/worker/agent.toml");
        let active=std::fs::read_to_string(&path).unwrap();
        let ctx=UserContext::admin_fallback();
        let input=json!({"guild_id":"111","agents":["worker"]});
        for status in ["paused","terminated","archived","deleted"] {
            std::fs::write(&path,&active).unwrap();
            handler.registry.write().await.scan().await.unwrap();
            let allowed=handler.handle_channels_discord_agent_choices_set(input.clone(),&ctx).await;
            assert!(matches!(allowed,WsFrame::Response {ok:true,..}),"{allowed:?}");
            let mut table:toml::Value=toml::from_str(&active).unwrap();
            table["agent"]["status"]=toml::Value::String(status.into());
            std::fs::write(&path,toml::to_string(&table).unwrap()).unwrap();
            handler.registry.write().await.scan().await.unwrap();
            let refused=handler.handle_channels_discord_agent_choices_set(input.clone(),&ctx).await;
            assert!(matches!(refused,WsFrame::Response {ok:false,..}),"{status}: {refused:?}");
            let listed=handler.handle_channels_discord_agent_choices_get(json!({"guild_id":"111"})).await;
            let WsFrame::Response {ok:true,payload:Some(data),..}=listed else {panic!("{listed:?}")};
            assert!(!data["available_agents"].as_array().unwrap().contains(&json!("worker")));
        }
    }
}
