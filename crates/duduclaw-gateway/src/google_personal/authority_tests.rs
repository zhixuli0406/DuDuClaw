//! Metadata-only authority tests. No network, browser or system keychain.
use super::*;
use crate::google_identity::requester_gate::UserDbPeople;
use crate::person_credentials::{self as vault, Metadata, Secret, Status};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_core::google_identity::CredentialKind;
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<UserDb>, String) {
    let home = tempfile::tempdir().unwrap();
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    crate::handlers::credential_protection_rpc::protection_for(home.path())
        .enable(false)
        .unwrap();
    crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
    std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=true\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='anyone'\n",
    )
    .unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let user = db
        .create_user(
            "login@unit.test",
            "Alice",
            "fixture-password-only",
            UserRole::Employee,
        )
        .unwrap();
    db.bind_agent(&user.id, "bot", AccessLevel::Owner).unwrap();
    db.set_google_consent(&user.id, "bot", true, &user.id)
        .unwrap();
    crate::mcp_oauth::upsert_client_config(
        home.path(),
        crate::mcp_oauth::McpOAuthClientConfig {
            provider_id: "google".into(),
            client_id: "unit-client".into(),
            client_secret: "unit-client-secret".into(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scopes: vec![],
            redirect_uri: "http://127.0.0.1:18789/api/mcp/oauth/callback".into(),
        },
    )
    .unwrap();
    let metadata = Metadata {
        owner_kind: "user".into(),
        owner_id: user.id.clone(),
        provider: "google".into(),
        credential_id: uuid::Uuid::new_v4().to_string(),
        revision: 1,
        account_email: "alice@unit.test".into(),
        google_sub: "unit-sub".into(),
        hosted_domain: None,
        granted_scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".into()],
        status: Status::Connected,
        status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(),
        last_refresh_at: None,
        oauth_client_id: "unit-client".into(),
        google_data_consent: Some(crate::google_data_consent::Evidence::issue(&user.id).unwrap()),
    };
    let epoch = vault::begin_connection(home.path(), &user.id).unwrap();
    let key = crate::google_identity::protection_gate(home.path()).unwrap();
    vault::connect(
        home.path(),
        &key,
        metadata,
        &Secret {
            access_token: "unit-access-never-output".into(),
            refresh_token: Some("unit-refresh-never-output".into()),
            expires_at: Some(4_000_000_000),
        },
        epoch,
    )
    .unwrap();
    (home, db, user.id)
}

#[test]
fn selection_uses_actual_tool_and_rejects_unknown_owner_only_and_consent_aba() {
    let (home, db, user) = fixture();
    let people = UserDbPeople(db.clone());
    let who = RequesterIdentity::Unknown;
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    assert_eq!(selected.account_email(), "alice@unit.test");
    assert!(
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_create_draft")
            .is_err()
    );
    assert!(
        PersonalSelection::capture(home.path(), "bot", &who, &people, "google_remote_mcp").is_err()
    );
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_ok()
    );
    db.set_google_consent(&user, "bot", false, &user).unwrap();
    db.set_google_consent(&user, "bot", true, &user).unwrap();
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
    for policy in ["bound_people", "owner_only"] {
        let path = home.path().join("agents/bot/agent.toml");
        std::fs::write(path, format!("[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='{policy}'\nfallback='home'\n")).unwrap();
        let refused = PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read")
            .unwrap_err();
        assert_eq!(refused.code, "requester_refused");
        assert!(!refused.fallback_eligible);
    }
}

