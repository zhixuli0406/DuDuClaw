//! SQLite-backed persistent store for autopilot (event-driven automation) rules.
//!
//! Each rule defines a trigger event, conditions, and an action to execute.
//! WAL mode + 5s busy_timeout for multi-process safety.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
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
    /// P3-3 lightweight CEP: optional JSON-encoded
    /// `cep_matcher::SequenceSpec` (`{"first":{...},"then":{...},
    /// "within_secs":N,"negate":bool}`). `None` for ordinary single-event
    /// rules. When `Some`, `AutopilotEngine::process_event` never dispatches
    /// this rule via the literal `trigger_event`/`conditions` path — it fires
    /// exclusively through `CepMatcher`'s synthetic `CepTrigger` event once
    /// the temporal pattern resolves. `trigger_event` is still required (by
    /// the existing dashboard write-time validator) but is an unused
    /// placeholder for sequence rules.
    pub sequence: Option<String>,
    /// P4-1 PBD rule induction: optional JSON metadata blob. Induced rules
    /// carry `{"induced":true,"induced_at":"<rfc3339>","fingerprint":"…",
    /// "source":"pbd_rule_induction"}` so the dashboard can badge them and a
    /// future lifecycle pass can retire them. `None` for hand-authored rules.
    pub metadata: Option<String>,
}

// ── History row ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutopilotHistoryRow {
    pub id: String,
    pub rule_id: String,
    pub rule_name: String,
    pub triggered_at: String,
    /// `success` / `failure` / `skipped`, the v1.82 approval-gate outcomes
    /// (`autopilot_approval::RESULT_*`), breaker transitions (`circuit_*`),
    /// screening (`screen_*`) or `paused_by_human`.
    pub result: String,
    pub details: Option<String>,
}

// ── Approval-gated dispatch ledger (v1.82) ──────────────────

