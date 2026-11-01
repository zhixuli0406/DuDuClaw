//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// Display contract for saved integration credentials (WP13).
///
/// A customer whose Google integration was working — "測試連線" green, all 19
/// tools reachable — reported that their saved credentials had disappeared,
/// because the settings form showed nothing but placeholders. Nothing had been
/// lost: the read path reported connection state from the access token's own
/// expiry (a Google access token dies after an hour) and never sent the stored
/// client id back at all. These tests pin the corrected contract: what is saved
/// is visible, and the secret still never leaves the gateway.
use super::*;
use crate::mcp_oauth::{McpOAuthClientConfig, McpOAuthConfig, McpOAuthToken};

fn google_template() -> McpOAuthConfig {
    crate::mcp_oauth::builtin_providers("http://localhost:3000/api/mcp/oauth/callback")
        .into_iter()
        .find(|p| p.provider_id == "google")
        .expect("google is a built-in provider")
}

/// The synthetic Google client secret, assembled at run time so no
/// contiguous `GOCSPX-…` literal sits in the source (source scanners flag a
/// fake one exactly like a live one).
fn google_client_secret() -> String {
    ["GOCSPX", "-super-", "secret-value-9f3c"].concat()
}

fn saved_client() -> McpOAuthClientConfig {
    McpOAuthClientConfig {
        provider_id: "google".into(),
        client_id: "1234567890-abcdef.apps.googleusercontent.com".into(),
        client_secret: google_client_secret(),
        auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
        token_url: "https://oauth2.googleapis.com/token".into(),
        scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".into()],
        redirect_uri: "http://localhost:3000/api/mcp/oauth/callback".into(),
    }
}

fn expired_token_with_refresh() -> McpOAuthToken {
    McpOAuthToken {
        provider_id: "google".into(),
        access_token: "ya29.expired".into(),
        refresh_token: Some("1//refresh".into()),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(2)),
        scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".into()],
    }
}

#[test]
fn google_saved_client_id_comes_back_but_the_secret_never_does() {
    let entry =
        MethodHandler::oauth_provider_entry(&google_template(), None, Some(saved_client()));

    assert_eq!(entry["configured"], json!(true));
    assert_eq!(
        entry["client_id"],
        json!("1234567890-abcdef.apps.googleusercontent.com"),
        "the saved client id must be shown — a placeholder reads as data loss"
    );
    assert_eq!(entry["has_client_secret"], json!(true));
    assert_eq!(entry["client_secret_masked"], json!("••••9f3c"));

    // The whole response, serialized, must not contain the secret in any form.
    let raw = serde_json::to_string(&entry).unwrap();
    assert!(
        !raw.contains(&google_client_secret()),
        "client secret leaked into the dashboard payload: {raw}"
    );
    assert!(
        !raw.contains("super-secret"),
        "secret fragment leaked: {raw}"
    );
}

#[test]
fn google_expired_access_token_with_refresh_token_still_reads_as_connected() {
    let entry = MethodHandler::oauth_provider_entry(
        &google_template(),
        Some(expired_token_with_refresh()),
        Some(saved_client()),
    );

    // The hour-old access token is stale, but the connection is live.
    assert_eq!(entry["token_status"], json!("authenticated"));
    assert_eq!(entry["status"], json!("authenticated"));
    assert_eq!(entry["can_refresh"], json!(true));
    assert_eq!(entry["access_token_valid"], json!(false));
}

#[test]
fn google_expired_token_without_refresh_needs_reauthorization() {
    let mut token = expired_token_with_refresh();
    token.refresh_token = None;
    let entry = MethodHandler::oauth_provider_entry(
        &google_template(),
        Some(token),
        Some(saved_client()),
    );

    assert_eq!(entry["token_status"], json!("expired"));
    assert_eq!(entry["can_refresh"], json!(false));
}

#[test]
fn google_with_nothing_saved_reports_unconfigured() {
    let entry = MethodHandler::oauth_provider_entry(&google_template(), None, None);

    assert_eq!(entry["configured"], json!(false));
    assert_eq!(entry["client_id"], json!(""));
    assert_eq!(entry["has_client_secret"], json!(false));
    assert_eq!(entry["client_secret_masked"], json!(""));
    assert_eq!(entry["token_status"], json!("none"));
}

