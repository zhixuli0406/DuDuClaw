//! Maintenance Mode — Entry A (dashboard software toggle + TTL).
//!
//! Authority: `commercial/docs/DESIGN-maintenance-mode-2026-08.md` §2. This
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
//!   — see [`reassert_closed_on_boot`] and §2.4's "reassert closed" design
//!   decision. This is the SAME `resume_on_restart=pause` lineage as
//!   `goal_loop.rs`, but stricter: there is no `auto` option here at all.
//! - A close action (`systemctl stop ssh.service`) failing never gets
//!   papered over: [`MaintenanceWindow::close_error`] is a distinct column,
//!   surfaced by `status()` alongside a LIVE observed probe
//!   ([`ssh_service_observed_active`]) so the dashboard reports what the
//!   system actually looks like, not just what the ledger optimistically
//!   claims (§6 fail-closed principle #2).

use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

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
    /// ssh.service` erroring) — never blocks the revoke itself (§6 fail-closed
    /// principle #2 talks about STATUS REPORTING, not about refusing to
    /// record a revoke that a human/TTL sweep already decided on), but keeps
    /// the discrepancy visible to forensics and to `status()`.
    pub close_error: Option<String>,
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
        if self.revoked_at.is_some() {
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
    })
}

const SELECT_COLUMNS: &str = "id, enabled_by, enabled_by_email, enabled_at, ttl_seconds, \
     expires_at, sub_capabilities, revoked_at, revoke_reason, reminded_at, close_error";

