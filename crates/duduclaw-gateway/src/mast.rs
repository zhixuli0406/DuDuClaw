//! MAST failure-taxonomy annotation (R3).
//!
//! Deterministic classifier mapping DuDuClaw's existing failure evidence
//! (`FailureReason` debug tokens, stream diagnostics, trajectory-guard
//! anomaly kinds, eval assertion outcomes) onto the **MAST** multi-agent
//! failure taxonomy — *"Why Do Multi-Agent LLM Systems Fail?"*
//! (arXiv:2503.13657, NeurIPS 2025).
//!
//! ## Taxonomy source (verified first-hand, 2026-07-11)
//! Mode names were taken from the paper's official repository
//! (`multi-agent-systems-failure-taxonomy/MAST`,
//! `taxonomy_definitions_examples/definitions.txt`) — the maintained
//! artifact. Note: the arXiv HTML render names FM-3.2/FM-3.3 slightly
//! differently ("No or incomplete verification" / "Incorrect
//! verification"); we follow the repo's canonical names
//! ("Weak Verification" / "No or Incorrect Verification").
//!
//! Three categories, fourteen modes:
//!
//! | Category | Modes |
//! |---|---|
//! | Specification issues | FM-1.1 Disobey Task Specification · FM-1.2 Disobey Role Specification · FM-1.3 Step Repetition · FM-1.4 Loss of Conversation History · FM-1.5 Unaware of Termination Conditions |
//! | Inter-agent misalignment | FM-2.1 Conversation Reset · FM-2.2 Fail to Ask for Clarification · FM-2.3 Task Derailment · FM-2.4 Information Withholding · FM-2.5 Ignored Other Agent's Input · FM-2.6 Action-Reasoning Mismatch |
//! | Task verification | FM-3.1 Premature Termination · FM-3.2 Weak Verification · FM-3.3 No or Incorrect Verification |
//!
//! ## Honesty rules
//! - Modes that require **semantic judgment** (e.g. FM-2.3 Task Derailment,
//!   FM-2.6 Action-Reasoning Mismatch) are never guessed from deterministic
//!   evidence — such evidence maps to [`MastLabel::Unclassified`].
//! - Provider/infrastructure failures (rate limit, billing, missing binary,
//!   spawn error, auth) are **outside** MAST's scope (the taxonomy assumes
//!   the agents actually ran) and map to [`MastLabel::Infra`], not to a mode.
//! - The classifier is a pure function over string tokens; it performs no
//!   I/O and never panics.

use serde::{Deserialize, Serialize};

// ─── Taxonomy ───────────────────────────────────────────────

/// The three MAST failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MastCategory {
    /// Category 1 — specification and system design issues.
    SpecificationIssues,
    /// Category 2 — inter-agent misalignment.
    InterAgentMisalignment,
    /// Category 3 — task verification and termination.
    TaskVerification,
}

impl MastCategory {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            MastCategory::SpecificationIssues => "specification_issues",
            MastCategory::InterAgentMisalignment => "inter_agent_misalignment",
            MastCategory::TaskVerification => "task_verification",
        }
    }
}

/// The fourteen MAST failure modes (paper-repo canonical names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MastMode {
    DisobeyTaskSpecification,       // FM-1.1
    DisobeyRoleSpecification,       // FM-1.2
    StepRepetition,                 // FM-1.3
    LossOfConversationHistory,      // FM-1.4
    UnawareOfTerminationConditions, // FM-1.5
    ConversationReset,              // FM-2.1
    FailToAskForClarification,      // FM-2.2
    TaskDerailment,                 // FM-2.3
    InformationWithholding,         // FM-2.4
    IgnoredOtherAgentsInput,        // FM-2.5
    ActionReasoningMismatch,        // FM-2.6
    PrematureTermination,           // FM-3.1
    WeakVerification,               // FM-3.2
    NoOrIncorrectVerification,      // FM-3.3
}

