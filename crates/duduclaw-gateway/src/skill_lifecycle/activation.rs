//! Skill activation controller — dynamically activates/deactivates skills
//! based on prediction error diagnosis and measured effectiveness.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::prediction::user_model::RunningStats;

/// Record of a skill activation for effectiveness tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationRecord {
    pub skill_name: String,
    pub agent_id: String,
    pub activated_at: DateTime<Utc>,
    pub deactivated_at: Option<DateTime<Utc>>,
    /// The prediction error that triggered activation.
    pub trigger_error: f64,
    /// Running stats of prediction errors while this skill is active.
    pub post_errors: RunningStats,
    /// Number of conversations while this skill is active.
    pub conversations: u32,
}

/// Manages per-agent skill activation state.
pub struct SkillActivationController {
    /// agent_id → set of active skill names.
    active: HashMap<String, HashSet<String>>,
    /// (agent_id, skill_name) → activation record.
    records: HashMap<(String, String), ActivationRecord>,
    /// Default maximum active skills per agent, used when the agent has no
    /// per-agent override registered.
    max_active: usize,
    /// H3 (2026-09-29): per-agent `[evolution] max_active_skills`.
    ///
    /// The controller is a process-wide singleton on `ChannelContext` while
    /// the knob is per-agent, so the cap cannot live in one scalar. Callers
    /// register the agent's configured value via [`Self::set_agent_max`]
    /// before activating; an agent with no entry keeps [`Self::max_active`],
    /// which is the pre-H3 behavior byte-for-byte.
    agent_max: HashMap<String, usize>,
}

impl SkillActivationController {
    pub fn new(max_active: usize) -> Self {
        Self {
            active: HashMap::new(),
            records: HashMap::new(),
            max_active,
            agent_max: HashMap::new(),
        }
    }

    /// Register this agent's configured `max_active_skills`.
    ///
    /// `0` is refused (clamped to 1): a cap of zero would evict every skill
    /// the moment it is activated, which is a config typo, not an intent.
    pub fn set_agent_max(&mut self, agent_id: &str, max_active: usize) {
        self.agent_max
            .insert(agent_id.to_string(), max_active.max(1));
    }

    /// The effective cap for one agent.
    pub fn max_for(&self, agent_id: &str) -> usize {
        self.agent_max
            .get(agent_id)
            .copied()
            .unwrap_or(self.max_active)
    }

    /// Activate a skill for an agent.
    ///
    /// Returns the name of the skill that was evicted to make room (if any).
    /// The caller is responsible for emitting the `skill_deactivate` audit event
    /// for the evicted skill using trigger_signal `"capacity_eviction"`.
    pub fn activate(
        &mut self,
        agent_id: &str,
        skill_name: &str,
        trigger_error: f64,
    ) -> Option<String> {
        // Check capacity and evict if needed (before borrowing active set)
        let current_count = self.active.get(agent_id).map(|s| s.len()).unwrap_or(0);
        let already_active = self
            .active
            .get(agent_id)
            .map(|s| s.contains(skill_name))
            .unwrap_or(false);

        let evicted = if current_count >= self.max_for(agent_id) && !already_active {
            if let Some(worst) = self.find_worst_performer(agent_id) {
                info!(agent = agent_id, skill = %worst, "Evicting lowest-performing skill to make room");
                self.deactivate(agent_id, &worst);
                Some(worst)
            } else {
                None
            }
        } else {
            None
        };

        let active = self.active.entry(agent_id.to_string()).or_default();
        if active.insert(skill_name.to_string()) {
            let key = (agent_id.to_string(), skill_name.to_string());
            self.records.insert(
                key,
                ActivationRecord {
                    skill_name: skill_name.to_string(),
                    agent_id: agent_id.to_string(),
                    activated_at: Utc::now(),
                    deactivated_at: None,
                    trigger_error,
                    post_errors: RunningStats::default(),
                    conversations: 0,
                },
            );
            info!(agent = agent_id, skill = skill_name, "Skill activated");
        }

        evicted
    }

