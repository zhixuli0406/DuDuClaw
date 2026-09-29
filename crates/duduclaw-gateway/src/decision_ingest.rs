//! Ticket-level export adapter for the support queue pilot.
//!
//! Reopened or transferred tickets need an upstream normalization rule before
//! using this adapter: each ID has one creation and at most one resolution.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Duration, Timelike};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_calibration::{
    CalibrationError, KnownDayInputs, ObservedSupportDay, ProspectiveForecast, fit_capacity,
    forecast_next_day, validate as validate_observed_days,
};
use crate::decision_sim::{
    DecisionSnapshot, InitialCohort, QueueModel, SimulationError, StaffingScenario, simulate,
};

const MAX_TICKETS: usize = 1_000_000;
const MAX_DAYS: usize = 366;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketEvent {
    /// Exact upstream queue identity. Legacy stored exports omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub ticket_id: String,
    pub created_at_utc: String,
    pub resolved_at_utc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyStaffing {
    /// Must match every ticket row in a queue-bound export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub day_utc: String,
    pub agents: u32,
    pub fixed_extra_capacity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportPilotExport {
    pub snapshot_id: String,
    pub baseline_scenario_id: String,
    pub window_start_utc: String,
    pub data_cutoff_utc: String,
    pub source_version_hashes: Vec<String>,
    pub seed: u64,
    pub horizon_days: usize,
    pub tickets: Vec<TicketEvent>,
    pub staffing: Vec<DailyStaffing>,
}

#[derive(Debug, Clone)]
pub struct ImportedSupportPilot {
    pub snapshot: DecisionSnapshot,
    pub baseline: StaffingScenario,
    pub observed_days: Vec<ObservedSupportDay>,
}

/// Version of the ticket timestamp to day-bucket SLA label calculation.
pub fn ticket_sla_label_engine_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_str!("decision_ingest.rs").as_bytes())
    )
}

