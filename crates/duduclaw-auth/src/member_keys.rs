//! RM-213 personal member keys. Only SHA-256 digests are persisted.
//! All authentication paths revalidate the row and live member identity.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use password_hash::rand_core::{OsRng, RngCore};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
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
/// Total retained rows, including revoked credentials.
pub const MAX_MEMBER_KEY_ROWS: i64 = 50;
pub const MEMBER_KEY_RETENTION_DAYS: i64 = 90;
const CREATE_METHOD: &str = "me.member_keys.create";
const REAUTH_TTL_SECS: i64 = 300;
/// Width of one endpoint-audit aggregation window.
pub const MEMBER_AUDIT_WINDOW_SECS: i64 = 600;
/// Endpoint audit-log rows and aggregates older than this are pruned.
pub const MEMBER_AUDIT_RETENTION_DAYS: i64 = 90;
/// Rows deleted per table by one prune pass.
pub const MEMBER_AUDIT_PRUNE_BATCH: i64 = 500;

/// Key identity, ACL generation and expiry from one validated read snapshot.
pub struct MemberKeyAuthority {
    pub context: crate::UserContext,
    /// Unix seconds, rounded down so a grant never outlives the key.
    pub expires_at: u64,
    pub include_conversations: bool,
    pub mcp_scopes: Vec<String>,
}

/// Idempotent `member_api_keys` migration, invoked by `UserDb::init_tables`.
///
/// Offboarding an account revokes its credentials permanently (the trigger);
/// a *suspended* account's credentials are refused at request time instead,
/// so a reinstated account does not have to re-pair every external system.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let conn = &tx;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS auth_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
        INSERT OR IGNORE INTO auth_meta(key,value) VALUES('acl_generation',1);")
        .map_err(|e| e.to_string())?;
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
        "CREATE TABLE IF NOT EXISTS member_key_create_proofs (
            token_hash TEXT PRIMARY KEY,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            session TEXT NOT NULL,
            credential_epoch INTEGER NOT NULL,
            password_hash TEXT NOT NULL,
            method TEXT NOT NULL,
            issued_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS member_api_keys (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            token_hash TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            credential_epoch INTEGER NOT NULL,
            last_used_at TEXT,
            revoked_at TEXT,
            include_conversations INTEGER NOT NULL DEFAULT 0,
            mcp_scopes TEXT NOT NULL DEFAULT '',
            conversations_migrated INTEGER NOT NULL DEFAULT 0
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
    .map_err(|e| format!("member_api_keys migration failed: {e}"))?;
    for (column, definition) in [
        ("include_conversations", "INTEGER NOT NULL DEFAULT 0"),
        ("mcp_scopes", "TEXT NOT NULL DEFAULT ''"),
        ("conversations_migrated", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !crate::user_row::has_column(conn, "member_api_keys", column)? {
            conn.execute(&format!("ALTER TABLE member_api_keys ADD COLUMN {column} {definition}"), [])
                .map_err(|e| e.to_string())?;
        }
    }
    conn.execute_batch("DROP TRIGGER IF EXISTS member_keys_acl_update;
        CREATE TRIGGER member_keys_acl_update
        AFTER UPDATE OF revoked_at, expires_at, credential_epoch, include_conversations, mcp_scopes ON member_api_keys
        BEGIN UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'; END;
        UPDATE member_api_keys SET include_conversations=1, conversations_migrated=1
        WHERE NOT EXISTS(SELECT 1 FROM auth_meta WHERE key='member_keys_v186_backfill');
        INSERT OR IGNORE INTO auth_meta(key,value) VALUES('member_keys_v186_backfill',1);")
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
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
    pub include_conversations: bool,
    pub mcp_scopes: Vec<String>,
    pub conversations_migrated: bool,
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
    InvalidScopes,
    CannotWiden,
    InvalidExpiry,
    AdminForbidden,
    LimitReached,
    HistoryLimitReached,
    InvalidPage,
    ReauthenticationRequired,
    AccountInactive,
    NotFound,
    Store,
}

impl MemberKeyError {
    /// Stable machine-readable code for the RPC error frame.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidScopes => "member_key_invalid_scopes",
            Self::CannotWiden => "member_key_cannot_widen",
            Self::InvalidExpiry => "member_key_invalid_expiry",
            Self::AdminForbidden => "member_key_admin_forbidden",
            Self::InvalidName => "member_key_invalid_name",
            Self::LimitReached => "member_key_limit_reached",
            Self::HistoryLimitReached => "member_key_history_limit_reached",
            Self::InvalidPage => "member_key_invalid_page",
            Self::ReauthenticationRequired => "member_key_step_up_required",
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
        include_conversations: row.get::<_, i64>(7)? == 1,
        mcp_scopes: stored_scopes(&row.get::<_, String>(8)?),
        conversations_migrated: row.get::<_, i64>(9)? == 1,
    })
}

