//! Rotating dashboard credentials. Only SHA-256 digests enter the identity store.
use ring::rand::{SecureRandom, SystemRandom};
use ring::hmac;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use crate::{UserDb, User, AccessLevel};

type Result<T> = std::result::Result<T, String>;
const DENIED: &str = "登入已失效，請重新登入。";
pub const DEFAULT_DAYS: u16 = 180;
mod policy;
pub fn socket_age_valid(age_secs: u64) -> bool { age_secs < 86400 }

#[derive(Debug, Serialize)]
pub struct Device {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub last_used_at: i64,
    pub expires_at: i64,
    pub ip: Option<String>,
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS remembered_policy_state (
        id INTEGER PRIMARY KEY CHECK(id=1), days INTEGER NOT NULL CHECK(days BETWEEN 1 AND 365));
        INSERT OR IGNORE INTO remembered_policy_state(id,days) VALUES(1,180);
        CREATE TABLE IF NOT EXISTS remembered_families (
        id TEXT PRIMARY KEY, user_id TEXT NOT NULL, epoch INTEGER NOT NULL, login_session TEXT NOT NULL UNIQUE,
        label TEXT NOT NULL, created_at INTEGER NOT NULL, last_used_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL, ip TEXT, revoked INTEGER NOT NULL DEFAULT 0,
        generation INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS remembered_user ON remembered_families(user_id);
        CREATE TABLE IF NOT EXISTS remembered_tokens (
        hash TEXT PRIMARY KEY, family TEXT NOT NULL REFERENCES remembered_families(id) ON DELETE CASCADE,
        used INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS remembered_token_family ON remembered_tokens(family);
        CREATE TRIGGER IF NOT EXISTS remembered_user_change AFTER UPDATE OF password_hash,credential_epoch,status,role ON users
        WHEN OLD.password_hash != NEW.password_hash OR OLD.credential_epoch != NEW.credential_epoch
            OR OLD.status != NEW.status OR OLD.role != NEW.role
        BEGIN UPDATE remembered_families SET revoked=1 WHERE user_id=NEW.id; END;
        CREATE TRIGGER IF NOT EXISTS session_status_epoch AFTER UPDATE OF status ON users
        WHEN OLD.status != NEW.status AND NEW.status != 'active'
        BEGIN UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=NEW.id; END;
        CREATE TRIGGER IF NOT EXISTS session_delete_acl AFTER DELETE ON users
        BEGIN INSERT INTO auth_meta(key,value) VALUES('acl_generation',2)
            ON CONFLICT(key) DO UPDATE SET value=value+1; END;
        CREATE TRIGGER IF NOT EXISTS remembered_user_delete BEFORE DELETE ON users
        BEGIN UPDATE remembered_families SET revoked=1 WHERE user_id=OLD.id; END;")
        .map_err(|e| e.to_string())?;
    // Unreleased prototype rows cannot authenticate generation credentials.
    if !crate::user_row::has_column(conn, "remembered_families", "generation")? {
        conn.execute_batch("ALTER TABLE remembered_families ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
            UPDATE remembered_families SET revoked=1;
            DELETE FROM remembered_tokens;").map_err(|e|e.to_string())?;
    }
    Ok(())
}
fn hash(secret: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, secret.as_bytes()).as_ref()
        .iter().map(|b| format!("{b:02x}")).collect()
}
fn audit(conn: &Connection, user: &str, action: &str, family: &str) -> Result<()> {
    conn.execute("INSERT INTO auth_audit_log(user_id,action,target,timestamp) VALUES(?1,?2,?3,?4)",
        params![user, action, family, chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
    Ok(())
}
fn active(conn: &Connection, user_id: &str, epoch: u64, policy_exempt: bool) -> Result<User> {
    let user = crate::mfa::issuance_user(conn, user_id, epoch, policy_exempt)?;
    if user.must_change_password { return Err(DENIED.into()); }
    Ok(user)
}
fn access(conn: &Connection, user: &str) -> Result<Vec<(String, AccessLevel)>> {
    let mut statement = conn.prepare("SELECT agent_name,access_level FROM user_agent_bindings WHERE user_id=?1").map_err(|e|e.to_string())?;
    let rows = statement.query_map([user], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?))).map_err(|e|e.to_string())?;
    rows.map(|row| { let (name, level) = row.map_err(|e|e.to_string())?;
        Ok((name, level.parse::<AccessLevel>().map_err(|_| DENIED)?)) }).collect()
}

/// A separate owner-only signing key keeps a users.db-only disclosure from
/// turning stored generation/hash metadata into bearer credentials.
pub(crate) fn load_key(db_path: &std::path::Path, conn: &Connection) -> Result<hmac::Key> {
    use std::io::Write;
    let path = db_path.with_extension("remember-key");
    if !path.try_exists().map_err(|e|e.to_string())? {
        let table: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='remembered_families')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
        if table && crate::user_row::has_column(conn,"remembered_families","generation")? && conn.query_row("SELECT EXISTS(SELECT 1 FROM remembered_families)",[],|r|r.get::<_,bool>(0)).map_err(|e|e.to_string())? {
            return Err("remembered session key is missing; restore the original key".into());
        }
        let mut bytes = [0u8;32];
        SystemRandom::new().fill(&mut bytes).map_err(|_|DENIED)?;
        let temp = path.with_extension(format!("remember-key-{}",uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|e|e.to_string())?;
        let result = (|| -> std::io::Result<()> {
            duduclaw_core::platform::set_owner_only(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            match std::fs::hard_link(&temp,&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(e) => Err(e),
            }
        })();
        let _ = std::fs::remove_file(&temp);
        result.map_err(|e|e.to_string())?;
        #[cfg(unix)]
        std::fs::File::open(path.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or_else(||std::path::Path::new(".")))
            .and_then(|dir|dir.sync_all()).map_err(|e|e.to_string())?;
    }
    if !std::fs::symlink_metadata(&path).map_err(|e|e.to_string())?.file_type().is_file() {
        return Err("remembered session key must be a regular file".into());
    }
    duduclaw_core::platform::set_owner_only(&path).map_err(|e|e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e|e.to_string())?;
    if bytes.len()!=32 { return Err("remembered session key is invalid".into()); }
    Ok(hmac::Key::new(hmac::HMAC_SHA256,&bytes))
}

// The key authenticates the generation, so a forged old/future generation
// cannot revoke a victim. Neither a current nor an old bearer reveals the key.
fn credential(key: &hmac::Key, family: &str, generation: u64) -> String {
    let payload = format!("{family}.{generation}");
    let tag = hmac::sign(key, payload.as_bytes());
    let mac: String = tag.as_ref().iter().map(|b|format!("{b:02x}")).collect();
    format!("{payload}.{mac}")
}
fn parts(secret: &str) -> Result<(&str, u64, Vec<u8>)> {
    let values: Vec<_> = secret.split('.').collect();
    if values.len()!=3 || uuid::Uuid::parse_str(values[0]).is_err() || values[2].len()!=64 { return Err(DENIED.into()); }
    let generation = values[1].parse::<u64>().map_err(|_|DENIED)?;
    if generation.to_string()!=values[1] { return Err(DENIED.into()); }
    let mac = values[2].as_bytes().chunks_exact(2).map(|pair| {
        let a = (pair[0] as char).to_digit(16).ok_or(DENIED)?;
        let b = (pair[1] as char).to_digit(16).ok_or(DENIED)?;
        Ok(((a << 4) | b) as u8)
    }).collect::<Result<Vec<_>>>()?;
    Ok((values[0],generation,mac))
}
fn authentic(key: &hmac::Key, family: &str, generation: u64, mac: &[u8]) -> bool {
    hmac::verify(key,
        format!("{family}.{generation}").as_bytes(),mac).is_ok()
}

/// Read-only session gate shared with derivative viewer tickets.
pub fn session_valid(conn: &Connection, session: Option<&str>, user: &str, now: i64) -> Result<bool> {
    let Some(session) = session else { return Ok(true); };
    // Unknown IDs are legacy JWT sessions. Retention covers the seven-day
    // login refresh lifetime and the enforced one-day socket lifetime.
    conn.query_row("SELECT f.user_id=?2 AND f.revoked=0 AND f.expires_at>?3 AND f.epoch=u.credential_epoch AND u.status='active' FROM remembered_families f LEFT JOIN users u ON u.id=f.user_id WHERE f.id=?1",
        params![session,user,now], |r|r.get::<_,bool>(0)).optional().map(|value|value.unwrap_or(true)).map_err(|e|e.to_string())
}

impl UserDb {
    /// Issuance and access JWT creation commit together. The caller must first prove a fresh login.
    /// `policy_exempt` is only for Personal edition; the issuer must not re-enter UserDb.
    pub fn remember_create<T>(&self, user_id: &str, epoch: u64, login_session: &str, label: &str, ip: Option<&str>, days: u16,
        now: i64, policy_exempt: bool, issue: impl FnOnce(&User, &[(String, AccessLevel)], &str) -> Result<T>) -> Result<(T, String, i64)> {
        if uuid::Uuid::parse_str(login_session).is_err() || !(1..=365).contains(&days) || now < 0 { return Err(DENIED.into()); }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let user = active(&tx, user_id, epoch, policy_exempt)?;
        tx.execute("DELETE FROM remembered_tokens WHERE family IN (SELECT id FROM remembered_families WHERE expires_at<=?1)", [now - 7*86400]).map_err(|e|e.to_string())?;
        tx.execute("DELETE FROM remembered_families WHERE expires_at<=?1", [now - 7*86400]).map_err(|e|e.to_string())?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM remembered_families WHERE user_id=?1 AND revoked=0 AND expires_at>?2", params![user_id,now], |r|r.get(0)).map_err(|e|e.to_string())?;
        let retained: i64 = tx.query_row("SELECT COUNT(*) FROM remembered_families WHERE user_id=?1", [user_id], |r|r.get(0)).map_err(|e|e.to_string())?;
        if retained >= 100 { return Err("保持登入紀錄已達上限，請先使用一般登入，待舊紀錄到期。".into()); }
        if count >= 20 { return Err("已達登入裝置上限，請先登出其他裝置。".into()); }
        // Keep the authenticated session identity so its existing WebSocket is also revocable.
        let family = login_session.to_owned();
        let secret = credential(&self.remember_key,&family,0);
        let expires = now.checked_add(i64::from(days.min(policy::days(&tx)))*86400).ok_or(DENIED)?;
        tx.execute("INSERT INTO remembered_families(id,user_id,epoch,label,created_at,last_used_at,expires_at,ip,login_session) VALUES(?1,?2,?3,?4,?5,?5,?6,?7,?8)",
            params![family,user_id,epoch,duduclaw_core::truncate_chars(label,160),now,expires,ip,login_session]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO remembered_tokens(hash,family) VALUES(?1,?2)", params![hash(&secret), family]).map_err(|e|e.to_string())?;
        let result = issue(&user, &access(&tx,user_id)?, &family)?;
        audit(&tx,user_id,"session.remembered",&family)?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok((result,secret,expires))
    }

    /// Recheck policy before issuing or rotating. `policy_exempt` is Personal-only.
    pub fn remember_rotate<T>(&self, secret: &str, now: i64, policy_exempt: bool,
        issue: impl FnOnce(&User, &[(String, AccessLevel)], &str) -> Result<T>) -> Result<(T,String,i64)> {
        if now<0 || secret.len()>160 { return Err(DENIED.into()); }
        let (family,generation,mac) = parts(secret)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let row: Option<(String,u64,i64,bool,u64)> = tx.query_row(
            "SELECT user_id,epoch,expires_at,revoked,generation FROM remembered_families WHERE id=?1",
            [family], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|e|e.to_string())?;
        let (user_id,epoch,expires,revoked,current) = row.ok_or(DENIED)?;
        if !authentic(&self.remember_key,family,generation,&mac) { return Err(DENIED.into()); }
        if revoked || expires<=now { return Err(DENIED.into()); }
        if generation != current {
            tx.execute("UPDATE remembered_families SET revoked=1 WHERE id=?1", [family]).map_err(|e|e.to_string())?;
            audit(&tx,&user_id,"session.reuse_detected",family)?;
            tx.commit().map_err(|e|e.to_string())?;
            return Err(DENIED.into());
        }
        let user = active(&tx,&user_id,epoch,policy_exempt)?;
        let expires = now.checked_add(i64::from(policy::days(&tx))*86400).ok_or(DENIED)?;
        let next = current.checked_add(1).ok_or(DENIED)?;
        let replacement = credential(&self.remember_key,family,next);
        let result = issue(&user,&access(&tx,&user_id)?,&family)?;
        tx.execute("DELETE FROM remembered_tokens WHERE family=?1", [family]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO remembered_tokens(hash,family) VALUES(?1,?2)",params![hash(&replacement),family]).map_err(|e|e.to_string())?;
        tx.execute("UPDATE remembered_families SET last_used_at=?2,generation=?3,expires_at=?4 WHERE id=?1",params![family,now,next,expires]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok((result,replacement,expires))
    }

    /// The original seven-day JWT cannot bypass a remembered family's revocation.
    pub fn remember_login_exchanged(&self, session: Option<&str>) -> Result<bool> {
        self.conn().query_row("SELECT EXISTS(SELECT 1 FROM remembered_families WHERE login_session=?1)",
            [session], |r|r.get(0)).map_err(|e|e.to_string())
    }

    pub fn remember_session_valid(&self, session: Option<&str>, user: &str, now: i64) -> Result<bool> {
        session_valid(&self.conn(), session, user, now)
    }
    pub fn remember_devices(&self,user: &str, now:i64)->Result<Vec<Device>> {
        let conn=self.conn();
        let mut statement=conn.prepare("SELECT id,label,created_at,last_used_at,expires_at,ip FROM remembered_families WHERE user_id=?1 AND revoked=0 AND expires_at>?2 ORDER BY last_used_at DESC").map_err(|e|e.to_string())?;
        statement.query_map(params![user,now],|r|Ok(Device{id:r.get(0)?,label:r.get(1)?,created_at:r.get(2)?,last_used_at:r.get(3)?,expires_at:r.get(4)?,ip:r.get(5)?}))
            .map_err(|e|e.to_string())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|e|e.to_string())
    }
    pub fn remember_revoke(&self,user:&str,family:Option<&str>)->Result<()> {
        let mut conn=self.conn();
        let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        tx.execute("UPDATE remembered_families SET revoked=1 WHERE user_id=?1 AND (?2 IS NULL OR id=?2)",params![user,family]).map_err(|e|e.to_string())?;
        if family.is_none() {
            tx.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [user]).map_err(|e|e.to_string())?;
            crate::user_row::bump_acl_generation(&tx).map_err(|e|e.to_string())?;
        }
        audit(&tx,user,"session.revoked",family.unwrap_or("all"))?;
        tx.commit().map_err(|e|e.to_string())
    }
    pub fn remember_logout(&self,secret:&str)->Result<()> {
        let (family,generation,mac) = parts(secret)?;
        let conn=self.conn();
        if !authentic(&self.remember_key,family,generation,&mac) { return Err(DENIED.into()); }
        conn.execute("UPDATE remembered_families SET revoked=1 WHERE id=?1",[family]).map_err(|e|e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
