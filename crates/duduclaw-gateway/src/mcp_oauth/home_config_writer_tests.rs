//! SOURCE-FIRST W2a common writer tests. NOT COMPILED OR RUN.
use super::*;
use crate::handlers::config_commit;
use crate::mcp_oauth::home_store::publication::{Fault, FaultGuard, GATE_FILE};
fn config(home: &Path) -> String {
    std::fs::read_to_string(home.join("config.toml")).unwrap()
}
fn commit(home: &Path, next: &str) -> std::result::Result<(), String> {
    let path = home.join("config.toml");
    let old = config(home);
    config_commit::commit_text_locked(&path, &config_commit::content_hash(&old), next)
}
#[test]
fn public_integration_setter_and_enable_fence_master_aba_and_old_ticket() {
    let d = fixture();
    let probe = test_probe::Guard::install(d.path());
    let t = ticket(d.path());
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    home_source::validate_home_source(d.path(), &receipt).unwrap();
    assert!(crate::google_workspace::set_integration_enabled(d.path(), false).unwrap());
    assert!(crate::google_workspace::enable_integration(d.path()).unwrap());
    assert!(!crate::google_workspace::enable_integration(d.path()).unwrap());
    assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&t, s, NOW)))
        .is_err()
    );
    assert_eq!(probe.writer_counts()[0], 2);
    assert_eq!(probe.counts(), [0, 0]);
}
#[test]
fn generic_config_hook_preserves_format_and_unrelated_changes_do_not_rotate_google() {
    let d = fixture();
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let probe = test_probe::Guard::install(d.path());
    let token = bytes(d.path());
    commit(
        d.path(),
        "# retained\n[integrations]\ngoogle_workspace=true\n[general]\nlog_level='debug'\n",
    )
    .unwrap();
    home_source::validate_home_source(d.path(), &receipt).unwrap();
    assert_eq!(bytes(d.path()), token);
    assert!(config(d.path()).starts_with("# retained"));
    assert_eq!(probe.writer_counts()[0], 0);
}
#[test]
fn generic_config_expected_hash_or_policy_refusal_never_enters_source_mutation() {
    let d = fixture();
    let probe = test_probe::Guard::install(d.path());
    let old = config(d.path());
    let token = bytes(d.path());
    assert!(
        config_commit::commit_text_locked(
            &d.path().join("config.toml"),
            "stale",
            &old.replace("true", "false")
        )
        .is_err()
    );
    let denied = format!("{old}\n[product]\nprofile='restricted'\n");
    assert!(commit(d.path(), &denied).is_err());
    assert_eq!(config(d.path()), old);
    assert_eq!(bytes(d.path()), token);
    assert_eq!(probe.writer_counts(), [0, 0]);
}
#[test]
fn bridge_and_resolver_full_config_writers_fence_source_aba_without_decrypt_or_provider() {
    for extra in [
        "\n[integrations.google_apps_script]\nurl='https://script.google.com/macros/s/FAKE/exec'\nsecret='enc:v1:fake'\n",
        "\n[secret_manager]\nbackend='fixture-only'\n",
    ] {
        let d = fixture();
        let old = config(d.path());
        let probe = test_probe::Guard::install(d.path());
        let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
        commit(d.path(), &format!("{old}{extra}")).unwrap();
        commit(d.path(), &old).unwrap();
        assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
        assert_eq!(probe.writer_counts()[0], 2);
        assert_eq!(probe.counts(), [0, 0]);
    }
}
#[test]
fn every_config_payload_durability_fault_blocks_google_and_keeps_uncertain_pending() {
    for f in [
        Fault::SourceOpen,
        Fault::SourceWrite,
        Fault::SourceFileSync,
        Fault::SourceRename,
        Fault::SourceDirSync,
    ] {
        let d = fixture();
        let probe = test_probe::Guard::install(d.path());
        let fault = FaultGuard::at(d.path(), f);
        assert!(crate::google_workspace::set_integration_enabled(d.path(), false).is_err());
        drop(fault);
        assert!(d.path().join(GATE_FILE).exists());
        assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
        super::publication_tests::assert_independent_google_blocked(d.path());
        assert_eq!(probe.writer_counts(), [1, 1]);
        assert_eq!(probe.counts(), [0, 0]);
    }
}
#[test]
fn config_fence_cannot_erase_unresolved_connect_finish_even_after_expiry() {
    let d = oauth_fixture();
    let t = ticket(d.path());
    let claim = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&t, s, NOW))
    })
    .unwrap();
    let p = payload(&["scope-a"]);
    let _change = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.begin_claimed_finish(&claim, s, &p, None, NOW))
    })
    .unwrap();
    let old = config(d.path());
    let token = bytes(d.path());
    let _expired = publication::ClockGuard::after_preflight(NOW + 501, NOW + 501);
    home_source::with_source_guard(d.path(), |g| {
        assert_eq!(host_time_now(g), NOW + 501);
        Ok(())
    })
    .unwrap();
    assert!(crate::google_workspace::set_integration_enabled(d.path(), false).is_err());
    assert_eq!(old, config(d.path()));
    assert_eq!(token, bytes(d.path()));
}

