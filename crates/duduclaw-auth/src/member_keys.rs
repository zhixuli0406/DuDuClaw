//! RM-213 personal member keys. Only SHA-256 digests are persisted.
//! All authentication paths revalidate the row and live member identity.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use password_hash::rand_core::{OsRng, RngCore};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::UserDb;

/// Every member key starts with this. Other bearer surfaces refuse
/// anything carrying it ([`is_member_key`]).
pub const MEMBER_KEY_PREFIX: &str = "ddc_member_";
/// Active (unrevoked) credentials one account may hold at once.
pub const MAX_ACTIVE_MEMBER_KEYS: i64 = 5;
/// Longest label a staff member may give a credential.
pub const MEMBER_KEY_NAME_MAX_CHARS: usize = 60;
/// Random bytes behind each credential.
const SECRET_BYTES: usize = 32;
/// `base64url(32 bytes)` without padding.
const SECRET_CHARS: usize = 43;
/// Revoked rows kept per account for the history list; older ones are pruned
/// when a new credential is created, so the table cannot grow without bound.
const REVOKED_KEPT_PER_USER: i64 = 20;
/// Width of one endpoint-audit aggregation window.
pub const MEMBER_AUDIT_WINDOW_SECS: i64 = 600;
/// Endpoint audit-log rows and aggregates older than this are pruned.
pub const MEMBER_AUDIT_RETENTION_DAYS: i64 = 90;
/// Rows deleted per table by one prune pass.
pub const MEMBER_AUDIT_PRUNE_BATCH: i64 = 500;

/// Idempotent `member_api_keys` migration, invoked by `UserDb::init_tables`.
///
/// Offboarding an account revokes its credentials permanently (the trigger);
/// a *suspended* account's credentials are refused at request time instead,
/// so a reinstated account does not have to re-pair every external system.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS member_key_audit_agg (
            token_id TEXT NOT NULL,
            result TEXT NOT NULL,
            window_start INTEGER NOT NULL,
            user_id TEXT NOT NULL,
            audit_id INTEGER,
            count INTEGER NOT NULL,
            employees INTEGER NOT NULL,
            first_seen TEXT NOT NULL,
            last_seen TEXT NOT NULL,
            last_ip TEXT,
            PRIMARY KEY (token_id, result, window_start)
         );
         CREATE INDEX IF NOT EXISTS idx_member_key_audit_agg_window
             ON member_key_audit_agg(window_start);",
    )
    .map_err(|e| format!("member_key_audit_agg migration failed: {e}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS member_api_keys (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            token_hash TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            credential_epoch INTEGER NOT NULL,
            last_used_at TEXT,
            revoked_at TEXT
         );
         CREATE TRIGGER IF NOT EXISTS member_keys_acl_update
         AFTER UPDATE OF revoked_at, expires_at, credential_epoch ON member_api_keys
         BEGIN
             UPDATE auth_meta SET value=value+1 WHERE key='acl_generation';
         END;
         CREATE TRIGGER IF NOT EXISTS member_keys_acl_delete AFTER DELETE ON member_api_keys
         BEGIN
             UPDATE auth_meta SET value=value+1 WHERE key='acl_generation';
         END;
         CREATE TRIGGER IF NOT EXISTS member_keys_password_epoch
         AFTER UPDATE OF password_hash ON users
         WHEN OLD.password_hash != NEW.password_hash AND OLD.credential_epoch=0
              AND EXISTS(SELECT 1 FROM member_api_keys WHERE user_id=NEW.id)
         BEGIN
             UPDATE users SET credential_epoch=CASE WHEN credential_epoch=0 THEN 1 ELSE credential_epoch END WHERE id=NEW.id;
         END;
         CREATE INDEX IF NOT EXISTS idx_member_api_keys_user ON member_api_keys(user_id);
         CREATE TRIGGER IF NOT EXISTS member_api_keys_revoke_offboarded
         AFTER UPDATE OF status ON users
         WHEN NEW.status = 'offboarded' AND OLD.status != NEW.status
         BEGIN
             UPDATE member_api_keys SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE user_id = NEW.id AND revoked_at IS NULL;
         END;",
    )
    .map_err(|e| format!("member_api_keys migration failed: {e}"))
}

