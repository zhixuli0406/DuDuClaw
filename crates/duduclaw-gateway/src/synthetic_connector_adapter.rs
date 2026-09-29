//! Gateway-owned producer for the explicitly synthetic Decision Lab fixture.
//!
//! Admin requests select a deterministic local fixture. They never supply a
//! connector identity, artifact ID, generation, digest, or upstream event.
//! This adapter derives those values from the generator and the local stores,
//! verifies the exact source bytes, then calls the trusted lifecycle bridge.

use std::path::{Path, PathBuf};

use duduclaw_memory::causal::{CausalStore, CausalStoreError, EvidenceScope};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::connector_lifecycle::{
    LifecycleError, LifecycleKind, LocalLifecycleEvent, LocalSourceBinding, StageOutcome,
    TrustedConnectorLifecycleBridge,
};
use crate::decision_dashboard::DecisionSyntheticPilotReceipt;
use crate::decision_store::{DecisionScope, DecisionStore, DecisionStoreError};
use crate::decision_synthetic::synthetic_support_export;

const CONNECTOR: &str = "dashboard_synthetic_support_v1";
const GENERATION: i64 = 1;

#[derive(Debug, Clone, Copy, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(rename_all = "snake_case")]
pub(crate) enum SyntheticLifecycleKind {
    AclLost,
    Deleted,
    Quarantined,
    VersionChanged,
}

