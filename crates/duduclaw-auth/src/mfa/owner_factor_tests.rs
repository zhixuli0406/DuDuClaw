//! F6d owner factor proof: strongest factor only, purpose-bound single-use
//! passkey / email challenges, lockout counting. F6fa: the strongest factor
//! and the credential are the tombstone's ([`FactorBaseline`]), never a
//! factor enrolled afterwards.
use super::*;
use super::super::passkey::ceremony_tests::{register, register_key, Authenticator, ORIGIN};
use crate::admin_eligibility::writer_tests::{open, Cipher, PASSWORD};
use OwnerFactor::{Email, Passkey, Totp};
use OwnerFactorError::{Failed, NotPreexisting, NotStrongest};
use serde_json::json;

const EMPLOYEE: &str = "0123456789abcdef0123456789abcdef";

fn purpose(incarnation: u32) -> String { restore_purpose(EMPLOYEE, incarnation).unwrap() }

fn code<'a>(purpose: &'a str, code: &'a str) -> RestoreProof<'a> {
    RestoreProof { purpose, code, challenge_id: None, credential: None }
}
fn with_challenge<'a>(purpose: &'a str, id: &'a str, code: &'a str) -> RestoreProof<'a> {
    RestoreProof { purpose, code, challenge_id: Some(id), credential: None }
}
fn assertion<'a>(purpose: &'a str, id: &'a str, credential: &'a Value) -> RestoreProof<'a> {
    RestoreProof { purpose, code: "", challenge_id: Some(id), credential: Some(credential) }
}

fn member(db: &UserDb) -> (String, String) {
    let id = db.create_user("owner@example.test", "Owner", PASSWORD, crate::UserRole::Employee).unwrap().id;
    let (token, secret) = super::super::test_totp_prepare(db, &id, PASSWORD, &Cipher);
    db.mfa_confirm(&id, Some(&token), &super::super::test_totp_code(&secret, 100), &Cipher, 3000).unwrap();
    (id, secret)
}

/// An account whose only factor is the email code of its current address.
fn email_owner(db: &UserDb) -> String {
    let id = db.create_user("mail@example.test", "Mail", PASSWORD, crate::UserRole::Employee).unwrap().id;
    db.conn().execute("INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,'mail@example.test',1)", [&id]).unwrap();
    id
}

fn epoch(db: &UserDb, id: &str) -> u64 { db.get_user(id).unwrap().unwrap().credential_epoch }

/// The factor baseline a tombstone taken now would record.
fn base(db: &UserDb, id: &str) -> FactorBaseline { FactorBaseline::read(&db.conn(), id).unwrap() }

fn count(db: &UserDb, sql: &str, id: &str) -> i64 { db.conn().query_row(sql, [id], |r| r.get(0)).unwrap() }

fn email_challenge(db: &UserDb, id: &str, purpose: &str, baseline: &FactorBaseline, now: i64) -> (String, String) {
    match db.restore_challenge(id, epoch(db, id), purpose, baseline, None, true, now).unwrap() {
        RestoreChallenge::Email { challenge_id, email, code } => {
            assert_eq!(email, "mail@example.test");
            (challenge_id, code)
        }
        other => panic!("expected an email challenge, got {other:?}"),
    }
}

fn passkey_challenge(db: &UserDb, id: &str, purpose: &str, baseline: &FactorBaseline, now: i64) -> Value {
    match db.restore_challenge(id, epoch(db, id), purpose, baseline, Some(ORIGIN), true, now).unwrap() {
        RestoreChallenge::Passkey(options) => options,
        other => panic!("expected a passkey challenge, got {other:?}"),
    }
}

/// A six-digit code that is not `code`.
fn wrong(code: &str) -> &'static str { if code == "000000" { "111111" } else { "000000" } }

