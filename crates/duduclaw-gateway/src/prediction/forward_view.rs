//! Read-only dashboard views over the task forward-model audit trail
//! (`task_prediction_log` in `prediction.db`).
//!
//! The v1.53 task forward model and v1.54 calibration layer had zero
//! dashboard surface — predictions were made, settled and scored with no
//! way for an operator to see any of it (LWM experiment D4 feedback:
//! "看不到 LLM→LWM 相關資訊"). LWM is a platform feature, not an
//! experiment artifact, so this view is generic: it reads only the
//! platform store and works for every agent — the trading experiment is
//! just one producer among many.
//!
//! Deliberately read-only and fail-open: a missing/corrupt db yields empty
//! views, never an error surfaced to the dashboard. The scan is bounded
//! ([`SCAN_CAP`] newest rows) and the response says so (`window_scanned`)
//! — a bounded window presented as the whole history would be dishonest.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

/// Newest-rows scan window for the per-agent summary fold.
pub const SCAN_CAP: usize = 5000;
/// Hard cap for `forward_recent`'s limit param.
pub const RECENT_MAX_LIMIT: usize = 200;

/// Per-agent aggregate over the scanned window.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ForwardAgentSummary {
    pub agent_id: String,
    /// Predictions in the window (settled + pending).
    pub total: u64,
    pub settled: u64,
    /// Mean stored Brier score over settled rows that have one (calibration
    /// on). `None` when no row carries a score.
    pub avg_brier: Option<f64>,
    pub avg_composite_error: Option<f64>,
    /// Settled rows by error category (negligible/moderate/significant/critical).
    pub categories: BTreeMap<String, u64>,
    /// Settled rows by observation fidelity (full/mcp_only/none).
    pub fidelity: BTreeMap<String, u64>,
    /// All rows by prediction source tier (canonical/marginal/prior/llm…).
    pub sources: BTreeMap<String, u64>,
    pub last_settled_at: Option<String>,
}

/// One recent prediction row for the dashboard list view.
#[derive(Debug, Clone, Serialize)]
pub struct ForwardRecentRow {
    pub prediction_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub round: u32,
    pub source: String,
    pub fidelity: Option<String>,
    pub category: Option<String>,
    pub brier: Option<f64>,
    pub composite_error: Option<f64>,
    pub created_at: String,
    pub settled_at: Option<String>,
}

struct ScannedRow {
    prediction_id: String,
    task_id: String,
    agent_id: String,
    round: u32,
    source: String,
    fidelity: Option<String>,
    category: Option<String>,
    brier: Option<f64>,
    composite_error: Option<f64>,
    created_at: String,
    settled_at: Option<String>,
}

/// Scan the newest rows (bounded), newest first. Any failure ⇒ empty.
fn scan_rows(db_path: &Path, agent_filter: Option<&str>, cap: usize) -> Vec<ScannedRow> {
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = Connection::open(db_path) else {
        return Vec::new();
    };
    let base = "SELECT prediction_id, task_id, agent_id, round, prediction_source,
                       fidelity, category, brier_score, composite_error,
                       created_at, settled_at
                FROM task_prediction_log";
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ScannedRow> {
        Ok(ScannedRow {
            prediction_id: row.get(0)?,
            task_id: row.get(1)?,
            agent_id: row.get(2)?,
            round: row.get::<_, i64>(3)?.max(0) as u32,
            source: row.get(4)?,
            fidelity: row.get(5)?,
            category: row.get(6)?,
            brier: row.get(7)?,
            composite_error: row.get(8)?,
            created_at: row.get(9)?,
            settled_at: row.get(10)?,
        })
    };
    let result = match agent_filter {
        Some(agent) => {
            let sql = format!("{base} WHERE agent_id = ?1 ORDER BY id DESC LIMIT ?2");
            conn.prepare(&sql).and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![agent, cap as i64], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        }
        None => {
            let sql = format!("{base} ORDER BY id DESC LIMIT ?1");
            conn.prepare(&sql).and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![cap as i64], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        }
    };
    result.unwrap_or_default()
}

