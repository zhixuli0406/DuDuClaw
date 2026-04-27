//! Rollout-to-Skill Synthesis Pipeline — orchestration layer (W19-P0).
//!
//! Coordinates the full COSPLAY-inspired feedback loop:
//!
//! ```text
//! EvolutionEvents JSONL
//!       ↓
//!   QualityScorer (top-20% filter)
//!       ↓  [Phase 1: dry-run stops here]
//!   MemorySearch (episodic context)
//!       ↓
//!   SkillExtract (Haiku 4.5 — LLM mode)
//!       ↓
//!   SecurityScan (must pass ≥ 95%)
//!       ↓
//!   SkillGraduate → Skill Bank
//!       ↓
//!   Emit `skill_graduate` EvolutionEvent
//! ```
//!
//! ## Week-1 design (dry-run mode)
//! The first week of W19 runs in **dry-run mode**:
//! - Quality scores are computed and logged.
//! - No skills are written to the Skill Bank.
//! - This lets us validate the score distribution before enabling auto-write.
//!
//! Enable full graduation by setting `PipelineConfig::dry_run = false`.
//!
//! ## Error isolation
//! All failures are **non-blocking**: the pipeline captures errors into
//! [`PipelineRun::errors`] and continues. The main agent flow is never
//! interrupted by synthesis pipeline failures.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::{info, warn};

use super::quality_scorer::{parse_events_from_dir, score_and_filter, ScoredTrajectory, ScorerConfig};
use crate::evolution_events::schema::{AuditEvent, AuditEventType, Outcome};

// ── Config ─────────────────────────────────────────────────────────────────────

/// Configuration for the Rollout-to-Skill pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Directory containing `YYYY-MM-DD.jsonl` EvolutionEvents files.
    ///
    /// Defaults to `data/evolution/events/` relative to the current working
    /// directory. Can be overridden via `EVOLUTION_EVENTS_DIR` env var.
    pub events_dir: PathBuf,

    /// Number of days of JSONL history to scan (today + N-1 previous days).
    /// Default: 1 (today only).
    pub lookback_days: u32,

    /// Quality scorer configuration (top-20%, weights, window size).
    pub scorer_config: ScorerConfig,

    /// When `true` (Week 1 default): compute and log quality scores but do NOT
    /// graduate skills into the Skill Bank.
    pub dry_run: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        // Respect EVOLUTION_EVENTS_DIR env var; fall back to local path.
        let events_dir = std::env::var("EVOLUTION_EVENTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/evolution/events"));

        Self {
            events_dir,
            lookback_days: 1,
            scorer_config: ScorerConfig::default(),
            dry_run: true, // Safe default: dry-run until explicitly enabled
        }
    }
}

impl PipelineConfig {
    /// Validate the pipeline configuration.
    ///
    /// Returns `Ok(())` when valid, or a descriptive error string.
    pub fn validate(&self) -> Result<(), String> {
        self.scorer_config.validate()?;
        if self.lookback_days == 0 {
            return Err("lookback_days must be >= 1".to_string());
        }
        Ok(())
    }
}

// ── Results ───────────────────────────────────────────────────────────────────

/// Summary of one pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineRun {
    /// UTC timestamp when this run started.
    pub started_at: chrono::DateTime<Utc>,
    /// UTC timestamp when this run completed.
    pub completed_at: chrono::DateTime<Utc>,
    /// Whether this was a dry run (no Skill Bank writes).
    pub dry_run: bool,
    /// Total number of `gvu_generation` events parsed.
    pub total_events_parsed: usize,
    /// Number of trajectory windows evaluated.
    pub total_trajectories: usize,
    /// Trajectories that passed the top-20% quality threshold.
    pub top_trajectories: Vec<ScoredTrajectory>,
    /// Number of skills successfully graduated (0 in dry-run mode).
    pub skills_graduated: usize,
    /// Non-fatal errors encountered during the run.
    pub errors: Vec<String>,
}

