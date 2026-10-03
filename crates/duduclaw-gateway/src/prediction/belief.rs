//! Belief Loop — WP1 platform backend core.
//!
//! See `commercial/docs/DESIGN-market-belief-loop-2026-08.md` §2 (schema)
//! and §3 WP1 (this module). Domain-agnostic on purpose: any agent can
//! record a structured belief about ANY external subject (a ticker, an
//! index, a KPI…) and have it settled against a realized outcome later —
//! the platform doesn't know or care that the pilot use case is trading.
//! Parallel to (and independent of) the task-layer forward model in
//! `task_forward.rs` / `task_forward_store.rs`: that predicts what an agent
//! itself will DO; this records what an agent believes about the WORLD.
//!
//! Hard constraints baked in (design §0 — do not violate without updating
//! the design doc first):
//! - §0-1: settlement is a deterministic diff computed HERE from a stored
//!   `ref_value` + a caller-supplied `realized_value`, never left to the
//!   agent's own recollection (Honest Lying, arXiv:2605.29463 — freely
//!   reflecting agents score ~0% on catching their own past errors;
//!   programmatic extraction scores 86%).
//! - §0-3: small-sample discipline — `n_verified < `[`MIN_SETTLED_FOR_STATS`]
//!   (30) yields counts only in [`stats`]: no Wilson bound, no mean Brier, no
//!   overconfidence figure, and [`BeliefStats::calibration_status`] is not `calibrated`.
//! - Verified vs self-reported (fixed after v1.68.1): only a settlement whose
//!   `settle_source` is exactly [`SETTLE_SOURCE_TICK_VERIFIED`] counts toward
//!   any calibration figure. Every other value (the
//!   [`SETTLE_SOURCE_UNVERIFIED`] self-report, an unknown string, NULL on a
//!   legacy row) is a self-report and is counted separately. NOTE: as of
//!   this version NOTHING in production supplies a `tick_price` to
//!   [`settle`] (the `belief_settle` MCP tool, its only non-test caller,
//!   passes `None` and the MCP process has no TickHub access), so in a real
//!   deployment every settlement is a self-report and calibration reads
//!   "no verified settlements" until a gateway-side verified settle path
//!   exists.
//! - §0-5: the realized-value cross-check against a caller-supplied
//!   `tick_price` is a hard deterministic gate — divergence beyond
//!   [`TICK_CROSS_CHECK_TOLERANCE_PCT`] refuses settlement outright. Never an
//!   LLM judgment call (FinCon arXiv:2407.06567 ablation: a deterministic
//!   CVaR-style hard trigger beat narrative-based risk control by a wide
//!   margin; an LLM asked to adjudicate a self-reported number will rationalize
//!   it instead of rejecting it).
//!
//! Wilson lower bound is **not** reimplemented here — it reuses
//! [`super::calibration::wilson_bounds`], the exact math
//! [`super::rule_gate`] already uses for the held-out gate, so the platform
//! has exactly one statistical confidence-interval implementation. The
//! three-way proper score reuses [`super::calibration::rps3`] (ranked
//! probability score over the ordinal down/flat/up classes) rather than
//! inventing a second "belief Brier" formula.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::calibration::{rps3, wilson_bounds};

// ─────────────────────────────────────────────────────────────────────────
// Tunables
// ─────────────────────────────────────────────────────────────────────────

/// Below this many settled observations, [`stats`] returns counts only —
/// design §0-3 / §2: "settled < 30 筆只展示計數,不做任何自動化決策".
pub const MIN_SETTLED_FOR_STATS: u64 = 30;

/// `settle_source` written when the caller-supplied `tick_price` agreed with
/// `realized_value` within tolerance — the ONLY value that counts as a
/// cross-checked (verified) settlement.
pub const SETTLE_SOURCE_TICK_VERIFIED: &str = "agent+tick_verified";

/// `settle_source` written when no usable `tick_price` was supplied: the
/// agent's own report of the realized value, never checked by the platform.
pub const SETTLE_SOURCE_UNVERIFIED: &str = "agent_unverified";

/// Whether a stored `settle_source` marks a cross-checked settlement. Exact
/// equality with [`SETTLE_SOURCE_TICK_VERIFIED`]; `None` (legacy rows), the
/// empty string and any unknown value are all unverified (fail closed).
pub fn is_verified_source(settle_source: Option<&str>) -> bool {
    settle_source == Some(SETTLE_SOURCE_TICK_VERIFIED)
}

/// Two-sided 95% critical value for the Wilson interval on hit rate. Same
/// value `rule_gate::DEFAULT_BASE_Z` uses for the held-out gate; kept as an
/// independent literal (not imported) so this module's statistical surface
/// stays self-contained and doesn't silently move if the rule gate's base
/// rate changes for unrelated reasons.
const WILSON_Z: f64 = 1.96;

/// Default `|Δ| / ref_value < flat_band_pct` ⇒ realized direction is "flat"
/// (design §2), expressed as a percentage (`0.3` == 0.3%).
const DEFAULT_FLAT_BAND_PCT: f64 = 0.3;

/// Cross-check tolerance between the agent-reported `realized_value` and the
/// platform's own `tick_price` (design §2): beyond this, settlement is
/// refused rather than trusting the agent's self-report (§0-1, §0-5).
const TICK_CROSS_CHECK_TOLERANCE_PCT: f64 = 1.0;

/// Rationale is capped compact (ACE-style — design §2 schema comment: "≤400
/// chars(ACE 緊湊原則)").
const RATIONALE_MAX_CHARS: usize = 400;

/// Subject identifiers are short tickers/indices/KPI names ("2317", "TAIEX",
/// "trial_conversion_rate"), not prose — capped generously so a malformed
/// caller can't stuff an essay into what is also used as a join key against
/// tick payload field names (WP3 tick-wake injection).
const MAX_SUBJECT_CHARS: usize = 64;

/// Free-form `horizon` labels are capped (design §4: "自由標籤（≤40 字…)")
/// so a caller can't stuff an essay into what is meant to be a short
/// settlement-timing label like "今日收盤" / "本週五" / "明日 18:00".
const MAX_HORIZON_CHARS: usize = 40;

/// A `submit()` within this many hours of the agent's last injected
/// calibration-stats section is stamped `stats_injected = true` (design
/// §0-2 / §2: an evaluable experiment, not an assumed-effective mechanism —
/// every belief row records whether it followed a stats injection so the
/// two populations can be compared after the fact).
const STATS_INJECTED_WINDOW_HOURS: i64 = 12;

/// Hard cap for [`recent`]'s `limit` parameter (mirrors
/// `forward_view::RECENT_MAX_LIMIT`'s bounded-scan discipline).
pub const RECENT_MAX_LIMIT: usize = 200;

// ─────────────────────────────────────────────────────────────────────────
// Config — `config.toml [belief]`
// ─────────────────────────────────────────────────────────────────────────

/// `[belief]` config (design §3 WP5: `flat_band_pct = 0.3`; design §4:
/// optional `tick_subject_map` for explicit tick-field ↔ subject mapping).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct BeliefConfig {
    /// Percentage band (e.g. `0.3` = 0.3%) within which a realized value
    /// move is classified "flat" regardless of the raw sign of the change.
    pub flat_band_pct: f64,
    /// Explicit tick-field-name → subject mapping (design §4 / §6): key is
    /// the tick payload's `json_fields` key (e.g. `"conversion_rate"`),
    /// value is the belief `subject` it corresponds to (e.g.
    /// `"trial_conversion_rate"`). Consulted by the autopilot tick-wake
    /// hook (`autopilot_engine::belief_tick_field_name`) BEFORE the
    /// platform's `zXXXX → XXXX` naming convention — an explicit entry
    /// always wins, the convention is only a fallback for tick sources
    /// that already follow it.
    pub tick_subject_map: HashMap<String, String>,
}

impl Default for BeliefConfig {
    fn default() -> Self {
        Self {
            flat_band_pct: DEFAULT_FLAT_BAND_PCT,
            tick_subject_map: HashMap::new(),
        }
    }
}

