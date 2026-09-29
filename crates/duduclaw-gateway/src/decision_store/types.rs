use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct StoredEmpiricalParameterFit {
    pub id: String,
    pub snapshot_id: String,
    pub snapshot_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub engine_sha256: String,
    pub fit_engine_sha256: String,
    pub fit: EmpiricalParameterFit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct StoredEmpiricalRun {
    pub id: String,
    pub fit_id: String,
    pub fit_sha256: String,
    pub snapshot_id: String,
    pub snapshot_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub data_cutoff_utc: String,
    pub seed: u64,
    pub horizon_days: usize,
    pub initial_backlog_sha256: String,
    pub engine_sha256: String,
    pub fit_engine_sha256: String,
    pub model_version: String,
    pub model_sha256: String,
    pub baseline_id: String,
    pub baseline_sha256: String,
    pub alternative_id: String,
    pub alternative_sha256: String,
    pub plan: EmpiricalSensitivityPlan,
    pub report: EmpiricalSensitivityReport,
}

/// Forward forecast diagnostics bound to the exact historical support export.
/// This is an engineering validation record, not a calibrated live interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredForecastValidation {
    pub id: String,
    pub snapshot_id: String,
    pub snapshot_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub source_sha256: String,
    pub window_start_utc: String,
    pub baseline_scenario_id: String,
    pub baseline_scenario_sha256: String,
    pub calibration_engine_sha256: String,
    pub min_training_days: usize,
    pub min_saturated_days: usize,
    pub calibration_points: usize,
    pub forecast: ForecastBacktestResult,
    pub rolling_interval: Option<IntervalDiagnostic>,
    pub fixed_interval: Option<IntervalDiagnostic>,
}

/// Immutable, source-bound ticket-level SLA replay diagnostic. Later demand
/// and staffing are observed inputs, so this is not a prospective forecast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSlaHoldout {
    pub id: String,
    pub snapshot_id: String,
    pub snapshot_sha256: String,
    pub model_version: String,
    pub model_sha256: String,
    pub baseline_scenario_id: String,
    pub baseline_scenario_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub source_sha256: String,
    pub window_start_utc: String,
    pub training_days: usize,
    pub min_saturated_days: usize,
    pub engine_sha256: String,
    pub diagnostic: SlaHoldoutDiagnostic,
}

