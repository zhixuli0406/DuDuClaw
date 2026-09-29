//! Statistically honest eval reporting (P0/WP-D).
//!
//! Pure math only — no I/O, no async, no gateway dependency — so every
//! number here is unit-testable against a hand-computed example. Three
//! papers ground this module:
//!
//! - Miller 2024, "Adding Error Bars to Evals" (arXiv:2411.00640, Anthropic):
//!   paired design on per-question differences, cluster-robust standard
//!   errors (§2.2 / App. C), the `K`-repeat variance-reduction identity, and
//!   the sample-size / MDE relationship (Eq. 9 / Eq. 10).
//! - "Resolution Diagnostics" (arXiv:2605.30315): a comparison is only
//!   meaningful when the suite is big enough to resolve the declared minimum
//!   detectable effect (MDE) — `q = n / n_required < 1` must be reported as
//!   `unresolved`, never silently rounded up to a winner.
//! - The Replay Gap (arXiv:2608.08239): a replayed transcript is a frozen
//!   artifact of one past model run; comparing it against a live run for a
//!   *different* model manufactures a false capability delta. That guard
//!   lives in `mod.rs` (it needs the report's `mode`/`model` header), not
//!   here — this module only supplies the math.
//!
//! ## Worked example (verified by hand, reused across the tests below)
//!
//! Four cases in two directories (clusters):
//! `dir a: [1, 0]`, `dir b: [1, 1]` → values `[1, 0, 1, 1]`.
//!
//! - `mean = 0.75`
//! - `population_variance = ((0.25)² + (-0.75)² + (0.25)² + (0.25)²) / 4
//!   = 0.75 / 4 = 0.1875`
//! - `se_clt = sqrt(0.1875 / 4) = sqrt(0.046875) ≈ 0.2165064`
//! - cluster `a` residuals `[0.25, -0.75]`: `S_a = -0.5`, `S_a² = 0.25`,
//!   `Σ(residual²) = 0.0625 + 0.5625 = 0.625`, `extra_a = 0.25 - 0.625 = -0.375`
//! - cluster `b` residuals `[0.25, 0.25]`: `S_b = 0.5`, `S_b² = 0.25`,
//!   `Σ(residual²) = 0.0625 + 0.0625 = 0.125`, `extra_b = 0.25 - 0.125 = 0.125`
//! - `Var_clustered = se_clt² + (extra_a + extra_b) / n²
//!   = 0.046875 + (-0.25) / 16 = 0.03125`
//! - `se_clustered = sqrt(0.03125) ≈ 0.1767767`
//! - `se_ratio = 0.1767767 / 0.2165064 ≈ 0.8164966`

use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────
// Basic descriptive statistics
// ─────────────────────────────────────────────────────────────────────────

/// Arithmetic mean. `NaN` on an empty slice (never a silent 0 — an empty
/// suite has no honest mean).
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Population variance (divides by `n`, not `n - 1`) — matches the
/// `SE_CLT = sqrt(p(1-p)/n)` binomial form Miller's paper uses as the
/// unclustered baseline. `NaN` on an empty slice.
pub fn population_variance(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let m = mean(values);
    values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / values.len() as f64
}

/// Population covariance of two equal-length paired series. `NaN` when
/// lengths differ or either is empty.
pub fn covariance(a: &[f64], b: &[f64]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return f64::NAN;
    }
    let ma = mean(a);
    let mb = mean(b);
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - ma) * (y - mb))
        .sum::<f64>()
        / a.len() as f64
}

/// Pearson correlation. `NaN` when either series has zero variance
/// (undefined, not a fabricated 0).
pub fn pearson_correlation(a: &[f64], b: &[f64]) -> f64 {
    let va = population_variance(a);
    let vb = population_variance(b);
    if !(va > 0.0) || !(vb > 0.0) {
        return f64::NAN;
    }
    covariance(a, b) / (va.sqrt() * vb.sqrt())
}

// ─────────────────────────────────────────────────────────────────────────
// Standard errors (Miller 2024 §2.2 unclustered CLT baseline, App. C
// cluster-robust correction)
// ─────────────────────────────────────────────────────────────────────────

/// Unclustered CLT standard error of the mean: `sqrt(population_variance / n)`.
/// Named for the binary pass/fail case (this reduces to the familiar
/// `sqrt(p(1-p)/n)` binomial SE when `values` are 0/1), but the formula is
/// exact for any bounded metric (e.g. per-case judge scores).
pub fn se_clt(values: &[f64]) -> f64 {
    let n = values.len();
    if n == 0 {
        return f64::NAN;
    }
    (population_variance(values) / n as f64).sqrt()
}

