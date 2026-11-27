//! Who may run a chat command in a conversation.
//!
//! Every channel's command intercept runs before the reply pipeline, so it
//! applies three checks of its own before a command takes effect:
//!
//! 1. the same per-user access gate the reply pipeline runs
//!    (`channel_reply::check_command_access_gate`, on the same conversation id
//!    the pipeline gates) — a blocked or unpaired sender gets exactly what a
//!    normal message would get (silence, or the pairing hint / `/pair`
//!    verdict);
//! 2. in a conversation several people share one history (a group, a space,
//!    a channel), clearing or compressing that history (`/new`, `/reset`,
//!    `/compact`, a "new conversation" button) is limited to a sender whose
//!    verified channel identity maps to an Active dashboard Admin or Manager
//!    (`decision_notify::mapped_role` — the same role mapping decision
//!    buttons and `/rules off` use). Fail-closed: no sender id, no verified
//!    binding, an inactive account or an unreadable identity store all
//!    refuse. A 1:1 conversation keeps resetting its own history as before.
//!
//!    This rule only applies when the installation HAS an approver identity
//!    system ([`crate::decision_notify::has_recognisable_approver_identity`]). A single-user / personal
//!    install with no `users.db`, or with no Active Admin/Manager holding any
//!    verified channel identity or recognisable derived Google identity, has
//!    nobody who could ever be a verified
//!    Admin/Manager; there, any sender who passes the access gate (check 1)
//!    may run these commands, exactly as before the rule existed. Check 1
//!    itself is unconditional.
//!
//! 3. non-exempt commands check their employee's effective settings through
//!    the shared entry gate, after access and shared-reset authorization.

use std::path::Path;

use super::ChatCommand;
use crate::channel_reply::ReplyContext;

/// Said to a member who tried to clear a shared conversation.
pub const GROUP_RESET_REFUSED: &str = "只有管理員可以清除這個聊天室的對話。";

/// Commands that erase or compress the conversation's whole history.
pub fn clears_conversation(cmd: &ChatCommand) -> bool {
    matches!(cmd, ChatCommand::New | ChatCommand::Compact)
}

/// `true` when `sender` may NOT clear a conversation: it is `shared`, the
/// installation has an approver identity system, and the sender is not a
/// verified Admin/Manager. `shared` must be `true` whenever the caller cannot
/// tell (fail-closed). Without an identity system nobody could ever qualify,
/// so the pre-rule behaviour is kept (allowed; the caller's access gate still
/// applies).
pub fn shared_clear_refused(home: &Path, channel: &str, sender: &str, shared: bool) -> bool {
    if !shared {
        return false;
    }
    if !crate::decision_notify::has_recognisable_approver_identity(home) {
        return false;
    }
    if sender.trim().is_empty() {
        return true;
    }
    !matches!(
        crate::decision_notify::mapped_role(home, channel, sender),
        Some(duduclaw_auth::UserRole::Admin | duduclaw_auth::UserRole::Manager)
    )
}

/// The refusal for `cmd` in this conversation, if it is refused.
pub fn shared_reset_refusal(
    home: &Path,
    channel: &str,
    sender: &str,
    shared: bool,
    cmd: &ChatCommand,
) -> Option<&'static str> {
    (clears_conversation(cmd) && shared_clear_refused(home, channel, sender, shared))
        .then_some(GROUP_RESET_REFUSED)
}

/// Where a chat command came from.
#[derive(Debug, Clone, Copy)]
pub struct CommandOrigin<'a> {
    /// Channel name as identities are bound (`googlechat`, `teams`, …).
    pub channel: &'a str,
    /// The conversation id the reply pipeline's access gate runs on.
    pub gate_session: &'a str,
    /// The session the command acts on (an employee's own bucket of the
    /// conversation may differ from `gate_session`).
    pub command_session: &'a str,
    /// The sender's own platform account id (empty = unknown).
    pub sender: &'a str,
    /// The raw message text (the gate recognises `/pair <code>` in it).
    pub text: &'a str,
    /// Several people share this conversation's history (`true` when the
    /// conversation type is unknown).
    pub shared: bool,
}

