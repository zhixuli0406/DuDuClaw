//! Version store — OPRO-style historical tracking for SOUL.md versions.
//!
//! Each SOUL.md change is recorded with before/after performance metrics,
//! enabling the Generator to learn from history (which directions improved,
//! which were rolled back).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

static VERSION_STORE_NO_CRYPTO_WARNED: AtomicBool = AtomicBool::new(false);

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
    /// SHA-256 hex digest of the plaintext rollback_diff for integrity verification.
    #[serde(default)]
    pub rollback_diff_hash: Option<String>,
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
        if !VERSION_STORE_NO_CRYPTO_WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "VersionStore initialized without encryption — \
                 rollback_diff stored as plaintext. \
                 Use VersionStore::with_crypto() for production."
            );
        }
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
                ON evolution_proposals(status);

            CREATE TABLE IF NOT EXISTS deferred_gvu (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                gradients_json TEXT NOT NULL,
                retry_after TEXT NOT NULL,
                retry_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
            );
            CREATE INDEX IF NOT EXISTS idx_deferred_agent
                ON deferred_gvu(agent_id, status);

            CREATE TABLE IF NOT EXISTS gvu_experiment_log (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                generations_used INTEGER NOT NULL,
                generations_budget INTEGER NOT NULL,
                duration_secs REAL NOT NULL,
                outcome TEXT NOT NULL,
                description TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_experiment_agent_time
                ON gvu_experiment_log(agent_id, timestamp DESC);"
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
            // Hash not stored in legacy DB rows — integrity check skipped for old records
            rollback_diff_hash: None,
        })
    }

    // ── GVU Experiment Log ──────────────────────────────────

    /// Record a GVU experiment outcome.
    pub fn record_experiment(&self, entry: &ExperimentLogEntry) {
        let conn = match self.open() {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to open DB for experiment log: {e}");
                return;
            }
        };

        if let Err(e) = conn.execute(
            "INSERT INTO gvu_experiment_log
             (id, agent_id, timestamp, generations_used, generations_budget, duration_secs, outcome, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.id,
                entry.agent_id,
                entry.timestamp.to_rfc3339(),
                entry.generations_used,
                entry.generations_budget,
                entry.duration_secs,
                entry.outcome,
                entry.description,
            ],
        ) {
            warn!(agent = %entry.agent_id, "Failed to record experiment: {e}");
        } else {
            info!(
                agent = %entry.agent_id,
                outcome = %entry.outcome,
                generations = entry.generations_used,
                duration = format!("{:.1}s", entry.duration_secs),
                "GVU experiment logged"
            );
        }
    }

    /// Get recent experiment log entries for an agent (newest first).
    pub fn get_experiments(&self, agent_id: &str, limit: usize) -> Vec<ExperimentLogEntry> {
        let conn = match self.open() {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let mut stmt = match conn.prepare(
            "SELECT id, agent_id, timestamp, generations_used, generations_budget,
                    duration_secs, outcome, description
             FROM gvu_experiment_log
             WHERE agent_id = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        stmt.query_map(params![agent_id, limit], |row| {
            let ts_str: String = row.get(2)?;
            Ok(ExperimentLogEntry {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                timestamp: DateTime::parse_from_rfc3339(&ts_str)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                generations_used: row.get(3)?,
                generations_budget: row.get(4)?,
                duration_secs: row.get(5)?,
                outcome: row.get(6)?,
                description: row.get(7)?,
            })
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Get summary statistics for an agent's GVU experiments.
    pub fn get_experiment_summary(&self, agent_id: &str) -> ExperimentSummary {
        let conn = match self.open() {
            Ok(c) => c,
            Err(_) => return ExperimentSummary::default(),
        };

        let mut summary = ExperimentSummary::default();

        // Aggregate counts and averages in a single query
        let result = conn.query_row(
            "SELECT
                COUNT(*) as total,
                SUM(CASE WHEN outcome = 'applied' THEN 1 ELSE 0 END),
                SUM(CASE WHEN outcome = 'abandoned' THEN 1 ELSE 0 END),
                SUM(CASE WHEN outcome = 'deferred' THEN 1 ELSE 0 END),
                SUM(CASE WHEN outcome = 'timed_out' THEN 1 ELSE 0 END),
                SUM(CASE WHEN outcome = 'skipped' THEN 1 ELSE 0 END),
                AVG(duration_secs),
                AVG(generations_used)
             FROM gvu_experiment_log
             WHERE agent_id = ?1",
            params![agent_id],
            |row| {
                summary.total_experiments = row.get::<_, i64>(0).unwrap_or(0) as u64;
                summary.applied_count = row.get::<_, i64>(1).unwrap_or(0) as u64;
                summary.abandoned_count = row.get::<_, i64>(2).unwrap_or(0) as u64;
                summary.deferred_count = row.get::<_, i64>(3).unwrap_or(0) as u64;
                summary.timed_out_count = row.get::<_, i64>(4).unwrap_or(0) as u64;
                summary.skipped_count = row.get::<_, i64>(5).unwrap_or(0) as u64;
                summary.avg_duration_secs = row.get::<_, f64>(6).unwrap_or(0.0);
                summary.avg_generations_used = row.get::<_, f64>(7).unwrap_or(0.0);
                Ok(())
            },
        );

        if result.is_err() {
            return summary;
        }

        let actionable = summary.total_experiments - summary.skipped_count;
        if actionable > 0 {
            summary.success_rate = summary.applied_count as f64 / actionable as f64;
        }

        summary
    }

    // ── Deferred GVU management (Phase 1.4) ─────────────────

    /// Store a deferred GVU attempt for later retry.
    pub fn store_deferred(
        &self,
        agent_id: &str,
        gradients: &[super::text_gradient::TextGradient],
        retry_after_hours: f64,
        retry_count: u32,
    ) -> Result<String, String> {
        let conn = self.open()?;
        let id = uuid::Uuid::new_v4().to_string();
        let gradients_json = serde_json::to_string(gradients).map_err(|e| e.to_string())?;
        let retry_after = chrono::Utc::now()
            + chrono::Duration::seconds((retry_after_hours * 3600.0) as i64);

        conn.execute(
            "INSERT INTO deferred_gvu (id, agent_id, gradients_json, retry_after, retry_count, created_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
            params![
                id,
                agent_id,
                gradients_json,
                retry_after.to_rfc3339(),
                retry_count,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| format!("Store deferred: {e}"))?;

        Ok(id)
    }

    /// Get pending deferred GVU attempts that are ready for retry.
    pub fn get_pending_deferred(
        &self,
        agent_id: &str,
    ) -> Vec<DeferredGvu> {
        let conn = match self.open() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let mut stmt = match conn.prepare(
            "SELECT id, agent_id, gradients_json, retry_after, retry_count
             FROM deferred_gvu
             WHERE agent_id = ?1 AND status = 'pending' AND retry_after <= ?2
             ORDER BY created_at ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let now = chrono::Utc::now().to_rfc3339();
        stmt.query_map(params![agent_id, now], |row| {
            let gradients_json: String = row.get(2)?;
            Ok(DeferredGvu {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                gradients: serde_json::from_str(&gradients_json).unwrap_or_default(),
                retry_count: row.get(4)?,
            })
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Mark a deferred GVU as completed (either retried or abandoned).
    pub fn mark_deferred_completed(&self, id: &str) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE deferred_gvu SET status = 'completed' WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Mark deferred completed: {e}"))?;
        Ok(())
    }
}

/// A pending deferred GVU retry.
#[derive(Debug, Clone)]
pub struct DeferredGvu {
    pub id: String,
    pub agent_id: String,
    pub gradients: Vec<super::text_gradient::TextGradient>,
    pub retry_count: u32,
}

// ── GVU Experiment Log (autoresearch-inspired) ────────────────────────────
//
// Unified log of ALL GVU attempts (applied/abandoned/deferred/timed_out/skipped).
// Analogous to autoresearch's `results.tsv` — enables MetaCognition analytics
// and historical experiment review.

/// A single GVU experiment log entry.
///
/// Records every GVU cycle outcome with timing and generation counts,
/// providing the data backbone for MetaCognition self-calibration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentLogEntry {
    pub id: String,
    pub agent_id: String,
    pub timestamp: DateTime<Utc>,
    /// How many generations were actually executed.
    pub generations_used: u32,
    /// The max_generations budget for this run.
    pub generations_budget: u32,
    /// Wall-clock duration of the entire cycle.
    pub duration_secs: f64,
    /// Outcome: "applied", "abandoned", "deferred", "timed_out", "skipped".
    pub outcome: String,
    /// Human-readable description of what happened.
    pub description: String,
}

impl ExperimentLogEntry {
    pub fn new(
        agent_id: &str,
        generations_used: u32,
        generations_budget: u32,
        duration: std::time::Duration,
        outcome: &str,
        description: &str,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            timestamp: Utc::now(),
            generations_used,
            generations_budget,
            duration_secs: duration.as_secs_f64(),
            outcome: outcome.to_string(),
            description: description.to_string(),
        }
    }
}

/// Summary statistics for an agent's GVU experiment history.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExperimentSummary {
    pub total_experiments: u64,
    pub applied_count: u64,
    pub abandoned_count: u64,
    pub deferred_count: u64,
    pub timed_out_count: u64,
    pub skipped_count: u64,
    pub avg_duration_secs: f64,
    pub avg_generations_used: f64,
    /// Success rate: applied / (total - skipped).
    pub success_rate: f64,
}
