//! B2 host/RAM source declarations. NOT COMPILED OR RUN.
use super::*;
use crate::mcp_oauth::{self as api, home_store};
fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected success: {other:?}"),
    }
}
fn refused(frame: WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: false, .. })
}
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    d
}
#[tokio::test(flavor = "current_thread")]
async fn google_oauth_start_requires_data_disclosure_acknowledgement() {
    let d = fixture();
    let h = MethodHandler::new(d.path().to_path_buf()).await;
    assert!(refused(
        h.handle_mcp_oauth_start(
            json!({"provider_id":"google","client_id":"owned-client"}),
            &UserContext::admin_fallback()
        )
        .await
    ));
    assert!(h.mcp_oauth_pending.read().await.is_empty());
    assert!(!d.path().join(api::TOKEN_FILE).exists());
}

#[tokio::test(flavor = "current_thread")]
async fn renamed_google_provider_cannot_enter_generic_oauth_storage() {
    let d = fixture();
    let h = MethodHandler::new(d.path().to_path_buf()).await;
    assert!(refused(
        h.handle_mcp_oauth_start(
            json!({
                "provider_id":"renamed-google",
                "client_id":"owned-client",
                "auth_url":"https://accounts.google.com/o/oauth2/v2/auth",
                "token_url":"https://oauth2.googleapis.com/token",
                "scopes":["https://www.googleapis.com/auth/gmail.readonly"]
            }),
            &UserContext::admin_fallback()
        )
        .await
    ));
    assert!(h.mcp_oauth_pending.read().await.is_empty());
    assert!(api::get_client_config(d.path(), "renamed-google").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn actual_handler_only_inserts_after_durable_ready_and_replaces_latest_google() {
    let d = fixture();
    let h = MethodHandler::new(d.path().to_path_buf()).await;
    let first = payload(
        h.handle_mcp_oauth_start(json!({"provider_id":"google","client_id":"owned-client","acknowledge_google_data":true}), &UserContext::admin_fallback())
            .await,
    );
    let second = payload(
        h.handle_mcp_oauth_start(json!({"provider_id":"google","client_id":"owned-client","acknowledge_google_data":true}), &UserContext::admin_fallback())
            .await,
    );
    assert_ne!(first["state"], second["state"]);
    let map = h.mcp_oauth_pending.read().await;
    assert_eq!(map.len(), 1);
    assert!(map.contains_key(second["state"].as_str().unwrap()));
    drop(map);
    let row: Value =
        serde_json::from_slice(&std::fs::read(d.path().join(api::TOKEN_FILE)).unwrap()).unwrap();
    assert_eq!(row[0]["google_home"]["state"], "ready");
    assert_eq!(
        api::builtin_providers(&api::redirect_uri(d.path()))
            .into_iter()
            .find(|p| p.provider_id == "google")
            .unwrap()
            .scopes
            .len(),
        12
    );
}
#[tokio::test(flavor = "current_thread")]
async fn actual_handler_start_durability_refusal_has_no_url_or_ram_pending() {
    let d = fixture();
    let h = MethodHandler::new(d.path().to_path_buf()).await;
    let _fault = home_store::test_probe::fail_start_gate(d.path());
    assert!(refused(
        h.handle_mcp_oauth_start(json!({"provider_id":"google","client_id":"owned-client","acknowledge_google_data":true}), &UserContext::admin_fallback())
            .await
    ));
    assert!(h.mcp_oauth_pending.read().await.is_empty());
    assert!(!d.path().join(api::TOKEN_FILE).exists());
}
#[tokio::test(flavor = "current_thread")]
async fn actual_google_pipeline_final_ram_wait_revalidates_same_ticket() {
    let d = fixture();
    let map = tokio::sync::RwLock::new(std::collections::HashMap::new());
    let held = map.write().await;
    let params = json!({"client_id":"owned-client"});
    let call = start_google_home_rpc(d.path(), &params, "test-admin", &map);
    tokio::pin!(call);
    tokio::select! {_=&mut call=>panic!("unexpected completion while RAM lock held"),_=tokio::time::sleep(std::time::Duration::from_millis(30))=>{}}
    let mut client = api::get_client_config(d.path(), "google").unwrap();
    client.client_id = "other-client".into();
    api::upsert_client_config(d.path(), client).unwrap();
    drop(held);
    assert!(call.await.is_err());
    assert!(map.read().await.is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn state_provider_error_missing_code_and_replay_consume_google_once() {
    for suffix in [
        "error=access_denied&error_description=%3Cscript%3E".to_owned(),
        String::new(),
        format!(
            "error=access_denied&error_description={}",
            "x".repeat(18_000)
        ),
    ] {
        let d = fixture();
        let map = tokio::sync::RwLock::new(std::collections::HashMap::new());
        let result = start_google_home_rpc(
            d.path(),
            &json!({"client_id":"owned-client"}),
            "test-admin",
            &map,
        )
        .await
        .unwrap();
        let state = result["state"].as_str().unwrap();
        assert!(
            api::complete_callback_query(
                d.path(),
                &map,
                &format!("state={state}&{suffix}"),
                |_| true
            )
            .await
            .is_err()
        );
        assert!(map.read().await.is_empty());
        assert!(
            api::complete_callback_query(
                d.path(),
                &map,
                &format!("state={state}&code=owned-code"),
                |_| true
            )
            .await
            .is_err()
        );
    }
}
#[test]
fn other_constructor_cannot_make_google_legacy_flow() {
    let config = api::builtin_providers("http://localhost/callback")
        .into_iter()
        .find(|p| p.provider_id == "google")
        .unwrap();
    assert!(api::PendingOAuth::other(config, "owned-verifier".into()).is_err());
}
#[tokio::test(flavor = "current_thread")]
async fn generic_google_exchange_refuses_before_any_provider_or_key() {
    let d = fixture();
    let wire = crate::google_identity::home_source::callback_testing::Wire::json(d.path(), None);
    let mut config = api::builtin_providers("http://localhost/callback")
        .into_iter()
        .find(|p| p.provider_id == "google")
        .unwrap();
    config.client_id = "owned-client".into();
    config.token_url = wire.url();
    assert!(
        api::exchange_and_store(d.path(), &config, "owned-code", "owned-verifier")
            .await
            .is_err()
    );
    assert!(
        api::exchange_code(&config, "owned-code", "owned-verifier")
            .await
            .is_err()
    );
    config.provider_id = "renamed-provider".into();
    assert!(api::PendingOAuth::other(config.clone(), "owned-verifier".into()).is_err());
    assert!(
        api::exchange_and_store(d.path(), &config, "owned-code", "owned-verifier")
            .await
            .is_err()
    );
    assert!(
        api::exchange_code(&config, "owned-code", "owned-verifier")
            .await
            .is_err()
    );
    assert!(api::refresh_token(&config, "owned-refresh").await.is_err());
    assert_eq!(wire.requests(), 0);
    assert!(!d.path().join(".keyfile").exists());
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_github_callback_coexists_without_rotating_or_decrypting_google() {
    let d = fixture();
    std::fs::write(d.path().join(".keyfile"), [9u8; 32]).unwrap();
    let map = tokio::sync::RwLock::new(std::collections::HashMap::new());
    start_google_home_rpc(
        d.path(),
        &json!({"client_id":"owned-client"}),
        "test-admin",
        &map,
    )
    .await
    .unwrap();
    let before: Value =
        serde_json::from_slice(&std::fs::read(d.path().join(api::TOKEN_FILE)).unwrap()).unwrap();
    let probe = home_store::test_probe::Guard::install(d.path());
    let wire = crate::google_identity::home_source::callback_testing::Wire::json(d.path(), None);
    let mut config = api::builtin_providers("http://localhost/callback")
        .into_iter()
        .find(|p| p.provider_id == "github")
        .unwrap();
    config.client_id = "owned-other".into();
    config.token_url = wire.url();
    let state = uuid::Uuid::new_v4().to_string();
    map.write().await.insert(
        state.clone(),
        api::PendingOAuth::other(config, "owned-verifier".into()).unwrap(),
    );
    assert_eq!(
        api::complete_callback_query(
            d.path(),
            &map,
            &format!("state={state}&code=owned-other-code"),
            |_| true,
        )
        .await
        .unwrap(),
        "github"
    );
    crate::github_workspace::enable_integration(d.path()).unwrap();
    let after: Value =
        serde_json::from_slice(&std::fs::read(d.path().join(api::TOKEN_FILE)).unwrap()).unwrap();
    assert_eq!(before[0], after[0]);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(wire.requests(), 1);
    assert_eq!(map.read().await.len(), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn actual_callback_query_scope_cannot_override_partial_token_response() {
    let d = fixture();
    let map = tokio::sync::RwLock::new(std::collections::HashMap::new());
    let start = start_google_home_rpc(
        d.path(),
        &json!({"client_id":"owned-client"}),
        "test-admin",
        &map,
    )
    .await
    .unwrap();
    let wire = crate::google_identity::home_source::callback_testing::Wire::scenario(
        d.path(),
        "partial_scope",
    );
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("state", start["state"].as_str().unwrap())
        .append_pair("code", "owned-code")
        .append_pair("scope", &crate::google_workspace::REQUIRED_SCOPES.join(" "))
        .finish();
    assert!(
        api::complete_callback_query(d.path(), &map, &query, |_| true)
            .await
            .is_err()
    );
    assert!(map.read().await.is_empty());
    assert_eq!(wire.requests(), 1);
    let rows: Value =
        serde_json::from_slice(&std::fs::read(d.path().join(api::TOKEN_FILE)).unwrap()).unwrap();
    assert!(rows[0].get("access_token").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn home_callback_rechecks_that_the_consent_actor_is_still_admin_before_provider_io() {
    let d = fixture();
    let map = tokio::sync::RwLock::new(std::collections::HashMap::new());
    let start = start_google_home_rpc(
        d.path(),
        &json!({"client_id":"owned-client"}),
        "admin-at-start",
        &map,
    )
    .await
    .unwrap();
    let wire = crate::google_identity::home_source::callback_testing::Wire::json(d.path(), None);
    let query = format!("state={}&code=owned-code", start["state"].as_str().unwrap());
    assert!(
        api::complete_callback_query(d.path(), &map, &query, |_| false)
            .await
            .is_err()
    );
    assert_eq!(wire.requests(), 0);
    assert!(map.read().await.is_empty());
}
