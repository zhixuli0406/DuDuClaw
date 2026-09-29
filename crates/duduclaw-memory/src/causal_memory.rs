//! Exact-scope bridge from live agent memories into causal source artifacts.
//!
//! The bridge is opt-in per memory ID. SQLite triggers are installed only
//! when the live memory table exists in the same database; upstream updates,
//! quarantine, forget, and GDPR deletion then revoke copied text atomically.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::causal::{CausalStore, CausalStoreError, EvidenceScope, SourceArtifact, now};

pub const MEMORY_ACL: &str = "agent-private";
const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;

fn parse_timestamp(value: &str) -> Result<i64, CausalStoreError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| date.timestamp())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .map(|date| date.and_utc().timestamp())
        })
        .map_err(|_| CausalStoreError::InvalidInput)
}

/// Marker recorded in `causal_ccr_outbox_meta` in the same transaction that
/// creates the trigger bodies below. **Bump the suffix whenever a body
/// changes** — an older database would otherwise keep running the old body.
///
/// This is deliberately separate from `causal::SCHEMA_VERSION`: the live
/// `memories` table belongs to the memory engine and can appear *after* the
/// causal schema is stamped, so the install has to stay retryable until the
/// table is actually there.
const MEMORY_TRIGGER_MARKER: &str = "memory_triggers:v1";

