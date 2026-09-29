//! Bounded Dashboard views and inline source adapters for the ticket-SLA shadow journal.
//!
//! Ticket identities never leave this layer. Opening and observation exports are
//! ticket-level and can reach two mebibytes, so the responses below carry only
//! counts, digests, identifiers, timestamps, and limitations.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveTime, Utc};
use duduclaw_memory::causal::EvidenceScope;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::decision_ingest::{KnownSlaDayInputs, ProspectiveSlaForecast};
use crate::decision_shadow_screen::{ShadowCoverageEvidence, ShadowReviewCriteria};
use crate::decision_sla_shadow_screen::{SlaShadowReviewScreen, StoredSlaShadowReviewScreen};
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, ShadowSlaBaselines, ShadowSlaDayStatus,
    ShadowSlaErrorSums, ShadowSlaIntervalDiagnostic, ShadowSlaOpeningExport,
    ShadowSlaPolicyAssessment, StoredShadowSlaForecast, StoredShadowSlaScore,
    StoredShadowSlaScoreCorrection,
};

/// Largest accepted ticket-SLA source body: two mebibytes of exported source
/// plus headroom for the surrounding JSON envelope and its string escaping.
pub const MAX_SHADOW_SLA_SOURCE_BODY_BYTES: usize = 2 * 1024 * 1024 + 64 * 1024;

/// The store and CLI both cap one inline SLA source at two mebibytes.
const MAX_SHADOW_SLA_SOURCE_BYTES: usize = 2 * 1024 * 1024;

fn evidence_scope(scope: &DecisionScope) -> EvidenceScope {
    EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    }
}

fn digest_lineage(values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .take(16)
        .cloned()
        .collect()
}