/// All fourteen modes, in taxonomy order (table-driven test anchor).
pub const ALL_MODES: [MastMode; 14] = [
    MastMode::DisobeyTaskSpecification,
    MastMode::DisobeyRoleSpecification,
    MastMode::StepRepetition,
    MastMode::LossOfConversationHistory,
    MastMode::UnawareOfTerminationConditions,
    MastMode::ConversationReset,
    MastMode::FailToAskForClarification,
    MastMode::TaskDerailment,
    MastMode::InformationWithholding,
    MastMode::IgnoredOtherAgentsInput,
    MastMode::ActionReasoningMismatch,
    MastMode::PrematureTermination,
    MastMode::WeakVerification,
    MastMode::NoOrIncorrectVerification,
];

impl MastMode {
    /// Paper identifier, e.g. `FM-1.3`.
    pub fn id(self) -> &'static str {
        match self {
            MastMode::DisobeyTaskSpecification => "FM-1.1",
            MastMode::DisobeyRoleSpecification => "FM-1.2",
            MastMode::StepRepetition => "FM-1.3",
            MastMode::LossOfConversationHistory => "FM-1.4",
            MastMode::UnawareOfTerminationConditions => "FM-1.5",
            MastMode::ConversationReset => "FM-2.1",
            MastMode::FailToAskForClarification => "FM-2.2",
            MastMode::TaskDerailment => "FM-2.3",
            MastMode::InformationWithholding => "FM-2.4",
            MastMode::IgnoredOtherAgentsInput => "FM-2.5",
            MastMode::ActionReasoningMismatch => "FM-2.6",
            MastMode::PrematureTermination => "FM-3.1",
            MastMode::WeakVerification => "FM-3.2",
            MastMode::NoOrIncorrectVerification => "FM-3.3",
        }
    }

    /// Canonical mode name (paper repo `definitions.txt`).
    pub fn name(self) -> &'static str {
        match self {
            MastMode::DisobeyTaskSpecification => "Disobey Task Specification",
            MastMode::DisobeyRoleSpecification => "Disobey Role Specification",
            MastMode::StepRepetition => "Step Repetition",
            MastMode::LossOfConversationHistory => "Loss of Conversation History",
            MastMode::UnawareOfTerminationConditions => "Unaware of Termination Conditions",
            MastMode::ConversationReset => "Conversation Reset",
            MastMode::FailToAskForClarification => "Fail to Ask for Clarification",
            MastMode::TaskDerailment => "Task Derailment",
            MastMode::InformationWithholding => "Information Withholding",
            MastMode::IgnoredOtherAgentsInput => "Ignored Other Agent's Input",
            MastMode::ActionReasoningMismatch => "Action-Reasoning Mismatch",
            MastMode::PrematureTermination => "Premature Termination",
            MastMode::WeakVerification => "Weak Verification",
            MastMode::NoOrIncorrectVerification => "No or Incorrect Verification",
        }
    }

    /// Category the mode belongs to.
    pub fn category(self) -> MastCategory {
        match self {
            MastMode::DisobeyTaskSpecification
            | MastMode::DisobeyRoleSpecification
            | MastMode::StepRepetition
            | MastMode::LossOfConversationHistory
            | MastMode::UnawareOfTerminationConditions => MastCategory::SpecificationIssues,
            MastMode::ConversationReset
            | MastMode::FailToAskForClarification
            | MastMode::TaskDerailment
            | MastMode::InformationWithholding
            | MastMode::IgnoredOtherAgentsInput
            | MastMode::ActionReasoningMismatch => MastCategory::InterAgentMisalignment,
            MastMode::PrematureTermination
            | MastMode::WeakVerification
            | MastMode::NoOrIncorrectVerification => MastCategory::TaskVerification,
        }
    }
}

/// Result of a classification. `Infra` = outside MAST scope (provider /
/// harness failure); `Unclassified` = would need semantic judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MastLabel {
    Mode(MastMode),
    Infra,
    Unclassified,
}

impl MastLabel {
    /// Stable wire token for JSONL records: `"FM-1.3"`, `"infra"`,
    /// `"unclassified"`.
    pub fn as_str(self) -> &'static str {
        match self {
            MastLabel::Mode(m) => m.id(),
            MastLabel::Infra => "infra",
            MastLabel::Unclassified => "unclassified",
        }
    }

    /// Category token (or the label itself for non-modes).
    pub fn category_str(self) -> &'static str {
        match self {
            MastLabel::Mode(m) => m.category().as_str(),
            MastLabel::Infra => "infra",
            MastLabel::Unclassified => "unclassified",
        }
    }

    /// Human-readable display, e.g. `FM-1.3 Step Repetition`.
    pub fn display(self) -> String {
        match self {
            MastLabel::Mode(m) => format!("{} {}", m.id(), m.name()),
            MastLabel::Infra => "infra (outside MAST scope)".to_string(),
            MastLabel::Unclassified => "unclassified".to_string(),
        }
    }
}

