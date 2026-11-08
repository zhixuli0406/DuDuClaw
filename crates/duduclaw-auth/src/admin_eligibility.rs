//! Admin eligibility log (native Linux isolation v1.81, F6 design §3.4,
//! review rows S1 and S16).
//!
//! `admin_eligibility_log(seq, user_id, eligible, reason, actor_user_id,
//! notice_address, at)` records every change that can make an account an
//! eligible Admin or stop it being one. The restore/early-purge approval rule
//! (F6c2) counts qualifying Admins and the 7-day cooling window from this log,
//! never from the live account list, so deleting, demoting or locking out the
//! other Admins cannot manufacture a "sole Admin".
//!
//! **Eligible** = role `admin`, status `active`, no pending forced password
//! change, and at least one enrolled second factor (TOTP, a passkey, or an
//! email factor for the account's current address — the same set as
//! `mfa::factor_count`).
//!
//! **Same transaction, every writer.** Rows are appended by SQLite triggers,
//! not by application code, so every writer of the watched fields logs in the
//! statement (hence the transaction) that makes the change, including writers
//! that are added later or that bypass `UserDb`:
//! - `users`: insert of an Admin (`admin_created`), any change of `role`,
//!   `status`, `must_change_password` or `email` on a row that is or becomes an
//!   Admin (`role` / `status` / `forced_password` / `email`), delete of an Admin
//!   (`deleted`, `eligible = 0`, the deleted address kept as notice address);
//! - `mfa_credentials`, `mfa_passkeys`, `mfa_email_credentials`: enrolment,
//!   removal and change of a factor of an Admin;
//! - lockout: `mfa_credentials.failed_attempts` or `mfa_factor_failures.attempts`
//!   reaching the lockout threshold (5). A lockout row repeats the current
//!   eligibility: lockout never makes an account ineligible, so failed logins
//!   cannot shrink the count.
//!
//! The log is append-only (delete refused, update refused) except that a
//! writer that knows the acting Admin stamps `actor_user_id` once, inside its
//! own transaction ([`stamp_actor`] after [`high_water`]). A NULL actor means
//! "not attributed" (CLI, migration, self-service, trigger-only writers); the
//! S2 rule must treat an unattributed promotion as possibly by the requester.
//!
//! The table is created once, seeded with one `seed` row per account eligible
//! at that moment, and `auth_meta.admin_eligibility_log_since` records the
//! creation time (Unix seconds): until the log covers 7 days F6c2 refuses
//! self-approval (`ADMIN_LOG_UNAVAILABLE`). `auth_meta.admin_eligibility_log_schema`
//! is the schema version; a newer version than this build knows refuses to
//! open users.db rather than writing an older shape into it.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

/// Schema version of the log written by this build.
pub const SCHEMA_VERSION: i64 = 1;
const SCHEMA_KEY: &str = "admin_eligibility_log_schema";
/// `auth_meta` key: Unix seconds at which the log was created.
pub const SINCE_KEY: &str = "admin_eligibility_log_since";
/// Failed second-factor attempts that lock an account (`mfa::check_factor`).
pub const LOCKOUT_ATTEMPTS: i64 = 5;

const NOW: &str = "CAST(strftime('%s','now') AS INTEGER)";

/// SQL expression: 1 when the account `id` (an SQL expression) is an
/// eligible Admin now, else 0 (also for a missing account).
fn eligible_sql(id: &str) -> String {
    format!(
        "COALESCE((SELECT (u.role='admin' AND u.status='active' AND u.must_change_password=0 AND (\
         EXISTS(SELECT 1 FROM mfa_credentials m WHERE m.user_id=u.id) OR \
         EXISTS(SELECT 1 FROM mfa_passkeys p WHERE p.user_id=u.id) OR \
         EXISTS(SELECT 1 FROM mfa_email_credentials e WHERE e.user_id=u.id AND e.email=u.email))) \
         FROM users u WHERE u.id={id}),0)"
    )
}

fn append_sql(id: &str, reason: &str) -> String {
    format!(
        "INSERT INTO admin_eligibility_log(user_id,eligible,reason,actor_user_id,notice_address,at) \
         VALUES({id},{},{reason},NULL,(SELECT email FROM users WHERE id={id}),{NOW});",
        eligible_sql(id)
    )
}

fn is_admin_sql(id: &str) -> String {
    format!("EXISTS(SELECT 1 FROM users WHERE id={id} AND role='admin')")
}

