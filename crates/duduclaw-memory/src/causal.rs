//! Evidence-backed causal *claims*, separate from temporal facts and PPR edges.
//!
//! A claim extracted from text is a quotation-backed proposal, not an
//! identified causal effect. Only a reviewer can accept it, and only while
//! supporting source spans are still valid and visible in the same scope.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

/// `true` when a `try_lock_exclusive` failure means "another handle holds the
/// lock" (the lease owner is alive) rather than a real I/O error.
///
/// Unix reports contention as `EWOULDBLOCK`, which `std` maps to
/// `ErrorKind::WouldBlock`. Windows reports `ERROR_LOCK_VIOLATION`, which
/// `std` leaves as an uncategorised OS error — matching on `WouldBlock`
/// alone made every live lease look like an I/O failure on Windows
/// (2026-09-29: the four `ccr_delivery_lease_*` / `*_revocation_*` tests
/// failed only on the Windows CI leg). `fs2::lock_contended_error()` is the
/// platform-correct comparison point.
fn is_lock_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || (error.raw_os_error().is_some()
            && error.raw_os_error() == fs2::lock_contended_error().raw_os_error())
}
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::causal_alias::resolve_alias_tx;
use crate::causal_memory::install_memory_triggers;
use crate::causal_wiki::sync_wiki_sources;

const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CCR_DELIVERY_LEASES_PER_SOURCE: i64 = 128;

/// SQLite `user_version` stamped once the DDL batch and the one-off revocation
/// backfill in `open()` have run. **Bump this whenever either changes** — a
/// file already reporting this version skips both, so a silently added table,
/// index or trigger would never be created. The live-memory trigger install
/// is deliberately NOT covered: it depends on a `memories` table that can
/// appear after the causal schema, so it carries its own marker (see
/// `causal_memory::install_memory_triggers`).
const SCHEMA_VERSION: i64 = 1;

