//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! Item: channels.add plaintext-beside-_enc parity. When encryption is
//! available, secrets persist ONLY as `_enc` (plaintext key removed, not
//! blanked). telegram/discord/line global tokens are the documented
//! exception (plaintext-only presence checks elsewhere in this file).
use super::*;

async fn add_channel(handler: &MethodHandler, ty: &str, cfg: Value) -> WsFrame {
    handler
        .handle_channels_add(json!({ "type": ty, "config": cfg }))
        .await
}

fn channels_table(home: &std::path::Path) -> toml::Table {
    let raw = std::fs::read_to_string(home.join("config.toml")).expect("config.toml");
    let table: toml::Table = raw.parse().expect("parse config.toml");
    table["channels"].as_table().cloned().expect("[channels]")
}

#[tokio::test]
async fn slack_secrets_are_enc_only_and_read_back() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Split so no contiguous vendor-shaped literal sits in the source: a
    // synthetic token with a real vendor shape trips source scanners exactly
    // like a live one.
    let bot_token = ["xoxb", "-bot-token"].concat();
    let app_token = ["xapp", "-app-token"].concat();
    let frame = add_channel(
        &handler,
        "slack",
        json!({ "token": bot_token, "secret": app_token }),
    )
    .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );

    let ch = channels_table(home.path());
    // Plaintext removed, _enc present.
    assert!(
        !ch.contains_key("slack_bot_token"),
        "plaintext bot token must be dropped"
    );
    assert!(ch.contains_key("slack_bot_token_enc"));
    assert!(
        !ch.contains_key("slack_app_token"),
        "plaintext app token must be dropped"
    );
    assert!(ch.contains_key("slack_app_token_enc"));

    // enc-only read path returns the original values.
    let bot = crate::config_crypto::read_encrypted_config_field(
        home.path(),
        "channels",
        "slack_bot_token",
    )
    .await;
    assert_eq!(bot, Some(bot_token));
    let app = crate::config_crypto::read_encrypted_config_field(
        home.path(),
        "channels",
        "slack_app_token",
    )
    .await;
    assert_eq!(app, Some(app_token));
}

#[tokio::test]
async fn whatsapp_token_enc_only_but_phone_number_id_plain() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = add_channel(
        &handler,
        "whatsapp",
        json!({
            "token": "wa-access-token",
            "secret": "123456789",              // phone_number_id — identifier
            "whatsapp_verify_token": "verify-me",
            "whatsapp_app_secret": "app-secret",
        }),
    )
    .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );

    let ch = channels_table(home.path());
    assert!(!ch.contains_key("whatsapp_access_token"));
    assert!(ch.contains_key("whatsapp_access_token_enc"));
    // Identifier stays plaintext (XC.3).
    assert_eq!(
        ch.get("whatsapp_phone_number_id").and_then(|v| v.as_str()),
        Some("123456789")
    );
    // Extra secrets are enc-only too.
    assert!(!ch.contains_key("whatsapp_verify_token"));
    assert!(ch.contains_key("whatsapp_verify_token_enc"));
    assert!(!ch.contains_key("whatsapp_app_secret"));
    assert!(ch.contains_key("whatsapp_app_secret_enc"));

    let tok = crate::config_crypto::read_encrypted_config_field(
        home.path(),
        "channels",
        "whatsapp_access_token",
    )
    .await;
    assert_eq!(tok.as_deref(), Some("wa-access-token"));
    let verify = crate::config_crypto::read_encrypted_config_field(
        home.path(),
        "channels",
        "whatsapp_verify_token",
    )
    .await;
    assert_eq!(verify.as_deref(), Some("verify-me"));
}

