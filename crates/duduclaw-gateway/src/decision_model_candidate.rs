//! Source-bound capacity-model proposals for simulation only.
//!
//! A reviewed fit can inform a later snapshot, but must not rewrite the
//! historical model or silently become an operational staffing policy.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_calibration::ObservedSupportDay;
use crate::decision_model_review::StoredOutcomeModelReviewScreen;
use crate::decision_sim::{
    DecisionSnapshot, QueueModel, SimulationResult, StaffingScenario, simulate,
};
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, StoredObservedOutcome,
    StoredOutcomeCalibration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredOutcomeModelCandidate {
    pub id: String,
    pub status: String,
    pub candidate_engine_sha256: String,
    pub screen_replay_hash: String,
    pub screen_record_sha256: String,
    pub fit_id: String,
    pub fit_record_sha256: String,
    pub outcome_id: String,
    pub parent_model_version: String,
    pub parent_model_sha256: String,
    pub queue_id: String,
    pub observed_through_utc: String,
    pub model: QueueModel,
    pub source_version_hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeModelCandidateComparison {
    pub status: String,
    pub candidate_id: String,
    pub target_snapshot_id: String,
    pub scenario_id: String,
    pub parent: SimulationResult,
    pub candidate: SimulationResult,
    /// Candidate minus parent, under identical snapshot and staffing inputs.
    pub final_backlog_delta: i128,
    pub resolved_within_sla_delta: i128,
    pub limitations: Vec<String>,
}

/// Immutable manifest for a comparison; every input and the output are checked on load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredOutcomeModelCandidateRun {
    pub replay_hash: String,
    pub comparison_engine_sha256: String,
    pub candidate_id: String,
    pub candidate_record_sha256: String,
    pub target_snapshot_id: String,
    pub target_snapshot_sha256: String,
    pub scenario_id: String,
    pub scenario_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub comparison: OutcomeModelCandidateComparison,
}

/// Error numerators over the same fully observed days; divide by `days` for MAE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateForecastErrors {
    pub days: usize,
    pub arrivals_abs_error_sum: u128,
    pub resolved_abs_error_sum: u128,
    pub backlog_abs_error_sum: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_within_sla_abs_error_sum: Option<u128>,
}

/// Prospective local-timing diagnostic, never an effect or promotion decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredOutcomeModelCandidateScore {
    pub replay_hash: String,
    pub status: String,
    pub scoring_engine_sha256: String,
    pub comparison_run_hash: String,
    pub comparison_run_sha256: String,
    pub outcome_id: String,
    pub outcome_record_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_label_engine_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_source_retention_until_utc: Option<String>,
    pub candidate_created_at_unix: i64,
    pub comparison_created_at_unix: i64,
    pub observed_window_start_utc: String,
    pub source_version_hashes: Vec<String>,
    pub parent: CandidateForecastErrors,
    pub candidate: CandidateForecastErrors,
    pub candidate_backlog_error_below_parent: bool,
    pub candidate_resolution_error_below_parent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_sla_error_below_parent: Option<bool>,
    pub limitations: Vec<String>,
}

fn candidate_engine_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_str!("decision_model_candidate.rs").as_bytes())
    )
}

