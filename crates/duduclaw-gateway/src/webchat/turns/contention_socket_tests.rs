//! Simultaneous final deliveries over real sockets: benign users.db contention
//! waits within the bounded budget, and an exhausted budget yields only the
//! fixed failure notice without taking the socket's other turns down.
//! Multi-threaded so that writers really collide and a blocked writer cannot
//! stall the test task that releases the held reservation.
use super::*;

/// Shorter than the delivery wait budget: every waiting delivery proceeds.
const BRIEF_HOLD: Duration = Duration::from_millis(60);

/// Hold users.db's write reservation from another connection, as a second
/// gateway process or a concurrent ACL writer would, while the replies finish.
async fn release_while_users_db_is_briefly_busy(fixture: &Fixture, turns: Vec<WaitingTurn>) {
    assert!(BRIEF_HOLD * 2 < duduclaw_auth::ACL_DELIVERY_WAIT);
    let hold = rusqlite::Connection::open(fixture._home.path().join("users.db")).unwrap();
    hold.execute_batch("BEGIN IMMEDIATE").unwrap();
    for turn in turns { turn.release.send(()).unwrap(); }
    // The hold ends on a thread of its own: runtime workers blocked in the
    // writers' bounded waits, or a loaded runtime, cannot push its COMMIT
    // past the wait budget the writers started after the release above.
    let holder = std::thread::spawn(move || {
        std::thread::sleep(BRIEF_HOLD);
        hold.execute_batch("COMMIT").unwrap();
    });
    tokio::task::spawn_blocking(move || holder.join()).await.unwrap().unwrap();
}

async fn assert_published(fixture: &Fixture, sids: &[String]) {
    let sessions = &fixture.state.ctx.session_manager;
    for sid in sids {
        let rows = sessions.get_messages(sid).await.unwrap();
        assert_eq!(rows.iter().filter(|row| row.role == "assistant" && row.content.contains("OWN_BOT_REPLY")).count(),
            1, "{sid}: the delivered reply is stored as published");
        assert!(rows.iter().all(|row| !row.content.contains("source changed")), "{sid}");
        assert!(sessions.webchat_history(sid).await.unwrap().iter().all(|row| row.notice_kind.is_none()),
            "{sid}: a delivered reply has no failure notice");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_replies_on_different_sockets_all_arrive_and_publish() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("contention-sockets");
    let mut sockets = Vec::new();
    for _ in 0..3 { sockets.push(fixture.connect(&token).await.0); }
    let mut turns = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        message(socket, &format!("c{index}"), &format!("m{index}")).await;
        turns.push(fixture.entered().await);
    }
    fixture.turn_count(3).await;
    let sids: Vec<String> = turns.iter().map(|turn| turn.session_id.clone()).collect();
    release_while_users_db_is_briefly_busy(&fixture, turns).await;
    for (index, socket) in sockets.iter_mut().enumerate() {
        done(socket, &format!("c{index}"), &format!("m{index}"), &sids[index]).await;
    }
    fixture.turn_count(0).await;
    assert_published(&fixture, &sids).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_replies_on_one_socket_all_arrive_and_publish() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let token = fixture.user("contention-one-socket");
    let (mut socket, _) = fixture.connect(&token).await;
    let mut turns = Vec::new();
    for index in 0..3 {
        message(&mut socket, &format!("c{index}"), &format!("m{index}")).await;
        turns.push(fixture.entered().await);
    }
    fixture.turn_count(3).await;
    let sids: Vec<String> = turns.iter().map(|turn| turn.session_id.clone()).collect();
    release_while_users_db_is_briefly_busy(&fixture, turns).await;
    let mut received = std::collections::HashMap::new();
    for _ in 0..3 {
        let reply = frame(&mut socket).await;
        assert_eq!(reply["type"], "assistant_done", "{reply}");
        assert!(reply["content"].as_str().unwrap().contains("OWN_BOT_REPLY"));
        received.insert(reply["msg_id"].as_str().unwrap().to_owned(), reply["session_id"].as_str().unwrap().to_owned());
    }
    for (index, sid) in sids.iter().enumerate() {
        assert_eq!(received.get(&format!("m{index}")), Some(sid), "every turn's frame arrived on the shared socket");
    }
    fixture.turn_count(0).await;
    assert_published(&fixture, &sids).await;
}

