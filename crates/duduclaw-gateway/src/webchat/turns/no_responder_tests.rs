//! A whole WebChat turn when no AI employee answers: the fixed notice
//! reaches the page through the guarded delivery path (`send_terminal`,
//! `persist_webchat`, `publish_webchat`) without a session or a stored row,
//! and no model boundary is crossed (the cloud spy panics if one is).
use super::*;

const VISITOR: &str = "widget-visitor:7f1c2a90-0000-4000-8000-0000000000aa";

async fn state_without_employees(home: &std::path::Path) -> (Arc<WebChatState>, String) {
    crate::team_edition::record(home, duduclaw_core::EditionProfile::Enterprise);
    std::fs::create_dir_all(home.join("agents")).unwrap();
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let admin = db.create_user("noresp-admin@test.invalid", "No Responder Admin", "Test-Password-12345",
        duduclaw_auth::UserRole::Admin).unwrap();
    std::fs::write(home.join("config.toml"), "").unwrap();
    crate::mfa::reload_policy(home);
    let state = Arc::new(WebChatState::new(
        crate::agent_channels::e2e_tests::reply_ctx(home).await,
        Arc::new(JwtConfig::new(b"no-responder-turn-test-key")),
        Arc::new(db),
    ));
    (state, admin.id)
}

/// Run one turn through `execute` and return the terminal frame the page's
/// socket subscription received (acknowledged like a real socket writer).
async fn webchat_turn(state: &Arc<WebChatState>, auth_user: &str, sid: &str) -> serde_json::Value {
    let (tx, mut rx) = outbound_channel();
    let subscription = state.subscriptions.register(auth_user, tx.clone());
    subscription.subscribe(sid, None);
    let ticket = state.turns.admit(auth_user, sid, None, "m", 3, 10).unwrap();
    let input = TurnInput {
        state: state.clone(), auth_user: auth_user.into(), credential: None,
        user_id: auth_user.into(), agent_id: String::new(), requested_agent: None,
        effective_agent: None, sid: sid.into(), conv_nonce: None,
        content: "hello".into(), attachments: Vec::new(), assign_event: None,
        resume_acl: false, arrival_seq: 1,
    };
    let task = tokio::spawn(execute(input, TurnSink::new(tx, sid, None, "m"), ticket));
    loop {
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await.expect("turn produced no frame").expect("socket open");
        let frame = match &output.message {
            Message::Text(text) => serde_json::from_str::<serde_json::Value>(text).unwrap(),
            _ => serde_json::Value::Null,
        };
        if let Some(ack) = output.ack { let _ = ack.send(true); }
        assert_ne!(frame["type"], "turn_accepted", "no user row may be stored");
        if frame["type"] == "assistant_done" || frame["type"] == "error" {
            task.await.unwrap();
            return frame;
        }
    }
}

/// An ordinary signed-in member (Employee role, no bindings at all).
fn member(state: &WebChatState) -> String {
    state.user_db.create_user("noresp-member@test.invalid", "No Responder Member", "Test-Password-12345",
        duduclaw_auth::UserRole::Employee).unwrap().id
}

async fn assert_nothing_stored(state: &WebChatState, sid: &str) {
    assert!(state.ctx.session_manager.session_agent(sid).await.unwrap().is_none(), "{sid}: no session");
    assert!(state.ctx.session_manager.webchat_history(sid).await.unwrap().is_empty(), "{sid}: no row");
}

#[tokio::test]
async fn widget_visitor_without_an_employee_receives_the_generic_notice() {
    let home = tempfile::tempdir().unwrap();
    let (state, _) = state_without_employees(home.path()).await;
    let spy = crate::channel_reply::NoImplicitCloud::at(home.path());
    let sid = "webchat:noresp-widget";
    let frame = webchat_turn(&state, VISITOR, sid).await;
    assert_eq!(frame["type"], "assistant_done", "{frame}");
    assert_eq!(frame["content"], "目前無法提供服務，請稍後再試。");
    assert_ne!(frame["content"], crate::channel_reply::NO_RESPONDER_TEXT);
    assert_eq!(spy.counts(), [0, 0], "no model call");
    assert_nothing_stored(&state, sid).await;
}

