//! TextGradient — structured feedback signals for the Generator.
//!
//! Inspired by TextGrad (arXiv 2406.07496, Nature): instead of returning a
//! numeric score, the Verifier produces concrete textual modification suggestions.
//! This converges 2-3x faster than score-based feedback.

use serde::{Deserialize, Serialize};

/// Severity of a gradient signal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GradientSeverity {
    /// Must be fixed before the proposal can be approved.
    Blocking,
    /// Suggestion that the Generator may choose to ignore.
    Advisory,
}

/// A structured feedback signal from a Verifier layer.
///
/// Instead of a numeric score, provides concrete modification advice
/// that the Generator can directly act upon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextGradient {
    /// Which part of the proposal this targets (e.g., "SOUL.md lines 15-18").
    pub target: String,
    /// What's wrong — the critique.
    pub critique: String,
    /// Specific fix suggestion.
    pub suggestion: String,
    /// Which verifier layer produced this gradient.
    pub source_layer: String,
    /// Whether this blocks approval or is merely advisory.
    pub severity: GradientSeverity,
}

impl TextGradient {
    /// Format as a markdown section for injection into the Generator's re-prompt.
    pub fn to_prompt_section(&self) -> String {
        let severity_label = match self.severity {
            GradientSeverity::Blocking => "BLOCKING",
            GradientSeverity::Advisory => "ADVISORY",
        };

        format!(
            "### Feedback from {source} [{severity}]\n\
             **Target:** {target}\n\
             **Issue:** {critique}\n\
             **Suggested fix:** {suggestion}",
            source = self.source_layer,
            severity = severity_label,
            target = self.target,
            critique = self.critique,
            suggestion = self.suggestion,
        )
    }

    /// Create a blocking gradient from a verifier layer.
    pub fn blocking(source_layer: &str, target: &str, critique: &str, suggestion: &str) -> Self {
        Self {
            target: target.to_string(),
            critique: critique.to_string(),
            suggestion: suggestion.to_string(),
            source_layer: source_layer.to_string(),
            severity: GradientSeverity::Blocking,
        }
    }

    /// Create an advisory gradient from a verifier layer.
    pub fn advisory(source_layer: &str, target: &str, critique: &str, suggestion: &str) -> Self {
        Self {
            target: target.to_string(),
            critique: critique.to_string(),
            suggestion: suggestion.to_string(),
            source_layer: source_layer.to_string(),
            severity: GradientSeverity::Advisory,
        }
    }
}
