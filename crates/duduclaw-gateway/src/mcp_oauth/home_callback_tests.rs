//! B2 SOURCE FIRST, NOT RUN. Real private pipeline; owned fake-wire only.
use super::*;
use crate::mcp_oauth::home_store::publication::{Fault, FaultGuard, GATE_FILE};
use crate::mcp_oauth::{self as api, home_lifecycle as flow};
async fn begun() -> (tempfile::TempDir, String, flow::GoogleHomePending) {
    begun_mode(false).await
}
async fn begun_mode(master: bool) -> (tempfile::TempDir, String, flow::GoogleHomePending) {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        format!("[integrations]\ngoogle_workspace={master}\n[general]\nlog_level='debug'\n"),
    )
    .unwrap();
    std::fs::write(d.path().join(".keyfile"), [7u8; 32]).unwrap();
    let input =
        flow::GoogleHomeStartInput::new(Some("owned-client"), Some("owned-client-secret")).unwrap();
    let plan = flow::capture_google_home_start(d.path(), input).unwrap();
    let (_, state, pending) = flow::begin_google_home_authorization(d.path(), plan)
        .await
        .unwrap()
        .into_response_parts();
    (d, state, pending)
}
#[tokio::test(flavor = "current_thread")]
async fn callback_same_ticket_three_points_enables_master_and_preserves_unrelated_fields() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, pending) = begun().await;
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    flow::complete_google_home_authorization(
        d.path(),
        pending,
        &state,
        zeroize::Zeroizing::new("owned-code".into()),
    )
    .await
    .unwrap();
    assert_eq!(wire.requests(), 1);
    let cfg: toml::Table = std::fs::read_to_string(d.path().join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        cfg["integrations"]["google_workspace"].as_bool(),
        Some(true)
    );
    assert_eq!(cfg["general"]["log_level"].as_str(), Some("debug"));
    let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(row[0]["google_home"]["state"], "ready");
    assert!(row[0]["google_home"]["active_connect"].is_null());
    assert_eq!(row[0]["scopes"].as_array().unwrap().len(), 11);
    assert!(!d.path().join(GATE_FILE).exists());
}
#[tokio::test(flavor = "current_thread")]
async fn wrong_state_stale_client_and_claim_cleanup_fail_before_provider() {
    for lane in [0, 1, 2] {
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let (d, state, pending) = begun().await;
        let wire = home_source::callback_testing::Wire::json(d.path(), None);
        let fault = if lane == 2 {
            Some(FaultGuard::at(d.path(), Fault::CleanupDirSync))
        } else {
            None
        };
        if lane == 1 {
            let p = d.path().join(api::CLIENT_CONFIG_FILE);
            let raw = std::fs::read_to_string(&p).unwrap();
            std::fs::write(p, raw.replace("owned-client", "different-client")).unwrap();
        }
        let returned = if lane == 0 { "wrong-state" } else { &state };
        assert!(
            flow::complete_google_home_authorization(
                d.path(),
                pending,
                returned,
                zeroize::Zeroizing::new("owned-code".into())
            )
            .await
            .is_err()
        );
        drop(fault);
        assert_eq!(wire.requests(), 0);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn exchange_interleave_revoke_client_master_and_resolver_aba_cannot_publish_or_enable() {
    for mutation in ["revoke", "client", "master", "resolver", "sa", "bridge"] {
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let (d, state, pending) = begun().await;
        let owned = d.path().to_path_buf();
        let wire = home_source::callback_testing::Wire::with_hook(
            d.path(),
            move || match mutation {
                "revoke" => revoke_google(&owned).unwrap(),
                "client" => {
                    let mut c = api::get_client_config(&owned, "google").unwrap();
                    let old = c.client_id.clone();
                    c.client_id = "other-client".into();
                    api::upsert_client_config(&owned, c.clone()).unwrap();
                    c.client_id = old;
                    api::upsert_client_config(&owned, c).unwrap();
                }
                "master" => {
                    crate::google_workspace::set_integration_enabled(&owned, true).unwrap();
                    crate::google_workspace::set_integration_enabled(&owned, false).unwrap();
                }
                "sa" | "bridge" => {
                    let p = owned.join("config.toml");
                    let original = std::fs::read_to_string(&p).unwrap();
                    let section = if mutation == "sa" {
                        std::fs::write(owned.join("owned-sa.json"), b"{} safe fake bytes").unwrap();
                        "[integrations.google_service_account]\nkey_file='owned-sa.json'\nsubject='owned@example.test'\n"
                    } else {
                        "[integrations.google_apps_script]\nurl='https://script.google.com/macros/s/owned-fixture/exec'\nsecret='owned-fixture-only'\n"
                    };
                    crate::commit_config_rendered(
                        &owned,
                        &crate::handlers::config_commit::content_hash(&original),
                        |current| Ok(format!("{current}\n{section}")),
                    )
                    .unwrap();
                    let next = std::fs::read_to_string(&p).unwrap();
                    crate::commit_config_rendered(
                        &owned,
                        &crate::handlers::config_commit::content_hash(&next),
                        |_| Ok(original),
                    )
                    .unwrap();
                }
                _ => {
                    let p = owned.join("config.toml");
                    let original = std::fs::read_to_string(&p).unwrap();
                    let h = crate::handlers::config_commit::content_hash(&original);
                    crate::commit_config_rendered(&owned, &h, |current| {
                        Ok(format!(
                            "{current}\n[secret_manager]\nbackend='filesystem'\n"
                        ))
                    })
                    .unwrap();
                    let next = std::fs::read_to_string(&p).unwrap();
                    crate::commit_config_rendered(
                        &owned,
                        &crate::handlers::config_commit::content_hash(&next),
                        |_| Ok(original),
                    )
                    .unwrap();
                }
            },
        );
        assert!(
            flow::complete_google_home_authorization(
                d.path(),
                pending,
                &state,
                zeroize::Zeroizing::new("owned-code".into())
            )
            .await
            .is_err()
        );
        assert_eq!(wire.requests(), 1);
        let cfg = std::fs::read_to_string(d.path().join("config.toml")).unwrap();
        assert!(
            !cfg.parse::<toml::Table>().unwrap()["integrations"]["google_workspace"]
                .as_bool()
                .unwrap()
        );
        let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        assert!(row[0].get("access_token").is_none());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn callback_each_durability_fault_no_success_and_single_provider_call() {
    for phase in [1, 2, 3] {
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
            if phase != 2
                && matches!(
                    fault,
                    Fault::SourceOpen
                        | Fault::SourceWrite
                        | Fault::SourceFileSync
                        | Fault::SourceRename
                        | Fault::SourceDirSync
                )
            {
                continue;
            }
            let _clock = publication::ClockGuard::fixed_if_unset(NOW);
            let (d, state, pending) = begun().await;
            let wire = home_source::callback_testing::Wire::json(d.path(), None);
            let installed = std::rc::Rc::new(std::cell::RefCell::new(None));
            let inside = installed.clone();
            let owned = d.path().to_path_buf();
            let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), phase, move || {
                *inside.borrow_mut() = Some(FaultGuard::at(&owned, fault))
            });
            assert!(
                flow::complete_google_home_authorization(
                    d.path(),
                    pending,
                    &state,
                    zeroize::Zeroizing::new("owned-code".into())
                )
                .await
                .is_err()
            );
            installed.borrow_mut().take();
            assert_eq!(wire.requests(), 1);
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn redirect_http_failure_and_reset_never_retry_or_publish() {
    for response in [
        "redirect",
        "timeout",
        "rejected",
        "reset",
        "partial_scope",
        "bad_expiry",
    ] {
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let (d, state, pending) = begun().await;
        let wire = home_source::callback_testing::Wire::scenario(d.path(), response);
        assert!(
            flow::complete_google_home_authorization(
                d.path(),
                pending,
                &state,
                zeroize::Zeroizing::new("owned-code".into())
            )
            .await
            .is_err()
        );
        assert_eq!(wire.requests(), 1);
        let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        assert!(row[0].get("access_token").is_none());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn callback_precrypto_internal_whitespace_capacity_and_postcrypto_winner_are_fenced() {
    for after_crypto in [false, true] {
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let (d, state, pending) = begun().await;
        let probe = test_probe::Guard::install(d.path());
        let wire = home_source::callback_testing::Wire::json(d.path(), None);
        let owned = d.path().to_path_buf();
        let _hook = flow::testing::FinishBoundaryGuard::at(
            d.path(),
            if after_crypto { 0 } else { -2 },
            move || {
                let p = owned.join(api::TOKEN_FILE);
                let mut rows: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
                if after_crypto {
                    rows.as_array_mut()
                        .unwrap()
                        .push(json!({"provider_id":"unrelated","future":true}));
                    std::fs::write(p, serde_json::to_vec(&rows).unwrap()).unwrap();
                } else {
                    let mut bytes = b"[".to_vec();
                    bytes.extend_from_slice(&serde_json::to_vec(&rows[0]).unwrap());
                    bytes.extend_from_slice(br#",{"provider_id":"unrelated","future":["#);
                    bytes.resize(raw_array::MAX_BYTES - 8, b' ');
                    bytes.extend_from_slice(b"]}]");
                    std::fs::write(p, bytes).unwrap();
                }
            },
        );
        assert!(
            flow::complete_google_home_authorization(
                d.path(),
                pending,
                &state,
                zeroize::Zeroizing::new("owned-code".into())
            )
            .await
            .is_err()
        );
        assert_eq!(wire.requests(), if after_crypto { 1 } else { 0 });
        if after_crypto {
            assert!(probe.google_crypto_count() > 1);
        } else {
            assert_eq!(probe.google_crypto_count(), 0);
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn already_enabled_home_needs_no_config_payload_and_ordinary_callback_succeeds() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, pending) = begun_mode(true).await;
    let before = std::fs::read(d.path().join("config.toml")).unwrap();
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let installed = std::rc::Rc::new(std::cell::RefCell::new(None));
    let inside = installed.clone();
    let owned = d.path().to_path_buf();
    let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), 2, move || {
        *inside.borrow_mut() = Some(FaultGuard::at(&owned, Fault::SourceWrite))
    });
    flow::complete_google_home_authorization(
        d.path(),
        pending,
        &state,
        zeroize::Zeroizing::new("owned-code".into()),
    )
    .await
    .unwrap();
    installed.borrow_mut().take();
    assert_eq!(wire.requests(), 1);
    assert_eq!(std::fs::read(d.path().join("config.toml")).unwrap(), before);
}
#[tokio::test(flavor = "current_thread")]
async fn too_large_encrypted_output_refuses_before_token_crypto_after_one_client_unseal() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, pending) = begun().await;
    let probe = test_probe::Guard::install(d.path());
    let wire = home_source::callback_testing::Wire::json(
        d.path(),
        Some(json!({"access_token":"A".repeat(800_000),"token_type":"Bearer","expires_in":3600})),
    );
    assert!(
        flow::complete_google_home_authorization(
            d.path(),
            pending,
            &state,
            zeroize::Zeroizing::new("owned-code".into())
        )
        .await
        .is_err()
    );
    assert_eq!(wire.requests(), 1);
    assert_eq!(probe.google_crypto_count(), 1);
    let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert!(row[0].get("access_token").is_none());
}
#[tokio::test(flavor = "current_thread")]
async fn final_config_after_digest_prevents_ready_on_unrelated_file_rewrite() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, pending) = begun().await;
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let path = d.path().join("config.toml");
    let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), 3, move || {
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str("\n# concurrent unrelated writer\n");
        std::fs::write(path, raw).unwrap();
    });
    assert!(
        flow::complete_google_home_authorization(
            d.path(),
            pending,
            &state,
            zeroize::Zeroizing::new("owned-code".into())
        )
        .await
        .is_err()
    );
    assert_eq!(wire.requests(), 1);
    let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(row[0]["google_home"]["state"], "pending");
}
#[tokio::test(flavor = "current_thread")]
async fn ticket_wall_expiry_between_each_finish_point_cannot_publish_ready() {
    for phase in [1, 2, 3] {
        let clock = std::rc::Rc::new(std::cell::RefCell::new(Some(
            publication::ClockGuard::fixed_if_unset(NOW),
        )));
        let previous = clock.clone();
        let (d, state, pending) = begun().await;
        let wire = home_source::callback_testing::Wire::json(d.path(), None);
        let next = std::rc::Rc::new(std::cell::RefCell::new(None));
        let installed = next.clone();
        let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), phase, move || {
            previous.borrow_mut().take();
            *installed.borrow_mut() = Some(publication::ClockGuard::after_preflight(
                NOW + 601,
                NOW + 601,
            ));
        });
        assert!(
            flow::complete_google_home_authorization(
                d.path(),
                pending,
                &state,
                zeroize::Zeroizing::new("owned-code".into())
            )
            .await
            .is_err()
        );
        assert_eq!(wire.requests(), 1);
        next.borrow_mut().take();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn captured_client_change_after_unseal_refuses_before_single_post() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, pending) = begun().await;
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let mut client = api::get_client_config(d.path(), "google").unwrap();
    let owned = d.path().to_path_buf();
    let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), -3, move || {
        client.client_id = "changed-after-unseal".into();
        api::upsert_client_config(&owned, client).unwrap();
    });
    assert!(
        flow::complete_google_home_authorization(
            d.path(),
            pending,
            &state,
            zeroize::Zeroizing::new("owned-code".into())
        )
        .await
        .is_err()
    );
    assert_eq!(wire.requests(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn exact_client_value_aba_through_public_writer_invalidates_original_claim() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    let input = flow::GoogleHomeStartInput::new(Some("owned-client"), None).unwrap();
    let plan = flow::capture_google_home_start(d.path(), input).unwrap();
    let (_, state, pending) = flow::begin_google_home_authorization(d.path(), plan)
        .await
        .unwrap()
        .into_response_parts();
    let old = std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    let owned = d.path().to_path_buf();
    let wire = home_source::callback_testing::Wire::with_hook(d.path(), move || {
        let mut config = api::get_client_config(&owned, "google").unwrap();
        config.client_id = "other-client".into();
        api::upsert_client_config(&owned, config.clone()).unwrap();
        config.client_id = "owned-client".into();
        api::upsert_client_config(&owned, config).unwrap();
    });
    assert!(
        flow::complete_google_home_authorization(
            d.path(),
            pending,
            &state,
            zeroize::Zeroizing::new("owned-code".into())
        )
        .await
        .is_err()
    );
    assert_eq!(wire.requests(), 1);
    let current = std::fs::read(d.path().join(api::CLIENT_CONFIG_FILE)).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&old).unwrap(),
        serde_json::from_slice::<Value>(&current).unwrap()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn final_ram_lock_wait_cannot_extend_durable_wall_ticket() {
    let clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    let input = flow::GoogleHomeStartInput::new(Some("owned-client"), None).unwrap();
    let plan = flow::capture_google_home_start(d.path(), input).unwrap();
    let started = flow::begin_google_home_authorization(d.path(), plan)
        .await
        .unwrap();
    let map = tokio::sync::RwLock::new(std::collections::HashMap::new());
    let lock = map.write().await;
    let insert = api::insert_google_start(d.path(), started, &map);
    tokio::pin!(insert);
    tokio::select! {_=&mut insert=>panic!("RAM lock unexpectedly acquired"),_=tokio::time::sleep(std::time::Duration::from_millis(30))=>{}}
    drop(clock);
    let _expired = publication::ClockGuard::after_preflight(NOW + 601, NOW + 601);
    drop(lock);
    assert!(insert.await.is_err());
    assert!(map.read().await.is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn full_response_and_superset_store_only_actual_granted_scopes() {
    for superset in [false, true] {
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let (d, state, pending) = begun().await;
        let mut scopes: Vec<_> = crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        if superset {
            scopes.push("extra:actual".into());
        }
        let wire = home_source::callback_testing::Wire::json(
            d.path(),
            Some(
                json!({"access_token":"owned-access","token_type":"Bearer","expires_in":3600,"scope":scopes.join(" ")}),
            ),
        );
        flow::complete_google_home_authorization(
            d.path(),
            pending,
            &state,
            zeroize::Zeroizing::new("owned-code".into()),
        )
        .await
        .unwrap();
        assert_eq!(wire.requests(), 1);
        let token = api::get_stored_token(d.path(), "google").unwrap();
        assert_eq!(token.scopes, normalized_scopes(scopes).unwrap());
        assert!(token.refresh_token.is_none());
    }
}

// R2 SOURCE FIRST: preserve the original RAM clock independently of wall time.
// The keyed test clock advances elapsed time; no sleep, ticket reset or ENV.
#[tokio::test(flavor = "current_thread")]
async fn original_ram_expiry_before_client_unseal_refuses_without_provider_or_crypto() {
    let _wall = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, mut pending) = begun().await;
    flow::testing::age_ram(&mut pending, std::time::Duration::from_secs(590));
    let clock = flow::testing::RamClockGuard::at(d.path());
    let elapsed = clock.elapsed_handle();
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let _probe = test_probe::Guard::install(d.path());
    let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), -7, move || {
        elapsed.store(11, std::sync::atomic::Ordering::SeqCst);
    });
    assert!(
        flow::complete_google_home_authorization(
            d.path(),
            pending,
            &state,
            zeroize::Zeroizing::new("owned-code".into())
        )
        .await
        .is_err()
    );
    assert!(_hook.triggered());
    assert_eq!(wire.requests(), 0);
    assert_eq!(_probe.google_crypto_count(), 0);
    let config: toml::Table = std::fs::read_to_string(d.path().join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        config["integrations"]["google_workspace"].as_bool(),
        Some(false)
    );
}
#[tokio::test(flavor = "current_thread")]
async fn original_ram_expiry_after_provider_and_body_await_rejects_with_valid_or_rolled_back_wall()
{
    for wall in [NOW, NOW - 20] {
        for boundary in [-5, -6] {
            let wall_clock = std::rc::Rc::new(std::cell::RefCell::new(Some(
                publication::ClockGuard::fixed_if_unset(NOW),
            )));
            let original_wall = wall_clock.clone();
            let (d, state, mut pending) = begun().await;
            flow::testing::age_ram(&mut pending, std::time::Duration::from_secs(590));
            let clock = flow::testing::RamClockGuard::at(d.path());
            let elapsed = clock.elapsed_handle();
            let wire = home_source::callback_testing::Wire::json(d.path(), None);
            let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), boundary, move || {
                original_wall.borrow_mut().take();
                *original_wall.borrow_mut() =
                    Some(publication::ClockGuard::after_preflight(wall, wall));
                elapsed.store(11, std::sync::atomic::Ordering::SeqCst);
            });
            assert!(
                flow::complete_google_home_authorization(
                    d.path(),
                    pending,
                    &state,
                    zeroize::Zeroizing::new("owned-code".into())
                )
                .await
                .is_err()
            );
            assert!(_hook.triggered());
            assert_eq!(wire.requests(), 1);
            let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
            assert!(row[0]["access_token"].as_str().is_none_or(str::is_empty));
            let config: toml::Table = std::fs::read_to_string(d.path().join("config.toml"))
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(
                config["integrations"]["google_workspace"].as_bool(),
                Some(false)
            );
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn original_ram_expiry_at_each_unseal_and_seal_boundary_never_completes() {
    for boundary in [-3, -4, -1, -8, -9, -10, 0] {
        let _wall = publication::ClockGuard::fixed_if_unset(NOW);
        let (d, state, mut pending) = begun().await;
        flow::testing::age_ram(&mut pending, std::time::Duration::from_secs(590));
        let clock = flow::testing::RamClockGuard::at(d.path());
        let elapsed = clock.elapsed_handle();
        let wire = home_source::callback_testing::Wire::json(d.path(), None);
        let probe = test_probe::Guard::install(d.path());
        let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), boundary, move || {
            elapsed.store(11, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(
            flow::complete_google_home_authorization(
                d.path(),
                pending,
                &state,
                zeroize::Zeroizing::new("owned-code".into())
            )
            .await
            .is_err()
        );
        assert!(_hook.triggered());
        assert_eq!(
            wire.requests(),
            if [-3, -4].contains(&boundary) { 0 } else { 1 }
        );
        assert_eq!(
            probe.google_crypto_count(),
            match boundary {
                -3 | -4 | -1 => 1,
                -8 | -9 => 2,
                -10 | 0 => 3,
                _ => unreachable!(),
            }
        );
        let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        assert!(row[0]["access_token"].as_str().is_none_or(str::is_empty));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn original_ram_expiry_at_all_three_publication_pre_and_post_fences_never_returns_success() {
    // Stage*100 + check1..10: lock, producer, pregate, postgate,
    // postdata, presource, postsource, afterproof, precleanup, postcleanup.
    for rollback in [false, true] {
        let boundaries = [1, 2, 3, 11, 12, 13]
            .into_iter()
            .chain((1..=3).flat_map(|stage| (1..=10).map(move |check| stage * 100 + check)));
        for boundary in boundaries {
            let wall_clock = std::rc::Rc::new(std::cell::RefCell::new(Some(
                publication::ClockGuard::fixed_if_unset(NOW),
            )));
            let original_wall = wall_clock.clone();
            let (d, state, mut pending) = begun().await;
            flow::testing::age_ram(&mut pending, std::time::Duration::from_secs(590));
            let original_started_at = flow::testing::ram_created_at_for_diagnostics(&pending);
            let clock = flow::testing::RamClockGuard::at(d.path());
            let elapsed = clock.elapsed_handle();
            let observed_elapsed = elapsed.clone();
            let wire = home_source::callback_testing::Wire::json(d.path(), None);
            let _hook = flow::testing::FinishBoundaryGuard::at(d.path(), boundary, move || {
                if rollback {
                    original_wall.borrow_mut().take();
                    *original_wall.borrow_mut() =
                        Some(publication::ClockGuard::after_preflight(NOW - 20, NOW - 20));
                }
                elapsed.store(11, std::sync::atomic::Ordering::SeqCst);
            });
            let result = flow::complete_google_home_authorization(
                d.path(),
                pending,
                &state,
                zeroize::Zeroizing::new("owned-code".into()),
            )
            .await;
            let diagnostics = format!(
                "boundary {boundary}, rollback={rollback}, result={result:?}, triggered={}, wire_requests={}, original_ram_elapsed_ms={}, injected_elapsed_secs={}",
                _hook.triggered(),
                wire.requests(),
                original_started_at.elapsed().as_millis(),
                observed_elapsed.load(std::sync::atomic::Ordering::SeqCst),
            );
            assert!(result.is_err(), "{diagnostics}");
            if boundary >= 100 {
                let expected = match boundary % 100 {
                    1..=3 => Error::Stale,
                    4 => Error::NotPublished,
                    5..=9 => Error::PublishedDurabilityUnconfirmed,
                    10 => Error::CommittedCleanupUnconfirmed,
                    _ => unreachable!(),
                };
                assert_eq!(result, Err(expected), "{diagnostics}");
            }
            assert!(_hook.triggered(), "boundary was not reached: {diagnostics}");
            assert_eq!(wire.requests(), 1);
            // Already durable writes are never rolled back or retried. A gate
            // remains on postdata refusal; postcleanup still refuses callback T.
            if boundary >= 100 && (4..=9).contains(&(boundary % 100)) {
                assert!(d.path().join(GATE_FILE).exists());
                assert!(api::get_stored_token(d.path(), "google").is_none());
            }
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn original_near_expiry_ram_deadline_without_elapsed_advance_still_completes() {
    let _wall = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, mut pending) = begun().await;
    flow::testing::age_ram(&mut pending, std::time::Duration::from_secs(590));
    let _clock = flow::testing::RamClockGuard::at(d.path());
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    flow::complete_google_home_authorization(
        d.path(),
        pending,
        &state,
        zeroize::Zeroizing::new("owned-code".into()),
    )
    .await
    .unwrap();
    assert_eq!(wire.requests(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn original_ram_expiry_during_single_owned_provider_request_prevents_token_crypto() {
    let _wall = publication::ClockGuard::fixed_if_unset(NOW);
    let (d, state, mut pending) = begun().await;
    flow::testing::age_ram(&mut pending, std::time::Duration::from_secs(590));
    let clock = flow::testing::RamClockGuard::at(d.path());
    let elapsed = clock.elapsed_handle();
    let observed = elapsed.clone();
    let wire = home_source::callback_testing::Wire::with_hook(d.path(), move || {
        elapsed.store(11, std::sync::atomic::Ordering::SeqCst);
    });
    let probe = test_probe::Guard::install(d.path());
    assert!(
        flow::complete_google_home_authorization(
            d.path(),
            pending,
            &state,
            zeroize::Zeroizing::new("owned-code".into())
        )
        .await
        .is_err()
    );
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 11);
    assert_eq!(wire.requests(), 1);
    assert_eq!(probe.google_crypto_count(), 1); // only captured client unseal
    let row: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert!(row[0].get("access_token").is_none());
}
