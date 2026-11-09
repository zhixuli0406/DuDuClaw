//! RM-172: the shared Telegram bot routes a staff member whose Telegram
//! identity is verified-bound to their dashboard account to that account's
//! 主要 AI 員工 (1:1 chats only), after the bind link and before the channel
//! default, without overriding an `@` call or a one-shot pick.

use super::*;
use crate::trigger_route::tests::write_agent;
use crate::trigger_route::turn::TurnDecision;
use duduclaw_auth::{AccessLevel, UserDb, UserRole, UserStatus};

/// The Telegram user id of the staff member (their DM chat id is the same).
const TG_USER: &str = "42";
const TG_DM: i64 = 42;

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: Arc<ReplyContext>,
    db: UserDb,
    user_id: String,
}

/// Employees: `dudu` (main, the gateway default), `finance` (the staff
/// member's personal employee, set as their primary), `sales-rep`, and
/// `retired` (archived). The staff member's Telegram identity is bound with
/// `verified`.
async fn fixture(verified: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    write_agent(&agents, "dudu", "嘟嘟總機", "@嘟嘟總機", "main", "");
    write_agent(&agents, "finance", "財務小幫手", "@財務小幫手", "specialist", "");
    write_agent(&agents, "sales-rep", "業務專員", "@sales-rep", "specialist", "");
    write_agent(&agents, "retired", "退休員工", "@退休員工", "specialist", "");
    let retired = agents.join("retired").join("agent.toml");
    let toml = std::fs::read_to_string(&retired).unwrap();
    std::fs::write(&retired, toml.replace("status = \"active\"", "status = \"archived\"")).unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[general]\ndefault_agent = \"dudu\"\n",
    )
    .unwrap();
    let mut registry = duduclaw_agent::AgentRegistry::new(agents);
    registry.scan().await.unwrap();
    assert!(
        registry
            .get("retired")
            .is_some_and(|a| !a.config.agent.status.is_operational()),
        "the archived employee keeps its registry entry"
    );
    let sessions =
        Arc::new(crate::session::SessionManager::new(&dir.path().join("sessions.db")).unwrap());
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let status = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let ctx = Arc::new(ReplyContext::new(
        Arc::new(tokio::sync::RwLock::new(registry)),
        dir.path().to_path_buf(),
        sessions,
        status,
        event_tx,
    ));
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    let user = db
        .create_user("staff@rm172.test", "Staff", "StaffPass-12345", UserRole::Employee)
        .unwrap();
    db.bind_agent(&user.id, "finance", AccessLevel::Owner).unwrap();
    db.set_primary_agent(&user.id, Some("finance")).unwrap();
    db.bind_channel_identity(&user.id, "telegram", TG_USER, verified)
        .unwrap();
    Fixture {
        _dir: dir,
        ctx,
        db,
        user_id: user.id,
    }
}

fn routed_to(route: SharedRoute) -> Option<String> {
    match route {
        SharedRoute::Bound(agent) | SharedRoute::Primary(agent) => Some(agent),
        SharedRoute::Unbound => None,
        SharedRoute::Guide(_) => Some("<guide>".into()),
    }
}

async fn route(f: &Fixture, private_chat: bool) -> Option<String> {
    routed_to(resolve_shared_route(&f.ctx, TG_USER, private_chat).await)
}

