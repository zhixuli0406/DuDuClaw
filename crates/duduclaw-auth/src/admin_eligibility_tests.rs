//! F6 S1/S16: every eligibility change of an Admin is logged in the writer's
//! own transaction; lockout is logged without changing eligibility; the log
//! is append-only, seeded once and refuses a future schema.
use super::*;
use crate::{UserDb, UserRole, UserStatus, UserUpdate};

fn open(dir: &tempfile::TempDir) -> UserDb { UserDb::new(&dir.path().join("users.db")).unwrap() }

fn rows_for(db: &UserDb, user: &str) -> Vec<EligibilityEntry> {
    db.admin_eligibility_entries().unwrap().into_iter().filter(|e| e.user_id == user).collect()
}

fn last(db: &UserDb, user: &str) -> EligibilityEntry { rows_for(db, user).pop().expect("a log row") }

fn exec(db: &UserDb, sql: &str, user: &str) { db.conn().execute(sql, [user]).unwrap(); }

fn enroll_totp(db: &UserDb, user: &str) {
    exec(db, "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'enc',0)", user);
}

#[test]
fn admin_eligibility_log_written_on_every_change() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let actor_id = db.create_user("root@example.test", "Root", "password1", UserRole::Admin).unwrap().id;
    let actor = actor_id.as_str();
    let admin = db.create_user("admin@example.test", "Admin", "password1", UserRole::Admin).unwrap().id;
    // Creation: an Admin without a second factor is not eligible.
    let created = last(&db, &admin);
    assert_eq!((created.reason.as_str(), created.eligible), ("admin_created", false));
    assert_eq!(created.notice_address.as_deref(), Some("admin@example.test"));
    // MFA enrolment makes it eligible.
    enroll_totp(&db, &admin);
    assert_eq!((last(&db, &admin).reason.as_str(), last(&db, &admin).eligible), ("mfa_enrolled", true));
    assert!(is_eligible(&db.conn(), &admin).unwrap());
    // Forced password change: ineligible, then eligible again.
    exec(&db, "UPDATE users SET must_change_password=1 WHERE id=?1", &admin);
    assert_eq!((last(&db, &admin).reason.as_str(), last(&db, &admin).eligible), ("forced_password", false));
    exec(&db, "UPDATE users SET must_change_password=0 WHERE id=?1", &admin);
    assert!(last(&db, &admin).eligible);
    // Status change by an attributed actor.
    db.set_user_status_as(Some(actor), &admin, UserStatus::Suspended).unwrap();
    let suspended = last(&db, &admin);
    assert_eq!((suspended.reason.as_str(), suspended.eligible), ("status", false));
    assert_eq!(suspended.actor_user_id.as_deref(), Some(actor));
    db.set_user_status(&admin, UserStatus::Active).unwrap();
    assert_eq!(last(&db, &admin).actor_user_id, None, "unattributed writer");
    assert!(last(&db, &admin).eligible);
    // Demotion and promotion record the actor.
    db.apply_user_update_as(Some(actor), &admin, &UserUpdate { role: Some(UserRole::Employee), ..Default::default() }).unwrap();
    let demoted = last(&db, &admin);
    assert_eq!((demoted.reason.as_str(), demoted.eligible, demoted.actor_user_id.as_deref()), ("role", false, Some(actor)));
    db.apply_user_update_as(Some(actor), &admin, &UserUpdate { role: Some(UserRole::Admin), ..Default::default() }).unwrap();
    assert_eq!((last(&db, &admin).reason.as_str(), last(&db, &admin).eligible), ("role", true));
    // Passkey and email factors.
    exec(&db, "INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES('pk',?1,'key','{}',0)", &admin);
    assert_eq!(last(&db, &admin).reason, "passkey_added");
    exec(&db, "DELETE FROM mfa_passkeys WHERE user_id=?1", &admin);
    assert_eq!(last(&db, &admin).reason, "passkey_removed");
    exec(&db, "INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,'admin@example.test',0)", &admin);
    assert_eq!(last(&db, &admin).reason, "email_factor_added");
    // Lockout is logged and never changes eligibility.
    exec(&db, "UPDATE mfa_credentials SET failed_attempts=5,failure_window=1 WHERE user_id=?1", &admin);
    let locked = last(&db, &admin);
    assert_eq!((locked.reason.as_str(), locked.eligible), ("lockout", true));
    exec(&db, "INSERT INTO mfa_factor_failures(user_id,attempts,window) VALUES(?1,5,1)", &admin);
    assert_eq!((last(&db, &admin).reason.as_str(), last(&db, &admin).eligible), ("lockout", true));
    // Removing every factor (admin MFA reset) makes it ineligible.
    exec(&db, "DELETE FROM mfa_email_credentials WHERE user_id=?1", &admin);
    exec(&db, "DELETE FROM mfa_credentials WHERE user_id=?1", &admin);
    assert_eq!((last(&db, &admin).reason.as_str(), last(&db, &admin).eligible), ("mfa_removed", false));
    // Offboarding by an actor.
    enroll_totp(&db, &admin);
    db.offboard_user(&admin, None, Some(actor)).unwrap();
    let offboarded = last(&db, &admin);
    assert_eq!((offboarded.reason.as_str(), offboarded.eligible, offboarded.actor_user_id.as_deref()),
        ("status", false, Some(actor)));
    // Deletion keeps the account (and its notice address) in the log.
    exec(&db, "DELETE FROM users WHERE id=?1", &admin);
    let deleted = last(&db, &admin);
    assert_eq!((deleted.reason.as_str(), deleted.eligible), ("deleted", false));
    assert_eq!(deleted.notice_address.as_deref(), Some("admin@example.test"));
    // An Admin created by an Admin through the member flow records its creator.
    let creator = db.get_user(actor).unwrap().unwrap();
    let member = crate::members::NewMember { display_name: "New".into(), email: "new-admin@example.test".into(),
        role: UserRole::Admin, department: None, primary_agent_id: "main".into(), invite: false,
        bindings: vec![crate::members::MemberBinding { agent_name: "main".into(), access_level: crate::AccessLevel::Owner }] };
    if let Ok(created) = db.create_member_at_epoch(&member, &creator.id, creator.credential_epoch) {
        assert_eq!(last(&db, &created.user.id).actor_user_id.as_deref(), Some(actor));
    }
    let made = db.create_user_as(Some(actor), &crate::NewUser { email: "third@example.test", display_name: "Third",
        password: "password1", role: UserRole::Admin, department: None, manager: None }, false).unwrap();
    assert_eq!(last(&db, &made.id).actor_user_id.as_deref(), Some(actor));
    // Changes of non-Admins are not eligibility changes.
    let staff = db.create_user("staff@example.test", "Staff", "password1", UserRole::Employee).unwrap().id;
    db.set_user_status(&staff, UserStatus::Suspended).unwrap();
    enroll_totp(&db, &staff);
    assert!(rows_for(&db, &staff).is_empty());
}

