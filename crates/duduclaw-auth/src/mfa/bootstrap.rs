//! RM-135: first-time two-factor enrollment of the first Administrator.
//!
//! Under an enforced policy a new account needs an *independent* enrollment
//! proof (`enrollment.rs`): a channel verified before enforcement, or a code
//! issued by another Admin or by the host. A fresh installation's first
//! Administrator has neither, so it could only finish through a host command.
//! This module adds a third proof kind, `bootstrap`, granted **only** by a
//! valid one-time setup code generated on the host, in addition to the
//! password the login itself verified:
//!
//! - the code is 128 random bits, generated only by the host command
//!   `duduclaw ops users mfa-setup-code` (never by the daemon, never printed
//!   to a log), stored only as its SHA-256 digest in the singleton
//!   `mfa_bootstrap_tokens` row, bound to the bootstrap owner, valid for
//!   [`BOOTSTRAP_TOKEN_TTL`] seconds, consumed by the first correct use and
//!   dropped after [`BOOTSTRAP_TOKEN_MAX_ATTEMPTS`] wrong ones. Comparison is
//!   constant-time.
//! - There is no "same machine" (loopback) claim: behind a reverse proxy on
//!   the same host every peer is loopback, so the peer address proves nothing.
//!
//! **Window.** A bootstrap proof is accepted only while
//! [`bootstrap_allowed`]: the policy is enforced, the enrolling account is the
//! active bootstrap-owner Administrator, **no Administrator (any status) has
//! any second factor**, and the **closed latch** is not set. The check runs
//! when the proof is made and again, in the same transaction that writes the
//! factor, when the enrollment completes (`require_independent_proof`).
//!
//! **Closed latch (once per installation).** `mfa_bootstrap_state` holds at
//! most one row; its presence means "the first Administrator factor was
//! written, bootstrap is over for good". Triggers insert it in the very
//! statement (hence transaction) that gives any Administrator a factor —
//! whichever writer adds it — and refuse every `UPDATE` and `DELETE` of it,
//! so later factor deletion, demotion, suspension or account deletion never
//! re-opens the window. When the table is first created on an existing
//! installation that already shows a past enrollment (any factor or recovery
//! code row, or an eligibility-log row recording an eligible Admin), the
//! migration sets it at once. An unreadable latch refuses bootstrap.
//! Triggers also delete every outstanding setup code as soon as any
//! Administrator gains a factor. Codes never enter any log; only the method
//! is recorded.
use super::*;

/// Lifetime of a host setup code (seconds) — the same 30 minutes as the
/// host-issued account setup token (`otp::SETUP_TOKEN_TTL_SECS`).
pub const BOOTSTRAP_TOKEN_TTL: i64 = crate::otp::SETUP_TOKEN_TTL_SECS;
/// Wrong presentations after which the outstanding code is dropped.
pub const BOOTSTRAP_TOKEN_MAX_ATTEMPTS: i64 = 5;
/// Random bytes in a setup code (shown as 32 hexadecimal characters).
const TOKEN_BYTES: usize = 16;
const TOKEN_CHARS: usize = TOKEN_BYTES * 2;
/// Audit method of the only bootstrap proof.
const METHOD: &str = "setup_code";

/// Refusal of setup-code issuance once the latch is set (plain zh-TW, points
/// to the existing host recovery commands).
pub const BOOTSTRAP_CLOSED: &str = "這套安裝的第一位管理員已完成過兩步驟驗證設定，首次設定碼不能再核發。\
若有管理員無法再用兩步驟驗證登入，請在執行 DuDuClaw 的主機上執行 duduclaw ops users mfa-reset <email> 重設該帳號，\
再依登入畫面的指示，在主機上執行 duduclaw ops users mfa-enrollment-code 取得註冊碼。";

/// SQL: some Administrator (any status) has any second factor. Stricter than
/// `factor_count`: an email factor counts even for a stale address.
const ADMIN_FACTOR_EXISTS: &str = "SELECT EXISTS(SELECT 1 FROM users u WHERE u.role='admin' AND (
    EXISTS(SELECT 1 FROM mfa_credentials m WHERE m.user_id=u.id)
    OR EXISTS(SELECT 1 FROM mfa_passkeys p WHERE p.user_id=u.id)
    OR EXISTS(SELECT 1 FROM mfa_email_credentials e WHERE e.user_id=u.id)))";

