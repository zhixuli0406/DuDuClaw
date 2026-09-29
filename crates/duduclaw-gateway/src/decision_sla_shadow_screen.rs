//! SLA-specific review gate over prospective support shadow scores.
//!
//! This screen permits human inspection only; it cannot promote a model or
//! authorize a staffing intervention.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::decision_shadow_screen::{ShadowCoverageEvidence, ShadowReviewCriteria};
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, ShadowDayStatus, ShadowSlaDayStatus,
    ShadowSlaPolicyAssessment, StoredShadowSlaForecast, StoredShadowSlaScore,
    StoredShadowSlaScoreCorrection, diagnose_fixed_sla_intervals,
};

fn sla_screen_engine_sha256() -> String {
    let mut hash = Sha256::new();
    hash.update(include_str!("decision_sla_shadow_screen.rs").as_bytes());
    // `decision_store.rs` became a directory module in the audit O6 file
    // split; the fingerprint still covers the whole store.
    for part in crate::decision_store::ENGINE_SOURCES {
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaShadowReviewScreen {
    pub status: String,
    pub replay_hash: String,
    pub screen_engine_sha256: String,
    pub criteria: ShadowReviewCriteria,
    pub assessment: ShadowSlaPolicyAssessment,
    pub coverage_evidence: Option<ShadowCoverageEvidence>,
    pub failed_checks: Vec<String>,
    pub eligible_for_human_review: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSlaShadowReviewScreen {
    pub replay_hash: String,
    pub policy_id: String,
    pub policy_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub report: SlaShadowReviewScreen,
}

/// Exact human-inspection receipt for one SLA screen, not action approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaShadowScreenReviewLink {
    pub approval_id: String,
    pub agent_id: String,
    pub replay_hash: String,
    pub screen_record_sha256: String,
    pub policy_id: String,
    pub policy_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaShadowScreenReviewStatus {
    pub link: SlaShadowScreenReviewLink,
    pub status: ApprovalStatus,
    pub expires_at_utc: String,
    pub decided_by: Option<String>,
}

fn sla_screen_hash(report: &SlaShadowReviewScreen) -> Result<String, DecisionStoreError> {
    let bytes = serde_json::to_vec(&(
        "support-sla-shadow-review-screen-v1",
        &report.status,
        &report.screen_engine_sha256,
        &report.criteria,
        &report.assessment,
        &report.coverage_evidence,
        &report.failed_checks,
        report.eligible_for_human_review,
        &report.limitations,
    ))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn screen_sla_shadow_assessment(
    assessment: ShadowSlaPolicyAssessment,
    criteria: &ShadowReviewCriteria,
) -> Result<SlaShadowReviewScreen, DecisionStoreError> {
    if !(21..=366).contains(&criteria.min_complete_days) || criteria.min_fixed_coverage_bps > 10_000
    {
        return Err(DecisionStoreError::Invalid);
    }
    let mut failed = Vec::new();
    let scored = assessment
        .days
        .iter()
        .filter(|day| day.status == ShadowSlaDayStatus::Scored)
        .count();
    let corrected = assessment
        .days
        .iter()
        .filter(|day| day.status == ShadowSlaDayStatus::Scored && day.corrected)
        .count();
    let complete = !assessment.days.is_empty() && scored == assessment.days.len();
    let mut consistent = assessment.due_days == assessment.days.len()
        && assessment.scored_days == scored
        && assessment.corrected_days == corrected
        && assessment.complete == complete;
    if complete {
        let mut sums = [0_u128; 6];
        for day in &assessment.days {
            let values = [
                day.abs_error,
                day.no_change_abs_error,
                day.seasonal_naive_abs_error,
                day.seven_day_mean_abs_error,
                day.predicted_resolved_within_sla,
                day.observed_resolved_within_sla,
            ];
            if values.iter().any(Option::is_none) || day.aggregate_status != ShadowDayStatus::Scored
            {
                consistent = false;
                break;
            }
            for (sum, value) in sums.iter_mut().zip(values) {
                *sum += u128::from(value.expect("checked"));
            }
        }
        consistent &= assessment.total_abs_error == Some(sums[0])
            && assessment.no_change_total_abs_error == Some(sums[1])
            && assessment.seasonal_naive_total_abs_error == Some(sums[2])
            && assessment.seven_day_mean_total_abs_error == Some(sums[3])
            && assessment.total_predicted_resolved_within_sla == Some(sums[4])
            && assessment.total_observed_resolved_within_sla == Some(sums[5])
            && assessment.model_abs_error_below_each_baseline
                == Some(sums[0] < sums[1] && sums[0] < sums[2] && sums[0] < sums[3]);
    } else {
        consistent &= assessment.total_abs_error.is_none()
            && assessment.no_change_total_abs_error.is_none()
            && assessment.seasonal_naive_total_abs_error.is_none()
            && assessment.seven_day_mean_total_abs_error.is_none()
            && assessment.total_predicted_resolved_within_sla.is_none()
            && assessment.total_observed_resolved_within_sla.is_none()
            && assessment.model_abs_error_below_each_baseline.is_none();
    }
    if complete && assessment.due_days >= 21 {
        consistent &= assessment.fixed_prefix_interval.as_ref()
            == Some(&diagnose_fixed_sla_intervals(&assessment.days, 14)?);
        let recent = assessment.recent_7_day_error_sums.as_ref();
        if let Some(recent) = recent {
            let mut sums = [0_u128; 4];
            for day in assessment.days.iter().rev().take(7) {
                let values = [
                    day.abs_error,
                    day.no_change_abs_error,
                    day.seasonal_naive_abs_error,
                    day.seven_day_mean_abs_error,
                ];
                if values.iter().any(Option::is_none) {
                    consistent = false;
                    break;
                }
                for (sum, value) in sums.iter_mut().zip(values) {
                    *sum += u128::from(value.expect("checked"));
                }
            }
            consistent &= recent.model == sums[0]
                && recent.no_change == sums[1]
                && recent.seasonal_naive == sums[2]
                && recent.seven_day_mean == sums[3]
                && assessment.recent_7_day_model_abs_error_below_each_baseline
                    == Some(sums[0] < sums[1] && sums[0] < sums[2] && sums[0] < sums[3]);
        } else {
            consistent = false;
        }
    } else {
        consistent &= assessment.fixed_prefix_interval.is_none()
            && assessment.recent_7_day_error_sums.is_none()
            && assessment
                .recent_7_day_model_abs_error_below_each_baseline
                .is_none();
    }
    if !consistent {
        failed.push("assessment_inconsistent".into());
    }
    if !assessment.complete {
        failed.push("incomplete_due_days".into());
    }
    if assessment.due_days < criteria.min_complete_days {
        failed.push("insufficient_complete_days".into());
    }
    if assessment.model_abs_error_below_each_baseline != Some(true) {
        failed.push("sla_skill_not_better_than_all_baselines".into());
    }
    if assessment.recent_7_day_model_abs_error_below_each_baseline != Some(true) {
        failed.push("recent_sla_skill_not_better_than_all_baselines".into());
    }
    let mut coverage_evidence = None;
    match assessment.fixed_prefix_interval.as_ref() {
        None => failed.push("fixed_prefix_interval_unavailable".into()),
        Some(interval) if consistent => {
            let covered = interval.points.iter().filter(|point| point.covered).count();
            let required = (usize::from(criteria.min_fixed_coverage_bps)
                * interval.evaluated_points)
                .div_ceil(10_000);
            coverage_evidence = Some(ShadowCoverageEvidence {
                covered_days: covered,
                evaluated_days: interval.evaluated_points,
                minimum_covered_days: required,
            });
            if covered < required {
                failed.push("fixed_prefix_coverage_below_limit".into());
            }
            if interval.drift_signal {
                failed.push("recent_drift_signal".into());
            }
        }
        Some(_) => failed.push("fixed_prefix_interval_inconsistent".into()),
    }
    let mut report = SlaShadowReviewScreen {
        status: "exploratory_sla_shadow_review_screen".into(),
        replay_hash: String::new(), screen_engine_sha256: sla_screen_engine_sha256(),
        criteria: criteria.clone(), assessment, coverage_evidence,
        eligible_for_human_review: failed.is_empty(), failed_checks: failed,
        limitations: vec![
            "SLA coverage is descriptive for this shadow window, not a calibrated future guarantee".into(),
            "A passing screen permits human inspection only; source authentication and action approval remain separate".into(),
        ],
    };
    report.replay_hash = sla_screen_hash(&report)?;
    Ok(report)
}

impl DecisionStore {
    fn sla_screen_matches_current(
        &self,
        scope: &DecisionScope,
        screen: &StoredSlaShadowReviewScreen,
    ) -> Result<bool, DecisionStoreError> {
        if screen.report.screen_engine_sha256 != sla_screen_engine_sha256() {
            return Ok(false);
        }
        let mut current = self.assess_shadow_sla_policy(scope, &screen.policy_id)?;
        current.assessed_at_utc = screen.report.assessment.assessed_at_utc.clone();
        Ok(current == screen.report.assessment)
    }

    pub fn screen_sla_shadow_policy(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<SlaShadowReviewScreen, DecisionStoreError> {
        self.screen_sla_shadow_policy_at(scope, policy_id, criteria, chrono::Utc::now().timestamp())
    }

    pub(crate) fn screen_sla_shadow_policy_at(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
        assessed_at: i64,
    ) -> Result<SlaShadowReviewScreen, DecisionStoreError> {
        screen_sla_shadow_assessment(
            self.assess_shadow_sla_policy_at(scope, policy_id, assessed_at)?,
            criteria,
        )
    }

    fn sla_shadow_screen_source_refs(
        &self,
        scope: &DecisionScope,
        assessment: &ShadowSlaPolicyAssessment,
    ) -> Result<Vec<String>, DecisionStoreError> {
        if !assessment.complete
            || assessment.due_days == 0
            || assessment.days.len() != assessment.due_days
        {
            return Err(DecisionStoreError::Invalid);
        }
        let assessed_at = chrono::DateTime::parse_from_rfc3339(&assessment.assessed_at_utc)
            .map_err(|_| DecisionStoreError::Corrupt)?
            .timestamp();
        let mut refs = Vec::with_capacity(assessment.days.len() * 4);
        for day in &assessment.days {
            if day.status != ShadowSlaDayStatus::Scored
                || day.aggregate_status != ShadowDayStatus::Scored
            {
                return Err(DecisionStoreError::Corrupt);
            }
            let forecast_id = day
                .aggregate_forecast_id
                .as_deref()
                .ok_or(DecisionStoreError::Corrupt)?;
            let sla_id = day
                .sla_forecast_id
                .as_deref()
                .ok_or(DecisionStoreError::Corrupt)?;
            let revision_id = day
                .score_revision_id
                .as_deref()
                .ok_or(DecisionStoreError::Corrupt)?;
            let forecast = self.load_shadow_forecast(scope, forecast_id)?;
            let sla = self.load_shadow_sla_forecast(scope, sla_id)?;
            let (_, sla_sha): (StoredShadowSlaForecast, String) =
                self.get_with_digest(scope, "shadow_sla_forecast", sla_id)?;
            let (score, revision_sha) = if day.corrected {
                let correction = self.load_shadow_sla_score_correction(scope, revision_id)?;
                let (_, digest): (StoredShadowSlaScoreCorrection, String) =
                    self.get_with_digest(scope, "shadow_sla_score_correction", revision_id)?;
                (correction.corrected_score, digest)
            } else {
                let score = self.load_shadow_sla_score(scope, revision_id)?;
                let (_, digest): (StoredShadowSlaScore, String) =
                    self.get_with_digest(scope, "shadow_sla_score", revision_id)?;
                (score, digest)
            };
            let (aggregate_score, aggregate_sha) =
                self.load_shadow_score_revision(scope, forecast_id, &score.aggregate_score_id)?;
            if forecast.policy_id != assessment.policy_id
                || forecast.policy_sha256 != assessment.policy_sha256
                || forecast.source_lineage != assessment.source_lineage
                || forecast.queue_id != assessment.queue_id
                || forecast.target_day_utc != day.target_day_utc
                || sla.forecast_id != forecast_id
                || sla.target_day_utc != day.target_day_utc
                || sla.committed_at > assessed_at
                || score.sla_forecast_id != sla_id
                || score.sla_forecast_sha256 != sla_sha
                || score.aggregate_score_sha256 != aggregate_sha
                || score.scored_at > assessed_at
                || day.score_revision_sha256.as_deref() != Some(revision_sha.as_str())
                || day.predicted_resolved_within_sla != Some(score.predicted_resolved_within_sla)
                || day.observed_resolved_within_sla != Some(score.observed_resolved_within_sla)
                || day.abs_error != Some(score.abs_error)
                || day.no_change_abs_error != Some(score.no_change_abs_error)
                || day.seasonal_naive_abs_error != Some(score.seasonal_naive_abs_error)
                || day.seven_day_mean_abs_error != Some(score.seven_day_mean_abs_error)
            {
                return Err(DecisionStoreError::Corrupt);
            }
            refs.extend([
                forecast.training_sha256,
                sla.opening_sha256,
                aggregate_score.observation_sha256,
                score.observation_sha256,
            ]);
        }
        refs.sort();
        refs.dedup();
        Ok(refs)
    }

    pub fn put_sla_shadow_review_screen(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<StoredSlaShadowReviewScreen, DecisionStoreError> {
        self.put_sla_shadow_review_screen_at(
            scope,
            policy_id,
            criteria,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(crate) fn put_sla_shadow_review_screen_at(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
        assessed_at: i64,
    ) -> Result<StoredSlaShadowReviewScreen, DecisionStoreError> {
        let report = self.screen_sla_shadow_policy_at(scope, policy_id, criteria, assessed_at)?;
        let source_version_hashes =
            self.sla_shadow_screen_source_refs(scope, &report.assessment)?;
        let record = StoredSlaShadowReviewScreen {
            replay_hash: report.replay_hash.clone(),
            policy_id: policy_id.into(),
            policy_sha256: report.assessment.policy_sha256.clone(),
            source_version_hashes,
            report,
        };
        self.put(
            scope,
            "sla_shadow_review_screen",
            &record.replay_hash,
            &record,
            Some(&record.source_version_hashes),
        )?;
        Ok(record)
    }

    pub fn load_sla_shadow_review_screen(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredSlaShadowReviewScreen, DecisionStoreError> {
        let record: StoredSlaShadowReviewScreen =
            self.get(scope, "sla_shadow_review_screen", replay_hash)?;
        let (policy, policy_sha): (crate::decision_store::ShadowPilotPolicy, String) =
            self.get_with_digest(scope, "shadow_policy", &record.policy_id)?;
        if record.replay_hash != replay_hash
            || record.report.replay_hash != replay_hash
            || record.policy_id != record.report.assessment.policy_id
            || record.policy_sha256 != policy_sha
            || record.report.assessment.policy_sha256 != policy_sha
            || record.report.assessment.queue_id != policy.queue_id
            || sla_screen_hash(&record.report)? != replay_hash
            || self.sla_shadow_screen_source_refs(scope, &record.report.assessment)?
                != record.source_version_hashes
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if record.report.screen_engine_sha256 == sla_screen_engine_sha256()
            && screen_sla_shadow_assessment(
                record.report.assessment.clone(),
                &record.report.criteria,
            )? != record.report
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    pub async fn request_sla_shadow_screen_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        replay_hash: &str,
        agent_id: &str,
        summary: &str,
        ttl_seconds: i64,
    ) -> Result<SlaShadowScreenReviewLink, DecisionStoreError> {
        if scope.tenant_id.trim().is_empty()
            || scope.acl.trim().is_empty()
            || replay_hash.trim().is_empty()
            || agent_id.trim().is_empty()
            || summary.trim().is_empty()
            || summary.len() > 500
            || !(1..=86_400).contains(&ttl_seconds)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let screen = self.load_sla_shadow_review_screen(scope, replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.sla_screen_matches_current(scope, &screen)?
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let (_, screen_record_sha256): (StoredSlaShadowReviewScreen, String) =
            self.get_with_digest(scope, "sla_shadow_review_screen", replay_hash)?;
        let payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "screen_record_sha256": screen_record_sha256,
            "policy_id": screen.policy_id,
            "policy_sha256": screen.policy_sha256,
        });
        let approval_id = broker
            .request(
                agent_id,
                "support_sla_shadow_screen_review",
                summary,
                payload,
                ttl_seconds,
            )
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        let link = SlaShadowScreenReviewLink {
            approval_id: approval_id.to_string(),
            agent_id: agent_id.into(),
            replay_hash: replay_hash.into(),
            screen_record_sha256,
            policy_id: screen.policy_id,
            policy_sha256: screen.policy_sha256,
        };
        self.put(
            scope,
            "sla_shadow_screen_review",
            &link.approval_id,
            &link,
            Some(&screen.source_version_hashes),
        )?;
        Ok(link)
    }

    pub async fn require_sla_shadow_screen_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
    ) -> Result<SlaShadowScreenReviewLink, DecisionStoreError> {
        if scope.tenant_id.trim().is_empty()
            || scope.acl.trim().is_empty()
            || approval_id.trim().is_empty()
            || replay_hash.trim().is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let link: SlaShadowScreenReviewLink =
            self.get(scope, "sla_shadow_screen_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let screen = self.load_sla_shadow_review_screen(scope, replay_hash)?;
        let (_, screen_record_sha256): (StoredSlaShadowReviewScreen, String) =
            self.get_with_digest(scope, "sla_shadow_review_screen", replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.sla_screen_matches_current(scope, &screen)?
            || link.screen_record_sha256 != screen_record_sha256
            || link.policy_id != screen.policy_id
            || link.policy_sha256 != screen.policy_sha256
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let id = ApprovalId::from(approval_id.to_owned());
        if broker
            .poll(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?
            != ApprovalStatus::Approved
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let rec = broker
            .get(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let deadline = rec
            .deadline_rfc3339()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(&time).ok())
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let expected_payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "screen_record_sha256": screen_record_sha256,
            "policy_id": screen.policy_id,
            "policy_sha256": screen.policy_sha256,
        });
        if rec.id.as_str() != approval_id
            || rec.action_kind != "support_sla_shadow_screen_review"
            || rec.agent_id != link.agent_id
            || rec.payload != expected_payload
            || rec.decided_by.as_deref().is_none_or(str::is_empty)
            || chrono::Utc::now() >= deadline
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        Ok(link)
    }

    /// Report pending and terminal SLA inspection states only while the saved
    /// screen, active sources, current assessment and broker payload still match.
    pub async fn sla_shadow_screen_review_status(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
    ) -> Result<SlaShadowScreenReviewStatus, DecisionStoreError> {
        if scope.tenant_id.trim().is_empty()
            || scope.acl.trim().is_empty()
            || approval_id.trim().is_empty()
            || replay_hash.trim().is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let link: SlaShadowScreenReviewLink =
            self.get(scope, "sla_shadow_screen_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let screen = self.load_sla_shadow_review_screen(scope, replay_hash)?;
        let (_, screen_record_sha256): (StoredSlaShadowReviewScreen, String) =
            self.get_with_digest(scope, "sla_shadow_review_screen", replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.sla_screen_matches_current(scope, &screen)?
            || link.screen_record_sha256 != screen_record_sha256
            || link.policy_id != screen.policy_id
            || link.policy_sha256 != screen.policy_sha256
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let id = ApprovalId::from(approval_id.to_owned());
        let polled = broker
            .poll(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        let rec = broker
            .get(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let expires_at_utc = rec
            .deadline_rfc3339()
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let expires_at = chrono::DateTime::parse_from_rfc3339(&expires_at_utc)
            .map_err(|_| DecisionStoreError::ReviewDenied)?;
        let expected_payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "screen_record_sha256": screen_record_sha256,
            "policy_id": screen.policy_id,
            "policy_sha256": screen.policy_sha256,
        });
        if rec.id.as_str() != approval_id
            || rec.action_kind != "support_sla_shadow_screen_review"
            || rec.agent_id != link.agent_id
            || rec.payload != expected_payload
            || rec.status != polled
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let status = if chrono::Utc::now() >= expires_at {
            ApprovalStatus::Expired
        } else {
            polled
        };
        if status == ApprovalStatus::Approved {
            self.require_sla_shadow_screen_review(broker, scope, approval_id, replay_hash)
                .await?;
        }
        if self.load_sla_shadow_review_screen(scope, replay_hash)? != screen
            || !self.sla_screen_matches_current(scope, &screen)?
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        Ok(SlaShadowScreenReviewStatus {
            link,
            status,
            expires_at_utc,
            decided_by: rec.decided_by,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_store::{ShadowSlaDayAssessment, ShadowSlaErrorSums};

    fn synthetic_assessment(holdout_actual: u64) -> ShadowSlaPolicyAssessment {
        let start = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let days: Vec<_> = (0..21)
            .map(|index| {
                let actual = if index < 14 { 16 } else { holdout_actual };
                let target_day_utc = start
                    .checked_add_days(chrono::Days::new(index))
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                ShadowSlaDayAssessment {
                    target_day_utc,
                    aggregate_status: ShadowDayStatus::Scored,
                    status: ShadowSlaDayStatus::Scored,
                    aggregate_forecast_id: Some(format!("forecast-{index}")),
                    sla_forecast_id: Some(format!("sla-{index}")),
                    score_revision_id: Some(format!("score-{index}")),
                    score_revision_sha256: Some(format!("digest-{index}")),
                    corrected: false,
                    predicted_resolved_within_sla: Some(16),
                    observed_resolved_within_sla: Some(actual),
                    abs_error: Some(16_u64.abs_diff(actual)),
                    no_change_abs_error: Some(actual),
                    seasonal_naive_abs_error: Some(actual),
                    seven_day_mean_abs_error: Some(actual),
                }
            })
            .collect();
        let model = u128::from(16_u64.abs_diff(holdout_actual)) * 7;
        let baseline = 14 * 16 + u128::from(holdout_actual) * 7;
        let recent_baseline = u128::from(holdout_actual) * 7;
        let interval = diagnose_fixed_sla_intervals(&days, 14).unwrap();
        ShadowSlaPolicyAssessment {
            policy_id: "policy".into(),
            policy_sha256: "policy-digest".into(),
            source_lineage: "support".into(),
            queue_id: Some("support".into()),
            assessed_at_utc: (start
                .checked_add_days(chrono::Days::new(22))
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc())
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            due_days: 21,
            scored_days: 21,
            corrected_days: 0,
            complete: true,
            total_abs_error: Some(model),
            no_change_total_abs_error: Some(baseline),
            seasonal_naive_total_abs_error: Some(baseline),
            seven_day_mean_total_abs_error: Some(baseline),
            model_abs_error_below_each_baseline: Some(model < baseline),
            fixed_prefix_interval: Some(interval),
            recent_7_day_error_sums: Some(ShadowSlaErrorSums {
                model,
                no_change: recent_baseline,
                seasonal_naive: recent_baseline,
                seven_day_mean: recent_baseline,
            }),
            recent_7_day_model_abs_error_below_each_baseline: Some(model < recent_baseline),
            total_predicted_resolved_within_sla: Some(16 * 21),
            total_observed_resolved_within_sla: Some(baseline),
            days,
        }
    }

    #[test]
    fn sla_screen_requires_coverage_recent_skill_and_consistent_evidence() {
        let criteria = ShadowReviewCriteria {
            min_complete_days: 21,
            min_fixed_coverage_bps: 8_000,
        };
        let eligible = screen_sla_shadow_assessment(synthetic_assessment(16), &criteria).unwrap();
        assert!(eligible.eligible_for_human_review);
        assert_eq!(eligible.coverage_evidence.as_ref().unwrap().covered_days, 7);
        let drift = screen_sla_shadow_assessment(synthetic_assessment(25), &criteria).unwrap();
        assert!(!drift.eligible_for_human_review);
        assert!(
            drift
                .failed_checks
                .contains(&"recent_drift_signal".to_owned())
        );
        assert!(
            drift
                .failed_checks
                .contains(&"fixed_prefix_coverage_below_limit".to_owned())
        );
        let mut contradictory = synthetic_assessment(25);
        contradictory
            .fixed_prefix_interval
            .as_mut()
            .unwrap()
            .observed_coverage_basis_points = 10_000;
        let rejected = screen_sla_shadow_assessment(contradictory, &criteria).unwrap();
        assert!(
            rejected
                .failed_checks
                .contains(&"assessment_inconsistent".to_owned())
        );
        assert!(!rejected.eligible_for_human_review);
    }
}
