//! B1 source-first producer tests. NOT COMPILED OR RUN; no RPC activation.
use super::*;
use crate::mcp_oauth::home_store::publication::{Fault, FaultGuard, GATE_FILE};
use crate::mcp_oauth::{self as api, home_lifecycle as start};
fn b1_home() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    d
}
fn input(secret: Option<&str>) -> start::GoogleHomeStartInput {
    start::GoogleHomeStartInput::new(Some("owned-client"), secret, "test-admin").unwrap()
}
fn capture(home: &Path, secret: Option<&str>) -> start::GoogleHomeStartPlan {
    start::capture_google_home_start(home, input(secret)).unwrap()
}
fn optional(home: &Path, file: &str) -> Option<Vec<u8>> {
    match std::fs::read(home.join(file)) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("owned read failed: {e}"),
    }
}
#[tokio::test(flavor = "current_thread")]
async fn private_req12_matches_url_client_ticket_pending_and_b2_public_builtin12() {
    let d = b1_home();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let probe = test_probe::Guard::install(d.path());
    let public = api::builtin_providers(&api::redirect_uri(d.path()))
        .into_iter()
        .find(|p| p.provider_id == "google")
        .unwrap();
    assert_eq!(public.scopes.len(), 12);
    let done = start::begin_google_home_authorization(d.path(), capture(d.path(), None))
        .await
        .unwrap();
    assert!(start::testing::pkce_matches(&done));
    let (url, state, pending) = done.into_response_parts();
    assert!(!state.is_empty());
    let query: url::Url = url.parse().unwrap();
    let values: std::collections::HashMap<_, _> = query.query_pairs().into_owned().collect();
    let request: Vec<_> = values["scope"].split(' ').map(str::to_owned).collect();
    let mut expected: Vec<_> = crate::google_workspace::REQUIRED_SCOPES
        .iter()
        .map(|s| s.to_string())
        .collect();
    expected.sort();
    expected.dedup();
    assert_eq!(expected.len(), 12);
    assert!(expected.iter().any(|scope| scope == "https://www.googleapis.com/auth/userinfo.email"));
    let mut public_scopes = public.scopes;
    public_scopes.sort();
    assert_eq!(public_scopes, expected);
    assert_eq!(request, expected);
    assert_eq!(values["state"], state);
    assert_eq!(values["code_challenge_method"], "S256");
    let clients: Value =
        serde_json::from_slice(&std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap())
            .unwrap();
    assert_eq!(clients[0]["scopes"], json!(expected));
    start::validate_google_home_pending(d.path(), &pending).unwrap();
    assert_eq!(probe.counts()[1], 0);
    assert!(!d.path().join(".keyfile").exists());
    // Start returned AFTER dropping its lease, not retaining it in Pending.
    let again = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    drop(again);
}
#[tokio::test(flavor = "current_thread")]
async fn source_client_and_token_changed_after_capture_refuse_before_crypto_or_new_ticket() {
    for file in ["config.toml", api::CLIENT_CONFIG_FILE, api::TOKEN_FILE] {
        let d = b1_home();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let plan = capture(d.path(), Some("must-not-be-encrypted"));
        let probe = test_probe::Guard::install(d.path());
        let winning = if file == "config.toml" {
            b"[integrations]\ngoogle_workspace=false\n[general]\nlog_level='debug'\n".to_vec()
        } else {
            b"[]".to_vec()
        };
        std::fs::write(d.path().join(file), &winning).unwrap();
        assert!(
            start::begin_google_home_authorization(d.path(), plan)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(d.path().join(file)).unwrap(), winning);
        assert_eq!(probe.counts(), [0, 0]);
        assert!(!d.path().join(".keyfile").exists());
        assert!(!d.path().join(GATE_FILE).exists());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn grant_wait_revalidates_original_snapshot_before_any_crypto() {
    let d = b1_home();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let plan = capture(d.path(), Some("must-not-be-encrypted"));
    let held = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    let probe = test_probe::Guard::install(d.path());
    let future = start::begin_google_home_authorization(d.path(), plan);
    tokio::pin!(future);
    tokio::select! {_=&mut future=>panic!("start unexpectedly completed while grant was held"),_=tokio::time::sleep(std::time::Duration::from_millis(30))=>{}}
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n[general]\nlog_level='debug'\n",
    )
    .unwrap();
    drop(held);
    assert!(future.await.is_err());
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!d.path().join(api::CLIENT_CONFIG_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
}
#[tokio::test(flavor = "current_thread")]
async fn crypto_boundary_rejects_client_or_token_winner_without_gate_or_url() {
    for file in [api::CLIENT_CONFIG_FILE, api::TOKEN_FILE] {
        let d = b1_home();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        std::fs::write(d.path().join(".keyfile"), [31u8; 32]).unwrap();
        let plan = capture(d.path(), Some("owned-new-secret"));
        let changed = d.path().join(file);
        let _hook = test_probe::BoundaryGuard::at(d.path(), move || {
            std::fs::write(changed, b"[]").unwrap()
        });
        let probe = test_probe::Guard::install(d.path());
        assert!(
            start::begin_google_home_authorization(d.path(), plan)
                .await
                .is_err()
        );
        assert!(probe.google_crypto_count() > 0);
        assert_eq!(probe.writer_counts(), [0, 0]);
        assert_eq!(probe.counts()[1], 0);
        assert!(!d.path().join(GATE_FILE).exists());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn saved_secret_is_captured_owned_prefill_and_mismatch_never_rebases() {
    let d = b1_home();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    std::fs::write(d.path().join(".keyfile"), [37u8; 32]).unwrap();
    let saved = json!([{"provider_id":"google","client_id":"owned-client","client_secret":"owned-old-legacy-secret","auth_url":"https://accounts.google.com/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":[]}]);
    std::fs::write(
        d.path().join(api::CLIENT_CONFIG_FILE),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    let plan = capture(d.path(), None);
    let probe = test_probe::Guard::install(d.path());
    let done = start::begin_google_home_authorization(d.path(), plan)
        .await
        .unwrap();
    let (_, _, pending) = done.into_response_parts();
    start::validate_google_home_pending(d.path(), &pending).unwrap();
    assert_eq!(
        api::get_client_config(d.path(), "google")
            .unwrap()
            .client_secret,
        "owned-old-legacy-secret"
    );
    assert!(probe.google_crypto_count() > 0);
    let stale = capture(d.path(), None);
    let mut external = saved;
    external[0]["client_secret"] = json!("owned-new-legacy-secret");
    std::fs::write(
        d.path().join(api::CLIENT_CONFIG_FILE),
        serde_json::to_vec(&external).unwrap(),
    )
    .unwrap();
    assert!(
        start::begin_google_home_authorization(d.path(), stale)
            .await
            .is_err()
    );
    assert!(!d.path().join(GATE_FILE).exists());
}
#[tokio::test(flavor = "current_thread")]
async fn known_bad_prefill_crypto_failure_never_publishes_client_ticket_or_response() {
    let d = b1_home();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    std::fs::write(d.path().join(".keyfile"), [41u8; 32]).unwrap();
    // Well-formed AES envelope metadata, wrong authentication tag/key.
    use base64::Engine as _;
    let bad = format!(
        "enc:v1:{}",
        base64::engine::general_purpose::STANDARD.encode([0u8; 32])
    );
    let saved = json!([{"provider_id":"google","client_id":"owned-client","client_secret":bad,"auth_url":"https://accounts.google.com/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":[]}]);
    std::fs::write(
        d.path().join(api::CLIENT_CONFIG_FILE),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    let before = optional(d.path(), api::CLIENT_CONFIG_FILE);
    let probe = test_probe::Guard::install(d.path());
    let plan = capture(d.path(), None);
    assert!(
        start::begin_google_home_authorization(d.path(), plan)
            .await
            .is_err()
    );
    assert!(probe.google_crypto_count() > 0);
    assert_eq!(optional(d.path(), api::CLIENT_CONFIG_FILE), before);
    assert!(!d.path().join(api::TOKEN_FILE).exists());
    assert!(!d.path().join(GATE_FILE).exists());
}
#[test]
fn static_primitives_reserved_prefill_and_encoded_size_refuse_before_crypto() {
    let d = b1_home();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let probe = test_probe::Guard::install(d.path());
    assert!(
        start::GoogleHomeStartInput::new(Some("bad\nid"), Some("secret"), "test-admin").is_err()
    );
    assert!(
        start::capture_google_home_start(
            d.path(),
            start::GoogleHomeStartInput::new(None, None, "test-admin").unwrap()
        )
        .is_err()
    );
    std::fs::write(d.path().join(api::CLIENT_CONFIG_FILE),br#"[{"provider_id":"google","client_id":"owned-client","client_secret":"enc:v2:unsupported","auth_url":"https://accounts.google.com/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":[]}]"#).unwrap();
    assert!(start::capture_google_home_start(d.path(), input(None)).is_err());
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!d.path().join(".keyfile").exists());
    std::fs::remove_file(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    assert!(
        start::capture_google_home_start(d.path(), input(Some(&"x".repeat(64 * 1024)))).is_err()
    );
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn connect_start_count_and_retained_internal_whitespace_capacity_refuse_precrypto() {
    for whitespace in [false, true] {
        let d = b1_home();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let raw = if whitespace {
            let mut raw=br#"[{"provider_id":"other","access_token":"","refresh_token":null,"expires_at":null,"scopes":[],"unknown":["#.to_vec();
            raw.resize(raw_array::MAX_BYTES - 68, b' ');
            raw.extend_from_slice(b"]}]");
            raw
        } else {
            serde_json::to_vec(&(0..512).map(|n|json!({"provider_id":format!("p{n}"),"access_token":"","refresh_token":null,"expires_at":null,"scopes":[]})).collect::<Vec<_>>()).unwrap()
        };
        std::fs::write(d.path().join(api::TOKEN_FILE), &raw).unwrap();
        let probe = test_probe::Guard::install(d.path());
        assert!(
            start::capture_google_home_start(d.path(), input(Some("must-not-encrypt"))).is_err()
        );
        assert_eq!(probe.counts(), [0, 0]);
        assert_eq!(bytes(d.path()), raw);
        assert!(!d.path().join(".keyfile").exists());
        assert!(!d.path().join(GATE_FILE).exists());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn each_durability_failure_returns_no_started_object_and_never_retries() {
    for fault in [
        Fault::GateCreate,
        Fault::GateWrite,
        Fault::GateFileSync,
        Fault::GateDirSync,
        Fault::DataOpen,
        Fault::DataWrite,
        Fault::DataFileSync,
        Fault::DataRename,
        Fault::DataDirSync,
        Fault::SourceOpen,
        Fault::SourceWrite,
        Fault::SourceFileSync,
        Fault::SourceRename,
        Fault::SourceDirSync,
        Fault::FinalProof,
        Fault::GateUnlink,
        Fault::CleanupDirSync,
    ] {
        let d = b1_home();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let plan = capture(d.path(), None);
        let probe = test_probe::Guard::install(d.path());
        let f = FaultGuard::at(d.path(), fault);
        assert!(
            start::begin_google_home_authorization(d.path(), plan)
                .await
                .is_err(),
            "{fault:?}"
        );
        drop(f);
        assert_eq!(probe.counts()[1], 0);
        if let Some(raw) = optional(d.path(), api::TOKEN_FILE) {
            let rows: Value = serde_json::from_slice(&raw).unwrap();
            assert_ne!(
                rows[0]["google_home"]["state"], "ready",
                "first transaction fault cannot return Ready ticket"
            );
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn only_latest_successful_start_is_valid_and_two_old_snapshots_do_not_recapture() {
    let d = b1_home();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let a = capture(d.path(), None);
    let stale = capture(d.path(), None);
    let (_, _, first) = start::begin_google_home_authorization(d.path(), a)
        .await
        .unwrap()
        .into_response_parts();
    assert!(
        start::begin_google_home_authorization(d.path(), stale)
            .await
            .is_err()
    );
    start::validate_google_home_pending(d.path(), &first).unwrap();
    let (_, _, second) = start::begin_google_home_authorization(d.path(), capture(d.path(), None))
        .await
        .unwrap()
        .into_response_parts();
    assert!(start::validate_google_home_pending(d.path(), &first).is_err());
    start::validate_google_home_pending(d.path(), &second).unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn wall_expiry_and_ram_instant_are_independent_and_expiry_does_not_recapture() {
    let d = b1_home();
    let clock = publication::ClockGuard::fixed_if_unset(NOW);
    let plan = capture(d.path(), None);
    drop(clock);
    let expiry = publication::ClockGuard::after_preflight(NOW + 601, NOW + 601);
    let probe = test_probe::Guard::install(d.path());
    assert!(
        start::begin_google_home_authorization(d.path(), plan)
            .await
            .is_err()
    );
    assert_eq!(probe.counts(), [0, 0]);
    drop(expiry);
    let clock = publication::ClockGuard::fixed_if_unset(NOW);
    let (_, _, mut pending) =
        start::begin_google_home_authorization(d.path(), capture(d.path(), None))
            .await
            .unwrap()
            .into_response_parts();
    start::testing::age_ram(&mut pending, std::time::Duration::from_secs(601));
    assert!(pending.is_expired_in_ram());
    assert!(start::validate_google_home_pending(d.path(), &pending).is_err());
    drop(clock);
}
#[tokio::test(flavor = "current_thread")]
async fn token_scope_history_and_foreign_client_slices_are_not_upgraded_by_start() {
    let d = b1_home();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let old_scopes: Vec<String> = [
        "https://www.googleapis.com/auth/gmail.readonly",
        "https://www.googleapis.com/auth/gmail.compose",
        "https://www.googleapis.com/auth/calendar.events",
        "https://www.googleapis.com/auth/spreadsheets",
        "https://www.googleapis.com/auth/userinfo.email",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    std::fs::write(d.path().join(api::TOKEN_FILE),serde_json::to_vec(&json!([{"provider_id":"google","access_token":"owned-legacy-token","refresh_token":null,"expires_at":null,"scopes":old_scopes}])).unwrap()).unwrap();
    let foreign =
        r#" { "provider_id":"other", "future": ["\"\\",null,{"台灣":true}] } "#.as_bytes();
    let mut clients = b"[".to_vec();
    clients.extend_from_slice(foreign);
    clients.push(b']');
    std::fs::write(d.path().join(api::CLIENT_CONFIG_FILE), clients).unwrap();
    let before = raw_array::RawArray::read(&d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    start::begin_google_home_authorization(d.path(), capture(d.path(), None))
        .await
        .unwrap();
    let tokens: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(tokens[0]["scopes"], json!(old_scopes));
    let after = raw_array::RawArray::read(&d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    assert_eq!(before.element("other"), after.element("other"));
}
#[tokio::test(flavor = "current_thread")]
async fn connect_start_pending_shape_fits_its_own_bound_without_projection_authority() {
    for preexisting_client in [false, true] {
        let d = b1_home();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        if preexisting_client {
            std::fs::write(d.path().join(api::CLIENT_CONFIG_FILE),br#"[{"provider_id":"google","client_id":"owned-old-client","client_secret":"","auth_url":"https://accounts.google.com/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":[]}]"#).unwrap();
        }
        let plan = capture(d.path(), None);
        let f = FaultGuard::at(d.path(), Fault::SourceWrite);
        assert!(
            start::begin_google_home_authorization(d.path(), plan)
                .await
                .is_err()
        );
        drop(f);
        let rows: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        let control = &rows[0]["google_home"];
        assert_eq!(control["state"], "pending");
        assert_eq!(control["pending_mutation"]["kind"], "connect_start");
        assert!(
            serde_json::to_vec_pretty(control).unwrap().len() < CONNECT_START_CONTROL_RESERVE_BYTES
        );
        fn arrays(value: &Value) -> usize {
            match value {
                Value::Array(v) if v.len() == 32 && v.iter().all(|x| x.as_u64().is_some()) => 1,
                Value::Array(v) => v.iter().map(arrays).sum(),
                Value::Object(v) => v.values().map(arrays).sum(),
                _ => 0,
            }
        }
        assert_eq!(arrays(control), if preexisting_client { 12 } else { 11 });
    }
}

#[tokio::test(flavor = "current_thread")]
async fn second_publication_faults_never_issue_started_or_replay_first_publication() {
    for fault in [
        Fault::GateCreate,
        Fault::GateWrite,
        Fault::GateFileSync,
        Fault::GateDirSync,
        Fault::DataOpen,
        Fault::DataWrite,
        Fault::DataFileSync,
        Fault::DataRename,
        Fault::DataDirSync,
        Fault::FinalProof,
        Fault::GateUnlink,
        Fault::CleanupDirSync,
    ] {
        let d = b1_home();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let plan = capture(d.path(), None);
        let owned = d.path().to_path_buf();
        let installed = std::rc::Rc::new(std::cell::RefCell::new(None));
        let inside = installed.clone();
        let _boundary = start::testing::BetweenGuard::at(d.path(), move || {
            *inside.borrow_mut() = Some(FaultGuard::at(&owned, fault));
        });
        let probe = test_probe::Guard::install(d.path());
        assert!(
            start::begin_google_home_authorization(d.path(), plan)
                .await
                .is_err(),
            "{fault:?}"
        );
        installed.borrow_mut().take();
        // First phase's one client payload, never a second ConnectStart/retry.
        assert_eq!(probe.writer_counts()[1], 1);
        assert_eq!(probe.counts()[1], 0);
        if matches!(fault, Fault::GateUnlink | Fault::CleanupDirSync) {
            let rows: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
            assert_eq!(rows[0]["google_home"]["state"], "ready");
            // Durable Ready is possible with unconfirmed cleanup, but no T/URL.
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn changed_config_or_expiry_between_publications_returns_no_url_or_reselection() {
    for expiry in [false, true] {
        let d = b1_home();
        let clock = std::rc::Rc::new(std::cell::RefCell::new(Some(
            publication::ClockGuard::fixed_if_unset(NOW),
        )));
        let old_clock = clock.clone();
        let plan = capture(d.path(), None);
        let owned = d.path().to_path_buf();
        let next = std::rc::Rc::new(std::cell::RefCell::new(None));
        let inside = next.clone();
        let _boundary = start::testing::BetweenGuard::at(d.path(), move || {
            if expiry {
                old_clock.borrow_mut().take();
                *inside.borrow_mut() = Some(publication::ClockGuard::after_preflight(
                    NOW + 601,
                    NOW + 601,
                ));
            } else {
                std::fs::write(
                    owned.join("config.toml"),
                    "[integrations]\ngoogle_workspace=false\n[gateway]\nport=19000\n",
                )
                .unwrap();
            }
        });
        assert!(
            start::begin_google_home_authorization(d.path(), plan)
                .await
                .is_err()
        );
        let rows: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        assert_eq!(rows[0]["google_home"]["state"], "pending");
        assert!(rows[0]["google_home"]["active_connect"].is_object());
        assert!(!d.path().join(GATE_FILE).exists());
        next.borrow_mut().take();
    }
}
#[test]
fn unresolved_config_and_connect_finish_barriers_cannot_be_replaced_by_private_start() {
    for finish in [false, true] {
        let d = oauth_fixture();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        if finish {
            let t = ticket(d.path());
            let claim = run(d.path(), |g, s| {
                publication_transaction(g, |r| r.claim(&t, s, NOW))
            })
            .unwrap();
            run(d.path(), |g, s| {
                publication_transaction(g, |r| {
                    r.begin_claimed_finish(&claim, s, &payload(&["scope-a"]), None, NOW)
                })
            })
            .unwrap();
        } else {
            run(d.path(), |g, s| {
                publication_transaction(g, |r| r.fence_change(s, s.binding().clone()))
            })
            .unwrap();
        }
        let before = bytes(d.path());
        let probe = test_probe::Guard::install(d.path());
        assert!(
            start::capture_google_home_start(d.path(), input(Some("must-not-encrypt"))).is_err()
        );
        assert_eq!(bytes(d.path()), before);
        assert_eq!(probe.counts(), [0, 0]);
        assert!(!d.path().join(".keyfile").exists());
    }
}
