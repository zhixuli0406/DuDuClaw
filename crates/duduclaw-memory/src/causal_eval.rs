//! Offline, time-split scoring for source-grounded causal extraction.
//! A synthetic fixture checks the evaluator, not real model accuracy.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::causal::{ClaimModality, ProposedCausalClaim, valid_proposal};
use crate::causal_extract::{build_extraction_prompt, parse_extraction_response};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionEvalDataset {
    pub version: String,
    pub cutoff_unix: i64,
    pub cases: Vec<ExtractionEvalCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionEvalCase {
    pub id: String,
    pub occurred_at: i64,
    pub source_lineage_id: String,
    /// Human-curated upstream source family. This is an assertion, not an
    /// automatically inferred provenance identity.
    #[serde(default)]
    pub source_family_id: String,
    pub question: String,
    pub source_text: String,
    pub gold: Vec<ProposedCausalClaim>,
    /// Some means every source claim in this case has a human edge decision.
    /// None means no reviewed-edge score is computed for this case.
    #[serde(default)]
    pub reviewed_edges: Option<Vec<ReviewedEdge>>,
    /// Exact provider response text, including invalid responses for failure scoring.
    pub response_json: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedEdge {
    pub cause_variable: String,
    pub effect_variable: String,
    pub lag_min_seconds: i64,
    pub lag_max_seconds: i64,
    pub modality: crate::causal::ClaimModality,
    pub accepted: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionCompareDataset {
    pub version: String,
    pub cutoff_unix: i64,
    pub cases: Vec<ExtractionCompareCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionCompareCase {
    pub id: String,
    pub occurred_at: i64,
    pub source_lineage_id: String,
    #[serde(default)]
    pub source_family_id: String,
    pub question: String,
    pub source_text: String,
    pub gold: Vec<ProposedCausalClaim>,
    #[serde(default)]
    pub reviewed_edges: Option<Vec<ReviewedEdge>>,
    pub responses: Vec<NamedExtractionResponse>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NamedExtractionResponse {
    pub model_id: String,
    pub response_json: String,
}

#[derive(Debug, Serialize)]
pub struct ModelExtractionReport {
    pub model_id: String,
    pub report: ExtractionEvalReport,
}

#[derive(Debug, Serialize)]
pub struct PairedExtractionComparison {
    pub first_model: String,
    pub second_model: String,
    pub first_fewer_full_claim_errors: usize,
    pub second_fewer_full_claim_errors: usize,
    pub full_claim_ties: usize,
    pub reviewed_cases: usize,
    pub first_fewer_reviewed_edge_errors: usize,
    pub second_fewer_reviewed_edge_errors: usize,
    pub reviewed_edge_ties: usize,
}

#[derive(Debug, Serialize)]
pub struct ExtractionCompareReport {
    pub dataset_sha256: String,
    pub cutoff_unix: i64,
    pub held_out_cases: usize,
    pub held_out_families: usize,
    pub models: Vec<ModelExtractionReport>,
    pub paired: Vec<PairedExtractionComparison>,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct EvalCount {
    pub true_positive: usize,
    pub predicted: usize,
    pub gold: usize,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub f1: Option<f64>,
}

impl EvalCount {
    fn new(true_positive: usize, predicted: usize, gold: usize) -> Self {
        let precision = (predicted > 0).then(|| true_positive as f64 / predicted as f64);
        let recall = (gold > 0).then(|| true_positive as f64 / gold as f64);
        let f1 = match (precision, recall) {
            (Some(p), Some(r)) if p + r > 0.0 => Some(2.0 * p * r / (p + r)),
            (Some(_), Some(_)) => Some(0.0),
            _ => None,
        };
        Self {
            true_positive,
            predicted,
            gold,
            precision,
            recall,
            f1,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ExtractionEvalReport {
    pub dataset_sha256: String,
    pub cutoff_unix: i64,
    pub training_cases: usize,
    pub held_out_cases: usize,
    pub held_out_families: usize,
    pub invalid_responses: usize,
    pub ungrounded_predictions: usize,
    pub full_claim: EvalCount,
    pub exact_span: EvalCount,
    /// Field-correct claims require a matched, grounded source span. Counts
    /// include all predictions and labels, including unmatched claims.
    pub direction_claim: EvalCount,
    pub stance_claim: EvalCount,
    pub modality_claim: EvalCount,
    pub lag_claim: EvalCount,
    /// Positive class is an explicitly negated claim at a matched source span.
    pub negation_claim: EvalCount,
    pub independent_lineage_claim: EvalCount,
    pub reviewed_cases: usize,
    pub reviewed_families: usize,
    pub reviewed_edge_case: EvalCount,
    pub rejected_edges_proposed: usize,
    pub matched_span_direction_correct: usize,
    pub matched_span_modality_correct: usize,
    pub matched_span_lag_correct: usize,
    pub matched_span_speaker_correct: usize,
    pub limitations: Vec<&'static str>,
}

fn edge_key(
    cause: &str,
    effect: &str,
    lag_min: i64,
    lag_max: i64,
    modality: crate::causal::ClaimModality,
) -> String {
    serde_json::to_string(&(cause, effect, lag_min, lag_max, modality))
        .expect("serializing simple edge key")
}

fn claim_edge_key(claim: &ProposedCausalClaim) -> String {
    edge_key(
        &claim.cause_variable,
        &claim.effect_variable,
        claim.lag_min_seconds,
        claim.lag_max_seconds,
        claim.modality,
    )
}

fn span_eq(a: &ProposedCausalClaim, b: &ProposedCausalClaim) -> bool {
    a.span_start == b.span_start && a.span_end == b.span_end && a.excerpt == b.excerpt
}

fn full_eq(a: &ProposedCausalClaim, b: &ProposedCausalClaim) -> bool {
    span_eq(a, b)
        && a.cause_variable == b.cause_variable
        && a.effect_variable == b.effect_variable
        && a.stance == b.stance
        && a.modality == b.modality
        && a.lag_min_seconds == b.lag_min_seconds
        && a.lag_max_seconds == b.lag_max_seconds
        && a.speaker_id == b.speaker_id
}

fn max_matches<F>(
    predictions: &[&ProposedCausalClaim],
    gold: &[ProposedCausalClaim],
    matches: F,
) -> usize
where
    F: Fn(&ProposedCausalClaim, &ProposedCausalClaim) -> bool,
{
    fn assign<F>(
        prediction_index: usize,
        predictions: &[&ProposedCausalClaim],
        gold: &[ProposedCausalClaim],
        matches: &F,
        seen: &mut [bool],
        owner: &mut [Option<usize>],
    ) -> bool
    where
        F: Fn(&ProposedCausalClaim, &ProposedCausalClaim) -> bool,
    {
        for (gold_index, item) in gold.iter().enumerate() {
            if seen[gold_index] || !matches(predictions[prediction_index], item) {
                continue;
            }
            seen[gold_index] = true;
            if owner[gold_index]
                .is_none_or(|previous| assign(previous, predictions, gold, matches, seen, owner))
            {
                owner[gold_index] = Some(prediction_index);
                return true;
            }
        }
        false
    }

    let mut owner = vec![None; gold.len()];
    let mut count = 0;
    for prediction_index in 0..predictions.len() {
        let mut seen = vec![false; gold.len()];
        count += usize::from(assign(
            prediction_index,
            predictions,
            gold,
            &matches,
            &mut seen,
            &mut owner,
        ));
    }
    count
}

fn lineage_key(family: usize, claim: &ProposedCausalClaim) -> String {
    serde_json::to_string(&(
        family,
        &claim.cause_variable,
        &claim.effect_variable,
        claim.stance,
        claim.modality,
        claim.lag_min_seconds,
        claim.lag_max_seconds,
    ))
    .expect("serializing simple claim key")
}

fn full_family_key(family: usize, claim: &ProposedCausalClaim) -> String {
    serde_json::to_string(&(
        family,
        &claim.cause_variable,
        &claim.effect_variable,
        claim.stance,
        claim.modality,
        claim.lag_min_seconds,
        claim.lag_max_seconds,
        &claim.speaker_id,
    ))
    .expect("serializing full family claim key")
}

fn family_edge_key(family: usize, edge: &str) -> String {
    serde_json::to_string(&(family, edge)).expect("serializing family edge key")
}

fn source_family_indexes(cases: &[ExtractionEvalCase]) -> Vec<usize> {
    fn root(parents: &mut [usize], mut index: usize) -> usize {
        while parents[index] != index {
            parents[index] = parents[parents[index]];
            index = parents[index];
        }
        index
    }
    let mut lineage_ids: HashMap<&str, usize> = HashMap::new();
    let mut declared_families: HashMap<&str, usize> = HashMap::new();
    let mut digest_owner: HashMap<String, usize> = HashMap::new();
    let mut parents = Vec::new();
    let mut case_indexes = Vec::with_capacity(cases.len());
    for case in cases {
        let index = *lineage_ids
            .entry(&case.source_lineage_id)
            .or_insert_with(|| {
                let index = parents.len();
                parents.push(index);
                index
            });
        if let Some(&other) = declared_families.get(case.source_family_id.as_str()) {
            let current_root = root(&mut parents, index);
            let other_root = root(&mut parents, other);
            parents[other_root] = current_root;
        } else {
            declared_families.insert(&case.source_family_id, index);
        }
        let digest = format!("{:x}", Sha256::digest(case.source_text.as_bytes()));
        if let Some(&other) = digest_owner.get(&digest) {
            let current_root = root(&mut parents, index);
            let other_root = root(&mut parents, other);
            parents[other_root] = current_root;
        } else {
            digest_owner.insert(digest, index);
        }
        case_indexes.push(index);
    }
    case_indexes
        .into_iter()
        .map(|index| root(&mut parents, index))
        .collect()
}

/// Scores only cases after the fixed cutoff. Explicit source families,
/// lineages, and exact source copies may not cross the time split.
pub fn evaluate_extraction(
    dataset: &ExtractionEvalDataset,
) -> Result<ExtractionEvalReport, String> {
    if dataset.version != "causal-extraction-eval-v2" || dataset.cases.is_empty() {
        return Err(
            "extraction evaluation requires nonempty v2 dataset with source_family_id annotations"
                .into(),
        );
    }
    let mut ids = HashSet::new();
    for case in &dataset.cases {
        if case.source_family_id.trim().is_empty()
            || case.source_family_id.trim() != case.source_family_id
        {
            return Err(format!(
                "missing or noncanonical source_family_id annotation in evaluation case: {}",
                case.id
            ));
        }
        if case.id.trim().is_empty()
            || case.source_lineage_id.trim().is_empty()
            || !ids.insert(&case.id)
            || build_extraction_prompt(&case.question, &case.source_text).is_err()
            || case.gold.iter().any(|claim| {
                !valid_proposal(claim)
                    || case.source_text.get(claim.span_start..claim.span_end)
                        != Some(claim.excerpt.as_str())
            })
        {
            return Err(format!("invalid evaluation case or gold span: {}", case.id));
        }
        if let Some(reviewed) = &case.reviewed_edges {
            let gold_edges: HashSet<_> = case.gold.iter().map(claim_edge_key).collect();
            let mut reviewed_keys = HashSet::new();
            if reviewed.len() != gold_edges.len()
                || reviewed.iter().any(|edge| {
                    let key = edge_key(
                        &edge.cause_variable,
                        &edge.effect_variable,
                        edge.lag_min_seconds,
                        edge.lag_max_seconds,
                        edge.modality,
                    );
                    !gold_edges.contains(&key) || !reviewed_keys.insert(key)
                })
            {
                return Err(format!(
                    "reviewed edges must adjudicate every distinct gold edge exactly once: {}",
                    case.id
                ));
            }
        }
    }
    let families = source_family_indexes(&dataset.cases);
    let mut train_lineages = HashSet::new();
    let mut held_lineages = HashSet::new();
    for (case, family) in dataset.cases.iter().zip(&families) {
        if case.occurred_at <= dataset.cutoff_unix {
            train_lineages.insert(*family);
        } else {
            held_lineages.insert(*family);
        }
    }
    if train_lineages.is_empty()
        || held_lineages.is_empty()
        || !train_lineages.is_disjoint(&held_lineages)
    {
        return Err(
            "evaluation requires nonempty time splits with disjoint declared source families, lineages, and exact source copies".into(),
        );
    }

    // Reviewed-edge agreement must cover a whole held-out family. Otherwise
    // selective annotation of a paraphrase could hide a model's mistakes.
    let mut family_review_coverage: HashMap<usize, bool> = HashMap::new();
    let mut family_review_decisions: HashMap<String, bool> = HashMap::new();
    for (case, family) in dataset.cases.iter().zip(&families) {
        if case.occurred_at <= dataset.cutoff_unix {
            continue;
        }
        let reviewed = case.reviewed_edges.is_some();
        if family_review_coverage
            .insert(*family, reviewed)
            .is_some_and(|prior| prior != reviewed)
        {
            return Err(format!(
                "reviewed edges must cover every held-out case in source family: {}",
                case.source_family_id
            ));
        }
        if let Some(edges) = &case.reviewed_edges {
            for edge in edges {
                let key = family_edge_key(
                    *family,
                    &edge_key(
                        &edge.cause_variable,
                        &edge.effect_variable,
                        edge.lag_min_seconds,
                        edge.lag_max_seconds,
                        edge.modality,
                    ),
                );
                if family_review_decisions
                    .insert(key, edge.accepted)
                    .is_some_and(|prior| prior != edge.accepted)
                {
                    return Err(format!(
                        "conflicting reviewed-edge decisions in source family: {}",
                        case.source_family_id
                    ));
                }
            }
        }
    }

    let mut full_family_gold = HashSet::new();
    let mut full_family_predicted = HashSet::new();
    let mut full_family_ungrounded = HashSet::new();
    let mut full_family_tp = HashSet::new();
    let mut span_tp = 0;
    let mut predicted = 0;
    let mut gold = 0;
    let mut invalid_responses = 0;
    let mut ungrounded_predictions = 0;
    let mut direction = 0;
    let mut direction_only = 0;
    let mut stance = 0;
    let mut modality = 0;
    let mut lag = 0;
    let mut speaker = 0;
    let mut negation = 0;
    let mut predicted_negated = 0;
    let mut gold_negated = 0;
    let mut gold_lineage_claims = HashSet::new();
    let mut predicted_lineage_claims = HashSet::new();
    let mut reviewed_cases = 0;
    let mut reviewed_family_gold = HashSet::new();
    let mut reviewed_family_predicted = HashSet::new();
    let mut reviewed_family_ungrounded = HashSet::new();
    let mut reviewed_family_rejected = HashSet::new();
    for (key, accepted) in family_review_decisions {
        if accepted {
            reviewed_family_gold.insert(key);
        } else {
            reviewed_family_rejected.insert(key);
        }
    }
    for (case, family) in dataset
        .cases
        .iter()
        .zip(&families)
        .filter(|(case, _)| case.occurred_at > dataset.cutoff_unix)
    {
        gold += case.gold.len();
        gold_negated += case
            .gold
            .iter()
            .filter(|claim| claim.modality == ClaimModality::Negated)
            .count();
        for claim in &case.gold {
            gold_lineage_claims.insert(lineage_key(*family, claim));
            full_family_gold.insert(full_family_key(*family, claim));
        }
        let claims = match parse_extraction_response(&case.response_json) {
            Ok(parsed) => parsed.claims,
            Err(_) => {
                invalid_responses += 1;
                if case.reviewed_edges.is_some() {
                    reviewed_cases += 1;
                }
                continue;
            }
        };
        predicted += claims.len();
        predicted_negated += claims
            .iter()
            .filter(|claim| claim.modality == ClaimModality::Negated)
            .count();
        let mut grounded = vec![false; claims.len()];
        for (prediction_index, claim) in claims.iter().enumerate() {
            if !valid_proposal(claim)
                || case.source_text.get(claim.span_start..claim.span_end)
                    != Some(claim.excerpt.as_str())
            {
                ungrounded_predictions += 1;
                full_family_ungrounded.insert(full_family_key(*family, claim));
                continue;
            }
            grounded[prediction_index] = true;
            full_family_predicted.insert(full_family_key(*family, claim));
            predicted_lineage_claims.insert(lineage_key(*family, claim));
            if case.gold.iter().any(|gold| full_eq(claim, gold)) {
                full_family_tp.insert(full_family_key(*family, claim));
            }
        }
        if case.reviewed_edges.is_some() {
            reviewed_cases += 1;
            let proposed_edges: HashSet<_> = claims
                .iter()
                .zip(&grounded)
                .filter_map(|(claim, grounded)| grounded.then(|| claim_edge_key(claim)))
                .collect();
            reviewed_family_predicted.extend(
                proposed_edges
                    .iter()
                    .map(|edge| family_edge_key(*family, edge)),
            );
            reviewed_family_ungrounded.extend(claims.iter().zip(&grounded).filter_map(
                |(claim, ok)| (!ok).then(|| family_edge_key(*family, &claim_edge_key(claim))),
            ));
        }
        let grounded_claims: Vec<_> = claims
            .iter()
            .zip(&grounded)
            .filter_map(|(claim, valid)| valid.then_some(claim))
            .collect();
        span_tp += max_matches(&grounded_claims, &case.gold, span_eq);
        direction_only += max_matches(&grounded_claims, &case.gold, |claim, item| {
            span_eq(claim, item)
                && claim.cause_variable == item.cause_variable
                && claim.effect_variable == item.effect_variable
        });
        stance += max_matches(&grounded_claims, &case.gold, |claim, item| {
            span_eq(claim, item) && claim.stance == item.stance
        });
        direction += max_matches(&grounded_claims, &case.gold, |claim, item| {
            span_eq(claim, item)
                && claim.cause_variable == item.cause_variable
                && claim.effect_variable == item.effect_variable
                && claim.stance == item.stance
        });
        modality += max_matches(&grounded_claims, &case.gold, |claim, item| {
            span_eq(claim, item) && claim.modality == item.modality
        });
        negation += max_matches(&grounded_claims, &case.gold, |claim, item| {
            span_eq(claim, item)
                && claim.modality == ClaimModality::Negated
                && item.modality == ClaimModality::Negated
        });
        lag += max_matches(&grounded_claims, &case.gold, |claim, item| {
            span_eq(claim, item)
                && claim.lag_min_seconds == item.lag_min_seconds
                && claim.lag_max_seconds == item.lag_max_seconds
        });
        speaker += max_matches(&grounded_claims, &case.gold, |claim, item| {
            span_eq(claim, item) && claim.speaker_id == item.speaker_id
        });
    }
    let lineage_tp = predicted_lineage_claims
        .intersection(&gold_lineage_claims)
        .count();
    let reviewed_true_positive = reviewed_family_predicted
        .intersection(&reviewed_family_gold)
        .count();
    // One semantic claim/edge contributes at most one prediction per family,
    // even when another paraphrase supplies an ungrounded copy. Grounding
    // remains necessary for a true positive, as computed above. Rejected-edge
    // diagnostics count proposed edges regardless of span validity.
    full_family_predicted.extend(full_family_ungrounded);
    reviewed_family_predicted.extend(reviewed_family_ungrounded);
    let rejected_edges_proposed = reviewed_family_predicted
        .intersection(&reviewed_family_rejected)
        .count();
    let digest = serde_json::to_vec(dataset).map_err(|e| e.to_string())?;
    Ok(ExtractionEvalReport {
        dataset_sha256: format!("{:x}", Sha256::digest(digest)),
        cutoff_unix: dataset.cutoff_unix,
        training_cases: dataset
            .cases
            .iter()
            .filter(|case| case.occurred_at <= dataset.cutoff_unix)
            .count(),
        held_out_cases: dataset
            .cases
            .iter()
            .filter(|case| case.occurred_at > dataset.cutoff_unix)
            .count(),
        held_out_families: held_lineages.len(),
        invalid_responses,
        ungrounded_predictions,
        full_claim: EvalCount::new(
            full_family_tp.len(),
            full_family_predicted.len(),
            full_family_gold.len(),
        ),
        exact_span: EvalCount::new(span_tp, predicted, gold),
        direction_claim: EvalCount::new(direction_only, predicted, gold),
        stance_claim: EvalCount::new(stance, predicted, gold),
        modality_claim: EvalCount::new(modality, predicted, gold),
        lag_claim: EvalCount::new(lag, predicted, gold),
        negation_claim: EvalCount::new(negation, predicted_negated, gold_negated),
        independent_lineage_claim: EvalCount::new(
            lineage_tp,
            predicted_lineage_claims.len(),
            gold_lineage_claims.len(),
        ),
        reviewed_cases,
        reviewed_families: family_review_coverage
            .values()
            .filter(|reviewed| **reviewed)
            .count(),
        reviewed_edge_case: EvalCount::new(
            reviewed_true_positive,
            reviewed_family_predicted.len(),
            reviewed_family_gold.len(),
        ),
        rejected_edges_proposed,
        matched_span_direction_correct: direction,
        matched_span_modality_correct: modality,
        matched_span_lag_correct: lag,
        matched_span_speaker_correct: speaker,
        limitations: vec![
            "Dataset labels and response provenance are supplied by the caller; this evaluator does not attest live provider quality.",
            "A correct field requires a grounded exact-span match; claim denominators include unmatched predictions and labels.",
            "Negation precision/recall uses only explicitly negated claims as its positive class.",
            "Full-claim and reviewed-edge counts deduplicate within a held-out source family; exact-span and span-grounded field counts remain per case.",
            "Reviewed-edge agreement requires complete coverage of a held-out family; reviewer identity is not attested and unlabeled families are excluded.",
            "Declared source_family_id is human-supplied. The evaluator also joins repeated lineage IDs and exact text copies, but cannot independently detect unmarked paraphrases or forged provenance.",
            "This report does not evaluate causal-effect intervals.",
        ],
    })
}

fn eval_errors(count: &EvalCount) -> usize {
    count.predicted + count.gold - 2 * count.true_positive
}

/// Evaluate each model on identical cases and labels, then count paired
/// per-family error wins. The response text is supplied offline, never invoked.
pub fn compare_extraction(
    dataset: &ExtractionCompareDataset,
) -> Result<ExtractionCompareReport, String> {
    if dataset.version != "causal-extraction-compare-v2" || dataset.cases.is_empty() {
        return Err(
            "extraction comparison requires nonempty v2 dataset with source_family_id annotations"
                .into(),
        );
    }
    let mut projected: BTreeMap<String, Vec<ExtractionEvalCase>> = BTreeMap::new();
    let mut expected_models: Option<Vec<String>> = None;
    for case in &dataset.cases {
        if case.source_family_id.trim().is_empty()
            || case.source_family_id.trim() != case.source_family_id
        {
            return Err(format!(
                "missing or noncanonical source_family_id annotation in comparison case: {}",
                case.id
            ));
        }
        let mut responses = BTreeMap::new();
        for response in &case.responses {
            if response.model_id.trim().is_empty()
                || responses
                    .insert(response.model_id.clone(), &response.response_json)
                    .is_some()
            {
                return Err(format!("duplicate or empty model ID in case {}", case.id));
            }
        }
        let model_ids: Vec<_> = responses.keys().cloned().collect();
        if model_ids.len() < 2
            || expected_models
                .as_ref()
                .is_some_and(|ids| ids != &model_ids)
        {
            return Err(format!(
                "every case must contain the same two or more model IDs: {}",
                case.id
            ));
        }
        expected_models = Some(model_ids);
        for (model_id, response_json) in responses {
            projected
                .entry(model_id)
                .or_default()
                .push(ExtractionEvalCase {
                    id: case.id.clone(),
                    occurred_at: case.occurred_at,
                    source_lineage_id: case.source_lineage_id.clone(),
                    source_family_id: case.source_family_id.clone(),
                    question: case.question.clone(),
                    source_text: case.source_text.clone(),
                    gold: case.gold.clone(),
                    reviewed_edges: case.reviewed_edges.clone(),
                    response_json: response_json.clone(),
                });
        }
    }
    let first_model_cases = projected.values().next().expect("nonempty model matrix");
    let families = source_family_indexes(first_model_cases);
    let mut held_families = HashSet::new();
    for (case, family) in first_model_cases.iter().zip(families) {
        if case.occurred_at > dataset.cutoff_unix && !held_families.insert(family) {
            return Err("paired comparison requires one held-out case per source family".into());
        }
    }
    let mut models = Vec::new();
    let mut per_case = BTreeMap::new();
    for (model_id, cases) in projected {
        let model_families = source_family_indexes(&cases);
        let model_held_families: HashSet<_> = cases
            .iter()
            .zip(model_families)
            .filter_map(|(case, family)| (case.occurred_at > dataset.cutoff_unix).then_some(family))
            .collect();
        if model_held_families != held_families {
            return Err(format!(
                "paired comparison model has a different held-out source-family set: {model_id}"
            ));
        }
        let report = evaluate_extraction(&ExtractionEvalDataset {
            version: "causal-extraction-eval-v2".into(),
            cutoff_unix: dataset.cutoff_unix,
            cases: cases.clone(),
        })?;
        let train: Vec<_> = cases
            .iter()
            .filter(|case| case.occurred_at <= dataset.cutoff_unix)
            .cloned()
            .collect();
        let mut case_errors = Vec::new();
        for held in cases
            .iter()
            .filter(|case| case.occurred_at > dataset.cutoff_unix)
        {
            let mut split = train.clone();
            split.push(held.clone());
            let single = evaluate_extraction(&ExtractionEvalDataset {
                version: "causal-extraction-eval-v2".into(),
                cutoff_unix: dataset.cutoff_unix,
                cases: split,
            })?;
            case_errors.push((
                eval_errors(&single.full_claim),
                (single.reviewed_cases == 1).then(|| eval_errors(&single.reviewed_edge_case)),
            ));
        }
        per_case.insert(model_id.clone(), case_errors);
        models.push(ModelExtractionReport { model_id, report });
    }
    let mut paired = Vec::new();
    for first in 0..models.len() {
        for second in first + 1..models.len() {
            let first_model = &models[first].model_id;
            let second_model = &models[second].model_id;
            let first_errors = &per_case[first_model];
            let second_errors = &per_case[second_model];
            let mut comparison = PairedExtractionComparison {
                first_model: first_model.clone(),
                second_model: second_model.clone(),
                first_fewer_full_claim_errors: 0,
                second_fewer_full_claim_errors: 0,
                full_claim_ties: 0,
                reviewed_cases: 0,
                first_fewer_reviewed_edge_errors: 0,
                second_fewer_reviewed_edge_errors: 0,
                reviewed_edge_ties: 0,
            };
            for (a, b) in first_errors.iter().zip(second_errors) {
                match a.0.cmp(&b.0) {
                    std::cmp::Ordering::Less => comparison.first_fewer_full_claim_errors += 1,
                    std::cmp::Ordering::Greater => comparison.second_fewer_full_claim_errors += 1,
                    std::cmp::Ordering::Equal => comparison.full_claim_ties += 1,
                }
                if let (Some(a), Some(b)) = (a.1, b.1) {
                    comparison.reviewed_cases += 1;
                    match a.cmp(&b) {
                        std::cmp::Ordering::Less => {
                            comparison.first_fewer_reviewed_edge_errors += 1
                        }
                        std::cmp::Ordering::Greater => {
                            comparison.second_fewer_reviewed_edge_errors += 1
                        }
                        std::cmp::Ordering::Equal => comparison.reviewed_edge_ties += 1,
                    }
                }
            }
            paired.push(comparison);
        }
    }
    let digest = serde_json::to_vec(dataset).map_err(|e| e.to_string())?;
    Ok(ExtractionCompareReport {
        dataset_sha256: format!("{:x}", Sha256::digest(digest)),
        cutoff_unix: dataset.cutoff_unix,
        held_out_cases: dataset
            .cases
            .iter()
            .filter(|case| case.occurred_at > dataset.cutoff_unix)
            .count(),
        held_out_families: held_families.len(),
        models,
        paired,
        limitations: vec![
            "Responses are supplied offline; this command does not call providers or attest model identity.",
            "Paired error wins are descriptive counts, not significance tests or real-model quality claims.",
            "The bundled synthetic fixture validates evaluator mechanics only; supplied external datasets require separate provenance checks.",
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{ClaimModality, EvidenceStance};
    use crate::causal_extract::ExtractionEnvelope;

    fn claim(text: &str) -> ProposedCausalClaim {
        ProposedCausalClaim {
            cause_variable: "arrivals".into(),
            effect_variable: "backlog".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86_400,
            modality: ClaimModality::Asserted,
            stance: EvidenceStance::Supports,
            span_start: 0,
            span_end: text.len(),
            excerpt: text.into(),
            speaker_id: Some("synthetic-rule".into()),
            context: serde_json::json!({}),
        }
    }

    #[test]
    fn held_out_scoring_counts_invalid_and_duplicate_lineages() {
        let text = "Arrivals increase backlog.";
        let training_text = "Ticket volume affects wait time.";
        let mut ungrounded = claim(text);
        ungrounded.excerpt = "invented evidence".into();
        let response_json = serde_json::to_string(&ExtractionEnvelope {
            claims: vec![claim(text), ungrounded],
        })
        .unwrap();
        let dataset = ExtractionEvalDataset {
            version: "causal-extraction-eval-v2".into(),
            cutoff_unix: 100,
            cases: vec![
                ExtractionEvalCase {
                    id: "train".into(),
                    occurred_at: 50,
                    source_lineage_id: "train-thread".into(),
                    source_family_id: "train-thread".into(),
                    question: "Why?".into(),
                    source_text: training_text.into(),
                    gold: vec![claim(training_text)],
                    reviewed_edges: None,
                    response_json: "{\"claims\":[]}".into(),
                },
                ExtractionEvalCase {
                    id: "held-1".into(),
                    occurred_at: 101,
                    source_lineage_id: "held-copy-thread".into(),
                    source_family_id: "held-copy-thread".into(),
                    question: "Why?".into(),
                    source_text: text.into(),
                    gold: vec![claim(text)],
                    reviewed_edges: None,
                    response_json,
                },
                ExtractionEvalCase {
                    id: "held-2".into(),
                    occurred_at: 102,
                    source_lineage_id: "held-thread".into(),
                    source_family_id: "held-thread".into(),
                    question: "Why?".into(),
                    source_text: text.into(),
                    gold: vec![claim(text)],
                    reviewed_edges: None,
                    response_json: "not JSON".into(),
                },
            ],
        };
        let report = evaluate_extraction(&dataset).unwrap();
        assert_eq!(
            (
                report.training_cases,
                report.held_out_cases,
                report.held_out_families
            ),
            (1, 2, 1)
        );
        assert_eq!(
            (report.invalid_responses, report.ungrounded_predictions),
            (1, 1)
        );
        assert_eq!(
            (
                report.full_claim.true_positive,
                report.full_claim.predicted,
                report.full_claim.gold
            ),
            (1, 1, 1)
        );
        assert_eq!(
            (
                report.independent_lineage_claim.true_positive,
                report.independent_lineage_claim.predicted,
                report.independent_lineage_claim.gold
            ),
            (1, 1, 1)
        );
        assert_eq!(report.matched_span_direction_correct, 1);
    }

    #[test]
    fn copied_lineage_cannot_cross_time_split() {
        let text = "Arrivals increase backlog.";
        let make = |id: &str, occurred_at| ExtractionEvalCase {
            id: id.into(),
            occurred_at,
            source_lineage_id: "copied-thread".into(),
            source_family_id: "copied-thread".into(),
            question: "Why?".into(),
            source_text: text.into(),
            gold: vec![claim(text)],
            reviewed_edges: None,
            response_json: "{\"claims\":[]}".into(),
        };
        let dataset = ExtractionEvalDataset {
            version: "causal-extraction-eval-v2".into(),
            cutoff_unix: 100,
            cases: vec![make("train", 99), make("held", 101)],
        };
        assert!(evaluate_extraction(&dataset).is_err());
    }

    #[test]
    fn relabeled_exact_copy_cannot_cross_time_split() {
        let text = "Arrivals increase backlog.";
        let make = |id: &str, occurred_at, lineage: &str| ExtractionEvalCase {
            id: id.into(),
            occurred_at,
            source_lineage_id: lineage.into(),
            source_family_id: lineage.into(),
            question: "Why?".into(),
            source_text: text.into(),
            gold: vec![claim(text)],
            reviewed_edges: None,
            response_json: "{\"claims\":[]}".into(),
        };
        let dataset = ExtractionEvalDataset {
            version: "causal-extraction-eval-v2".into(),
            cutoff_unix: 100,
            cases: vec![make("train", 99, "origin"), make("held", 101, "copy")],
        };
        assert!(evaluate_extraction(&dataset).is_err());
    }

    #[test]
    fn manually_grouped_paraphrase_cannot_cross_time_split() {
        let mut dataset: ExtractionEvalDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-synthetic.json"
        ))
        .unwrap();
        let (training, held_out) = dataset.cases.split_at_mut(1);
        let train = &mut training[0];
        train.source_family_id = "shared-source".into();
        let held = &mut held_out[0];
        held.source_family_id = "shared-source".into();
        assert_ne!(train.source_lineage_id, held.source_lineage_id);
        assert_ne!(train.source_text, held.source_text);
        assert!(
            evaluate_extraction(&dataset)
                .unwrap_err()
                .contains("disjoint declared source families")
        );
    }

    #[test]
    fn v1_or_missing_manual_family_annotation_is_rejected() {
        let mut dataset: ExtractionEvalDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-synthetic.json"
        ))
        .unwrap();
        dataset.version = "causal-extraction-eval-v1".into();
        assert!(
            evaluate_extraction(&dataset)
                .unwrap_err()
                .contains("v2 dataset")
        );
        dataset.version = "causal-extraction-eval-v2".into();
        dataset.cases[0].source_family_id = " ".into();
        assert!(evaluate_extraction(&dataset).is_err());
        let mut value = serde_json::to_value(&dataset).unwrap();
        value["cases"][0]
            .as_object_mut()
            .unwrap()
            .remove("source_family_id");
        let missing: ExtractionEvalDataset = serde_json::from_value(value).unwrap();
        assert!(
            evaluate_extraction(&missing)
                .unwrap_err()
                .contains("source_family_id annotation")
        );
    }

    #[test]
    fn checked_in_synthetic_fixture_scores_without_claiming_real_accuracy() {
        let dataset: ExtractionEvalDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-synthetic.json"
        ))
        .unwrap();
        let report = evaluate_extraction(&dataset).unwrap();
        assert_eq!(
            (
                report.training_cases,
                report.held_out_cases,
                report.held_out_families
            ),
            (1, 4, 3)
        );
        assert_eq!(report.invalid_responses, 1);
        assert_eq!(
            (
                report.full_claim.true_positive,
                report.full_claim.predicted,
                report.full_claim.gold
            ),
            (1, 2, 3)
        );
        assert_eq!(report.exact_span.true_positive, 3);
        assert_eq!(report.matched_span_modality_correct, 2);
        assert_eq!(report.direction_claim.true_positive, 3);
        assert_eq!(report.modality_claim.true_positive, 2);
        assert_eq!(report.lag_claim.true_positive, 3);
        assert_eq!(report.negation_claim.gold, 1);
        assert_eq!(report.negation_claim.predicted, 0);
        assert_eq!(report.negation_claim.recall, Some(0.0));
        assert_eq!(report.reviewed_cases, 4);
        assert_eq!(report.reviewed_families, 3);
        assert_eq!(
            (
                report.reviewed_edge_case.true_positive,
                report.reviewed_edge_case.predicted,
                report.reviewed_edge_case.gold
            ),
            (1, 2, 1)
        );
    }

    #[test]
    fn grounded_and_ungrounded_paraphrases_share_one_family_prediction() {
        let mut dataset: ExtractionEvalDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-synthetic.json"
        ))
        .unwrap();
        let paraphrase = dataset.cases.len() - 1;
        let mut response: ExtractionEnvelope =
            serde_json::from_str(&dataset.cases[paraphrase].response_json).unwrap();
        response.claims[0].excerpt = "fabricated excerpt".into();
        dataset.cases[paraphrase].response_json = serde_json::to_string(&response).unwrap();
        let report = evaluate_extraction(&dataset).unwrap();
        assert_eq!(report.ungrounded_predictions, 1);
        assert_eq!(report.full_claim.true_positive, 1);
        assert_eq!(report.full_claim.predicted, 2);
        assert_eq!(report.full_claim.gold, 3);
        assert_eq!(report.reviewed_edge_case.true_positive, 1);
        assert_eq!(report.reviewed_edge_case.predicted, 2);
        assert_eq!(report.reviewed_edge_case.gold, 1);
        assert_eq!(report.exact_span.true_positive, 2);
        assert_eq!(report.exact_span.predicted, 3);
        assert_eq!(report.exact_span.gold, 4);
    }

    #[test]
    fn rejected_ungrounded_edge_still_counts_as_proposed() {
        let mut dataset: ExtractionEvalDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-synthetic.json"
        ))
        .unwrap();
        let mut rejected = dataset.cases[2].gold[0].clone();
        rejected.excerpt = "fabricated excerpt".into();
        dataset.cases[2].response_json = serde_json::to_string(&ExtractionEnvelope {
            claims: vec![rejected],
        })
        .unwrap();
        let report = evaluate_extraction(&dataset).unwrap();
        assert_eq!(report.ungrounded_predictions, 1);
        assert_eq!(report.reviewed_edge_case.true_positive, 1);
        assert_eq!(report.reviewed_edge_case.predicted, 2);
        assert_eq!(report.reviewed_edge_case.gold, 1);
        assert_eq!(report.rejected_edges_proposed, 1);
    }

    #[test]
    fn reviewed_family_requires_complete_consistent_labels() {
        let mut dataset: ExtractionEvalDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-synthetic.json"
        ))
        .unwrap();
        let paraphrase = dataset.cases.len() - 1;
        dataset.cases[paraphrase].reviewed_edges = None;
        assert!(
            evaluate_extraction(&dataset)
                .unwrap_err()
                .contains("cover every held-out case")
        );
        let original_review = dataset.cases[1].reviewed_edges.clone();
        dataset.cases[paraphrase].reviewed_edges = original_review;
        dataset.cases[paraphrase].reviewed_edges.as_mut().unwrap()[0].accepted = false;
        assert!(
            evaluate_extraction(&dataset)
                .unwrap_err()
                .contains("conflicting reviewed-edge decisions")
        );
    }

    #[test]
    fn field_metrics_penalize_reversed_direction_and_false_negation() {
        let mut dataset: ExtractionEvalDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-synthetic.json"
        ))
        .unwrap();
        let mut wrong = dataset.cases[1].gold[0].clone();
        std::mem::swap(&mut wrong.cause_variable, &mut wrong.effect_variable);
        wrong.modality = ClaimModality::Negated;
        dataset.cases[1].response_json = serde_json::to_string(&ExtractionEnvelope {
            claims: vec![wrong],
        })
        .unwrap();
        let report = evaluate_extraction(&dataset).unwrap();
        assert_eq!(report.exact_span.true_positive, 3);
        assert_eq!(report.direction_claim.true_positive, 2);
        assert_eq!(report.direction_claim.predicted, 3);
        assert_eq!(report.direction_claim.gold, 4);
        assert_eq!(report.negation_claim.predicted, 1);
        assert_eq!(report.negation_claim.gold, 1);
        assert_eq!(report.negation_claim.true_positive, 0);
        assert_eq!(report.negation_claim.precision, Some(0.0));
        assert_eq!(report.negation_claim.recall, Some(0.0));
    }

    #[test]
    fn shared_span_field_score_uses_maximum_matching_in_any_response_order() {
        let text = "Two possible drivers of backlog.";
        let first = claim(text);
        let mut second = claim(text);
        second.cause_variable = "staffing".into();
        let mut predicted_first = first.clone();
        predicted_first.modality = ClaimModality::Speculated;
        let mut predicted_second = second.clone();
        predicted_second.modality = ClaimModality::Speculated;
        let mut dataset = ExtractionEvalDataset {
            version: "causal-extraction-eval-v2".into(),
            cutoff_unix: 100,
            cases: vec![
                ExtractionEvalCase {
                    id: "train".into(),
                    occurred_at: 99,
                    source_lineage_id: "train".into(),
                    source_family_id: "train".into(),
                    question: "Why?".into(),
                    source_text: "Training control text.".into(),
                    gold: vec![],
                    reviewed_edges: None,
                    response_json: "{\"claims\":[]}".into(),
                },
                ExtractionEvalCase {
                    id: "held".into(),
                    occurred_at: 101,
                    source_lineage_id: "held".into(),
                    source_family_id: "held".into(),
                    question: "Why?".into(),
                    source_text: text.into(),
                    gold: vec![first, second],
                    reviewed_edges: None,
                    response_json: String::new(),
                },
            ],
        };
        for predictions in [
            vec![predicted_second.clone(), predicted_first.clone()],
            vec![predicted_first.clone(), predicted_second.clone()],
        ] {
            dataset.cases[1].response_json = serde_json::to_string(&ExtractionEnvelope {
                claims: predictions,
            })
            .unwrap();
            let report = evaluate_extraction(&dataset).unwrap();
            assert_eq!(report.full_claim.true_positive, 0);
            assert_eq!(report.exact_span.true_positive, 2);
            assert_eq!(report.direction_claim.true_positive, 2);
            assert_eq!(report.matched_span_direction_correct, 2);
        }
    }

    #[test]
    fn rejected_reviewed_edge_is_a_false_positive_and_incomplete_review_fails() {
        let text = "Arrivals increase backlog.";
        let reviewed = ReviewedEdge {
            cause_variable: "arrivals".into(),
            effect_variable: "backlog".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86_400,
            modality: ClaimModality::Asserted,
            accepted: false,
        };
        let response_json = serde_json::to_string(&ExtractionEnvelope {
            claims: vec![claim(text)],
        })
        .unwrap();
        let mut dataset = ExtractionEvalDataset {
            version: "causal-extraction-eval-v2".into(),
            cutoff_unix: 100,
            cases: vec![
                ExtractionEvalCase {
                    id: "train".into(),
                    occurred_at: 99,
                    source_lineage_id: "train".into(),
                    source_family_id: "train".into(),
                    question: "Why?".into(),
                    source_text: "Training control text.".into(),
                    gold: vec![],
                    reviewed_edges: None,
                    response_json: "{\"claims\":[]}".into(),
                },
                ExtractionEvalCase {
                    id: "held".into(),
                    occurred_at: 101,
                    source_lineage_id: "held".into(),
                    source_family_id: "held".into(),
                    question: "Why?".into(),
                    source_text: text.into(),
                    gold: vec![claim(text)],
                    reviewed_edges: Some(vec![reviewed]),
                    response_json,
                },
            ],
        };
        let report = evaluate_extraction(&dataset).unwrap();
        assert_eq!(report.full_claim.true_positive, 1);
        assert_eq!(report.reviewed_edge_case.true_positive, 0);
        assert_eq!(report.reviewed_edge_case.predicted, 1);
        assert_eq!(report.rejected_edges_proposed, 1);

        dataset.cases[1].reviewed_edges = Some(vec![]);
        assert!(evaluate_extraction(&dataset).is_err());
    }

    #[test]
    fn paired_synthetic_comparison_exposes_claim_and_review_tradeoff() {
        let dataset: ExtractionCompareDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-compare-synthetic.json"
        ))
        .unwrap();
        let report = compare_extraction(&dataset).unwrap();
        assert_eq!(report.held_out_cases, 3);
        assert_eq!(report.held_out_families, 3);
        assert_eq!(report.models.len(), 2);
        assert_eq!(report.models[0].report.full_claim.true_positive, 1);
        assert_eq!(report.models[1].report.full_claim.true_positive, 3);
        assert_eq!(report.models[0].report.reviewed_edge_case.predicted, 2);
        assert_eq!(report.models[1].report.reviewed_edge_case.predicted, 3);
        assert_eq!(report.models[1].report.rejected_edges_proposed, 2);
        let paired = &report.paired[0];
        assert_eq!(paired.second_fewer_full_claim_errors, 2);
        assert_eq!(paired.first_fewer_reviewed_edge_errors, 1);
        assert_eq!(paired.reviewed_edge_ties, 2);
    }

    #[test]
    fn comparison_rejects_incomplete_model_matrix() {
        let mut dataset: ExtractionCompareDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-compare-synthetic.json"
        ))
        .unwrap();
        dataset.cases[1].responses.pop();
        assert!(compare_extraction(&dataset).is_err());
    }

    #[test]
    fn paired_comparison_rejects_v1_contract() {
        let mut dataset: ExtractionCompareDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-compare-synthetic.json"
        ))
        .unwrap();
        dataset.version = "causal-extraction-compare-v1".into();
        assert!(
            compare_extraction(&dataset)
                .unwrap_err()
                .contains("v2 dataset")
        );
    }

    #[test]
    fn paired_comparison_rejects_relabelled_held_out_copy() {
        let mut dataset: ExtractionCompareDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-compare-synthetic.json"
        ))
        .unwrap();
        let mut copy = dataset.cases[1].clone();
        copy.id = "relabeled-held-copy".into();
        copy.source_lineage_id = "different-declared-lineage".into();
        dataset.cases.push(copy);
        assert!(compare_extraction(&dataset).is_err());
    }

    #[test]
    fn paired_comparison_rejects_manually_grouped_paraphrase() {
        let mut dataset: ExtractionCompareDataset = serde_json::from_str(include_str!(
            "../../../fixtures/causal-extraction-compare-synthetic.json"
        ))
        .unwrap();
        let mut paraphrase = dataset.cases[1].clone();
        paraphrase.id = "arrival-paraphrase".into();
        paraphrase.occurred_at += 10;
        paraphrase.source_lineage_id = "different-lineage".into();
        paraphrase.source_text = "A higher arrival rate expands tomorrow's queue.".into();
        paraphrase.gold.clear();
        paraphrase.reviewed_edges = Some(vec![]);
        paraphrase
            .responses
            .iter_mut()
            .for_each(|response| response.response_json = "{\"claims\":[]}".into());
        dataset.cases.push(paraphrase);
        assert!(
            compare_extraction(&dataset)
                .unwrap_err()
                .contains("one held-out case per source family")
        );
    }

    #[test]
    fn exact_match_wins_over_earlier_wrong_modality_at_same_span() {
        let text = "Arrivals increase backlog.";
        let mut wrong = claim(text);
        wrong.modality = ClaimModality::Speculated;
        let response_json = serde_json::to_string(&ExtractionEnvelope {
            claims: vec![wrong, claim(text)],
        })
        .unwrap();
        let dataset = ExtractionEvalDataset {
            version: "causal-extraction-eval-v2".into(),
            cutoff_unix: 100,
            cases: vec![
                ExtractionEvalCase {
                    id: "train".into(),
                    occurred_at: 99,
                    source_lineage_id: "train".into(),
                    source_family_id: "train".into(),
                    question: "Why?".into(),
                    source_text: "Training control text.".into(),
                    gold: vec![],
                    reviewed_edges: None,
                    response_json: "{\"claims\":[]}".into(),
                },
                ExtractionEvalCase {
                    id: "held".into(),
                    occurred_at: 101,
                    source_lineage_id: "held".into(),
                    source_family_id: "held".into(),
                    question: "Why?".into(),
                    source_text: text.into(),
                    gold: vec![claim(text)],
                    reviewed_edges: None,
                    response_json,
                },
            ],
        };
        let report = evaluate_extraction(&dataset).unwrap();
        assert_eq!(report.full_claim.true_positive, 1);
        assert_eq!(report.exact_span.true_positive, 1);
    }
}
