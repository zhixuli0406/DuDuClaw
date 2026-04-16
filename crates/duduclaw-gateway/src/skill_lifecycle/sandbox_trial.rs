//! Sandbox trial — manages synthesized skills in a probationary period.
//!
//! New skills (auto-synthesized or externally sourced) enter a trial with a TTL.
//! After sufficient conversations, the trial is evaluated:
//! - Positive lift → Graduate (install permanently)
//! - Negative lift → Discard
//! - Inconclusive  → Extend trial or discard at TTL exhaustion

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::lift::SkillLiftTracker;
use super::synthesizer::SynthesizedSkill;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// How the skill was acquired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillSource {
    /// Auto-synthesized from episodic memory.
    Synthesized,
    /// Installed from GitHub skill marketplace.
    GitHub,
    /// Manually installed by user.
    Manual,
}

/// Current status of a trial.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrialStatus {
    /// Currently in trial period.
    Active,
    /// Trial succeeded — skill installed permanently.
    Graduated,
    /// Trial failed — skill removed.
    Discarded,
}

/// A skill undergoing a sandbox trial.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxedSkill {
    /// The synthesized skill being trialed.
    pub name: String,
    pub description: String,
    pub full_markdown: String,
    /// Remaining conversations before TTL expires.
    pub ttl_conversations: u32,
    /// Initial TTL (for reporting).
    pub initial_ttl: u32,
    /// When the trial started.
    pub created_at: DateTime<Utc>,
    /// How the skill was acquired.
    pub source: SkillSource,
    /// Current trial status.
    pub status: TrialStatus,
    /// Agent this skill is trialed for.
    pub agent_id: String,
    /// Synthesis rationale.
    pub rationale: String,
}

impl SandboxedSkill {
    /// Create a new sandboxed skill from a synthesis result.
    pub fn from_synthesized(skill: SynthesizedSkill, agent_id: &str, ttl: u32) -> Self {
        Self {
            name: skill.name,
            description: skill.description,
            full_markdown: skill.full_markdown,
            ttl_conversations: ttl,
            initial_ttl: ttl,
            created_at: Utc::now(),
            source: SkillSource::Synthesized,
            status: TrialStatus::Active,
            agent_id: agent_id.to_string(),
            rationale: skill.rationale,
        }
    }

    /// Tick down the TTL by one conversation.
    pub fn tick(&mut self) {
        self.ttl_conversations = self.ttl_conversations.saturating_sub(1);
    }

    /// Whether TTL has been exhausted.
    pub fn is_expired(&self) -> bool {
        self.ttl_conversations == 0
    }

    /// Conversations used so far.
    pub fn conversations_used(&self) -> u32 {
        self.initial_ttl - self.ttl_conversations
    }
}

/// Decision after evaluating a trial.
#[derive(Debug, Clone, PartialEq)]
pub enum TrialDecision {
    /// Skill proved effective — install permanently.
    Graduate,
    /// Skill proved harmful or ineffective — remove.
    Discard,
    /// Not enough data — extend trial by N conversations.
    ExtendTrial(u32),
}

