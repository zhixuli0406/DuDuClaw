//! SQLite-backed persistent store for autopilot (event-driven automation) rules.
//!
//! Each rule defines a trigger event, conditions, and an action to execute.
//! WAL mode + 5s busy_timeout for multi-process safety.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

// ── Rule row ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutopilotRuleRow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger_event: String,
    pub conditions: String, // JSON
    pub action: String,     // JSON
    pub created_at: String,
    pub last_triggered_at: Option<String>,
    pub trigger_count: i64,
}

// ── History row ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutopilotHistoryRow {
    pub id: String,
    pub rule_id: String,
    pub rule_name: String,
    pub triggered_at: String,
    pub result: String, // success | failure
    pub details: Option<String>,
}

// ── Store ───────────────────────────────────────────────────

pub struct AutopilotStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl AutopilotStore {
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("autopilot.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open autopilot store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "AutopilotStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS autopilot_rules (
                 id               TEXT PRIMARY KEY,
                 name             TEXT NOT NULL,
                 enabled          INTEGER NOT NULL DEFAULT 1,
                 trigger_event    TEXT NOT NULL,
                 conditions       TEXT NOT NULL DEFAULT '{}',
                 action           TEXT NOT NULL DEFAULT '{}',
                 created_at       TEXT NOT NULL,
                 last_triggered_at TEXT,
                 trigger_count    INTEGER NOT NULL DEFAULT 0
             );

             CREATE TABLE IF NOT EXISTS autopilot_history (
                 id           TEXT PRIMARY KEY,
                 rule_id      TEXT NOT NULL,
                 rule_name    TEXT NOT NULL,
                 triggered_at TEXT NOT NULL,
                 result       TEXT NOT NULL,
                 details      TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_ap_history_rule ON autopilot_history(rule_id);
             CREATE INDEX IF NOT EXISTS idx_ap_history_ts   ON autopilot_history(triggered_at DESC);",
        )
        .map_err(|e| format!("init autopilot schema: {e}"))?;
        Ok(())
    }

    // ── Rules CRUD ──────────────────────────────────────────