#[test]
fn restore_purpose_is_canonical() {
    assert_eq!(purpose(7), format!("restore:{EMPLOYEE}:7"));
    assert!(restore_purpose(&EMPLOYEE.to_uppercase(), 0).is_none());
    assert!(restore_purpose("abc", 0).is_none());
    let bad: Vec<String> = vec!["".into(), "restore:".into(), "restore:x:0".into(), format!("restore:{EMPLOYEE}:"),
        format!("restore:{EMPLOYEE}:01"), format!("restore:{EMPLOYEE}:0:1"), format!("login:{EMPLOYEE}:0"),
        "adopt:".into(), "adopt:x:0".into(), format!("adopt:{EMPLOYEE}:01"), format!("adopt:{EMPLOYEE}:0:1"),
        format!("adoptrestore:{EMPLOYEE}:0")];
    for purpose in &bad {
        assert!(!valid_purpose(purpose), "{purpose}");
    }
    assert!(valid_purpose(&purpose(0)));
    // F6g adoption: its own canonical purpose, never equal to the restore one.
    assert_eq!(adopt_purpose(EMPLOYEE, 7), Some(format!("adopt:{EMPLOYEE}:7")));
    assert!(adopt_purpose(&EMPLOYEE.to_uppercase(), 0).is_none());
    assert!(valid_purpose(&adopt_purpose(EMPLOYEE, 0).unwrap()));
    assert_ne!(adopt_purpose(EMPLOYEE, 0), restore_purpose(EMPLOYEE, 0));
}

#[test]
fn request_requires_strongest_factor() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let (id, secret) = member(&db);
    let e = epoch(&db, &id);
    let b = base(&db, &id);
    let p = purpose(0);
    // TOTP is the strongest: email is refused before any code check.
    assert_eq!(db.restore_owner_factor(&id, e, Email, &b, &code(&p, "123456"), &Cipher, None, 3030), Err(NotStrongest(Totp)));
    let right = super::super::test_totp_code(&secret, 101);
    assert_eq!(db.restore_owner_factor(&id, e, Totp, &b, &code(&p, &right), &Cipher, None, 3030), Ok(Totp));
    // A malformed purpose is refused before anything is read.
    assert_eq!(db.restore_owner_factor(&id, e, Totp, &b, &code("restore:x:0", &right), &Cipher, None, 3030),
        Err(OwnerFactorError::Denied));
    // No factor at the tombstone (F6fa: enrolling one afterwards never helps); a stale session epoch.
    let bare = db.create_user("bare@example.test", "Bare", PASSWORD, crate::UserRole::Employee).unwrap().id;
    let none = base(&db, &bare);
    assert_eq!(none.strongest, None);
    assert_eq!(db.restore_owner_factor(&bare, epoch(&db, &bare), Totp, &none, &code(&p, "123456"), &Cipher, None, 3060),
        Err(NotPreexisting));
    assert_eq!(db.restore_challenge(&bare, epoch(&db, &bare), &p, &none, Some(ORIGIN), true, 3060), Err(NotPreexisting));
    assert_eq!(db.restore_owner_factor(&id, e + 1, Totp, &b, &code(&p, "123456"), &Cipher, None, 3060), Err(OwnerFactorError::Denied));
}

