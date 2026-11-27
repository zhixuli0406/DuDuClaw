//! Purpose-bound, single-use Admin verification for in-house skill sources.
//! This is deliberately separate from member-key creation (which rejects Admins).
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;
use crate::{UserDb, UserRole};

const PURPOSE: &str = "skill_trust_pin";
const TTL: i64 = 300;
const DENIED: &str = "請重新輸入目前密碼或新的二次驗證碼。";
const STORE: &str = "目前無法驗證身分，請稍後再試。";

pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS skill_trust_pin_proofs (
        token_hash TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        session TEXT NOT NULL,
        credential_epoch INTEGER NOT NULL,
        password_hash TEXT NOT NULL,
        purpose TEXT NOT NULL,
        mfa_active INTEGER NOT NULL,
        issued_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
    );").map_err(|e| e.to_string())
}
fn store(_: impl std::fmt::Display) -> String { STORE.into() }
fn digest(proof: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, proof.as_bytes()).as_ref()
        .iter().map(|b| format!("{b:02x}")).collect()
}
impl UserDb {
    /// The gateway must rate-limit using the actual transport peer first.
    /// Proofs stay on the server and cannot authorize member-key creation.
    pub fn skill_trust_pin_reauthenticate(
        &self, user: &str, session: &str, epoch: u64, password: &str, code: &str,
        cipher: &dyn crate::mfa::MfaCipher, now: i64,
    ) -> Result<String, String> {
        if Uuid::parse_str(session).is_err() { return Err(DENIED.into()); }
        let account = self.get_user(user).map_err(store)?.ok_or(DENIED)?;
        if account.role != UserRole::Admin || account.credential_epoch != epoch {
            return Err(DENIED.into());
        }
        let snapshot: String = self.conn().query_row("SELECT password_hash FROM users WHERE id=?1", [user], |r| r.get(0)).map_err(store)?;
        let mfa = self.mfa_active(user).map_err(store)?;
        if mfa {
            self.mfa_step_up_verify(user, code, cipher, now).map_err(|_| DENIED)?;
        } else {
            let verified = self.verify_password(&account.email, password).map_err(|_| DENIED)?;
            if password.is_empty() || verified.id != user || verified.credential_epoch != epoch {
                return Err(DENIED.into());
            }
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store)?;
        let current: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND credential_epoch=?2
            AND password_hash=?3 AND status='active' AND must_change_password=0 AND role='admin')",
            params![user, epoch, snapshot], |r| r.get(0)).map_err(store)?;
        if !current || crate::mfa::any_factor(&tx, user).map_err(store)? != mfa
            || !crate::remembered_sessions::session_valid(&tx, Some(session), user, now).map_err(store)? {
            return Err(DENIED.into());
        }
        tx.execute("DELETE FROM skill_trust_pin_proofs WHERE user_id=?1 OR expires_at<=?2", params![user, now]).map_err(store)?;
        let proof = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO skill_trust_pin_proofs VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![digest(&proof), user, session, epoch, snapshot, PURPOSE, mfa, now, now + TTL]).map_err(store)?;
        tx.commit().map_err(store)?;
        Ok(proof)
    }

    /// Consume before any pin writes, including requests that later fail validation.
    pub fn skill_trust_pin_consume(
        &self, user: &str, session: &str, epoch: u64, proof: &str, now: i64,
    ) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store)?;
        let binding: Option<(String, bool)> = tx.query_row("SELECT password_hash,mfa_active FROM skill_trust_pin_proofs
            WHERE token_hash=?1 AND user_id=?2 AND session=?3 AND credential_epoch=?4 AND purpose=?5
            AND issued_at<=?6 AND expires_at>?6 AND expires_at-issued_at<=?7",
            params![digest(proof), user, session, epoch, PURPOSE, now, TTL], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional().map_err(store)?;
        let mut valid = false;
        if let Some((snapshot, mfa)) = binding {
            tx.execute("DELETE FROM skill_trust_pin_proofs WHERE token_hash=?1", [digest(proof)]).map_err(store)?;
            valid = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND credential_epoch=?2
                AND password_hash=?3 AND status='active' AND must_change_password=0 AND role='admin')",
                params![user, epoch, snapshot], |r| r.get::<_, bool>(0)).map_err(store)?
                && crate::mfa::any_factor(&tx, user).map_err(store)? == mfa
                && crate::remembered_sessions::session_valid(&tx, Some(session), user, now).map_err(store)?;
        }
        tx.commit().map_err(store)?;
        if valid { Ok(()) } else { Err(DENIED.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Cipher;
    impl crate::mfa::MfaCipher for Cipher {
        fn encrypt(&self, value: &str) -> Result<String, String> { Ok(value.into()) }
        fn decrypt(&self, value: &str) -> Result<String, String> { Ok(value.into()) }
    }
    fn fixture() -> (tempfile::TempDir, UserDb, crate::User, String) {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
        (home, db, admin, Uuid::new_v4().to_string())
    }
    #[test]
    fn skill_trust_pin_proof_is_admin_only_bound_single_use_and_five_minutes() {
        let (_home, db, admin, session) = fixture();
        let now = chrono::Utc::now().timestamp();
        let member = db.create_user("member@test.invalid", "Member", "test-password", UserRole::Employee).unwrap();
        assert!(db.skill_trust_pin_reauthenticate(&member.id, &session, 0, "test-password", "", &Cipher, now).is_err());
        for password in ["", "wrong"] {
            assert!(db.skill_trust_pin_reauthenticate(&admin.id, &session, 0, password, "", &Cipher, now).is_err());
        }
        let issue = || db.skill_trust_pin_reauthenticate(&admin.id, &session, 0, "test-password", "", &Cipher, now).unwrap();
        let proof = issue();
        for (user, sess, epoch) in [(&member.id, &session, 0), (&admin.id, &Uuid::new_v4().to_string(), 0), (&admin.id, &session, 1)] {
            assert!(db.skill_trust_pin_consume(user, sess, epoch, &proof, now).is_err());
        }
        assert!(db.member_key_create_authenticated(&admin.id, "x", 1, 0, &session, &proof).is_err());
        db.conn().execute("UPDATE skill_trust_pin_proofs SET purpose='me.member_keys.create'", []).unwrap();
        assert!(db.skill_trust_pin_consume(&admin.id, &session, 0, &proof, now).is_err());
        db.conn().execute("UPDATE skill_trust_pin_proofs SET purpose=?1", [PURPOSE]).unwrap();
        db.skill_trust_pin_consume(&admin.id, &session, 0, &proof, now + 299).unwrap();
        assert!(db.skill_trust_pin_consume(&admin.id, &session, 0, &proof, now + 299).is_err());
        assert!(db.skill_trust_pin_consume(&admin.id, &session, 0, &issue(), now + 300).is_err());
        assert!(db.skill_trust_pin_consume(&admin.id, &session, 0, &issue(), now - 1).is_err());
        let proof = issue();
        db.conn().execute("UPDATE users SET password_hash='changed' WHERE id=?1", [&admin.id]).unwrap();
        assert!(db.skill_trust_pin_consume(&admin.id, &session, 0, &proof, now).is_err());
    }
    #[test]
    fn skill_trust_pin_mfa_requires_fresh_code_and_consumes_it() {
        let (_home, db, admin, session) = fixture();
        let (enrollment, secret) = crate::mfa::test_totp_prepare(&db, &admin.id, "test-password", &Cipher);
        db.mfa_confirm(&admin.id, Some(&enrollment), &crate::mfa::test_totp_code(&secret, 100), &Cipher, 3000).unwrap();
        let epoch = db.get_user(&admin.id).unwrap().unwrap().credential_epoch;
        let now = chrono::Utc::now().timestamp();
        assert!(db.skill_trust_pin_reauthenticate(&admin.id, &session, epoch, "test-password", "wrong", &Cipher, now).is_err());
        let code = crate::mfa::test_totp_code(&secret, now / 30);
        let proof = db.skill_trust_pin_reauthenticate(&admin.id, &session, epoch, "", &code, &Cipher, now).unwrap();
        db.skill_trust_pin_consume(&admin.id, &session, epoch, &proof, now).unwrap();
        assert!(db.skill_trust_pin_consume(&admin.id, &session, epoch, &proof, now).is_err());
        assert!(db.skill_trust_pin_reauthenticate(&admin.id, &session, epoch, "", &code, &Cipher, now).is_err());
    }
    #[test]
    fn skill_trust_pin_rechecks_role_epoch_and_session() {
        for change in ["UPDATE users SET role='employee'", "UPDATE users SET credential_epoch=credential_epoch+1",
            "UPDATE users SET must_change_password=1", "UPDATE users SET status='suspended'",
            "UPDATE remembered_families SET revoked=1"] {
            let (_home, db, admin, session) = fixture();
            let now = chrono::Utc::now().timestamp();
            db.conn().execute("INSERT INTO remembered_families(id,user_id,epoch,login_session,label,created_at,last_used_at,expires_at)
                VALUES(?2,?1,0,?2,'test',?3,?3,?4)", params![admin.id, session, now, now+3600]).unwrap();
            let proof = db.skill_trust_pin_reauthenticate(&admin.id, &session, 0, "test-password", "", &Cipher, now).unwrap();
            db.conn().execute(change, []).unwrap();
            assert!(db.skill_trust_pin_consume(&admin.id, &session, 0, &proof, now).is_err(), "{change}");
        }
    }
}