pub(crate) fn install_memory_triggers(conn: &Connection) -> Result<(), CausalStoreError> {
    let has_memories: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories')",
        [],
        |row| row.get(0),
    )?;
    if !has_memories {
        return Ok(());
    }
    // Read-only short circuit: both bodies present AND at the current
    // revision. Without it every causal read paid for a `BEGIN IMMEDIATE`
    // write transaction that rewrote two triggers it had already written.
    let already_current: bool = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM sqlite_master WHERE type='trigger'
           AND name IN ('causal_memory_after_update','causal_memory_after_delete'))=2
         AND EXISTS(SELECT 1 FROM causal_ccr_outbox_meta WHERE name=?1)",
        [MEMORY_TRIGGER_MARKER],
        |row| row.get(0),
    )?;
    if already_current {
        return Ok(());
    }
    // Older databases can have the base table without the quarantine and
    // temporal columns. The memory engine migrates them before imports run.
    let mut columns = conn.prepare("PRAGMA table_info(memories)")?;
    let names = columns
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    if ![
        "id",
        "agent_id",
        "content",
        "timestamp",
        "quarantined",
        "valid_until",
        "invalidated_at",
    ]
    .iter()
    .all(|name| names.contains(*name))
    {
        return Ok(());
    }
    // The trigger bodies below are rewritten on every open (DROP + CREATE in
    // one transaction) rather than left to `CREATE TRIGGER IF NOT EXISTS`: an
    // older database would otherwise keep running an older body — including
    // the pre-fix body that aborted upstream memory writes while a CCR
    // delivery lease was held.
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS causal_memory_revisions (
            memory_id TEXT PRIMARY KEY, revision INTEGER NOT NULL DEFAULT 0
         );
         DROP TRIGGER IF EXISTS causal_memory_after_update;
         DROP TRIGGER IF EXISTS causal_memory_after_delete;
         CREATE TRIGGER causal_memory_after_update
         AFTER UPDATE OF content,agent_id,quarantined,valid_until,invalidated_at ON memories
         WHEN OLD.content IS NOT NEW.content OR OLD.agent_id IS NOT NEW.agent_id
           OR OLD.quarantined IS NOT NEW.quarantined
           OR OLD.valid_until IS NOT NEW.valid_until
           OR OLD.invalidated_at IS NOT NEW.invalidated_at
         BEGIN
           INSERT INTO causal_memory_revisions(memory_id,revision) VALUES (OLD.id,1)
             ON CONFLICT(memory_id) DO UPDATE SET revision=revision+1;
           UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
             diagnostics_json='{}',identification_state='memory_source_changed'
             WHERE data_snapshot_id IN (
               SELECT id FROM causal_artifacts WHERE kind='memory' AND external_id=OLD.id
                 AND tenant_id=OLD.agent_id AND acl='agent-private'
             ) OR model_id IN (
               SELECT me.model_id FROM causal_model_edges me JOIN causal_evidence e
                 ON e.claim_id=me.claim_id JOIN causal_artifacts a ON a.id=e.artifact_id
               WHERE a.kind='memory' AND a.external_id=OLD.id AND a.tenant_id=OLD.agent_id
                 AND a.acl='agent-private'
             );
           UPDATE causal_claims SET review_state='needs_review',reviewer=NULL,reviewed_at=NULL
             WHERE review_state='accepted' AND id IN (
               SELECT e.claim_id FROM causal_evidence e JOIN causal_artifacts a
                 ON a.id=e.artifact_id WHERE a.kind='memory' AND a.external_id=OLD.id
                 AND a.tenant_id=OLD.agent_id AND a.acl='agent-private'
             );
           UPDATE causal_evidence SET excerpt='' WHERE artifact_id IN (
             SELECT id FROM causal_artifacts WHERE kind='memory' AND external_id=OLD.id
               AND tenant_id=OLD.agent_id AND acl='agent-private'
           );
           UPDATE causal_claims SET context_json='{}' WHERE id IN (
             SELECT e.claim_id FROM causal_evidence e JOIN causal_artifacts a
               ON a.id=e.artifact_id WHERE a.kind='memory' AND a.external_id=OLD.id
               AND a.tenant_id=OLD.agent_id AND a.acl='agent-private'
               AND a.tenant_id=causal_claims.tenant_id AND a.acl=causal_claims.acl
           );
           UPDATE causal_negative_control_reviews SET rationale='' WHERE protocol_artifact_id IN (
             SELECT id FROM causal_artifacts WHERE kind='memory' AND external_id=OLD.id
               AND tenant_id=OLD.agent_id AND acl='agent-private'
           );
           UPDATE causal_artifacts SET content='',invalidated_at=CAST(strftime('%s','now') AS INTEGER)
             WHERE kind='memory' AND external_id=OLD.id AND tenant_id=OLD.agent_id
               AND acl='agent-private' AND invalidated_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM causal_ccr_delivery_leases l
                 WHERE l.tenant_id=causal_artifacts.tenant_id
                  AND l.acl=causal_artifacts.acl
                  AND l.artifact_id=causal_artifacts.id
                  AND l.version=causal_artifacts.version);
         END;
         CREATE TRIGGER causal_memory_after_delete
         AFTER DELETE ON memories
         BEGIN
           INSERT INTO causal_memory_revisions(memory_id,revision) VALUES (OLD.id,1)
             ON CONFLICT(memory_id) DO UPDATE SET revision=revision+1;
           UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
             diagnostics_json='{}',identification_state='memory_source_deleted'
             WHERE data_snapshot_id IN (
               SELECT id FROM causal_artifacts WHERE kind='memory' AND external_id=OLD.id
                 AND tenant_id=OLD.agent_id AND acl='agent-private'
             ) OR model_id IN (
               SELECT me.model_id FROM causal_model_edges me JOIN causal_evidence e
                 ON e.claim_id=me.claim_id JOIN causal_artifacts a ON a.id=e.artifact_id
               WHERE a.kind='memory' AND a.external_id=OLD.id AND a.tenant_id=OLD.agent_id
                 AND a.acl='agent-private'
             );
           UPDATE causal_claims SET review_state='needs_review',reviewer=NULL,reviewed_at=NULL
             WHERE review_state='accepted' AND id IN (
               SELECT e.claim_id FROM causal_evidence e JOIN causal_artifacts a
                 ON a.id=e.artifact_id WHERE a.kind='memory' AND a.external_id=OLD.id
                 AND a.tenant_id=OLD.agent_id AND a.acl='agent-private'
             );
           UPDATE causal_evidence SET excerpt='' WHERE artifact_id IN (
             SELECT id FROM causal_artifacts WHERE kind='memory' AND external_id=OLD.id
               AND tenant_id=OLD.agent_id AND acl='agent-private'
           );
           UPDATE causal_claims SET context_json='{}' WHERE id IN (
             SELECT e.claim_id FROM causal_evidence e JOIN causal_artifacts a
               ON a.id=e.artifact_id WHERE a.kind='memory' AND a.external_id=OLD.id
               AND a.tenant_id=OLD.agent_id AND a.acl='agent-private'
               AND a.tenant_id=causal_claims.tenant_id AND a.acl=causal_claims.acl
           );
           UPDATE causal_negative_control_reviews SET rationale='' WHERE protocol_artifact_id IN (
             SELECT id FROM causal_artifacts WHERE kind='memory' AND external_id=OLD.id
               AND tenant_id=OLD.agent_id AND acl='agent-private'
           );
           UPDATE causal_artifacts SET content='',invalidated_at=CAST(strftime('%s','now') AS INTEGER)
             WHERE kind='memory' AND external_id=OLD.id AND tenant_id=OLD.agent_id
               AND acl='agent-private' AND invalidated_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM causal_ccr_delivery_leases l
                 WHERE l.tenant_id=causal_artifacts.tenant_id
                  AND l.acl=causal_artifacts.acl
                  AND l.artifact_id=causal_artifacts.id
                  AND l.version=causal_artifacts.version);
         END;
         INSERT OR IGNORE INTO causal_ccr_outbox_meta(name) VALUES ('memory_triggers:v1');
         COMMIT;",
    )?;
    Ok(())
}

