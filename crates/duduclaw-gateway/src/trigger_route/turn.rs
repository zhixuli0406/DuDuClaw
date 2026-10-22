//! The channel-agnostic turn decision: given one incoming message, answer it
//! directly (list, refusal, "say the task"), run it as a called employee, or
//! leave it to normal routing. Used by the shared channel reply pipeline, by
//! the Telegram shared bot and by LINE's durable ingress.

use std::path::Path;

use duduclaw_agent::registry::AgentRegistry;

use super::access::{self, SenderAccess};
use super::list::{self, CallableAgent};
use super::pending::{self, PendingKey};
use super::texts::{self, Lang};
use super::{TriggerRoute, audit, decide};

/// What a decision may do with the one-shot addressee store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingPolicy<'a> {
    /// Arm, claim and cancel (the one pass that owns the event).
    Live,
    /// Read only: what `Live` would decide, without changing anything (gates
    /// that run before the message is accepted).
    Peek,
    /// Recompute an event whose addressee was already resolved and stored
    /// (LINE durable ingress): never touch the store; a message that calls
    /// nobody goes to the stored addressee, re-checked against the sender.
    Replay(Option<&'a str>),
}

/// One incoming channel message.
#[derive(Debug, Clone, Copy)]
pub struct ChannelTurn<'a> {
    /// Channel name (`telegram`, `discord`, …): the `channel_identities` key.
    pub channel: &'a str,
    /// Conversation key (the session id).
    pub conversation: &'a str,
    /// The sender's platform user id; empty when unknown (no one-shot state
    /// can be kept for an unknown sender).
    pub sender: &'a str,
    /// The text to match (native person mentions already blanked).
    pub text: &'a str,
    /// The conversation's default employee.
    pub fallback: Option<&'a str>,
    /// Identifies this event (for idempotent one-shot claims).
    pub event: &'a str,
    /// Order key, increasing with arrival order.
    pub seq: i64,
    /// Skip logging/audit.
    pub quiet: bool,
    pub pending: PendingPolicy<'a>,
}

/// Why the bot answers directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplyKind {
    /// The `@` list (or the choice for a partial / shared name).
    List,
    /// Calling an employee the sender may not call.
    Refusal,
    /// "Say the task" after `@name` alone.
    AskTask,
}

/// What to do with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnDecision {
    /// Normal routing: the default employee answers.
    Default,
    /// Run the model as this employee. `via_trigger` = chosen by `@` or a
    /// one-shot addressee (the reply is labelled and runs in the employee's
    /// own session, [`super::called_session`]).
    Agent { id: String, via_trigger: bool },
    /// Answer with `text` and no model call. `list` carries the callable
    /// employees when the answer is the `@` list (adapters may render
    /// buttons instead of the text list).
    Reply {
        text: String,
        list: Option<Vec<CallableAgent>>,
        kind: ReplyKind,
    },
}

impl TurnDecision {
    /// The message addresses the bot (for "mention only" group modes).
    pub fn addresses_bot(&self) -> bool {
        !matches!(self, TurnDecision::Default)
    }
}

/// Decide one message. Identity is looked up only when the text can mention
/// someone or a one-shot addressee is waiting.
pub fn decide_channel_turn(
    home: &Path,
    reg: &AgentRegistry,
    turn: ChannelTurn<'_>,
) -> TurnDecision {
    let access = if super::may_mention(turn.text) {
        access::for_channel_sender(home, turn.channel, turn.sender)
    } else {
        SenderAccess::unmapped()
    };
    let route = decide(reg, turn.text, &access, turn.fallback);
    decision_for_route(home, reg, turn, &access, route)
}

/// Turn a [`TriggerRoute`] into a [`TurnDecision`], applying the one-shot
/// addressee rules of `turn.pending`.
pub fn decision_for_route(
    home: &Path,
    reg: &AgentRegistry,
    turn: ChannelTurn<'_>,
    access: &SenderAccess,
    route: TriggerRoute,
) -> TurnDecision {
    let key = PendingKey::new(turn.channel, turn.conversation, turn.sender);
    let lang = texts::lang_for(home);
    match route {
        TriggerRoute::None => match addressee(home, reg, &key, turn) {
            Some(id) => TurnDecision::Agent {
                via_trigger: turn.fallback != Some(id.as_str()),
                id,
            },
            None => TurnDecision::Default,
        },
        TriggerRoute::Routed { agent, bare } => {
            if !turn.quiet {
                let via = super::grant_for(reg, access, &agent, turn.fallback);
                audit(home, turn.channel, access, turn.sender, &agent, "allowed", via.as_ref());
            }
            if bare && !turn.sender.is_empty() {
                if turn.pending == PendingPolicy::Live {
                    pending::arm(&key, &agent, turn.event, turn.seq);
                }
                return TurnDecision::Reply {
                    text: super::ask_for_task_text(lang, &super::display_name(reg, &agent)),
                    list: None,
                    kind: ReplyKind::AskTask,
                };
            }
            if turn.pending == PendingPolicy::Live {
                pending::cancel(&key, turn.event, turn.seq);
            }
            TurnDecision::Agent {
                via_trigger: turn.fallback != Some(agent.as_str()),
                id: agent,
            }
        }
        TriggerRoute::Forbidden { agent } => {
            if !turn.quiet {
                audit(home, turn.channel, access, turn.sender, &agent, "refused", None);
            }
            TurnDecision::Reply {
                text: texts::forbidden(lang).to_string(),
                list: None,
                kind: ReplyKind::Refusal,
            }
        }
        TriggerRoute::List { candidates, .. } => {
            let agents = list::callable_agents(reg, &candidates);
            TurnDecision::Reply {
                text: list::text_list(&agents, lang),
                list: Some(agents),
                kind: ReplyKind::List,
            }
        }
    }
}

