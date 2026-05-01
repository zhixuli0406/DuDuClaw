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
use std::sync::Arc;

use chrono::Utc;
use tracing::{info, warn};

use super::quality_scorer::{parse_events_from_dir, score_and_filter, ScoredTrajectory, ScorerConfig};
use crate::evolution_events::emitter::EvolutionEventEmitter;
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

    // ── Phase 2 fields (required when dry_run = false) ────────────────────────

    /// Anthropic API key for Haiku 4.5 skill synthesis calls.
    ///
    /// Required when `dry_run = false`. When `None` in full mode, the pipeline
    /// captures a non-fatal error and skips graduation for all trajectories.
    pub api_key: Option<String>,

    /// DuDuClaw home directory (typically `~/.duduclaw`).
    ///
    /// Used to locate agent SKILLS directories and the global skill bank.
    /// Defaults to `~/.duduclaw` if not set.
    pub home_dir: PathBuf,

    /// Agent ID that will own synthesized skills before graduation.
    ///
    /// Defaults to `"duduclaw-eng-agent"`.
    pub target_agent_id: String,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        // Respect EVOLUTION_EVENTS_DIR env var; fall back to local path.
        let events_dir = std::env::var("EVOLUTION_EVENTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/evolution/events"));

        let home_dir = std::env::var("HOME")
            .map(|h: String| PathBuf::from(&h).join(".duduclaw"))
            .unwrap_or_else(|_| PathBuf::from("~/.duduclaw"));

        Self {
            events_dir,
            lookback_days: 1,
            scorer_config: ScorerConfig::default(),
            dry_run: true, // Safe default: dry-run until explicitly enabled
            api_key: None,
            home_dir,
            target_agent_id: "duduclaw-eng-agent".to_string(),
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
        if self.target_agent_id.is_empty() {
            return Err("target_agent_id must not be empty".to_string());
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

    // ── Phase 3: Graduation ───────────────────────────────────────────────────

    let skills_graduated = if config.dry_run {
        info!(
            count = top_trajectories.len(),
            "DRY RUN: would graduate {} skill(s) — skipping Skill Bank writes",
            top_trajectories.len()
        );
        0
    } else {
        let (graduated, grad_errors) =
            graduate_trajectories(&top_trajectories, config).await;
        errors.extend(grad_errors);
        graduated
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

// ── Phase 2: Graduation implementation ───────────────────────────────────────

/// Graduate high-quality trajectories into reusable skills via Haiku 4.5.
///
/// For each [`ScoredTrajectory`]:
/// 1. Build a [`SynthesisInput`] from trajectory metadata + agent context.
/// 2. Call Haiku 4.5 via [`call_direct_api`] to generate a SKILL.md.
/// 3. Parse and validate the synthesized skill.
/// 4. Run [`security_scanner::scan_skill`] (security gate).
/// 5. Write to agent SKILLS directory.
/// 6. Call [`graduation::graduate_to_global`] → Skill Bank.
/// 7. Emit `skill_graduate` [`EvolutionEvent`] (non-blocking).
///
/// Returns `(graduated_count, non_fatal_errors)`.
///
/// All failures are **non-blocking**: errors are collected and the pipeline
/// continues processing the next trajectory.
async fn graduate_trajectories(
    top_trajectories: &[ScoredTrajectory],
    config: &PipelineConfig,
) -> (usize, Vec<String>) {
    use crate::direct_api::call_direct_api;
    use crate::evolution_events::emitter::EvolutionEventEmitter;
    use crate::skill_lifecycle::{graduation, security_scanner, synthesizer};

    let api_key = match &config.api_key {
        Some(k) if !k.is_empty() => k.clone(),
        _ => {
            warn!("No API key configured — skipping Phase 2 graduation");
            return (0, vec!["Phase 2 skipped: api_key not configured".to_string()]);
        }
    };

    let agent_skills_dir = config
        .home_dir
        .join("agents")
        .join(&config.target_agent_id)
        .join("SKILLS");
    let global_skills_dir = config.home_dir.join("skills");

    // Collect existing skill names to prevent naming conflicts.
    let existing_skill_names: Vec<String> = std::fs::read_dir(&agent_skills_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .strip_suffix(".md")
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    // Load agent SOUL.md for context (prevents duplicate content).
    let agent_soul = std::fs::read_to_string(
        config
            .home_dir
            .join("agents")
            .join(&config.target_agent_id)
            .join("SOUL.md"),
    )
    .unwrap_or_default();

    let emitter = EvolutionEventEmitter::global();
    let mut graduated = 0usize;
    let mut errors: Vec<String> = Vec::new();

    // Track already-used names within this pipeline run to avoid collisions
    // between trajectories processed in the same batch.
    let mut used_names: Vec<String> = existing_skill_names.clone();

    for (idx, traj) in top_trajectories.iter().enumerate() {
        let span_prefix = format!(
            "[{}/{}] agent={} score={:.3}",
            idx + 1,
            top_trajectories.len(),
            traj.agent_id,
            traj.quality_score
        );

        // ── Step 1: Build SynthesisInput ─────────────────────────────────────

        // Derive a topic label from the agent_id + time window for evidence.
        let topic = format!(
            "{}-trajectory-{}",
            traj.agent_id,
            traj.window_start.format("%Y%m%d-%H%M")
        );

        let evidence: Vec<String> = traj
            .events
            .iter()
            .map(|e| {
                format!(
                    "agent={} success={} effectiveness_delta={:.2} complexity={:.2}",
                    e.agent_id, e.is_success, e.effectiveness_score_delta, e.task_complexity
                )
            })
            .collect();

        let synthesis_input = synthesizer::SynthesisInput {
            trigger: crate::skill_lifecycle::gap_accumulator::SynthesisTrigger {
                agent_id: traj.agent_id.clone(),
                topic: topic.clone(),
                gap_count: traj.event_count,
                evidence,
                avg_composite_error: 1.0 - traj.quality_score, // inverse of quality
            },
            successful_conversations: Vec::new(), // episodic lookup deferred to P1
            agent_soul: agent_soul.clone(),
            existing_skill_names: used_names.clone(),
        };

        // ── Step 2: Call Haiku 4.5 to synthesize skill ───────────────────────

        let prompt = synthesizer::build_synthesis_prompt(&synthesis_input);
        let system = "You are a skill designer for an AI agent system. Generate concise, \
            reusable SKILL.md files based on task execution patterns. \
            Output ONLY valid SKILL.md with YAML frontmatter.";

        let llm_result = call_direct_api(
            &api_key,
            "claude-haiku-4-5", // Cost-efficient model per W19 spec
            system,
            &prompt,
            &[],
        )
        .await;

        let llm_response = match llm_result {
            Ok(resp) => resp.text,
            Err(e) => {
                let msg = format!("{span_prefix} LLM call failed: {e}");
                warn!("{}", msg);
                errors.push(msg);
                continue;
            }
        };

        // ── Step 3: Parse and validate synthesized skill ─────────────────────

        let synthesized = match synthesizer::parse_synthesis_response(
            &llm_response,
            &synthesis_input.trigger,
            &used_names,
        ) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("{span_prefix} Synthesis parse failed: {e}");
                warn!("{}", msg);
                errors.push(msg);
                continue;
            }
        };

        // ── Step 4: Security scan (gate — must pass) ──────────────────────────

        let scan = security_scanner::scan_skill(&synthesized.full_markdown, None);
        if !scan.passed {
            let msg = format!(
                "{span_prefix} Security scan FAILED for '{}' (risk={:?}, {} findings)",
                synthesized.name,
                scan.risk_level,
                scan.findings.len()
            );
            warn!("{}", msg);
            errors.push(msg);
            continue;
        }

        info!(
            "{span_prefix} Security scan passed for '{}'",
            synthesized.name
        );

        // ── Step 5: Write skill to agent SKILLS directory ─────────────────────

        if let Err(e) = tokio::fs::create_dir_all(&agent_skills_dir).await {
            let msg = format!("{span_prefix} Failed to create SKILLS dir: {e}");
            warn!("{}", msg);
            errors.push(msg);
            continue;
        }

        let skill_path = agent_skills_dir.join(format!("{}.md", synthesized.name));
        if let Err(e) = tokio::fs::write(&skill_path, &synthesized.full_markdown).await {
            let msg = format!("{span_prefix} Failed to write skill file: {e}");
            warn!("{}", msg);
            errors.push(msg);
            continue;
        }

        // ── Step 6: Graduate to global Skill Bank ─────────────────────────────

        let candidate = graduation::GraduationCandidate {
            skill_name: synthesized.name.clone(),
            source_agent_id: config.target_agent_id.clone(),
            lift: traj.quality_score, // use quality score as proxy for lift
            load_count: u64::from(traj.event_count),
            is_stable: true,
            first_activated: traj.window_start,
        };

        match graduation::graduate_to_global(&candidate, &agent_skills_dir, &global_skills_dir)
            .await
        {
            Ok(record) => {
                let home_clone = config.home_dir.clone();
                let record_clone = record.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    graduation::append_graduation_log(&record_clone, &home_clone);
                })
                .await;

                info!(
                    skill = %synthesized.name,
                    score = %format!("{:.3}", traj.quality_score),
                    "{span_prefix} Graduated '{}' to global Skill Bank",
                    synthesized.name
                );

                // ── Step 7: Emit SkillGraduate EvolutionEvent ─────────────────

                emitter.emit_skill_graduate(
                    &config.target_agent_id,
                    &synthesized.name,
                    serde_json::json!({
                        "quality_score": traj.quality_score,
                        "source_trajectories": traj.event_count,
                        "pipeline_version": "W19-P0",
                        "source_agent": traj.agent_id,
                        "window_start": traj.window_start.to_rfc3339(),
                    }),
                );

                used_names.push(synthesized.name.clone());
                graduated += 1;
            }
            Err(e) => {
                // Clean up the skill file we wrote but failed to graduate.
                let _ = tokio::fs::remove_file(&skill_path).await;
                let msg = format!(
                    "{span_prefix} Graduation to global scope failed for '{}': {e}",
                    synthesized.name
                );
                warn!("{}", msg);
                errors.push(msg);
            }
        }
    }

    info!(
        graduated = graduated,
        errors = errors.len(),
        "Phase 2 graduation complete"
    );

    (graduated, errors)
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

    #[test]
    fn test_pipeline_config_empty_agent_id_invalid() {
        let cfg = PipelineConfig {
            target_agent_id: "".to_string(),
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("target_agent_id"), "got: {err}");
    }

    #[test]
    fn test_pipeline_config_default_has_no_api_key() {
        // Default config has no API key — safe for dry-run
        assert!(
            PipelineConfig::default().api_key.is_none(),
            "Default config must not have an API key set"
        );
    }

    #[test]
    fn test_pipeline_config_default_target_agent() {
        let cfg = PipelineConfig::default();
        assert_eq!(cfg.target_agent_id, "duduclaw-eng-agent");
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
