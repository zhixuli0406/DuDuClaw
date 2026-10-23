//! Calling an AI employee by `@name`, limited to the employees the sender may
//! call (owner decision D6, 2026-10-21).
//!
//! Every message is routed on its own; nothing sticks. A message that calls
//! nobody goes to the conversation's default employee (the one
//! `AgentResolver` picks: channel binding, `allowed_channels`, main). A
//! message that calls an employee by `@trigger`, `@display name` or `@id`
//! goes to that employee only if the sender may call them:
//!
//! | sender | may call |
//! |---|---|
//! | dashboard Admin | everyone |
//! | dashboard user (WebChat), or a messaging-app sender mapped to one (verified `channel_identities`) | employees bound to the account (any level); for a Manager also the employees bound to anyone below them (`users.manager_user_id`, at most Operator); plus the default employee and `[channels] public_trigger` employees |
//! | anyone else (LINE friend, Telegram user, web-widget visitor) | the default employee and `public_trigger` employees |
//!
//! The account part is read live from `users.db` ([`access::load_account`]):
//! a binding, role or manager change reaches the next message (the caches
//! are keyed by `auth_meta.acl_generation`).
//!
//! A message that only calls employees the sender may not call is refused
//! without a model call when the sender is a dashboard account (directly or
//! through a verified channel identity); for an unmapped sender such a name
//! is treated like a name that matches nobody (owner decision: no employee
//! enumeration by outsiders). A lone `@` (or a partial or ambiguous name) asks for
//! the list of callable employees. On channels, `@name` with no task arms a
//! one-shot addressee ([`pending`]): only the sender's next message in that
//! conversation goes to that employee.
//!
//! Matching is in [`duduclaw_agent::trigger_match`]; this module adds the
//! sender's permissions, the one-shot state and the channel replies.

pub mod access;
pub mod assign;
mod assign_audit;
pub(crate) mod assign_create;
pub mod assign_texts;
pub mod list;
pub mod pending;
pub(crate) mod schedule_chat;
mod schedule_texts;
pub mod spans;
pub mod texts;
pub mod turn;

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod team_tests;

use duduclaw_agent::registry::{AgentRegistry, LoadedAgent};
use duduclaw_agent::trigger_match::{self, MentionScan, MentionTarget};
use duduclaw_auth::GrantSource;

pub use access::{Sender, SenderAccess};

/// What a message asks for, after permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerRoute {
    /// Calls nobody usable: normal routing.
    None,
    /// Calls `agent`, whom the sender may call. `bare` = the message is only
    /// the name, with no task.
    Routed { agent: String, bare: bool },
    /// Calls only employees the sender may not call (the first one named).
    Forbidden { agent: String },
    /// A lone `@`, or a partial / ambiguous name: the callable employees it
    /// could mean. Never routes on its own.
    List {
        token: String,
        candidates: Vec<String>,
    },
}

impl TriggerRoute {
    /// The message addresses the bot (for "mention only" group modes).
    pub fn addresses_bot(&self) -> bool {
        !matches!(self, TriggerRoute::None)
    }
}

/// Cheap pre-check: a message without any `@` cannot call anyone, so callers
/// may skip the identity lookup entirely.
pub fn may_mention(text: &str) -> bool {
    text.contains('@') || text.contains('＠')
}

/// Employees a mention may name: operational ones only (a paused, archived
/// or deleted employee cannot be called), in id order for stable output.
pub fn mention_targets(reg: &AgentRegistry) -> Vec<MentionTarget> {
    let mut agents: Vec<&LoadedAgent> = reg
        .list()
        .into_iter()
        .filter(|a| a.config.agent.status.is_operational())
        .collect();
    agents.sort_by(|a, b| a.config.agent.name.cmp(&b.config.agent.name));
    agents
        .iter()
        .map(|a| {
            MentionTarget::new(
                &a.config.agent.name,
                &a.config.agent.display_name,
                &a.config.agent.trigger,
            )
        })
        .collect()
}

