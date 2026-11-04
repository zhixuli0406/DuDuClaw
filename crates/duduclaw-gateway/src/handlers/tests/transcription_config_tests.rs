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
    let original = "# operator settings\n[transcription]\nengine='whisper'\nmax_bytes=1000\n";
    std::fs::write(home.path().join("config.toml"), original).unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    for invalid in [json!({"engine":"other"}), json!({"engine":"gemini", "model":"missing"}),
        json!({"engine":"whisper", "max_bytes":0}), json!({"engine":"whisper", "upload_url":"https://example.com"}), Value::Null] {
        assert!(!ok(&handler.handle_system_update_config(json!({"transcription":invalid}), &admin()).await));
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
    }
}
