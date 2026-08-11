//! Dual-process router — dispatches evolution actions based on prediction error category.
//!
//! System 1 (Negligible/Moderate): fast, zero LLM cost — update stats, store memory.
//! System 2 (Significant/Critical): slow, LLM-powered — trigger reflection or GVU loop.
//!
//! ## Hardening (2025-Q2)
//!
//! - **Epsilon-floor exploration**: Forced GVU reflection for 5% of Negligible errors.
//!   Prevents dark room convergence (Parr, Pezzulo & Friston 2024).
//! - **ConsistencyTracker**: Detects sycophantic capitulation patterns.
//!   (Sharma et al. ICLR 2024, Denison et al. Anthropic 2024)

use std::collections::VecDeque;

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

// ---------------------------------------------------------------------------
// Consistency tracker — anti-sycophancy
// ---------------------------------------------------------------------------

/// Tracks agent capitulation patterns to detect sycophancy drift.
///
/// A "capitulation" is when the agent changes its substantive position in
/// response to user sentiment (not new evidence). High capitulation rate
/// suggests the agent is evolving toward sycophantic behaviour.
///
/// Based on Sharma et al. (ICLR 2024) "Towards Understanding Sycophancy in LMs".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsistencyTracker {
    /// Rolling window of capitulation events (true = capitulated).
    capitulation_window: VecDeque<bool>,
    /// Window size (default 50).
    window_size: usize,
    /// Total capitulation events recorded.
    total_events: u64,
}

impl ConsistencyTracker {
    pub fn new(window_size: usize) -> Self {
        Self {
            capitulation_window: VecDeque::new(),
            window_size,
            total_events: 0,
        }
    }

    /// Record whether the agent capitulated in a conversation.
    ///
    /// `capitulated`: true if the agent changed its position after user expressed
    /// displeasure WITHOUT providing new factual information.
    pub fn record(&mut self, capitulated: bool) {
        self.capitulation_window.push_back(capitulated);
        while self.capitulation_window.len() > self.window_size {
            self.capitulation_window.pop_front();
        }
        self.total_events += 1;
    }

    /// Current capitulation rate (0.0 - 1.0). Returns 0.0 if no data.
    pub fn capitulation_rate(&self) -> f64 {
        if self.capitulation_window.is_empty() {
            0.0
        } else {
            let cap = self.capitulation_window.iter().filter(|&&b| b).count() as f64;
            cap / self.capitulation_window.len() as f64
        }
    }

    /// Whether sycophancy drift is detected (rate > threshold).
    pub fn is_sycophantic(&self, threshold: f64) -> bool {
        self.capitulation_window.len() >= 10 && self.capitulation_rate() > threshold
    }
}

// ---------------------------------------------------------------------------
// Epsilon-floor exploration state
// ---------------------------------------------------------------------------

/// Exploration state for the epsilon-greedy anti-dark-room mechanism.
///
/// Ensures a minimum forced exploration rate even when prediction errors
/// are consistently Negligible, preventing convergence to a "dark room"
/// where the agent only handles predictable interactions.
///
/// Based on Active Inference EFE decomposition (Da Costa et al. 2020):
/// epistemic value must be explicitly maintained alongside pragmatic value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationState {
    /// Total routing decisions made.
    pub total_routes: u64,
    /// Minimum exploration rate (default 0.05 = 5%).
    pub epsilon_min: f64,
    /// Initial exploration rate (decays with sqrt(total_routes)).
    pub epsilon_init: f64,
    /// Simple PRNG state for deterministic testing.
    /// `pub(crate)` allows tests to seed the counter for reproducible results.
    pub(crate) rng_counter: u64,
}

impl Default for ExplorationState {
    fn default() -> Self {
        Self {
            total_routes: 0,
            epsilon_min: 0.05,
            epsilon_init: 0.2,
            rng_counter: 0,
        }
    }
}

/// Hard-coded minimum exploration rate (Friston principle).
/// This constant CANNOT be overridden by MetaCognition, config, or any runtime mechanism.
/// It ensures the system always maintains epistemic foraging to prevent dark room convergence.
const EPSILON_FLOOR_ABSOLUTE: f64 = 0.05;

