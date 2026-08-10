//! WP-P1 — proper-scoring & calibration primitives (pure functions).
//!
//! Domain-agnostic scoring math for the task forward model: given
//! `(confidence, realized_outcome)` pairs from any agent, it measures whether
//! predictions are calibrated and whether they carry real skill. Everything
//! here is **pure**: zero LLM, zero I/O, no external crates — only `std`.
//!
//! Design constraints baked in (§1.3 / §3):
//! - Only **bounded** proper scores (Brier / RPS / Winkler); log score is
//!   banned because a single blow-up dominates at N≈30.
//! - Murphy decomposition (Brier = reliability − resolution + uncertainty):
//!   only rising *resolution* proves a world model; good reliability alone
//!   just means "always report the base rate".
//! - RPSS is scored against a **frozen** baseline (climatology / prior),
//!   otherwise the denominator also drifts and nothing is falsifiable.
//! - ECE is unreliable at small N → 3-bin equal-frequency description with
//!   Wilson CIs only, never 15-bin.
//!
//! Boundary policy (§3, hard requirement): empty slices, `n == 0`, out-of-range
//! `alpha`/`conf`, and `mean == 0` denominators **never panic** — they return
//! `NaN` (or a documented safe default). No `unwrap()` on nullable input.

// ─────────────────────────────────────────────────────────────────────────
// Result structs / enums
// ─────────────────────────────────────────────────────────────────────────

/// Murphy (1973) / Siegert (2017) three-way decomposition of the Brier score.
///
/// `reliability - resolution + uncertainty ≈ brier` (equality up to the
/// within-bin approximation; equal-frequency binning makes the identity
/// approximate, not exact — see [`murphy_decomposition`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MurphyParts {
    /// Mean squared gap between forecast prob and observed rate per bin
    /// (lower = better calibrated).
    pub reliability: f64,
    /// Mean squared gap between per-bin observed rate and the base rate
    /// (higher = more discriminative; this is the world-model signal).
    pub resolution: f64,
    /// Base-rate variance `ō(1-ō)` — irreducible, forecaster-independent.
    pub uncertainty: f64,
}

/// One equal-frequency reliability bin with a Wilson score interval on its
/// empirical hit rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReliabilityBin {
    /// Mean forecast confidence in the bin.
    pub p_mean: f64,
    /// Empirical hit rate in the bin (`k / n`).
    pub emp_rate: f64,
    /// Number of samples in the bin.
    pub n: usize,
    /// Wilson lower bound on `emp_rate`.
    pub wilson_lo: f64,
    /// Wilson upper bound on `emp_rate`.
    pub wilson_hi: f64,
}

/// Honest three-state verdict (§7). No "seems to work" / "preliminarily
/// validated" — only these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HonestLabel {
    /// Out-of-sample supported: CI excludes 0.5 **and** PSR ≥ 0.95.
    Supported,
    /// Not enough data to say anything (e.g. `n == 0`, NaN bounds).
    Candidate,
    /// CI contains 0.5 or PSR < 0.95 — cannot distinguish skill from luck.
    IndistinguishableFromLuck,
}

// ─────────────────────────────────────────────────────────────────────────
// §3 core proper scores
// ─────────────────────────────────────────────────────────────────────────

/// Binary Brier score: `(conf - I[correct])^2`, range `[0, 1]`.
///
/// Brier is a strictly proper score (Gneiting & Raftery, JASA 2007) and,
/// unlike log score, is bounded — robust at small N (§1.3). `conf` is clamped
/// to `[0, 1]` so a caller passing e.g. `1.2` cannot push the score above 1.
// Brier: Gneiting & Raftery 2007 (JASA 102:359); Brier 1950 (MWR 78:1).
pub fn brier_binary(conf: f64, correct: bool) -> f64 {
    if conf.is_nan() {
        return f64::NAN;
    }
    let c = conf.clamp(0.0, 1.0);
    let outcome = if correct { 1.0 } else { 0.0 };
    let d = c - outcome;
    d * d
}

