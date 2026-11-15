//! Google Chat commands (HTTP endpoint and Pub/Sub share `handle_message`)
//! run behind `command_preflight`: a policy that cannot be read or parsed
//! denies (security review H1); no policy keeps a personal install open.

use crate::channel_reply::access_gate_tests::{break_settings_store, reply_ctx};
use crate::channel_settings::keys;
use crate::chat_commands::CommandOrigin;

fn origin<'a>(sender: &'a str, text: &'a str) -> CommandOrigin<'a> {
    CommandOrigin {
        channel: "googlechat",
        gate_session: "googlechat:spaces/A",
        command_session: "googlechat:spaces/A",
        sender: crate::ccr_runtime::reply_principal_for_sender(sender),
        text,
        shared: true,
    }
}

async fn preflight(ctx: &crate::channel_reply::ReplyContext, sender: &str, text: &str) -> Option<String> {
    let cmd = crate::chat_commands::parse_command(text, None).unwrap();
    crate::chat_commands::command_preflight(&cmd, ctx, &origin(sender, text)).await
}

#[tokio::test]
async fn no_policy_allows_commands() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    for text in ["/status", "/help", "/undo"] {
        assert_eq!(preflight(&ctx, "users/a", text).await, None, "{text}");
    }
}

#[tokio::test]
async fn unreadable_policy_denies_commands() {
    for broken in ["sql", "json", "bool"] {
        let home = tempfile::tempdir().unwrap();
        let ctx = reply_ctx(home.path());
        match broken {
            "sql" => break_settings_store(home.path()),
            "json" => ctx
                .channel_settings
                .set("googlechat", "global", keys::BLOCKED_USERS, "[users/x")
                .await
                .unwrap(),
            _ => ctx
                .channel_settings
                .set("googlechat", "global", keys::REQUIRE_PAIRING, "on")
                .await
                .unwrap(),
        }
        for text in ["/status", "/undo", "/new"] {
            assert_eq!(preflight(&ctx, "users/a", text).await.as_deref(), Some(""), "{broken} {text}");
        }
    }
}

#[tokio::test]
async fn unidentified_sender_is_denied_once_a_policy_exists() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("googlechat", "global", keys::BLOCKED_USERS, r#"["users/bad"]"#)
        .await
        .unwrap();
    assert_eq!(preflight(&ctx, "unknown", "/status").await.as_deref(), Some(""));
    assert_eq!(preflight(&ctx, "users/bad", "/status").await.as_deref(), Some(""));
    assert_eq!(preflight(&ctx, "users/good", "/status").await, None);
}