impl From<SyntheticLifecycleKind> for LifecycleKind {
    fn from(value: SyntheticLifecycleKind) -> Self {
        match value {
            SyntheticLifecycleKind::AclLost => Self::AclLost,
            SyntheticLifecycleKind::Deleted => Self::Deleted,
            SyntheticLifecycleKind::Quarantined => Self::Quarantined,
            SyntheticLifecycleKind::VersionChanged => Self::VersionChanged,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SyntheticStageOutcome {
    Staged,
    AlreadyStaged,
    AlreadyCompleted,
}

impl From<StageOutcome> for SyntheticStageOutcome {
    fn from(value: StageOutcome) -> Self {
        match value {
            StageOutcome::Staged => Self::Staged,
            StageOutcome::AlreadyStaged => Self::AlreadyStaged,
            StageOutcome::AlreadyCompleted => Self::AlreadyCompleted,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SyntheticAdapterError {
    #[error("invalid synthetic fixture selector")]
    Invalid,
    #[error("synthetic fixture was not bound by the local adapter")]
    NotFound,
    #[error("synthetic fixture identity conflicts with its local source")]
    Conflict,
    #[error(transparent)]
    Decision(#[from] DecisionStoreError),
    #[error(transparent)]
    Causal(#[from] CausalStoreError),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

struct ExpectedFixture {
    snapshot_id: String,
    external_id: String,
    lineage_id: String,
    text: String,
    digest: String,
}

fn expected_fixture(seed: u64, days: usize) -> Result<ExpectedFixture, SyntheticAdapterError> {
    if !(21..=90).contains(&days) {
        return Err(SyntheticAdapterError::Invalid);
    }
    let export =
        synthetic_support_export(seed, days).map_err(|_| SyntheticAdapterError::Invalid)?;
    let text = serde_json::to_string(&(&export.tickets, &export.staffing))?;
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    if export.source_version_hashes != [digest.clone()] {
        return Err(SyntheticAdapterError::Conflict);
    }
    Ok(ExpectedFixture {
        snapshot_id: export.snapshot_id,
        external_id: format!("dashboard-support-{seed}-{days}"),
        lineage_id: "dashboard_synthetic_support_pilot".into(),
        text,
        digest,
    })
}

fn evidence_scope(scope: &DecisionScope) -> Result<EvidenceScope, SyntheticAdapterError> {
    if !scope.valid() {
        return Err(SyntheticAdapterError::Invalid);
    }
    Ok(EvidenceScope {
        tenant_id: scope.tenant_id.clone(),
        acl: scope.acl.clone(),
    })
}

pub(crate) struct SyntheticLocalConnectorAdapter {
    home: PathBuf,
}

impl SyntheticLocalConnectorAdapter {
    pub(crate) fn for_home(home: impl AsRef<Path>) -> Self {
        Self {
            home: home.as_ref().to_path_buf(),
        }
    }

    fn stores(&self) -> (DecisionStore, CausalStore) {
        let causal = CausalStore::new(self.home.join("memory.db"));
        (
            DecisionStore::with_causal_store(self.home.join("decisions.db"), causal.clone()),
            causal,
        )
    }

    /// The producer persists the fixture through the existing Decision Lab
    /// path, then binds only the local artifact whose retained bytes and
    /// metadata match a fresh deterministic generation.
    pub(crate) fn create_and_bind(
        &self,
        scope: &DecisionScope,
        seed: u64,
        days: usize,
    ) -> Result<DecisionSyntheticPilotReceipt, SyntheticAdapterError> {
        let expected = expected_fixture(seed, days)?;
        let evidence = evidence_scope(scope)?;
        let (store, causal) = self.stores();
        let receipt = store.create_dashboard_synthetic_pilot(scope, seed, days)?;
        if receipt.snapshot_id != expected.snapshot_id {
            return Err(SyntheticAdapterError::Conflict);
        }
        let artifact = causal.read_artifact_metadata(&evidence, &receipt.source_artifact_id)?;
        if artifact.kind != "synthetic_support_export"
            || artifact.external_id != expected.external_id
            || artifact.version != expected.digest
            || artifact.content_sha256 != expected.digest
            || artifact.lineage_id != expected.lineage_id
            || causal.source_text(&evidence, &artifact.id)? != expected.text
        {
            return Err(SyntheticAdapterError::Conflict);
        }
        TrustedConnectorLifecycleBridge::for_home(&self.home).bind_local_source(
            &LocalSourceBinding::from_verified_local_adapter(
                evidence, CONNECTOR, GENERATION, &artifact,
            ),
        )?;
        Ok(receipt)
    }

    /// A lifecycle action can target only a previously bound fixture. A
    /// repeated action remains safe after source bytes are erased: its fixed
    /// identity is checked against both the binding and the decision source
    /// link; the bridge verifies the journal and source metadata on replay.
    pub(crate) fn stage_event(
        &self,
        scope: &DecisionScope,
        seed: u64,
        days: usize,
        kind: SyntheticLifecycleKind,
    ) -> Result<SyntheticStageOutcome, SyntheticAdapterError> {
        let expected = expected_fixture(seed, days)?;
        let evidence = evidence_scope(scope)?;
        self.verify_existing_binding(scope, &expected)?;
        let outcome = TrustedConnectorLifecycleBridge::for_home(&self.home).stage_local_event(
            &LocalLifecycleEvent::from_verified_local_adapter(
                evidence,
                CONNECTOR,
                &expected.external_id,
                GENERATION,
                kind.into(),
            ),
        )?;
        Ok(outcome.into())
    }

    fn verify_existing_binding(
        &self,
        scope: &DecisionScope,
        expected: &ExpectedFixture,
    ) -> Result<(), SyntheticAdapterError> {
        let memory_path = self.home.join("memory.db");
        if !memory_path.is_file() || !self.home.join("decisions.db").is_file() {
            return Err(SyntheticAdapterError::NotFound);
        }
        let memory = Connection::open_with_flags(memory_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let row: Option<(
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<i64>,
            String,
        )> = memory
            .query_row(
                "SELECT b.artifact_id,b.version,b.content_sha256,b.state,
                        a.kind,a.external_id,a.lineage_id,a.content_sha256,
                        a.invalidated_at,a.content
                 FROM local_connector_source_bindings b
                 JOIN causal_artifacts a ON a.id=b.artifact_id
                    AND a.tenant_id=b.tenant_id AND a.acl=b.acl
                 WHERE b.tenant_id=?1 AND b.acl=?2 AND b.connector=?3
                   AND b.external_id=?4 AND b.generation=?5",
                params![
                    scope.tenant_id,
                    scope.acl,
                    CONNECTOR,
                    expected.external_id,
                    GENERATION
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            artifact_id,
            version,
            digest,
            state,
            source_kind,
            external_id,
            lineage_id,
            source_digest,
            invalidated_at,
            content,
        )) = row
        else {
            return Err(SyntheticAdapterError::NotFound);
        };
        if version != expected.digest
            || digest != expected.digest
            || source_digest != expected.digest
            || source_kind != "synthetic_support_export"
            || external_id != expected.external_id
            || lineage_id != expected.lineage_id
        {
            return Err(SyntheticAdapterError::Conflict);
        }
        if (!content.is_empty() && content != expected.text)
            || (content.is_empty() && invalidated_at.is_none())
        {
            return Err(SyntheticAdapterError::Conflict);
        }
        if invalidated_at.is_some() || state != "active" {
            let staged: bool = memory.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_connector_lifecycle_events
                 WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4
                   AND generation=?5 AND artifact_id=?6 AND version=?7
                   AND content_sha256=?8)",
                params![
                    scope.tenant_id,
                    scope.acl,
                    CONNECTOR,
                    expected.external_id,
                    GENERATION,
                    artifact_id,
                    expected.digest,
                    expected.digest
                ],
                |row| row.get(0),
            )?;
            if !staged {
                return Err(SyntheticAdapterError::Conflict);
            }
        }
        let decisions = Connection::open_with_flags(
            self.home.join("decisions.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let linked: bool = decisions.query_row(
            "SELECT EXISTS(SELECT 1 FROM decision_causal_refs
             WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3
               AND artifact_id=?4 AND source_version=?5)",
            params![
                scope.tenant_id,
                scope.acl,
                expected.snapshot_id,
                artifact_id,
                expected.digest
            ],
            |row| row.get(0),
        )?;
        if !linked {
            return Err(SyntheticAdapterError::Conflict);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccr_dashboard::CcrDashboardStore;

    fn demo_scope(tenant: &str) -> DecisionScope {
        DecisionScope {
            tenant_id: tenant.into(),
            acl: "private".into(),
        }
    }

    #[test]
    fn synthetic_admin_producer_reaches_worker_and_dashboard() {
        let home = tempfile::tempdir().unwrap();
        let adapter = SyntheticLocalConnectorAdapter::for_home(home.path());
        let scope = demo_scope("tenant-a");
        let receipt = adapter.create_and_bind(&scope, 47, 35).unwrap();
        assert_eq!(adapter.create_and_bind(&scope, 47, 35).unwrap(), receipt);
        assert!(matches!(
            adapter.stage_event(
                &demo_scope("tenant-b"),
                47,
                35,
                SyntheticLifecycleKind::Deleted
            ),
            Err(SyntheticAdapterError::NotFound)
        ));
        assert!(matches!(
            adapter.stage_event(&scope, 48, 35, SyntheticLifecycleKind::Deleted),
            Err(SyntheticAdapterError::NotFound)
        ));

        assert!(matches!(
            adapter
                .stage_event(&scope, 47, 35, SyntheticLifecycleKind::Deleted)
                .unwrap(),
            SyntheticStageOutcome::Staged
        ));
        assert!(matches!(
            adapter
                .stage_event(&scope, 47, 35, SyntheticLifecycleKind::Deleted)
                .unwrap(),
            SyntheticStageOutcome::AlreadyStaged
        ));
        let dashboard = CcrDashboardStore::from_home(home.path());
        let pending = dashboard.snapshot(&scope.tenant_id).unwrap();
        assert!(pending.connector_lifecycle.available);
        assert_eq!(pending.connector_lifecycle.pending_events, 1);
        assert_eq!(
            dashboard
                .snapshot("tenant-b")
                .unwrap()
                .connector_lifecycle
                .pending_events,
            0
        );
        let evidence = evidence_scope(&scope).unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        assert!(matches!(
            causal.source_text(&evidence, &receipt.source_artifact_id),
            Err(CausalStoreError::NotFound)
        ));

        // This is the same drain used by Gateway's startup/minute worker.
        let bridge = TrustedConnectorLifecycleBridge::for_home(home.path());
        let drained = bridge.drain_once(128).unwrap();
        assert_eq!(drained.completed, 1);
        assert_eq!(drained.retry_pending, 0);
        assert_eq!(
            dashboard
                .snapshot(&scope.tenant_id)
                .unwrap()
                .connector_lifecycle
                .pending_events,
            0
        );
        assert!(matches!(
            adapter
                .stage_event(&scope, 47, 35, SyntheticLifecycleKind::Deleted)
                .unwrap(),
            SyntheticStageOutcome::AlreadyCompleted
        ));
        assert!(matches!(
            adapter.create_and_bind(&scope, 47, 35),
            Err(SyntheticAdapterError::Decision(_))
                | Err(SyntheticAdapterError::Causal(_))
                | Err(SyntheticAdapterError::Lifecycle(_))
        ));
        let retained: String = Connection::open(home.path().join("memory.db"))
            .unwrap()
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&receipt.source_artifact_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(retained.is_empty());
    }

    #[test]
    fn synthetic_action_rechecks_exact_bytes_before_staging() {
        let home = tempfile::tempdir().unwrap();
        let adapter = SyntheticLocalConnectorAdapter::for_home(home.path());
        let scope = demo_scope("tenant-a");
        let receipt = adapter.create_and_bind(&scope, 47, 35).unwrap();
        Connection::open(home.path().join("memory.db"))
            .unwrap()
            .execute(
                "UPDATE causal_artifacts SET content='changed' WHERE id=?1",
                [&receipt.source_artifact_id],
            )
            .unwrap();
        assert!(matches!(
            adapter.stage_event(&scope, 47, 35, SyntheticLifecycleKind::AclLost),
            Err(SyntheticAdapterError::Conflict)
        ));
        assert_eq!(
            CcrDashboardStore::from_home(home.path())
                .snapshot(&scope.tenant_id)
                .unwrap()
                .connector_lifecycle
                .pending_events,
            0
        );
    }

    #[test]
    fn synthetic_terminal_upgrade_rechecks_retained_invalidated_bytes() {
        let home = tempfile::tempdir().unwrap();
        let adapter = SyntheticLocalConnectorAdapter::for_home(home.path());
        let scope = demo_scope("tenant-a");
        let receipt = adapter.create_and_bind(&scope, 47, 35).unwrap();
        adapter
            .stage_event(&scope, 47, 35, SyntheticLifecycleKind::VersionChanged)
            .unwrap();
        assert_eq!(
            TrustedConnectorLifecycleBridge::for_home(home.path())
                .drain_once(128)
                .unwrap()
                .completed,
            1
        );
        Connection::open(home.path().join("memory.db"))
            .unwrap()
            .execute(
                "UPDATE causal_artifacts SET content='changed' WHERE id=?1",
                [&receipt.source_artifact_id],
            )
            .unwrap();
        assert!(matches!(
            adapter.stage_event(&scope, 47, 35, SyntheticLifecycleKind::Deleted),
            Err(SyntheticAdapterError::Conflict)
        ));
    }
}
