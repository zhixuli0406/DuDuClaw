//! WP17 — CEO/Board governance mode (opt-in `[governance] board_mode`).
//!
//! Off by default: a solo deployment sees zero change. When on, it maps the
//! paperclip "living board" metaphor onto DuDuClaw primitives — Board = human
//! users with board rights, CEO = the `reports_to` root agent, Initiative = a
//! top-level Task Board task, and every consequential decision (strategic plan,
//! hiring, budget change) flows through the ApprovalBroker with a hard
//! invariant: **the Board is always a human, never an agent.**
//!
//! This module owns the deterministic, security-critical core: the typed
//! [`ApprovalKind`] (collapsing the ad-hoc `action_kind` strings so automation
//! can only auto-decide safe kinds), and the fail-closed authorization
//! predicates. The dashboard Board panel, the CEO strategic-proposal flow, and
//! the cascade-budget wiring build on these.

use serde::{Deserialize, Serialize};

/// Typed approval kind. Serde uses the existing snake_case string values so the
/// `approvals.db` `action_kind` column stays compatible (additive migration —
/// no rewrite). Unknown/legacy strings map to [`ApprovalKind::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalKind {
    /// Browser/computer-use action (legacy `browser_action`).
    BrowserAction,
    /// A tool call requiring HITL approval (legacy `tool_call`).
    ToolCall,
    /// WP8 — activating a newly created skill.
    SkillActivation,
    /// WP17 — a CEO agent's strategic plan awaiting Board approval.
    StrategicPlan,
    /// WP17 — creating a persistent agent ("hiring") awaiting Board approval.
    AgentHire,
    /// WP3 — an AI-proposed shared-wiki page awaiting curation.
    WikiIngest,
    /// Any other/legacy string kind.
    Other(String),
}

impl ApprovalKind {
    pub fn as_str(&self) -> &str {
        match self {
            ApprovalKind::BrowserAction => "browser_action",
            ApprovalKind::ToolCall => "tool_call",
            ApprovalKind::SkillActivation => "skill_activation",
            ApprovalKind::StrategicPlan => "strategic_plan",
            ApprovalKind::AgentHire => "agent_hire",
            ApprovalKind::WikiIngest => "wiki_ingest",
            ApprovalKind::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "browser_action" => ApprovalKind::BrowserAction,
            "tool_call" => ApprovalKind::ToolCall,
            "skill_activation" => ApprovalKind::SkillActivation,
            "strategic_plan" => ApprovalKind::StrategicPlan,
            "agent_hire" => ApprovalKind::AgentHire,
            "wiki_ingest" => ApprovalKind::WikiIngest,
            other => ApprovalKind::Other(other.to_string()),
        }
    }

    /// Kinds that only a Board-rights **human** may decide. Deciding these as an
    /// agent (or a non-board user) must be refused + audited.
    pub fn requires_board(&self) -> bool {
        matches!(self, ApprovalKind::StrategicPlan | ApprovalKind::AgentHire)
    }
}

/// Who is attempting a decision. `is_agent` is the hard "Board = human" guard:
/// even a board-flagged identity that turns out to be an agent is refused.
#[derive(Debug, Clone, Copy)]
pub struct Decider {
    pub is_agent: bool,
    pub has_board_rights: bool,
}

impl Decider {
    pub fn human_board() -> Self {
        Self { is_agent: false, has_board_rights: true }
    }
    pub fn human_regular() -> Self {
        Self { is_agent: false, has_board_rights: false }
    }
    pub fn agent() -> Self {
        Self { is_agent: true, has_board_rights: false }
    }
}

/// Fail-closed: may `decider` decide an approval of `kind`?
///
/// - Board-only kinds (StrategicPlan/AgentHire) require a human with board
///   rights. An agent is refused unconditionally (Board = human invariant).
/// - Other kinds keep their existing behaviour (this predicate returns `true`;
///   the caller's own scope check still applies).
pub fn can_decide(kind: &ApprovalKind, decider: Decider) -> bool {
    if kind.requires_board() {
        return decider.has_board_rights && !decider.is_agent;
    }
    true
}

/// May an Initiative be created by this decider? Only a human with board rights;
/// the CEO agent can be *delegated* an Initiative but cannot self-create one.
pub fn can_create_initiative(decider: Decider) -> bool {
    decider.has_board_rights && !decider.is_agent
}

/// When `board_mode` is on, no agent (including the CEO) may edit a `[budget]`
/// value through MCP/tool paths — budget changes go only through the Board panel
/// RPC. This prevents an agent from raising its own spending cap (self-promotion).
/// Returns whether an agent-originated budget edit is allowed.
pub fn agent_may_edit_budget(board_mode: bool) -> bool {
    !board_mode
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_kind_string_roundtrip() {
        for k in [
            ApprovalKind::BrowserAction,
            ApprovalKind::ToolCall,
            ApprovalKind::SkillActivation,
            ApprovalKind::StrategicPlan,
            ApprovalKind::AgentHire,
            ApprovalKind::WikiIngest,
        ] {
            assert_eq!(ApprovalKind::parse(k.as_str()), k);
        }
        // Legacy/unknown strings survive as Other and round-trip.
        let other = ApprovalKind::parse("legacy_thing");
        assert_eq!(other.as_str(), "legacy_thing");
    }

    #[test]
    fn board_only_kinds_require_human_board() {
        // Agent can never decide a StrategicPlan / AgentHire (Board = human).
        assert!(!can_decide(&ApprovalKind::StrategicPlan, Decider::agent()));
        assert!(!can_decide(&ApprovalKind::AgentHire, Decider::agent()));
        // Regular human (no board rights) also refused.
        assert!(!can_decide(&ApprovalKind::StrategicPlan, Decider::human_regular()));
        // Human with board rights: allowed.
        assert!(can_decide(&ApprovalKind::StrategicPlan, Decider::human_board()));
        assert!(can_decide(&ApprovalKind::AgentHire, Decider::human_board()));
    }

    #[test]
    fn non_board_kinds_unrestricted_by_this_predicate() {
        assert!(can_decide(&ApprovalKind::SkillActivation, Decider::human_regular()));
        assert!(can_decide(&ApprovalKind::ToolCall, Decider::agent()));
    }

    #[test]
    fn initiative_creation_is_board_human_only() {
        assert!(can_create_initiative(Decider::human_board()));
        assert!(!can_create_initiative(Decider::agent()));
        assert!(!can_create_initiative(Decider::human_regular()));
    }

    #[test]
    fn budget_edit_locked_for_agents_in_board_mode() {
        assert!(!agent_may_edit_budget(true)); // board_mode on ⇒ agents locked out
        assert!(agent_may_edit_budget(false)); // off ⇒ legacy behaviour
    }
}
