//! Persistent second-factor state. Callers supply the machine cipher; keys never enter users.db.
use hmac::{Hmac, Mac};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha1::Sha1;
use subtle::ConstantTimeEq;

use crate::{
    UserDb, UserRole,
    db::{hash_password, verify_password_hash},
};

/// A credential is valid only at the exact epoch that minted it.
pub fn credential_epoch_matches(presented: u64, current: u64) -> bool {
    presented == current
}

pub const TTL: i64 = 300;
const DENIED: &str = "雙因素驗證失敗或已過期，請重新登入。";
type Result<T> = std::result::Result<T, String>;

pub trait MfaCipher {
    fn encrypt(&self, plaintext: &str) -> Result<String>;
    fn decrypt(&self, ciphertext: &str) -> Result<String>;
}

#[derive(Clone, Copy)]
pub enum FirstFactor {
    Password,
    ChannelOtp,
}
impl FirstFactor {
    fn name(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::ChannelOtp => "channel_otp",
        }
    }
}

pub(crate) fn migrate(conn: &Connection, enforced: bool) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let conn = &*tx;
    let policy_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE lower(name)='mfa_policy_state')",
        [], |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    if !crate::user_row::has_column(conn, "users", "credential_epoch")? {
        conn.execute_batch("ALTER TABLE users ADD COLUMN credential_epoch INTEGER NOT NULL DEFAULT 0;")
            .map_err(|e| e.to_string())?;
    }
    if !crate::user_row::has_column(conn, "channel_identities", "verified_at")? {
        conn.execute_batch("ALTER TABLE channel_identities ADD COLUMN verified_at INTEGER;")
            .map_err(|e| e.to_string())?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mfa_policy_state (
        id INTEGER PRIMARY KEY CHECK(id=1), enforced INTEGER NOT NULL, enforced_at INTEGER);
        CREATE TABLE IF NOT EXISTS mfa_credentials (
        user_id TEXT PRIMARY KEY, secret_enc TEXT NOT NULL, last_step INTEGER NOT NULL,
        failed_attempts INTEGER NOT NULL DEFAULT 0, failure_window INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS mfa_pending (
        user_id TEXT PRIMARY KEY, binding TEXT NOT NULL, secret_enc TEXT NOT NULL,
        expires_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS mfa_recovery_codes (
        user_id TEXT NOT NULL, code_hash TEXT NOT NULL, PRIMARY KEY(user_id, code_hash));
        CREATE TABLE IF NOT EXISTS mfa_challenges (
        token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL, first_factor TEXT NOT NULL,
        purpose TEXT NOT NULL CHECK(purpose IN ('login','enroll')), expires_at INTEGER NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0, consumed INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS idx_mfa_challenges_user ON mfa_challenges(user_id);
        CREATE TABLE IF NOT EXISTS mfa_enrollment_proofs (
        token_hash TEXT PRIMARY KEY, session TEXT, code_hash TEXT, kind TEXT NOT NULL,
        verified INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS mfa_enrollment_codes (
        user_id TEXT PRIMARY KEY, code_hash TEXT NOT NULL, expires_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS migration_dashboard_proofs (
        user_id TEXT NOT NULL, session_hash TEXT NOT NULL, epoch INTEGER NOT NULL,
        verified_at INTEGER NOT NULL, csrf BLOB NOT NULL,
        PRIMARY KEY(user_id,session_hash));
        CREATE TABLE IF NOT EXISTS migration_step_up_attempts (
        user_id TEXT PRIMARY KEY, window INTEGER NOT NULL, attempts INTEGER NOT NULL);
        UPDATE users SET credential_epoch=1 WHERE credential_epoch=0
            AND EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=users.id);
        CREATE TRIGGER IF NOT EXISTS mfa_password_epoch AFTER UPDATE OF password_hash ON users
        WHEN OLD.password_hash != NEW.password_hash
        BEGIN
            UPDATE users SET credential_epoch=CASE WHEN credential_epoch>0 THEN credential_epoch+1 ELSE 0 END WHERE id=NEW.id;
            DELETE FROM mfa_pending WHERE user_id=NEW.id;
            DELETE FROM mfa_challenges WHERE user_id=NEW.id;
            UPDATE auth_meta SET value=value+1 WHERE key='acl_generation';
        END;",
    )
    .map_err(|e| e.to_string())?;
    if !crate::user_row::has_column(conn, "mfa_enrollment_codes", "challenge")? {
        conn.execute_batch("ALTER TABLE mfa_enrollment_codes ADD COLUMN challenge TEXT;
            DELETE FROM mfa_enrollment_codes;") .map_err(|e|e.to_string())?;
    }
    // Released v1.79.0-v1.79.2 created this table without seeding it.
    // Only migration may initialize that legacy empty state; readers stay strict.
    if !policy_exists {
        conn.execute("INSERT OR IGNORE INTO mfa_policy_state(id,enforced,enforced_at) VALUES(1,?1,?2)",
            params![enforced, enforced.then(|| chrono::Utc::now().timestamp_micros())])
            .map_err(|e| e.to_string())?;
    }
    if policy_exists && conn.query_row("SELECT COUNT(*) FROM mfa_policy_state", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())? == 0
    {
        conn.execute("INSERT INTO mfa_policy_state(id,enforced,enforced_at) VALUES(1,0,NULL)", [])
            .map_err(|e| e.to_string())?;
        tracing::warn!("Legacy unseeded MFA policy row was initialized as not enforced");
    }
    email::migrate(conn)?;
    passkey::migrate(conn)?;
    read_enforcement(conn)?;
    tx.commit().map_err(|e| e.to_string())
}

// Read under the same immediate transaction as the credential mutation.
fn policy_enforced(conn: &Connection) -> Result<bool> {
    read_enforcement(conn)
}

/// A present policy table must contain exactly one valid singleton row.
/// Do not use SQLite's permissive integer-to-bool conversion here.
pub fn read_enforcement(conn: &Connection) -> Result<bool> {
    let mut statement = conn.prepare("SELECT id, enforced FROM mfa_policy_state")
        .map_err(|e| e.to_string())?;
    let mut rows = statement.query([]).map_err(|e| e.to_string())?;
    let row = rows.next().map_err(|e| e.to_string())?
        .ok_or("MFA policy singleton missing")?;
    let id: i64 = row.get(0).map_err(|e| e.to_string())?;
    let value: i64 = row.get(1).map_err(|e| e.to_string())?;
    if id != 1 || !matches!(value, 0 | 1) || rows.next().map_err(|e| e.to_string())?.is_some() {
        return Err("MFA policy singleton invalid or duplicated".into());
    }
    Ok(value == 1)
}

/// Persist the observed policy transition; timestamps are Unix microseconds.
/// A restart/reload of true preserves the original boundary.
pub fn record_enforcement(conn: &mut Connection, enforced: bool, now: i64) -> Result<Option<i64>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
    // Schema initialization owns seeding. A policy transition must never
    // turn a missing or malformed singleton into a valid disabled policy.
    read_enforcement(&tx)?;
    tx.execute("INSERT INTO mfa_policy_state(id,enforced,enforced_at) VALUES(1,?1,?2)
        ON CONFLICT(id) DO UPDATE SET enforced=excluded.enforced,
        enforced_at=CASE WHEN excluded.enforced=0 THEN NULL WHEN mfa_policy_state.enforced=1
        THEN mfa_policy_state.enforced_at ELSE excluded.enforced_at END",
        params![enforced, enforced.then_some(now)]).map_err(|e|e.to_string())?;
    let since = tx.query_row("SELECT enforced_at FROM mfa_policy_state WHERE id=1", [], |r|r.get(0)).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e|e.to_string())?;
    Ok(since)
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut data = vec![0; bytes];
    SystemRandom::new().fill(&mut data).map_err(|_| DENIED)?;
    Ok(data.iter().map(|b| format!("{b:02x}")).collect())
}
/// Short, non-secret reference to read to the administrator (not a bearer token).
pub fn enrollment_challenge_ref(token: &str) -> String { token_hash(token)[..12].to_owned() }

fn token_hash(token: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut bits, mut acc) = (0, 0u32);
    for b in bytes {
        acc = (acc << 8) | *b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}
fn decode_secret(secret: &str) -> Result<Vec<u8>> {
    let (mut bits, mut acc) = (0, 0u32);
    let mut bytes = Vec::new();
    for c in secret.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return Err(DENIED.into()),
        };
        acc = (acc << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
        }
    }
    if bytes.len() != 20 || bits != 0 {
        return Err(DENIED.into());
    }
    Ok(bytes)
}
fn hotp(secret: &[u8], step: i64, digits: u32) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&(step as u64).to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 15) as usize;
    let n = u32::from_be_bytes(digest[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    format!("{:0width$}", n % 10u32.pow(digits), width = digits as usize)
}
fn accepted_step(secret: &str, code: &str, now: i64, last: i64) -> Result<i64> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) || now < 0 {
        return Err(DENIED.into());
    }
    let key = decode_secret(secret)?;
    let mut found = None;
    for step in [now / 30 - 1, now / 30, now / 30 + 1] {
        let equal: bool = hotp(&key, step, 6).as_bytes().ct_eq(code.as_bytes()).into();
        if equal && step >= 0 && step > last {
            found = Some(step);
        }
    }
    found.ok_or_else(|| DENIED.into())
}
fn decrypt(cipher: &dyn MfaCipher, user: &str, encrypted: &str) -> Result<String> {
    // Bind ciphertext to its user even when the underlying channel cipher has no AAD.
    let value = cipher.decrypt(encrypted)?;
    let (owner, secret): (String, String) = serde_json::from_str(&value).map_err(|_| DENIED)?;
    if owner != user {
        return Err(DENIED.into());
    }
    decode_secret(&secret)?;
    Ok(secret)
}
fn audit(tx: &Connection, user: &str, action: &str, actor: Option<&str>) -> Result<()> {
    tx.execute(
        "INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp) VALUES(?1,?2,?1,?3,?4)",
        params![user, action, actor, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
fn active_user(tx: &Connection, user: &str) -> Result<()> {
    let active: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND status='active')",
            [user],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if active { Ok(()) } else { Err(DENIED.into()) }
}
fn replace_recovery(tx: &Connection, user: &str) -> Result<Vec<String>> {
    let mut codes = Vec::new();
    tx.execute("DELETE FROM mfa_recovery_codes WHERE user_id=?1", [user])
        .map_err(|e| e.to_string())?;
    for _ in 0..10 {
        let code = random_hex(16)?;
        let hash = hash_password(&code)?;
        tx.execute(
            "INSERT INTO mfa_recovery_codes(user_id,code_hash) VALUES(?1,?2)",
            params![user, hash],
        )
        .map_err(|e| e.to_string())?;
        codes.push(code);
    }
    Ok(codes)
}
fn check_factor(
    tx: &Connection,
    user: &str,
    code: &str,
    recovery: bool,
    cipher: &dyn MfaCipher,
    now: i64,
) -> Result<()> {
    let credential: Option<(String, i64, i64, i64)> = tx.query_row(
        "SELECT secret_enc,last_step,failed_attempts,failure_window FROM mfa_credentials WHERE user_id=?1", [user],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|_| DENIED)?;
    let (enc, last, failures, window) = match credential {
        Some(value) => value,
        None if recovery && any_factor(tx, user)? => {
            let (failures, window) = tx.query_row(
                "SELECT attempts,window FROM mfa_factor_failures WHERE user_id=?1", [user],
                |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional()
                .map_err(|e|e.to_string())?.unwrap_or((0,0));
            (String::new(), -1, failures, window)
        }
        None => return Err(DENIED.into()),
    };
    if window > now - 900 && failures >= 5 {
        return Err(DENIED.into());
    }
    let verified = if recovery {
        if code.len() != 32 || !code.bytes().all(|b| b.is_ascii_hexdigit()) {
            false
        } else {
            let mut stmt = tx
                .prepare("SELECT code_hash FROM mfa_recovery_codes WHERE user_id=?1")
                .map_err(|e| e.to_string())?;
            let hashes = stmt
                .query_map([user], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            let matched = hashes
                .into_iter()
                .find(|h| verify_password_hash(code, h).is_ok());
            if let Some(hash) = matched {
                tx.execute(
                    "DELETE FROM mfa_recovery_codes WHERE user_id=?1 AND code_hash=?2",
                    params![user, hash],
                )
                .map_err(|e| e.to_string())?;
                audit(tx, user, "mfa.recovery_used", None)?;
                true
            } else {
                false
            }
        }
    } else if let Ok(step) = decrypt(cipher, user, &enc).and_then(|secret| accepted_step(&secret, code, now, last)) {
        tx.execute(
            "UPDATE mfa_credentials SET last_step=?2 WHERE user_id=?1",
            params![user, step],
        )
        .map_err(|e| e.to_string())?;
        true
    } else {
        false
    };
    tx.execute(
        "UPDATE mfa_credentials SET failed_attempts=?2,failure_window=?3 WHERE user_id=?1",
        params![
            user,
            if verified {
                0
            } else if window <= now - 900 {
                1
            } else {
                failures + 1
            },
            if window <= now - 900 { now } else { window }
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO mfa_factor_failures(user_id,attempts,window) VALUES(?1,?2,?3)
        ON CONFLICT(user_id) DO UPDATE SET attempts=excluded.attempts,window=excluded.window",
        params![user,if verified {0} else if window<=now-900 {1} else {failures+1},if window<=now-900 {now} else {window}])
        .map_err(|e|e.to_string())?;
    if verified { Ok(()) } else { Err(DENIED.into()) }
}
fn challenge_user(tx: &Connection, token: &str, purpose: &str, now: i64) -> Result<String> {
    tx.query_row("SELECT user_id FROM mfa_challenges WHERE token_hash=?1 AND purpose=?2 AND consumed=0 AND attempts<5 AND expires_at>?3",
        params![token_hash(token),purpose,now], |r| r.get(0)).map_err(|_| DENIED.into())
}

impl UserDb {
    /// Serialize legacy token issuance with enrollment and policy changes.
    /// `epoch` comes from first-factor verification; the issuer must not re-enter UserDb.
    pub fn mfa_issue_if_unenrolled<T>(
        &self,
        user: &str,
        epoch: u64,
        policy_exempt: bool,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)]) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let current = issuance_user(&tx, user, epoch, policy_exempt)?;
        let active = any_factor(&tx, user)?;
        if policy_enforced(&tx)? || active {
            return Err(DENIED.into());
        }
        let bindings = crate::org::direct_bindings(&tx, user)?.into_iter().collect::<Vec<_>>();
        let result = issue(&current, &bindings)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }

    /// The verified refresh credential's epoch proves it survived the last
    /// factor change. Check policy and session exchange while issuance is locked.
    /// The compatibility edition flag is ignored; do not re-enter UserDb.
    pub fn mfa_refresh_and_issue<T>(
        &self,
        user: &str,
        epoch: u64,
        session: Option<&str>,
        policy_exempt: bool,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)]) -> Result<T>,
    ) -> Result<T> {
        // The compatibility edition flag never exempts a stored policy.
        let _ = policy_exempt;
        self.mfa_issue_session(user, epoch, true, session, issue)
    }
    /// Enforced refresh requires current enrollment and a matching credential epoch.
    pub fn mfa_issue_refresh<T>(
        &self, user: &str, epoch: u64,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)]) -> Result<T>,
    ) -> Result<T> {
        self.mfa_issue_session(user, epoch, true, None, issue)
    }
    /// Local auto-login has no second factor, even for an enrolled owner.
    pub fn mfa_issue_local_session<T>(
        &self, user: &str, epoch: u64,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)]) -> Result<T>,
    ) -> Result<T> {
        self.mfa_issue_session(user, epoch, false, None, issue)
    }
    fn mfa_issue_session<T>(
        &self, user: &str, epoch: u64, refresh: bool, session: Option<&str>,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)]) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let required = policy_enforced(&tx)?;
        active_user(&tx, user)?;
        let enrolled = any_factor(&tx, user)?;
        if required && !(refresh && enrolled) { return Err(DENIED.into()); }
        if refresh {
            let exchanged: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM remembered_families WHERE login_session=?1)",
                [session], |r| r.get(0),
            ).map_err(|e| e.to_string())?;
            if exchanged { return Err(DENIED.into()); }
        }
        let result = issue_mfa_result(&tx, user, Vec::new(), |user, access, _| {
            if !credential_epoch_matches(epoch, user.credential_epoch)
                || (!refresh && user.must_change_password) { return Err(DENIED.into()); }
            issue(user, access)
        })?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }
    pub fn mfa_active(&self, user: &str) -> Result<bool> {
        any_factor(&self.conn(), user)
    }
    pub fn mfa_challenge(
        &self,
        user: &str,
        first: FirstFactor,
        enrollment: bool,
        now: i64,
    ) -> Result<String> {
        let token = random_hex(32)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        active_user(&tx, user)?;
        let enrolled = any_factor(&tx, user)?;
        if enrolled == enrollment {
            return Err(DENIED.into());
        }
        if enrollment {
            let recent: i64 = tx.query_row("SELECT COUNT(*) FROM mfa_challenges WHERE user_id=?1 AND purpose='enroll' AND expires_at>?2",
                params![user,now+TTL-60],|r|r.get(0)).map_err(|e|e.to_string())?;
            if recent>=3 { return Err("嘗試次數過多，請稍後再試。".into()); }
        }
        tx.execute("DELETE FROM mfa_enrollment_proofs WHERE token_hash NOT IN (SELECT token_hash FROM mfa_challenges WHERE expires_at>?1)",[now]).map_err(|e|e.to_string())?;
        tx.execute("DELETE FROM mfa_challenges WHERE expires_at<=?1", [now])
            .map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO mfa_challenges(token_hash,user_id,first_factor,purpose,expires_at) VALUES(?1,?2,?3,?4,?5)",
            params![token_hash(&token),user,first.name(),if enrollment {"enroll"} else {"login"},now+TTL]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(token)
    }
    pub fn mfa_enrollment_user(&self, token: &str, now: i64) -> Result<String> {
        challenge_user(&self.conn(), token, "enroll", now)
    }
    pub fn mfa_begin(
        &self,
        user: &str,
        enrollment: Option<&str>,
        cipher: &dyn MfaCipher,
        now: i64,
    ) -> Result<String> {
        if enrollment.is_none() { return Err(DENIED.into()); }
        let mut bytes = [0; 20];
        SystemRandom::new().fill(&mut bytes).map_err(|_| DENIED)?;
        let secret = base32(&bytes);
        let encrypted =
            cipher.encrypt(&serde_json::to_string(&(user, &secret)).map_err(|_| DENIED)?)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        active_user(&tx, user)?;
        if let Some(token) = enrollment
            && (challenge_user(&tx, token, "enroll", now)? != user || !proof_verified(&tx, token)?) {
            return Err(DENIED.into());
        }
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1)",
                [user],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if active {
            return Err("雙因素驗證已啟用。".into());
        }
        let binding = token_hash(enrollment.ok_or(DENIED)?);
        tx.execute("INSERT INTO mfa_pending(user_id,binding,secret_enc,expires_at) VALUES(?1,?2,?3,?4)
            ON CONFLICT(user_id) DO UPDATE SET binding=excluded.binding,secret_enc=excluded.secret_enc,expires_at=excluded.expires_at,attempts=0",
            params![user,binding,encrypted,now+TTL]).map_err(|e|e.to_string())?;
        audit(&tx, user, "mfa.enroll_begin", None)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(secret)
    }
    pub fn mfa_confirm(
        &self,
        user: &str,
        enrollment: Option<&str>,
        code: &str,
        cipher: &dyn MfaCipher,
        now: i64,
    ) -> Result<Vec<String>> {
        self.mfa_confirm_and_issue(user, enrollment, code, cipher, now, |_, _, codes| Ok(codes))
    }

    /// Issue the replacement session while enrollment, recovery codes and epoch
    /// are still uncommitted. An issuer error rolls back all of them.
    /// The callback must not re-enter UserDb (the connection is locked).
    #[allow(clippy::too_many_arguments)]
    pub fn mfa_confirm_and_issue<T>(
        &self,
        user: &str,
        enrollment: Option<&str>,
        code: &str,
        cipher: &dyn MfaCipher,
        now: i64,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)], Vec<String>) -> Result<T>,
    ) -> Result<T> {
        if enrollment.is_none() { return Err(DENIED.into()); }
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        // F6d (Q3): the account holder's own enrolment, stamped in this tx.
        let since = crate::credential_changes::high_water(&tx)?;
        active_user(&tx, user)?;
        if policy_enforced(&tx)? && !any_factor(&tx, user)? {
            enrollment::require_independent_proof(&tx, enrollment.ok_or(DENIED)?, now)?;
        }
        if let Some(token) = enrollment {
            if challenge_user(&tx, token, "enroll", now)? != user || !proof_verified(&tx, token)? {
                return Err(DENIED.into());
            }
            tx.execute(
                "UPDATE mfa_challenges SET attempts=attempts+1 WHERE token_hash=?1",
                [token_hash(token)],
            )
            .map_err(|e| e.to_string())?;
        }
        let binding = token_hash(enrollment.ok_or(DENIED)?);
        let encrypted: Option<String> = tx.query_row("SELECT secret_enc FROM mfa_pending WHERE user_id=?1 AND binding=?2 AND expires_at>?3 AND attempts<5",
            params![user,binding,now],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        tx.execute(
            "UPDATE mfa_pending SET attempts=attempts+1 WHERE user_id=?1 AND binding=?2",
            params![user, binding],
        )
        .map_err(|e| e.to_string())?;
        let verified = encrypted
            .as_ref()
            .ok_or_else(|| DENIED.to_string())
            .and_then(|enc| decrypt(cipher, user, enc))
            .and_then(|secret| accepted_step(&secret, code, now, -1));
        let step = match verified {
            Ok(step) => step,
            Err(_) => {
                audit(&tx, user, "mfa.verification_failed", None)?;
                tx.commit().map_err(|e| e.to_string())?;
                return Err(DENIED.into());
            }
        };
        tx.execute(
            "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,?2,?3)",
            params![user, encrypted, step],
        )
        .map_err(|_| DENIED)?;
        let has_codes: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM mfa_recovery_codes WHERE user_id=?1)", [user], |r|r.get(0)).map_err(|e|e.to_string())?;
        let codes = if has_codes { Vec::new() } else { replace_recovery(&tx, user)? };
        tx.execute("DELETE FROM mfa_pending WHERE user_id=?1", [user])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1", [user])
            .map_err(|e| e.to_string())?;
        bump_epoch(&tx, user)?;
        audit(&tx, user, "mfa.enrolled", None)?;
        stamp_self(&tx, since, user)?;
        let result = issue_mfa_result(&tx, user, codes, issue)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }
    pub fn mfa_verify(
        &self,
        token: &str,
        code: &str,
        recovery: bool,
        cipher: &dyn MfaCipher,
        now: i64,
    ) -> Result<(String, u64)> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let user = challenge_user(&tx, token, "login", now)?;
        active_user(&tx, &user)?;
        tx.execute(
            "UPDATE mfa_challenges SET attempts=attempts+1 WHERE token_hash=?1",
            [token_hash(token)],
        )
        .map_err(|e| e.to_string())?;
        let result = check_factor(&tx, &user, code, recovery, cipher, now);
        if result.is_ok() {
            tx.execute(
                "UPDATE mfa_challenges SET consumed=1 WHERE token_hash=?1",
                [token_hash(token)],
            )
            .map_err(|e| e.to_string())?;
            audit(&tx, &user, "mfa.login", None)?;
            if !recovery { set_last_factor(&tx, &user, "totp")?; }
        } else {
            audit(&tx, &user, "mfa.verification_failed", None)?;
        }
        let epoch=crate::user_row::load_user(&tx,&user)?.ok_or(DENIED)?.credential_epoch;
        tx.commit().map_err(|e| e.to_string())?;
        result.map(|_| (user,epoch))
    }
    pub fn mfa_change(
        &self,
        user: &str,
        code: &str,
        disable: bool,
        cipher: &dyn MfaCipher,
        now: i64,
    ) -> Result<Vec<String>> {
        self.mfa_change_and_issue(user, code, disable, cipher, now, |_, _, codes| Ok(codes))
    }

    /// As with enrollment, failed session issuance rolls back a disable,
    /// including factor consumption and the credential epoch.
    #[allow(clippy::too_many_arguments)]
    pub fn mfa_change_and_issue<T>(
        &self,
        user: &str,
        code: &str,
        disable: bool,
        cipher: &dyn MfaCipher,
        now: i64,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)], Vec<String>) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let since = crate::credential_changes::high_water(&tx)?;
        active_user(&tx, user)?;
        if disable && policy_enforced(&tx)? && factor_count(&tx, user)? <= 1 {
            return Err("管理員已要求雙因素驗證，無法停用。".into());
        }
        if check_factor(&tx, user, code, code.len() != 6, cipher, now).is_err() {
            audit(&tx, user, "mfa.verification_failed", None)?;
            tx.commit().map_err(|e| e.to_string())?;
            return Err(DENIED.into());
        }
        let codes = if disable {
            tx.execute("DELETE FROM mfa_credentials WHERE user_id=?1", [user]).map_err(|e|e.to_string())?;
            tx.execute("DELETE FROM mfa_pending WHERE user_id=?1", [user]).map_err(|e|e.to_string())?;
            tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1", [user]).map_err(|e|e.to_string())?;
            if !any_factor(&tx, user)? {
                tx.execute("DELETE FROM mfa_recovery_codes WHERE user_id=?1", [user]).map_err(|e|e.to_string())?;
            }
            bump_epoch(&tx, user)?;
            Vec::new()
        } else {
            replace_recovery(&tx, user)?
        };
        audit(
            &tx,
            user,
            if disable {
                "mfa.disabled"
            } else {
                "mfa.recovery_regenerated"
            },
            None,
        )?;
        stamp_self(&tx, since, user)?;
        let result = issue_mfa_result(&tx, user, codes, issue)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }
    /// Host break-glass recovery for any account.
    /// The CLI must enforce its AI-session and Bash-lane guards before calling.
    pub fn mfa_operator_reset(&self, email: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let id: String = tx
            .query_row(
                "SELECT id FROM users WHERE email=?1",
                [email],
                |r| r.get(0),
            )
            .map_err(|_| DENIED)?;
        clear(&tx, &id)?;
        audit(
            &tx,
            &id,
            "mfa.operator_reset",
            Some("本機指令列（身分未驗證）"),
        )?;
        tx.commit().map_err(|e| e.to_string())
    }
    /// Fresh role and target checks share the reset transaction.
    pub fn mfa_admin_reset(&self, actor: &str, user: &str, code: &str, cipher: &dyn MfaCipher, now: i64) -> Result<()> {
        if actor == user { return Err("不能重設自己的雙因素驗證。".into()); }
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let role: String = tx
            .query_row(
                "SELECT role FROM users WHERE id=?1 AND status='active'",
                [actor],
                |r| r.get(0),
            )
            .map_err(|_| DENIED)?;
        if role != UserRole::Admin.to_string() {
            return Err("需要目前的管理員權限。".into());
        }
        active_user(&tx, user)?;
        if check_factor(&tx, actor, code, code.len() != 6, cipher, now).is_err() {
            audit(&tx, actor, "mfa.verification_failed", None)?;
            tx.commit().map_err(|e| e.to_string())?;
            return Err(DENIED.into());
        }
        let since = crate::admin_eligibility::high_water(&tx)?;
        clear(&tx, user)?;
        crate::admin_eligibility::stamp_actor(&tx, since, actor)?;
        audit(&tx, user, "mfa.reset", Some(actor))?;
        tx.commit().map_err(|e| e.to_string())
    }
}
/// Call only inside the immediate transaction that also invokes the issuer.
/// Enrollment and factor changes bump the epoch, invalidating one-factor tokens
/// minted before enrollment. Even Personal refuses an unreadable policy store.
pub(crate) fn issuance_user(
    tx: &Connection,
    user: &str,
    epoch: u64,
    _policy_exempt: bool,
) -> Result<crate::User> {
    let current = crate::user_row::load_user(tx, user)?.ok_or(DENIED)?;
    if current.status != crate::UserStatus::Active || current.credential_epoch != epoch {
        return Err(DENIED.into());
    }
    let enforced = read_enforcement(tx)?;
    let enrolled = any_factor(tx, user)?;
    if enforced && !enrolled { return Err(DENIED.into()); }
    Ok(current)
}

