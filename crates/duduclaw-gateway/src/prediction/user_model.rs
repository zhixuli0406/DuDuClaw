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

        // Update topic distribution with extracted keywords
        for keyword in &metrics.extracted_topics {
            *self.topic_distribution.entry(keyword.clone()).or_insert(0.0) += 1.0;
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

        self.total_conversations += 1;
        self.last_updated = Utc::now();
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
