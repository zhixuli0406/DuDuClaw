//! WP-P3 — held-out rule gate (pure functions).
//!
//! See `commercial/docs/DESIGN-lwm-calibration-2026-08-10.md` §1.4, §4
//! (platform section), §6 (the seven anti-overfitting criteria), §9.
//!
//! ## The one rule this module enforces
//!
//! **Reflection may only PROPOSE a candidate rule; it may never DECIDE that
//! the rule is adopted.** The literature is unambiguous that a self-evolving
//! loop which lets a model grade its own lessons collapses:
//! - self-preference / self-recognition bias — a judge favors its own
//!   generations (arXiv:2404.13076);
//! - sycophancy — persuasiveness is not correctness (arXiv:2310.13548);
//! - intrinsic self-correction without an external signal is ineffective or
//!   actively harmful (arXiv:2310.01798);
//! - reflection distils *locally correct, non-transferable* experience into
//!   over-general rules and **bypasses an LLM auditor** — "ask another LLM to
//!   review the rule" does not catch it (OEP arXiv:2605.18930).
//!
//! So adoption authority is handed to a gate that is **later in time**, scored
//! by an **external numeric oracle**, and **multiple-testing corrected**:
//! - RSEA (arXiv:2606.28374): online evolution without a held-out gate
//!   collapses (Dynamic Cheatsheet 70.7% → 0.14% on a domain switch); a
//!   keep-better hard gate is what makes it monotone-safe.
//! - ExpeL (arXiv:2308.10144): the data used to *extract* a rule and the data
//!   used to *validate* it must be different batches.
//! - AURA / surprise-gated memory (arXiv:2606.03787): only a genuine
//!   prediction error earns a write.
//!
//! At N≈30 the honest output is very often "we still don't know" — a candidate
//! that has not yet accumulated enough out-of-sample evidence stays a shadow
//! candidate ([`GateDecision::Shadow`]); it is **not** promoted just because
//! reflection is confident about it.
//!
//! ## Hard boundary (do not violate)
//!
//! [`evaluate_rule_gate`] consumes **only a numeric hit/miss record**
//! ([`HeldOutStats`]). An LLM's self-assessment score MUST NEVER be passed
//! into this function — it is telemetry, never a gate input (design §1.4:
//! "LLM 自評分數只當 telemetry,永不當 gate"). There is deliberately no field
//! on [`HeldOutStats`] that could carry one.
//!
//! Everything here is **pure**: zero LLM, zero I/O. The proper-scoring math it
//! builds on lives in [`super::calibration`] (`wilson_bounds`, `bonferroni_z`).

use serde::{Deserialize, Serialize};

use super::calibration::{bonferroni_z, wilson_bounds};

// ─────────────────────────────────────────────────────────────────────────
// Tunable constants
// ─────────────────────────────────────────────────────────────────────────

/// Base two-sided critical value before any multiple-testing correction
/// (95%). Passed to [`bonferroni_z`], which returns a stricter (larger) `z`
/// as the number of concurrently-trialed candidates grows.
pub const DEFAULT_BASE_Z: f64 = 1.96;

/// Minimum out-of-sample observations before the gate will promote or retire a
/// candidate. Below this it stays [`GateDecision::Shadow`] — "not enough data
/// to say anything" is the honest verdict at small N (design §1.4: a 18/30
/// Wilson 95% CI still contains 0.5). Deliberately equal to [`ROLL_WINDOW`] so
/// that the moment a candidate is decidable, its rolling window is already
/// full and the keep-better check below is meaningful rather than reacting to a
/// one-sample blip.
pub const MIN_HELD_OUT_SAMPLES: u64 = 8;

/// Size of the recent rolling window used for keep-better demotion (RSEA
/// arXiv:2606.28374). A promoted rule is watched here so a regime shift that
/// makes it stop working demotes it quickly, without waiting for its large
/// cumulative record to average the regression away.
pub const ROLL_WINDOW: usize = 8;

/// Default frozen baseline hit rate a candidate must beat out-of-sample: the
/// coin-flip / "no rule at all" prior. Domain-agnostic and **frozen** on
/// purpose (design §1.3: a drifting baseline makes the skill claim
/// unfalsifiable). Callers that have a real measured no-rule success rate for
/// the state should thread it in instead of this default.
pub const DEFAULT_BASELINE_HIT_RATE: f64 = 0.5;

// ─────────────────────────────────────────────────────────────────────────
// Lesson classification (domain-agnostic process/outcome mapping)
// ─────────────────────────────────────────────────────────────────────────