/// The DM's session, with a unique topic so the process-wide one-shot store
/// is never shared with another test.
fn dm_session() -> String {
    format!("telegram:{TG_DM}:{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn rm172_verified_user_with_primary_is_routed_to_it() {
    let f = fixture(true).await;
    assert_eq!(route(&f, true).await.as_deref(), Some("finance"));
    // The 1:1 test the call sites use: the DM chat id is the sender's id.
    assert!(is_private_chat_with(TG_DM, TG_USER));
    assert!(!is_private_chat_with(-100_123, TG_USER));
    assert!(!is_private_chat_with(TG_DM, ""));
    assert!(!is_private_chat_with(TG_DM, "43"));
}

#[tokio::test]
async fn rm172_unverified_identity_gets_the_default() {
    let f = fixture(false).await;
    assert_eq!(route(&f, true).await, None, "a claimed identity is not proof");
}

#[tokio::test]
async fn rm172_no_primary_gets_the_default() {
    let f = fixture(true).await;
    f.db.set_primary_agent(&f.user_id, None).unwrap();
    assert_eq!(route(&f, true).await, None);
}

#[tokio::test]
async fn rm172_archived_primary_falls_back_to_the_default() {
    let f = fixture(true).await;
    f.db.bind_agent(&f.user_id, "retired", AccessLevel::Owner).unwrap();
    f.db.set_primary_agent(&f.user_id, Some("retired")).unwrap();
    assert_eq!(route(&f, true).await, None);
}

#[tokio::test]
async fn rm172_account_no_longer_allowed_falls_back_to_the_default() {
    let f = fixture(true).await;
    assert_eq!(route(&f, true).await.as_deref(), Some("finance"));

    // Downgraded to Viewer: WebChat would no longer hand them this employee.
    f.db.bind_agent(&f.user_id, "finance", AccessLevel::Viewer).unwrap();
    assert_eq!(route(&f, true).await, None, "viewer binding");

    // Binding removed entirely.
    f.db.bind_agent(&f.user_id, "finance", AccessLevel::Owner).unwrap();
    assert_eq!(route(&f, true).await.as_deref(), Some("finance"));
    f.db.unbind_agent(&f.user_id, "finance").unwrap();
    assert_eq!(route(&f, true).await, None, "binding removed");

    // Account suspended.
    f.db.bind_agent(&f.user_id, "finance", AccessLevel::Owner).unwrap();
    f.db.set_primary_agent(&f.user_id, Some("finance")).unwrap();
    assert_eq!(route(&f, true).await.as_deref(), Some("finance"));
    f.db.set_user_status(&f.user_id, UserStatus::Suspended).unwrap();
    assert_eq!(route(&f, true).await, None, "suspended account");
}

#[tokio::test]
async fn rm172_primary_follows_the_webchat_target_rules() {
    // A non-admin's primary on the main-role (gateway default) employee is
    // refused by WebChat, so the shared bot does not route there either; the
    // message takes the ordinary default path instead of a refusal.
    let f = fixture(true).await;
    f.db.bind_agent(&f.user_id, "dudu", AccessLevel::Operator).unwrap();
    f.db.set_primary_agent(&f.user_id, Some("dudu")).unwrap();
    assert_eq!(route(&f, true).await, None);
}

#[tokio::test]
async fn rm172_bind_link_beats_the_primary() {
    let f = fixture(true).await;
    let token = f
        .ctx
        .agent_binding
        .generate_bind_token("telegram", "sales-rep", 10, 1)
        .await;
    f.ctx
        .agent_binding
        .redeem_bind_token("telegram", &token, TG_USER)
        .await
        .unwrap();
    assert_eq!(route(&f, true).await.as_deref(), Some("sales-rep"));
}

#[tokio::test]
async fn rm172_mention_and_one_shot_beat_the_primary() {
    let f = fixture(true).await;
    // The staff member may call sales-rep (any binding level).
    f.db.bind_agent(&f.user_id, "sales-rep", AccessLevel::Viewer).unwrap();
    let primary = route(&f, true).await;
    assert_eq!(primary.as_deref(), Some("finance"));
    let session = dm_session();

    // A message that calls nobody stays with the primary (the default).
    let plain = telegram_trigger_decision(
        &f.ctx, &session, TG_USER, "幫我算帳", primary.as_deref(), "rm172-e1", false,
    )
    .await;
    assert_eq!(plain, TurnDecision::Default);

    // `@` another employee: that employee answers this message, labelled.
    let called = telegram_trigger_decision(
        &f.ctx, &session, TG_USER, "@sales-rep 報價", primary.as_deref(), "rm172-e2", false,
    )
    .await;
    assert_eq!(
        called,
        TurnDecision::Agent {
            id: "sales-rep".into(),
            via_trigger: true
        }
    );

    // A one-shot pick from the `@` list wins over the primary for the next
    // message, then routing returns to the primary.
    let ask = telegram_pick(&f.ctx, &session, TG_USER, "sales-rep", true).await;
    assert!(ask.contains("業務專員"), "{ask}");
    let next = telegram_trigger_decision(
        &f.ctx, &session, TG_USER, "這個月的報價", primary.as_deref(), "rm172-e3", true,
    )
    .await;
    assert_eq!(
        next,
        TurnDecision::Agent {
            id: "sales-rep".into(),
            via_trigger: true
        }
    );
    let after = telegram_trigger_decision(
        &f.ctx, &session, TG_USER, "謝謝", primary.as_deref(), "rm172-e4", true,
    )
    .await;
    assert_eq!(after, TurnDecision::Default);
}

#[tokio::test]
async fn rm172_groups_keep_the_channel_default() {
    let f = fixture(true).await;
    assert_eq!(route(&f, false).await, None, "group: default employee");
    // A bind link still routes per sender in groups, exactly as before.
    let token = f
        .ctx
        .agent_binding
        .generate_bind_token("telegram", "sales-rep", 10, 1)
        .await;
    f.ctx
        .agent_binding
        .redeem_bind_token("telegram", &token, TG_USER)
        .await
        .unwrap();
    assert_eq!(route(&f, false).await.as_deref(), Some("sales-rep"));
}

#[tokio::test]
async fn rm172_shared_bot_binding_mode_routes_a_verified_primary() {
    let f = fixture(true).await;
    f.ctx
        .channel_settings
        .set("telegram", "global", keys::SHARED_BOT_BINDING, "true")
        .await
        .unwrap();
    // Identified staff member: routed, not told to find a bind link.
    assert_eq!(route(&f, true).await.as_deref(), Some("finance"));
    // In a group the mode's guidance is unchanged.
    assert_eq!(route(&f, false).await.as_deref(), Some("<guide>"));
    // Without a usable primary the guidance is unchanged too.
    f.db.set_primary_agent(&f.user_id, None).unwrap();
    assert_eq!(route(&f, true).await.as_deref(), Some("<guide>"));
}

/// The DM conversation the reply-path tests below use (each fixture has its
/// own sessions database, so the plain id is not shared between tests).
fn dm_conversation() -> String {
    format!("telegram:{TG_DM}")
}

/// Run one shared-bot turn for the staff member in their DM the way the
/// message loop does (route → session → reply), with the model replaced by
/// the e2e seam: it claims the session for the answering employee like the
/// real turn and answers `OWN_BOT_REPLY agent=<id> session=<sid>`, or
/// `SESSION_CLASH …` when the session belongs to another employee.
async fn dm_turn(f: &Fixture, text: &str) -> (String, String) {
    let conversation = dm_conversation();
    let route = resolve_shared_route(&f.ctx, TG_USER, true).await;
    let session = shared_route_session(&conversation, &route);
    let (reply, _guard, _agent) =
        shared_route_reply(&f.ctx, route, text, &conversation, TG_USER, None).await;
    (session, reply)
}

/// The employee that owns `session` must be `owner`: claiming it for
/// `other` fails the ownership check.
async fn assert_owned_by(f: &Fixture, session: &str, owner: &str, other: &str) {
    assert!(
        f.ctx.session_manager.get_or_create(session, owner).await.is_ok(),
        "{session} belongs to {owner}"
    );
    assert!(
        f.ctx.session_manager.get_or_create(session, other).await.is_err(),
        "{session} is not {other}'s"
    );
}

#[tokio::test]
async fn rm172_existing_default_session_does_not_block_primary() {
    let f = fixture(true).await;
    crate::agent_channels::e2e_seam::arm(f._dir.path());
    let conversation = dm_conversation();
    // The staff member chatted with the default employee in this DM before
    // their identity was verified / their primary was set.
    f.ctx
        .session_manager
        .get_or_create(&conversation, "dudu")
        .await
        .unwrap();
    // Control: answering as the primary in the plain DM session is exactly
    // the ownership clash this guards against.
    let clash = build_guarded_reply_for_agent("幫我算帳", &f.ctx, "finance", &conversation, TG_USER, None)
        .await;
    assert!(clash.text.contains("SESSION_CLASH"), "{}", clash.text);

    let (session, reply) = dm_turn(&f, "幫我算帳").await;
    let bucket = crate::trigger_route::called_session(&conversation, "finance");
    assert_eq!(session, bucket, "the primary answers in its own bucket");
    assert!(!reply.contains("SESSION_CLASH"), "{reply}");
    assert!(
        reply.contains(&format!("agent=finance session={bucket}")),
        "{reply}"
    );
    // The default employee's conversation is untouched and still its own.
    assert_owned_by(&f, &conversation, "dudu", "finance").await;
    assert_owned_by(&f, &bucket, "finance", "dudu").await;
}

#[tokio::test]
async fn rm172_primary_change_uses_new_bucket() {
    let f = fixture(true).await;
    crate::agent_channels::e2e_seam::arm(f._dir.path());
    let conversation = dm_conversation();
    let finance = crate::trigger_route::called_session(&conversation, "finance");
    let sales = crate::trigger_route::called_session(&conversation, "sales-rep");

    let (session, reply) = dm_turn(&f, "幫我算帳").await;
    assert_eq!(session, finance);
    assert!(reply.contains(&format!("agent=finance session={finance}")), "{reply}");

    // The primary changes to another employee they own.
    f.db.bind_agent(&f.user_id, "sales-rep", AccessLevel::Owner).unwrap();
    f.db.set_primary_agent(&f.user_id, Some("sales-rep")).unwrap();
    let (session, reply) = dm_turn(&f, "這個月的報價").await;
    assert_eq!(session, sales, "the new primary has its own bucket");
    assert!(!reply.contains("SESSION_CLASH"), "{reply}");
    assert!(reply.contains(&format!("agent=sales-rep session={sales}")), "{reply}");
    // The earlier primary's history stays in its bucket.
    assert_owned_by(&f, &finance, "finance", "sales-rep").await;
}

#[tokio::test]
async fn rm172_fallback_after_revocation_uses_plain_session() {
    let f = fixture(true).await;
    crate::agent_channels::e2e_seam::arm(f._dir.path());
    let conversation = dm_conversation();
    let finance = crate::trigger_route::called_session(&conversation, "finance");

    let (session, reply) = dm_turn(&f, "幫我算帳").await;
    assert_eq!(session, finance);
    assert!(reply.contains(&format!("agent=finance session={finance}")), "{reply}");

    // Access to the primary is revoked: the DM falls back to the default
    // employee, in the plain conversation session as before RM-172.
    f.db.unbind_agent(&f.user_id, "finance").unwrap();
    let (session, reply) = dm_turn(&f, "你好").await;
    assert_eq!(session, conversation);
    assert!(!reply.contains("SESSION_CLASH"), "{reply}");
    assert!(
        reply.contains(&format!("agent=dudu session={conversation}")),
        "{reply}"
    );
    // The primary's bucket is left as it was.
    assert_owned_by(&f, &finance, "finance", "dudu").await;
}

#[tokio::test]
async fn rm172_shared_bot_binding_mode_still_guides_an_unverified_user() {
    let f = fixture(false).await;
    f.ctx
        .channel_settings
        .set("telegram", "global", keys::SHARED_BOT_BINDING, "true")
        .await
        .unwrap();
    assert_eq!(route(&f, true).await.as_deref(), Some("<guide>"));
}
