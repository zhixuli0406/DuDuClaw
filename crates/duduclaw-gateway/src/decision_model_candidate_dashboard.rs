//! Bounded, source-checked Dashboard projections of immutable model candidates.

use serde::Serialize;

use crate::decision_model_candidate::{
    CandidateForecastErrors, StoredOutcomeModelCandidate, StoredOutcomeModelCandidateRun,
    StoredOutcomeModelCandidateScore,
};
use crate::decision_sim::{QueueModel, SimulationResult, engine_code_sha256};
use crate::decision_store::{DecisionScope, DecisionStore, DecisionStoreError};

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

#[derive(Debug, Clone, Serialize)]
pub struct DashboardModelCandidate {
    pub id: String,
    pub status: String,
    pub queue_id: String,
    pub parent_model_version: String,
    pub model: QueueModel,
    pub observed_through_utc: String,
    pub screen_replay_hash: String,
    pub fit_id: String,
    pub outcome_id: String,
    pub source_version_hashes: Vec<String>,
    pub source_version_hashes_total: usize,
    pub record_sha256: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardCandidateKpis {
    pub model_version: String,
    pub replay_hash: String,
    /// Decimal strings: the dashboard compares these with `BigInt` and
    /// re-derives the deltas below, which a JSON number would silently round.
    pub total_arrivals: String,
    pub total_resolved: String,
    pub final_backlog: String,
    pub total_resolved_within_sla: String,
    pub total_staff_cost_cents: String,
    /// Whether the stored result's engine digest is the installed simulator's.
    /// Derived from the recorded digest, never assumed: `DecisionStore::
    /// load_outcome_model_candidate_run` already refuses a manifest it cannot
    /// reproduce with the current engine, so on that path every returned run
    /// reads `true`. Reading it from the payload keeps the display from
    /// asserting a currency it never checked if that load path is relaxed.
    pub engine_matches_current: bool,
}

impl From<&SimulationResult> for DashboardCandidateKpis {
    fn from(result: &SimulationResult) -> Self {
        Self {
            model_version: result.model_version.clone(),
            replay_hash: result.replay_hash.clone(),
            engine_matches_current: result.engine_sha256 == engine_code_sha256(),
            total_arrivals: result.total_arrivals.to_string(),
            total_resolved: result.total_resolved.to_string(),
            final_backlog: result.final_backlog.to_string(),
            total_resolved_within_sla: result.total_resolved_within_sla.to_string(),
            total_staff_cost_cents: result.total_staff_cost_cents.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardModelCandidateRun {
    pub replay_hash: String,
    pub status: String,
    pub candidate_id: String,
    pub target_snapshot_id: String,
    pub scenario_id: String,
    pub record_sha256: String,
    pub candidate_record_sha256: String,
    pub target_snapshot_sha256: String,
    pub scenario_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub source_version_hashes_total: usize,
    pub parent: DashboardCandidateKpis,
    pub candidate: DashboardCandidateKpis,
    pub final_backlog_delta: String,
    pub resolved_within_sla_delta: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardCandidateForecastErrors {
    pub days: usize,
    pub arrivals_abs_error_sum: String,
    pub resolved_abs_error_sum: String,
    pub backlog_abs_error_sum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_within_sla_abs_error_sum: Option<String>,
}

impl From<&CandidateForecastErrors> for DashboardCandidateForecastErrors {
    fn from(errors: &CandidateForecastErrors) -> Self {
        Self {
            days: errors.days,
            arrivals_abs_error_sum: errors.arrivals_abs_error_sum.to_string(),
            resolved_abs_error_sum: errors.resolved_abs_error_sum.to_string(),
            backlog_abs_error_sum: errors.backlog_abs_error_sum.to_string(),
            resolved_within_sla_abs_error_sum: errors
                .resolved_within_sla_abs_error_sum
                .map(|value| value.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardModelCandidateScore {
    pub replay_hash: String,
    pub status: String,
    pub comparison_run_hash: String,
    pub outcome_id: String,
    pub record_sha256: String,
    pub comparison_run_sha256: String,
    pub outcome_record_sha256: String,
    pub ticket_source_sha256: Option<String>,
    pub ticket_label_engine_sha256: Option<String>,
    pub ticket_source_retention_until_utc: Option<String>,
    pub candidate_created_at_unix: i64,
    pub comparison_created_at_unix: i64,
    pub observed_window_start_utc: String,
    pub source_version_hashes: Vec<String>,
    pub source_version_hashes_total: usize,
    pub parent: DashboardCandidateForecastErrors,
    pub candidate: DashboardCandidateForecastErrors,
    pub candidate_backlog_error_below_parent: bool,
    pub candidate_resolution_error_below_parent: bool,
    pub candidate_sla_error_below_parent: Option<bool>,
    pub limitations: Vec<String>,
}

impl DecisionStore {
    pub fn dashboard_create_model_candidate(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
        screen_replay_hash: &str,
    ) -> Result<DashboardModelCandidate, DecisionStoreError> {
        self.put_outcome_model_candidate(scope, candidate_id, screen_replay_hash)?;
        self.dashboard_load_model_candidate(scope, candidate_id)
    }

    pub fn dashboard_load_model_candidate(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
    ) -> Result<DashboardModelCandidate, DecisionStoreError> {
        let candidate = self.load_outcome_model_candidate(scope, candidate_id)?;
        let (stored, record_sha256): (StoredOutcomeModelCandidate, String) =
            self.get_with_digest(scope, "outcome_model_candidate", candidate_id)?;
        if candidate != stored
            || self.load_outcome_model_candidate(scope, candidate_id)? != candidate
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardModelCandidate {
            id: candidate.id,
            status: candidate.status,
            queue_id: candidate.queue_id,
            parent_model_version: candidate.parent_model_version,
            model: candidate.model,
            observed_through_utc: candidate.observed_through_utc,
            screen_replay_hash: candidate.screen_replay_hash,
            fit_id: candidate.fit_id,
            outcome_id: candidate.outcome_id,
            source_version_hashes: digest_lineage(&candidate.source_version_hashes),
            source_version_hashes_total: digest_lineage_total(&candidate.source_version_hashes),
            record_sha256,
            limitations: vec![
                "This is a simulation-only capacity proposal; it does not activate a model or authorize staffing".into(),
                "Local source digests and timestamps do not authenticate the upstream observation producer".into(),
            ],
        })
    }

    pub fn dashboard_save_model_candidate_run(
        &self,
        scope: &DecisionScope,
        candidate_id: &str,
        target_snapshot_id: &str,
        scenario_id: &str,
    ) -> Result<DashboardModelCandidateRun, DecisionStoreError> {
        let saved = self.put_outcome_model_candidate_run(
            scope,
            candidate_id,
            target_snapshot_id,
            scenario_id,
        )?;
        self.dashboard_load_model_candidate_run(scope, &saved.replay_hash)
    }

    pub fn dashboard_load_model_candidate_run(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<DashboardModelCandidateRun, DecisionStoreError> {
        let run = self.load_outcome_model_candidate_run(scope, replay_hash)?;
        let (stored, record_sha256): (StoredOutcomeModelCandidateRun, String) =
            self.get_with_digest(scope, "outcome_model_candidate_run", replay_hash)?;
        if run != stored || self.load_outcome_model_candidate_run(scope, replay_hash)? != run {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardModelCandidateRun {
            replay_hash: run.replay_hash,
            status: run.comparison.status,
            candidate_id: run.candidate_id,
            target_snapshot_id: run.target_snapshot_id,
            scenario_id: run.scenario_id,
            record_sha256,
            candidate_record_sha256: run.candidate_record_sha256,
            target_snapshot_sha256: run.target_snapshot_sha256,
            scenario_sha256: run.scenario_sha256,
            source_version_hashes: digest_lineage(&run.source_version_hashes),
            source_version_hashes_total: digest_lineage_total(&run.source_version_hashes),
            parent: (&run.comparison.parent).into(),
            candidate: (&run.comparison.candidate).into(),
            final_backlog_delta: run.comparison.final_backlog_delta.to_string(),
            resolved_within_sla_delta: run.comparison.resolved_within_sla_delta.to_string(),
            limitations: run.comparison.limitations,
        })
    }

    pub fn dashboard_save_model_candidate_score(
        &self,
        scope: &DecisionScope,
        comparison_run_hash: &str,
        outcome_id: &str,
    ) -> Result<DashboardModelCandidateScore, DecisionStoreError> {
        let saved =
            self.put_outcome_model_candidate_score(scope, comparison_run_hash, outcome_id)?;
        self.dashboard_load_model_candidate_score(scope, &saved.replay_hash)
    }

    pub fn dashboard_load_model_candidate_score(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<DashboardModelCandidateScore, DecisionStoreError> {
        let score = self.load_outcome_model_candidate_score(scope, replay_hash)?;
        let (stored, record_sha256): (StoredOutcomeModelCandidateScore, String) =
            self.get_with_digest(scope, "outcome_model_candidate_score", replay_hash)?;
        if score != stored || self.load_outcome_model_candidate_score(scope, replay_hash)? != score
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(DashboardModelCandidateScore {
            replay_hash: score.replay_hash,
            status: score.status,
            comparison_run_hash: score.comparison_run_hash,
            outcome_id: score.outcome_id,
            record_sha256,
            comparison_run_sha256: score.comparison_run_sha256,
            outcome_record_sha256: score.outcome_record_sha256,
            ticket_source_sha256: score.ticket_source_sha256,
            ticket_label_engine_sha256: score.ticket_label_engine_sha256,
            ticket_source_retention_until_utc: score.ticket_source_retention_until_utc,
            candidate_created_at_unix: score.candidate_created_at_unix,
            comparison_created_at_unix: score.comparison_created_at_unix,
            observed_window_start_utc: score.observed_window_start_utc,
            source_version_hashes: digest_lineage(&score.source_version_hashes),
            source_version_hashes_total: digest_lineage_total(&score.source_version_hashes),
            parent: (&score.parent).into(),
            candidate: (&score.candidate).into(),
            candidate_backlog_error_below_parent: score.candidate_backlog_error_below_parent,
            candidate_resolution_error_below_parent: score.candidate_resolution_error_below_parent,
            candidate_sla_error_below_parent: score.candidate_sla_error_below_parent,
            limitations: score.limitations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_sim::{DecisionSnapshot, StaffingScenario, simulate};

    fn sample_result() -> SimulationResult {
        let snapshot = DecisionSnapshot {
            id: "candidate-kpi-snapshot".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["candidate-kpi-source".into()],
            seed: 1,
            arrivals_by_day: vec![5, 5],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "candidate-kpi-model".into(),
            service_capacity_per_agent_day: 3,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "candidate-kpi-scenario".into(),
            agents_by_day: vec![1, 1],
            fixed_extra_capacity_by_day: vec![0, 0],
        };
        simulate(&snapshot, &model, &scenario).unwrap()
    }

    /// Regression: the candidate/parent KPI projection carried no engine
    /// state, so a stored result produced by an older simulator looked exactly
    /// like one the installed engine still reproduces. The flag must come from
    /// the recorded digest, not from the fact that the record loaded.
    #[test]
    fn candidate_kpis_report_engine_state_from_the_recorded_digest() {
        let current = sample_result();
        let fresh = DashboardCandidateKpis::from(&current);
        assert!(fresh.engine_matches_current);
        assert_eq!(
            serde_json::to_value(&fresh).unwrap()["engine_matches_current"],
            serde_json::json!(true)
        );
        let stale = SimulationResult {
            engine_sha256: "0".repeat(64),
            ..current
        };
        let projected = DashboardCandidateKpis::from(&stale);
        assert!(!projected.engine_matches_current);
        assert_eq!(
            serde_json::to_value(&projected).unwrap()["engine_matches_current"],
            serde_json::json!(false)
        );
    }
}
