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
