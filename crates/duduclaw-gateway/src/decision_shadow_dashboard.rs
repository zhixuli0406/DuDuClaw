//! Bounded Dashboard views and inline source adapters for the aggregate shadow journal.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveTime, Utc};
use duduclaw_memory::causal::EvidenceScope;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::decision_calibration::{IntervalDiagnostic, KnownDayInputs, ProspectiveForecast};
use crate::decision_shadow_screen::{
    ShadowCoverageEvidence, ShadowReviewCriteria, ShadowReviewScreen, StoredShadowReviewScreen,
};
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, ObservedOutcomeExport, ShadowDayStatus,
    ShadowErrorSums, ShadowPilotPolicy, ShadowPolicyAssessment, StoredShadowForecast,
    StoredShadowScore, validate_shadow_observation_source, validate_shadow_training_source,
};

/// Bounded lineage preview. The paired `*_total` field reports how many
/// digests the stored record actually carries, so a truncated list cannot be
/// read as a complete one.
fn digest_lineage(values: &[String]) -> Vec<String> {
    valid_digests(values).take(16).cloned().collect()
}

fn digest_lineage_total(values: &[String]) -> usize {
    valid_digests(values).count()
}

fn valid_digests(values: &[String]) -> impl Iterator<Item = &String> {
    values
        .iter()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
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

fn source_queue(source: &str) -> Result<String, DecisionStoreError> {
    if source.is_empty() || source.len() > 2 * 1024 * 1024 {
        return Err(DecisionStoreError::Invalid);
    }
    let export: ObservedOutcomeExport =
        serde_json::from_str(source).map_err(|_| DecisionStoreError::Invalid)?;
    let queue = export.queue_id.ok_or(DecisionStoreError::Invalid)?;
    if queue.is_empty() || queue.trim() != queue || queue.len() > 128 {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(queue)
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowPolicy {
    pub id: String,
    pub source_lineage: String,
    pub queue_id: Option<String>,
    pub effective_from_utc: String,
    pub effective_until_utc: String,
    pub issue_deadline_seconds: u32,
    pub min_training_days: usize,
    pub min_saturated_days: usize,
    pub calibration_engine_sha256: String,
    pub registered_at: i64,
    pub record_sha256: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowForecast {
    pub id: String,
    pub policy_id: String,
    pub policy_sha256: String,
    pub training_artifact_id: String,
    pub training_sha256: String,
    pub source_lineage: String,
    pub queue_id: Option<String>,
    pub training_window_start_utc: String,
    pub target_day_utc: String,
    pub committed_at: i64,
    pub min_saturated_days: usize,
    pub known: DashboardKnownDayInputs,
    pub calibration_engine_sha256: String,
    pub forecast: DashboardProspectiveForecast,
    pub record_sha256: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardKnownDayInputs {
    pub opening_backlog: String,
    pub planned_agents: u32,
    pub planned_fixed_extra_capacity: u32,
}

impl From<&KnownDayInputs> for DashboardKnownDayInputs {
    fn from(value: &KnownDayInputs) -> Self {
        Self {
            opening_backlog: value.opening_backlog.to_string(),
            planned_agents: value.planned_agents,
            planned_fixed_extra_capacity: value.planned_fixed_extra_capacity,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardProspectiveForecast {
    pub predicted_arrivals: u32,
    pub predicted_backlog_end: String,
    pub no_change_backlog_end: String,
    pub seasonal_naive_backlog_end: String,
    pub mean_change_backlog_end: String,
    pub fitted_capacity_per_agent: u32,
    pub training_days: usize,
}

impl From<&ProspectiveForecast> for DashboardProspectiveForecast {
    fn from(value: &ProspectiveForecast) -> Self {
        Self {
            predicted_arrivals: value.predicted_arrivals,
            predicted_backlog_end: value.predicted_backlog_end.to_string(),
            no_change_backlog_end: value.no_change_backlog_end.to_string(),
            seasonal_naive_backlog_end: value.seasonal_naive_backlog_end.to_string(),
            mean_change_backlog_end: value.mean_change_backlog_end.to_string(),
            fitted_capacity_per_agent: value.fitted_capacity_per_agent,
            training_days: value.training_days,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowScore {
    pub id: String,
    pub forecast_id: String,
    pub forecast_sha256: String,
    pub observation_artifact_id: String,
    pub observation_sha256: String,
    pub scored_at: i64,
    pub arrivals_abs_error: String,
    pub backlog_abs_error: String,
    pub no_change_abs_error: String,
    pub seasonal_naive_abs_error: String,
    pub mean_change_abs_error: String,
    pub record_sha256: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowErrorSums {
    pub arrivals: String,
    pub backlog: String,
    pub no_change_backlog: String,
    pub seasonal_naive_backlog: String,
    pub mean_change_backlog: String,
}

impl From<&ShadowErrorSums> for DashboardShadowErrorSums {
    fn from(value: &ShadowErrorSums) -> Self {
        Self {
            arrivals: value.arrivals.to_string(),
            backlog: value.backlog.to_string(),
            no_change_backlog: value.no_change_backlog.to_string(),
            seasonal_naive_backlog: value.seasonal_naive_backlog.to_string(),
            mean_change_backlog: value.mean_change_backlog.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowInterval {
    pub method: String,
    pub target_coverage_basis_points: u16,
    pub observed_coverage_basis_points: u16,
    pub calibration_points: usize,
    pub evaluated_points: usize,
    pub recent_miss_count: usize,
    pub recent_window_points: usize,
    pub drift_signal: bool,
    pub covered_points: usize,
}

impl From<&IntervalDiagnostic> for DashboardShadowInterval {
    fn from(value: &IntervalDiagnostic) -> Self {
        Self {
            method: value.method.clone(),
            target_coverage_basis_points: value.target_coverage_basis_points,
            observed_coverage_basis_points: value.observed_coverage_basis_points,
            calibration_points: value.calibration_points,
            evaluated_points: value.evaluated_points,
            recent_miss_count: value.recent_miss_count,
            recent_window_points: value.recent_window_points,
            drift_signal: value.drift_signal,
            covered_points: value.points.iter().filter(|point| point.covered).count(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowAssessment {
    pub policy_id: String,
    pub policy_sha256: String,
    pub source_lineage: String,
    pub queue_id: Option<String>,
    pub assessed_at_utc: String,
    pub due_days: usize,
    pub scored_days: usize,
    pub corrected_days: usize,
    pub complete: bool,
    pub error_sums: Option<DashboardShadowErrorSums>,
    pub backlog_abs_error_below_each_baseline: Option<bool>,
    pub fixed_prefix_interval: Option<DashboardShadowInterval>,
    pub recent_7_day_error_sums: Option<DashboardShadowErrorSums>,
    pub recent_7_day_backlog_abs_error_below_each_baseline: Option<bool>,
    pub day_status_counts: BTreeMap<String, usize>,
    pub limitations: Vec<String>,
}

impl From<ShadowPolicyAssessment> for DashboardShadowAssessment {
    fn from(value: ShadowPolicyAssessment) -> Self {
        let mut day_status_counts = BTreeMap::new();
        for day in &value.days {
            let key = match day.status {
                ShadowDayStatus::MissingForecast => "missing_forecast",
                ShadowDayStatus::ForecastInvalid => "forecast_invalid",
                ShadowDayStatus::ForecastRevoked => "forecast_revoked",
                ShadowDayStatus::Unscored => "unscored",
                ShadowDayStatus::ScoreInvalid => "score_invalid",
                ShadowDayStatus::ScoreRevoked => "score_revoked",
                ShadowDayStatus::Scored => "scored",
            };
            *day_status_counts.entry(key.to_owned()).or_insert(0) += 1;
        }
        Self {
            policy_id: value.policy_id,
            policy_sha256: value.policy_sha256,
            source_lineage: value.source_lineage,
            queue_id: value.queue_id,
            assessed_at_utc: value.assessed_at_utc,
            due_days: value.due_days,
            scored_days: value.scored_days,
            corrected_days: value.corrected_days,
            complete: value.complete,
            error_sums: value.error_sums.as_ref().map(Into::into),
            backlog_abs_error_below_each_baseline: value.backlog_abs_error_below_each_baseline,
            fixed_prefix_interval: value.fixed_prefix_interval.as_ref().map(Into::into),
            recent_7_day_error_sums: value.recent_7_day_error_sums.as_ref().map(Into::into),
            recent_7_day_backlog_abs_error_below_each_baseline:
                value.recent_7_day_backlog_abs_error_below_each_baseline,
            day_status_counts,
            limitations: vec![
                "This is an aggregate backlog forecast diagnostic, not an SLA validation or staffing effect".into(),
                "Local source possession and timestamps do not authenticate an upstream producer".into(),
                "Coverage is descriptive for the completed shadow window, not a calibrated future guarantee".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardShadowScreenReport {
    pub status: String,
    pub screen_engine_sha256: String,
    pub criteria: ShadowReviewCriteria,
    pub assessment: DashboardShadowAssessment,
    pub coverage_evidence: Option<ShadowCoverageEvidence>,
    pub failed_checks: Vec<String>,
    pub eligible_for_human_review: bool,
    pub limitations: Vec<String>,
}

impl From<ShadowReviewScreen> for DashboardShadowScreenReport {
    fn from(value: ShadowReviewScreen) -> Self {
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
pub struct DashboardShadowScreen {
    pub replay_hash: String,
    pub policy_id: String,
    pub policy_sha256: String,
    pub record_sha256: Option<String>,
    pub source_version_hashes: Vec<String>,
    pub source_version_hashes_total: usize,
    pub report: DashboardShadowScreenReport,
}

impl DecisionStore {
    pub fn dashboard_create_shadow_policy(
        &self,
        scope: &DecisionScope,
        id: &str,
        source_lineage: &str,
        queue_id: &str,
        effective_from_utc: &str,
        effective_until_utc: &str,
        issue_deadline_seconds: u32,
        min_training_days: usize,
        min_saturated_days: usize,
    ) -> Result<DashboardShadowPolicy, DecisionStoreError> {
        self.put_shadow_policy(
            scope,
            id,
            source_lineage,
            queue_id,
            effective_from_utc,
            effective_until_utc,
            issue_deadline_seconds,
            min_training_days,
            min_saturated_days,
        )?;
        self.dashboard_load_shadow_policy(scope, id)
    }

    pub fn dashboard_load_shadow_policy(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<DashboardShadowPolicy, DecisionStoreError> {
        let policy = self.load_shadow_policy(scope, id)?;
        let (stored, record_sha256): (ShadowPilotPolicy, String) =
            self.get_with_digest(scope, "shadow_policy", id)?;
        if stored != policy || self.load_shadow_policy(scope, id)? != policy {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardShadowPolicy {
            id: policy.id, source_lineage: policy.source_lineage, queue_id: policy.queue_id,
            effective_from_utc: policy.effective_from_utc,
            effective_until_utc: policy.effective_until_utc,
            issue_deadline_seconds: policy.issue_deadline_seconds,
            min_training_days: policy.min_training_days,
            min_saturated_days: policy.min_saturated_days,
            calibration_engine_sha256: policy.calibration_engine_sha256,
            registered_at: policy.registered_at, record_sha256,
            limitations: vec!["This policy only schedules exploratory shadow forecasts; it does not authorize staffing".into()],
        })
    }

    pub fn dashboard_create_shadow_forecast(
        &self,
        scope: &DecisionScope,
        id: &str,
        policy_id: &str,
        target_day_utc: &str,
        known: KnownDayInputs,
        training_artifact_id: Option<&str>,
        training_source_json: Option<&str>,
        retention_until_utc: Option<&str>,
    ) -> Result<DashboardShadowForecast, DecisionStoreError> {
        if known.opening_backlog > 9_007_199_254_740_991 {
            return Err(DecisionStoreError::Invalid);
        }
        if training_artifact_id.is_some() == training_source_json.is_some()
            || training_source_json.is_some() != retention_until_utc.is_some()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (artifact_id, created) = if let Some(source) = training_source_json {
            let policy = self.load_shadow_policy(scope, policy_id)?;
            let target = utc_midnight(target_day_utc)?;
            if target < utc_midnight(&policy.effective_from_utc)?
                || target >= utc_midnight(&policy.effective_until_utc)?
            {
                return Err(DecisionStoreError::Invalid);
            }
            let retention_at = retention(
                retention_until_utc.ok_or(DecisionStoreError::Invalid)?,
                target + 86_400,
            )?;
            let queue = source_queue(source)?;
            if policy.queue_id.as_deref() != Some(queue.as_str()) {
                return Err(DecisionStoreError::Invalid);
            }
            validate_shadow_training_source(source, target_day_utc, &policy, &known)?;
            let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
            match self.get_with_digest::<StoredShadowForecast>(scope, "shadow_forecast", id) {
                Ok((existing, _)) => {
                    let metadata = self.causal_source_store()?.read_artifact_metadata(
                        &EvidenceScope {
                            tenant_id: scope.tenant_id.clone(),
                            acl: scope.acl.clone(),
                        },
                        &existing.training_artifact_id,
                    )?;
                    if existing.policy_id == policy_id
                        && existing.target_day_utc == target_day_utc
                        && existing.known == known
                        && existing.training_sha256 == digest
                        && metadata.retention_at == retention_at
                    {
                        return self.dashboard_load_shadow_forecast(scope, id);
                    }
                    return Err(DecisionStoreError::VersionConflict);
                }
                Err(DecisionStoreError::NotFound) => {}
                Err(error) => return Err(error),
            }
            let now = Utc::now().timestamp();
            if now < target || now > target + i64::from(policy.issue_deadline_seconds) {
                return Err(DecisionStoreError::Invalid);
            }
            let causal = self.causal_source_store()?;
            let (artifact, created) = causal.add_artifact_with_created(
                &EvidenceScope {
                    tenant_id: scope.tenant_id.clone(),
                    acl: scope.acl.clone(),
                },
                "shadow_training_export",
                &format!("{id}:{}", uuid::Uuid::new_v4()),
                &digest,
                &policy.source_lineage,
                source,
                target,
                retention_at,
            )?;
            (artifact.id, created)
        } else {
            (
                training_artifact_id
                    .ok_or(DecisionStoreError::Invalid)?
                    .to_owned(),
                false,
            )
        };
        if let Err(error) =
            self.put_shadow_forecast(scope, id, &artifact_id, target_day_utc, known, policy_id)
        {
            // The forecast write can fail after the source was stored, for
            // example when another ID already reserved this target day. Keep
            // reused versions and any source referenced by a committed row.
            let committed = self
                .get_with_digest::<StoredShadowForecast>(scope, "shadow_forecast", id)
                .is_ok_and(|(record, _)| record.training_artifact_id == artifact_id);
            if created && !committed {
                self.causal_source_store()?.erase_artifact(
                    &EvidenceScope {
                        tenant_id: scope.tenant_id.clone(),
                        acl: scope.acl.clone(),
                    },
                    &artifact_id,
                )?;
            }
            return Err(error);
        }
        self.dashboard_load_shadow_forecast(scope, id)
    }

    pub fn dashboard_load_shadow_forecast(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<DashboardShadowForecast, DecisionStoreError> {
        let record = self.load_shadow_forecast(scope, id)?;
        let (stored, record_sha256): (StoredShadowForecast, String) =
            self.get_with_digest(scope, "shadow_forecast", id)?;
        if stored != record || self.load_shadow_forecast(scope, id)? != record {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardShadowForecast {
            id: record.id, policy_id: record.policy_id, policy_sha256: record.policy_sha256,
            training_artifact_id: record.training_artifact_id,
            training_sha256: record.training_sha256, source_lineage: record.source_lineage,
            queue_id: record.queue_id, training_window_start_utc: record.training_window_start_utc,
            target_day_utc: record.target_day_utc, committed_at: record.committed_at,
            min_saturated_days: record.min_saturated_days, known: (&record.known).into(),
            calibration_engine_sha256: record.calibration_engine_sha256,
            forecast: (&record.forecast).into(), record_sha256,
            limitations: vec![
                "The forecast was locally committed in the policy issue window; source identity is not externally authenticated".into(),
                "This is an aggregate demand and backlog forecast, not an SLA forecast or staffing effect".into(),
            ],
        })
    }

    pub fn dashboard_create_shadow_score(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        observation_artifact_id: Option<&str>,
        observation_source_json: Option<&str>,
        retention_until_utc: Option<&str>,
    ) -> Result<DashboardShadowScore, DecisionStoreError> {
        self.dashboard_create_shadow_score_inner(
            scope,
            id,
            forecast_id,
            observation_artifact_id,
            observation_source_json,
            retention_until_utc,
            |_| Ok(()),
        )
    }

    #[cfg(test)]
    pub(crate) fn dashboard_test_create_shadow_score_after_source<
        F: FnOnce(&str) -> Result<(), DecisionStoreError>,
    >(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        observation_source_json: &str,
        retention_until_utc: &str,
        after_source: F,
    ) -> Result<DashboardShadowScore, DecisionStoreError> {
        self.dashboard_create_shadow_score_inner(
            scope,
            id,
            forecast_id,
            None,
            Some(observation_source_json),
            Some(retention_until_utc),
            after_source,
        )
    }

    fn dashboard_create_shadow_score_inner<F: FnOnce(&str) -> Result<(), DecisionStoreError>>(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        observation_artifact_id: Option<&str>,
        observation_source_json: Option<&str>,
        retention_until_utc: Option<&str>,
        after_source: F,
    ) -> Result<DashboardShadowScore, DecisionStoreError> {
        if observation_artifact_id.is_some() == observation_source_json.is_some()
            || observation_source_json.is_some() != retention_until_utc.is_some()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (artifact_id, created) = if let Some(source) = observation_source_json {
            let forecast = self.load_shadow_forecast(scope, forecast_id)?;
            let end = utc_midnight(&forecast.target_day_utc)? + 86_400;
            if Utc::now().timestamp() < end {
                return Err(DecisionStoreError::Invalid);
            }
            let retention_at =
                retention(retention_until_utc.ok_or(DecisionStoreError::Invalid)?, end)?;
            let queue = source_queue(source)?;
            if forecast.queue_id.as_deref() != Some(queue.as_str()) {
                return Err(DecisionStoreError::Invalid);
            }
            validate_shadow_observation_source(source, &forecast)?;
            let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
            match self.get_with_digest::<StoredShadowScore>(scope, "shadow_score", id) {
                Ok((existing, _)) => {
                    let metadata = self.causal_source_store()?.read_artifact_metadata(
                        &EvidenceScope {
                            tenant_id: scope.tenant_id.clone(),
                            acl: scope.acl.clone(),
                        },
                        &existing.observation_artifact_id,
                    )?;
                    if existing.forecast_id == forecast_id
                        && existing.observation_sha256 == digest
                        && metadata.retention_at == retention_at
                    {
                        return self.dashboard_load_shadow_score(scope, id);
                    }
                    return Err(DecisionStoreError::VersionConflict);
                }
                Err(DecisionStoreError::NotFound) => {}
                Err(error) => return Err(error),
            }
            let causal = self.causal_source_store()?;
            let (artifact, created) = causal.add_artifact_with_created(
                &EvidenceScope {
                    tenant_id: scope.tenant_id.clone(),
                    acl: scope.acl.clone(),
                },
                "shadow_observation_export",
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
        let submitted = after_source(&artifact_id)
            .and_then(|_| self.put_shadow_score(scope, id, forecast_id, &artifact_id));
        if let Err(error) = submitted {
            let committed = self
                .get_with_digest::<StoredShadowScore>(scope, "shadow_score", id)
                .is_ok_and(|(record, _)| record.observation_artifact_id == artifact_id);
            if created && !committed {
                self.causal_source_store()?.erase_artifact(
                    &EvidenceScope {
                        tenant_id: scope.tenant_id.clone(),
                        acl: scope.acl.clone(),
                    },
                    &artifact_id,
                )?;
            }
            return Err(error);
        }
        self.dashboard_load_shadow_score(scope, id)
    }

    pub fn dashboard_load_shadow_score(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<DashboardShadowScore, DecisionStoreError> {
        let score = self.load_shadow_score(scope, id)?;
        let (stored, record_sha256): (StoredShadowScore, String) =
            self.get_with_digest(scope, "shadow_score", id)?;
        if stored != score || self.load_shadow_score(scope, id)? != score {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardShadowScore {
            id: score.id, forecast_id: score.forecast_id,
            forecast_sha256: score.forecast_sha256,
            observation_artifact_id: score.observation_artifact_id,
            observation_sha256: score.observation_sha256, scored_at: score.scored_at,
            arrivals_abs_error: score.arrivals_abs_error.to_string(),
            backlog_abs_error: score.backlog_abs_error.to_string(),
            no_change_abs_error: score.no_change_abs_error.to_string(),
            seasonal_naive_abs_error: score.seasonal_naive_abs_error.to_string(),
            mean_change_abs_error: score.mean_change_abs_error.to_string(), record_sha256,
            limitations: vec![
                "This scores one frozen aggregate forecast against a later supplied observation".into(),
                "The observation source is locally checked but its upstream producer is not authenticated".into(),
            ],
        })
    }

    pub fn dashboard_assess_shadow_policy(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
    ) -> Result<DashboardShadowAssessment, DecisionStoreError> {
        Ok(self.assess_shadow_policy(scope, policy_id)?.into())
    }

    pub fn dashboard_evaluate_shadow_screen(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<DashboardShadowScreen, DecisionStoreError> {
        let report = self.screen_shadow_policy(scope, policy_id, criteria)?;
        Ok(DashboardShadowScreen {
            replay_hash: report.replay_hash.clone(),
            policy_id: policy_id.to_owned(),
            policy_sha256: report.assessment.policy_sha256.clone(),
            record_sha256: None,
            source_version_hashes: Vec::new(),
            source_version_hashes_total: 0,
            report: report.into(),
        })
    }

    pub fn dashboard_save_shadow_screen(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<DashboardShadowScreen, DecisionStoreError> {
        let saved = self.put_shadow_review_screen(scope, policy_id, criteria)?;
        self.dashboard_load_shadow_screen(scope, &saved.replay_hash)
    }

    pub fn dashboard_load_shadow_screen(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<DashboardShadowScreen, DecisionStoreError> {
        let screen = self.load_shadow_review_screen(scope, replay_hash)?;
        let (stored, record_sha256): (StoredShadowReviewScreen, String) =
            self.get_with_digest(scope, "shadow_review_screen", replay_hash)?;
        if stored != screen || self.load_shadow_review_screen(scope, replay_hash)? != screen {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardShadowScreen {
            replay_hash: screen.replay_hash,
            policy_id: screen.policy_id,
            policy_sha256: screen.policy_sha256,
            record_sha256: Some(record_sha256),
            source_version_hashes: digest_lineage(&screen.source_version_hashes),
            source_version_hashes_total: digest_lineage_total(&screen.source_version_hashes),
            report: screen.report.into(),
        })
    }
}