/// Ranked Probability Score for 3 **ordinal** classes, range `[0, 1]`.
///
/// `p = [P(down), P(flat), P(up)]`, `outcome_idx ∈ {0,1,2}` = down/flat/up.
/// `RPS = ((p0-o0)^2 + ((p0+p1)-(o0+o1))^2) / 2` where `o` is the one-hot
/// realized class. Penalizes forecasts by *distance* along the order (calling
/// "up" when "down" happened is worse than calling "flat").
///
/// The forecast vector is normalized by its sum defensively so any
/// non-negative input yields a score in `[0, 1]`; a non-positive/NaN sum or
/// `outcome_idx > 2` returns `NaN`.
// RPS: Epstein 1969 (J. Appl. Meteor. 8:985); ordinal proper score, arXiv:2606.24959.
pub fn rps3(p: [f64; 3], outcome_idx: usize) -> f64 {
    if outcome_idx > 2 {
        return f64::NAN;
    }
    if p.iter().any(|x| x.is_nan() || *x < 0.0) {
        return f64::NAN;
    }
    let sum = p[0] + p[1] + p[2];
    if !(sum > 0.0) || !sum.is_finite() {
        return f64::NAN;
    }
    let p0 = p[0] / sum;
    let p1 = p[1] / sum;
    let (o0, o1) = match outcome_idx {
        0 => (1.0, 0.0),
        1 => (0.0, 1.0),
        _ => (0.0, 0.0),
    };
    let a = p0 - o0;
    let b = (p0 + p1) - (o0 + o1);
    (a * a + b * b) / 2.0
}

/// Winkler (1972) interval score for a central `(1-alpha)` prediction interval
/// `[l, u]` against realized `y`. Always `>= 0`.
///
/// `score = (u-l) + (2/α)(l-y)·1[y<l] + (2/α)(y-u)·1[y>u]` — rewards narrow
/// intervals but symmetrically penalizes misses on either side (Gneiting &
/// Raftery 2007 §6.2). Returns `NaN` for `alpha` outside `(0, 1]`, a degenerate
/// `l > u`, or any NaN input.
// Winkler interval score: Winkler 1972 (JASA 67:187); Gneiting & Raftery 2007 §6.2.
pub fn winkler_score(l: f64, u: f64, y: f64, alpha: f64) -> f64 {
    if l.is_nan() || u.is_nan() || y.is_nan() || alpha.is_nan() {
        return f64::NAN;
    }
    if !(alpha > 0.0 && alpha <= 1.0) {
        return f64::NAN;
    }
    if l > u {
        return f64::NAN;
    }
    let mut s = u - l;
    if y < l {
        s += (2.0 / alpha) * (l - y);
    } else if y > u {
        s += (2.0 / alpha) * (y - u);
    }
    s
}

/// Murphy / Siegert decomposition of the mean Brier score into
/// reliability, resolution, and uncertainty using **equal-frequency** binning.
///
/// `Brier ≈ reliability - resolution + uncertainty`. Bins default to 3 (§1.3:
/// ECE is unreliable at small N so we describe with 3 equal-frequency bins,
/// not 15). Empty input or `bins == 0` returns all-`NaN`; `bins` is capped at
/// the sample count.
// Murphy decomposition: Murphy 1973 (J. Appl. Meteor. 12:595); Siegert 2017 (QJRMS 143:1178).
pub fn murphy_decomposition(samples: &[(f64, bool)], bins: usize) -> MurphyParts {
    let n = samples.len();
    if n == 0 || bins == 0 {
        return MurphyParts {
            reliability: f64::NAN,
            resolution: f64::NAN,
            uncertainty: f64::NAN,
        };
    }
    let hits = samples.iter().filter(|(_, c)| *c).count();
    let base_rate = hits as f64 / n as f64;
    let uncertainty = base_rate * (1.0 - base_rate);

    // Sort a local copy by confidence for equal-frequency partitioning.
    let mut sorted: Vec<(f64, bool)> = samples
        .iter()
        .filter(|(c, _)| !c.is_nan())
        .copied()
        .collect();
    if sorted.is_empty() {
        return MurphyParts {
            reliability: f64::NAN,
            resolution: f64::NAN,
            uncertainty,
        };
    }
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let m = sorted.len();
    let k_bins = bins.min(m);
    let mut reliability = 0.0;
    let mut resolution = 0.0;
    // Equal-frequency: split m items into k_bins near-equal contiguous groups.
    let mut start = 0usize;
    for i in 0..k_bins {
        let end = (m * (i + 1)) / k_bins;
        if end <= start {
            continue;
        }
        let slice = &sorted[start..end];
        let bn = slice.len() as f64;
        let f_k = slice.iter().map(|(c, _)| *c).sum::<f64>() / bn;
        let o_k = slice.iter().filter(|(_, c)| *c).count() as f64 / bn;
        reliability += bn * (f_k - o_k) * (f_k - o_k);
        resolution += bn * (o_k - base_rate) * (o_k - base_rate);
        start = end;
    }
    let denom = m as f64;
    MurphyParts {
        reliability: reliability / denom,
        resolution: resolution / denom,
        uncertainty,
    }
}

