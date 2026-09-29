//! A reproducible observational fixture for the limited binary-treatment
//! estimator. It seeds candidates only; human reviews are never fabricated.

use std::collections::BTreeMap;
use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_memory::causal::{
    CausalStore, ClaimModality, EvidenceScope, EvidenceStance, ProposedCausalClaim,
};
use duduclaw_memory::causal_effect::{
    EffectResult, NegativeControlPlan, ObservedDataset, ObservedUnit,
};
use duduclaw_memory::causal_effect_eval::{
    EffectEvalCase, EffectEvalDataset, EffectEvalReport, ExpectedDiagnosticFlags, evaluate_effects,
};
use duduclaw_memory::causal_model::{ModelDraft, VariableKind};
use duduclaw_memory::causal_negative_control::NegativeControlReadiness;
use serde::Serialize;

const VERSION: &str = "synthetic-observational-v1";
const PROTOCOL: &str = "Synthetic DGP protocol: audit_count is generated from queue_cohort alone. Extra staffing cannot change audit_count. Queue cohort changes both staffing assignment propensity and audit_count, so adjustment for cohort is required before comparing treated and untreated units. This synthetic exclusion rule does not validate a real support-system negative control.";

const RULES: [(&str, &str, &str, &str); 4] = [
    (
        "cohort-staffing",
        "queue_cohort",
        "extra_staffing",
        "Synthetic DGP: queue_cohort changes the chance of receiving extra_staffing.",
    ),
    (
        "cohort-wait",
        "queue_cohort",
        "wait_hours",
        "Synthetic DGP: queue_cohort changes wait_hours before staffing intervention.",
    ),
    (
        "staffing-wait",
        "extra_staffing",
        "wait_hours",
        "Synthetic DGP: extra_staffing reduces wait_hours by exactly two hours for each unit.",
    ),
    (
        "cohort-audit",
        "queue_cohort",
        "audit_count",
        "Synthetic DGP: queue_cohort changes audit_count independently of extra_staffing.",
    ),
];

#[derive(Debug, Serialize)]
struct ObservationalDemoReport {
    scope: EvidenceScope,
    model_id: String,
    candidate_claim_ids: Vec<String>,
    cohort_variable_id: String,
    negative_control_variable_id: String,
    protocol_artifact_id: String,
    dataset_file: String,
    dataset_units: usize,
    known_synthetic_effect: f64,
    negative_control_review_id: Option<String>,
    evaluation_manifest_file: Option<String>,
    evaluation_estimate_ids: Option<[String; 2]>,
    evaluation_report: Option<EffectEvalReport>,
    next_steps: Vec<&'static str>,
}

fn demo_error(error: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(format!("causal observational demo: {error}"))
}

fn write_new_or_equal(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        let existing = std::fs::read(path).map_err(demo_error)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(DuDuClawError::Config(format!(
            "dataset file already exists with different content: {}",
            path.display(),
        )));
    }
    std::fs::write(path, bytes).map_err(demo_error)
}

fn dataset(cohort_id: &str, control_id: &str, review_id: Option<&str>) -> ObservedDataset {
    let units = (0..80)
        .map(|index| {
            let high = index >= 40;
            let within = index % 20;
            let period = (index % 40) / 20;
            let treated = within < if high { 15 } else { 5 };
            let cohort = if high { "high" } else { "low" };
            let baseline = if high { 14.0 } else { 10.0 };
            let treatment_at = if period == 0 { 20 } else { 60 };
            ObservedUnit {
                unit_id: format!("synthetic-{index:03}"),
                adjustment_measured_at: Some(treatment_at - 10),
                adjustment_values: BTreeMap::from([(cohort_id.into(), cohort.into())]),
                treatment_assigned_at: treatment_at,
                outcome_recorded_at: treatment_at + 10,
                treated,
                outcome: baseline - if treated { 2.0 } else { 0.0 },
                pre_treatment_outcome: Some(baseline),
                pre_treatment_outcome_recorded_at: Some(treatment_at - 5),
                negative_control_outcome: review_id.map(|_| if high { 8.0 } else { 5.0 }),
                negative_control_recorded_at: review_id.map(|_| treatment_at + 5),
                stratum: cohort.into(),
            }
        })
        .collect();
    ObservedDataset {
        adjustment_variable_ids: vec![cohort_id.into()],
        evaluation_cutoff: Some(50),
        negative_control: review_id.map(|id| NegativeControlPlan {
            variable_id: control_id.into(),
            review_id: id.into(),
        }),
        units,
    }
}