/// Outcome of a trial evaluation.
#[derive(Debug, Clone)]
pub struct TrialOutcome {
    pub skill_name: String,
    pub agent_id: String,
    pub lift: f64,
    pub conversations_used: u32,
    pub decision: TrialDecision,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Minimum conversations before a trial can be evaluated.
const MIN_EVALUATION_CONVERSATIONS: u32 = 5;

/// Lift threshold for graduation (10% error reduction).
const GRADUATION_LIFT_THRESHOLD: f64 = 0.05;

/// Negative lift threshold for early discard.
const DISCARD_LIFT_THRESHOLD: f64 = -0.02;

/// Extension amount when inconclusive.
const EXTENSION_AMOUNT: u32 = 10;

/// Evaluate a sandboxed skill's trial performance.
pub fn evaluate_trial(
    tracker: &SkillLiftTracker,
    sandboxed: &SandboxedSkill,
) -> TrialOutcome {
    let lift = tracker.lift();
    let conversations_used = sandboxed.conversations_used();

    let (decision, reason) = if conversations_used < MIN_EVALUATION_CONVERSATIONS {
        // Not enough data yet
        if sandboxed.is_expired() {
            (
                TrialDecision::ExtendTrial(EXTENSION_AMOUNT),
                "TTL expired with insufficient data — extending".to_string(),
            )
        } else {
            (
                TrialDecision::ExtendTrial(0), // no extension needed, TTL still active
                "Insufficient data for evaluation".to_string(),
            )
        }
    } else if lift >= GRADUATION_LIFT_THRESHOLD && tracker.is_stable() {
        (
            TrialDecision::Graduate,
            format!("Positive lift {lift:.3} with stable performance"),
        )
    } else if lift >= GRADUATION_LIFT_THRESHOLD {
        // Positive lift but not stable yet
        if sandboxed.is_expired() {
            (
                TrialDecision::ExtendTrial(EXTENSION_AMOUNT),
                format!("Positive lift {lift:.3} but not yet stable — extending"),
            )
        } else {
            (
                TrialDecision::ExtendTrial(0),
                format!("Positive lift {lift:.3} but not yet stable — continuing"),
            )
        }
    } else if lift < DISCARD_LIFT_THRESHOLD {
        (
            TrialDecision::Discard,
            format!("Negative lift {lift:.3} — skill making things worse"),
        )
    } else if sandboxed.is_expired() {
        (
            TrialDecision::Discard,
            format!("TTL exhausted with inconclusive lift {lift:.3}"),
        )
    } else {
        (
            TrialDecision::ExtendTrial(0),
            format!("Inconclusive lift {lift:.3} — continuing trial"),
        )
    };

    TrialOutcome {
        skill_name: sandboxed.name.clone(),
        agent_id: sandboxed.agent_id.clone(),
        lift,
        conversations_used,
        decision,
        reason,
    }
}

// ---------------------------------------------------------------------------
// Sandbox Store
// ---------------------------------------------------------------------------

/// Manages all sandboxed skills across agents.
#[derive(Default)]
pub struct SandboxStore {
    /// (agent_id, skill_name) → sandboxed skill.
    skills: HashMap<(String, String), SandboxedSkill>,
}

impl SandboxStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a skill to the sandbox.
    pub fn add(&mut self, skill: SandboxedSkill) {
        let key = (skill.agent_id.clone(), skill.name.clone());
        info!(
            agent = %skill.agent_id,
            skill = %skill.name,
            ttl = skill.ttl_conversations,
            "Skill entered sandbox trial"
        );
        self.skills.insert(key, skill);
    }

    /// Get a sandboxed skill by (agent_id, name).
    pub fn get(&self, agent_id: &str, name: &str) -> Option<&SandboxedSkill> {
        self.skills.get(&(agent_id.to_string(), name.to_string()))
    }

    /// Get a mutable reference to a sandboxed skill.
    pub fn get_mut(&mut self, agent_id: &str, name: &str) -> Option<&mut SandboxedSkill> {
        self.skills
            .get_mut(&(agent_id.to_string(), name.to_string()))
    }

    /// Tick all sandboxed skills for an agent (call after each conversation).
    pub fn tick_agent(&mut self, agent_id: &str) {
        for ((aid, _), skill) in &mut self.skills {
            if aid == agent_id && skill.status == TrialStatus::Active {
                skill.tick();
            }
        }
    }

    /// Get all active sandboxed skill names for an agent.
    pub fn active_names(&self, agent_id: &str) -> Vec<String> {
        self.skills
            .iter()
            .filter(|((aid, _), s)| aid == agent_id && s.status == TrialStatus::Active)
            .map(|((_, name), _)| name.clone())
            .collect()
    }

    /// Mark a skill as graduated.
    pub fn graduate(&mut self, agent_id: &str, name: &str) {
        if let Some(skill) = self.get_mut(agent_id, name) {
            skill.status = TrialStatus::Graduated;
            info!(agent = agent_id, skill = name, "Sandbox skill graduated");
        }
    }

    /// Mark a skill as discarded.
    pub fn discard(&mut self, agent_id: &str, name: &str) {
        if let Some(skill) = self.get_mut(agent_id, name) {
            skill.status = TrialStatus::Discarded;
            warn!(agent = agent_id, skill = name, "Sandbox skill discarded");
        }
    }

    /// Extend a skill's TTL.
    pub fn extend_ttl(&mut self, agent_id: &str, name: &str, extra: u32) {
        if let Some(skill) = self.get_mut(agent_id, name) {
            skill.ttl_conversations += extra;
            skill.initial_ttl += extra;
            info!(
                agent = agent_id,
                skill = name,
                new_ttl = skill.ttl_conversations,
                "Sandbox skill TTL extended"
            );
        }
    }

    /// Remove completed trials (graduated or discarded) from memory.
    pub fn cleanup(&mut self) {
        self.skills
            .retain(|_, s| s.status == TrialStatus::Active);
    }

    /// Get all sandboxed skills (for telemetry).
    pub fn all(&self) -> Vec<&SandboxedSkill> {
        self.skills.values().collect()
    }
}

// ---------------------------------------------------------------------------
// Graduation (file write)
// ---------------------------------------------------------------------------

