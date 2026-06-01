//! MCP refresh-token authentication (v1.16.0).
//!
//! Long-lived, revocable, scoped credentials that supersede the legacy 30-day
//! `ddc_<env>_<32hex>` API keys.  Behaviour summary:
//!
//! - **Format**: `ddc_refresh_<env>_<64hex>` (twice the entropy of legacy).
//! - **Storage**: SHA-256 hash + metadata in `~/.duduclaw/mcp_tokens.db`.
//! - **Lifetime**: 90 days from issuance, or until revoked.
//! - **Revocation**: per-token, via `duduclaw mcp revoke-token <jti>`.
//! - **Compat**: legacy `ddc_<env>_<32hex>` keys keep working via
//!   `mcp_auth::authenticate_with_key`. Both code paths are exercised by
//!   the existing `authenticate_from_env` dispatcher.
//!
//! The choice of "long-lived refresh token but no separate access token" is
//! deliberate.  MCP servers are spawned as long-running subprocesses that
//! authenticate exactly once at startup; there is no subsequent request flow
//! that an access token would protect.  The refresh token therefore IS the
//! credential, and "refresh" in this module means "renew before expiry by
//! issuing a fresh token and revoking the old one" — managed via CLI.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::Digest;

use crate::mcp_auth::{AuthError, Principal, Scope, parse_scopes};

// ── Constants ────────────────────────────────────────────────────────────────

/// Default refresh-token lifetime — 90 days from issuance.
pub const REFRESH_TOKEN_TTL_DAYS: i64 = 90;

/// SQLite filename relative to `~/.duduclaw/`.
const TOKEN_DB_FILENAME: &str = "mcp_tokens.db";

