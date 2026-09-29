//! Exploratory Monte Carlo sensitivity with common exogenous draws.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_sim::{
    DecisionSnapshot, QueueModel, SimulationError, StaffingScenario, simulate,
};

const MAX_RUNS: usize = 1_000;
const MAX_DAILY_ARRIVALS: u32 = 1_000_000;

/// Fixed SplitMix64 draw stream. This is for reproducible scenario sensitivity,
/// not cryptography; changing it requires an engine version bump.
pub(crate) struct DrawStream(pub(crate) u64);

impl DrawStream {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D049BB133111EB);
        value ^ (value >> 31)
    }

    /// Uniform draw in `[band.min, band.max]`. An inverted band is a caller
    /// bug: it panics in debug and is clamped to a single value in release,
    /// rather than wrapping into a width that silently breaks the rejection
    /// threshold. Callers still validate their bands before sampling.
    pub(crate) fn inclusive(&mut self, band: BoundedCount) -> u32 {
        debug_assert!(
            band.min <= band.max,
            "sampling band must not be inverted: {}..={}",
            band.min,
            band.max
        );
        let width = (band.max as u64).saturating_sub(band.min as u64) + 1;
        let threshold = width.wrapping_neg() % width;
        loop {
            let draw = self.next();
            if draw >= threshold {
                return (band.min as u64 + draw % width) as u32;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedCount {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitivityPlan {
    pub runs: usize,
    pub daily_arrival_bands: Vec<BoundedCount>,
    pub service_capacity_band: BoundedCount,
    pub max_final_backlog: u64,
    pub max_staff_cost_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitivityReport {
    pub status: String,
    pub replay_hash: String,
    pub seed: u64,
    pub runs: usize,
    /// Alternative minus baseline final backlog; lower is better.
    pub delta_backlog_p05: i128,
    pub delta_backlog_p50: i128,
    pub delta_backlog_p95: i128,
    pub alternative_lower_backlog_bps: u32,
    pub baseline_backlog_violation_bps: u32,
    pub alternative_backlog_violation_bps: u32,
    pub baseline_cost_violation_bps: u32,
    pub alternative_cost_violation_bps: u32,
    pub worst_alternative_backlog: u64,
    pub limitations: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SensitivityError {
    #[error("invalid or unbounded sensitivity plan")]
    InvalidPlan,
    #[error("simulation failed: {0}")]
    Simulation(#[from] SimulationError),
    #[error("sensitivity replay serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

fn bps(count: usize, runs: usize) -> u32 {
    ((count as u64 * 10_000) / runs as u64) as u32
}

fn percentile(sorted: &[i128], percentage: usize) -> i128 {
    sorted[(sorted.len() - 1) * percentage / 100]
}

/// The same sampled arrivals and capacity are used for both scenarios in each
/// run. Bands must be externally justified; this function does not fit them.
pub fn simulate_sensitivity(
    snapshot: &DecisionSnapshot,
    model: &QueueModel,
    baseline: &StaffingScenario,
    alternative: &StaffingScenario,
    plan: &SensitivityPlan,
) -> Result<SensitivityReport, SensitivityError> {
    if baseline.id == alternative.id
        || plan.runs == 0
        || plan.runs > MAX_RUNS
        || plan.daily_arrival_bands.len() != snapshot.arrivals_by_day.len()
        || plan.daily_arrival_bands.is_empty()
        || plan.service_capacity_band.min == 0
        || plan.service_capacity_band.min > plan.service_capacity_band.max
        || plan
            .daily_arrival_bands
            .iter()
            .any(|band| band.min > band.max || band.max > MAX_DAILY_ARRIVALS)
    {
        return Err(SensitivityError::InvalidPlan);
    }
    let replay_bytes = serde_json::to_vec(&(
        "support-sensitivity-splitmix64-v1",
        snapshot,
        model,
        baseline,
        alternative,
        plan,
    ))?;
    let replay_hash = format!("{:x}", Sha256::digest(replay_bytes));
    let mut rng = DrawStream(snapshot.seed);
    let mut deltas = Vec::with_capacity(plan.runs);
    let mut lower_count = 0;
    let mut base_backlog_violations = 0;
    let mut alt_backlog_violations = 0;
    let mut base_cost_violations = 0;
    let mut alt_cost_violations = 0;
    let mut worst_alt_backlog = 0;
    for _ in 0..plan.runs {
        let mut draw_snapshot = snapshot.clone();
        for (arrivals, band) in draw_snapshot
            .arrivals_by_day
            .iter_mut()
            .zip(&plan.daily_arrival_bands)
        {
            *arrivals = rng.inclusive(*band);
        }
        let mut draw_model = model.clone();
        draw_model.service_capacity_per_agent_day = rng.inclusive(plan.service_capacity_band);
        let base = simulate(&draw_snapshot, &draw_model, baseline)?;
        let alt = simulate(&draw_snapshot, &draw_model, alternative)?;
        let delta = alt.final_backlog as i128 - base.final_backlog as i128;
        deltas.push(delta);
        lower_count += usize::from(delta < 0);
        base_backlog_violations += usize::from(base.final_backlog > plan.max_final_backlog);
        alt_backlog_violations += usize::from(alt.final_backlog > plan.max_final_backlog);
        base_cost_violations +=
            usize::from(base.total_staff_cost_cents > plan.max_staff_cost_cents);
        alt_cost_violations += usize::from(alt.total_staff_cost_cents > plan.max_staff_cost_cents);
        worst_alt_backlog = worst_alt_backlog.max(alt.final_backlog);
    }
    deltas.sort_unstable();
    Ok(SensitivityReport {
        status: "exploratory".into(), replay_hash, seed: snapshot.seed, runs: plan.runs,
        delta_backlog_p05: percentile(&deltas, 5),
        delta_backlog_p50: percentile(&deltas, 50),
        delta_backlog_p95: percentile(&deltas, 95),
        alternative_lower_backlog_bps: bps(lower_count, plan.runs),
        baseline_backlog_violation_bps: bps(base_backlog_violations, plan.runs),
        alternative_backlog_violation_bps: bps(alt_backlog_violations, plan.runs),
        baseline_cost_violation_bps: bps(base_cost_violations, plan.runs),
        alternative_cost_violation_bps: bps(alt_cost_violations, plan.runs),
        worst_alternative_backlog: worst_alt_backlog,
        limitations: vec![
            "Demand and capacity bands are supplied inputs, not calibrated probability distributions".into(),
            "Uniform draws omit temporal correlation, demand response, and operational constraints beyond the stated limits".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_draw_stream_has_a_versioned_known_output() {
        let mut stream = DrawStream(0);
        assert_eq!(stream.next(), 0xe220_a839_7b1d_cdaf);
    }

    /// Regression: an inverted band used to wrap `width` around instead of
    /// being caught, which left the rejection threshold meaningless.
    #[test]
    #[should_panic(expected = "sampling band must not be inverted")]
    fn inverted_sampling_band_is_caught_instead_of_wrapping() {
        DrawStream(1).inclusive(BoundedCount { min: 5, max: 4 });
    }

    #[test]
    fn single_value_band_returns_that_value() {
        let mut stream = DrawStream(9);
        assert_eq!(stream.inclusive(BoundedCount { min: 7, max: 7 }), 7);
    }

    #[test]
    fn common_draws_replay_and_report_tail_and_violations() {
        let snapshot = DecisionSnapshot {
            id: "s1".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["tickets-v1".into()],
            seed: 87,
            arrivals_by_day: vec![5, 5, 5],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let base = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1; 3],
            fixed_extra_capacity_by_day: vec![0; 3],
        };
        let alt = StaffingScenario {
            id: "alt".into(),
            agents_by_day: vec![2; 3],
            fixed_extra_capacity_by_day: vec![0; 3],
        };
        let plan = SensitivityPlan {
            runs: 100,
            daily_arrival_bands: vec![BoundedCount { min: 4, max: 6 }; 3],
            service_capacity_band: BoundedCount { min: 2, max: 4 },
            max_final_backlog: 4,
            max_staff_cost_cents: 400,
        };
        let a = simulate_sensitivity(&snapshot, &model, &base, &alt, &plan).unwrap();
        let b = simulate_sensitivity(&snapshot, &model, &base, &alt, &plan).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.status, "exploratory");
        assert!(a.delta_backlog_p95 <= 0);
        assert!(a.alternative_lower_backlog_bps > 0);
        assert_eq!(a.alternative_cost_violation_bps, 10_000);
        assert_eq!(a.baseline_cost_violation_bps, 0);
    }

    #[test]
    fn bad_bands_are_rejected() {
        let snapshot = DecisionSnapshot {
            id: "s1".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["tickets-v1".into()],
            seed: 1,
            arrivals_by_day: vec![1],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 1,
            sla_days: 1,
            staff_cost_cents_per_agent_day: 1,
        };
        let base = StaffingScenario {
            id: "b".into(),
            agents_by_day: vec![1],
            fixed_extra_capacity_by_day: vec![0],
        };
        let alt = StaffingScenario {
            id: "a".into(),
            agents_by_day: vec![2],
            fixed_extra_capacity_by_day: vec![0],
        };
        let plan = SensitivityPlan {
            runs: 2,
            daily_arrival_bands: vec![BoundedCount { min: 3, max: 1 }],
            service_capacity_band: BoundedCount { min: 1, max: 1 },
            max_final_backlog: 1,
            max_staff_cost_cents: 1,
        };
        assert!(matches!(
            simulate_sensitivity(&snapshot, &model, &base, &alt, &plan),
            Err(SensitivityError::InvalidPlan)
        ));
    }
}
