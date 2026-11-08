//! Owner factor proof for a Personal restore request (native Linux isolation
//! v1.81, F6 design §3.4 "Personal adds", owner decision Q2, task F6d).
//!
//! `me.isolation.request_restore` / `cancel_restore` need a fresh proof of the
//! owner's **strongest** enrolled second factor, in the call itself:
//! passkey > TOTP > email code. Presenting a weaker one than the strongest
//! enrolled is refused ([`OwnerFactorError::NotStrongest`]) without touching
//! any counter or challenge; there is never a fallback to a weaker factor.
//!
//! Every proof is bound to a **purpose** `restore:<employee_id>:<incarnation>`
//! ([`restore_purpose`]) that the server derives from the row acted on:
//! - **passkey**: [`UserDb::restore_challenge`] (`me.isolation.restore_challenge`)
//!   saves a WebAuthn assertion ceremony bound to user + purpose (the passkey
//!   login's ceremony table, relying party, verifier and counter update,
//!   [`TTL`] seconds, single use); the request presents `challenge_id` and
//!   the assertion;
//! - **email**: the challenge mints a six-digit code for the account's
//!   current, verified address, sent by the email-factor sender (same send
//!   budget as login codes); stored only as a hash over challenge id, purpose
//!   and code, [`TTL`] seconds, consumed by any presentation;
//! - **TOTP**: no challenge; the code goes through the login check.
//!
//! A wrong TOTP goes through `check_factor`; a failed passkey or email proof
//! is counted the same way. Both count toward the login lockout
//! (`mfa_credentials.failed_attempts` / `mfa_factor_failures`, L1), and an
//! account at the lockout threshold is refused even with a correct proof.
//! Every attempt that reaches the proof check is committed (counter, consumed
//! challenge, audit `isolation.restore_factor_verified` / `_failed`).
use super::*;
use serde_json::Value;

/// The owner's enrolled second factors, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerFactor { Passkey, Totp, Email }

impl OwnerFactor {
    pub fn as_str(self) -> &'static str {
        match self { Self::Passkey => "passkey", Self::Totp => "totp", Self::Email => "email" }
    }
    /// Exact wire name; anything else is not a factor.
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Passkey, Self::Totp, Self::Email].into_iter().find(|factor| factor.as_str() == value)
    }
}

/// Why an owner factor proof was refused. [`Self::code`] is stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerFactorError {
    /// Not an active account at this credential epoch, a forced password
    /// change, or a malformed purpose.
    Denied,
    /// No second factor enrolled (請先設定兩步驟驗證).
    NoFactor,
    /// A stronger factor is enrolled; the request must use it.
    NotStrongest(OwnerFactor),
    /// Wrong, expired, reused or unbound proof (counted toward the login
    /// lockout), or locked out.
    Failed,
    /// The email-factor send budget is spent (≤ 5 per hour, ≥ 60 s apart).
    RateLimited,
    Unavailable,
}

impl OwnerFactorError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Denied => "FACTOR_DENIED",
            Self::NoFactor => "FACTOR_NOT_ENROLLED",
            Self::NotStrongest(_) => "FACTOR_NOT_STRONGEST",
            Self::Failed => "FACTOR_FAILED",
            Self::RateLimited => "FACTOR_RATE_LIMITED",
            Self::Unavailable => "FACTOR_UNAVAILABLE",
        }
    }
}

/// Seconds a restore challenge (passkey ceremony or email code) lives.
pub const RESTORE_CHALLENGE_TTL: i64 = TTL;
const PURPOSE_PREFIX: &str = "restore:";
/// The login lockout of `check_factor`: 5 failures in 15 minutes.
const LOCKOUT_WINDOW_SECS: i64 = 900;
const LOCKOUT_ATTEMPTS: i64 = 5;
const SEND_WINDOW_SECS: i64 = 3600;
const SENDS_PER_WINDOW: i64 = 5;
const SEND_SPACING_SECS: i64 = 60;

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS mfa_restore_email_codes (
        token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL, purpose TEXT NOT NULL, email TEXT NOT NULL,
        code_hash TEXT NOT NULL, epoch INTEGER NOT NULL, expires_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_mfa_restore_email_codes_user ON mfa_restore_email_codes(user_id, purpose);
        CREATE TRIGGER IF NOT EXISTS mfa_restore_email_codes_user_deleted AFTER DELETE ON users BEGIN
            DELETE FROM mfa_restore_email_codes WHERE user_id=OLD.id;
        END;").map_err(|e| e.to_string())
}