/// Passkey owners prove with a WebAuthn assertion over the challenge of
/// `me.isolation.restore_challenge`, bound to user + purpose; the same
/// verifier, relying party and counter update as the passkey login.
#[test]
fn restore_request_accepts_passkey_assertion_bound_to_purpose() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let id = db.create_user("pk@example.test", "Key", PASSWORD, crate::UserRole::Employee).unwrap().id;
    let key = register(&db, &id, PASSWORD, 3000);
    let e = epoch(&db, &id);
    let bl = base(&db, &id);
    let (a, b) = (purpose(0), purpose(1));
    let begin = passkey_challenge(&db, &id, &a, &bl, 3010);
    assert_eq!(begin["publicKey"]["userVerification"], "required");
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let signed = key.assertion(&begin, 1, true);
    // Another purpose (incarnation) never finds the ceremony.
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &assertion(&b, &challenge, &signed), &Cipher, Some(ORIGIN), 3011), Err(Failed));
    // No configured origin: unavailable, nothing consumed.
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &assertion(&a, &challenge, &signed), &Cipher, None, 3011),
        Err(OwnerFactorError::Unavailable));
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &assertion(&a, &challenge, &signed), &Cipher, Some(ORIGIN), 3012), Ok(Passkey));
    // The live counter was updated (login verifier): an assertion that does
    // not advance it is refused.
    let begin = passkey_challenge(&db, &id, &a, &bl, 3015);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let stale = key.assertion(&begin, 1, true);
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &assertion(&a, &challenge, &stale), &Cipher, Some(ORIGIN), 3016), Err(Failed));
    // A proof without user verification, or without an assertion, fails.
    let begin = passkey_challenge(&db, &id, &a, &bl, 3020);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let unverified = key.assertion(&begin, 2, false);
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &assertion(&a, &challenge, &unverified), &Cipher, Some(ORIGIN), 3021), Err(Failed));
    let begin = passkey_challenge(&db, &id, &a, &bl, 3025);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &with_challenge(&a, &challenge, ""), &Cipher, Some(ORIGIN), 3026), Err(Failed));
    // Another account cannot use the owner's ceremony.
    let other = db.create_user("pk2@example.test", "Other", PASSWORD, crate::UserRole::Employee).unwrap().id;
    db.conn().execute("INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES('other-key',?1,'k','{}',1)", [&other]).unwrap();
    let other_base = base(&db, &other);
    let begin = passkey_challenge(&db, &id, &a, &bl, 3030);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let signed = key.assertion(&begin, 3, true);
    assert_eq!(db.restore_owner_factor(&other, epoch(&db, &other), Passkey, &other_base, &assertion(&a, &challenge, &signed), &Cipher,
        Some(ORIGIN), 3031), Err(Failed));
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &assertion(&a, &challenge, &signed), &Cipher, Some(ORIGIN), 3032), Ok(Passkey));
    // Failed proofs were counted toward the login lockout and audited; the
    // success reset the counter, as a verified TOTP does.
    assert_eq!(count(&db, "SELECT attempts FROM mfa_factor_failures WHERE user_id=?1", &id), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM auth_audit_log WHERE user_id=?1 AND action='isolation.restore_factor_failed'", &id), 4);
}

/// Email-strongest owners prove with a one-time code sent for this purpose.
#[test]
fn restore_request_accepts_email_code_bound_to_purpose() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let id = email_owner(&db);
    let e = epoch(&db, &id);
    let bl = base(&db, &id);
    let (a, b) = (purpose(0), purpose(1));
    // No sender: refused before a code or the send budget is spent.
    assert_eq!(db.restore_challenge(&id, e, &a, &bl, None, false, 3000), Err(OwnerFactorError::Unavailable));
    assert_eq!(count(&db, "SELECT count(*) FROM mfa_email_sends WHERE user_id=?1", &id), 0);
    let (c1, k1) = email_challenge(&db, &id, &a, &bl, 3000);
    // The code is never stored in clear; the login email budget applies.
    assert_eq!(db.conn().query_row("SELECT count(*) FROM mfa_restore_email_codes WHERE code_hash=?1 OR token_hash=?2",
        params![k1, c1], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.restore_challenge(&id, e, &a, &bl, None, true, 3010), Err(OwnerFactorError::RateLimited));
    // Bound to the purpose: the right code for another purpose fails and consumes it.
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&b, &c1, &k1), &Cipher, None, 3005), Err(Failed));
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &c1, &k1), &Cipher, None, 3006), Err(Failed));
    let (c2, k2) = email_challenge(&db, &id, &a, &bl, 3061);
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &c2, &k2), &Cipher, None, 3062), Ok(Email));
    // Wrong codes count toward the login lockout like TOTP; at the threshold
    // even a fresh challenge is refused.
    let (c3, k3) = email_challenge(&db, &id, &a, &bl, 3122);
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &c3, wrong(&k3)), &Cipher, None, 3123), Err(Failed));
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &c3, &k3), &Cipher, None, 3124), Err(Failed), "consumed");
    let unknown = "f".repeat(64);
    for now in 3125..3128 {
        assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &unknown, "123456"), &Cipher, None, now), Err(Failed));
    }
    assert_eq!(count(&db, "SELECT attempts FROM mfa_factor_failures WHERE user_id=?1", &id), 5);
    assert_eq!(db.restore_challenge(&id, e, &a, &bl, None, true, 3200), Err(Failed), "locked out");
    assert_eq!(count(&db, "SELECT count(*) FROM auth_audit_log WHERE user_id=?1 AND action='isolation.restore_factor_verified'", &id), 1);
}

