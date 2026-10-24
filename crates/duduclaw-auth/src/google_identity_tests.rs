use super::*;

fn person(db: &UserDb, email: &str) -> User {
    db.create_user(email, email, "fixture-password-only", UserRole::Employee)
        .unwrap()
}

#[test]
fn primary_owner_is_explicit_stable_and_only_an_owner() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@c.test", "Admin", "fixture-password-only", UserRole::Admin).unwrap();
    let alice = person(&db, "alice@c.test");
    let bob = person(&db, "bob@c.test");
    db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
    assert_eq!(db.google_owner("bot").unwrap().id, alice.id);
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    assert_eq!(
        db.google_owner("bot").unwrap().id,
        alice.id,
        "second owner must not silently change identity"
    );
    assert!(
        db.get_agent_users("bot")
            .unwrap()
            .iter()
            .find(|b| b.user_id == alice.id)
            .unwrap()
            .is_primary
    );
    db.set_google_primary_owner("bot", &bob.id, &admin.id)
        .unwrap();
    assert_eq!(db.google_owner("bot").unwrap().id, bob.id);
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    assert_eq!(
        db.google_owner("bot").unwrap().id,
        bob.id,
        "binding update keeps primary"
    );
    db.bind_agent(&bob.id, "bot", AccessLevel::Viewer).unwrap();
    assert!(
        db.set_google_primary_owner("bot", &bob.id, &admin.id)
            .is_err()
    );
    assert_eq!(db.google_owner("bot").unwrap().id, alice.id);
}

#[test]
fn ambiguous_legacy_bindings_and_nonowners_refuse() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@c.test", "Admin", "fixture-password-only", UserRole::Admin).unwrap();
    let alice = person(&db, "alice@c.test");
    let bob = person(&db, "bob@c.test");
    db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    db.conn()
        .execute("UPDATE user_agent_bindings SET is_primary = 0", [])
        .unwrap();
    assert_eq!(db.google_owner("bot").unwrap_err(), OwnerError::Ambiguous);
    db.set_google_primary_owner("bot", &alice.id, &admin.id)
        .unwrap();
    db.unbind_agent(&alice.id, "bot").unwrap();
    assert_eq!(db.google_owner("bot").unwrap().id, bob.id);
    db.unbind_agent(&bob.id, "bot").unwrap();
    assert_eq!(db.google_owner("bot").unwrap_err(), OwnerError::Missing);
}

#[test]
fn only_the_active_person_can_consent_to_an_owned_employee() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let alice = person(&db, "alice@c.test");
    db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), None);
    assert!(
        db.set_google_consent(&alice.id, "bot", true, "admin")
            .is_err()
    );
    db.set_google_consent(&alice.id, "bot", true, &alice.id)
        .unwrap();
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), Some(true));
    db.set_google_consent(&alice.id, "bot", false, &alice.id)
        .unwrap();
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), Some(false));
    db.set_user_status(&alice.id, UserStatus::Suspended)
        .unwrap();
    assert!(
        db.set_google_consent(&alice.id, "bot", true, &alice.id)
            .is_err()
    );
    db.set_user_status(&alice.id, UserStatus::Active).unwrap();
    db.unbind_agent(&alice.id, "bot").unwrap();
    assert!(
        db.set_google_consent(&alice.id, "bot", true, &alice.id)
            .is_err()
    );
}


fn primary_generation_fixture() -> (tempfile::TempDir, UserDb, User, User, User) {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let alice = person(&db, "alice@generation.test");
    let bob = person(&db, "bob@generation.test");
    let admin = db.create_user("admin@generation.test", "Admin", "fixture-password-only", UserRole::Admin).unwrap();
    db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    (home, db, alice, bob, admin)
}

