use super::*;

/// Prediction captured while its engine version is available. The primary
/// input ID is the replay hash, so a later observation can name it directly.
/// Loading validates the recorded input digests only; an engine upgrade does
/// not silently recompute or rewrite this historical result, so a reader that
/// needs "is this still reproducible by current code?" must ask for it through
/// [`DecisionStore::load_daily_run_with_engine_state`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDailyRun {
    pub replay_hash: String,
    pub snapshot_id: String,
    pub snapshot_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub model_version: String,
    pub model_sha256: String,
    pub scenario_id: String,
    pub scenario_sha256: String,
    pub result: SimulationResult,
}

/// A stored historical run plus whether current engine code still carries the
/// engine identity that produced it. The stored result is never recomputed on
/// load, so this flag is the only signal a reader gets that the row came from
/// an older engine. It is not part of any hashed payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedRun<T> {
    pub run: T,
    pub engine_matches_current: bool,
}

/// Immutable ticket-event prediction captured with exact input digests.
/// Loading requires the original export bytes; an engine upgrade does not
/// silently recompute or rewrite this historical result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEventRun {
    pub replay_hash: String,
    pub daily_engine_sha256: String,
    pub snapshot_id: String,
    pub snapshot_sha256: String,
    pub source_version_hashes: Vec<String>,
    pub source_sha256: String,
    pub model_version: String,
    pub model_sha256: String,
    pub scenario_id: String,
    pub scenario_sha256: String,
    pub baseline_scenario_id: String,
    pub baseline_scenario_sha256: String,
    pub window_start_utc: String,
    pub config: EventQueueConfig,
    pub result: EventSimulationResult,
}

/// Exact observed day stocks imported after the forecast cutoff. The source
/// bytes are hashed at ingest; they must come from a separately controlled
/// export, not a model-generated scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedOutcomeExport {
    /// Exact queue label for prospective exports; legacy observations omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub window_start_utc: String,
    pub observed_through_utc: String,
    pub observed_days: Vec<ObservedSupportDay>,
    /// Optional complete daily counts, supplied by the observation producer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_within_sla_by_day: Option<Vec<u32>>,
    /// The day-bucket SLA threshold used to produce the optional counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sla_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeAssessment {
    /// Numerators of daily mean absolute errors; divide by evaluated_days.
    pub evaluated_days: usize,
    pub arrivals_abs_error_sum: u128,
    pub resolved_abs_error_sum: u128,
    pub backlog_abs_error_sum: u128,
    /// Present only when every observed day has an SLA resolution count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_within_sla_abs_error_sum: Option<u128>,
    pub predicted_final_backlog: u64,
    pub observed_final_backlog: u64,
}

pub(super) fn assess_observed_days(
    predicted: &SimulationResult,
    observed: &[ObservedSupportDay],
    sla_labels: Option<&[u32]>,
) -> OutcomeAssessment {
    OutcomeAssessment {
        evaluated_days: predicted.days.len(),
        arrivals_abs_error_sum: predicted
            .days
            .iter()
            .zip(observed)
            .map(|(day, observed)| day.arrivals.abs_diff(observed.arrivals) as u128)
            .sum(),
        resolved_abs_error_sum: predicted
            .days
            .iter()
            .zip(observed)
            .map(|(day, observed)| day.resolved.abs_diff(observed.resolved) as u128)
            .sum(),
        backlog_abs_error_sum: predicted
            .days
            .iter()
            .zip(observed)
            .map(|(day, observed)| day.backlog_end.abs_diff(observed.backlog_end) as u128)
            .sum(),
        resolved_within_sla_abs_error_sum: sla_labels.map(|labels| {
            predicted
                .days
                .iter()
                .zip(labels)
                .map(|(day, actual)| u128::from(day.resolved_within_sla.abs_diff(*actual)))
                .sum()
        }),
        predicted_final_backlog: predicted.final_backlog,
        observed_final_backlog: observed
            .last()
            .expect("validated nonempty observation")
            .backlog_end,
    }
}

/// Immutable observation linked to the exact simulation the operator saw.
/// This is a forecast diagnostic, not an intervention effect estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredObservedOutcome {
    pub id: String,
    pub snapshot_id: String,
    /// Exact queue asserted by the source export, absent in legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub model_version: String,
    pub scenario_id: String,
    pub replay_hash: String,
    pub recorded_by: String,
    /// Local SQLite creation time of the frozen run, not an external timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_run_created_at_unix: Option<i64>,
    /// Whether that local run existed before the first observed UTC day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_run_precedes_window: Option<bool>,
    pub observed_source_sha256: String,
    pub window_start_utc: String,
    pub observed_through_utc: String,
    /// Immutable daily aggregates retained for later calibration versions.
    pub observed_days: Vec<ObservedSupportDay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_within_sla_by_day: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sla_days: Option<u32>,
    /// Exact ticket-source bytes were checked at ingest; old journals omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_label_engine_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_source_retention_until_utc: Option<String>,
    pub assessment: OutcomeAssessment,
}

