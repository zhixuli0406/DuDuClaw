//! Unit tests for the per-employee channel helpers (pure parts).

use super::*;
use serde_json::json;

#[test]
fn telegram_bot_id_is_the_numeric_prefix() {
    assert_eq!(
        conflicts::telegram_bot_id("123456:AAE-secret_x"),
        Some("123456")
    );
    assert_eq!(conflicts::telegram_bot_id(" 123456:x "), Some("123456"));
    assert_eq!(conflicts::telegram_bot_id("abc:x"), None);
    assert_eq!(conflicts::telegram_bot_id(":x"), None);
    assert_eq!(conflicts::telegram_bot_id("no-colon"), None);
}

#[test]
fn get_me_classification_separates_rejection_from_outage() {
    let ok = json!({"ok": true, "result": {"id": 1, "username": "sales_bot"}});
    assert_eq!(
        telegram_probe::classify(&ok),
        telegram_probe::Probe::Verified {
            username: "sales_bot".into()
        }
    );
    let bad = json!({"ok": false, "error_code": 401, "description": "Unauthorized"});
    assert_eq!(
        telegram_probe::classify(&bad),
        telegram_probe::Probe::Rejected("Unauthorized".into())
    );
    let gone = json!({"ok": false, "error_code": 404, "description": "Not Found"});
    assert!(matches!(
        telegram_probe::classify(&gone),
        telegram_probe::Probe::Rejected(_)
    ));
    let busy = json!({"ok": false, "error_code": 429, "description": "Too Many Requests"});
    assert!(matches!(
        telegram_probe::classify(&busy),
        telegram_probe::Probe::Unverified(_)
    ));
}

#[test]
fn project_number_must_be_digits() {
    assert_eq!(
        googlechat_config::validate_project_number(" 123456789012 ").unwrap(),
        "123456789012"
    );
    assert!(googlechat_config::validate_project_number("my-project-id").is_err());
    assert!(googlechat_config::validate_project_number("12345").is_err());
    assert!(googlechat_config::validate_project_number("").is_err());
}

#[test]
fn service_account_json_is_checked_without_network() {
    assert!(googlechat_config::validate_service_account_json("not json").is_err());
    assert!(googlechat_config::validate_service_account_json("{}").is_err());
    let not_a_key = json!({
        "type": "service_account",
        "client_email": "bot@p.iam.gserviceaccount.com",
        "private_key": "-----BEGIN PRIVATE KEY-----\nFAKE\n-----END PRIVATE KEY-----\n"
    })
    .to_string();
    assert!(googlechat_config::validate_service_account_json(&not_a_key).is_err());
    let wrong_type =
        json!({"type": "authorized_user", "client_email": "a@b", "private_key": "x"}).to_string();
    assert!(googlechat_config::validate_service_account_json(&wrong_type).is_err());
}

#[test]
fn token_uri_must_be_googles_endpoint() {
    assert!(googlechat_config::token_uri_allowed("https://oauth2.googleapis.com/token"));
    assert!(!googlechat_config::token_uri_allowed("https://evil.example/token"));
    assert!(!googlechat_config::token_uri_allowed("http://oauth2.googleapis.com/token"));
    assert!(!googlechat_config::token_uri_allowed("https://oauth2.googleapis.com.evil.com/token"));
    assert!(!googlechat_config::token_uri_allowed("https://oauth2.googleapis.com:8443/token"));
    // Debug builds (tests) accept a loopback fake.
    assert!(googlechat_config::token_uri_allowed("http://127.0.0.1:9/token"));
}

#[test]
fn store_writes_ciphertext_only_and_removes_sections() {
    let mut t: toml::Table = "[channels.telegram]\nbot_token = \"plain\"\n"
        .parse()
        .unwrap();
    store::apply_telegram(&mut t, "CIPHER").unwrap();
    let tg = t["channels"]["telegram"].as_table().unwrap();
    assert!(tg.get("bot_token").is_none(), "stale plaintext is dropped");
    assert_eq!(tg["bot_token_enc"].as_str(), Some("CIPHER"));

    store::apply_googlechat(&mut t, "123456789", Some("SAENC")).unwrap();
    store::apply_googlechat(&mut t, "987654321", None).unwrap();
    let gc = t["channels"]["googlechat"].as_table().unwrap();
    assert_eq!(gc["project_number"].as_str(), Some("987654321"));
    assert_eq!(
        gc["service_account_json_enc"].as_str(),
        Some("SAENC"),
        "key kept on number-only change"
    );

    assert!(store::remove_channel(&mut t, "telegram"));
    assert!(!store::remove_channel(&mut t, "telegram"));
    assert!(t["channels"].get("googlechat").is_some());
}

#[test]
fn state_word_reports_plain_states() {
    let none = status::Runtime::default();
    assert_eq!(status::state_word(false, &none), "not_configured");
    assert_eq!(status::state_word(true, &none), "connecting");
    let up = status::Runtime {
        present: true,
        connected: true,
        ..Default::default()
    };
    assert_eq!(status::state_word(true, &up), "connected");
    let down = status::Runtime {
        present: true,
        error: Some("Bot Token 無效".into()),
        ..Default::default()
    };
    assert_eq!(status::state_word(true, &down), "error");
    let retry = status::Runtime {
        present: true,
        error: Some("connecting — dns".into()),
        ..Default::default()
    };
    assert_eq!(status::state_word(true, &retry), "connecting");
}

#[test]
fn endpoint_path_and_label_shapes() {
    assert_eq!(
        googlechat_endpoint_path("sales-bot"),
        "/webhook/googlechat/agents/sales-bot"
    );
    assert_eq!(label("telegram", "sales-bot"), "telegram:sales-bot");
}
