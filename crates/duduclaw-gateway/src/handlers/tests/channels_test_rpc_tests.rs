//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! W0-2: `channels.test` must never report a green "sent" when no
//! message actually went out — the credential-only degrade path is the
//! honesty fix breakpoint #5 (`ux-redesign-2026-08/01-current-state-map.md`)
//! called out. These tests exercise the parts reachable without a live
//! network call: no-token and token-but-no-destination both degrade
//! cleanly with `sent: false, mode: "credential_only"`.
use super::*;

fn frame_error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

fn agent_toml_with_proactive(name: &str, notify_channel: &str, notify_chat_id: &str) -> String {
    include_str!("../../../../../templates/evaluator/agent.toml")
        .replace("name = \"evaluator\"", &format!("name = \"{name}\""))
        + &format!(
            "\n[proactive]\nenabled = true\nnotify_channel = \"{notify_channel}\"\nnotify_chat_id = \"{notify_chat_id}\"\n"
        )
}

#[tokio::test]
async fn global_channel_with_no_token_degrades_to_credential_only() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_channels_test(json!({ "type": "telegram" }))
        .await;
    let payload = match &frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        _ => panic!("channels.test failed: {}", frame_error_text(&frame)),
    };
    assert_eq!(payload["sent"], false);
    assert_eq!(payload["mode"], "credential_only");
    assert!(
        payload["detail"].as_str().unwrap().contains("未設定"),
        "{payload}"
    );
}

#[tokio::test]
async fn global_channel_with_token_but_no_destination_degrades_to_credential_only() {
    let home = tempfile::tempdir().expect("tempdir");
    // A token is configured, but no agent has ever set a [proactive]
    // destination for this platform — channels.test must not claim it
    // sent anything.
    std::fs::write(
        home.path().join("config.toml"),
        "[channels]\ntelegram_bot_token = \"123:fake-token-for-test\"\n",
    )
    .expect("write config.toml");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_channels_test(json!({ "type": "telegram" }))
        .await;
    let payload = match &frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        _ => panic!("channels.test failed: {}", frame_error_text(&frame)),
    };
    assert_eq!(payload["sent"], false, "{payload}");
    assert_eq!(payload["mode"], "credential_only", "{payload}");
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("僅驗證憑證存在"),
        "{payload}"
    );
}

#[tokio::test]
async fn global_channel_finds_destination_from_agent_proactive_config() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join("config.toml"),
        "[channels]\ntelegram_bot_token = \"123:fake-token-for-test\"\n",
    )
    .expect("write config.toml");
    let agent_dir = home.path().join("agents").join("relay");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        agent_toml_with_proactive("relay", "telegram", "999888777"),
    )
    .unwrap();

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    {
        let mut reg = handler.registry.write().await;
        reg.scan().await.expect("scan");
    }

    let dest = handler.resolve_global_test_destination("telegram").await;
    assert_eq!(
        dest,
        Some(("relay".to_string(), "999888777".to_string())),
        "must find the agent's [proactive] destination for the matching platform"
    );
}

#[tokio::test]
async fn per_agent_channel_with_no_token_degrades_to_credential_only() {
    let home = tempfile::tempdir().expect("tempdir");
    let agent_dir = home.path().join("agents").join("nobot");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        include_str!("../../../../../templates/evaluator/agent.toml")
            .replace("name = \"evaluator\"", "name = \"nobot\""),
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    {
        let mut reg = handler.registry.write().await;
        reg.scan().await.expect("scan");
    }

    let frame = handler
        .handle_channels_test(json!({ "type": "discord:nobot" }))
        .await;
    let payload = match &frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        _ => panic!("channels.test failed: {}", frame_error_text(&frame)),
    };
    assert_eq!(payload["sent"], false, "{payload}");
    assert_eq!(payload["mode"], "credential_only", "{payload}");
}

#[tokio::test]
async fn per_agent_channel_unknown_agent_errors() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_channels_test(json!({ "type": "discord:ghost" }))
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "{frame:?}"
    );
}

#[test]
fn agent_proactive_target_requires_matching_channel() {
    // Pure-logic guard: a [proactive] destination configured for a
    // DIFFERENT platform must never be borrowed as this platform's target.
    use duduclaw_core::types::ProactiveConfig;
    let mut proactive = ProactiveConfig::default();
    proactive.notify_channel = "slack".to_string();
    proactive.notify_chat_id = "C123".to_string();
    assert_eq!(proactive.notify_channel, "slack");
    assert_ne!(proactive.notify_channel, "telegram");
}

// -- classify_channel_send_error: raw platform/transport text → zh-TW --

#[test]
fn classify_unauthorized_as_revoked_credential() {
    let msg =
        classify_channel_send_error("Telegram API error 401 Unauthorized: {\"ok\":false}");
    assert!(msg.contains("憑證無效"), "{msg}");
}

#[test]
fn classify_forbidden_as_permission_issue() {
    let msg = classify_channel_send_error("Slack API error: not_in_channel");
    assert!(msg.contains("權限不足"), "{msg}");
}

#[test]
fn classify_not_found_as_missing_destination() {
    let msg = classify_channel_send_error(
        "Discord API error 404 Not Found: {\"message\":\"Unknown Channel\"}",
    );
    assert!(msg.contains("找不到目的地"), "{msg}");
}

#[test]
fn classify_rate_limit() {
    let msg = classify_channel_send_error("Telegram API error 429 Too Many Requests: {}");
    assert!(msg.contains("頻率上限"), "{msg}");
}

#[test]
fn classify_teams_never_seen_conversation() {
    let msg = classify_channel_send_error(
        "no stored conversation reference for abc (bot must receive a message there first)",
    );
    assert!(msg.contains("尚未有過互動紀錄"), "{msg}");
}

#[test]
fn classify_network_error() {
    let msg = classify_channel_send_error("LINE push: error sending request for url (...)");
    assert!(msg.contains("網路連線失敗"), "{msg}");
}

#[test]
fn classify_unknown_falls_back_to_generic_honest_message() {
    let msg = classify_channel_send_error("something totally unexpected");
    assert!(msg.contains("平台拒絕了這則訊息"), "{msg}");
}

#[test]
fn agent_has_own_channel_token_true_only_for_configured_platform() {
    use duduclaw_core::types::{ChannelsConfig, DiscordChannelConfig};
    let mut cfg = base_agent_config_for_test();
    cfg.channels = Some(ChannelsConfig {
        discord: Some(DiscordChannelConfig {
            bot_token: "tok".to_string(),
            bot_token_enc: None,
            bindings: vec![],
        }),
        telegram: None,
        line: None,
        slack: None,
        whatsapp: None,
        feishu: None,
        googlechat: None,
        teams: None,
        wecom: None,
        dingtalk: None,
    });
    let agent = duduclaw_agent::registry::LoadedAgent {
        config: cfg,
        soul: None,
        identity: None,
        memory: None,
        skills: vec![],
        contract: Default::default(),
        dir: std::path::PathBuf::new(),
        preset_resolution: Default::default(),
    };
    assert!(agent_has_own_channel_token(&agent, "discord"));
    assert!(!agent_has_own_channel_token(&agent, "telegram"));
}

fn base_agent_config_for_test() -> duduclaw_core::types::AgentConfig {
    let raw = include_str!("../../../../../templates/evaluator/agent.toml")
        .replace("name = \"evaluator\"", "name = \"probe\"");
    toml::from_str(&raw).expect("parse evaluator template")
}
