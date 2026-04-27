//! EvolutionEvent audit-log schema — Sprint N P0.
//!
//! Agnes-confirmed 8-field schema with 5 event types.
//! This schema is the authoritative source of truth for the JSONL audit log
//! written by [`super::logger::EvolutionEventLogger`].
//!
//! ## Reserved / future fields
//! - `intent_category` (`repair | optimize | innovate`) — P2 extension.
//!   Do NOT add to this struct until P2 is approved; document here to prevent
//!   accidental schema drift.

use serde::{Deserialize, Serialize};

// ── Event type ────────────────────────────────────────────────────────────────

/// The type of evolution event being recorded.
///
/// All five variants are defined here even though `signal_suppressed` is only
/// triggered in P1 — the schema must be stable before runtime code catches up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// A skill was activated for an agent.
    SkillActivate,
    /// A skill was deactivated for an agent.
    SkillDeactivate,
    /// A security scan was performed on a skill or SOUL.md.
    SecurityScan,
    /// A GVU (Generator-Verifier-Updater) generation cycle ran.
    GvuGeneration,
    /// A signal was suppressed due to stagnation detection (P1 reserved).
    ///
    /// ⚠️ Runtime trigger logic is not implemented in P0.
    /// The variant is defined here so the schema remains stable.
    SignalSuppressed,
    /// A skill was graduated from agent-local scope to global Skill Bank
    /// by the Rollout-to-Skill synthesis pipeline (W19-P0).
    ///
    /// Emitted after: quality_score passes top-20% threshold →
    /// security scan passes → `skill_graduate` MCP write succeeds.
    SkillGraduate,
}

impl std::fmt::Display for AuditEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Matches the snake_case serde serialisation.
        let s = match self {
            Self::SkillActivate => "skill_activate",
            Self::SkillDeactivate => "skill_deactivate",
            Self::SecurityScan => "security_scan",
            Self::GvuGeneration => "gvu_generation",
            Self::SignalSuppressed => "signal_suppressed",
            Self::SkillGraduate => "skill_graduate",
        };
        f.write_str(s)
    }
}

// ── Outcome ───────────────────────────────────────────────────────────────────

/// The result of the evolution action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The action completed successfully.
    Success,
    /// The action failed (see `metadata` for details).
    Failure,
    /// The action was intentionally suppressed (e.g. stagnation detection).
    Suppressed,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Suppressed => "suppressed",
        };
        f.write_str(s)
    }
}

// ── Audit event ───────────────────────────────────────────────────────────────

/// One row in the EvolutionEvents JSONL audit log.
///
/// Serialises to a single-line JSON object; the logger appends a `\n`
/// after each record so the file is valid JSONL.
///
/// All `Option` fields serialise as `null` when absent — this keeps the JSON
/// schema fixed-width (no missing keys) which simplifies downstream parsers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// RFC3339 / ISO8601 timestamp of when the event was recorded.
    pub timestamp: String,

    /// Which type of evolution action occurred.
    pub event_type: AuditEventType,

    /// The agent that triggered or was affected by the event.
    pub agent_id: String,

    /// The skill involved, if any.
    pub skill_id: Option<String>,

    /// GVU generation number (1-based), populated for `gvu_generation` events.
    pub generation: Option<i64>,

    /// Whether the action succeeded, failed, or was suppressed.
    pub outcome: Outcome,

    /// The upstream signal that triggered this event, e.g. `"prediction_error"`,
    /// `"manual_toggle"`, or `"heartbeat"`. `None` if not applicable.
    pub trigger_signal: Option<String>,

    /// Arbitrary structured metadata for diagnostics.
    ///
    /// Keep entries small (<1 KB) to avoid bloating the JSONL file.
    pub metadata: serde_json::Value,
}

