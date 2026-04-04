//! Diversity metrics — tracks proposal variety and response diversity over time.
//!
//! Prevents mode collapse by monitoring whether GVU proposals and agent responses
//! are becoming increasingly homogeneous.
//!
//! Based on:
//! - Guo et al. (2024) "Curious Decline of Linguistic Diversity"
//! - Padmakumar et al. (ICLR 2024) "Does Writing with LMs Reduce Content Diversity?"
//! - Quality-Diversity algorithms (MAP-Elites, QDAIF)

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Tracks diversity of GVU proposals and agent responses over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiversityTracker {
    /// Rolling window of proposal fingerprints (character bigram sets).
    proposal_fingerprints: VecDeque<ProposalFingerprint>,
    /// Rolling window of response diversity samples.
    response_samples: VecDeque<ResponseSample>,
    /// Maximum window size.
    window_size: usize,
    /// Historical diversity scores for trend detection.
    diversity_history: VecDeque<DiversitySnapshot>,
}

/// Fingerprint of a GVU proposal for diversity comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProposalFingerprint {
    /// Character bigrams from the proposal content.
    bigrams: HashSet<String>,
    /// Keywords from the rationale.
    rationale_keywords: HashSet<String>,
    /// When this proposal was made.
    timestamp: DateTime<Utc>,
}

/// A sample of agent response for diversity measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseSample {
    /// Unique bigrams in the response.
    unique_bigrams: usize,
    /// Total bigrams in the response.
    total_bigrams: usize,
    /// Response length in characters.
    char_count: usize,
    /// Timestamp.
    timestamp: DateTime<Utc>,
}

/// A snapshot of diversity metrics at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiversitySnapshot {
    /// When this snapshot was taken.
    pub timestamp: DateTime<Utc>,
    /// Proposal diversity (0.0 = identical proposals, 1.0 = maximally diverse).
    pub proposal_diversity: f64,
    /// Response vocabulary diversity (unique bigrams / total bigrams).
    pub response_vocabulary_diversity: f64,
    /// Response length variance (high variance = diverse response styles).
    pub response_length_variance: f64,
}

/// Overall diversity health assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiversityHealth {
    /// Diversity is within healthy range.
    Healthy,
    /// Diversity is declining — early warning.
    Declining,
    /// Diversity has collapsed — mode collapse likely.
    Collapsed,
}

impl Default for DiversityTracker {
    fn default() -> Self {
        Self {
            proposal_fingerprints: VecDeque::new(),
            response_samples: VecDeque::new(),
            window_size: 50,
            diversity_history: VecDeque::new(),
        }
    }
}

impl DiversityTracker {
    /// Record a GVU proposal for diversity tracking.
    pub fn record_proposal(&mut self, content: &str, rationale: &str) {
        let bigrams = Self::extract_bigrams(content);
        let rationale_keywords = Self::extract_keywords(rationale);

        self.proposal_fingerprints.push_back(ProposalFingerprint {
            bigrams,
            rationale_keywords,
            timestamp: Utc::now(),
        });

        while self.proposal_fingerprints.len() > self.window_size {
            self.proposal_fingerprints.pop_front();
        }
    }

    /// Record an agent response for diversity tracking.
    pub fn record_response(&mut self, response: &str) {
        let bigrams = Self::extract_bigrams(response);
        let unique_count = bigrams.len();
        let chars: Vec<char> = response.chars().collect();
        let total_bigrams = if chars.len() > 1 { chars.len() - 1 } else { 1 };

        self.response_samples.push_back(ResponseSample {
            unique_bigrams: unique_count,
            total_bigrams,
            char_count: chars.len(),
            timestamp: Utc::now(),
        });

        while self.response_samples.len() > self.window_size {
            self.response_samples.pop_front();
        }
    }

    /// Compute current diversity snapshot.
    pub fn snapshot(&mut self) -> DiversitySnapshot {
        let proposal_diversity = self.compute_proposal_diversity();
        let response_vocabulary_diversity = self.compute_vocabulary_diversity();
        let response_length_variance = self.compute_length_variance();

        let snap = DiversitySnapshot {
            timestamp: Utc::now(),
            proposal_diversity,
            response_vocabulary_diversity,
            response_length_variance,
        };

        self.diversity_history.push_back(snap.clone());
        while self.diversity_history.len() > 100 {
            self.diversity_history.pop_front();
        }

        snap
    }

