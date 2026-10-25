use super::*;

fn fixture() -> (tempfile::TempDir, ProtectedKey) {
    let dir = tempfile::tempdir().unwrap();
    duduclaw_core::gateway_instance::acquire(dir.path()).unwrap();
    crate::handlers::credential_protection_rpc::protection_for(dir.path())
        .enable(false)
        .unwrap();
    let key = crate::google_identity::protection_gate(dir.path()).unwrap();
    (dir, key)
}

fn metadata(user: &str) -> Metadata {
    Metadata {
        owner_kind: "user".into(),
        owner_id: user.into(),
        provider: "google".into(),
        credential_id: uuid::Uuid::new_v4().to_string(),
        revision: 1,
        account_email: "alice@c.test".into(),
        google_sub: "google-person-1".into(),
        hosted_domain: Some("c.test".into()),
        granted_scopes: vec!["openid".into(), "email".into()],
        status: Status::Connected,
        status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(),
        last_refresh_at: None,
        oauth_client_id: "client-1".into(),
        google_data_consent: Some(crate::google_data_consent::Evidence::issue(user).unwrap()),
    }
}

fn secret() -> Secret {
    Secret {
        access_token: "fixture-access-secret".into(),
        refresh_token: Some("fixture-refresh-secret".into()),
        expires_at: Some(4_000_000_000),
    }
}

#[test]
fn suspend_and_resume_preserve_a_preexisting_reconnect_requirement() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let record = get(home.path(), "user-a").unwrap().unwrap();
    mark_needs_reconnect(home.path(), &record.metadata).unwrap();
    sync_user_status(home.path(), "user-a", false).unwrap();
    assert_eq!(
        get(home.path(), "user-a").unwrap().unwrap().metadata.status,
        Status::Suspended
    );
    sync_user_status(home.path(), "user-a", true).unwrap();
    assert_eq!(
        get(home.path(), "user-a").unwrap().unwrap().metadata.status,
        Status::NeedsReconnect
    );
}

#[test]
fn secrets_are_sealed_and_epoch_cas_prevents_late_finish_resurrection() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let text = std::fs::read_to_string(path(home.path())).unwrap();
    assert!(text.contains("pk:v1:"));
    assert!(!text.contains("fixture-access-secret") && !text.contains("fixture-refresh-secret"));
    let record = get(home.path(), "user-a").unwrap().unwrap();
    assert_eq!(
        unseal(&key, &record).unwrap().access_token,
        "fixture-access-secret"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path(home.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path(home.path()).parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let stale_epoch = begin_connection(home.path(), "user-a").unwrap();
    disconnect(home.path(), "user-a").unwrap();
    assert!(
        connect(
            home.path(),
            &key,
            metadata("user-a"),
            &secret(),
            stale_epoch
        )
        .is_err()
    );
    assert!(get(home.path(), "user-a").unwrap().is_none());
}

#[test]
fn refresh_cas_never_overwrites_a_reconnect_or_disconnect() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let old = get(home.path(), "user-a").unwrap().unwrap();
    let fresh_epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(
        home.path(),
        &key,
        metadata("user-a"),
        &secret(),
        fresh_epoch,
    )
    .unwrap();
    assert!(!finish_refresh(home.path(), &key, &old.metadata, &secret()).unwrap());
    let current = get(home.path(), "user-a").unwrap().unwrap();
    assert!(!mark_needs_reconnect(home.path(), &old.metadata).unwrap());
    assert_eq!(
        get(home.path(), "user-a").unwrap().unwrap().metadata.status,
        Status::Connected
    );
    assert!(mark_needs_reconnect(home.path(), &current.metadata).unwrap());
    assert!(
        !mark_needs_reconnect(home.path(), &current.metadata).unwrap(),
        "notify only the first successful transition"
    );
    disconnect(home.path(), "user-a").unwrap();
    assert!(!finish_refresh(home.path(), &key, &current.metadata, &secret()).unwrap());
}

