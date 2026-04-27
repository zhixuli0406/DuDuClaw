//! Quality Scorer — Phase 1 of the Rollout-to-Skill synthesis pipeline (W19-P0).
//!
//! Parses `data/evolution/events/YYYY-MM-DD.jsonl`, extracts `gvu_generation`
//! events with `outcome=success` (semantically equivalent to COSPLAY's "Applied"),
//! groups them by `agent_id` + 6-hour time windows, computes composite quality
//! scores, and returns the top-20% high-quality trajectories.
//!
//! ## Quality Score Formula
//! ```text
//! quality_score = success_rate × 0.40
//!               + effectiveness_score_delta × 0.35
//!               + task_complexity × 0.25
//! ```
//!
//! ## Metadata Field Contract
//! These optional fields are expected inside `AuditEvent.metadata` for
//! `gvu_generation` events. Missing fields fall back to safe defaults:
//! - `effectiveness_score_delta` (f64, range [0, 1], default: 0.0)
//! - `task_complexity` (f64, range [0, 1], default: 0.5)
//!
//! ## COSPLAY Reference
//! arXiv:2604.20987 — "COSPLAY: Skill-augmented Agent Self-Play"
//! The +25.1% performance improvement comes from closing the feedback loop:
//! task trajectory → quality filter → skill synthesis → skill bank.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::evolution_events::schema::{AuditEvent, AuditEventType, Outcome};

// ── Types ─────────────────────────────────────────────────────────────────────

/// A single `gvu_generation` event extracted from the JSONL audit log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryEvent {
    /// RFC3339 timestamp parsed from the audit event.
    pub timestamp: DateTime<Utc>,
    /// Agent that produced this event.
    pub agent_id: String,
    /// Improvement in effectiveness score (from metadata).
    /// Range [0, 1]. Defaults to 0.0 if absent in metadata.
    pub effectiveness_score_delta: f64,
    /// Estimated task complexity (from metadata).
    /// Range [0, 1]. Defaults to 0.5 if absent in metadata.
    pub task_complexity: f64,
    /// Whether the GVU cycle succeeded (`outcome=success`).
    pub is_success: bool,
    /// Skill ID, if any (for downstream graduation linkage).
    pub skill_id: Option<String>,
}

/// A group of [`TrajectoryEvent`]s within one agent + time-window bucket,
/// annotated with an aggregate quality score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredTrajectory {
    /// Agent that produced all events in this bucket.
    pub agent_id: String,
    /// Start of the time window (timestamp of first event).
    pub window_start: DateTime<Utc>,
    /// End of the time window (timestamp of last event).
    pub window_end: DateTime<Utc>,
    /// Total number of `gvu_generation` events in this window.
    pub event_count: u32,
    /// Number of successful events.
    pub success_count: u32,
    /// Composite quality score in [0.0, 1.0].
    pub quality_score: f64,
    /// The underlying events (preserved for downstream extraction).
    pub events: Vec<TrajectoryEvent>,
}

impl ScoredTrajectory {
    /// Convenience: success rate for this trajectory window.
    pub fn success_rate(&self) -> f64 {
        if self.event_count == 0 {
            return 0.0;
        }
        self.success_count as f64 / self.event_count as f64
    }
}

/// Configuration for the quality scorer.
///
/// All weights must sum to 1.0 (validated by [`ScorerConfig::validate`]).
#[derive(Debug, Clone)]
pub struct ScorerConfig {
    /// Window size for grouping events into trajectories.
    /// Default: 6 hours (matches the pipeline trigger interval).
    pub window_duration: Duration,
    /// Fraction of top-scoring trajectories to keep.
    /// 0.20 = top 20%. Must be in (0, 1].
    pub top_percentile: f64,
    /// Weight for the success rate component (default: 0.40).
    pub weight_success_rate: f64,
    /// Weight for the effectiveness_score_delta component (default: 0.35).
    pub weight_effectiveness: f64,
    /// Weight for the task_complexity component (default: 0.25).
    pub weight_complexity: f64,
}

impl Default for ScorerConfig {
    fn default() -> Self {
        Self {
            window_duration: Duration::hours(6),
            top_percentile: 0.20,
            weight_success_rate: 0.40,
            weight_effectiveness: 0.35,
            weight_complexity: 0.25,
        }
    }
}