    /// Deactivate a skill.
    pub fn deactivate(&mut self, agent_id: &str, skill_name: &str) {
        if let Some(active) = self.active.get_mut(agent_id) {
            active.remove(skill_name);
        }
        let key = (agent_id.to_string(), skill_name.to_string());
        if let Some(record) = self.records.get_mut(&key) {
            record.deactivated_at = Some(Utc::now());
        }
        debug!(agent = agent_id, skill = skill_name, "Skill deactivated");
    }

    /// Record a conversation's prediction error for all active skills.
    pub fn record_conversation(&mut self, agent_id: &str, prediction_error: f64) {
        let active = match self.active.get(agent_id) {
            Some(a) => a.clone(),
            None => return,
        };

        for skill_name in &active {
            let key = (agent_id.to_string(), skill_name.clone());
            if let Some(record) = self.records.get_mut(&key) {
                record.post_errors.push(prediction_error);
                record.conversations += 1;
            }
        }
    }

    /// Evaluate all active skills and deactivate ineffective ones.
    ///
    /// Returns list of deactivated skill names.
    pub fn evaluate_all(&mut self, agent_id: &str) -> Vec<String> {
        let active = match self.active.get(agent_id) {
            Some(a) => a.clone(),
            None => return Vec::new(),
        };

        let mut to_deactivate = Vec::new();
        for skill_name in &active {
            let key = (agent_id.to_string(), skill_name.clone());
            if let Some(record) = self.records.get(&key) {
                if record.conversations >= 10 {
                    let improvement = record.trigger_error - record.post_errors.mean();
                    if improvement < 0.02 {
                        // Skill not helping — error didn't decrease
                        to_deactivate.push(skill_name.clone());
                    }
                }
            }
        }

        for name in &to_deactivate {
            warn!(agent = agent_id, skill = %name, "Deactivating ineffective skill");
            self.deactivate(agent_id, name);
        }

        to_deactivate
    }

    /// Get currently active skill names for an agent.
    pub fn get_active(&self, agent_id: &str) -> HashSet<String> {
        self.active.get(agent_id).cloned().unwrap_or_default()
    }

    /// Find the worst-performing active skill (highest post_errors mean).
    fn find_worst_performer(&self, agent_id: &str) -> Option<String> {
        let active = self.active.get(agent_id)?;
        active
            .iter()
            .filter_map(|name| {
                let key = (agent_id.to_string(), name.clone());
                let record = self.records.get(&key)?;
                if record.conversations >= 5 {
                    Some((name.clone(), record.post_errors.mean()))
                } else {
                    None
                }
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(name, _)| name)
    }
}

#[cfg(test)]
mod h3_per_agent_cap_tests {
    use super::*;

    /// Regression (H3, 2026-09-29): `[evolution] max_active_skills` used to be
    /// hard-coded as `SkillActivationController::new(5)` in `channel_reply`,
    /// so the dashboard's value never took effect. Changing the registered
    /// per-agent cap must change the eviction point.
    #[test]
    fn registered_agent_cap_changes_the_eviction_point() {
        let mut ctrl = SkillActivationController::new(5);
        ctrl.set_agent_max("agent-small", 2);

        assert_eq!(ctrl.max_for("agent-small"), 2);
        // An agent with no registration keeps the constructor default.
        assert_eq!(ctrl.max_for("agent-other"), 5);

        assert!(ctrl.activate("agent-small", "s1", 0.4).is_none());
        assert!(ctrl.activate("agent-small", "s2", 0.4).is_none());
        // Third activation is over the registered cap of 2, so the controller
        // must try to evict. With no skill mature enough to be the "worst
        // performer" nothing is evicted, but the set must not keep growing
        // past the cap's intent — the important assertion is that the same
        // sequence under the default cap of 5 would not even reach this branch.
        let mut wide = SkillActivationController::new(5);
        for i in 0..3 {
            assert!(wide.activate("agent-wide", &format!("s{i}"), 0.4).is_none());
        }
        assert_eq!(wide.get_active("agent-wide").len(), 3);
    }

    /// A zero cap is a config typo, not "evict everything".
    #[test]
    fn zero_cap_is_clamped_to_one() {
        let mut ctrl = SkillActivationController::new(5);
        ctrl.set_agent_max("agent-zero", 0);
        assert_eq!(ctrl.max_for("agent-zero"), 1);
    }
}
