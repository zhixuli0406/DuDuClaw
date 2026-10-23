//! Who an employee's work is for (`on_behalf_of`), carried from the turn or
//! task that started it into every spawn, queue row and approval card.
//!
//! | hop | carrier | written by | read by |
//! |---|---|---|---|
//! | chat turn → employee runtime | [`ON_BEHALF_OF`] task-local → `DUDUCLAW_ON_BEHALF_OF` | `channel_reply::guarded` (from the turn's [`RequesterIdentity`]) | spawns ([`inject_env`], [`env_pairs`]) |
//! | `send_to_agent` / `spawn_agent` → bus | `message_queue.on_behalf_of`, `bus_queue.jsonl` field | MCP server (from its env only) | dispatcher |
//! | bus → target employee | task-local → env | dispatcher ([`scope_first`]) | spawns |
//! | task → executing employee | `message_queue.on_behalf_of` copied from `tasks.on_behalf_of` | goal loop enqueue, heartbeat wake-up | dispatcher |
//! | approval raised in the run | `approvals.on_behalf_of` | approval broker (task-local, else host env) | approval cards |
//!
//! The first value set wins; a later hop never overwrites it. Tool
//! arguments are never read. A malformed stored value is dropped and audited
//! as `on_behalf_of_rejected`.
//!
//! The person is resolved by the same requester mapping the knowledge base
//! uses ([`crate::knowledge::requester_map`]): a WebChat account or a
//! verified channel identity is `user:<id>`; an unbound messaging-app sender
//! or a public visitor is `unmapped:<channel>:<keyed digest>` (the digest the
//! `@` routing audit uses, never the raw platform id); system turns name
//! nobody.

pub mod access;
pub mod context;
pub mod route;
pub mod label;
pub mod self_approval;

use std::future::Future;
use std::path::Path;

use duduclaw_core::{HumanPrincipal, PrincipalRejection, ENV_ON_BEHALF_OF};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_security::audit::{AuditEvent, Severity};

tokio::task_local! {
    /// The person the work in this task is for, in wire form (validated
    /// before it is scoped).
    pub static ON_BEHALF_OF: Option<String>;
}

/// Audit event for a principal value that was not accepted.
pub const AUDIT_REJECTED: &str = "on_behalf_of_rejected";

/// The principal in scope, if any.
pub fn current() -> Option<String> {
    ON_BEHALF_OF.try_with(Clone::clone).ok().flatten()
}

/// Run `fut` with `value` as the principal, unless one is already in scope
/// (the first writer wins). `value` must already be validated
/// ([`principal_for_requester`], [`from_stored`]).
pub fn scope_first<F: Future>(value: Option<String>, fut: F) -> impl Future<Output = F::Output> {
    let effective = current().or(value);
    ON_BEHALF_OF.scope(effective, fut)
}

/// `(DUDUCLAW_ON_BEHALF_OF, value)` when a principal is in scope, for the
/// runtimes that hand turn identity to the MCP server as explicit pairs.
pub fn env_pairs() -> Vec<(String, String)> {
    let mut pairs = current()
        .map(|v| vec![(ENV_ON_BEHALF_OF.to_string(), v)])
        .unwrap_or_default();
    pairs.extend(context::env_pair());
    pairs
}

/// Set the variable on a spawn from the scope, removing any value the
/// command would otherwise inherit from the gateway's own environment.
pub fn inject_env(cmd: &mut tokio::process::Command) {
    cmd.env_remove(ENV_ON_BEHALF_OF);
    if let Some(v) = current() {
        cmd.env(ENV_ON_BEHALF_OF, v);
    }
}

/// The principal of a chat turn's person (see the module docs).
pub fn principal_for_requester(home: &Path, who: &RequesterIdentity) -> Option<String> {
    let principal = match who {
        RequesterIdentity::DashboardUser { user_id }
        | RequesterIdentity::ChannelUser {
            user_id: Some(user_id),
            ..
        } => HumanPrincipal::user(user_id),
        RequesterIdentity::ChannelUser {
            channel,
            chat,
            sender,
            user_id: None,
        } => {
            // Public visitors carry no sender; their conversation id names
            // them instead.
            let id = if sender.is_empty() { chat } else { sender };
            if id.is_empty() {
                return None;
            }
            let digest = crate::trigger_route::access::keyed_digest(home, channel, id);
            HumanPrincipal::unmapped(channel, &digest)
        }
        RequesterIdentity::System {
            on_behalf_of: Some(user_id),
            ..
        } => HumanPrincipal::user(user_id),
        RequesterIdentity::System { .. } | RequesterIdentity::Unknown => None,
    };
    principal.map(|p| p.to_string())
}

/// A stored value (queue row, task row), validated. A malformed value is
/// audited and dropped; `source` names where it was read for the audit.
pub fn from_stored(home: &Path, raw: Option<&str>, agent: &str, source: &str) -> Option<String> {
    let raw = raw.map(str::trim).filter(|r| !r.is_empty())?;
    match HumanPrincipal::parse(raw) {
        Ok(p) => Some(p.to_string()),
        Err(rejection) => {
            audit_rejected(home, agent, source, &rejection);
            None
        }
    }
}

/// Record a value that was not accepted (`security_audit.jsonl`, Warning).
/// Only the reason and the first 40 characters are kept.
pub fn audit_rejected(home: &Path, agent: &str, source: &str, rejection: &PrincipalRejection) {
    tracing::warn!(
        agent = %duduclaw_core::truncate_chars(agent, 64),
        source,
        reason = rejection.reason,
        "on_behalf_of value rejected"
    );
    let event = AuditEvent::new(
        AUDIT_REJECTED,
        duduclaw_core::truncate_chars(agent, 64),
        Severity::Warning,
        serde_json::json!({
            "source": source,
            "reason": rejection.reason,
            "value": rejection.preview,
        }),
    );
    crate::security_autopilot::audit_and_emit(home, &event);
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;