/// Ranked Probability Skill Score against a **frozen** baseline.
///
/// `RPSS = 1 - mean(agent) / mean(baseline)`. `agent`/`baseline` are per-sample
/// proper scores (Brier or RPS). Positive = agent beats the frozen climatology;
/// 0 = no skill; negative = worse than baseline. The baseline MUST be frozen
/// (§1.3), else the denominator drifts and the score is unfalsifiable.
///
/// Returns `NaN` if either slice is empty or `mean(baseline)` is ~0 (a perfect
/// baseline leaves skill undefined — division by zero — reported as `NaN`
/// rather than a misleading `±inf`).
// RPSS: Weigel et al. 2007 (MWR 135:118); frozen-baseline requirement per design §1.3.
pub fn rpss(agent: &[f64], baseline: &[f64]) -> f64 {
    if agent.is_empty() || baseline.is_empty() {
        return f64::NAN;
    }
    let ma = mean_finite(agent);
    let mb = mean_finite(baseline);
    if ma.is_nan() || mb.is_nan() {
        return f64::NAN;
    }
    // mean(baseline) ≈ 0 → skill undefined; return NaN (not ±inf). See doc.
    if mb.abs() < 1e-12 {
        return f64::NAN;
    }
    1.0 - ma / mb
}

/// Wilson score interval for a binomial proportion `k / n` at critical value
/// `z` (e.g. `z = 1.96` for 95%). Returns `(lo, hi)`.
///
/// Preferred over the normal (Wald) interval at small N and near 0/1 — it never
/// leaves `[0, 1]` and is not degenerate at `k ∈ {0, n}`. `n == 0` returns
/// `(NaN, NaN)`; bounds are clamped to `[0, 1]`.
// Wilson score interval: Wilson 1927 (JASA 22:209).
pub fn wilson_bounds(k: u64, n: u64, z: f64) -> (f64, f64) {
    if n == 0 || z.is_nan() {
        return (f64::NAN, f64::NAN);
    }
    let n_f = n as f64;
    let p_hat = k as f64 / n_f;
    let z2 = z * z;
    let denom = 1.0 + z2 / n_f;
    let center = (p_hat + z2 / (2.0 * n_f)) / denom;
    let margin =
        (z / denom) * ((p_hat * (1.0 - p_hat) / n_f) + z2 / (4.0 * n_f * n_f)).sqrt();
    let lo = (center - margin).clamp(0.0, 1.0);
    let hi = (center + margin).clamp(0.0, 1.0);
    (lo, hi)
}

/// Bonferroni-corrected one-sided critical `z` for `k_candidates` tests.
///
/// Converts `base_z` to its one-sided upper-tail `α = 1 - Φ(base_z)`, divides
/// by `k` (Bonferroni), and maps back through the inverse normal CDF. Controls
/// the family-wise error rate when many rules are trialed (§6 point 4). `k <= 1`
/// returns `base_z` unchanged; the corrected `z` is always `>= base_z`.
// Bonferroni correction: Dunn 1961 (JASA 56:52). Φ⁻¹ via Acklam's rational approximation.
pub fn bonferroni_z(base_z: f64, k_candidates: usize) -> f64 {
    if base_z.is_nan() {
        return f64::NAN;
    }
    if k_candidates <= 1 {
        return base_z;
    }
    let alpha = 1.0 - norm_cdf(base_z);
    let alpha_corr = alpha / k_candidates as f64;
    // z' = Φ⁻¹(1 - α'). Guard the ~0 tail: an already-huge base_z can make
    // 1 - α' round to 1.0 → +inf; that's the honest "off the table" answer.
    inv_norm_cdf(1.0 - alpha_corr)
}

