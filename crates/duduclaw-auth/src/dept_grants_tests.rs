use super::*;
use crate::test_support::test_db;
use crate::user_row::read_acl_generation;

fn user(db: &UserDb, name: &str, role: UserRole) -> String {
    db.create_user(&format!("{name}@example.test"), name, "password-test", role).unwrap().id
}

#[test]
fn dept_grants_reach_gain_loss_same_and_net_groups() {
    let (db, _path) = test_db();
    let manager = user(&db, "王經理", UserRole::Manager);
    let groups = [GrantGroup::Schedules, GrantGroup::Skills];
    db.set_dept_grants("admin", &manager, "sales", &groups, true).unwrap();
    let gain = reach_change(&db, Some("mfg"), Some("sales")).unwrap();
    assert_eq!(gain, vec![ReachLine { user_id: manager.clone(), display_name: "王經理".into(),
        department: "sales".into(), groups: groups.to_vec(), change: ReachChange::Gains }]);
    let loss = reach_change(&db, Some("sales"), None).unwrap();
    assert_eq!(loss[0].change, ReachChange::Loses);
    assert_eq!(loss[0].groups, groups);
    assert!(reach_change(&db, Some("sales"), Some("sales")).unwrap().is_empty());
    assert!(reach_change(&db, Some("mfg"), Some("Sales")).unwrap().is_empty());
    assert!(reach_change(&db, None, Some("sales-extra")).unwrap().is_empty());
    db.set_dept_grants("admin", &manager, "mfg", &groups, true).unwrap();
    assert!(reach_change(&db, Some("mfg"), Some("sales")).unwrap().is_empty());
    db.set_dept_grants("admin", &manager, "mfg", &[GrantGroup::Skills, GrantGroup::Cost], true).unwrap();
    let net = reach_change(&db, Some("mfg"), Some("sales")).unwrap();
    assert_eq!(net.len(), 2);
    assert_eq!((&net[0].department, &net[0].groups, net[0].change),
        (&"mfg".to_owned(), &vec![GrantGroup::Cost], ReachChange::Loses));
    assert_eq!((&net[1].department, &net[1].groups, net[1].change),
        (&"sales".to_owned(), &vec![GrantGroup::Schedules], ReachChange::Gains));
    assert!(reach_change(&db, Some(""), None).unwrap().is_empty());
}

#[test]
fn dept_grants_reach_skips_inactive_and_non_managers_and_fails_on_read_error() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &GrantGroup::ALL, true).unwrap();
    // Reinsert stale grants after the cleanup trigger: readers must still
    // check live users even if legacy/corrupt data retains a grant row.
    db.conn().execute("UPDATE users SET status='suspended' WHERE id=?1", [&manager]).unwrap();
    let insert_stale = || {
        db.conn().execute("INSERT INTO department_manager_grants VALUES (?1, 'sales', 'skills', 'admin', 'now')",
            [&manager]).unwrap();
        assert_eq!(db.list_dept_grants().unwrap().len(), 1);
    };
    insert_stale();
    assert!(reach_change(&db, None, Some("sales")).unwrap().is_empty());
    db.conn().execute("UPDATE users SET status='active', role='employee' WHERE id=?1", [&manager]).unwrap();
    insert_stale();
    assert!(reach_change(&db, None, Some("sales")).unwrap().is_empty());
    db.conn().execute("DROP TABLE department_manager_grants", []).unwrap();
    assert!(reach_change(&db, None, Some("sales")).is_err());
}