/// The checks above. `Some(reply)` = send `reply` instead of running the
/// command (empty = send nothing); `None` = the command may run.
pub async fn command_preflight(
    cmd: &ChatCommand,
    ctx: &ReplyContext,
    origin: &CommandOrigin<'_>,
) -> Option<String> {
    // Fail-closed snapshot gate on the real sender (an unreadable policy or a
    // missing sender under a policy denies like a block).
    if let Some(reply) = crate::channel_reply::check_command_access_gate(
        ctx,
        origin.gate_session,
        origin.sender,
        origin.text,
    )
    .await
    {
        return Some(reply);
    }
    shared_reset_refusal(&ctx.home_dir, origin.channel, origin.sender, origin.shared, cmd)
        .map(str::to_string)
}

/// Check the employee only after the adapter has authorized the sender.
/// Assignment planning authorizes and resolves the destination before its
/// settings are inspected; an authorization/roster reply keeps its wording.
pub async fn command_entry_refusal(
    cmd: &ChatCommand,
    ctx: &ReplyContext,
    origin: &CommandOrigin<'_>,
    agent_id: &str,
) -> Option<String> {
    if crate::entry_gate::command_is_exempt(cmd) {
        return None;
    }
    let mut target = agent_id.to_string();
    if let ChatCommand::Assign(assign) = cmd {
        use crate::trigger_route::assign::{AssignCaller, Plan};
        let caller = AssignCaller {
            home: &ctx.home_dir, channel: origin.channel,
            conversation: origin.gate_session, sender: origin.sender,
            event: None, seq: 0, fallback: (!agent_id.is_empty()).then_some(agent_id),
            access: crate::trigger_route::access::for_channel_sender(
                &ctx.home_dir, origin.channel, origin.sender,
            ),
        };
        let registry = ctx.registry.read().await;
        match crate::trigger_route::assign::plan(
            &*registry, crate::trigger_route::texts::lang_for(&ctx.home_dir), assign, &caller,
        ) {
            Plan::Arm { agent, .. } => target = agent,
            Plan::Create(spec) | Plan::ScheduleCreate { spec, .. } => target = spec.agent,
            Plan::Reply(reply) if reply.list.is_none() => return Some(reply.text),
            _ => {}
        }
    }
    if matches!(cmd, ChatCommand::Goal(_)) {
        let access = crate::trigger_route::access::for_channel_sender(
            &ctx.home_dir, origin.channel, origin.sender,
        );
        let principal = crate::trigger_route::assign::principal_of(&access);
        let goal_principal = principal.clone().map(|p| (p, origin.channel.to_string()));
        if let Err(refusal) = super::with_goal_principal(goal_principal,
            crate::machine_employee::check_chat_goal(ctx, &target, origin.command_session),
        ).await {
            return Some(refusal);
        }
        if let Err(refusal) = crate::on_behalf_of::access::check_main_target(
            &ctx.home_dir, principal.as_deref(), &target,
        ).await {
            return Some(refusal);
        }
    }
    let result = {
        let registry = ctx.registry.read().await;
        crate::entry_gate::resolve_and_check(&ctx.home_dir, &*registry, &target)
    };
    if let Err(refusal) = result {
        let language = crate::prompt_identity::resolve_fixed_message_language_for_id(
            &ctx.home_dir, &target,
        ).await;
        return Some(refusal.user_text(origin.gate_session, language.as_deref(), "command"));
    }
    None
}

