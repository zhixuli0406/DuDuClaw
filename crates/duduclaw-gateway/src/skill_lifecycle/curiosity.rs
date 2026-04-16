//! Curiosity engine — proactive exploration of underexplored domains.
//!
//! Analyzes the agent's topic coverage map to identify "frontier" topics:
//! low occurrence but high importance. When curiosity score exceeds a threshold,
//! triggers exploration actions (skill search, wiki expansion, SOUL.md guidance).
//!
//! References:
//! - OMNI-EPIC (ICLR 2025): interest-driven open-ended learning
//! - Active Inference (Friston): epistemic foraging
//! - Parr, Pezzulo & Friston 2024: surprise-driven exploration

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Topic Coverage Map
// ---------------------------------------------------------------------------

/// Coverage data for a single topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicCoverage {
    /// Topic string (normalized).
    pub topic: String,
    /// Number of conversations mentioning this topic.
    pub occurrence_count: u32,
    /// Running average importance score.
    pub avg_importance: f64,
    /// Running average prediction error for this topic.
    pub avg_prediction_error: f64,
    /// Last time this topic was encountered.
    pub last_seen: DateTime<Utc>,
    /// Number of existing skills matching this topic.
    pub skill_count: u32,
    /// Number of wiki pages matching this topic.
    pub wiki_page_count: u32,
    // Internal: running totals for average calculation (not serialized)
    #[serde(skip)]
    total_importance: f64,
    #[serde(skip)]
    total_error: f64,
}

impl TopicCoverage {
    fn new(topic: String, importance: f64, error: f64) -> Self {
        Self {
            topic,
            occurrence_count: 1,
            avg_importance: importance,
            avg_prediction_error: error,
            last_seen: Utc::now(),
            skill_count: 0,
            wiki_page_count: 0,
            total_importance: importance,
            total_error: error,
        }
    }

    fn update(&mut self, importance: f64, error: f64) {
        self.occurrence_count += 1;
        self.last_seen = Utc::now();
        self.total_importance += importance;
        self.total_error += error;
        self.avg_importance = self.total_importance / self.occurrence_count as f64;
        self.avg_prediction_error = self.total_error / self.occurrence_count as f64;
    }
}

/// Map of all topics encountered by an agent.
pub struct TopicCoverageMap {
    topics: HashMap<String, TopicCoverage>,
}

impl Default for TopicCoverageMap {
    fn default() -> Self {
        Self { topics: HashMap::new() }
    }
}

