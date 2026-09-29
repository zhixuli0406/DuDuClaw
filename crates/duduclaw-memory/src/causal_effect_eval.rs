//! Offline synthetic-truth scoring for persisted observational estimates.
//! Source timestamps and lineages come from the scoped causal store, not the
//! evaluation JSON. Coverage here cannot validate performance on real data.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::causal::{CausalStore, EvidenceScope, now};
use crate::causal_effect::{EffectResult, ObservedDataset, ObservedUnit};

const NOMINAL_INTERVAL_COVERAGE: f64 = 0.95;
const WILSON_Z_95: f64 = 1.96;
const MIN_HELD_OUT_INTERVAL_CASES_FOR_SIGNAL: usize = 20;

fn cohort_unit_fingerprint(units: &[ObservedUnit]) -> Result<String, String> {
    let mut sorted: Vec<&ObservedUnit> = units.iter().collect();
    sorted.sort_unstable_by(|left, right| left.unit_id.cmp(&right.unit_id));
    if sorted.iter().any(|unit| unit.unit_id.trim().is_empty())
        || sorted
            .windows(2)
            .any(|pair| pair[0].unit_id == pair[1].unit_id)
    {
        return Err("effect source has empty or duplicate unit IDs".into());
    }
    // Unit IDs can be relabeled without changing the cohort. Compare the
    // complete multiset of measured rows after dropping that local label.
    let mut rows = Vec::with_capacity(sorted.len());
    for unit in sorted {
        let mut row = serde_json::to_value(unit).map_err(|error| error.to_string())?;
        row.as_object_mut()
            .ok_or("invalid cohort unit shape")?
            .remove("unit_id");
        rows.push(serde_json::to_vec(&row).map_err(|error| error.to_string())?);
    }
    rows.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(b"causal-cohort-id-independent-v1\0");
    for row in rows {
        digest.update((row.len() as u64).to_be_bytes());
        digest.update(row);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectEvalDataset {
    pub version: String,
    pub truth_basis: String,
    pub model_id: String,
    pub cutoff_unix: i64,
    pub cases: Vec<EffectEvalCase>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectEvalCase {
    pub id: String,
    pub estimate_id: String,
    /// Known average effect from the synthetic data-generating process.
    pub true_effect: f64,
    /// Optional synthetic DGP labels for diagnostic behavior. Unlabeled
    /// checks are excluded, and this command cannot verify these labels.
    #[serde(default)]
    pub expected_diagnostics: Option<ExpectedDiagnosticFlags>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExpectedDiagnosticFlags {
    pub unit_deletion_sign_flip: Option<bool>,
    pub temporal_sign_flip: Option<bool>,
    pub pre_treatment_imbalance: Option<bool>,
    pub negative_control_imbalance: Option<bool>,
}

impl ExpectedDiagnosticFlags {
    fn has_label(&self) -> bool {
        self.unit_deletion_sign_flip.is_some()
            || self.temporal_sign_flip.is_some()
            || self.pre_treatment_imbalance.is_some()
            || self.negative_control_imbalance.is_some()
    }
}

#[derive(Debug, Default, Serialize)]
pub struct DiagnosticDetection {
    pub labeled: usize,
    pub evaluated: usize,
    pub unavailable_positive: usize,
    pub unavailable_negative: usize,
    pub true_positive: usize,
    pub false_positive: usize,
    pub true_negative: usize,
    pub false_negative: usize,
    /// Conditional on the diagnostic being available.
    pub sensitivity_evaluated: Option<f64>,
    pub specificity_evaluated: Option<f64>,
    /// Conservatively treats unavailable positive cases as missed detections.
    pub positive_detection_rate_all_labeled: Option<f64>,
}

impl DiagnosticDetection {
    fn record(&mut self, expected: Option<bool>, observed: Option<bool>) {
        let Some(expected) = expected else { return };
        self.labeled += 1;
        match (expected, observed) {
            (true, Some(true)) => self.true_positive += 1,
            (true, Some(false)) => self.false_negative += 1,
            (false, Some(true)) => self.false_positive += 1,
            (false, Some(false)) => self.true_negative += 1,
            (true, None) => self.unavailable_positive += 1,
            (false, None) => self.unavailable_negative += 1,
        }
    }

    fn finish(&mut self) {
        self.evaluated =
            self.true_positive + self.false_positive + self.true_negative + self.false_negative;
        let available_positive = self.true_positive + self.false_negative;
        let available_negative = self.true_negative + self.false_positive;
        let all_positive = available_positive + self.unavailable_positive;
        self.sensitivity_evaluated =
            (available_positive > 0).then(|| self.true_positive as f64 / available_positive as f64);
        self.specificity_evaluated =
            (available_negative > 0).then(|| self.true_negative as f64 / available_negative as f64);
        self.positive_detection_rate_all_labeled =
            (all_positive > 0).then(|| self.true_positive as f64 / all_positive as f64);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalCalibrationStatus {
    InsufficientHeldOutCases,
    UndercoverageSignal,
    Inconclusive,
}

#[derive(Debug, Serialize)]
pub struct IntervalCalibrationAssessment {
    /// The persisted estimator uses estimate +/- 1.96 standard errors.
    pub nominal_coverage: f64,
    pub scored_cases: usize,
    pub covered_cases: usize,
    pub below_interval_cases: usize,
    pub above_interval_cases: usize,
    pub observed_coverage: Option<f64>,
    pub mean_interval_width: Option<f64>,
    /// Descriptive 95% interval score: width + 40 times miss distance.
    pub mean_interval_score: Option<f64>,
    /// Wilson 95% interval for the observed held-out coverage fraction.
    pub coverage_wilson_95_lower: Option<f64>,
    pub coverage_wilson_95_upper: Option<f64>,
    pub minimum_cases_for_signal: usize,
    pub status: IntervalCalibrationStatus,
}

#[derive(Default)]
struct IntervalCalibrationBuilder {
    scored: usize,
    covered: usize,
    below: usize,
    above: usize,
    total_width: f64,
    total_score: f64,
}

impl IntervalCalibrationBuilder {
    fn record(&mut self, lower: f64, upper: f64, truth: f64) -> Result<(), String> {
        if ![lower, upper, truth].iter().all(|value| value.is_finite()) || lower > upper {
            return Err("invalid held-out effect interval or synthetic truth".into());
        }
        let width = upper - lower;
        let (below, above, covered, miss_distance) = if truth < lower {
            (1, 0, 0, lower - truth)
        } else if truth > upper {
            (0, 1, 0, truth - upper)
        } else {
            (0, 0, 1, 0.0)
        };
        let score = width + (2.0 / (1.0 - NOMINAL_INTERVAL_COVERAGE)) * miss_distance;
        let total_width = self.total_width + width;
        let total_score = self.total_score + score;
        if ![width, miss_distance, score, total_width, total_score]
            .iter()
            .all(|value| value.is_finite())
        {
            return Err("numeric overflow while scoring effect intervals".into());
        }
        self.below += below;
        self.above += above;
        self.covered += covered;
        self.total_width = total_width;
        self.total_score = total_score;
        self.scored += 1;
        Ok(())
    }

    fn finish(self) -> IntervalCalibrationAssessment {
        let (observed_coverage, mean_interval_width, mean_interval_score, lower, upper) =
            if self.scored > 0 {
                let n = self.scored as f64;
                let p = self.covered as f64 / n;
                let z2 = WILSON_Z_95 * WILSON_Z_95;
                let divisor = 1.0 + z2 / n;
                let center = (p + z2 / (2.0 * n)) / divisor;
                let half_width =
                    WILSON_Z_95 * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt() / divisor;
                (
                    Some(p),
                    Some(self.total_width / n),
                    Some(self.total_score / n),
                    Some((center - half_width).clamp(0.0, 1.0)),
                    Some((center + half_width).clamp(0.0, 1.0)),
                )
            } else {
                (None, None, None, None, None)
            };
        let status = if self.scored < MIN_HELD_OUT_INTERVAL_CASES_FOR_SIGNAL {
            IntervalCalibrationStatus::InsufficientHeldOutCases
        } else if upper.is_some_and(|value| value < NOMINAL_INTERVAL_COVERAGE) {
            IntervalCalibrationStatus::UndercoverageSignal
        } else {
            IntervalCalibrationStatus::Inconclusive
        };
        IntervalCalibrationAssessment {
            nominal_coverage: NOMINAL_INTERVAL_COVERAGE,
            scored_cases: self.scored,
            covered_cases: self.covered,
            below_interval_cases: self.below,
            above_interval_cases: self.above,
            observed_coverage,
            mean_interval_width,
            mean_interval_score,
            coverage_wilson_95_lower: lower,
            coverage_wilson_95_upper: upper,
            minimum_cases_for_signal: MIN_HELD_OUT_INTERVAL_CASES_FOR_SIGNAL,
            status,
        }
    }
}

fn record_case_diagnostics(
    labels: Option<&ExpectedDiagnosticFlags>,
    observed: [Option<bool>; 4],
    scores: &mut [DiagnosticDetection; 4],
) {
    let Some(labels) = labels else { return };
    for (score, (expected, actual)) in scores.iter_mut().zip(
        [
            labels.unit_deletion_sign_flip,
            labels.temporal_sign_flip,
            labels.pre_treatment_imbalance,
            labels.negative_control_imbalance,
        ]
        .into_iter()
        .zip(observed),
    ) {
        score.record(expected, actual);
    }
}

#[derive(Debug, Serialize)]
pub struct EffectEvalReport {
    pub dataset_sha256: String,
    pub model_id: String,
    pub cutoff_unix: i64,
    pub training_cases: usize,
    pub held_out_cases: usize,
    pub scored_cases: usize,
    pub unknown_cases: usize,
    pub mean_error: Option<f64>,
    pub mean_absolute_error: Option<f64>,
    pub root_mean_squared_error: Option<f64>,
    /// Observed fraction of synthetic truths inside the estimator's
    /// uncalibrated descriptive normal intervals.
    pub interval_coverage: Option<f64>,
    pub interval_calibration: IntervalCalibrationAssessment,
    pub unit_deletion_sign_flips: usize,
    pub temporal_sign_flips: usize,
    pub pre_treatment_imbalance_flags: usize,
    pub negative_control_imbalance_flags: usize,
    pub unit_deletion_detection: DiagnosticDetection,
    pub temporal_detection: DiagnosticDetection,
    pub pre_treatment_detection: DiagnosticDetection,
    pub negative_control_detection: DiagnosticDetection,
    pub limitations: Vec<&'static str>,
}

/// Score only estimates from cohorts observed after the fixed cutoff.
/// Export time alone does not establish a held-out cohort: every assignment
/// in a held-out source must itself occur after the cutoff.
pub fn evaluate_effects(
    store: &CausalStore,
    scope: &EvidenceScope,
    dataset: &EffectEvalDataset,
) -> Result<EffectEvalReport, String> {
    if !scope.valid()
        || dataset.version != "causal-effect-eval-v1"
        || dataset.truth_basis != "synthetic_dgp"
        || dataset.model_id.trim().is_empty()
        || dataset.cases.len() < 2
        || dataset.cases.len() > 1_000
    {
        return Err("invalid synthetic effect evaluation configuration".into());
    }
    let conn = store.open().map_err(|error| error.to_string())?;
    let mut case_ids = HashSet::new();
    let mut estimate_ids = HashSet::new();
    let mut train_lineages = HashSet::new();
    let mut held_lineages = HashSet::new();
    let mut train_digests = HashSet::new();
    let mut held_digests = HashSet::new();
    let mut train_unit_fingerprints = HashSet::new();
    let mut held_unit_fingerprints = HashSet::new();
    let mut training_cases = 0;
    let mut held_out_cases = 0;
    let mut held_out = Vec::new();
    for case in &dataset.cases {
        if case.id.trim().is_empty()
            || case.estimate_id.trim().is_empty()
            || !case.true_effect.is_finite()
            || case
                .expected_diagnostics
                .as_ref()
                .is_some_and(|flags| !flags.has_label())
            || !case_ids.insert(&case.id)
            || !estimate_ids.insert(&case.estimate_id)
        {
            return Err(format!("invalid or duplicate effect case: {}", case.id));
        }
        let source: Option<(
            String,
            String,
            String,
            i64,
            String,
            String,
            String,
            Option<i64>,
            i64,
        )> = conn
            .query_row(
                "SELECT e.model_id,a.id,a.lineage_id,a.occurred_at,a.kind,
                    CASE WHEN NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                      WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                       AND r.artifact_id=a.id AND r.version=a.version)
                      AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                       WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                        AND o.artifact_id=a.id AND o.version=a.version
                        AND o.delivered_at IS NULL)
                    THEN a.content ELSE '' END,
                    a.content_sha256,a.invalidated_at,a.retention_at
             FROM causal_effect_estimates e
             JOIN causal_models m ON m.id=e.model_id
             JOIN causal_artifacts a ON a.id=e.data_snapshot_id
             WHERE e.id=?1 AND m.tenant_id=?2 AND m.acl=?3
               AND a.tenant_id=?2 AND a.acl=?3",
                params![case.estimate_id, scope.tenant_id, scope.acl],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let (
            model_id,
            source_id,
            lineage,
            occurred_at,
            kind,
            content,
            content_sha256,
            invalidated_at,
            retention_at,
        ) = source
            .ok_or_else(|| format!("estimate or source unavailable in scope: {}", case.id))?;
        if model_id != dataset.model_id || kind != "causal_dataset" || lineage.trim().is_empty() {
            return Err(format!(
                "estimate model or source kind mismatch: {}",
                case.id
            ));
        }
        let held = occurred_at > dataset.cutoff_unix;
        let active = invalidated_at.is_none() && retention_at > now() && !content.is_empty();
        if !active && !held {
            return Err(format!(
                "training source unavailable for split validation: {}",
                case.id
            ));
        }
        let mut unit_fingerprint = None;
        if active {
            if format!("{:x}", Sha256::digest(content.as_bytes())) != content_sha256 {
                return Err(format!("effect source digest invalid: {}", case.id));
            }
            let observations: ObservedDataset = serde_json::from_str(&content)
                .map_err(|_| format!("effect source schema invalid: {}", case.id))?;
            unit_fingerprint = Some(cohort_unit_fingerprint(&observations.units)?);
            if observations.units.is_empty()
                || observations.units.iter().any(|unit| {
                    unit.treatment_assigned_at >= unit.outcome_recorded_at
                        || unit.outcome_recorded_at > occurred_at
                        || if held {
                            unit.treatment_assigned_at <= dataset.cutoff_unix
                        } else {
                            unit.outcome_recorded_at > dataset.cutoff_unix
                        }
                })
            {
                return Err(format!(
                    "effect source crosses evaluation cutoff or export time: {}",
                    case.id
                ));
            }
        }
        if !held {
            if !train_lineages.insert(lineage) {
                return Err("duplicate training source lineage".into());
            }
            if !train_digests.insert(content_sha256) {
                return Err("duplicate training source content".into());
            }
            if !train_unit_fingerprints.insert(unit_fingerprint.expect("active training source")) {
                return Err("duplicate training cohort units".into());
            }
            training_cases += 1;
        } else {
            if !held_lineages.insert(lineage) {
                return Err("duplicate held-out source lineage".into());
            }
            if !held_digests.insert(content_sha256) {
                return Err("duplicate held-out source content".into());
            }
            if let Some(fingerprint) = unit_fingerprint {
                if !held_unit_fingerprints.insert(fingerprint) {
                    return Err("duplicate held-out cohort units".into());
                }
            }
            held_out_cases += 1;
            held_out.push((case, source_id));
        }
    }
    if training_cases == 0
        || held_out_cases == 0
        || !train_lineages.is_disjoint(&held_lineages)
        || !train_digests.is_disjoint(&held_digests)
        || !train_unit_fingerprints.is_disjoint(&held_unit_fingerprints)
    {
        return Err(
            "evaluation requires nonempty time splits with disjoint source lineages and content"
                .into(),
        );
    }
    let mut scored_cases = 0;
    let mut unknown_cases = 0;
    let mut sum_error = 0.0;
    let mut sum_abs_error = 0.0;
    let mut sum_squared_error = 0.0;
    let mut intervals = IntervalCalibrationBuilder::default();
    let mut unit_deletion_sign_flips = 0;
    let mut temporal_sign_flips = 0;
    let mut pre_treatment_imbalance_flags = 0;
    let mut negative_control_imbalance_flags = 0;
    let mut detection_scores: [DiagnosticDetection; 4] =
        std::array::from_fn(|_| DiagnosticDetection::default());
    for (case, source_id) in held_out {
        // Same connection as the split validation above: one `open()` per
        // case used to re-run schema setup and the live-source resync for
        // every one of up to 1,000 cases.
        match store
            .read_effect_estimate_with_conn(&conn, scope, &case.estimate_id)
            .map_err(|error| error.to_string())?
        {
            EffectResult::Unknown { .. } => {
                unknown_cases += 1;
                record_case_diagnostics(
                    case.expected_diagnostics.as_ref(),
                    [None; 4],
                    &mut detection_scores,
                );
            }
            EffectResult::Estimated(estimate) => {
                if estimate.model_id != dataset.model_id
                    || estimate.data_artifact_id != source_id
                    || ![
                        estimate.estimate,
                        estimate.lower_bound,
                        estimate.upper_bound,
                    ]
                    .iter()
                    .all(|number| number.is_finite())
                    || estimate.lower_bound > estimate.estimate
                    || estimate.estimate > estimate.upper_bound
                {
                    return Err(format!("invalid persisted estimate: {}", case.id));
                }
                let error = estimate.estimate - case.true_effect;
                sum_error += error;
                sum_abs_error += error.abs();
                sum_squared_error += error * error;
                intervals.record(estimate.lower_bound, estimate.upper_bound, case.true_effect)?;
                unit_deletion_sign_flips +=
                    usize::from(estimate.leave_one_unit_out.sign_flip == Some(true));
                temporal_sign_flips += usize::from(
                    estimate
                        .temporal_holdout
                        .as_ref()
                        .is_some_and(|result| result.sign_flip),
                );
                pre_treatment_imbalance_flags += usize::from(
                    estimate
                        .pre_treatment_placebo
                        .as_ref()
                        .is_some_and(|result| result.imbalance_flag),
                );
                negative_control_imbalance_flags += usize::from(
                    estimate
                        .negative_control
                        .as_ref()
                        .is_some_and(|result| result.imbalance_flag),
                );
                record_case_diagnostics(
                    case.expected_diagnostics.as_ref(),
                    [
                        estimate.leave_one_unit_out.sign_flip,
                        estimate
                            .temporal_holdout
                            .as_ref()
                            .map(|result| result.sign_flip),
                        estimate
                            .pre_treatment_placebo
                            .as_ref()
                            .map(|result| result.imbalance_flag),
                        estimate
                            .negative_control
                            .as_ref()
                            .map(|result| result.imbalance_flag),
                    ],
                    &mut detection_scores,
                );
                scored_cases += 1;
            }
        }
    }
    if ![sum_error, sum_abs_error, sum_squared_error]
        .iter()
        .all(|number| number.is_finite())
    {
        return Err("numeric overflow while scoring effects".into());
    }
    let denominator = scored_cases as f64;
    let interval_calibration = intervals.finish();
    if interval_calibration.scored_cases != scored_cases {
        return Err("effect interval score count mismatch".into());
    }
    for score in &mut detection_scores {
        score.finish();
    }
    let [
        unit_deletion_detection,
        temporal_detection,
        pre_treatment_detection,
        negative_control_detection,
    ] = detection_scores;
    let digest = serde_json::to_vec(dataset).map_err(|error| error.to_string())?;
    Ok(EffectEvalReport {
        dataset_sha256: format!("{:x}", Sha256::digest(digest)),
        model_id: dataset.model_id.clone(),
        cutoff_unix: dataset.cutoff_unix,
        training_cases,
        held_out_cases,
        scored_cases,
        unknown_cases,
        mean_error: (scored_cases > 0).then(|| sum_error / denominator),
        mean_absolute_error: (scored_cases > 0).then(|| sum_abs_error / denominator),
        root_mean_squared_error: (scored_cases > 0)
            .then(|| (sum_squared_error / denominator).sqrt()),
        interval_coverage: interval_calibration.observed_coverage,
        interval_calibration,
        unit_deletion_sign_flips,
        temporal_sign_flips,
        pre_treatment_imbalance_flags,
        negative_control_imbalance_flags,
        unit_deletion_detection,
        temporal_detection,
        pre_treatment_detection,
        negative_control_detection,
        limitations: vec![
            "Truth labels are declared synthetic DGP values; they are not verified by this command.",
            "Coverage of an uncalibrated normal interval on synthetic exports does not establish real-data calibration.",
            "The 95% interval score penalizes width and misses; the Wilson band and undercoverage signal descriptively treat held-out cases as independent, which distinct source IDs do not establish.",
            "Fewer than 20 scored held-out cases cannot produce an undercoverage signal here; an inconclusive status never attests calibration.",
            "Only post-cutoff estimates are scored; revoked or unreadable estimates count as unknown.",
            "Active-source deduplication catches identical measured row multisets even after unit-ID renaming; erased sources retain only raw digests, and partial overlap or upstream cohort identity is not attested.",
            "Diagnostic labels are supplied synthetic expectations; absent diagnostics are reported separately and do not enter evaluated-only sensitivity or specificity.",
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_assessment_penalizes_width_and_misses_without_small_sample_claim() {
        let mut builder = IntervalCalibrationBuilder::default();
        builder.record(0.0, 2.0, 1.0).unwrap();
        builder.record(0.0, 2.0, 3.0).unwrap();
        builder.record(0.0, 2.0, -1.0).unwrap();
        let report = builder.finish();
        assert_eq!(
            (
                report.scored_cases,
                report.covered_cases,
                report.below_interval_cases,
                report.above_interval_cases
            ),
            (3, 1, 1, 1)
        );
        assert_eq!(report.observed_coverage, Some(1.0 / 3.0));
        assert_eq!(report.mean_interval_width, Some(2.0));
        assert!(
            report
                .mean_interval_score
                .is_some_and(|score| (score - (86.0 / 3.0)).abs() < 1e-10)
        );
        assert!(report.coverage_wilson_95_lower.unwrap() < report.observed_coverage.unwrap());
        assert!(report.coverage_wilson_95_upper.unwrap() > report.observed_coverage.unwrap());
        assert_eq!(
            report.status,
            IntervalCalibrationStatus::InsufficientHeldOutCases
        );
    }

    #[test]
    fn interval_assessment_requires_enough_held_out_cases_for_signal() {
        let mut missed = IntervalCalibrationBuilder::default();
        let mut covered = IntervalCalibrationBuilder::default();
        for _ in 0..MIN_HELD_OUT_INTERVAL_CASES_FOR_SIGNAL {
            missed.record(0.0, 1.0, 2.0).unwrap();
            covered.record(0.0, 1.0, 0.5).unwrap();
        }
        let missed = missed.finish();
        assert_eq!(
            missed.status,
            IntervalCalibrationStatus::UndercoverageSignal
        );
        assert!(missed.coverage_wilson_95_upper.unwrap() < NOMINAL_INTERVAL_COVERAGE);
        let covered = covered.finish();
        assert_eq!(covered.status, IntervalCalibrationStatus::Inconclusive);
        assert_eq!(covered.observed_coverage, Some(1.0));
    }

    #[test]
    fn interval_assessment_rejects_invalid_or_overflowing_values() {
        let mut builder = IntervalCalibrationBuilder::default();
        assert!(builder.record(2.0, 1.0, 1.5).is_err());
        assert!(builder.record(0.0, f64::NAN, 1.0).is_err());
        assert!(builder.record(-f64::MAX, f64::MAX, 0.0).is_err());
        let result = builder.finish();
        assert_eq!(result.scored_cases, 0);
        assert_eq!(
            (
                result.covered_cases,
                result.below_interval_cases,
                result.above_interval_cases
            ),
            (0, 0, 0)
        );
    }

    #[test]
    fn diagnostic_detection_separates_false_alarms_misses_and_unavailable() {
        let mut detection = DiagnosticDetection::default();
        for (expected, observed) in [
            (Some(true), Some(true)),
            (Some(true), Some(false)),
            (Some(true), None),
            (Some(false), Some(true)),
            (Some(false), Some(false)),
            (Some(false), None),
            (None, Some(true)),
        ] {
            detection.record(expected, observed);
        }
        detection.finish();
        assert_eq!((detection.labeled, detection.evaluated), (6, 4));
        assert_eq!(
            (
                detection.true_positive,
                detection.false_negative,
                detection.false_positive,
                detection.true_negative,
                detection.unavailable_positive,
                detection.unavailable_negative,
            ),
            (1, 1, 1, 1, 1, 1)
        );
        assert_eq!(detection.sensitivity_evaluated, Some(0.5));
        assert_eq!(detection.specificity_evaluated, Some(0.5));
        assert_eq!(
            detection.positive_detection_rate_all_labeled,
            Some(1.0 / 3.0)
        );
    }

    #[test]
    fn diagnostic_labels_allow_one_named_check_and_reject_unknown_fields() {
        let labels: ExpectedDiagnosticFlags =
            serde_json::from_str(r#"{"negative_control_imbalance":true}"#).unwrap();
        assert!(labels.negative_control_imbalance == Some(true));
        assert!(labels.unit_deletion_sign_flip.is_none());
        assert!(labels.has_label());
        assert!(
            serde_json::from_str::<ExpectedDiagnosticFlags>(
                r#"{"negative_control_imbalnce":true}"#
            )
            .is_err()
        );
    }
}
