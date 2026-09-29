//! One-command, test-only C7 shadow validation. The simulated clock below is
//! passed only to DecisionStore's private `_at` methods from this cfg(test)
//! child module. Production public methods keep their wall-clock gates.

use super::*;
use crate::decision_shadow_screen::ShadowReviewCriteria;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::params;
use serde::Serialize;

const DAYS: usize = 21;
const START: &str = "2025-01-01T00:00:00Z";
const LINEAGE: &str = "c7-synthetic-queue";
const QUEUE: &str = "c7-synthetic-support";

fn utc(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[derive(Debug, Serialize)]
struct CaseSummary {
    id: &'static str,
    due_days: usize,
    aggregate_scored_days: usize,
    sla_scored_days: usize,
    corrected_days: usize,
    complete: bool,
    sla_complete: bool,
    aggregate_screen_eligible: bool,
    sla_screen_eligible: bool,
    aggregate_failed_checks: Vec<String>,
    sla_failed_checks: Vec<String>,
    observed_day_status: &'static str,
}

#[derive(Debug, Serialize)]
struct HarnessSummary {
    status: &'static str,
    synthetic_only: bool,
    test_only_virtual_clock: bool,
    forecast_committed_before_outcome: bool,
    run_id: &'static str,
    cases: Vec<CaseSummary>,
    limitations: Vec<&'static str>,
}

struct DayRecord {
    forecast_id: String,
    sla_forecast_id: String,
    aggregate_score_id: Option<String>,
    sla_score_id: Option<String>,
    training_artifact_id: String,
    observed: ObservedSupportDay,
    ticket_export: ShadowSlaObservationExport,
    end: DateTime<Utc>,
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: DecisionStore,
    causal: CausalStore,
    scope: DecisionScope,
    evidence: EvidenceScope,
    policy_id: String,
    end: DateTime<Utc>,
    days: Vec<DayRecord>,
}

fn source(
    causal: &CausalStore,
    evidence: &EvidenceScope,
    kind: &str,
    external_id: &str,
    content: &str,
    occurred_at: i64,
    ingested_at: i64,
) -> SourceArtifact {
    let artifact = causal
        .add_artifact(
            evidence,
            kind,
            external_id,
            "v1",
            LINEAGE,
            content,
            occurred_at,
            i64::MAX,
        )
        .unwrap();
    // Only the test fixture can stage historical ingestion. The actual
    // forecast/score methods below still enforce observed-at ordering.
    Connection::open(causal.path())
        .unwrap()
        .execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![ingested_at, artifact.id],
        )
        .unwrap();
    artifact
}

