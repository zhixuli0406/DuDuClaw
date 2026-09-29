//! Durable local causal-source revocation delivery to CCR.
//!
//! CausalStore writes a content-free notice in the same SQLite transaction as
//! each source mutation. A failed CCR write leaves that notice pending. Native
//! CCR reads independently recheck the source, so the delivery lag cannot
//! grant stale causal content to a gateway caller.

use std::path::{Path, PathBuf};
use std::time::Duration;

use duduclaw_llm::CcrStore;
use duduclaw_memory::causal::CausalStore;
use tracing::{info, warn};

const DRAIN_BATCH: usize = 128;

/// One bounded, retryable pass. Never creates a missing source database.
pub(crate) fn drain_once(home: &Path) -> Result<usize, String> {
    let causal_path = home.join("memory.db");
    if !causal_path.is_file() {
        return Ok(0);
    }
    let causal = CausalStore::new(causal_path);
    let notices = causal
        .pending_ccr_revocations(DRAIN_BATCH)
        .map_err(|error| format!("read causal CCR outbox: {error}"))?;
    if notices.is_empty() {
        return Ok(0);
    }
    let ccr = CcrStore::new(home.join("ccr").join("ccr.db"));
    let mut delivered = 0;
    for notice in notices {
        if notice.connector != "causal" {
            return Err("causal CCR outbox contains an unexpected connector".into());
        }
        // CCR rejects such bindings at write time, so no handle can match an
        // oversized source identity. Acknowledge to avoid wedging the queue.
        if !notice.tenant_id.trim().is_empty()
            && !notice.artifact_id.trim().is_empty()
            && !notice.version.trim().is_empty()
            && notice.artifact_id.len() <= 512
            && notice.version.len() <= 512
        {
            ccr.revoke_artifact_version(
                &notice.tenant_id,
                &notice.connector,
                &notice.artifact_id,
                &notice.version,
            )
            .map_err(|error| format!("tombstone causal version in CCR: {error}"))?;
        }
        causal
            .acknowledge_ccr_revocation(&notice)
            .map_err(|error| format!("acknowledge causal CCR outbox: {error}"))?;
        delivered += 1;
    }
    Ok(delivered)
}

