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

#[tokio::test]
async fn raw_and_canonical_home_share_the_store_across_blocking_calls() {
    use crate::handlers::credential_protection_rpc::protection_for;
    use duduclaw_security::credential_protection::ProtectionState;
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().to_path_buf();
    let canonical = raw.canonicalize().unwrap();
    let store = test_store::for_home(&raw);
    let enabled = protection_for(&raw).enable(false).unwrap();
    duduclaw_core::gateway_instance::acquire(&canonical).unwrap();
    let sealed = protection_for(&raw)
        .person_identity_gate()
        .unwrap()
        .seal(b"owned-fixture")
        .unwrap();
    let (blocking_store, state, key_id, opened) = tokio::task::spawn_blocking(move || {
        let cp = protection_for(&canonical);
        let status = cp.status();
        let key = cp.person_identity_gate().unwrap();
        (
            test_store::for_home(&canonical),
            status.state,
            status.key_id,
            key.unseal(&sealed).unwrap(),
        )
    })
    .await
    .unwrap();
    assert!(Arc::ptr_eq(&store, &blocking_store));
    assert_eq!(state, ProtectionState::Ready);
    assert_eq!(key_id, enabled.key_id);
    assert_eq!(&opened[..], b"owned-fixture");
}

#[cfg(unix)]
#[test]
fn owned_symlink_home_shares_keys_and_preserves_missing_and_changed_refusals() {
    use crate::handlers::credential_protection_rpc::protection_for;
    use duduclaw_security::credential_protection::{ProtectionState, StoreError, marker};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let canonical = real.canonicalize().unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&canonical, &alias).unwrap();
    assert_ne!(
        alias, canonical,
        "the fixture must exercise distinct path spellings"
    );
    let aliased_store = test_store::for_home(&alias);
    let physical_store = test_store::for_home(&canonical);
    assert!(Arc::ptr_eq(&aliased_store, &physical_store));
    let enabled = protection_for(&alias).enable(false).unwrap();
    assert_eq!(enabled.state, ProtectionState::Ready);
    assert_eq!(protection_for(&canonical).status().key_id, enabled.key_id);
    assert_eq!(
        protection_for(&canonical)
            .person_identity_gate()
            .unwrap_err()
            .code,
        "not_gateway"
    );
    duduclaw_core::gateway_instance::acquire(&canonical).unwrap();
    let alias_key = protection_for(&alias).person_identity_gate().unwrap();
    let canonical_key = protection_for(&canonical).person_identity_gate().unwrap();
    assert_eq!(alias_key.key_id(), canonical_key.key_id());
    let sealed = alias_key.seal(b"owned-symlink-fixture").unwrap();
    assert_eq!(
        &canonical_key.unseal(&sealed).unwrap()[..],
        b"owned-symlink-fixture"
    );

    aliased_store.force_read_error(Some(StoreError::NotFound));
    assert_eq!(
        protection_for(&canonical).status().state,
        ProtectionState::Missing
    );
    assert_eq!(
        protection_for(&alias)
            .person_identity_gate()
            .unwrap_err()
            .code,
        "missing"
    );
    aliased_store.force_read_error(None);
    assert_eq!(
        protection_for(&alias).status().state,
        ProtectionState::Ready
    );
    physical_store.tamper(&marker::account_for_home(&canonical), &[0; 32]);
    assert_eq!(
        protection_for(&alias).status().state,
        ProtectionState::KeyChanged
    );
    assert_eq!(
        protection_for(&canonical)
            .person_identity_gate()
            .unwrap_err()
            .code,
        "key_changed"
    );
}

