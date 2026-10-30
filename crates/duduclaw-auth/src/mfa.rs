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

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mfa_credentials (
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
        CREATE INDEX IF NOT EXISTS idx_mfa_challenges_user ON mfa_challenges(user_id);",
    )
    .map_err(|e| e.to_string())
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut data = vec![0; bytes];
    SystemRandom::new().fill(&mut data).map_err(|_| DENIED)?;
    Ok(data.iter().map(|b| format!("{b:02x}")).collect())
}
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
    let (enc, last, failures, window): (String, i64, i64, i64) = tx.query_row(
        "SELECT secret_enc,last_step,failed_attempts,failure_window FROM mfa_credentials WHERE user_id=?1", [user],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|_| DENIED)?;
    if window > now - 900 && failures >= 5 {
        return Err(DENIED.into());
    }
    // Recovery also fails closed when the credential is unreadable.
    let secret = decrypt(cipher, user, &enc)?;
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
    } else if let Ok(step) = accepted_step(&secret, code, now, last) {
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
    if verified { Ok(()) } else { Err(DENIED.into()) }
}
fn challenge_user(tx: &Connection, token: &str, purpose: &str, now: i64) -> Result<String> {
    tx.query_row("SELECT user_id FROM mfa_challenges WHERE token_hash=?1 AND purpose=?2 AND consumed=0 AND attempts<5 AND expires_at>?3",
        params![token_hash(token),purpose,now], |r| r.get(0)).map_err(|_| DENIED.into())
}

impl UserDb {
    /// Serialize legacy token issuance with enrollment, so activation cannot
    /// race the first-factor check and accidentally mint a one-factor session.
    pub fn mfa_issue_if_unenrolled<T>(
        &self,
        user: &str,
        issue: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        active_user(&tx, user)?;
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1)",
                [user],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if active {
            return Err(DENIED.into());
        }
        let result = issue()?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }
    pub fn mfa_active(&self, user: &str) -> Result<bool> {
        self.conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1)",
                [user],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
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
        let enrolled: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1)",
                [user],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if enrolled == enrollment {
            return Err(DENIED.into());
        }
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
        if let Some(token) = enrollment {
            if challenge_user(&tx, token, "enroll", now)? != user {
                return Err(DENIED.into());
            }
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
        let binding = enrollment
            .map(token_hash)
            .unwrap_or_else(|| "dashboard".into());
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
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        active_user(&tx, user)?;
        if let Some(token) = enrollment {
            if challenge_user(&tx, token, "enroll", now)? != user {
                return Err(DENIED.into());
            }
            tx.execute(
                "UPDATE mfa_challenges SET attempts=attempts+1 WHERE token_hash=?1",
                [token_hash(token)],
            )
            .map_err(|e| e.to_string())?;
        }
        let binding = enrollment
            .map(token_hash)
            .unwrap_or_else(|| "dashboard".into());
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
        let codes = replace_recovery(&tx, user)?;
        tx.execute("DELETE FROM mfa_pending WHERE user_id=?1", [user])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1", [user])
            .map_err(|e| e.to_string())?;
        audit(&tx, user, "mfa.enrolled", None)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(codes)
    }
    pub fn mfa_verify(
        &self,
        token: &str,
        code: &str,
        recovery: bool,
        cipher: &dyn MfaCipher,
        now: i64,
    ) -> Result<String> {
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
        } else {
            audit(&tx, &user, "mfa.verification_failed", None)?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        result.map(|_| user)
    }
    pub fn mfa_change(
        &self,
        user: &str,
        code: &str,
        disable: bool,
        cipher: &dyn MfaCipher,
        now: i64,
    ) -> Result<Vec<String>> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        active_user(&tx, user)?;
        if check_factor(&tx, user, code, code.len() != 6, cipher, now).is_err() {
            audit(&tx, user, "mfa.verification_failed", None)?;
            tx.commit().map_err(|e| e.to_string())?;
            return Err(DENIED.into());
        }
        let codes = if disable {
            clear(&tx, user)?;
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
        tx.commit().map_err(|e| e.to_string())?;
        Ok(codes)
    }
    /// Host break-glass exception: only the sole remaining active Admin.
    /// The CLI must enforce its AI-session and Bash-lane guards before calling.
    pub fn mfa_operator_reset_last_admin(&self, email: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM users WHERE role='admin' AND status='active'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if count != 1 {
            return Err("此救援指令僅供最後一位管理員使用；請由目前管理員在儀表板重設。".into());
        }
        let id: String = tx
            .query_row(
                "SELECT id FROM users WHERE email=?1 AND role='admin' AND status='active'",
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
    pub fn mfa_admin_reset(&self, actor: &str, user: &str) -> Result<()> {
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
        clear(&tx, user)?;
        audit(&tx, user, "mfa.reset", Some(actor))?;
        tx.commit().map_err(|e| e.to_string())
    }
}
fn clear(tx: &Connection, user: &str) -> Result<()> {
    for table in [
        "mfa_credentials",
        "mfa_pending",
        "mfa_recovery_codes",
        "mfa_challenges",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE user_id=?1"), [user])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
