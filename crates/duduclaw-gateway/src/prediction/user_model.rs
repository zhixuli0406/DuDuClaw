//! Per-user statistical model for prediction-error-driven evolution.
//!
//! Uses Welford's online algorithm for numerically stable running statistics.
//! Each (user_id, agent_id) pair has its own model that adapts over time.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// RunningStats — Welford's online algorithm
// ---------------------------------------------------------------------------

/// Numerically stable online mean + variance using Welford's algorithm.
///
/// Suitable for streaming updates without storing all past values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningStats {
    count: u64,
    mean: f64,
    m2: f64,
}

impl Default for RunningStats {
    fn default() -> Self {
        Self { count: 0, mean: 0.0, m2: 0.0 }
    }
}

impl RunningStats {
    /// Push a new observation.
    pub fn push(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
    }

    /// Current mean (0.0 if empty).
    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// Population variance (0.0 if fewer than 2 samples).
    pub fn variance(&self) -> f64 {
        if self.count < 2 {
            0.0
        } else {
            self.m2 / self.count as f64
        }
    }

    /// Population standard deviation.
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    /// Number of observations recorded.
    pub fn sample_count(&self) -> u64 {
        self.count
    }
}

// ---------------------------------------------------------------------------
// LanguageStats
// ---------------------------------------------------------------------------

/// Tracks language distribution across conversations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageStats {
    pub primary_language: String,
    pub distribution: HashMap<String, u64>,
    total: u64,
}

impl Default for LanguageStats {
    fn default() -> Self {
        Self {
            primary_language: "unknown".to_string(),
            distribution: HashMap::new(),
            total: 0,
        }
    }
}

impl LanguageStats {
    /// Record a detected language from a conversation.
    pub fn update(&mut self, detected_lang: &str) {
        *self.distribution.entry(detected_lang.to_string()).or_insert(0) += 1;
        self.total += 1;

        // Recalculate primary language
        if let Some((lang, _)) = self.distribution.iter().max_by_key(|(_, c)| **c) {
            self.primary_language = lang.clone();
        }
    }
}

// ---------------------------------------------------------------------------
// UserModel
// ---------------------------------------------------------------------------

/// Statistical model for a specific (user, agent) pair.
///
/// Captures behavioural patterns to predict future interactions,
/// enabling the prediction-error-driven evolution engine to detect
/// when reality diverges from expectations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserModel {
    pub user_id: String,
    pub agent_id: String,

    /// Average length of assistant responses the user seems to prefer.
    pub preferred_response_length: RunningStats,

    /// User satisfaction score (0.0 = very dissatisfied, 1.0 = very satisfied).
    /// Derived from explicit feedback signals and implicit behavioural cues.
    pub avg_satisfaction: RunningStats,

    /// Keyword frequency distribution (simple TF proxy).
    pub topic_distribution: HashMap<String, f64>,

    /// Per-hour activity probability (index 0 = midnight, 23 = 11pm).
    pub active_hours: [f64; 24],

    /// Rate at which the user corrects the agent.
    pub correction_rate: RunningStats,

    /// Rate at which the user sends follow-up questions (potential dissatisfaction signal).
    pub follow_up_rate: RunningStats,

    /// Language preference tracking.
    pub language_preference: LanguageStats,

    /// Total conversations tracked.
    pub total_conversations: u64,

    /// Last time this model was updated.
    pub last_updated: DateTime<Utc>,

    // ── Proactive need prediction (Phase D) ─────────────────────

    /// Predicted next conversation topic (most frequent recent topic).
    #[serde(default)]
    pub predicted_next_topic: Option<String>,

    /// Predicted hours until user returns (based on active_hours pattern).
    #[serde(default)]
    pub predicted_return_hours: Option<f32>,

    /// Proactive receptivity score (0.0-1.0) — how likely the user is to
    /// welcome proactive messages. Updated by accept/dismiss feedback.
    #[serde(default = "default_receptivity")]
    pub proactive_receptivity: f64,

    /// Number of proactive messages accepted by this user.
    #[serde(default)]
    pub proactive_accepted: u32,

    /// Number of proactive messages dismissed/ignored by this user.
    #[serde(default)]
    pub proactive_dismissed: u32,

    // ── Embedding-based topic tracking (Hardening 2025-Q2) ────

    /// Rolling window of conversation embedding vectors with timestamps.
    /// Each entry: (embedding, unix_timestamp_secs).
    /// Used for semantic topic_surprise computation via cosine similarity.
    ///
    /// Skipped from JSON serialization to avoid bloating model_json column.
    /// 100 x 512-dim = ~400KB in JSON — too large for debounced persistence.
    /// Embeddings re-accumulate from conversations after restart (acceptable
    /// cold-start behavior, similar to user_text in ConversationMetrics).
    #[serde(skip)]
    pub topic_embeddings: std::collections::VecDeque<(Vec<f32>, f64)>,

    /// Historical character bigrams for vocabulary_novelty fallback.
    /// Stored as a HashSet for O(1) lookup during novelty computation.
    /// Capped by evicting oldest entries via a companion insertion-order ring.
    ///
    /// Skipped from serialization to avoid bloating model_json (audit #11):
    /// 10,000 bigrams × ~6 bytes = ~60KB per user. Re-accumulates from conversations.
    #[serde(skip)]
    pub historical_bigrams: std::collections::HashSet<String>,

    /// Insertion-order ring for historical_bigrams eviction.
    /// When cap is reached, pop_front to remove the oldest bigram.
    #[serde(skip)]
    pub bigram_insertion_order: std::collections::VecDeque<String>,
}

