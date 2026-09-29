//! Admin Decision Lab projections and a reproducible synthetic queue fixture.

use std::collections::BTreeMap;

use duduclaw_memory::causal::EvidenceScope;
use rusqlite::params;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::decision_brief::BriefEventEvidence;
use crate::decision_calibration::{
    CalibrationError, CapacityFit, ForecastBacktestResult, ForecastPoint, IntervalDiagnostic,
    backtest_one_step_forecast, diagnose_fixed_forecast_intervals, fit_capacity,
};
use crate::decision_empirical::{
    CapacityIdentification, EmpiricalSamplingMode, EmpiricalSensitivityPlan,
    fit_empirical_parameters,
};
use crate::decision_event::{EventQueueConfig, EventSimulationResult};
use crate::decision_ingest::{
    DailyStaffing, SlaHoldoutDiagnostic, SlaHoldoutError, SlaOneStepBacktest, SlaOneStepPoint,
    TicketEvent, build_support_pilot, evaluate_ticket_sla_holdout, ticket_sla_label_engine_sha256,
};
use crate::decision_operator_import::OperatorPilotImportReceipt;
use crate::decision_policy::{
    JointRiskScreenChoice, JointRiskScreenCriteria, StaffingResourcePlan,
};
use crate::decision_sensitivity::{
    BoundedCount, SensitivityPlan, SensitivityReport, simulate_sensitivity,
};
use crate::decision_sim::{DecisionSnapshot, QueueModel, StaffingScenario};
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, ShadowDayStatus, ShadowSlaDayStatus,
    StoredEventRun, StoredSlaHoldout,
};
use crate::decision_synthetic::synthetic_support_export;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionCatalog {
    pub snapshots: Vec<DecisionCatalogSnapshot>,
    pub models: Vec<DecisionCatalogModel>,
    pub scenarios: Vec<DecisionCatalogScenario>,
    pub synthetic_pilots: Vec<DecisionCatalogSyntheticPilot>,
    pub uploaded_pilots: Vec<OperatorPilotImportReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionCatalogSyntheticPilot {
    pub snapshot_id: String,
    pub model_version: String,
    pub baseline_scenario_id: String,
    pub alternative_scenario_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionCatalogSnapshot {
    pub id: String,
    pub queue_id: Option<String>,
    pub data_cutoff_utc: String,
    pub horizon_days: usize,
    pub source_kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionCatalogModel {
    pub id: String,
    pub service_capacity_per_agent_day: u32,
    pub sla_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionCatalogScenario {
    pub id: String,
    pub horizon_days: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionSyntheticPilotReceipt {
    pub snapshot_id: String,
    pub model_version: String,
    pub baseline_scenario_id: String,
    pub alternative_scenario_id: String,
    pub source_artifact_id: String,
}

/// Wire mirror of [`ForecastBacktestResult`] whose absolute-error sums are
/// decimal strings. The stored record keeps `u128`; changing it would rewrite
/// every historical payload digest, so only the response shape differs. A JSON
/// number cannot carry these sums exactly once they pass 2^53.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionForecastBacktestReport {
    pub evaluation_days: usize,
    pub arrival_abs_error_sum: String,
    pub model_abs_error_sum: String,
    pub no_change_abs_error_sum: String,
    pub seasonal_naive_abs_error_sum: String,
    pub mean_change_abs_error_sum: String,
    pub model_beats_all_baselines: bool,
    pub points: Vec<ForecastPoint>,
}

impl From<ForecastBacktestResult> for DecisionForecastBacktestReport {
    fn from(value: ForecastBacktestResult) -> Self {
        Self {
            evaluation_days: value.evaluation_days,
            arrival_abs_error_sum: value.arrival_abs_error_sum.to_string(),
            model_abs_error_sum: value.model_abs_error_sum.to_string(),
            no_change_abs_error_sum: value.no_change_abs_error_sum.to_string(),
            seasonal_naive_abs_error_sum: value.seasonal_naive_abs_error_sum.to_string(),
            mean_change_abs_error_sum: value.mean_change_abs_error_sum.to_string(),
            model_beats_all_baselines: value.model_beats_all_baselines,
            points: value.points,
        }
    }
}

/// Wire mirror of [`SlaOneStepBacktest`] with decimal-string error sums.
/// `Deserialize` is derived only so this type can sit inside the decision
/// brief, which is `Deserialize`; nothing parses a report back from the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct DecisionSlaOneStepBacktestReport {
    pub evaluation_days: usize,
    pub arrival_abs_error_sum: String,
    pub model_abs_error_sum: String,
    pub no_change_abs_error_sum: String,
    pub seasonal_naive_abs_error_sum: String,
    pub seven_day_mean_abs_error_sum: String,
    pub model_beats_all_baselines: bool,
    pub points: Vec<SlaOneStepPoint>,
}

impl From<SlaOneStepBacktest> for DecisionSlaOneStepBacktestReport {
    fn from(value: SlaOneStepBacktest) -> Self {
        Self {
            evaluation_days: value.evaluation_days,
            arrival_abs_error_sum: value.arrival_abs_error_sum.to_string(),
            model_abs_error_sum: value.model_abs_error_sum.to_string(),
            no_change_abs_error_sum: value.no_change_abs_error_sum.to_string(),
            seasonal_naive_abs_error_sum: value.seasonal_naive_abs_error_sum.to_string(),
            seven_day_mean_abs_error_sum: value.seven_day_mean_abs_error_sum.to_string(),
            model_beats_all_baselines: value.model_beats_all_baselines,
            points: value.points,
        }
    }
}

/// Wire mirror of [`SlaHoldoutDiagnostic`] with decimal-string error sums.
/// Also the brief's `exploratory_sla_holdout.diagnostic`, so one stored record
/// has exactly one wire shape wherever it is displayed. `Deserialize` is
/// derived only so it can sit inside the `Deserialize` brief.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct DecisionSlaHoldoutReport {
    pub method: String,
    pub source_sha256: String,
    pub model_version: String,
    pub training_days: usize,
    pub holdout_days: usize,
    pub sla_days: u32,
    pub provided_model_capacity_per_agent_day: u32,
    pub fitted_capacity_per_agent_day: u32,
    pub provided_model_matches_fit: bool,
    pub observed_within_sla_by_day: Vec<u32>,
    pub predicted_within_sla_by_day: Vec<u32>,
    pub observed_total_within_sla: u64,
    pub predicted_total_within_sla: u64,
    pub daily_abs_error_sum: String,
    pub no_change_prediction_per_day: u32,
    pub seasonal_naive_prediction_by_day: Vec<u32>,
    pub training_mean_prediction_per_day: u32,
    pub no_change_abs_error_sum: String,
    pub seasonal_naive_abs_error_sum: String,
    pub training_mean_abs_error_sum: String,
    pub conditional_model_beats_all_baselines: bool,
    pub one_step_forecast: Option<DecisionSlaOneStepBacktestReport>,
    pub one_step_unavailable_reason: Option<String>,
    pub limitations: Vec<String>,
}

impl From<SlaHoldoutDiagnostic> for DecisionSlaHoldoutReport {
    fn from(value: SlaHoldoutDiagnostic) -> Self {
        Self {
            method: value.method,
            source_sha256: value.source_sha256,
            model_version: value.model_version,
            training_days: value.training_days,
            holdout_days: value.holdout_days,
            sla_days: value.sla_days,
            provided_model_capacity_per_agent_day: value.provided_model_capacity_per_agent_day,
            fitted_capacity_per_agent_day: value.fitted_capacity_per_agent_day,
            provided_model_matches_fit: value.provided_model_matches_fit,
            observed_within_sla_by_day: value.observed_within_sla_by_day,
            predicted_within_sla_by_day: value.predicted_within_sla_by_day,
            observed_total_within_sla: value.observed_total_within_sla,
            predicted_total_within_sla: value.predicted_total_within_sla,
            daily_abs_error_sum: value.daily_abs_error_sum.to_string(),
            no_change_prediction_per_day: value.no_change_prediction_per_day,
            seasonal_naive_prediction_by_day: value.seasonal_naive_prediction_by_day,
            training_mean_prediction_per_day: value.training_mean_prediction_per_day,
            no_change_abs_error_sum: value.no_change_abs_error_sum.to_string(),
            seasonal_naive_abs_error_sum: value.seasonal_naive_abs_error_sum.to_string(),
            training_mean_abs_error_sum: value.training_mean_abs_error_sum.to_string(),
            conditional_model_beats_all_baselines: value.conditional_model_beats_all_baselines,
            one_step_forecast: value.one_step_forecast.map(Into::into),
            one_step_unavailable_reason: value.one_step_unavailable_reason,
            limitations: value.limitations,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionEngineeringValidation {
    pub status: String,
    pub generated_at_utc: String,
    pub snapshot_id: String,
    pub model_version: String,
    pub source_artifact_id: String,
    pub source_version_sha256: String,
    pub engineering_engine_sha256: String,
    pub simulation_engine_sha256: String,
    pub replay_hash: String,
    pub service_capacity_per_agent_day: u32,
    pub initial_prefix_fit: Option<CapacityFit>,
    pub initial_prefix_unavailable_reason: Option<String>,
    pub provided_model_matches_initial_prefix_fit: Option<bool>,
    pub one_day_backtest: Option<DecisionForecastBacktestReport>,
    pub one_day_backtest_unavailable_reason: Option<String>,
    pub fixed_interval_diagnostic: Option<IntervalDiagnostic>,
    pub fixed_interval_unavailable_reason: Option<String>,
    pub ticket_sla_holdout: Option<DecisionSlaHoldoutReport>,
    pub ticket_sla_unavailable_reason: Option<String>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionEventScenarioSummary {
    pub scenario_id: String,
    pub replay_hash: String,
    pub total_resolved_within_sla: u64,
    pub wait_seconds_p50: Option<u64>,
    pub wait_seconds_p95: Option<u64>,
    pub final_backlog: u64,
    pub total_staff_cost_cents: u64,
}

impl From<&EventSimulationResult> for DecisionEventScenarioSummary {
    fn from(result: &EventSimulationResult) -> Self {
        Self {
            scenario_id: result.scenario_id.clone(),
            replay_hash: result.replay_hash.clone(),
            total_resolved_within_sla: result.total_resolved_within_sla,
            wait_seconds_p50: result.wait_seconds_p50,
            wait_seconds_p95: result.wait_seconds_p95,
            final_backlog: result.final_backlog,
            total_staff_cost_cents: result.total_staff_cost_cents,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionEventComparison {
    pub status: &'static str,
    pub source_origin: &'static str,
    pub source_sha256: String,
    pub event_engine_sha256: String,
    pub baseline: DecisionEventScenarioSummary,
    pub alternative: DecisionEventScenarioSummary,
    pub limitations: Vec<&'static str>,
}

/// Source-bound synthetic empirical fit/run, condensed for Decision Lab. The
/// full records stay in DecisionStore for exact replay and revocation checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionEmpiricalResampling {
    pub status: &'static str,
    pub fit: DecisionEmpiricalFitSummary,
    pub run: DecisionEmpiricalRunSummary,
    pub screen: Option<DecisionEmpiricalScreenSummary>,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionEmpiricalFitSummary {
    pub id: String,
    pub training_days: usize,
    pub min_saturated_days: usize,
    pub capacity_identification: CapacityIdentification,
    pub arrival_sample_count: usize,
    pub capacity_sample_count: usize,
    pub paired_saturated_day_count: usize,
    pub unsaturated_lower_bound_day_count: usize,
    pub partial_saturated_day_count: usize,
    pub max_capacity_lower_bound: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionEmpiricalRunSummary {
    pub id: String,
    pub replay_hash: String,
    pub common_draws_sha256: String,
    pub runs: usize,
    pub arrival_block_days: usize,
    pub sampling_mode: EmpiricalSamplingMode,
    pub capacity_source: String,
    pub delta_backlog_p05: i128,
    pub delta_backlog_p50: i128,
    pub delta_backlog_p95: i128,
    pub delta_sla_resolved_p05: i128,
    pub delta_sla_resolved_p50: i128,
    pub delta_sla_resolved_p95: i128,
    pub baseline_joint_violation_bps: u32,
    pub alternative_joint_violation_bps: u32,
    pub alternative_joint_recovery_bps: u32,
    pub alternative_sla_improvement_bps: u32,
    pub worst_alternative_backlog: u64,
    pub worst_alternative_sla_resolved: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionEmpiricalScreenSummary {
    pub choice: JointRiskScreenChoice,
    pub replay_hash: String,
    pub failed_alternative_checks: Vec<String>,
    pub baseline_deterministic_feasible: bool,
    pub alternative_deterministic_feasible: bool,
}

/// Bounded, content-free projection of a scoped shadow policy. Review screens
/// remain separate human-inspection gates; this monitor only reports the
/// current descriptive assessment and never persists a new result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionShadowMonitor {
    pub status: &'static str,
    pub assessed_at_utc: String,
    pub policy: DecisionShadowMonitorPolicy,
    pub aggregate: DecisionShadowMonitorAssessment,
    pub sla: DecisionShadowMonitorAssessment,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionShadowMonitorPolicy {
    pub id: String,
    pub queue_id: Option<String>,
    pub effective_from_utc: String,
    pub effective_until_utc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionShadowMonitorAssessment {
    pub due_days: usize,
    pub scored_days: usize,
    pub corrected_days: usize,
    pub complete: bool,
    pub whole_window_skill: Option<bool>,
    pub recent_skill: Option<bool>,
    pub fixed_interval_available: bool,
    pub drift_signal: Option<bool>,
    /// Every key is a fixed, safe enum label; no source or score IDs appear.
    pub status_counts: BTreeMap<&'static str, usize>,
}

fn aggregate_status_counts(
    days: &[crate::decision_store::ShadowDayAssessment],
) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::from([
        ("missing_forecast", 0),
        ("forecast_invalid", 0),
        ("forecast_revoked", 0),
        ("unscored", 0),
        ("score_invalid", 0),
        ("score_revoked", 0),
        ("scored", 0),
    ]);
    for day in days {
        let key = match day.status {
            ShadowDayStatus::MissingForecast => "missing_forecast",
            ShadowDayStatus::ForecastInvalid => "forecast_invalid",
            ShadowDayStatus::ForecastRevoked => "forecast_revoked",
            ShadowDayStatus::Unscored => "unscored",
            ShadowDayStatus::ScoreInvalid => "score_invalid",
            ShadowDayStatus::ScoreRevoked => "score_revoked",
            ShadowDayStatus::Scored => "scored",
        };
        *counts.get_mut(key).expect("fixed status key") += 1;
    }
    counts
}

fn sla_status_counts(
    days: &[crate::decision_store::ShadowSlaDayAssessment],
) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::from([
        ("aggregate_unavailable", 0),
        ("missing_sla_forecast", 0),
        ("sla_forecast_invalid", 0),
        ("sla_forecast_revoked", 0),
        ("unscored", 0),
        ("score_invalid", 0),
        ("score_revoked", 0),
        ("score_stale", 0),
        ("cross_day_identity_mismatch", 0),
        ("scored", 0),
    ]);
    for day in days {
        let key = match day.status {
            ShadowSlaDayStatus::AggregateUnavailable => "aggregate_unavailable",
            ShadowSlaDayStatus::MissingSlaForecast => "missing_sla_forecast",
            ShadowSlaDayStatus::SlaForecastInvalid => "sla_forecast_invalid",
            ShadowSlaDayStatus::SlaForecastRevoked => "sla_forecast_revoked",
            ShadowSlaDayStatus::Unscored => "unscored",
            ShadowSlaDayStatus::ScoreInvalid => "score_invalid",
            ShadowSlaDayStatus::ScoreRevoked => "score_revoked",
            ShadowSlaDayStatus::ScoreStale => "score_stale",
            ShadowSlaDayStatus::CrossDayIdentityMismatch => "cross_day_identity_mismatch",
            ShadowSlaDayStatus::Scored => "scored",
        };
        *counts.get_mut(key).expect("fixed status key") += 1;
    }
    counts
}

fn shadow_days_match(
    aggregate: &[crate::decision_store::ShadowDayAssessment],
    sla: &[crate::decision_store::ShadowSlaDayAssessment],
) -> bool {
    aggregate.len() == sla.len()
        && aggregate.iter().zip(sla).all(|(backlog_day, sla_day)| {
            backlog_day.target_day_utc == sla_day.target_day_utc
                && backlog_day.status == sla_day.aggregate_status
        })
}

impl DecisionStore {
    /// Re-evaluate the exact policy scope without exposing per-day identifiers
    /// or source contents. A single captured assessment time is used for both
    /// engines, including the UTC midnight boundary.
    pub fn dashboard_shadow_monitor(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
    ) -> Result<DecisionShadowMonitor, DecisionStoreError> {
        if !scope.valid() || policy_id.trim().is_empty() || policy_id.len() > 512 {
            return Err(DecisionStoreError::Invalid);
        }
        let effective_until = || -> Result<i64, DecisionStoreError> {
            Ok(self.open()?.query_row(
                "SELECT effective_until FROM decision_shadow_policy_windows
                 WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
                params![scope.tenant_id, scope.acl, policy_id],
                |row| row.get(0),
            )?)
        };
        let policy = self.load_shadow_policy(scope, policy_id)?;
        let active_until = effective_until()?;
        let declared_start = chrono::DateTime::parse_from_rfc3339(&policy.effective_from_utc)
            .map_err(|_| DecisionStoreError::Corrupt)?
            .timestamp();
        let declared_end = chrono::DateTime::parse_from_rfc3339(&policy.effective_until_utc)
            .map_err(|_| DecisionStoreError::Corrupt)?
            .timestamp();
        if active_until <= declared_start
            || active_until > declared_end
            || active_until.rem_euclid(86_400) != 0
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let assessed_at = chrono::Utc::now().timestamp();
        let aggregate = self.assess_shadow_policy_at(scope, policy_id, assessed_at)?;
        let sla = self.assess_shadow_sla_policy_at(scope, policy_id, assessed_at)?;
        // SLA re-evaluates aggregate internally. A score correction can change
        // the revision and metrics while both passes still say `Scored`, so
        // matching only the per-day status is insufficient.
        let aggregate_after = self.assess_shadow_policy_at(scope, policy_id, assessed_at)?;
        // A ticket correction can likewise change SLA revisions without
        // changing its `Scored` statuses or any aggregate revision.
        let sla_after = self.assess_shadow_sla_policy_at(scope, policy_id, assessed_at)?;
        // The immutable policy keeps its original end after a reviewed
        // handoff; the reservation window holds the active end. Check both
        // policy and active window again before displaying that end.
        let active_until_after = effective_until()?;
        let policy_after = self.load_shadow_policy(scope, policy_id)?;
        if aggregate.policy_id != policy.id
            || policy != policy_after
            || active_until != active_until_after
            || aggregate != aggregate_after
            || sla != sla_after
            || sla.policy_id != policy.id
            || aggregate.policy_sha256 != sla.policy_sha256
            || aggregate.source_lineage != sla.source_lineage
            || aggregate.queue_id != policy.queue_id
            || sla.queue_id != policy.queue_id
            || aggregate.assessed_at_utc != sla.assessed_at_utc
            || aggregate.due_days != sla.due_days
            || aggregate.days.len() != aggregate.due_days
            || sla.days.len() != sla.due_days
            || aggregate.scored_days > aggregate.due_days
            || sla.scored_days > sla.due_days
            || !shadow_days_match(&aggregate.days, &sla.days)
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let aggregate_counts = aggregate_status_counts(&aggregate.days);
        let sla_counts = sla_status_counts(&sla.days);
        if aggregate_counts.values().sum::<usize>() != aggregate.due_days
            || sla_counts.values().sum::<usize>() != sla.due_days
            || aggregate_counts["scored"] != aggregate.scored_days
            || sla_counts["scored"] != sla.scored_days
            || aggregate.corrected_days
                != aggregate
                    .days
                    .iter()
                    .filter(|day| day.status == ShadowDayStatus::Scored && day.corrected)
                    .count()
            || sla.corrected_days
                != sla
                    .days
                    .iter()
                    .filter(|day| day.status == ShadowSlaDayStatus::Scored && day.corrected)
                    .count()
            || aggregate.complete
                != (aggregate.due_days > 0 && aggregate.scored_days == aggregate.due_days)
            || sla.complete != (sla.due_days > 0 && sla.scored_days == sla.due_days)
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DecisionShadowMonitor {
            status: "descriptive_shadow_monitor",
            assessed_at_utc: aggregate.assessed_at_utc,
            policy: DecisionShadowMonitorPolicy {
                id: policy.id,
                queue_id: policy.queue_id,
                effective_from_utc: policy.effective_from_utc,
                effective_until_utc: chrono::DateTime::<chrono::Utc>::from_timestamp(
                    active_until,
                    0,
                )
                .ok_or(DecisionStoreError::Corrupt)?
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            aggregate: DecisionShadowMonitorAssessment {
                due_days: aggregate.due_days,
                scored_days: aggregate.scored_days,
                corrected_days: aggregate.corrected_days,
                complete: aggregate.complete,
                whole_window_skill: aggregate.backlog_abs_error_below_each_baseline,
                recent_skill: aggregate.recent_7_day_backlog_abs_error_below_each_baseline,
                fixed_interval_available: aggregate.fixed_prefix_interval.is_some(),
                drift_signal: aggregate
                    .fixed_prefix_interval
                    .as_ref()
                    .map(|interval| interval.drift_signal),
                status_counts: aggregate_counts,
            },
            sla: DecisionShadowMonitorAssessment {
                due_days: sla.due_days,
                scored_days: sla.scored_days,
                corrected_days: sla.corrected_days,
                complete: sla.complete,
                whole_window_skill: sla.model_abs_error_below_each_baseline,
                recent_skill: sla.recent_7_day_model_abs_error_below_each_baseline,
                fixed_interval_available: sla.fixed_prefix_interval.is_some(),
                drift_signal: sla
                    .fixed_prefix_interval
                    .as_ref()
                    .map(|interval| interval.drift_signal),
                status_counts: sla_counts,
            },
            limitations: vec![
                "Skill is withheld until every due day has a valid current score.",
                "Residual-band coverage and drift are descriptive, not calibrated production intervals.",
                "This monitor does not authenticate upstream exports, promote a model, or authorize staffing action.",
            ],
        })
    }
}

impl DecisionStore {
    fn dashboard_operator_engineering_validation(
        &self,
        scope: &DecisionScope,
        receipt: &OperatorPilotImportReceipt,
    ) -> Result<DecisionEngineeringValidation, DecisionStoreError> {
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &receipt.snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", &receipt.model_version)?;
        let export = self.verified_operator_export(scope, receipt)?;
        let pilot = build_support_pilot(&export).map_err(|_| DecisionStoreError::Corrupt)?;
        if pilot.snapshot != snapshot || pilot.baseline.id != receipt.baseline_scenario_id {
            return Err(DecisionStoreError::Invalid);
        }
        let (initial_prefix_fit, initial_prefix_unavailable_reason) = if pilot.observed_days.len()
            < 7
        {
            (
                None,
                Some("At least seven complete days are required for a prefix capacity fit.".into()),
            )
        } else {
            match fit_capacity(&pilot.observed_days[..7], 7) {
                Ok(fit) => (Some(fit), None),
                Err(CalibrationError::Unidentified) => (
                    None,
                    Some("The first seven days do not identify per-agent capacity.".into()),
                ),
                Err(error) => return Err(error.into()),
            }
        };
        let (one_day_backtest, one_day_backtest_unavailable_reason) = if pilot.observed_days.len()
            < 15
        {
            (
                None,
                Some("At least fifteen complete days are required for a one-day backtest.".into()),
            )
        } else {
            match backtest_one_step_forecast(&pilot.observed_days, 14, 7) {
                Ok(backtest) => (Some(backtest), None),
                Err(CalibrationError::Unidentified | CalibrationError::InsufficientHistory) => (
                    None,
                    Some(
                        "The historical prefixes do not identify a one-day capacity forecast."
                            .into(),
                    ),
                ),
                Err(error) => return Err(error.into()),
            }
        };
        let (fixed_interval_diagnostic, fixed_interval_unavailable_reason) = if let Some(backtest) =
            &one_day_backtest
        {
            if backtest.points.len() > 14 {
                (Some(diagnose_fixed_forecast_intervals(backtest, 14)?), None)
            } else {
                (None, Some("At least 15 one-day evaluation points are required after the 14-point calibration prefix.".into()))
            }
        } else {
            (None, Some("A valid one-day backtest is required before a fixed residual interval can be shown.".into()))
        };
        let (ticket_sla_holdout, ticket_sla_unavailable_reason) = if export.horizon_days < 14 {
            (
                None,
                Some("SLA holdout requires at least 14 complete days.".into()),
            )
        } else {
            let training_days = export.horizon_days - (export.horizon_days / 3).max(7);
            match evaluate_ticket_sla_holdout(&export, &model, training_days, 7) {
                Ok(report) => (Some(report), None),
                Err(SlaHoldoutError::Capacity(_)) => (
                    None,
                    Some("The training prefix does not identify SLA holdout capacity.".into()),
                ),
                Err(_) => return Err(DecisionStoreError::Corrupt),
            }
        };
        let status = "exploratory_operator_upload".to_owned();
        let limitations = vec![
            "Uploaded source identity, operational definitions, and upstream authenticity are unverified.".into(),
            "The prefix fit and one-day backtest describe this historical export; they do not establish production calibration.".into(),
            "The one-day backtest conditions on actual target-day staffing and opening backlog; it is not an intervention or multi-day demand forecast.".into(),
            "The fixed-prefix interval has no prospective coverage guarantee, and the ticket SLA holdout is not a shadow commitment.".into(),
        ];
        let simulation_engine_sha256 = crate::decision_sim::engine_code_sha256();
        let calibration_engine_sha256 = crate::decision_calibration::calibration_engine_sha256();
        let ticket_engine_sha256 = ticket_sla_label_engine_sha256();
        let dashboard_source_sha256 = format!(
            "{:x}",
            Sha256::digest(include_str!("decision_dashboard.rs").as_bytes())
        );
        let import_source_sha256 = format!(
            "{:x}",
            Sha256::digest(include_str!("decision_operator_import.rs").as_bytes())
        );
        let engineering_engine_sha256 = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                "dashboard-operator-engineering-engine-v1",
                &dashboard_source_sha256,
                &import_source_sha256,
                &calibration_engine_sha256,
                &ticket_engine_sha256,
                &simulation_engine_sha256,
            ))?)
        );
        let replay_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                "dashboard-operator-engineering-validation-v1",
                &status,
                &snapshot,
                &model,
                receipt,
                &engineering_engine_sha256,
                &initial_prefix_fit,
                &initial_prefix_unavailable_reason,
                &one_day_backtest,
                &one_day_backtest_unavailable_reason,
                &fixed_interval_diagnostic,
                &fixed_interval_unavailable_reason,
                &ticket_sla_holdout,
                &ticket_sla_unavailable_reason,
                &limitations,
            ))?)
        );
        self.verify_simulation_inputs_still_current(scope, &snapshot, &model, &[])?;
        self.validate_operator_pilot_receipt(scope, receipt)?;
        Ok(DecisionEngineeringValidation {
            status,
            generated_at_utc: chrono::Utc::now().to_rfc3339(),
            snapshot_id: snapshot.id,
            model_version: model.version,
            source_artifact_id: receipt.source_artifact_id.clone(),
            source_version_sha256: receipt.source_sha256.clone(),
            engineering_engine_sha256,
            simulation_engine_sha256,
            replay_hash,
            service_capacity_per_agent_day: model.service_capacity_per_agent_day,
            provided_model_matches_initial_prefix_fit: initial_prefix_fit
                .as_ref()
                .map(|fit| model.service_capacity_per_agent_day == fit.service_per_agent_day),
            initial_prefix_fit,
            initial_prefix_unavailable_reason,
            one_day_backtest: one_day_backtest.map(Into::into),
            one_day_backtest_unavailable_reason,
            fixed_interval_diagnostic,
            fixed_interval_unavailable_reason,
            ticket_sla_holdout: ticket_sla_holdout.map(Into::into),
            ticket_sla_unavailable_reason,
            limitations,
        })
    }

    /// Recompute descriptive engineering diagnostics from an active canonical
    /// synthetic fixture or completed operator upload. The families retain
    /// distinct provenance and neither result promotes calibration.
    pub fn dashboard_synthetic_engineering_validation(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
    ) -> Result<DecisionEngineeringValidation, DecisionStoreError> {
        if !scope.valid() {
            return Err(DecisionStoreError::Invalid);
        }
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let catalog = self.dashboard_catalog(scope)?;
        if let Some(receipt) = catalog
            .uploaded_pilots
            .iter()
            .find(|pilot| pilot.snapshot_id == snapshot_id && pilot.model_version == model_version)
        {
            return self.dashboard_operator_engineering_validation(scope, receipt);
        }
        if !catalog
            .synthetic_pilots
            .iter()
            .any(|pilot| pilot.snapshot_id == snapshot_id && pilot.model_version == model_version)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let links = self.active_source_links(scope, &snapshot)?;
        if links.len() != 1 {
            return Err(DecisionStoreError::Invalid);
        }
        let link = &links[0];
        let causal = self
            .causal_store()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let metadata = causal.read_artifact_metadata(&evidence_scope, &link.artifact_id)?;
        if metadata.kind != "synthetic_support_export"
            || metadata.content_sha256 != link.source_version_sha256
            || metadata.version != link.source_version_sha256
        {
            return Err(DecisionStoreError::Revoked);
        }
        let source_text = causal.source_text(&evidence_scope, &link.artifact_id)?;
        let digest = format!("{:x}", Sha256::digest(source_text.as_bytes()));
        if digest != link.source_version_sha256 {
            return Err(DecisionStoreError::Revoked);
        }
        let (tickets, staffing): (Vec<TicketEvent>, Vec<DailyStaffing>) =
            serde_json::from_str(&source_text).map_err(|_| DecisionStoreError::Corrupt)?;
        let Some(suffix) = snapshot_id.strip_prefix("synthetic-support-") else {
            return Err(DecisionStoreError::Invalid);
        };
        let Some((seed, days)) = suffix.split_once('-') else {
            return Err(DecisionStoreError::Invalid);
        };
        let seed = seed
            .parse::<u64>()
            .map_err(|_| DecisionStoreError::Invalid)?;
        let days = days
            .parse::<usize>()
            .map_err(|_| DecisionStoreError::Invalid)?;
        if format!("synthetic-support-{seed}-{days}") != snapshot_id
            || days != snapshot.arrivals_by_day.len()
            || model.version != model_version
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let canonical =
            synthetic_support_export(seed, days).map_err(|_| DecisionStoreError::Corrupt)?;
        let canonical_text = serde_json::to_string(&(&tickets, &staffing))?;
        if canonical.tickets != tickets
            || canonical.staffing != staffing
            || canonical_text != source_text
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let mut export = canonical;
        export.snapshot_id = snapshot_id.to_owned();
        export.baseline_scenario_id =
            format!("dashboard-synthetic-baseline-two-agents-{seed}-{days}");
        export.tickets = tickets;
        export.staffing = staffing;
        export.source_version_hashes = vec![digest.clone()];

        let pilot = build_support_pilot(&export).map_err(|_| DecisionStoreError::Corrupt)?;
        if pilot.snapshot != snapshot {
            return Err(DecisionStoreError::VersionConflict);
        }
        let source_fit = fit_capacity(&pilot.observed_days, 7)?;
        if model.service_capacity_per_agent_day != source_fit.service_per_agent_day
            || model.sla_days != 2
            || model.staff_cost_cents_per_agent_day != 10_000
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let initial_prefix_fit = fit_capacity(&pilot.observed_days[..7], 7)?;
        let one_day_backtest = backtest_one_step_forecast(&pilot.observed_days, 14, 7)?;
        let (fixed_interval_diagnostic, fixed_interval_unavailable_reason) = if one_day_backtest
            .points
            .len()
            > 14
        {
            (
                Some(diagnose_fixed_forecast_intervals(&one_day_backtest, 14)?),
                None,
            )
        } else {
            (None, Some("At least 15 one-day evaluation points are required after the 14-point calibration prefix.".into()))
        };
        let (ticket_sla_holdout, ticket_sla_unavailable_reason) =
            match evaluate_ticket_sla_holdout(&export, &model, 7, 3) {
                Ok(report) => (Some(report), None),
                Err(SlaHoldoutError::Capacity(error)) => (None, Some(error.to_string())),
                Err(
                    SlaHoldoutError::Invalid
                    | SlaHoldoutError::SourceMismatch
                    | SlaHoldoutError::Import(_)
                    | SlaHoldoutError::Simulation(_),
                ) => {
                    return Err(DecisionStoreError::Corrupt);
                }
            };
        let provided_model_matches_initial_prefix_fit =
            model.service_capacity_per_agent_day == initial_prefix_fit.service_per_agent_day;
        let status = "synthetic_only_exploratory".to_owned();
        let limitations = vec![
            "Synthetic fixture only; these diagnostics are not real calibration or production evidence.".into(),
            "The one-day backtest conditions on actual target-day staffing and opening backlog; it is not an intervention or multi-day demand forecast.".into(),
            "The fixed-prefix interval is a descriptive residual diagnostic without a coverage guarantee.".into(),
            "Ticket SLA holdout reconstructs historical opening cohorts and is not a prospective shadow commitment.".into(),
        ];
        // Bind the public validation digest to every implementation that
        // computes a displayed diagnostic, including the queue simulator used
        // by the ticket-level SLA holdout.
        let simulation_engine_sha256 = crate::decision_sim::engine_code_sha256();
        let calibration_engine_sha256 = crate::decision_calibration::calibration_engine_sha256();
        let ticket_engine_sha256 = ticket_sla_label_engine_sha256();
        let dashboard_source_sha256 = format!(
            "{:x}",
            Sha256::digest(include_str!("decision_dashboard.rs").as_bytes())
        );
        let synthetic_source_sha256 = format!(
            "{:x}",
            Sha256::digest(include_str!("decision_synthetic.rs").as_bytes())
        );
        let engineering_engine_sha256 = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                "dashboard-synthetic-engineering-engine-v2",
                &dashboard_source_sha256,
                &synthetic_source_sha256,
                &calibration_engine_sha256,
                &ticket_engine_sha256,
                &simulation_engine_sha256,
            ))?)
        );
        let replay_manifest = serde_json::to_vec(&(
            "dashboard-synthetic-engineering-validation-v2",
            &status,
            &snapshot,
            &model,
            &digest,
            (
                &calibration_engine_sha256,
                &ticket_engine_sha256,
                &simulation_engine_sha256,
                &synthetic_source_sha256,
                &engineering_engine_sha256,
            ),
            &initial_prefix_fit,
            provided_model_matches_initial_prefix_fit,
            &one_day_backtest,
            &fixed_interval_diagnostic,
            &fixed_interval_unavailable_reason,
            &ticket_sla_holdout,
            &ticket_sla_unavailable_reason,
            &limitations,
        ))?;
        let replay_hash = format!("{:x}", Sha256::digest(replay_manifest));
        self.verify_simulation_inputs_still_current(scope, &snapshot, &model, &[])?;
        // Re-read active metadata after computation so a concurrent revocation
        // cannot return a stale diagnostic.
        let current_source = causal.read_artifact_metadata(&evidence_scope, &link.artifact_id)?;
        if current_source.content_sha256 != digest || current_source.version != digest {
            return Err(DecisionStoreError::Revoked);
        }
        Ok(DecisionEngineeringValidation {
            status,
            generated_at_utc: chrono::Utc::now().to_rfc3339(),
            snapshot_id: snapshot_id.into(),
            model_version: model.version,
            source_artifact_id: link.artifact_id.clone(),
            source_version_sha256: digest,
            engineering_engine_sha256,
            simulation_engine_sha256,
            replay_hash,
            service_capacity_per_agent_day: model.service_capacity_per_agent_day,
            initial_prefix_fit: Some(initial_prefix_fit),
            initial_prefix_unavailable_reason: None,
            provided_model_matches_initial_prefix_fit: Some(
                provided_model_matches_initial_prefix_fit,
            ),
            one_day_backtest: Some(one_day_backtest.into()),
            one_day_backtest_unavailable_reason: None,
            fixed_interval_diagnostic,
            fixed_interval_unavailable_reason,
            ticket_sla_holdout: ticket_sla_holdout.map(Into::into),
            ticket_sla_unavailable_reason,
            limitations,
        })
    }

    /// Replay the ticket-level FIFO queue for one complete dashboard pilot
    /// pair. Only aggregate results leave this boundary; the source stays in
    /// the exact scoped store and is revalidated after both manifests commit.
    pub fn dashboard_event_comparison(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
    ) -> Result<DecisionEventComparison, DecisionStoreError> {
        self.dashboard_event_comparison_with_source(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
        )
        .map(|(report, _)| report)
    }

    /// The source bytes stay inside the gateway process and are only used to
    /// load exact event runs into an empirical decision brief.
    pub(crate) fn dashboard_event_comparison_with_source(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
    ) -> Result<(DecisionEventComparison, Vec<u8>), DecisionStoreError> {
        if !scope.valid() || baseline_id == alternative_id {
            return Err(DecisionStoreError::Invalid);
        }
        let catalog = self.dashboard_catalog(scope)?;
        let synthetic_pair = catalog.synthetic_pilots.iter().any(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        let uploaded_pair = catalog.uploaded_pilots.iter().find(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        if !synthetic_pair && uploaded_pair.is_none() {
            return Err(DecisionStoreError::NotFound);
        }
        if synthetic_pair && uploaded_pair.is_some() {
            return Err(DecisionStoreError::Invalid);
        }

        // This shared verifier checks the active source, snapshot and model;
        // for synthetic pilots it additionally proves generator identity.
        let verified =
            self.dashboard_synthetic_engineering_validation(scope, snapshot_id, model_version)?;
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", baseline_id)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", alternative_id)?;
        let export = if let Some(receipt) = uploaded_pair {
            self.verified_operator_export(scope, receipt)?
        } else {
            let mut export =
                synthetic_support_export(snapshot.seed, snapshot.arrivals_by_day.len())
                    .map_err(|_| DecisionStoreError::Corrupt)?;
            export.baseline_scenario_id = baseline_id.into();
            export
        };
        let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
        let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
        let imported = build_support_pilot(&export).map_err(|_| DecisionStoreError::Corrupt)?;
        if source_sha256 != verified.source_version_sha256
            || imported.snapshot != snapshot
            || imported.baseline != baseline
            || export.baseline_scenario_id != baseline_id
        {
            return Err(DecisionStoreError::Corrupt);
        }

        let config = EventQueueConfig {
            shift_start_seconds: 9 * 3_600,
            shift_seconds: 8 * 3_600,
        };
        let baseline_run = self.put_event_run(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            &source_bytes,
            &export.window_start_utc,
            baseline_id,
            &config,
        )?;
        let alternative_run = self.put_event_run(
            scope,
            snapshot_id,
            model_version,
            alternative_id,
            &source_bytes,
            &export.window_start_utc,
            baseline_id,
            &config,
        )?;
        if baseline_run.result.event_engine_sha256 != alternative_run.result.event_engine_sha256 {
            return Err(DecisionStoreError::Corrupt);
        }

        let current =
            self.dashboard_synthetic_engineering_validation(scope, snapshot_id, model_version)?;
        if current.source_artifact_id != verified.source_artifact_id
            || current.source_version_sha256 != source_sha256
        {
            return Err(DecisionStoreError::Revoked);
        }
        if let Some(receipt) = uploaded_pair {
            let current_export = self.verified_operator_export(scope, receipt)?;
            if serde_json::to_vec(&(&current_export.tickets, &current_export.staffing))?
                != source_bytes
                || current_export.window_start_utc != export.window_start_utc
            {
                return Err(DecisionStoreError::Revoked);
            }
        }
        self.verify_simulation_inputs_still_current(
            scope,
            &snapshot,
            &model,
            &[&baseline, &alternative],
        )?;
        let current_catalog = self.dashboard_catalog(scope)?;
        let still_paired = if synthetic_pair {
            current_catalog.synthetic_pilots.iter().any(|pilot| {
                pilot.snapshot_id == snapshot_id
                    && pilot.model_version == model_version
                    && pilot.baseline_scenario_id == baseline_id
                    && pilot.alternative_scenario_id == alternative_id
            })
        } else {
            current_catalog.uploaded_pilots.iter().any(|pilot| {
                pilot.snapshot_id == snapshot_id
                    && pilot.model_version == model_version
                    && pilot.baseline_scenario_id == baseline_id
                    && pilot.alternative_scenario_id == alternative_id
            })
        };
        if !still_paired {
            return Err(DecisionStoreError::Revoked);
        }

        Ok((
            DecisionEventComparison {
                status: if synthetic_pair {
                    "synthetic_only_exploratory"
                } else {
                    "operator_supplied_exploratory"
                },
                source_origin: if synthetic_pair {
                    "synthetic"
                } else {
                    "uploaded"
                },
                source_sha256,
                event_engine_sha256: baseline_run.result.event_engine_sha256.clone(),
                baseline: (&baseline_run.result).into(),
                alternative: (&alternative_run.result).into(),
                limitations: vec![
                    "Fixed 09:00–17:00 UTC service slots are an illustrative queue assumption.",
                    "Wait quantiles include resolved tickets only; unresolved waits are censored.",
                    "Source consistency and replay are checked locally, not upstream authenticity or a measured staffing effect.",
                ],
            },
            source_bytes,
        ))
    }

    /// A post-brief fence over immutable manifests and the currently active
    /// exact source. The first comparison already simulated both event runs;
    /// this path intentionally performs no ticket-event simulation or
    /// engineering diagnostic a second time.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn verify_dashboard_event_evidence_still_current(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        expected: &DecisionEventComparison,
        source_bytes: &[u8],
        evidence: &BriefEventEvidence,
    ) -> Result<(), DecisionStoreError> {
        let catalog = self.dashboard_catalog(scope)?;
        let synthetic = catalog.synthetic_pilots.iter().any(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        let uploaded = catalog.uploaded_pilots.iter().find(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        if synthetic == uploaded.is_some()
            || (synthetic
                && (expected.source_origin != "synthetic"
                    || expected.status != "synthetic_only_exploratory"))
            || (uploaded.is_some()
                && (expected.source_origin != "uploaded"
                    || expected.status != "operator_supplied_exploratory"))
        {
            return Err(DecisionStoreError::Revoked);
        }

        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", baseline_id)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", alternative_id)?;
        let export = if let Some(receipt) = uploaded {
            self.verified_operator_export(scope, receipt)?
        } else {
            let mut canonical =
                synthetic_support_export(snapshot.seed, snapshot.arrivals_by_day.len())
                    .map_err(|_| DecisionStoreError::Corrupt)?;
            canonical.baseline_scenario_id = baseline_id.into();
            let links = self.active_source_links(scope, &snapshot)?;
            if links.len() != 1 {
                return Err(DecisionStoreError::Revoked);
            }
            let causal = self
                .causal_store()
                .ok_or(DecisionStoreError::CausalStoreRequired)?;
            let evidence_scope = EvidenceScope {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
            };
            let metadata = causal.read_artifact_metadata(&evidence_scope, &links[0].artifact_id)?;
            let stored_source = causal.source_text(&evidence_scope, &links[0].artifact_id)?;
            if metadata.kind != "synthetic_support_export"
                || metadata.content_sha256 != expected.source_sha256
                || metadata.version != expected.source_sha256
                || links[0].source_version_sha256 != expected.source_sha256
                || stored_source.as_bytes() != source_bytes
            {
                return Err(DecisionStoreError::Revoked);
            }
            canonical
        };
        let current_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
        let source_sha256 = format!("{:x}", Sha256::digest(source_bytes));
        let imported = build_support_pilot(&export).map_err(|_| DecisionStoreError::Corrupt)?;
        if current_bytes != source_bytes
            || source_sha256 != expected.source_sha256
            || snapshot.source_version_hashes != [source_sha256.clone()]
            || imported.snapshot != snapshot
            || imported.baseline != baseline
            || export.baseline_scenario_id != baseline_id
            || evidence.source_sha256 != source_sha256
            || evidence.window_start_utc != export.window_start_utc
            || evidence.event_engine_sha256 != expected.event_engine_sha256
            || evidence.baseline_run_hash != expected.baseline.replay_hash
            || evidence.alternative_run_hash != expected.alternative.replay_hash
        {
            return Err(DecisionStoreError::Revoked);
        }

        let baseline_run =
            self.load_event_run(scope, &expected.baseline.replay_hash, source_bytes)?;
        let alternative_run =
            self.load_event_run(scope, &expected.alternative.replay_hash, source_bytes)?;
        let (stored_baseline, baseline_digest): (StoredEventRun, String) =
            self.get_with_digest(scope, "event_run", &expected.baseline.replay_hash)?;
        let (stored_alternative, alternative_digest): (StoredEventRun, String) =
            self.get_with_digest(scope, "event_run", &expected.alternative.replay_hash)?;
        let current_event_engine = format!(
            "{:x}",
            Sha256::digest(include_str!("decision_event.rs").as_bytes())
        );
        let valid_run = |run: &StoredEventRun, scenario_id: &str| {
            run.snapshot_id == snapshot_id
                && run.model_version == model_version
                && run.scenario_id == scenario_id
                && run.baseline_scenario_id == baseline_id
                && run.source_sha256 == source_sha256
                && run.window_start_utc == export.window_start_utc
                && run.config == evidence.config
                && run.daily_engine_sha256 == crate::decision_sim::engine_code_sha256()
                && run.result.event_engine_sha256 == current_event_engine
        };
        if baseline_run != stored_baseline
            || alternative_run != stored_alternative
            || !valid_run(&baseline_run, baseline_id)
            || !valid_run(&alternative_run, alternative_id)
            || baseline_digest != evidence.baseline_run_sha256
            || alternative_digest != evidence.alternative_run_sha256
            || DecisionEventScenarioSummary::from(&baseline_run.result) != expected.baseline
            || DecisionEventScenarioSummary::from(&alternative_run.result) != expected.alternative
            || baseline_run.result.event_engine_sha256 != expected.event_engine_sha256
            || alternative_run.result.event_engine_sha256 != expected.event_engine_sha256
        {
            return Err(DecisionStoreError::Revoked);
        }
        self.verify_simulation_inputs_still_current(
            scope,
            &snapshot,
            &model,
            &[&baseline, &alternative],
        )?;
        if let Some(receipt) = uploaded {
            let current: OperatorPilotImportReceipt =
                self.get(scope, "uploaded_pilot_receipt", snapshot_id)?;
            if &current != receipt {
                return Err(DecisionStoreError::Revoked);
            }
        }
        Ok(())
    }

    /// Resolve a completed upload's exact ticket/staffing source for the
    /// brief loader. The caller repeats this after composition and compares
    /// both the receipt and bytes before returning the brief.
    pub(crate) fn dashboard_uploaded_sla_holdout_source(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        holdout_id: &str,
    ) -> Result<(OperatorPilotImportReceipt, Vec<u8>), DecisionStoreError> {
        let catalog = self.dashboard_catalog(scope)?;
        let receipt = catalog
            .uploaded_pilots
            .into_iter()
            .find(|pilot| {
                pilot.snapshot_id == snapshot_id
                    && pilot.model_version == model_version
                    && pilot.baseline_scenario_id == baseline_id
                    && pilot.alternative_scenario_id == alternative_id
            })
            .ok_or(DecisionStoreError::NotFound)?;
        if receipt.sla_holdout_id.as_deref() != Some(holdout_id) {
            return Err(DecisionStoreError::Invalid);
        }
        let export = self.verified_operator_export(scope, &receipt)?;
        let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
        if format!("{:x}", Sha256::digest(&source_bytes)) != receipt.source_sha256 {
            return Err(DecisionStoreError::Revoked);
        }
        let (record, record_digest): (StoredSlaHoldout, String) =
            self.get_with_digest(scope, "sla_holdout", holdout_id)?;
        if receipt.sla_holdout_sha256.as_deref() != Some(record_digest.as_str())
            || record.snapshot_id != snapshot_id
            || record.model_version != model_version
            || record.baseline_scenario_id != baseline_id
            || record.source_sha256 != receipt.source_sha256
            || record.window_start_utc != receipt.window_start_utc
        {
            return Err(DecisionStoreError::Revoked);
        }
        let current_receipt: OperatorPilotImportReceipt =
            self.get(scope, "uploaded_pilot_receipt", snapshot_id)?;
        if current_receipt != receipt {
            return Err(DecisionStoreError::Revoked);
        }
        Ok((receipt, source_bytes))
    }

    /// Fit only a verified active source training prefix, store an immutable
    /// source-bound empirical run, and optionally apply the review screen.
    /// Caller-supplied observations and fit IDs are never accepted.
    #[allow(clippy::too_many_arguments)]
    pub fn dashboard_empirical_resampling(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        training_days: usize,
        min_saturated_days: usize,
        plan: &EmpiricalSensitivityPlan,
        screen: Option<(&StaffingResourcePlan, &JointRiskScreenCriteria)>,
    ) -> Result<DecisionEmpiricalResampling, DecisionStoreError> {
        if !scope.valid()
            || !(1..=1_000).contains(&plan.runs)
            || plan.arrival_block_days == 0
            || !(1..=512).contains(&min_saturated_days)
            || plan.max_final_backlog > 1_000_000
            || plan.max_staff_cost_cents > 1_000_000_000_000
            || plan.min_sla_resolved.is_some_and(|value| value > 1_000_000)
            || plan
                .capacity_fallback_range
                .is_some_and(|range| range.min == 0 || range.min >= range.max || range.max > 512)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let catalog = self.dashboard_catalog(scope)?;
        let synthetic_pair = catalog.synthetic_pilots.iter().any(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        let uploaded_pair = catalog.uploaded_pilots.iter().find(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        if !synthetic_pair && uploaded_pair.is_none() {
            return Err(DecisionStoreError::Invalid);
        }
        // Both families reload an exact active source and source-bound model.
        // The synthetic branch additionally proves generator identity.
        let verified =
            self.dashboard_synthetic_engineering_validation(scope, snapshot_id, model_version)?;
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", baseline_id)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", alternative_id)?;
        let days = snapshot.arrivals_by_day.len();
        if training_days < 7
            || training_days > days.saturating_sub(3)
            || min_saturated_days > training_days
            || plan.arrival_block_days > training_days
        {
            return Err(DecisionStoreError::Invalid);
        }
        let export = if let Some(receipt) = uploaded_pair {
            self.verified_operator_export(scope, receipt)?
        } else {
            let mut export = synthetic_support_export(snapshot.seed, days)
                .map_err(|_| DecisionStoreError::Corrupt)?;
            if export.snapshot_id != snapshot_id
                || export.source_version_hashes != vec![verified.source_version_sha256.clone()]
            {
                return Err(DecisionStoreError::Corrupt);
            }
            export.baseline_scenario_id = baseline_id.into();
            export
        };
        let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
        if format!("{:x}", Sha256::digest(&source_bytes)) != verified.source_version_sha256 {
            return Err(DecisionStoreError::Corrupt);
        }
        let pilot = build_support_pilot(&export).map_err(|_| DecisionStoreError::Corrupt)?;
        if pilot.snapshot != snapshot {
            return Err(DecisionStoreError::VersionConflict);
        }
        let fit =
            fit_empirical_parameters(&pilot.observed_days, training_days, min_saturated_days)?;
        if (fit.capacity_identification == CapacityIdentification::Unidentified
            && (plan.capacity_fallback_range.is_none()
                || plan.sampling_mode != EmpiricalSamplingMode::Independent))
            || (fit.capacity_identification == CapacityIdentification::EmpiricalSaturatedDays
                && plan.capacity_fallback_range.is_some())
        {
            return Err(DecisionStoreError::Invalid);
        }
        if let Some((resource, criteria)) = screen {
            if plan.min_sla_resolved.is_none()
                || resource.max_final_backlog != plan.max_final_backlog
                || resource.max_staff_cost_cents != plan.max_staff_cost_cents
                || resource.available_agents_by_day.len() != days
                || resource
                    .available_agents_by_day
                    .iter()
                    .any(|&agents| agents > 512)
                || resource.max_added_agents_per_day > 512
                || resource.max_total_agent_days > 512 * 366
                || resource.service_capacity_band.min == 0
                || resource.service_capacity_band.min > resource.service_capacity_band.max
                || resource.service_capacity_band.max > 512
                || criteria.max_joint_violation_bps > 10_000
                || criteria.min_joint_recovery_bps > 10_000
                || criteria.min_sla_improvement_bps > 10_000
            {
                return Err(DecisionStoreError::Invalid);
            }
        }
        let family = if uploaded_pair.is_some() {
            "operator"
        } else {
            "synthetic"
        };
        let fit_namespace = if uploaded_pair.is_some() {
            "dashboard-operator-empirical-fit-v1"
        } else {
            "dashboard-synthetic-empirical-fit-v1"
        };
        let fit_id = format!(
            "dashboard-{family}-empirical-fit-{:x}",
            Sha256::digest(serde_json::to_vec(&(
                fit_namespace,
                snapshot_id,
                &verified.source_version_sha256,
                training_days,
                min_saturated_days,
            ))?)
        );
        self.put_empirical_fit(
            scope,
            &fit_id,
            snapshot_id,
            &source_bytes,
            &export.window_start_utc,
            baseline_id,
            &fit,
        )?;
        let stored_fit = self.load_empirical_fit(scope, &fit_id)?;
        if stored_fit.fit != fit {
            return Err(DecisionStoreError::Corrupt);
        }
        let run_namespace = if uploaded_pair.is_some() {
            "dashboard-operator-empirical-run-v1"
        } else {
            "dashboard-synthetic-empirical-run-v1"
        };
        let run_id = format!(
            "dashboard-{family}-empirical-run-{:x}",
            Sha256::digest(serde_json::to_vec(&(
                run_namespace,
                &fit_id,
                model_version,
                baseline_id,
                alternative_id,
                plan,
            ))?)
        );
        self.put_empirical_run(
            scope,
            &run_id,
            &fit_id,
            model_version,
            baseline_id,
            alternative_id,
            plan,
        )?;
        let stored_run = self.load_empirical_run(scope, &run_id)?;
        let screen = if let Some((resource, criteria)) = screen {
            // Preserve the exact screen as immutable, source-bound evidence so
            // the dashboard can compose it into a later verified brief.
            let report = self
                .put_policy_screen(scope, &run_id, resource, criteria)?
                .report;
            Some(DecisionEmpiricalScreenSummary {
                choice: report.choice,
                replay_hash: report.replay_hash,
                failed_alternative_checks: report.failed_alternative_checks,
                baseline_deterministic_feasible: report.baseline_deterministic_feasible,
                alternative_deterministic_feasible: report.alternative_deterministic_feasible,
            })
        } else {
            None
        };
        // An export erased or rebound while calculation ran cannot be
        // presented as current. The store check also revalidates scenarios.
        let verified_after =
            self.dashboard_synthetic_engineering_validation(scope, snapshot_id, model_version)?;
        if verified.source_artifact_id != verified_after.source_artifact_id
            || verified.source_version_sha256 != verified_after.source_version_sha256
        {
            return Err(DecisionStoreError::Revoked);
        }
        self.verify_simulation_inputs_still_current(
            scope,
            &snapshot,
            &model,
            &[&baseline, &alternative],
        )?;
        let report = stored_run.report;
        let (status, limitations) = if uploaded_pair.is_some() {
            (
                "exploratory_operator_upload",
                vec![
                    "Only the active operator-uploaded historical export was sampled; upstream identity and operational definitions are unverified.",
                    "Common draws compare two fixed staffing scenarios under declared limits; fractions are exploratory, not calibrated probabilities or intervention effects.",
                    "A review-screen candidate does not promote a model or authorize staffing changes.",
                ],
            )
        } else {
            (
                "synthetic_only_exploratory",
                vec![
                    "Only the verified canonical synthetic support export was sampled; no real demand distribution was fitted.",
                    "Common draws compare these two fixed staffing scenarios under the declared limits; fractions are exploratory, not calibrated probabilities.",
                    "A review-screen candidate does not promote a model or authorize staffing changes.",
                ],
            )
        };
        Ok(DecisionEmpiricalResampling {
            status,
            fit: DecisionEmpiricalFitSummary {
                id: fit_id,
                training_days: fit.training_days,
                min_saturated_days: fit.min_saturated_days,
                capacity_identification: fit.capacity_identification,
                arrival_sample_count: fit.arrival_samples.len(),
                capacity_sample_count: fit.capacity_samples.len(),
                paired_saturated_day_count: fit.saturated_day_pairs.len(),
                unsaturated_lower_bound_day_count: fit.unsaturated_day_lower_bounds.len(),
                partial_saturated_day_count: fit.partial_saturated_day_lower_bounds.len(),
                max_capacity_lower_bound: fit
                    .unsaturated_day_lower_bounds
                    .iter()
                    .map(|bound| bound.minimum_capacity_per_agent)
                    .chain(
                        fit.partial_saturated_day_lower_bounds
                            .iter()
                            .map(|bound| bound.minimum_capacity_per_agent),
                    )
                    .max()
                    .unwrap_or(0),
            },
            run: DecisionEmpiricalRunSummary {
                id: run_id,
                replay_hash: report.replay_hash,
                common_draws_sha256: report.common_draws_sha256,
                runs: report.runs,
                arrival_block_days: report.arrival_block_days,
                sampling_mode: report.sampling_mode,
                capacity_source: report.capacity_source,
                delta_backlog_p05: report.delta_backlog_p05,
                delta_backlog_p50: report.delta_backlog_p50,
                delta_backlog_p95: report.delta_backlog_p95,
                delta_sla_resolved_p05: report.delta_sla_resolved_p05,
                delta_sla_resolved_p50: report.delta_sla_resolved_p50,
                delta_sla_resolved_p95: report.delta_sla_resolved_p95,
                baseline_joint_violation_bps: report.baseline_joint_constraint_violation_bps,
                alternative_joint_violation_bps: report.alternative_joint_constraint_violation_bps,
                alternative_joint_recovery_bps: report.alternative_joint_constraint_recovery_bps,
                alternative_sla_improvement_bps: report.alternative_more_sla_resolved_bps,
                worst_alternative_backlog: report.worst_alternative_backlog,
                worst_alternative_sla_resolved: report.worst_alternative_sla_resolved,
            },
            screen,
            limitations,
        })
    }

    /// Use one declared uniform arrival perturbation and paired draws for an
    /// exploratory dashboard sensitivity check. These bands are user inputs,
    /// not fitted forecast distributions.
    pub fn dashboard_uniform_sensitivity(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        runs: usize,
        arrival_delta: u32,
        capacity_min: u32,
        capacity_max: u32,
        max_final_backlog: u64,
        max_staff_cost_cents: u64,
    ) -> Result<SensitivityReport, DecisionStoreError> {
        if !scope.valid()
            || !(1..=1000).contains(&runs)
            || arrival_delta > 1000
            || capacity_min == 0
            || capacity_min > capacity_max
            || capacity_max > 512
        {
            return Err(DecisionStoreError::Invalid);
        }
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", baseline_id)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", alternative_id)?;
        let daily_arrival_bands = snapshot
            .arrivals_by_day
            .iter()
            .map(|&arrivals| -> Result<BoundedCount, DecisionStoreError> {
                Ok(BoundedCount {
                    min: arrivals.saturating_sub(arrival_delta),
                    max: arrivals
                        .checked_add(arrival_delta)
                        .ok_or(DecisionStoreError::Invalid)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let plan = SensitivityPlan {
            runs,
            daily_arrival_bands,
            service_capacity_band: BoundedCount {
                min: capacity_min,
                max: capacity_max,
            },
            max_final_backlog,
            max_staff_cost_cents,
        };
        let report = simulate_sensitivity(&snapshot, &model, &baseline, &alternative, &plan)
            .map_err(|_| DecisionStoreError::Invalid)?;
        self.verify_simulation_inputs_still_current(
            scope,
            &snapshot,
            &model,
            &[&baseline, &alternative],
        )?;
        Ok(report)
    }

    /// Enumerate bounded choices, revalidating each input and its active causal
    /// source before it can be selected for a dashboard comparison.
    pub fn dashboard_catalog(
        &self,
        scope: &DecisionScope,
    ) -> Result<DecisionCatalog, DecisionStoreError> {
        if !scope.valid() {
            return Err(DecisionStoreError::Invalid);
        }
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT kind,input_id FROM (
               SELECT kind,input_id,ROW_NUMBER() OVER (
                 PARTITION BY kind ORDER BY created_at DESC,input_id
               ) AS row_number
               FROM decision_inputs
               WHERE tenant_id=?1 AND acl=?2 AND invalidated_at IS NULL
                 AND kind IN ('snapshot','model','scenario')
                 AND NOT EXISTS (
                   SELECT 1 FROM decision_operator_pilot_imports p
                   WHERE p.tenant_id=decision_inputs.tenant_id
                     AND p.acl=decision_inputs.acl
                     AND kind='snapshot' AND input_id=p.snapshot_id
                     AND (p.completed_at IS NULL OR NOT EXISTS (
                       SELECT 1 FROM decision_inputs r
                       WHERE r.tenant_id=p.tenant_id AND r.acl=p.acl
                         AND r.kind='uploaded_pilot_receipt' AND r.input_id=p.snapshot_id
                         AND r.invalidated_at IS NULL
                     ))
                 )
             ) WHERE row_number<=200 ORDER BY kind,input_id",
        )?;
        let rows = statement
            .query_map(params![scope.tenant_id, scope.acl], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(conn);

        let mut catalog = DecisionCatalog {
            snapshots: Vec::new(),
            models: Vec::new(),
            scenarios: Vec::new(),
            synthetic_pilots: Vec::new(),
            uploaded_pilots: Vec::new(),
        };
        for (kind, id) in rows {
            match kind.as_str() {
                "snapshot" => {
                    let snapshot: DecisionSnapshot = match self.get(scope, &kind, &id) {
                        Ok(snapshot) => snapshot,
                        Err(DecisionStoreError::Revoked | DecisionStoreError::NotFound) => continue,
                        Err(error) => return Err(error),
                    };
                    if snapshot.id != id {
                        return Err(DecisionStoreError::Corrupt);
                    }
                    let mut source_kinds = Vec::new();
                    if let Some(causal) = self.causal_store() {
                        let evidence_scope = EvidenceScope {
                            tenant_id: scope.tenant_id.clone(),
                            acl: scope.acl.clone(),
                        };
                        for link in self.active_source_links(scope, &snapshot)? {
                            source_kinds.push(
                                causal
                                    .read_artifact_metadata(&evidence_scope, &link.artifact_id)?
                                    .kind,
                            );
                        }
                    }
                    source_kinds.sort();
                    source_kinds.dedup();
                    catalog.snapshots.push(DecisionCatalogSnapshot {
                        id,
                        queue_id: snapshot.queue_id,
                        data_cutoff_utc: snapshot.data_cutoff_utc,
                        horizon_days: snapshot.arrivals_by_day.len(),
                        source_kinds,
                    });
                }
                "model" => {
                    let model: QueueModel = match self.get(scope, &kind, &id) {
                        Ok(model) => model,
                        Err(DecisionStoreError::Revoked) => continue,
                        Err(error) => return Err(error),
                    };
                    if model.version != id {
                        return Err(DecisionStoreError::Corrupt);
                    }
                    catalog.models.push(DecisionCatalogModel {
                        id,
                        service_capacity_per_agent_day: model.service_capacity_per_agent_day,
                        sla_days: model.sla_days,
                    });
                }
                "scenario" => {
                    let scenario: StaffingScenario = match self.get(scope, &kind, &id) {
                        Ok(scenario) => scenario,
                        Err(DecisionStoreError::Revoked) => continue,
                        Err(error) => return Err(error),
                    };
                    if scenario.id != id {
                        return Err(DecisionStoreError::Corrupt);
                    }
                    catalog.scenarios.push(DecisionCatalogScenario {
                        id,
                        horizon_days: scenario.agents_by_day.len(),
                    });
                }
                _ => return Err(DecisionStoreError::Corrupt),
            }
        }
        // Only advertise pairs whose canonical IDs are all present in the
        // active, revalidated catalog. Scenario list order is not a pairing
        // signal: multiple pilots can share a horizon.
        for snapshot in &catalog.snapshots {
            if !snapshot
                .source_kinds
                .iter()
                .any(|kind| kind == "synthetic_support_export")
            {
                continue;
            }
            let Some(suffix) = snapshot.id.strip_prefix("synthetic-support-") else {
                continue;
            };
            let Some((seed, days)) = suffix.split_once('-') else {
                continue;
            };
            if seed
                .parse::<u64>()
                .ok()
                .is_none_or(|value| value.to_string() != seed)
                || days
                    .parse::<usize>()
                    .ok()
                    .is_none_or(|value| value.to_string() != days)
                || days.parse::<usize>().ok() != Some(snapshot.horizon_days)
            {
                continue;
            }
            let model_version = format!("dashboard-synthetic-capacity-v1-{seed}-{days}");
            let baseline_scenario_id =
                format!("dashboard-synthetic-baseline-two-agents-{seed}-{days}");
            let alternative_scenario_id = format!("dashboard-synthetic-three-agents-{seed}-{days}");
            let has_model = catalog.models.iter().any(|model| model.id == model_version);
            let baseline_active = catalog.scenarios.iter().any(|scenario| {
                scenario.id == baseline_scenario_id
                    && scenario.horizon_days == snapshot.horizon_days
            });
            let alternative_active = catalog.scenarios.iter().any(|scenario| {
                scenario.id == alternative_scenario_id
                    && scenario.horizon_days == snapshot.horizon_days
            });
            if has_model && baseline_active && alternative_active {
                catalog
                    .synthetic_pilots
                    .push(DecisionCatalogSyntheticPilot {
                        snapshot_id: snapshot.id.clone(),
                        model_version,
                        baseline_scenario_id,
                        alternative_scenario_id,
                    });
            }
        }
        catalog
            .synthetic_pilots
            .sort_by(|left, right| left.snapshot_id.cmp(&right.snapshot_id));
        catalog.uploaded_pilots = self
            .completed_operator_pilots(scope)?
            .into_iter()
            .filter(|pilot| {
                catalog
                    .snapshots
                    .iter()
                    .any(|snapshot| snapshot.id == pilot.snapshot_id)
                    && catalog
                        .models
                        .iter()
                        .any(|model| model.id == pilot.model_version)
                    && catalog
                        .scenarios
                        .iter()
                        .any(|scenario| scenario.id == pilot.baseline_scenario_id)
                    && catalog
                        .scenarios
                        .iter()
                        .any(|scenario| scenario.id == pilot.alternative_scenario_id)
            })
            .collect();
        Ok(catalog)
    }

    /// Create an explicitly synthetic fixture in the same scoped stores used
    /// by the dashboard. Stable IDs and immutable writes make a retry safe.
    pub fn create_dashboard_synthetic_pilot(
        &self,
        scope: &DecisionScope,
        seed: u64,
        days: usize,
    ) -> Result<DecisionSyntheticPilotReceipt, DecisionStoreError> {
        if !scope.valid() || !(21..=90).contains(&days) {
            return Err(DecisionStoreError::Invalid);
        }
        let causal = self
            .causal_store()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        let mut export =
            synthetic_support_export(seed, days).map_err(|_| DecisionStoreError::Invalid)?;
        // Every immutable fixture needs a distinct scenario identity. The
        // generator's baseline ID is intentionally shared across its exports.
        export.baseline_scenario_id =
            format!("dashboard-synthetic-baseline-two-agents-{seed}-{days}");
        let pilot = build_support_pilot(&export).map_err(|_| DecisionStoreError::Invalid)?;
        let fit = fit_capacity(&pilot.observed_days, 7)?;
        let model = QueueModel {
            version: format!("dashboard-synthetic-capacity-v1-{seed}-{days}"),
            service_capacity_per_agent_day: fit.service_per_agent_day,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 10_000,
        };
        let alternative = StaffingScenario {
            id: format!("dashboard-synthetic-three-agents-{seed}-{days}"),
            agents_by_day: vec![3; days],
            fixed_extra_capacity_by_day: vec![0; days],
        };
        let source_text = serde_json::to_string(&(&export.tickets, &export.staffing))?;
        let source_digest = format!("{:x}", Sha256::digest(source_text.as_bytes()));
        if export.source_version_hashes != [source_digest.clone()] {
            return Err(DecisionStoreError::Corrupt);
        }
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let artifact = causal.add_artifact(
            &evidence_scope,
            "synthetic_support_export",
            &format!("dashboard-support-{seed}-{days}"),
            &source_digest,
            "dashboard_synthetic_support_pilot",
            &source_text,
            0,
            i64::MAX,
        )?;
        self.put_snapshot(scope, &pilot.snapshot)?;
        self.bind_causal_artifact(scope, &pilot.snapshot.id, &artifact.id)?;
        self.put_model(scope, &model)?;
        self.put_scenario(scope, &pilot.baseline)?;
        self.put_scenario(scope, &alternative)?;
        Ok(DecisionSyntheticPilotReceipt {
            snapshot_id: pilot.snapshot.id,
            model_version: model.version,
            baseline_scenario_id: pilot.baseline.id,
            alternative_scenario_id: alternative.id,
            source_artifact_id: artifact.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_operator_import::OperatorPilotImportRequest;
    use crate::decision_policy::StaffingResourcePlan;
    use crate::decision_store::{ShadowDayAssessment, ShadowSlaDayAssessment};
    use duduclaw_memory::causal::CausalStore;

    #[test]
    fn synthetic_event_comparison_is_replayable_scoped_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "synthetic-event".into(),
            acl: "private".into(),
        };
        let pilot = store
            .create_dashboard_synthetic_pilot(&scope, 47, 35)
            .unwrap();
        let compare = || {
            store.dashboard_event_comparison(
                &scope,
                &pilot.snapshot_id,
                &pilot.model_version,
                &pilot.baseline_scenario_id,
                &pilot.alternative_scenario_id,
            )
        };
        let first = compare().unwrap();
        assert_eq!(first.status, "synthetic_only_exploratory");
        assert_eq!(first.source_origin, "synthetic");
        assert_eq!(first.source_sha256.len(), 64);
        assert_eq!(first.event_engine_sha256.len(), 64);
        assert_ne!(first.baseline.replay_hash, first.alternative.replay_hash);
        assert_eq!(first, compare().unwrap());
        let response = serde_json::to_string(&first).unwrap();
        assert!(!response.contains("ticket_id"));
        assert!(!response.contains("\"tickets\""));
        let other = DecisionScope {
            tenant_id: "other-tenant".into(),
            acl: scope.acl.clone(),
        };
        assert!(
            store
                .dashboard_event_comparison(
                    &other,
                    &pilot.snapshot_id,
                    &pilot.model_version,
                    &pilot.baseline_scenario_id,
                    &pilot.alternative_scenario_id,
                )
                .is_err()
        );
        causal
            .invalidate_artifact(
                &EvidenceScope {
                    tenant_id: scope.tenant_id.clone(),
                    acl: scope.acl.clone(),
                },
                &pilot.source_artifact_id,
            )
            .unwrap();
        assert!(compare().is_err());
    }

    #[test]
    fn uploaded_pilot_engineering_and_empirical_are_source_bound() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "operator-tenant".into(),
            acl: "private".into(),
        };
        let mut export = synthetic_support_export(57, 35).unwrap();
        export.snapshot_id = "operator-upload-57-35".into();
        export.baseline_scenario_id = "operator-baseline-57-35".into();
        export.tickets[0].ticket_id = "operator-private-ticket".into();
        export.source_version_hashes = vec![format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&export.tickets, &export.staffing)).unwrap())
        )];
        let request = OperatorPilotImportRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            expected_queue_id: "synthetic-support-queue".into(),
            source_lineage: "operator-private-lineage".into(),
            retention_until_utc: (chrono::Utc::now() + chrono::Duration::days(30))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            export,
            model: QueueModel {
                version: "operator-model-57-35".into(),
                service_capacity_per_agent_day: 8,
                sla_days: 2,
                staff_cost_cents_per_agent_day: 10_000,
            },
            alternative_scenario: StaffingScenario {
                id: "operator-alternative-57-35".into(),
                agents_by_day: vec![3; 35],
                fixed_extra_capacity_by_day: vec![0; 35],
            },
        };
        let receipt = store.import_operator_pilot(&request).unwrap();
        let engineering = store
            .dashboard_synthetic_engineering_validation(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
            )
            .unwrap();
        assert_eq!(engineering.status, "exploratory_operator_upload");
        assert_eq!(engineering.source_artifact_id, receipt.source_artifact_id);
        assert_eq!(engineering.source_version_sha256, receipt.source_sha256);
        assert_eq!(
            engineering.provided_model_matches_initial_prefix_fit,
            Some(true)
        );
        assert!(engineering.ticket_sla_holdout.is_some());
        let engineering_json = serde_json::to_string(&engineering).unwrap();
        assert!(!engineering_json.contains("operator-private-ticket"));
        assert!(!engineering_json.contains("operator-private-lineage"));
        let event = store
            .dashboard_event_comparison(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
            )
            .unwrap();
        assert_eq!(event.status, "operator_supplied_exploratory");
        assert_eq!(event.source_origin, "uploaded");
        assert_eq!(event.source_sha256, receipt.source_sha256);
        assert_eq!(event.baseline.replay_hash.len(), 64);
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("operator-private-ticket")
        );
        let plan = EmpiricalSensitivityPlan {
            runs: 8,
            arrival_block_days: 2,
            sampling_mode: EmpiricalSamplingMode::PairedSaturatedDays,
            capacity_fallback_range: None,
            max_final_backlog: 500,
            max_staff_cost_cents: 2_000_000,
            min_sla_resolved: Some(0),
        };
        let resource = StaffingResourcePlan {
            available_agents_by_day: vec![3; 35],
            max_added_agents_per_day: 1,
            max_total_agent_days: 105,
            max_staff_cost_cents: plan.max_staff_cost_cents,
            max_final_backlog: plan.max_final_backlog,
            service_capacity_band: BoundedCount { min: 7, max: 9 },
        };
        let criteria = JointRiskScreenCriteria {
            max_joint_violation_bps: 10_000,
            min_joint_recovery_bps: 0,
            min_sla_improvement_bps: 0,
        };
        let run = || {
            store.dashboard_empirical_resampling(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                14,
                7,
                &plan,
                Some((&resource, &criteria)),
            )
        };
        let first = run().unwrap();
        assert_eq!(first, run().unwrap());
        assert_eq!(first.status, "exploratory_operator_upload");
        let source_bytes =
            serde_json::to_vec(&(&request.export.tickets, &request.export.staffing)).unwrap();
        let brief = store
            .compare_scenarios_with_evidence(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                crate::decision_brief::BriefEvidenceSelection {
                    empirical_run_id: Some(&first.run.id),
                    policy_screen_hash: None,
                    event_runs: Some((
                        &event.baseline.replay_hash,
                        &event.alternative.replay_hash,
                        &source_bytes,
                    )),
                    forecast_validation: None,
                    sla_holdout: None,
                    effect_ids: &[],
                },
                Vec::new(),
            )
            .unwrap();
        let event_evidence = brief.exploratory_event.as_ref().unwrap();
        let fence = || {
            store.verify_dashboard_event_evidence_still_current(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                &event,
                &source_bytes,
                event_evidence,
            )
        };
        fence().unwrap();
        let conn = store.open().unwrap();
        conn.execute(
            "UPDATE decision_inputs SET payload_sha256='0'
             WHERE tenant_id=?1 AND acl=?2 AND kind='event_run' AND input_id=?3",
            rusqlite::params![scope.tenant_id, scope.acl, event.baseline.replay_hash],
        )
        .unwrap();
        assert!(fence().is_err());
        conn.execute(
            "UPDATE decision_inputs SET payload_sha256=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='event_run' AND input_id=?4",
            rusqlite::params![
                event_evidence.baseline_run_sha256,
                scope.tenant_id,
                scope.acl,
                event.baseline.replay_hash
            ],
        )
        .unwrap();
        fence().unwrap();
        assert!(
            first
                .fit
                .id
                .starts_with("dashboard-operator-empirical-fit-")
        );
        assert!(
            first
                .run
                .id
                .starts_with("dashboard-operator-empirical-run-")
        );
        assert!(first.screen.is_some());
        let evidence_brief = store
            .compare_scenarios_with_empirical_run(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                &first.run.id,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            evidence_brief
                .exploratory_empirical
                .as_ref()
                .unwrap()
                .source_origin,
            crate::decision_brief::BriefEmpiricalOrigin::OperatorUpload
        );
        assert!(
            evidence_brief
                .limitations
                .iter()
                .all(|text| !text.contains("finite synthetic resamples"))
        );
        assert!(matches!(
            store.dashboard_empirical_resampling(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.alternative_scenario_id,
                &receipt.baseline_scenario_id,
                14,
                7,
                &plan,
                None,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let other_scope = DecisionScope {
            tenant_id: "other-tenant".into(),
            acl: "private".into(),
        };
        assert!(
            store
                .dashboard_synthetic_engineering_validation(
                    &other_scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                )
                .is_err()
        );
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        causal
            .invalidate_artifact(&evidence_scope, &receipt.source_artifact_id)
            .unwrap();
        assert!(fence().is_err());
        assert!(
            store
                .dashboard_synthetic_engineering_validation(
                    &scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                )
                .is_err()
        );
        assert!(run().is_err());
        assert!(store.load_empirical_run(&scope, &first.run.id).is_err());
        assert!(
            store
                .dashboard_event_comparison(
                    &scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                    &receipt.baseline_scenario_id,
                    &receipt.alternative_scenario_id,
                )
                .is_err()
        );
    }

    #[test]
    fn uploaded_unsaturated_pilot_reports_unidentified_capacity_but_allows_declared_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::with_causal_store(
            dir.path().join("decisions.db"),
            CausalStore::new(dir.path().join("memory.db")),
        );
        let scope = DecisionScope {
            tenant_id: "operator-tenant".into(),
            acl: "private".into(),
        };
        let mut export = synthetic_support_export(58, 21).unwrap();
        export.snapshot_id = "operator-unsaturated-58-21".into();
        export.baseline_scenario_id = "operator-unsaturated-baseline".into();
        for ticket in &mut export.tickets {
            let resolution_day = if ticket.created_at_utc.starts_with("2025-12-31") {
                "2026-01-01"
            } else {
                ticket.created_at_utc.get(..10).unwrap_or("2026-01-01")
            };
            ticket.resolved_at_utc = Some(format!("{resolution_day}T20:00:00Z"));
        }
        for staff in &mut export.staffing {
            staff.agents = 10;
        }
        export.source_version_hashes = vec![format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&export.tickets, &export.staffing)).unwrap())
        )];
        let request = OperatorPilotImportRequest {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            expected_queue_id: "synthetic-support-queue".into(),
            source_lineage: "operator-unsaturated-source".into(),
            retention_until_utc: (chrono::Utc::now() + chrono::Duration::days(30))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            export,
            model: QueueModel {
                version: "operator-unsaturated-model".into(),
                service_capacity_per_agent_day: 8,
                sla_days: 2,
                staff_cost_cents_per_agent_day: 10_000,
            },
            alternative_scenario: StaffingScenario {
                id: "operator-unsaturated-alternative".into(),
                agents_by_day: vec![9; 21],
                fixed_extra_capacity_by_day: vec![0; 21],
            },
        };
        let receipt = store.import_operator_pilot(&request).unwrap();
        let report = store
            .dashboard_synthetic_engineering_validation(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
            )
            .unwrap();
        assert_eq!(report.status, "exploratory_operator_upload");
        assert!(report.initial_prefix_fit.is_none());
        assert!(report.initial_prefix_unavailable_reason.is_some());
        assert!(report.provided_model_matches_initial_prefix_fit.is_none());
        assert!(report.one_day_backtest.is_none());
        assert!(report.one_day_backtest_unavailable_reason.is_some());
        assert!(report.ticket_sla_holdout.is_none());
        let mut plan = EmpiricalSensitivityPlan {
            runs: 8,
            arrival_block_days: 2,
            sampling_mode: EmpiricalSamplingMode::Independent,
            capacity_fallback_range: None,
            max_final_backlog: 500,
            max_staff_cost_cents: 4_000_000,
            min_sla_resolved: Some(0),
        };
        let run = |plan: &EmpiricalSensitivityPlan| {
            store.dashboard_empirical_resampling(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                14,
                7,
                plan,
                None,
            )
        };
        assert!(matches!(run(&plan), Err(DecisionStoreError::Invalid)));
        plan.capacity_fallback_range = Some(BoundedCount { min: 1, max: 9 });
        let result = run(&plan).unwrap();
        assert_eq!(result.status, "exploratory_operator_upload");
        assert_eq!(
            result.fit.capacity_identification,
            CapacityIdentification::Unidentified
        );
        assert_eq!(result.run.capacity_source, "operator_range_uniform");
    }

    #[test]
    fn shadow_monitor_rejects_mixed_aggregate_generations() {
        let aggregate = vec![ShadowDayAssessment {
            target_day_utc: "2026-01-01T00:00:00Z".into(),
            status: ShadowDayStatus::Scored,
            forecast_id: None,
            score_revision_id: None,
            score_revision_sha256: None,
            corrected: false,
        }];
        let mut sla = vec![ShadowSlaDayAssessment {
            target_day_utc: "2026-01-01T00:00:00Z".into(),
            aggregate_status: ShadowDayStatus::Scored,
            status: ShadowSlaDayStatus::Scored,
            aggregate_forecast_id: None,
            sla_forecast_id: None,
            score_revision_id: None,
            score_revision_sha256: None,
            corrected: false,
            predicted_resolved_within_sla: None,
            observed_resolved_within_sla: None,
            abs_error: None,
            no_change_abs_error: None,
            seasonal_naive_abs_error: None,
            seven_day_mean_abs_error: None,
        }];
        assert!(shadow_days_match(&aggregate, &sla));
        sla[0].aggregate_status = ShadowDayStatus::ForecastRevoked;
        assert!(!shadow_days_match(&aggregate, &sla));
        sla[0].aggregate_status = ShadowDayStatus::Scored;
        sla[0].target_day_utc = "2026-01-02T00:00:00Z".into();
        assert!(!shadow_days_match(&aggregate, &sla));
    }

    #[test]
    fn shadow_monitor_future_policy_is_empty_scoped_and_content_free() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store = DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal);
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let start = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            + chrono::Duration::days(2);
        let end = start + chrono::Duration::days(21);
        let policy = store
            .put_shadow_policy(
                &scope,
                "future-policy",
                "secret-source-lineage",
                "support-queue",
                &start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                &end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                3_600,
                14,
                7,
            )
            .unwrap();
        let report = store.dashboard_shadow_monitor(&scope, &policy.id).unwrap();
        assert_eq!(report.status, "descriptive_shadow_monitor");
        assert_eq!(report.policy.id, policy.id);
        assert_eq!(report.policy.queue_id.as_deref(), Some("support-queue"));
        for assessment in [&report.aggregate, &report.sla] {
            assert_eq!(assessment.due_days, 0);
            assert_eq!(assessment.scored_days, 0);
            assert_eq!(assessment.corrected_days, 0);
            assert!(!assessment.complete);
            assert_eq!(assessment.whole_window_skill, None);
            assert_eq!(assessment.recent_skill, None);
            assert!(!assessment.fixed_interval_available);
            assert_eq!(assessment.drift_signal, None);
            assert!(assessment.status_counts.values().all(|count| *count == 0));
        }
        let cutoff = start + chrono::Duration::days(7);
        let cutoff_utc = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let end_utc = end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let handoff = store
            .supersede_shadow_policy(
                &scope,
                &policy.id,
                "future-policy-successor",
                &cutoff_utc,
                &end_utc,
                "admin-reviewer",
                3_600,
                14,
                7,
            )
            .unwrap();
        assert_eq!(
            store
                .supersede_shadow_policy(
                    &scope,
                    &policy.id,
                    "future-policy-successor",
                    &cutoff_utc,
                    &end_utc,
                    "admin-reviewer",
                    3_600,
                    14,
                    7,
                )
                .unwrap(),
            handoff
        );
        let shortened = store.dashboard_shadow_monitor(&scope, &policy.id).unwrap();
        assert_eq!(shortened.policy.effective_until_utc, cutoff_utc);
        assert_eq!(shortened.aggregate.due_days, 0);
        assert_eq!(shortened.sla.due_days, 0);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("secret-source-lineage"));
        assert!(!json.contains("artifact_id"));
        assert!(!json.contains("forecast_id"));
        assert!(!json.contains("score_revision_id"));
        assert!(matches!(
            store.dashboard_shadow_monitor(
                &DecisionScope {
                    tenant_id: "tenant-b".into(),
                    acl: "private".into(),
                },
                &policy.id,
            ),
            Err(DecisionStoreError::NotFound)
        ));
        assert!(matches!(
            store.dashboard_shadow_monitor(
                &DecisionScope {
                    tenant_id: "tenant-a".into(),
                    acl: "staff".into(),
                },
                &policy.id,
            ),
            Err(DecisionStoreError::NotFound)
        ));
    }

    #[test]
    fn empirical_resampling_is_canonical_repeatable_and_source_bound() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let receipt = store
            .create_dashboard_synthetic_pilot(&scope, 47, 21)
            .unwrap();
        let plan = EmpiricalSensitivityPlan {
            runs: 16,
            arrival_block_days: 2,
            sampling_mode: EmpiricalSamplingMode::PairedSaturatedDays,
            capacity_fallback_range: None,
            max_final_backlog: 300,
            max_staff_cost_cents: 1_000_000,
            min_sla_resolved: Some(0),
        };
        let resource = StaffingResourcePlan {
            available_agents_by_day: vec![3; 21],
            max_added_agents_per_day: 1,
            max_total_agent_days: 63,
            max_staff_cost_cents: plan.max_staff_cost_cents,
            max_final_backlog: plan.max_final_backlog,
            service_capacity_band: BoundedCount { min: 7, max: 9 },
        };
        let criteria = JointRiskScreenCriteria {
            max_joint_violation_bps: 10_000,
            min_joint_recovery_bps: 0,
            min_sla_improvement_bps: 0,
        };
        let run = |scope: &DecisionScope, plan: &EmpiricalSensitivityPlan| {
            store.dashboard_empirical_resampling(
                scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                14,
                7,
                plan,
                Some((&resource, &criteria)),
            )
        };
        let first = run(&scope, &plan).unwrap();
        let repeated = run(&scope, &plan).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(first.status, "synthetic_only_exploratory");
        assert_eq!(
            first.fit.capacity_identification,
            CapacityIdentification::EmpiricalSaturatedDays
        );
        assert_eq!(first.fit.arrival_sample_count, 14);
        assert_eq!(first.run.runs, 16);
        assert_eq!(first.run.arrival_block_days, 2);
        assert!(first.screen.is_some());
        assert_eq!(
            store
                .load_policy_screen(&scope, &first.screen.as_ref().unwrap().replay_hash)
                .unwrap()
                .report
                .replay_hash,
            first.screen.as_ref().unwrap().replay_hash
        );
        let screen_hash = first.screen.as_ref().unwrap().replay_hash.as_str();
        let brief = store
            .compare_scenarios_with_evidence(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                crate::decision_brief::BriefEvidenceSelection {
                    empirical_run_id: Some(&first.run.id),
                    policy_screen_hash: Some(screen_hash),
                    event_runs: None,
                    forecast_validation: None,
                    sla_holdout: None,
                    effect_ids: &[],
                },
                vec![],
            )
            .unwrap();
        assert_eq!(
            brief.exploratory_empirical.as_ref().unwrap().run_id,
            first.run.id
        );
        assert_eq!(
            brief.exploratory_empirical.as_ref().unwrap().source_origin,
            crate::decision_brief::BriefEmpiricalOrigin::SyntheticFixture
        );
        assert_eq!(
            brief
                .exploratory_policy_screen
                .as_ref()
                .unwrap()
                .screen_hash,
            screen_hash
        );
        let public_json = serde_json::to_string(&first).unwrap();
        for hidden in [
            "source_artifact_id",
            "source_version_sha256",
            "tickets",
            "staffing",
            "draw_paths",
        ] {
            assert!(!public_json.contains(&format!("\"{hidden}\"")));
        }
        assert_eq!(
            store
                .load_empirical_run(&scope, &first.run.id)
                .unwrap()
                .report
                .replay_hash,
            first.run.replay_hash
        );
        let mut wrong_fallback = plan.clone();
        wrong_fallback.capacity_fallback_range = Some(BoundedCount { min: 7, max: 9 });
        assert!(matches!(
            run(&scope, &wrong_fallback),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            store.dashboard_empirical_resampling(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.alternative_scenario_id,
                &receipt.baseline_scenario_id,
                14,
                7,
                &plan,
                None,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(
            run(
                &DecisionScope {
                    tenant_id: "tenant-b".into(),
                    acl: "private".into()
                },
                &plan
            )
            .is_err()
        );
        assert!(
            run(
                &DecisionScope {
                    tenant_id: "tenant-a".into(),
                    acl: "staff".into()
                },
                &plan
            )
            .is_err()
        );
        causal
            .erase_artifact(
                &EvidenceScope {
                    tenant_id: scope.tenant_id.clone(),
                    acl: scope.acl.clone(),
                },
                &receipt.source_artifact_id,
            )
            .unwrap();
        assert!(run(&scope, &plan).is_err());
        assert!(store.load_empirical_run(&scope, &first.run.id).is_err());
        assert!(store.load_policy_screen(&scope, screen_hash).is_err());
    }

    #[test]
    fn synthetic_pilot_catalog_compare_and_source_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let receipt = store
            .create_dashboard_synthetic_pilot(&scope, 47, 35)
            .unwrap();
        assert_eq!(
            store
                .create_dashboard_synthetic_pilot(&scope, 47, 35)
                .unwrap(),
            receipt
        );
        let second = store
            .create_dashboard_synthetic_pilot(&scope, 48, 35)
            .unwrap();
        assert_ne!(second.baseline_scenario_id, receipt.baseline_scenario_id);
        assert_ne!(
            second.alternative_scenario_id,
            receipt.alternative_scenario_id
        );
        assert_eq!(
            store
                .create_dashboard_synthetic_pilot(&scope, 48, 35)
                .unwrap(),
            second
        );
        let shorter = store
            .create_dashboard_synthetic_pilot(&scope, 47, 21)
            .unwrap();
        assert_ne!(shorter.baseline_scenario_id, receipt.baseline_scenario_id);
        assert_ne!(
            shorter.alternative_scenario_id,
            receipt.alternative_scenario_id
        );
        assert_eq!(
            store
                .create_dashboard_synthetic_pilot(&scope, 47, 21)
                .unwrap(),
            shorter
        );
        let catalog = store.dashboard_catalog(&scope).unwrap();
        assert_eq!(catalog.snapshots.len(), 3);
        assert_eq!(catalog.models.len(), 3);
        assert_eq!(catalog.scenarios.len(), 6);
        assert_eq!(catalog.synthetic_pilots.len(), 3);
        assert!(
            catalog
                .synthetic_pilots
                .contains(&DecisionCatalogSyntheticPilot {
                    snapshot_id: receipt.snapshot_id.clone(),
                    model_version: receipt.model_version.clone(),
                    baseline_scenario_id: receipt.baseline_scenario_id.clone(),
                    alternative_scenario_id: receipt.alternative_scenario_id.clone(),
                })
        );
        assert!(
            catalog
                .synthetic_pilots
                .contains(&DecisionCatalogSyntheticPilot {
                    snapshot_id: second.snapshot_id.clone(),
                    model_version: second.model_version.clone(),
                    baseline_scenario_id: second.baseline_scenario_id.clone(),
                    alternative_scenario_id: second.alternative_scenario_id.clone(),
                })
        );
        assert!(
            catalog
                .snapshots
                .iter()
                .any(|item| item.id == receipt.snapshot_id)
        );
        assert!(
            catalog
                .snapshots
                .iter()
                .find(|item| item.id == receipt.snapshot_id)
                .unwrap()
                .source_kinds
                .contains(&"synthetic_support_export".to_string())
        );
        assert!(
            catalog
                .models
                .iter()
                .any(|item| item.id == receipt.model_version)
        );
        assert!(
            catalog
                .scenarios
                .iter()
                .any(|item| item.id == receipt.alternative_scenario_id)
        );
        let brief = store
            .compare_scenarios(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(brief.status, "exploratory");
        assert!(brief.delta.final_backlog.parse::<i128>().unwrap() < 0);
        assert_eq!(brief.source_artifact_links.len(), 1);
        assert_eq!(
            store
                .replay(
                    &scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                    &receipt.baseline_scenario_id,
                )
                .unwrap()
                .replay_hash,
            brief.replay.baseline_replay_hash
        );
        let sweep = store
            .sweep_staffing_policy(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                &StaffingResourcePlan {
                    available_agents_by_day: vec![3; 35],
                    max_added_agents_per_day: 1,
                    max_total_agent_days: 105,
                    max_staff_cost_cents: 1_100_000,
                    max_final_backlog: 300,
                    service_capacity_band: BoundedCount { min: 7, max: 9 },
                },
            )
            .unwrap();
        assert_eq!(sweep.status, "exploratory");
        assert_eq!(sweep.points.len(), 3);
        let sensitivity = store
            .dashboard_uniform_sensitivity(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                32,
                2,
                7,
                9,
                300,
                1_100_000,
            )
            .unwrap();
        assert_eq!(sensitivity.status, "exploratory");
        assert_eq!(sensitivity.runs, 32);
        assert!(sensitivity.delta_backlog_p05 <= sensitivity.delta_backlog_p95);
        let other = DecisionScope {
            tenant_id: "tenant-b".into(),
            acl: "private".into(),
        };
        assert!(
            store
                .dashboard_catalog(&other)
                .unwrap()
                .snapshots
                .is_empty()
        );
        let engineering = store
            .dashboard_synthetic_engineering_validation(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
            )
            .unwrap();
        assert_eq!(engineering.status, "synthetic_only_exploratory");
        assert_eq!(
            engineering
                .one_day_backtest
                .as_ref()
                .unwrap()
                .evaluation_days,
            21
        );
        assert_eq!(
            engineering.one_day_backtest.as_ref().unwrap().points.len(),
            21
        );
        assert!(engineering.fixed_interval_diagnostic.is_some());
        assert!(engineering.ticket_sla_holdout.is_some());
        assert_eq!(engineering.source_artifact_id, receipt.source_artifact_id);
        assert_eq!(engineering.source_version_sha256.len(), 64);
        assert_eq!(
            engineering.simulation_engine_sha256,
            crate::decision_sim::engine_code_sha256()
        );
        assert_eq!(engineering.engineering_engine_sha256.len(), 64);
        let repeated_engineering = store
            .dashboard_synthetic_engineering_validation(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
            )
            .unwrap();
        assert_eq!(engineering.replay_hash, repeated_engineering.replay_hash);
        let short_validation = store
            .dashboard_synthetic_engineering_validation(
                &scope,
                &shorter.snapshot_id,
                &shorter.model_version,
            )
            .unwrap();
        assert_eq!(
            short_validation
                .one_day_backtest
                .as_ref()
                .unwrap()
                .evaluation_days,
            7
        );
        assert!(short_validation.fixed_interval_diagnostic.is_none());
        assert!(short_validation.fixed_interval_unavailable_reason.is_some());
        let loaded_snapshot: DecisionSnapshot =
            store.get(&scope, "snapshot", &receipt.snapshot_id).unwrap();
        let loaded_model: QueueModel = store.get(&scope, "model", &receipt.model_version).unwrap();
        let loaded_baseline: StaffingScenario = store
            .get(&scope, "scenario", &receipt.baseline_scenario_id)
            .unwrap();
        let loaded_alternative: StaffingScenario = store
            .get(&scope, "scenario", &receipt.alternative_scenario_id)
            .unwrap();
        causal
            .erase_artifact(
                &EvidenceScope {
                    tenant_id: scope.tenant_id.clone(),
                    acl: scope.acl.clone(),
                },
                &receipt.source_artifact_id,
            )
            .unwrap();
        assert!(matches!(
            store.verify_simulation_inputs_still_current(
                &scope,
                &loaded_snapshot,
                &loaded_model,
                &[&loaded_baseline, &loaded_alternative],
            ),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.replay(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
            ),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.dashboard_synthetic_engineering_validation(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
            ),
            Err(DecisionStoreError::Revoked)
        ));
    }
}
