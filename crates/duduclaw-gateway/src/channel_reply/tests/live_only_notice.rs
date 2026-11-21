//! RM-191: a fixed notice returned before the turn ensured its session
//! (`mark_live_only_notice`). On a brand-new WebChat conversation it is
//! delivered live and nothing is stored; in an existing conversation it is
//! stored as before. Not run here (owner forbids builds).
use super::*;

const NOTICE: &str = "⚠️ 這則訊息沒有交給 AI 處理（測試用固定通知）。";

async fn raw_rows(mgr: &SessionManager, sid: &str) -> i64 {
    // Pending rows are hidden from `get_messages`, so count raw rows.
    mgr.acquire().await
        .query_row("SELECT COUNT(*) FROM session_messages WHERE session_id = ?1",
            rusqlite::params![sid], |r| r.get(0))
        .unwrap()
}

async fn marked_delivery(text: &str) -> Arc<CcrTurnDelivery> {
    let delivery = Arc::new(CcrTurnDelivery::default());
    CCR_TURN_DELIVERY.scope(delivery.clone(), async { mark_live_only_notice(text) }).await;
    delivery
}

/// Persist and publish `text` the way `send_terminal` does.
async fn deliver(mgr: &Arc<SessionManager>, dir: &std::path::Path, sid: &str, delivery: Arc<CcrTurnDelivery>, text: &str) -> Result<(), ()> {
    let reply = GuardedReply::new(text.into(), None, Vec::new(),
        Some(TurnRollback { delivery, session_mgr: mgr.clone() })).await;
    reply.persist_webchat(mgr, sid, text).await?;
    let users = duduclaw_auth::UserDb::new(&dir.join("users.db")).unwrap();
    reply.publish_webchat(mgr, sid, &users, users.acl_generation().unwrap(),
        &crate::webchat::access::TargetPolicy::test_admin(), &tokio_util::sync::CancellationToken::new()).await
}

#[tokio::test]
async fn new_conversation_gets_the_notice_live_and_stores_nothing() {
    let sid = "webchat:rm191-new";
    let ((_dir, mgr, verdict, delivered), cleanup) = with_delivery_cleanup(async {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
        let delivery = marked_delivery(NOTICE).await;
        assert!(delivery.fixed_notice());
        let verdict = delivery.subscribe();
        let delivered = deliver(&mgr, dir.path(), sid, delivery, NOTICE).await;
        (dir, mgr, verdict, delivered)
    }).await;
    cleanup.unwrap();
    assert_eq!(delivered, Ok(()), "the notice is delivered, not refused");
    assert_eq!(verdict.await, Ok(true));
    assert!(mgr.turn_generation(sid).await.unwrap().is_none(), "no session row is created");
    assert_eq!(raw_rows(&mgr, sid).await, 0, "nothing is stored");
}

#[tokio::test]
async fn existing_conversation_still_stores_the_notice() {
    let sid = "webchat:rm191-existing";
    let ((_dir, mgr, delivered), cleanup) = with_delivery_cleanup(async {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
        mgr.get_or_create(sid, "agent-a").await.unwrap();
        let delivery = marked_delivery(NOTICE).await;
        let delivered = deliver(&mgr, dir.path(), sid, delivery, NOTICE).await;
        (dir, mgr, delivered)
    }).await;
    cleanup.unwrap();
    assert_eq!(delivered, Ok(()));
    let history = mgr.get_messages(sid).await.unwrap();
    assert_eq!(history.iter().filter(|m| m.role == "assistant" && m.content == NOTICE).count(), 1);
}

/// Only the marked text itself may skip storage: anything else takes the
/// ordinary path, which on a conversation with no row refuses as before.
#[tokio::test]
async fn other_text_never_takes_the_live_only_path() {
    let sid = "webchat:rm191-other";
    let ((_dir, mgr, delivered), cleanup) = with_delivery_cleanup(async {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
        let delivery = marked_delivery(NOTICE).await;
        let delivered = deliver(&mgr, dir.path(), sid, delivery, "model-written reply").await;
        (dir, mgr, delivered)
    }).await;
    cleanup.unwrap();
    assert!(delivered.is_err());
    assert_eq!(raw_rows(&mgr, sid).await, 0);
}

#[tokio::test]
async fn blank_text_is_not_marked() {
    let delivery = marked_delivery("  ").await;
    assert!(!delivery.fixed_notice());
}
