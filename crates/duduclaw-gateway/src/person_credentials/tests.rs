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