/// Does `raw` claim to be a member key? Prefix only, on purpose: every
/// non-status bearer surface uses this to refuse the whole credential type,
/// well-formed or not.
pub fn is_member_key(raw: &str) -> bool {
    raw.trim().starts_with(MEMBER_KEY_PREFIX)
}

/// Exact shape of a credential this module issued.
fn well_formed(raw: &str) -> bool {
    raw.strip_prefix(MEMBER_KEY_PREFIX).is_some_and(|secret| {
        secret.len() == SECRET_CHARS
            && secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

fn digest(secret: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, secret.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn clean_name(raw: &str) -> Result<String, MemberKeyError> {
    let name = raw.trim();
    if name.is_empty()
        || name.chars().count() > MEMBER_KEY_NAME_MAX_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(MemberKeyError::InvalidName);
    }
    Ok(name.to_string())
}

/// One credential as listed to its owner. Never carries the secret or digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberKeyInfo {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub expires_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    pub active: bool,
}

/// One credential as listed to an Admin, with its owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberKeyAdminEntry {
    #[serde(flatten)]
    pub token: MemberKeyInfo,
    pub user_id: String,
    /// `None` when the owner row no longer exists.
    pub user_email: Option<String>,
    pub user_display_name: Option<String>,
}

/// A freshly created credential. Redacting `Debug`, deliberately no `Serialize`:
/// `secret` is the bearer credential and is shown to its owner exactly once.
pub struct CreatedMemberKey {
    pub info: MemberKeyInfo,
    pub secret: String,
}

impl std::fmt::Debug for CreatedMemberKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedMemberKey").field("info", &self.info)
            .field("secret", &"[REDACTED]").finish()
    }
}

/// Why a create / revoke was refused. Store details go to the log only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKeyError {
    InvalidName,
    InvalidExpiry,
    AdminForbidden,
    LimitReached,
    AccountInactive,
    NotFound,
    Store,
}

impl MemberKeyError {
    /// Stable machine-readable code for the RPC error frame.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidExpiry => "member_key_invalid_expiry",
            Self::AdminForbidden => "member_key_admin_forbidden",
            Self::InvalidName => "member_key_invalid_name",
            Self::LimitReached => "member_key_limit_reached",
            Self::AccountInactive => "member_key_account_inactive",
            Self::NotFound => "member_key_not_found",
            Self::Store => "member_key_unavailable",
        }
    }
}

/// What a presented credential resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberKeyLookup {
    /// Malformed or unknown. Nothing to attribute it to.
    Unknown,
    /// A real credential that may not be used right now.
    Refused {
        token_id: String,
        user_id: String,
        /// `revoked` | `account_inactive`.
        reason: &'static str,
    },
    /// Usable. The caller still resolves the owner's live bindings.
    Valid { token_id: String, user_id: String },
}

fn store_error(e: impl std::fmt::Display) -> MemberKeyError {
    tracing::warn!(error = %e, "member key store error");
    MemberKeyError::Store
}

fn info_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemberKeyInfo> {
    let revoked_at: Option<String> = row.get(4)?;
    Ok(MemberKeyInfo {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        last_used_at: row.get(3)?,
        expires_at: row.get(5)?,
        active: revoked_at.is_none() && row.get::<_, bool>(6)?,
        revoked_at,
    })
}

const INFO_COLUMNS: &str = "t.id, t.name, t.created_at, t.last_used_at, t.revoked_at, t.expires_at,
    (julianday(t.expires_at) > julianday('now') AND EXISTS(SELECT 1 FROM users u
     WHERE u.id=t.user_id AND u.status='active' AND u.role IN ('employee','manager')
     AND u.must_change_password=0 AND u.credential_epoch=t.credential_epoch))";

fn audit(
    conn: &Connection,
    actor: &str,
    action: &str,
    token_id: &str,
    detail: serde_json::Value,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![actor, action, token_id, detail.to_string(), Utc::now().to_rfc3339()],
    )
}

/// What [`UserDb::member_key_record_call`] did with one endpoint call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberCallRecord {
    /// First call of this (credential, result) in the window: one audit-log
    /// row written, the aggregate opened, `last_used_at` stamped for `ok`.
    Opened,
    /// Counted into the window's open aggregate; nothing else written.
    Counted,
}

