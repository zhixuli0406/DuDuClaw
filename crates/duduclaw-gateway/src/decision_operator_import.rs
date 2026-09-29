//! Admin-only, exploratory operator upload. A completed receipt is the
//! visibility boundary across the causal and decision SQLite stores.

use chrono::{DateTime, Utc};
use duduclaw_memory::causal::{CausalStoreError, EvidenceScope};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_ingest::{
    DailyStaffing, SlaHoldoutError, SupportPilotExport, TicketEvent, build_support_pilot,
    evaluate_ticket_sla_holdout, pilot_queue_id,
};
use crate::decision_sim::{DecisionSnapshot, QueueModel, StaffingScenario, simulate};
use crate::decision_store::{DecisionScope, DecisionStore, DecisionStoreError};

pub const MAX_OPERATOR_IMPORT_BODY_BYTES: usize = 2_306_867;
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MODEL_OR_SCENARIO_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPilotImportRequest {
    pub tenant_id: String,
    pub acl: String,
    pub expected_queue_id: String,
    pub source_lineage: String,
    pub retention_until_utc: String,
    pub export: SupportPilotExport,
    pub model: QueueModel,
    pub alternative_scenario: StaffingScenario,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorPilotImportReceipt {
    pub status: String,
    pub queue_id: String,
    pub source_artifact_id: String,
    pub source_sha256: String,
    pub snapshot_id: String,
    pub window_start_utc: String,
    pub model_version: String,
    pub baseline_scenario_id: String,
    pub alternative_scenario_id: String,
    pub baseline_replay_hash: String,
    pub alternative_replay_hash: String,
    pub sla_holdout_id: Option<String>,
    pub sla_holdout_sha256: Option<String>,
    pub sla_holdout_unavailable_reason: Option<String>,
    pub limitations: Vec<String>,
}

fn selector(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl DecisionStore {
    /// Validate completely before writing. An import intent hides incomplete
    /// rows from the catalog, and its request digest makes recovery exact.
    pub fn import_operator_pilot(
        &self,
        request: &OperatorPilotImportRequest,
    ) -> Result<OperatorPilotImportReceipt, DecisionStoreError> {
        if ![
            &request.tenant_id,
            &request.acl,
            &request.expected_queue_id,
            &request.source_lineage,
        ]
        .into_iter()
        .all(|value| selector(value))
            || !selector(&request.export.snapshot_id)
            || request.export.snapshot_id.starts_with("synthetic-support-")
            || !selector(&request.export.baseline_scenario_id)
            || !selector(&request.model.version)
            || !selector(&request.alternative_scenario.id)
            || request.export.baseline_scenario_id == request.alternative_scenario.id
        {
            return Err(DecisionStoreError::Invalid);
        }
        let retention = DateTime::parse_from_rfc3339(&request.retention_until_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        if retention.offset().local_minus_utc() != 0
            || retention.timestamp() <= Utc::now().timestamp()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let pilot =
            build_support_pilot(&request.export).map_err(|_| DecisionStoreError::Invalid)?;
        if pilot_queue_id(&request.export).map_err(|_| DecisionStoreError::Invalid)?
            != Some(request.expected_queue_id.as_str())
        {
            return Err(DecisionStoreError::Invalid);
        }
        if DateTime::parse_from_rfc3339(&request.export.data_cutoff_utc)
            .map_err(|_| DecisionStoreError::Invalid)?
            .timestamp()
            > Utc::now().timestamp()
        {
            return Err(DecisionStoreError::Invalid);
        }
        // Submitted output IDs may not exactly reuse a ticket ID: the API
        // returns these identifiers in its receipt and catalog.
        if request.export.tickets.iter().any(|ticket| {
            [
                &request.expected_queue_id,
                &request.export.snapshot_id,
                &request.model.version,
                &pilot.baseline.id,
                &request.alternative_scenario.id,
            ]
            .contains(&&ticket.ticket_id)
        }) {
            return Err(DecisionStoreError::Invalid);
        }
        let source_bytes =
            serde_json::to_vec(&(&request.export.tickets, &request.export.staffing))?;
        if source_bytes.is_empty() || source_bytes.len() > MAX_SOURCE_BYTES {
            return Err(DecisionStoreError::TooLarge);
        }
        if serde_json::to_vec(&request.model)?.len() > MAX_MODEL_OR_SCENARIO_BYTES
            || serde_json::to_vec(&request.alternative_scenario)?.len()
                > MAX_MODEL_OR_SCENARIO_BYTES
        {
            return Err(DecisionStoreError::TooLarge);
        }
        let source_sha256 = digest(&source_bytes);
        if request.export.source_version_hashes != [source_sha256.clone()]
            || pilot.snapshot.queue_id.as_deref() != Some(request.expected_queue_id.as_str())
        {
            return Err(DecisionStoreError::Invalid);
        }
        let baseline = simulate(&pilot.snapshot, &request.model, &pilot.baseline)?;
        let alternative = simulate(
            &pilot.snapshot,
            &request.model,
            &request.alternative_scenario,
        )?;
        let (sla_training, sla_unavailable) = if request.export.horizon_days < 14 {
            (
                None,
                Some("SLA holdout requires at least 14 complete days".to_string()),
            )
        } else {
            let holdout_days = (request.export.horizon_days / 3).max(7);
            let training_days = request.export.horizon_days - holdout_days;
            match evaluate_ticket_sla_holdout(&request.export, &request.model, training_days, 7) {
                Ok(_) => (Some(training_days), None),
                Err(SlaHoldoutError::Capacity(_)) => (
                    None,
                    Some("SLA holdout training capacity is unidentified".to_string()),
                ),
                Err(_) => return Err(DecisionStoreError::Invalid),
            }
        };
        let scope = DecisionScope {
            tenant_id: request.tenant_id.clone(),
            acl: request.acl.clone(),
        };
        let request_sha256 = digest(&serde_json::to_vec(request)?);
        let causal = self
            .causal_store()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;

        // Fail predictable immutable ID conflicts before creating a source
        // artifact. The DB writes still perform their own conflict checks.
        let conn = self.open()?;
        let prior: Option<(String, String, String, String, String, Option<i64>)> = conn.query_row(
            "SELECT request_sha256,source_sha256,model_version,baseline_scenario_id,alternative_scenario_id,completed_at
             FROM decision_operator_pilot_imports WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3",
            params![scope.tenant_id, scope.acl, pilot.snapshot.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).optional()?;
        if let Some((old_request, old_source, old_model, old_baseline, old_alternative, _)) = &prior
        {
            if old_request != &request_sha256
                || old_source != &source_sha256
                || old_model != &request.model.version
                || old_baseline != &pilot.baseline.id
                || old_alternative != &request.alternative_scenario.id
            {
                return Err(DecisionStoreError::VersionConflict);
            }
        } else {
            let occupied: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM decision_inputs WHERE tenant_id=?1 AND acl=?2 AND kind='snapshot' AND input_id=?3)",
                params![scope.tenant_id, scope.acl, pilot.snapshot.id], |row| row.get(0),
            )?;
            if occupied {
                return Err(DecisionStoreError::VersionConflict);
            }
        }
        for (kind, id, expected) in [
            (
                "model",
                request.model.version.as_str(),
                serde_json::to_string(&request.model)?,
            ),
            (
                "scenario",
                pilot.baseline.id.as_str(),
                serde_json::to_string(&pilot.baseline)?,
            ),
            (
                "scenario",
                request.alternative_scenario.id.as_str(),
                serde_json::to_string(&request.alternative_scenario)?,
            ),
        ] {
            let existing: Option<(String, Option<i64>)> = conn.query_row(
                "SELECT payload_json,invalidated_at FROM decision_inputs WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND input_id=?4",
                params![scope.tenant_id, scope.acl, kind, id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((payload, invalidated)) = existing {
                if invalidated.is_some() {
                    return Err(DecisionStoreError::Revoked);
                }
                if payload != expected {
                    return Err(DecisionStoreError::VersionConflict);
                }
            }
        }
        conn.execute(
            "INSERT OR IGNORE INTO decision_operator_pilot_imports
             (tenant_id,acl,snapshot_id,request_sha256,source_sha256,model_version,baseline_scenario_id,alternative_scenario_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![scope.tenant_id, scope.acl, pilot.snapshot.id, request_sha256,
                source_sha256, request.model.version, pilot.baseline.id, request.alternative_scenario.id],
        )?;
        // A concurrent request may have won the unique snapshot key after
        // our preflight read. Never continue under somebody else's intent.
        let committed_request: String = conn.query_row(
            "SELECT request_sha256 FROM decision_operator_pilot_imports
             WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3",
            params![scope.tenant_id, scope.acl, pilot.snapshot.id],
            |row| row.get(0),
        )?;
        if committed_request != request_sha256 {
            return Err(DecisionStoreError::VersionConflict);
        }
        drop(conn);

        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let artifact = causal.add_artifact(
            &evidence_scope,
            "local_support_pilot_export",
            &format!(
                "operator-pilot:{}",
                digest(
                    format!("{}\0{}\0{}", scope.tenant_id, scope.acl, pilot.snapshot.id).as_bytes()
                )
            ),
            &source_sha256,
            &request.source_lineage,
            std::str::from_utf8(&source_bytes).map_err(|_| DecisionStoreError::Invalid)?,
            DateTime::parse_from_rfc3339(&request.export.data_cutoff_utc)
                .map_err(|_| DecisionStoreError::Invalid)?
                .timestamp(),
            retention.timestamp(),
        )?;
        let writer = self.operator_import_writer();
        writer.put_snapshot(&scope, &pilot.snapshot)?;
        writer.bind_causal_artifact(&scope, &pilot.snapshot.id, &artifact.id)?;
        writer.put_model(&scope, &request.model)?;
        writer.put_scenario(&scope, &pilot.baseline)?;
        writer.put_scenario(&scope, &request.alternative_scenario)?;
        let (sla_id, sla_digest) = if let Some(training_days) = sla_training {
            let id = format!(
                "{}:sla-holdout-v3:{}:{}",
                pilot.snapshot.id, request.model.version, training_days
            );
            let (_, digest) = writer.put_sla_holdout(
                &scope,
                &id,
                &pilot.snapshot.id,
                &request.model.version,
                &pilot.baseline.id,
                &source_bytes,
                &request.export.window_start_utc,
                training_days,
                7,
            )?;
            (Some(id), Some(digest))
        } else {
            (None, None)
        };
        let receipt = OperatorPilotImportReceipt {
            status: "exploratory_operator_upload".into(),
            queue_id: request.expected_queue_id.clone(),
            source_artifact_id: artifact.id,
            source_sha256: source_sha256.clone(),
            snapshot_id: pilot.snapshot.id,
            window_start_utc: request.export.window_start_utc.clone(),
            model_version: request.model.version.clone(),
            baseline_scenario_id: pilot.baseline.id,
            alternative_scenario_id: request.alternative_scenario.id.clone(),
            baseline_replay_hash: baseline.replay_hash,
            alternative_replay_hash: alternative.replay_hash,
            sla_holdout_id: sla_id,
            sla_holdout_sha256: sla_digest,
            sla_holdout_unavailable_reason: sla_unavailable,
            limitations: vec![
                "Operator upload identity, definitions, and upstream source authenticity are unverified".into(),
                "Exploratory replay is not prospective calibration or evidence of a staffing intervention effect".into(),
            ],
        };
        writer.validate_operator_pilot_receipt(&scope, &receipt)?;
        writer.put(
            &scope,
            "uploaded_pilot_receipt",
            &receipt.snapshot_id,
            &receipt,
            Some(&[source_sha256]),
        )?;
        let completed = self.open()?.execute(
            "UPDATE decision_operator_pilot_imports SET completed_at=?4
             WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3 AND request_sha256=?5",
            params![
                scope.tenant_id,
                scope.acl,
                receipt.snapshot_id,
                Utc::now().timestamp(),
                request_sha256
            ],
        )?;
        if completed != 1 {
            return Err(DecisionStoreError::VersionConflict);
        }
        self.validate_operator_pilot_receipt(&scope, &receipt)?;
        Ok(receipt)
    }

    /// The receipt never substitutes for current source, binding, and replay checks.
    pub(crate) fn validate_operator_pilot_receipt(
        &self,
        scope: &DecisionScope,
        receipt: &OperatorPilotImportReceipt,
    ) -> Result<(), DecisionStoreError> {
        let causal = self
            .causal_store()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let metadata =
            causal.read_artifact_metadata(&evidence_scope, &receipt.source_artifact_id)?;
        let source = causal.source_text(&evidence_scope, &receipt.source_artifact_id)?;
        if metadata.kind != "local_support_pilot_export"
            || metadata.content_sha256 != receipt.source_sha256
            || metadata.version != receipt.source_sha256
            || digest(source.as_bytes()) != receipt.source_sha256
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &receipt.snapshot_id)?;
        let cutoff = DateTime::parse_from_rfc3339(&snapshot.data_cutoff_utc)
            .map_err(|_| DecisionStoreError::Corrupt)?;
        if snapshot.queue_id.as_deref() != Some(receipt.queue_id.as_str())
            || snapshot.source_version_hashes != [receipt.source_sha256.clone()]
            || metadata.occurred_at != cutoff.timestamp()
            || self
                .active_source_links(scope, &snapshot)?
                .iter()
                .map(|link| link.artifact_id.as_str())
                .collect::<Vec<_>>()
                != [receipt.source_artifact_id.as_str()]
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let (tickets, staffing): (Vec<TicketEvent>, Vec<DailyStaffing>) =
            serde_json::from_str(&source).map_err(|_| DecisionStoreError::Corrupt)?;
        let rebuilt = build_support_pilot(&SupportPilotExport {
            snapshot_id: receipt.snapshot_id.clone(),
            baseline_scenario_id: receipt.baseline_scenario_id.clone(),
            window_start_utc: receipt.window_start_utc.clone(),
            data_cutoff_utc: snapshot.data_cutoff_utc.clone(),
            source_version_hashes: vec![receipt.source_sha256.clone()],
            seed: snapshot.seed,
            horizon_days: snapshot.arrivals_by_day.len(),
            tickets,
            staffing,
        })
        .map_err(|_| DecisionStoreError::Corrupt)?;
        let stored_baseline: StaffingScenario =
            self.get(scope, "scenario", &receipt.baseline_scenario_id)?;
        if rebuilt.snapshot != snapshot || rebuilt.baseline != stored_baseline {
            return Err(DecisionStoreError::Corrupt);
        }
        let baseline = self.replay(
            scope,
            &receipt.snapshot_id,
            &receipt.model_version,
            &receipt.baseline_scenario_id,
        )?;
        let alternative = self.replay(
            scope,
            &receipt.snapshot_id,
            &receipt.model_version,
            &receipt.alternative_scenario_id,
        )?;
        if baseline.replay_hash != receipt.baseline_replay_hash
            || alternative.replay_hash != receipt.alternative_replay_hash
            || receipt.baseline_scenario_id == receipt.alternative_scenario_id
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if let (Some(id), Some(digest_expected)) =
            (&receipt.sla_holdout_id, &receipt.sla_holdout_sha256)
        {
            self.load_sla_holdout(scope, id, source.as_bytes())?;
            let (_, actual): (serde_json::Value, String) =
                self.get_with_digest(scope, "sla_holdout", id)?;
            if &actual != digest_expected {
                return Err(DecisionStoreError::Corrupt);
            }
        } else if receipt.sla_holdout_id.is_some() || receipt.sla_holdout_sha256.is_some() {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(())
    }

    /// Reconstruct the validated operator export without returning ticket rows
    /// through any dashboard response. Callers must recheck the receipt after
    /// their calculation to close a concurrent source-revocation window.
    pub(crate) fn verified_operator_export(
        &self,
        scope: &DecisionScope,
        receipt: &OperatorPilotImportReceipt,
    ) -> Result<SupportPilotExport, DecisionStoreError> {
        self.validate_operator_pilot_receipt(scope, receipt)?;
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &receipt.snapshot_id)?;
        let causal = self
            .causal_store()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let source = causal.source_text(&evidence_scope, &receipt.source_artifact_id)?;
        let (tickets, staffing): (Vec<TicketEvent>, Vec<DailyStaffing>) =
            serde_json::from_str(&source).map_err(|_| DecisionStoreError::Corrupt)?;
        let export = SupportPilotExport {
            snapshot_id: snapshot.id.clone(),
            baseline_scenario_id: receipt.baseline_scenario_id.clone(),
            window_start_utc: receipt.window_start_utc.clone(),
            data_cutoff_utc: snapshot.data_cutoff_utc.clone(),
            source_version_hashes: vec![receipt.source_sha256.clone()],
            seed: snapshot.seed,
            horizon_days: snapshot.arrivals_by_day.len(),
            tickets,
            staffing,
        };
        let rebuilt = build_support_pilot(&export).map_err(|_| DecisionStoreError::Corrupt)?;
        if rebuilt.snapshot != snapshot || digest(source.as_bytes()) != receipt.source_sha256 {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(export)
    }

    pub(crate) fn completed_operator_pilots(
        &self,
        scope: &DecisionScope,
    ) -> Result<Vec<OperatorPilotImportReceipt>, DecisionStoreError> {
        let conn = self.open()?;
        let rows = conn.prepare(
            "SELECT snapshot_id,source_sha256,model_version,baseline_scenario_id,alternative_scenario_id
             FROM decision_operator_pilot_imports
             WHERE tenant_id=?1 AND acl=?2 AND completed_at IS NOT NULL ORDER BY snapshot_id LIMIT 200"
        )?.query_map(params![scope.tenant_id, scope.acl], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?,
        )))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(conn);
        let mut receipts = Vec::new();
        for (id, source, model, baseline, alternative) in rows {
            let receipt: OperatorPilotImportReceipt =
                match self.get(scope, "uploaded_pilot_receipt", &id) {
                    Ok(value) => value,
                    Err(
                        DecisionStoreError::NotFound
                        | DecisionStoreError::Revoked
                        | DecisionStoreError::Causal(CausalStoreError::NotFound),
                    ) => continue,
                    Err(error) => return Err(error),
                };
            if receipt.snapshot_id != id
                || receipt.source_sha256 != source
                || receipt.model_version != model
                || receipt.baseline_scenario_id != baseline
                || receipt.alternative_scenario_id != alternative
            {
                return Err(DecisionStoreError::Corrupt);
            }
            match self.validate_operator_pilot_receipt(scope, &receipt) {
                Ok(()) => receipts.push(receipt),
                Err(
                    DecisionStoreError::NotFound
                    | DecisionStoreError::Revoked
                    | DecisionStoreError::Causal(CausalStoreError::NotFound),
                ) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(receipts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_synthetic::synthetic_support_export;
    use duduclaw_memory::causal::CausalStore;

    fn request() -> OperatorPilotImportRequest {
        let mut export = synthetic_support_export(47, 21).unwrap();
        export.snapshot_id = "uploaded-support-47-21".into();
        export.baseline_scenario_id = "uploaded-baseline-47-21".into();
        OperatorPilotImportRequest {
            tenant_id: "operator-tenant".into(),
            acl: "private".into(),
            expected_queue_id: "synthetic-support-queue".into(),
            source_lineage: "operator-provided-export".into(),
            retention_until_utc: (Utc::now() + chrono::Duration::days(30))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            export,
            model: QueueModel {
                version: "uploaded-model-47-21".into(),
                service_capacity_per_agent_day: 8,
                sla_days: 2,
                staff_cost_cents_per_agent_day: 10_000,
            },
            alternative_scenario: StaffingScenario {
                id: "uploaded-alternative-47-21".into(),
                agents_by_day: vec![3; 21],
                fixed_extra_capacity_by_day: vec![0; 21],
            },
        }
    }

    #[test]
    fn operator_upload_is_source_bound_retryable_and_revocable() {
        let temp = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(temp.path().join("causal.db"));
        let store =
            DecisionStore::with_causal_store(temp.path().join("decision.db"), causal.clone());
        let request = request();
        let scope = DecisionScope {
            tenant_id: request.tenant_id.clone(),
            acl: request.acl.clone(),
        };
        let receipt = store.import_operator_pilot(&request).unwrap();
        assert_eq!(receipt.status, "exploratory_operator_upload");
        assert_eq!(receipt, store.import_operator_pilot(&request).unwrap());
        assert_eq!(
            store.dashboard_catalog(&scope).unwrap().uploaded_pilots,
            vec![receipt.clone()]
        );
        let brief = store
            .compare_scenarios(
                &scope,
                &receipt.snapshot_id,
                &receipt.model_version,
                &receipt.baseline_scenario_id,
                &receipt.alternative_scenario_id,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            brief.replay.baseline_replay_hash,
            receipt.baseline_replay_hash
        );
        assert_eq!(
            brief.replay.alternative_replay_hash,
            receipt.alternative_replay_hash
        );
        let other_scope = DecisionScope {
            tenant_id: "other".into(),
            acl: "private".into(),
        };
        assert!(
            store
                .dashboard_catalog(&other_scope)
                .unwrap()
                .uploaded_pilots
                .is_empty()
        );
        let mut conflict = request.clone();
        conflict.alternative_scenario.agents_by_day[0] = 4;
        assert!(matches!(
            store.import_operator_pilot(&conflict),
            Err(DecisionStoreError::VersionConflict)
        ));
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        causal
            .invalidate_artifact(&evidence_scope, &receipt.source_artifact_id)
            .unwrap();
        assert!(
            store
                .dashboard_catalog(&scope)
                .unwrap()
                .uploaded_pilots
                .is_empty()
        );
        assert!(
            store
                .replay(
                    &scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                    &receipt.baseline_scenario_id
                )
                .is_err()
        );
        assert!(store.import_operator_pilot(&request).is_err());
    }

    #[test]
    fn uploaded_sla_holdout_source_rejects_tampered_artifact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(temp.path().join("causal.db"));
        let store =
            DecisionStore::with_causal_store(temp.path().join("decision.db"), causal.clone());
        let mut request = request();
        let mut export = synthetic_support_export(88, 21).unwrap();
        export.snapshot_id = request.export.snapshot_id.clone();
        export.baseline_scenario_id = request.export.baseline_scenario_id.clone();
        request.export = export;
        let scope = DecisionScope {
            tenant_id: request.tenant_id.clone(),
            acl: request.acl.clone(),
        };
        let receipt = store.import_operator_pilot(&request).unwrap();
        let holdout_id = receipt.sla_holdout_id.as_deref().unwrap();
        assert!(
            store
                .dashboard_uploaded_sla_holdout_source(
                    &scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                    &receipt.baseline_scenario_id,
                    &receipt.alternative_scenario_id,
                    holdout_id,
                )
                .is_ok()
        );
        rusqlite::Connection::open(causal.path())
            .unwrap()
            .execute(
                "UPDATE causal_artifacts SET content='[[],[]]' WHERE id=?1",
                params![receipt.source_artifact_id],
            )
            .unwrap();
        assert!(
            store
                .dashboard_uploaded_sla_holdout_source(
                    &scope,
                    &receipt.snapshot_id,
                    &receipt.model_version,
                    &receipt.baseline_scenario_id,
                    &receipt.alternative_scenario_id,
                    holdout_id,
                )
                .is_err()
        );
    }

    #[test]
    fn pending_operator_snapshot_is_not_selectable_and_shared_model_remains_visible() {
        let temp = tempfile::tempdir().unwrap();
        let store = DecisionStore::with_causal_store(
            temp.path().join("decision.db"),
            CausalStore::new(temp.path().join("causal.db")),
        );
        let request = request();
        let scope = DecisionScope {
            tenant_id: request.tenant_id.clone(),
            acl: request.acl.clone(),
        };
        let pilot = build_support_pilot(&request.export).unwrap();
        let request_sha256 = digest(&serde_json::to_vec(&request).unwrap());
        store.put_model(&scope, &request.model).unwrap();
        store.put_snapshot(&scope, &pilot.snapshot).unwrap();
        store.put_scenario(&scope, &pilot.baseline).unwrap();
        store
            .put_scenario(&scope, &request.alternative_scenario)
            .unwrap();
        store.open().unwrap().execute(
            "INSERT INTO decision_operator_pilot_imports
             (tenant_id,acl,snapshot_id,request_sha256,source_sha256,model_version,baseline_scenario_id,alternative_scenario_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![scope.tenant_id, scope.acl, pilot.snapshot.id, request_sha256, request.export.source_version_hashes[0],
                request.model.version, pilot.baseline.id, request.alternative_scenario.id],
        ).unwrap();
        let catalog = store.dashboard_catalog(&scope).unwrap();
        assert!(catalog.snapshots.is_empty());
        assert!(catalog.uploaded_pilots.is_empty());
        assert_eq!(catalog.models.len(), 1);
        assert!(matches!(
            store.replay(
                &scope,
                &pilot.snapshot.id,
                &request.model.version,
                &pilot.baseline.id
            ),
            Err(DecisionStoreError::NotFound)
        ));
        assert!(matches!(
            store.compare_scenarios(
                &scope,
                &pilot.snapshot.id,
                &request.model.version,
                &pilot.baseline.id,
                &request.alternative_scenario.id,
                Vec::new()
            ),
            Err(DecisionStoreError::NotFound)
        ));
        let overview = store.dashboard_overview(&scope, 20).unwrap();
        assert!(!overview.counts.contains_key("snapshot"));
        assert!(
            overview
                .artifacts
                .iter()
                .all(|item| item.id != pilot.snapshot.id)
        );
        // A half-written status marker without its immutable receipt must
        // still be invisible to every direct snapshot consumer.
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE decision_operator_pilot_imports SET completed_at=1
             WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3",
                params![scope.tenant_id, scope.acl, pilot.snapshot.id],
            )
            .unwrap();
        assert!(matches!(
            store.replay(
                &scope,
                &pilot.snapshot.id,
                &request.model.version,
                &pilot.baseline.id
            ),
            Err(DecisionStoreError::NotFound)
        ));
        assert!(
            store
                .dashboard_catalog(&scope)
                .unwrap()
                .snapshots
                .is_empty()
        );
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE decision_operator_pilot_imports SET completed_at=NULL
             WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3",
                params![scope.tenant_id, scope.acl, pilot.snapshot.id],
            )
            .unwrap();
        let recovered = store.import_operator_pilot(&request).unwrap();
        assert_eq!(
            store.dashboard_catalog(&scope).unwrap().uploaded_pilots,
            vec![recovered]
        );
    }

    #[test]
    fn invalid_queue_or_source_digest_is_rejected_before_any_write() {
        let temp = tempfile::tempdir().unwrap();
        let decision_path = temp.path().join("decision.db");
        let causal_path = temp.path().join("causal.db");
        let store =
            DecisionStore::with_causal_store(&decision_path, CausalStore::new(&causal_path));
        let mut bad_queue = request();
        bad_queue.export.tickets[0].queue_id = Some("another-queue".into());
        assert!(matches!(
            store.import_operator_pilot(&bad_queue),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(!decision_path.exists());
        assert!(!causal_path.exists());
        let mut bad_digest = request();
        bad_digest.export.source_version_hashes[0] = "a".repeat(64);
        assert!(matches!(
            store.import_operator_pilot(&bad_digest),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(!decision_path.exists());
        assert!(!causal_path.exists());
    }
}