impl ScorerConfig {
    /// Validate that the weight sum is (approximately) 1.0 and that
    /// `top_percentile` is in range (0, 1].
    ///
    /// Returns `Ok(())` when valid, or an error description.
    pub fn validate(&self) -> Result<(), String> {
        let weight_sum =
            self.weight_success_rate + self.weight_effectiveness + self.weight_complexity;
        if (weight_sum - 1.0).abs() > 1e-6 {
            return Err(format!(
                "ScorerConfig weights must sum to 1.0, got {weight_sum:.6}"
            ));
        }
        if self.top_percentile <= 0.0 || self.top_percentile > 1.0 {
            return Err(format!(
                "top_percentile must be in (0, 1], got {}",
                self.top_percentile
            ));
        }
        if self.window_duration <= Duration::zero() {
            return Err("window_duration must be positive".to_string());
        }
        Ok(())
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse all `gvu_generation` events from a single JSONL file.
///
/// Lines that fail to deserialise are skipped with a `debug!` log.
/// Non-`gvu_generation` events are silently ignored.
pub fn parse_events_from_file(path: &Path) -> Vec<TrajectoryEvent> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to read evolution events file {}: {e}", path.display());
            return Vec::new();
        }
    };

    let mut events = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<AuditEvent>(trimmed) {
            Ok(event) if event.event_type == AuditEventType::GvuGeneration => {
                if let Some(ev) = extract_trajectory_event(&event) {
                    events.push(ev);
                }
            }
            Ok(_) => {} // Other event types — silently skip
            Err(e) => {
                debug!(
                    file = %path.display(),
                    line = line_num + 1,
                    "Skipping malformed event line: {e}"
                );
            }
        }
    }

    debug!(
        file = %path.display(),
        count = events.len(),
        "Loaded gvu_generation events"
    );
    events
}

/// Parse events from all JSONL files in `dir` covering `lookback_days` days
/// (today inclusive).
///
/// Files that don't exist are silently skipped (no events yet for that day).
pub fn parse_events_from_dir(dir: &Path, lookback_days: u32) -> Vec<TrajectoryEvent> {
    let mut all_events = Vec::new();
    let now = Utc::now();

    for days_back in 0..=lookback_days {
        let date = now - Duration::days(i64::from(days_back));
        let filename = format!("{}.jsonl", date.format("%Y-%m-%d"));
        let path = dir.join(&filename);

        if path.exists() {
            let events = parse_events_from_file(&path);
            debug!(
                filename = %filename,
                count = events.len(),
                "Events loaded from JSONL"
            );
            all_events.extend(events);
        }
    }

    all_events
}

// ── Extraction helper ─────────────────────────────────────────────────────────

/// Convert an [`AuditEvent`] into a [`TrajectoryEvent`].
///
/// Returns `None` if the timestamp is unparseable (defensive guard).
/// Missing `metadata` fields fall back to safe defaults.
fn extract_trajectory_event(event: &AuditEvent) -> Option<TrajectoryEvent> {
    let timestamp = DateTime::parse_from_rfc3339(&event.timestamp)
        .ok()?
        .with_timezone(&Utc);

    let is_success = event.outcome == Outcome::Success;

    // Clamp to [0, 1] to guard against out-of-range values in metadata.
    let effectiveness_score_delta = event
        .metadata
        .get("effectiveness_score_delta")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);

    // Default to medium complexity (0.5) when not provided.
    let task_complexity = event
        .metadata
        .get("task_complexity")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);

    Some(TrajectoryEvent {
        timestamp,
        agent_id: event.agent_id.clone(),
        effectiveness_score_delta,
        task_complexity,
        is_success,
        skill_id: event.skill_id.clone(),
    })
}

// ── Grouping ──────────────────────────────────────────────────────────────────