fn build_fixture(tenant: &str, missing_score_day: Option<usize>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let causal = CausalStore::new(dir.path().join("memory.db"));
    let store = DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
    let scope = DecisionScope {
        tenant_id: tenant.into(),
        acl: "private".into(),
    };
    let evidence = EvidenceScope {
        tenant_id: tenant.into(),
        acl: "private".into(),
    };
    let start = DateTime::parse_from_rfc3339(START)
        .unwrap()
        .with_timezone(&Utc);
    let end = start + Duration::days(DAYS as i64);
    let policy_id = format!("c7-policy-{tenant}");
    store
        .put_shadow_policy_at(
            &scope,
            &policy_id,
            LINEAGE,
            QUEUE,
            &utc(start),
            &utc(end),
            3_600,
            14,
            7,
            start.timestamp() - 86_400,
        )
        .unwrap();
    store
        .put_model(
            &scope,
            &QueueModel {
                version: "c7-synthetic-sla-model".into(),
                service_capacity_per_agent_day: 8,
                sla_days: 1_000,
                staff_cost_cents_per_agent_day: 100,
            },
        )
        .unwrap();
    let training_start = start - Duration::days(14);
    let mut backlog = 100_u64;
    let mut history = Vec::new();
    for _ in 0..14 {
        history.push(ObservedSupportDay {
            arrivals: 20,
            backlog_start: backlog,
            resolved: 16,
            backlog_end: backlog + 4,
            agents: 2,
            fixed_extra_capacity: 0,
        });
        backlog += 4;
    }
    let mut opening_tickets: Vec<ShadowSlaOpeningTicket> = (0..backlog)
        .map(|id| ShadowSlaOpeningTicket {
            ticket_id: format!("opening-{id}"),
            created_at_utc: utc(start - Duration::days(1)),
        })
        .collect();
    let mut prior_resolved: Vec<TicketEvent> = (0..7)
        .flat_map(|day| {
            (0..16).map(move |ticket| {
                let prior_day = start - Duration::days(7 - day);
                TicketEvent {
                    queue_id: Some(QUEUE.into()),
                    ticket_id: format!("prior-{day}-{ticket}"),
                    created_at_utc: utc(prior_day - Duration::days(1)),
                    resolved_at_utc: Some(utc(prior_day + Duration::seconds(300))),
                }
            })
        })
        .collect();
    let mut records = Vec::new();
    for index in 0..DAYS {
        let target = start + Duration::days(index as i64);
        let day_end = target + Duration::days(1);
        let agents: u32 = if index % 2 == 0 { 2 } else { 3 };
        let training = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some(QUEUE.into()),
            window_start_utc: utc(training_start),
            observed_through_utc: utc(target),
            observed_days: history.clone(),
        };
        let training_artifact = source(
            &causal,
            &evidence,
            "shadow_training_export",
            &format!("training-{index}"),
            &serde_json::to_string(&training).unwrap(),
            target.timestamp(),
            target.timestamp() + 60,
        );
        let target_outcomes_before_commit: i64 = Connection::open(causal.path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM causal_artifacts WHERE tenant_id=?1 AND acl=?2
                 AND kind IN ('shadow_observation_export','shadow_sla_observation_export')
                 AND occurred_at=?3",
                params![evidence.tenant_id, evidence.acl, day_end.timestamp()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(target_outcomes_before_commit, 0);
        let forecast = store
            .put_shadow_forecast_at(
                &scope,
                &format!("forecast-{index}"),
                &training_artifact.id,
                &utc(target),
                KnownDayInputs {
                    opening_backlog: backlog,
                    planned_agents: agents,
                    planned_fixed_extra_capacity: 0,
                },
                &policy_id,
                target.timestamp() + 120,
            )
            .unwrap();
        let mut ages = BTreeMap::<u32, u32>::new();
        for ticket in &opening_tickets {
            let created = DateTime::parse_from_rfc3339(&ticket.created_at_utc)
                .unwrap()
                .timestamp();
            let age = u32::try_from((target.timestamp() - created + 86_399) / 86_400).unwrap();
            *ages.entry(age).or_default() += 1;
        }
        let opening = ShadowSlaOpeningExport {
            inputs: KnownSlaDayInputs {
                target_day_utc: utc(target),
                queue_id: Some(QUEUE.into()),
                opening_cohorts: ages
                    .into_iter()
                    .map(|(age_days, count)| crate::decision_sim::InitialCohort { age_days, count })
                    .collect(),
                known: forecast.known.clone(),
            },
            opening_tickets: opening_tickets.clone(),
            prior_resolved_tickets: prior_resolved
                .iter()
                .filter(|ticket| {
                    DateTime::parse_from_rfc3339(ticket.resolved_at_utc.as_deref().unwrap())
                        .unwrap()
                        .timestamp()
                        >= target.timestamp() - 7 * 86_400
                })
                .cloned()
                .collect(),
        };
        let opening_artifact = source(
            &causal,
            &evidence,
            "shadow_sla_opening_export",
            &format!("opening-{index}"),
            &serde_json::to_string(&opening).unwrap(),
            target.timestamp(),
            target.timestamp() + 130,
        );
        let sla_forecast = store
            .put_shadow_sla_forecast_at(
                &scope,
                &format!("sla-forecast-{index}"),
                &forecast.id,
                "c7-synthetic-sla-model",
                &opening_artifact.id,
                target.timestamp() + 150,
            )
            .unwrap();
        assert!(forecast.committed_at < day_end.timestamp());
        assert!(sla_forecast.committed_at < day_end.timestamp());
        assert!(forecast.committed_at < sla_forecast.committed_at);

        let resolved = agents * 8;
        let observed = ObservedSupportDay {
            arrivals: 20,
            backlog_start: backlog,
            resolved,
            backlog_end: backlog + 20 - u64::from(resolved),
            agents,
            fixed_extra_capacity: 0,
        };
        let observation = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some(QUEUE.into()),
            window_start_utc: utc(target),
            observed_through_utc: utc(day_end),
            observed_days: vec![observed.clone()],
        };
        let observed_artifact = source(
            &causal,
            &evidence,
            "shadow_observation_export",
            &format!("observed-{index}"),
            &serde_json::to_string(&observation).unwrap(),
            day_end.timestamp(),
            day_end.timestamp() + 1,
        );
        let mut day_tickets: Vec<TicketEvent> = opening_tickets
            .iter()
            .enumerate()
            .map(|(ticket_index, ticket)| TicketEvent {
                queue_id: Some(QUEUE.into()),
                ticket_id: ticket.ticket_id.clone(),
                created_at_utc: ticket.created_at_utc.clone(),
                resolved_at_utc: (ticket_index < resolved as usize)
                    .then(|| utc(target + Duration::seconds(300))),
            })
            .collect();
        day_tickets.extend((0..20).map(|arrival| TicketEvent {
            queue_id: Some(QUEUE.into()),
            ticket_id: format!("arrival-{index}-{arrival}"),
            created_at_utc: utc(target + Duration::seconds(100)),
            resolved_at_utc: None,
        }));
        let ticket_export = ShadowSlaObservationExport {
            queue_id: QUEUE.into(),
            target_day_utc: utc(target),
            observed_through_utc: utc(day_end),
            tickets: day_tickets.clone(),
        };
        let ticket_artifact = source(
            &causal,
            &evidence,
            "shadow_sla_observation_export",
            &format!("tickets-{index}"),
            &serde_json::to_string(&ticket_export).unwrap(),
            day_end.timestamp(),
            day_end.timestamp() + 2,
        );
        let (aggregate_score_id, sla_score_id) = if Some(index) == missing_score_day {
            (None, None)
        } else {
            let score = store
                .put_shadow_score_at(
                    &scope,
                    &format!("score-{index}"),
                    &forecast.id,
                    &observed_artifact.id,
                    day_end.timestamp() + 3,
                )
                .unwrap();
            let sla_score = store
                .put_shadow_sla_score_at(
                    &scope,
                    &format!("sla-score-{index}"),
                    &sla_forecast.id,
                    &score.id,
                    &ticket_artifact.id,
                    day_end.timestamp() + 4,
                )
                .unwrap();
            (Some(score.id), Some(sla_score.id))
        };
        prior_resolved.extend(
            day_tickets
                .iter()
                .filter(|ticket| ticket.resolved_at_utc.is_some())
                .cloned(),
        );
        opening_tickets = day_tickets
            .into_iter()
            .filter(|ticket| ticket.resolved_at_utc.is_none())
            .map(|ticket| ShadowSlaOpeningTicket {
                ticket_id: ticket.ticket_id,
                created_at_utc: ticket.created_at_utc,
            })
            .collect();
        backlog = observed.backlog_end;
        history.push(observed.clone());
        records.push(DayRecord {
            forecast_id: forecast.id,
            sla_forecast_id: sla_forecast.id,
            aggregate_score_id,
            sla_score_id,
            training_artifact_id: training_artifact.id,
            observed,
            ticket_export,
            end: day_end,
        });
    }
    Fixture {
        _dir: dir,
        store,
        causal,
        scope,
        evidence,
        policy_id,
        end,
        days: records,
    }
}

