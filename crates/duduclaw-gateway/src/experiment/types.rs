//! Data types for experiment recording.

use serde::{Deserialize, Serialize};

/// Per-conversation experiment record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentRecord {
    pub conversation_id: String,
    pub agent_id: String,
    pub user_id: String,
    pub channel: String,
    pub predicted_satisfaction: f64,
    pub actual_inferred_satisfaction: f64,
    pub composite_error: f64,
    pub error_category: String, // Negligible / Moderate / Significant / Critical
    pub evolution_action: String, // None / StoreEpisodic / TriggerReflection / TriggerEmergency
    pub llm_calls_count: u32,
    pub llm_tokens_used: u64,
    pub latency_ms: u64,
    pub timestamp: String, // RFC3339
}

/// GVU self-play round record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GvuRecord {
    pub agent_id: String,
    pub conversation_id: String,
    pub gvu_rounds: u32,
    pub final_verdict: String, // approved / rejected
    pub l1_passed: bool,
    pub l2_passed: bool,
    pub l3_passed: bool,
    pub l4_passed: bool,
    pub text_gradient_count: u32,
    pub generator_model: String,
    pub verifier_model: String,
    pub total_llm_cost_usd: f64,
    pub timestamp: String,
}

/// SOUL.md version outcome record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionOutcomeRecord {
    pub agent_id: String,
    pub version_id: String,
    pub status: String, // Confirmed / RolledBack
    pub observation_hours: f64,
    pub pre_satisfaction: f64,
    pub post_satisfaction: f64,
    pub pre_correction_rate: f64,
    pub post_correction_rate: f64,
    pub rollback_reason: Option<String>,
    pub timestamp: String,
}

// ---------------------------------------------------------------------------
// Evolution event logging (Sutskever Day 1 principle + Kahneman anchors)
// ---------------------------------------------------------------------------

/// Type of evolution event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvolutionEventType {
    /// Prediction error was calculated for a conversation.
    PredictionError,
    /// GVU reflection was triggered.
    GvuTrigger,
    /// SOUL.md was updated (new version applied).
    SoulUpdate,
    /// SOUL.md was rolled back.
    Rollback,
    /// SOUL.md version was confirmed after observation.
    Confirmed,
    /// Epsilon-floor forced exploration triggered.
    EpistemicForaging,
    /// Anti-sycophancy alert triggered.
    SycophancyAlert,
}

impl std::fmt::Display for EvolutionEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PredictionError => write!(f, "prediction_error"),
            Self::GvuTrigger => write!(f, "gvu_trigger"),
            Self::SoulUpdate => write!(f, "soul_update"),
            Self::Rollback => write!(f, "rollback"),
            Self::Confirmed => write!(f, "confirmed"),
            Self::EpistemicForaging => write!(f, "epistemic_foraging"),
            Self::SycophancyAlert => write!(f, "sycophancy_alert"),
        }
    }
}

/// External validation anchor for a GVU evolution event.
///
/// Every GVU outcome should have at least one external signal (Kahneman principle).
/// Records without external validation are marked as "unverified" and receive
/// reduced learning weight in Phase 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalValidation {
    /// Type of external signal.
    pub validation_type: String, // "user_feedback" | "human_check" | "ab_metric" | "channel_metric"
    /// Signal value: -1.0 (strongly negative) to 1.0 (strongly positive).
    pub signal_value: f64,
    /// Timestamp when the external signal was received.
    pub timestamp: String, // RFC3339
}

/// Structured evolution event record.
///
/// Captures every meaningful state change in the prediction-evolution pipeline.
/// This is the system's "memory of itself" (Sutskever principle) — the raw
/// material for Phase 2 evolution history learning and Phase 3 world model training.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionEvent {
    /// Unique event identifier.
    pub event_id: String,
    /// Agent this event belongs to.
    pub agent_id: String,
    /// Event type.
    pub event_type: EvolutionEventType,
    /// Prediction composite error (for PredictionError events).
    pub composite_error: Option<f64>,
    /// Error category (for PredictionError events).
    pub error_category: Option<String>,
    /// GVU trigger context (for GvuTrigger events).
    pub trigger_context: Option<String>,
    /// SOUL.md diff content (for SoulUpdate/Rollback events).
    pub soul_diff: Option<String>,
    /// Version ID (for SoulUpdate/Rollback/Confirmed events).
    pub version_id: Option<String>,
    /// Rollback reason (for Rollback events).
    pub rollback_reason: Option<String>,
    /// External validation anchor (Kahneman principle).
    /// None = "unverified" — reduced learning weight in Phase 2.
    pub external_validation: Option<ExternalValidation>,
    /// RFC3339 timestamp.
    pub timestamp: String,
}