/// DDL of the approval-gated dispatch ledger. Must stay byte-identical to the
/// `autopilot_approval_dispatch` statements at the end of
/// `crates/duduclaw-import/src/autopilot-v182.sql` (the importer compares
/// `sqlite_schema` exactly; `migration_import`'s catalog test pins the two).
///
/// `dedupe_key` identifies (ingress event, rule, rule revision): one ingress
/// event files at most one dispatch row per rule revision (see
/// [`AutopilotStore::insert_dispatch`]).
///
/// The `notice_*` columns are the outbox of the outcome announcements
/// (v1.82 review: terminal status and notices durable together). The same
/// transaction that settles a row writes its history row and sets
/// `notice_activity_pending = 1` (the Activity Feed entry is still owed) and,
/// when the outcome is pushed to chat, `notice_push_state = 'pending'`. The
/// engine clears them only after the write / push succeeded; boot recovery
/// and the lock holder's periodic sweep re-drive every row still owing one.
/// `notice_push_attempts` / `notice_push_next_at` are the push's retry
/// counter and its next-attempt time (also the in-flight lease);
/// `notice_pushed_at` is when the push was sent or durably queued.
pub(crate) const DISPATCH_SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS autopilot_approval_dispatch (
    dispatch_id   TEXT PRIMARY KEY,
    approval_id   TEXT UNIQUE,
    rule_id       TEXT NOT NULL,
    rule_name     TEXT NOT NULL,
    rule_hash     TEXT NOT NULL,
    event_name    TEXT NOT NULL,
    event_ref     TEXT NOT NULL,
    dedupe_key    TEXT NOT NULL,
    frozen_json   TEXT NOT NULL,
    frozen_digest TEXT NOT NULL,
    state         TEXT NOT NULL,
    outcome       TEXT,
    detail        TEXT,
    notice_activity_pending INTEGER NOT NULL DEFAULT 0,
    notice_push_state TEXT,
    notice_push_attempts INTEGER NOT NULL DEFAULT 0,
    notice_push_next_at TEXT,
    notice_pushed_at TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ap_dispatch_state ON autopilot_approval_dispatch(state);
CREATE INDEX IF NOT EXISTS idx_ap_dispatch_dedupe ON autopilot_approval_dispatch(dedupe_key);
CREATE INDEX IF NOT EXISTS idx_ap_dispatch_notice ON autopilot_approval_dispatch(notice_activity_pending, notice_push_state);";

/// `notice_push_state`: the outcome's chat push is owed (not yet sent).
pub const PUSH_PENDING: &str = "pending";
/// Sent to at least one Admin's verified channel.
pub const PUSH_SENT: &str = "sent";
/// Durably queued for after quiet hours (`notify_governance`).
pub const PUSH_QUEUED: &str = "queued";
/// No Admin has a reachable verified channel: dashboard only, nothing to retry.
pub const PUSH_NO_TARGET: &str = "no_target";
/// Held back by the per-rule hourly limiter (limited outcomes only).
pub const PUSH_SUPPRESSED: &str = "suppressed";
/// Every bounded retry failed; the Activity Feed entry remains.
pub const PUSH_GAVE_UP: &str = "gave_up";

/// `autopilot_history.id` of a dispatch's outcome row — one per dispatch
/// (`INSERT OR IGNORE`, written in the settling transaction).
pub fn dispatch_history_id(dispatch_id: &str) -> String {
    format!("dispatch-outcome:{dispatch_id}")
}

/// Row inserted before the approval is filed; no approval id yet.
pub const DISPATCH_REQUESTING: &str = "requesting";
/// The approval is filed and pending (or decided but not yet acted on).
pub const DISPATCH_AWAITING: &str = "awaiting";
/// Claimed by the executor. Nothing runs before the approval is consumed.
pub const DISPATCH_EXECUTING: &str = "executing";
/// Terminal; `outcome` says how it ended.
pub const DISPATCH_SETTLED: &str = "settled";

/// One approval-gated fire: the exact action a human is asked to approve,
/// frozen at trigger time. `frozen_digest` binds the approval card's payload
/// to this row (see `autopilot_approval`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutopilotDispatchRow {
    pub dispatch_id: String,
    pub approval_id: Option<String>,
    pub rule_id: String,
    pub rule_name: String,
    pub rule_hash: String,
    pub event_name: String,
    pub event_ref: String,
    /// See [`DISPATCH_SCHEMA_SQL`] (`autopilot_approval::dedupe_key`).
    pub dedupe_key: String,
    pub frozen_json: String,
    pub frozen_digest: String,
    pub state: String,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    /// The outcome's Activity Feed entry is still owed (see [`DISPATCH_SCHEMA_SQL`]).
    pub notice_activity_pending: bool,
    /// `None` (no chat push for this outcome) or one of the `PUSH_*` states.
    pub notice_push_state: Option<String>,
    pub notice_push_attempts: i64,
    /// Earliest next push attempt (also the lease of an attempt in flight).
    pub notice_push_next_at: Option<String>,
    /// When the outcome's chat push was sent or durably queued.
    pub notice_pushed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const DISPATCH_COLUMNS: &str = "dispatch_id, approval_id, rule_id, rule_name, rule_hash, event_name, \
     event_ref, dedupe_key, frozen_json, frozen_digest, state, outcome, detail, \
     notice_activity_pending, notice_push_state, notice_push_attempts, notice_push_next_at, \
     notice_pushed_at, created_at, updated_at";

/// Run `f` inside `BEGIN IMMEDIATE` on `conn`: commit on `Ok`, roll back on
/// `Err` (the connection is never left inside a transaction).
fn in_immediate_tx<T>(
    conn: &Connection,
    what: &str,
    f: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| format!("{what}: {e}"))?;
    match f(conn) {
        Ok(value) => {
            if let Err(e) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(format!("{what}: {e}"));
            }
            Ok(value)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// SELECT list of one rule (shared by `get_rule` and the rule fence).
const RULE_SELECT: &str = "SELECT id, name, enabled, trigger_event, conditions, action,
                    created_at, last_triggered_at, trigger_count, sequence, metadata
             FROM autopilot_rules WHERE id = ?1";

// ── Store ───────────────────────────────────────────────────

pub struct AutopilotStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

/// v1.82 (M2): a write-locked view of `autopilot.db` held across the final
/// check of a rule and the consumption of its approval.
///
/// Created by [`AutopilotStore::fence_rule`]: holds this store's connection
/// (so every in-process rule write waits) inside `BEGIN IMMEDIATE` (so every
/// other process's rule write waits on SQLite's write lock, up to its busy
/// timeout). Rule update / delete / disable therefore cannot land between
/// the executor's final `enabled` + revision check and the point of no
/// return. [`RuleFence::release`] commits (nothing is written under it);
/// dropping it rolls back. Never call another store method while holding
/// one — it would wait on the same connection.
pub struct RuleFence<'a> {
    conn: tokio::sync::MutexGuard<'a, Connection>,
    open: bool,
}

impl RuleFence<'_> {
    /// End the fence (commit the empty write transaction).
    pub fn release(mut self) -> Result<(), String> {
        self.open = false;
        match self.conn.execute_batch("COMMIT") {
            Ok(()) => Ok(()),
            Err(e) => {
                // Never leave the shared connection inside a transaction.
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(format!("release rule fence: {e}"))
            }
        }
    }
}

