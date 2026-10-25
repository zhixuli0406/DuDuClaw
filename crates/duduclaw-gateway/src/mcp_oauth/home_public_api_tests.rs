//! W2b-A SOURCE-FIRST public API/raw byte tests; NOT COMPILED OR RUN.
use super::*;
use crate::mcp_oauth as api;
use crate::mcp_oauth::home_store::publication::{Fault, FaultGuard, GATE_FILE};
use crate::mcp_oauth::home_store::raw_array::RawArray;
fn raw_element(home: &Path, file: &str, provider: &str) -> Vec<u8> {
    RawArray::read(&home.join(file))
        .unwrap()
        .element(provider)
        .unwrap()
        .to_vec()
}
fn other_token(provider: &str) -> api::McpOAuthToken {
    api::McpOAuthToken {
        provider_id: provider.into(),
        access_token: String::new(),
        refresh_token: None,
        expires_at: None,
        scopes: vec![],
    }
}
fn other_client(provider: &str) -> api::McpOAuthClientConfig {
    api::McpOAuthClientConfig {
        provider_id: provider.into(),
        client_id: "fixture-id".into(),
        client_secret: String::new(),
        auth_url: "https://example.test/auth".into(),
        token_url: "https://example.test/token".into(),
        redirect_uri: "http://127.0.0.1/callback".into(),
        scopes: vec![],
    }
}
#[test]
fn public_non_google_token_save_upsert_remove_preserve_opaque_google_and_target_unknowns() {
    let d = fixture();
    let _t = ticket(d.path());
    let path = d.path().join(api::TOKEN_FILE);
    let raw = std::fs::read_to_string(&path).unwrap().replace(
        "\"provider_id\": \"google\"",
        "\"provider_id\" : \"google\"",
    );
    std::fs::write(&path, raw).unwrap();
    let google = raw_element(d.path(), api::TOKEN_FILE, "google");
    let probe = test_probe::Guard::install(d.path());
    api::upsert_token(d.path(), other_token("github")).unwrap();
    let doc = RawArray::read(&path).unwrap();
    assert_eq!(doc.values()[0]["future_field"], json!({"retained":true}));
    api::save_tokens(d.path(), &[other_token("github"), other_token("notion")]).unwrap();
    assert_eq!(raw_element(d.path(), api::TOKEN_FILE, "google"), google);
    api::remove_token(d.path(), "notion").unwrap();
    api::save_tokens(d.path(), &[]).unwrap();
    assert_eq!(raw_element(d.path(), api::TOKEN_FILE, "google"), google);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(probe.writer_counts(), [0, 0]);
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn token_target_getters_do_not_unseal_google_and_explicit_all_load_remains_explicit() {
    let d = fixture();
    let probe = test_probe::Guard::install(d.path());
    assert!(api::get_token(d.path(), "github").is_some());
    assert!(api::get_stored_token(d.path(), "github").is_some());
    assert_eq!(probe.google_crypto_count(), 0);
    // Deliberately unusable Google ciphertext is never returned as a bearer.
    assert!(api::get_token(d.path(), "google").is_none());
    assert!(probe.google_crypto_count() > 0);
}
#[test]
fn public_non_google_client_writes_preserve_google_raw_and_every_untouched_row() {
    let d = fixture();
    let file = api::CLIENT_CONFIG_FILE;
    let path = d.path().join(file);
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path,original.trim_end_matches(']').to_owned()+r#", {"provider_id":"github","client_id":"old","client_secret":"","auth_url":"https://example.test/a","token_url":"https://example.test/t","redirect_uri":"http://127.0.0.1/c","scopes":[],"unknown":{"nested":["quote\"","backslash\\","台灣",null]}} ]"#).unwrap();
    let google = raw_element(d.path(), file, "google");
    let token = bytes(d.path());
    let probe = test_probe::Guard::install(d.path());
    api::upsert_client_config(d.path(), other_client("github")).unwrap();
    let raw = RawArray::read(&path).unwrap();
    assert!(raw.values()[1].get("unknown").is_some());
    api::upsert_client_config(d.path(), other_client("notion")).unwrap();
    api::remove_client_config(d.path(), "notion").unwrap();
    assert_eq!(raw_element(d.path(), file, "google"), google);
    assert_eq!(bytes(d.path()), token);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(probe.writer_counts()[0], 0);
}
#[test]
fn saved_google_client_without_home_token_or_master_remains_personal_compatible() {
    let d = fixture();
    std::fs::remove_file(d.path().join(api::TOKEN_FILE)).unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    let path = d.path().join(api::CLIENT_CONFIG_FILE);
    let raw = std::fs::read_to_string(&path)
        .unwrap()
        .replace("enc:v1:unusable-client", "owned-legacy-client-secret");
    std::fs::write(&path, raw).unwrap();
    let probe = test_probe::Guard::install(d.path());
    assert!(api::has_client_config(d.path(), "google"));
    assert_eq!(probe.counts(), [0, 0]);
    let client = api::get_client_config(d.path(), "google").unwrap();
    assert_eq!(client.client_secret, "owned-legacy-client-secret");
    assert!(!d.path().join(api::TOKEN_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
    // This is the public client seam used by Personal flow.start/access;
    // it does not claim a complete Personal provider/Keychain flow execution.
}
#[test]
fn shared_gate_hides_google_clients_but_allows_non_google_reads_and_refuses_all_writes() {
    let d = fixture();
    api::upsert_client_config(d.path(), other_client("github")).unwrap();
    let tokens = bytes(d.path());
    let clients = std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    std::fs::write(d.path().join(GATE_FILE), b"owned-unresolved-gate").unwrap();
    let probe = test_probe::Guard::install(d.path());
    assert!(!api::has_client_config(d.path(), "google"));
    assert!(api::get_client_config(d.path(), "google").is_none());
    assert!(api::get_client_config(d.path(), "github").is_some());
    assert!(api::get_token(d.path(), "github").is_some());
    assert!(api::upsert_client_config(d.path(), other_client("github")).is_err());
    assert!(api::save_tokens(d.path(), &[]).is_err());
    assert!(api::upsert_token(d.path(), other_token("other")).is_err());
    assert_eq!(bytes(d.path()), tokens);
    assert_eq!(
        std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap(),
        clients
    );
    assert_eq!(probe.google_crypto_count(), 0);
}
#[test]
fn raw_ranges_handle_escaping_nesting_unicode_and_every_removal_position() {
    let rows=br#" [ {"provider_id":"first","x":[{"q":"\"\\"},null]}, {"provider_id":"google","x":"\u53f0\u7063","unknown":null}, {"provider_id":"last","nested":[[],{}]} ] "#;
    let original = RawArray::from_bytes(Some(rows.to_vec())).unwrap();
    for provider in ["first", "google", "last"] {
        let mut next = original.values().to_vec();
        next.retain(|r| r["provider_id"] != provider);
        let output = original.render(&next).unwrap();
        let reread = RawArray::from_bytes(Some(output.to_vec())).unwrap();
        assert_eq!(reread.values(), next);
        for retained in ["first", "google", "last"] {
            if retained != provider {
                assert_eq!(original.element(retained), reread.element(retained));
            }
        }
    }
    let sole = RawArray::from_bytes(Some(br#"[{"provider_id":"only"}]"#.to_vec())).unwrap();
    assert_eq!(sole.render(&[]).unwrap().as_slice(), b"[\n]\n");
}
#[test]
fn raw_array_rejects_malformed_trailing_duplicate_unsafe_and_excessive_documents() {
    for raw in [
        b"[".as_slice(),
        b"[] trailing",
        b"[null]",
        br#"[{"provider_id":"google","provider_id":"other"}]"#,
        b"[{\"provider_id\":\"\"}]",
        b"[{\"provider_id\":\"g\"},{\"provider_id\":\"g\"}]",
        b"[{},]",
        b"[\xff]",
    ] {
        assert!(RawArray::from_bytes(Some(raw.to_vec())).is_err());
    }
    let rows: Vec<Value> = (0..513)
        .map(|n| json!({"provider_id":format!("p{n}")}))
        .collect();
    assert!(RawArray::from_bytes(Some(serde_json::to_vec(&rows).unwrap())).is_err());
    assert!(RawArray::from_bytes(Some(vec![b' '; 4 * 1024 * 1024 + 1])).is_err());
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("unsafe.json");
    std::fs::create_dir(&path).unwrap();
    assert!(RawArray::read(&path).is_err());
    #[cfg(unix)]
    {
        let link = d.path().join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(RawArray::read(&link).is_err());
    }
}
#[test]
fn non_google_transaction_raw_cas_rejects_semantically_equal_external_rewrite() {
    let d = fixture();
    let before = bytes(d.path());
    let err = home_source::with_source_guard(d.path(), |g| {
        transaction(g, |r| {
            let mut external = before.clone();
            external.push(b' ');
            std::fs::write(d.path().join(api::TOKEN_FILE), &external).unwrap();
            r.rows
                .push(serde_json::to_value(other_token("other")).unwrap());
            r.dirty = true;
            Ok(())
        })
    });
    assert!(matches!(err, Err(Error::Stale)));
    let mut external = before;
    external.push(b' ');
    assert_eq!(bytes(d.path()), external);
}
#[test]
fn static_invalid_or_combined_limits_refuse_before_crypto_and_preserve_files() {
    let d = fixture();
    let probe = test_probe::Guard::install(d.path());
    let before = bytes(d.path());
    let mut bad = other_token("other");
    bad.access_token = "target-secret".into();
    assert!(api::save_tokens(d.path(), &[bad.clone(), bad.clone()]).is_err());
    let mut google = bad.clone();
    google.provider_id = "google".into();
    assert!(api::upsert_token(d.path(), google.clone()).is_err());
    assert!(api::save_tokens(d.path(), &[google]).is_err());
    let mut client = other_client("google");
    client.client_secret = "target-secret".into();
    client.scopes = vec!["".into()];
    assert!(api::upsert_client_config(d.path(), client).is_err());
    let many: Vec<_> = (0..512).map(|n| other_token(&format!("p{n}"))).collect();
    assert!(api::save_tokens(d.path(), &many).is_err());
    // Each encrypted token fits the per-field cap; only their combined
    // document exceeds 4 MiB. This must fail before touching the key.
    let huge: Vec<_> = (0..5)
        .map(|n| {
            let mut token = other_token(&format!("large-{n}"));
            token.access_token = "x".repeat(700_000);
            token
        })
        .collect();
    assert!(api::save_tokens(d.path(), &huge).is_err());
    assert_eq!(probe.counts(), [0, 0]);
    assert_eq!(bytes(d.path()), before);
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn cold_client_metadata_noop_and_unrelated_empty_target_never_create_google_control_or_key() {
    let d = tempfile::tempdir().unwrap();
    let probe = test_probe::Guard::install(d.path());
    assert!(!api::has_client_config(d.path(), "google"));
    assert!(api::get_client_config(d.path(), "google").is_none());
    api::remove_client_config(d.path(), "google").unwrap();
    api::upsert_client_config(d.path(), other_client("github")).unwrap();
    assert!(api::get_client_config(d.path(), "github").is_some());
    assert_eq!(probe.google_crypto_count(), 0);
    assert!(!d.path().join(api::TOKEN_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
    assert!(!d.path().join(GATE_FILE).exists());
}
#[test]
fn google_client_public_remove_and_upsert_fence_old_ticket_and_receipt() {
    let d = fixture();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let t = ticket(d.path());
    let old = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let raw = RawArray::read(&d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    let mut client: api::McpOAuthClientConfig =
        serde_json::from_value(raw.values()[0].clone()).unwrap();
    client.client_secret.clear();
    api::upsert_client_config(d.path(), client).unwrap();
    assert!(home_source::validate_home_source(d.path(), &old).is_err());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&t, s, NOW)))
        .is_err()
    );
    api::remove_client_config(d.path(), "google").unwrap();
    assert!(!api::has_client_config(d.path(), "google"));
    assert!(
        api::get_stored_token(d.path(), "google").is_none(),
        "fixture ciphertext is unusable, not a token string"
    );
}
#[test]
fn google_client_payload_faults_leave_barrier_and_no_ready_selection() {
    for f in [
        Fault::SourceOpen,
        Fault::SourceWrite,
        Fault::SourceFileSync,
        Fault::SourceRename,
        Fault::SourceDirSync,
        Fault::CleanupDirSync,
    ] {
        let d = fixture();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let t = ticket(d.path());
        let guard = FaultGuard::at(d.path(), f);
        assert!(api::remove_client_config(d.path(), "google").is_err());
        drop(guard);
        assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
        assert!(
            run(d.path(), |g, s| publication_transaction(g, |r| r
                .claim(&t, s, NOW)))
            .is_err()
        );
    }
}
#[test]
fn target_crypto_can_run_without_any_google_side_read_and_is_not_fake_zero_crypto() {
    let d = fixture();
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let g = raw_element(d.path(), api::TOKEN_FILE, "google");
    let probe = test_probe::Guard::install(d.path());
    let mut token = other_token("other");
    token.access_token = "owned-target-secret".into();
    api::upsert_token(d.path(), token).unwrap();
    assert_eq!(
        api::get_token(d.path(), "other").unwrap().access_token,
        "owned-target-secret"
    );
    assert!(probe.counts()[0] > 0);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(raw_element(d.path(), api::TOKEN_FILE, "google"), g);
}

#[test]
fn actual_public_remove_covers_first_middle_last_and_keeps_all_retained_row_bytes() {
    for target in ["first", "middle", "last"] {
        let d = fixture();
        let path = d.path().join(api::TOKEN_FILE);
        let google = raw_element(d.path(), api::TOKEN_FILE, "google");
        let mut source = b"[".to_vec();
        source.extend_from_slice(&google);
        for p in ["first", "middle", "last"] {
            source.push(b',');
            source.extend_from_slice(format!(r#"{{ "provider_id":"{p}","access_token":"","refresh_token":null,"expires_at":null,"scopes":[],"unknown":{{"kept":"台灣"}} }}"#).as_bytes());
        }
        source.push(b']');
        std::fs::write(&path, source).unwrap();
        let before = RawArray::read(&path).unwrap();
        api::remove_token(d.path(), target).unwrap();
        let after = RawArray::read(&path).unwrap();
        for p in ["google", "first", "middle", "last"] {
            if p != target {
                assert_eq!(before.element(p), after.element(p));
            }
        }
    }
}
#[test]
fn actual_target_read_and_write_recheck_byte_cas_after_real_crypto() {
    for read in [true, false] {
        let d = fixture();
        std::fs::write(d.path().join(".keyfile"), [9u8; 32]).unwrap();
        let mut token = other_token("other");
        token.access_token = "owned-target-secret".into();
        api::upsert_token(d.path(), token.clone()).unwrap();
        let path = d.path().join(api::TOKEN_FILE);
        let before = std::fs::read(&path).unwrap();
        let changed = path.clone();
        let _boundary = test_probe::BoundaryGuard::at(d.path(), move || {
            let mut bytes = std::fs::read(&changed).unwrap();
            bytes.push(b' ');
            std::fs::write(&changed, bytes).unwrap();
        });
        if read {
            assert!(api::get_token(d.path(), "other").is_none());
        } else {
            token.access_token = "replacement-secret".into();
            assert!(api::upsert_token(d.path(), token).is_err());
        }
        let mut winner = before;
        winner.push(b' ');
        assert_eq!(std::fs::read(path).unwrap(), winner);
    }
}
#[test]
fn client_write_rechecks_token_bytes_not_just_equal_google_metadata() {
    let d = fixture();
    let clients = std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    let raw = RawArray::read(&d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    let mut client: api::McpOAuthClientConfig =
        serde_json::from_value(raw.values()[0].clone()).unwrap();
    client.client_secret.clear();
    let path = d.path().join(api::TOKEN_FILE);
    let _boundary = test_probe::BoundaryGuard::at(d.path(), move || {
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(b' ');
        std::fs::write(path, bytes).unwrap();
    });
    let probe = test_probe::Guard::install(d.path());
    assert!(api::upsert_client_config(d.path(), client).is_err());
    assert_eq!(probe.writer_counts()[0], 0);
    assert_eq!(
        std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap(),
        clients
    );
}
#[test]
fn non_google_token_durability_failures_preserve_google_bytes_without_claiming_success() {
    for fault in [
        Fault::DataOpen,
        Fault::DataWrite,
        Fault::DataFileSync,
        Fault::DataRename,
        Fault::DataDirSync,
    ] {
        let d = fixture();
        let _receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
        let g = raw_element(d.path(), api::TOKEN_FILE, "google");
        let f = FaultGuard::at(d.path(), fault);
        assert!(api::upsert_token(d.path(), other_token("other")).is_err());
        drop(f);
        assert_eq!(raw_element(d.path(), api::TOKEN_FILE, "google"), g);
        // Unrelated data uncertainty is the existing Before/AfterRename error,
        // not a new Google publication kind or an invented success receipt.
    }
}

#[test]
fn actual_public_google_client_first_save_is_available_without_home_oauth_credential() {
    let d = tempfile::tempdir().unwrap();
    let probe = test_probe::Guard::install(d.path());
    let mut client = other_client("google");
    client.scopes = vec!["scope-a".into()];
    api::upsert_client_config(d.path(), client).unwrap();
    assert!(api::has_client_config(d.path(), "google"));
    assert!(api::get_client_config(d.path(), "google").is_some());
    let raw: Value =
        serde_json::from_slice(&std::fs::read(d.path().join(api::TOKEN_FILE)).unwrap()).unwrap();
    assert_eq!(raw[0]["google_home"]["credential"], Value::Null);
    assert!(api::get_stored_token(d.path(), "google").is_none());
    assert_eq!(probe.google_crypto_count(), 0);
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn explicit_all_views_hide_google_during_gate_and_target_metadata_never_decrypts() {
    let d = fixture();
    api::upsert_client_config(d.path(), other_client("github")).unwrap();
    let probe = test_probe::Guard::install(d.path());
    assert!(api::has_client_config(d.path(), "google"));
    assert_eq!(probe.google_crypto_count(), 0);
    std::fs::write(d.path().join(GATE_FILE), b"owned-uncertain-gate").unwrap();
    assert!(
        api::load_client_configs(d.path())
            .iter()
            .all(|v| v.provider_id != "google")
    );
    assert!(
        api::load_tokens(d.path())
            .iter()
            .all(|v| v.provider_id != "google")
    );
    assert_eq!(probe.google_crypto_count(), 0);
}

#[test]
fn target_client_read_revalidates_same_owned_bytes_after_actual_crypto() {
    let d = fixture();
    std::fs::write(d.path().join(".keyfile"), [3u8; 32]).unwrap();
    let mut client = other_client("github");
    client.client_secret = "owned-target-client-secret".into();
    api::upsert_client_config(d.path(), client).unwrap();
    let path = d.path().join(api::CLIENT_CONFIG_FILE);
    let changed = path.clone();
    let _boundary = test_probe::BoundaryGuard::at(d.path(), move || {
        let mut raw = std::fs::read(&changed).unwrap();
        raw.push(b' ');
        std::fs::write(changed, raw).unwrap();
    });
    let probe = test_probe::Guard::install(d.path());
    assert!(api::get_client_config(d.path(), "github").is_none());
    assert!(probe.counts()[0] > 0);
    assert_eq!(probe.google_crypto_count(), 0);
}
#[test]
fn unrelated_client_target_preserves_even_unusable_google_metadata_without_home_side_effects() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join(api::CLIENT_CONFIG_FILE);
    std::fs::write(
        &path,
        br#"[ {"provider_id":"google","client_secret":"enc:v1:unusable","future":{"raw":true}} ]"#,
    )
    .unwrap();
    let g = raw_element(d.path(), api::CLIENT_CONFIG_FILE, "google");
    let probe = test_probe::Guard::install(d.path());
    api::upsert_client_config(d.path(), other_client("github")).unwrap();
    assert!(api::get_client_config(d.path(), "github").is_some());
    assert_eq!(raw_element(d.path(), api::CLIENT_CONFIG_FILE, "google"), g);
    assert_eq!(probe.google_crypto_count(), 0);
    assert!(!d.path().join(api::TOKEN_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn public_client_duplicate_and_combined_limit_refuse_before_target_secret_crypto() {
    for duplicate in [true, false] {
        let d = fixture();
        let path = d.path().join(api::CLIENT_CONFIG_FILE);
        let original = RawArray::read(&path).unwrap();
        let mut rows = original.values().to_vec();
        if duplicate {
            rows.push(rows[0].clone());
        } else {
            for n in 0..511 {
                rows.push(serde_json::to_value(other_client(&format!("p{n}"))).unwrap());
            }
        }
        std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let probe = test_probe::Guard::install(d.path());
        let mut target = other_client("new-target");
        target.client_secret = "must-not-touch-key".into();
        assert!(api::upsert_client_config(d.path(), target).is_err());
        assert_eq!(probe.counts(), [0, 0]);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!d.path().join(".keyfile").exists());
    }
}
#[test]
fn intentional_same_google_client_update_and_client_aba_fence_previous_authorities() {
    let d = fixture();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let raw = RawArray::read(&d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    let mut a: api::McpOAuthClientConfig = serde_json::from_value(raw.values()[0].clone()).unwrap();
    a.client_secret.clear();
    api::upsert_client_config(d.path(), a.clone()).unwrap();
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    api::upsert_client_config(d.path(), a.clone()).unwrap();
    assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
    let before_aba = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let mut b = a.clone();
    b.client_id.push_str("-new");
    api::upsert_client_config(d.path(), b).unwrap();
    api::upsert_client_config(d.path(), a).unwrap();
    assert!(home_source::validate_home_source(d.path(), &before_aba).is_err());
}

#[test]
fn client_combined_byte_limit_includes_opaque_unknown_fields_before_crypto() {
    let d = fixture();
    let path = d.path().join(api::CLIENT_CONFIG_FILE);
    let mut rows = RawArray::read(&path).unwrap().values().to_vec();
    let mut retained = serde_json::to_value(other_client("retained")).unwrap();
    retained["unknown_blob"] = json!("u".repeat(raw_array::MAX_BYTES - 2000));
    rows.push(retained);
    let raw = serde_json::to_vec(&rows).unwrap();
    assert!(raw.len() < raw_array::MAX_BYTES);
    std::fs::write(&path, &raw).unwrap();
    let probe = test_probe::Guard::install(d.path());
    let mut target = other_client("new-target");
    target.client_secret = "s".repeat(2000);
    assert!(api::upsert_client_config(d.path(), target).is_err());
    assert_eq!(probe.counts(), [0, 0]);
    assert_eq!(std::fs::read(path).unwrap(), raw);
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn client_fence_cannot_use_unit_producer_or_wrong_operation() {
    for wrong_op in [false, true] {
        let d = fixture();
        let token = bytes(d.path());
        let clients = std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
        assert!(
            run(d.path(), |g, source| {
                let doc = RawArray::read(&g.home().join(api::CLIENT_CONFIG_FILE))?;
                let mut rows = doc.values().to_vec();
                rows[0]["client_id"] = json!("different-client");
                let target = source.preview_google_client(&rows[0])?;
                let write = prepare_client_write(doc.bytes(), doc.render(&rows)?.to_vec())?;
                publication_transaction(g, |r| {
                    let mut made = r.fence_source_change(source, target, Some(write))?;
                    if wrong_op {
                        made.tamper_for_test("operation");
                    }
                    Ok(made.unit_for_test())
                })
            })
            .is_err()
        );
        assert_eq!(bytes(d.path()), token);
        assert_eq!(
            std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap(),
            clients
        );
        assert!(!d.path().join(GATE_FILE).exists());
    }
}

#[path = "home_public_api_r2_tests.rs"]
mod review_r2_tests;