impl CausalStore {
    /// Import a live memory into the same agent's private causal scope. The
    /// source version includes a lifecycle revision, so re-import after a
    /// quarantine release or content edit cannot reactivate an erased version.
    pub fn import_memory_source(
        &self,
        agent_id: &str,
        memory_id: &str,
    ) -> Result<SourceArtifact, CausalStoreError> {
        if agent_id.trim().is_empty() || memory_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let scope = EvidenceScope {
            tenant_id: agent_id.into(),
            acl: MEMORY_ACL.into(),
        };
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, String, Option<String>, i64)> = tx.query_row(
            "SELECT content,timestamp,valid_until,COALESCE((SELECT revision FROM causal_memory_revisions
                WHERE memory_id=memories.id),0)
             FROM memories WHERE id=?1 AND agent_id=?2 AND quarantined=0
               AND invalidated_at IS NULL
               AND (valid_until IS NULL OR julianday(valid_until)>julianday('now'))",
            params![memory_id,agent_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional()?;
        let (content, timestamp, valid_until, revision) = row.ok_or(CausalStoreError::NotFound)?;
        if content.is_empty() || content.len() > MAX_CONTENT_BYTES {
            return Err(CausalStoreError::InvalidInput);
        }
        let occurred_at = parse_timestamp(&timestamp)?;
        let retention_at = valid_until
            .as_deref()
            .map(parse_timestamp)
            .transpose()?
            .unwrap_or(i64::MAX);
        if retention_at <= now() {
            return Err(CausalStoreError::NotFound);
        }
        let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
        let version = format!("{digest}:{revision}");
        let existing: Option<(String, i64)> = tx
            .query_row(
                "SELECT id,ingested_at FROM causal_artifacts WHERE tenant_id=?1 AND acl=?2
                 AND kind='memory' AND external_id=?3 AND version=?4",
                params![scope.tenant_id, scope.acl, memory_id, version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (id, ingested_at) = if let Some((id, ingested_at)) = existing {
            let active: bool = tx.query_row(
                "SELECT invalidated_at IS NULL AND content_sha256=?2 AND content=?3 AND retention_at=?4
                 FROM causal_artifacts WHERE id=?1", params![id,digest,content,retention_at],
                |row| row.get(0),
            )?;
            if !active {
                return Err(CausalStoreError::Conflict);
            }
            (id, ingested_at)
        } else {
            let id = Uuid::new_v4().to_string();
            let ingested_at = now();
            tx.execute(
                "INSERT INTO causal_artifacts
                 (id,tenant_id,acl,kind,external_id,version,lineage_id,content_sha256,
                  content,occurred_at,ingested_at,retention_at)
                 VALUES (?1,?2,?3,'memory',?4,?5,?4,?6,?7,?8,?9,?10)",
                params![
                    id,
                    scope.tenant_id,
                    scope.acl,
                    memory_id,
                    version,
                    digest,
                    content,
                    occurred_at,
                    ingested_at,
                    retention_at
                ],
            )?;
            (id, ingested_at)
        };
        tx.commit()?;
        Ok(SourceArtifact {
            id,
            tenant_id: scope.tenant_id,
            acl: scope.acl,
            kind: "memory".into(),
            external_id: memory_id.into(),
            version,
            lineage_id: memory_id.into(),
            content_sha256: digest,
            occurred_at,
            ingested_at,
            retention_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{ClaimModality, EvidenceStance};

    fn memory_db() -> (tempfile::TempDir, CausalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("memory.db"));
        let conn = Connection::open(store.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
            id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, content TEXT NOT NULL,
            timestamp TEXT NOT NULL, quarantined INTEGER NOT NULL DEFAULT 0,
            valid_until TEXT, invalidated_at TEXT
        );
        INSERT INTO memories (id,agent_id,content,timestamp)
        VALUES ('m1','agent-a','A changes B','2026-01-01T00:00:00Z');",
        )
        .unwrap();
        (dir, store)
    }

    fn ccr_acl_revision(scope: &EvidenceScope) -> String {
        format!(
            "immutable-acl-sha256:{:x}",
            Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.acl))
        )
    }

    /// W3-2 regression: `install_memory_triggers` ran a `BEGIN IMMEDIATE`
    /// write transaction on every causal read to rewrite two triggers it had
    /// already written. The short circuit must be driven by a marker written
    /// in that same transaction — and must still heal a missing trigger,
    /// because the live `memories` table can appear after the causal schema.
    #[test]
    fn memory_trigger_install_stamps_its_marker_and_heals_a_dropped_trigger() {
        let (_dir, store) = memory_db();
        store.open().unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let installed_triggers = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger'
                 AND name IN ('causal_memory_after_update','causal_memory_after_delete')",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        let marked: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM causal_ccr_outbox_meta WHERE name=?1)",
                [MEMORY_TRIGGER_MARKER],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            marked,
            "the marker literal in the install batch must stay in sync with MEMORY_TRIGGER_MARKER"
        );
        assert_eq!(installed_triggers(&conn), 2);

        conn.execute_batch("DROP TRIGGER causal_memory_after_delete;")
            .unwrap();
        store.open().unwrap();
        assert_eq!(
            installed_triggers(&conn),
            2,
            "a missing trigger must clear the short circuit and be reinstalled"
        );
    }

    /// F1 regression: the memory triggers used to erase the causal copy
    /// unconditionally, so a held CCR delivery lease turned every upstream
    /// `UPDATE`/`DELETE memories` (GDPR deletion included) into a hard
    /// `RAISE(ABORT)`. The erase is now deferred instead, the in-flight lease
    /// is invalidated immediately, and the copy is scrubbed on the first open
    /// after the lease drops.
    #[test]
    fn ccr_delivery_lease_defers_memory_scrub_instead_of_aborting_upstream_writes() {
        let (_dir, store) = memory_db();
        let source = store.import_memory_source("agent-a", "m1").unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: MEMORY_ACL.into(),
        };
        let lease = store
            .acquire_ccr_delivery_lease(
                &scope,
                &source.id,
                &source.version,
                &source.content_sha256,
                &ccr_acl_revision(&scope),
            )
            .unwrap();
        assert!(lease.still_valid());

        let conn = Connection::open(store.path()).unwrap();
        conn.execute("UPDATE memories SET content='A changes C' WHERE id='m1'", [])
            .expect("a held CCR delivery lease must not abort the upstream memory write");
        let content: String = conn
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            content, "A changes B",
            "the leased copy is retained for a later retry, not erased mid-delivery"
        );
        assert!(
            !lease.still_valid(),
            "the copied bytes must stop being deliverable as soon as the memory changes"
        );
        // Every `/api/causal/*` entry point starts with `open()`; it must not
        // fail while the lease is held.
        store.open().expect("open() must not fail while a lease is held");
        conn.execute("DELETE FROM memories WHERE id='m1'", [])
            .expect("a held CCR delivery lease must not abort an upstream deletion");

        drop(lease);
        // Production opens a fresh store per request, so the next request runs
        // the deferred scrub; one instance reused here has to drop its
        // maintenance throttle to stand in for that.
        store.reset_maintenance_throttle();
        store.open().unwrap();
        let content: String = conn
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            content, "",
            "the deferred scrub must run on the first open after the lease drops"
        );
    }

    /// F2 regression: only `erase_artifact` used to scrub the copied wording
    /// that lives in `causal_claims.context_json`; a live-memory change cleared
    /// the excerpt and the artifact content but left the extracted variable
    /// names on disk.
    #[test]
    fn upstream_memory_change_also_scrubs_copied_claim_context() {
        let (_dir, store) = memory_db();
        let source = store.import_memory_source("agent-a", "m1").unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: MEMORY_ACL.into(),
        };
        let claim = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({ "original_variable_names": ["A changes B"] }),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &source.id,
                0,
                1,
                "A",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT context_json FROM causal_claims WHERE id=?1",
                [&claim.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored.contains("A changes B"));

        conn.execute("UPDATE memories SET content='A changes C' WHERE id='m1'", [])
            .unwrap();
        let scrubbed: String = conn
            .query_row(
                "SELECT context_json FROM causal_claims WHERE id=?1",
                [&claim.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            scrubbed, "{}",
            "source wording copied into claim context must be scrubbed with the excerpt"
        );
    }

    #[test]
    fn quarantine_and_delete_scrub_source_and_demote_accepted_claim() {
        let (_dir, store) = memory_db();
        let source = store.import_memory_source("agent-a", "m1").unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: MEMORY_ACL.into(),
        };
        assert!(matches!(
            store.import_memory_source("agent-b", "m1"),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store.import_memory_source("agent-a", "m1").unwrap().id,
            source.id
        );
        let claim = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &source.id,
                0,
                1,
                "A",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        store
            .review_claim(&scope, &claim.id, "reviewer", true)
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "INSERT INTO causal_models
            (id,tenant_id,acl,name,version,treatment_variable_id,outcome_variable_id,
             population,window_start,window_end,created_at)
             VALUES ('model','agent-a','agent-private','test','v1','a','b','all',0,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO causal_model_edges (model_id,claim_id) VALUES ('model',?1)",
            [&claim.id],
        )
        .unwrap();
        conn.execute("INSERT INTO causal_effect_estimates
            (id,model_id,data_snapshot_id,method,code_sha256,estimate,lower_bound,upper_bound,
             diagnostics_json,identification_state,created_at)
             VALUES ('effect','model','snapshot','test','hash',1.0,0.5,1.5,'{\"private\":1}','identified',1)", []).unwrap();
        conn.execute("UPDATE memories SET quarantined=1 WHERE id='m1'", [])
            .unwrap();
        assert!(matches!(
            store.source_text(&scope, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store.claim_state(&scope, &claim.id).unwrap(),
            "needs_review"
        );
        assert_eq!(
            store.evidence_for_claim(&scope, &claim.id).unwrap()[0]
                .span
                .excerpt,
            ""
        );
        let old_content: String = conn
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_content, "");
        let (estimate, diagnostics, state): (Option<f64>, String, String) = conn.query_row(
            "SELECT estimate,diagnostics_json,identification_state FROM causal_effect_estimates WHERE id='effect'",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(estimate, None);
        assert_eq!(diagnostics, "{}");
        assert_eq!(state, "memory_source_changed");
        conn.execute("UPDATE memories SET quarantined=0 WHERE id='m1'", [])
            .unwrap();
        let replacement = store.import_memory_source("agent-a", "m1").unwrap();
        assert_ne!(replacement.id, source.id);
        assert_ne!(replacement.version, source.version);
        conn.execute(
            "UPDATE memories SET content='A now changes C' WHERE id='m1'",
            [],
        )
        .unwrap();
        assert!(matches!(
            store.source_text(&scope, &replacement.id),
            Err(CausalStoreError::NotFound)
        ));
        let edited = store.import_memory_source("agent-a", "m1").unwrap();
        assert_ne!(edited.id, replacement.id);
        conn.execute("DELETE FROM memories WHERE id='m1'", [])
            .unwrap();
        assert!(matches!(
            store.source_text(&scope, &edited.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.import_memory_source("agent-a", "m1"),
            Err(CausalStoreError::NotFound)
        ));
    }

    #[test]
    fn upstream_expiry_bounds_causal_retention() {
        let (_dir, store) = memory_db();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "UPDATE memories SET valid_until='2099-01-01T00:00:00Z' WHERE id='m1'",
            [],
        )
        .unwrap();
        let source = store.import_memory_source("agent-a", "m1").unwrap();
        assert_eq!(
            source.retention_at,
            parse_timestamp("2099-01-01T00:00:00Z").unwrap()
        );
    }
}