/// [`super::handle_command_for_sender`] behind [`command_preflight`]. An
/// empty result means "send nothing" (a blocked sender).
pub async fn handle_gated_command(
    cmd: &ChatCommand,
    ctx: &ReplyContext,
    origin: &CommandOrigin<'_>,
    agent_id: &str,
) -> String {
    if let Some(reply) = command_preflight(cmd, ctx, origin).await {
        return reply;
    }
    if let Some(reply) = command_entry_refusal(cmd, ctx, origin, agent_id).await {
        return reply;
    }
    super::handle_command_for_sender(
        cmd,
        ctx,
        origin.channel,
        origin.command_session,
        agent_id,
        origin.sender,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_settings::keys;

    fn test_ctx(home: &Path) -> ReplyContext {
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
            duduclaw_agent::AgentRegistry::new(home.join("agents")),
        ));
        let sessions = std::sync::Arc::new(
            crate::session::SessionManager::new(&home.join("sessions.db")).unwrap(),
        );
        let status: crate::channel_reply::ChannelStatusMap =
            std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
    }

    // Google Chat identities use users/<digits>, including in fixtures.
    fn bind(home: &Path, channel_user: &str, role: duduclaw_auth::UserRole) {
        let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let email = format!("{}@test.local", channel_user.replace('/', "-"));
        let user = db.create_user(&email, channel_user, "StaffPass-12345", role).unwrap();
        db.bind_channel_identity(&user.id, "googlechat", channel_user, true).unwrap();
    }

    fn origin<'a>(sender: &'a str, text: &'a str, shared: bool) -> CommandOrigin<'a> {
        CommandOrigin {
            channel: "googlechat",
            gate_session: "googlechat:spaces/A",
            command_session: "googlechat:spaces/A",
            sender,
            text,
            shared,
        }
    }

    async fn run(ctx: &ReplyContext, text: &str, sender: &str, shared: bool) -> String {
        let cmd = super::super::parse_command(text, None).unwrap();
        handle_gated_command(&cmd, ctx, &origin(sender, text, shared), "agent-a").await
    }

    #[tokio::test]
    async fn blocked_member_cannot_new_or_compact() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        bind(dir.path(), "users/105", duduclaw_auth::UserRole::Admin);
        ctx.channel_settings
            .set("googlechat", "global", keys::BLOCKED_USERS, r#"["users/105"]"#)
            .await
            .unwrap();
        for text in ["/new", "/reset", "/compact"] {
            // Even a mapped Admin who is blocked on the channel gets silence.
            assert_eq!(run(&ctx, text, "users/105", true).await, "", "{text}");
            assert_eq!(run(&ctx, text, "users/105", false).await, "", "{text}");
        }
    }

    #[tokio::test]
    async fn non_admin_in_space_is_refused_admin_and_manager_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        bind(dir.path(), "users/101", duduclaw_auth::UserRole::Employee);
        bind(dir.path(), "users/102", duduclaw_auth::UserRole::Admin);
        bind(dir.path(), "users/103", duduclaw_auth::UserRole::Manager);
        for text in ["/new", "/reset", "/compact"] {
            assert_eq!(run(&ctx, text, "users/101", true).await, GROUP_RESET_REFUSED);
            assert_eq!(run(&ctx, text, "users/104", true).await, GROUP_RESET_REFUSED);
            assert_eq!(run(&ctx, text, "", true).await, GROUP_RESET_REFUSED);
        }
        for who in ["users/102", "users/103"] {
            let reply = run(&ctx, "/new", who, true).await;
            assert!(reply.starts_with('✅'), "{who}: {reply}");
        }
        // Other commands are not affected by the rule.
        assert_ne!(run(&ctx, "/help", "users/101", true).await, GROUP_RESET_REFUSED);
    }

    #[tokio::test]
    async fn one_to_one_member_may_reset_own_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let reply = run(&ctx, "/new", "users/101", false).await;
        assert!(reply.starts_with('✅'), "{reply}");
    }

    #[tokio::test]
    async fn no_identity_system_member_in_group_may_clear() {
        // Personal edition: no users.db at all.
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        for text in ["/new", "/reset", "/compact"] {
            let reply = run(&ctx, text, "users/101", true).await;
            assert_ne!(reply, GROUP_RESET_REFUSED, "{text}");
            assert!(!reply.is_empty(), "{text}");
            assert!(!shared_clear_refused(dir.path(), "googlechat", "users/101", true));
        }
        assert!(run(&ctx, "/new", "users/101", true).await.starts_with('✅'));
        // The check must not conjure an identity store as a side effect.
        assert!(!dir.path().join("users.db").exists());
    }

    #[tokio::test]
    async fn users_db_without_verified_approver_is_no_identity_system() {
        // users.db exists, but no Active Admin/Manager has a verified channel
        // identity (an employee binding and an unverified admin claim only):
        // nobody can ever be a verified approver, so the old behaviour holds.
        let dir = tempfile::tempdir().unwrap();
        bind(dir.path(), "users/101", duduclaw_auth::UserRole::Employee);
        let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
        let admin = db
            .create_user("admin@test.local", "admin", "StaffPass-12345", duduclaw_auth::UserRole::Admin)
            .unwrap();
        db.bind_channel_identity(&admin.id, "googlechat", "users/102", false).unwrap();
        let ctx = test_ctx(dir.path());
        assert!(run(&ctx, "/new", "users/101", true).await.starts_with('✅'));
        assert!(run(&ctx, "/new", "users/104", true).await.starts_with('✅'));
        // Once one approver verifies a binding, the rule applies to everyone.
        db.bind_channel_identity(&admin.id, "googlechat", "users/102", true).unwrap();
        assert_eq!(run(&ctx, "/new", "users/101", true).await, GROUP_RESET_REFUSED);
        assert!(run(&ctx, "/new", "users/102", true).await.starts_with('✅'));
    }

    #[tokio::test]
    async fn blocked_sender_refused_with_and_without_identity_system() {
        async fn block_and_check(home: &Path) {
            let ctx = test_ctx(home);
            ctx.channel_settings
                .set("googlechat", "global", keys::BLOCKED_USERS, r#"["users/105"]"#)
                .await
                .unwrap();
            for text in ["/new", "/reset", "/compact"] {
                assert_eq!(run(&ctx, text, "users/105", true).await, "", "{text}");
                assert_eq!(run(&ctx, text, "users/105", false).await, "", "{text}");
            }
        }
        // 1. No users.db.
        let none = tempfile::tempdir().unwrap();
        block_and_check(none.path()).await;
        // 2. users.db with a verified Admin — the blocked sender IS that admin.
        let with_admin = tempfile::tempdir().unwrap();
        bind(with_admin.path(), "users/105", duduclaw_auth::UserRole::Admin);
        block_and_check(with_admin.path()).await;
        // 3. Unreadable users.db.
        let broken = tempfile::tempdir().unwrap();
        std::fs::write(broken.path().join("users.db"), b"not a sqlite database").unwrap();
        block_and_check(broken.path()).await;
    }

    #[tokio::test]
    async fn identity_lookup_error_refuses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("users.db"), b"not a sqlite database").unwrap();
        let ctx = test_ctx(dir.path());
        assert_eq!(run(&ctx, "/new", "users/102", true).await, GROUP_RESET_REFUSED);
        assert!(shared_clear_refused(dir.path(), "googlechat", "users/102", true));
    }

    #[tokio::test]
    async fn member_scoped_code_pairs_only_that_member() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        ctx.channel_settings
            .set("googlechat", "global", keys::REQUIRE_PAIRING, "true")
            .await
            .unwrap();
        let code = ctx.access_control.generate_pairing_code("users/alice").await.unwrap();
        let text = format!("/pair {code}");
        let bob = run(&ctx, &text, "users/bob", true).await;
        assert!(bob.starts_with('❌'), "{bob}");
        let alice = run(&ctx, &text, "users/alice", true).await;
        assert!(alice.starts_with('✅'), "{alice}");
        // Alice is through; Bob still needs pairing; the space is not paired.
        assert_eq!(run(&ctx, "/help", "users/bob", true).await,
            "🔒 尚未配對。請向管理員索取配對碼，並輸入：/pair <配對碼>");
        assert!(!run(&ctx, "/help", "users/alice", true).await.starts_with('🔒'));
        assert_eq!(ctx.access_control.runtime_approved_users().await, vec!["users/alice"]);
    }

    #[tokio::test]
    async fn open_channel_pair_uses_sender_then_normalised_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        // No access settings: the gate is open and `/pair` reaches the
        // command handler, which an employee's own app runs in its bucket.
        let code = ctx.access_control.generate_pairing_code("users/alice").await.unwrap();
        let text = format!("/pair {code}");
        let cmd = super::super::parse_command(&text, None).unwrap();
        let bucket = CommandOrigin {
            command_session: "googlechat:spaces/A#agent:sales",
            ..origin("users/bob", &text, true)
        };
        assert!(handle_gated_command(&cmd, &ctx, &bucket, "sales").await.starts_with('❌'));
        let alice = CommandOrigin { sender: "users/alice", ..bucket };
        assert!(handle_gated_command(&cmd, &ctx, &alice, "sales").await.starts_with('✅'));

        let space_code = ctx.access_control.generate_pairing_code("googlechat:spaces/A").await.unwrap();
        let text = format!("/pair {space_code}");
        let cmd = super::super::parse_command(&text, None).unwrap();
        let carol = CommandOrigin { sender: "users/carol", text: &text, ..bucket };
        assert!(handle_gated_command(&cmd, &ctx, &carol, "sales").await.starts_with('✅'));
        // Stored without the bucket suffix, so the space-level check honours it.
        let approved = ctx.access_control.runtime_approved_users().await;
        assert!(approved.iter().any(|s| s == "googlechat:spaces/A"), "{approved:?}");
        assert!(!approved.iter().any(|s| s.ends_with("#agent:sales")), "{approved:?}");
        assert_eq!(
            ctx.access_control
                .check_access_dual("users/dave", "googlechat:spaces/A", None, &[], true)
                .await,
            crate::access_control::AccessDecision::Allowed
        );
    }
}

#[cfg(test)]
#[path = "entry_gate_tests.rs"]
mod entry_gate_tests;
