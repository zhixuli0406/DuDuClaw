//! Maintenance Mode — Entry A (dashboard software toggle + TTL).
//!
//! Authority: `commercial/docs/specs/DESIGN-maintenance-mode-2026-08.md` §2. This
//! module implements exactly the Entry A scope of that design: a
//! remote-reachable, Admin-only, TTL-bounded, fully-audited software switch
//! that unlocks two sub-capabilities beyond the normal (Info-Enhanced) tier —
//! SSH remote access and full raw "顯示詳情" disclosure for `device.*`
//! shell-out results. It deliberately does NOT unlock a bootloader menu,
//! an emergency/rescue shell, serial getty, or any other VT — those are
//! Entry B (physical rescue boot item), permanently out of this entry's
//! reach (§2.6).
//!
//! ## Two independent stores, on purpose (§2.3 / §2.4 / rejected approach #4)
//!
//! - [`MaintenanceStore`] (`<home>/maintenance.db`, its own file — never a
//!   table inside `approvals.db`) is the STATE MACHINE: at most one active
//!   window at a time, keyed by hard TTL, read by every "is maintenance
//!   currently open" check and by the periodic sweep.
//! - [`MaintenanceAuditLog`] (`<home>/maintenance_audit.jsonl`) is the
//!   HISTORY: every lifecycle transition (enable / disable / ttl_expired /
//!   gateway_restart) AND every fine-grained access during an open window
//!   (a "顯示詳情" view). A reader asking "did this machine ever run in
//!   maintenance mode" gets the answer from which file has lines, not from
//!   filtering a shared audit stream (§2.3).
//!
//! ## Fail-closed conventions (§6)
//!
//! - A DB that will not open, a query error, or an unparseable timestamp all
//!   resolve to **no active window** (deny) — mirrors `capability_grants.rs`
//!   and `approval.rs`.
//! - TTL is a hard cap with no extension endpoint at all (not even an
//!   "extend" RPC exists) — `enable` refuses outright when a window is
//!   already active (§2.2 "沒有累加式的隱性延長入口").
//! - A gateway process restart force-closes any in-flight window
//!   (`revoke_reason = "gateway_restart"`), even if its TTL had not expired
//!   — see [`lifecycle::revoke_on_boot`] and §2.4's "reassert closed" design
//!   decision. This is the SAME `resume_on_restart=pause` lineage as
//!   `goal_loop.rs`, but stricter: there is no `auto` option here at all.
//! - A close action (stopping the SSH server through `duduclaw-sysd`) failing
//!   never gets papered over: [`MaintenanceWindow::close_error`] is a distinct
//!   column, and on an appliance an unreachable sysd is a close error, never
//!   "nothing to close". `maintenance.status` reports what sysd actually says
//!   (`close_unconfirmed`, `ssh_observed_active`), not what the ledger claims
//!   (§6 fail-closed principle #2).
//!
//! ## SSH key windows (2026-10)
//!
//! Authority for the `ssh` sub-capability since 2026-10:
//! `commercial/docs/specs/DESIGN-maintenance-ssh-account-2026-10.md`. SSH opens
//! only for the `duduclaw-maint` account with one public key that sysd checks
//! and writes; there is no port-only open any more. Submodules:
//! [`lifecycle`] (enable/disable/sweep/boot reconciliation/poll/status),
//! [`supervisor`] (the boot-started task that runs them),
//! [`sysd_port`] (the three sysd verbs behind a mockable trait),
//! [`lockout`] (password re-entry lockout, event dedup), [`notify`] (Admin
//! notices).

use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::{error, warn};

pub mod lifecycle;
pub mod lockout;
pub mod notify;
pub mod supervisor;
pub mod sysd_port;
#[cfg(test)]
mod tests_ssh;

pub use lifecycle::{
    EnableRequest, MaintEnv, MaintError, ReauthVerifier, disable, enable, error_code, revoke_on_boot,
    run_close_actions, status_json, sweep_expired,
};

// ── Constants ───────────────────────────────────────────────

/// §2.2: default TTL when the caller does not specify one.
pub const DEFAULT_TTL_HOURS: i64 = 4;
/// §2.2: hard upper bound — `enable` refuses anything above this, and there
/// is no extension endpoint to get past it later either.
pub const MAX_TTL_HOURS: i64 = 24;

/// §2.1: the fixed type-to-confirm string, mirroring `device.factory_reset`'s
/// `"RESET"` convention in `web/src/pages/DevicePage.tsx`.
pub const CONFIRM_TEXT: &str = "MAINTENANCE";

/// `revoke_reason` values (§2.4) — a closed, stable set so a forensic reader
/// can always tell "expired on schedule" from "an admin turned it off" from
/// "the gateway process restarted out from under it".
pub const REVOKE_REASON_ADMIN: &str = "admin_disabled";
pub const REVOKE_REASON_TTL: &str = "ttl_expired";
pub const REVOKE_REASON_RESTART: &str = "gateway_restart";
/// The SSH open never completed (sysd refused or failed it); the row is kept
/// as a record of the attempt, closed at the moment of the failure.
pub const REVOKE_REASON_OPEN_FAILED: &str = "open_failed";
/// The OS side closed the SSH window by itself (sysd's own deadline, the PID 1
/// fallback timer, or sysd's startup cleanup) and the supervisor saw the key
/// gone.
pub const REVOKE_REASON_OS: &str = "closed_by_os";