// ── Store ───────────────────────────────────────────────────

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
        let conn = Connection::open(&db_path).map_err(|e| format!("open maintenance store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "MaintenanceStore initialized");
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
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

             CREATE INDEX IF NOT EXISTS idx_maint_revoked ON maintenance_windows(revoked_at);",
        )
        .map_err(|e| format!("init maintenance_windows schema: {e}"))?;
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

    /// Enable a new window. Refuses (§2.2, no accumulation) when a window is
    /// already unrevoked, whether or not it has technically expired yet — a
    /// caller who wants a fresh window after expiry must wait for the sweep
    /// (or call `disable` first), never silently stack a second one.
    #[allow(clippy::too_many_arguments)]
    pub async fn enable(
        &self,
        enabled_by: &str,
        enabled_by_email: &str,
        ttl_hours: i64,
        sub_capabilities: &[String],
    ) -> Result<MaintenanceWindow, String> {
        if self.unrevoked_row().await?.is_some() {
            return Err("maintenance mode is already active".to_string());
        }
        if ttl_hours <= 0 || ttl_hours > MAX_TTL_HOURS {
            return Err(format!("ttl_hours must be in 1..={MAX_TTL_HOURS}"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let ttl_seconds = ttl_hours.saturating_mul(3600);
        let expires_at = (now + chrono::Duration::seconds(ttl_seconds)).to_rfc3339();
        let sub_caps_json = serde_json::to_string(sub_capabilities)
            .map_err(|e| format!("serialize sub_capabilities: {e}"))?;

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO maintenance_windows
                (id, enabled_by, enabled_by_email, enabled_at, ttl_seconds,
                 expires_at, sub_capabilities)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                enabled_by,
                enabled_by_email,
                now.to_rfc3339(),
                ttl_seconds,
                expires_at,
                sub_caps_json,
            ],
        )
        .map_err(|e| format!("insert maintenance window: {e}"))?;

        Ok(MaintenanceWindow {
            id,
            enabled_by: enabled_by.to_string(),
            enabled_by_email: enabled_by_email.to_string(),
            enabled_at: now.to_rfc3339(),
            ttl_seconds,
            expires_at,
            sub_capabilities: sub_capabilities.to_vec(),
            revoked_at: None,
            revoke_reason: None,
            reminded_at: None,
            close_error: None,
        })
    }

    /// Revoke whatever is currently unrevoked (active or already
    /// TTL-expired-but-unswept), stamping `revoke_reason`. Returns the
    /// PRE-revoke window (so the caller can run close actions for its
    /// `sub_capabilities`) or `None` if nothing was open.
    async fn revoke_unrevoked(&self, revoke_reason: &str) -> Result<Option<MaintenanceWindow>, String> {
        let Some(window) = self.unrevoked_row().await? else {
            return Ok(None);
        };
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE maintenance_windows SET revoked_at = ?1, revoke_reason = ?2 WHERE id = ?3",
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
            "UPDATE maintenance_windows SET close_error = ?1 WHERE id = ?2",
            params![message, id],
        ) {
            warn!(error = %e, window_id = id, "failed to record maintenance close_error note");
        }
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
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
                warn!(error = %e, path = %path.display(), "maintenance: could not chmod 0600 the audit log");
            }
        }
        Ok(Self { file: StdMutex::new(file), path })
    }

    /// Append one JSON line: `{"ts_ms":.., "kind":.., ...fields}`. Fail-open
    /// on the audit trail itself (same split every audit log in this repo
    /// uses) — a broken audit sink must never become a reason to block the
    /// actual enable/disable/sweep/view action it is trying to record.
    pub fn record(&self, kind: &str, fields: Value) {
        let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
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

/// `systemctl is-active ssh.service` — read-only, needs no root (unlike
/// `start`/`stop`), so this runs directly in the gateway process rather than
/// crossing the `duduclaw-sysd` socket. `None` on any non-appliance host,
/// spawn failure, or unrecognized output — callers must treat `None` as "no
/// live signal available", never coerce it to `false`.
///
/// This exists so `maintenance.status()` can answer with what the system
/// ACTUALLY looks like, not just what the ledger's `revoked_at` column
/// optimistically claims — the exact fail-closed gap §6 principle #2 calls
/// out ("狀態回報必須反映實際觀測到的系統狀態，而不是「應該要是什麼狀態」").
pub async fn ssh_service_observed_active() -> Option<bool> {
    if !duduclaw_core::is_appliance() {
        return None;
    }
    let out = tokio::process::Command::new("systemctl")
        .args(["is-active", "ssh.service"])
        .output()
        .await
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let state = stdout.trim();
    match state {
        "active" => Some(true),
        "inactive" | "failed" | "unknown" | "" => Some(false),
        _ => None, // an unrecognized token — no confident answer either way
    }
}

// ── Orchestration (store + audit + device actions, one call each) ──

/// Run the close action(s) for a window's `sub_capabilities`. Best-effort per
/// capability — one failing does not skip the others. `None` when every
/// action succeeded (or there was nothing to close); `Some(message)`
/// otherwise, for [`MaintenanceStore::set_close_error`] + the audit line.
async fn run_close_actions(sub_capabilities: &[String]) -> Option<String> {
    let mut errors: Vec<String> = Vec::new();
    if sub_capabilities.iter().any(|c| c == SUB_CAPABILITY_SSH) {
        match crate::device_ops::select_sysd_ops() {
            Some(ops) => match ops.ssh_stop().await {
                Ok(out) if !out.success => {
                    errors.push(format!("ssh_stop exited non-zero: {}", out.stderr.trim()))
                }
                Ok(_) => {}
                Err(e) => errors.push(format!("ssh_stop: {e}")),
            },
            // Off-appliance / no sysd socket reachable: nothing could have
            // been opened in the first place (see `run_open_actions` — "ssh"
            // can never succeed to open without a reachable sysd), so there
            // is nothing to close. Not an error.
            None => {}
        }
    }
    // SUB_CAPABILITY_SHOW_DETAILS needs no system action — it is a pure
    // read-side flag consulted by `maintenance.status()`/the dashboard, so
    // "closing" it is exactly "the flag now reads false", which the revoke
    // itself already accomplished.
    if errors.is_empty() { None } else { Some(errors.join("; ")) }
}

/// Run the open action(s) for a window's `sub_capabilities`. Fail-closed:
/// returns `Err` on the first failure. Deliberately called BEFORE
/// [`MaintenanceStore::enable`] persists anything (see [`enable`] below) so a
/// failure here never leaves a ledger row describing a window that was never
/// actually open.
async fn run_open_actions(sub_capabilities: &[String]) -> Result<(), String> {
    if sub_capabilities.iter().any(|c| c == SUB_CAPABILITY_SSH) {
        let ops = crate::device_ops::select_sysd_ops().ok_or_else(|| {
            "SSH sub-capability requires the appliance's privileged daemon, which is not reachable on this host".to_string()
        })?;
        let out = ops.ssh_start().await.map_err(|e| format!("ssh_start: {e}"))?;
        if !out.success {
            return Err(format!("ssh_start exited non-zero: {}", out.stderr.trim()));
        }
    }
    Ok(())
}

/// §2.8 `maintenance.enable`. Ordering is deliberate: sub-capabilities are
/// validated, then OPENED, and only once that succeeds is the window
/// persisted — never the other way around, so there is no window in the
/// audit trail that claims to be open while its actual unlock failed. A
/// narrow TOCTOU (something else raced the ledger insert after the open
/// action already ran) is closed by rolling the just-opened action back
/// before returning the error.
#[allow(clippy::too_many_arguments)]
pub async fn enable(
    home_dir: &Path,
    enabled_by: &str,
    enabled_by_email: &str,
    ttl_hours: i64,
    requested_sub_capabilities: &[String],
) -> Result<MaintenanceWindow, String> {
    let sub_caps = validate_sub_capabilities(requested_sub_capabilities)?;
    if !(1..=MAX_TTL_HOURS).contains(&ttl_hours) {
        return Err(format!("ttl_hours must be in 1..={MAX_TTL_HOURS}"));
    }
    let store = MaintenanceStore::open(home_dir)?;
    let audit = MaintenanceAuditLog::open(home_dir).map_err(|e| format!("open maintenance audit log: {e}"))?;

    if store.active_window().await?.is_some() {
        return Err("maintenance mode is already active".to_string());
    }

    if let Err(open_err) = run_open_actions(&sub_caps).await {
        audit.record(
            "enable_failed",
            json!({
                "enabled_by": enabled_by,
                "enabled_by_email": enabled_by_email,
                "sub_capabilities": sub_caps,
                "error": open_err,
            }),
        );
        return Err(format!("could not open the requested sub-capabilities: {open_err}"));
    }

    match store.enable(enabled_by, enabled_by_email, ttl_hours, &sub_caps).await {
        Ok(window) => {
            audit.record(
                "enabled",
                json!({
                    "window_id": window.id,
                    "enabled_by": enabled_by,
                    "enabled_by_email": enabled_by_email,
                    "ttl_hours": ttl_hours,
                    "sub_capabilities": sub_caps,
                }),
            );
            info!(window_id = %window.id, %enabled_by_email, ttl_hours, "maintenance mode ENABLED");
            Ok(window)
        }
        Err(e) => {
            if let Some(close_err) = run_close_actions(&sub_caps).await {
                warn!(error = %close_err, "maintenance: rollback close after enable race failed");
            }
            audit.record(
                "enable_race_rolled_back",
                json!({ "enabled_by": enabled_by, "sub_capabilities": sub_caps, "error": e }),
            );
            Err(e)
        }
    }
}

/// §2.8 `maintenance.disable` — an admin explicitly turning it off.
/// `reason` is the RPC's optional free-text operator note; it is audited
/// verbatim but never changes `revoke_reason`, which stays the fixed
/// [`REVOKE_REASON_ADMIN`] token (§2.3/§2.4's closed three-value set).
pub async fn disable(
    home_dir: &Path,
    disabled_by: &str,
    reason: Option<&str>,
) -> Result<Option<MaintenanceWindow>, String> {
    let store = MaintenanceStore::open(home_dir)?;
    let audit = MaintenanceAuditLog::open(home_dir).map_err(|e| format!("open maintenance audit log: {e}"))?;
    let Some(window) = store.disable().await? else {
        return Ok(None);
    };
    let close_err = run_close_actions(&window.sub_capabilities).await;
    if let Some(ref err) = close_err {
        store.set_close_error(&window.id, err).await;
        error!(window_id = %window.id, error = %err, "maintenance: close action failed on manual disable");
    }
    audit.record(
        "disabled",
        json!({
            "window_id": window.id,
            "disabled_by": disabled_by,
            "revoke_reason": REVOKE_REASON_ADMIN,
            "sub_capabilities": window.sub_capabilities,
            "close_error": close_err,
            "note": reason,
        }),
    );
    info!(window_id = %window.id, %disabled_by, "maintenance mode disabled by admin");
    Ok(Some(window))
}

/// Called from `dispatch_engine.rs`'s 30s tick (§2.4). No-op when nothing is
/// past its TTL.
pub async fn sweep_expired_maintenance_window(home_dir: &Path) {
    let store = match MaintenanceStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "maintenance: store open failed for TTL sweep");
            return;
        }
    };
    // WP20-style reminder check runs every tick regardless of whether
    // anything happens to be expiring this exact tick.
    if let Ok(Some(active)) = store.active_window().await {
        if active.reminder_due(Utc::now()) {
            let _ = store.mark_reminded(&active.id).await;
            if let Ok(audit) = MaintenanceAuditLog::open(home_dir) {
                audit.record(
                    "reminder_sent",
                    json!({
                        "window_id": active.id,
                        "remaining_seconds": active.remaining_seconds(Utc::now()),
                    }),
                );
            }
        }
    }

    let swept = match store.expire_stale().await {
        Ok(Some(w)) => w,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, "maintenance: expire_stale failed");
            return;
        }
    };
    let close_err = run_close_actions(&swept.sub_capabilities).await;
    if let Some(ref err) = close_err {
        store.set_close_error(&swept.id, err).await;
        error!(window_id = %swept.id, error = %err, "maintenance: close action failed on TTL expiry");
    }
    match MaintenanceAuditLog::open(home_dir) {
        Ok(audit) => audit.record(
            "ttl_expired",
            json!({ "window_id": swept.id, "sub_capabilities": swept.sub_capabilities, "close_error": close_err }),
        ),
        Err(e) => warn!(error = %e, "maintenance: audit log open failed for ttl_expired event"),
    }
    info!(window_id = %swept.id, "maintenance mode auto-closed: TTL expired");
}

