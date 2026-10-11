use super::*;

fn assert_refused(frame: WsFrame) {
    match frame {
        WsFrame::Response {
            error: Some(error), ..
        } => {
            assert!(
                error.to_string().contains("Product profile")
                    || error.to_string().contains("product.profile"),
                "{error}"
            );
        }
        other => panic!("Expected product policy refusal: {other:?}"),
    }
}

#[tokio::test]
async fn restricted_rejects_configuration_and_host_setup_before_side_effects() {
    let home = tempfile::tempdir().unwrap();
    let original = "[product]\nprofile='restricted'\n";
    std::fs::write(home.path().join("config.toml"), original).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    assert_refused(
        handler
            .handle_channels_add(json!({ "type": "slack", "config": {"token":"test"} }))
            .await,
    );
    assert_refused(handler.handle_channels_test(json!({"type":"slack"})).await);
    assert_refused(
        handler
            .handle_accounts_add(
                json!({"id":"test", "provider":"openai", "key":"test", "type":"api_key"}),
            )
            .await,
    );
    assert_refused(
        handler
            .handle_runtime_install(json!({"provider":"claude"}), &UserContext::admin_fallback())
            .await,
    );
    assert_refused(
        handler
            .handle_cli_login_start(json!({"runtime":"claude"}))
            .await,
    );
    assert_refused(
        handler
            .handle_agents_create(json!({"name":"policy-test", "runtime":{"provider":"claude"}}))
            .await,
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
        original
    );
    assert!(!home.path().join("agents/policy-test").exists());
}

#[tokio::test]
async fn raw_editor_cannot_change_profile_or_reintroduce_blocked_provider() {
    let home = tempfile::tempdir().unwrap();
    let original = "[product]\nprofile='restricted'\n";
    std::fs::write(home.path().join("config.toml"), original).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for content in [
        "[product]\nprofile='standard'\n".to_string(),
        format!(
            "{original}[[accounts]]\nid='test'\ntype='api_key'\nprovider='openai'\napi_key='test'\n"
        ),
    ] {
        let frame = handler.handle_config_raw_set(json!({"file":"config", "base_hash":super::super::config_commit::content_hash(original), "content":content}), &UserContext::admin_fallback()).await;
        assert_refused(frame);
        assert_eq!(
            std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
            original
        );
    }
    assert_refused(
        handler
            .handle_system_update_config(
                json!({"product":{"profile":"standard"}}),
                &UserContext::admin_fallback(),
            )
            .await,
    );
}
