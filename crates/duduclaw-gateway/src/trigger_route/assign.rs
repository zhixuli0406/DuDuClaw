//! Assigning a task from a chat: `/交辦 @employee <task>` (also `/assign`,
//! `／交辦`). The gateway writes the goal task itself, with no model in
//! between, so the right to assign is decided here and nowhere else:
//!
//! | sender | may assign to |
//! |---|---|
//! | dashboard Admin | every operational employee |
//! | a mapped account | employees it holds at Operator or above: a direct Owner/Operator binding, or a team grant that derived Operator |
//! | the default employee, `public_trigger` employees, a Viewer binding | nobody (they may only talk: "call only") |
//! | an unmapped sender | nobody ("link your account first"; nothing about which employees exist) |
//!
//! The task is a goal task (`goal_create_core`) stamped with
//! `on_behalf_of = user:<account>`, `created_via = <channel>` and the source
//! conversation, so goal-loop progress comes back to that chat. `/交辦 @name`
//! alone arms a one-shot [`PendingKind::Assign`]: only the sender's next
//! message becomes the task (owner decision D6: nothing sticks).

use std::path::Path;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_agent::trigger_match::{self, MentionScan};
use duduclaw_auth::{AccessLevel, GrantSource, UserContext};

use super::access::{Sender, SenderAccess};
use super::assign_texts as t;
use super::list::{self, CallableAgent};
use super::pending::{self, PendingKey, PendingKind};
use super::texts::{self, Lang};
use crate::chat_commands::GoalCommand;

/// Whether a sender may hand an employee a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignRight {
    /// Yes; the grant it comes from.
    Allowed(GrantSource),
    /// They may talk to the employee, nothing more.
    CallOnly,
    /// Neither (or the employee is not operational).
    Denied,
    /// We cannot name the sender (no verified dashboard account).
    NeedsIdentity,
}

/// What `/交辦` was asked to do (parsed by `chat_commands`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignCommand {
    /// `/交辦` alone: the employees the sender may assign to.
    List,
    /// `/交辦 狀態` / `/assign status`: the sender's unfinished tasks.
    Status,
    /// `/交辦 <text>`: `@employee` plus the task (or the name alone).
    Text(String),
}

/// The sender of one `/交辦` (or of the message an armed `/交辦 @name` was
/// waiting for).
#[derive(Debug, Clone)]
pub struct AssignCaller<'a> {
    pub home: &'a Path,
    /// Channel name (`line`, `webchat`, `telegram`, …).
    pub channel: &'a str,
    /// The conversation's session id (`line:<chat>`, …): where progress goes.
    pub conversation: &'a str,
    /// The sender's platform id (WebChat: the account id). Empty = unknown.
    pub sender: &'a str,
    /// Identifies the incoming message when the channel has a stable id
    /// (LINE `webhookEventId`): the task id is derived from it, so a
    /// redelivered message creates nothing twice.
    pub event: Option<&'a str>,
    /// Order key of the message (see [`pending::next_seq`]).
    pub seq: i64,
    /// The conversation's default employee.
    pub fallback: Option<&'a str>,
    pub access: SenderAccess,
}

/// A created task, for clients that render a card (WebChat).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedTask {
    pub id: String,
    pub agent: String,
}

/// The bot's answer to one `/交辦`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignReply {
    pub text: String,
    /// The assignable employees, when the answer is that list (LINE renders
    /// them as quick-reply buttons that send `/交辦 @name`).
    pub list: Option<Vec<CallableAgent>>,
    pub task: Option<AssignedTask>,
}

impl AssignReply {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            list: None,
            task: None,
        }
    }
}

/// `user:<id>` for a dashboard account, or `None` for anything that is not
/// one (the gateway admin token, a system sender name, an id with
/// characters an account id never has).
pub fn principal_for_user(user_id: &str) -> Option<String> {
    let valid = !user_id.is_empty()
        && user_id.len() <= 64
        && user_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && user_id != "system"
        && !duduclaw_core::delegation_policy::is_system_sender(user_id);
    valid.then(|| format!("user:{user_id}"))
}