/// Delete at most `batch` expired endpoint audit-log rows and at most
/// `batch` expired aggregates. Other audit actions are never touched.
fn prune_member_audit(conn: &Connection, now: DateTime<Utc>, batch: i64) -> rusqlite::Result<usize> {
    let cutoff = now - chrono::Duration::days(MEMBER_AUDIT_RETENTION_DAYS);
    let aggregates = conn.execute(
        "DELETE FROM member_key_audit_agg WHERE rowid IN (
             SELECT rowid FROM member_key_audit_agg WHERE window_start < ?1 LIMIT ?2)",
        params![cutoff.timestamp(), batch],
    )?;
    let rows = conn.execute(
        "DELETE FROM auth_audit_log WHERE id IN (
             SELECT id FROM auth_audit_log
              WHERE action IN ('member_key.used', 'member_key.refused')
                AND timestamp < ?1
              ORDER BY timestamp LIMIT ?2)",
        params![cutoff.to_rfc3339(), batch],
    )?;
    Ok(aggregates + rows)
}

impl UserDb {
    /// Issue a credential for `user_id` (who must be an active account not
    /// awaiting a password change). At most [`MAX_ACTIVE_MEMBER_KEYS`]
    /// unrevoked credentials per account. The secret is returned once and
    /// only its digest is stored.
    pub fn member_key_create(
        &self,
        user_id: &str,
        name: &str,
        expires_in_days: u64,
        verified_epoch: u64,
    ) -> Result<CreatedMemberKey, MemberKeyError> {
        if !(1..=365).contains(&expires_in_days) { return Err(MemberKeyError::InvalidExpiry); }
        let name = clean_name(name)?;
        let mut bytes = [0u8; SECRET_BYTES];
        OsRng.try_fill_bytes(&mut bytes)
            .map_err(|_| MemberKeyError::Store)?;
        let secret = format!("{MEMBER_KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        let hash = digest(&secret);
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires_at = (now + chrono::Duration::days(expires_in_days as i64)).to_rfc3339();
        let now = now.to_rfc3339();

        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let role: Option<String> = tx.query_row("SELECT role FROM users WHERE id=?1", [user_id], |r| r.get(0))
            .optional().map_err(store_error)?;
        if role.as_deref() == Some("admin") { return Err(MemberKeyError::AdminForbidden); }
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM users
                   WHERE id = ?1 AND status = 'active' AND must_change_password = 0
                     AND role IN ('employee','manager') AND credential_epoch = ?2)",
                params![user_id, verified_epoch],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        if !active {
            return Err(MemberKeyError::AccountInactive);
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM member_api_keys WHERE user_id = ?1 AND revoked_at IS NULL
                   AND julianday(expires_at) > julianday('now') AND credential_epoch = ?2",
                params![user_id, verified_epoch],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        if count >= MAX_ACTIVE_MEMBER_KEYS {
            return Err(MemberKeyError::LimitReached);
        }
        tx.execute(
            "DELETE FROM member_api_keys WHERE user_id = ?1 AND revoked_at IS NOT NULL
               AND id NOT IN (SELECT id FROM member_api_keys
                               WHERE user_id = ?1 AND revoked_at IS NOT NULL
                               ORDER BY revoked_at DESC LIMIT ?2)",
            params![user_id, REVOKED_KEPT_PER_USER],
        )
        .map_err(store_error)?;
        tx.execute(
            "INSERT INTO member_api_keys(id, user_id, name, token_hash, created_at, expires_at, credential_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, user_id, name, hash, now, expires_at, verified_epoch],
        )
        .map_err(store_error)?;
        audit(
            &tx,
            user_id,
            "member_key.created",
            &id,
            serde_json::json!({ "owner": user_id }),
        )
        .map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(CreatedMemberKey {
            info: MemberKeyInfo {
                id,
                name,
                created_at: now,
                expires_at,
                last_used_at: None,
                revoked_at: None,
                active: true,
            },
            secret,
        })
    }

    /// The caller's own credentials, newest first.
    pub fn member_api_keys_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<MemberKeyInfo>, MemberKeyError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {INFO_COLUMNS} FROM member_api_keys t
                  WHERE t.user_id = ?1 ORDER BY t.created_at DESC, t.id"
            ))
            .map_err(store_error)?;
        let rows = stmt.query_map([user_id], info_from_row).map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    /// Every credential with its owner, for the Admin overview.
    pub fn member_api_keys_all(&self, admin_id: &str) -> Result<Vec<MemberKeyAdminEntry>, MemberKeyError> {
        let mut conn = self.conn();
        let conn = conn.transaction().map_err(store_error)?;
        require_admin(&conn, admin_id)?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {INFO_COLUMNS}, t.user_id, u.email, u.display_name
                   FROM member_api_keys t LEFT JOIN users u ON u.id = t.user_id
                  ORDER BY t.created_at DESC, t.id"
            ))
            .map_err(store_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(MemberKeyAdminEntry {
                    token: info_from_row(row)?,
                    user_id: row.get(7)?,
                    user_email: row.get(8)?,
                    user_display_name: row.get(9)?,
                })
            })
            .map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    /// Revoke one of `user_id`'s own credentials. Another account's
    /// credential reads as [`MemberKeyError::NotFound`]. Revoking an
    /// already-revoked credential succeeds without changing it.
    pub fn member_key_revoke_own(
        &self,
        user_id: &str,
        token_id: &str,
    ) -> Result<(), MemberKeyError> {
        self.revoke_member_key(user_id, token_id, Some(user_id))
    }

    /// Admin revocation of any account's credential.
    pub fn member_key_admin_revoke(
        &self,
        admin_id: &str,
        token_id: &str,
    ) -> Result<(), MemberKeyError> {
        self.revoke_member_key(admin_id, token_id, None)
    }

    fn revoke_member_key(
        &self,
        actor: &str,
        token_id: &str,
        owner: Option<&str>,
    ) -> Result<(), MemberKeyError> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        if owner.is_none() { require_admin(&tx, actor)?; }
        let row: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT user_id, revoked_at FROM member_api_keys
                  WHERE id = ?1 AND (?2 IS NULL OR user_id = ?2)",
                params![token_id, owner],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(store_error)?;
        let Some((token_owner, revoked_at)) = row else {
            return Err(MemberKeyError::NotFound);
        };
        if revoked_at.is_some() {
            return Ok(());
        }
        tx.execute(
            "UPDATE member_api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![token_id, Utc::now().to_rfc3339()],
        )
        .map_err(store_error)?;
        audit(
            &tx,
            actor,
            "member_key.revoked",
            token_id,
            serde_json::json!({ "owner": token_owner, "by_admin": owner.is_none() }),
        )
        .map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    /// Resolve a presented credential. Read-only: the caller decides (after
    /// its rate limit) whether to [`Self::member_key_touch`] it. A database
    /// error is an `Err`, which the caller must treat as a refusal.
    pub fn member_key_lookup(&self, raw: &str) -> Result<MemberKeyLookup, String> {
        if !well_formed(raw) {
            return Ok(MemberKeyLookup::Unknown);
        }
        let hash = digest(raw);
        let conn = self.conn();
        let row: Option<(String, String, String, bool, bool)> = conn
            .query_row(
                "SELECT t.id, t.user_id, t.token_hash, t.revoked_at IS NOT NULL,
                        COALESCE(u.status = 'active' AND u.must_change_password = 0
                          AND u.role IN ('employee','manager')
                          AND u.credential_epoch=t.credential_epoch
                          AND julianday(t.expires_at)>julianday('now'), 0)
                   FROM member_api_keys t LEFT JOIN users u ON u.id = t.user_id
                  WHERE t.token_hash = ?1",
                [&hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| format!("member key lookup: {e}"))?;
        let Some((token_id, user_id, stored, revoked, active)) = row else {
            return Ok(MemberKeyLookup::Unknown);
        };
        // The index lookup already matched; the constant-time comparison keeps
        // the final accept decision independent of any early-exit compare.
        if !bool::from(stored.as_bytes().ct_eq(hash.as_bytes())) {
            return Ok(MemberKeyLookup::Unknown);
        }
        if revoked {
            return Ok(MemberKeyLookup::Refused { token_id, user_id, reason: "revoked" });
        }
        if !active {
            return Ok(MemberKeyLookup::Refused {
                token_id,
                user_id,
                reason: "account_inactive",
            });
        }
        Ok(MemberKeyLookup::Valid { token_id, user_id })
    }

    /// Revalidate an already-open connection; never use an ACL cache for keys.
    pub fn member_key_context(&self, id: &str, user_id: &str) -> Result<crate::UserContext, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|_| "authentication service unavailable")?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM member_api_keys k JOIN users u ON u.id=k.user_id
             WHERE k.id=?1 AND k.user_id=?2 AND k.revoked_at IS NULL
             AND julianday(k.expires_at)>julianday('now') AND k.credential_epoch=u.credential_epoch
             AND u.status='active' AND u.must_change_password=0 AND u.role IN ('employee','manager'))",
            params![id, user_id], |r| r.get(0)).map_err(|_| "authentication service unavailable")?;
        if !valid { return Err("member key invalid".into()); }
        // Key validity, epoch, role and bindings come from the same snapshot.
        let (user, access) = crate::org::load_access_on(&tx, user_id)?.ok_or("member key invalid")?;
        let mut ctx = crate::UserContext::from_live(&user, &access);
        if ctx.is_admin() || ctx.must_change_password { return Err("member key invalid".into()); }
        ctx.member_key_id = Some(id.to_owned());
        Ok(ctx)
    }

    /// Write attempts carry only the key id and method, never params or secrets.
    pub fn member_key_record_write(&self, id: &str, user_id: &str, method: &str) -> Result<(), String> {
        audit(&self.conn(), user_id, "member_key.write", id, serde_json::json!({"method": method}))
            .map(|_| ()).map_err(|_| "member key audit unavailable".into())
    }

    /// Audit one attributable endpoint call, aggregated per (credential,
    /// `result`, [`MEMBER_AUDIT_WINDOW_SECS`] window) — see the module docs.
    /// When a window opens, `ok` also stamps `last_used_at` and one batch of
    /// expired endpoint audit rows is pruned. `now` is the call time (a
    /// parameter so retention and windows can be tested).
    pub fn member_key_record_call(
        &self,
        token_id: &str,
        user_id: &str,
        result: &str,
        employees: usize,
        ip: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<MemberCallRecord, String> {
        let window = now.timestamp().div_euclid(MEMBER_AUDIT_WINDOW_SECS) * MEMBER_AUDIT_WINDOW_SECS;
        let at = now.to_rfc3339();
        let employees = i64::try_from(employees).unwrap_or(i64::MAX);
        let err = |e: rusqlite::Error| format!("member key audit: {e}");
        let mut conn = self.conn();
        // Immediate: two pooled connections must not both open one window.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let counted = tx
            .execute(
                "UPDATE member_key_audit_agg
                    SET count = count + 1, last_seen = ?4, employees = ?5, last_ip = ?6
                  WHERE token_id = ?1 AND result = ?2 AND window_start = ?3",
                params![token_id, result, window, at, employees, ip],
            )
            .map_err(err)?;
        if counted > 0 {
            tx.commit().map_err(err)?;
            return Ok(MemberCallRecord::Counted);
        }
        let action = if result == "ok" { "member_key.used" } else { "member_key.refused" };
        let detail = serde_json::json!({
            "result": result,
            "employees": employees,
            "aggregated_secs": MEMBER_AUDIT_WINDOW_SECS,
        });
        tx.execute(
            "INSERT INTO auth_audit_log (user_id, action, target, detail, ip, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![user_id, action, token_id, detail.to_string(), ip, at],
        )
        .map_err(err)?;
        let audit_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO member_key_audit_agg
                 (token_id, result, window_start, user_id, audit_id, count, employees,
                  first_seen, last_seen, last_ip)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?7, ?8)",
            params![token_id, result, window, user_id, audit_id, employees, at, ip],
        )
        .map_err(err)?;
        if result == "ok" {
            tx.execute(
                "UPDATE member_api_keys SET last_used_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![token_id, at],
            )
            .map_err(err)?;
        }
        prune_member_audit(&tx, now, MEMBER_AUDIT_PRUNE_BATCH).map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(MemberCallRecord::Opened)
    }

    /// One prune pass (at most `batch` rows per table) of endpoint audit rows
    /// and aggregates older than [`MEMBER_AUDIT_RETENTION_DAYS`]. Returns the
    /// number of rows deleted; call again while it returns a full batch.
    pub fn member_key_audit_prune(&self, now: DateTime<Utc>, batch: i64) -> Result<usize, String> {
        prune_member_audit(&self.conn(), now, batch.max(1))
            .map_err(|e| format!("member key audit prune: {e}"))
    }

    /// Stamp `last_used_at` on an unrevoked credential.
    pub fn member_key_touch(&self, token_id: &str) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE member_api_keys SET last_used_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![token_id, Utc::now().to_rfc3339()],
            )
            .map(|_| ())
            .map_err(|e| format!("member key touch: {e}"))
    }
}


