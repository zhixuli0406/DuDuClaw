use super::*;
use crate::{AccessLevel, UserRole};

fn fixture() -> (tempfile::TempDir, UserDb, String) {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.create_user("isolation@example.test", "Isolation", "fixture-password", UserRole::Employee).unwrap();
    db.conn().execute("UPDATE users SET last_login='2026-11-05T00:00:00Z',must_change_password=0,credential_epoch=4 WHERE id=?1", [&user.id]).unwrap();
    (home, db, user.id)
}

#[test]
fn isolation_authority_tenant_is_stable_and_read_only() {
    let (home, db, _) = fixture();
    let first = db.isolation_authority_snapshot(None, "sales").unwrap();
    assert!(first.human.is_none());
    let reopened = UserDb::new(&home.path().join("users.db")).unwrap();
    assert_eq!(first.tenant_id, reopened.isolation_authority_snapshot(None, "sales").unwrap().tenant_id);
    let mut readonly = Connection::open_with_flags(home.path().join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(first.tenant_id, read_isolation_authority(&mut readonly, None, "sales").unwrap().tenant_id);
    assert_eq!(db.list_users().unwrap().len(), 1, "background reads must not create users");
}

#[test]
fn isolation_authority_missing_or_corrupt_tenant_refuses_without_repair() {
    let (home, db, _) = fixture();
    db.conn().execute("DELETE FROM auth_meta WHERE key=?1", [TENANT_KEY]).unwrap();
    assert!(db.isolation_authority_snapshot(None, "sales").is_err());
    assert_eq!(db.conn().query_row("SELECT count(*) FROM auth_meta WHERE key=?1", [TENANT_KEY], |r| r.get::<_, i64>(0)).unwrap(), 0);
    for invalid in ["not-a-uuid", "00000000-0000-0000-0000-000000000000", "0F239D86-7CE8-4815-B564-B21200AEFA78", "7"] {
        db.conn().execute("INSERT OR REPLACE INTO auth_meta(key,value) VALUES(?1,?2)", params![TENANT_KEY, invalid]).unwrap();
        assert!(db.isolation_authority_snapshot(None, "sales").is_err());
        assert!(UserDb::new(&home.path().join("users.db")).is_err());
        let value: String = db.conn().query_row("SELECT CAST(value AS TEXT) FROM auth_meta WHERE key=?1", [TENANT_KEY], |r| r.get(0)).unwrap();
        assert_eq!(value, invalid);
    }
}

#[test]
fn isolation_authority_human_requires_active_login_and_operator_access() {
    let (_home, db, user) = fixture();
    assert!(db.isolation_authority_snapshot(Some(&user), "sales").is_err());
    assert!(db.isolation_authority_snapshot(Some("missing"), "sales").is_err());
    db.bind_agent(&user, "sales", AccessLevel::Viewer).unwrap();
    assert!(db.isolation_authority_snapshot(Some(&user), "sales").is_err());
    db.bind_agent(&user, "sales", AccessLevel::Operator).unwrap();
    let current = db.isolation_authority_snapshot(Some(&user), "sales").unwrap();
    assert_eq!(current.human.unwrap().credential_epoch, 4);
    for statement in ["UPDATE users SET status='suspended'", "UPDATE users SET status='offboarded'",
        "UPDATE users SET status='active',must_change_password=1", "UPDATE users SET must_change_password=0,last_login=NULL"] {
        db.conn().execute(statement, []).unwrap();
        assert!(db.isolation_authority_snapshot(Some(&user), "sales").is_err());
    }
}

#[test]
fn isolation_authority_binding_readd_never_revives_old_generation() {
    let (_home, db, user) = fixture();
    db.bind_agent(&user, "sales", AccessLevel::Owner).unwrap();
    let first = db.isolation_authority_snapshot(Some(&user), "sales").unwrap();
    db.unbind_agent(&user, "sales").unwrap();
    assert!(db.isolation_authority_snapshot(Some(&user), "sales").is_err());
    db.bind_agent(&user, "sales", AccessLevel::Owner).unwrap();
    let next = db.isolation_authority_snapshot(Some(&user), "sales").unwrap();
    assert!(next.acl_generation > first.acl_generation);
    assert_eq!(next.tenant_id, first.tenant_id);
    assert_eq!(next.human.unwrap().acl_generation, next.acl_generation);
}

#[test]
fn isolation_authority_reuses_manager_access_and_refuses_missing_generation() {
    let (_home, db, user) = fixture();
    let manager = db.create_user("manager@example.test", "Manager", "fixture-password", UserRole::Manager).unwrap();
    db.conn().execute("UPDATE users SET last_login='2026-11-05T00:00:00Z',must_change_password=0 WHERE id=?1", [&manager.id]).unwrap();
    db.set_manager(&user, Some(&manager.id)).unwrap();
    db.bind_agent(&user, "sales", AccessLevel::Owner).unwrap();
    let snapshot = db.isolation_authority_snapshot(Some(&manager.id), "sales").unwrap();
    assert!(snapshot.human.unwrap().can_assign_to("sales"));
    db.conn().execute("DELETE FROM auth_meta WHERE key='acl_generation'", []).unwrap();
    assert!(db.isolation_authority_snapshot(Some(&manager.id), "sales").is_err());
    assert!(db.isolation_authority_snapshot(None, "sales").is_err());
}

#[test]
fn isolation_authority_policy_epoch_tracks_fenced_aba_and_refuses_missing_or_corrupt() {
    let (home, db, _) = fixture();
    let before = db.isolation_authority_snapshot(None, "sales").unwrap();
    assert_eq!(before.policy_epoch, 1);
    for _ in 0..2 { duduclaw_core::isolation_policy::fence_change(home.path()).unwrap(); }
    let after = db.isolation_authority_snapshot(None, "sales").unwrap();
    assert_eq!(after.policy_epoch, 3);
    assert_eq!(after.acl_generation, before.acl_generation);
    assert_eq!(after.tenant_id, before.tenant_id);
    db.conn().execute("UPDATE auth_meta SET value='broken' WHERE key=?1",
        [duduclaw_core::isolation_policy::POLICY_EPOCH_KEY]).unwrap();
    assert!(db.isolation_authority_snapshot(None, "sales").is_err());
    assert!(UserDb::new(&home.path().join("users.db")).is_err());
    db.conn().execute("DELETE FROM auth_meta WHERE key=?1",
        [duduclaw_core::isolation_policy::POLICY_EPOCH_KEY]).unwrap();
    assert!(db.isolation_authority_snapshot(None, "sales").is_err());
    assert!(UserDb::new(&home.path().join("users.db")).is_err());
    assert_eq!(db.conn().query_row("SELECT count(*) FROM auth_meta WHERE key=?1",
        [duduclaw_core::isolation_policy::POLICY_EPOCH_KEY], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn isolation_authority_initialization_rejects_half_state_and_rolls_back_partial_insert() {
    let (home, db, _) = fixture();
    db.conn().execute("DELETE FROM auth_meta WHERE key=?1", [TENANT_KEY]).unwrap();
    assert!(UserDb::new(&home.path().join("users.db")).is_err());
    assert_eq!(db.conn().query_row("SELECT count(*) FROM auth_meta WHERE key=?1",
        [TENANT_KEY], |row| row.get::<_, i64>(0)).unwrap(), 0);
    db.conn().execute("DELETE FROM auth_meta WHERE key=?1",
        [duduclaw_core::isolation_policy::POLICY_EPOCH_KEY]).unwrap();
    db.conn().execute_batch("CREATE TRIGGER fail_isolation_epoch BEFORE INSERT ON auth_meta
        WHEN NEW.key='isolation_policy_epoch' BEGIN SELECT RAISE(ABORT,'injected epoch failure'); END;").unwrap();
    assert!(UserDb::new(&home.path().join("users.db")).is_err());
    assert_eq!(db.conn().query_row("SELECT count(*) FROM auth_meta WHERE key IN (?1,?2)",
        params![TENANT_KEY, duduclaw_core::isolation_policy::POLICY_EPOCH_KEY],
        |row| row.get::<_, i64>(0)).unwrap(), 0, "failed initialization must leave neither key");
    db.conn().execute_batch("DROP TRIGGER fail_isolation_epoch;").unwrap();
    let reopened = UserDb::new(&home.path().join("users.db")).unwrap();
    assert_eq!(reopened.isolation_authority_snapshot(None, "sales").unwrap().policy_epoch, 1);
}

#[test]
fn personal_agent_snapshot_reports_owner_and_member_count() {
    let (home, db, user) = fixture();
    let empty = db.isolation_authority_snapshot(None, "sales").unwrap();
    assert_eq!((empty.personal_owner, empty.agent_member_count), (None, 0));
    db.bind_agent(&user, "sales", AccessLevel::Owner).unwrap();
    let shared = db.isolation_authority_snapshot(Some(&user), "sales").unwrap();
    assert_eq!((shared.personal_owner, shared.agent_member_count), (None, 1));
    {
        let mut conn = db.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
        assert!(crate::db::insert_personal_agent_in_transaction(&tx, "sales", &user).unwrap());
        tx.commit().unwrap();
    }
    let personal = db.isolation_authority_snapshot(Some(&user), "sales").unwrap();
    assert_eq!(personal.personal_owner.as_deref(), Some(user.as_str()));
    assert_eq!(personal.agent_member_count, 1);
    assert!(personal.acl_generation > shared.acl_generation, "recording the owner fences older snapshots");
    let background = db.isolation_authority_snapshot(None, "sales").unwrap();
    assert_eq!((background.personal_owner.as_deref(), background.agent_member_count), (Some(user.as_str()), 1));
    let mut readonly = Connection::open_with_flags(home.path().join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(read_isolation_authority(&mut readonly, None, "sales").unwrap().personal_owner.as_deref(), Some(user.as_str()));

    // Other agents are unaffected and count every bound user at any level.
    let other = db.create_user("other@example.test", "Other", "fixture-password", UserRole::Employee).unwrap();
    db.bind_agent(&user, "team", AccessLevel::Owner).unwrap();
    db.bind_agent(&other.id, "team", AccessLevel::Viewer).unwrap();
    let team = db.isolation_authority_snapshot(None, "team").unwrap();
    assert_eq!((team.personal_owner, team.agent_member_count), (None, 2));
    assert!(db.bind_agent(&other.id, "sales", AccessLevel::Owner).is_err());
    assert_eq!(db.isolation_authority_snapshot(None, "sales").unwrap().agent_member_count, 1);

    // An unreadable mirror is unavailable, never "not Personal".
    db.conn().execute_batch("DROP TABLE isolation_personal_agents").unwrap();
    assert!(db.isolation_authority_snapshot(None, "sales").is_err());
    assert!(db.isolation_authority_snapshot(Some(&user), "team").is_err());
}

#[test]
fn isolation_authority_legacy_initialization_enables_standalone_policy_fence() {
    let (home, db, _) = fixture();
    db.conn().execute("DELETE FROM auth_meta WHERE key IN (?1,?2)",
        params![TENANT_KEY, duduclaw_core::isolation_policy::POLICY_EPOCH_KEY]).unwrap();
    assert!(duduclaw_core::isolation_policy::fence_change(home.path()).is_err());
    let initialized = UserDb::new(&home.path().join("users.db")).unwrap();
    let before = initialized.isolation_authority_snapshot(None, "sales").unwrap();
    {
        let org = duduclaw_core::org_store::lock_org_mutations(home.path()).unwrap();
        duduclaw_core::org_store::upsert(&org, "sales", duduclaw_core::OrgEntry::new("boss", "sales")).unwrap();
    }
    let after = initialized.isolation_authority_snapshot(None, "sales").unwrap();
    assert_eq!(after.tenant_id, before.tenant_id);
    assert_eq!(after.policy_epoch, before.policy_epoch + 1);
}