/// Cluster-robust standard error of the mean (Miller 2024 §2.2):
///
/// `SE_clustered² = SE_CLT² + (1/n²) Σ_c Σ_i Σ_{j≠i∈c} (s_i - s̄)(s_j - s̄)`
///
/// Implemented via the equivalent closed form per cluster `c`
/// (`S_c = Σ_{i∈c}(s_i - s̄)`, the cluster's residual sum):
///
/// `Σ_{i}Σ_{j≠i∈c}(s_i-s̄)(s_j-s̄) = S_c² - Σ_{i∈c}(s_i-s̄)²`
///
/// so the extra term is `(1/n²) Σ_c (S_c² - Σ_{i∈c}(s_i-s̄)²)`. Positive
/// within-cluster correlation (clustermates pulled the same direction from
/// the mean) inflates the SE above `se_clt`; negative within-cluster
/// correlation can shrink it below. A cluster of size 1 contributes exactly
/// 0 (`S_c² == (s_i-s̄)²`), so a fully-unclustered input (every case its own
/// cluster) reproduces `se_clt` exactly.
///
/// `clusters[i]` is the cluster key for `values[i]`; lengths must match.
/// `NaN` on empty/mismatched input; negative floating-point noise in the
/// variance is clamped to 0 before the final `sqrt`.
pub fn se_clustered(values: &[f64], clusters: &[&str]) -> f64 {
    let n = values.len();
    if n == 0 || clusters.len() != n {
        return f64::NAN;
    }
    let m = mean(values);
    let se_clt_sq = population_variance(values) / n as f64;

    let mut groups: HashMap<&str, Vec<f64>> = HashMap::new();
    for (v, c) in values.iter().zip(clusters.iter()) {
        groups.entry(*c).or_default().push(*v);
    }

    let mut extra = 0.0;
    for members in groups.values() {
        let s_c: f64 = members.iter().map(|v| v - m).sum();
        let diag: f64 = members.iter().map(|v| (v - m).powi(2)).sum();
        extra += s_c * s_c - diag;
    }

    let var_clustered = se_clt_sq + extra / (n as f64 * n as f64);
    var_clustered.max(0.0).sqrt()
}

/// Design-effect diagnostic: how much clustering moved the SE relative to
/// the naive unclustered CLT estimate. `NaN` when `se_clt_value` is 0 (no
/// baseline to compare against — an all-identical-value suite).
pub fn se_ratio(se_clustered_value: f64, se_clt_value: f64) -> f64 {
    if !(se_clt_value > 0.0) {
        return f64::NAN;
    }
    se_clustered_value / se_clt_value
}

// ─────────────────────────────────────────────────────────────────────────
// K-repeat variance reduction (Miller 2024)
// ─────────────────────────────────────────────────────────────────────────

/// `Var(mean|K) = Var(mean|K=1) · (1+2/K) / 3` — the variance of a
/// per-question estimate averaged over `K` resampled repeats of the *same*
/// question. Unlike i.i.d. averaging (`Var/K → 0`), repeated LLM sampling on
/// one prompt is correlated (shared context, shared scoring leniency), so
/// variance floors at `1/3` of the single-draw variance as `K → ∞` instead of
/// vanishing. `K=1` is the identity (`(1+2)/3 = 1`, unchanged).
///
/// `NaN` when `k <= 0` (undefined) or `var_k1` is negative/NaN.
///
/// Exposed as a standalone planning utility (unit-tested, hand-verified —
/// see the tests below) rather than wired into `mod.rs`'s live report
/// glue: that glue estimates each report's SE empirically (cluster-robust,
/// directly from the observed per-case values — see `se_clustered`), which
/// already captures whatever real repeat-correlation structure exists
/// without needing this parametric formula. Use this function instead when
/// *planning* a suite ahead of time (`duduclaw-cli` has no CLI surface for
/// that today — a natural `duduclaw eval plan` companion command). `#[allow(dead_code)]`
/// because `duduclaw-cli` is a binary-only crate (no external crate links
/// against its `pub` surface), so nothing in-tree calls this outside its
/// own tests yet.
#[allow(dead_code)]
pub fn variance_with_k_repeats(var_k1: f64, k: f64) -> f64 {
    if !(k > 0.0) || var_k1.is_nan() || var_k1 < 0.0 {
        return f64::NAN;
    }
    var_k1 * (1.0 + 2.0 / k) / 3.0
}

// ─────────────────────────────────────────────────────────────────────────
// Paired comparison against a baseline report
// ─────────────────────────────────────────────────────────────────────────

/// Outcome of a per-case-id paired comparison between the current run and a
/// `--baseline` report.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PairedResult {
    /// Number of case ids present in both the candidate run and the baseline
    /// (the paired-design sample size — unmatched ids on either side are
    /// dropped, never guessed).
    pub n: usize,
    /// Mean of `candidate_i - baseline_i` over the matched case ids.
    pub paired_delta: f64,
    /// Pearson correlation between the candidate's and baseline's per-case
    /// values. Positive correlation is what makes pairing worthwhile
    /// (agreement on which questions are hard cancels out of the diff).
    pub corr_with_baseline: f64,
    /// Standard error backing `ci95_low`/`ci95_high`: the clustered SE of
    /// the per-case diffs, unless `fallback_to_unpaired` — see its doc.
    pub se: f64,
    /// Which estimator produced [`Self::se`]. A single-cluster suite (every
    /// case in one directory — the shape of `evals/demo/*.toml`, i.e. of the
    /// smoke run) cannot use the cluster-robust estimator at all, and
    /// [`se_clustered`] is *identically zero* there by construction. Reported
    /// so a `clt_single_cluster` interval is never read as a clustered one.
    pub se_source: crate::eval::matrix::SeSource,
    /// `se` carries no information: zero or non-finite. A zero-width interval
    /// must never be read as certainty — before this flag existed, a single
    /// directory suite reported `se: 0.0`, `ci95_low == ci95_high`, and
    /// `mod.rs`'s `effective_variance = 0` then produced `n_required = 0`,
    /// `q = NaN` and a permanent `(Unresolved, Candidate)` verdict — labelling
    /// "the estimator degenerated" as "not enough samples", which more cases
    /// could never fix.
    pub degenerate: bool,
    pub ci95_low: f64,
    pub ci95_high: f64,
    /// `true` when `corr_with_baseline < 0`: a paired design amplifies
    /// variance instead of cancelling it in that regime
    /// (`Var(paired) = Var(A) + Var(B) - 2·Cov(A,B)`, and `Cov < 0` makes
    /// that *larger* than treating the two arms as independent samples), so
    /// `se` falls back to the unpaired two-sample SE
    /// (`sqrt(se_clustered(A)² + se_clustered(B)²)`) instead of amplifying
    /// noise into a false-precision result.
    pub fallback_to_unpaired: bool,
}

