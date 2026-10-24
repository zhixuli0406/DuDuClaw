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
    // An explicit dangling selection is not equivalent to an unset one.
    if ctx.registry.read().await.get(&name).is_none() {
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
