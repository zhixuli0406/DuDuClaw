//! Which execution paths `[container] sandbox_enabled = true` does NOT cover,
//! said out loud.
//!
//! The task sandbox wraps exactly one path: a delegated task through
//! `dispatcher::dispatch_to_agent`. Every other way a sandbox-enabled
//! employee's AI CLI can be started either needs the platform tool surface and
//! the conversation state the sandbox deliberately withholds (channel replies,
//! cron, reminders, …) and keeps running on the host, or depends on those
//! tools for its whole effect and is skipped (the Agent Mail arrival trigger).
//! Neither may happen silently: [`note_not_applied`] writes the audit event
//! [`AUDIT_TASK_SANDBOX_NOT_APPLIED`] and one `warn!`, at most once per
//! `(agent, path)` per gateway process so a busy channel cannot flood the
//! audit log.
//!
//! An employee with the sandbox off gets nothing from this module: no audit
//! row, no log line, no lock taken.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde_json::json;

/// Audit event type written by [`note_not_applied`].
pub const AUDIT_TASK_SANDBOX_NOT_APPLIED: &str = "task_sandbox_not_applied";

/// An execution path that runs (or skips) a sandbox-enabled employee outside
/// the task sandbox. `as_str` is the `path` field of the audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostPath {
    /// A live channel reply (`channel_reply`).
    ChannelReply,
    /// A scheduled cron task (`cron_scheduler`).
    Cron,
    /// An `agent_callback` reminder (`reminder_scheduler`).
    Reminder,
    /// The Agent Mail arrival trigger (`mail_worker`).
    Mail,
    /// The heartbeat's proactive check (`PROACTIVE.md`).
    Proactive,
    /// An ephemeral agent spawned on the employee's behalf (`spawn_ephemeral`).
    Ephemeral,
    /// An ACP (`duduclaw acp`) prompt answered by the employee.
    Acp,
    /// A live-mode `duduclaw eval` case run against the employee.
    Eval,
}

impl HostPath {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ChannelReply => "channel_reply",
            Self::Cron => "cron",
            Self::Reminder => "reminder",
            Self::Mail => "mail",
            Self::Proactive => "proactive",
            Self::Ephemeral => "ephemeral",
            Self::Acp => "acp",
            Self::Eval => "eval",
        }
    }
}

/// What happened instead of the sandbox. `as_str` is the `action` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAction {
    /// The run went ahead on the host, unsandboxed.
    RanOnHost,
    /// The run was not started at all.
    Skipped,
}

impl HostAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RanOnHost => "ran_on_host",
            Self::Skipped => "skipped",
        }
    }
}

/// `(agent, path)` pairs already reported by this process.
fn reported() -> &'static Mutex<HashSet<(String, HostPath)>> {
    static SEEN: OnceLock<Mutex<HashSet<(String, HostPath)>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// `true` the first time `(agent_id, path)` is seen in this process. The lock
/// is held only for the set insert; nothing awaits under it. A poisoned lock
/// still answers (the set is plain data), so a panic elsewhere can never turn
/// the notice off.
fn first_report(agent_id: &str, path: HostPath) -> bool {
    let mut seen = reported().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    seen.insert((agent_id.to_string(), path))
}

/// Report that `agent_id`'s run on `path` is not going through the task
/// sandbox.
///
/// `sandbox_enabled` is the caller's already-loaded `[container]
/// sandbox_enabled` for this employee; `false` returns immediately, so an
/// employee without the sandbox is byte-identical to before. Otherwise the
/// first call per `(agent_id, path)` in this process writes the audit event
/// `task_sandbox_not_applied` with `{path, action}` and one `warn!`; later
/// calls do nothing. Returns whether this call reported.
pub fn note_not_applied(
    home: &Path,
    agent_id: &str,
    sandbox_enabled: bool,
    path: HostPath,
    action: HostAction,
) -> bool {
    if !sandbox_enabled || !first_report(agent_id, path) {
        return false;
    }
    tracing::warn!(
        agent = %agent_id,
        path = path.as_str(),
        action = action.as_str(),
        "agent has [container] sandbox_enabled = true, but this path does not use the task \
         sandbox (reported once per agent and path until the gateway restarts)"
    );
    super::audit(
        home,
        AUDIT_TASK_SANDBOX_NOT_APPLIED,
        agent_id,
        json!({ "path": path.as_str(), "action": action.as_str() }),
    );
    true
}

