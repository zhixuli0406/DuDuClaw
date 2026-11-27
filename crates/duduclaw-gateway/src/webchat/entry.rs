//! Preflight before WebChat admission and again after its FIFO wait.
use super::WebChatState;
use crate::trigger_route::assign::{AssignCaller, Plan};

pub(super) fn exempt(content: &str) -> bool {
    crate::chat_commands::parse_command(content, None).as_ref()
        .is_some_and(crate::entry_gate::command_is_exempt)
        || crate::chat_commands::parse_takeover(content).as_ref()
            .is_some_and(crate::entry_gate::takeover_is_exempt)
}

/// Refused attachments must not suppress exempt safety or repair commands.
pub(super) fn continues_without_files(content: &str) -> bool {
    exempt(content)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn check(
    state: &WebChatState, auth_user: &str, agent: &str, sid: &str, user_id: &str,
    content: &str, attachments: bool, turn: &str,
) -> Result<(), String> {
    let command = crate::chat_commands::parse_command(content, None);
    let exempt = exempt(content);
    // WebChat has no text-decision handler: decision-shaped text enters the
    // normal reply pipeline and must pass the employee gate too.
    if !attachments && exempt { return Ok(()); }
    let mut target = if agent.is_empty() {
        let registry = state.ctx.registry.read().await;
        match crate::channel_reply::resolve_fallback_agent(&state.ctx, &*registry, sid, user_id).await {
            Some(agent) => agent.config.agent.name.clone(),
            None => return Ok(()), // Preserve the reply pipeline's no-responder notice.
        }
    } else { agent.to_string() };
    if let Some(crate::chat_commands::ChatCommand::Assign(cmd)) = &command {
        let access = state.sender_access(auth_user).await;
        let caller = AssignCaller {
            home: &state.ctx.home_dir, channel: "webchat", conversation: sid,
            sender: auth_user, event: None, seq: 0, fallback: Some(&target), access,
        };
        let registry = state.ctx.registry.read().await;
        match crate::trigger_route::assign::plan(
            &registry, crate::trigger_route::texts::lang_for(&state.ctx.home_dir), cmd, &caller,
        ) {
            Plan::Arm { agent, .. } => target = agent,
            Plan::Create(spec) | Plan::ScheduleCreate { spec, .. } => target = spec.agent,
            _ => {}
        }
    }
    if state.no_responder_turn(&target, sid, user_id).await && !attachments {
        return Ok(());
    }
    let result = {
        let registry = state.ctx.registry.read().await;
        crate::entry_gate::resolve_and_check(&state.ctx.home_dir, &*registry, &target)
    };
    if let Err(refusal) = result {
        let language = crate::prompt_identity::resolve_fixed_message_language_for_id(&state.ctx.home_dir, &target).await;
        return Err(crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
            auth_user.to_string(), async { refusal.user_text(sid, language.as_deref(), turn) },
        ).await);
    }
    Ok(())
}