#[test]
fn legacy_personal_token_and_clear_metadata_consent_tamper_cannot_be_selected() {
    let (home, db, user) = fixture();
    let current = vault::disconnect(home.path(), &user).unwrap().unwrap();
    let mut legacy = current.metadata;
    legacy.credential_id = uuid::Uuid::new_v4().to_string();
    legacy.google_data_consent = None;
    let key = crate::google_identity::protection_gate(home.path()).unwrap();
    let epoch = vault::begin_connection(home.path(), &user).unwrap();
    vault::connect(
        home.path(),
        &key,
        legacy,
        &Secret {
            access_token: "legacy-access-never-output".into(),
            refresh_token: Some("legacy-refresh-never-output".into()),
            expires_at: Some(4_000_000_000),
        },
        epoch,
    )
    .unwrap();

    let people = UserDbPeople(db.clone());
    let who = RequesterIdentity::Unknown;
    assert_eq!(
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read")
            .unwrap_err()
            .code,
        "needs_reconnect"
    );

    // Clear metadata is not authority. Adding current-looking consent without
    // resealing the binding must still fail before a bearer is returned.
    let path = vault::path(home.path());
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw["records"][0]["google_data_consent"] =
        serde_json::to_value(crate::google_data_consent::Evidence::issue(&user).unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_ok()
    );
    let tampered = vault::get(home.path(), &user).unwrap().unwrap();
    assert!(vault::unseal(&key, &tampered).is_err());

    // Cleanup remains possible even though normal selection is fenced.
    assert!(vault::disconnect(home.path(), &user).unwrap().is_some());
}

#[test]
fn call_binding_fences_employee_key_tool_args_expiry_and_redacts_all_secrets() {
    let (home, db, _) = fixture();
    let people = UserDbPeople(db);
    let selected = PersonalSelection::capture(
        home.path(),
        "bot",
        &RequesterIdentity::Unknown,
        &people,
        "gmail_read",
    )
    .unwrap();
    let reference = CredentialRef {
        kind: CredentialKind::Legacy,
        sha256: "a".repeat(64),
    };
    let args = serde_json::json!({"message_id":"unit"});
    let binding = PersonalCallBinding::new(selected, "bot", &reference, "gmail_read", &args, 100);
    assert!(binding.matches("bot", &reference, "gmail_read", &args, 101));
    assert!(!binding.matches("other", &reference, "gmail_read", &args, 101));
    assert!(!binding.matches(
        "bot",
        &CredentialRef {
            kind: CredentialKind::Legacy,
            sha256: "b".repeat(64)
        },
        "gmail_read",
        &args,
        101
    ));
    assert!(!binding.matches("bot", &reference, "gmail_search", &args, 101));
    assert!(!binding.matches(
        "bot",
        &reference,
        "gmail_read",
        &serde_json::json!({"message_id":"other"}),
        101
    ));
    assert!(!binding.matches("bot", &reference, "gmail_read", &args, 100 + 1800));
    let wire = serde_json::to_string(&binding).unwrap();
    assert!(!wire.contains("unit-access-never-output"));
    assert!(!wire.contains("unit-refresh-never-output"));
    assert!(!wire.contains("unit-client-secret"));
    assert!(!wire.contains("pk:v1:"));
}

#[test]
fn disconnected_fallback_selection_cannot_turn_into_a_later_account() {
    let (home, db, user) = fixture();
    let path = home.path().join("agents/bot/agent.toml");
    std::fs::write(
        path,
        "[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='anyone'\nfallback='home'\n",
    )
    .unwrap();
    vault::disconnect(home.path(), &user).unwrap();
    let people = UserDbPeople(db);
    let selected = PersonalSelection::capture(
        home.path(),
        "bot",
        &RequesterIdentity::Unknown,
        &people,
        "gmail_read",
    )
    .unwrap();
    assert!(selected.configured_home_fallback());
    assert!(
        selected
            .validate(
                home.path(),
                "bot",
                &RequesterIdentity::Unknown,
                &people,
                "gmail_read"
            )
            .is_ok()
    );
    // Even a new OAuth attempt, before publication, invalidates the account
    // selection that the human saw as unavailable/Home fallback.
    vault::begin_connection(home.path(), &user).unwrap();
    assert!(
        selected
            .validate(
                home.path(),
                "bot",
                &RequesterIdentity::Unknown,
                &people,
                "gmail_read"
            )
            .is_err()
    );
}

#[test]
fn remote_scope_requires_every_scope_and_public_visitors_cannot_borrow_anyone() {
    let (home, db, _) = fixture();
    let people = UserDbPeople(db);
    for channel in ["widget", "odoolivechat"] {
        let who = RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: "unit".into(),
            sender: "unit".into(),
            user_id: None,
        };
        assert_eq!(
            PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read")
                .unwrap_err()
                .code,
            "requester_refused"
        );
    }
    for omitted in crate::google_workspace::REQUIRED_SCOPES {
        let scopes: Vec<String> = crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .filter(|s| *s != omitted)
            .map(|s| s.to_string())
            .collect();
        assert!(!crate::google_personal::scopes::permitted(
            "google_remote_mcp",
            &scopes
        ));
    }
    let all: Vec<String> = crate::google_workspace::REQUIRED_SCOPES
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(crate::google_personal::scopes::permitted(
        "google_remote_mcp",
        &all
    ));
}