/// Group a flat list of events into per-agent, per-time-window buckets.
///
/// Returns a `Vec<Vec<&TrajectoryEvent>>` — each inner vec is one bucket
/// (same agent, consecutive events within `config.window_duration`).
pub fn group_into_trajectories<'a>(
    events: &'a [TrajectoryEvent],
    config: &ScorerConfig,
) -> Vec<Vec<&'a TrajectoryEvent>> {
    if events.is_empty() {
        return Vec::new();
    }

    // Separate events by agent.
    let mut by_agent: HashMap<&str, Vec<&TrajectoryEvent>> = HashMap::new();
    for event in events {
        by_agent.entry(event.agent_id.as_str()).or_default().push(event);
    }

    let mut groups: Vec<Vec<&TrajectoryEvent>> = Vec::new();

    for agent_events in by_agent.into_values() {
        // Sort by timestamp within each agent bucket.
        let mut sorted = agent_events;
        sorted.sort_by_key(|e| e.timestamp);

        let mut window_start = sorted[0].timestamp;
        let mut current_window: Vec<&TrajectoryEvent> = Vec::new();

        for event in sorted {
            if event.timestamp - window_start > config.window_duration {
                // Close current window and start a new one.
                if !current_window.is_empty() {
                    groups.push(current_window);
                }
                window_start = event.timestamp;
                current_window = Vec::new();
            }
            current_window.push(event);
        }

        if !current_window.is_empty() {
            groups.push(current_window);
        }
    }

    groups
}

// ── Scoring ───────────────────────────────────────────────────────────────────