#[test]
fn admin_eligibility_log_shares_the_writer_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let admin = db.create_user("admin@example.test", "Admin", "password1", UserRole::Admin).unwrap().id;
    let before = db.admin_eligibility_entries().unwrap().len();
    {
        let mut conn = db.conn();
        let tx = conn.transaction().unwrap();
        tx.execute("UPDATE users SET role='employee' WHERE id=?1", [&admin]).unwrap();
        tx.execute("DELETE FROM users WHERE id=?1", [&admin]).unwrap();
        // Rolled back: neither the change nor its log rows exist.
        tx.rollback().unwrap();
    }
    assert_eq!(db.admin_eligibility_entries().unwrap().len(), before);
    // A failed change after the log row was appended aborts both.
    db.conn().execute_batch("CREATE TRIGGER fail_after_log AFTER INSERT ON admin_eligibility_log
        WHEN NEW.reason='status' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(db.set_user_status(&admin, UserStatus::Suspended).is_err());
    assert_eq!(db.get_user(&admin).unwrap().unwrap().status, UserStatus::Active);
    assert_eq!(db.admin_eligibility_entries().unwrap().len(), before);
}

#[test]
fn admin_eligibility_log_is_append_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let admin = db.create_user("admin@example.test", "Admin", "password1", UserRole::Admin).unwrap().id;
    let row = last(&db, &admin);
    let conn = db.conn();
    assert!(conn.execute("DELETE FROM admin_eligibility_log", []).is_err());
    assert!(conn.execute("UPDATE admin_eligibility_log SET eligible=1 WHERE seq=?1", [row.seq]).is_err());
    assert!(conn.execute("UPDATE admin_eligibility_log SET at=0 WHERE seq=?1", [row.seq]).is_err());
    // The actor may be stamped once, never changed or cleared.
    stamp_actor(&conn, row.seq - 1, "someone").unwrap();
    assert!(conn.execute("UPDATE admin_eligibility_log SET actor_user_id='other' WHERE seq=?1", [row.seq]).is_err());
    assert!(conn.execute("UPDATE admin_eligibility_log SET actor_user_id=NULL WHERE seq=?1", [row.seq]).is_err());
    drop(conn);
    assert_eq!(last(&db, &admin).actor_user_id.as_deref(), Some("someone"));
}