impl ExplorationState {
    /// Current epsilon value: decays from `epsilon_init` to `epsilon_min`.
    ///
    /// Guarantees: epsilon >= EPSILON_FLOOR_ABSOLUTE (0.05) at all times.
    /// This floor is enforced by a hard constant, not by `self.epsilon_min`,
    /// so even if `epsilon_min` is accidentally set to 0.0, the floor holds.
    pub fn epsilon(&self) -> f64 {
        if self.total_routes == 0 {
            self.epsilon_init.max(EPSILON_FLOOR_ABSOLUTE)
        } else {
            f64::max(
                EPSILON_FLOOR_ABSOLUTE, // hard floor — cannot be bypassed
                f64::max(
                    self.epsilon_min,
                    self.epsilon_init / (self.total_routes as f64).sqrt(),
                ),
            )
        }
    }

    /// Record a routing decision (called unconditionally for all categories).
    /// This ensures epsilon decay is based on ALL conversations, not just Negligible ones.
    pub fn record_route(&mut self) {
        self.total_routes += 1;
    }

    /// Whether this routing decision should be forced exploration.
    /// Uses a simple hash-based approach for reproducibility.
    /// NOTE: call `record_route()` first to ensure epsilon decay is correct.
    pub fn should_explore(&mut self) -> bool {
        self.rng_counter = self.rng_counter.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let random_val = (self.rng_counter >> 33) as f64 / (u32::MAX as f64);
        random_val < self.epsilon()
    }
}

// ---------------------------------------------------------------------------
// Skill extraction hook
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Exploration marker — the AEE strategy-mix trigger surface (WP2.3 §3.2.2)
// ---------------------------------------------------------------------------

/// Section header that marks a reflection as ε-floor **exploration** rather
/// than a failure-driven repair.
///
/// AEE's intent selection needs to tell these two apart: an exploration round
/// has no concrete failure behind it and is therefore the natural window for
/// `Innovate`, whereas a Significant/Critical error round must consume the
/// failure it was born from. Before this constant existed the two were
/// indistinguishable — both arrived as `TriggerReflection { context }` — so
/// every exploration round was being spent on repair work it had no material
/// for. Kept as one constant used by both the writer and
/// [`context_is_exploration`] so they cannot drift apart.
pub const EXPLORATION_MARKER: &str = "## Epistemic Foraging";

/// Classify a reflection context produced by [`route`].
///
/// Anchored to the start of a line (never a bare `contains`, per CLAUDE.md
/// security convention #2) so a user message quoting the phrase cannot make
/// an ordinary error round look like exploration.
pub fn context_is_exploration(context: &str) -> bool {
    context.lines().any(|l| l.trim_start().starts_with(EXPLORATION_MARKER))
}

/// Check if the current context warrants skill extraction alongside evolution.
///
/// When the prediction engine triggers a Reflect or GVU loop, we also check
/// if the recent conversation trajectory could yield a reusable skill.
///
/// Phase 3: Also evaluate if recent trajectory is skill-extractable.
/// Dual purpose: evolve SOUL.md (GVU) + extract procedural skill (SkillBank).
pub fn should_extract_skill(tool_call_count: u32) -> bool {
    tool_call_count >= 5 // Same threshold as TrajectoryRecorder
}

// ---------------------------------------------------------------------------
// Main routing function
// ---------------------------------------------------------------------------

