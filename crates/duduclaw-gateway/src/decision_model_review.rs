//! Read-only readiness screen for a capacity-fit candidate.
//!
//! Passing permits human model inspection only. It does not activate a model
//! or authorize an operational staffing change.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::decision_outcome_calibration::{CapacityHoldoutDiagnostic, engine_sha256};
use crate::decision_sim::DecisionSnapshot;
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, StoredObservedOutcome,
    StoredOutcomeCalibration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeModelReviewCriteria {
    pub min_saturated_days: usize,
    pub min_holdout_days: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeModelReviewScreen {
    pub status: String,
    pub replay_hash: String,
    pub review_engine_sha256: String,
    pub criteria: OutcomeModelReviewCriteria,
    pub fit_id: String,
    pub fit_record_sha256: String,
    pub outcome_id: String,
    pub replayed_run_hash: String,
    pub observed_source_sha256: String,
    pub parent_model_version: String,
    pub parent_model_sha256: String,
    pub fitted_capacity_per_agent_day: u32,
    pub saturated_training_days: usize,
    pub holdout: Option<CapacityHoldoutDiagnostic>,
    pub queue_id: Option<String>,
    pub local_run_precedes_window: Option<bool>,
    pub eligible_for_human_review: bool,
    pub failed_checks: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOutcomeModelReviewScreen {
    pub replay_hash: String,
    pub fit_id: String,
    pub fit_record_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub report: OutcomeModelReviewScreen,
}

/// Receipt for human inspection of a model candidate, not model activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeModelReviewLink {
    pub approval_id: String,
    pub agent_id: String,
    pub replay_hash: String,
    pub screen_record_sha256: String,
    pub fit_id: String,
    pub fit_record_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeModelReviewStatus {
    pub link: OutcomeModelReviewLink,
    pub status: ApprovalStatus,
    pub expires_at_utc: String,
    pub decided_by: Option<String>,
}

fn review_engine_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_str!("decision_model_review.rs").as_bytes())
    )
}

fn screen_hash(screen: &OutcomeModelReviewScreen) -> Result<String, DecisionStoreError> {
    let mut payload = screen.clone();
    payload.replay_hash.clear();
    let mut hash = Sha256::new();
    hash.update(b"support-outcome-model-review-v1");
    hash.update(serde_json::to_vec(&payload)?);
    Ok(format!("{:x}", hash.finalize()))
}

pub fn screen_outcome_model_review(
    fit: &StoredOutcomeCalibration,
    fit_record_sha256: &str,
    outcome: &StoredObservedOutcome,
    criteria: &OutcomeModelReviewCriteria,
) -> Result<OutcomeModelReviewScreen, DecisionStoreError> {
    if !(3..=366).contains(&criteria.min_saturated_days)
        || !(3..=366).contains(&criteria.min_holdout_days)
        || fit_record_sha256.is_empty()
        || fit.outcome_id != outcome.id
        || fit.replay_hash != outcome.replay_hash
        || fit.observed_source_sha256 != outcome.observed_source_sha256
        || fit.parent_model_version != outcome.model_version
    {
        return Err(DecisionStoreError::Invalid);
    }
    let mut failed = Vec::new();
    if fit.fit_engine_sha256 != engine_sha256() {
        failed.push("fit_engine_not_current".into());
    }
    if fit.fit.saturated_days < criteria.min_saturated_days {
        failed.push("insufficient_saturated_training_days".into());
    }
    match &fit.holdout {
        Some(holdout) => {
            if holdout.holdout_days < criteria.min_holdout_days {
                failed.push("insufficient_holdout_days".into());
            }
            if !holdout.candidate_beats_parent
                || holdout.candidate_abs_error_sum >= holdout.parent_abs_error_sum
            {
                failed.push("candidate_does_not_beat_parent".into());
            }
            if !holdout.candidate_beats_no_change
                || holdout.candidate_abs_error_sum >= holdout.no_change_abs_error_sum
            {
                failed.push("candidate_does_not_beat_no_change".into());
            }
        }
        None => failed.push("holdout_unavailable".into()),
    }
    if outcome.local_run_precedes_window != Some(true) {
        failed.push("run_not_committed_before_observation_window".into());
    }
    if outcome.queue_id.as_deref().is_none_or(str::is_empty) {
        failed.push("queue_identity_unavailable".into());
    }
    let mut screen = OutcomeModelReviewScreen {
        status: "exploratory_outcome_model_review_screen".into(),
        replay_hash: String::new(), review_engine_sha256: review_engine_sha256(),
        criteria: criteria.clone(), fit_id: fit.id.clone(),
        fit_record_sha256: fit_record_sha256.into(),
        outcome_id: outcome.id.clone(), replayed_run_hash: outcome.replay_hash.clone(),
        observed_source_sha256: outcome.observed_source_sha256.clone(),
        parent_model_version: fit.parent_model_version.clone(),
        parent_model_sha256: fit.parent_model_sha256.clone(),
        fitted_capacity_per_agent_day: fit.fit.service_per_agent_day,
        saturated_training_days: fit.fit.saturated_days,
        holdout: fit.holdout.clone(), queue_id: outcome.queue_id.clone(),
        local_run_precedes_window: outcome.local_run_precedes_window,
        eligible_for_human_review: failed.is_empty(), failed_checks: failed,
        limitations: vec![
            "Holdout errors use observed arrivals and opening backlog; they are not live demand forecast skill".into(),
            "Local run timing and source digests do not authenticate an upstream data producer".into(),
            "Passing permits human inspection only; model activation and staffing actions require separate gates".into(),
        ],
    };
    screen.replay_hash = screen_hash(&screen)?;
    Ok(screen)
}

