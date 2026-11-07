use super::*;
use base64::Engine as _;

const RESOURCE: &str = "https://mcp.example.com/mcp";
fn validate_oauth_token(raw: &str, home: &Path) -> Result<Principal, AuthError> {
    super::validate_oauth_token(raw, home, RESOURCE)
}
fn mint_tokens(store: &mut OAuthStore, id: &str, name: &str, scope: &str, family: &str) -> Value {
    super::mint_tokens(store, id, name, scope, family, RESOURCE)
}
fn token_response(home: &Path, request: &HashMap<String, String>) -> Response {
    super::token_response(home, request, RESOURCE)
}

fn register_response(home: &Path, request: Value) -> Response {
    super::register_response(home, request, &RegistrationBudget::default())
}

fn form(values: &[(&str, &str)]) -> HashMap<String, String> {
    values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

async fn body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn fixture(home: &Path) -> Value {
    let key = "ddc_dev_0123456789abcdef0123456789abcdef";
    std::fs::write(home.join("config.toml"), format!(
        "[mcp_keys.{key}]\nclient_id='operator'\nscopes=['memory:read','wiki:read']\nis_external=false\ncreated_at='{}'\n",
        chrono::Utc::now().to_rfc3339(),
    )).unwrap();
    update_store(home, |store| {
        store.grants.insert("family".into(), Grant {
            resource: RESOURCE.into(),            client_id: "client".into(), client_name: "Example".into(), scope: "memory:read wiki:read".into(),
            issued_unix: now_unix(), last_used_unix: None,
            consent: Consent::capture(home, key, "memory:read wiki:read").unwrap(),
        });
        mint_tokens(store, "client", "Example", "memory:read wiki:read", "family")
    }).unwrap()
}

fn refresh(home: &Path, token: &str) -> Response {
    token_response(home, &form(&[("grant_type", "refresh_token"), ("client_id", "client"), ("refresh_token", token)]))
}

#[tokio::test]
async fn refresh_after_consent_key_revocation_deletes_family() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let response = refresh(home.path(), tokens["refresh_token"].as_str().unwrap());
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(response).await["error"], "invalid_grant");
    let store = load_store(home.path()).unwrap();
    assert!(store.refresh.is_empty() && store.tokens.is_empty() && store.grants.is_empty());
}

#[tokio::test]
async fn changed_or_unreadable_consent_cannot_refresh() {
    for replacement in ["[broken", "", "[mcp_keys]"] {
        let home = tempfile::tempdir().unwrap();
        let tokens = fixture(home.path());
        std::fs::write(home.path().join("config.toml"), replacement).unwrap();
        assert_eq!(body(refresh(home.path(), tokens["refresh_token"].as_str().unwrap())).await["error"], "invalid_grant");
    }
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let path = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap().replace("'memory:read','wiki:read'", "'memory:read'");
    std::fs::write(path, raw).unwrap();
    assert_eq!(body(refresh(home.path(), tokens["refresh_token"].as_str().unwrap())).await["error"], "invalid_grant");
}

#[tokio::test]
async fn refresh_scopes_only_narrow_and_replay_revokes_family() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let raw = tokens["refresh_token"].as_str().unwrap();
    let mut request = form(&[("grant_type", "refresh_token"), ("client_id", "client"), ("refresh_token", raw), ("scope", "admin")]);
    assert_eq!(body(token_response(home.path(), &request)).await["error"], "invalid_scope");
    request.insert("scope".into(), "memory:read".into());
    let next = body(token_response(home.path(), &request)).await;
    assert_eq!(next["scope"], "memory:read");
    assert!(validate_oauth_token(next["access_token"].as_str().unwrap(), home.path()).is_ok());
    assert_eq!(body(refresh(home.path(), raw)).await["error"], "invalid_grant");
    assert!(validate_oauth_token(next["access_token"].as_str().unwrap(), home.path()).is_err());
}