#[test]
fn dept_grants_reach_digest_is_canonical_and_sentence_is_user_facing() {
    let mut lines = vec![
        ReachLine { user_id: "a".into(), display_name: "王經理".into(), department: "業務部".into(),
            groups: vec![GrantGroup::Schedules, GrantGroup::Skills], change: ReachChange::Gains },
        ReachLine { user_id: "b".into(), display_name: "李副理".into(), department: "製造部".into(),
            groups: vec![GrantGroup::Cost], change: ReachChange::Loses },
    ];
    assert_eq!(reach_sentence_zh(&lines).unwrap(),
        "核准後，王經理（業務部）將可管理這位 AI 員工的排程與技能；李副理（製造部）將不再能管理這位 AI 員工的用量。");
    let digest = reach_digest(&lines);
    assert_eq!(digest.len(), 64);
    lines[0].groups.reverse();
    lines.reverse();
    lines[0].display_name = "新名字".into();
    assert_eq!(reach_digest(&lines), digest);
    lines[0].change = ReachChange::Gains;
    assert_ne!(reach_digest(&lines), digest);
    assert_eq!(reach_digest(&[]), "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945");
    assert_eq!(reach_sentence_zh(&[]), None);
    lines[0].groups = GrantGroup::ALL.to_vec();
    let sentence = reach_sentence_zh(&lines).unwrap();
    assert!(sentence.contains("排程、自動化規則、技能、用量與對外傳送審核"));
    assert!(!sentence.contains("egress_review"));
}

#[test]
fn dept_grants_reach_supports_read_only_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("users.db");
    let db = UserDb::new(&path).unwrap();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Skills], true).unwrap();
    let reader = UserDb::open_read_only(&path).unwrap();
    assert_eq!(reach_change(&reader, None, Some("sales")).unwrap().len(), 1);
}

fn generation(db: &UserDb) -> u64 { read_acl_generation(&db.conn()).unwrap() }

fn audits(db: &UserDb, action: &str) -> Vec<(String, String, serde_json::Value)> {
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT user_id, target, detail FROM auth_audit_log WHERE action=?1 ORDER BY id").unwrap();
    stmt.query_map([action], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
    }).unwrap().map(|row| {
        let (actor, target, detail) = row.unwrap();
        (actor, target, serde_json::from_str(&detail).unwrap())
    }).collect()
}

#[test]
fn dept_grants_use_audit_preserves_acl_generation_and_grants() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Schedules], true).unwrap();
    let before = generation(&db);
    let grants = db.dept_grants_for_user(&manager).unwrap();
    db.record_dept_grant_use(&manager, "a1", GrantGroup::Schedules, "run_now", "cron.run_now").unwrap();
    assert_eq!(generation(&db), before);
    assert_eq!(db.dept_grants_for_user(&manager).unwrap(), grants);
    assert_eq!(audits(&db, "dept_grant_used"), vec![(manager, "a1".into(),
        serde_json::json!({"group":"schedules", "action":"run_now", "method":"cron.run_now"}))]);
}

#[test]
fn dept_grants_set_read_audit_noop_and_revoke() {
    let (db, _path) = test_db();
    let admin = user(&db, "admin", UserRole::Admin);
    let manager = user(&db, "manager", UserRole::Manager);
    let before = generation(&db);
    db.set_dept_grants(&admin, &manager, "業務部", &[GrantGroup::Skills, GrantGroup::Schedules], true).unwrap();
    assert_eq!(generation(&db), before + 1);
    assert_eq!(db.dept_grants_for_user(&manager).unwrap(),
        vec![("業務部".into(), vec![GrantGroup::Schedules, GrantGroup::Skills])]);
    let grants = db.list_dept_grants().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].user_id, manager);
    assert_eq!(grants[0].granted_by, admin);
    assert!(!grants[0].granted_at.is_empty());
    let audit = audits(&db, "dept_grant_changed");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0, admin);
    assert_eq!(audit[0].1, manager);
    assert_eq!(audit[0].2, serde_json::json!({"department":"業務部", "before":[], "after":["schedules","skills"], "acknowledged":true}));
    db.set_dept_grants(&admin, &manager, "業務部", &[GrantGroup::Schedules, GrantGroup::Skills, GrantGroup::Skills], true).unwrap();
    assert_eq!(generation(&db), before + 1);
    assert_eq!(audits(&db, "dept_grant_changed").len(), 1);
    db.set_dept_grants(&admin, &manager, "業務部", &[], true).unwrap();
    assert!(db.dept_grants_for_user(&manager).unwrap().is_empty());
    assert_eq!(generation(&db), before + 2);
    let audit = audits(&db, "dept_grant_changed");
    assert_eq!(audit[1].2["before"], serde_json::json!(["schedules", "skills"]));
    assert_eq!(audit[1].2["after"], serde_json::json!([]));
}

