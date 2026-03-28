//! Metacognition — self-calibrating thresholds for the prediction engine.
//!
//! Inspired by ICML 2025 "Truly Self-Improving Agents Require Intrinsic
//! Metacognitive Learning": the evolution engine doesn't just improve agent
//! performance — it evaluates and adjusts its own triggering thresholds.
//!
//! Every `evaluation_interval` predictions, the metacognition layer checks
//! whether each error category's triggers are effective and adjusts thresholds
//! up (less sensitive) or down (more sensitive) accordingly.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::engine::{ErrorCategory, PredictionError};

// ---------------------------------------------------------------------------
// AdaptiveThresholds
// ---------------------------------------------------------------------------

/// Thresholds that divide composite_error into ErrorCategory buckets.
///
/// These adapt over time based on measured effectiveness of each category's
/// evolution response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveThresholds {
    /// Upper bound for Negligible (below this = Negligible).
    pub negligible_upper: f64,
    /// Upper bound for Moderate (below this = Moderate, above = Significant).
    pub moderate_upper: f64,
    /// Upper bound for Significant (above this = Critical).
    pub significant_upper: f64,
}

impl Default for AdaptiveThresholds {
    fn default() -> Self {
        Self {
            negligible_upper: 0.2,
            moderate_upper: 0.5,
            significant_upper: 0.8,
        }
    }
}

impl AdaptiveThresholds {
    /// Classify a composite error into a category.
    pub fn category_for(&self, composite_error: f64) -> ErrorCategory {
        if composite_error < self.negligible_upper {
            ErrorCategory::Negligible
        } else if composite_error < self.moderate_upper {
            ErrorCategory::Moderate
        } else if composite_error < self.significant_upper {
            ErrorCategory::Significant
        } else {
            ErrorCategory::Critical
        }
    }
}

// ---------------------------------------------------------------------------
// LayerEffectiveness
// ---------------------------------------------------------------------------

/// Tracks how effective a particular error category's response is.
///
/// Uses a rolling window: only the last `window_size` events are counted,
/// preventing early cold-start data from permanently polluting the signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerEffectiveness {
    /// Rolling window of recent outcomes (true = improved, false = not improved).
    pub recent_outcomes: Vec<bool>,
    /// Maximum window size (default 50).
    pub window_size: usize,
    /// Lifetime trigger count (for diagnostics only, not used in rate calculation).
    pub total_triggers: u64,
}

impl Default for LayerEffectiveness {
    fn default() -> Self {
        Self {
            recent_outcomes: Vec::new(),
            window_size: 50,
            total_triggers: 0,
        }
    }
}

impl LayerEffectiveness {
    /// Record a trigger (outcome not yet known).
    pub fn record_trigger(&mut self) {
        self.total_triggers += 1;
    }

    /// Record an outcome for a previous trigger.
    pub fn record_outcome(&mut self, improved: bool) {
        self.recent_outcomes.push(improved);
        // Evict oldest if window exceeded
        while self.recent_outcomes.len() > self.window_size {
            self.recent_outcomes.remove(0);
        }
    }

    /// Improvement rate over the rolling window (0.0 - 1.0). Returns 0.5 if no data.
    pub fn improvement_rate(&self) -> f64 {
        if self.recent_outcomes.is_empty() {
            0.5
        } else {
            let improved = self.recent_outcomes.iter().filter(|&&b| b).count() as f64;
            improved / self.recent_outcomes.len() as f64
        }
    }

    /// Number of outcomes in the current window.
    pub fn window_count(&self) -> usize {
        self.recent_outcomes.len()
    }
}

// ---------------------------------------------------------------------------
// MetaCognition
// ---------------------------------------------------------------------------

/// Self-calibrating metacognition system.
///
/// Periodically evaluates whether the prediction engine's thresholds are
/// well-calibrated and adjusts them based on measured outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaCognition {
    /// Current adaptive thresholds.
    pub thresholds: AdaptiveThresholds,

    /// Effectiveness tracking per error category.
    pub layer_stats: HashMap<String, LayerEffectiveness>,

    /// How many predictions between evaluations.
    pub evaluation_interval: u64,

    /// Predictions since last evaluation.
    pub predictions_since_last_eval: u64,

    /// Total predictions ever made.
    pub total_predictions: u64,
}

impl Default for MetaCognition {
    fn default() -> Self {
        Self {
            thresholds: AdaptiveThresholds::default(),
            layer_stats: HashMap::new(),
            evaluation_interval: 100,
            predictions_since_last_eval: 0,
            total_predictions: 0,
        }
    }
}

