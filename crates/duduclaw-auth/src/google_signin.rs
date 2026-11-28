//! Google sign-in storage. Callers own authorization and policy decisions.
//! All INTEGER timestamps are Unix seconds. Transaction helpers never commit.
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::Serialize;
use crate::UserDb;

pub const SIGNIN_REQUEST_TTL_SECS: i64 = 14 * 24 * 60 * 60;
pub const SIGNIN_PENDING_MAX: u32 = 50;
pub const SIGNIN_ATTESTATION_TTL_SECS: i64 = 14 * 24 * 60 * 60;
pub const SIGNIN_REJECT_COOLDOWN_SECS: i64 = 7 * 24 * 60 * 60;

pub fn valid_google_sub(s: &str) -> bool {
    (1..=255).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}
const STORAGE: &str = "signin_storage_unavailable";
fn error(_: rusqlite::Error) -> String { STORAGE.into() }

pub(crate) fn migration_needed(conn: &Connection) -> Result<bool, String> {
    for table in ["google_signin_links", "google_signin_requests", "google_signin_attestations"] {
        if !crate::authority_migrations::schema_object_exists(conn, "table", table)? { return Ok(true); }
    }
    Ok(false)
}
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS google_signin_links (
        user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        google_sub TEXT NOT NULL UNIQUE CHECK(google_sub GLOB '[0-9]*' AND google_sub NOT GLOB '*[^0-9]*' AND length(google_sub) BETWEEN 1 AND 255 AND instr(google_sub,char(0))=0),
        hd TEXT NOT NULL, email_at_link TEXT NOT NULL,
        linked_via TEXT NOT NULL CHECK(linked_via IN ('jit_direct','jit_approved','self_link','admin_attest')),
        linked_by TEXT NOT NULL, linked_at INTEGER NOT NULL, last_signin_at INTEGER
    );
    CREATE TABLE IF NOT EXISTS google_signin_requests (
        id TEXT PRIMARY KEY,
        google_sub TEXT NOT NULL UNIQUE CHECK(google_sub GLOB '[0-9]*' AND google_sub NOT GLOB '*[^0-9]*' AND length(google_sub) BETWEEN 1 AND 255 AND instr(google_sub,char(0))=0),
        email TEXT NOT NULL, hd TEXT NOT NULL, display_name TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('pending','approved','rejected','expired')),
        requested_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, decided_by TEXT, decided_at INTEGER,
        user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
        mfa_preauthorized INTEGER NOT NULL DEFAULT 0 CHECK(mfa_preauthorized IN (0,1))
    );
    CREATE TABLE IF NOT EXISTS google_signin_attestations (
        user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        email_at_attestation TEXT NOT NULL, credential_seq INTEGER NOT NULL,
        attested_by TEXT NOT NULL, attested_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
    );").map_err(error)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GoogleSigninLink {
    pub user_id: String, pub google_sub: String, pub hd: String, pub email_at_link: String,
    pub linked_via: String, pub linked_by: String, pub linked_at: i64, pub last_signin_at: Option<i64>,
}
#[derive(Debug, Clone, Serialize)]
pub struct NewGoogleSigninLink {
    pub user_id: String, pub google_sub: String, pub hd: String, pub email_at_link: String,
    pub linked_via: String, pub linked_by: String, pub linked_at: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SigninRequestStatus { Pending, Approved, Rejected, Expired }
impl SigninRequestStatus {
    pub fn as_str(self) -> &'static str {
        match self { Self::Pending => "pending", Self::Approved => "approved", Self::Rejected => "rejected", Self::Expired => "expired" }
    }
    pub fn parse(s: &str) -> Result<Self, String> {
        match s { "pending" => Ok(Self::Pending), "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected), "expired" => Ok(Self::Expired), _ => Err("invalid_signin_request_status".into()) }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SigninRequest {
    pub id: String, pub google_sub: String, pub email: String, pub hd: String, pub display_name: String,
    pub status: SigninRequestStatus, pub requested_at: i64, pub expires_at: i64,
    pub decided_by: Option<String>, pub decided_at: Option<i64>, pub user_id: Option<String>, pub mfa_preauthorized: bool,
}
const LINK_COLUMNS: &str = "user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at,last_signin_at";
const REQUEST_COLUMNS: &str = "id,google_sub,email,hd,display_name,status,requested_at,expires_at,decided_by,decided_at,user_id,mfa_preauthorized";
const COUNT_JIT_SINCE_SQL: &str = "SELECT (SELECT COUNT(*) FROM google_signin_links WHERE linked_via IN ('jit_direct','jit_approved') AND linked_at>=?1)
    + (SELECT COUNT(*) FROM google_signin_requests WHERE status='pending' AND requested_at>=?1)";
fn link_row(r: &Row<'_>) -> rusqlite::Result<GoogleSigninLink> {
    Ok(GoogleSigninLink { user_id: r.get(0)?, google_sub: r.get(1)?, hd: r.get(2)?, email_at_link: r.get(3)?,
        linked_via: r.get(4)?, linked_by: r.get(5)?, linked_at: r.get(6)?, last_signin_at: r.get(7)? })
}
fn request_row(r: &Row<'_>) -> rusqlite::Result<SigninRequest> {
    let status: String = r.get(5)?;
    Ok(SigninRequest { id: r.get(0)?, google_sub: r.get(1)?, email: r.get(2)?, hd: r.get(3)?, display_name: r.get(4)?,
        status: SigninRequestStatus::parse(&status).map_err(|_| rusqlite::Error::InvalidQuery)?, requested_at: r.get(6)?,
        expires_at: r.get(7)?, decided_by: r.get(8)?, decided_at: r.get(9)?, user_id: r.get(10)?, mfa_preauthorized: r.get(11)? })
}
/// Public metadata only: no Google subject, email snapshot or actor identifiers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GoogleSigninSummary {
    pub linked_at: String,
    pub last_signin_at: Option<String>,
}

fn signin_rfc3339(epoch: i64) -> Result<String, String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0)
        .map(|time| time.to_rfc3339()).ok_or_else(|| STORAGE.to_owned())
}