#[test]
fn narrow_local_commit_preserves_caller_policy_and_rejects_stale_before_render() {
    let d = fixture();
    let old = config(d.path());
    let probe = test_probe::Guard::install(d.path());
    let entered = std::cell::Cell::new(false);
    assert!(
        crate::commit_config_rendered(d.path(), "stale", |_| {
            entered.set(true);
            Ok(old.clone())
        })
        .is_err()
    );
    assert!(!entered.get());
    // Local onboarding could already set its installation product profile;
    // the narrow final writer does not invent dashboard authorization there.
    let local = format!("{old}\n[product]\nprofile='restricted'\n");
    crate::commit_config_rendered(d.path(), &config_commit::content_hash(&old), |actual| {
        assert_eq!(actual, old);
        Ok(local)
    })
    .unwrap();
    assert!(config(d.path()).contains("profile='restricted'"));
    assert_eq!(probe.writer_counts()[0], 0);
}
#[test]
fn cold_unrelated_settings_never_create_google_control_or_require_client_metadata() {
    let d = tempfile::tempdir().unwrap();
    crate::commit_config_rendered(d.path(), &config_commit::content_hash(""), |_| {
        Ok("[general]\nlog_level='debug'\n".into())
    })
    .unwrap();
    assert!(!d.path().join(super::super::super::TOKEN_FILE).exists());
    assert!(
        !d.path()
            .join(super::super::super::CLIENT_CONFIG_FILE)
            .exists()
    );
    assert!(!d.path().join(".keyfile").exists());
    assert!(!d.path().join(GATE_FILE).exists());
    // A bad unrelated pre-existing Google client is not read/unsealed by a
    // pure other-section update and cannot cause a needless refusal.
    std::fs::write(
        d.path().join(super::super::super::CLIENT_CONFIG_FILE),
        b"{bad-client",
    )
    .unwrap();
    let old = config(d.path());
    crate::commit_config_rendered(d.path(), &config_commit::content_hash(&old), |_| {
        Ok(format!("{old}\n[backup]\nretention_count=3\n"))
    })
    .unwrap();
    assert!(!d.path().join(super::super::super::TOKEN_FILE).exists());
}

#[test]
fn config_gate_barrier_failure_cannot_write_config_or_token_pending() {
    let d = fixture();
    let before = config(d.path());
    let token = bytes(d.path());
    let probe = test_probe::Guard::install(d.path());
    let fault = FaultGuard::at(d.path(), Fault::GateFileSync);
    assert!(crate::google_workspace::set_integration_enabled(d.path(), false).is_err());
    drop(fault);
    assert_eq!(before, config(d.path()));
    assert_eq!(token, bytes(d.path()));
    assert_eq!(probe.writer_counts(), [1, 0]);
}

#[test]
fn service_account_config_source_aba_hashes_owned_bytes_without_loading_a_private_key() {
    let d = fixture();
    let key = d.path().join("source-only-key.json");
    std::fs::write(&key, b"source-only-not-a-private-key").unwrap();
    let old = config(d.path());
    let probe = test_probe::Guard::install(d.path());
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let next = format!(
        "{old}\n[integrations.google_service_account]\nkey_file='source-only-key.json'\nsubject='fixture@example.test'\n"
    );
    commit(d.path(), &next).unwrap();
    assert!(
        home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err(),
        "configured SA must not silently become OAuth"
    );
    commit(d.path(), &old).unwrap();
    assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
    assert_eq!(probe.writer_counts()[0], 2);
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!d.path().join(".keyfile").exists());
}