impl MetaCognition {
    /// Record a prediction error (called after every prediction).
    pub fn record_prediction(&mut self, error: &PredictionError) {
        let key = format!("{:?}", error.category);
        let stats = self.layer_stats.entry(key).or_default();
        stats.record_trigger();

        self.predictions_since_last_eval += 1;
        self.total_predictions += 1;
    }

    /// Record whether a triggered evolution actually improved things.
    ///
    /// Called after an evolution cycle completes and we can measure the outcome.
    /// Uses rolling window so early cold-start data doesn't permanently pollute.
    pub fn record_outcome(&mut self, category: ErrorCategory, improved: bool) {
        let key = format!("{category:?}");
        let stats = self.layer_stats.entry(key).or_default();
        stats.record_outcome(improved);
    }

    /// Whether it's time to evaluate and adjust thresholds.
    pub fn should_evaluate(&self) -> bool {
        self.predictions_since_last_eval >= self.evaluation_interval
    }

    /// Evaluate threshold effectiveness and adjust.
    ///
    /// The key insight: if a category triggers often but rarely leads to
    /// improvement, the threshold is too low (too sensitive). If the next
    /// category up has a high trigger-to-improvement ratio, the lower
    /// threshold might be too high (missing opportunities).
    pub fn evaluate_and_adjust(&mut self) {
        let sig_key = format!("{:?}", ErrorCategory::Significant);
        let crit_key = format!("{:?}", ErrorCategory::Critical);
        let mod_key = format!("{:?}", ErrorCategory::Moderate);

        let sig_rate = self
            .layer_stats
            .get(&sig_key)
            .map(|s| s.improvement_rate())
            .unwrap_or(0.5);

        let crit_proportion = {
            let crit_triggers = self.layer_stats.get(&crit_key).map(|s| s.total_triggers).unwrap_or(0);
            let total_triggers: u64 = self.layer_stats.values().map(|s| s.total_triggers).sum();
            if total_triggers > 0 {
                crit_triggers as f64 / total_triggers as f64
            } else {
                0.0
            }
        };

        let _mod_rate = self
            .layer_stats
            .get(&mod_key)
            .map(|s| s.improvement_rate())
            .unwrap_or(0.5);

        let mut adjusted = false;

        // If Significant triggers rarely lead to improvement → too sensitive, raise threshold
        // Uses window_count (rolling window) as the minimum sample guard
        if sig_rate < 0.3 && self.layer_stats.get(&sig_key).map(|s| s.window_count()).unwrap_or(0) >= 5 {
            self.thresholds.moderate_upper = (self.thresholds.moderate_upper + 0.05).min(0.85);
            adjusted = true;
        }

        // If Significant triggers frequently lead to improvement → well-calibrated or too conservative
        if sig_rate > 0.7 && self.layer_stats.get(&sig_key).map(|s| s.window_count()).unwrap_or(0) >= 5 {
            self.thresholds.moderate_upper = (self.thresholds.moderate_upper - 0.03).max(0.2);
            adjusted = true;
        }

        // If Critical proportion is too high → Moderate/Significant thresholds are too high
        if crit_proportion > 0.2 {
            self.thresholds.significant_upper = (self.thresholds.significant_upper - 0.05).max(0.4);
            adjusted = true;
        }

        // Clamp all thresholds to valid ranges
        self.thresholds.negligible_upper = self.thresholds.negligible_upper.clamp(0.1, 0.4);
        self.thresholds.moderate_upper = self.thresholds.moderate_upper.clamp(0.2, 0.85);
        self.thresholds.significant_upper = self.thresholds.significant_upper.clamp(0.4, 0.95);

        // Ensure ordering: negligible < moderate < significant
        if self.thresholds.negligible_upper >= self.thresholds.moderate_upper {
            self.thresholds.negligible_upper = self.thresholds.moderate_upper - 0.05;
        }
        if self.thresholds.moderate_upper >= self.thresholds.significant_upper {
            self.thresholds.moderate_upper = self.thresholds.significant_upper - 0.05;
        }

        if adjusted {
            info!(
                negligible = format!("{:.2}", self.thresholds.negligible_upper),
                moderate = format!("{:.2}", self.thresholds.moderate_upper),
                significant = format!("{:.2}", self.thresholds.significant_upper),
                "Metacognition adjusted thresholds"
            );
        }

        // Reset counter (but keep layer_stats for ongoing tracking)
        self.predictions_since_last_eval = 0;
    }

    /// Persist state to a JSON file.
    pub fn persist(&self, path: &Path) {
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    warn!("Failed to persist metacognition: {e}");
                }
            }
            Err(e) => warn!("Failed to serialize metacognition: {e}"),
        }
    }

    /// Load state from a JSON file.
    pub fn load(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }
}