#[tokio::test]
async fn telegram_is_enc_only_and_still_counts_as_configured() {
    // Updated 2026-07: this test previously asserted the buggy behavior
    // (plaintext kept beside `_enc` for telegram/discord/line because the
    // presence checks were plaintext-only). The presence checks are now
    // `_enc`-aware, so these three channels are enc-only like the rest.
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Assembled at run time — see the note in `slack_secrets_are_enc_only…`.
    let tg_token = ["1234567890", ":AAtest", "TELEGRAMtokenFIXTUREvalue01"].concat();
    let frame = add_channel(&handler, "telegram", json!({ "token": tg_token })).await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );

    let ch = channels_table(home.path());
    assert!(
        !ch.contains_key("telegram_bot_token"),
        "plaintext telegram token must be dropped when encryption succeeds"
    );
    assert!(ch.contains_key("telegram_bot_token_enc"));

    // enc-only read path returns the original value.
    let tok = crate::config_crypto::read_encrypted_config_field(
        home.path(),
        "channels",
        "telegram_bot_token",
    )
    .await;
    assert_eq!(tok, Some(tg_token));

    // Presence checks see the enc-only channel.
    assert_eq!(
        handler.count_configured_channels().await,
        1,
        "enc-only telegram must count as configured"
    );
    let status = handler.handle_channels_status().await;
    let listed = match &status {
        WsFrame::Response {
            payload: Some(payload),
            ..
        } => payload["channels"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|c| c["name"] == "telegram")),
        _ => false,
    };
    assert!(
        listed,
        "enc-only telegram must appear in channels.status: {status:?}"
    );
}

#[tokio::test]
async fn agents_update_channel_tokens_are_enc_only_roundtrip() {
    // 2026-07 MED: the agents.update `set_channel_token` path used to
    // write plaintext + `_enc` and never strip. It now mirrors
    // channels.add: enc-only on successful encryption (keyfile-unavailable
    // falls back to legacy plaintext). Since v1.68 only the per-agent
    // telegram/discord/slack tokens are accepted.
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let created = handler
        .handle_agents_create(json!({ "name": "enc-bot", "display_name": "EncBot" }))
        .await;
    assert!(
        matches!(created, WsFrame::Response { ok: true, .. }),
        "{created:?}"
    );

    // Assembled at run time — see the note in `slack_secrets_are_enc_only…`.
    let tg_token = ["1234567891", ":AAagent", "TELEGRAMtokenFIXTUREvalu1"].concat();
    let updated = handler
        .handle_agents_update(json!({
            "agent_id": "enc-bot",
            "telegram_bot_token": tg_token,
            "wecom_corp_id": "corp-id-plain",
            "wecom_corp_secret": "corp-secret-value",
            "dingtalk_app_secret": "ding-secret-value",
        }))
        .await;
    assert!(
        matches!(updated, WsFrame::Response { ok: true, .. }),
        "{updated:?}"
    );

    let raw = std::fs::read_to_string(
        home.path()
            .join("agents")
            .join("enc-bot")
            .join("agent.toml"),
    )
    .expect("agent.toml");
    let table: toml::Table = raw.parse().expect("parse agent.toml");
    let channels = table["channels"].as_table().expect("[channels]");

    let tg = channels["telegram"].as_table().unwrap();
    assert!(
        !tg.contains_key("bot_token"),
        "plaintext must be stripped: {tg:?}"
    );
    let enc = tg["bot_token_enc"].as_str().unwrap();
    assert_eq!(
        crate::config_crypto::resolve_agent_token(
            &Some(enc.to_string()),
            "",
            home.path(),
            &duduclaw_security::secret_manager::SecretManagerConfig::default(),
        )
        .await
        .map(|s| s.expose_owned())
        .unwrap_or_default(),
        tg_token,
        "enc-only roundtrip must recover the original token"
    );

    // v1.68: per-agent WeCom / DingTalk credentials are no longer accepted
    // (those channels read only the global config), so nothing is written.
    assert!(!channels.contains_key("wecom"), "{channels:?}");
    assert!(!channels.contains_key("dingtalk"), "{channels:?}");
}
