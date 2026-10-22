//! `security.credential_protection*` through `MethodHandler::handle`, over
//! the per-home in-memory store (`credential_protection_rpc::test_store`) —
//! no real keychain is touched.
use super::*;
use crate::handlers::credential_protection_rpc::test_store;

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => e.as_str().unwrap_or_default().to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn audit_rows(home: &Path, event: &str) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event_type"] == event)
        .collect()
}

fn employee() -> UserContext {
    UserContext {
        user_id: "emp-1".into(),
        email: "emp-1@example.invalid".into(),
        role: UserRole::Employee,
        agent_access: Default::default(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

fn key_hex(home: &Path) -> String {
    use duduclaw_security::credential_protection::{KeyStore, marker};
    let raw = test_store::for_home(home).read(&marker::account_for_home(home)).unwrap();
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

async fn call(h: &MethodHandler, method: &str, params: Value, who: &UserContext) -> WsFrame {
    Box::pin(h.handle(method, params, who)).await
}

#[tokio::test]
async fn non_admins_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let h = MethodHandler::new(home.clone()).await;
    for method in [
        "security.credential_protection",
        "security.credential_protection_enable",
        "security.credential_protection_disable",
    ] {
        let frame = call(&h, method, json!({ "confirm": true }), &employee()).await;
        assert!(!err(&frame).is_empty(), "{method} must refuse a non-admin");
    }
    assert!(!home.join("credential_protection.json").exists());
}

#[tokio::test]
async fn enable_status_disable_round_trip_without_leaking_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let h = MethodHandler::new(home.clone()).await;
    let admin = UserContext::admin_fallback();

    let before = ok(&call(&h, "security.credential_protection", json!({}), &admin).await);
    assert_eq!(before["state"], "not_enabled");
    assert_eq!(before["level"], "file");
    assert_eq!(before["summary"], "憑證保護：檔案");
    assert_eq!(before["person_identity_allowed"], false);

    let enabled = ok(&call(&h, "security.credential_protection_enable", json!({}), &admin).await);
    assert_eq!(enabled["state"], "ready");
    assert_eq!(enabled["level"], "app_bound");
    assert_eq!(enabled["person_identity_allowed"], true);
    let secret = key_hex(&home);
    assert!(!enabled.to_string().contains(&secret), "status must not carry the key");

    let rows = audit_rows(&home, "credential_protection_enabled");
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].to_string().contains(&secret), "audit must not carry the key");
    assert_eq!(rows[0]["details"]["key_id"], enabled["key_id"]);

    // A second plain enable changes nothing and writes no audit row.
    ok(&call(&h, "security.credential_protection_enable", json!({}), &admin).await);
    assert_eq!(audit_rows(&home, "credential_protection_enabled").len(), 1);

    let refused = call(&h, "security.credential_protection_disable", json!({}), &admin).await;
    assert!(err(&refused).contains("confirm"));
    assert_eq!(ok(&call(&h, "security.credential_protection", json!({}), &admin).await)["state"], "ready");

    let disabled = ok(&call(&h, "security.credential_protection_disable", json!({ "confirm": true }), &admin).await);
    assert_eq!(disabled["state"], "not_enabled");
    assert_eq!(audit_rows(&home, "credential_protection_disabled").len(), 1);
}

#[tokio::test]
async fn a_changed_key_needs_an_explicit_reset() {
    use duduclaw_security::credential_protection::marker;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let h = MethodHandler::new(home.clone()).await;
    let admin = UserContext::admin_fallback();
    let first = ok(&call(&h, "security.credential_protection_enable", json!({}), &admin).await);

    test_store::for_home(&home).tamper(&marker::account_for_home(&home), &[3u8; 32]);
    let status = ok(&call(&h, "security.credential_protection", json!({}), &admin).await);
    assert_eq!(status["state"], "key_changed");
    assert_eq!(status["person_identity_allowed"], false);
    // Seen once → one Critical audit row, not one per page load.
    ok(&call(&h, "security.credential_protection", json!({}), &admin).await);
    let broken = audit_rows(&home, "credential_protection_broken");
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0]["details"]["state"], "key_changed");

    let refused = call(&h, "security.credential_protection_enable", json!({}), &admin).await;
    assert!(err(&refused).contains("作廢"), "re-enable must explain what a reset voids");
    let reset = ok(&call(&h, "security.credential_protection_enable", json!({ "reset": true }), &admin).await);
    assert_eq!(reset["state"], "ready");
    assert_ne!(reset["key_id"], first["key_id"]);
    let rows = audit_rows(&home, "credential_protection_enabled");
    assert_eq!(rows.last().unwrap()["details"]["previous_state"], "key_changed");
    assert_eq!(audit_rows(&home, "credential_protection_enable_failed").len(), 1);
}