#[test]
fn dept_grants_refuse_invalid_targets_and_names_without_writes() {
    let (db, _path) = test_db();
    let admin = user(&db, "admin", UserRole::Admin);
    let employee = user(&db, "employee", UserRole::Employee);
    let suspended = user(&db, "suspended", UserRole::Manager);
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_user_status(&suspended, UserStatus::Suspended).unwrap();
    let before = generation(&db);
    for target in [&admin, &employee, &suspended, "missing"] {
        assert!(db.set_dept_grants(&admin, target, "sales", &GrantGroup::ALL, true).is_err());
    }
    for department in ["", "../sales", "sales/team", " sales", "sales\n"] {
        assert!(db.set_dept_grants(&admin, &manager, department, &GrantGroup::ALL, true).is_err());
    }
    assert_eq!(generation(&db), before);
    assert!(db.list_dept_grants().unwrap().is_empty());
    assert!(audits(&db, "dept_grant_changed").is_empty());
}

#[test]
fn dept_grants_unknown_group_mapping_and_check_constraint() {
    assert_eq!(GrantGroup::parse("bogus"), None);
    assert_eq!(GrantGroup::parse("Schedules"), None);
    for group in GrantGroup::ALL { assert_eq!(GrantGroup::parse(group.as_str()), Some(group)); }
    let (db, _path) = test_db();
    let conn = db.conn();
    // Exercise the exact reader mapper without weakening the table's CHECK.
    let unknown = conn.query_row("SELECT 'u', 'sales', 'bogus', 'admin', 'now'", [], grant_from_row).unwrap();
    assert!(unknown.is_none());
    let known = conn.query_row("SELECT 'u', 'sales', 'cost', 'admin', 'now'", [], grant_from_row).unwrap().unwrap();
    assert_eq!(known.groups, vec![GrantGroup::Cost]);
    assert!(conn.execute("INSERT INTO department_manager_grants VALUES ('u','sales','bogus','admin','now')", []).is_err());
}

#[test]
fn dept_grants_department_names_are_exact_and_not_user_profile_authority() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.conn().execute("UPDATE users SET department='sales' WHERE id=?1", [&manager]).unwrap();
    assert!(db.dept_grants_for_user(&manager).unwrap().is_empty());
    db.set_dept_grants("admin", &manager, "Sales", &[GrantGroup::Cost], true).unwrap();
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Skills], true).unwrap();
    db.set_dept_grants("admin", &manager, "sales", &[], true).unwrap();
    assert_eq!(db.dept_grants_for_user(&manager).unwrap(), vec![("Sales".into(), vec![GrantGroup::Cost])]);
}

#[test]
fn dept_grants_role_change_revokes_only_department_grants() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &GrantGroup::ALL, true).unwrap();
    db.set_main_agent_users("admin", &[manager.clone()], true).unwrap();
    let before = generation(&db);
    db.update_user(&manager, None, Some(UserRole::Employee), None).unwrap();
    assert_eq!(generation(&db), before + 1);
    assert!(db.list_dept_grants().unwrap().is_empty());
    assert!(db.is_main_agent_user(&manager).unwrap());
    let audit = audits(&db, "dept_grant_revoked");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0, "system");
    assert_eq!(audit[0].1, manager);
    assert_eq!(audit[0].2["actor_unknown"], true);
    assert_eq!(audit[0].2["new_role"], "employee");
}

#[test]
fn dept_grants_raw_role_update_and_rollback_are_covered() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &GrantGroup::ALL, true).unwrap();
    {
        let mut conn = db.conn();
        let tx = conn.transaction().unwrap();
        tx.execute("UPDATE users SET role='admin' WHERE id=?1", [&manager]).unwrap();
        let count: i64 = tx.query_row("SELECT count(*) FROM department_manager_grants", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
        tx.rollback().unwrap();
    }
    assert_eq!(db.list_dept_grants().unwrap().len(), 1);
    assert!(audits(&db, "dept_grant_revoked").is_empty());
    db.conn().execute("UPDATE users SET role='admin' WHERE id=?1", [&manager]).unwrap();
    assert!(db.list_dept_grants().unwrap().is_empty());
    assert_eq!(audits(&db, "dept_grant_revoked").len(), 1);
}