/// `restore:<employee_id>:<incarnation>`; `None` unless `employee_hex` is
/// exactly 32 lowercase hex digits.
pub fn restore_purpose(employee_hex: &str, incarnation: u32) -> Option<String> {
    let hex = employee_hex.len() == 32 && employee_hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    hex.then(|| format!("{PURPOSE_PREFIX}{employee_hex}:{incarnation}"))
}

/// Only the canonical form [`restore_purpose`] builds.
fn valid_purpose(purpose: &str) -> bool {
    let Some(rest) = purpose.strip_prefix(PURPOSE_PREFIX) else { return false };
    let Some((hex, incarnation)) = rest.split_once(':') else { return false };
    incarnation.parse::<u32>().is_ok_and(|n| restore_purpose(hex, n).as_deref() == Some(purpose))
}

/// What the owner presents for one proof. `purpose` comes from the server
/// (the row being acted on), never from the client.
#[derive(Debug, Clone, Copy)]
pub struct RestoreProof<'a> {
    pub purpose: &'a str,
    /// TOTP or email code.
    pub code: &'a str,
    /// Passkey / email: the id [`UserDb::restore_challenge`] returned.
    pub challenge_id: Option<&'a str>,
    /// Passkey: the WebAuthn assertion (`PublicKeyCredential` JSON).
    pub credential: Option<&'a Value>,
}

/// What [`UserDb::restore_challenge`] issued for the strongest factor.
#[derive(Debug, Clone, PartialEq)]
pub enum RestoreChallenge {
    /// WebAuthn request options (`publicKey`) with `challenge_id`.
    Passkey(Value),
    /// Deliver `code` to `email` with the email-factor sender. The code is
    /// never returned to the caller.
    Email { challenge_id: String, email: String, code: String },
    /// TOTP needs no challenge.
    Totp,
}

/// The strongest enrolled factor of `user` (`None`: none enrolled). The email
/// factor counts only for the account's current address.
pub fn strongest_factor(conn: &Connection, user: &str) -> Result<Option<OwnerFactor>> {
    let exists = |sql: &str| -> Result<bool> { conn.query_row(sql, [user], |r| r.get(0)).map_err(|e| e.to_string()) };
    if exists("SELECT EXISTS(SELECT 1 FROM mfa_passkeys WHERE user_id=?1)")? { return Ok(Some(OwnerFactor::Passkey)); }
    if exists("SELECT EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1)")? { return Ok(Some(OwnerFactor::Totp)); }
    if exists("SELECT EXISTS(SELECT 1 FROM mfa_email_credentials c JOIN users u ON u.id=c.user_id
        WHERE c.user_id=?1 AND c.email=u.email)")? { return Ok(Some(OwnerFactor::Email)); }
    Ok(None)
}

type Proof<T> = std::result::Result<T, OwnerFactorError>;

/// Any store error is `Unavailable` (never "verified").
fn unavailable<E>(_: E) -> OwnerFactorError { OwnerFactorError::Unavailable }

/// The account, active at the session's credential epoch.
fn owner_account(tx: &Connection, user: &str, epoch: u64) -> Proof<crate::User> {
    let account = crate::user_row::load_user(tx, user).map_err(unavailable)?.ok_or(OwnerFactorError::Denied)?;
    if account.id != user || account.status != crate::UserStatus::Active || account.must_change_password
        || !credential_epoch_matches(epoch, account.credential_epoch) {
        return Err(OwnerFactorError::Denied);
    }
    Ok(account)
}

/// `(failures, window start)` of the login lockout, the higher of the two
/// counters `check_factor` keeps.
fn lockout(tx: &Connection, user: &str) -> Proof<(i64, i64)> {
    let read = |sql: &str| tx.query_row(sql, [user], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).optional()
        .map_err(unavailable).map(|row| row.unwrap_or((0, 0)));
    let factor = read("SELECT attempts,window FROM mfa_factor_failures WHERE user_id=?1")?;
    let totp = read("SELECT failed_attempts,failure_window FROM mfa_credentials WHERE user_id=?1")?;
    Ok(if totp.0 > factor.0 { totp } else { factor })
}