impl DecisionStore {
    pub fn screen_outcome_model_candidate(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
        criteria: &OutcomeModelReviewCriteria,
    ) -> Result<OutcomeModelReviewScreen, DecisionStoreError> {
        let fit = self.load_outcome_calibration(scope, fit_id)?;
        let (stored, fit_record_sha256): (StoredOutcomeCalibration, String) =
            self.get_with_digest(scope, "outcome_calibration", fit_id)?;
        if stored != fit {
            return Err(DecisionStoreError::Corrupt);
        }
        let outcome = self.get_observed_outcome(scope, &fit.outcome_id)?;
        screen_outcome_model_review(&fit, &fit_record_sha256, &outcome, criteria)
    }

    fn outcome_model_review_source_refs(
        &self,
        scope: &DecisionScope,
        outcome: &StoredObservedOutcome,
    ) -> Result<Vec<String>, DecisionStoreError> {
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &outcome.snapshot_id)?;
        let mut refs = snapshot.source_version_hashes;
        refs.push(outcome.observed_source_sha256.clone());
        refs.sort();
        refs.dedup();
        Ok(refs)
    }

    pub fn put_outcome_model_review_screen(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
        criteria: &OutcomeModelReviewCriteria,
    ) -> Result<StoredOutcomeModelReviewScreen, DecisionStoreError> {
        let report = self.screen_outcome_model_candidate(scope, fit_id, criteria)?;
        let outcome = self.get_observed_outcome(scope, &report.outcome_id)?;
        let record = StoredOutcomeModelReviewScreen {
            replay_hash: report.replay_hash.clone(),
            fit_id: fit_id.into(),
            fit_record_sha256: report.fit_record_sha256.clone(),
            source_version_hashes: self.outcome_model_review_source_refs(scope, &outcome)?,
            report,
        };
        self.put(
            scope,
            "outcome_model_review_screen",
            &record.replay_hash,
            &record,
            Some(&record.source_version_hashes),
        )?;
        Ok(record)
    }

    pub fn load_outcome_model_review_screen(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredOutcomeModelReviewScreen, DecisionStoreError> {
        let record: StoredOutcomeModelReviewScreen =
            self.get(scope, "outcome_model_review_screen", replay_hash)?;
        let fit = self.load_outcome_calibration(scope, &record.fit_id)?;
        let (stored_fit, fit_record_sha256): (StoredOutcomeCalibration, String) =
            self.get_with_digest(scope, "outcome_calibration", &record.fit_id)?;
        let outcome = self.get_observed_outcome(scope, &fit.outcome_id)?;
        if record.replay_hash != replay_hash
            || record.report.replay_hash != replay_hash
            || record.fit_id != record.report.fit_id
            || stored_fit != fit
            || record.fit_record_sha256 != fit_record_sha256
            || record.report.fit_record_sha256 != fit_record_sha256
            || record.report.outcome_id != outcome.id
            || record.report.replayed_run_hash != outcome.replay_hash
            || record.report.observed_source_sha256 != outcome.observed_source_sha256
            || record.report.parent_model_version != fit.parent_model_version
            || record.report.parent_model_sha256 != fit.parent_model_sha256
            || screen_hash(&record.report)? != replay_hash
            || self.outcome_model_review_source_refs(scope, &outcome)?
                != record.source_version_hashes
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if record.report.review_engine_sha256 == review_engine_sha256()
            && screen_outcome_model_review(
                &fit,
                &fit_record_sha256,
                &outcome,
                &record.report.criteria,
            )
            .map_err(|_| DecisionStoreError::Corrupt)?
                != record.report
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    fn outcome_model_review_matches_current(
        &self,
        scope: &DecisionScope,
        screen: &StoredOutcomeModelReviewScreen,
    ) -> Result<bool, DecisionStoreError> {
        if screen.report.review_engine_sha256 != review_engine_sha256() {
            return Ok(false);
        }
        Ok(
            self.screen_outcome_model_candidate(scope, &screen.fit_id, &screen.report.criteria)?
                == screen.report,
        )
    }

    pub async fn request_outcome_model_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        replay_hash: &str,
        agent_id: &str,
        summary: &str,
        ttl_seconds: i64,
    ) -> Result<OutcomeModelReviewLink, DecisionStoreError> {
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
        let screen = self.load_outcome_model_review_screen(scope, replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.outcome_model_review_matches_current(scope, &screen)?
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let (_, screen_record_sha256): (StoredOutcomeModelReviewScreen, String) =
            self.get_with_digest(scope, "outcome_model_review_screen", replay_hash)?;
        let payload = serde_json::json!({
            "tenant_id": scope.tenant_id, "acl": scope.acl,
            "replay_hash": replay_hash, "screen_record_sha256": screen_record_sha256,
            "fit_id": screen.fit_id, "fit_record_sha256": screen.fit_record_sha256,
        });
        let approval_id = broker
            .request(
                agent_id,
                "support_outcome_model_screen_review",
                summary,
                payload,
                ttl_seconds,
            )
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        let link = OutcomeModelReviewLink {
            approval_id: approval_id.to_string(),
            agent_id: agent_id.into(),
            replay_hash: replay_hash.into(),
            screen_record_sha256,
            fit_id: screen.fit_id,
            fit_record_sha256: screen.fit_record_sha256,
        };
        self.put(
            scope,
            "outcome_model_screen_review",
            &link.approval_id,
            &link,
            Some(&screen.source_version_hashes),
        )?;
        Ok(link)
    }

    pub async fn require_outcome_model_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
    ) -> Result<OutcomeModelReviewLink, DecisionStoreError> {
        if scope.tenant_id.trim().is_empty()
            || scope.acl.trim().is_empty()
            || approval_id.trim().is_empty()
            || replay_hash.trim().is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let link: OutcomeModelReviewLink =
            self.get(scope, "outcome_model_screen_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let screen = self.load_outcome_model_review_screen(scope, replay_hash)?;
        let (_, screen_record_sha256): (StoredOutcomeModelReviewScreen, String) =
            self.get_with_digest(scope, "outcome_model_review_screen", replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.outcome_model_review_matches_current(scope, &screen)?
            || link.screen_record_sha256 != screen_record_sha256
            || link.fit_id != screen.fit_id
            || link.fit_record_sha256 != screen.fit_record_sha256
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
            "tenant_id": scope.tenant_id, "acl": scope.acl,
            "replay_hash": replay_hash, "screen_record_sha256": screen_record_sha256,
            "fit_id": screen.fit_id, "fit_record_sha256": screen.fit_record_sha256,
        });
        if rec.id.as_str() != approval_id
            || rec.action_kind != "support_outcome_model_screen_review"
            || rec.agent_id != link.agent_id
            || rec.payload != expected_payload
            || rec.decided_by.as_deref().is_none_or(str::is_empty)
            || chrono::Utc::now() >= deadline
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        Ok(link)
    }

    /// Report a pending or terminal human-inspection receipt after checking
    /// the exact saved screen, fit, observation source, broker payload and TTL.
    pub async fn outcome_model_review_status(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
    ) -> Result<OutcomeModelReviewStatus, DecisionStoreError> {
        if !scope.valid() || approval_id.trim().is_empty() || replay_hash.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let link: OutcomeModelReviewLink =
            self.get(scope, "outcome_model_screen_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let screen = self.load_outcome_model_review_screen(scope, replay_hash)?;
        let (_, screen_record_sha256): (StoredOutcomeModelReviewScreen, String) =
            self.get_with_digest(scope, "outcome_model_review_screen", replay_hash)?;
        if !screen.report.eligible_for_human_review
            || !self.outcome_model_review_matches_current(scope, &screen)?
            || link.screen_record_sha256 != screen_record_sha256
            || link.fit_id != screen.fit_id
            || link.fit_record_sha256 != screen.fit_record_sha256
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
            "fit_id": screen.fit_id, "fit_record_sha256": screen.fit_record_sha256,
        });
        if rec.id.as_str() != approval_id
            || rec.action_kind != "support_outcome_model_screen_review"
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
            self.require_outcome_model_review(broker, scope, approval_id, replay_hash)
                .await?;
        }
        let final_screen = self.load_outcome_model_review_screen(scope, replay_hash)?;
        if final_screen != screen {
            return Err(DecisionStoreError::ReviewDenied);
        }
        Ok(OutcomeModelReviewStatus {
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
    use std::collections::VecDeque;
    use std::sync::Arc;

    use crate::approval::ApprovalStore;
    use crate::decision_calibration::ObservedSupportDay;
    use crate::decision_ingest::{
        DailyStaffing, SupportPilotExport, TicketEvent, derive_ticket_sla_labels,
    };
    use crate::decision_sim::{QueueModel, StaffingScenario};
    use crate::decision_store::ObservedOutcomeExport;
    use rusqlite::{Connection, params};

    #[tokio::test]
    async fn saved_model_screen_review_binds_fit_timing_and_active_sources() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let broker = ApprovalBroker::new(Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let start = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            - chrono::Duration::days(30);
        let snapshot = DecisionSnapshot {
            id: "model-review-snapshot".into(),
            queue_id: Some("support".into()),
            data_cutoff_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            source_version_hashes: vec!["synthetic-model-review-source".into()],
            seed: 1,
            arrivals_by_day: vec![5; 7],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "model-parent".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        let run = store
            .put_daily_run(&scope, &snapshot.id, &model.version, &scenario.id)
            .unwrap();
        // Simulate a run created before the seven observed days. Production
        // obtains this timestamp from the local SQLite journal.
        Connection::open(store.path())
            .unwrap()
            .execute(
                "UPDATE decision_inputs SET created_at=?1 WHERE tenant_id=?2 AND acl=?3
             AND kind='daily_run' AND input_id=?4",
                params![
                    start.timestamp() - 86_400,
                    scope.tenant_id,
                    scope.acl,
                    run.replay_hash
                ],
            )
            .unwrap();
        let observed_days = (0..7)
            .map(|day| ObservedSupportDay {
                arrivals: 5,
                backlog_start: day * 2,
                resolved: 3,
                backlog_end: (day + 1) * 2,
                agents: 1,
                fixed_extra_capacity: 0,
            })
            .collect();
        let export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support".into()),
            window_start_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_through_utc: (start + chrono::Duration::days(7))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_days,
        };
        let outcome = store
            .record_observed_outcome(
                &scope,
                "observed",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &run.replay_hash,
                "recorder",
                &serde_json::to_vec(&export).unwrap(),
            )
            .unwrap();
        assert_eq!(outcome.local_run_precedes_window, Some(true));
        store
            .put_outcome_calibration(&scope, "fit", &outcome.id, 3, 4)
            .unwrap();
        let criteria = OutcomeModelReviewCriteria {
            min_saturated_days: 3,
            min_holdout_days: 3,
        };
        let screen = store
            .put_outcome_model_review_screen(&scope, "fit", &criteria)
            .unwrap();
        assert!(
            screen.report.eligible_for_human_review,
            "{:?}",
            screen.report.failed_checks
        );
        assert_eq!(
            store
                .load_outcome_model_review_screen(&scope, &screen.replay_hash)
                .unwrap(),
            screen
        );
        let failed = store
            .put_outcome_model_review_screen(
                &scope,
                "fit",
                &OutcomeModelReviewCriteria {
                    min_saturated_days: 5,
                    min_holdout_days: 4,
                },
            )
            .unwrap();
        assert!(!failed.report.eligible_for_human_review);
        assert!(matches!(
            store.put_outcome_model_candidate(&scope, "failed-fit-candidate", &failed.replay_hash),
            Err(DecisionStoreError::ReviewDenied)
        ));
        assert!(matches!(
            store.put_outcome_model_candidate(&scope, &model.version, &screen.replay_hash),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            store
                .request_outcome_model_review(
                    &broker,
                    &scope,
                    &failed.replay_hash,
                    "agent",
                    "Inspect failed fit",
                    3_600,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        let link = store
            .request_outcome_model_review(
                &broker,
                &scope,
                &screen.replay_hash,
                "agent",
                "Inspect synthetic capacity fit",
                3_600,
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .require_outcome_model_review(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        broker
            .decide(
                &ApprovalId::from(link.approval_id.clone()),
                true,
                "human-reviewer",
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .require_outcome_model_review(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &screen.replay_hash,
                )
                .await
                .unwrap(),
            link
        );
        let candidate = store
            .put_outcome_model_candidate(&scope, "candidate-capacity-v2", &screen.replay_hash)
            .unwrap();
        assert_eq!(candidate.status, "simulation_only_model_candidate");
        assert_eq!(candidate.model.service_capacity_per_agent_day, 3);
        assert_eq!(
            store
                .load_outcome_model_candidate(&scope, &candidate.id)
                .unwrap(),
            candidate
        );
        assert!(matches!(
            store.get::<QueueModel>(&scope, "model", &candidate.id),
            Err(DecisionStoreError::NotFound)
        ));
        let mut target = snapshot.clone();
        target.id = "future-synthetic-window".into();
        target.data_cutoff_utc =
            (start + chrono::Duration::days(10)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        target.source_version_hashes = vec!["synthetic-future-source".into()];
        store.put_snapshot(&scope, &target).unwrap();
        let comparison = store
            .compare_outcome_model_candidate(&scope, &candidate.id, &target.id, &scenario.id)
            .unwrap();
        assert_eq!(
            comparison.status,
            "exploratory_model_assumption_sensitivity"
        );
        assert_eq!(comparison.parent.model_version, model.version);
        assert_eq!(comparison.candidate.model_version, candidate.id);
        assert!(comparison.final_backlog_delta < 0);
        let saved_comparison = store
            .put_outcome_model_candidate_run(&scope, &candidate.id, &target.id, &scenario.id)
            .unwrap();
        assert_eq!(saved_comparison.comparison, comparison);
        assert_eq!(
            saved_comparison.source_version_hashes.len(),
            saved_comparison
                .source_version_hashes
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
        assert_eq!(
            store
                .load_outcome_model_candidate_run(&scope, &saved_comparison.replay_hash,)
                .unwrap(),
            saved_comparison
        );
        assert_eq!(
            store
                .put_outcome_model_candidate_run(&scope, &candidate.id, &target.id, &scenario.id,)
                .unwrap(),
            saved_comparison
        );
        let saved_json = serde_json::to_string(&saved_comparison).unwrap();
        let mut changed_run: serde_json::Value = serde_json::from_str(&saved_json).unwrap();
        changed_run["comparison"]["final_backlog_delta"] = 0.into();
        let changed_run_json = serde_json::to_string(&changed_run).unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_model_candidate_run' AND input_id=?5",
            params![
                changed_run_json,
                format!("{:x}", Sha256::digest(changed_run_json.as_bytes())),
                scope.tenant_id,
                scope.acl,
                saved_comparison.replay_hash
            ],
        )
        .unwrap();
        assert!(matches!(
            store.load_outcome_model_candidate_run(&scope, &saved_comparison.replay_hash,),
            Err(DecisionStoreError::Corrupt)
        ));
        let mut extra_field_run: serde_json::Value = serde_json::from_str(&saved_json).unwrap();
        extra_field_run["comparison"]["unrecognized_note"] = "modified".into();
        let extra_field_json = serde_json::to_string(&extra_field_run).unwrap();
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_model_candidate_run' AND input_id=?5",
            params![
                extra_field_json,
                format!("{:x}", Sha256::digest(extra_field_json.as_bytes())),
                scope.tenant_id,
                scope.acl,
                saved_comparison.replay_hash
            ],
        )
        .unwrap();
        assert!(matches!(
            store.load_outcome_model_candidate_run(&scope, &saved_comparison.replay_hash,),
            Err(DecisionStoreError::Corrupt)
        ));
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_model_candidate_run' AND input_id=?5",
            params![
                saved_json,
                format!("{:x}", Sha256::digest(saved_json.as_bytes())),
                scope.tenant_id,
                scope.acl,
                saved_comparison.replay_hash
            ],
        )
        .unwrap();
        let target_parent_run = store
            .put_daily_run(&scope, &target.id, &model.version, &scenario.id)
            .unwrap();
        assert_eq!(
            target_parent_run.replay_hash,
            saved_comparison.comparison.parent.replay_hash
        );
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='daily_run' AND input_id=?4",
            params![
                start.timestamp() + 9 * 86_400,
                scope.tenant_id,
                scope.acl,
                target_parent_run.replay_hash
            ],
        )
        .unwrap();
        let target_start = chrono::DateTime::parse_from_rfc3339(&target.data_cutoff_utc).unwrap();
        let mut tickets = Vec::<TicketEvent>::new();
        let mut staffing = Vec::<DailyStaffing>::new();
        let mut pending = VecDeque::<usize>::new();
        for day in 0..7 {
            let date = target_start + chrono::Duration::days(day);
            staffing.push(DailyStaffing {
                queue_id: Some("support".into()),
                day_utc: date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                agents: 1,
                fixed_extra_capacity: 0,
            });
            for ticket in 0..5 {
                pending.push_back(tickets.len());
                tickets.push(TicketEvent {
                    queue_id: Some("support".into()),
                    ticket_id: format!("target-{day}-{ticket}"),
                    created_at_utc: date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    resolved_at_utc: None,
                });
            }
            for _ in 0..3 {
                let index = pending.pop_front().unwrap();
                tickets[index].resolved_at_utc = Some(
                    (date + chrono::Duration::hours(12))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );
            }
        }
        let ticket_rows_digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&tickets, &staffing)).unwrap())
        );
        let ticket_export = SupportPilotExport {
            snapshot_id: target.id.clone(),
            baseline_scenario_id: scenario.id.clone(),
            window_start_utc: target.data_cutoff_utc.clone(),
            data_cutoff_utc: (start + chrono::Duration::days(17))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            source_version_hashes: vec![ticket_rows_digest],
            seed: 1,
            horizon_days: 7,
            tickets,
            staffing,
        };
        let ticket_source_bytes = serde_json::to_vec(&ticket_export).unwrap();
        let ticket_labels = derive_ticket_sla_labels(&ticket_export, model.sla_days).unwrap();
        let ticket_retention = (chrono::Utc::now() + chrono::Duration::days(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let target_export = ObservedOutcomeExport {
            sla_days: Some(model.sla_days),
            resolved_within_sla_by_day: Some(ticket_labels),
            queue_id: Some("support".into()),
            window_start_utc: target.data_cutoff_utc.clone(),
            observed_through_utc: (start + chrono::Duration::days(17))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_days: (0..7)
                .map(|day| ObservedSupportDay {
                    arrivals: 5,
                    backlog_start: day * 2,
                    resolved: 3,
                    backlog_end: (day + 1) * 2,
                    agents: 1,
                    fixed_extra_capacity: 0,
                })
                .collect(),
        };
        assert!(matches!(
            store.record_observed_outcome_with_ticket_source(
                &scope,
                "invalid-retention",
                &target.id,
                &model.version,
                &scenario.id,
                &target_parent_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&target_export).unwrap(),
                &ticket_source_bytes,
                &target_export.observed_through_utc,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let fractional_retention = format!(
            "{}.500Z",
            (chrono::Utc::now() + chrono::Duration::days(30)).format("%Y-%m-%dT%H:%M:%S")
        );
        assert!(matches!(
            store.record_observed_outcome_with_ticket_source(
                &scope,
                "fractional-retention",
                &target.id,
                &model.version,
                &scenario.id,
                &target_parent_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&target_export).unwrap(),
                &ticket_source_bytes,
                &fractional_retention,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut invalid_sla_export = target_export.clone();
        invalid_sla_export.resolved_within_sla_by_day = Some(vec![4; 7]);
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "invalid-later-observed",
                &target.id,
                &model.version,
                &scenario.id,
                &target_parent_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&invalid_sla_export).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        invalid_sla_export.resolved_within_sla_by_day =
            target_export.resolved_within_sla_by_day.clone();
        invalid_sla_export.sla_days = Some(model.sla_days + 1);
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "wrong-sla-definition",
                &target.id,
                &model.version,
                &scenario.id,
                &target_parent_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&invalid_sla_export).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut changed_ticket_export = ticket_export;
        changed_ticket_export.tickets[0].ticket_id = "changed".into();
        assert!(matches!(
            store.record_observed_outcome_with_ticket_source(
                &scope,
                "changed-ticket-source",
                &target.id,
                &model.version,
                &scenario.id,
                &target_parent_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&target_export).unwrap(),
                &serde_json::to_vec(&changed_ticket_export).unwrap(),
                &ticket_retention,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let target_outcome = store
            .record_observed_outcome_with_ticket_source(
                &scope,
                "later-observed",
                &target.id,
                &model.version,
                &scenario.id,
                &target_parent_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&target_export).unwrap(),
                &ticket_source_bytes,
                &ticket_retention,
            )
            .unwrap();
        changed_ticket_export.source_version_hashes = vec![format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    &changed_ticket_export.tickets,
                    &changed_ticket_export.staffing
                ))
                .unwrap()
            )
        )];
        let alternate_ticket_bytes = serde_json::to_vec(&changed_ticket_export).unwrap();
        assert!(matches!(
            store.record_observed_outcome_with_ticket_source(
                &scope,
                "later-observed",
                &target.id,
                &model.version,
                &scenario.id,
                &target_parent_run.replay_hash,
                "recorder",
                &serde_json::to_vec(&target_export).unwrap(),
                &alternate_ticket_bytes,
                &ticket_retention,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        let alternate_digest = format!("{:x}", Sha256::digest(&alternate_ticket_bytes));
        let alternate_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM decision_ticket_source_blobs
             WHERE tenant_id=?1 AND acl=?2 AND source_sha256=?3",
                params![scope.tenant_id, scope.acl, alternate_digest],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alternate_rows, 0);
        assert_eq!(
            target_outcome.ticket_source_sha256.as_deref(),
            Some(format!("{:x}", Sha256::digest(&ticket_source_bytes)).as_str())
        );
        assert!(matches!(
            store.put_outcome_model_candidate_score(
                &scope,
                &saved_comparison.replay_hash,
                &target_outcome.id,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='outcome_model_candidate_run' AND input_id=?4",
            params![
                start.timestamp() + 9 * 86_400,
                scope.tenant_id,
                scope.acl,
                saved_comparison.replay_hash
            ],
        )
        .unwrap();
        assert!(matches!(
            store.put_outcome_model_candidate_score(
                &scope,
                &saved_comparison.replay_hash,
                &target_outcome.id,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='outcome_model_candidate' AND input_id=?4",
            params![
                start.timestamp() + 8 * 86_400,
                scope.tenant_id,
                scope.acl,
                candidate.id
            ],
        )
        .unwrap();
        let score = store
            .put_outcome_model_candidate_score(
                &scope,
                &saved_comparison.replay_hash,
                &target_outcome.id,
            )
            .unwrap();
        assert_eq!(
            score.status,
            "exploratory_locally_timed_forecast_diagnostic"
        );
        assert!(score.candidate_backlog_error_below_parent);
        assert!(score.candidate_resolution_error_below_parent);
        assert_eq!(score.candidate.resolved_within_sla_abs_error_sum, Some(0));
        assert_eq!(score.candidate_sla_error_below_parent, Some(true));
        assert_eq!(
            score.ticket_source_sha256,
            target_outcome.ticket_source_sha256
        );
        assert_eq!(score.candidate.backlog_abs_error_sum, 0);
        assert_eq!(
            store
                .load_outcome_model_candidate_score(&scope, &score.replay_hash,)
                .unwrap(),
            score
        );
        conn.execute(
            "UPDATE decision_ticket_source_blobs SET source_bytes=?1
             WHERE tenant_id=?2 AND acl=?3 AND source_sha256=?4",
            params![
                b"{}".as_slice(),
                scope.tenant_id,
                scope.acl,
                target_outcome.ticket_source_sha256
            ],
        )
        .unwrap();
        assert!(matches!(
            store.get_observed_outcome(&scope, &target_outcome.id),
            Err(DecisionStoreError::Corrupt)
        ));
        conn.execute(
            "UPDATE decision_ticket_source_blobs SET source_bytes=?1
             WHERE tenant_id=?2 AND acl=?3 AND source_sha256=?4",
            params![
                ticket_source_bytes,
                scope.tenant_id,
                scope.acl,
                target_outcome.ticket_source_sha256
            ],
        )
        .unwrap();
        assert!(
            store
                .get_observed_outcome(&scope, &target_outcome.id)
                .is_ok()
        );
        let score_json = serde_json::to_string(&score).unwrap();
        let mut changed_score: serde_json::Value = serde_json::from_str(&score_json).unwrap();
        changed_score["candidate"]["backlog_abs_error_sum"] = 999.into();
        let changed_score_json = serde_json::to_string(&changed_score).unwrap();
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_model_candidate_score' AND input_id=?5",
            params![changed_score_json, format!("{:x}", Sha256::digest(changed_score_json.as_bytes())),
                scope.tenant_id, scope.acl, score.replay_hash],
        ).unwrap();
        assert!(matches!(
            store.load_outcome_model_candidate_score(&scope, &score.replay_hash,),
            Err(DecisionStoreError::Corrupt)
        ));
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_model_candidate_score' AND input_id=?5",
            params![score_json, format!("{:x}", Sha256::digest(score_json.as_bytes())),
                scope.tenant_id, scope.acl, score.replay_hash],
        ).unwrap();
        assert!(matches!(
            store.compare_outcome_model_candidate(
                &scope,
                &candidate.id,
                &snapshot.id,
                &scenario.id
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut wrong_queue = target.clone();
        wrong_queue.id = "other-support-queue".into();
        wrong_queue.queue_id = Some("different-queue".into());
        store.put_snapshot(&scope, &wrong_queue).unwrap();
        assert!(matches!(
            store.compare_outcome_model_candidate(
                &scope,
                &candidate.id,
                &wrong_queue.id,
                &scenario.id
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let original_json = serde_json::to_string(&candidate).unwrap();
        let mut changed: serde_json::Value = serde_json::from_str(&original_json).unwrap();
        changed["model"]["service_capacity_per_agent_day"] = 99.into();
        let changed_json = serde_json::to_string(&changed).unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_model_candidate' AND input_id=?5",
            params![
                changed_json,
                format!("{:x}", Sha256::digest(changed_json.as_bytes())),
                scope.tenant_id,
                scope.acl,
                candidate.id
            ],
        )
        .unwrap();
        assert!(matches!(
            store.load_outcome_model_candidate(&scope, &candidate.id),
            Err(DecisionStoreError::Corrupt)
        ));
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_model_candidate' AND input_id=?5",
            params![
                original_json,
                format!("{:x}", Sha256::digest(original_json.as_bytes())),
                scope.tenant_id,
                scope.acl,
                candidate.id
            ],
        )
        .unwrap();
        assert!(matches!(
            store
                .require_outcome_model_review(&broker, &scope, &link.approval_id, "wrong-screen",)
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        conn.execute(
            "UPDATE decision_ticket_source_blobs SET retention_until=0
             WHERE tenant_id=?1 AND acl=?2 AND source_sha256=?3",
            params![
                scope.tenant_id,
                scope.acl,
                target_outcome.ticket_source_sha256
            ],
        )
        .unwrap();
        assert_eq!(store.scrub_expired_ticket_sources(&scope).unwrap(), 1);
        assert_eq!(store.scrub_expired_ticket_sources(&scope).unwrap(), 0);
        let (blob_bytes, invalidated): (Vec<u8>, Option<i64>) = conn
            .query_row(
                "SELECT source_bytes,invalidated_at FROM decision_ticket_source_blobs
             WHERE tenant_id=?1 AND acl=?2 AND source_sha256=?3",
                params![
                    scope.tenant_id,
                    scope.acl,
                    target_outcome.ticket_source_sha256
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(blob_bytes.is_empty() && invalidated.is_some());
        let (score_payload, score_invalidated): (String, Option<i64>) = conn
            .query_row(
                "SELECT payload_json,invalidated_at FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND kind='outcome_model_candidate_score'
             AND input_id=?3",
                params![scope.tenant_id, scope.acl, score.replay_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(score_payload.is_empty() && score_invalidated.is_some());
        assert!(matches!(
            store.get_observed_outcome(&scope, &target_outcome.id),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.load_outcome_model_candidate_score(&scope, &score.replay_hash),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(
            store
                .load_outcome_model_candidate_run(&scope, &saved_comparison.replay_hash,)
                .is_ok()
        );
        store
            .revoke_source_version(&scope, &outcome.observed_source_sha256)
            .unwrap();
        assert!(matches!(
            store.load_outcome_model_review_screen(&scope, &screen.replay_hash,),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.load_outcome_model_candidate(&scope, &candidate.id),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.load_outcome_model_candidate_run(&scope, &saved_comparison.replay_hash),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.load_outcome_model_candidate_score(&scope, &score.replay_hash),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store
                .require_outcome_model_review(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::Revoked)
        ));
    }
}
