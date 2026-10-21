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
//! | dashboard user (WebChat) | employees bound to the account (any level) |
//! | messaging-app sender mapped to a dashboard account (verified `channel_identities`) | bound employees, plus the default employee and `[channels] public_trigger` employees |
//! | anyone else (LINE friend, Telegram user, web-widget visitor) | the default employee and `public_trigger` employees |
//!
//! A message that only calls employees the sender may not call is refused
//! without a model call. A lone `@` (or a partial or ambiguous name) asks for
//! the list of callable employees. On channels, `@name` with no task arms a
//! one-shot addressee ([`pending`]): only the sender's next message in that
//! conversation goes to that employee.
//!
//! Matching is in [`duduclaw_agent::trigger_match`]; this module adds the
//! sender's permissions, the one-shot state and the channel replies.

pub mod access;
pub mod list;
pub mod pending;
pub mod turn;

#[cfg(test)]
pub(crate) mod tests;

use duduclaw_agent::registry::{AgentRegistry, LoadedAgent};
use duduclaw_agent::trigger_match::{self, MentionScan, MentionTarget};

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
/// employee (who answers when nobody is called).
pub fn decide(
    reg: &AgentRegistry,
    text: &str,
    access: &SenderAccess,
    fallback: Option<&str>,
) -> TriggerRoute {
    if !may_mention(text) {
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
    mention_targets(reg)
        .into_iter()
        .filter(|t| reg.get(&t.id).is_some_and(|a| access.may_call(a, fallback)))
        .map(|t| t.id)
        .collect()
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

/// The refusal for calling an employee the sender may not call. It does not
/// name the employee; it does confirm that the name belongs to one (a name
/// that matches nobody is ordinary text for the default employee).
pub const FORBIDDEN_TEXT: &str =
    "你沒有呼叫這位 AI 員工的權限，這則訊息沒有送出。輸入「@」可以查看你能呼叫的 AI 員工。";

/// The refusal when a WebChat account may not use the default employee.
pub const DEFAULT_FORBIDDEN_TEXT: &str =
    "你沒有與預設 AI 員工對話的權限。請選擇你可以使用的 AI 員工，或輸入「@」呼叫。";

/// Shown when nobody is callable (empty `@` list).
pub const NOBODY_CALLABLE_TEXT: &str = "目前沒有你可以呼叫的 AI 員工。";

/// The prompt after `@name` with no task.
pub fn ask_for_task_text(display_name: &str) -> String {
    format!(
        "請說要交給「{display_name}」的事，你的下一則訊息會直接交給他（{} 分鐘內有效）。",
        pending::PENDING_TTL.as_secs() / 60
    )
}

/// Log a trigger-routing decision. Allowed switches log at info; refusals
/// also go to `security_audit.jsonl` so they can be reviewed later. The
/// sender is recorded as the dashboard account when mapped, else as a short
/// digest of the channel id (never the raw platform id).
pub fn audit(
    home: &std::path::Path,
    channel: &str,
    access: &SenderAccess,
    channel_sender: &str,
    agent: &str,
    outcome: &'static str,
) {
    let who = access.audit_label(channel, channel_sender);
    tracing::info!(channel, sender = %who, agent, outcome, "trigger routing decision");
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
