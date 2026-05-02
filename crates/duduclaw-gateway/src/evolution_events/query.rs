//! Audit Trail Query API — M4 (W19-P1).
//!
//! SQLite-backed index cache over the JSONL audit files written by
//! [`EvolutionEventLogger`].  Supports filtered, paginated queries without
//! scanning JSONL files on every request.
//!
//! ## Architecture
//! ```text
//! EvolutionEventLogger ──writes──▶  YYYY-MM-DD.jsonl (source of truth)
//!                                         │
//!                          sync_from_files()
//!                                         ▼
//!                           audit_index.db  (SQLite index cache)
//!                                         │
//!                             query(filter)
//!                                         ▼
//!                           AuditQueryResult { events, total }
//! ```
//!
//! ## Idempotency
//! `sync_from_files` records how many lines have been indexed per file in the
//! `indexed_files` tracking table.  Re-running only reads new appends — never
//! duplicates rows.
//!
//! ## SQLite configuration
//! WAL mode + 5 s busy_timeout — same as `events_store.rs`.
//!
//! ## Index path
//! `<home_dir>/audit_index.db`  (configurable for tests via
//! [`AuditEventIndex::open_with_events_dir`]).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::Value as Json;
use tracing::{info, warn};

use super::logger::resolve_default_dir;
use super::reliability::{
    ReliabilitySummary, consistency_from_rows, fallback_trigger_rate_from_counts,
    skill_adoption_rate_from_counts, task_success_rate_from_counts,
};
use super::schema::AuditEvent;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default page size for [`AuditQueryFilter::limit`].
pub const DEFAULT_LIMIT: i64 = 100;
/// Hard maximum page size — queries larger than this are clamped.
pub const MAX_LIMIT: i64 = 1000;
/// Hard maximum pagination offset — prevents DoS via enormous OFFSET values (INFRA-SEC-04).
/// SQLite must scan `offset` rows before returning results; an unbounded offset can
/// cause excessive CPU/IO under adversarial or accidental usage.
pub const MAX_OFFSET: i64 = 1_000_000;

/// Allowlist of column names that may appear in [`build_filter_clause`].
///
/// Prevents future SQL-injection vectors if a caller ever passes user-controlled
/// data as a column name (INFRA-SEC-H1 fix).  Every column referenced in
/// `eq_filter!` or `cmp_filter!` macros **must** appear in this list.
pub const ALLOWED_FILTER_COLS: &[&str] = &[
    "agent_id",
    "event_type",
    "outcome",
    "skill_id",
    "timestamp",
];

// ── Error type ────────────────────────────────────────────────────────────────

/// Error variants for [`AuditEventIndex`] query-building operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditQueryError {
    /// A column name that is not in [`ALLOWED_FILTER_COLS`] was passed to a
    /// filter-clause builder.  This is a programmer error — do not expose the
    /// raw variant to end-users.
    InvalidColumn(String),
}

impl std::fmt::Display for AuditQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditQueryError::InvalidColumn(col) => {
                write!(f, "invalid filter column {col:?}: not in ALLOWED_FILTER_COLS allowlist")
            }
        }
    }
}

// ── Filter ────────────────────────────────────────────────────────────────────

/// Structured filter for [`AuditEventIndex::query`].
///
/// All fields are optional; omitted fields match every row.
#[derive(Debug, Default, Clone)]
pub struct AuditQueryFilter {
    /// Match only events emitted by this agent.
    pub agent_id: Option<String>,
    /// Match only this event type (snake_case, e.g. `"governance_violation"`).
    pub event_type: Option<String>,
    /// Match only this outcome (snake_case, e.g. `"blocked"`).
    pub outcome: Option<String>,
    /// Match only events that reference this skill.
    pub skill_id: Option<String>,
    /// Inclusive lower bound on `timestamp` (RFC3339).
    pub since: Option<String>,
    /// Exclusive upper bound on `timestamp` (RFC3339).
    pub until: Option<String>,
    /// Max rows per page.  Clamped to `[1, MAX_LIMIT]`.  Default: `DEFAULT_LIMIT`.
    pub limit: Option<i64>,
    /// Zero-based row offset for pagination.
    pub offset: Option<i64>,
}

// ── Result ────────────────────────────────────────────────────────────────────

/// Paginated query result from [`AuditEventIndex::query`].
#[derive(Debug, Clone)]
pub struct AuditQueryResult {
    /// Events on the current page, ordered by `(timestamp ASC, id ASC)`.
    pub events: Vec<AuditEvent>,
    /// Total matching rows (ignores limit / offset).
    pub total: i64,
    /// Effective limit that was applied.
    pub limit: i64,
    /// Effective offset that was applied.
    pub offset: i64,
}

// ── Index ─────────────────────────────────────────────────────────────────────

/// SQLite-backed index cache over EvolutionEvent JSONL audit logs.
///
/// Obtain one via [`AuditEventIndex::open`] and keep it alive for the process
/// lifetime (or behind an `Arc`).
pub struct AuditEventIndex {
    conn: tokio::sync::Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
    /// Directory that contains the source `.jsonl` files.
    events_dir: PathBuf,
}

