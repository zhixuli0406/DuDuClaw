//! `@` routing for LINE (owner decision D6).
//!
//! The durable ingress fingerprints "who handles this event" when it accepts
//! the event, before it runs it and before every send
//! (`ingress::revision`). The one-shot addressee is therefore resolved only
//! once, when the event's snapshot is first taken ([`RouteMode::Admission`]),
//! and the result is stored with the event (`ingress_routes`). Every later
//! computation ([`RouteMode::Replay`]) reuses that result and only re-checks
//! that the sender may still call the employee; it never arms, claims or
//! cancels anything, so a retried event cannot re-arm an addressee.
//!
//! Native LINE mentions of people (`message.mention.mentionees`) are blanked
//! before matching; a mention of the bot itself counts as addressing the bot
//! in "mention only" groups.

use super::*;
use crate::trigger_route::access;
use crate::trigger_route::turn::{
    ChannelTurn, PendingPolicy, ReplyKind, TurnDecision, decision_for_route,
};
use duduclaw_agent::resolver::AgentResolver;
use duduclaw_core::types::{Message, MessageType};

/// The stored route of an event (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredRoute {
    pub agent: String,
    /// The employee came from a one-shot addressee (the message itself calls
    /// nobody).
    pub one_shot: bool,
}

/// How a route is computed.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RouteMode<'a> {
    /// The first snapshot of a new event: arm/claim the one-shot addressee.
    Admission,
    /// Any later computation: reuse the stored result.
    Replay(Option<&'a StoredRoute>),
}

/// The route of one LINE event.
#[derive(Debug, Clone)]
pub(crate) struct LineRoute {
    /// The conversation's default employee (answers when nobody is called).
    pub fallback: String,
    pub decision: TurnDecision,
    /// The decision came from a one-shot addressee.
    pub one_shot: bool,
    /// The message mentions the bot itself (LINE native mention).
    pub bot_mentioned: bool,
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

    /// Whether the event addresses the bot: it mentions the bot, calls
    /// someone by `@`, asks for the list, or is the message a one-shot
    /// addressee was waiting for. "Mention only" groups answer only these.
    pub fn addresses_bot(&self) -> bool {
        self.bot_mentioned || self.decision.addresses_bot()
    }

    /// A reply from a called (non-default) employee is labelled with its
    /// name and runs in that employee's own session bucket.
    pub fn labelled(&self) -> bool {
        matches!(
            self.decision,
            TurnDecision::Agent {
                via_trigger: true,
                ..
            }
        )
    }

    /// What to store for this event after its first snapshot.
    pub fn stored(&self) -> StoredRoute {
        StoredRoute {
            agent: self.agent().to_string(),
            one_shot: self.one_shot,
        }
    }
}

/// Only message events take part in `@` routing (a postback, follow or
/// unsend event neither calls anyone nor consumes a one-shot addressee).
fn routable(event: &LineEvent) -> bool {
    event.event_type == "message"
        && event.message.as_ref().is_some_and(|m| {
            ["text", "image", "video", "audio", "file"].contains(&m.msg_type.as_str())
        })
}

/// The text to match: native mentions of people blanked (they name a
/// person, never an AI employee), plus whether the bot itself is mentioned.
pub(crate) fn routing_text(event: &LineEvent) -> (String, bool) {
    let Some(msg) = event.message.as_ref().filter(|m| m.msg_type == "text") else {
        return (String::new(), false);
    };
    let text = msg.text.as_deref().unwrap_or("");
    let mentionees = msg
        .mention
        .as_ref()
        .map(|m| m.mentionees.as_slice())
        .unwrap_or(&[]);
    let people: Vec<(usize, usize)> = mentionees
        .iter()
        .filter(|m| !m.is_self)
        .map(|m| (m.index, m.length))
        .collect();
    let bot_mentioned = mentionees.iter().any(|m| m.is_self);
    (
        crate::trigger_route::spans::blank_utf16_spans(text, &people),
        bot_mentioned,
    )
}

fn sender_of(event: &LineEvent) -> &str {
    event
        .source
        .as_ref()
        .and_then(|s| s.user_id.as_deref())
        .unwrap_or("")
}

/// A stable id for this event's one-shot state. LINE always sends
/// `webhookEventId` (the durable ingress refuses events without one); fall
/// back to a digest of what identifies the message so two id-less events
/// never share state.
fn event_key(event: &LineEvent, session: &str) -> String {
    match event
        .webhook_event_id
        .as_deref()
        .filter(|id| !id.is_empty())
    {
        Some(id) => id.to_string(),
        None => crate::channel_ingress::digest(&[
            session,
            sender_of(event),
            &event.timestamp.unwrap_or_default().to_string(),
            event
                .message
                .as_ref()
                .and_then(|m| m.id.as_deref())
                .unwrap_or(""),
        ]),
    }
}