// ─── Evidence & classifier ──────────────────────────────────

/// Deterministic failure evidence available at classification time. All
/// fields optional; the classifier only uses what is present.
#[derive(Debug, Default, Clone)]
pub struct FailureEvidence<'a> {
    /// `FailureReason` Debug token from `channel_reply` classification,
    /// e.g. `"RateLimited"`, `"Timeout"`, `"EmptyResponse"`.
    pub reason: Option<&'a str>,
    /// Trajectory-guard anomaly kind wire token, e.g. `"repeated_tool_loop"`.
    pub anomaly: Option<&'a str>,
    /// Stream-json `result` event subtype, e.g. `"error_max_turns"`.
    pub result_subtype: Option<&'a str>,
    /// Last assistant `stop_reason`, e.g. `"max_tokens"`.
    pub stop_reason: Option<&'a str>,
    /// Raw error text (may embed rendered `StreamDiagnostics`).
    pub error_text: Option<&'a str>,
}

/// `FailureReason` Debug tokens that are provider / harness failures —
/// outside MAST's agentic-failure scope.
const INFRA_REASONS: &[&str] = &[
    "BinaryMissing",
    "RateLimited",
    "Billing",
    "AuthFailed",
    "SpawnError",
    "NoAccounts",
];

/// Classify deterministic runtime failure evidence onto a MAST label.
///
/// Table (first match wins):
/// 1. anomaly `repeated_tool_loop` → **FM-1.3 Step Repetition** (definitional
///    match: "unnecessarily repeats a phase … already completed").
/// 2. other trajectory anomalies (`excessive_depth`, `cost_slope_spike`,
///    `trajectory_stall`) → `unclassified` (whether they constitute e.g.
///    Task Derailment needs semantic judgment — never guess).
/// 3. `result_subtype == "error_max_turns"` (also detected as a token in
///    `error_text`, since diagnostics are embedded there) →
///    **FM-1.5 Unaware of Termination Conditions** (the run burned its full
///    turn budget without stopping).
/// 4. infra reasons ([`INFRA_REASONS`]) → `infra`.
/// 5. reason `Timeout` → **FM-1.5** (the hard deadline killed a run that
///    failed to terminate within bounds).
/// 6. reason `EmptyResponse` → **FM-3.1 Premature Termination** (the run
///    ended without delivering the objective).
/// 7. anything else → `unclassified`.
pub fn classify(ev: &FailureEvidence) -> MastLabel {
    if let Some(a) = ev.anomaly {
        if a == "repeated_tool_loop" {
            return MastLabel::Mode(MastMode::StepRepetition);
        }
        if matches!(
            a,
            "excessive_depth" | "cost_slope_spike" | "trajectory_stall"
        ) {
            return MastLabel::Unclassified;
        }
    }
    let max_turns = ev.result_subtype == Some("error_max_turns")
        || ev.error_text.is_some_and(|t| t.contains("error_max_turns"));
    if max_turns {
        return MastLabel::Mode(MastMode::UnawareOfTerminationConditions);
    }
    if let Some(r) = ev.reason {
        if INFRA_REASONS.iter().any(|k| *k == r) {
            return MastLabel::Infra;
        }
        if r == "Timeout" {
            return MastLabel::Mode(MastMode::UnawareOfTerminationConditions);
        }
        if r == "EmptyResponse" {
            return MastLabel::Mode(MastMode::PrematureTermination);
        }
    }
    MastLabel::Unclassified
}

