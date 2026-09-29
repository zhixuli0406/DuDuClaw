//! Limited stratified observational estimator behind model and data gates.
//!
//! Under an approved backdoor adjustment set encoded as joint strata, the
//! standardized contrast is sum_w P(W=w) [E(Y|A=1,W=w)-E(Y|A=0,W=w)].
//! Review approval cannot prove exchangeability; outputs remain explicitly
//! observational and require refutation and external validation.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::causal::{CausalStore, CausalStoreError, EvidenceScope, now};
use crate::causal_identify::AdjustmentReadiness;
use crate::causal_model::EffectReadiness;
use crate::causal_negative_control::NegativeControlReadiness;

const MIN_UNITS: usize = 20;
const MAX_UNITS: usize = 10_000;
const METHOD: &str = "stratified_backdoor_standardization_v7";
const PLACEBO_RUNS: usize = 128;

/// Counts must survive the f64/JSON path as exact, nonnegative integers.
fn valid_outcome_value(kind: &str, value: f64) -> bool {
    match kind {
        "continuous" => value.is_finite(),
        "count" => value.is_finite() && value >= 0.0
            && value <= 9_007_199_254_740_991.0 && value.fract() == 0.0,
        _ => false,
    }
}

fn estimator_code_digest() -> String {
    format!(
        "{:x}",
        Sha256::digest(
            [
                include_str!("causal_effect.rs"),
                include_str!("causal_identify.rs"),
                include_str!("causal_negative_control.rs"),
            ]
            .concat()
            .as_bytes()
        )
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedUnit {
    pub unit_id: String,
    /// Every adjustment variable must have been measured before assignment.
    #[serde(default)]
    pub adjustment_measured_at: Option<i64>,
    /// Exact variable-ID-to-value mapping behind the joint stratum label.
    #[serde(default)]
    pub adjustment_values: BTreeMap<String, String>,
    pub treatment_assigned_at: i64,
    pub outcome_recorded_at: i64,
    pub treated: bool,
    pub outcome: f64,
    /// Optional observation of the same outcome before treatment assignment.
    /// If supplied, every unit must supply both value and timestamp.
    #[serde(default)]
    pub pre_treatment_outcome: Option<f64>,
    #[serde(default)]
    pub pre_treatment_outcome_recorded_at: Option<i64>,
    /// Optional post-assignment negative-control outcome observed for every
    /// unit under a separately reviewed exclusion plan.
    #[serde(default)]
    pub negative_control_outcome: Option<f64>,
    #[serde(default)]
    pub negative_control_recorded_at: Option<i64>,
    /// A joint, pre-treatment confounder stratum fixed before outcome review.
    pub stratum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedDataset {
    /// Exact variable versions used to form each unit's joint stratum.
    /// The graph check rejects inadmissible or post-treatment variables.
    #[serde(default)]
    pub adjustment_variable_ids: Vec<String>,
    /// Optional predeclared treatment-assignment cutoff for a temporal holdout.
    /// Both sides must retain treated and control overlap in every stratum.
    #[serde(default)]
    pub evaluation_cutoff: Option<i64>,
    #[serde(default)]
    pub negative_control: Option<NegativeControlPlan>,
    pub units: Vec<ObservedUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegativeControlPlan {
    pub variable_id: String,
    pub review_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationalEstimate {
    pub id: String,
    pub model_id: String,
    pub data_artifact_id: String,
    pub data_sha256: String,
    pub method: String,
    pub estimate: f64,
    /// Approximate normal interval conditional on the recorded strata.
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub unit_count: usize,
    pub strata_count: usize,
    pub leave_one_stratum_out_sign_flip: Option<bool>,
    pub leave_one_unit_out: LeaveOneUnitOutDiagnostic,
    pub temporal_holdout: Option<TemporalHoldoutDiagnostic>,
    pub permutation_diagnostic: PermutationDiagnostic,
    pub pre_treatment_placebo: Option<PreTreatmentPlaceboDiagnostic>,
    pub negative_control: Option<NegativeControlDiagnostic>,
    pub identification_state: String,
}

/// A same-outcome, pre-assignment contrast. An imbalance flags a potential
/// exchangeability problem; a balanced value cannot rule one out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreTreatmentPlaceboDiagnostic {
    pub contrast: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub imbalance_flag: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NegativeControlDiagnostic {
    pub variable_id: String,
    pub review_id: String,
    pub contrast: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub imbalance_flag: bool,
}

/// A within-stratum label shuffle diagnostic. It does not test unmeasured
/// confounding or prove that treatment was assigned at random.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermutationDiagnostic {
    pub method: String,
    pub runs: usize,
    pub abs_at_least_observed: usize,
    pub median_abs_placebo: f64,
    pub max_abs_placebo: f64,
}

/// Descriptive sensitivity to deleting one observed unit. Deletions that
/// break the minimum within-stratum group size are not evaluated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaveOneUnitOutDiagnostic {
    pub evaluated_units: usize,
    pub skipped_due_positivity: usize,
    pub max_abs_effect_shift: Option<f64>,
    pub sign_flip: Option<bool>,
}

/// A descriptive check for effect drift across a predeclared assignment-time
/// split. Both periods are standardized to the full dataset's stratum weights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalHoldoutDiagnostic {
    pub cutoff: i64,
    pub training_units: usize,
    pub holdout_units: usize,
    pub training_estimate: f64,
    pub holdout_estimate: f64,
    pub abs_gap: f64,
    pub sign_flip: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EffectResult {
    Unknown { reasons: Vec<String> },
    Estimated(ObservationalEstimate),
}

fn sample_variance(values: &[f64], mean: f64) -> f64 {
    values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64
}

fn same_stored_number(left: f64, right: f64) -> bool {
    left.is_finite()
        && right.is_finite()
        && (left - right).abs()
            <= 4.0 * f64::EPSILON * left.abs().max(right.abs()).max(1.0)
}

fn same_stored_result(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    match (left, right) {
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) =>
            a.as_f64().zip(b.as_f64())
                .is_some_and(|(a, b)| same_stored_number(a, b)),
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) =>
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_stored_result(a, b)),
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) =>
            a.len() == b.len() && a.iter().all(|(key, value)|
                b.get(key).is_some_and(|other| same_stored_result(value, other))),
        _ => left == right,
    }
}

fn next_splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D049BB133111EB);
    value ^ (value >> 31)
}

fn permutation_diagnostic(
    strata: &BTreeMap<String, (Vec<f64>, Vec<f64>)>,
    observed: f64,
    model_id: &str,
    data_sha256: &str,
) -> Option<PermutationDiagnostic> {
    let digest = Sha256::digest(format!("{model_id}\0{data_sha256}\0{METHOD}").as_bytes());
    let mut state = u64::from_be_bytes(digest[..8].try_into().ok()?);
    let total: usize = strata.values().map(|(a, b)| a.len() + b.len()).sum();
    if total == 0 || !observed.is_finite() {
        return None;
    }
    let mut placebo_abs = Vec::with_capacity(PLACEBO_RUNS);
    for _ in 0..PLACEBO_RUNS {
        let mut estimate = 0.0;
        for (treated, control) in strata.values() {
            if treated.is_empty() || control.is_empty() {
                return None;
            }
            let mut outcomes: Vec<f64> = treated.iter().chain(control).copied().collect();
            for index in (1..outcomes.len()).rev() {
                let swap = (next_splitmix64(&mut state) % (index as u64 + 1)) as usize;
                outcomes.swap(index, swap);
            }
            let treated_mean = outcomes[..treated.len()].iter().sum::<f64>() / treated.len() as f64;
            let control_mean = outcomes[treated.len()..].iter().sum::<f64>() / control.len() as f64;
            let weight = outcomes.len() as f64 / total as f64;
            estimate += weight * (treated_mean - control_mean);
        }
        if !estimate.is_finite() {
            return None;
        }
        placebo_abs.push(estimate.abs());
    }
    let abs_at_least_observed = placebo_abs
        .iter()
        .filter(|value| **value >= observed.abs())
        .count();
    placebo_abs.sort_by(f64::total_cmp);
    Some(PermutationDiagnostic {
        method: "within_stratum_label_shuffle_v1".into(),
        runs: PLACEBO_RUNS,
        abs_at_least_observed,
        median_abs_placebo: placebo_abs[PLACEBO_RUNS / 2],
        max_abs_placebo: placebo_abs[PLACEBO_RUNS - 1],
    })
}

fn pre_treatment_placebo_diagnostic(
    strata: &BTreeMap<String, (Vec<f64>, Vec<f64>)>,
) -> Option<PreTreatmentPlaceboDiagnostic> {
    let total: usize = strata.values().map(|(treated, control)| treated.len() + control.len()).sum();
    if total == 0 {
        return None;
    }
    let mut contrast = 0.0;
    let mut variance = 0.0;
    for (treated, control) in strata.values() {
        if treated.len() < 2 || control.len() < 2 {
            return None;
        }
        let treated_mean = treated.iter().sum::<f64>() / treated.len() as f64;
        let control_mean = control.iter().sum::<f64>() / control.len() as f64;
        let weight = (treated.len() + control.len()) as f64 / total as f64;
        contrast += weight * (treated_mean - control_mean);
        variance += weight * weight
            * (sample_variance(treated, treated_mean) / treated.len() as f64
                + sample_variance(control, control_mean) / control.len() as f64);
    }
    let half_width = 1.96 * variance.sqrt();
    let lower_bound = contrast - half_width;
    let upper_bound = contrast + half_width;
    if ![contrast, lower_bound, upper_bound].iter().all(|value| value.is_finite()) {
        return None;
    }
    Some(PreTreatmentPlaceboDiagnostic {
        contrast,
        lower_bound,
        upper_bound,
        imbalance_flag: lower_bound > 0.0 || upper_bound < 0.0,
    })
}

