//! Scoring (SPEC §5): attainment, work, parallel penalty, the beta sweep's
//! Pareto AUC and reward, context mismatch rate, and the 9-decimal rounding
//! every emitted float goes through.
//!
//! Internal arithmetic runs on unrounded values; [`round9`] is applied once,
//! when a value is emitted (stored in a score struct, serialized, compared).

use serde::{Deserialize, Serialize};

use super::tree::WorldTree;

/// Fixed beta grid of the sweep (SPEC §5).
pub const BETA_GRID: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
/// λ of `pareto_reward = pareto_auc − λ · parallel_penalty` (SPEC §5).
pub const PARETO_LAMBDA: f64 = 0.1;

/// Round to 9 decimal places the way Python's `round(x, 9)` does: the exact
/// binary value is rounded to the nearest 9-decimal string (ties to even)
/// and parsed back. Non-finite values pass through unchanged.
pub fn round9(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.9}").parse::<f64>().unwrap_or(x)
}

/// Best valid score among the revealed cells, in oriented space; the
/// oriented baseline when none is valid.
pub fn oriented_best<'a, I>(tree: &WorldTree, revealed: I) -> f64
where
    I: IntoIterator<Item = &'a str>,
{
    let dir = tree.world().direction;
    let mut best: Option<f64> = None;
    for id in revealed {
        if let Some(s) = tree.node(id).and_then(|n| n.valid_score()) {
            let o = dir.orient(s);
            best = Some(best.map_or(o, |b: f64| b.max(o)));
        }
    }
    best.unwrap_or_else(|| dir.orient(tree.world().baseline_score))
}

/// Attainment of a revealed set (SPEC §5), unrounded.
pub fn attainment<'a, I>(tree: &WorldTree, revealed: I) -> f64
where
    I: IntoIterator<Item = &'a str>,
{
    let baseline = tree.world().direction.orient(tree.world().baseline_score);
    let best = oriented_best(tree, revealed);
    let ceiling = tree.oriented_ceiling();
    attainment_from(best, ceiling, baseline)
}

/// Attainment from oriented `best`, `ceiling`, `baseline`.
pub fn attainment_from(best: f64, ceiling: f64, baseline: f64) -> f64 {
    if ceiling > baseline {
        ((best - baseline) / (ceiling - baseline)).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// `probes / total_cells`; 0 when the world is empty.
pub fn work(probes: u64, total_cells: usize) -> f64 {
    if total_cells == 0 {
        return 0.0;
    }
    probes as f64 / total_cells as f64
}

/// `effective_sequential_rounds / probes`, or 1.0 without probes.
pub fn parallel_penalty(effective_sequential_rounds: u64, probes: u64) -> f64 {
    if probes > 0 {
        effective_sequential_rounds as f64 / probes as f64
    } else {
        1.0
    }
}

/// `mismatches / probes`, or 0 without probes.
pub fn context_mismatch_rate(mismatches: u64, probes: u64) -> f64 {
    if probes > 0 {
        mismatches as f64 / probes as f64
    } else {
        0.0
    }
}

/// Exact integral over `[0, 1]` of the step frontier
/// `F(x) = max{attainment_i : work_i ≤ x}` (0 where no point qualifies).
/// `points` are `(work, attainment)`; non-finite points are ignored and
/// `work` is clamped into `[0, 1]`.
pub fn pareto_auc(points: &[(f64, f64)]) -> f64 {
    let mut pts: Vec<(f64, f64)> = points
        .iter()
        .filter(|(w, a)| w.is_finite() && a.is_finite())
        .map(|&(w, a)| (w.clamp(0.0, 1.0), a))
        .collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut auc = 0.0;
    let mut frontier = 0.0_f64;
    let mut i = 0;
    while i < pts.len() {
        let x = pts[i].0;
        while i < pts.len() && pts[i].0 == x {
            frontier = frontier.max(pts[i].1);
            i += 1;
        }
        let next = if i < pts.len() { pts[i].0 } else { 1.0 };
        auc += (next - x) * frontier;
    }
    auc
}

/// `pareto_auc − λ · mean_parallel_penalty`.
pub fn pareto_reward(pareto_auc: f64, mean_parallel_penalty: f64) -> f64 {
    pareto_auc - PARETO_LAMBDA * mean_parallel_penalty
}

/// One point of the beta sweep. Floats are rounded to 9 decimals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointScore {
    pub beta: f64,
    pub probes: u64,
    pub decision_rounds: u64,
    pub effective_sequential_rounds: u64,
    pub attainment: f64,
    pub work: f64,
    pub parallel_penalty: f64,
    pub context_mismatch_rate: f64,
    /// Probe sequence: the cell ids of each accepted batch, in order.
    pub batches: Vec<Vec<String>>,
}

/// Unrounded per-point values, kept so aggregates are computed before
/// rounding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawPoint {
    pub attainment: f64,
    pub work: f64,
    pub parallel_penalty: f64,
}

/// Aggregate of one (policy, world) sweep. Floats are rounded to 9 decimals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldScore {
    pub run_id: String,
    pub round: u32,
    pub points: Vec<PointScore>,
    pub pareto_auc: f64,
    /// Mean of the points' parallel penalties.
    pub parallel_penalty: f64,
    pub pareto_reward: f64,
    pub out_of_support: bool,
}

impl WorldScore {
    /// Build the aggregate from the sweep's points and their raw values.
    pub fn aggregate(
        run_id: String,
        round: u32,
        points: Vec<PointScore>,
        raw: &[RawPoint],
        out_of_support: bool,
    ) -> Self {
        let pairs: Vec<(f64, f64)> = raw.iter().map(|p| (p.work, p.attainment)).collect();
        let auc = pareto_auc(&pairs);
        let mean_penalty = if raw.is_empty() {
            0.0
        } else {
            raw.iter().map(|p| p.parallel_penalty).sum::<f64>() / raw.len() as f64
        };
        let reward = if out_of_support {
            0.0
        } else {
            pareto_reward(auc, mean_penalty)
        };
        Self {
            run_id,
            round,
            points,
            pareto_auc: round9(auc),
            parallel_penalty: round9(mean_penalty),
            pareto_reward: round9(reward),
            out_of_support,
        }
    }
}

/// `V`: mean `pareto_reward` over worlds (0 for an empty slice).
pub fn policy_value(worlds: &[WorldScore]) -> f64 {
    if worlds.is_empty() {
        return 0.0;
    }
    round9(worlds.iter().map(|w| w.pareto_reward).sum::<f64>() / worlds.len() as f64)
}
