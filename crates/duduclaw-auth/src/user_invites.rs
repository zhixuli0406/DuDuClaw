//! Single-use account invitations. Bearer tokens are returned only to the
//! delivery layer; the database and audit trail contain no bearer secrets.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::UserDb;
use crate::db::hash_password;
use crate::user_row::bump_acl_generation;

pub const INVITE_TTL_SECS: i64 = 7 * 24 * 60 * 60;
pub const INVITE_ADMIN_REQUIRED: &str = "invite_admin_required";
pub const INVITE_USER_INELIGIBLE: &str = "invite_user_ineligible";

/// Idempotent `user_invites` migration, invoked by `UserDb::init_tables`.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS user_invites (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL REFERENCES users(id),
            token_sha256 TEXT UNIQUE NOT NULL,
            created_by TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            used_at INTEGER,
            revoked_at INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_user_invites_user ON user_invites(user_id);
         CREATE TRIGGER IF NOT EXISTS user_invites_revoke_account_change
         AFTER UPDATE OF email,password_hash,status ON users
         WHEN OLD.email != NEW.email OR OLD.password_hash != NEW.password_hash
              OR NEW.status != 'active'
         BEGIN
             INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp)
                 SELECT NEW.id,'user_invite.revoked',NEW.id,id,strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 FROM user_invites WHERE user_id=NEW.id AND used_at IS NULL AND revoked_at IS NULL;
             UPDATE user_invites SET revoked_at=CAST(strftime('%s','now') AS INTEGER)
                 WHERE user_id=NEW.id AND used_at IS NULL AND revoked_at IS NULL;
         END;",
    ).map_err(|_| "user_invites migration failed".to_owned())
}

/// Deliberately has no `Debug` or serialization implementation: `token` is a
/// bearer credential and must not accidentally enter logs or RPC output.
pub struct IssuedUserInvite {
    pub id: String,
    pub user_id: String,
    pub email: String,
    pub token: String,
    pub expires_at: i64,
}

fn token_sha256(token: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
        .as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

fn token_shape_valid(token: &str) -> bool {
    // A canonical base64url encoding of exactly 32 random bytes is 43 chars.
    token.len() == 43
        && URL_SAFE_NO_PAD.decode(token).is_ok_and(|bytes| bytes.len() == 32)
}

fn audit(conn: &Connection, actor: &str, action: &str, user: &str, invite_id: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp)
         VALUES(?1,?2,?3,?4,?5)",
        params![actor, action, user, invite_id, Utc::now().to_rfc3339()],
    ).map_err(|_| "invitation audit unavailable".to_owned())?;
    Ok(())
}

impl UserDb {
    /// Issue and revoke older unused invitations in one write transaction.
    /// Re-read Admin authority and target eligibility under that same lock.
    pub fn issue_user_invite(&self, user_id: &str, created_by: &str) -> Result<IssuedUserInvite, String> {
        self.issue_user_invite_checked(user_id, created_by, None)
    }

    /// RPC callers bind the issuance to the epoch of the authenticated session,
    /// preventing an in-flight, revoked Admin session from creating an invite.
    pub fn issue_user_invite_at_epoch(&self, user_id: &str, created_by: &str, epoch: u64) -> Result<IssuedUserInvite, String> {
        self.issue_user_invite_checked(user_id, created_by, Some(epoch))
    }

