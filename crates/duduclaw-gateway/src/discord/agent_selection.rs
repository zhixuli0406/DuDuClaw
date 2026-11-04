//! Shared explicit target policy for Discord messages, commands and /ask.
use super::*;

/// None deliberately means the shared channel pipeline still owns fallback
/// and sender-aware trigger routing. Never turn it into a preselected main.
pub(super) async fn explicit_agent(
    ctx: &ReplyContext,
    per_bot: Option<&str>,
    guild_id: &str,
) -> Result<Option<String>, ()> {
    let scope = if guild_id.is_empty() { "dm" } else { guild_id };
    let rows = ctx
        .channel_settings
        .refresh_channel_snapshot("discord", scope)
        .await
        .map_err(|_| ())?;
    let configured = |wanted: &str| {
        rows.iter()
            .find(|(sc, key, value)| {
                sc == wanted && key == keys::AGENT_OVERRIDE && !value.is_empty()
            })
            .map(|(_, _, value)| value.clone())
    };
    let selected = match per_bot {
        Some(name) => Some(name.to_owned()),
        None => configured(scope).or_else(|| configured("global")),
    };
    let Some(name) = selected else {
        return Ok(None);
    };
    if per_bot.is_none() {
        let choices = choices_from_rows(&rows, guild_id);
        let registry = ctx.registry.read().await;
        let allowed = registry.get(&name).is_some_and(|agent| {
            agent.config.agent.status.is_operational() && choice_allowed(&choices, &name, agent.config.agent.role == duduclaw_core::types::AgentRole::Main)
        });
        if !allowed {
            static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("Ignoring a Discord agent override outside the administrator choices");
            }
            return Ok(None);
        }
    }
    // An explicit dangling per-bot selection is not equivalent to an unset one.
    if ctx.registry.read().await.get(&name).is_none_or(|agent| !agent.config.agent.status.is_operational()) {
        return Err(());
    }
    Ok(Some(name))
}

pub(super) async fn command_agent(
    ctx: &ReplyContext,
    explicit: Option<&str>,
    session_id: &str,
    user_id: &str,
) -> Result<String, ()> {
    let registry = ctx.registry.read().await;
    match explicit {
        Some(name) => registry
            .get(name)
            .filter(|agent| agent.config.agent.status.is_operational())
            .map(|agent| agent.config.agent.name.clone())
            .ok_or(()),
        None => Ok(crate::channel_reply::resolve_fallback_agent(
            ctx, &registry, session_id, user_id,
        )
        .await
        .map(|agent| agent.config.agent.name.clone())
        .unwrap_or_default()),
    }
}


pub(super) const CHOICES_UNAVAILABLE: &str = "此伺服器尚未開放這位 AI 員工的切換。請伺服器管理員聯絡 DuDuClaw 管理員設定可選名單。";

fn choices_from_rows(rows: &[(String, String, String)], guild: &str) -> Vec<String> {
    if guild.is_empty() || guild == "global" || guild == "dm" { return vec![]; }
    rows.iter().find(|(scope, key, _)| scope == guild && key == keys::DISCORD_AGENT_CHOICES)
        .and_then(|(_, _, value)| serde_json::from_str(value).ok()).unwrap_or_default()
}

fn choice_allowed(choices: &[String], name: &str, is_main: bool) -> bool {
    !is_main && duduclaw_core::is_valid_agent_id(name) && choices.iter().any(|id| id == name)
}

pub(super) async fn allowed_choices(ctx: &ReplyContext, guild: &str) -> Vec<String> {
    let Ok(rows) = ctx.channel_settings.refresh_channel_snapshot("discord", guild).await else { return vec![]; };
    let choices = choices_from_rows(&rows, guild);
    let registry = ctx.registry.read().await;
    let mut allowed: Vec<String> = choices.into_iter().filter(|name| {
        registry.get(name).is_some_and(|agent| agent.config.agent.status.is_operational() && choice_allowed(std::slice::from_ref(name), name,
            agent.config.agent.role == duduclaw_core::types::AgentRole::Main))
    }).collect();
    allowed.sort();
    allowed.dedup();
    allowed
}

#[cfg(test)]
mod discord_choices_tests {
    use super::*;
    #[tokio::test]
    async fn discord_live_status_revokes_choices_overrides_and_dedicated_bots() {
        let home=tempfile::tempdir().unwrap();
        let handler=crate::handlers::MethodHandler::new(home.path().to_path_buf()).await;
        let created=handler.handle_agents_create(serde_json::json!({"name":"worker","display_name":"Worker","role":"specialist"})).await;
        assert!(matches!(created,crate::protocol::WsFrame::Response {ok:true,..}),"{created:?}");
        let registry=Arc::new(tokio::sync::RwLock::new(duduclaw_agent::AgentRegistry::new(home.path().join("agents"))));
        let sessions=Arc::new(crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap());
        let (tx,_)=tokio::sync::broadcast::channel(16);
        let ctx=ReplyContext::new(registry.clone(),home.path().to_path_buf(),sessions,
            Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),tx);
        ctx.channel_settings.set("discord","111",keys::DISCORD_AGENT_CHOICES,r#"["worker"]"#).await.unwrap();
        ctx.channel_settings.set("discord","111",keys::AGENT_OVERRIDE,"worker").await.unwrap();
        let path=home.path().join("agents/worker/agent.toml");
        let active=std::fs::read_to_string(&path).unwrap();
        for status in ["paused","terminated","archived","deleted"] {
            std::fs::write(&path,&active).unwrap();
            registry.write().await.scan().await.unwrap();
            assert_eq!(allowed_choices(&ctx,"111").await,vec!["worker"]);
            assert_eq!(explicit_agent(&ctx,None,"111").await,Ok(Some("worker".into())));
            assert_eq!(explicit_agent(&ctx,Some("worker"),"111").await,Ok(Some("worker".into())));
            let mut table:toml::Value=toml::from_str(&active).unwrap();
            table["agent"]["status"]=toml::Value::String(status.into());
            std::fs::write(&path,toml::to_string(&table).unwrap()).unwrap();
            registry.write().await.scan().await.unwrap();
            assert!(allowed_choices(&ctx,"111").await.is_empty(),"{status}");
            assert_eq!(explicit_agent(&ctx,None,"111").await,Ok(None),"{status}");
            assert!(explicit_agent(&ctx,Some("worker"),"111").await.is_err(),"{status}");
            assert!(command_agent(&ctx,Some("worker"),"discord:222","user").await.is_err());
        }
    }

    #[test]
    fn discord_main_and_empty_choices_are_refused() {
        assert!(!choice_allowed(&["main".into()], "main", true));
        assert!(!choice_allowed(&[], "worker", false));
        assert!(choice_allowed(&["worker".into()], "worker", false));
        assert!(!choice_allowed(&["worker".into()], "other", false));
    }
    #[test]
    fn discord_stale_override_and_global_choices_are_ignored() {
        let rows = vec![("global".into(), keys::DISCORD_AGENT_CHOICES.into(), "[\"worker\"]".into()),
            ("guild".into(), keys::DISCORD_AGENT_CHOICES.into(), "[\"new-worker\"]".into())];
        assert!(choices_from_rows(&rows, "other-guild").is_empty());
        assert!(!choice_allowed(&choices_from_rows(&rows, "guild"), "worker", false));
    }
}
