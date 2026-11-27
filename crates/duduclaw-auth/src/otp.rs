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
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{generate_password, hash_password, verify_password_hash, UserDb};
use crate::models::User;
use crate::user_row::bump_acl_generation;

/// OTP validity window.
pub const OTP_TTL_SECS: i64 = 300;
/// Max wrong guesses before a challenge is invalidated.
pub const OTP_MAX_ATTEMPTS: i64 = 5;
/// Rate-limit window and cap: at most `OTP_RATE_MAX` live challenges per user
/// within `OTP_RATE_WINDOW_SECS`.
pub const OTP_RATE_WINDOW_SECS: i64 = 60;
pub const OTP_RATE_MAX: usize = 3;
/// Setup-token validity window (30 minutes).
pub const SETUP_TOKEN_TTL_SECS: i64 = 1800;

/// Channels on which an administrator may bind a person's channel identity
/// (`channel_identities.channel`). A binding on one of these makes the sender
/// recognisable as that dashboard account; it says nothing about whether a
/// login code can be sent there — see [`OTP_DELIVERY_CHANNELS`].
///
/// `googlechat` is bindable (RM-152) but is NOT an OTP delivery channel:
/// there is no 1:1 bot-DM sender for it (`otp_delivery::token_field`).
pub const BINDABLE_CHANNELS: [&str; 5] = ["telegram", "line", "discord", "slack", "googlechat"];

/// Channels through which a login / enrollment code can be delivered as a 1:1
/// bot DM. Must stay a subset of [`BINDABLE_CHANNELS`] and in step with
/// `duduclaw_gateway::otp_delivery::token_field`.
pub const OTP_DELIVERY_CHANNELS: [&str; 4] = ["telegram", "line", "discord", "slack"];

/// The error [`UserDb::bind_channel_identity_authorized`] returns when the
/// target account is not Active (suspended, offboarded or deleted).
pub const BIND_TARGET_NOT_ACTIVE: &str = "target user is not active";

/// Whether a verified identity on `channel` is somewhere a proactive 1:1
/// notification / DM can be addressed. A Google Chat identity
/// (`users/<id>`, RM-152) only recognises an inbound sender; outbound Chat
/// messages go to a `spaces/<id>` the app is in, so a `googlechat` row is
/// never a delivery destination. Every other channel keeps its behaviour.
///
/// Deliberately NOT [`OTP_DELIVERY_CHANNELS`]: notification delivery also
/// reaches channels that are not bindable at all (WhatsApp, Feishu, Teams
/// rows written by older versions), and those must keep receiving what they
/// received before. The two sets agree only in excluding `googlechat`.
pub fn is_dm_destination_channel(channel: &str) -> bool {
    channel != "googlechat"
}

/// Exact, whole-value membership in [`BINDABLE_CHANNELS`].
pub fn is_bindable_channel(channel: &str) -> bool {
    BINDABLE_CHANNELS.iter().any(|c| *c == channel)
}

/// Exact, whole-value membership in [`OTP_DELIVERY_CHANNELS`].
pub fn is_otp_delivery_channel(channel: &str) -> bool {
    OTP_DELIVERY_CHANNELS.iter().any(|c| *c == channel)
}

/// A verified (or pending) channel identity for a user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub user_id: String,
    pub channel: String,
    pub channel_user_id: String,
    pub verified: bool,
    pub created_at: String,
}

