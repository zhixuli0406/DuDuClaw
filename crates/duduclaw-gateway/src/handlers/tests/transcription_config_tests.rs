use super::*;

fn admin() -> UserContext {
    UserContext {
        user_id: "system".into(), email: "admin@localhost".into(), role: UserRole::Admin,
        credential_epoch: 0, agent_access: Default::default(), must_change_password: false,
        team_access: Default::default(), acl_generation: 0,
    }
}
fn ok(frame: &WsFrame) -> bool { matches!(frame, WsFrame::Response { ok: true, .. }) }

#[tokio::test]
async fn transcription_settings_require_live_admin_and_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let request = json!({"transcription": {"engine": "whisper", "max_bytes": 123456}});
    let mut employee = admin();
    employee.role = UserRole::Employee;
    assert!(!ok(&handler.handle_system_update_config(request.clone(), &employee).await));
    assert!(!home.path().join("config.toml").exists());
    let mut stale = admin();
    stale.user_id = "removed-admin".into();
    assert!(!ok(&handler.handle_system_update_config(request.clone(), &stale).await));
    assert!(ok(&handler.handle_system_update_config(request, &admin()).await));
    let config = duduclaw_inference::transcription::config::TranscriptionConfig::load(home.path()).unwrap();
    assert_eq!(config.engine.as_str(), "whisper");
    assert_eq!(config.max_bytes, 123456);
    let result = handler.handle_system_config().await;
    let WsFrame::Response { payload: Some(value), .. } = result else { panic!("expected config"); };
    assert_eq!(value["transcription"]["engine"], "whisper");
    assert!(value["transcription_error"].is_null());
}

#[tokio::test]
async fn transcription_settings_reject_unknown_fields_and_bad_models_without_writing() {
    let home = tempfile::tempdir().unwrap();
    let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let original = "# operator settings\n[transcription]\nengine='whisper'\nmax_bytes=1000\n";
    std::fs::write(home.path().join("config.toml"), original).unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    for invalid in [json!({"engine":"other"}), json!({"engine":"gemini", "model":"missing"}),
        json!({"engine":"whisper", "max_bytes":0}), json!({"engine":"whisper", "upload_url":"https://example.com"}), Value::Null] {
        assert!(!ok(&handler.handle_system_update_config(json!({"transcription":invalid}), &admin()).await));
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
    }
}

#[tokio::test]
async fn transcription_google_stt_credentials_require_admin_and_are_encrypted_and_masked() {
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};
    let home = tempfile::tempdir().unwrap();
    let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let private = key.to_pkcs8_pem(LineEnding::LF).unwrap();
    let raw = json!({"type":"service_account", "client_email":"test@unit.iam.gserviceaccount.com", "private_key":private.as_str()}).to_string();
    let mut employee = admin(); employee.role = UserRole::Employee;
    let request = json!({"google_stt_key":raw});
    assert!(!ok(&handler.handle("transcription_credentials.set", request.clone(), &employee).await));
    let mut stale = admin(); stale.user_id = "removed-admin".into();
    assert!(!ok(&handler.handle("transcription_credentials.set", request.clone(), &stale).await));
    assert!(!ok(&handler.handle("transcription_credentials.set", json!({"google_stt_key":"invalid"}), &admin()).await));
    assert!(ok(&handler.handle("transcription_credentials.set", request, &admin()).await));
    let installed = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!installed.contains("BEGIN PRIVATE KEY"));
    let table: toml::Table = installed.parse().unwrap();
    assert!(table["transcription_credentials"].get("google_stt_key").is_none());
    assert_eq!(crate::config_crypto::decrypt_config_field_async(&table, "transcription_credentials", "google_stt_key", home.path()).await.unwrap().expose(), raw);
    let response = handler.handle_system_config().await;
    let WsFrame::Response { payload: Some(value), .. } = response else { panic!("expected config"); };
    let output = value.to_string();
    assert!(!output.contains("BEGIN PRIVATE KEY"));
    assert!(!output.contains(table["transcription_credentials"]["google_stt_key_enc"].as_str().unwrap()));
    assert!(!ok(&handler.handle("transcription_credentials.clear", json!({}), &employee).await));
    assert!(ok(&handler.handle("transcription_credentials.clear", json!({}), &admin()).await));
    let cleared: toml::Table = std::fs::read_to_string(home.path().join("config.toml")).unwrap().parse().unwrap();
    assert!(cleared["transcription_credentials"].get("google_stt_key_enc").is_none());
    assert!(cleared["transcription_credentials"].get("google_stt_key").is_none());
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!audit.contains("BEGIN PRIVATE KEY"));
    assert!(!audit.contains(table["transcription_credentials"]["google_stt_key_enc"].as_str().unwrap()));
    let rows: Vec<Value> = audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).collect();
    for action in ["set", "clear"] {
        assert!(rows.iter().any(|row| row["event_type"] == format!("transcription_credentials.{action}")
            && row["details"] == json!({"actor_user_id":"system","action":action,"status":"success"})));
    }
}