fn case_summary(fixture: &Fixture, id: &'static str, observed_index: usize) -> CaseSummary {
    let assessed_at = fixture.end.timestamp() + 100;
    let aggregate = fixture
        .store
        .assess_shadow_policy_at(&fixture.scope, &fixture.policy_id, assessed_at)
        .unwrap();
    let sla = fixture
        .store
        .assess_shadow_sla_policy_at(&fixture.scope, &fixture.policy_id, assessed_at)
        .unwrap();
    let criteria = ShadowReviewCriteria {
        min_complete_days: DAYS,
        min_fixed_coverage_bps: 8_000,
    };
    let aggregate_screen =
        crate::decision_shadow_screen::screen_shadow_assessment(aggregate.clone(), &criteria)
            .unwrap();
    let sla_screen =
        crate::decision_sla_shadow_screen::screen_sla_shadow_assessment(sla.clone(), &criteria)
            .unwrap();
    let day_status = match aggregate.days[observed_index].status {
        ShadowDayStatus::Scored => "scored",
        ShadowDayStatus::Unscored => "unscored",
        ShadowDayStatus::ForecastRevoked => "forecast_revoked",
        _ => "other",
    };
    CaseSummary {
        id,
        due_days: aggregate.due_days,
        aggregate_scored_days: aggregate.scored_days,
        sla_scored_days: sla.scored_days,
        corrected_days: aggregate.corrected_days,
        complete: aggregate.complete,
        sla_complete: sla.complete,
        aggregate_screen_eligible: aggregate_screen.eligible_for_human_review,
        sla_screen_eligible: sla_screen.eligible_for_human_review,
        aggregate_failed_checks: aggregate_screen.failed_checks,
        sla_failed_checks: sla_screen.failed_checks,
        observed_day_status: day_status,
    }
}

