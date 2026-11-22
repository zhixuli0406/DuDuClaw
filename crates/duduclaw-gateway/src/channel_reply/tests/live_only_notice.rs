//! RM-191: a fixed notice returned before the turn ensured its session
//! (`mark_live_only_notice`). The marker decides persistence only: the
//! guarded builders run the notice through the whole output pipeline and
//! then bind its FINAL text (`CcrTurnDelivery::bind_live_only_notice`). Only
//! that exact final text may skip storage: on a brand-new WebChat
//! conversation it is delivered live and nothing is stored; in an existing
//! conversation it is stored once, as before. The end-to-end pipeline cases
//! (contract, sanitising, real gates) are in `fixed_notice_pipeline.rs`.
//! Not run here (owner forbids builds); the reviewer runs them.
use super::*;

const NOTICE: &str = "⚠️ 這則訊息沒有交給 AI 處理（測試用固定通知）。";

async fn raw_rows(mgr: &SessionManager, sid: &str) -> i64 {
    // Pending rows are hidden from `get_messages`, so count raw rows.
    mgr.acquire().await
        .query_row("SELECT COUNT(*) FROM session_messages WHERE session_id = ?1",
            rusqlite::params![sid], |r| r.get(0))
        .unwrap()
}

/// Mark `raw` the way the pipeline does, then bind `final_text` the way the
/// guarded builders do once contract, sanitising and language ran.
async fn marked_delivery(raw: &str, final_text: &str) -> Arc<CcrTurnDelivery> {
    let delivery = Arc::new(CcrTurnDelivery::default());
    CCR_TURN_DELIVERY.scope(delivery.clone(), async { mark_live_only_notice(raw) }).await;
    if delivery.marked_fixed_notice(raw) {
        delivery.bind_live_only_notice(final_text);
    }
    delivery
}

/// Only mark `raw` (no pipeline ran yet, nothing bound).
async fn raw_marked_delivery(raw: &str) -> Arc<CcrTurnDelivery> {
    let delivery = Arc::new(CcrTurnDelivery::default());
    CCR_TURN_DELIVERY.scope(delivery.clone(), async { mark_live_only_notice(raw) }).await;
    delivery
}

/// Persist and publish `text` the way `send_terminal` does. `persists`
/// staging calls (a retried terminal frame stages twice).
async fn deliver_staged(
    mgr: &Arc<SessionManager>, dir: &std::path::Path, sid: &str, delivery: Arc<CcrTurnDelivery>, text: &str,
    persists: usize,
) -> Result<(), ()> {
    let reply = GuardedReply::new(text.into(), None, Vec::new(),
        Some(TurnRollback { delivery, session_mgr: mgr.clone() })).await;
    for _ in 0..persists {
        reply.persist_webchat(mgr, sid, text).await?;
    }
    let users = duduclaw_auth::UserDb::new(&dir.join("users.db")).unwrap();
    reply.publish_webchat(mgr, sid, &users, users.acl_generation().unwrap(),
        &crate::webchat::access::TargetPolicy::test_admin(), &tokio_util::sync::CancellationToken::new()).await
}

async fn deliver(mgr: &Arc<SessionManager>, dir: &std::path::Path, sid: &str, delivery: Arc<CcrTurnDelivery>, text: &str) -> Result<(), ()> {
    deliver_staged(mgr, dir, sid, delivery, text, 1).await
}

#[tokio::test]
async fn new_conversation_gets_the_notice_live_and_stores_nothing() {
    let sid = "webchat:rm191-new";
    let ((_dir, mgr, verdict, delivered), cleanup) = with_delivery_cleanup(async {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
        let delivery = marked_delivery(NOTICE, NOTICE).await;
        assert_eq!(delivery.live_only_text().as_deref(), Some(NOTICE));
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
        let delivery = marked_delivery(NOTICE, NOTICE).await;
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
        let delivery = marked_delivery(NOTICE, NOTICE).await;
        let delivered = deliver(&mgr, dir.path(), sid, delivery, "model-written reply").await;
        (dir, mgr, delivered)
    }).await;
    cleanup.unwrap();
    assert!(delivered.is_err());
    assert_eq!(raw_rows(&mgr, sid).await, 0);
}