impl PipelineRun {
    /// Human-readable summary for logs and activity feeds.
    pub fn summary(&self) -> String {
        if self.dry_run {
            format!(
                "[DRY RUN] Parsed {} events → {} trajectories → {} top-20% candidates (0 graduated, {} errors)",
                self.total_events_parsed,
                self.total_trajectories,
                self.top_trajectories.len(),
                self.errors.len()
            )
        } else {
            format!(
                "Parsed {} events → {} trajectories → {} top-20% candidates → {} skills graduated ({} errors)",
                self.total_events_parsed,
                self.total_trajectories,
                self.top_trajectories.len(),
                self.skills_graduated,
                self.errors.len()
            )
        }
    }
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// Run the Rollout-to-Skill synthesis pipeline.
///
/// In **dry-run mode** (the W19 Week-1 default):
/// 1. Parses JSONL event files.
/// 2. Scores and filters trajectories.
/// 3. Logs the quality distribution.
/// 4. Returns [`PipelineRun`] without writing to the Skill Bank.
///
/// In **full mode** (`dry_run = false`):
/// Steps 1-3 plus skill extraction, security scan, and graduation.
/// (Phase 2 — full implementation follows W19 Week-1 dry-run validation.)
///
/// This function is designed to be **non-blocking**: all errors are captured
/// into [`PipelineRun::errors`] rather than propagating. The caller is
/// responsible for logging the run summary.
pub async fn run(config: &PipelineConfig) -> PipelineRun {
    let started_at = Utc::now();
    let mut errors: Vec<String> = Vec::new();

    // ── Phase 0: Config validation ──────────────────────────────────────────

    if let Err(e) = config.validate() {
        errors.push(format!("Config validation failed: {e}"));
        return PipelineRun {
            started_at,
            completed_at: Utc::now(),
            dry_run: config.dry_run,
            total_events_parsed: 0,
            total_trajectories: 0,
            top_trajectories: Vec::new(),
            skills_graduated: 0,
            errors,
        };
    }

    // ── Phase 1: Parse JSONL events ─────────────────────────────────────────

    let events = parse_events_from_dir(&config.events_dir, config.lookback_days);
    let total_events_parsed = events.len();

    info!(
        events = total_events_parsed,
        events_dir = %config.events_dir.display(),
        lookback_days = config.lookback_days,
        "Rollout-to-Skill pipeline: events parsed"
    );

    if events.is_empty() {
        info!("No gvu_generation events found — pipeline run complete (nothing to process)");
        return PipelineRun {
            started_at,
            completed_at: Utc::now(),
            dry_run: config.dry_run,
            total_events_parsed: 0,
            total_trajectories: 0,
            top_trajectories: Vec::new(),
            skills_graduated: 0,
            errors,
        };
    }

    // ── Phase 2: Score and filter top-20% trajectories ──────────────────────

    // score_and_filter returns (top_slice, total_count) in one pass —
    // avoids a redundant second call to group_into_trajectories.
    let (top_trajectories, total_trajectories) =
        score_and_filter(&events, &config.scorer_config);

    info!(
        total = total_trajectories,
        top_n = top_trajectories.len(),
        "Top trajectories selected by quality scorer"
    );

    for (i, traj) in top_trajectories.iter().enumerate() {
        info!(
            rank = i + 1,
            agent = %traj.agent_id,
            score = %format!("{:.3}", traj.quality_score),
            success_rate = %format!("{:.1}%", traj.success_rate() * 100.0),
            events = traj.event_count,
            "Top trajectory"
        );
    }

    // ── Phase 3: Graduation (stub in dry-run mode) ───────────────────────────

    let skills_graduated = if config.dry_run {
        info!(
            count = top_trajectories.len(),
            "DRY RUN: would graduate {} skill(s) — skipping Skill Bank writes",
            top_trajectories.len()
        );
        0
    } else {
        // Phase 2 implementation (W19 Week 2+).
        // Full flow: memory_search → skill_extract → security_scan → skill_graduate
        // → emit skill_graduate EvolutionEvent.
        //
        // This is a planned stub — graduate_trajectories() will be implemented
        // after Week-1 dry-run score distribution is validated.
        warn!("Full graduation mode is not yet implemented — treating as dry-run");
        0
    };

    let run = PipelineRun {
        started_at,
        completed_at: Utc::now(),
        dry_run: config.dry_run,
        total_events_parsed,
        total_trajectories,
        top_trajectories,
        skills_graduated,
        errors,
    };

    info!(summary = %run.summary(), "Rollout-to-Skill pipeline run complete");
    run
}

// ── EvolutionEvent emitter ────────────────────────────────────────────────────

/// Build a `skill_graduate` [`AuditEvent`] for a successfully graduated trajectory.
///
/// Called by the Phase 2 graduation flow (after `skill_graduate` MCP write succeeds).
pub fn build_skill_graduate_event(
    agent_id: &str,
    skill_id: &str,
    quality_score: f64,
    source_trajectory_count: usize,
) -> AuditEvent {
    AuditEvent::now(AuditEventType::SkillGraduate, agent_id, Outcome::Success)
        .with_skill_id(skill_id)
        .with_trigger_signal("rollout_to_skill_pipeline")
        .with_metadata(serde_json::json!({
            "quality_score": quality_score,
            "source_trajectories": source_trajectory_count,
            "pipeline_version": "W19-P0"
        }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_test_jsonl(dir: &Path, filename: &str, lines: &[&str]) {
        fs::write(dir.join(filename), lines.join("\n")).unwrap();
    }

    // ── PipelineConfig ────────────────────────────────────────────────────────

    #[test]
    fn test_pipeline_config_default_valid() {
        assert!(PipelineConfig::default().validate().is_ok());
    }

    #[test]
    fn test_pipeline_config_zero_lookback_invalid() {
        let cfg = PipelineConfig {
            lookback_days: 0,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("lookback_days"), "got: {err}");
    }

    #[test]
    fn test_pipeline_config_default_is_dry_run() {
        assert!(
            PipelineConfig::default().dry_run,
            "Default config must be dry_run=true for safety"
        );
    }

    // ── run() — dry-run mode ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_dry_run_no_events() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = PipelineConfig {
            events_dir: dir.path().to_path_buf(),
            lookback_days: 1,
            dry_run: true,
            ..Default::default()
        };

        let run = run(&config).await;
        assert!(run.dry_run);
        assert_eq!(run.total_events_parsed, 0);
        assert_eq!(run.skills_graduated, 0);
        assert!(run.errors.is_empty());
    }

    #[tokio::test]
    async fn test_run_dry_run_with_events() {
        let dir = tempfile::TempDir::new().unwrap();
        let today = Utc::now().format("%Y-%m-%d").to_string();

        write_test_jsonl(
            dir.path(),
            &format!("{today}.jsonl"),
            &[
                r#"{"timestamp":"2026-04-25T01:00:00Z","event_type":"gvu_generation","agent_id":"agent-a","skill_id":null,"generation":1,"outcome":"success","trigger_signal":null,"metadata":{"effectiveness_score_delta":0.8,"task_complexity":0.7}}"#,
                r#"{"timestamp":"2026-04-25T02:00:00Z","event_type":"gvu_generation","agent_id":"agent-a","skill_id":null,"generation":2,"outcome":"success","trigger_signal":null,"metadata":{"effectiveness_score_delta":0.9,"task_complexity":0.8}}"#,
                r#"{"timestamp":"2026-04-25T03:00:00Z","event_type":"gvu_generation","agent_id":"agent-b","skill_id":null,"generation":1,"outcome":"failure","trigger_signal":null,"metadata":{}}"#,
            ],
        );

        let config = PipelineConfig {
            events_dir: dir.path().to_path_buf(),
            lookback_days: 1,
            dry_run: true,
            ..Default::default()
        };

        let run_result = run(&config).await;
        assert!(run_result.dry_run);
        assert_eq!(run_result.total_events_parsed, 3);
        assert!(run_result.total_trajectories >= 1);
        assert!(!run_result.top_trajectories.is_empty());
        assert_eq!(run_result.skills_graduated, 0, "Dry run must not graduate skills");
        assert!(run_result.errors.is_empty());
    }

    #[tokio::test]
    async fn test_run_invalid_config_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = PipelineConfig {
            events_dir: dir.path().to_path_buf(),
            lookback_days: 0, // Invalid
            dry_run: true,
            ..Default::default()
        };

        let run_result = run(&config).await;
        assert!(!run_result.errors.is_empty(), "Invalid config must produce errors");
        assert_eq!(run_result.total_events_parsed, 0);
    }