/// §2.6: the closed set of sub-capabilities Entry A may unlock. Deliberately
/// NOT an open string — every value that reaches [`MaintenanceStore::enable`]
/// is checked against this list, so a malformed/forged RPC payload can never
/// smuggle in a capability token this module doesn't know how to open OR
/// close (which would leave an un-closeable half-open state).
pub const SUB_CAPABILITY_SSH: &str = "ssh";
pub const SUB_CAPABILITY_SHOW_DETAILS: &str = "show_details";
const KNOWN_SUB_CAPABILITIES: &[&str] = &[SUB_CAPABILITY_SSH, SUB_CAPABILITY_SHOW_DETAILS];

/// WP20-style reminder fraction, reused verbatim from `approval.rs`'s
/// `REMIND_AT_FRACTION` rationale — a maintenance window is exactly the kind
/// of TTL-bounded grant that design already solved once.
pub const REMIND_AT_FRACTION: f64 = 2.0 / 3.0;

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// Validate a requested sub-capability list: non-empty, every entry known,
/// de-duplicated. Fail-closed — any unknown token rejects the WHOLE request
/// rather than silently dropping it (silently dropping could make a caller
/// believe a capability is open when [`MaintenanceStore::enable`] refused it).
pub fn validate_sub_capabilities(requested: &[String]) -> Result<Vec<String>, String> {
    if requested.is_empty() {
        return Err("sub_capabilities must not be empty".to_string());
    }
    let mut out: Vec<String> = Vec::new();
    for token in requested {
        if !KNOWN_SUB_CAPABILITIES.contains(&token.as_str()) {
            return Err(format!("unknown sub_capability: {token}"));
        }
        if !out.contains(token) {
            out.push(token.clone());
        }
    }
    Ok(out)
}

// ── Window row ──────────────────────────────────────────────

/// One row of `maintenance_windows` — the full lifecycle of a single
/// enable→(expire|disable|restart) cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceWindow {
    pub id: String,
    pub enabled_by: String,
    pub enabled_by_email: String,
    pub enabled_at: String,
    pub ttl_seconds: i64,
    pub expires_at: String,
    pub sub_capabilities: Vec<String>,
    pub revoked_at: Option<String>,
    pub revoke_reason: Option<String>,
    pub reminded_at: Option<String>,
    /// Best-effort record of a close-action failure (`systemctl stop
    /// <ssh unit>` erroring) — never blocks the revoke itself (§6 fail-closed
    /// principle #2 talks about STATUS REPORTING, not about refusing to
    /// record a revoke that a human/TTL sweep already decided on), but keeps
    /// the discrepancy visible to forensics and to `status()`.
    pub close_error: Option<String>,
    /// Lifecycle of the open itself (`opening` → `open` | `open_failed`, then
    /// `closed` once revoked). Only an `open` row counts as active.
    pub state: WindowState,
    /// The one SSH key this window installed (SSH windows only).
    pub ssh_key_type: Option<String>,
    pub ssh_key_fingerprint: Option<String>,
    /// Display-only label from the key comment (`[A-Za-z0-9@._+-]`, ≤ 64).
    pub ssh_key_label: Option<String>,
    /// The device's SSH host key fingerprints as sysd reported them on open.
    pub host_key_fingerprints: Vec<String>,
    /// Why the open failed (`open_failed` rows only).
    pub open_error: Option<String>,
}

/// [`MaintenanceWindow::state`]. Stored as its `as_str()` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowState {
    /// Row written, sysd not yet confirmed the SSH open.
    Opening,
    Open,
    OpenFailed,
    Closed,
}

impl WindowState {
    pub fn as_str(self) -> &'static str {
        match self {
            WindowState::Opening => "opening",
            WindowState::Open => "open",
            WindowState::OpenFailed => "open_failed",
            WindowState::Closed => "closed",
        }
    }

    /// Unknown text fails closed to `Closed` (never counted as active).
    pub fn parse(s: &str) -> Self {
        match s {
            "opening" => WindowState::Opening,
            "open" => WindowState::Open,
            "open_failed" => WindowState::OpenFailed,
            _ => WindowState::Closed,
        }
    }
}

impl MaintenanceWindow {
    fn expires_at_dt(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.expires_at)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }

    /// True when this row is currently open: not revoked and not past its
    /// TTL. An unparseable `expires_at` fails closed (treated as expired).
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        if self.revoked_at.is_some() || self.state != WindowState::Open {
            return false;
        }
        match self.expires_at_dt() {
            Some(exp) => now < exp,
            None => false,
        }
    }

    /// Seconds remaining until TTL expiry, floored at 0. `None` if the row is
    /// already revoked or the timestamp is unparseable.
    pub fn remaining_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        if self.revoked_at.is_some() {
            return None;
        }
        self.expires_at_dt()
            .map(|exp| (exp - now).num_seconds().max(0))
    }

    /// WP20-style: true once [`REMIND_AT_FRACTION`] of the TTL has elapsed
    /// and no reminder has been sent yet.
    pub fn reminder_due(&self, now: DateTime<Utc>) -> bool {
        if self.revoked_at.is_some() || self.reminded_at.is_some() {
            return false;
        }
        let Ok(enabled) = DateTime::parse_from_rfc3339(&self.enabled_at) else {
            return false;
        };
        let enabled = enabled.with_timezone(&Utc);
        let elapsed_ms = (now - enabled).num_milliseconds();
        let ttl_ms = self.ttl_seconds.saturating_mul(1000);
        if ttl_ms <= 0 {
            return false;
        }
        elapsed_ms >= (ttl_ms as f64 * REMIND_AT_FRACTION) as i64 && elapsed_ms < ttl_ms
    }
}

