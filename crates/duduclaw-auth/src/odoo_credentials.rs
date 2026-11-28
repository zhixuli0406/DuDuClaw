//! Per-member Odoo credentials. Only the gateway supplies a machine sealer.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use zeroize::Zeroizing;
use crate::UserDb;

const UNAVAILABLE: &str = "目前無法讀取或保存 Odoo 連線，請稍後再試。";

pub trait OdooSecretSealer {
    fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String>;
    fn open(&self, ct: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, String>;
}

#[derive(Debug, Clone, Serialize)]
pub struct OdooCredentialStatus {
    pub login: String,
    pub odoo_uid: i64,
    pub odoo_name: String,
    pub server_fp: String,
    pub rev: i64,
    pub state: String,
    pub verified_at: String,
    pub last_used_at: Option<String>,
}

pub struct UnsealedOdooCredential {
    pub login: String,
    pub odoo_uid: i64,
    pub secret: Zeroizing<String>,
    pub rev: i64,
}
impl std::fmt::Debug for UnsealedOdooCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnsealedOdooCredential").field("login", &self.login)
            .field("odoo_uid", &self.odoo_uid).field("secret", &"[REDACTED]")
            .field("rev", &self.rev).finish()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OdooCredentialAdminEntry {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub login: String,
    pub state: String,
    pub verified_at: String,
    pub last_used_at: Option<String>,
}

pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS member_odoo_credentials (
        user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        login TEXT NOT NULL, odoo_uid INTEGER NOT NULL, odoo_name TEXT NOT NULL,
        secret_ct BLOB NOT NULL, server_fp TEXT NOT NULL,
        credential_epoch INTEGER NOT NULL DEFAULT -1,
        rev INTEGER NOT NULL, state TEXT NOT NULL CHECK (state IN ('ok','invalid','needs_reverify')),
        verified_at TEXT NOT NULL, last_used_at TEXT, updated_at TEXT NOT NULL);
        CREATE TRIGGER IF NOT EXISTS member_odoo_credentials_offboarded AFTER UPDATE OF status ON users
        WHEN NEW.status = 'offboarded' BEGIN DELETE FROM member_odoo_credentials WHERE user_id = NEW.id; END;")
        .map_err(store_error)?;
    // Probe before ALTER so reopening an upgraded database is idempotent.
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(store_error)?;
    let present: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('member_odoo_credentials')
        WHERE name='credential_epoch')", [], |r| r.get(0)).map_err(store_error)?;
    if !present {
        tx.execute_batch("ALTER TABLE member_odoo_credentials ADD COLUMN credential_epoch INTEGER NOT NULL DEFAULT -1;")
            .map_err(store_error)?;
    }
    tx.commit().map_err(store_error)
}
fn store_error(_: rusqlite::Error) -> String { UNAVAILABLE.to_owned() }
fn aad(user_id: &str) -> Vec<u8> { format!("odoo-member-v1|{user_id}").into_bytes() }
fn require_admin(conn: &Connection, id: &str) -> Result<(), String> {
    let allowed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1
        AND status='active' AND role='admin' AND must_change_password=0)", [id], |r| r.get(0)).map_err(store_error)?;
    if allowed { Ok(()) } else { Err("只有管理員可以管理同仁的 Odoo 連線。".into()) }
}

