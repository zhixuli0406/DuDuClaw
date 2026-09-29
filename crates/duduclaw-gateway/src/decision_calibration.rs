//! Conservative capacity fitting and rolling one-step backlog backtests.
//!
//! Only days ending with backlog can identify saturated service capacity.
//! This does not infer a causal staffing effect, demand response, or a
//! calibrated uncertainty interval.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bind stored validation reports to the implementation that produced them.
pub fn calibration_engine_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_str!("decision_calibration.rs").as_bytes())
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedSupportDay {
    pub arrivals: u32,
    pub backlog_start: u64,
    pub resolved: u32,
    pub backlog_end: u64,
    pub agents: u32,
    pub fixed_extra_capacity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityFit {
    pub service_per_agent_day: u32,
    /// Observed range among saturated training days; this is not a confidence interval.
    pub observed_min: u32,
    pub observed_max: u32,
    pub saturated_days: usize,
    pub training_days: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestResult {
    pub evaluation_days: usize,
    /// Divide each numerator by evaluation_days to obtain mean absolute error.
    pub model_abs_error_sum: u128,
    pub no_change_abs_error_sum: u128,
    pub seasonal_naive_abs_error_sum: u128,
    pub model_beats_both_baselines: bool,
    pub last_fit: CapacityFit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForecastPoint {
    pub day_index: usize,
    pub predicted_arrivals: u32,
    pub actual_arrivals: u32,
    pub predicted_backlog_end: u64,
    pub actual_backlog_end: u64,
    pub no_change_backlog_end: u64,
    pub seasonal_naive_backlog_end: u64,
    pub mean_change_backlog_end: u64,
    pub fitted_capacity_per_agent: u32,
}

/// One-step forecast using only earlier arrivals and outcomes. The current
/// staffing schedule and opening backlog are treated as available at forecast
/// time; this is not a multi-day demand forecast or an intervention estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForecastBacktestResult {
    pub evaluation_days: usize,
    pub arrival_abs_error_sum: u128,
    pub model_abs_error_sum: u128,
    pub no_change_abs_error_sum: u128,
    pub seasonal_naive_abs_error_sum: u128,
    pub mean_change_abs_error_sum: u128,
    pub model_beats_all_baselines: bool,
    pub points: Vec<ForecastPoint>,
}

/// Inputs available before the target day begins. Outcome and arrivals are
/// deliberately absent so a shadow prediction cannot read the target row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownDayInputs {
    pub opening_backlog: u64,
    pub planned_agents: u32,
    pub planned_fixed_extra_capacity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProspectiveForecast {
    pub predicted_arrivals: u32,
    pub predicted_backlog_end: u64,
    pub no_change_backlog_end: u64,
    pub seasonal_naive_backlog_end: u64,
    pub mean_change_backlog_end: u64,
    pub fitted_capacity_per_agent: u32,
    pub training_days: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalDiagnosticPoint {
    pub day_index: usize,
    pub predicted_backlog_end: u64,
    pub actual_backlog_end: u64,
    pub lower_bound: u64,
    pub upper_bound: u64,
    pub calibration_radius: u64,
    pub covered: bool,
}

/// Rolling residual-band diagnostic, not a calibrated production interval.
/// The sliding window and potentially changing demand do not establish
/// exchangeability or a coverage guarantee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalDiagnostic {
    pub method: String,
    pub target_coverage_basis_points: u16,
    pub observed_coverage_basis_points: u16,
    pub calibration_points: usize,
    pub evaluated_points: usize,
    pub recent_miss_count: usize,
    pub recent_window_points: usize,
    pub drift_signal: bool,
    pub points: Vec<IntervalDiagnosticPoint>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CalibrationError {
    #[error("observations violate backlog conservation or capacity bounds")]
    InvalidObservation,
    #[error("too few identifiable saturated training days")]
    Unidentified,
    #[error("too few days for a rolling seasonal backtest")]
    InsufficientHistory,
    #[error("calibration arithmetic overflow")]
    Overflow,
}

pub(crate) fn validate(days: &[ObservedSupportDay]) -> Result<(), CalibrationError> {
    if days.len() > 3_660 {
        return Err(CalibrationError::InvalidObservation);
    }
    for (index, day) in days.iter().enumerate() {
        let available = day
            .backlog_start
            .checked_add(day.arrivals as u64)
            .ok_or(CalibrationError::Overflow)?;
        let accounted = day
            .backlog_end
            .checked_add(day.resolved as u64)
            .ok_or(CalibrationError::Overflow)?;
        // With no staff, fixed extra capacity is the only possible service
        // source. Conservation alone would accept invented resolutions.
        if available != accounted
            || (day.agents == 0 && day.resolved > day.fixed_extra_capacity)
            || (index > 0 && days[index - 1].backlog_end != day.backlog_start)
        {
            return Err(CalibrationError::InvalidObservation);
        }
    }
    Ok(())
}

/// Exact per-agent integer service observations from backlog-positive days.
/// A nondivisible staff resolution count is model mismatch, not a rounded
/// observation of an integer capacity.
pub(crate) fn saturated_capacity_samples(
    days: &[ObservedSupportDay],
) -> Result<Vec<u32>, CalibrationError> {
    validate(days)?;
    let mut samples = Vec::new();
    for day in days {
        if day.backlog_end == 0 || day.agents == 0 {
            continue;
        }
        if day.resolved < day.fixed_extra_capacity {
            return Err(CalibrationError::InvalidObservation);
        }
        let staff_resolved = day.resolved - day.fixed_extra_capacity;
        if staff_resolved % day.agents == 0 {
            samples.push(staff_resolved / day.agents);
        }
    }
    Ok(samples)
}

/// Fit an integer capacity using the median of saturated-day observations.
/// Days with zero final backlog cannot reveal unused service capacity.
pub fn fit_capacity(
    days: &[ObservedSupportDay],
    min_saturated_days: usize,
) -> Result<CapacityFit, CalibrationError> {
    let mut samples = saturated_capacity_samples(days)?;
    if days.iter().any(|day| {
        day.backlog_end > 0
            && day.agents > 0
            && (day.resolved - day.fixed_extra_capacity) % day.agents != 0
    }) {
        return Err(CalibrationError::Unidentified);
    }
    if samples.len() < min_saturated_days.max(1) {
        return Err(CalibrationError::Unidentified);
    }
    samples.sort_unstable();
    Ok(CapacityFit {
        service_per_agent_day: samples[samples.len() / 2],
        observed_min: samples[0],
        observed_max: samples[samples.len() - 1],
        saturated_days: samples.len(),
        training_days: days.len(),
    })
}

fn abs_error(predicted: u64, actual: u64) -> u128 {
    predicted.abs_diff(actual) as u128
}

/// Produce an as-of forecast from an observed prefix and scheduled resources.
/// The caller must commit the returned forecast before revealing target-day
/// arrivals or outcomes in an actual shadow workflow.
pub fn forecast_next_day(
    training: &[ObservedSupportDay],
    known: &KnownDayInputs,
    min_saturated_days: usize,
) -> Result<ProspectiveForecast, CalibrationError> {
    if training.len() < 7 {
        return Err(CalibrationError::InsufficientHistory);
    }
    validate(training)?;
    if training
        .last()
        .is_none_or(|day| day.backlog_end != known.opening_backlog)
    {
        return Err(CalibrationError::InvalidObservation);
    }
    let fit = fit_capacity(training, min_saturated_days)?;
    let prior = &training[training.len() - 7..];
    let arrival_total: u64 = prior.iter().map(|day| day.arrivals as u64).sum();
    let predicted_arrivals =
        u32::try_from((arrival_total + 3) / 7).map_err(|_| CalibrationError::Overflow)?;
    let available = known
        .opening_backlog
        .checked_add(predicted_arrivals as u64)
        .ok_or(CalibrationError::Overflow)?;
    let capacity = (known.planned_agents as u64)
        .checked_mul(fit.service_per_agent_day as u64)
        .and_then(|value| value.checked_add(known.planned_fixed_extra_capacity as u64))
        .ok_or(CalibrationError::Overflow)?;
    let mean_change = prior
        .iter()
        .map(|day| day.backlog_end as i128 - day.backlog_start as i128)
        .sum::<i128>()
        / 7;
    let mean_prediction = u64::try_from((known.opening_backlog as i128 + mean_change).max(0))
        .map_err(|_| CalibrationError::Overflow)?;
    Ok(ProspectiveForecast {
        predicted_arrivals,
        predicted_backlog_end: available - capacity.min(available),
        no_change_backlog_end: known.opening_backlog,
        seasonal_naive_backlog_end: prior[0].backlog_end,
        mean_change_backlog_end: mean_prediction,
        fitted_capacity_per_agent: fit.service_per_agent_day,
        training_days: training.len(),
    })
}

/// Rolling one-step predictions use only training days before the target day.
/// Actual arrivals and staffing are supplied as known external inputs for this
/// diagnostic. A later forecast must also predict those inputs separately.
pub fn backtest_capacity(
    days: &[ObservedSupportDay],
    min_training_days: usize,
    min_saturated_days: usize,
) -> Result<BacktestResult, CalibrationError> {
    validate(days)?;
    let start = min_training_days.max(7);
    if days.len() <= start {
        return Err(CalibrationError::InsufficientHistory);
    }
    let mut model_sum = 0_u128;
    let mut no_change_sum = 0_u128;
    let mut seasonal_sum = 0_u128;
    let mut last_fit = None;
    for index in start..days.len() {
        let fit = fit_capacity(&days[..index], min_saturated_days)?;
        let day = &days[index];
        let available = day
            .backlog_start
            .checked_add(day.arrivals as u64)
            .ok_or(CalibrationError::Overflow)?;
        let capacity = (day.agents as u64)
            .checked_mul(fit.service_per_agent_day as u64)
            .and_then(|value| value.checked_add(day.fixed_extra_capacity as u64))
            .ok_or(CalibrationError::Overflow)?;
        let predicted = available - capacity.min(available);
        model_sum = model_sum
            .checked_add(abs_error(predicted, day.backlog_end))
            .ok_or(CalibrationError::Overflow)?;
        no_change_sum = no_change_sum
            .checked_add(abs_error(day.backlog_start, day.backlog_end))
            .ok_or(CalibrationError::Overflow)?;
        seasonal_sum = seasonal_sum
            .checked_add(abs_error(days[index - 7].backlog_end, day.backlog_end))
            .ok_or(CalibrationError::Overflow)?;
        last_fit = Some(fit);
    }
    Ok(BacktestResult {
        evaluation_days: days.len() - start,
        model_abs_error_sum: model_sum,
        no_change_abs_error_sum: no_change_sum,
        seasonal_naive_abs_error_sum: seasonal_sum,
        model_beats_both_baselines: model_sum < no_change_sum && model_sum < seasonal_sum,
        last_fit: last_fit.ok_or(CalibrationError::InsufficientHistory)?,
    })
}

/// Rolling prequential diagnostic. For target day t, the demand forecast is
/// the rounded mean of arrivals on t-7..t-1; capacity is fitted on days < t.
/// A seven-day mean of observed backlog changes is the statistical baseline.
pub fn backtest_one_step_forecast(
    days: &[ObservedSupportDay],
    min_training_days: usize,
    min_saturated_days: usize,
) -> Result<ForecastBacktestResult, CalibrationError> {
    validate(days)?;
    let start = min_training_days.max(7);
    if days.len() <= start {
        return Err(CalibrationError::InsufficientHistory);
    }
    let mut points = Vec::with_capacity(days.len() - start);
    let mut arrival_sum = 0_u128;
    let mut model_sum = 0_u128;
    let mut no_change_sum = 0_u128;
    let mut seasonal_sum = 0_u128;
    let mut mean_change_sum = 0_u128;
    for index in start..days.len() {
        let day = &days[index];
        let forecast = forecast_next_day(
            &days[..index],
            &KnownDayInputs {
                opening_backlog: day.backlog_start,
                planned_agents: day.agents,
                planned_fixed_extra_capacity: day.fixed_extra_capacity,
            },
            min_saturated_days,
        )?;
        arrival_sum = arrival_sum
            .checked_add(forecast.predicted_arrivals.abs_diff(day.arrivals) as u128)
            .ok_or(CalibrationError::Overflow)?;
        model_sum = model_sum
            .checked_add(abs_error(forecast.predicted_backlog_end, day.backlog_end))
            .ok_or(CalibrationError::Overflow)?;
        no_change_sum = no_change_sum
            .checked_add(abs_error(forecast.no_change_backlog_end, day.backlog_end))
            .ok_or(CalibrationError::Overflow)?;
        seasonal_sum = seasonal_sum
            .checked_add(abs_error(
                forecast.seasonal_naive_backlog_end,
                day.backlog_end,
            ))
            .ok_or(CalibrationError::Overflow)?;
        mean_change_sum = mean_change_sum
            .checked_add(abs_error(forecast.mean_change_backlog_end, day.backlog_end))
            .ok_or(CalibrationError::Overflow)?;
        points.push(ForecastPoint {
            day_index: index,
            predicted_arrivals: forecast.predicted_arrivals,
            actual_arrivals: day.arrivals,
            predicted_backlog_end: forecast.predicted_backlog_end,
            actual_backlog_end: day.backlog_end,
            no_change_backlog_end: forecast.no_change_backlog_end,
            seasonal_naive_backlog_end: forecast.seasonal_naive_backlog_end,
            mean_change_backlog_end: forecast.mean_change_backlog_end,
            fitted_capacity_per_agent: forecast.fitted_capacity_per_agent,
        });
    }
    Ok(ForecastBacktestResult {
        evaluation_days: points.len(),
        arrival_abs_error_sum: arrival_sum,
        model_abs_error_sum: model_sum,
        no_change_abs_error_sum: no_change_sum,
        seasonal_naive_abs_error_sum: seasonal_sum,
        mean_change_abs_error_sum: mean_change_sum,
        model_beats_all_baselines: model_sum < no_change_sum
            && model_sum < seasonal_sum
            && model_sum < mean_change_sum,
        points,
    })
}

/// At each target point, use only the preceding `calibration_points` absolute
/// forecast residuals. The 90% finite-sample rank is ceil(0.9 * (n+1));
/// rolling windows make the result a diagnostic rather than a coverage claim.
pub fn diagnose_forecast_intervals(
    forecast: &ForecastBacktestResult,
    calibration_points: usize,
) -> Result<IntervalDiagnostic, CalibrationError> {
    if calibration_points < 14 || forecast.points.len() <= calibration_points {
        return Err(CalibrationError::InsufficientHistory);
    }
    if forecast
        .points
        .windows(2)
        .any(|pair| pair[0].day_index.checked_add(1) != Some(pair[1].day_index))
    {
        return Err(CalibrationError::InvalidObservation);
    }
    let rank = ((calibration_points + 1) * 9).div_ceil(10);
    let mut points = Vec::with_capacity(forecast.points.len() - calibration_points);
    for index in calibration_points..forecast.points.len() {
        let mut residuals: Vec<u64> = forecast.points[index - calibration_points..index]
            .iter()
            .map(|point| {
                point
                    .predicted_backlog_end
                    .abs_diff(point.actual_backlog_end)
            })
            .collect();
        residuals.sort_unstable();
        let radius = residuals[rank - 1];
        let point = &forecast.points[index];
        let lower_bound = point.predicted_backlog_end.saturating_sub(radius);
        let upper_bound = point
            .predicted_backlog_end
            .checked_add(radius)
            .ok_or(CalibrationError::Overflow)?;
        points.push(IntervalDiagnosticPoint {
            day_index: point.day_index,
            predicted_backlog_end: point.predicted_backlog_end,
            actual_backlog_end: point.actual_backlog_end,
            lower_bound,
            upper_bound,
            calibration_radius: radius,
            covered: (lower_bound..=upper_bound).contains(&point.actual_backlog_end),
        });
    }
    let covered = points.iter().filter(|point| point.covered).count();
    let recent_window_points = points.len().min(7);
    let recent_miss_count = points
        .iter()
        .rev()
        .take(recent_window_points)
        .filter(|point| !point.covered)
        .count();
    Ok(IntervalDiagnostic {
        method: "rolling_absolute_residual_rank90_v1".into(),
        target_coverage_basis_points: 9_000,
        observed_coverage_basis_points: u16::try_from(covered * 10_000 / points.len())
            .map_err(|_| CalibrationError::Overflow)?,
        calibration_points,
        evaluated_points: points.len(),
        recent_miss_count,
        recent_window_points,
        drift_signal: recent_window_points == 7 && recent_miss_count >= 3,
        points,
    })
}

/// Calibrate once on the earliest forecast residuals, then score only later
/// points. This is a forward holdout diagnostic on the supplied series, not
/// evidence of coverage on a future or real customer population.
pub fn diagnose_fixed_forecast_intervals(
    forecast: &ForecastBacktestResult,
    calibration_points: usize,
) -> Result<IntervalDiagnostic, CalibrationError> {
    if calibration_points < 14 || forecast.points.len() <= calibration_points {
        return Err(CalibrationError::InsufficientHistory);
    }
    if forecast
        .points
        .windows(2)
        .any(|pair| pair[0].day_index.checked_add(1) != Some(pair[1].day_index))
    {
        return Err(CalibrationError::InvalidObservation);
    }
    let mut residuals: Vec<u64> = forecast.points[..calibration_points]
        .iter()
        .map(|point| {
            point
                .predicted_backlog_end
                .abs_diff(point.actual_backlog_end)
        })
        .collect();
    residuals.sort_unstable();
    let rank = ((calibration_points + 1) * 9).div_ceil(10);
    let radius = residuals[rank - 1];
    let mut points = Vec::with_capacity(forecast.points.len() - calibration_points);
    for point in &forecast.points[calibration_points..] {
        let lower_bound = point.predicted_backlog_end.saturating_sub(radius);
        let upper_bound = point
            .predicted_backlog_end
            .checked_add(radius)
            .ok_or(CalibrationError::Overflow)?;
        points.push(IntervalDiagnosticPoint {
            day_index: point.day_index,
            predicted_backlog_end: point.predicted_backlog_end,
            actual_backlog_end: point.actual_backlog_end,
            lower_bound,
            upper_bound,
            calibration_radius: radius,
            covered: (lower_bound..=upper_bound).contains(&point.actual_backlog_end),
        });
    }
    let covered = points.iter().filter(|point| point.covered).count();
    let recent_window_points = points.len().min(7);
    let recent_miss_count = points
        .iter()
        .rev()
        .take(recent_window_points)
        .filter(|point| !point.covered)
        .count();
    Ok(IntervalDiagnostic {
        method: "fixed_prefix_absolute_residual_rank90_v1".into(),
        target_coverage_basis_points: 9_000,
        observed_coverage_basis_points: u16::try_from(covered * 10_000 / points.len())
            .map_err(|_| CalibrationError::Overflow)?,
        calibration_points,
        evaluated_points: points.len(),
        recent_miss_count,
        recent_window_points,
        drift_signal: recent_window_points == 7 && recent_miss_count >= 3,
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_backtest_uses_only_prior_saturated_days() {
        let mut days = Vec::new();
        let mut backlog = 10_u64;
        for _ in 0..30 {
            let end = backlog + 20 - 16;
            days.push(ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: end,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog = end;
        }
        let result = backtest_capacity(&days, 14, 3).unwrap();
        assert_eq!(result.last_fit.service_per_agent_day, 8);
        assert_eq!(result.model_abs_error_sum, 0);
        assert!(result.model_beats_both_baselines);
        assert_eq!(result.evaluation_days, 16);
    }

    #[test]
    fn unsaturated_or_inconsistent_data_cannot_claim_a_fit() {
        let empty_backlog = vec![
            ObservedSupportDay {
                arrivals: 1,
                backlog_start: 0,
                resolved: 1,
                backlog_end: 0,
                agents: 2,
                fixed_extra_capacity: 0
            };
            10
        ];
        assert_eq!(
            fit_capacity(&empty_backlog, 3),
            Err(CalibrationError::Unidentified)
        );
        let bad = vec![ObservedSupportDay {
            backlog_end: 2,
            ..empty_backlog[0].clone()
        }];
        assert_eq!(
            fit_capacity(&bad, 1),
            Err(CalibrationError::InvalidObservation)
        );
    }

    #[test]
    fn zero_staff_cannot_resolve_more_than_fixed_extra_capacity() {
        let impossible = ObservedSupportDay {
            arrivals: 5,
            backlog_start: 3,
            resolved: 3,
            backlog_end: 5,
            agents: 0,
            fixed_extra_capacity: 2,
        };
        assert_eq!(
            validate(std::slice::from_ref(&impossible)),
            Err(CalibrationError::InvalidObservation)
        );
        let feasible = ObservedSupportDay {
            resolved: 2,
            backlog_end: 6,
            ..impossible
        };
        assert_eq!(validate(&[feasible]), Ok(()));
    }

    #[test]
    fn nondivisible_saturated_service_is_not_rounded_into_a_capacity_fit() {
        let day = ObservedSupportDay {
            arrivals: 20,
            backlog_start: 10,
            resolved: 15,
            backlog_end: 15,
            agents: 2,
            fixed_extra_capacity: 0,
        };
        assert!(
            saturated_capacity_samples(std::slice::from_ref(&day))
                .unwrap()
                .is_empty()
        );
        assert_eq!(fit_capacity(&[day], 1), Err(CalibrationError::Unidentified));
    }

    #[test]
    fn forecast_uses_only_prior_arrivals_and_has_statistical_baseline() {
        let mut days = Vec::new();
        let mut backlog = 10_u64;
        for index in 0..30 {
            let arrivals = 18 + (index % 5) as u32;
            let end = backlog + arrivals as u64 - 16;
            days.push(ObservedSupportDay {
                arrivals,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: end,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog = end;
        }
        let result = backtest_one_step_forecast(&days, 14, 7).unwrap();
        assert_eq!(result.evaluation_days, 16);
        assert_eq!(result.points[0].predicted_arrivals, 20);
        assert_eq!(result.points[0].fitted_capacity_per_agent, 8);
        let known = KnownDayInputs {
            opening_backlog: days[14].backlog_start,
            planned_agents: days[14].agents,
            planned_fixed_extra_capacity: days[14].fixed_extra_capacity,
        };
        let prospective = forecast_next_day(&days[..14], &known, 7).unwrap();
        assert_eq!(prospective.training_days, 14);
        assert_eq!(
            prospective.predicted_arrivals,
            result.points[0].predicted_arrivals
        );
        assert_eq!(
            prospective.predicted_backlog_end,
            result.points[0].predicted_backlog_end
        );
        assert_eq!(
            prospective.seasonal_naive_backlog_end,
            result.points[0].seasonal_naive_backlog_end
        );
        assert_eq!(
            forecast_next_day(
                &days[..14],
                &KnownDayInputs {
                    opening_backlog: known.opening_backlog + 1,
                    ..known.clone()
                },
                7
            ),
            Err(CalibrationError::InvalidObservation)
        );
        assert!(result.arrival_abs_error_sum > 0);
        assert_eq!(
            result.model_abs_error_sum,
            result
                .points
                .iter()
                .map(|point| abs_error(point.predicted_backlog_end, point.actual_backlog_end))
                .sum()
        );
        assert_eq!(
            result.model_beats_all_baselines,
            result.model_abs_error_sum < result.no_change_abs_error_sum
                && result.model_abs_error_sum < result.seasonal_naive_abs_error_sum
                && result.model_abs_error_sum < result.mean_change_abs_error_sum
        );

        // Changing the target day's arrivals/outcome cannot affect its forecast.
        let mut altered = days.clone();
        altered[14].arrivals += 4;
        altered[14].backlog_end += 4;
        altered[15].backlog_start += 4;
        altered[15].backlog_end += 4;
        for day in &mut altered[16..] {
            day.backlog_start += 4;
            day.backlog_end += 4;
        }
        let changed = backtest_one_step_forecast(&altered, 14, 7).unwrap();
        assert_eq!(
            result.points[0].predicted_arrivals,
            changed.points[0].predicted_arrivals
        );
        assert_eq!(
            result.points[0].predicted_backlog_end,
            changed.points[0].predicted_backlog_end
        );
    }

    #[test]
    fn interval_diagnostic_uses_only_past_residuals_and_exposes_drift() {
        let points: Vec<_> = (0..25)
            .map(|index| ForecastPoint {
                day_index: index + 14,
                predicted_arrivals: 20,
                actual_arrivals: 20,
                predicted_backlog_end: 100,
                actual_backlog_end: 100 + index as u64,
                no_change_backlog_end: 100,
                seasonal_naive_backlog_end: 100,
                mean_change_backlog_end: 100,
                fitted_capacity_per_agent: 8,
            })
            .collect();
        let forecast = ForecastBacktestResult {
            evaluation_days: points.len(),
            arrival_abs_error_sum: 0,
            model_abs_error_sum: 0,
            no_change_abs_error_sum: 0,
            seasonal_naive_abs_error_sum: 0,
            mean_change_abs_error_sum: 0,
            model_beats_all_baselines: false,
            points,
        };
        let diagnostic = diagnose_forecast_intervals(&forecast, 14).unwrap();
        assert_eq!(diagnostic.evaluated_points, 11);
        assert_eq!(diagnostic.points[0].calibration_radius, 13);
        assert!(!diagnostic.points[0].covered);
        assert!(diagnostic.drift_signal);
        assert_eq!(diagnostic.recent_miss_count, 7);

        let mut altered = forecast.clone();
        altered.points[14].actual_backlog_end = 1_000;
        let changed = diagnose_forecast_intervals(&altered, 14).unwrap();
        assert_eq!(
            diagnostic.points[0].lower_bound,
            changed.points[0].lower_bound
        );
        assert_eq!(
            diagnostic.points[0].upper_bound,
            changed.points[0].upper_bound
        );
        assert_eq!(
            diagnose_forecast_intervals(&forecast, 26),
            Err(CalibrationError::InsufficientHistory)
        );
    }

    #[test]
    fn fixed_prefix_interval_exposes_later_shift_without_recalibrating_on_holdout() {
        let points: Vec<_> = (0..28)
            .map(|index| ForecastPoint {
                day_index: index + 14,
                predicted_arrivals: 20,
                actual_arrivals: 20,
                predicted_backlog_end: 100,
                actual_backlog_end: if index < 21 { 101 } else { 110 },
                no_change_backlog_end: 100,
                seasonal_naive_backlog_end: 100,
                mean_change_backlog_end: 100,
                fitted_capacity_per_agent: 8,
            })
            .collect();
        let forecast = ForecastBacktestResult {
            evaluation_days: points.len(),
            arrival_abs_error_sum: 0,
            model_abs_error_sum: 0,
            no_change_abs_error_sum: 0,
            seasonal_naive_abs_error_sum: 0,
            mean_change_abs_error_sum: 0,
            model_beats_all_baselines: false,
            points,
        };
        let fixed = diagnose_fixed_forecast_intervals(&forecast, 14).unwrap();
        let rolling = diagnose_forecast_intervals(&forecast, 14).unwrap();
        assert_eq!(fixed.evaluated_points, 14);
        assert_eq!(fixed.observed_coverage_basis_points, 5_000);
        assert!(
            fixed
                .points
                .iter()
                .all(|point| point.calibration_radius == 1)
        );
        assert_eq!(fixed.recent_miss_count, 7);
        assert!(fixed.drift_signal);
        assert_eq!(rolling.recent_miss_count, 1);
        assert!(!rolling.drift_signal);

        let mut altered = forecast.clone();
        altered.points[14].actual_backlog_end = 1_000;
        let unchanged_radius = diagnose_fixed_forecast_intervals(&altered, 14).unwrap();
        assert_eq!(unchanged_radius.points[0].calibration_radius, 1);
        assert!(
            unchanged_radius
                .points
                .iter()
                .all(|point| point.calibration_radius == 1)
        );
        assert_eq!(
            diagnose_fixed_forecast_intervals(&forecast, 28),
            Err(CalibrationError::InsufficientHistory)
        );
    }
}
