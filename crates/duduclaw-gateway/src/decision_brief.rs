//! Auditable comparison of two exploratory support-queue scenarios.
//!
//! All numeric results come from the deterministic simulator. The brief
//! intentionally leaves uncertainty intervals absent until a calibrated
//! distribution and held-out validation exist.

use duduclaw_memory::causal::EvidenceScope;
use duduclaw_memory::causal_effect::{
    EffectResult, LeaveOneUnitOutDiagnostic, NegativeControlDiagnostic, PermutationDiagnostic,
    PreTreatmentPlaceboDiagnostic, TemporalHoldoutDiagnostic,
};
use serde::{Deserialize, Serialize};

use crate::decision_dashboard::DecisionSlaHoldoutReport;
use crate::decision_empirical::{EmpiricalSensitivityPlan, EmpiricalSensitivityReport};
use crate::decision_event::{EventQueueConfig, EventSimulationResult};
#[cfg(doc)]
use crate::decision_ingest::SlaHoldoutDiagnostic;
use crate::decision_policy::JointRiskScreenReport;
use crate::decision_sim::{
    DecisionSnapshot, QueueModel, SimulationResult, StaffingScenario, simulate,
};
use crate::decision_store::{
    DecisionScope, DecisionSourceLink, DecisionStore, DecisionStoreError, StoredEmpiricalRun,
    StoredEventRun, StoredForecastValidation, StoredPolicyScreen, StoredSlaHoldout,
};

/// Simulated totals for one scenario.
///
/// Every field is a **decimal string**, the wire type this crate uses for an
/// accumulated integer (see `docs/spec/support-decision-twin.md`, "Wire types
/// for large integers"). A JSON number is an IEEE-754 double in most readers
/// and silently rounds past 2^53; backlog, resolution counts, and staff cost
/// in cents are accumulated over a caller-chosen horizon and are not bounded
/// by anything this crate enforces. The simulator keeps `u64`; only the
/// response shape differs, and no hashed payload contains this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefKpis {
    pub final_backlog: String,
    pub total_resolved: String,
    pub resolved_within_sla: String,
    pub total_staff_cost_cents: String,
}