impl AuditEvent {
    /// Construct a new event with the current UTC time.
    pub fn now(
        event_type: AuditEventType,
        agent_id: impl Into<String>,
        outcome: Outcome,
    ) -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event_type,
            agent_id: agent_id.into(),
            skill_id: None,
            generation: None,
            outcome,
            trigger_signal: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Builder: set the `skill_id`.
    pub fn with_skill_id(mut self, skill_id: impl Into<String>) -> Self {
        self.skill_id = Some(skill_id.into());
        self
    }

    /// Builder: set the GVU `generation` counter.
    pub fn with_generation(mut self, generation_num: i64) -> Self {
        self.generation = Some(generation_num);
        self
    }

    /// Builder: set the `trigger_signal`.
    pub fn with_trigger_signal(mut self, signal: impl Into<String>) -> Self {
        self.trigger_signal = Some(signal.into());
        self
    }

    /// Builder: set arbitrary `metadata`.
    pub fn with_metadata(mut self, meta: serde_json::Value) -> Self {
        self.metadata = meta;
        self
    }
}

// ── Stagnation detection config ───────────────────────────────────────────────

/// Runtime configuration for stagnation detection, parsed from the
/// `[evolution.stagnation_detection]` TOML section.
///
/// Validated by [`StagnationDetectionConfig::validate`] before being written
/// to `agent.toml` by the `evolution_toggle` handler.  This prevents illegal
/// values (e.g. `window_seconds = 0`) from corrupting the configuration that
/// P1 stagnation-detection logic depends on.
#[derive(Debug, Default, Clone)]
pub struct StagnationDetectionConfig {
    /// Whether stagnation detection is enabled.
    pub enabled: Option<bool>,
    /// Observation window in seconds.  Must be `> 0` when supplied.
    pub window_seconds: Option<u64>,
    /// Number of consecutive trigger events before suppression fires.
    /// Must be `> 0` when supplied.
    pub trigger_threshold: Option<u64>,
    /// Action taken on stagnation: `"log_only"` or `"suppress"`.
    pub action: Option<String>,
}

impl StagnationDetectionConfig {
    /// Validate that all numeric fields hold semantically meaningful values.
    ///
    /// Returns `Ok(())` when no illegal values are present (including when
    /// no values are set at all).  Returns `Err(description)` on the first
    /// violation found.
    ///
    /// # Invariants checked
    /// - `window_seconds`, if provided, must be `>= 1`
    /// - `trigger_threshold`, if provided, must be `>= 1`
    pub fn validate(&self) -> Result<(), String> {
        if let Some(ws) = self.window_seconds {
            if ws == 0 {
                return Err(
                    "stagnation_detection.window_seconds must be >= 1 (got 0)".into(),
                );
            }
        }
        if let Some(tt) = self.trigger_threshold {
            if tt == 0 {
                return Err(
                    "stagnation_detection.trigger_threshold must be >= 1 (got 0)".into(),
                );
            }
        }
        Ok(())
    }
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Validate an [`AuditEvent`] for semantic correctness.
///
/// Returns `Ok(())` if valid, or an error string describing the first
/// violation found.
pub fn validate(event: &AuditEvent) -> Result<(), String> {
    if event.agent_id.is_empty() {
        return Err("agent_id must not be empty".into());
    }
    if event.timestamp.is_empty() {
        return Err("timestamp must not be empty".into());
    }
    // Validate timestamp is parseable as RFC3339.
    chrono::DateTime::parse_from_rfc3339(&event.timestamp)
        .map_err(|e| format!("timestamp is not valid RFC3339: {e}"))?;

    // generation must be positive if present
    if let Some(generation_num) = event.generation {
        if generation_num < 1 {
            return Err(format!("generation must be >= 1, got {generation_num}"));
        }
    }

    // P0 constraint: signal_suppressed events must not carry a generation.
    if event.event_type == AuditEventType::SignalSuppressed && event.generation.is_some() {
        return Err(
            "signal_suppressed events must not carry a generation (P1 reserved)".into(),
        );
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> AuditEvent {
        AuditEvent::now(AuditEventType::SkillActivate, "agent-001", Outcome::Success)
            .with_skill_id("python-patterns")
            .with_trigger_signal("manual_toggle")
    }

    // ── Serialisation ──

    #[test]
    fn test_serialises_to_valid_json() {
        let ev = sample_event();
        let json = serde_json::to_string(&ev).expect("serialise");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed["event_type"], "skill_activate");
        assert_eq!(parsed["outcome"], "success");
        assert_eq!(parsed["agent_id"], "agent-001");
        assert_eq!(parsed["skill_id"], "python-patterns");
    }

    #[test]
    fn test_null_fields_are_present_in_json() {
        let ev = AuditEvent::now(AuditEventType::SecurityScan, "agent-002", Outcome::Success);
        let json = serde_json::to_string(&ev).expect("serialise");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        // Null optionals must be explicit keys (not absent).
        assert!(parsed.get("skill_id").is_some());
        assert_eq!(parsed["skill_id"], serde_json::Value::Null);
        assert!(parsed.get("generation").is_some());
        assert_eq!(parsed["generation"], serde_json::Value::Null);
    }

    #[test]
    fn test_all_event_types_serialise() {
        let types = [
            AuditEventType::SkillActivate,
            AuditEventType::SkillDeactivate,
            AuditEventType::SecurityScan,
            AuditEventType::GvuGeneration,
            AuditEventType::SignalSuppressed,
            AuditEventType::SkillGraduate,
        ];
        let expected = [
            "skill_activate",
            "skill_deactivate",
            "security_scan",
            "gvu_generation",
            "signal_suppressed",
            "skill_graduate",
        ];
        for (t, exp) in types.iter().zip(expected.iter()) {
            let ev = AuditEvent::now(t.clone(), "x", Outcome::Success);
            let json = serde_json::to_string(&ev).unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["event_type"], *exp, "mismatch for {t}");
        }
    }

