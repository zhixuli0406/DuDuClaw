use super::*;

const PASSWORD: &str = "website-password-canary-DoNotLog";

fn request() -> Upsert {
    Upsert {
        id: None, label: "work".into(), login_url: "https://EXAMPLE.com:443/login".into(),
        allowed_paths: Vec::new(),
        username: "person@example.com".into(), password: PASSWORD.into(),
        allowed_agents: vec!["alice".into()],
    }
}

#[test]
fn storage_round_trip_is_encrypted_and_listing_has_no_password() {
    let home = tempfile::tempdir().unwrap();
    let saved = upsert(home.path(), request(), "owner").unwrap();
    assert_eq!(saved.origin, "https://example.com");
    assert_eq!(saved.login_url, "https://example.com/login");
    assert_eq!(saved.login_path, "/login");
    assert!(saved.allowed_paths.is_empty());
    assert_eq!(saved.created_by.owner_kind, "user");
    assert_eq!(saved.created_by.owner_id, "owner");
    let read = for_agent(home.path(), "alice", "work").unwrap().unwrap();
    assert_eq!(read.password, PASSWORD);
    assert_eq!(read.metadata, saved);
    let disk = std::fs::read_to_string(path(home.path())).unwrap();
    for value in [PASSWORD, "person@example.com", "https://example.com", "alice"] {
        assert!(!disk.contains(value));
    }
    let listed = serde_json::to_value(list(home.path()).unwrap()).unwrap();
    assert!(listed[0].get("password").is_none());
    assert!(!listed.to_string().contains(PASSWORD));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path(home.path())).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(home.path().join(".keyfile")).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn unknown_and_non_allowed_agents_cannot_select_a_secret() {
    let home = tempfile::tempdir().unwrap();
    upsert(home.path(), request(), "owner").unwrap();
    assert!(for_agent(home.path(), "bob", "work").unwrap().is_none());
    assert!(for_agent(home.path(), "alice-extra", "work").unwrap().is_none());
    assert!(for_agent(home.path(), "alice", "unknown").unwrap().is_none());
    assert!(for_agent(home.path(), "alice", "WORK").unwrap().is_none());
}

fn assert_unavailable_and_unchanged(home: &Path, existing_id: &str) {
    let before = std::fs::read(path(home)).unwrap();
    assert_eq!(list(home).err().as_deref(), Some(UNAVAILABLE));
    assert_eq!(for_agent(home, "alice", "work").err().as_deref(), Some(UNAVAILABLE));
    assert_eq!(for_agent(home, "alice", "second").err().as_deref(), Some(UNAVAILABLE));
    let mut added = request();
    added.label = "new".into();
    assert_eq!(upsert(home, added, "owner").err().as_deref(), Some(UNAVAILABLE));
    let mut updated = request();
    updated.id = Some(existing_id.into());
    assert_eq!(upsert(home, updated, "owner").err().as_deref(), Some(UNAVAILABLE));
    assert_eq!(delete(home, existing_id).err().as_deref(), Some(UNAVAILABLE));
    assert_eq!(delete(home, "invalid-id").err().as_deref(), Some(UNAVAILABLE));
    let mut invalid_input = request();
    invalid_input.login_url = "http://example.com/login".into();
    assert_eq!(upsert(home, invalid_input, "owner").err().as_deref(), Some(UNAVAILABLE));
    assert_eq!(delete(home, &uuid::Uuid::new_v4().to_string()).err().as_deref(), Some(UNAVAILABLE));
    assert_eq!(std::fs::read(path(home)).unwrap(), before);
}

#[test]
fn any_undecryptable_record_blocks_all_reads_and_mutations() {
    let home = tempfile::tempdir().unwrap();
    let first = upsert(home.path(), request(), "owner").unwrap();
    let mut next = request();
    next.label = "second".into();
    upsert(home.path(), next, "owner").unwrap();
    let mut store = read_store(home.path()).unwrap();
    store.records[0] = "corrupt".into();
    std::fs::write(path(home.path()), serde_json::to_vec(&store).unwrap()).unwrap();
    assert_unavailable_and_unchanged(home.path(), &first.id);
}

#[test]
fn invalid_decrypted_metadata_or_legacy_origin_only_record_blocks_the_store() {
    let home = tempfile::tempdir().unwrap();
    let saved = upsert(home.path(), request(), "owner").unwrap();
    let engine = read_engine(home.path()).unwrap();
    let mut store = read_store(home.path()).unwrap();
    let plain = engine.decrypt_string(&store.records[0]).unwrap();
    let original: serde_json::Value = serde_json::from_str(&plain).unwrap();
    for field in ["origin", "login_path", "login_url", "allowed_paths", "legacy"] {
        let mut invalid = original.clone();
        if field == "legacy" {
            invalid["record"].as_object_mut().unwrap().remove("login_url");
            invalid["record"].as_object_mut().unwrap().remove("login_path");
        } else {
            invalid["record"][field] = match field {
                "origin" => serde_json::json!("https://other.example"),
                "login_path" => serde_json::json!("/different"),
                "login_url" => serde_json::json!("https://EXAMPLE.com:443/login"),
                _ => serde_json::json!(["//other.example/password"]),
            };
        }
        store.records[0] = engine.encrypt_string(&invalid.to_string()).unwrap();
        std::fs::write(path(home.path()), serde_json::to_vec(&store).unwrap()).unwrap();
        assert_unavailable_and_unchanged(home.path(), &saved.id);
    }
}

