//! Skill lift tracker — measures the causal effect of each skill
//! by comparing prediction errors when the skill is active vs inactive.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::prediction::user_model::RunningStats;

/// Tracks the A/B lift effect of a single skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillLiftTracker {
    pub skill_name: String,
    pub agent_id: String,
    /// Prediction errors when this skill was active.
    pub errors_with: RunningStats,
    /// Prediction errors when this skill was NOT active.
    pub errors_without: RunningStats,
    /// Total times this skill was loaded into the prompt.
    pub load_count: u64,
    /// When this skill was first activated.
    pub first_activated: DateTime<Utc>,
}

impl SkillLiftTracker {
    pub fn new(skill_name: String, agent_id: String) -> Self {
        Self {
            skill_name,
            agent_id,
            errors_with: RunningStats::default(),
            errors_without: RunningStats::default(),
            load_count: 0,
            first_activated: Utc::now(),
        }
    }

    /// Record a conversation where this skill WAS active.
    pub fn record_with(&mut self, prediction_error: f64) {
        self.errors_with.push(prediction_error);
        self.load_count += 1;
    }

    /// Record a conversation where this skill was NOT active.
    pub fn record_without(&mut self, prediction_error: f64) {
        self.errors_without.push(prediction_error);
    }

    /// Calculate the lift: positive = skill helps reduce errors.
    ///
    /// `lift = errors_without.mean() - errors_with.mean()`
    pub fn lift(&self) -> f64 {
        if self.errors_with.sample_count() < 10 || self.errors_without.sample_count() < 10 {
            return 0.0; // insufficient data
        }
        self.errors_without.mean() - self.errors_with.mean()
    }

    /// Whether the skill's effect is stable (low variance in recent errors).
    pub fn is_stable(&self) -> bool {
        self.errors_with.sample_count() >= 20 && self.errors_with.std_dev() < 0.1
    }

    /// Whether the skill has enough usage data for distillation consideration.
    pub fn is_mature(&self) -> bool {
        self.load_count >= 50
            && self.errors_with.sample_count() >= 10
            && self.errors_without.sample_count() >= 10
    }
}

/// Collection of lift trackers for all skills of an agent.
pub struct LiftTrackerStore {
    trackers: HashMap<(String, String), SkillLiftTracker>,
}

impl LiftTrackerStore {
    pub fn new() -> Self {
        Self { trackers: HashMap::new() }
    }

    /// Get or create a tracker for a skill.
    pub fn get_or_create(&mut self, agent_id: &str, skill_name: &str) -> &mut SkillLiftTracker {
        let key = (agent_id.to_string(), skill_name.to_string());
        self.trackers.entry(key).or_insert_with(|| {
            SkillLiftTracker::new(skill_name.to_string(), agent_id.to_string())
        })
    }

    /// Get all trackers for an agent.
    pub fn get_all(&self, agent_id: &str) -> Vec<&SkillLiftTracker> {
        self.trackers
            .iter()
            .filter(|((aid, _), _)| aid == agent_id)
            .map(|(_, t)| t)
            .collect()
    }
}