/// The principal of a channel sender: their dashboard account, if mapped.
pub fn principal_of(access: &SenderAccess) -> Option<String> {
    match &access.sender {
        Sender::Admin { user_id: Some(id) } | Sender::Account { user_id: id, .. } => {
            principal_for_user(id)
        }
        Sender::Admin { user_id: None } | Sender::Unmapped => None,
    }
}

/// Audit label of a dashboard caller's right to assign `agent` (`admin`,
/// `binding`, `team:<user>:<depth>`), for the activity metadata.
pub fn dashboard_grant_via(ctx: &UserContext, agent: &str) -> Option<String> {
    if ctx.is_admin() {
        return Some(GrantSource::Admin.label());
    }
    if ctx.has_agent_access(agent, AccessLevel::Operator) {
        return Some("binding".to_string());
    }
    ctx.team_access.get(agent).map(|g| {
        GrantSource::Team {
            via_user: g.via_user.clone(),
            depth: g.depth,
        }
        .label()
    })
}

/// May this sender hand `agent` a task? Unmapped senders are answered
/// before the employee is even looked up, so the reply says nothing about
/// which employees exist.
pub fn assign_right(
    reg: &AgentRegistry,
    access: &SenderAccess,
    agent: &str,
    fallback: Option<&str>,
) -> AssignRight {
    if matches!(access.sender, Sender::Unmapped) {
        return AssignRight::NeedsIdentity;
    }
    let Some(loaded) = reg
        .get(agent)
        .filter(|a| a.config.agent.status.is_operational())
    else {
        return AssignRight::Denied;
    };
    match &access.sender {
        Sender::Admin { .. } => AssignRight::Allowed(GrantSource::Admin),
        Sender::Account { access: ea, .. } => match ea.level(agent) {
            Some((level, source)) if level.at_least(AccessLevel::Operator) => {
                AssignRight::Allowed(source)
            }
            _ if access.may_call(loaded, fallback) => AssignRight::CallOnly,
            _ => AssignRight::Denied,
        },
        Sender::Unmapped => AssignRight::NeedsIdentity,
    }
}

/// Every operational employee the sender may assign to, in id order.
pub fn assignable_ids(
    reg: &AgentRegistry,
    access: &SenderAccess,
    fallback: Option<&str>,
) -> Vec<String> {
    super::mention_targets(reg)
        .into_iter()
        .filter(|t| matches!(assign_right(reg, access, &t.id, fallback), AssignRight::Allowed(_)))
        .map(|t| t.id)
        .collect()
}

/// What to do, decided under the registry lock (no I/O except the
/// identity data already in `caller.access`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Plan {
    Reply(AssignReply),
    /// Arm the one-shot assign addressee and ask for the task.
    Arm { agent: String, name: String },
    Create(CreateSpec),
    Status,
}

/// One task to create.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreateSpec {
    pub agent: String,
    pub agent_name: String,
    pub grant: GrantSource,
    pub goal: GoalCommand,
}

/// Decide what `/交辦` asks for.
pub(crate) fn plan(reg: &AgentRegistry, lang: Lang, cmd: &AssignCommand, caller: &AssignCaller<'_>) -> Plan {
    if matches!(caller.access.sender, Sender::Unmapped) {
        return refused(caller, None, "needs_identity", t::needs_identity(lang).to_string());
    }
    match cmd {
        AssignCommand::Status => Plan::Status,
        AssignCommand::List => Plan::Reply(assignable_list(reg, lang, caller, None)),
        AssignCommand::Text(text) => plan_text(reg, lang, text, caller),
    }
}