#[test]
fn different_physical_homes_keep_store_failures_isolated() {
    use crate::handlers::credential_protection_rpc::protection_for;
    use duduclaw_security::credential_protection::{ProtectionState, StoreError, marker};
    use std::sync::Arc;

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let store_a = test_store::for_home(a.path());
    let store_b = test_store::for_home(b.path());
    assert!(!Arc::ptr_eq(&store_a, &store_b));
    let cp_a = protection_for(a.path());
    let cp_b = protection_for(b.path());
    cp_a.enable(false).unwrap();
    let enabled_b = cp_b.enable(false).unwrap();
    duduclaw_core::gateway_instance::acquire(a.path()).unwrap();
    duduclaw_core::gateway_instance::acquire(b.path()).unwrap();
    let sealed_b = cp_b
        .person_identity_gate()
        .unwrap()
        .seal(b"isolated-fixture")
        .unwrap();

    store_a.force_read_error(Some(StoreError::NotFound));
    assert_eq!(cp_a.person_identity_gate().unwrap_err().code, "missing");
    assert_eq!(cp_b.status().state, ProtectionState::Ready);
    assert_eq!(cp_b.status().key_id, enabled_b.key_id);
    assert_eq!(
        &cp_b
            .person_identity_gate()
            .unwrap()
            .unseal(&sealed_b)
            .unwrap()[..],
        b"isolated-fixture"
    );
    store_a.force_read_error(None);
    store_a.tamper(&marker::account_for_home(a.path()), &[0; 32]);
    assert_eq!(cp_a.person_identity_gate().unwrap_err().code, "key_changed");
    assert_eq!(cp_b.status().state, ProtectionState::Ready);
    assert_eq!(
        &cp_b
            .person_identity_gate()
            .unwrap()
            .unseal(&sealed_b)
            .unwrap()[..],
        b"isolated-fixture"
    );
}

fn check_missing_home_identity_survives_enable(
    home: &Path,
    physical: &Path,
    missing_ancestor: &Path,
) {
    use crate::handlers::credential_protection_rpc::protection_for;
    use duduclaw_security::credential_protection::ProtectionState;
    use std::sync::Arc;

    let before = test_store::for_home(home);
    assert!(Arc::ptr_eq(&before, &test_store::for_home(physical)));
    let cp = protection_for(home);
    assert_eq!(cp.status().state, ProtectionState::NotEnabled);
    assert!(
        !home.exists(),
        "factory and status must not create a missing home"
    );
    assert!(!physical.exists());
    assert!(
        !missing_ancestor.exists(),
        "lookup must not create missing ancestors either"
    );
    let enabled = cp.enable(false).unwrap();
    assert_eq!(enabled.state, ProtectionState::Ready);
    let canonical = home.canonicalize().unwrap();
    assert_eq!(canonical, physical);
    assert!(Arc::ptr_eq(&before, &test_store::for_home(&canonical)));
    assert!(Arc::ptr_eq(&before, &test_store::for_home(home)));
    assert_eq!(
        protection_for(&canonical).status().state,
        ProtectionState::Ready
    );
    assert_eq!(protection_for(&canonical).status().key_id, enabled.key_id);
    duduclaw_core::gateway_instance::acquire(&canonical).unwrap();
    let sealed = cp
        .person_identity_gate()
        .unwrap()
        .seal(b"created-home-fixture")
        .unwrap();
    let key = protection_for(&canonical).person_identity_gate().unwrap();
    assert_eq!(&key.unseal(&sealed).unwrap()[..], b"created-home-fixture");
}

#[test]
fn missing_home_lookup_is_read_only_and_first_enable_keeps_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("missing-parent").join("home");
    let physical = dir
        .path()
        .canonicalize()
        .unwrap()
        .join("missing-parent")
        .join("home");
    check_missing_home_identity_survives_enable(&home, &physical, physical.parent().unwrap());
}

#[cfg(unix)]
#[test]
fn missing_home_below_symlink_ancestor_keeps_the_store_after_enable() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let canonical = real.canonicalize().unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&canonical, &alias).unwrap();
    let home = alias
        .join("missing-parent")
        .join("unused")
        .join("..")
        .join("home");
    let physical = canonical.join("missing-parent").join("home");
    check_missing_home_identity_survives_enable(&home, &physical, physical.parent().unwrap());
}

#[cfg(unix)]
#[test]
fn missing_parent_before_existing_symlink_keeps_the_store_after_enable() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let canonical = real.canonicalize().unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&canonical, &alias).unwrap();
    let missing_ancestor = dir.path().join("missing");
    let home = missing_ancestor.join("..").join("alias").join("new-home");
    let physical = canonical.join("new-home");
    check_missing_home_identity_survives_enable(&home, &physical, &missing_ancestor);
}
