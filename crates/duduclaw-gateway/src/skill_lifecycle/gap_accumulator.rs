//! Gap accumulator — tracks repeated skill gaps to trigger auto-synthesis.
//!
//! When the diagnostician detects the same domain gap N times (default 3),
//! the accumulator fires a `SynthesisTrigger` so the system can automatically
//! synthesize a new skill from episodic memory.
//!
//! Zero LLM cost — pure bookkeeping.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::diagnostician::SkillGap;

/// Maximum evidence entries per gap record.
const MAX_EVIDENCE: usize = 10;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A record of accumulated gap occurrences for a single topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapRecord {
    /// Normalized topic string.
    pub topic: String,
    /// Number of times this gap has been observed.
    pub count: u32,
    /// First observation timestamp.
    pub first_seen: DateTime<Utc>,
    /// Most recent observation timestamp.
    pub last_seen: DateTime<Utc>,
    /// Evidence strings from diagnostician (ring buffer, max 10).
    pub evidence: VecDeque<String>,
    /// Recent composite error magnitudes.
    pub composite_errors: VecDeque<f64>,
}

impl GapRecord {
    fn new(topic: String, evidence: String, composite_error: f64) -> Self {
        let mut ev = VecDeque::with_capacity(MAX_EVIDENCE);
        ev.push_back(evidence);
        let mut ce = VecDeque::with_capacity(MAX_EVIDENCE);
        ce.push_back(composite_error);
        Self {
            topic,
            count: 1,
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            evidence: ev,
            composite_errors: ce,
        }
    }

    fn push_evidence(&mut self, evidence: String, composite_error: f64) {
        self.count += 1;
        self.last_seen = Utc::now();

        if self.evidence.len() >= MAX_EVIDENCE {
            self.evidence.pop_front();
        }
        self.evidence.push_back(evidence);

        if self.composite_errors.len() >= MAX_EVIDENCE {
            self.composite_errors.pop_front();
        }
        self.composite_errors.push_back(composite_error);
    }

    fn avg_composite_error(&self) -> f64 {
        if self.composite_errors.is_empty() {
            return 0.0;
        }
        self.composite_errors.iter().sum::<f64>() / self.composite_errors.len() as f64
    }
}

/// Trigger emitted when a gap accumulates enough evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisTrigger {
    /// Agent that needs the skill.
    pub agent_id: String,
    /// Normalized topic.
    pub topic: String,
    /// Number of gap observations.
    pub gap_count: u32,
    /// Collected evidence strings.
    pub evidence: Vec<String>,
    /// Average composite prediction error for this topic.
    pub avg_composite_error: f64,
}

// ---------------------------------------------------------------------------
// Accumulator
// ---------------------------------------------------------------------------

/// Accumulates skill gaps per (agent, topic) and triggers synthesis.
///
/// Lifecycle: `record_gap()` fires trigger → caller marks pending via
/// `mark_pending()` → synthesis runs → `confirm_synthesis()` on success
/// or `cancel_pending()` on failure (re-enables future triggers).
pub struct GapAccumulator {
    /// (agent_id, normalized_topic) → gap record.
    gaps: HashMap<(String, String), GapRecord>,
    /// Number of gap occurrences required to trigger synthesis.
    synthesis_threshold: u32,
    /// Cooldown period after successful synthesis (prevent re-triggering same topic).
    cooldown_hours: u64,
    /// Last successful synthesis time per (agent_id, topic).
    last_synthesis: HashMap<(String, String), DateTime<Utc>>,
    /// Topics currently being synthesized (prevents re-triggering during async synthesis).
    pending: std::collections::HashSet<(String, String)>,
}

impl GapAccumulator {
    pub fn new(synthesis_threshold: u32, cooldown_hours: u64) -> Self {
        Self {
            gaps: HashMap::new(),
            synthesis_threshold,
            cooldown_hours,
            last_synthesis: HashMap::new(),
            pending: std::collections::HashSet::new(),
        }
    }

