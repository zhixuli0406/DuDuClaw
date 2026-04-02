//! SQLite-backed experiment logger for paper experiments and ROI analytics.

use std::path::Path;

use tokio::sync::Mutex;
use tracing::{info, warn};

use super::types::{ExperimentRecord, GvuRecord, VersionOutcomeRecord};

/// Experiment data logger backed by SQLite.
///
/// Uses `tokio::sync::Mutex` because `rusqlite::Connection` is `!Send`.
/// Write operations (log_*) are fast (<1ms INSERT) and non-blocking in practice.
/// Read operations (export_*) are only called from CLI, not during request handling.
pub struct ExperimentLogger {
    db: Mutex<rusqlite::Connection>,
}

/// Escape a string value for CSV output.
/// Wraps in quotes if the value contains commas, quotes, newlines, or starts with
/// characters that could trigger formula injection in spreadsheets (=, +, -, @).
pub fn csv_escape(val: &str) -> String {
    let needs_quote = val.contains(',')
        || val.contains('"')
        || val.contains('\n')
        || val.contains('\r')
        || val.starts_with('=')
        || val.starts_with('+')
        || val.starts_with('-')
        || val.starts_with('@');
    if needs_quote {
        format!("\"{}\"", val.replace('"', "\"\""))
    } else {
        val.to_string()
    }
}