pub(crate) async fn run(home: PathBuf, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let mut consecutive_full_batches = 0;
        loop {
            let next_home = home.clone();
            match tokio::task::spawn_blocking(move || drain_once(&next_home)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(count)) => {
                    info!(count, "causal CCR revocation notices delivered");
                    if count < DRAIN_BATCH {
                        break;
                    }
                    consecutive_full_batches += 1;
                    if consecutive_full_batches == 32 {
                        // A large backlog drains promptly without monopolizing
                        // the blocking pool under a continuous source producer.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        consecutive_full_batches = 0;
                    } else {
                        tokio::task::yield_now().await;
                    }
                }
                Ok(Err(error)) => {
                    warn!(%error, "causal CCR revocation delivery will retry");
                    break;
                }
                Err(error) => {
                    warn!(%error, "causal CCR revocation drain task failed");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_llm::{CcrScope, CcrSourceArtifact};
    use duduclaw_memory::causal::EvidenceScope;

    fn scope(tenant: &str) -> (EvidenceScope, CcrScope) {
        (
            EvidenceScope {
                tenant_id: tenant.into(),
                acl: "private".into(),
            },
            CcrScope {
                tenant_id: tenant.into(),
                agent_id: "agent".into(),
                session_id: "session".into(),
                source_acl: "private".into(),
            },
        )
    }

    fn bound(
        causal: &CausalStore,
        ccr: &CcrStore,
        tenant: &str,
        external_id: &str,
        retention_at: i64,
    ) -> (EvidenceScope, CcrScope, String, String) {
        let (source_scope, ccr_scope) = scope(tenant);
        let content = format!("source for {tenant} {external_id}");
        let source = causal
            .add_artifact(
                &source_scope,
                "ticket",
                external_id,
                "v1",
                external_id,
                &content,
                1,
                retention_at,
            )
            .unwrap();
        let entry = ccr
            .put_bound(
                &ccr_scope,
                "mcp:search",
                external_id,
                &content,
                &CcrSourceArtifact {
                    connector: "causal".into(),
                    artifact_id: source.id.clone(),
                    version: source.version,
                    acl_revision: "revision".into(),
                },
            )
            .unwrap();
        (source_scope, ccr_scope, source.id, entry.id)
    }

    #[test]
    fn direct_source_mutation_drains_exact_source_and_tenant_only() {
        let home = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let ccr = CcrStore::new(home.path().join("ccr/ccr.db"));
        let future = i64::MAX / 2;
        let (a_scope, a_ccr, a_id, a_handle) = bound(&causal, &ccr, "a", "first", future);
        let (_b_scope, b_ccr, _b_id, b_handle) = bound(&causal, &ccr, "b", "first", future);
        let (_a_other_scope, a_other_ccr, _a_other_id, a_other_handle) =
            bound(&causal, &ccr, "a", "second", future);

        causal.invalidate_artifact(&a_scope, &a_id).unwrap();
        assert_eq!(drain_once(home.path()).unwrap(), 1);
        // Public CcrStore reads deliberately refuse every source-bound entry
        // without a live application validator. Inspect exact stored IDs here
        // to prove that the outbox scrubbed only the invalidated source.
        let conn = rusqlite::Connection::open(ccr.path()).unwrap();
        let retained = |id: &str, scope: &CcrScope| -> bool {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM ccr_entries WHERE id=?1 AND tenant_id=?2)",
                rusqlite::params![id, scope.tenant_id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        };
        assert!(!retained(&a_handle, &a_ccr));
        assert!(retained(&b_handle, &b_ccr));
        assert!(retained(&a_other_handle, &a_other_ccr));
        assert!(causal.pending_ccr_revocations(128).unwrap().is_empty());
    }

    #[test]
    fn failed_ccr_delivery_retries_and_prevents_later_put() {
        let home = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let source_scope = scope("a").0;
        let source = causal
            .add_artifact(
                &source_scope,
                "ticket",
                "first",
                "v1",
                "first",
                "source",
                1,
                i64::MAX / 2,
            )
            .unwrap();
        causal.erase_artifact(&source_scope, &source.id).unwrap();
        std::fs::write(home.path().join("ccr"), b"not a directory").unwrap();
        assert!(drain_once(home.path()).is_err());
        assert_eq!(causal.pending_ccr_revocations(128).unwrap().len(), 1);
        std::fs::remove_file(home.path().join("ccr")).unwrap();
        assert_eq!(drain_once(home.path()).unwrap(), 1);
        let ccr = CcrStore::new(home.path().join("ccr/ccr.db"));
        assert!(
            ccr.put_bound(
                &scope("a").1,
                "mcp:search",
                "call",
                "source",
                &CcrSourceArtifact {
                    connector: "causal".into(),
                    artifact_id: source.id,
                    version: "v1".into(),
                    acl_revision: "revision".into(),
                },
            )
            .is_err()
        );
    }

    #[test]
    fn out_of_band_acl_and_version_update_tombstones_old_identity() {
        let home = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let ccr = CcrStore::new(home.path().join("ccr/ccr.db"));
        let (_source_scope, ccr_scope, source_id, handle) =
            bound(&causal, &ccr, "a", "first", i64::MAX / 2);
        // The installed SQLite trigger covers mutations made through another
        // connection, not only CausalStore's high-level invalidation API.
        rusqlite::Connection::open(causal.path())
            .unwrap()
            .execute(
                "UPDATE causal_artifacts SET acl='moved',version='v2' WHERE id=?1",
                [&source_id],
            )
            .unwrap();
        let pending = causal.pending_ccr_revocations(128).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tenant_id, "a");
        assert_eq!(pending[0].connector, "causal");
        assert_eq!(pending[0].artifact_id, source_id);
        assert_eq!(pending[0].version, "v1");
        assert_eq!(drain_once(home.path()).unwrap(), 1);
        assert!(ccr.retrieve(&ccr_scope, &handle, None, 0, 256).is_err());
    }

    #[test]
    fn out_of_band_source_delete_is_retried_as_old_version() {
        let home = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let ccr = CcrStore::new(home.path().join("ccr/ccr.db"));
        let (_source_scope, ccr_scope, source_id, handle) =
            bound(&causal, &ccr, "a", "first", i64::MAX / 2);
        rusqlite::Connection::open(causal.path())
            .unwrap()
            .execute("DELETE FROM causal_artifacts WHERE id=?1", [&source_id])
            .unwrap();
        assert_eq!(drain_once(home.path()).unwrap(), 1);
        assert!(ccr.retrieve(&ccr_scope, &handle, None, 0, 256).is_err());
    }

    #[test]
    fn interrupted_schema_install_retries_legacy_source_backfill() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("memory.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE causal_artifacts (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                kind TEXT NOT NULL, external_id TEXT NOT NULL, version TEXT NOT NULL,
                lineage_id TEXT NOT NULL, content_sha256 TEXT NOT NULL,
                content TEXT NOT NULL, occurred_at INTEGER NOT NULL,
                ingested_at INTEGER NOT NULL, retention_at INTEGER NOT NULL,
                invalidated_at INTEGER
             );
             CREATE TABLE causal_ccr_revocation_outbox (
                tenant_id TEXT NOT NULL, connector TEXT NOT NULL,
                artifact_id TEXT NOT NULL, version TEXT NOT NULL,
                queued_at INTEGER NOT NULL, delivered_at INTEGER,
                PRIMARY KEY(tenant_id, connector, artifact_id, version)
             );
             INSERT INTO causal_artifacts VALUES
             ('legacy','a','private','ticket','legacy','v1','legacy','digest',
              '',1,1,0,1);",
        )
        .unwrap();
        drop(db);
        // The outbox table exists, but the backfill completion marker does
        // not: this is the crash point between schema creation and migration.
        let causal = CausalStore::new(path);
        let pending = causal.pending_ccr_revocations(128).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tenant_id, "a");
        assert_eq!(pending[0].artifact_id, "legacy");
        assert_eq!(pending[0].version, "v1");
        assert_eq!(drain_once(home.path()).unwrap(), 1);
        assert!(causal.pending_ccr_revocations(128).unwrap().is_empty());
    }

    #[test]
    fn impossible_binding_identity_does_not_block_valid_notice() {
        let home = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let ccr = CcrStore::new(home.path().join("ccr/ccr.db"));
        let (source_scope, ccr_scope, source_id, handle) =
            bound(&causal, &ccr, "a", "first", i64::MAX / 2);
        let db = rusqlite::Connection::open(causal.path()).unwrap();
        db.execute(
            "INSERT INTO causal_ccr_revocation_outbox
             (tenant_id,connector,artifact_id,version,queued_at)
             VALUES ('a','causal','','v1',0)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO causal_ccr_revocation_outbox
             (tenant_id,connector,artifact_id,version,queued_at)
             VALUES ('a','other','bogus','v1',0)",
            [],
        )
        .unwrap();
        causal
            .invalidate_artifact(&source_scope, &source_id)
            .unwrap();
        assert_eq!(drain_once(home.path()).unwrap(), 2);
        assert!(ccr.retrieve(&ccr_scope, &handle, None, 0, 256).is_err());
        assert!(causal.pending_ccr_revocations(128).unwrap().is_empty());
    }

    #[test]
    fn retention_expiry_queues_and_drains_without_direct_removal() {
        let home = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let ccr = CcrStore::new(home.path().join("ccr/ccr.db"));
        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 2;
        let (source_scope, ccr_scope, source_id, handle) =
            bound(&causal, &ccr, "a", "soon", deadline);
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(drain_once(home.path()).unwrap(), 1);
        assert!(causal.source_text(&source_scope, &source_id).is_err());
        assert!(ccr.retrieve(&ccr_scope, &handle, None, 0, 256).is_err());
    }
}