#[tokio::test]
async fn signed_in_dashboard_account_without_an_employee_receives_the_setup_notice() {
    let home = tempfile::tempdir().unwrap();
    let (state, admin) = state_without_employees(home.path()).await;
    let spy = crate::channel_reply::NoImplicitCloud::at(home.path());
    let sid = "webchat:noresp-admin";
    let frame = webchat_turn(&state, &admin, sid).await;
    assert_eq!(frame["type"], "assistant_done", "{frame}");
    assert_eq!(frame["content"], crate::channel_reply::NO_RESPONDER_TEXT);
    assert_eq!(spy.counts(), [0, 0], "no model call");
    assert_nothing_stored(&state, sid).await;
}

/// Review round 2 (M): a non-Admin account has no employee to be checked
/// against; the target-less re-check lets the notice through.
#[tokio::test]
async fn signed_in_member_without_an_employee_receives_the_setup_notice() {
    let home = tempfile::tempdir().unwrap();
    let (state, _) = state_without_employees(home.path()).await;
    let member = member(&state);
    let spy = crate::channel_reply::NoImplicitCloud::at(home.path());
    let sid = "webchat:noresp-member";
    let frame = webchat_turn(&state, &member, sid).await;
    assert_eq!(frame["type"], "assistant_done", "{frame}");
    assert_eq!(frame["content"], crate::channel_reply::NO_RESPONDER_TEXT);
    assert!(frame["agent"].is_null(), "no employee is named: {frame}");
    assert_eq!(spy.counts(), [0, 0], "no model call");
    assert_nothing_stored(&state, sid).await;
}

#[tokio::test]
async fn deactivated_account_without_an_employee_receives_no_notice() {
    let home = tempfile::tempdir().unwrap();
    let (state, _) = state_without_employees(home.path()).await;
    let member = member(&state);
    state.user_db.set_user_status(&member, duduclaw_auth::UserStatus::Suspended).unwrap();
    let spy = crate::channel_reply::NoImplicitCloud::at(home.path());
    let sid = "webchat:noresp-suspended";
    let frame = webchat_turn(&state, &member, sid).await;
    assert_eq!(frame["type"], "error", "{frame}");
    assert_eq!(frame["code"], "agent_unavailable", "{frame}");
    let shown = frame["message"].as_str().unwrap_or_default();
    assert!(!crate::channel_reply::is_no_responder_notice(shown), "{frame}");
    assert_eq!(spy.counts(), [0, 0], "no model call");
    assert_nothing_stored(&state, sid).await;
}

/// The delivery re-check of the notice: the target-less policy holds for an
/// Active member, and a deactivation — before it is built or after its
/// users.db generation was captured — refuses the final write.
#[tokio::test]
async fn target_less_delivery_recheck_needs_an_active_account() {
    let home = tempfile::tempdir().unwrap();
    let (state, _) = state_without_employees(home.path()).await;
    let member = member(&state);
    assert!(state.authorize_no_target(&member).await.is_ok());
    let generation = state.user_db.acl_generation().unwrap();
    let policy = state.turn_policy(&member, None, true).await.unwrap();
    assert!(matches!(state.user_db.with_acl_generation(generation, || policy.with_current(|| true)), Ok(Ok(true))));
    state.user_db.set_user_status(&member, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert!(!matches!(state.user_db.with_acl_generation(generation, || policy.with_current(|| true)), Ok(Ok(true))));
    assert!(state.authorize_no_target(&member).await.is_err());
    assert!(state.turn_policy(&member, None, true).await.is_err());
    assert!(state.authorize_no_target("unknown-account").await.is_err(), "unreadable fails closed");
}
