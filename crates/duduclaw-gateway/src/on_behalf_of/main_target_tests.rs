use super::*;
use duduclaw_auth::UserDb;
use duduclaw_core::EditionProfile;

struct Fixture {
    dir: tempfile::TempDir,
    db: UserDb,
    admin: String,
    emp: String,
}

fn fixture(edition: Option<EditionProfile>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@example.invalid", "Admin", "isolated-test-password", UserRole::Admin)
        .unwrap().id;
    let emp = db.create_user("emp@example.invalid", "Employee", "isolated-test-password", UserRole::Employee)
        .unwrap().id;
    if let Some(edition) = edition { crate::team_edition::record(dir.path(), edition); }
    Fixture { dir, db, admin, emp }
}

const USES: [MainUse; 2] = [MainUse::Converse, MainUse::Assign];

#[test]
fn active_admin_and_exact_listed_employee_decisions() {
    let f = fixture(Some(EditionProfile::Enterprise));
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(true));
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), "missing", use_), Ok(false));
    }
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(true));
        assert_eq!(main_access_allowed(f.dir.path(), &format!("{}-extra", f.emp), use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &format!(" {} ", f.emp), use_), Ok(false));
    }
    f.db.set_main_agent_users(&f.admin, &[], true).unwrap();
    assert_eq!(main_access_allowed(f.dir.path(), &f.emp, MainUse::Converse), Ok(false));
}

#[test]
fn suspension_removes_list_entry_and_denies_even_admin() {
    let f = fixture(Some(EditionProfile::Enterprise));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let conn = rusqlite::Connection::open(f.dir.path().join("users.db")).unwrap();
    conn.execute("UPDATE users SET status='suspended' WHERE id=?1", [&f.emp]).unwrap();
    assert!(!f.db.is_main_agent_user(&f.emp).unwrap());
    assert!(f.db.main_agent_users().unwrap().is_empty());
    conn.execute("UPDATE users SET status='suspended' WHERE id=?1", [&f.admin]).unwrap();
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(false));
    }
}

#[test]
fn personal_downgrade_ignores_stale_rows_but_retains_admin() {
    let f = fixture(Some(EditionProfile::Enterprise));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    crate::team_edition::record(f.dir.path(), EditionProfile::Personal);
    assert!(f.db.is_main_agent_user(&f.emp).unwrap());
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(true));
    }
}

#[test]
fn unresolved_edition_ignores_list() {
    let f = fixture(None);
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(true));
    }
}

#[test]
fn missing_or_unreadable_identity_store_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(main_access_allowed(dir.path(), "admin", MainUse::Converse).is_err());
    let path = dir.path().join("users.db");
    std::fs::create_dir(&path).unwrap();
    for use_ in USES {
        assert!(main_access_allowed(dir.path(), "admin", use_).is_err());
        assert!(requires_main_restriction_for(dir.path(), Some("user:admin"), use_).is_err());
    }
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(path, b"not a SQLite database").unwrap();
    assert!(main_access_allowed(dir.path(), "admin", MainUse::Converse).is_err());
}

#[test]
fn principal_classification_and_legacy_admin_only_gate_are_preserved() {
    let f = fixture(Some(EditionProfile::Enterprise));
    let employee = format!("user:{}", f.emp);
    for use_ in USES {
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some(&employee), use_), Ok(true));
    }
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(requires_main_restriction_for(f.dir.path(), None, use_), Ok(false));
        assert_eq!(requires_main_restriction_for(f.dir.path(),
            Some("unmapped:line:0123456789abcdef"), use_), Ok(false));
        for unknown in ["", "unknown:rejected-principal", "user:"] {
            assert!(requires_main_restriction_for(f.dir.path(), Some(unknown), use_).is_err());
        }
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some(&employee), use_), Ok(false));
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some("user:missing"), use_), Ok(true));
        assert_eq!(requires_main_restriction_for(f.dir.path(),
            Some(&format!("  {employee}  ")), use_), Ok(false));
    }
    assert_eq!(requires_main_restriction(f.dir.path(), Some(&employee)), Ok(true));
    assert_eq!(requires_main_restriction(f.dir.path(), Some(&format!("user:{}", f.admin))), Ok(false));
}