    pub async fn list_rules(&self) -> Result<Vec<AutopilotRuleRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, enabled, trigger_event, conditions, action,
                        created_at, last_triggered_at, trigger_count
                 FROM autopilot_rules ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare list rules: {e}"))?;
        let rows = stmt
            .query_map([], row_to_rule)
            .map_err(|e| format!("query rules: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect rules: {e}"))?;
        Ok(rows)
    }

    pub async fn get_rule(&self, id: &str) -> Result<Option<AutopilotRuleRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, name, enabled, trigger_event, conditions, action,
                    created_at, last_triggered_at, trigger_count
             FROM autopilot_rules WHERE id = ?1",
            params![id],
            row_to_rule,
        )
        .optional()
        .map_err(|e| format!("get rule: {e}"))
    }

    pub async fn insert_rule(&self, row: &AutopilotRuleRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO autopilot_rules
                (id, name, enabled, trigger_event, conditions, action,
                 created_at, last_triggered_at, trigger_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.id,
                row.name,
                if row.enabled { 1 } else { 0 },
                row.trigger_event,
                row.conditions,
                row.action,
                row.created_at,
                row.last_triggered_at,
                row.trigger_count,
            ],
        )
        .map_err(|e| format!("insert rule: {e}"))?;
        Ok(())
    }

    pub async fn update_rule(&self, id: &str, fields: &serde_json::Value) -> Result<Option<AutopilotRuleRow>, String> {
        let has_changes = {
            let conn = self.conn.lock().await;
            let mut sets: Vec<String> = Vec::new();
            let mut binds: Vec<String> = Vec::new();

            if let Some(v) = fields.get("name").and_then(|v| v.as_str()) {
                binds.push(v.to_string());
                sets.push(format!("name = ?{}", binds.len()));
            }
            if let Some(v) = fields.get("enabled") {
                if let Some(b) = v.as_bool() {
                    binds.push(if b { "1".into() } else { "0".into() });
                    sets.push(format!("enabled = ?{}", binds.len()));
                }
            }
            if let Some(v) = fields.get("trigger_event").and_then(|v| v.as_str()) {
                binds.push(v.to_string());
                sets.push(format!("trigger_event = ?{}", binds.len()));
            }
            if let Some(v) = fields.get("conditions") {
                binds.push(v.to_string());
                sets.push(format!("conditions = ?{}", binds.len()));
            }
            if let Some(v) = fields.get("action") {
                binds.push(v.to_string());
                sets.push(format!("action = ?{}", binds.len()));
            }

            if sets.is_empty() {
                false
            } else {
                binds.push(id.to_string());
                let sql = format!(
                    "UPDATE autopilot_rules SET {} WHERE id = ?{}",
                    sets.join(", "),
                    binds.len()
                );
                let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                    binds.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
                conn.execute(&sql, params_ref.as_slice())
                    .map_err(|e| format!("update rule: {e}"))?;
                true
            }
        };
        self.get_rule(id).await
    }

    pub async fn remove_rule(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let count = conn
            .execute("DELETE FROM autopilot_rules WHERE id = ?1", params![id])
            .map_err(|e| format!("remove rule: {e}"))?;
        // Also clean up history
        let _ = conn.execute("DELETE FROM autopilot_history WHERE rule_id = ?1", params![id]);
        Ok(count > 0)
    }

    // ── History ─────────────────────────────────────────────

    pub async fn list_history(&self, rule_id: Option<&str>, limit: i64) -> Result<Vec<AutopilotHistoryRow>, String> {
        let conn = self.conn.lock().await;
        let (sql, bind_val): (String, Option<String>) = match rule_id {
            Some(rid) => (
                format!(
                    "SELECT id, rule_id, rule_name, triggered_at, result, details
                     FROM autopilot_history WHERE rule_id = ?1
                     ORDER BY triggered_at DESC LIMIT {limit}"
                ),
                Some(rid.to_string()),
            ),
            None => (
                format!(
                    "SELECT id, rule_id, rule_name, triggered_at, result, details
                     FROM autopilot_history
                     ORDER BY triggered_at DESC LIMIT {limit}"
                ),
                None,
            ),
        };

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare history: {e}"))?;
        let rows = if let Some(ref rid) = bind_val {
            stmt.query_map(params![rid], row_to_history)
        } else {
            stmt.query_map([], row_to_history)
        }
        .map_err(|e| format!("query history: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("collect history: {e}"))?;
        Ok(rows)
    }

    pub async fn append_history(&self, row: &AutopilotHistoryRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO autopilot_history (id, rule_id, rule_name, triggered_at, result, details)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![row.id, row.rule_id, row.rule_name, row.triggered_at, row.result, row.details],
        )
        .map_err(|e| format!("append history: {e}"))?;

        // Bump trigger_count + last_triggered_at
        let _ = conn.execute(
            "UPDATE autopilot_rules SET trigger_count = trigger_count + 1, last_triggered_at = ?1 WHERE id = ?2",
            params![row.triggered_at, row.rule_id],
        );
        Ok(())
    }
}

fn row_to_rule(row: &rusqlite::Row) -> rusqlite::Result<AutopilotRuleRow> {
    Ok(AutopilotRuleRow {
        id: row.get(0)?,
        name: row.get(1)?,
        enabled: row.get::<_, i32>(2)? != 0,
        trigger_event: row.get(3)?,
        conditions: row.get(4)?,
        action: row.get(5)?,
        created_at: row.get(6)?,
        last_triggered_at: row.get(7)?,
        trigger_count: row.get(8)?,
    })
}

fn row_to_history(row: &rusqlite::Row) -> rusqlite::Result<AutopilotHistoryRow> {
    Ok(AutopilotHistoryRow {
        id: row.get(0)?,
        rule_id: row.get(1)?,
        rule_name: row.get(2)?,
        triggered_at: row.get(3)?,
        result: row.get(4)?,
        details: row.get(5)?,
    })
}