#[test]
fn dept_grants_deactivation_and_delete_clean_both_tables() {
    for action in ["suspended", "offboarded", "delete"] {
        let (db, _path) = test_db();
        let manager = user(&db, "manager", UserRole::Manager);
        db.set_dept_grants("admin", &manager, "sales", &GrantGroup::ALL, true).unwrap();
        db.set_main_agent_users("admin", &[manager.clone()], true).unwrap();
        match action {
            "suspended" => db.set_user_status(&manager, UserStatus::Suspended).unwrap(),
            "offboarded" => { db.offboard_user(&manager, None, Some("admin")).unwrap(); },
            _ => {
                db.conn().execute("DELETE FROM users WHERE id=?1", [&manager]).unwrap();
            }
        };
        assert!(db.list_dept_grants().unwrap().is_empty());
        assert!(db.main_agent_users().unwrap().is_empty());
        assert!(!db.is_main_agent_user(&manager).unwrap());
        assert_eq!(audits(&db, "dept_grant_revoked").len(), 1);
    }
}

#[test]
fn dept_grants_main_users_replace_validate_and_audit() {
    let (db, _path) = test_db();
    let admin = user(&db, "admin", UserRole::Admin);
    let employee = user(&db, "employee", UserRole::Employee);
    let inactive = user(&db, "inactive", UserRole::Employee);
    db.set_user_status(&inactive, UserStatus::Suspended).unwrap();
    let before = generation(&db);
    db.set_main_agent_users(&admin, &[admin.clone(), employee.clone(), employee.clone()], true).unwrap();
    assert_eq!(generation(&db), before + 1);
    assert_eq!(db.main_agent_users().unwrap().len(), 2);
    assert!(db.is_main_agent_user(&admin).unwrap());
    db.set_main_agent_users(&admin, &[employee.clone(), admin.clone()], true).unwrap();
    assert_eq!(generation(&db), before + 1);
    for invalid in [&inactive, "missing"] {
        assert!(db.set_main_agent_users(&admin, &[employee.clone(), invalid.to_string()], true).is_err());
        assert_eq!(db.main_agent_users().unwrap().len(), 2);
        assert_eq!(generation(&db), before + 1);
    }
    db.set_main_agent_users(&admin, &[employee.clone()], true).unwrap();
    assert_eq!(db.main_agent_users().unwrap(), vec![employee.clone()]);
    assert!(!db.is_main_agent_user(&admin).unwrap());
    let audit = audits(&db, "main_access_changed");
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[0].2["before"], serde_json::json!([]));
    assert_eq!(audit[1].2["after"], serde_json::json!([employee]));
    db.set_main_agent_users(&admin, &[], true).unwrap();
    assert!(db.main_agent_users().unwrap().is_empty());
}

#[test]
fn dept_grants_audit_failure_rolls_back_grants_and_generation() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Cost], true).unwrap();
    let before = generation(&db);
    db.conn().execute_batch("CREATE TRIGGER reject_grant_audit BEFORE INSERT ON auth_audit_log
        BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;").unwrap();
    assert!(db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Skills], true).is_err());
    assert!(db.set_main_agent_users("admin", &[manager.clone()], true).is_err());
    assert!(db.update_user(&manager, None, Some(UserRole::Employee), None).is_err());
    assert_eq!(generation(&db), before);
    assert_eq!(db.dept_grants_for_user(&manager).unwrap(), vec![("sales".into(), vec![GrantGroup::Cost])]);
    assert!(db.main_agent_users().unwrap().is_empty());
    assert_eq!(db.get_user(&manager).unwrap().unwrap().role, UserRole::Manager);
}

#[test]
fn dept_grants_migration_is_idempotent_and_read_errors_fail_closed() {
    let (db, path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Cost], true).unwrap();
    let reopened = UserDb::new(path.path()).unwrap();
    assert_eq!(reopened.list_dept_grants().unwrap().len(), 1);
    db.conn().execute_batch("DROP TABLE department_manager_grants; DROP TABLE main_agent_users;").unwrap();
    assert!(db.dept_grants_for_user(&manager).is_err());
    assert!(db.list_dept_grants().is_err());
    assert!(db.main_agent_users().is_err());
    assert!(db.is_main_agent_user(&manager).is_err());
}