/// Classify a failed `duduclaw eval` assertion by its report name.
///
/// Golden-task assertions encode the *task specification*; a deterministic
/// assertion failure therefore maps to **FM-1.1 Disobey Task Specification**.
/// Budget/shape checks (`max_tool_calls`, `min_text_blocks`) and anything
/// unknown stay `unclassified` (exceeding a tool budget is not necessarily
/// repetition, and guessing is forbidden). Prefix match is anchored at the
/// start of the assertion name (the names are generated by our own
/// `assertions.rs`, `"<field>: <value>"`).
pub fn classify_eval_assertion(name: &str) -> MastLabel {
    const SPEC_PREFIXES: &[&str] = &[
        "must_use_tools:",
        "must_not_use_tools:",
        "output_contains:",
        "output_not_contains:",
        "output_regex:",
    ];
    // WP4 GroundEval (arXiv:2606.22737): a `grounded:` assertion failure
    // means the final answer could not be traced back to real tool
    // evidence — the task-verification failure to check the agent's own
    // claims, i.e. FM-3.3. This is a structural classification (the
    // assertion *type* itself is deterministic evidence), not a semantic
    // guess about the content.
    if name.starts_with("grounded:") {
        return MastLabel::Mode(MastMode::NoOrIncorrectVerification);
    }
    if SPEC_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return MastLabel::Mode(MastMode::DisobeyTaskSpecification);
    }
    MastLabel::Unclassified
}

