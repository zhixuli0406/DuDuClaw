//! The channel-agnostic turn decision: given one incoming message, answer it
//! directly (list, refusal, "say the task"), run it as a called employee, or
//! leave it to normal routing. Used by the shared channel reply pipeline and
//! by adapters that render the `@` list with buttons (Telegram).

use std::path::Path;

use duduclaw_agent::registry::AgentRegistry;

use super::access::{self, SenderAccess};
use super::list::{self, CallableAgent};
use super::pending::{self, PendingKey};
use super::{TriggerRoute, audit, decide};

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
    pub text: &'a str,
    /// The conversation's default employee.
    pub fallback: Option<&'a str>,
    /// Identifies this event (for idempotent one-shot claims).
    pub event: &'a str,
    /// Order key, increasing with arrival order.
    pub seq: i64,
    /// Skip logging/audit (a route recomputed for a durable-ingress
    /// snapshot; the run that answers the event logs it once).
    pub quiet: bool,
}

/// What to do with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnDecision {
    /// Normal routing: the default employee answers.
    Default,
    /// Run the model as this employee. `via_trigger` = chosen by `@` or a
    /// one-shot addressee (the reply is labelled with the employee's name).
    Agent { id: String, via_trigger: bool },
    /// Answer with `text` and no model call. `list` carries the callable
    /// employees when the answer is the `@` list (adapters may render
    /// buttons instead of the text list).
    Reply {
        text: String,
        list: Option<Vec<CallableAgent>>,
    },
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
/// addressee rules. Split out so adapters that already computed the route
/// (LINE, inside its durable-ingress snapshot) share the same wording.
pub fn decision_for_route(
    home: &Path,
    reg: &AgentRegistry,
    turn: ChannelTurn<'_>,
    access: &SenderAccess,
    route: TriggerRoute,
) -> TurnDecision {
    let key = PendingKey::new(turn.channel, turn.conversation, turn.sender);
    match route {
        TriggerRoute::None => {
            if turn.sender.is_empty() {
                return TurnDecision::Default;
            }
            match claim_valid(home, reg, &key, turn) {
                Some(id) => TurnDecision::Agent {
                    via_trigger: turn.fallback != Some(id.as_str()),
                    id,
                },
                None => TurnDecision::Default,
            }
        }
        TriggerRoute::Routed { agent, bare } => {
            if !turn.quiet {
                audit(home, turn.channel, access, turn.sender, &agent, "allowed");
            }
            if bare && !turn.sender.is_empty() {
                pending::arm(&key, &agent, turn.event, turn.seq);
                return TurnDecision::Reply {
                    text: super::ask_for_task_text(&super::display_name(reg, &agent)),
                    list: None,
                };
            }
            pending::cancel(&key, turn.event, turn.seq);
            TurnDecision::Agent {
                via_trigger: turn.fallback != Some(agent.as_str()),
                id: agent,
            }
        }
        TriggerRoute::Forbidden { agent } => {
            if !turn.quiet {
                audit(home, turn.channel, access, turn.sender, &agent, "refused");
            }
            TurnDecision::Reply {
                text: super::FORBIDDEN_TEXT.to_string(),
                list: None,
            }
        }
        TriggerRoute::List { candidates, .. } => {
            let agents = list::callable_agents(reg, &candidates);
            TurnDecision::Reply {
                text: list::text_list(&agents),
                list: Some(agents),
            }
        }
    }
}

/// Claim the one-shot addressee for this event, re-checking that the sender
/// may still call that employee (a binding can be revoked while it waits).
fn claim_valid(
    home: &Path,
    reg: &AgentRegistry,
    key: &PendingKey,
    turn: ChannelTurn<'_>,
) -> Option<String> {
    let id = pending::claim(key, turn.event, turn.seq)?;
    let agent = reg
        .get(&id)
        .filter(|a| a.config.agent.status.is_operational())?;
    let access = access::for_channel_sender(home, turn.channel, turn.sender);
    if access.may_call(agent, turn.fallback) {
        Some(id)
    } else {
        if !turn.quiet {
            audit(home, turn.channel, &access, turn.sender, &id, "refused");
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
    if turn.sender.is_empty() {
        return super::FORBIDDEN_TEXT.to_string();
    }
    let access = access::for_channel_sender(home, turn.channel, turn.sender);
    let callable = reg
        .get(agent)
        .filter(|a| a.config.agent.status.is_operational())
        .is_some_and(|a| access.may_call(a, turn.fallback));
    if !callable {
        audit(home, turn.channel, &access, turn.sender, agent, "refused");
        return super::FORBIDDEN_TEXT.to_string();
    }
    audit(home, turn.channel, &access, turn.sender, agent, "allowed");
    let key = PendingKey::new(turn.channel, turn.conversation, turn.sender);
    pending::arm(&key, agent, turn.event, turn.seq);
    super::ask_for_task_text(&super::display_name(reg, agent))
}

/// Whether `text` calls any employee at all (or asks for the list),
/// regardless of who sent it — the "is the bot addressed" test for
/// mention-only group modes. Permission is applied later, per sender.
pub fn calls_anyone(reg: &AgentRegistry, text: &str) -> bool {
    let everyone = SenderAccess {
        sender: super::Sender::Admin,
        channel: true,
    };
    super::decide(reg, text, &everyone, None).addresses_bot()
}