#[test]
fn dept_grants_widening_requires_acknowledgement_and_revoke_does_not() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    let before = generation(&db);
    assert_eq!(db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Schedules], false),
        Err(ACKNOWLEDGEMENT_REQUIRED.into()));
    assert!(db.dept_grants_for_user(&manager).unwrap().is_empty());
    assert!(audits(&db, "dept_grant_changed").is_empty());
    assert_eq!(generation(&db), before);
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Schedules], true).unwrap();
    assert_eq!(audits(&db, "dept_grant_changed")[0].2["acknowledged"], true);
    // Replacing one group by another is widening even when cardinality stays equal.
    assert_eq!(db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Cost], false),
        Err(ACKNOWLEDGEMENT_REQUIRED.into()));
    assert_eq!(db.dept_grants_for_user(&manager).unwrap()[0].1, vec![GrantGroup::Schedules]);
    assert_eq!(generation(&db), before + 1);
    assert_eq!(audits(&db, "dept_grant_changed").len(), 1);
    db.set_dept_grants("admin", &manager, "sales", &GrantGroup::ALL, true).unwrap();
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Schedules], false).unwrap();
    assert_eq!(audits(&db, "dept_grant_changed")[2].2["acknowledged"], false);
    db.set_dept_grants("admin", &manager, "sales", &[], false).unwrap();
    assert!(db.dept_grants_for_user(&manager).unwrap().is_empty());
    assert_eq!(audits(&db, "dept_grant_changed")[3].2["acknowledged"], false);
}

#[test]
fn dept_grants_main_widening_requires_acknowledgement_and_removal_does_not() {
    let (db, _path) = test_db();
    let employee = user(&db, "employee", UserRole::Employee);
    let other = user(&db, "other", UserRole::Employee);
    let before = generation(&db);
    assert_eq!(db.set_main_agent_users("admin", &[employee.clone()], false),
        Err(ACKNOWLEDGEMENT_REQUIRED.into()));
    assert!(db.main_agent_users().unwrap().is_empty());
    assert!(audits(&db, "main_access_changed").is_empty());
    assert_eq!(generation(&db), before);
    db.set_main_agent_users("admin", &[employee.clone()], true).unwrap();
    assert_eq!(audits(&db, "main_access_changed")[0].2["acknowledged"], true);
    assert_eq!(db.set_main_agent_users("admin", &[other.clone()], false),
        Err(ACKNOWLEDGEMENT_REQUIRED.into()));
    assert_eq!(db.main_agent_users().unwrap(), vec![employee.clone()]);
    assert_eq!(generation(&db), before + 1);
    assert_eq!(audits(&db, "main_access_changed").len(), 1);
    db.set_main_agent_users("admin", &[employee.clone(), other], true).unwrap();
    db.set_main_agent_users("admin", &[employee], false).unwrap();
    assert_eq!(audits(&db, "main_access_changed")[2].2["acknowledged"], false);
    db.set_main_agent_users("admin", &[], false).unwrap();
    assert!(db.main_agent_users().unwrap().is_empty());
    assert_eq!(audits(&db, "main_access_changed")[3].2["acknowledged"], false);
}

#[test]
fn dept_grants_display_creator_snapshot_reads_fresh_facts_and_reports_errors() {
    let (db, _path) = test_db();
    let manager = user(&db, "manager", UserRole::Manager);
    db.set_dept_grants("admin", &manager, "sales", &[GrantGroup::Schedules], true).unwrap();
    let first = db.display_creator_access(&manager).unwrap().unwrap();
    assert_eq!(first.grants, vec![("sales".into(), vec![GrantGroup::Schedules])]);
    assert!(first.bindings.is_empty());
    assert!(db.display_creator_access("missing").unwrap().is_none());
    db.set_dept_grants("admin", &manager, "sales", &[], false).unwrap();
    assert!(db.display_creator_access(&manager).unwrap().unwrap().grants.is_empty());
    db.conn().execute("DROP TABLE department_manager_grants", []).unwrap();
    assert!(db.display_creator_access(&manager).is_err());
}

