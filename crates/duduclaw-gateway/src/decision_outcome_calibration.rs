//! Time-split capacity diagnostics from post-run support observations.
//!
//! Held-out arrivals and opening backlog are observed inputs. The resulting
//! errors isolate the service-capacity assumption; they are not live demand
//! forecast errors or causal intervention effects.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_calibration::{
    CalibrationError, CapacityFit, ObservedSupportDay, fit_capacity, validate,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityHoldoutDiagnostic {
    pub training_days: usize,
    pub holdout_days: usize,
    /// Absolute ending-backlog errors; divide by holdout_days for MAE.
    pub candidate_abs_error_sum: u128,
    pub parent_abs_error_sum: u128,
    pub no_change_abs_error_sum: u128,
    pub candidate_beats_parent: bool,
    pub candidate_beats_no_change: bool,
}

pub fn engine_sha256() -> String {
    let mut digest = Sha256::new();
    digest.update(include_str!("decision_calibration.rs").as_bytes());
    digest.update(include_str!("decision_outcome_calibration.rs").as_bytes());
    format!("{:x}", digest.finalize())
}

pub fn fit_and_score(
    days: &[ObservedSupportDay],
    parent_capacity_per_agent_day: u32,
    training_days: usize,
    min_saturated_days: usize,
) -> Result<(CapacityFit, CapacityHoldoutDiagnostic), CalibrationError> {
    if parent_capacity_per_agent_day == 0
        || min_saturated_days < 3
        || training_days < min_saturated_days
        || days.len().saturating_sub(training_days) < 3
    {
        return Err(CalibrationError::InsufficientHistory);
    }
    validate(days)?;
    let fit = fit_capacity(&days[..training_days], min_saturated_days)?;
    if fit.service_per_agent_day == 0 {
        return Err(CalibrationError::Unidentified);
    }
    let mut candidate_abs_error_sum = 0_u128;
    let mut parent_abs_error_sum = 0_u128;
    let mut no_change_abs_error_sum = 0_u128;
    for day in &days[training_days..] {
        let available = day
            .backlog_start
            .checked_add(day.arrivals as u64)
            .ok_or(CalibrationError::Overflow)?;
        let predicted = |per_agent: u32| -> Result<u64, CalibrationError> {
            let capacity = (day.agents as u64)
                .checked_mul(per_agent as u64)
                .and_then(|n| n.checked_add(day.fixed_extra_capacity as u64))
                .ok_or(CalibrationError::Overflow)?;
            Ok(available - capacity.min(available))
        };
        candidate_abs_error_sum = candidate_abs_error_sum
            .checked_add(predicted(fit.service_per_agent_day)?.abs_diff(day.backlog_end) as u128)
            .ok_or(CalibrationError::Overflow)?;
        parent_abs_error_sum = parent_abs_error_sum
            .checked_add(
                predicted(parent_capacity_per_agent_day)?.abs_diff(day.backlog_end) as u128,
            )
            .ok_or(CalibrationError::Overflow)?;
        no_change_abs_error_sum = no_change_abs_error_sum
            .checked_add(day.backlog_start.abs_diff(day.backlog_end) as u128)
            .ok_or(CalibrationError::Overflow)?;
    }
    Ok((
        fit,
        CapacityHoldoutDiagnostic {
            training_days,
            holdout_days: days.len() - training_days,
            candidate_abs_error_sum,
            parent_abs_error_sum,
            no_change_abs_error_sum,
            candidate_beats_parent: candidate_abs_error_sum < parent_abs_error_sum,
            candidate_beats_no_change: candidate_abs_error_sum < no_change_abs_error_sum,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_out_days_change_score_without_changing_training_fit() {
        let mut days: Vec<_> = (0..7)
            .map(|day| ObservedSupportDay {
                arrivals: 5,
                backlog_start: day * 2,
                resolved: 3,
                backlog_end: (day + 1) * 2,
                agents: 1,
                fixed_extra_capacity: 0,
            })
            .collect();
        let (fit, score) = fit_and_score(&days, 2, 4, 3).unwrap();
        assert_eq!(fit.service_per_agent_day, 3);
        assert_eq!(score.candidate_abs_error_sum, 0);
        assert_eq!(score.parent_abs_error_sum, 3);
        days[6].arrivals = 6;
        days[6].resolved = 2;
        days[6].backlog_end = 16;
        let (changed_fit, changed_score) = fit_and_score(&days, 2, 4, 3).unwrap();
        assert_eq!(changed_fit, fit);
        assert_eq!(changed_score.candidate_abs_error_sum, 1);
    }

    #[test]
    fn unidentifiable_training_prefix_is_refused() {
        let days = vec![
            ObservedSupportDay {
                arrivals: 3,
                backlog_start: 0,
                resolved: 3,
                backlog_end: 0,
                agents: 1,
                fixed_extra_capacity: 0,
            };
            6
        ];
        assert!(matches!(
            fit_and_score(&days, 2, 3, 3),
            Err(CalibrationError::Unidentified)
        ));
        assert!(matches!(
            fit_and_score(&days, 2, 5, 3),
            Err(CalibrationError::InsufficientHistory)
        ));
    }
}
