//! Evolution experiment store (`<home>/evolution.db`).
//!
//! **S11 (2026-09-29): the SOUL.md version history this module was built for
//! is gone.** With the legacy SOUL rewrite path removed nothing creates,
//! observes, rolls back or consolidates a `SoulVersion` any more, so the
//! `soul_versions` / `evolution_proposals` / `deferred_gvu` /
//! `gvu_low_data_alerts` / `gvu_consolidations` tables and every accessor
//! over them were removed with it. Existing rows are left on disk untouched
//! — dropping a user's history is not this change's business — they simply
//! have no reader.
//!
//! What remains is the **experiment log** (`gvu_experiment_log`): the unified
//! per-round outcome record AEE writes and `gvu::stagnation` /
//! `gvu::telemetry` read.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

static VERSION_STORE_NO_CRYPTO_WARNED: AtomicBool = AtomicBool::new(false);

use duduclaw_security::crypto::CryptoEngine;

/// Persistent store for the evolution experiment log.
///
/// The `crypto` field is retained because `with_crypto` is the constructor
/// every caller uses (the keyfile is loaded once at boot and threaded
/// through); no column it writes today is encrypted.
pub struct VersionStore {
    db_path: PathBuf,
    #[allow(dead_code)]
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
        Self {
            db_path: db_path.to_path_buf(),
            crypto,
        }
    }

    fn init_tables(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS gvu_experiment_log (
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
                ON gvu_experiment_log(agent_id, timestamp DESC);",
        )
        .map_err(|e| e.to_string())?;

        Ok(())
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



    // ── Crypto helpers ─────────────────────────────────────────



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
    ///
    /// `ORDER BY timestamp DESC, rowid DESC` — the `rowid` tiebreak matters.
    /// `timestamp` is an RFC-3339 *string* and `to_rfc3339()` uses chrono's
    /// `AutoSi` precision, so two experiments logged inside the same clock tick
    /// (or by a platform with coarse `SystemTime` granularity) can serialize to
    /// byte-identical strings. With no tiebreak SQLite is free to return those
    /// rows in either order, and every stagnation signal in
    /// [`crate::gvu::stagnation`] scans this list newest-first and stops at the
    /// first `applied` row — so an ambiguous tie can flip an agent between
    /// "recovered" and "still stuck". `rowid` is monotonic in insertion order,
    /// which is exactly the intended ordering when timestamps collide.
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
             ORDER BY timestamp DESC, rowid DESC
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (S11, 2026-09-29): the store must bootstrap and round-trip
    /// the experiment log on a database that has none of the removed SOUL
    /// tables — the only schema this file still owns.
    #[test]
    fn experiment_log_round_trips_on_a_fresh_db() {
        let tmp = std::env::temp_dir().join(format!("dudu_vs_{}.db", uuid::Uuid::new_v4()));
        let store = VersionStore::new(&tmp);
        store.record_experiment(&ExperimentLogEntry::new(
            "agent-a",
            1,
            3,
            std::time::Duration::from_secs(2),
            "applied",
            "committed one delta",
        ));
        let rows = store.get_experiments("agent-a", 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, "applied");
        let summary = store.get_experiment_summary("agent-a");
        assert_eq!(summary.total_experiments, 1);
        assert_eq!(summary.applied_count, 1);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Regression (S11): opening an *existing* `evolution.db` that still
    /// carries the removed legacy tables must not fail — users upgrading in
    /// place keep their file, they just lose the readers.
    #[test]
    fn opens_a_db_that_still_has_legacy_soul_tables() {
        let tmp = std::env::temp_dir().join(format!("dudu_vs_{}.db", uuid::Uuid::new_v4()));
        {
            let conn = Connection::open(&tmp).unwrap();
            conn.execute_batch(
                "CREATE TABLE soul_versions (version_id TEXT PRIMARY KEY, agent_id TEXT);
                 CREATE TABLE gvu_consolidations (id TEXT PRIMARY KEY, agent_id TEXT);",
            )
            .unwrap();
        }
        let store = VersionStore::new(&tmp);
        store.record_experiment(&ExperimentLogEntry::new(
            "agent-b",
            0,
            3,
            std::time::Duration::from_secs(1),
            "skipped",
            "cooldown",
        ));
        assert_eq!(store.get_experiments("agent-b", 5).len(), 1);
        let _ = std::fs::remove_file(&tmp);
    }
}