#[test]
fn restore_challenge_single_use_and_expires() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let id = email_owner(&db);
    let e = epoch(&db, &id);
    let bl = base(&db, &id);
    let a = purpose(0);
    // Expired after the TTL.
    let (c, k) = email_challenge(&db, &id, &a, &bl, 3000);
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &c, &k), &Cipher, None, 3000 + RESTORE_CHALLENGE_TTL),
        Err(Failed));
    // A new challenge for the same purpose replaces the old one.
    let (old, old_code) = email_challenge(&db, &id, &a, &bl, 3400);
    let (new, new_code) = email_challenge(&db, &id, &a, &bl, 3461);
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &old, &old_code), &Cipher, None, 3462), Err(Failed));
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &new, &new_code), &Cipher, None, 3463), Ok(Email));
    // Single use.
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &new, &new_code), &Cipher, None, 3464), Err(Failed));
    // A delivery failure discards the code.
    let (lost, lost_code) = email_challenge(&db, &id, &a, &bl, 3522);
    db.restore_challenge_discard(&id, &lost).unwrap();
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &lost, &lost_code), &Cipher, None, 3523), Err(Failed));

    // Passkey: the ceremony expires after the TTL and is single use.
    let owner = db.create_user("pk@example.test", "Key", PASSWORD, crate::UserRole::Employee).unwrap().id;
    let key = register(&db, &owner, PASSWORD, 3000);
    let pe = epoch(&db, &owner);
    let pb = base(&db, &owner);
    let begin = passkey_challenge(&db, &owner, &a, &pb, 3010);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let signed = key.assertion(&begin, 1, true);
    assert_eq!(db.restore_owner_factor(&owner, pe, Passkey, &pb, &assertion(&a, &challenge, &signed), &Cipher, Some(ORIGIN),
        3010 + RESTORE_CHALLENGE_TTL), Err(Failed));
    let begin = passkey_challenge(&db, &owner, &a, &pb, 3400);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let signed = key.assertion(&begin, 2, true);
    assert_eq!(db.restore_owner_factor(&owner, pe, Passkey, &pb, &assertion(&a, &challenge, &signed), &Cipher, Some(ORIGIN), 3401), Ok(Passkey));
    assert_eq!(db.restore_owner_factor(&owner, pe, Passkey, &pb, &assertion(&a, &challenge, &signed), &Cipher, Some(ORIGIN), 3402), Err(Failed));
    // A credential change (new epoch) ends a live ceremony.
    let begin = passkey_challenge(&db, &owner, &a, &pb, 3410);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    db.change_own_password(&owner, "another-password").unwrap();
    let signed = key.assertion(&begin, 3, true);
    let pe2 = epoch(&db, &owner);
    assert_ne!(pe, pe2);
    assert_eq!(db.restore_owner_factor(&owner, pe2, Passkey, &pb, &assertion(&a, &challenge, &signed), &Cipher, Some(ORIGIN), 3411), Err(Failed));
}

