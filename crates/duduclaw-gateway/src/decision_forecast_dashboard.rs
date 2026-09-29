//! Source-bound historical one-step forecast receipts for Decision Lab.
//!
//! No ticket ID, opening-ticket list, or per-ticket row leaves the gateway on
//! any Decision Lab path. "Aggregates only" is true of *this* module's summary
//! ([`DecisionForecastValidationSummary`] carries counts, digests, and
//! whole-window error sums and nothing per day) — it is not a Decision Lab-wide
//! property. `decision_dashboard::DecisionStore::dashboard_operator_engineering_validation`
//! rebuilds its diagnostics from the same operator-uploaded export and does
//! return per-day series: predicted-versus-observed arrivals and backlog
//! (`one_day_backtest.points`), per-day interval bands
//! (`fixed_interval_diagnostic`), and per-day within-SLA counts
//! (`ticket_sla_holdout`). Those are daily aggregates, never ticket rows, but a
//! real support queue's day-by-day volumes and SLA compliance do leave the
//! gateway there — to the same instance admin who uploaded them.

use duduclaw_memory::causal::EvidenceScope;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::decision_calibration::{CalibrationError, backtest_one_step_forecast};
use crate::decision_ingest::{SupportPilotExport, build_support_pilot};
use crate::decision_operator_import::OperatorPilotImportReceipt;
use crate::decision_sim::{DecisionSnapshot, QueueModel, StaffingScenario};
use crate::decision_store::{
    DecisionScope, DecisionStore, DecisionStoreError, StoredForecastValidation,
};
use crate::decision_synthetic::synthetic_support_export;