impl UserDb {
    pub fn google_signin_summaries(&self) -> Result<std::collections::HashMap<String, GoogleSigninSummary>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT user_id,linked_at,last_signin_at FROM google_signin_links").map_err(error)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?))).map_err(error)?;
        rows.map(|row| {
            let (id, linked, last) = row.map_err(error)?;
            Ok((id, GoogleSigninSummary { linked_at: signin_rfc3339(linked)?,
                last_signin_at: last.map(signin_rfc3339).transpose()? }))
        }).collect()
    }

    /// Same non-empty-password predicate used by Google unlink and password flows.
    pub fn local_password_statuses(&self) -> Result<std::collections::HashMap<String, bool>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id,password_hash<>'' FROM users").map_err(error)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?;
        rows.collect::<rusqlite::Result<_>>().map_err(error)
    }

    pub fn has_local_password(&self, user: &str) -> Result<bool, String> {
        self.conn().query_row("SELECT password_hash<>'' FROM users WHERE id=?1", [user], |r| r.get(0)).map_err(error)
    }

    /// Credential changes also fence epoch-zero accounts, whose first password
    /// change deliberately preserves epoch zero for legacy login compatibility.
    pub fn google_signin_credential_revision(&self, user: &str) -> Result<i64, String> {
        self.conn().query_row("SELECT COALESCE(MAX(seq),0) FROM credential_changes WHERE user_id=?1", [user], |r| r.get(0)).map_err(error)
    }

    /// Runs on the caller's blocking pool. Never accepts another user's identity.
    pub fn google_signin_verify_password(&self, user: &str, epoch: u64, password: &str) -> Result<(), &'static str> {
        if !self.has_local_password(user).map_err(|_| STORAGE)? { return Err("signin_link_needs_password"); }
        if password.is_empty() || password.len()>4096 { return Err("signin_password_invalid"); }
        let account = self.get_user(user).map_err(|_| STORAGE)?.ok_or("signin_session_expired")?;
        let verified = self.verify_password(&account.email, password).map_err(|_| "signin_password_invalid")?;
        if verified.id != user || verified.status != crate::UserStatus::Active || verified.credential_epoch != epoch {
            return Err("signin_session_expired");
        }
        Ok(())
    }

    /// The verified OIDC identity comes only from the gateway's session-bound ticket.
    pub fn self_link_google_signin(&self, user: &str, epoch: u64, revision: i64, identity: &crate::signin_members::VerifiedGoogleIdentity, now: i64) -> Result<(), &'static str> {
        let _authority = self.google_authority_write().map_err(|_| STORAGE)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND status='active' AND credential_epoch=?2 AND password_hash<>'')",
            params![user,epoch], |r| r.get(0)).map_err(|_| STORAGE)?;
        if !valid { return Err("signin_session_expired"); }
        let current_revision: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0) FROM credential_changes WHERE user_id=?1", [user], |r| r.get(0)).map_err(|_| STORAGE)?;
        if current_revision != revision { return Err("signin_session_expired"); }
        let email: String = tx.query_row("SELECT email FROM users WHERE id=?1", [user], |r| r.get(0)).map_err(|_| STORAGE)?;
        if !email.eq_ignore_ascii_case(&identity.email) { return Err("signin_link_email_mismatch"); }
        let sub_used: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM google_signin_links WHERE google_sub=?1)", [&identity.sub], |r| r.get(0)).map_err(|_| STORAGE)?;
        if sub_used { return Err("signin_sub_linked"); }
        let linked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM google_signin_links WHERE user_id=?1)", [user], |r| r.get(0)).map_err(|_| STORAGE)?;
        if linked { return Err("signin_already_linked"); }
        Self::insert_google_signin_link_tx(&tx, &NewGoogleSigninLink {
            user_id: user.into(), google_sub: identity.sub.clone(), hd: identity.hd.clone(),
            email_at_link: identity.email.clone(), linked_via: "self_link".into(), linked_by: user.into(), linked_at: now,
        }).map_err(|_| STORAGE)?;
        crate::signin_members::audit(&tx, user, "google_signin.self_linked", user, &serde_json::json!({}), now)?;
        tx.commit().map_err(|_| STORAGE)
    }

    pub fn self_unlink_google_signin(&self, user: &str, epoch: u64, password: &str, now: i64) -> Result<(), &'static str> {
        let snapshot: String = self.conn().query_row("SELECT password_hash FROM users WHERE id=?1", [user], |r| r.get(0)).map_err(|_| STORAGE)?;
        if snapshot.is_empty() { return Err("signin_unlink_needs_password"); }
        self.google_signin_verify_password(user, epoch, password)?;
        let _authority = self.google_authority_write().map_err(|_| STORAGE)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND status='active' AND credential_epoch=?2 AND password_hash=?3)",
            params![user,epoch,snapshot], |r| r.get(0)).map_err(|_| STORAGE)?;
        if !valid { return Err("signin_session_expired"); }
        if tx.execute("DELETE FROM google_signin_links WHERE user_id=?1", [user]).map_err(|_| STORAGE)? != 1 { return Err("signin_not_linked"); }
        tx.execute("DELETE FROM google_signin_attestations WHERE user_id=?1", [user]).map_err(|_| STORAGE)?;
        crate::mfa::bump_epoch(&tx, user).map_err(|_| STORAGE)?;
        // Invalidate already-issued management capabilities as well as sessions.
        tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1", [user]).map_err(|_| STORAGE)?;
        crate::signin_members::audit(&tx, user, "google_signin.self_unlinked", user, &serde_json::json!({}), now)?;
        tx.commit().map_err(|_| STORAGE)
    }

    pub fn google_signin_link_by_sub(&self, sub: &str) -> Result<Option<GoogleSigninLink>, String> {
        self.conn().query_row(&format!("SELECT {LINK_COLUMNS} FROM google_signin_links WHERE google_sub=?1"), [sub], link_row).optional().map_err(error)
    }
    pub fn google_signin_link_for_user(&self, user_id: &str) -> Result<Option<GoogleSigninLink>, String> {
        self.conn().query_row(&format!("SELECT {LINK_COLUMNS} FROM google_signin_links WHERE user_id=?1"), [user_id], link_row).optional().map_err(error)
    }
    pub fn insert_google_signin_link_tx(tx: &Transaction<'_>, link: &NewGoogleSigninLink) -> Result<(), String> {
        if !valid_google_sub(&link.google_sub) { return Err("invalid_google_sub".into()); }
        tx.execute("INSERT INTO google_signin_links(user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at)
            VALUES(?1,?2,?3,?4,?5,?6,?7)", params![link.user_id,link.google_sub,link.hd,link.email_at_link,link.linked_via,link.linked_by,link.linked_at])
            .map(|_| ()).map_err(error)
    }
    pub fn delete_google_signin_link(&self, user_id: &str) -> Result<bool, String> {
        self.conn().execute("DELETE FROM google_signin_links WHERE user_id=?1", [user_id]).map(|n| n != 0).map_err(error)
    }
    pub fn touch_google_signin(&self, user_id: &str, now: i64) -> Result<(), String> {
        self.conn().execute("UPDATE google_signin_links SET last_signin_at=?2 WHERE user_id=?1", params![user_id,now]).map(|_| ()).map_err(error)
    }
    /// Re-submission preserves a pending request's id and original request/expiry times.
    /// A non-pending row is replaced for the same sub; cooldown policy belongs to P3.
    pub fn upsert_pending_signin_request_tx(tx: &Transaction<'_>, id: &str, sub: &str, email: &str,
        hd: &str, display_name: &str, now: i64) -> Result<(), String> {
        if !valid_google_sub(sub) { return Err("invalid_google_sub".into()); }
        let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM google_signin_requests WHERE google_sub=?1 AND status='pending')", [sub], |r| r.get(0)).map_err(error)?;
        let count: u32 = tx.query_row("SELECT COUNT(*) FROM google_signin_requests WHERE status='pending'", [], |r| r.get(0)).map_err(error)?;
        if !existing && count >= SIGNIN_PENDING_MAX { return Err("signin_pending_full".into()); }
        let expires = now.checked_add(SIGNIN_REQUEST_TTL_SECS).ok_or("signin_timestamp_overflow")?;
        if existing {
            tx.execute("UPDATE google_signin_requests SET email=?2,hd=?3,display_name=?4 WHERE google_sub=?1",
                params![sub,email,hd,display_name]).map_err(error)?;
        } else {
            tx.execute("INSERT INTO google_signin_requests(id,google_sub,email,hd,display_name,status,requested_at,expires_at)
                VALUES(?1,?2,?3,?4,?5,'pending',?6,?7) ON CONFLICT(google_sub) DO UPDATE SET
                id=excluded.id,email=excluded.email,hd=excluded.hd,display_name=excluded.display_name,status='pending',
                requested_at=excluded.requested_at,expires_at=excluded.expires_at,decided_by=NULL,decided_at=NULL,user_id=NULL,mfa_preauthorized=0",
                params![id,sub,email,hd,display_name,now,expires]).map_err(error)?;
        }
        Ok(())
    }
    pub fn signin_request_by_id(&self, id: &str) -> Result<Option<SigninRequest>, String> {
        self.conn().query_row(&format!("SELECT {REQUEST_COLUMNS} FROM google_signin_requests WHERE id=?1"), [id], request_row).optional().map_err(error)
    }
    pub(crate) fn signin_request_by_id_tx(tx: &Transaction<'_>, id: &str) -> Result<Option<SigninRequest>, String> {
        tx.query_row(&format!("SELECT {REQUEST_COLUMNS} FROM google_signin_requests WHERE id=?1"), [id], request_row).optional().map_err(error)
    }
    pub(crate) fn signin_request_by_sub_tx(tx: &Transaction<'_>, sub: &str) -> Result<Option<SigninRequest>, String> {
        tx.query_row(&format!("SELECT {REQUEST_COLUMNS} FROM google_signin_requests WHERE google_sub=?1"), [sub], request_row).optional().map_err(error)
    }
    pub fn signin_request_by_sub(&self, sub: &str) -> Result<Option<SigninRequest>, String> {
        self.conn().query_row(&format!("SELECT {REQUEST_COLUMNS} FROM google_signin_requests WHERE google_sub=?1"), [sub], request_row).optional().map_err(error)
    }
    pub fn list_pending_signin_requests(&self) -> Result<Vec<SigninRequest>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {REQUEST_COLUMNS} FROM google_signin_requests WHERE status='pending' ORDER BY requested_at,id")).map_err(error)?;
        let rows = stmt.query_map([], request_row).map_err(error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(error)
    }
    pub fn set_signin_request_status_tx(tx: &Transaction<'_>, id: &str, status: SigninRequestStatus,
        decided_by: &str, now: i64, user_id: Option<&str>, mfa_preauthorized: bool) -> Result<(), String> {
        let changed = tx.execute("UPDATE google_signin_requests SET status=?2,decided_by=?3,decided_at=?4,user_id=?5,mfa_preauthorized=?6 WHERE id=?1",
            params![id,status.as_str(),decided_by,now,user_id,mfa_preauthorized]).map_err(error)?;
        if changed != 1 { return Err("signin_request_not_found".into()); }
        Ok(())
    }
    pub fn expire_signin_requests(&self, now: i64) -> Result<usize, String> {
        self.conn().execute("UPDATE google_signin_requests SET status='expired' WHERE status='pending' AND expires_at<=?1", [now]).map_err(error)
    }
    pub fn count_signin_jit_since(&self, since: i64) -> Result<u32, String> {
        self.conn().query_row(COUNT_JIT_SINCE_SQL, [since], |r| r.get(0)).map_err(error)
    }
    pub fn count_signin_jit_since_tx(tx: &Transaction<'_>, since: i64) -> Result<u32, String> {
        tx.query_row(COUNT_JIT_SINCE_SQL, [since], |r| r.get(0)).map_err(error)
    }
    pub(crate) fn upsert_google_signin_attestation(&self, user_id: &str, attested_by: &str, now: i64) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(error)?;
        Self::upsert_google_signin_attestation_tx(&tx, user_id, attested_by, now)?;
        tx.commit().map_err(error)
    }

    fn upsert_google_signin_attestation_tx(tx: &Transaction<'_>, user_id: &str, attested_by: &str, now: i64) -> Result<(), String> {
        let expires = now.checked_add(SIGNIN_ATTESTATION_TTL_SECS).ok_or("signin_timestamp_overflow")?;
        let email: String = tx.query_row("SELECT email FROM users WHERE id=?1", [user_id], |r| r.get(0)).map_err(error)?;
        let seq = crate::credential_changes::high_water(tx).map_err(|_| STORAGE)?;
        tx.execute("INSERT INTO google_signin_attestations(user_id,email_at_attestation,credential_seq,attested_by,attested_at,expires_at)
            VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(user_id) DO UPDATE SET email_at_attestation=excluded.email_at_attestation,
            credential_seq=excluded.credential_seq,attested_by=excluded.attested_by,attested_at=excluded.attested_at,expires_at=excluded.expires_at",
            params![user_id,email,seq,attested_by,now,expires]).map_err(error)?;
        Ok(())
    }

    /// Authorize, validate and attest the whole batch in one write transaction.
    pub fn attest_google_signin_users(&self, admin_id: &str, admin_epoch: u64, user_ids: &[String], now: i64) -> Result<usize, &'static str> {
        let _authority = self.google_authority_write().map_err(|_| STORAGE)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        crate::signin_members::live_admin(&tx, admin_id, admin_epoch)?;
        let unique: std::collections::HashSet<_> = user_ids.iter().collect();
        if user_ids.len() > 100 || unique.len() != user_ids.len() { return Err("signin_attest_invalid_target"); }
        for user_id in user_ids {
            let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users u WHERE u.id=?1 AND u.status='active'
                AND NOT EXISTS(SELECT 1 FROM google_signin_links l WHERE l.user_id=u.id))",
                [user_id], |r| r.get(0)).map_err(|_| STORAGE)?;
            if !valid { return Err("signin_attest_invalid_target"); }
            Self::upsert_google_signin_attestation_tx(&tx, user_id, admin_id, now).map_err(|_| STORAGE)?;
            crate::signin_members::audit(&tx, admin_id, "google_signin.attested", user_id, &serde_json::json!({}), now)?;
        }
        tx.commit().map_err(|_| STORAGE)?;
        Ok(user_ids.len())
    }

    /// Unlinking invalidates dashboard tokens and live ACL snapshots atomically.
    pub fn unlink_google_signin(&self, admin_id: &str, admin_epoch: u64, user_id: &str, now: i64) -> Result<(), &'static str> {
        let _authority = self.google_authority_write().map_err(|_| STORAGE)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        crate::signin_members::live_admin(&tx, admin_id, admin_epoch)?;
        if tx.execute("DELETE FROM google_signin_links WHERE user_id=?1", [user_id]).map_err(|_| STORAGE)? != 1 {
            return Err("signin_not_linked");
        }
        tx.execute("DELETE FROM google_signin_attestations WHERE user_id=?1", [user_id]).map_err(|_| STORAGE)?;
        crate::mfa::bump_epoch(&tx, user_id).map_err(|_| STORAGE)?;
        crate::signin_members::audit(&tx, admin_id, "google_signin.unlinked", user_id, &serde_json::json!({}), now)?;
        tx.commit().map_err(|_| STORAGE)
    }
    /// Any later credential change of this user invalidates the captured high-water mark.
    /// A token-email mismatch alone leaves an otherwise valid attestation for its owner.
    pub fn take_valid_google_signin_attestation_tx(tx: &Transaction<'_>, user_id: &str, token_email: &str, now: i64) -> Result<bool, String> {
        let stored: Option<(String,i64,i64)> = tx.query_row("SELECT email_at_attestation,credential_seq,expires_at FROM google_signin_attestations WHERE user_id=?1",
            [user_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(error)?;
        let Some((email,seq,expires)) = stored else { return Ok(false) };
        let current: Option<String> = tx.query_row("SELECT email FROM users WHERE id=?1", [user_id], |r| r.get(0)).optional().map_err(error)?;
        let changed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM credential_changes WHERE user_id=?1 AND seq>?2)", params![user_id,seq], |r| r.get(0)).map_err(error)?;
        let stale = expires <= now || changed || !current.as_deref().is_some_and(|s| email.eq_ignore_ascii_case(s));
        let valid = !stale && email.eq_ignore_ascii_case(token_email);
        if stale || valid { tx.execute("DELETE FROM google_signin_attestations WHERE user_id=?1", [user_id]).map_err(error)?; }
        Ok(valid)
    }
    pub fn delete_google_signin_attestation(&self, user_id: &str) -> Result<(), String> {
        self.conn().execute("DELETE FROM google_signin_attestations WHERE user_id=?1", [user_id]).map(|_| ()).map_err(error)
    }
}

