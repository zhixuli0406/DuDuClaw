//! RM-064 read-only work-status credentials (`ddc_status_…`).
//!
//! A staff member creates these on their own account page so an external
//! business system (for example an Odoo "my AI assistant" card) can read the
//! work status of the AI employees bound to *them*. They are deliberately a
//! separate credential type:
//!
//! - not an `[mcp_keys]` entry and not a dashboard JWT — the gateway's only
//!   consumer is `GET /api/v1/status/employees`, and every other bearer surface
//!   (dashboard handshake, `/api/*`, the MCP HTTP server) refuses the prefix;
//! - tied to one dashboard account; which employees are visible is resolved
//!   from that account's live bindings on every request, so an unbinding, a
//!   suspension or a deletion takes effect on the next call;
//! - stored only as a SHA-256 digest. The bearer secret (32 random bytes,
//!   base64url) is returned exactly once, by [`UserDb::status_token_create`],
//!   in a type with no `Debug` / `Serialize` implementation.
//!
//! Revocation is a column write and every request re-reads the row, so it is
//! effective immediately. Turning the feature off (`[external_status]`) does
//! not touch this table; the endpoint simply refuses every request.
//!
//! ## Endpoint audit
//!
//! An integration polling every second must not write a row per call. Calls
//! are aggregated per (credential, result, [`STATUS_AUDIT_WINDOW_SECS`]
//! window) in `status_token_audit_agg`: the first call of a window writes
//! one `auth_audit_log` row (`status_token.used` / `status_token.refused`)
//! and opens the aggregate; later calls in the window only bump its `count`
//! / `last_seen`. `last_used_at` moves only when an `ok` window opens. Both
//! the endpoint's audit-log rows and the aggregates are kept
//! [`STATUS_AUDIT_RETENTION_DAYS`] days, pruned in batches of
//! [`STATUS_AUDIT_PRUNE_BATCH`] each time a window opens.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::UserDb;

/// Every status credential starts with this. Other bearer surfaces refuse
/// anything carrying it ([`is_status_token`]).
pub const STATUS_TOKEN_PREFIX: &str = "ddc_status_";
/// Active (unrevoked) credentials one account may hold at once.
pub const MAX_ACTIVE_STATUS_TOKENS: i64 = 5;
/// Longest label a staff member may give a credential.
pub const STATUS_TOKEN_NAME_MAX_CHARS: usize = 60;
/// Random bytes behind each credential.
const SECRET_BYTES: usize = 32;
/// `base64url(32 bytes)` without padding.
const SECRET_CHARS: usize = 43;
/// Revoked rows kept per account for the history list; older ones are pruned
/// when a new credential is created, so the table cannot grow without bound.
const REVOKED_KEPT_PER_USER: i64 = 20;
/// Width of one endpoint-audit aggregation window.
pub const STATUS_AUDIT_WINDOW_SECS: i64 = 600;
/// Endpoint audit-log rows and aggregates older than this are pruned.
pub const STATUS_AUDIT_RETENTION_DAYS: i64 = 90;
/// Rows deleted per table by one prune pass.
pub const STATUS_AUDIT_PRUNE_BATCH: i64 = 500;

/// Idempotent `status_tokens` migration, invoked by `UserDb::init_tables`.
///
/// Offboarding an account revokes its credentials permanently (the trigger);
/// a *suspended* account's credentials are refused at request time instead,
/// so a reinstated account does not have to re-pair every external system.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS status_token_audit_agg (
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
         CREATE INDEX IF NOT EXISTS idx_status_audit_agg_window
             ON status_token_audit_agg(window_start);",
    )
    .map_err(|e| format!("status_token_audit_agg migration failed: {e}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS status_tokens (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            token_hash TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            last_used_at TEXT,
            revoked_at TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_status_tokens_user ON status_tokens(user_id);
         CREATE TRIGGER IF NOT EXISTS status_tokens_revoke_offboarded
         AFTER UPDATE OF status ON users
         WHEN NEW.status = 'offboarded' AND OLD.status != NEW.status
         BEGIN
             UPDATE status_tokens SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE user_id = NEW.id AND revoked_at IS NULL;
         END;",
    )
    .map_err(|e| format!("status_tokens migration failed: {e}"))
}