/// passkey > TOTP > email: a weaker factor is refused, touches no counter
/// and no challenge, and no challenge is ever issued for it.
#[test]
fn restore_never_accepts_weaker_factor() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let (id, secret) = member(&db);
    db.conn().execute("INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,'owner@example.test',1)", [&id]).unwrap();
    let a = purpose(0);
    let e = epoch(&db, &id);
    let bl = base(&db, &id);
    // TOTP outranks email: the challenge is TOTP's (none), no code is sent.
    assert_eq!(db.restore_challenge(&id, e, &a, &bl, Some(ORIGIN), true, 3010), Ok(RestoreChallenge::Totp));
    assert_eq!(count(&db, "SELECT count(*) FROM mfa_email_sends WHERE user_id=?1", &id), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM mfa_restore_email_codes WHERE user_id=?1", &id), 0);
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &"a".repeat(64), "123456"), &Cipher, None, 3011),
        Err(NotStrongest(Totp)));
    // A passkey (enrolled before this tombstone) outranks TOTP: a correct
    // TOTP code and an email code are refused.
    let key = register(&db, &id, PASSWORD, 3020);
    let e = epoch(&db, &id);
    let bl = base(&db, &id);
    let begin = passkey_challenge(&db, &id, &a, &bl, 3030);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let totp = super::super::test_totp_code(&secret, 101);
    assert_eq!(db.restore_owner_factor(&id, e, Totp, &bl, &with_challenge(&a, &challenge, &totp), &Cipher, Some(ORIGIN), 3031),
        Err(NotStrongest(Passkey)));
    assert_eq!(db.restore_owner_factor(&id, e, Email, &bl, &with_challenge(&a, &challenge, "123456"), &Cipher, Some(ORIGIN), 3031),
        Err(NotStrongest(Passkey)));
    // Nothing counted, the TOTP step unused, the ceremony still live.
    let (failed, last_step): (i64, i64) = db.conn().query_row("SELECT failed_attempts,last_step FROM mfa_credentials WHERE user_id=?1",
        [&id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((failed, last_step), (0, 100));
    let signed = key.assertion(&begin, 1, true);
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &bl, &assertion(&a, &challenge, &signed), &Cipher, Some(ORIGIN), 3032), Ok(Passkey));
    // Without a configured origin a passkey owner is refused, never downgraded.
    assert_eq!(db.restore_challenge(&id, e, &a, &bl, None, true, 3040), Err(OwnerFactorError::Unavailable));
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
    let bl = base(&db, &id);
    let p = purpose(0);
    for _ in 0..5 {
        assert_eq!(db.restore_owner_factor(&id, e, Totp, &bl, &code(&p, "000000"), &Cipher, None, 3030), Err(Failed));
    }
    // The same counter a failed login uses.
    let (failed, attempts): (i64, i64) = db.conn().query_row(
        "SELECT c.failed_attempts,f.attempts FROM mfa_credentials c JOIN mfa_factor_failures f ON f.user_id=c.user_id
         WHERE c.user_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((failed, attempts), (5, 5));
    // Locked out: even the right code is refused now.
    let right = super::super::test_totp_code(&secret, 101);
    assert_eq!(db.restore_owner_factor(&id, e, Totp, &bl, &code(&p, &right), &Cipher, None, 3040), Err(Failed));
    assert_eq!(count(&db, "SELECT count(*) FROM auth_audit_log WHERE user_id=?1 AND action='isolation.restore_factor_failed'", &id), 6,
        "every failed proof is audited");
}

// ── F6fa: the tombstone's factor and credential, never a later one ──────

