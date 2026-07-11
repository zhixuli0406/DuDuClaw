//! Passwordless channel-OTP login + host-access bootstrap (WP12).
//!
//! Security posture (fail-closed): OTP codes are cryptographically random,
//! bound to a single challenge id, short-lived (5 min), single-use (consumed on
//! success), attempt-capped (≤5 then invalidated), and per-account rate-limited.
//! Codes are argon2-hashed at rest so a DB read cannot replay them. The
//! presentation layer (RPC) is responsible for enumeration-consistent responses
//! ("if the account exists, a code was sent") — this engine simply returns
//! `Ok(None)` when there is nothing to send, so timing/shape stays uniform.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{generate_password, hash_password, verify_password_hash, UserDb};
use crate::models::{User, UserRole};

/// OTP validity window.
pub const OTP_TTL_SECS: i64 = 300;
/// Max wrong guesses before a challenge is invalidated.
pub const OTP_MAX_ATTEMPTS: i64 = 5;
/// Rate-limit window and cap: at most `OTP_RATE_MAX` live challenges per user
/// within `OTP_RATE_WINDOW_SECS`.
pub const OTP_RATE_WINDOW_SECS: i64 = 60;
pub const OTP_RATE_MAX: usize = 3;
/// Setup-token validity window (1 hour).
pub const SETUP_TOKEN_TTL_SECS: i64 = 3600;

/// A verified (or pending) channel identity for a user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub user_id: String,
    pub channel: String,
    pub channel_user_id: String,
    pub verified: bool,
    pub created_at: String,
}

/// The result of requesting an OTP — carries the plaintext code for the caller
/// (RPC) to deliver over the channel, and a masked target for the UI. The code
/// is NEVER stored in plaintext; only its hash lives in the DB.
#[derive(Debug, Clone)]
pub struct OtpChallenge {
    pub challenge_id: String,
    pub code: String,
    pub user_id: String,
    pub channel: String,
    pub channel_user_id: String,
    pub masked_target: String,
}

/// Why an OTP verification failed. Kept coarse on purpose so the RPC can map to
/// a single generic user-facing message (no oracle for code-guessing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtpError {
    NotFound,
    Expired,
    Consumed,
    TooManyAttempts,
    BadCode,
    Internal,
}

impl std::fmt::Display for OtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::NotFound => "challenge not found",
            Self::Expired => "code expired",
            Self::Consumed => "code already used",
            Self::TooManyAttempts => "too many attempts",
            Self::BadCode => "invalid code",
            Self::Internal => "internal error",
        };
        write!(f, "{s}")
    }
}