/// Compute the route. `None` when no employee is loaded at all.
pub(crate) async fn line_route(
    state: &LineState,
    event: &LineEvent,
    mode: RouteMode<'_>,
    quiet: bool,
) -> Option<LineRoute> {
    let sender = sender_of(event);
    let session = format!("line:{}", ingress::line_conversation(event));
    let (text, bot_mentioned) = routing_text(event);
    let reg = state.ctx.registry.read().await;
    let fallback = default_employee(&reg, &session, sender)?;
    if !routable(event) {
        return Some(LineRoute {
            fallback,
            decision: TurnDecision::Default,
            one_shot: false,
            bot_mentioned,
        });
    }
    let access = if crate::trigger_route::may_mention(&text) {
        access::for_channel_sender(&state.home_dir, "line", sender)
    } else {
        crate::trigger_route::SenderAccess::unmapped()
    };
    let route = crate::trigger_route::decide(&reg, &text, &access, Some(&fallback));
    let calls_nobody = route == crate::trigger_route::TriggerRoute::None;
    let pending = match mode {
        RouteMode::Admission => PendingPolicy::Live,
        RouteMode::Replay(stored) => {
            PendingPolicy::Replay(stored.filter(|s| s.one_shot).map(|s| s.agent.as_str()))
        }
    };
    let event_id = event_key(event, &session);
    let decision = decision_for_route(
        &state.home_dir,
        &reg,
        ChannelTurn {
            channel: "line",
            conversation: &session,
            sender,
            text: &text,
            fallback: Some(&fallback),
            event: &event_id,
            seq: event
                .timestamp
                .unwrap_or_else(crate::trigger_route::pending::next_seq),
            quiet,
            pending,
        },
        &access,
        route,
    );
    let one_shot = calls_nobody && matches!(decision, TurnDecision::Agent { .. });
    Some(LineRoute {
        fallback,
        decision,
        one_shot,
        bot_mentioned,
    })
}

/// The conversation's default employee.
fn default_employee(
    reg: &duduclaw_agent::registry::AgentRegistry,
    session: &str,
    sender: &str,
) -> Option<String> {
    let message = Message {
        id: String::new(),
        message_type: MessageType::Incoming,
        channel: "line".into(),
        chat_id: session.to_string(),
        sender: sender.into(),
        text: String::new(),
        timestamp: chrono::Utc::now(),
        agent_id: None,
    };
    AgentResolver::new(reg)
        .resolve(&message)
        .map(|a| a.config.agent.name.clone())
}

/// Lists and refusals are throttled per sender per conversation.
pub(crate) fn direct_reply_allowed(kind: ReplyKind, event: &LineEvent) -> bool {
    kind == ReplyKind::AskTask
        || crate::trigger_route::allow_direct_reply(
            "line",
            &ingress::line_conversation(event),
            sender_of(event),
            kind,
        )
}

/// The messages for a direct (no model) answer: the `@` list as quick-reply
/// buttons, or plain text.
pub(crate) fn direct_reply_messages(
    text: &str,
    list: Option<&[crate::trigger_route::list::CallableAgent]>,
    lang: crate::trigger_route::texts::Lang,
) -> Vec<serde_json::Value> {
    match list {
        Some(agents) => vec![crate::trigger_route::list::line_list_message(
            agents, 0, lang,
        )],
        None => vec![serde_json::json!({ "type": "text", "text": text })],
    }
}

/// A "更多" press on the `@` list: the requested page, re-filtered for the
/// presser (a press never widens what they may call).
pub(crate) async fn list_page_messages(
    state: &LineState,
    event: &LineEvent,
    page: usize,
) -> Vec<serde_json::Value> {
    let sender = sender_of(event);
    let session = format!("line:{}", ingress::line_conversation(event));
    let reg = state.ctx.registry.read().await;
    let fallback = default_employee(&reg, &session, sender);
    let access = access::for_channel_sender(&state.home_dir, "line", sender);
    let ids = crate::trigger_route::callable_ids(&reg, &access, fallback.as_deref());
    let agents = crate::trigger_route::list::callable_agents(&reg, &ids);
    let lang = crate::trigger_route::texts::lang_for(&state.home_dir);
    vec![crate::trigger_route::list::line_list_message(
        &agents, page, lang,
    )]
}

#[cfg(test)]
#[path = "trigger_tests.rs"]
mod tests;