impl BeliefConfig {
    /// Isolation parsing (same convention as `RecentActionsConfig::from_home`
    /// / `TaskForwardModelConfig::from_home`): a missing/malformed
    /// `config.toml`, or a missing/malformed `[belief]` section,
    /// always degrades to [`Self::default`] — never a hard error.
    pub fn from_home(home_dir: &Path) -> Self {
        let default = Self::default();
        let path = home_dir.join("config.toml");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return default;
        };
        let Ok(table) = raw.parse::<toml::Table>() else {
            return default;
        };
        let Some(section) = table.get("belief") else {
            return default;
        };
        section
            .clone()
            .try_into::<BeliefConfig>()
            .unwrap_or(default)
    }

    /// [`submit`] / [`settle`] only receive `db_path` (per the platform API
    /// contract dashboard/MCP callers depend on), not `home_dir`. Derives the
    /// home directory by swapping the filename — the same "same directory,
    /// different file" trick `cost_telemetry::resolve_prediction_db_path`
    /// already uses to go the other way (telemetry.db → prediction.db).
    fn from_db_path(db_path: &Path) -> Self {
        let home = db_path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_home(home)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Public data model (design §2)
// ─────────────────────────────────────────────────────────────────────────

/// One row of `belief_log`. Every field from the design §2 schema is
/// present and `pub` — the I2 dashboard line (WP4) depends on this exact
/// shape, so fields are additive-only going forward.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BeliefRow {
    pub belief_id: String,
    pub agent_id: String,
    pub subject: String,
    pub horizon: String,
    pub direction: String,
    pub prob: f64,
    pub rationale: Option<String>,
    pub ref_value: Option<f64>,
    pub predicted_at: String,
    pub stats_injected: bool,
    pub realized_value: Option<f64>,
    pub realized_direction: Option<String>,
    pub outcome: Option<String>,
    pub brier: Option<f64>,
    pub settled_at: Option<String>,
    pub settle_source: Option<String>,
    pub source_goal_id: Option<String>,
}

impl BeliefRow {
    /// True only for a settled row whose `settle_source` is a cross-checked
    /// one ([`is_verified_source`]).
    pub fn is_verified_settlement(&self) -> bool {
        self.settled_at.is_some() && is_verified_source(self.settle_source.as_deref())
    }
}

/// Verified (cross-checked) settlements of one subject. Raw counts only —
/// no per-subject Wilson bound (subject-level N is typically far below
/// [`MIN_SETTLED_FOR_STATS`], so a bound there would invite exactly the
/// small-sample overreach §0-3 forbids at the aggregate level).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubjectVerified {
    pub n: u64,
    pub hits: u64,
    /// `None` when `n == 0`.
    pub mean_brier: Option<f64>,
}

/// Self-reported (unverified) settlements of one subject: a count, nothing
/// that could be read as a score.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubjectSelfReported {
    pub n: u64,
}

/// Per-subject breakdown within [`BeliefStats`], split into the verified and
/// self-reported blocks. Proportions must use `verified.n` as denominator.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubjectStat {
    pub subject: String,
    pub verified: SubjectVerified,
    pub self_reported: SubjectSelfReported,
}

/// Whether [`BeliefStats`] carries calibration figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationStatus {
    /// Zero verified settlements — nothing calibrated exists. Every
    /// calibration field is `None`; this is not a score of 0 or 1.
    NoVerifiedSettlements,
    /// 1..[`MIN_SETTLED_FOR_STATS`] verified settlements: counts only.
    InsufficientSamples,
    /// At least [`MIN_SETTLED_FOR_STATS`] verified settlements.
    Calibrated,
}

/// Calibration figures over VERIFIED settlements only. Below
/// [`MIN_SETTLED_FOR_STATS`] verified rows only the counts (`n`, `hits`) are
/// filled; every derived figure is `None` (§0-3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerifiedStats {
    pub n: u64,
    pub hits: u64,
    /// `None` unless `calibration_status == Calibrated`.
    pub hit_rate: Option<f64>,
    /// Wilson 95% lower bound; same gating as `hit_rate`.
    pub hit_rate_wilson_low: Option<f64>,
    /// Mean three-way score; same gating.
    pub mean_brier: Option<f64>,
    /// `mean(prob) - hit_rate`: positive means the agent's stated confidence
    /// systematically outruns its actual hit rate; same gating.
    pub overconfidence: Option<f64>,
}

/// The agent's own unverified settlements (includes legacy rows with a
/// missing/empty/unknown `settle_source`). NOT calibration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelfReportedStats {
    pub n: u64,
    /// DESCRIPTIVE ONLY: share of these rows the agent reported as hits.
    /// `None` when `n == 0`. Never gated by sample size because it is never
    /// presented as a result.
    pub hit_rate: Option<f64>,
}

/// Per-agent summary (design §3 WP1 "stats 輸出"). Calibration lives only in
/// `verified`; `self_reported` is a count plus a descriptive rate. There are
/// deliberately no flat calibration fields, so a reader written against the
/// pre-split shape fails to type-check instead of showing a wrong rate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BeliefStats {
    pub agent_id: String,
    /// Every belief the agent has submitted, settled or not.
    pub n_submitted: u64,
    /// Every settled belief: `verified.n + self_reported.n`.
    pub n_settled_all: u64,
    pub calibration_status: CalibrationStatus,
    pub verified: VerifiedStats,
    pub self_reported: SelfReportedStats,
    pub per_subject: Vec<SubjectStat>,
}

/// Input to [`submit`]. Plain struct (not `Deserialize`) — callers (the
/// `belief_submit` MCP tool, WP2) build this by hand from validated JSON
/// args rather than deserializing directly, so a malformed extra JSON field
/// can never silently become a struct field via `#[serde(default)]` drift.
#[derive(Debug, Clone)]
pub struct NewBelief {
    pub agent_id: String,
    pub subject: String,
    pub horizon: String,
    pub direction: String,
    pub prob: f64,
    pub rationale: Option<String>,
    pub ref_value: Option<f64>,
    pub source_goal_id: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// Storage internals
// ─────────────────────────────────────────────────────────────────────────

/// Idempotent table creation (design §3 WP1: "沿用該 db 既有 idempotent
/// migration 模式" — same `CREATE TABLE IF NOT EXISTS` discipline
/// `task_forward_store::init_task_tables` uses on the same `prediction.db`
/// file). Deliberately called from every public entry point below rather
/// than from a one-time engine-boot hook: the `belief_submit` /
/// `belief_settle` / `belief_stats` MCP tools run in the separate
/// `duduclaw mcp-server` process, which never constructs a
/// `PredictionEngine` — mirroring `TaskStore::open`'s per-call
/// `init_schema`, not `TaskForwardModel::new`'s once-at-construction init.
fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS belief_log (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            belief_id           TEXT NOT NULL UNIQUE,
            agent_id            TEXT NOT NULL,
            subject             TEXT NOT NULL,
            horizon             TEXT NOT NULL,
            direction           TEXT NOT NULL,
            prob                REAL NOT NULL,
            rationale           TEXT,
            ref_value           REAL,
            predicted_at        TEXT NOT NULL,
            stats_injected      INTEGER NOT NULL DEFAULT 0,
            realized_value      REAL,
            realized_direction  TEXT,
            outcome             TEXT,
            brier               REAL,
            settled_at          TEXT,
            settle_source       TEXT,
            source_goal_id      TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_bl_agent_settled
            ON belief_log(agent_id, settled_at);
        CREATE INDEX IF NOT EXISTS idx_bl_agent_subject_date
            ON belief_log(agent_id, subject, predicted_at);

        CREATE TABLE IF NOT EXISTS belief_meta (
            key        TEXT PRIMARY KEY,
            value      TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )
    .map_err(|e| e.to_string())
}

fn open_conn(db_path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path).map_err(|e| format!("open prediction.db: {e}"))?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// Column list shared by every `SELECT` against `belief_log` so
/// [`map_row`] never has to guess positional order.
const SELECT_COLUMNS: &str = "belief_id, agent_id, subject, horizon, direction, prob, rationale, \
     ref_value, predicted_at, stats_injected, realized_value, realized_direction, outcome, brier, \
     settled_at, settle_source, source_goal_id";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BeliefRow> {
    Ok(BeliefRow {
        belief_id: row.get(0)?,
        agent_id: row.get(1)?,
        subject: row.get(2)?,
        horizon: row.get(3)?,
        direction: row.get(4)?,
        prob: row.get(5)?,
        rationale: row.get(6)?,
        ref_value: row.get(7)?,
        predicted_at: row.get(8)?,
        stats_injected: row.get::<_, i64>(9)? != 0,
        realized_value: row.get(10)?,
        realized_direction: row.get(11)?,
        outcome: row.get(12)?,
        brier: row.get(13)?,
        settled_at: row.get(14)?,
        settle_source: row.get(15)?,
        source_goal_id: row.get(16)?,
    })
}

/// Collapse internal whitespace/newlines to a single space — subject is
/// also used as an exact-match join key (WP3 tick-wake `zXXXX → XXXX`
/// matching), so it must never carry embedded newlines or run-on spaces.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Maps `"down"/"flat"/"up"` to the ordinal index [`rps3`] expects
/// (`0 = down, 1 = flat, 2 = up`). Any other string is not a valid
/// direction.
fn direction_idx(direction: &str) -> Option<usize> {
    match direction {
        "down" => Some(0),
        "flat" => Some(1),
        "up" => Some(2),
        _ => None,
    }
}

/// Build the three-way forecast vector for [`rps3`] (design §2: "申報方向=
/// prob、其餘均分 (1-prob)/2").
fn three_way_prob_vector(declared_direction: &str, prob: f64) -> Option<[f64; 3]> {
    let idx = direction_idx(declared_direction)?;
    let clamped = prob.clamp(0.0, 1.0);
    let other = (1.0 - clamped) / 2.0;
    let mut p = [other, other, other];
    p[idx] = clamped;
    Some(p)
}

/// design §0-2 / §2: whether `agent_id` had a calibration-stats section
/// injected into its prompt within the last `hours` hours
/// ([`belief_meta`]'s `last_stats_injected_at:<agent>` key, written by
/// [`mark_stats_injected`]). Missing key, unparsable timestamp, or any DB
/// error ⇒ `false` (fail-closed on the "was it injected" question — an
/// unattributed belief is safer than a falsely-attributed one for the A/B
/// bookkeeping this feeds).
fn last_stats_injected_within(conn: &Connection, agent_id: &str, hours: i64) -> bool {
    let key = format!("last_stats_injected_at:{agent_id}");
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM belief_meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(ts) = value else {
        return false;
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&ts) else {
        return false;
    };
    let elapsed_secs = Utc::now()
        .signed_duration_since(parsed.with_timezone(&Utc))
        .num_seconds();
    elapsed_secs < hours * 3600
}

// ─────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────

/// Record a new belief. Validation is fail-closed (design §3 WP2: "參數驗證
/// fail-closed"): a malformed `horizon`/`direction`/`prob`/`ref_value`
/// refuses the write with a descriptive error rather than coercing it to
/// something plausible-looking. Returns the newly minted `belief_id`.
pub fn submit(db_path: &Path, b: NewBelief) -> Result<String, String> {
    let agent_id = b.agent_id.trim();
    if agent_id.is_empty() {
        return Err("agent_id is required".to_string());
    }
    let subject = one_line(b.subject.trim());
    if subject.is_empty() {
        return Err("subject is required".to_string());
    }
    if subject.chars().count() > MAX_SUBJECT_CHARS {
        return Err(format!("subject must be <= {MAX_SUBJECT_CHARS} chars"));
    }
    let horizon = one_line(b.horizon.trim());
    if horizon.is_empty() {
        return Err("horizon is required".to_string());
    }
    let horizon = duduclaw_core::truncate_chars(&horizon, MAX_HORIZON_CHARS);
    if direction_idx(&b.direction).is_none() {
        return Err(format!(
            "direction must be 'up', 'down', or 'flat', got: {}",
            b.direction
        ));
    }
    if !b.prob.is_finite() || !(0.0..=1.0).contains(&b.prob) {
        return Err(format!("prob must be within [0,1], got: {}", b.prob));
    }
    if let Some(rv) = b.ref_value {
        if !rv.is_finite() || rv <= 0.0 {
            return Err("ref_value must be a positive finite number".to_string());
        }
    }
    let rationale = b
        .rationale
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| duduclaw_core::truncate_chars(s, RATIONALE_MAX_CHARS));