    /// Record a skill gap occurrence. Returns a `SynthesisTrigger` if the
    /// threshold is reached and the cooldown has expired.
    pub fn record_gap(
        &mut self,
        agent_id: &str,
        gap: &SkillGap,
        composite_error: f64,
    ) -> Option<SynthesisTrigger> {
        let normalized = normalize_topic(&gap.suggested_name);
        let key = (agent_id.to_string(), normalized.clone());

        // Skip if synthesis is already pending for this topic
        if self.pending.contains(&key) {
            debug!(agent = agent_id, topic = %normalized, "Gap skipped — synthesis pending");
            return None;
        }

        // Check cooldown from last successful synthesis
        if let Some(last) = self.last_synthesis.get(&key) {
            let elapsed = Utc::now().signed_duration_since(*last);
            if elapsed.num_hours() < self.cooldown_hours as i64 {
                debug!(
                    agent = agent_id,
                    topic = %normalized,
                    cooldown_remaining_h = self.cooldown_hours as i64 - elapsed.num_hours(),
                    "Gap recording skipped — synthesis cooldown active"
                );
                return None;
            }
        }

        let evidence_str = gap.evidence.first().cloned().unwrap_or_default();

        let record = self.gaps.entry(key.clone());
        let record = match record {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().push_evidence(evidence_str, composite_error);
                e.into_mut()
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(GapRecord::new(normalized.clone(), evidence_str, composite_error))
            }
        };

        debug!(
            agent = agent_id,
            topic = %normalized,
            count = record.count,
            threshold = self.synthesis_threshold,
            "Gap recorded"
        );

