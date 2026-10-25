//! SOURCE-FIRST writer slice W1, NOT RUN. Owned metadata/ciphertext only.
use super::*;
use crate::mcp_oauth::home_lifecycle::{self, PreparedGoogleClient};
use crate::mcp_oauth::home_store::publication::{Fault, FaultGuard, GATE_FILE};
fn desired_client() -> PreparedGoogleClient {
    PreparedGoogleClient::from_encrypted_raw(json!({"provider_id":"google","client_id":"desired-client","client_secret":"enc:v1:fake-desired","auth_url":"https://accounts.google.com/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":["scope-a"]})).unwrap()
}
fn start_desired(home: &Path) -> Result<GoogleConnectTicket> {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    home_lifecycle::begin_prepared_google_home_connect(
        home,
        desired_client(),
        RawDigest::of(b"desired-state"),
        NOW + 500,
    )
}
#[test]
fn desired_first_client_connect_is_durable_before_issuing_a_ticket_and_preserves_non_google_raw() {
    let d = fixture();
    std::fs::write(
        d.path().join(super::super::super::CLIENT_CONFIG_FILE),
        br#"[{"provider_id":"github","client_id":"untouched","future":{"raw":true}}]"#,
    )
    .unwrap();
    let before_tokens: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    let t = start_desired(d.path()).unwrap();
    let clients: Value = serde_json::from_slice(
        &std::fs::read(d.path().join(super::super::super::CLIENT_CONFIG_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        clients[0],
        json!({"provider_id":"github","client_id":"untouched","future":{"raw":true}})
    );
    assert_eq!(clients[1]["client_id"], "desired-client");
    assert_eq!(clients[1]["client_secret"], "enc:v1:fake-desired");
    let after: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_eq!(before_tokens[0], after[0]);
    assert_eq!(before_tokens[1]["access_token"], after[1]["access_token"]);
    run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&t, s, NOW))
    })
    .unwrap();
    assert!(!d.path().join(".keyfile").exists());
    assert!(!d.path().join(GATE_FILE).exists());
}
#[test]
fn latest_desired_start_invalidates_previous_ticket_and_same_client_reconnect_fences_receipt() {
    let d = fixture();
    let old = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let first = start_desired(d.path()).unwrap();
    let second = start_desired(d.path()).unwrap();
    assert!(home_source::validate_home_source(d.path(), &old).is_err());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&first, s, NOW)))
        .is_err()
    );
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&second, s, NOW)))
        .is_ok()
    );
}
#[test]
fn each_client_durability_fault_returns_no_ticket_and_leaves_cross_process_google_blocked() {
    for fault in [
        Fault::SourceOpen,
        Fault::SourceWrite,
        Fault::SourceFileSync,
        Fault::SourceRename,
        Fault::SourceDirSync,
    ] {
        let d = fixture();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let f = FaultGuard::at(d.path(), fault);
        assert!(start_desired(d.path()).is_err(), "{fault:?}");
        drop(f);
        assert!(d.path().join(GATE_FILE).exists());
        let rows: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
        assert_eq!(rows[1]["google_home"]["state"], "pending");
        assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
        assert!(
            load_public(d.path())
                .unwrap()
                .iter()
                .all(|t| t.provider_id != "google")
        );
        super::publication_tests::assert_independent_google_blocked(d.path());
        let i = publication::inspect_publication(d.path()).unwrap();
        assert!(publication::stabilize_exact_publication(d.path(), &i).is_err());
        assert!(!d.path().join(".keyfile").exists());
    }
}
#[test]
fn gate_barrier_failure_precedes_both_pending_row_and_client_payload_writes() {
    let d = fixture();
    let tokens = bytes(d.path());
    let clients = std::fs::read(d.path().join(super::super::super::CLIENT_CONFIG_FILE)).unwrap();
    let f = FaultGuard::at(d.path(), Fault::GateFileSync);
    assert!(start_desired(d.path()).is_err());
    drop(f);
    assert_eq!(tokens, bytes(d.path()));
    assert_eq!(
        clients,
        std::fs::read(d.path().join(super::super::super::CLIENT_CONFIG_FILE)).unwrap()
    );
}