#[test]
fn google_provider_carries_a_display_name_for_the_card() {
    let entry = MethodHandler::oauth_provider_entry(&google_template(), None, None);
    assert_eq!(entry["name"], json!("Google"));
    assert_eq!(MethodHandler::oauth_provider_name("github"), "GitHub");
}

#[tokio::test]
async fn google_path_replacement_requires_confirmation_and_preserves_vault() {
    let home = tempfile::tempdir().unwrap();
    crate::mcp_oauth::google_home_test_fixture::connect(home.path(), "owned-setup-access", Some("owned-setup-refresh")).await;
    let vault = std::fs::read(home.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let params = json!({"mode":"apps_script","url":"https://script.google.com/macros/s/test/exec","secret":"bridge-test-secret"});
    let rejected = handler.handle_google_credentials_set(params.clone()).await;
    assert!(matches!(rejected, WsFrame::Response { ok:false, .. }));
    let mut confirmed = params;
    confirmed["replace_confirmed"] = json!(true);
    assert!(matches!(handler.handle_google_credentials_set(confirmed).await, WsFrame::Response { ok:true, .. }));
    assert_eq!(crate::google_workspace::selected_path(home.path()).unwrap(), "apps_script");
    assert!(matches!(crate::google_workspace::resolve_backend(home.path()).await.unwrap(), crate::google_workspace::GoogleBackend::AppsScript(_)));
    // Config publication may rotate non-secret Home metadata; bearer material
    // remains present and the ordinary OAuth source is no longer selected.
    let stored: Value = serde_json::from_slice(&std::fs::read(home.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap()).unwrap();
    let before: Value = serde_json::from_slice(&vault).unwrap();
    assert_eq!(stored[0]["access_token"], before[0]["access_token"]);
    assert_eq!(stored[0]["refresh_token"], before[0]["refresh_token"]);
    assert!(crate::google_workspace::get_valid_google_token(home.path()).await.is_err());
    assert!(matches!(handler.handle_google_credentials_set(json!({"mode":"oauth","replace_confirmed":true,"testing":true})).await, WsFrame::Response { ok:true, .. }));
    assert_eq!(crate::google_workspace::selected_path(home.path()).unwrap(), "apps_script");
    assert!(matches!(crate::google_workspace::resolve_backend(home.path()).await.unwrap(), crate::google_workspace::GoogleBackend::AppsScript(_)));
    assert!(crate::google_workspace::get_valid_google_token(home.path()).await.is_err());
    assert_eq!(crate::mcp_oauth::google_setup_status(home.path())["testing"], true);
    assert!(matches!(handler.handle_google_credentials_set(json!({"mode":"none","replace_confirmed":true})).await, WsFrame::Response { ok:true, .. }));
    assert!(crate::google_workspace::resolve_backend(home.path()).await.is_err());
}

// Not run: source-only regression coverage for staged OAuth activation.
#[tokio::test(flavor = "current_thread")]
async fn google_callback_activates_oauth_and_retains_previous_bridge() {
    use crate::mcp_oauth::home_lifecycle as flow;
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    assert!(matches!(handler.handle_google_credentials_set(json!({
        "mode":"apps_script", "url":"https://script.google.com/macros/s/test/exec", "secret":"bridge-test-secret"
    })).await, WsFrame::Response { ok:true, .. }));
    let before = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let before: toml::Table = before.parse().unwrap();
    assert!(matches!(handler.handle_google_credentials_set(json!({"mode":"oauth", "replace_confirmed":true})).await, WsFrame::Response { ok:true, .. }));
    let input = flow::GoogleHomeStartInput::new(Some("owned-client"), Some("owned-secret"), "test-admin").unwrap();
    let plan = flow::capture_google_home_start(home.path(), input).unwrap();
    let (_, state, pending) = flow::begin_google_home_authorization(home.path(), plan).await.unwrap().into_response_parts();
    assert_eq!(crate::google_workspace::selected_path(home.path()).unwrap(), "apps_script");
    let _wire = crate::google_identity::home_source::callback_testing::Wire::json(home.path(), None);
    flow::complete_google_home_authorization(home.path(), pending, &state, zeroize::Zeroizing::new("owned-code".into())).await.unwrap();
    assert_eq!(crate::google_workspace::selected_path(home.path()).unwrap(), "oauth");
    let after = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let after: toml::Table = after.parse().unwrap();
    assert_eq!(after["integrations"]["google_inactive_credentials"]["google_apps_script"], before["integrations"]["google_apps_script"]);
    assert!(after["integrations"].get("google_apps_script").is_none());
}