fn default_receptivity() -> f64 {
    0.5 // Neutral starting point
}

impl UserModel {
    /// Create a new model with cold-start defaults.
    pub fn new(user_id: String, agent_id: String) -> Self {
        Self {
            user_id,
            agent_id,
            preferred_response_length: RunningStats::default(),
            avg_satisfaction: RunningStats::default(),
            topic_distribution: HashMap::new(),
            active_hours: [0.0; 24],
            correction_rate: RunningStats::default(),
            follow_up_rate: RunningStats::default(),
            language_preference: LanguageStats::default(),
            total_conversations: 0,
            last_updated: Utc::now(),
            predicted_next_topic: None,
            predicted_return_hours: None,
            proactive_receptivity: 0.5,
            proactive_accepted: 0,
            proactive_dismissed: 0,
            topic_embeddings: std::collections::VecDeque::new(),
            historical_bigrams: std::collections::HashSet::new(),
            bigram_insertion_order: std::collections::VecDeque::new(),
        }
    }

    /// Update from conversation metrics extracted after a completed conversation.
    pub fn update_from_metrics(&mut self, metrics: &super::metrics::ConversationMetrics) {
        self.preferred_response_length.push(metrics.avg_assistant_response_length);

        // Follow-up rate: ratio of follow-up messages to total exchanges
        let exchanges = metrics.assistant_message_count.max(1) as f64;
        let follow_up_ratio = metrics.user_follow_ups as f64 / exchanges;
        self.follow_up_rate.push(follow_up_ratio);

        // Correction rate: ratio of corrections to total user messages
        let user_msgs = metrics.user_message_count.max(1) as f64;
        let correction_ratio = metrics.user_corrections as f64 / user_msgs;
        self.correction_rate.push(correction_ratio);

        // Update topic distribution with extracted keywords (capped at 200 entries)
        for keyword in &metrics.extracted_topics {
            *self.topic_distribution.entry(keyword.clone()).or_insert(0.0) += 1.0;
        }
        // Evict lowest-frequency topics when cap exceeded (audit #2: unbounded growth)
        const TOPIC_CAP: usize = 200;
        while self.topic_distribution.len() > TOPIC_CAP {
            if let Some(min_key) = self.topic_distribution.iter()
                .min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(k, _)| k.clone())
            {
                self.topic_distribution.remove(&min_key);
            } else {
                break;
            }
        }

        // Update active hours
        let hour = metrics.timestamp.hour() as usize;
        if hour < 24 {
            self.active_hours[hour] += 1.0;
        }

        // Update language preference
        if !metrics.detected_language.is_empty() {
            self.language_preference.update(&metrics.detected_language);
        }

        // Update historical bigrams for vocabulary_novelty fallback.
        // Uses insertion-order VecDeque for FIFO eviction when cap is reached.
        if !metrics.user_text.is_empty() {
            let chars: Vec<char> = metrics.user_text.chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            for w in chars.windows(2) {
                let bigram: String = w.iter().collect();
                if self.historical_bigrams.insert(bigram.clone()) {
                    // Only track insertion order for newly added bigrams
                    self.bigram_insertion_order.push_back(bigram);
                }
            }
            // FIFO eviction: remove oldest bigrams when cap exceeded
            const BIGRAM_CAP: usize = 10_000;
            while self.historical_bigrams.len() > BIGRAM_CAP {
                if let Some(oldest) = self.bigram_insertion_order.pop_front() {
                    self.historical_bigrams.remove(&oldest);
                } else {
                    break; // Safety: should never happen
                }
            }
        }

        self.total_conversations += 1;
        self.last_updated = Utc::now();
    }

    /// Store an embedding vector for this conversation.
    ///
    /// Called by `PredictionEngine::update_model` when an embedding provider
    /// is available. The embedding is stored with a timestamp for time-weighted
    /// cosine similarity computation.
    pub fn update_embedding(&mut self, embedding: Vec<f32>, max_history: usize) {
        let now = Utc::now().timestamp() as f64;
        self.topic_embeddings.push_back((embedding, now));
        while self.topic_embeddings.len() > max_history {
            self.topic_embeddings.pop_front();
        }
    }

    /// Update satisfaction from explicit user feedback.
    pub fn update_from_feedback(&mut self, signal_type: &str) {
        let score = match signal_type {
            "positive" => 1.0,
            "negative" => 0.0,
            "correction" => 0.2,
            _ => 0.5,
        };
        self.avg_satisfaction.push(score);

        if signal_type == "correction" {
            // Boost the correction rate stat with a strong signal
            self.correction_rate.push(1.0);
        }

        self.last_updated = Utc::now();
    }

    /// Confidence level based on data richness (0.0 = cold start, 1.0 = mature).
    pub fn confidence(&self) -> f64 {
        (self.total_conversations.min(50) as f64) / 50.0
    }
}

use chrono::Timelike;