/// Pair `candidate` and `baseline` by case id (inner join — an id missing on
/// either side is excluded, never treated as a 0), then compute the
/// clustered paired comparison at the 95% level implied by `alpha`.
///
/// `clusters` maps case id → cluster key (the case's directory, per the
/// module's `--cluster-by dir` default); an id absent from `clusters` falls
/// into a single `"default"` cluster (degrades to the unclustered SE for
/// that case rather than panicking on a lookup miss).
///
/// `None` when no case id is shared between the two reports — there is
/// nothing to compare, and that must never be silently reported as a
/// zero-effect tie.
pub fn paired_comparison(
    candidate: &[(String, f64)],
    baseline: &[(String, f64)],
    clusters: &HashMap<String, String>,
    alpha: f64,
) -> Option<PairedResult> {
    let baseline_map: HashMap<&str, f64> = baseline.iter().map(|(k, v)| (k.as_str(), *v)).collect();

    let mut cand_vals = Vec::new();
    let mut base_vals = Vec::new();
    let mut diffs = Vec::new();
    let mut cluster_keys: Vec<&str> = Vec::new();

    for (id, cv) in candidate {
        if let Some(&bv) = baseline_map.get(id.as_str()) {
            cand_vals.push(*cv);
            base_vals.push(bv);
            diffs.push(cv - bv);
            cluster_keys.push(clusters.get(id).map(String::as_str).unwrap_or("default"));
        }
    }

    let n = diffs.len();
    if n == 0 {
        return None;
    }

    let corr = pearson_correlation(&cand_vals, &base_vals);
    let paired_delta = mean(&diffs);
    let fallback_to_unpaired = corr.is_finite() && corr < 0.0;

    // Both branches go through `choose_se` (review finding 9). `se_clustered`
    // is identically zero on a single-cluster sample — the residual sum `S_c`
    // cancels `se_clt²` exactly — which is the correct value of a *useless*
    // estimator, not a narrow interval. `matrix::choose_se` already had this
    // fallback for cells and Δs; `paired_comparison` was the one estimator
    // that skipped it, so `--baseline` on a one-directory suite was
    // permanently `unresolved` no matter how many cases were added.
    //
    // `choose_se` lives in `matrix` (where the cell/Δ callers are) rather than
    // here; importing it is what keeps ONE definition of "which SE may I
    // report" across the whole eval layer.
    use crate::eval::matrix::{ChosenSe, SeSource, choose_se};
    let chosen = if fallback_to_unpaired {
        let a = choose_se(&cand_vals, &cluster_keys);
        let b = choose_se(&base_vals, &cluster_keys);
        ChosenSe {
            se: (a.se * a.se + b.se * b.se).sqrt(),
            // Both arms share `cluster_keys`, so they always agree; a `match`
            // rather than picking one arbitrarily keeps that explicit.
            source: match (a.source, b.source) {
                (SeSource::Clustered, SeSource::Clustered) => SeSource::Clustered,
                (SeSource::Undefined, _) | (_, SeSource::Undefined) => SeSource::Undefined,
                _ => SeSource::CltSingleCluster,
            },
            n_clusters: a.n_clusters,
        }
    } else {
        choose_se(&diffs, &cluster_keys)
    };
    let se = chosen.se;

    let z = z_two_sided(alpha);
    Some(PairedResult {
        n,
        paired_delta,
        corr_with_baseline: corr,
        se,
        se_source: chosen.source,
        degenerate: chosen.is_degenerate(),
        ci95_low: paired_delta - z * se,
        ci95_high: paired_delta + z * se,
        fallback_to_unpaired,
    })
}

/// Exact two-role Shapley attribution from a *joint team* 2×2 probe. Each
/// input row contains the same case's outcome under [weak/weak,
/// strong-planner/weak-executor, weak-planner/strong-executor, strong/strong].
/// Decoupled executor/verifier cells must never be passed here: they cannot
/// observe interactions. The full team-round harness is responsible for
/// producing these four matched outcomes.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JointShapley {
    pub n: usize,
    pub planner_phi: f64,
    pub executor_phi: f64,
    pub interaction: f64,
    pub planner_ci95: Option<[f64; 2]>,
    pub executor_ci95: Option<[f64; 2]>,
    pub se_source: &'static str,
}