impl Drop for RuleFence<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

impl AutopilotStore {
    /// Open (or create) `<home>/autopilot.db` with the same owner-only,
    /// no-follow hardening as `approvals.db`
    /// ([`crate::approval::open_private_sqlite`]): the ledger holds full
    /// rendered actions awaiting approval. An insecure path (symlink, not a
    /// regular file, permissions that cannot be set) is refused; the caller
    /// then starts no autopilot engine (fail closed).
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let (conn, db_path) =
            crate::approval::open_private_sqlite(home_dir, "autopilot.db", "autopilot")?;
        Self::init_schema(&conn)?;
        crate::approval::secure_private_sidecars(&db_path, "autopilot")?;
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

        // P3-3 additive migration: optional CEP sequence pattern spec (JSON).
        // Idempotent — a "duplicate column name" error on an already-migrated
        // DB is expected and ignored (same pattern as cost_telemetry.rs /
        // session.rs / custom_skills.rs).
        if let Err(e) = conn.execute("ALTER TABLE autopilot_rules ADD COLUMN sequence TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(format!("autopilot_rules sequence migration failed: {e}"));
            }
        }

        // P4-1 additive migration: optional JSON metadata blob (induced-rule
        // provenance). Same idempotent "duplicate column name" tolerance as the
        // `sequence` migration above.
        if let Err(e) = conn.execute("ALTER TABLE autopilot_rules ADD COLUMN metadata TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(format!("autopilot_rules metadata migration failed: {e}"));
            }
        }

        // v1.82 approval gate: the durable ledger of approval-gated fires.
        conn.execute_batch(DISPATCH_SCHEMA_SQL)
            .map_err(|e| format!("init autopilot approval dispatch schema: {e}"))?;
        Ok(())
    }

    // ── Rules CRUD ──────────────────────────────────────────

    pub async fn list_rules(&self) -> Result<Vec<AutopilotRuleRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, enabled, trigger_event, conditions, action,
                        created_at, last_triggered_at, trigger_count, sequence, metadata
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
        conn.query_row(RULE_SELECT, params![id], row_to_rule)
            .optional()
            .map_err(|e| format!("get rule: {e}"))
    }

    /// v1.82 (M2): take the rule fence ([`RuleFence`]) and read rule `id`
    /// under it. Every rule write ([`Self::update_rule`],
    /// [`Self::remove_rule`], any other process's write to `autopilot.db`)
    /// waits until the fence is released or dropped.
    pub async fn fence_rule(&self, id: &str) -> Result<(RuleFence<'_>, Option<AutopilotRuleRow>), String> {
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("take rule fence: {e}"))?;
        let fence = RuleFence { conn, open: true };
        // On error the fence drops here and rolls back.
        let rule = fence
            .conn
            .query_row(RULE_SELECT, params![id], row_to_rule)
            .optional()
            .map_err(|e| format!("read rule under fence: {e}"))?;
        Ok((fence, rule))
    }

    pub async fn insert_rule(&self, row: &AutopilotRuleRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO autopilot_rules
                (id, name, enabled, trigger_event, conditions, action,
                 created_at, last_triggered_at, trigger_count, sequence, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                row.sequence,
                row.metadata,
            ],
        )
        .map_err(|e| format!("insert rule: {e}"))?;
        Ok(())
    }

    /// Rule writes (this, [`Self::remove_rule`]) wait while an approved
    /// autopilot action holds the [`RuleFence`] for its final check (v1.82).
    pub async fn update_rule(
        &self,
        id: &str,
        fields: &serde_json::Value,
    ) -> Result<Option<AutopilotRuleRow>, String> {
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
            // P3-3: `sequence` accepts either a JSON object (stored as its
            // string form, mirroring `conditions`/`action`) or an explicit
            // `null` to clear a rule back to ordinary single-event dispatch.
            // `null` needs no bind parameter — it's a hardcoded SQL literal,
            // not caller-controlled, so there is no injection concern.
            if let Some(v) = fields.get("sequence") {
                if v.is_null() {
                    sets.push("sequence = NULL".to_string());
                } else {
                    binds.push(v.to_string());
                    sets.push(format!("sequence = ?{}", binds.len()));
                }
            }
            // P4-1: `metadata` follows the same object-or-null contract as
            // `sequence`. Hardcoded NULL literal — not caller-controlled — so
            // no injection concern.
            if let Some(v) = fields.get("metadata") {
                if v.is_null() {
                    sets.push("metadata = NULL".to_string());
                } else {
                    binds.push(v.to_string());
                    sets.push(format!("metadata = ?{}", binds.len()));
                }
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
                let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds
                    .iter()
                    .map(|s| s as &dyn rusqlite::types::ToSql)
                    .collect();
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
        let _ = conn.execute(
            "DELETE FROM autopilot_history WHERE rule_id = ?1",
            params![id],
        );
        Ok(count > 0)
    }

    // ── History ─────────────────────────────────────────────

    pub async fn list_history(
        &self,
        rule_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AutopilotHistoryRow>, String> {
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

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare history: {e}"))?;
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
            params![
                row.id,
                row.rule_id,
                row.rule_name,
                row.triggered_at,
                row.result,
                row.details
            ],
        )
        .map_err(|e| format!("append history: {e}"))?;

        // Bump trigger_count + last_triggered_at
        let _ = conn.execute(
            "UPDATE autopilot_rules SET trigger_count = trigger_count + 1, last_triggered_at = ?1 WHERE id = ?2",
            params![row.triggered_at, row.rule_id],
        );
        Ok(())
    }


    // ── Approval-gated dispatch ledger (v1.82) ──────────────

    /// Insert a fresh ledger row (state [`DISPATCH_REQUESTING`]) unless a row
    /// with the same `dedupe_key` was created within the last
    /// [`crate::autopilot_approval::DEDUPE_WINDOW_SECS`] (any state).
    ///
    /// `Ok(true)`: inserted. `Ok(false)`: the same ingress event already filed
    /// a dispatch for this rule revision — nothing inserted. The check and
    /// the insert run in one `BEGIN IMMEDIATE` transaction, so two writers
    /// (two gateway processes on one home) cannot both insert.
    pub async fn insert_dispatch(&self, row: &AutopilotDispatchRow) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let cutoff = (chrono::Utc::now()
            - chrono::Duration::seconds(crate::autopilot_approval::DEDUPE_WINDOW_SECS))
        .to_rfc3339();
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| format!("insert approval dispatch: {e}"))?;
        let result = (|| -> Result<bool, String> {
            let dup: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM autopilot_approval_dispatch
                     WHERE dedupe_key = ?1 AND julianday(created_at) >= julianday(?2)",
                    params![row.dedupe_key, cutoff],
                    |r| r.get(0),
                )
                .map_err(|e| format!("check approval dispatch dedupe: {e}"))?;
            if dup > 0 {
                return Ok(false);
            }
            conn.execute(
                &format!(
                    "INSERT INTO autopilot_approval_dispatch ({DISPATCH_COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                             ?17, ?18, ?19, ?20)"
                ),
                params![
                    row.dispatch_id,
                    row.approval_id,
                    row.rule_id,
                    row.rule_name,
                    row.rule_hash,
                    row.event_name,
                    row.event_ref,
                    row.dedupe_key,
                    row.frozen_json,
                    row.frozen_digest,
                    row.state,
                    row.outcome,
                    row.detail,
                    row.notice_activity_pending,
                    row.notice_push_state,
                    row.notice_push_attempts,
                    row.notice_push_next_at,
                    row.notice_pushed_at,
                    row.created_at,
                    row.updated_at,
                ],
            )
            .map_err(|e| format!("insert approval dispatch: {e}"))?;
            Ok(true)
        })();
        match result {
            Ok(inserted) => {
                if let Err(e) = conn.execute_batch("COMMIT") {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(format!("insert approval dispatch: {e}"));
                }
                Ok(inserted)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    // ── Outcome announcements (outbox) ──────────────────────

    /// Settled rows that still owe an announcement now: the Activity Feed
    /// entry, or a pending chat push whose next attempt is due (`now`,
    /// RFC 3339). Oldest first, at most `limit`.
    pub async fn list_dispatch_notices_due(&self, now: &str, limit: i64) -> Result<Vec<AutopilotDispatchRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {DISPATCH_COLUMNS} FROM autopilot_approval_dispatch
                 WHERE state = ?1
                   AND (notice_activity_pending = 1
                        OR (notice_push_state = ?2
                            AND (notice_push_next_at IS NULL
                                 OR julianday(notice_push_next_at) <= julianday(?3))))
                 ORDER BY updated_at ASC LIMIT ?4"
            ))
            .map_err(|e| format!("prepare list dispatch notices: {e}"))?;
        let rows = stmt
            .query_map(params![DISPATCH_SETTLED, PUSH_PENDING, now, limit.max(1)], row_to_dispatch)
            .map_err(|e| format!("query dispatch notices: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect dispatch notices: {e}"))?;
        Ok(rows)
    }

    /// The outcome's Activity Feed entry exists: clear the flag. `Ok(true)`
    /// only for the caller that cleared it.
    pub async fn mark_dispatch_activity_done(&self, dispatch_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE autopilot_approval_dispatch SET notice_activity_pending = 0
                 WHERE dispatch_id = ?1 AND notice_activity_pending = 1",
                params![dispatch_id],
            )
            .map_err(|e| format!("mark dispatch activity done: {e}"))?;
        Ok(n == 1)
    }

    /// Claim one attempt of the outcome's chat push: compare-and-set on the
    /// attempt counter the caller saw, only while the push is pending and
    /// due (`now`). Bumps the counter and sets `notice_push_next_at` to
    /// `lease_until`, so no other sweep (in this or another process) starts
    /// a second attempt while this one is in flight; an attempt interrupted
    /// by a crash becomes due again when the lease runs out.
    pub async fn claim_dispatch_notice_push(
        &self,
        dispatch_id: &str,
        attempts_seen: i64,
        now: &str,
        lease_until: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE autopilot_approval_dispatch
                 SET notice_push_attempts = notice_push_attempts + 1, notice_push_next_at = ?1
                 WHERE dispatch_id = ?2 AND notice_push_state = ?3 AND notice_push_attempts = ?4
                   AND (notice_push_next_at IS NULL OR julianday(notice_push_next_at) <= julianday(?5))",
                params![lease_until, dispatch_id, PUSH_PENDING, attempts_seen, now],
            )
            .map_err(|e| format!("claim dispatch notice push: {e}"))?;
        Ok(n == 1)
    }

    /// End a pending push in `state` (`PUSH_SENT` / `PUSH_QUEUED` record
    /// `notice_pushed_at`). `Ok(false)` when it was no longer pending.
    pub async fn finish_dispatch_notice_push(&self, dispatch_id: &str, state: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let delivered = state == PUSH_SENT || state == PUSH_QUEUED;
        let n = conn
            .execute(
                "UPDATE autopilot_approval_dispatch
                 SET notice_push_state = ?1, notice_push_next_at = NULL,
                     notice_pushed_at = CASE WHEN ?2 THEN ?3 ELSE notice_pushed_at END
                 WHERE dispatch_id = ?4 AND notice_push_state = ?5",
                params![state, delivered, chrono::Utc::now().to_rfc3339(), dispatch_id, PUSH_PENDING],
            )
            .map_err(|e| format!("finish dispatch notice push: {e}"))?;
        Ok(n == 1)
    }

    /// A push attempt failed: keep it pending, next attempt at `next_at`.
    pub async fn defer_dispatch_notice_push(&self, dispatch_id: &str, next_at: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE autopilot_approval_dispatch SET notice_push_next_at = ?1
                 WHERE dispatch_id = ?2 AND notice_push_state = ?3",
                params![next_at, dispatch_id, PUSH_PENDING],
            )
            .map_err(|e| format!("defer dispatch notice push: {e}"))?;
        Ok(n == 1)
    }

    pub async fn get_dispatch(&self, dispatch_id: &str) -> Result<Option<AutopilotDispatchRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {DISPATCH_COLUMNS} FROM autopilot_approval_dispatch WHERE dispatch_id = ?1"),
            params![dispatch_id],
            row_to_dispatch,
        )
        .optional()
        .map_err(|e| format!("get approval dispatch: {e}"))
    }

    /// Rows in `state`, oldest first, at most `limit`.
    pub async fn list_dispatches(&self, state: &str, limit: i64) -> Result<Vec<AutopilotDispatchRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {DISPATCH_COLUMNS} FROM autopilot_approval_dispatch
                 WHERE state = ?1 ORDER BY created_at ASC LIMIT ?2"
            ))
            .map_err(|e| format!("prepare list dispatches: {e}"))?;
        let rows = stmt
            .query_map(params![state, limit.max(1)], row_to_dispatch)
            .map_err(|e| format!("query dispatches: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect dispatches: {e}"))?;
        Ok(rows)
    }

    /// [`DISPATCH_REQUESTING`] → [`DISPATCH_AWAITING`] with the filed
    /// approval id. `false` when the row is no longer requesting.
    pub async fn mark_dispatch_awaiting(&self, dispatch_id: &str, approval_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE autopilot_approval_dispatch SET state = ?1, approval_id = ?2, updated_at = ?3
                 WHERE dispatch_id = ?4 AND state = ?5",
                params![
                    DISPATCH_AWAITING,
                    approval_id,
                    chrono::Utc::now().to_rfc3339(),
                    dispatch_id,
                    DISPATCH_REQUESTING
                ],
            )
            .map_err(|e| format!("mark dispatch awaiting: {e}"))?;
        Ok(n == 1)
    }

    /// Compare-and-set the ledger state (`from` → `to`). `true` only for the
    /// caller that moved the row — the single-use claim of an execution.
    pub async fn transition_dispatch(&self, dispatch_id: &str, from: &str, to: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE autopilot_approval_dispatch SET state = ?1, updated_at = ?2
                 WHERE dispatch_id = ?3 AND state = ?4",
                params![to, chrono::Utc::now().to_rfc3339(), dispatch_id, from],
            )
            .map_err(|e| format!("transition dispatch: {e}"))?;
        Ok(n == 1)
    }

    /// `from` → [`DISPATCH_SETTLED`] with an outcome, in ONE transaction
    /// with everything that must survive a crash right after it:
    ///
    /// - the dispatch's outcome history row ([`dispatch_history_id`],
    ///   `INSERT OR IGNORE`: exactly one per dispatch), without bumping the
    ///   rule's trigger counter unless `count_fire` (a fire that never got as
    ///   far as `awaiting_approval`, so no earlier row counted it);
    /// - the announcements still owed: `notice_activity_pending = 1`, and
    ///   `notice_push_state = 'pending'` when `push`.
    ///
    /// `Ok(false)` when the row was not in `from` (someone else settled or
    /// claimed it) — nothing is written then.
    pub async fn settle_dispatch(
        &self,
        dispatch_id: &str,
        from: &str,
        outcome: &str,
        detail: &str,
        push: bool,
        count_fire: bool,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let now = chrono::Utc::now().to_rfc3339();
        in_immediate_tx(&conn, "settle dispatch", |conn| {
            let n = conn
                .execute(
                    "UPDATE autopilot_approval_dispatch
                     SET state = ?1, outcome = ?2, detail = ?3, updated_at = ?4,
                         notice_activity_pending = 1, notice_push_state = ?5,
                         notice_push_attempts = 0, notice_push_next_at = NULL
                     WHERE dispatch_id = ?6 AND state = ?7",
                    params![
                        DISPATCH_SETTLED,
                        outcome,
                        detail,
                        now,
                        push.then_some(PUSH_PENDING),
                        dispatch_id,
                        from
                    ],
                )
                .map_err(|e| format!("settle dispatch: {e}"))?;
            if n != 1 {
                return Ok(false);
            }
            conn.execute(
                "INSERT OR IGNORE INTO autopilot_history (id, rule_id, rule_name, triggered_at, result, details)
                 SELECT ?1, rule_id, rule_name, ?2, outcome, detail
                 FROM autopilot_approval_dispatch WHERE dispatch_id = ?3",
                params![dispatch_history_id(dispatch_id), now, dispatch_id],
            )
            .map_err(|e| format!("settle dispatch history: {e}"))?;
            if count_fire {
                conn.execute(
                    "UPDATE autopilot_rules SET trigger_count = trigger_count + 1, last_triggered_at = ?1
                     WHERE id = (SELECT rule_id FROM autopilot_approval_dispatch WHERE dispatch_id = ?2)",
                    params![now, dispatch_id],
                )
                .map_err(|e| format!("settle dispatch trigger count: {e}"))?;
            }
            Ok(true)
        })
    }
}

fn row_to_dispatch(row: &rusqlite::Row) -> rusqlite::Result<AutopilotDispatchRow> {
    Ok(AutopilotDispatchRow {
        dispatch_id: row.get(0)?,
        approval_id: row.get(1)?,
        rule_id: row.get(2)?,
        rule_name: row.get(3)?,
        rule_hash: row.get(4)?,
        event_name: row.get(5)?,
        event_ref: row.get(6)?,
        dedupe_key: row.get(7)?,
        frozen_json: row.get(8)?,
        frozen_digest: row.get(9)?,
        state: row.get(10)?,
        outcome: row.get(11)?,
        detail: row.get(12)?,
        notice_activity_pending: row.get::<_, i64>(13)? != 0,
        notice_push_state: row.get(14)?,
        notice_push_attempts: row.get(15)?,
        notice_push_next_at: row.get(16)?,
        notice_pushed_at: row.get(17)?,
        created_at: row.get(18)?,
        updated_at: row.get(19)?,
    })
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
        sequence: row.get(9)?,
        metadata: row.get(10)?,
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