fn plan_text(reg: &AgentRegistry, lang: Lang, text: &str, caller: &AssignCaller<'_>) -> Plan {
    let targets = super::mention_targets(reg);
    let allowed =
        |id: &str| matches!(assign_right(reg, &caller.access, id, caller.fallback), AssignRight::Allowed(_));
    match trigger_match::scan_mentions(text, &targets, &allowed) {
        MentionScan::Hit(hit) => {
            let AssignRight::Allowed(grant) = assign_right(reg, &caller.access, &hit.id, caller.fallback)
            else {
                return Plan::Reply(AssignReply::text(texts::forbidden(lang)));
            };
            let name = super::display_name(reg, &hit.id);
            let task = trigger_match::task_text_without(text, &hit);
            if task.is_empty() {
                return Plan::Arm { agent: hit.id, name };
            }
            spec_for(hit.id, name, grant, &task, lang)
        }
        MentionScan::Forbidden(hit) => {
            match assign_right(reg, &caller.access, &hit.id, caller.fallback) {
                AssignRight::CallOnly => {
                    let name = super::display_name(reg, &hit.id);
                    refused(caller, Some(&hit.id), "call_only", t::call_only(lang, &name))
                }
                AssignRight::NeedsIdentity => {
                    refused(caller, None, "needs_identity", t::needs_identity(lang).to_string())
                }
                _ => refused(caller, Some(&hit.id), "denied", texts::forbidden(lang).to_string()),
            }
        }
        MentionScan::Partial { candidates, .. } => {
            Plan::Reply(assignable_list(reg, lang, caller, Some(&candidates)))
        }
        MentionScan::None => Plan::Reply(AssignReply::text(t::usage(lang))),
    }
}

/// The task text (with optional `||` segments) as a goal.
fn spec_for(agent: String, agent_name: String, grant: GrantSource, task: &str, lang: Lang) -> Plan {
    match crate::chat_commands::parse_goal_args(Some(task)) {
        goal @ GoalCommand::Create { .. } => Plan::Create(CreateSpec {
            agent,
            agent_name,
            grant,
            goal,
        }),
        // `status` alone, or a malformed `時限:` segment.
        _ => Plan::Reply(AssignReply::text(t::usage(lang))),
    }
}

/// The plan for the message an armed `/交辦 @name` was waiting for: the
/// whole text is the task, and the right is checked again now.
pub(crate) fn plan_pending(reg: &AgentRegistry, lang: Lang, agent: &str, text: &str, caller: &AssignCaller<'_>) -> Plan {
    match assign_right(reg, &caller.access, agent, caller.fallback) {
        AssignRight::Allowed(grant) => {
            let name = super::display_name(reg, agent);
            spec_for(agent.to_string(), name, grant, text.trim(), lang)
        }
        AssignRight::NeedsIdentity => refused(caller, None, "needs_identity", t::needs_identity(lang).to_string()),
        AssignRight::CallOnly => {
            let name = super::display_name(reg, agent);
            refused(caller, Some(agent), "call_only", t::call_only(lang, &name))
        }
        AssignRight::Denied => refused(caller, Some(agent), "denied", texts::forbidden(lang).to_string()),
    }
}

/// The assignable list (optionally limited to `only`, the candidates of a
/// partial name), as text plus the entries for button rendering.
fn assignable_list(
    reg: &AgentRegistry,
    lang: Lang,
    caller: &AssignCaller<'_>,
    only: Option<&[String]>,
) -> AssignReply {
    let ids: Vec<String> = assignable_ids(reg, &caller.access, caller.fallback)
        .into_iter()
        .filter(|id| only.is_none_or(|o| o.contains(id)))
        .collect();
    let agents = list::callable_agents(reg, &ids);
    let text = if agents.is_empty() {
        t::nobody_assignable(lang).to_string()
    } else {
        let mut out = t::list_heading(lang, &agents[0].call_text);
        for a in &agents {
            out.push_str("\n• ");
            out.push_str(&a.call_text);
        }
        out
    };
    AssignReply {
        text,
        list: (!agents.is_empty()).then_some(agents),
        task: None,
    }
}

/// A refusal: audited, and on LINE and Telegram (metered or group-heavy
/// channels whose adapters skip an empty reply) sent at most once per sender
/// per conversation every [`super::DIRECT_REPLY_INTERVAL`], like the `@`
/// refusal; repeats inside the window get no answer. Other channels always
/// answer.
fn refused(caller: &AssignCaller<'_>, agent: Option<&str>, reason: &'static str, text: String) -> Plan {
    super::assign_audit::refused(caller, agent.unwrap_or(""), reason);
    let throttled = matches!(caller.channel, "line" | "telegram")
        && !super::allow_direct_reply(
            caller.channel,
            caller.conversation,
            caller.sender,
            super::turn::ReplyKind::Refusal,
        );
    Plan::Reply(AssignReply::text(if throttled { String::new() } else { text }))
}