#[test]
fn admin_eligibility_log_seeded_once_and_future_schema_refused() {
    let dir = tempfile::tempdir().unwrap();
    let admin = {
        let db = open(&dir);
        let admin = db.create_user("admin@example.test", "Admin", "password1", UserRole::Admin).unwrap().id;
        enroll_totp(&db, &admin);
        // Simulate a users.db from before the log existed.
        db.conn().execute_batch("DROP TABLE admin_eligibility_log;
            DELETE FROM auth_meta WHERE key IN ('admin_eligibility_log_schema','admin_eligibility_log_since');").unwrap();
        admin
    };
    let db = open(&dir);
    let seeded = rows_for(&db, &admin);
    assert_eq!(seeded.len(), 1);
    assert_eq!((seeded[0].reason.as_str(), seeded[0].eligible), ("seed", true));
    assert!(log_since(&db.conn()).unwrap().is_some());
    drop(db);
    // Re-opening is idempotent: no second seed.
    let db = open(&dir);
    assert_eq!(rows_for(&db, &admin).len(), 1);
    db.conn().execute("UPDATE auth_meta SET value=?1 WHERE key='admin_eligibility_log_schema'", [SCHEMA_VERSION + 1]).unwrap();
    drop(db);
    assert!(UserDb::new(&dir.path().join("users.db")).is_err(), "a newer log schema is refused");
}

/// The set-valued readers (`eligible_user_ids`, the migration seed) apply the
/// rule per account, not one arbitrary row's verdict to every account.
#[test]
fn eligible_user_ids_and_seed_are_per_account() {
    let dir = tempfile::tempdir().unwrap();
    let (eligible, others) = {
        let db = open(&dir);
        let eligible = db.create_user("a@example.test", "A", "password1", UserRole::Admin).unwrap().id;
        enroll_totp(&db, &eligible);
        let no_mfa = db.create_user("nomfa@example.test", "N", "password1", UserRole::Admin).unwrap().id;
        let employee = db.create_user("emp@example.test", "E", "password1", UserRole::Employee).unwrap().id;
        enroll_totp(&db, &employee);
        let suspended = db.create_user("s@example.test", "S", "password1", UserRole::Admin).unwrap().id;
        enroll_totp(&db, &suspended);
        db.set_user_status(&suspended, UserStatus::Suspended).unwrap();
        let forced = db.create_user("f@example.test", "F", "password1", UserRole::Admin).unwrap().id;
        enroll_totp(&db, &forced);
        exec(&db, "UPDATE users SET must_change_password=1 WHERE id=?1", &forced);
        let expected: std::collections::BTreeSet<String> = [eligible.clone()].into_iter().collect();
        assert_eq!(eligible_user_ids(&db.conn()).unwrap(), expected);
        db.conn().execute_batch("DROP TABLE admin_eligibility_log;
            DELETE FROM auth_meta WHERE key IN ('admin_eligibility_log_schema','admin_eligibility_log_since');").unwrap();
        (eligible, [no_mfa, employee, suspended, forced])
    };
    let db = open(&dir);
    assert_eq!(rows_for(&db, &eligible).len(), 1, "the eligible Admin is seeded");
    for other in &others {
        assert!(rows_for(&db, other).is_empty(), "an ineligible account is not seeded");
    }
}
