//! Version store — OPRO-style historical tracking for SOUL.md versions.
//!
//! Each SOUL.md change is recorded with before/after performance metrics,
//! enabling the Generator to learn from history (which directions improved,
//! which were rolled back).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_security::crypto::CryptoEngine;

/// Performance metrics measured over a time period.
///
/// Used as both pre_metrics (baseline) and post_metrics (after change).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionMetrics {
    /// Ratio of positive feedback signals (0.0 - 1.0).
    pub positive_feedback_ratio: f64,
    /// Average prediction error during the period.
    pub avg_prediction_error: f64,
    /// Average user correction rate.
    pub user_correction_rate: f64,
    /// Number of contract violations.
    pub contract_violations: u32,
    /// Total conversations in the measurement period.
    pub conversations_count: u32,
}

/// Lifecycle status of a SOUL.md version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// Currently active and being observed.
    Observing,
    /// Observation passed — this version is confirmed.
    Confirmed,
    /// Observation failed — this version was rolled back.
    RolledBack,
}

impl VersionStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Observing => "observing",
            Self::Confirmed => "confirmed",
            Self::RolledBack => "rolled_back",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "confirmed" => Self::Confirmed,
            "rolled_back" => Self::RolledBack,
            _ => Self::Observing,
        }
    }
}

/// A versioned SOUL.md snapshot with associated metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoulVersion {
    pub version_id: String,
    pub agent_id: String,
    /// SHA-256 hash of the SOUL.md content.
    pub soul_hash: String,
    /// Summary of this version's SOUL.md (first 200 chars).
    pub soul_summary: String,
    /// When this version was applied.
    pub applied_at: DateTime<Utc>,
    /// When the observation period ends.
    pub observation_end: DateTime<Utc>,
    /// Current lifecycle status.
    pub status: VersionStatus,
    /// Performance metrics measured before this version was applied.
    pub pre_metrics: VersionMetrics,
    /// Performance metrics measured after the observation period.
    pub post_metrics: Option<VersionMetrics>,
    /// ID of the proposal that created this version.
    pub proposal_id: String,
    /// Reverse diff to undo this change.
    pub rollback_diff: String,
}

/// Persistent store for SOUL.md version history.
///
/// When a `CryptoEngine` is provided, `rollback_diff` (which contains full SOUL.md
/// content) is encrypted at rest using AES-256-GCM. Without crypto, it's stored as plaintext.
pub struct VersionStore {
    db_path: PathBuf,
    crypto: Option<CryptoEngine>,
}

impl VersionStore {
    /// Create a new VersionStore, initializing SQLite tables.
    ///
    /// If `key_bytes` is provided, rollback_diff will be encrypted at rest.
    pub fn new(db_path: &Path) -> Self {
        Self::with_crypto(db_path, None)
    }

    /// Create with optional encryption for rollback_diff.
    pub fn with_crypto(db_path: &Path, key_bytes: Option<&[u8; 32]>) -> Self {
        if let Ok(conn) = Connection::open(db_path) {
            let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
            if let Err(e) = Self::init_tables(&conn) {
                warn!("Failed to init version store tables: {e}");
            }
        }
        let crypto = key_bytes.and_then(|k| CryptoEngine::new(k).ok());
        Self { db_path: db_path.to_path_buf(), crypto }
    }

    fn init_tables(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS soul_versions (
                version_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                soul_hash TEXT NOT NULL,
                soul_summary TEXT NOT NULL,
                applied_at TEXT NOT NULL,
                observation_end TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'observing',
                pre_metrics_json TEXT NOT NULL,
                post_metrics_json TEXT,
                proposal_id TEXT NOT NULL,
                rollback_diff TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_versions_agent
                ON soul_versions(agent_id);
            CREATE INDEX IF NOT EXISTS idx_versions_status
                ON soul_versions(status);

            CREATE TABLE IF NOT EXISTS evolution_proposals (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                proposal_type TEXT NOT NULL,
                content TEXT NOT NULL,
                rationale TEXT NOT NULL,
                generation INTEGER DEFAULT 1,
                status TEXT NOT NULL DEFAULT 'generating',
                trigger_context TEXT,
                created_at TEXT NOT NULL,
                resolved_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_proposals_agent
                ON evolution_proposals(agent_id);
            CREATE INDEX IF NOT EXISTS idx_proposals_status
                ON evolution_proposals(status);"
        ).map_err(|e| e.to_string())
    }

    /// Expose db_path for creating sibling VersionStore instances.
    pub fn db_path_ref(&self) -> &Path {
        &self.db_path
    }

    fn open(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| e.to_string())?;
        Ok(conn)
    }

    /// Record a new SOUL version.
    /// rollback_diff is encrypted at rest if a CryptoEngine is configured.
    pub fn record_version(&self, version: &SoulVersion) -> Result<(), String> {
        let conn = self.open()?;
        let pre_json = serde_json::to_string(&version.pre_metrics).map_err(|e| e.to_string())?;
        let post_json = version.post_metrics.as_ref().and_then(|m| serde_json::to_string(m).ok());
        let encrypted_rollback = self.encrypt_rollback(&version.rollback_diff);

        conn.execute(
            "INSERT OR REPLACE INTO soul_versions
             (version_id, agent_id, soul_hash, soul_summary, applied_at, observation_end,
              status, pre_metrics_json, post_metrics_json, proposal_id, rollback_diff)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                version.version_id,
                version.agent_id,
                version.soul_hash,
                version.soul_summary,
                version.applied_at.to_rfc3339(),
                version.observation_end.to_rfc3339(),
                version.status.as_str(),
                pre_json,
                post_json,
                version.proposal_id,
                encrypted_rollback,
            ],
        ).map_err(|e| e.to_string())?;

