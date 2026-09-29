//! Durable bridge from a *local, trusted* connector adapter to causal source
//! revocation. An MCP result or caller-supplied `_meta` is never an event.
//!
//! Bindings, the content-free event journal, and the causal disclosure fence
//! live in the same `memory.db` transaction. A process crash can therefore
//! leave retryable work, but cannot leave a committed event with unfenced
//! source bytes. The actual cross-database cascade uses DecisionStore's
//! retry-safe removal path.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use duduclaw_memory::causal::{CausalStore, CausalStoreError, EvidenceScope, SourceArtifact};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::decision_store::{
    CausalSourceRemoval, DecisionScope, DecisionStore, DecisionStoreError,
};

const MAX_KEY_BYTES: usize = 512;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn valid_key(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_KEY_BYTES && !value.contains('\0')
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("invalid connector lifecycle input")]
    Invalid,
    #[error("connector source binding not found in scope")]
    NotFound,
    #[error("connector generation or source identity conflict")]
    Conflict,
    #[error("connector generation is stale or already revoked")]
    Stale,
    #[error("causal source error: {0}")]
    Causal(#[from] CausalStoreError),
    #[error("decision source removal error: {0}")]
    Decision(#[from] DecisionStoreError),
    #[error("connector lifecycle storage error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Identity supplied only by a local adapter after it has authenticated its
/// upstream event and written the corresponding immutable causal artifact.
/// No HTTP or MCP argument should be deserialized into this type directly.
#[derive(Debug, Clone)]
pub(crate) struct LocalSourceBinding {
    scope: EvidenceScope,
    connector: String,
    external_id: String,
    generation: i64,
    artifact_id: String,
    version: String,
    content_sha256: String,
}

impl LocalSourceBinding {
    /// A gateway-local adapter may call this only after authenticating its
    /// upstream identity. The artifact ID, version, and digest come from the
    /// local causal store, never from MCP arguments or tool-result metadata.
    pub(crate) fn from_verified_local_adapter(
        scope: EvidenceScope,
        connector: impl Into<String>,
        generation: i64,
        artifact: &SourceArtifact,
    ) -> Self {
        Self {
            scope,
            connector: connector.into(),
            external_id: artifact.external_id.clone(),
            generation,
            artifact_id: artifact.id.clone(),
            version: artifact.version.clone(),
            content_sha256: artifact.content_sha256.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleKind {
    AclLost,
    Deleted,
    Quarantined,
    VersionChanged,
}

impl LifecycleKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::AclLost => "acl_lost",
            Self::Deleted => "deleted",
            Self::Quarantined => "quarantined",
            Self::VersionChanged => "version_changed",
        }
    }

    fn removal(self) -> CausalSourceRemoval {
        if self == Self::VersionChanged {
            CausalSourceRemoval::Invalidate
        } else {
            CausalSourceRemoval::Erase
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LocalLifecycleEvent {
    scope: EvidenceScope,
    connector: String,
    external_id: String,
    generation: i64,
    kind: LifecycleKind,
}

impl LocalLifecycleEvent {
    /// Only a gateway-local authenticated connector adapter should construct
    /// this event. Its target is still resolved against the durable binding.
    pub(crate) fn from_verified_local_adapter(
        scope: EvidenceScope,
        connector: impl Into<String>,
        external_id: impl Into<String>,
        generation: i64,
        kind: LifecycleKind,
    ) -> Self {
        Self {
            scope,
            connector: connector.into(),
            external_id: external_id.into(),
            generation,
            kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindOutcome {
    Bound,
    AlreadyBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StageOutcome {
    Staged,
    AlreadyStaged,
    AlreadyCompleted,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LifecycleDrainReport {
    pub completed: u64,
    pub blocked: u64,
    pub retry_pending: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHealthStatus {
    #[default]
    Unavailable,
    MissingSchema,
    Available,
}

/// Sanitized tenant inventory. No connector, external ID, artifact ID, source
/// version, content hash, or source wording leaves this aggregate.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct LifecycleHealth {
    pub status: LifecycleHealthStatus,
    pub pending_count: u64,
    pub blocked_count: u64,
    pub completed_count: u64,
    pub oldest_pending_age_seconds: Option<u64>,
    pub latest_completed_age_seconds: Option<u64>,
    pub last_failure_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TrustedConnectorLifecycleBridge {
    causal: CausalStore,
    decisions: DecisionStore,
    ccr_db: PathBuf,
}

impl TrustedConnectorLifecycleBridge {
    pub fn for_home(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref();
        let causal = CausalStore::new(home.join("memory.db"));
        Self {
            decisions: DecisionStore::with_causal_store(home.join("decisions.db"), causal.clone()),
            causal,
            ccr_db: home.join("ccr").join("ccr.db"),
        }
    }

    fn open(&self) -> Result<Connection, LifecycleError> {
        if !self.causal.path().is_file() {
            return Err(LifecycleError::NotFound);
        }
        let mut conn =
            Connection::open_with_flags(self.causal.path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_connector_source_bindings (
               tenant_id TEXT NOT NULL, acl TEXT NOT NULL, connector TEXT NOT NULL,
               external_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation>0),
               artifact_id TEXT NOT NULL, version TEXT NOT NULL,
               content_sha256 TEXT NOT NULL, state TEXT NOT NULL
                 CHECK(state IN ('active','revoking','completed')),
               bound_at INTEGER NOT NULL,
               PRIMARY KEY(tenant_id,acl,connector,external_id,generation),
               UNIQUE(tenant_id,acl,artifact_id)
             );
             CREATE TABLE IF NOT EXISTS local_connector_lifecycle_events (
               tenant_id TEXT NOT NULL, acl TEXT NOT NULL, connector TEXT NOT NULL,
               external_id TEXT NOT NULL, generation INTEGER NOT NULL,
               artifact_id TEXT NOT NULL, version TEXT NOT NULL,
               content_sha256 TEXT NOT NULL,
               kind TEXT NOT NULL CHECK(kind IN
                 ('acl_lost','deleted','quarantined','version_changed')),
               status TEXT NOT NULL CHECK(status IN ('pending','blocked','completed')),
               last_failure_code TEXT,
               attempt_count INTEGER NOT NULL DEFAULT 0,
               staged_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
               PRIMARY KEY(tenant_id,acl,connector,external_id,generation),
               FOREIGN KEY(tenant_id,acl,connector,external_id,generation)
                 REFERENCES local_connector_source_bindings
                   (tenant_id,acl,connector,external_id,generation)
             );
             CREATE INDEX IF NOT EXISTS idx_local_connector_events_status
               ON local_connector_lifecycle_events(status,staged_at);",
        )?;
        let mut statement = tx.prepare("PRAGMA table_info(local_connector_lifecycle_events)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if !columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case("last_failure_code"))
        {
            tx.execute_batch(
                "ALTER TABLE local_connector_lifecycle_events
                 ADD COLUMN last_failure_code TEXT;",
            )?;
        }
        if !columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case("attempt_count"))
        {
            tx.execute_batch(
                "ALTER TABLE local_connector_lifecycle_events
                 ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        tx.commit()?;
        Ok(conn)
    }

    /// Register exactly one immutable local causal artifact for this upstream
    /// generation. A higher generation is accepted only after the old one has
    /// been fenced; older generations can never be rebound or resurrected.
    pub(crate) fn bind_local_source(
        &self,
        binding: &LocalSourceBinding,
    ) -> Result<BindOutcome, LifecycleError> {
        if !valid_binding(binding) {
            return Err(LifecycleError::Invalid);
        }
        let source = self
            .causal
            .read_artifact_metadata(&binding.scope, &binding.artifact_id)?;
        if source.external_id != binding.external_id
            || source.version != binding.version
            || source.content_sha256 != binding.content_sha256
        {
            return Err(LifecycleError::Conflict);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest: Option<(i64, String, String, String, String)> = tx
            .query_row(
                "SELECT generation,artifact_id,version,content_sha256,state
                 FROM local_connector_source_bindings
                 WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4
                 ORDER BY generation DESC LIMIT 1",
                params![
                    binding.scope.tenant_id,
                    binding.scope.acl,
                    binding.connector,
                    binding.external_id
                ],
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
        if let Some((generation, id, version, digest, state)) = latest {
            if generation > binding.generation {
                return Err(LifecycleError::Stale);
            }
            if generation == binding.generation {
                return if id == binding.artifact_id
                    && version == binding.version
                    && digest == binding.content_sha256
                    && state == "active"
                {
                    Ok(BindOutcome::AlreadyBound)
                } else {
                    Err(LifecycleError::Conflict)
                };
            }
            if state == "active" {
                return Err(LifecycleError::Conflict);
            }
        }
        // Recheck after acquiring the writer lock. No source mutation can race
        // the binding commit, and the original digest is checked from bytes.
        let current: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT external_id,version,content_sha256,content FROM causal_artifacts a
                 WHERE id=?1 AND tenant_id=?2 AND acl=?3
                   AND invalidated_at IS NULL AND retention_at>?4
                   AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                    WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                      AND r.artifact_id=a.id AND r.version=a.version)
                   AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                    WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                      AND o.artifact_id=a.id AND o.version=a.version
                      AND o.delivered_at IS NULL)",
                params![
                    binding.artifact_id,
                    binding.scope.tenant_id,
                    binding.scope.acl,
                    now()
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((external_id, version, digest, content)) = current else {
            return Err(LifecycleError::NotFound);
        };
        if external_id != binding.external_id
            || version != binding.version
            || digest != binding.content_sha256
            || format!("{:x}", Sha256::digest(content.as_bytes())) != digest
        {
            return Err(LifecycleError::Conflict);
        }
        tx.execute(
            "INSERT INTO local_connector_source_bindings
             (tenant_id,acl,connector,external_id,generation,artifact_id,version,
              content_sha256,state,bound_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'active',?9)",
            params![
                binding.scope.tenant_id,
                binding.scope.acl,
                binding.connector,
                binding.external_id,
                binding.generation,
                binding.artifact_id,
                binding.version,
                binding.content_sha256,
                now()
            ],
        )?;
        tx.commit()?;
        Ok(BindOutcome::Bound)
    }

    /// Stage a typed, authenticated local adapter event and fence the exact
    /// immutable causal version in one writer transaction. The adapter must
    /// establish upstream authenticity before calling this method.
    pub(crate) fn stage_local_event(
        &self,
        event: &LocalLifecycleEvent,
    ) -> Result<StageOutcome, LifecycleError> {
        if !valid_key(&event.scope.tenant_id)
            || !valid_key(&event.scope.acl)
            || !valid_key(&event.connector)
            || !valid_key(&event.external_id)
            || event.generation <= 0
        {
            return Err(LifecycleError::Invalid);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_generation: Option<i64> = tx.query_row(
            "SELECT MAX(generation) FROM local_connector_source_bindings
             WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4",
            params![
                event.scope.tenant_id,
                event.scope.acl,
                event.connector,
                event.external_id
            ],
            |row| row.get(0),
        )?;
        let historical_terminal = matches!(
            current_generation,
            Some(generation) if generation > event.generation
                && matches!(
                    event.kind,
                    LifecycleKind::Deleted | LifecycleKind::AclLost | LifecycleKind::Quarantined
                )
        );
        match current_generation {
            Some(generation) if generation > event.generation && !historical_terminal => {
                return Err(LifecycleError::Stale);
            }
            Some(generation) if generation == event.generation => {}
            Some(generation) if generation > event.generation && historical_terminal => {}
            _ => return Err(LifecycleError::NotFound),
        }
        // A terminal event for an older generation must scrub that old
        // version's retained wording, while leaving the current generation
        // untouched. Its journal row is upgraded to the erase action.
        let effective_kind = if historical_terminal {
            LifecycleKind::Deleted
        } else {
            event.kind
        };
        let (artifact_id, version, digest, state): (String, String, String, String) = tx
            .query_row(
                "SELECT artifact_id,version,content_sha256,state
             FROM local_connector_source_bindings
             WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4 AND generation=?5",
                params![
                    event.scope.tenant_id,
                    event.scope.acl,
                    event.connector,
                    event.external_id,
                    event.generation
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        // Verify the stored binding against the retained immutable source on
        // every replay, including a completed generation. An out-of-band
        // journal edit cannot redirect a terminal upgrade to another source.
        let source: Option<(String, String, String, String, Option<i64>)> = tx
            .query_row(
                "SELECT external_id,version,content_sha256,content,invalidated_at
                 FROM causal_artifacts
                 WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![artifact_id, event.scope.tenant_id, event.scope.acl],
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
        let Some((actual_external_id, actual_version, actual_digest, content, invalidated_at)) =
            source
        else {
            return Err(LifecycleError::NotFound);
        };
        if actual_external_id != event.external_id
            || actual_version != version
            || actual_digest != digest
            || (invalidated_at.is_none()
                && format!("{:x}", Sha256::digest(content.as_bytes())) != digest)
        {
            return Err(LifecycleError::Conflict);
        }
        let prior: Option<(String, String, String, String, String)> = tx
            .query_row(
                "SELECT kind,status,artifact_id,version,content_sha256
                 FROM local_connector_lifecycle_events
             WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4 AND generation=?5",
                params![
                    event.scope.tenant_id,
                    event.scope.acl,
                    event.connector,
                    event.external_id,
                    event.generation
                ],
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
        if let Some((prior_kind, status, prior_id, prior_version, prior_digest)) = prior {
            if prior_id != artifact_id || prior_version != version || prior_digest != digest {
                return Err(LifecycleError::Conflict);
            }
            // A terminal event must erase wording even when a previously
            // completed version-change event only invalidated the source.
            // Canonicalizing it to deleted also repairs older journal rows
            // whose ACL/quarantine handler only invalidated the source.
            if effective_kind != LifecycleKind::VersionChanged && prior_kind != "deleted" {
                tx.execute(
                    "UPDATE local_connector_lifecycle_events
                     SET kind='deleted',status='pending',last_failure_code=NULL,
                         attempt_count=0,updated_at=?1
                     WHERE tenant_id=?2 AND acl=?3 AND connector=?4 AND external_id=?5 AND generation=?6",
                    params![now(), event.scope.tenant_id, event.scope.acl,
                        event.connector, event.external_id, event.generation],
                )?;
                tx.execute(
                    "UPDATE local_connector_source_bindings SET state='revoking'
                     WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4 AND generation=?5",
                    params![event.scope.tenant_id, event.scope.acl, event.connector,
                        event.external_id, event.generation],
                )?;
                tx.commit()?;
                return Ok(StageOutcome::Staged);
            }
            return Ok(if status == "completed" {
                StageOutcome::AlreadyCompleted
            } else {
                StageOutcome::AlreadyStaged
            });
        }
        if state != "active" {
            return Err(LifecycleError::Conflict);
        }
        tx.execute(
            "INSERT OR IGNORE INTO causal_ccr_revoking
             (tenant_id,acl,artifact_id,version,started_at) VALUES (?1,?2,?3,?4,?5)",
            params![
                event.scope.tenant_id,
                event.scope.acl,
                artifact_id,
                version,
                now()
            ],
        )?;
        tx.execute(
            "INSERT INTO local_connector_lifecycle_events
             (tenant_id,acl,connector,external_id,generation,artifact_id,version,
              content_sha256,kind,status,staged_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending',?10,?10)",
            params![
                event.scope.tenant_id,
                event.scope.acl,
                event.connector,
                event.external_id,
                event.generation,
                artifact_id,
                version,
                digest,
                effective_kind.as_str(),
                now()
            ],
        )?;
        tx.execute(
            "UPDATE local_connector_source_bindings SET state='revoking'
             WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4 AND generation=?5",
            params![
                event.scope.tenant_id,
                event.scope.acl,
                event.connector,
                event.external_id,
                event.generation
            ],
        )?;
        tx.commit()?;
        Ok(StageOutcome::Staged)
    }

    /// Process a bounded batch. Blocked active delivery leases remain fenced
    /// and are retried on the next call. Failures never clear a staged event.
    pub fn drain_once(&self, limit: usize) -> Result<LifecycleDrainReport, LifecycleError> {
        if limit == 0 || !self.causal.path().is_file() {
            return Ok(LifecycleDrainReport::default());
        }
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT tenant_id,acl,connector,external_id,generation,artifact_id,version,
                    content_sha256,kind FROM local_connector_lifecycle_events
             WHERE status IN ('pending','blocked')
             ORDER BY attempt_count,staged_at,rowid LIMIT ?1",
        )?;
        let events = statement
            .query_map([limit.min(256) as i64], |row| {
                Ok(JournalEvent {
                    scope: EvidenceScope {
                        tenant_id: row.get(0)?,
                        acl: row.get(1)?,
                    },
                    connector: row.get(2)?,
                    external_id: row.get(3)?,
                    generation: row.get(4)?,
                    artifact_id: row.get(5)?,
                    version: row.get(6)?,
                    digest: row.get(7)?,
                    kind: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(conn);
        let mut report = LifecycleDrainReport::default();
        for event in events {
            // Journal corruption must not redirect a removal to another
            // artifact or generation. The source record retains its metadata
            // even after invalidation or erasure, so retries can verify it.
            if !self.journal_identity_matches(&event)? {
                let conn = self.open()?;
                conn.execute(
                    "UPDATE local_connector_lifecycle_events
                     SET status='blocked',last_failure_code='identity_mismatch',
                         attempt_count=attempt_count+1,updated_at=?1
                     WHERE tenant_id=?2 AND acl=?3 AND connector=?4 AND external_id=?5
                       AND generation=?6 AND kind=?7 AND status IN ('pending','blocked')",
                    params![
                        now(),
                        event.scope.tenant_id,
                        event.scope.acl,
                        event.connector,
                        event.external_id,
                        event.generation,
                        event.kind
                    ],
                )?;
                report.retry_pending += 1;
                continue;
            }
            let kind = match event.kind.as_str() {
                "deleted" => LifecycleKind::Deleted,
                "acl_lost" => LifecycleKind::AclLost,
                "quarantined" => LifecycleKind::Quarantined,
                "version_changed" => LifecycleKind::VersionChanged,
                _ => {
                    report.retry_pending += 1;
                    continue;
                }
            };
            let scope = DecisionScope {
                tenant_id: event.scope.tenant_id.clone(),
                acl: event.scope.acl.clone(),
            };
            let removal = self.decisions.open().and_then(|_| {
                self.decisions.remove_causal_artifact_with_dependents(
                    &scope,
                    &event.artifact_id,
                    kind.removal(),
                    Some(&self.ccr_db),
                )
            });
            match removal {
                Ok(_) => {
                    let mut conn = self.open()?;
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let changed = tx.execute(
                        "UPDATE local_connector_lifecycle_events
                         SET status='completed',last_failure_code=NULL,
                             attempt_count=attempt_count+1,updated_at=?1
                         WHERE tenant_id=?2 AND acl=?3 AND connector=?4 AND external_id=?5
                           AND generation=?6 AND kind=?7 AND status IN ('pending','blocked')",
                        params![
                            now(),
                            event.scope.tenant_id,
                            event.scope.acl,
                            event.connector,
                            event.external_id,
                            event.generation,
                            event.kind
                        ],
                    )?;
                    if changed == 1 {
                        tx.execute(
                            "UPDATE local_connector_source_bindings SET state='completed'
                             WHERE tenant_id=?1 AND acl=?2 AND connector=?3 AND external_id=?4
                               AND generation=?5",
                            params![
                                event.scope.tenant_id,
                                event.scope.acl,
                                event.connector,
                                event.external_id,
                                event.generation
                            ],
                        )?;
                        tx.commit()?;
                        report.completed += 1;
                    } else {
                        tx.commit()?;
                        report.retry_pending += 1;
                    }
                }
                Err(error) => {
                    let blocked = matches!(
                        error,
                        DecisionStoreError::Causal(CausalStoreError::Conflict)
                    );
                    let conn = self.open()?;
                    conn.execute(
                        "UPDATE local_connector_lifecycle_events
                         SET status=?1,last_failure_code=?2,
                             attempt_count=attempt_count+1,updated_at=?3
                         WHERE tenant_id=?4 AND acl=?5 AND connector=?6 AND external_id=?7
                           AND generation=?8 AND kind=?9 AND status IN ('pending','blocked')",
                        params![
                            if blocked { "blocked" } else { "pending" },
                            if blocked {
                                "delivery_lease_active"
                            } else {
                                "dependent_store_unavailable"
                            },
                            now(),
                            event.scope.tenant_id,
                            event.scope.acl,
                            event.connector,
                            event.external_id,
                            event.generation,
                            event.kind
                        ],
                    )?;
                    if blocked {
                        report.blocked += 1
                    } else {
                        report.retry_pending += 1
                    }
                }
            }
        }
        Ok(report)
    }

    fn journal_identity_matches(&self, event: &JournalEvent) -> Result<bool, LifecycleError> {
        let conn = self.open()?;
        let match_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM local_connector_source_bindings b
             JOIN causal_artifacts a ON a.id=b.artifact_id
             WHERE b.tenant_id=?1 AND b.acl=?2 AND b.connector=?3
               AND b.external_id=?4 AND b.generation=?5
               AND b.artifact_id=?6 AND b.version=?7 AND b.content_sha256=?8
               AND a.tenant_id=b.tenant_id AND a.acl=b.acl
               AND a.external_id=b.external_id
               AND a.version=b.version AND a.content_sha256=b.content_sha256",
            params![
                event.scope.tenant_id,
                event.scope.acl,
                event.connector,
                event.external_id,
                event.generation,
                event.artifact_id,
                event.version,
                event.digest
            ],
            |row| row.get(0),
        )?;
        Ok(match_count == 1)
    }

    pub fn tenant_health(&self, tenant_id: &str) -> Result<LifecycleHealth, LifecycleError> {
        if !valid_key(tenant_id) {
            return Err(LifecycleError::Invalid);
        }
        if !self.causal.path().is_file() {
            return Ok(LifecycleHealth::default());
        }
        // Dashboard inspection is strictly read-only: it never runs causal
        // migrations, creates the journal, or sweeps retention.
        let conn =
            Connection::open_with_flags(self.causal.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let schema_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='local_connector_lifecycle_events')",
            [],
            |row| row.get(0),
        )?;
        if !schema_exists {
            return Ok(LifecycleHealth {
                status: LifecycleHealthStatus::MissingSchema,
                ..LifecycleHealth::default()
            });
        }
        let (pending, blocked, oldest): (i64, i64, Option<i64>) = conn.query_row(
            "SELECT COUNT(*),COALESCE(SUM(status='blocked'),0),MIN(staged_at)
             FROM local_connector_lifecycle_events
             WHERE tenant_id=?1 AND status IN ('pending','blocked')",
            [tenant_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let (completed, latest_completed): (i64, Option<i64>) = conn.query_row(
            "SELECT COUNT(*),MAX(updated_at) FROM local_connector_lifecycle_events
             WHERE tenant_id=?1 AND status='completed'",
            [tenant_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let last_failure_code: Option<String> = conn
            .query_row(
                "SELECT last_failure_code FROM local_connector_lifecycle_events
             WHERE tenant_id=?1 AND status IN ('pending','blocked')
               AND last_failure_code IN
                 ('delivery_lease_active','dependent_store_unavailable','identity_mismatch')
             ORDER BY updated_at DESC,rowid DESC LIMIT 1",
                [tenant_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(LifecycleHealth {
            status: LifecycleHealthStatus::Available,
            pending_count: pending.max(0) as u64,
            blocked_count: blocked.max(0) as u64,
            completed_count: completed.max(0) as u64,
            oldest_pending_age_seconds: oldest.map(|time| now().saturating_sub(time) as u64),
            latest_completed_age_seconds: latest_completed
                .map(|time| now().saturating_sub(time) as u64),
            last_failure_code,
        })
    }
}

/// Startup and periodic bounded retry worker. Each pass processes one batch
/// and journals its result, so an unavailable downstream store never traps
/// the async runtime or suppresses later events.
pub async fn run(home: PathBuf, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let next_home = home.clone();
        match tokio::task::spawn_blocking(move || {
            TrustedConnectorLifecycleBridge::for_home(next_home).drain_once(128)
        })
        .await
        {
            Ok(Ok(report)) if report == LifecycleDrainReport::default() => {}
            Ok(Ok(report)) => tracing::info!(
                completed = report.completed,
                blocked = report.blocked,
                retry_pending = report.retry_pending,
                "trusted connector lifecycle retry pass"
            ),
            Ok(Err(error)) => {
                tracing::warn!(%error, "trusted connector lifecycle retry will resume")
            }
            Err(error) => tracing::warn!(%error, "trusted connector lifecycle retry worker failed"),
        }
    }
}

#[derive(Debug)]
struct JournalEvent {
    scope: EvidenceScope,
    connector: String,
    external_id: String,
    generation: i64,
    artifact_id: String,
    version: String,
    digest: String,
    kind: String,
}

fn valid_binding(binding: &LocalSourceBinding) -> bool {
    valid_key(&binding.scope.tenant_id)
        && valid_key(&binding.scope.acl)
        && valid_key(&binding.connector)
        && valid_key(&binding.external_id)
        && valid_key(&binding.artifact_id)
        && valid_key(&binding.version)
        && binding.content_sha256.len() == 64
        && binding
            .content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        && binding.generation > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_sim::DecisionSnapshot;
    use duduclaw_llm::{CcrScope, CcrSourceArtifact, CcrStore};
    use duduclaw_memory::causal::{ClaimModality, EvidenceStance};

    fn source(
        causal: &CausalStore,
        tenant: &str,
        external_id: &str,
        version: &str,
    ) -> (EvidenceScope, duduclaw_memory::causal::SourceArtifact) {
        let scope = EvidenceScope {
            tenant_id: tenant.into(),
            acl: "private".into(),
        };
        let artifact = causal
            .add_artifact(
                &scope,
                "ticket_export",
                external_id,
                version,
                external_id,
                &format!("synthetic ticket data {tenant} {external_id} {version}"),
                1,
                i64::MAX / 2,
            )
            .unwrap();
        (scope, artifact)
    }

    fn binding(
        scope: &EvidenceScope,
        artifact: &duduclaw_memory::causal::SourceArtifact,
        generation: i64,
    ) -> LocalSourceBinding {
        LocalSourceBinding::from_verified_local_adapter(
            scope.clone(),
            "local_test_adapter",
            generation,
            artifact,
        )
    }

    fn event(scope: &EvidenceScope, generation: i64, kind: LifecycleKind) -> LocalLifecycleEvent {
        LocalLifecycleEvent::from_verified_local_adapter(
            scope.clone(),
            "local_test_adapter",
            "ticket-1",
            generation,
            kind,
        )
    }

    #[test]
    fn dashboard_inventory_does_not_create_source_or_journal_database() {
        let home = tempfile::tempdir().unwrap();
        let bridge = TrustedConnectorLifecycleBridge::for_home(home.path());
        let health = bridge.tenant_health("tenant-a").unwrap();
        assert_eq!(health.status, LifecycleHealthStatus::Unavailable);
        assert!(!home.path().join("memory.db").exists());

        let causal = CausalStore::new(home.path().join("memory.db"));
        let _ = source(&causal, "tenant-a", "ticket-1", "v1");
        let health = bridge.tenant_health("tenant-a").unwrap();
        assert_eq!(health.status, LifecycleHealthStatus::MissingSchema);
        let conn = Connection::open(causal.path()).unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='local_connector_lifecycle_events')",
            [], |row| row.get(0),
        ).unwrap();
        assert!(!exists);
    }

    #[test]
    fn old_event_schema_with_pending_row_migrates_and_retries() {
        let home = tempfile::tempdir().unwrap();
        let bridge = TrustedConnectorLifecycleBridge::for_home(home.path());
        let causal = CausalStore::new(home.path().join("memory.db"));
        let (scope, artifact) = source(&causal, "tenant-a", "ticket-1", "v1");
        bridge
            .bind_local_source(&binding(&scope, &artifact, 1))
            .unwrap();
        bridge
            .stage_local_event(&event(&scope, 1, LifecycleKind::Quarantined))
            .unwrap();

        // Recreate the previous released journal layout: its pending row and
        // causal fence survive, but attempt_count has not been introduced.
        let conn = Connection::open(causal.path()).unwrap();
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE local_connector_lifecycle_events RENAME TO old_lifecycle_events;
             CREATE TABLE local_connector_lifecycle_events (
               tenant_id TEXT NOT NULL, acl TEXT NOT NULL, connector TEXT NOT NULL,
               external_id TEXT NOT NULL, generation INTEGER NOT NULL,
               artifact_id TEXT NOT NULL, version TEXT NOT NULL,
               content_sha256 TEXT NOT NULL, kind TEXT NOT NULL,
               status TEXT NOT NULL, last_failure_code TEXT,
               staged_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
               PRIMARY KEY(tenant_id,acl,connector,external_id,generation)
             );
             INSERT INTO local_connector_lifecycle_events
               (tenant_id,acl,connector,external_id,generation,artifact_id,version,
                content_sha256,kind,status,last_failure_code,staged_at,updated_at)
             SELECT tenant_id,acl,connector,external_id,generation,artifact_id,version,
                    content_sha256,kind,status,last_failure_code,staged_at,updated_at
             FROM old_lifecycle_events;
             DROP TABLE old_lifecycle_events;
             COMMIT;",
        )
        .unwrap();
        assert!(matches!(
            causal.source_text(&scope, &artifact.id),
            Err(CausalStoreError::NotFound)
        ));
        let old_status: String = conn
            .query_row(
                "SELECT status FROM local_connector_lifecycle_events",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_status, "pending");
        drop(conn);

        assert_eq!(bridge.drain_once(10).unwrap().completed, 1);
        assert_eq!(
            bridge.drain_once(10).unwrap(),
            LifecycleDrainReport::default()
        );
        let conn = Connection::open(causal.path()).unwrap();
        let (status, attempts): (String, i64) = conn
            .query_row(
                "SELECT status,attempt_count FROM local_connector_lifecycle_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(attempts, 1);
        assert_eq!(bridge.tenant_health("tenant-a").unwrap().pending_count, 0);
    }

    #[test]
    fn lease_blocks_cascade_but_staged_event_denies_reads_then_retries() {
        let home = tempfile::tempdir().unwrap();
        let bridge = TrustedConnectorLifecycleBridge::for_home(home.path());
        let causal = CausalStore::new(home.path().join("memory.db"));
        let (scope, artifact) = source(&causal, "tenant-a", "ticket-1", "v1");
        assert_eq!(
            bridge
                .bind_local_source(&binding(&scope, &artifact, 1))
                .unwrap(),
            BindOutcome::Bound
        );
        let claim = causal
            .add_claim(
                &scope,
                "arrival_rate",
                "queue_backlog",
                0,
                3600,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        causal
            .add_evidence(
                &scope,
                &claim.id,
                &artifact.id,
                0,
                9,
                "synthetic",
                EvidenceStance::Supports,
                None,
                "test-extractor",
            )
            .unwrap();
        assert_eq!(
            causal.evidence_for_claim(&scope, &claim.id).unwrap()[0]
                .span
                .excerpt,
            "synthetic"
        );

        let decisions =
            DecisionStore::with_causal_store(home.path().join("decisions.db"), causal.clone());
        let decision_scope = DecisionScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let snapshot = DecisionSnapshot {
            id: "snapshot-1".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec![artifact.content_sha256.clone()],
            seed: 1,
            arrivals_by_day: vec![3],
            initial_backlog: vec![],
        };
        decisions.put_snapshot(&decision_scope, &snapshot).unwrap();
        decisions
            .bind_causal_artifact(&decision_scope, &snapshot.id, &artifact.id)
            .unwrap();

        let ccr = CcrStore::new(home.path().join("ccr/ccr.db"));
        let ccr_scope = CcrScope {
            tenant_id: scope.tenant_id.clone(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            source_acl: scope.acl.clone(),
        };
        let stored = ccr
            .put_bound(
                &ccr_scope,
                "mcp:search",
                "call-1",
                "synthetic ticket data tenant-a ticket-1 v1",
                &CcrSourceArtifact {
                    connector: "causal".into(),
                    artifact_id: artifact.id.clone(),
                    version: artifact.version.clone(),
                    acl_revision: "revision".into(),
                },
            )
            .unwrap();

        let acl_revision = format!(
            "immutable-acl-sha256:{:x}",
            Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.acl)),
        );
        let lease = causal
            .acquire_ccr_delivery_lease(
                &scope,
                &artifact.id,
                &artifact.version,
                &artifact.content_sha256,
                &acl_revision,
            )
            .unwrap();
        assert!(lease.still_valid());
        assert_eq!(
            bridge
                .stage_local_event(&event(&scope, 1, LifecycleKind::Quarantined))
                .unwrap(),
            StageOutcome::Staged
        );
        assert!(!lease.still_valid());
        assert!(matches!(
            causal.source_text(&scope, &artifact.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            causal.read_artifact_metadata(&scope, &artifact.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(
            causal.evidence_for_claim(&scope, &claim.id).unwrap()[0]
                .span
                .excerpt
                .is_empty()
        );
        let (_, replacement) = source(&causal, "tenant-a", "ticket-1", "v2");
        assert_eq!(
            bridge
                .bind_local_source(&binding(&scope, &replacement, 2))
                .unwrap(),
            BindOutcome::Bound
        );
        assert!(causal.source_text(&scope, &replacement.id).is_ok());
        assert!(matches!(
            causal.source_text(&scope, &artifact.id),
            Err(CausalStoreError::NotFound)
        ));

        let first = bridge.drain_once(10).unwrap();
        assert_eq!(first.blocked, 1);
        let health = bridge.tenant_health("tenant-a").unwrap();
        assert_eq!(health.pending_count, 1);
        assert_eq!(health.blocked_count, 1);
        assert_eq!(
            health.last_failure_code.as_deref(),
            Some("delivery_lease_active")
        );
        drop(lease);

        let second = bridge.drain_once(10).unwrap();
        assert_eq!(second.completed, 1);
        assert_eq!(bridge.tenant_health("tenant-a").unwrap().pending_count, 0);
        let conn = Connection::open(causal.path()).unwrap();
        let (retained_content, retained_excerpt): (String, String) = conn
            .query_row(
                "SELECT a.content,e.excerpt FROM causal_artifacts a
                 JOIN causal_evidence e ON e.artifact_id=a.id WHERE a.id=?1",
                [&artifact.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(retained_content.is_empty());
        assert!(retained_excerpt.is_empty());
        assert!(matches!(
            decisions.get::<DecisionSnapshot>(&decision_scope, "snapshot", &snapshot.id),
            Err(DecisionStoreError::Revoked)
        ));
        let conn = Connection::open(ccr.path()).unwrap();
        let retained: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ccr_entries WHERE id=?1)",
                [&stored.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!retained);
        assert!(matches!(
            bridge.stage_local_event(&event(&scope, 1, LifecycleKind::VersionChanged)),
            Err(LifecycleError::Stale)
        ));
    }

    #[test]
    fn generation_replay_and_cross_tenant_event_cannot_revoke_current_source() {
        let home = tempfile::tempdir().unwrap();
        let bridge = TrustedConnectorLifecycleBridge::for_home(home.path());
        let causal = CausalStore::new(home.path().join("memory.db"));
        let (scope_a, old_a) = source(&causal, "tenant-a", "ticket-1", "v1");
        let (scope_b, b) = source(&causal, "tenant-b", "ticket-1", "v1");
        bridge
            .bind_local_source(&binding(&scope_a, &old_a, 1))
            .unwrap();
        bridge.bind_local_source(&binding(&scope_b, &b, 1)).unwrap();
        assert!(matches!(
            bridge.stage_local_event(&event(&scope_b, 2, LifecycleKind::Deleted)),
            Err(LifecycleError::NotFound)
        ));
        bridge
            .stage_local_event(&event(&scope_a, 1, LifecycleKind::VersionChanged))
            .unwrap();
        assert_eq!(bridge.drain_once(10).unwrap().completed, 1);
        assert!(causal.source_text(&scope_b, &b.id).is_ok());
        assert_eq!(bridge.tenant_health("tenant-b").unwrap().pending_count, 0);

        let (_, new_a) = source(&causal, "tenant-a", "ticket-1", "v2");
        assert_eq!(
            bridge
                .bind_local_source(&binding(&scope_a, &new_a, 2))
                .unwrap(),
            BindOutcome::Bound
        );
        assert!(matches!(
            bridge.stage_local_event(&event(&scope_a, 1, LifecycleKind::VersionChanged)),
            Err(LifecycleError::Stale)
        ));
        assert_eq!(
            bridge
                .stage_local_event(&event(&scope_a, 1, LifecycleKind::Deleted))
                .unwrap(),
            StageOutcome::Staged
        );
        assert_eq!(bridge.drain_once(10).unwrap().completed, 1);
        let retained_old_content: String = Connection::open(causal.path())
            .unwrap()
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&old_a.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(retained_old_content.is_empty());
        assert_eq!(
            bridge
                .stage_local_event(&event(&scope_a, 1, LifecycleKind::AclLost))
                .unwrap(),
            StageOutcome::AlreadyCompleted
        );
        assert!(causal.source_text(&scope_a, &new_a.id).is_ok());
        assert!(causal.source_text(&scope_b, &b.id).is_ok());
        assert!(matches!(
            bridge.bind_local_source(&binding(&scope_a, &old_a, 1)),
            Err(LifecycleError::Causal(CausalStoreError::NotFound))
        ));
    }

    #[test]
    fn acl_loss_erases_original_source_bytes() {
        let home = tempfile::tempdir().unwrap();
        let bridge = TrustedConnectorLifecycleBridge::for_home(home.path());
        let causal = CausalStore::new(home.path().join("memory.db"));
        let (scope, artifact) = source(&causal, "tenant-a", "ticket-1", "v1");
        bridge
            .bind_local_source(&binding(&scope, &artifact, 1))
            .unwrap();
        assert_eq!(
            bridge
                .stage_local_event(&event(&scope, 1, LifecycleKind::AclLost))
                .unwrap(),
            StageOutcome::Staged
        );
        assert_eq!(bridge.drain_once(10).unwrap().completed, 1);
        let retained_content: String = Connection::open(causal.path())
            .unwrap()
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&artifact.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(retained_content.is_empty());
    }

    #[test]
    fn late_terminal_event_erases_words_after_version_change_completion() {
        for terminal in [LifecycleKind::AclLost, LifecycleKind::Quarantined] {
            let home = tempfile::tempdir().unwrap();
            let bridge = TrustedConnectorLifecycleBridge::for_home(home.path());
            let causal = CausalStore::new(home.path().join("memory.db"));
            let (scope, artifact) = source(&causal, "tenant-a", "ticket-1", "v1");
            bridge
                .bind_local_source(&binding(&scope, &artifact, 1))
                .unwrap();
            bridge
                .stage_local_event(&event(&scope, 1, LifecycleKind::VersionChanged))
                .unwrap();
            assert_eq!(bridge.drain_once(10).unwrap().completed, 1);
            let stored_before: String = Connection::open(causal.path())
                .unwrap()
                .query_row(
                    "SELECT content FROM causal_artifacts WHERE id=?1",
                    [&artifact.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!stored_before.is_empty());

            assert_eq!(
                bridge
                    .stage_local_event(&event(&scope, 1, terminal))
                    .unwrap(),
                StageOutcome::Staged
            );
            assert_eq!(bridge.drain_once(10).unwrap().completed, 1);
            let stored_after: String = Connection::open(causal.path())
                .unwrap()
                .query_row(
                    "SELECT content FROM causal_artifacts WHERE id=?1",
                    [&artifact.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(stored_after.is_empty());
            assert_eq!(
                bridge
                    .stage_local_event(&event(&scope, 1, terminal))
                    .unwrap(),
                StageOutcome::AlreadyCompleted
            );
        }
    }
}
