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
        SharedRoute::Bound(agent) => Some(agent),
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