/// Does `raw` claim to be a status credential? Prefix only, on purpose: every
/// non-status bearer surface uses this to refuse the whole credential type,
/// well-formed or not.
pub fn is_status_token(raw: &str) -> bool {
    raw.trim().starts_with(STATUS_TOKEN_PREFIX)
}

/// Exact shape of a credential this module issued.
fn well_formed(raw: &str) -> bool {
    raw.strip_prefix(STATUS_TOKEN_PREFIX).is_some_and(|secret| {
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

fn clean_name(raw: &str) -> Result<String, StatusTokenError> {
    let name = raw.trim();
    if name.is_empty()
        || name.chars().count() > STATUS_TOKEN_NAME_MAX_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(StatusTokenError::InvalidName);
    }
    Ok(name.to_string())
}

/// One credential as listed to its owner. Never carries the secret or digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusTokenInfo {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    pub active: bool,
}

/// One credential as listed to an Admin, with its owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusTokenAdminEntry {
    #[serde(flatten)]
    pub token: StatusTokenInfo,
    pub user_id: String,
    /// `None` when the owner row no longer exists.
    pub user_email: Option<String>,
    pub user_display_name: Option<String>,
}

/// A freshly created credential. Deliberately no `Debug` / `Serialize`:
/// `secret` is the bearer credential and is shown to its owner exactly once.
pub struct CreatedStatusToken {
    pub info: StatusTokenInfo,
    pub secret: String,
}

/// Why a create / revoke was refused. Store details go to the log only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTokenError {
    InvalidName,
    LimitReached,
    AccountInactive,
    NotFound,
    Store,
}

impl StatusTokenError {
    /// Stable machine-readable code for the RPC error frame.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidName => "status_token_invalid_name",
            Self::LimitReached => "status_token_limit_reached",
            Self::AccountInactive => "status_token_account_inactive",
            Self::NotFound => "status_token_not_found",
            Self::Store => "status_token_unavailable",
        }
    }
}

/// What a presented credential resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusTokenLookup {
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

fn store_error(e: impl std::fmt::Display) -> StatusTokenError {
    tracing::warn!(error = %e, "status token store error");
    StatusTokenError::Store
}

fn info_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StatusTokenInfo> {
    let revoked_at: Option<String> = row.get(4)?;
    Ok(StatusTokenInfo {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        last_used_at: row.get(3)?,
        active: revoked_at.is_none(),
        revoked_at,
    })
}

const INFO_COLUMNS: &str = "t.id, t.name, t.created_at, t.last_used_at, t.revoked_at";

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

/// What [`UserDb::status_token_record_call`] did with one endpoint call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCallRecord {
    /// First call of this (credential, result) in the window: one audit-log
    /// row written, the aggregate opened, `last_used_at` stamped for `ok`.
    Opened,
    /// Counted into the window's open aggregate; nothing else written.
    Counted,
}

/// Delete at most `batch` expired endpoint audit-log rows and at most
/// `batch` expired aggregates. Other audit actions are never touched.
fn prune_status_audit(conn: &Connection, now: DateTime<Utc>, batch: i64) -> rusqlite::Result<usize> {
    let cutoff = now - chrono::Duration::days(STATUS_AUDIT_RETENTION_DAYS);
    let aggregates = conn.execute(
        "DELETE FROM status_token_audit_agg WHERE rowid IN (
             SELECT rowid FROM status_token_audit_agg WHERE window_start < ?1 LIMIT ?2)",
        params![cutoff.timestamp(), batch],
    )?;
    let rows = conn.execute(
        "DELETE FROM auth_audit_log WHERE id IN (
             SELECT id FROM auth_audit_log
              WHERE action IN ('status_token.used', 'status_token.refused')
                AND timestamp < ?1
              ORDER BY timestamp LIMIT ?2)",
        params![cutoff.to_rfc3339(), batch],
    )?;
    Ok(aggregates + rows)
}

