//! Live-role and employee-binding regression coverage for the health snapshot.
use super::agent_files_rpc_tests::{fixture, ok, err};
use super::*;

#[tokio::test]
async fn employee_health_rpc_live_role_binding_and_window() {
    let f = fixture().await;
    // This fixture creates users.db but does not wire it into RPC dispatch.
    let db = Arc::new(duduclaw_auth::UserDb::new(&f.home.join("users.db")).unwrap());
    f.h.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"health-test-secret-at-least-32-bytes-long"))).await;
    assert!(!err(&f.call("agents.health", json!({}), &f.viewer).await).is_empty());
    let mut manager = f.operator.clone();
    manager.role = UserRole::Manager;
    // Use the production writer so cached live identities see a new ACL generation.
    db.update_user(&manager.user_id, None, Some(UserRole::Manager), None).unwrap();
    crate::employee_health::record_received(&f.home, "a1").await;
    crate::employee_health::record_turn(&f.home, "a1", Some("gate_refused")).await;
    crate::employee_health::record_received(&f.home, "b1").await;
    crate::employee_health::record_turn(&f.home, "b1", Some("timeout")).await;
    let response = ok(&f.call("agents.health", json!({ "window_days": 7 }), &manager).await);
    let agents = response["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["id"], "a1");
    assert_eq!(agents[0]["failures"]["gate_refused"], 1);
    assert_eq!(response["window_days"], 7);
    assert!(!err(&f.call("agents.health", json!({ "window_days": 8 }), &f.admin).await).is_empty());
    let all = ok(&f.call("agents.health", json!({}), &f.admin).await);
    assert_eq!(all["agents"].as_array().unwrap().len(), 2);
    db.update_user(&manager.user_id, None, Some(UserRole::Employee), None).unwrap();
    assert!(!err(&f.call("agents.health", json!({}), &manager).await).is_empty());
}
