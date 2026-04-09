//! Reward computation for RL training trajectories.

use super::types::RLTrajectory;

/// Trait for computing rewards from trajectories.
pub trait RewardComputer: Send + Sync {
    fn compute(&self, trajectory: &RLTrajectory) -> f64;
    fn name(&self) -> &str;
}

/// Binary outcome reward (task completion).
pub struct OutcomeReward;

impl RewardComputer for OutcomeReward {
    fn compute(&self, trajectory: &RLTrajectory) -> f64 {
        trajectory.outcome_reward
    }
    fn name(&self) -> &str {
        "outcome"
    }
}

/// Tool use efficiency reward (fewer calls for same outcome = higher reward).
pub struct ToolEfficiencyReward {
    /// Expected number of tool calls for a typical task.
    baseline_calls: f64,
}

impl ToolEfficiencyReward {
    pub fn new(baseline: f64) -> Self {
        Self {
            baseline_calls: baseline,
        }
    }
}

impl RewardComputer for ToolEfficiencyReward {
    fn compute(&self, trajectory: &RLTrajectory) -> f64 {
        let actual = trajectory.total_tool_calls() as f64;
        if actual == 0.0 {
            return 0.0;
        }
        // Reward = baseline / actual, capped at 1.0
        (self.baseline_calls / actual).min(1.0)
    }
    fn name(&self) -> &str {
        "tool_efficiency"
    }
}

/// Soft overlong punishment (OpenHands RL SWE, arXiv 2508.03501).
///
/// Linear penalty when total tokens exceed threshold.
pub struct SoftOverlongPunishment {
    threshold: u64,
    alpha: f64,
}

impl SoftOverlongPunishment {
    pub fn new(threshold: u64, alpha: f64) -> Self {
        Self { threshold, alpha }
    }

    pub fn default_config() -> Self {
        Self {
            threshold: 32_000,
            alpha: 0.5,
        }
    }
}

impl RewardComputer for SoftOverlongPunishment {
    fn compute(&self, trajectory: &RLTrajectory) -> f64 {
        if trajectory.total_tokens <= self.threshold {
            0.0 // No penalty
        } else {
            let excess = (trajectory.total_tokens - self.threshold) as f64;
            -self.alpha * excess / self.threshold as f64
        }
    }
    fn name(&self) -> &str {
        "overlong_punishment"
    }
}

/// Composite reward combining multiple signals.
pub struct CompositeReward {
    components: Vec<(Box<dyn RewardComputer>, f64)>, // (computer, weight)
}

impl CompositeReward {
    pub fn new() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    pub fn add(mut self, computer: Box<dyn RewardComputer>, weight: f64) -> Self {
        self.components.push((computer, weight));
        self
    }

    /// Create default composite: outcome(0.7) + efficiency(0.2) + overlong(0.1)
    pub fn default_config() -> Self {
        Self::new()
            .add(Box::new(OutcomeReward), 0.7)
            .add(Box::new(ToolEfficiencyReward::new(5.0)), 0.2)
            .add(Box::new(SoftOverlongPunishment::default_config()), 0.1)
    }
}

impl Default for CompositeReward {
    fn default() -> Self {
        Self::new()
    }
}

impl RewardComputer for CompositeReward {
    fn compute(&self, trajectory: &RLTrajectory) -> f64 {
        self.components
            .iter()
            .map(|(computer, weight)| computer.compute(trajectory) * weight)
            .sum()
    }
    fn name(&self) -> &str {
        "composite"
    }
}