const INFO_COLUMNS: &str = "t.id, t.name, t.created_at, t.last_used_at, t.revoked_at, t.expires_at,
    (julianday(t.expires_at) > julianday('now') AND EXISTS(SELECT 1 FROM users u
     WHERE u.id=t.user_id AND u.status='active' AND u.role IN ('employee','manager')
     AND u.must_change_password=0 AND u.credential_epoch=t.credential_epoch)),
    t.include_conversations, t.mcp_scopes, t.conversations_migrated";

/// Canonical wire representation; unknown entries invalidate the entire set.
fn canonical_scopes(scopes: &[String]) -> Result<Vec<String>, MemberKeyError> {
    if scopes.iter().any(|scope| !duduclaw_core::mcp_scopes::PERSON_KEY_MCP_SCOPE_STRINGS.contains(&scope.as_str())) {
        return Err(MemberKeyError::InvalidScopes);
    }
    let mut result = scopes.to_vec();
    result.sort();
    result.dedup();
    Ok(result)
}
fn stored_scopes(raw: &str) -> Vec<String> {
    if raw.is_empty() { return Vec::new(); }
    canonical_scopes(&raw.split(',').map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default()
}

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

/// Bounded keyset page. The cursor contains only public ordering metadata.
#[derive(Debug, Serialize)]
pub struct MemberKeyPage<T> {
    pub keys: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct KeyCursor { created_at: String, id: String }
fn page_cursor(limit: u32, cursor: Option<&str>) -> Result<Option<KeyCursor>, MemberKeyError> {
    if !(1..=100).contains(&limit) { return Err(MemberKeyError::InvalidPage); }
    cursor.map(|raw| {
        if raw.len() > 512 { return Err(MemberKeyError::InvalidPage); }
        let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| MemberKeyError::InvalidPage)?;
        let value: KeyCursor = serde_json::from_slice(&bytes).map_err(|_| MemberKeyError::InvalidPage)?;
        if DateTime::parse_from_rfc3339(&value.created_at).is_err() || Uuid::parse_str(&value.id).is_err() {
            return Err(MemberKeyError::InvalidPage);
        }
        Ok(value)
    }).transpose()
}
fn next_cursor(info: &MemberKeyInfo) -> Result<String, MemberKeyError> {
    serde_json::to_vec(&KeyCursor { created_at: info.created_at.clone(), id: info.id.clone() })
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes)).map_err(store_error)
}