pub(super) fn valid_observed_sla_labels(
    observed: &[ObservedSupportDay],
    labels: Option<&[u32]>,
    sla_days: Option<u32>,
) -> bool {
    match (labels, sla_days) {
        (None, None) => true,
        (Some(labels), Some(days)) => {
            days > 0
                && labels.len() == observed.len()
                && labels
                    .iter()
                    .zip(observed)
                    .all(|(within_sla, day)| *within_sla <= day.resolved)
        }
        _ => false,
    }
}

pub(super) fn verify_ticket_source_observation(
    ticket_source_bytes: &[u8],
    observed: &ObservedOutcomeExport,
    snapshot_id: &str,
    scenario_id: &str,
    sla_days: u32,
) -> Result<(), DecisionStoreError> {
    if ticket_source_bytes.is_empty() || ticket_source_bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(DecisionStoreError::Invalid);
    }
    let ticket: SupportPilotExport = serde_json::from_slice(ticket_source_bytes)?;
    let ticket_rows_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(&ticket.tickets, &ticket.staffing))?)
    );
    if ticket.source_version_hashes != vec![ticket_rows_digest]
        || ticket.snapshot_id != snapshot_id
        || ticket.baseline_scenario_id != scenario_id
        || ticket.window_start_utc != observed.window_start_utc
        || ticket.data_cutoff_utc != observed.observed_through_utc
        || ticket.horizon_days != observed.observed_days.len()
    {
        return Err(DecisionStoreError::Invalid);
    }
    let imported = build_support_pilot(&ticket).map_err(|_| DecisionStoreError::Invalid)?;
    let labels =
        derive_ticket_sla_labels(&ticket, sla_days).map_err(|_| DecisionStoreError::Invalid)?;
    if imported.snapshot.queue_id != observed.queue_id
        || imported.observed_days != observed.observed_days
        || observed.sla_days != Some(sla_days)
        || observed.resolved_within_sla_by_day.as_deref() != Some(labels.as_slice())
    {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(())
}

/// A candidate fit from later observations. It never replaces the model used
/// for the historical run and is not a calibrated prediction interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOutcomeCalibration {
    pub id: String,
    pub outcome_id: String,
    pub replay_hash: String,
    pub observed_source_sha256: String,
    pub parent_model_version: String,
    pub parent_model_sha256: String,
    pub fit_engine_sha256: String,
    pub min_saturated_days: usize,
    #[serde(default)]
    pub training_days: usize,
    #[serde(default)]
    pub holdout: Option<CapacityHoldoutDiagnostic>,
    pub fit: CapacityFit,
}

/// Immutable link between one human approval and one exact simulation run.
/// It is only a review receipt; operational tools need their own policy gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotReviewLink {
    pub approval_id: String,
    pub agent_id: String,
    pub replay_hash: String,
    pub snapshot_id: String,
    pub scenario_id: String,
}

/// Current human-inspection state for one exact, still accessible run.
/// Approval is only an inspection receipt; no staffing action follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotReviewStatus {
    pub link: PilotReviewLink,
    pub status: ApprovalStatus,
    pub expires_at_utc: String,
    pub decided_by: Option<String>,
}

/// Outcome of one ticket-retention sweep across every scope in the store.
/// `failures` exists so a scope that could not be scrubbed is visible instead
/// of being counted as "nothing to do".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TicketRetentionSweep {
    pub scopes: usize,
    pub scrubbed: usize,
    pub failures: Vec<String>,
}

/// Enforce `decision_ticket_source_blobs.retention_until` on a schedule.
///
/// The deadline used to be executed only by an operator pressing the scrub
/// endpoint or by someone happening to read an expired blob, so retained
/// ticket rows outlived their own retention promise. This task closes that
/// gap. It never creates the store: a deployment that has never used the
/// Decision Lab keeps paying one `exists()` per tick and nothing else.
pub async fn run_ticket_retention_sweeper(home: PathBuf, interval: std::time::Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let path = home.join("decisions.db");
        if !path.exists() {
            continue;
        }
        match tokio::task::spawn_blocking(move || {
            DecisionStore::new(path).sweep_expired_ticket_sources()
        })
        .await
        {
            Ok(report) if report == TicketRetentionSweep::default() => {}
            Ok(report) => {
                for failure in &report.failures {
                    tracing::warn!(
                        scope = %failure,
                        "decision ticket retention sweep failed for one scope; will retry"
                    );
                }
                if report.scrubbed > 0 {
                    tracing::info!(
                        scopes = report.scopes,
                        scrubbed = report.scrubbed,
                        "decision ticket retention sweep revoked expired source bytes"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(%error, "decision ticket retention sweep worker failed")
            }
        }
    }
}

/// Exact-scope input provenance for a decision snapshot. This identifies a
/// source artifact; it does not assert a reviewed causal effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct DecisionSourceLink {
    pub tenant_id: String,
    pub acl: String,
    pub artifact_id: String,
    pub source_version_sha256: String,
}
