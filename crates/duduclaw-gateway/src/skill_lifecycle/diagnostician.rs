//! Error diagnostician — analyzes prediction errors to determine what skills are needed.
//!
//! Zero LLM cost — pure rule-based diagnosis from PredictionError signals.

use super::compression::CompressedSkill;
use super::relevance;
use crate::prediction::engine::PredictionError;

/// Primary cause of a prediction error.
#[derive(Debug, Clone)]
pub enum ErrorCause {
    /// Response style doesn't match user preference (too long/short/formal/casual).
    StyleMismatch { aspect: String },
    /// Agent lacks knowledge in a specific domain.
    DomainGap { topic: String },
    /// Response wasn't accurate or precise enough.
    PrecisionIssue,
    /// User expected different behavior than agent provided.
    ExpectationMismatch,
    /// Cannot determine cause.
    Unknown,
}

/// A detected gap — no existing skill matches the need.
#[derive(Debug, Clone)]
pub struct SkillGap {
    pub suggested_name: String,
    pub suggested_description: String,
    pub evidence: Vec<String>,
}

/// Action the diagnostician recommends.
///
/// Note: Wiki-based reconstruction is handled by the caller (channel_reply.rs)
/// when it receives `ReportGap` — the diagnostician doesn't have async wiki access.
#[derive(Debug, Clone)]
pub enum DiagnosisAction {
    /// Activate existing skills.
    ActivateSkills(Vec<String>),
    /// Report a knowledge gap — caller should attempt wiki reconstruction first,
    /// then fall back to `inject_skill_gap` if no wiki pages match.
    ReportGap(SkillGap),
    /// No action needed.
    None,
}

/// Result of diagnosing a prediction error.
#[derive(Debug, Clone)]
pub struct ErrorDiagnosis {
    pub primary_cause: ErrorCause,
    pub related_topics: Vec<String>,
    /// Existing skills that might help (by name).
    pub suggested_skills: Vec<String>,
    /// If no existing skill matches, a gap is reported.
    pub skill_gap: Option<SkillGap>,
    /// Recommended action (includes wiki reconstruction option).
    pub action: DiagnosisAction,
}

/// Diagnose a prediction error and suggest skills to activate.
///
/// Zero LLM cost — uses error signals and keyword matching.
pub fn diagnose(
    error: &PredictionError,
    available_skills: &[CompressedSkill],
) -> Option<ErrorDiagnosis> {
    // Only diagnose Moderate+ errors
    if error.composite_error < 0.2 {
        return None;
    }

    let topics = &error.actual.extracted_topics;

    // Determine primary cause
    let primary_cause = if error.unexpected_correction {
        if error.actual.avg_assistant_response_length > 500.0 {
            ErrorCause::StyleMismatch { aspect: "too_verbose".to_string() }
        } else {
            ErrorCause::PrecisionIssue
        }
    } else if error.unexpected_follow_up {
        if topics.is_empty() {
            ErrorCause::ExpectationMismatch
        } else {
            ErrorCause::DomainGap { topic: topics.first().cloned().unwrap_or_default() }
        }
    } else if error.topic_surprise > 0.5 {
        ErrorCause::DomainGap {
            topic: topics.first().cloned().unwrap_or_else(|| "unknown".to_string()),
        }
    } else {
        ErrorCause::Unknown
    };

    // Match topics against available skills
    let message_proxy = topics.join(" ");
    let ranked = relevance::rank_skills(&message_proxy, available_skills);

    let mut suggested_skills = Vec::new();
    for (idx, score) in &ranked {
        if *score > 0.05 {
            suggested_skills.push(available_skills[*idx].name.clone());
        }
        if suggested_skills.len() >= 3 {
            break;
        }
    }

    // Determine action based on what's available
    let (skill_gap, action) = if !suggested_skills.is_empty() {
        (None, DiagnosisAction::ActivateSkills(suggested_skills.clone()))
    } else if !topics.is_empty() {
        let topic = topics.first().cloned().unwrap_or_default();
        let gap = SkillGap {
            suggested_name: format!("{}_expertise", topic.chars().take(20).collect::<String>()),
            suggested_description: format!(
                "Agent needs better handling of '{}' related conversations",
                topic
            ),
            evidence: vec![format!(
                "Prediction error {:.2} with topics: {}",
                error.composite_error,
                topics.join(", ")
            )],
        };
        // Note: wiki search is done by the caller (channel_reply.rs) since
        // it requires async WikiStore access. We report the gap and the caller
        // decides whether to attempt reconstruction.
        (Some(gap.clone()), DiagnosisAction::ReportGap(gap))
    } else {
        (None, DiagnosisAction::None)
    };

    Some(ErrorDiagnosis {
        primary_cause,
        related_topics: topics.clone(),
        suggested_skills,
        skill_gap,
        action,
    })
}