/// Derive complete daily SLA resolution counts from validated ticket events.
/// The rule matches the queue simulator: resolution-day minus creation-day
/// must be strictly less than `sla_days`.
pub fn derive_ticket_sla_labels(
    export: &SupportPilotExport,
    sla_days: u32,
) -> Result<Vec<u32>, PilotImportError> {
    if sla_days == 0 {
        return Err(PilotImportError::Invalid("SLA days must be positive"));
    }
    build_support_pilot(export)?;
    let start = utc_time(&export.window_start_utc)?;
    let mut labels = vec![0_u32; export.horizon_days];
    for ticket in &export.tickets {
        let Some(resolved) = ticket.resolved_at_utc.as_deref() else {
            continue;
        };
        let created_day = day_index(utc_time(&ticket.created_at_utc)?, start)?;
        let resolved_day = day_index(utc_time(resolved)?, start)?;
        if (0..export.horizon_days as i64).contains(&resolved_day)
            && resolved_day - created_day < i64::from(sla_days)
        {
            increment(&mut labels[resolved_day as usize])?;
        }
    }
    Ok(labels)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PilotImportError {
    #[error("invalid pilot export: {0}")]
    Invalid(&'static str),
    #[error("ticket count overflow")]
    Overflow,
}

#[derive(Debug, thiserror::Error)]
pub enum SlaHoldoutError {
    #[error("invalid ticket-level SLA holdout configuration")]
    Invalid,
    #[error("ticket/staffing source version differs from its canonical SHA-256")]
    SourceMismatch,
    #[error("support export import failed: {0}")]
    Import(#[from] PilotImportError),
    #[error("capacity cannot be fitted on the training prefix: {0}")]
    Capacity(#[from] CalibrationError),
    #[error("SLA holdout simulation failed: {0}")]
    Simulation(#[from] SimulationError),
}

/// Descriptive held-out check of the stock-flow model's day-level SLA rule.
/// Actual held-out arrivals and staffing are supplied to the simulation, so
/// this isolates capacity, age-cohort, and FIFO assumptions rather than
/// measuring a prospective demand forecast or intervention effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaHoldoutDiagnostic {
    pub method: String,
    pub source_sha256: String,
    pub model_version: String,
    pub training_days: usize,
    pub holdout_days: usize,
    pub sla_days: u32,
    pub provided_model_capacity_per_agent_day: u32,
    pub fitted_capacity_per_agent_day: u32,
    pub provided_model_matches_fit: bool,
    pub observed_within_sla_by_day: Vec<u32>,
    pub predicted_within_sla_by_day: Vec<u32>,
    pub observed_total_within_sla: u64,
    pub predicted_total_within_sla: u64,
    pub daily_abs_error_sum: u128,
    #[serde(default)]
    pub no_change_prediction_per_day: u32,
    #[serde(default)]
    pub seasonal_naive_prediction_by_day: Vec<u32>,
    #[serde(default)]
    pub training_mean_prediction_per_day: u32,
    #[serde(default)]
    pub no_change_abs_error_sum: u128,
    #[serde(default)]
    pub seasonal_naive_abs_error_sum: u128,
    #[serde(default)]
    pub training_mean_abs_error_sum: u128,
    #[serde(default)]
    pub conditional_model_beats_all_baselines: bool,
    /// Sequential retrospective forecasts using only information from before
    /// each target day. None is accompanied by an explicit reason.
    #[serde(default)]
    pub one_step_forecast: Option<SlaOneStepBacktest>,
    #[serde(default)]
    pub one_step_unavailable_reason: Option<String>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaOneStepPoint {
    pub day_index: usize,
    pub opening_backlog: u64,
    pub predicted_arrivals: u32,
    pub actual_arrivals: u32,
    pub fitted_capacity_per_agent_day: u32,
    pub predicted_within_sla: u32,
    pub actual_within_sla: u32,
    pub no_change_prediction: u32,
    pub seasonal_naive_prediction: u32,
    pub seven_day_mean_prediction: u32,
}

/// Rolling one-day forecasts. Prior holdout days become known before the next
/// prediction; no target-day arrivals or resolutions enter its prediction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlaOneStepBacktest {
    pub evaluation_days: usize,
    pub arrival_abs_error_sum: u128,
    pub model_abs_error_sum: u128,
    pub no_change_abs_error_sum: u128,
    pub seasonal_naive_abs_error_sum: u128,
    pub seven_day_mean_abs_error_sum: u128,
    pub model_beats_all_baselines: bool,
    pub points: Vec<SlaOneStepPoint>,
}

/// Information available at the start of one UTC support day. Ticket-level
/// opening ages must sum to the separately known opening stock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownSlaDayInputs {
    pub target_day_utc: String,
    pub queue_id: Option<String>,
    pub opening_cohorts: Vec<InitialCohort>,
    pub known: KnownDayInputs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProspectiveSlaForecast {
    pub backlog_forecast: ProspectiveForecast,
    pub predicted_resolved_within_sla: u32,
    pub daily_engine_sha256: String,
}

fn utc_time(value: &str) -> Result<DateTime<chrono::FixedOffset>, PilotImportError> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| PilotImportError::Invalid("invalid RFC3339 time"))?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(PilotImportError::Invalid("times must be UTC"));
    }
    Ok(parsed)
}

fn day_index(
    time: DateTime<chrono::FixedOffset>,
    start: DateTime<chrono::FixedOffset>,
) -> Result<i64, PilotImportError> {
    let seconds = time
        .timestamp()
        .checked_sub(start.timestamp())
        .ok_or(PilotImportError::Overflow)?;
    Ok(seconds.div_euclid(86_400))
}

fn increment(value: &mut u32) -> Result<(), PilotImportError> {
    *value = value.checked_add(1).ok_or(PilotImportError::Overflow)?;
    Ok(())
}

/// Compute one SLA nowcast from inputs available at the target-day start.
/// This pure calculation does not attest when those inputs were captured;
/// a prospective store must bind them to a pre-outcome source artifact.
pub fn forecast_next_day_sla(
    training: &[ObservedSupportDay],
    inputs: &KnownSlaDayInputs,
    model: &QueueModel,
    min_saturated_days: usize,
) -> Result<ProspectiveSlaForecast, SlaHoldoutError> {
    let target = utc_time(&inputs.target_day_utc)?;
    if target.hour() != 0
        || target.minute() != 0
        || target.second() != 0
        || target.timestamp_subsec_nanos() != 0
        || inputs
            .queue_id
            .as_deref()
            .is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 128)
        || inputs.opening_cohorts.len() > 3_660
    {
        return Err(SlaHoldoutError::Invalid);
    }
    let mut ages = HashSet::new();
    let mut opening = 0_u64;
    for cohort in &inputs.opening_cohorts {
        if cohort.age_days == 0 || cohort.count == 0 || !ages.insert(cohort.age_days) {
            return Err(SlaHoldoutError::Invalid);
        }
        opening = opening
            .checked_add(cohort.count as u64)
            .ok_or(SlaHoldoutError::Invalid)?;
    }
    if opening != inputs.known.opening_backlog {
        return Err(SlaHoldoutError::Invalid);
    }
    let backlog_forecast = forecast_next_day(training, &inputs.known, min_saturated_days)?;
    let input_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(inputs).map_err(|_| SlaHoldoutError::Invalid)?)
    );
    let snapshot = DecisionSnapshot {
        id: format!("sla-nowcast:{}", inputs.target_day_utc),
        queue_id: inputs.queue_id.clone(),
        data_cutoff_utc: inputs.target_day_utc.clone(),
        source_version_hashes: vec![input_sha256],
        seed: 0,
        arrivals_by_day: vec![backlog_forecast.predicted_arrivals],
        initial_backlog: inputs.opening_cohorts.clone(),
    };
    let scenario = StaffingScenario {
        id: "sla-nowcast-staffing".into(),
        agents_by_day: vec![inputs.known.planned_agents],
        fixed_extra_capacity_by_day: vec![inputs.known.planned_fixed_extra_capacity],
    };
    let mut fitted_model = model.clone();
    fitted_model.service_capacity_per_agent_day = backlog_forecast.fitted_capacity_per_agent;
    let result = simulate(&snapshot, &fitted_model, &scenario)?;
    if result.final_backlog != backlog_forecast.predicted_backlog_end {
        return Err(SlaHoldoutError::Invalid);
    }
    Ok(ProspectiveSlaForecast {
        backlog_forecast,
        predicted_resolved_within_sla: result.days[0].resolved_within_sla,
        daily_engine_sha256: result.engine_sha256,
    })
}

