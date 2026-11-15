//! Telegram commands and buttons are gated on the human sender, never on
//! the group chat id every member shares (security review H2), and fail
//! closed when the access policy cannot be read (H1).

use super::*;
use crate::channel_reply::access_gate_tests::{break_settings_store, reply_ctx};
use crate::channel_settings::keys;

const GROUP: i64 = -100_123;

#[tokio::test]
async fn blocked_member_cannot_run_group_commands_or_press_buttons() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("telegram", "global", keys::BLOCKED_USERS, r#"["42"]"#)
        .await
        .unwrap();
    for text in ["/undo", "/handoff line", "/new", "/status", ""] {
        assert_eq!(
            telegram_command_gate(&ctx, GROUP, None, Some(42), text).await.as_deref(),
            Some(""),
            "blocked member ran {text:?}"
        );
        assert_eq!(
            telegram_command_gate(&ctx, GROUP, Some(7), Some(42), text).await.as_deref(),
            Some(""),
            "blocked member ran {text:?} in a topic"
        );
        // Another member of the same group is unaffected.
        assert_eq!(telegram_command_gate(&ctx, GROUP, None, Some(43), text).await, None);
    }
    // The old principal (the shared chat id) is not what is checked any more.
    assert_ne!(reply_principal_for_sender(Some(42)), GROUP.to_string());
}

#[tokio::test]
async fn senderless_command_is_denied_only_when_a_policy_exists() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    assert_eq!(telegram_command_gate(&ctx, GROUP, None, None, "/status").await, None);
    ctx.channel_settings
        .set("telegram", "global", keys::BLOCKED_USERS, r#"["999"]"#)
        .await
        .unwrap();
    assert_eq!(
        telegram_command_gate(&ctx, GROUP, None, None, "/status").await.as_deref(),
        Some("")
    );
}

#[tokio::test]
async fn personal_pairing_still_works_on_the_sender() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("telegram", "global", keys::REQUIRE_PAIRING, "true")
        .await
        .unwrap();
    // A private chat's id is the user's own id.
    let me = 4242_i64;
    let hint = telegram_command_gate(&ctx, me, None, Some(me), "/undo").await.unwrap();
    assert!(hint.starts_with('🔒'), "{hint}");
    let code = ctx.access_control.generate_pairing_code("4242").await.unwrap();
    let verdict = telegram_command_gate(&ctx, me, None, Some(me), &format!("/pair {code}"))
        .await
        .unwrap();
    assert!(verdict.starts_with('✅'), "{verdict}");
    assert_eq!(telegram_command_gate(&ctx, me, None, Some(me), "/undo").await, None);
    // A paired member does not pair the whole group.
    assert!(
        telegram_command_gate(&ctx, GROUP, None, Some(77), "/undo")
            .await
            .is_some_and(|reply| reply.starts_with('🔒'))
    );
}

#[tokio::test]
async fn unreadable_policy_denies_commands_buttons_and_start() {
    for broken in ["sql", "json"] {
        let home = tempfile::tempdir().unwrap();
        let ctx = reply_ctx(home.path());
        assert_eq!(telegram_command_gate(&ctx, GROUP, None, Some(42), "/undo").await, None);
        assert!(telegram_start_allowed(&ctx, 42, None, Some(42)).await);
        if broken == "sql" {
            break_settings_store(home.path());
        } else {
            ctx.channel_settings
                .set("telegram", "global", keys::BLOCKED_USERS, "not json")
                .await
                .unwrap();
        }
        assert_eq!(
            telegram_command_gate(&ctx, GROUP, None, Some(42), "/undo").await.as_deref(),
            Some(""),
            "{broken}"
        );
        assert!(!telegram_start_allowed(&ctx, 42, None, Some(42)).await, "{broken}");
    }
}

#[tokio::test]
async fn start_onboarding_open_to_unpaired_but_not_to_blocked() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("telegram", "global", keys::REQUIRE_PAIRING, "true")
        .await
        .unwrap();
    assert!(telegram_start_allowed(&ctx, 42, None, Some(42)).await);
    ctx.channel_settings
        .set("telegram", "global", keys::BLOCKED_USERS, r#"["42"]"#)
        .await
        .unwrap();
    assert!(!telegram_start_allowed(&ctx, 42, None, Some(42)).await);
    assert!(telegram_start_allowed(&ctx, 43, None, Some(43)).await);
}
