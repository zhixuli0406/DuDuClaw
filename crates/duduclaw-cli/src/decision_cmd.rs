//! Local synthetic support pilot and scoped replay with optional run storage.
//!
//! The database is private to its OS owner; tenant and ACL flags select an
//! exact stored scope but are not remote authentication credentials.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::approval::ApprovalBroker;
use duduclaw_gateway::decision_brief::{BriefEvidenceSelection, DecisionBrief, ReplayCommand};
use duduclaw_gateway::decision_calibration::{
    BacktestResult, CapacityFit, ForecastBacktestResult, IntervalDiagnostic, KnownDayInputs,
    backtest_capacity, backtest_one_step_forecast, diagnose_fixed_forecast_intervals,
    diagnose_forecast_intervals, fit_capacity,
};
use duduclaw_gateway::decision_empirical::{
    EmpiricalParameterFit, EmpiricalSamplingMode, EmpiricalSensitivityPlan,
    EmpiricalSensitivityReport, JointCapacityHoldoutReport, evaluate_joint_capacity_holdout,
    fit_empirical_parameters,
};
use duduclaw_gateway::decision_event::{
    EventQueueConfig, EventSimulationResult, simulate_ticket_events,
};
use duduclaw_gateway::decision_ingest::{
    SlaHoldoutDiagnostic, SlaHoldoutError, SupportPilotExport, build_support_pilot,
    evaluate_ticket_sla_holdout,
};
use duduclaw_gateway::decision_model_review::OutcomeModelReviewCriteria;
use duduclaw_gateway::decision_policy::{
    JointRiskScreenCriteria, JointRiskScreenReport, PolicySweepReport, StaffingResourcePlan,
};
use duduclaw_gateway::decision_sensitivity::{
    BoundedCount, SensitivityPlan, SensitivityReport, simulate_sensitivity,
};
use duduclaw_gateway::decision_shadow_screen::ShadowReviewCriteria;
use duduclaw_gateway::decision_sim::{QueueModel, SimulationResult, StaffingScenario, simulate};
use duduclaw_gateway::decision_store::{
    CausalInvalidationResult, CausalSourceRemoval, DecisionScope, DecisionStore,
    StoredEmpiricalRun, StoredForecastValidation, StoredSlaHoldout,
    validate_shadow_observation_source, validate_shadow_training_source,
};
use duduclaw_gateway::decision_synthetic::synthetic_support_export;
use duduclaw_memory::causal::{CausalStore, EvidenceScope};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub struct ReplayOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub snapshot: String,
    pub model: String,
    pub scenario: String,
    pub expected_hash: Option<String>,
}

pub struct BriefOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub snapshot: String,
    pub model: String,
    pub baseline: String,
    pub alternative: String,
    pub effect_ids: Vec<String>,
    pub assumptions: Vec<String>,
    pub empirical_run: Option<String>,
    pub policy_screen: Option<String>,
    pub forecast_validation: Option<String>,
    pub sla_holdout: Option<String>,
    pub baseline_event_hash: Option<String>,
    pub alternative_event_hash: Option<String>,
    pub source_file: Option<PathBuf>,
}

pub struct ForecastValidationOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub source_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub record: String,
    pub expected_sha256: Option<String>,
}

pub struct ShadowForecastOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub training_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub forecast_id: String,
    pub policy_id: String,
    pub target_day_utc: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
    pub opening_backlog: u64,
    pub planned_agents: u32,
    pub planned_fixed_extra_capacity: u32,
}

pub struct ShadowSlaForecastOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub opening_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub sla_id: String,
    pub forecast_id: String,
    pub model_version: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
}

pub struct ShadowSlaScoreOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub observation_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub score_id: String,
    pub sla_forecast_id: String,
    pub aggregate_score_id: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
}

pub struct ShadowSlaScoreCorrectionOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub observation_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub correction_id: String,
    pub sla_forecast_id: String,
    pub previous_revision_id: String,
    pub aggregate_revision_id: String,
    pub reviewer: String,
    pub reason: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
}

pub struct ShadowSlaScoreCurrentOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub sla_forecast_id: String,
}

pub struct ShadowPolicyOptions {
    pub db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub policy_id: String,
    pub source_lineage: String,
    pub queue_id: String,
    pub effective_from_utc: String,
    pub effective_until_utc: String,
    pub issue_deadline_seconds: u32,
    pub min_training_days: usize,
    pub min_saturated_days: usize,
}

pub struct ShadowPolicySupersedeOptions {
    pub db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub old_policy_id: String,
    pub new_policy_id: String,
    pub cutoff_utc: String,
    pub effective_until_utc: String,
    pub reviewer: String,
    pub issue_deadline_seconds: u32,
    pub min_training_days: usize,
    pub min_saturated_days: usize,
}

pub struct ShadowScoreOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub observation_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub score_id: String,
    pub forecast_id: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
}

pub struct ShadowScoreCorrectionOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub observation_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub correction_id: String,
    pub forecast_id: String,
    pub previous_revision_id: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
    pub reviewer: String,
    pub reason: String,
}

pub struct ShadowScoreCurrentOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub forecast_id: String,
}

pub struct ShadowPolicyAssessmentOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub policy_id: String,
}

pub struct ShadowPolicyScreenOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub policy_id: String,
    pub min_complete_days: usize,
    pub min_fixed_coverage_bps: u16,
    pub save_run: bool,
}

pub struct ShadowPolicyLoadScreenOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
}

pub struct ShadowScreenRequestReviewOptions {
    pub home: PathBuf,
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
    pub agent: String,
    pub summary: String,
    pub ttl_seconds: i64,
}

pub struct ShadowScreenCheckReviewOptions {
    pub home: PathBuf,
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
    pub approval: String,
}

pub struct StoreRunOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub snapshot: String,
    pub model: String,
    pub scenario: String,
}

pub struct RequestReviewOptions {
    pub home: PathBuf,
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
    pub agent: String,
    pub summary: String,
    pub ttl_seconds: i64,
}

pub struct CheckReviewOptions {
    pub home: PathBuf,
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
    pub approval: String,
}

pub struct RecordOutcomeOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub source_file: PathBuf,
    pub ticket_source_file: Option<PathBuf>,
    pub ticket_source_retention_until_utc: Option<String>,
    pub tenant: String,
    pub acl: String,
    pub observation: String,
    pub snapshot: String,
    pub model: String,
    pub scenario: String,
    pub expected_hash: String,
    pub recorded_by: String,
}

pub struct ScrubExpiredTicketSourcesOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
}

pub struct FitOutcomeOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub outcome: String,
    pub fit_id: String,
    pub training_days: usize,
    pub min_saturated_days: usize,
}

pub struct ScreenOutcomeModelOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub fit_id: String,
    pub min_saturated_days: usize,
    pub min_holdout_days: usize,
    pub save_run: bool,
}

pub struct LoadOutcomeModelScreenOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
}

pub struct OutcomeModelScreenRequestReviewOptions {
    pub home: PathBuf,
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
    pub agent: String,
    pub summary: String,
    pub ttl_seconds: i64,
}

pub struct OutcomeModelScreenCheckReviewOptions {
    pub home: PathBuf,
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
    pub approval: String,
}

pub struct ProposeOutcomeModelOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub screen_hash: String,
    pub candidate_id: String,
}

pub struct LoadOutcomeModelCandidateOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub candidate_id: String,
}

pub struct CompareOutcomeModelCandidateOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub candidate_id: String,
    pub target_snapshot: String,
    pub scenario: String,
    pub save_run: bool,
}

pub struct LoadOutcomeModelCandidateRunOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
}

pub struct ScoreOutcomeModelCandidateOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub comparison_run: String,
    pub outcome: String,
}

pub struct LoadOutcomeModelCandidateScoreOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
}

pub struct EventReplayOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub source_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub snapshot: String,
    pub model: String,
    pub scenario: String,
    pub window_start: String,
    pub baseline_scenario: String,
    pub shift_start_seconds: u32,
    pub shift_seconds: u32,
    pub expected_hash: Option<String>,
    pub save_run: bool,
}

pub struct EventLoadRunOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub source_file: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
}

pub struct EmpiricalReplayOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub fit: String,
    pub run_id: Option<String>,
    pub model: String,
    pub baseline: String,
    pub alternative: String,
    pub runs: usize,
    pub arrival_block_days: usize,
    pub paired_saturated_days: bool,
    pub max_final_backlog: u64,
    pub max_staff_cost_cents: u64,
    pub min_sla_resolved: Option<u64>,
    pub capacity_min: Option<u32>,
    pub capacity_max: Option<u32>,
    pub expected_hash: Option<String>,
}

pub struct EmpiricalScreenOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub run_id: String,
    pub resource_plan_file: PathBuf,
    pub max_joint_violation_bps: u32,
    pub min_joint_recovery_bps: u32,
    pub min_sla_improvement_bps: u32,
    pub expected_hash: Option<String>,
    pub save_run: bool,
}

pub struct EmpiricalLoadScreenOptions {
    pub db: PathBuf,
    pub causal_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub replay_hash: String,
}

pub struct DemoOptions {
    pub db: PathBuf,
    pub seed: u64,
    pub days: usize,
}

pub struct ImportPilotOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub export_file: PathBuf,
    pub model_file: PathBuf,
    pub source_output: PathBuf,
    pub tenant: String,
    pub acl: String,
    pub queue_id: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
}

#[derive(Serialize)]
pub struct ImportPilotReport {
    pub status: String,
    pub queue_id: String,
    pub source_file: String,
    pub source_sha256: String,
    pub source_artifact_id: String,
    pub snapshot_id: String,
    pub model_version: String,
    pub baseline_scenario_id: String,
    pub replay_hash: String,
    pub replay_command: ReplayCommand,
    pub sla_holdout: Option<SlaHoldoutDiagnostic>,
    pub sla_holdout_unavailable_reason: Option<String>,
    pub sla_holdout_id: Option<String>,
    pub sla_holdout_sha256: Option<String>,
    pub sla_holdout_command: Option<ReplayCommand>,
    pub limitation: String,
}

pub struct InvalidateSourceOptions {
    pub db: PathBuf,
    pub causal_db: PathBuf,
    pub ccr_db: Option<PathBuf>,
    pub tenant: String,
    pub acl: String,
    pub artifact: String,
}

#[derive(Serialize)]
pub struct DemoReport {
    pub status: String,
    pub seed: u64,
    pub days: usize,
    pub source_file: String,
    pub source_sha256: String,
    pub causal_db: String,
    pub source_artifact_id: String,
    pub capacity_fit: CapacityFit,
    pub empirical_fit_id: String,
    pub empirical_fit_sha256: String,
    pub empirical_fit: EmpiricalParameterFit,
    pub joint_capacity_holdout: JointCapacityHoldoutReport,
    pub sla_holdout: SlaHoldoutDiagnostic,
    pub sla_holdout_id: String,
    pub sla_holdout_sha256: String,
    pub sla_holdout_command: ReplayCommand,
    pub empirical_run_id: String,
    pub empirical_run_sha256: String,
    pub empirical_run: StoredEmpiricalRun,
    pub empirical_plan: EmpiricalSensitivityPlan,
    pub empirical_sensitivity: EmpiricalSensitivityReport,
    pub empirical_replay_command: ReplayCommand,
    pub backtest: BacktestResult,
    pub forecast_backtest: ForecastBacktestResult,
    pub interval_diagnostic: Option<IntervalDiagnostic>,
    pub fixed_interval_diagnostic: Option<IntervalDiagnostic>,
    pub forecast_validation_id: String,
    pub forecast_validation_sha256: String,
    pub forecast_validation_command: ReplayCommand,
    pub event_queue_config: EventQueueConfig,
    pub event_baseline: EventSimulationResult,
    pub event_alternative: EventSimulationResult,
    pub event_baseline_manifest_id: String,
    pub event_alternative_manifest_id: String,
    pub event_baseline_command: ReplayCommand,
    pub event_alternative_command: ReplayCommand,
    pub brief: DecisionBrief,
    pub sensitivity: SensitivityReport,
    pub policy: PolicySweepReport,
    pub joint_risk_screen: JointRiskScreenReport,
    pub joint_risk_screen_manifest_id: String,
    pub limitations: Vec<String>,
}

/// One immutable stored record plus whether the installed engine still
/// carries the identity that produced it. The flag is recomputed on every
/// read and stays *outside* `record` on purpose: `record` is the hashed
/// payload and must serialize byte-identically to what was stored, so a
/// derived, read-time fact can never be mistaken for part of it.
#[derive(Serialize)]
struct EngineStateReport<T: Serialize> {
    record: T,
    engine_matches_current: bool,
}

fn gateway_error(error: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Gateway(error.to_string())
}

fn canonical_utf8(path: &PathBuf) -> Result<String> {
    path.canonicalize()?
        .into_os_string()
        .into_string()
        .map_err(|_| DuDuClawError::Gateway("replay path is not UTF-8".into()))
}

fn event_command(options: &EventReplayOptions) -> Result<ReplayCommand> {
    let db = canonical_utf8(&options.db)?;
    let source_file = canonical_utf8(&options.source_file)?;
    let mut args = vec![
        "decision-event-replay".into(),
        "--db".into(),
        db,
        "--source-file".into(),
        source_file,
    ];
    if let Some(path) = &options.causal_db {
        args.push("--causal-db".into());
        args.push(canonical_utf8(path)?);
    }
    args.extend([
        "--tenant".into(),
        options.tenant.clone(),
        "--acl".into(),
        options.acl.clone(),
        "--snapshot".into(),
        options.snapshot.clone(),
        "--model".into(),
        options.model.clone(),
        "--scenario".into(),
        options.scenario.clone(),
        "--window-start".into(),
        options.window_start.clone(),
        "--baseline-scenario".into(),
        options.baseline_scenario.clone(),
        "--shift-start-seconds".into(),
        options.shift_start_seconds.to_string(),
        "--shift-seconds".into(),
        options.shift_seconds.to_string(),
    ]);
    if let Some(hash) = &options.expected_hash {
        args.push("--expected-hash".into());
        args.push(hash.clone());
    }
    Ok(ReplayCommand {
        program: "duduclaw".into(),
        args,
    })
}

fn empirical_command(options: &EmpiricalReplayOptions) -> Result<ReplayCommand> {
    let mut args = vec![
        "decision-empirical-replay".into(),
        "--db".into(),
        canonical_utf8(&options.db)?,
    ];
    if let Some(path) = &options.causal_db {
        args.extend(["--causal-db".into(), canonical_utf8(path)?]);
    }
    args.extend([
        "--tenant".into(),
        options.tenant.clone(),
        "--acl".into(),
        options.acl.clone(),
        "--fit".into(),
        options.fit.clone(),
        "--model".into(),
        options.model.clone(),
        "--baseline".into(),
        options.baseline.clone(),
        "--alternative".into(),
        options.alternative.clone(),
        "--runs".into(),
        options.runs.to_string(),
        "--arrival-block-days".into(),
        options.arrival_block_days.to_string(),
        "--max-final-backlog".into(),
        options.max_final_backlog.to_string(),
        "--max-staff-cost-cents".into(),
        options.max_staff_cost_cents.to_string(),
    ]);
    if options.paired_saturated_days {
        args.push("--paired-saturated-days".into());
    }
    if let Some(run_id) = &options.run_id {
        args.extend(["--run-id".into(), run_id.clone()]);
    }
    if let Some(target) = options.min_sla_resolved {
        args.extend(["--min-sla-resolved".into(), target.to_string()]);
    }
    if let (Some(min), Some(max)) = (options.capacity_min, options.capacity_max) {
        args.extend([
            "--capacity-min".into(),
            min.to_string(),
            "--capacity-max".into(),
            max.to_string(),
        ]);
    }
    if let Some(hash) = &options.expected_hash {
        args.extend(["--expected-hash".into(), hash.clone()]);
    }
    Ok(ReplayCommand {
        program: "duduclaw".into(),
        args,
    })
}