impl ExperimentLogger {
    /// Open or create the experiment database.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("experiment.db");
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| format!("Failed to open experiment.db: {e}"))?;

        // Enable WAL mode for concurrent read/write
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("Failed to set WAL mode: {e}"))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS experiment_records (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                channel TEXT NOT NULL,
                predicted_satisfaction REAL NOT NULL,
                actual_inferred_satisfaction REAL NOT NULL,
                composite_error REAL NOT NULL,
                error_category TEXT NOT NULL,
                evolution_action TEXT NOT NULL,
                llm_calls_count INTEGER NOT NULL DEFAULT 0,
                llm_tokens_used INTEGER NOT NULL DEFAULT 0,
                latency_ms INTEGER NOT NULL DEFAULT 0,
                timestamp TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS gvu_records (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                gvu_rounds INTEGER NOT NULL,
                final_verdict TEXT NOT NULL,
                l1_passed INTEGER NOT NULL DEFAULT 1,
                l2_passed INTEGER NOT NULL DEFAULT 1,
                l3_passed INTEGER NOT NULL DEFAULT 1,
                l4_passed INTEGER NOT NULL DEFAULT 1,
                text_gradient_count INTEGER NOT NULL DEFAULT 0,
                generator_model TEXT NOT NULL DEFAULT '',
                verifier_model TEXT NOT NULL DEFAULT '',
                total_llm_cost_usd REAL NOT NULL DEFAULT 0.0,
                timestamp TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS version_outcomes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                version_id TEXT NOT NULL,
                status TEXT NOT NULL,
                observation_hours REAL NOT NULL DEFAULT 0.0,
                pre_satisfaction REAL NOT NULL DEFAULT 0.0,
                post_satisfaction REAL NOT NULL DEFAULT 0.0,
                pre_correction_rate REAL NOT NULL DEFAULT 0.0,
                post_correction_rate REAL NOT NULL DEFAULT 0.0,
                rollback_reason TEXT,
                timestamp TEXT NOT NULL
            );",
        )
        .map_err(|e| format!("Failed to create experiment tables: {e}"))?;

        info!("Experiment logger initialized at {}", db_path.display());

        Ok(Self {
            db: Mutex::new(conn),
        })
    }

    /// Log a per-conversation experiment record.
    pub async fn log_experiment(&self, record: &ExperimentRecord) {
        let db = self.db.lock().await;
        if let Err(e) = db.execute(
            "INSERT INTO experiment_records
             (conversation_id, agent_id, user_id, channel,
              predicted_satisfaction, actual_inferred_satisfaction,
              composite_error, error_category, evolution_action,
              llm_calls_count, llm_tokens_used, latency_ms, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                record.conversation_id,
                record.agent_id,
                record.user_id,
                record.channel,
                record.predicted_satisfaction,
                record.actual_inferred_satisfaction,
                record.composite_error,
                record.error_category,
                record.evolution_action,
                record.llm_calls_count,
                record.llm_tokens_used,
                record.latency_ms,
                record.timestamp,
            ],
        ) {
            warn!("Failed to log experiment record: {e}");
        }
    }

    /// Log a GVU self-play round record.
    pub async fn log_gvu(&self, record: &GvuRecord) {
        let db = self.db.lock().await;
        if let Err(e) = db.execute(
            "INSERT INTO gvu_records
             (agent_id, conversation_id, gvu_rounds, final_verdict,
              l1_passed, l2_passed, l3_passed, l4_passed,
              text_gradient_count, generator_model, verifier_model,
              total_llm_cost_usd, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                record.agent_id,
                record.conversation_id,
                record.gvu_rounds,
                record.final_verdict,
                record.l1_passed as i32,
                record.l2_passed as i32,
                record.l3_passed as i32,
                record.l4_passed as i32,
                record.text_gradient_count,
                record.generator_model,
                record.verifier_model,
                record.total_llm_cost_usd,
                record.timestamp,
            ],
        ) {
            warn!("Failed to log GVU record: {e}");
        }
    }

    /// Log a SOUL.md version outcome.
    pub async fn log_version_outcome(&self, record: &VersionOutcomeRecord) {
        let db = self.db.lock().await;
        if let Err(e) = db.execute(
            "INSERT INTO version_outcomes
             (agent_id, version_id, status, observation_hours,
              pre_satisfaction, post_satisfaction,
              pre_correction_rate, post_correction_rate,
              rollback_reason, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                record.agent_id,
                record.version_id,
                record.status,
                record.observation_hours,
                record.pre_satisfaction,
                record.post_satisfaction,
                record.pre_correction_rate,
                record.post_correction_rate,
                record.rollback_reason,
                record.timestamp,
            ],
        ) {
            warn!("Failed to log version outcome: {e}");
        }
    }

    /// Export experiment records as JSON array, with optional time range and agent filter.
    pub async fn export_experiments(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Vec<ExperimentRecord>, String> {
        let db = self.db.lock().await;
        let mut sql = "SELECT conversation_id, agent_id, user_id, channel, \
            predicted_satisfaction, actual_inferred_satisfaction, \
            composite_error, error_category, evolution_action, \
            llm_calls_count, llm_tokens_used, latency_ms, timestamp \
            FROM experiment_records WHERE 1=1".to_string();
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(f) = from {
            sql.push_str(" AND timestamp >= ?");
            params_vec.push(Box::new(f.to_string()));
        }
        if let Some(t) = to {
            sql.push_str(" AND timestamp <= ?");
            params_vec.push(Box::new(t.to_string()));
        }
        if let Some(a) = agent {
            sql.push_str(" AND agent_id = ?");
            params_vec.push(Box::new(a.to_string()));
        }
        sql.push_str(" ORDER BY timestamp ASC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let mut stmt = db.prepare(&sql).map_err(|e| format!("SQL error: {e}"))?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(ExperimentRecord {
                conversation_id: row.get(0)?,
                agent_id: row.get(1)?,
                user_id: row.get(2)?,
                channel: row.get(3)?,
                predicted_satisfaction: row.get(4)?,
                actual_inferred_satisfaction: row.get(5)?,
                composite_error: row.get(6)?,
                error_category: row.get(7)?,
                evolution_action: row.get(8)?,
                llm_calls_count: row.get(9)?,
                llm_tokens_used: row.get(10)?,
                latency_ms: row.get(11)?,
                timestamp: row.get(12)?,
            })
        }).map_err(|e| format!("Query error: {e}"))?;

        let mut results = Vec::new();
        for r in rows {
            results.push(r.map_err(|e| format!("Row error: {e}"))?);
        }
        Ok(results)
    }

    /// Export GVU records as JSON array.
    pub async fn export_gvu(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Vec<GvuRecord>, String> {
        let db = self.db.lock().await;
        let mut sql = "SELECT agent_id, conversation_id, gvu_rounds, final_verdict, \
            l1_passed, l2_passed, l3_passed, l4_passed, \
            text_gradient_count, generator_model, verifier_model, \
            total_llm_cost_usd, timestamp \
            FROM gvu_records WHERE 1=1".to_string();
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(f) = from {
            sql.push_str(" AND timestamp >= ?");
            params_vec.push(Box::new(f.to_string()));
        }
        if let Some(t) = to {
            sql.push_str(" AND timestamp <= ?");
            params_vec.push(Box::new(t.to_string()));
        }
        if let Some(a) = agent {
            sql.push_str(" AND agent_id = ?");
            params_vec.push(Box::new(a.to_string()));
        }
        sql.push_str(" ORDER BY timestamp ASC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let mut stmt = db.prepare(&sql).map_err(|e| format!("SQL error: {e}"))?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(GvuRecord {
                agent_id: row.get(0)?,
                conversation_id: row.get(1)?,
                gvu_rounds: row.get(2)?,
                final_verdict: row.get(3)?,
                l1_passed: row.get::<_, i32>(4)? != 0,
                l2_passed: row.get::<_, i32>(5)? != 0,
                l3_passed: row.get::<_, i32>(6)? != 0,
                l4_passed: row.get::<_, i32>(7)? != 0,
                text_gradient_count: row.get(8)?,
                generator_model: row.get(9)?,
                verifier_model: row.get(10)?,
                total_llm_cost_usd: row.get(11)?,
                timestamp: row.get(12)?,
            })
        }).map_err(|e| format!("Query error: {e}"))?;

        let mut results = Vec::new();
        for r in rows {
            results.push(r.map_err(|e| format!("Row error: {e}"))?);
        }
        Ok(results)
    }

    /// Export version outcome records as JSON array.
    pub async fn export_versions(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Vec<VersionOutcomeRecord>, String> {
        let db = self.db.lock().await;
        let mut sql = "SELECT agent_id, version_id, status, observation_hours, \
            pre_satisfaction, post_satisfaction, pre_correction_rate, post_correction_rate, \
            rollback_reason, timestamp \
            FROM version_outcomes WHERE 1=1".to_string();
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(f) = from {
            sql.push_str(" AND timestamp >= ?");
            params_vec.push(Box::new(f.to_string()));
        }
        if let Some(t) = to {
            sql.push_str(" AND timestamp <= ?");
            params_vec.push(Box::new(t.to_string()));
        }
        if let Some(a) = agent {
            sql.push_str(" AND agent_id = ?");
            params_vec.push(Box::new(a.to_string()));
        }
        sql.push_str(" ORDER BY timestamp ASC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let mut stmt = db.prepare(&sql).map_err(|e| format!("SQL error: {e}"))?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(VersionOutcomeRecord {
                agent_id: row.get(0)?,
                version_id: row.get(1)?,
                status: row.get(2)?,
                observation_hours: row.get(3)?,
                pre_satisfaction: row.get(4)?,
                post_satisfaction: row.get(5)?,
                pre_correction_rate: row.get(6)?,
                post_correction_rate: row.get(7)?,
                rollback_reason: row.get(8)?,
                timestamp: row.get(9)?,
            })
        }).map_err(|e| format!("Query error: {e}"))?;

        let mut results = Vec::new();
        for r in rows {
            results.push(r.map_err(|e| format!("Row error: {e}"))?);
        }
        Ok(results)
    }

    /// Count total experiment records (for diagnostics).
    pub async fn count_records(&self) -> (u64, u64, u64) {
        let db = self.db.lock().await;
        let experiments: u64 = db
            .query_row("SELECT COUNT(*) FROM experiment_records", [], |r| r.get(0))
            .unwrap_or(0);
        let gvu: u64 = db
            .query_row("SELECT COUNT(*) FROM gvu_records", [], |r| r.get(0))
            .unwrap_or(0);
        let versions: u64 = db
            .query_row("SELECT COUNT(*) FROM version_outcomes", [], |r| r.get(0))
            .unwrap_or(0);
        (experiments, gvu, versions)
    }
}