#[tokio::test]
async fn revoke_rotated_refresh_revokes_access_and_family() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let old_refresh = tokens["refresh_token"].as_str().unwrap();
    let next = body(refresh(home.path(), old_refresh)).await;
    let response = revoke_response(home.path(), &form(&[("client_id", "client"), ("token", old_refresh), ("token_type_hint", "access_token")]));
    assert_eq!(response.status(), StatusCode::OK);
    for pair in [&tokens, &next] {
        assert!(validate_oauth_token(pair["access_token"].as_str().unwrap(), home.path()).is_err());
        assert_eq!(body(refresh(home.path(), pair["refresh_token"].as_str().unwrap())).await["error"], "invalid_grant");
    }
}

#[tokio::test]
async fn revocation_unknown_wrong_client_and_missing_client() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let raw = tokens["refresh_token"].as_str().unwrap();
    for (client, token) in [("client", "unknown"), ("other", raw)] {
        assert_eq!(revoke_response(home.path(), &form(&[("client_id", client), ("token", token)])).status(), StatusCode::OK);
    }
    assert!(validate_oauth_token(tokens["access_token"].as_str().unwrap(), home.path()).is_ok());
    assert_eq!(body(revoke_response(home.path(), &form(&[("token", raw)]))).await["error"], "invalid_client");
    assert_eq!(body(revoke_response(home.path(), &form(&[("client_id", "client")]))).await["error"], "invalid_request");
    assert_eq!(revoke_response(home.path(), &form(&[("client_id", "client"), ("token", tokens["access_token"].as_str().unwrap())])).status(), StatusCode::OK);
    assert!(load_store(home.path()).unwrap().refresh.is_empty());
}

#[tokio::test]
async fn metadata_advertises_revocation_endpoint() {
    let metadata = body(authorization_metadata("https://mcp.example.com")).await;
    assert_eq!(metadata["revocation_endpoint"], "https://mcp.example.com/oauth/revoke");
    assert_eq!(metadata["revocation_endpoint_auth_methods_supported"], json!(["none"]));
}

#[tokio::test]
async fn legacy_refresh_without_consent_is_invalid_grant() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    update_store(home.path(), |s| {
        s.grants.clear();
        for t in s.tokens.values_mut().chain(s.refresh.values_mut()) { t.family_id.clear(); }
    }).unwrap();
    assert_eq!(body(refresh(home.path(), tokens["refresh_token"].as_str().unwrap())).await["error"], "invalid_grant");
}

#[tokio::test]
async fn authorization_code_rechecks_consent_before_minting() {
    let home = tempfile::tempdir().unwrap();
    fixture(home.path());
    let verifier = "a".repeat(43);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    update_store(home.path(), |s| {
        s.codes.insert(sha256_hex("code"), PendingCode {
            resource: RESOURCE.into(),            client_id: "client".into(), client_name: "Example".into(), redirect_uri: "https://example.com/cb".into(),
            code_challenge: challenge, scope: "memory:read".into(), issued_unix: now_unix(),
            consent: Some(s.grants["family"].consent.clone()),
        });
    }).unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let request = form(&[("grant_type", "authorization_code"), ("code", "code"), ("client_id", "client"),
        ("redirect_uri", "https://example.com/cb"), ("code_verifier", &verifier)]);
    assert_eq!(body(token_response(home.path(), &request)).await["error"], "invalid_grant");
    assert!(load_store(home.path()).unwrap().codes.is_empty());
}

#[tokio::test]
async fn expired_access_token_preserves_refresh_family() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let mut store = load_store(home.path()).unwrap();
    store.tokens.get_mut(&sha256_hex(tokens["access_token"].as_str().unwrap())).unwrap().expires_unix = now_unix() - 1;
    // Keep the expired row so validation, rather than write-time pruning, sees it.
    std::fs::write(store_path(home.path()), serde_json::to_vec(&store).unwrap()).unwrap();
    assert!(validate_oauth_token(tokens["access_token"].as_str().unwrap(), home.path()).is_err());
    let next = body(refresh(home.path(), tokens["refresh_token"].as_str().unwrap())).await;
    assert!(validate_oauth_token(next["access_token"].as_str().unwrap(), home.path()).is_ok());
}

