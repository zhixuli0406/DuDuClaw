//! Explicit engineering review gate over a prospective shadow assessment.
//!
//! This screen is read-only. A pass permits human inspection of the evidence;
//! it does not approve model promotion or a staffing intervention.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, ShadowDayStatus, ShadowPolicyAssessment,
    StoredShadowForecast, StoredShadowScore, StoredShadowScoreCorrection,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowReviewCriteria {
    /// At least 21 complete due days are needed for the fixed 14/7 split.
    pub min_complete_days: usize,
    pub min_fixed_coverage_bps: u16,
}

/// Exact holdout counts behind the displayed coverage percentage. With a
/// short shadow window, one day can move the percentage materially.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowCoverageEvidence {
    pub covered_days: usize,
    pub evaluated_days: usize,
    pub minimum_covered_days: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowReviewScreen {
    pub status: String,
    pub replay_hash: String,
    pub screen_engine_sha256: String,
    pub criteria: ShadowReviewCriteria,
    pub assessment: ShadowPolicyAssessment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_evidence: Option<ShadowCoverageEvidence>,
    pub failed_checks: Vec<String>,
    pub eligible_for_human_review: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShadowReviewScreen {
    pub replay_hash: String,
    pub policy_id: String,
    pub policy_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub report: ShadowReviewScreen,
}

/// Human inspection receipt for one exact saved screen. This is not model
/// promotion or authorization to execute a staffing change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowScreenReviewLink {
    pub approval_id: String,
    pub agent_id: String,
    pub replay_hash: String,
    pub screen_record_sha256: String,
    pub policy_id: String,
    pub policy_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowScreenReviewStatus {
    pub link: ShadowScreenReviewLink,
    pub status: ApprovalStatus,
    pub expires_at_utc: String,
    pub decided_by: Option<String>,
}

pub(crate) fn screen_hash(report: &ShadowReviewScreen) -> Result<String, DecisionStoreError> {
    let bytes = serde_json::to_vec(&(
        "support-shadow-review-screen-v3",
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

/// Earlier immutable screens lack exact coverage counts. Keep their audit
/// payloads readable; current-review checks still require the current engine.
fn legacy_screen_hash(report: &ShadowReviewScreen) -> Result<String, DecisionStoreError> {
    let bytes = serde_json::to_vec(&(
        "support-shadow-review-screen-v2",
        &report.status,
        &report.screen_engine_sha256,
        &report.criteria,
        &report.assessment,
        &report.failed_checks,
        report.eligible_for_human_review,
        &report.limitations,
    ))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn screen_hash_matches(
    report: &ShadowReviewScreen,
    expected: &str,
) -> Result<bool, DecisionStoreError> {
    Ok(screen_hash(report)? == expected
        || (report.coverage_evidence.is_none() && legacy_screen_hash(report)? == expected))
}

fn screen_engine_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_str!("decision_shadow_screen.rs").as_bytes())
    )
}

pub(crate) fn screen_shadow_assessment(
    assessment: ShadowPolicyAssessment,
    criteria: &ShadowReviewCriteria,
) -> Result<ShadowReviewScreen, DecisionStoreError> {
    if !(21..=366).contains(&criteria.min_complete_days) || criteria.min_fixed_coverage_bps > 10_000
    {
        return Err(DecisionStoreError::Invalid);
    }
    let mut failed: Vec<String> = Vec::new();
    if !assessment.complete {
        failed.push("incomplete_due_days".into());
    }
    if assessment.due_days < criteria.min_complete_days {
        failed.push("insufficient_complete_days".into());
    }
    if assessment.backlog_abs_error_below_each_baseline != Some(true) {
        failed.push("backlog_skill_not_better_than_all_baselines".into());
    }
    if assessment.recent_7_day_backlog_abs_error_below_each_baseline != Some(true) {
        failed.push("recent_backlog_skill_not_better_than_all_baselines".into());
    }
    let mut coverage_evidence = None;
    match assessment.fixed_prefix_interval.as_ref() {
        None => failed.push("fixed_prefix_interval_unavailable".into()),
        Some(interval) => {
            let covered = interval.points.iter().filter(|point| point.covered).count();
            let recent_points = interval.points.len().min(7);
            let recent_misses = interval
                .points
                .iter()
                .rev()
                .take(recent_points)
                .filter(|point| !point.covered)
                .count();
            let consistent = interval.method == "fixed_prefix_absolute_residual_rank90_v1"
                && interval.target_coverage_basis_points == 9_000
                && interval.calibration_points == 14
                && interval.evaluated_points == interval.points.len()
                && (7..=352).contains(&interval.evaluated_points)
                && (!assessment.complete
                    || interval.evaluated_points.checked_add(14) == Some(assessment.due_days))
                && interval.observed_coverage_basis_points as usize
                    == covered * 10_000 / interval.evaluated_points
                && interval.recent_window_points == recent_points
                && interval.recent_miss_count == recent_misses
                && interval.drift_signal == (recent_points == 7 && recent_misses >= 3)
                && interval
                    .points
                    .windows(2)
                    .all(|pair| pair[0].day_index.checked_add(1) == Some(pair[1].day_index))
                && interval.points.iter().all(|point| {
                    point.lower_bound
                        == point
                            .predicted_backlog_end
                            .saturating_sub(point.calibration_radius)
                        && point
                            .predicted_backlog_end
                            .checked_add(point.calibration_radius)
                            == Some(point.upper_bound)
                        && point.covered
                            == (point.lower_bound..=point.upper_bound)
                                .contains(&point.actual_backlog_end)
                });
            if !consistent {
                failed.push("fixed_prefix_interval_inconsistent".into());
            } else {
                let required = (criteria.min_fixed_coverage_bps as usize
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
        }
    }
    let screen_engine_sha256 = screen_engine_sha256();
    let mut report = ShadowReviewScreen {
        status: "exploratory_shadow_review_screen".into(),
        replay_hash: String::new(),
        screen_engine_sha256,
        criteria: criteria.clone(),
        assessment,
        coverage_evidence,
        eligible_for_human_review: failed.is_empty(),
        failed_checks: failed,
        limitations: vec![
            "Observed coverage is descriptive for this shadow window, not a calibrated future guarantee".into(),
            "A passing screen requires independent source provenance, operator review, and existing action approvals before any operational use".into(),
        ],
    };
    report.replay_hash = screen_hash(&report)?;
    Ok(report)
}

impl DecisionStore {
    fn shadow_screen_matches_current(
        &self,
        scope: &DecisionScope,
        screen: &StoredShadowReviewScreen,
    ) -> Result<bool, DecisionStoreError> {
        if screen.report.screen_engine_sha256 != screen_engine_sha256() {
            return Ok(false);
        }
        let mut current = self.assess_shadow_policy(scope, &screen.policy_id)?;
        current.assessed_at_utc = screen.report.assessment.assessed_at_utc.clone();
        Ok(current == screen.report.assessment)
    }

    pub fn screen_shadow_policy(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<ShadowReviewScreen, DecisionStoreError> {
        screen_shadow_assessment(self.assess_shadow_policy(scope, policy_id)?, criteria)
    }

    fn shadow_screen_source_refs(
        &self,
        scope: &DecisionScope,
        assessment: &ShadowPolicyAssessment,
    ) -> Result<Vec<String>, DecisionStoreError> {
        if !assessment.complete
            || assessment.due_days == 0
            || assessment.scored_days != assessment.due_days
            || assessment.days.len() != assessment.due_days
            || assessment.error_sums.is_none()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let mut refs = Vec::with_capacity(assessment.days.len() * 2);
        for day in &assessment.days {
            if day.status != ShadowDayStatus::Scored {
                return Err(DecisionStoreError::Corrupt);
            }
            let forecast_id = day
                .forecast_id
                .as_deref()
                .ok_or(DecisionStoreError::Corrupt)?;
            let revision_id = day
                .score_revision_id
                .as_deref()
                .ok_or(DecisionStoreError::Corrupt)?;
            let expected_revision_sha256 = day
                .score_revision_sha256
                .as_deref()
                .ok_or(DecisionStoreError::Corrupt)?;
            let forecast = self.load_shadow_forecast(scope, forecast_id)?;
            let (_, forecast_sha256): (StoredShadowForecast, String) =
                self.get_with_digest(scope, "shadow_forecast", forecast_id)?;
            if forecast.policy_id != assessment.policy_id
                || forecast.policy_sha256 != assessment.policy_sha256
                || forecast.source_lineage != assessment.source_lineage
                || forecast.queue_id != assessment.queue_id
                || forecast.target_day_utc != day.target_day_utc
            {
                return Err(DecisionStoreError::Corrupt);
            }
            let (score, revision_sha256) = if day.corrected {
                let correction = self.load_shadow_score_correction(scope, revision_id)?;
                let (_, digest): (StoredShadowScoreCorrection, String) =
                    self.get_with_digest(scope, "shadow_score_correction", revision_id)?;
                (correction.corrected_score, digest)
            } else {
                let score = self.load_shadow_score(scope, revision_id)?;
                let (_, digest): (StoredShadowScore, String) =
                    self.get_with_digest(scope, "shadow_score", revision_id)?;
                (score, digest)
            };
            if revision_sha256 != expected_revision_sha256
                || score.forecast_id != forecast_id
                || score.forecast_sha256 != forecast_sha256
            {
                return Err(DecisionStoreError::Corrupt);
            }
            refs.push(forecast.training_sha256);
            refs.push(score.observation_sha256);
        }
        refs.sort();
        refs.dedup();
        Ok(refs)
    }

    /// Freeze one complete shadow screen and its exact forecast/score sources.
    /// Later corrections do not rewrite this historical assessment.
    pub fn put_shadow_review_screen(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        criteria: &ShadowReviewCriteria,
    ) -> Result<StoredShadowReviewScreen, DecisionStoreError> {
        let report = self.screen_shadow_policy(scope, policy_id, criteria)?;
        let source_version_hashes = self.shadow_screen_source_refs(scope, &report.assessment)?;
        let record = StoredShadowReviewScreen {
            replay_hash: report.replay_hash.clone(),
            policy_id: policy_id.to_owned(),
            policy_sha256: report.assessment.policy_sha256.clone(),
            source_version_hashes,
            report,
        };
        self.put(
            scope,
            "shadow_review_screen",
            &record.replay_hash,
            &record,
            Some(&record.source_version_hashes),
        )?;
        Ok(record)
    }

    /// Read the historical screen after rechecking its exact source records.
    /// It does not silently substitute a later correction or policy result.
    pub fn load_shadow_review_screen(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredShadowReviewScreen, DecisionStoreError> {
        let record: StoredShadowReviewScreen =
            self.get(scope, "shadow_review_screen", replay_hash)?;
        let (policy, policy_sha256): (crate::decision_store::ShadowPilotPolicy, String) =
            self.get_with_digest(scope, "shadow_policy", &record.policy_id)?;
        if record.replay_hash != replay_hash
            || record.report.replay_hash != replay_hash
            || record.policy_id != record.report.assessment.policy_id
            || record.policy_sha256 != policy_sha256
            || record.report.assessment.policy_sha256 != policy_sha256
            || record.report.assessment.queue_id != policy.queue_id
            || !screen_hash_matches(&record.report, replay_hash)?
            || self.shadow_screen_source_refs(scope, &record.report.assessment)?
                != record.source_version_hashes
        {
            return Err(DecisionStoreError::Corrupt);
        }
        // The stored hash binds the verdict to the stored assessment, but a
        // record written straight into the owner-only SQLite file could carry
        // a self-consistent hash over a verdict this engine would never
        // produce. Recompute it whenever the engine identity still matches.
        if record.report.screen_engine_sha256 == screen_engine_sha256()
            && screen_shadow_assessment(record.report.assessment.clone(), &record.report.criteria)?
                != record.report
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    /// Ask a human to inspect an eligible, immutable screen. A broker request
    /// without its local link cannot pass the verification path.
    pub async fn request_shadow_screen_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        replay_hash: &str,
        agent_id: &str,
        summary: &str,
        ttl_seconds: i64,
    ) -> Result<ShadowScreenReviewLink, DecisionStoreError> {
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
        let screen = self.load_shadow_review_screen(scope, replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.shadow_screen_matches_current(scope, &screen)?
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let (_, screen_record_sha256): (StoredShadowReviewScreen, String) =
            self.get_with_digest(scope, "shadow_review_screen", replay_hash)?;
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
                "support_shadow_screen_review",
                summary,
                payload,
                ttl_seconds,
            )
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        let link = ShadowScreenReviewLink {
            approval_id: approval_id.to_string(),
            agent_id: agent_id.to_owned(),
            replay_hash: replay_hash.to_owned(),
            screen_record_sha256,
            policy_id: screen.policy_id,
            policy_sha256: screen.policy_sha256,
        };
        self.put(
            scope,
            "shadow_screen_review",
            &link.approval_id,
            &link,
            Some(&screen.source_version_hashes),
        )?;
        Ok(link)
    }

    /// Return the inspection receipt only while its exact screen, sources,
    /// broker decision, and deadline are still valid.
    pub async fn require_shadow_screen_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
    ) -> Result<ShadowScreenReviewLink, DecisionStoreError> {
        if scope.tenant_id.trim().is_empty()
            || scope.acl.trim().is_empty()
            || approval_id.trim().is_empty()
            || replay_hash.trim().is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let link: ShadowScreenReviewLink = self.get(scope, "shadow_screen_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let screen = self.load_shadow_review_screen(scope, replay_hash)?;
        let (_, screen_record_sha256): (StoredShadowReviewScreen, String) =
            self.get_with_digest(scope, "shadow_review_screen", replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.shadow_screen_matches_current(scope, &screen)?
            || link.screen_record_sha256 != screen_record_sha256
            || link.policy_id != screen.policy_id
            || link.policy_sha256 != screen.policy_sha256
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let id = ApprovalId::from(approval_id.to_owned());
        let status = broker
            .poll(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        if status != ApprovalStatus::Approved {
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
            || rec.action_kind != "support_shadow_screen_review"
            || rec.agent_id != link.agent_id
            || rec.payload != expected_payload
            || rec.decided_by.as_deref().is_none_or(str::is_empty)
            || chrono::Utc::now() >= deadline
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        Ok(link)
    }

    /// Report pending and terminal inspection states only while the saved
    /// screen, active sources, current assessment and broker payload still match.
    pub async fn shadow_screen_review_status(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
    ) -> Result<ShadowScreenReviewStatus, DecisionStoreError> {
        if !scope.valid() || approval_id.trim().is_empty() || replay_hash.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let link: ShadowScreenReviewLink = self.get(scope, "shadow_screen_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let screen = self.load_shadow_review_screen(scope, replay_hash)?;
        let (_, screen_record_sha256): (StoredShadowReviewScreen, String) =
            self.get_with_digest(scope, "shadow_review_screen", replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.shadow_screen_matches_current(scope, &screen)?
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
            "tenant_id": scope.tenant_id, "acl": scope.acl,
            "replay_hash": replay_hash, "screen_record_sha256": screen_record_sha256,
            "policy_id": screen.policy_id, "policy_sha256": screen.policy_sha256,
        });
        if rec.id.as_str() != approval_id
            || rec.action_kind != "support_shadow_screen_review"
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
            self.require_shadow_screen_review(broker, scope, approval_id, replay_hash)
                .await?;
        }
        if self.load_shadow_review_screen(scope, replay_hash)? != screen
            || !self.shadow_screen_matches_current(scope, &screen)?
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        Ok(ShadowScreenReviewStatus {
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
    use crate::decision_calibration::{IntervalDiagnostic, IntervalDiagnosticPoint};

    fn assessment() -> ShadowPolicyAssessment {
        ShadowPolicyAssessment {
            policy_id: "pilot".into(),
            policy_sha256: "digest".into(),
            source_lineage: "queue".into(),
            queue_id: Some("support-queue".into()),
            assessed_at_utc: "2026-09-27T00:00:00Z".into(),
            due_days: 21,
            scored_days: 21,
            corrected_days: 0,
            complete: true,
            error_sums: None,
            backlog_abs_error_below_each_baseline: Some(true),
            fixed_prefix_interval: Some(IntervalDiagnostic {
                method: "fixed_prefix_absolute_residual_rank90_v1".into(),
                target_coverage_basis_points: 9_000,
                observed_coverage_basis_points: 8_571,
                calibration_points: 14,
                evaluated_points: 7,
                recent_miss_count: 1,
                recent_window_points: 7,
                drift_signal: false,
                points: (0..7)
                    .map(|index| IntervalDiagnosticPoint {
                        day_index: 14 + index,
                        predicted_backlog_end: 100,
                        actual_backlog_end: if index == 6 { 102 } else { 100 },
                        lower_bound: 99,
                        upper_bound: 101,
                        calibration_radius: 1,
                        covered: index != 6,
                    })
                    .collect(),
            }),
            recent_7_day_error_sums: None,
            recent_7_day_backlog_abs_error_below_each_baseline: Some(true),
            days: vec![],
        }
    }

    #[test]
    fn shadow_screen_requires_complete_skill_coverage_and_no_drift() {
        let criteria = ShadowReviewCriteria {
            min_complete_days: 21,
            min_fixed_coverage_bps: 8_000,
        };
        let passing = screen_shadow_assessment(assessment(), &criteria).unwrap();
        assert!(passing.eligible_for_human_review);
        assert_eq!(
            passing.coverage_evidence,
            Some(ShadowCoverageEvidence {
                covered_days: 6,
                evaluated_days: 7,
                minimum_covered_days: 6,
            })
        );
        let mut historical = passing.clone();
        historical.coverage_evidence = None;
        historical.replay_hash = legacy_screen_hash(&historical).unwrap();
        let historical_json = serde_json::to_string(&historical).unwrap();
        assert!(!historical_json.contains("coverage_evidence"));
        let restored: ShadowReviewScreen = serde_json::from_str(&historical_json).unwrap();
        assert!(screen_hash_matches(&restored, &historical.replay_hash).unwrap());
        let mut recently_worse = assessment();
        recently_worse.recent_7_day_backlog_abs_error_below_each_baseline = Some(false);
        let recent_screen = screen_shadow_assessment(recently_worse, &criteria).unwrap();
        assert_eq!(
            recent_screen.failed_checks,
            vec!["recent_backlog_skill_not_better_than_all_baselines"]
        );
        let mut failing = assessment();
        failing.complete = false;
        failing.due_days = 20;
        failing.backlog_abs_error_below_each_baseline = None;
        failing.recent_7_day_backlog_abs_error_below_each_baseline = None;
        let interval = failing.fixed_prefix_interval.as_mut().unwrap();
        for index in [4, 5] {
            interval.points[index].actual_backlog_end = 102;
            interval.points[index].covered = false;
        }
        interval.observed_coverage_basis_points = 5_714;
        interval.recent_miss_count = 3;
        interval.drift_signal = true;
        let report = screen_shadow_assessment(failing, &criteria).unwrap();
        assert!(!report.eligible_for_human_review);
        assert_eq!(
            report.failed_checks,
            vec![
                "incomplete_due_days",
                "insufficient_complete_days",
                "backlog_skill_not_better_than_all_baselines",
                "recent_backlog_skill_not_better_than_all_baselines",
                "fixed_prefix_coverage_below_limit",
                "recent_drift_signal",
            ]
        );
        assert_eq!(screen_hash(&report).unwrap(), report.replay_hash);
        let mut contradictory = assessment();
        contradictory
            .fixed_prefix_interval
            .as_mut()
            .unwrap()
            .observed_coverage_basis_points = 10_000;
        let rejected = screen_shadow_assessment(
            contradictory,
            &ShadowReviewCriteria {
                min_complete_days: 21,
                min_fixed_coverage_bps: 8_000,
            },
        )
        .unwrap();
        assert!(!rejected.eligible_for_human_review);
        assert!(rejected.coverage_evidence.is_none());
        assert!(
            rejected
                .failed_checks
                .contains(&"fixed_prefix_interval_inconsistent".into())
        );
        let mut reordered = assessment();
        reordered.fixed_prefix_interval.as_mut().unwrap().points[1].day_index = 40;
        assert!(
            screen_shadow_assessment(
                reordered,
                &ShadowReviewCriteria {
                    min_complete_days: 21,
                    min_fixed_coverage_bps: 8_000,
                }
            )
            .unwrap()
            .failed_checks
            .contains(&"fixed_prefix_interval_inconsistent".into())
        );
        let mut changed_output = report.clone();
        changed_output.failed_checks.clear();
        assert_ne!(screen_hash(&changed_output).unwrap(), report.replay_hash);
        let mut invalid = criteria;
        invalid.min_fixed_coverage_bps = 10_001;
        assert!(matches!(
            screen_shadow_assessment(assessment(), &invalid),
            Err(DecisionStoreError::Invalid)
        ));
    }
}
