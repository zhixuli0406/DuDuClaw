//! Aggregate Dashboard views over scoped outcome fits and review screens.

use serde::Serialize;

use crate::decision_calibration::CapacityFit;
use crate::decision_model_review::{
    OutcomeModelReviewCriteria, OutcomeModelReviewScreen, StoredOutcomeModelReviewScreen,
};
use crate::decision_outcome_calibration::CapacityHoldoutDiagnostic;
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, StoredOutcomeCalibration,
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

#[derive(Debug, Clone, Serialize)]
pub struct DashboardOutcomeHoldout {
    pub training_days: usize,
    pub holdout_days: usize,
    pub candidate_abs_error_sum: String,
    pub parent_abs_error_sum: String,
    pub no_change_abs_error_sum: String,
    pub candidate_beats_parent: bool,
    pub candidate_beats_no_change: bool,
}

impl From<&CapacityHoldoutDiagnostic> for DashboardOutcomeHoldout {
    fn from(value: &CapacityHoldoutDiagnostic) -> Self {
        Self {
            training_days: value.training_days,
            holdout_days: value.holdout_days,
            candidate_abs_error_sum: value.candidate_abs_error_sum.to_string(),
            parent_abs_error_sum: value.parent_abs_error_sum.to_string(),
            no_change_abs_error_sum: value.no_change_abs_error_sum.to_string(),
            candidate_beats_parent: value.candidate_beats_parent,
            candidate_beats_no_change: value.candidate_beats_no_change,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardOutcomeFit {
    pub id: String,
    pub outcome_id: String,
    pub replay_hash: String,
    pub observed_source_sha256: String,
    pub parent_model_version: String,
    pub parent_model_sha256: String,
    pub fit_engine_sha256: String,
    pub min_saturated_days: usize,
    pub training_days: usize,
    pub fit: CapacityFit,
    pub holdout: Option<DashboardOutcomeHoldout>,
    pub record_sha256: String,
    /// Whether the installed simulator still carries the engine identity that
    /// produced the historical run at `replay_hash`. Loading never recomputes
    /// that run, so this is the only signal a reader gets that the prediction
    /// this fit scores came from an older engine.
    pub engine_matches_current: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardOutcomeScreenReport {
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
    pub holdout: Option<DashboardOutcomeHoldout>,
    pub queue_id: Option<String>,
    pub local_run_precedes_window: Option<bool>,
    pub eligible_for_human_review: bool,
    pub failed_checks: Vec<String>,
    pub limitations: Vec<String>,
}

impl From<OutcomeModelReviewScreen> for DashboardOutcomeScreenReport {
    fn from(report: OutcomeModelReviewScreen) -> Self {
        Self {
            status: report.status,
            replay_hash: report.replay_hash,
            review_engine_sha256: report.review_engine_sha256,
            criteria: report.criteria,
            fit_id: report.fit_id,
            fit_record_sha256: report.fit_record_sha256,
            outcome_id: report.outcome_id,
            replayed_run_hash: report.replayed_run_hash,
            observed_source_sha256: report.observed_source_sha256,
            parent_model_version: report.parent_model_version,
            parent_model_sha256: report.parent_model_sha256,
            fitted_capacity_per_agent_day: report.fitted_capacity_per_agent_day,
            saturated_training_days: report.saturated_training_days,
            holdout: report.holdout.as_ref().map(Into::into),
            queue_id: report.queue_id,
            local_run_precedes_window: report.local_run_precedes_window,
            eligible_for_human_review: report.eligible_for_human_review,
            failed_checks: report.failed_checks,
            limitations: report.limitations,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardOutcomeScreen {
    pub replay_hash: String,
    pub fit_id: String,
    pub fit_record_sha256: String,
    pub record_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub source_version_hashes_total: usize,
    /// Engine state of the run at `report.replayed_run_hash`. It sits beside
    /// the report rather than inside it: the report is a saved, hashed
    /// verdict, while this is recomputed against the installed engine on every
    /// read and must never be mistaken for part of that frozen record.
    pub engine_matches_current: bool,
    pub report: DashboardOutcomeScreenReport,
}

impl DecisionStore {
    pub fn dashboard_create_outcome_fit(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
        outcome_id: &str,
        min_saturated_days: usize,
        training_days: usize,
    ) -> Result<DashboardOutcomeFit, DecisionStoreError> {
        self.put_outcome_calibration(scope, fit_id, outcome_id, min_saturated_days, training_days)?;
        self.dashboard_load_outcome_fit(scope, fit_id)
    }

    pub fn dashboard_load_outcome_fit(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
    ) -> Result<DashboardOutcomeFit, DecisionStoreError> {
        let fit = self.load_outcome_calibration(scope, fit_id)?;
        let (stored, record_sha256): (StoredOutcomeCalibration, String) =
            self.get_with_digest(scope, "outcome_calibration", fit_id)?;
        if stored != fit || self.load_outcome_calibration(scope, fit_id)? != fit {
            return Err(DecisionStoreError::Corrupt);
        }
        let mut limitations = vec![
            "Held-out capacity errors use observed arrivals and opening backlog; they are not live demand forecast skill".into(),
            "The fit is exploratory and never activates a model or staffing action".into(),
        ];
        if fit.holdout.is_none() {
            limitations.push("This saved fit has no held-out diagnostic".into());
        }
        let engine_matches_current = self
            .load_daily_run_with_engine_state(scope, &fit.replay_hash)?
            .engine_matches_current;
        if !engine_matches_current {
            limitations.push(
                "The scored run was produced by an earlier simulator version and is not reproducible by the installed engine".into(),
            );
        }
        Ok(DashboardOutcomeFit {
            id: fit.id,
            outcome_id: fit.outcome_id,
            replay_hash: fit.replay_hash,
            observed_source_sha256: fit.observed_source_sha256,
            parent_model_version: fit.parent_model_version,
            parent_model_sha256: fit.parent_model_sha256,
            fit_engine_sha256: fit.fit_engine_sha256,
            min_saturated_days: fit.min_saturated_days,
            training_days: fit.training_days,
            fit: fit.fit,
            holdout: fit.holdout.as_ref().map(Into::into),
            record_sha256,
            engine_matches_current,
            limitations,
        })
    }

    pub fn dashboard_save_outcome_screen(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
        criteria: &OutcomeModelReviewCriteria,
    ) -> Result<DashboardOutcomeScreen, DecisionStoreError> {
        let saved = self.put_outcome_model_review_screen(scope, fit_id, criteria)?;
        self.dashboard_load_outcome_screen(scope, &saved.replay_hash)
    }

    pub fn dashboard_load_outcome_screen(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<DashboardOutcomeScreen, DecisionStoreError> {
        let screen = self.load_outcome_model_review_screen(scope, replay_hash)?;
        let (stored, record_sha256): (StoredOutcomeModelReviewScreen, String) =
            self.get_with_digest(scope, "outcome_model_review_screen", replay_hash)?;
        if stored != screen || self.load_outcome_model_review_screen(scope, replay_hash)? != screen
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let engine_matches_current = self
            .load_daily_run_with_engine_state(scope, &screen.report.replayed_run_hash)?
            .engine_matches_current;
        Ok(DashboardOutcomeScreen {
            replay_hash: screen.replay_hash,
            fit_id: screen.fit_id,
            fit_record_sha256: screen.fit_record_sha256,
            record_sha256,
            source_version_hashes: digest_lineage(&screen.source_version_hashes),
            source_version_hashes_total: digest_lineage_total(&screen.source_version_hashes),
            engine_matches_current,
            report: screen.report.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_calibration::ObservedSupportDay;
    use crate::decision_sim::{DecisionSnapshot, QueueModel, StaffingScenario, simulate};
    use crate::decision_store::{ObservedOutcomeExport, StoredDailyRun};
    use rusqlite::{Connection, params};

    /// One scoped fit plus its review screen over a seven-day observation.
    /// With `stale_engine_sha256` the stored daily run keeps its real inputs
    /// and replay hash but records a foreign engine digest — the shape a run
    /// left behind by an earlier simulator has on disk. Loading never
    /// recomputes such a run, which is exactly why the reader needs a flag.
    fn seeded_fit(
        dir: &std::path::Path,
        stale_engine_sha256: Option<&str>,
    ) -> (DecisionStore, DecisionScope) {
        let store = DecisionStore::new(dir.join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "outcome-dashboard".into(),
            acl: "private".into(),
        };
        let start = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            - chrono::Duration::days(30);
        let snapshot = DecisionSnapshot {
            id: "outcome-dashboard-snapshot".into(),
            queue_id: Some("support".into()),
            data_cutoff_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            source_version_hashes: vec!["outcome-dashboard-source".into()],
            seed: 1,
            arrivals_by_day: vec![5; 7],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "outcome-dashboard-parent".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "outcome-dashboard-base".into(),
            agents_by_day: vec![1; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        let snapshot_sha256 = store.put_snapshot(&scope, &snapshot).unwrap();
        let model_sha256 = store.put_model(&scope, &model).unwrap();
        let scenario_sha256 = store.put_scenario(&scope, &scenario).unwrap();
        let replay_hash = match stale_engine_sha256 {
            None => {
                store
                    .put_daily_run(&scope, &snapshot.id, &model.version, &scenario.id)
                    .unwrap()
                    .replay_hash
            }
            Some(engine) => {
                let mut result = simulate(&snapshot, &model, &scenario).unwrap();
                result.engine_sha256 = engine.to_owned();
                let record = StoredDailyRun {
                    replay_hash: result.replay_hash.clone(),
                    snapshot_id: snapshot.id.clone(),
                    snapshot_sha256,
                    source_version_hashes: snapshot.source_version_hashes.clone(),
                    model_version: model.version.clone(),
                    model_sha256,
                    scenario_id: scenario.id.clone(),
                    scenario_sha256,
                    result,
                };
                store
                    .put(
                        &scope,
                        "daily_run",
                        &record.replay_hash,
                        &record,
                        Some(&snapshot.source_version_hashes),
                    )
                    .unwrap();
                record.replay_hash
            }
        };
        // Production reads this from the local SQLite journal; the fit screen
        // requires the run to predate the observed window.
        Connection::open(store.path())
            .unwrap()
            .execute(
                "UPDATE decision_inputs SET created_at=?1 WHERE tenant_id=?2 AND acl=?3
                 AND kind='daily_run' AND input_id=?4",
                params![
                    start.timestamp() - 86_400,
                    scope.tenant_id,
                    scope.acl,
                    replay_hash
                ],
            )
            .unwrap();
        let export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support".into()),
            window_start_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_through_utc: (start + chrono::Duration::days(7))
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
        store
            .record_observed_outcome(
                &scope,
                "observed",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay_hash,
                "recorder",
                &serde_json::to_vec(&export).unwrap(),
            )
            .unwrap();
        store
            .dashboard_create_outcome_fit(&scope, "fit", "observed", 3, 4)
            .unwrap();
        (store, scope)
    }

    /// Regression: `load_daily_run_with_engine_state` existed with no callers,
    /// so the dashboard fit and screen projections shipped a stored prediction
    /// with no way to tell whether the installed simulator still produces it.
    #[test]
    fn outcome_fit_and_screen_report_engine_state_of_the_scored_run() {
        let current_dir = tempfile::tempdir().unwrap();
        let (store, scope) = seeded_fit(current_dir.path(), None);
        let fit = store.dashboard_load_outcome_fit(&scope, "fit").unwrap();
        assert!(fit.engine_matches_current);
        assert!(
            !fit.limitations
                .iter()
                .any(|limit| limit.contains("earlier simulator version")),
            "{:?}",
            fit.limitations
        );
        assert_eq!(
            serde_json::to_value(&fit).unwrap()["engine_matches_current"],
            serde_json::json!(true)
        );
        let criteria = OutcomeModelReviewCriteria {
            min_saturated_days: 3,
            min_holdout_days: 3,
        };
        let screen = store
            .dashboard_save_outcome_screen(&scope, "fit", &criteria)
            .unwrap();
        assert!(screen.engine_matches_current);
        assert!(
            store
                .dashboard_load_outcome_screen(&scope, &screen.replay_hash)
                .unwrap()
                .engine_matches_current
        );

        let stale_dir = tempfile::tempdir().unwrap();
        let (stale_store, stale_scope) = seeded_fit(stale_dir.path(), Some(&"7".repeat(64)));
        let stale_fit = stale_store
            .dashboard_load_outcome_fit(&stale_scope, "fit")
            .unwrap();
        assert!(!stale_fit.engine_matches_current);
        assert!(
            stale_fit
                .limitations
                .iter()
                .any(|limit| limit.contains("earlier simulator version")),
            "{:?}",
            stale_fit.limitations
        );
        assert_eq!(
            serde_json::to_value(&stale_fit).unwrap()["engine_matches_current"],
            serde_json::json!(false)
        );
        let stale_screen = stale_store
            .dashboard_save_outcome_screen(&stale_scope, "fit", &criteria)
            .unwrap();
        assert!(!stale_screen.engine_matches_current);
        assert!(
            !stale_store
                .dashboard_load_outcome_screen(&stale_scope, &stale_screen.replay_hash)
                .unwrap()
                .engine_matches_current
        );
    }
}