/// Probabilistic Sharpe Ratio (Bailey & López de Prado): the probability that
/// the true Sharpe ratio exceeds a reference `sr_ref`, given `n` observations
/// and the return distribution's `skew` and `kurt` (non-excess / raw kurtosis;
/// a normal has kurt = 3).
///
/// `PSR = Φ( (SR_obs - SR_ref)·√(n-1) / √(1 - skew·SR_obs + (kurt-1)/4·SR_obs²) )`.
///
/// Returns `NaN` for `n < 2` or a non-positive variance term. Range `[0, 1]`.
// PSR: Bailey & López de Prado, "The Sharpe Ratio Efficient Frontier", SSRN 2460551 (2012).
pub fn psr(sr_obs: f64, sr_ref: f64, n: u64, skew: f64, kurt: f64) -> f64 {
    if n < 2 || sr_obs.is_nan() || sr_ref.is_nan() || skew.is_nan() || kurt.is_nan() {
        return f64::NAN;
    }
    let var_term = 1.0 - skew * sr_obs + ((kurt - 1.0) / 4.0) * sr_obs * sr_obs;
    if !(var_term > 0.0) {
        return f64::NAN;
    }
    let numer = (sr_obs - sr_ref) * ((n - 1) as f64).sqrt();
    norm_cdf(numer / var_term.sqrt())
}

/// Minimum Track Record Length (Bailey & López de Prado): the number of
/// observations needed for `PSR(sr_ref) >= conf`, i.e. to declare `SR_obs`
/// distinguishable from `sr_ref` at confidence `conf`.
///
/// `MinTRL = 1 + (1 - skew·SR + (kurt-1)/4·SR²)·(Φ⁻¹(conf) / (SR_obs - SR_ref))²`.
///
/// Returns `NaN` if `SR_obs <= SR_ref` (target never reachable), `conf` is
/// outside `(0,1)`, or the variance term is non-positive.
// MinTRL: Bailey & López de Prado, SSRN 2460551 (2012), eq. for minimum track record.
pub fn min_track_record_length(
    sr_obs: f64,
    sr_ref: f64,
    skew: f64,
    kurt: f64,
    conf: f64,
) -> f64 {
    if sr_obs.is_nan()
        || sr_ref.is_nan()
        || skew.is_nan()
        || kurt.is_nan()
        || conf.is_nan()
    {
        return f64::NAN;
    }
    if !(conf > 0.0 && conf < 1.0) {
        return f64::NAN;
    }
    if sr_obs <= sr_ref {
        return f64::NAN;
    }
    let var_term = 1.0 - skew * sr_obs + ((kurt - 1.0) / 4.0) * sr_obs * sr_obs;
    if !(var_term > 0.0) {
        return f64::NAN;
    }
    let z = inv_norm_cdf(conf);
    let ratio = z / (sr_obs - sr_ref);
    1.0 + var_term * ratio * ratio
}

/// Equal-frequency reliability diagram: `bins` contiguous groups sorted by
/// confidence, each with mean forecast, empirical rate, and a Wilson 95% CI.
///
/// Companion to [`murphy_decomposition`] — the descriptive, per-bin view (§1.3:
/// 3-bin equal-frequency + Wilson/Beta CI, never 15-bin ECE). Empty input or
/// `bins == 0` returns an empty `Vec`; `bins` is capped at the sample count.
// Reliability diagram (equal-frequency): DeGroot & Fienberg 1983 (The Statistician 32:12).
pub fn reliability_bins_equal_freq(samples: &[(f64, bool)], bins: usize) -> Vec<ReliabilityBin> {
    if samples.is_empty() || bins == 0 {
        return Vec::new();
    }
    let mut sorted: Vec<(f64, bool)> = samples
        .iter()
        .filter(|(c, _)| !c.is_nan())
        .copied()
        .collect();
    if sorted.is_empty() {
        return Vec::new();
    }
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let m = sorted.len();
    let k_bins = bins.min(m);
    let z = 1.96; // 95% Wilson interval
    let mut out = Vec::with_capacity(k_bins);
    let mut start = 0usize;
    for i in 0..k_bins {
        let end = (m * (i + 1)) / k_bins;
        if end <= start {
            continue;
        }
        let slice = &sorted[start..end];
        let bn = slice.len();
        let p_mean = slice.iter().map(|(c, _)| *c).sum::<f64>() / bn as f64;
        let k = slice.iter().filter(|(_, c)| *c).count();
        let emp_rate = k as f64 / bn as f64;
        let (wilson_lo, wilson_hi) = wilson_bounds(k as u64, bn as u64, z);
        out.push(ReliabilityBin {
            p_mean,
            emp_rate,
            n: bn,
            wilson_lo,
            wilson_hi,
        });
        start = end;
    }
    out
}