impl AuditEventIndex {
    /// Open (or create) the index at `<home_dir>/audit_index.db`.
    ///
    /// JSONL source files are read from the path resolved by
    /// [`resolve_default_dir`] (`$EVOLUTION_EVENTS_DIR` → `$DUDUCLAW_HOME/…`
    /// → `$HOME/.duduclaw/evolution/events`).
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        Self::open_with_events_dir(home_dir, resolve_default_dir())
    }

    /// Open with an explicit `events_dir` path — intended for tests.
    pub fn open_with_events_dir(
        home_dir: &Path,
        events_dir: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let db_path = home_dir.join("audit_index.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open audit index DB: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "AuditEventIndex initialised");
        Ok(Self {
            conn: tokio::sync::Mutex::new(conn),
            db_path,
            events_dir: events_dir.into(),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS audit_index (
                 id             INTEGER PRIMARY KEY AUTOINCREMENT,
                 timestamp      TEXT    NOT NULL,
                 event_type     TEXT    NOT NULL,
                 agent_id       TEXT    NOT NULL,
                 skill_id       TEXT,
                 generation     INTEGER,
                 outcome        TEXT    NOT NULL,
                 trigger_signal TEXT,
                 metadata       TEXT    NOT NULL DEFAULT '{}',
                 source_file    TEXT    NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_audit_ts
                 ON audit_index(timestamp);
             CREATE INDEX IF NOT EXISTS idx_audit_agent
                 ON audit_index(agent_id);
             CREATE INDEX IF NOT EXISTS idx_audit_event_type
                 ON audit_index(event_type);
             CREATE INDEX IF NOT EXISTS idx_audit_outcome
                 ON audit_index(outcome);
             CREATE INDEX IF NOT EXISTS idx_audit_skill
                 ON audit_index(skill_id)
                 WHERE skill_id IS NOT NULL;

             -- Track which files (and how many lines) have been indexed.
             CREATE TABLE IF NOT EXISTS indexed_files (
                 filename       TEXT    PRIMARY KEY,
                 lines_indexed  INTEGER NOT NULL DEFAULT 0,
                 last_synced_at TEXT    NOT NULL
             );",
        )
        .map_err(|e| format!("init audit index schema: {e}"))?;
        Ok(())
    }

    // ── Sync ──────────────────────────────────────────────────────────────────

    /// Ingest any new lines from JSONL files in the events directory.
    ///
    /// Returns the number of new rows inserted into the index.
    /// Already-indexed lines are skipped — the operation is idempotent.
    pub async fn sync_from_files(&self) -> Result<usize, String> {
        let files = self.collect_jsonl_files()?;
        if files.is_empty() {
            return Ok(0);
        }

        let mut total_inserted = 0usize;
        let conn = self.conn.lock().await;

        for (filename, path) in files {
            let already: i64 = conn
                .query_row(
                    "SELECT lines_indexed FROM indexed_files WHERE filename = ?1",
                    params![filename],
                    |r| r.get(0),
                )
                .unwrap_or(0);

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("sync_from_files: cannot read {path:?}: {e}");
                    continue;
                }
            };

            // `lines_indexed` tracks the **number of lines scanned** (not inserted),
            // so the slice offset stays correct even when empty/malformed lines are
            // skipped.  Using inserted-count as the offset would mis-align the cursor
            // on subsequent syncs (HIGH bug fix — QA round 2).
            let all_lines: Vec<&str> = content.lines().collect();
            let new_lines = &all_lines[(already as usize).min(all_lines.len())..];
            let mut inserted = 0usize;
            let mut scanned = 0usize; // lines actually examined in this batch

            for line in new_lines {
                scanned += 1;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let ev: AuditEvent = match serde_json::from_str(line) {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("sync_from_files: malformed JSONL line in {filename}: {e}");
                        continue;
                    }
                };

                let meta_str = serde_json::to_string(&ev.metadata).unwrap_or_else(|_| "{}".into());

                match conn.execute(
                    "INSERT INTO audit_index
                     (timestamp, event_type, agent_id, skill_id, generation,
                      outcome, trigger_signal, metadata, source_file)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        ev.timestamp,
                        ev.event_type.to_string(),
                        ev.agent_id,
                        ev.skill_id,
                        ev.generation,
                        ev.outcome.to_string(),
                        ev.trigger_signal,
                        meta_str,
                        filename,
                    ],
                ) {
                    Ok(_) => inserted += 1,
                    Err(e) => warn!("sync_from_files: INSERT failed in {filename}: {e}"),
                }
            }

            // Record the number of lines *scanned* (not just inserted), so that the
            // next sync correctly skips blank / malformed lines via the slice offset.
            // Upsert uses SQLite ≥ 3.24 ON CONFLICT syntax.
            let new_lines_indexed = already + scanned as i64;
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO indexed_files (filename, lines_indexed, last_synced_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(filename) DO UPDATE
                     SET lines_indexed = ?2, last_synced_at = ?3",
                params![filename, new_lines_indexed, now],
            )
            .map_err(|e| format!("upsert indexed_files[{filename}]: {e}"))?;

            total_inserted += inserted;
        }

        info!(total_inserted, "AuditEventIndex sync complete");
        Ok(total_inserted)
    }

    // ── Query ─────────────────────────────────────────────────────────────────

    /// Query the index with optional filters.
    ///
    /// Results are ordered by `(timestamp ASC, id ASC)`.
    /// The returned [`AuditQueryResult::total`] is the count of all matching
    /// rows before pagination — use it to implement `total_pages`.
    pub async fn query(&self, filter: AuditQueryFilter) -> Result<AuditQueryResult, String> {
        let effective_limit = filter.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        // INFRA-SEC-04 fix: clamp offset to [0, MAX_OFFSET] to prevent DoS via
        // enormous OFFSET values that force SQLite to scan millions of rows.
        let effective_offset = filter.offset.unwrap_or(0).clamp(0, MAX_OFFSET);

        let conn = self.conn.lock().await;

        // Build a shared WHERE clause + param list from the filter.
        // build_filter_clause validates every column name against ALLOWED_FILTER_COLS
        // (INFRA-SEC-H1) — propagate any InvalidColumn error as a String.
        let FilterClause { where_sql, params: filter_params } =
            build_filter_clause(&filter).map_err(|e| e.to_string())?;

        // ── COUNT query ───────────────────────────────────────────────────────
        let count_sql = format!("SELECT COUNT(*) FROM audit_index {where_sql}");
        let count_refs: Vec<&dyn rusqlite::ToSql> =
            filter_params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let total: i64 = conn
            .query_row(&count_sql, count_refs.as_slice(), |r| r.get(0))
            .map_err(|e| format!("audit query count: {e}"))?;

        // ── PAGE query ────────────────────────────────────────────────────────
        // LIMIT / OFFSET are bound as i64 (INTEGER affinity) rather than String
        // to preserve correct SQLite type semantics (HIGH fix — QA round 2).
        // filter_params (Vec<String>) supply positions ?1..?N; limit/offset use
        // ?N+1 and ?N+2 via a separate mixed-type params slice.
        let n = filter_params.len();
        let page_sql = format!(
            "SELECT timestamp, event_type, agent_id, skill_id, generation,
                    outcome, trigger_signal, metadata
             FROM audit_index
             {where_sql}
             ORDER BY timestamp ASC, id ASC
             LIMIT ?{} OFFSET ?{}",
            n + 1,
            n + 2,
        );

        // Build a unified &[&dyn ToSql] that mixes Vec<String> filter params
        // (positions 1..=N) with i64 limit/offset (positions N+1, N+2).
        // The lifetime-safe approach: collect refs from two separate owned vecs.
        let filter_refs: Vec<&dyn rusqlite::ToSql> =
            filter_params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let page_refs: Vec<&dyn rusqlite::ToSql> = filter_refs
            .iter()
            .copied()
            .chain([
                &effective_limit as &dyn rusqlite::ToSql,
                &effective_offset as &dyn rusqlite::ToSql,
            ])
            .collect();

        let mut stmt = conn
            .prepare(&page_sql)
            .map_err(|e| format!("audit query prepare: {e}"))?;

        let events: Vec<AuditEvent> = stmt
            .query_map(page_refs.as_slice(), |row| {
                Ok(RawRow {
                    timestamp:      row.get(0)?,
                    event_type_str: row.get(1)?,
                    agent_id:       row.get(2)?,
                    skill_id:       row.get(3)?,
                    generation:     row.get(4)?,
                    outcome_str:    row.get(5)?,
                    trigger_signal: row.get(6)?,
                    metadata_str:   row.get(7)?,
                })
            })
            .map_err(|e| format!("audit query execute: {e}"))?
            .filter_map(|res| {
                let raw = res.ok()?;
                raw_to_audit_event(raw)
            })
            .collect();

        Ok(AuditQueryResult {
            events,
            total,
            limit: effective_limit,
            offset: effective_offset,
        })
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Enumerate `.jsonl` files in `self.events_dir`, sorted by name (= date order).
    ///
    /// Hidden files (prefixed with `.`) are excluded so `.healthcheck` is skipped.
    fn collect_jsonl_files(&self) -> Result<Vec<(String, PathBuf)>, String> {
        let rd = match std::fs::read_dir(&self.events_dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(format!(
                    "collect_jsonl_files: read {:?}: {e}",
                    self.events_dir
                ))
            }
        };

        let mut files: Vec<(String, PathBuf)> = rd
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?.to_owned();
                if name.ends_with(".jsonl") && !name.starts_with('.') {
                    Some((name, path))
                } else {
                    None
                }
            })
            .collect();

        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Intermediate row from a SQLite `query_map` closure.