fn utc_second(value: &str) -> Result<i64, DecisionStoreError> {
    let time = DateTime::parse_from_rfc3339(value).map_err(|_| DecisionStoreError::Invalid)?;
    if time.offset().local_minus_utc() != 0 {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(time.timestamp())
}

fn utc_midnight(value: &str) -> Result<i64, DecisionStoreError> {
    let time = DateTime::parse_from_rfc3339(value).map_err(|_| DecisionStoreError::Invalid)?;
    if time.offset().local_minus_utc() != 0
        || time.time() != NaiveTime::from_hms_opt(0, 0, 0).expect("midnight")
    {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(time.timestamp())
}

fn retention(value: &str, after: i64) -> Result<i64, DecisionStoreError> {
    let time = utc_second(value)?;
    if time <= Utc::now().timestamp() || time <= after {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(time)
}

fn bounded_source(source: &str) -> Result<(), DecisionStoreError> {
    if source.is_empty() || source.len() > MAX_SHADOW_SLA_SOURCE_BYTES {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(())
}

/// Counts only. The opening export lists every open ticket identity; the
/// Dashboard receives their totals and never their rows.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaOpening {
    pub opening_backlog: String,
    pub opening_cohort_count: usize,
    pub prior_resolved_ticket_count: usize,
    pub planned_agents: u32,
    pub planned_fixed_extra_capacity: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaPrediction {
    pub predicted_resolved_within_sla: u32,
    pub predicted_arrivals: u32,
    pub predicted_backlog_end: String,
    pub fitted_capacity_per_agent: u32,
    pub training_days: usize,
}

impl From<&ProspectiveSlaForecast> for DashboardShadowSlaPrediction {
    fn from(value: &ProspectiveSlaForecast) -> Self {
        Self {
            predicted_resolved_within_sla: value.predicted_resolved_within_sla,
            predicted_arrivals: value.backlog_forecast.predicted_arrivals,
            predicted_backlog_end: value.backlog_forecast.predicted_backlog_end.to_string(),
            fitted_capacity_per_agent: value.backlog_forecast.fitted_capacity_per_agent,
            training_days: value.backlog_forecast.training_days,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaBaselines {
    pub no_change: u32,
    pub seasonal_naive: u32,
    pub seven_day_mean: u32,
}

impl From<&ShadowSlaBaselines> for DashboardShadowSlaBaselines {
    fn from(value: &ShadowSlaBaselines) -> Self {
        Self {
            no_change: value.no_change,
            seasonal_naive: value.seasonal_naive,
            seven_day_mean: value.seven_day_mean,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaForecast {
    pub id: String,
    pub forecast_id: String,
    pub forecast_sha256: String,
    pub policy_id: String,
    pub model_version: String,
    pub model_sha256: String,
    pub opening_artifact_id: String,
    pub opening_sha256: String,
    pub source_lineage: String,
    pub queue_id: String,
    pub target_day_utc: String,
    pub committed_at: i64,
    pub opening: DashboardShadowSlaOpening,
    pub prediction: DashboardShadowSlaPrediction,
    pub baselines: DashboardShadowSlaBaselines,
    pub daily_engine_sha256: String,
    pub record_sha256: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaScore {
    pub id: String,
    pub sla_forecast_id: String,
    pub sla_forecast_sha256: String,
    pub aggregate_score_id: String,
    pub aggregate_score_sha256: String,
    pub observation_artifact_id: String,
    pub observation_sha256: String,
    pub scored_at: i64,
    pub predicted_resolved_within_sla: String,
    pub observed_resolved_within_sla: String,
    pub abs_error: String,
    pub no_change_abs_error: String,
    pub seasonal_naive_abs_error: String,
    pub seven_day_mean_abs_error: String,
    /// True when the current revision is a reviewed correction rather than the
    /// initial score. Corrections are filed through the CLI, not this Dashboard.
    pub correction_revision: bool,
    pub record_sha256: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaErrorSums {
    pub model: String,
    pub no_change: String,
    pub seasonal_naive: String,
    pub seven_day_mean: String,
}

impl From<&ShadowSlaErrorSums> for DashboardShadowSlaErrorSums {
    fn from(value: &ShadowSlaErrorSums) -> Self {
        Self {
            model: value.model.to_string(),
            no_change: value.no_change.to_string(),
            seasonal_naive: value.seasonal_naive.to_string(),
            seven_day_mean: value.seven_day_mean.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaInterval {
    pub method: String,
    pub target_coverage_basis_points: u16,
    pub observed_coverage_basis_points: u16,
    pub calibration_points: usize,
    pub evaluated_points: usize,
    pub calibration_radius: String,
    pub recent_miss_count: usize,
    pub recent_window_points: usize,
    pub drift_signal: bool,
    pub covered_points: usize,
}

impl From<&ShadowSlaIntervalDiagnostic> for DashboardShadowSlaInterval {
    fn from(value: &ShadowSlaIntervalDiagnostic) -> Self {
        Self {
            method: value.method.clone(),
            target_coverage_basis_points: value.target_coverage_basis_points,
            observed_coverage_basis_points: value.observed_coverage_basis_points,
            calibration_points: value.calibration_points,
            evaluated_points: value.evaluated_points,
            calibration_radius: value.calibration_radius.to_string(),
            recent_miss_count: value.recent_miss_count,
            recent_window_points: value.recent_window_points,
            drift_signal: value.drift_signal,
            covered_points: value.points.iter().filter(|point| point.covered).count(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowSlaAssessment {
    pub policy_id: String,
    pub policy_sha256: String,
    pub source_lineage: String,
    pub queue_id: Option<String>,
    pub assessed_at_utc: String,
    pub due_days: usize,
    pub scored_days: usize,
    pub corrected_days: usize,
    pub complete: bool,
    pub error_sums: Option<DashboardShadowSlaErrorSums>,
    pub model_abs_error_below_each_baseline: Option<bool>,
    pub fixed_prefix_interval: Option<DashboardShadowSlaInterval>,
    pub recent_7_day_error_sums: Option<DashboardShadowSlaErrorSums>,
    pub recent_7_day_model_abs_error_below_each_baseline: Option<bool>,
    pub total_predicted_resolved_within_sla: Option<String>,
    pub total_observed_resolved_within_sla: Option<String>,
    pub day_status_counts: BTreeMap<String, usize>,
    pub limitations: Vec<String>,
}

impl From<ShadowSlaPolicyAssessment> for DashboardShadowSlaAssessment {
    fn from(value: ShadowSlaPolicyAssessment) -> Self {
        let mut day_status_counts = BTreeMap::new();
        for day in &value.days {
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
            *day_status_counts.entry(key.to_owned()).or_insert(0) += 1;
        }
        let sums = |model: Option<u128>,
                    no_change: Option<u128>,
                    seasonal_naive: Option<u128>,
                    seven_day_mean: Option<u128>| {
            Some(DashboardShadowSlaErrorSums {
                model: model?.to_string(),
                no_change: no_change?.to_string(),
                seasonal_naive: seasonal_naive?.to_string(),
                seven_day_mean: seven_day_mean?.to_string(),
            })
        };
        Self {
            error_sums: sums(
                value.total_abs_error,
                value.no_change_total_abs_error,
                value.seasonal_naive_total_abs_error,
                value.seven_day_mean_total_abs_error,
            ),
            policy_id: value.policy_id,
            policy_sha256: value.policy_sha256,
            source_lineage: value.source_lineage,
            queue_id: value.queue_id,
            assessed_at_utc: value.assessed_at_utc,
            due_days: value.due_days,
            scored_days: value.scored_days,
            corrected_days: value.corrected_days,
            complete: value.complete,
            model_abs_error_below_each_baseline: value.model_abs_error_below_each_baseline,
            fixed_prefix_interval: value.fixed_prefix_interval.as_ref().map(Into::into),
            recent_7_day_error_sums: value.recent_7_day_error_sums.as_ref().map(Into::into),
            recent_7_day_model_abs_error_below_each_baseline: value
                .recent_7_day_model_abs_error_below_each_baseline,
            total_predicted_resolved_within_sla: value
                .total_predicted_resolved_within_sla
                .map(|total| total.to_string()),
            total_observed_resolved_within_sla: value
                .total_observed_resolved_within_sla
                .map(|total| total.to_string()),
            day_status_counts,
            limitations: vec![
                "This is a ticket-age SLA diagnostic over a local shadow window, not a served SLA guarantee".into(),
                "Local source possession and timestamps do not authenticate an upstream ticket system".into(),
                "Coverage is descriptive for the completed shadow window, not a calibrated future interval".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardSlaShadowScreenReport {
    pub status: String,
    pub screen_engine_sha256: String,
    pub criteria: ShadowReviewCriteria,
    pub assessment: DashboardShadowSlaAssessment,
    pub coverage_evidence: Option<ShadowCoverageEvidence>,
    pub failed_checks: Vec<String>,
    pub eligible_for_human_review: bool,
    pub limitations: Vec<String>,
}

impl From<SlaShadowReviewScreen> for DashboardSlaShadowScreenReport {
    fn from(value: SlaShadowReviewScreen) -> Self {
        Self {
            status: value.status,
            screen_engine_sha256: value.screen_engine_sha256,
            criteria: value.criteria,
            assessment: value.assessment.into(),
            coverage_evidence: value.coverage_evidence,
            failed_checks: value.failed_checks,
            eligible_for_human_review: value.eligible_for_human_review,
            limitations: value.limitations,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardSlaShadowScreen {
    pub replay_hash: String,
    pub policy_id: String,
    pub policy_sha256: String,
    pub record_sha256: Option<String>,
    pub source_version_hashes: Vec<String>,
    pub report: DashboardSlaShadowScreenReport,
}

fn opening_summary(
    inputs: &KnownSlaDayInputs,
    prior_resolved_ticket_count: usize,
) -> DashboardShadowSlaOpening {
    DashboardShadowSlaOpening {
        opening_backlog: inputs.known.opening_backlog.to_string(),
        opening_cohort_count: inputs.opening_cohorts.len(),
        prior_resolved_ticket_count,
        planned_agents: inputs.known.planned_agents,
        planned_fixed_extra_capacity: inputs.known.planned_fixed_extra_capacity,
    }
}

fn sla_forecast_limitations() -> Vec<String> {
    vec![
        "The SLA forecast was locally committed inside the policy issue window; the ticket source is not externally authenticated".into(),
        "Ticket identities stay in the bound source artifact; only counts and digests are returned".into(),
        "This is a synthetic or engineering shadow prediction, not a model promotion or staffing authorization".into(),
    ]
}

fn sla_score_limitations() -> Vec<String> {
    vec![
        "This scores one frozen ticket-SLA prediction against a later supplied day-end ticket export".into(),
        "The observation source is locally reconciled with the aggregate score but its upstream producer is not authenticated".into(),
        "A scored day is exploratory evidence only; it does not promote a model or authorize staffing".into(),
    ]
}

impl DecisionStore {
    /// Commit one ticket-SLA forecast from an existing artifact ID or from an
    /// inline opening export of at most two mebibytes.
    pub fn dashboard_create_shadow_sla_forecast(
        &self,
        scope: &DecisionScope,
        sla_id: &str,
        forecast_id: &str,
        model_version: &str,
        opening_artifact_id: Option<&str>,
        opening_source_json: Option<&str>,
        retention_until_utc: Option<&str>,
    ) -> Result<DashboardShadowSlaForecast, DecisionStoreError> {
        if opening_artifact_id.is_some() == opening_source_json.is_some()
            || opening_source_json.is_some() != retention_until_utc.is_some()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (artifact_id, created) = if let Some(source) = opening_source_json {
            let forecast = self.load_shadow_forecast(scope, forecast_id)?;
            let policy = self.load_shadow_policy(scope, &forecast.policy_id)?;
            let target = utc_midnight(&forecast.target_day_utc)?;
            let retention_at = retention(
                retention_until_utc.ok_or(DecisionStoreError::Invalid)?,
                target + 86_400,
            )?;
            bounded_source(source)?;
            self.preview_shadow_sla_forecast(scope, forecast_id, model_version, source)?;
            let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
            match self.get_with_digest::<StoredShadowSlaForecast>(
                scope,
                "shadow_sla_forecast",
                sla_id,
            ) {
                Ok((existing, _)) => {
                    let metadata = self.causal_source_store()?.read_artifact_metadata(
                        &evidence_scope(scope),
                        &existing.opening_artifact_id,
                    )?;
                    if existing.forecast_id == forecast_id
                        && existing.model_version == model_version
                        && existing.opening_sha256 == digest
                        && metadata.retention_at == retention_at
                    {
                        return self.dashboard_load_shadow_sla_forecast(scope, sla_id);
                    }
                    return Err(DecisionStoreError::VersionConflict);
                }
                Err(DecisionStoreError::NotFound) => {}
                Err(error) => return Err(error),
            }
            // A completed exact retry above stays readable after the historical
            // issue deadline; a new commitment must still be inside the window.
            let now = Utc::now().timestamp();
            if now < forecast.committed_at
                || now > target + i64::from(policy.issue_deadline_seconds)
                || now >= target + 86_400
            {
                return Err(DecisionStoreError::Invalid);
            }
            let causal = self.causal_source_store()?;
            let (artifact, created) = causal.add_artifact_with_created(
                &evidence_scope(scope),
                "shadow_sla_opening_export",
                &format!("{sla_id}:{}", uuid::Uuid::new_v4()),
                &digest,
                &forecast.source_lineage,
                source,
                target,
                retention_at,
            )?;
            (artifact.id, created)
        } else {
            (
                opening_artifact_id
                    .ok_or(DecisionStoreError::Invalid)?
                    .to_owned(),
                false,
            )
        };
        if let Err(error) =
            self.put_shadow_sla_forecast(scope, sla_id, forecast_id, model_version, &artifact_id)
        {
            // The SLA write can fail after the source was stored, for example
            // when another ID already reserved this backlog forecast. Keep
            // reused versions and any source referenced by a committed row.
            let committed = self
                .get_with_digest::<StoredShadowSlaForecast>(scope, "shadow_sla_forecast", sla_id)
                .is_ok_and(|(record, _)| record.opening_artifact_id == artifact_id);
            if created && !committed {
                self.causal_source_store()?
                    .erase_artifact(&evidence_scope(scope), &artifact_id)?;
            }
            return Err(error);
        }
        self.dashboard_load_shadow_sla_forecast(scope, sla_id)
    }

    pub fn dashboard_load_shadow_sla_forecast(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<DashboardShadowSlaForecast, DecisionStoreError> {
        let record = self.load_shadow_sla_forecast(scope, id)?;
        let (stored, record_sha256): (StoredShadowSlaForecast, String) =
            self.get_with_digest(scope, "shadow_sla_forecast", id)?;
        if stored != record || self.load_shadow_sla_forecast(scope, id)? != record {
            return Err(DecisionStoreError::Corrupt);
        }
        let forecast = self.load_shadow_forecast(scope, &record.forecast_id)?;
        let source = self
            .causal_source_store()?
            .source_text(&evidence_scope(scope), &record.opening_artifact_id)?;
        let opening: ShadowSlaOpeningExport =
            serde_json::from_str(&source).map_err(|_| DecisionStoreError::Corrupt)?;
        Ok(DashboardShadowSlaForecast {
            opening: opening_summary(&record.inputs, opening.prior_resolved_tickets.len()),
            prediction: (&record.prediction).into(),
            baselines: (&record.baselines).into(),
            daily_engine_sha256: record.prediction.daily_engine_sha256,
            policy_id: forecast.policy_id,
            source_lineage: forecast.source_lineage,
            id: record.id,
            forecast_id: record.forecast_id,
            forecast_sha256: record.forecast_sha256,
            model_version: record.model_version,
            model_sha256: record.model_sha256,
            opening_artifact_id: record.opening_artifact_id,
            opening_sha256: record.opening_sha256,
            queue_id: record.queue_id,
            target_day_utc: record.target_day_utc,
            committed_at: record.committed_at,
            record_sha256,
            limitations: sla_forecast_limitations(),
        })
    }

    /// Score one committed ticket-SLA forecast from an existing artifact ID or
    /// from an inline day-end ticket export of at most two mebibytes.
    pub fn dashboard_create_shadow_sla_score(
        &self,
        scope: &DecisionScope,
        id: &str,
        sla_forecast_id: &str,
        aggregate_score_id: &str,
        observation_artifact_id: Option<&str>,
        observation_source_json: Option<&str>,
        retention_until_utc: Option<&str>,
    ) -> Result<DashboardShadowSlaScore, DecisionStoreError> {
        self.dashboard_create_shadow_sla_score_inner(
            scope,
            id,
            sla_forecast_id,
            aggregate_score_id,
            observation_artifact_id,
            observation_source_json,
            retention_until_utc,
            |_| Ok(()),
        )
    }

    #[cfg(test)]
    pub(crate) fn dashboard_test_create_shadow_sla_score_after_source<
        F: FnOnce(&str) -> Result<(), DecisionStoreError>,
    >(
        &self,
        scope: &DecisionScope,
        id: &str,
        sla_forecast_id: &str,
        aggregate_score_id: &str,
        observation_source_json: &str,
        retention_until_utc: &str,
        after_source: F,
    ) -> Result<DashboardShadowSlaScore, DecisionStoreError> {
        self.dashboard_create_shadow_sla_score_inner(
            scope,
            id,
            sla_forecast_id,
            aggregate_score_id,
            None,
            Some(observation_source_json),
            Some(retention_until_utc),
            after_source,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dashboard_create_shadow_sla_score_inner<
        F: FnOnce(&str) -> Result<(), DecisionStoreError>,
    >(
        &self,
        scope: &DecisionScope,
        id: &str,
        sla_forecast_id: &str,
        aggregate_score_id: &str,
        observation_artifact_id: Option<&str>,
        observation_source_json: Option<&str>,
        retention_until_utc: Option<&str>,
        after_source: F,
    ) -> Result<DashboardShadowSlaScore, DecisionStoreError> {
        if observation_artifact_id.is_some() == observation_source_json.is_some()
            || observation_source_json.is_some() != retention_until_utc.is_some()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (artifact_id, created) = if let Some(source) = observation_source_json {
            let sla = self.load_shadow_sla_forecast(scope, sla_forecast_id)?;
            let forecast = self.load_shadow_forecast(scope, &sla.forecast_id)?;
            let end = utc_midnight(&sla.target_day_utc)? + 86_400;
            if Utc::now().timestamp() < end {
                return Err(DecisionStoreError::Invalid);
            }
            let retention_at =
                retention(retention_until_utc.ok_or(DecisionStoreError::Invalid)?, end)?;
            bounded_source(source)?;
            self.preview_shadow_sla_score(scope, sla_forecast_id, aggregate_score_id, source)?;
            let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
            match self.get_with_digest::<StoredShadowSlaScore>(scope, "shadow_sla_score", id) {
                Ok((existing, _)) => {
                    let metadata = self.causal_source_store()?.read_artifact_metadata(
                        &evidence_scope(scope),
                        &existing.observation_artifact_id,
                    )?;
                    if existing.sla_forecast_id == sla_forecast_id
                        && existing.aggregate_score_id == aggregate_score_id
                        && existing.observation_sha256 == digest
                        && metadata.retention_at == retention_at
                    {
                        return self.dashboard_load_shadow_sla_score(scope, id);
                    }
                    return Err(DecisionStoreError::VersionConflict);
                }
                Err(DecisionStoreError::NotFound) => {}
                Err(error) => return Err(error),
            }
            let causal = self.causal_source_store()?;
            let (artifact, created) = causal.add_artifact_with_created(
                &evidence_scope(scope),
                "shadow_sla_observation_export",
                &format!("{id}:{}", uuid::Uuid::new_v4()),
                &digest,
                &forecast.source_lineage,
                source,
                end,
                retention_at,
            )?;
            (artifact.id, created)
        } else {
            (
                observation_artifact_id
                    .ok_or(DecisionStoreError::Invalid)?
                    .to_owned(),
                false,
            )
        };
        let submitted = after_source(&artifact_id).and_then(|_| {
            self.put_shadow_sla_score(scope, id, sla_forecast_id, aggregate_score_id, &artifact_id)
        });
        if let Err(error) = submitted {
            let committed = self
                .get_with_digest::<StoredShadowSlaScore>(scope, "shadow_sla_score", id)
                .is_ok_and(|(record, _)| record.observation_artifact_id == artifact_id);
            if created && !committed {
                self.causal_source_store()?
                    .erase_artifact(&evidence_scope(scope), &artifact_id)?;
            }
            return Err(error);
        }
        self.dashboard_load_shadow_sla_score(scope, id)
    }

    pub fn dashboard_load_shadow_sla_score(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<DashboardShadowSlaScore, DecisionStoreError> {
        let score = self.load_shadow_sla_score(scope, id)?;
        let (stored, record_sha256): (StoredShadowSlaScore, String) =
            self.get_with_digest(scope, "shadow_sla_score", id)?;
        if stored != score || self.load_shadow_sla_score(scope, id)? != score {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(dashboard_sla_score(score, record_sha256, false))
    }

    /// Resolve the current revision, which a reviewed CLI correction may have
    /// advanced past the initial score.
    pub fn dashboard_load_current_shadow_sla_score(
        &self,
        scope: &DecisionScope,
        sla_forecast_id: &str,
    ) -> Result<DashboardShadowSlaScore, DecisionStoreError> {
        let score = self.load_current_shadow_sla_score(scope, sla_forecast_id)?;
        if self.load_current_shadow_sla_score(scope, sla_forecast_id)? != score {
            return Err(DecisionStoreError::Corrupt);
        }
        match self.get_with_digest::<StoredShadowSlaScore>(scope, "shadow_sla_score", &score.id) {
            Ok((stored, record_sha256)) => {
                if stored != score {
                    return Err(DecisionStoreError::Corrupt);
                }
                Ok(dashboard_sla_score(score, record_sha256, false))
            }
            Err(DecisionStoreError::NotFound) => {
                let (correction, record_sha256): (StoredShadowSlaScoreCorrection, String) =
                    self.get_with_digest(scope, "shadow_sla_score_correction", &score.id)?;
                if correction.corrected_score != score {
                    return Err(DecisionStoreError::Corrupt);
                }
                Ok(dashboard_sla_score(score, record_sha256, true))
            }
            Err(error) => Err(error),
        }
    }

    pub fn dashboard_assess_shadow_sla_policy(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
    ) -> Result<DashboardShadowSlaAssessment, DecisionStoreError> {
        Ok(self.assess_shadow_sla_policy(scope, policy_id)?.into())
    }

    pub fn dashboard_evaluate_sla_shadow_screen(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<DashboardSlaShadowScreen, DecisionStoreError> {
        let report = self.screen_sla_shadow_policy(scope, policy_id, criteria)?;
        Ok(DashboardSlaShadowScreen {
            replay_hash: report.replay_hash.clone(),
            policy_id: policy_id.to_owned(),
            policy_sha256: report.assessment.policy_sha256.clone(),
            record_sha256: None,
            source_version_hashes: Vec::new(),
            report: report.into(),
        })
    }

    pub fn dashboard_save_sla_shadow_screen(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<DashboardSlaShadowScreen, DecisionStoreError> {
        let saved = self.put_sla_shadow_review_screen(scope, policy_id, criteria)?;
        self.dashboard_load_sla_shadow_screen(scope, &saved.replay_hash)
    }

    pub fn dashboard_load_sla_shadow_screen(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<DashboardSlaShadowScreen, DecisionStoreError> {
        let screen = self.load_sla_shadow_review_screen(scope, replay_hash)?;
        let (stored, record_sha256): (StoredSlaShadowReviewScreen, String) =
            self.get_with_digest(scope, "sla_shadow_review_screen", replay_hash)?;
        if stored != screen || self.load_sla_shadow_review_screen(scope, replay_hash)? != screen {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardSlaShadowScreen {
            replay_hash: screen.replay_hash,
            policy_id: screen.policy_id,
            policy_sha256: screen.policy_sha256,
            record_sha256: Some(record_sha256),
            source_version_hashes: digest_lineage(&screen.source_version_hashes),
            report: screen.report.into(),
        })
    }
}

fn dashboard_sla_score(
    score: StoredShadowSlaScore,
    record_sha256: String,
    correction_revision: bool,
) -> DashboardShadowSlaScore {
    DashboardShadowSlaScore {
        id: score.id,
        sla_forecast_id: score.sla_forecast_id,
        sla_forecast_sha256: score.sla_forecast_sha256,
        aggregate_score_id: score.aggregate_score_id,
        aggregate_score_sha256: score.aggregate_score_sha256,
        observation_artifact_id: score.observation_artifact_id,
        observation_sha256: score.observation_sha256,
        scored_at: score.scored_at,
        predicted_resolved_within_sla: score.predicted_resolved_within_sla.to_string(),
        observed_resolved_within_sla: score.observed_resolved_within_sla.to_string(),
        abs_error: score.abs_error.to_string(),
        no_change_abs_error: score.no_change_abs_error.to_string(),
        seasonal_naive_abs_error: score.seasonal_naive_abs_error.to_string(),
        seven_day_mean_abs_error: score.seven_day_mean_abs_error.to_string(),
        correction_revision,
        record_sha256,
        limitations: sla_score_limitations(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_store::c7_synthetic_shadow_harness::seed_sla_dashboard_window;
    use duduclaw_memory::causal::CausalStore;
    use rusqlite::Connection;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: DecisionStore,
        causal: CausalStore,
        scope: DecisionScope,
        evidence: EvidenceScope,
        sla_forecast_id: String,
        aggregate_score_id: String,
        observation_json: String,
        retention_utc: String,
    }

    fn fixture(tenant: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: tenant.into(),
            acl: "private".into(),
        };
        let evidence = evidence_scope(&scope);
        let retained = Utc::now() + chrono::Duration::days(30);
        let seed = seed_sla_dashboard_window(
            &store,
            &causal,
            &scope,
            &evidence,
            &format!("sla-dashboard-{tenant}"),
            retained.timestamp(),
        );
        Fixture {
            _dir: dir,
            store,
            causal,
            scope,
            evidence,
            sla_forecast_id: seed.last_sla_forecast_id,
            aggregate_score_id: seed.last_aggregate_score_id,
            observation_json: seed.last_observation_json,
            retention_utc: retained.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }
    }

    #[test]
    fn failed_sla_score_erases_only_its_own_new_source() {
        let fixture = fixture("cleanup");
        let failed = fixture
            .store
            .dashboard_test_create_shadow_sla_score_after_source(
                &fixture.scope,
                "sla-score-dashboard",
                &fixture.sla_forecast_id,
                &fixture.aggregate_score_id,
                &fixture.observation_json,
                &fixture.retention_utc,
                |_| Err(DecisionStoreError::VersionConflict),
            );
        assert!(matches!(failed, Err(DecisionStoreError::VersionConflict)));
        let orphans: i64 = Connection::open(fixture.causal.path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM causal_artifacts
                 WHERE tenant_id=?1 AND acl=?2 AND kind='shadow_sla_observation_export'
                 AND external_id LIKE 'sla-score-dashboard:%' AND invalidated_at IS NULL",
                rusqlite::params![fixture.scope.tenant_id, fixture.scope.acl],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);

        // The same ID succeeds afterwards and keeps its own committed source.
        let saved = fixture
            .store
            .dashboard_create_shadow_sla_score(
                &fixture.scope,
                "sla-score-dashboard",
                &fixture.sla_forecast_id,
                &fixture.aggregate_score_id,
                None,
                Some(&fixture.observation_json),
                Some(&fixture.retention_utc),
            )
            .unwrap();
        assert_eq!(
            fixture
                .causal
                .source_text(&fixture.evidence, &saved.observation_artifact_id)
                .unwrap(),
            fixture.observation_json
        );
        let repeated = fixture
            .store
            .dashboard_create_shadow_sla_score(
                &fixture.scope,
                "sla-score-dashboard",
                &fixture.sla_forecast_id,
                &fixture.aggregate_score_id,
                None,
                Some(&fixture.observation_json),
                Some(&fixture.retention_utc),
            )
            .unwrap();
        assert_eq!(
            repeated.observation_artifact_id,
            saved.observation_artifact_id
        );
        assert_eq!(repeated.record_sha256, saved.record_sha256);
    }

    #[test]
    fn interleaved_sla_score_callers_keep_the_surviving_source() {
        let fixture = fixture("interleaved");
        let (a_ready_tx, a_ready_rx) = std::sync::mpsc::channel();
        let (b_ready_tx, b_ready_rx) = std::sync::mpsc::channel();
        let (release_a_tx, release_a_rx) = std::sync::mpsc::channel();
        let (release_b_tx, release_b_rx) = std::sync::mpsc::channel();
        let spawn = |store: DecisionStore,
                     scope: DecisionScope,
                     sla_forecast_id: String,
                     aggregate_score_id: String,
                     source: String,
                     retention: String,
                     ready: std::sync::mpsc::Sender<String>,
                     release: std::sync::mpsc::Receiver<()>,
                     inject_failure: bool| {
            std::thread::spawn(move || {
                store.dashboard_test_create_shadow_sla_score_after_source(
                    &scope,
                    "sla-score-contended",
                    &sla_forecast_id,
                    &aggregate_score_id,
                    &source,
                    &retention,
                    |artifact_id| {
                        ready.send(artifact_id.to_owned()).unwrap();
                        release.recv().unwrap();
                        if inject_failure {
                            Err(DecisionStoreError::VersionConflict)
                        } else {
                            Ok(())
                        }
                    },
                )
            })
        };
        let contender_a = spawn(
            fixture.store.clone(),
            fixture.scope.clone(),
            fixture.sla_forecast_id.clone(),
            fixture.aggregate_score_id.clone(),
            fixture.observation_json.clone(),
            fixture.retention_utc.clone(),
            a_ready_tx,
            release_a_rx,
            true,
        );
        // Let A finish registering its source before B starts. The property
        // under test is the interleaving *after* both sources exist (A fails
        // and cleans up while B is still in flight); racing the two source
        // inserts themselves only adds SQLite write contention that can make
        // one contender fail before it ever reaches its ready signal.
        let source_a = a_ready_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        let contender_b = spawn(
            fixture.store.clone(),
            fixture.scope.clone(),
            fixture.sla_forecast_id.clone(),
            fixture.aggregate_score_id.clone(),
            fixture.observation_json.clone(),
            fixture.retention_utc.clone(),
            b_ready_tx,
            release_b_rx,
            false,
        );
        let source_b = b_ready_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        assert_ne!(source_a, source_b);
        release_a_tx.send(()).unwrap();
        assert!(matches!(
            contender_a.join().unwrap(),
            Err(DecisionStoreError::VersionConflict)
        ));
        let (loser_content, loser_invalidated): (String, Option<i64>) =
            Connection::open(fixture.causal.path())
                .unwrap()
                .query_row(
                    "SELECT content,invalidated_at FROM causal_artifacts WHERE id=?1",
                    [&source_a],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
        assert!(loser_content.is_empty());
        assert!(loser_invalidated.is_some());
        // The surviving source is still in flight when A's cleanup ends.
        assert_eq!(
            fixture
                .causal
                .source_text(&fixture.evidence, &source_b)
                .unwrap(),
            fixture.observation_json
        );
        release_b_tx.send(()).unwrap();
        let winner = contender_b.join().unwrap().unwrap();
        assert_eq!(winner.observation_artifact_id, source_b);
        assert_eq!(winner.correction_revision, false);
        let current = fixture
            .store
            .dashboard_load_current_shadow_sla_score(&fixture.scope, &fixture.sla_forecast_id)
            .unwrap();
        assert_eq!(current.record_sha256, winner.record_sha256);
        assert!(!current.correction_revision);
    }

    #[test]
    fn inline_sla_sources_reject_oversized_and_ambiguous_input() {
        let fixture = fixture("bounds");
        let oversized = "x".repeat(MAX_SHADOW_SLA_SOURCE_BYTES + 1);
        assert!(matches!(
            fixture.store.dashboard_create_shadow_sla_score(
                &fixture.scope,
                "sla-score-oversized",
                &fixture.sla_forecast_id,
                &fixture.aggregate_score_id,
                None,
                Some(&oversized),
                Some(&fixture.retention_utc),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            fixture.store.dashboard_create_shadow_sla_score(
                &fixture.scope,
                "sla-score-both",
                &fixture.sla_forecast_id,
                &fixture.aggregate_score_id,
                Some("some-artifact"),
                Some(&fixture.observation_json),
                Some(&fixture.retention_utc),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            fixture.store.dashboard_create_shadow_sla_score(
                &fixture.scope,
                "sla-score-no-retention",
                &fixture.sla_forecast_id,
                &fixture.aggregate_score_id,
                None,
                Some(&fixture.observation_json),
                None,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let untouched: i64 = Connection::open(fixture.causal.path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM causal_artifacts
                 WHERE tenant_id=?1 AND acl=?2 AND kind='shadow_sla_observation_export'
                 AND external_id LIKE 'sla-score-%'",
                rusqlite::params![fixture.scope.tenant_id, fixture.scope.acl],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(untouched, 0);
    }
}