fn build_demo(options: &DemoOptions) -> Result<DemoReport> {
    if !(21..=366).contains(&options.days) {
        return Err(DuDuClawError::Config(
            "decision demo requires 21 to 366 days for rolling backtest".into(),
        ));
    }
    let source_path = options.db.with_extension("source.json");
    let causal_path = options.db.with_extension("causal.sqlite");
    if options.db == source_path
        || options.db == causal_path
        || source_path == causal_path
        || options.db.exists()
        || source_path.exists()
        || causal_path.exists()
    {
        return Err(DuDuClawError::Config(
            "decision demo requires new decision, causal, and source-file paths".into(),
        ));
    }
    let export = synthetic_support_export(options.seed, options.days).map_err(gateway_error)?;
    let pilot = build_support_pilot(&export).map_err(gateway_error)?;
    let fit = fit_capacity(&pilot.observed_days, 7).map_err(gateway_error)?;
    let empirical_fit = fit_empirical_parameters(&pilot.observed_days, options.days - 14, 7)
        .map_err(gateway_error)?;
    let joint_capacity_holdout =
        evaluate_joint_capacity_holdout(&pilot.observed_days, options.days - 14, 7)
            .map_err(gateway_error)?;
    let backtest = backtest_capacity(&pilot.observed_days, 14, 7).map_err(gateway_error)?;
    let forecast_backtest =
        backtest_one_step_forecast(&pilot.observed_days, 14, 7).map_err(gateway_error)?;
    let interval_diagnostic = if forecast_backtest.points.len() > 14 {
        Some(diagnose_forecast_intervals(&forecast_backtest, 14).map_err(gateway_error)?)
    } else {
        None
    };
    let fixed_interval_diagnostic = if forecast_backtest.points.len() > 14 {
        Some(diagnose_fixed_forecast_intervals(&forecast_backtest, 14).map_err(gateway_error)?)
    } else {
        None
    };
    let model = QueueModel {
        version: format!("synthetic-capacity-v1-{}-{}", options.seed, options.days),
        service_capacity_per_agent_day: fit.service_per_agent_day,
        sla_days: 2,
        staff_cost_cents_per_agent_day: 10_000,
    };
    let sla_holdout = evaluate_ticket_sla_holdout(&export, &model, options.days - 14, 7)
        .map_err(gateway_error)?;
    let alternative = StaffingScenario {
        id: format!("synthetic-three-agents-{}-{}", options.seed, options.days),
        agents_by_day: vec![3; options.days],
        fixed_extra_capacity_by_day: vec![0; options.days],
    };
    let baseline_result =
        simulate(&pilot.snapshot, &model, &pilot.baseline).map_err(gateway_error)?;
    let event_queue_config = EventQueueConfig {
        shift_start_seconds: 9 * 3_600,
        shift_seconds: 8 * 3_600,
    };
    let event_baseline =
        simulate_ticket_events(&export, &model, &pilot.baseline, &event_queue_config)
            .map_err(gateway_error)?;
    let event_alternative =
        simulate_ticket_events(&export, &model, &alternative, &event_queue_config)
            .map_err(gateway_error)?;
    if !baseline_result
        .days
        .iter()
        .zip(&pilot.observed_days)
        .all(|(predicted, observed)| {
            predicted.resolved == observed.resolved && predicted.backlog_end == observed.backlog_end
        })
    {
        return Err(DuDuClawError::Gateway(
            "synthetic fixture disagrees with queue engine".into(),
        ));
    }
    let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    if export.source_version_hashes != vec![source_sha256.clone()] {
        return Err(DuDuClawError::Gateway(
            "synthetic source version differs from exact source bytes".into(),
        ));
    }
    if let Some(parent) = options
        .db
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let db_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&options.db)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&options.db, std::fs::Permissions::from_mode(0o600))?;
    }
    drop(db_file);
    let mut source_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&source_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o600))?;
    }
    source_file.write_all(&source_bytes)?;
    source_file.sync_all()?;
    let scope = DecisionScope {
        tenant_id: "synthetic-demo".into(),
        acl: "private".into(),
    };
    let causal = CausalStore::new(&causal_path);
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let source_text = std::str::from_utf8(&source_bytes)
        .map_err(|_| DuDuClawError::Gateway("synthetic source is not UTF-8".into()))?;
    let source_artifact = causal
        .add_artifact(
            &evidence_scope,
            "synthetic_support_export",
            &format!("support-{}-{}", options.seed, options.days),
            &source_sha256,
            "synthetic_support_pilot",
            source_text,
            0,
            i64::MAX,
        )
        .map_err(gateway_error)?;
    let store = DecisionStore::with_causal_store(&options.db, causal);
    store
        .put_snapshot(&scope, &pilot.snapshot)
        .map_err(gateway_error)?;
    store
        .bind_causal_artifact(&scope, &pilot.snapshot.id, &source_artifact.id)
        .map_err(gateway_error)?;
    store.put_model(&scope, &model).map_err(gateway_error)?;
    store
        .put_scenario(&scope, &pilot.baseline)
        .map_err(gateway_error)?;
    store
        .put_scenario(&scope, &alternative)
        .map_err(gateway_error)?;
    let sla_holdout_id = format!("synthetic-sla-holdout-v3-{}-{}", options.seed, options.days,);
    let (stored_sla, sla_holdout_sha256) = store
        .put_sla_holdout(
            &scope,
            &sla_holdout_id,
            &pilot.snapshot.id,
            &model.version,
            &pilot.baseline.id,
            &source_bytes,
            &export.window_start_utc,
            options.days - 14,
            7,
        )
        .map_err(gateway_error)?;
    if stored_sla.diagnostic != sla_holdout {
        return Err(DuDuClawError::Gateway(
            "stored SLA holdout differs from synthetic report".into(),
        ));
    }
    let forecast_validation_id = format!(
        "synthetic-forecast-validation-v1-{}-{}",
        options.seed, options.days,
    );
    let (stored_forecast, forecast_validation_sha256) = store
        .put_forecast_validation(
            &scope,
            &forecast_validation_id,
            &pilot.snapshot.id,
            &source_bytes,
            &export.window_start_utc,
            &pilot.baseline.id,
            14,
            7,
            14,
        )
        .map_err(gateway_error)?;
    if stored_forecast.forecast != forecast_backtest
        || stored_forecast.rolling_interval != interval_diagnostic
        || stored_forecast.fixed_interval != fixed_interval_diagnostic
    {
        return Err(DuDuClawError::Gateway(
            "stored forecast validation differs from synthetic report".into(),
        ));
    }
    store
        .put_daily_run(
            &scope,
            &pilot.snapshot.id,
            &model.version,
            &pilot.baseline.id,
        )
        .map_err(gateway_error)?;
    store
        .put_daily_run(&scope, &pilot.snapshot.id, &model.version, &alternative.id)
        .map_err(gateway_error)?;
    let event_baseline_manifest = store
        .put_event_run(
            &scope,
            &pilot.snapshot.id,
            &model.version,
            &pilot.baseline.id,
            &source_bytes,
            &export.window_start_utc,
            &export.baseline_scenario_id,
            &event_queue_config,
        )
        .map_err(gateway_error)?;
    let event_alternative_manifest = store
        .put_event_run(
            &scope,
            &pilot.snapshot.id,
            &model.version,
            &alternative.id,
            &source_bytes,
            &export.window_start_utc,
            &export.baseline_scenario_id,
            &event_queue_config,
        )
        .map_err(gateway_error)?;
    if event_baseline_manifest.result != event_baseline
        || event_alternative_manifest.result != event_alternative
    {
        return Err(DuDuClawError::Gateway(
            "stored event run differs from synthetic replay".into(),
        ));
    }
    let empirical_fit_id = format!(
        "synthetic-empirical-fit-v1-{}-{}-train-{}",
        options.seed, options.days, empirical_fit.training_days,
    );
    let empirical_fit_sha256 = store
        .put_empirical_fit(
            &scope,
            &empirical_fit_id,
            &pilot.snapshot.id,
            &source_bytes,
            &export.window_start_utc,
            &export.baseline_scenario_id,
            &empirical_fit,
        )
        .map_err(gateway_error)?;
    let capacity_low = fit.service_per_agent_day.saturating_sub(1).max(1);
    let capacity_high = fit.service_per_agent_day.saturating_add(1);
    let staff_cost = (options.days as u64)
        .checked_mul(model.staff_cost_cents_per_agent_day)
        .ok_or_else(|| DuDuClawError::Gateway("synthetic staff cost overflow".into()))?;
    let sensitivity_plan = SensitivityPlan {
        runs: 100,
        daily_arrival_bands: pilot
            .snapshot
            .arrivals_by_day
            .iter()
            .map(|&arrivals| BoundedCount {
                min: arrivals.saturating_sub(2),
                max: arrivals.saturating_add(2),
            })
            .collect(),
        service_capacity_band: BoundedCount {
            min: capacity_low,
            max: capacity_high,
        },
        max_final_backlog: 20,
        max_staff_cost_cents: staff_cost
            .checked_mul(2)
            .ok_or_else(|| DuDuClawError::Gateway("synthetic budget overflow".into()))?,
    };
    let sensitivity = simulate_sensitivity(
        &pilot.snapshot,
        &model,
        &pilot.baseline,
        &alternative,
        &sensitivity_plan,
    )
    .map_err(gateway_error)?;
    let empirical_plan = EmpiricalSensitivityPlan {
        runs: 100,
        arrival_block_days: 7,
        sampling_mode: EmpiricalSamplingMode::Independent,
        capacity_fallback_range: None,
        max_final_backlog: 20,
        max_staff_cost_cents: staff_cost
            .checked_mul(3)
            .ok_or_else(|| DuDuClawError::Gateway("synthetic budget overflow".into()))?,
        min_sla_resolved: Some(
            (options.days as u64)
                .checked_mul(15)
                .ok_or_else(|| DuDuClawError::Gateway("synthetic SLA target overflow".into()))?,
        ),
    };
    let empirical_sensitivity = store
        .simulate_stored_empirical(
            &scope,
            &empirical_fit_id,
            &model.version,
            &pilot.baseline.id,
            &alternative.id,
            &empirical_plan,
        )
        .map_err(gateway_error)?;
    let empirical_run_id = format!(
        "synthetic-empirical-run-v1-{}-{}-train-{}",
        options.seed, options.days, empirical_fit.training_days,
    );
    let empirical_run_sha256 = store
        .put_empirical_run(
            &scope,
            &empirical_run_id,
            &empirical_fit_id,
            &model.version,
            &pilot.baseline.id,
            &alternative.id,
            &empirical_plan,
        )
        .map_err(gateway_error)?;
    let empirical_run = store
        .load_empirical_run(&scope, &empirical_run_id)
        .map_err(gateway_error)?;
    if empirical_run.report != empirical_sensitivity {
        return Err(DuDuClawError::Gateway(
            "stored empirical run differs from reported sensitivity".into(),
        ));
    }
    let brief = store
        .compare_scenarios_with_evidence(
            &scope,
            &pilot.snapshot.id,
            &model.version,
            &pilot.baseline.id,
            &alternative.id,
            BriefEvidenceSelection {
                empirical_run_id: Some(&empirical_run_id),
                policy_screen_hash: None,
                event_runs: Some((
                    &event_baseline_manifest.replay_hash,
                    &event_alternative_manifest.replay_hash,
                    &source_bytes,
                )),
                forecast_validation: Some((&forecast_validation_id, &source_bytes)),
                sla_holdout: Some((&sla_holdout_id, &source_bytes)),
                effect_ids: &[],
            },
            vec!["Fixed-seed synthetic arrivals, FIFO queue, and known service rule".into()],
        )
        .map_err(gateway_error)?;
    let policy_plan = StaffingResourcePlan {
        available_agents_by_day: vec![3; options.days],
        max_added_agents_per_day: 1,
        max_total_agent_days: (options.days as u64)
            .checked_mul(3)
            .ok_or_else(|| DuDuClawError::Gateway("synthetic agent-days overflow".into()))?,
        max_staff_cost_cents: staff_cost
            .checked_mul(3)
            .ok_or_else(|| DuDuClawError::Gateway("synthetic budget overflow".into()))?,
        max_final_backlog: 20,
        service_capacity_band: BoundedCount {
            min: capacity_low,
            max: capacity_high,
        },
    };
    let policy = store
        .sweep_staffing_policy(
            &scope,
            &pilot.snapshot.id,
            &model.version,
            &pilot.baseline.id,
            &alternative.id,
            &policy_plan,
        )
        .map_err(gateway_error)?;
    let joint_risk_screen_manifest = store
        .put_policy_screen(
            &scope,
            &empirical_run_id,
            &policy_plan,
            &JointRiskScreenCriteria {
                max_joint_violation_bps: 2_000,
                min_joint_recovery_bps: 5_000,
                min_sla_improvement_bps: 5_000,
            },
        )
        .map_err(gateway_error)?;
    let joint_risk_screen = joint_risk_screen_manifest.report.clone();
    if store
        .load_policy_screen(&scope, &joint_risk_screen_manifest.replay_hash)
        .map_err(gateway_error)?
        != joint_risk_screen_manifest
    {
        return Err(DuDuClawError::Gateway(
            "stored joint risk screen differs from report".into(),
        ));
    }
    if joint_risk_screen.policy_sweep != policy {
        return Err(DuDuClawError::Gateway(
            "joint risk screen disagrees with policy sweep".into(),
        ));
    }
    let source_file = source_path
        .canonicalize()?
        .into_os_string()
        .into_string()
        .map_err(|_| DuDuClawError::Gateway("source path is not UTF-8".into()))?;
    let causal_db = causal_path
        .canonicalize()?
        .into_os_string()
        .into_string()
        .map_err(|_| DuDuClawError::Gateway("causal database path is not UTF-8".into()))?;
    let decision_db = options
        .db
        .canonicalize()?
        .into_os_string()
        .into_string()
        .map_err(|_| DuDuClawError::Gateway("decision database path is not UTF-8".into()))?;
    let forecast_validation_command = ReplayCommand {
        program: "duduclaw".into(),
        args: vec![
            "decision-forecast-validation".into(),
            "--db".into(),
            decision_db,
            "--causal-db".into(),
            causal_db.clone(),
            "--source-file".into(),
            source_file.clone(),
            "--tenant".into(),
            scope.tenant_id.clone(),
            "--acl".into(),
            scope.acl.clone(),
            "--record".into(),
            forecast_validation_id.clone(),
            "--expected-sha256".into(),
            forecast_validation_sha256.clone(),
        ],
    };
    let sla_holdout_command = ReplayCommand {
        program: "duduclaw".into(),
        args: vec![
            "decision-sla-holdout".into(),
            "--db".into(),
            canonical_utf8(&options.db)?,
            "--causal-db".into(),
            causal_db.clone(),
            "--source-file".into(),
            source_file.clone(),
            "--tenant".into(),
            scope.tenant_id.clone(),
            "--acl".into(),
            scope.acl.clone(),
            "--record".into(),
            sla_holdout_id.clone(),
            "--expected-sha256".into(),
            sla_holdout_sha256.clone(),
        ],
    };
    let event_replay_options = |scenario: &str, expected_hash: &str| EventReplayOptions {
        db: options.db.clone(),
        causal_db: Some(causal_path.clone()),
        source_file: source_path.clone(),
        tenant: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
        snapshot: pilot.snapshot.id.clone(),
        model: model.version.clone(),
        scenario: scenario.into(),
        window_start: export.window_start_utc.clone(),
        baseline_scenario: export.baseline_scenario_id.clone(),
        shift_start_seconds: event_queue_config.shift_start_seconds,
        shift_seconds: event_queue_config.shift_seconds,
        expected_hash: Some(expected_hash.into()),
        save_run: false,
    };
    let event_baseline_command = event_command(&event_replay_options(
        &pilot.baseline.id,
        &event_baseline.replay_hash,
    ))?;
    let event_alternative_command = event_command(&event_replay_options(
        &alternative.id,
        &event_alternative.replay_hash,
    ))?;
    let empirical_replay_command = empirical_command(&EmpiricalReplayOptions {
        db: options.db.clone(),
        causal_db: Some(causal_path.clone()),
        tenant: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
        fit: empirical_fit_id.clone(),
        run_id: Some(empirical_run_id.clone()),
        model: model.version.clone(),
        baseline: pilot.baseline.id.clone(),
        alternative: alternative.id.clone(),
        runs: empirical_plan.runs,
        arrival_block_days: empirical_plan.arrival_block_days,
        paired_saturated_days: empirical_plan.sampling_mode
            == EmpiricalSamplingMode::PairedSaturatedDays,
        max_final_backlog: empirical_plan.max_final_backlog,
        max_staff_cost_cents: empirical_plan.max_staff_cost_cents,
        min_sla_resolved: empirical_plan.min_sla_resolved,
        capacity_min: empirical_plan
            .capacity_fallback_range
            .as_ref()
            .map(|range| range.min),
        capacity_max: empirical_plan
            .capacity_fallback_range
            .as_ref()
            .map(|range| range.max),
        expected_hash: Some(empirical_sensitivity.replay_hash.clone()),
    })?;
    Ok(DemoReport {
        status: "synthetic_engineering_fixture".into(),
        seed: options.seed,
        days: options.days,
        source_file,
        source_sha256,
        causal_db,
        source_artifact_id: source_artifact.id,
        capacity_fit: fit,
        empirical_fit_id,
        empirical_fit_sha256,
        empirical_fit,
        joint_capacity_holdout,
        sla_holdout,
        sla_holdout_id,
        sla_holdout_sha256,
        sla_holdout_command,
        empirical_run_id,
        empirical_run_sha256,
        empirical_run,
        empirical_plan,
        empirical_sensitivity,
        empirical_replay_command,
        backtest,
        forecast_backtest,
        interval_diagnostic,
        fixed_interval_diagnostic,
        forecast_validation_id,
        forecast_validation_sha256,
        forecast_validation_command,
        event_queue_config,
        event_baseline,
        event_alternative,
        event_baseline_manifest_id: event_baseline_manifest.replay_hash,
        event_alternative_manifest_id: event_alternative_manifest.replay_hash,
        event_baseline_command,
        event_alternative_command,
        brief,
        sensitivity,
        policy,
        joint_risk_screen,
        joint_risk_screen_manifest_id: joint_risk_screen_manifest.replay_hash,
        limitations: vec![
            "The generator uses the same FIFO service rule as the simulator; a zero synthetic backtest error is not real forecast validation".into(),
            "The one-step forecast knows opening backlog and scheduled staffing, but estimates arrivals only from prior days".into(),
            "When enough forecast points exist, rolling residual bands and drift signals are diagnostics on synthetic data, not calibrated prediction intervals".into(),
            "Demand, resource availability, and uncertainty bands are synthetic assumptions".into(),
            "The within-SLA resolution target of 15 per day is illustrative and has not been approved by a support operator".into(),
            "No staffing action is executed and no causal intervention effect is claimed".into(),
        ],
    })
}

pub fn demo(options: DemoOptions) -> Result<()> {
    let report = build_demo(&options)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn build_import_pilot(options: &ImportPilotOptions) -> Result<ImportPilotReport> {
    if [
        options.tenant.as_str(),
        options.acl.as_str(),
        options.queue_id.as_str(),
        options.source_lineage.as_str(),
    ]
    .iter()
    .any(|value| value.trim().is_empty())
    {
        return Err(DuDuClawError::Config(
            "pilot import requires tenant, ACL, queue ID, and source lineage".into(),
        ));
    }
    if options.queue_id.trim() != options.queue_id || options.queue_id.len() > 128 {
        return Err(DuDuClawError::Config("invalid expected queue ID".into()));
    }
    let retention = chrono::DateTime::parse_from_rfc3339(&options.retention_until_utc)
        .map_err(|_| DuDuClawError::Config("invalid retention UTC time".into()))?;
    if retention.offset().local_minus_utc() != 0
        || retention.timestamp() <= chrono::Utc::now().timestamp()
    {
        return Err(DuDuClawError::Config(
            "pilot source retention must be a future UTC time".into(),
        ));
    }
    let mut export_bytes = Vec::new();
    std::fs::File::open(&options.export_file)?
        .take(2 * 1024 * 1024 + 65_537)
        .read_to_end(&mut export_bytes)?;
    if export_bytes.len() > 2 * 1024 * 1024 + 65_536 {
        return Err(DuDuClawError::Config(
            "pilot export exceeds local source limit".into(),
        ));
    }
    let export: SupportPilotExport = serde_json::from_slice(&export_bytes)?;
    // `synthetic-support-` is reserved for the built-in fixture namespace. The
    // dashboard import path already refuses it (`decision_operator_import`);
    // the reserved prefix must have exactly one gate, not one per entry point.
    if export.snapshot_id.starts_with("synthetic-support-") {
        return Err(DuDuClawError::Config(
            "pilot snapshot ID must not use the reserved synthetic-support- prefix".into(),
        ));
    }
    let pilot = build_support_pilot(&export).map_err(gateway_error)?;
    let source_queue_id = duduclaw_gateway::decision_ingest::pilot_queue_id(&export)
        .map_err(gateway_error)?
        .ok_or_else(|| {
            DuDuClawError::Config(
                "new pilot imports require a queue_id on every ticket and staffing row".into(),
            )
        })?;
    if source_queue_id != options.queue_id {
        return Err(DuDuClawError::Config(
            "pilot source queue_id differs from the expected queue ID".into(),
        ));
    }
    let mut model_bytes = Vec::new();
    std::fs::File::open(&options.model_file)?
        .take(16_385)
        .read_to_end(&mut model_bytes)?;
    if model_bytes.len() > 16_384 {
        return Err(DuDuClawError::Config(
            "pilot model file exceeds 16 KiB".into(),
        ));
    }
    let model: QueueModel = serde_json::from_slice(&model_bytes)?;
    simulate(&pilot.snapshot, &model, &pilot.baseline).map_err(gateway_error)?;
    let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
    if source_bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Config(
            "pilot ticket/staffing source exceeds 2 MiB".into(),
        ));
    }
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    if export.source_version_hashes != vec![source_sha256.clone()] {
        return Err(DuDuClawError::Config(
            "pilot source version must equal the SHA-256 of canonical [tickets, staffing]".into(),
        ));
    }
    // Reserve at least seven complete days on each side of the split. An
    // unidentified training capacity is an explicit diagnostic limitation,
    // not a reason to reject an otherwise valid exploratory source import.
    let (sla_holdout, sla_holdout_unavailable_reason) = if export.horizon_days < 14 {
        (
            None,
            Some(
                "SLA holdout needs at least 14 complete days (seven training and seven later days)"
                    .into(),
            ),
        )
    } else {
        let holdout_days = (export.horizon_days / 3).max(7);
        let training_days = export.horizon_days - holdout_days;
        match evaluate_ticket_sla_holdout(&export, &model, training_days, 7) {
            Ok(report) => (Some(report), None),
            Err(SlaHoldoutError::Capacity(error)) => (
                None,
                Some(format!(
                    "SLA holdout training capacity is unidentified: {error}"
                )),
            ),
            Err(error) => return Err(gateway_error(error)),
        }
    };
    let cutoff = chrono::DateTime::parse_from_rfc3339(&export.data_cutoff_utc)
        .map_err(|_| DuDuClawError::Config("invalid pilot data cutoff".into()))?;
    if options.db == options.causal_db
        || options.db == options.source_output
        || options.causal_db == options.source_output
        || [&options.db, &options.causal_db, &options.source_output]
            .iter()
            .any(|path| path.exists())
    {
        return Err(DuDuClawError::Config(
            "pilot import requires distinct new database and source-output paths".into(),
        ));
    }
    for path in [&options.db, &options.causal_db, &options.source_output] {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut create = OpenOptions::new();
        create.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            create.mode(0o600);
        }
        let file = create.open(path)?;
        if path == &options.source_output {
            let mut file = file;
            file.write_all(&source_bytes)?;
            file.sync_all()?;
        }
    }
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let causal = CausalStore::new(&options.causal_db);
    let artifact = causal
        .add_artifact(
            &evidence_scope,
            "local_support_pilot_export",
            &options.source_lineage,
            &source_sha256,
            &options.source_lineage,
            std::str::from_utf8(&source_bytes)
                .map_err(|_| DuDuClawError::Config("pilot source is not UTF-8".into()))?,
            cutoff.timestamp(),
            retention.timestamp(),
        )
        .map_err(gateway_error)?;
    let store = DecisionStore::with_causal_store(&options.db, causal);
    store
        .put_snapshot(&scope, &pilot.snapshot)
        .map_err(gateway_error)?;
    store
        .bind_causal_artifact(&scope, &pilot.snapshot.id, &artifact.id)
        .map_err(gateway_error)?;
    store.put_model(&scope, &model).map_err(gateway_error)?;
    store
        .put_scenario(&scope, &pilot.baseline)
        .map_err(gateway_error)?;
    let saved_sla = if let Some(diagnostic) = &sla_holdout {
        let id = format!(
            "{}:sla-holdout-v3:{}:{}",
            pilot.snapshot.id, model.version, diagnostic.training_days,
        );
        let (record, digest) = store
            .put_sla_holdout(
                &scope,
                &id,
                &pilot.snapshot.id,
                &model.version,
                &pilot.baseline.id,
                &source_bytes,
                &export.window_start_utc,
                diagnostic.training_days,
                7,
            )
            .map_err(gateway_error)?;
        if record.diagnostic != *diagnostic {
            return Err(DuDuClawError::Gateway(
                "stored SLA holdout differs from import report".into(),
            ));
        }
        Some((id, digest))
    } else {
        None
    };
    let result = store
        .replay(
            &scope,
            &pilot.snapshot.id,
            &model.version,
            &pilot.baseline.id,
        )
        .map_err(gateway_error)?;
    let replay_command = ReplayCommand {
        program: "duduclaw".into(),
        args: vec![
            "decision-replay".into(),
            "--db".into(),
            canonical_utf8(&options.db)?,
            "--causal-db".into(),
            canonical_utf8(&options.causal_db)?,
            "--tenant".into(),
            scope.tenant_id.clone(),
            "--acl".into(),
            scope.acl.clone(),
            "--snapshot".into(),
            pilot.snapshot.id.clone(),
            "--model".into(),
            model.version.clone(),
            "--scenario".into(),
            pilot.baseline.id.clone(),
            "--expected-hash".into(),
            result.replay_hash.clone(),
        ],
    };
    let sla_holdout_command = if let Some((id, digest)) = saved_sla.as_ref() {
        Some(ReplayCommand {
            program: "duduclaw".into(),
            args: vec![
                "decision-sla-holdout".into(),
                "--db".into(),
                canonical_utf8(&options.db)?,
                "--causal-db".into(),
                canonical_utf8(&options.causal_db)?,
                "--source-file".into(),
                canonical_utf8(&options.source_output)?,
                "--tenant".into(),
                scope.tenant_id.clone(),
                "--acl".into(),
                scope.acl.clone(),
                "--record".into(),
                id.clone(),
                "--expected-sha256".into(),
                digest.clone(),
            ],
        })
    } else {
        None
    };
    Ok(ImportPilotReport {
        status: "exploratory_local_import".into(),
        queue_id: options.queue_id.clone(),
        source_file: canonical_utf8(&options.source_output)?,
        source_sha256,
        source_artifact_id: artifact.id,
        snapshot_id: pilot.snapshot.id,
        model_version: model.version,
        baseline_scenario_id: pilot.baseline.id,
        replay_hash: result.replay_hash,
        replay_command,
        sla_holdout,
        sla_holdout_unavailable_reason,
        sla_holdout_id: saved_sla.as_ref().map(|(id, _)| id.clone()),
        sla_holdout_sha256: saved_sla.as_ref().map(|(_, digest)| digest.clone()),
        sla_holdout_command,
        limitation: "Local file integrity and scope are checked; upstream identity, operator definitions, model calibration, and forecast skill remain unverified".into(),
    })
}