///
/// All values are stored as `String` / `Option<String>` so the closure
/// has no lifetime dependencies on anything outside it.
struct RawRow {
    timestamp:      String,
    event_type_str: String,
    agent_id:       String,
    skill_id:       Option<String>,
    generation:     Option<i64>,
    outcome_str:    String,
    trigger_signal: Option<String>,
    metadata_str:   String,
}

/// Convert a [`RawRow`] back to an [`AuditEvent`].
///
/// Returns `None` if `event_type` or `outcome` fails to deserialise (should
/// never happen in a well-formed index; degraded gracefully via `warn`).
fn raw_to_audit_event(raw: RawRow) -> Option<AuditEvent> {
    let event_type =
        serde_json::from_value(serde_json::Value::String(raw.event_type_str.clone()))
            .map_err(|e| warn!("audit query: unknown event_type '{}': {e}", raw.event_type_str))
            .ok()?;

    let outcome =
        serde_json::from_value(serde_json::Value::String(raw.outcome_str.clone()))
            .map_err(|e| warn!("audit query: unknown outcome '{}': {e}", raw.outcome_str))
            .ok()?;

    let metadata: Json =
        serde_json::from_str(&raw.metadata_str).unwrap_or(Json::Object(Default::default()));

    Some(AuditEvent {
        timestamp: raw.timestamp,
        event_type,
        agent_id: raw.agent_id,
        skill_id: raw.skill_id,
        generation: raw.generation,
        outcome,
        trigger_signal: raw.trigger_signal,
        metadata,
    })
}

/// Shared WHERE clause builder — returns both the SQL fragment and the
/// corresponding positional parameter values (all as `String` for uniform
/// `rusqlite::ToSql` binding).
#[derive(Debug)]
struct FilterClause {
    where_sql: String,
    params:    Vec<String>,
}

/// Returns `col` unchanged if it is in [`ALLOWED_FILTER_COLS`], or an
/// [`AuditQueryError::InvalidColumn`] otherwise.
///
/// Call this before inserting any column name into a SQL fragment to prevent
/// SQL-injection vectors (INFRA-SEC-H1).
fn validate_col(col: &str) -> Result<&str, AuditQueryError> {
    if ALLOWED_FILTER_COLS.contains(&col) {
        Ok(col)
    } else {
        Err(AuditQueryError::InvalidColumn(col.to_string()))
    }
}

/// Build a shared WHERE clause + positional param list from the filter fields.
///
/// Each column name is validated against [`ALLOWED_FILTER_COLS`] before being
/// interpolated into the SQL fragment (INFRA-SEC-H1 fix).  Returns
/// [`AuditQueryError::InvalidColumn`] if an unrecognised column is encountered —
/// this should never happen with the current hardcoded callers, but the check
/// prevents future contributors from accidentally introducing SQL injection by
/// passing user-controlled column names.
fn build_filter_clause(filter: &AuditQueryFilter) -> Result<FilterClause, AuditQueryError> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();

    macro_rules! eq_filter {
        ($col:expr, $opt:expr) => {
            if let Some(ref v) = $opt {
                let col = validate_col($col)?;
                clauses.push(format!("{col} = ?"));
                params.push(v.clone());
            }
        };
    }
    macro_rules! cmp_filter {
        ($col:expr, $op:expr, $opt:expr) => {
            if let Some(ref v) = $opt {
                let col = validate_col($col)?;
                clauses.push(format!("{col} {op} ?", op = $op));
                params.push(v.clone());
            }
        };
    }

    eq_filter!("agent_id",   filter.agent_id);
    eq_filter!("event_type", filter.event_type);
    eq_filter!("outcome",    filter.outcome);
    eq_filter!("skill_id",   filter.skill_id);
    cmp_filter!("timestamp", ">=", filter.since);
    cmp_filter!("timestamp", "<",  filter.until);

    Ok(FilterClause {
        where_sql: if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        },
        params,
    })
}

// ── Reliability Dashboard (W20-P0) ────────────────────────────────────────────

