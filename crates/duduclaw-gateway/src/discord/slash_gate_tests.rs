//! Every Discord slash command and component press passes the DuDuClaw
//! access policy on the real invoker before dispatch (security review H3),
//! fail-closed (H1). Discord permissions only narrow it.

use super::*;
use crate::channel_reply::access_gate_tests::{break_settings_store, reply_ctx};

async fn gate(ctx: &ReplyContext, guild: &str, user: &str) -> Option<String> {
    discord_interaction_gate(ctx, "A1", guild, "C1", user, "").await
}

#[tokio::test]
async fn blocked_member_is_refused_before_any_slash_command() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    assert_eq!(gate(&ctx, "G1", "H1").await, None, "no policy: open");
    assert_eq!(gate(&ctx, "", "H1").await, None, "no policy: open in a DM");
    ctx.channel_settings
        .set("discord", "global", keys::BLOCKED_USERS, r#"["H1"]"#)
        .await
        .unwrap();
    assert_eq!(gate(&ctx, "G1", "H1").await.as_deref(), Some(""));
    assert_eq!(gate(&ctx, "", "H1").await.as_deref(), Some(""));
    assert_eq!(gate(&ctx, "G1", "H2").await, None);
    // An interaction without a user never runs once a policy exists.
    assert!(gate(&ctx, "G1", "").await.is_some());
}

#[tokio::test]
async fn pairing_policy_answers_with_the_pairing_hint() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("discord", "global", keys::REQUIRE_PAIRING, "true")
        .await
        .unwrap();
    let hint = gate(&ctx, "G1", "H1").await.unwrap();
    assert!(hint.starts_with('🔒'), "{hint}");
}

#[tokio::test]
async fn unreadable_policy_denies_slash_commands() {
    for broken in ["sql", "json", "bool"] {
        let home = tempfile::tempdir().unwrap();
        let ctx = reply_ctx(home.path());
        match broken {
            "sql" => break_settings_store(home.path()),
            "json" => ctx
                .channel_settings
                .set("discord", "global", keys::BLOCKED_USERS, "nope")
                .await
                .unwrap(),
            _ => ctx
                .channel_settings
                .set("discord", "global", keys::REQUIRE_PAIRING, "maybe")
                .await
                .unwrap(),
        }
        assert_eq!(gate(&ctx, "G1", "H1").await.as_deref(), Some(""), "{broken}");
        assert_eq!(gate(&ctx, "", "H1").await.as_deref(), Some(""), "{broken} DM");
    }
}

#[tokio::test]
async fn unauthorised_guild_is_refused_for_every_command() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings
        .set("discord", "global", keys::ALLOWED_GUILDS, r#"["G-ok"]"#)
        .await
        .unwrap();
    assert!(gate(&ctx, "G1", "H1").await.is_some_and(|reply| reply.starts_with('❌')));
    assert_eq!(gate(&ctx, "G-ok", "H1").await, None);
}

/// Structural: the gate runs once before the slash-command `match` and
/// before any component action, so no arm can be reached without it.
#[test]
fn gate_precedes_every_dispatch() {
    const SRC: &str = include_str!("../discord.rs");
    for (handler, dispatch) in [
        ("async fn handle_slash_command(", "match cmd_name {"),
        ("async fn handle_component_interaction(", "parse_gintent_action(custom_id)"),
    ] {
        let (_, body) = SRC.split_once(handler).expect("handler moved");
        let gate_at = body.find("discord_interaction_gate(").expect("gate missing");
        let dispatch_at = body.find(dispatch).expect("dispatch moved");
        assert!(gate_at < dispatch_at, "{handler} dispatches before the access gate");
    }
}
