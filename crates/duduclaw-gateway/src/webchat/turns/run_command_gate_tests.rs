//! WebChat chat commands meet the same access gate as a normal message,
//! on the same session and sender, before any command handling (security
//! review M1), fail-closed (H1).

use super::webchat_command_gate;
use crate::channel_reply::access_gate_tests::{break_settings_store, reply_ctx};
use crate::channel_settings::keys;

const SID: &str = "webchat:x";
const USER: &str = "webchat:u";

#[tokio::test]
async fn no_policy_lets_commands_run() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    for text in ["/new", "/undo", "/status"] {
        assert_eq!(webchat_command_gate(&ctx, SID, USER, text).await, None, "{text}");
    }
}

#[tokio::test]
async fn blocked_user_cannot_run_commands() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("webchat", "global", keys::BLOCKED_USERS, r#"["webchat:u"]"#)
        .await
        .unwrap();
    for text in ["/new", "/undo", "/status"] {
        assert_eq!(webchat_command_gate(&ctx, SID, USER, text).await.as_deref(), Some(""), "{text}");
    }
    assert_eq!(webchat_command_gate(&ctx, SID, "webchat:other", "/status").await, None);
}

#[tokio::test]
async fn pairing_gets_only_the_generic_pairing_hint() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("webchat", "global", keys::REQUIRE_PAIRING, "true")
        .await
        .unwrap();
    let reply = webchat_command_gate(&ctx, SID, USER, "/status").await.unwrap();
    let normal = crate::channel_reply::check_user_access_gate(&ctx, SID, USER, "hello")
        .await
        .unwrap();
    assert_eq!(reply, normal, "a command must get exactly what a message gets");
}

#[tokio::test]
async fn unreadable_policy_denies_commands() {
    for broken in ["sql", "json"] {
        let home = tempfile::tempdir().unwrap();
        let ctx = reply_ctx(home.path());
        if broken == "sql" {
            break_settings_store(home.path());
        } else {
            ctx.channel_settings
                .set("webchat", "global", keys::ALLOWED_USERS, "not json")
                .await
                .unwrap();
        }
        // Silence, never an error string with internal detail.
        assert_eq!(webchat_command_gate(&ctx, SID, USER, "/new").await.as_deref(), Some(""), "{broken}");
    }
}

/// Structural: in `run`, the gate comes before command parsing / handling.
#[test]
fn gate_precedes_command_handling() {
    const SRC: &str = include_str!("run.rs");
    let (_, body) = SRC.split_once("pub(super) async fn run(").expect("run moved");
    let gate_at = body.find("webchat_command_gate(").expect("gate missing");
    for handler in ["parse_command(", "authorize_command(", "handle_command"] {
        let at = body.find(handler).expect("handler moved");
        assert!(gate_at < at, "{handler} runs before the access gate");
    }
}
