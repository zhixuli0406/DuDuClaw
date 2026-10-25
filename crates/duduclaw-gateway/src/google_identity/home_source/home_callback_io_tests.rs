//! B2 SOURCE FIRST. Pure parser declarations; NOT COMPILED OR RUN.
use super::*;
use serde_json::json;
fn requested() -> Vec<String> {
    crate::google_workspace::REQUIRED_SCOPES
        .iter()
        .map(|s| s.to_string())
        .collect()
}
fn good() -> Value {
    json!({"access_token":"owned.access_-token","token_type":"Bearer","expires_in":3600,"refresh_token":"owned-refresh"})
}
#[test]
fn response_scope_and_superset_are_actual_while_missing_uses_only_this_request() {
    let req = requested();
    let a = parse_tokens(good(), &req, 1_800_000_000).unwrap();
    assert_eq!(
        a.scopes,
        home_store::normalized_scopes(req.clone()).unwrap()
    );
    assert!(matches!(
        a.evidence,
        GoogleScopeEvidence::CapturedRequestOmission
    ));
    let mut v = good();
    let mut actual = req.clone();
    actual.push("extra:granted".into());
    v["scope"] = json!(actual.join(" "));
    let b = parse_tokens(v, &req, 1_800_000_000).unwrap();
    assert!(b.scopes.contains(&"extra:granted".into()));
    assert!(matches!(b.evidence, GoogleScopeEvidence::ResponseScope));
}
#[test]
fn present_null_wrong_empty_partial_and_invalid_scope_never_get_omission_fallback() {
    for scope in [
        Value::Null,
        json!([]),
        json!(42),
        json!(""),
        json!(" "),
        json!("scope-a"),
        json!("bad\nvalue"),
        json!("a\\b"),
        json!("a\"b"),
        json!("a  b"),
        json!(
            (0..129)
                .map(|n| format!("s{n}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ] {
        let mut v = good();
        v["scope"] = scope;
        assert!(parse_tokens(v, &requested(), 1_800_000_000).is_err());
    }
}
#[test]
fn strict_bearer_nonempty_bounded_access_and_positive_checked_expiry() {
    for (field, value) in [
        ("token_type", Value::Null),
        ("token_type", json!("Basic")),
        ("access_token", json!("")),
        ("access_token", json!("bad token")),
        ("access_token", json!("bad\r\ntoken")),
        ("expires_in", json!(0)),
        ("expires_in", json!(-1)),
        ("expires_in", json!("3600")),
        ("expires_in", json!(u64::MAX)),
        ("refresh_token", Value::Null),
    ] {
        let mut v = good();
        v[field] = value;
        assert!(
            parse_tokens(v, &requested(), 1_800_000_000).is_err(),
            "{field}"
        );
    }
    let mut v = good();
    v["token_type"] = json!("bEaReR");
    v.as_object_mut().unwrap().remove("refresh_token");
    assert!(
        parse_tokens(v, &requested(), 1_800_000_000)
            .unwrap()
            .refresh
            .is_none()
    );
}
#[test]
fn legacy_five_or_query_scope_is_not_captured_request_provenance() {
    let old_five: Vec<_> = requested().into_iter().take(5).collect();
    assert_eq!(old_five.len(), 5);
    assert!(parse_tokens(good(), &old_five, 1_800_000_000).is_err());
    let mut v = good();
    v["scope"] = json!("partial");
    v["query.scope"] = json!(requested().join(" "));
    assert!(parse_tokens(v, &requested(), 1_800_000_000).is_err());
}

#[test]
fn full_required_scopes_with_illegal_extra_or_129_raw_duplicates_still_refuse() {
    let req = requested();
    let valid = req.join(" ");
    for extra in [
        "bad\nvalue",
        "a\\b",
        "a\"b",
        "two  spaces",
        " ",
        &"x".repeat(2049),
    ] {
        let mut response = good();
        response["scope"] = json!(format!("{valid} {extra}"));
        assert!(parse_tokens(response, &req, 1_800_000_000).is_err());
    }
    // Coverage is complete and only the original-count bound can reject:
    // normalized deduplication would leave the same valid eleven scopes.
    let mut duplicates = req.clone();
    duplicates.extend(std::iter::repeat_n(req[0].clone(), 129 - req.len()));
    assert_eq!(duplicates.len(), 129);
    let mut response = good();
    response["scope"] = json!(duplicates.join(" "));
    assert!(parse_tokens(response, &req, 1_800_000_000).is_err());
    let mut positive = good();
    positive["scope"] = json!(valid);
    assert!(parse_tokens(positive, &req, 1_800_000_000).is_ok());
}