impl UserDb {
    /// Issue a credential for `user_id` (who must be an active account not
    /// awaiting a password change). At most [`MAX_ACTIVE_STATUS_TOKENS`]
    /// unrevoked credentials per account. The secret is returned once and
    /// only its digest is stored.
    pub fn status_token_create(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<CreatedStatusToken, StatusTokenError> {
        let name = clean_name(name)?;
        let mut bytes = [0u8; SECRET_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| StatusTokenError::Store)?;
        let secret = format!("{STATUS_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        let hash = digest(&secret);
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM users
                   WHERE id = ?1 AND status = 'active' AND must_change_password = 0)",
                [user_id],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        if !active {
            return Err(StatusTokenError::AccountInactive);
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM status_tokens WHERE user_id = ?1 AND revoked_at IS NULL",
                [user_id],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        if count >= MAX_ACTIVE_STATUS_TOKENS {
            return Err(StatusTokenError::LimitReached);
        }
        tx.execute(
            "DELETE FROM status_tokens WHERE user_id = ?1 AND revoked_at IS NOT NULL
               AND id NOT IN (SELECT id FROM status_tokens
                               WHERE user_id = ?1 AND revoked_at IS NOT NULL
                               ORDER BY revoked_at DESC LIMIT ?2)",
            params![user_id, REVOKED_KEPT_PER_USER],
        )
        .map_err(store_error)?;
        tx.execute(
            "INSERT INTO status_tokens(id, user_id, name, token_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, user_id, name, hash, now],
        )
        .map_err(store_error)?;
        audit(
            &tx,
            user_id,
            "status_token.created",
            &id,
            serde_json::json!({ "owner": user_id }),
        )
        .map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(CreatedStatusToken {
            info: StatusTokenInfo {
                id,
                name,
                created_at: now,
                last_used_at: None,
                revoked_at: None,
                active: true,
            },
            secret,
        })
    }

    /// The caller's own credentials, newest first.
    pub fn status_tokens_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<StatusTokenInfo>, StatusTokenError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {INFO_COLUMNS} FROM status_tokens t
                  WHERE t.user_id = ?1 ORDER BY t.created_at DESC, t.id"
            ))
            .map_err(store_error)?;
        let rows = stmt.query_map([user_id], info_from_row).map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    /// Every credential with its owner, for the Admin overview.
    pub fn status_tokens_all(&self) -> Result<Vec<StatusTokenAdminEntry>, StatusTokenError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {INFO_COLUMNS}, t.user_id, u.email, u.display_name
                   FROM status_tokens t LEFT JOIN users u ON u.id = t.user_id
                  ORDER BY t.created_at DESC, t.id"
            ))
            .map_err(store_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(StatusTokenAdminEntry {
                    token: info_from_row(row)?,
                    user_id: row.get(5)?,
                    user_email: row.get(6)?,
                    user_display_name: row.get(7)?,
                })
            })
            .map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    /// Revoke one of `user_id`'s own credentials. Another account's
    /// credential reads as [`StatusTokenError::NotFound`]. Revoking an
    /// already-revoked credential succeeds without changing it.
    pub fn status_token_revoke_own(
        &self,
        user_id: &str,
        token_id: &str,
    ) -> Result<(), StatusTokenError> {
        self.revoke_status_token(user_id, token_id, Some(user_id))
    }

    /// Admin revocation of any account's credential.
    pub fn status_token_admin_revoke(
        &self,
        admin_id: &str,
        token_id: &str,
    ) -> Result<(), StatusTokenError> {
        self.revoke_status_token(admin_id, token_id, None)
    }

    fn revoke_status_token(
        &self,
        actor: &str,
        token_id: &str,
        owner: Option<&str>,
    ) -> Result<(), StatusTokenError> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT user_id, revoked_at FROM status_tokens
                  WHERE id = ?1 AND (?2 IS NULL OR user_id = ?2)",
                params![token_id, owner],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(store_error)?;
        let Some((token_owner, revoked_at)) = row else {
            return Err(StatusTokenError::NotFound);
        };
        if revoked_at.is_some() {
            return Ok(());
        }
        tx.execute(
            "UPDATE status_tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![token_id, Utc::now().to_rfc3339()],
        )
        .map_err(store_error)?;
        audit(
            &tx,
            actor,
            "status_token.revoked",
            token_id,
            serde_json::json!({ "owner": token_owner, "by_admin": owner.is_none() }),
        )
        .map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    /// Resolve a presented credential. Read-only: the caller decides (after
    /// its rate limit) whether to [`Self::status_token_touch`] it. A database
    /// error is an `Err`, which the caller must treat as a refusal.
    pub fn status_token_lookup(&self, raw: &str) -> Result<StatusTokenLookup, String> {
        if !well_formed(raw) {
            return Ok(StatusTokenLookup::Unknown);
        }
        let hash = digest(raw);
        let conn = self.conn();
        let row: Option<(String, String, String, bool, bool)> = conn
            .query_row(
                "SELECT t.id, t.user_id, t.token_hash, t.revoked_at IS NOT NULL,
                        COALESCE(u.status = 'active' AND u.must_change_password = 0, 0)
                   FROM status_tokens t LEFT JOIN users u ON u.id = t.user_id
                  WHERE t.token_hash = ?1",
                [&hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| format!("status token lookup: {e}"))?;
        let Some((token_id, user_id, stored, revoked, active)) = row else {
            return Ok(StatusTokenLookup::Unknown);
        };
        // The index lookup already matched; the constant-time comparison keeps
        // the final accept decision independent of any early-exit compare.
        if !bool::from(stored.as_bytes().ct_eq(hash.as_bytes())) {
            return Ok(StatusTokenLookup::Unknown);
        }
        if revoked {
            return Ok(StatusTokenLookup::Refused { token_id, user_id, reason: "revoked" });
        }
        if !active {
            return Ok(StatusTokenLookup::Refused {
                token_id,
                user_id,
                reason: "account_inactive",
            });
        }
        Ok(StatusTokenLookup::Valid { token_id, user_id })
    }

    /// Audit one attributable endpoint call, aggregated per (credential,
    /// `result`, [`STATUS_AUDIT_WINDOW_SECS`] window) — see the module docs.
    /// When a window opens, `ok` also stamps `last_used_at` and one batch of
    /// expired endpoint audit rows is pruned. `now` is the call time (a
    /// parameter so retention and windows can be tested).
    pub fn status_token_record_call(
        &self,
        token_id: &str,
        user_id: &str,
        result: &str,
        employees: usize,
        ip: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<StatusCallRecord, String> {
        let window = now.timestamp().div_euclid(STATUS_AUDIT_WINDOW_SECS) * STATUS_AUDIT_WINDOW_SECS;
        let at = now.to_rfc3339();
        let employees = i64::try_from(employees).unwrap_or(i64::MAX);
        let err = |e: rusqlite::Error| format!("status token audit: {e}");
        let mut conn = self.conn();
        // Immediate: two pooled connections must not both open one window.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let counted = tx
            .execute(
                "UPDATE status_token_audit_agg
                    SET count = count + 1, last_seen = ?4, employees = ?5, last_ip = ?6
                  WHERE token_id = ?1 AND result = ?2 AND window_start = ?3",
                params![token_id, result, window, at, employees, ip],
            )
            .map_err(err)?;
        if counted > 0 {
            tx.commit().map_err(err)?;
            return Ok(StatusCallRecord::Counted);
        }
        let action = if result == "ok" { "status_token.used" } else { "status_token.refused" };
        let detail = serde_json::json!({
            "result": result,
            "employees": employees,
            "aggregated_secs": STATUS_AUDIT_WINDOW_SECS,
        });
        tx.execute(
            "INSERT INTO auth_audit_log (user_id, action, target, detail, ip, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![user_id, action, token_id, detail.to_string(), ip, at],
        )
        .map_err(err)?;
        let audit_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO status_token_audit_agg
                 (token_id, result, window_start, user_id, audit_id, count, employees,
                  first_seen, last_seen, last_ip)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?7, ?8)",
            params![token_id, result, window, user_id, audit_id, employees, at, ip],
        )
        .map_err(err)?;
        if result == "ok" {
            tx.execute(
                "UPDATE status_tokens SET last_used_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![token_id, at],
            )
            .map_err(err)?;
        }
        prune_status_audit(&tx, now, STATUS_AUDIT_PRUNE_BATCH).map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(StatusCallRecord::Opened)
    }

    /// One prune pass (at most `batch` rows per table) of endpoint audit rows
    /// and aggregates older than [`STATUS_AUDIT_RETENTION_DAYS`]. Returns the
    /// number of rows deleted; call again while it returns a full batch.
    pub fn status_token_audit_prune(&self, now: DateTime<Utc>, batch: i64) -> Result<usize, String> {
        prune_status_audit(&self.conn(), now, batch.max(1))
            .map_err(|e| format!("status token audit prune: {e}"))
    }

    /// Stamp `last_used_at` on an unrevoked credential.
    pub fn status_token_touch(&self, token_id: &str) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE status_tokens SET last_used_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![token_id, Utc::now().to_rfc3339()],
            )
            .map(|_| ())
            .map_err(|e| format!("status token touch: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{UserRole, UserStatus};
    use tempfile::NamedTempFile;

    fn db() -> (UserDb, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        (UserDb::new(tmp.path()).unwrap(), tmp)
    }

    fn user(db: &UserDb, name: &str) -> String {
        db.create_user(&format!("{name}@test.invalid"), name, "isolated-password", UserRole::Employee)
            .unwrap()
            .id
    }

    #[test]
    fn created_secret_is_shown_once_and_only_its_digest_is_stored() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let created = db.status_token_create(&alice, "  Odoo 卡片 ").unwrap();
        assert!(created.secret.starts_with(STATUS_TOKEN_PREFIX));
        assert!(well_formed(&created.secret), "{}", created.secret.len());
        assert_eq!(created.info.name, "Odoo 卡片");
        assert!(created.info.active);

        // Neither listing returns the secret or its digest.
        let mine = db.status_tokens_for_user(&alice).unwrap();
        assert_eq!(mine.len(), 1);
        let listed = serde_json::to_string(&mine).unwrap()
            + &serde_json::to_string(&db.status_tokens_all().unwrap()).unwrap();
        assert!(!listed.contains(&created.secret));
        assert!(!listed.contains(&digest(&created.secret)));
        assert!(!listed.contains("token_hash"));

        // The database holds the digest, never the secret.
        let conn = db.conn();
        let stored: String = conn
            .query_row("SELECT token_hash FROM status_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, digest(&created.secret));
        let dump: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM status_tokens WHERE id LIKE ?1 OR name LIKE ?1 OR token_hash LIKE ?1",
                [format!("%{}%", &created.secret[STATUS_TOKEN_PREFIX.len()..])],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dump, 0);
        let audit_leak: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM auth_audit_log WHERE detail LIKE ?1 OR target LIKE ?1",
                [format!("%{}%", &created.secret[STATUS_TOKEN_PREFIX.len()..])],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audit_leak, 0);
    }

    #[test]
    fn lookup_revoke_is_immediate_and_owner_scoped() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let bob = user(&db, "bob");
        let created = db.status_token_create(&alice, "odoo").unwrap();
        let id = created.info.id.clone();
        assert_eq!(
            db.status_token_lookup(&created.secret).unwrap(),
            StatusTokenLookup::Valid { token_id: id.clone(), user_id: alice.clone() }
        );

        // Bob can neither see nor revoke Alice's credential.
        assert!(db.status_tokens_for_user(&bob).unwrap().is_empty());
        assert_eq!(db.status_token_revoke_own(&bob, &id), Err(StatusTokenError::NotFound));
        assert!(matches!(db.status_token_lookup(&created.secret).unwrap(), StatusTokenLookup::Valid { .. }));

        db.status_token_revoke_own(&alice, &id).unwrap();
        assert_eq!(
            db.status_token_lookup(&created.secret).unwrap(),
            StatusTokenLookup::Refused { token_id: id.clone(), user_id: alice.clone(), reason: "revoked" }
        );
        // Idempotent; touching a revoked row changes nothing.
        db.status_token_revoke_own(&alice, &id).unwrap();
        db.status_token_touch(&id).unwrap();
        let listed = db.status_tokens_for_user(&alice).unwrap();
        assert!(!listed[0].active && listed[0].last_used_at.is_none());
    }

    #[test]
    fn admin_revokes_any_and_lists_owners() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let admin = db
            .create_user("admin@test.invalid", "Admin", "isolated-password", UserRole::Admin)
            .unwrap()
            .id;
        let created = db.status_token_create(&alice, "odoo").unwrap();
        let all = db.status_tokens_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].user_id, alice);
        assert_eq!(all[0].user_email.as_deref(), Some("alice@test.invalid"));
        db.status_token_admin_revoke(&admin, &created.info.id).unwrap();
        assert!(matches!(
            db.status_token_lookup(&created.secret).unwrap(),
            StatusTokenLookup::Refused { reason: "revoked", .. }
        ));
        assert_eq!(db.status_token_admin_revoke(&admin, "missing"), Err(StatusTokenError::NotFound));
    }

    #[test]
    fn at_most_five_active_per_account() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let ids: Vec<String> = (0..5)
            .map(|i| db.status_token_create(&alice, &format!("t{i}")).unwrap().info.id)
            .collect();
        assert_eq!(
            db.status_token_create(&alice, "sixth").err(),
            Some(StatusTokenError::LimitReached)
        );
        db.status_token_revoke_own(&alice, &ids[0]).unwrap();
        db.status_token_create(&alice, "sixth").unwrap();
    }

    #[test]
    fn names_are_validated() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let too_long = "字".repeat(STATUS_TOKEN_NAME_MAX_CHARS + 1);
        for bad in ["", "   ", "a\nb", too_long.as_str()] {
            assert_eq!(db.status_token_create(&alice, bad).err(), Some(StatusTokenError::InvalidName), "{bad:?}");
        }
        db.status_token_create(&alice, &"字".repeat(STATUS_TOKEN_NAME_MAX_CHARS)).unwrap();
    }

    #[test]
    fn inactive_or_deleted_owner_is_refused() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let created = db.status_token_create(&alice, "odoo").unwrap();

        db.set_user_status(&alice, UserStatus::Suspended).unwrap();
        assert!(matches!(
            db.status_token_lookup(&created.secret).unwrap(),
            StatusTokenLookup::Refused { reason: "account_inactive", .. }
        ));
        assert_eq!(db.status_token_create(&alice, "x").err(), Some(StatusTokenError::AccountInactive));
        db.set_user_status(&alice, UserStatus::Active).unwrap();
        assert!(matches!(db.status_token_lookup(&created.secret).unwrap(), StatusTokenLookup::Valid { .. }));

        // Offboarding revokes permanently.
        db.set_user_status(&alice, UserStatus::Offboarded).unwrap();
        db.set_user_status(&alice, UserStatus::Active).unwrap();
        assert!(matches!(
            db.status_token_lookup(&created.secret).unwrap(),
            StatusTokenLookup::Refused { reason: "revoked", .. }
        ));

        // A deleted owner is refused even without foreign-key enforcement.
        let bob = user(&db, "bob");
        let bobs = db.status_token_create(&bob, "odoo").unwrap();
        db.conn().execute("DELETE FROM users WHERE id = ?1", [&bob]).unwrap();
        assert!(!matches!(db.status_token_lookup(&bobs.secret).unwrap(), StatusTokenLookup::Valid { .. }));

        // An unknown account cannot mint credentials.
        assert_eq!(db.status_token_create("system", "x").err(), Some(StatusTokenError::AccountInactive));
    }

    fn count(db: &UserDb, sql: &str, token_id: &str) -> i64 {
        db.conn().query_row(sql, [token_id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn sustained_polling_keeps_the_audit_bounded() {
        use chrono::TimeZone;
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let id = db.status_token_create(&alice, "odoo").unwrap().info.id;
        // Window-aligned start; 60 calls a minute for a simulated hour.
        let t0 = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        assert_eq!(t0.timestamp() % STATUS_AUDIT_WINDOW_SECS, 0);
        let mut opened = 0;
        for second in 0..3600 {
            let at = t0 + chrono::Duration::seconds(second);
            let record = db.status_token_record_call(&id, &alice, "ok", 2, Some("203.0.113.7"), at).unwrap();
            if record == StatusCallRecord::Opened {
                opened += 1;
            }
        }
        assert_eq!(opened, 6, "one window per 10 minutes");
        let used = count(&db, "SELECT COUNT(*) FROM auth_audit_log WHERE action = 'status_token.used' AND target = ?1", &id);
        assert_eq!(used, 6, "audit-log rows bounded by windows, not calls");
        let aggregates = count(&db, "SELECT COUNT(*) FROM status_token_audit_agg WHERE token_id = ?1", &id);
        assert_eq!(aggregates, 6);
        let calls = count(&db, "SELECT SUM(count) FROM status_token_audit_agg WHERE token_id = ?1", &id);
        assert_eq!(calls, 3600, "every call is still counted");
        // last_used_at moved once per window: it holds the last window's
        // opening call, not the last call.
        let listed = db.status_tokens_for_user(&alice).unwrap();
        assert_eq!(
            listed[0].last_used_at.as_deref(),
            Some((t0 + chrono::Duration::minutes(50)).to_rfc3339().as_str())
        );
        let last_seen: String = db
            .conn()
            .query_row(
                "SELECT last_seen FROM status_token_audit_agg WHERE token_id = ?1 ORDER BY window_start DESC LIMIT 1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last_seen, (t0 + chrono::Duration::seconds(3599)).to_rfc3339());

        // Another result opens its own window; a refusal never moves last_used_at.
        let late = t0 + chrono::Duration::seconds(3599);
        assert_eq!(db.status_token_record_call(&id, &alice, "agent_not_visible", 0, None, late).unwrap(), StatusCallRecord::Opened);
        assert_eq!(db.status_token_record_call(&id, &alice, "agent_not_visible", 0, None, late).unwrap(), StatusCallRecord::Counted);
        let refused = count(&db, "SELECT COUNT(*) FROM auth_audit_log WHERE action = 'status_token.refused' AND target = ?1", &id);
        assert_eq!(refused, 1);
        assert_eq!(db.status_tokens_for_user(&alice).unwrap()[0].last_used_at, listed[0].last_used_at);
    }

    #[test]
    fn endpoint_audit_expires_after_the_retention_window_in_batches() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let id = db.status_token_create(&alice, "odoo").unwrap().info.id;
        let now = Utc::now();
        let old = now - chrono::Duration::days(STATUS_AUDIT_RETENTION_DAYS + 1);
        for i in 0..3 {
            let at = old + chrono::Duration::seconds(i * STATUS_AUDIT_WINDOW_SECS);
            db.status_token_record_call(&id, &alice, "ok", 1, None, at).unwrap();
        }
        // An old row of another kind is not this endpoint's to prune.
        db.conn()
            .execute(
                "INSERT INTO auth_audit_log (user_id, action, target, timestamp) VALUES (?1, 'user.login', NULL, ?2)",
                params![alice, old.to_rfc3339()],
            )
            .unwrap();
        let endpoint = |db: &UserDb| {
            count(db, "SELECT COUNT(*) FROM auth_audit_log WHERE action = 'status_token.used' AND target = ?1", &id)
                + count(db, "SELECT COUNT(*) FROM status_token_audit_agg WHERE token_id = ?1", &id)
        };
        assert_eq!(endpoint(&db), 6);
        assert_eq!(db.status_token_audit_prune(now, 2).unwrap(), 4, "two per table");
        assert_eq!(endpoint(&db), 2);
        assert_eq!(db.status_token_audit_prune(now, 2).unwrap(), 2);
        assert_eq!(db.status_token_audit_prune(now, 2).unwrap(), 0);
        assert_eq!(endpoint(&db), 0);
        let others: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM auth_audit_log WHERE action IN ('user.login', 'status_token.created')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(others, 2, "other audit actions are kept");

        // Opening a window prunes in passing; recent rows stay.
        db.status_token_record_call(&id, &alice, "ok", 1, None, old).unwrap();
        assert_eq!(endpoint(&db), 2);
        db.status_token_record_call(&id, &alice, "ok", 1, None, now).unwrap();
        assert_eq!(endpoint(&db), 2, "the old window went, the new one stays");
        let recent = count(&db, "SELECT COUNT(*) FROM status_token_audit_agg WHERE token_id = ?1 AND count = 1", &id);
        assert_eq!(recent, 1);
    }

    #[test]
    fn malformed_and_foreign_credentials_are_unknown() {
        let (db, _tmp) = db();
        let alice = user(&db, "alice");
        let created = db.status_token_create(&alice, "odoo").unwrap();
        let mut tampered = created.secret.clone();
        tampered.pop();
        tampered.push(if created.secret.ends_with('A') { 'B' } else { 'A' });
        for raw in [
            "".to_string(),
            "ddc_status_".to_string(),
            "ddc_prod_0123456789abcdef0123456789abcdef".to_string(),
            format!("{}x", created.secret),
            created.secret.replace(STATUS_TOKEN_PREFIX, "ddc_statuz_"),
            tampered,
            format!(" {}", created.secret),
        ] {
            assert_eq!(db.status_token_lookup(&raw).unwrap(), StatusTokenLookup::Unknown, "{raw:?}");
        }
        assert!(is_status_token(&created.secret));
        assert!(is_status_token("ddc_status_anything"));
        assert!(!is_status_token("ddc_prod_0123"));
    }
}
