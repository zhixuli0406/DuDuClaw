use super::*;

fn send() -> Value {
    json!({"metadata": {"agentId": "researcher"}, "configuration": {"returnImmediately": true},
        "message": {"messageId": "m-1", "role": "ROLE_USER", "parts": [{"text": "整理公開文件 🐾"}]}})
}

#[test]
fn a2a_version_and_legacy_method_mapping() {
    assert!(version(Some("1.0")).is_ok());
    for value in [None, Some(""), Some("0.3"), Some("1.0.0"), Some("2.0")] {
        assert_eq!(version(value).unwrap_err().code, -32009);
    }
    assert_eq!(canonical_method("message/send"), "SendMessage");
    assert_eq!(canonical_method("tasks/get"), "GetTask");
    assert_eq!(canonical_method("tasks/cancel"), "tasks/cancel");
    assert_eq!(minimum_role("unknown"), duduclaw_auth::UserRole::Admin);
}

#[test]
fn a2a_send_validates_v1_parts_and_rejects_unsupported_modes() {
    assert_eq!(parse_send(send()).unwrap().text, "整理公開文件 🐾");
    for (key, value, code) in [
        ("role", json!("user"), -32602), ("messageId", json!(""), -32602),
        ("parts", json!([]), -32602), ("parts", json!([{"text":"ok", "raw":"eA=="}]), -32005),
        ("parts", json!([{"kind":"text", "text":"ok"}]), -32005),
        ("taskId", json!("another-task"), -32004), ("contextId", json!("existing"), -32004),
    ] {
        let mut input = send(); input["message"][key] = value;
        assert_eq!(parse_send(input).err().unwrap().code, code, "{key}");
    }
    let mut input = send(); input["configuration"]["returnImmediately"] = json!(false);
    assert_eq!(parse_send(input).err().unwrap().code, -32004);
    let mut input = send(); input["configuration"]["taskPushNotificationConfig"] = json!({"url":"https://example.invalid"});
    assert_eq!(parse_send(input).err().unwrap().code, -32003);
    let mut input = send(); input["message"]["parts"] = json!([{"text":"字".repeat(4001)}]);
    assert_eq!(parse_send(input).err().unwrap().code, -32602);
}

#[test]
fn a2a_config_defaults_and_bad_values_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    assert!(Config::from_home(home.path()).is_err());
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    assert!(!Config::from_home(home.path()).unwrap().exposes("researcher"));
    for invalid in [
        "[a2a]\nexposed_agents=true",
        "[a2a]\nexposed_agents=['../researcher']",
        "[a2a]\nexposed_agents=['researcher']\npublic_url='http://example.invalid/a2a'",
        "[a2a]\nexposed_agents=['researcher']\npublic_url='https://example.invalid/other'",
        "[a2a]\nunknown=true",
    ] {
        std::fs::write(home.path().join("config.toml"), invalid).unwrap();
        assert!(Config::from_home(home.path()).is_err(), "{invalid}");
    }
    std::fs::write(home.path().join("config.toml"),
        "[a2a]\nexposed_agents=['researcher']\npublic_url='https://example.invalid/a2a'").unwrap();
    let config = Config::from_home(home.path()).unwrap();
    assert!(config.exposes("researcher"));
    assert!(!config.exposes("researcher-other"));
}

#[test]
fn a2a_error_details_and_task_states_are_v1() {
    let error = Error::missing().value();
    assert_eq!(error["code"], -32001);
    assert!(error["data"].is_array());
    assert_eq!(error["data"][0]["reason"], "TASK_NOT_FOUND");
    assert_eq!(state("cancelled"), "TASK_STATE_CANCELED");
    assert_eq!(state("needs_human"), "TASK_STATE_INPUT_REQUIRED");
    assert_eq!(state("done"), "TASK_STATE_COMPLETED");
}

#[test]
fn a2a_send_retains_message_identity_and_hashes_canonical_payload() {
    let value = send();
    let parsed = parse_send(value.clone()).unwrap();
    assert_eq!(parsed.message_id, "m-1");
    let reordered: Value = serde_json::from_str(&crate::a2a_signing::canonical_json(&value)).unwrap();
    assert_eq!(parsed.payload_sha256, parse_send(reordered).unwrap().payload_sha256);
    let mut changed = value;
    changed["message"]["parts"] = json!([{"text": "A different request"}]);
    assert_ne!(parsed.payload_sha256, parse_send(changed).unwrap().payload_sha256);
}
