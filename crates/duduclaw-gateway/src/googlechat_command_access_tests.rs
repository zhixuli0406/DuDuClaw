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

fn threaded(space: &str, thread: &str, dm: bool) -> crate::googlechat_conversation::GoogleChatConversation {
    crate::googlechat_conversation::GoogleChatConversation::from_event(
        &serde_json::json!({"space":{"name":space},"message":{
            "thread":{"name":format!("{space}/threads/{thread}")},"threadReply":true}}), dm, false,
    ).unwrap()
}

#[tokio::test]
async fn rm182_group_new_requires_manager_and_clears_only_current_thread_agent() {
    use duduclaw_auth::{UserDb, UserRole};
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@rm182.test", "Admin", "AdminPass-12345", UserRole::Admin).unwrap();
    let manager = db.create_user("manager@rm182.test", "Manager", "ManagerPass-12345", UserRole::Manager).unwrap();
    db.bind_channel_identity(&admin.id, "googlechat", "users/1", true).unwrap();
    db.bind_channel_identity(&manager.id, "googlechat", "users/2", true).unwrap();
    let a = threaded("spaces/A", "one", false);
    let b = threaded("spaces/A", "two", false);
    let current = a.session_for(Some("sales"));
    let untouched = [b.session_for(Some("sales")), a.session_for(Some("hr")), "googlechat:spaces/A".into()];
    for sid in std::iter::once(&current).chain(untouched.iter()) {
        ctx.session_manager.get_or_create(sid, "sales").await.unwrap();
        ctx.session_manager.append_message(sid, "user", "KEEP-ME", 1).await.unwrap();
        ctx.session_manager.set_pinned(sid, "PIN-MARKER").await.unwrap();
        ctx.session_manager.record_tool_reference(sid, "sales", "gmail_create_draft",
            &serde_json::json!({"_meta":{"duduclaw_created_ref":{"id":"draft-182","label":"draft"}}})).await.unwrap();
    }
    let cmd = crate::chat_commands::parse_command("/new", None).unwrap();
    for sender in ["users/1", "users/2", "users/3"] {
        let origin = CommandOrigin { channel: "googlechat", gate_session: a.gate_session(),
            command_session: &current, sender, text: "/new", shared: !a.is_dm() };
        let refusal = crate::chat_commands::command_preflight(&cmd, &ctx, &origin).await;
        if sender == "users/3" {
            assert_eq!(refusal.as_deref(), Some(crate::chat_commands::GROUP_RESET_REFUSED));
        } else { assert_eq!(refusal, None); }
    }
    crate::chat_commands::handle_command_for_sender(&cmd, &ctx, "googlechat", &current, "sales", "users/2").await;
    ctx.session_manager.get_or_create(&current, "sales").await.unwrap();
    assert!(ctx.session_manager.history_snapshot(&current).await.unwrap().messages.is_empty());
    assert!(ctx.session_manager.get_pinned(&current).await.unwrap().is_empty());
    assert!(ctx.session_manager.tool_refs_context(&current).await.unwrap().is_empty());
    for sid in &untouched {
        assert_eq!(ctx.session_manager.history_snapshot(sid).await.unwrap().messages.len(), 1);
        assert_eq!(ctx.session_manager.get_pinned(sid).await.unwrap(), "PIN-MARKER");
        assert!(ctx.session_manager.tool_refs_context(sid).await.unwrap().contains("draft-182"));
    }
    let dm = threaded("spaces/DM", "one", true);
    let dm_key = dm.session_for(Some("sales"));
    let dm_origin = CommandOrigin { channel: "googlechat", gate_session: dm.gate_session(),
        command_session: &dm_key, sender: "users/3", text: "/new", shared: !dm.is_dm() };
    assert_eq!(crate::chat_commands::command_preflight(&cmd, &ctx, &dm_origin).await, None);
}

#[tokio::test]
async fn rm182_thread_commands_keep_space_permission_and_pairing_scope() {
    let home = tempfile::tempdir().unwrap();
    let ctx = reply_ctx(home.path());
    ctx.channel_settings.set("googlechat", "global", keys::BLOCKED_USERS, r#"["googlechat:spaces/A"]"#).await.unwrap();
    let c = threaded("spaces/A", "one", false);
    let key = c.session_for(Some("sales"));
    let cmd = crate::chat_commands::parse_command("/status", None).unwrap();
    let origin = CommandOrigin { channel: "googlechat", gate_session: c.gate_session(),
        command_session: &key, sender: "users/1", text: "/status", shared: true };
    assert_eq!(crate::chat_commands::command_preflight(&cmd, &ctx, &origin).await.as_deref(), Some(""));
}

#[tokio::test]
async fn rm182_takeover_is_space_wide_but_records_each_thread_separately() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[takeover]\nenabled = true\n").unwrap();
    let ctx = reply_ctx(home.path());
    let a = threaded("spaces/A", "one", false);
    let b = threaded("spaces/A", "two", false);
    crate::takeover::begin_takeover(home.path(), a.gate_session(), "sales", "users/1", "Manager",
        &duduclaw_core::takeover_state::TakeoverConfig::default()).await.unwrap();
    for c in [&a, &b] {
        assert_eq!(crate::takeover::intercept(&ctx, c.session_key(), "users/2", "HUMAN-TURN").await,
            Some(crate::takeover::Intercepted::Silent));
        assert_eq!(ctx.session_manager.history_snapshot(c.session_key()).await.unwrap().messages.len(), 1);
    }
    assert!(ctx.session_manager.history_snapshot(a.gate_session()).await.unwrap().messages.is_empty());
}