fn build_evaluation(
    store: &CausalStore,
    scope: &EvidenceScope,
    model_id: &str,
    combined: &ObservedDataset,
    review_id: &str,
    manifest_path: &Path,
) -> Result<([String; 2], EffectEvalReport)> {
    let mut estimate_ids = Vec::new();
    for (name, treatment_at, export_at) in [("training", 20, 40), ("heldout", 60, 80)] {
        let split = ObservedDataset {
            adjustment_variable_ids: combined.adjustment_variable_ids.clone(),
            evaluation_cutoff: None,
            negative_control: combined.negative_control.clone(),
            units: combined
                .units
                .iter()
                .filter(|unit| unit.treatment_assigned_at == treatment_at)
                .cloned()
                .collect(),
        };
        if split.units.len() != 40 {
            return Err(DuDuClawError::Config(
                "synthetic evaluation split is incomplete".into(),
            ));
        }
        let content = serde_json::to_string(&split).map_err(demo_error)?;
        let artifact = store
            .add_artifact(
                scope,
                "causal_dataset",
                &format!("synthetic-observational-{name}"),
                &format!("{VERSION}:{review_id}"),
                &format!("synthetic-observational-{name}-lineage"),
                &content,
                export_at,
                i64::MAX,
            )
            .map_err(demo_error)?;
        let estimate = match store
            .estimate_stratified_effect(scope, model_id, &artifact.id)
            .map_err(demo_error)?
        {
            EffectResult::Estimated(value) => value,
            EffectResult::Unknown { reasons } => {
                return Err(DuDuClawError::Config(format!(
                    "synthetic {name} estimate is unknown: {}",
                    reasons.join("; "),
                )));
            }
        };
        estimate_ids.push(estimate.id);
    }
    let estimate_ids: [String; 2] = estimate_ids
        .try_into()
        .map_err(|_| DuDuClawError::Config("synthetic evaluation estimates missing".into()))?;
    let manifest = EffectEvalDataset {
        version: "causal-effect-eval-v1".into(),
        truth_basis: "synthetic_dgp".into(),
        model_id: model_id.into(),
        cutoff_unix: 50,
        cases: vec![
            EffectEvalCase {
                id: "synthetic-training".into(),
                estimate_id: estimate_ids[0].clone(),
                true_effect: -2.0,
                expected_diagnostics: None,
            },
            EffectEvalCase {
                id: "synthetic-heldout".into(),
                estimate_id: estimate_ids[1].clone(),
                true_effect: -2.0,
                expected_diagnostics: Some(ExpectedDiagnosticFlags {
                    unit_deletion_sign_flip: Some(false),
                    temporal_sign_flip: None,
                    pre_treatment_imbalance: Some(false),
                    negative_control_imbalance: Some(false),
                }),
            },
        ],
    };
    let report = evaluate_effects(store, scope, &manifest).map_err(demo_error)?;
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(demo_error)?;
    write_new_or_equal(manifest_path, &bytes)?;
    Ok((estimate_ids, report))
}