/// How a proposed lesson is graded, derived purely from whether it carries
/// programmatic (trajectory) evidence.
///
/// This is the domain-agnostic encoding of the design §6 process-vs-outcome
/// split — **no trading/market vocabulary**:
/// - **[`LessonKind::Verified`]** — the lesson is grounded in a deterministic
///   oracle (a tool error code, a failed assertion, a structured verdict).
///   There is a real signal behind it, so it may be re-injected on the normal
///   path.
/// - **[`LessonKind::Inductive`]** — the lesson has no programmatic evidence;
///   it is an induced generalization over a small, noisy sample. At N≈30 such
///   a rule cannot be distinguished from noise, so it must be born as a shadow
///   candidate and earn adoption out-of-sample through the held-out gate
///   (design §6 item 1: "無法歸類 → 預設 outcome (fail-closed)" — no evidence
///   is treated as the noisy, shadow-only case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LessonKind {
    Verified,
    Inductive,
}

/// Classify a proposed lesson from the single fact of whether it is backed by
/// programmatic trajectory evidence.
///
/// `has_trajectory_evidence == true` ⇒ [`LessonKind::Verified`] (an oracle
/// exists); `false` ⇒ [`LessonKind::Inductive`] (shadow-only, must go through
/// the held-out gate). Fail-closed by construction: absence of evidence never
/// yields the trusted class.
pub fn classify_lesson(has_trajectory_evidence: bool) -> LessonKind {
    if has_trajectory_evidence {
        LessonKind::Verified
    } else {
        LessonKind::Inductive
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Held-out record + gate decision
// ─────────────────────────────────────────────────────────────────────────

/// Out-of-sample hit/miss record for one candidate rule.
///
/// Prequential time split (design §6 item 2): a candidate is scored **only on
/// observations that arrive strictly after it was born** — the birth batch
/// never counts toward its own validation (that would be train==test).
/// [`born_seq`](Self::born_seq) records the birth cursor so a caller can drop
/// any same-instant observation; the counters below only ever accumulate
/// genuinely out-of-sample outcomes.
///
/// **No LLM score is stored here** — see the module-level hard boundary. The
/// only inputs are deterministic numeric hits/misses.
///
/// `#[serde(default)]` (both on the struct and every field) so a rule whose
/// metadata predates this field deserializes cleanly as an all-zero record
/// rather than failing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeldOutStats {
    /// Out-of-sample outcomes graded a hit (Negligible/Moderate settle).
    pub oos_hits: u64,
    /// Out-of-sample outcomes graded a miss (Significant/Critical settle).
    pub oos_misses: u64,
    /// Birth cursor (e.g. unix seconds) — observations at/below this are the
    /// birth batch and must not be counted (see struct docs).
    pub born_seq: u64,
    /// Last [`ROLL_WINDOW`] outcomes (`true` = hit), newest at the back. Used
    /// only for keep-better demotion; bounded so it never grows the metadata
    /// blob.
    pub recent: Vec<bool>,
}

impl HeldOutStats {
    /// A fresh record for a candidate born at `seq`.
    pub fn born(seq: u64) -> Self {
        Self {
            born_seq: seq,
            ..Default::default()
        }
    }

    /// Total out-of-sample observations recorded.
    pub fn total(&self) -> u64 {
        self.oos_hits.saturating_add(self.oos_misses)
    }

    /// Record one out-of-sample outcome (deterministic; no LLM input). Bumps
    /// the cumulative counter and appends to the bounded rolling window.
    pub fn record(&mut self, hit: bool) {
        if hit {
            self.oos_hits = self.oos_hits.saturating_add(1);
        } else {
            self.oos_misses = self.oos_misses.saturating_add(1);
        }
        self.recent.push(hit);
        // Keep only the most recent ROLL_WINDOW outcomes.
        if self.recent.len() > ROLL_WINDOW {
            let overflow = self.recent.len() - ROLL_WINDOW;
            self.recent.drain(0..overflow);
        }
    }

    /// Metadata JSON key holding this record. Sibling to `rule_stats` in the
    /// same blob (see [`from_metadata`](Self::from_metadata)).
    pub const METADATA_KEY: &'static str = "held_out_stats";

    /// Parse from an entry's metadata blob; missing/malformed → all-zero
    /// record (never a hard error — same posture as
    /// `rule_lifecycle::RuleStats::from_metadata`).
    ///
    /// Kept as a sibling metadata key rather than a field embedded inside
    /// `RuleStats` deliberately: `RuleStats` is a `Copy` value type consumed
    /// via struct literals across the codebase; adding a `Vec`-bearing field
    /// would break `Copy` and force a migration of every literal. Co-locating
    /// this record in the same metadata object achieves the same "the rule
    /// carries its held-out counters, backward-compatibly" contract without
    /// that churn.
    pub fn from_metadata(metadata: &serde_json::Value) -> Self {
        metadata
            .get(Self::METADATA_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Write this record back into a metadata object, preserving sibling keys
    /// (`rule_stats`, `source_mistake_ids`, playbook meta, …).
    pub fn merge_into(&self, metadata: &mut serde_json::Value) {
        if !metadata.is_object() {
            *metadata = serde_json::json!({});
        }
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert(
                Self::METADATA_KEY.to_string(),
                serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({})),
            );
        }
    }
}

/// What the held-out gate says should happen to a candidate, given only its
/// numeric out-of-sample record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Out-of-sample lower bound beats the baseline and the recent window is
    /// holding — the rule may be injected.
    Inject,
    /// Not yet decidable (insufficient samples) or beaten-but-not-hopeless —
    /// keep it as a shadow candidate, do not inject, keep scoring it.
    Shadow,
    /// A previously-good rule whose recent rolling window fell below baseline
    /// (keep-better / RSEA): stop injecting it, but **keep the row and its
    /// history** — it may recover. Never a deletion.
    Demote,
    /// Even the optimistic upper bound cannot beat the baseline — the
    /// candidate is spent and should be retired.
    Retire,
}

