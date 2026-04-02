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
