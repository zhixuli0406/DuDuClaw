use super::*;
use duduclaw_core::agent_seats::{seat_holders, SeatAdmission, SeatDecision, AVATAR_SEAT_REFUSAL};
use std::path::Path;

fn agent(home: &Path, name: &str) -> Result<(), String> {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("agent.toml"), format!("[agent]\nname='{name}'\nstatus='active'\n"))
        .map_err(|e| e.to_string())
}

fn fixture() -> (tempfile::TempDir, UserDb, User) {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.migration_invite_user("abc-1", &[1; 32], "avatar@example.test", "Avatar", UserRole::Admin).unwrap();
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    db.publish_new_avatar_under(&user.id, &user.id, "avatar", &admission, 1, |_| agent(home.path(), "avatar")).unwrap();
    assert!(seat_holders(home.path()).unwrap().is_empty());
    drop(admission);
    (home, db, user)
}

// Includes trigger effects, credential fencing, ACL generation and the receipt.
fn snapshot(db: &UserDb, user: &User) -> (String, i64, bool, i64, i64, i64) {
    db.conn().query_row("SELECT u.status,u.credential_epoch,r.disabled,
        (SELECT value FROM auth_meta WHERE key='acl_generation'),
        (SELECT COUNT(*) FROM admin_eligibility_log),
        (SELECT COUNT(*) FROM auth_audit_log)
        FROM users u JOIN migration_user_receipts r ON r.user_id=u.id WHERE u.id=?1",
        [&user.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).unwrap()
}

#[test]
fn migration_disable_full_seats_rolls_back_status_receipt_epoch_and_logs() {
    let (home, db, user) = fixture();
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    agent(home.path(), "ordinary").unwrap();
    let before = snapshot(&db, &user);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    assert_eq!(db.migration_user_disable_under("abc-1", &[1; 32], Some((&admission, 1))).unwrap_err(), AVATAR_SEAT_REFUSAL);
    assert_eq!(snapshot(&db, &user), before);
    assert_eq!(db.migration_user_readback("abc-1", &[1; 32]).unwrap().id, user.id);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    // A known larger cap permits the same transaction, including its receipt.
    db.migration_user_disable_under("abc-1", &[1; 32], Some((&admission, 2))).unwrap();
    assert_eq!(seat_holders(home.path()).unwrap().len(), 2);
    assert_eq!(db.get_user(&user.id).unwrap().unwrap().status, UserStatus::Suspended);
    assert!(snapshot(&db, &user).2);
    db.migration_invite_user_under("abc-1", &[1; 32], &user.email, &user.display_name, user.role,
        Some((&admission, 1))).unwrap();
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    assert!(!snapshot(&db, &user).2);
}

#[test]
fn migration_offline_limit_refuses_growth_but_allows_reactivation() {
    let (home, db, user) = fixture();
    let before = snapshot(&db, &user);
    assert_eq!(db.migration_user_disable("abc-1", &[1; 32]).unwrap_err(), AVATAR_SEAT_REFUSAL);
    assert_eq!(snapshot(&db, &user), before);
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    db.migration_user_disable_under("abc-1", &[1; 32], Some((&admission, 1))).unwrap();
    drop(admission);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    db.migration_invite_user("abc-1", &[1; 32], &user.email, &user.display_name, user.role).unwrap();
    assert!(seat_holders(home.path()).unwrap().is_empty());
    assert_eq!(db.get_user(&user.id).unwrap().unwrap().status, UserStatus::Active);
}

#[test]
fn migration_reactivate_and_concurrent_create_share_admission() {
    let (home, db, user) = fixture();
    // Open both pools before holding admission: initialization also takes it.
    let other = UserDb::new(&home.path().join("users.db")).unwrap();
    let held = SeatAdmission::acquire(home.path()).unwrap();
    db.migration_user_disable_under("abc-1", &[1; 32], Some((&held, 1))).unwrap();
    let barrier = std::sync::Barrier::new(3);
    let (finished, received) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let reactivate = scope.spawn(|| {
            barrier.wait();
            let result = other.migration_invite_user("abc-1", &[1; 32], &user.email, &user.display_name, user.role);
            finished.send(()).unwrap();
            result
        });
        let create = scope.spawn(|| {
            barrier.wait();
            let admission = SeatAdmission::acquire(home.path()).unwrap();
            match admission.decide(1, 1, None) {
                SeatDecision::Admitted => { agent(home.path(), "concurrent").unwrap(); true }
                SeatDecision::Full { .. } => false,
                other => panic!("unexpected admission: {other:?}"),
            }
        });
        barrier.wait();
        // Reactivation must wait even though it only releases a seat; a direct
        // users.db status write would finish while this admission is held.
        let blocked = matches!(received.recv_timeout(std::time::Duration::from_millis(200)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout));
        drop(held);
        assert!(blocked, "reactivation bypassed admission");
        assert_eq!(reactivate.join().unwrap().unwrap().id, user.id);
        let created = create.join().unwrap();
        assert_eq!(seat_holders(home.path()).unwrap().len(), usize::from(created));
    });
    assert_eq!(db.get_user(&user.id).unwrap().unwrap().status, UserStatus::Active);
    assert!(!snapshot(&db, &user).2);
}

#[test]
fn migration_binding_disable_rolls_back_exemption_and_receipt() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.migration_invite_user("abc-1", &[1; 32], "binding@example.test", "Binding", UserRole::Admin).unwrap();
    db.conn().execute("UPDATE users SET must_change_password=0,last_login='now' WHERE id=?1", [&user.id]).unwrap();
    agent(home.path(), "avatar").unwrap();
    db.migration_bind_shared("def-1", &[2; 32], &user.id, "avatar", crate::AccessLevel::Owner).unwrap();
    db.set_avatar_as(&user.id, &user.id, Some("avatar")).unwrap();
    // Model a previously migrated binding that later became a Personal avatar.
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    {
        let mut conn = db.conn();
        let tx = conn.transaction().unwrap();
        crate::db::insert_personal_agent_in_transaction(&tx, "avatar", &user.id).unwrap();
        crate::avatar_seats::grant_personal_incarnation(&tx, "avatar", &user.id).unwrap();
        tx.commit().unwrap();
    }
    agent(home.path(), "ordinary").unwrap();
    let generation = db.acl_generation().unwrap();
    assert_eq!(db.migration_binding_disable_under("def-1", &[2; 32], Some((&admission, 1))).unwrap_err(), AVATAR_SEAT_REFUSAL);
    assert_eq!(db.acl_generation().unwrap(), generation);
    assert_eq!(db.migration_binding_readback("def-1", &[2; 32]).unwrap().access_level, crate::AccessLevel::Owner);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    drop(admission);
    assert_eq!(db.migration_binding_disable("def-1", &[2; 32]).unwrap_err(), AVATAR_SEAT_REFUSAL);
    assert_eq!(db.acl_generation().unwrap(), generation);
}
