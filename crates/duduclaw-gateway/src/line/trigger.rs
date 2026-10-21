//! `@` routing for LINE (owner decision D6).
//!
//! The durable ingress fingerprints "who handles this event" when it accepts
//! the event, before it runs it and before every send
//! (`ingress::revision`). The route therefore has to come out the same each
//! time it is computed for one event: the one-shot addressee is armed and
//! claimed by event id and ordered by the event's LINE timestamp
//! (`trigger_route::pending`), and the route snapshot is computed with
//! `quiet` set so only the run that answers the event logs and audits it.

use super::*;
use crate::trigger_route::access;
use crate::trigger_route::turn::{ChannelTurn, TurnDecision, decision_for_route};
use duduclaw_agent::resolver::AgentResolver;
use duduclaw_core::types::{Message, MessageType};

/// The route of one LINE event.
#[derive(Debug, Clone)]
pub(crate) struct LineRoute {
    /// The conversation's default employee (answers when nobody is called).
    pub fallback: String,
    pub decision: TurnDecision,
}

impl LineRoute {
    /// The employee whose model turn answers this event (for a direct reply
    /// such as the `@` list, the default one).
    pub fn agent(&self) -> &str {
        match &self.decision {
            TurnDecision::Agent { id, .. } => id,
            _ => &self.fallback,
        }
    }

    /// Whether the event addresses the bot: it calls someone by `@`, asks
    /// for the list, or is the message a one-shot addressee was waiting for.
    /// "Mention only" groups answer only these.
    pub fn addresses_bot(&self) -> bool {
        !matches!(self.decision, TurnDecision::Default)
    }

    /// A reply from a called (non-default) employee is labelled with its name.
    pub fn labelled(&self) -> bool {
        matches!(self.decision, TurnDecision::Agent { via_trigger: true, .. })
    }
}

/// Only message events take part in `@` routing (a postback, follow or
/// unsend event neither calls anyone nor consumes a one-shot addressee).
fn routable(event: &LineEvent) -> bool {
    event.event_type == "message"
        && event
            .message
            .as_ref()
            .is_some_and(|m| ["text", "image", "video", "audio", "file"].contains(&m.msg_type.as_str()))
}

/// Compute the route. `None` when no employee is loaded at all.
pub(crate) async fn line_route(state: &LineState, event: &LineEvent, quiet: bool) -> Option<LineRoute> {
    let sender = event
        .source
        .as_ref()
        .and_then(|s| s.user_id.as_deref())
        .unwrap_or("");
    let session = format!("line:{}", ingress::line_conversation(event));
    let text = event
        .message
        .as_ref()
        .filter(|m| m.msg_type == "text")
        .and_then(|m| m.text.as_deref())
        .unwrap_or("");
    let reg = state.ctx.registry.read().await;
    let message = Message {
        id: String::new(),
        message_type: MessageType::Incoming,
        channel: "line".into(),
        chat_id: session.clone(),
        sender: sender.into(),
        text: String::new(),
        timestamp: chrono::Utc::now(),
        agent_id: None,
    };
    let fallback = AgentResolver::new(&reg).resolve(&message)?.config.agent.name.clone();
    if !routable(event) {
        return Some(LineRoute {
            fallback,
            decision: TurnDecision::Default,
        });
    }
    let access = if crate::trigger_route::may_mention(text) {
        access::for_channel_sender(&state.home_dir, "line", sender)
    } else {
        crate::trigger_route::SenderAccess::unmapped()
    };
    let route = crate::trigger_route::decide(&reg, text, &access, Some(&fallback));
    let event_id = event.webhook_event_id.as_deref().unwrap_or("");
    let decision = decision_for_route(
        &state.home_dir,
        &reg,
        ChannelTurn {
            channel: "line",
            conversation: &session,
            sender,
            text,
            fallback: Some(&fallback),
            event: event_id,
            seq: event
                .timestamp
                .unwrap_or_else(crate::trigger_route::pending::next_seq),
            quiet,
        },
        &access,
        route,
    );
    Some(LineRoute { fallback, decision })
}

/// The messages for a direct (no model) answer: the `@` list as quick-reply
/// buttons, or plain text.
pub(crate) fn direct_reply_messages(text: &str, list: Option<&[crate::trigger_route::list::CallableAgent]>) -> Vec<serde_json::Value> {
    match list {
        Some(agents) => vec![crate::trigger_route::list::line_list_message(agents, 0)],
        None => vec![serde_json::json!({ "type": "text", "text": text })],
    }
}

/// A "更多" press on the `@` list: the requested page, re-filtered for the
/// presser (a press never widens what they may call).
pub(crate) async fn list_page_messages(state: &LineState, event: &LineEvent, page: usize) -> Vec<serde_json::Value> {
    let sender = event
        .source
        .as_ref()
        .and_then(|s| s.user_id.as_deref())
        .unwrap_or("");
    let session = format!("line:{}", ingress::line_conversation(event));
    let reg = state.ctx.registry.read().await;
    let message = Message {
        id: String::new(),
        message_type: MessageType::Incoming,
        channel: "line".into(),
        chat_id: session,
        sender: sender.into(),
        text: String::new(),
        timestamp: chrono::Utc::now(),
        agent_id: None,
    };
    let fallback = AgentResolver::new(&reg)
        .resolve(&message)
        .map(|a| a.config.agent.name.clone());
    let access = access::for_channel_sender(&state.home_dir, "line", sender);
    let ids = crate::trigger_route::callable_ids(&reg, &access, fallback.as_deref());
    let agents = crate::trigger_route::list::callable_agents(&reg, &ids);
    vec![crate::trigger_route::list::line_list_message(&agents, page)]
}

#[cfg(test)]
#[path = "trigger_tests.rs"]
mod tests;
