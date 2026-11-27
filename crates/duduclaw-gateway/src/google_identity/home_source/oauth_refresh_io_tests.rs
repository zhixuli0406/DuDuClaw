//! C1 parser source declarations, all UNRUN. No live credential/provider use.
use super::*;
use serde_json::json;
fn good() -> Value {
    json!({"access_token":"owned-access","token_type":"Bearer","expires_in":3600,"scope":"scope-a"})
}
#[test]
fn omission_retains_captured_scopes_and_present_rotation_is_not_hidden() {
    let mut missing = good();
    missing.as_object_mut().unwrap().remove("scope");
    let parsed = parse_tokens(missing, &["stored-scope".into()], 1_800_000_000).unwrap();
    assert_eq!(parsed.scopes, vec!["stored-scope"]);
    assert_eq!(Error::ScopeUnverified.to_string(), "scope_unverified");
    let mut rotated = good();
    rotated["scope"] = json!("scope-b");
    rotated["refresh_token"] = json!("new-refresh");
    let parsed = parse_tokens(rotated, &["stored-scope".into()], 1_800_000_000).unwrap();
    assert_eq!(parsed.scopes, vec!["scope-b"]);
    assert_eq!(
        parsed.refresh.as_deref().map(String::as_str),
        Some("new-refresh")
    );
}
#[test]
fn invalid_extra_scopes_cannot_hide_behind_missing_legacy_eleven() {
    for scope in [
        json!("scope-a extra\\bad"),
        json!("scope-a extra\"bad"),
        json!("scope-a extra\n"),
        json!("scope-a 非ASCII"),
        json!("scope-a  extra"),
        json!(""),
        Value::Null,
        json!([]),
        json!((0..129).map(|_| "scope-a").collect::<Vec<_>>().join(" ")),
    ] {
        let mut body = good();
        body["scope"] = scope;
        assert!(parse_tokens(body, &["scope-a".into()], 1_800_000_000).is_err());
    }
}
#[test]
fn strict_present_refresh_type_bearer_expiry_and_per_token_bounds() {
    for (field, value) in [
        ("refresh_token", Value::Null),
        ("refresh_token", json!("")),
        ("refresh_token", json!(0)),
        ("refresh_token", json!("bad refresh")),
        ("access_token", json!("")),
        ("access_token", json!("bad access")),
        ("access_token", json!("enc:v1:fake")),
        ("access_token", json!("A".repeat(MAX_TOKEN + 1))),
        ("token_type", json!("Basic")),
        ("expires_in", json!(0)),
        ("expires_in", json!(-1)),
        ("expires_in", json!(u64::MAX)),
    ] {
        let mut body = good();
        body[field] = value;
        assert!(parse_tokens(body, &["scope-a".into()], 1_800_000_000).is_err(), "{field}");
    }
    assert!(parse_tokens(good(), &["scope-a".into()], u64::MAX).is_err());
}
#[test]
fn home_freshness_has_same_sixty_second_boundary_and_none_policy() {
    let now = 1_800_000_000;
    assert!(!home_store::home_oauth_expired(None, now));
    assert!(home_store::home_oauth_expired(
        chrono::DateTime::from_timestamp(now as i64 + 60, 0),
        now
    ));
    assert!(!home_store::home_oauth_expired(
        chrono::DateTime::from_timestamp(now as i64 + 61, 0),
        now
    ));
    assert!(!home_store::home_oauth_expired(
        chrono::DateTime::from_timestamp(now as i64 + 60, 1),
        now
    ));
}

#[test]
fn home_refresh_write_scope_isolated_but_readonly_is_accepted() {
    for (scope, isolated) in [
        (crate::google_personal::oauth::DRIVE_WRITE_SCOPE, true),
        (crate::google_personal::oauth::DRIVE_TOOLS_SCOPE, false),
    ] {
        let mut body = good();
        body["scope"] = json!(scope);
        let result = parse_tokens(body, &["scope-a".into()], 1_800_000_000);
        if isolated { assert!(matches!(result, Err(Error::WriteScopeIsolated))); }
        else { assert!(result.is_ok()); }
    }
}

#[tokio::test]
async fn home_refresh_without_scope_and_no_write_credential_retains_captured_scopes() {
    use crate::google_identity::write_scope_guard::testing::TokeninfoWire;
    use crate::google_personal::oauth::TEST_TOKENINFO_URL;
    let home = tempfile::tempdir().unwrap();
    assert!(!crate::person_credentials::write_store::ever_granted(home.path(), "home", "home").unwrap());
    let mut body = good();
    body.as_object_mut().unwrap().remove("scope");
    let mut parsed = parse_tokens(body, &["captured-scope".into()], 1_800_000_000).unwrap();
    let wire = TokeninfoWire::new("owned-access", "home-client", None).await;
    TEST_TOKENINFO_URL.scope(wire.url.clone(), verify_refresh_scopes(home.path(), "home-client", &mut parsed))
        .await.unwrap();
    assert_eq!(parsed.scopes, vec!["captured-scope"]);
    assert_eq!(wire.calls(), 0);
}