fn locked((failures, window): (i64, i64), now: i64) -> bool {
    window > now - LOCKOUT_WINDOW_SECS && failures >= LOCKOUT_ATTEMPTS
}

/// Count one passkey / email attempt exactly as `check_factor` counts a TOTP.
fn count_attempt(tx: &Connection, user: &str, verified: bool, (failures, window): (i64, i64), now: i64) -> Proof<()> {
    let fresh = window <= now - LOCKOUT_WINDOW_SECS;
    let attempts = if verified { 0 } else if fresh { 1 } else { failures + 1 };
    let window = if fresh { now } else { window };
    tx.execute("UPDATE mfa_credentials SET failed_attempts=?2,failure_window=?3 WHERE user_id=?1", params![user, attempts, window])
        .map_err(unavailable)?;
    tx.execute("INSERT INTO mfa_factor_failures(user_id,attempts,window) VALUES(?1,?2,?3)
        ON CONFLICT(user_id) DO UPDATE SET attempts=excluded.attempts,window=excluded.window", params![user, attempts, window])
        .map_err(unavailable)?;
    Ok(())
}

fn email_code_hash(challenge: &str, purpose: &str, code: &str) -> String {
    // The challenge id is stored only as a hash: no offline six-digit dictionary.
    token_hash(&format!("restore-email:{challenge}:{purpose}:{code}"))
}

/// Consume the email code of `challenge_id` (any presentation, any purpose)
/// and check it: same user, purpose, current verified address and epoch, not
/// expired, constant-time hash match.
fn email_proof(tx: &Connection, user: &str, proof: &RestoreProof<'_>, now: i64) -> Proof<bool> {
    let Some(id) = proof.challenge_id else { return Ok(false) };
    let row: Option<(String, String, i64)> = tx.query_row("SELECT c.purpose,c.code_hash,c.expires_at
        FROM mfa_restore_email_codes c JOIN users u ON u.id=c.user_id
        WHERE c.token_hash=?1 AND c.user_id=?2 AND c.email=u.email AND c.epoch=u.credential_epoch
        AND EXISTS(SELECT 1 FROM mfa_email_credentials e WHERE e.user_id=c.user_id AND e.email=u.email)",
        params![token_hash(id), user], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(unavailable)?;
    tx.execute("DELETE FROM mfa_restore_email_codes WHERE token_hash=?1 AND user_id=?2", params![token_hash(id), user])
        .map_err(unavailable)?;
    let digits = proof.code.len() == 6 && proof.code.bytes().all(|b| b.is_ascii_digit());
    Ok(row.is_some_and(|(purpose, hash, expires_at)| purpose == proof.purpose && expires_at > now && digits
        && bool::from(hash.as_bytes().ct_eq(email_code_hash(id, proof.purpose, proof.code).as_bytes()))))
}

/// Consume the passkey ceremony and verify the assertion (passkey module).
fn passkey_proof(tx: &Connection, user: &str, proof: &RestoreProof<'_>, origin: &str, now: i64) -> bool {
    match (proof.challenge_id, proof.credential) {
        (Some(id), Some(credential)) =>
            passkey::purpose_finish(tx, origin, user, proof.purpose, id, credential.clone(), now).is_ok(),
        _ => false,
    }
}

impl UserDb {
    /// `me.isolation.restore_challenge`: issue the proof challenge of the
    /// owner's strongest factor for `purpose`. `origin` is the
    /// operator-configured dashboard origin (passkey relying party; `None` ⇒
    /// a passkey owner is refused unavailable); `email_available` is the
    /// email-factor sender's availability (checked before any code or send
    /// budget is spent).
    pub fn restore_challenge(&self, user: &str, epoch: u64, purpose: &str, origin: Option<&str>, email_available: bool,
        now: i64) -> Proof<RestoreChallenge> {
        use OwnerFactorError::*;
        if user.is_empty() || now <= 0 || !valid_purpose(purpose) { return Err(Denied); }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        let account = owner_account(&tx, user, epoch)?;
        let strongest = strongest_factor(&tx, user).map_err(unavailable)?.ok_or(NoFactor)?;
        if locked(lockout(&tx, user)?, now) { return Err(Failed); }
        let issued = match strongest {
            OwnerFactor::Totp => RestoreChallenge::Totp,
            OwnerFactor::Passkey => RestoreChallenge::Passkey(
                passkey::purpose_begin(&tx, origin.ok_or(Unavailable)?, user, purpose, now).map_err(unavailable)?),
            OwnerFactor::Email => {
                if !email_available { return Err(Unavailable); }
                // The login email factor's send budget.
                tx.execute("DELETE FROM mfa_email_sends WHERE sent_at<=?1", [now - SEND_WINDOW_SECS]).map_err(unavailable)?;
                let (count, last): (i64, Option<i64>) = tx.query_row("SELECT COUNT(*),MAX(sent_at) FROM mfa_email_sends WHERE user_id=?1",
                    [user], |r| Ok((r.get(0)?, r.get(1)?))).map_err(unavailable)?;
                if count >= SENDS_PER_WINDOW || last.is_some_and(|at| at > now - SEND_SPACING_SECS) { return Err(RateLimited); }
                let code = email::numeric_code().map_err(unavailable)?;
                let challenge_id = random_hex(32).map_err(unavailable)?;
                // One live code per purpose: a new challenge replaces the old.
                tx.execute("DELETE FROM mfa_restore_email_codes WHERE expires_at<=?1 OR (user_id=?2 AND purpose=?3)",
                    params![now, user, purpose]).map_err(unavailable)?;
                tx.execute("INSERT INTO mfa_restore_email_codes(token_hash,user_id,purpose,email,code_hash,epoch,expires_at)
                    VALUES(?1,?2,?3,?4,?5,?6,?7)", params![token_hash(&challenge_id), user, purpose, account.email,
                    email_code_hash(&challenge_id, purpose, &code), account.credential_epoch, now + RESTORE_CHALLENGE_TTL])
                    .map_err(unavailable)?;
                tx.execute("INSERT INTO mfa_email_sends(user_id,sent_at) VALUES(?1,?2)", params![user, now]).map_err(unavailable)?;
                RestoreChallenge::Email { challenge_id, email: account.email.clone(), code }
            }
        };
        audit(&tx, user, "isolation.restore_factor_challenge", None).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(issued)
    }

    /// The email code of `challenge_id` could not be delivered: drop it.
    pub fn restore_challenge_discard(&self, user: &str, challenge_id: &str) -> Result<()> {
        self.conn().execute("DELETE FROM mfa_restore_email_codes WHERE token_hash=?1 AND user_id=?2",
            params![token_hash(challenge_id), user]).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Verify the owner's strongest factor for one restore step. `epoch` is
    /// the session's credential epoch, `origin` as for
    /// [`Self::restore_challenge`]. Every attempt that reaches the proof check
    /// is committed (counter, consumed challenge, audit), success or not.
    #[allow(clippy::too_many_arguments)]
    pub fn restore_owner_factor(&self, user: &str, epoch: u64, presented: OwnerFactor, proof: &RestoreProof<'_>,
        cipher: &dyn MfaCipher, origin: Option<&str>, now: i64) -> Proof<OwnerFactor> {
        use OwnerFactorError::*;
        if user.is_empty() || proof.code.len() > 64 || now <= 0 || !valid_purpose(proof.purpose)
            || proof.challenge_id.is_some_and(|id| id.is_empty() || id.len() > 128) {
            return Err(Denied);
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        owner_account(&tx, user, epoch)?;
        let strongest = strongest_factor(&tx, user).map_err(unavailable)?.ok_or(NoFactor)?;
        if presented != strongest { return Err(NotStrongest(strongest)); }
        let verified = match strongest {
            OwnerFactor::Totp => check_factor(&tx, user, proof.code, false, cipher, now).is_ok(),
            OwnerFactor::Passkey | OwnerFactor::Email => {
                let failures = lockout(&tx, user)?;
                if locked(failures, now) { false } else {
                    let verified = match strongest {
                        OwnerFactor::Passkey => passkey_proof(&tx, user, proof, origin.ok_or(Unavailable)?, now),
                        _ => email_proof(&tx, user, proof, now)?,
                    };
                    count_attempt(&tx, user, verified, failures, now)?;
                    verified
                }
            }
        };
        audit(&tx, user, if verified { "isolation.restore_factor_verified" } else { "isolation.restore_factor_failed" }, None)
            .map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        if verified { Ok(strongest) } else { Err(Failed) }
    }
}

#[cfg(test)]
#[path = "owner_factor_tests.rs"]
mod tests;