/// Per-agent summaries over the scanned window, sorted by agent id.
/// Returns `(summaries, window_scanned)`.
pub fn forward_summaries(
    db_path: &Path,
    agent_filter: Option<&str>,
) -> (Vec<ForwardAgentSummary>, u64) {
    let rows = scan_rows(db_path, agent_filter, SCAN_CAP);
    let scanned = rows.len() as u64;
    let mut by_agent: BTreeMap<String, (ForwardAgentSummary, f64, u64, f64, u64)> = BTreeMap::new();
    for r in rows {
        let entry = by_agent.entry(r.agent_id.clone()).or_insert_with(|| {
            (
                ForwardAgentSummary { agent_id: r.agent_id.clone(), ..Default::default() },
                0.0, // brier sum
                0,   // brier n
                0.0, // composite sum
                0,   // composite n
            )
        });
        let (summary, brier_sum, brier_n, comp_sum, comp_n) = entry;
        summary.total += 1;
        *summary.sources.entry(r.source).or_insert(0) += 1;
        if r.settled_at.is_some() {
            summary.settled += 1;
            if let Some(c) = r.category {
                *summary.categories.entry(c).or_insert(0) += 1;
            }
            if let Some(f) = r.fidelity {
                *summary.fidelity.entry(f).or_insert(0) += 1;
            }
            if let Some(b) = r.brier {
                *brier_sum += b;
                *brier_n += 1;
            }
            if let Some(e) = r.composite_error {
                *comp_sum += e;
                *comp_n += 1;
            }
            // Rows come newest-first, so the first settled_at seen per agent
            // is the latest one.
            if summary.last_settled_at.is_none() {
                summary.last_settled_at = r.settled_at;
            }
        }
    }
    let summaries = by_agent
        .into_values()
        .map(|(mut s, brier_sum, brier_n, comp_sum, comp_n)| {
            if brier_n > 0 {
                s.avg_brier = Some(brier_sum / brier_n as f64);
            }
            if comp_n > 0 {
                s.avg_composite_error = Some(comp_sum / comp_n as f64);
            }
            s
        })
        .collect();
    (summaries, scanned)
}

