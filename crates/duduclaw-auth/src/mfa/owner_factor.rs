//! Owner factor proof for a Personal restore request (native Linux isolation
//! v1.81, F6 design §3.4 "Personal adds", owner decision Q2, task F6d).
//!
//! `me.isolation.request_restore` / `cancel_restore` need a fresh proof of the
//! owner's **strongest** enrolled second factor, in the call itself:
//! passkey > TOTP > email code. Presenting a weaker one than the strongest
//! enrolled is refused ([`OwnerFactorError::NotStrongest`]) without touching
//! any counter. A wrong code goes through the login factor check
//! (`check_factor`), so it counts toward the same lockout counter as a failed
//! login (`mfa_credentials.failed_attempts` / `mfa_factor_failures`, L1), and
//! an account at the lockout threshold is refused even with a correct code.
//!
//! Only TOTP has an in-call proof path in this build. An owner whose
//! strongest factor is a passkey or the email code is refused
//! ([`OwnerFactorError::Unsupported`]) — fail closed, never a fallback to a
//! weaker factor; the WebAuthn / email-code ceremonies for this RPC are left
//! for a follow-up (F6d report).
use super::*;

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
    /// Not an active account at this credential epoch, or a forced password change.
    Denied,
    /// No second factor enrolled (請先設定兩步驟驗證).
    NoFactor,
    /// A stronger factor is enrolled; the request must use it.
    NotStrongest(OwnerFactor),
    /// The strongest factor has no proof path in this RPC yet.
    Unsupported(OwnerFactor),
    /// Wrong code (counted toward the login lockout) or locked out.
    Failed,
    Unavailable,
}

impl OwnerFactorError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Denied => "FACTOR_DENIED",
            Self::NoFactor => "FACTOR_NOT_ENROLLED",
            Self::NotStrongest(_) => "FACTOR_NOT_STRONGEST",
            Self::Unsupported(_) => "FACTOR_UNSUPPORTED",
            Self::Failed => "FACTOR_FAILED",
            Self::Unavailable => "FACTOR_UNAVAILABLE",
        }
    }
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