fn leave_one_unit_out_diagnostic(
    strata: &BTreeMap<String, (Vec<f64>, Vec<f64>)>,
    observed: f64,
) -> Option<LeaveOneUnitOutDiagnostic> {
    let total: usize = strata.values().map(|(treated, control)| treated.len() + control.len()).sum();
    if total < 2 || !observed.is_finite() {
        return None;
    }
    let mut weighted_effect_sum = 0.0;
    let mut groups = Vec::with_capacity(strata.len());
    for (treated, control) in strata.values() {
        if treated.len() < 2 || control.len() < 2 {
            return None;
        }
        let treated_sum = treated.iter().sum::<f64>();
        let control_sum = control.iter().sum::<f64>();
        let size = treated.len() + control.len();
        let effect = treated_sum / treated.len() as f64 - control_sum / control.len() as f64;
        weighted_effect_sum += size as f64 * effect;
        groups.push((treated, control, treated_sum, control_sum, size, effect));
    }
    if !weighted_effect_sum.is_finite() {
        return None;
    }
    let mut evaluated_units = 0;
    let mut skipped_due_positivity = 0;
    let mut max_abs_effect_shift = 0.0_f64;
    let mut sign_flip = false;
    for (treated, control, treated_sum, control_sum, size, effect) in groups {
        for (values, is_treated) in [(treated, true), (control, false)] {
            if values.len() <= 2 {
                skipped_due_positivity += values.len();
                continue;
            }
            for value in values {
                let omitted_effect = if is_treated {
                    (treated_sum - value) / (treated.len() - 1) as f64
                        - control_sum / control.len() as f64
                } else {
                    treated_sum / treated.len() as f64
                        - (control_sum - value) / (control.len() - 1) as f64
                };
                let omitted = (weighted_effect_sum - size as f64 * effect
                    + (size - 1) as f64 * omitted_effect) / (total - 1) as f64;
                if !omitted.is_finite() {
                    return None;
                }
                let shift = (omitted - observed).abs();
                if !shift.is_finite() {
                    return None;
                }
                evaluated_units += 1;
                max_abs_effect_shift = max_abs_effect_shift.max(shift);
                sign_flip |= (omitted > 0.0) != (observed > 0.0)
                    && omitted != 0.0 && observed != 0.0;
            }
        }
    }
    Some(LeaveOneUnitOutDiagnostic {
        evaluated_units,
        skipped_due_positivity,
        max_abs_effect_shift: (evaluated_units > 0).then_some(max_abs_effect_shift),
        sign_flip: (evaluated_units > 0).then_some(sign_flip),
    })
}

/// Leave-one-stratum-out sensitivity: would dropping any single stratum flip
/// the sign of the pooled estimate?
///
/// `None` means "not assessable" — a single stratum (nothing to leave out), or
/// a non-finite recomputation. A zero on either side is NOT a sign flip: a
/// value of exactly 0.0 has no sign to flip, and treating it as negative
/// reported a flip for every completely null effect. Matches the guards in
/// `leave_one_unit_out_diagnostic` and `temporal_holdout_diagnostic`.
fn leave_one_stratum_out_sign_flip(estimate: f64, effects: &[(f64, f64)]) -> Option<bool> {
    if effects.len() <= 1 {
        return None;
    }
    let mut flipped = false;
    for (weight, effect) in effects {
        let omitted = (estimate - weight * effect) / (1.0 - weight);
        if !omitted.is_finite() {
            return None;
        }
        if (omitted > 0.0) != (estimate > 0.0) && omitted != 0.0 && estimate != 0.0 {
            flipped = true;
        }
    }
    Some(flipped)
}

fn temporal_holdout_diagnostic(
    all: &BTreeMap<String, (Vec<f64>, Vec<f64>)>,
    training: &BTreeMap<String, (Vec<f64>, Vec<f64>)>,
    holdout: &BTreeMap<String, (Vec<f64>, Vec<f64>)>,
    cutoff: i64,
) -> Option<TemporalHoldoutDiagnostic> {
    let total: usize = all.values().map(|(treated, control)| treated.len() + control.len()).sum();
    if total == 0 || training.len() != all.len() || holdout.len() != all.len() {
        return None;
    }
    let mut training_units = 0;
    let mut holdout_units = 0;
    let mut training_estimate = 0.0;
    let mut holdout_estimate = 0.0;
    for (stratum, (all_treated, all_control)) in all {
        let (train_treated, train_control) = training.get(stratum)?;
        let (held_treated, held_control) = holdout.get(stratum)?;
        if [train_treated.len(), train_control.len(), held_treated.len(), held_control.len()]
            .iter().any(|count| *count < 2) {
            return None;
        }
        training_units += train_treated.len() + train_control.len();
        holdout_units += held_treated.len() + held_control.len();
        let weight = (all_treated.len() + all_control.len()) as f64 / total as f64;
        training_estimate += weight * (
            train_treated.iter().sum::<f64>() / train_treated.len() as f64
                - train_control.iter().sum::<f64>() / train_control.len() as f64
        );
        holdout_estimate += weight * (
            held_treated.iter().sum::<f64>() / held_treated.len() as f64
                - held_control.iter().sum::<f64>() / held_control.len() as f64
        );
    }
    let abs_gap = (holdout_estimate - training_estimate).abs();
    if training_units + holdout_units != total
        || ![training_estimate, holdout_estimate, abs_gap].iter().all(|value| value.is_finite()) {
        return None;
    }
    Some(TemporalHoldoutDiagnostic {
        cutoff,
        training_units,
        holdout_units,
        training_estimate,
        holdout_estimate,
        abs_gap,
        sign_flip: (training_estimate > 0.0) != (holdout_estimate > 0.0)
            && training_estimate != 0.0 && holdout_estimate != 0.0,
    })
}

impl CausalStore {
    /// Return numeric results only while their model, assumptions, and data
    /// source remain valid in the requester's exact scope.
    pub fn read_effect_estimate(
        &self,
        scope: &EvidenceScope,
        estimate_id: &str,
    ) -> Result<EffectResult, CausalStoreError> {
        let conn = self.open()?;
        self.read_effect_estimate_with_conn(&conn, scope, estimate_id)
    }

