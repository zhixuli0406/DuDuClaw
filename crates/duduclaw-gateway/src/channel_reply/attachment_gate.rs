//! Check the answering employee before an adapter downloads inbound media.
use super::ReplyContext;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AttachmentEntry {
    Proceed,
    /// Keep text and the existing access/pairing flow, without files or a reply.
    SkipFiles,
    /// The original parsed command text, without file references or quotes.
    DropFilesAndContinue(String),
    Refuse(String),
}

/// `selected` is the adapter's existing selection (own bot, stored LINE
/// route, space override, etc.). An unset selection retains the reply
/// pipeline's fallback and sender-aware one-shot routing. A peek must never
/// consume the pending addressee before the real pipeline sees the message.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn check(
    ctx: &ReplyContext,
    gate_session: &str,
    reply_session: &str,
    sender: &str,
    text: &str,
    selected: Option<&str>,
    access_policy: Option<&super::UserAccessPolicy>,
) -> AttachmentEntry {
    // Preserve the adapter's existing denial/pairing flow. In particular,
    // never disclose employee state to someone who cannot use this channel.
    // Empty text makes this read-only with respect to /pair codes: only the
    // original command handler may consume one.
    let denied = match access_policy {
        Some(policy) => super::check_command_access_with_policy(ctx, gate_session, sender, "", policy).await,
        None => super::check_command_access_gate(ctx, gate_session, sender, "").await,
    };
    if denied.is_some() {
        return AttachmentEntry::SkipFiles;
    }
    // Decisions validate the task's assignee in their own lane. Never let
    // an unrelated answering employee or an attachment block a decline.
    if crate::channel_decision_route::is_strict_decision(text) {
        return AttachmentEntry::DropFilesAndContinue(text.to_string());
    }
    let (target, session) = {
        let registry = ctx.registry.read().await;
        match selected.filter(|id| !id.is_empty()) {
            Some(id) => (id.to_string(), reply_session.to_string()),
            None => {
                let fallback = super::resolve_fallback_agent_in(
                    &ctx.home_dir, &*registry, reply_session, sender,
                ).await;
                // The pipeline sees a nonempty attachment reference too. In
                // particular, "@employee" plus a file is work, not an empty
                // call that should merely arm the next message.
                let routing_text = format!("{text}\n\n[附件]");
                let decision = crate::trigger_route::turn::decide_channel_turn(
                    &ctx.home_dir, &*registry,
                    crate::trigger_route::turn::ChannelTurn {
                        channel: gate_session.split(':').next().unwrap_or("unknown"),
                        conversation: reply_session, sender,
                        text: &routing_text,
                        fallback: fallback.map(|agent| agent.config.agent.name.as_str()),
                        event: "attachment-preflight",
                        seq: crate::trigger_route::pending::next_seq(), quiet: true,
                        pending: crate::trigger_route::turn::PendingPolicy::Peek,
                    },
                );
                match decision {
                    crate::trigger_route::turn::TurnDecision::Agent { id, via_trigger } => {
                        let session = if via_trigger {
                            crate::trigger_route::called_session(reply_session, &id)
                        } else { reply_session.to_string() };
                        (id, session)
                    }
                    crate::trigger_route::turn::TurnDecision::Assign { id } =>
                        (id, reply_session.to_string()),
                    crate::trigger_route::turn::TurnDecision::Reply { .. } =>
                        return AttachmentEntry::Proceed,
                    crate::trigger_route::turn::TurnDecision::Default => {
                        let Some(agent) = fallback else { return AttachmentEntry::Proceed; };
                        (agent.config.agent.name.clone(), reply_session.to_string())
                    }
                }
            }
        }
    };
    let result = {
        let registry = ctx.registry.read().await;
        crate::entry_gate::resolve_and_check(&ctx.home_dir, &*registry, &target)
    };
    let Err(refusal) = result else { return AttachmentEntry::Proceed; };
    if crate::chat_commands::parse_command(text, None).as_ref()
        .is_some_and(crate::entry_gate::command_is_exempt)
        || crate::chat_commands::parse_takeover(text).as_ref()
            .is_some_and(crate::entry_gate::takeover_is_exempt)
    {
        return AttachmentEntry::DropFilesAndContinue(text.to_string());
    }
    let language = crate::prompt_identity::resolve_fixed_message_language_for_id(
        &ctx.home_dir, &target,
    ).await;
    AttachmentEntry::Refuse(refusal.user_text(&session, language.as_deref(), "attachment"))
}

#[cfg(test)]
#[path = "attachment_gate_tests.rs"]
mod tests;