/// Shortest gap between two maintenance passes — live-Wiki resync, retention
/// expiry sweep, deferred memory scrub, orphaned lease-lock reap — on one
/// store instance.
///
/// Every caller that serves a request builds its own `CausalStore`, so this
/// does not delay a *new* request's freshness check: it collapses the repeat
/// passes inside one request that reads N claims or models (the N+1 open).
/// Egress safety does not depend on it either — `still_valid()` re-reads the
/// live Wiki file and the live memory revision on every single call.
const MAINTENANCE_INTERVAL_SECS: i64 = 30;

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Erase every copy of a source's wording that lives OUTSIDE
/// `causal_evidence.excerpt`.
///
/// A claim's `context_json` carries model-extracted variable names and the
/// operator's free-text `proposal_context`, both frequently verbatim source
/// substrings; a negative-control review's `rationale` is free text about the
/// protocol source. Erasing the artifact and its excerpts while leaving these
/// behind keeps source wording on disk past its retention or revocation, so
/// every scrub path — erase, retention expiry, live Wiki change, live memory
/// change (the trigger bodies in `causal_memory.rs` repeat this in SQL) — runs
/// this in the same transaction as the erasure.
pub(crate) fn scrub_copied_source_wording(
    tx: &rusqlite::Transaction<'_>,
    artifact_id: &str,
) -> Result<(), CausalStoreError> {
    tx.execute(
        "UPDATE causal_claims SET context_json='{}'
         WHERE id IN (
            SELECT e.claim_id FROM causal_evidence e JOIN causal_artifacts a
             ON a.id=e.artifact_id
            WHERE e.artifact_id=?1 AND a.tenant_id=causal_claims.tenant_id
             AND a.acl=causal_claims.acl)",
        [artifact_id],
    )?;
    tx.execute(
        "UPDATE causal_negative_control_reviews SET rationale=''
         WHERE protocol_artifact_id=?1",
        [artifact_id],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceScope {
    pub tenant_id: String,
    pub acl: String,
}

impl EvidenceScope {
    pub(crate) fn valid(&self) -> bool {
        !self.tenant_id.trim().is_empty() && !self.acl.trim().is_empty()
    }
}

/// `i64::MAX` is the in-database sentinel for "no retention deadline" (a Wiki
/// page or an open-ended memory lives as long as its live source does). As
/// JSON that is `9223372036854775807`, past `Number.MAX_SAFE_INTEGER`, so a
/// browser silently reads back `9223372036854776000` — a bogus deadline that
/// would compare wrong the first time anyone used the field. Serialise the
/// sentinel as `null` instead: "no expiry" is what it means, and `null` is
/// the one value no client can misread as a timestamp.
fn serialize_retention_at<S: serde::Serializer>(
    retention_at: &i64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match *retention_at {
        i64::MAX => serializer.serialize_none(),
        value => serializer.serialize_some(&value),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceArtifact {
    pub id: String,
    pub tenant_id: String,
    pub acl: String,
    pub kind: String,
    pub external_id: String,
    pub version: String,
    pub lineage_id: String,
    pub content_sha256: String,
    pub occurred_at: i64,
    pub ingested_at: i64,
    #[serde(serialize_with = "serialize_retention_at")]
    pub retention_at: i64,
}

/// Content-free, durable notice that an immutable source version must be
/// tombstoned in CCR. The causal database owns this outbox so a source change
/// and its notice commit in the same SQLite transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalCcrRevocation {
    pub tenant_id: String,
    pub connector: String,
    pub artifact_id: String,
    pub version: String,
}

/// A durable barrier between a copied CCR fragment and causal source removal.
/// The database row survives a process crash; the OS lock proves whether the
/// owning process still holds the lease, without trusting a wall-clock TTL.
#[derive(Debug)]
pub struct CausalCcrDeliveryLease {
    store: CausalStore,
    lease_id: String,
    scope: EvidenceScope,
    artifact_id: String,
    version: String,
    saved_sha256: String,
    retention_at: i64,
    lock_file: File,
    lock_path: PathBuf,
}

impl CausalCcrDeliveryLease {
    /// Recheck the live exact source and lease immediately before egress.
    /// A staged revoke intentionally makes this false, though the guard must
    /// remain held until its caller has stopped using every copied byte.
    pub fn still_valid(&self) -> bool {
        if now() >= self.retention_at || !self.store.path.is_file() {
            return false;
        }
        let Ok(conn) = Connection::open_with_flags(
            &self.store.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            return false;
        };
        let row: Result<Option<(String, String, String, i64, String, String)>, _> = conn
            .query_row(
                "SELECT a.version,a.content_sha256,a.content,a.retention_at,a.kind,a.external_id
                 FROM causal_ccr_delivery_leases l
                 JOIN causal_artifacts a ON a.id=l.artifact_id
                  AND a.tenant_id=l.tenant_id AND a.acl=l.acl
                 WHERE l.lease_id=?1 AND l.tenant_id=?2 AND l.acl=?3
                  AND l.artifact_id=?4 AND l.version=?5
                  AND a.invalidated_at IS NULL AND a.retention_at>?6
                  AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                   WHERE r.tenant_id=l.tenant_id AND r.acl=l.acl
                    AND r.artifact_id=l.artifact_id AND r.version=l.version)
                  AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                   WHERE o.tenant_id=l.tenant_id AND o.connector='causal'
                    AND o.artifact_id=l.artifact_id AND o.version=l.version
                    AND o.delivered_at IS NULL)",
                params![
                    self.lease_id,
                    self.scope.tenant_id,
                    self.scope.acl,
                    self.artifact_id,
                    self.version,
                    now()
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional();
        let Ok(Some((version, digest, content, retention_at, kind, external_id))) = row else {
            return false;
        };
        if version != self.version
            || digest != self.saved_sha256
            || retention_at != self.retention_at
            || format!("{:x}", Sha256::digest(content.as_bytes())) != digest
        {
            return false;
        }
        // A Wiki-imported source lives outside SQLite, so no trigger or
        // read-only query can notice that its page changed while the provider
        // call (or the channel send) was in flight. Compare the live file here
        // — `sync_wiki_sources` only runs inside `CausalStore::open()`, which
        // this deliberately avoids.
        if matches!(kind.as_str(), "wiki_agent" | "wiki_shared") {
            return crate::causal_wiki::live_wiki_source_matches(
                &self.store.path,
                &kind,
                &self.scope.tenant_id,
                &self.scope.acl,
                &external_id,
                &version,
                &digest,
            );
        }
        // A live-memory source pins its lifecycle revision into the version.
        // While this lease is held the memory trigger deliberately skips the
        // artifact erase (aborting it would fail the upstream write), so the
        // revision counter — which the trigger always bumps — is the only
        // in-flight signal that the upstream memory was edited, quarantined or
        // deleted. Read-only, on the connection already open.
        if kind == "memory" {
            let Some((_, pinned)) = version.rsplit_once(':') else {
                return false;
            };
            let Ok(pinned) = pinned.parse::<i64>() else {
                return false;
            };
            // Fail closed: the revisions table always exists wherever a
            // `memory` artifact does (`import_memory_source` installs it), so a
            // failure here means the bridge is gone, not that nothing changed.
            let Ok(current) = conn.query_row(
                "SELECT COALESCE((SELECT revision FROM causal_memory_revisions
                 WHERE memory_id=?1),0)",
                params![external_id],
                |row| row.get::<_, i64>(0),
            ) else {
                return false;
            };
            return current == pinned;
        }
        true
    }
}

impl Drop for CausalCcrDeliveryLease {
    fn drop(&mut self) {
        // A failed deletion leaves a durable row. A revoker can reclaim it
        // after this OS lock is released, never merely because time passed.
        let mut removed = false;
        if let Ok(conn) = Connection::open_with_flags(
            &self.store.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        ) {
            let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
            removed = conn
                .execute(
                    "DELETE FROM causal_ccr_delivery_leases WHERE lease_id=?1",
                    [&self.lease_id],
                )
                .is_ok();
        }
        let _ = self.lock_file.unlock();
        if removed {
            let _ = std::fs::remove_file(&self.lock_path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimModality {
    Asserted,
    Speculated,
    Negated,
    Questioned,
}

impl ClaimModality {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Asserted => "asserted",
            Self::Speculated => "speculated",
            Self::Negated => "negated",
            Self::Questioned => "questioned",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStance {
    Supports,
    Opposes,
}

impl EvidenceStance {
    fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Opposes => "opposes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CausalClaim {
    pub id: String,
    pub scope: EvidenceScope,
    pub cause_variable: String,
    pub effect_variable: String,
    pub lag_min_seconds: i64,
    pub lag_max_seconds: i64,
    pub context_json: String,
    pub modality: ClaimModality,
    pub review_state: String,
    pub reviewer: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceSpan {
    pub id: String,
    pub claim_id: String,
    pub artifact_id: String,
    pub span_start: usize,
    pub span_end: usize,
    pub excerpt: String,
    pub stance: EvidenceStance,
    pub speaker_id: Option<String>,
    pub extractor_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceDetail {
    pub span: EvidenceSpan,
    pub source_lineage_id: String,
    pub source_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimLineageSummary {
    pub supporting_spans: u64,
    pub opposing_spans: u64,
    pub independent_supporting_lineages: usize,
    pub independent_opposing_lineages: usize,
    pub mixed_stance_lineages: usize,
}

/// A model's structured proposal. It remains a candidate claim after ingest;
/// the evidence excerpt is validated against the original artifact bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedCausalClaim {
    pub cause_variable: String,
    pub effect_variable: String,
    pub lag_min_seconds: i64,
    pub lag_max_seconds: i64,
    pub modality: ClaimModality,
    pub stance: EvidenceStance,
    pub span_start: usize,
    pub span_end: usize,
    pub excerpt: String,
    pub speaker_id: Option<String>,
    pub context: serde_json::Value,
}

pub(crate) fn valid_proposal(proposal: &ProposedCausalClaim) -> bool {
    !proposal.cause_variable.trim().is_empty()
        && !proposal.effect_variable.trim().is_empty()
        && proposal.cause_variable != proposal.effect_variable
        && proposal.lag_min_seconds >= 0
        && proposal.lag_max_seconds >= proposal.lag_min_seconds
        && proposal.span_start < proposal.span_end
        && !proposal.excerpt.is_empty()
        && proposal.context.is_object()
}

#[derive(Debug, thiserror::Error)]
pub enum CausalStoreError {
    #[error("invalid causal evidence input")]
    InvalidInput,
    #[error("source or claim not found in scope")]
    NotFound,
    #[error("causal review state changed before this decision")]
    Conflict,
    /// The Wiki delivery fence was held by another participant. Distinct from
    /// `InvalidInput` on purpose: nothing about the request was wrong, and the
    /// caller should retry rather than be told its input was malformed.
    #[error("wiki trust state is busy; retry")]
    Busy,
    #[error("accepted claims require an active supporting source span")]
    MissingSupport,
    #[error("causal store SQLite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("causal store filesystem failure: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct CausalStore {
    path: PathBuf,
    /// Unix seconds of the last maintenance pass, shared by every clone of
    /// this instance (a delivery lease holds one). `0` means "never", so the
    /// first `open()` of an instance always runs the pass.
    last_maintenance: Arc<AtomicI64>,
}

impl CausalStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_maintenance: Arc::new(AtomicI64::new(0)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take the maintenance slot for this instance, or report that another
    /// open within `MAINTENANCE_INTERVAL_SECS` already did the work. The
    /// compare-and-swap makes two concurrent opens of one instance run the
    /// pass once, not twice.
    fn claim_maintenance_slot(&self) -> bool {
        let now = now();
        let last = self.last_maintenance.load(Ordering::Acquire);
        if last != 0 && now.saturating_sub(last) < MAINTENANCE_INTERVAL_SECS {
            return false;
        }
        self.last_maintenance
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Make the next `open()` run maintenance again. Production builds a
    /// fresh store per request, so only tests that reuse one instance across
    /// an out-of-band source change need this.
    #[cfg(test)]
    pub(crate) fn reset_maintenance_throttle(&self) {
        self.last_maintenance.store(0, Ordering::Release);
    }

    fn ccr_lease_lock_path(&self, lease_id: &str) -> PathBuf {
        let mut dir = self.path.as_os_str().to_os_string();
        dir.push(".ccr-leases");
        PathBuf::from(dir).join(format!("{lease_id}.lock"))
    }

    /// Acquire a durable causal-source disclosure lease. The content digest,
    /// immutable version, exact tenant/ACL and retention are all checked in
    /// the same writer transaction that creates the lease row. A staged
    /// source revocation wins the race by excluding new leases first.
    pub fn acquire_ccr_delivery_lease(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
        version: &str,
        saved_sha256: &str,
        acl_revision: &str,
    ) -> Result<CausalCcrDeliveryLease, CausalStoreError> {
        if !scope.valid()
            || artifact_id.trim().is_empty()
            || version.trim().is_empty()
            || saved_sha256.len() != 64
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let expected_acl = format!(
            "immutable-acl-sha256:{:x}",
            Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.acl))
        );
        if acl_revision != expected_acl || !self.path.is_file() {
            return Err(CausalStoreError::NotFound);
        }
        let lease_id = Uuid::new_v4().to_string();
        let lock_path = self.ccr_lease_lock_path(&lease_id);
        let parent = lock_path.parent().ok_or(CausalStoreError::InvalidInput)?;
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock_file = options.open(&lock_path)?;
        if let Err(error) = lock_file.lock_exclusive() {
            let _ = std::fs::remove_file(&lock_path);
            return Err(error.into());
        }
        let result = (|| {
            let mut conn = self.open()?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let source: Option<(String, String, String, i64)> = tx
                .query_row(
                    "SELECT version,content_sha256,content,retention_at
                     FROM causal_artifacts a WHERE id=?1 AND tenant_id=?2 AND acl=?3
                     AND invalidated_at IS NULL AND retention_at>?4
                     AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                      WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                       AND r.artifact_id=a.id AND r.version=a.version)
                     AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                      WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                       AND o.artifact_id=a.id AND o.version=a.version
                       AND o.delivered_at IS NULL)",
                    params![artifact_id, scope.tenant_id, scope.acl, now()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let (actual_version, digest, content, retention_at) =
                source.ok_or(CausalStoreError::NotFound)?;
            if actual_version != version
                || digest != saved_sha256
                || format!("{:x}", Sha256::digest(content.as_bytes())) != digest
            {
                return Err(CausalStoreError::NotFound);
            }
            let active: i64 = tx.query_row(
                "SELECT COUNT(*) FROM causal_ccr_delivery_leases
                 WHERE tenant_id=?1 AND acl=?2 AND artifact_id=?3 AND version=?4",
                params![scope.tenant_id, scope.acl, artifact_id, version],
                |row| row.get(0),
            )?;
            if active >= MAX_CCR_DELIVERY_LEASES_PER_SOURCE {
                return Err(CausalStoreError::Conflict);
            }
            tx.execute(
                "INSERT INTO causal_ccr_delivery_leases
                 (lease_id,tenant_id,acl,artifact_id,version,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    lease_id,
                    scope.tenant_id,
                    scope.acl,
                    artifact_id,
                    version,
                    now()
                ],
            )?;
            tx.commit()?;
            Ok(retention_at)
        })();
        match result {
            Ok(retention_at) => Ok(CausalCcrDeliveryLease {
                store: self.clone(),
                lease_id,
                scope: scope.clone(),
                artifact_id: artifact_id.to_owned(),
                version: version.to_owned(),
                saved_sha256: saved_sha256.to_owned(),
                retention_at,
                lock_file,
                lock_path,
            }),
            Err(error) => {
                let _ = lock_file.unlock();
                let _ = std::fs::remove_file(lock_path);
                Err(error)
            }
        }
    }

    /// Fence the exact current version before a multi-database CCR/source
    /// revoke. A retry leaves the fence in place and returns the same version.
    pub fn begin_ccr_revocation(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() || !self.path.is_file() {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: String = tx
            .query_row(
                "SELECT version FROM causal_artifacts
                 WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![artifact_id, scope.tenant_id, scope.acl],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)?;
        tx.execute(
            "INSERT OR IGNORE INTO causal_ccr_revoking
             (tenant_id,acl,artifact_id,version,started_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![scope.tenant_id, scope.acl, artifact_id, version, now()],
        )?;
        tx.commit()?;
        Ok(version)
    }

    /// Undo a revocation that never got past its fence.
    ///
    /// `begin_ccr_revocation` is insert-only on purpose: a source is hidden
    /// the moment a revoke starts, and a retry finishes the remaining steps.
    /// That left no way back when an operator abandoned a revoke that had
    /// stopped at the fence (a live lease, or a later failed UPDATE), so the
    /// source stayed invisible forever while the UI showed only "busy".
    ///
    /// This is NOT "un-revoke": it only removes a fence whose revocation
    /// demonstrably never took effect. It refuses (`Conflict`) when the
    /// artifact is already invalidated, when a tombstone notice is already
    /// queued for CCR, or while any delivery lease for that version is still
    /// recorded — in each of those cases something downstream has already
    /// acted on the revocation, and restoring visibility would contradict it.
    /// Returns the fenced version that was cleared.
    pub fn clear_revocation_fence(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() || !self.path.is_file() {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: String = tx
            .query_row(
                "SELECT version FROM causal_ccr_revoking
                 WHERE tenant_id=?1 AND acl=?2 AND artifact_id=?3",
                params![scope.tenant_id, scope.acl, artifact_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)?;
        let active: Option<bool> = tx
            .query_row(
                "SELECT invalidated_at IS NULL AND content<>'' FROM causal_artifacts
                 WHERE id=?1 AND tenant_id=?2 AND acl=?3 AND version=?4",
                params![artifact_id, scope.tenant_id, scope.acl, version],
                |row| row.get(0),
            )
            .optional()?;
        if active != Some(true) {
            return Err(CausalStoreError::Conflict);
        }
        let blocked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_ccr_revocation_outbox o
               WHERE o.tenant_id=?1 AND o.connector='causal'
                AND o.artifact_id=?2 AND o.version=?3 AND o.delivered_at IS NULL)
             OR EXISTS(SELECT 1 FROM causal_ccr_delivery_leases l
               WHERE l.tenant_id=?1 AND l.acl=?4 AND l.artifact_id=?2 AND l.version=?3)",
            params![scope.tenant_id, artifact_id, version, scope.acl],
            |row| row.get(0),
        )?;
        if blocked {
            return Err(CausalStoreError::Conflict);
        }
        tx.execute(
            "DELETE FROM causal_ccr_revoking
             WHERE tenant_id=?1 AND acl=?2 AND artifact_id=?3 AND version=?4",
            params![scope.tenant_id, scope.acl, artifact_id, version],
        )?;
        tx.commit()?;
        Ok(version)
    }

    /// Reclaim only leases whose owner OS lock can be acquired. A missing
    /// lockfile stays blocked because owner death cannot be established.
    fn ensure_ccr_delivery_drained(
        &self,
        tx: &rusqlite::Transaction<'_>,
        scope: &EvidenceScope,
        artifact_id: &str,
        version: &str,
    ) -> Result<Vec<PathBuf>, CausalStoreError> {
        let mut statement = tx.prepare(
            "SELECT lease_id FROM causal_ccr_delivery_leases
             WHERE tenant_id=?1 AND acl=?2 AND artifact_id=?3 AND version=?4",
        )?;
        let ids = statement
            .query_map(
                params![scope.tenant_id, scope.acl, artifact_id, version],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut reclaimed = Vec::new();
        for id in ids {
            let path = self.ccr_lease_lock_path(&id);
            let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
                continue;
            };
            match file.try_lock_exclusive() {
                Ok(()) => {
                    tx.execute(
                        "DELETE FROM causal_ccr_delivery_leases WHERE lease_id=?1
                         AND tenant_id=?2 AND acl=?3 AND artifact_id=?4 AND version=?5",
                        params![id, scope.tenant_id, scope.acl, artifact_id, version],
                    )?;
                    let _ = file.unlock();
                    reclaimed.push(path);
                    // Keep the lockfile until the transaction commits. If a
                    // different live lease forces rollback, removing it here
                    // would make this orphan impossible to prove dead later.
                }
                Err(error) if is_lock_contended(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let active: i64 = tx.query_row(
            "SELECT COUNT(*) FROM causal_ccr_delivery_leases
             WHERE tenant_id=?1 AND acl=?2 AND artifact_id=?3 AND version=?4",
            params![scope.tenant_id, scope.acl, artifact_id, version],
            |row| row.get(0),
        )?;
        if active > 0 {
            return Err(CausalStoreError::Conflict);
        }
        Ok(reclaimed)
    }

    /// Return at most `limit` pending causal-to-CCR notices. Opening the store
    /// also performs the normal retention sweep, which queues expiry notices.
    pub fn pending_ccr_revocations(
        &self,
        limit: usize,
    ) -> Result<Vec<CausalCcrRevocation>, CausalStoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT tenant_id,connector,artifact_id,version FROM causal_ccr_revocation_outbox
             WHERE delivered_at IS NULL AND connector='causal'
             ORDER BY queued_at, rowid LIMIT ?1",
        )?;
        let notices = statement
            .query_map([limit.min(512) as i64], |row| {
                Ok(CausalCcrRevocation {
                    tenant_id: row.get(0)?,
                    connector: row.get(1)?,
                    artifact_id: row.get(2)?,
                    version: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(notices)
    }

    /// Remove the notice only after CCR durably tombstones this exact version.
    /// CCR's own tombstone makes repeated delivery idempotent; this keeps the
    /// outbox proportional to pending work instead of lifetime source count.
    pub fn acknowledge_ccr_revocation(
        &self,
        notice: &CausalCcrRevocation,
    ) -> Result<(), CausalStoreError> {
        if notice.connector != "causal" {
            return Err(CausalStoreError::InvalidInput);
        }
        if !self.path.is_file() {
            return Err(CausalStoreError::NotFound);
        }
        // The preceding outbox read installed the schema and ran source
        // expiry. Acknowledgment must stay cheap even for a large backlog.
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute(
            "DELETE FROM causal_ccr_revocation_outbox
             WHERE tenant_id=?1 AND connector=?2 AND artifact_id=?3 AND version=?4
             AND delivered_at IS NULL",
            params![
                notice.tenant_id,
                notice.connector,
                notice.artifact_id,
                notice.version
            ],
        )?;
        Ok(())
    }

    /// Return an active source only within its exact tenant and ACL scope.
    /// This is a point-in-time read. External dispatch must use
    /// `source_text_with_delivery_lease` and retain its guard through egress.
    pub fn source_text(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let (content, digest): (String, String) = self
            .open()?
            .query_row(
                "SELECT a.content,a.content_sha256 FROM causal_artifacts a
                 WHERE a.id=?1 AND a.tenant_id=?2 AND a.acl=?3
                 AND a.invalidated_at IS NULL AND a.retention_at>?4
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                  WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                   AND r.artifact_id=a.id AND r.version=a.version)
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                  WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                   AND o.artifact_id=a.id AND o.version=a.version
                   AND o.delivered_at IS NULL)",
                params![artifact_id, scope.tenant_id, scope.acl, now()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)?;
        if format!("{:x}", Sha256::digest(content.as_bytes())) != digest {
            return Err(CausalStoreError::InvalidInput);
        }
        Ok(content)
    }

    /// Read source text under a durable delivery lease. The caller must
    /// recheck `lease.still_valid()` immediately before external egress and
    /// keep the lease alive until the provider call and response processing
    /// finish. A revoke staged before lease acquisition or text read fails.
    pub fn source_text_with_delivery_lease(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<(String, CausalCcrDeliveryLease), CausalStoreError> {
        let metadata = self.read_artifact_metadata(scope, artifact_id)?;
        let acl_revision = format!(
            "immutable-acl-sha256:{:x}",
            Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.acl))
        );
        let lease = self.acquire_ccr_delivery_lease(
            scope,
            artifact_id,
            &metadata.version,
            &metadata.content_sha256,
            &acl_revision,
        )?;
        let content = self.source_text(scope, artifact_id)?;
        if !lease.still_valid() {
            return Err(CausalStoreError::NotFound);
        }
        Ok((content, lease))
    }

    /// Check whether an artifact record exists in this exact scope, including
    /// invalidated and expired records. This does not expose source content.
    pub fn source_record_exists(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<bool, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        Ok(self.open()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_artifacts WHERE id=?1 AND tenant_id=?2 AND acl=?3)",
            params![artifact_id, scope.tenant_id, scope.acl],
            |row| row.get(0),
        )?)
    }

    /// Immutable version of an exact scoped record, including invalidated
    /// records. Cross-store revocation must be retryable after the first
    /// source transaction commits; no source text is exposed here.
    pub fn source_record_version(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        self.open()?
            .query_row(
                "SELECT version FROM causal_artifacts WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![artifact_id, scope.tenant_id, scope.acl],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)
    }

    /// Active metadata for an artifact in the requester's exact scope.
    /// Source text is never returned; the digest is checked against stored bytes.
    pub fn read_artifact_metadata(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<SourceArtifact, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let row = self
            .open()?
            .query_row(
                "SELECT kind,external_id,version,lineage_id,content_sha256,content,
             occurred_at,ingested_at,retention_at FROM causal_artifacts
             WHERE id=?1 AND tenant_id=?2 AND acl=?3
             AND invalidated_at IS NULL AND retention_at>?4
             AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
              WHERE r.tenant_id=causal_artifacts.tenant_id
               AND r.acl=causal_artifacts.acl
               AND r.artifact_id=causal_artifacts.id
               AND r.version=causal_artifacts.version)
             AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
              WHERE o.tenant_id=causal_artifacts.tenant_id AND o.connector='causal'
               AND o.artifact_id=causal_artifacts.id
               AND o.version=causal_artifacts.version
               AND o.delivered_at IS NULL)",
                params![artifact_id, scope.tenant_id, scope.acl, now()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)?;
        let (
            kind,
            external_id,
            version,
            lineage_id,
            content_sha256,
            content,
            occurred_at,
            ingested_at,
            retention_at,
        ) = row;
        if format!("{:x}", Sha256::digest(content.as_bytes())) != content_sha256 {
            return Err(CausalStoreError::InvalidInput);
        }
        Ok(SourceArtifact {
            id: artifact_id.into(),
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            kind,
            external_id,
            version,
            lineage_id,
            content_sha256,
            occurred_at,
            ingested_at,
            retention_at,
        })
    }

    /// One connection for a multi-row read, to be passed to the
    /// `*_with_conn` helpers. It runs the same schema check and live-source
    /// freshness pass a single `read_*` call would, once instead of per row.
    /// Writes stay behind the store's own methods.
    pub fn read_connection(&self) -> Result<Connection, CausalStoreError> {
        self.open()
    }

    pub(crate) fn open(&self) -> Result<Connection, CausalStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        // Every causal read opens a connection. A file already carrying the
        // current schema must not pay for the ~30-statement DDL batch and the
        // backfill's `BEGIN IMMEDIATE` window, which serialise pure reads
        // against any concurrent writer.
        // The `memories` table shares this file, so `user_version` is not
        // ours alone: pair it with proof that the causal tables are actually
        // there, or a future co-writer stamping the same number would silently
        // skip creating them.
        let schema_current: bool = conn.query_row(
            "SELECT (SELECT * FROM pragma_user_version)=?1
             AND EXISTS(SELECT 1 FROM sqlite_master
              WHERE type='table' AND name='causal_artifacts')",
            [SCHEMA_VERSION],
            |row| row.get(0),
        )?;
        if schema_current {
            conn.pragma_update(None, "foreign_keys", "ON")?;
        } else {
            self.install_schema(&mut conn)?;
        }
        install_memory_triggers(&conn)?;
        if self.claim_maintenance_slot() {
            self.run_maintenance(&mut conn)?;
        }
        Ok(conn)
    }

    /// Create the table/index/trigger set and run the one-off revocation
    /// backfill, then stamp `user_version` so later opens skip both.
    fn install_schema(&self, conn: &mut Connection) -> Result<(), CausalStoreError> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS causal_artifacts (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                kind TEXT NOT NULL, external_id TEXT NOT NULL, version TEXT NOT NULL,
                lineage_id TEXT NOT NULL, content_sha256 TEXT NOT NULL,
                content TEXT NOT NULL, occurred_at INTEGER NOT NULL,
                ingested_at INTEGER NOT NULL, retention_at INTEGER NOT NULL,
                invalidated_at INTEGER
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_causal_artifact_version
                ON causal_artifacts(tenant_id, acl, kind, external_id, version);
             CREATE TABLE IF NOT EXISTS causal_ccr_revocation_outbox (
                tenant_id TEXT NOT NULL, connector TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                version TEXT NOT NULL, queued_at INTEGER NOT NULL,
                delivered_at INTEGER,
                PRIMARY KEY(tenant_id, connector, artifact_id, version)
             );
             CREATE INDEX IF NOT EXISTS idx_causal_ccr_revocation_pending
                ON causal_ccr_revocation_outbox(delivered_at,queued_at);
             CREATE TABLE IF NOT EXISTS causal_ccr_delivery_leases (
                lease_id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL,
                acl TEXT NOT NULL, artifact_id TEXT NOT NULL,
                version TEXT NOT NULL, created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_causal_ccr_delivery_source
                ON causal_ccr_delivery_leases(tenant_id,acl,artifact_id,version);
             CREATE TABLE IF NOT EXISTS causal_ccr_revoking (
                tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                artifact_id TEXT NOT NULL, version TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                PRIMARY KEY(tenant_id,acl,artifact_id,version)
             );
             CREATE TABLE IF NOT EXISTS causal_ccr_outbox_meta (
                name TEXT PRIMARY KEY
             );
             CREATE TRIGGER IF NOT EXISTS causal_ccr_prevent_leased_update
             BEFORE UPDATE ON causal_artifacts
             WHEN (OLD.id IS NOT NEW.id
               OR OLD.tenant_id IS NOT NEW.tenant_id
               OR OLD.acl IS NOT NEW.acl
               OR OLD.version IS NOT NEW.version
               OR OLD.content_sha256 IS NOT NEW.content_sha256
               OR OLD.content IS NOT NEW.content
               OR OLD.retention_at IS NOT NEW.retention_at
               OR OLD.invalidated_at IS NOT NEW.invalidated_at)
              AND EXISTS (SELECT 1 FROM causal_ccr_delivery_leases l
               WHERE l.tenant_id=OLD.tenant_id AND l.acl=OLD.acl
                AND l.artifact_id=OLD.id AND l.version=OLD.version)
             BEGIN
               SELECT RAISE(ABORT,'causal CCR delivery lease active');
             END;
             CREATE TRIGGER IF NOT EXISTS causal_ccr_prevent_leased_delete
             BEFORE DELETE ON causal_artifacts
             WHEN EXISTS (SELECT 1 FROM causal_ccr_delivery_leases l
               WHERE l.tenant_id=OLD.tenant_id AND l.acl=OLD.acl
                AND l.artifact_id=OLD.id AND l.version=OLD.version)
             BEGIN
               SELECT RAISE(ABORT,'causal CCR delivery lease active');
             END;
             CREATE TRIGGER IF NOT EXISTS causal_ccr_artifact_changed
             AFTER UPDATE ON causal_artifacts
             WHEN OLD.id IS NOT NEW.id
               OR OLD.tenant_id IS NOT NEW.tenant_id
               OR OLD.acl IS NOT NEW.acl
               OR OLD.version IS NOT NEW.version
               OR OLD.content_sha256 IS NOT NEW.content_sha256
               OR OLD.content IS NOT NEW.content
               OR OLD.retention_at IS NOT NEW.retention_at
               OR OLD.invalidated_at IS NOT NEW.invalidated_at
             BEGIN
               INSERT OR IGNORE INTO causal_ccr_revocation_outbox
               (tenant_id,connector,artifact_id,version,queued_at)
               VALUES (OLD.tenant_id,'causal',OLD.id,OLD.version,CAST(strftime('%s','now') AS INTEGER));
             END;
             CREATE TRIGGER IF NOT EXISTS causal_ccr_artifact_deleted
             AFTER DELETE ON causal_artifacts
             BEGIN
               INSERT OR IGNORE INTO causal_ccr_revocation_outbox
               (tenant_id,connector,artifact_id,version,queued_at)
               VALUES (OLD.tenant_id,'causal',OLD.id,OLD.version,CAST(strftime('%s','now') AS INTEGER));
             END;
             CREATE TABLE IF NOT EXISTS causal_claims (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                cause_variable TEXT NOT NULL, effect_variable TEXT NOT NULL,
                lag_min_seconds INTEGER NOT NULL, lag_max_seconds INTEGER NOT NULL,
                context_json TEXT NOT NULL, modality TEXT NOT NULL,
                review_state TEXT NOT NULL DEFAULT 'candidate', reviewer TEXT,
                created_at INTEGER NOT NULL, reviewed_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS causal_claim_reviews (
                id TEXT PRIMARY KEY, claim_id TEXT NOT NULL REFERENCES causal_claims(id),
                reviewer TEXT NOT NULL, decision TEXT NOT NULL,
                reviewed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS causal_claim_revisions (
                id TEXT PRIMARY KEY, old_claim_id TEXT NOT NULL REFERENCES causal_claims(id),
                new_claim_id TEXT NOT NULL UNIQUE REFERENCES causal_claims(id),
                reviewer TEXT NOT NULL, note TEXT NOT NULL, revised_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS causal_evidence (
                id TEXT PRIMARY KEY, claim_id TEXT NOT NULL REFERENCES causal_claims(id),
                artifact_id TEXT NOT NULL REFERENCES causal_artifacts(id),
                span_start INTEGER NOT NULL, span_end INTEGER NOT NULL,
                excerpt TEXT NOT NULL, stance TEXT NOT NULL, speaker_id TEXT,
                extractor_version TEXT NOT NULL,
                UNIQUE(claim_id, artifact_id, span_start, span_end, stance)
             );
             CREATE TABLE IF NOT EXISTS causal_lineage_claims (
                tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                lineage_id TEXT NOT NULL, semantic_sha256 TEXT NOT NULL,
                claim_id TEXT NOT NULL REFERENCES causal_claims(id),
                PRIMARY KEY(tenant_id, acl, lineage_id, semantic_sha256)
             );
             CREATE TABLE IF NOT EXISTS causal_variable_aliases (
                tenant_id TEXT NOT NULL, acl TEXT NOT NULL, alias TEXT NOT NULL,
                canonical_name TEXT NOT NULL, review_id TEXT NOT NULL,
                reviewer TEXT NOT NULL, reviewed_at INTEGER NOT NULL,
                PRIMARY KEY(tenant_id, acl, alias)
             );
             CREATE TABLE IF NOT EXISTS causal_alias_reviews (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                alias TEXT NOT NULL, canonical_name TEXT,
                decision TEXT NOT NULL, reviewer TEXT NOT NULL, reviewed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS causal_claim_alias_dependencies (
                claim_id TEXT NOT NULL REFERENCES causal_claims(id),
                alias TEXT NOT NULL, review_id TEXT NOT NULL,
                PRIMARY KEY(claim_id, alias, review_id)
             );
             CREATE INDEX IF NOT EXISTS idx_causal_claim_scope
                ON causal_claims(tenant_id, acl, review_state);
             CREATE INDEX IF NOT EXISTS idx_causal_evidence_artifact
                ON causal_evidence(artifact_id);
             CREATE TABLE IF NOT EXISTS causal_variables (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                name TEXT NOT NULL, version TEXT NOT NULL,
                definition TEXT NOT NULL, unit TEXT NOT NULL,
                value_kind TEXT NOT NULL, created_at INTEGER NOT NULL,
                UNIQUE(tenant_id, acl, name, version)
             );
             CREATE TABLE IF NOT EXISTS causal_models (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
                name TEXT NOT NULL, version TEXT NOT NULL,
                treatment_variable_id TEXT NOT NULL, outcome_variable_id TEXT NOT NULL,
                population TEXT NOT NULL, window_start INTEGER NOT NULL,
                window_end INTEGER NOT NULL, review_state TEXT NOT NULL DEFAULT 'draft',
                reviewer TEXT, reviewed_at INTEGER,
                conflicts_acknowledged INTEGER NOT NULL DEFAULT 0,
                acknowledged_opposition_digest TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                UNIQUE(tenant_id, acl, name, version)
             );
             CREATE TABLE IF NOT EXISTS causal_model_reviews (
                id TEXT PRIMARY KEY, model_id TEXT NOT NULL REFERENCES causal_models(id),
                reviewer TEXT NOT NULL, decision TEXT NOT NULL,
                acknowledged_opposition_digest TEXT NOT NULL,
                reviewed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS causal_model_variables (
                model_id TEXT NOT NULL REFERENCES causal_models(id),
                variable_id TEXT NOT NULL REFERENCES causal_variables(id),
                PRIMARY KEY(model_id, variable_id)
             );
             CREATE TABLE IF NOT EXISTS causal_model_edges (
                model_id TEXT NOT NULL REFERENCES causal_models(id),
                claim_id TEXT NOT NULL REFERENCES causal_claims(id),
                PRIMARY KEY(model_id, claim_id)
             );
             CREATE TABLE IF NOT EXISTS causal_assumptions (
                model_id TEXT NOT NULL REFERENCES causal_models(id),
                kind TEXT NOT NULL, verdict TEXT NOT NULL,
                rationale TEXT NOT NULL, reviewer TEXT NOT NULL,
                reviewed_at INTEGER NOT NULL, review_id TEXT NOT NULL,
                PRIMARY KEY(model_id, kind)
             );
             CREATE TABLE IF NOT EXISTS causal_assumption_reviews (
                id TEXT PRIMARY KEY, model_id TEXT NOT NULL REFERENCES causal_models(id),
                kind TEXT NOT NULL, verdict TEXT NOT NULL,
                rationale TEXT NOT NULL, reviewer TEXT NOT NULL,
                reviewed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS causal_effect_estimates (
                id TEXT PRIMARY KEY, model_id TEXT NOT NULL REFERENCES causal_models(id),
                data_snapshot_id TEXT NOT NULL, method TEXT NOT NULL,
                code_sha256 TEXT NOT NULL, estimate REAL, lower_bound REAL,
                upper_bound REAL, diagnostics_json TEXT NOT NULL,
                identification_state TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS causal_negative_control_reviews (
                id TEXT PRIMARY KEY,
                model_id TEXT NOT NULL REFERENCES causal_models(id),
                variable_id TEXT NOT NULL REFERENCES causal_variables(id),
                protocol_artifact_id TEXT NOT NULL REFERENCES causal_artifacts(id),
                protocol_sha256 TEXT NOT NULL,
                verdict TEXT NOT NULL,
                rationale TEXT NOT NULL,
                reviewer TEXT NOT NULL,
                reviewed_at INTEGER NOT NULL
             );",
        )?;
        let backfill_complete: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_ccr_outbox_meta
             WHERE name='initial_backfill_complete')",
            [],
            |row| row.get(0),
        )?;
        if !backfill_complete {
            // Table creation is a separate schema step. If the process dies
            // before this transaction commits, the missing marker forces a
            // retry on the next open instead of silently losing old sources.
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT OR IGNORE INTO causal_ccr_revocation_outbox
                 (tenant_id,connector,artifact_id,version,queued_at)
                 SELECT tenant_id,'causal',id,version,?1 FROM causal_artifacts
                 WHERE invalidated_at IS NOT NULL OR retention_at<=?1 OR content=''",
                [now()],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO causal_ccr_outbox_meta(name)
                 VALUES ('initial_backfill_complete')",
                [],
            )?;
            tx.commit()?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    /// Live-source resync, retention expiry, deferred memory scrub and
    /// orphaned lease-lock reap. Throttled by `claim_maintenance_slot`.
    fn run_maintenance(&self, conn: &mut Connection) -> Result<(), CausalStoreError> {
        sync_wiki_sources(conn, &self.path)?;
        let cutoff = now();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE causal_effect_estimates SET estimate=NULL, lower_bound=NULL,
             upper_bound=NULL, diagnostics_json='{}', identification_state='source_expired'
             WHERE data_snapshot_id IN (
                SELECT id FROM causal_artifacts WHERE retention_at<=?1 AND content<>''
             ) OR model_id IN (
                SELECT me.model_id FROM causal_model_edges me JOIN causal_evidence e
                ON e.claim_id=me.claim_id JOIN causal_artifacts a ON a.id=e.artifact_id
                WHERE a.retention_at<=?1 AND a.content<>''
             ) OR EXISTS (
                SELECT 1 FROM causal_negative_control_reviews r
                JOIN causal_artifacts a ON a.id=r.protocol_artifact_id
                WHERE a.retention_at<=?1 AND a.content<>''
                  AND r.model_id=causal_effect_estimates.model_id
                  AND r.variable_id=json_extract(causal_effect_estimates.diagnostics_json,
                    '$.negative_control.variable_id')
                  AND r.id=json_extract(causal_effect_estimates.diagnostics_json,
                    '$.negative_control.review_id')
             )",
            [cutoff],
        )?;
        tx.execute(
            "UPDATE causal_claims SET review_state='needs_review', reviewer=NULL, reviewed_at=NULL
             WHERE review_state='accepted' AND id IN (
                SELECT e.claim_id FROM causal_evidence e JOIN causal_artifacts a ON a.id=e.artifact_id
                WHERE a.retention_at<=?1 AND a.content<>''
             )",
            [cutoff],
        )?;
        tx.execute(
            "UPDATE causal_evidence SET excerpt='' WHERE artifact_id IN (
                SELECT id FROM causal_artifacts WHERE retention_at<=?1 AND content<>''
             )",
            [cutoff],
        )?;
        // Retention expiry is a deletion promise, so the copied wording that
        // lives outside `causal_evidence.excerpt` expires with it. Collected
        // by id first because the shared scrub is per-artifact; expiring
        // sources are a bounded set.
        let expiring: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM causal_artifacts WHERE retention_at<=?1 AND content<>''",
            )?;
            let rows = stmt.query_map([cutoff], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for id in &expiring {
            scrub_copied_source_wording(&tx, id)?;
        }
        tx.execute(
            "UPDATE causal_artifacts SET content='', invalidated_at=?1
             WHERE retention_at<=?1 AND content<>''
             AND NOT EXISTS (SELECT 1 FROM causal_ccr_delivery_leases l
              WHERE l.tenant_id=causal_artifacts.tenant_id
               AND l.acl=causal_artifacts.acl
               AND l.artifact_id=causal_artifacts.id
               AND l.version=causal_artifacts.version)",
            [cutoff],
        )?;
        // Deferred memory scrubs: a live-memory change whose causal copy was
        // held by a CCR delivery lease leaves the artifact behind (the trigger
        // skips it rather than aborting the upstream write — see
        // `causal_memory::install_memory_triggers`). The revision counter has
        // already moved on, so the stale copy is erased here on the first open
        // after the lease drops.
        let deferred: Vec<String> = {
            let has_revisions: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type='table' AND name='causal_memory_revisions')",
                [],
                |row| row.get(0),
            )?;
            if has_revisions {
                let mut stmt = tx.prepare(
                    "SELECT id FROM causal_artifacts
                     WHERE kind='memory' AND content<>'' AND instr(version,':')>0
                      AND CAST(substr(version,instr(version,':')+1) AS INTEGER) <>
                          COALESCE((SELECT revision FROM causal_memory_revisions r
                            WHERE r.memory_id=causal_artifacts.external_id),0)
                      AND NOT EXISTS (SELECT 1 FROM causal_ccr_delivery_leases l
                       WHERE l.tenant_id=causal_artifacts.tenant_id
                        AND l.acl=causal_artifacts.acl
                        AND l.artifact_id=causal_artifacts.id
                        AND l.version=causal_artifacts.version)",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            }
        };
        for id in &deferred {
            tx.execute(
                "UPDATE causal_evidence SET excerpt='' WHERE artifact_id=?1",
                [id],
            )?;
            scrub_copied_source_wording(&tx, id)?;
            tx.execute(
                "UPDATE causal_artifacts SET content='', invalidated_at=?2
                 WHERE id=?1 AND content<>''",
                params![id, cutoff],
            )?;
        }
        let live_lease_ids: std::collections::HashSet<String> = {
            let mut stmt = tx.prepare("SELECT lease_id FROM causal_ccr_delivery_leases")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        tx.commit()?;
        self.reap_orphan_lease_locks(&live_lease_ids);
        Ok(())
    }

    /// Delete `.ccr-leases/*.lock` files with no lease row.
    ///
    /// `acquire_ccr_delivery_lease` creates the lock file and takes the OS
    /// lock *before* inserting its row, so a process killed in that window
    /// leaves a file `ensure_ccr_delivery_drained` — which enumerates from
    /// database rows — can never list. The same ordering is why only a file
    /// whose exclusive lock this process can take is removed: a live acquirer
    /// still holds it, and an unlockable file is never assumed dead.
    ///
    /// Best effort by design: a failure here is directory hygiene, never
    /// correctness, so it must not fail an otherwise good `open()`.
    fn reap_orphan_lease_locks(&self, live_lease_ids: &std::collections::HashSet<String>) {
        let mut dir = self.path.as_os_str().to_os_string();
        dir.push(".ccr-leases");
        let Ok(entries) = std::fs::read_dir(PathBuf::from(dir)) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("lock") {
                continue;
            }
            let Some(lease_id) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if live_lease_ids.contains(lease_id) {
                continue;
            }
            let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
                continue;
            };
            if file.try_lock_exclusive().is_ok() {
                let _ = std::fs::remove_file(&path);
                let _ = file.unlock();
            }
        }
    }

    /// An artifact version is immutable. Repeating the same version/content
    /// returns its ID; conflicting content for that version is refused.
    pub fn add_artifact(
        &self,
        scope: &EvidenceScope,
        kind: &str,
        external_id: &str,
        version: &str,
        lineage_id: &str,
        content: &str,
        occurred_at: i64,
        retention_at: i64,
    ) -> Result<SourceArtifact, CausalStoreError> {
        self.add_artifact_with_created(
            scope,
            kind,
            external_id,
            version,
            lineage_id,
            content,
            occurred_at,
            retention_at,
        )
        .map(|(artifact, _)| artifact)
    }

    /// Return whether this call inserted the immutable artifact version. A
    /// caller may erase a newly inserted source after its own commit fails,
    /// without erasing an existing version reused by another record.
    pub fn add_artifact_with_created(
        &self,
        scope: &EvidenceScope,
        kind: &str,
        external_id: &str,
        version: &str,
        lineage_id: &str,
        content: &str,
        occurred_at: i64,
        retention_at: i64,
    ) -> Result<(SourceArtifact, bool), CausalStoreError> {
        if !scope.valid()
            || [kind, external_id, version, lineage_id]
                .iter()
                .any(|s| s.trim().is_empty())
            || content.is_empty()
            || content.len() > MAX_ARTIFACT_BYTES
            || retention_at <= now()
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
        let mut conn = self.open()?;
        let tx = conn.transaction()?;
        let existing: Option<(String, String, String, i64, i64)> = tx
            .query_row(
                "SELECT id, content_sha256, lineage_id, occurred_at, retention_at
                 FROM causal_artifacts WHERE tenant_id=?1 AND acl=?2
             AND kind=?3 AND external_id=?4 AND version=?5",
                params![scope.tenant_id, scope.acl, kind, external_id, version],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let (id, ingested_at, created) =
            if let Some((id, prior_digest, prior_lineage, prior_occurred, prior_retention)) =
                existing
            {
                if prior_digest != digest
                    || prior_lineage != lineage_id
                    || prior_occurred != occurred_at
                    || prior_retention != retention_at
                {
                    return Err(CausalStoreError::InvalidInput);
                }
                let ingested_at = tx.query_row(
                    "SELECT ingested_at FROM causal_artifacts WHERE id=?1",
                    [&id],
                    |r| r.get(0),
                )?;
                (id, ingested_at, false)
            } else {
                let id = Uuid::new_v4().to_string();
                let ingested_at = now();
                tx.execute(
                    "INSERT INTO causal_artifacts
                 (id, tenant_id, acl, kind, external_id, version, lineage_id, content_sha256,
                  content, occurred_at, ingested_at, retention_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        id,
                        scope.tenant_id,
                        scope.acl,
                        kind,
                        external_id,
                        version,
                        lineage_id,
                        digest,
                        content,
                        occurred_at,
                        ingested_at,
                        retention_at
                    ],
                )?;
                (id, ingested_at, true)
            };
        tx.commit()?;
        Ok((
            SourceArtifact {
                id,
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                kind: kind.into(),
                external_id: external_id.into(),
                version: version.into(),
                lineage_id: lineage_id.into(),
                content_sha256: digest,
                occurred_at,
                ingested_at,
                retention_at,
            },
            created,
        ))
    }

    pub fn add_claim(
        &self,
        scope: &EvidenceScope,
        cause_variable: &str,
        effect_variable: &str,
        lag_min_seconds: i64,
        lag_max_seconds: i64,
        context: &serde_json::Value,
        modality: ClaimModality,
    ) -> Result<CausalClaim, CausalStoreError> {
        if !scope.valid()
            || cause_variable.trim().is_empty()
            || effect_variable.trim().is_empty()
            || cause_variable == effect_variable
            || lag_min_seconds < 0
            || lag_max_seconds < lag_min_seconds
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let id = Uuid::new_v4().to_string();
        let created_at = now();
        let context_json = context.to_string();
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO causal_claims
             (id,tenant_id,acl,cause_variable,effect_variable,lag_min_seconds,
              lag_max_seconds,context_json,modality,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                id,
                scope.tenant_id,
                scope.acl,
                cause_variable,
                effect_variable,
                lag_min_seconds,
                lag_max_seconds,
                context_json,
                modality.as_str(),
                created_at
            ],
        )?;
        Ok(CausalClaim {
            id,
            scope: scope.clone(),
            cause_variable: cause_variable.into(),
            effect_variable: effect_variable.into(),
            lag_min_seconds,
            lag_max_seconds,
            context_json,
            modality,
            review_state: "candidate".into(),
            reviewer: None,
            created_at,
        })
    }

    /// Byte offsets refer to the immutable UTF-8 artifact. The supplied
    /// excerpt must match exactly; a model cannot fabricate source wording.
    pub fn add_evidence(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
        artifact_id: &str,
        span_start: usize,
        span_end: usize,
        excerpt: &str,
        stance: EvidenceStance,
        speaker_id: Option<&str>,
        extractor_version: &str,
    ) -> Result<EvidenceSpan, CausalStoreError> {
        if !scope.valid()
            || claim_id.is_empty()
            || artifact_id.is_empty()
            || excerpt.is_empty()
            || extractor_version.trim().is_empty()
            || span_start >= span_end
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let claim_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_claims WHERE id=?1 AND tenant_id=?2 AND acl=?3)",
            params![claim_id, scope.tenant_id, scope.acl],
            |r| r.get(0),
        )?;
        if !claim_exists {
            return Err(CausalStoreError::NotFound);
        }
        let (content, digest): (String, String) = tx
            .query_row(
                "SELECT a.content,a.content_sha256 FROM causal_artifacts a
                 WHERE a.id=?1 AND a.tenant_id=?2 AND a.acl=?3
                 AND a.invalidated_at IS NULL AND a.retention_at>?4
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                  WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                   AND r.artifact_id=a.id AND r.version=a.version)
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                  WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                   AND o.artifact_id=a.id AND o.version=a.version
                   AND o.delivered_at IS NULL)",
                params![artifact_id, scope.tenant_id, scope.acl, now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)?;
        if format!("{:x}", Sha256::digest(content.as_bytes())) != digest
            || content.get(span_start..span_end) != Some(excerpt)
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO causal_evidence
             (id,claim_id,artifact_id,span_start,span_end,excerpt,stance,speaker_id,extractor_version)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![id, claim_id, artifact_id, span_start as i64, span_end as i64,
                excerpt, stance.as_str(), speaker_id, extractor_version],
        )?;
        tx.commit()?;
        Ok(EvidenceSpan {
            id,
            claim_id: claim_id.into(),
            artifact_id: artifact_id.into(),
            span_start,
            span_end,
            excerpt: excerpt.into(),
            stance,
            speaker_id: speaker_id.map(str::to_owned),
            extractor_version: extractor_version.into(),
        })
    }

    /// Atomically ingest one query-conditioned extraction as a candidate and
    /// a verified source span. A failed span check leaves neither row behind.
    pub fn ingest_extracted_claim(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
        question: &str,
        extractor_version: &str,
        proposal: &ProposedCausalClaim,
    ) -> Result<(CausalClaim, EvidenceSpan), CausalStoreError> {
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let result = Self::ingest_extracted_claim_tx(
            &tx,
            scope,
            artifact_id,
            question,
            extractor_version,
            proposal,
        )?;
        tx.commit()?;
        Ok(result)
    }

    /// Commit all proposals from one model response together. Later alias or
    /// source failures roll back earlier candidate/evidence writes.
    pub(crate) fn ingest_extracted_claims(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
        question: &str,
        extractor_version: &str,
        proposals: &[ProposedCausalClaim],
    ) -> Result<Vec<(CausalClaim, EvidenceSpan)>, CausalStoreError> {
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut results = Vec::with_capacity(proposals.len());
        for proposal in proposals {
            results.push(Self::ingest_extracted_claim_tx(
                &tx,
                scope,
                artifact_id,
                question,
                extractor_version,
                proposal,
            )?);
        }
        tx.commit()?;
        Ok(results)
    }

    fn ingest_extracted_claim_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: &EvidenceScope,
        artifact_id: &str,
        question: &str,
        extractor_version: &str,
        proposal: &ProposedCausalClaim,
    ) -> Result<(CausalClaim, EvidenceSpan), CausalStoreError> {
        if !scope.valid()
            || question.trim().is_empty()
            || extractor_version.trim().is_empty()
            || !valid_proposal(proposal)
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let span_start: i64 = proposal
            .span_start
            .try_into()
            .map_err(|_| CausalStoreError::InvalidInput)?;
        let span_end: i64 = proposal
            .span_end
            .try_into()
            .map_err(|_| CausalStoreError::InvalidInput)?;
        let source: Option<(String, String, String)> = tx
            .query_row(
                "SELECT a.content,a.lineage_id,a.content_sha256 FROM causal_artifacts a
                 WHERE a.id=?1 AND a.tenant_id=?2 AND a.acl=?3
                 AND a.invalidated_at IS NULL AND a.retention_at>?4
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                  WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                   AND r.artifact_id=a.id AND r.version=a.version)
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                  WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                   AND o.artifact_id=a.id AND o.version=a.version
                   AND o.delivered_at IS NULL)",
                params![artifact_id, scope.tenant_id, scope.acl, now()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (content, lineage_id, digest) = source.ok_or(CausalStoreError::NotFound)?;
        if format!("{:x}", Sha256::digest(content.as_bytes())) != digest
            || content.get(proposal.span_start..proposal.span_end)
                != Some(proposal.excerpt.as_str())
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let (cause_name, cause_alias) = resolve_alias_tx(&tx, scope, &proposal.cause_variable)?;
        let (effect_name, effect_alias) = resolve_alias_tx(&tx, scope, &proposal.effect_variable)?;
        if cause_name == effect_name {
            return Err(CausalStoreError::InvalidInput);
        }
        // The model's free-form context and the question can repeat source
        // wording. Persist only digests for audit correlation; verified spans
        // remain the sole stored source quote and can be scrubbed on erase.
        let proposal_context_bytes =
            serde_json::to_vec(&proposal.context).map_err(|_| CausalStoreError::InvalidInput)?;
        let mut context = serde_json::json!({
            "question_sha256": format!("{:x}", Sha256::digest(question.as_bytes())),
            "source_lineage_id": lineage_id,
            "proposal_context_sha256": format!("{:x}", Sha256::digest(proposal_context_bytes)),
        });
        if cause_alias.is_some() || effect_alias.is_some() {
            context["original_variable_names"] = serde_json::json!({
                "cause": proposal.cause_variable,
                "effect": proposal.effect_variable,
            });
            context["alias_review_ids"] = serde_json::json!({
                "cause": cause_alias.as_ref().map(|(_, id)| id),
                "effect": effect_alias.as_ref().map(|(_, id)| id),
            });
        }
        let context_json = context.to_string();
        let semantic_bytes = if cause_alias.is_some() || effect_alias.is_some() {
            serde_json::to_vec(&(
                "causal-claim-semantic-v2-reviewed-alias",
                &cause_name,
                &effect_name,
                proposal.lag_min_seconds,
                proposal.lag_max_seconds,
                proposal.modality.as_str(),
                &proposal.context,
                &cause_alias,
                &effect_alias,
            ))
        } else {
            serde_json::to_vec(&(
                "causal-claim-semantic-v1",
                &cause_name,
                &effect_name,
                proposal.lag_min_seconds,
                proposal.lag_max_seconds,
                proposal.modality.as_str(),
                &proposal.context,
            ))
        }
        .map_err(|_| CausalStoreError::InvalidInput)?;
        let semantic_sha256 = format!("{:x}", Sha256::digest(semantic_bytes));
        let same_lineage_claim_id: Option<String> = tx
            .query_row(
                "SELECT lc.claim_id FROM causal_lineage_claims lc
                 JOIN causal_claims c ON c.id=lc.claim_id
                 WHERE lc.tenant_id=?1 AND lc.acl=?2 AND lc.lineage_id=?3
                 AND lc.semantic_sha256=?4 AND c.tenant_id=?1 AND c.acl=?2",
                params![scope.tenant_id, scope.acl, lineage_id, semantic_sha256],
                |row| row.get(0),
            )
            .optional()?;
        let copied_source_claim_id: Option<String> = if same_lineage_claim_id.is_none() {
            tx.query_row(
                "SELECT lc.claim_id FROM causal_lineage_claims lc
                 JOIN causal_claims c ON c.id=lc.claim_id
                 JOIN causal_evidence e ON e.claim_id=lc.claim_id
                 JOIN causal_artifacts a ON a.id=e.artifact_id
                 WHERE lc.tenant_id=?1 AND lc.acl=?2 AND lc.semantic_sha256=?3
                 AND c.tenant_id=?1 AND c.acl=?2 AND a.tenant_id=?1 AND a.acl=?2
                 AND a.content_sha256=?4 AND a.invalidated_at IS NULL
                 AND a.retention_at>?5
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                  WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                   AND r.artifact_id=a.id AND r.version=a.version)
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                  WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                   AND o.artifact_id=a.id AND o.version=a.version
                   AND o.delivered_at IS NULL)
                 ORDER BY c.created_at,c.id LIMIT 1",
                params![scope.tenant_id, scope.acl, semantic_sha256, digest, now()],
                |row| row.get(0),
            )
            .optional()?
        } else {
            None
        };
        let is_new_lineage_mapping = same_lineage_claim_id.is_none();
        let is_new_claim = is_new_lineage_mapping && copied_source_claim_id.is_none();
        let claim_id = same_lineage_claim_id
            .or(copied_source_claim_id)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let evidence_id = Uuid::new_v4().to_string();
        let created_at = now();
        if is_new_claim {
            tx.execute(
                "INSERT INTO causal_claims
             (id,tenant_id,acl,cause_variable,effect_variable,lag_min_seconds,
              lag_max_seconds,context_json,modality,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    claim_id,
                    scope.tenant_id,
                    scope.acl,
                    cause_name,
                    effect_name,
                    proposal.lag_min_seconds,
                    proposal.lag_max_seconds,
                    context_json,
                    proposal.modality.as_str(),
                    created_at
                ],
            )?;
        }
        if is_new_lineage_mapping {
            tx.execute(
                "INSERT INTO causal_lineage_claims
                 (tenant_id,acl,lineage_id,semantic_sha256,claim_id)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    scope.tenant_id,
                    scope.acl,
                    lineage_id,
                    semantic_sha256,
                    claim_id
                ],
            )?;
        }
        for (alias, review_id) in [cause_alias.as_ref(), effect_alias.as_ref()]
            .into_iter()
            .flatten()
        {
            tx.execute(
                "INSERT OR IGNORE INTO causal_claim_alias_dependencies
                 (claim_id,alias,review_id) VALUES (?1,?2,?3)",
                params![claim_id, alias, review_id],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO causal_evidence
             (id,claim_id,artifact_id,span_start,span_end,excerpt,stance,speaker_id,extractor_version)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![evidence_id, claim_id, artifact_id, span_start, span_end,
                proposal.excerpt, proposal.stance.as_str(), proposal.speaker_id, extractor_version],
        )?;
        let (evidence_id, excerpt, speaker_id, extractor_version): (
            String,
            String,
            Option<String>,
            String,
        ) = tx.query_row(
            "SELECT id,excerpt,speaker_id,extractor_version FROM causal_evidence
             WHERE claim_id=?1 AND artifact_id=?2 AND span_start=?3 AND span_end=?4 AND stance=?5",
            params![
                claim_id,
                artifact_id,
                span_start,
                span_end,
                proposal.stance.as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let (context_json, review_state, reviewer, created_at): (
            String,
            String,
            Option<String>,
            i64,
        ) = tx.query_row(
            "SELECT context_json,review_state,reviewer,created_at FROM causal_claims
             WHERE id=?1 AND tenant_id=?2 AND acl=?3",
            params![claim_id, scope.tenant_id, scope.acl],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Ok((
            CausalClaim {
                id: claim_id.clone(),
                scope: scope.clone(),
                cause_variable: cause_name,
                effect_variable: effect_name,
                lag_min_seconds: proposal.lag_min_seconds,
                lag_max_seconds: proposal.lag_max_seconds,
                context_json,
                modality: proposal.modality,
                review_state,
                reviewer,
                created_at,
            },
            EvidenceSpan {
                id: evidence_id,
                claim_id,
                artifact_id: artifact_id.into(),
                span_start: proposal.span_start,
                span_end: proposal.span_end,
                excerpt,
                stance: proposal.stance,
                speaker_id,
                extractor_version,
            },
        ))
    }

    /// A human review changes status; the store does not infer acceptance from
    /// a model-generated confidence score.
    pub fn review_claim(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
        reviewer: &str,
        accept: bool,
    ) -> Result<(), CausalStoreError> {
        self.review_claim_with_expected(scope, claim_id, reviewer, accept, None)
    }

    /// Compare-and-set review for a human curation surface. An immediate
    /// transaction prevents a stale reviewer from overwriting a newer state.
    pub fn review_claim_if_state(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
        reviewer: &str,
        accept: bool,
        expected_state: &str,
    ) -> Result<(), CausalStoreError> {
        if !matches!(
            expected_state,
            "candidate" | "accepted" | "rejected" | "needs_review"
        ) {
            return Err(CausalStoreError::InvalidInput);
        }
        self.review_claim_with_expected(scope, claim_id, reviewer, accept, Some(expected_state))
    }

    fn review_claim_with_expected(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
        reviewer: &str,
        accept: bool,
        expected_state: Option<&str>,
    ) -> Result<(), CausalStoreError> {
        if !scope.valid() || reviewer.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current_state: Option<String> = tx
            .query_row(
                "SELECT review_state FROM causal_claims WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![claim_id, scope.tenant_id, scope.acl],
                |row| row.get(0),
            )
            .optional()?;
        let current_state = current_state.ok_or(CausalStoreError::NotFound)?;
        if current_state == "superseded" {
            return Err(CausalStoreError::Conflict);
        }
        if expected_state.is_some_and(|expected| expected != current_state) {
            return Err(CausalStoreError::Conflict);
        }
        if accept {
            let stale_alias: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM causal_claim_alias_dependencies d
                 LEFT JOIN causal_variable_aliases a ON a.tenant_id=?2 AND a.acl=?3
                    AND a.alias=d.alias
                 WHERE d.claim_id=?1 AND (a.review_id IS NULL OR a.review_id!=d.review_id))",
                params![claim_id, scope.tenant_id, scope.acl],
                |row| row.get(0),
            )?;
            if stale_alias {
                return Err(CausalStoreError::Conflict);
            }
            let support: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM causal_evidence e
                 JOIN causal_artifacts a ON a.id=e.artifact_id
                 WHERE e.claim_id=?1 AND e.stance='supports' AND a.tenant_id=?2 AND a.acl=?3
                 AND a.invalidated_at IS NULL AND a.retention_at>?4
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                  WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                   AND r.artifact_id=a.id AND r.version=a.version)
                 AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                  WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                   AND o.artifact_id=a.id AND o.version=a.version
                   AND o.delivered_at IS NULL))",
                params![claim_id, scope.tenant_id, scope.acl, now()],
                |r| r.get(0),
            )?;
            if !support {
                return Err(CausalStoreError::MissingSupport);
            }
        }
        let reviewed_at = now();
        let count = tx.execute(
            "UPDATE causal_claims SET review_state=?1, reviewer=?2, reviewed_at=?3
             WHERE id=?4 AND tenant_id=?5 AND acl=?6",
            params![
                if accept { "accepted" } else { "rejected" },
                reviewer,
                reviewed_at,
                claim_id,
                scope.tenant_id,
                scope.acl
            ],
        )?;
        if count == 0 {
            return Err(CausalStoreError::NotFound);
        }
        tx.execute(
            "INSERT INTO causal_claim_reviews (id,claim_id,reviewer,decision,reviewed_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                Uuid::new_v4().to_string(),
                claim_id,
                reviewer,
                if accept { "accepted" } else { "rejected" },
                reviewed_at
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Revoking a source never erases its audit record. Every accepted claim
    /// citing it is demoted until the remaining evidence can be reviewed.
    pub fn invalidate_artifact(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<usize, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let version = self.begin_ccr_revocation(scope, artifact_id)?;
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let reclaimed = self.ensure_ccr_delivery_drained(&tx, scope, artifact_id, &version)?;
        let changed = tx.execute(
            "UPDATE causal_artifacts SET invalidated_at=?1 WHERE id=?2 AND tenant_id=?3
             AND acl=?4 AND invalidated_at IS NULL",
            params![now(), artifact_id, scope.tenant_id, scope.acl],
        )?;
        if changed == 0 {
            return Err(CausalStoreError::NotFound);
        }
        tx.execute(
            "UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
             diagnostics_json='{}',identification_state='source_invalidated'
             WHERE data_snapshot_id=?1 OR model_id IN (
                SELECT me.model_id FROM causal_model_edges me JOIN causal_evidence e
                ON e.claim_id=me.claim_id WHERE e.artifact_id=?1)
             OR EXISTS (
                SELECT 1 FROM causal_negative_control_reviews r
                WHERE r.protocol_artifact_id=?1
                  AND r.model_id=causal_effect_estimates.model_id
                  AND r.variable_id=json_extract(causal_effect_estimates.diagnostics_json,
                    '$.negative_control.variable_id')
                  AND r.id=json_extract(causal_effect_estimates.diagnostics_json,
                    '$.negative_control.review_id'))",
            [artifact_id],
        )?;
        let demoted = tx.execute(
            "UPDATE causal_claims SET review_state='needs_review', reviewer=NULL, reviewed_at=NULL
             WHERE tenant_id=?1 AND acl=?2 AND review_state='accepted'
             AND id IN (SELECT claim_id FROM causal_evidence WHERE artifact_id=?3)",
            params![scope.tenant_id, scope.acl, artifact_id],
        )?;
        tx.commit()?;
        for path in reclaimed {
            let _ = std::fs::remove_file(path);
        }
        Ok(demoted)
    }

    /// A deletion request removes retained source wording and excerpts while
    /// keeping opaque IDs and digests for audit. Dependent accepted claims
    /// return to review in the same transaction.
    pub fn erase_artifact(
        &self,
        scope: &EvidenceScope,
        artifact_id: &str,
    ) -> Result<usize, CausalStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let version = self.begin_ccr_revocation(scope, artifact_id)?;
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let reclaimed = self.ensure_ccr_delivery_drained(&tx, scope, artifact_id, &version)?;
        let changed = tx.execute(
            "UPDATE causal_artifacts SET content='', invalidated_at=?1
             WHERE id=?2 AND tenant_id=?3 AND acl=?4 AND content<>''",
            params![now(), artifact_id, scope.tenant_id, scope.acl],
        )?;
        if changed == 0 {
            return Err(CausalStoreError::NotFound);
        }
        tx.execute(
            "UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
             diagnostics_json='{}',identification_state='source_erased'
             WHERE data_snapshot_id=?1 OR model_id IN (
                SELECT me.model_id FROM causal_model_edges me JOIN causal_evidence e
                ON e.claim_id=me.claim_id WHERE e.artifact_id=?1)
             OR EXISTS (
                SELECT 1 FROM causal_negative_control_reviews r
                WHERE r.protocol_artifact_id=?1
                  AND r.model_id=causal_effect_estimates.model_id
                  AND r.variable_id=json_extract(causal_effect_estimates.diagnostics_json,
                    '$.negative_control.variable_id')
                  AND r.id=json_extract(causal_effect_estimates.diagnostics_json,
                    '$.negative_control.review_id'))",
            [artifact_id],
        )?;
        tx.execute(
            "UPDATE causal_evidence SET excerpt='' WHERE artifact_id=?1",
            [artifact_id],
        )?;
        // Extracted/revised claim context may repeat source wording even
        // after evidence excerpts are removed. Scrub every directly linked
        // claim in the same transaction as the source erasure.
        scrub_copied_source_wording(&tx, artifact_id)?;
        let demoted = tx.execute(
            "UPDATE causal_claims SET review_state='needs_review', reviewer=NULL, reviewed_at=NULL
             WHERE tenant_id=?1 AND acl=?2 AND review_state='accepted'
             AND id IN (SELECT claim_id FROM causal_evidence WHERE artifact_id=?3)",
            params![scope.tenant_id, scope.acl, artifact_id],
        )?;
        tx.commit()?;
        for path in reclaimed {
            let _ = std::fs::remove_file(path);
        }
        Ok(demoted)
    }

    /// Bounded curation index. IDs are returned only within the exact scope;
    /// callers can then request the claim and its source-backed spans.
    pub fn list_claim_ids(
        &self,
        scope: &EvidenceScope,
        review_state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>, CausalStoreError> {
        if !scope.valid()
            || !matches!(
                review_state,
                None | Some("candidate" | "accepted" | "rejected" | "needs_review" | "superseded")
            )
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM causal_claims WHERE tenant_id=?1 AND acl=?2
             AND (?3 IS NULL OR review_state=?3)
             ORDER BY created_at DESC, rowid DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                scope.tenant_id,
                scope.acl,
                review_state,
                limit.clamp(1, 100) as i64
            ],
            |row| row.get(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn read_claim(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
    ) -> Result<CausalClaim, CausalStoreError> {
        let conn = self.open()?;
        Self::read_claim_with_conn(&conn, scope, claim_id)
    }

    /// `read_claim` on a connection the caller already opened. A curation
    /// listing reads up to 100 claims per request; one `open()` each made
    /// that request re-run schema setup and the live-source resync 100 times.
    pub fn read_claim_with_conn(
        conn: &Connection,
        scope: &EvidenceScope,
        claim_id: &str,
    ) -> Result<CausalClaim, CausalStoreError> {
        if !scope.valid() || claim_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let row: Option<(
            String,
            String,
            i64,
            i64,
            String,
            String,
            String,
            Option<String>,
            i64,
        )> = conn
            .query_row(
                "SELECT c.cause_variable,c.effect_variable,c.lag_min_seconds,c.lag_max_seconds,
             CASE WHEN EXISTS (
              SELECT 1 FROM causal_evidence e LEFT JOIN causal_artifacts a ON a.id=e.artifact_id
              WHERE e.claim_id=c.id AND (
               a.id IS NULL OR a.invalidated_at IS NOT NULL OR a.retention_at<=?4
               OR EXISTS (SELECT 1 FROM causal_ccr_revoking r
                WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                 AND r.artifact_id=a.id AND r.version=a.version)
               OR EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                 AND o.artifact_id=a.id AND o.version=a.version
                 AND o.delivered_at IS NULL)))
             THEN '{}' ELSE c.context_json END,
             c.modality,c.review_state,c.reviewer,c.created_at FROM causal_claims c
             WHERE c.id=?1 AND c.tenant_id=?2 AND c.acl=?3",
                params![claim_id, scope.tenant_id, scope.acl, now()],
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
                    ))
                },
            )
            .optional()?;
        let (
            cause_variable,
            effect_variable,
            lag_min_seconds,
            lag_max_seconds,
            context_json,
            modality,
            review_state,
            reviewer,
            created_at,
        ) = row.ok_or(CausalStoreError::NotFound)?;
        let modality = match modality.as_str() {
            "asserted" => ClaimModality::Asserted,
            "speculated" => ClaimModality::Speculated,
            "negated" => ClaimModality::Negated,
            "questioned" => ClaimModality::Questioned,
            _ => return Err(CausalStoreError::InvalidInput),
        };
        Ok(CausalClaim {
            id: claim_id.into(),
            scope: scope.clone(),
            cause_variable,
            effect_variable,
            lag_min_seconds,
            lag_max_seconds,
            context_json,
            modality,
            review_state,
            reviewer,
            created_at,
        })
    }

    /// Evidence from invalidated or expired sources is listed without its
    /// excerpt. This keeps provenance visible without resurfacing revoked text.
    pub fn evidence_for_claim(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
    ) -> Result<Vec<EvidenceDetail>, CausalStoreError> {
        let conn = self.open()?;
        Self::read_claim_with_conn(&conn, scope, claim_id)?;
        let mut stmt = conn.prepare(
            "SELECT e.id,e.artifact_id,e.span_start,e.span_end,e.excerpt,e.stance,
             e.speaker_id,e.extractor_version,a.lineage_id,
             (a.invalidated_at IS NULL AND a.retention_at>?2
              AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
               WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                AND r.artifact_id=a.id AND r.version=a.version)
              AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
               WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                AND o.artifact_id=a.id AND o.version=a.version
                AND o.delivered_at IS NULL))
             FROM causal_evidence e JOIN causal_artifacts a ON a.id=e.artifact_id
             WHERE e.claim_id=?1 AND a.tenant_id=?3 AND a.acl=?4 ORDER BY e.rowid",
        )?;
        let rows = stmt.query_map(
            params![claim_id, now(), scope.tenant_id, scope.acl],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, bool>(9)?,
                ))
            },
        )?;
        let mut result = Vec::new();
        for row in rows {
            let (
                id,
                artifact_id,
                start,
                end,
                excerpt,
                stance,
                speaker_id,
                extractor_version,
                source_lineage_id,
                source_active,
            ) = row?;
            let stance = match stance.as_str() {
                "supports" => EvidenceStance::Supports,
                "opposes" => EvidenceStance::Opposes,
                _ => return Err(CausalStoreError::InvalidInput),
            };
            result.push(EvidenceDetail {
                span: EvidenceSpan {
                    id,
                    claim_id: claim_id.into(),
                    artifact_id,
                    span_start: start
                        .try_into()
                        .map_err(|_| CausalStoreError::InvalidInput)?,
                    span_end: end.try_into().map_err(|_| CausalStoreError::InvalidInput)?,
                    excerpt: if source_active {
                        excerpt
                    } else {
                        String::new()
                    },
                    stance,
                    speaker_id,
                    extractor_version,
                },
                source_lineage_id,
                source_active,
            });
        }
        Ok(result)
    }

    /// Count active support and opposition by conservative source family.
    /// Versions in one lineage and byte-identical content uploaded under
    /// different lineage IDs form one connected family. Spans remain visible.
    pub fn claim_lineage_summary(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
    ) -> Result<ClaimLineageSummary, CausalStoreError> {
        self.read_claim(scope, claim_id)?;
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT a.lineage_id,a.content_sha256,e.stance,
             (a.invalidated_at IS NULL AND a.retention_at>?4
              AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
               WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                AND r.artifact_id=a.id AND r.version=a.version)
              AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
               WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                AND o.artifact_id=a.id AND o.version=a.version
                AND o.delivered_at IS NULL)),COUNT(*)
             FROM causal_evidence e
             JOIN causal_artifacts a ON a.id=e.artifact_id
             WHERE e.claim_id=?1 AND a.tenant_id=?2 AND a.acl=?3
             GROUP BY 1,2,3,4",
        )?;
        let rows = stmt.query_map(
            params![claim_id, scope.tenant_id, scope.acl, now()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, u64>(4)?,
                ))
            },
        )?;
        fn root(parents: &mut [usize], mut index: usize) -> usize {
            while parents[index] != index {
                parents[index] = parents[parents[index]];
                index = parents[index];
            }
            index
        }
        let mut lineage_ids: HashMap<String, usize> = HashMap::new();
        let mut digest_owner: HashMap<String, usize> = HashMap::new();
        let mut parents: Vec<usize> = Vec::new();
        let mut stance_rows = Vec::new();
        for row in rows {
            let (lineage, digest, stance, active, count) = row?;
            let lineage_index = *lineage_ids.entry(lineage).or_insert_with(|| {
                let index = parents.len();
                parents.push(index);
                index
            });
            if let Some(&other_index) = digest_owner.get(&digest) {
                let lineage_root = root(&mut parents, lineage_index);
                let other_root = root(&mut parents, other_index);
                parents[other_root] = lineage_root;
            } else {
                digest_owner.insert(digest, lineage_index);
            }
            match stance.as_str() {
                "supports" | "opposes" if active => {
                    stance_rows.push((lineage_index, stance, count))
                }
                "supports" | "opposes" => {}
                _ => return Err(CausalStoreError::InvalidInput),
            }
        }
        let mut families: HashMap<usize, (u64, u64)> = HashMap::new();
        for (lineage_index, stance, count) in stance_rows {
            let family = families
                .entry(root(&mut parents, lineage_index))
                .or_default();
            match stance.as_str() {
                "supports" => family.0 += count,
                "opposes" => family.1 += count,
                _ => unreachable!(),
            }
        }
        Ok(ClaimLineageSummary {
            supporting_spans: families.values().map(|(support, _)| *support).sum(),
            opposing_spans: families.values().map(|(_, oppose)| *oppose).sum(),
            independent_supporting_lineages: families
                .values()
                .filter(|(support, _)| *support > 0)
                .count(),
            independent_opposing_lineages: families
                .values()
                .filter(|(_, oppose)| *oppose > 0)
                .count(),
            mixed_stance_lineages: families
                .values()
                .filter(|(support, oppose)| *support > 0 && *oppose > 0)
                .count(),
        })
    }

    pub fn claim_state(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() {
            return Err(CausalStoreError::InvalidInput);
        }
        self.open()?
            .query_row(
                "SELECT CASE WHEN c.review_state='accepted' AND NOT EXISTS (
                    SELECT 1 FROM causal_evidence e JOIN causal_artifacts a ON a.id=e.artifact_id
                    WHERE e.claim_id=c.id AND e.stance='supports' AND a.invalidated_at IS NULL
                    AND a.retention_at>?4 AND a.tenant_id=c.tenant_id AND a.acl=c.acl
                    AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                     WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                      AND r.artifact_id=a.id AND r.version=a.version)
                    AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                     WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                      AND o.artifact_id=a.id AND o.version=a.version
                      AND o.delivered_at IS NULL)
                 ) THEN 'needs_review' ELSE c.review_state END
                 FROM causal_claims c WHERE c.id=?1 AND c.tenant_id=?2 AND c.acl=?3",
                params![claim_id, scope.tenant_id, scope.acl, now()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ccr_acl_revision(scope: &EvidenceScope) -> String {
        format!(
            "immutable-acl-sha256:{:x}",
            Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.acl))
        )
    }

    #[test]
    fn staged_revocation_hides_source_and_prevents_new_or_active_support() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let source = store
            .add_artifact(
                &tenant,
                "ticket",
                "stage",
                "v1",
                "lineage",
                "staffing lowered backlog",
                1,
                now() + 3600,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &tenant,
                "staffing",
                "backlog",
                0,
                86_400,
                &serde_json::json!({"source_quote":"staffing lowered backlog"}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &tenant,
                &claim.id,
                &source.id,
                0,
                8,
                "staffing",
                EvidenceStance::Supports,
                None,
                "v1",
            )
            .unwrap();
        store
            .review_claim(&tenant, &claim.id, "reviewer", true)
            .unwrap();
        let (guarded_text, lease) = store
            .source_text_with_delivery_lease(&tenant, &source.id)
            .unwrap();
        assert_eq!(guarded_text, "staffing lowered backlog");
        assert!(lease.still_valid());
        assert_eq!(
            store.begin_ccr_revocation(&tenant, &source.id).unwrap(),
            "v1"
        );
        assert!(!lease.still_valid());
        assert!(matches!(
            store.source_text_with_delivery_lease(&tenant, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.invalidate_artifact(&tenant, &source.id),
            Err(CausalStoreError::Conflict)
        ));
        assert!(matches!(
            store.source_text(&tenant, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store.read_claim(&tenant, &claim.id).unwrap().context_json,
            "{}"
        );
        let evidence = store.evidence_for_claim(&tenant, &claim.id).unwrap();
        assert_eq!(evidence.len(), 1);
        assert!(!evidence[0].source_active);
        assert!(evidence[0].span.excerpt.is_empty());
        assert_eq!(
            store
                .claim_lineage_summary(&tenant, &claim.id)
                .unwrap()
                .supporting_spans,
            0
        );
        assert_eq!(
            store.claim_state(&tenant, &claim.id).unwrap(),
            "needs_review"
        );
        assert!(matches!(
            store.review_claim(&tenant, &claim.id, "reviewer", true),
            Err(CausalStoreError::MissingSupport)
        ));
        assert!(matches!(
            store.add_evidence(
                &tenant,
                &claim.id,
                &source.id,
                0,
                8,
                "staffing",
                EvidenceStance::Supports,
                None,
                "v1"
            ),
            Err(CausalStoreError::NotFound)
        ));
        let proposal = ProposedCausalClaim {
            cause_variable: "staffing".into(),
            effect_variable: "backlog".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86_400,
            modality: ClaimModality::Asserted,
            stance: EvidenceStance::Supports,
            span_start: 0,
            span_end: 8,
            excerpt: "staffing".into(),
            speaker_id: None,
            context: serde_json::json!({}),
        };
        assert!(matches!(
            store.ingest_extracted_claim(&tenant, &source.id, "Why?", "v1", &proposal),
            Err(CausalStoreError::NotFound)
        ));
        drop(lease);
        store.erase_artifact(&tenant, &source.id).unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let stored_context: String = conn
            .query_row(
                "SELECT context_json FROM causal_claims WHERE id=?1",
                [&claim.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_context, "{}");
    }

    #[test]
    fn ccr_delivery_lease_fences_revoke_and_direct_sql_until_release() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let a = scope("a");
        let b = scope("b");
        let source_a = store
            .add_artifact(&a, "ticket", "a", "v1", "a", "source a", 1, now() + 3600)
            .unwrap();
        let source_b = store
            .add_artifact(&b, "ticket", "b", "v1", "b", "source b", 1, now() + 3600)
            .unwrap();
        assert!(
            store
                .acquire_ccr_delivery_lease(
                    &b,
                    &source_a.id,
                    &source_a.version,
                    &source_a.content_sha256,
                    &ccr_acl_revision(&b),
                )
                .is_err()
        );
        let lease = store
            .acquire_ccr_delivery_lease(
                &a,
                &source_a.id,
                &source_a.version,
                &source_a.content_sha256,
                &ccr_acl_revision(&a),
            )
            .unwrap();
        assert!(lease.still_valid());
        assert!(matches!(
            store.invalidate_artifact(&a, &source_a.id),
            Err(CausalStoreError::Conflict)
        ));
        assert!(!lease.still_valid());
        assert!(
            store
                .acquire_ccr_delivery_lease(
                    &a,
                    &source_a.id,
                    &source_a.version,
                    &source_a.content_sha256,
                    &ccr_acl_revision(&a),
                )
                .is_err()
        );
        let direct = Connection::open(store.path()).unwrap();
        assert!(
            direct
                .execute(
                    "UPDATE causal_artifacts SET content='changed' WHERE id=?1",
                    [&source_a.id],
                )
                .is_err()
        );
        assert!(
            direct
                .execute("DELETE FROM causal_artifacts WHERE id=?1", [&source_a.id])
                .is_err()
        );
        assert!(store.invalidate_artifact(&b, &source_b.id).is_ok());
        drop(lease);
        store.invalidate_artifact(&a, &source_a.id).unwrap();
        assert!(matches!(
            store.source_text(&a, &source_a.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(
            store
                .pending_ccr_revocations(10)
                .unwrap()
                .iter()
                .any(|row| row.tenant_id == "a" && row.artifact_id == source_a.id)
        );
    }

    #[test]
    fn ccr_direct_sql_aba_stays_revoked_while_outbox_notice_is_pending() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let source = store
            .add_artifact(
                &tenant,
                "ticket",
                "aba",
                "v1",
                "lineage",
                "original",
                1,
                now() + 3600,
            )
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let changed_digest = format!("{:x}", Sha256::digest(b"temporary"));
        conn.execute(
            "UPDATE causal_artifacts SET content='temporary',content_sha256=?1 WHERE id=?2",
            params![changed_digest, source.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET content='original',content_sha256=?1 WHERE id=?2",
            params![source.content_sha256, source.id],
        )
        .unwrap();
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM causal_ccr_revocation_outbox
             WHERE tenant_id=?1 AND connector='causal' AND artifact_id=?2
               AND version='v1' AND delivered_at IS NULL",
                params![tenant.tenant_id, source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
        assert!(matches!(
            store.read_artifact_metadata(&tenant, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.source_text(&tenant, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(
            store
                .acquire_ccr_delivery_lease(
                    &tenant,
                    &source.id,
                    &source.version,
                    &source.content_sha256,
                    &ccr_acl_revision(&tenant),
                )
                .is_err()
        );
    }

    #[test]
    fn ccr_delivery_reclaims_only_os_unlocked_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let source = store
            .add_artifact(&tenant, "ticket", "a", "v1", "a", "source", 1, now() + 3600)
            .unwrap();
        let orphan_id = Uuid::new_v4().to_string();
        let path = store.ccr_lease_lock_path(&orphan_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "INSERT INTO causal_ccr_delivery_leases
             (lease_id,tenant_id,acl,artifact_id,version,created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                orphan_id,
                tenant.tenant_id,
                tenant.acl,
                source.id,
                source.version,
                now()
            ],
        )
        .unwrap();
        store.invalidate_artifact(&tenant, &source.id).unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM causal_ccr_delivery_leases WHERE lease_id=?1",
                [&orphan_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
        assert!(!path.exists());

        let other = store
            .add_artifact(
                &tenant,
                "ticket",
                "other",
                "v1",
                "other",
                "more",
                1,
                now() + 3600,
            )
            .unwrap();
        let unknown_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO causal_ccr_delivery_leases
             (lease_id,tenant_id,acl,artifact_id,version,created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                unknown_id,
                tenant.tenant_id,
                tenant.acl,
                other.id,
                other.version,
                0
            ],
        )
        .unwrap();
        assert!(matches!(
            store.invalidate_artifact(&tenant, &other.id),
            Err(CausalStoreError::Conflict)
        ));
    }

    #[test]
    fn ccr_delivery_lease_prevents_expiry_scrub_until_guard_drops() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let source = store
            .add_artifact(
                &tenant,
                "ticket",
                "soon",
                "v1",
                "soon",
                "source",
                1,
                // Whole-second clock: keep a wide margin so a slow first open
                // (loaded CI runner) cannot push the lease past retention.
                now() + 5,
            )
            .unwrap();
        let lease = store
            .acquire_ccr_delivery_lease(
                &tenant,
                &source.id,
                &source.version,
                &source.content_sha256,
                &ccr_acl_revision(&tenant),
            )
            .unwrap();
        // Poll until retention passes instead of sleeping a fixed time.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while lease.still_valid() {
            assert!(
                std::time::Instant::now() < deadline,
                "lease still valid 10s after retention was set to now()+5"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!lease.still_valid());
        // Production opens a fresh store per request, so each request runs the
        // retention sweep; one instance reused across the expiry has to drop
        // its maintenance throttle to stand in for that.
        store.reset_maintenance_throttle();
        store.open().unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let retained: String = conn
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, "source");
        drop(lease);
        store.reset_maintenance_throttle();
        store.open().unwrap();
        let scrubbed: String = conn
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(scrubbed.is_empty());
    }

    #[test]
    fn artifact_metadata_is_scoped_and_revocation_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let scope = EvidenceScope {
            tenant_id: "a".into(),
            acl: "private".into(),
        };
        let other = EvidenceScope {
            tenant_id: "b".into(),
            acl: "private".into(),
        };
        let source = store
            .add_artifact(
                &scope,
                "causal_dataset",
                "synthetic",
                "v1",
                "synthetic-lineage",
                "synthetic rows",
                10,
                i64::MAX,
            )
            .unwrap();
        assert_eq!(
            store.read_artifact_metadata(&scope, &source.id).unwrap(),
            source
        );
        assert!(matches!(
            store.read_artifact_metadata(&other, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store.begin_ccr_revocation(&scope, &source.id).unwrap(),
            "v1"
        );
        assert!(matches!(
            store.read_artifact_metadata(&scope, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        store.invalidate_artifact(&scope, &source.id).unwrap();
        assert!(matches!(
            store.read_artifact_metadata(&scope, &source.id),
            Err(CausalStoreError::NotFound)
        ));
    }

    fn scope(tenant: &str) -> EvidenceScope {
        EvidenceScope {
            tenant_id: tenant.into(),
            acl: "private".into(),
        }
    }

    /// W3-2 regression: `ensure_ccr_delivery_drained` enumerates lock files
    /// from database rows, so a process killed between `create_new`-ing the
    /// lock file and committing its lease row left a `.ccr-leases/*.lock`
    /// orphan nothing could ever list — the directory grew by one file per
    /// hard crash forever.
    #[test]
    fn open_reaps_orphan_lease_lock_files_but_keeps_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let source = store
            .add_artifact(
                &tenant,
                "ticket",
                "orphan",
                "v1",
                "lineage",
                "staffing lowered backlog",
                1,
                now() + 3600,
            )
            .unwrap();
        let lease = store
            .acquire_ccr_delivery_lease(
                &tenant,
                &source.id,
                &source.version,
                &source.content_sha256,
                &ccr_acl_revision(&tenant),
            )
            .unwrap();
        let live_lock = store.ccr_lease_lock_path(&lease.lease_id);
        let orphan = live_lock
            .parent()
            .expect("lease lock directory")
            .join("00000000-0000-0000-0000-000000000000.lock");
        std::fs::write(&orphan, b"").unwrap();

        // A fresh instance so the maintenance throttle does not skip the pass.
        CausalStore::new(dir.path().join("causal.db")).open().unwrap();

        assert!(
            !orphan.exists(),
            "a lock file with no lease row must be reaped on open"
        );
        assert!(
            live_lock.exists(),
            "a lock file still held by a live lease must survive"
        );
        drop(lease);
    }

    /// W3-2 regression: every `open()` — one per claim on a 100-claim listing
    /// — re-ran the whole `CREATE TABLE/INDEX/TRIGGER` batch. A file already
    /// stamped with the current `SCHEMA_VERSION` must skip it, which is also
    /// why the constant has to be bumped whenever the batch changes.
    #[test]
    fn open_skips_the_ddl_batch_once_the_schema_version_is_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("causal.db");
        CausalStore::new(&path).open().unwrap();
        let conn = Connection::open(&path).unwrap();
        let stamped: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stamped, SCHEMA_VERSION);

        let table_exists = |conn: &Connection| -> bool {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type='table' AND name='causal_alias_reviews')",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        conn.execute_batch("DROP TABLE causal_alias_reviews;").unwrap();
        CausalStore::new(&path).open().unwrap();
        assert!(
            !table_exists(&conn),
            "a file stamped with the current schema version must skip the DDL batch"
        );

        conn.pragma_update(None, "user_version", 0).unwrap();
        CausalStore::new(&path).open().unwrap();
        assert!(
            table_exists(&conn),
            "clearing the stamp must bring the DDL batch back"
        );

        // `memory.db` is shared with the memory engine, so the stamp alone is
        // not proof: a co-writer setting the same number must not make the
        // causal tables disappear silently.
        conn.execute_batch("DROP TABLE causal_artifacts;").unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
        CausalStore::new(&path).open().unwrap();
        let artifacts_exist: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type='table' AND name='causal_artifacts')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            artifacts_exist,
            "a foreign user_version must not skip creating the causal tables"
        );
    }

    /// W3-2 regression: `begin_ccr_revocation` had no DELETE anywhere in the
    /// repo, so a revoke that stopped at its fence hid the source forever.
    /// Clearing is deliberately narrow — only a revoke that demonstrably
    /// never took effect.
    #[test]
    fn clear_revocation_fence_restores_only_an_abandoned_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let source = store
            .add_artifact(
                &tenant,
                "ticket",
                "fence",
                "v1",
                "lineage",
                "staffing lowered backlog",
                1,
                now() + 3600,
            )
            .unwrap();
        assert!(matches!(
            store.clear_revocation_fence(&tenant, &source.id),
            Err(CausalStoreError::NotFound)
        ));

        // An abandoned revoke: the fence is staged, then the revoke itself
        // fails because the copied bytes are still being delivered.
        let lease = store
            .acquire_ccr_delivery_lease(
                &tenant,
                &source.id,
                &source.version,
                &source.content_sha256,
                &ccr_acl_revision(&tenant),
            )
            .unwrap();
        store.begin_ccr_revocation(&tenant, &source.id).unwrap();
        assert!(matches!(
            store.source_text(&tenant, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.invalidate_artifact(&tenant, &source.id),
            Err(CausalStoreError::Conflict)
        ));
        assert!(
            matches!(
                store.clear_revocation_fence(&tenant, &source.id),
                Err(CausalStoreError::Conflict)
            ),
            "a live delivery lease must block the fence clear too"
        );
        drop(lease);

        // A tombstone notice already queued for CCR means the revocation was
        // acted on downstream; restoring visibility would contradict it.
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "INSERT INTO causal_ccr_revocation_outbox
             (tenant_id,connector,artifact_id,version,queued_at)
             VALUES (?1,'causal',?2,?3,1)",
            params![tenant.tenant_id, source.id, source.version],
        )
        .unwrap();
        assert!(matches!(
            store.clear_revocation_fence(&tenant, &source.id),
            Err(CausalStoreError::Conflict)
        ));
        conn.execute("DELETE FROM causal_ccr_revocation_outbox", [])
            .unwrap();

        assert_eq!(
            store.clear_revocation_fence(&tenant, &source.id).unwrap(),
            source.version
        );
        assert_eq!(
            store.source_text(&tenant, &source.id).unwrap(),
            "staffing lowered backlog"
        );

        // A revocation that did take effect stays irreversible.
        store.invalidate_artifact(&tenant, &source.id).unwrap();
        assert!(matches!(
            store.clear_revocation_fence(&tenant, &source.id),
            Err(CausalStoreError::Conflict)
        ));
    }

    /// W3-2 regression: `i64::MAX` ("no retention deadline") serialised as
    /// `9223372036854775807`, which a browser reads back as
    /// `9223372036854776000` — a bogus deadline the first caller to compare
    /// the field would trust.
    #[test]
    fn a_source_without_a_retention_deadline_serialises_as_null() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let open_ended = store
            .add_artifact(
                &tenant, "ticket", "forever", "v1", "lineage", "text", 1, i64::MAX,
            )
            .unwrap();
        let bounded = store
            .add_artifact(
                &tenant,
                "ticket",
                "bounded",
                "v1",
                "lineage-2",
                "text",
                1,
                now() + 3600,
            )
            .unwrap();
        let open_ended = serde_json::to_value(&open_ended).unwrap();
        assert!(
            open_ended["retention_at"].is_null(),
            "no deadline must serialise as null, got {}",
            open_ended["retention_at"]
        );
        assert_eq!(
            serde_json::to_value(&bounded).unwrap()["retention_at"].as_i64(),
            Some(bounded.retention_at)
        );
    }

    /// F2 regression: retention expiry cleared the artifact content and the
    /// evidence excerpt but left the source wording copied into
    /// `causal_claims.context_json` and a negative control's `rationale` on
    /// disk — only `erase_artifact` scrubbed those.
    #[test]
    fn retention_expiry_scrubs_claim_context_and_negative_control_rationale() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let tenant = scope("a");
        let source = store
            .add_artifact(
                &tenant,
                "ticket",
                "expiring",
                "v1",
                "expiring",
                "staffing lowered backlog",
                1,
                now() + 3600,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &tenant,
                "staffing",
                "backlog",
                0,
                1,
                &serde_json::json!({ "original_variable_names": ["staffing lowered backlog"] }),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &tenant,
                &claim.id,
                &source.id,
                0,
                8,
                "staffing",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        // Foreign keys are enforced (`PRAGMA foreign_keys = ON`), so the
        // review's model and variable must exist.
        conn.execute_batch(
            "INSERT INTO causal_variables
             (id,tenant_id,acl,name,version,definition,unit,value_kind,created_at)
             VALUES ('var','a','private','backlog','v1','defn','count','count',1);
             INSERT INTO causal_models
             (id,tenant_id,acl,name,version,treatment_variable_id,outcome_variable_id,
              population,window_start,window_end,created_at)
             VALUES ('model','a','private','test','v1','var','var','all',0,1,1);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO causal_negative_control_reviews
             (id,model_id,variable_id,protocol_artifact_id,protocol_sha256,verdict,
              rationale,reviewer,reviewed_at)
             VALUES ('nc','model','var',?1,'hash','pass',
              'protocol says: staffing lowered backlog','reviewer',1)",
            [&source.id],
        )
        .unwrap();
        // Reaching the retention horizon without waiting for wall-clock time.
        conn.execute(
            "UPDATE causal_artifacts SET retention_at=1 WHERE id=?1",
            [&source.id],
        )
        .unwrap();

        // Stand in for the next request's fresh store, which always runs the
        // retention sweep on its first open.
        store.reset_maintenance_throttle();
        store.open().unwrap();

        let (content, excerpt, context, rationale): (String, String, String, String) = conn
            .query_row(
                "SELECT a.content,
                  (SELECT excerpt FROM causal_evidence WHERE artifact_id=a.id),
                  (SELECT context_json FROM causal_claims WHERE id=?2),
                  (SELECT rationale FROM causal_negative_control_reviews
                    WHERE protocol_artifact_id=a.id)
                 FROM causal_artifacts a WHERE a.id=?1",
                params![source.id, claim.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(content, "");
        assert_eq!(excerpt, "");
        assert_eq!(
            context, "{}",
            "expiry is a deletion promise — copied wording in claim context expires with it"
        );
        assert_eq!(rationale, "");
    }

    #[test]
    fn duplicate_lineage_extractions_share_a_claim_and_count_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let first = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "copy-1",
                "v1",
                "origin-thread-1",
                "staffing lowered backlog",
                1,
                i64::MAX,
            )
            .unwrap();
        let second = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "copy-2",
                "v1",
                "origin-thread-1",
                "staffing lowered backlog, revised",
                2,
                i64::MAX,
            )
            .unwrap();
        let independent = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "independent",
                "v1",
                "origin-thread-2",
                "staffing lowered backlog in a separately observed queue",
                3,
                i64::MAX,
            )
            .unwrap();
        let copied_other_lineage = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "claimed-independent-copy",
                "v1",
                "claimed-thread-3",
                "staffing lowered backlog, revised",
                4,
                i64::MAX,
            )
            .unwrap();
        let proposal = ProposedCausalClaim {
            cause_variable: "staffing".into(),
            effect_variable: "backlog".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86_400,
            modality: ClaimModality::Asserted,
            stance: EvidenceStance::Supports,
            span_start: 0,
            span_end: 8,
            excerpt: "staffing".into(),
            speaker_id: None,
            context: serde_json::json!({"queue":"support"}),
        };
        let (first_claim, first_span) = store
            .ingest_extracted_claim(&scope("a"), &first.id, "Why?", "extractor-v1", &proposal)
            .unwrap();
        let (second_claim, second_span) = store
            .ingest_extracted_claim(
                &scope("a"),
                &second.id,
                "What changed?",
                "extractor-v2",
                &proposal,
            )
            .unwrap();
        assert_eq!(first_claim.id, second_claim.id);
        assert_ne!(first_span.id, second_span.id);
        let (_, repeated_span) = store
            .ingest_extracted_claim(&scope("a"), &first.id, "Again?", "extractor-v3", &proposal)
            .unwrap();
        assert_eq!(first_span.id, repeated_span.id);
        let (copied_claim, copied_span) = store
            .ingest_extracted_claim(
                &scope("a"),
                &copied_other_lineage.id,
                "Copied source?",
                "extractor-v4",
                &proposal,
            )
            .unwrap();
        assert_eq!(copied_claim.id, first_claim.id);
        assert_ne!(copied_span.id, first_span.id);
        assert_eq!(
            store
                .claim_lineage_summary(&scope("a"), &first_claim.id)
                .unwrap(),
            ClaimLineageSummary {
                supporting_spans: 3,
                opposing_spans: 0,
                independent_supporting_lineages: 1,
                independent_opposing_lineages: 0,
                mixed_stance_lineages: 0,
            }
        );
        let mut opposing = proposal.clone();
        opposing.stance = EvidenceStance::Opposes;
        let (opposing_claim, _) = store
            .ingest_extracted_claim(
                &scope("a"),
                &second.id,
                "Disagree?",
                "extractor-v2",
                &opposing,
            )
            .unwrap();
        assert_eq!(opposing_claim.id, first_claim.id);
        store
            .add_evidence(
                &scope("a"),
                &first_claim.id,
                &independent.id,
                0,
                8,
                "staffing",
                EvidenceStance::Supports,
                None,
                "manual-v1",
            )
            .unwrap();
        let summary = store
            .claim_lineage_summary(&scope("a"), &first_claim.id)
            .unwrap();
        assert_eq!(summary.supporting_spans, 4);
        assert_eq!(summary.independent_supporting_lineages, 2);
        assert_eq!(summary.opposing_spans, 1);
        assert_eq!(summary.independent_opposing_lineages, 1);
        assert_eq!(summary.mixed_stance_lineages, 1);
        assert!(matches!(
            store.claim_lineage_summary(&scope("b"), &first_claim.id),
            Err(CausalStoreError::NotFound)
        ));
        store.invalidate_artifact(&scope("a"), &second.id).unwrap();
        let summary = store
            .claim_lineage_summary(&scope("a"), &first_claim.id)
            .unwrap();
        assert_eq!(summary.supporting_spans, 3);
        assert_eq!(summary.independent_supporting_lineages, 2);
        assert_eq!(summary.opposing_spans, 0);
        assert_eq!(summary.mixed_stance_lineages, 0);
    }

    #[test]
    fn altered_source_content_cannot_ground_new_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let source = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "t1",
                "v1",
                "origin-1",
                "staffing lowered backlog",
                1,
                i64::MAX,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &scope("a"),
                "staffing",
                "backlog",
                0,
                86_400,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET content='staffing raised backlog' WHERE id=?1",
            [&source.id],
        )
        .unwrap();
        assert!(matches!(
            store.source_text(&scope("a"), &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.add_evidence(
                &scope("a"),
                &claim.id,
                &source.id,
                0,
                8,
                "staffing",
                EvidenceStance::Supports,
                None,
                "v1"
            ),
            Err(CausalStoreError::NotFound)
        ));
        let proposal = ProposedCausalClaim {
            cause_variable: "staffing".into(),
            effect_variable: "backlog".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86_400,
            modality: ClaimModality::Asserted,
            stance: EvidenceStance::Supports,
            span_start: 0,
            span_end: 8,
            excerpt: "staffing".into(),
            speaker_id: None,
            context: serde_json::json!({}),
        };
        assert!(matches!(
            store.ingest_extracted_claim(&scope("a"), &source.id, "Why?", "v1", &proposal),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store
                .claim_lineage_summary(&scope("a"), &claim.id)
                .unwrap()
                .supporting_spans,
            0
        );
    }

    #[test]
    fn claims_require_exact_active_support_and_human_review() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let source = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "t1",
                "v1",
                "thread-1",
                "增加人力後，積壓下降。",
                100,
                now() + 3600,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &scope("a"),
                "staffing",
                "backlog",
                0,
                86400,
                &serde_json::json!({"department":"support"}),
                ClaimModality::Asserted,
            )
            .unwrap();
        assert!(matches!(
            store.review_claim(&scope("a"), &claim.id, "alice", true),
            Err(CausalStoreError::MissingSupport)
        ));
        assert!(matches!(
            store.add_evidence(
                &scope("a"),
                &claim.id,
                &source.id,
                0,
                6,
                "不存在",
                EvidenceStance::Supports,
                None,
                "v1"
            ),
            Err(CausalStoreError::InvalidInput)
        ));
        let excerpt = "增加人力後";
        store
            .add_evidence(
                &scope("a"),
                &claim.id,
                &source.id,
                0,
                excerpt.len(),
                excerpt,
                EvidenceStance::Supports,
                None,
                "v1",
            )
            .unwrap();
        store
            .review_claim_if_state(&scope("a"), &claim.id, "alice", true, "candidate")
            .unwrap();
        assert!(matches!(
            store.review_claim_if_state(&scope("a"), &claim.id, "bob", false, "candidate"),
            Err(CausalStoreError::Conflict)
        ));
        assert_eq!(
            store.claim_state(&scope("a"), &claim.id).unwrap(),
            "accepted"
        );
        assert!(matches!(
            store.claim_state(&scope("b"), &claim.id),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store.invalidate_artifact(&scope("a"), &source.id).unwrap(),
            1
        );
        assert_eq!(
            store.claim_state(&scope("a"), &claim.id).unwrap(),
            "needs_review"
        );
    }

    #[test]
    fn one_artifact_version_cannot_silently_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let retention = now() + 3600;
        let a = store
            .add_artifact(
                &scope("a"),
                "meeting",
                "m1",
                "v1",
                "m1",
                "original",
                100,
                retention,
            )
            .unwrap();
        let again = store
            .add_artifact(
                &scope("a"),
                "meeting",
                "m1",
                "v1",
                "m1",
                "original",
                100,
                retention,
            )
            .unwrap();
        assert_eq!(a.id, again.id);
        assert!(matches!(
            store.add_artifact(
                &scope("a"),
                "meeting",
                "m1",
                "v1",
                "m1",
                "changed",
                100,
                retention
            ),
            Err(CausalStoreError::InvalidInput)
        ));
    }

    #[test]
    fn expired_support_is_not_returned_as_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let source = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "t1",
                "v1",
                "thread-1",
                "capacity caused delay",
                100,
                now() + 3600,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &scope("a"),
                "capacity",
                "delay",
                0,
                3600,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope("a"),
                &claim.id,
                &source.id,
                0,
                8,
                "capacity",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        store
            .review_claim(&scope("a"), &claim.id, "reviewer", true)
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET retention_at=0 WHERE id=?1",
            [&source.id],
        )
        .unwrap();
        // Stand in for the next request's fresh store, which always runs the
        // retention sweep on its first open.
        store.reset_maintenance_throttle();
        assert_eq!(
            store.claim_state(&scope("a"), &claim.id).unwrap(),
            "needs_review"
        );
        let (content, excerpt): (String, String) = conn
            .query_row(
                "SELECT a.content, e.excerpt FROM causal_artifacts a JOIN causal_evidence e
             ON e.artifact_id=a.id WHERE a.id=?1",
                [&source.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(content.is_empty() && excerpt.is_empty());
    }

    #[test]
    fn erase_scrubs_source_and_spans_with_exact_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let source = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "t1",
                "v1",
                "thread-1",
                "secret cause",
                100,
                now() + 3600,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &scope("a"),
                "secret",
                "cause",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope("a"),
                &claim.id,
                &source.id,
                0,
                6,
                "secret",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        store
            .review_claim(&scope("a"), &claim.id, "reviewer", true)
            .unwrap();
        assert!(matches!(
            store.erase_artifact(&scope("b"), &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(store.erase_artifact(&scope("a"), &source.id).unwrap(), 1);
        assert_eq!(
            store.claim_state(&scope("a"), &claim.id).unwrap(),
            "needs_review"
        );
        let conn = Connection::open(store.path()).unwrap();
        let (raw, excerpt): (String, String) = conn
            .query_row(
                "SELECT a.content, e.excerpt FROM causal_artifacts a
                 JOIN causal_evidence e ON e.artifact_id=a.id WHERE a.id=?1",
                [&source.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(raw.is_empty() && excerpt.is_empty());
    }

    #[test]
    fn extracted_candidate_is_grounded_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let content = "客服量增加，等待時間變長。";
        let source = store
            .add_artifact(
                &scope("a"),
                "ticket",
                "t2",
                "v1",
                "thread-2",
                content,
                100,
                now() + 3600,
            )
            .unwrap();
        assert!(matches!(
            store.source_text(&scope("b"), &source.id),
            Err(CausalStoreError::NotFound)
        ));
        let excerpt = "等待時間變長";
        let start = content.find(excerpt).unwrap();
        let mut proposed = ProposedCausalClaim {
            cause_variable: "ticket_volume".into(),
            effect_variable: "wait_time".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86400,
            modality: ClaimModality::Asserted,
            stance: EvidenceStance::Supports,
            span_start: start,
            span_end: start + excerpt.len(),
            excerpt: "捏造內容".into(),
            speaker_id: Some("operator".into()),
            context: serde_json::json!({"team":"support","private_source_quote":"等待時間變長"}),
        };
        assert!(matches!(
            store.ingest_extracted_claim(
                &scope("a"),
                &source.id,
                "What drives wait?",
                "extractor-v1",
                &proposed
            ),
            Err(CausalStoreError::InvalidInput)
        ));
        let conn = Connection::open(store.path()).unwrap();
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM causal_claims", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 0);
        proposed.excerpt = excerpt.into();
        let (claim, span) = store
            .ingest_extracted_claim(
                &scope("a"),
                &source.id,
                "What drives wait?",
                "extractor-v1",
                &proposed,
            )
            .unwrap();
        assert_eq!(claim.review_state, "candidate");
        assert_eq!(span.excerpt, excerpt);
        assert!(claim.context_json.contains("thread-2"));
        assert!(!claim.context_json.contains(excerpt));
        assert!(!claim.context_json.contains("What drives wait?"));
        assert_eq!(
            store
                .list_claim_ids(&scope("a"), Some("candidate"), 10)
                .unwrap(),
            vec![claim.id.clone()]
        );
        assert!(
            store
                .list_claim_ids(&scope("b"), None, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .read_claim(&scope("a"), &claim.id)
                .unwrap()
                .cause_variable,
            "ticket_volume"
        );
        let details = store.evidence_for_claim(&scope("a"), &claim.id).unwrap();
        assert_eq!(details[0].span.excerpt, excerpt);
        assert!(details[0].source_active);
        store.invalidate_artifact(&scope("a"), &source.id).unwrap();
        let details = store.evidence_for_claim(&scope("a"), &claim.id).unwrap();
        assert!(!details[0].source_active);
        assert!(details[0].span.excerpt.is_empty());
    }
}