#[tokio::test]
async fn wrong_refresh_client_does_not_consume_token() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let raw = tokens["refresh_token"].as_str().unwrap();
    let request = form(&[("grant_type", "refresh_token"), ("client_id", "other"), ("refresh_token", raw)]);
    assert_eq!(body(token_response(home.path(), &request)).await["error"], "invalid_grant");
    assert_eq!(refresh(home.path(), raw).status(), StatusCode::OK);
}

#[test]
fn oauth_access_token_format_is_anchored() {
    assert!(is_oauth_access_token(&format!("ddc_oauth_{}", "a".repeat(64))));
    for token in ["ddc_oauth_", "ddc_oauth_r_deadbeef", "prefix_ddc_oauth_deadbeef"] {
        assert!(!is_oauth_access_token(token));
    }
}

#[test]
fn unknown_revocation_does_not_rewrite_store() {
    let home = tempfile::tempdir().unwrap();
    fixture(home.path());
    let compact = serde_json::to_vec(&load_store(home.path()).unwrap()).unwrap();
    std::fs::write(store_path(home.path()), &compact).unwrap();
    assert_eq!(revoke_response(home.path(), &form(&[("client_id", "client"), ("token", "unknown")])).status(), StatusCode::OK);
    assert_eq!(std::fs::read(store_path(home.path())).unwrap(), compact);
}

#[tokio::test]
async fn authorization_code_is_single_use_and_tracks_last_used() {
    let home = tempfile::tempdir().unwrap();
    fixture(home.path());
    let verifier = "b".repeat(43);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    update_store(home.path(), |s| {
        s.codes.insert(sha256_hex("new-code"), PendingCode {
            resource: RESOURCE.into(),            client_id: "new-client".into(), client_name: "New client".into(), redirect_uri: "https://example.com/cb".into(),
            code_challenge: challenge, scope: "memory:read".into(), issued_unix: now_unix(),
            consent: Some(s.grants["family"].consent.clone()),
        });
    }).unwrap();
    let request = form(&[("grant_type", "authorization_code"), ("code", "new-code"), ("client_id", "new-client"),
        ("redirect_uri", "https://example.com/cb"), ("code_verifier", &verifier)]);
    let response = token_response(home.path(), &request);
    assert_eq!(response.status(), StatusCode::OK);
    let tokens = body(response).await;
    assert_eq!(body(token_response(home.path(), &request)).await["error"], "invalid_grant");
    let before = load_store(home.path()).unwrap().active_grants(home.path()).unwrap();
    assert!(before.iter().any(|g| g["client_name"] == "New client" && g["last_used_unix"].is_null()));
    let principal = validate_oauth_token(tokens["access_token"].as_str().unwrap(), home.path()).unwrap();
    assert!(principal.is_external);
    assert_eq!(principal.scopes, parse_scopes("memory:read").unwrap());
    note_oauth_use(tokens["access_token"].as_str().unwrap(), home.path());
    let after = load_store(home.path()).unwrap().active_grants(home.path()).unwrap();
    assert!(after.iter().any(|g| g["client_name"] == "New client" && g["last_used_unix"].as_i64().is_some()));
}

