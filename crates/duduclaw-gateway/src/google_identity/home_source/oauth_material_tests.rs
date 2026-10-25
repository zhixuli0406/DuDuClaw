//! C1 SOURCE FIRST. All declarations UNRUN; no compiler/runtime evidence.
//! Owned temp homes/wire only; crypto fixtures require separate Root runtime GO.
use super::*;
use serde_json::json;
use std::future::Future;

fn fixture(expired: bool, client: bool) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=true\n",
    )
    .unwrap();
    let expiry = expired.then(|| "2000-01-01T00:00:00Z");
    std::fs::write(d.path().join(crate::mcp_oauth::TOKEN_FILE), serde_json::to_vec(&json!([
        {"provider_id":"google","access_token":"legacy.access","refresh_token":"legacy-refresh","expires_at":expiry,"scopes":["scope-a"],"unknown_google":{"kept":true}},
        {"provider_id":"github","access_token":"foreign-token","refresh_token":null,"expires_at":null,"scopes":["repo"]}
    ])).unwrap()).unwrap();
    if client {
        std::fs::write(d.path().join(crate::mcp_oauth::CLIENT_CONFIG_FILE), serde_json::to_vec(&json!([
            {"provider_id":"google","client_id":format!("owned-{}", uuid::Uuid::new_v4()),"client_secret":"","auth_url":"https://accounts.google.com/o/oauth2/v2/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":["historical-five"]}
        ])).unwrap()).unwrap();
    }
    d
}
fn raw(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(home.join(crate::mcp_oauth::TOKEN_FILE)).unwrap())
        .unwrap()
}
fn mutate(home: &Path, f: impl FnOnce(&mut Value)) {
    let mut rows = raw(home);
    f(&mut rows);
    std::fs::write(
        home.join(crate::mcp_oauth::TOKEN_FILE),
        serde_json::to_vec(&rows).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn original_public_path_fresh_legacy_fewer_scopes_no_client_none_expiry() {
    let d = fixture(false, false);
    let before = raw(d.path());
    let access = crate::google_workspace::get_valid_google_token(d.path())
        .await
        .unwrap();
    assert_eq!(access, "legacy.access");
    let after = raw(d.path());
    for field in [
        "access_token",
        "refresh_token",
        "expires_at",
        "scopes",
        "unknown_google",
    ] {
        assert_eq!(before[0][field], after[0][field]);
    }
    assert_eq!(before[1], after[1]);
    assert!(!d.path().join(".keyfile").exists());
    let bytes = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    assert_eq!(
        crate::google_workspace::get_valid_google_token(d.path())
            .await
            .unwrap(),
        access
    );
    assert_eq!(
        std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap(),
        bytes
    );
}

#[tokio::test]
async fn actionable_missing_data_errors_do_not_need_crypto_or_network() {
    let d = fixture(true, false);
    assert!(matches!(
        get_valid_google_token(d.path()).await,
        Err(GoogleAuthError::ClientConfigMissing)
    ));
    mutate(d.path(), |r| r[0]["refresh_token"] = Value::Null);
    assert!(matches!(
        get_valid_google_token(d.path()).await,
        Err(GoogleAuthError::NoRefreshToken)
    ));
    std::fs::write(d.path().join(crate::mcp_oauth::TOKEN_FILE), b"[]").unwrap();
    assert!(matches!(
        get_valid_google_token(d.path()).await,
        Err(GoogleAuthError::NotConnected { .. })
    ));
    assert!(!d.path().join(".keyfile").exists());
}

#[tokio::test]
async fn proof_exact_revision_payload_home_and_purpose_with_zero_crypto_validation() {
    let d = fixture(false, false);
    let receipt = prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let resolved = resolve_home_oauth_pinned(d.path(), &receipt, &["scope-a"])
        .await
        .unwrap();
    validate_home_oauth_material(d.path(), &receipt, resolved.proof(), &["scope-a"]).unwrap();
    assert!(validate_home_oauth_material(d.path(), &receipt, resolved.proof(), &[]).is_err());
    let other = fixture(false, false);
    assert!(
        validate_home_oauth_material(other.path(), &receipt, resolved.proof(), &["scope-a"])
            .is_err()
    );
    let before = raw(d.path());
    mutate(d.path(), |r| {
        r[0]["access_token"] = json!("replacement.access")
    });
    assert!(
        validate_home_oauth_material(d.path(), &receipt, resolved.proof(), &["scope-a"]).is_err()
    );
    std::fs::write(
        d.path().join(crate::mcp_oauth::TOKEN_FILE),
        serde_json::to_vec(&before).unwrap(),
    )
    .unwrap();
    mutate(d.path(), |r| {
        r[0]["google_home"]["credential"]["token_revision"] = json!(2)
    });
    assert!(
        validate_home_oauth_material(d.path(), &receipt, resolved.proof(), &["scope-a"]).is_err()
    );
    assert!(!d.path().join(".keyfile").exists());
}

#[tokio::test]
async fn unknown_encryption_never_becomes_a_bearer_and_no_key_is_created() {
    for stored in ["enc:v2:fake", "enc:v1:invalid"] {
        let d = fixture(false, false);
        mutate(d.path(), |r| r[0]["access_token"] = json!(stored));
        let receipt = prepare_home_selection(d.path(), &[]).unwrap();
        assert!(
            resolve_home_oauth_pinned(d.path(), &receipt, &[])
                .await
                .is_err()
        );
        assert!(!d.path().join(".keyfile").exists());
    }
}

#[tokio::test]
async fn ordinary_expired_refresh_publishes_exact_revision_and_then_uses_fresh() {
    let d = fixture(true, true);
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let receipt = prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let before = with_metadata(d.path(), |g, s| home_store::observe_home_oauth(g, s)).unwrap();
    let wire = refresh_io::testing::Wire::json(d.path(), None);
    assert_eq!(
        crate::google_workspace::get_valid_google_token(d.path())
            .await
            .unwrap(),
        "owned-access"
    );
    let material = resolve_home_oauth_pinned(d.path(), &receipt, &["scope-a"])
        .await
        .unwrap();
    assert_eq!(wire.requests(), 1);
    let after = with_metadata(d.path(), |g, s| home_store::observe_home_oauth(g, s)).unwrap();
    assert_eq!(
        after.credential.credential_id,
        before.credential.credential_id
    );
    assert_eq!(
        after.credential.logical_epoch,
        before.credential.logical_epoch
    );
    assert_eq!(
        after.credential.token_revision,
        before.credential.token_revision + 1
    );
    validate_home_oauth_material(d.path(), &receipt, material.proof(), &["scope-a"]).unwrap();
    assert_eq!(
        crate::google_workspace::get_valid_google_token(d.path())
            .await
            .unwrap(),
        "owned-access"
    );
    assert_eq!(wire.requests(), 1);
    assert_eq!(raw(d.path())[0]["unknown_google"], json!({"kept":true}));
    assert_eq!(raw(d.path())[1]["access_token"], "foreign-token");
    assert!(
        refresh_lock(&d.path().canonicalize().unwrap())
            .unwrap()
            .try_lock()
            .is_ok()
    );
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    drop(lease);
}

#[tokio::test]
async fn encrypted_fresh_no_client_uses_only_captured_access() {
    let d = fixture(false, false);
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let access = crate::config_crypto::encrypt_value("encrypted.access", d.path()).unwrap();
    mutate(d.path(), |r| {
        r[0]["access_token"] = json!(format!("enc:v1:{access}"));
        r[0]["refresh_token"] = json!("enc:v2:must-not-open-unused-refresh");
    });
    let before = raw(d.path());
    assert_eq!(
        crate::google_workspace::get_valid_google_token(d.path())
            .await
            .unwrap(),
        "encrypted.access"
    );
    assert_eq!(raw(d.path())[0]["access_token"], before[0]["access_token"]);
    assert_eq!(
        raw(d.path())[0]["refresh_token"],
        before[0]["refresh_token"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn first_actual_capture_is_after_local_mutex_not_before_wait() {
    let d = fixture(false, false);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let canonical = d.path().canonicalize().unwrap();
    let lock = refresh_lock(&canonical).unwrap();
    let held = lock.lock().await;
    let mut resolving = Box::pin(resolve_home_oauth_pinned(d.path(), &receipt, &[]));
    std::future::poll_fn(|cx| {
        assert!(resolving.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    mutate(d.path(), |r| {
        r[0]["access_token"] = json!("same-lineage.new-access");
        let revision = r[0]["google_home"]["credential"]["token_revision"]
            .as_u64()
            .unwrap();
        r[0]["google_home"]["credential"]["token_revision"] = json!(revision + 1);
    });
    drop(held);
    let material = resolving.await.unwrap();
    assert_eq!(material.access.as_str(), "same-lineage.new-access");
    validate_home_oauth_material(d.path(), &receipt, material.proof(), &[]).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn global_grant_wait_keeps_exact_captured_raw_even_semantic_whitespace_change() {
    let d = fixture(true, true);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let held = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    let captured = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = captured.clone();
    let _hook = testing::Guard::at(d.path(), 1, move || {
        seen.store(true, std::sync::atomic::Ordering::SeqCst)
    });
    let wire = refresh_io::testing::Wire::json(d.path(), None);
    let mut resolving = Box::pin(resolve_home_oauth_pinned(d.path(), &receipt, &[]));
    std::future::poll_fn(|cx| {
        assert!(resolving.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(captured.load(std::sync::atomic::Ordering::SeqCst));
    let path = d.path().join(crate::mcp_oauth::TOKEN_FILE);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(b' ');
    std::fs::write(path, bytes).unwrap();
    drop(held);
    assert!(matches!(resolving.await, Err(Error::Stale)));
    assert_eq!(wire.requests(), 0);
    assert!(!d.path().join(".keyfile").exists());
}

#[tokio::test]
async fn original_cipher_is_retained_on_omission_not_reencrypted() {
    let d = fixture(true, true);
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let refresh = format!(
        "enc:v1:{}",
        crate::config_crypto::encrypt_value("owned-refresh", d.path()).unwrap()
    );
    mutate(d.path(), |r| r[0]["refresh_token"] = json!(refresh));
    let wire = refresh_io::testing::Wire::json(
        d.path(),
        Some(json!({"access_token":"owned-access","token_type":"Bearer","expires_in":3600})),
    );
    assert_eq!(
        get_valid_google_token(d.path()).await.unwrap(),
        "owned-access"
    );
    assert_eq!(raw(d.path())[0]["refresh_token"], refresh);
    assert_eq!(wire.requests(), 1);
}

#[tokio::test]
async fn redirect_reset_error_and_invalid_body_issue_one_post_and_no_publication() {
    for scenario in [
        "redirect",
        "redirect303",
        "redirect307",
        "redirect308",
        "reset",
        "rejected",
        "bad_expiry",
        "chunk_error",
    ] {
        let d = fixture(true, true);
        std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
        let receipt = prepare_home_selection(d.path(), &[]).unwrap();
        let before = raw(d.path());
        let wire = refresh_io::testing::Wire::scenario(d.path(), scenario);
        assert!(
            resolve_home_oauth_pinned(d.path(), &receipt, &[])
                .await
                .is_err()
        );
        assert_eq!(wire.requests(), 1, "{scenario}");
        assert_eq!(raw(d.path()), before);
    }
}

#[tokio::test]
async fn each_crypto_await_preseal_boundary_rejects_changed_capture_without_retry() {
    for phase in [2, 3, 4, 5, 6, 7, 8, 12] {
        let d = fixture(true, true);
        std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
        let receipt = prepare_home_selection(d.path(), &[]).unwrap();
        let owned = d.path().to_owned();
        let _hook = testing::Guard::at(d.path(), phase, move || {
            mutate(&owned, |r| r[0]["access_token"] = json!("newer-access"))
        });
        let wire = refresh_io::testing::Wire::json(d.path(), None);
        assert!(
            resolve_home_oauth_pinned(d.path(), &receipt, &[])
                .await
                .is_err()
        );
        assert_eq!(wire.requests(), usize::from(phase >= 5));
        assert_eq!(raw(d.path())[0]["access_token"], "newer-access");
        assert!(
            !d.path()
                .join(".google-home-publication-intent.json")
                .exists()
        );
    }
}

#[tokio::test]
async fn malformed_actual_response_and_oversized_body_do_not_publish_or_seal() {
    for body in [
        json!({"access_token":"owned-access","token_type":"Bearer","expires_in":3600,"scope":"scope-a extra\\invalid"}),
        json!({"access_token":"owned-access","token_type":"Bearer","expires_in":3600,"refresh_token":null}),
        json!({"access_token":"owned-access","token_type":"Basic","expires_in":3600}),
        json!({"access_token":"A".repeat(4 * 1024 * 1024 + 1),"token_type":"Bearer","expires_in":3600}),
    ] {
        let d = fixture(true, true);
        let receipt = prepare_home_selection(d.path(), &[]).unwrap();
        let before = raw(d.path());
        let probe = home_store::test_probe::Guard::install(d.path());
        let wire = refresh_io::testing::Wire::json(d.path(), Some(body));
        assert!(
            resolve_home_oauth_pinned(d.path(), &receipt, &[])
                .await
                .is_err()
        );
        assert_eq!(wire.requests(), 1);
        assert_eq!(probe.google_crypto_count(), 0);
        assert_eq!(raw(d.path()), before);
        assert!(!d.path().join(".keyfile").exists());
    }
}

#[tokio::test]
async fn intervening_postpublication_revision_never_labels_new_row_as_old_response() {
    let d = fixture(true, true);
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let owned = d.path().to_owned();
    let _hook = testing::Guard::at(d.path(), 10, move || {
        mutate(&owned, |r| {
            let revision = r[0]["google_home"]["credential"]["token_revision"]
                .as_u64()
                .unwrap();
            r[0]["google_home"]["credential"]["token_revision"] = json!(revision + 1);
        })
    });
    let wire = refresh_io::testing::Wire::json(d.path(), None);
    let result = resolve_home_oauth_pinned(d.path(), &receipt, &[]).await;
    assert_eq!(
        result.err(),
        Some(Error::Stale),
        "postpublication result: hook_fired={}, hook_visits={}, wire_requests={}",
        _hook.fired(),
        _hook.visits(),
        wire.requests(),
    );
    assert_eq!(_hook.fired(), 1, "postpublication mutation must be reached");
    assert_eq!(wire.requests(), 1);
}

#[tokio::test]
async fn publication_faults_never_return_material_or_repeat_refresh() {
    use home_store::oauth_testing::{Fault, FaultGuard};
    for fault in [Fault::GateWrite, Fault::DataRename, Fault::CleanupDirSync] {
        let d = fixture(true, true);
        std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
        let receipt = prepare_home_selection(d.path(), &[]).unwrap();
        let _fault = FaultGuard::at(d.path(), fault);
        let wire = refresh_io::testing::Wire::json(d.path(), None);
        assert!(
            resolve_home_oauth_pinned(d.path(), &receipt, &[])
                .await
                .is_err()
        );
        assert_eq!(wire.requests(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn validator_expiry_and_barriers_never_open_keys_or_take_refresh_locks() {
    let d = fixture(false, false);
    let now = 1_800_000_000;
    let clock = home_store::oauth_testing::ClockGuard::fixed(now);
    mutate(d.path(), |r| {
        r[0]["expires_at"] = json!(chrono::DateTime::from_timestamp(now as i64 + 61, 0).unwrap())
    });
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let material = resolve_home_oauth_pinned(d.path(), &receipt, &[])
        .await
        .unwrap();
    let probe = home_store::test_probe::Guard::install(d.path());
    let lock = refresh_lock(&d.path().canonicalize().unwrap()).unwrap();
    let local = lock.lock().await;
    let grant = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    validate_home_oauth_material(d.path(), &receipt, material.proof(), &[]).unwrap();
    assert_eq!(probe.google_crypto_count(), 0);
    drop(clock);
    let _expired = home_store::oauth_testing::ClockGuard::fixed(now + 1);
    assert!(validate_home_oauth_material(d.path(), &receipt, material.proof(), &[]).is_err());
    assert_eq!(probe.google_crypto_count(), 0);
    assert!(!d.path().join(".keyfile").exists());
    drop(grant);
    drop(local);
}

#[tokio::test]
async fn preseal_uses_cipher_expansion_and_actual_foreign_whitespace_emitter() {
    for oversized_token in [true, false] {
        let d = fixture(true, true);
        if !oversized_token {
            let rows = raw(d.path());
            let google = serde_json::to_string(&rows[0]).unwrap();
            let spaces = " ".repeat(4 * 1024 * 1024 - 10 * 1024);
            let bytes = format!(
                "[{google},{{\"provider_id\":\"github\",{spaces}\"access_token\":\"foreign-token\",\"refresh_token\":null,\"expires_at\":null,\"scopes\":[\"repo\"]}}]"
            );
            std::fs::write(d.path().join(crate::mcp_oauth::TOKEN_FILE), bytes).unwrap();
        }
        let receipt = prepare_home_selection(d.path(), &[]).unwrap();
        let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
        let probe = home_store::test_probe::Guard::install(d.path());
        let wire = refresh_io::testing::Wire::json(
            d.path(),
            Some(
                json!({"access_token":"A".repeat(if oversized_token { 800_000 } else { 20_000 }),"token_type":"Bearer","expires_in":3600}),
            ),
        );
        // Reaching the pre-seal phase proves transport and JSON parsing
        // succeeded; an unrelated I/O failure cannot satisfy capacity checks.
        let parsed = testing::Guard::at(d.path(), 7, || {});
        assert!(
            resolve_home_oauth_pinned(d.path(), &receipt, &[])
                .await
                .is_err()
        );
        assert_eq!(parsed.fired(), 1);
        assert_eq!(wire.requests(), 1);
        assert_eq!(
            probe.google_crypto_count(),
            0,
            "capacity must reject before sealing"
        );
        assert!(!d.path().join(".keyfile").exists());
        assert_eq!(
            std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn configured_broken_service_account_keeps_original_precedence() {
    let d = fixture(false, false);
    std::fs::write(d.path().join("config.toml"), "[integrations]\ngoogle_workspace=true\n[integrations.google_service_account]\nkey_file='missing.json'\n").unwrap();
    assert!(matches!(
        crate::google_workspace::get_valid_google_token(d.path()).await,
        Err(GoogleAuthError::ServiceAccount(_))
    ));
    assert!(raw(d.path())[0].get("google_home").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_during_grant_wait_releases_local_mutex_and_does_not_spawn_work() {
    let d = fixture(true, true);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let held = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    let wire = refresh_io::testing::Wire::json(d.path(), None);
    let mut resolving = Box::pin(resolve_home_oauth_pinned(d.path(), &receipt, &[]));
    std::future::poll_fn(|cx| {
        assert!(resolving.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(resolving);
    let lock = refresh_lock(&d.path().canonicalize().unwrap()).unwrap();
    assert!(lock.try_lock().is_ok());
    drop(held);
    let grant = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    assert_eq!(wire.requests(), 0);
    assert!(!d.path().join(".keyfile").exists());
    drop(grant);
}

#[tokio::test]
async fn precondition_barriers_revocation_master_source_and_scope_refuse_before_crypto() {
    for scenario in [
        "publication",
        "uncertain",
        "revoke",
        "master",
        "duplicate",
        "scope",
    ] {
        let d = fixture(false, false);
        let receipt = prepare_home_selection(d.path(), &["scope-a"]).unwrap();
        let material = resolve_home_oauth_pinned(d.path(), &receipt, &["scope-a"])
            .await
            .unwrap();
        let probe = home_store::test_probe::Guard::install(d.path());
        match scenario {
            "publication" => {
                std::fs::write(d.path().join(".google-home-publication-intent.json"), b"{}")
                    .unwrap()
            }
            "uncertain" => {
                std::fs::create_dir_all(d.path().join("locks")).unwrap();
                std::fs::write(
                    d.path().join("locks/google-grant-revoke-uncertain.json"),
                    b"{}",
                )
                .unwrap();
            }
            "revoke" => crate::mcp_oauth::remove_token(d.path(), "google").unwrap(),
            "master" => std::fs::write(
                d.path().join("config.toml"),
                "[integrations]\ngoogle_workspace=false\n",
            )
            .unwrap(),
            "duplicate" => mutate(d.path(), |r| {
                let copy = r[0].clone();
                r.as_array_mut().unwrap().push(copy);
            }),
            "scope" => {}
            _ => unreachable!(),
        }
        let bytes = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
        let scopes: &[&str] = if scenario == "scope" {
            &["scope-b"]
        } else {
            &["scope-a"]
        };
        assert!(
            resolve_home_oauth_pinned(d.path(), &receipt, scopes)
                .await
                .is_err(),
            "{scenario}"
        );
        assert!(
            validate_home_oauth_material(d.path(), &receipt, material.proof(), scopes).is_err(),
            "{scenario}"
        );
        assert_eq!(probe.google_crypto_count(), 0);
        assert!(!d.path().join(".keyfile").exists());
        assert_eq!(
            std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap(),
            bytes
        );
    }
}

#[tokio::test]
async fn scope_rotation_lands_but_original_receipt_gets_no_material() {
    let d = fixture(true, true);
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let receipt = prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let wire = refresh_io::testing::Wire::json(
        d.path(),
        Some(
            json!({"access_token":"owned-access","token_type":"Bearer","expires_in":3600,"scope":"scope-b"}),
        ),
    );
    assert!(matches!(
        resolve_home_oauth_pinned(d.path(), &receipt, &["scope-a"]).await,
        Err(Error::Stale)
    ));
    assert_eq!(wire.requests(), 1);
    assert_eq!(raw(d.path())[0]["scopes"], json!(["scope-b"]));
    assert!(validate_home_source(d.path(), &receipt).is_err());
}

#[tokio::test]
async fn generic_google_refresh_refuses_before_bad_url_while_github_still_posts() {
    let d = fixture(false, false);
    let wire = refresh_io::testing::Wire::json(d.path(), None);
    let mut cfg = crate::mcp_oauth::McpOAuthConfig {
        provider_id: "google".into(),
        client_id: "fake".into(),
        client_secret: "fake".into(),
        auth_url: "https://example.invalid/".into(),
        token_url: wire.url(),
        redirect_uri: "http://127.0.0.1/".into(),
        scopes: vec!["repo".into()],
    };
    assert!(crate::mcp_oauth::refresh_token(&cfg, "fake").await.is_err());
    assert_eq!(wire.requests(), 0);
    cfg.provider_id = "github".into();
    assert_eq!(
        crate::mcp_oauth::refresh_token(&cfg, "fake")
            .await
            .unwrap()
            .access_token,
        "owned-access"
    );
    assert_eq!(wire.requests(), 1);
}

#[test]
fn sealed_material_proof_and_resolver_future_keep_safe_send_sync() {
    fn send_sync<T: Send + Sync>() {}
    fn future_send<T: std::future::Future + Send>(_: T) {}
    send_sync::<HomeOAuthResolved>();
    send_sync::<HomeOAuthMaterialProof>();
    send_sync::<HomeOAuthCapture>();
    let d = fixture(false, false);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    future_send(resolve_home_oauth_pinned(d.path(), &receipt, &[]));
}

// C1 r2 source-first additions. Every old/new declaration remains UNRUN.
fn encrypted_refresh_and_client_fixture() -> tempfile::TempDir {
    let d = fixture(true, true);
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let refresh = crate::config_crypto::encrypt_value("captured-refresh-A", d.path()).unwrap();
    let secret = crate::config_crypto::encrypt_value("captured-client-secret-B", d.path()).unwrap();
    mutate(d.path(), |r| {
        r[0]["refresh_token"] = json!(format!("enc:v1:{refresh}"))
    });
    let path = d.path().join(crate::mcp_oauth::CLIENT_CONFIG_FILE);
    let mut clients: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    clients[0]["client_secret"] = json!(format!("enc:v1:{secret}"));
    std::fs::write(path, serde_json::to_vec(&clients).unwrap()).unwrap();
    d
}
async fn assert_resolve_locks_released(home: &Path) {
    let lock = refresh_lock(&home.canonicalize().unwrap()).unwrap();
    assert!(lock.try_lock().is_ok());
    let lease = crate::google_personal::grants::acquire(home).await.unwrap();
    drop(lease);
}
#[tokio::test]
async fn nth_encrypted_crypto_boundary_has_exact_reached_crypto_provider_and_no_material_or_publish()
 {
    for (phase, nth, crypto_count, provider_count) in
        [(3, 1, 1, 0), (3, 2, 2, 0), (8, 1, 3, 1), (8, 2, 4, 1)]
    {
        let d = encrypted_refresh_and_client_fixture();
        let receipt = prepare_home_selection(d.path(), &[]).unwrap();
        let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
        let expected = Arc::new(Mutex::new(before));
        let changed = expected.clone();
        let owned = d.path().to_owned();
        let boundary = testing::Guard::at_nth(d.path(), phase, nth, move || {
            if phase == 3 && nth == 2 {
                let path = owned.join(crate::mcp_oauth::CLIENT_CONFIG_FILE);
                let mut clients: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                clients[0]["client_id"] = json!("changed-after-second-client-unseal");
                std::fs::write(path, serde_json::to_vec(&clients).unwrap()).unwrap();
            } else {
                mutate(&owned, |r| {
                    r[0]["access_token"] = json!("changed-at-selected-crypto")
                });
                *changed.lock().unwrap() =
                    std::fs::read(owned.join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
            }
        });
        let publication = testing::Guard::at(d.path(), 10, || {});
        let probe = home_store::test_probe::Guard::install(d.path());
        let wire = refresh_io::testing::Wire::json(d.path(), None);
        let result = resolve_home_oauth_pinned(d.path(), &receipt, &[]).await;
        let materials_returned = usize::from(result.is_ok());
        assert!(matches!(result, Err(Error::Stale)));
        assert_eq!(boundary.visits(), nth);
        assert_eq!(boundary.fired(), 1);
        assert_eq!(probe.google_crypto_count(), crypto_count);
        assert_eq!(wire.requests(), provider_count);
        assert_eq!(materials_returned, 0);
        assert_eq!(publication.fired(), 0);
        assert_eq!(
            std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap(),
            *expected.lock().unwrap()
        );
        assert!(
            !d.path()
                .join(".google-home-publication-intent.json")
                .exists()
        );
        assert_resolve_locks_released(d.path()).await;
    }
}
#[tokio::test]
async fn chunk_error_boundary_mutation_returns_stale_instead_of_transport_error_without_retry() {
    let d = fixture(true, true);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let owned = d.path().to_owned();
    let boundary = testing::Guard::at(d.path(), 13, move || {
        mutate(&owned, |r| {
            r[0]["access_token"] = json!("changed-after-real-chunk-error")
        });
    });
    let publication = testing::Guard::at(d.path(), 10, || {});
    let probe = home_store::test_probe::Guard::install(d.path());
    let wire = refresh_io::testing::Wire::scenario(d.path(), "chunk_error");
    let result = resolve_home_oauth_pinned(d.path(), &receipt, &[]).await;
    let materials_returned = usize::from(result.is_ok());
    assert!(matches!(result, Err(Error::Stale)));
    assert_eq!(boundary.visits(), 1);
    assert_eq!(boundary.fired(), 1);
    assert_eq!(wire.requests(), 1);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(materials_returned, 0);
    assert_eq!(publication.fired(), 0);
    assert_eq!(
        raw(d.path())[0]["access_token"],
        "changed-after-real-chunk-error"
    );
    assert!(!d.path().join(".keyfile").exists());
    assert!(
        !d.path()
            .join(".google-home-publication-intent.json")
            .exists()
    );
    assert_resolve_locks_released(d.path()).await;
}
#[tokio::test]
async fn valid_chunked_body_without_content_length_exceeds_cumulative_bound_before_seal() {
    let d = fixture(true, true);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    let publication = testing::Guard::at(d.path(), 10, || {});
    let probe = home_store::test_probe::Guard::install(d.path());
    let wire = refresh_io::testing::Wire::scenario(d.path(), "chunked_overflow");
    let result = resolve_home_oauth_pinned(d.path(), &receipt, &[]).await;
    let materials_returned = usize::from(result.is_ok());
    assert!(matches!(result, Err(Error::Invalid)));
    assert_eq!(wire.requests(), 1);
    assert!(wire.chunked_without_content_length());
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while !wire.chunked_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(wire.chunked_body_written() > 4 * 1024 * 1024);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(materials_returned, 0);
    assert_eq!(publication.fired(), 0);
    assert_eq!(
        std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap(),
        before
    );
    assert!(!d.path().join(".keyfile").exists());
    assert!(
        !d.path()
            .join(".google-home-publication-intent.json")
            .exists()
    );
    assert_resolve_locks_released(d.path()).await;
}
#[tokio::test]
async fn actual_twenty_second_reqwest_timeout_has_one_attempt_and_no_material_or_publication() {
    let d = fixture(true, true);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    let publication = testing::Guard::at(d.path(), 10, || {});
    let probe = home_store::test_probe::Guard::install(d.path());
    let wire = refresh_io::testing::Wire::scenario(d.path(), "timeout");
    let started = std::time::Instant::now();
    let result = resolve_home_oauth_pinned(d.path(), &receipt, &[]).await;
    let elapsed = started.elapsed();
    let materials_returned = usize::from(result.is_ok());
    assert!(matches!(result, Err(Error::Io)));
    assert!(elapsed >= std::time::Duration::from_secs(19));
    assert!(elapsed < std::time::Duration::from_secs(25));
    assert_eq!(wire.requests(), 1);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(materials_returned, 0);
    assert_eq!(publication.fired(), 0);
    assert_eq!(
        std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap(),
        before
    );
    assert!(!d.path().join(".keyfile").exists());
    assert!(
        !d.path()
            .join(".google-home-publication-intent.json")
            .exists()
    );
    assert_resolve_locks_released(d.path()).await;
}

#[tokio::test]
async fn cancellation_after_second_encrypted_unseal_drops_pending_send_and_both_leases() {
    let d = encrypted_refresh_and_client_fixture();
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    let second_unseal = testing::Guard::at_nth(d.path(), 3, 2, || {});
    let publication = testing::Guard::at(d.path(), 10, || {});
    let probe = home_store::test_probe::Guard::install(d.path());
    let wire = refresh_io::testing::Wire::scenario(d.path(), "timeout");
    let returned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut resolving = Box::pin(resolve_home_oauth_pinned(d.path(), &receipt, &[]));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::select! {
            result = resolving.as_mut() => {
                returned.store(usize::from(result.is_ok()), std::sync::atomic::Ordering::SeqCst);
                panic!("owned pending-response cancellation fixture completed before cancellation");
            },
            _ = async { while wire.requests() == 0 { tokio::task::yield_now().await; } } => {},
        }
    })
    .await
    .unwrap();
    assert_eq!(second_unseal.visits(), 2);
    assert_eq!(second_unseal.fired(), 1);
    assert_eq!(probe.google_crypto_count(), 2);
    assert_eq!(wire.requests(), 1);
    drop(resolving);
    assert_eq!(returned.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(publication.fired(), 0);
    assert_eq!(
        std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap(),
        before
    );
    assert!(
        !d.path()
            .join(".google-home-publication-intent.json")
            .exists()
    );
    assert_resolve_locks_released(d.path()).await;
    assert_eq!(wire.requests(), 1);
    assert_eq!(probe.google_crypto_count(), 2);
}
