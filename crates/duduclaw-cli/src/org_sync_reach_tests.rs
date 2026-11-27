use super::org_sync_reach_line;
use duduclaw_auth::{UserDb, UserRole};
use duduclaw_auth::dept_grants::GrantGroup;
use duduclaw_core::org_store::OrgEntry;

#[test]
fn org_sync_reach_prints_impact_or_warning_without_creating_store() {
    let home = tempfile::tempdir().unwrap();
    let before = OrgEntry::new("", "mfg");
    let after = OrgEntry::new("", "sales");
    let line = org_sync_reach_line(home.path(), Some(&before), &after).unwrap();
    assert!(line.contains("警告：授權影響無法計算"));
    assert!(!home.path().join("users.db").exists());
    assert!(org_sync_reach_line(home.path(), Some(&before), &before).is_none());

    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let manager = db.create_user("manager@example.test", "王經理", "test-password", UserRole::Manager).unwrap();
    db.set_dept_grants("admin", &manager.id, "sales", &[GrantGroup::Schedules, GrantGroup::Skills], true).unwrap();
    assert_eq!(org_sync_reach_line(home.path(), Some(&before), &after).unwrap(),
        "核准後，王經理（sales）將可管理這位 AI 員工的排程與技能。");
    assert!(org_sync_reach_line(home.path(), Some(&after), &before).unwrap().contains("將不再能管理"));
    assert!(org_sync_reach_line(home.path(), None, &before).is_none());

    drop(db);
    std::fs::write(home.path().join("users.db"), "unreadable database").unwrap();
    assert!(org_sync_reach_line(home.path(), Some(&before), &after).unwrap().contains("授權影響無法計算"));
}
