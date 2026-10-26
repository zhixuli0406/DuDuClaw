//! Owned test credentials enter through the actual Home authorization flow;
//! lower-level fixtures may synthesize only its non-secret control metadata.
//! No production writer, ENV endpoint, or real provider is used.
use super::*;

pub(crate) fn current_control(actor: &str) -> serde_json::Value {
    let consent = crate::google_data_consent::Evidence::issue(actor).unwrap();
    serde_json::json!({
        "schema": 1,
        "source_generation": uuid::Uuid::new_v4().to_string(),
        "state": "ready",
        "credential": {
            "credential_id": uuid::Uuid::new_v4().to_string(),
            "logical_epoch": 1,
            "token_revision": 1,
            "google_data_consent": consent,
        },
        "active_connect": null,
        "pending_mutation": null,
    })
}

pub(crate) async fn connect(home: &Path, access: &str, refresh: Option<&str>) {
    use crate::mcp_oauth::home_lifecycle as flow;
    let input = flow::GoogleHomeStartInput::new(
        Some("client-fixture"),
        Some("secret-fixture"),
        "test-admin",
    )
    .unwrap();
    let plan = flow::capture_google_home_start(home, input).unwrap();
    let (_, state, pending) = flow::begin_google_home_authorization(home, plan)
        .await
        .unwrap()
        .into_response_parts();
    let mut body = serde_json::json!({
        "access_token":access,"token_type":"Bearer","expires_in":3600,
        "scope":crate::google_workspace::REQUIRED_SCOPES.join(" ")
    });
    if let Some(refresh) = refresh {
        body["refresh_token"] = serde_json::json!(refresh);
    }
    let wire = crate::google_identity::home_source::callback_testing::Wire::json(home, Some(body));
    flow::complete_google_home_authorization(
        home,
        pending,
        &state,
        zeroize::Zeroizing::new("owned-fixture-code".into()),
    )
    .await
    .unwrap();
    assert_eq!(wire.requests(), 1);
    let stored = get_stored_token(home, "google").unwrap();
    assert_eq!(stored.access_token, access);
    assert_eq!(stored.refresh_token.as_deref(), refresh);
}