#[tokio::test]
async fn home_refresh_without_scope_after_write_disconnect_verifies_tokeninfo() {
    use crate::google_identity::write_scope_guard::testing::{install_write_credential, TokeninfoWire};
    use crate::google_personal::oauth;
    use crate::person_credentials::write_store;
    for scopes in [Some(oauth::DRIVE_WRITE_SCOPE), Some(oauth::DRIVE_TOOLS_SCOPE), None] {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        crate::handlers::credential_protection_rpc::protection_for(home.path()).enable(false).unwrap();
        let key = crate::google_identity::protection_gate(home.path()).unwrap();
        install_write_credential(home.path(), &key, "home", "home", "home-client");
        write_store::disconnect(home.path(), "home", "home").unwrap();
        assert!(write_store::get(home.path(), "home", "home").unwrap().is_none());
        let mut body = good();
        body.as_object_mut().unwrap().remove("scope");
        let mut parsed = parse_tokens(body, &["captured-scope".into()], 1_800_000_000).unwrap();
        let wire = TokeninfoWire::new("owned-access", "home-client", scopes).await;
        let result = oauth::TEST_TOKENINFO_URL.scope(wire.url.clone(),
            verify_refresh_scopes(home.path(), "home-client", &mut parsed)).await;
        match scopes {
            Some(oauth::DRIVE_WRITE_SCOPE) => assert!(matches!(result, Err(Error::WriteScopeIsolated))),
            Some(scope) => { result.unwrap(); assert_eq!(parsed.scopes, vec![scope]); }
            None => assert!(matches!(result, Err(Error::ScopeUnverified))),
        }
        assert_eq!(wire.calls(), 1);
    }
}

#[tokio::test]
async fn home_refresh_without_scope_with_write_credential_verifies_tokeninfo() {
    use crate::google_identity::write_scope_guard::testing::{install_write_credential, TokeninfoWire};
    use crate::google_personal::oauth;
    for scopes in [Some(oauth::DRIVE_WRITE_SCOPE), Some(oauth::DRIVE_TOOLS_SCOPE), None] {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        crate::handlers::credential_protection_rpc::protection_for(home.path()).enable(false).unwrap();
        let key = crate::google_identity::protection_gate(home.path()).unwrap();
        install_write_credential(home.path(), &key, "home", "home", "home-client");
        let before = std::fs::read(crate::person_credentials::write_store::path(home.path())).unwrap();
        let mut body = good();
        body.as_object_mut().unwrap().remove("scope");
        let mut parsed = parse_tokens(body, &["captured-scope".into()], 1_800_000_000).unwrap();
        let wire = TokeninfoWire::new("owned-access", "home-client", scopes).await;
        let result = oauth::TEST_TOKENINFO_URL.scope(wire.url.clone(),
            verify_refresh_scopes(home.path(), "home-client", &mut parsed)).await;
        match scopes {
            Some(oauth::DRIVE_WRITE_SCOPE) => assert!(matches!(result, Err(Error::WriteScopeIsolated))),
            Some(scope) => { result.unwrap(); assert_eq!(parsed.scopes, vec![scope]); }
            None => assert!(matches!(result, Err(Error::ScopeUnverified))),
        }
        assert_eq!(wire.calls(), 1);
        assert_eq!(std::fs::read(crate::person_credentials::write_store::path(home.path())).unwrap(), before);
    }
}

#[tokio::test]
async fn home_refresh_without_scope_and_unreadable_write_store_fails_closed() {
    use crate::google_identity::write_scope_guard::testing::TokeninfoWire;
    use crate::google_personal::oauth::TEST_TOKENINFO_URL;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("person_credentials")).unwrap();
    std::fs::write(crate::person_credentials::write_store::path(home.path()), "invalid-json").unwrap();
    let mut body = good();
    body.as_object_mut().unwrap().remove("scope");
    let mut parsed = parse_tokens(body, &["captured-scope".into()], 1_800_000_000).unwrap();
    let wire = TokeninfoWire::new("owned-access", "home-client", None).await;
    let result = TEST_TOKENINFO_URL.scope(wire.url.clone(),
        verify_refresh_scopes(home.path(), "home-client", &mut parsed)).await;
    assert!(matches!(result, Err(Error::ScopeUnverified)));
    assert_eq!(wire.calls(), 0);
}