/// Adaptive Conformal Inference online miscoverage update (§1.3 / §1.4):
/// `α_{t+1} = α_t + γ(target_α - I[err_t])`.
///
/// `err_t` is whether the realized value fell outside the last interval. Raises
/// `α` (widens future intervals) after a miss, lowers it after a hit — no
/// exchangeability assumption, robust to regime shift. Result is clamped to the
/// open interval `(0, 1)` (via `[ε, 1-ε]`) so downstream quantile math stays
/// well-defined.
// ACI: Gibbs & Candès 2021, "Adaptive Conformal Inference Under Distribution Shift", arXiv:2106.00170.
pub fn aci_update(alpha_t: f64, target_alpha: f64, err_t: bool, gamma: f64) -> f64 {
    if alpha_t.is_nan() || target_alpha.is_nan() || gamma.is_nan() {
        return f64::NAN;
    }
    let indicator = if err_t { 1.0 } else { 0.0 };
    let next = alpha_t + gamma * (target_alpha - indicator);
    // Clamp to the open interval (0,1); ε keeps it strictly inside.
    let eps = 1e-12;
    next.clamp(eps, 1.0 - eps)
}

/// Honest three-state verdict from a Wilson CI on the hit rate and a PSR (§7).
///
/// Rules (in order):
/// - NaN bounds (e.g. `n == 0`) → [`HonestLabel::Candidate`] (insufficient N).
/// - CI contains 0.5 **or** `psr < 0.95` → [`HonestLabel::IndistinguishableFromLuck`].
/// - otherwise → [`HonestLabel::Supported`].
///
/// A NaN `psr` is treated as failing the `< 0.95` test conservatively (cannot
/// confirm ≥ 0.95 → not Supported).
// Honest-luck labeling per design §7 (Wilson CI + Deflated/Probabilistic Sharpe gate).
pub fn honest_label(wilson_lo: f64, wilson_hi: f64, psr: f64) -> HonestLabel {
    if wilson_lo.is_nan() || wilson_hi.is_nan() {
        return HonestLabel::Candidate;
    }
    let ci_contains_half = wilson_lo <= 0.5 && 0.5 <= wilson_hi;
    let psr_ok = !psr.is_nan() && psr >= 0.95;
    if ci_contains_half || !psr_ok {
        return HonestLabel::IndistinguishableFromLuck;
    }
    HonestLabel::Supported
}

// ─────────────────────────────────────────────────────────────────────────
// Internal numeric helpers (std-only)
// ─────────────────────────────────────────────────────────────────────────

/// Mean over finite entries only; `NaN` if none are finite.
fn mean_finite(xs: &[f64]) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for &x in xs {
        if x.is_finite() {
            sum += x;
            count += 1;
        }
    }
    if count == 0 {
        f64::NAN
    } else {
        sum / count as f64
    }
}

/// Error function via Abramowitz & Stegun 7.1.26 (max abs error ~1.5e-7).
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-x * x).exp();
    sign * y
}