#[test]
fn primary_generation_legacy_zero_is_read_only_and_missing_table_fails_closed() {
    let (home, db, _, _, _) = primary_generation_fixture();
    let conn = Connection::open_with_flags(home.path().join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(read_primary_owner_generation(&conn, "bot").unwrap(), 0);
    assert_eq!(db.google_primary_owner_generation("bot").unwrap(), 0);
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM auth_meta WHERE key = ?1", [primary_owner_generation_key("bot")], |r| r.get::<_, i64>(0)).unwrap(), 0);
    drop(conn);
    db.conn().execute("DROP TABLE auth_meta", []).unwrap();
    assert!(db.google_primary_owner_generation("bot").is_err());
}

#[test]
fn primary_generation_increments_for_every_primary_write_without_touching_another_agent() {
    let (_home, db, alice, bob, admin) = primary_generation_fixture();
    db.bind_agent(&alice.id, "other", AccessLevel::Owner).unwrap();
    assert_eq!(db.google_primary_owner_generation("bot").unwrap(), 0);
    for (index, owner) in [&bob.id, &alice.id, &alice.id].iter().enumerate() {
        db.set_google_primary_owner("bot", owner, &admin.id).unwrap();
        assert_eq!(db.google_primary_owner_generation("bot").unwrap(), index as u64 + 1);
        assert_eq!(db.google_primary_owner_generation("other").unwrap(), 0);
        assert_eq!(db.google_owner("other").unwrap().id, alice.id);
    }
}

#[test]
fn primary_generation_refused_candidate_or_actor_does_not_increment() {
    let (_home, db, alice, bob, admin) = primary_generation_fixture();
    db.set_google_primary_owner("bot", &bob.id, &admin.id).unwrap();
    let before = db.google_primary_owner_generation("bot").unwrap();
    assert!(db.set_google_primary_owner("bot", "missing-owner", &admin.id).is_err());
    assert!(db.set_google_primary_owner("bot", &alice.id, "missing-admin").is_err());
    assert_eq!(db.google_primary_owner_generation("bot").unwrap(), before);
    assert_eq!(db.google_owner("bot").unwrap().id, bob.id);
}

#[test]
fn primary_generation_rolls_back_with_a_late_audit_failure() {
    let (_home, db, alice, bob, admin) = primary_generation_fixture();
    let before = db.google_primary_owner_generation("bot").unwrap();
    let acl_before = db.acl_generation().unwrap();
    db.conn().execute_batch("CREATE TRIGGER test_primary_audit_abort BEFORE INSERT ON auth_audit_log WHEN NEW.action = 'agent.primary_owner_set' BEGIN SELECT RAISE(ABORT, 'synthetic audit failure'); END;").unwrap();
    assert!(db.set_google_primary_owner("bot", &bob.id, &admin.id).is_err());
    assert_eq!(db.google_primary_owner_generation("bot").unwrap(), before);
    assert_eq!(db.acl_generation().unwrap(), acl_before);
    assert_eq!(db.google_owner("bot").unwrap().id, alice.id);
}

#[test]
fn primary_generation_corrupt_values_refuse_without_mutation() {
    let (_home, db, alice, bob, admin) = primary_generation_fixture();
    for bad in [rusqlite::types::Value::Text("broken".into()), rusqlite::types::Value::Real(1.5), rusqlite::types::Value::Integer(-1)] {
        let key = primary_owner_generation_key("bot");
        db.conn().execute("INSERT INTO auth_meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, bad]).unwrap();
        let acl_before = db.acl_generation().unwrap();
        assert!(db.google_primary_owner_generation("bot").is_err());
        assert!(db.set_google_primary_owner("bot", &bob.id, &admin.id).is_err());
        assert_eq!(db.google_owner("bot").unwrap().id, alice.id);
        assert_eq!(db.acl_generation().unwrap(), acl_before);
        assert_eq!(db.conn().query_row("SELECT value FROM auth_meta WHERE key = ?1", [key], |r| r.get::<_, rusqlite::types::Value>(0)).unwrap(), bad);
    }
}

#[test]
fn primary_generation_overflow_refuses_without_mutation() {
    let (_home, db, alice, bob, admin) = primary_generation_fixture();
    db.conn().execute("INSERT INTO auth_meta(key,value) VALUES (?1,?2)", params![primary_owner_generation_key("bot"), i64::MAX]).unwrap();
    let acl_before = db.acl_generation().unwrap();
    assert_eq!(db.google_primary_owner_generation("bot").unwrap(), i64::MAX as u64);
    assert!(db.set_google_primary_owner("bot", &bob.id, &admin.id).is_err());
    assert_eq!(db.google_primary_owner_generation("bot").unwrap(), i64::MAX as u64);
    assert_eq!(db.acl_generation().unwrap(), acl_before);
    assert_eq!(db.google_owner("bot").unwrap().id, alice.id);
}
