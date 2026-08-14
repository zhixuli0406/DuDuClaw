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

use super::calibration::{murphy_decomposition, reliability_bins_equal_freq};
use super::task_forward::{TaskObservation, TaskPrediction};

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
    /// Compact "what was predicted / what actually happened" pair so the list
    /// answers it without opening the chain drill-down (2026-08-14 operator
    /// feedback: a row showing only category + Brier reads as a black box).
    pub expected_outcome: Option<String>,
    pub observed_outcome: Option<String>,
    pub expected_artifact: Option<String>,
    pub observed_artifact: Option<String>,
    /// Task board title resolved by the RPC handler (this module reads only
    /// `prediction.db`); `None` when the task row is gone.
    pub task_title: Option<String>,
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
///
/// Unlike the summary fold this runs its own bounded query INCLUDING the two
/// JSON blobs (≤[`RECENT_MAX_LIMIT`] rows), parsing out just the compact
/// expected/observed pair — the 5000-row summary scan deliberately keeps
/// skipping them.
pub fn forward_recent(
    db_path: &Path,
    agent_filter: Option<&str>,
    limit: usize,
) -> Vec<ForwardRecentRow> {
    let capped = limit.clamp(1, RECENT_MAX_LIMIT);
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = Connection::open(db_path) else {
        return Vec::new();
    };
    let base = "SELECT prediction_id, task_id, agent_id, round, prediction_source,
                       fidelity, category, brier_score, composite_error,
                       created_at, settled_at, prediction_json, observation_json
                FROM task_prediction_log";
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(ScannedRow, Option<String>, Option<String>)> {
        Ok((
            ScannedRow {
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
            },
            row.get(11)?,
            row.get(12)?,
        ))
    };
    let result = match agent_filter {
        Some(agent) => {
            let sql = format!("{base} WHERE agent_id = ?1 ORDER BY id DESC LIMIT ?2");
            conn.prepare(&sql).and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![agent, capped as i64], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        }
        None => {
            let sql = format!("{base} ORDER BY id DESC LIMIT ?1");
            conn.prepare(&sql).and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![capped as i64], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        }
    };
    result
        .unwrap_or_default()
        .into_iter()
        .map(|(r, pred_json, obs_json)| {
            // Same typed structs the chain view parses; a malformed blob just
            // yields `None` sides (fail-open, like everything in this module).
            let pred = pred_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<TaskPrediction>(s).ok());
            let obs = obs_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<TaskObservation>(s).ok());
            ForwardRecentRow {
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
                expected_outcome: pred
                    .as_ref()
                    .map(|p| p.expected_outcome.as_str().to_string()),
                expected_artifact: pred
                    .as_ref()
                    .map(|p| p.expected_artifact.as_str().to_string()),
                observed_outcome: obs
                    .as_ref()
                    .map(|o| o.observed_outcome.as_str().to_string()),
                observed_artifact: obs
                    .as_ref()
                    .map(|o| o.observed_artifact.as_str().to_string()),
                task_title: None,
            }
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════
// Full predict→act→observe→score chain for one task (dashboard drill-down)
// ═══════════════════════════════════════════════════════════════════════

/// Expected side of one chain round, parsed out of `prediction_json`.
#[derive(Debug, Clone, Serialize)]
pub struct ChainExpected {
    pub tool_classes: Vec<String>,
    pub call_band: (u32, u32),
    pub outcome: String,
    pub artifact: String,
    pub confidence: f64,
}

/// Observed side of one chain round, parsed out of `observation_json`.
#[derive(Debug, Clone, Serialize)]
pub struct ChainObserved {
    pub tool_classes: Vec<String>,
    pub calls: u32,
    pub errors: u32,
    pub outcome: String,
    pub artifact: String,
    pub fidelity: String,
    pub window_start: String,
    pub window_end: String,
}

/// Per-dimension error breakdown persisted at settle (`error_json`).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChainErrorBreakdown {
    pub tool_set_error: f64,
    pub volume_error: f64,
    pub outcome_error: f64,
    pub outcome_error_applicable: bool,
    pub artifact_error: f64,
    #[serde(default)]
    pub eligible_for_stats: bool,
}