fn schema_sql() -> String {
    let mut sql = String::from(
        "CREATE TABLE IF NOT EXISTS admin_eligibility_log (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL,
            eligible INTEGER NOT NULL CHECK (eligible IN (0, 1)),
            reason TEXT NOT NULL,
            actor_user_id TEXT,
            notice_address TEXT,
            at INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS idx_admin_eligibility_log_at ON admin_eligibility_log(at);
         CREATE INDEX IF NOT EXISTS idx_admin_eligibility_log_user ON admin_eligibility_log(user_id, seq);
         CREATE TRIGGER IF NOT EXISTS admin_eligibility_log_no_delete BEFORE DELETE ON admin_eligibility_log
         BEGIN SELECT RAISE(ABORT, 'admin eligibility log is append-only'); END;
         CREATE TRIGGER IF NOT EXISTS admin_eligibility_log_actor_once BEFORE UPDATE ON admin_eligibility_log
         WHEN OLD.actor_user_id IS NOT NULL OR NEW.actor_user_id IS NULL OR NEW.seq IS NOT OLD.seq
            OR NEW.user_id IS NOT OLD.user_id OR NEW.eligible IS NOT OLD.eligible OR NEW.reason IS NOT OLD.reason
            OR NEW.notice_address IS NOT OLD.notice_address OR NEW.at IS NOT OLD.at
         BEGIN SELECT RAISE(ABORT, 'admin eligibility log is append-only'); END;\n",
    );
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS admin_eligibility_user_insert AFTER INSERT ON users
         WHEN NEW.role='admin' BEGIN {} END;\n",
        append_sql("NEW.id", "'admin_created'")
    ));
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS admin_eligibility_user_update
         AFTER UPDATE OF role, status, must_change_password, email ON users
         WHEN (OLD.role='admin' OR NEW.role='admin') AND (OLD.role IS NOT NEW.role OR OLD.status IS NOT NEW.status
            OR OLD.must_change_password IS NOT NEW.must_change_password OR OLD.email IS NOT NEW.email)
         BEGIN {} END;\n",
        append_sql("NEW.id", "CASE WHEN OLD.role IS NOT NEW.role THEN 'role' \
            WHEN OLD.status IS NOT NEW.status THEN 'status' \
            WHEN OLD.must_change_password IS NOT NEW.must_change_password THEN 'forced_password' ELSE 'email' END")
    ));
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS admin_eligibility_user_delete AFTER DELETE ON users
         WHEN OLD.role='admin' BEGIN
            INSERT INTO admin_eligibility_log(user_id,eligible,reason,actor_user_id,notice_address,at)
            VALUES(OLD.id,0,'deleted',NULL,OLD.email,{NOW});
         END;\n"
    ));
    // Second factors of an Admin: enrolment, removal and change.
    for (table, added, removed) in [
        ("mfa_credentials", "'mfa_enrolled'", "'mfa_removed'"),
        ("mfa_passkeys", "'passkey_added'", "'passkey_removed'"),
        ("mfa_email_credentials", "'email_factor_added'", "'email_factor_removed'"),
    ] {
        sql.push_str(&format!(
            "CREATE TRIGGER IF NOT EXISTS admin_eligibility_{table}_insert AFTER INSERT ON {table}
             WHEN {} BEGIN {} END;
             CREATE TRIGGER IF NOT EXISTS admin_eligibility_{table}_delete AFTER DELETE ON {table}
             WHEN {} BEGIN {} END;\n",
            is_admin_sql("NEW.user_id"), append_sql("NEW.user_id", added),
            is_admin_sql("OLD.user_id"), append_sql("OLD.user_id", removed),
        ));
    }
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS admin_eligibility_mfa_email_credentials_update
         AFTER UPDATE ON mfa_email_credentials WHEN {} BEGIN {} END;\n",
        is_admin_sql("NEW.user_id"), append_sql("NEW.user_id", "'email_factor_changed'")
    ));
    // Lockout: logged, never an eligibility change.
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS admin_eligibility_totp_lockout AFTER UPDATE OF failed_attempts ON mfa_credentials
         WHEN NEW.failed_attempts>={LOCKOUT_ATTEMPTS} AND OLD.failed_attempts<{LOCKOUT_ATTEMPTS} AND {}
         BEGIN {} END;
         CREATE TRIGGER IF NOT EXISTS admin_eligibility_factor_lockout_insert AFTER INSERT ON mfa_factor_failures
         WHEN NEW.attempts>={LOCKOUT_ATTEMPTS} AND {} BEGIN {} END;
         CREATE TRIGGER IF NOT EXISTS admin_eligibility_factor_lockout_update AFTER UPDATE OF attempts ON mfa_factor_failures
         WHEN NEW.attempts>={LOCKOUT_ATTEMPTS} AND OLD.attempts<{LOCKOUT_ATTEMPTS} AND {} BEGIN {} END;\n",
        is_admin_sql("NEW.user_id"), append_sql("NEW.user_id", "'lockout'"),
        is_admin_sql("NEW.user_id"), append_sql("NEW.user_id", "'lockout'"),
        is_admin_sql("NEW.user_id"), append_sql("NEW.user_id", "'lockout'"),
    ));
    sql
}