impl TopicCoverageMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the map with topics from a conversation.
    pub fn update_from_conversation(
        &mut self,
        topics: &[String],
        importance: f64,
        prediction_error: f64,
    ) {
        for topic in topics {
            let normalized = topic.to_lowercase().trim().to_string();
            if normalized.is_empty() {
                continue;
            }

            self.topics
                .entry(normalized.clone())
                .and_modify(|c| c.update(importance, prediction_error))
                .or_insert_with(|| TopicCoverage::new(normalized, importance, prediction_error));
        }
    }

    /// Update coverage counts for skills and wiki pages.
    pub fn update_coverage_counts(
        &mut self,
        skill_topics: &HashMap<String, u32>,
        wiki_topics: &HashMap<String, u32>,
    ) {
        for coverage in self.topics.values_mut() {
            coverage.skill_count = *skill_topics.get(&coverage.topic).unwrap_or(&0);
            coverage.wiki_page_count = *wiki_topics.get(&coverage.topic).unwrap_or(&0);
        }
    }

    /// Identify frontier topics — high potential, low coverage.
    pub fn get_frontiers(&self, limit: usize) -> Vec<TopicFrontier> {
        let mut frontiers: Vec<TopicFrontier> = self
            .topics
            .values()
            .filter(|c| {
                c.occurrence_count <= 5
                    && c.avg_importance >= 0.3
                    && c.skill_count == 0
            })
            .map(|c| {
                let novelty = 1.0 / (c.occurrence_count as f64 + 1.0).ln_1p();
                let engagement = c.avg_importance;
                // coverage_gap: 1.0 when no resources exist, 0.0 when ≥ MAX_EXPECTED resources cover the topic.
                const MAX_EXPECTED_RESOURCES: f64 = 3.0;
                let total_resources = c.skill_count as f64 + c.wiki_page_count as f64;
                let coverage_gap = (1.0 - total_resources / MAX_EXPECTED_RESOURCES).max(0.0);
                let curiosity_score = novelty * engagement * coverage_gap;

                TopicFrontier {
                    topic: c.topic.clone(),
                    curiosity_score,
                    novelty,
                    engagement,
                    coverage_gap,
                    occurrence_count: c.occurrence_count,
                    avg_error: c.avg_prediction_error,
                }
            })
            .filter(|f| f.curiosity_score > 0.0)
            .collect();

        frontiers.sort_by(|a, b| {
            b.curiosity_score
                .partial_cmp(&a.curiosity_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        frontiers.truncate(limit);
        frontiers
    }

    /// Total number of tracked topics.
    pub fn topic_count(&self) -> usize {
        self.topics.len()
    }
}

/// A frontier topic with curiosity scoring.
#[derive(Debug, Clone)]
pub struct TopicFrontier {
    pub topic: String,
    pub curiosity_score: f64,
    pub novelty: f64,
    pub engagement: f64,
    pub coverage_gap: f64,
    pub occurrence_count: u32,
    pub avg_error: f64,
}

// ---------------------------------------------------------------------------
// Curiosity Engine
// ---------------------------------------------------------------------------

/// Actions the curiosity engine can recommend.
#[derive(Debug, Clone)]
pub enum ExplorationAction {
    /// Search the skill marketplace for this topic.
    SearchSkills { query: String, topic: String },
    /// Suggest creating wiki pages about this topic.
    ExpandWiki { topic: String },
    /// Guide SOUL.md evolution toward this domain.
    GuideSoulEvolution { topic: String, curiosity_score: f64 },
}

/// Curiosity engine that drives proactive exploration.
pub struct CuriosityEngine {
    coverage_map: TopicCoverageMap,
    curiosity_threshold: f64,
    max_explorations_per_day: u32,
    explorations_today: u32,
    last_reset: DateTime<Utc>,
}

impl CuriosityEngine {
    pub fn new(curiosity_threshold: f64, max_daily: u32) -> Self {
        Self {
            coverage_map: TopicCoverageMap::new(),
            curiosity_threshold,
            max_explorations_per_day: max_daily,
            explorations_today: 0,
            last_reset: Utc::now(),
        }
    }

    /// Update coverage from a conversation.
    pub fn record_conversation(
        &mut self,
        topics: &[String],
        importance: f64,
        prediction_error: f64,
    ) {
        self.coverage_map
            .update_from_conversation(topics, importance, prediction_error);
    }

    /// Update coverage counts from external sources.
    pub fn update_coverage(
        &mut self,
        skill_topics: &HashMap<String, u32>,
        wiki_topics: &HashMap<String, u32>,
    ) {
        self.coverage_map
            .update_coverage_counts(skill_topics, wiki_topics);
    }

    /// Evaluate and return exploration actions if any frontiers are found.
    pub fn evaluate(&mut self) -> Vec<ExplorationAction> {
        self.maybe_reset_daily();

        if self.explorations_today >= self.max_explorations_per_day {
            debug!(
                used = self.explorations_today,
                max = self.max_explorations_per_day,
                "Curiosity budget exhausted for today"
            );
            return Vec::new();
        }

        let remaining = self.max_explorations_per_day - self.explorations_today;
        let frontiers = self.coverage_map.get_frontiers(remaining as usize);

        let mut actions = Vec::new();
        for frontier in &frontiers {
            if frontier.curiosity_score < self.curiosity_threshold {
                continue;
            }

            let action = if frontier.coverage_gap >= 0.8 {
                // Very low coverage — search for skills AND guide evolution
                ExplorationAction::GuideSoulEvolution {
                    topic: frontier.topic.clone(),
                    curiosity_score: frontier.curiosity_score,
                }
            } else if frontier.coverage_gap >= 0.5 {
                // Moderate gap — search for skills
                ExplorationAction::SearchSkills {
                    query: frontier.topic.clone(),
                    topic: frontier.topic.clone(),
                }
            } else {
                // Small gap — wiki expansion
                ExplorationAction::ExpandWiki {
                    topic: frontier.topic.clone(),
                }
            };

            self.explorations_today += 1;
            actions.push(action);

            info!(
                topic = %frontier.topic,
                score = %format!("{:.3}", frontier.curiosity_score),
                "Curiosity exploration triggered"
            );
        }

        actions
    }

    /// Get the coverage map (for telemetry).
    pub fn coverage_map(&self) -> &TopicCoverageMap {
        &self.coverage_map
    }

    /// Get today's exploration count.
    pub fn explorations_today(&self) -> u32 {
        self.explorations_today
    }

    fn maybe_reset_daily(&mut self) {
        let now = Utc::now();
        if now.date_naive() != self.last_reset.date_naive() {
            self.explorations_today = 0;
            self.last_reset = now;
            debug!("Daily curiosity budget reset");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frontier_detection() {
        let mut map = TopicCoverageMap::new();

        // Low occurrence, high importance → frontier
        map.update_from_conversation(&["rare-topic".to_string()], 0.8, 0.6);
        map.update_from_conversation(&["rare-topic".to_string()], 0.7, 0.5);

        // High occurrence → not frontier
        for _ in 0..20 {
            map.update_from_conversation(&["common-topic".to_string()], 0.5, 0.3);
        }

        let frontiers = map.get_frontiers(5);
        assert_eq!(frontiers.len(), 1);
        assert_eq!(frontiers[0].topic, "rare-topic");
    }

    #[test]
    fn test_well_covered_not_frontier() {
        let mut map = TopicCoverageMap::new();
        map.update_from_conversation(&["covered".to_string()], 0.8, 0.5);

        // Add coverage
        let mut skill_topics = HashMap::new();
        skill_topics.insert("covered".to_string(), 2);
        map.update_coverage_counts(&skill_topics, &HashMap::new());

        let frontiers = map.get_frontiers(5);
        assert!(frontiers.is_empty()); // has skills → not frontier
    }

    #[test]
    fn test_curiosity_score_calculation() {
        let mut map = TopicCoverageMap::new();
        map.update_from_conversation(&["novel".to_string()], 0.9, 0.7);

        let frontiers = map.get_frontiers(5);
        assert!(!frontiers.is_empty());
        let f = &frontiers[0];
        assert!(f.curiosity_score > 0.0);
        assert!(f.novelty > 0.0);
        assert_eq!(f.engagement, 0.9);
    }

    #[test]
    fn test_budget_limits_explorations() {
        let mut engine = CuriosityEngine::new(0.01, 2); // very low threshold, max 2/day

        // Add 5 frontier topics
        for i in 0..5 {
            engine.record_conversation(
                &[format!("topic-{i}")],
                0.9,
                0.7,
            );
        }

        let actions = engine.evaluate();
        assert!(actions.len() <= 2); // budget limited
        assert_eq!(engine.explorations_today(), actions.len() as u32);
    }

    #[test]
    fn test_coverage_map_update() {
        let mut map = TopicCoverageMap::new();
        map.update_from_conversation(&["test".to_string()], 0.5, 0.3);
        map.update_from_conversation(&["test".to_string()], 0.7, 0.5);

        assert_eq!(map.topic_count(), 1);
        let coverage = map.topics.get("test").unwrap();
        assert_eq!(coverage.occurrence_count, 2);
        assert!((coverage.avg_importance - 0.6).abs() < 0.01);
    }

    #[test]
    fn test_low_importance_not_frontier() {
        let mut map = TopicCoverageMap::new();
        // Low importance → not frontier even with low occurrence
        map.update_from_conversation(&["boring".to_string()], 0.1, 0.1);

        let frontiers = map.get_frontiers(5);
        assert!(frontiers.is_empty());
    }
}
