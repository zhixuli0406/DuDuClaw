use super::*;

fn fixture() -> (tempfile::TempDir, ProtectedKey) {
    let dir = tempfile::tempdir().unwrap();
    duduclaw_core::gateway_instance::acquire(dir.path()).unwrap();
    crate::handlers::credential_protection_rpc::protection_for(dir.path())
        .enable(false).unwrap();
    let key = crate::google_identity::protection_gate(dir.path()).unwrap();
    (dir, key)
}

fn metadata(kind: &str, id: &str) -> Metadata {
    Metadata {
        owner_kind: kind.into(), owner_id: id.into(), purpose: PURPOSE.into(),
        provider: "google".into(), credential_id: uuid::Uuid::new_v4().to_string(),
        revision: 1, account_email: "alice@c.test".into(),
        google_sub: "google-person-1".into(), hosted_domain: Some("c.test".into()),
        granted_scopes: vec![DRIVE_SCOPE.into(), "openid".into(), "email".into()],
        status: Status::Connected, status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None,
        oauth_client_id: "client-1".into(),
        google_data_consent: Some(crate::google_data_consent::Evidence::issue("user-a").unwrap()),
    }
}

fn secret() -> Secret {
    Secret {
        access_token: "fixture-write-access-secret".into(),
        refresh_token: Some("fixture-write-refresh-secret".into()),
        expires_at: Some(4_000_000_000),
    }
}

fn connected(home: &Path, key: &ProtectedKey, kind: &str, id: &str) -> Record {
    let epoch = begin_connection(home, kind, id).unwrap();
    connect(home, key, metadata(kind, id), &secret(), epoch).unwrap();
    get(home, kind, id).unwrap().unwrap()
}

fn connect_personal(home: &Path, key: &ProtectedKey) {
    let mut value = serde_json::to_value(metadata("user", "user-a")).unwrap();
    value.as_object_mut().unwrap().remove("purpose");
    value["granted_scopes"] = serde_json::json!(["openid", "email"]);
    let metadata: crate::person_credentials::Metadata = serde_json::from_value(value).unwrap();
    let epoch = crate::person_credentials::begin_connection(home, "user-a").unwrap();
    crate::person_credentials::connect(home, key, metadata, &secret(), epoch).unwrap();
}

fn ordinary_metadata(metadata: &Metadata) -> crate::person_credentials::Metadata {
    let mut value = serde_json::to_value(metadata).unwrap();
    value.as_object_mut().unwrap().remove("purpose");
    serde_json::from_value(value).unwrap()
}

#[test]
fn ordinary_unseal_rejects_transplanted_write_vault_secret() {
    let (home, key) = fixture();
    let record = connected(home.path(), &key, "user", "user-a");
    assert!(unseal(&key, &record).is_ok());
    let transplanted = crate::person_credentials::Record {
        metadata: ordinary_metadata(&record.metadata),
        secret: record.secret.clone(),
    };
    assert_eq!(
        crate::person_credentials::unseal(&key, &transplanted).err().as_deref(),
        Some("你的連線憑證無法辨識，請重新連結。"),
    );
}

#[test]
fn write_unseal_rejects_transplanted_ordinary_vault_secret() {
    let (home, key) = fixture();
    let mut record = connected(home.path(), &key, "user", "user-a");
    let metadata = ordinary_metadata(&record.metadata);
    let epoch = crate::person_credentials::begin_connection(home.path(), "user-a").unwrap();
    crate::person_credentials::connect(home.path(), &key, metadata, &secret(), epoch).unwrap();
    let ordinary = crate::person_credentials::get(home.path(), "user-a").unwrap().unwrap();
    assert!(crate::person_credentials::unseal(&key, &ordinary).is_ok());
    record.secret = ordinary.secret.clone();
    assert_eq!(
        unseal(&key, &record).err().as_deref(),
        Some("你的寫入連線憑證無法辨識，請重新連結。"),
    );
}