#[tokio::test]
async fn refresh_capacity_preserves_current_token_and_replay_evidence() {
    use duduclaw_core::mcp_oauth_store::MAX_FAMILY_ROWS;
    let home = tempfile::tempdir().unwrap();
    let first = fixture(home.path());
    let old = first["refresh_token"].as_str().unwrap();
    update_store(home.path(), |s| {
        let mut spent = s.refresh[&sha256_hex(old)].clone();
        spent.used = true;
        for n in 0..MAX_FAMILY_ROWS - 4 { s.refresh.insert(format!("spent-{n}"), spent.clone()); }
    }).unwrap();
    let next = body(refresh(home.path(), old)).await;
    let current = next["refresh_token"].as_str().unwrap();
    let before = std::fs::read(store_path(home.path())).unwrap();
    let denied = refresh(home.path(), current);
    assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body(denied).await["error"], "temporarily_unavailable");
    assert_eq!(std::fs::read(store_path(home.path())).unwrap(), before);
    let stored = load_store(home.path()).unwrap();
    assert!(!stored.refresh[&sha256_hex(current)].used);
    assert!(stored.refresh[&sha256_hex(old)].used);
    assert_eq!(body(refresh(home.path(), old)).await["error"], "invalid_grant");
    assert!(load_store(home.path()).unwrap().refresh.is_empty());
}

#[test]
fn access_validation_is_read_only_even_while_writer_is_locked() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let raw = tokens["access_token"].as_str().unwrap().to_owned();
    let path = store_path(home.path());
    let before = std::fs::read(&path).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::scope(|threads| {
        let lock_path = &path;
        threads.spawn(move || duduclaw_core::with_file_lock(lock_path, || {
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        }).unwrap());
        ready_rx.recv().unwrap();
        let home = home.path();
        threads.spawn(move || result_tx.send(validate_oauth_token(&raw, home).is_ok()).unwrap());
        let result = result_rx.recv_timeout(std::time::Duration::from_secs(2));
        release_tx.send(()).unwrap();
        assert_eq!(result.unwrap(), true);
    });
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn last_use_is_throttled_and_never_restores_revoked_family() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let raw = tokens["access_token"].as_str().unwrap();
    note_oauth_use(raw, home.path());
    let before = std::fs::read(store_path(home.path())).unwrap();
    note_oauth_use(raw, home.path());
    assert_eq!(std::fs::read(store_path(home.path())).unwrap(), before);
    update_store(home.path(), |s| s.revoke_family("family")).unwrap();
    note_oauth_use(raw, home.path());
    assert!(load_store(home.path()).unwrap().grants.is_empty());
}