impl From<&SimulationResult> for BriefKpis {
    fn from(result: &SimulationResult) -> Self {
        Self {
            final_backlog: result.final_backlog.to_string(),
            total_resolved: result.total_resolved.to_string(),
            resolved_within_sla: result.total_resolved_within_sla.to_string(),
            total_staff_cost_cents: result.total_staff_cost_cents.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefDelta {
    /// Scenario minus baseline, as a signed decimal string for the same
    /// reason as [`BriefKpis`]. Negative backlog and positive SLA resolutions
    /// are desirable; cost is reported separately, never hidden in a score.
    pub final_backlog: String,
    pub total_resolved: String,
    pub resolved_within_sla: String,
    pub total_staff_cost_cents: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaySpec {
    pub engine_sha256: String,
    pub snapshot_id: String,
    pub snapshot_sha256: String,
    pub model_version: String,
    pub model_sha256: String,
    pub baseline_scenario_id: String,
    pub baseline_scenario_sha256: String,
    pub alternative_scenario_id: String,
    pub alternative_scenario_sha256: String,
    pub baseline_replay_hash: String,
    pub alternative_replay_hash: String,
    pub baseline_command: ReplayCommand,
    pub alternative_command: ReplayCommand,
}

/// Execute as process arguments, not by interpolating into a shell string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefEmpiricalOrigin {
    SyntheticFixture,
    OperatorUpload,
    Unclassified,
}

impl Default for BriefEmpiricalOrigin {
    fn default() -> Self {
        Self::Unclassified
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefEmpiricalEvidence {
    /// Source class rederived from current scoped causal metadata. Older
    /// saved briefs have no class and remain unclassified on deserialization.
    #[serde(default)]
    pub source_origin: BriefEmpiricalOrigin,
    pub run_id: String,
    pub run_sha256: String,
    pub fit_id: String,
    pub fit_sha256: String,
    pub plan: EmpiricalSensitivityPlan,
    /// Descriptive resampling outcomes, not calibrated confidence intervals.
    pub report: EmpiricalSensitivityReport,
}

/// A saved exploratory review screen over the brief's exact empirical run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefPolicyEvidence {
    pub screen_hash: String,
    pub screen_record_sha256: String,
    pub report: JointRiskScreenReport,
}

/// A revalidated historical forecast assessment, separate from scenario KPIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefForecastEvidence {
    pub record_id: String,
    pub record_sha256: String,
    pub source_sha256: String,
    pub calibration_engine_sha256: String,
    pub window_start_utc: String,
    pub calibration_points: usize,
    pub forecast_points: usize,
    /// Decimal strings, matching the Decision Lab forecast reports these are
    /// meant to be compared against. The stored record keeps `u128`; a JSON
    /// number cannot carry these sums exactly past 2^53, and shipping the same
    /// quantity as a number here and a string there made the two views
    /// impossible to compare without knowing which one you were holding.
    pub arrival_abs_error_sum: String,
    pub model_abs_error_sum: String,
    pub no_change_abs_error_sum: String,
    pub seasonal_naive_abs_error_sum: String,
    pub mean_change_abs_error_sum: String,
    pub model_beats_all_baselines: bool,
    pub interval_evaluated_points: Option<usize>,
    pub rolling_observed_coverage_basis_points: Option<u16>,
    pub fixed_observed_coverage_basis_points: Option<u16>,
}

/// Revalidated ticket-level SLA replay using observed later demand and staffing.
///
/// `diagnostic` is the **wire mirror** [`DecisionSlaHoldoutReport`], the exact
/// type `POST /api/decision/engineering-validation` already returns for this
/// same record — its `u128` error sums cross as decimal strings instead of
/// JSON numbers that round past 2^53. The stored [`SlaHoldoutDiagnostic`] is
/// unchanged, and the mirror is built **after** the record has been reloaded,
/// recomputed from the exact source bytes, and digested (see
/// `DecisionStore::attach_sla_holdout`): `record_sha256` and the store's
/// `replay_hash` are always taken over the stored `u128` struct, never over
/// this presentation type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefSlaHoldoutEvidence {
    pub record_id: String,
    pub record_sha256: String,
    pub source_sha256: String,
    pub engine_sha256: String,
    pub diagnostic: DecisionSlaHoldoutReport,
}

/// Ticket-event totals for one scenario. Decimal strings for the same reason
/// as [`BriefKpis`]; `null` still means "not computable", never zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefEventKpis {
    pub final_backlog: String,
    pub total_resolved: String,
    pub resolved_within_sla: String,
    pub total_staff_cost_cents: String,
    /// Among resolved tickets only; pending waits are right-censored.
    pub resolved_wait_seconds_p50: Option<String>,
    pub resolved_wait_seconds_p95: Option<String>,
}

impl From<&EventSimulationResult> for BriefEventKpis {
    fn from(result: &EventSimulationResult) -> Self {
        Self {
            final_backlog: result.final_backlog.to_string(),
            total_resolved: result.total_resolved.to_string(),
            resolved_within_sla: result.total_resolved_within_sla.to_string(),
            total_staff_cost_cents: result.total_staff_cost_cents.to_string(),
            resolved_wait_seconds_p50: result.wait_seconds_p50.map(|v| v.to_string()),
            resolved_wait_seconds_p95: result.wait_seconds_p95.map(|v| v.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefEventEvidence {
    pub source_sha256: String,
    pub window_start_utc: String,
    pub daily_engine_sha256: String,
    pub event_engine_sha256: String,
    pub config: EventQueueConfig,
    pub baseline_run_hash: String,
    pub baseline_run_sha256: String,
    pub alternative_run_hash: String,
    pub alternative_run_sha256: String,
    pub baseline: BriefEventKpis,
    pub alternative: BriefEventKpis,
    /// Alternative minus baseline, among resolved tickets in each scenario.
    /// Signed decimal string; `null` when either side has no resolved ticket.
    pub resolved_wait_seconds_p95_delta: Option<String>,
}

/// A reviewed observational estimate, kept separate from simulator KPIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefCausalEvidenceLink {
    pub estimate_id: String,
    pub model_id: String,
    pub data_artifact_id: String,
    pub data_sha256: String,
    /// Evidence may be ingested after the observation cutoff for retrospective analysis.
    pub data_ingested_at: i64,
    pub method: String,
    pub identification_state: String,
    /// Approximate conditional interval from the observational estimator;
    /// this is not a calibrated intervention or scenario KPI interval.
    pub estimate: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub interval_kind: String,
    pub unit_count: usize,
    pub strata_count: usize,
    /// Descriptive checks of stability and falsification, never proof of no confounding.
    pub leave_one_stratum_out_sign_flip: Option<bool>,
    pub leave_one_unit_out: LeaveOneUnitOutDiagnostic,
    pub temporal_holdout: Option<TemporalHoldoutDiagnostic>,
    pub permutation_diagnostic: PermutationDiagnostic,
    pub pre_treatment_placebo: Option<PreTreatmentPlaceboDiagnostic>,
    pub negative_control: Option<NegativeControlDiagnostic>,
    pub treatment_variable_id: String,
    pub treatment_name: String,
    pub treatment_unit: String,
    pub outcome_variable_id: String,
    pub outcome_name: String,
    pub outcome_unit: String,
    pub population: String,
    pub window_start: i64,
    pub window_end: i64,
}

/// Optional evidence selected for one freshly verified scenario comparison.
/// Event runs require an empirical run and the exact source bytes.
pub struct BriefEvidenceSelection<'a> {
    pub empirical_run_id: Option<&'a str>,
    pub policy_screen_hash: Option<&'a str>,
    pub event_runs: Option<(&'a str, &'a str, &'a [u8])>,
    pub forecast_validation: Option<(&'a str, &'a [u8])>,
    pub sla_holdout: Option<(&'a str, &'a [u8])>,
    pub effect_ids: &'a [String],
}

fn replay_command(
    db_path: &str,
    causal_db_path: Option<&str>,
    scope: &DecisionScope,
    snapshot_id: &str,
    model_version: &str,
    scenario_id: &str,
    expected_hash: &str,
) -> ReplayCommand {
    let mut args = vec!["decision-replay".into(), "--db".into(), db_path.into()];
    if let Some(causal_db_path) = causal_db_path {
        args.push("--causal-db".into());
        args.push(causal_db_path.into());
    }
    args.extend([
        "--tenant".into(),
        scope.tenant_id.clone(),
        "--acl".into(),
        scope.acl.clone(),
        "--snapshot".into(),
        snapshot_id.into(),
        "--model".into(),
        model_version.into(),
        "--scenario".into(),
        scenario_id.into(),
        "--expected-hash".into(),
        expected_hash.into(),
    ]);
    ReplayCommand {
        program: "duduclaw".into(),
        args,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionBrief {
    pub status: String,
    /// Exact queue ID from the immutable snapshot when row-level identity exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub data_cutoff_utc: String,
    pub source_version_hashes: Vec<String>,
    /// Active source-artifact links for input provenance, not causal effects.
    pub source_artifact_links: Vec<DecisionSourceLink>,
    pub horizon_days: usize,
    pub replay: ReplaySpec,
    pub baseline: BriefKpis,
    pub alternative: BriefKpis,
    pub delta: BriefDelta,
    /// No confidence interval is asserted before calibration and backtesting.
    pub uncertainty_interval: Option<(BriefDelta, BriefDelta)>,
    #[serde(default)]
    pub exploratory_empirical: Option<BriefEmpiricalEvidence>,
    #[serde(default)]
    pub exploratory_policy_screen: Option<BriefPolicyEvidence>,
    #[serde(default)]
    pub exploratory_forecast: Option<BriefForecastEvidence>,
    #[serde(default)]
    pub exploratory_sla_holdout: Option<BriefSlaHoldoutEvidence>,
    #[serde(default)]
    pub exploratory_event: Option<BriefEventEvidence>,
    #[serde(default)]
    pub observational_effects: Vec<BriefCausalEvidenceLink>,
    pub assumptions: Vec<String>,
    pub limitations: Vec<String>,
}

impl DecisionStore {
    /// Build a read-only brief from immutable inputs in one exact ACL scope.
    pub fn compare_scenarios(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        assumptions: Vec<String>,
    ) -> Result<DecisionBrief, DecisionStoreError> {
        if baseline_id == alternative_id || assumptions.iter().any(|item| item.trim().is_empty()) {
            return Err(DecisionStoreError::Invalid);
        }
        let (snapshot, snapshot_sha256): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", snapshot_id)?;
        let (model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", model_version)?;
        let (baseline_scenario, baseline_scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", baseline_id)?;
        let (alternative_scenario, alternative_scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", alternative_id)?;
        let baseline = simulate(&snapshot, &model, &baseline_scenario)?;
        let alternative = simulate(&snapshot, &model, &alternative_scenario)?;
        let baseline_kpis = BriefKpis::from(&baseline);
        let alternative_kpis = BriefKpis::from(&alternative);
        // Subtract in `i128` (exact for any `u64` pair), then render. The
        // arithmetic never goes through `f64`, so the decimal string is the
        // exact difference, not a rounded one.
        let delta = BriefDelta {
            final_backlog: (alternative.final_backlog as i128 - baseline.final_backlog as i128)
                .to_string(),
            total_resolved: (alternative.total_resolved as i128
                - baseline.total_resolved as i128)
                .to_string(),
            resolved_within_sla: (alternative.total_resolved_within_sla as i128
                - baseline.total_resolved_within_sla as i128)
                .to_string(),
            total_staff_cost_cents: (alternative.total_staff_cost_cents as i128
                - baseline.total_staff_cost_cents as i128)
                .to_string(),
        };
        let db_path = self
            .path()
            .canonicalize()?
            .into_os_string()
            .into_string()
            .map_err(|_| DecisionStoreError::Invalid)?;
        let causal_db_path = self
            .causal_store_path()
            .map(|path| {
                path.canonicalize()?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| DecisionStoreError::Invalid)
            })
            .transpose()?;
        let baseline_command = replay_command(
            &db_path,
            causal_db_path.as_deref(),
            scope,
            &snapshot.id,
            &model.version,
            &baseline_scenario.id,
            &baseline.replay_hash,
        );
        let alternative_command = replay_command(
            &db_path,
            causal_db_path.as_deref(),
            scope,
            &snapshot.id,
            &model.version,
            &alternative_scenario.id,
            &alternative.replay_hash,
        );
        let source_artifact_links = self.active_source_links(scope, &snapshot)?;
        Ok(DecisionBrief {
            status: "exploratory".into(),
            queue_id: snapshot.queue_id.clone(),
            data_cutoff_utc: snapshot.data_cutoff_utc,
            source_version_hashes: snapshot.source_version_hashes,
            source_artifact_links,
            horizon_days: snapshot.arrivals_by_day.len(),
            replay: ReplaySpec {
                engine_sha256: baseline.engine_sha256,
                snapshot_id: snapshot.id,
                snapshot_sha256,
                model_version: model.version,
                model_sha256,
                baseline_scenario_id: baseline_scenario.id,
                baseline_scenario_sha256,
                alternative_scenario_id: alternative_scenario.id,
                alternative_scenario_sha256,
                baseline_replay_hash: baseline.replay_hash,
                alternative_replay_hash: alternative.replay_hash,
                baseline_command,
                alternative_command,
            },
            baseline: baseline_kpis,
            alternative: alternative_kpis,
            delta,
            uncertainty_interval: None,
            exploratory_empirical: None,
            exploratory_policy_screen: None,
            exploratory_forecast: None,
            exploratory_sla_holdout: None,
            exploratory_event: None,
            observational_effects: Vec::new(),
            assumptions,
            limitations: vec![
                "Arrival counts are fixed to the recorded snapshot; demand response is not modelled".into(),
                "Service capacity and staffing cost are fixed inputs, not fitted distributions".into(),
                "This brief has no real-data held-out validation or calibrated uncertainty interval".into(),
            ],
        })
    }

    /// Attach a currently valid, exact-scope observational estimate by ID.
    /// The estimate is only an evidence link: it does not alter simulated KPIs.
    pub fn compare_scenarios_with_causal_evidence(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        estimate_ids: &[String],
        assumptions: Vec<String>,
    ) -> Result<DecisionBrief, DecisionStoreError> {
        if estimate_ids.is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        self.compare_scenarios_with_evidence(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
            BriefEvidenceSelection {
                empirical_run_id: None,
                policy_screen_hash: None,
                event_runs: None,
                forecast_validation: None,
                sla_holdout: None,
                effect_ids: estimate_ids,
            },
            assumptions,
        )
    }

    /// Compose independently revalidated simulation and observational evidence.
    /// Every branch starts from the same scoped snapshot and scenario order.
    pub fn compare_scenarios_with_evidence(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        selection: BriefEvidenceSelection<'_>,
        assumptions: Vec<String>,
    ) -> Result<DecisionBrief, DecisionStoreError> {
        let mut brief = match (selection.empirical_run_id, selection.event_runs) {
            (None, None) => self.compare_scenarios(
                scope,
                snapshot_id,
                model_version,
                baseline_id,
                alternative_id,
                assumptions,
            )?,
            (Some(run_id), None) => self.compare_scenarios_with_empirical_run(
                scope,
                snapshot_id,
                model_version,
                baseline_id,
                alternative_id,
                run_id,
                assumptions,
            )?,
            (Some(run_id), Some((baseline_event, alternative_event, source_bytes))) => self
                .compare_scenarios_with_empirical_and_event_runs(
                    scope,
                    snapshot_id,
                    model_version,
                    baseline_id,
                    alternative_id,
                    run_id,
                    baseline_event,
                    alternative_event,
                    source_bytes,
                    assumptions,
                )?,
            (None, Some(_)) => return Err(DecisionStoreError::Invalid),
        };
        if let Some(screen_hash) = selection.policy_screen_hash {
            brief = self.attach_policy_screen(scope, brief, screen_hash)?;
        }
        if let Some((record_id, source_bytes)) = selection.forecast_validation {
            brief = self.attach_forecast_validation(scope, brief, record_id, source_bytes)?;
        }
        if let Some((record_id, source_bytes)) = selection.sla_holdout {
            brief = self.attach_sla_holdout(scope, brief, record_id, source_bytes)?;
        }
        if selection.effect_ids.is_empty() {
            return Ok(brief);
        }
        self.attach_causal_evidence(scope, brief, selection.effect_ids)
    }

    fn attach_policy_screen(
        &self,
        scope: &DecisionScope,
        mut brief: DecisionBrief,
        screen_hash: &str,
    ) -> Result<DecisionBrief, DecisionStoreError> {
        let empirical = brief
            .exploratory_empirical
            .as_ref()
            .ok_or(DecisionStoreError::Invalid)?;
        let record = self.load_policy_screen(scope, screen_hash)?;
        let (_, screen_record_sha256): (StoredPolicyScreen, String) =
            self.get_with_digest(scope, "policy_screen", screen_hash)?;
        let sweep = &record.report.policy_sweep;
        if record.empirical_run_id != empirical.run_id
            || record.report.output_integrity_version != 3
            || record.empirical_run_sha256 != empirical.run_sha256
            || record.source_version_hashes != brief.source_version_hashes
            || sweep.snapshot_id != brief.replay.snapshot_id
            || sweep.model_version != brief.replay.model_version
            || sweep.baseline_scenario_id != brief.replay.baseline_scenario_id
            || sweep.alternative_scenario_id != brief.replay.alternative_scenario_id
        {
            return Err(DecisionStoreError::Invalid);
        }
        brief.exploratory_policy_screen = Some(BriefPolicyEvidence {
            screen_hash: record.replay_hash,
            screen_record_sha256,
            report: record.report,
        });
        brief.limitations.push(
            "The linked policy screen is exploratory, uses the saved empirical draws and operator limits, and does not authorize a staffing action".into(),
        );
        Ok(brief)
    }

    fn attach_forecast_validation(
        &self,
        scope: &DecisionScope,
        mut brief: DecisionBrief,
        record_id: &str,
        source_bytes: &[u8],
    ) -> Result<DecisionBrief, DecisionStoreError> {
        let record = self.load_forecast_validation(scope, record_id, source_bytes)?;
        let (_, record_sha256): (StoredForecastValidation, String) =
            self.get_with_digest(scope, "forecast_validation", record_id)?;
        if record.snapshot_id != brief.replay.snapshot_id
            || record.snapshot_sha256 != brief.replay.snapshot_sha256
            || record.source_version_hashes != brief.source_version_hashes
            || record.baseline_scenario_id != brief.replay.baseline_scenario_id
            || record.baseline_scenario_sha256 != brief.replay.baseline_scenario_sha256
        {
            return Err(DecisionStoreError::Invalid);
        }
        brief.exploratory_forecast = Some(BriefForecastEvidence {
            record_id: record.id,
            record_sha256,
            source_sha256: record.source_sha256,
            calibration_engine_sha256: record.calibration_engine_sha256,
            window_start_utc: record.window_start_utc,
            calibration_points: record.calibration_points,
            forecast_points: record.forecast.evaluation_days,
            arrival_abs_error_sum: record.forecast.arrival_abs_error_sum.to_string(),
            model_abs_error_sum: record.forecast.model_abs_error_sum.to_string(),
            no_change_abs_error_sum: record.forecast.no_change_abs_error_sum.to_string(),
            seasonal_naive_abs_error_sum: record.forecast.seasonal_naive_abs_error_sum.to_string(),
            mean_change_abs_error_sum: record.forecast.mean_change_abs_error_sum.to_string(),
            model_beats_all_baselines: record.forecast.model_beats_all_baselines,
            interval_evaluated_points: record
                .fixed_interval
                .as_ref()
                .map(|value| value.evaluated_points),
            rolling_observed_coverage_basis_points: record
                .rolling_interval
                .as_ref()
                .map(|value| value.observed_coverage_basis_points),
            fixed_observed_coverage_basis_points: record
                .fixed_interval
                .as_ref()
                .map(|value| value.observed_coverage_basis_points),
        });
        brief.limitations.push(
            "Forecast coverage is retrospective on the source export and does not calibrate scenario KPI intervals".into(),
        );
        Ok(brief)
    }

    fn attach_sla_holdout(
        &self,
        scope: &DecisionScope,
        mut brief: DecisionBrief,
        record_id: &str,
        source_bytes: &[u8],
    ) -> Result<DecisionBrief, DecisionStoreError> {
        let record = self.load_sla_holdout(scope, record_id, source_bytes)?;
        let (_, record_sha256): (StoredSlaHoldout, String) =
            self.get_with_digest(scope, "sla_holdout", record_id)?;
        if record.snapshot_id != brief.replay.snapshot_id
            || record.snapshot_sha256 != brief.replay.snapshot_sha256
            || record.model_version != brief.replay.model_version
            || record.model_sha256 != brief.replay.model_sha256
            || record.baseline_scenario_id != brief.replay.baseline_scenario_id
            || record.baseline_scenario_sha256 != brief.replay.baseline_scenario_sha256
            || record.source_version_hashes != brief.source_version_hashes
        {
            return Err(DecisionStoreError::Invalid);
        }
        // Convert to the wire mirror only here: `load_sla_holdout` has already
        // recomputed the stored `u128` diagnostic from the exact source bytes
        // and compared it byte-for-byte, and `record_sha256` above is the
        // digest of that stored struct. Nothing downstream re-derives a hash
        // from the presentation type.
        brief.exploratory_sla_holdout = Some(BriefSlaHoldoutEvidence {
            record_id: record.id,
            record_sha256,
            source_sha256: record.source_sha256,
            engine_sha256: record.engine_sha256,
            diagnostic: record.diagnostic.into(),
        });
        brief.limitations.push(
            "The SLA holdout uses observed later demand and staffing; its error does not validate prospective demand, calibrated intervals, or a staffing intervention".into(),
        );
        Ok(brief)
    }

    fn attach_causal_evidence(
        &self,
        scope: &DecisionScope,
        mut brief: DecisionBrief,
        estimate_ids: &[String],
    ) -> Result<DecisionBrief, DecisionStoreError> {
        if estimate_ids.is_empty() || estimate_ids.len() > 32 {
            return Err(DecisionStoreError::Invalid);
        }
        let causal_store = self
            .causal_store()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let cutoff = chrono::DateTime::parse_from_rfc3339(&brief.data_cutoff_utc)
            .map_err(|_| DecisionStoreError::Invalid)?
            .timestamp();
        let mut seen = std::collections::HashSet::new();
        for estimate_id in estimate_ids {
            if !seen.insert(estimate_id.as_str()) {
                return Err(DecisionStoreError::Invalid);
            }
            let EffectResult::Estimated(effect) =
                causal_store.read_effect_estimate(&evidence_scope, estimate_id)?
            else {
                return Err(DecisionStoreError::Invalid);
            };
            let artifact =
                causal_store.read_artifact_metadata(&evidence_scope, &effect.data_artifact_id)?;
            let view = causal_store.model_review_view(&evidence_scope, &effect.model_id)?;
            let model = view.model;
            if artifact.content_sha256 != effect.data_sha256
                || artifact.occurred_at > cutoff
                || model.window_end > cutoff
                || model.window_start > model.window_end
            {
                return Err(DecisionStoreError::Invalid);
            }
            let treatment = view
                .variables
                .iter()
                .find(|variable| variable.id == model.treatment_variable_id)
                .ok_or(DecisionStoreError::Invalid)?;
            let outcome = view
                .variables
                .iter()
                .find(|variable| variable.id == model.outcome_variable_id)
                .ok_or(DecisionStoreError::Invalid)?;
            brief.observational_effects.push(BriefCausalEvidenceLink {
                estimate_id: effect.id,
                model_id: model.id,
                data_artifact_id: artifact.id,
                data_sha256: artifact.content_sha256,
                data_ingested_at: artifact.ingested_at,
                method: effect.method,
                identification_state: effect.identification_state,
                estimate: effect.estimate,
                lower_bound: effect.lower_bound,
                upper_bound: effect.upper_bound,
                interval_kind: "approximate_normal_conditional_on_recorded_strata".into(),
                unit_count: effect.unit_count,
                strata_count: effect.strata_count,
                leave_one_stratum_out_sign_flip: effect.leave_one_stratum_out_sign_flip,
                leave_one_unit_out: effect.leave_one_unit_out,
                temporal_holdout: effect.temporal_holdout,
                permutation_diagnostic: effect.permutation_diagnostic,
                pre_treatment_placebo: effect.pre_treatment_placebo,
                negative_control: effect.negative_control,
                treatment_variable_id: treatment.id.clone(),
                treatment_name: treatment.name.clone(),
                treatment_unit: treatment.unit.clone(),
                outcome_variable_id: outcome.id.clone(),
                outcome_name: outcome.name.clone(),
                outcome_unit: outcome.unit.clone(),
                population: model.population,
                window_start: model.window_start,
                window_end: model.window_end,
            });
        }
        brief.limitations.push(
            "Linked effects are reviewed observational estimates for their stated population and window; they do not validate the simulated staffing KPI deltas".into(),
        );
        Ok(brief)
    }

    /// Attach only a fully revalidated run over the same immutable inputs.
    /// Calibrated `uncertainty_interval` remains absent.
    pub fn compare_scenarios_with_empirical_run(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        run_id: &str,
        assumptions: Vec<String>,
    ) -> Result<DecisionBrief, DecisionStoreError> {
        let mut brief = self.compare_scenarios(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
            assumptions,
        )?;
        let run = self.load_empirical_run(scope, run_id)?;
        let (_, run_sha256): (StoredEmpiricalRun, String) =
            self.get_with_digest(scope, "empirical_run", run_id)?;
        if run.snapshot_id != brief.replay.snapshot_id
            || run.snapshot_sha256 != brief.replay.snapshot_sha256
            || run.source_version_hashes != brief.source_version_hashes
            || run.model_version != brief.replay.model_version
            || run.model_sha256 != brief.replay.model_sha256
            || run.baseline_id != brief.replay.baseline_scenario_id
            || run.baseline_sha256 != brief.replay.baseline_scenario_sha256
            || run.alternative_id != brief.replay.alternative_scenario_id
            || run.alternative_sha256 != brief.replay.alternative_scenario_sha256
        {
            return Err(DecisionStoreError::Invalid);
        }
        let source_origin = if let [link] = brief.source_artifact_links.as_slice() {
            if brief.source_version_hashes.len() != 1
                || brief.source_version_hashes[0] != link.source_version_sha256
            {
                return Err(DecisionStoreError::Corrupt);
            }
            if let Some(causal) = self.causal_store() {
                let metadata = causal.read_artifact_metadata(
                    &EvidenceScope {
                        tenant_id: scope.tenant_id.clone(),
                        acl: scope.acl.clone(),
                    },
                    &link.artifact_id,
                )?;
                if metadata.content_sha256 != link.source_version_sha256
                    || metadata.version != link.source_version_sha256
                {
                    return Err(DecisionStoreError::Revoked);
                }
                match metadata.kind.as_str() {
                    "synthetic_support_export" => BriefEmpiricalOrigin::SyntheticFixture,
                    "local_support_pilot_export" => BriefEmpiricalOrigin::OperatorUpload,
                    _ => BriefEmpiricalOrigin::Unclassified,
                }
            } else {
                BriefEmpiricalOrigin::Unclassified
            }
        } else {
            BriefEmpiricalOrigin::Unclassified
        };
        brief.exploratory_empirical = Some(BriefEmpiricalEvidence {
            source_origin: source_origin.clone(),
            run_id: run.id,
            run_sha256,
            fit_id: run.fit_id,
            fit_sha256: run.fit_sha256,
            plan: run.plan,
            report: run.report,
        });
        brief.limitations.push(match source_origin {
            BriefEmpiricalOrigin::SyntheticFixture => "Empirical ranges summarize finite synthetic resamples; no real-data coverage or calibrated confidence interval is claimed".into(),
            BriefEmpiricalOrigin::OperatorUpload => "Empirical ranges summarize finite resamples from an operator-uploaded historical export; upstream identity and calibrated confidence coverage are unverified".into(),
            BriefEmpiricalOrigin::Unclassified => "Empirical ranges summarize finite source-bound resamples; source class and calibrated confidence coverage are unverified".into(),
        });
        Ok(brief)
    }

    /// Attach two exact-source event runs to a fresh daily/empirical brief.
    /// Event waits remain descriptive among resolved tickets, not calibrated
    /// population or individual counterfactual outcomes.
    pub fn compare_scenarios_with_empirical_and_event_runs(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        empirical_run_id: &str,
        baseline_event_hash: &str,
        alternative_event_hash: &str,
        source_bytes: &[u8],
        assumptions: Vec<String>,
    ) -> Result<DecisionBrief, DecisionStoreError> {
        let mut brief = self.compare_scenarios_with_empirical_run(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
            empirical_run_id,
            assumptions,
        )?;
        let baseline = self.load_event_run(scope, baseline_event_hash, source_bytes)?;
        let alternative = self.load_event_run(scope, alternative_event_hash, source_bytes)?;
        let (_, baseline_sha256): (StoredEventRun, String) =
            self.get_with_digest(scope, "event_run", baseline_event_hash)?;
        let (_, alternative_sha256): (StoredEventRun, String) =
            self.get_with_digest(scope, "event_run", alternative_event_hash)?;
        let compatible = |run: &StoredEventRun, scenario_id: &str, scenario_sha256: &str| {
            run.snapshot_id == brief.replay.snapshot_id
                && run.snapshot_sha256 == brief.replay.snapshot_sha256
                && run.source_version_hashes == brief.source_version_hashes
                && run.model_version == brief.replay.model_version
                && run.model_sha256 == brief.replay.model_sha256
                && run.daily_engine_sha256 == brief.replay.engine_sha256
                && run.scenario_id == scenario_id
                && run.scenario_sha256 == scenario_sha256
                && run.baseline_scenario_id == brief.replay.baseline_scenario_id
                && run.baseline_scenario_sha256 == brief.replay.baseline_scenario_sha256
        };
        if baseline_event_hash == alternative_event_hash
            || !compatible(
                &baseline,
                &brief.replay.baseline_scenario_id,
                &brief.replay.baseline_scenario_sha256,
            )
            || !compatible(
                &alternative,
                &brief.replay.alternative_scenario_id,
                &brief.replay.alternative_scenario_sha256,
            )
            || baseline.source_sha256 != alternative.source_sha256
            || baseline.window_start_utc != alternative.window_start_utc
            || baseline.config != alternative.config
            || baseline.result.event_engine_sha256 != alternative.result.event_engine_sha256
            || baseline.result.initial_backlog != alternative.result.initial_backlog
            || baseline.result.total_arrivals != alternative.result.total_arrivals
        {
            return Err(DecisionStoreError::Invalid);
        }
        let p95_delta = baseline
            .result
            .wait_seconds_p95
            .zip(alternative.result.wait_seconds_p95)
            .map(|(base, alt)| (alt as i128 - base as i128).to_string());
        brief.exploratory_event = Some(BriefEventEvidence {
            source_sha256: baseline.source_sha256,
            window_start_utc: baseline.window_start_utc,
            daily_engine_sha256: baseline.daily_engine_sha256,
            event_engine_sha256: baseline.result.event_engine_sha256.clone(),
            config: baseline.config,
            baseline_run_hash: baseline.replay_hash,
            baseline_run_sha256: baseline_sha256,
            alternative_run_hash: alternative.replay_hash,
            alternative_run_sha256: alternative_sha256,
            baseline: BriefEventKpis::from(&baseline.result),
            alternative: BriefEventKpis::from(&alternative.result),
            resolved_wait_seconds_p95_delta: p95_delta,
        });
        brief.limitations.push(
            "Event wait quantiles exclude unresolved tickets and do not estimate a causal staffing effect".into(),
        );
        Ok(brief)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_is_replayable_and_does_not_invent_uncertainty() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "a".into(),
            acl: "private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "snap".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["source-v1".into()],
            seed: 1,
            arrivals_by_day: vec![5, 5],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 3,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let base = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1, 1],
            fixed_extra_capacity_by_day: vec![0, 0],
        };
        let more = StaffingScenario {
            id: "more".into(),
            agents_by_day: vec![2, 2],
            fixed_extra_capacity_by_day: vec![0, 0],
        };
        let snapshot_digest = store.put_snapshot(&scope, &snapshot).unwrap();
        let model_digest = store.put_model(&scope, &model).unwrap();
        let base_digest = store.put_scenario(&scope, &base).unwrap();
        let more_digest = store.put_scenario(&scope, &more).unwrap();
        let brief = store
            .compare_scenarios(&scope, "snap", "v1", "base", "more", vec!["FIFO".into()])
            .unwrap();
        assert_eq!(brief.status, "exploratory");
        assert!(brief.source_artifact_links.is_empty());
        assert!(brief.delta.final_backlog.parse::<i128>().unwrap() < 0);
        assert!(brief.delta.total_staff_cost_cents.parse::<i128>().unwrap() > 0);
        assert!(brief.uncertainty_interval.is_none());
        assert!(brief.exploratory_empirical.is_none());
        assert!(brief.exploratory_forecast.is_none());
        assert!(brief.observational_effects.is_empty());
        assert!(matches!(
            store.compare_scenarios_with_evidence(
                &scope,
                "snap",
                "v1",
                "base",
                "more",
                BriefEvidenceSelection {
                    empirical_run_id: None,
                    policy_screen_hash: None,
                    event_runs: Some(("event-base", "event-more", b"source")),
                    forecast_validation: None,
                    sla_holdout: None,
                    effect_ids: &[],
                },
                vec!["FIFO".into()],
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            store.compare_scenarios_with_causal_evidence(
                &scope,
                "snap",
                "v1",
                "base",
                "more",
                &["missing".into()],
                vec![],
            ),
            Err(DecisionStoreError::CausalStoreRequired)
        ));
        assert_eq!(brief.replay.snapshot_sha256, snapshot_digest);
        assert_eq!(
            brief.replay.engine_sha256,
            crate::decision_sim::engine_code_sha256()
        );
        assert_eq!(brief.replay.model_sha256, model_digest);
        assert_eq!(brief.replay.baseline_scenario_sha256, base_digest);
        assert_eq!(brief.replay.alternative_scenario_sha256, more_digest);
        assert_eq!(brief.replay.baseline_command.program, "duduclaw");
        assert_eq!(brief.replay.baseline_command.args[0], "decision-replay");
        assert!(
            brief
                .replay
                .baseline_command
                .args
                .contains(&brief.replay.baseline_replay_hash)
        );
        assert_eq!(
            brief.replay.baseline_replay_hash,
            store
                .replay(&scope, "snap", "v1", "base")
                .unwrap()
                .replay_hash
        );
        assert_eq!(
            brief.replay.alternative_replay_hash,
            store
                .replay(&scope, "snap", "v1", "more")
                .unwrap()
                .replay_hash
        );
    }

    /// Regression: the brief sent these sums as JSON numbers while the
    /// Decision Lab forecast report sent the same quantity as a decimal
    /// string. Two views of one stored record could not be compared without
    /// knowing which one you held, and a sum past 2^53 rounded in a browser.
    #[test]
    fn forecast_evidence_error_sums_are_decimal_strings() {
        let beyond_double = (u64::MAX as u128) + 1;
        let evidence = BriefForecastEvidence {
            record_id: "forecast-1".into(),
            record_sha256: "a".repeat(64),
            source_sha256: "b".repeat(64),
            calibration_engine_sha256: "c".repeat(64),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            calibration_points: 14,
            forecast_points: 16,
            arrival_abs_error_sum: 9_u128.to_string(),
            model_abs_error_sum: beyond_double.to_string(),
            no_change_abs_error_sum: 18_u128.to_string(),
            seasonal_naive_abs_error_sum: 15_u128.to_string(),
            mean_change_abs_error_sum: 14_u128.to_string(),
            model_beats_all_baselines: true,
            interval_evaluated_points: Some(2),
            rolling_observed_coverage_basis_points: Some(5_000),
            fixed_observed_coverage_basis_points: Some(10_000),
        };
        let value = serde_json::to_value(&evidence).unwrap();
        for field in [
            "arrival_abs_error_sum",
            "model_abs_error_sum",
            "no_change_abs_error_sum",
            "seasonal_naive_abs_error_sum",
            "mean_change_abs_error_sum",
        ] {
            assert!(
                value[field].is_string(),
                "{field} must be a decimal string, got {}",
                value[field]
            );
        }
        assert_eq!(
            value["model_abs_error_sum"],
            serde_json::json!("18446744073709551616")
        );
    }

    /// Regression: the brief's scenario totals and their delta crossed as JSON
    /// numbers. A reader parses a JSON number as an IEEE-754 double, so a
    /// backlog, resolution count, or staff-cost sum past 2^53 rounded in the
    /// dashboard while the gateway still held it exactly — the same split the
    /// forecast sums had. All eight fields are decimal strings now, and the
    /// delta is subtracted in `i128` before it is rendered, never in `f64`.
    #[test]
    fn kpi_and_delta_totals_are_decimal_strings() {
        const KPI_FIELDS: [&str; 4] = [
            "final_backlog",
            "total_resolved",
            "resolved_within_sla",
            "total_staff_cost_cents",
        ];
        let beyond_double = (u64::MAX as i128) + 1;
        let kpis = BriefKpis {
            final_backlog: u64::MAX.to_string(),
            // 2^53 + 1: the first integer an IEEE-754 double cannot represent.
            total_resolved: "9007199254740993".into(),
            resolved_within_sla: "0".into(),
            total_staff_cost_cents: u64::MAX.to_string(),
        };
        let delta = BriefDelta {
            final_backlog: (-beyond_double).to_string(),
            total_resolved: beyond_double.to_string(),
            resolved_within_sla: "0".into(),
            total_staff_cost_cents: beyond_double.to_string(),
        };
        let kpi_value = serde_json::to_value(&kpis).unwrap();
        let delta_value = serde_json::to_value(&delta).unwrap();
        for field in KPI_FIELDS {
            assert!(
                kpi_value[field].is_string(),
                "{field} must be a decimal string, got {}",
                kpi_value[field]
            );
            assert!(
                delta_value[field].is_string(),
                "delta {field} must be a decimal string, got {}",
                delta_value[field]
            );
        }
        assert_eq!(
            kpi_value["total_resolved"],
            serde_json::json!("9007199254740993")
        );
        assert_eq!(
            kpi_value["final_backlog"],
            serde_json::json!("18446744073709551615")
        );
        assert_eq!(
            delta_value["final_backlog"],
            serde_json::json!("-18446744073709551616")
        );
        // Exact round-trip: nothing is lost on the way back in.
        assert_eq!(serde_json::from_value::<BriefKpis>(kpi_value).unwrap(), kpis);
        assert_eq!(
            serde_json::from_value::<BriefDelta>(delta_value).unwrap(),
            delta
        );
    }

    /// Regression: the ticket-event KPIs and the p95 wait delta had the same
    /// JSON-number problem, and `null` must keep meaning "no resolved ticket"
    /// rather than collapsing into a zero string.
    #[test]
    fn event_kpis_and_p95_delta_are_decimal_strings() {
        let kpis = BriefEventKpis {
            final_backlog: u64::MAX.to_string(),
            total_resolved: "9007199254740993".into(),
            resolved_within_sla: "0".into(),
            total_staff_cost_cents: u64::MAX.to_string(),
            resolved_wait_seconds_p50: Some("3600".into()),
            resolved_wait_seconds_p95: None,
        };
        let value = serde_json::to_value(&kpis).unwrap();
        assert!(value["final_backlog"].is_string());
        assert_eq!(
            value["final_backlog"],
            serde_json::json!("18446744073709551615")
        );
        assert_eq!(value["resolved_wait_seconds_p50"], serde_json::json!("3600"));
        assert!(value["resolved_wait_seconds_p95"].is_null());
        assert_eq!(serde_json::from_value::<BriefEventKpis>(value).unwrap(), kpis);

        let delta: Option<String> = Some((-1800_i128).to_string());
        assert_eq!(
            serde_json::to_value(&delta).unwrap(),
            serde_json::json!("-1800")
        );
    }

    /// Regression: `exploratory_sla_holdout.diagnostic` embedded the *stored*
    /// `SlaHoldoutDiagnostic`, whose `u128` error sums serialize as JSON
    /// numbers — the one hashed structure in the brief that still crossed the
    /// wire as a double. It now carries the same wire mirror the engineering
    /// validation endpoint returns. This test pins the ordering that makes
    /// that safe: the digest is taken over the stored struct, and building the
    /// mirror afterwards cannot move it.
    #[test]
    fn sla_holdout_wire_mirror_is_built_after_the_stored_digest() {
        use sha2::{Digest, Sha256};

        // 2^53 + 1: the first integer an IEEE-754 double cannot represent,
        // and therefore the first value a browser silently rounds when it
        // arrives as a JSON number. It still fits `u64`, so `serde_json` will
        // happily emit it as one — which is exactly the defect.
        let beyond_double = (1_u128 << 53) + 1;
        let stored = crate::decision_ingest::SlaHoldoutDiagnostic {
            method: "ticket_sla_holdout".into(),
            source_sha256: "a".repeat(64),
            model_version: "v1".into(),
            training_days: 14,
            holdout_days: 7,
            sla_days: 2,
            provided_model_capacity_per_agent_day: 8,
            fitted_capacity_per_agent_day: 8,
            provided_model_matches_fit: true,
            observed_within_sla_by_day: vec![1, 2],
            predicted_within_sla_by_day: vec![1, 3],
            observed_total_within_sla: 3,
            predicted_total_within_sla: 4,
            daily_abs_error_sum: beyond_double,
            no_change_prediction_per_day: 1,
            seasonal_naive_prediction_by_day: vec![1, 1],
            training_mean_prediction_per_day: 1,
            no_change_abs_error_sum: 2,
            seasonal_naive_abs_error_sum: 3,
            training_mean_abs_error_sum: 4,
            conditional_model_beats_all_baselines: false,
            one_step_forecast: None,
            one_step_unavailable_reason: Some("not enough history".into()),
            limitations: vec!["exploratory".into()],
        };
        let digest_before = Sha256::digest(serde_json::to_vec(&stored).unwrap());
        // The stored form is what gets hashed, and it is still a JSON number.
        let stored_value = serde_json::to_value(&stored).unwrap();
        assert!(stored_value["daily_abs_error_sum"].is_number());

        let mirror = DecisionSlaHoldoutReport::from(stored.clone());
        let digest_after = Sha256::digest(serde_json::to_vec(&stored).unwrap());
        assert_eq!(
            digest_before, digest_after,
            "converting to the wire mirror must not touch the hashed payload"
        );

        let evidence = BriefSlaHoldoutEvidence {
            record_id: "sla-1".into(),
            record_sha256: format!("{digest_before:x}"),
            source_sha256: "a".repeat(64),
            engine_sha256: "b".repeat(64),
            diagnostic: mirror,
        };
        let value = serde_json::to_value(&evidence).unwrap();
        for field in [
            "daily_abs_error_sum",
            "no_change_abs_error_sum",
            "seasonal_naive_abs_error_sum",
            "training_mean_abs_error_sum",
        ] {
            assert!(
                value["diagnostic"][field].is_string(),
                "{field} must be a decimal string, got {}",
                value["diagnostic"][field]
            );
        }
        assert_eq!(
            value["diagnostic"]["daily_abs_error_sum"],
            serde_json::json!("9007199254740993")
        );
        // Bounded per-day vectors stay JSON numbers.
        assert!(value["diagnostic"]["observed_within_sla_by_day"][0].is_number());
        assert_eq!(
            serde_json::from_value::<BriefSlaHoldoutEvidence>(value).unwrap(),
            evidence
        );

        // Past `u64::MAX` the old shape was not merely lossy, it was
        // unrepresentable: `serde_json` refuses to emit such a `u128` as a
        // number at all. The mirror carries it exactly.
        let huge = crate::decision_ingest::SlaHoldoutDiagnostic {
            daily_abs_error_sum: (u64::MAX as u128) + 1,
            ..stored
        };
        assert!(serde_json::to_value(&huge).is_err());
        let value = serde_json::to_value(DecisionSlaHoldoutReport::from(huge)).unwrap();
        assert_eq!(
            value["daily_abs_error_sum"],
            serde_json::json!("18446744073709551616")
        );
    }
}