    // ── build_skill_graduate_event ────────────────────────────────────────────

    #[test]
    fn test_build_skill_graduate_event() {
        let ev = build_skill_graduate_event("agent-001", "python-patterns", 0.82, 3);
        assert_eq!(ev.event_type, AuditEventType::SkillGraduate);
        assert_eq!(ev.agent_id, "agent-001");
        assert_eq!(ev.skill_id.as_deref(), Some("python-patterns"));
        assert_eq!(ev.trigger_signal.as_deref(), Some("rollout_to_skill_pipeline"));
        assert_eq!(ev.outcome, Outcome::Success);
        assert!((ev.metadata["quality_score"].as_f64().unwrap() - 0.82).abs() < 1e-9);
        assert_eq!(ev.metadata["source_trajectories"].as_u64().unwrap(), 3);
        assert_eq!(ev.metadata["pipeline_version"].as_str().unwrap(), "W19-P0");
    }

    // ── PipelineRun::summary ──────────────────────────────────────────────────

    #[test]
    fn test_pipeline_run_summary_dry_run() {
        let run_result = PipelineRun {
            started_at: Utc::now(),
            completed_at: Utc::now(),
            dry_run: true,
            total_events_parsed: 100,
            total_trajectories: 20,
            top_trajectories: Vec::new(),
            skills_graduated: 0,
            errors: Vec::new(),
        };
        let summary = run_result.summary();
        assert!(summary.contains("DRY RUN"), "Summary must indicate dry run mode");
        assert!(summary.contains("100"), "Summary must include event count");
    }

    #[test]
    fn test_pipeline_run_summary_full_mode() {
        let run_result = PipelineRun {
            started_at: Utc::now(),
            completed_at: Utc::now(),
            dry_run: false,
            total_events_parsed: 50,
            total_trajectories: 10,
            top_trajectories: Vec::new(),
            skills_graduated: 2,
            errors: Vec::new(),
        };
        let summary = run_result.summary();
        assert!(!summary.contains("DRY RUN"), "Non-dry-run must not show DRY RUN");
        assert!(summary.contains("2 skills graduated"), "Must show graduated count");
    }
}
