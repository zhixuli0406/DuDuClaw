use super::*;
use crate::UserRole;

fn fixture() -> (tempfile::TempDir, UserDb, String, String) {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.create_user("member@test.invalid", "Member", "test-password", UserRole::Employee).unwrap();
    let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
    (home, db, user.id, admin.id)
}

#[test]
fn member_scopes_migration_is_atomic_idempotent_and_backfills_once() {
    let (home, db, user, _) = fixture();
    // Recreate the pre-1.86 key table, including its old ACL trigger.
    db.conn().execute_batch("DROP TABLE member_api_keys;
        DELETE FROM auth_meta WHERE key='member_keys_v186_backfill';
        CREATE TABLE member_api_keys(id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id),
            name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL, credential_epoch INTEGER NOT NULL, last_used_at TEXT, revoked_at TEXT);
        CREATE TRIGGER member_keys_acl_update AFTER UPDATE OF revoked_at, expires_at, credential_epoch ON member_api_keys
        BEGIN UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'; END;").unwrap();
    db.conn().execute("INSERT INTO member_api_keys(id,user_id,name,token_hash,created_at,expires_at,credential_epoch)
        VALUES('legacy',?1,'legacy','digest',strftime('%Y-%m-%dT%H:%M:%fZ','now'),
        strftime('%Y-%m-%dT%H:%M:%fZ','now','+1 day'),0)", [&user]).unwrap();
    migrate(&db.conn()).unwrap();
    migrate(&db.conn()).unwrap();
    let key = &db.member_api_keys_for_user(&user).unwrap()[0];
    assert!(key.include_conversations && key.conversations_migrated);
    assert!(key.mcp_scopes.is_empty());
    db.member_key_narrow_own(&user, "legacy", Some(false), None).unwrap();
    drop(db);
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let old = &db.member_api_keys_for_user(&user).unwrap()[0];
    assert!(!old.include_conversations);
    assert!(old.conversations_migrated);
    let new = db.member_key_create(&user, "new", 1, 0, false, &[]).unwrap();
    assert!(!new.info.include_conversations && !new.info.conversations_migrated);
}

#[test]
fn member_scopes_narrowing_is_live_audited_and_never_widens() {
    let (_home, db, user, admin) = fixture();
    for by_admin in [false, true] {
        let key = db.member_key_create(&user, "scoped", 1, 0, true,
            &["tasks:read".into(), "odoo:read".into(), "tasks:read".into()]).unwrap();
        assert_eq!(key.info.mcp_scopes, ["odoo:read", "tasks:read"]);
        let narrow = |flag, scopes: Option<&[String]>| if by_admin {
            db.member_key_narrow_admin(&admin, &key.info.id, flag, scopes)
        } else { db.member_key_narrow_own(&user, &key.info.id, flag, scopes) };
        let before = db.member_key_authority(&key.info.id, &user).unwrap();
        narrow(Some(false), None).unwrap();
        let middle = db.member_key_authority(&key.info.id, &user).unwrap();
        assert!(middle.context.acl_generation > before.context.acl_generation);
        narrow(None, Some(&["tasks:read".into()])).unwrap();
        let after = db.member_key_authority(&key.info.id, &user).unwrap();
        assert!(!after.include_conversations);
        assert_eq!(after.mcp_scopes, ["tasks:read"]);
        assert!(after.context.acl_generation > middle.context.acl_generation);
        assert_eq!(narrow(Some(true), None), Err(MemberKeyError::CannotWiden));
        assert_eq!(narrow(None, Some(&["odoo:read".into()])), Err(MemberKeyError::CannotWiden));
        assert_eq!(narrow(None, Some(&["admin".into()])), Err(MemberKeyError::InvalidScopes));
        db.member_key_revoke_own(&user, &key.info.id).unwrap();
        assert_eq!(narrow(Some(false), None), Err(MemberKeyError::NotFound));
    }
    let audits: i64 = db.conn().query_row("SELECT COUNT(*) FROM auth_audit_log WHERE action='member_key_narrowed'
        AND json_extract(detail,'$.before.include_conversations') IS NOT NULL
        AND json_extract(detail,'$.after.include_conversations')=0", [], |r| r.get(0)).unwrap();
    assert_eq!(audits, 4);
}

#[test]
fn member_scopes_unknown_storage_and_invalid_keys_fail_closed() {
    let (_home, db, user, admin) = fixture();
    assert!(matches!(db.member_key_create(&user, "bad", 1, 0, false, &["unknown".into()]), Err(MemberKeyError::InvalidScopes)));
    let key = db.member_key_create(&user, "key", 1, 0, false, &[]).unwrap();
    db.conn().execute("UPDATE member_api_keys SET mcp_scopes='tasks:read,unknown' WHERE id=?1", [&key.info.id]).unwrap();
    assert!(db.member_key_authority(&key.info.id, &user).unwrap().mcp_scopes.is_empty());
    assert!(db.member_api_keys_for_user(&user).unwrap()[0].mcp_scopes.is_empty());
    assert_eq!(db.member_key_narrow_own(&admin, &key.info.id, Some(false), None), Err(MemberKeyError::NotFound));
    assert_eq!(db.member_key_narrow_admin(&user, &key.info.id, Some(false), None), Err(MemberKeyError::AccountInactive));
    assert_eq!(db.member_key_narrow_own(&user, &key.info.id, None, Some(&["tasks:read".into()])), Err(MemberKeyError::CannotWiden));
    db.conn().execute("UPDATE member_api_keys SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1", [&key.info.id]).unwrap();
    assert_eq!(db.member_key_narrow_own(&user, &key.info.id, Some(false), None), Err(MemberKeyError::NotFound));
    assert_eq!(db.member_key_narrow_admin(&admin, &key.info.id, Some(false), None), Err(MemberKeyError::NotFound));
    assert!(db.member_key_authority(&key.info.id, &user).is_err());
}