fn row_to_window(row: &rusqlite::Row) -> rusqlite::Result<MaintenanceWindow> {
    let sub_caps_text: String = row.get(6)?;
    let sub_capabilities: Vec<String> = serde_json::from_str(&sub_caps_text).unwrap_or_default();
    Ok(MaintenanceWindow {
        id: row.get(0)?,
        enabled_by: row.get(1)?,
        enabled_by_email: row.get(2)?,
        enabled_at: row.get(3)?,
        ttl_seconds: row.get(4)?,
        expires_at: row.get(5)?,
        sub_capabilities,
        revoked_at: row.get(7)?,
        revoke_reason: row.get(8)?,
        reminded_at: row.get(9)?,
        close_error: row.get(10)?,
        state: WindowState::parse(&row.get::<_, String>(11)?),
        ssh_key_type: row.get(12)?,
        ssh_key_fingerprint: row.get(13)?,
        ssh_key_label: row.get(14)?,
        host_key_fingerprints: row
            .get::<_, Option<String>>(15)?
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default(),
        open_error: row.get(16)?,
    })
}

const SELECT_COLUMNS: &str = "id, enabled_by, enabled_by_email, enabled_at, ttl_seconds, \
     expires_at, sub_capabilities, revoked_at, revoke_reason, reminded_at, close_error, \
     state, ssh_key_type, ssh_key_fingerprint, ssh_key_label, host_key_fingerprints, open_error";

/// Columns added for the maintenance SSH key windows (2026-10). Added with
/// `ALTER TABLE` to a `maintenance.db` an older gateway created.
const WINDOW_COLUMNS_2026_10: &[(&str, &str)] = &[
    ("state", "TEXT NOT NULL DEFAULT 'open'"),
    ("ssh_key_type", "TEXT"),
    ("ssh_key_fingerprint", "TEXT"),
    ("ssh_key_label", "TEXT"),
    ("host_key_fingerprints", "TEXT"),
    ("open_error", "TEXT"),
    ("close_confirmed_at", "TEXT"),
];

// ── Store ───────────────────────────────────────────────────

/// Arguments of [`MaintenanceStore::insert`].
pub struct NewWindow<'a> {
    pub enabled_by: &'a str,
    pub enabled_by_email: &'a str,
    pub ttl_hours: i64,
    pub sub_capabilities: &'a [String],
    pub state: WindowState,
    /// The parsed key of an SSH window (fingerprint, type and label are stored).
    pub ssh_key: Option<&'a duduclaw_sysd::ssh_key::ParsedKey>,
}

/// SQLite-backed state machine. Mirrors the `ApprovalStore` /
/// `CapabilityGrantStore` idioms: `Mutex<Connection>`, WAL, self-healing
/// schema, parameterized SQL only. Deliberately its own file
/// (`maintenance.db`), never a table inside `approvals.db` — see the module
/// doc comment and `capability_grants.rs`'s "must never share a table name"
/// lesson.
pub struct MaintenanceStore {
    conn: Mutex<Connection>,
}