/// X1 方案 1 — `duduclaw decision-task-board-export`.
pub struct TaskBoardExportOptions {
    pub db: Option<PathBuf>,
    pub queue: String,
    pub horizon_days: usize,
    pub out: Option<PathBuf>,
}

/// X1 方案 4 — `duduclaw decision-odoo-export`.
pub struct OdooExportCliOptions {
    pub agent: String,
    pub profile: Option<String>,
    pub model: String,
    pub queue: i64,
    pub since: String,
    pub until: String,
    pub horizon_days: usize,
    pub out: Option<PathBuf>,
}

/// Write JSON to `out`, or stdout when it is `None`.
fn emit_json<T: Serialize>(value: &T, out: Option<&std::path::Path>) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    match out {
        Some(path) => {
            std::fs::write(path, text.as_bytes())?;
            println!("{}", path.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

/// Export the local task board as a decision-twin pilot.
///
/// Prints the whole envelope (export + provenance + limitations), not the bare
/// export: the staffing series is a proxy and the reader has to see that in the
/// same breath as the numbers.
pub fn task_board_export(options: TaskBoardExportOptions) -> Result<()> {
    use duduclaw_gateway::decision_task_board_export as tb;

    let home = duduclaw_core::duduclaw_home();
    let db = options.db.unwrap_or_else(|| home.join("tasks.db"));
    let queue = if options.queue.trim() == tb::ALL_QUEUE_AGENT {
        tb::TaskBoardQueue::All
    } else {
        tb::TaskBoardQueue::Agent(options.queue.trim().to_string())
    };
    let exported = tb::export_from_db(
        &home,
        &db,
        &tb::TaskBoardExportOptions {
            queue,
            horizon_days: options.horizon_days,
            window_start_utc: None,
            seed: 0,
        },
    )
    .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

    emit_json(
        &serde_json::json!({
            "queue_id": exported.queue_id,
            "source_lineage": exported.source_lineage,
            "tasks_db_sha256": exported.tasks_db_sha256,
            "contributing_agents": exported.contributing_agents,
            "export": exported.export,
            "limitations": [
                "staffing[].agents is agent.toml [heartbeat] max_concurrent_runs — a static proxy for staffed capacity",
                "Importing this pilot records an exploratory replay; it is not evidence of a staffing intervention effect",
            ],
        }),
        options.out.as_deref(),
    )
}

/// Export an Odoo helpdesk / project queue as a decision-twin pilot.
pub async fn odoo_export(options: OdooExportCliOptions) -> Result<()> {
    use duduclaw_gateway::decision_odoo_export as od;

    let home = duduclaw_core::duduclaw_home();
    let export = od::export_for_agent(
        &home,
        &options.agent,
        options.profile.as_deref(),
        &od::OdooExportRequest {
            model: options.model,
            queue: options.queue,
            since_utc: options.since,
            until_utc: options.until,
            horizon_days: options.horizon_days,
        },
    )
    .await
    .map_err(DuDuClawError::Gateway)?;
    emit_json(&export, options.out.as_deref())
}

pub fn import_pilot(options: ImportPilotOptions) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&build_import_pilot(&options)?)?
    );
    Ok(())
}

fn checked_replay(options: &ReplayOptions) -> Result<SimulationResult> {
    if !options.db.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision database does not exist".into(),
        ));
    }
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let store = match &options.causal_db {
        Some(path) if path.is_file() => {
            DecisionStore::with_causal_store(&options.db, CausalStore::new(path))
        }
        Some(_) => {
            return Err(DuDuClawError::Gateway(
                "causal source database does not exist".into(),
            ));
        }
        None => DecisionStore::new(&options.db),
    };
    let result = store
        .replay(&scope, &options.snapshot, &options.model, &options.scenario)
        .map_err(|error| DuDuClawError::Gateway(error.to_string()))?;
    if options
        .expected_hash
        .as_ref()
        .is_some_and(|expected| expected != &result.replay_hash)
    {
        return Err(DuDuClawError::Gateway(
            "decision replay hash differs from expected hash".into(),
        ));
    }
    Ok(result)
}

pub fn replay(options: ReplayOptions) -> Result<()> {
    let result = checked_replay(&options)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub(crate) fn build_brief(options: &BriefOptions) -> Result<DecisionBrief> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    if options.assumptions.is_empty() {
        return Err(DuDuClawError::Gateway(
            "decision brief requires assumptions".into(),
        ));
    }
    let mut source_bytes = Vec::new();
    let needs_source = options.forecast_validation.is_some()
        || options.sla_holdout.is_some()
        || options.baseline_event_hash.is_some()
        || options.alternative_event_hash.is_some();
    match (needs_source, options.source_file.as_ref()) {
        (true, Some(path)) => {
            std::fs::File::open(path)?
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut source_bytes)?;
            if source_bytes.len() > 2 * 1024 * 1024 {
                return Err(DuDuClawError::Gateway(
                    "brief source exceeds size limit".into(),
                ));
            }
        }
        (false, None) => {}
        _ => {
            return Err(DuDuClawError::Gateway(
                "forecast, SLA holdout, or event evidence requires an exact source file".into(),
            ));
        }
    }
    let event_runs = match (
        options.baseline_event_hash.as_deref(),
        options.alternative_event_hash.as_deref(),
    ) {
        (None, None) => None,
        (Some(baseline), Some(alternative)) if options.empirical_run.is_some() => {
            Some((baseline, alternative, source_bytes.as_slice()))
        }
        _ => {
            return Err(DuDuClawError::Gateway(
                "event evidence requires empirical run and both event hashes".into(),
            ));
        }
    };
    store
        .compare_scenarios_with_evidence(
            &scope,
            &options.snapshot,
            &options.model,
            &options.baseline,
            &options.alternative,
            BriefEvidenceSelection {
                empirical_run_id: options.empirical_run.as_deref(),
                policy_screen_hash: options.policy_screen.as_deref(),
                event_runs,
                forecast_validation: options
                    .forecast_validation
                    .as_deref()
                    .map(|id| (id, source_bytes.as_slice())),
                sla_holdout: options
                    .sla_holdout
                    .as_deref()
                    .map(|id| (id, source_bytes.as_slice())),
                effect_ids: &options.effect_ids,
            },
            options.assumptions.clone(),
        )
        .map_err(gateway_error)
}

pub fn brief(options: BriefOptions) -> Result<()> {
    let report = build_brief(&options)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn checked_forecast_validation(
    options: &ForecastValidationOptions,
) -> Result<StoredForecastValidation> {
    let store = review_store(&options.db, &options.causal_db)?;
    let mut source_bytes = Vec::new();
    std::fs::File::open(&options.source_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut source_bytes)?;
    if source_bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Gateway(
            "forecast source exceeds size limit".into(),
        ));
    }
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let record = store
        .load_forecast_validation(&scope, &options.record, &source_bytes)
        .map_err(gateway_error)?;
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_string(&record)?.as_bytes(),)
    );
    if options
        .expected_sha256
        .as_ref()
        .is_some_and(|expected| expected != &digest)
    {
        return Err(DuDuClawError::Gateway(
            "forecast validation digest differs from expected SHA-256".into(),
        ));
    }
    Ok(record)
}

pub fn forecast_validation(options: ForecastValidationOptions) -> Result<()> {
    let record = checked_forecast_validation(&options)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

fn checked_sla_holdout(options: &ForecastValidationOptions) -> Result<StoredSlaHoldout> {
    let store = review_store(&options.db, &options.causal_db)?;
    let mut source_bytes = Vec::new();
    std::fs::File::open(&options.source_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut source_bytes)?;
    if source_bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Gateway(
            "SLA holdout source exceeds size limit".into(),
        ));
    }
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let record = store
        .load_sla_holdout(&scope, &options.record, &source_bytes)
        .map_err(gateway_error)?;
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_string(&record)?.as_bytes())
    );
    if options
        .expected_sha256
        .as_ref()
        .is_some_and(|expected| expected != &digest)
    {
        return Err(DuDuClawError::Gateway(
            "SLA holdout digest differs from expected SHA-256".into(),
        ));
    }
    Ok(record)
}

pub fn sla_holdout(options: ForecastValidationOptions) -> Result<()> {
    let record = checked_sla_holdout(&options)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

pub fn store_run(options: StoreRunOptions) -> Result<()> {
    if !options.db.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision database does not exist".into(),
        ));
    }
    let store = match &options.causal_db {
        Some(path) if path.is_file() => {
            DecisionStore::with_causal_store(&options.db, CausalStore::new(path))
        }
        Some(_) => {
            return Err(DuDuClawError::Gateway(
                "causal source database does not exist".into(),
            ));
        }
        None => DecisionStore::new(&options.db),
    };
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let run = store
        .put_daily_run(&scope, &options.snapshot, &options.model, &options.scenario)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&run)?);
    Ok(())
}

fn review_store(db: &PathBuf, causal_db: &Option<PathBuf>) -> Result<DecisionStore> {
    if !db.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision database does not exist".into(),
        ));
    }
    match causal_db {
        Some(path) if path.is_file() => {
            Ok(DecisionStore::with_causal_store(db, CausalStore::new(path)))
        }
        Some(_) => Err(DuDuClawError::Gateway(
            "causal source database does not exist".into(),
        )),
        None => Ok(DecisionStore::new(db)),
    }
}

fn shadow_source_file(path: &PathBuf) -> Result<(Vec<u8>, String, String)> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Config(
            "shadow source must be nonempty and at most 2 MiB".into(),
        ));
    }
    let export: duduclaw_gateway::decision_store::ObservedOutcomeExport =
        serde_json::from_slice(&bytes)?;
    let queue_id = export.queue_id.ok_or_else(|| {
        DuDuClawError::Config("prospective shadow sources require a queue_id".into())
    })?;
    if queue_id.is_empty() || queue_id.trim() != queue_id || queue_id.len() > 128 {
        return Err(DuDuClawError::Config("invalid shadow queue_id".into()));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| DuDuClawError::Config("shadow source must be UTF-8".into()))?
        .to_owned();
    Ok((bytes, text, queue_id))
}

fn shadow_retention(value: &str, after: i64) -> Result<i64> {
    let time = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|_| DuDuClawError::Config("invalid shadow retention UTC time".into()))?;
    if time.offset().local_minus_utc() != 0
        || time.timestamp() <= chrono::Utc::now().timestamp()
        || time.timestamp() <= after
    {
        return Err(DuDuClawError::Config(
            "shadow retention must be UTC and after the observed day".into(),
        ));
    }
    Ok(time.timestamp())
}

pub fn shadow_policy(options: ShadowPolicyOptions) -> Result<()> {
    let store = review_store(&options.db, &None)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let policy = store
        .put_shadow_policy(
            &scope,
            &options.policy_id,
            &options.source_lineage,
            &options.queue_id,
            &options.effective_from_utc,
            &options.effective_until_utc,
            options.issue_deadline_seconds,
            options.min_training_days,
            options.min_saturated_days,
        )
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&policy)?);
    Ok(())
}

pub fn shadow_policy_supersede(options: ShadowPolicySupersedeOptions) -> Result<()> {
    let store = review_store(&options.db, &None)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let record = store
        .supersede_shadow_policy(
            &scope,
            &options.old_policy_id,
            &options.new_policy_id,
            &options.cutoff_utc,
            &options.effective_until_utc,
            &options.reviewer,
            options.issue_deadline_seconds,
            options.min_training_days,
            options.min_saturated_days,
        )
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

pub fn shadow_forecast(options: ShadowForecastOptions) -> Result<()> {
    let target = chrono::DateTime::parse_from_rfc3339(&options.target_day_utc)
        .map_err(|_| DuDuClawError::Config("invalid target UTC day".into()))?;
    let now = chrono::Utc::now().timestamp();
    if target.offset().local_minus_utc() != 0
        || target.time() != chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("midnight")
        || now < target.timestamp()
        || now >= target.timestamp() + 86_400
    {
        return Err(DuDuClawError::Config(
            "shadow forecast must be committed during its target UTC day".into(),
        ));
    }
    let store = review_store(&options.db, &Some(options.causal_db.clone()))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let policy = store
        .load_shadow_policy(&scope, &options.policy_id)
        .map_err(gateway_error)?;
    let effective_from = chrono::DateTime::parse_from_rfc3339(&policy.effective_from_utc)
        .map_err(|_| DuDuClawError::Config("stored policy start is invalid".into()))?;
    let effective_until = chrono::DateTime::parse_from_rfc3339(&policy.effective_until_utc)
        .map_err(|_| DuDuClawError::Config("stored policy end is invalid".into()))?;
    if policy.source_lineage != options.source_lineage
        || target < effective_from
        || target >= effective_until
        || now > target.timestamp() + policy.issue_deadline_seconds as i64
    {
        return Err(DuDuClawError::Config(
            "shadow target, lineage, or issue time differs from predeclared policy".into(),
        ));
    }
    let retention = shadow_retention(&options.retention_until_utc, target.timestamp() + 86_400)?;
    let (source_bytes, source_text, queue_id) = shadow_source_file(&options.training_file)?;
    if policy
        .queue_id
        .as_deref()
        .is_some_and(|expected| expected != queue_id.as_str())
    {
        return Err(DuDuClawError::Config(
            "shadow training queue ID differs from predeclared policy".into(),
        ));
    }
    let known = KnownDayInputs {
        opening_backlog: options.opening_backlog,
        planned_agents: options.planned_agents,
        planned_fixed_extra_capacity: options.planned_fixed_extra_capacity,
    };
    validate_shadow_training_source(&source_text, &options.target_day_utc, &policy, &known)
        .map_err(gateway_error)?;
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let causal = CausalStore::new(&options.causal_db);
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    let artifact = causal
        .add_artifact(
            &evidence_scope,
            "shadow_training_export",
            &options.source_lineage,
            &source_sha256,
            &options.source_lineage,
            &source_text,
            target.timestamp(),
            retention,
        )
        .map_err(gateway_error)?;
    let record = store
        .put_shadow_forecast(
            &scope,
            &options.forecast_id,
            &artifact.id,
            &options.target_day_utc,
            known,
            &options.policy_id,
        )
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source_artifact_id": artifact.id, "forecast": record,
        }))?
    );
    Ok(())
}

pub fn shadow_sla_forecast(options: ShadowSlaForecastOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db.clone()))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let forecast = store
        .load_shadow_forecast(&scope, &options.forecast_id)
        .map_err(gateway_error)?;
    let policy = store
        .load_shadow_policy(&scope, &forecast.policy_id)
        .map_err(gateway_error)?;
    let target = chrono::DateTime::parse_from_rfc3339(&forecast.target_day_utc)
        .map_err(|_| DuDuClawError::Config("stored SLA target day is invalid".into()))?;
    let now = chrono::Utc::now().timestamp();
    if options.source_lineage != forecast.source_lineage
        || now < forecast.committed_at
        || now > target.timestamp() + policy.issue_deadline_seconds as i64
        || now >= target.timestamp() + 86_400
    {
        return Err(DuDuClawError::Config(
            "SLA shadow target, lineage, or issue time differs from frozen forecast".into(),
        ));
    }
    let retention = shadow_retention(&options.retention_until_utc, target.timestamp() + 86_400)?;
    let mut source_bytes = Vec::new();
    std::fs::File::open(&options.opening_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut source_bytes)?;
    if source_bytes.is_empty() || source_bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Config(
            "SLA opening source must be nonempty and at most 2 MiB".into(),
        ));
    }
    let source_text = std::str::from_utf8(&source_bytes)
        .map_err(|_| DuDuClawError::Config("SLA opening source must be UTF-8".into()))?;
    store
        .preview_shadow_sla_forecast(
            &scope,
            &options.forecast_id,
            &options.model_version,
            source_text,
        )
        .map_err(gateway_error)?;
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    let causal = CausalStore::new(&options.causal_db);
    let artifact = causal
        .add_artifact(
            &evidence_scope,
            "shadow_sla_opening_export",
            &options.source_lineage,
            &source_sha256,
            &options.source_lineage,
            source_text,
            target.timestamp(),
            retention,
        )
        .map_err(gateway_error)?;
    let saved = store
        .put_shadow_sla_forecast(
            &scope,
            &options.sla_id,
            &options.forecast_id,
            &options.model_version,
            &artifact.id,
        )
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source_artifact_id": artifact.id, "sla_forecast": saved,
        }))?
    );
    Ok(())
}

pub fn shadow_sla_score(options: ShadowSlaScoreOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db.clone()))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let sla = store
        .load_shadow_sla_forecast(&scope, &options.sla_forecast_id)
        .map_err(gateway_error)?;
    let forecast = store
        .load_shadow_forecast(&scope, &sla.forecast_id)
        .map_err(gateway_error)?;
    let target = chrono::DateTime::parse_from_rfc3339(&sla.target_day_utc)
        .map_err(|_| DuDuClawError::Config("stored SLA target day is invalid".into()))?;
    let end = target.timestamp() + 86_400;
    if chrono::Utc::now().timestamp() < end || forecast.source_lineage != options.source_lineage {
        return Err(DuDuClawError::Config(
            "SLA observation day has not ended or source lineage differs".into(),
        ));
    }
    let retention = shadow_retention(&options.retention_until_utc, end)?;
    let mut source_bytes = Vec::new();
    std::fs::File::open(&options.observation_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut source_bytes)?;
    if source_bytes.is_empty() || source_bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Config(
            "SLA observation source must be nonempty and at most 2 MiB".into(),
        ));
    }
    let source_text = std::str::from_utf8(&source_bytes)
        .map_err(|_| DuDuClawError::Config("SLA observation source must be UTF-8".into()))?;
    store
        .preview_shadow_sla_score(
            &scope,
            &options.sla_forecast_id,
            &options.aggregate_score_id,
            source_text,
        )
        .map_err(gateway_error)?;
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    let causal = CausalStore::new(&options.causal_db);
    let artifact = causal
        .add_artifact(
            &evidence_scope,
            "shadow_sla_observation_export",
            &options.source_lineage,
            &source_sha256,
            &options.source_lineage,
            source_text,
            end,
            retention,
        )
        .map_err(gateway_error)?;
    let saved = store
        .put_shadow_sla_score(
            &scope,
            &options.score_id,
            &options.sla_forecast_id,
            &options.aggregate_score_id,
            &artifact.id,
        )
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source_artifact_id": artifact.id, "sla_score": saved,
        }))?
    );
    Ok(())
}

pub fn shadow_sla_score_correction(options: ShadowSlaScoreCorrectionOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db.clone()))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let sla = store
        .load_shadow_sla_forecast(&scope, &options.sla_forecast_id)
        .map_err(gateway_error)?;
    let forecast = store
        .load_shadow_forecast(&scope, &sla.forecast_id)
        .map_err(gateway_error)?;
    let target = chrono::DateTime::parse_from_rfc3339(&sla.target_day_utc)
        .map_err(|_| DuDuClawError::Config("stored SLA target day is invalid".into()))?;
    let end = target.timestamp() + 86_400;
    if chrono::Utc::now().timestamp() < end || forecast.source_lineage != options.source_lineage {
        return Err(DuDuClawError::Config(
            "SLA correction day has not ended or source lineage differs".into(),
        ));
    }
    let retention = shadow_retention(&options.retention_until_utc, end)?;
    let mut source_bytes = Vec::new();
    std::fs::File::open(&options.observation_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut source_bytes)?;
    if source_bytes.is_empty() || source_bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Config(
            "SLA correction source must be nonempty and at most 2 MiB".into(),
        ));
    }
    let source_text = std::str::from_utf8(&source_bytes)
        .map_err(|_| DuDuClawError::Config("SLA correction source must be UTF-8".into()))?;
    store
        .preview_shadow_sla_score_correction(
            &scope,
            &options.sla_forecast_id,
            &options.previous_revision_id,
            &options.aggregate_revision_id,
            source_text,
        )
        .map_err(gateway_error)?;
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    let causal = CausalStore::new(&options.causal_db);
    let artifact = causal
        .add_artifact(
            &evidence_scope,
            "shadow_sla_observation_export",
            &options.source_lineage,
            &source_sha256,
            &options.source_lineage,
            source_text,
            end,
            retention,
        )
        .map_err(gateway_error)?;
    let saved = store
        .put_shadow_sla_score_correction(
            &scope,
            &options.correction_id,
            &options.sla_forecast_id,
            &options.previous_revision_id,
            &options.aggregate_revision_id,
            &artifact.id,
            &options.reviewer,
            &options.reason,
        )
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source_artifact_id": artifact.id, "sla_score_correction": saved,
        }))?
    );
    Ok(())
}