/// Decide a candidate's fate from its out-of-sample record alone.
///
/// Algorithm (all bounded proper-scoring math, design §6 items 4–5):
/// 1. `n < `[`MIN_HELD_OUT_SAMPLES`] → [`GateDecision::Shadow`] (honest
///    "don't know yet").
/// 2. Compute the Wilson interval on the cumulative hit rate at a
///    **Bonferroni-corrected** critical value ([`bonferroni_z`] over
///    `k_concurrent_candidates`) — trialing more candidates at once makes
///    promotion strictly harder, controlling the family-wise error rate.
/// 3. Optimistic upper bound `≤ baseline` → [`GateDecision::Retire`] (spent).
/// 4. Conservative lower bound `> baseline` (promotable):
///    - recent rolling window regressed below baseline → [`GateDecision::Demote`]
///      (keep-better);
///    - otherwise → [`GateDecision::Inject`].
/// 5. Otherwise (lower `≤ baseline < ` upper) → [`GateDecision::Shadow`]
///    (undetermined; keep evaluating).
///
/// `k_concurrent_candidates` is clamped to `>= 1` before correction. This
/// function never sees, and cannot be influenced by, any LLM self-assessment
/// (module hard boundary).
pub fn evaluate_rule_gate(
    stats: &HeldOutStats,
    k_concurrent_candidates: usize,
    baseline_hit_rate: f64,
) -> GateDecision {
    let n = stats.total();
    if n < MIN_HELD_OUT_SAMPLES {
        return GateDecision::Shadow;
    }

    let z = bonferroni_z(DEFAULT_BASE_Z, k_concurrent_candidates.max(1));
    let (lo, hi) = wilson_bounds(stats.oos_hits, n, z);
    // A NaN bound (shouldn't happen for n >= MIN) is treated as "not yet
    // decidable" rather than promoting on garbage.
    if lo.is_nan() || hi.is_nan() {
        return GateDecision::Shadow;
    }

    if hi <= baseline_hit_rate {
        return GateDecision::Retire;
    }

    if lo > baseline_hit_rate {
        if recent_window_regressed(stats, baseline_hit_rate) {
            return GateDecision::Demote;
        }
        return GateDecision::Inject;
    }

    GateDecision::Shadow
}

