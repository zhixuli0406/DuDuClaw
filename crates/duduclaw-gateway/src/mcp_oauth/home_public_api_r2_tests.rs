//! W2b-A r2 source-first review regressions. NOT COMPILED OR RUN.
use super::*;
fn plain_google_fixture() -> tempfile::TempDir {
    let d = fixture();
    let path = d.path().join(api::TOKEN_FILE);
    let mut rows: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    rows[0]["access_token"] = json!("");
    rows[1]["access_token"] = json!("owned-access");
    rows[1]["refresh_token"] = json!("owned-refresh");
    std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
    let path = d.path().join(api::CLIENT_CONFIG_FILE);
    let mut clients: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    clients[0]["client_secret"] = json!("owned-client");
    std::fs::write(&path, serde_json::to_vec(&clients).unwrap()).unwrap();
    api::upsert_client_config(d.path(), other_client("github")).unwrap();
    d
}
#[test]
fn real_public_google_unknown_prefixes_refuse_access_refresh_and_client_material() {
    for unknown in [
        "enc:v2:unsupported",
        "enc:badversion",
        "enc:v1x:cipher",
        "enc:",
    ] {
        for field in ["access_token", "refresh_token", "client_secret"] {
            let d = plain_google_fixture();
            let file = if field == "client_secret" {
                api::CLIENT_CONFIG_FILE
            } else {
                api::TOKEN_FILE
            };
            let path = d.path().join(file);
            let mut rows: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let index = if field == "client_secret" { 0 } else { 1 };
            rows[index][field] = json!(unknown);
            std::fs::write(path, serde_json::to_vec(&rows).unwrap()).unwrap();
            let token = bytes(d.path());
            let client = std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
            let probe = test_probe::Guard::install(d.path());
            if field == "client_secret" {
                assert!(api::get_client_config(d.path(), "google").is_none());
                assert!(
                    api::load_client_configs(d.path())
                        .iter()
                        .all(|v| v.provider_id != "google")
                );
            } else {
                assert!(api::get_token(d.path(), "google").is_none());
                assert!(api::get_stored_token(d.path(), "google").is_none());
                assert!(
                    api::load_tokens(d.path())
                        .iter()
                        .all(|v| v.provider_id != "google")
                );
            }
            assert!(api::get_token(d.path(), "github").is_some());
            assert!(api::get_client_config(d.path(), "github").is_some());
            assert_eq!(probe.google_crypto_count(), 0);
            assert_eq!(probe.counts()[1], 0);
            assert_eq!(probe.writer_counts(), [0, 0]);
            assert_eq!(bytes(d.path()), token);
            assert_eq!(
                std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap(),
                client
            );
            assert!(!d.path().join(".keyfile").exists());
        }
    }
}
#[test]
fn real_public_google_plain_known_bad_v1_and_real_v1_contracts_remain_distinct() {
    for mode in ["plain", "bad-v1", "real-v1"] {
        let d = plain_google_fixture();
        let encode = |text: &str| -> String {
            match mode {
                "plain" => text.into(),
                "bad-v1" => "enc:v1:unusable".into(),
                _ => {
                    std::fs::write(d.path().join(".keyfile"), [17u8; 32]).unwrap();
                    format!(
                        "enc:v1:{}",
                        crate::config_crypto::encrypt_value(text, d.path()).unwrap()
                    )
                }
            }
        };
        let path = d.path().join(api::TOKEN_FILE);
        let mut rows: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        rows[1]["access_token"] = json!(encode("owned-access"));
        rows[1]["refresh_token"] = json!(encode("owned-refresh"));
        std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
        let path = d.path().join(api::CLIENT_CONFIG_FILE);
        let mut clients: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        clients[0]["client_secret"] = json!(encode("owned-client"));
        std::fs::write(&path, serde_json::to_vec(&clients).unwrap()).unwrap();
        let token = bytes(d.path());
        let client = std::fs::read(&path).unwrap();
        let probe = test_probe::Guard::install(d.path());
        if mode == "bad-v1" {
            assert!(api::get_token(d.path(), "google").is_none());
            assert!(api::get_stored_token(d.path(), "google").is_none());
            assert!(api::get_client_config(d.path(), "google").is_none());
            assert!(
                api::load_tokens(d.path())
                    .iter()
                    .all(|v| v.provider_id != "google")
            );
            assert!(
                api::load_client_configs(d.path())
                    .iter()
                    .all(|v| v.provider_id != "google")
            );
        } else {
            assert_eq!(
                api::get_token(d.path(), "google").unwrap().access_token,
                "owned-access"
            );
            assert_eq!(
                api::get_stored_token(d.path(), "google")
                    .unwrap()
                    .refresh_token
                    .as_deref(),
                Some("owned-refresh")
            );
            assert_eq!(
                api::get_client_config(d.path(), "google")
                    .unwrap()
                    .client_secret,
                "owned-client"
            );
            assert!(
                api::load_tokens(d.path())
                    .iter()
                    .any(|v| v.provider_id == "google")
            );
            assert!(
                api::load_client_configs(d.path())
                    .iter()
                    .any(|v| v.provider_id == "google")
            );
        }
        if mode == "plain" {
            assert_eq!(probe.google_crypto_count(), 0);
        } else {
            assert!(probe.google_crypto_count() > 0);
        }
        assert_eq!(probe.counts()[1], 0);
        assert_eq!(bytes(d.path()), token);
        assert_eq!(std::fs::read(path).unwrap(), client);
    }
}
#[test]
fn real_public_non_google_unknown_prefix_preserves_legacy_fallback() {
    let d = plain_google_fixture();
    let unknown = "enc:v2:legacy-other-provider";
    let path = d.path().join(api::TOKEN_FILE);
    let mut rows: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    rows[0]["access_token"] = json!(unknown);
    rows[0]["refresh_token"] = json!(unknown);
    std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
    let path = d.path().join(api::CLIENT_CONFIG_FILE);
    let mut clients: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    clients[1]["client_secret"] = json!(unknown);
    std::fs::write(&path, serde_json::to_vec(&clients).unwrap()).unwrap();
    let token = bytes(d.path());
    let client = std::fs::read(&path).unwrap();
    let probe = test_probe::Guard::install(d.path());
    assert_eq!(
        api::get_token(d.path(), "github").unwrap().access_token,
        unknown
    );
    assert_eq!(
        api::get_stored_token(d.path(), "github")
            .unwrap()
            .refresh_token
            .as_deref(),
        Some(unknown)
    );
    assert_eq!(
        api::get_client_config(d.path(), "github")
            .unwrap()
            .client_secret,
        unknown
    );
    assert!(
        api::load_tokens(d.path())
            .iter()
            .any(|v| v.provider_id == "google")
    );
    assert!(
        api::load_client_configs(d.path())
            .iter()
            .any(|v| v.provider_id == "google")
    );
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(bytes(d.path()), token);
    assert_eq!(std::fs::read(path).unwrap(), client);
    assert!(!d.path().join(".keyfile").exists());
}
fn token_rows(count: usize) -> Vec<u8> {
    serde_json::to_vec(
        &(0..count)
            .map(|n| serde_json::to_value(other_token(&format!("p{n}"))).unwrap())
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
fn internal_whitespace_near_cap() -> Vec<u8> {
    let mut bytes=br#"[{"provider_id":"retained","access_token":"","refresh_token":null,"expires_at":null,"scopes":[],"unknown":["#.to_vec();
    bytes.resize(raw_array::MAX_BYTES - 68, b' ');
    bytes.extend_from_slice(b"]}]");
    let parsed: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert!(serde_json::to_vec_pretty(&parsed).unwrap().len() < 1024);
    assert!(bytes.len() > raw_array::MAX_BYTES - 128);
    bytes
}
fn capacity_home(tokens: &[u8]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    std::fs::write(d.path().join(api::TOKEN_FILE), tokens).unwrap();
    d
}
fn secret_google_client() -> api::McpOAuthClientConfig {
    let mut c = other_client("google");
    c.client_secret = "owned-nonempty-client-secret".into();
    c.scopes = vec!["scope-a".into()];
    c
}
#[test]
fn actual_first_google_client_rejects_token_row_capacity_before_crypto() {
    let raw = token_rows(512);
    let d = capacity_home(&raw);
    let probe = test_probe::Guard::install(d.path());
    assert!(api::upsert_client_config(d.path(), secret_google_client()).is_err());
    assert_eq!(probe.counts(), [0, 0]);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(probe.writer_counts(), [0, 0]);
    assert_eq!(bytes(d.path()), raw);
    assert!(!d.path().join(api::CLIENT_CONFIG_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
    assert!(!d.path().join(GATE_FILE).exists());
}
#[test]
fn actual_first_google_client_rejects_retained_internal_whitespace_growth_before_crypto() {
    let raw = internal_whitespace_near_cap();
    let d = capacity_home(&raw);
    let probe = test_probe::Guard::install(d.path());
    assert!(api::upsert_client_config(d.path(), secret_google_client()).is_err());
    assert_eq!(probe.counts(), [0, 0]);
    assert_eq!(probe.google_crypto_count(), 0);
    assert_eq!(probe.writer_counts(), [0, 0]);
    assert_eq!(bytes(d.path()), raw);
    assert!(!d.path().join(api::CLIENT_CONFIG_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
    assert!(!d.path().join(GATE_FILE).exists());
}
#[test]
fn actual_google_revoke_checks_real_emitted_raw_capacity_before_gate() {
    let raw = internal_whitespace_near_cap();
    let d = capacity_home(&raw);
    let probe = test_probe::Guard::install(d.path());
    // No client-writer projection hides this case: the real sealed revoke
    // publication itself must render its exact future token bytes pre-gate.
    assert!(api::remove_token(d.path(), "google").is_err());
    assert_eq!(bytes(d.path()), raw);
    assert_eq!(probe.counts(), [0, 0]);
    assert_eq!(probe.writer_counts(), [0, 0]);
    assert!(!d.path().join(GATE_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn capacity_changed_after_real_client_crypto_rejects_post_cas_without_gate() {
    let d = capacity_home(b"[]");
    std::fs::write(d.path().join(".keyfile"), [23u8; 32]).unwrap();
    let path = d.path().join(api::TOKEN_FILE);
    let winner = token_rows(512);
    let changed = winner.clone();
    let _boundary =
        test_probe::BoundaryGuard::at(d.path(), move || std::fs::write(path, changed).unwrap());
    let probe = test_probe::Guard::install(d.path());
    assert!(api::upsert_client_config(d.path(), secret_google_client()).is_err());
    assert!(probe.google_crypto_count() > 0);
    assert!(probe.counts()[0] > 0);
    assert_eq!(probe.counts()[1], 0);
    assert_eq!(probe.writer_counts(), [0, 0]);
    assert_eq!(bytes(d.path()), winner);
    assert!(!d.path().join(api::CLIENT_CONFIG_FILE).exists());
    assert!(!d.path().join(GATE_FILE).exists());
}
#[test]
fn actual_first_google_client_with_real_crypto_succeeds_when_token_growth_fits() {
    let raw = token_rows(2);
    let d = capacity_home(&raw);
    std::fs::write(d.path().join(".keyfile"), [29u8; 32]).unwrap();
    let before = RawArray::read(&d.path().join(api::TOKEN_FILE)).unwrap();
    let probe = test_probe::Guard::install(d.path());
    api::upsert_client_config(d.path(), secret_google_client()).unwrap();
    assert_eq!(
        api::get_client_config(d.path(), "google")
            .unwrap()
            .client_secret,
        "owned-nonempty-client-secret"
    );
    let after = RawArray::read(&d.path().join(api::TOKEN_FILE)).unwrap();
    for p in ["p0", "p1"] {
        assert_eq!(before.element(p), after.element(p));
    }
    assert!(probe.google_crypto_count() > 0);
    assert_eq!(probe.counts()[1], 0);
    assert!(api::get_stored_token(d.path(), "google").is_none());
    assert!(!d.path().join(GATE_FILE).exists());
}
