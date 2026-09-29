//! Durable, scoped originals for reversible tool-result compression.
//!
//! This module stores *post-redaction* text only. Callers must complete their
//! existing security interceptor before calling `put`; an opaque ID is never
//! sufficient authorization for `retrieve`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const MAX_ORIGINAL_BYTES: usize = 2 * 1024 * 1024;
const MAX_STORE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RETURN_BYTES: usize = 64 * 1024;
const DEFAULT_TTL_SECONDS: i64 = 30 * 60;
const MAX_BOUND_FIND_VALIDATIONS: usize = 8;
const CCR_ENTRY_VERSION: i64 = 1;
/// SQLite `user_version` written once every table, index and migration step in
/// [`CcrStore::open_with_busy_timeout`] has been applied. **Bump this whenever
/// that DDL batch or a migration changes** — a file already reporting this
/// version skips both, so a silently added table or index would never be
/// created. Mirrors `decision_store.rs`'s `SCHEMA_VERSION` contract.
///
/// History: `1` → `2` adds `ccr_loop_telemetry.ccr_find_rate_limited`. A file
/// stamped `1` re-enters the migration window, gets the `ALTER TABLE`, and is
/// re-stamped `2`; a file stamped `2` skips both as before.
const SCHEMA_VERSION: i64 = 2;
/// Minimum wall-clock gap between two expiry sweeps on the same store handle.
/// The sweep is a `DELETE` (a write lock); every read path opens a connection,
/// so running it per call serialised pure reads against any concurrent writer.
/// Expiry itself is never trusted to the sweep: `find`/`retrieve` filter on
/// `expires_at` in SQL, so a skipped sweep can only leave a dead row on disk.
const EXPIRY_SWEEP_MIN_INTERVAL_SECONDS: i64 = 60;
pub const CCR_LOOP_TELEMETRY_MAX_ROWS: i64 = 10_000;
/// O7: the CCR tool names and their JSON schemas are defined once, in
/// `duduclaw_core::tool_catalog` — the crate both this tool loop and the CLI
/// can reach — instead of being inlined at the point of injection. Re-exported
/// here so `duduclaw_llm::CCR_RETRIEVE_TOOL` keeps working for every existing
/// caller.
pub use duduclaw_core::tool_catalog::{CCR_FIND_TOOL, CCR_RETRIEVE_TOOL};

mod find;
mod preview;
mod retrieve;
mod revocation;
mod runtime;
mod store;

#[cfg(test)]
mod tests;

pub use runtime::{CcrBoundSourceValidator, CcrDeliveryLease, CcrRuntime};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CcrScope {
    pub tenant_id: String,
    pub agent_id: String,
    pub session_id: String,
    /// Exact caller visibility boundary. Gateway scopes include agent,
    /// session, and channel-user identity; artifact ACL provenance remains
    /// separate and must come from a trusted connector.
    pub source_acl: String,
}

impl CcrScope {
    fn valid(&self) -> bool {
        [
            &self.tenant_id,
            &self.agent_id,
            &self.session_id,
            &self.source_acl,
        ]
        .iter()
        .all(|v| !v.trim().is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcrEntry {
    pub id: String,
    pub scope: CcrScope,
    pub source_tool: String,
    pub source_call_id: String,
    pub content_sha256: String,
    pub transform_version: i64,
    pub content_bytes: usize,
    pub created_at: i64,
    pub expires_at: i64,
}

/// Source identity supplied by a trusted connector after its own ACL check.
/// This metadata is never inferred from model text or an MCP tool response.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CcrSourceArtifact {
    pub connector: String,
    pub artifact_id: String,
    pub version: String,
    pub acl_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CcrBoundSource {
    artifact: CcrSourceArtifact,
    saved_sha256: String,
}

impl CcrSourceArtifact {
    fn valid(&self) -> bool {
        [
            &self.connector,
            &self.artifact_id,
            &self.version,
            &self.acl_revision,
        ]
        .iter()
        .all(|value| !value.trim().is_empty() && value.len() <= 512)
    }
}

#[derive(Debug, Clone)]
pub struct RetrievedChunk {
    pub text: String,
    /// UTF-8 byte offset into the saved original; always a char boundary.
    pub byte_offset: usize,
    pub total_bytes: usize,
    pub truncated: bool,
    delivery_guard: Option<Arc<dyn CcrDeliveryLease>>,
}

impl RetrievedChunk {
    /// Keep this guard alive through each destination that receives `text`.
    pub fn delivery_guard(&self) -> Option<Arc<dyn CcrDeliveryLease>> {
        self.delivery_guard.clone()
    }
}

impl PartialEq for RetrievedChunk {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.byte_offset == other.byte_offset
            && self.total_bytes == other.total_bytes
            && self.truncated == other.truncated
    }
}

impl Eq for RetrievedChunk {}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CcrFindHit {
    pub id: String,
    pub byte_offset: usize,
    pub total_bytes: usize,
    pub exact_phrase: bool,
    pub matched_terms: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CcrFindReport {
    pub hits: Vec<CcrFindHit>,
    /// More bound candidates matched, but their upstream state was not checked.
    pub source_validation_limited: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CcrError {
    #[error("invalid CCR scope or source identity")]
    InvalidScope,
    #[error("CCR search query must be 3 to 128 bytes")]
    InvalidQuery,
    #[error("CCR original exceeds size limit")]
    TooLarge,
    #[error("CCR original not found, expired, or outside scope")]
    NotFound,
    #[error("CCR source has been revoked")]
    Revoked,
    #[error("CCR original failed integrity validation")]
    Corrupt,
    #[error("invalid CCR loop telemetry")]
    InvalidTelemetry,
    #[error("CCR storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("CCR filesystem failure: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct CcrStore {
    path: PathBuf,
    ttl_seconds: i64,
    max_entries: usize,
    /// Unix second of this handle's last expiry sweep. Shared by `clone()` on
    /// purpose: the gateway hands clones of one store to every tool loop, and
    /// a per-clone throttle would degrade back to a sweep per call.
    last_expiry_sweep: Arc<std::sync::atomic::AtomicI64>,
}

/// First 8 hex characters of `sha256(handle)` — the log-side form of the
/// digest `ccr_retrieval_audit.requested_id_sha256` already stores. The spec
/// says audit rows keep a digest of the requested ID; logs must not become a
/// second, undocumented audit surface carrying the plaintext handle.
pub fn handle_log_digest(handle: &str) -> String {
    format!("{:x}", Sha256::digest(handle.as_bytes()))
        .chars()
        .take(8)
        .collect()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