/// One round of a task's forward-model loop: prediction → (observation +
/// score once settled). `expected`/`observed` are `None` when the stored
/// JSON is missing or unparsable — the row still renders from its scalar
/// columns rather than disappearing.
#[derive(Debug, Clone, Serialize)]
pub struct ForwardChainRound {
    pub prediction_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub round: u32,
    pub source: String,
    pub state_key: String,
    pub created_at: String,
    pub settled_at: Option<String>,
    pub expected: Option<ChainExpected>,
    pub observed: Option<ChainObserved>,
    pub fidelity: Option<String>,
    pub category: Option<String>,
    pub composite_error: Option<f64>,
    pub brier: Option<f64>,
    /// Which dimension the prediction missed on — `None` for legacy rows
    /// settled before `error_json` existed.
    pub error_breakdown: Option<ChainErrorBreakdown>,
}

fn tool_classes_to_strings(set: &std::collections::BTreeSet<super::tool_class::ToolClass>) -> Vec<String> {
    set.iter()
        .filter_map(|t| serde_json::to_value(t).ok())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// All rounds of one task's forward-model loop, oldest round first. Any
/// failure ⇒ empty (fail-open, same posture as the other views).
pub fn forward_chain(db_path: &Path, task_id: &str) -> Vec<ForwardChainRound> {
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = Connection::open(db_path) else {
        return Vec::new();
    };
    let sql = "SELECT prediction_id, task_id, agent_id, round, prediction_source,
                      state_key, created_at, settled_at, prediction_json,
                      observation_json, fidelity, category, composite_error,
                      brier_score, error_json
               FROM task_prediction_log
               WHERE task_id = ?1
               ORDER BY round ASC";
    let result = conn.prepare(sql).and_then(|mut stmt| {
        stmt.query_map(rusqlite::params![task_id], |row| {
            let prediction_json: Option<String> = row.get(8)?;
            let observation_json: Option<String> = row.get(9)?;
            let expected = prediction_json
                .as_deref()
                .and_then(|j| serde_json::from_str::<TaskPrediction>(j).ok())
                .map(|p| ChainExpected {
                    tool_classes: tool_classes_to_strings(&p.expected_tool_classes),
                    call_band: p.expected_call_band,
                    outcome: p.expected_outcome.as_str().to_string(),
                    artifact: p.expected_artifact.as_str().to_string(),
                    confidence: p.confidence,
                });
            let observed = observation_json
                .as_deref()
                .and_then(|j| serde_json::from_str::<TaskObservation>(j).ok())
                .map(|o| ChainObserved {
                    tool_classes: tool_classes_to_strings(&o.observed_tool_classes),
                    calls: o.observed_calls,
                    errors: o.observed_errors,
                    outcome: o.observed_outcome.as_str().to_string(),
                    artifact: o.observed_artifact.as_str().to_string(),
                    fidelity: o.fidelity.as_str().to_string(),
                    window_start: o.window.0.clone(),
                    window_end: o.window.1.clone(),
                });
            let error_breakdown = row
                .get::<_, Option<String>>(14)?
                .as_deref()
                .and_then(|j| serde_json::from_str::<ChainErrorBreakdown>(j).ok());
            Ok(ForwardChainRound {
                prediction_id: row.get(0)?,
                task_id: row.get(1)?,
                agent_id: row.get(2)?,
                round: row.get::<_, i64>(3)?.max(0) as u32,
                source: row.get(4)?,
                state_key: row.get(5)?,
                created_at: row.get(6)?,
                settled_at: row.get(7)?,
                expected,
                observed,
                fidelity: row.get(10)?,
                category: row.get(11)?,
                composite_error: row.get(12)?,
                brier: row.get(13)?,
                error_breakdown,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
    });
    result.unwrap_or_default()
}

// ═══════════════════════════════════════════════════════════════════════
// Calibration view — is this agent's prediction skill real?
// ═══════════════════════════════════════════════════════════════════════

/// Minimum settled-with-confidence samples before any skill verdict — below
/// this the label is always `candidate` (echoes `rule_gate`'s
/// MIN_HELD_OUT_SAMPLES posture: small-N verdicts are noise).
pub const CALIBRATION_MIN_SAMPLES: usize = 8;

/// Serialized reliability bin for the dashboard (mirrors
/// `calibration::ReliabilityBin`, which deliberately has no Serialize —
/// it is a pure-math type).
#[derive(Debug, Clone, Serialize)]
pub struct CalibrationBin {
    pub p_mean: f64,
    pub emp_rate: f64,
    pub n: usize,
}

/// Query-time calibration verdict over the scanned window. All statistics
/// are computed on the fly from `(confidence, correct)` pairs — nothing
/// here is stored, so an empty store yields an honest `candidate`.
///
/// `label` is three-state, per the §7 honesty doctrine (no "seems to
/// work"): `candidate` (n < [`CALIBRATION_MIN_SAMPLES`]),
/// `supported` (Brier skill score > 0 AND Murphy resolution > 0 — only
/// rising resolution counts as real skill, reliability alone can be
/// gamed by forecasting the base rate), else
/// `indistinguishable_from_luck`. The PSR gate of
/// `calibration::honest_label` is deliberately not applied — its
/// Sharpe-ratio inputs have no meaning for task-outcome predictions.
#[derive(Debug, Clone, Serialize)]
pub struct ForwardCalibrationView {
    pub agent_id: String,
    /// Settled rows whose stored prediction carried a parsable confidence.
    pub n: usize,
    pub hit_rate: Option<f64>,
    pub avg_brier: Option<f64>,
    /// 1 − brier/uncertainty; positive = beats always-forecasting-the-base-rate.
    pub brier_skill_score: Option<f64>,
    pub reliability: Option<f64>,
    pub resolution: Option<f64>,
    pub uncertainty: Option<f64>,
    pub bins: Vec<CalibrationBin>,
    /// `supported` | `candidate` | `indistinguishable_from_luck`
    pub label: String,
}

/// Compute the calibration view for one agent (bounded window scan).
pub fn forward_calibration(db_path: &Path, agent_id: &str) -> ForwardCalibrationView {
    let empty = |label: &str| ForwardCalibrationView {
        agent_id: agent_id.to_string(),
        n: 0,
        hit_rate: None,
        avg_brier: None,
        brier_skill_score: None,
        reliability: None,
        resolution: None,
        uncertainty: None,
        bins: Vec::new(),
        label: label.to_string(),
    };
    if !db_path.exists() {
        return empty("candidate");
    }
    let Ok(conn) = Connection::open(db_path) else {
        return empty("candidate");
    };
    let sql = "SELECT prediction_json, category FROM task_prediction_log
               WHERE agent_id = ?1 AND settled_at IS NOT NULL
               ORDER BY id DESC LIMIT ?2";
    let rows: Vec<(Option<String>, Option<String>)> = conn
        .prepare(sql)
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params![agent_id, SCAN_CAP as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();

    // (confidence, correct) — correct ⇔ Negligible/Moderate, the same rule
    // `task_forward::task_prediction_correct` applies at settle time.
    let samples: Vec<(f64, bool)> = rows
        .into_iter()
        .filter_map(|(pj, category)| {
            let conf = pj
                .as_deref()
                .and_then(|j| serde_json::from_str::<TaskPrediction>(j).ok())
                .map(|p| p.confidence)?;
            let correct = matches!(category.as_deref(), Some("negligible") | Some("moderate"));
            Some((conf, correct))
        })
        .collect();

    let n = samples.len();
    if n == 0 {
        return empty("candidate");
    }
    let hits = samples.iter().filter(|(_, c)| *c).count();
    let hit_rate = hits as f64 / n as f64;
    let avg_brier = samples
        .iter()
        .map(|(conf, correct)| super::calibration::brier_binary(*conf, *correct))
        .sum::<f64>()
        / n as f64;
    let murphy = murphy_decomposition(&samples, 3);
    let finite = |x: f64| if x.is_finite() { Some(x) } else { None };
    let bss = finite(murphy.uncertainty)
        .filter(|u| *u > 0.0)
        .map(|u| 1.0 - avg_brier / u);
    let bins = reliability_bins_equal_freq(&samples, 3)
        .into_iter()
        .map(|b| CalibrationBin { p_mean: b.p_mean, emp_rate: b.emp_rate, n: b.n })
        .collect();
    let label = if n < CALIBRATION_MIN_SAMPLES {
        "candidate"
    } else if bss.is_some_and(|s| s > 0.0) && finite(murphy.resolution).is_some_and(|r| r > 0.0) {
        "supported"
    } else {
        "indistinguishable_from_luck"
    };
    ForwardCalibrationView {
        agent_id: agent_id.to_string(),
        n,
        hit_rate: Some(hit_rate),
        avg_brier: Some(avg_brier),
        brier_skill_score: bss,
        reliability: finite(murphy.reliability),
        resolution: finite(murphy.resolution),
        uncertainty: finite(murphy.uncertainty),
        bins,
        label: label.to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// State models — what the world model has actually learned
// ═══════════════════════════════════════════════════════════════════════

/// One learned state bucket (`task_state_models` row). `state_key` is the
/// canonical `agent|goal_kind|phase|has_spec` string.
#[derive(Debug, Clone, Serialize)]
pub struct ForwardStateRow {
    pub state_key: String,
    pub agent_id: String,
    pub n_samples: u64,
    pub last_updated: String,
}

/// Learned state buckets, most-sampled first. Any failure ⇒ empty.
pub fn forward_states(db_path: &Path, agent_filter: Option<&str>, limit: usize) -> Vec<ForwardStateRow> {
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = Connection::open(db_path) else {
        return Vec::new();
    };
    let capped = limit.clamp(1, RECENT_MAX_LIMIT) as i64;
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ForwardStateRow> {
        Ok(ForwardStateRow {
            state_key: row.get(0)?,
            agent_id: row.get(1)?,
            n_samples: row.get::<_, i64>(2)?.max(0) as u64,
            last_updated: row.get(3)?,
        })
    };
    let base = "SELECT state_key, agent_id, n_samples, last_updated FROM task_state_models";
    let result = match agent_filter {
        Some(agent) => {
            let sql = format!("{base} WHERE agent_id = ?1 ORDER BY n_samples DESC LIMIT ?2");
            conn.prepare(&sql).and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![agent, capped], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        }
        None => {
            let sql = format!("{base} ORDER BY n_samples DESC LIMIT ?1");
            conn.prepare(&sql).and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![capped], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        }
    };
    result.unwrap_or_default()
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

    /// Real serde shapes for the chain fixture — built from the actual
    /// types so a field rename breaks this test, not production parsing.
    fn prediction_json_fixture(confidence: f64) -> String {
        use crate::prediction::task_forward::*;
        use crate::prediction::tool_class::ToolClass;
        let p = TaskPrediction {
            prediction_id: "p1".into(),
            task_id: "t1".into(),
            agent_id: "trader".into(),
            round: 1,
            state_key: TaskStateKey {
                agent_id: "trader".into(),
                goal_kind: GoalKind::CodingSimple,
                phase: RoundPhase::First,
                has_outcome_spec: false,
            },
            expected_tool_classes: [ToolClass::Read, ToolClass::Exec].into_iter().collect(),
            expected_call_band: (2, 8),
            expected_outcome: ExpectedOutcome::Accept,
            expected_artifact: ArtifactShape::TextOnly,
            confidence,
            source: PredictionSource::Statistical,
            created_at: chrono::Utc::now(),
        };
        serde_json::to_string(&p).unwrap()
    }

    fn observation_json_fixture() -> String {
        use crate::prediction::task_forward::*;
        use crate::prediction::tool_class::ToolClass;
        let o = TaskObservation {
            task_id: "t1".into(),
            agent_id: "trader".into(),
            round: 1,
            observed_tool_classes: [ToolClass::Read].into_iter().collect(),
            observed_calls: 5,
            observed_errors: 1,
            observed_outcome: ObservedOutcome::Accepted,
            observed_artifact: ArtifactShape::TextOnly,
            fidelity: ObservationFidelity::McpOnly,
            window: ("2026-08-13T01:00:00+00:00".into(), "2026-08-13T02:00:00+00:00".into()),
            runtime: "claude".into(),
        };
        serde_json::to_string(&o).unwrap()
    }

    #[test]
    fn chain_parses_expected_and_observed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        let _model = TaskForwardModel::new(db_path.clone());
        let conn = Connection::open(&db_path).unwrap();
        let insert = "INSERT INTO task_prediction_log
            (prediction_id, task_id, agent_id, round, state_key, prediction_json,
             prediction_source, created_at, observation_json, fidelity,
             composite_error, category, settled_at, brier_score)
            VALUES (?1, ?2, ?3, ?4, 'trader|coding_simple|first|0', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";
        // Round 2 inserted first — chain must still come back round-ordered.
        conn.execute(
            insert,
            rusqlite::params![
                "p2", "t1", "trader", 2, prediction_json_fixture(0.4), "prior",
                "2026-08-13T03:00:00+00:00", None::<String>, None::<String>,
                None::<f64>, None::<String>, None::<String>, None::<f64>
            ],
        )
        .unwrap();
        conn.execute(
            insert,
            rusqlite::params![
                "p1", "t1", "trader", 1, prediction_json_fixture(0.8), "statistical",
                "2026-08-13T01:00:00+00:00", observation_json_fixture(), "mcp_only",
                Some(0.1_f64), Some("negligible"), Some("2026-08-13T02:00:00+00:00"),
                Some(0.04_f64)
            ],
        )
        .unwrap();
        // Unparsable JSON degrades to scalar-only, never drops the row.
        conn.execute(
            insert,
            rusqlite::params![
                "p3", "t1", "trader", 3, "not json", "prior",
                "2026-08-13T05:00:00+00:00", None::<String>, None::<String>,
                None::<f64>, None::<String>, None::<String>, None::<f64>
            ],
        )
        .unwrap();

        // Settle-time per-dimension breakdown (error_json) parses through.
        conn.execute(
            "UPDATE task_prediction_log SET error_json = ?1 WHERE prediction_id = 'p1'",
            rusqlite::params![
                r#"{"tool_set_error":0.5,"volume_error":0.0,"outcome_error":0.0,"outcome_error_applicable":true,"artifact_error":0.0,"eligible_for_stats":true}"#
            ],
        )
        .unwrap();

        let chain = forward_chain(&db_path, "t1");
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].round, 1);
        let breakdown = chain[0].error_breakdown.as_ref().unwrap();
        assert!((breakdown.tool_set_error - 0.5).abs() < 1e-9);
        assert!(breakdown.outcome_error_applicable);
        assert!(chain[1].error_breakdown.is_none(), "unsettled row has no breakdown");
        let exp = chain[0].expected.as_ref().unwrap();
        assert_eq!(exp.tool_classes, vec!["read", "exec"]);
        assert_eq!(exp.call_band, (2, 8));
        assert_eq!(exp.outcome, "accept");
        assert!((exp.confidence - 0.8).abs() < 1e-9);
        let obs = chain[0].observed.as_ref().unwrap();
        assert_eq!(obs.tool_classes, vec!["read"]);
        assert_eq!(obs.calls, 5);
        assert_eq!(obs.outcome, "accepted");
        assert_eq!(obs.fidelity, "mcp_only");
        assert_eq!(chain[1].round, 2);
        assert!(chain[1].observed.is_none());
        assert_eq!(chain[2].round, 3);
        assert!(chain[2].expected.is_none(), "bad JSON must degrade, not drop");
        assert!(forward_chain(&db_path, "unknown-task").is_empty());
    }

    #[test]
    fn calibration_labels_are_honest() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        let _model = TaskForwardModel::new(db_path.clone());
        let conn = Connection::open(&db_path).unwrap();
        let insert = "INSERT INTO task_prediction_log
            (prediction_id, task_id, agent_id, round, state_key, prediction_json,
             prediction_source, created_at, observation_json, fidelity,
             composite_error, category, settled_at, brier_score)
            VALUES (?1, ?2, 'trader', 1, 'k', ?3, 'statistical',
                    '2026-08-13T01:00:00+00:00', '{}', 'mcp_only', 0.1, ?4,
                    '2026-08-13T02:00:00+00:00', 0.1)";
        // Empty store → candidate, all-None stats.
        let v = forward_calibration(&db_path, "trader");
        assert_eq!(v.label, "candidate");
        assert_eq!(v.n, 0);

        // 3 settled samples (< min 8) → still candidate, but stats present.
        for i in 0..3 {
            conn.execute(
                insert,
                rusqlite::params![
                    format!("cp{i}"),
                    format!("ct{i}"),
                    prediction_json_fixture(0.9),
                    "negligible"
                ],
            )
            .unwrap();
        }
        let v = forward_calibration(&db_path, "trader");
        assert_eq!(v.n, 3);
        assert_eq!(v.label, "candidate");
        assert_eq!(v.hit_rate, Some(1.0));
        assert!(v.avg_brier.unwrap() < 0.05);

        // 8+ mixed, well-separated samples → discriminative → supported.
        for i in 3..12 {
            let correct = i % 3 != 0;
            conn.execute(
                insert,
                rusqlite::params![
                    format!("cp{i}"),
                    format!("ct{i}"),
                    prediction_json_fixture(if correct { 0.95 } else { 0.05 }),
                    if correct { "negligible" } else { "critical" }
                ],
            )
            .unwrap();
        }
        let v = forward_calibration(&db_path, "trader");
        assert!(v.n >= 8);
        assert_eq!(v.label, "supported", "bss={:?} res={:?}", v.brier_skill_score, v.resolution);
        assert!(!v.bins.is_empty());
        // Unknown agent stays honest-empty.
        assert_eq!(forward_calibration(&db_path, "nobody").label, "candidate");
    }

    #[test]
    fn states_ranked_by_samples() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        let _model = TaskForwardModel::new(db_path.clone());
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO task_state_models (state_key, agent_id, model_json, n_samples, last_updated)
             VALUES ('trader|coding_simple|first|0', 'trader', '{}', 12, '2026-08-13T01:00:00+00:00'),
                    ('trader|research_or_qa|first|0', 'trader', '{}', 3, '2026-08-13T02:00:00+00:00'),
                    ('helper|coding_simple|first|0', 'helper', '{}', 7, '2026-08-13T03:00:00+00:00')",
            [],
        )
        .unwrap();
        let all = forward_states(&db_path, None, 10);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].n_samples, 12);
        let trader = forward_states(&db_path, Some("trader"), 10);
        assert_eq!(trader.len(), 2);
        assert!(trader.iter().all(|r| r.agent_id == "trader"));
        assert_eq!(forward_states(&db_path, None, 1).len(), 1);
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