#[test]
fn corrupt_store_is_never_replaced_with_an_empty_vault() {
    let (home, _key) = fixture();
    begin_connection(home.path(), "user-a").unwrap();
    std::fs::write(path(home.path()), "not-json").unwrap();
    assert!(begin_connection(home.path(), "user-a").is_err());
    assert!(disconnect(home.path(), "user-a").is_err());
    assert_eq!(
        std::fs::read_to_string(path(home.path())).unwrap(),
        "not-json"
    );
}

#[test]
fn clear_metadata_cannot_transfer_a_sealed_account_or_expand_scopes() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let mut record = get(home.path(), "user-a").unwrap().unwrap();
    record.metadata.owner_id = "user-b".into();
    assert!(unseal(&key, &record).is_err());
    record.metadata.owner_id = "user-a".into();
    record
        .metadata
        .granted_scopes
        .push("https://www.googleapis.com/auth/drive".into());
    assert!(unseal(&key, &record).is_err());
}

#[test]
fn authority_snapshot_ignores_refresh_but_fences_scope_and_status_aba() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let selected = authority_snapshot(home.path(), "user-a").unwrap().unwrap();
    let initial = get(home.path(), "user-a").unwrap().unwrap();
    assert!(finish_refresh(home.path(), &key, &initial.metadata, &secret()).unwrap());
    let refreshed = authority_snapshot(home.path(), "user-a").unwrap().unwrap();
    assert_eq!(
        selected, refreshed,
        "routine refresh preserves logical authority"
    );
    let current = get(home.path(), "user-a").unwrap().unwrap();
    let reduced = vec!["openid".to_owned()];
    assert!(
        finish_refresh_with_scopes(
            home.path(),
            &key,
            &current.metadata,
            &secret(),
            Some(&reduced)
        )
        .unwrap()
    );
    let current = get(home.path(), "user-a").unwrap().unwrap();
    assert!(
        finish_refresh_with_scopes(
            home.path(),
            &key,
            &current.metadata,
            &secret(),
            Some(&initial.metadata.granted_scopes)
        )
        .unwrap()
    );
    assert_ne!(
        selected,
        authority_snapshot(home.path(), "user-a").unwrap().unwrap(),
        "scope A-B-A is new authority"
    );
    let selected = authority_snapshot(home.path(), "user-a").unwrap().unwrap();
    sync_user_status(home.path(), "user-a", false).unwrap();
    sync_user_status(home.path(), "user-a", true).unwrap();
    assert_ne!(
        selected,
        authority_snapshot(home.path(), "user-a").unwrap().unwrap(),
        "status A-B-A is new authority"
    );
    let value = serde_json::to_string(&selected).unwrap();
    assert!(!value.contains("fixture-access-secret"));
    assert!(!value.contains("fixture-refresh-secret"));
    assert!(!value.contains("pk:v1:"));
}

#[test]
fn publication_advances_grant_epoch_and_consumes_the_connection_epoch() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let record = get(home.path(), "user-a").unwrap().unwrap();
    let publication_epoch = begin_connection(home.path(), "user-a").unwrap();
    let pending = authority_snapshot(home.path(), "user-a").unwrap().unwrap();
    connect(
        home.path(),
        &key,
        record.metadata.clone(),
        &secret(),
        publication_epoch,
    )
    .unwrap();
    assert_ne!(
        pending,
        authority_snapshot(home.path(), "user-a").unwrap().unwrap(),
        "same account/id publication must fence a selection made while OAuth was pending"
    );
    assert!(
        connect(
            home.path(),
            &key,
            record.metadata,
            &secret(),
            publication_epoch
        )
        .is_err(),
        "a successful publication consumes its epoch"
    );
}

#[test]
fn refresh_scope_order_is_not_new_grant_authority() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let selected = authority_snapshot(home.path(), "user-a").unwrap().unwrap();
    let record = get(home.path(), "user-a").unwrap().unwrap();
    let mut scopes = record.metadata.granted_scopes.clone();
    scopes.reverse();
    assert!(
        finish_refresh_with_scopes(
            home.path(),
            &key,
            &record.metadata,
            &secret(),
            Some(&scopes)
        )
        .unwrap()
    );
    assert_eq!(
        selected,
        authority_snapshot(home.path(), "user-a").unwrap().unwrap()
    );
}