/// Compute per-case Shapley values first, then their means and uncertainty.
/// `None` rejects malformed or unpaired data. A zero-width interval is absent,
/// not evidence of certainty; one cluster falls back to the unclustered CLT.
pub fn joint_shapley_2x2(rows: &[([f64; 4], &str)]) -> Option<JointShapley> {
    if rows.is_empty()
        || rows
            .iter()
            .any(|(v, _)| v.iter().any(|x| !x.is_finite() || !(0.0..=1.0).contains(x)))
    {
        return None;
    }
    let mut planner = Vec::with_capacity(rows.len());
    let mut executor = Vec::with_capacity(rows.len());
    let mut interactions = Vec::with_capacity(rows.len());
    let clusters: Vec<&str> = rows.iter().map(|(_, cluster)| *cluster).collect();
    for ([v00, v10, v01, v11], _) in rows {
        planner.push(((v10 - v00) + (v11 - v01)) / 2.0);
        executor.push(((v01 - v00) + (v11 - v10)) / 2.0);
        interactions.push(v11 - v10 - v01 + v00);
    }
    let distinct_clusters = clusters
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .len();
    let se_source = if distinct_clusters >= 2 {
        "clustered"
    } else {
        "clt_single_cluster"
    };
    let interval = |values: &[f64]| -> Option<[f64; 2]> {
        if values.len() < 2 {
            return None;
        }
        let se = if distinct_clusters >= 2 {
            se_clustered(values, &clusters)
        } else {
            se_clt(values)
        };
        if !se.is_finite() || se <= 0.0 {
            return None;
        }
        let m = mean(values);
        let z = z_two_sided(0.05);
        Some([m - z * se, m + z * se])
    };
    Some(JointShapley {
        n: rows.len(),
        planner_phi: mean(&planner),
        executor_phi: mean(&executor),
        interaction: mean(&interactions),
        planner_ci95: interval(&planner),
        executor_ci95: interval(&executor),
        se_source,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Sample-size planning (Miller 2024 Eq. 9 / Eq. 10) + resolution diagnostics
// ─────────────────────────────────────────────────────────────────────────

/// `z_{α/2}`: the two-sided critical value for significance level `alpha`
/// (e.g. `z_two_sided(0.05) ≈ 1.959964`).
pub fn z_two_sided(alpha: f64) -> f64 {
    inverse_normal_cdf(1.0 - alpha / 2.0)
}

/// `z_β` for target statistical power `power = 1 - β`
/// (e.g. `z_power(0.8) ≈ 0.841621`).
pub fn z_power(power: f64) -> f64 {
    inverse_normal_cdf(power)
}

/// Eq. 9 — required question count `n` to detect a minimum detectable
/// effect `mde` at significance `alpha` and power `power`:
///
/// `n = (z_{α/2} + z_β)² · (ω² + σ²_A/K_A + σ²_B/K_B) / δ²`
///
/// `omega2` is the between-question variance of the true (infinite-repeat)
/// per-question difference (`Var(x_A) + Var(x_B) - 2·Cov(x_A,x_B)` at
/// `K → ∞`); `sigma2_a`/`sigma2_b` are each arm's single-draw (`K=1`)
/// conditional variance, divided down by the planned repeat counts
/// `k_a`/`k_b`. Pass `omega2 = 0` and put the whole observed variance into
/// `sigma2_a` (with `sigma2_b = 0, k_a = k_b = 1`) when the two components
/// can't be separately estimated (no repeated-draw data) — see `mod.rs`'s
/// `estimate_variance_components` for exactly this fallback, which is exact
/// (not approximate) at `K=1` since the `/K` divisor is then a no-op either
/// way the split falls.
///
/// `NaN` when `mde`, `k_a`, or `k_b` is non-positive/non-finite (undefined).
pub fn n_required_for_mde(
    alpha: f64,
    power: f64,
    mde: f64,
    omega2: f64,
    sigma2_a: f64,
    sigma2_b: f64,
    k_a: f64,
    k_b: f64,
) -> f64 {
    if !(mde > 0.0) || !(k_a > 0.0) || !(k_b > 0.0) {
        return f64::NAN;
    }
    let z = z_two_sided(alpha) + z_power(power);
    z * z * (omega2 + sigma2_a / k_a + sigma2_b / k_b) / (mde * mde)
}

/// Eq. 10 — the inverse of [`n_required_for_mde`]: the minimum detectable
/// effect achievable with a fixed question count `n`.
///
/// `δ = (z_{α/2} + z_β) · sqrt(ω² + σ²_A/K_A + σ²_B/K_B) / sqrt(n)`
///
/// `NaN` when `n`, `k_a`, or `k_b` is non-positive/non-finite.
pub fn mde_for_n(
    alpha: f64,
    power: f64,
    n: f64,
    omega2: f64,
    sigma2_a: f64,
    sigma2_b: f64,
    k_a: f64,
    k_b: f64,
) -> f64 {
    if !(n > 0.0) || !(k_a > 0.0) || !(k_b > 0.0) {
        return f64::NAN;
    }
    let z = z_two_sided(alpha) + z_power(power);
    z * ((omega2 + sigma2_a / k_a + sigma2_b / k_b) / n).sqrt()
}

/// Resolution ratio `q = n / n_required` (arXiv:2605.30315). `q < 1` means
/// the suite is too small to resolve the declared MDE — the caller must
/// report `unresolved`, never round a lucky point estimate up to a winner.
///
/// `NaN` when `n_required` is non-positive/non-finite (nothing to divide by)
/// or `n` is negative/non-finite.
pub fn resolution_ratio_q(n: f64, n_required: f64) -> f64 {
    if !(n_required > 0.0) || !(n >= 0.0) {
        return f64::NAN;
    }
    n / n_required
}

// ─────────────────────────────────────────────────────────────────────────
// Honest three-state verdict/label
// ─────────────────────────────────────────────────────────────────────────

/// CI-vs-decision vocabulary surfaced in the JSON report and the one-line
/// console summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    Unresolved,
}

/// Honest three-state label (mirrors `duduclaw-gateway::prediction::calibration::HonestLabel`'s
/// naming and three-state discipline — duplicated here on purpose so this
/// crate never depends on the gateway crate for a four-variant enum. Not
/// semantically identical: the gateway's version gates on a PSR ≥ 0.95
/// investment-specific check; this one gates on resolution ratio `q` and
/// whether the confidence interval straddles the pass line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum HonestLabel {
    /// Resolved (`q >= 1`) and the CI excludes the pass line, in either
    /// direction — the evidence supports a real conclusion, win or lose.
    Supported,
    /// `q < 1`: not enough data (cases × repeats × clusters) to resolve the
    /// declared MDE at all. Not a judgment — a sample-size shortfall.
    Candidate,
    /// Resolved (`q >= 1`) but the CI still straddles the pass line: enough
    /// data was collected, and it says the outcome can't be told apart from
    /// chance (standalone mode) or from the baseline (paired mode).
    IndistinguishableFromLuck,
}

/// Classify one point estimate + CI against a `pass_line` and resolution
/// ratio `q`, producing the `(verdict, label)` pair reported at every level
/// of the `stats` block (suite, per-directory, and the top-level summary).
///
/// `pass_line` is the boundary the CI must clear to mean anything: `0.5`
/// (chance) for a standalone pass-rate check, `0.0` (no difference) for a
/// paired `--baseline` comparison.
///
/// Decision table:
/// - `q` unresolved (`NaN` or `< 1`) → `(Unresolved, Candidate)`.
/// - CI bounds non-finite → `(Unresolved, Candidate)` (can't evaluate a
///   straddle test against `NaN`/`inf`, so this fails the same way as "not
///   enough data").
/// - CI straddles `pass_line` → `(Unresolved, IndistinguishableFromLuck)`.
/// - CI entirely above `pass_line` → `(Pass, Supported)`.
/// - CI entirely below `pass_line` → `(Fail, Supported)`.
pub fn classify(
    point_estimate: f64,
    ci95_low: f64,
    ci95_high: f64,
    pass_line: f64,
    q: f64,
) -> (Verdict, HonestLabel) {
    if !(q >= 1.0) {
        return (Verdict::Unresolved, HonestLabel::Candidate);
    }
    if !ci95_low.is_finite() || !ci95_high.is_finite() {
        return (Verdict::Unresolved, HonestLabel::Candidate);
    }
    let straddles = ci95_low <= pass_line && pass_line <= ci95_high;
    if straddles {
        return (Verdict::Unresolved, HonestLabel::IndistinguishableFromLuck);
    }
    if point_estimate > pass_line {
        (Verdict::Pass, HonestLabel::Supported)
    } else {
        (Verdict::Fail, HonestLabel::Supported)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Inverse normal CDF (Acklam's rational approximation)
// ─────────────────────────────────────────────────────────────────────────

/// Standard normal quantile function `Φ⁻¹(p)`. Peter Acklam's rational
/// approximation (public domain), relative error < 1.15e-9 across
/// `(0, 1)` — no external crate needed for two z-scores.
///
/// `NaN` outside `(0, 1)`; `±∞` are not returned for `p ∈ {0, 1}` since a
/// caller here always feeds a probability strictly inside `(0, 1)` (alpha
/// and power are validated at the CLI boundary) — `p <= 0.0` or `p >= 1.0`
/// both return `NaN` rather than pretending an infinite z-score is useful.
fn inverse_normal_cdf(p: f64) -> f64 {
    if !(p > 0.0 && p < 1.0) {
        return f64::NAN;
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    // ── inverse normal CDF against known constants ──────────────────

    #[test]
    fn inverse_normal_cdf_matches_known_quantiles() {
        assert!(approx(inverse_normal_cdf(0.5), 0.0, 1e-9));
        assert!(approx(inverse_normal_cdf(0.975), 1.959964, 1e-5));
        assert!(approx(inverse_normal_cdf(0.8), 0.841621, 1e-5));
        assert!(approx(inverse_normal_cdf(0.95), 1.644854, 1e-5));
        // Symmetry: Φ⁻¹(p) == -Φ⁻¹(1-p).
        assert!(approx(
            inverse_normal_cdf(0.025),
            -inverse_normal_cdf(0.975),
            1e-9
        ));
        assert!(inverse_normal_cdf(0.0).is_nan());
        assert!(inverse_normal_cdf(1.0).is_nan());
        assert!(inverse_normal_cdf(-0.1).is_nan());
    }

    #[test]
    fn z_helpers_match_textbook_values() {
        assert!(approx(z_two_sided(0.05), 1.959964, 1e-5));
        assert!(approx(z_power(0.8), 0.841621, 1e-5));
    }

    // ── mean / variance / se_clt / se_clustered / se_ratio: the module-doc
    // worked example, hand-verified ──────────────────────────────────

    #[test]
    fn worked_example_mean_and_unclustered_se() {
        let values = [1.0, 0.0, 1.0, 1.0];
        assert!(approx(mean(&values), 0.75, 1e-12));
        assert!(approx(population_variance(&values), 0.1875, 1e-12));
        assert!(approx(se_clt(&values), 0.216506, 1e-5));
    }

    #[test]
    fn worked_example_clustered_se_can_shrink_below_clt() {
        // dir a: [1, 0] (heterogeneous — pulls in opposite directions from
        // the mean); dir b: [1, 1] (homogeneous). Hand-computed in the
        // module doc comment: se_clustered ≈ 0.1767767, se_ratio ≈ 0.8164966.
        let values = [1.0, 0.0, 1.0, 1.0];
        let clusters = ["a", "a", "b", "b"];
        let clt = se_clt(&values);
        let clustered = se_clustered(&values, &clusters);
        assert!(approx(clustered, 0.1767767, 1e-6), "got {clustered}");
        let ratio = se_ratio(clustered, clt);
        assert!(approx(ratio, 0.8164966, 1e-6), "got {ratio}");
    }

    #[test]
    fn homogeneous_clusters_inflate_se_above_clt() {
        // Two clusters, each internally identical (0.5 apart), a case where
        // clustering is expected to inflate the SE: values [1,1,0,0],
        // clusters [A,A,B,B]. Hand-computed: se_clt=0.25,
        // se_clustered=sqrt(0.125)≈0.3535534, ratio=sqrt(2)≈1.4142136.
        let values = [1.0, 1.0, 0.0, 0.0];
        let clusters = ["A", "A", "B", "B"];
        let clt = se_clt(&values);
        assert!(approx(clt, 0.25, 1e-9));
        let clustered = se_clustered(&values, &clusters);
        assert!(approx(clustered, 0.3535534, 1e-6), "got {clustered}");
        let ratio = se_ratio(clustered, clt);
        assert!(approx(ratio, std::f64::consts::SQRT_2, 1e-6), "got {ratio}");
    }

    #[test]
    fn singleton_clusters_reproduce_se_clt_exactly() {
        // Every case its own cluster ⇒ se_clustered == se_clt (the "extra"
        // term is 0 in every cluster of size 1).
        let values = [0.3, 0.9, 0.1, 0.7, 0.5];
        let clusters = ["c0", "c1", "c2", "c3", "c4"];
        let clt = se_clt(&values);
        let clustered = se_clustered(&values, &clusters);
        assert!(
            approx(clt, clustered, 1e-12),
            "clt={clt} clustered={clustered}"
        );
        assert!(approx(se_ratio(clustered, clt), 1.0, 1e-9));
    }

    #[test]
    fn se_functions_are_nan_safe_on_empty_or_mismatched_input() {
        assert!(mean(&[]).is_nan());
        assert!(population_variance(&[]).is_nan());
        assert!(se_clt(&[]).is_nan());
        assert!(se_clustered(&[], &[]).is_nan());
        assert!(
            se_clustered(&[1.0, 2.0], &["a"]).is_nan(),
            "length mismatch must be NaN, not panic"
        );
        assert!(se_ratio(0.1, 0.0).is_nan());
    }

    // ── K-repeat variance reduction ──────────────────────────────────

    #[test]
    fn k_repeat_variance_reduction_identity_and_floor() {
        // K=1: no reduction (identity).
        assert!(approx(variance_with_k_repeats(0.2, 1.0), 0.2, 1e-12));
        // K=4: (1+2/4)/3 = 1.5/3 = 0.5 ⇒ half the single-draw variance.
        assert!(approx(variance_with_k_repeats(0.2, 4.0), 0.1, 1e-12));
        // K→∞ floors at 1/3 of the single-draw variance, never 0.
        let huge = variance_with_k_repeats(0.9, 1_000_000.0);
        assert!(approx(huge, 0.9 / 3.0, 1e-6), "got {huge}");
        assert!(variance_with_k_repeats(-0.1, 4.0).is_nan());
        assert!(variance_with_k_repeats(0.1, 0.0).is_nan());
    }

    // ── paired comparison: the module-doc worked example ─────────────

    #[test]
    fn paired_comparison_worked_example() {
        // candidate = [1,1,0,1], baseline = [0,1,0,0] over ids c1..c4.
        // Hand-computed: mean_c=0.75, mean_b=0.25, cov=0.0625,
        // var_c=var_b=0.1875, corr=1/3, diffs=[1,0,0,1], paired_delta=0.5,
        // Var(diff)=varA+varB-2cov=0.375-0.125=0.25 (identity check against
        // the direct diffs variance below), unclustered se=sqrt(0.25/4)=0.25,
        // ci95 = 0.5 ± 1.959964*0.25 = [0.010009, 0.989991].
        let candidate = vec![
            ("c1".to_string(), 1.0),
            ("c2".to_string(), 1.0),
            ("c3".to_string(), 0.0),
            ("c4".to_string(), 1.0),
        ];
        let baseline = vec![
            ("c1".to_string(), 0.0),
            ("c2".to_string(), 1.0),
            ("c3".to_string(), 0.0),
            ("c4".to_string(), 0.0),
        ];
        // No clustering signal (every case its own cluster) so se_clustered
        // reduces to the plain unclustered diff SE for this check.
        let mut clusters = HashMap::new();
        for id in ["c1", "c2", "c3", "c4"] {
            clusters.insert(id.to_string(), id.to_string());
        }

        let cand_vals: Vec<f64> = candidate.iter().map(|(_, v)| *v).collect();
        let base_vals: Vec<f64> = baseline.iter().map(|(_, v)| *v).collect();
        assert!(approx(
            pearson_correlation(&cand_vals, &base_vals),
            1.0 / 3.0,
            1e-9
        ));

        let result = paired_comparison(&candidate, &baseline, &clusters, 0.05).unwrap();
        assert_eq!(result.n, 4);
        assert!(approx(result.paired_delta, 0.5, 1e-12));
        assert!(approx(result.corr_with_baseline, 1.0 / 3.0, 1e-9));
        assert!(!result.fallback_to_unpaired, "correlation is positive here");
        assert!(approx(result.se, 0.25, 1e-9), "got {}", result.se);
        assert!(
            approx(result.ci95_low, 0.010009, 1e-4),
            "got {}",
            result.ci95_low
        );
        assert!(
            approx(result.ci95_high, 0.989991, 1e-4),
            "got {}",
            result.ci95_high
        );
    }

    #[test]
    fn paired_comparison_falls_back_to_unpaired_on_negative_correlation() {
        // Perfectly anti-correlated arms: candidate improves exactly where
        // baseline fails and vice versa.
        let candidate = vec![
            ("c1".to_string(), 1.0),
            ("c2".to_string(), 0.0),
            ("c3".to_string(), 1.0),
            ("c4".to_string(), 0.0),
        ];
        let baseline = vec![
            ("c1".to_string(), 0.0),
            ("c2".to_string(), 1.0),
            ("c3".to_string(), 0.0),
            ("c4".to_string(), 1.0),
        ];
        let clusters = HashMap::new();
        let result = paired_comparison(&candidate, &baseline, &clusters, 0.05).unwrap();
        assert!(result.corr_with_baseline < 0.0);
        assert!(result.fallback_to_unpaired);
        // Review finding 9: this test used to assert only the flag, which is
        // exactly how the single-cluster zero SE slipped through. An empty
        // `clusters` map puts every case in one `"default"` cluster, so the
        // cluster-robust estimator is undefined and the CLT fallback must have
        // fired with a real, positive SE.
        assert_eq!(
            result.se_source,
            crate::eval::matrix::SeSource::CltSingleCluster
        );
        assert!(
            result.se > 0.0 && result.se.is_finite(),
            "a single-cluster SE must be the CLT estimate, never an identically-zero \
             cluster-robust one: {result:?}"
        );
        assert!(!result.degenerate);
        assert!(
            result.ci95_low < result.ci95_high,
            "a zero-width interval would be false precision: {result:?}"
        );
    }

    /// Review finding 9 regression: the realistic shape — one directory, so
    /// one cluster — must still produce a usable interval, because
    /// `se_clustered` is identically zero there.
    #[test]
    fn paired_comparison_on_a_single_directory_suite_reports_a_usable_interval() {
        let candidate = vec![
            ("c1".to_string(), 1.0),
            ("c2".to_string(), 1.0),
            ("c3".to_string(), 0.0),
            ("c4".to_string(), 1.0),
        ];
        let baseline = vec![
            ("c1".to_string(), 1.0),
            ("c2".to_string(), 0.0),
            ("c3".to_string(), 0.0),
            ("c4".to_string(), 0.0),
        ];
        // Every case in `evals/demo` — one cluster.
        let clusters: HashMap<String, String> = ["c1", "c2", "c3", "c4"]
            .iter()
            .map(|c| (c.to_string(), "demo".to_string()))
            .collect();
        let result = paired_comparison(&candidate, &baseline, &clusters, 0.05).unwrap();
        assert_eq!(
            result.se_source,
            crate::eval::matrix::SeSource::CltSingleCluster
        );
        assert!(result.se > 0.0, "{result:?}");
        assert!(!result.degenerate, "{result:?}");
        // The pre-fix value, pinned so a regression is visible.
        assert_eq!(
            se_clustered(&[0.0, 1.0, 0.0, 1.0], &["demo", "demo", "demo", "demo"]),
            0.0,
            "the estimator this fallback exists for really is identically zero"
        );
    }

    /// An all-identical sample has no spread at all: the CLT SE is legitimately
    /// zero and must be labelled `degenerate` rather than reported as a
    /// confident tie.
    #[test]
    fn paired_comparison_flags_a_genuinely_degenerate_sample() {
        let candidate = vec![("c1".to_string(), 1.0), ("c2".to_string(), 1.0)];
        let baseline = vec![("c1".to_string(), 1.0), ("c2".to_string(), 1.0)];
        let clusters: HashMap<String, String> = ["c1", "c2"]
            .iter()
            .map(|c| (c.to_string(), "demo".to_string()))
            .collect();
        let result = paired_comparison(&candidate, &baseline, &clusters, 0.05).unwrap();
        assert_eq!(result.se, 0.0);
        assert!(
            result.degenerate,
            "a zero-width interval must be flagged, not read as certainty: {result:?}"
        );
    }

    #[test]
    fn paired_comparison_only_matches_shared_case_ids() {
        let candidate = vec![("c1".to_string(), 1.0), ("only-candidate".to_string(), 1.0)];
        let baseline = vec![("c1".to_string(), 0.0), ("only-baseline".to_string(), 0.0)];
        let clusters = HashMap::new();
        let result = paired_comparison(&candidate, &baseline, &clusters, 0.05).unwrap();
        assert_eq!(
            result.n, 1,
            "unmatched ids on either side must be excluded, not zero-filled"
        );
    }

    #[test]
    fn paired_comparison_none_when_no_overlap() {
        let candidate = vec![("only-a".to_string(), 1.0)];
        let baseline = vec![("only-b".to_string(), 0.0)];
        assert!(paired_comparison(&candidate, &baseline, &HashMap::new(), 0.05).is_none());
    }

    // ── sample-size planning: Eq. 9 / Eq. 10 round-trip ───────────────

    #[test]
    fn n_required_for_mde_matches_textbook_two_proportion_sample_size() {
        // Classic textbook check: detecting a 10-percentage-point gap at
        // worst-case Bernoulli variance p=0.5 (var=0.25 per arm), alpha=0.05,
        // power=0.8, K=1 each side. Widely cited value: n ≈ 392 per arm
        // ((1.959964+0.841621)^2 * 0.5 / 0.01 ≈ 392.44).
        let n = n_required_for_mde(0.05, 0.8, 0.10, 0.0, 0.25, 0.25, 1.0, 1.0);
        assert!(approx(n, 392.44, 0.5), "got {n}");
    }

    #[test]
    fn mde_for_n_is_the_exact_inverse_of_n_required_for_mde() {
        let mde = 0.10;
        let n = n_required_for_mde(0.05, 0.8, mde, 0.0, 0.25, 0.25, 1.0, 1.0);
        let recovered = mde_for_n(0.05, 0.8, n, 0.0, 0.25, 0.25, 1.0, 1.0);
        assert!(approx(recovered, mde, 1e-6), "got {recovered}");
    }

    #[test]
    fn n_required_shrinks_with_more_repeats() {
        let n_k1 = n_required_for_mde(0.05, 0.8, 0.10, 0.0, 0.25, 0.25, 1.0, 1.0);
        let n_k4 = n_required_for_mde(0.05, 0.8, 0.10, 0.0, 0.25, 0.25, 4.0, 4.0);
        assert!(
            n_k4 < n_k1,
            "more repeats per question must lower the required question count"
        );
    }

    #[test]
    fn sample_size_functions_are_nan_safe() {
        assert!(n_required_for_mde(0.05, 0.8, 0.0, 0.0, 0.1, 0.1, 1.0, 1.0).is_nan());
        assert!(n_required_for_mde(0.05, 0.8, 0.1, 0.0, 0.1, 0.1, 0.0, 1.0).is_nan());
        assert!(mde_for_n(0.05, 0.8, 0.0, 0.0, 0.1, 0.1, 1.0, 1.0).is_nan());
    }

    #[test]
    fn resolution_ratio_q_basic() {
        assert!(approx(resolution_ratio_q(200.0, 400.0), 0.5, 1e-9));
        assert!(resolution_ratio_q(200.0, 0.0).is_nan());
        assert!(resolution_ratio_q(-1.0, 400.0).is_nan());
        assert!(resolution_ratio_q(800.0, 400.0) > 1.0);
    }

    // ── classify(): the (verdict, label) decision table ───────────────

    #[test]
    fn classify_unresolved_when_q_below_one() {
        let (verdict, label) = classify(0.6, 0.55, 0.65, 0.5, 0.5);
        assert_eq!(verdict, Verdict::Unresolved);
        assert_eq!(label, HonestLabel::Candidate);
    }

    #[test]
    fn classify_unresolved_indistinguishable_when_ci_straddles_pass_line_and_resolved() {
        let (verdict, label) = classify(0.55, 0.40, 0.65, 0.5, 1.2);
        assert_eq!(verdict, Verdict::Unresolved);
        assert_eq!(label, HonestLabel::IndistinguishableFromLuck);
    }

    #[test]
    fn classify_pass_when_resolved_and_ci_entirely_above_pass_line() {
        let (verdict, label) = classify(0.7, 0.60, 0.80, 0.5, 1.5);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(label, HonestLabel::Supported);
    }

    #[test]
    fn classify_fail_when_resolved_and_ci_entirely_below_pass_line() {
        let (verdict, label) = classify(0.3, 0.20, 0.40, 0.5, 1.5);
        assert_eq!(verdict, Verdict::Fail);
        assert_eq!(label, HonestLabel::Supported);
    }

    #[test]
    fn classify_unresolved_on_nan_ci_bounds() {
        let (verdict, label) = classify(0.6, f64::NAN, 0.9, 0.5, 2.0);
        assert_eq!(verdict, Verdict::Unresolved);
        assert_eq!(label, HonestLabel::Candidate);
    }

    #[test]
    fn joint_shapley_allocates_interaction_without_blame_to_last_actor() {
        // In the first case planner alone adds 0.2, executor alone 0.1,
        // and together they unlock another 0.4. Each gets half of it.
        let rows = [([0.0, 0.2, 0.1, 0.7], "a"), ([0.0, 0.4, 0.1, 0.9], "b")];
        let result = joint_shapley_2x2(&rows).unwrap();
        assert_eq!(result.n, 2);
        assert!((result.planner_phi - 0.5).abs() < 1e-10);
        assert!((result.executor_phi - 0.3).abs() < 1e-10);
        assert!((result.interaction - 0.4).abs() < 1e-10);
        assert!((result.planner_phi + result.executor_phi - 0.8).abs() < 1e-10);
    }

    #[test]
    fn joint_shapley_rejects_fake_precision_and_invalid_scores() {
        let identical = [([0.0, 0.0, 0.0, 1.0], "one"), ([0.0, 0.0, 0.0, 1.0], "one")];
        let result = joint_shapley_2x2(&identical).unwrap();
        assert_eq!(result.se_source, "clt_single_cluster");
        assert!(result.planner_ci95.is_none());
        assert!(result.executor_ci95.is_none());
        assert!(joint_shapley_2x2(&[([0.0, 0.0, 0.0, f64::NAN], "a")]).is_none());
    }
}