/// Review fix (MEDIUM): the raw marker alone is never a live-only text. It
/// only becomes one once the output pipeline bound its FINAL text — and when
/// the pipeline rewrote the notice (a contract block, sanitising), the raw
/// pre-pipeline text can no longer take the live-only path.
#[tokio::test]
async fn only_the_bound_final_text_takes_the_live_only_path() {
    let rewritten = CONTRACT_BLOCK_MESSAGE;
    let ((_dir, mgr, unbound, raw_after_rewrite, final_after_rewrite), cleanup) = with_delivery_cleanup(async {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
        // Marked but never bound (no pipeline ran): not live-only.
        let delivery = raw_marked_delivery(NOTICE).await;
        assert!(delivery.marked_fixed_notice(NOTICE));
        assert!(delivery.live_only_text().is_none(), "marking never binds a live-only text");
        let unbound = deliver(&mgr, dir.path(), "webchat:rm191-unbound", delivery, NOTICE).await;
        // The pipeline replaced the notice: the raw text is refused, the
        // replacement is delivered live.
        let delivery = marked_delivery(NOTICE, rewritten).await;
        assert_eq!(delivery.live_only_text().as_deref(), Some(rewritten));
        let raw_after_rewrite = deliver(&mgr, dir.path(), "webchat:rm191-rewritten-raw", delivery, NOTICE).await;
        let delivery = marked_delivery(NOTICE, rewritten).await;
        let final_after_rewrite = deliver(&mgr, dir.path(), "webchat:rm191-rewritten", delivery, rewritten).await;
        (dir, mgr, unbound, raw_after_rewrite, final_after_rewrite)
    }).await;
    cleanup.unwrap();
    assert!(unbound.is_err(), "an unbound raw marker must not skip storage");
    assert!(raw_after_rewrite.is_err(), "the pre-pipeline text must not be delivered");
    assert_eq!(final_after_rewrite, Ok(()));
    for sid in ["webchat:rm191-unbound", "webchat:rm191-rewritten-raw", "webchat:rm191-rewritten"] {
        assert_eq!(raw_rows(&mgr, sid).await, 0, "{sid}");
        assert!(mgr.turn_generation(sid).await.unwrap().is_none(), "{sid}");
    }
}

#[tokio::test]
async fn blank_text_is_not_marked() {
    let delivery = raw_marked_delivery("  ").await;
    assert!(!delivery.marked_fixed_notice("  "));
    delivery.bind_live_only_notice("  ");
    assert!(delivery.live_only_text().is_none());
}

/// Item 5 (unit level): the marker lives in the per-turn delivery object
/// only. Two interleaved scopes in one task never see each other's marker,
/// a cancelled scope leaves nothing outside its own object, and no ambient
/// state exists outside a guarded turn.
#[tokio::test]
async fn marker_lives_in_its_own_turn_object_only() {
    use futures_util::FutureExt;
    let first = Arc::new(CcrTurnDelivery::default());
    let second = Arc::new(CcrTurnDelivery::default());
    let (marked_tx, marked_rx) = tokio::sync::oneshot::channel::<()>();
    let (checked_tx, checked_rx) = tokio::sync::oneshot::channel::<()>();
    let a = CCR_TURN_DELIVERY.scope(first.clone(), async move {
        mark_live_only_notice(NOTICE);
        marked_tx.send(()).unwrap();
        checked_rx.await.unwrap();
        // The other turn marked its own text meanwhile.
        CCR_TURN_DELIVERY.with(|d| d.marked_fixed_notice(NOTICE))
    });
    let b = CCR_TURN_DELIVERY.scope(second.clone(), async move {
        marked_rx.await.unwrap();
        let saw_other = CCR_TURN_DELIVERY.with(|d| d.marked_fixed_notice(NOTICE));
        mark_live_only_notice("other turn's notice");
        checked_tx.send(()).unwrap();
        saw_other
    });
    let (a_still_own, b_saw_other) = tokio::join!(a, b);
    assert!(a_still_own, "a turn keeps its own marker");
    assert!(!b_saw_other, "a concurrent turn never sees another turn's marker");
    assert!(first.marked_fixed_notice(NOTICE) && !first.marked_fixed_notice("other turn's notice"));
    assert!(second.marked_fixed_notice("other turn's notice") && !second.marked_fixed_notice(NOTICE));

    // Cancelled after marking: the marker stays inside the dropped turn's
    // object; a fresh turn (the next one, same session or not) starts clean.
    let cancelled = Arc::new(CcrTurnDelivery::default());
    let turn = CCR_TURN_DELIVERY.scope(cancelled.clone(), async {
        mark_live_only_notice(NOTICE);
        std::future::pending::<()>().await;
    });
    assert!(turn.now_or_never().is_none(), "the turn is cancelled mid-flight");
    assert!(cancelled.marked_fixed_notice(NOTICE));
    let next = Arc::new(CcrTurnDelivery::default());
    assert!(!next.marked_fixed_notice(NOTICE) && next.live_only_text().is_none());
    assert!(CCR_TURN_DELIVERY.try_with(|_| ()).is_err(), "no ambient marker outside a turn");
    mark_live_only_notice(NOTICE); // no-op outside a guarded reply
    assert!(!next.marked_fixed_notice(NOTICE));
}