#[test]
fn main_access_display_batch_resolves_names_preserves_time_and_fails_on_read_error() {
    let (db, _path) = test_db();
    let granter = user(&db, "王管理員", UserRole::Admin);
    let member = user(&db, "李成員", UserRole::Employee);
    db.set_main_agent_users(&granter, &[member.clone()], true).unwrap();
    db.conn().execute("UPDATE main_agent_users SET granted_at='stored-time'", []).unwrap();
    let rows = db.main_agent_users_display().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].user.id, member);
    assert_eq!(rows[0].granted_by_name.as_deref(), Some("王管理員"));
    assert_eq!(rows[0].granted_at, "stored-time");
    db.conn().execute("DELETE FROM users WHERE id=?1", [&granter]).unwrap();
    let rows = db.main_agent_users_display().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].granted_by_name, None);
    assert_eq!(rows[0].granted_at, "stored-time");
    db.conn().execute("ALTER TABLE main_agent_users RENAME COLUMN granted_at TO broken", []).unwrap();
    assert!(db.main_agent_users_display().is_err());
}


#[test]
fn main_access_hash_checked_in_write_transaction_and_covers_grant_evidence() {
    let (db, path) = test_db();
    let member = user(&db, "member", UserRole::Employee);
    let initial = db.main_agent_users_display_with_hash().unwrap().1;
    assert_eq!(initial.len(), 64);
    db.set_main_agent_users_if_unchanged("admin", &[member.clone()], true, &initial).unwrap();
    let hash = db.main_agent_users_display_with_hash().unwrap().1;
    assert_ne!(initial, hash);
    let other = UserDb::new(path.path()).unwrap();
    for column in ["granted_by", "granted_at"] {
        let stale = db.main_agent_users_display_with_hash().unwrap().1;
        other.conn().execute(&format!("UPDATE main_agent_users SET {column}='changed'"), []).unwrap();
        let current = db.main_agent_users_display_with_hash().unwrap().1;
        let generation_before = generation(&db);
        let audit_count = audits(&db, "main_access_changed").len();
        assert_eq!(db.set_main_agent_users_if_unchanged("admin", &[], false, &stale),
            Err(MAIN_AGENT_USERS_HASH_CONFLICT.into()));
        // Even an otherwise unchanged list must compare the stored evidence.
        assert_eq!(db.set_main_agent_users_if_unchanged("admin", &[member.clone()], true, &stale),
            Err(MAIN_AGENT_USERS_HASH_CONFLICT.into()));
        assert_eq!(db.main_agent_users_display_with_hash().unwrap().1, current);
        assert_eq!(db.main_agent_users().unwrap(), vec![member.clone()]);
        assert_eq!(generation(&db), generation_before);
        assert_eq!(audits(&db, "main_access_changed").len(), audit_count);
    }
    let fresh = db.main_agent_users_display_with_hash().unwrap().1;
    db.set_main_agent_users_if_unchanged("admin", &[], false, &fresh).unwrap();
    assert_eq!(db.main_agent_users_display_with_hash().unwrap().1, initial);
    db.conn().execute("ALTER TABLE main_agent_users RENAME COLUMN granted_by TO broken", []).unwrap();
    assert!(db.set_main_agent_users_if_unchanged("admin", &[], false, &initial).is_err());
}

#[test]
fn main_access_hash_is_stable_across_row_order_and_reopen() {
    let (db, path) = test_db();
    let a = user(&db, "a", UserRole::Employee);
    let b = user(&db, "b", UserRole::Employee);
    db.set_main_agent_users("admin", &[a, b], true).unwrap();
    let before = db.main_agent_users_display_with_hash().unwrap().1;
    db.conn().execute_batch("CREATE TEMP TABLE saved AS SELECT * FROM main_agent_users;
        DELETE FROM main_agent_users;
        INSERT INTO main_agent_users SELECT * FROM saved ORDER BY user_id DESC;
        DROP TABLE saved;").unwrap();
    assert_eq!(db.main_agent_users_display_with_hash().unwrap().1, before);
    assert_eq!(UserDb::new(path.path()).unwrap().main_agent_users_display_with_hash().unwrap().1, before);
}