#[test]
fn seal_round_trip_and_clear_identity_tampering_is_rejected() {
    let (home, key) = fixture();
    let record = connected(home.path(), &key, "user", "user-a");
    let plain = unseal(&key, &record).unwrap();
    assert_eq!(plain.access_token, secret().access_token);
    assert_eq!(plain.refresh_token, secret().refresh_token);
    assert_eq!(plain.expires_at, secret().expires_at);
    let text = std::fs::read_to_string(path(home.path())).unwrap();
    assert!(text.contains("pk:v1:"));
    assert!(!text.contains("fixture-write-access-secret"));
    assert!(!text.contains("fixture-write-refresh-secret"));
    let mut tampered = record.clone();
    tampered.metadata.account_email = "bob@c.test".into();
    assert!(unseal(&key, &tampered).is_err());
    let mut tampered = record.clone();
    tampered.metadata.owner_id = "user-b".into();
    assert!(unseal(&key, &tampered).is_err());
    let mut tampered = record;
    tampered.metadata.owner_kind = "home".into();
    tampered.metadata.owner_id = "home".into();
    assert!(unseal(&key, &tampered).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path(home.path())).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path(home.path()).parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }
}

#[test]
fn connect_rejects_wrong_purpose_and_nonexact_drive_scopes() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user", "user-a").unwrap();
    let before = std::fs::read(path(home.path())).unwrap();
    let mut wrong = metadata("user", "user-a");
    wrong.purpose = "drive_read".into();
    assert!(connect(home.path(), &key, wrong, &secret(), epoch).is_err());
    for scope in ["openid", "https://www.googleapis.com/auth/drive.readonly",
        "https://www.googleapis.com/auth/drive.file", "prefixhttps://www.googleapis.com/auth/drive"] {
        let mut wrong = metadata("user", "user-a");
        wrong.granted_scopes = vec![scope.into()];
        assert!(connect(home.path(), &key, wrong, &secret(), epoch).is_err());
    }
    assert_eq!(std::fs::read(path(home.path())).unwrap(), before);
    connect(home.path(), &key, metadata("user", "user-a"), &secret(), epoch).unwrap();
}

#[test]
fn disconnect_epoch_fences_stale_connect_and_replay() {
    let (home, key) = fixture();
    let epoch = begin_connection(home.path(), "user", "user-a").unwrap();
    disconnect(home.path(), "user", "user-a").unwrap();
    assert!(connect(home.path(), &key, metadata("user", "user-a"), &secret(), epoch).is_err());
    assert!(get(home.path(), "user", "user-a").unwrap().is_none());
    let epoch = begin_connection(home.path(), "user", "user-a").unwrap();
    connect(home.path(), &key, metadata("user", "user-a"), &secret(), epoch).unwrap();
    assert!(connect(home.path(), &key, metadata("user", "user-a"), &secret(), epoch).is_err());
}

#[test]
fn member_suspension_and_reactivation_restore_previous_write_status() {
    let (home, key) = fixture();
    for status in [Status::Connected, Status::NeedsReconnect] {
        let record = connected(home.path(), &key, "user", "user-a");
        if status == Status::NeedsReconnect {
            assert!(mark_needs_reconnect(home.path(), &record.metadata).unwrap());
        }
        crate::person_credentials::sync_user_status(home.path(), "user-a", false).unwrap();
        let suspended = get(home.path(), "user", "user-a").unwrap().unwrap();
        assert_eq!(suspended.metadata.status, Status::Suspended);
        assert_eq!(suspended.metadata.status_before_suspension, Some(status));
        assert!(unseal(&key, &suspended).is_ok());
        crate::person_credentials::sync_user_status(home.path(), "user-a", true).unwrap();
        let restored = get(home.path(), "user", "user-a").unwrap().unwrap();
        assert_eq!(restored.metadata.status, status);
        assert_eq!(restored.metadata.status_before_suspension, None);
        assert!(unseal(&key, &restored).is_ok());
    }
}

#[test]
fn ordinary_disconnect_removes_write_record_and_fences_pending_finish() {
    let (home, key) = fixture();
    connect_personal(home.path(), &key);
    connected(home.path(), &key, "user", "user-a");
    connected(home.path(), &key, "home", "home");
    let epoch = begin_connection(home.path(), "user", "user-a").unwrap();
    assert!(crate::person_credentials::disconnect(home.path(), "user-a").unwrap().is_some());
    assert!(get(home.path(), "user", "user-a").unwrap().is_none());
    assert!(get(home.path(), "home", "home").unwrap().is_some());
    assert!(connect(home.path(), &key, metadata("user", "user-a"), &secret(), epoch).is_err());
    // Cleanup still fences a write-only record when the ordinary record is absent.
    connected(home.path(), &key, "user", "user-a");
    assert!(crate::person_credentials::disconnect(home.path(), "user-a").unwrap().is_none());
    assert!(get(home.path(), "user", "user-a").unwrap().is_none());
}