/// Self-confirmed chat identity and delivery destination. Times are Unix seconds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatApprovalProof {
    pub channel: String,
    pub channel_user_id: String,
    pub proof_account: String,
    pub proof_dm: String,
    pub proven_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelProofStatus {
    SelfConfirmed,
    AdminPrefilled,
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
pub(crate) fn generate_numeric_code() -> String {
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
    /// All of the caller's bindings, including unverified admin-prefilled rows.
    /// The gateway supplies the authenticated owner, never a request parameter.
    pub fn channel_identities_for_user(&self, user_id: &str) -> Result<Vec<ChannelIdentity>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT user_id, channel, channel_user_id, verified, created_at
             FROM channel_identities WHERE user_id = ?1 ORDER BY channel, channel_user_id"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([user_id], |r| Ok(ChannelIdentity {
            user_id: r.get(0)?, channel: r.get(1)?, channel_user_id: r.get(2)?,
            verified: r.get::<_, i64>(3)? != 0, created_at: r.get(4)?,
        })).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Read self-proof only; deliberately does NOT check role, Active status,
    /// or current bot configuration. The gateway must check those at use time.
    /// Multiple identities on a channel are ordered by most recent proof.
    pub fn approval_proof(&self, user_id: &str, channel: &str) -> Result<Option<ChatApprovalProof>, String> {
        Ok(self.approval_proofs(user_id)?.into_iter().find(|p| p.channel == channel))
    }

    /// All self-proven identities, including multiple accounts per channel.
    /// Like `approval_proof`, this is evidence, not live gateway authorization.
    pub fn approval_proofs(&self, user_id: &str) -> Result<Vec<ChatApprovalProof>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT channel, channel_user_id, proof_account, proof_dm, proven_at
             FROM channel_identities WHERE user_id = ?1 AND verified = 1
                AND proof_method = 'chat_handshake' AND proof_user_id = user_id
                AND proof_account IS NOT NULL AND proof_account != ''
                AND proof_dm IS NOT NULL AND proof_dm != '' AND proven_at IS NOT NULL
             ORDER BY proven_at DESC, channel, channel_user_id"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([user_id], |r| Ok(ChatApprovalProof {
            channel: r.get(0)?, channel_user_id: r.get(1)?, proof_account: r.get(2)?,
            proof_dm: r.get(3)?, proven_at: r.get(4)?,
        })).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Revoke only the authenticated caller's self-proof for this exact binding.
    /// Keep the verified binding for notifications and channel login codes.
    /// Returns false for an absent, unproven, or differently owned row.
    pub fn clear_approval_proof(
        &self, user_id: &str, channel: &str, channel_user_id: &str,
    ) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("revoke approval proof failed: {e}"))?;
        let changed = tx.execute(
            "UPDATE channel_identities SET proof_method = NULL, proof_user_id = NULL,
                proven_at = NULL, proof_account = NULL, proof_dm = NULL
             WHERE user_id = ?1 AND channel = ?2 AND channel_user_id = ?3
                AND verified = 1 AND proof_method = 'chat_handshake' AND proof_user_id = user_id
                AND proof_account IS NOT NULL AND proof_account != ''
                AND proof_dm IS NOT NULL AND proof_dm != '' AND proven_at IS NOT NULL",
            params![user_id, channel, channel_user_id],
        ).map_err(|e| format!("revoke approval proof failed: {e}"))?;
        if changed == 0 { return Ok(false); }
        bump_acl_generation(&tx).map_err(|e| format!("revoke approval proof failed: {e}"))?;
        tx.commit().map_err(|e| format!("revoke approval proof failed: {e}"))?;
        Ok(true)
    }

    /// Dashboard label for one exact row; missing rows return None. Unknown or
    /// incomplete proof never counts as self-confirmed. No role/bot checks.
    pub fn channel_identity_proof_status(
        &self, user_id: &str, channel: &str, channel_user_id: &str,
    ) -> Result<Option<ChannelProofStatus>, String> {
        self.conn().query_row(
            "SELECT CASE WHEN verified = 1 AND proof_method = 'chat_handshake'
                AND proof_user_id = user_id AND proof_account IS NOT NULL AND proof_account != ''
                AND proof_dm IS NOT NULL AND proof_dm != '' AND proven_at IS NOT NULL
                THEN 1 ELSE 0 END FROM channel_identities
             WHERE user_id = ?1 AND channel = ?2 AND channel_user_id = ?3",
            params![user_id, channel, channel_user_id],
            |r| Ok(if r.get::<_, bool>(0)? { ChannelProofStatus::SelfConfirmed }
                else { ChannelProofStatus::AdminPrefilled }),
        ).optional().map_err(|e| e.to_string())
    }

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
        self.bind_channel_identity_authorized(user_id, channel, channel_user_id, verified, None)
    }

    /// Host callers pass None. Dashboard callers must supply the verified actor.
    pub fn bind_channel_identity_authorized(
        &self, user_id: &str, channel: &str, channel_user_id: &str, verified: bool,
        actor: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        // One write transaction: the takeover check, the upsert and the ACL
        // generation bump (a channel sender's mapping is authorization input).
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("bind lookup failed: {e}"))?;

        if let Some(actor) = actor {
            let admin: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active')", [actor], |r|r.get(0)).map_err(|e|e.to_string())?;
            if !admin { return Err("admin required".into()); }
            let active = crate::mfa::any_factor(&tx, user_id)?;
            let enforced = crate::mfa::read_enforcement(&tx)?;
            if enforced && !active && actor == user_id {
                return Err("強制雙因素驗證期間，請由另一位管理員或主機操作員核准通道綁定。".into());
            }
        }
        // A channel identity is authorization input: never attach one to a
        // suspended, offboarded or deleted account (re-read in this write
        // transaction, so a concurrent suspension cannot slip between).
        let target_active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND status='active')",
                [user_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("bind lookup failed: {e}"))?;
        if !target_active {
            return Err(BIND_TARGET_NOT_ACTIVE.to_string());
        }
        let existing: Option<(String, i64)> = tx
            .query_row(
                "SELECT user_id, verified FROM channel_identities WHERE channel = ?1 AND channel_user_id = ?2",
                params![channel, channel_user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("bind lookup failed: {e}"))?;
        if matches!(&existing, Some((owner, ver)) if *ver != 0 && owner != user_id) {
            return Err("channel already bound to another user".to_string());
        }

        tx.execute(
            "INSERT INTO channel_identities (user_id, channel, channel_user_id, verified, created_at, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(channel, channel_user_id)
             DO UPDATE SET user_id = ?1, verified = ?4, verified_at = ?6,
                proof_method = CASE WHEN channel_identities.user_id = ?1 THEN proof_method ELSE NULL END,
                proof_user_id = CASE WHEN channel_identities.user_id = ?1 THEN proof_user_id ELSE NULL END,
                proven_at = CASE WHEN channel_identities.user_id = ?1 THEN proven_at ELSE NULL END,
                proof_account = CASE WHEN channel_identities.user_id = ?1 THEN proof_account ELSE NULL END,
                proof_dm = CASE WHEN channel_identities.user_id = ?1 THEN proof_dm ELSE NULL END",
            params![user_id, channel, channel_user_id, verified as i64, now, verified.then(|| Utc::now().timestamp_micros())],
        )
        .map_err(|e| format!("bind channel identity failed: {e}"))?;
        bump_acl_generation(&tx).map_err(|e| format!("bind channel identity failed: {e}"))?;
        tx.commit().map_err(|e| format!("bind channel identity failed: {e}"))
    }

    /// Remove a channel identity binding.
    pub fn unbind_channel_identity(&self, channel: &str, channel_user_id: &str) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("unbind failed: {e}"))?;
        tx.execute(
            "DELETE FROM channel_identities WHERE channel = ?1 AND channel_user_id = ?2",
            params![channel, channel_user_id],
        )
        .map_err(|e| format!("unbind failed: {e}"))?;
        bump_acl_generation(&tx).map_err(|e| format!("unbind failed: {e}"))?;
        tx.commit().map_err(|e| format!("unbind failed: {e}"))
    }

    /// Dashboard unbind: remove `user_id`'s own binding of
    /// `(channel, channel_user_id)`. `actor` must be an active administrator
    /// (re-read inside the write transaction). Returns `false` when no such
    /// binding of THAT user exists — a binding owned by someone else is never
    /// touched by a request naming a different user.
    pub fn unbind_channel_identity_authorized(
        &self, user_id: &str, channel: &str, channel_user_id: &str, actor: &str,
    ) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("unbind failed: {e}"))?;
        let admin: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active')",
                [actor],
                |r| r.get(0),
            )
            .map_err(|e| format!("unbind failed: {e}"))?;
        if !admin {
            return Err("admin required".into());
        }
        let removed = tx
            .execute(
                "DELETE FROM channel_identities WHERE channel = ?1 AND channel_user_id = ?2 AND user_id = ?3",
                params![channel, channel_user_id, user_id],
            )
            .map_err(|e| format!("unbind failed: {e}"))?;
        if removed == 0 {
            return Ok(false);
        }
        bump_acl_generation(&tx).map_err(|e| format!("unbind failed: {e}"))?;
        tx.commit().map_err(|e| format!("unbind failed: {e}"))?;
        Ok(true)
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

    /// Reverse lookup restricted to **verified** bindings — the only form
    /// admissible as an authenticated identity.
    ///
    /// [`Self::find_user_id_by_channel`] deliberately also returns *pending*
    /// (unverified) rows because the OTP flow itself must find the row it is
    /// about to verify. Any caller doing an authorization decision must use
    /// THIS method instead: an unverified row is merely "someone typed this
    /// channel id into the binding form", which must never be treated as proof
    /// of who is on the other end.
    pub fn find_verified_user_id_by_channel(
        &self,
        channel: &str,
        channel_user_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT user_id FROM channel_identities
             WHERE channel = ?1 AND channel_user_id = ?2 AND verified = 1",
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

    /// The verified identities of `user_id` that a proactive notification /
    /// DM may be addressed to ([`is_dm_destination_channel`]). Every outbound
    /// delivery list uses this; [`Self::verified_channels_for_user`] stays the
    /// "who is this person on which channel" answer (identity display,
    /// memory subjects, sender labels).
    pub fn verified_dm_destinations_for_user(&self, user_id: &str) -> Result<Vec<ChannelIdentity>, String> {
        let mut destinations: Vec<_> = self.verified_channels_for_user(user_id)?
            .into_iter().filter(|c| is_dm_destination_channel(&c.channel)).collect();
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT d.space_id, d.verified_at
            FROM verified_googlechat_dms d JOIN channel_identities i
              ON i.user_id=d.user_id AND i.channel='googlechat' AND i.channel_user_id=d.principal_id AND i.verified=1
            JOIN users u ON u.id=d.user_id AND u.status='active' WHERE d.user_id=?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([user_id], |r| Ok(ChannelIdentity {
            user_id: user_id.to_owned(), channel: "googlechat".into(), channel_user_id: r.get(0)?,
            verified: true, created_at: r.get(1)?,
        })).map_err(|e| e.to_string())?;
        for row in rows { destinations.push(row.map_err(|e| e.to_string())?); }
        Ok(destinations)
    }

    /// Call only after JWT verification and positive one-to-one classification.
    /// A typed identity/space supplied by a user or model is never evidence.
    pub fn record_googlechat_dm(&self, account: &str, space: &str, principal: &str) -> Result<(), String> {
        let conn = self.conn();
        conn.execute("INSERT INTO verified_googlechat_dms(account_id, space_id, user_id, principal_id, verified_at)
            SELECT ?1, ?2, i.user_id, ?3, ?4 FROM channel_identities i JOIN users u ON u.id=i.user_id
            WHERE i.channel='googlechat' AND i.channel_user_id=?3 AND i.verified=1 AND u.status='active'
            ON CONFLICT(account_id, space_id) DO UPDATE SET user_id=excluded.user_id,
                principal_id=excluded.principal_id, verified_at=excluded.verified_at",
            params![account, space, principal, chrono::Utc::now().to_rfc3339()]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn matches_googlechat_dm(&self, uid: &str, account: &str, space: &str, principal: &str) -> Result<bool, String> {
        self.conn().query_row("SELECT EXISTS(SELECT 1 FROM verified_googlechat_dms d
            JOIN channel_identities i ON i.user_id=d.user_id AND i.channel='googlechat'
                AND i.channel_user_id=d.principal_id AND i.verified=1
            WHERE d.user_id=?1 AND d.account_id=?2 AND d.space_id=?3 AND d.principal_id=?4)",
            params![uid, account, space, principal], |r| r.get(0)).map_err(|e| e.to_string())
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
        // Only a channel a code can actually be delivered through is a login
        // target: a bindable-only identity (Google Chat) must never be picked
        // ahead of the person's Telegram / LINE / Discord / Slack DM.
        let channels = self.verified_channels_for_user(&user_id)?;
        let target = match channels
            .into_iter()
            .find(|c| is_otp_delivery_channel(&c.channel))
        {
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

        // Always verify (uniform timing regardless of correctness). The
        // pooled connection is released for the slow argon2 check; the
        // consume below is a compare-and-swap, so nothing relies on holding it.
        drop(conn);
        let code_ok = verify_password_hash(code, &code_hash).is_ok();
        let conn = self.conn();
        if code_ok {
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
        // Increment in SQL (not from the value read above): the connection
        // was released during the hash check, so a concurrent wrong guess
        // must not be lost.
        let next: i64 = conn
            .query_row(
                "UPDATE otp_challenges SET attempts = attempts + 1 WHERE id = ?1 RETURNING attempts",
                params![challenge_id],
                |r| r.get(0),
            )
            .map_err(|_| OtpError::Internal)?;
        if next >= OTP_MAX_ATTEMPTS {
            conn.execute(
                "UPDATE otp_challenges SET consumed = 1 WHERE id = ?1",
                params![challenge_id],
            )
            .map_err(|_| OtpError::Internal)?;
            return Err(OtpError::TooManyAttempts);
        }
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
        let token = generate_password(40);
        let token_hash = hash_password(&token)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires = now + Duration::seconds(SETUP_TOKEN_TTL_SECS);
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("setup transaction failed: {e}"))?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .map_err(|e| format!("count failed: {e}"))?;
        if !force && count > 0 { return Ok(None); }
        tx.execute("UPDATE setup_tokens SET consumed = 1", [])
            .map_err(|e| format!("setup revocation failed: {e}"))?;
        tx.execute(
            "INSERT INTO setup_tokens (id, token_hash, created_at, expires_at, consumed, recovery)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![id, token_hash, now.to_rfc3339(), expires.to_rfc3339(), force],
        ).map_err(|e| format!("setup token insert failed: {e}"))?;
        tx.commit().map_err(|e| format!("setup commit failed: {e}"))?;
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
        self.claim_setup_token_inner(token, email, display_name, None)
    }

    /// Migration bootstrap: create the FIRST administrator and set its chosen
    /// password in the same token-consumption transaction. Recovery tokens are
    /// never accepted by this entry point.
    pub fn claim_first_admin_setup_token(
        &self, token: &str, email: &str, display_name: &str, password: &str,
    ) -> Result<User, String> {
        let email = crate::db::validate_account_email(email).map_err(str::to_owned)?;
        if password.chars().count() < 12 || password.len() > 1024 {
            return Err("password must be 12 characters or longer and at most 1024 bytes".into());
        }
        self.claim_setup_token_inner(token, email, display_name, Some(password))
    }

    fn claim_setup_token_inner(
        &self, token: &str, email: &str, display_name: &str, password: Option<&str>,
    ) -> Result<User, String> {
        if token.is_empty() || token.len() > 256 || email.is_empty() || email.len() > 320
            || display_name.len() > 256 {
            return Err("invalid setup request".into());
        }
        let now = Utc::now();
        // Account creation, token consumption and revocation share the write lock.
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "setup transaction unavailable".to_owned())?;
        let (window_start, attempts): (i64, i64) = tx.query_row(
            "SELECT window_start, attempts FROM setup_claim_rate WHERE id = 1", [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional().map_err(|_| "setup rate unavailable")?.unwrap_or((now.timestamp(), 0));
        let (window_start, attempts) = if now.timestamp() >= window_start.saturating_add(60) {
            (now.timestamp(), 0)
        } else { (window_start, attempts) };
        if attempts >= 5 { return Err("setup attempts limited".into()); }
        tx.execute("INSERT INTO setup_claim_rate VALUES (1, ?1, ?2)
            ON CONFLICT(id) DO UPDATE SET window_start = excluded.window_start, attempts = excluded.attempts",
            params![window_start, attempts + 1]).map_err(|_| "setup rate unavailable")?;
        let candidate: Option<(String, String, String, bool, i64)> = tx.query_row(
            "SELECT id, token_hash, expires_at, recovery, attempts FROM setup_tokens
             WHERE consumed = 0 ORDER BY created_at DESC LIMIT 1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).optional().map_err(|_| "setup token unavailable")?;
        let matched = candidate.as_ref().is_some_and(|(_, hash, expires, _, failures)| {
            *failures < 20 && DateTime::parse_from_rfc3339(expires)
                .is_ok_and(|t| t.with_timezone(&Utc) > now)
                && verify_password_hash(token, hash).is_ok()
        });
        if !matched {
            if let Some((id, _, _, _, _)) = candidate {
                tx.execute("UPDATE setup_tokens SET attempts = attempts + 1,
                    consumed = CASE WHEN attempts + 1 >= 20 THEN 1 ELSE consumed END WHERE id = ?1",
                    [id]).map_err(|_| "setup token unavailable")?;
            }
            tx.commit().map_err(|_| "setup commit unavailable")?;
            return Err("invalid or expired setup token".into());
        }
        let (token_id, _, expires, recovery, _) = candidate.ok_or("setup token unavailable")?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .map_err(|_| "setup users unavailable")?;
        if password.is_some() && recovery { return Err("recovery token refused for first administrator".into()); }
        if (!recovery || password.is_some()) && count != 0 { return Err("first administrator already exists".into()); }
        let changed = tx.execute("UPDATE setup_tokens SET consumed = 1
            WHERE id = ?1 AND consumed = 0 AND expires_at = ?2 AND expires_at > ?3",
            params![token_id, expires, Utc::now().to_rfc3339()])
            .map_err(|_| "setup token unavailable")?;
        if changed != 1 { return Err("invalid or expired setup token".into()); }
        let id = Uuid::new_v4().to_string();
        let timestamp = now.to_rfc3339();
        let password_hash = match password { Some(value) => hash_password(value)?, None => hash_password(&generate_password(32))? };
        tx.execute("INSERT INTO users (id, email, display_name, password_hash, role, status, created_at, updated_at, bootstrap_owner)
            VALUES (?1, ?2, ?3, ?4, 'admin', 'active', ?5, ?5, ?6)",
            params![id, email, display_name, password_hash, timestamp, password.is_some()])
            .map_err(|_| "setup account creation failed")?;
        tx.execute("UPDATE setup_tokens SET consumed = 1", [])
            .map_err(|_| "setup revocation failed")?;
        bump_acl_generation(&tx).map_err(|_| "setup generation failed")?;
        let user = crate::user_row::load_user(&tx, &id)?.ok_or("setup account unavailable")?;
        tx.commit().map_err(|_| "setup commit unavailable")?;
        Ok(user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UserRole;

    use crate::test_support::test_db as db;

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
        let (db, _home) = db();
        let _u = make_user(&db);
        // No channel bound yet.
        assert!(db.request_otp("boss@example.com").unwrap().is_none());
        // Unknown account also None (enumeration-consistent).
        assert!(db.request_otp("nobody@example.com").unwrap().is_none());
    }

    #[test]
    fn happy_path_request_and_verify() {
        let (db, _home) = db();
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
        let (db, _home) = db();
        let u = make_user(&db);
        db.bind_channel_identity(&u.id, "telegram", "tg-1234567", true).unwrap();
        let ch = db.request_otp("boss@example.com").unwrap().unwrap();
        db.verify_otp(&ch.challenge_id, &ch.code).unwrap();
        // Second use is rejected.
        assert_eq!(db.verify_otp(&ch.challenge_id, &ch.code), Err(OtpError::Consumed));
    }

    #[test]
    fn wrong_code_invalidates_after_five_attempts() {
        let (db, _home) = db();
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
        let (db, _home) = db();
        assert_eq!(db.verify_otp("no-such-id", "123456"), Err(OtpError::NotFound));
    }

    #[test]
    fn rate_limit_caps_live_challenges() {
        let (db, _home) = db();
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
        let (db, _home) = db();
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
        let (db, _home) = db();
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
    fn verified_only_lookup_ignores_pending_bindings() {
        let (db, _home) = db();
        let u = db
            .create_user("u@example.com", "U", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        // A pending (unverified) binding: the OTP flow must still find it…
        db.bind_channel_identity(&u.id, "telegram", "tg-pending", false).unwrap();
        assert_eq!(
            db.find_user_id_by_channel("telegram", "tg-pending").unwrap(),
            Some(u.id.clone())
        );
        // …but it is NOT an authenticated identity.
        assert_eq!(
            db.find_verified_user_id_by_channel("telegram", "tg-pending")
                .unwrap(),
            None
        );
        // Once verified, both agree.
        db.bind_channel_identity(&u.id, "telegram", "tg-pending", true).unwrap();
        assert_eq!(
            db.find_verified_user_id_by_channel("telegram", "tg-pending")
                .unwrap(),
            Some(u.id)
        );
    }

    // ── RM-152: bindable vs OTP-delivery channel lists ───────────

    #[test]
    fn googlechat_is_bindable_but_not_an_otp_delivery_channel() {
        assert!(is_bindable_channel("googlechat"));
        assert!(!is_otp_delivery_channel("googlechat"));
        // Exact whole-value membership only.
        for near in ["googlechat ", "Googlechat", "google", "googlechat:agent", ""] {
            assert!(!is_bindable_channel(near), "{near:?}");
            assert!(!is_otp_delivery_channel(near), "{near:?}");
        }
    }

    #[test]
    fn existing_channels_keep_their_membership_in_both_lists() {
        for c in ["telegram", "line", "discord", "slack"] {
            assert!(is_bindable_channel(c), "{c} must stay bindable");
            assert!(is_otp_delivery_channel(c), "{c} must stay an OTP channel");
        }
        assert_eq!(OTP_DELIVERY_CHANNELS, ["telegram", "line", "discord", "slack"]);
        assert_eq!(BINDABLE_CHANNELS.len(), OTP_DELIVERY_CHANNELS.len() + 1);
        assert!(OTP_DELIVERY_CHANNELS.iter().all(|c| is_bindable_channel(c)));
        for other in ["whatsapp", "feishu", "teams", "webchat", "widget"] {
            assert!(!is_bindable_channel(other) && !is_otp_delivery_channel(other), "{other}");
        }
    }

    #[test]
    fn otp_is_never_targeted_at_a_googlechat_binding() {
        let (db, _home) = db();
        let u = make_user(&db);
        // Bound first, so it is the oldest verified identity.
        db.bind_channel_identity(&u.id, "googlechat", "users/1234567890", true).unwrap();
        assert!(
            db.request_otp("boss@example.com").unwrap().is_none(),
            "a Google Chat-only account has no OTP login target"
        );
        db.bind_channel_identity(&u.id, "line", "U-line-1", true).unwrap();
        let ch = db.request_otp("boss@example.com").unwrap().expect("challenge");
        assert_eq!(ch.channel, "line");
    }

    #[test]
    fn non_admin_cannot_bind_or_unbind_googlechat_for_themselves() {
        let (db, _home) = db();
        let _admin = make_user(&db);
        let emp = db
            .create_user("emp@example.com", "Emp", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        assert_eq!(
            db.bind_channel_identity_authorized(&emp.id, "googlechat", "users/42", true, Some(&emp.id))
                .unwrap_err(),
            "admin required"
        );
        assert_eq!(db.find_user_id_by_channel("googlechat", "users/42").unwrap(), None);
        db.bind_channel_identity(&emp.id, "googlechat", "users/42", true).unwrap();
        assert_eq!(
            db.unbind_channel_identity_authorized(&emp.id, "googlechat", "users/42", &emp.id)
                .unwrap_err(),
            "admin required"
        );
        assert_eq!(
            db.find_verified_user_id_by_channel("googlechat", "users/42").unwrap(),
            Some(emp.id)
        );
    }

    #[test]
    fn admin_unbind_only_removes_the_named_users_binding() {
        let (db, _home) = db();
        let admin = make_user(&db);
        let a = db
            .create_user("a@example.com", "A", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        let b = db
            .create_user("b@example.com", "B", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        db.bind_channel_identity_authorized(&a.id, "googlechat", "users/7", true, Some(&admin.id))
            .unwrap();
        // A request naming another user leaves A's binding alone.
        assert!(!db
            .unbind_channel_identity_authorized(&b.id, "googlechat", "users/7", &admin.id)
            .unwrap());
        assert_eq!(
            db.find_verified_user_id_by_channel("googlechat", "users/7").unwrap(),
            Some(a.id.clone())
        );
        assert!(db
            .unbind_channel_identity_authorized(&a.id, "googlechat", "users/7", &admin.id)
            .unwrap());
        assert_eq!(db.find_verified_user_id_by_channel("googlechat", "users/7").unwrap(), None);
    }

    #[test]
    fn googlechat_id_has_at_most_one_verified_owner() {
        let (db, _home) = db();
        let admin = make_user(&db);
        let a = db
            .create_user("a@example.com", "A", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        let b = db
            .create_user("b@example.com", "B", "pw-unused-123456", UserRole::Employee)
            .unwrap();
        db.bind_channel_identity_authorized(&a.id, "googlechat", "users/99", true, Some(&admin.id))
            .unwrap();
        assert_eq!(
            db.bind_channel_identity_authorized(&b.id, "googlechat", "users/99", true, Some(&admin.id))
                .unwrap_err(),
            "channel already bound to another user"
        );
        assert_eq!(
            db.find_verified_user_id_by_channel("googlechat", "users/99").unwrap(),
            Some(a.id)
        );
    }

    #[test]
    fn binding_requires_an_active_target_account() {
        use crate::models::UserStatus;
        for status in [UserStatus::Suspended, UserStatus::Offboarded] {
            let (db, _home) = db();
            let admin = make_user(&db);
            let t = db
                .create_user("t@example.com", "T", "pw-unused-123456", UserRole::Employee)
                .unwrap();
            db.set_user_status(&t.id, status).unwrap();
            for channel in ["googlechat", "telegram"] {
                let id = if channel == "googlechat" { "users/5150" } else { "tg-5150" };
                assert_eq!(
                    db.bind_channel_identity_authorized(&t.id, channel, id, true, Some(&admin.id))
                        .unwrap_err(),
                    BIND_TARGET_NOT_ACTIVE,
                    "{status:?} {channel}"
                );
                // The host path enforces the same rule.
                assert_eq!(
                    db.bind_channel_identity(&t.id, channel, id, true).unwrap_err(),
                    BIND_TARGET_NOT_ACTIVE
                );
                assert_eq!(db.find_user_id_by_channel(channel, id).unwrap(), None);
            }
        }
        // A deleted (never existing) account.
        let (db, _home) = db();
        let admin = make_user(&db);
        assert_eq!(
            db.bind_channel_identity_authorized("no-such-user", "googlechat", "users/1", true, Some(&admin.id))
                .unwrap_err(),
            BIND_TARGET_NOT_ACTIVE
        );
        assert_eq!(db.find_user_id_by_channel("googlechat", "users/1").unwrap(), None);
    }

    #[test]
    fn a_googlechat_identity_is_never_a_dm_destination() {
        assert!(!is_dm_destination_channel("googlechat"));
        for c in ["telegram", "line", "discord", "slack", "whatsapp", "feishu", "teams"] {
            assert!(is_dm_destination_channel(c), "{c}");
        }
        let (db, _home) = db();
        let u = make_user(&db);
        db.bind_channel_identity(&u.id, "googlechat", "users/31337", true).unwrap();
        assert!(db.verified_dm_destinations_for_user(&u.id).unwrap().is_empty());
        db.bind_channel_identity(&u.id, "line", "U-31337", true).unwrap();
        let dests = db.verified_dm_destinations_for_user(&u.id).unwrap();
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].channel, "line");
        // The identity itself is still listed as an identity.
        assert_eq!(db.verified_channels_for_user(&u.id).unwrap().len(), 2);
    }

    #[test]
    fn setup_claim_is_atomic_across_connections() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let first = UserDb::new(&path).unwrap();
        let token = first.issue_setup_token(false).unwrap().unwrap();
        // Independent pools share only the SQLite file, not a process mutex.
        let second = UserDb::new(&path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [first, second].into_iter().enumerate().map(|(index, db)| {
            let (token, barrier) = (token.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                db.claim_setup_token(&token, &format!("admin{index}@example.com"), "Admin").is_ok()
            })
        }).collect();
        let successes = threads.into_iter().map(|thread| thread.join().unwrap()).filter(|ok| *ok).count();
        assert_eq!(successes, 1);
        let db = UserDb::new(&path).unwrap();
        let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
        let live: i64 = db.conn().query_row("SELECT COUNT(*) FROM setup_tokens WHERE consumed = 0", [], |r| r.get(0)).unwrap();
        assert_eq!(live, 0);
    }

    #[test]
    fn setup_legacy_schema_upgrade_revokes_unbound_tokens() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let token = generate_password(40);
        let now = Utc::now();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE setup_tokens (
                id TEXT PRIMARY KEY, token_hash TEXT NOT NULL,
                created_at TEXT NOT NULL, expires_at TEXT NOT NULL,
                consumed INTEGER NOT NULL DEFAULT 0
            );").unwrap();
            conn.execute("INSERT INTO setup_tokens VALUES ('legacy', ?1, ?2, ?3, 0)",
                params![hash_password(&token).unwrap(), now.to_rfc3339(),
                    (now + Duration::seconds(3600)).to_rfc3339()]).unwrap();
        }
        let db = UserDb::new(&path).unwrap();
        let row: (i64, i64, i64) = db.conn().query_row(
            "SELECT consumed, recovery, attempts FROM setup_tokens WHERE id = 'legacy'", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(row, (1, 0, 0));
        assert!(db.claim_setup_token(&token, "legacy@example.com", "Legacy").is_err());
        let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn setup_issued_token_preserves_recovery_purpose_on_reopen() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        let first = db.issue_setup_token(false).unwrap().unwrap();
        let purpose: i64 = db.conn().query_row(
            "SELECT recovery FROM setup_tokens WHERE consumed = 0", [], |r| r.get(0)).unwrap();
        assert_eq!(purpose, 0);
        db.claim_setup_token(&first, "first@example.com", "First").unwrap();
        let recovery = db.issue_setup_token(true).unwrap().unwrap();
        drop(db);
        let reopened = UserDb::new(&path).unwrap();
        let purpose: i64 = reopened.conn().query_row(
            "SELECT recovery FROM setup_tokens WHERE consumed = 0", [], |r| r.get(0)).unwrap();
        assert_eq!(purpose, 1);
        assert_eq!(reopened.claim_setup_token(&recovery, "recovered@example.com", "Recovered").unwrap().role,
            UserRole::Admin);
        let count: i64 = reopened.conn().query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn setup_new_token_revokes_old_and_failed_insert_rolls_back() {
        let (db, _home) = db();
        let old = db.issue_setup_token(false).unwrap().unwrap();
        let current = db.issue_setup_token(false).unwrap().unwrap();
        assert!(db.claim_setup_token(&old, "first@example.com", "First").is_err());
        db.claim_setup_token(&current, "first@example.com", "First").unwrap();
        let recovery = db.issue_setup_token(true).unwrap().unwrap();
        assert!(db.claim_setup_token(&recovery, "first@example.com", "Duplicate").is_err());
        assert!(db.claim_setup_token(&recovery, "recovery@example.com", "Recovery").is_ok());
    }

    #[test]
    fn setup_first_admin_claim_refuses_intervening_user() {
        let (db, _home) = db();
        let token = db.issue_setup_token(false).unwrap().unwrap();
        make_user(&db);
        assert!(db.claim_setup_token(&token, "root@example.com", "Root").is_err());
    }

    #[test]
    fn setup_expiry_and_attempt_limits_fail_closed() {
        let (db, _home) = db();
        let token = db.issue_setup_token(false).unwrap().unwrap();
        db.conn().execute("UPDATE setup_tokens SET expires_at = ?1", [Utc::now().to_rfc3339()]).unwrap();
        assert!(db.claim_setup_token(&token, "root@example.com", "Root").is_err());
        let token = db.issue_setup_token(false).unwrap().unwrap();
        db.conn().execute("DELETE FROM setup_claim_rate", []).unwrap();
        for _ in 0..5 { assert!(db.claim_setup_token("wrong", "root@example.com", "Root").is_err()); }
        assert_eq!(db.claim_setup_token(&token, "root@example.com", "Root").unwrap_err(), "setup attempts limited");
        db.conn().execute("DELETE FROM setup_claim_rate", []).unwrap();
        db.conn().execute("UPDATE setup_tokens SET attempts = 19 WHERE consumed = 0", []).unwrap();
        assert!(db.claim_setup_token("wrong", "root@example.com", "Root").is_err());
        assert!(db.claim_setup_token(&token, "root@example.com", "Root").is_err());
    }

    #[test]
    fn break_glass_force_issues_even_with_users() {
        let (db, _home) = db();
        make_user(&db);
        assert!(db.issue_setup_token(false).unwrap().is_none());
        assert!(db.issue_setup_token(true).unwrap().is_some());
    }
    #[test]
    fn migration_first_admin_claim_sets_password_atomically_and_rejects_replay() {
        let (db, _home) = db();
        let token = db.issue_setup_token(false).unwrap().unwrap();
        assert!(db.claim_first_admin_setup_token(&token, "first@example.com", "First", "short").is_err());
        let user = db.claim_first_admin_setup_token(&token, "first@example.com", "First", "new-password-123").unwrap();
        assert_eq!(user.role, UserRole::Admin);
        assert!(db.verify_password("first@example.com", "new-password-123").is_ok());
        assert!(db.claim_first_admin_setup_token(&token, "second@example.com", "Second", "new-password-123").is_err());
        let recovery = db.issue_setup_token(true).unwrap().unwrap();
        assert!(db.claim_first_admin_setup_token(&recovery, "second@example.com", "Second", "new-password-123").is_err());
    }

}

#[cfg(test)]
mod verified_googlechat_dm_tests {
    use super::*;

    #[test]
    fn dm_destination_requires_verified_identity_and_is_revoked_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::UserDb::new(&dir.path().join("users.db")).unwrap();
        let user = db.create_user("owner@example.com", "Owner", "test-password", crate::UserRole::Employee).unwrap();
        db.bind_channel_identity(&user.id, "googlechat", "users/123", false).unwrap();
        db.record_googlechat_dm("app@example.com", "spaces/DM1", "users/123").unwrap();
        assert!(db.verified_dm_destinations_for_user(&user.id).unwrap().is_empty());
        db.bind_channel_identity(&user.id, "googlechat", "users/123", true).unwrap();
        db.record_googlechat_dm("app@example.com", "spaces/DM1", "users/123").unwrap();
        let targets = db.verified_dm_destinations_for_user(&user.id).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].channel_user_id, "spaces/DM1");
        assert!(db.matches_googlechat_dm(&user.id, "app@example.com", "spaces/DM1", "users/123").unwrap());
        assert!(!db.matches_googlechat_dm(&user.id, "other@example.com", "spaces/DM1", "users/123").unwrap());
        db.bind_channel_identity(&user.id, "googlechat", "users/123", false).unwrap();
        assert!(db.verified_dm_destinations_for_user(&user.id).unwrap().is_empty());
    }
}