impl MaintenanceStore {
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("maintenance.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open maintenance store: {e}"))?;
        Self::init_schema(&conn)?;
        tracing::debug!(?db_path, "MaintenanceStore opened");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS maintenance_windows (
                 id               TEXT PRIMARY KEY,
                 enabled_by       TEXT NOT NULL,
                 enabled_by_email TEXT NOT NULL,
                 enabled_at       TEXT NOT NULL,
                 ttl_seconds      INTEGER NOT NULL,
                 expires_at       TEXT NOT NULL,
                 sub_capabilities TEXT NOT NULL DEFAULT '[]',
                 revoked_at       TEXT,
                 revoke_reason    TEXT,
                 reminded_at      TEXT,
                 close_error      TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_maint_revoked ON maintenance_windows(revoked_at);

             CREATE TABLE IF NOT EXISTS maintenance_reauth (
                 user_id              TEXT PRIMARY KEY,
                 consecutive_failures INTEGER NOT NULL DEFAULT 0,
                 locked_until         TEXT
             );

             CREATE TABLE IF NOT EXISTS maintenance_ssh_events (
                 window_id TEXT NOT NULL,
                 cursor    TEXT NOT NULL,
                 PRIMARY KEY (window_id, cursor)
             );",
        )
        .map_err(|e| format!("init maintenance_windows schema: {e}"))?;
        Self::migrate_window_columns(conn)
    }

    /// Add the 2026-10 columns to an older table. A row that was already
    /// revoked before the `state` column existed becomes `closed`; an
    /// unrevoked one stays `open` (it was opened by the old flow, which only
    /// wrote rows after a successful open).
    fn migrate_window_columns(conn: &Connection) -> Result<(), String> {
        let existing: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(maintenance_windows)")
                .map_err(|e| format!("read maintenance_windows columns: {e}"))?;
            stmt.query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| format!("read maintenance_windows columns: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("read maintenance_windows columns: {e}"))?
        };
        for (name, decl) in WINDOW_COLUMNS_2026_10 {
            if existing.iter().any(|c| c == name) {
                continue;
            }
            match conn.execute(&format!("ALTER TABLE maintenance_windows ADD COLUMN {name} {decl}"), []) {
                Ok(_) => {}
                // Another connection added it between the read and the ALTER.
                Err(e) if e.to_string().contains("duplicate column name") => continue,
                Err(e) => return Err(format!("add maintenance_windows.{name}: {e}")),
            }
            if *name == "state" {
                conn.execute(
                    "UPDATE maintenance_windows SET state = 'closed' WHERE revoked_at IS NOT NULL",
                    [],
                )
                .map_err(|e| format!("backfill maintenance_windows.state: {e}"))?;
            }
        }
        Ok(())
    }

    /// The single not-yet-revoked row, if any. A schema invariant (`enable`
    /// refuses to insert a second one while one is unrevoked) keeps this to
    /// at most one row, but the query does not assume that — it takes the
    /// most recently enabled one if more than one somehow exists, which is a
    /// safer fail-closed shape than erroring the whole read.
    async fn unrevoked_row(&self) -> Result<Option<MaintenanceWindow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM maintenance_windows \
                 WHERE revoked_at IS NULL ORDER BY enabled_at DESC LIMIT 1"
            ),
            [],
            row_to_window,
        )
        .optional()
        .map_err(|e| format!("query unrevoked window: {e}"))
    }

    /// The currently ACTIVE window (unrevoked AND not past TTL). `None` both
    /// when nothing is open and when the open row has quietly outlived its
    /// TTL but has not been swept yet — callers that need "is it open right
    /// now" (as opposed to "does a record exist") should use this, not
    /// [`Self::unrevoked_row`] directly. Fail-closed: any read error is
    /// treated as "not active", never surfaced as "active" by omission.
    pub async fn active_window(&self) -> Result<Option<MaintenanceWindow>, String> {
        let row = self.unrevoked_row().await?;
        Ok(row.filter(|w| w.is_active(Utc::now())))
    }

    /// Enable a new window that is open right away (no SSH key involved).
    /// Refuses (§2.2, no accumulation) when a window is already unrevoked,
    /// whether or not it has technically expired yet — a caller who wants a
    /// fresh window after expiry must wait for the sweep (or call `disable`
    /// first), never silently stack a second one.
    pub async fn enable(
        &self,
        enabled_by: &str,
        enabled_by_email: &str,
        ttl_hours: i64,
        sub_capabilities: &[String],
    ) -> Result<MaintenanceWindow, String> {
        self.insert(NewWindow {
            enabled_by,
            enabled_by_email,
            ttl_hours,
            sub_capabilities,
            state: WindowState::Open,
            ssh_key: None,
        })
        .await
    }

    /// Insert a window row in `new.state` (`Open`, or `Opening` for an SSH
    /// window whose sysd open has not answered yet). Same refusal as
    /// [`Self::enable`]: any unrevoked row, `opening` included, blocks it.
    pub async fn insert(&self, new: NewWindow<'_>) -> Result<MaintenanceWindow, String> {
        if self.unrevoked_row().await?.is_some() {
            return Err("maintenance mode is already active".to_string());
        }
        if new.ttl_hours <= 0 || new.ttl_hours > MAX_TTL_HOURS {
            return Err(format!("ttl_hours must be in 1..={MAX_TTL_HOURS}"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let ttl_seconds = new.ttl_hours.saturating_mul(3600);
        let expires_at = (now + chrono::Duration::seconds(ttl_seconds)).to_rfc3339();
        let sub_caps_json = serde_json::to_string(new.sub_capabilities)
            .map_err(|e| format!("serialize sub_capabilities: {e}"))?;
        let key_type = new.ssh_key.map(|k| k.key_type.name().to_string());
        let key_fingerprint = new.ssh_key.map(|k| k.fingerprint.clone());
        let key_label = new.ssh_key.map(|k| k.label.clone()).filter(|l| !l.is_empty());

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO maintenance_windows
                (id, enabled_by, enabled_by_email, enabled_at, ttl_seconds,
                 expires_at, sub_capabilities, state, ssh_key_type,
                 ssh_key_fingerprint, ssh_key_label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                new.enabled_by,
                new.enabled_by_email,
                now.to_rfc3339(),
                ttl_seconds,
                expires_at,
                sub_caps_json,
                new.state.as_str(),
                key_type,
                key_fingerprint,
                key_label,
            ],
        )
        .map_err(|e| format!("insert maintenance window: {e}"))?;

        Ok(MaintenanceWindow {
            id,
            enabled_by: new.enabled_by.to_string(),
            enabled_by_email: new.enabled_by_email.to_string(),
            enabled_at: now.to_rfc3339(),
            ttl_seconds,
            expires_at,
            sub_capabilities: new.sub_capabilities.to_vec(),
            revoked_at: None,
            revoke_reason: None,
            reminded_at: None,
            close_error: None,
            state: new.state,
            ssh_key_type: key_type,
            ssh_key_fingerprint: key_fingerprint,
            ssh_key_label: key_label,
            host_key_fingerprints: Vec::new(),
            open_error: None,
        })
    }

    /// `opening` → `open` once sysd confirmed the SSH open. Only touches a row
    /// that is still unrevoked and `opening`; `Ok(false)` otherwise (the
    /// caller then closes what sysd just opened).
    pub async fn mark_open(&self, id: &str, host_key_fingerprints: &[String]) -> Result<bool, String> {
        let fps = serde_json::to_string(host_key_fingerprints)
            .map_err(|e| format!("serialize host key fingerprints: {e}"))?;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE maintenance_windows SET state = 'open', host_key_fingerprints = ?1
                 WHERE id = ?2 AND revoked_at IS NULL AND state = 'opening'",
                params![fps, id],
            )
            .map_err(|e| format!("mark maintenance window open: {e}"))?;
        Ok(n == 1)
    }

    /// The SSH open failed: the row becomes `open_failed` and is closed at
    /// once (`revoke_reason = open_failed`), with the rollback's close error
    /// when there was one.
    pub async fn mark_open_failed(
        &self,
        id: &str,
        open_error: &str,
        close_error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE maintenance_windows
                SET state = 'open_failed', open_error = ?1, close_error = ?2,
                    revoked_at = ?3, revoke_reason = ?4
              WHERE id = ?5",
            params![open_error, close_error, now_rfc3339(), REVOKE_REASON_OPEN_FAILED, id],
        )
        .map_err(|e| format!("mark maintenance window open_failed: {e}"))?;
        Ok(())
    }

    /// The most recently enabled row, revoked or not.
    pub async fn latest_window(&self) -> Result<Option<MaintenanceWindow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM maintenance_windows ORDER BY enabled_at DESC LIMIT 1"),
            [],
            row_to_window,
        )
        .optional()
        .map_err(|e| format!("query latest window: {e}"))
    }

    /// The unrevoked row (open, opening, or past TTL but unswept), if any.
    pub async fn current_row(&self) -> Result<Option<MaintenanceWindow>, String> {
        self.unrevoked_row().await
    }

    /// Revoke whatever is currently unrevoked (active or already
    /// TTL-expired-but-unswept), stamping `revoke_reason`. Returns the
    /// PRE-revoke window (so the caller can run close actions for its
    /// `sub_capabilities`) or `None` if nothing was open.
    async fn revoke_unrevoked(
        &self,
        revoke_reason: &str,
    ) -> Result<Option<MaintenanceWindow>, String> {
        let Some(window) = self.unrevoked_row().await? else {
            return Ok(None);
        };
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE maintenance_windows SET revoked_at = ?1, revoke_reason = ?2, state = 'closed' WHERE id = ?3",
            params![now_rfc3339(), revoke_reason, window.id],
        )
        .map_err(|e| format!("revoke maintenance window: {e}"))?;
        Ok(Some(window))
    }

    /// §2.1/§2.8 `maintenance.disable`: an admin explicitly turning it off.
    pub async fn disable(&self) -> Result<Option<MaintenanceWindow>, String> {
        self.revoke_unrevoked(REVOKE_REASON_ADMIN).await
    }

    /// §2.4 TTL sweep: revoke iff the unrevoked row has actually passed its
    /// TTL. Absolute-time comparison (never "time since last sweep"), so a
    /// delayed tick can never be mistaken for "not expired yet". Returns
    /// `Some` only when something was actually swept, so the tick-loop caller
    /// doesn't run close actions/audit for a no-op tick.
    pub async fn expire_stale(&self) -> Result<Option<MaintenanceWindow>, String> {
        let Some(window) = self.unrevoked_row().await? else {
            return Ok(None);
        };
        if window.is_active(Utc::now()) {
            return Ok(None); // not expired yet
        }
        self.revoke_unrevoked(REVOKE_REASON_TTL).await
    }

    /// §2.4 gateway-restart reassert-closed: revoke the unrevoked row
    /// UNCONDITIONALLY (even if its TTL has plenty of time left) — see the
    /// module doc comment. Idempotent: `None` when nothing was open, so the
    /// boot path can call this on every start without it ever being a
    /// meaningful event when maintenance mode was never on.
    pub async fn reassert_closed_on_boot(&self) -> Result<Option<MaintenanceWindow>, String> {
        self.revoke_unrevoked(REVOKE_REASON_RESTART).await
    }

    /// The OS closed the SSH window itself: revoke the unrevoked row with
    /// [`REVOKE_REASON_OS`], but only when it is still the row `id`.
    pub async fn revoke_closed_by_os(&self, id: &str) -> Result<Option<MaintenanceWindow>, String> {
        match self.unrevoked_row().await? {
            Some(w) if w.id == id => self.revoke_unrevoked(REVOKE_REASON_OS).await,
            _ => Ok(None),
        }
    }

    /// WP20-style once-only reminder guard.
    pub async fn mark_reminded(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE maintenance_windows SET reminded_at = ?1 WHERE id = ?2 AND reminded_at IS NULL",
            params![now_rfc3339(), id],
        )
        .map_err(|e| format!("mark_reminded: {e}"))?;
        Ok(())
    }

    /// Best-effort forensic note: a close action failed for this window.
    /// Never propagates a failure of ITS OWN — recording the note is
    /// diagnostic, not part of the fail-closed contract (the revoke already
    /// happened regardless).
    pub async fn set_close_error(&self, id: &str, message: &str) {
        let conn = self.conn.lock().await;
        if let Err(e) = conn.execute(
            "UPDATE maintenance_windows SET close_error = ?1, close_confirmed_at = NULL WHERE id = ?2",
            params![message, id],
        ) {
            warn!(error = %e, window_id = id, "failed to record maintenance close_error note");
        }
    }

    /// An unresolved SSH close stays visible across later show-details
    /// windows. Keep the original error for history when sysd confirms it.
    pub async fn has_unconfirmed_close(&self) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM maintenance_windows
             WHERE close_error IS NOT NULL AND close_confirmed_at IS NULL)",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("read unconfirmed maintenance closes: {e}"))
    }

    /// Called under the writer lock only after a fresh positive sysd status
    /// confirms no key, window, socket, session or cleanup remains.
    pub async fn confirm_ssh_closed(&self) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE maintenance_windows SET close_confirmed_at = ?1
             WHERE close_error IS NOT NULL AND close_confirmed_at IS NULL",
            params![now_rfc3339()],
        )
        .map_err(|e| format!("confirm maintenance SSH closes: {e}"))?;
        Ok(())
    }

    /// `maintenance.history`-adjacent: recent windows (lifecycle rows), newest
    /// first. NOTE: fine-grained access events (每次「顯示詳情」還原) live in
    /// [`MaintenanceAuditLog`], not here — see the module doc comment for why
    /// the two are deliberately separate reads.
    pub async fn recent_windows(&self, limit: i64) -> Result<Vec<MaintenanceWindow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SELECT_COLUMNS} FROM maintenance_windows ORDER BY enabled_at DESC LIMIT ?1"
            ))
            .map_err(|e| format!("prepare recent_windows: {e}"))?;
        let rows = stmt
            .query_map(params![limit], row_to_window)
            .map_err(|e| format!("query recent_windows: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect recent_windows: {e}"))?;
        Ok(rows)
    }
}