fn build(
    db: &Path,
    tenant: &str,
    acl: &str,
    output: &Path,
    review_id: Option<&str>,
    evaluation_manifest: Option<&Path>,
) -> Result<ObservationalDemoReport> {
    if tenant.trim().is_empty() || acl.trim().is_empty() || output.as_os_str().is_empty() {
        return Err(DuDuClawError::Config(
            "tenant, ACL, and output path are required".into(),
        ));
    }
    if evaluation_manifest.is_some() && review_id.is_none() {
        return Err(DuDuClawError::Config(
            "--evaluation-manifest requires a current --review-id".into(),
        ));
    }
    if evaluation_manifest.is_some_and(|path| path == output) {
        return Err(DuDuClawError::Config(
            "dataset and evaluation manifest must use different output paths".into(),
        ));
    }
    let scope = EvidenceScope {
        tenant_id: tenant.into(),
        acl: acl.into(),
    };
    let store = CausalStore::new(db);
    let treatment = store
        .register_variable(
            &scope,
            "extra_staffing",
            VERSION,
            "Binary assignment of extra support staffing",
            "yes/no",
            VariableKind::Binary,
        )
        .map_err(demo_error)?;
    let outcome = store
        .register_variable(
            &scope,
            "wait_hours",
            VERSION,
            "Wait until synthetic ticket resolution",
            "hours",
            VariableKind::Continuous,
        )
        .map_err(demo_error)?;
    let cohort = store
        .register_variable(
            &scope,
            "queue_cohort",
            VERSION,
            "Pre-assignment synthetic queue cohort",
            "category",
            VariableKind::Categorical,
        )
        .map_err(demo_error)?;
    let control = store
        .register_variable(
            &scope,
            "audit_count",
            VERSION,
            "Synthetic audit count excluded from staffing effect",
            "count",
            VariableKind::Count,
        )
        .map_err(demo_error)?;

    let mut claim_ids = Vec::new();
    for (external_id, cause, effect, content) in RULES {
        let source = store
            .add_artifact(
                &scope,
                "synthetic_dgp_rule",
                external_id,
                VERSION,
                external_id,
                content,
                1,
                i64::MAX,
            )
            .map_err(demo_error)?;
        let (claim, _) = store
            .ingest_extracted_claim(
                &scope,
                &source.id,
                "What rules generated this synthetic observational cohort?",
                VERSION,
                &ProposedCausalClaim {
                    cause_variable: cause.into(),
                    effect_variable: effect.into(),
                    lag_min_seconds: 0,
                    lag_max_seconds: 100,
                    modality: ClaimModality::Asserted,
                    stance: EvidenceStance::Supports,
                    span_start: 0,
                    span_end: content.len(),
                    excerpt: content.into(),
                    speaker_id: Some("synthetic-generator".into()),
                    context: serde_json::json!({ "synthetic": true, "version": VERSION }),
                },
            )
            .map_err(demo_error)?;
        claim_ids.push(claim.id);
    }
    let protocol = store
        .add_artifact(
            &scope,
            "negative_control_protocol",
            "audit-exclusion",
            VERSION,
            "synthetic-audit-exclusion",
            PROTOCOL,
            1,
            i64::MAX,
        )
        .map_err(demo_error)?;
    let model_id = if let Some(existing) = store
        .list_model_ids(&scope, 100)
        .map_err(demo_error)?
        .into_iter()
        .map(|id| store.read_model(&scope, &id).map_err(demo_error))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|model| model.name == "synthetic-support-observational" && model.version == VERSION)
    {
        existing.id
    } else {
        store
            .create_model(
                &scope,
                &ModelDraft {
                    name: "synthetic-support-observational".into(),
                    version: VERSION.into(),
                    treatment_variable_id: treatment.id.clone(),
                    outcome_variable_id: outcome.id.clone(),
                    population: "80 synthetic support units in two assignment periods".into(),
                    window_start: 0,
                    window_end: 100,
                    variable_ids: vec![
                        treatment.id,
                        outcome.id,
                        cohort.id.clone(),
                        control.id.clone(),
                    ],
                    claim_ids: claim_ids.clone(),
                },
            )
            .map_err(demo_error)?
            .id
    };
    if let Some(id) = review_id {
        if !matches!(
            store
                .negative_control_readiness(&scope, &model_id, &control.id, id)
                .map_err(demo_error)?,
            NegativeControlReadiness::Reviewed { .. }
        ) {
            return Err(DuDuClawError::Config(
                "negative-control review must be current, passing, and source-backed".into(),
            ));
        }
    }
    let data = dataset(&cohort.id, &control.id, review_id);
    let bytes = serde_json::to_vec_pretty(&data).map_err(demo_error)?;
    write_new_or_equal(output, &bytes)?;
    let evaluated = match (evaluation_manifest, review_id) {
        (Some(path), Some(id)) => Some(build_evaluation(
            &store, &scope, &model_id, &data, id, path,
        )?),
        _ => None,
    };
    Ok(ObservationalDemoReport {
        scope,
        model_id,
        candidate_claim_ids: claim_ids,
        cohort_variable_id: cohort.id,
        negative_control_variable_id: control.id,
        protocol_artifact_id: protocol.id,
        dataset_file: output.display().to_string(),
        dataset_units: data.units.len(),
        known_synthetic_effect: -2.0,
        negative_control_review_id: review_id.map(str::to_owned),
        evaluation_manifest_file: evaluation_manifest.map(|path| path.display().to_string()),
        evaluation_estimate_ids: evaluated.as_ref().map(|(ids, _)| ids.clone()),
        evaluation_report: evaluated.map(|(_, report)| report),
        next_steps: vec![
            "Review candidate claims, the draft model, and six assumptions in the admin curation page; the generator does not approve them.",
            "Review the negative-control exclusion protocol in the same scope, then rerun with --review-id and a new output path to include control observations.",
            "Submit the JSON dataset through the admin effect panel; estimates remain observational and uncalibrated.",
            "An optional evaluation manifest scores separate synthetic pre/post-cutoff cohorts; it does not measure real-data calibration.",
        ],
    })
}