/// §2.4 boot-time reconciliation — called ONLY from the gateway boot path
/// (mirrors `goal_loop::pause_inflight_on_restart`'s own "never from hot
/// reload" contract). Returns the number of windows force-closed (0 or 1 by
/// construction, but the caller only needs "did anything happen").
pub async fn reassert_closed_on_boot(home_dir: &Path) -> usize {
    let store = match MaintenanceStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            error!(
                error = %e,
                "maintenance: store open failed for boot reassert-closed — cannot verify no orphan window is open"
            );
            return 0;
        }
    };
    let window = match store.reassert_closed_on_boot().await {
        Ok(Some(w)) => w,
        Ok(None) => return 0,
        Err(e) => {
            error!(error = %e, "maintenance: reassert_closed_on_boot query failed");
            return 0;
        }
    };

    let close_err = run_close_actions(&window.sub_capabilities).await;
    if let Some(ref err) = close_err {
        store.set_close_error(&window.id, err).await;
        error!(
            window_id = %window.id, error = %err,
            "maintenance: close action failed during boot reassert-closed — a sub-capability may still be open"
        );
    }
    match MaintenanceAuditLog::open(home_dir) {
        Ok(audit) => audit.record(
            "gateway_restart_revoked",
            json!({ "window_id": window.id, "sub_capabilities": window.sub_capabilities, "close_error": close_err }),
        ),
        Err(e) => warn!(error = %e, "maintenance: audit log open failed for gateway_restart_revoked event"),
    }
    warn!(window_id = %window.id, "maintenance mode force-closed: gateway restarted while a window was open");
    1
}

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
    if !window.sub_capabilities.iter().any(|c| c == SUB_CAPABILITY_SHOW_DETAILS) {
        return Err("show_details sub-capability is not unlocked in the active window".to_string());
    }
    let audit = MaintenanceAuditLog::open(home_dir).map_err(|e| format!("open maintenance audit log: {e}"))?;
    fields.insert("window_id".to_string(), json!(window.id));
    fields.insert("viewed_by".to_string(), json!(viewed_by));
    fields.insert("viewed_by_email".to_string(), json!(viewed_by_email));
    audit.record(kind, Value::Object(fields));
    Ok(())
}