/// Review finding 1: an attacker holding the owner's session and password
/// enrols their own passkey after the tombstone (the writer stamps it
/// owner-authenticated). It is never usable: an owner with only TOTP at the
/// tombstone still needs that TOTP, and an owner with a passkey then gets a
/// ceremony offering only that passkey; an assertion of the new key fails.
#[test]
fn f6fa_newly_enrolled_passkey_refused_for_restore() {
    let p = purpose(0);
    // 1. TOTP at the tombstone; a passkey enrolled afterwards.
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let (id, secret) = member(&db);
    let tombstone = base(&db, &id);
    assert_eq!(tombstone.strongest, Some(Totp));
    let planted = register(&db, &id, PASSWORD, 3010);
    assert_eq!(strongest_factor(&db.conn(), &id).unwrap(), Some(Passkey), "the live account now has a passkey");
    let e = epoch(&db, &id);
    // The challenge is the tombstone's factor (TOTP: none), never a passkey ceremony.
    assert_eq!(db.restore_challenge(&id, e, &p, &tombstone, Some(ORIGIN), true, 3020), Ok(RestoreChallenge::Totp));
    assert_eq!(count(&db, "SELECT count(*) FROM mfa_passkey_ceremonies WHERE user_id=?1", &id), 0);
    let forged = planted.assertion(&json!({"publicKey": {"challenge": "x"}}), 1, true);
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &tombstone, &assertion(&p, &"c".repeat(64), &forged), &Cipher,
        Some(ORIGIN), 3021), Err(NotPreexisting));
    assert_eq!(count(&db, "SELECT count(*) FROM auth_audit_log WHERE user_id=?1 AND action='isolation.restore_factor_failed'", &id), 0,
        "refused before any proof is checked");
    // The owner's own TOTP still proves it.
    let right = super::super::test_totp_code(&secret, 101);
    assert_eq!(db.restore_owner_factor(&id, e, Totp, &tombstone, &code(&p, &right), &Cipher, None, 3030), Ok(Totp));

    // 2. A passkey at the tombstone; a second one enrolled afterwards.
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let id = db.create_user("pk@example.test", "Key", PASSWORD, crate::UserRole::Employee).unwrap().id;
    let own = register(&db, &id, PASSWORD, 3000);
    let tombstone = base(&db, &id);
    assert_eq!((tombstone.strongest, tombstone.passkeys.len()), (Some(Passkey), 1));
    let planted = register_key(&db, &id, PASSWORD, 3005, Authenticator::with_id(7));
    assert_eq!(base(&db, &id).passkeys.len(), 2);
    let e = epoch(&db, &id);
    let begin = passkey_challenge(&db, &id, &p, &tombstone, 3010);
    assert_eq!(begin["publicKey"]["allowCredentials"].as_array().map(Vec::len), Some(1), "only the tombstone's passkey is offered");
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let forged = planted.assertion(&begin, 1, true);
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &tombstone, &assertion(&p, &challenge, &forged), &Cipher, Some(ORIGIN), 3011),
        Err(Failed));
    // The tombstone's passkey proves it.
    let begin = passkey_challenge(&db, &id, &p, &tombstone, 3020);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let signed = own.assertion(&begin, 1, true);
    assert_eq!(db.restore_owner_factor(&id, e, Passkey, &tombstone, &assertion(&p, &challenge, &signed), &Cipher, Some(ORIGIN), 3021),
        Ok(Passkey));
    // With the tombstone's passkey removed, nothing remains to prove with.
    db.conn().execute("DELETE FROM mfa_passkeys WHERE id=?1", [&tombstone.passkeys[0]]).unwrap();
    assert_eq!(db.restore_challenge(&id, epoch(&db, &id), &p, &tombstone, Some(ORIGIN), true, 3030), Err(NotPreexisting));
}