#[test]
fn write_operations_leave_ordinary_store_bytes_unchanged() {
    let (home, key) = fixture();
    connect_personal(home.path(), &key);
    let file = crate::person_credentials::path(home.path());
    let before = std::fs::read(&file).unwrap();
    let record = connected(home.path(), &key, "user", "user-a");
    let mut scopes = record.metadata.granted_scopes.clone();
    scopes.push("https://www.googleapis.com/auth/documents".into());
    assert!(finish_refresh_with_scopes(home.path(), &key, &record.metadata, &secret(), Some(&scopes)).unwrap());
    let refreshed = get(home.path(), "user", "user-a").unwrap().unwrap();
    assert_eq!(refreshed.metadata.revision, 2);
    assert_eq!(refreshed.metadata.granted_scopes, scopes);
    assert!(unseal(&key, &refreshed).is_ok());
    assert!(mark_needs_reconnect(home.path(), &refreshed.metadata).unwrap());
    sync_user_status(home.path(), "user-a", false).unwrap();
    sync_user_status(home.path(), "user-a", true).unwrap();
    disconnect(home.path(), "user", "user-a").unwrap();
    assert_eq!(std::fs::read(file).unwrap(), before);
}

#[test]
fn corrupt_write_store_is_preserved_and_lifecycle_hooks_return_errors() {
    let (home, key) = fixture();
    connect_personal(home.path(), &key);
    begin_connection(home.path(), "user", "user-a").unwrap();
    std::fs::write(path(home.path()), "not-json").unwrap();
    let before = std::fs::read(crate::person_credentials::path(home.path())).unwrap();
    assert!(all_metadata(home.path()).is_err());
    assert!(get(home.path(), "user", "user-a").is_err());
    assert!(begin_connection(home.path(), "user", "user-a").is_err());
    assert!(disconnect(home.path(), "user", "user-a").is_err());
    assert!(crate::person_credentials::sync_user_status(home.path(), "user-a", false).is_err());
    assert!(crate::person_credentials::disconnect(home.path(), "user-a").is_err());
    assert_eq!(std::fs::read_to_string(path(home.path())).unwrap(), "not-json");
    assert_eq!(std::fs::read(crate::person_credentials::path(home.path())).unwrap(), before);
}

#[test]
fn missing_write_store_is_empty_without_creating_a_file() {
    let home = tempfile::tempdir().unwrap();
    assert!(all_metadata(home.path()).unwrap().is_empty());
    assert!(get(home.path(), "user", "user-a").unwrap().is_none());
    assert!(snapshot(home.path(), "home", "home").unwrap().is_none());
    assert!(!ever_granted(home.path(), "user", "user-a").unwrap());
    assert!(!path(home.path()).exists());
}

#[test]
fn ever_granted_survives_disconnect_suspension_and_ordinary_disconnect() {
    let (home, key) = fixture();
    connect_personal(home.path(), &key);
    connected(home.path(), &key, "user", "user-a");
    assert!(read(home.path()).unwrap().ever_granted.contains("user:user-a"));
    assert!(!ever_granted(home.path(), "user", "user-b").unwrap());
    assert!(!ever_granted(home.path(), "home", "home").unwrap());
    crate::person_credentials::sync_user_status(home.path(), "user-a", false).unwrap();
    assert!(ever_granted(home.path(), "user", "user-a").unwrap());
    disconnect(home.path(), "user", "user-a").unwrap();
    assert!(get(home.path(), "user", "user-a").unwrap().is_none());
    assert!(ever_granted(home.path(), "user", "user-a").unwrap());
    crate::person_credentials::disconnect(home.path(), "user-a").unwrap();
    assert!(crate::person_credentials::get(home.path(), "user-a").unwrap().is_none());
    assert!(ever_granted(home.path(), "user", "user-a").unwrap());
}