/// SQL statement body: set the closed latch (never conflicts, so an outer
/// `INSERT OR …` conflict policy cannot turn it into an error or a no-op).
fn set_latch_sql(reason: &str) -> String {
    format!("INSERT INTO mfa_bootstrap_state(id,completed_at,reason)
        SELECT 1,CAST(strftime('%s','now') AS INTEGER),'{reason}'
        WHERE NOT EXISTS(SELECT 1 FROM mfa_bootstrap_state);")
}

/// SQL: `id` (an SQL expression) has a second factor of any kind.
fn has_factor_sql(id: &str) -> String {
    format!("(EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id={id})
        OR EXISTS(SELECT 1 FROM mfa_passkeys WHERE user_id={id})
        OR EXISTS(SELECT 1 FROM mfa_email_credentials WHERE user_id={id}))")
}

/// Evidence on an existing installation that a first enrollment already
/// happened: any factor or recovery-code row of any account, or an
/// eligibility-log row recording an eligible Administrator.
const PAST_ENROLLMENT_EXISTS: &str = "SELECT EXISTS(SELECT 1 FROM mfa_credentials)
    OR EXISTS(SELECT 1 FROM mfa_passkeys) OR EXISTS(SELECT 1 FROM mfa_email_credentials)
    OR EXISTS(SELECT 1 FROM mfa_recovery_codes)
    OR EXISTS(SELECT 1 FROM admin_eligibility_log WHERE eligible=1)";

/// Create the setup-code table, the closed latch and the triggers that end
/// the bootstrap window. Runs at the end of `UserDb::init_tables`, after every
/// MFA table, the eligibility log and the `users.bootstrap_owner` column
/// exist. Idempotent.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let latch_existed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='mfa_bootstrap_state')",
        [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let mut sql = String::from(
        "CREATE TABLE IF NOT EXISTS mfa_bootstrap_tokens (
            id INTEGER PRIMARY KEY CHECK(id=1),
            user_id TEXT NOT NULL,
            token_hash TEXT NOT NULL,
            expires_at INTEGER NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE IF NOT EXISTS mfa_bootstrap_state (
            id INTEGER PRIMARY KEY CHECK(id=1),
            completed_at INTEGER NOT NULL,
            reason TEXT NOT NULL);
         CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_state_no_delete BEFORE DELETE ON mfa_bootstrap_state
         BEGIN SELECT RAISE(ABORT, 'first administrator setup is closed for good'); END;
         CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_state_no_update BEFORE UPDATE ON mfa_bootstrap_state
         BEGIN SELECT RAISE(ABORT, 'first administrator setup is closed for good'); END;
         CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_end_admin_promoted AFTER UPDATE OF role ON users
         WHEN NEW.role='admin' AND (EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=NEW.id)
            OR EXISTS(SELECT 1 FROM mfa_passkeys WHERE user_id=NEW.id)
            OR EXISTS(SELECT 1 FROM mfa_email_credentials WHERE user_id=NEW.id))
         BEGIN DELETE FROM mfa_bootstrap_tokens; END;
         CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_end_owner_deleted AFTER DELETE ON users
         WHEN EXISTS(SELECT 1 FROM mfa_bootstrap_tokens WHERE user_id=OLD.id)
         BEGIN DELETE FROM mfa_bootstrap_tokens; END;\n",
    );
    for table in ["mfa_credentials", "mfa_passkeys", "mfa_email_credentials"] {
        sql.push_str(&format!(
            "CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_end_{table}_insert AFTER INSERT ON {table}
             WHEN EXISTS(SELECT 1 FROM users WHERE id=NEW.user_id AND role='admin')
             BEGIN DELETE FROM mfa_bootstrap_tokens; END;
             CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_latch_{table}_insert AFTER INSERT ON {table}
             WHEN EXISTS(SELECT 1 FROM users WHERE id=NEW.user_id AND role='admin')
             BEGIN {} END;\n",
            set_latch_sql("admin_factor"),
        ));
    }
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_end_mfa_email_credentials_update AFTER UPDATE ON mfa_email_credentials
         WHEN EXISTS(SELECT 1 FROM users WHERE id=NEW.user_id AND role='admin')
         BEGIN DELETE FROM mfa_bootstrap_tokens; END;
         CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_latch_mfa_email_credentials_update AFTER UPDATE ON mfa_email_credentials
         WHEN EXISTS(SELECT 1 FROM users WHERE id=NEW.user_id AND role='admin')
         BEGIN {} END;
         CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_latch_admin_promoted AFTER UPDATE OF role ON users
         WHEN NEW.role='admin' AND {}
         BEGIN {} END;
         CREATE TRIGGER IF NOT EXISTS mfa_bootstrap_latch_admin_created AFTER INSERT ON users
         WHEN NEW.role='admin' AND {}
         BEGIN {} END;\n",
        set_latch_sql("admin_factor"),
        has_factor_sql("NEW.id"), set_latch_sql("admin_promoted_with_factor"),
        has_factor_sql("NEW.id"), set_latch_sql("admin_created_with_factor"),
    ));
    tx.execute_batch(&sql).map_err(|e| format!("MFA bootstrap migration failed: {e}"))?;
    // Existing installation, first run of this build: close at once when a
    // first enrollment demonstrably happened before.
    if !latch_existed {
        let past: bool = tx.query_row(PAST_ENROLLMENT_EXISTS, [], |r| r.get(0))
            .map_err(|e| format!("MFA bootstrap migration failed: {e}"))?;
        let admin_factor: bool = tx.query_row(ADMIN_FACTOR_EXISTS, [], |r| r.get(0))
            .map_err(|e| format!("MFA bootstrap migration failed: {e}"))?;
        if past || admin_factor {
            tx.execute_batch(&set_latch_sql("migration_existing_enrollment"))
                .map_err(|e| format!("MFA bootstrap migration failed: {e}"))?;
            tx.execute("DELETE FROM mfa_bootstrap_tokens", []).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Whether the closed latch is set. A missing table, a read error or an
/// impossible row count is an error, never "open".
pub(super) fn latch_closed(conn: &Connection) -> Result<bool> {
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM mfa_bootstrap_state WHERE id=1", [], |r| r.get(0))
        .map_err(|e| format!("first administrator setup state unreadable: {e}"))?;
    match rows { 0 => Ok(false), 1 => Ok(true), _ => Err("first administrator setup state invalid".into()) }
}

/// Whether a bootstrap proof may be made or used for `user` now (module docs).
/// Every read error is an error, never "allowed".
pub(super) fn bootstrap_allowed(conn: &Connection, user: &str) -> Result<bool> {
    if latch_closed(conn)? { return Ok(false); }
    if !read_enforcement(conn)? { return Ok(false); }
    let owner: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND bootstrap_owner=1 AND role='admin' AND status='active')",
        [user], |r| r.get(0)).map_err(|e| e.to_string())?;
    if !owner { return Ok(false); }
    let factor: bool = conn.query_row(ADMIN_FACTOR_EXISTS, [], |r| r.get(0)).map_err(|e| e.to_string())?;
    Ok(!factor)
}