impl UserDb {
    /// Verify the owner's strongest factor for a restore request. `epoch` is
    /// the session's credential epoch. Every attempt that reaches the code
    /// check is committed (counter and audit), success or not.
    pub fn restore_owner_factor(&self, user: &str, epoch: u64, presented: OwnerFactor, code: &str,
        cipher: &dyn MfaCipher, now: i64) -> std::result::Result<OwnerFactor, OwnerFactorError> {
        use OwnerFactorError::*;
        if user.is_empty() || code.len() > 64 || now <= 0 { return Err(Denied); }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| Unavailable)?;
        let account = crate::user_row::load_user(&tx, user).map_err(|_| Unavailable)?.ok_or(Denied)?;
        if account.id != user || account.status != crate::UserStatus::Active || account.must_change_password
            || !credential_epoch_matches(epoch, account.credential_epoch) {
            return Err(Denied);
        }
        let strongest = strongest_factor(&tx, user).map_err(|_| Unavailable)?.ok_or(NoFactor)?;
        if presented != strongest { return Err(NotStrongest(strongest)); }
        if strongest != OwnerFactor::Totp { return Err(Unsupported(strongest)); }
        let verified = check_factor(&tx, user, code, false, cipher, now).is_ok();
        audit(&tx, user, if verified { "isolation.restore_factor_verified" } else { "isolation.restore_factor_failed" }, None)
            .map_err(|_| Unavailable)?;
        tx.commit().map_err(|_| Unavailable)?;
        if verified { Ok(OwnerFactor::Totp) } else { Err(Failed) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin_eligibility::writer_tests::{open, Cipher, PASSWORD};

    fn member(db: &UserDb) -> (String, String) {
        let id = db.create_user("owner@example.test", "Owner", PASSWORD, crate::UserRole::Employee).unwrap().id;
        let (token, secret) = super::super::test_totp_prepare(db, &id, PASSWORD, &Cipher);
        db.mfa_confirm(&id, Some(&token), &super::super::test_totp_code(&secret, 100), &Cipher, 3000).unwrap();
        (id, secret)
    }

    fn epoch(db: &UserDb, id: &str) -> u64 { db.get_user(id).unwrap().unwrap().credential_epoch }

    #[test]
    fn request_requires_strongest_factor() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir);
        let (id, secret) = member(&db);
        let e = epoch(&db, &id);
        // TOTP is the strongest: email is refused before any code check.
        assert_eq!(db.restore_owner_factor(&id, e, OwnerFactor::Email, "123456", &Cipher, 3030),
            Err(OwnerFactorError::NotStrongest(OwnerFactor::Totp)));
        assert_eq!(db.restore_owner_factor(&id, e, OwnerFactor::Totp, &super::super::test_totp_code(&secret, 101), &Cipher, 3030),
            Ok(OwnerFactor::Totp));
        // A passkey outranks TOTP: TOTP is no longer enough, and the passkey
        // has no proof path here (fail closed, never a fallback).
        db.conn().execute("INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES('pk1',?1,'key','{}',1)", [&id]).unwrap();
        assert_eq!(db.restore_owner_factor(&id, e, OwnerFactor::Totp, &super::super::test_totp_code(&secret, 102), &Cipher, 3060),
            Err(OwnerFactorError::NotStrongest(OwnerFactor::Passkey)));
        assert_eq!(db.restore_owner_factor(&id, e, OwnerFactor::Passkey, "x", &Cipher, 3060),
            Err(OwnerFactorError::Unsupported(OwnerFactor::Passkey)));
        // No factor at all; a stale session epoch.
        let bare = db.create_user("bare@example.test", "Bare", PASSWORD, crate::UserRole::Employee).unwrap().id;
        assert_eq!(db.restore_owner_factor(&bare, epoch(&db, &bare), OwnerFactor::Totp, "123456", &Cipher, 3060),
            Err(OwnerFactorError::NoFactor));
        assert_eq!(db.restore_owner_factor(&id, e + 1, OwnerFactor::Passkey, "x", &Cipher, 3060), Err(OwnerFactorError::Denied));
    }

    /// Q3 (F6d): the owner's own self-service changes are stamped
    /// owner-authenticated in the writer's transaction; an Admin-side write of
    /// the same field stays unattributed (so a Personal restore refuses it).
    #[test]
    fn owner_self_credential_change_is_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir);
        let (id, _) = member(&db);
        db.change_own_password(&id, "owner-new-password").unwrap();
        db.change_own_email(&id, "owner-new@example.test").unwrap();
        db.update_user(&id, None, None, Some("admin-set-password")).unwrap();
        let changes = crate::credential_changes::changes_since(&db.conn(), &id, 0, 100).unwrap();
        let seen: Vec<(&str, bool)> = changes.iter().map(|c| (c.field.as_str(), c.by_owner())).collect();
        assert_eq!(seen, vec![("totp", true), ("password", true), ("email", true), ("password", false)], "{changes:?}");
        // Only the account's own rows are ever stamped by its writer.
        let other = db.create_user("other@example.test", "Other", PASSWORD, crate::UserRole::Employee).unwrap().id;
        db.update_user(&other, None, None, Some("admin-set-password")).unwrap();
        db.change_own_password(&id, "owner-third-password").unwrap();
        let others = crate::credential_changes::changes_since(&db.conn(), &other, 0, 100).unwrap();
        assert!(others.iter().all(|c| !c.by_owner()), "{others:?}");
    }

    #[test]
    fn factor_failures_count_toward_lockout() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir);
        let (id, secret) = member(&db);
        let e = epoch(&db, &id);
        for _ in 0..5 {
            assert_eq!(db.restore_owner_factor(&id, e, OwnerFactor::Totp, "000000", &Cipher, 3030), Err(OwnerFactorError::Failed));
        }
        // The same counter a failed login uses.
        let (failed, attempts): (i64, i64) = db.conn().query_row(
            "SELECT c.failed_attempts,f.attempts FROM mfa_credentials c JOIN mfa_factor_failures f ON f.user_id=c.user_id
             WHERE c.user_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((failed, attempts), (5, 5));
        // Locked out: even the right code is refused now.
        assert_eq!(db.restore_owner_factor(&id, e, OwnerFactor::Totp, &super::super::test_totp_code(&secret, 101), &Cipher, 3040),
            Err(OwnerFactorError::Failed));
        let failures: i64 = db.conn().query_row(
            "SELECT count(*) FROM auth_audit_log WHERE user_id=?1 AND action='isolation.restore_factor_failed'", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(failures, 6, "every failed proof is audited");
    }
}