        if record.count >= self.synthesis_threshold {
            let trigger = SynthesisTrigger {
                agent_id: agent_id.to_string(),
                topic: normalized.clone(),
                gap_count: record.count,
                evidence: record.evidence.iter().cloned().collect(),
                avg_composite_error: record.avg_composite_error(),
            };

            info!(
                agent = agent_id,
                topic = %normalized,
                count = record.count,
                avg_error = %format!("{:.3}", trigger.avg_composite_error),
                "Synthesis trigger fired"
            );

            // Note: We do NOT clear the gap or set cooldown here.
            // The caller must call `confirm_synthesis()` after successful synthesis
            // to set cooldown and clear evidence. This prevents losing accumulated
            // evidence if synthesis fails.

            Some(trigger)
        } else {
            None
        }
    }

    /// Mark a topic as having synthesis in progress.
    /// Prevents re-triggering while the async synthesis pipeline runs.
    pub fn mark_pending(&mut self, agent_id: &str, topic: &str) {
        let normalized = normalize_topic(topic);
        self.pending.insert((agent_id.to_string(), normalized));
    }

    /// Confirm that synthesis for a topic succeeded.
    /// Sets cooldown and clears the accumulated gap evidence.
    /// Call this only after the synthesis pipeline completes successfully.
    pub fn confirm_synthesis(&mut self, agent_id: &str, topic: &str) {
        let normalized = normalize_topic(topic);
        let key = (agent_id.to_string(), normalized);
        self.pending.remove(&key);
        self.last_synthesis.insert(key.clone(), Utc::now());
        self.gaps.remove(&key);
    }

    /// Cancel a pending synthesis (e.g., on failure).
    /// Removes from pending set but does NOT set cooldown,
    /// allowing the topic to re-accumulate and trigger again.
    pub fn cancel_pending(&mut self, agent_id: &str, topic: &str) {
        let normalized = normalize_topic(topic);
        let key = (agent_id.to_string(), normalized);
        self.pending.remove(&key);
    }

    /// Clear accumulated gaps for a topic (e.g., after manual skill install).
    pub fn clear_topic(&mut self, agent_id: &str, topic: &str) {
        let normalized = normalize_topic(topic);
        let key = (agent_id.to_string(), normalized);
        self.gaps.remove(&key);
    }

    /// Get current gap counts (for telemetry/debugging).
    pub fn snapshot(&self) -> Vec<(&str, &str, u32)> {
        self.gaps
            .iter()
            .map(|((agent, topic), record)| (agent.as_str(), topic.as_str(), record.count))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Normalize topic for consistent matching.
fn normalize_topic(topic: &str) -> String {
    topic
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gap(name: &str) -> SkillGap {
        SkillGap {
            suggested_name: name.to_string(),
            suggested_description: format!("Gap for {name}"),
            evidence: vec![format!("Error related to {name}")],
        }
    }

    #[test]
    fn test_below_threshold_no_trigger() {
        let mut acc = GapAccumulator::new(3, 24);
        let gap = make_gap("return_policy");

        assert!(acc.record_gap("agent-a", &gap, 0.5).is_none());
        assert!(acc.record_gap("agent-a", &gap, 0.6).is_none());
        // Only 2 — should not trigger
    }

    #[test]
    fn test_threshold_triggers() {
        let mut acc = GapAccumulator::new(3, 24);
        let gap = make_gap("return_policy");

        assert!(acc.record_gap("agent-a", &gap, 0.5).is_none());
        assert!(acc.record_gap("agent-a", &gap, 0.6).is_none());
        let trigger = acc.record_gap("agent-a", &gap, 0.7);

        assert!(trigger.is_some());
        let t = trigger.unwrap();
        assert_eq!(t.agent_id, "agent-a");
        assert_eq!(t.topic, "return_policy");
        assert_eq!(t.gap_count, 3);
        assert!((t.avg_composite_error - 0.6).abs() < 0.01);
    }

    #[test]
    fn test_cooldown_prevents_retrigger() {
        let mut acc = GapAccumulator::new(3, 24);
        let gap = make_gap("return_policy");

        // First trigger
        acc.record_gap("agent-a", &gap, 0.5);
        acc.record_gap("agent-a", &gap, 0.6);
        let t1 = acc.record_gap("agent-a", &gap, 0.7);
        assert!(t1.is_some());

        // Confirm synthesis (sets cooldown + clears evidence)
        acc.confirm_synthesis("agent-a", "return_policy");

        // Try again — should be blocked by cooldown
        assert!(acc.record_gap("agent-a", &gap, 0.5).is_none());
        assert!(acc.record_gap("agent-a", &gap, 0.6).is_none());
        assert!(acc.record_gap("agent-a", &gap, 0.7).is_none());
    }

    #[test]
    fn test_different_topics_independent() {
        let mut acc = GapAccumulator::new(3, 24);
        let gap_a = make_gap("return_policy");
        let gap_b = make_gap("shipping_info");

        acc.record_gap("agent-a", &gap_a, 0.5);
        acc.record_gap("agent-a", &gap_b, 0.5);
        acc.record_gap("agent-a", &gap_a, 0.6);
        acc.record_gap("agent-a", &gap_b, 0.6);

        // gap_a: 2 hits, gap_b: 2 hits — neither triggers
        assert!(acc.record_gap("agent-a", &gap_b, 0.7).is_some()); // 3rd for b
    }

    #[test]
    fn test_evidence_ring_buffer() {
        let mut acc = GapAccumulator::new(15, 24);
        let gap = make_gap("test_topic");

        for i in 0..12 {
            acc.record_gap("agent-a", &gap, 0.5 + i as f64 * 0.01);
        }

        let key = ("agent-a".to_string(), "test_topic".to_string());
        let record = acc.gaps.get(&key).unwrap();
        assert_eq!(record.evidence.len(), 10); // ring buffer cap
        assert_eq!(record.composite_errors.len(), 10);
    }

    #[test]
    fn test_normalize_topic() {
        assert_eq!(normalize_topic("  Return  Policy  "), "return policy");
        assert_eq!(normalize_topic("SHIPPING"), "shipping");
    }

    #[test]
    fn test_clear_topic() {
        let mut acc = GapAccumulator::new(3, 24);
        let gap = make_gap("return_policy");

        acc.record_gap("agent-a", &gap, 0.5);
        acc.record_gap("agent-a", &gap, 0.6);
        acc.clear_topic("agent-a", "return_policy");

        // Should start from scratch — 2 more won't trigger
        assert!(acc.record_gap("agent-a", &gap, 0.5).is_none());
        assert!(acc.record_gap("agent-a", &gap, 0.6).is_none());
    }

    #[test]
    fn test_snapshot() {
        let mut acc = GapAccumulator::new(5, 24);
        let gap = make_gap("return_policy");
        acc.record_gap("agent-a", &gap, 0.5);
        acc.record_gap("agent-a", &gap, 0.6);

        let snap = acc.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].2, 2);
    }
}