pub fn shadow_sla_score_current(options: ShadowSlaScoreCurrentOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let score = store
        .load_current_shadow_sla_score(&scope, &options.sla_forecast_id)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&score)?);
    Ok(())
}

pub fn shadow_score(options: ShadowScoreOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db.clone()))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let forecast = store
        .load_shadow_forecast(&scope, &options.forecast_id)
        .map_err(gateway_error)?;
    let target = chrono::DateTime::parse_from_rfc3339(&forecast.target_day_utc)
        .map_err(|_| DuDuClawError::Config("stored target UTC day is invalid".into()))?;
    let end = target.timestamp() + 86_400;
    if chrono::Utc::now().timestamp() < end {
        return Err(DuDuClawError::Config(
            "shadow observation cannot be scored before day end".into(),
        ));
    }
    let retention = shadow_retention(&options.retention_until_utc, end)?;
    let (source_bytes, source_text, queue_id) = shadow_source_file(&options.observation_file)?;
    if forecast.source_lineage != options.source_lineage
        || forecast.queue_id.as_deref() != Some(queue_id.as_str())
    {
        return Err(DuDuClawError::Config(
            "shadow observation queue ID or lineage differs from forecast".into(),
        ));
    }
    validate_shadow_observation_source(&source_text, &forecast).map_err(gateway_error)?;
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let causal = CausalStore::new(&options.causal_db);
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    let artifact = causal
        .add_artifact(
            &evidence_scope,
            "shadow_observation_export",
            &options.source_lineage,
            &source_sha256,
            &options.source_lineage,
            &source_text,
            end,
            retention,
        )
        .map_err(gateway_error)?;
    let record = store
        .put_shadow_score(
            &scope,
            &options.score_id,
            &options.forecast_id,
            &artifact.id,
        )
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source_artifact_id": artifact.id, "score": record,
        }))?
    );
    Ok(())
}

pub fn shadow_score_correction(options: ShadowScoreCorrectionOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db.clone()))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let forecast = store
        .load_shadow_forecast(&scope, &options.forecast_id)
        .map_err(gateway_error)?;
    if forecast.source_lineage != options.source_lineage {
        return Err(DuDuClawError::Config(
            "shadow correction lineage differs from forecast".into(),
        ));
    }
    let target = chrono::DateTime::parse_from_rfc3339(&forecast.target_day_utc)
        .map_err(|_| DuDuClawError::Config("stored target UTC day is invalid".into()))?;
    let end = target.timestamp() + 86_400;
    if chrono::Utc::now().timestamp() < end {
        return Err(DuDuClawError::Config(
            "shadow score cannot be corrected before day end".into(),
        ));
    }
    let retention = shadow_retention(&options.retention_until_utc, end)?;
    let (source_bytes, source_text, queue_id) = shadow_source_file(&options.observation_file)?;
    if forecast.queue_id.as_deref() != Some(queue_id.as_str()) {
        return Err(DuDuClawError::Config(
            "shadow correction queue ID differs from forecast".into(),
        ));
    }
    validate_shadow_observation_source(&source_text, &forecast).map_err(gateway_error)?;
    let evidence_scope = EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    };
    let causal = CausalStore::new(&options.causal_db);
    let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
    let artifact = causal
        .add_artifact(
            &evidence_scope,
            "shadow_observation_export",
            &options.source_lineage,
            &source_sha256,
            &options.source_lineage,
            &source_text,
            end,
            retention,
        )
        .map_err(gateway_error)?;
    let record = store
        .put_shadow_score_correction(
            &scope,
            &options.correction_id,
            &options.forecast_id,
            &options.previous_revision_id,
            &artifact.id,
            &options.reviewer,
            &options.reason,
        )
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source_artifact_id": artifact.id, "correction": record,
        }))?
    );
    Ok(())
}

pub fn shadow_score_current(options: ShadowScoreCurrentOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let score = store
        .load_current_shadow_score(&scope, &options.forecast_id)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&score)?);
    Ok(())
}

pub fn shadow_policy_assess(options: ShadowPolicyAssessmentOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let report = store
        .assess_shadow_policy(&scope, &options.policy_id)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn shadow_sla_policy_assess(options: ShadowPolicyAssessmentOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let report = store
        .assess_shadow_sla_policy(&scope, &options.policy_id)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn shadow_policy_screen(options: ShadowPolicyScreenOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let criteria = ShadowReviewCriteria {
        min_complete_days: options.min_complete_days,
        min_fixed_coverage_bps: options.min_fixed_coverage_bps,
    };
    if options.save_run {
        let record = store
            .put_shadow_review_screen(&scope, &options.policy_id, &criteria)
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else {
        let report = store
            .screen_shadow_policy(&scope, &options.policy_id, &criteria)
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

pub fn shadow_sla_policy_screen(options: ShadowPolicyScreenOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let criteria = ShadowReviewCriteria {
        min_complete_days: options.min_complete_days,
        min_fixed_coverage_bps: options.min_fixed_coverage_bps,
    };
    if options.save_run {
        let record = store
            .put_sla_shadow_review_screen(&scope, &options.policy_id, &criteria)
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else {
        let report = store
            .screen_sla_shadow_policy(&scope, &options.policy_id, &criteria)
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

pub fn shadow_policy_load_screen(options: ShadowPolicyLoadScreenOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let record = store
        .load_shadow_review_screen(&scope, &options.replay_hash)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

pub fn shadow_sla_policy_load_screen(options: ShadowPolicyLoadScreenOptions) -> Result<()> {
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let record = store
        .load_sla_shadow_review_screen(&scope, &options.replay_hash)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

pub async fn shadow_screen_request_review(options: ShadowScreenRequestReviewOptions) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .request_shadow_screen_review(
            &broker,
            &scope,
            &options.replay_hash,
            &options.agent,
            &options.summary,
            options.ttl_seconds,
        )
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub async fn shadow_screen_check_review(options: ShadowScreenCheckReviewOptions) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .require_shadow_screen_review(&broker, &scope, &options.approval, &options.replay_hash)
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub async fn sla_shadow_screen_request_review(
    options: ShadowScreenRequestReviewOptions,
) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .request_sla_shadow_screen_review(
            &broker,
            &scope,
            &options.replay_hash,
            &options.agent,
            &options.summary,
            options.ttl_seconds,
        )
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub async fn sla_shadow_screen_check_review(options: ShadowScreenCheckReviewOptions) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &Some(options.causal_db))?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .require_sla_shadow_screen_review(&broker, &scope, &options.approval, &options.replay_hash)
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub async fn request_review(options: RequestReviewOptions) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &options.causal_db)?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .request_pilot_review(
            &broker,
            &scope,
            &options.replay_hash,
            &options.agent,
            &options.summary,
            options.ttl_seconds,
        )
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub async fn check_review(options: CheckReviewOptions) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &options.causal_db)?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .require_pilot_review(&broker, &scope, &options.approval, &options.replay_hash)
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub fn record_outcome(options: RecordOutcomeOptions) -> Result<()> {
    if !options.db.is_file() || !options.source_file.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision database or observed outcome export does not exist".into(),
        ));
    }
    let store = match &options.causal_db {
        Some(path) if path.is_file() => {
            DecisionStore::with_causal_store(&options.db, CausalStore::new(path))
        }
        Some(_) => {
            return Err(DuDuClawError::Gateway(
                "causal source database does not exist".into(),
            ));
        }
        None => DecisionStore::new(&options.db),
    };
    let mut source_bytes = Vec::new();
    std::fs::File::open(&options.source_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut source_bytes)?;
    if source_bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Gateway(
            "observed outcome export exceeds 2 MiB".into(),
        ));
    }
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let saved = if let Some(path) = &options.ticket_source_file {
        let retention = options
            .ticket_source_retention_until_utc
            .as_deref()
            .ok_or_else(|| {
                DuDuClawError::Config(
                    "--ticket-source-retention-until-utc is required with --ticket-source-file"
                        .into(),
                )
            })?;
        let mut ticket_bytes = Vec::new();
        std::fs::File::open(path)?
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut ticket_bytes)?;
        if ticket_bytes.is_empty() || ticket_bytes.len() > 2 * 1024 * 1024 {
            return Err(DuDuClawError::Gateway(
                "ticket source must be nonempty and at most 2 MiB".into(),
            ));
        }
        store.record_observed_outcome_with_ticket_source(
            &scope,
            &options.observation,
            &options.snapshot,
            &options.model,
            &options.scenario,
            &options.expected_hash,
            &options.recorded_by,
            &source_bytes,
            &ticket_bytes,
            retention,
        )
    } else {
        if options.ticket_source_retention_until_utc.is_some() {
            return Err(DuDuClawError::Config(
                "ticket source retention requires --ticket-source-file".into(),
            ));
        }
        store.record_observed_outcome(
            &scope,
            &options.observation,
            &options.snapshot,
            &options.model,
            &options.scenario,
            &options.expected_hash,
            &options.recorded_by,
            &source_bytes,
        )
    }
    .map_err(gateway_error)?;
    let engine_matches_current = store
        .load_daily_run_with_engine_state(&scope, &saved.replay_hash)
        .map_err(gateway_error)?
        .engine_matches_current;
    println!(
        "{}",
        serde_json::to_string_pretty(&EngineStateReport {
            record: &saved,
            engine_matches_current,
        })?
    );
    Ok(())
}

pub fn scrub_expired_ticket_sources(options: ScrubExpiredTicketSourcesOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let scrubbed = store
        .scrub_expired_ticket_sources(&scope)
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "scrubbed_ticket_sources": scrubbed,
        }))?
    );
    Ok(())
}

pub fn fit_outcome(options: FitOutcomeOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let fit = store
        .put_outcome_calibration(
            &scope,
            &options.fit_id,
            &options.outcome,
            options.min_saturated_days,
            options.training_days,
        )
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&fit)?);
    Ok(())
}

pub fn screen_outcome_model(options: ScreenOutcomeModelOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let criteria = OutcomeModelReviewCriteria {
        min_saturated_days: options.min_saturated_days,
        min_holdout_days: options.min_holdout_days,
    };
    if options.save_run {
        let screen = store
            .put_outcome_model_review_screen(&scope, &options.fit_id, &criteria)
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&screen)?);
    } else {
        let screen = store
            .screen_outcome_model_candidate(&scope, &options.fit_id, &criteria)
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&screen)?);
    }
    Ok(())
}

pub fn load_outcome_model_screen(options: LoadOutcomeModelScreenOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let screen = store
        .load_outcome_model_review_screen(&scope, &options.replay_hash)
        .map_err(gateway_error)?;
    let engine_matches_current = store
        .load_daily_run_with_engine_state(&scope, &screen.report.replayed_run_hash)
        .map_err(gateway_error)?
        .engine_matches_current;
    println!(
        "{}",
        serde_json::to_string_pretty(&EngineStateReport {
            record: &screen,
            engine_matches_current,
        })?
    );
    Ok(())
}

pub async fn outcome_model_screen_request_review(
    options: OutcomeModelScreenRequestReviewOptions,
) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &options.causal_db)?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .request_outcome_model_review(
            &broker,
            &scope,
            &options.replay_hash,
            &options.agent,
            &options.summary,
            options.ttl_seconds,
        )
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub async fn outcome_model_screen_check_review(
    options: OutcomeModelScreenCheckReviewOptions,
) -> Result<()> {
    if !options.home.is_dir() {
        return Err(DuDuClawError::Gateway(
            "approval home directory does not exist".into(),
        ));
    }
    let store = review_store(&options.db, &options.causal_db)?;
    let broker = ApprovalBroker::open(&options.home).map_err(gateway_error)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let link = store
        .require_outcome_model_review(&broker, &scope, &options.approval, &options.replay_hash)
        .await
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&link)?);
    Ok(())
}

pub fn propose_outcome_model(options: ProposeOutcomeModelOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let candidate = store
        .put_outcome_model_candidate(&scope, &options.candidate_id, &options.screen_hash)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&candidate)?);
    Ok(())
}

pub fn load_outcome_model_candidate(options: LoadOutcomeModelCandidateOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let candidate = store
        .load_outcome_model_candidate(&scope, &options.candidate_id)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&candidate)?);
    Ok(())
}

pub fn compare_outcome_model_candidate(options: CompareOutcomeModelCandidateOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    if options.save_run {
        let run = store
            .put_outcome_model_candidate_run(
                &scope,
                &options.candidate_id,
                &options.target_snapshot,
                &options.scenario,
            )
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&run)?);
    } else {
        let comparison = store
            .compare_outcome_model_candidate(
                &scope,
                &options.candidate_id,
                &options.target_snapshot,
                &options.scenario,
            )
            .map_err(gateway_error)?;
        println!("{}", serde_json::to_string_pretty(&comparison)?);
    }
    Ok(())
}

pub fn load_outcome_model_candidate_run(
    options: LoadOutcomeModelCandidateRunOptions,
) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let run = store
        .load_outcome_model_candidate_run(&scope, &options.replay_hash)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&run)?);
    Ok(())
}

pub fn score_outcome_model_candidate(options: ScoreOutcomeModelCandidateOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let score = store
        .put_outcome_model_candidate_score(&scope, &options.comparison_run, &options.outcome)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&score)?);
    Ok(())
}

pub fn load_outcome_model_candidate_score(
    options: LoadOutcomeModelCandidateScoreOptions,
) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let score = store
        .load_outcome_model_candidate_score(&scope, &options.replay_hash)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&score)?);
    Ok(())
}

fn checked_empirical_replay(
    options: &EmpiricalReplayOptions,
) -> Result<EmpiricalSensitivityReport> {
    if !options.db.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision database does not exist".into(),
        ));
    }
    let store = match &options.causal_db {
        Some(path) if path.is_file() => {
            DecisionStore::with_causal_store(&options.db, CausalStore::new(path))
        }
        Some(_) => {
            return Err(DuDuClawError::Gateway(
                "causal source database does not exist".into(),
            ));
        }
        None => DecisionStore::new(&options.db),
    };
    let capacity_fallback_range = match (options.capacity_min, options.capacity_max) {
        (None, None) => None,
        (Some(min), Some(max)) => Some(BoundedCount { min, max }),
        _ => {
            return Err(DuDuClawError::Config(
                "capacity-min and capacity-max must be supplied together".into(),
            ));
        }
    };
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let plan = EmpiricalSensitivityPlan {
        runs: options.runs,
        arrival_block_days: options.arrival_block_days,
        sampling_mode: if options.paired_saturated_days {
            EmpiricalSamplingMode::PairedSaturatedDays
        } else {
            EmpiricalSamplingMode::Independent
        },
        capacity_fallback_range,
        max_final_backlog: options.max_final_backlog,
        max_staff_cost_cents: options.max_staff_cost_cents,
        min_sla_resolved: options.min_sla_resolved,
    };
    let result = if let Some(run_id) = &options.run_id {
        let stored = store
            .load_empirical_run(&scope, run_id)
            .map_err(gateway_error)?;
        if stored.fit_id != options.fit
            || stored.model_version != options.model
            || stored.baseline_id != options.baseline
            || stored.alternative_id != options.alternative
            || stored.plan != plan
        {
            return Err(DuDuClawError::Gateway(
                "empirical replay flags differ from stored run manifest".into(),
            ));
        }
        stored.report
    } else {
        store
            .simulate_stored_empirical(
                &scope,
                &options.fit,
                &options.model,
                &options.baseline,
                &options.alternative,
                &plan,
            )
            .map_err(gateway_error)?
    };
    if options
        .expected_hash
        .as_ref()
        .is_some_and(|expected| expected != &result.replay_hash)
    {
        return Err(DuDuClawError::Gateway(
            "empirical replay hash differs from expected hash".into(),
        ));
    }
    Ok(result)
}

pub fn empirical_replay(options: EmpiricalReplayOptions) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&checked_empirical_replay(&options)?)?
    );
    Ok(())
}

fn checked_empirical_screen(options: &EmpiricalScreenOptions) -> Result<JointRiskScreenReport> {
    if !options.resource_plan_file.is_file() {
        return Err(DuDuClawError::Gateway(
            "resource plan file does not exist".into(),
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&options.resource_plan_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Gateway(
            "resource plan file exceeds size limit".into(),
        ));
    }
    let resource_plan: StaffingResourcePlan = serde_json::from_slice(&bytes)?;
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let report = store
        .screen_empirical_policy(
            &scope,
            &options.run_id,
            &resource_plan,
            &JointRiskScreenCriteria {
                max_joint_violation_bps: options.max_joint_violation_bps,
                min_joint_recovery_bps: options.min_joint_recovery_bps,
                min_sla_improvement_bps: options.min_sla_improvement_bps,
            },
        )
        .map_err(gateway_error)?;
    if options
        .expected_hash
        .as_ref()
        .is_some_and(|hash| hash != &report.replay_hash)
    {
        return Err(DuDuClawError::Gateway(
            "empirical screen hash differs from expected hash".into(),
        ));
    }
    if options.save_run {
        let stored = store
            .put_policy_screen(
                &scope,
                &options.run_id,
                &resource_plan,
                &JointRiskScreenCriteria {
                    max_joint_violation_bps: options.max_joint_violation_bps,
                    min_joint_recovery_bps: options.min_joint_recovery_bps,
                    min_sla_improvement_bps: options.min_sla_improvement_bps,
                },
            )
            .map_err(gateway_error)?;
        if stored.report != report {
            return Err(DuDuClawError::Gateway(
                "stored empirical screen differs from replay".into(),
            ));
        }
    }
    Ok(report)
}

pub fn empirical_screen(options: EmpiricalScreenOptions) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&checked_empirical_screen(&options)?)?
    );
    Ok(())
}

pub fn empirical_load_screen(options: EmpiricalLoadScreenOptions) -> Result<()> {
    let store = review_store(&options.db, &options.causal_db)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let record = store
        .load_policy_screen(&scope, &options.replay_hash)
        .map_err(gateway_error)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

/// Read a ticket source file under the same 2 MiB cap the store enforces. An
/// unbounded `std::fs::read` would allocate the whole file before the size
/// rejection ever ran.
fn read_ticket_source_file(path: &std::path::Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(DuDuClawError::Gateway(
            "ticket source file exceeds size limit".into(),
        ));
    }
    Ok(bytes)
}

fn checked_event_replay(options: &EventReplayOptions) -> Result<EventSimulationResult> {
    if !options.db.is_file() || !options.source_file.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision database and ticket source file must exist".into(),
        ));
    }
    let store = match &options.causal_db {
        Some(path) if path.is_file() => {
            DecisionStore::with_causal_store(&options.db, CausalStore::new(path))
        }
        Some(_) => {
            return Err(DuDuClawError::Gateway(
                "causal source database does not exist".into(),
            ));
        }
        None => DecisionStore::new(&options.db),
    };
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let source_bytes = read_ticket_source_file(&options.source_file)?;
    let config = EventQueueConfig {
        shift_start_seconds: options.shift_start_seconds,
        shift_seconds: options.shift_seconds,
    };
    let result = store
        .replay_ticket_events(
            &scope,
            &options.snapshot,
            &options.model,
            &options.scenario,
            &source_bytes,
            &options.window_start,
            &options.baseline_scenario,
            &config,
        )
        .map_err(gateway_error)?;
    if options
        .expected_hash
        .as_ref()
        .is_some_and(|expected| expected != &result.replay_hash)
    {
        return Err(DuDuClawError::Gateway(
            "event replay hash differs from expected hash".into(),
        ));
    }
    Ok(result)
}