/// Newest predictions (settled and pending), newest first.
pub fn forward_recent(
    db_path: &Path,
    agent_filter: Option<&str>,
    limit: usize,
) -> Vec<ForwardRecentRow> {
    let capped = limit.clamp(1, RECENT_MAX_LIMIT);
    scan_rows(db_path, agent_filter, capped)
        .into_iter()
        .map(|r| ForwardRecentRow {
            prediction_id: r.prediction_id,
            task_id: r.task_id,
            agent_id: r.agent_id,
            round: r.round,
            source: r.source,
            fidelity: r.fidelity,
            category: r.category,
            brier: r.brier,
            composite_error: r.composite_error,
            created_at: r.created_at,
            settled_at: r.settled_at,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prediction::task_forward_store::TaskForwardModel;

    /// Create the REAL schema via the store (no duplicated DDL — schema
    /// drift would silently break these views otherwise), then insert rows
    /// with raw SQL for fixture brevity.
    fn seeded_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let db_path = dir.path().join("prediction.db");
        let _model = TaskForwardModel::new(db_path.clone());
        let conn = Connection::open(&db_path).unwrap();
        let insert = "INSERT INTO task_prediction_log
            (prediction_id, task_id, agent_id, round, state_key, prediction_json,
             prediction_source, created_at, observation_json, fidelity,
             composite_error, category, settled_at, brier_score)
            VALUES (?1, ?2, ?3, ?4, 'k', '{}', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)";
        // trader: two settled (one clean, one critical), one pending.
        conn.execute(
            insert,
            rusqlite::params![
                "p1", "t1", "trader", 1, "canonical", "2026-08-13T01:00:00+00:00",
                Some("{}"), Some("full"), Some(0.1_f64), Some("negligible"),
                Some("2026-08-13T02:00:00+00:00"), Some(0.04_f64)
            ],
        )
        .unwrap();
        conn.execute(
            insert,
            rusqlite::params![
                "p2", "t2", "trader", 1, "prior", "2026-08-13T03:00:00+00:00",
                Some("{}"), Some("mcp_only"), Some(0.9_f64), Some("critical"),
                Some("2026-08-13T04:00:00+00:00"), Some(0.64_f64)
            ],
        )
        .unwrap();
        conn.execute(
            insert,
            rusqlite::params![
                "p3", "t3", "trader", 2, "canonical", "2026-08-13T05:00:00+00:00",
                None::<String>, None::<String>, None::<f64>, None::<String>,
                None::<String>, None::<f64>
            ],
        )
        .unwrap();
        // other agent: one settled row without a brier score (calibration off).
        conn.execute(
            insert,
            rusqlite::params![
                "p4", "t4", "helper", 1, "marginal", "2026-08-13T01:30:00+00:00",
                Some("{}"), Some("none"), Some(0.5_f64), Some("moderate"),
                Some("2026-08-13T01:45:00+00:00"), None::<f64>
            ],
        )
        .unwrap();
        db_path
    }

    #[test]
    fn missing_db_yields_empty_views() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.db");
        assert_eq!(forward_summaries(&path, None).0, Vec::new());
        assert!(forward_recent(&path, None, 10).is_empty());
    }

    #[test]
    fn summaries_aggregate_per_agent() {
        let dir = tempfile::tempdir().unwrap();
        let db = seeded_db(&dir);
        let (summaries, scanned) = forward_summaries(&db, None);
        assert_eq!(scanned, 4);
        assert_eq!(summaries.len(), 2);

        let trader = summaries.iter().find(|s| s.agent_id == "trader").unwrap();
        assert_eq!(trader.total, 3);
        assert_eq!(trader.settled, 2);
        assert_eq!(trader.categories.get("negligible"), Some(&1));
        assert_eq!(trader.categories.get("critical"), Some(&1));
        assert_eq!(trader.fidelity.get("full"), Some(&1));
        assert_eq!(trader.fidelity.get("mcp_only"), Some(&1));
        assert_eq!(trader.sources.get("canonical"), Some(&2));
        assert_eq!(trader.sources.get("prior"), Some(&1));
        let avg = trader.avg_brier.unwrap();
        assert!((avg - 0.34).abs() < 1e-9, "avg of 0.04/0.64, got {avg}");
        // Newest settled row wins the last_settled_at slot.
        assert_eq!(trader.last_settled_at.as_deref(), Some("2026-08-13T04:00:00+00:00"));

        let helper = summaries.iter().find(|s| s.agent_id == "helper").unwrap();
        assert_eq!(helper.settled, 1);
        // No brier column written (calibration off) → honest None, not 0.0.
        assert!(helper.avg_brier.is_none());
        assert_eq!(helper.avg_composite_error, Some(0.5));
    }

    #[test]
    fn agent_filter_scopes_both_views() {
        let dir = tempfile::tempdir().unwrap();
        let db = seeded_db(&dir);
        let (summaries, _) = forward_summaries(&db, Some("helper"));
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].agent_id, "helper");
        let rows = forward_recent(&db, Some("trader"), 50);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.agent_id == "trader"));
    }

    #[test]
    fn recent_is_newest_first_and_limit_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let db = seeded_db(&dir);
        let rows = forward_recent(&db, None, 2);
        assert_eq!(rows.len(), 2);
        // Insert order p1..p4 → newest by rowid is p4 then p3.
        assert_eq!(rows[0].prediction_id, "p4");
        assert_eq!(rows[1].prediction_id, "p3");
        assert_eq!(rows[1].settled_at, None);
        // limit 0 clamps to 1, never a panic or full scan.
        assert_eq!(forward_recent(&db, None, 0).len(), 1);
    }
}