/// The registry's already-loaded `[container] sandbox_enabled` for
/// `agent_id` (`"default"` ⇒ the main agent). An unknown agent reads `false`,
/// matching `dispatch_to_agent`. The read guard is dropped before returning.
pub async fn sandbox_enabled_in_registry(
    registry: &tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>,
    agent_id: &str,
) -> bool {
    let reg = registry.read().await;
    let agent = if agent_id == "default" { reg.main_agent() } else { reg.get(agent_id) };
    agent.map(|a| a.config.container.sandbox_enabled).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit_text(home: &Path) -> String {
        let mut all = String::new();
        let mut stack = vec![home.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    all.push_str(&text);
                }
            }
        }
        all
    }

    #[test]
    fn flag_off_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        for path in [HostPath::ChannelReply, HostPath::Cron, HostPath::Reminder, HostPath::Mail] {
            assert!(!note_not_applied(home.path(), "cov-off", false, path, HostAction::RanOnHost));
        }
        assert!(!audit_text(home.path()).contains(AUDIT_TASK_SANDBOX_NOT_APPLIED));
        // Flag off never consumes the once-per-process slot either.
        assert!(note_not_applied(home.path(), "cov-off", true, HostPath::Cron, HostAction::RanOnHost));
    }

    #[test]
    fn reports_once_per_agent_and_path() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert!(note_not_applied(h, "cov-busy", true, HostPath::ChannelReply, HostAction::RanOnHost));
        for _ in 0..5 {
            assert!(!note_not_applied(h, "cov-busy", true, HostPath::ChannelReply, HostAction::RanOnHost));
        }
        // A different path of the same agent, and the same path of another
        // agent, each report once.
        assert!(note_not_applied(h, "cov-busy", true, HostPath::Reminder, HostAction::RanOnHost));
        assert!(note_not_applied(h, "cov-other", true, HostPath::ChannelReply, HostAction::RanOnHost));

        let audit = audit_text(h);
        assert_eq!(audit.matches(AUDIT_TASK_SANDBOX_NOT_APPLIED).count(), 3, "{audit}");
        assert!(audit.contains("\"path\":\"channel_reply\""), "{audit}");
        assert!(audit.contains("\"path\":\"reminder\""), "{audit}");
        assert!(audit.contains("\"action\":\"ran_on_host\""), "{audit}");
    }

    #[test]
    fn skipped_action_is_recorded() {
        let home = tempfile::tempdir().unwrap();
        assert!(note_not_applied(home.path(), "cov-mail", true, HostPath::Mail, HostAction::Skipped));
        let audit = audit_text(home.path());
        assert!(audit.contains("\"path\":\"mail\"") && audit.contains("\"action\":\"skipped\""), "{audit}");
    }

    #[test]
    fn path_and_action_tokens_are_stable() {
        let all = [
            (HostPath::ChannelReply, "channel_reply"),
            (HostPath::Cron, "cron"),
            (HostPath::Reminder, "reminder"),
            (HostPath::Mail, "mail"),
            (HostPath::Proactive, "proactive"),
            (HostPath::Ephemeral, "ephemeral"),
            (HostPath::Acp, "acp"),
            (HostPath::Eval, "eval"),
        ];
        for (path, token) in all {
            assert_eq!(path.as_str(), token);
        }
        assert_eq!(HostAction::RanOnHost.as_str(), "ran_on_host");
        assert_eq!(HostAction::Skipped.as_str(), "skipped");
    }
}