fn rejects_expected_metadata_edit(edit: impl FnOnce(&mut Metadata)) -> bool {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let before = std::fs::read(path(home.path())).unwrap();
    let mut expected = get(home.path(), "user-a").unwrap().unwrap().metadata;
    edit(&mut expected);
    let result = finish_refresh(home.path(), &key, &expected, &secret());
    !matches!(result, Ok(true)) && before == std::fs::read(path(home.path())).unwrap()
}

#[test]
fn refresh_rejects_modified_expected_account_binding_without_changing_stored_bytes() {
    let cases = [
        "google_sub",
        "account_email",
        "hosted_domain",
        "oauth_client_id",
    ];
    let mut accepted_or_changed = Vec::new();
    for case in cases {
        let refused = rejects_expected_metadata_edit(|expected| match case {
            "google_sub" => expected.google_sub = "different-google-person".into(),
            "account_email" => expected.account_email = "different@c.test".into(),
            "hosted_domain" => expected.hosted_domain = Some("different.test".into()),
            "oauth_client_id" => expected.oauth_client_id = "different-client".into(),
            _ => unreachable!(),
        });
        if !refused {
            accepted_or_changed.push(case);
        }
    }
    assert!(
        accepted_or_changed.is_empty(),
        "modified expected binding was accepted or changed stored bytes: {accepted_or_changed:?}"
    );
}

#[test]
fn refresh_rejects_modified_expected_scopes_without_changing_stored_bytes() {
    assert!(
        rejects_expected_metadata_edit(|expected| {
            expected
                .granted_scopes
                .push("https://www.googleapis.com/auth/gmail.modify".into());
        }),
        "modified expected scopes must not become refresh authority"
    );
}

#[test]
fn refresh_rejects_modified_expected_status_without_changing_stored_bytes() {
    let mut accepted_or_changed = Vec::new();
    for case in ["suspended", "needs_reconnect", "suspension_origin"] {
        let refused = rejects_expected_metadata_edit(|expected| match case {
            "suspended" => expected.status = Status::Suspended,
            "needs_reconnect" => expected.status = Status::NeedsReconnect,
            "suspension_origin" => expected.status_before_suspension = Some(Status::NeedsReconnect),
            _ => unreachable!(),
        });
        if !refused {
            accepted_or_changed.push(case);
        }
    }
    assert!(
        accepted_or_changed.is_empty(),
        "modified expected status was accepted or changed stored bytes: {accepted_or_changed:?}"
    );
}

#[test]
fn metadata_resume_cas_fences_epoch_revision_and_preserves_reconnect_origin() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    let record = get(home.path(), "user-a").unwrap().unwrap();
    mark_needs_reconnect(home.path(), &record.metadata).unwrap();
    sync_user_status(home.path(), "user-a", false).unwrap();
    let state = authority_state(home.path(), "user-a").unwrap();
    let expected = state.metadata.unwrap();
    assert!(
        resume_status_if_current(home.path(), &expected, state.epoch + 1, || Ok(()))
            .unwrap()
            .is_none()
    );
    let resumed = resume_status_if_current(home.path(), &expected, state.epoch, || Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(resumed.metadata.unwrap().status, Status::NeedsReconnect);
    assert!(
        resume_status_if_current(home.path(), &expected, state.epoch, || Ok(()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn metadata_resume_authorization_is_rechecked_inside_cas_before_write() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user-a").unwrap();
    connect(home.path(), &key, metadata("user-a"), &secret(), epoch).unwrap();
    sync_user_status(home.path(), "user-a", false).unwrap();
    let state = authority_state(home.path(), "user-a").unwrap();
    let expected = state.metadata.unwrap();
    let before = std::fs::read(path(home.path())).unwrap();
    assert!(
        resume_status_if_current(home.path(), &expected, state.epoch, || Err(
            "fixed authority changed".into()
        ))
        .is_err()
    );
    assert!(before == std::fs::read(path(home.path())).unwrap());
}