    #[test]
    fn test_all_outcomes_serialise() {
        for (outcome, expected) in [
            (Outcome::Success, "success"),
            (Outcome::Failure, "failure"),
            (Outcome::Suppressed, "suppressed"),
        ] {
            let ev = AuditEvent::now(AuditEventType::SecurityScan, "x", outcome);
            let json = serde_json::to_string(&ev).unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["outcome"], expected);
        }
    }

    #[test]
    fn test_deserialises_from_json() {
        let json = r#"{
            "timestamp": "2026-04-25T00:00:00Z",
            "event_type": "gvu_generation",
            "agent_id": "duduclaw-main",
            "skill_id": null,
            "generation": 2,
            "outcome": "success",
            "trigger_signal": "prediction_error",
            "metadata": {}
        }"#;
        let ev: AuditEvent = serde_json::from_str(json).expect("deserialise");
        assert_eq!(ev.event_type, AuditEventType::GvuGeneration);
        assert_eq!(ev.generation, Some(2));
        assert_eq!(ev.outcome, Outcome::Success);
    }

    // ── Validation ──

    #[test]
    fn test_valid_event_passes() {
        let ev = sample_event();
        assert!(validate(&ev).is_ok());
    }

    #[test]
    fn test_empty_agent_id_fails() {
        let ev = AuditEvent::now(AuditEventType::SkillActivate, "", Outcome::Success);
        assert!(validate(&ev).is_err());
    }

    #[test]
    fn test_invalid_timestamp_fails() {
        let mut ev = sample_event();
        ev.timestamp = "not-a-date".into();
        let err = validate(&ev).unwrap_err();
        assert!(err.contains("RFC3339"), "got: {err}");
    }

    #[test]
    fn test_generation_zero_fails() {
        let ev = AuditEvent::now(AuditEventType::GvuGeneration, "a", Outcome::Success)
            .with_generation(0);
        let err = validate(&ev).unwrap_err();
        assert!(err.contains("generation must be >= 1"), "got: {err}");
    }

    #[test]
    fn test_signal_suppressed_with_generation_fails() {
        let ev = AuditEvent::now(AuditEventType::SignalSuppressed, "a", Outcome::Suppressed)
            .with_generation(1);
        let err = validate(&ev).unwrap_err();
        assert!(err.contains("P1 reserved"), "got: {err}");
    }

    #[test]
    fn test_signal_suppressed_without_generation_passes() {
        let ev =
            AuditEvent::now(AuditEventType::SignalSuppressed, "agent-x", Outcome::Suppressed);
        assert!(validate(&ev).is_ok());
    }

    // ── StagnationDetectionConfig::validate ──

    #[test]
    fn test_stagnation_config_empty_is_valid() {
        let cfg = StagnationDetectionConfig::default();
        assert!(cfg.validate().is_ok(), "empty config must be valid");
    }

    #[test]
    fn test_stagnation_config_valid_values() {
        let cfg = StagnationDetectionConfig {
            window_seconds: Some(300),
            trigger_threshold: Some(5),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_stagnation_window_seconds_zero_fails() {
        let cfg = StagnationDetectionConfig {
            window_seconds: Some(0),
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("window_seconds"),
            "error must mention window_seconds, got: {err}"
        );
    }

    #[test]
    fn test_stagnation_trigger_threshold_zero_fails() {
        let cfg = StagnationDetectionConfig {
            trigger_threshold: Some(0),
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("trigger_threshold"),
            "error must mention trigger_threshold, got: {err}"
        );
    }

    #[test]
    fn test_stagnation_both_zero_returns_first_error() {
        let cfg = StagnationDetectionConfig {
            window_seconds: Some(0),
            trigger_threshold: Some(0),
            ..Default::default()
        };
        // validate() returns the first error found; window_seconds is checked first.
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("window_seconds"), "got: {err}");
    }

    #[test]
    fn test_stagnation_config_enabled_only_is_valid() {
        let cfg = StagnationDetectionConfig {
            enabled: Some(true),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    // ── Display ──

    #[test]
    fn test_event_type_display() {
        assert_eq!(AuditEventType::SkillActivate.to_string(), "skill_activate");
        assert_eq!(AuditEventType::SignalSuppressed.to_string(), "signal_suppressed");
        assert_eq!(AuditEventType::SkillGraduate.to_string(), "skill_graduate");
    }

    // ── W19-P0: SkillGraduate event type ──

    #[test]
    fn test_skill_graduate_event_serialises() {
        let ev = AuditEvent::now(AuditEventType::SkillGraduate, "agent-001", Outcome::Success)
            .with_skill_id("python-patterns")
            .with_trigger_signal("rollout_to_skill_pipeline");
        let json = serde_json::to_string(&ev).expect("serialise");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed["event_type"], "skill_graduate");
        assert_eq!(parsed["skill_id"], "python-patterns");
        assert_eq!(parsed["trigger_signal"], "rollout_to_skill_pipeline");
    }

    #[test]
    fn test_skill_graduate_event_deserialises() {
        let json = r#"{
            "timestamp": "2026-04-25T00:00:00Z",
            "event_type": "skill_graduate",
            "agent_id": "duduclaw-eng-agent",
            "skill_id": "cosplay-extraction",
            "generation": null,
            "outcome": "success",
            "trigger_signal": "rollout_to_skill_pipeline",
            "metadata": {"quality_score": 0.82, "source_trajectories": 3}
        }"#;
        let ev: AuditEvent = serde_json::from_str(json).expect("deserialise");
        assert_eq!(ev.event_type, AuditEventType::SkillGraduate);
        assert_eq!(ev.skill_id.as_deref(), Some("cosplay-extraction"));
        assert_eq!(ev.metadata["quality_score"], 0.82);
    }

    #[test]
    fn test_skill_graduate_validation_passes() {
        let ev = AuditEvent::now(AuditEventType::SkillGraduate, "agent-001", Outcome::Success)
            .with_skill_id("some-skill");
        assert!(validate(&ev).is_ok(), "skill_graduate with valid fields must pass validation");
    }

    #[test]
    fn test_outcome_display() {
        assert_eq!(Outcome::Success.to_string(), "success");
        assert_eq!(Outcome::Suppressed.to_string(), "suppressed");
    }
}
