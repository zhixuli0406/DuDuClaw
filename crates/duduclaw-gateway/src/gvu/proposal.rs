//! Evolution proposal types — the unit of change in the GVU loop.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::text_gradient::TextGradient;

/// What kind of evolution change is being proposed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalType {
    /// Modify SOUL.md (content is a unified diff).
    SoulPatch,
    /// Add a new skill file (content is the full skill markdown).
    SkillAdd,
    /// Archive an existing skill (content is the skill filename).
    SkillArchive,
    /// Amend CONTRACT.toml (content is the new boundaries section).
    ContractAmend,
}

/// Lifecycle status of a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ProposalStatus {
    /// Generator is producing the proposal.
    Generating,
    /// Verifier is evaluating.
    Verifying,
    /// Failed verification — includes structured feedback.
    Rejected { gradient: TextGradient },
    /// Passed all verifier layers.
    Approved,
    /// Written to disk, observation period started.
    Applied,
    /// Observation period active.
    Observing,
    /// Observation passed, change is permanent.
    Confirmed,
    /// Observation failed, change was reverted.
    RolledBack { reason: String },
}

impl ProposalStatus {
    /// Short label for persistence / display.
    pub fn label(&self) -> &str {
        match self {
            Self::Generating => "generating",
            Self::Verifying => "verifying",
            Self::Rejected { .. } => "rejected",
            Self::Approved => "approved",
            Self::Applied => "applied",
            Self::Observing => "observing",
            Self::Confirmed => "confirmed",
            Self::RolledBack { .. } => "rolled_back",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Confirmed | Self::RolledBack { .. } | Self::Rejected { .. })
    }
}

/// A single evolution proposal flowing through the GVU loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionProposal {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Target agent.
    pub agent_id: String,
    /// What kind of change.
    pub proposal_type: ProposalType,
    /// The proposed change content (diff, full text, or filename).
    pub content: String,
    /// Why this change was proposed (human-readable).
    pub rationale: String,
    /// Current generation attempt (1-based, max 3).
    pub generation: u32,
    /// Current lifecycle status.
    pub status: ProposalStatus,
    /// Context that triggered this evolution (prediction error details).
    pub trigger_context: String,
    /// When the proposal was created.
    pub created_at: DateTime<Utc>,
    /// When the proposal reached a terminal state.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl EvolutionProposal {
    /// Create a new proposal in Generating status.
    pub fn new(agent_id: String, proposal_type: ProposalType, trigger_context: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id,
            proposal_type,
            content: String::new(),
            rationale: String::new(),
            generation: 1,
            status: ProposalStatus::Generating,
            trigger_context,
            created_at: Utc::now(),
            resolved_at: None,
        }
    }
}