#[test]
fn c7_synthetic_shadow_harness() {
    let complete = build_fixture("complete", None);
    let complete_case = case_summary(&complete, "complete", 20);
    assert_eq!(complete_case.due_days, DAYS);
    assert_eq!(complete_case.aggregate_scored_days, DAYS);
    assert_eq!(complete_case.sla_scored_days, DAYS);
    assert!(complete_case.complete && complete_case.sla_complete);
    assert!(complete_case.aggregate_screen_eligible);
    assert!(complete_case.sla_screen_eligible);
    let complete_monitor = complete
        .store
        .dashboard_shadow_monitor(&complete.scope, &complete.policy_id)
        .unwrap();
    assert_eq!(complete_monitor.aggregate.status_counts["scored"], DAYS);
    assert_eq!(complete_monitor.sla.status_counts["scored"], DAYS);
    assert_eq!(complete_monitor.aggregate.whole_window_skill, Some(true));
    assert_eq!(complete_monitor.sla.whole_window_skill, Some(true));
    assert!(complete_monitor.aggregate.fixed_interval_available);
    assert!(complete_monitor.sla.fixed_interval_available);
    let criteria = ShadowReviewCriteria {
        min_complete_days: DAYS,
        min_fixed_coverage_bps: 8_000,
    };
    let saved_aggregate_screen = complete
        .store
        .put_shadow_review_screen(&complete.scope, &complete.policy_id, &criteria)
        .unwrap();
    let saved_sla_screen = complete
        .store
        .put_sla_shadow_review_screen(&complete.scope, &complete.policy_id, &criteria)
        .unwrap();

    // A missing score remains in both denominators and suppresses full-window
    // skill and review eligibility; later days cannot conceal the gap.
    let missing = build_fixture("missing", Some(10));
    let missing_case = case_summary(&missing, "missing_day", 10);
    assert_eq!(missing_case.observed_day_status, "unscored");
    assert_eq!(missing_case.aggregate_scored_days, DAYS - 1);
    assert!(!missing_case.complete && !missing_case.sla_complete);
    assert!(!missing_case.aggregate_screen_eligible);
    assert!(!missing_case.sla_screen_eligible);
    assert!(
        missing_case
            .aggregate_failed_checks
            .contains(&"incomplete_due_days".into())
    );
    assert!(
        missing_case
            .sla_failed_checks
            .contains(&"incomplete_due_days".into())
    );
    let missing_monitor = missing
        .store
        .dashboard_shadow_monitor(&missing.scope, &missing.policy_id)
        .unwrap();
    assert_eq!(missing_monitor.aggregate.status_counts["unscored"], 1);
    assert_eq!(
        missing_monitor.sla.status_counts["aggregate_unavailable"],
        1
    );
    assert_eq!(missing_monitor.aggregate.whole_window_skill, None);
    assert_eq!(missing_monitor.sla.whole_window_skill, None);

    // Correct a later observed outcome, first aggregate then ticket source.
    // The temporary revision skew must fail the SLA window until the matching
    // ticket correction is committed.
    let assessment_time = chrono::Utc::now().timestamp();
    let before_correction = complete
        .store
        .assess_shadow_policy_at(&complete.scope, &complete.policy_id, assessment_time)
        .unwrap();
    let last = complete.days.last().unwrap();
    let corrected_observed = ObservedSupportDay {
        resolved: last.observed.resolved - 1,
        backlog_end: last.observed.backlog_end + 1,
        ..last.observed.clone()
    };
    let corrected_aggregate = ObservedOutcomeExport {
        sla_days: None,
        resolved_within_sla_by_day: None,
        queue_id: Some(QUEUE.into()),
        window_start_utc: utc(last.end - Duration::days(1)),
        observed_through_utc: utc(last.end),
        observed_days: vec![corrected_observed],
    };
    let corrected_observation = source(
        &complete.causal,
        &complete.evidence,
        "shadow_observation_export",
        "corrected-observed-20",
        &serde_json::to_string(&corrected_aggregate).unwrap(),
        last.end.timestamp(),
        last.end.timestamp() + 20,
    );
    let aggregate_correction = complete
        .store
        .put_shadow_score_correction_at(
            &complete.scope,
            "aggregate-correction-20",
            &last.forecast_id,
            last.aggregate_score_id.as_deref().unwrap(),
            &corrected_observation.id,
            "synthetic-reviewer",
            "one synthetic resolution was misclassified",
            last.end.timestamp() + 21,
        )
        .unwrap();
    let after_correction = complete
        .store
        .assess_shadow_policy_at(&complete.scope, &complete.policy_id, assessment_time)
        .unwrap();
    assert_eq!(
        before_correction
            .days
            .iter()
            .map(|day| day.status)
            .collect::<Vec<_>>(),
        after_correction
            .days
            .iter()
            .map(|day| day.status)
            .collect::<Vec<_>>()
    );
    assert_ne!(
        before_correction.days.last().unwrap().score_revision_sha256,
        after_correction.days.last().unwrap().score_revision_sha256
    );
    assert_ne!(before_correction, after_correction);
    let skew = case_summary(&complete, "temporary_revision_skew", 20);
    assert!(skew.complete && !skew.sla_complete);
    assert!(!skew.sla_screen_eligible);
    let skew_monitor = complete
        .store
        .dashboard_shadow_monitor(&complete.scope, &complete.policy_id)
        .unwrap();
    assert_eq!(skew_monitor.sla.status_counts["score_stale"], 1);
    assert_eq!(skew_monitor.sla.whole_window_skill, None);
    let mut corrected_tickets = last.ticket_export.clone();
    let last_resolved = corrected_tickets
        .tickets
        .iter_mut()
        .rfind(|ticket| ticket.resolved_at_utc.is_some())
        .unwrap();
    last_resolved.resolved_at_utc = None;
    let corrected_ticket_source = source(
        &complete.causal,
        &complete.evidence,
        "shadow_sla_observation_export",
        "corrected-tickets-20",
        &serde_json::to_string(&corrected_tickets).unwrap(),
        last.end.timestamp(),
        last.end.timestamp() + 22,
    );
    complete
        .store
        .put_shadow_sla_score_correction_at(
            &complete.scope,
            "sla-correction-20",
            &last.sla_forecast_id,
            last.sla_score_id.as_deref().unwrap(),
            &aggregate_correction.id,
            &corrected_ticket_source.id,
            "synthetic-reviewer",
            "match the corrected synthetic ticket outcome",
            last.end.timestamp() + 23,
        )
        .unwrap();
    let corrected_case = case_summary(&complete, "corrected", 20);
    assert!(corrected_case.complete && corrected_case.sla_complete);
    assert_eq!(corrected_case.corrected_days, 1);
    let corrected_monitor = complete
        .store
        .dashboard_shadow_monitor(&complete.scope, &complete.policy_id)
        .unwrap();
    assert_eq!(corrected_monitor.aggregate.corrected_days, 1);
    assert_eq!(corrected_monitor.sla.corrected_days, 1);
    assert_eq!(corrected_monitor.sla.status_counts["scored"], DAYS);
    assert_eq!(
        complete
            .store
            .load_shadow_review_screen(&complete.scope, &saved_aggregate_screen.replay_hash)
            .unwrap(),
        saved_aggregate_screen
    );
    assert_eq!(
        complete
            .store
            .load_sla_shadow_review_screen(&complete.scope, &saved_sla_screen.replay_hash)
            .unwrap(),
        saved_sla_screen
    );
    assert_ne!(
        complete
            .store
            .screen_shadow_policy(&complete.scope, &complete.policy_id, &criteria)
            .unwrap()
            .replay_hash,
        saved_aggregate_screen.replay_hash
    );
    assert_ne!(
        complete
            .store
            .screen_sla_shadow_policy(&complete.scope, &complete.policy_id, &criteria)
            .unwrap()
            .replay_hash,
        saved_sla_screen.replay_hash
    );

    // A real source cascade, not a fabricated assessment flag, revokes the
    // first forecast and removes complete-window skill from both screens.
    complete
        .store
        .remove_causal_artifact_with_dependents(
            &complete.scope,
            &complete.days[0].training_artifact_id,
            CausalSourceRemoval::Erase,
            Some(&complete._dir.path().join("ccr.db")),
        )
        .unwrap();
    let revoked_case = case_summary(&complete, "source_revoked", 0);
    assert_eq!(revoked_case.observed_day_status, "forecast_revoked");
    assert!(!revoked_case.complete && !revoked_case.sla_complete);
    assert!(!revoked_case.aggregate_screen_eligible);
    assert!(!revoked_case.sla_screen_eligible);
    let revoked_monitor = complete
        .store
        .dashboard_shadow_monitor(&complete.scope, &complete.policy_id)
        .unwrap();
    assert_eq!(
        revoked_monitor.aggregate.status_counts["forecast_revoked"],
        1
    );
    assert_eq!(
        revoked_monitor.sla.status_counts["aggregate_unavailable"],
        1
    );
    assert!(matches!(
        complete
            .store
            .load_shadow_review_screen(&complete.scope, &saved_aggregate_screen.replay_hash),
        Err(DecisionStoreError::Revoked)
    ));
    assert!(matches!(
        complete
            .store
            .load_sla_shadow_review_screen(&complete.scope, &saved_sla_screen.replay_hash),
        Err(DecisionStoreError::Revoked)
    ));

    let summary = HarnessSummary {
        status: "synthetic_test_only",
        synthetic_only: true,
        test_only_virtual_clock: true,
        forecast_committed_before_outcome: true,
        run_id: "c7-synthetic-shadow-v1",
        cases: vec![
            complete_case,
            missing_case,
            skew,
            corrected_case,
            revoked_case,
        ],
        limitations: vec![
            "Synthetic FIFO service and ticket identities are deterministic assumptions.",
            "Historical timestamps are staged only in this cfg(test) fixture; production time gates remain active.",
            "A passing review screen permits human inspection, not model promotion or staffing action.",
            "No real authenticated exports, calibrated uncertainty, or measured intervention are supplied.",
        ],
    };
    println!(
        "C7_SYNTHETIC_SHADOW_SUMMARY={}",
        serde_json::to_string(&summary).unwrap()
    );
}

