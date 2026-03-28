//! Dual-process router — dispatches evolution actions based on prediction error category.
//!
//! System 1 (Negligible/Moderate): fast, zero LLM cost — update stats, store memory.
//! System 2 (Significant/Critical): slow, LLM-powered — trigger reflection or GVU loop.

use serde::{Deserialize, Serialize};

use super::engine::{ErrorCategory, PredictionError};

/// Action to take after calculating a prediction error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvolutionAction {
    /// No action needed — prediction was accurate. System 1 only.
    None,

    /// Store an observation as episodic memory (no LLM). System 1.5.
    StoreEpisodic {
        content: String,
        importance: f64,
    },

    /// Trigger a deep LLM reflection (Meso-level). System 2.
    TriggerReflection {
        context: String,
    },

    /// Trigger emergency evolution — immediate GVU loop. System 2+.
    TriggerEmergencyEvolution {
        context: String,
    },
}

/// Route a prediction error to the appropriate evolution action.
///
/// Takes into account both the current error category and the history
/// of consecutive significant errors (escalation).
pub fn route(error: &PredictionError, consecutive_significant: usize) -> EvolutionAction {
    match error.category {
        ErrorCategory::Negligible => EvolutionAction::None,

        ErrorCategory::Moderate => {
            let content = format!(
                "Prediction deviation: expected satisfaction {:.2}, inferred {:.2} (delta {:.2}). \
                 Topic surprise: {:.2}. Corrections: {}. Follow-ups: {}.",
                error.prediction.expected_satisfaction,
                error.prediction.expected_satisfaction - error.delta_satisfaction,
                error.delta_satisfaction,
                error.topic_surprise,
                if error.unexpected_correction { "yes" } else { "no" },
                if error.unexpected_follow_up { "yes" } else { "no" },
            );
            // Importance scales with composite error (range 4.0 - 7.0 for Moderate)
            let importance = 4.0 + error.composite_error * 6.0;
            EvolutionAction::StoreEpisodic { content, importance }
        }

        ErrorCategory::Significant => {
            let context = format_reflection_context(error);

            // Escalation: 3+ consecutive Significant → treat as emergency
            if consecutive_significant >= 3 {
                EvolutionAction::TriggerEmergencyEvolution { context }
            } else {
                EvolutionAction::TriggerReflection { context }
            }
        }

        ErrorCategory::Critical => {
            let context = format_reflection_context(error);
            EvolutionAction::TriggerEmergencyEvolution { context }
        }
    }
}

/// Format a detailed context string for LLM reflection.
fn format_reflection_context(error: &PredictionError) -> String {
    let mut sections = Vec::new();

    sections.push(format!(
        "## Prediction Error Report\n\
         - Composite error: {:.3} (category: {:?})\n\
         - Confidence in prediction: {:.2}\n\
         - Delta satisfaction: {:.3}\n\
         - Topic surprise: {:.3}",
        error.composite_error,
        error.category,
        error.prediction.confidence,
        error.delta_satisfaction,
        error.topic_surprise,
    ));

    if error.unexpected_correction {
        sections.push(
            "## Unexpected Correction\n\
             The user corrected the agent when the model predicted a smooth interaction. \
             This suggests the agent's understanding or tone may be misaligned."
                .to_string(),
        );
    }

    if error.unexpected_follow_up {
        sections.push(
            "## Unexpected Follow-ups\n\
             The user asked multiple follow-up questions when few were expected. \
             This may indicate the agent's responses lack sufficient depth or clarity."
                .to_string(),
        );
    }

    sections.push(format!(
        "## Conversation Summary\n\
         - Messages: {} total ({} user, {} assistant)\n\
         - Avg response length: {:.0} chars\n\
         - Language: {}\n\
         - Topics: {}",
        error.actual.message_count,
        error.actual.user_message_count,
        error.actual.assistant_message_count,
        error.actual.avg_assistant_response_length,
        error.actual.detected_language,
        error.actual.extracted_topics.join(", "),
    ));

    sections.join("\n\n")
}
