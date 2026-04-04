//! Metacognition — self-calibrating thresholds for the prediction engine.
//!
//! Inspired by ICML 2025 "Truly Self-Improving Agents Require Intrinsic
//! Metacognitive Learning": the evolution engine doesn't just improve agent
//! performance — it evaluates and adjusts its own triggering thresholds.
//!
//! ## Hardening (2025-Q2)
//!
//! - **SurpriseDeficitTracker**: Forces GVU exploration when prediction errors
//!   are consistently too low (dark room convergence). Based on Active Inference
//!   epistemic foraging (Parr, Pezzulo & Friston 2024).
//! - **High-confidence penalty**: Lowers thresholds when accuracy is suspiciously
//!   high for too long (Fountas et al. 2023).
//! - **Accumulation Principle**: Blends original baseline stats with current stats
//!   to prevent feedback loop amplification (Gerstgrasser et al. ICLR 2025).
//! - **CUSUM ChangePointDetector**: Replaces fixed 100-prediction evaluation
//!   interval with adaptive shift detection (Suk 2024).

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
    pub recent_outcomes: std::collections::VecDeque<bool>,
    /// Maximum window size (default 50).
    pub window_size: usize,
    /// Lifetime trigger count (for diagnostics only, not used in rate calculation).
    pub total_triggers: u64,
}

