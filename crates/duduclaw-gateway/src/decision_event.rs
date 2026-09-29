//! Deterministic ticket-event queue for exploratory support scenarios.
//!
//! Historical resolution timestamps validate the export but never drive
//! counterfactual service. Service opportunities occur at fixed shift slots;
//! an arrival can use a slot only when it existed before that slot began.

use std::collections::VecDeque;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_ingest::{PilotImportError, SupportPilotExport, build_support_pilot};
use crate::decision_sim::{QueueModel, StaffingScenario, engine_code_sha256, simulate};

const DAY_SECONDS: i64 = 86_400;
const EVENT_ENGINE_SCHEMA: &str = "support-event-queue-v1";

/// Identity of the current ticket-event engine. Single source of truth for
/// both the replay hash and any "was this row produced by today's code?"
/// check, so the two can never drift apart.
pub fn event_engine_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_str!("decision_event.rs").as_bytes())
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventQueueConfig {
    /// Seconds after UTC midnight when all agents begin their shift.
    pub shift_start_seconds: u32,
    /// Total staffed seconds per UTC day. It must divide evenly into the
    /// configured service slots per agent.
    pub shift_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSimulatedDay {
    pub day: usize,
    pub arrivals: u32,
    pub resolved: u32,
    pub backlog_end: u64,
    pub overdue_pending: u64,
    pub resolved_within_sla: u32,
    pub staff_cost_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSimulationResult {
    pub status: String,
    pub snapshot_id: String,
    pub model_version: String,
    pub scenario_id: String,
    pub event_engine_sha256: String,
    pub replay_hash: String,
    pub days: Vec<EventSimulatedDay>,
    pub initial_backlog: u64,
    pub total_arrivals: u64,
    pub total_resolved: u64,
    pub final_backlog: u64,
    pub total_resolved_within_sla: u64,
    pub total_staff_cost_cents: u64,
    /// Quantiles among tickets served within the horizon; pending waits are censored.
    pub wait_seconds_p50: Option<u64>,
    pub wait_seconds_p95: Option<u64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EventSimulationError {
    #[error("invalid event queue input: {0}")]
    Invalid(&'static str),
    #[error("ticket export invalid: {0}")]
    Export(#[from] PilotImportError),
    #[error("event queue arithmetic overflow")]
    Overflow,
    #[error("event queue serialization failed")]
    Serialization,
}

fn timestamp(value: &str) -> Result<i64, EventSimulationError> {
    DateTime::parse_from_rfc3339(value)
        .map_err(|_| EventSimulationError::Invalid("invalid ticket timestamp"))
        .and_then(|time| {
            if time.timestamp_subsec_nanos() != 0 {
                return Err(EventSimulationError::Invalid(
                    "event timestamps must have whole-second precision",
                ));
            }
            Ok(time.timestamp())
        })
}

fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    // Nearest-rank quantile; integer-only for bit-for-bit replay.
    let rank = sorted.len().saturating_mul(numerator).div_ceil(denominator);
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

/// Simulate fixed-duration FIFO service against exact ticket creation times.
/// The simple daily engine remains the primary stock-flow baseline; this
/// event engine exposes within-day waiting/SLA sensitivity under explicit
/// shift and service-duration assumptions.
pub fn simulate_ticket_events(
    export: &SupportPilotExport,
    model: &QueueModel,
    scenario: &StaffingScenario,
    config: &EventQueueConfig,
) -> Result<EventSimulationResult, EventSimulationError> {
    let pilot = build_support_pilot(export)?;
    let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))
        .map_err(|_| EventSimulationError::Serialization)?;
    let source_hash = format!("{:x}", Sha256::digest(source_bytes));
    if !export.source_version_hashes.contains(&source_hash) {
        return Err(EventSimulationError::Invalid(
            "source version does not match exact ticket/staffing export",
        ));
    }
    simulate(&pilot.snapshot, model, scenario)
        .map_err(|_| EventSimulationError::Invalid("daily model or scenario invalid"))?;
    if scenario
        .fixed_extra_capacity_by_day
        .iter()
        .any(|&value| value != 0)
    {
        return Err(EventSimulationError::Invalid(
            "fixed extra capacity has no ticket-level service schedule",
        ));
    }
    let slots = model.service_capacity_per_agent_day;
    if config.shift_seconds == 0
        || config.shift_seconds < slots
        || config.shift_seconds % slots != 0
        || config.shift_start_seconds >= DAY_SECONDS as u32
        || config.shift_start_seconds as u64 + config.shift_seconds as u64 > DAY_SECONDS as u64
    {
        return Err(EventSimulationError::Invalid(
            "shift must fit one UTC day and divide into service slots",
        ));
    }
    let start = timestamp(&export.window_start_utc)?;
    let horizon_end = start
        .checked_add(
            (export.horizon_days as i64)
                .checked_mul(DAY_SECONDS)
                .ok_or(EventSimulationError::Overflow)?,
        )
        .ok_or(EventSimulationError::Overflow)?;
    let mut initial = Vec::new();
    let mut incoming = Vec::new();
    for ticket in &export.tickets {
        let created = timestamp(&ticket.created_at_utc)?;
        let resolved = ticket
            .resolved_at_utc
            .as_deref()
            .map(timestamp)
            .transpose()?;
        if created < start {
            let was_open = resolved.is_none_or(|resolved| resolved >= start);
            if was_open {
                initial.push((created, ticket.ticket_id.as_str()));
            }
        } else if created < horizon_end {
            incoming.push((created, ticket.ticket_id.as_str()));
        }
    }
    initial.sort_unstable();
    incoming.sort_unstable();
    let initial_backlog = initial.len() as u64;
    let mut queue: VecDeque<i64> = initial.into_iter().map(|(created, _)| created).collect();
    let mut next_arrival = 0;
    let mut total_resolved = 0_u64;
    let mut total_within_sla = 0_u64;
    let mut total_staff_cost_cents = 0_u64;
    let mut waits = Vec::new();
    let mut days = Vec::with_capacity(export.horizon_days);
    let duration = (config.shift_seconds / slots) as i64;
    let sla_seconds = (model.sla_days as i64)
        .checked_mul(DAY_SECONDS)
        .ok_or(EventSimulationError::Overflow)?;
    for day in 0..export.horizon_days {
        let day_start = start
            .checked_add(
                (day as i64)
                    .checked_mul(DAY_SECONDS)
                    .ok_or(EventSimulationError::Overflow)?,
            )
            .ok_or(EventSimulationError::Overflow)?;
        let day_end = day_start
            .checked_add(DAY_SECONDS)
            .ok_or(EventSimulationError::Overflow)?;
        let mut resolved = 0_u32;
        let mut within_sla = 0_u32;
        for slot in 0..slots {
            let slot_start = day_start
                .checked_add(config.shift_start_seconds as i64)
                .and_then(|time| time.checked_add((slot as i64) * duration))
                .ok_or(EventSimulationError::Overflow)?;
            while next_arrival < incoming.len() && incoming[next_arrival].0 <= slot_start {
                queue.push_back(incoming[next_arrival].0);
                next_arrival += 1;
            }
            let completion = slot_start
                .checked_add(duration)
                .ok_or(EventSimulationError::Overflow)?;
            for _ in 0..scenario.agents_by_day[day].min(queue.len() as u32) {
                let created = queue
                    .pop_front()
                    .ok_or(EventSimulationError::Invalid("queue empty at service slot"))?;
                waits.push(
                    u64::try_from(slot_start - created)
                        .map_err(|_| EventSimulationError::Overflow)?,
                );
                resolved = resolved
                    .checked_add(1)
                    .ok_or(EventSimulationError::Overflow)?;
                if completion - created < sla_seconds {
                    within_sla = within_sla
                        .checked_add(1)
                        .ok_or(EventSimulationError::Overflow)?;
                }
            }
        }
        while next_arrival < incoming.len() && incoming[next_arrival].0 < day_end {
            queue.push_back(incoming[next_arrival].0);
            next_arrival += 1;
        }
        let overdue_pending = queue
            .iter()
            .filter(|&&created| day_end - created >= sla_seconds)
            .count() as u64;
        total_resolved = total_resolved
            .checked_add(resolved as u64)
            .ok_or(EventSimulationError::Overflow)?;
        total_within_sla = total_within_sla
            .checked_add(within_sla as u64)
            .ok_or(EventSimulationError::Overflow)?;
        let staff_cost_cents = (scenario.agents_by_day[day] as u64)
            .checked_mul(model.staff_cost_cents_per_agent_day)
            .ok_or(EventSimulationError::Overflow)?;
        total_staff_cost_cents = total_staff_cost_cents
            .checked_add(staff_cost_cents)
            .ok_or(EventSimulationError::Overflow)?;
        days.push(EventSimulatedDay {
            day,
            arrivals: pilot.snapshot.arrivals_by_day[day],
            resolved,
            backlog_end: queue.len() as u64,
            overdue_pending,
            resolved_within_sla: within_sla,
            staff_cost_cents,
        });
    }
    let total_arrivals = incoming.len() as u64;
    let final_backlog = queue.len() as u64;
    if initial_backlog
        .checked_add(total_arrivals)
        .and_then(|n| n.checked_sub(total_resolved))
        != Some(final_backlog)
        || total_arrivals
            != pilot
                .snapshot
                .arrivals_by_day
                .iter()
                .map(|&n| n as u64)
                .sum::<u64>()
    {
        return Err(EventSimulationError::Invalid(
            "ticket stock conservation violated",
        ));
    }
    waits.sort_unstable();
    let event_engine_sha256 = self::event_engine_sha256();
    let replay_bytes = serde_json::to_vec(&(
        EVENT_ENGINE_SCHEMA,
        &event_engine_sha256,
        engine_code_sha256(),
        export,
        model,
        scenario,
        config,
    ))
    .map_err(|_| EventSimulationError::Serialization)?;
    Ok(EventSimulationResult {
        status: "exploratory_event_scenario".into(),
        snapshot_id: export.snapshot_id.clone(),
        model_version: model.version.clone(),
        scenario_id: scenario.id.clone(),
        event_engine_sha256,
        replay_hash: format!("{:x}", Sha256::digest(replay_bytes)),
        days,
        initial_backlog,
        total_arrivals,
        total_resolved,
        final_backlog,
        total_resolved_within_sla: total_within_sla,
        total_staff_cost_cents,
        wait_seconds_p50: percentile(&waits, 50, 100),
        wait_seconds_p95: percentile(&waits, 95, 100),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_ingest::{DailyStaffing, TicketEvent};
    use crate::decision_synthetic::synthetic_support_export;

    fn config() -> EventQueueConfig {
        EventQueueConfig {
            shift_start_seconds: 9 * 3_600,
            shift_seconds: 8 * 3_600,
        }
    }

    #[test]
    fn synthetic_ticket_events_replay_and_match_daily_stocks() {
        let export = synthetic_support_export(47, 35).unwrap();
        let pilot = build_support_pilot(&export).unwrap();
        let model = QueueModel {
            version: "event-v1".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 10_000,
        };
        let event = simulate_ticket_events(&export, &model, &pilot.baseline, &config()).unwrap();
        assert_eq!(
            event,
            simulate_ticket_events(&export, &model, &pilot.baseline, &config()).unwrap()
        );
        let daily = simulate(&pilot.snapshot, &model, &pilot.baseline).unwrap();
        assert_eq!(event.initial_backlog, 10);
        assert_eq!(event.total_arrivals, daily.total_arrivals);
        assert_eq!(event.final_backlog, daily.final_backlog);
        assert_eq!(event.total_staff_cost_cents, daily.total_staff_cost_cents);
        assert!(event.wait_seconds_p95.unwrap() >= event.wait_seconds_p50.unwrap());
        for (event_day, daily_day) in event.days.iter().zip(&daily.days) {
            assert_eq!(event_day.resolved, daily_day.resolved);
            assert_eq!(event_day.backlog_end, daily_day.backlog_end);
            assert_eq!(event_day.staff_cost_cents, daily_day.staff_cost_cents);
            assert!(event_day.resolved <= 16);
        }
        let alternative = StaffingScenario {
            id: "three-agents".into(),
            agents_by_day: vec![3; 35],
            fixed_extra_capacity_by_day: vec![0; 35],
        };
        let more = simulate_ticket_events(&export, &model, &alternative, &config()).unwrap();
        assert_eq!(more.total_arrivals, event.total_arrivals);
        assert!(more.final_backlog < event.final_backlog);
        assert_ne!(more.replay_hash, event.replay_hash);
    }

    #[test]
    fn late_arrival_cannot_use_past_capacity_or_historical_resolution() {
        let mut export = SupportPilotExport {
            snapshot_id: "late".into(),
            baseline_scenario_id: "base".into(),
            window_start_utc: "2026-01-01T00:00:00Z".into(),
            data_cutoff_utc: "2026-01-02T00:00:00Z".into(),
            source_version_hashes: vec!["source-v1".into()],
            seed: 1,
            horizon_days: 1,
            tickets: vec![
                TicketEvent {
                    queue_id: None,
                    ticket_id: "early".into(),
                    created_at_utc: "2026-01-01T08:00:00Z".into(),
                    resolved_at_utc: Some("2026-01-01T23:00:00Z".into()),
                },
                TicketEvent {
                    queue_id: None,
                    ticket_id: "late".into(),
                    created_at_utc: "2026-01-01T18:00:00Z".into(),
                    resolved_at_utc: Some("2026-01-01T23:00:00Z".into()),
                },
            ],
            staffing: vec![DailyStaffing {
                queue_id: None,
                day_utc: "2026-01-01T00:00:00Z".into(),
                agents: 1,
                fixed_extra_capacity: 0,
            }],
        };
        let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing)).unwrap();
        export.source_version_hashes = vec![format!("{:x}", Sha256::digest(source_bytes))];
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 1,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1],
            fixed_extra_capacity_by_day: vec![0],
        };
        let daily = simulate(
            &build_support_pilot(&export).unwrap().snapshot,
            &model,
            &scenario,
        )
        .unwrap();
        let event = simulate_ticket_events(&export, &model, &scenario, &config()).unwrap();
        assert_eq!(daily.total_resolved, 2);
        assert_eq!(event.total_resolved, 1);
        assert_eq!(event.final_backlog, 1);
        assert_eq!(event.wait_seconds_p50, Some(3_600));
        let mut changed_history = export.clone();
        for ticket in &mut changed_history.tickets {
            ticket.resolved_at_utc = None;
        }
        assert!(matches!(
            simulate_ticket_events(&changed_history, &model, &scenario, &config()),
            Err(EventSimulationError::Invalid(
                "source version does not match exact ticket/staffing export"
            ))
        ));
        let changed_bytes =
            serde_json::to_vec(&(&changed_history.tickets, &changed_history.staffing)).unwrap();
        changed_history.source_version_hashes =
            vec![format!("{:x}", Sha256::digest(changed_bytes))];
        let second =
            simulate_ticket_events(&changed_history, &model, &scenario, &config()).unwrap();
        assert_eq!(second.days, event.days);
        assert_eq!(second.wait_seconds_p50, event.wait_seconds_p50);
        assert_ne!(second.replay_hash, event.replay_hash);
    }

    #[test]
    fn unsupported_service_schedule_is_rejected() {
        assert_eq!(
            timestamp("2026-01-01T09:00:00.500Z"),
            Err(EventSimulationError::Invalid(
                "event timestamps must have whole-second precision"
            ))
        );
        let export = synthetic_support_export(47, 8).unwrap();
        let pilot = build_support_pilot(&export).unwrap();
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let mut scenario = pilot.baseline;
        scenario.fixed_extra_capacity_by_day[0] = 1;
        assert_eq!(
            simulate_ticket_events(&export, &model, &scenario, &config()),
            Err(EventSimulationError::Invalid(
                "fixed extra capacity has no ticket-level service schedule"
            ))
        );
    }
}