    fn issue_user_invite_checked(&self, user_id: &str, created_by: &str, epoch: Option<u64>) -> Result<IssuedUserInvite, String> {
        let mut bytes = [0u8; 32];
        SystemRandom::new().fill(&mut bytes).map_err(|_| "invitation RNG unavailable")?;
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let id = Uuid::new_v4().to_string();
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "invitation storage unavailable")?;
        let now = Utc::now().timestamp();
        let admin: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active'
             AND must_change_password=0 AND (?2 IS NULL OR credential_epoch=?2))",
            params![created_by, epoch], |r| r.get(0),
        ).map_err(|_| "invitation storage unavailable")?;
        if !admin { return Err(INVITE_ADMIN_REQUIRED.to_owned()); }
        let email: Option<String> = tx.query_row(
            "SELECT email FROM users WHERE id=?1 AND status='active'
             AND password_hash=''",
            [user_id], |r| r.get(0),
        ).optional().map_err(|_| "invitation storage unavailable")?;
        let email = email.ok_or(INVITE_USER_INELIGIBLE)?;
        let revoked = {
            let mut stmt = tx.prepare(
                "SELECT id FROM user_invites WHERE user_id=?1 AND used_at IS NULL AND revoked_at IS NULL",
            ).map_err(|_| "invitation storage unavailable")?;
            let rows = stmt.query_map([user_id], |r| r.get::<_, String>(0))
                .map_err(|_| "invitation storage unavailable")?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|_| "invitation storage unavailable")?
        };
        tx.execute(
            "UPDATE user_invites SET revoked_at=?2 WHERE user_id=?1 AND used_at IS NULL AND revoked_at IS NULL",
            params![user_id, now],
        ).map_err(|_| "invitation storage unavailable")?;
        for old_id in revoked {
            audit(&tx, created_by, "user_invite.revoked", user_id, &old_id)?;
        }
        let expires_at = now + INVITE_TTL_SECS;
        tx.execute(
            "INSERT INTO user_invites(id,user_id,token_sha256,created_by,created_at,expires_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![id, user_id, token_sha256(&token), created_by, now, expires_at],
        ).map_err(|_| "invitation storage unavailable")?;
        audit(&tx, created_by, "user_invite.created", user_id, &id)?;
        tx.commit().map_err(|_| "invitation storage unavailable")?;
        Ok(IssuedUserInvite { id, user_id: user_id.to_owned(), email, token, expires_at })
    }

    /// Return an email only while this invitation can still be consumed.
    /// The public HTTP layer must mask this value before returning it.
    pub fn user_invite_email(&self, token: &str) -> Result<Option<String>, String> {
        if !token_shape_valid(token) { return Ok(None); }
        let conn = self.conn();
        conn.query_row(
            "SELECT u.email FROM user_invites i JOIN users u ON u.id=i.user_id
             WHERE i.token_sha256=?1 AND i.used_at IS NULL AND i.revoked_at IS NULL
             AND i.expires_at>?2 AND u.status='active'
             AND u.password_hash=''",
            params![token_sha256(token), Utc::now().timestamp()], |r| r.get(0),
        ).optional().map_err(|_| "invitation storage unavailable".to_owned())
    }

    /// Consume and set the password atomically. `None` is the single public
    /// invalid outcome (unknown, malformed, expired, used, revoked, disabled).
    /// The gateway must apply its normal change-password strength rules first.
    /// Changing password_hash invokes the existing MFA credential-epoch trigger.
    pub fn consume_user_invite(&self, token: &str, password: &str) -> Result<Option<String>, String> {
        if password.chars().count() < 8 { return Err("password_too_short".to_owned()); }
        if password.len() > 1024 { return Err("password_too_long".to_owned()); }
        if self.user_invite_email(token)?.is_none() { return Ok(None); }
        let password_hash = hash_password(password)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "invitation storage unavailable")?;
        let now = Utc::now().timestamp();
        let hash = token_sha256(token);
        let candidate: Option<(String, String)> = tx.query_row(
            "SELECT i.id,i.user_id FROM user_invites i JOIN users u ON u.id=i.user_id
             WHERE i.token_sha256=?1 AND i.used_at IS NULL AND i.revoked_at IS NULL
             AND i.expires_at>?2 AND u.status='active'
             AND u.password_hash=''",
            params![hash, now], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional().map_err(|_| "invitation storage unavailable")?;
        let Some((id, user_id)) = candidate else { return Ok(None); };
        let affected = tx.execute(
            "UPDATE user_invites SET used_at=?2 WHERE token_sha256=?1
             AND used_at IS NULL AND revoked_at IS NULL AND expires_at>?2",
            params![hash, now],
        ).map_err(|_| "invitation storage unavailable")?;
        if affected != 1 { return Ok(None); }
        let changed = tx.execute(
            "UPDATE users SET password_hash=?2,must_change_password=0,updated_at=?3
             WHERE id=?1 AND status='active' AND password_hash=''",
            params![user_id, password_hash, Utc::now().to_rfc3339()],
        ).map_err(|_| "invitation storage unavailable")?;
        if changed != 1 { return Ok(None); }
        // The existing password trigger increments nonzero epochs and clears
        // outstanding MFA challenges. Also retire legacy epoch-zero tokens.
        tx.execute("UPDATE users SET credential_epoch=1 WHERE id=?1 AND credential_epoch=0", [&user_id])
            .map_err(|_| "invitation storage unavailable")?;
        bump_acl_generation(&tx).map_err(|_| "invitation storage unavailable")?;
        audit(&tx, &user_id, "user_invite.used", &user_id, &id)?;
        tx.commit().map_err(|_| "invitation storage unavailable")?;
        Ok(Some(user_id))
    }

    /// Whether an Admin can invite this account. This exposes no credential
    /// material and is informational only: issuance rechecks under its lock.
    pub fn user_invite_eligible(&self, user_id: &str) -> Result<bool, String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND status='active'
             AND password_hash='')",
            [user_id], |r| r.get(0),
        ).map_err(|_| "invitation storage unavailable".to_owned())
    }

    /// Latest issuance state for an Admin's user list. Reissued rows remain in
    /// the database for audit; rowid breaks ties for same-second reissues.
    pub fn user_invite_status(&self, user_id: &str) -> Result<&'static str, String> {
        let conn = self.conn();
        let latest: Option<(Option<i64>, Option<i64>, i64)> = conn.query_row(
            "SELECT used_at,revoked_at,expires_at FROM user_invites WHERE user_id=?1
             ORDER BY created_at DESC,rowid DESC LIMIT 1",
            [user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional().map_err(|_| "invitation storage unavailable")?;
        Ok(match latest {
            None => "none",
            Some((Some(_), _, _)) => "used",
            Some((None, Some(_), _)) => "none",
            Some((None, None, expiry)) if expiry <= Utc::now().timestamp() => "expired",
            Some(_) => "pending",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewUser, UserRole, UserStatus};
    use std::sync::{Arc, Barrier};

    fn setup() -> (tempfile::TempDir, Arc<UserDb>, String, String) {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&temp.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@example.com", "Admin", "admin-password", UserRole::Admin).unwrap();
        let user = db.create_invited_user_with(&NewUser {
            email: "new@example.com", display_name: "New", password: "ignored-password",
            role: UserRole::Employee, department: None, manager: None,
        }).unwrap();
        (temp, db, admin.id, user.id)
    }

    #[test]
    fn only_hash_is_stored_and_invited_account_has_no_password() {
        let (_temp, db, admin, user) = setup();
        let issued = db.issue_user_invite(&user, &admin).unwrap();
        assert_eq!(issued.token.len(), 43);
        assert!(db.verify_password("new@example.com", "").is_err());
        assert!(db.verify_password("new@example.com", "ignored-password").is_err());
        assert!(db.get_user(&user).unwrap().unwrap().must_change_password);
        let conn = db.conn();
        let stored: String = conn.query_row("SELECT token_sha256 FROM user_invites WHERE id=?1", [&issued.id], |r| r.get(0)).unwrap();
        assert_eq!(stored, token_sha256(&issued.token));
        assert_ne!(stored, issued.token);
        let mut stmt = conn.prepare("SELECT COALESCE(detail,'') FROM auth_audit_log").unwrap();
        for detail in stmt.query_map([], |r| r.get::<_, String>(0)).unwrap() {
            let detail = detail.unwrap();
            assert!(!detail.contains(&issued.token));
            assert!(!detail.contains(&stored));
        }
    }

    #[test]
    fn expiry_reissue_disabled_and_unknown_are_all_invalid() {
        let (_temp, db, admin, user) = setup();
        assert_eq!(db.user_invite_status(&user).unwrap(), "none");
        let old = db.issue_user_invite(&user, &admin).unwrap();
        assert_eq!(db.user_invite_status(&user).unwrap(), "pending");
        db.conn().execute("UPDATE user_invites SET expires_at=?1", [Utc::now().timestamp()]).unwrap();
        assert_eq!(db.user_invite_status(&user).unwrap(), "expired");
        assert!(db.user_invite_email(&old.token).unwrap().is_none());
        assert!(db.consume_user_invite(&old.token, "new-password-123").unwrap().is_none());
        let new = db.issue_user_invite(&user, &admin).unwrap();
        assert!(db.user_invite_email(&old.token).unwrap().is_none());
        let revoked: i64 = db.conn().query_row("SELECT COUNT(*) FROM user_invites WHERE revoked_at IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(revoked, 1);
        assert!(db.user_invite_email(&new.token).unwrap().is_some());
        db.set_user_status(&user, UserStatus::Suspended).unwrap();
        assert!(db.user_invite_email(&new.token).unwrap().is_none());
        assert!(db.consume_user_invite(&new.token, "new-password-123").unwrap().is_none());
        assert!(db.user_invite_email(&"A".repeat(43)).unwrap().is_none());
        assert!(db.consume_user_invite("bad", "new-password-123").unwrap().is_none());
        assert!(db.issue_user_invite(&user, &admin).is_err());
    }

    #[test]
    fn concurrent_attempts_only_one_password_lands_and_epoch_advances() {
        let (_temp, db, admin, user) = setup();
        // Existing password updates increment a nonzero credential epoch via
        // the MFA trigger; invitation redemption must follow the same path.
        db.conn().execute("UPDATE users SET credential_epoch=4 WHERE id=?1", [&user]).unwrap();
        let issued = db.issue_user_invite(&user, &admin).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let threads = ["first-strong-password", "second-strong-password"].into_iter().map(|password| {
            let db = db.clone(); let token = issued.token.clone(); let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (password, db.consume_user_invite(&token, password).unwrap().is_some())
            })
        }).collect::<Vec<_>>();
        let results = threads.into_iter().map(|thread| thread.join().unwrap()).collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|(_, used)| *used).count(), 1);
        for (password, used) in results { assert_eq!(db.verify_password("new@example.com", password).is_ok(), used); }
        let saved = db.get_user(&user).unwrap().unwrap();
        assert!(!saved.must_change_password);
        assert_eq!(saved.credential_epoch, 5);
        assert_eq!(db.user_invite_status(&user).unwrap(), "used");
        assert!(db.user_invite_email(&issued.token).unwrap().is_none());
        assert!(db.consume_user_invite(&issued.token, "third-strong-password").unwrap().is_none());
        assert!(db.issue_user_invite(&user, &admin).is_err());
    }


    #[test]
    fn account_changes_revoke_invites_even_after_reactivation() {
        for change in ["email", "password", "status"] {
            let (_temp, db, admin, user) = setup();
            let issued = db.issue_user_invite(&user, &admin).unwrap();
            match change {
                "email" => db.change_email(&user, "changed@example.com").unwrap(),
                "password" => db.update_user(&user, None, None, Some("changed-password")).unwrap(),
                "status" => {
                    db.set_user_status(&user, UserStatus::Suspended).unwrap();
                    db.set_user_status(&user, UserStatus::Active).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(db.user_invite_email(&issued.token).unwrap().is_none());
            assert!(db.consume_user_invite(&issued.token, "new-password-123").unwrap().is_none());
            let audits = db.query_audit_log(None, Some("user_invite.revoked"), 20).unwrap();
            assert_eq!(audits.len(), 1);
        }
    }

    #[test]
    fn strength_rules_and_legacy_epoch_are_enforced() {
        let (_temp, db, admin, user) = setup();
        let issued = db.issue_user_invite(&user, &admin).unwrap();
        assert_eq!(db.consume_user_invite(&issued.token, "短密碼").unwrap_err(), "password_too_short");
        assert_eq!(db.consume_user_invite(&issued.token, &"a".repeat(1025)).unwrap_err(), "password_too_long");
        assert!(db.user_invite_email(&issued.token).unwrap().is_some());
        assert!(db.consume_user_invite(&issued.token, "這是足夠長度的密碼").unwrap().is_some());
        assert_eq!(db.get_user(&user).unwrap().unwrap().credential_epoch, 1);
        let revoked: i64 = db.conn().query_row("SELECT COUNT(*) FROM user_invites WHERE revoked_at IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(revoked, 0, "redemption does not revoke its own already-used row");
    }


    #[test]
    fn issuance_rechecks_admin_epoch_and_forced_change() {
        let (_temp, db, admin, user) = setup();
        assert!(db.issue_user_invite_at_epoch(&user, &admin, 0).is_ok());
        db.conn().execute("UPDATE users SET credential_epoch=1 WHERE id=?1", [&admin]).unwrap();
        assert_eq!(db.issue_user_invite_at_epoch(&user, &admin, 0).err().unwrap(), INVITE_ADMIN_REQUIRED);
        assert!(db.issue_user_invite_at_epoch(&user, &admin, 1).is_ok());
        db.conn().execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&admin]).unwrap();
        assert_eq!(db.issue_user_invite(&user, &admin).err().unwrap(), INVITE_ADMIN_REQUIRED);
    }

    #[test]
    fn eligibility_is_false_for_missing_disabled_and_settled_accounts() {
        let (_temp, db, admin, user) = setup();
        assert!(db.user_invite_eligible(&user).unwrap());
        assert!(!db.user_invite_eligible("missing-user").unwrap());
        assert!(!db.user_invite_eligible(&admin).unwrap());
        db.set_user_status(&user, UserStatus::Suspended).unwrap();
        assert!(!db.user_invite_eligible(&user).unwrap());
        db.set_user_status(&user, UserStatus::Active).unwrap();
        assert!(db.user_invite_eligible(&user).unwrap());
        let issued = db.issue_user_invite(&user, &admin).unwrap();
        assert!(db.consume_user_invite(&issued.token, "settled-password").unwrap().is_some());
        assert!(!db.user_invite_eligible(&user).unwrap());
    }

    #[test]
    fn forced_password_change_never_allows_an_invite_to_reset_a_password() {
        let (_temp, db, admin, user) = setup();
        let issued = db.issue_user_invite(&user, &admin).unwrap();
        db.update_user(&user, None, None, Some("existing-password")).unwrap();
        db.conn().execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&user]).unwrap();
        // Simulate a legacy unrevoked invite as well as checking new issuance.
        db.conn().execute("UPDATE user_invites SET revoked_at=NULL WHERE id=?1", [&issued.id]).unwrap();
        assert!(!db.user_invite_eligible(&user).unwrap());
        assert_eq!(db.issue_user_invite(&user, &admin).err().unwrap(), INVITE_USER_INELIGIBLE);
        assert!(db.user_invite_email(&issued.token).unwrap().is_none());
        assert!(db.consume_user_invite(&issued.token, "replacement-password").unwrap().is_none());
        let conn = db.conn();
        let used: Option<i64> = conn.query_row("SELECT used_at FROM user_invites WHERE id=?1", [&issued.id], |r| r.get(0)).unwrap();
        assert!(used.is_none());
    }

    #[test]
    fn non_admin_cannot_issue_or_revoke() {
        let (_temp, db, admin, user) = setup();
        let issued = db.issue_user_invite(&user, &admin).unwrap();
        let err = db.issue_user_invite(&user, &user).err().unwrap();
        assert_eq!(err, INVITE_ADMIN_REQUIRED);
        assert!(db.user_invite_email(&issued.token).unwrap().is_some());
    }
}