/// A TOTP re-enrolled after the tombstone (another secret under the same
/// factor kind) is not the tombstone's TOTP: a valid code of the new secret
/// is refused before it is checked, and no challenge is issued.
#[test]
fn f6fa_reenrolled_totp_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let (id, _) = member(&db);
    let tombstone = base(&db, &id);
    // A valid secret the attacker controls, enrolled for the owner (delete +
    // insert, as a disable then re-enrol does).
    let other = db.create_user("attacker@example.test", "Attacker", PASSWORD, crate::UserRole::Employee).unwrap().id;
    let (_, planted) = super::super::test_totp_prepare(&db, &other, PASSWORD, &Cipher);
    let sealed = Cipher.encrypt(&serde_json::to_string(&(id.as_str(), planted.as_str())).unwrap()).unwrap();
    db.conn().execute("DELETE FROM mfa_credentials WHERE user_id=?1", [&id]).unwrap();
    db.conn().execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,?2,0)", params![id, sealed]).unwrap();
    assert_ne!(base(&db, &id).totp, tombstone.totp);
    let e = epoch(&db, &id);
    let p = purpose(0);
    assert_eq!(db.restore_challenge(&id, e, &p, &tombstone, Some(ORIGIN), true, 3020), Err(NotPreexisting));
    let code_of_new = super::super::test_totp_code(&planted, 101);
    assert_eq!(db.restore_owner_factor(&id, e, Totp, &tombstone, &code(&p, &code_of_new), &Cipher, None, 3030), Err(NotPreexisting));
    let (failed, last_step): (i64, i64) = db.conn().query_row("SELECT failed_attempts,last_step FROM mfa_credentials WHERE user_id=?1",
        [&id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((failed, last_step), (0, 0), "the new secret was never checked");
}

/// The owner's own factor, unchanged since the tombstone, proves it — also
/// after the owner's own later password change (owner decision Q3) and when
/// the owner adds another factor afterwards.
#[test]
fn f6fa_preexisting_factor_accepted() {
    let p = purpose(0);
    // TOTP.
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let (id, secret) = member(&db);
    let tombstone = base(&db, &id);
    db.change_own_password(&id, "owner-new-password").unwrap();
    db.conn().execute("INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,'owner@example.test',1)", [&id]).unwrap();
    let e = epoch(&db, &id);
    assert_eq!(db.restore_challenge(&id, e, &p, &tombstone, Some(ORIGIN), true, 3020), Ok(RestoreChallenge::Totp));
    let right = super::super::test_totp_code(&secret, 101);
    assert_eq!(db.restore_owner_factor(&id, e, Totp, &tombstone, &code(&p, &right), &Cipher, None, 3030), Ok(Totp));
    // Email (the address recorded at the tombstone).
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let id = email_owner(&db);
    let tombstone = base(&db, &id);
    assert_eq!(tombstone.strongest, Some(Email));
    let (challenge, sent) = email_challenge(&db, &id, &p, &tombstone, 3000);
    assert_eq!(db.restore_owner_factor(&id, epoch(&db, &id), Email, &tombstone, &with_challenge(&p, &challenge, &sent), &Cipher, None, 3001),
        Ok(Email));
    // Another address now (the factor follows the account address): not the
    // tombstone's email factor.
    db.conn().execute("UPDATE users SET email='elsewhere@example.test' WHERE id=?1", [&id]).unwrap();
    db.conn().execute("INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,'elsewhere@example.test',2)", [&id]).unwrap();
    assert_eq!(db.restore_challenge(&id, epoch(&db, &id), &p, &tombstone, None, true, 3100), Err(NotPreexisting));
    // Passkey, with a second one added later by the owner.
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let id = db.create_user("pk@example.test", "Key", PASSWORD, crate::UserRole::Employee).unwrap().id;
    let own = register(&db, &id, PASSWORD, 3000);
    let tombstone = base(&db, &id);
    register_key(&db, &id, PASSWORD, 3005, Authenticator::with_id(9));
    let begin = passkey_challenge(&db, &id, &p, &tombstone, 3010);
    let challenge = begin["challenge_id"].as_str().unwrap().to_owned();
    let signed = own.assertion(&begin, 1, true);
    assert_eq!(db.restore_owner_factor(&id, epoch(&db, &id), Passkey, &tombstone, &assertion(&p, &challenge, &signed), &Cipher,
        Some(ORIGIN), 3011), Ok(Passkey));
}

/// A snapshot recorded before F6fa carries no factor identities: there is
/// no baseline, and nothing falls back to the live account. Malformed or
/// self-inconsistent baselines are refused the same way.
#[test]
fn f6fa_legacy_snapshot_without_factor_ids_refused() {
    let legacy = json!({"kind": "personal", "owner_user_id": "u", "credential_epoch": 3, "email": "a@example.test",
        "google_sub": null, "factors_digest": "0".repeat(64), "notice_address": "a@example.test"});
    assert_eq!(FactorBaseline::from_snapshot(&legacy), None);
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let (id, _) = member(&db);
    let good = base(&db, &id);
    let mut snapshot = legacy.clone();
    snapshot["owner_factors"] = good.to_value();
    assert_eq!(FactorBaseline::from_snapshot(&snapshot), Some(good.clone()), "round trip");
    let mutations: [fn(&mut Value); 7] = [
        |v| v["owner_factors"]["version"] = json!(2),
        |v| v["owner_factors"]["strongest"] = json!("passkey"),
        |v| v["owner_factors"]["strongest"] = json!(null),
        |v| v["owner_factors"]["totp"] = json!("not-a-digest"),
        |v| v["owner_factors"]["passkeys"] = json!([""]),
        |v| v["owner_factors"]["extra"] = json!(1),
        |v| v["owner_factors"] = json!("v1"),
    ];
    for mutate in mutations {
        let mut bad = snapshot.clone();
        mutate(&mut bad);
        assert_eq!(FactorBaseline::from_snapshot(&bad), None, "{bad}");
    }
    assert_eq!(OwnerFactorError::BaselineMissing.code(), "FACTOR_BASELINE_MISSING");
    assert_eq!(OwnerFactorError::NotPreexisting.code(), "FACTOR_NOT_PREEXISTING");
}