/// Calculate the composite quality score for a trajectory window.
///
/// Formula:
/// ```text
/// quality_score = success_rate × weight_success_rate
///               + avg_effectiveness × weight_effectiveness
///               + avg_complexity × weight_complexity
/// ```
///
/// Returns 0.0 for an empty slice.
pub fn calculate_quality_score(
    events: &[&TrajectoryEvent],
    config: &ScorerConfig,
) -> f64 {
    if events.is_empty() {
        return 0.0;
    }

    let n = events.len() as f64;

    let success_rate = events.iter().filter(|e| e.is_success).count() as f64 / n;

    // Weights already validated via ScorerConfig::validate.
    let avg_effectiveness = events
        .iter()
        .map(|e| e.effectiveness_score_delta)
        .sum::<f64>()
        / n;

    let avg_complexity = events.iter().map(|e| e.task_complexity).sum::<f64>() / n;

    let raw = config.weight_success_rate * success_rate
        + config.weight_effectiveness * avg_effectiveness
        + config.weight_complexity * avg_complexity;

    // Guard: clamp to [0,1] and replace NaN (possible if weights are modified
    // programmatically after bypassing validate()) with 0.0.
    if raw.is_finite() {
        raw.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Score all trajectory windows, sort by quality descending, and return the
/// top `config.top_percentile` fraction plus the total window count.
///
/// Returns `(top_trajectories, total_trajectory_count)`:
/// - `top_trajectories` — filtered top-N% results (minimum 1 if non-empty)
/// - `total_trajectory_count` — total number of windows before filtering
///
/// Returning both avoids callers re-running `group_into_trajectories` just to
/// obtain the unfiltered count (eliminates a redundant O(n) grouping pass).
pub fn score_and_filter(
    events: &[TrajectoryEvent],
    config: &ScorerConfig,
) -> (Vec<ScoredTrajectory>, usize) {
    let groups = group_into_trajectories(events, config);
    let total = groups.len();

    let mut scored: Vec<ScoredTrajectory> = groups
        .into_iter()
        .map(|group| {
            let agent_id = group[0].agent_id.clone();
            let window_start = group[0].timestamp;
            let window_end = group[group.len() - 1].timestamp;
            let event_count = group.len() as u32;
            let success_count = group.iter().filter(|e| e.is_success).count() as u32;
            let quality_score = calculate_quality_score(&group, config);
            let events: Vec<TrajectoryEvent> = group.into_iter().cloned().collect();

            ScoredTrajectory {
                agent_id,
                window_start,
                window_end,
                event_count,
                success_count,
                quality_score,
                events,
            }
        })
        .collect();

    // Sort descending by quality score; use agent_id as a tiebreaker for
    // deterministic output in tests.
    scored.sort_by(|a, b| {
        b.quality_score
            .partial_cmp(&a.quality_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });

    // Always keep at least 1 result (ceiling of top_percentile * total).
    let top_n = ((scored.len() as f64 * config.top_percentile).ceil() as usize).max(1);
    scored.truncate(top_n);

    (scored, total)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_event(
        agent_id: &str,
        hour_offset: i64,
        is_success: bool,
        effectiveness: f64,
        complexity: f64,
    ) -> TrajectoryEvent {
        TrajectoryEvent {
            timestamp: Utc.with_ymd_and_hms(2026, 4, 25, 0, 0, 0).unwrap()
                + Duration::hours(hour_offset),
            agent_id: agent_id.to_string(),
            effectiveness_score_delta: effectiveness,
            task_complexity: complexity,
            is_success,
            skill_id: None,
        }
    }

    fn default_config() -> ScorerConfig {
        ScorerConfig::default()
    }

    // ── ScorerConfig::validate ────────────────────────────────────────────────

    #[test]
    fn test_scorer_config_default_valid() {
        assert!(default_config().validate().is_ok());
    }

    #[test]
    fn test_scorer_config_weights_not_sum_to_one() {
        let cfg = ScorerConfig {
            weight_success_rate: 0.5,
            weight_effectiveness: 0.5,
            weight_complexity: 0.5,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("sum to 1.0"), "got: {err}");
    }

    #[test]
    fn test_scorer_config_invalid_top_percentile_zero() {
        let cfg = ScorerConfig {
            top_percentile: 0.0,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("top_percentile"), "got: {err}");
    }

    #[test]
    fn test_scorer_config_invalid_top_percentile_over_one() {
        let cfg = ScorerConfig {
            top_percentile: 1.1,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("top_percentile"), "got: {err}");
    }

    // ── calculate_quality_score ───────────────────────────────────────────────

    #[test]
    fn test_quality_score_all_success_max_values() {
        let events = vec![
            make_event("agent-a", 0, true, 1.0, 1.0),
            make_event("agent-a", 1, true, 1.0, 1.0),
        ];
        let refs: Vec<&TrajectoryEvent> = events.iter().collect();
        let score = calculate_quality_score(&refs, &default_config());
        // 1.0 * 0.40 + 1.0 * 0.35 + 1.0 * 0.25 = 1.0
        assert!((score - 1.0).abs() < 1e-9, "expected 1.0, got {score}");
    }

    #[test]
    fn test_quality_score_all_failure_zero_effectiveness() {
        let events = vec![
            make_event("agent-a", 0, false, 0.0, 0.0),
            make_event("agent-a", 1, false, 0.0, 0.0),
        ];
        let refs: Vec<&TrajectoryEvent> = events.iter().collect();
        let score = calculate_quality_score(&refs, &default_config());
        // 0 * 0.40 + 0 * 0.35 + 0 * 0.25 = 0.0
        assert!((score - 0.0).abs() < 1e-9, "expected 0.0, got {score}");
    }

    #[test]
    fn test_quality_score_mixed_trajectory() {
        // 2 success / 2 total = 0.5 success_rate
        // avg effectiveness = (0.8 + 0.6) / 2 = 0.7
        // avg complexity = (0.9 + 0.4) / 2 = 0.65
        // expected = 0.5*0.40 + 0.7*0.35 + 0.65*0.25 = 0.20 + 0.245 + 0.1625 = 0.6075
        let events = vec![
            make_event("agent-a", 0, true, 0.8, 0.9),
            make_event("agent-a", 1, false, 0.6, 0.4),
        ];
        let refs: Vec<&TrajectoryEvent> = events.iter().collect();
        let score = calculate_quality_score(&refs, &default_config());
        assert!((score - 0.6075).abs() < 1e-9, "expected 0.6075, got {score}");
    }

    #[test]
    fn test_quality_score_empty_returns_zero() {
        let score = calculate_quality_score(&[], &default_config());
        assert_eq!(score, 0.0);
    }

    // ── group_into_trajectories ───────────────────────────────────────────────

    #[test]
    fn test_group_same_agent_same_window() {
        let events = vec![
            make_event("agent-a", 0, true, 0.5, 0.5),
            make_event("agent-a", 2, true, 0.5, 0.5), // +2h, within 6h window
            make_event("agent-a", 4, true, 0.5, 0.5), // +4h, still in window
        ];
        let groups = group_into_trajectories(&events, &default_config());
        assert_eq!(groups.len(), 1, "All 3 events should be in one window");
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn test_group_same_agent_different_windows() {
        let events = vec![
            make_event("agent-a", 0, true, 0.5, 0.5),
            make_event("agent-a", 7, true, 0.5, 0.5), // +7h, exceeds 6h window
        ];
        let groups = group_into_trajectories(&events, &default_config());
        assert_eq!(groups.len(), 2, "Events 7h apart should be in separate windows");
    }

    #[test]
    fn test_group_different_agents_separate_windows() {
        let events = vec![
            make_event("agent-a", 0, true, 0.5, 0.5),
            make_event("agent-b", 0, true, 0.5, 0.5),
            make_event("agent-a", 1, true, 0.5, 0.5),
        ];
        let groups = group_into_trajectories(&events, &default_config());
        // agent-a: 1 window (2 events), agent-b: 1 window (1 event) = 2 groups
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn test_group_empty_events() {
        let groups = group_into_trajectories(&[], &default_config());
        assert!(groups.is_empty());
    }

    // ── score_and_filter — top N% selection ──────────────────────────────────

    #[test]
    fn test_score_and_filter_top_20_percent() {
        // Create 5 distinct agent trajectories with known scores.
        // Scores (approx) for all-success, effectiveness=X, complexity=0.5:
        // agent-a: 0.40 + 0.35*0.9 + 0.25*0.5 = 0.40 + 0.315 + 0.125 = 0.840
        // agent-b: 0.40 + 0.35*0.5 + 0.25*0.5 = 0.40 + 0.175 + 0.125 = 0.700
        // agent-c: 0.40 + 0.35*0.3 + 0.25*0.5 = 0.40 + 0.105 + 0.125 = 0.630
        // agent-d: 0.40 + 0.35*0.1 + 0.25*0.5 = 0.40 + 0.035 + 0.125 = 0.560
        // agent-e: 0.40 + 0.35*0.0 + 0.25*0.5 = 0.40 + 0.000 + 0.125 = 0.525
        let events = vec![
            make_event("agent-a", 0, true, 0.9, 0.5),
            make_event("agent-b", 0, true, 0.5, 0.5),
            make_event("agent-c", 0, true, 0.3, 0.5),
            make_event("agent-d", 0, true, 0.1, 0.5),
            make_event("agent-e", 0, true, 0.0, 0.5),
        ];
        let (result, total) = score_and_filter(&events, &default_config());
        // top 20% of 5 = ceil(1) = 1 trajectory
        assert_eq!(total, 5, "Total window count must be 5 (one per agent)");
        assert_eq!(result.len(), 1, "Top 20% of 5 should yield 1 trajectory");
        assert_eq!(result[0].agent_id, "agent-a", "Highest scoring agent should win");
        assert!((result[0].quality_score - 0.840).abs() < 1e-9);
    }

    #[test]
    fn test_score_and_filter_minimum_one_result() {
        // Even with 1 event, score_and_filter returns at least 1 result.
        let events = vec![make_event("agent-a", 0, true, 0.5, 0.5)];
        let (result, total) = score_and_filter(&events, &default_config());
        assert_eq!(total, 1);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_score_and_filter_empty_returns_empty() {
        let (result, total) = score_and_filter(&[], &default_config());
        assert!(result.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn test_score_and_filter_all_failures() {
        // All-failure trajectories should still be scored (score = 0.0 * 0.40 + ...).
        let events = vec![
            make_event("agent-a", 0, false, 0.0, 0.5),
            make_event("agent-b", 0, false, 0.0, 0.5),
        ];
        let (result, total) = score_and_filter(&events, &default_config());
        assert_eq!(total, 2, "Two separate agent windows");
        // top 20% of 2 = ceil(0.4) = 1
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].success_count, 0);
        // quality_score = 0.0*0.40 + 0.0*0.35 + 0.5*0.25 = 0.125
        assert!((result[0].quality_score - 0.125).abs() < 1e-9);
    }

    #[test]
    fn test_scored_trajectory_success_rate() {
        let events = vec![
            make_event("agent-a", 0, true, 0.5, 0.5),
            make_event("agent-a", 1, false, 0.5, 0.5),
        ];
        let (result, _total) = score_and_filter(&events, &default_config());
        assert_eq!(result.len(), 1);
        assert!((result[0].success_rate() - 0.5).abs() < 1e-9);
    }

    // ── parse_events_from_file ────────────────────────────────────────────────

    #[test]
    fn test_parse_events_from_file_valid_jsonl() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("2026-04-25.jsonl");

        // Write two valid gvu_generation events and one other event type.
        let lines = vec![
            r#"{"timestamp":"2026-04-25T01:00:00Z","event_type":"gvu_generation","agent_id":"agent-test","skill_id":null,"generation":1,"outcome":"success","trigger_signal":null,"metadata":{"effectiveness_score_delta":0.7,"task_complexity":0.8}}"#,
            r#"{"timestamp":"2026-04-25T02:00:00Z","event_type":"gvu_generation","agent_id":"agent-test","skill_id":null,"generation":2,"outcome":"failure","trigger_signal":null,"metadata":{}}"#,
            r#"{"timestamp":"2026-04-25T03:00:00Z","event_type":"skill_activate","agent_id":"agent-test","skill_id":"python-patterns","generation":null,"outcome":"success","trigger_signal":null,"metadata":{}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let events = parse_events_from_file(&path);
        assert_eq!(events.len(), 2, "Only gvu_generation events should be parsed");
        assert!(events[0].is_success);
        assert!((events[0].effectiveness_score_delta - 0.7).abs() < 1e-9);
        assert!((events[0].task_complexity - 0.8).abs() < 1e-9);
        assert!(!events[1].is_success);
        // Missing metadata fields fallback to defaults.
        assert_eq!(events[1].effectiveness_score_delta, 0.0);
        assert_eq!(events[1].task_complexity, 0.5);
    }

    #[test]
    fn test_parse_events_from_file_skips_malformed_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("2026-04-25.jsonl");

        let lines = vec![
            "not valid json",
            r#"{"timestamp":"2026-04-25T01:00:00Z","event_type":"gvu_generation","agent_id":"ok","skill_id":null,"generation":1,"outcome":"success","trigger_signal":null,"metadata":{}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let events = parse_events_from_file(&path);
        assert_eq!(events.len(), 1, "Malformed lines should be skipped gracefully");
    }

    #[test]
    fn test_parse_events_from_file_empty_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        let events = parse_events_from_file(&path);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_events_from_file_nonexistent() {
        let events = parse_events_from_file(Path::new("/nonexistent/path/2099-01-01.jsonl"));
        assert!(events.is_empty(), "Missing file should return empty vec, not panic");
    }

    #[test]
    fn test_parse_events_clamps_out_of_range_metadata() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("2026-04-25.jsonl");

        let line = r#"{"timestamp":"2026-04-25T01:00:00Z","event_type":"gvu_generation","agent_id":"agent","skill_id":null,"generation":1,"outcome":"success","trigger_signal":null,"metadata":{"effectiveness_score_delta":99.9,"task_complexity":-5.0}}"#;
        std::fs::write(&path, line).unwrap();

        let events = parse_events_from_file(&path);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].effectiveness_score_delta, 1.0, "Should clamp to 1.0");
        assert_eq!(events[0].task_complexity, 0.0, "Should clamp to 0.0");
    }

    // ── parse_events_from_dir ─────────────────────────────────────────────────

    #[test]
    fn test_parse_events_from_dir_loads_recent_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = Utc::now();

        // Write a file for today.
        let today = format!("{}.jsonl", now.format("%Y-%m-%d"));
        let line = r#"{"timestamp":"2026-04-25T01:00:00Z","event_type":"gvu_generation","agent_id":"agent-x","skill_id":null,"generation":1,"outcome":"success","trigger_signal":null,"metadata":{}}"#;
        std::fs::write(dir.path().join(&today), line).unwrap();

        let events = parse_events_from_dir(dir.path(), 1);
        assert_eq!(events.len(), 1, "Should load today's file");
    }

    #[test]
    fn test_parse_events_from_dir_missing_files_ok() {
        let dir = tempfile::TempDir::new().unwrap();
        // No files in directory.
        let events = parse_events_from_dir(dir.path(), 3);
        assert!(events.is_empty(), "Missing files should not panic");
    }
}
