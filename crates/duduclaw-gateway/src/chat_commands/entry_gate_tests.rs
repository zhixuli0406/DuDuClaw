//! W4 command matrix, using the same fixtures as the all-entry suite.
use super::*;
use crate::channel_reply::entry_gate_test_helpers::{owned_channel, assert_refused_before_any_side_effect};
use duduclaw_core::types::RuntimeType;

fn origin<'a>(text: &'a str, session: &'a str) -> CommandOrigin<'a> {
    CommandOrigin { channel: "telegram", gate_session: session, command_session: session,
        sender: "42", text, shared: false }
}

fn bind_admin(home: &Path) {
    let users = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let admin = users.create_user("command@test.invalid", "Admin", "Fixture-Password-12345",
        duduclaw_auth::UserRole::Admin).unwrap();
    users.bind_channel_identity(&admin.id, "telegram", "42", true).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn broken_command_targets_refuse_before_any_work_and_blocked_senders_get_silence() {
    for malformed in [false, true] {
        let (home, ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
        bind_admin(home.path());
        let path = home.path().join("agents/worker/agent.toml");
        let healthy = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, if malformed { "[model".to_string() }
            else { healthy.replace("provider='codex'", "provider='grok'") }).unwrap();
        let before = crate::model_call_probe::calls();
        let code = if malformed { "agent_config_unreadable" } else { "model_provider_mismatch" };
        for text in ["/goal 整理報表", "/交辦 @worker 整理報表", "/usage"] {
            let cmd = crate::chat_commands::parse_command(text, None).unwrap();
            let selected = if matches!(&cmd, ChatCommand::Assign(_)) { "main" } else { "worker" };
            let reply = handle_gated_command(&cmd, &ctx, &origin(text, "telegram:42"), selected).await;
            assert!(reply.contains(code), "{text}: {reply}");
            assert_refused_before_any_side_effect(&ctx, "telegram:42", &calls, before).await;
            let reply = handle_gated_command(&cmd, &ctx, &origin(text, "widget-visitor:test"), selected).await;
            assert_eq!(reply, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
        }
        ctx.channel_settings.set("telegram", "global", crate::channel_settings::keys::BLOCKED_USERS,
            r#"["42"]"#).await.unwrap();
        for text in ["/goal 整理報表", "/交辦 @worker 整理報表", "/usage", "/help"] {
            let cmd = crate::chat_commands::parse_command(text, None).unwrap();
            assert_eq!(handle_gated_command(&cmd, &ctx, &origin(text, "telegram:42"), "worker").await, "");
        }
        assert_refused_before_any_side_effect(&ctx, "telegram:42", &calls, before).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn exempt_commands_keep_their_handlers_and_healthy_commands_are_unchanged() {
    let (home, ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
    bind_admin(home.path());
    ctx.channel_settings.set("telegram", "global", "admin_users", r#"["42"]"#).await.unwrap();
    let path = home.path().join("agents/worker/agent.toml");
    let healthy = std::fs::read_to_string(&path).unwrap();
    for text in ["!STOP", "!STOP ALL", "!RESUME", "!STATUS", "/status", "/help", "/new", "/pair invalid", "/model"] {
        std::fs::write(&path, "[model").unwrap();
        let cmd = crate::chat_commands::parse_command(text, None).unwrap();
        assert!(crate::entry_gate::command_is_exempt(&cmd));
        assert!(command_entry_refusal(&cmd, &ctx, &origin(text, "telegram:42"), "worker").await.is_none());
        let reply = handle_gated_command(&cmd, &ctx, &origin(text, "telegram:42"), "worker").await;
        // Pairing still validates its code; exemption does not grant access.
        assert!(!reply.is_empty(), "{text}");
        assert!(!reply.contains("agent_config_unreadable"), "{text}: {reply}");
        assert!(!reply.contains("model_provider_mismatch"), "{text}: {reply}");
    }
    let takeover = crate::chat_commands::parse_takeover("/takeover").unwrap();
    assert!(crate::entry_gate::takeover_is_exempt(&takeover));
    std::fs::write(path, healthy).unwrap();
    for text in ["/usage", "/goal status"] {
        let cmd = crate::chat_commands::parse_command(text, None).unwrap();
        let expected = crate::chat_commands::handle_command_for_sender(
            &cmd, &ctx, "telegram", "telegram:42", "worker", "42",
        ).await;
        assert_eq!(handle_gated_command(&cmd, &ctx, &origin(text, "telegram:42"), "worker").await, expected);
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn unauthorized_assignment_preserves_the_identity_refusal() {
    let (home, ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap();
    let text = "/交辦 @worker 整理報表";
    let cmd = crate::chat_commands::parse_command(text, None).unwrap();
    let reply = handle_gated_command(&cmd, &ctx, &origin(text, "telegram:42"), "worker").await;
    assert!(!reply.is_empty());
    assert!(!reply.contains("agent_config_unreadable"));
    assert!(!reply.contains("model_provider_mismatch"));
}

#[test]
fn all_eleven_channels_reach_the_entry_gate() {
    for (name, source) in [
        ("whatsapp", include_str!("../whatsapp.rs")),
        ("feishu", include_str!("../feishu.rs")),
        ("teams", include_str!("../msteams.rs")),
        ("wecom", include_str!("../wecom.rs")),
        ("dingtalk", include_str!("../dingtalk.rs")),
        ("googlechat", include_str!("../googlechat.rs")),
        ("telegram", include_str!("../telegram.rs")),
        ("line", include_str!("../line.rs")),
        ("discord", include_str!("../discord.rs")),
        ("slack", include_str!("../slack.rs")),
    ] {
        assert!(source.contains("chat_commands::handle_gated_command(")
            || source.contains("chat_commands::command_entry_refusal("), "{name}");
    }
    assert!(include_str!("../webchat/turns/run.rs").contains("entry::check("));
    assert!(include_str!("../webchat/entry.rs").contains("entry_gate::resolve_and_check("));
    let shared = include_str!("conversation_guard.rs");
    let handler = shared.split("pub async fn handle_gated_command(").nth(1).unwrap();
    assert!(handler.find("command_preflight(").unwrap() < handler.find("command_entry_refusal(").unwrap());
    assert!(handler.find("command_entry_refusal(").unwrap() < handler.find("super::handle_command_for_sender(").unwrap());
}