#[tokio::test]
async fn authorization_code_capacity_refusal_can_retry_after_revocation() {
    use duduclaw_core::mcp_oauth_store::MAX_GRANTS;
    let home = tempfile::tempdir().unwrap();
    fixture(home.path());
    let verifier = "c".repeat(43);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    update_store(home.path(), |s| {
        let grant = s.grants["family"].clone();
        let token = s.tokens.values().next().unwrap().clone();
        for n in 0..MAX_GRANTS - 1 {
            let id = format!("full-{n}");
            s.grants.insert(id.clone(), grant.clone());
            let mut token = token.clone(); token.family_id = id.clone();
            s.tokens.insert(id, token);
        }
        s.codes.insert(sha256_hex("retry-code"), PendingCode {
            resource: RESOURCE.into(),            client_id: "client".into(), client_name: "Example".into(), redirect_uri: "https://example.com/cb".into(),
            code_challenge: challenge, scope: "memory:read".into(), issued_unix: now_unix(),
            consent: Some(grant.consent),
        });
    }).unwrap();
    let request = form(&[("grant_type", "authorization_code"), ("code", "retry-code"), ("client_id", "client"),
        ("redirect_uri", "https://example.com/cb"), ("code_verifier", &verifier)]);
    assert_eq!(token_response(home.path(), &request).status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(load_store(home.path()).unwrap().codes.contains_key(&sha256_hex("retry-code")));
    update_store(home.path(), |s| s.revoke_family("full-0")).unwrap();
    assert_eq!(token_response(home.path(), &request).status(), StatusCode::OK);
    assert_eq!(body(token_response(home.path(), &request)).await["error"], "invalid_grant");
}

#[tokio::test]
async fn resource_indicators_reject_empty_foreign_and_duplicate_foreign_targets() {
    assert_eq!(requested_resource(None, RESOURCE).unwrap(), RESOURCE);
    assert_eq!(requested_resource(Some(RESOURCE), RESOURCE).unwrap(), RESOURCE);
    for target in ["", "https://attacker.example/mcp", "https://mcp.example.com/other"] {
        assert_eq!(body(requested_resource(Some(target), RESOURCE).unwrap_err()).await["error"], "invalid_target");
        let params = vec![("resource".into(), target.into()), ("resource".into(), RESOURCE.into())];
        assert_eq!(body(resource_params(&params, RESOURCE).unwrap_err()).await["error"], "invalid_target");
    }
}

#[tokio::test]
async fn refresh_resource_is_pinned_and_bad_target_does_not_consume_token() {
    let home = tempfile::tempdir().unwrap();
    let tokens = fixture(home.path());
    let raw = tokens["refresh_token"].as_str().unwrap();
    let mut request = form(&[("grant_type", "refresh_token"), ("client_id", "client"),
        ("refresh_token", raw), ("resource", "https://attacker.example/mcp")]);
    let before = std::fs::read(store_path(home.path())).unwrap();
    assert_eq!(body(token_response(home.path(), &request)).await["error"], "invalid_target");
    assert_eq!(std::fs::read(store_path(home.path())).unwrap(), before);
    request.insert("resource".into(), RESOURCE.into());
    let next = body(token_response(home.path(), &request)).await;
    assert!(next["access_token"].is_string());
    let store = load_store(home.path()).unwrap();
    assert!(store.tokens.values().chain(store.refresh.values()).all(|t| t.resource == RESOURCE));
    assert!(store.grants.values().all(|g| g.resource == RESOURCE));
    assert!(super::validate_oauth_token(next["access_token"].as_str().unwrap(), home.path(), "https://other.example/mcp").is_err());
}

#[tokio::test]
async fn stored_refresh_audience_cannot_be_retargeted_or_migrated_implicitly() {
    for stored_resource in ["", "https://other.example/mcp"] {
        let home = tempfile::tempdir().unwrap();
        let tokens = fixture(home.path());
        update_store(home.path(), |store| {
            for t in store.tokens.values_mut().chain(store.refresh.values_mut()) { t.resource = stored_resource.into(); }
            for g in store.grants.values_mut() { g.resource = stored_resource.into(); }
        }).unwrap();
        assert!(validate_oauth_token(tokens["access_token"].as_str().unwrap(), home.path()).is_err());
        assert_eq!(body(refresh(home.path(), tokens["refresh_token"].as_str().unwrap())).await["error"], "invalid_grant");
    }
}

#[tokio::test]
async fn code_exchange_requires_the_stored_audience_and_defaults_only_to_server() {
    for stored_resource in [RESOURCE, "", "https://other.example/mcp"] {
        let home = tempfile::tempdir().unwrap();
        fixture(home.path());
        let verifier = "r".repeat(43);
        update_store(home.path(), |s| {
            s.codes.insert(sha256_hex("resource-code"), PendingCode {
                resource: stored_resource.into(), client_id: "client".into(), client_name: "Example".into(),
                redirect_uri: "https://example.com/cb".into(),
                code_challenge: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                scope: "memory:read".into(), issued_unix: now_unix(), consent: Some(s.grants["family"].consent.clone()),
            });
        }).unwrap();
        let mut request = form(&[("grant_type", "authorization_code"), ("code", "resource-code"),
            ("client_id", "client"), ("redirect_uri", "https://example.com/cb"), ("code_verifier", &verifier),
            ("resource", "https://attacker.example/mcp")]);
        assert_eq!(body(token_response(home.path(), &request)).await["error"], "invalid_target");
        assert!(load_store(home.path()).unwrap().codes.contains_key(&sha256_hex("resource-code")));
        request.remove("resource");
        let response = body(token_response(home.path(), &request)).await;
        if stored_resource == RESOURCE {
            assert!(validate_oauth_token(response["access_token"].as_str().unwrap(), home.path()).is_ok());
        } else {
            assert_eq!(response["error"], "invalid_grant");
        }
    }
}

#[tokio::test]
async fn decision_rejects_resource_tampering_before_operator_proof() {
    let home = tempfile::tempdir().unwrap();
    let client = RegisteredClient {
        client_name: "Example".into(), redirect_uris: vec!["https://example.com/cb".into()],
        created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None,
    };
    let decision = DecisionForm {
        resource: Some("https://attacker.example/mcp".into()), client_id: "client".into(),
        redirect_uri: "https://example.com/cb".into(), state: String::new(), code_challenge: "a".repeat(43),
        scope: String::new(), operator_key: String::new(), action: "approve".into(),
    };
    assert_eq!(body(decision_response(home.path(), client, decision, RESOURCE)).await["error"], "invalid_target");
    assert!(!store_path(home.path()).exists());
}

#[tokio::test]
async fn consent_pins_default_resource_and_marks_client_activity() {
    let home = tempfile::tempdir().unwrap();
    fixture(home.path());
    let registered = body(register_response(home.path(), json!({
        "client_name": "Consent client", "redirect_uris": ["https://example.com/cb"]
    }))).await;
    let client_id = registered["client_id"].as_str().unwrap();
    let client = load_store(home.path()).unwrap().clients[client_id].clone();
    let decision = DecisionForm {
        resource: None, client_id: client_id.into(), redirect_uri: "https://example.com/cb".into(),
        state: "opaque-state".into(), code_challenge: "a".repeat(43), scope: "memory:read".into(),
        operator_key: "ddc_dev_0123456789abcdef0123456789abcdef".into(), action: "approve".into(),
    };
    let response = decision_response(home.path(), client, decision, RESOURCE);
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let store = load_store(home.path()).unwrap();
    assert_eq!(store.codes.len(), 1);
    assert_eq!(store.codes.values().next().unwrap().resource, RESOURCE);
    assert!(store.clients[client_id].last_used_unix.is_some());
}

#[tokio::test]
async fn registration_capacity_and_quota_refusals_preserve_candidates_and_global_budget() {
    let home = tempfile::tempdir().unwrap();
    let budget = RegistrationBudget::default();
    let metadata = json!({"redirect_uris":["https://example.com/cb"]});
    update_store(home.path(), |store| {
        for n in 0..MAX_CLIENTS {
            store.clients.insert(n.to_string(), RegisteredClient {
                client_name: "Example".into(), redirect_uris: vec![],
                created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None,
            });
        }
    }).unwrap();
    for _ in 0..12 {
        assert_eq!(super::register_response(home.path(), metadata.clone(), &budget).status(), StatusCode::BAD_REQUEST);
    }
    update_store(home.path(), |store| store.clients.clear()).unwrap();
    for _ in 0..10 {
        assert_eq!(super::register_response(home.path(), metadata.clone(), &budget).status(), StatusCode::CREATED);
    }
    let old = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
    update_store(home.path(), |store| {
        for n in store.clients.len()..MAX_CLIENTS {
            store.clients.insert(n.to_string(), RegisteredClient {
                client_name: "Example".into(), redirect_uris: vec![], created_at: old.clone(), last_used_unix: None,
            });
        }
    }).unwrap();
    let before = std::fs::read(store_path(home.path())).unwrap();
    assert_eq!(super::register_response(home.path(), metadata.clone(), &budget).status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(std::fs::read(store_path(home.path())).unwrap(), before);
    assert_eq!(super::register_response(home.path(), metadata, &RegistrationBudget::default()).status(), StatusCode::CREATED);
    assert_eq!(load_store(home.path()).unwrap().clients.len(), MAX_CLIENTS);
}