const MIN_TRAINING_DAYS: usize = 14;
const MIN_SATURATED_DAYS: usize = 7;
const CALIBRATION_POINTS: usize = 14;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionForecastValidationSummary {
    pub status: &'static str,
    pub source_origin: &'static str,
    pub snapshot_id: String,
    pub model_version: String,
    pub baseline_scenario_id: String,
    pub alternative_scenario_id: String,
    pub source_sha256: String,
    pub min_training_days: usize,
    pub min_saturated_days: usize,
    pub calibration_points: usize,
    pub record_id: Option<String>,
    pub record_sha256: Option<String>,
    pub forecast_evaluation_days: Option<usize>,
    /// Decimal strings: these sums exceed the exact integer range of a JSON
    /// number's double, and the dashboard compares them with `BigInt`.
    pub arrival_abs_error_sum: Option<String>,
    pub model_abs_error_sum: Option<String>,
    pub no_change_abs_error_sum: Option<String>,
    pub seasonal_naive_abs_error_sum: Option<String>,
    pub mean_change_abs_error_sum: Option<String>,
    pub model_beats_all_baselines: Option<bool>,
    pub rolling_interval_evaluated_points: Option<usize>,
    pub rolling_observed_coverage_basis_points: Option<u16>,
    pub fixed_interval_evaluated_points: Option<usize>,
    pub fixed_observed_coverage_basis_points: Option<u16>,
    pub unavailable_reason: Option<String>,
    pub interval_unavailable_reason: Option<String>,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForecastEvidenceSource {
    pub source_bytes: Vec<u8>,
    pub source_sha256: String,
    pub source_artifact_id: String,
    pub record_sha256: String,
    pub operator_receipt: Option<OperatorPilotImportReceipt>,
}

struct VerifiedPilotSource {
    status: &'static str,
    source_origin: &'static str,
    export: SupportPilotExport,
    source_bytes: Vec<u8>,
    source_sha256: String,
    source_artifact_id: String,
    operator_receipt: Option<OperatorPilotImportReceipt>,
}

fn record_id(
    snapshot_id: &str,
    model_version: &str,
    baseline_id: &str,
    alternative_id: &str,
    source_sha256: &str,
    window_start_utc: &str,
) -> Result<String, DecisionStoreError> {
    let manifest = serde_json::to_vec(&(
        "dashboard-historical-forecast-v1",
        snapshot_id,
        model_version,
        baseline_id,
        alternative_id,
        source_sha256,
        window_start_utc,
        MIN_TRAINING_DAYS,
        MIN_SATURATED_DAYS,
        CALIBRATION_POINTS,
    ))?;
    Ok(format!(
        "dashboard-forecast-validation-v1-{:x}",
        Sha256::digest(manifest)
    ))
}

impl DecisionStore {
    #[allow(clippy::too_many_arguments)]
    fn verified_forecast_pilot_source(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
    ) -> Result<VerifiedPilotSource, DecisionStoreError> {
        if !scope.valid() || baseline_id == alternative_id {
            return Err(DecisionStoreError::Invalid);
        }
        let catalog = self.dashboard_catalog(scope)?;
        let synthetic = catalog.synthetic_pilots.iter().any(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        let uploaded = catalog.uploaded_pilots.iter().find(|pilot| {
            pilot.snapshot_id == snapshot_id
                && pilot.model_version == model_version
                && pilot.baseline_scenario_id == baseline_id
                && pilot.alternative_scenario_id == alternative_id
        });
        if synthetic == uploaded.is_some() {
            return Err(DecisionStoreError::NotFound);
        }
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", baseline_id)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", alternative_id)?;
        let (export, source_artifact_id, operator_receipt) = if let Some(receipt) = uploaded {
            (
                self.verified_operator_export(scope, receipt)?,
                receipt.source_artifact_id.clone(),
                Some(receipt.clone()),
            )
        } else {
            let mut export =
                synthetic_support_export(snapshot.seed, snapshot.arrivals_by_day.len())
                    .map_err(|_| DecisionStoreError::Corrupt)?;
            if export.snapshot_id != snapshot_id {
                return Err(DecisionStoreError::Corrupt);
            }
            export.baseline_scenario_id = baseline_id.into();
            let links = self.active_source_links(scope, &snapshot)?;
            if links.len() != 1 {
                return Err(DecisionStoreError::Corrupt);
            }
            let causal = self
                .causal_store()
                .ok_or(DecisionStoreError::CausalStoreRequired)?;
            let evidence_scope = EvidenceScope {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
            };
            let metadata = causal.read_artifact_metadata(&evidence_scope, &links[0].artifact_id)?;
            let stored_source = causal.source_text(&evidence_scope, &links[0].artifact_id)?;
            let canonical_source = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
            let canonical_sha256 = format!("{:x}", Sha256::digest(&canonical_source));
            if metadata.kind != "synthetic_support_export"
                || metadata.content_sha256 != canonical_sha256
                || metadata.version != canonical_sha256
                || links[0].source_version_sha256 != canonical_sha256
                || stored_source.as_bytes() != canonical_source
            {
                return Err(DecisionStoreError::Revoked);
            }
            (export, links[0].artifact_id.clone(), None)
        };
        let source_bytes = serde_json::to_vec(&(&export.tickets, &export.staffing))?;
        let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
        let imported = build_support_pilot(&export).map_err(|_| DecisionStoreError::Corrupt)?;
        if snapshot.source_version_hashes != [source_sha256.clone()]
            || export.source_version_hashes != [source_sha256.clone()]
            || imported.snapshot != snapshot
            || imported.baseline != baseline
            || export.baseline_scenario_id != baseline_id
        {
            return Err(DecisionStoreError::Corrupt);
        }
        self.verify_simulation_inputs_still_current(
            scope,
            &snapshot,
            &model,
            &[&baseline, &alternative],
        )?;
        if let Some(receipt) = &operator_receipt {
            let current: OperatorPilotImportReceipt =
                self.get(scope, "uploaded_pilot_receipt", snapshot_id)?;
            if &current != receipt {
                return Err(DecisionStoreError::Revoked);
            }
        }
        Ok(VerifiedPilotSource {
            status: if synthetic {
                "synthetic_only_exploratory"
            } else {
                "operator_supplied_exploratory"
            },
            source_origin: if synthetic { "synthetic" } else { "uploaded" },
            export,
            source_bytes,
            source_sha256,
            source_artifact_id,
            operator_receipt,
        })
    }

    /// Create an immutable historical diagnostic from a verified dashboard
    /// pilot. No forecast points or ticket rows leave this method.
    pub fn dashboard_forecast_validation(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
    ) -> Result<DecisionForecastValidationSummary, DecisionStoreError> {
        let source = self.verified_forecast_pilot_source(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
        )?;
        let pilot = build_support_pilot(&source.export).map_err(|_| DecisionStoreError::Corrupt)?;
        let unavailable_reason = if pilot.observed_days.len() <= MIN_TRAINING_DAYS {
            Some("At least 15 complete days are required for a one-day historical backtest".into())
        } else {
            match backtest_one_step_forecast(
                &pilot.observed_days,
                MIN_TRAINING_DAYS,
                MIN_SATURATED_DAYS,
            ) {
                Ok(_) => None,
                Err(CalibrationError::Unidentified) => {
                    Some("Historical training prefixes do not identify per-agent capacity".into())
                }
                Err(CalibrationError::InsufficientHistory) => Some(
                    "At least 15 complete days are required for a one-day historical backtest"
                        .into(),
                ),
                Err(error) => return Err(error.into()),
            }
        };
        let (record, record_sha256) = if unavailable_reason.is_some() {
            (None, None)
        } else {
            let id = record_id(
                snapshot_id,
                model_version,
                baseline_id,
                alternative_id,
                &source.source_sha256,
                &source.export.window_start_utc,
            )?;
            let (saved, digest) = self.put_forecast_validation(
                scope,
                &id,
                snapshot_id,
                &source.source_bytes,
                &source.export.window_start_utc,
                baseline_id,
                MIN_TRAINING_DAYS,
                MIN_SATURATED_DAYS,
                CALIBRATION_POINTS,
            )?;
            let loaded = self.load_forecast_validation(scope, &id, &source.source_bytes)?;
            let (_, loaded_digest): (StoredForecastValidation, String) =
                self.get_with_digest(scope, "forecast_validation", &id)?;
            if loaded != saved || loaded_digest != digest {
                return Err(DecisionStoreError::Corrupt);
            }
            (Some(saved), Some(digest))
        };
        let interval_unavailable_reason = match &record {
            None => Some("A saved one-day backtest is required before interval diagnostics".into()),
            Some(saved) if saved.fixed_interval.is_none() => Some(
                "The fixed-prefix interval requires at least 15 one-day evaluation points after its 14-point calibration prefix".into(),
            ),
            Some(_) => None,
        };
        let current = self.verified_forecast_pilot_source(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
        )?;
        if current.source_bytes != source.source_bytes
            || current.source_artifact_id != source.source_artifact_id
            || current.operator_receipt != source.operator_receipt
        {
            return Err(DecisionStoreError::Revoked);
        }
        Ok(DecisionForecastValidationSummary {
            status: source.status,
            source_origin: source.source_origin,
            snapshot_id: snapshot_id.into(),
            model_version: model_version.into(),
            baseline_scenario_id: baseline_id.into(),
            alternative_scenario_id: alternative_id.into(),
            source_sha256: source.source_sha256,
            min_training_days: MIN_TRAINING_DAYS,
            min_saturated_days: MIN_SATURATED_DAYS,
            calibration_points: CALIBRATION_POINTS,
            record_id: record.as_ref().map(|saved| saved.id.clone()),
            record_sha256,
            forecast_evaluation_days: record.as_ref().map(|saved| saved.forecast.evaluation_days),
            arrival_abs_error_sum: record
                .as_ref()
                .map(|saved| saved.forecast.arrival_abs_error_sum.to_string()),
            model_abs_error_sum: record
                .as_ref()
                .map(|saved| saved.forecast.model_abs_error_sum.to_string()),
            no_change_abs_error_sum: record
                .as_ref()
                .map(|saved| saved.forecast.no_change_abs_error_sum.to_string()),
            seasonal_naive_abs_error_sum: record
                .as_ref()
                .map(|saved| saved.forecast.seasonal_naive_abs_error_sum.to_string()),
            mean_change_abs_error_sum: record
                .as_ref()
                .map(|saved| saved.forecast.mean_change_abs_error_sum.to_string()),
            model_beats_all_baselines: record
                .as_ref()
                .map(|saved| saved.forecast.model_beats_all_baselines),
            rolling_interval_evaluated_points: record.as_ref().and_then(|saved| {
                saved
                    .rolling_interval
                    .as_ref()
                    .map(|value| value.evaluated_points)
            }),
            rolling_observed_coverage_basis_points: record.as_ref().and_then(|saved| {
                saved
                    .rolling_interval
                    .as_ref()
                    .map(|value| value.observed_coverage_basis_points)
            }),
            fixed_interval_evaluated_points: record.as_ref().and_then(|saved| {
                saved
                    .fixed_interval
                    .as_ref()
                    .map(|value| value.evaluated_points)
            }),
            fixed_observed_coverage_basis_points: record.as_ref().and_then(|saved| {
                saved
                    .fixed_interval
                    .as_ref()
                    .map(|value| value.observed_coverage_basis_points)
            }),
            unavailable_reason,
            interval_unavailable_reason,
            limitations: vec![
                "Historical one-step diagnostics condition on observed target-day staffing and opening backlog; they do not forecast multi-day demand or identify a staffing effect.",
                "Residual-band coverage is descriptive and does not establish calibrated prospective coverage.",
                if source.source_origin == "uploaded" {
                    "The operator-uploaded source has unverified upstream identity, definitions, and authenticity."
                } else {
                    "Synthetic fixture only; no real-data calibration or production evidence is claimed."
                },
            ],
        })
    }

    /// Load only a previously saved immutable record for the exact selected
    /// pair. The caller repeats this after brief composition.
    pub(crate) fn dashboard_forecast_evidence_source(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        expected_id: &str,
    ) -> Result<ForecastEvidenceSource, DecisionStoreError> {
        let source = self.verified_forecast_pilot_source(
            scope,
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
        )?;
        let exact_id = record_id(
            snapshot_id,
            model_version,
            baseline_id,
            alternative_id,
            &source.source_sha256,
            &source.export.window_start_utc,
        )?;
        if expected_id != exact_id {
            return Err(DecisionStoreError::Invalid);
        }
        let record = self.load_forecast_validation(scope, expected_id, &source.source_bytes)?;
        let (stored, record_sha256): (StoredForecastValidation, String) =
            self.get_with_digest(scope, "forecast_validation", expected_id)?;
        if record != stored
            || record.snapshot_id != snapshot_id
            || record.baseline_scenario_id != baseline_id
            || record.source_sha256 != source.source_sha256
            || record.window_start_utc != source.export.window_start_utc
            || record.min_training_days != MIN_TRAINING_DAYS
            || record.min_saturated_days != MIN_SATURATED_DAYS
            || record.calibration_points != CALIBRATION_POINTS
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(ForecastEvidenceSource {
            source_bytes: source.source_bytes,
            source_sha256: source.source_sha256,
            source_artifact_id: source.source_artifact_id,
            record_sha256,
            operator_receipt: source.operator_receipt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_brief::BriefEvidenceSelection;
    use crate::decision_operator_import::OperatorPilotImportRequest;
    use duduclaw_memory::causal::CausalStore;

    fn uploaded_request(seed: u64, days: usize, unsaturated: bool) -> OperatorPilotImportRequest {
        let mut export = synthetic_support_export(seed, days).unwrap();
        export.snapshot_id = format!("forecast-upload-{seed}-{days}");
        export.baseline_scenario_id = format!("forecast-upload-baseline-{seed}-{days}");
        if unsaturated {
            for ticket in &mut export.tickets {
                let resolution_day = if ticket.created_at_utc.starts_with("2025-12-31") {
                    "2026-01-01"
                } else {
                    ticket.created_at_utc.get(..10).unwrap_or("2026-01-01")
                };
                ticket.resolved_at_utc = Some(format!("{resolution_day}T20:00:00Z"));
            }
            for staffing in &mut export.staffing {
                staffing.agents = 10;
            }
        }
        export.source_version_hashes = vec![format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&export.tickets, &export.staffing)).unwrap())
        )];
        OperatorPilotImportRequest {
            tenant_id: "forecast-operator".into(),
            acl: "private".into(),
            expected_queue_id: "synthetic-support-queue".into(),
            source_lineage: "forecast-test-export".into(),
            retention_until_utc: (chrono::Utc::now() + chrono::Duration::days(30))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            export,
            model: QueueModel {
                version: format!("forecast-upload-model-{seed}-{days}"),
                service_capacity_per_agent_day: 8,
                sla_days: 2,
                staff_cost_cents_per_agent_day: 10_000,
            },
            alternative_scenario: StaffingScenario {
                id: format!("forecast-upload-alternative-{seed}-{days}"),
                agents_by_day: vec![if unsaturated { 9 } else { 3 }; days],
                fixed_extra_capacity_by_day: vec![0; days],
            },
        }
    }

    #[test]
    fn synthetic_forecast_receipt_is_replayable_scoped_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("causal.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decision.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "forecast-synthetic".into(),
            acl: "private".into(),
        };
        let pilot = store
            .create_dashboard_synthetic_pilot(&scope, 47, 35)
            .unwrap();
        let run = || {
            store.dashboard_forecast_validation(
                &scope,
                &pilot.snapshot_id,
                &pilot.model_version,
                &pilot.baseline_scenario_id,
                &pilot.alternative_scenario_id,
            )
        };
        let report = run().unwrap();
        assert_eq!(report, run().unwrap());
        assert_eq!(report.status, "synthetic_only_exploratory");
        assert_eq!(report.source_origin, "synthetic");
        assert_eq!(report.forecast_evaluation_days, Some(21));
        assert!(report.arrival_abs_error_sum.is_some());
        assert!(report.model_abs_error_sum.is_some());
        assert!(report.no_change_abs_error_sum.is_some());
        assert!(report.seasonal_naive_abs_error_sum.is_some());
        assert!(report.mean_change_abs_error_sum.is_some());
        assert!(report.model_beats_all_baselines.is_some());
        assert!(report.fixed_interval_evaluated_points.is_some());
        assert!(report.unavailable_reason.is_none());
        let record_id = report.record_id.as_deref().unwrap();
        let source = store
            .dashboard_forecast_evidence_source(
                &scope,
                &pilot.snapshot_id,
                &pilot.model_version,
                &pilot.baseline_scenario_id,
                &pilot.alternative_scenario_id,
                record_id,
            )
            .unwrap();
        assert_eq!(
            source.record_sha256,
            report.record_sha256.as_deref().unwrap()
        );
        let brief = store
            .compare_scenarios_with_evidence(
                &scope,
                &pilot.snapshot_id,
                &pilot.model_version,
                &pilot.baseline_scenario_id,
                &pilot.alternative_scenario_id,
                BriefEvidenceSelection {
                    empirical_run_id: None,
                    policy_screen_hash: None,
                    event_runs: None,
                    forecast_validation: Some((record_id, &source.source_bytes)),
                    sla_holdout: None,
                    effect_ids: &[],
                },
                Vec::new(),
            )
            .unwrap();
        assert_eq!(brief.exploratory_forecast.unwrap().record_id, record_id);
        let other = DecisionScope {
            tenant_id: "other".into(),
            acl: "private".into(),
        };
        assert!(
            store
                .dashboard_forecast_evidence_source(
                    &other,
                    &pilot.snapshot_id,
                    &pilot.model_version,
                    &pilot.baseline_scenario_id,
                    &pilot.alternative_scenario_id,
                    record_id,
                )
                .is_err()
        );
        let conn = store.open().unwrap();
        conn.execute(
            "UPDATE decision_inputs SET payload_sha256='0'
             WHERE tenant_id=?1 AND acl=?2 AND kind='forecast_validation' AND input_id=?3",
            rusqlite::params![scope.tenant_id, scope.acl, record_id],
        )
        .unwrap();
        assert!(
            store
                .dashboard_forecast_evidence_source(
                    &scope,
                    &pilot.snapshot_id,
                    &pilot.model_version,
                    &pilot.baseline_scenario_id,
                    &pilot.alternative_scenario_id,
                    record_id,
                )
                .is_err()
        );
        conn.execute(
            "UPDATE decision_inputs SET payload_sha256=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='forecast_validation' AND input_id=?4",
            rusqlite::params![
                report.record_sha256.as_deref().unwrap(),
                scope.tenant_id,
                scope.acl,
                record_id
            ],
        )
        .unwrap();
        causal
            .invalidate_artifact(
                &EvidenceScope {
                    tenant_id: scope.tenant_id.clone(),
                    acl: scope.acl.clone(),
                },
                &pilot.source_artifact_id,
            )
            .unwrap();
        assert!(
            store
                .dashboard_forecast_evidence_source(
                    &scope,
                    &pilot.snapshot_id,
                    &pilot.model_version,
                    &pilot.baseline_scenario_id,
                    &pilot.alternative_scenario_id,
                    record_id,
                )
                .is_err()
        );
    }

    #[test]
    fn uploaded_short_and_unidentified_forecasts_return_bounded_unavailability() {
        for (seed, days, unsaturated, expected_reason) in [
            (88, 10, false, "At least 15"),
            (58, 21, true, "do not identify"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let causal = CausalStore::new(dir.path().join("causal.db"));
            let store =
                DecisionStore::with_causal_store(dir.path().join("decision.db"), causal.clone());
            let request = uploaded_request(seed, days, unsaturated);
            let scope = DecisionScope {
                tenant_id: request.tenant_id.clone(),
                acl: request.acl.clone(),
            };
            let receipt = store.import_operator_pilot(&request).unwrap();
            let report = store
                .dashboard_forecast_validation(
                    &scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                    &receipt.baseline_scenario_id,
                    &receipt.alternative_scenario_id,
                )
                .unwrap();
            assert_eq!(report.status, "operator_supplied_exploratory");
            assert_eq!(report.source_origin, "uploaded");
            assert!(report.record_id.is_none());
            assert!(report.record_sha256.is_none());
            assert!(report.model_abs_error_sum.is_none());
            assert!(report.model_beats_all_baselines.is_none());
            assert!(
                report
                    .unavailable_reason
                    .as_deref()
                    .unwrap()
                    .contains(expected_reason)
            );
            assert!(
                !serde_json::to_string(&report)
                    .unwrap()
                    .contains("initial-0")
            );
            rusqlite::Connection::open(causal.path())
                .unwrap()
                .execute(
                    "UPDATE causal_artifacts SET content='[[],[]]' WHERE id=?1",
                    rusqlite::params![receipt.source_artifact_id],
                )
                .unwrap();
            assert!(
                store
                    .dashboard_forecast_validation(
                        &scope,
                        &receipt.snapshot_id,
                        &receipt.model_version,
                        &receipt.baseline_scenario_id,
                        &receipt.alternative_scenario_id,
                    )
                    .is_err()
            );
        }
    }
}