/// `client_msg_id` of every failure-notice row in `sid`'s history.
async fn failure_notices(fixture: &Fixture, sid: &str) -> Vec<Option<String>> {
    fixture.state.ctx.session_manager.webchat_history(sid).await.unwrap().into_iter()
        .filter(|row| row.notice_kind.as_deref() == Some("turn_failure"))
        .map(|row| row.client_msg_id).collect()
}

async fn wait_for_failure_notice(fixture: &Fixture, sid: &str, client_msg_id: Option<&str>) {
    tokio::time::timeout(DEADLINE, async {
        while !failure_notices(fixture, sid).await.iter()
            .any(|id| client_msg_id.is_none_or(|expected| id.as_deref() == Some(expected))) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("a reply refused for an exhausted budget must record the failure notice");
}

fn withheld_attempts(sid: &str, msg_id: &str) -> usize {
    crate::webchat::turns::run::withheld_notice_attempts().lock().unwrap().iter()
        .filter(|(session, msg)| session == sid && msg == msg_id).count()
}

/// Wait (bounded, not timed) until a notice attempt of `msg_id` has met the
/// busy database, so the retries really are under way.
async fn wait_for_withheld_notice_attempt(sid: &str, msg_id: &str) {
    tokio::time::timeout(DEADLINE, async {
        while withheld_attempts(sid, msg_id) == 0 { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.expect("a notice attempt must meet the busy database");
}

fn hold_users_db(fixture: &Fixture) -> rusqlite::Connection {
    let hold = rusqlite::Connection::open(fixture._home.path().join("users.db")).unwrap();
    hold.execute_batch("BEGIN IMMEDIATE").unwrap();
    hold
}

/// Send a barrier control and read up to its acknowledgement (or the
/// socket's close): no failure notice may arrive on the way.
async fn assert_no_notice_before_barrier(socket: &mut Socket) {
    let barrier = json!({"type":"subscribe", "session_id":null}).to_string();
    if socket.send(WireMessage::Text(barrier.into())).await.is_err() { return; }
    tokio::time::timeout(DEADLINE, async {
        while let Some(Ok(message)) = socket.next().await {
            match message {
                WireMessage::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    assert_ne!(value["code"], "reply_undelivered", "{value}");
                    if value["type"] == "subscribed" { return; }
                }
                WireMessage::Ping(data) => { let _ = socket.send(WireMessage::Pong(data)).await; }
                _ => {}
            }
        }
    }).await.expect("barrier acknowledgement or close");
}

/// The undelivered-notice path of a refused publication for `sid`, with the
/// turn's failure session bound as the real pipeline binds it.
async fn notify(state: std::sync::Arc<crate::webchat::WebChatState>, user: String, sid: String,
    msg_id: &'static str, cancel: tokio_util::sync::CancellationToken) {
    let session = state.ctx.session_manager.get_or_create(&sid, "finance").await.unwrap();
    crate::channel_reply::FAILURE_SESSION.scope(std::sync::Arc::new(std::sync::Mutex::new(None)), async {
        crate::channel_reply::bind_failure_session(&sid, &session.created_at, "finance");
        crate::webchat::turns::run::notify_undelivered(&state, &user, "finance", &sid, None, msg_id,
            &format!("direct-{msg_id}"), &cancel).await;
    }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exhausted_delivery_budget_sends_and_records_only_the_failure_notice() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    // As in production, the turn binds its failure session before the model runs.
    fixture.gate.bind_failure_sessions();
    let token = fixture.user("contention-exhausted");
    let (mut socket, _) = fixture.connect(&token).await;
    message(&mut socket, "busy", "busy").await;
    let busy = fixture.entered().await;
    message(&mut socket, "later", "later").await;
    let later = fixture.entered().await;
    fixture.turn_count(2).await;
    let sid = busy.session_id.clone();
    let sessions = &fixture.state.ctx.session_manager;
    // Held for longer than the budget, until the refused turn has recorded
    // its notice and a notice attempt has been withheld. The notice frame is
    // retried and passes the writer once the lock is gone, well inside the
    // retry window (the release follows the first withheld attempt).
    let hold = hold_users_db(&fixture);
    busy.release.send(()).unwrap();
    wait_for_failure_notice(&fixture, &sid, None).await;
    wait_for_withheld_notice_attempt(&sid, "busy").await;
    hold.execute_batch("ROLLBACK").unwrap();
    let notice = frame(&mut socket).await;
    assert_eq!(notice["type"], "error", "{notice}");
    assert_eq!(notice["code"], "reply_undelivered");
    assert_eq!(notice["msg_id"], "busy");
    assert_eq!(notice["session_id"], sid);
    assert!(!notice.to_string().contains("OWN_BOT_REPLY"));
    // The refusal took down only its own turn: the socket still serves the
    // next one, and its reply is the next frame (no second notice).
    later.release.send(()).unwrap();
    let later_sid = later.session_id.clone();
    done(&mut socket, "later", "later", &later_sid).await;
    fixture.turn_count(0).await;
    // A control acknowledgement is ordered behind any leaked reply frame.
    send(&mut socket, json!({"type":"subscribe", "session_id":null})).await;
    assert_eq!(frame(&mut socket).await["type"], "subscribed");
    let history = sessions.webchat_history(&sid).await.unwrap();
    let assistant: Vec<_> = history.iter().filter(|row| row.message.role == "assistant").collect();
    assert_eq!(assistant.len(), 1, "history holds the failure notice and nothing else");
    assert_eq!(assistant[0].notice_kind.as_deref(), Some("turn_failure"));
    assert_eq!(Some(assistant[0].message.content.as_str()), notice["message"].as_str(),
        "the client and history show the same fixed notice");
    assert!(sessions.get_messages(&sid).await.unwrap().iter().all(|row| !row.content.contains("OWN_BOT_REPLY")));
    assert_published(&fixture, &[later_sid]).await;
}

/// users.db stays busy for longer than the notice's retry window: the frame
/// is given up once (one WARN), history keeps the notice, and the socket
/// still serves the next turn once the database is free again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notice_frame_is_given_up_once_while_users_db_stays_busy() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    fixture.gate.bind_failure_sessions();
    let token = fixture.user("contention-abandoned");
    let (mut socket, _) = fixture.connect(&token).await;
    message(&mut socket, "busy", "busy").await;
    let busy = fixture.entered().await;
    message(&mut socket, "later", "later").await;
    let later = fixture.entered().await;
    fixture.turn_count(2).await;
    let sid = busy.session_id.clone();
    // turn_count's bound covers the longest the notice may hold the turn.
    assert!(crate::webchat::turns::run::NOTICE_HOLD_LIMIT < DEADLINE);
    let hold = hold_users_db(&fixture);
    let released = tokio::time::Instant::now();
    busy.release.send(()).unwrap();
    // The refused turn ends only when its notice retries have run out.
    fixture.turn_count(1).await;
    let held = released.elapsed();
    hold.execute_batch("ROLLBACK").unwrap();
    let abandoned = crate::webchat::turns::run::abandoned_notices().lock().unwrap()
        .iter().filter(|abandoned| **abandoned == sid).count();
    let withheld = withheld_attempts(&sid, "busy");
    assert_eq!(abandoned, 1,
        "one give-up, logged once (turn ended {held:?} after the release; {withheld} notice attempts withheld)");
    // Lower bounds only: a slower machine only makes these larger.
    assert!(held >= crate::webchat::turns::run::NOTICE_RETRY_WINDOW,
        "the turn ended before the retry window ran out: {held:?}");
    assert!(withheld >= 2, "the notice was retried while users.db stayed busy: {withheld}");
    assert_eq!(failure_notices(&fixture, &sid).await.len(), 1, "history holds the notice");
    // No notice frame was left behind: the next turn's reply is the next frame.
    later.release.send(()).unwrap();
    let later_sid = later.session_id.clone();
    done(&mut socket, "later", "later", &later_sid).await;
    fixture.turn_count(0).await;
    assert_no_notice_before_barrier(&mut socket).await;
    assert_published(&fixture, &[later_sid]).await;
}

/// A real access change while the notice waits for a busy database: the
/// retries stop at the refusal and no notice frame reaches the account.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn access_revoked_during_notice_retries_sends_no_notice() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    fixture.gate.bind_failure_sessions();
    let (user, token) = fixture.employee("contention-revoked", &["finance"], None).await;
    let (mut socket, _) = fixture.connect(&token).await;
    message(&mut socket, "revoked", "revoked").await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    let busy = fixture.entered().await;
    fixture.turn_count(1).await;
    let sid = busy.session_id.clone();
    let hold = hold_users_db(&fixture);
    busy.release.send(()).unwrap();
    wait_for_failure_notice(&fixture, &sid, None).await;
    // At least one notice attempt has met the busy database; the revocation
    // follows it, well inside the retry window.
    wait_for_withheld_notice_attempt(&sid, "revoked").await;
    // The holder itself revokes the binding, so no retry can pass before it.
    hold.execute("DELETE FROM user_agent_bindings WHERE user_id = ?1 AND agent_name = 'finance'",
        rusqlite::params![user]).unwrap();
    hold.execute_batch("UPDATE auth_meta SET value = value + 1 WHERE key = 'acl_generation'; COMMIT").unwrap();
    fixture.turn_count(0).await;
    assert!(!crate::webchat::turns::run::abandoned_notices().lock().unwrap().contains(&sid),
        "a real refusal ends the retries at once, not at the window's end");
    assert_no_notice_before_barrier(&mut socket).await;
    assert_eq!(failure_notices(&fixture, &sid).await.len(), 1);
}

/// Cancellation after the refused publication and before the notice records
/// and sends nothing; cancellation during the retries stops them at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_turn_records_and_sends_no_failure_notice() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    let (user, token) = fixture.employee("contention-cancel", &["finance"], None).await;
    let (mut socket, _) = fixture.connect(&token).await;
    // A delivered turn subscribes this socket to the conversation.
    message(&mut socket, "cancel", "first").await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    let first = fixture.entered().await;
    let sid = first.session_id.clone();
    first.release.send(()).unwrap();
    done(&mut socket, "cancel", "first", &sid).await;
    fixture.turn_count(0).await;
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    notify(fixture.state.clone(), user.clone(), sid.clone(), "cancelled", cancelled).await;
    assert!(failure_notices(&fixture, &sid).await.is_empty(), "a cancelled turn records no notice");
    let hold = hold_users_db(&fixture);
    let cancel = tokio_util::sync::CancellationToken::new();
    let retrying = tokio::spawn(notify(fixture.state.clone(), user.clone(), sid.clone(), "retrying", cancel.clone()));
    wait_for_failure_notice(&fixture, &sid, Some("turn_failure:direct-retrying")).await;
    wait_for_withheld_notice_attempt(&sid, "retrying").await;
    cancel.cancel();
    // Shorter than the window still left, so ending in time means the
    // cancellation, not the window, stopped the retries.
    tokio::time::timeout(crate::webchat::turns::run::NOTICE_RETRY_WINDOW / 2, retrying).await
        .expect("cancellation ends the notice retries at once").unwrap();
    hold.execute_batch("ROLLBACK").unwrap();
    // Control: the same path, not cancelled, delivers its notice; it is the
    // first notice frame, so neither cancelled one reached the client.
    notify(fixture.state.clone(), user, sid.clone(), "live", tokio_util::sync::CancellationToken::new()).await;
    let notice = frame(&mut socket).await;
    assert_eq!(notice["code"], "reply_undelivered", "{notice}");
    assert_eq!(notice["msg_id"], "live");
    assert_no_notice_before_barrier(&mut socket).await;
    assert!(!crate::webchat::turns::run::abandoned_notices().lock().unwrap().contains(&sid));
}