/// Token prefix that distinguishes refresh tokens from legacy API keys.
/// Legacy: `ddc_<env>_<32hex>`. Refresh: `ddc_refresh_<env>_<64hex>`.
pub const REFRESH_TOKEN_PREFIX: &str = "ddc_refresh_";

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RefreshTokenMeta {
    /// Token identifier — first 16 hex chars of SHA-256(token), used for
    /// human-readable references in revoke commands and audit logs.
    pub jti: String,
    pub client_id: String,
    pub scopes: HashSet<Scope>,
    pub is_external: bool,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl RefreshTokenMeta {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

// ── Format ───────────────────────────────────────────────────────────────────

/// Validate format: `ddc_refresh_(prod|staging|dev)_<64 hex>`.
pub fn is_refresh_token_format(token: &str) -> bool {
    let re = regex::Regex::new(r"^ddc_refresh_(prod|staging|dev)_[a-f0-9]{64}$").unwrap();
    re.is_match(token)
}

/// Compute the SHA-256 hex digest of a token (the lookup key in the DB).
fn token_hash(token: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Build the `jti` (JWT ID) from a token hash — first 16 hex chars.  This is
/// what operators see when listing tokens and pass to `revoke-token`.  Never
/// derivable from the raw token alone without computing the SHA-256, so it
/// can be logged safely.
fn jti_from_hash(hash: &str) -> String {
    hash.chars().take(16).collect()
}

// ── Storage layer ────────────────────────────────────────────────────────────

fn token_db_path(home: &Path) -> std::path::PathBuf {
    home.join(TOKEN_DB_FILENAME)
}

/// Initialize the SQLite schema. Idempotent.
pub fn init_schema(home: &Path) -> rusqlite::Result<()> {
    let conn = Connection::open(token_db_path(home))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS refresh_tokens (
            jti           TEXT PRIMARY KEY,
            token_hash    TEXT NOT NULL UNIQUE,
            client_id     TEXT NOT NULL,
            scopes        TEXT NOT NULL,
            is_external   INTEGER NOT NULL DEFAULT 0,
            issued_at     INTEGER NOT NULL,
            expires_at    INTEGER NOT NULL,
            revoked_at    INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_refresh_client_id
            ON refresh_tokens(client_id);
        CREATE INDEX IF NOT EXISTS idx_refresh_expires_at
            ON refresh_tokens(expires_at);
        ",
    )?;
    Ok(())
}

/// Issue a new refresh token for the given client + scopes.
///
/// Returns the raw token string — caller MUST display it once to the operator
/// then forget it. Only the SHA-256 hash is persisted in the DB, so an
/// operator who loses the token cannot recover it; they must issue a new one.
pub fn issue_refresh_token(
    home: &Path,
    env_label: &str, // "prod" | "staging" | "dev"
    client_id: &str,
    scopes: &HashSet<Scope>,
    is_external: bool,
) -> Result<(String, RefreshTokenMeta), AuthError> {
    if !matches!(env_label, "prod" | "staging" | "dev") {
        return Err(AuthError::InvalidFormat);
    }

    init_schema(home).map_err(|_| AuthError::InvalidFormat)?;

    // Generate 32 random bytes = 64 hex chars.
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let token = format!("{REFRESH_TOKEN_PREFIX}{env_label}_{hex}");

    let now = Utc::now();
    let expires = now + chrono::Duration::days(REFRESH_TOKEN_TTL_DAYS);
    let hash = token_hash(&token);
    let jti = jti_from_hash(&hash);

    let scopes_csv = scopes
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let conn = Connection::open(token_db_path(home)).map_err(|_| AuthError::InvalidFormat)?;
    conn.execute(
        "INSERT INTO refresh_tokens
            (jti, token_hash, client_id, scopes, is_external, issued_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            jti,
            hash,
            client_id,
            scopes_csv,
            is_external as i64,
            now.timestamp(),
            expires.timestamp(),
        ],
    )
    .map_err(|_| AuthError::InvalidFormat)?;

    let meta = RefreshTokenMeta {
        jti,
        client_id: client_id.to_string(),
        scopes: scopes.clone(),
        is_external,
        issued_at: now,
        expires_at: expires,
        revoked_at: None,
    };

    Ok((token, meta))
}

/// Look up a refresh token. Returns metadata on hit, `UnknownKey` on miss.
fn lookup_refresh_token(home: &Path, token: &str) -> Result<RefreshTokenMeta, AuthError> {
    init_schema(home).map_err(|_| AuthError::InvalidFormat)?;

    let conn = Connection::open(token_db_path(home)).map_err(|_| AuthError::InvalidFormat)?;
    let hash = token_hash(token);

    let row = conn
        .query_row(
            "SELECT jti, client_id, scopes, is_external, issued_at, expires_at, revoked_at
             FROM refresh_tokens WHERE token_hash = ?1",
            params![hash],
            |row| {
                let jti: String = row.get(0)?;
                let client_id: String = row.get(1)?;
                let scopes_csv: String = row.get(2)?;
                let is_external: i64 = row.get(3)?;
                let issued_ts: i64 = row.get(4)?;
                let expires_ts: i64 = row.get(5)?;
                let revoked_ts: Option<i64> = row.get(6)?;
                Ok((jti, client_id, scopes_csv, is_external, issued_ts, expires_ts, revoked_ts))
            },
        )
        .optional()
        .map_err(|_| AuthError::InvalidFormat)?;

    let (jti, client_id, scopes_csv, is_external, issued_ts, expires_ts, revoked_ts) =
        row.ok_or(AuthError::UnknownKey)?;

    let scopes = parse_scopes(&scopes_csv).unwrap_or_default();
    let issued_at = DateTime::<Utc>::from_timestamp(issued_ts, 0)
        .ok_or(AuthError::InvalidFormat)?;
    let expires_at = DateTime::<Utc>::from_timestamp(expires_ts, 0)
        .ok_or(AuthError::InvalidFormat)?;
    let revoked_at = revoked_ts.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0));

    Ok(RefreshTokenMeta {
        jti,
        client_id,
        scopes,
        is_external: is_external != 0,
        issued_at,
        expires_at,
        revoked_at,
    })
}

/// Authenticate via refresh token. Used by [`crate::mcp_auth::authenticate_from_env`]
/// when the credential matches the refresh-token format.
///
/// Mirrors `mcp_auth::authenticate_with_key`'s error model so callers don't
/// need to special-case credential type.
pub fn authenticate_with_refresh_token(
    token: &str,
    home: &Path,
) -> Result<Principal, AuthError> {
    if !is_refresh_token_format(token) {
        return Err(AuthError::InvalidFormat);
    }

    let meta = lookup_refresh_token(home, token)?;
    let now = Utc::now();

    if meta.is_revoked() {
        return Err(AuthError::UnknownKey);
    }
    if meta.is_expired(now) {
        let days_old = (now - meta.issued_at).num_days() as u64;
        return Err(AuthError::KeyExpired { days_old });
    }

    Ok(Principal {
        client_id: meta.client_id,
        scopes: meta.scopes,
        is_external: meta.is_external,
        created_at: meta.issued_at,
    })
}

/// Revoke a refresh token by its jti. Returns `true` if a row was updated.
pub fn revoke_token(home: &Path, jti: &str) -> rusqlite::Result<bool> {
    init_schema(home)?;
    let conn = Connection::open(token_db_path(home))?;
    let now = Utc::now().timestamp();
    let rows = conn.execute(
        "UPDATE refresh_tokens SET revoked_at = ?1
         WHERE jti = ?2 AND revoked_at IS NULL",
        params![now, jti],
    )?;
    Ok(rows > 0)
}

/// List all refresh tokens for operator inspection.
pub fn list_tokens(home: &Path) -> rusqlite::Result<Vec<RefreshTokenMeta>> {
    init_schema(home)?;
    let conn = Connection::open(token_db_path(home))?;
    let mut stmt = conn.prepare(
        "SELECT jti, client_id, scopes, is_external, issued_at, expires_at, revoked_at
         FROM refresh_tokens ORDER BY issued_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let jti: String = row.get(0)?;
        let client_id: String = row.get(1)?;
        let scopes_csv: String = row.get(2)?;
        let is_external: i64 = row.get(3)?;
        let issued_ts: i64 = row.get(4)?;
        let expires_ts: i64 = row.get(5)?;
        let revoked_ts: Option<i64> = row.get(6)?;
        Ok((jti, client_id, scopes_csv, is_external, issued_ts, expires_ts, revoked_ts))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (jti, client_id, scopes_csv, is_external, issued_ts, expires_ts, revoked_ts) = row?;
        let scopes = parse_scopes(&scopes_csv).unwrap_or_default();
        let issued_at = DateTime::<Utc>::from_timestamp(issued_ts, 0).unwrap_or_else(Utc::now);
        let expires_at = DateTime::<Utc>::from_timestamp(expires_ts, 0).unwrap_or_else(Utc::now);
        let revoked_at = revoked_ts.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0));
        out.push(RefreshTokenMeta {
            jti,
            client_id,
            scopes,
            is_external: is_external != 0,
            issued_at,
            expires_at,
            revoked_at,
        });
    }
    Ok(out)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_scopes() -> HashSet<Scope> {
        let mut s = HashSet::new();
        s.insert(Scope::MemoryRead);
        s.insert(Scope::WikiRead);
        s
    }

    #[test]
    fn issue_and_authenticate_roundtrip() {
        let dir = TempDir::new().unwrap();
        let (token, meta) =
            issue_refresh_token(dir.path(), "dev", "claude-desktop", &fresh_scopes(), false)
                .expect("issue should succeed");
        assert!(is_refresh_token_format(&token));
        assert_eq!(meta.client_id, "claude-desktop");
        assert!(!meta.is_revoked());
        assert!(!meta.is_expired(Utc::now()));

        let principal = authenticate_with_refresh_token(&token, dir.path())
            .expect("authenticate should succeed");
        assert_eq!(principal.client_id, "claude-desktop");
        assert!(principal.scopes.contains(&Scope::MemoryRead));
    }

    #[test]
    fn invalid_format_rejected() {
        let dir = TempDir::new().unwrap();
        // Legacy short format must NOT be treated as a refresh token.
        let result = authenticate_with_refresh_token(
            "ddc_dev_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4",
            dir.path(),
        );
        assert_eq!(result.unwrap_err(), AuthError::InvalidFormat);
    }

    #[test]
    fn unknown_token_rejected() {
        let dir = TempDir::new().unwrap();
        init_schema(dir.path()).unwrap();
        let unknown = format!("ddc_refresh_dev_{}", "f".repeat(64));
        let result = authenticate_with_refresh_token(&unknown, dir.path());
        assert_eq!(result.unwrap_err(), AuthError::UnknownKey);
    }

    #[test]
    fn revoked_token_rejected_as_unknown() {
        let dir = TempDir::new().unwrap();
        let (token, meta) =
            issue_refresh_token(dir.path(), "dev", "claude-desktop", &fresh_scopes(), false)
                .unwrap();

        let revoked = revoke_token(dir.path(), &meta.jti).unwrap();
        assert!(revoked, "revoke should succeed on first call");

        let result = authenticate_with_refresh_token(&token, dir.path());
        assert_eq!(result.unwrap_err(), AuthError::UnknownKey);

        // Second revoke is a no-op.
        let revoked_again = revoke_token(dir.path(), &meta.jti).unwrap();
        assert!(!revoked_again);
    }

    #[test]
    fn expired_token_rejected_as_expired() {
        let dir = TempDir::new().unwrap();
        init_schema(dir.path()).unwrap();
        // Hand-craft an expired entry (90 days in the past = barely-expired).
        let conn = Connection::open(token_db_path(dir.path())).unwrap();
        let token = format!("ddc_refresh_dev_{}", "a".repeat(64));
        let hash = token_hash(&token);
        let jti = jti_from_hash(&hash);
        let now = Utc::now().timestamp();
        let issued_at = now - 91 * 86400; // 91 days ago
        let expires_at = now - 86400; // 1 day ago
        conn.execute(
            "INSERT INTO refresh_tokens
                (jti, token_hash, client_id, scopes, is_external, issued_at, expires_at)
             VALUES (?1, ?2, 'test', 'memory:read', 0, ?3, ?4)",
            params![jti, hash, issued_at, expires_at],
        )
        .unwrap();

        let result = authenticate_with_refresh_token(&token, dir.path());
        match result.unwrap_err() {
            AuthError::KeyExpired { days_old } => assert!(days_old >= 90),
            other => panic!("expected KeyExpired, got {other:?}"),
        }
    }

    #[test]
    fn list_returns_issued_tokens_in_reverse_order() {
        let dir = TempDir::new().unwrap();
        let (_, m1) = issue_refresh_token(dir.path(), "dev", "a", &fresh_scopes(), false).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100)); // ensure issued_at differs
        let (_, m2) = issue_refresh_token(dir.path(), "dev", "b", &fresh_scopes(), false).unwrap();

        let listed = list_tokens(dir.path()).unwrap();
        assert_eq!(listed.len(), 2);
        // Newest first.
        assert_eq!(listed[0].jti, m2.jti);
        assert_eq!(listed[1].jti, m1.jti);
    }

    #[test]
    fn jti_is_deterministic_for_a_given_token() {
        let token = format!("ddc_refresh_dev_{}", "b".repeat(64));
        let hash1 = token_hash(&token);
        let hash2 = token_hash(&token);
        assert_eq!(hash1, hash2);
        assert_eq!(jti_from_hash(&hash1), jti_from_hash(&hash2));
        assert_eq!(jti_from_hash(&hash1).len(), 16);
    }

    #[test]
    fn invalid_env_label_rejected_at_issue() {
        let dir = TempDir::new().unwrap();
        let result = issue_refresh_token(dir.path(), "evil", "x", &fresh_scopes(), false);
        assert_eq!(result.unwrap_err(), AuthError::InvalidFormat);
    }
}
