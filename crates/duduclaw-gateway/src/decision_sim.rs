//! Deterministic, read-only customer-support queue simulation.
//!
//! This is a numerical transition engine, not an LLM judgment. Every scenario
//! consumes the same immutable demand snapshot so staffing policies can be
//! compared without changing the external arrivals between branches.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_DAYS: usize = 366;
const MAX_DAILY_TICKETS: u32 = 1_000_000;
const ENGINE_SCHEMA: &str = "support-queue-v2";

/// Digest of the numeric transition implementation shipped in this binary.
/// It changes when the engine source changes, even if callers reuse old IDs.
pub fn engine_code_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_str!("decision_sim.rs").as_bytes())
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialCohort {
    pub age_days: u32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionSnapshot {
    pub id: String,
    /// Row-verified queue identity for new pilot exports. Old snapshots omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub data_cutoff_utc: String,
    pub source_version_hashes: Vec<String>,
    pub seed: u64,
    pub arrivals_by_day: Vec<u32>,
    pub initial_backlog: Vec<InitialCohort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueModel {
    pub version: String,
    pub service_capacity_per_agent_day: u32,
    pub sla_days: u32,
    pub staff_cost_cents_per_agent_day: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaffingScenario {
    pub id: String,
    pub agents_by_day: Vec<u32>,
    pub fixed_extra_capacity_by_day: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulatedDay {
    pub day: usize,
    pub arrivals: u32,
    pub resolved: u32,
    pub backlog_end: u64,
    /// Pending tickets already older than the configured SLA at day's end.
    pub overdue_pending: u64,
    pub resolved_within_sla: u32,
    pub staff_cost_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationResult {
    pub snapshot_id: String,
    pub scenario_id: String,
    pub model_version: String,
    pub engine_sha256: String,
    /// Digest of the exact snapshot, model, scenario and engine schema.
    pub replay_hash: String,
    pub days: Vec<SimulatedDay>,
    pub total_arrivals: u64,
    pub total_resolved: u64,
    pub final_backlog: u64,
    pub total_resolved_within_sla: u64,
    pub total_staff_cost_cents: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SimulationError {
    #[error("invalid simulation input: {0}")]
    Invalid(&'static str),
    #[error("simulation arithmetic overflow")]
    Overflow,
    #[error("simulation serialization failed")]
    Serialization,
}

fn add(a: u64, b: u64) -> Result<u64, SimulationError> {
    a.checked_add(b).ok_or(SimulationError::Overflow)
}

/// Run one staffing policy on an immutable demand snapshot. The queue is
/// stored in arrival-day cohorts, so it remains efficient for large volumes.
pub fn simulate(
    snapshot: &DecisionSnapshot,
    model: &QueueModel,
    scenario: &StaffingScenario,
) -> Result<SimulationResult, SimulationError> {
    simulate_with_optional_capacity_path(snapshot, model, scenario, None)
}

/// Apply one exogenous per-agent capacity path to a scenario. Callers must
/// pass the same path to each policy branch for a paired comparison.
pub fn simulate_with_capacity_path(
    snapshot: &DecisionSnapshot,
    model: &QueueModel,
    scenario: &StaffingScenario,
    capacity_by_day: &[u32],
) -> Result<SimulationResult, SimulationError> {
    simulate_with_optional_capacity_path(snapshot, model, scenario, Some(capacity_by_day))
}

fn simulate_with_optional_capacity_path(
    snapshot: &DecisionSnapshot,
    model: &QueueModel,
    scenario: &StaffingScenario,
    capacity_by_day: Option<&[u32]>,
) -> Result<SimulationResult, SimulationError> {
    let horizon = snapshot.arrivals_by_day.len();
    if snapshot.id.trim().is_empty()
        || model.version.trim().is_empty()
        || scenario.id.trim().is_empty()
        || snapshot.data_cutoff_utc.trim().is_empty()
        || snapshot.source_version_hashes.is_empty()
        || snapshot
            .source_version_hashes
            .iter()
            .any(|hash| hash.trim().is_empty())
    {
        return Err(SimulationError::Invalid(
            "missing identity or source versions",
        ));
    }
    if chrono::DateTime::parse_from_rfc3339(&snapshot.data_cutoff_utc)
        .map(|time| time.offset().local_minus_utc() != 0)
        .unwrap_or(true)
    {
        return Err(SimulationError::Invalid(
            "data cutoff must be an RFC3339 UTC time",
        ));
    }
    if horizon == 0
        || horizon > MAX_DAYS
        || scenario.agents_by_day.len() != horizon
        || scenario.fixed_extra_capacity_by_day.len() != horizon
    {
        return Err(SimulationError::Invalid(
            "scenario horizon differs from demand snapshot",
        ));
    }
    if snapshot
        .queue_id
        .as_deref()
        .is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 128)
    {
        return Err(SimulationError::Invalid("invalid queue ID"));
    }
    if model.service_capacity_per_agent_day == 0 || model.sla_days == 0 {
        return Err(SimulationError::Invalid(
            "capacity and SLA must be positive",
        ));
    }
    if capacity_by_day.is_some_and(|path| {
        path.len() != horizon
            || path
                .iter()
                .any(|capacity| *capacity == 0 || *capacity > MAX_DAILY_TICKETS)
    }) {
        return Err(SimulationError::Invalid(
            "capacity path must be positive and match horizon",
        ));
    }
    if snapshot.initial_backlog.len() > 3_660
        || snapshot
            .arrivals_by_day
            .iter()
            .any(|n| *n > MAX_DAILY_TICKETS)
        || snapshot
            .initial_backlog
            .iter()
            .any(|c| c.count > MAX_DAILY_TICKETS)
    {
        return Err(SimulationError::Invalid(
            "ticket volume exceeds supported bound",
        ));
    }

    let engine_sha256 = engine_code_sha256();
    let bytes = match capacity_by_day {
        Some(path) => serde_json::to_vec(&(
            "support-queue-capacity-path-v1",
            ENGINE_SCHEMA,
            &engine_sha256,
            snapshot,
            model,
            scenario,
            path,
        )),
        None => serde_json::to_vec(&(ENGINE_SCHEMA, &engine_sha256, snapshot, model, scenario)),
    }
    .map_err(|_| SimulationError::Serialization)?;
    let replay_hash = format!("{:x}", Sha256::digest(bytes));

    let mut queue: VecDeque<(i64, u64)> = VecDeque::new();
    let mut initial = snapshot.initial_backlog.clone();
    initial.sort_by(|a, b| b.age_days.cmp(&a.age_days));
    let mut initial_count = 0_u64;
    for cohort in initial {
        if cohort.count == 0 {
            continue;
        }
        initial_count = add(initial_count, cohort.count as u64)?;
        queue.push_back((-(cohort.age_days as i64), cohort.count as u64));
    }

    let mut backlog = initial_count;
    let mut total_arrivals = 0_u64;
    let mut total_resolved = 0_u64;
    let mut total_resolved_within_sla = 0_u64;
    let mut total_staff_cost_cents = 0_u64;
    let mut days = Vec::with_capacity(horizon);

    for day in 0..horizon {
        let arrivals = snapshot.arrivals_by_day[day] as u64;
        total_arrivals = add(total_arrivals, arrivals)?;
        backlog = add(backlog, arrivals)?;
        if arrivals > 0 {
            queue.push_back((day as i64, arrivals));
        }

        let per_agent_capacity = capacity_by_day
            .map(|path| path[day])
            .unwrap_or(model.service_capacity_per_agent_day);
        let staffed = (scenario.agents_by_day[day] as u64)
            .checked_mul(per_agent_capacity as u64)
            .ok_or(SimulationError::Overflow)?;
        let capacity = add(staffed, scenario.fixed_extra_capacity_by_day[day] as u64)?;
        let mut remaining = capacity.min(backlog);
        let resolved = remaining;
        let mut within_sla = 0_u64;
        while remaining > 0 {
            let Some((arrived, count)) = queue.pop_front() else {
                return Err(SimulationError::Invalid("queue conservation violated"));
            };
            let taken = count.min(remaining);
            if (day as i64) - arrived < model.sla_days as i64 {
                within_sla = add(within_sla, taken)?;
            }
            remaining -= taken;
            if taken < count {
                queue.push_front((arrived, count - taken));
            }
        }
        backlog -= resolved;
        total_resolved = add(total_resolved, resolved)?;
        total_resolved_within_sla = add(total_resolved_within_sla, within_sla)?;

        let mut overdue_pending = 0_u64;
        for (arrived, count) in &queue {
            if (day as i64) - *arrived >= model.sla_days as i64 {
                overdue_pending = add(overdue_pending, *count)?;
            }
        }
        let cost = (scenario.agents_by_day[day] as u64)
            .checked_mul(model.staff_cost_cents_per_agent_day)
            .ok_or(SimulationError::Overflow)?;
        total_staff_cost_cents = add(total_staff_cost_cents, cost)?;
        days.push(SimulatedDay {
            day,
            arrivals: snapshot.arrivals_by_day[day],
            resolved: resolved.try_into().map_err(|_| SimulationError::Overflow)?,
            backlog_end: backlog,
            overdue_pending,
            resolved_within_sla: within_sla
                .try_into()
                .map_err(|_| SimulationError::Overflow)?,
            staff_cost_cents: cost,
        });
    }
    let expected_backlog = add(initial_count, total_arrivals)?
        .checked_sub(total_resolved)
        .ok_or(SimulationError::Overflow)?;
    if backlog != expected_backlog {
        return Err(SimulationError::Invalid(
            "final stock conservation violated",
        ));
    }
    Ok(SimulationResult {
        snapshot_id: snapshot.id.clone(),
        scenario_id: scenario.id.clone(),
        model_version: model.version.clone(),
        engine_sha256,
        replay_hash,
        days,
        total_arrivals,
        total_resolved,
        final_backlog: backlog,
        total_resolved_within_sla,
        total_staff_cost_cents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> DecisionSnapshot {
        DecisionSnapshot {
            id: "snap-1".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["tickets-v1".into()],
            seed: 7,
            arrivals_by_day: vec![5, 5, 5],
            initial_backlog: vec![InitialCohort {
                age_days: 2,
                count: 3,
            }],
        }
    }

    fn model() -> QueueModel {
        QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 3,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 10_000,
        }
    }

    #[test]
    fn replay_and_stock_conservation_are_deterministic() {
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1, 1, 1],
            fixed_extra_capacity_by_day: vec![0, 0, 0],
        };
        let a = simulate(&snapshot(), &model(), &scenario).unwrap();
        let b = simulate(&snapshot(), &model(), &scenario).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.final_backlog, 3 + 15 - a.total_resolved);
        assert_eq!(a.days[0].overdue_pending, 0);
    }

    #[test]
    fn extra_capacity_uses_same_demand_and_reduces_backlog() {
        let base = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1, 1, 1],
            fixed_extra_capacity_by_day: vec![0, 0, 0],
        };
        let hire = StaffingScenario {
            id: "hire".into(),
            agents_by_day: vec![2, 2, 2],
            fixed_extra_capacity_by_day: vec![0, 0, 0],
        };
        let a = simulate(&snapshot(), &model(), &base).unwrap();
        let b = simulate(&snapshot(), &model(), &hire).unwrap();
        assert_eq!(a.total_arrivals, b.total_arrivals);
        assert!(b.final_backlog < a.final_backlog);
        assert!(b.total_staff_cost_cents > a.total_staff_cost_cents);
        assert_ne!(a.replay_hash, b.replay_hash);
    }

    #[test]
    fn daily_capacity_path_changes_resolution_and_replay_identity() {
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1, 1, 1],
            fixed_extra_capacity_by_day: vec![0, 0, 0],
        };
        let path = [1, 3, 5];
        let a = simulate_with_capacity_path(&snapshot(), &model(), &scenario, &path).unwrap();
        let b = simulate_with_capacity_path(&snapshot(), &model(), &scenario, &path).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.days.iter().map(|day| day.resolved).collect::<Vec<_>>(),
            vec![1, 3, 5]
        );
        assert_eq!(a.final_backlog, 3 + 15 - a.total_resolved);
        assert_ne!(
            a.replay_hash,
            simulate(&snapshot(), &model(), &scenario)
                .unwrap()
                .replay_hash
        );
        assert!(matches!(
            simulate_with_capacity_path(&snapshot(), &model(), &scenario, &[1, 0, 5]),
            Err(SimulationError::Invalid(_))
        ));
    }

    #[test]
    fn bad_horizon_is_rejected() {
        let bad = StaffingScenario {
            id: "bad".into(),
            agents_by_day: vec![1],
            fixed_extra_capacity_by_day: vec![0],
        };
        assert_eq!(
            simulate(&snapshot(), &model(), &bad),
            Err(SimulationError::Invalid(
                "scenario horizon differs from demand snapshot"
            ))
        );
    }

    #[test]
    fn invalid_cutoff_and_empty_source_version_are_rejected() {
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1, 1, 1],
            fixed_extra_capacity_by_day: vec![0, 0, 0],
        };
        let mut bad = snapshot();
        bad.data_cutoff_utc = "yesterday".into();
        assert_eq!(
            simulate(&bad, &model(), &scenario),
            Err(SimulationError::Invalid(
                "data cutoff must be an RFC3339 UTC time"
            ))
        );
        bad = snapshot();
        bad.source_version_hashes = vec!["".into()];
        assert_eq!(
            simulate(&bad, &model(), &scenario),
            Err(SimulationError::Invalid(
                "missing identity or source versions"
            ))
        );
    }
}