impl UserDb {
    /// Fresh proof for exactly one create. Never returned over the RPC wire.
    /// The gateway must charge the transport peer's existing login rate limit first.
    pub fn member_key_reauthenticate(
        &self, user: &str, session: &str, epoch: u64, password: &str, code: &str,
        cipher: &dyn crate::mfa::MfaCipher, now: i64,
    ) -> Result<String, MemberKeyError> {
        let denied = MemberKeyError::ReauthenticationRequired;
        if Uuid::parse_str(session).is_err() { return Err(denied); }
        let account = self.get_user(user).map_err(store_error)?.ok_or(denied)?;
        if account.credential_epoch != epoch { return Err(denied); }
        let snapshot: String = self.conn().query_row("SELECT password_hash FROM users WHERE id=?1", [user], |r| r.get(0))
            .map_err(store_error)?;
        let mfa = self.mfa_active(user).map_err(store_error)?;
        if mfa {
            self.mfa_step_up_verify(user, code, cipher, now).map_err(|_| denied)?;
        } else {
            let verified = self.verify_password(&account.email, password).map_err(|_| denied)?;
            if password.is_empty() || verified.id != user || verified.credential_epoch != epoch { return Err(denied); }
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let current: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND credential_epoch=?2
            AND password_hash=?3 AND status='active' AND must_change_password=0 AND role IN ('employee','manager'))",
            params![user, epoch, snapshot], |r| r.get(0)).map_err(store_error)?;
        if !current || crate::mfa::any_factor(&tx, user).map_err(store_error)? != mfa
            || !crate::remembered_sessions::session_valid(&tx, Some(session), user, now).map_err(store_error)? {
            return Err(denied);
        }
        // At most one pending proof per account, even if issuance is interrupted.
        tx.execute("DELETE FROM member_key_create_proofs WHERE user_id=?1 OR expires_at<=?2", params![user, now]).map_err(store_error)?;
        let proof = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO member_key_create_proofs VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![digest(&proof), user, session, epoch, snapshot, CREATE_METHOD, now, now + REAUTH_TTL_SECS]).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(proof)
    }

    /// Consume before creation (including failed creation); a proof cannot be replayed.
    pub fn member_key_create_authenticated(
        &self, user: &str, name: &str, days: u64, epoch: u64, session: &str, proof: &str,
        include_conversations: bool, mcp_scopes: &[String],
    ) -> Result<CreatedMemberKey, MemberKeyError> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let now = Utc::now().timestamp();
        let snapshot: Option<String> = tx.query_row("SELECT password_hash FROM member_key_create_proofs
            WHERE token_hash=?1 AND user_id=?2 AND session=?3 AND credential_epoch=?4 AND method=?5
            AND issued_at<=?6 AND expires_at>?6 AND expires_at-issued_at<=?7",
            params![digest(proof), user, session, epoch, CREATE_METHOD, now, REAUTH_TTL_SECS], |r| r.get(0))
            .optional().map_err(store_error)?;
        if snapshot.is_some() {
            tx.execute("DELETE FROM member_key_create_proofs WHERE token_hash=?1", [digest(proof)]).map_err(store_error)?;
        }
        let valid = crate::remembered_sessions::session_valid(&tx, Some(session), user, now).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        drop(conn);
        // The password snapshot also closes the legacy epoch-zero first-key race.
        let snapshot = snapshot.filter(|_| valid).ok_or(MemberKeyError::ReauthenticationRequired)?;
        self.member_key_create_in_session(user, name, days, epoch, Some((session, &snapshot)), include_conversations, mcp_scopes)
    }

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
        include_conversations: bool,
        mcp_scopes: &[String],
    ) -> Result<CreatedMemberKey, MemberKeyError> {
        self.member_key_create_in_session(user_id, name, expires_in_days, verified_epoch, None, include_conversations, mcp_scopes)
    }

    fn member_key_create_in_session(
        &self, user_id: &str, name: &str, expires_in_days: u64, verified_epoch: u64, proof_binding: Option<(&str, &str)>,
        include_conversations: bool, mcp_scopes: &[String],
    ) -> Result<CreatedMemberKey, MemberKeyError> {
        if !(1..=365).contains(&expires_in_days) { return Err(MemberKeyError::InvalidExpiry); }
        let name = clean_name(name)?;
        let mcp_scopes = canonical_scopes(mcp_scopes)?;
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
                     AND role IN ('employee','manager') AND credential_epoch = ?2
                     AND (?3 IS NULL OR password_hash = ?3))",
                params![user_id, verified_epoch, proof_binding.map(|(_, hash)| hash)],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        if !active || !crate::remembered_sessions::session_valid(&tx, proof_binding.map(|(session, _)| session), user_id, Utc::now().timestamp()).map_err(store_error)? {
            return Err(MemberKeyError::AccountInactive);
        }
        tx.execute(
            "UPDATE member_api_keys SET revoked_at=?3 WHERE user_id=?1 AND revoked_at IS NULL
             AND (credential_epoch!=?2 OR julianday(expires_at)<=julianday(?3) OR julianday(expires_at) IS NULL)",
            params![user_id, verified_epoch, now],
        ).map_err(store_error)?;
        tx.execute(
            "DELETE FROM member_api_keys WHERE user_id=?1 AND revoked_at IS NOT NULL
             AND julianday(revoked_at)<julianday(?2)-?3",
            params![user_id, now, MEMBER_KEY_RETENTION_DAYS],
        ).map_err(store_error)?;
        let (active_count, total): (i64, i64) = tx.query_row(
            "SELECT COUNT(CASE WHEN revoked_at IS NULL THEN 1 END), COUNT(*) FROM member_api_keys WHERE user_id=?1",
            [user_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).map_err(store_error)?;
        if active_count >= MAX_ACTIVE_MEMBER_KEYS || total >= MAX_MEMBER_KEY_ROWS {
            // Keep maintenance even when the creation cap refuses the new row.
            tx.commit().map_err(store_error)?;
            return Err(if total >= MAX_MEMBER_KEY_ROWS { MemberKeyError::HistoryLimitReached } else { MemberKeyError::LimitReached });
        }
        tx.execute(
            "INSERT INTO member_api_keys(id, user_id, name, token_hash, created_at, expires_at, credential_epoch, include_conversations, mcp_scopes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, user_id, name, hash, now, expires_at, verified_epoch, include_conversations, mcp_scopes.join(",")],
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
                include_conversations,
                mcp_scopes,
                conversations_migrated: false,
            },
            secret,
        })
    }

    /// Bounded compatibility helper for internal callers.
    pub fn member_api_keys_for_user(&self, user_id: &str) -> Result<Vec<MemberKeyInfo>, MemberKeyError> {
        Ok(self.member_api_keys_for_user_page(user_id, 50, None)?.keys)
    }

    /// The caller's own credentials, newest first, with stable keyset pagination.
    pub fn member_api_keys_for_user_page(&self, user_id: &str, limit: u32, cursor: Option<&str>) -> Result<MemberKeyPage<MemberKeyInfo>, MemberKeyError> {
        let cursor = page_cursor(limit, cursor)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {INFO_COLUMNS} FROM member_api_keys t WHERE t.user_id=?1
             AND (?2 IS NULL OR t.created_at<?2 OR (t.created_at=?2 AND t.id>?3))
             ORDER BY t.created_at DESC, t.id LIMIT ?4"
        )).map_err(store_error)?;
        let mut keys = stmt.query_map(params![user_id, cursor.as_ref().map(|c| &c.created_at), cursor.as_ref().map(|c| &c.id), limit + 1], info_from_row)
            .map_err(store_error)?.collect::<Result<Vec<_>, _>>().map_err(store_error)?;
        let more = keys.len() > limit as usize;
        keys.truncate(limit as usize);
        let next_cursor = if more { keys.last().map(next_cursor).transpose()? } else { None };
        Ok(MemberKeyPage { keys, next_cursor })
    }

    /// Bounded compatibility helper for internal callers.
    pub fn member_api_keys_all(&self, admin_id: &str) -> Result<Vec<MemberKeyAdminEntry>, MemberKeyError> {
        Ok(self.member_api_keys_all_page(admin_id, 50, None)?.keys)
    }

    pub fn member_api_keys_all_page(&self, admin_id: &str, limit: u32, cursor: Option<&str>) -> Result<MemberKeyPage<MemberKeyAdminEntry>, MemberKeyError> {
        let cursor = page_cursor(limit, cursor)?;
        let mut conn = self.conn();
        let conn = conn.transaction().map_err(store_error)?;
        require_admin(&conn, admin_id)?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {INFO_COLUMNS}, t.user_id, u.email, u.display_name
             FROM member_api_keys t LEFT JOIN users u ON u.id=t.user_id
             WHERE (?1 IS NULL OR t.created_at<?1 OR (t.created_at=?1 AND t.id>?2))
             ORDER BY t.created_at DESC, t.id LIMIT ?3"
        )).map_err(store_error)?;
        let mut keys = stmt.query_map(params![cursor.as_ref().map(|c| &c.created_at), cursor.as_ref().map(|c| &c.id), limit + 1], |row| {
            Ok(MemberKeyAdminEntry { token: info_from_row(row)?, user_id: row.get(10)?,
                user_email: row.get(11)?, user_display_name: row.get(12)? })
        }).map_err(store_error)?.collect::<Result<Vec<_>, _>>().map_err(store_error)?;
        let more = keys.len() > limit as usize;
        keys.truncate(limit as usize);
        let next_cursor = if more { keys.last().map(|key| next_cursor(&key.token)).transpose()? } else { None };
        Ok(MemberKeyPage { keys, next_cursor })
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

    /// Narrow an owned key. Widening requires revocation and a new credential.
    pub fn member_key_narrow_own(
        &self, user_id: &str, key_id: &str, include_conversations: Option<bool>, mcp_scopes: Option<&[String]>,
    ) -> Result<(), MemberKeyError> {
        self.narrow_member_key(user_id, key_id, Some(user_id), include_conversations, mcp_scopes)
    }

    /// The actor is required so Admin authority and audit attribution stay live.
    pub fn member_key_narrow_admin(
        &self, admin_id: &str, key_id: &str, include_conversations: Option<bool>, mcp_scopes: Option<&[String]>,
    ) -> Result<(), MemberKeyError> {
        self.narrow_member_key(admin_id, key_id, None, include_conversations, mcp_scopes)
    }

    fn narrow_member_key(
        &self, actor: &str, key_id: &str, owner: Option<&str>,
        include_conversations: Option<bool>, mcp_scopes: Option<&[String]>,
    ) -> Result<(), MemberKeyError> {
        let requested = mcp_scopes.map(canonical_scopes).transpose()?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        if owner.is_none() { require_admin(&tx, actor)?; }
        let before = tx.query_row(&format!("SELECT {INFO_COLUMNS} FROM member_api_keys t
            WHERE t.id=?1 AND (?2 IS NULL OR t.user_id=?2)"), params![key_id, owner], info_from_row)
            .optional().map_err(store_error)?.filter(|key| key.active).ok_or(MemberKeyError::NotFound)?;
        let include = include_conversations.unwrap_or(before.include_conversations);
        let scopes = requested.as_ref().unwrap_or(&before.mcp_scopes);
        if (include && !before.include_conversations) || scopes.iter().any(|scope| !before.mcp_scopes.contains(scope)) {
            return Err(MemberKeyError::CannotWiden);
        }
        tx.execute("UPDATE member_api_keys SET include_conversations=?2, mcp_scopes=?3 WHERE id=?1",
            params![key_id, include, scopes.join(",")]).map_err(store_error)?;
        audit(&tx, actor, "member_key_narrowed", key_id, serde_json::json!({
            "by_admin": owner.is_none(),
            "before": {"include_conversations": before.include_conversations, "mcp_scopes": before.mcp_scopes},
            "after": {"include_conversations": include, "mcp_scopes": scopes},
        })).map_err(store_error)?;
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
        self.member_key_authority(id, user_id).map(|live| live.context)
    }

    /// Include expiry in the same transaction as key validity and live ACLs.
    pub fn member_key_authority(&self, id: &str, user_id: &str) -> Result<MemberKeyAuthority, String> {
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
        ctx.origin = crate::acl::ContextOrigin::MemberKey;
        // The schema requires expiry; missing, malformed or negative timestamps
        // remain refusals, never an unbounded credential.
        let (expiry, include_conversations, scopes): (String, i64, String) = tx.query_row(
            "SELECT expires_at, include_conversations, mcp_scopes FROM member_api_keys WHERE id=?1",
            [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|_| "authentication service unavailable")?;
        let expires_at = DateTime::parse_from_rfc3339(&expiry)
            .map_err(|_| "member key invalid")?.timestamp();
        let expires_at = u64::try_from(expires_at).map_err(|_| "member key invalid")?;
        Ok(MemberKeyAuthority { context: ctx, expires_at, include_conversations: include_conversations == 1, mcp_scopes: stored_scopes(&scopes) })
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
    fn context_origin_member_key() {
        let (_home, db, user) = fixture();
        let key = db.member_key_create(&user.id, "origin", 1, user.credential_epoch, false, &[]).unwrap();
        let ctx = db.member_key_context(&key.info.id, &user.id).unwrap();
        assert_eq!(ctx.origin, crate::acl::ContextOrigin::MemberKey);
        assert_eq!(ctx.member_key_id.as_deref(), Some(key.info.id.as_str()));
    }

    #[test]
    fn secret_is_random_redacted_and_never_listed_or_audited() {
        let (_home, db, user) = fixture();
        let a = db.member_key_create(&user.id, "服務", 90, user.credential_epoch, false, &[]).unwrap();
        let b = db.member_key_create(&user.id, "服務二", 90, user.credential_epoch, false, &[]).unwrap();
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
            assert_eq!(db.member_key_create(&user.id, "x", days, 0, false, &[]).unwrap_err(), MemberKeyError::InvalidExpiry);
        }
        for name in ["", "\n", "bad\nname"] {
            assert_eq!(db.member_key_create(&user.id, name, 1, 0, false, &[]).unwrap_err(), MemberKeyError::InvalidName);
        }
        let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
        assert_eq!(db.member_key_create(&admin.id, "x", 1, 0, false, &[]).unwrap_err(), MemberKeyError::AdminForbidden);
        let first = db.member_key_create(&user.id, "first", 1, 0, false, &[]).unwrap();
        for _ in 1..5 { db.member_key_create(&user.id, "x", 365, 0, false, &[]).unwrap(); }
        assert_eq!(db.member_key_create(&user.id, "six", 90, 0, false, &[]).unwrap_err(), MemberKeyError::LimitReached);
        assert!(db.member_api_keys_all(&user.id).is_err());
        assert!(db.member_key_admin_revoke(&user.id, &first.info.id).is_err());
        assert!(db.member_key_revoke_own(&admin.id, &first.info.id).is_err());
        assert_eq!(db.member_api_keys_all(&admin.id).unwrap().len(), 5);
        db.member_key_admin_revoke(&admin.id, &first.info.id).unwrap();
        db.member_key_create(&user.id, "replacement", 1, 0, false, &[]).unwrap();
    }
    #[test]
    fn new_and_open_credentials_refuse_all_revocation_causes() {
        for cause in ["revoke", "expire", "epoch", "password", "admin", "offboard", "suspend", "delete"] {
            let (_home, db, user) = fixture();
            let created = db.member_key_create(&user.id, "x", 1, 0, false, &[]).unwrap();
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
        let member = db.member_key_create(&user.id, "x", 1, 0, false, &[]).unwrap();
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
        let key = db.member_key_create(&user.id, "x", 1, 0, false, &[]).unwrap();
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
    struct Cipher;
    impl crate::mfa::MfaCipher for Cipher {
        fn encrypt(&self, value: &str) -> Result<String, String> { Ok(value.to_owned()) }
        fn decrypt(&self, value: &str) -> Result<String, String> { Ok(value.to_owned()) }
    }

    #[test]
    fn create_proof_requires_password_and_is_bound_single_use_and_short_lived() {
        let (_home, db, user) = fixture();
        let session = Uuid::new_v4().to_string();
        let other_session = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        for password in ["", "wrong"] {
            assert_eq!(db.member_key_reauthenticate(&user.id, &session, 0, password, "", &Cipher, now).unwrap_err(), MemberKeyError::ReauthenticationRequired);
        }
        let proof = db.member_key_reauthenticate(&user.id, &session, 0, "test-password", "", &Cipher, now).unwrap();
        for (owner, jti, epoch) in [("someone-else", session.as_str(), 0), (user.id.as_str(), other_session.as_str(), 0), (user.id.as_str(), session.as_str(), 1)] {
            assert!(db.member_key_create_authenticated(owner, "x", 1, epoch, jti, &proof, false, &[]).is_err());
        }
        db.conn().execute("UPDATE member_key_create_proofs SET method='users.change_password'", []).unwrap();
        assert!(db.member_key_create_authenticated(&user.id, "x", 1, 0, &session, &proof, false, &[]).is_err());
        db.conn().execute("UPDATE member_key_create_proofs SET method=?1", [CREATE_METHOD]).unwrap();
        db.member_key_create_authenticated(&user.id, "x", 1, 0, &session, &proof, false, &[]).unwrap();
        assert_eq!(db.member_key_create_authenticated(&user.id, "replay", 1, 0, &session, &proof, false, &[]).unwrap_err(), MemberKeyError::ReauthenticationRequired);
        let expired = db.member_key_reauthenticate(&user.id, &session, 0, "test-password", "", &Cipher, now - 300).unwrap();
        assert!(db.member_key_create_authenticated(&user.id, "expired", 1, 0, &session, &expired, false, &[]).is_err());
        let future = db.member_key_reauthenticate(&user.id, &session, 0, "test-password", "", &Cipher, now + 300).unwrap();
        assert!(db.member_key_create_authenticated(&user.id, "future", 1, 0, &session, &future, false, &[]).is_err());
        let stale = db.member_key_reauthenticate(&user.id, &session, 0, "test-password", "", &Cipher, now).unwrap();
        db.mfa_operator_reset(&user.email).unwrap();
        assert!(db.member_key_create_authenticated(&user.id, "stale", 1, 0, &session, &stale, false, &[]).is_err());
        assert_eq!(db.member_api_keys_for_user(&user.id).unwrap().len(), 1);
    }

    #[test]
    fn cleanup_revokes_expired_and_old_epochs_and_preserves_retention_at_total_cap() {
        let (_home, db, user) = fixture();
        let expired = db.member_key_create(&user.id, "expired", 1, 0, false, &[]).unwrap();
        let old_epoch = db.member_key_create(&user.id, "old epoch", 1, 0, false, &[]).unwrap();
        let old_revoked = db.member_key_create(&user.id, "old revoked", 1, 0, false, &[]).unwrap();
        db.conn().execute("UPDATE member_api_keys SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1", [&expired.info.id]).unwrap();
        db.conn().execute("UPDATE member_api_keys SET credential_epoch=99 WHERE id=?1", [&old_epoch.info.id]).unwrap();
        db.conn().execute("UPDATE member_api_keys SET revoked_at='2000-01-01T00:00:00Z' WHERE id=?1", [&old_revoked.info.id]).unwrap();
        let current = db.member_key_create(&user.id, "current", 1, 0, false, &[]).unwrap();
        let listed = db.member_api_keys_for_user(&user.id).unwrap();
        assert!(!listed.iter().any(|key| key.id == old_revoked.info.id));
        for id in [&expired.info.id, &old_epoch.info.id] {
            assert!(listed.iter().find(|key| &key.id == id).unwrap().revoked_at.is_some());
        }
        db.member_key_revoke_own(&user.id, &current.info.id).unwrap();
        for _ in listed.len()..MAX_MEMBER_KEY_ROWS as usize {
            let key = db.member_key_create(&user.id, "history", 1, 0, false, &[]).unwrap();
            db.member_key_revoke_own(&user.id, &key.info.id).unwrap();
        }
        assert_eq!(db.member_key_create(&user.id, "over cap", 1, 0, false, &[]).unwrap_err(), MemberKeyError::HistoryLimitReached);
        assert_eq!(db.member_api_keys_for_user(&user.id).unwrap().len(), 50);
        db.conn().execute("UPDATE member_api_keys SET revoked_at='2000-01-01T00:00:00Z' WHERE id=?1", [&current.info.id]).unwrap();
        db.member_key_create(&user.id, "after retention", 1, 0, false, &[]).unwrap();
        assert_eq!(db.member_api_keys_for_user(&user.id).unwrap().len(), 50);
    }

    #[test]
    fn keyset_pages_have_no_gaps_for_equal_timestamps_and_isolate_owners() {
        let (_home, db, user) = fixture();
        let other = db.create_user("other@test.invalid", "Other", "test-password", UserRole::Employee).unwrap();
        let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
        for owner in [&user.id, &other.id] {
            for _ in 0..3 { db.member_key_create(owner, "x", 1, 0, false, &[]).unwrap(); }
        }
        db.conn().execute("UPDATE member_api_keys SET created_at='2026-01-01T00:00:00Z'", []).unwrap();
        let first = db.member_api_keys_for_user_page(&user.id, 2, None).unwrap();
        let second = db.member_api_keys_for_user_page(&user.id, 2, first.next_cursor.as_deref()).unwrap();
        assert_eq!((first.keys.len(), second.keys.len()), (2, 1));
        assert!(second.next_cursor.is_none());
        assert!(!first.keys.iter().any(|key| key.id == second.keys[0].id));
        // A deleted cursor row does not make subsequent pages fail or repeat.
        db.conn().execute("DELETE FROM member_api_keys WHERE id=?1", [&first.keys[1].id]).unwrap();
        assert_eq!(db.member_api_keys_for_user_page(&user.id, 2, first.next_cursor.as_deref()).unwrap().keys, second.keys);
        let mut cursor = None;
        let mut ids = std::collections::HashSet::new();
        loop {
            let page = db.member_api_keys_all_page(&admin.id, 2, cursor.as_deref()).unwrap();
            for key in page.keys { assert!(ids.insert(key.token.id)); }
            cursor = page.next_cursor;
            if cursor.is_none() { break; }
        }
        assert_eq!(ids.len(), 5);
        assert!(db.member_api_keys_all_page(&user.id, 2, None).is_err());
        for limit in [0, 101, u32::MAX] { assert!(db.member_api_keys_for_user_page(&user.id, limit, None).is_err()); }
        assert!(db.member_api_keys_for_user_page(&user.id, 2, Some("invalid")).is_err());
    }

    #[test]
    fn mfa_create_requires_fresh_factor_and_consumes_factor_and_create_proof() {
        let (_home, db, user) = fixture();
        let (enrollment, secret) = crate::mfa::test_totp_prepare(&db, &user.id, "test-password", &Cipher);
        db.mfa_confirm(&user.id, Some(&enrollment), &crate::mfa::test_totp_code(&secret, 100), &Cipher, 3000).unwrap();
        let epoch = db.get_user(&user.id).unwrap().unwrap().credential_epoch;
        let now = Utc::now().timestamp();
        let session = Uuid::new_v4().to_string();
        assert_eq!(db.member_key_reauthenticate(&user.id, &session, epoch, "test-password", "", &Cipher, now).unwrap_err(), MemberKeyError::ReauthenticationRequired);
        let code = crate::mfa::test_totp_code(&secret, now / 30);
        let proof = db.member_key_reauthenticate(&user.id, &session, epoch, "", &code, &Cipher, now).unwrap();
        db.member_key_create_authenticated(&user.id, "mfa", 1, epoch, &session, &proof, false, &[]).unwrap();
        assert!(db.member_key_create_authenticated(&user.id, "replay", 1, epoch, &session, &proof, false, &[]).is_err());
        assert!(db.member_key_reauthenticate(&user.id, &session, epoch, "", &code, &Cipher, now).is_err());
    }

    #[test]
    fn first_key_proof_is_invalid_after_password_change_even_at_legacy_epoch_zero() {
        let (_home, db, user) = fixture();
        let session = Uuid::new_v4().to_string();
        let proof = db.member_key_reauthenticate(&user.id, &session, 0, "test-password", "", &Cipher, Utc::now().timestamp()).unwrap();
        // There are no key rows yet, so the legacy password trigger may leave epoch zero.
        db.conn().execute("UPDATE users SET password_hash='changed-test-hash' WHERE id=?1", [&user.id]).unwrap();
        assert!(db.member_key_create_authenticated(&user.id, "stale password", 1, 0, &session, &proof, false, &[]).is_err());
        assert!(db.member_api_keys_for_user(&user.id).unwrap().is_empty());
        let remaining: i64 = db.conn().query_row("SELECT COUNT(*) FROM member_key_create_proofs", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0);
    }

}

#[cfg(test)]
#[path = "member_key_scope_tests.rs"]
mod scope_tests;