#[tokio::test]
async fn transcription_google_stt_settings_round_trip_and_reject_unsupported_features() {
    let home = tempfile::tempdir().unwrap();
    let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let config = json!({"engine":"google_stt", "google_stt":{"project_id":"unit-project", "bucket":"unit-bucket"}});
    assert!(ok(&handler.handle_system_update_config(json!({"transcription":config}), &admin()).await));
    let config = duduclaw_inference::transcription::config::TranscriptionConfig::load(home.path()).unwrap();
    assert_eq!(config.google_stt.language, "cmn-Hant-TW");
    let installed = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    for value in [json!({"project_id":"unit-project","bucket":"unit-bucket","diarization":true}), json!({"project_id":"unit-project","bucket":"unit-bucket","model":"long"})] {
        assert!(!ok(&handler.handle_system_update_config(json!({"transcription":{"engine":"google_stt","google_stt":value}}), &admin()).await));
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), installed);
    }
}

#[tokio::test]
async fn transcription_credentials_never_reach_a_custom_extension() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    struct Spy(AtomicUsize);
    #[async_trait::async_trait]
    impl crate::extension::GatewayExtension for Spy {
        fn rpc_methods(&self) -> Vec<String> { vec!["ext.duduclaw.probe".into()] }
        async fn handle_method(&self, _: &str, _: Value, _: &UserContext) -> Option<WsFrame> {
            self.0.fetch_add(1, Ordering::SeqCst);
            // A plugin trying to bypass the core ACL must not receive this call.
            Some(WsFrame::ok_response("", json!({"plugin": true})))
        }
    }
    let home = tempfile::tempdir().unwrap();
    let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let spy = Arc::new(Spy(AtomicUsize::new(0)));
    let handler = MethodHandler::with_extension(home.path().into(), spy.clone()).await;
    let mut employee = admin(); employee.role = UserRole::Employee;
    for ctx in [&admin(), &employee] {
        assert!(!ok(&handler.handle("transcription_credentials.set", json!({"google_stt_key":"private-key-canary"}), ctx).await));
        assert!(!ok(&handler.handle("transcription_credentials.unknown", json!({"secret":"private-key-canary"}), ctx).await));
        for params in [json!({"transcription_google_stt_key":"private-key-canary"}),
            json!({"transcription_credentials":{"google_stt_key":"private-key-canary"}}),
            json!({"transcription_credentials.google_stt_key_enc":"private-key-canary"})] {
            assert!(!ok(&handler.handle("system.update_config", params.clone(), ctx).await));
            assert!(!ok(&handler.handle_system_update_config(params, ctx).await));
        }
    }
    assert!(!ok(&handler.handle("transcription_credentials.clear", json!({}), &employee).await));
    assert!(!home.path().join("config.toml").exists());
    assert!(ok(&handler.handle("transcription_credentials.clear", json!({}), &admin()).await));
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};
    let private = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap()
        .to_pkcs8_pem(LineEnding::LF).unwrap();
    let raw = json!({"type":"service_account", "client_email":"test@unit.iam.gserviceaccount.com", "private_key":private.as_str()}).to_string();
    let saved = handler.handle("transcription_credentials.set", json!({"google_stt_key":raw}), &admin()).await;
    assert!(ok(&saved));
    assert!(!serde_json::to_string(&saved).unwrap().contains("PRIVATE KEY"));
    assert!(!std::fs::read_to_string(home.path().join("config.toml")).unwrap().contains("PRIVATE KEY"));
    let before = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let rejected = handler.handle("config.raw.set", json!({
        "file":"config", "base_hash":super::super::config_commit::content_hash(&before),
        "content":"[transcription_credentials]\ngoogle_stt_key='private-key-canary'",
    }), &admin()).await;
    assert!(!ok(&rejected));
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), before);
    assert_eq!(spy.0.load(Ordering::SeqCst), 0);
    assert!(ok(&handler.handle("ext.duduclaw.probe", json!({}), &admin()).await));
    assert_eq!(spy.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn config_commit_guards_reject_a_demoted_admin_after_precheck() {
    use super::super::{config_commit, system_update_config::require_live_admin_at_commit};
    let home = tempfile::tempdir().unwrap();
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let user = db.create_user("commit-admin@example.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
    let mut ctx = admin();
    ctx.user_id = user.id.clone();
    require_live_admin_at_commit(home.path(), &ctx).unwrap();
    let path = home.path().join("config.toml");
    let original = "[transcription]\nengine='whisper'\nmax_bytes=1000\n";
    let replacement = "[transcription]\nengine='whisper'\nmax_bytes=2000\n";
    std::fs::write(&path, original).unwrap();
    db.update_user(&user.id, None, Some(UserRole::Employee), None).unwrap();
    assert!(config_commit::commit_text_locked_with_guard(&path, &config_commit::content_hash(original), replacement,
        || require_live_admin_at_commit(home.path(), &ctx)).is_err());
    let commit_home = home.path().to_path_buf();
    assert!(config_commit::commit_table_locked_with_guard(&path, config_commit::content_hash(original), &replacement.parse().unwrap(),
        move || require_live_admin_at_commit(&commit_home, &ctx)).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[tokio::test]
async fn raw_and_transcription_commits_fail_closed_without_a_readable_user_database() {
    for corrupt in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        if corrupt { std::fs::write(home.path().join("users.db"), "invalid database").unwrap(); }
        let original = "[transcription]\nengine='whisper'\nmax_bytes=1000\n";
        std::fs::write(home.path().join("config.toml"), original).unwrap();
        // Even the system token cannot commit while the authority DB is unavailable.
        assert!(!ok(&handler.handle_system_update_config(json!({"transcription":{"engine":"whisper","max_bytes":2000}}), &admin()).await));
        assert!(!ok(&handler.handle_config_raw_set(json!({"file":"config", "base_hash":super::super::config_commit::content_hash(original),
            "content":"[transcription]\nengine='whisper'\nmax_bytes=2000\n"}), &admin()).await));
        assert!(!ok(&handler.handle("transcription_credentials.clear", json!({}), &admin()).await));
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
        assert!(!std::fs::read_dir(home.path()).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().contains(".bak-")));
        if !corrupt { assert!(!home.path().join("users.db").exists()); }
    }
}

#[tokio::test]
async fn transcription_credential_audits_include_actor_action_and_result_without_secrets() {
    let home = tempfile::tempdir().unwrap();
    let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let mut employee = admin();
    employee.user_id = "denied-actor".into();
    employee.role = UserRole::Employee;
    assert!(!ok(&handler.handle("transcription_credentials.set", json!({"google_stt_key":"audit-secret-canary"}), &employee).await));
    assert!(!ok(&handler.handle("transcription_credentials.clear", json!({}), &employee).await));
    assert!(!ok(&handler.handle("transcription_credentials.set", json!({"google_stt_key":"audit-secret-canary"}), &admin()).await));
    assert!(ok(&handler.handle("transcription_credentials.clear", json!({}), &admin()).await));
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!audit.contains("audit-secret-canary"));
    let rows: Vec<Value> = audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event_type"].as_str().is_some_and(|event| event.starts_with("transcription_credentials."))).collect();
    assert_eq!(rows.len(), 4);
    for (row, (actor, action, status)) in rows.iter().zip([
        ("denied-actor", "set", "refused"), ("denied-actor", "clear", "refused"),
        ("system", "set", "refused"), ("system", "clear", "success"),
    ]) {
        assert_eq!(row["details"], json!({"actor_user_id":actor,"action":action,"status":status}));
    }
}