/// The single active bootstrap owner, if exactly one exists.
fn sole_owner(conn: &Connection) -> Result<Option<String>> {
    let mut q = conn.prepare("SELECT id FROM users WHERE bootstrap_owner=1 AND role='admin' AND status='active' LIMIT 2")
        .map_err(|e| e.to_string())?;
    let owners = q.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(match owners.as_slice() { [owner] => Some(owner.clone()), _ => None })
}

fn valid_code(code: &str) -> bool {
    code.len() == TOKEN_CHARS && code.bytes().all(|b| b.is_ascii_hexdigit())
}

impl UserDb {
    /// Whether the enrolling `user` may use a bootstrap proof now (login hint:
    /// the dashboard then asks for the host setup code).
    pub fn mfa_bootstrap_available(&self, user: &str) -> Result<bool> {
        bootstrap_allowed(&self.conn(), user)
    }

    /// Whether the first-Administrator setup window is open now (gateway
    /// start: tell the operator to run the host command; never issues a code).
    pub fn mfa_bootstrap_pending(&self) -> Result<bool> {
        let conn = self.conn();
        match sole_owner(&conn)? {
            Some(owner) => bootstrap_allowed(&conn, &owner),
            None => Ok(false),
        }
    }

    /// Whether the once-per-installation closed latch is set (module docs).
    pub fn mfa_bootstrap_closed(&self) -> Result<bool> {
        latch_closed(&self.conn())
    }

