//! Phase A source-first declarations. No compiler/runtime evidence.
use super::*;
use serde_json::json;

fn fixture(scopes: &[&str]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=true\n",
    )
    .unwrap();
    let google_home = crate::mcp_oauth::google_home_test_fixture::current_control("test-admin");
    std::fs::write(d.path().join(crate::mcp_oauth::TOKEN_FILE), serde_json::to_vec(&json!([
        {"provider_id":"google","access_token":"owned-native-access","refresh_token":null,"expires_at":null,"scopes":scopes,"unknown":{"kept":true},"google_home":google_home},
        {"provider_id":"github","access_token":"foreign","refresh_token":null,"expires_at":null,"scopes":["repo"]}
    ])).unwrap()).unwrap();
    d
}
fn rows(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(home.join(crate::mcp_oauth::TOKEN_FILE)).unwrap())
        .unwrap()
}
fn change_scopes(home: &Path, scopes: &[&str]) {
    let mut raw = rows(home);
    raw[0]["scopes"] = json!(scopes);
    std::fs::write(
        home.join(crate::mcp_oauth::TOKEN_FILE),
        serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
}

#[test]
fn native_preparation_uses_actual_granted_readonly_alternative_not_client_requested_scopes() {
    let d = fixture(&["https://www.googleapis.com/auth/spreadsheets.readonly"]);
    std::fs::write(d.path().join(crate::mcp_oauth::CLIENT_CONFIG_FILE), serde_json::to_vec(&json!([
        {"provider_id":"google","client_id":"native-client","client_secret":"enc:v1:unusable","auth_url":"https://accounts.google.com/o/oauth2/v2/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":crate::google_workspace::REQUIRED_SCOPES}
    ])).unwrap()).unwrap();
    let before = rows(d.path());
    let selected = prepare_home_native_selection(d.path(), "sheets_read").unwrap();
    let NativeHomePurpose::Http {
        tool,
        singleton_scope,
    } = &selected.purpose
    else {
        panic!("expected HTTP metadata purpose");
    };
    assert_eq!(tool, "sheets_read");
    assert_eq!(
        singleton_scope,
        "https://www.googleapis.com/auth/spreadsheets.readonly"
    );
    assert_eq!(selected.receipt.scopes(), &[singleton_scope.clone()]);
    let after = rows(d.path());
    assert_eq!(before[1], after[1]);
    for field in [
        "access_token",
        "refresh_token",
        "expires_at",
        "scopes",
        "unknown",
    ] {
        assert_eq!(before[0][field], after[0][field]);
    }
    assert!(after[0]["google_home"]["credential"]["token_revision"].is_u64());
    let stable = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    validate_home_native_selection(d.path(), &selected, "sheets_read").unwrap();
    assert!(prepare_home_native_selection(d.path(), "sheets_append").is_err());
    assert_eq!(
        stable,
        std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap()
    );
    assert!(!d.path().join(".keyfile").exists());
}

#[tokio::test]
async fn native_resolve_and_pure_validator_use_the_same_original_receipt_and_tool() {
    let d = fixture(&["https://www.googleapis.com/auth/gmail.readonly"]);
    let selected = prepare_home_native_selection(d.path(), "gmail_read").unwrap();
    let material = resolve_home_native_pinned(d.path(), &selected, "gmail_read")
        .await
        .unwrap();
    validate_home_native_material(d.path(), &selected, "gmail_read", material.proof()).unwrap();
    let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    assert!(
        resolve_home_native_pinned(d.path(), &selected, "gmail_search")
            .await
            .is_err()
    );
    assert!(
        validate_home_native_material(d.path(), &selected, "gmail_search", material.proof())
            .is_err()
    );
    assert_eq!(
        before,
        std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap()
    );
    assert!(!d.path().join(".keyfile").exists());
}

#[tokio::test]
async fn status_is_metadata_only_and_cannot_enter_the_material_resolver() {
    let d = fixture(&[]);
    // Even an unusable ciphertext can be observed as logical metadata;
    // a status operation must not try to decrypt it or acquire a refresh mutex.
    let mut raw = rows(d.path());
    raw[0]["access_token"] = json!("enc:v1:unusable-access");
    std::fs::write(
        d.path().join(crate::mcp_oauth::TOKEN_FILE),
        serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    let probe = home_store::test_probe::Guard::install(d.path());
    let selected = prepare_home_native_selection(d.path(), "google_status").unwrap();
    assert!(selected.is_metadata_status("google_status").unwrap());
    validate_home_native_selection(d.path(), &selected, "google_status").unwrap();
    assert!(
        resolve_home_native_pinned(d.path(), &selected, "google_status")
            .await
            .is_err()
    );
    assert_eq!(testing::resolver_entries(d.path()), 0);
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!d.path().join(".keyfile").exists());
    assert!(!d.path().join("locks/google-grants.lock").exists());
    assert!(selected.is_metadata_status("gmail_read").is_err());
}

#[tokio::test]
async fn status_material_validation_rejects_before_entering_c1_even_with_a_real_other_proof() {
    let d = fixture(&["https://www.googleapis.com/auth/gmail.readonly"]);
    let http = prepare_home_native_selection(d.path(), "gmail_read").unwrap();
    let material = resolve_home_native_pinned(d.path(), &http, "gmail_read")
        .await
        .unwrap();
    let status = prepare_home_native_selection(d.path(), "google_status").unwrap();
    let entries = testing::validator_entries(d.path());
    assert!(
        validate_home_native_material(d.path(), &status, "google_status", material.proof())
            .is_err()
    );
    assert_eq!(testing::validator_entries(d.path()), entries);
}

#[test]
fn aliases_unknown_and_remote_names_do_not_prepare_native_receipts() {
    for tool in [
        "mail_poll",
        "google_remote_mcp",
        "GMAIL_READ",
        "gmail_read ",
        "unknown",
    ] {
        let d = fixture(crate::google_workspace::REQUIRED_SCOPES);
        let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
        assert!(prepare_home_native_selection(d.path(), tool).is_err());
        assert_eq!(
            before,
            std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap()
        );
        assert!(!d.path().join(".google-home-source.lock").exists());
    }
}

#[tokio::test]
async fn disappearing_witness_is_not_replaced_with_another_legal_alternative() {
    let d = fixture(&[
        "https://www.googleapis.com/auth/spreadsheets",
        "https://www.googleapis.com/auth/spreadsheets.readonly",
    ]);
    let selected = prepare_home_native_selection(d.path(), "sheets_read").unwrap();
    let NativeHomePurpose::Http {
        singleton_scope, ..
    } = &selected.purpose
    else {
        panic!("HTTP metadata");
    };
    assert_eq!(
        singleton_scope,
        "https://www.googleapis.com/auth/spreadsheets"
    );
    change_scopes(
        d.path(),
        &["https://www.googleapis.com/auth/spreadsheets.readonly"],
    );
    let changed = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
    assert!(validate_home_native_selection(d.path(), &selected, "sheets_read").is_err());
    assert!(
        resolve_home_native_pinned(d.path(), &selected, "sheets_read")
            .await
            .is_err()
    );
    assert_eq!(
        changed,
        std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap()
    );
    assert!(!d.path().join(".keyfile").exists());
}

#[test]
fn scope_order_and_duplicates_do_not_change_the_prepared_singleton_witness() {
    let d = fixture(&[
        "https://www.googleapis.com/auth/gmail.readonly",
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/gmail.readonly",
    ]);
    let selected = prepare_home_native_selection(d.path(), "gmail_read").unwrap();
    let NativeHomePurpose::Http {
        singleton_scope, ..
    } = &selected.purpose
    else {
        panic!("HTTP metadata");
    };
    assert_eq!(singleton_scope, "https://mail.google.com/");
    change_scopes(
        d.path(),
        &[
            "https://mail.google.com/",
            "https://www.googleapis.com/auth/gmail.readonly",
        ],
    );
    validate_home_native_selection(d.path(), &selected, "gmail_read").unwrap();
    // This is logical selection validation, not permission to reuse an old
    // exact C1 material proof after any payload mutation.
}

#[test]
fn malformed_actual_grants_fail_before_metadata_migration() {
    for bad in [
        json!(null),
        json!([7]),
        json!(["bad\nscope"]),
        json!(vec!["same"; 129]),
    ] {
        let d = fixture(&[]);
        let mut raw = rows(d.path());
        raw[0]["scopes"] = bad;
        std::fs::write(
            d.path().join(crate::mcp_oauth::TOKEN_FILE),
            serde_json::to_vec(&raw).unwrap(),
        )
        .unwrap();
        let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
        assert!(prepare_home_native_selection(d.path(), "gmail_read").is_err());
        assert_eq!(
            before,
            std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap()
        );
    }
}

#[test]
fn legacy_source_never_issues_a_native_selection_even_with_publication_faults() {
    for fault in [
        home_store::oauth_testing::Fault::GateWrite,
        home_store::oauth_testing::Fault::DataRename,
        home_store::oauth_testing::Fault::CleanupDirSync,
    ] {
        let d = fixture(&["https://www.googleapis.com/auth/gmail.readonly"]);
        let mut raw = rows(d.path());
        raw[0].as_object_mut().unwrap().remove("google_home");
        std::fs::write(
            d.path().join(crate::mcp_oauth::TOKEN_FILE),
            serde_json::to_vec(&raw).unwrap(),
        )
        .unwrap();
        let _fault = home_store::oauth_testing::FaultGuard::at(d.path(), fault);
        assert!(prepare_home_native_selection(d.path(), "gmail_read").is_err());
        assert!(!d.path().join(".keyfile").exists());
    }
}

#[test]
fn master_off_sa_precedence_and_missing_oauth_never_choose_a_different_backend() {
    for config in [
        "[integrations]\ngoogle_workspace=false\n",
        "[integrations]\ngoogle_workspace=true\n[integrations.google_service_account]\nkey_file='missing.json'\nsubject='unit@example.test'\n",
    ] {
        let d = fixture(&["https://www.googleapis.com/auth/gmail.readonly"]);
        std::fs::write(d.path().join("config.toml"), config).unwrap();
        let before = std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap();
        assert!(prepare_home_native_selection(d.path(), "gmail_read").is_err());
        assert_eq!(
            before,
            std::fs::read(d.path().join(crate::mcp_oauth::TOKEN_FILE)).unwrap()
        );
    }
    let d = fixture(&[]);
    std::fs::write(d.path().join(crate::mcp_oauth::TOKEN_FILE), "[]").unwrap();
    assert!(prepare_home_native_selection(d.path(), "gmail_read").is_err());
}