/// Keep-better demotion trigger: the recent rolling window (once full) has a
/// Wilson lower bound at/below the baseline.
///
/// Uses the **uncorrected** [`DEFAULT_BASE_Z`] (not the Bonferroni-corrected
/// one used for promotion): demotion is deliberately asymmetric — hard to
/// promote, easy to demote — so a regression is caught fast (RSEA
/// monotone-safety). Requires a full [`ROLL_WINDOW`] so a half-empty window
/// never triggers a spurious demotion.
fn recent_window_regressed(stats: &HeldOutStats, baseline_hit_rate: f64) -> bool {
    if stats.recent.len() < ROLL_WINDOW {
        return false;
    }
    let window = &stats.recent[stats.recent.len() - ROLL_WINDOW..];
    let hits = window.iter().filter(|&&b| b).count() as u64;
    let (lo, _) = wilson_bounds(hits, ROLL_WINDOW as u64, DEFAULT_BASE_Z);
    !lo.is_nan() && lo <= baseline_hit_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_lesson ──

    #[test]
    fn classify_maps_evidence_to_verified_and_absence_to_inductive() {
        assert_eq!(classify_lesson(true), LessonKind::Verified);
        assert_eq!(classify_lesson(false), LessonKind::Inductive);
    }

    // ── HeldOutStats bookkeeping ──

    #[test]
    fn record_bumps_counters_and_bounds_recent_window() {
        let mut s = HeldOutStats::born(100);
        assert_eq!(s.born_seq, 100);
        for _ in 0..12 {
            s.record(true);
        }
        assert_eq!(s.oos_hits, 12);
        assert_eq!(s.oos_misses, 0);
        assert_eq!(s.total(), 12);
        // Rolling window never exceeds ROLL_WINDOW.
        assert_eq!(s.recent.len(), ROLL_WINDOW);
    }

    #[test]
    fn from_metadata_missing_key_is_zeroed_and_merge_preserves_siblings() {
        // Missing key → default (backward compatible).
        let empty = serde_json::json!({ "rule_stats": { "helpful": 3, "harmful": 1 } });
        assert_eq!(HeldOutStats::from_metadata(&empty), HeldOutStats::default());

        // merge_into keeps siblings intact.
        let mut blob = empty.clone();
        let mut s = HeldOutStats::born(7);
        s.record(true);
        s.record(false);
        s.merge_into(&mut blob);
        assert_eq!(blob["rule_stats"]["helpful"], 3);
        let round = HeldOutStats::from_metadata(&blob);
        assert_eq!(round, s);
        assert_eq!(round.oos_hits, 1);
        assert_eq!(round.oos_misses, 1);
        assert_eq!(round.born_seq, 7);
    }

    #[test]
    fn serde_defaults_missing_fields() {
        // An old metadata shape that only had oos_hits/oos_misses (no born_seq
        // / recent) must still deserialize.
        let old = serde_json::json!({ "held_out_stats": { "oos_hits": 4, "oos_misses": 2 } });
        let s = HeldOutStats::from_metadata(&old);
        assert_eq!(s.oos_hits, 4);
        assert_eq!(s.oos_misses, 2);
        assert_eq!(s.born_seq, 0);
        assert!(s.recent.is_empty());
    }

    // ── evaluate_rule_gate ──

    fn stats(hits: u64, misses: u64, recent: Vec<bool>) -> HeldOutStats {
        HeldOutStats {
            oos_hits: hits,
            oos_misses: misses,
            born_seq: 0,
            recent,
        }
    }

    #[test]
    fn insufficient_samples_stays_shadow() {
        let s = stats(3, 1, vec![true, true, true, false]);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Shadow);
    }

    #[test]
    fn strong_out_of_sample_record_injects() {
        // 9/10, recent all hits → Wilson lower bound > 0.5 at k=1.
        let s = stats(9, 1, vec![true; 8]);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Inject);
    }

    #[test]
    fn bonferroni_makes_promotion_harder_as_k_grows() {
        // Same 9/10 record: promotable at k=1, but a large concurrent trial
        // family widens the corrected z until the lower bound no longer clears
        // the baseline — so it falls back to Shadow, never Inject.
        let s = stats(9, 1, vec![true; 8]);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Inject);
        assert_eq!(
            evaluate_rule_gate(&s, 20, 0.5),
            GateDecision::Shadow,
            "K=20 Bonferroni correction must block a promotion that passed at K=1"
        );
    }

    #[test]
    fn keep_better_demotes_when_recent_window_regresses() {
        // Strong cumulative record (22 obs, lower bound > 0.5) but the recent
        // 8-window collapsed to 3/8 → demote, don't keep injecting.
        let recent = vec![true, true, true, false, false, false, false, false];
        let s = stats(20, 2, recent);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Demote);
    }

    #[test]
    fn strong_cumulative_and_healthy_window_still_injects() {
        // Same cumulative strength, but a healthy recent window → Inject.
        let s = stats(20, 2, vec![true; 8]);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Inject);
    }

    #[test]
    fn spent_candidate_retires() {
        // 1/16: even the optimistic upper bound is below baseline → Retire.
        let s = stats(1, 15, vec![false; 8]);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Retire);
    }

    #[test]
    fn undetermined_record_stays_shadow() {
        // 6/10: CI straddles 0.5 (lower < 0.5 < upper) → not decidable.
        let s = stats(6, 4, vec![true, true, true, true, true, true, false, false]);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Shadow);
    }

    #[test]
    fn baseline_other_than_half_is_honored() {
        // A high baseline (0.8) makes a 9/10 record no longer clearly better.
        let s = stats(9, 1, vec![true; 8]);
        assert_eq!(evaluate_rule_gate(&s, 1, 0.5), GateDecision::Inject);
        assert_ne!(
            evaluate_rule_gate(&s, 1, 0.8),
            GateDecision::Inject,
            "against a 0.8 frozen baseline a 9/10 record is not a clear win"
        );
    }
}
