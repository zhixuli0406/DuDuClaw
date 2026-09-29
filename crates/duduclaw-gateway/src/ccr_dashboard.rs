//! Read-only, tenant-scoped CCR operator projection.
//!
//! The CCR store holds post-redaction originals, caller identities, source
//! routes, and opaque handles. None of those values leave this projection.
//! Counts describe retained rows only: normal revocation and expiry remove
//! originals, so their historical entry counts cannot be reconstructed here.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::Serialize;

const RETRIEVAL_AUDIT_MAX_ROWS: u64 = 10_000;
const LOOP_TELEMETRY_MAX_ROWS: u64 = duduclaw_llm::CCR_LOOP_TELEMETRY_MAX_ROWS as u64;
const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const CCR_ENTRY_VERSION: i64 = 1;
const MAX_PROJECTION_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PROJECTION_ENTRIES: i64 = 100_000;

#[derive(Debug, Clone)]
pub struct CcrDashboardStore {
    path: PathBuf,
    memory_db_path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CcrDashboardError {
    #[error("a nonempty tenant selector is required")]
    InvalidScope,
    #[error("CCR dashboard store is unavailable")]
    Unavailable,
    #[error("CCR dashboard store has an invalid state")]
    InvalidState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CcrDashboardSnapshot {
    /// `absent` means no database file exists; no file is created by a read.
    pub status: &'static str,
    pub entries: CcrDashboardEntries,
    pub revocations: CcrDashboardRevocations,
    pub retrieval_audit: CcrDashboardRetrievalAudit,
    pub compression_metrics: CcrDashboardMetricAvailability,
    pub causal_revocation_outbox: CcrDashboardOutbox,
    pub causal_delivery: CcrDashboardDelivery,
    pub connector_lifecycle: CcrDashboardConnectorLifecycle,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CcrDashboardConnectorLifecycle {
    pub available: bool,
    pub reason: Option<&'static str>,
    pub pending_events: u64,
    pub blocked_events: u64,
    pub completed_events: u64,
    pub oldest_pending_seconds: Option<u64>,
    pub latest_completed_seconds: Option<u64>,
    pub last_failure_code: Option<String>,
}

impl CcrDashboardConnectorLifecycle {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            reason: Some(reason),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CcrDashboardDelivery {
    pub available: bool,
    pub reason: Option<&'static str>,
    pub active_leases: u64,
    pub oldest_lease_seconds: Option<u64>,
    /// Fences whose exact source version still awaits invalidation/erasure.
    pub revoking_sources: u64,
    pub oldest_revoking_seconds: Option<u64>,
}

impl CcrDashboardDelivery {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            reason: Some(reason),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CcrDashboardOutbox {
    /// False means the source DB or outbox schema could not be observed. Zero
    /// pending notices is meaningful only when this field is true.
    pub available: bool,
    pub reason: Option<&'static str>,
    pub pending_notices: u64,
    pub oldest_pending_seconds: Option<u64>,
}

impl CcrDashboardOutbox {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            reason: Some(reason),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CcrDashboardEntries {
    /// Unexpired, unrevoked retained rows. CCR checks the content digest at
    /// retrieval; this bounded metadata projection does not read originals.
    pub eligible_retained: u64,
    /// Rows left in the DB after their expiry; normally pruned on store open.
    pub expired_retained: u64,
    /// Unexpired retained rows blocked by a revocation tombstone.
    pub revoked_retained: u64,
    /// Sum of CCR's declared lengths, not freshly verified content bytes.
    pub declared_original_bytes_eligible: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CcrDashboardRevocations {
    /// Tombstone counts, not historical numbers of deleted originals.
    pub source_calls: u64,
    pub scopes: u64,
    pub artifact_versions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CcrDashboardRetrievalAudit {
    /// Only records still in the globally bounded audit window are counted.
    pub grants: u64,
    pub refusals: u64,
    pub returned_bytes_granted: u64,
    pub window_max_rows: u64,
}

impl Default for CcrDashboardRetrievalAudit {
    fn default() -> Self {
        Self {
            grants: 0,
            refusals: 0,
            returned_bytes_granted: 0,
            window_max_rows: RETRIEVAL_AUDIT_MAX_ROWS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CcrDashboardMetricAvailability {
    /// A legacy database may have no telemetry table yet.
    pub available: bool,
    pub reason: Option<&'static str>,
    /// Newest rows across all tenants, rather than a lifetime total.
    pub window_max_rows: u64,
    pub observed_loops: u64,
    pub provider_rounds: u64,
    pub usage_reported_rounds: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub ccr_compressed_results: u64,
    pub ccr_original_bytes: u64,
    pub ccr_delivered_bytes: u64,
    pub ccr_find_attempts: u64,
    pub ccr_find_hits: u64,
    pub ccr_find_misses: u64,
    /// `duduclaw_ccr_find` calls the per-loop budget refused. Reads `0` on a
    /// store written before CCR schema version 2 — the column is added by the
    /// CCR store's own migration, and this projection is read-only.
    pub ccr_find_rate_limited: u64,
    pub ccr_retrieve_attempts: u64,
    pub ccr_retrieve_successes: u64,
    pub ccr_retrieve_misses: u64,
    pub ccr_retrieved_bytes: u64,
    pub p95_elapsed_millis: Option<u64>,
}

impl Default for CcrDashboardMetricAvailability {
    fn default() -> Self {
        Self {
            available: false,
            reason: Some("not_persisted"),
            window_max_rows: LOOP_TELEMETRY_MAX_ROWS,
            observed_loops: 0,
            provider_rounds: 0,
            usage_reported_rounds: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            ccr_compressed_results: 0,
            ccr_original_bytes: 0,
            ccr_delivered_bytes: 0,
            ccr_find_attempts: 0,
            ccr_find_hits: 0,
            ccr_find_misses: 0,
            ccr_find_rate_limited: 0,
            ccr_retrieve_attempts: 0,
            ccr_retrieve_successes: 0,
            ccr_retrieve_misses: 0,
            ccr_retrieved_bytes: 0,
            p95_elapsed_millis: None,
        }
    }
}

impl CcrDashboardSnapshot {
    fn absent(
        causal_revocation_outbox: CcrDashboardOutbox,
        causal_delivery: CcrDashboardDelivery,
        connector_lifecycle: CcrDashboardConnectorLifecycle,
    ) -> Self {
        Self {
            status: "absent",
            entries: CcrDashboardEntries::default(),
            revocations: CcrDashboardRevocations::default(),
            retrieval_audit: CcrDashboardRetrievalAudit::default(),
            compression_metrics: CcrDashboardMetricAvailability::default(),
            causal_revocation_outbox,
            causal_delivery,
            connector_lifecycle,
        }
    }
}

impl CcrDashboardStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            memory_db_path: None,
        }
    }

    pub fn from_home(home: &Path) -> Self {
        Self {
            path: home.join("ccr").join("ccr.db"),
            memory_db_path: Some(home.join("memory.db")),
        }
    }

    /// Return aggregate metadata for one exact tenant without opening CCR's
    /// mutating store. A read never initializes tables or prunes expired rows.
    pub fn snapshot(&self, tenant_id: &str) -> Result<CcrDashboardSnapshot, CcrDashboardError> {
        if tenant_id.trim().is_empty() {
            return Err(CcrDashboardError::InvalidScope);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CcrDashboardError::Unavailable)?
            .as_secs() as i64;
        let causal_revocation_outbox = self.read_causal_outbox(tenant_id, now);
        let causal_delivery = self.read_causal_delivery(tenant_id, now);
        let connector_lifecycle = self.read_connector_lifecycle(tenant_id);
        let metadata = match std::fs::metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CcrDashboardSnapshot::absent(
                    causal_revocation_outbox,
                    causal_delivery,
                    connector_lifecycle,
                ));
            }
            Err(_) => return Err(CcrDashboardError::Unavailable),
            Ok(metadata) if !metadata.is_file() => return Err(CcrDashboardError::InvalidState),
            Ok(metadata) => metadata,
        };
        // Normal CCR writes cap original content at 64 MiB. This guard also
        // bounds unusual stores with many tiny entries or old tombstones.
        let mut projected_bytes = metadata.len();
        let mut wal_name = self.path.as_os_str().to_os_string();
        wal_name.push("-wal");
        let wal_path = PathBuf::from(wal_name);
        match std::fs::metadata(wal_path) {
            Ok(wal) => projected_bytes = projected_bytes.saturating_add(wal.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(CcrDashboardError::Unavailable),
        }
        if projected_bytes > MAX_PROJECTION_FILE_BYTES {
            return Err(CcrDashboardError::Unavailable);
        }

        let mut conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sqlite_error)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(sqlite_error)?;
        // One SQLite snapshot keeps entry, tombstone, and audit counts aligned
        // while the CCR writer is committing or scrubbing rows concurrently.
        let tx = conn.transaction().map_err(sqlite_error)?;
        let page_count: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(sqlite_error)?;
        let page_size: i64 = tx
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(sqlite_error)?;
        let logical_bytes = page_count
            .checked_mul(page_size)
            .ok_or(CcrDashboardError::InvalidState)?;
        if logical_bytes < 0 {
            return Err(CcrDashboardError::InvalidState);
        }
        if logical_bytes as u64 > MAX_PROJECTION_FILE_BYTES {
            return Err(CcrDashboardError::Unavailable);
        }
        verify_database_shape(&tx)?;
        let entries = read_entries(&tx, tenant_id, now)?;
        let revocations = read_revocations(&tx, tenant_id)?;
        let retrieval_audit = read_retrieval_audit(&tx, tenant_id)?;
        let compression_metrics = read_compression_metrics(&tx, tenant_id)?;
        tx.commit().map_err(sqlite_error)?;
        Ok(CcrDashboardSnapshot {
            status: "ready",
            entries,
            revocations,
            retrieval_audit,
            compression_metrics,
            causal_revocation_outbox,
            causal_delivery,
            connector_lifecycle,
        })
    }

    fn read_connector_lifecycle(&self, tenant_id: &str) -> CcrDashboardConnectorLifecycle {
        use crate::connector_lifecycle::{LifecycleHealthStatus, TrustedConnectorLifecycleBridge};

        let Some(path) = self.memory_db_path.as_ref() else {
            return CcrDashboardConnectorLifecycle::unavailable("not_configured");
        };
        match std::fs::metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return CcrDashboardConnectorLifecycle::unavailable("source_db_absent");
            }
            Ok(metadata) if metadata.is_file() => {}
            _ => return CcrDashboardConnectorLifecycle::unavailable("source_db_unavailable"),
        }
        let Some(home) = path.parent() else {
            return CcrDashboardConnectorLifecycle::unavailable("source_db_unavailable");
        };
        match TrustedConnectorLifecycleBridge::for_home(home).tenant_health(tenant_id) {
            Ok(health) if health.status == LifecycleHealthStatus::Available => {
                if health.blocked_count > health.pending_count
                    || health.pending_count > MAX_SAFE_JSON_INTEGER
                    || health.blocked_count > MAX_SAFE_JSON_INTEGER
                    || health.completed_count > MAX_SAFE_JSON_INTEGER
                    || health
                        .oldest_pending_age_seconds
                        .is_some_and(|age| age > MAX_SAFE_JSON_INTEGER)
                    || health
                        .latest_completed_age_seconds
                        .is_some_and(|age| age > MAX_SAFE_JSON_INTEGER)
                {
                    return CcrDashboardConnectorLifecycle::unavailable("source_db_unavailable");
                }
                let last_failure_code = match health.last_failure_code.as_deref() {
                    Some("delivery_lease_active") => Some("delivery_lease_active".into()),
                    Some("dependent_store_unavailable") => {
                        Some("dependent_store_unavailable".into())
                    }
                    Some("identity_mismatch") => Some("identity_mismatch".into()),
                    _ => None,
                };
                CcrDashboardConnectorLifecycle {
                    available: true,
                    reason: None,
                    pending_events: health.pending_count,
                    blocked_events: health.blocked_count,
                    completed_events: health.completed_count,
                    oldest_pending_seconds: health.oldest_pending_age_seconds,
                    latest_completed_seconds: health.latest_completed_age_seconds,
                    last_failure_code,
                }
            }
            Ok(health) if health.status == LifecycleHealthStatus::MissingSchema => {
                CcrDashboardConnectorLifecycle::unavailable("legacy_schema")
            }
            _ => CcrDashboardConnectorLifecycle::unavailable("source_db_unavailable"),
        }
    }

    /// Independent read-only source snapshot. An unavailable source projection
    /// must never be rendered as an empty delivery queue.
    fn read_causal_outbox(&self, tenant_id: &str, now: i64) -> CcrDashboardOutbox {
        let Some(path) = self.memory_db_path.as_ref() else {
            return CcrDashboardOutbox::unavailable("not_configured");
        };
        match std::fs::metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return CcrDashboardOutbox::unavailable("source_db_absent");
            }
            Ok(metadata) if metadata.is_file() => {}
            _ => return CcrDashboardOutbox::unavailable("source_db_unavailable"),
        }
        let read = || -> Result<CcrDashboardOutbox, rusqlite::Error> {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.busy_timeout(std::time::Duration::from_millis(250))?;
            let table_kind: Option<String> = conn
                .query_row(
                    "SELECT type FROM sqlite_master WHERE name='causal_ccr_revocation_outbox'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            if table_kind.as_deref() != Some("table") {
                return Ok(CcrDashboardOutbox::unavailable("legacy_schema"));
            }
            // Tenant is the leading primary-key column. No artifact IDs,
            // versions, ACLs, or queue rows cross the API boundary.
            let (count, oldest): (i64, Option<i64>) = conn.query_row(
                "SELECT COUNT(*),MIN(queued_at) FROM causal_ccr_revocation_outbox
                 WHERE tenant_id=?1 AND connector='causal' AND delivered_at IS NULL",
                [tenant_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let pending_notices = u64::try_from(count)
                .ok()
                .filter(|n| *n <= MAX_SAFE_JSON_INTEGER)
                .ok_or(rusqlite::Error::InvalidQuery)?;
            let oldest_pending_seconds = match (pending_notices, oldest) {
                (0, None) => None,
                (n, Some(queued)) if n > 0 && queued >= 0 => {
                    Some(now.saturating_sub(queued).max(0) as u64)
                }
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(CcrDashboardOutbox {
                available: true,
                reason: None,
                pending_notices,
                oldest_pending_seconds,
            })
        };
        read().unwrap_or_else(|_| CcrDashboardOutbox::unavailable("source_db_unavailable"))
    }

    /// Independent exact-tenant projection of durable disclosure leases and
    /// source versions fenced for revocation. No source identity is returned.
    fn read_causal_delivery(&self, tenant_id: &str, now: i64) -> CcrDashboardDelivery {
        let Some(path) = self.memory_db_path.as_ref() else {
            return CcrDashboardDelivery::unavailable("not_configured");
        };
        match std::fs::metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return CcrDashboardDelivery::unavailable("source_db_absent");
            }
            Ok(metadata) if metadata.is_file() => {}
            _ => return CcrDashboardDelivery::unavailable("source_db_unavailable"),
        }
        let read = || -> Result<CcrDashboardDelivery, rusqlite::Error> {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.busy_timeout(std::time::Duration::from_millis(250))?;
            for name in ["causal_ccr_delivery_leases", "causal_ccr_revoking"] {
                let kind: Option<String> = conn
                    .query_row(
                        "SELECT type FROM sqlite_master WHERE name=?1",
                        [name],
                        |row| row.get(0),
                    )
                    .optional()?;
                if kind.as_deref() != Some("table") {
                    return Ok(CcrDashboardDelivery::unavailable("legacy_schema"));
                }
            }
            let (leases, oldest_lease): (i64, Option<i64>) = conn.query_row(
                "SELECT COUNT(*),MIN(created_at) FROM causal_ccr_delivery_leases WHERE tenant_id=?1",
                [tenant_id], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let (revoking, oldest_revoking): (i64, Option<i64>) = conn.query_row(
                "SELECT COUNT(*),MIN(r.started_at) FROM causal_ccr_revoking r
                 JOIN causal_artifacts a ON a.id=r.artifact_id
                  AND a.tenant_id=r.tenant_id AND a.acl=r.acl AND a.version=r.version
                 WHERE r.tenant_id=?1 AND a.invalidated_at IS NULL AND a.content<>''",
                [tenant_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let age = |count: i64,
                       oldest: Option<i64>|
             -> Result<(u64, Option<u64>), rusqlite::Error> {
                let count = u64::try_from(count)
                    .ok()
                    .filter(|n| *n <= MAX_SAFE_JSON_INTEGER)
                    .ok_or(rusqlite::Error::InvalidQuery)?;
                let seconds = match (count, oldest) {
                    (0, None) => None,
                    (n, Some(t)) if n > 0 && t >= 0 => Some(now.saturating_sub(t).max(0) as u64),
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok((count, seconds))
            };
            let (active_leases, oldest_lease_seconds) = age(leases, oldest_lease)?;
            let (revoking_sources, oldest_revoking_seconds) = age(revoking, oldest_revoking)?;
            Ok(CcrDashboardDelivery {
                available: true,
                reason: None,
                active_leases,
                oldest_lease_seconds,
                revoking_sources,
                oldest_revoking_seconds,
            })
        };
        read().unwrap_or_else(|_| CcrDashboardDelivery::unavailable("source_db_unavailable"))
    }
}

fn sqlite_error(error: rusqlite::Error) -> CcrDashboardError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::DatabaseBusy =>
        {
            CcrDashboardError::Unavailable
        }
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::DatabaseLocked =>
        {
            CcrDashboardError::Unavailable
        }
        _ => CcrDashboardError::InvalidState,
    }
}

fn verify_database_shape(tx: &Transaction<'_>) -> Result<(), CcrDashboardError> {
    // A malformed file or missing table/column fails closed. Full PRAGMA
    // integrity_check would scan every original on every dashboard refresh.
    for sql in [
        "SELECT tenant_id,content_bytes,transform_version,created_at,expires_at FROM ccr_entries LIMIT 0",
        "SELECT tenant_id,agent_id,session_id,source_acl FROM ccr_revoked_scopes LIMIT 0",
        "SELECT tenant_id,agent_id,session_id,source_acl,source_tool,source_call_id FROM ccr_revoked_sources LIMIT 0",
        "SELECT entry_id,tenant_id,connector,artifact_id,version FROM ccr_artifact_bindings LIMIT 0",
        "SELECT tenant_id,connector,artifact_id,version FROM ccr_revoked_artifact_versions LIMIT 0",
        "SELECT tenant_id,status,returned_bytes FROM ccr_retrieval_audit LIMIT 0",
    ] {
        tx.prepare(sql).map_err(sqlite_error)?;
    }
    Ok(())
}

fn read_entries(
    tx: &Transaction<'_>,
    tenant_id: &str,
    now: i64,
) -> Result<CcrDashboardEntries, CcrDashboardError> {
    let sampled: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM ccr_entries LIMIT ?1)",
            [MAX_PROJECTION_ENTRIES + 1],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if sampled > MAX_PROJECTION_ENTRIES {
        return Err(CcrDashboardError::Unavailable);
    }
    let (invalid, eligible, expired, revoked, declared_bytes): (i64, i64, i64, i64, i64) = tx
        .query_row(
            "WITH classified AS (
                SELECT content_bytes,transform_version,created_at,expires_at,
                  (EXISTS(SELECT 1 FROM ccr_revoked_scopes r WHERE
                      r.tenant_id=e.tenant_id AND r.agent_id=e.agent_id
                      AND r.session_id=e.session_id AND r.source_acl=e.source_acl)
                   OR EXISTS(SELECT 1 FROM ccr_revoked_sources r WHERE
                      r.tenant_id=e.tenant_id AND r.agent_id=e.agent_id
                      AND r.session_id=e.session_id AND r.source_acl=e.source_acl
                      AND r.source_tool=e.source_tool AND r.source_call_id=e.source_call_id)
                   OR EXISTS(SELECT 1 FROM ccr_artifact_bindings b
                      JOIN ccr_revoked_artifact_versions r ON r.tenant_id=b.tenant_id
                       AND r.connector=b.connector AND r.artifact_id=b.artifact_id
                       AND r.version=b.version WHERE b.entry_id=e.id)) AS revoked
                FROM ccr_entries e WHERE e.tenant_id=?1
             )
             SELECT
               COALESCE(SUM(CASE WHEN typeof(content_bytes)!='integer'
                      OR typeof(transform_version)!='integer'
                      OR typeof(created_at)!='integer'
                      OR typeof(expires_at)!='integer'
                      OR content_bytes<0 OR transform_version!=?3
                      OR expires_at<=created_at THEN 1 ELSE 0 END),0),
               COALESCE(SUM(CASE WHEN expires_at>?2 AND NOT revoked THEN 1 ELSE 0 END),0),
               COALESCE(SUM(CASE WHEN expires_at<=?2 THEN 1 ELSE 0 END),0),
               COALESCE(SUM(CASE WHEN expires_at>?2 AND revoked THEN 1 ELSE 0 END),0),
               COALESCE(SUM(CASE WHEN expires_at>?2 AND NOT revoked THEN content_bytes ELSE 0 END),0)
             FROM classified",
            params![tenant_id, now, CCR_ENTRY_VERSION],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .map_err(sqlite_error)?;
    if invalid != 0 {
        return Err(CcrDashboardError::InvalidState);
    }
    Ok(CcrDashboardEntries {
        eligible_retained: u64::try_from(eligible).map_err(|_| CcrDashboardError::InvalidState)?,
        expired_retained: u64::try_from(expired).map_err(|_| CcrDashboardError::InvalidState)?,
        revoked_retained: u64::try_from(revoked).map_err(|_| CcrDashboardError::InvalidState)?,
        declared_original_bytes_eligible: u64::try_from(declared_bytes)
            .map_err(|_| CcrDashboardError::InvalidState)?,
    })
}

fn count_tenant_rows(
    tx: &Transaction<'_>,
    table: &str,
    tenant_id: &str,
) -> Result<u64, CcrDashboardError> {
    // `table` is selected only from three internal constants below.
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE tenant_id=?1");
    let count: i64 = tx
        .query_row(&sql, [tenant_id], |row| row.get(0))
        .map_err(sqlite_error)?;
    u64::try_from(count).map_err(|_| CcrDashboardError::InvalidState)
}

fn read_revocations(
    tx: &Transaction<'_>,
    tenant_id: &str,
) -> Result<CcrDashboardRevocations, CcrDashboardError> {
    Ok(CcrDashboardRevocations {
        source_calls: count_tenant_rows(tx, "ccr_revoked_sources", tenant_id)?,
        scopes: count_tenant_rows(tx, "ccr_revoked_scopes", tenant_id)?,
        artifact_versions: count_tenant_rows(tx, "ccr_revoked_artifact_versions", tenant_id)?,
    })
}

fn read_retrieval_audit(
    tx: &Transaction<'_>,
    tenant_id: &str,
) -> Result<CcrDashboardRetrievalAudit, CcrDashboardError> {
    let global_rows: i64 = tx
        .query_row("SELECT COUNT(*) FROM ccr_retrieval_audit", [], |row| {
            row.get(0)
        })
        .map_err(sqlite_error)?;
    if !(0..=RETRIEVAL_AUDIT_MAX_ROWS as i64).contains(&global_rows) {
        return Err(CcrDashboardError::InvalidState);
    }
    let mut stmt = tx
        .prepare("SELECT status,returned_bytes FROM ccr_retrieval_audit WHERE tenant_id=?1")
        .map_err(sqlite_error)?;
    let mut rows = stmt.query(params![tenant_id]).map_err(sqlite_error)?;
    let mut counts = CcrDashboardRetrievalAudit::default();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let status: String = row.get(0).map_err(sqlite_error)?;
        let bytes: i64 = row.get(1).map_err(sqlite_error)?;
        if bytes < 0 {
            return Err(CcrDashboardError::InvalidState);
        }
        match status.as_str() {
            "granted" => {
                counts.grants += 1;
                counts.returned_bytes_granted = counts
                    .returned_bytes_granted
                    .checked_add(bytes as u64)
                    .ok_or(CcrDashboardError::InvalidState)?;
            }
            "refused" if bytes == 0 => counts.refusals += 1,
            _ => return Err(CcrDashboardError::InvalidState),
        }
    }
    Ok(counts)
}

fn read_compression_metrics(
    tx: &Transaction<'_>,
    tenant_id: &str,
) -> Result<CcrDashboardMetricAvailability, CcrDashboardError> {
    let table_kind: Option<String> = tx
        .query_row(
            "SELECT type FROM sqlite_master WHERE name='ccr_loop_telemetry'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    match table_kind.as_deref() {
        None => return Ok(CcrDashboardMetricAvailability::default()),
        Some("table") => {}
        Some(_) => return Err(CcrDashboardError::InvalidState),
    }
    let global_rows: i64 = tx
        .query_row("SELECT COUNT(*) FROM ccr_loop_telemetry", [], |row| {
            row.get(0)
        })
        .map_err(sqlite_error)?;
    if !(0..=LOOP_TELEMETRY_MAX_ROWS as i64).contains(&global_rows) {
        return Err(CcrDashboardError::InvalidState);
    }
    // This projection is READ-ONLY and never migrates. A store still on CCR
    // schema version 1 has no `ccr_find_rate_limited` column, and failing the
    // whole snapshot over one counter would be a worse answer than an honest
    // zero — the CCR store adds the column the next time it opens for write.
    let telemetry_columns = tx
        .prepare("PRAGMA table_info(ccr_loop_telemetry)")
        .map_err(sqlite_error)?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    let rate_limited_column = if telemetry_columns
        .iter()
        .any(|column| column == "ccr_find_rate_limited")
    {
        "ccr_find_rate_limited"
    } else {
        "0"
    };
    let mut stmt = tx
        .prepare(&format!(
            "SELECT provider_rounds,usage_reported_rounds,input_tokens,output_tokens,
                    cache_read_tokens,cache_write_tokens,reasoning_tokens,
                    ccr_compressed_results,ccr_original_bytes,ccr_delivered_bytes,
                    ccr_find_attempts,ccr_find_hits,ccr_find_misses,
                    ccr_retrieve_attempts,ccr_retrieve_successes,ccr_retrieve_misses,
                    ccr_retrieved_bytes,{rate_limited_column},elapsed_millis,observed_at
             FROM ccr_loop_telemetry WHERE tenant_id=?1"
        ))
        .map_err(sqlite_error)?;
    let mut rows = stmt.query([tenant_id]).map_err(sqlite_error)?;
    let mut totals = [0_u64; 19];
    let mut latencies = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let fields = (0..20)
            .map(|index| row.get::<_, i64>(index).map_err(sqlite_error))
            .collect::<Result<Vec<_>, _>>()?;
        if fields.iter().any(|value| *value < 0)
            || fields[0] == 0
            || fields[1] > fields[0]
            || fields[11].checked_add(fields[12]) != Some(fields[10])
            || fields[14].checked_add(fields[15]) != Some(fields[13])
            || fields[9] > fields[8]
        {
            return Err(CcrDashboardError::InvalidState);
        }
        for (sum, value) in totals.iter_mut().zip(fields.iter().take(19)) {
            *sum = sum
                .checked_add(*value as u64)
                .filter(|sum| *sum <= MAX_SAFE_JSON_INTEGER)
                .ok_or(CcrDashboardError::InvalidState)?;
        }
        latencies.push(fields[18] as u64);
    }
    latencies.sort_unstable();
    let p95 = (!latencies.is_empty()).then(|| latencies[(95 * latencies.len()).div_ceil(100) - 1]);
    Ok(CcrDashboardMetricAvailability {
        available: true,
        reason: None,
        window_max_rows: LOOP_TELEMETRY_MAX_ROWS,
        observed_loops: latencies.len() as u64,
        provider_rounds: totals[0],
        usage_reported_rounds: totals[1],
        input_tokens: totals[2],
        output_tokens: totals[3],
        cache_read_tokens: totals[4],
        cache_write_tokens: totals[5],
        reasoning_tokens: totals[6],
        ccr_compressed_results: totals[7],
        ccr_original_bytes: totals[8],
        ccr_delivered_bytes: totals[9],
        ccr_find_attempts: totals[10],
        ccr_find_hits: totals[11],
        ccr_find_misses: totals[12],
        ccr_retrieve_attempts: totals[13],
        ccr_retrieve_successes: totals[14],
        ccr_retrieve_misses: totals[15],
        ccr_retrieved_bytes: totals[16],
        ccr_find_rate_limited: totals[17],
        p95_elapsed_millis: p95,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_llm::{CcrScope, CcrSourceArtifact, CcrStore, NormalizedUsage, ToolLoopTelemetry};

    fn scope(tenant: &str) -> CcrScope {
        CcrScope {
            tenant_id: tenant.to_owned(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            source_acl: "principal-secret".into(),
        }
    }

    #[test]
    fn absent_store_is_zero_and_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let dashboard = CcrDashboardStore::from_home(dir.path());
        let result = dashboard.snapshot("tenant").unwrap();
        assert_eq!(result.status, "absent");
        assert_eq!(result.entries.eligible_retained, 0);
        assert_eq!(result.retrieval_audit.window_max_rows, 10_000);
        assert!(!dir.path().join("ccr").exists());
        assert_eq!(
            dashboard.snapshot(" "),
            Err(CcrDashboardError::InvalidScope)
        );
    }

    #[test]
    fn counts_exact_tenant_without_content_or_principal_disclosure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccr.db");
        let store = CcrStore::new(&path);
        let a = scope("tenant-a");
        let b = scope("tenant-b");
        let live = store
            .put(&a, "server/tool", "live", "private-prompt-alpha")
            .unwrap();
        let expired = store
            .put(&a, "server/tool", "expired", "private-prompt-beta")
            .unwrap();
        let revoked = store
            .put(&a, "server/tool", "revoked", "private-prompt-gamma")
            .unwrap();
        let _other = store
            .put(&b, "server/tool", "other", "private-prompt-delta")
            .unwrap();
        store.retrieve(&a, &live.id, None, 0, 7).unwrap();
        assert!(store.retrieve(&a, "missing", None, 0, 7).is_err());
        store.retrieve(&b, &_other.id, None, 0, 7).unwrap();
        store
            .revoke_source_call(&a, "server/tool", "revoked")
            .unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE ccr_entries SET created_at=created_at-10, expires_at=created_at-9 WHERE id=?1",
            [&expired.id],
        )
        .unwrap();
        // Simulate an incomplete scrub to verify the retained revoked bucket.
        conn.execute(
            "INSERT INTO ccr_revoked_sources
             (tenant_id,agent_id,session_id,source_acl,source_tool,source_call_id,revoked_at)
             VALUES ('tenant-a','agent','session','principal-secret','server/tool','live',1)",
            [],
        )
        .unwrap();
        let result = CcrDashboardStore::new(&path).snapshot("tenant-a").unwrap();
        assert_eq!(result.entries.eligible_retained, 0);
        assert_eq!(result.entries.expired_retained, 1);
        assert_eq!(result.entries.revoked_retained, 1);
        assert_eq!(result.entries.declared_original_bytes_eligible, 0);
        assert_eq!(result.revocations.source_calls, 2);
        assert_eq!(result.retrieval_audit.grants, 1);
        assert_eq!(result.retrieval_audit.refusals, 1);
        assert_eq!(result.retrieval_audit.returned_bytes_granted, 7);
        let json = serde_json::to_string(&result).unwrap();
        for secret in [
            "private-prompt",
            "principal-secret",
            "server/tool",
            &live.id,
            &revoked.id,
        ] {
            assert!(!json.contains(secret));
        }
        let other = CcrDashboardStore::new(&path).snapshot("tenant-b").unwrap();
        assert_eq!(other.entries.eligible_retained, 1);
        assert_eq!(
            other.entries.declared_original_bytes_eligible,
            "private-prompt-delta".len() as u64
        );
    }

    #[test]
    fn rejects_invalid_database_and_invalid_retained_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccr.db");
        std::fs::write(&path, b"not sqlite").unwrap();
        assert_eq!(
            CcrDashboardStore::new(&path).snapshot("tenant"),
            Err(CcrDashboardError::InvalidState)
        );
        std::fs::remove_file(&path).unwrap();
        let store = CcrStore::new(&path);
        let a = scope("tenant");
        let entry = store
            .put(&a, "server/tool", "call", "integrity-secret")
            .unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE ccr_entries SET content_bytes=-1 WHERE id=?1",
            [&entry.id],
        )
        .unwrap();
        assert_eq!(
            CcrDashboardStore::new(&path).snapshot("tenant"),
            Err(CcrDashboardError::InvalidState)
        );
    }

    #[test]
    fn counts_bound_artifact_tombstones_without_leaking_artifact_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccr.db");
        let store = CcrStore::new(&path);
        let scope = scope("tenant");
        let artifact = CcrSourceArtifact {
            connector: "private-connector".into(),
            artifact_id: "private-artifact".into(),
            version: "v1".into(),
            acl_revision: "private-acl".into(),
        };
        store
            .put_bound(&scope, "server/tool", "call", "secret", &artifact)
            .unwrap();
        store
            .revoke_artifact_version(
                "tenant",
                &artifact.connector,
                &artifact.artifact_id,
                &artifact.version,
            )
            .unwrap();
        let report = CcrDashboardStore::new(&path).snapshot("tenant").unwrap();
        assert_eq!(report.entries.eligible_retained, 0);
        assert_eq!(report.revocations.artifact_versions, 1);
        let json = serde_json::to_string(&report).unwrap();
        for secret in [
            "private-connector",
            "private-artifact",
            "private-acl",
            "secret",
        ] {
            assert!(!json.contains(secret));
        }
    }

    #[test]
    fn refuses_oversized_store_before_scanning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccr.db");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PROJECTION_FILE_BYTES + 1).unwrap();
        assert_eq!(
            CcrDashboardStore::new(&path).snapshot("tenant"),
            Err(CcrDashboardError::Unavailable)
        );
    }

    #[test]
    fn projects_exact_tenant_loop_metrics_and_nearest_rank_p95() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccr.db");
        let store = CcrStore::new(&path);
        store
            .put(&scope("tenant-a"), "route", "legacy", "redacted original")
            .unwrap();
        let dashboard = CcrDashboardStore::new(&path);
        let legacy = dashboard.snapshot("tenant-a").unwrap();
        assert!(!legacy.compression_metrics.available);
        assert_eq!(legacy.compression_metrics.reason, Some("not_persisted"));
        for elapsed in 1..=20 {
            store
                .record_loop_telemetry(
                    &scope("tenant-a"),
                    &ToolLoopTelemetry {
                        provider_rounds: 2,
                        usage_reported_rounds: 1,
                        provider_usage: NormalizedUsage {
                            input_tokens: 3,
                            output_tokens: 1,
                            ..Default::default()
                        },
                        ccr_compressed_results: 1,
                        ccr_original_bytes: 1_000,
                        ccr_delivered_bytes: 200,
                        ccr_find_attempts: 1,
                        ccr_find_hits: 1,
                        ccr_retrieve_attempts: 1,
                        ccr_retrieve_successes: 1,
                        ccr_retrieved_bytes: 30,
                        elapsed_millis: elapsed,
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        store
            .record_loop_telemetry(
                &scope("tenant-b"),
                &ToolLoopTelemetry {
                    provider_rounds: 1,
                    elapsed_millis: 9_000,
                    ..Default::default()
                },
            )
            .unwrap();
        let result = dashboard.snapshot("tenant-a").unwrap();
        let metrics = &result.compression_metrics;
        assert!(metrics.available);
        assert_eq!(metrics.reason, None);
        assert_eq!(metrics.observed_loops, 20);
        assert_eq!(metrics.provider_rounds, 40);
        assert_eq!(metrics.usage_reported_rounds, 20);
        assert_eq!(metrics.input_tokens, 60);
        assert_eq!(metrics.ccr_original_bytes, 20_000);
        assert_eq!(metrics.ccr_delivered_bytes, 4_000);
        assert_eq!(metrics.ccr_retrieve_successes, 20);
        assert_eq!(metrics.p95_elapsed_millis, Some(19));
        let other = dashboard.snapshot("tenant-b").unwrap();
        assert_eq!(other.compression_metrics.observed_loops, 1);
        assert_eq!(other.compression_metrics.p95_elapsed_millis, Some(9_000));
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("redacted original"));
        assert!(!json.contains("principal-secret"));
        let conn = Connection::open(&path).unwrap();
        conn.execute("DELETE FROM ccr_loop_telemetry", []).unwrap();
        let empty = dashboard.snapshot("tenant-a").unwrap();
        assert!(empty.compression_metrics.available);
        assert_eq!(empty.compression_metrics.observed_loops, 0);
        assert_eq!(empty.compression_metrics.p95_elapsed_millis, None);
    }

    /// Regression (W3-2 #3): the per-loop `duduclaw_ccr_find` budget refusals
    /// are a persisted column now, so the tenant aggregate must carry them —
    /// and a store still on CCR schema version 1 must report an honest zero
    /// rather than failing the whole snapshot on a missing column.
    #[test]
    fn aggregates_find_rate_limit_refusals_and_tolerates_a_pre_migration_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccr.db");
        let store = CcrStore::new(&path);
        for refusals in [2_u64, 5] {
            store
                .record_loop_telemetry(
                    &scope("tenant-a"),
                    &ToolLoopTelemetry {
                        provider_rounds: 1,
                        ccr_find_attempts: 1,
                        ccr_find_misses: 1,
                        ccr_find_rate_limited: refusals,
                        elapsed_millis: 4,
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        let dashboard = CcrDashboardStore::new(&path);
        let metrics = dashboard.snapshot("tenant-a").unwrap().compression_metrics;
        assert!(metrics.available);
        assert_eq!(metrics.ccr_find_rate_limited, 7);
        assert_eq!(metrics.ccr_find_attempts, 2);

        // A read-only projection over a store the CCR writer has not yet
        // migrated: the same totals, the one new counter an honest zero. The
        // legacy file is written column-by-column rather than copied, so the
        // test does not depend on WAL checkpoint timing or `DROP COLUMN`.
        let legacy_path = dir.path().join("legacy.db");
        let legacy_store = CcrStore::new(&legacy_path);
        legacy_store
            .put(&scope("tenant-a"), "route", "seed", "redacted original")
            .unwrap();
        Connection::open(&legacy_path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE ccr_loop_telemetry (
                    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    tenant_id TEXT NOT NULL,
                    observed_at INTEGER NOT NULL,
                    provider_rounds INTEGER NOT NULL,
                    usage_reported_rounds INTEGER NOT NULL,
                    input_tokens INTEGER NOT NULL,
                    output_tokens INTEGER NOT NULL,
                    cache_read_tokens INTEGER NOT NULL,
                    cache_write_tokens INTEGER NOT NULL,
                    reasoning_tokens INTEGER NOT NULL,
                    ccr_compressed_results INTEGER NOT NULL,
                    ccr_original_bytes INTEGER NOT NULL,
                    ccr_delivered_bytes INTEGER NOT NULL,
                    ccr_find_attempts INTEGER NOT NULL,
                    ccr_find_hits INTEGER NOT NULL,
                    ccr_find_misses INTEGER NOT NULL,
                    ccr_retrieve_attempts INTEGER NOT NULL,
                    ccr_retrieve_successes INTEGER NOT NULL,
                    ccr_retrieve_misses INTEGER NOT NULL,
                    ccr_retrieved_bytes INTEGER NOT NULL,
                    elapsed_millis INTEGER NOT NULL
                );
                INSERT INTO ccr_loop_telemetry (
                    tenant_id,observed_at,provider_rounds,usage_reported_rounds,
                    input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,
                    reasoning_tokens,ccr_compressed_results,ccr_original_bytes,
                    ccr_delivered_bytes,ccr_find_attempts,ccr_find_hits,
                    ccr_find_misses,ccr_retrieve_attempts,ccr_retrieve_successes,
                    ccr_retrieve_misses,ccr_retrieved_bytes,elapsed_millis)
                VALUES ('tenant-a',1,1,0,0,0,0,0,0,0,0,0,2,0,2,0,0,0,0,4);
                PRAGMA user_version=1;",
            )
            .unwrap();
        let legacy = CcrDashboardStore::new(&legacy_path)
            .snapshot("tenant-a")
            .unwrap()
            .compression_metrics;
        assert!(legacy.available);
        assert_eq!(legacy.ccr_find_attempts, 2);
        assert_eq!(legacy.ccr_find_rate_limited, 0);
    }

    #[test]
    fn rejects_corrupt_loop_telemetry_instead_of_reporting_false_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ccr.db");
        let store = CcrStore::new(&path);
        store
            .record_loop_telemetry(
                &scope("tenant"),
                &ToolLoopTelemetry {
                    provider_rounds: 1,
                    elapsed_millis: 2,
                    ..Default::default()
                },
            )
            .unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE ccr_loop_telemetry SET usage_reported_rounds=2", [])
            .unwrap();
        assert_eq!(
            CcrDashboardStore::new(&path).snapshot("tenant"),
            Err(CcrDashboardError::InvalidState)
        );
    }

    #[test]
    fn causal_outbox_reports_exact_tenant_even_without_a_ccr_database() {
        let home = tempfile::tempdir().unwrap();
        let dashboard = CcrDashboardStore::from_home(home.path());
        let missing = dashboard.snapshot("tenant-a").unwrap();
        assert_eq!(missing.status, "absent");
        assert_eq!(
            missing.causal_revocation_outbox.reason,
            Some("source_db_absent")
        );
        assert!(!home.path().join("memory.db").exists());
        assert!(!home.path().join("ccr").exists());

        let source_path = home.path().join("memory.db");
        let conn = Connection::open(&source_path).unwrap();
        conn.execute_batch("CREATE TABLE legacy_memory (id INTEGER PRIMARY KEY)")
            .unwrap();
        let legacy = dashboard.snapshot("tenant-a").unwrap();
        assert_eq!(
            legacy.causal_revocation_outbox.reason,
            Some("legacy_schema")
        );
        assert!(!legacy.causal_revocation_outbox.available);

        conn.execute_batch(
            "CREATE TABLE causal_ccr_revocation_outbox (
                tenant_id TEXT NOT NULL, connector TEXT NOT NULL,
                artifact_id TEXT NOT NULL, version TEXT NOT NULL,
                queued_at INTEGER NOT NULL, delivered_at INTEGER,
                PRIMARY KEY(tenant_id,connector,artifact_id,version)
            )",
        )
        .unwrap();
        let empty = dashboard.snapshot("tenant-a").unwrap();
        assert!(empty.causal_revocation_outbox.available);
        assert_eq!(empty.causal_revocation_outbox.pending_notices, 0);
        assert_eq!(empty.causal_revocation_outbox.oldest_pending_seconds, None);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        for (tenant, id, queued, delivered) in [
            ("tenant-a", "secret-old", now - 360, None),
            ("tenant-a", "secret-new", now - 20, None),
            ("tenant-a", "secret-delivered", now - 7200, Some(now)),
            ("tenant-b", "other-secret", now - 86_400, None),
        ] {
            conn.execute(
                "INSERT INTO causal_ccr_revocation_outbox
                 (tenant_id,connector,artifact_id,version,queued_at,delivered_at)
                 VALUES (?1,'causal',?2,'v1',?3,?4)",
                params![tenant, id, queued, delivered],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO causal_ccr_revocation_outbox
             (tenant_id,connector,artifact_id,version,queued_at,delivered_at)
             VALUES ('tenant-a','unsupported','malformed', 'v1',?1,NULL)",
            [now - 172_800],
        )
        .unwrap();
        let a = dashboard.snapshot("tenant-a").unwrap();
        assert_eq!(a.status, "absent");
        assert_eq!(a.causal_revocation_outbox.pending_notices, 2);
        assert!(a.causal_revocation_outbox.oldest_pending_seconds.unwrap() >= 360);
        assert!(a.causal_revocation_outbox.oldest_pending_seconds.unwrap() < 400);
        let b = dashboard.snapshot("tenant-b").unwrap();
        assert_eq!(b.causal_revocation_outbox.pending_notices, 1);
        assert!(b.causal_revocation_outbox.oldest_pending_seconds.unwrap() >= 86_400);
        let json = serde_json::to_string(&a).unwrap();
        for secret in ["secret-old", "secret-new", "other-secret", "tenant-b"] {
            assert!(!json.contains(secret));
        }
        assert!(!home.path().join("ccr").exists());
    }

    #[test]
    fn causal_outbox_read_failure_is_unknown_not_zero_pending() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("memory.db"), b"not sqlite").unwrap();
        let result = CcrDashboardStore::from_home(home.path())
            .snapshot("tenant")
            .unwrap();
        assert_eq!(result.status, "absent");
        assert!(!result.causal_revocation_outbox.available);
        assert_eq!(
            result.causal_revocation_outbox.reason,
            Some("source_db_unavailable")
        );
        assert_eq!(result.causal_revocation_outbox.oldest_pending_seconds, None);
    }

    #[test]
    fn connector_lifecycle_projection_is_tenant_scoped_and_content_free() {
        let home = tempfile::tempdir().unwrap();
        let dashboard = CcrDashboardStore::from_home(home.path());
        let absent = dashboard.snapshot("tenant-a").unwrap();
        assert_eq!(absent.connector_lifecycle.reason, Some("source_db_absent"));
        assert!(!home.path().join("memory.db").exists());

        let conn = Connection::open(home.path().join("memory.db")).unwrap();
        let legacy = dashboard.snapshot("tenant-a").unwrap();
        assert_eq!(legacy.connector_lifecycle.reason, Some("legacy_schema"));
        conn.execute_batch(
            "CREATE TABLE local_connector_lifecycle_events (
               tenant_id TEXT NOT NULL, status TEXT NOT NULL,
               staged_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
               last_failure_code TEXT
             )",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        for (tenant, status, staged_at, failure) in [
            ("tenant-a", "pending", now - 300, None),
            (
                "tenant-a",
                "blocked",
                now - 90,
                Some("delivery_lease_active"),
            ),
            ("tenant-a", "completed", now - 15, None),
            (
                "tenant-b",
                "blocked",
                now - 3600,
                Some("private source wording"),
            ),
            ("tenant-b", "completed", now - 4, None),
        ] {
            conn.execute(
                "INSERT INTO local_connector_lifecycle_events
                 (tenant_id,status,staged_at,updated_at,last_failure_code)
                 VALUES (?1,?2,?3,?3,?4)",
                params![tenant, status, staged_at, failure],
            )
            .unwrap();
        }
        let a = dashboard.snapshot("tenant-a").unwrap();
        assert!(a.connector_lifecycle.available);
        assert_eq!(a.connector_lifecycle.pending_events, 2);
        assert_eq!(a.connector_lifecycle.blocked_events, 1);
        assert_eq!(a.connector_lifecycle.completed_events, 1);
        assert!(a.connector_lifecycle.oldest_pending_seconds.unwrap() >= 300);
        assert!(a.connector_lifecycle.latest_completed_seconds.unwrap() >= 15);
        assert_eq!(
            a.connector_lifecycle.last_failure_code.as_deref(),
            Some("delivery_lease_active")
        );
        let serialized = serde_json::to_string(&a).unwrap();
        assert!(!serialized.contains("private source wording"));
        assert!(!serialized.contains("tenant-b"));
        assert_eq!(
            dashboard
                .snapshot("tenant-b")
                .unwrap()
                .connector_lifecycle
                .last_failure_code,
            None
        );
    }

    #[test]
    fn causal_delivery_projection_is_exact_tenant_and_distinguishes_unknown() {
        let home = tempfile::tempdir().unwrap();
        let dashboard = CcrDashboardStore::from_home(home.path());
        let absent = dashboard.snapshot("tenant-a").unwrap();
        assert_eq!(absent.causal_delivery.reason, Some("source_db_absent"));
        let conn = Connection::open(home.path().join("memory.db")).unwrap();
        conn.execute_batch("CREATE TABLE legacy_memory (id INTEGER)")
            .unwrap();
        assert_eq!(
            dashboard
                .snapshot("tenant-a")
                .unwrap()
                .causal_delivery
                .reason,
            Some("legacy_schema")
        );
        conn.execute_batch(
            "CREATE TABLE causal_ccr_delivery_leases (
                lease_id TEXT PRIMARY KEY,tenant_id TEXT,acl TEXT,artifact_id TEXT,
                version TEXT,created_at INTEGER);
             CREATE TABLE causal_ccr_revoking (
                tenant_id TEXT,acl TEXT,artifact_id TEXT,version TEXT,started_at INTEGER);
             CREATE TABLE causal_artifacts (
                id TEXT,tenant_id TEXT,acl TEXT,version TEXT,invalidated_at INTEGER,content TEXT);",
        )
        .unwrap();
        let empty = dashboard.snapshot("tenant-a").unwrap();
        assert!(empty.causal_delivery.available);
        assert_eq!(empty.causal_delivery.active_leases, 0);
        assert_eq!(empty.causal_delivery.oldest_lease_seconds, None);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        for (id, tenant, age) in [
            ("private-a", "tenant-a", 90),
            ("private-b", "tenant-b", 900),
        ] {
            conn.execute(
                "INSERT INTO causal_ccr_delivery_leases VALUES (?1,?2,'private',?1,'v1',?3)",
                params![id, tenant, now - age],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO causal_artifacts VALUES (?1,?2,'private','v1',NULL,'source secret')",
                params![id, tenant],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO causal_ccr_revoking VALUES (?1,'private',?2,'v1',?3)",
                params![tenant, id, now - age],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO causal_ccr_revoking VALUES ('tenant-a','private','done','v1',?1)",
            [now - 1000],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO causal_artifacts VALUES ('done','tenant-a','private','v1',?1,'')",
            [now - 10],
        )
        .unwrap();
        let a = dashboard.snapshot("tenant-a").unwrap();
        assert_eq!(a.causal_delivery.active_leases, 1);
        assert_eq!(a.causal_delivery.revoking_sources, 1);
        assert!(a.causal_delivery.oldest_lease_seconds.unwrap() >= 90);
        assert!(a.causal_delivery.oldest_revoking_seconds.unwrap() >= 90);
        let b = dashboard.snapshot("tenant-b").unwrap();
        assert_eq!(b.causal_delivery.active_leases, 1);
        assert_eq!(b.causal_delivery.revoking_sources, 1);
        assert!(b.causal_delivery.oldest_lease_seconds.unwrap() >= 900);
        let json = serde_json::to_string(&a).unwrap();
        for secret in ["private-a", "private-b", "source secret", "tenant-b"] {
            assert!(!json.contains(secret));
        }
    }
}