    /// Assess overall diversity health.
    pub fn health(&self) -> DiversityHealth {
        if self.diversity_history.len() < 10 {
            return DiversityHealth::Healthy; // Not enough data
        }

        // Compare recent diversity to historical
        let recent: Vec<&DiversitySnapshot> = self.diversity_history.iter().rev().take(10).collect();
        let older: Vec<&DiversitySnapshot> = self.diversity_history.iter().rev().skip(10).take(10).collect();

        if older.is_empty() {
            return DiversityHealth::Healthy;
        }

        let recent_avg = recent.iter().map(|s| s.proposal_diversity).sum::<f64>() / recent.len() as f64;
        let older_avg = older.iter().map(|s| s.proposal_diversity).sum::<f64>() / older.len() as f64;

        let vocab_recent = recent.iter().map(|s| s.response_vocabulary_diversity).sum::<f64>() / recent.len() as f64;
        let vocab_older = older.iter().map(|s| s.response_vocabulary_diversity).sum::<f64>() / older.len() as f64;

        // Check for decline
        let proposal_declining = recent_avg < older_avg * 0.7;
        let vocab_declining = vocab_recent < vocab_older * 0.7;

        if proposal_declining && vocab_declining {
            DiversityHealth::Collapsed
        } else if proposal_declining || vocab_declining {
            DiversityHealth::Declining
        } else {
            DiversityHealth::Healthy
        }
    }

    /// Check if a new proposal is too similar to recent proposals (deduplication).
    ///
    /// Returns the overlap ratio with the most similar recent proposal.
    pub fn proposal_novelty(&self, content: &str) -> f64 {
        let new_bigrams = Self::extract_bigrams(content);
        if new_bigrams.is_empty() {
            return 1.0;
        }

        let max_overlap = self.proposal_fingerprints.iter().map(|fp| {
            let inter = new_bigrams.intersection(&fp.bigrams).count() as f64;
            let union = new_bigrams.union(&fp.bigrams).count() as f64;
            if union == 0.0 { 0.0 } else { inter / union }
        }).fold(0.0_f64, f64::max);

        1.0 - max_overlap // novelty = 1 - similarity
    }

    // --- Internal helpers ---

    fn compute_proposal_diversity(&self) -> f64 {
        if self.proposal_fingerprints.len() < 2 {
            return 1.0;
        }

        // Average pairwise Jaccard distance between recent proposals
        let fps: Vec<&ProposalFingerprint> = self.proposal_fingerprints.iter().collect();
        let mut total_distance = 0.0;
        let mut pairs = 0u32;

        for i in 0..fps.len() {
            for j in (i + 1)..fps.len() {
                let inter = fps[i].bigrams.intersection(&fps[j].bigrams).count() as f64;
                let union = fps[i].bigrams.union(&fps[j].bigrams).count() as f64;
                let similarity = if union == 0.0 { 0.0 } else { inter / union };
                total_distance += 1.0 - similarity;
                pairs += 1;
            }
        }

        if pairs == 0 { 1.0 } else { total_distance / pairs as f64 }
    }

    fn compute_vocabulary_diversity(&self) -> f64 {
        if self.response_samples.is_empty() {
            return 1.0;
        }

        let avg_ratio = self.response_samples.iter()
            .map(|s| s.unique_bigrams as f64 / s.total_bigrams.max(1) as f64)
            .sum::<f64>() / self.response_samples.len() as f64;

        avg_ratio.clamp(0.0, 1.0)
    }

    fn compute_length_variance(&self) -> f64 {
        if self.response_samples.len() < 2 {
            return 0.0;
        }

        let mean = self.response_samples.iter()
            .map(|s| s.char_count as f64)
            .sum::<f64>() / self.response_samples.len() as f64;

        let variance = self.response_samples.iter()
            .map(|s| (s.char_count as f64 - mean).powi(2))
            .sum::<f64>() / self.response_samples.len() as f64;

        variance.sqrt() // Standard deviation
    }

    fn extract_bigrams(text: &str) -> HashSet<String> {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        chars.windows(2).map(|w| w.iter().collect::<String>()).collect()
    }

    fn extract_keywords(text: &str) -> HashSet<String> {
        text.split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|w| w.to_lowercase())
            .collect()
    }
}