    let conn = open_conn(db_path)?;
    let belief_id = uuid::Uuid::new_v4().to_string();
    let predicted_at = Utc::now().to_rfc3339();
    let stats_injected = last_stats_injected_within(&conn, agent_id, STATS_INJECTED_WINDOW_HOURS);

    conn.execute(
        "INSERT INTO belief_log
         (belief_id, agent_id, subject, horizon, direction, prob, rationale,
          ref_value, predicted_at, stats_injected, source_goal_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            belief_id,
            agent_id,
            subject,
            horizon,
            b.direction,
            b.prob,
            rationale,
            b.ref_value,
            predicted_at,
            stats_injected as i64,
            b.source_goal_id,
        ],
    )
    .map_err(|e| format!("insert belief: {e}"))?;

    Ok(belief_id)
}

/// Settle a belief against a realized outcome (design §2 "結算規則").
///
/// Only a settlement that passes a `tick_price` cross-check is recorded as
/// [`SETTLE_SOURCE_TICK_VERIFIED`] and counts toward [`stats`] calibration;
/// without one it is [`SETTLE_SOURCE_UNVERIFIED`], a self-report.
///
/// Deterministic pipeline, zero LLM (§0-1):
/// 1. Look up the belief **scoped to `agent_id`** — a caller can only ever
///    settle its own beliefs, and a not-found/wrong-agent id gets the same
///    generic error (never leaks whether the id exists under someone else).
/// 2. Already-settled beliefs refuse a second settlement.
/// 3. `realized_direction` is `flat` when `|Δ%| < flat_band_pct`
///    ([`BeliefConfig`]), else the sign of the move.
/// 4. `outcome` is `"hit"` when the declared direction matches
///    `realized_direction`; `"flat_band"` when the observation landed inside
///    the flat band while the agent had called a real direction (an
///    undecidable "push", not a miss); `"miss"` otherwise.
/// 5. `brier` is [`rps3`] over the three-way forecast vector against the
///    realized ordinal class — an ordinal proper score, not a naive binary
///    hit/miss score, so calling "down" when "up" happened costs more than
///    calling "flat".
/// 6. When `tick_price` is supplied, `realized_value` must agree with it
///    within [`TICK_CROSS_CHECK_TOLERANCE_PCT`] or settlement is refused
///    outright (§0-5) and a `tracing::warn` records the rejection. A
///    malformed `tick_price` (non-finite / non-positive) is treated exactly
///    like "no tick data" — it is never trusted as a cross-check basis, only
///    ever a stricter gate, never a laxer one.
pub fn settle(
    db_path: &Path,
    agent_id: &str,
    belief_id: &str,
    realized_value: f64,
    tick_price: Option<f64>,
) -> Result<BeliefRow, String> {
    if !realized_value.is_finite() || realized_value <= 0.0 {
        return Err("realized_value must be a positive finite number".to_string());
    }
    let conn = open_conn(db_path)?;

    let existing = conn
        .query_row(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM belief_log \
                 WHERE belief_id = ?1 AND agent_id = ?2"
            ),
            params![belief_id, agent_id],
            map_row,
        )
        .optional()
        .map_err(|e| format!("query belief: {e}"))?;

    let Some(row) = existing else {
        return Err(format!("belief not found for this agent: {belief_id}"));
    };
    if row.settled_at.is_some() {
        return Err(format!("belief already settled: {belief_id}"));
    }
    let Some(ref_value) = row.ref_value else {
        return Err(format!(
            "belief {belief_id} has no ref_value recorded at submit time — cannot settle"
        ));
    };

    let cfg = BeliefConfig::from_db_path(db_path);
    let pct_change = (realized_value - ref_value) / ref_value * 100.0;
    let realized_direction = if pct_change.abs() < cfg.flat_band_pct {
        "flat"
    } else if pct_change > 0.0 {
        "up"
    } else {
        "down"
    };

    let outcome = if realized_direction == "flat" {
        if row.direction == "flat" {
            "hit"
        } else {
            "flat_band"
        }
    } else if row.direction == realized_direction {
        "hit"
    } else {
        "miss"
    };

    let realized_idx = direction_idx(realized_direction)
        .expect("realized_direction is always one of down/flat/up by construction above");
    let prob_vector = three_way_prob_vector(&row.direction, row.prob).ok_or_else(|| {
        format!(
            "belief {belief_id} has an invalid stored direction: {}",
            row.direction
        )
    })?;
    let brier = rps3(prob_vector, realized_idx);

    let settle_source = match tick_price {
        Some(tp) if tp.is_finite() && tp > 0.0 => {
            let diverge_pct = ((realized_value - tp).abs() / tp) * 100.0;
            if diverge_pct > TICK_CROSS_CHECK_TOLERANCE_PCT {
                warn!(
                    belief_id,
                    agent_id,
                    realized_value,
                    tick_price = tp,
                    diverge_pct,
                    tolerance_pct = TICK_CROSS_CHECK_TOLERANCE_PCT,
                    "belief: settlement refused — realized_value diverges from tick_price beyond tolerance"
                );
                return Err(format!(
                    "realized_value {realized_value:.4} diverges from tick_price {tp:.4} by \
                     {diverge_pct:.2}% (> {TICK_CROSS_CHECK_TOLERANCE_PCT}% tolerance) — \
                     settlement refused"
                ));
            }
            SETTLE_SOURCE_TICK_VERIFIED
        }
        // No tick data, or a malformed value that can't be trusted as a
        // cross-check basis — either way this is an unverified self-report.
        _ => SETTLE_SOURCE_UNVERIFIED,
    };

    let settled_at = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE belief_log
         SET realized_value = ?1, realized_direction = ?2, outcome = ?3, brier = ?4,
             settled_at = ?5, settle_source = ?6
         WHERE belief_id = ?7",
        params![
            realized_value,
            realized_direction,
            outcome,
            brier,
            settled_at,
            settle_source,
            belief_id,
        ],
    )
    .map_err(|e| format!("update belief: {e}"))?;

    conn.query_row(
        &format!("SELECT {SELECT_COLUMNS} FROM belief_log WHERE belief_id = ?1"),
        params![belief_id],
        map_row,
    )
    .map_err(|e| format!("re-read settled belief: {e}"))
}