    /// Host command only (`duduclaw ops users mfa-setup-code`): issue a new
    /// one-time setup code for the bootstrap owner, replacing any outstanding
    /// one. `Err(`[`BOOTSTRAP_CLOSED`]`)` once the closed latch is set (or an
    /// Administrator has a factor); `None` (and every outstanding code
    /// dropped) when no bootstrap is possible for another reason: the policy
    /// is not enforced or there is no single active bootstrap-owner Admin.
    /// `issuer` is a fixed label recorded in the audit row. Returns the
    /// plaintext once; the caller must not log it.
    pub fn mfa_issue_bootstrap_token(&self, issuer: &str, now: i64) -> Result<Option<String>> {
        if now <= 0 { return Err(DENIED.into()); }
        let token = random_hex(TOKEN_BYTES)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let closed = latch_closed(&tx)?
            || tx.query_row(ADMIN_FACTOR_EXISTS, [], |r| r.get::<_, bool>(0)).map_err(|e| e.to_string())?;
        if closed {
            tx.execute("DELETE FROM mfa_bootstrap_tokens", []).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            return Err(BOOTSTRAP_CLOSED.into());
        }
        let owner = match sole_owner(&tx)? {
            Some(owner) if bootstrap_allowed(&tx, &owner)? => owner,
            _ => {
                tx.execute("DELETE FROM mfa_bootstrap_tokens", []).map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(None);
            }
        };
        tx.execute("INSERT INTO mfa_bootstrap_tokens(id,user_id,token_hash,expires_at,attempts) VALUES(1,?1,?2,?3,0)
            ON CONFLICT(id) DO UPDATE SET user_id=excluded.user_id,token_hash=excluded.token_hash,
            expires_at=excluded.expires_at,attempts=0",
            params![owner, token_hash(&token), now + BOOTSTRAP_TOKEN_TTL]).map_err(|e| e.to_string())?;
        audit(&tx, &owner, "mfa.bootstrap_code_issued", Some(issuer))?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Some(token))
    }

    /// Turn the pending, unverified, public (HTTP login) enrollment proof of
    /// `token` into a verified `bootstrap` proof with the host setup code
    /// `code` (module docs). A wrong code counts toward
    /// [`BOOTSTRAP_TOKEN_MAX_ATTEMPTS`] and the enrollment challenge's own
    /// attempt limit, and is committed. There is no proof without a code.
    pub fn mfa_prove_bootstrap(&self, token: &str, code: &str, now: i64) -> Result<()> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(DENIED.into()); }
        if !valid_code(code) { return Err(DENIED.into()); }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let user = challenge_user(&tx, token, "enroll", now)?;
        active_user(&tx, &user)?;
        let pending: Option<(String, bool, Option<String>)> = tx.query_row(
            "SELECT kind,verified,session FROM mfa_enrollment_proofs WHERE token_hash=?1", [token_hash(token)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(|e| e.to_string())?;
        let usable = pending.as_ref().is_some_and(|(kind, verified, session)|
            !verified && session.is_none() && matches!(kind.as_str(), "admin" | "channel"));
        if !usable || any_factor(&tx, &user)? || !bootstrap_allowed(&tx, &user)? { return Err(DENIED.into()); }
        let stored: Option<(String, String, i64, i64)> = tx.query_row(
            "SELECT user_id,token_hash,expires_at,attempts FROM mfa_bootstrap_tokens WHERE id=1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().map_err(|e| e.to_string())?;
        let Some((owner, hash, expires_at, attempts)) = stored else { return Err(DENIED.into()) };
        if expires_at <= now || attempts >= BOOTSTRAP_TOKEN_MAX_ATTEMPTS {
            tx.execute("DELETE FROM mfa_bootstrap_tokens", []).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            return Err(DENIED.into());
        }
        let presented = token_hash(&code.to_ascii_lowercase());
        let matched: bool = hash.as_bytes().ct_eq(presented.as_bytes()).into();
        if !matched || owner != user {
            tx.execute("UPDATE mfa_bootstrap_tokens SET attempts=attempts+1 WHERE id=1", []).map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM mfa_bootstrap_tokens WHERE attempts>=?1", [BOOTSTRAP_TOKEN_MAX_ATTEMPTS])
                .map_err(|e| e.to_string())?;
            tx.execute("UPDATE mfa_challenges SET attempts=attempts+1 WHERE token_hash=?1", [token_hash(token)])
                .map_err(|e| e.to_string())?;
            audit(&tx, &user, "mfa.verification_failed", None)?;
            tx.commit().map_err(|e| e.to_string())?;
            return Err(DENIED.into());
        }
        // Single use: exactly this row, consumed in the proof transaction.
        let consumed = tx.execute("DELETE FROM mfa_bootstrap_tokens WHERE id=1 AND token_hash=?1 AND user_id=?2",
            params![hash, user]).map_err(|e| e.to_string())?;
        if consumed != 1 { return Err(DENIED.into()); }
        let changed = tx.execute("UPDATE mfa_enrollment_proofs SET kind='bootstrap',verified=1,code_hash=NULL
            WHERE token_hash=?1 AND verified=0 AND session IS NULL", [token_hash(token)]).map_err(|e| e.to_string())?;
        if changed != 1 { return Err(DENIED.into()); }
        audit(&tx, &user, "mfa.bootstrap_proved", Some(METHOD))?;
        tx.commit().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