#[test]
fn client_whole_file_cas_refuses_unrelated_provider_change_before_any_pending_or_payload_write() {
    let d = fixture();
    let original_tokens = bytes(d.path());
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let result = run(d.path(), |g, observed| {
        let before = read_client_snapshot(g)?.unwrap();
        let mut rows: Vec<Value> = serde_json::from_slice(&before).unwrap();
        let mut target = rows[0].clone();
        target["client_id"] = json!("desired-client");
        let binding = observed.preview_google_client(&target)?;
        rows[0] = target;
        let write = prepare_client_write(
            Some(before.as_slice()),
            serde_json::to_vec_pretty(&rows).unwrap(),
        )?;
        publication_transaction(g, |r| {
            let planned = r.begin_connect_to(
                observed,
                binding,
                Some(write),
                RawDigest::of(b"s"),
                NOW + 500,
                NOW,
            )?;
            // Simulate a noncooperating writer touching only another provider.
            let mut actual: Vec<Value> = serde_json::from_slice(&before).unwrap();
            actual.push(json!({"provider_id":"github","future":"keep-external-change"}));
            std::fs::write(
                g.home().join(super::super::super::CLIENT_CONFIG_FILE),
                serde_json::to_vec(&actual).unwrap(),
            )
            .unwrap();
            Ok(planned)
        })
    });
    assert!(result.is_err());
    assert_eq!(original_tokens, bytes(d.path()));
    assert!(!d.path().join(GATE_FILE).exists());
    let clients: Value = serde_json::from_slice(
        &std::fs::read(d.path().join(super::super::super::CLIENT_CONFIG_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(clients[1]["future"], "keep-external-change");
    assert_ne!(clients[0]["client_id"], "desired-client");
}

#[test]
fn full_start_rejects_invalid_scopes_before_any_crypto_key_or_publication_write() {
    for scopes in [
        vec![String::new()],
        vec!["scope-a".to_string(); 129],
        vec!["s".repeat(2049)],
        vec!["scope\nwrite".to_string()],
    ] {
        let d = fixture();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let probe = test_probe::Guard::install(d.path());
        let before_tokens = bytes(d.path());
        let client_path = d.path().join(super::super::super::CLIENT_CONFIG_FILE);
        let before_client = std::fs::read(&client_path).unwrap();
        assert!(!d.path().join(GATE_FILE).exists());
        assert!(!d.path().join(".keyfile").exists());
        // Establish the static source and exact host deadline independently:
        // the negative must not pass because an unrelated preflight refused.
        let valid = json!({"provider_id":"google","client_id":"desired-client","client_secret":"fixture-secret-never-encrypted",
            "auth_url":"https://accounts.google.com/auth","token_url":"https://oauth2.googleapis.com/token",
            "redirect_uri":"http://127.0.0.1/callback","scopes":["scope-a"]});
        run(d.path(), |g, observed| {
            home_source::validate_google_client_raw(&valid)?;
            let _target = observed.preview_google_client(&valid)?;
            let now = host_time_now(g);
            assert_eq!(now, NOW);
            assert!(NOW + 500 > now && NOW + 500 <= now + 600);
            Ok(())
        })
        .unwrap();
        let desired = super::super::super::McpOAuthConfig {
            provider_id: "google".into(),
            client_id: "desired-client".into(),
            client_secret: "fixture-secret-never-encrypted".into(),
            auth_url: "https://accounts.google.com/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            redirect_uri: "http://127.0.0.1/callback".into(),
            scopes,
        };
        assert!(matches!(
            home_lifecycle::begin_google_home_connect(
                d.path(),
                &desired,
                RawDigest::of(b"full-state"),
                NOW + 500
            ),
            Err(Error::Invalid)
        ));
        assert_eq!(
            probe.counts(),
            [0, 0],
            "invalid scope must not reach crypto/provider call sites"
        );
        assert!(!d.path().join(".keyfile").exists());
        assert!(!d.path().join(GATE_FILE).exists());
        assert_eq!(before_tokens, bytes(d.path()));
        assert_eq!(before_client, std::fs::read(client_path).unwrap());
    }
}