/// One-day prediction committed before that day's complete outcome can exist.
/// This is a demand/backlog nowcast, not an intervention effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowPilotPolicy {
    pub id: String,
    pub source_lineage: String,
    /// Predeclared queue target. Policies saved before this field remain readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub effective_from_utc: String,
    pub effective_until_utc: String,
    pub issue_deadline_seconds: u32,
    pub min_training_days: usize,
    pub min_saturated_days: usize,
    pub calibration_engine_sha256: String,
    pub registered_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowPolicySupersession {
    pub old_policy_id: String,
    pub new_policy_id: String,
    pub cutoff_utc: String,
    pub reviewer: String,
    pub reviewed_at: i64,
    pub old_policy_sha256: String,
    pub new_policy_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShadowForecast {
    pub id: String,
    pub policy_id: String,
    pub policy_sha256: String,
    pub training_artifact_id: String,
    pub training_sha256: String,
    pub source_lineage: String,
    /// Exact queue label from the training source. Old forecasts omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub training_window_start_utc: String,
    pub target_day_utc: String,
    pub committed_at: i64,
    pub min_saturated_days: usize,
    pub known: KnownDayInputs,
    pub calibration_engine_sha256: String,
    pub forecast: ProspectiveForecast,
}

/// Separate pre-outcome SLA forecast anchored to a frozen backlog forecast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShadowSlaForecast {
    pub id: String,
    pub forecast_id: String,
    pub forecast_sha256: String,
    pub opening_artifact_id: String,
    pub opening_sha256: String,
    pub model_version: String,
    pub model_sha256: String,
    pub queue_id: String,
    pub target_day_utc: String,
    pub committed_at: i64,
    pub inputs: KnownSlaDayInputs,
    pub prediction: ProspectiveSlaForecast,
    pub baselines: ShadowSlaBaselines,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSlaBaselines {
    pub no_change: u32,
    pub seasonal_naive: u32,
    pub seven_day_mean: u32,
}

/// Opening ticket identities committed with the age cohorts before outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowSlaOpeningTicket {
    pub ticket_id: String,
    pub created_at_utc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowSlaOpeningExport {
    pub inputs: KnownSlaDayInputs,
    pub opening_tickets: Vec<ShadowSlaOpeningTicket>,
    /// All resolved tickets in the seven complete UTC days before the target.
    pub prior_resolved_tickets: Vec<TicketEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowSlaObservationExport {
    pub queue_id: String,
    pub target_day_utc: String,
    pub observed_through_utc: String,
    pub tickets: Vec<TicketEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShadowSlaScore {
    pub id: String,
    pub sla_forecast_id: String,
    pub sla_forecast_sha256: String,
    pub aggregate_score_id: String,
    pub aggregate_score_sha256: String,
    pub observation_artifact_id: String,
    pub observation_sha256: String,
    pub scored_at: i64,
    pub predicted_resolved_within_sla: u64,
    pub observed_resolved_within_sla: u64,
    pub abs_error: u64,
    pub no_change_abs_error: u64,
    pub seasonal_naive_abs_error: u64,
    pub seven_day_mean_abs_error: u64,
}

/// Reviewer-attributed replacement of one SLA score; prior revisions remain auditable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShadowSlaScoreCorrection {
    pub id: String,
    pub sla_forecast_id: String,
    pub previous_revision_id: String,
    pub previous_revision_sha256: String,
    pub reviewer: String,
    pub reason: String,
    pub corrected_at: i64,
    pub corrected_score: StoredShadowSlaScore,
}

/// Later observed day and errors against the frozen forecast and baselines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShadowScore {
    pub id: String,
    pub forecast_id: String,
    pub forecast_sha256: String,
    pub observation_artifact_id: String,
    pub observation_sha256: String,
    pub scored_at: i64,
    pub observed: ObservedSupportDay,
    pub arrivals_abs_error: u64,
    pub backlog_abs_error: u64,
    pub no_change_abs_error: u64,
    pub seasonal_naive_abs_error: u64,
    pub mean_change_abs_error: u64,
}

/// Append-only, reviewer-attributed replacement of one shadow score.
/// The original score and every earlier correction remain in the audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShadowScoreCorrection {
    pub id: String,
    pub forecast_id: String,
    pub previous_revision_id: String,
    pub previous_revision_sha256: String,
    pub reviewer: String,
    pub reason: String,
    pub corrected_at: i64,
    pub corrected_score: StoredShadowScore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowDayStatus {
    MissingForecast,
    ForecastInvalid,
    ForecastRevoked,
    Unscored,
    ScoreInvalid,
    ScoreRevoked,
    Scored,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowDayAssessment {
    pub target_day_utc: String,
    pub status: ShadowDayStatus,
    pub forecast_id: Option<String>,
    pub score_revision_id: Option<String>,
    pub score_revision_sha256: Option<String>,
    pub corrected: bool,
}

/// Descriptive absolute-error numerators over every due day. Published only
/// when the policy has a valid forecast and current score for each due day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowErrorSums {
    pub arrivals: u128,
    pub backlog: u128,
    pub no_change_backlog: u128,
    pub seasonal_naive_backlog: u128,
    pub mean_change_backlog: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowPolicyAssessment {
    pub policy_id: String,
    pub policy_sha256: String,
    pub source_lineage: String,
    /// Predeclared policy queue, absent only for historical policies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub assessed_at_utc: String,
    pub due_days: usize,
    pub scored_days: usize,
    pub corrected_days: usize,
    pub complete: bool,
    pub error_sums: Option<ShadowErrorSums>,
    /// Descriptive strict comparison over every due day; absent on gaps.
    pub backlog_abs_error_below_each_baseline: Option<bool>,
    /// Fixed first-14-day residual radius on later days; requires 21 valid
    /// due days so a full recent seven-day drift window exists.
    pub fixed_prefix_interval: Option<IntervalDiagnostic>,
    /// Last seven fully scored days, kept separate from whole-window skill.
    #[serde(default)]
    pub recent_7_day_error_sums: Option<ShadowErrorSums>,
    #[serde(default)]
    pub recent_7_day_backlog_abs_error_below_each_baseline: Option<bool>,
    pub days: Vec<ShadowDayAssessment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowSlaDayStatus {
    AggregateUnavailable,
    MissingSlaForecast,
    SlaForecastInvalid,
    SlaForecastRevoked,
    Unscored,
    ScoreInvalid,
    ScoreRevoked,
    ScoreStale,
    CrossDayIdentityMismatch,
    Scored,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSlaDayAssessment {
    pub target_day_utc: String,
    pub aggregate_status: ShadowDayStatus,
    pub status: ShadowSlaDayStatus,
    pub aggregate_forecast_id: Option<String>,
    pub sla_forecast_id: Option<String>,
    pub score_revision_id: Option<String>,
    pub score_revision_sha256: Option<String>,
    pub corrected: bool,
    pub predicted_resolved_within_sla: Option<u64>,
    pub observed_resolved_within_sla: Option<u64>,
    pub abs_error: Option<u64>,
    pub no_change_abs_error: Option<u64>,
    pub seasonal_naive_abs_error: Option<u64>,
    pub seven_day_mean_abs_error: Option<u64>,
}

/// Descriptive SLA errors and baseline comparisons are withheld until every
/// due day has a valid current aggregate and ticket-level score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSlaPolicyAssessment {
    pub policy_id: String,
    pub policy_sha256: String,
    pub source_lineage: String,
    pub queue_id: Option<String>,
    pub assessed_at_utc: String,
    pub due_days: usize,
    pub scored_days: usize,
    pub corrected_days: usize,
    pub complete: bool,
    pub total_abs_error: Option<u128>,
    pub no_change_total_abs_error: Option<u128>,
    pub seasonal_naive_total_abs_error: Option<u128>,
    pub seven_day_mean_total_abs_error: Option<u128>,
    pub model_abs_error_below_each_baseline: Option<bool>,
    pub fixed_prefix_interval: Option<ShadowSlaIntervalDiagnostic>,
    pub recent_7_day_error_sums: Option<ShadowSlaErrorSums>,
    pub recent_7_day_model_abs_error_below_each_baseline: Option<bool>,
    pub total_predicted_resolved_within_sla: Option<u128>,
    pub total_observed_resolved_within_sla: Option<u128>,
    pub days: Vec<ShadowSlaDayAssessment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSlaErrorSums {
    pub model: u128,
    pub no_change: u128,
    pub seasonal_naive: u128,
    pub seven_day_mean: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSlaIntervalPoint {
    pub target_day_utc: String,
    pub predicted_resolved_within_sla: u64,
    pub observed_resolved_within_sla: u64,
    pub lower_bound: u64,
    pub upper_bound: u64,
    pub covered: bool,
}

/// Fixed-prefix residual band, not a calibrated production interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSlaIntervalDiagnostic {
    pub method: String,
    pub target_coverage_basis_points: u16,
    pub observed_coverage_basis_points: u16,
    pub calibration_points: usize,
    pub evaluated_points: usize,
    pub calibration_radius: u64,
    pub recent_miss_count: usize,
    pub recent_window_points: usize,
    pub drift_signal: bool,
    pub points: Vec<ShadowSlaIntervalPoint>,
}

pub fn diagnose_fixed_sla_intervals(
    days: &[ShadowSlaDayAssessment],
    calibration_points: usize,
) -> Result<ShadowSlaIntervalDiagnostic, DecisionStoreError> {
    if calibration_points < 14
        || days.len()
            < calibration_points
                .checked_add(7)
                .ok_or(DecisionStoreError::Invalid)?
    {
        return Err(DecisionStoreError::Invalid);
    }
    let mut residuals = Vec::with_capacity(calibration_points);
    let mut previous_day = None;
    for (index, day) in days.iter().enumerate() {
        let timestamp = shadow_utc_midnight(&day.target_day_utc)?.timestamp();
        if previous_day.is_some_and(|prior| timestamp != prior + 86_400)
            || day.status != ShadowSlaDayStatus::Scored
            || day.aggregate_status != ShadowDayStatus::Scored
        {
            return Err(DecisionStoreError::Invalid);
        }
        previous_day = Some(timestamp);
        let predicted = day
            .predicted_resolved_within_sla
            .ok_or(DecisionStoreError::Invalid)?;
        let observed = day
            .observed_resolved_within_sla
            .ok_or(DecisionStoreError::Invalid)?;
        if index < calibration_points {
            residuals.push(predicted.abs_diff(observed));
        }
    }
    residuals.sort_unstable();
    let rank = calibration_points
        .checked_add(1)
        .and_then(|count| count.checked_mul(9))
        .ok_or(DecisionStoreError::Invalid)?
        .div_ceil(10);
    let radius = residuals[rank - 1];
    let mut points = Vec::with_capacity(days.len() - calibration_points);
    for day in &days[calibration_points..] {
        let predicted = day
            .predicted_resolved_within_sla
            .ok_or(DecisionStoreError::Invalid)?;
        let observed = day
            .observed_resolved_within_sla
            .ok_or(DecisionStoreError::Invalid)?;
        let lower_bound = predicted.saturating_sub(radius);
        let upper_bound = predicted
            .checked_add(radius)
            .ok_or(DecisionStoreError::Invalid)?;
        points.push(ShadowSlaIntervalPoint {
            target_day_utc: day.target_day_utc.clone(),
            predicted_resolved_within_sla: predicted,
            observed_resolved_within_sla: observed,
            lower_bound,
            upper_bound,
            covered: (lower_bound..=upper_bound).contains(&observed),
        });
    }
    let covered = points.iter().filter(|point| point.covered).count();
    let recent_window_points = points.len().min(7);
    let recent_miss_count = points
        .iter()
        .rev()
        .take(recent_window_points)
        .filter(|point| !point.covered)
        .count();
    Ok(ShadowSlaIntervalDiagnostic {
        method: "fixed_prefix_sla_absolute_residual_rank90_v1".into(),
        target_coverage_basis_points: 9_000,
        observed_coverage_basis_points: u16::try_from(
            covered
                .checked_mul(10_000)
                .ok_or(DecisionStoreError::Invalid)?
                / points.len(),
        )
        .map_err(|_| DecisionStoreError::Invalid)?,
        calibration_points,
        evaluated_points: points.len(),
        calibration_radius: radius,
        recent_miss_count,
        recent_window_points,
        drift_signal: recent_window_points == 7 && recent_miss_count >= 3,
        points,
    })
}

/// Historical review screen bound to one immutable empirical run and its
/// source versions. The saved resource plan and thresholds live in `report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPolicyScreen {
    pub replay_hash: String,
    pub empirical_run_id: String,
    pub empirical_run_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub report: JointRiskScreenReport,
}

