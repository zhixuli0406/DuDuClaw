//! Evolution proposal types — the unit of change in the GVU loop.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::text_gradient::TextGradient;

/// Structured edit operation for a SOUL.md patch.
///
/// Replaces the legacy "LLM emits Markdown narrative → updater blindly appends"
/// flow with a typed instruction the updater can execute deterministically.
/// See [`crate::gvu::updater::apply_patch_to_soul`] for the application logic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SoulPatchOp {
    /// Replace the entire body of the named section (header line preserved).
    Replace,
    /// Insert lines at the END of the named section.
    AppendWithin,
    /// Insert lines at the START of the named section (just after the header).
    PrependWithin,
    /// Create a NEW section at the end of SOUL.md with this title.
    AddSection,
    /// Consolidate / compress the named section's body (v1.16.0).
    ///
    /// Semantically equivalent to `Replace` but with a hard size-shrink
    /// contract: `content.len() < (existing section body).len()`. Used when
    /// SOUL.md is approaching the line/byte caps and the LLM is asked to
    /// merge redundant bullets, tighten language, or summarize without
    /// changing behavior.
    ///
    /// `apply_patch_to_soul` enforces the shrink invariant before swapping —
    /// a `Consolidate` whose content is longer than the section it would
    /// replace is rejected as a misclassified patch.
    Consolidate,
}

/// A typed instruction for editing SOUL.md.
///
/// Example: `SoulPatch { section: "核心價值", op: SoulPatchOp::AppendWithin,
/// content: "- 主動監測知識基礎的演變" }` adds one bullet to the existing
/// `## 核心價值` section without touching anything else.
///
/// `section` matches the text after the leading `##` (h2) of the target
/// section. Case-sensitive, leading/trailing whitespace stripped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SoulPatch {
    pub section: String,
    pub op: SoulPatchOp,
    pub content: String,
}

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
    ///
    /// LEGACY field: free-form Markdown narrative from the LLM. The updater
    /// strips meta sections from this (see [`crate::gvu::updater::strip_proposal_meta`])
    /// before appending. New proposals SHOULD prefer the structured [`Self::patch`]
    /// field instead, which describes a typed edit operation the updater can
    /// apply deterministically without LLM-side narrative cleanup.
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
    /// Structured edit operation. When present, the updater applies this
    /// instead of the free-form [`Self::content`] append. Optional and
    /// `serde(default)` so existing on-disk proposals deserialize unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<SoulPatch>,
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
            patch: None,
        }
    }
}