fn one_step_sla_backtest(
    export: &SupportPilotExport,
    pilot: &ImportedSupportPilot,
    model: &QueueModel,
    observed_sla: &[u32],
    training_days: usize,
    min_saturated_days: usize,
    start: DateTime<chrono::FixedOffset>,
) -> Result<SlaOneStepBacktest, SlaHoldoutError> {
    let mut active = BTreeMap::<i64, u32>::new();
    let mut resolutions_by_day = vec![Vec::<i64>::new(); export.horizon_days];
    for ticket in &export.tickets {
        let created_day = day_index(utc_time(&ticket.created_at_utc)?, start)?;
        let resolved_day = ticket
            .resolved_at_utc
            .as_deref()
            .map(utc_time)
            .transpose()?
            .map(|time| day_index(time, start))
            .transpose()?;
        if created_day < 0 && resolved_day.is_none_or(|day| day >= 0) {
            increment(active.entry(created_day).or_default())?;
        }
        if let Some(day) = resolved_day {
            if (0..export.horizon_days as i64).contains(&day) {
                resolutions_by_day[day as usize].push(created_day);
            }
        }
    }
    let mut points = Vec::with_capacity(export.horizon_days - training_days);
    let mut arrival_error = 0_u128;
    let mut model_error = 0_u128;
    let mut no_change_error = 0_u128;
    let mut seasonal_error = 0_u128;
    let mut mean_error = 0_u128;
    for day in 0..export.horizon_days {
        let opening_backlog: u64 = active.values().map(|count| *count as u64).sum();
        if opening_backlog != pilot.observed_days[day].backlog_start {
            return Err(SlaHoldoutError::Invalid);
        }
        if day >= training_days {
            let known = KnownDayInputs {
                opening_backlog,
                planned_agents: pilot.baseline.agents_by_day[day],
                planned_fixed_extra_capacity: pilot.baseline.fixed_extra_capacity_by_day[day],
            };
            let target_start = start
                .checked_add_signed(Duration::days(day as i64))
                .ok_or(SlaHoldoutError::Invalid)?;
            let opening_cohorts = active
                .iter()
                .map(|(&created_day, &count)| {
                    u32::try_from(day as i64 - created_day)
                        .map(|age_days| InitialCohort { age_days, count })
                        .map_err(|_| SlaHoldoutError::Invalid)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let inputs = KnownSlaDayInputs {
                target_day_utc: target_start.to_rfc3339(),
                queue_id: pilot.snapshot.queue_id.clone(),
                opening_cohorts,
                known,
            };
            let sla = forecast_next_day_sla(
                &pilot.observed_days[..day],
                &inputs,
                model,
                min_saturated_days,
            )?;
            let forecast = &sla.backlog_forecast;
            let predicted = sla.predicted_resolved_within_sla;
            let actual = observed_sla[day];
            let no_change = observed_sla[day - 1];
            let seasonal = observed_sla[day - 7];
            let mean_total: u64 = observed_sla[day - 7..day]
                .iter()
                .map(|count| *count as u64)
                .sum();
            let mean = u32::try_from((mean_total + 3) / 7).map_err(|_| SlaHoldoutError::Invalid)?;
            arrival_error += forecast
                .predicted_arrivals
                .abs_diff(pilot.observed_days[day].arrivals) as u128;
            model_error += predicted.abs_diff(actual) as u128;
            no_change_error += no_change.abs_diff(actual) as u128;
            seasonal_error += seasonal.abs_diff(actual) as u128;
            mean_error += mean.abs_diff(actual) as u128;
            points.push(SlaOneStepPoint {
                day_index: day,
                opening_backlog,
                predicted_arrivals: forecast.predicted_arrivals,
                actual_arrivals: pilot.observed_days[day].arrivals,
                fitted_capacity_per_agent_day: forecast.fitted_capacity_per_agent,
                predicted_within_sla: predicted,
                actual_within_sla: actual,
                no_change_prediction: no_change,
                seasonal_naive_prediction: seasonal,
                seven_day_mean_prediction: mean,
            });
        }
        if pilot.observed_days[day].arrivals > 0
            && active
                .insert(day as i64, pilot.observed_days[day].arrivals)
                .is_some()
        {
            return Err(SlaHoldoutError::Invalid);
        }
        for &created_day in &resolutions_by_day[day] {
            let count = active
                .get_mut(&created_day)
                .ok_or(SlaHoldoutError::Invalid)?;
            *count = count.checked_sub(1).ok_or(SlaHoldoutError::Invalid)?;
        }
        active.retain(|_, count| *count > 0);
    }
    Ok(SlaOneStepBacktest {
        evaluation_days: points.len(),
        arrival_abs_error_sum: arrival_error,
        model_abs_error_sum: model_error,
        no_change_abs_error_sum: no_change_error,
        seasonal_naive_abs_error_sum: seasonal_error,
        seven_day_mean_abs_error_sum: mean_error,
        model_beats_all_baselines: model_error < no_change_error
            && model_error < seasonal_error
            && model_error < mean_error,
        points,
    })
}

/// A new pilot export carries one exact queue ID on every source row. All-None
/// remains readable for historical artifacts, but a partially labeled export
/// cannot silently combine queues. The local import command requires Some.
pub fn pilot_queue_id(export: &SupportPilotExport) -> Result<Option<&str>, PilotImportError> {
    let expected = export
        .staffing
        .first()
        .and_then(|row| row.queue_id.as_deref());
    if expected.is_some_and(|id| id.trim() != id || id.is_empty() || id.len() > 128)
        || export
            .staffing
            .iter()
            .any(|row| row.queue_id.as_deref() != expected)
        || export
            .tickets
            .iter()
            .any(|row| row.queue_id.as_deref() != expected)
    {
        return Err(PilotImportError::Invalid(
            "ticket and staffing rows must share one exact queue ID",
        ));
    }
    Ok(expected)
}

/// Convert immutable ticket and staffing exports to a replay snapshot and
/// observed daily stocks. No model capacity is inferred from unsaturated days.
pub fn build_support_pilot(
    export: &SupportPilotExport,
) -> Result<ImportedSupportPilot, PilotImportError> {
    if export.snapshot_id.trim().is_empty()
        || export.baseline_scenario_id.trim().is_empty()
        || export.horizon_days == 0
        || export.horizon_days > MAX_DAYS
        || export.tickets.len() > MAX_TICKETS
        || export.source_version_hashes.is_empty()
        || export
            .source_version_hashes
            .iter()
            .any(|version| version.trim().is_empty())
        || export.staffing.len() != export.horizon_days
    {
        return Err(PilotImportError::Invalid(
            "missing identity, versions, or complete staffing",
        ));
    }
    pilot_queue_id(export)?;
    let start = utc_time(&export.window_start_utc)?;
    let cutoff = utc_time(&export.data_cutoff_utc)?;
    if start.hour() != 0
        || start.minute() != 0
        || start.second() != 0
        || start.timestamp_subsec_nanos() != 0
        || cutoff.timestamp() < start.timestamp() + export.horizon_days as i64 * 86_400
    {
        return Err(PilotImportError::Invalid(
            "window start must be UTC midnight and cutoff must cover the horizon",
        ));
    }
    let mut arrivals = vec![0_u32; export.horizon_days];
    let mut resolutions = vec![0_u32; export.horizon_days];
    let mut initial_ages: BTreeMap<u32, u32> = BTreeMap::new();
    let mut ticket_ids = HashSet::new();
    for ticket in &export.tickets {
        if ticket.ticket_id.trim().is_empty() || !ticket_ids.insert(&ticket.ticket_id) {
            return Err(PilotImportError::Invalid("duplicate or empty ticket ID"));
        }
        let created = utc_time(&ticket.created_at_utc)?;
        let resolved = ticket
            .resolved_at_utc
            .as_deref()
            .map(utc_time)
            .transpose()?;
        if created > cutoff || resolved.as_ref().is_some_and(|time| time > &cutoff) {
            return Err(PilotImportError::Invalid(
                "ticket event exceeds data cutoff",
            ));
        }
        if resolved.as_ref().is_some_and(|time| time < &created) {
            return Err(PilotImportError::Invalid("resolution precedes creation"));
        }
        let created_day = day_index(created, start)?;
        let resolved_day = resolved.map(|time| day_index(time, start)).transpose()?;
        if created_day < 0 && resolved_day.is_none_or(|day| day >= 0) {
            let age = u32::try_from(-created_day).map_err(|_| PilotImportError::Overflow)?;
            increment(initial_ages.entry(age).or_default())?;
        }
        if (0..export.horizon_days as i64).contains(&created_day) {
            increment(&mut arrivals[created_day as usize])?;
        }
        if let Some(day) = resolved_day {
            if (0..export.horizon_days as i64).contains(&day) {
                increment(&mut resolutions[day as usize])?;
            }
        }
    }
    let mut staff_by_day: HashMap<usize, &DailyStaffing> = HashMap::new();
    for staff in &export.staffing {
        let date = utc_time(&staff.day_utc)?;
        if date.hour() != 0
            || date.minute() != 0
            || date.second() != 0
            || date.timestamp_subsec_nanos() != 0
        {
            return Err(PilotImportError::Invalid(
                "staffing day must be UTC midnight",
            ));
        }
        let day = day_index(date, start)?;
        if day < 0
            || day >= export.horizon_days as i64
            || staff_by_day.insert(day as usize, staff).is_some()
        {
            return Err(PilotImportError::Invalid(
                "duplicate or out-of-window staffing day",
            ));
        }
    }
    let mut agents = Vec::with_capacity(export.horizon_days);
    let mut extra = Vec::with_capacity(export.horizon_days);
    let mut observed = Vec::with_capacity(export.horizon_days);
    let mut backlog = initial_ages
        .values()
        .map(|count| *count as u64)
        .sum::<u64>();
    for day in 0..export.horizon_days {
        let staff = staff_by_day
            .get(&day)
            .ok_or(PilotImportError::Invalid("missing staffing day"))?;
        let start_backlog = backlog;
        let available = backlog
            .checked_add(arrivals[day] as u64)
            .ok_or(PilotImportError::Overflow)?;
        backlog =
            available
                .checked_sub(resolutions[day] as u64)
                .ok_or(PilotImportError::Invalid(
                    "resolutions exceed available tickets",
                ))?;
        agents.push(staff.agents);
        extra.push(staff.fixed_extra_capacity);
        observed.push(ObservedSupportDay {
            arrivals: arrivals[day],
            backlog_start: start_backlog,
            resolved: resolutions[day],
            backlog_end: backlog,
            agents: staff.agents,
            fixed_extra_capacity: staff.fixed_extra_capacity,
        });
    }
    validate_observed_days(&observed).map_err(|_| {
        PilotImportError::Invalid("ticket resolutions violate observed capacity bounds")
    })?;
    let initial_backlog = initial_ages
        .into_iter()
        .map(|(age_days, count)| InitialCohort { age_days, count })
        .collect();
    Ok(ImportedSupportPilot {
        snapshot: DecisionSnapshot {
            id: export.snapshot_id.clone(),
            queue_id: pilot_queue_id(export)?.map(str::to_owned),
            data_cutoff_utc: export.data_cutoff_utc.clone(),
            source_version_hashes: export.source_version_hashes.clone(),
            seed: export.seed,
            arrivals_by_day: arrivals,
            initial_backlog,
        },
        baseline: StaffingScenario {
            id: export.baseline_scenario_id.clone(),
            agents_by_day: agents,
            fixed_extra_capacity_by_day: extra,
        },
        observed_days: observed,
    })
}

/// Freeze the training-prefix capacity, reconstruct ticket ages at the first
/// held-out midnight from actual source events, then compare day-level SLA
/// completions on the later days. No held-out service count enters the fit.
pub fn evaluate_ticket_sla_holdout(
    export: &SupportPilotExport,
    model: &QueueModel,
    training_days: usize,
    min_saturated_days: usize,
) -> Result<SlaHoldoutDiagnostic, SlaHoldoutError> {
    if training_days < 7
        || training_days >= export.horizon_days
        || min_saturated_days == 0
        || model.sla_days == 0
    {
        return Err(SlaHoldoutError::Invalid);
    }
    let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))
        .map_err(|_| SlaHoldoutError::Invalid)?;
    let source_sha256 = format!("{:x}", Sha256::digest(source_bytes));
    if export.source_version_hashes != vec![source_sha256.clone()] {
        return Err(SlaHoldoutError::SourceMismatch);
    }
    let pilot = build_support_pilot(export)?;
    let fit = fit_capacity(&pilot.observed_days[..training_days], min_saturated_days)?;
    let start = utc_time(&export.window_start_utc)?;
    let holdout_start = start
        .checked_add_signed(Duration::days(training_days as i64))
        .ok_or(SlaHoldoutError::Invalid)?;
    let mut initial_ages = BTreeMap::<u32, u32>::new();
    let mut observed_all = vec![0_u32; export.horizon_days];
    for ticket in &export.tickets {
        let created = utc_time(&ticket.created_at_utc)?;
        let resolved = ticket
            .resolved_at_utc
            .as_deref()
            .map(utc_time)
            .transpose()?;
        let created_day = day_index(created, start)?;
        if created < holdout_start && resolved.is_none_or(|time| time >= holdout_start) {
            let age = u32::try_from(training_days as i64 - created_day)
                .map_err(|_| PilotImportError::Overflow)?;
            increment(initial_ages.entry(age).or_default())?;
        }
        if let Some(resolved) = resolved {
            let resolved_day = day_index(resolved, start)?;
            if (0..export.horizon_days as i64).contains(&resolved_day)
                && resolved_day - created_day < model.sla_days as i64
            {
                increment(&mut observed_all[resolved_day as usize])?;
            }
        }
    }
    let training_sla = &observed_all[..training_days];
    let observed = &observed_all[training_days..];
    let (one_step_forecast, one_step_unavailable_reason) = match one_step_sla_backtest(
        export,
        &pilot,
        model,
        &observed_all,
        training_days,
        min_saturated_days,
        start,
    ) {
        Ok(report) => (Some(report), None),
        Err(SlaHoldoutError::Capacity(error)) => (
            None,
            Some(format!(
                "rolling one-step capacity became unidentified: {error}"
            )),
        ),
        Err(error) => return Err(error),
    };
    let opening: u64 = initial_ages.values().map(|count| *count as u64).sum();
    if opening != pilot.observed_days[training_days].backlog_start {
        return Err(SlaHoldoutError::Invalid);
    }
    let holdout_snapshot = DecisionSnapshot {
        id: format!("{}:sla-holdout-{training_days}", pilot.snapshot.id),
        queue_id: pilot.snapshot.queue_id,
        data_cutoff_utc: holdout_start.to_rfc3339(),
        source_version_hashes: pilot.snapshot.source_version_hashes,
        seed: pilot.snapshot.seed,
        arrivals_by_day: pilot.snapshot.arrivals_by_day[training_days..].to_vec(),
        initial_backlog: initial_ages
            .into_iter()
            .map(|(age_days, count)| InitialCohort { age_days, count })
            .collect(),
    };
    let holdout_scenario = StaffingScenario {
        id: format!("{}:sla-holdout-{training_days}", pilot.baseline.id),
        agents_by_day: pilot.baseline.agents_by_day[training_days..].to_vec(),
        fixed_extra_capacity_by_day: pilot.baseline.fixed_extra_capacity_by_day[training_days..]
            .to_vec(),
    };
    let mut holdout_model = model.clone();
    holdout_model.service_capacity_per_agent_day = fit.service_per_agent_day;
    let predicted = simulate(&holdout_snapshot, &holdout_model, &holdout_scenario)?;
    let predicted_by_day: Vec<u32> = predicted
        .days
        .iter()
        .map(|day| day.resolved_within_sla)
        .collect();
    let observed_total: u64 = observed.iter().map(|count| *count as u64).sum();
    let predicted_total: u64 = predicted_by_day.iter().map(|count| *count as u64).sum();
    let daily_abs_error_sum = observed
        .iter()
        .zip(&predicted_by_day)
        .map(|(actual, forecast)| actual.abs_diff(*forecast) as u128)
        .sum();
    let no_change_prediction_per_day = training_sla[training_days - 1];
    let seasonal_naive_prediction_by_day: Vec<u32> = (0..observed.len())
        .map(|offset| training_sla[training_days - 7 + offset % 7])
        .collect();
    let training_total: u64 = training_sla.iter().map(|count| *count as u64).sum();
    let training_mean_prediction_per_day =
        u32::try_from((training_total + training_days as u64 / 2) / training_days as u64)
            .map_err(|_| SlaHoldoutError::Invalid)?;
    let no_change_abs_error_sum: u128 = observed
        .iter()
        .map(|actual| actual.abs_diff(no_change_prediction_per_day) as u128)
        .sum();
    let seasonal_naive_abs_error_sum: u128 = observed
        .iter()
        .zip(&seasonal_naive_prediction_by_day)
        .map(|(actual, forecast)| actual.abs_diff(*forecast) as u128)
        .sum();
    let training_mean_abs_error_sum: u128 = observed
        .iter()
        .map(|actual| actual.abs_diff(training_mean_prediction_per_day) as u128)
        .sum();
    Ok(SlaHoldoutDiagnostic {
        method: "training_prefix_fifo_day_sla_holdout_v3".into(),
        source_sha256,
        model_version: model.version.clone(),
        training_days,
        holdout_days: observed.len(),
        sla_days: model.sla_days,
        provided_model_capacity_per_agent_day: model.service_capacity_per_agent_day,
        fitted_capacity_per_agent_day: fit.service_per_agent_day,
        provided_model_matches_fit:
            model.service_capacity_per_agent_day == fit.service_per_agent_day,
        observed_within_sla_by_day: observed.to_vec(),
        predicted_within_sla_by_day: predicted_by_day,
        observed_total_within_sla: observed_total,
        predicted_total_within_sla: predicted_total,
        daily_abs_error_sum,
        no_change_prediction_per_day,
        seasonal_naive_prediction_by_day,
        training_mean_prediction_per_day,
        no_change_abs_error_sum,
        seasonal_naive_abs_error_sum,
        training_mean_abs_error_sum,
        conditional_model_beats_all_baselines: daily_abs_error_sum < no_change_abs_error_sum
            && daily_abs_error_sum < seasonal_naive_abs_error_sum
            && daily_abs_error_sum < training_mean_abs_error_sum,
        one_step_forecast,
        one_step_unavailable_reason,
        limitations: vec![
            "SLA replay uses training-fitted capacity; the supplied model capacity is reported separately and is not the replay capacity".into(),
            "Actual held-out arrivals and staffing are supplied; this does not evaluate a future demand forecast".into(),
            "Whole UTC-day SLA and FIFO assumptions can differ from the operator's ticket-level SLA clock".into(),
            "A descriptive error on one support export does not establish predictive coverage or causal staffing impact".into(),
            "Simple baselines use only pre-holdout SLA counts while FIFO replay receives actual later arrivals and staffing, so their errors are not an equal-information forecasting contest".into(),
            "Rolling one-step SLA results reconstruct opening ticket ages retrospectively and are not pre-outcome shadow commitments".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_events_build_conserved_daily_stocks_and_initial_age() {
        let export = SupportPilotExport {
            snapshot_id: "snap".into(),
            baseline_scenario_id: "base".into(),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            data_cutoff_utc: "2026-09-03T00:00:00Z".into(),
            source_version_hashes: vec!["tickets-v1".into(), "staff-v1".into()],
            seed: 7,
            horizon_days: 2,
            tickets: vec![
                TicketEvent {
                    queue_id: None,
                    ticket_id: "old".into(),
                    created_at_utc: "2026-08-30T12:00:00Z".into(),
                    resolved_at_utc: Some("2026-09-02T08:00:00Z".into()),
                },
                TicketEvent {
                    queue_id: None,
                    ticket_id: "new".into(),
                    created_at_utc: "2026-09-01T01:00:00Z".into(),
                    resolved_at_utc: Some("2026-09-01T20:00:00Z".into()),
                },
                TicketEvent {
                    queue_id: None,
                    ticket_id: "pending".into(),
                    created_at_utc: "2026-09-02T10:00:00Z".into(),
                    resolved_at_utc: None,
                },
            ],
            staffing: vec![
                DailyStaffing {
                    queue_id: None,
                    day_utc: "2026-09-01T00:00:00Z".into(),
                    agents: 1,
                    fixed_extra_capacity: 0,
                },
                DailyStaffing {
                    queue_id: None,
                    day_utc: "2026-09-02T00:00:00Z".into(),
                    agents: 2,
                    fixed_extra_capacity: 0,
                },
            ],
        };
        let pilot = build_support_pilot(&export).unwrap();
        assert_eq!(pilot.snapshot.queue_id, None);
        assert_eq!(
            pilot.snapshot.initial_backlog,
            vec![InitialCohort {
                age_days: 2,
                count: 1
            }]
        );
        assert_eq!(pilot.snapshot.arrivals_by_day, vec![1, 1]);
        assert_eq!(pilot.observed_days[0].backlog_end, 1);
        assert_eq!(pilot.observed_days[1].backlog_end, 1);
        assert_eq!(pilot.baseline.agents_by_day, vec![1, 2]);
    }

    #[test]
    fn duplicate_ticket_ids_are_rejected() {
        let mut export = SupportPilotExport {
            snapshot_id: "s".into(),
            baseline_scenario_id: "b".into(),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            data_cutoff_utc: "2026-09-02T00:00:00Z".into(),
            source_version_hashes: vec!["v1".into()],
            seed: 1,
            horizon_days: 1,
            tickets: vec![TicketEvent {
                queue_id: None,
                ticket_id: "x".into(),
                created_at_utc: "2026-09-01T00:00:00Z".into(),
                resolved_at_utc: None,
            }],
            staffing: vec![DailyStaffing {
                queue_id: None,
                day_utc: "2026-09-01T00:00:00Z".into(),
                agents: 1,
                fixed_extra_capacity: 0,
            }],
        };
        export.tickets.push(export.tickets[0].clone());
        assert_eq!(
            build_support_pilot(&export).unwrap_err(),
            PilotImportError::Invalid("duplicate or empty ticket ID")
        );
    }

    #[test]
    fn queue_identity_must_cover_every_ticket_and_staffing_row() {
        let mut export = crate::decision_synthetic::synthetic_support_export(47, 21).unwrap();
        assert_eq!(
            pilot_queue_id(&export).unwrap(),
            Some("synthetic-support-queue")
        );
        assert_eq!(
            build_support_pilot(&export)
                .unwrap()
                .snapshot
                .queue_id
                .as_deref(),
            Some("synthetic-support-queue")
        );
        export.tickets[0].queue_id = Some("other-queue".into());
        assert!(matches!(
            build_support_pilot(&export),
            Err(PilotImportError::Invalid(
                "ticket and staffing rows must share one exact queue ID"
            ))
        ));
        export.tickets[0].queue_id = Some("synthetic-support-queue".into());
        export.staffing[0].queue_id = None;
        assert!(build_support_pilot(&export).is_err());
        export.staffing[0].queue_id = Some("synthetic-support-queue".into());
        assert!(build_support_pilot(&export).is_ok());
    }

    #[test]
    fn zero_staff_ticket_resolutions_are_rejected_before_snapshot_import() {
        let export = SupportPilotExport {
            snapshot_id: "snap".into(),
            baseline_scenario_id: "base".into(),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            data_cutoff_utc: "2026-09-02T00:00:00Z".into(),
            source_version_hashes: vec!["source-v1".into()],
            seed: 1,
            horizon_days: 1,
            tickets: vec![TicketEvent {
                queue_id: Some("support-queue".into()),
                ticket_id: "resolved".into(),
                created_at_utc: "2026-09-01T01:00:00Z".into(),
                resolved_at_utc: Some("2026-09-01T02:00:00Z".into()),
            }],
            staffing: vec![DailyStaffing {
                queue_id: Some("support-queue".into()),
                day_utc: "2026-09-01T00:00:00Z".into(),
                agents: 0,
                fixed_extra_capacity: 0,
            }],
        };
        assert_eq!(
            build_support_pilot(&export).unwrap_err(),
            PilotImportError::Invalid("ticket resolutions violate observed capacity bounds")
        );
        let mut staffed = export;
        staffed.staffing[0].fixed_extra_capacity = 1;
        assert!(build_support_pilot(&staffed).is_ok());
    }

    #[test]
    fn ticket_sla_holdout_detects_fifo_mismatch_on_later_days() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let mut tickets = vec![TicketEvent {
            queue_id: None,
            ticket_id: "old-pending".into(),
            created_at_utc: "2026-08-31T09:00:00Z".into(),
            resolved_at_utc: None,
        }];
        let mut staffing = Vec::new();
        for day in 0..8 {
            let date = start + Duration::days(day);
            staffing.push(DailyStaffing {
                queue_id: None,
                day_utc: format!("{date}T00:00:00Z"),
                agents: 1,
                fixed_extra_capacity: 0,
            });
            for ticket in 0..2 {
                tickets.push(TicketEvent {
                    queue_id: None,
                    ticket_id: format!("{day}-{ticket}"),
                    created_at_utc: format!("{date}T09:00:00Z"),
                    resolved_at_utc: Some(format!("{date}T20:00:00Z")),
                });
            }
        }
        let mut export = SupportPilotExport {
            snapshot_id: "sla-mismatch".into(),
            baseline_scenario_id: "base".into(),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            data_cutoff_utc: "2026-09-09T00:00:00Z".into(),
            source_version_hashes: vec!["source-v1".into()],
            seed: 1,
            horizon_days: 8,
            tickets,
            staffing,
        };
        let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing)).unwrap();
        export.source_version_hashes = vec![format!("{:x}", Sha256::digest(source_bytes))];
        let model = QueueModel {
            version: "day-sla".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 1,
            staff_cost_cents_per_agent_day: 100,
        };
        let report = evaluate_ticket_sla_holdout(&export, &model, 7, 3).unwrap();
        assert_eq!(report.fitted_capacity_per_agent_day, 2);
        assert!(report.provided_model_matches_fit);
        assert_eq!(report.observed_within_sla_by_day, vec![2]);
        assert_eq!(report.predicted_within_sla_by_day, vec![1]);
        assert_eq!(report.daily_abs_error_sum, 1);
        assert_eq!(report.no_change_prediction_per_day, 2);
        assert_eq!(report.seasonal_naive_prediction_by_day, vec![2]);
        assert_eq!(report.training_mean_prediction_per_day, 2);
        assert_eq!(report.no_change_abs_error_sum, 0);
        assert_eq!(report.seasonal_naive_abs_error_sum, 0);
        assert_eq!(report.training_mean_abs_error_sum, 0);
        assert!(!report.conditional_model_beats_all_baselines);
        let prior_only = report.one_step_forecast.as_ref().unwrap();
        assert_eq!(prior_only.evaluation_days, 1);
        assert_eq!(prior_only.points[0].predicted_arrivals, 2);
        assert_eq!(prior_only.points[0].predicted_within_sla, 1);
        assert_eq!(prior_only.model_abs_error_sum, 1);
        assert!(!prior_only.model_beats_all_baselines);
        let mut fifo_shifted = export.clone();
        fifo_shifted.tickets[0].resolved_at_utc = Some("2026-09-08T20:00:00Z".into());
        fifo_shifted
            .tickets
            .retain(|ticket| ticket.ticket_id != "7-1");
        let shifted_bytes =
            serde_json::to_vec(&(&fifo_shifted.tickets, &fifo_shifted.staffing)).unwrap();
        fifo_shifted.source_version_hashes = vec![format!("{:x}", Sha256::digest(shifted_bytes))];
        let shifted = evaluate_ticket_sla_holdout(&fifo_shifted, &model, 7, 3).unwrap();
        assert_eq!(shifted.observed_within_sla_by_day, vec![1]);
        assert_eq!(shifted.predicted_within_sla_by_day, vec![1]);
        assert_eq!(shifted.daily_abs_error_sum, 0);
        assert_eq!(shifted.no_change_abs_error_sum, 1);
        assert_eq!(shifted.seasonal_naive_abs_error_sum, 1);
        assert_eq!(shifted.training_mean_abs_error_sum, 1);
        assert!(shifted.conditional_model_beats_all_baselines);
        let shifted_prior_only = shifted.one_step_forecast.as_ref().unwrap();
        assert_eq!(
            shifted_prior_only.points[0].predicted_arrivals,
            prior_only.points[0].predicted_arrivals
        );
        assert_eq!(
            shifted_prior_only.points[0].predicted_within_sla,
            prior_only.points[0].predicted_within_sla
        );
        assert_eq!(shifted_prior_only.points[0].actual_arrivals, 1);
        assert_eq!(shifted_prior_only.arrival_abs_error_sum, 1);
        assert_eq!(shifted_prior_only.model_abs_error_sum, 0);
        assert!(shifted_prior_only.model_beats_all_baselines);
        let imported = build_support_pilot(&export).unwrap();
        let known_sla = KnownSlaDayInputs {
            target_day_utc: "2026-09-08T00:00:00Z".into(),
            queue_id: None,
            opening_cohorts: vec![InitialCohort {
                age_days: 8,
                count: 1,
            }],
            known: KnownDayInputs {
                opening_backlog: 1,
                planned_agents: 1,
                planned_fixed_extra_capacity: 0,
            },
        };
        let pure =
            forecast_next_day_sla(&imported.observed_days[..7], &known_sla, &model, 3).unwrap();
        assert_eq!(pure.backlog_forecast.predicted_arrivals, 2);
        assert_eq!(pure.backlog_forecast.predicted_backlog_end, 1);
        assert_eq!(pure.predicted_resolved_within_sla, 1);
        let mut invalid_known = known_sla.clone();
        invalid_known.opening_cohorts[0].count = 2;
        assert!(matches!(
            forecast_next_day_sla(&imported.observed_days[..7], &invalid_known, &model, 3),
            Err(SlaHoldoutError::Invalid)
        ));
        invalid_known = known_sla.clone();
        invalid_known.opening_cohorts.push(InitialCohort {
            age_days: 8,
            count: 1,
        });
        assert!(matches!(
            forecast_next_day_sla(&imported.observed_days[..7], &invalid_known, &model, 3),
            Err(SlaHoldoutError::Invalid)
        ));
        invalid_known = known_sla.clone();
        invalid_known.opening_cohorts[0].age_days = 0;
        assert!(matches!(
            forecast_next_day_sla(&imported.observed_days[..7], &invalid_known, &model, 3),
            Err(SlaHoldoutError::Invalid)
        ));
        invalid_known = known_sla;
        invalid_known.target_day_utc = "2026-09-08T01:00:00Z".into();
        assert!(matches!(
            forecast_next_day_sla(&imported.observed_days[..7], &invalid_known, &model, 3),
            Err(SlaHoldoutError::Invalid)
        ));
        let mut changed = export.clone();
        changed.tickets[0].ticket_id.push_str("-changed");
        assert!(matches!(
            evaluate_ticket_sla_holdout(&changed, &model, 7, 3),
            Err(SlaHoldoutError::SourceMismatch)
        ));
        let mut mismatched_model = model;
        mismatched_model.service_capacity_per_agent_day = 3;
        let mismatch = evaluate_ticket_sla_holdout(&export, &mismatched_model, 7, 3).unwrap();
        assert_eq!(mismatch.provided_model_capacity_per_agent_day, 3);
        assert_eq!(mismatch.fitted_capacity_per_agent_day, 2);
        assert!(!mismatch.provided_model_matches_fit);
    }
}