/// Mask a channel user id for display, e.g. `12345678` → `••••5678`.
pub fn mask_target(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() <= 4 {
        return "••••".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

/// Cryptographically-random 6-digit numeric code.
fn generate_numeric_code() -> String {
    use argon2::password_hash::rand_core::RngCore;
    let mut rng = argon2::password_hash::rand_core::OsRng;
    // Rejection-sample a uniform 0..=999999 to avoid modulo bias.
    loop {
        let mut b = [0u8; 4];
        rng.fill_bytes(&mut b);
        let n = u32::from_le_bytes(b);
        // Largest multiple of 1_000_000 below u32::MAX for unbiased mapping.
        const LIMIT: u32 = (u32::MAX / 1_000_000) * 1_000_000;
        if n < LIMIT {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

impl UserDb {
    // ── Channel identity binding ─────────────────────────────────

    /// Bind (or re-bind) a channel DM identity to a user. `verified` should be
    /// true only after an exact-match confirmation handshake (WP16 discipline).
    ///
    /// Account-takeover guard (Haiku review #2): a channel identity that is
    /// already **verified** and owned by a *different* user is never silently
    /// re-pointed — that would let an attacker steal someone's login channel.
    /// Unverified / pending bindings (e.g. an admin pre-filling a known id) may
    /// still be (re)set, and the true owner may always re-bind their own.
    pub fn bind_channel_identity(
        &self,
        user_id: &str,
        channel: &str,
        channel_user_id: &str,
        verified: bool,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();

        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT user_id, verified FROM channel_identities WHERE channel = ?1 AND channel_user_id = ?2",
                params![channel, channel_user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("bind lookup failed: {e}"))?;
        if let Some((owner, ver)) = existing {
            if ver != 0 && owner != user_id {
                return Err("channel already bound to another user".to_string());
            }
        }

        conn.execute(
            "INSERT INTO channel_identities (user_id, channel, channel_user_id, verified, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(channel, channel_user_id)
             DO UPDATE SET user_id = ?1, verified = ?4",
            params![user_id, channel, channel_user_id, verified as i64, now],
        )
        .map_err(|e| format!("bind channel identity failed: {e}"))?;
        Ok(())
    }

    /// Remove a channel identity binding.
    pub fn unbind_channel_identity(&self, channel: &str, channel_user_id: &str) -> Result<(), String> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM channel_identities WHERE channel = ?1 AND channel_user_id = ?2",
            params![channel, channel_user_id],
        )
        .map_err(|e| format!("unbind failed: {e}"))?;
        Ok(())
    }

    /// Reverse lookup: which user owns this channel DM identity (verified or not)?
    pub fn find_user_id_by_channel(
        &self,
        channel: &str,
        channel_user_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT user_id FROM channel_identities WHERE channel = ?1 AND channel_user_id = ?2",
            params![channel, channel_user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("channel lookup failed: {e}"))
    }

    /// All verified channel identities for a user (login targets).
    pub fn verified_channels_for_user(&self, user_id: &str) -> Result<Vec<ChannelIdentity>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT user_id, channel, channel_user_id, verified, created_at
                 FROM channel_identities WHERE user_id = ?1 AND verified = 1
                 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare failed: {e}"))?;
        let rows = stmt
            .query_map(params![user_id], |row| {
                Ok(ChannelIdentity {
                    user_id: row.get(0)?,
                    channel: row.get(1)?,
                    channel_user_id: row.get(2)?,
                    verified: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| format!("query failed: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("row map failed: {e}"))
    }

    fn find_user_id_by_email(&self, email: &str) -> Result<Option<String>, String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id FROM users WHERE email = ?1 AND status = 'active'",
            params![email],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("email lookup failed: {e}"))
    }

    // ── OTP challenge lifecycle ──────────────────────────────────

    /// Request an OTP for an account. Returns `Ok(None)` when the account does
    /// not exist or has no verified channel (the RPC renders the same generic
    /// "code sent" message either way — enumeration-resistant). Returns
    /// `Err("rate_limited")` when the per-account window cap is exceeded.
    pub fn request_otp(&self, email: &str) -> Result<Option<OtpChallenge>, String> {
        let user_id = match self.find_user_id_by_email(email)? {
            Some(id) => id,
            // Timing-equalisation (Haiku review #4): the account-exists path runs
            // an argon2 hash of the code below; do a dummy hash here so a missing
            // account is not distinguishable by response time.
            None => {
                let _ = hash_password("timing-equalization-dummy");
                return Ok(None);
            }
        };
        let channels = self.verified_channels_for_user(&user_id)?;
        let target = match channels.into_iter().next() {
            Some(c) => c,
            None => {
                let _ = hash_password("timing-equalization-dummy");
                return Ok(None);
            }
        };

        // Per-account rate limit over the live-challenge window.
        if self.live_challenge_count(&user_id)? >= OTP_RATE_MAX {
            return Err("rate_limited".to_string());
        }

        let code = generate_numeric_code();
        let code_hash = hash_password(&code)?;
        let challenge_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires = now + Duration::seconds(OTP_TTL_SECS);

        let conn = self.conn();
        conn.execute(
            "INSERT INTO otp_challenges
                (id, user_id, code_hash, channel, channel_user_id, created_at, expires_at, attempts, consumed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0)",
            params![
                challenge_id,
                user_id,
                code_hash,
                target.channel,
                target.channel_user_id,
                now.to_rfc3339(),
                expires.to_rfc3339(),
            ],
        )
        .map_err(|e| format!("otp insert failed: {e}"))?;

        Ok(Some(OtpChallenge {
            challenge_id,
            code,
            user_id,
            channel: target.channel.clone(),
            masked_target: mask_target(&target.channel_user_id),
            channel_user_id: target.channel_user_id,
        }))
    }

    fn live_challenge_count(&self, user_id: &str) -> Result<usize, String> {
        let cutoff = Utc::now() - Duration::seconds(OTP_RATE_WINDOW_SECS);
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT created_at FROM otp_challenges WHERE user_id = ?1 AND consumed = 0")
            .map_err(|e| format!("prepare failed: {e}"))?;
        let created: Vec<String> = stmt
            .query_map(params![user_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("query failed: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("row map failed: {e}"))?;
        let count = created
            .iter()
            .filter_map(|s| DateTime::parse_from_rfc3339(s).ok())
            .filter(|t| t.with_timezone(&Utc) >= cutoff)
            .count();
        Ok(count)
    }

    /// Verify an OTP code against a challenge. On success the challenge is
    /// consumed and the user's `last_login` is updated. On the 5th wrong guess
    /// the challenge is invalidated. Always argon2-verifies to keep timing flat.
    pub fn verify_otp(&self, challenge_id: &str, code: &str) -> Result<User, OtpError> {
        let conn = self.conn();
        let row: Option<(String, String, i64, String, i64)> = conn
            .query_row(
                "SELECT user_id, code_hash, attempts, expires_at, consumed
                 FROM otp_challenges WHERE id = ?1",
                params![challenge_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|_| OtpError::Internal)?;

        let (user_id, code_hash, attempts, expires_at, consumed) = match row {
            Some(v) => v,
            None => return Err(OtpError::NotFound),
        };

        if consumed != 0 {
            return Err(OtpError::Consumed);
        }
        let expired = DateTime::parse_from_rfc3339(&expires_at)
            .map(|t| Utc::now() > t.with_timezone(&Utc))
            .unwrap_or(true);
        if expired {
            return Err(OtpError::Expired);
        }
        if attempts >= OTP_MAX_ATTEMPTS {
            let _ = conn.execute(
                "UPDATE otp_challenges SET consumed = 1 WHERE id = ?1",
                params![challenge_id],
            );
            return Err(OtpError::TooManyAttempts);
        }

        // Always verify (uniform timing regardless of correctness).
        if verify_password_hash(code, &code_hash).is_ok() {
            // Atomic compare-and-swap consume (Haiku review #1): guard on
            // `consumed = 0` so two concurrent verifications on the pooled
            // connections cannot both succeed — exactly one UPDATE flips the row.
            let affected = conn
                .execute(
                    "UPDATE otp_challenges SET consumed = 1 WHERE id = ?1 AND consumed = 0",
                    params![challenge_id],
                )
                .map_err(|_| OtpError::Internal)?;
            if affected == 0 {
                // Lost the race — another verify already consumed it.
                return Err(OtpError::Consumed);
            }
            drop(conn);
            let _ = self.update_last_login(&user_id);
            return self.get_user(&user_id).map_err(|_| OtpError::Internal)?.ok_or(OtpError::Internal);
        }

        // Record the wrong guess. On the Nth wrong guess (N = cap) the challenge
        // is invalidated and we report TooManyAttempts so the client stops
        // retrying; earlier wrong guesses report BadCode.
        let next = attempts + 1;
        if next >= OTP_MAX_ATTEMPTS {
            conn.execute(
                "UPDATE otp_challenges SET attempts = ?2, consumed = 1 WHERE id = ?1",
                params![challenge_id, next],
            )
            .map_err(|_| OtpError::Internal)?;
            return Err(OtpError::TooManyAttempts);
        }
        conn.execute(
            "UPDATE otp_challenges SET attempts = ?2 WHERE id = ?1",
            params![challenge_id, next],
        )
        .map_err(|_| OtpError::Internal)?;
        Err(OtpError::BadCode)
    }

    /// Delete expired / consumed challenges (housekeeping; safe to call anytime).
    pub fn purge_stale_otp(&self) -> Result<usize, String> {
        let conn = self.conn();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "DELETE FROM otp_challenges WHERE consumed = 1 OR expires_at < ?1",
            params![now],
        )
        .map_err(|e| format!("purge failed: {e}"))
    }

    // ── Host-access bootstrap tokens ─────────────────────────────

    /// Issue a one-time setup token. Without `force`, only issues when there are
    /// zero users (first-run). With `force` (break-glass, host access), issues
    /// regardless so a locked-out operator can always recover. Returns the
    /// plaintext token; only its hash is stored.
    pub fn issue_setup_token(&self, force: bool) -> Result<Option<String>, String> {
        if !force {
            let count: i64 = self
                .conn()
                .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
                .map_err(|e| format!("count failed: {e}"))?;
            if count > 0 {
                return Ok(None);
            }
        }
        let token = generate_password(40);
        let token_hash = hash_password(&token)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires = now + Duration::seconds(SETUP_TOKEN_TTL_SECS);
        self.conn()
            .execute(
                "INSERT INTO setup_tokens (id, token_hash, created_at, expires_at, consumed)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![id, token_hash, now.to_rfc3339(), expires.to_rfc3339()],
            )
            .map_err(|e| format!("setup token insert failed: {e}"))?;
        Ok(Some(token))
    }

    /// Claim a setup token to create the first admin (or a recovery admin). The
    /// account is created with a random throwaway password (passwordless-first:
    /// the operator logs in via channel OTP after binding a channel). Returns
    /// the created admin `User`.
    pub fn claim_setup_token(
        &self,
        token: &str,
        email: &str,
        display_name: &str,
    ) -> Result<User, String> {
        // Find a live, unconsumed token whose hash matches.
        let candidates: Vec<(String, String, String)> = {
            let conn = self.conn();
            let mut stmt = conn
                .prepare("SELECT id, token_hash, expires_at FROM setup_tokens WHERE consumed = 0")
                .map_err(|e| format!("prepare failed: {e}"))?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| format!("query failed: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("row map failed: {e}"))?
        };

        let now = Utc::now();
        let mut matched_id: Option<String> = None;
        for (id, hash, expires_at) in candidates {
            let expired = DateTime::parse_from_rfc3339(&expires_at)
                .map(|t| now > t.with_timezone(&Utc))
                .unwrap_or(true);
            if expired {
                continue;
            }
            if verify_password_hash(token, &hash).is_ok() {
                matched_id = Some(id);
                break;
            }
        }
        let token_id = matched_id.ok_or_else(|| "invalid or expired setup token".to_string())?;

        // Create the admin, then consume the token (create first so a failed
        // create leaves the token usable).
        let throwaway = generate_password(32);
        let user = self.create_user(email, display_name, &throwaway, UserRole::Admin)?;
        self.conn()
            .execute(
                "UPDATE setup_tokens SET consumed = 1 WHERE id = ?1",
                params![token_id],
            )
            .map_err(|e| format!("consume token failed: {e}"))?;
        Ok(user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UserRole;

    fn db() -> UserDb {
        // In-memory-ish: a temp file per test (UserDb requires a path).
        let dir = std::env::temp_dir().join(format!("otp-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        UserDb::new(&dir.join("auth.db")).unwrap()
    }

    fn make_user(db: &UserDb) -> User {
        db.create_user("boss@example.com", "Boss", "pw-unused-123456", UserRole::Admin)
            .unwrap()
    }

    #[test]
    fn mask_hides_all_but_last_four() {
        assert_eq!(mask_target("123456789"), "••••6789");
        assert_eq!(mask_target("12"), "••••");
    }

    #[test]
    fn numeric_code_is_six_digits() {
        for _ in 0..50 {
            let c = generate_numeric_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|ch| ch.is_ascii_digit()));
        }
    }

    #[test]
    fn request_otp_returns_none_without_verified_channel() {
        let db = db();
        let _u = make_user(&db);
        // No channel bound yet.
        assert!(db.request_otp("boss@example.com").unwrap().is_none());
        // Unknown account also None (enumeration-consistent).
        assert!(db.request_otp("nobody@example.com").unwrap().is_none());
    }

    #[test]
    fn happy_path_request_and_verify() {
        let db = db();
        let u = make_user(&db);
        db.bind_channel_identity(&u.id, "telegram", "tg-99887766", true).unwrap();

        let ch = db.request_otp("boss@example.com").unwrap().expect("challenge");
        assert_eq!(ch.channel, "telegram");
        assert_eq!(ch.masked_target, "••••7766");
        assert_eq!(ch.code.len(), 6);

        let user = db.verify_otp(&ch.challenge_id, &ch.code).expect("verify ok");
        assert_eq!(user.email, "boss@example.com");
    }

    #[test]
    fn code_is_single_use() {
        let db = db();
        let u = make_user(&db);
        db.bind_channel_identity(&u.id, "telegram", "tg-1234567", true).unwrap();
        let ch = db.request_otp("boss@example.com").unwrap().unwrap();
        db.verify_otp(&ch.challenge_id, &ch.code).unwrap();
        // Second use is rejected.
        assert_eq!(db.verify_otp(&ch.challenge_id, &ch.code), Err(OtpError::Consumed));
    }

    #[test]
    fn wrong_code_invalidates_after_five_attempts() {
        let db = db();
        let u = make_user(&db);
        db.bind_channel_identity(&u.id, "telegram", "tg-7654321", true).unwrap();
        let ch = db.request_otp("boss@example.com").unwrap().unwrap();
        let wrong = if ch.code == "000000" { "111111" } else { "000000" };
        for _ in 0..4 {
            assert_eq!(db.verify_otp(&ch.challenge_id, wrong), Err(OtpError::BadCode));
        }
        // 5th attempt trips the cap.
        assert_eq!(db.verify_otp(&ch.challenge_id, wrong), Err(OtpError::TooManyAttempts));
        // Even the correct code no longer works.
        assert_eq!(db.verify_otp(&ch.challenge_id, &ch.code), Err(OtpError::Consumed));
    }

    #[test]
    fn unknown_challenge_is_not_found() {
        let db = db();
        assert_eq!(db.verify_otp("no-such-id", "123456"), Err(OtpError::NotFound));
    }

    #[test]
    fn rate_limit_caps_live_challenges() {
        let db = db();
        let u = make_user(&db);
        db.bind_channel_identity(&u.id, "telegram", "tg-5555555", true).unwrap();
        for _ in 0..OTP_RATE_MAX {
            db.request_otp("boss@example.com").unwrap().unwrap();
        }
        assert_eq!(
            db.request_otp("boss@example.com").unwrap_err(),
            "rate_limited"
        );
    }

    #[test]
    fn setup_token_only_on_empty_then_claims_admin() {
        let db = db();
        // Zero users → token issued.
        let token = db.issue_setup_token(false).unwrap().expect("token");
        // Non-force with an existing user → None.
        let admin = db.claim_setup_token(&token, "root@example.com", "Root").unwrap();
        assert_eq!(admin.role, UserRole::Admin);
        assert!(db.issue_setup_token(false).unwrap().is_none());
        // Token is single-use.
        assert!(db.claim_setup_token(&token, "again@example.com", "Again").is_err());
    }

    #[test]
    fn cannot_hijack_verified_channel_of_another_user() {
        let db = db();
        let victim = db
            .create_user("victim@example.com", "Victim", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        db.bind_channel_identity(&victim.id, "telegram", "tg-shared", true).unwrap();
        let attacker = db
            .create_user("attacker@example.com", "Att", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        // Attacker cannot re-point the victim's VERIFIED channel to themselves.
        assert!(db
            .bind_channel_identity(&attacker.id, "telegram", "tg-shared", true)
            .is_err());
        assert_eq!(
            db.find_user_id_by_channel("telegram", "tg-shared").unwrap(),
            Some(victim.id.clone())
        );
        // The true owner may always re-bind their own identity.
        assert!(db
            .bind_channel_identity(&victim.id, "telegram", "tg-shared", true)
            .is_ok());
    }

    #[test]
    fn break_glass_force_issues_even_with_users() {
        let db = db();
        make_user(&db);
        assert!(db.issue_setup_token(false).unwrap().is_none());
        assert!(db.issue_setup_token(true).unwrap().is_some());
    }
}
