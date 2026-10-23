use super::*;

fn person(db: &UserDb, email: &str) -> User {
    db.create_user(email, email, "fixture-password-only", UserRole::Employee)
        .unwrap()
}

#[test]
fn primary_owner_is_explicit_stable_and_only_an_owner() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
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
    db.set_google_primary_owner("bot", &bob.id, "admin")
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
        db.set_google_primary_owner("bot", &bob.id, "admin")
            .is_err()
    );
    assert_eq!(db.google_owner("bot").unwrap().id, alice.id);
}

#[test]
fn ambiguous_legacy_bindings_and_nonowners_refuse() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let alice = person(&db, "alice@c.test");
    let bob = person(&db, "bob@c.test");
    db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    db.conn()
        .execute("UPDATE user_agent_bindings SET is_primary = 0", [])
        .unwrap();
    assert_eq!(db.google_owner("bot").unwrap_err(), OwnerError::Ambiguous);
    db.set_google_primary_owner("bot", &alice.id, "admin")
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