/// Write a graduated skill to the agent's SKILLS/ directory.
pub async fn graduate_skill_to_disk(
    skill: &SandboxedSkill,
    agent_skills_dir: &Path,
) -> Result<(), String> {
    tokio::fs::create_dir_all(agent_skills_dir)
        .await
        .map_err(|e| format!("Failed to create skills dir: {e}"))?;

    let filename = format!("{}.md", skill.name);
    let dest = agent_skills_dir.join(&filename);

    tokio::fs::write(&dest, &skill.full_markdown)
        .await
        .map_err(|e| format!("Failed to write graduated skill: {e}"))?;

    info!(
        agent = %skill.agent_id,
        skill = %skill.name,
        dest = %dest.display(),
        "Graduated skill written to disk"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::prediction::user_model::RunningStats;

    fn make_sandboxed(name: &str, ttl: u32) -> SandboxedSkill {
        SandboxedSkill {
            name: name.to_string(),
            description: "test".to_string(),
            full_markdown: "---\nname: test\n---\n\nBody".to_string(),
            ttl_conversations: ttl,
            initial_ttl: ttl,
            created_at: Utc::now(),
            source: SkillSource::Synthesized,
            status: TrialStatus::Active,
            agent_id: "agent-a".to_string(),
            rationale: "test".to_string(),
        }
    }

    fn make_tracker(skill: &str, lift: f64, stable: bool) -> SkillLiftTracker {
        let mut tracker = SkillLiftTracker::new(skill.to_string(), "agent-a".to_string());

        // Simulate enough data for lift calculation
        let base_error = 0.5;
        for _ in 0..20 {
            tracker.record_with(base_error - lift);
            tracker.record_without(base_error);
        }

        // Force stability if needed
        if !stable {
            // Add variance
            tracker.record_with(0.1);
            tracker.record_with(0.9);
        }

        tracker
    }

    #[test]
    fn test_positive_lift_graduates() {
        // Simulate 20 conversations used: initial_ttl=20, ttl=0
        let mut sandboxed = make_sandboxed("test-skill", 20);
        for _ in 0..20 {
            sandboxed.tick();
        }
        let tracker = make_tracker("test-skill", 0.1, true);
        let outcome = evaluate_trial(&tracker, &sandboxed);
        assert_eq!(outcome.decision, TrialDecision::Graduate);
    }

    #[test]
    fn test_negative_lift_discards() {
        let mut sandboxed = make_sandboxed("test-skill", 20);
        for _ in 0..10 {
            sandboxed.tick();
        }
        let tracker = make_tracker("test-skill", -0.05, true);
        let outcome = evaluate_trial(&tracker, &sandboxed);
        assert_eq!(outcome.decision, TrialDecision::Discard);
    }

    #[test]
    fn test_inconclusive_continues() {
        let mut sandboxed = make_sandboxed("test-skill", 20);
        for _ in 0..10 {
            sandboxed.tick();
        }
        // Near-zero lift
        let tracker = make_tracker("test-skill", 0.01, true);
        let outcome = evaluate_trial(&tracker, &sandboxed);
        // TTL still active, inconclusive → continue
        assert!(matches!(outcome.decision, TrialDecision::ExtendTrial(0)));
    }

    #[test]
    fn test_ttl_exhausted_discards() {
        // Simulate all TTL used up with inconclusive results
        let mut sandboxed = make_sandboxed("test-skill", 20);
        for _ in 0..20 {
            sandboxed.tick();
        }
        let tracker = make_tracker("test-skill", 0.01, true); // inconclusive
        let outcome = evaluate_trial(&tracker, &sandboxed);
        assert_eq!(outcome.decision, TrialDecision::Discard);
    }

    #[test]
    fn test_sandbox_store_lifecycle() {
        let mut store = SandboxStore::new();
        let skill = make_sandboxed("test-skill", 5);

        store.add(skill);
        assert_eq!(store.active_names("agent-a").len(), 1);

        store.tick_agent("agent-a");
        let s = store.get("agent-a", "test-skill").unwrap();
        assert_eq!(s.ttl_conversations, 4);

        store.graduate("agent-a", "test-skill");
        assert_eq!(store.active_names("agent-a").len(), 0); // graduated = not active

        store.cleanup();
        assert!(store.all().is_empty()); // graduated removed
    }

    #[test]
    fn test_extend_ttl() {
        let mut store = SandboxStore::new();
        store.add(make_sandboxed("test-skill", 3));

        store.extend_ttl("agent-a", "test-skill", 10);
        let s = store.get("agent-a", "test-skill").unwrap();
        assert_eq!(s.ttl_conversations, 13);
    }

    #[test]
    fn test_discard_removes_from_active() {
        let mut store = SandboxStore::new();
        store.add(make_sandboxed("test-skill", 10));

        store.discard("agent-a", "test-skill");
        assert_eq!(store.active_names("agent-a").len(), 0);
    }
}