#[test]
fn token_authority_refuses_refresh_while_approval_selection_remains_valid() {
    let (home, db, user) = fixture();
    let people = UserDbPeople(db);
    let who = RequesterIdentity::Unknown;
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    let record = vault::get(home.path(), &user).unwrap().unwrap();
    let authority = PersonalAuthority::new(selected.clone(), record.metadata.revision);
    assert!(
        authority
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_ok()
    );
    let key = crate::google_identity::protection_gate(home.path()).unwrap();
    vault::finish_refresh(
        home.path(),
        &key,
        &record.metadata,
        &Secret {
            access_token: "unit-new-bearer".into(),
            refresh_token: Some("unit-new-refresh".into()),
            expires_at: Some(4_000_000_000),
        },
    )
    .unwrap();
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_ok(),
        "same grant does not need fresh approval"
    );
    assert!(
        authority
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_err(),
        "client holding old bearer needs rebuild"
    );
}

#[test]
fn primary_owner_and_config_aba_require_a_new_selection() {
    let (home, db, alice) = fixture();
    let bob = db
        .create_user(
            "bob@unit.test",
            "Bob",
            "fixture-password-only",
            UserRole::Employee,
        )
        .unwrap();
    let admin = db
        .create_user(
            "admin@unit.test",
            "Admin",
            "fixture-password-only",
            UserRole::Admin,
        )
        .unwrap();
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    db.set_google_primary_owner("bot", &alice, &admin.id)
        .unwrap();
    let people = UserDbPeople(db.clone());
    let who = RequesterIdentity::Unknown;
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    db.set_google_primary_owner("bot", &bob.id, &admin.id)
        .unwrap();
    db.set_google_primary_owner("bot", &alice, &admin.id)
        .unwrap();
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    let path = home.path().join("agents/bot/agent.toml");
    let original = std::fs::read(&path).unwrap();
    // Product writers replace the file, so identical content in a new file
    // is still a new config incarnation and cannot restore an old receipt.
    let replacement = path.with_extension("replacement");
    std::fs::write(&replacement, &original).unwrap();
    std::fs::rename(replacement, path).unwrap();
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
}

#[test]
fn selected_account_reconnect_and_client_config_replacement_never_restore_authority() {
    let (home, db, user) = fixture();
    let people = UserDbPeople(db);
    let who = RequesterIdentity::Unknown;
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    let old = vault::get(home.path(), &user).unwrap().unwrap();
    let key = crate::google_identity::protection_gate(home.path()).unwrap();
    let epoch = vault::begin_connection(home.path(), &user).unwrap();
    // Reconnecting the same visible account, even with an intentionally
    // restored credential id, remains a different grant incarnation.
    vault::connect(
        home.path(),
        &key,
        old.metadata,
        &Secret {
            access_token: "unit-reconnected-bearer".into(),
            refresh_token: Some("unit-reconnected-refresh".into()),
            expires_at: Some(4_000_000_000),
        },
        epoch,
    )
    .unwrap();
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    let client = crate::mcp_oauth::get_client_config(home.path(), "google").unwrap();
    crate::mcp_oauth::upsert_client_config(home.path(), client).unwrap();
    assert!(
        selected
            .validate(home.path(), "bot", &who, &people, "gmail_read")
            .is_err(),
        "client config replacement requires re-selection even when values are identical"
    );
}

