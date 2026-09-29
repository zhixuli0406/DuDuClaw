//! Training-prefix empirical parameter draws for exploratory support scenarios.
//!
//! These are resampled observations, not a calibrated posterior or forecast.
//! Saturated days with exactly divisible staff service provide integer capacity
//! samples. Other days give per-day lower bounds only. If samples are too few
//! or fail to cover those bounds, simulation requires an explicit operator range.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_calibration::{
    CalibrationError, ObservedSupportDay, saturated_capacity_samples, validate,
};
use crate::decision_sensitivity::{BoundedCount, DrawStream};
use crate::decision_sim::{
    DecisionSnapshot, QueueModel, SimulationError, StaffingScenario, engine_code_sha256, simulate,
    simulate_with_capacity_path,
};

const MAX_RUNS: usize = 1_000;
const MAX_COUNT: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityIdentification {
    EmpiricalSaturatedDays,
    Unidentified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EmpiricalSamplingMode {
    #[default]
    Independent,
    PairedSaturatedDays,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaturatedDayPair {
    pub training_day_index: usize,
    pub arrivals: u32,
    pub capacity_per_agent: u32,
}

/// Unsaturated days reveal work completed, but leave unused capacity unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsaturatedDayCapacityBound {
    pub training_day_index: usize,
    pub minimum_capacity_per_agent: u32,
}

/// A saturated day with service not divisible by staffed agents cannot
/// identify the integer per-agent capacity assumed by the simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialSaturatedDayCapacityBound {
    pub training_day_index: usize,
    pub minimum_capacity_per_agent: u32,
    pub observed_staff_resolutions: u32,
    pub agents: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpiricalParameterFit {
    pub method: String,
    pub training_days: usize,
    pub min_saturated_days: usize,
    pub arrival_samples: Vec<u32>,
    pub capacity_samples: Vec<u32>,
    #[serde(default)]
    pub saturated_day_pairs: Vec<SaturatedDayPair>,
    #[serde(default)]
    pub unsaturated_day_lower_bounds: Vec<UnsaturatedDayCapacityBound>,
    #[serde(default)]
    pub partial_saturated_day_lower_bounds: Vec<PartialSaturatedDayCapacityBound>,
    pub capacity_identification: CapacityIdentification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpiricalSensitivityPlan {
    pub runs: usize,
    /// Consecutive training days per draw. Paired mode requires every day in
    /// each source block to have an identifiable saturated-day capacity.
    pub arrival_block_days: usize,
    #[serde(default)]
    pub sampling_mode: EmpiricalSamplingMode,
    /// Required when saturated evidence is insufficient or its observed
    /// support cannot cover an unsaturated-day lower bound.
    pub capacity_fallback_range: Option<BoundedCount>,
    pub max_final_backlog: u64,
    pub max_staff_cost_cents: u64,
    /// Operator-supplied minimum completed within SLA over the full horizon.
    pub min_sla_resolved: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpiricalSensitivityReport {
    pub status: String,
    pub replay_hash: String,
    pub common_draws_sha256: String,
    pub seed: u64,
    pub runs: usize,
    pub training_days: usize,
    pub arrival_sample_count: usize,
    pub arrival_block_days: usize,
    pub sampling_mode: EmpiricalSamplingMode,
    pub paired_saturated_day_count: usize,
    pub capacity_sample_count: usize,
    pub unsaturated_lower_bound_day_count: usize,
    #[serde(default)]
    pub partial_saturated_day_count: usize,
    pub max_unsaturated_capacity_lower_bound: u32,
    pub capacity_source: String,
    pub max_final_backlog: u64,
    pub max_staff_cost_cents: u64,
    pub min_sla_resolved: Option<u64>,
    pub delta_backlog_p05: i128,
    pub delta_backlog_p50: i128,
    pub delta_backlog_p95: i128,
    pub delta_sla_resolved_p05: i128,
    pub delta_sla_resolved_p50: i128,
    pub delta_sla_resolved_p95: i128,
    pub baseline_sla_resolved_p05: u64,
    pub baseline_sla_resolved_p50: u64,
    pub baseline_sla_resolved_p95: u64,
    pub alternative_sla_resolved_p05: u64,
    pub alternative_sla_resolved_p50: u64,
    pub alternative_sla_resolved_p95: u64,
    pub alternative_lower_backlog_bps: u32,
    pub alternative_more_sla_resolved_bps: u32,
    pub baseline_backlog_violation_bps: u32,
    pub alternative_backlog_violation_bps: u32,
    pub baseline_cost_violation_bps: u32,
    pub alternative_cost_violation_bps: u32,
    pub baseline_sla_target_violation_bps: Option<u32>,
    pub alternative_sla_target_violation_bps: Option<u32>,
    /// A draw violates when backlog, cost, or the configured SLA target fails.
    /// This union is measured on each shared draw, not reconstructed from margins.
    pub baseline_joint_constraint_violation_bps: u32,
    pub alternative_joint_constraint_violation_bps: u32,
    /// Shared draws where baseline violates at least one limit and alternative meets all.
    pub alternative_joint_constraint_recovery_bps: u32,
    pub worst_alternative_backlog: u64,
    pub worst_alternative_sla_resolved: u64,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JointCapacityHoldoutReport {
    pub method: String,
    pub training_days: usize,
    pub holdout_days: usize,
    pub scored_saturated_days: usize,
    pub skipped_unsaturated_days: usize,
    #[serde(default)]
    pub skipped_partial_saturated_days: usize,
    pub out_of_training_arrival_range_days: usize,
    pub paired_absolute_error_sum: u128,
    pub independent_median_absolute_error_sum: u128,
    pub last_training_capacity_absolute_error_sum: u128,
    pub paired_beats_independent: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EmpiricalError {
    #[error("invalid or mismatched empirical parameter input")]
    Invalid,
    #[error("service capacity unidentified; supply an explicit positive fallback range")]
    Unidentified,
    #[error("no identifiable saturated days in the holdout period")]
    NoHoldoutCapacity,
    #[error("invalid observed support history: {0}")]
    Calibration(#[from] CalibrationError),
    #[error("simulation failed: {0}")]
    Simulation(#[from] SimulationError),
    #[error("empirical replay serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Fit from `days[..training_days]` only. Later days may be reserved for an
/// honest holdout check; their values do not enter either sample pool.
pub fn fit_empirical_parameters(
    days: &[ObservedSupportDay],
    training_days: usize,
    min_saturated_days: usize,
) -> Result<EmpiricalParameterFit, EmpiricalError> {
    if training_days < 7 || training_days > days.len() || min_saturated_days == 0 {
        return Err(EmpiricalError::Invalid);
    }
    let training = &days[..training_days];
    validate(training)?;
    let arrival_samples: Vec<u32> = training.iter().map(|day| day.arrivals).collect();
    if arrival_samples.iter().any(|&n| n > MAX_COUNT) {
        return Err(EmpiricalError::Invalid);
    }
    let candidates = saturated_capacity_samples(training)?;
    if candidates.iter().any(|&n| !(1..=MAX_COUNT).contains(&n)) {
        return Err(EmpiricalError::Invalid);
    }
    let mut unsaturated_day_lower_bounds = Vec::new();
    let mut partial_saturated_day_lower_bounds = Vec::new();
    for (index, day) in training.iter().enumerate() {
        if day.agents == 0 {
            if day.resolved > day.fixed_extra_capacity {
                return Err(EmpiricalError::Invalid);
            }
            continue;
        }
        if day.backlog_end == 0 {
            let staff_required = day.resolved.saturating_sub(day.fixed_extra_capacity) as u64;
            let lower = staff_required.div_ceil(day.agents as u64);
            let lower = u32::try_from(lower).map_err(|_| EmpiricalError::Invalid)?;
            if lower > MAX_COUNT {
                return Err(EmpiricalError::Invalid);
            }
            unsaturated_day_lower_bounds.push(UnsaturatedDayCapacityBound {
                training_day_index: index,
                minimum_capacity_per_agent: lower,
            });
        } else {
            let staff_resolved = day.resolved - day.fixed_extra_capacity;
            if staff_resolved % day.agents != 0 {
                let lower = (staff_resolved as u64).div_ceil(day.agents as u64);
                let lower = u32::try_from(lower).map_err(|_| EmpiricalError::Invalid)?;
                if lower > MAX_COUNT {
                    return Err(EmpiricalError::Invalid);
                }
                partial_saturated_day_lower_bounds.push(PartialSaturatedDayCapacityBound {
                    training_day_index: index,
                    minimum_capacity_per_agent: lower,
                    observed_staff_resolutions: staff_resolved,
                    agents: day.agents,
                });
            }
        }
    }
    let max_unsaturated_bound = unsaturated_day_lower_bounds
        .iter()
        .map(|bound| bound.minimum_capacity_per_agent)
        .max()
        .unwrap_or(0);
    let identified = candidates.len() >= min_saturated_days
        && partial_saturated_day_lower_bounds.is_empty()
        && max_unsaturated_bound <= candidates.iter().copied().max().unwrap_or(0);
    Ok(EmpiricalParameterFit {
        method: "training_prefix_empirical_resampling_v4".into(),
        training_days,
        min_saturated_days,
        arrival_samples,
        capacity_samples: candidates.clone(),
        saturated_day_pairs: training
            .iter()
            .enumerate()
            .filter(|(_, day)| {
                day.backlog_end > 0
                    && day.agents > 0
                    && (day.resolved - day.fixed_extra_capacity) % day.agents == 0
            })
            .zip(candidates.iter())
            .map(|((index, day), capacity)| SaturatedDayPair {
                training_day_index: index,
                arrivals: day.arrivals,
                capacity_per_agent: *capacity,
            })
            .collect(),
        unsaturated_day_lower_bounds,
        partial_saturated_day_lower_bounds,
        capacity_identification: if identified {
            CapacityIdentification::EmpiricalSaturatedDays
        } else {
            CapacityIdentification::Unidentified
        },
    })
}

/// Evaluate demand-conditioned capacity using only the training prefix.
/// Actual held-out arrivals are supplied for this diagnostic, so it is not a
/// live arrival forecast or an intervention-effect estimate.
pub fn evaluate_joint_capacity_holdout(
    days: &[ObservedSupportDay],
    training_days: usize,
    min_saturated_days: usize,
) -> Result<JointCapacityHoldoutReport, EmpiricalError> {
    if days.len() < training_days.saturating_add(3) {
        return Err(EmpiricalError::Invalid);
    }
    validate(days)?;
    let fit = fit_empirical_parameters(days, training_days, min_saturated_days)?;
    if fit.capacity_identification != CapacityIdentification::EmpiricalSaturatedDays {
        return Err(EmpiricalError::Unidentified);
    }
    let mut sorted_capacity = fit.capacity_samples.clone();
    sorted_capacity.sort_unstable();
    let independent_median = sorted_capacity[sorted_capacity.len() / 2];
    let last_training_capacity = fit
        .saturated_day_pairs
        .last()
        .ok_or(EmpiricalError::Unidentified)?
        .capacity_per_agent;
    let min_training_arrivals = fit
        .saturated_day_pairs
        .iter()
        .map(|pair| pair.arrivals)
        .min()
        .ok_or(EmpiricalError::Unidentified)?;
    let max_training_arrivals = fit
        .saturated_day_pairs
        .iter()
        .map(|pair| pair.arrivals)
        .max()
        .ok_or(EmpiricalError::Unidentified)?;
    let heldout = &days[training_days..];
    let observed_capacities = saturated_capacity_samples(heldout)?;
    if observed_capacities.is_empty() {
        return Err(EmpiricalError::NoHoldoutCapacity);
    }
    let mut scored_saturated_days = 0;
    let skipped_partial_saturated_days = heldout
        .iter()
        .filter(|day| {
            day.backlog_end > 0
                && day.agents > 0
                && (day.resolved - day.fixed_extra_capacity) % day.agents != 0
        })
        .count();
    let mut out_of_training_arrival_range_days = 0;
    let mut paired_absolute_error_sum = 0_u128;
    let mut independent_median_absolute_error_sum = 0_u128;
    let mut last_training_capacity_absolute_error_sum = 0_u128;
    for (day, observed_capacity) in heldout
        .iter()
        .filter(|day| {
            day.backlog_end > 0
                && day.agents > 0
                && (day.resolved - day.fixed_extra_capacity) % day.agents == 0
        })
        .zip(observed_capacities)
    {
        if observed_capacity == 0 || observed_capacity > MAX_COUNT {
            return Err(EmpiricalError::Invalid);
        }
        let nearest = fit
            .saturated_day_pairs
            .iter()
            .min_by_key(|pair| {
                (
                    pair.arrivals.abs_diff(day.arrivals),
                    pair.training_day_index,
                )
            })
            .ok_or(EmpiricalError::Unidentified)?;
        paired_absolute_error_sum += nearest.capacity_per_agent.abs_diff(observed_capacity) as u128;
        independent_median_absolute_error_sum +=
            independent_median.abs_diff(observed_capacity) as u128;
        last_training_capacity_absolute_error_sum +=
            last_training_capacity.abs_diff(observed_capacity) as u128;
        scored_saturated_days += 1;
        out_of_training_arrival_range_days += usize::from(
            day.arrivals < min_training_arrivals || day.arrivals > max_training_arrivals,
        );
    }
    Ok(JointCapacityHoldoutReport {
        method: "conditional_nearest_arrival_capacity_v1".into(),
        training_days,
        holdout_days: heldout.len(),
        scored_saturated_days,
        skipped_unsaturated_days: heldout.len() - scored_saturated_days
            - skipped_partial_saturated_days,
        skipped_partial_saturated_days,
        out_of_training_arrival_range_days,
        paired_absolute_error_sum,
        independent_median_absolute_error_sum,
        last_training_capacity_absolute_error_sum,
        paired_beats_independent: paired_absolute_error_sum < independent_median_absolute_error_sum,
        limitations: vec![
            "This diagnostic uses actual held-out arrivals; it does not forecast future demand".into(),
            "Only backlog-positive days with service divisible by staffed agents identify the modeled integer per-agent capacity; selection may bias the apparent demand-capacity relation".into(),
            "Nearest-arrival matching does not validate scenario probabilities or causal effects".into(),
        ],
    })
}

/// Uniform draw from a non-empty slice. Every caller validates non-emptiness
/// first; returning `None` keeps an empty slice from underflowing the band.
fn sample<T: Copy>(rng: &mut DrawStream, values: &[T]) -> Option<T> {
    let last = values.len().checked_sub(1)?;
    let index = rng.inclusive(BoundedCount {
        min: 0,
        max: u32::try_from(last).ok()?,
    }) as usize;
    values.get(index).copied()
}

fn bps(count: usize, runs: usize) -> u32 {
    (count as u64 * 10_000 / runs as u64) as u32
}

fn percentile(sorted: &[i128], percentage: usize) -> i128 {
    sorted[(sorted.len() - 1) * percentage / 100]
}

/// Every run gives both staffing scenarios the same sampled external path.
/// Paired mode resamples arrivals and identifiable capacity together, but
/// only saturated training days can supply such pairs.
pub fn simulate_empirical_sensitivity(
    snapshot: &DecisionSnapshot,
    model: &QueueModel,
    baseline: &StaffingScenario,
    alternative: &StaffingScenario,
    fit: &EmpiricalParameterFit,
    plan: &EmpiricalSensitivityPlan,
) -> Result<EmpiricalSensitivityReport, EmpiricalError> {
    if baseline.id == alternative.id
        || plan.runs == 0
        || plan.runs > MAX_RUNS
        || plan.arrival_block_days == 0
        || plan.arrival_block_days > fit.training_days
        || fit.method != "training_prefix_empirical_resampling_v4"
        || fit.training_days < 7
        || fit.min_saturated_days == 0
        || fit.training_days > snapshot.arrivals_by_day.len()
        || fit.arrival_samples.len() != fit.training_days
        || fit.arrival_samples != snapshot.arrivals_by_day[..fit.training_days]
        || fit.arrival_samples.iter().any(|&n| n > MAX_COUNT)
        || fit.saturated_day_pairs.len() != fit.capacity_samples.len()
        || fit
            .saturated_day_pairs
            .iter()
            .zip(&fit.capacity_samples)
            .any(|(pair, capacity)| {
                pair.training_day_index >= fit.training_days
                    || fit.arrival_samples[pair.training_day_index] != pair.arrivals
                    || pair.capacity_per_agent != *capacity
            })
        || fit
            .saturated_day_pairs
            .windows(2)
            .any(|pair| pair[0].training_day_index >= pair[1].training_day_index)
        || fit.unsaturated_day_lower_bounds.iter().any(|bound| {
            bound.training_day_index >= fit.training_days
                || bound.minimum_capacity_per_agent > MAX_COUNT
                || fit
                    .saturated_day_pairs
                    .binary_search_by_key(&bound.training_day_index, |pair| pair.training_day_index)
                    .is_ok()
        })
        || fit
            .unsaturated_day_lower_bounds
            .windows(2)
            .any(|pair| pair[0].training_day_index >= pair[1].training_day_index)
        || fit.partial_saturated_day_lower_bounds.iter().any(|bound| {
            bound.training_day_index >= fit.training_days
                || bound.agents == 0
                || bound.observed_staff_resolutions % bound.agents == 0
                || bound.minimum_capacity_per_agent
                    != (bound.observed_staff_resolutions as u64).div_ceil(bound.agents as u64)
                        as u32
                || bound.minimum_capacity_per_agent > MAX_COUNT
                || fit
                    .saturated_day_pairs
                    .binary_search_by_key(&bound.training_day_index, |pair| pair.training_day_index)
                    .is_ok()
                || fit
                    .unsaturated_day_lower_bounds
                    .binary_search_by_key(&bound.training_day_index, |day| day.training_day_index)
                    .is_ok()
        })
        || fit
            .partial_saturated_day_lower_bounds
            .windows(2)
            .any(|pair| pair[0].training_day_index >= pair[1].training_day_index)
        || fit
            .capacity_samples
            .iter()
            .any(|&n| !(1..=MAX_COUNT).contains(&n))
    {
        return Err(EmpiricalError::Invalid);
    }
    let max_unsaturated_bound = fit
        .unsaturated_day_lower_bounds
        .iter()
        .map(|bound| bound.minimum_capacity_per_agent)
        .max()
        .unwrap_or(0);
    let max_partial_bound = fit
        .partial_saturated_day_lower_bounds
        .iter()
        .map(|bound| bound.minimum_capacity_per_agent)
        .max()
        .unwrap_or(0);
    let identified_by_observations = fit.capacity_samples.len() >= fit.min_saturated_days
        && fit.partial_saturated_day_lower_bounds.is_empty()
        && max_unsaturated_bound <= fit.capacity_samples.iter().copied().max().unwrap_or(0);
    let capacity_source = match fit.capacity_identification {
        CapacityIdentification::EmpiricalSaturatedDays => {
            if !identified_by_observations
                || fit
                    .capacity_samples
                    .iter()
                    .any(|&n| !(1..=MAX_COUNT).contains(&n))
                || plan.capacity_fallback_range.is_some()
            {
                return Err(EmpiricalError::Invalid);
            }
            if plan.sampling_mode == EmpiricalSamplingMode::PairedSaturatedDays {
                if plan.arrival_block_days == 1 {
                    "saturated_day_paired_empirical"
                } else {
                    "saturated_day_paired_block_empirical"
                }
            } else {
                "saturated_day_empirical"
            }
        }
        CapacityIdentification::Unidentified => {
            if identified_by_observations {
                return Err(EmpiricalError::Invalid);
            }
            if plan.sampling_mode == EmpiricalSamplingMode::PairedSaturatedDays {
                return Err(EmpiricalError::Unidentified);
            }
            let Some(range) = plan.capacity_fallback_range else {
                return Err(EmpiricalError::Unidentified);
            };
            if range.min == 0 || range.min >= range.max || range.max > MAX_COUNT {
                return Err(EmpiricalError::Invalid);
            }
            if fit
                .capacity_samples
                .iter()
                .any(|&sample| sample < range.min || sample > range.max)
                || fit
                    .unsaturated_day_lower_bounds
                    .iter()
                    .any(|bound| bound.minimum_capacity_per_agent > range.max)
                || max_partial_bound > range.max
            {
                return Err(EmpiricalError::Invalid);
            }
            "operator_range_uniform"
        }
    };
    let paired_block_starts: Vec<usize> =
        if plan.sampling_mode == EmpiricalSamplingMode::PairedSaturatedDays {
            fit.saturated_day_pairs
                .windows(plan.arrival_block_days)
                .enumerate()
                .filter_map(|(start, block)| {
                    block
                        .windows(2)
                        .all(|pair| pair[1].training_day_index == pair[0].training_day_index + 1)
                        .then_some(start)
                })
                .collect()
        } else {
            Vec::new()
        };
    if plan.sampling_mode == EmpiricalSamplingMode::PairedSaturatedDays
        && paired_block_starts.is_empty()
    {
        return Err(EmpiricalError::Unidentified);
    }
    let replay_bytes = serde_json::to_vec(&(
        "support-empirical-splitmix64-v1",
        engine_code_sha256(),
        format!(
            "{:x}",
            Sha256::digest(include_str!("decision_empirical.rs").as_bytes())
        ),
        snapshot,
        model,
        baseline,
        alternative,
        fit,
        plan,
    ))?;
    let mut rng = DrawStream(snapshot.seed);
    let mut draw_digest = Sha256::new();
    let mut deltas = Vec::with_capacity(plan.runs);
    let mut sla_deltas = Vec::with_capacity(plan.runs);
    let mut base_sla_counts = Vec::with_capacity(plan.runs);
    let mut alt_sla_counts = Vec::with_capacity(plan.runs);
    let mut lower = 0;
    let mut more_sla = 0;
    let mut base_backlog_violations = 0;
    let mut alt_backlog_violations = 0;
    let mut base_cost_violations = 0;
    let mut alt_cost_violations = 0;
    let mut base_sla_violations = 0;
    let mut alt_sla_violations = 0;
    let mut base_joint_violations = 0;
    let mut alt_joint_violations = 0;
    let mut alt_joint_recoveries = 0;
    let mut worst_alt_backlog = 0;
    let mut worst_alt_sla = u64::MAX;
    for _ in 0..plan.runs {
        let mut draw_snapshot = snapshot.clone();
        let (base, alt) = if plan.sampling_mode == EmpiricalSamplingMode::PairedSaturatedDays {
            let mut capacity_by_day = Vec::with_capacity(draw_snapshot.arrivals_by_day.len());
            let mut day = 0;
            while day < draw_snapshot.arrivals_by_day.len() {
                let start =
                    sample(&mut rng, &paired_block_starts).ok_or(EmpiricalError::Invalid)?;
                let length = plan
                    .arrival_block_days
                    .min(draw_snapshot.arrivals_by_day.len() - day);
                for offset in 0..length {
                    let pair = fit.saturated_day_pairs[start + offset];
                    draw_snapshot.arrivals_by_day[day + offset] = pair.arrivals;
                    capacity_by_day.push(pair.capacity_per_agent);
                    draw_digest.update(pair.arrivals.to_le_bytes());
                    draw_digest.update(pair.capacity_per_agent.to_le_bytes());
                }
                day += length;
            }
            (
                simulate_with_capacity_path(&draw_snapshot, model, baseline, &capacity_by_day)?,
                simulate_with_capacity_path(&draw_snapshot, model, alternative, &capacity_by_day)?,
            )
        } else {
            let mut day = 0;
            while day < draw_snapshot.arrivals_by_day.len() {
                let start = rng.inclusive(BoundedCount {
                    min: 0,
                    max: (fit.training_days - plan.arrival_block_days) as u32,
                }) as usize;
                let length = plan
                    .arrival_block_days
                    .min(draw_snapshot.arrivals_by_day.len() - day);
                for offset in 0..length {
                    let arrivals = fit.arrival_samples[start + offset];
                    draw_snapshot.arrivals_by_day[day + offset] = arrivals;
                    draw_digest.update(arrivals.to_le_bytes());
                }
                day += length;
            }
            let capacity = match plan.capacity_fallback_range {
                Some(range) => rng.inclusive(range),
                None => {
                    sample(&mut rng, &fit.capacity_samples).ok_or(EmpiricalError::Invalid)?
                }
            };
            draw_digest.update(capacity.to_le_bytes());
            let mut draw_model = model.clone();
            draw_model.service_capacity_per_agent_day = capacity;
            (
                simulate(&draw_snapshot, &draw_model, baseline)?,
                simulate(&draw_snapshot, &draw_model, alternative)?,
            )
        };
        let delta = alt.final_backlog as i128 - base.final_backlog as i128;
        let sla_delta =
            alt.total_resolved_within_sla as i128 - base.total_resolved_within_sla as i128;
        deltas.push(delta);
        sla_deltas.push(sla_delta);
        base_sla_counts.push(base.total_resolved_within_sla as i128);
        alt_sla_counts.push(alt.total_resolved_within_sla as i128);
        lower += usize::from(delta < 0);
        more_sla += usize::from(sla_delta > 0);
        let base_backlog_bad = base.final_backlog > plan.max_final_backlog;
        let alt_backlog_bad = alt.final_backlog > plan.max_final_backlog;
        let base_cost_bad = base.total_staff_cost_cents > plan.max_staff_cost_cents;
        let alt_cost_bad = alt.total_staff_cost_cents > plan.max_staff_cost_cents;
        base_backlog_violations += usize::from(base_backlog_bad);
        alt_backlog_violations += usize::from(alt_backlog_bad);
        base_cost_violations += usize::from(base_cost_bad);
        alt_cost_violations += usize::from(alt_cost_bad);
        let mut base_sla_bad = false;
        let mut alt_sla_bad = false;
        if let Some(target) = plan.min_sla_resolved {
            base_sla_bad = base.total_resolved_within_sla < target;
            alt_sla_bad = alt.total_resolved_within_sla < target;
            base_sla_violations += usize::from(base_sla_bad);
            alt_sla_violations += usize::from(alt_sla_bad);
        }
        let base_joint_bad = base_backlog_bad || base_cost_bad || base_sla_bad;
        let alt_joint_bad = alt_backlog_bad || alt_cost_bad || alt_sla_bad;
        base_joint_violations += usize::from(base_joint_bad);
        alt_joint_violations += usize::from(alt_joint_bad);
        alt_joint_recoveries += usize::from(base_joint_bad && !alt_joint_bad);
        worst_alt_backlog = worst_alt_backlog.max(alt.final_backlog);
        worst_alt_sla = worst_alt_sla.min(alt.total_resolved_within_sla);
    }
    deltas.sort_unstable();
    sla_deltas.sort_unstable();
    base_sla_counts.sort_unstable();
    alt_sla_counts.sort_unstable();
    Ok(EmpiricalSensitivityReport {
        status: "exploratory_empirical_resampling".into(),
        replay_hash: format!("{:x}", Sha256::digest(replay_bytes)),
        common_draws_sha256: format!("{:x}", draw_digest.finalize()),
        seed: snapshot.seed, runs: plan.runs, training_days: fit.training_days,
        arrival_sample_count: fit.arrival_samples.len(),
        arrival_block_days: plan.arrival_block_days,
        sampling_mode: plan.sampling_mode,
        paired_saturated_day_count: fit.saturated_day_pairs.len(),
        capacity_sample_count: fit.capacity_samples.len(),
        unsaturated_lower_bound_day_count: fit.unsaturated_day_lower_bounds.len(),
        partial_saturated_day_count: fit.partial_saturated_day_lower_bounds.len(),
        max_unsaturated_capacity_lower_bound: max_unsaturated_bound,
        capacity_source: capacity_source.into(),
        max_final_backlog: plan.max_final_backlog,
        max_staff_cost_cents: plan.max_staff_cost_cents,
        min_sla_resolved: plan.min_sla_resolved,
        delta_backlog_p05: percentile(&deltas, 5),
        delta_backlog_p50: percentile(&deltas, 50),
        delta_backlog_p95: percentile(&deltas, 95),
        delta_sla_resolved_p05: percentile(&sla_deltas, 5),
        delta_sla_resolved_p50: percentile(&sla_deltas, 50),
        delta_sla_resolved_p95: percentile(&sla_deltas, 95),
        baseline_sla_resolved_p05: percentile(&base_sla_counts, 5) as u64,
        baseline_sla_resolved_p50: percentile(&base_sla_counts, 50) as u64,
        baseline_sla_resolved_p95: percentile(&base_sla_counts, 95) as u64,
        alternative_sla_resolved_p05: percentile(&alt_sla_counts, 5) as u64,
        alternative_sla_resolved_p50: percentile(&alt_sla_counts, 50) as u64,
        alternative_sla_resolved_p95: percentile(&alt_sla_counts, 95) as u64,
        alternative_lower_backlog_bps: bps(lower, plan.runs),
        alternative_more_sla_resolved_bps: bps(more_sla, plan.runs),
        baseline_backlog_violation_bps: bps(base_backlog_violations, plan.runs),
        alternative_backlog_violation_bps: bps(alt_backlog_violations, plan.runs),
        baseline_cost_violation_bps: bps(base_cost_violations, plan.runs),
        alternative_cost_violation_bps: bps(alt_cost_violations, plan.runs),
        baseline_sla_target_violation_bps: plan
            .min_sla_resolved
            .map(|_| bps(base_sla_violations, plan.runs)),
        alternative_sla_target_violation_bps: plan
            .min_sla_resolved
            .map(|_| bps(alt_sla_violations, plan.runs)),
        baseline_joint_constraint_violation_bps: bps(base_joint_violations, plan.runs),
        alternative_joint_constraint_violation_bps: bps(alt_joint_violations, plan.runs),
        alternative_joint_constraint_recovery_bps: bps(alt_joint_recoveries, plan.runs),
        worst_alternative_backlog: worst_alt_backlog,
        worst_alternative_sla_resolved: worst_alt_sla,
        limitations: vec![
            "Empirical samples are from a finite training prefix and are not calibrated predictive probabilities".into(),
            if plan.arrival_block_days == 1 {
                "Independent-day resampling omits seasonality, temporal correlation, and demand response".into()
            } else {
                "Contiguous training blocks retain only within-block demand dependence; block boundaries, seasonality, and demand response remain unmodeled".into()
            },
            if plan.sampling_mode == EmpiricalSamplingMode::PairedSaturatedDays {
                "Paired arrival-capacity draws use only saturated days, which may select high-demand conditions; only consecutive saturated training days can form a paired block, and unsaturated-day capacity remains unidentified".into()
            } else {
                "Per-agent capacity is sampled independently of demand; the joint demand-capacity distribution is unvalidated".into()
            },
            "Unsaturated and nondivisible saturated days impose only per-day capacity lower bounds; an operator range must cover those bounds and exact saturated samples but remains an assumption, not a fit".into(),
            "A nondivisible saturated day cannot be reproduced exactly by this integer per-agent capacity model; its operator range is a sensitivity assumption until partial shifts or service variability are modeled".into(),
            "SLA targets are operator inputs; violation fractions describe resampled scenarios, not calibrated operational probabilities".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_ingest::build_support_pilot;
    use crate::decision_synthetic::synthetic_support_export;

    /// Regression: sampling an empty slice used to underflow `len() - 1` and
    /// panic; it now reports "no value" so the caller decides.
    #[test]
    fn sampling_an_empty_slice_returns_none_instead_of_underflowing() {
        let mut rng = DrawStream(3);
        assert_eq!(sample(&mut rng, &[] as &[u32]), None);
        assert_eq!(sample(&mut rng, &[42u32]), Some(42));
    }

    #[test]
    fn training_prefix_fit_and_common_draws_are_replayable() {
        let export = synthetic_support_export(47, 35).unwrap();
        let pilot = build_support_pilot(&export).unwrap();
        let fit = fit_empirical_parameters(&pilot.observed_days, 21, 7).unwrap();
        assert_eq!(
            fit.capacity_identification,
            CapacityIdentification::EmpiricalSaturatedDays
        );
        assert!(fit.capacity_samples.iter().all(|&value| value == 8));
        assert_eq!(fit.arrival_samples, pilot.snapshot.arrivals_by_day[..21]);
        let mut changed_holdout = pilot.observed_days.clone();
        for day in &mut changed_holdout[21..] {
            day.arrivals = 999_999;
        }
        assert_eq!(
            fit,
            fit_empirical_parameters(&changed_holdout, 21, 7).unwrap()
        );
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 10_000,
        };
        let alt = StaffingScenario {
            id: "three".into(),
            agents_by_day: vec![3; 35],
            fixed_extra_capacity_by_day: vec![0; 35],
        };
        let plan = EmpiricalSensitivityPlan {
            runs: 100,
            arrival_block_days: 7,
            sampling_mode: EmpiricalSamplingMode::Independent,
            capacity_fallback_range: None,
            max_final_backlog: 20,
            max_staff_cost_cents: 1_000_000,
            min_sla_resolved: Some(525),
        };
        let a = simulate_empirical_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &alt,
            &fit,
            &plan,
        )
        .unwrap();
        let b = simulate_empirical_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &alt,
            &fit,
            &plan,
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.capacity_source, "saturated_day_empirical");
        assert_eq!(a.arrival_block_days, 7);
        assert_eq!(a.min_sla_resolved, Some(525));
        assert!(a.delta_sla_resolved_p50 >= 0);
        assert!(a.baseline_sla_resolved_p05 <= a.baseline_sla_resolved_p50);
        assert!(a.baseline_sla_resolved_p50 <= a.baseline_sla_resolved_p95);
        assert!(a.alternative_sla_resolved_p05 <= a.alternative_sla_resolved_p50);
        assert!(a.alternative_sla_resolved_p50 <= a.alternative_sla_resolved_p95);
        assert!(a.alternative_sla_resolved_p50 >= a.baseline_sla_resolved_p50);
        assert!(a.alternative_more_sla_resolved_bps > 0);
        assert!(a.baseline_sla_target_violation_bps.is_some());
        assert!(a.alternative_sla_target_violation_bps.is_some());
        assert!(a.delta_backlog_p95 <= 0);
        let mut overlapping_failures = plan.clone();
        overlapping_failures.max_final_backlog = 0;
        overlapping_failures.max_staff_cost_cents = 0;
        overlapping_failures.min_sla_resolved = Some(u64::MAX);
        let overlap = simulate_empirical_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &alt,
            &fit,
            &overlapping_failures,
        )
        .unwrap();
        assert_eq!(overlap.baseline_backlog_violation_bps, 10_000);
        assert_eq!(overlap.baseline_cost_violation_bps, 10_000);
        assert_eq!(overlap.baseline_sla_target_violation_bps, Some(10_000));
        assert_eq!(overlap.baseline_joint_constraint_violation_bps, 10_000);
        assert_eq!(overlap.alternative_joint_constraint_violation_bps, 10_000);
        assert_eq!(overlap.alternative_joint_constraint_recovery_bps, 0);
        let mut recoverable = plan.clone();
        recoverable.max_staff_cost_cents = 1_200_000;
        recoverable.min_sla_resolved = None;
        let recovery = simulate_empirical_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &alt,
            &fit,
            &recoverable,
        )
        .unwrap();
        assert_eq!(recovery.baseline_joint_constraint_violation_bps, 10_000);
        assert_eq!(recovery.alternative_joint_constraint_violation_bps, 0);
        assert_eq!(recovery.alternative_joint_constraint_recovery_bps, 10_000);
        let mut more = alt.clone();
        more.id = "four".into();
        more.agents_by_day.fill(4);
        let c = simulate_empirical_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &more,
            &fit,
            &plan,
        )
        .unwrap();
        assert_eq!(a.common_draws_sha256, c.common_draws_sha256);
        assert_ne!(a.replay_hash, c.replay_hash);
        let mut independent = plan.clone();
        independent.arrival_block_days = 1;
        let iid = simulate_empirical_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &alt,
            &fit,
            &independent,
        )
        .unwrap();
        assert_ne!(a.common_draws_sha256, iid.common_draws_sha256);
        assert_ne!(a.replay_hash, iid.replay_hash);
        let mut full_block = plan.clone();
        full_block.runs = 1;
        full_block.arrival_block_days = fit.training_days;
        let full_block_report = simulate_empirical_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &alt,
            &fit,
            &full_block,
        )
        .unwrap();
        let mut expected_draws = Sha256::new();
        for arrivals in fit.arrival_samples.iter().cycle().take(35) {
            expected_draws.update(arrivals.to_le_bytes());
        }
        expected_draws.update(8_u32.to_le_bytes());
        assert_eq!(
            full_block_report.common_draws_sha256,
            format!("{:x}", expected_draws.finalize())
        );
        independent.arrival_block_days = fit.training_days + 1;
        assert!(matches!(
            simulate_empirical_sensitivity(
                &pilot.snapshot,
                &model,
                &pilot.baseline,
                &alt,
                &fit,
                &independent
            ),
            Err(EmpiricalError::Invalid)
        ));
    }

    #[test]
    fn unidentified_capacity_requires_explicit_range() {
        let days = vec![
            ObservedSupportDay {
                arrivals: 1,
                backlog_start: 0,
                resolved: 1,
                backlog_end: 0,
                agents: 2,
                fixed_extra_capacity: 0,
            };
            14
        ];
        let fit = fit_empirical_parameters(&days, 14, 3).unwrap();
        assert_eq!(
            fit.capacity_identification,
            CapacityIdentification::Unidentified
        );
        assert!(fit.capacity_samples.is_empty());
        let snapshot = DecisionSnapshot {
            id: "s".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["source".into()],
            seed: 7,
            arrivals_by_day: vec![1; 14],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 1,
            sla_days: 1,
            staff_cost_cents_per_agent_day: 1,
        };
        let base = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1; 14],
            fixed_extra_capacity_by_day: vec![0; 14],
        };
        let alt = StaffingScenario {
            id: "alt".into(),
            agents_by_day: vec![2; 14],
            fixed_extra_capacity_by_day: vec![0; 14],
        };
        let mut plan = EmpiricalSensitivityPlan {
            runs: 20,
            arrival_block_days: 1,
            sampling_mode: EmpiricalSamplingMode::Independent,
            capacity_fallback_range: None,
            max_final_backlog: 2,
            max_staff_cost_cents: 50,
            min_sla_resolved: None,
        };
        assert!(matches!(
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &plan),
            Err(EmpiricalError::Unidentified)
        ));
        plan.capacity_fallback_range = Some(BoundedCount { min: 1, max: 3 });
        let report =
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &plan).unwrap();
        assert_eq!(report.capacity_source, "operator_range_uniform");
        assert_eq!(report.capacity_sample_count, 0);
        assert_eq!(report.unsaturated_lower_bound_day_count, 14);
        assert_eq!(report.max_unsaturated_capacity_lower_bound, 1);
        assert_eq!(report.min_sla_resolved, None);
        assert_eq!(report.baseline_sla_target_violation_bps, None);
        assert_eq!(report.alternative_sla_target_violation_bps, None);
    }

    #[test]
    fn partial_saturated_evidence_and_unsaturated_lower_bounds_constrain_operator_range() {
        let mut days = Vec::new();
        let mut backlog = 0_u64;
        for index in 0..7 {
            let (arrivals, resolved) = if index < 5 { (19, 19) } else { (20, 16) };
            let end = backlog + arrivals as u64 - resolved as u64;
            days.push(ObservedSupportDay {
                arrivals,
                backlog_start: backlog,
                resolved,
                backlog_end: end,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog = end;
        }
        let fit = fit_empirical_parameters(&days, 7, 3).unwrap();
        assert_eq!(
            fit.capacity_identification,
            CapacityIdentification::Unidentified
        );
        assert_eq!(fit.capacity_samples, vec![8, 8]);
        assert_eq!(fit.saturated_day_pairs.len(), 2);
        assert_eq!(fit.unsaturated_day_lower_bounds.len(), 5);
        assert!(
            fit.unsaturated_day_lower_bounds
                .iter()
                .all(|bound| bound.minimum_capacity_per_agent == 10)
        );
        let snapshot = DecisionSnapshot {
            id: "partial".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["source".into()],
            seed: 7,
            arrivals_by_day: days.iter().map(|day| day.arrivals).collect(),
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 1,
            staff_cost_cents_per_agent_day: 1,
        };
        let base = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        let alt = StaffingScenario {
            id: "alt".into(),
            agents_by_day: vec![2; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        let mut plan = EmpiricalSensitivityPlan {
            runs: 20,
            arrival_block_days: 1,
            sampling_mode: EmpiricalSamplingMode::Independent,
            capacity_fallback_range: Some(BoundedCount { min: 1, max: 9 }),
            max_final_backlog: 100,
            max_staff_cost_cents: 100,
            min_sla_resolved: None,
        };
        assert!(matches!(
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &plan,),
            Err(EmpiricalError::Invalid)
        ));
        plan.capacity_fallback_range = Some(BoundedCount { min: 9, max: 10 });
        assert!(matches!(
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &plan,),
            Err(EmpiricalError::Invalid)
        ));
        plan.capacity_fallback_range = Some(BoundedCount { min: 8, max: 10 });
        let report =
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &plan).unwrap();
        assert_eq!(report.capacity_source, "operator_range_uniform");
        assert_eq!(report.capacity_sample_count, 2);
        assert_eq!(report.unsaturated_lower_bound_day_count, 5);
        assert_eq!(report.max_unsaturated_capacity_lower_bound, 10);
        let sufficient_count_but_incomplete_support =
            fit_empirical_parameters(&days, 7, 2).unwrap();
        assert_eq!(
            sufficient_count_but_incomplete_support.capacity_samples,
            vec![8, 8]
        );
        assert_eq!(
            sufficient_count_but_incomplete_support.capacity_identification,
            CapacityIdentification::Unidentified
        );
        assert!(
            simulate_empirical_sensitivity(
                &snapshot,
                &model,
                &base,
                &alt,
                &sufficient_count_but_incomplete_support,
                &plan,
            )
            .is_ok()
        );
    }

    #[test]
    fn nondivisible_saturated_day_requires_a_nontrivial_operator_range() {
        let mut days = Vec::new();
        let mut backlog = 10_u64;
        for index in 0..7 {
            let resolved = if index == 3 { 15 } else { 16 };
            let end = backlog + 20 - resolved as u64;
            days.push(ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved,
                backlog_end: end,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog = end;
        }
        let fit = fit_empirical_parameters(&days, 7, 3).unwrap();
        assert_eq!(fit.capacity_samples, vec![8; 6]);
        assert_eq!(fit.saturated_day_pairs.len(), 6);
        assert_eq!(
            fit.partial_saturated_day_lower_bounds,
            vec![PartialSaturatedDayCapacityBound {
                training_day_index: 3,
                minimum_capacity_per_agent: 8,
                observed_staff_resolutions: 15,
                agents: 2,
            }]
        );
        assert_eq!(
            fit.capacity_identification,
            CapacityIdentification::Unidentified
        );
        let snapshot = DecisionSnapshot {
            id: "partial-service".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["source".into()],
            seed: 1,
            arrivals_by_day: vec![20; 7],
            initial_backlog: vec![crate::decision_sim::InitialCohort {
                age_days: 0,
                count: 10,
            }],
        };
        let model = QueueModel {
            version: "model".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let baseline = StaffingScenario {
            id: "baseline".into(),
            agents_by_day: vec![2; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        let alternative = StaffingScenario {
            id: "alternative".into(),
            agents_by_day: vec![3; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        let mut plan = EmpiricalSensitivityPlan {
            runs: 4,
            arrival_block_days: 1,
            sampling_mode: EmpiricalSamplingMode::Independent,
            capacity_fallback_range: None,
            max_final_backlog: 100,
            max_staff_cost_cents: 10_000,
            min_sla_resolved: None,
        };
        assert!(matches!(
            simulate_empirical_sensitivity(&snapshot, &model, &baseline, &alternative, &fit, &plan),
            Err(EmpiricalError::Unidentified)
        ));
        plan.capacity_fallback_range = Some(BoundedCount { min: 8, max: 8 });
        assert!(matches!(
            simulate_empirical_sensitivity(&snapshot, &model, &baseline, &alternative, &fit, &plan),
            Err(EmpiricalError::Invalid)
        ));
        plan.capacity_fallback_range = Some(BoundedCount { min: 7, max: 8 });
        let report =
            simulate_empirical_sensitivity(&snapshot, &model, &baseline, &alternative, &fit, &plan)
                .unwrap();
        assert_eq!(report.capacity_source, "operator_range_uniform");
        assert_eq!(report.partial_saturated_day_count, 1);
    }

    #[test]
    fn paired_saturated_days_preserve_joint_arrival_capacity_draws() {
        let mut days = Vec::new();
        let mut backlog = 0_u64;
        for index in 0..21 {
            let (arrivals, resolved) = if index % 2 == 0 { (12, 4) } else { (24, 8) };
            let end = backlog + arrivals as u64 - resolved as u64;
            days.push(ObservedSupportDay {
                arrivals,
                backlog_start: backlog,
                resolved,
                backlog_end: end,
                agents: 1,
                fixed_extra_capacity: 0,
            });
            backlog = end;
        }
        let fit = fit_empirical_parameters(&days, 14, 7).unwrap();
        assert_eq!(fit.saturated_day_pairs.len(), 14);
        assert!(
            fit.saturated_day_pairs.iter().all(|pair| {
                matches!((pair.arrivals, pair.capacity_per_agent), (12, 4) | (24, 8))
            })
        );
        let holdout = evaluate_joint_capacity_holdout(&days, 14, 7).unwrap();
        assert_eq!(holdout.scored_saturated_days, 7);
        assert_eq!(holdout.paired_absolute_error_sum, 0);
        assert!(holdout.independent_median_absolute_error_sum > 0);
        assert!(holdout.paired_beats_independent);
        let mut partial_holdout = days.clone();
        partial_holdout[14].agents = 2;
        partial_holdout[14].resolved = 5;
        partial_holdout[14].backlog_end -= 1;
        for day in &mut partial_holdout[15..] {
            day.backlog_start -= 1;
            day.backlog_end -= 1;
        }
        let partial_score = evaluate_joint_capacity_holdout(&partial_holdout, 14, 7).unwrap();
        assert_eq!(partial_score.scored_saturated_days, 6);
        assert_eq!(partial_score.skipped_partial_saturated_days, 1);
        assert_eq!(partial_score.skipped_unsaturated_days, 0);
        assert_eq!(partial_score.paired_absolute_error_sum, 0);
        let mut no_saturated_holdout = days.clone();
        let mut opening = no_saturated_holdout[13].backlog_end;
        for day in &mut no_saturated_holdout[14..] {
            day.arrivals = 1;
            day.backlog_start = opening;
            day.resolved = (opening + 1) as u32;
            day.backlog_end = 0;
            opening = 0;
        }
        assert!(matches!(
            evaluate_joint_capacity_holdout(&no_saturated_holdout, 14, 7),
            Err(EmpiricalError::NoHoldoutCapacity)
        ));
        let snapshot = DecisionSnapshot {
            id: "joint".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["joint-source".into()],
            seed: 19,
            arrivals_by_day: fit.arrival_samples.clone(),
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 6,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 1,
        };
        let base = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1; 14],
            fixed_extra_capacity_by_day: vec![0; 14],
        };
        let alt = StaffingScenario {
            id: "alt".into(),
            agents_by_day: vec![2; 14],
            fixed_extra_capacity_by_day: vec![0; 14],
        };
        let plan = EmpiricalSensitivityPlan {
            runs: 1,
            arrival_block_days: 1,
            sampling_mode: EmpiricalSamplingMode::PairedSaturatedDays,
            capacity_fallback_range: None,
            max_final_backlog: 200,
            max_staff_cost_cents: 100,
            min_sla_resolved: None,
        };
        let report =
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &plan).unwrap();
        assert_eq!(report.capacity_source, "saturated_day_paired_empirical");
        assert_eq!(report.paired_saturated_day_count, 14);
        let mut rng = DrawStream(snapshot.seed);
        let mut expected_digest = Sha256::new();
        for _ in 0..14 {
            let pair = sample(&mut rng, &fit.saturated_day_pairs).unwrap();
            expected_digest.update(pair.arrivals.to_le_bytes());
            expected_digest.update(pair.capacity_per_agent.to_le_bytes());
        }
        assert_eq!(
            report.common_draws_sha256,
            format!("{:x}", expected_digest.finalize())
        );
        assert_eq!(
            report,
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &plan).unwrap()
        );
        let mut more_agents = alt.clone();
        more_agents.id = "more-agents".into();
        more_agents.agents_by_day.fill(3);
        let changed_policy =
            simulate_empirical_sensitivity(&snapshot, &model, &base, &more_agents, &fit, &plan)
                .unwrap();
        assert_eq!(
            report.common_draws_sha256,
            changed_policy.common_draws_sha256
        );
        assert_ne!(report.replay_hash, changed_policy.replay_hash);
        let mut full_block = plan.clone();
        full_block.arrival_block_days = fit.training_days;
        let block_report =
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &full_block)
                .unwrap();
        assert_eq!(
            block_report.capacity_source,
            "saturated_day_paired_block_empirical"
        );
        let mut block_digest = Sha256::new();
        for pair in &fit.saturated_day_pairs {
            block_digest.update(pair.arrivals.to_le_bytes());
            block_digest.update(pair.capacity_per_agent.to_le_bytes());
        }
        assert_eq!(
            block_report.common_draws_sha256,
            format!("{:x}", block_digest.finalize())
        );
        assert_eq!(
            block_report,
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &full_block)
                .unwrap()
        );
        let mut tail_snapshot = snapshot.clone();
        tail_snapshot.arrivals_by_day.push(0);
        let mut tail_base = base.clone();
        tail_base.agents_by_day.push(1);
        tail_base.fixed_extra_capacity_by_day.push(0);
        let mut tail_alt = alt.clone();
        tail_alt.agents_by_day.push(2);
        tail_alt.fixed_extra_capacity_by_day.push(0);
        let tail_report = simulate_empirical_sensitivity(
            &tail_snapshot,
            &model,
            &tail_base,
            &tail_alt,
            &fit,
            &full_block,
        )
        .unwrap();
        let mut tail_digest = Sha256::new();
        for pair in fit
            .saturated_day_pairs
            .iter()
            .chain(fit.saturated_day_pairs.first())
        {
            tail_digest.update(pair.arrivals.to_le_bytes());
            tail_digest.update(pair.capacity_per_agent.to_le_bytes());
        }
        assert_eq!(
            tail_report.common_draws_sha256,
            format!("{:x}", tail_digest.finalize())
        );
        let mut interrupted_days = Vec::new();
        let mut opening = 0_u64;
        for index in 0..14 {
            let agents = if index == 7 { 100 } else { 1 };
            let resolved = if index == 7 { opening as u32 + 12 } else { 4 };
            let end = opening + 12 - resolved as u64;
            interrupted_days.push(ObservedSupportDay {
                arrivals: 12,
                backlog_start: opening,
                resolved,
                backlog_end: end,
                agents,
                fixed_extra_capacity: 0,
            });
            opening = end;
        }
        let interrupted_fit = fit_empirical_parameters(&interrupted_days, 14, 7).unwrap();
        assert_eq!(
            interrupted_fit.capacity_identification,
            CapacityIdentification::EmpiricalSaturatedDays
        );
        let mut interrupted_snapshot = snapshot.clone();
        interrupted_snapshot.arrivals_by_day = interrupted_fit.arrival_samples.clone();
        let mut unavailable_block = plan.clone();
        unavailable_block.arrival_block_days = 8;
        assert!(matches!(
            simulate_empirical_sensitivity(
                &interrupted_snapshot,
                &model,
                &base,
                &alt,
                &interrupted_fit,
                &unavailable_block
            ),
            Err(EmpiricalError::Unidentified)
        ));
        let mut invalid = plan.clone();
        invalid.arrival_block_days = fit.training_days + 1;
        assert!(matches!(
            simulate_empirical_sensitivity(&snapshot, &model, &base, &alt, &fit, &invalid),
            Err(EmpiricalError::Invalid)
        ));
    }
}