#[test]
fn legacy_write_store_without_ever_granted_parses_and_disconnect_retains_grant() {
    let (home, key) = fixture();
    connected(home.path(), &key, "user", "user-a");
    let mut legacy: serde_json::Value = serde_json::from_slice(&std::fs::read(path(home.path())).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("ever_granted");
    std::fs::write(path(home.path()), serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(read(home.path()).unwrap().ever_granted.is_empty());
    assert!(ever_granted(home.path(), "user", "user-a").unwrap());
    disconnect(home.path(), "user", "user-a").unwrap();
    assert!(get(home.path(), "user", "user-a").unwrap().is_none());
    assert!(ever_granted(home.path(), "user", "user-a").unwrap());

    legacy["records"] = serde_json::json!([]);
    std::fs::write(path(home.path()), serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(!ever_granted(home.path(), "user", "user-a").unwrap());
}

#[test]
fn ever_granted_rejects_unreadable_store_and_invalid_markers() {
    let home = tempfile::tempdir().unwrap();
    begin_connection(home.path(), "user", "user-a").unwrap();
    for contents in ["invalid-json".to_owned(),
        serde_json::json!({"version": super::super::VERSION, "records": [], "ever_granted": ["home:other"]}).to_string(),
    ] {
        std::fs::write(path(home.path()), contents).unwrap();
        assert!(ever_granted(home.path(), "user", "user-a").is_err());
    }
}

#[test]
fn unknown_record_fields_and_duplicate_owner_records_fail_closed() {
    let (home, key) = fixture();
    connected(home.path(), &key, "user", "user-a");
    let bytes = std::fs::read(path(home.path())).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mut unknown = original.clone();
    unknown["records"][0]["future_grant"] = serde_json::json!(true);
    let mut duplicate = original;
    let record = duplicate["records"][0].clone();
    duplicate["records"].as_array_mut().unwrap().push(record);
    for invalid in [unknown, duplicate] {
        let bytes = serde_json::to_vec(&invalid).unwrap();
        std::fs::write(path(home.path()), &bytes).unwrap();
        assert!(all_metadata(home.path()).is_err());
        assert!(disconnect(home.path(), "user", "user-a").is_err());
        assert_eq!(std::fs::read(path(home.path())).unwrap(), bytes);
    }
}

#[test]
fn owner_namespaces_are_separate_and_connect_replaces_only_same_owner() {
    let (home, key) = fixture();
    connected(home.path(), &key, "user", "home");
    connected(home.path(), &key, "home", "home");
    let old = snapshot(home.path(), "user", "home").unwrap().unwrap();
    let record = connected(home.path(), &key, "user", "home");
    assert_ne!(old.credential_id, record.metadata.credential_id);
    assert_eq!(all_metadata(home.path()).unwrap().len(), 2);
    assert!(begin_connection(home.path(), "home", "other").is_err());
    assert!(begin_connection(home.path(), "other", "home").is_err());
}

#[test]
fn refresh_cas_and_exact_scope_validation_preserve_current_grant() {
    let (home, key) = fixture();
    let old = connected(home.path(), &key, "user", "user-a");
    let current = connected(home.path(), &key, "user", "user-a");
    assert!(!finish_refresh_with_scopes(home.path(), &key, &old.metadata, &secret(), None).unwrap());
    assert!(!mark_needs_reconnect(home.path(), &old.metadata).unwrap());
    let before = std::fs::read(path(home.path())).unwrap();
    let scopes = vec!["https://www.googleapis.com/auth/drive.readonly".into()];
    assert!(finish_refresh_with_scopes(home.path(), &key, &current.metadata, &secret(), Some(&scopes)).is_err());
    assert_eq!(std::fs::read(path(home.path())).unwrap(), before);
    assert!(mark_needs_reconnect(home.path(), &current.metadata).unwrap());
    assert!(!finish_refresh_with_scopes(home.path(), &key, &current.metadata, &secret(), None).unwrap());
}

#[test]
fn concurrent_epoch_updates_are_serialized_by_the_shared_file_lock() {
    let home = tempfile::tempdir().unwrap();
    let threads: Vec<_> = (0..8).map(|_| {
        let home = home.path().to_path_buf();
        std::thread::spawn(move || {
            for _ in 0..8 {
                begin_connection(&home, "user", "user-a").unwrap();
            }
        })
    }).collect();
    for thread in threads { thread.join().unwrap(); }
    assert_eq!(begin_connection(home.path(), "user", "user-a").unwrap(), 65);
}

#[test]
fn syncing_absent_user_does_not_create_write_store() {
    let home = tempfile::tempdir().unwrap();
    for active in [true, false] {
        sync_user_status(home.path(), "user-a", active).unwrap();
        assert!(!path(home.path()).exists());
    }
}
