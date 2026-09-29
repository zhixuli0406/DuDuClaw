//! Reproducible synthetic ticket export for engineering verification only.
//! It is not evidence that the queue model fits a real support operation.

use std::collections::VecDeque;

use chrono::{Duration, NaiveDate};
use sha2::{Digest, Sha256};

use crate::decision_ingest::{DailyStaffing, PilotImportError, SupportPilotExport, TicketEvent};

fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Generate a deliberately saturated 2-agent FIFO queue: 18–22 new tickets
/// and exactly 16 resolutions per day, plus 10 initially pending tickets.
/// The known service capacity is 8 tickets per agent per day.
pub fn synthetic_support_export(
    seed: u64,
    days: usize,
) -> Result<SupportPilotExport, PilotImportError> {
    if !(8..=366).contains(&days) {
        return Err(PilotImportError::Invalid("synthetic days must be 8..=366"));
    }
    let start = NaiveDate::from_ymd_opt(2026, 1, 1)
        .ok_or(PilotImportError::Invalid("invalid synthetic start date"))?;
    let mut state = seed;
    let mut tickets = Vec::new();
    let mut pending = VecDeque::new();
    for index in 0..10 {
        tickets.push(TicketEvent {
            queue_id: Some("synthetic-support-queue".into()),
            ticket_id: format!("initial-{index}"),
            created_at_utc: "2025-12-31T09:00:00Z".into(),
            resolved_at_utc: None,
        });
        pending.push_back(tickets.len() - 1);
    }
    let mut staffing = Vec::with_capacity(days);
    for day in 0..days {
        let date = start + Duration::days(day as i64);
        staffing.push(DailyStaffing {
            queue_id: Some("synthetic-support-queue".into()),
            day_utc: format!("{date}T00:00:00Z"),
            agents: 2,
            fixed_extra_capacity: 0,
        });
        let arrivals = 18 + next(&mut state) % 5;
        for index in 0..arrivals {
            tickets.push(TicketEvent {
                queue_id: Some("synthetic-support-queue".into()),
                ticket_id: format!("day-{day}-ticket-{index}"),
                created_at_utc: format!("{date}T09:00:00Z"),
                resolved_at_utc: None,
            });
            pending.push_back(tickets.len() - 1);
        }
        for _ in 0..16 {
            let index = pending.pop_front().ok_or(PilotImportError::Invalid(
                "synthetic queue unexpectedly empty",
            ))?;
            tickets[index].resolved_at_utc = Some(format!("{date}T20:00:00Z"));
        }
    }
    let cutoff = start + Duration::days(days as i64);
    let source_bytes = serde_json::to_vec(&(&tickets, &staffing))
        .map_err(|_| PilotImportError::Invalid("synthetic serialization failed"))?;
    let source_hash = format!("{:x}", Sha256::digest(source_bytes));
    Ok(SupportPilotExport {
        snapshot_id: format!("synthetic-support-{seed}-{days}"),
        baseline_scenario_id: "baseline-two-agents".into(),
        window_start_utc: format!("{start}T00:00:00Z"),
        data_cutoff_utc: format!("{cutoff}T00:00:00Z"),
        source_version_hashes: vec![source_hash],
        seed,
        horizon_days: days,
        tickets,
        staffing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_calibration::{backtest_capacity, fit_capacity};
    use crate::decision_ingest::build_support_pilot;
    use crate::decision_policy::{PolicyChoice, StaffingResourcePlan};
    use crate::decision_sensitivity::{BoundedCount, SensitivityPlan, simulate_sensitivity};
    use crate::decision_sim::{QueueModel, StaffingScenario, simulate};
    use crate::decision_store::{DecisionScope, DecisionStore, DecisionStoreError};

    #[test]
    fn synthetic_export_replays_end_to_end_and_keeps_scope() {
        let export = synthetic_support_export(47, 35).unwrap();
        assert_eq!(
            export.tickets,
            synthetic_support_export(47, 35).unwrap().tickets
        );
        let pilot = build_support_pilot(&export).unwrap();
        let fit = fit_capacity(&pilot.observed_days, 7).unwrap();
        assert_eq!(fit.service_per_agent_day, 8);
        let backtest = backtest_capacity(&pilot.observed_days, 14, 7).unwrap();
        assert_eq!(backtest.evaluation_days, 21);
        assert_eq!(backtest.model_abs_error_sum, 0);
        assert!(backtest.model_beats_both_baselines);

        let model = QueueModel {
            version: "synthetic-capacity-fit-v1".into(),
            service_capacity_per_agent_day: fit.service_per_agent_day,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 10_000,
        };
        let alternative = StaffingScenario {
            id: "alternative-three-agents".into(),
            agents_by_day: vec![3; 35],
            fixed_extra_capacity_by_day: vec![0; 35],
        };
        let baseline_result = simulate(&pilot.snapshot, &model, &pilot.baseline).unwrap();
        for (predicted, observed) in baseline_result.days.iter().zip(&pilot.observed_days) {
            assert_eq!(predicted.resolved, observed.resolved);
            assert_eq!(predicted.backlog_end, observed.backlog_end);
        }
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("synthetic-decisions.sqlite"));
        let scope = DecisionScope {
            tenant_id: "synthetic-test".into(),
            acl: "private".into(),
        };
        store.put_snapshot(&scope, &pilot.snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &pilot.baseline).unwrap();
        store.put_scenario(&scope, &alternative).unwrap();
        assert_eq!(
            store
                .replay(
                    &scope,
                    &pilot.snapshot.id,
                    &model.version,
                    &pilot.baseline.id
                )
                .unwrap()
                .replay_hash,
            baseline_result.replay_hash
        );
        let brief = store
            .compare_scenarios(
                &scope,
                &pilot.snapshot.id,
                &model.version,
                &pilot.baseline.id,
                &alternative.id,
                vec!["Synthetic fixed FIFO arrivals and service".into()],
            )
            .unwrap();
        assert_eq!(brief.status, "exploratory");
        assert!(brief.delta.final_backlog.parse::<i128>().unwrap() < 0);
        assert!(brief.delta.total_staff_cost_cents.parse::<i128>().unwrap() > 0);
        assert!(brief.uncertainty_interval.is_none());
        let plan = SensitivityPlan {
            runs: 100,
            daily_arrival_bands: pilot
                .snapshot
                .arrivals_by_day
                .iter()
                .map(|&arrivals| BoundedCount {
                    min: arrivals.saturating_sub(2),
                    max: arrivals + 2,
                })
                .collect(),
            service_capacity_band: BoundedCount { min: 7, max: 9 },
            max_final_backlog: 20,
            max_staff_cost_cents: 1_000_000,
        };
        let report = simulate_sensitivity(
            &pilot.snapshot,
            &model,
            &pilot.baseline,
            &alternative,
            &plan,
        )
        .unwrap();
        assert_eq!(report.status, "exploratory");
        assert!(report.delta_backlog_p95 < 0);
        let resource_plan = StaffingResourcePlan {
            available_agents_by_day: vec![3; 35],
            max_added_agents_per_day: 1,
            max_total_agent_days: 105,
            max_staff_cost_cents: 1_100_000,
            max_final_backlog: 20,
            service_capacity_band: BoundedCount { min: 7, max: 9 },
        };
        let policy = store
            .sweep_staffing_policy(
                &scope,
                &pilot.snapshot.id,
                &model.version,
                &pilot.baseline.id,
                &alternative.id,
                &resource_plan,
            )
            .unwrap();
        assert_eq!(policy.status, "exploratory");
        assert!(
            policy
                .points
                .iter()
                .any(|point| point.choice == PolicyChoice::Alternative)
        );
        let other = DecisionScope {
            tenant_id: "other-tenant".into(),
            acl: "private".into(),
        };
        assert!(matches!(
            store.replay(
                &other,
                &pilot.snapshot.id,
                &model.version,
                &pilot.baseline.id
            ),
            Err(DecisionStoreError::NotFound)
        ));
        store
            .revoke_source_version(&scope, &pilot.snapshot.source_version_hashes[0])
            .unwrap();
        assert!(matches!(
            store.replay(
                &scope,
                &pilot.snapshot.id,
                &model.version,
                &pilot.baseline.id
            ),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.sweep_staffing_policy(
                &scope,
                &pilot.snapshot.id,
                &model.version,
                &pilot.baseline.id,
                &alternative.id,
                &resource_plan,
            ),
            Err(DecisionStoreError::Revoked)
        ));
    }
}