pub fn event_replay(options: EventReplayOptions) -> Result<()> {
    let result = checked_event_replay(&options)?;
    if options.save_run {
        let source_bytes = read_ticket_source_file(&options.source_file)?;
        let store = match &options.causal_db {
            Some(path) => DecisionStore::with_causal_store(&options.db, CausalStore::new(path)),
            None => DecisionStore::new(&options.db),
        };
        let scope = DecisionScope {
            tenant_id: options.tenant,
            acl: options.acl,
        };
        let config = EventQueueConfig {
            shift_start_seconds: options.shift_start_seconds,
            shift_seconds: options.shift_seconds,
        };
        let run = store
            .put_event_run(
                &scope,
                &options.snapshot,
                &options.model,
                &options.scenario,
                &source_bytes,
                &options.window_start,
                &options.baseline_scenario,
                &config,
            )
            .map_err(gateway_error)?;
        if run.result != result {
            return Err(DuDuClawError::Gateway(
                "stored event run differs from replay".into(),
            ));
        }
        println!("{}", serde_json::to_string_pretty(&run)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}

pub fn event_load_run(options: EventLoadRunOptions) -> Result<()> {
    if !options.db.is_file() || !options.source_file.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision database and ticket source file must exist".into(),
        ));
    }
    let store = match &options.causal_db {
        Some(path) if path.is_file() => {
            DecisionStore::with_causal_store(&options.db, CausalStore::new(path))
        }
        Some(_) => {
            return Err(DuDuClawError::Gateway(
                "causal source database does not exist".into(),
            ));
        }
        None => DecisionStore::new(&options.db),
    };
    let mut source_bytes = Vec::new();
    std::fs::File::open(&options.source_file)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut source_bytes)?;
    let scope = DecisionScope {
        tenant_id: options.tenant,
        acl: options.acl,
    };
    let loaded = store
        .load_event_run_with_engine_state(&scope, &options.replay_hash, &source_bytes)
        .map_err(gateway_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&EngineStateReport {
            record: &loaded.run,
            engine_matches_current: loaded.engine_matches_current,
        })?
    );
    Ok(())
}

fn checked_invalidate_source_all(
    options: &InvalidateSourceOptions,
) -> Result<(CausalInvalidationResult, Option<usize>)> {
    let ccr_db = options.ccr_db.as_deref().ok_or_else(|| {
        DuDuClawError::Gateway(
            "--ccr-db is required so CCR is tombstoned before source invalidation".into(),
        )
    })?;
    if !options.db.is_file() || !options.causal_db.is_file() {
        return Err(DuDuClawError::Gateway(
            "decision and causal databases must both exist".into(),
        ));
    }
    let store = DecisionStore::with_causal_store(&options.db, CausalStore::new(&options.causal_db));
    let scope = DecisionScope {
        tenant_id: options.tenant.clone(),
        acl: options.acl.clone(),
    };
    let removed = store
        .remove_causal_artifact_with_dependents(
            &scope,
            &options.artifact,
            CausalSourceRemoval::Invalidate,
            Some(ccr_db),
        )
        .map_err(gateway_error)?;
    Ok((
        CausalInvalidationResult {
            demoted_claims: removed.demoted_claims,
            scrubbed_snapshots: removed.scrubbed_snapshots,
        },
        removed.scrubbed_ccr_originals,
    ))
}

