//! Bias aggregation — turn retrieved experiences into a clamped logit-bias map.
//!
//! Retrieval + aggregation follows the JitRL recipe (arXiv:2601.18510): rank
//! stored experiences by Jaccard similarity to the current prompt, then form
//! a per-token additive logit adjustment from the similarity-weighted,
//! age-decayed outcome weights. The result is clamped to `±max_bias` and
//! capped in size so it can be injected either as an OpenAI-compat
//! `logit_bias` map (Tier B) or applied per decode step (Tier A seam,
//! [`StepBiasAdjuster`]).

use std::collections::HashMap;

use super::JitrlConfig;
use super::fingerprint::jaccard;
use super::store::{ExperienceRecord, decayed_weight};

/// Biases smaller than this (absolute) are dropped — they cannot meaningfully
/// move a softmax and only bloat the request body.
const MIN_EFFECTIVE_BIAS: f32 = 1e-3;

/// Aggregate retrieved experiences into a token → bias map.
///
/// Steps: score every record by Jaccard similarity against `query_sketch`,
/// keep those `>= min_similarity`, take the `top_k` most similar, then for
/// each token sum `similarity × decayed_weight × max_bias` and clamp the
/// total to `[-max_bias, +max_bias]`. Returns `None` when nothing survives —
/// callers must then leave the request untouched.
pub fn aggregate_bias(
    query_sketch: &[u64],
    records: &[ExperienceRecord],
    now_epoch: i64,
    cfg: &JitrlConfig,
) -> Option<HashMap<u32, f32>> {
    if query_sketch.is_empty() || records.is_empty() {
        return None;
    }

    let mut scored: Vec<(f32, &ExperienceRecord)> = records
        .iter()
        .map(|r| (jaccard(query_sketch, &r.sketch), r))
        .filter(|(sim, _)| *sim >= cfg.min_similarity && *sim > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(cfg.top_k.max(1));

    if scored.is_empty() {
        return None;
    }

    let mut bias: HashMap<u32, f32> = HashMap::new();
    for (sim, rec) in &scored {
        let age = now_epoch - rec.created_at;
        for (&token_id, &weight) in &rec.token_weights {
            let contribution = sim * decayed_weight(weight, age, cfg.half_life_days) * cfg.max_bias;
            *bias.entry(token_id).or_insert(0.0) += contribution;
        }
    }

    // Clamp, drop negligible entries.
    bias.retain(|_, v| {
        *v = v.clamp(-cfg.max_bias, cfg.max_bias);
        v.abs() >= MIN_EFFECTIVE_BIAS && v.is_finite()
    });

    // Cap map size: keep the strongest |bias| entries (OpenAI-style
    // `logit_bias` maps are bounded; llama.cpp server also prefers small maps).
    if bias.len() > cfg.max_bias_tokens {
        let mut entries: Vec<(u32, f32)> = bias.into_iter().collect();
        entries.sort_by(|a, b| {
            b.1.abs()
                .partial_cmp(&a.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(cfg.max_bias_tokens);
        bias = entries.into_iter().collect();
    }

    if bias.is_empty() { None } else { Some(bias) }
}

/// Tier A seam: per-decode-step logit adjustment.
///
/// This is the surface a future in-process sampling loop (llama.cpp via
/// `llama-cpp-2`, which exposes raw logits each decode step) plugs into.
/// Today `LlamaCppBackend::generate` is a stub, so no live path calls this in
/// production — it is fully tested and ready for when the sampling loop lands.
pub struct StepBiasAdjuster {
    bias: HashMap<u32, f32>,
}

impl StepBiasAdjuster {
    pub fn new(bias: HashMap<u32, f32>) -> Self {
        Self { bias }
    }

    /// Number of biased tokens.
    pub fn len(&self) -> usize {
        self.bias.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bias.is_empty()
    }

    /// Apply the additive bias to a raw logits slice (index == token id).
    /// Token ids outside the vocabulary slice are ignored — a bias map built
    /// against a different checkpoint must never panic the sampler.
    pub fn adjust_logits(&self, logits: &mut [f32]) {
        for (&token_id, &b) in &self.bias {
            if let Some(l) = logits.get_mut(token_id as usize) {
                *l += b;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jitrl::fingerprint::shingle_sketch;

    fn cfg() -> JitrlConfig {
        JitrlConfig {
            enabled: true,
            max_bias: 2.0,
            top_k: 8,
            min_similarity: 0.3,
            half_life_days: 14.0,
            max_bias_tokens: 128,
            max_records: 2000,
            store_path: None,
        }
    }

    fn rec(prompt: &str, tokens: &[(u32, f32)], created_at: i64) -> ExperienceRecord {
        ExperienceRecord {
            id: "t".into(),
            model_id: "m".into(),
            sketch: shingle_sketch(prompt),
            token_weights: tokens.iter().copied().collect(),
            reward: 1.0,
            created_at,
        }
    }

    #[test]
    fn aggregation_clamps_to_max_bias() {
        let now = 1_700_000_000;
        let prompt = "please summarize this quarterly report";
        // Many identical fresh records stack way past max_bias without a clamp.
        let records: Vec<_> = (0..10).map(|_| rec(prompt, &[(42, 1.0)], now)).collect();
        let bias = aggregate_bias(&shingle_sketch(prompt), &records, now, &cfg()).unwrap();
        let b = bias[&42];
        assert!(b <= 2.0 + f32::EPSILON, "must clamp, got {b}");
        assert!(
            b >= 1.9,
            "stacked fresh identical hits should saturate, got {b}"
        );
        // Negative weights clamp on the other side.
        let records: Vec<_> = (0..10).map(|_| rec(prompt, &[(7, -1.0)], now)).collect();
        let bias = aggregate_bias(&shingle_sketch(prompt), &records, now, &cfg()).unwrap();
        assert!(bias[&7] >= -2.0 - f32::EPSILON);
    }

    #[test]
    fn dissimilar_records_are_filtered_out() {
        let now = 1_700_000_000;
        let records = vec![rec(
            "write a rust function that reverses a linked list",
            &[(42, 1.0)],
            now,
        )];
        let query = shingle_sketch("please summarize this quarterly report");
        assert!(aggregate_bias(&query, &records, now, &cfg()).is_none());
    }

    #[test]
    fn old_experiences_decay() {
        let prompt = "please summarize this quarterly report";
        let now = 1_700_000_000;
        let fresh = vec![rec(prompt, &[(1, 1.0)], now)];
        let old = vec![rec(prompt, &[(1, 1.0)], now - 28 * 86_400)]; // 2 half-lives
        let c = cfg();
        let q = shingle_sketch(prompt);
        let fresh_b = aggregate_bias(&q, &fresh, now, &c).unwrap()[&1];
        let old_b = aggregate_bias(&q, &old, now, &c).unwrap()[&1];
        assert!(
            (old_b - fresh_b / 4.0).abs() < 0.05,
            "fresh {fresh_b} old {old_b}"
        );
    }

    #[test]
    fn empty_inputs_yield_none() {
        let c = cfg();
        assert!(aggregate_bias(&[], &[], 0, &c).is_none());
        let q = shingle_sketch("hello world friend");
        assert!(aggregate_bias(&q, &[], 0, &c).is_none());
    }

    #[test]
    fn map_size_is_capped_by_strongest_bias() {
        let now = 1_700_000_000;
        let prompt = "please summarize this quarterly report";
        let mut c = cfg();
        c.max_bias_tokens = 2;
        // One record with 4 tokens of varying weight.
        let records = vec![rec(prompt, &[(1, 1.0), (2, 0.8), (3, 0.2), (4, 0.1)], now)];
        let bias = aggregate_bias(&shingle_sketch(prompt), &records, now, &c).unwrap();
        assert_eq!(bias.len(), 2);
        assert!(bias.contains_key(&1) && bias.contains_key(&2));
    }

    #[test]
    fn step_adjuster_applies_and_ignores_out_of_vocab() {
        let mut bias = HashMap::new();
        bias.insert(1u32, 1.5f32);
        bias.insert(3u32, -0.5f32);
        bias.insert(999u32, 2.0f32); // out of vocab — must not panic
        let adj = StepBiasAdjuster::new(bias);
        let mut logits = vec![0.0f32, 0.0, 0.0, 1.0];
        adj.adjust_logits(&mut logits);
        assert_eq!(logits, vec![0.0, 1.5, 0.0, 0.5]);
        assert_eq!(adj.len(), 3);
        assert!(!adj.is_empty());
    }
}