// ── Audit log (JSONL, independent file — §2.3) ─────────────

/// Append-only JSONL audit trail, kept deliberately separate from
/// `security_audit.jsonl`/`tool_calls.jsonl` (see module doc comment).
///
/// Single-writer-process concurrency reasoning, same as
/// `codrive::audit::AuditLog` / `shell_control::audit::ShellControlAuditLog`:
/// exactly one process (this gateway) appends to this file for its whole
/// lifetime, so a plain `std::sync::Mutex<File>` is sufficient — reaching for
/// `duduclaw_core::with_file_lock` would add a cross-process guarantee this
/// file does not need. If a second writer process is ever introduced, switch
/// to that helper then (flagged here so it isn't forgotten, same as those two
/// modules' own doc comments).
pub struct MaintenanceAuditLog {
    file: StdMutex<std::fs::File>,
    path: PathBuf,
}

impl MaintenanceAuditLog {
    pub fn open(home_dir: &Path) -> std::io::Result<Self> {
        let path = home_dir.join("maintenance_audit.jsonl");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            {
                warn!(error = %e, path = %path.display(), "maintenance: could not chmod 0600 the audit log");
            }
        }
        Ok(Self {
            file: StdMutex::new(file),
            path,
        })
    }

    /// Append one JSON line: `{"ts_ms":.., "kind":.., ...fields}`. Fail-open
    /// on the audit trail itself (same split every audit log in this repo
    /// uses) — a broken audit sink must never become a reason to block the
    /// actual enable/disable/sweep/view action it is trying to record.
    pub fn record(&self, kind: &str, fields: Value) {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut event = match fields {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        event.insert("ts_ms".to_string(), json!(ts_ms));
        event.insert("kind".to_string(), json!(kind));

        let line = match serde_json::to_string(&Value::Object(event)) {
            Ok(l) => l,
            Err(e) => {
                error!(error = %e, "maintenance: failed to serialize audit event — dropping this line");
                return;
            }
        };
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        use std::io::Write;
        if let Err(e) = writeln!(guard, "{line}") {
            error!(error = %e, "maintenance: failed to append audit log line");
            return;
        }
        let _ = guard.flush();
    }

    /// Paginated read for `maintenance.history`, newest-first. Reads the
    /// whole file and slices in memory — acceptable at this scale (a single
    /// duty-station appliance's maintenance-mode history, not a
    /// high-frequency log); unparseable lines are skipped rather than
    /// aborting the whole read (one corrupted line must not hide every
    /// earlier one).
    pub fn read_recent(&self, limit: usize, offset: usize) -> std::io::Result<Vec<Value>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut rows: Vec<Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect();
        rows.reverse(); // newest first
        Ok(rows.into_iter().skip(offset).take(limit).collect())
    }
}

