//! The user access policy snapshot fails closed (security review H1), and
//! a personal install with no policy stays fully open.

use std::path::Path;

use super::*;
use crate::channel_settings::keys;

/// A reply context whose channel settings live in `home/sessions.db`.
pub(crate) fn reply_ctx(home: &Path) -> crate::channel_reply::ReplyContext {
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.join("agents")),
    ));
    let sessions = std::sync::Arc::new(
        crate::session::SessionManager::new(&home.join("sessions.db")).unwrap(),
    );
    let status: crate::channel_reply::ChannelStatusMap =
        std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    crate::channel_reply::ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
}

/// Make every channel-settings read fail with an SQL error.
pub(crate) fn break_settings_store(home: &Path) {
    rusqlite::Connection::open(home.join("sessions.db"))
        .unwrap()
        .execute_batch("DROP TABLE channel_settings")
        .unwrap();
}

/// Undo [`break_settings_store`] (an empty settings table again).
pub(crate) fn restore_settings_store(home: &Path) {
    drop(crate::channel_settings::ChannelSettingsManager::from_session_db(
        &home.join("sessions.db"),
    ))
}

fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ("global".to_string(), (*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn parse_distinguishes_unset_from_malformed() {
    let open = parse_user_access_rows(&[]).unwrap();
    assert!(open.is_open());
    // The legacy "cleared" encoding and an empty allowlist stay open.
    let cleared = parse_user_access_rows(&rows(&[
        (keys::ALLOWED_USERS, "[]"),
        (keys::BLOCKED_USERS, ""),
        (keys::REQUIRE_PAIRING, "false"),
    ]))
    .unwrap();
    assert!(cleared.is_open());
    let policy = parse_user_access_rows(&rows(&[
        (keys::ALLOWED_USERS, r#"["a"]"#),
        (keys::BLOCKED_USERS, r#"["b"]"#),
        (keys::REQUIRE_PAIRING, "true"),
    ]))
    .unwrap();
    assert_eq!(policy.allowed.as_deref(), Some(&["a".to_string()][..]));
    assert_eq!(policy.blocked, vec!["b".to_string()]);
    assert!(policy.require_pairing);
    for bad in [
        (keys::BLOCKED_USERS, "not json"),
        (keys::BLOCKED_USERS, "[1,2]"),
        (keys::ALLOWED_USERS, "{\"a\":1}"),
        (keys::REQUIRE_PAIRING, "yes"),
        (keys::REQUIRE_PAIRING, "TRUE"),
        (keys::REQUIRE_PAIRING, ""),
    ] {
        assert_eq!(
            parse_user_access_rows(&rows(&[bad])),
            Err(AccessPolicyError::Malformed),
            "{bad:?}"
        );
    }
    // Rows of another scope never count as the channel-wide policy.
    let other_scope = vec![(
        "G1".to_string(),
        keys::BLOCKED_USERS.to_string(),
        "not json".to_string(),
    )];
    assert!(parse_user_access_rows(&other_scope).unwrap().is_open());
}

#[tokio::test]
async fn snapshot_classifies_read_and_parse_errors_and_never_caches_them() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    assert!(read_user_access_policy(&ctx, "telegram").await.unwrap().is_open());

    ctx.channel_settings
        .set("telegram", "global", keys::BLOCKED_USERS, "not json")
        .await
        .unwrap();
    assert_eq!(
        read_user_access_policy(&ctx, "telegram").await,
        Err(AccessPolicyError::Malformed)
    );
    ctx.channel_settings
        .delete("telegram", "global", keys::BLOCKED_USERS)
        .await
        .unwrap();

    break_settings_store(home.path());
    assert_eq!(
        read_user_access_policy(&ctx, "telegram").await,
        Err(AccessPolicyError::Unavailable)
    );
    // The plain getter answers None for a failed read but must not cache it.
    assert_eq!(ctx.channel_settings.get("telegram", "global", keys::REQUIRE_PAIRING).await, None);
    restore_settings_store(home.path());
    let other = crate::channel_settings::ChannelSettingsManager::from_session_db(
        &home.path().join("sessions.db"),
    )
    .unwrap();
    other
        .set("telegram", "global", keys::REQUIRE_PAIRING, "true")
        .await
        .unwrap();
    assert_eq!(
        ctx.channel_settings
            .get("telegram", "global", keys::REQUIRE_PAIRING)
            .await
            .as_deref(),
        Some("true"),
        "a read error was cached as unset"
    );
    assert!(read_user_access_policy(&ctx, "telegram").await.unwrap().require_pairing);
}

#[tokio::test]
async fn unreadable_or_malformed_policy_denies_every_gate() {
    for broken in ["sql", "json", "bool"] {
        let home = tempfile::tempdir().unwrap();
        let ctx = reply_ctx(home.path());
        match broken {
            "sql" => break_settings_store(home.path()),
            "json" => ctx
                .channel_settings
                .set("googlechat", "global", keys::ALLOWED_USERS, "[oops")
                .await
                .unwrap(),
            _ => ctx
                .channel_settings
                .set("googlechat", "global", keys::REQUIRE_PAIRING, "1")
                .await
                .unwrap(),
        }
        let session = "googlechat:spaces/A";
        assert_eq!(
            check_user_access_gate(&ctx, session, "users/a", "hello").await.as_deref(),
            Some(""),
            "{broken}: normal message"
        );
        assert_eq!(
            check_command_access_gate(&ctx, session, "users/a", "/new").await.as_deref(),
            Some(""),
            "{broken}: command"
        );
        // `/pair` is no way around an unknown policy either.
        assert_eq!(
            check_command_access_gate(&ctx, session, "users/a", "/pair 123456").await.as_deref(),
            Some(""),
            "{broken}: pair"
        );
        assert_eq!(
            gate_direct_reply(&ctx, session, "users/a", "@").await.as_deref(),
            Some(""),
            "{broken}: direct reply"
        );
        // Non-channel sessions are not gated at all, as before.
        assert_eq!(check_user_access_gate(&ctx, "default", "u", "x").await, None);
    }
}

#[tokio::test]
async fn no_policy_keeps_every_channel_open_for_commands() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    for channel in GATED_CHANNELS {
        let session = format!("{channel}:conversation");
        assert_eq!(check_command_access_gate(&ctx, &session, "someone", "/status").await, None);
        // Even an unknown sender: a personal install has no policy to apply.
        assert_eq!(check_command_access_gate(&ctx, &session, "", "/status").await, None);
        assert_eq!(check_user_access_gate(&ctx, &session, "someone", "hi").await, None);
    }
}

#[tokio::test]
async fn command_without_sender_is_denied_once_a_policy_exists() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("slack", "global", keys::BLOCKED_USERS, r#"["U-bad"]"#)
        .await
        .unwrap();
    assert_eq!(
        check_command_access_gate(&ctx, "slack:group:C1", "", "/new").await.as_deref(),
        Some("")
    );
    assert_eq!(
        check_command_access_gate(&ctx, "slack:group:C1", "U-bad", "/new").await.as_deref(),
        Some("")
    );
    assert_eq!(check_command_access_gate(&ctx, "slack:group:C1", "U-good", "/new").await, None);
}