/// The one-shot addressee for a message that calls nobody, per the turn's
/// [`PendingPolicy`], re-checked against the sender's current access (a
/// binding can be revoked while it waits).
fn addressee(
    home: &Path,
    reg: &AgentRegistry,
    key: &PendingKey,
    turn: ChannelTurn<'_>,
) -> Option<String> {
    if turn.sender.is_empty() {
        return None;
    }
    let id = match turn.pending {
        PendingPolicy::Live => pending::claim(key, turn.event, turn.seq)?,
        PendingPolicy::Peek => pending::peek(key, turn.seq)?,
        PendingPolicy::Replay(stored) => stored?.to_string(),
    };
    if turn.fallback == Some(id.as_str()) {
        return Some(id);
    }
    let agent = reg
        .get(&id)
        .filter(|a| a.config.agent.status.is_operational())?;
    let access = access::for_channel_sender(home, turn.channel, turn.sender);
    if access.may_call(agent, turn.fallback) {
        Some(id)
    } else {
        if !turn.quiet {
            audit(home, turn.channel, &access, turn.sender, &id, "refused", None);
        }
        None
    }
}

/// Whether a one-shot addressee is waiting for this sender (for "mention
/// only" gates, which run before the message is handled).
pub fn has_pending(channel: &str, conversation: &str, sender: &str, seq: i64) -> bool {
    !sender.is_empty()
        && pending::peek(&PendingKey::new(channel, conversation, sender), seq).is_some()
}

/// A tap on an employee in the `@` list (Telegram inline keyboard): arm the
/// one-shot addressee for the presser after re-checking they may call that
/// employee, and return the text to send.
pub fn pick_from_list(
    home: &Path,
    reg: &AgentRegistry,
    turn: ChannelTurn<'_>,
    agent: &str,
) -> String {
    let lang = texts::lang_for(home);
    if turn.sender.is_empty() {
        return texts::forbidden(lang).to_string();
    }
    let access = access::for_channel_sender(home, turn.channel, turn.sender);
    let callable = reg
        .get(agent)
        .filter(|a| a.config.agent.status.is_operational())
        .is_some_and(|a| access.may_call(a, turn.fallback));
    if !callable {
        audit(home, turn.channel, &access, turn.sender, agent, "refused", None);
        return texts::forbidden(lang).to_string();
    }
    let via = super::grant_for(reg, &access, agent, turn.fallback);
    audit(home, turn.channel, &access, turn.sender, agent, "allowed", via.as_ref());
    let key = PendingKey::new(turn.channel, turn.conversation, turn.sender);
    pending::arm(&key, agent, turn.event, turn.seq);
    super::ask_for_task_text(lang, &super::display_name(reg, agent))
}

/// Whether `text` calls any employee at all (or asks for the list),
/// regardless of who sent it — the "is the bot addressed" test for
/// mention-only group modes. Permission is applied later, per sender.
pub fn calls_anyone(reg: &AgentRegistry, text: &str) -> bool {
    let everyone = SenderAccess {
        sender: super::Sender::Admin { user_id: None },
    };
    super::decide(reg, text, &everyone, None).addresses_bot()
}

/// [`calls_anyone`] for one sender: an unmapped sender's name for an
/// employee they may not call reads like an unknown name, so it does not
/// count as addressing the bot either (mention-only groups stay silent,
/// exactly as for a name that matches nobody).
pub fn calls_anyone_as(
    home: &Path,
    reg: &AgentRegistry,
    channel: &str,
    sender: &str,
    text: &str,
    fallback: Option<&str>,
) -> bool {
    if !super::may_mention(text) {
        return false;
    }
    let access = access::for_channel_sender(home, channel, sender);
    super::decide(reg, text, &access, fallback).addresses_bot()
}

/// Whether this reply language is used for `home` (re-exported for adapters
/// building their own direct replies).
pub fn lang(home: &Path) -> Lang {
    texts::lang_for(home)
}