/// Create (once, seeded) or validate the log and its triggers. Runs at the
/// end of `UserDb::init_tables`, after the MFA tables exist. Idempotent; a
/// schema version newer than [`SCHEMA_VERSION`] is refused.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let version: Option<i64> = tx
        .query_row("SELECT value FROM auth_meta WHERE key=?1", [SCHEMA_KEY], |r| r.get(0))
        .optional().map_err(|e| e.to_string())?;
    if let Some(version) = version.filter(|v| *v > SCHEMA_VERSION) {
        return Err(format!("users.db admin eligibility log schema {version} is newer than this build ({SCHEMA_VERSION})"));
    }
    let exists: bool = tx
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='admin_eligibility_log')",
            [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    tx.execute_batch(&schema_sql()).map_err(|e| format!("admin eligibility log migration failed: {e}"))?;
    if !exists {
        tx.execute_batch(&format!(
            "INSERT INTO admin_eligibility_log(user_id,eligible,reason,actor_user_id,notice_address,at)
             SELECT u.id,1,'seed',NULL,u.email,{NOW} FROM users u WHERE {}=1 ORDER BY u.id;
             INSERT INTO auth_meta(key,value) VALUES('{SINCE_KEY}',{NOW})
             ON CONFLICT(key) DO UPDATE SET value=excluded.value;",
            eligible_sql("u.id")
        )).map_err(|e| format!("admin eligibility log seed failed: {e}"))?;
    }
    tx.execute("INSERT INTO auth_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![SCHEMA_KEY, SCHEMA_VERSION]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Highest `seq` so far (0 for an empty log). Read it inside the writer's
/// transaction before the change, then [`stamp_actor`] after it.
pub fn high_water(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COALESCE(MAX(seq),0) FROM admin_eligibility_log", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// Attribute every not-yet-attributed row after `since` to `actor`. Call in
/// the same transaction as the change (the rows exist only there).
pub fn stamp_actor(conn: &Connection, since: i64, actor: &str) -> Result<(), String> {
    conn.execute("UPDATE admin_eligibility_log SET actor_user_id=?2 WHERE seq>?1 AND actor_user_id IS NULL",
        params![since, actor]).map(|_| ()).map_err(|e| e.to_string())
}

/// Whether `user` is an eligible Admin now (same rule as the log).
pub fn is_eligible(conn: &Connection, user: &str) -> Result<bool, String> {
    conn.query_row(&format!("SELECT {}", eligible_sql("?1")), [user], |r| r.get(0)).map_err(|e| e.to_string())
}

/// When the log was created (Unix seconds); `None` when the marker is
/// missing, which F6c2 treats as an unreadable log.
pub fn log_since(conn: &Connection) -> Result<Option<i64>, String> {
    conn.query_row("SELECT value FROM auth_meta WHERE key=?1", [SINCE_KEY], |r| r.get(0))
        .optional().map_err(|e| e.to_string())
}

/// One log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibilityEntry {
    pub seq: i64,
    pub user_id: String,
    pub eligible: bool,
    pub reason: String,
    pub actor_user_id: Option<String>,
    pub notice_address: Option<String>,
    pub at: i64,
}

/// Rows with `at >= since`, oldest first, at most `limit` (an over-long
/// answer is an error, never a truncated history).
pub fn entries_since(conn: &Connection, since: i64, limit: usize) -> Result<Vec<EligibilityEntry>, String> {
    let bound = i64::try_from(limit.saturating_add(1)).map_err(|e| e.to_string())?;
    let mut statement = conn.prepare(
        "SELECT seq,user_id,eligible,reason,actor_user_id,notice_address,at FROM admin_eligibility_log
         WHERE at>=?1 ORDER BY seq LIMIT ?2").map_err(|e| e.to_string())?;
    let rows = statement.query_map(params![since, bound], |r| Ok(EligibilityEntry {
        seq: r.get(0)?, user_id: r.get(1)?, eligible: r.get::<_, i64>(2)? == 1, reason: r.get(3)?,
        actor_user_id: r.get(4)?, notice_address: r.get(5)?, at: r.get(6)?,
    })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    if rows.len() > limit { return Err("admin eligibility log longer than the read limit".into()); }
    Ok(rows)
}

impl crate::UserDb {
    /// Every log row (tests and the F6c2 rule read through [`entries_since`]).
    pub fn admin_eligibility_entries(&self) -> Result<Vec<EligibilityEntry>, String> {
        entries_since(&self.conn(), i64::MIN, 1_000_000)
    }
}

#[cfg(test)]
#[path = "admin_eligibility_tests.rs"]
mod tests;