// ── Live observed-state probe (§6 fail-closed principle #2) ─

/// Whether the image's SSH server is up: one of `duduclaw_sysd::SSH_UNITS`
/// is active, or a session process is still running under its cgroup. Read-only and needs no root (unlike `start`/`stop`),
/// so this runs directly in the gateway process rather than crossing the
/// `duduclaw-sysd` socket. `None` on any non-appliance host, spawn failure,
/// or unrecognized output; callers must treat `None` as "no live signal
/// available", never coerce it to `false`.
///
/// This exists so `maintenance.status()` can answer with what the system
/// ACTUALLY looks like, not just what the ledger's `revoked_at` column
/// optimistically claims — the exact fail-closed gap §6 principle #2 calls
/// out ("狀態回報必須反映實際觀測到的系統狀態，而不是「應該要是什麼狀態」").
/// Until 2026-10 it only asked about `ssh.service`, which Yocto images do not
/// have, so it always answered "not running" there.
pub async fn ssh_service_observed_active() -> Option<bool> {
    if !duduclaw_core::is_appliance() {
        return None;
    }
    for unit in duduclaw_sysd::SSH_UNITS {
        let (ok, load) =
            systemctl_output(&["show", "--property=LoadState", "--value", unit.unit]).await?;
        match (ok, load.trim()) {
            (true, "not-found") => continue,
            (true, "loaded" | "masked") => {}
            _ => return None, // systemd gave no clear answer
        }
        let (_, state) = systemctl_output(&["is-active", unit.unit]).await?;
        match state.trim() {
            "active" | "activating" | "reloading" | "deactivating" => return Some(true),
            "inactive" | "failed" => {}
            _ => return None, // empty or unrecognized — no confident answer
        }
        if let Some(cgroup) = unit.session_cgroup {
            let cgroup = std::path::Path::new(cgroup);
            // A missing directory means "no sessions" only on a cgroup v2 host.
            if !cgroup.is_dir() && !std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
                return None;
            }
            match cgroup_has_processes(cgroup) {
                Ok(true) => return Some(true),
                Ok(false) => {}
                Err(_) => return None,
            }
        }
    }
    // Every candidate was checked and nothing is up (or none is installed).
    Some(false)
}

