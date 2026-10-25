//! SOURCE-FIRST, NOT RUN. Core fixtures use owned unusable ciphertext.
//! B1 start_tests additionally declare owned local .keyfile crypto cases;
//! none uses an OS protection store, token endpoint or real provider.
use super::*;
use crate::google_identity::home_source::{self, ObservedSource, SourceGuard};
use serde_json::json;
const NOW: u64 = 1_800_000_000;
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=true\n",
    )
    .unwrap();
    std::fs::write(d.path().join(super::super::CLIENT_CONFIG_FILE), r#"[{"provider_id":"google","client_id":"fake-client","client_secret":"enc:v1:unusable-client","auth_url":"https://accounts.google.com/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":["scope-a"]}]"#).unwrap();
    std::fs::write(d.path().join(super::super::TOKEN_FILE), serde_json::to_vec(&json!([
        {"provider_id":"github","access_token":"enc:v1:non-google","refresh_token":null,"expires_at":null,"scopes":["repo"],"future_field":{"retained":true}},
        {"provider_id":"google","access_token":"enc:v1:unusable-access","refresh_token":"enc:v1:unusable-refresh","expires_at":null,"scopes":["scope-a","scope-b"]}
    ])).unwrap()).unwrap();
    // Unique client ids isolate the provider-boundary counter across tests.
    let path = d.path().join(super::super::CLIENT_CONFIG_FILE);
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        raw.replace(
            "fake-client",
            &format!("fake-client-{}", uuid::Uuid::new_v4()),
        ),
    )
    .unwrap();
    d
}
fn run<T>(
    home: &Path,
    f: impl FnOnce(&SourceGuard, &mut ObservedSource) -> Result<T>,
) -> Result<T> {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    home_source::with_metadata(home, f)
}
fn ticket(home: &Path) -> GoogleConnectTicket {
    let start = run(home, |g, s| {
        publication_transaction(g, |r| {
            r.begin_connect(s, RawDigest::of(b"fake-state"), NOW + 500, NOW)
        })
    })
    .unwrap();
    run(home, |g, s| {
        publication_transaction(g, |r| r.publish_start(&start, s, NOW))
    })
    .unwrap()
}
fn payload(scope: &[&str]) -> PreparedGooglePayload {
    PreparedGooglePayload::from_raw(json!({"provider_id":"google","access_token":"enc:v1:new-access","refresh_token":"enc:v1:new-refresh","expires_at":null,"scopes":scope})).unwrap()
}
fn bytes(home: &Path) -> Vec<u8> {
    std::fs::read(home.join(super::super::TOKEN_FILE)).unwrap()
}
#[test]
fn migration_changes_only_control_and_never_reads_or_creates_a_key() {
    let d = fixture();
    let probe = test_probe::Guard::install(d.path());
    let before: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    let a = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let after: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(before[0], after[0]);
    for k in ["access_token", "refresh_token", "expires_at", "scopes"] {
        assert_eq!(before[1][k], after[1][k]);
    }
    assert!(!d.path().join(".keyfile").exists());
    let unchanged = bytes(d.path());
    assert_eq!(
        a,
        home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap()
    );
    assert_eq!(unchanged, bytes(d.path()));
    home_source::validate_home_source(d.path(), &a).unwrap();
    assert_eq!(
        probe.counts(),
        [0, 0],
        "metadata must not enter OAuth crypto or provider code"
    );
}
#[test]
fn missing_invalid_static_metadata_refuses_before_control_or_payload_write() {
    for bad in [
        "missing-config",
        "bad-config",
        "missing-client",
        "bad-client",
        "duplicate-client",
        "missing-client-id",
    ] {
        let d = fixture();
        // A missing client is a refusal when this access token needs refresh.
        let mut expired: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        expired[1]["expires_at"] = json!("2000-01-01T00:00:00Z");
        std::fs::write(
            d.path().join(super::super::TOKEN_FILE),
            serde_json::to_vec(&expired).unwrap(),
        )
        .unwrap();
        let before = bytes(d.path());
        match bad {
            "missing-config" => std::fs::remove_file(d.path().join("config.toml")).unwrap(),
            "bad-config" => std::fs::write(d.path().join("config.toml"), "{broken").unwrap(),
            "missing-client" => {
                std::fs::remove_file(d.path().join(super::super::CLIENT_CONFIG_FILE)).unwrap()
            }
            "bad-client" => {
                std::fs::write(d.path().join(super::super::CLIENT_CONFIG_FILE), "{broken").unwrap()
            }
            "duplicate-client" => std::fs::write(
                d.path().join(super::super::CLIENT_CONFIG_FILE),
                "[{\"provider_id\":\"google\"},{\"provider_id\":\"google\"}]",
            )
            .unwrap(),
            _ => std::fs::write(
                d.path().join(super::super::CLIENT_CONFIG_FILE),
                "[{\"provider_id\":\"google\",\"client_id\":\"\"}]",
            )
            .unwrap(),
        }
        assert!(
            home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err(),
            "{bad}"
        );
        assert_eq!(before, bytes(d.path()), "{bad}");
        assert!(!d.path().join(".keyfile").exists());
    }
}
#[test]
fn unknown_schema_duplicate_google_and_corrupt_store_fail_without_replacement() {
    for bad in ["duplicate", "schema", "corrupt"] {
        let d = fixture();
        let path = d.path().join(super::super::TOKEN_FILE);
        if bad == "schema" {
            home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
        }
        let mut rows: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        if bad == "duplicate" {
            let copy = rows[1].clone();
            rows.as_array_mut().unwrap().push(copy);
        }
        if bad == "schema" {
            rows[1]["google_home"]["schema"] = json!(99);
        }
        std::fs::write(
            &path,
            if bad == "corrupt" {
                b"{bad".to_vec()
            } else {
                serde_json::to_vec(&rows).unwrap()
            },
        )
        .unwrap();
        let before = bytes(d.path());
        assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
        assert_eq!(before, bytes(d.path()));
    }
}
#[test]
fn start_pending_is_durable_and_never_an_issued_receipt_until_publication() {
    let d = fixture();
    let old = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let start = run(d.path(), |g, s| {
        publication_transaction(g, |r| {
            r.begin_connect(s, RawDigest::of(b"s"), NOW + 500, NOW)
        })
    })
    .unwrap();
    assert!(home_source::validate_home_source(d.path(), &old).is_err());
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    assert!(
        super::super::load_tokens(d.path())
            .iter()
            .all(|t| t.provider_id != "google")
    );
    run(d.path(), |g, s| {
        publication_transaction(g, |r| r.publish_start(&start, s, NOW))
    })
    .unwrap();
    assert!(home_source::validate_home_source(d.path(), &old).is_err());
}
#[test]
fn latest_start_and_single_claim_reject_copies_without_recapturing_the_client() {
    let d = fixture();
    let old = ticket(d.path());
    let new = ticket(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&old, s, NOW)))
        .is_err()
    );
    let claimed = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&new, s, NOW))
    })
    .unwrap();
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&new, s, NOW)))
        .is_err()
    );
    run(d.path(), |g, s| {
        transaction(g, |r| r.validate_claim(&claimed, s, NOW))
    })
    .unwrap();
    let client = d.path().join(super::super::CLIENT_CONFIG_FILE);
    let raw = std::fs::read_to_string(&client).unwrap();
    std::fs::write(client, raw.replace("fake-client", "different-client")).unwrap();
    let before = bytes(d.path());
    assert!(
        run(d.path(), |g, s| transaction(g, |r| r
            .validate_claim(&claimed, s, NOW)))
        .is_err()
    );
    assert_eq!(before, bytes(d.path()));
}
#[test]
fn expiry_and_revoke_tombstone_reject_pre_and_post_claim_aba() {
    let d = fixture();
    let t = ticket(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r.claim(
            &t,
            s,
            NOW + 501
        )))
        .is_err()
    );
    let c = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&t, s, NOW))
    })
    .unwrap();
    revoke_google(d.path()).unwrap();
    assert!(
        super::super::load_tokens(d.path())
            .iter()
            .all(|t| t.provider_id != "google")
    );
    let before = bytes(d.path());
    assert!(
        run(d.path(), |g, s| transaction(g, |r| r
            .validate_claim(&c, s, NOW)))
        .is_err()
    );
    assert_eq!(before, bytes(d.path()));
    let new = ticket(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&t, s, NOW)))
        .is_err()
    );
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&new, s, NOW)))
        .is_ok()
    );
}
#[test]
fn managed_google_generic_upsert_and_whole_save_cannot_resurrect_or_erase_tombstone() {
    let d = fixture();
    revoke_google(d.path()).unwrap();
    let before = bytes(d.path());
    let t = super::super::McpOAuthToken {
        provider_id: "google".into(),
        access_token: "must-not-publish".into(),
        refresh_token: None,
        expires_at: None,
        scopes: vec![],
    };
    assert!(super::super::upsert_token(d.path(), t).is_err());
    assert_eq!(before, bytes(d.path()));
    let google_before = raw_array::RawArray::read(&d.path().join(super::super::TOKEN_FILE))
        .unwrap()
        .element("google")
        .unwrap()
        .to_vec();
    super::super::save_tokens(d.path(), &[]).unwrap();
    assert_eq!(
        raw_array::RawArray::read(&d.path().join(super::super::TOKEN_FILE))
            .unwrap()
            .element("google")
            .unwrap(),
        google_before
    );
    assert!(super::super::get_stored_token(d.path(), "google").is_none());
}
#[test]
fn ordinary_non_google_upsert_preserves_raw_google_control_and_ciphertext() {
    let d = fixture();
    home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let before: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    super::super::upsert_token(
        d.path(),
        super::super::McpOAuthToken {
            provider_id: "other".into(),
            access_token: "".into(),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
        },
    )
    .unwrap();
    let after: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(before[1], after[1]);
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn refresh_cas_ignores_scope_order_but_fences_scope_changes_and_stale_lineage() {
    let d = fixture();
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let expected = run(d.path(), |g, s| transaction(g, |r| r.refresh_identity(s))).unwrap();
    run(d.path(), |g, s| {
        publication_transaction(g, |r| {
            r.finish_refresh(&expected, s, payload(&["scope-b", "scope-a"]))
        })
    })
    .unwrap();
    home_source::validate_home_source(d.path(), &receipt).unwrap();
    let before = bytes(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .finish_refresh(&expected, s, payload(&["scope-a"]))))
        .is_err()
    );
    assert_eq!(before, bytes(d.path()));
    let current = run(d.path(), |g, s| transaction(g, |r| r.refresh_identity(s))).unwrap();
    run(d.path(), |g, s| {
        publication_transaction(g, |r| r.finish_refresh(&current, s, payload(&["scope-a"])))
    })
    .unwrap();
    assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
    revoke_google(d.path()).unwrap();
    let before = bytes(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .finish_refresh(&current, s, payload(&["scope-a"]))))
        .is_err()
    );
    assert_eq!(before, bytes(d.path()));
}
#[test]
fn metadata_refuses_disabled_scope_and_configured_sa_instead_of_selecting_oauth() {
    let d = fixture();
    let before = bytes(d.path());
    assert!(home_source::prepare_home_selection(d.path(), &["missing-scope"]).is_err());
    assert_eq!(before, bytes(d.path()));
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    assert_eq!(before, bytes(d.path()));
    std::fs::write(d.path().join("config.toml"),"[integrations]\ngoogle_workspace=true\n[integrations.google_service_account]\nsubject='owner@example.test'\nkey_file='missing-key.json'\n").unwrap();
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    assert_eq!(before, bytes(d.path()));
}
#[test]
fn client_change_before_start_publication_keeps_pending_and_never_issues_ticket() {
    let d = fixture();
    let start = run(d.path(), |g, s| {
        publication_transaction(g, |r| {
            r.begin_connect(s, RawDigest::of(b"s"), NOW + 500, NOW)
        })
    })
    .unwrap();
    let path = d.path().join(super::super::CLIENT_CONFIG_FILE);
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("fake-client", "other-client")).unwrap();
    let before = bytes(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .publish_start(&start, s, NOW)))
        .is_err()
    );
    assert_eq!(before, bytes(d.path()));
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
}
#[test]
fn durable_master_disable_enable_aba_fences_receipts_and_old_callback() {
    let d = fixture();
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let t = ticket(d.path());
    for enabled in [false, true] {
        let change = run(d.path(), |g, s| {
            publication_transaction(g, |r| {
                r.fence_change(s, s.binding_with_master(enabled).clone())
            })
        })
        .unwrap();
        assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
        std::fs::write(
            d.path().join("config.toml"),
            format!("[integrations]\ngoogle_workspace={enabled}\n"),
        )
        .unwrap();
        run(d.path(), |g, s| {
            publication_transaction(g, |r| r.finish_change(&change, s, NOW))
        })
        .unwrap();
    }
    assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&t, s, NOW)))
        .is_err()
    );
    home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
}
#[test]
fn callback_finish_has_three_durable_points_and_only_expected_master_enable() {
    let d = fixture();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    let t = ticket(d.path());
    let claim = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&t, s, NOW))
    })
    .unwrap();
    let p = payload(&["scope-a"]);
    let before = std::fs::read(d.path().join("config.toml")).unwrap();
    let after = b"[integrations]\ngoogle_workspace=true\n".to_vec();
    let write = prepare_config_write(Some(&before), after).unwrap();
    let finish = run(d.path(), |g, s| {
        publication_transaction(g, |r| {
            r.begin_claimed_finish(&claim, s, &p, Some(&write), NOW)
        })
    })
    .unwrap();
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .finish_change(&finish, s, NOW)))
        .is_err()
    );
    run(d.path(), |g, s| {
        publication_transaction(g, |r| {
            r.publish_claimed_payload(&finish, &claim, s, p, Some(write), NOW)
        })
    })
    .unwrap();
    assert!(
        super::super::load_tokens(d.path())
            .iter()
            .all(|t| t.provider_id != "google")
    );
    run(d.path(), |g, s| {
        publication_transaction(g, |r| r.finish_change(&finish, s, NOW))
    })
    .unwrap();
    let ready = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    assert_ne!(ready.source_generation(), t.record.generation);
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&t, s, NOW)))
        .is_err()
    );
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn revoke_during_claimed_finish_prevents_payload_and_master_publication() {
    let d = fixture();
    let t = ticket(d.path());
    let claim = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&t, s, NOW))
    })
    .unwrap();
    let p = payload(&["scope-a"]);
    let finish = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.begin_claimed_finish(&claim, s, &p, None, NOW))
    })
    .unwrap();
    revoke_google(d.path()).unwrap();
    let before = bytes(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .publish_claimed_payload(&finish, &claim, s, p, None, NOW)))
        .is_err()
    );
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .finish_change(&finish, s, NOW)))
        .is_err()
    );
    assert_eq!(before, bytes(d.path()));
}
#[test]
fn unresolved_cross_file_mutation_cannot_be_made_ready_by_starting_another_connect() {
    let d = fixture();
    let change = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.fence_change(s, s.binding().clone()))
    })
    .unwrap();
    let before = bytes(d.path());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .begin_connect(s, RawDigest::of(b"new"), NOW + 500, NOW)))
        .is_err()
    );
    assert_eq!(before, bytes(d.path()));
    run(d.path(), |g, s| {
        publication_transaction(g, |r| r.finish_change(&change, s, NOW))
    })
    .unwrap();
}
#[test]
fn invalid_legacy_google_import_is_rejected_before_any_key_or_store_write() {
    let d = fixture();
    let t = super::super::McpOAuthToken {
        provider_id: "google".into(),
        access_token: "synthetic-import".into(),
        refresh_token: None,
        expires_at: None,
        scopes: vec![],
    };
    let before = bytes(d.path());
    assert!(super::super::save_tokens(d.path(), &[t.clone(), t]).is_err());
    assert_eq!(before, bytes(d.path()));
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn migration_raw_store_cas_refuses_a_changed_row_instead_of_overwriting_it() {
    let d = fixture();
    let result = run(d.path(), |g, s| {
        publication_transaction(g, |r| {
            let prepared = r.prepare(s)?;
            // Simulate an uncooperative writer between the raw read and publish.
            let mut actual: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
            actual[1]["access_token"] = json!("enc:v1:other-writer");
            std::fs::write(
                d.path().join(super::super::TOKEN_FILE),
                serde_json::to_vec(&actual).unwrap(),
            )
            .unwrap();
            Ok(prepared)
        })
    });
    assert!(result.is_err());
    let actual: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(actual[1]["access_token"], "enc:v1:other-writer");
    assert!(actual[1].get("google_home").is_none());
}
#[test]
fn valid_access_without_client_metadata_is_still_a_metadata_only_oauth_source() {
    let d = fixture();
    std::fs::remove_file(d.path().join(super::super::CLIENT_CONFIG_FILE)).unwrap();
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    home_source::validate_home_source(d.path(), &receipt).unwrap();
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .begin_connect(s, RawDigest::of(b"s"), NOW + 500, NOW)))
        .is_err()
    );
    assert!(!d.path().join(".keyfile").exists());
}
#[test]
fn refresh_cas_checks_actual_id_epoch_revision_and_generation_not_a_modified_expected_clone() {
    let d = fixture();
    home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let actual = run(d.path(), |g, s| transaction(g, |r| r.refresh_identity(s))).unwrap();
    for field in ["id", "epoch", "revision", "generation"] {
        let mut expected = actual.clone();
        match field {
            "id" => expected.credential.credential_id = id(),
            "epoch" => expected.credential.logical_epoch += 1,
            "revision" => expected.credential.token_revision += 1,
            _ => expected.generation = id(),
        };
        let before = bytes(d.path());
        assert!(
            run(d.path(), |g, s| publication_transaction(g, |r| r
                .finish_refresh(&expected, s, payload(&["scope-a"]))))
            .is_err()
        );
        assert_eq!(before, bytes(d.path()), "{field}");
    }
}
#[test]
fn generic_noop_fence_cannot_publish_staged_callback_payload_before_or_after_expiry() {
    for attempted_at in [NOW + 100, NOW + 501] {
        let d = fixture();
        let t = ticket(d.path());
        let claim = run(d.path(), |g, s| {
            publication_transaction(g, |r| r.claim(&t, s, NOW))
        })
        .unwrap();
        let prepared = payload(&["scope-a"]);
        let finish = run(d.path(), |g, s| {
            publication_transaction(g, |r| {
                r.begin_claimed_finish(&claim, s, &prepared, None, NOW)
            })
        })
        .unwrap();
        run(d.path(), |g, s| {
            publication_transaction(g, |r| {
                r.publish_claimed_payload(&finish, &claim, s, prepared, None, NOW)
            })
        })
        .unwrap();
        let before = bytes(d.path());
        let staged: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(staged[1]["access_token"], "enc:v1:new-access");
        assert_eq!(staged[1]["google_home"]["state"], "pending");

        let generic = run(d.path(), |g, s| {
            publication_transaction(g, |r| r.fence_change(s, s.binding().clone()))
        });
        // With the old bug, this completes Config without checking the
        // original callback's TTL and makes its staged payload public.
        if let Ok(change) = &generic {
            run(d.path(), |g, s| {
                publication_transaction(g, |r| r.finish_change(change, s, attempted_at))
            })
            .unwrap();
        }
        assert!(
            matches!(generic, Err(Error::NotReady)),
            "attempted_at={attempted_at}"
        );
        assert!(matches!(
            run(d.path(), |g, _| transaction(g, |r| r.metadata())),
            Err(Error::NotReady)
        ));
        assert_eq!(
            home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap_err(),
            Error::NotReady
        );
        assert!(
            super::super::load_tokens(d.path())
                .iter()
                .all(|t| t.provider_id != "google")
        );
        assert_eq!(
            before,
            bytes(d.path()),
            "pending control and staged ciphertext must stay unchanged"
        );
        assert!(!d.path().join(".keyfile").exists());
    }
}
#[test]
fn ordinary_ready_to_config_fence_still_completes_without_changing_ciphertext() {
    let d = fixture();
    let old = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let before: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    let change = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.fence_change(s, s.binding().clone()))
    })
    .unwrap();
    assert!(matches!(
        run(d.path(), |g, _| transaction(g, |r| r.metadata())),
        Err(Error::NotReady)
    ));
    run(d.path(), |g, s| {
        publication_transaction(g, |r| r.finish_change(&change, s, NOW))
    })
    .unwrap();
    let ready = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    assert_ne!(ready.source_generation(), old.source_generation());
    assert!(home_source::validate_home_source(d.path(), &old).is_err());
    let after: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(after[1]["google_home"]["state"], "ready");
    for field in ["access_token", "refresh_token", "expires_at", "scopes"] {
        assert_eq!(before[1][field], after[1][field]);
    }
    // Ready is metadata-only: the deliberately invalid cipher stays unchanged,
    // and the public decoder must still refuse its unusable secret material.
    assert!(
        super::super::load_tokens(d.path())
            .iter()
            .all(|t| t.provider_id != "google")
    );
    assert_eq!(before[0], after[0]);
    assert!(!d.path().join(".keyfile").exists());
}