    /// `read_effect_estimate` on a connection the caller already opened.
    /// `duduclaw causal-effect-eval` scores up to 1,000 cases in one run, and
    /// this call used to cost three `CausalStore::open()`s each.
    pub fn read_effect_estimate_with_conn(
        &self,
        conn: &Connection,
        scope: &EvidenceScope,
        estimate_id: &str,
    ) -> Result<EffectResult, CausalStoreError> {
        if !scope.valid() || estimate_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let row: Option<(String,String,String,String,Option<f64>,Option<f64>,Option<f64>,String,String,
            Option<String>,Option<String>,Option<i64>,Option<i64>)> = conn.query_row(
            "SELECT e.model_id,e.data_snapshot_id,e.method,e.code_sha256,e.estimate,e.lower_bound,e.upper_bound,
             e.identification_state,e.diagnostics_json,a.content_sha256,a.content,a.invalidated_at,a.retention_at
             FROM causal_effect_estimates e JOIN causal_models m ON m.id=e.model_id
             LEFT JOIN causal_artifacts a ON a.id=e.data_snapshot_id
                 AND a.tenant_id=m.tenant_id AND a.acl=m.acl
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                  WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                   AND r.artifact_id=a.id AND r.version=a.version)
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                  WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                   AND o.artifact_id=a.id AND o.version=a.version
                   AND o.delivered_at IS NULL)
             WHERE e.id=?1 AND m.tenant_id=?2 AND m.acl=?3",
            params![estimate_id,scope.tenant_id,scope.acl],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,
                row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?,row.get(12)?)),
        ).optional()?;
        let (
            model_id,
            data_artifact_id,
            method,
            code_sha256,
            estimate,
            lower_bound,
            upper_bound,
            state,
            diagnostics,
            data_sha256,
            data_content,
            invalidated_at,
            retention_at,
        ) = row.ok_or(CausalStoreError::NotFound)?;
        if state != "observational_adjusted_unvalidated"
            || method != METHOD
            || code_sha256 != estimator_code_digest()
            || invalidated_at.is_some()
            || retention_at.unwrap_or(0) <= now()
            || data_sha256.is_none()
            || data_content.is_none()
            || estimate.is_none()
            || lower_bound.is_none()
            || upper_bound.is_none()
            || estimate.is_some_and(|value| !value.is_finite())
            || lower_bound.is_some_and(|value| !value.is_finite())
            || upper_bound.is_some_and(|value| !value.is_finite())
            || lower_bound.zip(estimate).is_some_and(|(lower, value)| lower > value)
            || estimate.zip(upper_bound).is_some_and(|(value, upper)| value > upper)
        {
            return Ok(EffectResult::Unknown {
                reasons: vec!["effect source or record is no longer valid".into()],
            });
        }
        if format!(
            "{:x}",
            Sha256::digest(data_content.as_ref().expect("validated above").as_bytes())
        ) != *data_sha256.as_ref().expect("validated above")
        {
            return Ok(EffectResult::Unknown {
                reasons: vec!["effect data source failed digest validation".into()],
            });
        }
        if let EffectReadiness::Unknown { reasons } =
            Self::effect_readiness_with_conn(conn, scope, &model_id)?
        {
            return Ok(EffectResult::Unknown { reasons });
        }
        let review_ids = conn
            .prepare("SELECT review_id FROM causal_assumptions WHERE model_id=?1 ORDER BY kind")?
            .query_map([&model_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let expected_id = format!("{:x}", Sha256::digest(
            serde_json::to_vec(&(
                &model_id, &data_artifact_id,
                data_sha256.as_deref().expect("validated above"),
                &code_sha256, &review_ids,
            )).map_err(|_| CausalStoreError::InvalidInput)?
        ));
        if expected_id != estimate_id {
            return Ok(EffectResult::Unknown {
                reasons: vec!["effect assumption review version changed".into()],
            });
        }
        let diagnostics: serde_json::Value =
            serde_json::from_str(&diagnostics).map_err(|_| CausalStoreError::InvalidInput)?;
        if diagnostics.get("source_sha256").and_then(|value| value.as_str())
            != data_sha256.as_deref() {
            return Ok(EffectResult::Unknown {
                reasons: vec!["effect source version changed".into()],
            });
        }
        let source_dataset: ObservedDataset = match serde_json::from_str(
            data_content.as_deref().expect("validated above")
        ) {
            Ok(value) => value,
            Err(_) => return Ok(EffectResult::Unknown {
                reasons: vec!["effect source schema invalid".into()],
            }),
        };
        let unit_count = diagnostics
            .get("unit_count")
            .and_then(|value| value.as_u64())
            .ok_or(CausalStoreError::InvalidInput)? as usize;
        let strata_count = diagnostics
            .get("strata_count")
            .and_then(|value| value.as_u64())
            .ok_or(CausalStoreError::InvalidInput)? as usize;
        if source_dataset.units.len() != unit_count
            || source_dataset.units.iter().map(|unit| &unit.stratum).collect::<HashSet<_>>().len() != strata_count {
            return Ok(EffectResult::Unknown {
                reasons: vec!["effect source population changed".into()],
            });
        }
        let sign_flip = diagnostics
            .get("leave_one_stratum_out_sign_flip")
            .and_then(|value| value.as_bool());
        let leave_one_unit_out: LeaveOneUnitOutDiagnostic = match diagnostics
            .get("leave_one_unit_out")
            .and_then(|value| serde_json::from_value::<LeaveOneUnitOutDiagnostic>(value.clone()).ok())
        {
            Some(value)
                if value.evaluated_units.checked_add(value.skipped_due_positivity) == Some(unit_count)
                    && ((value.evaluated_units == 0
                        && value.max_abs_effect_shift.is_none()
                        && value.sign_flip.is_none())
                        || (value.evaluated_units > 0
                            && value.max_abs_effect_shift.is_some_and(|shift| shift.is_finite() && shift >= 0.0)
                            && value.sign_flip.is_some())) => value,
            _ => return Ok(EffectResult::Unknown {
                reasons: vec!["effect lacks valid leave-one-unit-out diagnostics".into()],
            }),
        };
        let temporal_holdout = match (source_dataset.evaluation_cutoff, diagnostics.get("temporal_holdout")) {
            (None, Some(serde_json::Value::Null)) => None,
            (Some(cutoff), Some(value)) => match serde_json::from_value::<TemporalHoldoutDiagnostic>(value.clone()) {
                Ok(diagnostic)
                    if diagnostic.cutoff == cutoff
                        && diagnostic.training_units.checked_add(diagnostic.holdout_units) == Some(unit_count)
                        && diagnostic.training_units > 0 && diagnostic.holdout_units > 0
                        && [diagnostic.training_estimate, diagnostic.holdout_estimate, diagnostic.abs_gap]
                            .iter().all(|number| number.is_finite())
                        && (diagnostic.abs_gap - (diagnostic.holdout_estimate - diagnostic.training_estimate).abs()).abs() < 1e-9
                        && diagnostic.sign_flip == ((diagnostic.training_estimate > 0.0) != (diagnostic.holdout_estimate > 0.0)
                            && diagnostic.training_estimate != 0.0 && diagnostic.holdout_estimate != 0.0) => Some(diagnostic),
                _ => return Ok(EffectResult::Unknown {
                    reasons: vec!["temporal holdout diagnostic invalid".into()],
                }),
            },
            _ => return Ok(EffectResult::Unknown {
                reasons: vec!["effect lacks current temporal holdout metadata".into()],
            }),
        };
        let permutation_diagnostic: PermutationDiagnostic = match diagnostics
            .get("permutation_diagnostic")
            .and_then(|value| serde_json::from_value::<PermutationDiagnostic>(value.clone()).ok())
        {
            Some(value)
                if value.method == "within_stratum_label_shuffle_v1"
                    && value.runs == PLACEBO_RUNS
                    && value.abs_at_least_observed <= value.runs
                    && value.median_abs_placebo.is_finite()
                    && value.max_abs_placebo.is_finite() =>
            {
                value
            }
            _ => {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["effect lacks current permutation diagnostics".into()],
                });
            }
        };
        let pre_treatment_placebo = match diagnostics.get("pre_treatment_placebo") {
            Some(serde_json::Value::Null) => None,
            Some(value) => match serde_json::from_value::<PreTreatmentPlaceboDiagnostic>(value.clone()) {
                Ok(value)
                    if [value.contrast, value.lower_bound, value.upper_bound]
                        .iter().all(|n| n.is_finite())
                        && value.lower_bound <= value.contrast
                        && value.contrast <= value.upper_bound
                        && value.imbalance_flag
                            == (value.lower_bound > 0.0 || value.upper_bound < 0.0) => Some(value),
                _ => return Ok(EffectResult::Unknown {
                    reasons: vec!["pre-treatment placebo diagnostic invalid".into()],
                }),
            },
            None => return Ok(EffectResult::Unknown {
                reasons: vec!["effect lacks pre-treatment placebo metadata".into()],
            }),
        };
        let negative_control = match (source_dataset.negative_control.as_ref(), diagnostics.get("negative_control")) {
            (None, Some(serde_json::Value::Null)) => None,
            (Some(plan), Some(value)) => match serde_json::from_value::<NegativeControlDiagnostic>(value.clone()) {
                Ok(diagnostic)
                    if diagnostic.variable_id == plan.variable_id
                        && diagnostic.review_id == plan.review_id
                        && [diagnostic.contrast, diagnostic.lower_bound, diagnostic.upper_bound]
                            .iter().all(|number| number.is_finite())
                        && diagnostic.lower_bound <= diagnostic.contrast
                        && diagnostic.contrast <= diagnostic.upper_bound
                        && diagnostic.imbalance_flag
                            == (diagnostic.lower_bound > 0.0 || diagnostic.upper_bound < 0.0) => {
                    match self.negative_control_readiness(scope, &model_id, &plan.variable_id, &plan.review_id)? {
                        NegativeControlReadiness::Reviewed { .. } => Some(diagnostic),
                        NegativeControlReadiness::Unknown { reasons } =>
                            return Ok(EffectResult::Unknown { reasons }),
                    }
                }
                _ => return Ok(EffectResult::Unknown {
                    reasons: vec!["negative-control diagnostic invalid".into()],
                }),
            },
            _ => return Ok(EffectResult::Unknown {
                reasons: vec!["effect lacks current negative-control metadata".into()],
            }),
        };
        let adjustment_variable_ids: Vec<String> = match diagnostics
            .get("adjustment_variable_ids")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
        {
            Some(ids) => ids,
            None => {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["effect predates graphical adjustment validation".into()],
                });
            }
        };
        let mut source_adjustment_ids = source_dataset.adjustment_variable_ids;
        source_adjustment_ids.sort();
        if source_adjustment_ids != adjustment_variable_ids {
            return Ok(EffectResult::Unknown {
                reasons: vec!["effect adjustment metadata differs from source".into()],
            });
        }
        if let AdjustmentReadiness::Unknown { reasons } =
            self.check_backdoor_adjustment(scope, &model_id, &adjustment_variable_ids)?
        {
            return Ok(EffectResult::Unknown { reasons });
        }
        let stored = ObservationalEstimate {
            id: estimate_id.into(),
            model_id,
            data_artifact_id,
            data_sha256: data_sha256.expect("validated above"),
            method,
            estimate: estimate.expect("validated above"),
            lower_bound: lower_bound.expect("validated above"),
            upper_bound: upper_bound.expect("validated above"),
            unit_count,
            strata_count,
            leave_one_stratum_out_sign_flip: sign_flip,
            leave_one_unit_out,
            temporal_holdout,
            permutation_diagnostic,
            pre_treatment_placebo,
            negative_control,
            identification_state: state,
        };
        // The row ID binds the source and review versions, but SQLite columns
        // can still be edited independently. Recompute from the exact source
        // without writing before exposing any numeric result or diagnostic.
        match self.estimate_stratified_effect_impl(
            scope, &stored.model_id, &stored.data_artifact_id, false,
        )? {
            EffectResult::Estimated(recomputed) => {
                // JSON value conversion can move a float by one ULP. Compare
                // the complete result tree with a narrow numeric tolerance;
                // identifiers, flags, counts, and field presence remain exact.
                let stored_value = serde_json::to_value(&stored)
                    .map_err(|_| CausalStoreError::InvalidInput)?;
                let computed_value = serde_json::to_value(&recomputed)
                    .map_err(|_| CausalStoreError::InvalidInput)?;
                if same_stored_result(&stored_value, &computed_value) {
                    Ok(EffectResult::Estimated(stored))
                } else {
                    Ok(EffectResult::Unknown {
                        reasons: vec!["effect result differs from source recomputation".into()],
                    })
                }
            }
            EffectResult::Unknown { reasons } => Ok(EffectResult::Unknown { reasons }),
        }
    }

    /// Estimate a population average contrast from a versioned, scoped
    /// `causal_dataset` source. The result is never promoted to an approved
    /// causal effect merely because the computation succeeded.
    pub fn estimate_stratified_effect(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        data_artifact_id: &str,
    ) -> Result<EffectResult, CausalStoreError> {
        self.estimate_stratified_effect_impl(scope, model_id, data_artifact_id, true)
    }

    fn estimate_stratified_effect_impl(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        data_artifact_id: &str,
        persist: bool,
    ) -> Result<EffectResult, CausalStoreError> {
        if !scope.valid() || data_artifact_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        match self.effect_readiness(scope, model_id)? {
            EffectReadiness::Unknown { reasons } => return Ok(EffectResult::Unknown { reasons }),
            EffectReadiness::ReadyForEstimator => {}
        }
        let conn = self.open()?;
        let model: Option<(i64, i64, String, String)> = conn
            .query_row(
                "SELECT m.window_start,m.window_end,t.value_kind,o.value_kind
             FROM causal_models m JOIN causal_variables t ON t.id=m.treatment_variable_id
             JOIN causal_variables o ON o.id=m.outcome_variable_id
             WHERE m.id=?1 AND m.tenant_id=?2 AND m.acl=?3",
                params![model_id, scope.tenant_id, scope.acl],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let (window_start, window_end, treatment_kind, outcome_kind) =
            model.ok_or(CausalStoreError::NotFound)?;
        if treatment_kind != "binary" || !matches!(outcome_kind.as_str(), "continuous" | "count") {
            return Ok(EffectResult::Unknown {
                reasons: vec!["unsupported variable measurement kinds".into()],
            });
        }
        let source: Option<(String,String,String)> = conn.query_row(
            "SELECT a.kind,a.content,a.content_sha256 FROM causal_artifacts a
             WHERE a.id=?1 AND a.tenant_id=?2 AND a.acl=?3
              AND a.invalidated_at IS NULL AND a.retention_at>?4
              AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
               WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                AND r.artifact_id=a.id AND r.version=a.version)
              AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
               WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                AND o.artifact_id=a.id AND o.version=a.version
                AND o.delivered_at IS NULL)",
            params![data_artifact_id,scope.tenant_id,scope.acl,now()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        let (kind, content, data_sha256) = match source {
            Some(value) => value,
            None => {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["data source unavailable in scope".into()],
                });
            }
        };
        if kind != "causal_dataset"
            || format!("{:x}", Sha256::digest(content.as_bytes())) != data_sha256
        {
            return Ok(EffectResult::Unknown {
                reasons: vec!["data source kind or digest invalid".into()],
            });
        }
        let data: ObservedDataset = match serde_json::from_str(&content) {
            Ok(value) => value,
            Err(_) => {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["observation schema invalid".into()],
                });
            }
        };
        if data.units.len() < MIN_UNITS || data.units.len() > MAX_UNITS {
            return Ok(EffectResult::Unknown {
                reasons: vec!["insufficient or excessive observations".into()],
            });
        }
        if data.evaluation_cutoff.is_some_and(|cutoff| cutoff <= window_start || cutoff >= window_end) {
            return Ok(EffectResult::Unknown {
                reasons: vec!["temporal holdout cutoff falls outside model window".into()],
            });
        }
        let adjustment_variable_ids =
            match self.check_backdoor_adjustment(scope, model_id, &data.adjustment_variable_ids)? {
                AdjustmentReadiness::Unknown { reasons } => {
                    return Ok(EffectResult::Unknown { reasons });
                }
                AdjustmentReadiness::GraphicallyAdmissible {
                    adjustment_variable_ids,
                } => adjustment_variable_ids,
            };
        let negative_control_kind = if let Some(plan) = &data.negative_control {
            if adjustment_variable_ids.contains(&plan.variable_id) {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["negative-control outcome cannot be an adjustment variable".into()],
                });
            }
            if let NegativeControlReadiness::Unknown { reasons } =
                self.negative_control_readiness(scope, model_id, &plan.variable_id, &plan.review_id)?
            {
                return Ok(EffectResult::Unknown { reasons });
            }
            let kind: Option<String> = conn.query_row(
                "SELECT v.value_kind FROM causal_model_variables mv
                 JOIN causal_variables v ON v.id=mv.variable_id
                 WHERE mv.model_id=?1 AND mv.variable_id=?2
                   AND v.tenant_id=?3 AND v.acl=?4",
                params![model_id, plan.variable_id, scope.tenant_id, scope.acl],
                |row| row.get(0),
            ).optional()?;
            match kind.as_deref() {
                Some("continuous" | "count") => kind,
                _ => return Ok(EffectResult::Unknown {
                    reasons: vec!["negative-control measurement kind unavailable".into()],
                }),
            }
        } else {
            None
        };
        let has_negative_control_values = data.units.iter().any(|unit| {
            unit.negative_control_outcome.is_some() || unit.negative_control_recorded_at.is_some()
        });
        if (data.negative_control.is_some() && data.units.iter().any(|unit| {
            unit.negative_control_outcome.is_none() || unit.negative_control_recorded_at.is_none()
        })) || (data.negative_control.is_none() && has_negative_control_values) {
            return Ok(EffectResult::Unknown {
                reasons: vec!["negative-control plan or observation coverage is incomplete".into()],
            });
        }
        let has_pre_treatment_outcome = data.units.iter().any(|unit| {
            unit.pre_treatment_outcome.is_some()
                || unit.pre_treatment_outcome_recorded_at.is_some()
        });
        if has_pre_treatment_outcome && data.units.iter().any(|unit| {
            unit.pre_treatment_outcome.is_none()
                || unit.pre_treatment_outcome_recorded_at.is_none()
        }) {
            return Ok(EffectResult::Unknown {
                reasons: vec!["pre-treatment placebo coverage is incomplete".into()],
            });
        }
        let mut unit_ids = HashSet::new();
        let mut strata: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        let mut pre_strata: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        let mut negative_control_strata: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        let mut training_strata: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        let mut holdout_strata: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        let adjustment_ids: HashSet<&str> =
            adjustment_variable_ids.iter().map(String::as_str).collect();
        let mut values_by_stratum: BTreeMap<String, String> = BTreeMap::new();
        let mut stratum_by_values: BTreeMap<String, String> = BTreeMap::new();
        for unit in &data.units {
            if !valid_outcome_value(&outcome_kind, unit.outcome) {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["outcome value incompatible with declared measurement kind".into()],
                });
            }
            if unit.unit_id.trim().is_empty()
                || !unit_ids.insert(&unit.unit_id)
                || unit.stratum.trim().is_empty()
                || unit.treatment_assigned_at < window_start
                || unit.treatment_assigned_at >= unit.outcome_recorded_at
                || unit.outcome_recorded_at >= window_end
            {
                return Ok(EffectResult::Unknown {
                    reasons: vec![
                        "time order, scope, uniqueness, or value validation failed".into(),
                    ],
                });
            }
            if unit.adjustment_values.len() != adjustment_ids.len()
                || unit.adjustment_values.iter().any(|(id, value)| {
                    !adjustment_ids.contains(id.as_str()) || value.trim().is_empty()
                })
                || (!adjustment_ids.is_empty()
                    && !unit
                        .adjustment_measured_at
                        .is_some_and(|time| time < unit.treatment_assigned_at))
            {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["adjustment values or pre-treatment timing invalid".into()],
                });
            }
            if has_pre_treatment_outcome
                && (!unit.pre_treatment_outcome.is_some_and(|value|
                    valid_outcome_value(&outcome_kind, value))
                    || !unit.pre_treatment_outcome_recorded_at.is_some_and(|time| {
                        time >= window_start && time < unit.treatment_assigned_at
                    }))
            {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["pre-treatment placebo value or timing invalid".into()],
                });
            }
            if data.negative_control.is_some()
                && (!unit.negative_control_outcome.is_some_and(|value|
                    negative_control_kind.as_deref().is_some_and(|kind|
                        valid_outcome_value(kind, value)))
                    || !unit.negative_control_recorded_at.is_some_and(|time| {
                        time > unit.treatment_assigned_at && time < window_end
                    }))
            {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["negative-control outcome value or timing invalid".into()],
                });
            }
            let value_key = serde_json::to_string(&unit.adjustment_values)
                .map_err(|_| CausalStoreError::InvalidInput)?;
            if values_by_stratum
                .insert(unit.stratum.clone(), value_key.clone())
                .is_some_and(|old| old != value_key)
                || stratum_by_values
                    .insert(value_key, unit.stratum.clone())
                    .is_some_and(|old| old != unit.stratum)
            {
                return Ok(EffectResult::Unknown {
                    reasons: vec!["stratum labels do not match adjustment values".into()],
                });
            }
            let groups = strata.entry(unit.stratum.clone()).or_default();
            if unit.treated {
                groups.0.push(unit.outcome);
            } else {
                groups.1.push(unit.outcome);
            }
            if let Some(cutoff) = data.evaluation_cutoff {
                let period = if unit.treatment_assigned_at < cutoff {
                    &mut training_strata
                } else {
                    &mut holdout_strata
                };
                let period_groups = period.entry(unit.stratum.clone()).or_default();
                if unit.treated {
                    period_groups.0.push(unit.outcome);
                } else {
                    period_groups.1.push(unit.outcome);
                }
            }
            if has_pre_treatment_outcome {
                let pre_groups = pre_strata.entry(unit.stratum.clone()).or_default();
                let value = unit.pre_treatment_outcome.expect("validated above");
                if unit.treated {
                    pre_groups.0.push(value);
                } else {
                    pre_groups.1.push(value);
                }
            }
            if data.negative_control.is_some() {
                let groups = negative_control_strata.entry(unit.stratum.clone()).or_default();
                let value = unit.negative_control_outcome.expect("validated above");
                if unit.treated {
                    groups.0.push(value);
                } else {
                    groups.1.push(value);
                }
            }
        }
        if strata
            .values()
            .any(|(treated, control)| treated.len() < 2 || control.len() < 2)
        {
            return Ok(EffectResult::Unknown {
                reasons: vec!["positivity failed in a recorded stratum".into()],
            });
        }
        let total = data.units.len() as f64;
        let mut effects = Vec::with_capacity(strata.len());
        let mut estimate = 0.0;
        let mut variance = 0.0;
        for (treated, control) in strata.values() {
            let treated_mean = treated.iter().sum::<f64>() / treated.len() as f64;
            let control_mean = control.iter().sum::<f64>() / control.len() as f64;
            let effect = treated_mean - control_mean;
            let weight = (treated.len() + control.len()) as f64 / total;
            let within_variance = sample_variance(treated, treated_mean) / treated.len() as f64
                + sample_variance(control, control_mean) / control.len() as f64;
            estimate += weight * effect;
            variance += weight * weight * within_variance;
            effects.push((weight, effect));
        }
        let standard_error = variance.sqrt();
        let lower_bound = estimate - 1.96 * standard_error;
        let upper_bound = estimate + 1.96 * standard_error;
        if ![estimate, lower_bound, upper_bound]
            .iter()
            .all(|value| value.is_finite())
        {
            return Ok(EffectResult::Unknown {
                reasons: vec!["numeric estimate overflow".into()],
            });
        }
        let sign_flip = leave_one_stratum_out_sign_flip(estimate, &effects);
        let leave_one_unit_out = match leave_one_unit_out_diagnostic(&strata, estimate) {
            Some(value) => value,
            None => return Ok(EffectResult::Unknown {
                reasons: vec!["leave-one-unit-out diagnostic failed numeric validation".into()],
            }),
        };
        let temporal_holdout = if let Some(cutoff) = data.evaluation_cutoff {
            match temporal_holdout_diagnostic(&strata, &training_strata, &holdout_strata, cutoff) {
                Some(value) => Some(value),
                None => return Ok(EffectResult::Unknown {
                    reasons: vec!["temporal holdout lacks overlap or failed numeric validation".into()],
                }),
            }
        } else {
            None
        };
        let permutation_diagnostic =
            match permutation_diagnostic(&strata, estimate, model_id, &data_sha256) {
                Some(value) => value,
                None => {
                    return Ok(EffectResult::Unknown {
                        reasons: vec!["permutation diagnostic failed numeric validation".into()],
                    });
                }
            };
        let pre_treatment_placebo = if has_pre_treatment_outcome {
            match pre_treatment_placebo_diagnostic(&pre_strata) {
                Some(value) => Some(value),
                None => return Ok(EffectResult::Unknown {
                    reasons: vec!["pre-treatment placebo diagnostic failed numeric validation".into()],
                }),
            }
        } else {
            None
        };
        let negative_control = if let Some(plan) = &data.negative_control {
            match pre_treatment_placebo_diagnostic(&negative_control_strata) {
                Some(value) => Some(NegativeControlDiagnostic {
                    variable_id: plan.variable_id.clone(),
                    review_id: plan.review_id.clone(),
                    contrast: value.contrast,
                    lower_bound: value.lower_bound,
                    upper_bound: value.upper_bound,
                    imbalance_flag: value.imbalance_flag,
                }),
                None => return Ok(EffectResult::Unknown {
                    reasons: vec!["negative-control diagnostic failed numeric validation".into()],
                }),
            }
        } else {
            None
        };
        let code_sha256 = estimator_code_digest();
        let review_ids = conn
            .prepare("SELECT review_id FROM causal_assumptions WHERE model_id=?1 ORDER BY kind")?
            .query_map([model_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let estimate_id = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    model_id,
                    data_artifact_id,
                    &data_sha256,
                    &code_sha256,
                    &review_ids
                ))
                .map_err(|_| CausalStoreError::InvalidInput)?
            )
        );
        let diagnostics = serde_json::json!({
            "unit_count":data.units.len(), "strata_count":strata.len(),
            "source_sha256":data_sha256,
            "adjustment_variable_ids":adjustment_variable_ids,
            "standard_error":standard_error,
            "leave_one_stratum_out_sign_flip":sign_flip,
            "leave_one_unit_out":leave_one_unit_out,
            "temporal_holdout":temporal_holdout,
            "permutation_diagnostic":permutation_diagnostic,
            "pre_treatment_placebo":pre_treatment_placebo,
            "negative_control":negative_control,
            "interval_method":"normal_approximation_unvalidated",
        })
        .to_string();
        if persist {
            conn.execute(
            "INSERT OR IGNORE INTO causal_effect_estimates
            (id,model_id,data_snapshot_id,method,code_sha256,estimate,lower_bound,
             upper_bound,diagnostics_json,identification_state,created_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                estimate_id,
                model_id,
                data_artifact_id,
                METHOD,
                code_sha256,
                estimate,
                lower_bound,
                upper_bound,
                diagnostics,
                "observational_adjusted_unvalidated",
                now()
            ],
            )?;
        }
        Ok(EffectResult::Estimated(ObservationalEstimate {
            id: estimate_id,
            model_id: model_id.into(),
            data_artifact_id: data_artifact_id.into(),
            data_sha256,
            method: METHOD.into(),
            estimate,
            lower_bound,
            upper_bound,
            unit_count: data.units.len(),
            strata_count: strata.len(),
            leave_one_stratum_out_sign_flip: sign_flip,
            leave_one_unit_out,
            temporal_holdout,
            permutation_diagnostic,
            pre_treatment_placebo,
            negative_control,
            identification_state: "observational_adjusted_unvalidated".into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{ClaimModality, EvidenceStance};
    use crate::causal_effect_eval::{EffectEvalCase, EffectEvalDataset, ExpectedDiagnosticFlags, evaluate_effects};
    use crate::causal_model::{AssumptionKind, AssumptionVerdict, ModelDraft, VariableKind};

    #[test]
    fn count_measurements_reject_fractional_negative_and_inexact_values() {
        assert!(valid_outcome_value("count", 0.0));
        assert!(valid_outcome_value("count", 9_007_199_254_740_991.0));
        for value in [-1.0, 1.5, 9_007_199_254_740_992.0, f64::NAN] {
            assert!(!valid_outcome_value("count", value));
        }
        assert!(valid_outcome_value("continuous", -1.5));
    }

    #[test]
    fn stratum_deletion_reports_no_sign_flip_for_a_zero_estimate() {
        // A pooled estimate of exactly 0.0 has no sign, so the strata that
        // cancel it out must not be reported as flipping it. Before the guard,
        // any positive leave-one-out recomputation here scored a "sign flip"
        // and that boolean was persisted into `diagnostics_json`.
        let cancelling = [(0.5, 2.0), (0.5, -2.0)];
        assert_eq!(
            leave_one_stratum_out_sign_flip(0.0, &cancelling),
            Some(false)
        );

        // A real flip (positive pooled estimate, negative without one stratum)
        // is still reported.
        assert_eq!(
            leave_one_stratum_out_sign_flip(1.0, &[(0.5, 8.0), (0.5, -6.0)]),
            Some(true)
        );
        // Nothing to leave out, and non-finite recomputations, stay unknown.
        assert_eq!(leave_one_stratum_out_sign_flip(1.0, &[(1.0, 1.0)]), None);
        assert_eq!(
            leave_one_stratum_out_sign_flip(1.0, &[(1.0, 1.0), (0.5, 1.0)]),
            None
        );
    }

    #[test]
    fn placebo_permutation_is_reproducible_and_zero_contrast_is_not_evidence() {
        let strata = BTreeMap::from([
            ("a".into(), (vec![1.0, 2.0, 3.0], vec![1.0, 2.0, 3.0])),
            ("b".into(), (vec![4.0, 5.0, 6.0], vec![4.0, 5.0, 6.0])),
        ]);
        let first = permutation_diagnostic(&strata, 0.0, "model", "data").unwrap();
        let second = permutation_diagnostic(&strata, 0.0, "model", "data").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.abs_at_least_observed, PLACEBO_RUNS);
        assert!(first.max_abs_placebo >= first.median_abs_placebo);
    }

    #[test]
    fn unit_deletion_exposes_outlier_sign_flip_and_positivity_limits() {
        let outlier = BTreeMap::from([(
            "all".into(),
            (vec![10.0, 10.0, 10.0, 100.0], vec![20.0; 4]),
        )]);
        let diagnostic = leave_one_unit_out_diagnostic(&outlier, 12.5).unwrap();
        assert_eq!(diagnostic.evaluated_units, 8);
        assert_eq!(diagnostic.skipped_due_positivity, 0);
        assert!((diagnostic.max_abs_effect_shift.unwrap() - 22.5).abs() < 1e-10);
        assert_eq!(diagnostic.sign_flip, Some(true));

        let sparse = BTreeMap::from([("all".into(), (vec![1.0, 2.0], vec![3.0, 4.0]))]);
        let diagnostic = leave_one_unit_out_diagnostic(&sparse, -2.0).unwrap();
        assert_eq!(diagnostic.evaluated_units, 0);
        assert_eq!(diagnostic.skipped_due_positivity, 4);
        assert_eq!(diagnostic.max_abs_effect_shift, None);
        assert_eq!(diagnostic.sign_flip, None);
    }

    #[test]
    fn temporal_holdout_detects_effect_reversal_on_common_weights() {
        let all = BTreeMap::from([(
            "all".into(),
            (vec![3.0, 3.0, 1.0, 1.0], vec![2.0; 4]),
        )]);
        let training = BTreeMap::from([("all".into(), (vec![3.0; 2], vec![2.0; 2]))]);
        let holdout = BTreeMap::from([("all".into(), (vec![1.0; 2], vec![2.0; 2]))]);
        let result = temporal_holdout_diagnostic(&all, &training, &holdout, 50).unwrap();
        assert_eq!(result.training_estimate, 1.0);
        assert_eq!(result.holdout_estimate, -1.0);
        assert_eq!(result.abs_gap, 2.0);
        assert!(result.sign_flip);
    }

    #[test]
    fn estimate_requires_reviewed_model_and_positive_strata() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let scope = EvidenceScope {
            tenant_id: "a".into(),
            acl: "private".into(),
        };
        let treatment = store
            .register_variable(
                &scope,
                "staffing",
                "v1",
                "extra staffing",
                "yes/no",
                VariableKind::Binary,
            )
            .unwrap();
        let outcome = store
            .register_variable(
                &scope,
                "wait",
                "v1",
                "wait hours",
                "hours",
                VariableKind::Continuous,
            )
            .unwrap();
        let cohort = store
            .register_variable(
                &scope,
                "cohort",
                "v1",
                "pre-treatment queue type",
                "category",
                VariableKind::Categorical,
            )
            .unwrap();
        let negative_outcome = store
            .register_variable(
                &scope,
                "unrelated_service_metric",
                "v1",
                "synthetic outcome excluded from staffing effect",
                "count",
                VariableKind::Count,
            )
            .unwrap();
        let source = store
            .add_artifact(
                &scope,
                "ticket",
                "claim",
                "v1",
                "claim-lineage",
                "staffing changed wait",
                1,
                i64::MAX,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &scope,
                "staffing",
                "wait",
                0,
                10,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &source.id,
                0,
                8,
                "staffing",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        store
            .review_claim(&scope, &claim.id, "reviewer", true)
            .unwrap();
        let confounder_source = store
            .add_artifact(
                &scope,
                "review_note",
                "confounder",
                "v1",
                "confounder-lineage",
                "cohort affects staffing and wait",
                1,
                i64::MAX,
            )
            .unwrap();
        let mut confounder_claim_ids = Vec::new();
        for effect in ["staffing", "wait"] {
            let confounder_claim = store
                .add_claim(
                    &scope,
                    "cohort",
                    effect,
                    0,
                    10,
                    &serde_json::json!({}),
                    ClaimModality::Asserted,
                )
                .unwrap();
            store
                .add_evidence(
                    &scope,
                    &confounder_claim.id,
                    &confounder_source.id,
                    0,
                    6,
                    "cohort",
                    EvidenceStance::Supports,
                    None,
                    "test",
                )
                .unwrap();
            store
                .review_claim(&scope, &confounder_claim.id, "reviewer", true)
                .unwrap();
            confounder_claim_ids.push(confounder_claim.id);
        }
        let draft = ModelDraft {
            name: "pilot".into(),
            version: "v1".into(),
            treatment_variable_id: treatment.id.clone(),
            outcome_variable_id: outcome.id.clone(),
            population: "support".into(),
            window_start: 10,
            window_end: 100,
            variable_ids: vec![treatment.id, outcome.id, cohort.id.clone(), negative_outcome.id.clone()],
            claim_ids: [vec![claim.id], confounder_claim_ids].concat(),
        };
        let model = store.create_model(&scope, &draft).unwrap();
        let rows: Vec<_> = (0..40)
            .map(|n| ObservedUnit {
                unit_id: format!("u{n}"),
                adjustment_measured_at: Some(15),
                adjustment_values: BTreeMap::from([(
                    cohort.id.clone(),
                    if n < 20 { "low" } else { "high" }.into(),
                )]),
                treatment_assigned_at: 20,
                outcome_recorded_at: 30,
                treated: n % 2 == 0,
                outcome: if n % 2 == 0 { 8.0 } else { 10.0 },
                pre_treatment_outcome: None,
                pre_treatment_outcome_recorded_at: None,
                negative_control_outcome: None,
                negative_control_recorded_at: None,
                stratum: if n < 20 { "low" } else { "high" }.into(),
            })
            .collect();
        let dataset = store
            .add_artifact(
                &scope,
                "causal_dataset",
                "cohort",
                "v1",
                "cohort-v1",
                &serde_json::to_string(&ObservedDataset {
                    adjustment_variable_ids: vec![cohort.id.clone()],
                    evaluation_cutoff: None,
                    negative_control: None,
                    units: rows.clone(),
                })
                .unwrap(),
                40,
                i64::MAX,
            )
            .unwrap();
        assert!(matches!(
            store
                .estimate_stratified_effect(&scope, &model.id, &dataset.id)
                .unwrap(),
            EffectResult::Unknown { .. }
        ));
        store
            .review_model(&scope, &model.id, "reviewer", true, false)
            .unwrap();
        for kind in AssumptionKind::ALL {
            store
                .record_assumption(
                    &scope,
                    &model.id,
                    kind,
                    AssumptionVerdict::Pass,
                    "Pilot cohort review completed",
                    "reviewer",
                )
                .unwrap();
        }
        let unadjusted_source = store
            .add_artifact(
                &scope,
                "causal_dataset",
                "unadjusted",
                "v1",
                "unadjusted-v1",
                &serde_json::to_string(&ObservedDataset {
                    adjustment_variable_ids: vec![],
                    evaluation_cutoff: None,
                    negative_control: None,
                    units: rows.clone(),
                })
                .unwrap(),
                20,
                i64::MAX,
            )
            .unwrap();
        assert!(matches!(
            store
                .estimate_stratified_effect(&scope, &model.id, &unadjusted_source.id)
                .unwrap(),
            EffectResult::Unknown { .. }
        ));
        let outcome = store
            .estimate_stratified_effect(&scope, &model.id, &dataset.id)
            .unwrap();
        let EffectResult::Estimated(value) = outcome else {
            panic!("expected estimate")
        };
        assert!((value.estimate + 2.0).abs() < 1e-10);
        assert_eq!(value.strata_count, 2);
        assert_eq!(value.leave_one_unit_out.evaluated_units, 40);
        assert_eq!(value.leave_one_unit_out.skipped_due_positivity, 0);
        assert_eq!(value.leave_one_unit_out.max_abs_effect_shift, Some(0.0));
        assert_eq!(value.leave_one_unit_out.sign_flip, Some(false));
        assert_eq!(value.permutation_diagnostic.runs, PLACEBO_RUNS);
        assert!(value.permutation_diagnostic.abs_at_least_observed <= PLACEBO_RUNS);
        assert!(value.permutation_diagnostic.median_abs_placebo.is_finite());
        let EffectResult::Estimated(read_back) = store.read_effect_estimate(&scope, &value.id).unwrap() else {
            panic!("expected persisted estimate")
        };
        assert_eq!(read_back.id, value.id);
        assert_eq!(read_back.leave_one_unit_out, value.leave_one_unit_out);
        assert!((read_back.estimate - value.estimate).abs() < 1e-12);
        assert!((read_back.permutation_diagnostic.median_abs_placebo
            - value.permutation_diagnostic.median_abs_placebo).abs() < 1e-12);
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let original_review_id: String = conn.query_row(
            "SELECT review_id FROM causal_assumptions WHERE model_id=?1 AND kind='exchangeability'",
            [&model.id], |row| row.get(0),
        ).unwrap();
        conn.execute(
            "UPDATE causal_assumptions SET review_id='tampered-review' WHERE model_id=?1 AND kind='exchangeability'",
            [&model.id],
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        conn.execute(
            "UPDATE causal_assumptions SET review_id=?1 WHERE model_id=?2 AND kind='exchangeability'",
            params![original_review_id, model.id],
        ).unwrap();
        let stored_diagnostics: String = conn.query_row(
            "SELECT diagnostics_json FROM causal_effect_estimates WHERE id=?1",
            [&value.id],
            |row| row.get(0),
        ).unwrap();
        let mut incomplete_diagnostics: serde_json::Value =
            serde_json::from_str(&stored_diagnostics).unwrap();
        incomplete_diagnostics.as_object_mut().unwrap().remove("leave_one_unit_out");
        conn.execute(
            "UPDATE causal_effect_estimates SET diagnostics_json=?1 WHERE id=?2",
            params![incomplete_diagnostics.to_string(), value.id],
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        conn.execute(
            "UPDATE causal_effect_estimates SET diagnostics_json=?1 WHERE id=?2",
            params![stored_diagnostics, value.id],
        ).unwrap();
        conn.execute(
            "UPDATE causal_effect_estimates SET lower_bound=?1 WHERE id=?2",
            params![value.estimate + 1.0, value.id],
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        conn.execute(
            "UPDATE causal_effect_estimates SET lower_bound=?1 WHERE id=?2",
            params![value.lower_bound, value.id],
        ).unwrap();
        // A plausible in-range edit passes the interval-shape check but must
        // still fail against the immutable source computation.
        conn.execute(
            "UPDATE causal_effect_estimates SET estimate=?1 WHERE id=?2",
            params![value.estimate + 0.25, value.id],
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        conn.execute(
            "UPDATE causal_effect_estimates SET estimate=?1 WHERE id=?2",
            params![value.estimate, value.id],
        ).unwrap();
        let mut changed_diagnostics: serde_json::Value =
            serde_json::from_str(&stored_diagnostics).unwrap();
        changed_diagnostics["leave_one_unit_out"]["sign_flip"] =
            serde_json::Value::Bool(true);
        conn.execute(
            "UPDATE causal_effect_estimates SET diagnostics_json=?1 WHERE id=?2",
            params![changed_diagnostics.to_string(), value.id],
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        conn.execute(
            "UPDATE causal_effect_estimates SET diagnostics_json=?1 WHERE id=?2",
            params![stored_diagnostics, value.id],
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Estimated(_)
        ));
        assert!(value.pre_treatment_placebo.is_none());
        assert!(value.temporal_holdout.is_none());
        let mut balanced_pre = rows.clone();
        for unit in &mut balanced_pre {
            unit.pre_treatment_outcome = Some(5.0);
            unit.pre_treatment_outcome_recorded_at = Some(18);
        }
        let mut imbalanced_pre = balanced_pre.clone();
        for unit in &mut imbalanced_pre {
            unit.pre_treatment_outcome = Some(if unit.treated { 7.0 } else { 5.0 });
        }
        let mut incomplete_pre = balanced_pre.clone();
        incomplete_pre[0].pre_treatment_outcome = None;
        let mut late_pre = balanced_pre.clone();
        late_pre[0].pre_treatment_outcome_recorded_at = Some(22);
        for (name, units, expected_imbalance) in [
            ("balanced-pre", balanced_pre, Some(false)),
            ("imbalanced-pre", imbalanced_pre, Some(true)),
            ("incomplete-pre", incomplete_pre, None),
            ("late-pre", late_pre, None),
        ] {
            let artifact = store.add_artifact(
                &scope, "causal_dataset", name, "v1", name,
                &serde_json::to_string(&ObservedDataset {
                    adjustment_variable_ids: vec![cohort.id.clone()], evaluation_cutoff: None,
                    negative_control: None, units,
                }).unwrap(),
                20, i64::MAX,
            ).unwrap();
            let result = store.estimate_stratified_effect(&scope, &model.id, &artifact.id).unwrap();
            match expected_imbalance {
                Some(expected) => {
                    let EffectResult::Estimated(estimate) = result else { panic!("expected placebo diagnostic") };
                    let placebo = estimate.pre_treatment_placebo.as_ref().unwrap();
                    assert_eq!(placebo.imbalance_flag, expected);
                    let placebo_read = store.read_effect_estimate(&scope, &estimate.id).unwrap();
                    let EffectResult::Estimated(read_back) = placebo_read else {
                        panic!("expected persisted placebo estimate: {placebo_read:?}")
                    };
                    assert_eq!(read_back.id, estimate.id);
                    assert_eq!(read_back.leave_one_unit_out, estimate.leave_one_unit_out);
                    assert_eq!(read_back.pre_treatment_placebo, estimate.pre_treatment_placebo);
                }
                None => assert!(matches!(result, EffectResult::Unknown { .. })),
            }
        }
        let mut temporal_rows = rows.clone();
        for (index, unit) in temporal_rows.iter_mut().enumerate() {
            if index % 20 >= 10 {
                unit.treatment_assigned_at = 60;
                unit.outcome_recorded_at = 70;
                if unit.treated {
                    unit.outcome = 6.0;
                }
            }
        }
        let temporal_source = store.add_artifact(
            &scope, "causal_dataset", "temporal", "v1", "temporal-v1",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: Some(50),
                negative_control: None,
                units: temporal_rows.clone(),
            }).unwrap(),
            20, i64::MAX,
        ).unwrap();
        let EffectResult::Estimated(temporal_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &temporal_source.id).unwrap() else {
                panic!("expected temporal estimate")
            };
        let holdout = temporal_estimate.temporal_holdout.as_ref().unwrap();
        assert_eq!((holdout.training_units, holdout.holdout_units), (20, 20));
        assert!((holdout.training_estimate + 2.0).abs() < 1e-10);
        assert!((holdout.holdout_estimate + 4.0).abs() < 1e-10);
        assert!((holdout.abs_gap - 2.0).abs() < 1e-10);
        assert!(!holdout.sign_flip);
        let temporal_read = store.read_effect_estimate(&scope, &temporal_estimate.id).unwrap();
        let EffectResult::Estimated(read_back) = temporal_read else {
            panic!("expected persisted temporal estimate: {temporal_read:?}")
        };
        assert_eq!(read_back.temporal_holdout, temporal_estimate.temporal_holdout);
        let no_overlap_source = store.add_artifact(
            &scope, "causal_dataset", "no-overlap", "v1", "no-overlap-v1",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: Some(80),
                negative_control: None,
                units: temporal_rows,
            }).unwrap(),
            20, i64::MAX,
        ).unwrap();
        assert!(matches!(
            store.estimate_stratified_effect(&scope, &model.id, &no_overlap_source.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let mut heldout_rows = rows.clone();
        for unit in &mut heldout_rows {
            unit.unit_id = format!("heldout-{}", unit.unit_id);
            unit.adjustment_measured_at = Some(55);
            unit.treatment_assigned_at = 60;
            unit.outcome_recorded_at = 70;
        }
        let evaluation_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-heldout", "v1", "effect-eval-heldout-lineage",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: None,
                negative_control: None,
                units: heldout_rows.clone(),
            }).unwrap(),
            80, i64::MAX,
        ).unwrap();
        let EffectResult::Estimated(evaluation_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &evaluation_source.id).unwrap() else {
                panic!("expected held-out estimate")
            };
        let mut reversal_rows = rows.clone();
        for (index, unit) in reversal_rows.iter_mut().enumerate() {
            unit.unit_id = format!("reversal-{}", unit.unit_id);
            let late = index % 20 >= 10;
            unit.adjustment_measured_at = Some(if late { 75 } else { 55 });
            unit.treatment_assigned_at = if late { 80 } else { 60 };
            unit.outcome_recorded_at = if late { 90 } else { 70 };
            if late && unit.treated {
                unit.outcome = 12.0;
            }
        }
        let reversal_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-reversal", "v1", "effect-eval-reversal-lineage",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: Some(70),
                negative_control: None,
                units: reversal_rows,
            }).unwrap(),
            95, i64::MAX,
        ).unwrap();
        let EffectResult::Estimated(reversal_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &reversal_source.id).unwrap() else {
                panic!("expected held-out reversal estimate")
            };
        assert_eq!(reversal_estimate.temporal_holdout.as_ref().map(|value| value.sign_flip), Some(true));
        let mut imbalanced_heldout_rows = heldout_rows.clone();
        for unit in &mut imbalanced_heldout_rows {
            unit.unit_id = format!("pre-imbalance-{}", unit.unit_id);
            unit.pre_treatment_outcome = Some(if unit.treated { 7.0 } else { 5.0 });
            unit.pre_treatment_outcome_recorded_at = Some(55);
        }
        let imbalanced_heldout_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-pre-imbalance", "v1", "effect-eval-pre-imbalance-lineage",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: None,
                negative_control: None,
                units: imbalanced_heldout_rows,
            }).unwrap(),
            80, i64::MAX,
        ).unwrap();
        let EffectResult::Estimated(imbalanced_heldout_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &imbalanced_heldout_source.id).unwrap() else {
                panic!("expected held-out pre-treatment imbalance estimate")
            };
        assert_eq!(imbalanced_heldout_estimate.pre_treatment_placebo.as_ref().map(|value| value.imbalance_flag), Some(true));
        let mut influential_rows = heldout_rows.clone();
        for unit in &mut influential_rows {
            unit.unit_id = format!("influential-{}", unit.unit_id);
        }
        // One treated unit has a large baseline shock independent of the
        // treatment's known -2 DGP effect.
        influential_rows[0].outcome = 100.0;
        let influential_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-influential", "v1", "effect-eval-influential-lineage",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: None,
                negative_control: None,
                units: influential_rows,
            }).unwrap(),
            80, i64::MAX,
        ).unwrap();
        let EffectResult::Estimated(influential_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &influential_source.id).unwrap() else {
                panic!("expected held-out influential-unit estimate")
            };
        assert_eq!(influential_estimate.leave_one_unit_out.sign_flip, Some(true));
        let effect_evaluation = EffectEvalDataset {
            version: "causal-effect-eval-v1".into(),
            truth_basis: "synthetic_dgp".into(),
            model_id: model.id.clone(),
            cutoff_unix: 50,
            cases: vec![
                EffectEvalCase { id: "training".into(), estimate_id: value.id.clone(), true_effect: -2.0, expected_diagnostics: None },
                EffectEvalCase { id: "held-out".into(), estimate_id: evaluation_estimate.id.clone(), true_effect: -2.0,
                    expected_diagnostics: Some(ExpectedDiagnosticFlags {
                        unit_deletion_sign_flip: Some(false), temporal_sign_flip: Some(false),
                        pre_treatment_imbalance: None, negative_control_imbalance: None,
                    }) },
                EffectEvalCase { id: "held-out-reversal".into(), estimate_id: reversal_estimate.id.clone(), true_effect: 0.0,
                    expected_diagnostics: Some(ExpectedDiagnosticFlags {
                        unit_deletion_sign_flip: None, temporal_sign_flip: Some(true),
                        pre_treatment_imbalance: None, negative_control_imbalance: None,
                    }) },
                EffectEvalCase { id: "held-out-pre-imbalance".into(), estimate_id: imbalanced_heldout_estimate.id.clone(), true_effect: -2.0,
                    expected_diagnostics: Some(ExpectedDiagnosticFlags {
                        unit_deletion_sign_flip: None, temporal_sign_flip: None,
                        pre_treatment_imbalance: Some(true), negative_control_imbalance: None,
                    }) },
                EffectEvalCase { id: "held-out-influential".into(), estimate_id: influential_estimate.id.clone(), true_effect: -2.0,
                    expected_diagnostics: Some(ExpectedDiagnosticFlags {
                        unit_deletion_sign_flip: Some(true), temporal_sign_flip: None,
                        pre_treatment_imbalance: None, negative_control_imbalance: None,
                    }) },
            ],
        };
        let report = evaluate_effects(&store, &scope, &effect_evaluation).unwrap();
        assert_eq!((report.training_cases, report.held_out_cases, report.scored_cases), (1, 4, 4));
        assert_eq!(report.unknown_cases, 0);
        assert!(report.mean_absolute_error.is_some_and(|value| (value - 1.15).abs() < 1e-10));
        assert!(report.interval_coverage.is_some());
        assert_eq!(report.unit_deletion_detection.true_negative, 1);
        assert_eq!(report.unit_deletion_detection.true_positive, 1);
        assert_eq!(report.temporal_detection.unavailable_negative, 1);
        assert_eq!(report.temporal_detection.true_positive, 1);
        assert_eq!(report.temporal_detection.positive_detection_rate_all_labeled, Some(1.0));
        assert_eq!(report.pre_treatment_detection.true_positive, 1);
        let relabeled_old_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-relabeled-old", "v1", "late-export-old-cohort",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: None, negative_control: None, units: rows.clone(),
            }).unwrap(),
            80, i64::MAX,
        ).unwrap();
        let EffectResult::Estimated(relabeled_old_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &relabeled_old_source.id).unwrap() else {
                panic!("expected arithmetic estimate on old cohort")
            };
        let relabeled_evaluation = EffectEvalDataset {
            version: "causal-effect-eval-v1".into(), truth_basis: "synthetic_dgp".into(),
            model_id: model.id.clone(), cutoff_unix: 50,
            cases: vec![
                EffectEvalCase { id: "training".into(), estimate_id: value.id.clone(), true_effect: -2.0, expected_diagnostics: None },
                EffectEvalCase { id: "false-heldout".into(), estimate_id: relabeled_old_estimate.id, true_effect: -2.0, expected_diagnostics: None },
            ],
        };
        assert!(evaluate_effects(&store, &scope, &relabeled_evaluation).is_err());
        let mut reordered_rows = heldout_rows.clone();
        reordered_rows.reverse();
        let reordered_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-reordered", "v1", "claimed-reordered-cohort",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: None, negative_control: None, units: reordered_rows,
            }).unwrap(),
            80, i64::MAX,
        ).unwrap();
        assert_ne!(reordered_source.content_sha256, evaluation_source.content_sha256);
        let EffectResult::Estimated(reordered_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &reordered_source.id).unwrap() else {
                panic!("expected reordered-cohort estimate")
            };
        let reordered_evaluation = EffectEvalDataset {
            version: "causal-effect-eval-v1".into(), truth_basis: "synthetic_dgp".into(),
            model_id: model.id.clone(), cutoff_unix: 50,
            cases: vec![
                EffectEvalCase { id: "training".into(), estimate_id: value.id.clone(),
                    true_effect: -2.0, expected_diagnostics: None },
                EffectEvalCase { id: "held-original".into(), estimate_id: evaluation_estimate.id.clone(),
                    true_effect: -2.0, expected_diagnostics: None },
                EffectEvalCase { id: "held-reordered".into(), estimate_id: reordered_estimate.id,
                    true_effect: -2.0, expected_diagnostics: None },
            ],
        };
        assert!(evaluate_effects(&store, &scope, &reordered_evaluation)
            .unwrap_err().contains("duplicate held-out cohort units"));
        let mut remapped_rows = heldout_rows.clone();
        for (index, unit) in remapped_rows.iter_mut().enumerate() {
            unit.unit_id = format!("renamed-{index}");
        }
        remapped_rows.reverse();
        let remapped_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-remapped", "v1", "claimed-remapped-cohort",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: None, negative_control: None, units: remapped_rows,
            }).unwrap(),
            80, i64::MAX,
        ).unwrap();
        assert_ne!(remapped_source.content_sha256, evaluation_source.content_sha256);
        let EffectResult::Estimated(remapped_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &remapped_source.id).unwrap() else {
                panic!("expected remapped-cohort arithmetic estimate")
            };
        let remapped_evaluation = EffectEvalDataset {
            version: "causal-effect-eval-v1".into(), truth_basis: "synthetic_dgp".into(),
            model_id: model.id.clone(), cutoff_unix: 50,
            cases: vec![
                EffectEvalCase { id: "training".into(), estimate_id: value.id.clone(),
                    true_effect: -2.0, expected_diagnostics: None },
                EffectEvalCase { id: "held-original".into(), estimate_id: evaluation_estimate.id.clone(),
                    true_effect: -2.0, expected_diagnostics: None },
                EffectEvalCase { id: "held-remapped".into(), estimate_id: remapped_estimate.id,
                    true_effect: -2.0, expected_diagnostics: None },
            ],
        };
        assert!(evaluate_effects(&store, &scope, &remapped_evaluation)
            .unwrap_err().contains("duplicate held-out cohort units"));
        store.begin_ccr_revocation(&scope, &evaluation_source.id).unwrap();
        let staged_revocation = evaluate_effects(&store, &scope, &effect_evaluation).unwrap();
        assert_eq!(staged_revocation.unknown_cases, 1);
        store.invalidate_artifact(&scope, &evaluation_source.id).unwrap();
        let after_revocation = evaluate_effects(&store, &scope, &effect_evaluation).unwrap();
        assert_eq!(after_revocation.scored_cases, 3);
        assert_eq!(after_revocation.unknown_cases, 1);
        assert!(after_revocation.interval_coverage.is_some());
        assert_eq!(after_revocation.unit_deletion_detection.true_positive, 1);
        assert_eq!(after_revocation.unit_deletion_detection.unavailable_negative, 1);
        assert_eq!(after_revocation.temporal_detection.true_positive, 1);
        assert_eq!(after_revocation.temporal_detection.unavailable_negative, 1);
        assert_eq!(after_revocation.pre_treatment_detection.true_positive, 1);
        let copied_lineage_source = store.add_artifact(
            &scope, "causal_dataset", "effect-eval-copy", "v1", "claimed-new-cohort",
            &serde_json::to_string(&ObservedDataset {
                adjustment_variable_ids: vec![cohort.id.clone()],
                evaluation_cutoff: None,
                negative_control: None,
                units: heldout_rows,
            }).unwrap(),
            80, i64::MAX,
        ).unwrap();
        let EffectResult::Estimated(copied_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &copied_lineage_source.id).unwrap() else {
                panic!("expected copied-lineage estimate")
            };
        let mut copied_evaluation = effect_evaluation;
        copied_evaluation.cases.push(EffectEvalCase {
            id: "held-out-relabeled-copy".into(), estimate_id: copied_estimate.id,
            true_effect: -2.0, expected_diagnostics: None,
        });
        assert!(evaluate_effects(&store, &scope, &copied_evaluation)
            .unwrap_err().contains("duplicate held-out source content"));
        let mut mislabeled = rows.clone();
        mislabeled[0]
            .adjustment_values
            .insert(cohort.id.clone(), "high".into());
        let mut late = rows.clone();
        late[0].adjustment_measured_at = Some(25);
        for (name, units) in [("mislabeled", mislabeled), ("late", late)] {
            let bad_source = store
                .add_artifact(
                    &scope,
                    "causal_dataset",
                    name,
                    "v1",
                    name,
                    &serde_json::to_string(&ObservedDataset {
                        adjustment_variable_ids: vec![cohort.id.clone()],
                        evaluation_cutoff: None,
                        negative_control: None,
                        units,
                    })
                    .unwrap(),
                    20,
                    i64::MAX,
                )
                .unwrap();
            assert!(matches!(
                store
                    .estimate_stratified_effect(&scope, &model.id, &bad_source.id)
                    .unwrap(),
                EffectResult::Unknown { .. }
            ));
        }
        let other_scope = EvidenceScope {
            tenant_id: "other".into(),
            acl: scope.acl.clone(),
        };
        assert!(matches!(
            store.read_effect_estimate(&other_scope, &value.id),
            Err(CausalStoreError::NotFound)
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET content='tampered' WHERE id=?1",
            [&dataset.id],
        )
        .unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let original_dataset = serde_json::to_string(&ObservedDataset {
            adjustment_variable_ids: vec![cohort.id.clone()],
            evaluation_cutoff: None,
            negative_control: None,
            units: rows.clone(),
        })
        .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET content=?1 WHERE id=?2",
            params![original_dataset, dataset.id],
        )
        .unwrap();
        // Direct SQL changes queue a CCR notice even if bytes are restored.
        // This estimator test simulates the downstream tombstone delivery
        // before continuing unrelated numeric assertions.
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let notice = store.pending_ccr_revocations(100).unwrap()
            .into_iter().find(|notice| notice.artifact_id == dataset.id).unwrap();
        store.acknowledge_ccr_revocation(&notice).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Estimated(_)
        ));
        let mut changed_rows = rows.clone();
        changed_rows[0].outcome = 999.0;
        let changed_dataset = serde_json::to_string(&ObservedDataset {
            adjustment_variable_ids: vec![cohort.id.clone()],
            evaluation_cutoff: None,
            negative_control: None,
            units: changed_rows,
        }).unwrap();
        let changed_digest = format!("{:x}", Sha256::digest(changed_dataset.as_bytes()));
        conn.execute(
            "UPDATE causal_artifacts SET content=?1, content_sha256=?2 WHERE id=?3",
            params![changed_dataset, changed_digest, dataset.id],
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        conn.execute(
            "UPDATE causal_artifacts SET content=?1, content_sha256=?2 WHERE id=?3",
            params![original_dataset, value.data_sha256, dataset.id],
        ).unwrap();
        let notice = store.pending_ccr_revocations(100).unwrap()
            .into_iter().find(|notice| notice.artifact_id == dataset.id).unwrap();
        store.acknowledge_ccr_revocation(&notice).unwrap();
        let mut sparse = rows.clone();
        for unit in sparse.iter_mut().filter(|unit| unit.stratum == "high") {
            unit.treated = true;
        }
        let sparse_source = store
            .add_artifact(
                &scope,
                "causal_dataset",
                "sparse",
                "v1",
                "sparse-v1",
                &serde_json::to_string(&ObservedDataset {
                    adjustment_variable_ids: vec![cohort.id.clone()],
                    evaluation_cutoff: None,
                    negative_control: None,
                    units: sparse,
                })
                .unwrap(),
                20,
                i64::MAX,
            )
            .unwrap();
        assert!(matches!(
            store
                .estimate_stratified_effect(&scope, &model.id, &sparse_source.id)
                .unwrap(),
            EffectResult::Unknown { .. }
        ));
        let protocol = store.add_artifact(
            &scope, "negative_control_protocol", "exclusion", "v1", "nco-protocol-v1",
            "Synthetic protocol: this service metric shares cohort selection but staffing cannot affect its data-generating process.",
            1, i64::MAX,
        ).unwrap();
        let review_id = store.review_negative_control(
            &scope, &model.id, &negative_outcome.id, &protocol.id,
            AssumptionVerdict::Pass,
            "Synthetic generation excludes every staffing-to-control path.",
            "reviewer",
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Estimated(_)
        ));
        let mut balanced_control = rows.clone();
        for unit in &mut balanced_control {
            unit.negative_control_outcome = Some(5.0);
            unit.negative_control_recorded_at = Some(25);
        }
        let control_dataset = |name: &str, units: Vec<ObservedUnit>, review_id: &str| {
            store.add_artifact(
                &scope, "causal_dataset", name, "v1", name,
                &serde_json::to_string(&ObservedDataset {
                    adjustment_variable_ids: vec![cohort.id.clone()],
                    evaluation_cutoff: None,
                    negative_control: Some(NegativeControlPlan {
                        variable_id: negative_outcome.id.clone(),
                        review_id: review_id.into(),
                    }),
                    units,
                }).unwrap(),
                20, i64::MAX,
            ).unwrap()
        };
        let balanced_source = control_dataset("nco-balanced", balanced_control.clone(), &review_id);
        let EffectResult::Estimated(balanced_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &balanced_source.id).unwrap() else {
                panic!("expected reviewed negative-control estimate")
            };
        let control = balanced_estimate.negative_control.as_ref().unwrap();
        assert_eq!(control.review_id, review_id);
        assert_eq!(control.contrast, 0.0);
        assert!(!control.imbalance_flag);
        let EffectResult::Estimated(read_back) = store
            .read_effect_estimate(&scope, &balanced_estimate.id).unwrap() else {
                panic!("expected persisted negative-control diagnostic")
            };
        assert_eq!(read_back.negative_control, balanced_estimate.negative_control);
        for (name, invalid_value) in [("nco-fractional", 5.5), ("nco-negative", -1.0)] {
            let mut invalid = balanced_control.clone();
            invalid[0].negative_control_outcome = Some(invalid_value);
            let source = control_dataset(name, invalid, &review_id);
            assert!(matches!(
                store.estimate_stratified_effect(&scope, &model.id, &source.id).unwrap(),
                EffectResult::Unknown { .. }
            ));
        }
        let mut imbalanced_control = balanced_control.clone();
        for unit in &mut imbalanced_control {
            if unit.treated { unit.negative_control_outcome = Some(7.0); }
        }
        let imbalanced_source = control_dataset("nco-imbalanced", imbalanced_control, &review_id);
        let EffectResult::Estimated(imbalanced_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &imbalanced_source.id).unwrap() else {
                panic!("expected imbalance diagnostic")
            };
        assert!(imbalanced_estimate.negative_control.unwrap().imbalance_flag);
        let mut incomplete_control = balanced_control.clone();
        incomplete_control[0].negative_control_outcome = None;
        let incomplete_source = control_dataset("nco-incomplete", incomplete_control, &review_id);
        assert!(matches!(
            store.estimate_stratified_effect(&scope, &model.id, &incomplete_source.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let mut late_control = balanced_control.clone();
        late_control[0].negative_control_recorded_at = Some(101);
        let late_source = control_dataset("nco-late", late_control, &review_id);
        assert!(matches!(
            store.estimate_stratified_effect(&scope, &model.id, &late_source.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let fail_review = store.review_negative_control(
            &scope, &model.id, &negative_outcome.id, &protocol.id,
            AssumptionVerdict::Fail,
            "Review found an unexcluded staffing-to-control path in the protocol.",
            "reviewer",
        ).unwrap();
        assert_ne!(fail_review, review_id);
        assert!(matches!(
            store.read_effect_estimate(&scope, &balanced_estimate.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let scrubbed: Option<f64> = conn.query_row(
            "SELECT estimate FROM causal_effect_estimates WHERE id=?1",
            [&balanced_estimate.id], |row| row.get(0),
        ).unwrap();
        assert!(scrubbed.is_none());
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Estimated(_)
        ));
        let new_review = store.review_negative_control(
            &scope, &model.id, &negative_outcome.id, &protocol.id,
            AssumptionVerdict::Pass,
            "A revised synthetic exclusion protocol was independently checked.",
            "reviewer",
        ).unwrap();
        let fresh_source = control_dataset("nco-fresh", balanced_control, &new_review);
        let EffectResult::Estimated(fresh_estimate) = store
            .estimate_stratified_effect(&scope, &model.id, &fresh_source.id).unwrap() else {
                panic!("expected new reviewed plan to estimate")
            };
        store.begin_ccr_revocation(&scope, &protocol.id).unwrap();
        assert_eq!(
            store.latest_negative_control_review(&scope, &model.id, &negative_outcome.id)
                .unwrap().unwrap().rationale,
            ""
        );
        assert!(matches!(
            store.read_effect_estimate(&scope, &fresh_estimate.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        store.invalidate_artifact(&scope, &protocol.id).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &fresh_estimate.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let scrubbed: Option<f64> = conn.query_row(
            "SELECT estimate FROM causal_effect_estimates WHERE id=?1",
            [&fresh_estimate.id], |row| row.get(0),
        ).unwrap();
        assert!(scrubbed.is_none());
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Estimated(_)
        ));
        store.record_assumption(
            &scope, &model.id, AssumptionKind::Exchangeability,
            AssumptionVerdict::Pass,
            "Revised synthetic exchangeability review with new supporting rationale",
            "reviewer",
        ).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let invalidated_by_review: Option<f64> = conn.query_row(
            "SELECT estimate FROM causal_effect_estimates WHERE id=?1",
            [&value.id], |row| row.get(0),
        ).unwrap();
        assert!(invalidated_by_review.is_none());
        store.invalidate_artifact(&scope, &source.id).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let revoked_value: Option<f64> = conn
            .query_row(
                "SELECT estimate FROM causal_effect_estimates WHERE id=?1",
                [&value.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(revoked_value.is_none());
        store.erase_artifact(&scope, &dataset.id).unwrap();
        assert!(matches!(
            store.read_effect_estimate(&scope, &value.id).unwrap(),
            EffectResult::Unknown { .. }
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let erased_value: Option<f64> = conn
            .query_row(
                "SELECT estimate FROM causal_effect_estimates WHERE id=?1",
                [&value.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(erased_value.is_none());
        assert!(matches!(
            store
                .estimate_stratified_effect(&scope, &model.id, &dataset.id)
                .unwrap(),
            EffectResult::Unknown { .. }
        ));
    }
}
