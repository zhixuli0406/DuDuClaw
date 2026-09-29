//! Exploratory staffing choice under explicit resource and backlog limits.
//!
//! This sweep uses recorded arrivals. It is a deterministic stress check,
//! not a demand forecast, causal effect, or permission to change staffing.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_sensitivity::BoundedCount;
use crate::decision_sim::{
    DecisionSnapshot, QueueModel, SimulationError, StaffingScenario, simulate,
};
use crate::decision_store::{DecisionScope, DecisionStore, DecisionStoreError, StoredEmpiricalRun};

const MAX_CAPACITY_VALUES: u32 = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaffingResourcePlan {
    /// Staff actually available for each UTC day of the scenario horizon.
    pub available_agents_by_day: Vec<u32>,
    /// Maximum added agents above baseline on any one day.
    pub max_added_agents_per_day: u32,
    pub max_total_agent_days: u64,
    pub max_staff_cost_cents: u64,
    pub max_final_backlog: u64,
    /// Tested per-agent service capacities, inclusive and bounded.
    pub service_capacity_band: BoundedCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyChoice {
    Baseline,
    Alternative,
    NeitherFeasible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyPoint {
    pub service_capacity_per_agent_day: u32,
    pub baseline_final_backlog: u64,
    pub alternative_final_backlog: u64,
    pub baseline_staff_cost_cents: u64,
    pub alternative_staff_cost_cents: u64,
    pub baseline_feasible: bool,
    pub alternative_feasible: bool,
    pub choice: PolicyChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyFlip {
    pub at_capacity_per_agent_day: u32,
    pub from: PolicyChoice,
    pub to: PolicyChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySweepReport {
    pub status: String,
    pub replay_hash: String,
    pub snapshot_id: String,
    pub model_version: String,
    pub baseline_scenario_id: String,
    pub alternative_scenario_id: String,
    pub points: Vec<PolicyPoint>,
    pub flips: Vec<PolicyFlip>,
    pub decision_rule: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JointRiskScreenCriteria {
    /// Maximum share of common empirical draws violating any KPI constraint.
    pub max_joint_violation_bps: u32,
    /// Minimum share where the alternative resolves a baseline joint violation.
    pub min_joint_recovery_bps: u32,
    /// Minimum share where the alternative resolves more tickets within SLA.
    pub min_sla_improvement_bps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JointRiskScreenChoice {
    AlternativeForReview,
    BaselineOnly,
    NoCandidate,
}

/// Exact boundary values for the current alternative and the current shared
/// empirical draws. All adjustable limits must hold together; changing a KPI
/// cap requires rerunning the empirical comparison with that cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlternativeReviewThresholds {
    pub required_available_agents_by_day: Vec<u32>,
    pub required_max_added_agents_per_day: u32,
    pub required_max_total_agent_days: u64,
    pub required_max_staff_cost_cents: u64,
    pub required_max_final_backlog: u64,
    pub required_max_joint_violation_bps: u32,
    pub highest_min_joint_recovery_bps: u32,
    pub highest_min_sla_improvement_bps: u32,
    /// This fixed comparison cannot be repaired by relaxing a numeric limit.
    pub blocked_by_worse_joint_risk_than_baseline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JointRiskScreenReport {
    pub status: String,
    pub replay_hash: String,
    pub screen_engine_sha256: String,
    pub empirical_run_id: String,
    pub empirical_run_sha256: String,
    pub empirical_report_hash: String,
    pub policy_sweep: PolicySweepReport,
    pub resource_plan: StaffingResourcePlan,
    pub criteria: JointRiskScreenCriteria,
    pub model_capacity_per_agent_day: u32,
    pub baseline_deterministic_feasible: bool,
    pub alternative_deterministic_feasible: bool,
    pub baseline_joint_violation_bps: u32,
    pub alternative_joint_violation_bps: u32,
    pub alternative_joint_recovery_bps: u32,
    pub alternative_sla_improvement_bps: u32,
    pub failed_alternative_checks: Vec<String>,
    pub choice: JointRiskScreenChoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative_review_thresholds: Option<AlternativeReviewThresholds>,
    pub limitations: Vec<String>,
    /// 0 marks historical input-only hashes; v3 binds the displayed verdict.
    #[serde(default)]
    pub output_integrity_version: u8,
}

fn policy_screen_report_hash(report: &JointRiskScreenReport) -> Result<String, serde_json::Error> {
    let mut normalized = report.clone();
    normalized.replay_hash.clear();
    let bytes = serde_json::to_vec(&("support-joint-risk-screen-v3", normalized))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn legacy_policy_screen_hash(report: &JointRiskScreenReport) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(
        "support-joint-risk-screen-v2",
        &report.screen_engine_sha256,
        &report.empirical_run_sha256,
        &report.policy_sweep,
        &report.resource_plan,
        &report.criteria,
        &report.alternative_review_thresholds,
    ))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn policy_screen_hash_matches(
    report: &JointRiskScreenReport,
) -> Result<bool, serde_json::Error> {
    match report.output_integrity_version {
        3 => Ok(policy_screen_report_hash(report)? == report.replay_hash),
        0 => Ok(legacy_policy_screen_hash(report)? == report.replay_hash),
        _ => Ok(false),
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicySweepError {
    #[error("invalid or unsupported policy sweep input")]
    Invalid,
    #[error("policy sweep serialization failed")]
    Serialization,
    #[error("simulation failed: {0}")]
    Simulation(#[from] SimulationError),
}

fn within_resource_limits(
    scenario: &StaffingScenario,
    baseline: &StaffingScenario,
    plan: &StaffingResourcePlan,
) -> Result<bool, PolicySweepError> {
    let mut total = 0_u64;
    for (day, &agents) in scenario.agents_by_day.iter().enumerate() {
        total = total
            .checked_add(agents as u64)
            .ok_or(PolicySweepError::Invalid)?;
        if agents > plan.available_agents_by_day[day]
            || agents.saturating_sub(baseline.agents_by_day[day]) > plan.max_added_agents_per_day
        {
            return Ok(false);
        }
    }
    Ok(total <= plan.max_total_agent_days)
}

/// Rank feasible schedules by lower staffing cost, then lower final backlog;
/// ties keep the baseline. A schedule is feasible only when resource, cost,
/// and backlog limits all hold. A failed limit yields no recommendation.
pub fn sweep_staffing_policy(
    snapshot: &DecisionSnapshot,
    model: &QueueModel,
    baseline: &StaffingScenario,
    alternative: &StaffingScenario,
    plan: &StaffingResourcePlan,
) -> Result<PolicySweepReport, PolicySweepError> {
    let horizon = snapshot.arrivals_by_day.len();
    if baseline.id == alternative.id
        || horizon == 0
        || plan.available_agents_by_day.len() != horizon
        || baseline.agents_by_day.len() != horizon
        || alternative.agents_by_day.len() != horizon
        || baseline.fixed_extra_capacity_by_day.len() != horizon
        || alternative.fixed_extra_capacity_by_day.len() != horizon
        || baseline.fixed_extra_capacity_by_day.iter().any(|&n| n != 0)
        || alternative
            .fixed_extra_capacity_by_day
            .iter()
            .any(|&n| n != 0)
        || plan.service_capacity_band.min == 0
        || plan.service_capacity_band.min > plan.service_capacity_band.max
        || plan.service_capacity_band.max - plan.service_capacity_band.min >= MAX_CAPACITY_VALUES
    {
        return Err(PolicySweepError::Invalid);
    }
    let baseline_resources = within_resource_limits(baseline, baseline, plan)?;
    let alternative_resources = within_resource_limits(alternative, baseline, plan)?;
    let replay_bytes = serde_json::to_vec(&(
        "support-staffing-policy-sweep-v1",
        snapshot,
        model,
        baseline,
        alternative,
        plan,
    ))
    .map_err(|_| PolicySweepError::Serialization)?;
    let replay_hash = format!("{:x}", Sha256::digest(replay_bytes));
    let mut points = Vec::with_capacity(
        (plan.service_capacity_band.max - plan.service_capacity_band.min + 1) as usize,
    );
    let mut flips = Vec::new();
    for capacity in plan.service_capacity_band.min..=plan.service_capacity_band.max {
        let mut trial_model = model.clone();
        trial_model.service_capacity_per_agent_day = capacity;
        let base = simulate(snapshot, &trial_model, baseline)?;
        let alt = simulate(snapshot, &trial_model, alternative)?;
        let base_feasible = baseline_resources
            && base.total_staff_cost_cents <= plan.max_staff_cost_cents
            && base.final_backlog <= plan.max_final_backlog;
        let alt_feasible = alternative_resources
            && alt.total_staff_cost_cents <= plan.max_staff_cost_cents
            && alt.final_backlog <= plan.max_final_backlog;
        let choice = match (base_feasible, alt_feasible) {
            (false, false) => PolicyChoice::NeitherFeasible,
            (true, false) => PolicyChoice::Baseline,
            (false, true) => PolicyChoice::Alternative,
            (true, true) => {
                if alt.total_staff_cost_cents < base.total_staff_cost_cents
                    || (alt.total_staff_cost_cents == base.total_staff_cost_cents
                        && alt.final_backlog < base.final_backlog)
                {
                    PolicyChoice::Alternative
                } else {
                    PolicyChoice::Baseline
                }
            }
        };
        if let Some(previous) = points.last() {
            let previous: &PolicyPoint = previous;
            if previous.choice != choice {
                flips.push(PolicyFlip {
                    at_capacity_per_agent_day: capacity,
                    from: previous.choice,
                    to: choice,
                });
            }
        }
        points.push(PolicyPoint {
            service_capacity_per_agent_day: capacity,
            baseline_final_backlog: base.final_backlog,
            alternative_final_backlog: alt.final_backlog,
            baseline_staff_cost_cents: base.total_staff_cost_cents,
            alternative_staff_cost_cents: alt.total_staff_cost_cents,
            baseline_feasible: base_feasible,
            alternative_feasible: alt_feasible,
            choice,
        });
    }
    Ok(PolicySweepReport {
        status: "exploratory".into(),
        replay_hash,
        snapshot_id: snapshot.id.clone(),
        model_version: model.version.clone(),
        baseline_scenario_id: baseline.id.clone(),
        alternative_scenario_id: alternative.id.clone(),
        points,
        flips,
        decision_rule: "Feasible schedules meet daily staffing, added-agent, total-agent-day, staff-cost, and final-backlog limits; choose lower staff cost, then lower final backlog, then baseline on a tie".into(),
        limitations: vec![
            "Capacity sweep holds recorded demand fixed and is not a calibrated forecast".into(),
            "Unpriced fixed extra capacity is unsupported by this decision rule".into(),
            "Resource availability and limits are operator inputs, not inferred facts".into(),
        ],
    })
}

impl DecisionStore {
    /// Load every input from one exact tenant/ACL scope before running a
    /// read-only sweep. Revoked source snapshots cannot be replayed here.
    pub fn sweep_staffing_policy(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        plan: &StaffingResourcePlan,
    ) -> Result<PolicySweepReport, DecisionStoreError> {
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", baseline_id)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", alternative_id)?;
        let report = sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, plan)?;
        self.verify_simulation_inputs_still_current(
            scope,
            &snapshot,
            &model,
            &[&baseline, &alternative],
        )?;
        Ok(report)
    }

    /// Screen a fully revalidated empirical run against one explicit set of
    /// staffing and KPI limits. Passing creates a review candidate only.
    pub fn screen_empirical_policy(
        &self,
        scope: &DecisionScope,
        run_id: &str,
        resource_plan: &StaffingResourcePlan,
        criteria: &JointRiskScreenCriteria,
    ) -> Result<JointRiskScreenReport, DecisionStoreError> {
        if criteria.max_joint_violation_bps > 10_000
            || criteria.min_joint_recovery_bps > 10_000
            || criteria.min_sla_improvement_bps > 10_000
        {
            return Err(DecisionStoreError::Invalid);
        }
        let run = self.load_empirical_run(scope, run_id)?;
        if run.plan.min_sla_resolved.is_none()
            || run.plan.max_final_backlog != resource_plan.max_final_backlog
            || run.plan.max_staff_cost_cents != resource_plan.max_staff_cost_cents
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (_, empirical_run_sha256): (StoredEmpiricalRun, String) =
            self.get_with_digest(scope, "empirical_run", run_id)?;
        let (model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", &run.model_version)?;
        if model_sha256 != run.model_sha256 {
            return Err(DecisionStoreError::Corrupt);
        }
        let policy_sweep = self.sweep_staffing_policy(
            scope,
            &run.snapshot_id,
            &run.model_version,
            &run.baseline_id,
            &run.alternative_id,
            resource_plan,
        )?;
        let point = policy_sweep
            .points
            .iter()
            .find(|point| {
                point.service_capacity_per_agent_day == model.service_capacity_per_agent_day
            })
            .ok_or(DecisionStoreError::Invalid)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", &run.alternative_id)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", &run.baseline_id)?;
        let required_max_total_agent_days = alternative
            .agents_by_day
            .iter()
            .try_fold(0_u64, |total, &agents| total.checked_add(agents as u64))
            .ok_or(DecisionStoreError::Invalid)?;
        let alternative_review_thresholds = AlternativeReviewThresholds {
            required_available_agents_by_day: alternative.agents_by_day.clone(),
            required_max_added_agents_per_day: alternative
                .agents_by_day
                .iter()
                .zip(&baseline.agents_by_day)
                .map(|(&alternative, &baseline)| alternative.saturating_sub(baseline))
                .max()
                .unwrap_or(0),
            required_max_total_agent_days,
            required_max_staff_cost_cents: point.alternative_staff_cost_cents,
            required_max_final_backlog: point.alternative_final_backlog,
            required_max_joint_violation_bps: run.report.alternative_joint_constraint_violation_bps,
            highest_min_joint_recovery_bps: run.report.alternative_joint_constraint_recovery_bps,
            highest_min_sla_improvement_bps: run.report.alternative_more_sla_resolved_bps,
            blocked_by_worse_joint_risk_than_baseline: run
                .report
                .alternative_joint_constraint_violation_bps
                > run.report.baseline_joint_constraint_violation_bps,
        };
        let baseline_deterministic_feasible = point.baseline_feasible;
        let alternative_deterministic_feasible = point.alternative_feasible;
        let mut failed = Vec::new();
        if !point.alternative_feasible {
            failed.push("alternative_resource_or_deterministic_limit".into());
        }
        if run.report.alternative_joint_constraint_violation_bps > criteria.max_joint_violation_bps
        {
            failed.push("alternative_joint_violation_limit".into());
        }
        if run.report.alternative_joint_constraint_violation_bps
            > run.report.baseline_joint_constraint_violation_bps
        {
            failed.push("alternative_joint_risk_worse_than_baseline".into());
        }
        if run.report.alternative_joint_constraint_recovery_bps < criteria.min_joint_recovery_bps {
            failed.push("insufficient_paired_joint_recovery".into());
        }
        if run.report.alternative_more_sla_resolved_bps < criteria.min_sla_improvement_bps {
            failed.push("insufficient_paired_sla_improvement".into());
        }
        let choice = if failed.is_empty() {
            JointRiskScreenChoice::AlternativeForReview
        } else if point.baseline_feasible
            && run.report.baseline_joint_constraint_violation_bps
                <= criteria.max_joint_violation_bps
        {
            JointRiskScreenChoice::BaselineOnly
        } else {
            JointRiskScreenChoice::NoCandidate
        };
        let screen_engine_sha256 = format!(
            "{:x}",
            Sha256::digest(include_str!("decision_policy.rs").as_bytes())
        );
        let mut report = JointRiskScreenReport {
            status: "exploratory_review_screen".into(),
            replay_hash: String::new(),
            screen_engine_sha256,
            empirical_run_id: run.id,
            empirical_run_sha256,
            empirical_report_hash: run.report.replay_hash,
            policy_sweep,
            resource_plan: resource_plan.clone(),
            criteria: criteria.clone(),
            model_capacity_per_agent_day: model.service_capacity_per_agent_day,
            baseline_deterministic_feasible,
            alternative_deterministic_feasible,
            baseline_joint_violation_bps: run.report.baseline_joint_constraint_violation_bps,
            alternative_joint_violation_bps: run.report.alternative_joint_constraint_violation_bps,
            alternative_joint_recovery_bps: run.report.alternative_joint_constraint_recovery_bps,
            alternative_sla_improvement_bps: run.report.alternative_more_sla_resolved_bps,
            failed_alternative_checks: failed,
            choice,
            alternative_review_thresholds: Some(alternative_review_thresholds),
            limitations: vec![
                "Empirical draw fractions are exploratory and lack real held-out calibration".into(),
                "A passing screen is only a candidate for human review; it authorizes no staffing action".into(),
                "Thresholds describe this fixed scenario and draw set; changing backlog or cost caps requires a new empirical run".into(),
            ],
            output_integrity_version: 3,
        };
        report.replay_hash = policy_screen_report_hash(&report)?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        DecisionSnapshot,
        QueueModel,
        StaffingScenario,
        StaffingScenario,
        StaffingResourcePlan,
    ) {
        let snapshot = DecisionSnapshot {
            id: "pilot".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["tickets-v1".into()],
            seed: 17,
            arrivals_by_day: vec![5; 3],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 3,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let baseline = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1; 3],
            fixed_extra_capacity_by_day: vec![0; 3],
        };
        let alternative = StaffingScenario {
            id: "extra".into(),
            agents_by_day: vec![2; 3],
            fixed_extra_capacity_by_day: vec![0; 3],
        };
        let plan = StaffingResourcePlan {
            available_agents_by_day: vec![2; 3],
            max_added_agents_per_day: 1,
            max_total_agent_days: 6,
            max_staff_cost_cents: 1_000,
            max_final_backlog: 3,
            service_capacity_band: BoundedCount { min: 2, max: 5 },
        };
        (snapshot, model, baseline, alternative, plan)
    }

    #[test]
    fn capacity_threshold_flips_from_alternative_to_cheaper_baseline() {
        let (snapshot, model, baseline, alternative, plan) = fixture();
        let report =
            sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, &plan).unwrap();
        assert_eq!(
            report
                .points
                .iter()
                .map(|point| point.choice)
                .collect::<Vec<_>>(),
            vec![
                PolicyChoice::Alternative,
                PolicyChoice::Alternative,
                PolicyChoice::Baseline,
                PolicyChoice::Baseline,
            ]
        );
        assert_eq!(
            report.flips,
            vec![PolicyFlip {
                at_capacity_per_agent_day: 4,
                from: PolicyChoice::Alternative,
                to: PolicyChoice::Baseline,
            }]
        );
        assert_eq!(
            report,
            sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, &plan).unwrap()
        );
    }

    #[test]
    fn resource_or_budget_violation_prevents_an_alternative_recommendation() {
        let (snapshot, model, baseline, alternative, mut plan) = fixture();
        plan.available_agents_by_day = vec![1; 3];
        let report =
            sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, &plan).unwrap();
        assert!(
            report
                .points
                .iter()
                .all(|point| !point.alternative_feasible)
        );
        assert_eq!(report.points[0].choice, PolicyChoice::NeitherFeasible);
        assert_eq!(report.points[2].choice, PolicyChoice::Baseline);
        plan.available_agents_by_day = vec![2; 3];
        plan.max_staff_cost_cents = 400;
        let report =
            sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, &plan).unwrap();
        assert!(
            report
                .points
                .iter()
                .all(|point| !point.alternative_feasible)
        );
        assert!(
            report
                .points
                .iter()
                .all(|point| point.alternative_staff_cost_cents == 600)
        );
    }

    #[test]
    fn current_screen_hash_binds_verdict_and_legacy_input_hash_is_identified() {
        let (snapshot, model, baseline, alternative, plan) = fixture();
        let sweep =
            sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, &plan).unwrap();
        let mut report = JointRiskScreenReport {
            status: "exploratory_review_screen".into(),
            replay_hash: String::new(),
            screen_engine_sha256: "engine".into(),
            empirical_run_id: "run".into(),
            empirical_run_sha256: "run-digest".into(),
            empirical_report_hash: "report-digest".into(),
            policy_sweep: sweep,
            resource_plan: plan,
            criteria: JointRiskScreenCriteria {
                max_joint_violation_bps: 2_000,
                min_joint_recovery_bps: 1_000,
                min_sla_improvement_bps: 1_000,
            },
            model_capacity_per_agent_day: model.service_capacity_per_agent_day,
            baseline_deterministic_feasible: true,
            alternative_deterministic_feasible: true,
            baseline_joint_violation_bps: 2_000,
            alternative_joint_violation_bps: 1_000,
            alternative_joint_recovery_bps: 1_500,
            alternative_sla_improvement_bps: 2_000,
            failed_alternative_checks: vec![],
            choice: JointRiskScreenChoice::AlternativeForReview,
            alternative_review_thresholds: None,
            limitations: vec!["synthetic".into()],
            output_integrity_version: 3,
        };
        report.replay_hash = policy_screen_report_hash(&report).unwrap();
        assert!(policy_screen_hash_matches(&report).unwrap());
        let mut changed = report.clone();
        changed.choice = JointRiskScreenChoice::NoCandidate;
        assert!(!policy_screen_hash_matches(&changed).unwrap());
        changed = report.clone();
        changed.failed_alternative_checks.push("forged".into());
        assert!(!policy_screen_hash_matches(&changed).unwrap());
        let mut historical = report;
        historical.output_integrity_version = 0;
        historical.replay_hash = legacy_policy_screen_hash(&historical).unwrap();
        let mut json = serde_json::to_value(&historical).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("output_integrity_version");
        let restored: JointRiskScreenReport = serde_json::from_value(json).unwrap();
        assert_eq!(restored.output_integrity_version, 0);
        assert!(policy_screen_hash_matches(&restored).unwrap());
    }

    #[test]
    fn unpriced_extra_capacity_and_unbounded_sweeps_are_rejected() {
        let (snapshot, model, baseline, mut alternative, mut plan) = fixture();
        alternative.fixed_extra_capacity_by_day[0] = 1;
        assert_eq!(
            sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, &plan),
            Err(PolicySweepError::Invalid)
        );
        alternative.fixed_extra_capacity_by_day[0] = 0;
        plan.service_capacity_band.max = 1_000;
        assert_eq!(
            sweep_staffing_policy(&snapshot, &model, &baseline, &alternative, &plan),
            Err(PolicySweepError::Invalid)
        );
    }

    #[test]
    fn scoped_store_sweep_fails_after_source_revocation() {
        let (snapshot, model, baseline, alternative, plan) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("policy.sqlite"));
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &baseline).unwrap();
        store.put_scenario(&scope, &alternative).unwrap();
        let report = store
            .sweep_staffing_policy(
                &scope,
                &snapshot.id,
                &model.version,
                &baseline.id,
                &alternative.id,
                &plan,
            )
            .unwrap();
        assert_eq!(report.points[0].choice, PolicyChoice::Alternative);
        let other = DecisionScope {
            tenant_id: "tenant-b".into(),
            acl: "private".into(),
        };
        assert!(matches!(
            store.sweep_staffing_policy(
                &other,
                &snapshot.id,
                &model.version,
                &baseline.id,
                &alternative.id,
                &plan,
            ),
            Err(DecisionStoreError::NotFound)
        ));
        store
            .revoke_source_version(&scope, &snapshot.source_version_hashes[0])
            .unwrap();
        assert!(matches!(
            store.sweep_staffing_policy(
                &scope,
                &snapshot.id,
                &model.version,
                &baseline.id,
                &alternative.id,
                &plan,
            ),
            Err(DecisionStoreError::Revoked)
        ));
    }
}
