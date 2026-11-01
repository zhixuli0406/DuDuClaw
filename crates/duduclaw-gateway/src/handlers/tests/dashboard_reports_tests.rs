use super::*;
use super::agent_files_rpc_tests::{fixture, ok, err};

#[tokio::test]
async fn dashboard_picker_and_view_follow_manager_links_and_target_layout() {
    let f = fixture().await;
    f.h.set_user_db(
        Arc::new(UserDb::new(&f.home.join("users.db")).unwrap()),
        Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!")),
    ).await;
    f.h.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    conn.execute("UPDATE users SET role='manager' WHERE id=?1", [&f.operator.user_id]).unwrap();
    conn.execute("UPDATE users SET manager_user_id=?1 WHERE id=?2", params![f.operator.user_id, f.owner_a.user_id]).unwrap();
    let mut manager = f.operator.clone();
    manager.role = UserRole::Manager;
    let users = ok(&f.call("users.subordinates", json!({}), &manager).await);
    assert_eq!(users["users"].as_array().unwrap().len(), 1);
    assert_eq!(users["users"][0]["id"], f.owner_a.user_id);
    assert!(!err(&f.call("dashboard.layout.view", json!({"user_id":f.owner_b.user_id}), &manager).await).is_empty());
    ok(&f.call("dashboard.layout.set", json!({"widgets":[{"id":"recent_activity"}]}), &manager).await);
    let missing = ok(&f.call("dashboard.layout.view", json!({"user_id":f.owner_a.user_id}), &manager).await);
    assert!(missing["layout"].is_null(), "never fall back to viewer's layout");
    ok(&f.call("dashboard.layout.set", json!({"widgets":[{"id":"my_tasks"}]}), &f.owner_a).await);
    let view = ok(&f.call("dashboard.layout.view", json!({"user_id":f.owner_a.user_id}), &manager).await);
    assert_eq!(view["layout"]["widgets"][0]["id"], "my_tasks");
    let admin_users = ok(&f.call("users.subordinates", json!({}), &f.admin).await);
    assert_eq!(admin_users["users"].as_array().unwrap().len(), 4);
    conn.execute("UPDATE users SET manager_user_id=NULL WHERE id=?1", [&f.owner_a.user_id]).unwrap();
    assert!(ok(&f.call("users.subordinates", json!({}), &manager).await)["users"].as_array().unwrap().is_empty());
    assert!(!err(&f.call("dashboard.layout.view", json!({"user_id":f.owner_a.user_id}), &manager).await).is_empty());
}