/// Run `systemctl <args>`; `(exit success, stdout)`, or `None` if it cannot
/// spawn. `is-active` exits non-zero for inactive units, so callers judge
/// the output themselves.
async fn systemctl_output(args: &[&str]) -> Option<(bool, String)> {
    let out = tokio::process::Command::new("systemctl")
        .args(args)
        .output()
        .await
        .ok()?;
    Some((out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Whether any process lives anywhere under a cgroup v2 directory (session
/// processes can outlive their unit, see `duduclaw_sysd::SshUnit`). A
/// missing directory holds none. `cgroup.procs` is world-readable.
fn cgroup_has_processes(dir: &std::path::Path) -> std::io::Result<bool> {
    match std::fs::read_to_string(dir.join("cgroup.procs")) {
        Ok(procs) if procs.lines().any(|l| !l.trim().is_empty()) => return Ok(true),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && cgroup_has_processes(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ── Orchestration lives in `lifecycle` (enable/disable/sweep/boot/status) ──

/// §2.3 fine-grained access log: one call per "顯示詳情" full-text reveal (or
/// any future `show_details`-gated action). Refuses (rather than silently
/// recording a fabricated line) unless maintenance mode is genuinely active
/// AND `show_details` is one of its unlocked sub-capabilities — a stray or
/// forged call cannot manufacture an audit line implying a view happened
/// under authority that was never granted.
pub async fn log_access(
    home_dir: &Path,
    viewed_by: &str,
    viewed_by_email: &str,
    kind: &str,
    mut fields: serde_json::Map<String, Value>,
) -> Result<(), String> {
    let store = MaintenanceStore::open(home_dir)?;
    let Some(window) = store.active_window().await? else {
        return Err("maintenance mode is not active".to_string());
    };
    if !window
        .sub_capabilities
        .iter()
        .any(|c| c == SUB_CAPABILITY_SHOW_DETAILS)
    {
        return Err("show_details sub-capability is not unlocked in the active window".to_string());
    }
    let audit = MaintenanceAuditLog::open(home_dir)
        .map_err(|e| format!("open maintenance audit log: {e}"))?;
    fields.insert("window_id".to_string(), json!(window.id));
    fields.insert("viewed_by".to_string(), json!(viewed_by));
    fields.insert("viewed_by_email".to_string(), json!(viewed_by_email));
    audit.record(kind, Value::Object(fields));
    Ok(())
}

/// Assembles the `maintenance.history()` RPC payload from the JSONL audit
/// trail (NOT the SQLite lifecycle table — see the module doc comment for
/// why `history` reads the richer file: it also carries every fine-grained
/// access event, not just enable/disable/expire transitions).
pub fn history_json(home_dir: &Path, limit: usize, offset: usize) -> Result<Value, String> {
    let audit = MaintenanceAuditLog::open(home_dir)
        .map_err(|e| format!("open maintenance audit log: {e}"))?;
    let events = audit
        .read_recent(limit, offset)
        .map_err(|e| format!("read maintenance history: {e}"))?;
    Ok(json!({ "events": events }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn window(ttl_hours: i64) -> (MaintenanceStore, MaintenanceWindow) {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store
            .enable(
                "u1",
                "admin@local",
                ttl_hours,
                &[SUB_CAPABILITY_SSH.to_string()],
            )
            .await
            .unwrap();
        (store, w)
    }

    #[test]
    fn validate_sub_capabilities_rejects_unknown_and_empty() {
        assert!(validate_sub_capabilities(&[]).is_err());
        assert!(validate_sub_capabilities(&["shell_access".to_string()]).is_err());
        let ok = validate_sub_capabilities(&[
            SUB_CAPABILITY_SSH.to_string(),
            SUB_CAPABILITY_SSH.to_string(), // duplicate
            SUB_CAPABILITY_SHOW_DETAILS.to_string(),
        ])
        .unwrap();
        assert_eq!(
            ok,
            vec![
                SUB_CAPABILITY_SSH.to_string(),
                SUB_CAPABILITY_SHOW_DETAILS.to_string()
            ]
        );
    }

    #[tokio::test]
    async fn enable_refuses_ttl_out_of_range() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        assert!(
            store
                .enable("u1", "a@b", 0, &[SUB_CAPABILITY_SSH.to_string()])
                .await
                .is_err()
        );
        assert!(
            store
                .enable(
                    "u1",
                    "a@b",
                    MAX_TTL_HOURS + 1,
                    &[SUB_CAPABILITY_SSH.to_string()]
                )
                .await
                .is_err()
        );
        assert!(
            store
                .enable(
                    "u1",
                    "a@b",
                    MAX_TTL_HOURS,
                    &[SUB_CAPABILITY_SSH.to_string()]
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn enable_refuses_when_already_active_no_accumulation() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        store
            .enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .unwrap();
        let second = store
            .enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()])
            .await;
        assert!(
            second.is_err(),
            "a second enable while one is active must be refused"
        );
    }

    #[tokio::test]
    async fn active_window_reflects_ttl_expiry_without_a_sweep() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        // TTL 24h so the row is comfortably active for read-consistency checks.
        store
            .enable("u1", "a@b", 24, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .unwrap();
        assert!(store.active_window().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn expire_stale_is_a_noop_before_ttl_and_revokes_after() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store
            .enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .unwrap();
        assert!(
            store.expire_stale().await.unwrap().is_none(),
            "not expired yet"
        );

        // Force expiry by rewriting expires_at into the past directly (unit
        // test, not an integration timing test).
        {
            let conn = store.conn.lock().await;
            conn.execute(
                "UPDATE maintenance_windows SET expires_at = ?1 WHERE id = ?2",
                params![
                    (Utc::now() - chrono::Duration::seconds(10)).to_rfc3339(),
                    w.id
                ],
            )
            .unwrap();
        }
        let swept = store.expire_stale().await.unwrap();
        assert!(swept.is_some(), "must sweep once past TTL");
        assert_eq!(swept.unwrap().id, w.id);
        assert!(store.active_window().await.unwrap().is_none());

        // Idempotent: nothing left to sweep on a second pass.
        assert!(store.expire_stale().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn disable_stamps_admin_disabled_and_returns_pre_revoke_window() {
        let (store, w) = window(4).await;
        let revoked = store.disable().await.unwrap().unwrap();
        assert_eq!(revoked.id, w.id);
        assert_eq!(
            revoked.revoked_at, None,
            "returned row is the PRE-revoke snapshot"
        );
        assert!(store.active_window().await.unwrap().is_none());

        // A second disable with nothing active is a harmless no-op, not an error.
        assert!(store.disable().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reassert_closed_on_boot_force_closes_even_with_ttl_remaining() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        store
            .enable("u1", "a@b", 24, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .unwrap();
        assert!(store.active_window().await.unwrap().is_some());

        let reasserted = store.reassert_closed_on_boot().await.unwrap();
        assert!(reasserted.is_some(), "must force-close even with 24h left");
        assert!(store.active_window().await.unwrap().is_none());

        let windows = store.recent_windows(10).await.unwrap();
        assert_eq!(
            windows[0].revoke_reason.as_deref(),
            Some(REVOKE_REASON_RESTART)
        );

        // Idempotent on a clean boot (nothing was open).
        assert!(store.reassert_closed_on_boot().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn is_active_fails_closed_on_unparseable_expiry() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store
            .enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .unwrap();
        {
            let conn = store.conn.lock().await;
            conn.execute(
                "UPDATE maintenance_windows SET expires_at = 'not-a-timestamp' WHERE id = ?1",
                params![w.id],
            )
            .unwrap();
        }
        assert!(
            store.active_window().await.unwrap().is_none(),
            "an unparseable expiry must fail closed to inactive, never active"
        );
    }

    #[tokio::test]
    async fn remaining_seconds_is_none_once_revoked() {
        let (store, _w) = window(4).await;
        store.disable().await.unwrap();
        let windows = store.recent_windows(1).await.unwrap();
        assert_eq!(windows[0].remaining_seconds(Utc::now()), None);
    }

    #[tokio::test]
    async fn reminder_due_fires_only_in_the_configured_window() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store
            .enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .unwrap();
        assert!(
            !w.reminder_due(Utc::now()),
            "must not be due immediately after enabling"
        );

        let almost_expired =
            Utc::now() + chrono::Duration::seconds((w.ttl_seconds as f64 * 0.9) as i64);
        assert!(w.reminder_due(almost_expired));

        let already_expired = Utc::now() + chrono::Duration::seconds(w.ttl_seconds + 10);
        assert!(
            !w.reminder_due(already_expired),
            "past expiry is a denial, not a reminder"
        );
    }

    #[tokio::test]
    async fn mark_reminded_is_idempotent_and_stops_further_reminders() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store
            .enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .unwrap();
        store.mark_reminded(&w.id).await.unwrap();
        let windows = store.recent_windows(1).await.unwrap();
        assert!(windows[0].reminded_at.is_some());
        let almost_expired =
            Utc::now() + chrono::Duration::seconds((w.ttl_seconds as f64 * 0.9) as i64);
        assert!(
            !windows[0].reminder_due(almost_expired),
            "already reminded ⇒ never due again"
        );
    }

    #[test]
    fn audit_log_records_one_json_line_with_kind_and_fields() {
        let dir = tempfile::tempdir().unwrap();
        let log = MaintenanceAuditLog::open(dir.path()).unwrap();
        log.record("enabled", json!({"enabled_by": "u1", "ttl_hours": 4}));
        log.record("disabled", json!({"revoke_reason": REVOKE_REASON_ADMIN}));

        let rows = log.read_recent(10, 0).unwrap();
        assert_eq!(rows.len(), 2);
        // newest first
        assert_eq!(rows[0]["kind"], "disabled");
        assert_eq!(rows[1]["kind"], "enabled");
        assert_eq!(rows[1]["enabled_by"], "u1");
    }

    #[test]
    fn audit_log_pagination_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let log = MaintenanceAuditLog::open(dir.path()).unwrap();
        for i in 0..5 {
            log.record("access_view_detail", json!({"seq": i}));
        }
        let page = log.read_recent(2, 1).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0]["seq"], 3); // newest is seq=4 (offset 0), so offset 1 starts at seq=3
        assert_eq!(page[1]["seq"], 2);
    }

    #[test]
    fn audit_log_skips_unparseable_lines_without_losing_earlier_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maintenance_audit.jsonl");
        std::fs::write(
            &path,
            "{\"kind\":\"enabled\",\"ts_ms\":1}\nnot json\n{\"kind\":\"disabled\",\"ts_ms\":2}\n",
        )
        .unwrap();
        let log = MaintenanceAuditLog::open(dir.path()).unwrap();
        let rows = log.read_recent(10, 0).unwrap();
        assert_eq!(
            rows.len(),
            2,
            "the malformed line must be skipped, not abort the whole read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn audit_log_file_is_created_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _log = MaintenanceAuditLog::open(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join("maintenance_audit.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn cgroup_has_processes_sees_leftover_session_processes() {
        let root = tempfile::tempdir().unwrap();
        let slice = root.path().join("system-dropbear.slice");
        assert!(!cgroup_has_processes(&slice).unwrap());
        let session = slice.join("dropbear@0.service");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(slice.join("cgroup.procs"), "").unwrap();
        std::fs::write(session.join("cgroup.procs"), "").unwrap();
        assert!(!cgroup_has_processes(&slice).unwrap());
        std::fs::write(session.join("cgroup.procs"), "777\n").unwrap();
        assert!(cgroup_has_processes(&slice).unwrap());
    }

    #[tokio::test]
    async fn ssh_service_observed_active_is_none_off_appliance() {
        assert!(!duduclaw_core::is_appliance());
        assert_eq!(ssh_service_observed_active().await, None);
    }

    // Orchestration tests (enable/disable/sweep/boot/status/log_access with a
    // mocked sysd) live in `maintenance/tests_ssh.rs`.
}