/// Decide what `text` asks for. `fallback` is the conversation's default
/// employee (who answers when nobody is called). A chat command (`/goal …`,
/// a safety word) never calls anyone: its `@name` is an argument, not an
/// addressee, so it must not arm a one-shot addressee either.
pub fn decide(
    reg: &AgentRegistry,
    text: &str,
    access: &SenderAccess,
    fallback: Option<&str>,
) -> TriggerRoute {
    if !may_mention(text) || crate::chat_commands::is_command(text) {
        return TriggerRoute::None;
    }
    let targets = mention_targets(reg);
    let may_call = |id: &str| reg.get(id).is_some_and(|a| access.may_call(a, fallback));
    match trigger_match::scan_mentions(text, &targets, &may_call) {
        MentionScan::None => TriggerRoute::None,
        MentionScan::Hit(hit) => TriggerRoute::Routed {
            bare: trigger_match::task_text_without(text, &hit).is_empty(),
            agent: hit.id,
        },
        // Only a sender we can name (a dashboard account, directly or
        // through a verified channel identity) is told a call was refused.
        // For an unmapped sender (a LINE friend, a Telegram user, a website
        // or Odoo Live Chat visitor) a name they may not call reads like a
        // name that matches nobody: plain text for the default employee, so
        // the refusal cannot be used to find out which employees exist.
        MentionScan::Forbidden(_) if matches!(access.sender, Sender::Unmapped) => TriggerRoute::None,
        MentionScan::Forbidden(hit) => TriggerRoute::Forbidden { agent: hit.id },
        MentionScan::Partial { token, candidates } => TriggerRoute::List { token, candidates },
    }
}

/// Every employee `access` may call, in id order (the `@` list).
pub fn callable_ids(
    reg: &AgentRegistry,
    access: &SenderAccess,
    fallback: Option<&str>,
) -> Vec<String> {
    callable_grants(reg, access, fallback)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Every employee `access` may call, in id order, with why. Uses exactly
/// the predicate [`decide`] uses, so the list and the router never disagree.
pub fn callable_grants(
    reg: &AgentRegistry,
    access: &SenderAccess,
    fallback: Option<&str>,
) -> Vec<(String, GrantSource)> {
    mention_targets(reg)
        .into_iter()
        .filter_map(|t| {
            let grant = reg.get(&t.id).and_then(|a| access.grant(a, fallback))?;
            Some((t.id, grant))
        })
        .collect()
}

/// Why `access` may call `agent` (for the audit trail), if they may.
pub fn grant_for(
    reg: &AgentRegistry,
    access: &SenderAccess,
    agent: &str,
    fallback: Option<&str>,
) -> Option<GrantSource> {
    reg.get(agent).and_then(|a| access.grant(a, fallback))
}

/// The display name of `id`, or the id itself.
pub fn display_name(reg: &AgentRegistry, id: &str) -> String {
    reg.get(id)
        .map(|a| a.config.agent.display_name.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_string())
}

/// Prefix for a channel reply from an employee other than the
/// conversation's default one, so the reader can see who answered. The
/// default employee's replies carry no prefix.
pub fn reply_prefix(display_name: &str) -> String {
    format!("【{display_name}】")
}

/// `reply` with the answering employee's prefix, unless it is empty (an
/// empty reply means "send nothing" and must stay empty).
pub fn with_reply_prefix(reply: String, display_name: &str) -> String {
    if reply.trim().is_empty() {
        return reply;
    }
    format!("{}\n{reply}", reply_prefix(display_name))
}

/// The refusal for calling an employee the sender may not call (zh-TW; the
/// channel paths use [`texts::forbidden`] in the configured language). It
/// does not name the employee; it does confirm that the name belongs to one
/// (owner decision: an explicit refusal beats silently answering as the
/// default employee).
pub const FORBIDDEN_TEXT: &str =
    "你沒有呼叫這位 AI 員工的權限，這則訊息沒有送出。輸入「@」可以查看你能呼叫的 AI 員工。";

/// The prompt after `@name` with no task.
pub fn ask_for_task_text(lang: texts::Lang, display_name: &str) -> String {
    texts::ask_for_task(lang, display_name, pending::PENDING_TTL.as_secs() / 60)
}

/// The session a called employee answers in: its own bucket inside the
/// conversation, so history, secret restoration and pending goal state never
/// mix between the default employee and the one called by `@`.
pub fn called_session(session_id: &str, agent: &str) -> String {
    format!("{session_id}#agent:{agent}")
}

/// At most one `@` list and one refusal per sender per conversation every
/// [`DIRECT_REPLY_INTERVAL`]; repeats inside the window are answered with
/// silence (saves the platform's message quota under spam).
pub const DIRECT_REPLY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// `true` when a list/refusal may be sent to this sender now.
pub fn allow_direct_reply(
    channel: &str,
    conversation: &str,
    sender: &str,
    kind: turn::ReplyKind,
) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    const MAX: usize = 4_096;
    type Key = (String, String, String, turn::ReplyKind);
    static LAST: OnceLock<Mutex<HashMap<Key, Instant>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if last.len() >= MAX {
        last.retain(|_, t| now.saturating_duration_since(*t) < DIRECT_REPLY_INTERVAL);
        if last.len() >= MAX {
            last.clear();
        }
    }
    let key = (
        channel.to_string(),
        conversation.to_string(),
        sender.to_string(),
        kind,
    );
    match last.get(&key) {
        Some(t) if now.saturating_duration_since(*t) < DIRECT_REPLY_INTERVAL => false,
        _ => {
            last.insert(key, now);
            true
        }
    }
}