fn comparison_replay_hash(
    run: &StoredOutcomeModelCandidateRun,
) -> Result<String, DecisionStoreError> {
    let mut content = run.clone();
    content.replay_hash.clear();
    let bytes = serde_json::to_vec(&("outcome_model_candidate_run_v1", content))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn score_replay_hash(
    score: &StoredOutcomeModelCandidateScore,
) -> Result<String, DecisionStoreError> {
    let mut content = score.clone();
    content.replay_hash.clear();
    let bytes = serde_json::to_vec(&("outcome_model_candidate_score_v1", content))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn forecast_errors(
    predicted: &SimulationResult,
    observed: &[ObservedSupportDay],
    sla_labels: Option<&[u32]>,
) -> Result<CandidateForecastErrors, DecisionStoreError> {
    if observed.is_empty()
        || predicted.days.len() != observed.len()
        || sla_labels.is_some_and(|labels| {
            labels.len() != observed.len()
                || labels
                    .iter()
                    .zip(observed)
                    .any(|(label, day)| *label > day.resolved)
        })
    {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(CandidateForecastErrors {
        days: observed.len(),
        arrivals_abs_error_sum: predicted
            .days
            .iter()
            .zip(observed)
            .map(|(p, o)| u128::from(p.arrivals.abs_diff(o.arrivals)))
            .sum(),
        resolved_abs_error_sum: predicted
            .days
            .iter()
            .zip(observed)
            .map(|(p, o)| u128::from(p.resolved.abs_diff(o.resolved)))
            .sum(),
        backlog_abs_error_sum: predicted
            .days
            .iter()
            .zip(observed)
            .map(|(p, o)| u128::from(p.backlog_end.abs_diff(o.backlog_end)))
            .sum(),
        resolved_within_sla_abs_error_sum: sla_labels.map(|labels| {
            predicted
                .days
                .iter()
                .zip(labels)
                .map(|(day, actual)| u128::from(day.resolved_within_sla.abs_diff(*actual)))
                .sum()
        }),
    })
}

fn utc_time(value: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, DecisionStoreError> {
    let parsed =
        chrono::DateTime::parse_from_rfc3339(value).map_err(|_| DecisionStoreError::Invalid)?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(parsed)
}

impl DecisionStore {
    fn expected_outcome_model_candidate(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
        screen_hash: &str,
    ) -> Result<StoredOutcomeModelCandidate, DecisionStoreError> {
        if candidate_id.is_empty()
            || candidate_id.trim() != candidate_id
            || candidate_id.len() > 128
            || screen_hash.is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        match self.get::<QueueModel>(scope, "model", candidate_id) {
            Ok(_) => return Err(DecisionStoreError::Invalid),
            Err(DecisionStoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let screen: StoredOutcomeModelReviewScreen =
            self.load_outcome_model_review_screen(scope, screen_hash)?;
        if !screen.report.eligible_for_human_review
            || self.screen_outcome_model_candidate(
                scope,
                &screen.fit_id,
                &screen.report.criteria,
            )? != screen.report
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let (_, screen_record_sha256): (StoredOutcomeModelReviewScreen, String) =
            self.get_with_digest(scope, "outcome_model_review_screen", screen_hash)?;
        let fit: StoredOutcomeCalibration = self.load_outcome_calibration(scope, &screen.fit_id)?;
        let (_, fit_record_sha256): (StoredOutcomeCalibration, String) =
            self.get_with_digest(scope, "outcome_calibration", &screen.fit_id)?;
        let outcome: StoredObservedOutcome = self.get_observed_outcome(scope, &fit.outcome_id)?;
        let (parent, parent_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", &fit.parent_model_version)?;
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &outcome.snapshot_id)?;
        let queue_id = outcome
            .queue_id
            .clone()
            .ok_or(DecisionStoreError::Invalid)?;
        if queue_id.is_empty()
            || candidate_id == parent.version
            || snapshot.queue_id.as_deref() != Some(queue_id.as_str())
            || parent_sha256 != fit.parent_model_sha256
            || screen.fit_record_sha256 != fit_record_sha256
            || utc_time(&outcome.observed_through_utc)? <= utc_time(&outcome.window_start_utc)?
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let mut model = parent.clone();
        model.version = candidate_id.to_owned();
        model.service_capacity_per_agent_day = fit.fit.service_per_agent_day;
        let mut sources = screen.source_version_hashes;
        sources.sort();
        sources.dedup();
        Ok(StoredOutcomeModelCandidate {
            id: candidate_id.to_owned(),
            status: "simulation_only_model_candidate".into(),
            candidate_engine_sha256: candidate_engine_sha256(),
            screen_replay_hash: screen_hash.to_owned(),
            screen_record_sha256,
            fit_id: fit.id,
            fit_record_sha256,
            outcome_id: outcome.id,
            parent_model_version: parent.version,
            parent_model_sha256: parent_sha256,
            queue_id,
            observed_through_utc: outcome.observed_through_utc,
            model,
            source_version_hashes: sources,
        })
    }

    /// Save a proposal in a separate namespace; `put_model` is never called.
    pub fn put_outcome_model_candidate(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
        screen_hash: &str,
    ) -> Result<StoredOutcomeModelCandidate, DecisionStoreError> {
        let candidate = self.expected_outcome_model_candidate(scope, candidate_id, screen_hash)?;
        self.put(
            scope,
            "outcome_model_candidate",
            candidate_id,
            &candidate,
            Some(&candidate.source_version_hashes),
        )?;
        Ok(candidate)
    }

    pub fn load_outcome_model_candidate(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
    ) -> Result<StoredOutcomeModelCandidate, DecisionStoreError> {
        let candidate: StoredOutcomeModelCandidate =
            self.get(scope, "outcome_model_candidate", candidate_id)?;
        if candidate.candidate_engine_sha256 != candidate_engine_sha256() {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let expected = self.expected_outcome_model_candidate(
            scope,
            candidate_id,
            &candidate.screen_replay_hash,
        )?;
        if candidate != expected {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(candidate)
    }

    /// Apply parent and candidate capacity to the same later demand snapshot.
    /// This compares model assumptions, not staffing interventions or effects.
    pub fn compare_outcome_model_candidate(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
        target_snapshot_id: &str,
        scenario_id: &str,
    ) -> Result<OutcomeModelCandidateComparison, DecisionStoreError> {
        let candidate = self.load_outcome_model_candidate(scope, candidate_id)?;
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", target_snapshot_id)?;
        let scenario: StaffingScenario = self.get(scope, "scenario", scenario_id)?;
        let parent: QueueModel = self.get(scope, "model", &candidate.parent_model_version)?;
        if snapshot.queue_id.as_deref() != Some(candidate.queue_id.as_str())
            || utc_time(&snapshot.data_cutoff_utc)? < utc_time(&candidate.observed_through_utc)?
        {
            return Err(DecisionStoreError::Invalid);
        }
        let parent_result = simulate(&snapshot, &parent, &scenario)?;
        let candidate_result = simulate(&snapshot, &candidate.model, &scenario)?;
        Ok(OutcomeModelCandidateComparison {
            status: "exploratory_model_assumption_sensitivity".into(),
            candidate_id: candidate.id,
            target_snapshot_id: snapshot.id,
            scenario_id: scenario.id,
            final_backlog_delta: i128::from(candidate_result.final_backlog)
                - i128::from(parent_result.final_backlog),
            resolved_within_sla_delta: i128::from(candidate_result.total_resolved_within_sla)
                - i128::from(parent_result.total_resolved_within_sla),
            parent: parent_result,
            candidate: candidate_result,
            limitations: vec![
                "Both arms use the same supplied arrivals, initial backlog, and staffing; this is model sensitivity, not an intervention effect".into(),
                "A locally timestamped, source-digested fit does not authenticate the upstream observation producer".into(),
                "The candidate is not activated as a model or authorized for staffing action".into(),
            ],
        })
    }

    fn expected_outcome_model_candidate_run(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
        target_snapshot_id: &str,
        scenario_id: &str,
    ) -> Result<StoredOutcomeModelCandidateRun, DecisionStoreError> {
        let candidate = self.load_outcome_model_candidate(scope, candidate_id)?;
        let (_, candidate_record_sha256): (StoredOutcomeModelCandidate, String) =
            self.get_with_digest(scope, "outcome_model_candidate", candidate_id)?;
        let (snapshot, target_snapshot_sha256): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", target_snapshot_id)?;
        let (_, scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", scenario_id)?;
        let comparison = self.compare_outcome_model_candidate(
            scope,
            candidate_id,
            target_snapshot_id,
            scenario_id,
        )?;
        let mut source_version_hashes = candidate.source_version_hashes;
        source_version_hashes.extend(snapshot.source_version_hashes);
        source_version_hashes.sort();
        source_version_hashes.dedup();
        let mut run = StoredOutcomeModelCandidateRun {
            replay_hash: String::new(),
            comparison_engine_sha256: candidate_engine_sha256(),
            candidate_id: candidate_id.to_owned(),
            candidate_record_sha256,
            target_snapshot_id: target_snapshot_id.to_owned(),
            target_snapshot_sha256,
            scenario_id: scenario_id.to_owned(),
            scenario_sha256,
            source_version_hashes,
            comparison,
        };
        run.replay_hash = comparison_replay_hash(&run)?;
        Ok(run)
    }

    pub fn put_outcome_model_candidate_run(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
        target_snapshot_id: &str,
        scenario_id: &str,
    ) -> Result<StoredOutcomeModelCandidateRun, DecisionStoreError> {
        let run = self.expected_outcome_model_candidate_run(
            scope,
            candidate_id,
            target_snapshot_id,
            scenario_id,
        )?;
        self.put(
            scope,
            "outcome_model_candidate_run",
            &run.replay_hash,
            &run,
            Some(&run.source_version_hashes),
        )?;
        Ok(run)
    }

    pub fn load_outcome_model_candidate_run(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredOutcomeModelCandidateRun, DecisionStoreError> {
        let (run, payload_sha256): (StoredOutcomeModelCandidateRun, String) =
            self.get_with_digest(scope, "outcome_model_candidate_run", replay_hash)?;
        if format!("{:x}", Sha256::digest(serde_json::to_vec(&run)?)) != payload_sha256
            || run.replay_hash != replay_hash
            || comparison_replay_hash(&run)? != replay_hash
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if run.comparison_engine_sha256 != candidate_engine_sha256() {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let expected = self.expected_outcome_model_candidate_run(
            scope,
            &run.candidate_id,
            &run.target_snapshot_id,
            &run.scenario_id,
        )?;
        if run != expected {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(run)
    }

    fn expected_outcome_model_candidate_score(
        &self,
        scope: &DecisionScope,
        comparison_run_hash: &str,
        outcome_id: &str,
    ) -> Result<StoredOutcomeModelCandidateScore, DecisionStoreError> {
        let run = self.load_outcome_model_candidate_run(scope, comparison_run_hash)?;
        let (_, comparison_run_sha256): (StoredOutcomeModelCandidateRun, String) =
            self.get_with_digest(scope, "outcome_model_candidate_run", comparison_run_hash)?;
        let outcome = self.get_observed_outcome(scope, outcome_id)?;
        let (_, outcome_record_sha256): (StoredObservedOutcome, String) =
            self.get_with_digest(scope, "observed_outcome", outcome_id)?;
        let comparison_created_at_unix = self.decision_input_created_at(
            scope,
            "outcome_model_candidate_run",
            comparison_run_hash,
        )?;
        let candidate_created_at_unix =
            self.decision_input_created_at(scope, "outcome_model_candidate", &run.candidate_id)?;
        let candidate = self.load_outcome_model_candidate(scope, &run.candidate_id)?;
        let window_start = utc_time(&outcome.window_start_utc)?;
        if run.target_snapshot_id != outcome.snapshot_id
            || run.scenario_id != outcome.scenario_id
            || run.comparison.parent.model_version != outcome.model_version
            || run.comparison.parent.replay_hash != outcome.replay_hash
            || outcome.local_run_precedes_window != Some(true)
            || candidate_created_at_unix <= 0
            || candidate_created_at_unix < utc_time(&candidate.observed_through_utc)?.timestamp()
            || candidate_created_at_unix > comparison_created_at_unix
            || comparison_created_at_unix <= 0
            || comparison_created_at_unix >= window_start.timestamp()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let sla_labels = outcome.resolved_within_sla_by_day.as_deref();
        let parent = forecast_errors(&run.comparison.parent, &outcome.observed_days, sla_labels)?;
        let candidate = forecast_errors(
            &run.comparison.candidate,
            &outcome.observed_days,
            sla_labels,
        )?;
        if parent.arrivals_abs_error_sum != candidate.arrivals_abs_error_sum {
            return Err(DecisionStoreError::Corrupt);
        }
        let mut source_version_hashes = run.source_version_hashes;
        source_version_hashes.push(outcome.observed_source_sha256);
        if let Some(ticket_source_sha256) = outcome.ticket_source_sha256.as_ref() {
            source_version_hashes.push(ticket_source_sha256.clone());
        }
        source_version_hashes.sort();
        source_version_hashes.dedup();
        let mut score = StoredOutcomeModelCandidateScore {
            replay_hash: String::new(),
            status: "exploratory_locally_timed_forecast_diagnostic".into(),
            scoring_engine_sha256: candidate_engine_sha256(),
            comparison_run_hash: comparison_run_hash.to_owned(),
            comparison_run_sha256,
            outcome_id: outcome_id.to_owned(),
            outcome_record_sha256,
            ticket_source_sha256: outcome.ticket_source_sha256.clone(),
            ticket_label_engine_sha256: outcome.ticket_label_engine_sha256.clone(),
            ticket_source_retention_until_utc:
                outcome.ticket_source_retention_until_utc.clone(),
            candidate_created_at_unix,
            comparison_created_at_unix,
            observed_window_start_utc: outcome.window_start_utc,
            source_version_hashes,
            candidate_backlog_error_below_parent:
                candidate.backlog_abs_error_sum < parent.backlog_abs_error_sum,
            candidate_resolution_error_below_parent:
                candidate.resolved_abs_error_sum < parent.resolved_abs_error_sum,
            candidate_sla_error_below_parent: match (
                candidate.resolved_within_sla_abs_error_sum,
                parent.resolved_within_sla_abs_error_sum,
            ) {
                (Some(candidate_error), Some(parent_error)) => Some(candidate_error < parent_error),
                _ => None,
            },
            parent,
            candidate,
            limitations: vec![
                "Local SQLite timestamps do not attest an external forecast commitment or source producer".into(),
                if outcome.ticket_source_sha256.is_some() {
                    "SLA counts were recomputed from one supplied ticket export at ingest; upstream source identity is not authenticated"
                } else if sla_labels.is_some() {
                    "SLA counts are aggregate labels supplied by the observation producer; ticket-level timestamps are not independently verified"
                } else {
                    "No resolved-within-SLA labels were supplied; SLA accuracy is not scored"
                }.into(),
                "This scores model forecasts under the recorded staffing path, not a staffing intervention effect".into(),
                "No model is activated and no operational action is authorized".into(),
            ],
        };
        score.replay_hash = score_replay_hash(&score)?;
        Ok(score)
    }

    pub fn put_outcome_model_candidate_score(
        &self,
        scope: &DecisionScope,
        comparison_run_hash: &str,
        outcome_id: &str,
    ) -> Result<StoredOutcomeModelCandidateScore, DecisionStoreError> {
        let score =
            self.expected_outcome_model_candidate_score(scope, comparison_run_hash, outcome_id)?;
        self.put(
            scope,
            "outcome_model_candidate_score",
            &score.replay_hash,
            &score,
            Some(&score.source_version_hashes),
        )?;
        Ok(score)
    }

    pub fn load_outcome_model_candidate_score(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredOutcomeModelCandidateScore, DecisionStoreError> {
        let (score, payload_sha256): (StoredOutcomeModelCandidateScore, String) =
            self.get_with_digest(scope, "outcome_model_candidate_score", replay_hash)?;
        if format!("{:x}", Sha256::digest(serde_json::to_vec(&score)?)) != payload_sha256
            || score.replay_hash != replay_hash
            || score_replay_hash(&score)? != replay_hash
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if score.scoring_engine_sha256 != candidate_engine_sha256() {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let expected = self.expected_outcome_model_candidate_score(
            scope,
            &score.comparison_run_hash,
            &score.outcome_id,
        )?;
        if score != expected {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(score)
    }
}