#[tokio::test]
async fn native_resumed_user_first_call_keeps_personal_identity_with_and_without_home_fallback() {
    for fallback in ["none", "home"] {
        let (home, db, user) = fixture();
        std::fs::write(home.path().join("agents/bot/agent.toml"), format!("[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='anyone'\nfallback='{fallback}'\n")).unwrap();
        let people = UserDbPeople(db.clone());
        let who = RequesterIdentity::Unknown;
        db.set_user_status(&user, duduclaw_auth::UserStatus::Suspended)
            .unwrap();
        let error = crate::google_identity::resolve::resolve_person_for_tool(
            home.path(),
            "bot",
            &who,
            &people,
            "gmail_read",
        )
        .await;
        assert!(
            error.is_err()
                || matches!(
                    error,
                    Ok(crate::google_identity::resolve::Resolved::UseHome { .. })
                )
        );
        assert_eq!(
            vault::get(home.path(), &user)
                .unwrap()
                .unwrap()
                .metadata
                .status,
            Status::Suspended
        );
        db.set_user_status(&user, duduclaw_auth::UserStatus::Active)
            .unwrap();
        let resolved = crate::google_identity::resolve::resolve_person_for_tool(
            home.path(),
            "bot",
            &who,
            &people,
            "gmail_read",
        )
        .await;
        assert!(
            matches!(resolved, Ok(crate::google_identity::resolve::Resolved::Personal { ref user_id, .. }) if user_id == &user),
            "first resumed call must use Personal, fallback={fallback}: {resolved:?}"
        );
    }
}

#[test]
#[cfg(unix)]
fn canonical_home_alias_keeps_the_gateway_recorded_edition() {
    let (home, _, _) = fixture();
    let alias_parent = tempfile::tempdir().unwrap();
    let alias = alias_parent.path().join("home-alias");
    std::os::unix::fs::symlink(home.path(), &alias).unwrap();
    assert!(
        crate::team_edition::team_grants_enabled(&alias),
        "edition must be keyed by canonical home, not path spelling"
    );
}

fn configure_home_fallback(home: &Path) {
    std::fs::write(
        home.join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='anyone'\nfallback='home'\n",
    )
    .unwrap();
}

#[test]
fn original_unavailable_validator_is_read_only_and_keeps_the_original_failure_fact() {
    let (home, db, user) = fixture();
    configure_home_fallback(home.path());
    vault::disconnect(home.path(), &user).unwrap();
    let people = UserDbPeople(db.clone());
    let who = RequesterIdentity::Unknown;
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    let before = std::fs::read(vault::path(home.path())).unwrap();
    for _ in 0..3 {
        selected
            .validate_original_unavailable(home.path(), "bot", &who, &people, "gmail_read")
            .unwrap();
    }
    assert_eq!(before, std::fs::read(vault::path(home.path())).unwrap());
    assert!(
        selected
            .validate_original_unavailable(home.path(), "bot", &who, &people, "gmail_search")
            .is_err()
    );
    db.set_user_status(&user, duduclaw_auth::UserStatus::Suspended)
        .unwrap();
    assert!(
        selected
            .validate_original_unavailable(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
    assert_eq!(before, std::fs::read(vault::path(home.path())).unwrap());
}

#[test]
fn a_healthy_original_selection_cannot_become_home_authority_after_a_later_failure() {
    let (home, db, user) = fixture();
    configure_home_fallback(home.path());
    let people = UserDbPeople(db);
    let who = RequesterIdentity::Unknown;
    let healthy =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    assert!(
        healthy
            .validate_original_unavailable(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
    vault::disconnect(home.path(), &user).unwrap();
    assert!(
        healthy
            .validate_original_unavailable(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
}

#[test]
fn original_unavailable_rejects_reconnection_and_same_failure_epoch_aba() {
    let (home, db, user) = fixture();
    configure_home_fallback(home.path());
    let old = vault::get(home.path(), &user).unwrap().unwrap();
    vault::disconnect(home.path(), &user).unwrap();
    let people = UserDbPeople(db);
    let who = RequesterIdentity::Unknown;
    let selected =
        PersonalSelection::capture(home.path(), "bot", &who, &people, "gmail_read").unwrap();
    let key = crate::google_identity::protection_gate(home.path()).unwrap();
    let epoch = vault::begin_connection(home.path(), &user).unwrap();
    vault::connect(
        home.path(),
        &key,
        old.metadata,
        &Secret {
            access_token: "owned-reconnected".into(),
            refresh_token: None,
            expires_at: Some(4_000_000_000),
        },
        epoch,
    )
    .unwrap();
    assert!(
        selected
            .validate_original_unavailable(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
    vault::disconnect(home.path(), &user).unwrap();
    assert!(
        selected
            .validate_original_unavailable(home.path(), "bot", &who, &people, "gmail_read")
            .is_err()
    );
}