pub fn invalidate_source(options: InvalidateSourceOptions) -> Result<()> {
    let (result, scrubbed_ccr_originals) = checked_invalidate_source_all(&options)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "demoted_claims": result.demoted_claims,
            "scrubbed_snapshots": result.scrubbed_snapshots,
            "scrubbed_ccr_originals": scrubbed_ccr_originals,
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_on_big_stack;
    use clap::Parser;
    use duduclaw_gateway::decision_calibration::ObservedSupportDay;
    use duduclaw_gateway::decision_sim::{DecisionSnapshot, QueueModel, StaffingScenario};
    use duduclaw_gateway::decision_store::ObservedOutcomeExport;

    /// Regression: `--save-run` re-read the ticket source with an unbounded
    /// `std::fs::read`, allocating the whole file before any size check. Both
    /// reads now share one capped reader.
    #[test]
    fn oversized_ticket_source_file_is_refused_by_the_shared_capped_reader() {
        let dir = tempfile::tempdir().unwrap();
        let oversized = dir.path().join("oversized-source.json");
        std::fs::write(&oversized, vec![b'x'; 2 * 1024 * 1024 + 8]).unwrap();
        assert!(read_ticket_source_file(&oversized).is_err());
        let within_limit = dir.path().join("source.json");
        std::fs::write(&within_limit, b"[[],[]]").unwrap();
        assert_eq!(read_ticket_source_file(&within_limit).unwrap(), b"[[],[]]");
    }

    #[test]
    fn prospective_shadow_file_requires_queue_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shadow.json");
        let mut export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: None,
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            observed_through_utc: "2026-09-02T00:00:00Z".into(),
            observed_days: vec![ObservedSupportDay {
                arrivals: 4,
                backlog_start: 0,
                resolved: 2,
                backlog_end: 2,
                agents: 1,
                fixed_extra_capacity: 0,
            }],
        };
        std::fs::write(&path, serde_json::to_vec(&export).unwrap()).unwrap();
        assert!(shadow_source_file(&path).is_err());
        export.queue_id = Some("support-queue".into());
        std::fs::write(&path, serde_json::to_vec(&export).unwrap()).unwrap();
        assert_eq!(shadow_source_file(&path).unwrap().2, "support-queue");
        export.queue_id = Some(" support-queue ".into());
        std::fs::write(&path, serde_json::to_vec(&export).unwrap()).unwrap();
        assert!(shadow_source_file(&path).is_err());
    }

    // ── X1 方案 1 / 方案 4: the two new export leaves ────────────────

    #[test]
    fn task_board_export_leaf_parses_with_defaults_and_overrides() {
        run_on_big_stack(task_board_export_leaf_parses_with_defaults_and_overrides_body);
    }

    fn task_board_export_leaf_parses_with_defaults_and_overrides_body() {
        // Bare form: every argument optional, `all` is the default queue.
        let bare = crate::Cli::try_parse_from(["duduclaw", "decision-task-board-export"]);
        assert!(bare.is_ok(), "{:?}", bare.err());
        let full = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-task-board-export",
            "--queue",
            "alice",
            "--horizon-days",
            "21",
            "--db",
            "/tmp/tasks.db",
            "--out",
            "/tmp/out.json",
        ]);
        assert!(full.is_ok(), "{:?}", full.err());
    }

    #[test]
    fn odoo_export_leaf_requires_its_identity_and_window_arguments() {
        run_on_big_stack(odoo_export_leaf_requires_its_identity_and_window_arguments_body);
    }

    fn odoo_export_leaf_requires_its_identity_and_window_arguments_body() {
        let full = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-odoo-export",
            "--agent",
            "support",
            "--model",
            "helpdesk.ticket",
            "--queue",
            "7",
            "--since",
            "2026-09-01T00:00:00Z",
            "--until",
            "2026-09-15T00:00:00Z",
        ]);
        assert!(full.is_ok(), "{:?}", full.err());
        // The agent identity is not optional: an export must name whose
        // credentials it connects with.
        assert!(
            crate::Cli::try_parse_from([
                "duduclaw",
                "decision-odoo-export",
                "--model",
                "helpdesk.ticket",
                "--queue",
                "7",
                "--since",
                "2026-09-01T00:00:00Z",
                "--until",
                "2026-09-15T00:00:00Z",
            ])
            .is_err(),
            "--agent must be required"
        );
    }

    #[test]
    fn shadow_policy_handoff_cli_parses_and_persists_review() {
        run_on_big_stack(shadow_policy_handoff_cli_parses_and_persists_review_body);
    }

    fn shadow_policy_handoff_cli_parses_and_persists_review_body() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("decisions.db");
        std::fs::File::create(&db).unwrap();
        let midnight = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let from = (midnight + chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let cutoff = (midnight + chrono::Duration::days(3))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let until = (midnight + chrono::Duration::days(10))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let parsed_policy = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-policy",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--policy-id",
            "old",
            "--source-lineage",
            "queue",
            "--queue-id",
            "support-queue",
            "--effective-from-utc",
            &from,
            "--effective-until-utc",
            &until,
        ])
        .unwrap();
        assert!(
            matches!(parsed_policy.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowPolicy {
            queue_id, ..
        }) if queue_id == "support-queue")
        );
        shadow_policy(ShadowPolicyOptions {
            db: db.clone(),
            tenant: "tenant".into(),
            acl: "private".into(),
            policy_id: "old".into(),
            source_lineage: "queue".into(),
            queue_id: "support-queue".into(),
            effective_from_utc: from,
            effective_until_utc: until.clone(),
            issue_deadline_seconds: 3_600,
            min_training_days: 14,
            min_saturated_days: 7,
        })
        .unwrap();
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-policy-supersede",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--old-policy-id",
            "old",
            "--new-policy-id",
            "new",
            "--cutoff-utc",
            &cutoff,
            "--effective-until-utc",
            &until,
            "--reviewer",
            "operator-1",
        ])
        .unwrap();
        let crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowPolicySupersede {
            db,
            tenant,
            acl,
            old_policy_id,
            new_policy_id,
            cutoff_utc,
            effective_until_utc,
            reviewer,
            issue_deadline_seconds,
            min_training_days,
            min_saturated_days,
        }) = parsed.command
        else {
            panic!("policy handoff command did not parse")
        };
        shadow_policy_supersede(ShadowPolicySupersedeOptions {
            db: db.clone(),
            tenant: tenant.clone(),
            acl: acl.clone(),
            old_policy_id,
            new_policy_id,
            cutoff_utc,
            effective_until_utc,
            reviewer,
            issue_deadline_seconds,
            min_training_days,
            min_saturated_days,
        })
        .unwrap();
        let record = DecisionStore::new(db)
            .load_shadow_policy_supersession(
                &DecisionScope {
                    tenant_id: tenant,
                    acl,
                },
                "old",
            )
            .unwrap();
        assert_eq!(record.new_policy_id, "new");
        assert_eq!(record.reviewer, "operator-1");
    }

    #[test]
    fn shadow_cli_requires_predeclared_future_policy() {
        run_on_big_stack(shadow_cli_requires_predeclared_future_policy_body);
    }

    fn shadow_cli_requires_predeclared_future_policy_body() {
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-forecast",
            "--db",
            "decision.db",
            "--causal-db",
            "causal.db",
            "--opening-file",
            "opening.json",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--sla-id",
            "sla-1",
            "--forecast-id",
            "forecast-1",
            "--model-version",
            "model-v1",
            "--source-lineage",
            "support-queue",
            "--retention-until-utc",
            "2099-01-01T00:00:00Z",
        ])
        .unwrap();
        assert!(
            matches!(parsed.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaForecast {
            sla_id, forecast_id, model_version, ..
        }) if sla_id == "sla-1" && forecast_id == "forecast-1" && model_version == "model-v1")
        );
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-score",
            "--db",
            "decision.db",
            "--causal-db",
            "causal.db",
            "--observation-file",
            "tickets.json",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--score-id",
            "sla-score-1",
            "--sla-forecast-id",
            "sla-1",
            "--aggregate-score-id",
            "score-1",
            "--source-lineage",
            "support-queue",
            "--retention-until-utc",
            "2099-01-01T00:00:00Z",
        ])
        .unwrap();
        assert!(
            matches!(parsed.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaScore {
            score_id, sla_forecast_id, aggregate_score_id, ..
        }) if score_id == "sla-score-1" && sla_forecast_id == "sla-1"
            && aggregate_score_id == "score-1")
        );
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-score-correction",
            "--db",
            "decision.db",
            "--causal-db",
            "causal.db",
            "--observation-file",
            "tickets-repaired.json",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--correction-id",
            "sla-correction-1",
            "--sla-forecast-id",
            "sla-1",
            "--previous-revision-id",
            "sla-score-1",
            "--aggregate-revision-id",
            "correction-1",
            "--reviewer",
            "reviewer-1",
            "--reason",
            "verified export repair",
            "--source-lineage",
            "support-queue",
            "--retention-until-utc",
            "2099-01-01T00:00:00Z",
        ])
        .unwrap();
        assert!(
            matches!(parsed.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaScoreCorrection {
            correction_id, sla_forecast_id, aggregate_revision_id, ..
        }) if correction_id == "sla-correction-1" && sla_forecast_id == "sla-1"
            && aggregate_revision_id == "correction-1")
        );
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-score-current",
            "--db",
            "decision.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--sla-forecast-id",
            "sla-1",
        ])
        .unwrap();
        assert!(
            matches!(parsed.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaScoreCurrent {
            sla_forecast_id, ..
        }) if sla_forecast_id == "sla-1")
        );
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("decisions.db");
        let causal_db = dir.path().join("causal.db");
        std::fs::File::create(&db).unwrap();
        std::fs::File::create(&causal_db).unwrap();
        let midnight = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let target_day = midnight.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let tomorrow = (midnight + chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let policy_end = (midnight + chrono::Duration::days(31))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        shadow_policy(ShadowPolicyOptions {
            db: db.clone(),
            tenant: "tenant".into(),
            acl: "private".into(),
            policy_id: "tomorrow-policy".into(),
            source_lineage: "support-queue".into(),
            queue_id: "support-queue".into(),
            effective_from_utc: tomorrow,
            effective_until_utc: policy_end,
            issue_deadline_seconds: 3_600,
            min_training_days: 14,
            min_saturated_days: 7,
        })
        .unwrap();
        let store = DecisionStore::with_causal_store(&db, CausalStore::new(&causal_db));
        assert_eq!(
            store
                .load_shadow_policy(
                    &DecisionScope {
                        tenant_id: "tenant".into(),
                        acl: "private".into(),
                    },
                    "tomorrow-policy"
                )
                .unwrap()
                .min_training_days,
            14
        );
        let mut backlog = 10_u64;
        let mut days = Vec::new();
        for _ in 0..14 {
            days.push(ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: backlog + 4,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog += 4;
        }
        let training_file = dir.path().join("training.json");
        std::fs::write(
            &training_file,
            serde_json::to_vec(&ObservedOutcomeExport {
                sla_days: None,
                resolved_within_sla_by_day: None,
                queue_id: Some("support-queue".into()),
                window_start_utc: (midnight - chrono::Duration::days(14))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                observed_through_utc: target_day.clone(),
                observed_days: days,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            shadow_forecast(ShadowForecastOptions {
                db: db.clone(),
                causal_db: causal_db.clone(),
                training_file,
                tenant: "tenant".into(),
                acl: "private".into(),
                forecast_id: "today-forecast".into(),
                policy_id: "tomorrow-policy".into(),
                target_day_utc: target_day.clone(),
                source_lineage: "support-queue".into(),
                retention_until_utc: "2099-01-01T00:00:00Z".into(),
                opening_backlog: backlog,
                planned_agents: 2,
                planned_fixed_extra_capacity: 0,
            })
            .is_err()
        );
        let observation_file = dir.path().join("observation.json");
        std::fs::write(
            &observation_file,
            serde_json::to_vec(&ObservedOutcomeExport {
                sla_days: None,
                resolved_within_sla_by_day: None,
                queue_id: Some("support-queue".into()),
                window_start_utc: target_day,
                observed_through_utc: (midnight + chrono::Duration::days(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                observed_days: vec![ObservedSupportDay {
                    arrivals: 20,
                    backlog_start: backlog,
                    resolved: 16,
                    backlog_end: backlog + 4,
                    agents: 2,
                    fixed_extra_capacity: 0,
                }],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            shadow_score(ShadowScoreOptions {
                db,
                causal_db,
                observation_file,
                tenant: "tenant".into(),
                acl: "private".into(),
                score_id: "too-early".into(),
                forecast_id: "today-forecast".into(),
                source_lineage: "support-queue".into(),
                retention_until_utc: "2099-01-01T00:00:00Z".into(),
            })
            .is_err()
        );
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-score-correction",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--observation-file",
            "repair.json",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--correction-id",
            "repair-1",
            "--forecast-id",
            "forecast-1",
            "--previous-revision-id",
            "score-1",
            "--source-lineage",
            "support-queue",
            "--retention-until-utc",
            "2099-01-01T00:00:00Z",
            "--reviewer",
            "operator-1",
            "--reason",
            "verified source repair",
        ])
        .unwrap();
        assert!(
            matches!(parsed.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowScoreCorrection {
            previous_revision_id, reviewer, ..
        }) if previous_revision_id == "score-1" && reviewer == "operator-1")
        );
        let current = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-score-current",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--forecast-id",
            "forecast-1",
        ])
        .unwrap();
        assert!(
            matches!(current.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowScoreCurrent {
            forecast_id, ..
        }) if forecast_id == "forecast-1")
        );
        let assess = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-policy-assess",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--policy-id",
            "tomorrow-policy",
        ])
        .unwrap();
        assert!(
            matches!(assess.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowPolicyAssess {
            policy_id, ..
        }) if policy_id == "tomorrow-policy")
        );
        let sla_assess = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-policy-assess",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--policy-id",
            "tomorrow-policy",
        ])
        .unwrap();
        assert!(matches!(sla_assess.command,
            crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaPolicyAssess { policy_id, .. })
            if policy_id == "tomorrow-policy"));
        let screen = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-policy-screen",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--policy-id",
            "tomorrow-policy",
            "--min-complete-days",
            "21",
            "--min-fixed-coverage-bps",
            "8000",
            "--save-run",
        ])
        .unwrap();
        assert!(
            matches!(screen.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowPolicyScreen {
            policy_id, min_complete_days: 21, min_fixed_coverage_bps: 8000, save_run: true, ..
        }) if policy_id == "tomorrow-policy")
        );
        let sla_screen = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-policy-screen",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--policy-id",
            "tomorrow-policy",
            "--min-complete-days",
            "21",
            "--min-fixed-coverage-bps",
            "8000",
            "--save-run",
        ])
        .unwrap();
        assert!(matches!(sla_screen.command,
            crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaPolicyScreen {
                policy_id, min_complete_days: 21,
                min_fixed_coverage_bps: 8000, save_run: true, ..
            }) if policy_id == "tomorrow-policy"));
        let loaded = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-policy-load-screen",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--replay-hash",
            "screen-hash",
        ])
        .unwrap();
        assert!(
            matches!(loaded.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowPolicyLoadScreen {
            replay_hash, ..
        }) if replay_hash == "screen-hash")
        );
        let sla_loaded = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-policy-load-screen",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--replay-hash",
            "sla-screen-hash",
        ])
        .unwrap();
        assert!(matches!(sla_loaded.command,
            crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaPolicyLoadScreen { replay_hash, .. })
            if replay_hash == "sla-screen-hash"));
        let review = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-screen-request-review",
            "--home",
            "home",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--replay-hash",
            "screen-hash",
            "--agent",
            "support-agent",
            "--summary",
            "inspect candidate",
        ])
        .unwrap();
        assert!(
            matches!(review.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowScreenRequestReview {
            replay_hash, ttl_seconds: 3600, ..
        }) if replay_hash == "screen-hash")
        );
        let checked = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-screen-check-review",
            "--home",
            "home",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--replay-hash",
            "screen-hash",
            "--approval",
            "approval-id",
        ])
        .unwrap();
        assert!(
            matches!(checked.command, crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowScreenCheckReview {
            replay_hash, approval, ..
        }) if replay_hash == "screen-hash" && approval == "approval-id")
        );
        let sla_review = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-screen-request-review",
            "--home",
            "home",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--replay-hash",
            "sla-screen-hash",
            "--agent",
            "support-agent",
            "--summary",
            "inspect SLA candidate",
        ])
        .unwrap();
        assert!(matches!(sla_review.command,
            crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaScreenRequestReview {
                replay_hash, ttl_seconds: 3600, ..
            }) if replay_hash == "sla-screen-hash"));
        let sla_checked = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-shadow-sla-screen-check-review",
            "--home",
            "home",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--replay-hash",
            "sla-screen-hash",
            "--approval",
            "approval-id",
        ])
        .unwrap();
        assert!(matches!(sla_checked.command,
            crate::Commands::DecisionShadow(crate::DecisionShadowCommands::DecisionShadowSlaScreenCheckReview {
                replay_hash, approval, ..
            }) if replay_hash == "sla-screen-hash" && approval == "approval-id"));
    }

    #[test]
    fn pilot_import_binds_exact_source_and_replays_until_revoked() {
        run_on_big_stack(pilot_import_binds_exact_source_and_replays_until_revoked_body);
    }

    fn pilot_import_binds_exact_source_and_replays_until_revoked_body() {
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-import-pilot",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--export-file",
            "pilot.json",
            "--model-file",
            "model.json",
            "--source-output",
            "source.json",
            "--tenant",
            "tenant",
            "--acl",
            "private",
            "--queue-id",
            "support-queue",
            "--source-lineage",
            "source-lineage",
            "--retention-until-utc",
            "2099-01-01T00:00:00Z",
        ])
        .unwrap();
        assert!(
            matches!(parsed.command, crate::Commands::DecisionPilot(crate::DecisionPilotCommands::DecisionImportPilot {
            queue_id, ..
        }) if queue_id == "support-queue")
        );
        let dir = tempfile::tempdir().unwrap();
        let mut export = synthetic_support_export(47, 21).unwrap();
        // `synthetic-support-` is the reserved built-in fixture namespace; the
        // local importer now refuses it, same as the dashboard path.
        export.snapshot_id = "local-import-47-21".into();
        let model = QueueModel {
            version: "imported-model-v1".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 10_000,
        };
        let export_file = dir.path().join("export.json");
        let model_file = dir.path().join("model.json");
        std::fs::write(&export_file, serde_json::to_vec(&export).unwrap()).unwrap();
        std::fs::write(&model_file, serde_json::to_vec(&model).unwrap()).unwrap();
        let options = ImportPilotOptions {
            db: dir.path().join("decision.sqlite"),
            causal_db: dir.path().join("causal.sqlite"),
            export_file: export_file.clone(),
            model_file,
            source_output: dir.path().join("source.json"),
            tenant: "synthetic-test".into(),
            acl: "private".into(),
            queue_id: "synthetic-support-queue".into(),
            source_lineage: "local-ticket-export".into(),
            retention_until_utc: "2099-01-01T00:00:00Z".into(),
        };
        let mut unlabeled = export.clone();
        for ticket in &mut unlabeled.tickets {
            ticket.queue_id = None;
        }
        for day in &mut unlabeled.staffing {
            day.queue_id = None;
        }
        let unlabeled_source =
            serde_json::to_vec(&(&unlabeled.tickets, &unlabeled.staffing)).unwrap();
        unlabeled.source_version_hashes = vec![format!("{:x}", Sha256::digest(unlabeled_source))];
        let unlabeled_file = dir.path().join("unlabeled-export.json");
        std::fs::write(&unlabeled_file, serde_json::to_vec(&unlabeled).unwrap()).unwrap();
        let unlabeled_options = ImportPilotOptions {
            db: dir.path().join("unlabeled-decision.sqlite"),
            causal_db: dir.path().join("unlabeled-causal.sqlite"),
            export_file: unlabeled_file,
            model_file: options.model_file.clone(),
            source_output: dir.path().join("unlabeled-source.json"),
            tenant: options.tenant.clone(),
            acl: options.acl.clone(),
            queue_id: options.queue_id.clone(),
            source_lineage: options.source_lineage.clone(),
            retention_until_utc: options.retention_until_utc.clone(),
        };
        assert!(build_import_pilot(&unlabeled_options).is_err());
        assert!(!unlabeled_options.db.exists());
        let wrong_queue_options = ImportPilotOptions {
            db: dir.path().join("wrong-queue-decision.sqlite"),
            causal_db: dir.path().join("wrong-queue-causal.sqlite"),
            export_file: export_file.clone(),
            model_file: options.model_file.clone(),
            source_output: dir.path().join("wrong-queue-source.json"),
            tenant: options.tenant.clone(),
            acl: options.acl.clone(),
            queue_id: "different-queue".into(),
            source_lineage: options.source_lineage.clone(),
            retention_until_utc: options.retention_until_utc.clone(),
        };
        assert!(build_import_pilot(&wrong_queue_options).is_err());
        assert!(!wrong_queue_options.db.exists());
        let mut unidentified = export.clone();
        for day in &mut unidentified.staffing {
            day.agents = 3;
        }
        let unidentified_source =
            serde_json::to_vec(&(&unidentified.tickets, &unidentified.staffing)).unwrap();
        unidentified.source_version_hashes =
            vec![format!("{:x}", Sha256::digest(unidentified_source))];
        let unidentified_file = dir.path().join("unidentified-export.json");
        std::fs::write(
            &unidentified_file,
            serde_json::to_vec(&unidentified).unwrap(),
        )
        .unwrap();
        let unidentified_options = ImportPilotOptions {
            db: dir.path().join("unidentified-decision.sqlite"),
            causal_db: dir.path().join("unidentified-causal.sqlite"),
            export_file: unidentified_file,
            model_file: options.model_file.clone(),
            source_output: dir.path().join("unidentified-source.json"),
            tenant: options.tenant.clone(),
            acl: options.acl.clone(),
            queue_id: options.queue_id.clone(),
            source_lineage: options.source_lineage.clone(),
            retention_until_utc: options.retention_until_utc.clone(),
        };
        let unidentified_report = build_import_pilot(&unidentified_options).unwrap();
        assert!(unidentified_report.sla_holdout.is_none());
        assert!(unidentified_report.sla_holdout_id.is_none());
        assert!(
            unidentified_report
                .sla_holdout_unavailable_reason
                .as_deref()
                .unwrap()
                .contains("unidentified")
        );
        // Regression: the reserved synthetic fixture namespace was refused by
        // the dashboard importer but not by this local one, so a real upload
        // could squat on fixture identifiers through the CLI.
        let mut reserved = export.clone();
        reserved.snapshot_id = "synthetic-support-47-21".into();
        let reserved_file = dir.path().join("reserved-export.json");
        std::fs::write(&reserved_file, serde_json::to_vec(&reserved).unwrap()).unwrap();
        let reserved_options = ImportPilotOptions {
            db: dir.path().join("reserved-decision.sqlite"),
            causal_db: dir.path().join("reserved-causal.sqlite"),
            export_file: reserved_file,
            model_file: options.model_file.clone(),
            source_output: dir.path().join("reserved-source.json"),
            tenant: options.tenant.clone(),
            acl: options.acl.clone(),
            queue_id: options.queue_id.clone(),
            source_lineage: options.source_lineage.clone(),
            retention_until_utc: options.retention_until_utc.clone(),
        };
        assert!(build_import_pilot(&reserved_options).is_err());
        assert!(!reserved_options.db.exists());
        assert!(!reserved_options.source_output.exists());
        let report = build_import_pilot(&options).unwrap();
        assert_eq!(report.status, "exploratory_local_import");
        assert_eq!(report.queue_id, "synthetic-support-queue");
        let sla = report.sla_holdout.as_ref().unwrap();
        assert_eq!(sla.source_sha256, report.source_sha256);
        assert_eq!(sla.training_days, 14);
        assert_eq!(sla.holdout_days, 7);
        assert_eq!(sla.daily_abs_error_sum, 0);
        assert_eq!(sla.one_step_forecast.as_ref().unwrap().evaluation_days, 7);
        assert!(sla.provided_model_matches_fit);
        assert!(report.sla_holdout_unavailable_reason.is_none());
        let sla_command = report.sla_holdout_command.clone().unwrap();
        let parsed = crate::Cli::try_parse_from(
            std::iter::once(sla_command.program).chain(sla_command.args),
        )
        .unwrap();
        let crate::Commands::Decision(crate::DecisionCommands::DecisionSlaHoldout {
            db,
            causal_db,
            source_file,
            tenant,
            acl,
            record,
            expected_sha256,
        }) = parsed.command
        else {
            panic!("wrong SLA holdout command")
        };
        let mut sla_options = ForecastValidationOptions {
            db,
            causal_db,
            source_file,
            tenant,
            acl,
            record,
            expected_sha256,
        };
        let stored_sla = checked_sla_holdout(&sla_options).unwrap();
        assert_eq!(stored_sla.diagnostic, *sla);
        assert_eq!(
            sla_options.record,
            report.sla_holdout_id.as_ref().unwrap().as_str()
        );
        sla_options.expected_sha256 = Some("wrong".into());
        assert!(checked_sla_holdout(&sla_options).is_err());
        sla_options.expected_sha256 = report.sla_holdout_sha256.clone();
        #[cfg(unix)]
        for path in [&options.db, &options.causal_db, &options.source_output] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(report.source_sha256, export.source_version_hashes[0]);
        assert_eq!(
            std::fs::read(&options.source_output).unwrap(),
            serde_json::to_vec(&(&export.tickets, &export.staffing)).unwrap(),
        );
        let replay = ReplayOptions {
            db: options.db.clone(),
            causal_db: Some(options.causal_db.clone()),
            tenant: options.tenant.clone(),
            acl: options.acl.clone(),
            snapshot: report.snapshot_id.clone(),
            model: report.model_version.clone(),
            scenario: report.baseline_scenario_id.clone(),
            expected_hash: Some(report.replay_hash.clone()),
        };
        assert_eq!(
            checked_replay(&replay).unwrap().replay_hash,
            report.replay_hash
        );
        let decision_store =
            DecisionStore::with_causal_store(&options.db, CausalStore::new(&options.causal_db));
        let decision_scope = DecisionScope {
            tenant_id: options.tenant.clone(),
            acl: options.acl.clone(),
        };
        let source_bytes = std::fs::read(&options.source_output).unwrap();
        assert!(matches!(
            decision_store.put_sla_holdout(
                &decision_scope,
                report.sla_holdout_id.as_deref().unwrap(),
                &report.snapshot_id,
                &report.model_version,
                &report.baseline_scenario_id,
                &source_bytes,
                &export.window_start_utc,
                13,
                7,
            ),
            Err(duduclaw_gateway::decision_store::DecisionStoreError::VersionConflict)
        ));
        let causal = CausalStore::new(&options.causal_db);
        causal
            .invalidate_artifact(
                &EvidenceScope {
                    tenant_id: options.tenant.clone(),
                    acl: options.acl.clone(),
                },
                &report.source_artifact_id,
            )
            .unwrap();
        assert!(checked_replay(&replay).is_err());
        assert!(checked_sla_holdout(&sla_options).is_err());

        let mut wrong_export = export;
        wrong_export.source_version_hashes = vec!["claimed-other-version".into()];
        let wrong_file = dir.path().join("wrong-export.json");
        std::fs::write(&wrong_file, serde_json::to_vec(&wrong_export).unwrap()).unwrap();
        let wrong = ImportPilotOptions {
            db: dir.path().join("wrong-decision.sqlite"),
            causal_db: dir.path().join("wrong-causal.sqlite"),
            export_file: wrong_file,
            model_file: options.model_file,
            source_output: dir.path().join("wrong-source.json"),
            tenant: options.tenant,
            acl: options.acl,
            queue_id: options.queue_id,
            source_lineage: options.source_lineage,
            retention_until_utc: options.retention_until_utc,
        };
        assert!(build_import_pilot(&wrong).is_err());
        assert!(!wrong.db.exists());
    }

    #[test]
    fn brief_cli_accepts_multiple_effect_ids_in_exact_scope() {
        run_on_big_stack(brief_cli_accepts_multiple_effect_ids_in_exact_scope_body);
    }

    fn brief_cli_accepts_multiple_effect_ids_in_exact_scope_body() {
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-brief",
            "--db",
            "decisions.db",
            "--causal-db",
            "causal.db",
            "--tenant",
            "synthetic",
            "--acl",
            "private",
            "--snapshot",
            "snapshot",
            "--model",
            "model",
            "--baseline",
            "base",
            "--alternative",
            "more",
            "--effect-id",
            "effect-a",
            "--effect-id",
            "effect-b",
            "--empirical-run",
            "run-a",
            "--policy-screen",
            "screen-a",
            "--forecast-validation",
            "forecast-a",
            "--sla-holdout",
            "sla-a",
            "--baseline-event-hash",
            "event-base",
            "--alternative-event-hash",
            "event-more",
            "--source-file",
            "source.json",
            "--assumption",
            "FIFO",
        ])
        .unwrap();
        let crate::Commands::Decision(crate::DecisionCommands::DecisionBrief {
            tenant,
            acl,
            effect_ids,
            assumption,
            empirical_run,
            policy_screen,
            forecast_validation,
            sla_holdout,
            baseline_event_hash,
            alternative_event_hash,
            source_file,
            ..
        }) = parsed.command
        else {
            panic!("wrong brief command")
        };
        assert_eq!((tenant.as_str(), acl.as_str()), ("synthetic", "private"));
        assert_eq!(effect_ids, ["effect-a", "effect-b"]);
        assert_eq!(assumption, ["FIFO"]);
        assert_eq!(empirical_run.as_deref(), Some("run-a"));
        assert_eq!(policy_screen.as_deref(), Some("screen-a"));
        assert_eq!(forecast_validation.as_deref(), Some("forecast-a"));
        assert_eq!(sla_holdout.as_deref(), Some("sla-a"));
        assert_eq!(baseline_event_hash.as_deref(), Some("event-base"));
        assert_eq!(alternative_event_hash.as_deref(), Some("event-more"));
        assert_eq!(source_file, Some(PathBuf::from("source.json")));
    }

    #[test]
    fn local_replay_verifies_expected_hash_and_scope() {
        run_on_big_stack(local_replay_verifies_expected_hash_and_scope_body);
    }

    fn local_replay_verifies_expected_hash_and_scope_body() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("decisions.sqlite");
        let store = DecisionStore::new(&db);
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "snapshot".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["tickets-v1".into()],
            seed: 7,
            arrivals_by_day: vec![5],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "model-v1".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1],
            fixed_extra_capacity_by_day: vec![0],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        let alternative = StaffingScenario {
            id: "more".into(),
            agents_by_day: vec![2],
            fixed_extra_capacity_by_day: vec![0],
        };
        store.put_scenario(&scope, &alternative).unwrap();
        let expected = store
            .replay(&scope, "snapshot", "model-v1", "base")
            .unwrap();
        let brief = store
            .compare_scenarios(
                &scope,
                "snapshot",
                "model-v1",
                "base",
                "more",
                vec!["Synthetic FIFO queue".into()],
            )
            .unwrap();
        let command = brief.replay.baseline_command;
        let parsed =
            crate::Cli::try_parse_from(std::iter::once(command.program).chain(command.args))
                .unwrap();
        let crate::Commands::Decision(crate::DecisionCommands::DecisionReplay {
            db: parsed_db,
            causal_db,
            tenant,
            acl,
            snapshot: parsed_snapshot,
            model: parsed_model,
            scenario: parsed_scenario,
            expected_hash,
        }) = parsed.command
        else {
            panic!("brief did not produce the decision replay CLI command")
        };
        assert_eq!(
            checked_replay(&ReplayOptions {
                db: parsed_db,
                causal_db,
                tenant,
                acl,
                snapshot: parsed_snapshot,
                model: parsed_model,
                scenario: parsed_scenario,
                expected_hash,
            })
            .unwrap(),
            expected
        );
        let mut options = ReplayOptions {
            db: db.clone(),
            causal_db: None,
            tenant: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            snapshot: snapshot.id.clone(),
            model: model.version,
            scenario: scenario.id,
            expected_hash: Some(expected.replay_hash.clone()),
        };
        assert_eq!(checked_replay(&options).unwrap(), expected);
        options.expected_hash = Some("wrong".into());
        assert!(checked_replay(&options).is_err());
        options.expected_hash = None;
        options.tenant = "other".into();
        assert!(checked_replay(&options).is_err());

        let causal_db = dir.path().join("causal.sqlite");
        let causal = CausalStore::new(&causal_db);
        let evidence_scope = duduclaw_memory::causal::EvidenceScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let artifact = causal
            .add_artifact(
                &evidence_scope,
                "ticket_export",
                "source",
                "v1",
                "tickets",
                "source bytes",
                1,
                i64::MAX,
            )
            .unwrap();
        let bound_snapshot = DecisionSnapshot {
            id: "bound".into(),
            source_version_hashes: vec![artifact.content_sha256.clone()],
            ..snapshot
        };
        let bound_store = DecisionStore::with_causal_store(&db, causal.clone());
        bound_store.put_snapshot(&scope, &bound_snapshot).unwrap();
        bound_store
            .bind_causal_artifact(&scope, &bound_snapshot.id, &artifact.id)
            .unwrap();
        options.tenant = scope.tenant_id.clone();
        options.snapshot = bound_snapshot.id;
        options.causal_db = Some(causal_db);
        assert!(checked_replay(&options).is_ok());
        causal
            .invalidate_artifact(&evidence_scope, &artifact.id)
            .unwrap();
        assert!(checked_replay(&options).is_err());
    }

    #[test]
    fn record_outcome_cli_reads_exact_export_and_checks_replay_hash() {
        run_on_big_stack(record_outcome_cli_reads_exact_export_and_checks_replay_hash_body);
    }

    fn record_outcome_cli_reads_exact_export_and_checks_replay_hash_body() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("decisions.sqlite");
        let source_file = dir.path().join("observed.json");
        let store = DecisionStore::new(&db);
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "forecast".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["forecast-source".into()],
            seed: 1,
            arrivals_by_day: vec![3],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "model".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1],
            fixed_extra_capacity_by_day: vec![0],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        let hash = store
            .replay(&scope, "forecast", "model", "base")
            .unwrap()
            .replay_hash;
        let parsed_run = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-store-run",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--snapshot",
            "forecast",
            "--model",
            "model",
            "--scenario",
            "base",
        ])
        .unwrap();
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionStoreRun {
            db: run_db,
            causal_db: run_causal_db,
            tenant: run_tenant,
            acl: run_acl,
            snapshot: run_snapshot,
            model: run_model,
            scenario: run_scenario,
        }) = parsed_run.command
        else {
            panic!("wrong store-run command")
        };
        store_run(StoreRunOptions {
            db: run_db,
            causal_db: run_causal_db,
            tenant: run_tenant,
            acl: run_acl,
            snapshot: run_snapshot,
            model: run_model,
            scenario: run_scenario,
        })
        .unwrap();
        assert_eq!(
            store.load_daily_run(&scope, &hash).unwrap().replay_hash,
            hash
        );
        let export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            observed_through_utc: "2026-09-02T00:00:00Z".into(),
            observed_days: vec![ObservedSupportDay {
                arrivals: 4,
                backlog_start: 0,
                resolved: 2,
                backlog_end: 2,
                agents: 1,
                fixed_extra_capacity: 0,
            }],
        };
        std::fs::write(&source_file, serde_json::to_vec(&export).unwrap()).unwrap();
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-record-outcome",
            "--db",
            db.to_str().unwrap(),
            "--source-file",
            source_file.to_str().unwrap(),
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--observation",
            "obs-1",
            "--snapshot",
            "forecast",
            "--model",
            "model",
            "--scenario",
            "base",
            "--expected-hash",
            &hash,
            "--recorded-by",
            "reviewer-1",
        ])
        .unwrap();
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionRecordOutcome {
            db,
            causal_db,
            source_file,
            ticket_source_file,
            ticket_source_retention_until_utc,
            tenant,
            acl,
            observation,
            snapshot,
            model,
            scenario,
            expected_hash,
            recorded_by,
        }) = parsed.command
        else {
            panic!("wrong command")
        };
        record_outcome(RecordOutcomeOptions {
            db: db.clone(),
            causal_db,
            source_file: source_file.clone(),
            ticket_source_file,
            ticket_source_retention_until_utc,
            tenant,
            acl,
            observation,
            snapshot,
            model,
            scenario,
            expected_hash,
            recorded_by,
        })
        .unwrap();
        let ticket_option = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-record-outcome",
            "--db",
            db.to_str().unwrap(),
            "--source-file",
            source_file.to_str().unwrap(),
            "--ticket-source-file",
            "tickets.json",
            "--tenant",
            "tenant-a",
            "--ticket-source-retention-until-utc",
            "2027-01-01T00:00:00Z",
            "--acl",
            "private",
            "--observation",
            "obs-ticket",
            "--snapshot",
            "forecast",
            "--model",
            "model",
            "--scenario",
            "base",
            "--expected-hash",
            &hash,
            "--recorded-by",
            "reviewer-1",
        ])
        .unwrap();
        assert!(matches!(ticket_option.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionRecordOutcome {
                ticket_source_file: Some(path),
                ticket_source_retention_until_utc: Some(retention), ..
            }) if path == std::path::PathBuf::from("tickets.json")
                && retention == "2027-01-01T00:00:00Z"));
        let saved = store.get_observed_outcome(&scope, "obs-1").unwrap();
        assert_eq!(saved.assessment.arrivals_abs_error_sum, 1);
        assert_eq!(saved.assessment.backlog_abs_error_sum, 1);
        let mut ticket_observation = export.clone();
        ticket_observation.sla_days = Some(2);
        ticket_observation.resolved_within_sla_by_day = Some(vec![2]);
        let ticket_observation_file = dir.path().join("observed-ticket.json");
        std::fs::write(
            &ticket_observation_file,
            serde_json::to_vec(&ticket_observation).unwrap(),
        )
        .unwrap();
        let ticket_source_file = dir.path().join("tickets.json");
        let tickets = (0..4)
            .map(|index| duduclaw_gateway::decision_ingest::TicketEvent {
                queue_id: Some("support-queue".into()),
                ticket_id: format!("ticket-{index}"),
                created_at_utc: "2026-09-01T00:00:00Z".into(),
                resolved_at_utc: (index < 2).then(|| "2026-09-01T12:00:00Z".into()),
            })
            .collect::<Vec<_>>();
        let staffing = vec![duduclaw_gateway::decision_ingest::DailyStaffing {
            queue_id: Some("support-queue".into()),
            day_utc: "2026-09-01T00:00:00Z".into(),
            agents: 1,
            fixed_extra_capacity: 0,
        }];
        let ticket_rows_digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&tickets, &staffing)).unwrap())
        );
        let ticket_export = duduclaw_gateway::decision_ingest::SupportPilotExport {
            snapshot_id: "forecast".into(),
            baseline_scenario_id: "base".into(),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            data_cutoff_utc: "2026-09-02T00:00:00Z".into(),
            source_version_hashes: vec![ticket_rows_digest],
            seed: 1,
            horizon_days: 1,
            tickets,
            staffing,
        };
        std::fs::write(
            &ticket_source_file,
            serde_json::to_vec(&ticket_export).unwrap(),
        )
        .unwrap();
        record_outcome(RecordOutcomeOptions {
            db: db.clone(),
            causal_db: None,
            source_file: ticket_observation_file,
            ticket_source_file: Some(ticket_source_file),
            tenant: "tenant-a".into(),
            ticket_source_retention_until_utc: Some("2027-01-01T00:00:00Z".into()),
            acl: "private".into(),
            observation: "obs-ticket".into(),
            snapshot: "forecast".into(),
            model: "model".into(),
            scenario: "base".into(),
            expected_hash: hash.clone(),
            recorded_by: "reviewer-1".into(),
        })
        .unwrap();
        let ticket_saved = store.get_observed_outcome(&scope, "obs-ticket").unwrap();
        assert!(ticket_saved.ticket_source_sha256.is_some());
        assert_eq!(
            ticket_saved.assessment.resolved_within_sla_abs_error_sum,
            Some(0)
        );
        let scrub = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-scrub-expired-ticket-sources",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
        ])
        .unwrap();
        assert!(matches!(
            scrub.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionScrubExpiredTicketSources { .. })
        ));
        let fit = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-fit-outcome",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--outcome",
            "obs-1",
            "--fit-id",
            "fit-v2",
            "--training-days",
            "3",
        ])
        .unwrap();
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionFitOutcome {
            db,
            causal_db,
            tenant,
            acl,
            outcome,
            fit_id,
            training_days,
            min_saturated_days,
        }) = fit.command
        else {
            panic!("wrong fit command")
        };
        assert_eq!(min_saturated_days, 3);
        assert!(
            fit_outcome(FitOutcomeOptions {
                db,
                causal_db,
                tenant,
                acl,
                outcome,
                fit_id,
                training_days,
                min_saturated_days,
            })
            .is_err(),
            "one observed day cannot identify capacity with a three-day minimum"
        );
        let model_screen = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-screen-outcome-model",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--fit-id",
            "fit-v2",
            "--min-saturated-days",
            "5",
            "--min-holdout-days",
            "7",
            "--save-run",
        ])
        .unwrap();
        assert!(matches!(model_screen.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionScreenOutcomeModel {
                fit_id, min_saturated_days: 5, min_holdout_days: 7,
                save_run: true, ..
            }) if fit_id == "fit-v2"));
        let loaded_screen = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-load-outcome-model-screen",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--replay-hash",
            "screen-hash",
        ])
        .unwrap();
        assert!(matches!(loaded_screen.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionLoadOutcomeModelScreen { replay_hash, .. })
            if replay_hash == "screen-hash"));
        let review_request = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-outcome-model-screen-request-review",
            "--home",
            "home",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--replay-hash",
            "screen-hash",
            "--agent",
            "review-agent",
            "--summary",
            "inspect model fit",
        ])
        .unwrap();
        assert!(matches!(review_request.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionOutcomeModelScreenRequestReview {
                replay_hash, ttl_seconds: 3600, ..
            }) if replay_hash == "screen-hash"));
        let review_check = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-outcome-model-screen-check-review",
            "--home",
            "home",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--replay-hash",
            "screen-hash",
            "--approval",
            "approval-id",
        ])
        .unwrap();
        assert!(matches!(review_check.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionOutcomeModelScreenCheckReview {
                replay_hash, approval, ..
            }) if replay_hash == "screen-hash" && approval == "approval-id"));
        let proposed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-propose-outcome-model",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--screen-hash",
            "screen-hash",
            "--candidate-id",
            "candidate-v2",
        ])
        .unwrap();
        assert!(matches!(proposed.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionProposeOutcomeModel { candidate_id, .. })
            if candidate_id == "candidate-v2"));
        let compared = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-compare-outcome-model-candidate",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--candidate-id",
            "candidate-v2",
            "--target-snapshot",
            "future",
            "--scenario",
            "base",
            "--save-run",
        ])
        .unwrap();
        assert!(matches!(compared.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionCompareOutcomeModelCandidate { target_snapshot, save_run: true, .. })
            if target_snapshot == "future"));
        let loaded_run = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-load-outcome-model-candidate-run",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--replay-hash",
            "run-hash",
        ])
        .unwrap();
        assert!(matches!(loaded_run.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionLoadOutcomeModelCandidateRun { replay_hash, .. })
            if replay_hash == "run-hash"));
        let scored = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-score-outcome-model-candidate",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--comparison-run",
            "run-hash",
            "--outcome",
            "later-outcome",
        ])
        .unwrap();
        assert!(matches!(scored.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionScoreOutcomeModelCandidate { comparison_run, outcome, .. })
            if comparison_run == "run-hash" && outcome == "later-outcome"));
        let loaded_score = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-load-outcome-model-candidate-score",
            "--db",
            "decisions.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--replay-hash",
            "score-hash",
        ])
        .unwrap();
        assert!(matches!(loaded_score.command,
            crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionLoadOutcomeModelCandidateScore { replay_hash, .. })
            if replay_hash == "score-hash"));
    }

    #[test]
    fn review_cli_commands_keep_exact_run_and_scope() {
        run_on_big_stack(review_cli_commands_keep_exact_run_and_scope_body);
    }

    fn review_cli_commands_keep_exact_run_and_scope_body() {
        let request = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-request-review",
            "--home",
            "/tmp/home",
            "--db",
            "/tmp/decision.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--replay-hash",
            "run-hash",
            "--agent",
            "support",
            "--summary",
            "Review staffing pilot",
        ])
        .unwrap();
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionRequestReview {
            tenant,
            acl,
            replay_hash,
            ttl_seconds,
            ..
        }) = request.command
        else {
            panic!("wrong request command")
        };
        assert_eq!(
            (
                tenant.as_str(),
                acl.as_str(),
                replay_hash.as_str(),
                ttl_seconds
            ),
            ("tenant-a", "private", "run-hash", 3600)
        );
        let check = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-check-review",
            "--home",
            "/tmp/home",
            "--db",
            "/tmp/decision.db",
            "--tenant",
            "tenant-a",
            "--acl",
            "private",
            "--replay-hash",
            "run-hash",
            "--approval",
            "approval-id",
        ])
        .unwrap();
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionCheckReview {
            replay_hash,
            approval,
            ..
        }) = check.command
        else {
            panic!("wrong check command")
        };
        assert_eq!(
            (replay_hash.as_str(), approval.as_str()),
            ("run-hash", "approval-id")
        );
    }

    #[test]
    fn demo_persists_exact_synthetic_source_and_replayable_brief() {
        run_on_big_stack(demo_persists_exact_synthetic_source_and_replayable_brief_body);
    }

    fn demo_persists_exact_synthetic_source_and_replayable_brief_body() {
        let dir = tempfile::tempdir().unwrap();
        let options = DemoOptions {
            db: dir.path().join("pilot.sqlite"),
            seed: 47,
            days: 35,
        };
        let report = build_demo(&options).unwrap();
        assert_eq!(report.status, "synthetic_engineering_fixture");
        assert_eq!(report.brief.queue_id.as_deref(), Some("synthetic-support-queue"));
        assert_eq!(report.joint_risk_screen.status, "exploratory_review_screen");
        assert_eq!(report.joint_risk_screen.policy_sweep, report.policy);
        assert_eq!(report.joint_risk_screen.empirical_run_id, report.empirical_run_id);
        assert_eq!(report.joint_risk_screen.empirical_run_sha256, report.empirical_run_sha256);
        assert!(report.joint_risk_screen.failed_alternative_checks.is_empty());
        assert_eq!(
            report.joint_risk_screen.choice,
            duduclaw_gateway::decision_policy::JointRiskScreenChoice::AlternativeForReview,
        );
        assert_eq!(report.capacity_fit.service_per_agent_day, 8);
        assert_eq!(report.empirical_fit.training_days, 21);
        assert_eq!(report.joint_capacity_holdout.holdout_days, 14);
        assert_eq!(report.joint_capacity_holdout.scored_saturated_days, 14);
        assert_eq!(report.joint_capacity_holdout.paired_absolute_error_sum, 0);
        assert_eq!(report.sla_holdout.source_sha256, report.source_sha256);
        assert_eq!(report.sla_holdout.holdout_days, 14);
        assert_eq!(report.sla_holdout.daily_abs_error_sum, 0);
        assert_eq!(report.sla_holdout.one_step_forecast.as_ref().unwrap().evaluation_days, 14);
        assert!(
            report
                .empirical_fit
                .capacity_samples
                .iter()
                .all(|&value| value == 8)
        );
        assert_eq!(
            report.empirical_sensitivity.capacity_source,
            "saturated_day_empirical"
        );
        assert_eq!(report.empirical_sensitivity.runs, 100);
        assert_eq!(report.empirical_plan.min_sla_resolved, Some(525));
        assert!(
            report
                .empirical_sensitivity
                .baseline_sla_target_violation_bps
                .is_some()
        );
        assert!(
            report
                .empirical_sensitivity
                .alternative_sla_target_violation_bps
                .is_some()
        );
        assert_eq!(report.empirical_run.report, report.empirical_sensitivity);
        let brief_empirical = report.brief.exploratory_empirical.as_ref().unwrap();
        assert_eq!(brief_empirical.run_id, report.empirical_run_id);
        assert_eq!(brief_empirical.run_sha256, report.empirical_run_sha256);
        assert_eq!(brief_empirical.fit_sha256, report.empirical_fit_sha256);
        assert_eq!(brief_empirical.plan, report.empirical_plan);
        assert_eq!(brief_empirical.report, report.empirical_sensitivity);
        let brief_event = report.brief.exploratory_event.as_ref().unwrap();
        assert_eq!(brief_event.source_sha256, report.source_sha256);
        assert_eq!(brief_event.baseline_run_hash, report.event_baseline_manifest_id);
        assert_eq!(brief_event.alternative_run_hash, report.event_alternative_manifest_id);
        // The brief renders these totals as decimal strings (a JSON number
        // rounds past 2^53); the stored run keeps the integers.
        assert_eq!(brief_event.baseline.final_backlog,
            report.event_baseline.final_backlog.to_string());
        assert_eq!(brief_event.alternative.final_backlog,
            report.event_alternative.final_backlog.to_string());
        assert_eq!(brief_event.alternative.resolved_wait_seconds_p95,
            report.event_alternative.wait_seconds_p95.map(|value| value.to_string()));
        let brief_forecast = report.brief.exploratory_forecast.as_ref().unwrap();
        assert_eq!(brief_forecast.record_id, report.forecast_validation_id);
        assert_eq!(brief_forecast.record_sha256, report.forecast_validation_sha256);
        assert_eq!(brief_forecast.source_sha256, report.source_sha256);
        assert_eq!(brief_forecast.forecast_points, report.forecast_backtest.evaluation_days);
        assert_eq!(brief_forecast.interval_evaluated_points,
            report.fixed_interval_diagnostic.as_ref().map(|value| value.evaluated_points));
        assert_eq!(brief_forecast.fixed_observed_coverage_basis_points,
            report.fixed_interval_diagnostic.as_ref()
                .map(|value| value.observed_coverage_basis_points));
        let brief_sla = report.brief.exploratory_sla_holdout.as_ref().unwrap();
        assert_eq!(brief_sla.record_id, report.sla_holdout_id);
        assert_eq!(brief_sla.record_sha256, report.sla_holdout_sha256);
        assert_eq!(brief_sla.source_sha256, report.source_sha256);
        // The brief carries the wire mirror (decimal-string u128 sums); the
        // demo report still carries the stored struct it was hashed from.
        assert_eq!(
            brief_sla.diagnostic,
            duduclaw_gateway::decision_dashboard::DecisionSlaHoldoutReport::from(
                report.sla_holdout.clone()
            )
        );
        assert!(report.brief.uncertainty_interval.is_none());
        let combined_options = BriefOptions {
            db: options.db.clone(),
            causal_db: Some(PathBuf::from(&report.causal_db)),
            tenant: "synthetic-demo".into(), acl: "private".into(),
            snapshot: report.brief.replay.snapshot_id.clone(),
            model: report.brief.replay.model_version.clone(),
            baseline: report.brief.replay.baseline_scenario_id.clone(),
            alternative: report.brief.replay.alternative_scenario_id.clone(),
            effect_ids: Vec::new(),
            assumptions: vec!["Synthetic FIFO and fixed service capacity".into()],
            empirical_run: Some(report.empirical_run_id.clone()),
            policy_screen: Some(report.joint_risk_screen_manifest_id.clone()),
            forecast_validation: Some(report.forecast_validation_id.clone()),
            sla_holdout: Some(report.sla_holdout_id.clone()),
            baseline_event_hash: Some(report.event_baseline_manifest_id.clone()),
            alternative_event_hash: Some(report.event_alternative_manifest_id.clone()),
            source_file: Some(PathBuf::from(&report.source_file)),
        };
        let combined = build_brief(&combined_options).unwrap();
        assert_eq!(combined.exploratory_empirical, report.brief.exploratory_empirical);
        assert_eq!(combined.exploratory_event, report.brief.exploratory_event);
        assert_eq!(combined.exploratory_forecast, report.brief.exploratory_forecast);
        assert_eq!(combined.exploratory_sla_holdout, report.brief.exploratory_sla_holdout);
        let policy_evidence = combined.exploratory_policy_screen.as_ref().unwrap();
        assert_eq!(policy_evidence.screen_hash, report.joint_risk_screen_manifest_id);
        assert_eq!(policy_evidence.report, report.joint_risk_screen);
        assert_eq!(combined.delta, report.brief.delta);
        assert!(combined.observational_effects.is_empty());
        let mut incomplete_options = combined_options;
        incomplete_options.alternative_event_hash = None;
        assert!(build_brief(&incomplete_options).is_err());
        incomplete_options.baseline_event_hash = None;
        incomplete_options.empirical_run = None;
        assert!(build_brief(&incomplete_options).is_err());
        incomplete_options.policy_screen = None;
        let forecast_only = build_brief(&incomplete_options).unwrap();
        assert_eq!(forecast_only.exploratory_forecast, report.brief.exploratory_forecast);
        assert_eq!(forecast_only.exploratory_sla_holdout, report.brief.exploratory_sla_holdout);
        assert!(forecast_only.exploratory_event.is_none());
        incomplete_options.forecast_validation = None;
        let sla_only = build_brief(&incomplete_options).unwrap();
        assert!(sla_only.exploratory_forecast.is_none());
        assert_eq!(sla_only.exploratory_sla_holdout, report.brief.exploratory_sla_holdout);
        assert!(sla_only.uncertainty_interval.is_none());
        incomplete_options.source_file = None;
        assert!(build_brief(&incomplete_options).is_err());
        incomplete_options.source_file = Some(PathBuf::from(&report.source_file));
        std::mem::swap(&mut incomplete_options.baseline, &mut incomplete_options.alternative);
        assert!(build_brief(&incomplete_options).is_err());
        assert_eq!(report.empirical_run.seed, options.seed);
        assert_eq!(report.empirical_run.horizon_days, options.days);
        assert_eq!(
            report.empirical_run.source_version_hashes,
            vec![report.source_sha256.clone()]
        );
        assert_eq!(
            report.empirical_run.engine_sha256,
            report.brief.replay.engine_sha256
        );
        let empirical_command = report.empirical_replay_command.clone();
        let parsed = crate::Cli::try_parse_from(
            std::iter::once(empirical_command.program).chain(empirical_command.args),
        )
        .unwrap();
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionEmpiricalReplay {
            db,
            causal_db,
            tenant,
            acl,
            fit,
            run_id,
            model,
            baseline,
            alternative,
            runs,
            arrival_block_days,
            paired_saturated_days,
            max_final_backlog,
            max_staff_cost_cents,
            min_sla_resolved,
            capacity_min,
            capacity_max,
            expected_hash,
        }) = parsed.command
        else {
            panic!("demo did not produce empirical replay arguments")
        };
        let mut empirical_options = EmpiricalReplayOptions {
            db,
            causal_db,
            tenant,
            acl,
            fit,
            run_id,
            model,
            baseline,
            alternative,
            runs,
            arrival_block_days,
            paired_saturated_days,
            max_final_backlog,
            max_staff_cost_cents,
            min_sla_resolved,
            capacity_min,
            capacity_max,
            expected_hash,
        };
        assert_eq!(
            checked_empirical_replay(&empirical_options).unwrap(),
            report.empirical_sensitivity
        );
        empirical_options.max_final_backlog += 1;
        assert!(checked_empirical_replay(&empirical_options).is_err());
        empirical_options.max_final_backlog -= 1;
        empirical_options.min_sla_resolved = None;
        assert!(checked_empirical_replay(&empirical_options).is_err());
        empirical_options.min_sla_resolved = report.empirical_plan.min_sla_resolved;
        empirical_options.expected_hash = Some("wrong".into());
        assert!(checked_empirical_replay(&empirical_options).is_err());
        empirical_options.expected_hash = Some(report.empirical_sensitivity.replay_hash.clone());
        empirical_options.capacity_min = Some(1);
        assert!(checked_empirical_replay(&empirical_options).is_err());
        empirical_options.capacity_min = None;
        let stored_run_id = empirical_options.run_id.take();
        let stored_hash = empirical_options.expected_hash.take();
        let stored_block_days = empirical_options.arrival_block_days;
        empirical_options.arrival_block_days = 1;
        empirical_options.paired_saturated_days = true;
        let paired_report = checked_empirical_replay(&empirical_options).unwrap();
        assert_eq!(paired_report.capacity_source, "saturated_day_paired_empirical");
        empirical_options.expected_hash = Some(paired_report.replay_hash.clone());
        assert_eq!(checked_empirical_replay(&empirical_options).unwrap(), paired_report);
        empirical_options.expected_hash = None;
        empirical_options.arrival_block_days = 7;
        let paired_block_report = checked_empirical_replay(&empirical_options).unwrap();
        assert_eq!(paired_block_report.capacity_source, "saturated_day_paired_block_empirical");
        assert_ne!(paired_block_report.common_draws_sha256, paired_report.common_draws_sha256);
        empirical_options.expected_hash = Some(paired_block_report.replay_hash.clone());
        assert_eq!(checked_empirical_replay(&empirical_options).unwrap(), paired_block_report);
        empirical_options.run_id = stored_run_id;
        empirical_options.expected_hash = stored_hash;
        empirical_options.arrival_block_days = stored_block_days;
        empirical_options.paired_saturated_days = false;
        assert_eq!(report.backtest.model_abs_error_sum, 0);
        assert!(report.forecast_backtest.arrival_abs_error_sum > 0);
        assert_eq!(report.forecast_backtest.evaluation_days, 21);
        assert_eq!(
            report
                .interval_diagnostic
                .as_ref()
                .unwrap()
                .evaluated_points,
            7
        );
        let fixed_interval = report.fixed_interval_diagnostic.as_ref().unwrap();
        assert_eq!(fixed_interval.evaluated_points, 7);
        assert_eq!(fixed_interval.method, "fixed_prefix_absolute_residual_rank90_v1");
        assert!(fixed_interval.points.iter().all(|point|
            point.calibration_radius == fixed_interval.points[0].calibration_radius));
        assert_eq!(report.brief.uncertainty_interval, None);
        assert_eq!(report.brief.status, "exploratory");
        assert_eq!(report.event_baseline.status, "exploratory_event_scenario");
        assert_eq!(
            report.event_baseline.final_backlog.to_string(),
            report.brief.baseline.final_backlog
        );
        assert_eq!(
            report.event_alternative.final_backlog.to_string(),
            report.brief.alternative.final_backlog
        );
        assert!(report.event_baseline.wait_seconds_p95.is_some());
        assert_eq!(report.source_sha256, report.brief.source_version_hashes[0]);
        assert_eq!(report.brief.source_artifact_links.len(), 1);
        let source_link = &report.brief.source_artifact_links[0];
        assert_eq!(source_link.artifact_id, report.source_artifact_id);
        assert_eq!(source_link.source_version_sha256, report.source_sha256);
        assert_eq!(source_link.tenant_id, "synthetic-demo");
        assert_eq!(source_link.acl, "private");
        let source_bytes = std::fs::read(&report.source_file).unwrap();
        assert_eq!(
            report.source_sha256,
            format!("{:x}", Sha256::digest(&source_bytes))
        );
        let command = report.brief.replay.alternative_command;
        let parsed =
            crate::Cli::try_parse_from(std::iter::once(command.program).chain(command.args))
                .unwrap();
        let crate::Commands::Decision(crate::DecisionCommands::DecisionReplay {
            db,
            causal_db,
            tenant,
            acl,
            snapshot,
            model,
            scenario,
            expected_hash,
        }) = parsed.command
        else {
            panic!("demo brief did not produce replay arguments")
        };
        assert_eq!(
            causal_db.as_deref(),
            Some(std::path::Path::new(&report.causal_db))
        );
        let replay_options = ReplayOptions {
            db,
            causal_db,
            tenant,
            acl,
            snapshot,
            model,
            scenario,
            expected_hash,
        };
        assert!(checked_replay(&replay_options).is_ok());
        let event_command = report.event_alternative_command.clone();
        let parsed = crate::Cli::try_parse_from(
            std::iter::once(event_command.program).chain(event_command.args),
        )
        .unwrap();
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionEventReplay {
            db,
            causal_db,
            source_file,
            tenant,
            acl,
            snapshot,
            model,
            scenario,
            window_start,
            baseline_scenario,
            shift_start_seconds,
            shift_seconds,
            expected_hash,
            save_run,
        }) = parsed.command
        else {
            panic!("demo did not produce event replay arguments")
        };
        let mut event_options = EventReplayOptions {
            db,
            causal_db,
            source_file,
            tenant,
            acl,
            snapshot,
            model,
            scenario,
            window_start,
            baseline_scenario,
            shift_start_seconds,
            shift_seconds,
            expected_hash,
            save_run,
        };
        assert_eq!(
            checked_event_replay(&event_options).unwrap(),
            report.event_alternative
        );
        event_options.expected_hash = Some("wrong".into());
        assert!(checked_event_replay(&event_options).is_err());
        event_options.expected_hash = Some(report.event_alternative.replay_hash.clone());
        let original_source_file = event_options.source_file.clone();
        let tampered = dir.path().join("tampered.source.json");
        let mut tampered_bytes = source_bytes.clone();
        tampered_bytes.push(b' ');
        std::fs::write(&tampered, tampered_bytes).unwrap();
        event_options.source_file = tampered;
        assert!(checked_event_replay(&event_options).is_err());
        event_options.source_file = original_source_file;
        let causal = CausalStore::new(report.causal_db.clone());
        let evidence_scope = EvidenceScope {
            tenant_id: replay_options.tenant.clone(),
            acl: replay_options.acl.clone(),
        };
        let decision_store = DecisionStore::with_causal_store(&options.db, causal.clone());
        let decision_scope = DecisionScope {
            tenant_id: replay_options.tenant.clone(),
            acl: replay_options.acl.clone(),
        };
        let stored_forecast = decision_store.load_forecast_validation(
            &decision_scope, &report.forecast_validation_id, &source_bytes,
        ).unwrap();
        let stored_sla = decision_store.load_sla_holdout(
            &decision_scope, &report.sla_holdout_id, &source_bytes,
        ).unwrap();
        assert_eq!(stored_sla.diagnostic, report.sla_holdout);
        assert_eq!(report.sla_holdout_sha256,
            format!("{:x}", Sha256::digest(
                serde_json::to_string(&stored_sla).unwrap().as_bytes())));
        let sla_command = report.sla_holdout_command.clone();
        let parsed = crate::Cli::try_parse_from(
            std::iter::once(sla_command.program).chain(sla_command.args)
        ).unwrap();
        let crate::Commands::Decision(crate::DecisionCommands::DecisionSlaHoldout {
            db, causal_db, source_file, tenant, acl, record, expected_sha256,
        }) = parsed.command else { panic!("wrong SLA holdout command") };
        let mut sla_options = ForecastValidationOptions {
            db, causal_db, source_file, tenant, acl, record, expected_sha256,
        };
        assert_eq!(checked_sla_holdout(&sla_options).unwrap(), stored_sla);
        sla_options.expected_sha256 = Some("wrong".into());
        assert!(checked_sla_holdout(&sla_options).is_err());
        assert!(decision_store.load_sla_holdout(
            &decision_scope, &report.sla_holdout_id, b"changed",
        ).is_err());
        assert_eq!(stored_forecast.forecast, report.forecast_backtest);
        assert_eq!(stored_forecast.rolling_interval, report.interval_diagnostic);
        assert_eq!(stored_forecast.fixed_interval, report.fixed_interval_diagnostic);
        assert_eq!(report.forecast_validation_sha256,
            format!("{:x}", Sha256::digest(
                serde_json::to_string(&stored_forecast).unwrap().as_bytes())));
        let forecast_command = report.forecast_validation_command.clone();
        let parsed = crate::Cli::try_parse_from(
            std::iter::once(forecast_command.program).chain(forecast_command.args)
        ).unwrap();
        let crate::Commands::Decision(crate::DecisionCommands::DecisionForecastValidation {
            db, causal_db, source_file, tenant, acl, record, expected_sha256,
        }) = parsed.command else { panic!("wrong forecast validation command") };
        let mut forecast_options = ForecastValidationOptions {
            db, causal_db, source_file, tenant, acl, record, expected_sha256,
        };
        assert_eq!(checked_forecast_validation(&forecast_options).unwrap(), stored_forecast);
        forecast_options.expected_sha256 = Some("wrong".into());
        assert!(checked_forecast_validation(&forecast_options).is_err());
        forecast_options.expected_sha256 = Some(report.forecast_validation_sha256.clone());
        assert!(decision_store.load_forecast_validation(
            &decision_scope, &report.forecast_validation_id, b"changed",
        ).is_err());
        assert!(matches!(decision_store.put_forecast_validation(
            &decision_scope, &report.forecast_validation_id,
            &report.brief.replay.snapshot_id, &source_bytes,
            &report.brief.exploratory_event.as_ref().unwrap().window_start_utc,
            &report.brief.replay.baseline_scenario_id,
            14, 7, 15,
        ), Err(duduclaw_gateway::decision_store::DecisionStoreError::VersionConflict)));
        let resource_plan_file = dir.path().join("resources.json");
        std::fs::write(
            &resource_plan_file,
            serde_json::to_vec(&report.joint_risk_screen.resource_plan).unwrap(),
        ).unwrap();
        let mut screen_options = EmpiricalScreenOptions {
            db: options.db.clone(),
            causal_db: Some(PathBuf::from(&report.causal_db)),
            tenant: decision_scope.tenant_id.clone(),
            acl: decision_scope.acl.clone(),
            run_id: report.empirical_run_id.clone(),
            resource_plan_file: resource_plan_file.clone(),
            max_joint_violation_bps: report.joint_risk_screen.criteria.max_joint_violation_bps,
            min_joint_recovery_bps: report.joint_risk_screen.criteria.min_joint_recovery_bps,
            min_sla_improvement_bps: report.joint_risk_screen.criteria.min_sla_improvement_bps,
            expected_hash: Some(report.joint_risk_screen.replay_hash.clone()),
            save_run: false,
        };
        assert_eq!(checked_empirical_screen(&screen_options).unwrap(), report.joint_risk_screen);
        screen_options.save_run = true;
        assert_eq!(checked_empirical_screen(&screen_options).unwrap(), report.joint_risk_screen);
        screen_options.save_run = false;
        let stored_screen = decision_store.load_policy_screen(
            &decision_scope, &report.joint_risk_screen_manifest_id,
        ).unwrap();
        assert_eq!(stored_screen.report, report.joint_risk_screen);
        assert_eq!(stored_screen.source_version_hashes, vec![report.source_sha256.clone()]);
        let conn = rusqlite::Connection::open(&options.db).unwrap();
        let (saved_digest, saved_json): (String, String) = conn.query_row(
            "SELECT payload_sha256,payload_json FROM decision_inputs
             WHERE kind='policy_screen' AND input_id=?1",
            [&report.joint_risk_screen_manifest_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        let mut changed: serde_json::Value = serde_json::from_str(&saved_json).unwrap();
        changed["report"]["failed_alternative_checks"] =
            serde_json::json!(["forged_passing_verdict"]);
        let changed_json = serde_json::to_string(&changed).unwrap();
        let changed_digest = format!("{:x}", Sha256::digest(changed_json.as_bytes()));
        conn.execute(
            "UPDATE decision_inputs SET payload_sha256=?1,payload_json=?2
             WHERE kind='policy_screen' AND input_id=?3",
            rusqlite::params![changed_digest, changed_json,
                report.joint_risk_screen_manifest_id],
        ).unwrap();
        assert!(decision_store.load_policy_screen(
            &decision_scope, &report.joint_risk_screen_manifest_id,
        ).is_err());
        conn.execute(
            "UPDATE decision_inputs SET payload_sha256=?1,payload_json=?2
             WHERE kind='policy_screen' AND input_id=?3",
            rusqlite::params![saved_digest, saved_json,
                report.joint_risk_screen_manifest_id],
        ).unwrap();
        screen_options.expected_hash = Some("wrong".into());
        assert!(checked_empirical_screen(&screen_options).is_err());
        screen_options.expected_hash = None;
        let mut constrained = report.joint_risk_screen.resource_plan.clone();
        constrained.available_agents_by_day.fill(2);
        std::fs::write(&resource_plan_file, serde_json::to_vec(&constrained).unwrap()).unwrap();
        let constrained_screen = checked_empirical_screen(&screen_options).unwrap();
        assert_eq!(constrained_screen.choice,
            duduclaw_gateway::decision_policy::JointRiskScreenChoice::NoCandidate);
        assert!(constrained_screen.failed_alternative_checks
            .contains(&"alternative_resource_or_deterministic_limit".to_string()));
        let thresholds = constrained_screen.alternative_review_thresholds.as_ref().unwrap();
        assert!(thresholds.required_available_agents_by_day.iter().all(|&agents| agents == 3));
        assert_eq!(thresholds.required_max_added_agents_per_day, 1);
        assert_eq!(thresholds.required_max_total_agent_days,
            3 * thresholds.required_available_agents_by_day.len() as u64);
        assert_eq!(thresholds.required_max_staff_cost_cents,
            constrained_screen.policy_sweep.points.iter()
                .find(|point| point.service_capacity_per_agent_day
                    == constrained_screen.model_capacity_per_agent_day)
                .unwrap().alternative_staff_cost_cents);
        assert_eq!(thresholds.required_max_joint_violation_bps,
            constrained_screen.alternative_joint_violation_bps);
        assert_eq!(thresholds.highest_min_joint_recovery_bps,
            constrained_screen.alternative_joint_recovery_bps);
        assert_eq!(thresholds.highest_min_sla_improvement_bps,
            constrained_screen.alternative_sla_improvement_bps);
        assert_eq!(decision_store.load_policy_screen(
            &decision_scope, &report.joint_risk_screen_manifest_id,
        ).unwrap(), stored_screen);
        constrained.max_final_backlog += 1;
        std::fs::write(&resource_plan_file, serde_json::to_vec(&constrained).unwrap()).unwrap();
        assert!(checked_empirical_screen(&screen_options).is_err());
        std::fs::write(&resource_plan_file,
            serde_json::to_vec(&report.joint_risk_screen.resource_plan).unwrap()).unwrap();
        screen_options.max_joint_violation_bps = 10_001;
        assert!(checked_empirical_screen(&screen_options).is_err());
        screen_options.max_joint_violation_bps =
            report.joint_risk_screen.criteria.max_joint_violation_bps;
        let stored_event = decision_store
            .load_event_run(&decision_scope, &report.event_alternative_manifest_id, &source_bytes)
            .unwrap();
        assert_eq!(stored_event.result, report.event_alternative);
        assert_eq!(stored_event.source_sha256, report.source_sha256);
        assert!(matches!(
            decision_store.compare_scenarios_with_empirical_and_event_runs(
                &decision_scope,
                &report.brief.replay.snapshot_id,
                &report.brief.replay.model_version,
                &report.brief.replay.baseline_scenario_id,
                &report.brief.replay.alternative_scenario_id,
                &report.empirical_run_id,
                &report.event_alternative_manifest_id,
                &report.event_baseline_manifest_id,
                &source_bytes,
                vec!["swapped event scenarios".into()],
            ),
            Err(duduclaw_gateway::decision_store::DecisionStoreError::Invalid)
        ));
        assert!(decision_store
            .load_event_run(&decision_scope, &report.event_alternative_manifest_id, b"changed")
            .is_err());
        let stored_fit = decision_store
            .load_empirical_fit(&decision_scope, &report.empirical_fit_id)
            .unwrap();
        assert_eq!(stored_fit.fit, report.empirical_fit);
        assert!(matches!(
            decision_store.compare_scenarios_with_empirical_run(
                &decision_scope,
                &report.empirical_run.snapshot_id,
                &report.empirical_run.model_version,
                &report.empirical_run.alternative_id,
                &report.empirical_run.baseline_id,
                &report.empirical_run_id,
                vec!["swapped order".into()],
            ),
            Err(duduclaw_gateway::decision_store::DecisionStoreError::Invalid)
        ));
        assert_eq!(
            decision_store
                .load_empirical_run(&decision_scope, &report.empirical_run_id)
                .unwrap(),
            report.empirical_run
        );
        let wrong_scope = DecisionScope {
            tenant_id: "other".into(),
            acl: decision_scope.acl.clone(),
        };
        assert!(decision_store
            .load_policy_screen(&wrong_scope, &report.joint_risk_screen_manifest_id)
            .is_err());
        assert!(
            decision_store
                .load_empirical_fit(&wrong_scope, &report.empirical_fit_id)
                .is_err()
        );
        assert!(
            decision_store
                .load_empirical_run(&wrong_scope, &report.empirical_run_id)
                .is_err()
        );
        assert!(decision_store
            .load_event_run(&wrong_scope, &report.event_alternative_manifest_id, &source_bytes)
            .is_err());
        let mut changed_plan = report.empirical_plan.clone();
        changed_plan.max_final_backlog += 1;
        assert!(matches!(
            decision_store.put_empirical_run(
                &decision_scope,
                &report.empirical_run_id,
                &report.empirical_fit_id,
                &report.empirical_run.model_version,
                &report.empirical_run.baseline_id,
                &report.empirical_run.alternative_id,
                &changed_plan,
            ),
            Err(duduclaw_gateway::decision_store::DecisionStoreError::VersionConflict)
        ));
        let mut forged_fit = report.empirical_fit.clone();
        forged_fit.arrival_samples[0] += 1;
        let export = synthetic_support_export(options.seed, options.days).unwrap();
        assert!(
            decision_store
                .put_empirical_fit(
                    &decision_scope,
                    "forged-fit",
                    &export.snapshot_id,
                    &source_bytes,
                    &export.window_start_utc,
                    &export.baseline_scenario_id,
                    &forged_fit,
                )
                .is_err()
        );
        let shorter_fit =
            fit_empirical_parameters(&build_support_pilot(&export).unwrap().observed_days, 20, 7)
                .unwrap();
        assert!(matches!(
            decision_store.put_empirical_fit(
                &decision_scope,
                &report.empirical_fit_id,
                &export.snapshot_id,
                &source_bytes,
                &export.window_start_utc,
                &export.baseline_scenario_id,
                &shorter_fit,
            ),
            Err(duduclaw_gateway::decision_store::DecisionStoreError::VersionConflict)
        ));
        assert_eq!(
            causal
                .source_text(&evidence_scope, &report.source_artifact_id)
                .unwrap(),
            String::from_utf8(source_bytes.clone()).unwrap()
        );
        assert!(build_demo(&options).is_err());
        let duplicate = build_demo(&DemoOptions {
            db: dir.path().join("duplicate.sqlite"),
            seed: options.seed,
            days: options.days,
        })
        .unwrap();
        assert_eq!(report.source_sha256, duplicate.source_sha256);
        assert_eq!(
            report.brief.replay.alternative_replay_hash,
            duplicate.brief.replay.alternative_replay_hash
        );
        let other_seed = build_demo(&DemoOptions {
            db: dir.path().join("other-seed.sqlite"),
            seed: 48,
            days: options.days,
        })
        .unwrap();
        assert_ne!(report.source_sha256, other_seed.source_sha256);
        let shorter = build_demo(&DemoOptions {
            db: dir.path().join("shorter.sqlite"),
            seed: 47,
            days: 21,
        })
        .unwrap();
        assert!(shorter.interval_diagnostic.is_none());
        assert!(shorter.fixed_interval_diagnostic.is_none());
        let short = DemoOptions {
            db: dir.path().join("too-short.sqlite"),
            seed: 1,
            days: 7,
        };
        assert!(build_demo(&short).is_err());
        assert!(!short.db.exists());
        let ccr_db = dir.path().join("ccr.db");
        let ccr = duduclaw_llm::CcrStore::new(&ccr_db);
        let ccr_scope = duduclaw_llm::CcrScope {
            tenant_id: replay_options.tenant.clone(), agent_id: "support".into(),
            session_id: "s1".into(), source_acl: "private".into(),
        };
        let artifact_version = causal.source_record_version(
            &evidence_scope, &report.source_artifact_id,
        ).unwrap();
        let ccr_entry = ccr.put_bound(
            &ccr_scope, "search", "call-1", "bound causal source text",
            &duduclaw_llm::CcrSourceArtifact {
                connector: "causal".into(), artifact_id: report.source_artifact_id.clone(),
                version: artifact_version, acl_revision: evidence_scope.acl.clone(),
            },
        ).unwrap();
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "decision-invalidate-source",
            "--db",
            replay_options.db.to_str().unwrap(),
            "--causal-db",
            &report.causal_db,
            "--ccr-db",
            ccr_db.to_str().unwrap(),
            "--tenant",
            &replay_options.tenant,
            "--acl",
            &replay_options.acl,
            "--artifact",
            &report.source_artifact_id,
        ])
        .unwrap();
        assert!(
            crate::Cli::try_parse_from([
                "duduclaw",
                "decision-invalidate-source",
                "--db",
                replay_options.db.to_str().unwrap(),
                "--causal-db",
                &report.causal_db,
                "--tenant",
                &replay_options.tenant,
                "--acl",
                &replay_options.acl,
                "--artifact",
                &report.source_artifact_id,
            ])
            .is_err(),
            "CLI must require a CCR database before any source mutation"
        );
        let crate::Commands::DecisionOutcome(crate::DecisionOutcomeCommands::DecisionInvalidateSource {
            db,
            causal_db,
            ccr_db,
            tenant,
            acl,
            artifact,
        }) = parsed.command
        else {
            panic!("source invalidation CLI was not parsed")
        };
        let invalidate = InvalidateSourceOptions {
            db,
            causal_db,
            ccr_db: Some(ccr_db),
            tenant,
            acl,
            artifact,
        };
        let missing_ccr = InvalidateSourceOptions {
            db: invalidate.db.clone(),
            causal_db: invalidate.causal_db.clone(),
            ccr_db: None,
            tenant: invalidate.tenant.clone(),
            acl: invalidate.acl.clone(),
            artifact: invalidate.artifact.clone(),
        };
        assert!(checked_invalidate_source_all(&missing_ccr).is_err());
        assert!(
            causal
                .source_text(&evidence_scope, &report.source_artifact_id)
                .is_ok(),
            "missing CCR path must refuse before mutating the causal source"
        );
        let first = checked_invalidate_source_all(&invalidate).unwrap();
        assert_eq!(first.0.scrubbed_snapshots, 1);
        assert_eq!(first.1, Some(1));
        assert!(ccr.retrieve(&ccr_scope, &ccr_entry.id, None, 0, 100).is_err());
        let retry = checked_invalidate_source_all(&invalidate).unwrap();
        assert_eq!(retry.0.scrubbed_snapshots, 0);
        assert_eq!(retry.1, Some(0));
        assert!(checked_replay(&replay_options).is_err());
        assert!(checked_event_replay(&event_options).is_err());
        assert!(decision_store
            .load_event_run(&decision_scope, &report.event_alternative_manifest_id, &source_bytes)
            .is_err());
        assert!(decision_store.load_forecast_validation(
            &decision_scope, &report.forecast_validation_id, &source_bytes,
        ).is_err());
        assert!(build_brief(&incomplete_options).is_err());
        assert!(checked_forecast_validation(&forecast_options).is_err());
        assert!(checked_empirical_replay(&empirical_options).is_err());
        assert!(checked_empirical_screen(&screen_options).is_err());
        assert!(decision_store
            .load_policy_screen(&decision_scope, &report.joint_risk_screen_manifest_id)
            .is_err());
        assert!(
            decision_store
                .compare_scenarios(
                    &decision_scope,
                    &report.empirical_run.snapshot_id,
                    &report.empirical_run.model_version,
                    &report.empirical_run.baseline_id,
                    &report.empirical_run.alternative_id,
                    vec!["synthetic".into()],
                )
                .is_err()
        );
        assert!(
            decision_store
                .load_empirical_fit(&decision_scope, &report.empirical_fit_id)
                .is_err()
        );
        assert!(
            decision_store
                .load_empirical_run(&decision_scope, &report.empirical_run_id)
                .is_err()
        );
        let conn = rusqlite::Connection::open(&options.db).unwrap();
        let (digest, payload): (String, String) = conn.query_row(
            "SELECT payload_sha256,payload_json FROM decision_inputs WHERE kind='parameter_fit' AND input_id=?1",
            [&report.empirical_fit_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(digest, report.empirical_fit_sha256);
        assert!(payload.is_empty());
        let (digest, payload): (String, String) = conn.query_row(
            "SELECT payload_sha256,payload_json FROM decision_inputs WHERE kind='empirical_run' AND input_id=?1",
            [&report.empirical_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(digest, report.empirical_run_sha256);
        assert!(payload.is_empty());
    }

    /// Regression: `decision-record-outcome`, `decision-load-outcome-model-screen`
    /// and `decision-event-load-run` printed only the stored record, so a run
    /// captured by an earlier simulator was indistinguishable from one the
    /// installed engine still reproduces. The derived flag must sit beside the
    /// record, never inside it — the record is the hashed payload.
    #[test]
    fn engine_state_report_keeps_the_stored_record_separate_from_the_derived_flag() {
        let stored = serde_json::json!({
            "replay_hash": "a".repeat(64),
            "model_version": "model-1",
        });
        let value = serde_json::to_value(EngineStateReport {
            record: &stored,
            engine_matches_current: false,
        })
        .unwrap();
        assert_eq!(value["engine_matches_current"], serde_json::json!(false));
        assert_eq!(value["record"], stored);
        assert!(value["record"].get("engine_matches_current").is_none());
    }
}