/// List recent beliefs, newest first. Fail-open: a missing/corrupt db or any
/// query failure yields an empty list, never an error surfaced to the
/// dashboard/MCP caller (design §3 WP1: "recent: fail-open 空表" — same
/// posture as `forward_view::scan_rows`). `limit == 0` yields an empty list
/// without touching the db; any other value is clamped to
/// [`RECENT_MAX_LIMIT`].
pub fn recent(db_path: &Path, agent: Option<&str>, limit: usize) -> Vec<BeliefRow> {
    if limit == 0 {
        return Vec::new();
    }
    let limit = limit.min(RECENT_MAX_LIMIT) as i64;
    let Ok(conn) = open_conn(db_path) else {
        return Vec::new();
    };

    let result = match agent {
        Some(a) => (|| -> rusqlite::Result<Vec<BeliefRow>> {
            let sql = format!(
                "SELECT {SELECT_COLUMNS} FROM belief_log \
                 WHERE agent_id = ?1 ORDER BY id DESC LIMIT ?2"
            );
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(params![a, limit], map_row)?.collect()
        })(),
        None => (|| -> rusqlite::Result<Vec<BeliefRow>> {
            let sql = format!("SELECT {SELECT_COLUMNS} FROM belief_log ORDER BY id DESC LIMIT ?1");
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(params![limit], map_row)?.collect()
        })(),
    };
    result.unwrap_or_default()
}

/// Per-agent calibration summary (design §3 WP1). Fail-open: any db/query
/// failure yields the all-zero, `no_verified_settlements` shape — same
/// posture as [`recent`], never an error to the caller.
///
/// Calibration figures use verified settlements only (see [`BeliefStats`]).
pub fn stats(db_path: &Path, agent: &str) -> BeliefStats {
    let empty = || BeliefStats {
        agent_id: agent.to_string(),
        n_submitted: 0,
        n_settled_all: 0,
        calibration_status: CalibrationStatus::NoVerifiedSettlements,
        verified: VerifiedStats {
            n: 0,
            hits: 0,
            hit_rate: None,
            hit_rate_wilson_low: None,
            mean_brier: None,
            overconfidence: None,
        },
        self_reported: SelfReportedStats {
            n: 0,
            hit_rate: None,
        },
        per_subject: Vec::new(),
    };
    let Ok(conn) = open_conn(db_path) else {
        return empty();
    };

    let n_total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM belief_log WHERE agent_id = ?1",
            params![agent],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // A NULL/empty/unknown `settle_source` never equals the verified literal,
    // so legacy rows fall on the self-reported side.
    let settled_agg = conn
        .query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN COALESCE(settle_source, '') = ?2 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN COALESCE(settle_source, '') = ?2 AND outcome = 'hit' THEN 1 ELSE 0 END),
                    AVG(CASE WHEN COALESCE(settle_source, '') = ?2 THEN brier END),
                    AVG(CASE WHEN COALESCE(settle_source, '') = ?2 THEN prob END),
                    SUM(CASE WHEN COALESCE(settle_source, '') <> ?2 AND outcome = 'hit' THEN 1 ELSE 0 END)
             FROM belief_log
             WHERE agent_id = ?1 AND settled_at IS NOT NULL",
            params![agent, SETTLE_SOURCE_TICK_VERIFIED],
            |r| {
                let n: i64 = r.get(0)?;
                let n_verified: Option<i64> = r.get(1)?;
                let hits: Option<i64> = r.get(2)?;
                let mean_brier: Option<f64> = r.get(3)?;
                let mean_prob: Option<f64> = r.get(4)?;
                let self_hits: Option<i64> = r.get(5)?;
                Ok((
                    n,
                    n_verified.unwrap_or(0),
                    hits.unwrap_or(0),
                    mean_brier,
                    mean_prob,
                    self_hits.unwrap_or(0),
                ))
            },
        )
        .optional()
        .unwrap_or(None);

    let Some((n_settled, n_verified, hits, mean_brier_raw, mean_prob_raw, self_hits)) =
        settled_agg
    else {
        return empty();
    };

    let per_subject = subject_breakdown(&conn, agent);
    let n_settled_u64 = n_settled.max(0) as u64;
    let n_verified_u64 = (n_verified.max(0) as u64).min(n_settled_u64);
    let n_self_u64 = n_settled_u64 - n_verified_u64;
    let hits_u64 = (hits.max(0) as u64).min(n_verified_u64);
    let self_reported = SelfReportedStats {
        n: n_self_u64,
        hit_rate: if n_self_u64 > 0 {
            Some(self_hits.max(0) as f64 / n_self_u64 as f64)
        } else {
            None
        },
    };
    let n_submitted = n_total.max(0) as u64;

    if n_verified_u64 < MIN_SETTLED_FOR_STATS {
        return BeliefStats {
            agent_id: agent.to_string(),
            n_submitted,
            n_settled_all: n_settled_u64,
            calibration_status: if n_verified_u64 == 0 {
                CalibrationStatus::NoVerifiedSettlements
            } else {
                CalibrationStatus::InsufficientSamples
            },
            verified: VerifiedStats {
                n: n_verified_u64,
                hits: hits_u64,
                hit_rate: None,
                hit_rate_wilson_low: None,
                mean_brier: None,
                overconfidence: None,
            },
            self_reported,
            per_subject,
        };
    }

    let hit_rate = hits_u64 as f64 / n_verified_u64 as f64;
    let (wilson_lo, _) = wilson_bounds(hits_u64, n_verified_u64, WILSON_Z);
    let overconfidence = mean_prob_raw.map(|mp| mp - hit_rate);

    BeliefStats {
        agent_id: agent.to_string(),
        n_submitted,
        n_settled_all: n_settled_u64,
        calibration_status: CalibrationStatus::Calibrated,
        verified: VerifiedStats {
            n: n_verified_u64,
            hits: hits_u64,
            hit_rate: Some(hit_rate),
            hit_rate_wilson_low: if wilson_lo.is_nan() {
                None
            } else {
                Some(wilson_lo)
            },
            mean_brier: mean_brier_raw,
            overconfidence,
        },
        self_reported,
        per_subject,
    }
}

fn subject_breakdown(conn: &Connection, agent: &str) -> Vec<SubjectStat> {
    let query = || -> rusqlite::Result<Vec<SubjectStat>> {
        let mut stmt = conn.prepare(
            "SELECT subject, COUNT(*),
                    SUM(CASE WHEN COALESCE(settle_source, '') = ?2 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN COALESCE(settle_source, '') = ?2 AND outcome = 'hit' THEN 1 ELSE 0 END),
                    AVG(CASE WHEN COALESCE(settle_source, '') = ?2 THEN brier END)
             FROM belief_log
             WHERE agent_id = ?1 AND settled_at IS NOT NULL
             GROUP BY subject
             ORDER BY subject",
        )?;
        stmt.query_map(params![agent, SETTLE_SOURCE_TICK_VERIFIED], |r| {
            let subject: String = r.get(0)?;
            let n: i64 = r.get(1)?;
            let n_verified: Option<i64> = r.get(2)?;
            let hits: Option<i64> = r.get(3)?;
            let mean_brier: Option<f64> = r.get(4)?;
            let n_settled = n.max(0) as u64;
            let n_verified = (n_verified.unwrap_or(0).max(0) as u64).min(n_settled);
            Ok(SubjectStat {
                subject,
                verified: SubjectVerified {
                    n: n_verified,
                    hits: (hits.unwrap_or(0).max(0) as u64).min(n_verified),
                    mean_brier,
                },
                self_reported: SubjectSelfReported {
                    n: n_settled - n_verified,
                },
            })
        })?
        .collect()
    };
    query().unwrap_or_default()
}

/// Stamp `belief_meta`'s `last_stats_injected_at:<agent>` marker (design §3
/// WP3: "注入同時 upsert belief_meta 的 last_stats_injected_at"). Called by
/// the pre-market goal-loop injection hook the moment it actually renders a
/// calibration section — never speculatively, so the marker only ever
/// reflects prompts that really carried the section.
pub fn mark_stats_injected(db_path: &Path, agent_id: &str) -> Result<(), String> {
    let conn = open_conn(db_path)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO belief_meta (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![format!("last_stats_injected_at:{agent_id}"), now, now],
    )
    .map_err(|e| format!("mark_stats_injected: {e}"))?;
    Ok(())
}