/// Standard normal CDF `Φ(x)` via [`erf`].
fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Inverse standard normal CDF `Φ⁻¹(p)` via Acklam's rational approximation
/// (relative error < 1.15e-9 over the full range). `p <= 0 → -inf`,
/// `p >= 1 → +inf`, `NaN → NaN`.
fn inv_norm_cdf(p: f64) -> f64 {
    if p.is_nan() {
        return f64::NAN;
    }
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    // Acklam coefficients.
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    const P_LOW: f64 = 0.02425;
    let p_high = 1.0 - P_LOW;
    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= p_high {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // ── brier_binary ──
    #[test]
    fn brier_perfect_and_worst() {
        // conf=1.0 correct → 0; conf=1.0 wrong → 1 (textbook).
        assert!(approx(brier_binary(1.0, true), 0.0));
        assert!(approx(brier_binary(1.0, false), 1.0));
        // conf=0.0 correct → 1; conf=0.0 wrong → 0.
        assert!(approx(brier_binary(0.0, true), 1.0));
        assert!(approx(brier_binary(0.0, false), 0.0));
    }

    #[test]
    fn brier_half_and_range() {
        // conf=0.5 either outcome → 0.25.
        assert!(approx(brier_binary(0.5, true), 0.25));
        assert!(approx(brier_binary(0.5, false), 0.25));
        // Out-of-range conf clamps into [0,1] → score stays in [0,1].
        let s = brier_binary(1.5, true);
        assert!(approx(s, 0.0));
        assert!(brier_binary(-0.5, false) >= 0.0 && brier_binary(-0.5, false) <= 1.0);
        assert!(brier_binary(f64::NAN, true).is_nan());
    }

    // ── rps3 ──
    #[test]
    fn rps_certain_correct_is_zero() {
        // All mass on the realized class → 0.
        assert!(approx(rps3([1.0, 0.0, 0.0], 0), 0.0));
        assert!(approx(rps3([0.0, 1.0, 0.0], 1), 0.0));
        assert!(approx(rps3([0.0, 0.0, 1.0], 2), 0.0));
    }

    #[test]
    fn rps_ordinal_distance_penalty() {
        // Predict "up" with certainty, "down" happens: worst ordinal miss.
        // p=[0,0,1], outcome 0: a=p0-1=-1; b=(p0+p1)-(o0+o1)=0-1=-1 → (1+1)/2=1.
        assert!(approx(rps3([0.0, 0.0, 1.0], 0), 1.0));
        // Predict "flat", "down" happens (adjacent): a=0-1=-1; b=(0+1)-(1+0)=0 → 0.5.
        assert!(approx(rps3([0.0, 1.0, 0.0], 0), 0.5));
        // Uniform forecast, any outcome, in [0,1].
        let s = rps3([1.0, 1.0, 1.0], 1);
        assert!(s >= 0.0 && s <= 1.0);
    }

    #[test]
    fn rps_boundaries() {
        assert!(rps3([1.0, 0.0, 0.0], 3).is_nan());
        assert!(rps3([0.0, 0.0, 0.0], 0).is_nan()); // zero sum
        assert!(rps3([f64::NAN, 0.0, 1.0], 0).is_nan());
    }

    // ── winkler_score ──
    #[test]
    fn winkler_hit_equals_width() {
        // y inside [l,u] → score == interval width.
        assert!(approx(winkler_score(0.0, 1.0, 0.5, 0.1), 1.0));
        assert!(approx(winkler_score(2.0, 5.0, 3.0, 0.05), 3.0));
    }

    #[test]
    fn winkler_miss_penalized_both_sides() {
        // y below l: width + (2/α)(l-y). l=0,u=1,y=-1,α=0.1 → 1 + 20*1 = 21.
        assert!(approx(winkler_score(0.0, 1.0, -1.0, 0.1), 21.0));
        // y above u: width + (2/α)(y-u). symmetric magnitude for symmetric miss.
        assert!(approx(winkler_score(0.0, 1.0, 2.0, 0.1), 21.0));
        // Always non-negative.
        assert!(winkler_score(0.0, 1.0, 5.0, 0.2) >= 0.0);
    }

    #[test]
    fn winkler_boundaries() {
        assert!(winkler_score(0.0, 1.0, 0.5, 0.0).is_nan()); // alpha out of range
        assert!(winkler_score(0.0, 1.0, 0.5, 1.5).is_nan());
        assert!(winkler_score(1.0, 0.0, 0.5, 0.1).is_nan()); // l > u
        assert!(winkler_score(f64::NAN, 1.0, 0.5, 0.1).is_nan());
    }

    // ── murphy_decomposition ──
    #[test]
    fn murphy_perfect_forecasts() {
        // conf=0 always wrong, conf=1 always right → reliability ≈ 0,
        // and Brier == 0 == reliability - resolution + uncertainty.
        let samples = vec![(0.0, false), (0.0, false), (1.0, true), (1.0, true)];
        let m = murphy_decomposition(&samples, 2);
        assert!(approx(m.reliability, 0.0));
        assert!(approx(m.resolution, 0.25));
        assert!(approx(m.uncertainty, 0.25));
        // Brier identity.
        let brier: f64 =
            samples.iter().map(|(c, o)| brier_binary(*c, *o)).sum::<f64>() / samples.len() as f64;
        assert!(approx(brier, m.reliability - m.resolution + m.uncertainty));
    }

    #[test]
    fn murphy_no_resolution_when_uninformative() {
        // Constant 0.5 forecast, 50% hits → no resolution (all bins == base rate).
        let samples = vec![(0.5, true), (0.5, false), (0.5, true), (0.5, false)];
        let m = murphy_decomposition(&samples, 2);
        assert!(approx(m.resolution, 0.0));
        assert!(approx(m.uncertainty, 0.25));
    }

    #[test]
    fn murphy_boundaries() {
        let empty: Vec<(f64, bool)> = Vec::new();
        let m = murphy_decomposition(&empty, 3);
        assert!(m.reliability.is_nan() && m.resolution.is_nan() && m.uncertainty.is_nan());
        let m0 = murphy_decomposition(&[(0.5, true)], 0);
        assert!(m0.reliability.is_nan());
    }

    // ── rpss ──
    #[test]
    fn rpss_skill_levels() {
        // Agent half the baseline error → skill 0.5.
        let agent = vec![0.1, 0.1, 0.1];
        let baseline = vec![0.2, 0.2, 0.2];
        assert!(approx(rpss(&agent, &baseline), 0.5));
        // Equal → 0 skill.
        assert!(approx(rpss(&baseline, &baseline), 0.0));
        // Worse than baseline → negative.
        assert!(rpss(&vec![0.4, 0.4], &vec![0.2, 0.2]) < 0.0);
    }

    #[test]
    fn rpss_boundaries() {
        assert!(rpss(&[], &[0.2]).is_nan());
        assert!(rpss(&[0.1], &[]).is_nan());
        // mean(baseline) == 0 → NaN, not ±inf.
        assert!(rpss(&[0.1], &[0.0, 0.0]).is_nan());
    }

    // ── wilson_bounds ──
    #[test]
    fn wilson_symmetry_at_half() {
        // k = n/2 → interval symmetric around 0.5.
        let (lo, hi) = wilson_bounds(5, 10, 1.96);
        assert!(approx((lo + hi) / 2.0, 0.5));
        assert!(lo > 0.0 && hi < 1.0 && lo < hi);
    }

    #[test]
    fn wilson_known_value_and_bounds() {
        // 18/30 at z=1.96: known Wilson 95% CI ≈ [0.423, 0.750] (design §1.4).
        let (lo, hi) = wilson_bounds(18, 30, 1.96);
        assert!((lo - 0.423).abs() < 0.01, "lo={lo}");
        assert!((hi - 0.750).abs() < 0.01, "hi={hi}");
        // CI contains 0.5 → cannot beat a coin flip at N=30.
        assert!(lo <= 0.5 && 0.5 <= hi);
    }

    #[test]
    fn wilson_extremes_stay_in_range() {
        let (lo, hi) = wilson_bounds(0, 10, 1.96);
        assert!(approx(lo, 0.0) && hi > 0.0 && hi < 1.0);
        let (lo2, hi2) = wilson_bounds(10, 10, 1.96);
        assert!(lo2 > 0.0 && lo2 < 1.0 && approx(hi2, 1.0));
        // n = 0 → NaN.
        let (ln, hn) = wilson_bounds(0, 0, 1.96);
        assert!(ln.is_nan() && hn.is_nan());
    }

    // ── bonferroni_z ──
    #[test]
    fn bonferroni_monotone_and_identity() {
        // k <= 1 → unchanged.
        assert!(approx(bonferroni_z(1.96, 1), 1.96));
        assert!(approx(bonferroni_z(1.96, 0), 1.96));
        // More candidates → stricter (larger) z.
        let z10 = bonferroni_z(1.96, 10);
        assert!(z10 > 1.96, "z10={z10}");
        let z100 = bonferroni_z(1.96, 100);
        assert!(z100 > z10);
    }

    #[test]
    fn bonferroni_known_value() {
        // base α = 0.05 (z=1.645), K=10 → α'=0.005 → z ≈ 2.576.
        let z = bonferroni_z(1.6448536269514722, 10);
        assert!((z - 2.5758).abs() < 0.01, "z={z}");
    }

    // ── psr / min_track_record_length ──
    #[test]
    fn psr_basic_properties() {
        // SR_obs == SR_ref → PSR = Φ(0) = 0.5.
        assert!((psr(1.0, 1.0, 100, 0.0, 3.0) - 0.5).abs() < 1e-6);
        // Higher observed SR with more data → PSR toward 1.
        let p = psr(2.0, 0.0, 100, 0.0, 3.0);
        assert!(p > 0.99, "psr={p}");
        // Below reference → < 0.5.
        assert!(psr(0.0, 1.0, 100, 0.0, 3.0) < 0.5);
    }

    #[test]
    fn psr_boundaries() {
        assert!(psr(1.0, 0.0, 1, 0.0, 3.0).is_nan()); // n < 2
        assert!(psr(f64::NAN, 0.0, 10, 0.0, 3.0).is_nan());
    }

    #[test]
    fn min_trl_properties() {
        // Unreachable when SR_obs <= SR_ref.
        assert!(min_track_record_length(0.5, 0.5, 0.0, 3.0, 0.95).is_nan());
        // Larger edge → shorter required track record.
        let big_edge = min_track_record_length(2.0, 0.0, 0.0, 3.0, 0.95);
        let small_edge = min_track_record_length(0.5, 0.0, 0.0, 3.0, 0.95);
        assert!(big_edge < small_edge);
        // conf out of range → NaN.
        assert!(min_track_record_length(1.0, 0.0, 0.0, 3.0, 1.5).is_nan());
    }

    // ── reliability_bins_equal_freq ──
    #[test]
    fn reliability_bins_shape() {
        let samples = vec![
            (0.1, false),
            (0.2, false),
            (0.5, true),
            (0.6, false),
            (0.9, true),
            (0.95, true),
        ];
        let bins = reliability_bins_equal_freq(&samples, 3);
        assert_eq!(bins.len(), 3);
        // Each bin has 2 samples (equal frequency).
        assert!(bins.iter().all(|b| b.n == 2));
        // Wilson bounds well-formed.
        for b in &bins {
            assert!(b.wilson_lo <= b.emp_rate + EPS && b.emp_rate <= b.wilson_hi + EPS);
        }
    }

    #[test]
    fn reliability_bins_boundaries() {
        assert!(reliability_bins_equal_freq(&[], 3).is_empty());
        assert!(reliability_bins_equal_freq(&[(0.5, true)], 0).is_empty());
        // bins capped at sample count.
        let one = reliability_bins_equal_freq(&[(0.5, true)], 5);
        assert_eq!(one.len(), 1);
    }

    // ── aci_update ──
    #[test]
    fn aci_raises_on_miss_lowers_on_hit() {
        // Miss (err=true): α decreases by γ·(1 - target)... check formula.
        // α + γ(target - I[err]). target=0.1, γ=0.05.
        assert!(approx(aci_update(0.1, 0.1, false, 0.05), 0.105)); // hit: +γ·target
        assert!(approx(aci_update(0.1, 0.1, true, 0.05), 0.055)); // miss: +γ·(target-1)
    }

    #[test]
    fn aci_clamped_to_open_interval() {
        // Cannot exceed 1 or drop to 0.
        let hi = aci_update(0.999, 0.9, false, 1.0);
        assert!(hi > 0.0 && hi < 1.0);
        let lo = aci_update(0.001, 0.0, true, 1.0);
        assert!(lo > 0.0 && lo < 1.0);
        assert!(aci_update(f64::NAN, 0.1, false, 0.05).is_nan());
    }

    // ── honest_label ──
    #[test]
    fn honest_label_states() {
        // CI excludes 0.5 and PSR high → Supported.
        assert_eq!(honest_label(0.55, 0.75, 0.99), HonestLabel::Supported);
        // CI contains 0.5 → luck.
        assert_eq!(
            honest_label(0.42, 0.75, 0.99),
            HonestLabel::IndistinguishableFromLuck
        );
        // PSR < 0.95 → luck.
        assert_eq!(
            honest_label(0.55, 0.75, 0.90),
            HonestLabel::IndistinguishableFromLuck
        );
        // NaN bounds (insufficient N) → Candidate.
        assert_eq!(honest_label(f64::NAN, f64::NAN, 0.99), HonestLabel::Candidate);
        // NaN PSR treated as failing → luck.
        assert_eq!(
            honest_label(0.55, 0.75, f64::NAN),
            HonestLabel::IndistinguishableFromLuck
        );
    }

    // ── internal helpers sanity ──
    #[test]
    fn norm_cdf_and_inverse_roundtrip() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-6);
        assert!((norm_cdf(1.959963985) - 0.975).abs() < 1e-4);
        assert!((inv_norm_cdf(0.975) - 1.959963985).abs() < 1e-4);
        assert!((inv_norm_cdf(0.5)).abs() < 1e-6);
    }
}