impl Default for LayerEffectiveness {
    fn default() -> Self {
        Self {
            recent_outcomes: std::collections::VecDeque::new(),
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
        self.recent_outcomes.push_back(improved);
        while self.recent_outcomes.len() > self.window_size {
            self.recent_outcomes.pop_front();
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
// SurpriseDeficitTracker — anti-dark-room
// ---------------------------------------------------------------------------

/// Tracks cumulative surprise deficit to detect dark room convergence.
///
/// When prediction errors are consistently below a floor, the cumulative
/// deficit grows. Once it exceeds a budget, forced exploration is triggered.
///
/// Based on Active Inference epistemic foraging: agents must maintain a
/// minimum level of surprise to ensure continued learning.
/// (Parr, Pezzulo & Friston 2024)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurpriseDeficitTracker {
    /// Minimum expected surprise level (composite error floor).
    pub expected_surprise_floor: f64,
    /// Accumulated deficit: Σ max(0, floor - actual_error).
    pub cumulative_deficit: f64,
    /// Budget: when cumulative_deficit exceeds this, force exploration.
    pub deficit_budget: f64,
    /// How many times the deficit budget has been exceeded (lifetime).
    pub forced_exploration_count: u64,
}

impl Default for SurpriseDeficitTracker {
    fn default() -> Self {
        Self {
            expected_surprise_floor: 0.15,
            cumulative_deficit: 0.0,
            deficit_budget: 2.0,
            forced_exploration_count: 0,
        }
    }
}

impl SurpriseDeficitTracker {
    /// Record a prediction error and accumulate any deficit.
    pub fn record(&mut self, composite_error: f64) {
        let deficit = (self.expected_surprise_floor - composite_error).max(0.0);
        self.cumulative_deficit += deficit;
    }

    /// Whether the deficit budget is exceeded (force exploration).
    pub fn should_force_exploration(&self) -> bool {
        self.cumulative_deficit > self.deficit_budget
    }

    /// Reset deficit after forced exploration is triggered.
    pub fn reset(&mut self) {
        self.cumulative_deficit = 0.0;
        self.forced_exploration_count += 1;
    }
}

// ---------------------------------------------------------------------------
// CUSUM ChangePointDetector — adaptive evaluation interval
// ---------------------------------------------------------------------------

/// Detects significant distribution shifts in prediction errors using CUSUM.
///
/// Replaces the fixed 100-prediction evaluation interval with adaptive
/// detection: only recalibrate thresholds when a real shift is detected.
///
/// Based on Suk (2024) "Adaptive Smooth Non-Stationary Bandits".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePointDetector {
    /// Running mean of composite errors.
    running_mean: f64,
    /// Running count for mean calculation.
    count: u64,
    /// CUSUM positive statistic (detects upward shifts).
    cusum_pos: f64,
    /// CUSUM negative statistic (detects downward shifts).
    cusum_neg: f64,
    /// Slack parameter (minimum detectable shift / 2).
    slack: f64,
    /// Detection threshold.
    threshold: f64,
    /// Latched detection flag — set by `record()`, cleared by `acknowledge()`.
    /// Prevents the detection from being lost between `record()` and `should_evaluate()`.
    detected: bool,
}

impl Default for ChangePointDetector {
    fn default() -> Self {
        Self {
            running_mean: 0.3,  // initial estimate
            count: 0,
            cusum_pos: 0.0,
            cusum_neg: 0.0,
            slack: 0.05,
            threshold: 4.0,
            detected: false,
        }
    }
}

impl ChangePointDetector {
    /// Record a new composite error and check for distribution shift.
    /// Returns `true` if a change point is detected.
    pub fn record(&mut self, composite_error: f64) -> bool {
        // CUSUM deviation BEFORE updating mean — otherwise the deviation is always
        // near-zero because we'd be comparing against a mean that includes this observation.
        // (Audit issue #3: neutered change-point detector)
        self.cusum_pos = (self.cusum_pos + composite_error - self.running_mean - self.slack).max(0.0);
        self.cusum_neg = (self.cusum_neg - composite_error + self.running_mean - self.slack).max(0.0);

        // THEN update running mean (Welford's online mean)
        self.count += 1;
        let delta = composite_error - self.running_mean;
        self.running_mean += delta / self.count as f64;

        if self.cusum_pos > self.threshold || self.cusum_neg > self.threshold {
            // Latch the detection and reset CUSUM accumulators
            self.detected = true;
            self.cusum_pos = 0.0;
            self.cusum_neg = 0.0;
            true
        } else {
            false
        }
    }

    /// Whether a change point has been detected since the last acknowledgment.
    pub fn is_detected(&self) -> bool {
        self.detected
    }

    /// Acknowledge a detection — clears the latched flag.
    pub fn acknowledge(&mut self) {
        self.detected = false;
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

    /// How many predictions between evaluations (fallback if CUSUM disabled).
    pub evaluation_interval: u64,

    /// Predictions since last evaluation.
    pub predictions_since_last_eval: u64,

    /// Total predictions ever made.
    pub total_predictions: u64,

    // ── Hardening: anti-dark-room (Risk 2) ─────────────────────

    /// Surprise deficit tracker — forces exploration when predictions are
    /// consistently too accurate (dark room convergence).
    #[serde(default)]
    pub surprise_deficit: SurpriseDeficitTracker,

    /// Consecutive accurate predictions counter (for high-confidence penalty).
    #[serde(default)]
    pub consecutive_accurate: u64,

    // ── Hardening: anti-feedback-loop (Risk 3) ─────────────────

    /// CUSUM change-point detector — replaces fixed evaluation interval.
    #[serde(default)]
    pub change_detector: ChangePointDetector,

    /// Original improvement rate for Significant category (anchored at first calibration).
    /// Used for Accumulation Principle: blend 30% original + 70% current.
    #[serde(default)]
    pub original_sig_improvement_rate: Option<f64>,

    // ── Proactive self-calibration (Phase D3) ───────────────────

    /// Proactive message threshold (0.0-1.0). Only send proactive messages
    /// when the motivation score exceeds this threshold.
    /// Self-calibrates based on user accept/dismiss feedback.
    #[serde(default = "default_proactive_threshold")]
    pub proactive_threshold: f64,

    /// Total proactive messages sent (for calibration).
    #[serde(default)]
    pub proactive_sent: u64,

    /// Total proactive messages accepted by users.
    #[serde(default)]
    pub proactive_accepted: u64,

    /// Total proactive messages dismissed by users.
    #[serde(default)]
    pub proactive_dismissed: u64,

    /// Proactive evaluations since last calibration.
    #[serde(default)]
    pub proactive_since_last_cal: u64,
}

impl Default for MetaCognition {
    fn default() -> Self {
        Self {
            thresholds: AdaptiveThresholds::default(),
            layer_stats: HashMap::new(),
            evaluation_interval: 100,
            predictions_since_last_eval: 0,
            total_predictions: 0,
            surprise_deficit: SurpriseDeficitTracker::default(),
            consecutive_accurate: 0,
            change_detector: ChangePointDetector::default(),
            original_sig_improvement_rate: None,
            proactive_threshold: 0.5,
            proactive_sent: 0,
            proactive_accepted: 0,
            proactive_dismissed: 0,
            proactive_since_last_cal: 0,
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

        // --- Hardening: surprise deficit tracking (Risk 2) ---
        self.surprise_deficit.record(error.composite_error);

        // Track consecutive accurate predictions (high-confidence penalty)
        if error.category == ErrorCategory::Negligible {
            self.consecutive_accurate += 1;
        } else {
            self.consecutive_accurate = 0;
        }

        // --- Hardening: CUSUM change-point detection (Risk 3) ---
        // Feed composite error to the change detector (result used in should_evaluate)
        self.change_detector.record(error.composite_error);
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
    ///
    /// Uses CUSUM change-point detection as primary trigger, with fixed
    /// interval as fallback.
    pub fn should_evaluate(&self) -> bool {
        // CUSUM detected a distribution shift (latched flag, not stale values)
        let cusum_triggered = self.change_detector.is_detected();

        // Fallback: fixed interval
        let interval_triggered = self.predictions_since_last_eval >= self.evaluation_interval;

        cusum_triggered || interval_triggered
    }

    /// Whether the surprise deficit tracker requires forced exploration.
    pub fn should_force_exploration(&self) -> bool {
        self.surprise_deficit.should_force_exploration()
    }

    /// Reset surprise deficit after forced exploration is acted upon.
    pub fn reset_surprise_deficit(&mut self) {
        self.surprise_deficit.reset();
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

        let current_sig_rate = self
            .layer_stats
            .get(&sig_key)
            .map(|s| s.improvement_rate())
            .unwrap_or(0.5);

        // --- Hardening: Accumulation Principle (Risk 3) ---
        // Anchor first-ever sig improvement rate as baseline.
        // Blend 30% original + 70% current to prevent feedback loop amplification.
        // (Gerstgrasser et al. ICLR 2025 "Is Model Collapse Inevitable?")
        if self.original_sig_improvement_rate.is_none()
            && self.layer_stats.get(&sig_key).map(|s| s.window_count()).unwrap_or(0) >= 5
        {
            self.original_sig_improvement_rate = Some(current_sig_rate);
            info!(rate = format!("{current_sig_rate:.2}"), "Anchored original sig improvement rate");
        }

        let sig_rate = if let Some(original) = self.original_sig_improvement_rate {
            0.3 * original + 0.7 * current_sig_rate
        } else {
            current_sig_rate
        };

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
        if sig_rate < 0.3 && self.layer_stats.get(&sig_key).map(|s| s.window_count()).unwrap_or(0) >= 5 {
            self.thresholds.moderate_upper = (self.thresholds.moderate_upper + 0.05).min(0.85);
            adjusted = true;
        }

        // If Significant triggers frequently lead to improvement → too conservative
        if sig_rate > 0.7 && self.layer_stats.get(&sig_key).map(|s| s.window_count()).unwrap_or(0) >= 5 {
            self.thresholds.moderate_upper = (self.thresholds.moderate_upper - 0.03).max(0.2);
            adjusted = true;
        }

        // If Critical proportion is too high → thresholds are too high
        if crit_proportion > 0.2 {
            self.thresholds.significant_upper = (self.thresholds.significant_upper - 0.05).max(0.4);
            adjusted = true;
        }

        // --- Hardening: high-confidence penalty (Risk 2) ---
        // When predictions are accurate for too long, the prediction space may
        // have narrowed rather than improved. Lower thresholds to let more
        // errors through. (Fountas et al. 2023)
        if self.consecutive_accurate > 200 {
            self.thresholds.negligible_upper = (self.thresholds.negligible_upper - 0.03).max(0.1);
            adjusted = true;
            info!(
                consecutive = self.consecutive_accurate,
                "High-confidence penalty applied — lowering negligible threshold"
            );
            // Reset to prevent repeated penalty application on every subsequent
            // evaluate_and_adjust call (which would drive threshold to minimum).
            self.consecutive_accurate = 0;
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

        // Acknowledge CUSUM detection so it doesn't re-trigger immediately
        self.change_detector.acknowledge();
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

    // ── Proactive self-calibration (Phase D3) ───────────────────

    /// Record that a proactive message was sent.
    pub fn record_proactive_sent(&mut self) {
        self.proactive_sent += 1;
        self.proactive_since_last_cal += 1;
    }

    /// Record user feedback on a proactive message.
    pub fn record_proactive_feedback(&mut self, accepted: bool) {
        if accepted {
            self.proactive_accepted += 1;
        } else {
            self.proactive_dismissed += 1;
        }
        self.proactive_since_last_cal += 1;

        // Calibrate every 20 proactive interactions
        if self.proactive_since_last_cal >= 20 {
            self.calibrate_proactive_threshold();
            self.proactive_since_last_cal = 0;
        }
    }

    /// Self-calibrate the proactive threshold based on accept/dismiss ratio.
    ///
    /// - High accept rate (>70%) → lower threshold (more proactive)
    /// - Low accept rate (<30%) → raise threshold (less proactive)
    /// - Otherwise → no change
    fn calibrate_proactive_threshold(&mut self) {
        let total = self.proactive_accepted + self.proactive_dismissed;
        if total < 5 {
            return; // Not enough data
        }

        let accept_rate = self.proactive_accepted as f64 / total as f64;
        let old_threshold = self.proactive_threshold;

        if accept_rate > 0.7 {
            // Users welcome proactive messages → lower threshold (more proactive)
            self.proactive_threshold = (self.proactive_threshold - 0.05).max(0.2);
        } else if accept_rate < 0.3 {
            // Users dismiss proactive messages → raise threshold (less proactive)
            self.proactive_threshold = (self.proactive_threshold + 0.05).min(0.9);
        }

        if (self.proactive_threshold - old_threshold).abs() > f64::EPSILON {
            info!(
                old = format!("{old_threshold:.2}"),
                new = format!("{:.2}", self.proactive_threshold),
                accept_rate = format!("{:.1}", accept_rate * 100.0),
                "MetaCognition: proactive threshold calibrated"
            );
        }
    }

    /// Get the current proactive threshold.
    pub fn proactive_threshold(&self) -> f64 {
        self.proactive_threshold
    }

    /// Get proactive stats summary.
    pub fn proactive_stats(&self) -> (u64, u64, u64, f64) {
        (self.proactive_sent, self.proactive_accepted, self.proactive_dismissed, self.proactive_threshold)
    }
}

fn default_proactive_threshold() -> f64 {
    0.5
}