/// Generic `belief_meta` key-value read. Thin public wrapper so callers
/// outside this module (design §3 「自主研究」 — `self_study.rs`'s
/// once-per-local-day claim marker) can persist a small marker in the same
/// table [`mark_stats_injected`] already uses, instead of inventing a
/// second key-value store. Missing key or any DB error ⇒ `None`
/// (fail-open read, same posture as [`last_stats_injected_within`]).
pub fn get_meta(db_path: &Path, key: &str) -> Option<String> {
    let conn = open_conn(db_path).ok()?;
    conn.query_row(
        "SELECT value FROM belief_meta WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Generic `belief_meta` key-value upsert. Sibling of [`get_meta`]; same
/// upsert shape [`mark_stats_injected`] uses.
pub fn set_meta(db_path: &Path, key: &str, value: &str) -> Result<(), String> {
    let conn = open_conn(db_path)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO belief_meta (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now],
    )
    .map_err(|e| format!("set_meta: {e}"))?;
    Ok(())
}

/// Today's (UTC, by `predicted_at` date) unsettled beliefs for `agent_id`,
/// oldest first — the WP3 tick-wake injection hook's query surface (design
/// §3: "該 subject 當日未結 belief 存在"). Fail-open empty on any db/query
/// failure, same posture as [`recent`] / [`stats`].
pub fn unsettled_today(db_path: &Path, agent_id: &str) -> Vec<BeliefRow> {
    let Ok(conn) = open_conn(db_path) else {
        return Vec::new();
    };
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let query = || -> rusqlite::Result<Vec<BeliefRow>> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM belief_log \
             WHERE agent_id = ?1 AND settled_at IS NULL AND substr(predicted_at, 1, 10) = ?2 \
             ORDER BY id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_map(params![agent_id, today], map_row)?.collect()
    };
    query().unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────
// WP3 prompt rendering (pure — no I/O). Format mirrors the existing
// `## 工作狀態` / `## 近期自身行動` injected-section convention (plain
// markdown headers, zh-TW body, no XML escaping needed since these are not
// wrapped in an XML `<state>` block like `goal_loop/state.rs`'s A1 blocks are).
// ─────────────────────────────────────────────────────────────────────────

/// Render the pre-dispatch `## 信念校準（程式化統計，勿自行臆測歷史）`
/// dispatch-prompt section for a [`stats`] result (design §3 WP3 / §4). `None`
/// when there is nothing calibrated to say yet (`verified.n == 0` — design: "無資料 →
/// 零注入"); the caller decides whether to also call [`mark_stats_injected`]
/// once this actually gets used in a prompt.
///
/// Only verified settlements are ever phrased as a calibration result; the
/// sample-size gate is on `n_verified`. Self-reported settlements are named
/// as such and never summarised as a score.
pub fn render_calibration_section(stats: &BeliefStats) -> Option<String> {
    // No verified settlement ⇒ nothing calibrated to say ⇒ zero injection.
    // (The caller stamps `stats_injected` on injection, an experiment flag
    // that must not be set for a prompt without any calibration figure; the
    // agent learns a settlement is unverified from the `belief_settle`
    // response itself.)
    if stats.verified.n == 0 {
        return None;
    }
    let self_note = if stats.self_reported.n > 0 {
        format!(
            "\n另有 {} 筆為你自行回報、未經交叉驗證的結算，不計入以上任何數字。",
            stats.self_reported.n
        )
    } else {
        String::new()
    };
    let body = if stats.calibration_status != CalibrationStatus::Calibrated {
        format!(
            "已驗證結算 {} 筆，未達 {MIN_SETTLED_FOR_STATS} 筆統計門檻 — 樣本不足，\
             目前只提供計數，不做任何命中率或校準判斷。{self_note}",
            stats.verified.n
        )
    } else {
        let v = &stats.verified;
        let hit_pct = v.hit_rate.unwrap_or(f64::NAN) * 100.0;
        let wilson_pct = v.hit_rate_wilson_low.unwrap_or(f64::NAN) * 100.0;
        let brier = v.mean_brier.unwrap_or(f64::NAN);
        let overconf = v.overconfidence.unwrap_or(f64::NAN);
        let overconf_note = if overconf > 0.05 {
            "你宣告的信心持續高於實際命中率，宣告機率時應更保守"
        } else if overconf < -0.05 {
            "你宣告的信心持續低於實際命中率，你的判斷比自己以為的更準"
        } else {
            "宣告信心與實際命中率大致相符"
        };
        format!(
            "已驗證結算 {n} 筆。\n\
             - 命中率（Wilson 95% 下界，保守估計）：{wilson_pct:.0}%（實際命中率有 95% 信心不低於此值；\
             原始命中率 {hit_pct:.0}%）。\n\
             - 平均校準分數（三向 Brier，範圍 0-1，越低代表方向判斷越準）：{brier:.3}。\n\
             - 過度自信指標（宣告機率 − 實際命中率）：{overconf:+.2}（{overconf_note}）。{self_note}",
            n = v.n,
        )
    };
    Some(format!("## 信念校準（程式化統計，勿自行臆測歷史）\n{body}"))
}

/// Render one `## 信念對照` diff line for an unsettled belief against a live
/// tick value (design §3 WP3 tick-wake hook). `None` when the belief carries
/// no `ref_value` (nothing to diff against) or either value is non-finite.
pub fn render_tick_diff_line(row: &BeliefRow, tick_value: f64) -> Option<String> {
    let ref_value = row.ref_value?;
    if !ref_value.is_finite() || ref_value == 0.0 || !tick_value.is_finite() {
        return None;
    }
    let pct_change = (tick_value - ref_value) / ref_value * 100.0;
    let arrow = if pct_change > 0.0 {
        "▲"
    } else if pct_change < 0.0 {
        "▼"
    } else {
        "→"
    };
    Some(format!(
        "你今日申報 {subject} {direction} 信心 {prob:.0}%；判斷基準值 {ref_value:.4} → 現值 {tick_value:.4}\
         （{arrow} {pct_change:+.2}%）",
        subject = &row.subject,
        direction = &row.direction,
        prob = row.prob * 100.0,
    ))
}

/// Render the whole `## 信念對照` section from matched `(belief, tick_value)`
/// pairs (design: "多筆全列"). `None` when there is nothing to show —
/// callers should treat that as zero injection, never an empty header.
pub fn render_tick_diff_section(pairs: &[(BeliefRow, f64)]) -> Option<String> {
    if pairs.is_empty() {
        return None;
    }
    let lines: Vec<String> = pairs
        .iter()
        .filter_map(|(row, tick_value)| render_tick_diff_line(row, *tick_value))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!("## 信念對照\n{}", lines.join("\n")))
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        (db_path, dir)
    }

    fn belief(agent: &str, subject: &str, direction: &str, prob: f64, ref_value: f64) -> NewBelief {
        NewBelief {
            agent_id: agent.to_string(),
            subject: subject.to_string(),
            horizon: "今日收盤".to_string(),
            direction: direction.to_string(),
            prob,
            rationale: Some("test rationale".to_string()),
            ref_value: Some(ref_value),
            source_goal_id: None,
        }
    }

    // ── submit: validation ──

    #[test]
    fn submit_rejects_empty_horizon() {
        let (db, _dir) = temp_db();
        let mut b = belief("trader", "2317", "up", 0.6, 100.0);
        b.horizon = "   ".to_string();
        let err = submit(&db, b).unwrap_err();
        assert!(err.contains("horizon"), "{err}");
    }

    #[test]
    fn submit_truncates_long_horizon_to_free_label_cap() {
        let (db, _dir) = temp_db();
        let mut b = belief("trader", "2317", "up", 0.6, 100.0);
        b.horizon = "x".repeat(1000);
        let id = submit(&db, b).unwrap();
        let row = recent(&db, Some("trader"), 10)
            .into_iter()
            .find(|r| r.belief_id == id)
            .unwrap();
        assert_eq!(row.horizon.chars().count(), 40);
    }

    #[test]
    fn submit_accepts_arbitrary_free_form_horizon_labels() {
        let (db, _dir) = temp_db();
        for label in ["本週五", "明日 18:00", "next_week", "Q3 review"] {
            let mut b = belief("trader", "2317", "up", 0.6, 100.0);
            b.horizon = label.to_string();
            let id = submit(&db, b).unwrap();
            let row = recent(&db, Some("trader"), 10)
                .into_iter()
                .find(|r| r.belief_id == id)
                .unwrap();
            assert_eq!(row.horizon, label);
        }
    }

    #[test]
    fn submit_rejects_invalid_direction() {
        let (db, _dir) = temp_db();
        let mut b = belief("trader", "2317", "sideways", 0.6, 100.0);
        b.direction = "sideways".to_string();
        let err = submit(&db, b).unwrap_err();
        assert!(err.contains("direction"), "{err}");
    }

    #[test]
    fn submit_rejects_prob_out_of_range() {
        let (db, _dir) = temp_db();
        let b = belief("trader", "2317", "up", 1.5, 100.0);
        let err = submit(&db, b).unwrap_err();
        assert!(err.contains("prob"), "{err}");
    }

    #[test]
    fn submit_rejects_empty_subject_and_agent() {
        let (db, _dir) = temp_db();
        let b = belief("trader", "   ", "up", 0.6, 100.0);
        assert!(submit(&db, b).is_err());
        let mut b2 = belief("", "2317", "up", 0.6, 100.0);
        b2.agent_id = "  ".to_string();
        assert!(submit(&db, b2).is_err());
    }

    #[test]
    fn submit_rejects_non_positive_ref_value() {
        let (db, _dir) = temp_db();
        let b = belief("trader", "2317", "up", 0.6, -1.0);
        assert!(submit(&db, b).is_err());
    }

    #[test]
    fn submit_truncates_long_rationale_and_collapses_subject_whitespace() {
        let (db, _dir) = temp_db();
        let mut b = belief("trader", "  2317   TW  ", "up", 0.6, 100.0);
        b.rationale = Some("x".repeat(1000));
        let id = submit(&db, b).unwrap();
        let row = recent(&db, Some("trader"), 10)
            .into_iter()
            .find(|r| r.belief_id == id)
            .unwrap();
        assert_eq!(row.subject, "2317 TW");
        assert_eq!(row.rationale.unwrap().chars().count(), 400);
    }

    // ── submit + recent round trip ──

    #[test]
    fn submit_then_recent_round_trips_all_fields() {
        let (db, _dir) = temp_db();
        let b = belief("trader", "2317", "up", 0.7, 100.0);
        let id = submit(&db, b).unwrap();
        let rows = recent(&db, Some("trader"), 10);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.belief_id, id);
        assert_eq!(row.agent_id, "trader");
        assert_eq!(row.subject, "2317");
        assert_eq!(row.horizon, "今日收盤");
        assert_eq!(row.direction, "up");
        assert!((row.prob - 0.7).abs() < 1e-9);
        assert_eq!(row.ref_value, Some(100.0));
        assert!(!row.stats_injected, "no mark_stats_injected call yet");
        assert!(row.settled_at.is_none());
        assert!(row.outcome.is_none());
    }

    #[test]
    fn recent_filters_by_agent_and_respects_limit() {
        let (db, _dir) = temp_db();
        submit(&db, belief("alice", "2317", "up", 0.6, 100.0)).unwrap();
        submit(&db, belief("bob", "2317", "up", 0.6, 100.0)).unwrap();
        submit(&db, belief("alice", "TAIEX", "down", 0.6, 18000.0)).unwrap();

        let alice_rows = recent(&db, Some("alice"), 10);
        assert_eq!(alice_rows.len(), 2);
        assert!(alice_rows.iter().all(|r| r.agent_id == "alice"));

        let all_rows = recent(&db, None, 10);
        assert_eq!(all_rows.len(), 3);

        let limited = recent(&db, None, 1);
        assert_eq!(limited.len(), 1);

        assert!(recent(&db, Some("alice"), 0).is_empty());
    }

    #[test]
    fn recent_and_stats_are_fail_open_on_missing_db_directory() {
        let missing = std::path::PathBuf::from("/nonexistent/deeply/nested/prediction.db");
        assert!(recent(&missing, Some("trader"), 10).is_empty());
        let s = stats(&missing, "trader");
        assert_ne!(s.calibration_status, CalibrationStatus::Calibrated);
        assert_eq!(s.n_submitted, 0);
        assert_eq!(s.calibration_status, CalibrationStatus::NoVerifiedSettlements);
    }

    // ── mark_stats_injected → submit stamping ──

    #[test]
    fn stats_injected_marker_flows_into_subsequent_submit() {
        let (db, _dir) = temp_db();
        // Before marking: not injected.
        let id1 = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        let row1 = recent(&db, Some("trader"), 10)
            .into_iter()
            .find(|r| r.belief_id == id1)
            .unwrap();
        assert!(!row1.stats_injected);

        mark_stats_injected(&db, "trader").unwrap();
        let id2 = submit(&db, belief("trader", "TAIEX", "down", 0.6, 18000.0)).unwrap();
        let row2 = recent(&db, Some("trader"), 10)
            .into_iter()
            .find(|r| r.belief_id == id2)
            .unwrap();
        assert!(
            row2.stats_injected,
            "submit within the 12h window after marking must be stamped"
        );

        // A different agent's marker must not leak across agents.
        let id3 = submit(&db, belief("someone-else", "2317", "up", 0.6, 100.0)).unwrap();
        let row3 = recent(&db, Some("someone-else"), 10)
            .into_iter()
            .find(|r| r.belief_id == id3)
            .unwrap();
        assert!(!row3.stats_injected);
    }

    // ── settle: happy path / flat band / brier ──

    #[test]
    fn settle_directional_hit_computes_agent_unverified_and_positive_brier() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.7, 100.0)).unwrap();
        let row = settle(&db, "trader", &id, 110.0, None).unwrap();
        assert_eq!(row.realized_direction.as_deref(), Some("up"));
        assert_eq!(row.outcome.as_deref(), Some("hit"));
        assert_eq!(row.settle_source.as_deref(), Some("agent_unverified"));
        let expected = rps3([0.15, 0.15, 0.7], 2);
        assert!((row.brier.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn settle_directional_miss_computes_miss_outcome() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.7, 100.0)).unwrap();
        let row = settle(&db, "trader", &id, 90.0, None).unwrap();
        assert_eq!(row.realized_direction.as_deref(), Some("down"));
        assert_eq!(row.outcome.as_deref(), Some("miss"));
    }

    #[test]
    fn settle_flat_band_when_declared_direction_but_observation_flat() {
        let (db, _dir) = temp_db();
        // Default flat_band_pct = 0.3%; a 0.1% move must classify as flat.
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        let row = settle(&db, "trader", &id, 100.1, None).unwrap();
        assert_eq!(row.realized_direction.as_deref(), Some("flat"));
        assert_eq!(row.outcome.as_deref(), Some("flat_band"));
    }

    #[test]
    fn settle_hit_when_declared_flat_and_observation_flat() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "flat", 0.6, 100.0)).unwrap();
        let row = settle(&db, "trader", &id, 100.1, None).unwrap();
        assert_eq!(row.realized_direction.as_deref(), Some("flat"));
        assert_eq!(row.outcome.as_deref(), Some("hit"));
    }

    #[test]
    fn settle_custom_flat_band_from_config() {
        let (db, dir) = temp_db();
        std::fs::write(
            dir.path().join("config.toml"),
            "[belief]\nflat_band_pct = 1.0\n",
        )
        .unwrap();
        // A 0.5% move is inside a 1.0% band, so it must read as flat even
        // though it would NOT be flat under the 0.3% default.
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        let row = settle(&db, "trader", &id, 100.5, None).unwrap();
        assert_eq!(row.realized_direction.as_deref(), Some("flat"));
        assert_eq!(row.outcome.as_deref(), Some("flat_band"));
    }

    // ── settle: tick cross-check ──

    #[test]
    fn settle_accepts_tick_price_within_tolerance() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        // 0.5% divergence, within the 1% tolerance.
        let row = settle(&db, "trader", &id, 110.0, Some(110.5)).unwrap();
        assert_eq!(row.settle_source.as_deref(), Some("agent+tick_verified"));
    }

    #[test]
    fn settle_refuses_when_tick_price_diverges_beyond_tolerance() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        // 5% divergence, well beyond the 1% tolerance — must be refused.
        let err = settle(&db, "trader", &id, 110.0, Some(115.0)).unwrap_err();
        assert!(err.contains("diverges"), "{err}");

        // The belief must remain unsettled after a refused settlement.
        let row = recent(&db, Some("trader"), 10)
            .into_iter()
            .find(|r| r.belief_id == id)
            .unwrap();
        assert!(row.settled_at.is_none());

        // A subsequent settle with a consistent tick must still succeed.
        let settled = settle(&db, "trader", &id, 110.0, Some(110.2)).unwrap();
        assert_eq!(
            settled.settle_source.as_deref(),
            Some("agent+tick_verified")
        );
    }

    #[test]
    fn settle_treats_malformed_tick_price_as_unverified_not_a_hard_error() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        let row = settle(&db, "trader", &id, 110.0, Some(f64::NAN)).unwrap();
        assert_eq!(row.settle_source.as_deref(), Some("agent_unverified"));
    }

    // ── settle: guards ──

    #[test]
    fn settle_rejects_unknown_belief() {
        let (db, _dir) = temp_db();
        let err = settle(&db, "trader", "no-such-id", 100.0, None).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn settle_rejects_wrong_agent() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        let err = settle(&db, "someone-else", &id, 110.0, None).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn settle_rejects_already_settled() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        settle(&db, "trader", &id, 110.0, None).unwrap();
        let err = settle(&db, "trader", &id, 111.0, None).unwrap_err();
        assert!(err.contains("already settled"), "{err}");
    }

    #[test]
    fn settle_rejects_non_positive_realized_value() {
        let (db, _dir) = temp_db();
        let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        assert!(settle(&db, "trader", &id, -5.0, None).is_err());
        assert!(settle(&db, "trader", &id, f64::NAN, None).is_err());
    }

    #[test]
    fn settle_rejects_belief_with_no_ref_value() {
        let (db, _dir) = temp_db();
        let mut b = belief("trader", "2317", "up", 0.6, 100.0);
        b.ref_value = None;
        let id = submit(&db, b).unwrap();
        let err = settle(&db, "trader", &id, 110.0, None).unwrap_err();
        assert!(err.contains("ref_value"), "{err}");
    }

    // ── stats: small-sample gating ──

    #[test]
    fn stats_below_min_settled_shows_counts_only() {
        let (db, _dir) = temp_db();
        for i in 0..5 {
            let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
            let _ = i;
            settle(&db, "trader", &id, 110.0, None).unwrap();
        }
        let s = stats(&db, "trader");
        assert_ne!(s.calibration_status, CalibrationStatus::Calibrated);
        assert_eq!(s.n_settled_all, 5);
        assert_eq!(s.self_reported.n, 5);
        assert_eq!(s.n_submitted, 5);
        assert!(s.verified.hit_rate.is_none());
        assert!(s.verified.hit_rate_wilson_low.is_none());
        assert!(s.verified.mean_brier.is_none());
        assert!(s.verified.overconfidence.is_none());
    }

    #[test]
    fn stats_unsettled_only_beliefs_are_insufficient_with_zero_settled() {
        let (db, _dir) = temp_db();
        submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        submit(&db, belief("trader", "TAIEX", "down", 0.6, 18000.0)).unwrap();
        let s = stats(&db, "trader");
        assert_ne!(s.calibration_status, CalibrationStatus::Calibrated);
        assert_eq!(s.n_submitted, 2);
        assert_eq!(s.n_settled_all, 0);
        assert!(
            s.per_subject.is_empty(),
            "no settled rows ⇒ no per-subject breakdown"
        );
    }

    // ── stats: Wilson bound + overconfidence at N=30 (known-value cross-check
    // against the same 18/30 fixture calibration.rs's own wilson_bounds test
    // uses) ──

    #[test]
    fn stats_at_min_settled_computes_wilson_lower_bound_and_overconfidence() {
        let (db, _dir) = temp_db();
        // 18 hits (realized up, matches declared "up"), 12 misses (realized
        // down) — same declared prob=0.6 throughout so overconfidence has a
        // clean expected value (mean(prob) == 0.6 == hit_rate ⇒ 0.0).
        for _ in 0..18 {
            let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
            settle(&db, "trader", &id, 110.0, Some(110.0)).unwrap();
        }
        for _ in 0..12 {
            let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
            settle(&db, "trader", &id, 90.0, Some(90.0)).unwrap();
        }

        let s = stats(&db, "trader");
        assert_eq!(s.calibration_status, CalibrationStatus::Calibrated);
        assert_eq!(s.calibration_status, CalibrationStatus::Calibrated);
        assert_eq!(s.n_settled_all, 30);
        assert_eq!(s.verified.n, 30);
        assert_eq!(s.self_reported.n, 0);
        assert!(s.self_reported.hit_rate.is_none());
        assert!((s.verified.hit_rate.unwrap() - 0.6).abs() < 1e-9);
        // Known Wilson 95% CI lower bound for 18/30 ≈ 0.423 (calibration.rs
        // `wilson_known_value_and_bounds`).
        assert!((s.verified.hit_rate_wilson_low.unwrap() - 0.423).abs() < 0.01);
        assert!(s.verified.hit_rate_wilson_low.unwrap() < s.verified.hit_rate.unwrap());
        // hit rows score rps3([0.2,0.2,0.6], up)=0.1; miss rows score
        // rps3([0.2,0.2,0.6], down)=0.5 ⇒ mean = (18*0.1+12*0.5)/30 = 0.26.
        assert!((s.verified.mean_brier.unwrap() - 0.26).abs() < 1e-9);
        assert!((s.verified.overconfidence.unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn stats_per_subject_breakdown_only_covers_settled_rows() {
        let (db, _dir) = temp_db();
        let id1 = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        settle(&db, "trader", &id1, 110.0, Some(110.0)).unwrap(); // hit
        let id2 = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        settle(&db, "trader", &id2, 90.0, Some(90.0)).unwrap(); // miss
        let id3 = submit(&db, belief("trader", "TAIEX", "down", 0.6, 18000.0)).unwrap();
        settle(&db, "trader", &id3, 17000.0, Some(17000.0)).unwrap(); // hit
        submit(&db, belief("trader", "TAIEX", "down", 0.6, 18000.0)).unwrap(); // unsettled

        let s = stats(&db, "trader");
        let by_2317 = s.per_subject.iter().find(|r| r.subject == "2317").unwrap();
        assert_eq!(by_2317.verified.n, 2);
        assert_eq!(by_2317.verified.hits, 1);
        let by_taiex = s.per_subject.iter().find(|r| r.subject == "TAIEX").unwrap();
        assert_eq!(
            by_taiex.verified.n, 1,
            "the unsettled TAIEX row must not be counted"
        );
        assert_eq!(by_taiex.verified.hits, 1);
    }

    // ── stats: verified vs self-reported split ──

    #[test]
    fn stats_self_reported_only_has_no_calibration_even_at_31_rows() {
        let (db, _dir) = temp_db();
        for _ in 0..31 {
            let id = submit(&db, belief("trader", "2317", "up", 0.9, 100.0)).unwrap();
            settle(&db, "trader", &id, 110.0, None).unwrap(); // all "hits"
        }
        let s = stats(&db, "trader");
        assert_eq!(s.n_settled_all, 31);
        assert_eq!(s.verified.n, 0);
        assert_eq!(s.self_reported.n, 31);
        assert_eq!(s.calibration_status, CalibrationStatus::NoVerifiedSettlements);
        assert_ne!(s.calibration_status, CalibrationStatus::Calibrated);
        assert!(s.verified.hit_rate.is_none());
        assert!(s.verified.hit_rate_wilson_low.is_none());
        assert!(s.verified.mean_brier.is_none());
        assert!(s.verified.overconfidence.is_none());
        assert_eq!(s.self_reported.hit_rate, Some(1.0));
    }

    #[test]
    fn stats_mixed_calibration_uses_verified_rows_only() {
        let (db, _dir) = temp_db();
        // 30 verified: 18 hits / 12 misses. 10 self-reported hits must not
        // move any calibration figure.
        for _ in 0..18 {
            let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
            settle(&db, "trader", &id, 110.0, Some(110.0)).unwrap();
        }
        for _ in 0..12 {
            let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
            settle(&db, "trader", &id, 90.0, Some(90.0)).unwrap();
        }
        for _ in 0..10 {
            let id = submit(&db, belief("trader", "2317", "up", 0.99, 100.0)).unwrap();
            settle(&db, "trader", &id, 110.0, None).unwrap();
        }
        let s = stats(&db, "trader");
        assert_eq!(s.n_settled_all, 40);
        assert_eq!(s.verified.n, 30);
        assert_eq!(s.self_reported.n, 10);
        assert!((s.verified.hit_rate.unwrap() - 0.6).abs() < 1e-9);
        assert!((s.verified.mean_brier.unwrap() - 0.26).abs() < 1e-9);
        assert!((s.verified.overconfidence.unwrap() - 0.0).abs() < 1e-9);
        assert_eq!(s.self_reported.hit_rate, Some(1.0));
        let subj = &s.per_subject[0];
        assert_eq!(subj.verified.n, 30);
        assert_eq!(subj.self_reported.n, 10);
        assert_eq!(subj.verified.hits, 18);
    }

    #[test]
    fn stats_verified_below_min_is_insufficient_not_no_verified() {
        let (db, _dir) = temp_db();
        for _ in 0..3 {
            let id = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
            settle(&db, "trader", &id, 110.0, Some(110.0)).unwrap();
        }
        let s = stats(&db, "trader");
        assert_eq!(s.verified.n, 3);
        assert_eq!(s.calibration_status, CalibrationStatus::InsufficientSamples);
        assert!(s.verified.hit_rate.is_none());
    }

    /// Insert a settled row directly with an arbitrary `settle_source`
    /// (`None` = SQL NULL, as on pre-column legacy rows).
    fn insert_settled_row(db: &Path, source: Option<&str>) {
        let conn = open_conn(db).unwrap();
        conn.execute(
            "INSERT INTO belief_log
             (belief_id, agent_id, subject, horizon, direction, prob, ref_value,
              predicted_at, realized_value, realized_direction, outcome, brier,
              settled_at, settle_source)
             VALUES (?1, 'trader', '2317', 'h', 'up', 0.6, 100.0, ?2,
                     110.0, 'up', 'hit', 0.1, ?2, ?3)",
            params![uuid::Uuid::new_v4().to_string(), Utc::now().to_rfc3339(), source],
        )
        .unwrap();
    }

    #[test]
    fn stats_unknown_empty_and_null_settle_source_count_as_unverified() {
        let (db, _dir) = temp_db();
        insert_settled_row(&db, Some("agent+tick_verified_v2"));
        insert_settled_row(&db, Some("tick_verified"));
        insert_settled_row(&db, Some(""));
        insert_settled_row(&db, None);
        insert_settled_row(&db, Some("agent_unverified"));
        let s = stats(&db, "trader");
        assert_eq!(s.n_settled_all, 5);
        assert_eq!(s.verified.n, 0);
        assert_eq!(s.self_reported.n, 5);
        assert_eq!(s.calibration_status, CalibrationStatus::NoVerifiedSettlements);
        // exact match is verified
        insert_settled_row(&db, Some(SETTLE_SOURCE_TICK_VERIFIED));
        let s = stats(&db, "trader");
        assert_eq!(s.verified.n, 1);
        assert_eq!(s.self_reported.n, 5);
    }

    #[test]
    fn is_verified_source_is_exact_equality() {
        assert!(is_verified_source(Some("agent+tick_verified")));
        assert!(!is_verified_source(Some("agent+tick_verified ")));
        assert!(!is_verified_source(Some("agent_unverified")));
        assert!(!is_verified_source(Some("")));
        assert!(!is_verified_source(None));
    }

    // ── get_meta / set_meta ──

    #[test]
    fn meta_get_set_round_trips_and_upserts() {
        let (db, _dir) = temp_db();
        assert_eq!(get_meta(&db, "self_study_last:trader"), None);
        set_meta(&db, "self_study_last:trader", "2026-08-14").unwrap();
        assert_eq!(
            get_meta(&db, "self_study_last:trader").as_deref(),
            Some("2026-08-14")
        );
        // Upsert overwrites, does not duplicate.
        set_meta(&db, "self_study_last:trader", "2026-08-15").unwrap();
        assert_eq!(
            get_meta(&db, "self_study_last:trader").as_deref(),
            Some("2026-08-15")
        );
        // A different key is independent.
        assert_eq!(get_meta(&db, "self_study_last:someone-else"), None);
    }

    #[test]
    fn meta_get_is_fail_open_on_missing_db() {
        let missing = std::path::PathBuf::from("/nonexistent/deeply/nested/prediction.db");
        assert_eq!(get_meta(&missing, "any-key"), None);
    }

    // ── unsettled_today ──

    #[test]
    fn unsettled_today_returns_only_unsettled_rows_for_the_agent() {
        let (db, _dir) = temp_db();
        let id1 = submit(&db, belief("trader", "2317", "up", 0.6, 100.0)).unwrap();
        submit(&db, belief("trader", "TAIEX", "down", 0.6, 18000.0)).unwrap();
        submit(&db, belief("someone-else", "2317", "up", 0.6, 100.0)).unwrap();

        let before = unsettled_today(&db, "trader");
        assert_eq!(before.len(), 2);

        settle(&db, "trader", &id1, 110.0, None).unwrap();
        let after = unsettled_today(&db, "trader");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].subject, "TAIEX");
    }

    #[test]
    fn unsettled_today_is_fail_open_on_missing_db() {
        let missing = std::path::PathBuf::from("/nonexistent/deeply/nested/prediction.db");
        assert!(unsettled_today(&missing, "trader").is_empty());
    }

    // ── BeliefConfig: tick_subject_map parsing ──

    #[test]
    fn belief_config_parses_explicit_tick_subject_map() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[belief]\nflat_band_pct = 0.5\n\n[belief.tick_subject_map]\n\
             conversion_rate = \"trial_conversion_rate\"\n\
             complaint_count = \"daily_complaints\"\n",
        )
        .unwrap();
        let cfg = BeliefConfig::from_home(dir.path());
        assert!((cfg.flat_band_pct - 0.5).abs() < 1e-9);
        assert_eq!(
            cfg.tick_subject_map
                .get("conversion_rate")
                .map(String::as_str),
            Some("trial_conversion_rate")
        );
        assert_eq!(
            cfg.tick_subject_map
                .get("complaint_count")
                .map(String::as_str),
            Some("daily_complaints")
        );
    }

    #[test]
    fn belief_config_missing_tick_subject_map_defaults_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[belief]\nflat_band_pct = 0.5\n",
        )
        .unwrap();
        let cfg = BeliefConfig::from_home(dir.path());
        assert!(cfg.tick_subject_map.is_empty());
    }

    // ── render_calibration_section ──

    fn stats_fixture(n_verified: u64, n_self: u64) -> BeliefStats {
        let calibrated = n_verified >= MIN_SETTLED_FOR_STATS;
        BeliefStats {
            agent_id: "trader".to_string(),
            n_submitted: n_verified + n_self,
            n_settled_all: n_verified + n_self,
            calibration_status: if calibrated {
                CalibrationStatus::Calibrated
            } else if n_verified == 0 {
                CalibrationStatus::NoVerifiedSettlements
            } else {
                CalibrationStatus::InsufficientSamples
            },
            verified: VerifiedStats {
                n: n_verified,
                hits: 0,
                hit_rate: if calibrated { Some(0.6) } else { None },
                hit_rate_wilson_low: if calibrated { Some(0.42) } else { None },
                mean_brier: if calibrated { Some(0.26) } else { None },
                overconfidence: if calibrated { Some(0.0) } else { None },
            },
            self_reported: SelfReportedStats {
                n: n_self,
                hit_rate: if n_self > 0 { Some(1.0) } else { None },
            },
            per_subject: Vec::new(),
        }
    }

    #[test]
    fn render_calibration_section_is_none_when_nothing_settled() {
        assert!(render_calibration_section(&stats_fixture(0, 0)).is_none());
    }

    #[test]
    fn render_calibration_section_below_min_shows_counts_only() {
        let section = render_calibration_section(&stats_fixture(5, 0)).unwrap();
        assert!(section.starts_with("## 信念校準"));
        assert!(section.contains("已驗證結算 5 筆"));
        assert!(section.contains("未達"));
        // Must never fabricate a hit rate / Wilson figure at small N.
        assert!(!section.contains("Wilson"));
    }

    #[test]
    fn render_calibration_section_at_min_shows_full_stats() {
        let section = render_calibration_section(&stats_fixture(30, 0)).unwrap();
        assert!(section.contains("已驗證結算 30 筆"));
        assert!(section.contains("Wilson"));
        assert!(section.contains("42%"));
        assert!(section.contains("0.260"));
        assert!(section.contains("+0.00"));
        assert!(!section.contains("自行回報"), "no self-reported rows ⇒ no note");
    }

    #[test]
    fn render_calibration_section_self_reported_only_is_zero_injection() {
        // Injection would stamp `stats_injected` for a prompt with no
        // calibration figure at all; the agent learns "unverified" from the
        // belief_settle response instead.
        assert!(render_calibration_section(&stats_fixture(0, 31)).is_none());
    }

    #[test]
    fn render_calibration_section_mixed_counts_only_verified_and_notes_the_rest() {
        let section = render_calibration_section(&stats_fixture(30, 7)).unwrap();
        assert!(section.contains("已驗證結算 30 筆"));
        assert!(section.contains("另有 7 筆"));
        assert!(section.contains("不計入"));
    }

    // ── render_tick_diff_line / render_tick_diff_section ──

    fn belief_row_fixture(
        subject: &str,
        direction: &str,
        prob: f64,
        ref_value: Option<f64>,
    ) -> BeliefRow {
        BeliefRow {
            belief_id: "b1".to_string(),
            agent_id: "trader".to_string(),
            subject: subject.to_string(),
            horizon: "今日收盤".to_string(),
            direction: direction.to_string(),
            prob,
            rationale: None,
            ref_value,
            predicted_at: "2026-08-14T00:00:00Z".to_string(),
            stats_injected: false,
            realized_value: None,
            realized_direction: None,
            outcome: None,
            brier: None,
            settled_at: None,
            settle_source: None,
            source_goal_id: None,
        }
    }

    #[test]
    fn render_tick_diff_line_none_without_ref_value() {
        let row = belief_row_fixture("2317", "up", 0.6, None);
        assert!(render_tick_diff_line(&row, 105.0).is_none());
    }

    #[test]
    fn render_tick_diff_line_renders_subject_direction_and_prices() {
        let row = belief_row_fixture("2317", "up", 0.65, Some(100.0));
        let line = render_tick_diff_line(&row, 105.0).unwrap();
        assert!(line.contains("2317"));
        assert!(line.contains("up"));
        assert!(line.contains("65%"));
        assert!(line.contains("100.0000"));
        assert!(line.contains("105.0000"));
        assert!(line.contains("▲"));
        assert!(line.contains("+5.00%"));
    }

    #[test]
    fn render_tick_diff_section_lists_multiple_and_is_none_when_empty() {
        assert!(render_tick_diff_section(&[]).is_none());
        let rows = vec![
            (belief_row_fixture("2317", "up", 0.6, Some(100.0)), 105.0),
            (
                belief_row_fixture("TAIEX", "down", 0.7, Some(18000.0)),
                17500.0,
            ),
        ];
        let section = render_tick_diff_section(&rows).unwrap();
        assert!(section.starts_with("## 信念對照"));
        assert!(section.contains("2317"));
        assert!(section.contains("TAIEX"));
        assert_eq!(
            section.matches('\n').count(),
            2,
            "one header line + two belief lines"
        );
    }
}