/// Route a prediction error to the appropriate evolution action.
///
/// Takes into account:
/// - Error category (Negligible/Moderate/Significant/Critical)
/// - Consecutive significant error history (escalation)
/// - Epsilon-floor forced exploration (anti-dark-room)
/// - Consistency tracker sycophancy detection
pub fn route(
    error: &PredictionError,
    consecutive_significant: usize,
    exploration: &mut ExplorationState,
    consistency: &ConsistencyTracker,
) -> EvolutionAction {
    // Record every routing decision for epsilon decay (audit #6)
    exploration.record_route();

    // Anti-sycophancy: if capitulation rate is high, force reflection
    if consistency.is_sycophantic(0.4) {
        return EvolutionAction::TriggerReflection {
            context: format!(
                "## Anti-Sycophancy Alert\n\
                 Capitulation rate: {:.1}% (threshold: 40%)\n\
                 The agent appears to be changing positions without new evidence.\n\
                 Review SOUL.md for sycophantic drift.\n\n{}",
                consistency.capitulation_rate() * 100.0,
                format_reflection_context(error),
            ),
        };
    }

    match error.category {
        ErrorCategory::Negligible => {
            // Epsilon-floor: forced exploration even for negligible errors
            if exploration.should_explore() {
                EvolutionAction::TriggerReflection {
                    context: format!(
                        "{EXPLORATION_MARKER} (forced exploration, \u{03B5}={:.3})\n\
                         Low prediction error but exploring to prevent dark-room convergence.\n\n{}",
                        exploration.epsilon(),
                        format_reflection_context(error),
                    ),
                }
            } else {
                EvolutionAction::None
            }
        }

        ErrorCategory::Moderate => {
            let content = format_moderate_telemetry(error);
            // Importance scales with composite error (range 4.0 - 10.0; ~5.2-7.0 for default Moderate thresholds)
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

/// Format the episodic-memory telemetry string stored for a Moderate error.
///
/// The base line is pure numeric telemetry (deviation, surprise, correction/
/// follow-up flags) — useless to a human reading the Memory page without
/// knowing *what conversation* it came from. This appends the conversation's
/// extracted topics (already short keywords/CJK bigrams from `extract_keywords`,
/// so no truncation is needed) when available, so the "learning signal" card
/// can show what the agent was actually learning about.
///
/// `web/src/lib/memory-format.ts`'s `PREDICTION_RE` mirrors this exact format —
/// keep the two in sync when changing either the base line or the optional
/// trailing sections.
fn format_moderate_telemetry(error: &PredictionError) -> String {
    let mut content = format!(
        "Prediction deviation: expected satisfaction {:.2}, inferred {:.2} (delta {:.2}). \
         Topic surprise: {:.2}. Corrections: {}. Follow-ups: {}.",
        error.prediction.expected_satisfaction,
        error.prediction.expected_satisfaction - error.delta_satisfaction,
        error.delta_satisfaction,
        error.topic_surprise,
        if error.unexpected_correction { "yes" } else { "no" },
        if error.unexpected_follow_up { "yes" } else { "no" },
    );

    if !error.actual.extracted_topics.is_empty() {
        let topics = error.actual.extracted_topics.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        content.push_str(&format!(" Topics: {topics}."));
    }

    if let Some(expected) = &error.prediction.expected_topic {
        let already_covered = error.actual.extracted_topics.iter().any(|t| t == expected);
        if !already_covered {
            content.push_str(&format!(" Expected topic: {expected}."));
        }
    }

    content
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

    if error.task_completion_failure {
        sections.push(
            "## Task Completion Failure\n\
             The user's task was detected as incomplete or failed. \
             The agent may need to improve its ability to fully address user requests."
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

#[cfg(test)]
mod exploration_marker_tests {
    use super::*;

    #[test]
    fn epsilon_reflection_context_is_classified_as_exploration() {
        let ctx = format!("{EXPLORATION_MARKER} (forced exploration, \u{03B5}=0.050)\nbody");
        assert!(context_is_exploration(&ctx));
    }

    #[test]
    fn ordinary_error_context_is_not_exploration() {
        assert!(!context_is_exploration(
            "## Prediction Error Report\n- Composite error: 0.812 (category: Critical)"
        ));
    }

    #[test]
    fn a_quoted_marker_inside_prose_does_not_fake_exploration() {
        // The marker must be a line header, not a substring anywhere — a user
        // message echoed into the context must not be able to relabel the
        // round.
        assert!(!context_is_exploration(
            "## Prediction Error Report\nThe user wrote: please read ## Epistemic Foraging docs"
        ));
    }
}
