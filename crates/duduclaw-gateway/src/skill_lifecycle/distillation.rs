//! Skill distillation — detects skills that have earned a permanent place.
//!
//! Skills that are consistently effective (high lift, stable, mature) are
//! distillation candidates. **S11 (2026-09-29)**: `build_distillation_input`,
//! which assembled a GVU `GeneratorInput` asking an LLM to rewrite `SOUL.md`
//! with the skill's behaviours, was removed together with the legacy SOUL
//! write path — `SOUL.md` is read-only for agents, and the function had no
//! production caller (only its own test). What is left is the detector
//! ([`scan_for_distillation`]), whose candidates `channel_reply` logs; the
//! landing surface for a distilled behaviour is the playbook, which AEE owns.

use super::lift::SkillLiftTracker;

/// Minimum readiness score for distillation (0.0 - 1.0).
pub const DISTILLATION_THRESHOLD: f64 = 0.75;

/// A skill that is ready for distillation into SOUL.md.
#[derive(Debug, Clone)]
pub struct DistillationCandidate {
    pub skill_name: String,
    pub agent_id: String,
    pub load_count: u64,
    pub lift: f64,
    pub is_stable: bool,
    pub readiness: f64,
}

impl DistillationCandidate {
    /// Calculate readiness for distillation.
    pub fn from_tracker(tracker: &SkillLiftTracker) -> Self {
        let lift = tracker.lift();
        let is_stable = tracker.is_stable();
        let usage_maturity = (tracker.load_count as f64 / 50.0).min(1.0);
        let positive_lift = if lift > 0.05 {
            1.0
        } else {
            (lift / 0.05).max(0.0)
        };
        let stability = if is_stable { 1.0 } else { 0.3 };

        let readiness =
            (0.3 * usage_maturity + 0.5 * positive_lift + 0.2 * stability).clamp(0.0, 1.0);

        Self {
            skill_name: tracker.skill_name.clone(),
            agent_id: tracker.agent_id.clone(),
            load_count: tracker.load_count,
            lift,
            is_stable,
            readiness,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.readiness >= DISTILLATION_THRESHOLD
    }
}

/// Scan all skill trackers and return candidates ready for distillation.
pub fn scan_for_distillation(
    _agent_id: &str,
    trackers: &[&SkillLiftTracker],
) -> Vec<DistillationCandidate> {
    trackers
        .iter()
        .filter(|t| t.is_mature())
        .map(|t| DistillationCandidate::from_tracker(t))
        .filter(|c| c.is_ready())
        .collect()
}
