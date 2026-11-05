use super::*;

fn fixture() -> (tempfile::TempDir, Connection) {
    let home = tempfile::tempdir().unwrap();
    let db = Connection::open(home.path().join("users.db")).unwrap();
    db.execute_batch("CREATE TABLE auth_meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
        INSERT INTO auth_meta VALUES('isolation_policy_epoch',1);").unwrap();
    (home, db)
}
fn epoch(db: &Connection) -> i64 {
    db.query_row("SELECT value FROM auth_meta WHERE key=?1", [POLICY_EPOCH_KEY], |row| row.get(0)).unwrap()
}

#[test]
fn isolation_policy_absent_database_is_not_created() {
    let home = tempfile::tempdir().unwrap();
    fence_change(home.path()).unwrap();
    assert!(!home.path().join("users.db").exists());
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn isolation_policy_org_aba_advances_epoch_and_rejects_broken_epoch_before_write() {
    let (home, db) = fixture();
    let original = crate::org_store::OrgEntry::new("boss", "sales");
    crate::org_store::upsert(home.path(), "alice", original.clone()).unwrap();
    let first_epoch = epoch(&db);
    let original_bytes = std::fs::read(crate::org_store::org_store_path(home.path())).unwrap();
    crate::org_store::upsert(home.path(), "alice", crate::org_store::OrgEntry::new("boss", "support")).unwrap();
    crate::org_store::upsert(home.path(), "alice", original).unwrap();
    assert_eq!(std::fs::read(crate::org_store::org_store_path(home.path())).unwrap(), original_bytes);
    assert_eq!(epoch(&db), first_epoch + 2, "restoring bytes cannot restore run authority");
    db.execute("DELETE FROM auth_meta WHERE key=?1", [POLICY_EPOCH_KEY]).unwrap();
    assert!(crate::org_store::remove(home.path(), "alice").is_err());
    assert_eq!(std::fs::read(crate::org_store::org_store_path(home.path())).unwrap(), original_bytes);
}

#[test]
fn isolation_policy_corrupt_or_exhausted_epoch_is_never_repaired() {
    let (home, db) = fixture();
    for value in [rusqlite::types::Value::Integer(0), rusqlite::types::Value::Integer(-1),
        rusqlite::types::Value::Integer(i64::MAX), rusqlite::types::Value::Real(1.5),
        rusqlite::types::Value::Text("corrupt".into())] {
        db.execute("UPDATE auth_meta SET value=?1 WHERE key=?2", rusqlite::params![value, POLICY_EPOCH_KEY]).unwrap();
        assert!(fence_change(home.path()).is_err());
        let saved: rusqlite::types::Value = db.query_row("SELECT value FROM auth_meta WHERE key=?1", [POLICY_EPOCH_KEY], |row| row.get(0)).unwrap();
        assert_eq!(saved, value);
    }
    db.execute("DELETE FROM auth_meta WHERE key=?1", [POLICY_EPOCH_KEY]).unwrap();
    assert!(fence_change(home.path()).is_err());
    assert_eq!(db.query_row("SELECT count(*) FROM auth_meta", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn isolation_policy_existing_database_without_schema_is_not_initialized() {
    let home = tempfile::tempdir().unwrap();
    let db = Connection::open(home.path().join("users.db")).unwrap();
    assert!(fence_change(home.path()).is_err());
    assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn isolation_policy_readonly_and_dangling_symlink_databases_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (home, db) = fixture();
    let path = home.path().join("users.db");
    let permissions = std::fs::metadata(&path).unwrap().permissions();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(fence_change(home.path()).is_err());
    assert_eq!(epoch(&db), 1);
    std::fs::set_permissions(&path, permissions).unwrap();
    drop(db);
    std::fs::remove_file(&path).unwrap();
    symlink(home.path().join("missing.db"), &path).unwrap();
    assert!(fence_change(home.path()).is_err());
    assert!(!home.path().join("missing.db").exists());
}

#[test]
fn isolation_policy_preset_and_bootstrap_writers_fence_before_publication() {
    let (home, db) = fixture();
    crate::preset::install_system_operator_preset(home.path(), false).unwrap();
    assert_eq!(epoch(&db), 2);
    crate::preset::unbind(home.path(), "alice", "operator", "test").unwrap();
    assert_eq!(epoch(&db), 3);
    crate::config::write_minimal_config(home.path(), "127.0.0.1", 18889).unwrap();
    assert_eq!(epoch(&db), 4);
    db.execute("DELETE FROM auth_meta WHERE key=?1", [POLICY_EPOCH_KEY]).unwrap();
    let before = std::fs::read(home.path().join("config.toml")).unwrap();
    assert!(crate::config::write_minimal_config(home.path(), "127.0.0.1", 19000).is_err());
    assert_eq!(std::fs::read(home.path().join("config.toml")).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn isolation_policy_resolves_trusted_home_but_never_database_symlink() {
    use std::os::unix::fs::symlink;
    let (home, db) = fixture();
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("home");
    symlink(home.path(), &alias).unwrap();
    fence_change(&alias).unwrap();
    assert_eq!(epoch(&db), 2);
    let path = home.path().join("users.db");
    let saved = home.path().join("saved.db");
    std::fs::rename(&path, &saved).unwrap();
    symlink(&saved, &path).unwrap();
    assert!(fence_change(&alias).is_err());
    assert_eq!(epoch(&db), 2);
}