/// For a channel that admits an event before handling it (LINE durable
/// ingress): when `text` is `/交辦 @name` alone and the sender may assign to
/// that employee, arm the one-shot assign addressee now, so the sender's
/// next message (admitted right after) already finds it. Handling the event
/// later arms again from the same event, which changes nothing. Silent: no
/// reply, no audit (the handling pass does both).
pub fn arm_on_admission(reg: &AgentRegistry, text: &str, caller: &AssignCaller<'_>) {
    let Some(rest) = crate::chat_commands::strip_assign_command(text) else {
        return;
    };
    let AssignCommand::Text(rest) = crate::chat_commands::parse_assign_args(rest) else {
        return;
    };
    if caller.sender.is_empty() || matches!(caller.access.sender, Sender::Unmapped) {
        return;
    }
    let targets = super::mention_targets(reg);
    let allowed =
        |id: &str| matches!(assign_right(reg, &caller.access, id, caller.fallback), AssignRight::Allowed(_));
    if let MentionScan::Hit(hit) = trigger_match::scan_mentions(&rest, &targets, &allowed) {
        if trigger_match::task_text_without(&rest, &hit).is_empty() {
            let origin = caller.event.unwrap_or_default();
            pending::arm_kind(&pending_key(caller), &hit.id, PendingKind::Assign, origin, caller.seq);
        }
    }
}

/// The one-shot key for this sender in this conversation.
pub fn pending_key(caller: &AssignCaller<'_>) -> PendingKey {
    PendingKey::new(caller.channel, caller.conversation, caller.sender)
}

/// Run a `/交辦` command.
pub async fn run(
    ctx: &crate::channel_reply::ReplyContext,
    cmd: &AssignCommand,
    caller: &AssignCaller<'_>,
) -> AssignReply {
    let home = ctx.home_dir.as_path();
    let lang = texts::lang_for(home);
    let decided = {
        let reg = ctx.registry.read().await;
        plan(&reg, lang, cmd, caller)
    };
    // Boxed: the reply pipelines that await this are already very large
    // futures (a debug build overflowed its stack with this inlined).
    Box::pin(execute(home, lang, decided, caller)).await
}

/// Run the message an armed `/交辦 @name` (for `agent`) was waiting for.
pub async fn run_pending(
    ctx: &crate::channel_reply::ReplyContext,
    agent: &str,
    text: &str,
    caller: &AssignCaller<'_>,
) -> AssignReply {
    let home = ctx.home_dir.as_path();
    let lang = texts::lang_for(home);
    let decided = {
        let reg = ctx.registry.read().await;
        plan_pending(&reg, lang, agent, text, caller)
    };
    Box::pin(execute(home, lang, decided, caller)).await
}

/// Carry out a [`Plan`].
pub(crate) async fn execute(home: &Path, lang: Lang, decided: Plan, caller: &AssignCaller<'_>) -> AssignReply {
    match decided {
        Plan::Reply(reply) => reply,
        Plan::Status => AssignReply::text(super::assign_create::status_text(home, lang, caller).await),
        Plan::Arm { agent, name } => {
            if caller.sender.is_empty() {
                return AssignReply::text(t::usage(lang));
            }
            let origin = caller
                .event
                .map(str::to_string)
                .unwrap_or_else(|| format!("assign-arm:{}", caller.seq));
            pending::arm_kind(&pending_key(caller), &agent, PendingKind::Assign, &origin, caller.seq);
            AssignReply::text(t::ask_for_task(lang, &name, pending::PENDING_TTL.as_secs() / 60))
        }
        Plan::Create(spec) => super::assign_create::create(home, lang, spec, caller).await,
    }
}

#[cfg(test)]
#[path = "assign_tests.rs"]
mod tests;