impl AuditEventIndex {
    /// Compute an Agent Reliability Summary for `agent_id` over the last `window_days` days.
    ///
    /// Executes SQL aggregate queries directly against the `audit_index` SQLite cache.
    /// Callers **should** call [`sync_from_files`] first to ensure the index is current.
    ///
    /// # Metrics computed
    ///
    /// | Field | SQL approach |
    /// |-------|-------------|
    /// | `consistency_score`     | AVG per-event-type success rate via GROUP BY |
    /// | `task_success_rate`     | SUM(outcome='success') / COUNT(*) |
    /// | `skill_adoption_rate`   | SUM(event_type='skill_activate') / COUNT(*) |
    /// | `fallback_trigger_rate` | SUM(event_type='llm_fallback_triggered') / COUNT(*) |
    ///
    /// All rate fields default to `1.0` or `0.0` when no data is present (see
    /// [`reliability`] module doc for per-metric defaults).
    pub async fn compute_reliability_summary(
        &self,
        agent_id: &str,
        window_days: u32,
    ) -> Result<ReliabilitySummary, String> {
        use chrono::Utc;

        let since = Utc::now()
            - chrono::Duration::try_days(window_days as i64)
                .unwrap_or(chrono::Duration::zero());
        let since_str = since.to_rfc3339();
        let generated_at = Utc::now().to_rfc3339();

        let conn = self.conn.lock().await;

        // ── Single-pass aggregate query (total, success, skill, fallback) ──────
        let (total, success_count, skill_count, fallback_count): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT
                     COUNT(*),
                     SUM(CASE WHEN outcome    = 'success'                THEN 1 ELSE 0 END),
                     SUM(CASE WHEN event_type = 'skill_activate'         THEN 1 ELSE 0 END),
                     SUM(CASE WHEN event_type = 'llm_fallback_triggered' THEN 1 ELSE 0 END)
                 FROM audit_index
                 WHERE agent_id = ?1 AND timestamp >= ?2",
                params![agent_id, since_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0).unwrap_or(0),
                        row.get::<_, i64>(1).unwrap_or(0),
                        row.get::<_, i64>(2).unwrap_or(0),
                        row.get::<_, i64>(3).unwrap_or(0),
                    ))
                },
            )
            .map_err(|e| format!("reliability aggregate query: {e}"))?;

        // ── Per-event-type query for consistency_score ────────────────────────
        let per_type_rows: Vec<(String, i64, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT event_type,
                            COUNT(*) AS total,
                            SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) AS success_count
                     FROM audit_index
                     WHERE agent_id = ?1 AND timestamp >= ?2
                     GROUP BY event_type
                     HAVING COUNT(*) > 0",
                )
                .map_err(|e| format!("consistency query prepare: {e}"))?;

            stmt.query_map(params![agent_id, since_str], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| format!("consistency query execute: {e}"))?
            .filter_map(|r| r.map_err(|e| tracing::warn!("consistency row error: {e}")).ok())
            .collect()
        };

        Ok(ReliabilitySummary {
            agent_id: agent_id.to_string(),
            window_days,
            consistency_score: consistency_from_rows(&per_type_rows),
            task_success_rate: task_success_rate_from_counts(total, success_count),
            skill_adoption_rate: skill_adoption_rate_from_counts(total, skill_count),
            fallback_trigger_rate: fallback_trigger_rate_from_counts(total, fallback_count),
            total_events: total,
            generated_at,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::evolution_events::{
        logger::EvolutionEventLogger,
        schema::{AuditEvent, AuditEventType, Outcome},
    };

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Create a temp dir pair: `(home_dir, events_dir)`.
    fn fresh_dirs() -> (TempDir, TempDir) {
        let home = TempDir::new().unwrap();
        let events = TempDir::new().unwrap();
        (home, events)
    }

    /// Write `events` as JSONL lines into `<events_dir>/<filename>`.
    fn write_jsonl(events_dir: &Path, filename: &str, events: &[AuditEvent]) {
        use std::io::Write as _;
        let path = events_dir.join(filename);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        for ev in events {
            let line = serde_json::to_string(ev).unwrap();
            writeln!(f, "{line}").unwrap();
        }
    }

    fn sample_activate(agent: &str) -> AuditEvent {
        AuditEvent::now(AuditEventType::SkillActivate, agent, Outcome::Success)
            .with_skill_id("python-patterns")
            .with_trigger_signal("manual_toggle")
    }

    fn sample_violation(agent: &str) -> AuditEvent {
        AuditEvent::now(AuditEventType::GovernanceViolation, agent, Outcome::Blocked)
            .with_metadata(serde_json::json!({
                "policy_id": "default-rate-mcp",
                "policy_type": "rate",
            }))
    }

    fn sample_retry(agent: &str) -> AuditEvent {
        AuditEvent::now(AuditEventType::DurabilityRetryAttempt, agent, Outcome::Failure)
    }

    fn open_index(home: &TempDir, events: &TempDir) -> AuditEventIndex {
        AuditEventIndex::open_with_events_dir(home.path(), events.path()).unwrap()
    }

    // ── Schema / open ─────────────────────────────────────────────────────────

    #[test]
    fn open_creates_db_file() {
        let (home, events) = fresh_dirs();
        let _idx = open_index(&home, &events);
        assert!(
            home.path().join("audit_index.db").exists(),
            "DB file must be created on open"
        );
    }

    #[test]
    fn open_is_idempotent() {
        let (home, events) = fresh_dirs();
        let _a = open_index(&home, &events);
        let _b = open_index(&home, &events); // must not fail
    }

    // ── sync_from_files ───────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn sync_empty_events_dir_returns_zero() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);
        let n = idx.sync_from_files().await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_missing_events_dir_returns_zero() {
        let home = TempDir::new().unwrap();
        let idx = AuditEventIndex::open_with_events_dir(
            home.path(),
            home.path().join("nonexistent"),
        )
        .unwrap();
        let n = idx.sync_from_files().await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_ingests_events_from_jsonl() {
        let (home, events) = fresh_dirs();
        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[
                sample_activate("agent-a"),
                sample_violation("agent-b"),
            ],
        );
        let idx = open_index(&home, &events);
        let n = idx.sync_from_files().await.unwrap();
        assert_eq!(n, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_is_idempotent() {
        let (home, events) = fresh_dirs();
        write_jsonl(events.path(), "2026-04-29.jsonl", &[sample_activate("a")]);
        let idx = open_index(&home, &events);

        let first  = idx.sync_from_files().await.unwrap();
        let second = idx.sync_from_files().await.unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 0, "second sync must not re-insert already-indexed rows");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_picks_up_new_appends() {
        let (home, events) = fresh_dirs();
        write_jsonl(events.path(), "2026-04-29.jsonl", &[sample_activate("a")]);
        let idx = open_index(&home, &events);

        let first = idx.sync_from_files().await.unwrap();
        assert_eq!(first, 1);

        // Append a second event to the same file.
        write_jsonl(events.path(), "2026-04-29.jsonl", &[sample_violation("b")]);
        let second = idx.sync_from_files().await.unwrap();
        assert_eq!(second, 1, "must pick up only the new line");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_handles_multiple_jsonl_files() {
        let (home, events) = fresh_dirs();
        write_jsonl(events.path(), "2026-04-27.jsonl", &[sample_activate("x")]);
        write_jsonl(events.path(), "2026-04-28.jsonl", &[sample_activate("y"), sample_retry("z")]);
        let idx = open_index(&home, &events);
        let n = idx.sync_from_files().await.unwrap();
        assert_eq!(n, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_skips_non_jsonl_files() {
        let (home, events) = fresh_dirs();
        // Write a non-JSONL file — must be ignored.
        std::fs::write(events.path().join("README.txt"), b"ignore me").unwrap();
        std::fs::write(events.path().join(".healthcheck"), b"ok").unwrap();
        write_jsonl(events.path(), "2026-04-29.jsonl", &[sample_activate("a")]);
        let idx = open_index(&home, &events);
        let n = idx.sync_from_files().await.unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_skips_malformed_json_lines() {
        let (home, events) = fresh_dirs();
        let path = events.path().join("2026-04-29.jsonl");
        std::fs::write(
            &path,
            b"{\"timestamp\":\"2026-04-29T00:00:00Z\",\"event_type\":\"skill_activate\",\
              \"agent_id\":\"ok\",\"skill_id\":null,\"generation\":null,\
              \"outcome\":\"success\",\"trigger_signal\":null,\"metadata\":{}}\n\
              NOT VALID JSON\n",
        )
        .unwrap();
        let idx = open_index(&home, &events);
        let n = idx.sync_from_files().await.unwrap();
        assert_eq!(n, 1, "only the valid line must be indexed");
    }

    // ── query — no filters ────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_empty_index_returns_empty() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);
        let result = idx.query(AuditQueryFilter::default()).await.unwrap();
        assert_eq!(result.total, 0);
        assert!(result.events.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_no_filters_returns_all() {
        let (home, events) = fresh_dirs();
        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[sample_activate("a"), sample_violation("b"), sample_retry("c")],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx.query(AuditQueryFilter::default()).await.unwrap();
        assert_eq!(result.total, 3);
        assert_eq!(result.events.len(), 3);
    }

    // ── query — agent_id filter ───────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_filter_by_agent_id() {
        let (home, events) = fresh_dirs();
        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[
                sample_activate("agent-alpha"),
                sample_activate("agent-beta"),
                sample_violation("agent-alpha"),
            ],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                agent_id: Some("agent-alpha".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 2, "only agent-alpha rows should match");
        assert!(result.events.iter().all(|e| e.agent_id == "agent-alpha"));
    }

    // ── query — event_type filter ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_filter_by_event_type() {
        let (home, events) = fresh_dirs();
        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[
                sample_activate("a"),
                sample_violation("a"),
                sample_violation("b"),
                sample_retry("a"),
            ],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                event_type: Some("governance_violation".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 2);
        assert!(result
            .events
            .iter()
            .all(|e| e.event_type == AuditEventType::GovernanceViolation));
    }

    // ── query — outcome filter ────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_filter_by_outcome() {
        let (home, events) = fresh_dirs();
        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[
                sample_activate("a"),    // success
                sample_violation("b"),   // blocked
                sample_retry("c"),       // failure
            ],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                outcome: Some("blocked".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.events[0].outcome, Outcome::Blocked);
    }

    // ── query — skill_id filter ───────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_filter_by_skill_id() {
        let (home, events) = fresh_dirs();
        let ev_with_skill =
            AuditEvent::now(AuditEventType::SkillActivate, "a", Outcome::Success)
                .with_skill_id("python-patterns");
        let ev_other_skill =
            AuditEvent::now(AuditEventType::SkillActivate, "a", Outcome::Success)
                .with_skill_id("golang-patterns");
        let ev_no_skill =
            AuditEvent::now(AuditEventType::SecurityScan, "a", Outcome::Success);

        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[ev_with_skill, ev_other_skill, ev_no_skill],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                skill_id: Some("python-patterns".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.events[0].skill_id.as_deref(), Some("python-patterns"));
    }

    // ── query — time range filters ────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_filter_by_since() {
        let (home, events) = fresh_dirs();

        let old_ev = {
            let mut e = sample_activate("a");
            e.timestamp = "2026-04-01T00:00:00Z".into();
            e
        };
        let new_ev = {
            let mut e = sample_activate("b");
            e.timestamp = "2026-04-29T12:00:00Z".into();
            e
        };

        write_jsonl(events.path(), "2026-04-01.jsonl", &[old_ev]);
        write_jsonl(events.path(), "2026-04-29.jsonl", &[new_ev]);
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                since: Some("2026-04-15T00:00:00Z".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.events[0].agent_id, "b");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_filter_by_until() {
        let (home, events) = fresh_dirs();

        let old_ev = {
            let mut e = sample_activate("a");
            e.timestamp = "2026-04-01T00:00:00Z".into();
            e
        };
        let new_ev = {
            let mut e = sample_activate("b");
            e.timestamp = "2026-04-29T12:00:00Z".into();
            e
        };

        write_jsonl(events.path(), "2026-04-01.jsonl", &[old_ev]);
        write_jsonl(events.path(), "2026-04-29.jsonl", &[new_ev]);
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                until: Some("2026-04-15T00:00:00Z".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.events[0].agent_id, "a");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_filter_since_until_range() {
        let (home, events) = fresh_dirs();

        let make_ev = |agent: &str, ts: &str| {
            let mut e = sample_activate(agent);
            e.timestamp = ts.into();
            e
        };

        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[
                make_ev("before", "2026-04-29T00:00:00Z"),
                make_ev("in-range", "2026-04-29T06:00:00Z"),
                make_ev("after", "2026-04-29T23:59:00Z"),
            ],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                since: Some("2026-04-29T03:00:00Z".into()),
                until: Some("2026-04-29T12:00:00Z".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.events[0].agent_id, "in-range");
    }

    // ── query — pagination ────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_pagination_limit_and_offset() {
        let (home, events) = fresh_dirs();
        let evs: Vec<AuditEvent> = (0..10)
            .map(|i| sample_activate(&format!("agent-{i:02}")))
            .collect();
        write_jsonl(events.path(), "2026-04-29.jsonl", &evs);
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        // First page.
        let page1 = idx
            .query(AuditQueryFilter {
                limit: Some(3),
                offset: Some(0),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(page1.total, 10, "total must be full count, not page size");
        assert_eq!(page1.events.len(), 3);
        assert_eq!(page1.limit, 3);
        assert_eq!(page1.offset, 0);

        // Second page.
        let page2 = idx
            .query(AuditQueryFilter {
                limit: Some(3),
                offset: Some(3),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(page2.events.len(), 3);
        // Pages must not overlap.
        let ids1: Vec<_> = page1.events.iter().map(|e| &e.agent_id).collect();
        let ids2: Vec<_> = page2.events.iter().map(|e| &e.agent_id).collect();
        assert!(
            ids1.iter().all(|id| !ids2.contains(id)),
            "pages must not overlap"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_limit_clamped_to_max() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);

        let result = idx
            .query(AuditQueryFilter {
                limit: Some(MAX_LIMIT + 999),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.limit, MAX_LIMIT);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_default_limit_applied() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);

        let result = idx.query(AuditQueryFilter::default()).await.unwrap();
        assert_eq!(result.limit, DEFAULT_LIMIT);
    }

    // ── query — combined filters ──────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_combined_agent_and_event_type() {
        let (home, events) = fresh_dirs();
        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[
                sample_activate("agent-x"),      // activate + agent-x
                sample_violation("agent-x"),     // violation + agent-x
                sample_activate("agent-y"),      // activate + agent-y
            ],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx
            .query(AuditQueryFilter {
                agent_id:   Some("agent-x".into()),
                event_type: Some("skill_activate".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.events[0].agent_id, "agent-x");
        assert_eq!(result.events[0].event_type, AuditEventType::SkillActivate);
    }

    // ── query — result integrity ──────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_result_preserves_metadata() {
        let (home, events) = fresh_dirs();
        let ev = sample_violation("agent-meta");
        write_jsonl(events.path(), "2026-04-29.jsonl", &[ev.clone()]);
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx.query(AuditQueryFilter::default()).await.unwrap();
        assert_eq!(result.events.len(), 1);
        let got = &result.events[0];
        assert_eq!(got.metadata["policy_id"], "default-rate-mcp");
        assert_eq!(got.metadata["policy_type"], "rate");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_result_preserves_skill_id_and_trigger_signal() {
        let (home, events) = fresh_dirs();
        let ev = sample_activate("agent-rich");
        // sample_activate sets skill_id and trigger_signal.
        write_jsonl(events.path(), "2026-04-29.jsonl", &[ev]);
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx.query(AuditQueryFilter::default()).await.unwrap();
        let got = &result.events[0];
        assert_eq!(got.skill_id.as_deref(), Some("python-patterns"));
        assert_eq!(got.trigger_signal.as_deref(), Some("manual_toggle"));
    }

    // ── query — ordered results ───────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn query_results_ordered_by_timestamp_asc() {
        let (home, events) = fresh_dirs();

        let make_ev = |agent: &str, ts: &str| {
            let mut e = sample_activate(agent);
            e.timestamp = ts.into();
            e
        };

        // Write out-of-order intentionally.
        write_jsonl(
            events.path(),
            "2026-04-29.jsonl",
            &[
                make_ev("c", "2026-04-29T12:00:00Z"),
                make_ev("a", "2026-04-29T08:00:00Z"),
                make_ev("b", "2026-04-29T10:00:00Z"),
            ],
        );
        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let result = idx.query(AuditQueryFilter::default()).await.unwrap();
        let agents: Vec<&str> = result.events.iter().map(|e| e.agent_id.as_str()).collect();
        assert_eq!(agents, vec!["a", "b", "c"], "results must be sorted by timestamp ASC");
    }

    // ── validate_col / allowlist (INFRA-SEC-H1) ───────────────────────────────

    #[test]
    fn validate_col_accepts_every_allowlisted_column() {
        for col in ALLOWED_FILTER_COLS {
            assert_eq!(
                validate_col(col),
                Ok(*col),
                "expected allowlisted column {col:?} to pass validation"
            );
        }
    }

    #[test]
    fn validate_col_rejects_unknown_columns() {
        let bad_cols = [
            "evil; DROP TABLE audit_index; --",
            "injected_col",
            "1=1",
            "",
            "AGENT_ID",  // case-sensitive: uppercase must be rejected
            "agent_id )",
        ];
        for col in &bad_cols {
            assert!(
                validate_col(col).is_err(),
                "expected column {col:?} to be rejected by validate_col"
            );
            assert_eq!(
                validate_col(col),
                Err(AuditQueryError::InvalidColumn(col.to_string())),
                "wrong error variant for column {col:?}"
            );
        }
    }

    #[test]
    fn build_filter_clause_succeeds_for_all_filter_fields() {
        // All six filter fields use allowlisted columns — must return Ok.
        let filter = AuditQueryFilter {
            agent_id:   Some("agent-x".into()),
            event_type: Some("skill_activate".into()),
            outcome:    Some("success".into()),
            skill_id:   Some("python-patterns".into()),
            since:      Some("2026-01-01T00:00:00Z".into()),
            until:      Some("2026-12-31T23:59:59Z".into()),
            ..Default::default()
        };
        let result = build_filter_clause(&filter);
        assert!(result.is_ok(), "all-fields filter must succeed: {result:?}");
        let fc = result.unwrap();
        assert!(
            fc.where_sql.starts_with("WHERE "),
            "non-empty filter must produce a WHERE clause"
        );
        assert_eq!(fc.params.len(), 6, "six filter fields → six params");
    }

    #[test]
    fn build_filter_clause_empty_filter_produces_empty_where() {
        let fc = build_filter_clause(&AuditQueryFilter::default()).unwrap();
        assert!(fc.where_sql.is_empty(), "empty filter must produce no WHERE clause");
        assert!(fc.params.is_empty());
    }

    // ── Integration: logger → sync → query ───────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn integration_logger_sync_query_roundtrip() {
        let (home, events) = fresh_dirs();

        // Write via the real EvolutionEventLogger.
        let logger = Arc::new(EvolutionEventLogger::new(events.path()));
        let ev = AuditEvent::now(AuditEventType::SkillGraduate, "agent-roundtrip", Outcome::Success)
            .with_skill_id("my-graduated-skill")
            .with_metadata(serde_json::json!({"quality_score": 0.91}));
        logger.log(ev).await;
        logger.flush().await.unwrap();

        let idx = open_index(&home, &events);
        let n = idx.sync_from_files().await.unwrap();
        assert_eq!(n, 1, "one event must be indexed");

        let result = idx
            .query(AuditQueryFilter {
                skill_id: Some("my-graduated-skill".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        let got = &result.events[0];
        assert_eq!(got.event_type, AuditEventType::SkillGraduate);
        assert_eq!(got.agent_id, "agent-roundtrip");
        assert_eq!(got.metadata["quality_score"], 0.91);
    }

    // ── Reliability Dashboard Integration Tests (W20-P0) ──────────────────────

    /// Helper: write a raw JSONL line (supports non-schema event types like
    /// `llm_fallback_triggered` that are stored in the DB as raw strings).
    fn write_raw_jsonl(events_dir: &Path, filename: &str, lines: &[&str]) {
        use std::io::Write as _;
        let path = events_dir.join(filename);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    /// Build a minimal JSONL event line with the given event_type and outcome.
    fn jsonl_line(agent_id: &str, event_type: &str, outcome: &str) -> String {
        serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event_type": event_type,
            "agent_id": agent_id,
            "skill_id": null,
            "generation": null,
            "outcome": outcome,
            "trigger_signal": null,
            "metadata": {}
        })
        .to_string()
    }

    /// All events succeed → task_success_rate = 1.0, consistency_score = 1.0
    #[tokio::test]
    async fn reliability_all_success() {
        let (home, events) = fresh_dirs();
        let lines: Vec<String> = (0..5)
            .map(|_| jsonl_line("agent-ok", "skill_activate", "success"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        write_raw_jsonl(events.path(), "2026-05-01.jsonl", &refs);

        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let s = idx.compute_reliability_summary("agent-ok", 7).await.unwrap();
        assert_eq!(s.agent_id, "agent-ok");
        assert_eq!(s.window_days, 7);
        assert_eq!(s.total_events, 5);
        assert!((s.task_success_rate - 1.0).abs() < 1e-9, "tsr={}", s.task_success_rate);
        assert!((s.consistency_score - 1.0).abs() < 1e-9, "cs={}", s.consistency_score);
        assert!((s.skill_adoption_rate - 1.0).abs() < 1e-9, "sar={}", s.skill_adoption_rate);
        assert!((s.fallback_trigger_rate - 0.0).abs() < 1e-9, "ftr={}", s.fallback_trigger_rate);
    }

    /// No events for agent → all defaults apply
    #[tokio::test]
    async fn reliability_no_events_uses_defaults() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);
        // no sync needed — empty index

        let s = idx
            .compute_reliability_summary("ghost-agent", 7)
            .await
            .unwrap();
        assert_eq!(s.total_events, 0);
        assert_eq!(s.consistency_score, 1.0, "empty → neutral 1.0");
        assert_eq!(s.task_success_rate, 1.0, "empty → neutral 1.0");
        assert_eq!(s.skill_adoption_rate, 0.0, "empty → 0.0");
        assert_eq!(s.fallback_trigger_rate, 0.0, "empty → 0.0");
    }

    /// Mixed outcomes → task_success_rate < 1.0 and consistency_score < 1.0
    #[tokio::test]
    async fn reliability_mixed_outcomes() {
        let (home, events) = fresh_dirs();
        // 6 events for "agent-mixed": 4 success, 2 failure (same event_type)
        let mut lines = Vec::new();
        for _ in 0..4 {
            lines.push(jsonl_line("agent-mixed", "gvu_generation", "success"));
        }
        for _ in 0..2 {
            lines.push(jsonl_line("agent-mixed", "gvu_generation", "failure"));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        write_raw_jsonl(events.path(), "2026-05-01.jsonl", &refs);

        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let s = idx
            .compute_reliability_summary("agent-mixed", 7)
            .await
            .unwrap();
        assert_eq!(s.total_events, 6);
        // 4/6 ≈ 0.6667
        assert!((s.task_success_rate - 4.0 / 6.0).abs() < 1e-6, "tsr={}", s.task_success_rate);
        // single event type: consistency = 4/6
        assert!((s.consistency_score - 4.0 / 6.0).abs() < 1e-6, "cs={}", s.consistency_score);
    }

    /// Skill adoption rate: 3/10 events are skill_activate
    #[tokio::test]
    async fn reliability_skill_adoption_rate() {
        let (home, events) = fresh_dirs();
        let mut lines = Vec::new();
        for _ in 0..3 {
            lines.push(jsonl_line("agent-skill", "skill_activate", "success"));
        }
        for _ in 0..7 {
            lines.push(jsonl_line("agent-skill", "gvu_generation", "success"));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        write_raw_jsonl(events.path(), "2026-05-01.jsonl", &refs);

        let idx = open_index(&home, &events);
        idx.sync_from_files().await.unwrap();

        let s = idx
            .compute_reliability_summary("agent-skill", 7)
            .await
            .unwrap();
        assert_eq!(s.total_events, 10);
        assert!((s.skill_adoption_rate - 0.3).abs() < 1e-9, "sar={}", s.skill_adoption_rate);
    }

    /// Fallback trigger rate: 2/10 events are llm_fallback_triggered
    /// NOTE: this writes the event as a raw JSONL string (bypassing enum deserialization)
    /// to simulate what llm_fallback.rs will write when integrated with the evolution logger.
    #[tokio::test]
    async fn reliability_fallback_trigger_rate() {
        let (home, events) = fresh_dirs();
        let mut lines = Vec::new();
        for _ in 0..2 {
            lines.push(jsonl_line(
                "agent-fallback",
                "llm_fallback_triggered",
                "failure",
            ));
        }
        for _ in 0..8 {
            lines.push(jsonl_line("agent-fallback", "gvu_generation", "success"));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        write_raw_jsonl(events.path(), "2026-05-01.jsonl", &refs);

        let idx = open_index(&home, &events);
        // sync manually to insert raw rows (llm_fallback_triggered will be skipped
        // by sync_from_files because it's not a known AuditEventType; the aggregate
        // query counts it from the raw DB string — see query.rs implementation)
        // Instead, we insert rows directly to the SQLite index to simulate future
        // integration where llm_fallback emits to EvolutionEventLogger.
        // For now, test that the compute function handles existing rows correctly.
        let _ = idx.sync_from_files().await;

        // Insert llm_fallback_triggered rows manually (bypassing JSONL deserialization)
        // to simulate the future state where these events appear in the index.
        {
            let conn = idx.conn.lock().await;
            let ts = chrono::Utc::now().to_rfc3339();
            for _ in 0..2 {
                conn.execute(
                    "INSERT INTO audit_index
                     (timestamp, event_type, agent_id, skill_id, generation,
                      outcome, trigger_signal, metadata, source_file)
                     VALUES (?1, 'llm_fallback_triggered', 'agent-fallback', NULL, NULL,
                             'failure', NULL, '{}', 'synthetic')",
                    params![ts],
                )
                .unwrap();
            }
        }

        let s = idx
            .compute_reliability_summary("agent-fallback", 7)
            .await
            .unwrap();
        // gvu events were indexed (8), fallback inserted manually (2) → total = 10
        assert_eq!(s.total_events, 10, "total={}", s.total_events);
        assert!((s.fallback_trigger_rate - 0.2).abs() < 1e-9, "ftr={}", s.fallback_trigger_rate);
    }

    /// Window filtering: events outside window_days are excluded
    #[tokio::test]
    async fn reliability_window_filters_old_events() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);

        // Insert events with timestamps: 1 recent (today) and 1 old (30 days ago)
        {
            let conn = idx.conn.lock().await;
            let now = chrono::Utc::now().to_rfc3339();
            let old = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();

            conn.execute(
                "INSERT INTO audit_index
                 (timestamp, event_type, agent_id, skill_id, generation,
                  outcome, trigger_signal, metadata, source_file)
                 VALUES (?1, 'skill_activate', 'agent-window', NULL, NULL,
                         'success', NULL, '{}', 'synthetic')",
                params![now],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO audit_index
                 (timestamp, event_type, agent_id, skill_id, generation,
                  outcome, trigger_signal, metadata, source_file)
                 VALUES (?1, 'skill_activate', 'agent-window', NULL, NULL,
                         'failure', NULL, '{}', 'synthetic')",
                params![old],
            )
            .unwrap();
        }

        // 7-day window: only the recent event counts
        let s7 = idx
            .compute_reliability_summary("agent-window", 7)
            .await
            .unwrap();
        assert_eq!(s7.total_events, 1, "7-day window should include only 1 event");
        assert!((s7.task_success_rate - 1.0).abs() < 1e-9, "recent event is success");

        // 60-day window: both events count
        let s60 = idx
            .compute_reliability_summary("agent-window", 60)
            .await
            .unwrap();
        assert_eq!(s60.total_events, 2, "60-day window should include both events");
        assert!((s60.task_success_rate - 0.5).abs() < 1e-9, "1 success + 1 failure = 0.5");
    }

    /// Agent isolation: other agents' events must not pollute results
    #[tokio::test]
    async fn reliability_agent_isolation() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);

        {
            let conn = idx.conn.lock().await;
            let ts = chrono::Utc::now().to_rfc3339();
            // agent-a: 5 failures
            for _ in 0..5 {
                conn.execute(
                    "INSERT INTO audit_index
                     (timestamp, event_type, agent_id, skill_id, generation,
                      outcome, trigger_signal, metadata, source_file)
                     VALUES (?1, 'gvu_generation', 'agent-a', NULL, NULL,
                             'failure', NULL, '{}', 'synthetic')",
                    params![ts],
                )
                .unwrap();
            }
            // agent-b: 5 successes
            for _ in 0..5 {
                conn.execute(
                    "INSERT INTO audit_index
                     (timestamp, event_type, agent_id, skill_id, generation,
                      outcome, trigger_signal, metadata, source_file)
                     VALUES (?1, 'gvu_generation', 'agent-b', NULL, NULL,
                             'success', NULL, '{}', 'synthetic')",
                    params![ts],
                )
                .unwrap();
            }
        }

        let sa = idx.compute_reliability_summary("agent-a", 7).await.unwrap();
        let sb = idx.compute_reliability_summary("agent-b", 7).await.unwrap();

        assert_eq!(sa.total_events, 5);
        assert!((sa.task_success_rate - 0.0).abs() < 1e-9, "agent-a all failure");

        assert_eq!(sb.total_events, 5);
        assert!((sb.task_success_rate - 1.0).abs() < 1e-9, "agent-b all success");
    }

    /// Consistency score with multiple event types
    #[tokio::test]
    async fn reliability_consistency_multiple_types() {
        let (home, events) = fresh_dirs();
        let idx = open_index(&home, &events);

        {
            let conn = idx.conn.lock().await;
            let ts = chrono::Utc::now().to_rfc3339();
            // type_a: 10/10 success → rate = 1.0
            for _ in 0..10 {
                conn.execute(
                    "INSERT INTO audit_index
                     (timestamp, event_type, agent_id, skill_id, generation,
                      outcome, trigger_signal, metadata, source_file)
                     VALUES (?1, 'skill_activate', 'agent-cs', NULL, NULL,
                             'success', NULL, '{}', 'synthetic')",
                    params![ts],
                )
                .unwrap();
            }
            // type_b: 5/10 success → rate = 0.5
            for i in 0..10 {
                let outcome = if i < 5 { "success" } else { "failure" };
                conn.execute(
                    "INSERT INTO audit_index
                     (timestamp, event_type, agent_id, skill_id, generation,
                      outcome, trigger_signal, metadata, source_file)
                     VALUES (?1, 'gvu_generation', 'agent-cs', NULL, NULL,
                             ?2, NULL, '{}', 'synthetic')",
                    params![ts, outcome],
                )
                .unwrap();
            }
        }

        let s = idx.compute_reliability_summary("agent-cs", 7).await.unwrap();
        assert_eq!(s.total_events, 20);
        // consistency = avg(1.0, 0.5) = 0.75
        assert!(
            (s.consistency_score - 0.75).abs() < 1e-9,
            "cs={}",
            s.consistency_score
        );
        // task_success_rate = 15/20 = 0.75
        assert!(
            (s.task_success_rate - 0.75).abs() < 1e-9,
            "tsr={}",
            s.task_success_rate
        );
        // skill_adoption = 10/20 = 0.5
        assert!(
            (s.skill_adoption_rate - 0.5).abs() < 1e-9,
            "sar={}",
            s.skill_adoption_rate
        );
    }
}