#[path = "home_publication_tests.rs"]
mod publication_tests;

#[path = "home_lifecycle_tests.rs"]
mod lifecycle_tests;

#[path = "home_config_writer_tests.rs"]
mod config_writer_tests;

#[path = "home_public_api_tests.rs"]
mod public_api_tests;

#[path = "home_start_tests.rs"]
mod start_tests;

#[path = "home_callback_tests.rs"]
mod callback_tests;

#[test]
fn native_singleton_witness_is_exactly_the_existing_any_of_policy_for_all_http_tools() {
    let candidates = [
        "https://www.googleapis.com/auth/gmail.readonly",
        "https://www.googleapis.com/auth/gmail.modify",
        "https://www.googleapis.com/auth/gmail.compose",
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/calendar.events",
        "https://www.googleapis.com/auth/calendar.events.readonly",
        "https://www.googleapis.com/auth/calendar",
        "https://www.googleapis.com/auth/spreadsheets",
        "https://www.googleapis.com/auth/spreadsheets.readonly",
        "https://www.googleapis.com/auth/forms.body.readonly",
        "https://www.googleapis.com/auth/forms.body",
        "https://www.googleapis.com/auth/forms.responses.readonly",
        "https://www.googleapis.com/auth/tasks",
        "https://www.googleapis.com/auth/tasks.readonly",
        "https://www.googleapis.com/auth/drive.readonly",
        "https://www.googleapis.com/auth/drive",
        "https://www.googleapis.com/auth/documents",
        "https://www.googleapis.com/auth/documents.readonly",
        "https://www.googleapis.com/auth/presentations.readonly",
        "https://www.googleapis.com/auth/presentations",
        "https://www.googleapis.com/auth/userinfo.email",
        "scope-unrelated",
    ];
    for tool in duduclaw_core::google_identity::NATIVE_GOOGLE_TOOLS
        .iter()
        .copied()
        .filter(|t| *t != "google_status")
    {
        let mut inputs = vec![
            Vec::<String>::new(),
            candidates.iter().map(|s| s.to_string()).collect(),
        ];
        for a in candidates {
            for b in candidates {
                inputs.push(vec![b.into(), a.into(), b.into()]);
            }
        }
        for granted in inputs {
            let expected = crate::google_personal::scopes::permitted(tool, &granted);
            let witness = singleton_native_scope(tool, granted.clone());
            assert_eq!(witness.is_ok(), expected, "tool={tool}");
            if let Ok(witness) = witness {
                let mut sorted = granted;
                sorted.sort();
                sorted.dedup();
                let first = sorted
                    .into_iter()
                    .find(|s| {
                        crate::google_personal::scopes::permitted(tool, std::slice::from_ref(s))
                    })
                    .unwrap();
                assert_eq!(witness, first);
            }
        }
    }
    for tool in [
        "google_status",
        "mail_poll",
        "google_remote_mcp",
        "GMAIL_READ",
        "gmail_read ",
        "unknown",
    ] {
        assert!(
            singleton_native_scope(tool, candidates.iter().map(|s| s.to_string()).collect())
                .is_err()
        );
    }
}