/// The final day of a seeded ticket-SLA window, handed to the Dashboard tests
/// so they can drive the production create paths instead of the `_at` seams.
#[allow(dead_code)]
pub(crate) struct SlaDashboardSeed {
    pub(crate) policy_id: String,
    pub(crate) model_version: String,
    pub(crate) last_forecast_id: String,
    pub(crate) last_sla_forecast_id: String,
    pub(crate) last_aggregate_score_id: String,
    pub(crate) last_opening_json: String,
    pub(crate) last_observation_json: String,
}

fn retained_source(
    causal: &CausalStore,
    evidence: &EvidenceScope,
    lineage: &str,
    kind: &str,
    external_id: &str,
    content: &str,
    occurred_at: i64,
    ingested_at: i64,
    retention_at: i64,
) -> SourceArtifact {
    let artifact = causal
        .add_artifact(
            evidence,
            kind,
            external_id,
            "v1",
            lineage,
            content,
            occurred_at,
            retention_at,
        )
        .unwrap();
    // Only this cfg(test) fixture stages historical ingestion. The forecast and
    // score methods below still enforce observed-at ordering.
    Connection::open(causal.path())
        .unwrap()
        .execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![ingested_at, artifact.id],
        )
        .unwrap();
    artifact
}

/// Seed one complete 21-day synthetic backlog and ticket-SLA window into a
/// caller-owned store. Every day carries an aggregate forecast, aggregate
/// score, and SLA forecast; the final day deliberately has no SLA score so a
/// Dashboard test can commit it through the production wall-clock path.
///
/// Sources are retained until `retention_at` rather than forever so an exact
/// inline retry can restate the same retention deadline.
pub(crate) fn seed_sla_dashboard_window(
    store: &DecisionStore,
    causal: &CausalStore,
    scope: &DecisionScope,
    evidence: &EvidenceScope,
    policy_id: &str,
    retention_at: i64,
) -> SlaDashboardSeed {
    let lineage = format!("{policy_id}-lineage");
    let queue = format!("{policy_id}-queue");
    let model_version = format!("{policy_id}-model");
    let start = DateTime::parse_from_rfc3339(START)
        .unwrap()
        .with_timezone(&Utc);
    let end = start + Duration::days(DAYS as i64);
    store
        .put_shadow_policy_at(
            scope,
            policy_id,
            &lineage,
            &queue,
            &utc(start),
            &utc(end),
            3_600,
            14,
            7,
            start.timestamp() - 86_400,
        )
        .unwrap();
    store
        .put_model(
            scope,
            &QueueModel {
                version: model_version.clone(),
                service_capacity_per_agent_day: 8,
                sla_days: 1_000,
                staff_cost_cents_per_agent_day: 100,
            },
        )
        .unwrap();
    let training_start = start - Duration::days(14);
    let mut backlog = 100_u64;
    let mut history = Vec::new();
    for _ in 0..14 {
        history.push(ObservedSupportDay {
            arrivals: 20,
            backlog_start: backlog,
            resolved: 16,
            backlog_end: backlog + 4,
            agents: 2,
            fixed_extra_capacity: 0,
        });
        backlog += 4;
    }
    let mut opening_tickets: Vec<ShadowSlaOpeningTicket> = (0..backlog)
        .map(|id| ShadowSlaOpeningTicket {
            ticket_id: format!("opening-{id}"),
            created_at_utc: utc(start - Duration::days(1)),
        })
        .collect();
    let mut prior_resolved: Vec<TicketEvent> = (0..7)
        .flat_map(|day| {
            let queue = queue.clone();
            (0..16).map(move |ticket| {
                let prior_day = start - Duration::days(7 - day);
                TicketEvent {
                    queue_id: Some(queue.clone()),
                    ticket_id: format!("prior-{day}-{ticket}"),
                    created_at_utc: utc(prior_day - Duration::days(1)),
                    resolved_at_utc: Some(utc(prior_day + Duration::seconds(300))),
                }
            })
        })
        .collect();
    let mut seed = SlaDashboardSeed {
        policy_id: policy_id.to_owned(),
        model_version: model_version.clone(),
        last_forecast_id: String::new(),
        last_sla_forecast_id: String::new(),
        last_aggregate_score_id: String::new(),
        last_opening_json: String::new(),
        last_observation_json: String::new(),
    };
    for index in 0..DAYS {
        let target = start + Duration::days(index as i64);
        let day_end = target + Duration::days(1);
        let agents: u32 = if index % 2 == 0 { 2 } else { 3 };
        let training = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some(queue.clone()),
            window_start_utc: utc(training_start),
            observed_through_utc: utc(target),
            observed_days: history.clone(),
        };
        let training_artifact = retained_source(
            causal,
            evidence,
            &lineage,
            "shadow_training_export",
            &format!("training-{index}"),
            &serde_json::to_string(&training).unwrap(),
            target.timestamp(),
            target.timestamp() + 60,
            retention_at,
        );
        let forecast = store
            .put_shadow_forecast_at(
                scope,
                &format!("forecast-{index}"),
                &training_artifact.id,
                &utc(target),
                KnownDayInputs {
                    opening_backlog: backlog,
                    planned_agents: agents,
                    planned_fixed_extra_capacity: 0,
                },
                policy_id,
                target.timestamp() + 120,
            )
            .unwrap();
        let mut ages = BTreeMap::<u32, u32>::new();
        for ticket in &opening_tickets {
            let created = DateTime::parse_from_rfc3339(&ticket.created_at_utc)
                .unwrap()
                .timestamp();
            let age = u32::try_from((target.timestamp() - created + 86_399) / 86_400).unwrap();
            *ages.entry(age).or_default() += 1;
        }
        let opening = ShadowSlaOpeningExport {
            inputs: KnownSlaDayInputs {
                target_day_utc: utc(target),
                queue_id: Some(queue.clone()),
                opening_cohorts: ages
                    .into_iter()
                    .map(|(age_days, count)| crate::decision_sim::InitialCohort { age_days, count })
                    .collect(),
                known: forecast.known.clone(),
            },
            opening_tickets: opening_tickets.clone(),
            prior_resolved_tickets: prior_resolved
                .iter()
                .filter(|ticket| {
                    DateTime::parse_from_rfc3339(ticket.resolved_at_utc.as_deref().unwrap())
                        .unwrap()
                        .timestamp()
                        >= target.timestamp() - 7 * 86_400
                })
                .cloned()
                .collect(),
        };
        let opening_json = serde_json::to_string(&opening).unwrap();
        let opening_artifact = retained_source(
            causal,
            evidence,
            &lineage,
            "shadow_sla_opening_export",
            &format!("opening-{index}"),
            &opening_json,
            target.timestamp(),
            target.timestamp() + 130,
            retention_at,
        );
        let sla_forecast = store
            .put_shadow_sla_forecast_at(
                scope,
                &format!("sla-forecast-{index}"),
                &forecast.id,
                &model_version,
                &opening_artifact.id,
                target.timestamp() + 150,
            )
            .unwrap();
        let resolved = agents * 8;
        let observed = ObservedSupportDay {
            arrivals: 20,
            backlog_start: backlog,
            resolved,
            backlog_end: backlog + 20 - u64::from(resolved),
            agents,
            fixed_extra_capacity: 0,
        };
        let observation = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some(queue.clone()),
            window_start_utc: utc(target),
            observed_through_utc: utc(day_end),
            observed_days: vec![observed.clone()],
        };
        let observed_artifact = retained_source(
            causal,
            evidence,
            &lineage,
            "shadow_observation_export",
            &format!("observed-{index}"),
            &serde_json::to_string(&observation).unwrap(),
            day_end.timestamp(),
            day_end.timestamp() + 1,
            retention_at,
        );
        let mut day_tickets: Vec<TicketEvent> = opening_tickets
            .iter()
            .enumerate()
            .map(|(ticket_index, ticket)| TicketEvent {
                queue_id: Some(queue.clone()),
                ticket_id: ticket.ticket_id.clone(),
                created_at_utc: ticket.created_at_utc.clone(),
                resolved_at_utc: (ticket_index < resolved as usize)
                    .then(|| utc(target + Duration::seconds(300))),
            })
            .collect();
        day_tickets.extend((0..20).map(|arrival| TicketEvent {
            queue_id: Some(queue.clone()),
            ticket_id: format!("arrival-{index}-{arrival}"),
            created_at_utc: utc(target + Duration::seconds(100)),
            resolved_at_utc: None,
        }));
        let ticket_export = ShadowSlaObservationExport {
            queue_id: queue.clone(),
            target_day_utc: utc(target),
            observed_through_utc: utc(day_end),
            tickets: day_tickets.clone(),
        };
        let observation_json = serde_json::to_string(&ticket_export).unwrap();
        let score = store
            .put_shadow_score_at(
                scope,
                &format!("score-{index}"),
                &forecast.id,
                &observed_artifact.id,
                day_end.timestamp() + 3,
            )
            .unwrap();
        if index + 1 < DAYS {
            let ticket_artifact = retained_source(
                causal,
                evidence,
                &lineage,
                "shadow_sla_observation_export",
                &format!("tickets-{index}"),
                &observation_json,
                day_end.timestamp(),
                day_end.timestamp() + 2,
                retention_at,
            );
            store
                .put_shadow_sla_score_at(
                    scope,
                    &format!("sla-score-{index}"),
                    &sla_forecast.id,
                    &score.id,
                    &ticket_artifact.id,
                    day_end.timestamp() + 4,
                )
                .unwrap();
        } else {
            seed.last_forecast_id = forecast.id.clone();
            seed.last_sla_forecast_id = sla_forecast.id.clone();
            seed.last_aggregate_score_id = score.id.clone();
            seed.last_opening_json = opening_json;
            seed.last_observation_json = observation_json;
        }
        prior_resolved.extend(
            day_tickets
                .iter()
                .filter(|ticket| ticket.resolved_at_utc.is_some())
                .cloned(),
        );
        opening_tickets = day_tickets
            .into_iter()
            .filter(|ticket| ticket.resolved_at_utc.is_none())
            .map(|ticket| ShadowSlaOpeningTicket {
                ticket_id: ticket.ticket_id,
                created_at_utc: ticket.created_at_utc,
            })
            .collect();
        backlog = observed.backlog_end;
        history.push(observed);
    }
    seed
}