/// Log a trigger-routing decision. Allowed switches log at info with where
/// the right came from (`via`: `admin`, `binding`, `team:<user>:<depth>`,
/// `default`, `public`); refusals also go to `security_audit.jsonl` so they
/// can be reviewed later. The sender is recorded as the dashboard account
/// when mapped, else as an HMAC of the channel id under a per-home key
/// (never the raw platform id).
pub fn audit(
    home: &std::path::Path,
    channel: &str,
    access: &SenderAccess,
    channel_sender: &str,
    agent: &str,
    outcome: &'static str,
    via: Option<&GrantSource>,
) {
    let who = access.audit_label(home, channel, channel_sender);
    let via = via.map(GrantSource::label).unwrap_or_default();
    tracing::info!(channel, sender = %who, agent, outcome, via = %via, "trigger routing decision");
    if outcome == "refused" && !recently_audited(&who, agent) {
        let event = duduclaw_security::audit::AuditEvent::new(
            "trigger_route_refused",
            agent,
            duduclaw_security::audit::Severity::Warning,
            serde_json::json!({ "channel": channel, "sender": who }),
        );
        duduclaw_security::audit::append_audit_event(home, &event);
    }
}

/// The employee that answered one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsweredBy {
    pub id: String,
    pub display_name: String,
    /// Chosen by `@` (or a one-shot addressee) rather than being the
    /// conversation's default employee.
    pub via_trigger: bool,
    /// The session the turn ran in ([`called_session`] for a called
    /// employee); secret restoration must use the same one.
    pub session_id: String,
}

tokio::task_local! {
    /// Set by a channel adapter around a reply built for an employee called
    /// by `@` (or a one-shot addressee): the reply pipeline then runs the
    /// turn in that employee's own session bucket ([`called_session`]) after
    /// the access gate and takeover checks ran on the conversation itself.
    pub static CALLED_TURN: ();
}

/// Run `fut` as a called-employee turn when `called` is true.
pub async fn as_called_turn<F: std::future::Future>(called: bool, fut: F) -> F::Output {
    if called {
        CALLED_TURN.scope((), fut).await
    } else {
        fut.await
    }
}

/// Whether the current reply is a called-employee turn.
pub fn in_called_turn() -> bool {
    CALLED_TURN.try_with(|_| ()).is_ok()
}

tokio::task_local! {
    /// Set by an employee's own bot (its own Telegram bot or Google Chat app)
    /// around the reply it builds: the turn runs in that employee's own
    /// session bucket ([`called_session`]) after the access gate and takeover
    /// checks ran on the conversation itself — exactly like a called turn,
    /// but it is not a call (no name prefix, no `@` semantics). Without it two
    /// employees' bots in one chat (and every Telegram DM: a person has the
    /// same private-chat id with every bot) shared one session, and the second
    /// employee's turn failed on the session-ownership check.
    pub static OWN_BOT_TURN: ();
}

/// Run `fut` as a turn answered by an employee's own bot.
pub async fn as_own_bot_turn<F: std::future::Future>(fut: F) -> F::Output {
    OWN_BOT_TURN.scope((), fut).await
}

/// Whether the current reply is answered by an employee's own bot.
pub fn in_own_bot_turn() -> bool {
    OWN_BOT_TURN.try_with(|_| ()).is_ok()
}

tokio::task_local! {
    /// Set by the reply builder for one turn; read by its wrapper so the
    /// reply is restored, contract-checked and labelled as the employee that
    /// actually answered.
    pub static ANSWERED_BY: std::sync::Arc<std::sync::Mutex<Option<AnsweredBy>>>;
}

/// Record who answers the current turn (no-op outside a reply scope).
pub fn record_answered_by(who: AnsweredBy) {
    let _ = ANSWERED_BY.try_with(|slot| {
        if let Ok(mut s) = slot.lock() {
            *s = Some(who);
        }
    });
}

/// One audit row per (sender, employee) per minute: a sender repeating a
/// refused call cannot grow `security_audit.jsonl` without bound.
fn recently_audited(who: &str, agent: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    const WINDOW: Duration = Duration::from_secs(60);
    const MAX: usize = 4_096;
    static SEEN: OnceLock<Mutex<HashMap<(String, String), Instant>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if seen.len() >= MAX {
        seen.retain(|_, t| now.saturating_duration_since(*t) < WINDOW);
        if seen.len() >= MAX {
            seen.clear();
        }
    }
    let key = (who.to_string(), agent.to_string());
    match seen.get(&key) {
        Some(t) if now.saturating_duration_since(*t) < WINDOW => true,
        _ => {
            seen.insert(key, now);
            false
        }
    }
}