impl UserDb {
    /// Advisory count of active avatar owners with stored credentials, including
    /// credentials needing verification. Reads no secrets and changes no state.
    pub fn odoo_credential_count_active_avatars(&self) -> Result<i64, String> {
        self.conn().query_row("SELECT COUNT(*) FROM users u
            JOIN member_odoo_credentials c ON c.user_id=u.id
            WHERE u.status='active' AND u.avatar_agent_id IS NOT NULL", [], |row| row.get(0))
            .map_err(store_error)
    }

    /// This query deliberately never reads secret_ct (also used by onboarding).
    pub fn odoo_credential_status(&self, user_id: &str) -> Result<Option<OdooCredentialStatus>, String> {
        self.conn().query_row("SELECT c.login,c.odoo_uid,c.odoo_name,c.server_fp,c.rev,
            CASE WHEN c.credential_epoch < 0 OR c.credential_epoch != u.credential_epoch
                THEN 'needs_reverify' ELSE c.state END,c.verified_at,c.last_used_at
            FROM member_odoo_credentials c JOIN users u ON u.id=c.user_id WHERE c.user_id=?1", [user_id], |r| Ok(OdooCredentialStatus {
                login: r.get(0)?, odoo_uid: r.get(1)?, odoo_name: r.get(2)?, server_fp: r.get(3)?,
                rev: r.get(4)?, state: r.get(5)?, verified_at: r.get(6)?, last_used_at: r.get(7)?,
            })).optional().map_err(store_error)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn odoo_credential_put(&self, user_id: &str, login: &str, odoo_uid: i64,
        odoo_name: &str, secret: &str, server_fp: &str, sealer: &dyn OdooSecretSealer) -> Result<(), String> {
        if user_id.contains('\0') || login.is_empty() || secret.is_empty() || odoo_uid <= 0 || server_fp.is_empty() {
            return Err(UNAVAILABLE.into());
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let epoch: Option<i64> = tx.query_row("SELECT credential_epoch FROM users WHERE id=?1 AND status='active'",
            [user_id], |r| r.get(0)).optional().map_err(store_error)?;
        let epoch = epoch.filter(|epoch| *epoch >= 0).ok_or(UNAVAILABLE)?;
        let ct = sealer.seal(secret.as_bytes(), &aad(user_id)).map_err(|_| UNAVAILABLE.to_owned())?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute("INSERT INTO member_odoo_credentials
            (user_id,login,odoo_uid,odoo_name,secret_ct,server_fp,rev,state,verified_at,updated_at,credential_epoch)
            VALUES(?1,?2,?3,?4,?5,?6,1,'ok',?7,?7,?8)
            ON CONFLICT(user_id) DO UPDATE SET login=excluded.login,odoo_uid=excluded.odoo_uid,
            odoo_name=excluded.odoo_name,secret_ct=excluded.secret_ct,server_fp=excluded.server_fp,
            rev=member_odoo_credentials.rev+1,state='ok',verified_at=excluded.verified_at,
            last_used_at=NULL,updated_at=excluded.updated_at,credential_epoch=excluded.credential_epoch",
            params![user_id, login, odoo_uid, odoo_name, ct, server_fp, now, epoch]).map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    pub fn odoo_credential_mark_state(&self, user_id: &str, expected_rev: i64, state: &str) -> Result<(), String> {
        if !matches!(state, "invalid" | "needs_reverify") { return Err(UNAVAILABLE.into()); }
        self.conn().execute("UPDATE member_odoo_credentials SET state=?3,updated_at=?4 WHERE user_id=?1 AND rev=?2",
            params![user_id, expected_rev, state, chrono::Utc::now().to_rfc3339()]).map_err(store_error)?;
        Ok(())
    }
    pub fn odoo_credential_touch(&self, user_id: &str) -> Result<(), String> {
        self.conn().execute("UPDATE member_odoo_credentials SET last_used_at=?2,updated_at=?2 WHERE user_id=?1
            AND state='ok' AND credential_epoch >= 0 AND EXISTS(SELECT 1 FROM users u WHERE u.id=?1 AND u.status='active'
                AND u.credential_epoch=member_odoo_credentials.credential_epoch)",
            params![user_id, chrono::Utc::now().to_rfc3339()]).map_err(store_error)?;
        Ok(())
    }
    pub fn odoo_credential_delete(&self, user_id: &str) -> Result<bool, String> {
        self.conn().execute("DELETE FROM member_odoo_credentials WHERE user_id=?1", [user_id])
            .map(|n| n != 0).map_err(store_error)
    }
    pub fn odoo_credential_unseal(&self, user_id: &str, current_server_fp: &str,
        sealer: &dyn OdooSecretSealer) -> Result<Option<UnsealedOdooCredential>, String> {
        self.odoo_unseal_inner(user_id, Some(current_server_fp), None, sealer)
    }

    /// Explicit browser re-verification only. Never use this for an identity route.
    /// The gateway must first show the current server to this browser session.
    pub fn odoo_credential_unseal_for_reverification(&self, user_id: &str, expected_rev: i64,
        sealer: &dyn OdooSecretSealer) -> Result<Option<UnsealedOdooCredential>, String> {
        self.odoo_unseal_inner(user_id, None, Some(expected_rev), sealer)
    }
    fn odoo_unseal_inner(&self, user_id: &str, fp: Option<&str>, expected_rev: Option<i64>,
        sealer: &dyn OdooSecretSealer) -> Result<Option<UnsealedOdooCredential>, String> {
        if user_id.contains('\0') { return Err(UNAVAILABLE.into()); }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let row = tx.query_row("SELECT c.login,c.odoo_uid,c.rev,c.state,c.server_fp,c.credential_epoch,u.credential_epoch FROM member_odoo_credentials c
            JOIN users u ON u.id=c.user_id WHERE c.user_id=?1 AND u.status='active'", [user_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?))
            }).optional().map_err(store_error)?;
        let Some((login, odoo_uid, rev, state, server_fp, epoch, live_epoch)) = row else { return Ok(None); };
        if epoch < 0 || epoch != live_epoch || fp.is_some_and(|fp| fp != server_fp) {
            tx.execute("UPDATE member_odoo_credentials SET state='needs_reverify',updated_at=?2 WHERE user_id=?1",
                params![user_id, chrono::Utc::now().to_rfc3339()]).map_err(store_error)?;
            tx.commit().map_err(store_error)?;
            return Ok(None);
        }
        if (fp.is_some() && state != "ok") || expected_rev.is_some_and(|r| r != rev) { return Ok(None); }
        let ct: Vec<u8> = tx.query_row("SELECT secret_ct FROM member_odoo_credentials WHERE user_id=?1",
            [user_id], |r| r.get(0)).map_err(store_error)?;
        let plain = sealer.open(&ct, &aad(user_id)).map_err(|_| UNAVAILABLE.to_owned())?;
        let secret = Zeroizing::new(std::str::from_utf8(&plain).map_err(|_| UNAVAILABLE.to_owned())?.to_owned());
        if secret.is_empty() { return Err(UNAVAILABLE.into()); }
        tx.commit().map_err(store_error)?;
        Ok(Some(UnsealedOdooCredential { login, odoo_uid, secret, rev }))
    }

    /// CAS update: a test in flight must not resurrect a deleted or replaced credential.
    pub fn odoo_credential_reverified(&self, user_id: &str, expected_rev: i64, odoo_uid: i64,
        odoo_name: &str, server_fp: &str) -> Result<bool, String> {
        if odoo_uid <= 0 || server_fp.is_empty() { return Err(UNAVAILABLE.into()); }
        self.conn().execute("UPDATE member_odoo_credentials SET odoo_uid=?3,odoo_name=?4,server_fp=?5,
            rev=rev+1,state='ok',verified_at=?6,updated_at=?6 WHERE user_id=?1 AND rev=?2
            AND credential_epoch >= 0 AND EXISTS(SELECT 1 FROM users u WHERE u.id=?1 AND u.status='active'
                AND u.credential_epoch=member_odoo_credentials.credential_epoch)",
            params![user_id, expected_rev, odoo_uid, odoo_name, server_fp, chrono::Utc::now().to_rfc3339()])
            .map(|n| n != 0).map_err(store_error)
    }
    pub fn odoo_credentials_admin_list(&self, admin_id: &str) -> Result<Vec<OdooCredentialAdminEntry>, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(store_error)?;
        require_admin(&tx, admin_id)?;
        let mut stmt = tx.prepare("SELECT c.user_id,u.email,u.display_name,c.login,
            CASE WHEN c.credential_epoch < 0 OR c.credential_epoch != u.credential_epoch
                THEN 'needs_reverify' ELSE c.state END,c.verified_at,c.last_used_at
            FROM member_odoo_credentials c JOIN users u ON u.id=c.user_id ORDER BY c.user_id").map_err(store_error)?;
        stmt.query_map([], |r| Ok(OdooCredentialAdminEntry { user_id: r.get(0)?, email: r.get(1)?,
            display_name: r.get(2)?, login: r.get(3)?, state: r.get(4)?, verified_at: r.get(5)?, last_used_at: r.get(6)? }))
            .map_err(store_error)?.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }
    /// Check the live administrator and delete in the same write transaction.
    pub fn odoo_credential_admin_revoke(&self, admin_id: &str, user_id: &str) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        require_admin(&tx, admin_id)?;
        let changed = tx.execute("DELETE FROM member_odoo_credentials WHERE user_id=?1", [user_id]).map_err(store_error)? != 0;
        tx.commit().map_err(store_error)?;
        Ok(changed)
    }
}

#[cfg(test)]
#[path = "odoo_credentials_tests.rs"]
mod tests;