/// Assembles the `maintenance.status()` RPC payload — active state, the
/// active window's public fields (never `close_error`, an internal forensic
/// note), and the live SSH probe (§6 principle #2).
pub async fn status_json(home_dir: &Path) -> Result<Value, String> {
    let store = MaintenanceStore::open(home_dir)?;
    let active = store.active_window().await?;
    let ssh_observed = ssh_service_observed_active().await;
    let now = Utc::now();
    Ok(json!({
        "active": active.is_some(),
        "window": active.as_ref().map(|w| json!({
            "id": w.id,
            "enabled_by": w.enabled_by,
            "enabled_by_email": w.enabled_by_email,
            "enabled_at": w.enabled_at,
            "ttl_seconds": w.ttl_seconds,
            "expires_at": w.expires_at,
            "sub_capabilities": w.sub_capabilities,
            "remaining_seconds": w.remaining_seconds(now),
        })),
        "ssh_observed_active": ssh_observed,
    }))
}

/// Assembles the `maintenance.history()` RPC payload from the JSONL audit
/// trail (NOT the SQLite lifecycle table — see the module doc comment for
/// why `history` reads the richer file: it also carries every fine-grained
/// access event, not just enable/disable/expire transitions).
pub fn history_json(home_dir: &Path, limit: usize, offset: usize) -> Result<Value, String> {
    let audit = MaintenanceAuditLog::open(home_dir).map_err(|e| format!("open maintenance audit log: {e}"))?;
    let events = audit.read_recent(limit, offset).map_err(|e| format!("read maintenance history: {e}"))?;
    Ok(json!({ "events": events }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn window(ttl_hours: i64) -> (MaintenanceStore, MaintenanceWindow) {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store
            .enable("u1", "admin@local", ttl_hours, &[SUB_CAPABILITY_SSH.to_string()])
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
        assert_eq!(ok, vec![SUB_CAPABILITY_SSH.to_string(), SUB_CAPABILITY_SHOW_DETAILS.to_string()]);
    }

    #[tokio::test]
    async fn enable_refuses_ttl_out_of_range() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        assert!(store.enable("u1", "a@b", 0, &[SUB_CAPABILITY_SSH.to_string()]).await.is_err());
        assert!(store
            .enable("u1", "a@b", MAX_TTL_HOURS + 1, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .is_err());
        assert!(store
            .enable("u1", "a@b", MAX_TTL_HOURS, &[SUB_CAPABILITY_SSH.to_string()])
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn enable_refuses_when_already_active_no_accumulation() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        store.enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
        let second = store.enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()]).await;
        assert!(second.is_err(), "a second enable while one is active must be refused");
    }

    #[tokio::test]
    async fn active_window_reflects_ttl_expiry_without_a_sweep() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        // TTL 24h so the row is comfortably active for read-consistency checks.
        store.enable("u1", "a@b", 24, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
        assert!(store.active_window().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn expire_stale_is_a_noop_before_ttl_and_revokes_after() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store.enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
        assert!(store.expire_stale().await.unwrap().is_none(), "not expired yet");

        // Force expiry by rewriting expires_at into the past directly (unit
        // test, not an integration timing test).
        {
            let conn = store.conn.lock().await;
            conn.execute(
                "UPDATE maintenance_windows SET expires_at = ?1 WHERE id = ?2",
                params![(Utc::now() - chrono::Duration::seconds(10)).to_rfc3339(), w.id],
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
        assert_eq!(revoked.revoked_at, None, "returned row is the PRE-revoke snapshot");
        assert!(store.active_window().await.unwrap().is_none());

        // A second disable with nothing active is a harmless no-op, not an error.
        assert!(store.disable().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reassert_closed_on_boot_force_closes_even_with_ttl_remaining() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        store.enable("u1", "a@b", 24, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
        assert!(store.active_window().await.unwrap().is_some());

        let reasserted = store.reassert_closed_on_boot().await.unwrap();
        assert!(reasserted.is_some(), "must force-close even with 24h left");
        assert!(store.active_window().await.unwrap().is_none());

        let windows = store.recent_windows(10).await.unwrap();
        assert_eq!(windows[0].revoke_reason.as_deref(), Some(REVOKE_REASON_RESTART));

        // Idempotent on a clean boot (nothing was open).
        assert!(store.reassert_closed_on_boot().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn is_active_fails_closed_on_unparseable_expiry() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store.enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
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
        let w = store.enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
        assert!(!w.reminder_due(Utc::now()), "must not be due immediately after enabling");

        let almost_expired = Utc::now() + chrono::Duration::seconds((w.ttl_seconds as f64 * 0.9) as i64);
        assert!(w.reminder_due(almost_expired));

        let already_expired = Utc::now() + chrono::Duration::seconds(w.ttl_seconds + 10);
        assert!(!w.reminder_due(already_expired), "past expiry is a denial, not a reminder");
    }

    #[tokio::test]
    async fn mark_reminded_is_idempotent_and_stops_further_reminders() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let w = store.enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
        store.mark_reminded(&w.id).await.unwrap();
        let windows = store.recent_windows(1).await.unwrap();
        assert!(windows[0].reminded_at.is_some());
        let almost_expired = Utc::now() + chrono::Duration::seconds((w.ttl_seconds as f64 * 0.9) as i64);
        assert!(!windows[0].reminder_due(almost_expired), "already reminded ⇒ never due again");
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
        std::fs::write(&path, "{\"kind\":\"enabled\",\"ts_ms\":1}\nnot json\n{\"kind\":\"disabled\",\"ts_ms\":2}\n")
            .unwrap();
        let log = MaintenanceAuditLog::open(dir.path()).unwrap();
        let rows = log.read_recent(10, 0).unwrap();
        assert_eq!(rows.len(), 2, "the malformed line must be skipped, not abort the whole read");
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

    #[tokio::test]
    async fn ssh_service_observed_active_is_none_off_appliance() {
        assert!(!duduclaw_core::is_appliance());
        assert_eq!(ssh_service_observed_active().await, None);
    }

    // ── Orchestration-level tests (enable/disable/sweep/reassert/log_access/
    // status_json/history_json) — use SUB_CAPABILITY_SHOW_DETAILS only, since
    // it needs no system action and so behaves identically on and off the
    // appliance (this dev/CI host is never the appliance — SSH-gated
    // orchestration paths are covered indirectly by the "requires sysd, off-
    // appliance rejects" assertion below, mirroring `device_ops.rs`'s own
    // "authorization outcome, not underlying command" test discipline). ────

    #[tokio::test]
    async fn enable_and_disable_orchestration_round_trip_with_audit() {
        let home = tempfile::tempdir().unwrap();
        let window = enable(
            home.path(),
            "u1",
            "admin@local",
            4,
            &[SUB_CAPABILITY_SHOW_DETAILS.to_string()],
        )
        .await
        .unwrap();

        let status = status_json(home.path()).await.unwrap();
        assert_eq!(status["active"], true);
        assert_eq!(status["window"]["id"], window.id);

        let disabled = disable(home.path(), "u1", Some("done for the day")).await.unwrap().unwrap();
        assert_eq!(disabled.id, window.id);

        let status = status_json(home.path()).await.unwrap();
        assert_eq!(status["active"], false);

        let history = history_json(home.path(), 10, 0).unwrap();
        let events = history["events"].as_array().unwrap();
        assert_eq!(events[0]["kind"], "disabled");
        assert_eq!(events[1]["kind"], "enabled");
    }

    #[tokio::test]
    async fn enable_ssh_off_appliance_fails_closed_without_persisting_a_window() {
        let home = tempfile::tempdir().unwrap();
        let result = enable(home.path(), "u1", "admin@local", 4, &[SUB_CAPABILITY_SSH.to_string()]).await;
        assert!(result.is_err(), "SSH cannot open without a reachable sysd daemon");

        // The failed open must never have left a window on record.
        let status = status_json(home.path()).await.unwrap();
        assert_eq!(status["active"], false);
        let history = history_json(home.path(), 10, 0).unwrap();
        assert_eq!(history["events"][0]["kind"], "enable_failed");
    }

    #[tokio::test]
    async fn enable_refuses_double_enable_through_the_orchestration_layer() {
        let home = tempfile::tempdir().unwrap();
        enable(home.path(), "u1", "a@b", 4, &[SUB_CAPABILITY_SHOW_DETAILS.to_string()])
            .await
            .unwrap();
        let second = enable(home.path(), "u1", "a@b", 4, &[SUB_CAPABILITY_SHOW_DETAILS.to_string()]).await;
        assert!(second.is_err());
    }

    #[tokio::test]
    async fn sweep_expired_maintenance_window_closes_and_audits() {
        let home = tempfile::tempdir().unwrap();
        let window = enable(home.path(), "u1", "a@b", 4, &[SUB_CAPABILITY_SHOW_DETAILS.to_string()])
            .await
            .unwrap();

        // Force expiry directly on the store, same technique as the
        // store-level test above.
        let store = MaintenanceStore::open(home.path()).unwrap();
        {
            let conn = store.conn.lock().await;
            conn.execute(
                "UPDATE maintenance_windows SET expires_at = ?1 WHERE id = ?2",
                params![(Utc::now() - chrono::Duration::seconds(10)).to_rfc3339(), window.id],
            )
            .unwrap();
        }
        drop(store);

        sweep_expired_maintenance_window(home.path()).await;

        let status = status_json(home.path()).await.unwrap();
        assert_eq!(status["active"], false);
        let history = history_json(home.path(), 10, 0).unwrap();
        assert_eq!(history["events"][0]["kind"], "ttl_expired");

        // Idempotent: a second sweep with nothing left to expire is silent.
        sweep_expired_maintenance_window(home.path()).await;
        let history_after = history_json(home.path(), 10, 0).unwrap();
        assert_eq!(
            history["events"].as_array().unwrap().len(),
            history_after["events"].as_array().unwrap().len(),
            "a no-op sweep must not add a second ttl_expired line"
        );
    }

    #[tokio::test]
    async fn reassert_closed_on_boot_orchestration_force_closes_and_audits() {
        let home = tempfile::tempdir().unwrap();
        enable(home.path(), "u1", "a@b", 24, &[SUB_CAPABILITY_SHOW_DETAILS.to_string()])
            .await
            .unwrap();

        let closed = reassert_closed_on_boot(home.path()).await;
        assert_eq!(closed, 1);

        let status = status_json(home.path()).await.unwrap();
        assert_eq!(status["active"], false);
        let history = history_json(home.path(), 10, 0).unwrap();
        assert_eq!(history["events"][0]["kind"], "gateway_restart_revoked");

        // A clean boot (nothing was open) is a silent no-op.
        assert_eq!(reassert_closed_on_boot(home.path()).await, 0);
    }

    #[tokio::test]
    async fn log_access_requires_an_active_window_with_show_details_unlocked() {
        let home = tempfile::tempdir().unwrap();

        // No window open at all ⇒ refused.
        let mut fields = serde_json::Map::new();
        fields.insert("operation".to_string(), json!("device.update_apply"));
        assert!(log_access(home.path(), "u1", "a@b", "access_view_detail", fields.clone())
            .await
            .is_err());

        // Window open but WITHOUT show_details ⇒ still refused. Opened via the
        // low-level store directly (not the `enable()` orchestration, which
        // would try to actually start SSH and fail off-appliance) purely to
        // get a "window active, show_details NOT among its capabilities"
        // fixture state.
        let store = MaintenanceStore::open(home.path()).unwrap();
        store.enable("u1", "a@b", 4, &[SUB_CAPABILITY_SSH.to_string()]).await.unwrap();
        assert!(log_access(home.path(), "u1", "a@b", "access_view_detail", fields.clone())
            .await
            .is_err());
        store.disable().await.unwrap();

        // Window open WITH show_details ⇒ succeeds and is auditable.
        enable(home.path(), "u1", "a@b", 4, &[SUB_CAPABILITY_SHOW_DETAILS.to_string()])
            .await
            .unwrap();
        log_access(home.path(), "u1", "a@b", "access_view_detail", fields)
            .await
            .unwrap();
        let history = history_json(home.path(), 10, 0).unwrap();
        assert_eq!(history["events"][0]["kind"], "access_view_detail");
        assert_eq!(history["events"][0]["operation"], "device.update_apply");
        assert_eq!(history["events"][0]["viewed_by_email"], "a@b");
    }
}