/// Item 7: every early fixed reply the pipeline marks (failsafe halted and
/// restricted, breaker refusal, killswitch cost limit, budget refusal, the
/// redaction failure). New conversation: delivered live, nothing stored.
/// Existing conversation: stored exactly once as the assistant row, even
/// when the terminal frame is staged twice. The texts are the producers'
/// own; the real-gate cases (failsafe, budget) run end to end in
/// `fixed_notice_pipeline.rs`.
#[tokio::test]
async fn every_marked_fixed_reply_is_live_only_when_new_and_stored_once_when_existing() {
    let budget_deny = crate::budget::BudgetRefusal(crate::budget::BudgetVerdict::Deny {
        scope: "monthly", spent_cents: 5_000, cap_cents: 5_000,
    }).to_string();
    let budget_unavailable = crate::budget::BudgetRefusal(crate::budget::BudgetVerdict::Unavailable {
        reason: "test".into(),
    }).to_string();
    let cases: Vec<(&str, String)> = vec![
        ("failsafe halted", "Service paused.".into()),
        ("failsafe restricted", "Service restricted.".into()),
        ("breaker refusal", super::super::breaker_wait::refused_text(crate::trigger_route::texts::Lang::ZhTw).into()),
        ("killswitch cost limit", "Service restricted.".into()),
        ("budget deny", budget_deny),
        ("budget unavailable", budget_unavailable),
        ("redaction failure", REDACTION_FAILED_REPLY.into()),
    ];
    for (n, (kind, text)) in cases.into_iter().enumerate() {
        let new_sid = format!("webchat:rm191-table-new-{n}");
        let old_sid = format!("webchat:rm191-table-old-{n}");
        let ((_dir, mgr, new_delivered, old_delivered), cleanup) = with_delivery_cleanup(async {
            let dir = tempfile::tempdir().unwrap();
            let mgr = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
            let new_delivered = deliver(&mgr, dir.path(), &new_sid, marked_delivery(&text, &text).await, &text).await;
            mgr.get_or_create(&old_sid, "agent-a").await.unwrap();
            let old_delivered = deliver_staged(&mgr, dir.path(), &old_sid,
                marked_delivery(&text, &text).await, &text, 2).await;
            (dir, mgr, new_delivered, old_delivered)
        }).await;
        cleanup.unwrap();
        assert_eq!(new_delivered, Ok(()), "{kind}: delivered live on a new conversation");
        assert!(mgr.turn_generation(&new_sid).await.unwrap().is_none(), "{kind}: no session row");
        assert_eq!(raw_rows(&mgr, &new_sid).await, 0, "{kind}: nothing stored");
        assert_eq!(old_delivered, Ok(()), "{kind}: delivered in an existing conversation");
        let history = mgr.get_messages(&old_sid).await.unwrap();
        assert_eq!(history.iter().filter(|m| m.role == "assistant" && m.content == text).count(), 1,
            "{kind}: stored once as the assistant notice");
        assert_eq!(raw_rows(&mgr, &old_sid).await, 1, "{kind}: no second row");
    }
}