#[cfg(test)]
#[path = "google_signin_tests.rs"]
mod tests;

#[cfg(test)]
mod batch_tests {

    #[test]
    fn signin_batch_links_and_passwords_are_public_and_fail_closed() {
        let (db, _home) = crate::test_support::test_db();
        let normal = db.create_user("normal@company.test", "Normal", "password123", crate::UserRole::Employee).unwrap();
        let jit = db.create_user("jit@company.test", "JIT", "password123", crate::UserRole::Employee).unwrap();
        db.conn().execute("UPDATE users SET password_hash='' WHERE id=?1", [&jit.id]).unwrap();
        db.conn().execute("INSERT INTO google_signin_links(user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at,last_signin_at)
            VALUES(?1,'123','company.test','jit@company.test','jit_direct','actor',1700000000,1700000060)", [&jit.id]).unwrap();
        let links = db.google_signin_summaries().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[&jit.id].linked_at, "2023-11-14T22:13:20+00:00");
        assert_eq!(links[&jit.id].last_signin_at.as_deref(), Some("2023-11-14T22:14:20+00:00"));
        let passwords = db.local_password_statuses().unwrap();
        assert!(passwords[&normal.id]);
        assert!(!passwords[&jit.id]);
        for id in [&normal.id, &jit.id] {
            assert_eq!(passwords[id], db.has_local_password(id).unwrap());
        }
        let summary = serde_json::to_value(&links[&jit.id]).unwrap();
        assert_eq!(summary.as_object().unwrap().len(), 2);
        db.conn().execute("UPDATE google_signin_links SET last_signin_at=NULL", []).unwrap();
        assert_eq!(db.google_signin_summaries().unwrap()[&jit.id].last_signin_at, None);
        db.conn().execute("UPDATE google_signin_links SET linked_at=9223372036854775807", []).unwrap();
        assert!(db.google_signin_summaries().is_err());
        db.conn().execute("DROP TABLE google_signin_links", []).unwrap();
        assert!(db.google_signin_summaries().is_err());
        db.conn().execute("ALTER TABLE users RENAME COLUMN password_hash TO unavailable", []).unwrap();
        assert!(db.local_password_statuses().is_err());
    }
}
