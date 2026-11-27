//! C1 parser source declarations, all UNRUN. No live credential/provider use.
use super::*;
use serde_json::json;
fn good() -> Value {
    json!({"access_token":"owned-access","token_type":"Bearer","expires_in":3600,"scope":"scope-a"})
}
#[test]
fn omission_refuses_and_present_rotation_is_not_hidden() {
    let mut missing = good();
    missing.as_object_mut().unwrap().remove("scope");
    assert!(matches!(parse_tokens(missing, 1_800_000_000), Err(Error::ScopeUnverified)));
    assert_eq!(Error::ScopeUnverified.to_string(), "scope_unverified");
    let mut rotated = good();
    rotated["scope"] = json!("scope-b");
    rotated["refresh_token"] = json!("new-refresh");
    let parsed = parse_tokens(rotated, 1_800_000_000).unwrap();
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
        assert!(parse_tokens(body, 1_800_000_000).is_err());
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
        assert!(parse_tokens(body, 1_800_000_000).is_err(), "{field}");
    }
    assert!(parse_tokens(good(), u64::MAX).is_err());
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
        let result = parse_tokens(body, 1_800_000_000);
        if isolated { assert!(matches!(result, Err(Error::WriteScopeIsolated))); }
        else { assert!(result.is_ok()); }
    }
}