pub fn demo(
    db: &Path,
    tenant: &str,
    acl: &str,
    output: &Path,
    review_id: Option<&str>,
    evaluation_manifest: Option<&Path>,
) -> Result<()> {
    let report = build(db, tenant, acl, output, review_id, evaluation_manifest)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(demo_error)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_gateway::decision_sim::{DecisionSnapshot, QueueModel, StaffingScenario};
    use duduclaw_gateway::decision_store::{DecisionScope, DecisionStore};
    use duduclaw_memory::causal_model::{AssumptionKind, AssumptionVerdict};

    #[test]
    fn fixture_requires_real_review_then_estimates_known_synthetic_effect() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let base_path = dir.path().join("base.json");
        let first = build(&db, "synthetic", "private", &base_path, None, None).unwrap();
        let second = build(&db, "synthetic", "private", &base_path, None, None).unwrap();
        assert_eq!(first.model_id, second.model_id);
        assert_eq!(first.candidate_claim_ids, second.candidate_claim_ids);
        let store = CausalStore::new(&db);
        for id in &first.candidate_claim_ids {
            assert_eq!(store.claim_state(&first.scope, id).unwrap(), "candidate");
        }
        assert_eq!(
            store.model_state(&first.scope, &first.model_id).unwrap(),
            "draft"
        );
        let base: ObservedDataset =
            serde_json::from_slice(&std::fs::read(&base_path).unwrap()).unwrap();
        assert_eq!(base.units.len(), 80);
        assert!(base.negative_control.is_none());
        assert!(
            base.units
                .iter()
                .all(|unit| unit.negative_control_outcome.is_none())
        );
        assert!(
            build(
                &db,
                "synthetic",
                "private",
                &dir.path().join("nco-before.json"),
                Some("missing"),
                None
            )
            .is_err()
        );
        assert!(
            build(
                &db,
                "synthetic",
                "private",
                &dir.path().join("base-with-eval.json"),
                None,
                Some(&dir.path().join("premature-eval.json"))
            )
            .is_err()
        );
        for id in &first.candidate_claim_ids {
            store
                .review_claim(&first.scope, id, "human-reviewer", true)
                .unwrap();
        }
        store
            .review_model(&first.scope, &first.model_id, "human-reviewer", true, false)
            .unwrap();
        for kind in AssumptionKind::ALL {
            store
                .record_assumption(
                    &first.scope,
                    &first.model_id,
                    kind,
                    AssumptionVerdict::Pass,
                    "Reviewed for synthetic engineering fixture",
                    "human-reviewer",
                )
                .unwrap();
        }
        let review_id = store
            .review_negative_control(
                &first.scope,
                &first.model_id,
                &first.negative_control_variable_id,
                &first.protocol_artifact_id,
                AssumptionVerdict::Pass,
                "Synthetic generator explicitly excludes the staffing effect on audit count",
                "human-reviewer",
            )
            .unwrap();
        let nco_path = dir.path().join("with-control.json");
        let manifest_path = dir.path().join("evaluation.json");
        let completed = build(
            &db,
            "synthetic",
            "private",
            &nco_path,
            Some(&review_id),
            Some(&manifest_path),
        )
        .unwrap();
        assert_eq!(
            completed.negative_control_review_id.as_deref(),
            Some(review_id.as_str())
        );
        let completed_again = build(
            &db,
            "synthetic",
            "private",
            &nco_path,
            Some(&review_id),
            Some(&manifest_path),
        )
        .unwrap();
        assert_eq!(completed.model_id, completed_again.model_id);
        let evaluation: EffectEvalDataset =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(evaluation.cutoff_unix, 50);
        assert_eq!(evaluation.cases.len(), 2);
        let report = completed.evaluation_report.unwrap();
        assert_eq!(
            (
                report.training_cases,
                report.held_out_cases,
                report.scored_cases
            ),
            (1, 1, 1)
        );
        assert_eq!(report.mean_absolute_error, Some(0.0));
        assert_eq!(report.interval_coverage, Some(1.0));
        assert_eq!(report.interval_calibration.scored_cases, 1);
        assert_eq!(report.interval_calibration.covered_cases, 1);
        assert_eq!(report.interval_calibration.mean_interval_width, Some(0.0));
        assert_eq!(
            report.interval_calibration.mean_interval_score,
            report.interval_calibration.mean_interval_width
        );
        let cli_report = serde_json::to_value(&report).unwrap();
        assert_eq!(
            cli_report["interval_calibration"]["status"],
            "insufficient_held_out_cases"
        );
        assert_eq!(report.negative_control_imbalance_flags, 0);
        assert_eq!(report.negative_control_detection.true_negative, 1);
        let completed_data: ObservedDataset =
            serde_json::from_slice(&std::fs::read(&nco_path).unwrap()).unwrap();
        assert_eq!(
            completed_data.negative_control.as_ref().unwrap().review_id,
            review_id
        );
        let mut biased_control = completed_data.clone();
        biased_control.evaluation_cutoff = None;
        biased_control
            .units
            .retain(|unit| unit.treatment_assigned_at == 60);
        for unit in &mut biased_control.units {
            if unit.treated {
                *unit.negative_control_outcome.as_mut().unwrap() += 2.0;
            }
        }
        let biased_artifact = store
            .add_artifact(
                &first.scope,
                "causal_dataset",
                "synthetic-biased-control",
                VERSION,
                "synthetic-biased-control-lineage",
                &serde_json::to_string(&biased_control).unwrap(),
                80,
                i64::MAX,
            )
            .unwrap();
        let EffectResult::Estimated(biased_estimate) = store
            .estimate_stratified_effect(&first.scope, &first.model_id, &biased_artifact.id)
            .unwrap()
        else {
            panic!("expected a diagnostic estimate for the biased synthetic control")
        };
        let mut biased_manifest = evaluation;
        biased_manifest.cases[1].estimate_id = biased_estimate.id;
        biased_manifest.cases[1]
            .expected_diagnostics
            .as_mut()
            .unwrap()
            .negative_control_imbalance = Some(true);
        let biased_report = evaluate_effects(&store, &first.scope, &biased_manifest).unwrap();
        assert_eq!(biased_report.negative_control_imbalance_flags, 1);
        assert_eq!(biased_report.negative_control_detection.true_positive, 1);
        assert_eq!(
            biased_report
                .negative_control_detection
                .positive_detection_rate_all_labeled,
            Some(1.0)
        );
        let artifact = store
            .add_artifact(
                &first.scope,
                "causal_dataset",
                "synthetic-observed",
                VERSION,
                "synthetic-cohort",
                &serde_json::to_string(&completed_data).unwrap(),
                80,
                i64::MAX,
            )
            .unwrap();
        let EffectResult::Estimated(estimate) = store
            .estimate_stratified_effect(&first.scope, &first.model_id, &artifact.id)
            .unwrap()
        else {
            panic!("expected synthetic estimate after manual reviews")
        };
        assert!((estimate.estimate + 2.0).abs() < 1e-10);
        assert_eq!(estimate.negative_control.unwrap().contrast, 0.0);
        assert_eq!(estimate.temporal_holdout.unwrap().abs_gap, 0.0);

        let decision_db = dir.path().join("decisions.db");
        let decisions = DecisionStore::with_causal_store(&decision_db, store.clone());
        let decision_scope = DecisionScope {
            tenant_id: first.scope.tenant_id.clone(),
            acl: first.scope.acl.clone(),
        };
        decisions
            .put_snapshot(
                &decision_scope,
                &DecisionSnapshot {
                    id: "synthetic-snapshot".into(),
                    queue_id: None,
                    data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
                    source_version_hashes: vec!["synthetic-v1".into()],
                    seed: 17,
                    arrivals_by_day: vec![5, 5],
                    initial_backlog: vec![],
                },
            )
            .unwrap();
        decisions
            .put_model(
                &decision_scope,
                &QueueModel {
                    version: "synthetic-queue-v1".into(),
                    service_capacity_per_agent_day: 3,
                    sla_days: 2,
                    staff_cost_cents_per_agent_day: 100,
                },
            )
            .unwrap();
        for (id, agents) in [("base", 1), ("more", 2)] {
            decisions
                .put_scenario(
                    &decision_scope,
                    &StaffingScenario {
                        id: id.into(),
                        agents_by_day: vec![agents; 2],
                        fixed_extra_capacity_by_day: vec![0; 2],
                    },
                )
                .unwrap();
        }
        let options = crate::decision_cmd::BriefOptions {
            db: decision_db,
            causal_db: Some(db),
            tenant: "synthetic".into(),
            acl: "private".into(),
            snapshot: "synthetic-snapshot".into(),
            model: "synthetic-queue-v1".into(),
            baseline: "base".into(),
            alternative: "more".into(),
            effect_ids: vec![estimate.id.clone()],
            assumptions: vec!["FIFO and fixed synthetic capacity".into()],
            empirical_run: None,
            policy_screen: None,
            forecast_validation: None,
            sla_holdout: None,
            baseline_event_hash: None,
            alternative_event_hash: None,
            source_file: None,
        };
        let brief = crate::decision_cmd::build_brief(&options).unwrap();
        assert_eq!(brief.observational_effects.len(), 1);
        let link = &brief.observational_effects[0];
        assert_eq!(link.estimate_id, estimate.id);
        assert_eq!(link.data_sha256, artifact.content_sha256);
        assert_eq!(link.data_ingested_at, artifact.ingested_at);
        assert_eq!(link.treatment_name, "extra_staffing");
        assert_eq!(link.outcome_name, "wait_hours");
        assert_eq!(link.outcome_unit, "hours");
        assert_eq!(
            link.population,
            "80 synthetic support units in two assignment periods"
        );
        assert_eq!(
            link.identification_state,
            "observational_adjusted_unvalidated"
        );
        assert!((link.estimate + 2.0).abs() < 1e-10);
        assert!(link.lower_bound <= link.estimate && link.estimate <= link.upper_bound);
        assert_eq!(
            link.interval_kind,
            "approximate_normal_conditional_on_recorded_strata"
        );
        assert_eq!(link.unit_count, 80);
        assert_eq!(link.strata_count, 2);
        assert_eq!(link.negative_control.as_ref().unwrap().contrast, 0.0);
        assert!(!link.negative_control.as_ref().unwrap().imbalance_flag);
        assert_eq!(link.temporal_holdout.as_ref().unwrap().abs_gap, 0.0);
        assert!(!link.temporal_holdout.as_ref().unwrap().sign_flip);
        assert!(link.permutation_diagnostic.runs > 0);
        assert_eq!(link.leave_one_unit_out.evaluated_units, 80);
        assert!(brief.uncertainty_interval.is_none());
        let ordinary = decisions
            .compare_scenarios(
                &decision_scope,
                "synthetic-snapshot",
                "synthetic-queue-v1",
                "base",
                "more",
                options.assumptions.clone(),
            )
            .unwrap();
        assert_eq!(brief.delta, ordinary.delta);
        decisions
            .put_snapshot(
                &decision_scope,
                &DecisionSnapshot {
                    id: "boundary-snapshot".into(),
                    queue_id: None,
                    // The model window ends at second 100, exactly this cutoff.
                    data_cutoff_utc: "1970-01-01T00:01:40Z".into(),
                    source_version_hashes: vec!["synthetic-v1".into()],
                    seed: 17,
                    arrivals_by_day: vec![5, 5],
                    initial_backlog: vec![],
                },
            )
            .unwrap();
        let boundary_options = crate::decision_cmd::BriefOptions {
            db: options.db.clone(),
            causal_db: options.causal_db.clone(),
            tenant: options.tenant.clone(),
            acl: options.acl.clone(),
            snapshot: "boundary-snapshot".into(),
            model: options.model.clone(),
            baseline: options.baseline.clone(),
            alternative: options.alternative.clone(),
            effect_ids: options.effect_ids.clone(),
            assumptions: options.assumptions.clone(),
            empirical_run: None,
            policy_screen: None,
            forecast_validation: None,
            sla_holdout: None,
            baseline_event_hash: None,
            alternative_event_hash: None,
            source_file: None,
        };
        let boundary_brief = crate::decision_cmd::build_brief(&boundary_options).unwrap();
        assert_eq!(boundary_brief.observational_effects.len(), 1);
        assert_eq!(
            boundary_brief.observational_effects[0].data_ingested_at,
            artifact.ingested_at
        );
        assert!(artifact.ingested_at > 100);
        decisions
            .put_snapshot(
                &decision_scope,
                &DecisionSnapshot {
                    id: "early-snapshot".into(),
                    queue_id: None,
                    data_cutoff_utc: "1970-01-01T00:00:50Z".into(),
                    source_version_hashes: vec!["synthetic-v1".into()],
                    seed: 17,
                    arrivals_by_day: vec![5, 5],
                    initial_backlog: vec![],
                },
            )
            .unwrap();
        let early_options = crate::decision_cmd::BriefOptions {
            db: options.db.clone(),
            causal_db: options.causal_db.clone(),
            tenant: options.tenant.clone(),
            acl: options.acl.clone(),
            snapshot: "early-snapshot".into(),
            model: options.model.clone(),
            baseline: options.baseline.clone(),
            alternative: options.alternative.clone(),
            effect_ids: options.effect_ids.clone(),
            assumptions: options.assumptions.clone(),
            empirical_run: None,
            policy_screen: None,
            forecast_validation: None,
            sla_holdout: None,
            baseline_event_hash: None,
            alternative_event_hash: None,
            source_file: None,
        };
        assert!(crate::decision_cmd::build_brief(&early_options).is_err());
        store
            .invalidate_artifact(&first.scope, &artifact.id)
            .unwrap();
        assert!(crate::decision_cmd::build_brief(&options).is_err());
        assert!(write_new_or_equal(&nco_path, b"different").is_err());
    }
}
