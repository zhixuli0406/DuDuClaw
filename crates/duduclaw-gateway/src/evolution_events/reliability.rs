//! Agent Reliability Dashboard — Phase 1 (W20-P0).
//!
//! This module defines the [`ReliabilitySummary`] data model and **pure**
//! helper functions used by [`AuditEventIndex::compute_reliability_summary`]
//! to compute the four reliability metrics.
//!
//! ## Metrics
//! | Metric | Description | Empty default |
//! |--------|-------------|---------------|
//! | `consistency_score`     | Unweighted arithmetic mean per-event-type success rate | `1.0` |
//! | `task_success_rate`     | Fraction of events with `outcome=success`  | `1.0` |
//! | `skill_adoption_rate`   | Fraction of `skill_activate` events        | `0.0` |
//! | `fallback_trigger_rate` | Fraction of `llm_fallback_triggered` events| `0.0` |
//!
//! ## Design note
//! The pure helpers (`consistency_from_rows`, etc.) are extracted from the DB
//! query method so they can be tested independently, without needing a live
//! SQLite connection.  All four helpers are `pub(super)` so that
//! [`super::query`] can import and use them.

use serde::{Deserialize, Serialize};

// ── Data model ─────────────────────────────────────────────────────────────────

/// Reliability summary for a single agent over a configurable time window.
///
/// All rate fields are in `[0.0, 1.0]`.  `generated_at` is an ISO8601 / RFC3339
/// timestamp recorded when the summary was computed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReliabilitySummary {
    /// The agent whose reliability was measured.
    pub agent_id: String,

    /// Size of the measurement window in days (default 7).
    pub window_days: u32,

    /// Unweighted arithmetic mean of per-task-type (event_type) success rate.
    ///
    /// Computed as: `mean( success_count[t] / total[t] )` over all `event_type`
    /// groups `t` that have at least one event in the window.
    /// Returns `1.0` when no events are present (conservative neutral default).
    pub consistency_score: f64,

    /// Fraction of all events in the window where `outcome = 'success'`.
    ///
    /// Returns `1.0` when no events are present.
    pub task_success_rate: f64,

    /// Fraction of all events in the window that are `skill_activate` events.
    ///
    /// Returns `0.0` when no events are present.
    pub skill_adoption_rate: f64,

    /// Fraction of all events in the window that are `llm_fallback_triggered` events.
    ///
    /// Returns `0.0` when no events are present.
    /// NOTE (W20): Full data requires `llm_fallback.rs` to emit to EvolutionEventLogger
    /// (planned for Phase 2). Until then, this field reflects only fallback events
    /// already captured in the evolution-events audit trail.
    pub fallback_trigger_rate: f64,

    /// Total number of audit events counted in the window.
    /// `0` means no audit history is available for this agent.
    pub total_events: i64,

    /// RFC3339 timestamp when this summary was generated.
    pub generated_at: String,
}

// ── Pure helper functions (independently testable) ─────────────────────────────

/// Compute the average success rate across per-event-type groups.
///
/// `rows` is a list of `(event_type, total_count, success_count)` tuples.
/// Each tuple represents one `GROUP BY event_type` bucket from the SQLite query.
///
/// Returns `1.0` (neutral) when `rows` is empty (no audit history → assume perfect).
pub(super) fn consistency_from_rows(rows: &[(String, i64, i64)]) -> f64 {
    if rows.is_empty() {
        return 1.0;
    }
    let sum: f64 = rows
        .iter()
        .map(|(_, total, success)| *success as f64 / (*total as f64).max(1.0))
        .sum();
    (sum / rows.len() as f64).clamp(0.0, 1.0)
}