fn issue_mfa_result<T>(
    tx: &Connection,
    user: &str,
    codes: Vec<String>,
    issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)], Vec<String>) -> Result<T>,
) -> Result<T> {
    let user = crate::user_row::load_user(tx, user)?.ok_or(DENIED)?;
    let bindings = crate::org::direct_bindings(tx, &user.id)?.into_iter().collect::<Vec<_>>();
    issue(&user, &bindings, codes)
}

fn bump_epoch(tx: &Connection, user: &str) -> Result<()> {
    tx.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [user]).map_err(|e|e.to_string())?;
    crate::user_row::bump_acl_generation(tx).map_err(|e|e.to_string())
}

pub(super) fn factor_count(conn: &Connection, user: &str) -> Result<i64> {
    conn.query_row("SELECT (SELECT COUNT(*) FROM mfa_credentials WHERE user_id=?1)
        + (SELECT COUNT(*) FROM mfa_email_credentials e JOIN users u ON u.id=e.user_id WHERE e.user_id=?1 AND e.email=u.email)
        + (SELECT COUNT(*) FROM mfa_passkeys WHERE user_id=?1)", [user], |r|r.get(0)).map_err(|e|e.to_string())
}
pub(crate) fn any_factor(conn: &Connection, user: &str) -> Result<bool> {
    Ok(factor_count(conn, user)? > 0)
}
/// F6d (Q3): a self-service factor writer (the account holder proved a
/// factor, a reauth or an enrolment token of their own in this call) stamps
/// its `credential_changes` rows as owner-authenticated, inside its own
/// transaction. Only rows of `user` after `since` are touched; an Admin reset
/// never goes through here and stays unattributed (refuses a restore).
pub(super) fn stamp_self(conn: &Connection, since: i64, user: &str) -> Result<()> {
    crate::credential_changes::stamp_owner(conn, since, user)
}
pub(super) fn set_last_factor(conn: &Connection, user: &str, factor: &str) -> Result<()> {
    conn.execute("INSERT INTO mfa_factor_preferences(user_id,last_used) VALUES(?1,?2)
        ON CONFLICT(user_id) DO UPDATE SET last_used=excluded.last_used", params![user,factor])
        .map_err(|e|e.to_string())?;
    Ok(())
}

pub(super) fn check_reauth(conn: &Connection, user: &str, token: &str, session: &str, now: i64) -> Result<()> {
    active_user(conn, user)?;
    if session.is_empty() || challenge_user(conn, token, "enroll", now)? != user { return Err(DENIED.into()); }
    let valid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM mfa_enrollment_proofs
        WHERE token_hash=?1 AND session=?2 AND verified=1)", params![token_hash(token),session], |r|r.get(0))
        .map_err(|e|e.to_string())?;
    if valid { Ok(()) } else { Err(DENIED.into()) }
}
fn clear(tx: &Connection, user: &str) -> Result<()> {
    bump_epoch(tx, user)?;
    for table in [
        "mfa_credentials",
        "mfa_pending",
        "mfa_recovery_codes",
        "mfa_challenges",
        "mfa_enrollment_codes",
        "mfa_email_credentials",
        "mfa_email_codes",
        "mfa_factor_preferences",
        "mfa_factor_failures",
        "mfa_passkeys",
        "mfa_passkey_ceremonies",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE user_id=?1"), [user])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Test support for other modules (F6c2 eligibility writer tests): start a
/// TOTP enrolment through the real API at `now = 3000` and return the
/// enrolment token and secret; finish with `mfa_confirm(user, Some(&token),
/// &test_totp_code(&secret, 100), cipher, 3000)`.
#[cfg(test)]
pub(crate) fn test_totp_prepare(db: &UserDb, user: &str, password: &str, cipher: &dyn MfaCipher) -> (String, String) {
    let token = db.mfa_password_challenge(user, password, "test-session", 3000).expect("enrolment challenge");
    db.mfa_prepare_enrollment(&token, false, 3000).expect("enrolment proof");
    let secret = db.mfa_begin(user, Some(&token), cipher, 3000).expect("enrolment begin");
    (token, secret)
}

/// TOTP code of `secret` at `step` (test support, see [`test_totp_prepare`]).
#[cfg(test)]
pub(crate) fn test_totp_code(secret: &str, step: i64) -> String {
    hotp(&decode_secret(secret).expect("valid secret"), step, 6)
}

mod enrollment;
mod migration;
pub use migration::MigrationConfirmationSession;
pub mod email;
pub mod passkey;
pub mod owner_factor;
pub use owner_factor::{OwnerFactor, OwnerFactorError};
use enrollment::proof_verified;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod issuance_tests;
