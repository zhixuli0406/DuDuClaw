use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    owned_channel, misaligned_worker, assert_no_saved_attachments,
    assert_refused_before_any_side_effect,
};
use duduclaw_core::types::RuntimeType;

const SESSIONS: &[&str] = &[
    "telegram:42:7", "line:42", "discord:thread:42", "slack:group:C1",
    "whatsapp:42", "feishu:42", "googlechat:spaces/A", "teams:42",
];

#[tokio::test(flavor = "current_thread")]
async fn broken_attachments_refuse_with_code_before_any_side_effect() {
    for malformed in [false, true] {
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        if malformed {
            std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap();
        }
        let code = if malformed { "agent_config_unreadable" } else { "model_provider_mismatch" };
        let before = crate::model_call_probe::calls();
        for session in SESSIONS {
            let result = check(&ctx, session, session, "42", "檢查附件", Some("worker"), None).await;
            let AttachmentEntry::Refuse(reply) = result else { panic!("{session}: {result:?}"); };
            assert!(reply.contains(code), "{session}: {reply}");
            assert_no_saved_attachments(home.path());
            assert_refused_before_any_side_effect(&ctx, session, &calls, before).await;
        }
        assert_eq!(check(&ctx, "widget-visitor:test", "widget-visitor:test", "42", "附件", Some("worker"), None).await,
            AttachmentEntry::Refuse(crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE.into()));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn only_parsed_exempt_commands_drop_files_and_keep_the_command() {
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let before = crate::model_call_probe::calls();
    for text in ["!STOP", "!STOP ALL", "!RESUME", "!STATUS", "/status", "/help", "/new",
        "/reset", "/pair code", "/model gpt-5.4", "/takeover", "/takeover end"] {
        assert_eq!(check(&ctx, "telegram:42", "telegram:42", "42", text, Some("worker"), None).await,
            AttachmentEntry::DropFilesAndContinue(text.into()), "{text}");
    }
    for text in ["/helpful", "!STOP later", "/usage", "/goal 整理附件"] {
        assert!(matches!(check(&ctx, "telegram:42", "telegram:42", "42", text, Some("worker"), None).await,
            AttachmentEntry::Refuse(_)), "{text}");
    }
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "telegram:42", &calls, before).await;
}

#[tokio::test(flavor = "current_thread")]
async fn strict_decisions_drop_files_before_resolving_the_answering_employee() {
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let before = crate::model_call_probe::calls();
    for malformed in [false, true] {
        if malformed {
            std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap();
        }
        for session in SESSIONS {
            for verb in ["deny", "approve", "answer"] {
                let text = format!("{verb} 12345678-1234-1234-1234-123456789abc");
                for selected in [Some("worker"), Some("missing"), None] {
                    assert_eq!(check(&ctx, session, session, "42", &text, selected, None).await,
                        AttachmentEntry::DropFilesAndContinue(text.clone()), "{session}: {text}");
                }
            }
            assert_refused_before_any_side_effect(&ctx, session, &calls, before).await;
        }
    }
    assert_no_saved_attachments(home.path());
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_or_unpaired_senders_keep_the_original_access_flow() {
    let (home, ctx, _runtime, _calls) = misaligned_worker("").await;
    for session in SESSIONS {
        let channel = session.split(':').next().unwrap();
        ctx.channel_settings.set(channel, "global", crate::channel_settings::keys::BLOCKED_USERS,
            r#"["42"]"#).await.unwrap();
        for text in ["附件", "/help", "deny 12345678-1234-1234-1234-123456789abc"] {
            assert_eq!(check(&ctx, session, session, "42", text, Some("worker"), None).await,
                AttachmentEntry::SkipFiles);
        }
    }
    ctx.channel_settings.set("telegram", "global", crate::channel_settings::keys::BLOCKED_USERS, "[]").await.unwrap();
    ctx.channel_settings.set("telegram", "global", crate::channel_settings::keys::REQUIRE_PAIRING, "true").await.unwrap();
    let code = ctx.access_control.generate_pairing_code("42").await.unwrap();
    let text = format!("/pair {code}");
    assert_eq!(check(&ctx, "telegram:42", "telegram:42", "42", &text, Some("worker"), None).await,
        AttachmentEntry::SkipFiles);
    // Preflight must not consume the code or approve its sender.
    assert!(ctx.access_control.runtime_approved_users().await.is_empty());
    let reply = crate::channel_reply::check_command_access_gate(&ctx, "telegram:42", "42", &text).await.unwrap();
    assert!(reply.starts_with('✅'), "{reply}");
    assert_no_saved_attachments(home.path());
}

#[tokio::test(flavor = "current_thread")]
async fn healthy_targets_and_bot_bindings_do_not_check_the_wrong_employee() {
    let (home, ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    let main_path = home.path().join("agents/main/agent.toml");
    let worker_path = home.path().join("agents/worker/agent.toml");
    let main = std::fs::read_to_string(&main_path).unwrap();
    let worker = std::fs::read_to_string(&worker_path).unwrap();
    for session in SESSIONS {
        for selected in [None, Some("worker")] {
            for text in ["附件", "/help"] {
                assert_eq!(check(&ctx, session, session, "42", text, selected, None).await, AttachmentEntry::Proceed);
            }
        }
    }
    for channel in ["telegram", "discord", "slack"] {
        let session = format!("{channel}:42");
        std::fs::write(&main_path, &main).unwrap();
        std::fs::write(&worker_path, worker.replace("provider='codex'", "provider='grok'")).unwrap();
        assert!(matches!(check(&ctx, &session, &session, "42", "附件", Some("worker"), None).await,
            AttachmentEntry::Refuse(text) if text.contains("model_provider_mismatch")));
        assert_eq!(check(&ctx, &session, &session, "42", "附件", None, None).await, AttachmentEntry::Proceed);
        std::fs::write(&main_path, "[model").unwrap();
        std::fs::write(&worker_path, &worker).unwrap();
        assert_eq!(check(&ctx, &session, &session, "42", "附件", Some("worker"), None).await, AttachmentEntry::Proceed);
        assert!(matches!(check(&ctx, &session, &session, "42", "附件", None, None).await,
            AttachmentEntry::Refuse(text) if text.contains("agent_config_unreadable")));
    }
    assert_no_saved_attachments(home.path());
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_policy_and_thread_binding_match_the_reply_pipeline() {
    let (home, ctx, _runtime, _calls) = misaligned_worker(
        "\n[[channels.discord.bindings]]\nkind='thread'\nid='42'\n",
    ).await;
    assert_eq!(check(&ctx, "discord:channel", "discord:channel", "42", "附件", None, None).await,
        AttachmentEntry::Proceed);
    assert!(matches!(check(&ctx, "discord:thread:42", "discord:thread:42", "42", "附件", None, None).await,
        AttachmentEntry::Refuse(_)));
    ctx.channel_settings.set("discord", "global", crate::channel_settings::keys::BLOCKED_USERS,
        r#"["42"]"#).await.unwrap();
    let scoped = crate::channel_reply::UserAccessPolicy { allowed: None, blocked: vec![], require_pairing: false };
    assert!(matches!(check(&ctx, "discord:thread:42", "discord:thread:42", "42", "附件", None, Some(&scoped)).await,
        AttachmentEntry::Refuse(_)));
    let blocked = crate::channel_reply::UserAccessPolicy { blocked: vec!["42".into()], ..scoped };
    assert_eq!(check(&ctx, "discord:thread:42", "discord:thread:42", "42", "附件", None, Some(&blocked)).await,
        AttachmentEntry::SkipFiles);
    assert_no_saved_attachments(home.path());
}

#[test]
fn every_downloading_channel_reaches_attachment_gate_before_download() {
    for (name, source, download) in [
        ("telegram", include_str!("../telegram.rs"), "match transcribe_voice("),
        ("line", include_str!("../line.rs"), "crate::media::download_url("),
        ("discord", include_str!("../discord.rs"), "crate::media::download_url("),
        ("slack", include_str!("../slack.rs"), "crate::media::download_url("),
        ("whatsapp", include_str!("../whatsapp.rs"), "match download_media("),
        ("feishu", include_str!("../feishu.rs"), "match download_feishu_resource("),
        ("googlechat", include_str!("../googlechat.rs"), "match download_media("),
        ("teams", include_str!("../msteams.rs"), "crate::media::download_url("),
    ] {
        assert!(source.find("channel_reply::attachment_gate::check(").unwrap()
            < source.find(download).unwrap(), "{name}");
    }
    // Durable LINE downloads only in the shared adapter, after its stored route.
    assert!(include_str!("../line/ingress/worker.rs").contains("Box::pin(process_line_events("));
    for source in [include_str!("../wecom.rs"), include_str!("../dingtalk.rs")] {
        assert!(!source.contains("download_url(") && !source.contains("save_attachment"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn no_responder_retains_the_existing_flow() {
    let (home, ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    *ctx.registry.write().await = duduclaw_agent::AgentRegistry::new(home.path().join("empty-agents"));
    assert_eq!(check(&ctx, "telegram:42", "telegram:42", "42", "附件", None, None).await,
        AttachmentEntry::Proceed);
    assert_no_saved_attachments(home.path());
}

#[tokio::test(flavor = "current_thread")]
async fn employee_mention_with_a_file_checks_the_called_employee() {
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = users.create_user("attachment@test.invalid", "Admin", "Fixture-Password-12345",
        duduclaw_auth::UserRole::Admin).unwrap();
    users.bind_channel_identity(&admin.id, "discord", "42", true).unwrap();
    let before = crate::model_call_probe::calls();
    assert!(matches!(check(&ctx, "discord:C1", "discord:C1", "42", "@worker", None, None).await,
        AttachmentEntry::Refuse(text) if text.contains("model_provider_mismatch")));
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "discord:C1", &calls, before).await;
}