fn require_admin(conn: &Connection, id: &str) -> Result<(), MemberKeyError> {
    let allowed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1
        AND status='active' AND role='admin' AND must_change_password=0)", [id], |r| r.get(0)).map_err(store_error)?;
    if allowed { Ok(()) } else { Err(MemberKeyError::AccountInactive) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UserRole, UserStatus};

    fn fixture() -> (tempfile::TempDir, UserDb, crate::User) {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("member@test.invalid", "Member", "test-password", UserRole::Employee).unwrap();
        (home, db, user)
    }
    #[test]
    fn secret_is_random_redacted_and_never_listed_or_audited() {
        let (_home, db, user) = fixture();
        let a = db.member_key_create(&user.id, "服務", 90, user.credential_epoch).unwrap();
        let b = db.member_key_create(&user.id, "服務二", 90, user.credential_epoch).unwrap();
        assert_ne!(a.secret, b.secret);
        assert_eq!(URL_SAFE_NO_PAD.decode(a.secret.strip_prefix(MEMBER_KEY_PREFIX).unwrap()).unwrap().len(), 32);
        assert!(!format!("{a:?}").contains(&a.secret));
        let stored: String = db.conn().query_row("SELECT token_hash FROM member_api_keys WHERE id=?1", [&a.info.id], |r| r.get(0)).unwrap();
        assert_eq!(stored, digest(&a.secret));
        let listed = serde_json::to_string(&db.member_api_keys_for_user(&user.id).unwrap()).unwrap();
        assert!(!listed.contains(&a.secret) && !listed.contains(&stored) && !listed.contains("token_hash"));
        let leaks: i64 = db.conn().query_row("SELECT COUNT(*) FROM auth_audit_log WHERE detail LIKE ?1 OR target LIKE ?1", [format!("%{}%", a.secret)], |r| r.get(0)).unwrap();
        assert_eq!(leaks, 0);
    }
    #[test]
    fn expiry_cap_admin_and_owner_gates() {
        let (_home, db, user) = fixture();
        for days in [0, 366, u64::MAX] {
            assert_eq!(db.member_key_create(&user.id, "x", days, 0).unwrap_err(), MemberKeyError::InvalidExpiry);
        }
        for name in ["", "\n", "bad\nname"] {
            assert_eq!(db.member_key_create(&user.id, name, 1, 0).unwrap_err(), MemberKeyError::InvalidName);
        }
        let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
        assert_eq!(db.member_key_create(&admin.id, "x", 1, 0).unwrap_err(), MemberKeyError::AdminForbidden);
        let first = db.member_key_create(&user.id, "first", 1, 0).unwrap();
        for _ in 1..5 { db.member_key_create(&user.id, "x", 365, 0).unwrap(); }
        assert_eq!(db.member_key_create(&user.id, "six", 90, 0).unwrap_err(), MemberKeyError::LimitReached);
        assert!(db.member_api_keys_all(&user.id).is_err());
        assert!(db.member_key_admin_revoke(&user.id, &first.info.id).is_err());
        assert!(db.member_key_revoke_own(&admin.id, &first.info.id).is_err());
        assert_eq!(db.member_api_keys_all(&admin.id).unwrap().len(), 5);
        db.member_key_admin_revoke(&admin.id, &first.info.id).unwrap();
        db.member_key_create(&user.id, "replacement", 1, 0).unwrap();
    }
    #[test]
    fn new_and_open_credentials_refuse_all_revocation_causes() {
        for cause in ["revoke", "expire", "epoch", "password", "admin", "offboard", "suspend", "delete"] {
            let (_home, db, user) = fixture();
            let created = db.member_key_create(&user.id, "x", 1, 0).unwrap();
            let before = db.member_key_context(&created.info.id, &user.id).unwrap();
            assert_eq!(before.member_key_id.as_deref(), Some(created.info.id.as_str()));
            match cause {
                "revoke" => db.member_key_revoke_own(&user.id, &created.info.id).unwrap(),
                "expire" => { db.conn().execute("UPDATE member_api_keys SET expires_at='2000-01-01T00:00:00Z'", []).unwrap(); },
                "epoch" => { db.mfa_operator_reset(&user.email).unwrap(); },
                "password" => { db.conn().execute("UPDATE users SET password_hash='changed-test-hash' WHERE id=?1", [&user.id]).unwrap(); },
                "admin" => { db.conn().execute("UPDATE users SET role='admin' WHERE id=?1", [&user.id]).unwrap(); },
                "offboard" => {
                    db.set_user_status(&user.id, UserStatus::Offboarded).unwrap();
                    db.set_user_status(&user.id, UserStatus::Active).unwrap();
                    assert!(db.member_api_keys_for_user(&user.id).unwrap()[0].revoked_at.is_some());
                }
                "suspend" => db.set_user_status(&user.id, UserStatus::Suspended).unwrap(),
                "delete" => { db.conn().execute("DELETE FROM users WHERE id=?1", [&user.id]).unwrap(); },
                _ => unreachable!(),
            }
            assert!(db.member_key_context(&created.info.id, &user.id).is_err(), "{cause}");
            assert!(!matches!(db.member_key_lookup(&created.secret).unwrap(), MemberKeyLookup::Valid { .. }), "{cause}");
        }
    }
    #[test]
    fn token_types_do_not_cross_authenticate() {
        let (_home, db, user) = fixture();
        let member = db.member_key_create(&user.id, "x", 1, 0).unwrap();
        let status = db.status_token_create(&user.id, "x").unwrap();
        let jwt = crate::JwtConfig::new(b"member-key-tests-secret-at-least-32-bytes").issue_access_token(&user, &[]).unwrap();
        for raw in [status.secret.as_str(), jwt.as_str(), "ddc_prod_0123456789abcdef0123456789abcdef", "ddc_member_", "ddc_member_字"] {
            assert_eq!(db.member_key_lookup(raw).unwrap(), MemberKeyLookup::Unknown);
        }
        assert_eq!(db.status_token_lookup(&member.secret).unwrap(), crate::status_tokens::StatusTokenLookup::Unknown);
    }
    #[test]
    fn use_is_aggregated_but_each_write_is_audited() {
        let (_home, db, user) = fixture();
        let key = db.member_key_create(&user.id, "x", 1, 0).unwrap();
        let now = Utc::now();
        for _ in 0..120 {
            db.member_key_record_call(&key.info.id, &user.id, "ok", 0, None, now).unwrap();
        }
        for _ in 0..2 { db.member_key_record_write(&key.info.id, &user.id, "tasks.create").unwrap(); }
        let conn = db.conn();
        let calls: i64 = conn.query_row("SELECT count FROM member_key_audit_agg WHERE token_id=?1", [&key.info.id], |r| r.get(0)).unwrap();
        let reads: i64 = conn.query_row("SELECT COUNT(*) FROM auth_audit_log WHERE action='member_key.used'", [], |r| r.get(0)).unwrap();
        let writes: i64 = conn.query_row("SELECT COUNT(*) FROM auth_audit_log WHERE action='member_key.write'", [], |r| r.get(0)).unwrap();
        assert_eq!((calls, reads, writes), (120, 1, 2));
        drop(conn);
        assert_eq!(db.member_api_keys_for_user(&user.id).unwrap()[0].last_used_at, Some(now.to_rfc3339()));
    }
}