        info!(version = %version.version_id, agent = %version.agent_id, "Soul version recorded");
        Ok(())
    }

    /// Get the currently observing version for an agent (if any).
    pub fn get_observing_version(&self, agent_id: &str) -> Option<SoulVersion> {
        let conn = self.open().ok()?;
        self.query_single(
            &conn,
            "SELECT * FROM soul_versions WHERE agent_id = ?1 AND status = 'observing' ORDER BY applied_at DESC LIMIT 1",
            params![agent_id],
        )
    }

    /// Get all versions past their observation end time that are still observing.
    pub fn get_expired_observations(&self) -> Vec<SoulVersion> {
        let conn = match self.open() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let now = Utc::now().to_rfc3339();
        self.query_many(
            &conn,
            "SELECT * FROM soul_versions WHERE status = 'observing' AND observation_end < ?1",
            params![now],
        )
    }

    /// Get version history for an agent (newest first), used by Generator for OPRO context.
    pub fn get_history(&self, agent_id: &str, limit: usize) -> Vec<SoulVersion> {
        let conn = match self.open() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        self.query_many(
            &conn,
            "SELECT * FROM soul_versions WHERE agent_id = ?1 ORDER BY applied_at DESC LIMIT ?2",
            params![agent_id, limit],
        )
    }

    /// Mark a version as confirmed.
    pub fn mark_confirmed(&self, version_id: &str, post_metrics: &VersionMetrics) -> Result<(), String> {
        let conn = self.open()?;
        let json = serde_json::to_string(post_metrics).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE soul_versions SET status = 'confirmed', post_metrics_json = ?1 WHERE version_id = ?2",
            params![json, version_id],
        ).map_err(|e| e.to_string())?;
        info!(version = version_id, "Soul version confirmed");
        Ok(())
    }

    /// Mark a version as rolled back.
    pub fn mark_rolled_back(&self, version_id: &str, reason: &str) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE soul_versions SET status = 'rolled_back' WHERE version_id = ?1",
            params![version_id],
        ).map_err(|e| e.to_string())?;
        info!(version = version_id, reason, "Soul version rolled back");
        Ok(())
    }

    // ── Crypto helpers ─────────────────────────────────────────

    /// Encrypt rollback_diff if crypto is available, otherwise return as-is.
    fn encrypt_rollback(&self, plaintext: &str) -> String {
        match &self.crypto {
            Some(engine) => engine.encrypt_string(plaintext).unwrap_or_else(|e| {
                warn!("Failed to encrypt rollback_diff: {e} — storing as plaintext");
                plaintext.to_string()
            }),
            None => plaintext.to_string(),
        }
    }

    /// Decrypt rollback_diff if crypto is available, otherwise return as-is.
    fn decrypt_rollback(&self, stored: &str) -> String {
        match &self.crypto {
            Some(engine) => engine.decrypt_string(stored).unwrap_or_else(|e| {
                // May be plaintext from before encryption was enabled — log and fallback
                warn!("Failed to decrypt rollback_diff (may be pre-encryption plaintext): {e}");
                stored.to_string()
            }),
            None => stored.to_string(),
        }
    }

    // ── Query helpers ─────────────────────────────────────────

    fn query_single(&self, conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Option<SoulVersion> {
        conn.query_row(sql, params, |row| Self::row_to_version(row))
            .ok()
            .map(|mut v| { v.rollback_diff = self.decrypt_rollback(&v.rollback_diff); v })
    }

    fn query_many(&self, conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Vec<SoulVersion> {
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map(params, |row| Self::row_to_version(row)) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        rows.filter_map(|r| r.ok())
            .map(|mut v| { v.rollback_diff = self.decrypt_rollback(&v.rollback_diff); v })
            .collect()
    }

    fn row_to_version(row: &rusqlite::Row) -> rusqlite::Result<SoulVersion> {
        let applied_str: String = row.get("applied_at")?;
        let obs_str: String = row.get("observation_end")?;
        let status_str: String = row.get("status")?;
        let pre_json: String = row.get("pre_metrics_json")?;
        let post_json: Option<String> = row.get("post_metrics_json")?;

        Ok(SoulVersion {
            version_id: row.get("version_id")?,
            agent_id: row.get("agent_id")?,
            soul_hash: row.get("soul_hash")?,
            soul_summary: row.get("soul_summary")?,
            applied_at: DateTime::parse_from_rfc3339(&applied_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            observation_end: DateTime::parse_from_rfc3339(&obs_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            status: VersionStatus::from_str(&status_str),
            pre_metrics: serde_json::from_str(&pre_json).unwrap_or_default(),
            post_metrics: post_json.and_then(|j| serde_json::from_str(&j).ok()),
            proposal_id: row.get("proposal_id")?,
            rollback_diff: row.get("rollback_diff")?,
        })
    }
}