#[test]
fn actual_whole_table_snapshot_helper_cannot_restore_intervening_google_sources() {
    for extra in [
        "master",
        "\n[integrations.google_service_account]\nkey_file='source-only-key.json'\nsubject='fixture@example.test'\n",
        "\n[integrations.google_apps_script]\nurl='https://script.google.com/macros/s/FAKE/exec'\nsecret='enc:v1:fake'\n",
        "\n[secret_manager]\nbackend='fixture-only'\n",
    ] {
        let d = fixture();
        std::fs::write(d.path().join("source-only-key.json"), b"not-a-key").unwrap();
        let path = d.path().join("config.toml");
        // This is the same producer and final helper used by the nine RPC
        // whole-table callers and their shared helper, not a supplied hash.
        let (mut stale, snapshot) = config_commit::read_home_table_snapshot(&path).unwrap();
        let mut identity = toml::Table::new();
        identity.insert("provider".into(), toml::Value::String("wiki".into()));
        stale.insert("identity".into(), toml::Value::Table(identity));
        let a = config(d.path());
        let b = if extra == "master" {
            a.replace("true", "false")
        } else {
            format!("{a}{extra}")
        };
        commit(d.path(), &b).unwrap();
        let winner = config(d.path());
        let token = bytes(d.path());
        let probe = test_probe::Guard::install(d.path());
        assert!(config_commit::commit_home_table_snapshot(&path, snapshot, &stale).is_err());
        assert_eq!(config(d.path()), winner);
        assert_eq!(bytes(d.path()), token);
        assert_eq!(probe.writer_counts(), [0, 0]);
        assert_eq!(probe.counts(), [0, 0]);
    }
}

#[test]
fn actual_whole_table_snapshot_helper_retains_accounts_guard_and_format() {
    let d = fixture();
    let path = d.path().join("config.toml");
    let (mut table, snapshot) = config_commit::read_home_table_snapshot(&path).unwrap();
    table.insert("accounts".into(), toml::Value::Array(vec![]));
    let old = config(d.path());
    assert!(config_commit::commit_home_table_snapshot(&path, snapshot, &table).is_err());
    assert_eq!(config(d.path()), old);
    let (mut table, snapshot) = config_commit::read_home_table_snapshot(&path).unwrap();
    // A real identity update supplies a field. An empty implicit table may
    // use the renderer's documented semantic-safe pretty-print fallback.
    let mut identity = toml::Table::new();
    identity.insert("provider".into(), toml::Value::String("wiki_cache".into()));
    table.insert("identity".into(), toml::Value::Table(identity));
    config_commit::commit_home_table_snapshot(&path, snapshot, &table).unwrap();
    let rendered = config(d.path());
    assert!(rendered.starts_with(&old));
    assert_eq!(rendered.parse::<toml::Table>().unwrap(), table);
}

#[test]
fn reset_rendered_public_commit_fences_old_receipt_and_ticket() {
    let d = fixture();
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let t = ticket(d.path());
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    let original = config(d.path());
    let probe = test_probe::Guard::install(d.path());
    crate::commit_config_rendered(d.path(), &config_commit::content_hash(&original), |_| {
        Ok("# reset wizard replacement\n[general]\nlog_level='debug'\n".into())
    })
    .unwrap();
    assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .claim(&t, s, NOW)))
        .is_err()
    );
    assert_eq!(probe.writer_counts()[0], 1);
    assert_eq!(probe.counts(), [0, 0]);
}
#[test]
fn reset_rendered_public_commit_propagates_durability_fault_without_retry() {
    for fault in [
        Fault::GateFileSync,
        Fault::DataFileSync,
        Fault::SourceFileSync,
        Fault::SourceDirSync,
    ] {
        let d = fixture();
        let _clock = publication::ClockGuard::fixed_if_unset(NOW);
        let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
        let original = config(d.path());
        let token = bytes(d.path());
        let probe = test_probe::Guard::install(d.path());
        let f = FaultGuard::at(d.path(), fault);
        assert!(
            crate::commit_config_rendered(
                d.path(),
                &config_commit::content_hash(&original),
                |_| { Ok("# reset wizard replacement\n[general]\nlog_level='debug'\n".into()) }
            )
            .is_err()
        );
        drop(f);
        assert!(d.path().join(GATE_FILE).exists());
        assert!(home_source::validate_home_source(d.path(), &receipt).is_err());
        assert_eq!(probe.writer_counts()[0], 1);
        assert_eq!(probe.counts(), [0, 0]);
        if fault == Fault::GateFileSync {
            assert_eq!(config(d.path()), original);
            assert_eq!(bytes(d.path()), token);
            assert_eq!(probe.writer_counts()[1], 0);
        }
    }
}
