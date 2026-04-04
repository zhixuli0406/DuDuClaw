//! Shadow-mode observation — parallel SOUL.md comparison without deployment.
//!
//! Runs updated SOUL.md in "shadow" alongside production during the observation
//! period. Collects hypothetical metrics for the new version without affecting
//! real users, enabling pre-commitment evaluation.
//!
//! Based on industry shadow-mode testing practices (2024-2025) and
//! CANDOR (arXiv:2412.08052) counterfactual evaluation methodology.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A shadow observation session comparing old vs new SOUL.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowSession {
    /// Unique session identifier.
    pub session_id: String,
    /// Agent this shadow session belongs to.
    pub agent_id: String,
    /// Version ID of the new SOUL.md being shadowed.
    pub version_id: String,
    /// When the shadow session started.
    pub started_at: DateTime<Utc>,
    /// Production SOUL.md content (current live version).
    pub production_soul: String,
    /// Shadow SOUL.md content (proposed new version).
    pub shadow_soul: String,
    /// Collected comparison results.
    pub comparisons: Vec<ShadowComparison>,
    /// Whether the shadow session is still active.
    pub active: bool,
}

/// A single comparison between production and shadow responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowComparison {
    /// User message that triggered the comparison.
    pub user_message_preview: String,
    /// Timestamp of the comparison.
    pub timestamp: DateTime<Utc>,
    /// Whether the shadow response differed materially from production.
    pub response_diverged: bool,
    /// Divergence score (0.0 = identical, 1.0 = completely different).
    pub divergence_score: f64,
    /// Which quality dimensions changed (if any).
    pub dimension_changes: HashMap<String, f64>,
}

/// Summary of a shadow observation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowSummary {
    /// Total comparisons made.
    pub total_comparisons: u32,
    /// How many responses diverged materially.
    pub diverged_count: u32,
    /// Average divergence score across all comparisons.
    pub avg_divergence: f64,
    /// Per-dimension average changes.
    pub dimension_averages: HashMap<String, f64>,
    /// Recommendation based on shadow results.
    pub recommendation: ShadowRecommendation,
}

/// Recommendation from shadow-mode analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShadowRecommendation {
    /// Shadow version performs similarly — safe to deploy.
    SafeToDeploy,
    /// Shadow version shows improvement — recommend deployment.
    RecommendDeploy,
    /// Shadow version shows regression — do not deploy.
    DoNotDeploy,
    /// Insufficient data — extend observation.
    NeedMoreData,
}

impl ShadowSession {
    /// Create a new shadow observation session.
    pub fn new(
        agent_id: String,
        version_id: String,
        production_soul: String,
        shadow_soul: String,
    ) -> Self {
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
            agent_id,
            version_id,
            started_at: Utc::now(),
            production_soul,
            shadow_soul,
            comparisons: Vec::new(),
            active: true,
        }
    }

    /// Record a comparison result.
    pub fn record_comparison(&mut self, comparison: ShadowComparison) {
        self.comparisons.push(comparison);
    }

    /// Generate a summary of the shadow session.
    pub fn summarize(&self) -> ShadowSummary {
        let total = self.comparisons.len() as u32;

        if total < 5 {
            return ShadowSummary {
                total_comparisons: total,
                diverged_count: 0,
                avg_divergence: 0.0,
                dimension_averages: HashMap::new(),
                recommendation: ShadowRecommendation::NeedMoreData,
            };
        }

        let diverged_count = self.comparisons.iter()
            .filter(|c| c.response_diverged)
            .count() as u32;

        let avg_divergence = self.comparisons.iter()
            .map(|c| c.divergence_score)
            .sum::<f64>() / total as f64;

        // Compute per-dimension averages
        let mut dim_sums: HashMap<String, (f64, u32)> = HashMap::new();
        for comp in &self.comparisons {
            for (dim, val) in &comp.dimension_changes {
                let entry = dim_sums.entry(dim.clone()).or_insert((0.0, 0));
                entry.0 += val;
                entry.1 += 1;
            }
        }
        let dimension_averages: HashMap<String, f64> = dim_sums.into_iter()
            .map(|(k, (sum, count))| (k, sum / count as f64))
            .collect();

        // Determine recommendation
        let recommendation = if avg_divergence < 0.1 {
            ShadowRecommendation::SafeToDeploy
        } else if avg_divergence < 0.3 {
            // Check if changes are positive
            let net_positive = dimension_averages.values()
                .filter(|&&v| v > 0.0).count();
            let net_negative = dimension_averages.values()
                .filter(|&&v| v < -0.05).count();

            if net_positive > net_negative {
                ShadowRecommendation::RecommendDeploy
            } else {
                ShadowRecommendation::SafeToDeploy
            }
        } else {
            // High divergence — check if regression
            let any_significant_regression = dimension_averages.values()
                .any(|&v| v < -0.1);
            if any_significant_regression {
                ShadowRecommendation::DoNotDeploy
            } else {
                ShadowRecommendation::RecommendDeploy
            }
        };

        ShadowSummary {
            total_comparisons: total,
            diverged_count,
            avg_divergence,
            dimension_averages,
            recommendation,
        }
    }

    /// Close the shadow session.
    pub fn close(&mut self) {
        self.active = false;
    }
}

/// Compute divergence score between two response texts.
///
/// Uses character-bigram Jaccard distance as a lightweight proxy.
pub fn compute_divergence(response_a: &str, response_b: &str) -> f64 {
    use std::collections::HashSet;

    fn bigrams(text: &str) -> HashSet<String> {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        chars.windows(2).map(|w| w.iter().collect::<String>()).collect()
    }

    let a_bg = bigrams(response_a);
    let b_bg = bigrams(response_b);

    if a_bg.is_empty() && b_bg.is_empty() {
        return 0.0;
    }

    let inter = a_bg.intersection(&b_bg).count() as f64;
    let union = a_bg.union(&b_bg).count() as f64;

    if union == 0.0 { 0.0 } else { 1.0 - (inter / union) }
}