/// Compute the task success rate from aggregate counts.
///
/// Returns `1.0` when `total == 0`.
pub(super) fn task_success_rate_from_counts(total: i64, success_count: i64) -> f64 {
    if total == 0 {
        1.0
    } else {
        (success_count as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// Compute the skill adoption rate from aggregate counts.
///
/// Returns `0.0` when `total == 0`.
pub(super) fn skill_adoption_rate_from_counts(total: i64, skill_count: i64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (skill_count as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// Compute the fallback trigger rate from aggregate counts.
///
/// Returns `0.0` when `total == 0`.
pub(super) fn fallback_trigger_rate_from_counts(total: i64, fallback_count: i64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (fallback_count as f64 / total as f64).clamp(0.0, 1.0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-10;

    // ── consistency_from_rows ──────────────────────────────────────────────────

    /// Empty input → neutral default of 1.0
    #[test]
    fn consistency_empty_returns_one() {
        assert_eq!(consistency_from_rows(&[]), 1.0);
    }

    /// All events succeed for every task type → 1.0
    #[test]
    fn consistency_all_success() {
        let rows = vec![
            ("skill_activate".to_string(), 10, 10),
            ("gvu_generation".to_string(), 5, 5),
            ("security_scan".to_string(), 3, 3),
        ];
        assert!((consistency_from_rows(&rows) - 1.0).abs() < EPS);
    }

    /// All events fail for every task type → 0.0
    #[test]
    fn consistency_all_failure() {
        let rows = vec![
            ("skill_activate".to_string(), 10, 0),
            ("gvu_generation".to_string(), 4, 0),
        ];
        assert!((consistency_from_rows(&rows) - 0.0).abs() < EPS);
    }

    /// Single task type with 100% success
    #[test]
    fn consistency_single_type_full_success() {
        let rows = vec![("gvu_generation".to_string(), 7, 7)];
        assert!((consistency_from_rows(&rows) - 1.0).abs() < EPS);
    }

    /// Two types: one 100%, one 50% → average = 0.75
    #[test]
    fn consistency_two_types_mixed() {
        let rows = vec![
            ("type_a".to_string(), 10, 10), // 1.0
            ("type_b".to_string(), 10, 5),  // 0.5
        ];
        assert!((consistency_from_rows(&rows) - 0.75).abs() < EPS);
    }

    /// Three types: 1.0, 0.5, 0.0 → average = 0.5
    #[test]
    fn consistency_three_types_varied() {
        let rows = vec![
            ("type_a".to_string(), 4, 4),  // 1.0
            ("type_b".to_string(), 4, 2),  // 0.5
            ("type_c".to_string(), 4, 0),  // 0.0
        ];
        let expected = (1.0_f64 + 0.5 + 0.0) / 3.0;
        assert!((consistency_from_rows(&rows) - expected).abs() < EPS);
    }

    /// Single-event task type with success
    #[test]
    fn consistency_single_event_success() {
        let rows = vec![("skill_activate".to_string(), 1, 1)];
        assert!((consistency_from_rows(&rows) - 1.0).abs() < EPS);
    }

    /// Single-event task type with failure
    #[test]
    fn consistency_single_event_failure() {
        let rows = vec![("skill_activate".to_string(), 1, 0)];
        assert!((consistency_from_rows(&rows) - 0.0).abs() < EPS);
    }

    // ── task_success_rate_from_counts ──────────────────────────────────────────

    /// No events → neutral 1.0
    #[test]
    fn task_success_rate_empty() {
        assert_eq!(task_success_rate_from_counts(0, 0), 1.0);
    }

    /// All events succeed
    #[test]
    fn task_success_rate_all_success() {
        assert!((task_success_rate_from_counts(10, 10) - 1.0).abs() < EPS);
    }

    /// All events fail
    #[test]
    fn task_success_rate_all_failure() {
        assert!((task_success_rate_from_counts(10, 0) - 0.0).abs() < EPS);
    }

    /// 8/10 succeed → 0.8
    #[test]
    fn task_success_rate_partial() {
        assert!((task_success_rate_from_counts(10, 8) - 0.8).abs() < EPS);
    }

    /// 1/100 succeed → 0.01
    #[test]
    fn task_success_rate_rare_success() {
        assert!((task_success_rate_from_counts(100, 1) - 0.01).abs() < EPS);
    }

    // ── skill_adoption_rate_from_counts ───────────────────────────────────────

    /// No events → 0.0
    #[test]
    fn skill_adoption_empty() {
        assert_eq!(skill_adoption_rate_from_counts(0, 0), 0.0);
    }

    /// 3 skill_activates out of 10 events → 0.3
    #[test]
    fn skill_adoption_partial() {
        assert!((skill_adoption_rate_from_counts(10, 3) - 0.3).abs() < EPS);
    }

    /// All events are skill_activate → 1.0
    #[test]
    fn skill_adoption_all() {
        assert!((skill_adoption_rate_from_counts(5, 5) - 1.0).abs() < EPS);
    }

    /// No skill_activate events → 0.0
    #[test]
    fn skill_adoption_none() {
        assert!((skill_adoption_rate_from_counts(20, 0) - 0.0).abs() < EPS);
    }

    // ── fallback_trigger_rate_from_counts ─────────────────────────────────────

    /// No events → 0.0
    #[test]
    fn fallback_rate_empty() {
        assert_eq!(fallback_trigger_rate_from_counts(0, 0), 0.0);
    }

    /// 5/100 fallbacks → 0.05
    #[test]
    fn fallback_rate_partial() {
        assert!((fallback_trigger_rate_from_counts(100, 5) - 0.05).abs() < EPS);
    }

    /// No fallbacks → 0.0
    #[test]
    fn fallback_rate_zero() {
        assert!((fallback_trigger_rate_from_counts(50, 0) - 0.0).abs() < EPS);
    }

    /// All events are fallbacks → 1.0
    #[test]
    fn fallback_rate_all() {
        assert!((fallback_trigger_rate_from_counts(10, 10) - 1.0).abs() < EPS);
    }

    // ── ReliabilitySummary serde round-trip ───────────────────────────────────

    #[test]
    fn reliability_summary_serde_round_trip() {
        let original = ReliabilitySummary {
            agent_id: "test-agent".to_string(),
            window_days: 7,
            consistency_score: 0.87,
            task_success_rate: 0.91,
            skill_adoption_rate: 0.34,
            fallback_trigger_rate: 0.05,
            total_events: 100,
            generated_at: "2026-05-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ReliabilitySummary = serde_json::from_str(&json).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn reliability_summary_json_fields() {
        let summary = ReliabilitySummary {
            agent_id: "my-agent".to_string(),
            window_days: 7,
            consistency_score: 0.9,
            task_success_rate: 0.85,
            skill_adoption_rate: 0.2,
            fallback_trigger_rate: 0.02,
            total_events: 50,
            generated_at: "2026-05-01T10:00:00Z".to_string(),
        };
        let v: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert_eq!(v["agent_id"], "my-agent");
        assert_eq!(v["window_days"], 7);
        assert!((v["consistency_score"].as_f64().unwrap() - 0.9).abs() < EPS);
        assert!((v["task_success_rate"].as_f64().unwrap() - 0.85).abs() < EPS);
        assert_eq!(v["total_events"], 50);
    }
}
