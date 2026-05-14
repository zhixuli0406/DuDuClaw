//! Source / Caller / RestoreTarget — context that drives redaction & restore
//! decisions.

use serde::{Deserialize, Serialize};

/// Where a piece of text came from. The pipeline reads its source-policy
/// table to decide whether to redact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    /// A message from a human in a channel (LINE / Telegram / Discord / ...).
    /// Default policy: **passthrough** — users are trusted with their own
    /// inputs and redacting them would prevent normal actions like
    /// "send email to alice@acme.com".
    UserChannelInput { channel_id: String },

    /// A tool result returned to the LLM. Default policy: **redact** —
    /// this is the main protection point.
    ToolResult { tool_name: String },

    /// Part of the agent's system prompt (SOUL.md, sender block, team
    /// roster, ...). Default policy: **selective** — only rules tagged
    /// `apply_to_system_prompt = true` fire here.
    SystemPrompt { component: String },

    /// Reply from a delegated sub-agent. Default policy: **inherit** —
    /// the sub-agent's own pipeline already redacted; passthrough at
    /// the parent.
    SubAgentReply { agent_id: String },

    /// Context inserted by a cron / autopilot task (not from a live
    /// conversation). Default policy: **redact** — same risk as a tool
    /// result.
    CronContext,
}

impl Source {
    /// Stable category string used in audit logs.
    pub fn category(&self) -> &'static str {
        match self {
            Source::UserChannelInput { .. } => "user_channel_input",
            Source::ToolResult { .. } => "tool_result",
            Source::SystemPrompt { .. } => "system_prompt",
            Source::SubAgentReply { .. } => "sub_agent_reply",
            Source::CronContext => "cron_context",
        }
    }
}

/// Who is asking to restore a token. Drives the [`RestoreScope`] check.
///
/// [`RestoreScope`]: crate::rules::RestoreScope
#[derive(Debug, Clone)]
pub struct Caller {
    pub agent_id: String,
    pub scopes: Vec<String>,
    /// True when the caller is the channel's end-user (their own request,
    /// their own session). End-users always get owner-scope restore
    /// for text destined to their own channel.
    pub is_owner: bool,
}

impl Caller {
    pub fn owner(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            scopes: Vec::new(),
            is_owner: true,
        }
    }

    pub fn agent(agent_id: impl Into<String>, scopes: Vec<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            scopes,
            is_owner: false,
        }
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

/// Where a restored string is going. Affects audit context and may modify
/// the restore decision (e.g. `AuditLog` should never decrypt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RestoreTarget {
    /// Final reply to a user in a channel.
    UserChannel,
    /// Payload forwarded to a sub-agent.
    SubAgent { agent_id: String },
    /// Restoration purely for diagnostics — must not actually decrypt.
    AuditLog,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_owner_helper() {
        let c = Caller::owner("agnes");
        assert!(c.is_owner);
        assert_eq!(c.agent_id, "agnes");
    }

    #[test]
    fn caller_has_scope() {
        let c = Caller::agent("agnes", vec!["CustomerRead".into(), "FinanceRead".into()]);
        assert!(c.has_scope("CustomerRead"));
        assert!(!c.has_scope("HRRead"));
    }

    #[test]
    fn source_category_strings() {
        assert_eq!(
            Source::UserChannelInput { channel_id: "x".into() }.category(),
            "user_channel_input"
        );
        assert_eq!(
            Source::ToolResult { tool_name: "odoo.x".into() }.category(),
            "tool_result"
        );
        assert_eq!(Source::CronContext.category(), "cron_context");
    }
}