#[test]
fn missing_or_wrong_key_is_unavailable_and_never_replaced() {
    let home = tempfile::tempdir().unwrap();
    let saved = upsert(home.path(), request(), "owner").unwrap();
    std::fs::remove_file(home.path().join(".keyfile")).unwrap();
    assert_unavailable_and_unchanged(home.path(), &saved.id);
    assert!(!home.path().join(".keyfile").exists());
    std::fs::write(home.path().join(".keyfile"), [9_u8; 32]).unwrap();
    assert_unavailable_and_unchanged(home.path(), &saved.id);
    assert_eq!(std::fs::read(home.path().join(".keyfile")).unwrap(), [9_u8; 32]);
}

#[test]
fn login_urls_are_canonical_https_and_paths_are_exact() {
    let url = normalize_login_url("https://EXAMPLE.com:443/a/../login").unwrap();
    assert_eq!(url.as_str(), "https://example.com/login");
    assert_eq!(url.origin().ascii_serialization(), "https://example.com");
    assert_eq!(url.path(), "/login");
    for value in ["http://example.com/login", "http://localhost/login", "http://127.0.0.1",
        "https://user:pass@example.com/login", "https://example.com/login?x=1",
        "https://example.com/login#fragment", "https://example.com//login", "file:///tmp", "javascript:alert(1)", "garbage",
        " https://example.com/login", "https://example.com/lo\ngin"] {
        assert!(normalize_login_url(value).is_err(), "{value}");
    }
    for path in ["/", "/login", "/password", "/login%20step"] {
        assert!(valid_login_path("https://example.com", path), "{path}");
    }
    for path in ["", "password", "//evil.test/login", "/a/../password", "/a/./password",
        "/password?x=1", "/password#fragment", "/a\\password", "https://example.com/password"] {
        assert!(!valid_login_path("https://example.com", path), "{path}");
    }
}

#[test]
fn allowed_second_step_paths_are_optional_but_must_be_canonical() {
    let home = tempfile::tempdir().unwrap();
    let mut input = request();
    input.allowed_paths = vec!["/password".into(), "/auth/password".into()];
    let saved = upsert(home.path(), input, "owner").unwrap();
    assert_eq!(saved.allowed_paths, vec!["/password", "/auth/password"]);
    for invalid_path in ["//evil.test/login", "/a/../password", "/password?x=1", "https://example.com/password"] {
        let mut invalid = request();
        invalid.id = Some(saved.id.clone());
        invalid.allowed_paths = vec![invalid_path.into()];
        let before = std::fs::read(path(home.path())).unwrap();
        assert!(upsert(home.path(), invalid, "owner").is_err());
        assert_eq!(std::fs::read(path(home.path())).unwrap(), before);
    }
}

#[test]
fn updates_preserve_creator_and_delete_removes_access() {
    let home = tempfile::tempdir().unwrap();
    let saved = upsert(home.path(), request(), "creator").unwrap();
    let mut updated = request();
    updated.id = Some(saved.id.clone());
    updated.password = "replacement-secret".into();
    updated.allowed_agents = vec!["bob".into()];
    let changed = upsert(home.path(), updated, "editor").unwrap();
    assert_eq!(changed.created_by.owner_id, "creator");
    assert!(for_agent(home.path(), "alice", "work").unwrap().is_none());
    assert_eq!(for_agent(home.path(), "bob", "work").unwrap().unwrap().password, "replacement-secret");
    assert!(delete(home.path(), &saved.id).unwrap());
    assert!(!delete(home.path(), &saved.id).unwrap());
    assert!(list(home.path()).unwrap().is_empty());
}

#[test]
fn empty_grants_and_duplicate_labels_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let mut invalid = request();
    invalid.allowed_agents.clear();
    assert!(upsert(home.path(), invalid, "owner").is_err());
    upsert(home.path(), request(), "owner").unwrap();
    assert!(upsert(home.path(), request(), "owner").is_err());
    assert_eq!(list(home.path()).unwrap().len(), 1);
}

#[test]
fn labels_are_trimmed_to_match_mcp_arguments_and_uniqueness() {
    let home = tempfile::tempdir().unwrap();
    let mut padded = request();
    padded.label = "  work  ".into();
    let saved = upsert(home.path(), padded, "owner").unwrap();
    assert_eq!(saved.label, "work");
    assert!(for_agent(home.path(), "alice", "work").unwrap().is_some());
    assert!(upsert(home.path(), request(), "owner").is_err());
    let mut empty = request();
    empty.label = " \t ".into();
    assert!(upsert(home.path(), empty, "owner").is_err());
}

#[test]
fn invalid_key_and_corrupt_store_are_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".keyfile"), b"bad").unwrap();
    assert!(upsert(home.path(), request(), "owner").is_err());
    assert_eq!(std::fs::read(home.path().join(".keyfile")).unwrap(), b"bad");
    std::fs::create_dir_all(home.path().join("secrets")).unwrap();
    std::fs::write(path(home.path()), b"bad store").unwrap();
    assert!(upsert(home.path(), request(), "owner").is_err());
    assert_eq!(std::fs::read(path(home.path())).unwrap(), b"bad store");
}

#[test]
fn rpc_input_uses_login_url_and_defaults_to_no_additional_paths() {
    let input: Upsert = serde_json::from_value(serde_json::json!({
        "label":"work", "login_url":"https://example.com/login", "username":"person",
        "password":"secret", "allowed_agents":["alice"]
    })).unwrap();
    assert!(input.allowed_paths.is_empty());
    assert!(serde_json::from_value::<Upsert>(serde_json::json!({
        "label":"work", "origin":"https://example.com", "username":"person",
        "password":"secret", "allowed_agents":["alice"]
    })).is_err());
}