/// Classify an eval case that died before assertions could run (load /
/// spawn / stream / parse failure). The agent's behavior was never
/// observed, so this is always a harness/provider failure: `infra`.
pub fn classify_eval_error(_error: &str) -> MastLabel {
    MastLabel::Infra
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Taxonomy integrity ──────────────────────────────

    #[test]
    fn fourteen_modes_three_categories() {
        assert_eq!(ALL_MODES.len(), 14);
        let spec = ALL_MODES
            .iter()
            .filter(|m| m.category() == MastCategory::SpecificationIssues)
            .count();
        let inter = ALL_MODES
            .iter()
            .filter(|m| m.category() == MastCategory::InterAgentMisalignment)
            .count();
        let verif = ALL_MODES
            .iter()
            .filter(|m| m.category() == MastCategory::TaskVerification)
            .count();
        assert_eq!((spec, inter, verif), (5, 6, 3));
    }

    #[test]
    fn mode_ids_are_unique_and_well_formed() {
        let mut ids: Vec<&str> = ALL_MODES.iter().map(|m| m.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 14, "duplicate mode id");
        for m in ALL_MODES {
            assert!(m.id().starts_with("FM-"), "{}", m.id());
            assert!(!m.name().is_empty());
        }
    }

    #[test]
    fn canonical_names_match_paper_repo() {
        // Spot-check against definitions.txt headings (fetched 2026-07-11).
        assert_eq!(MastMode::StepRepetition.name(), "Step Repetition");
        assert_eq!(MastMode::WeakVerification.name(), "Weak Verification");
        assert_eq!(
            MastMode::NoOrIncorrectVerification.name(),
            "No or Incorrect Verification"
        );
        assert_eq!(
            MastMode::ActionReasoningMismatch.name(),
            "Action-Reasoning Mismatch"
        );
    }

    // ── Runtime classifier (table-driven) ───────────────

    #[test]
    fn classify_table() {
        let cases: &[(FailureEvidence, MastLabel)] = &[
            (
                FailureEvidence {
                    anomaly: Some("repeated_tool_loop"),
                    ..Default::default()
                },
                MastLabel::Mode(MastMode::StepRepetition),
            ),
            (
                FailureEvidence {
                    anomaly: Some("excessive_depth"),
                    ..Default::default()
                },
                MastLabel::Unclassified,
            ),
            (
                FailureEvidence {
                    anomaly: Some("cost_slope_spike"),
                    ..Default::default()
                },
                MastLabel::Unclassified,
            ),
            (
                FailureEvidence {
                    anomaly: Some("trajectory_stall"),
                    ..Default::default()
                },
                MastLabel::Unclassified,
            ),
            (
                FailureEvidence {
                    result_subtype: Some("error_max_turns"),
                    ..Default::default()
                },
                MastLabel::Mode(MastMode::UnawareOfTerminationConditions),
            ),
            (
                FailureEvidence {
                    error_text: Some("exit=1 ... result_subtype=Some(\"error_max_turns\") ..."),
                    ..Default::default()
                },
                MastLabel::Mode(MastMode::UnawareOfTerminationConditions),
            ),
            (
                FailureEvidence {
                    reason: Some("RateLimited"),
                    ..Default::default()
                },
                MastLabel::Infra,
            ),
            (
                FailureEvidence {
                    reason: Some("Billing"),
                    ..Default::default()
                },
                MastLabel::Infra,
            ),
            (
                FailureEvidence {
                    reason: Some("AuthFailed"),
                    ..Default::default()
                },
                MastLabel::Infra,
            ),
            (
                FailureEvidence {
                    reason: Some("BinaryMissing"),
                    ..Default::default()
                },
                MastLabel::Infra,
            ),
            (
                FailureEvidence {
                    reason: Some("SpawnError"),
                    ..Default::default()
                },
                MastLabel::Infra,
            ),
            (
                FailureEvidence {
                    reason: Some("NoAccounts"),
                    ..Default::default()
                },
                MastLabel::Infra,
            ),
            (
                FailureEvidence {
                    reason: Some("Timeout"),
                    ..Default::default()
                },
                MastLabel::Mode(MastMode::UnawareOfTerminationConditions),
            ),
            (
                FailureEvidence {
                    reason: Some("EmptyResponse"),
                    ..Default::default()
                },
                MastLabel::Mode(MastMode::PrematureTermination),
            ),
            (
                FailureEvidence {
                    reason: Some("Unknown"),
                    ..Default::default()
                },
                MastLabel::Unclassified,
            ),
            (FailureEvidence::default(), MastLabel::Unclassified),
        ];
        for (ev, want) in cases {
            assert_eq!(classify(ev), *want, "evidence: {ev:?}");
        }
    }

    #[test]
    fn anomaly_takes_precedence_over_reason() {
        // A repeated-loop anomaly recorded alongside an infra reason is the
        // richer agentic signal — the loop is what the operator must fix.
        let ev = FailureEvidence {
            anomaly: Some("repeated_tool_loop"),
            reason: Some("RateLimited"),
            ..Default::default()
        };
        assert_eq!(classify(&ev), MastLabel::Mode(MastMode::StepRepetition));
    }

    // ── Eval classifier ─────────────────────────────────

    #[test]
    fn eval_assertion_table() {
        let cases: &[(&str, MastLabel)] = &[
            (
                "must_use_tools: tasks_create",
                MastLabel::Mode(MastMode::DisobeyTaskSpecification),
            ),
            (
                "must_not_use_tools: Bash",
                MastLabel::Mode(MastMode::DisobeyTaskSpecification),
            ),
            (
                "output_contains: \"order #1234\"",
                MastLabel::Mode(MastMode::DisobeyTaskSpecification),
            ),
            (
                "output_not_contains: \"sk-ant-\"",
                MastLabel::Mode(MastMode::DisobeyTaskSpecification),
            ),
            (
                "output_regex: \"refund\"",
                MastLabel::Mode(MastMode::DisobeyTaskSpecification),
            ),
            ("max_tool_calls: 3", MastLabel::Unclassified),
            ("min_text_blocks: 2", MastLabel::Unclassified),
            ("something_new: x", MastLabel::Unclassified),
            (
                "grounded: tool=memory_search min_overlap_chars=12",
                MastLabel::Mode(MastMode::NoOrIncorrectVerification),
            ),
        ];
        for (name, want) in cases {
            assert_eq!(classify_eval_assertion(name), *want, "name: {name}");
        }
    }

    #[test]
    fn eval_error_is_infra() {
        assert_eq!(
            classify_eval_error("spawn claude: No such file"),
            MastLabel::Infra
        );
    }

    #[test]
    fn label_wire_tokens() {
        assert_eq!(MastLabel::Mode(MastMode::StepRepetition).as_str(), "FM-1.3");
        assert_eq!(MastLabel::Infra.as_str(), "infra");
        assert_eq!(MastLabel::Unclassified.as_str(), "unclassified");
        assert_eq!(
            MastLabel::Mode(MastMode::PrematureTermination).category_str(),
            "task_verification"
        );
        assert!(MastLabel::Mode(MastMode::StepRepetition)
            .display()
            .contains("Step Repetition"));
    }
}
